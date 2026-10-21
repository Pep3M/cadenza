//! `limiter`: lookahead brickwall true-peak limiter.
//!
//! Gain path: per-sample required gain → sliding minimum over `L + 1`
//! samples → moving average over `L` samples → release follower, with the
//! audio delayed by `L`. The moving average of a window-minimum that covers
//! the delayed sample guarantees the applied gain is never above what that
//! sample needs, so peaks cannot overshoot, and the attack is a smooth ramp.
//!
//! The per-sample "required gain" is derived from a 4x-oversampled estimate
//! of the signal (a polyphase Kaiser-windowed-sinc interpolator, same idea as
//! the BS.1770-4 Annex 2 true-peak meter in `cadenza-engine`'s mix analyzer),
//! not from the raw sample value. That way inter-sample peaks that a plain
//! sample-peak limiter misses are caught before they reach the ceiling, and
//! the limiter's own output matches what the analyzer later reports as
//! `true_peak_db`. The interpolator only looks backward, so it adds no extra
//! output latency: it anticipates peaks within the existing lookahead window
//! exactly like the raw-sample detector did before.
//!
//! Oversampling (`oversample` 2x / 4x, default 1x = the path above): the
//! signal is upsampled by a linear-phase polyphase windowed-sinc FIR, the same
//! limiter runs at the higher rate (lookahead and release in the same ms, a
//! plain sample-peak detector, since the samples are now close to the
//! continuous waveform), its hard clamp acts on the oversampled points, and the
//! decimating FIR brings it back. Inter-sample peaks the 1x clamp can create
//! are caught at the high rate, so the true peak sits closer to the ceiling.
//! The two FIRs add [`OS_LATENCY`] samples (at the base rate) to the lookahead,
//! reported through `latency()`. Every mode is allocated in `prepare`, so
//! switching is realtime safe (the latency changes: hosts recompile).

use std::f64::consts::PI;

use crate::processor::{DeviceInfo, DeviceRole, MidiEvent, ParamInfo, Processor, Transport};
use crate::util::delay_line::DelayLine;
use crate::util::smoother::Smoother;
use crate::util::{ParamValues, db_to_gain, gain_to_db, one_pole_coeff};

const P_INPUT: usize = 0;
const P_CEILING: usize = 1;
const P_RELEASE: usize = 2;
const P_OVERSAMPLE: usize = 3;
const N_PARAMS: usize = 4;

/// Oversampling factors of `oversample` 0, 1, 2.
pub const OVERSAMPLE_FACTORS: [usize; 3] = [1, 2, 4];
/// Taps per polyphase branch of the oversampling FIRs; also the latency (base
/// samples) the up + down filters add.
const OS_TAPS: usize = 32;
/// Extra latency of 2x / 4x oversampling, base-rate samples.
pub const OS_LATENCY: usize = OS_TAPS;
/// Detection ceiling margin in the oversampled path: the oversampled points go
/// through the same 4x estimator (8x / 16x of the base rate), so only the
/// decimator's ripple is left to absorb.
const OS_SAFETY_MARGIN: [f32; 3] = [TP_SAFETY_MARGIN, 0.97, 0.98];

/// Lookahead + oversampling latency in samples, as `latency()` reports it.
pub fn limiter_latency_samples(oversample: f32, sample_rate: f32) -> u32 {
	let la = ((LOOKAHEAD_MS * 0.001 * sample_rate).round() as usize).max(1);
	let mode = (oversample.round().max(0.0) as usize).min(2);
	(la + if mode == 0 { 0 } else { OS_LATENCY }) as u32
}

const LOOKAHEAD_MS: f32 = 5.0;
/// Headroom applied to the sidechain's detection ceiling (not the output hard
/// clamp) to absorb the small approximation error between the moving-average
/// gain path and a true instantaneous bound on the oversampled peak.
const TP_SAFETY_MARGIN: f32 = 0.955; // approx -0.4 dB

static PARAMS: [ParamInfo; N_PARAMS] = [
	ParamInfo::new(
		"input_gain",
		"Input Gain",
		0.0,
		24.0,
		0.0,
		"dB",
		"Drive into the limiter; raises loudness. 2-6 dB is typical on a master, more squashes transients.",
	),
	ParamInfo::new(
		"ceiling",
		"Ceiling",
		-24.0,
		0.0,
		-1.0,
		"dB",
		"Absolute maximum output peak. -1 dB is safe for streaming/encoding.",
	),
	ParamInfo::new(
		"release",
		"Release",
		1.0,
		1_000.0,
		100.0,
		"ms",
		"Recovery time after a peak. 50-200 ms; shorter is louder but can distort bass.",
	),
	ParamInfo::new(
		"oversample",
		"Oversampling",
		0.0,
		2.0,
		0.0,
		"",
		"0 = 1x, 1 = 2x, 2 = 4x. Runs the limiter at 2x/4x the sample rate so inter-sample (true) \
		 peaks stay under the ceiling more tightly and hard limiting aliases less; costs CPU and \
		 adds 32 samples of latency. Use 2 (4x) on the master for streaming deliverables.",
	),
];

pub static INFO: DeviceInfo = DeviceInfo {
	id: "limiter",
	name: "Limiter",
	role: DeviceRole::Effect,
	description: "Transparent lookahead brickwall limiter (5 ms latency): output never exceeds the \
		ceiling. Put it LAST on the master bus to catch peaks and raise loudness: ceiling -1, \
		release 100, input_gain 2-6 dB (watch that reduction stays modest). Can also guard \
		individual buses against overs. oversample 2 (4x) for the tightest true-peak control.",
	params: &PARAMS,
};

/// Fixed-capacity monotonic deque giving the minimum over a sliding window.
#[derive(Debug, Default)]
struct SlidingMin {
	values: Vec<f32>,
	stamps: Vec<u64>,
	head: usize,
	len: usize,
	window: u64,
	now: u64,
}

impl SlidingMin {
	fn allocate(&mut self, window: usize) {
		let cap = window + 1;
		self.values.clear();
		self.values.resize(cap, 1.0);
		self.stamps.clear();
		self.stamps.resize(cap, 0);
		self.window = window as u64;
		self.clear();
	}

	fn clear(&mut self) {
		self.head = 0;
		self.len = 0;
		self.now = 0;
	}

	#[inline]
	fn push(&mut self, v: f32) -> f32 {
		let cap = self.values.len();
		if cap == 0 {
			return v;
		}
		while self.len > 0 {
			let back = (self.head + self.len - 1) % cap;
			if self.values[back] >= v {
				self.len -= 1;
			} else {
				break;
			}
		}
		while self.len > 0 && self.now - self.stamps[self.head] >= self.window {
			self.head = (self.head + 1) % cap;
			self.len -= 1;
		}
		let slot = (self.head + self.len) % cap;
		self.values[slot] = v;
		self.stamps[slot] = self.now;
		self.len += 1;
		self.now += 1;
		self.values[self.head]
	}
}

const TP_PHASES: usize = 4;
/// Taps per polyphase branch; 32-48 is the sweet spot between stop-band
/// rejection (so true peaks aren't under-estimated) and CPU cost.
const TP_TAPS: usize = 40;

/// Causal polyphase 4x oversampling interpolator used purely as a peak
/// detector (not to produce audio): for every new real sample it returns the
/// largest absolute value among that sample and the three interpolated
/// points following it, i.e. an estimate of the continuous-time peak. Fixed
/// size, no heap allocation, built once outside the audio thread.
#[derive(Debug, Clone)]
struct TruePeakDetector {
	kernel: [[f32; TP_TAPS]; TP_PHASES],
	/// Per-channel history, duplicated so any window of `TP_TAPS` samples is
	/// contiguous regardless of the write position.
	history: [[f32; 2 * TP_TAPS]; 2],
	pos: usize,
}

fn bessel_i0(x: f64) -> f64 {
	let mut sum = 1.0;
	let mut term = 1.0;
	for k in 1..50 {
		term *= (x / 2.0) / k as f64;
		sum += term * term;
	}
	sum
}

impl TruePeakDetector {
	fn new() -> Self {
		let n = TP_PHASES * TP_TAPS;
		let center = (n - 1) as f64 / 2.0;
		let beta = 8.0;
		let mut kernel = [[0.0f32; TP_TAPS]; TP_PHASES];
		for (p, phase) in kernel.iter_mut().enumerate() {
			let mut taps = [0.0f64; TP_TAPS];
			for (j, tap) in taps.iter_mut().enumerate() {
				let k = j * TP_PHASES + p;
				let x = (k as f64 - center) / TP_PHASES as f64;
				let sinc = if x.abs() < 1e-12 {
					1.0
				} else {
					(PI * x).sin() / (PI * x)
				};
				let r = 2.0 * k as f64 / (n - 1) as f64 - 1.0;
				let w = bessel_i0(beta * (1.0 - r * r).max(0.0).sqrt()) / bessel_i0(beta);
				*tap = sinc * w;
			}
			let sum: f64 = taps.iter().sum();
			for (dst, src) in phase.iter_mut().zip(taps) {
				*dst = (src / sum) as f32;
			}
		}
		Self {
			kernel,
			history: [[0.0; 2 * TP_TAPS]; 2],
			pos: 0,
		}
	}

	fn reset(&mut self) {
		self.history = [[0.0; 2 * TP_TAPS]; 2];
		self.pos = 0;
	}

	/// Pushes one new stereo sample and returns the peak absolute value
	/// found across the two channels and all 4 oversampled phases.
	#[inline]
	fn push_peak(&mut self, l: f32, r: f32) -> f32 {
		self.pos = (self.pos + TP_TAPS - 1) % TP_TAPS;
		for (h, x) in self.history.iter_mut().zip([l, r]) {
			h[self.pos] = x;
			h[self.pos + TP_TAPS] = x;
		}
		let mut peak = 0.0f32;
		for h in &self.history {
			let window = &h[self.pos..self.pos + TP_TAPS];
			for phase in &self.kernel {
				let acc: f32 = phase.iter().zip(window).map(|(c, x)| c * x).sum();
				peak = peak.max(acc.abs());
			}
		}
		peak
	}
}

/// The lookahead gain path at one rate: window minimum, moving average,
/// release follower and the delayed, clamped audio.
#[derive(Debug, Default)]
struct Core {
	lookahead: usize,
	inv_len: f64,
	delay_l: DelayLine,
	delay_r: DelayLine,
	min: SlidingMin,
	avg_buf: Vec<f32>,
	avg_pos: usize,
	avg_sum: f64,
	env: f32,
	release_coeff: f32,
	/// Lowest gain of the current block.
	min_gain: f32,
}

impl Core {
	fn allocate(&mut self, lookahead: usize, release_ms: f32, sample_rate: f32) {
		self.lookahead = lookahead;
		self.inv_len = 1.0 / lookahead as f64;
		self.delay_l.allocate(lookahead + 1);
		self.delay_r.allocate(lookahead + 1);
		self.min.allocate(lookahead + 1);
		self.avg_buf.clear();
		self.avg_buf.resize(lookahead, 1.0);
		self.set_release(release_ms, sample_rate);
		self.reset();
	}

	fn set_release(&mut self, release_ms: f32, sample_rate: f32) {
		self.release_coeff = one_pole_coeff(release_ms, sample_rate);
	}

	fn reset(&mut self) {
		self.delay_l.clear();
		self.delay_r.clear();
		self.min.clear();
		self.avg_buf.fill(1.0);
		self.avg_pos = 0;
		self.avg_sum = self.avg_buf.len() as f64;
		self.env = 1.0;
		self.min_gain = 1.0;
	}

	/// One sample: `peak` is the detector's value for `(xl, xr)`.
	#[inline]
	fn run(
		&mut self,
		xl: f32,
		xr: f32,
		peak: f32,
		ceiling: f32,
		detect_ceiling: f32,
	) -> (f32, f32) {
		let la = self.lookahead;
		let need = if peak > detect_ceiling {
			detect_ceiling / peak
		} else {
			1.0
		};
		let held = self.min.push(need);
		self.avg_sum += held as f64 - self.avg_buf[self.avg_pos] as f64;
		self.avg_buf[self.avg_pos] = held;
		self.avg_pos = (self.avg_pos + 1) % la;
		let smooth = ((self.avg_sum * self.inv_len) as f32).min(1.0);
		self.env = if smooth < self.env {
			smooth
		} else {
			self.env + (smooth - self.env) * self.release_coeff
		};
		self.min_gain = self.min_gain.min(self.env);

		self.delay_l.push(xl);
		self.delay_r.push(xr);
		let dl = self.delay_l.read_int(la + 1);
		let dr = self.delay_r.read_int(la + 1);
		(
			(dl * self.env).clamp(-ceiling, ceiling),
			(dr * self.env).clamp(-ceiling, ceiling),
		)
	}

	/// Ends a block: returns its lowest gain and re-anchors the running sum so
	/// float drift cannot accumulate.
	fn end_block(&mut self) -> f32 {
		self.avg_sum = self.avg_buf.iter().map(|&v| v as f64).sum();
		std::mem::replace(&mut self.min_gain, 1.0)
	}
}

/// Linear-phase polyphase up/down sampler by `factor` around the oversampled
/// limiter: a Kaiser-windowed sinc of `factor * OS_TAPS + 1` taps (cutoff 0.45 of
/// the base rate), used to interpolate (x factor gain) and to decimate. Both
/// filters delay by `factor * OS_TAPS / 2` high-rate samples, so the pair adds
/// exactly [`OS_LATENCY`] base samples. Buffers are allocated in `new`.
#[derive(Debug)]
struct Oversampler {
	factor: usize,
	kernel: Vec<f32>,
	/// Base-rate input history per channel, duplicated so any window is contiguous.
	up: [Vec<f32>; 2],
	up_pos: usize,
	/// High-rate output history per channel, duplicated likewise.
	down: [Vec<f32>; 2],
	down_pos: usize,
}

impl Oversampler {
	fn new(factor: usize) -> Self {
		let n = factor * OS_TAPS + 1;
		let center = (n - 1) as f64 / 2.0;
		let cutoff = 0.45 / factor as f64;
		let beta = 8.0;
		let mut kernel: Vec<f64> = (0..n)
			.map(|k| {
				let x = k as f64 - center;
				let sinc = if x.abs() < 1e-12 {
					2.0 * cutoff
				} else {
					(2.0 * PI * cutoff * x).sin() / (PI * x)
				};
				let r = 2.0 * k as f64 / (n - 1) as f64 - 1.0;
				sinc * bessel_i0(beta * (1.0 - r * r).max(0.0).sqrt()) / bessel_i0(beta)
			})
			.collect();
		let sum: f64 = kernel.iter().sum();
		for k in &mut kernel {
			*k /= sum;
		}
		let up_len = OS_TAPS + 1;
		Self {
			factor,
			kernel: kernel.into_iter().map(|k| k as f32).collect(),
			up: [vec![0.0; 2 * up_len], vec![0.0; 2 * up_len]],
			up_pos: 0,
			down: [vec![0.0; 2 * n], vec![0.0; 2 * n]],
			down_pos: 0,
		}
	}

	fn reset(&mut self) {
		for h in self.up.iter_mut().chain(&mut self.down) {
			h.fill(0.0);
		}
		self.up_pos = 0;
		self.down_pos = 0;
	}

	/// Pushes one base-rate sample per channel; `phase(p)` then gives the `p`-th
	/// interpolated point (p = 0 is the oldest).
	#[inline]
	fn push_base(&mut self, l: f32, r: f32) {
		let len = OS_TAPS + 1;
		self.up_pos = (self.up_pos + len - 1) % len;
		for (h, x) in self.up.iter_mut().zip([l, r]) {
			h[self.up_pos] = x;
			h[self.up_pos + len] = x;
		}
	}

	#[inline]
	fn phase(&self, p: usize) -> (f32, f32) {
		let len = OS_TAPS + 1;
		let f = self.factor;
		let gain = f as f32;
		let mut out = [0.0f32; 2];
		for (o, h) in out.iter_mut().zip(&self.up) {
			let window = &h[self.up_pos..self.up_pos + len];
			let mut acc = 0.0f32;
			for (j, x) in window.iter().enumerate() {
				if let Some(c) = self.kernel.get(p + j * f) {
					acc += c * x;
				}
			}
			*o = acc * gain;
		}
		(out[0], out[1])
	}

	/// Pushes one high-rate sample per channel.
	#[inline]
	fn push_high(&mut self, l: f32, r: f32) {
		let n = self.kernel.len();
		self.down_pos = (self.down_pos + n - 1) % n;
		for (h, x) in self.down.iter_mut().zip([l, r]) {
			h[self.down_pos] = x;
			h[self.down_pos + n] = x;
		}
	}

	/// The decimated value at the newest high-rate sample.
	#[inline]
	fn decimate(&self) -> (f32, f32) {
		let n = self.kernel.len();
		let mut out = [0.0f32; 2];
		for (o, h) in out.iter_mut().zip(&self.down) {
			let window = &h[self.down_pos..self.down_pos + n];
			*o = self.kernel.iter().zip(window).map(|(c, x)| c * x).sum();
		}
		(out[0], out[1])
	}
}

pub struct Limiter {
	params: ParamValues<N_PARAMS>,
	sample_rate: f32,
	lookahead: usize,
	/// One gain path per mode of [`OVERSAMPLE_FACTORS`].
	cores: [Core; 3],
	oversamplers: [Oversampler; 2],
	mode: usize,
	input: Smoother,
	ceiling: Smoother,
	gr_meter: f32,
	true_peak: TruePeakDetector,
	/// The same 4x estimator on the 2x / 4x oversampled signal.
	os_true_peak: [TruePeakDetector; 2],
}

impl Default for Limiter {
	fn default() -> Self {
		Self::new()
	}
}

impl Limiter {
	pub fn new() -> Self {
		let params = ParamValues::new(&INFO);
		Self {
			sample_rate: 48_000.0,
			lookahead: 0,
			cores: Default::default(),
			oversamplers: [Oversampler::new(2), Oversampler::new(4)],
			mode: 0,
			input: Smoother::new(db_to_gain(params.get(P_INPUT))),
			ceiling: Smoother::new(db_to_gain(params.get(P_CEILING))),
			gr_meter: 0.0,
			true_peak: TruePeakDetector::new(),
			os_true_peak: [TruePeakDetector::new(), TruePeakDetector::new()],
			params,
		}
	}

	/// Largest gain reduction during the last `process` call, positive dB.
	pub fn gain_reduction_db(&self) -> f32 {
		self.gr_meter
	}

	fn mode_of(v: f32) -> usize {
		(v.round().max(0.0) as usize).min(2)
	}

	fn set_releases(&mut self) {
		let release = self.params.get(P_RELEASE);
		for (core, f) in self.cores.iter_mut().zip(OVERSAMPLE_FACTORS) {
			core.set_release(release, self.sample_rate * f as f32);
		}
	}
}

impl Processor for Limiter {
	fn info(&self) -> &'static DeviceInfo {
		&INFO
	}

	fn prepare(&mut self, sample_rate: f32, _max_block: usize) {
		self.sample_rate = sample_rate.max(1.0);
		self.lookahead = ((LOOKAHEAD_MS * 0.001 * self.sample_rate).round() as usize).max(1);
		let release = self.params.get(P_RELEASE);
		for (core, f) in self.cores.iter_mut().zip(OVERSAMPLE_FACTORS) {
			core.allocate(self.lookahead * f, release, self.sample_rate * f as f32);
		}
		self.input.prepare(self.sample_rate, 20.0);
		self.ceiling.prepare(self.sample_rate, 20.0);
		self.reset();
	}

	fn set_param(&mut self, index: usize, value: f32) {
		let Some(v) = self.params.set(&INFO, index, value) else {
			return;
		};
		match index {
			P_INPUT => self.input.set_target(db_to_gain(v)),
			P_CEILING => self.ceiling.set_target(db_to_gain(v)),
			P_RELEASE => self.set_releases(),
			P_OVERSAMPLE => {
				let mode = Self::mode_of(v);
				if mode != self.mode {
					self.mode = mode;
					self.cores[mode].reset();
					if mode > 0 {
						self.oversamplers[mode - 1].reset();
						self.os_true_peak[mode - 1].reset();
					}
				}
			}
			_ => {}
		}
	}

	fn process(
		&mut self,
		left: &mut [f32],
		right: &mut [f32],
		_events: &[MidiEvent],
		_transport: &Transport,
	) {
		if self.cores[0].avg_buf.is_empty() {
			// Not prepared: behave as a safe clipper rather than doing nothing.
			let c = self.ceiling.value();
			for x in left.iter_mut().chain(right.iter_mut()) {
				*x = x.clamp(-c, c);
			}
			return;
		}
		let mode = self.mode;
		for (l, r) in left.iter_mut().zip(right.iter_mut()) {
			let input = self.input.tick();
			let ceiling = self.ceiling.tick();
			let xl = *l * input;
			let xr = *r * input;
			if mode == 0 {
				// Detect on the 4x-oversampled estimate so inter-sample peaks drive
				// gain reduction, not just the raw sample value (which the plain
				// `xl.abs().max(xr.abs())` sample peak would under-report). The
				// moving-average gain path was derived against a per-sample
				// detector; against the oversampled one it is a (very close)
				// approximation rather than an exact bound, so aim the sidechain
				// a hair under the ceiling to absorb that approximation error
				// while the hard clamp below still enforces the true ceiling.
				let detect_ceiling = ceiling * TP_SAFETY_MARGIN;
				let peak = self.true_peak.push_peak(xl, xr).max(xl.abs()).max(xr.abs());
				(*l, *r) = self.cores[0].run(xl, xr, peak, ceiling, detect_ceiling);
			} else {
				let os = &mut self.oversamplers[mode - 1];
				let tp = &mut self.os_true_peak[mode - 1];
				let core = &mut self.cores[mode];
				let detect_ceiling = ceiling * OS_SAFETY_MARGIN[mode];
				os.push_base(xl, xr);
				let mut out = (0.0, 0.0);
				for p in 0..os.factor {
					let (hl, hr) = os.phase(p);
					let peak = tp.push_peak(hl, hr).max(hl.abs()).max(hr.abs());
					let (yl, yr) = core.run(hl, hr, peak, ceiling, detect_ceiling);
					os.push_high(yl, yr);
					if p == 0 {
						out = os.decimate();
					}
				}
				*l = out.0.clamp(-ceiling, ceiling);
				*r = out.1.clamp(-ceiling, ceiling);
			}
		}
		let min_gain = self.cores[mode].end_block();
		self.gr_meter = -gain_to_db(min_gain);
	}

	fn readings(&self, out: &mut [f32]) -> usize {
		match out.get_mut(crate::readings::GAIN_REDUCTION_DB) {
			Some(gr) => {
				*gr = self.gr_meter;
				1
			}
			None => 0,
		}
	}

	fn reset(&mut self) {
		for core in &mut self.cores {
			core.reset();
		}
		for os in &mut self.oversamplers {
			os.reset();
		}
		for tp in &mut self.os_true_peak {
			tp.reset();
		}
		self.gr_meter = 0.0;
		self.input.snap();
		self.ceiling.snap();
		self.true_peak.reset();
	}

	fn latency(&self) -> u32 {
		limiter_latency_samples(self.mode as f32, self.sample_rate)
	}

	fn tail_seconds(&self) -> f32 {
		LOOKAHEAD_MS * 0.001
			+ if self.mode == 0 {
				0.0
			} else {
				OS_LATENCY as f32 / self.sample_rate
			}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn sliding_min_window() {
		let mut m = SlidingMin::default();
		m.allocate(3);
		let out: Vec<f32> = [5.0, 3.0, 4.0, 6.0, 7.0, 8.0, 1.0]
			.into_iter()
			.map(|v| m.push(v))
			.collect();
		assert_eq!(out, vec![5.0, 3.0, 3.0, 3.0, 4.0, 6.0, 1.0]);
	}

	#[test]
	fn never_exceeds_ceiling_and_delays_by_latency() {
		let mut lim = Limiter::new();
		lim.prepare(48_000.0, 512);
		lim.set_param(P_INPUT, 12.0);
		lim.set_param(P_CEILING, -3.0);
		lim.reset();
		let ceiling = db_to_gain(-3.0);
		let mut rng = crate::util::Rng::new(7);
		let mut l: Vec<f32> = (0..96_000)
			.map(|n| {
				let burst = if (n / 4_000) % 3 == 0 { 1.5 } else { 0.2 };
				(n as f32 * 0.05).sin() * burst + rng.bipolar() * 0.3 * burst
			})
			.collect();
		let mut r: Vec<f32> = l.iter().map(|x| -x * 0.8).collect();
		let mut pos = 0;
		let mut size = 1;
		while pos < l.len() {
			let end = (pos + size).min(l.len());
			lim.process(
				&mut l[pos..end],
				&mut r[pos..end],
				&[],
				&Transport::default(),
			);
			pos = end;
			size = size % 511 + 7;
		}
		let peak = l.iter().chain(&r).fold(0.0_f32, |m, x| m.max(x.abs()));
		assert!(peak <= ceiling * 1.000_01, "peak {peak} > {ceiling}");
		assert!(lim.latency() == 240);
	}

	#[test]
	fn impulse_comes_out_after_latency() {
		let mut lim = Limiter::new();
		lim.prepare(48_000.0, 1024);
		let mut l = vec![0.0_f32; 1024];
		let mut r = vec![0.0_f32; 1024];
		l[10] = 0.5;
		lim.process(&mut l, &mut r, &[], &Transport::default());
		let idx = l.iter().position(|&x| x != 0.0).unwrap();
		assert_eq!(idx, 10 + lim.latency() as usize);
		assert!((l[idx] - 0.5).abs() < 1e-6);
	}

	/// Independent offline 4x-oversampled true-peak meter, deliberately not
	/// sharing code or tap count with `TruePeakDetector`: a symmetric
	/// (non-causal, whole-signal) windowed-sinc upsampler, closer in spirit
	/// to an off-line BS.1770 true-peak measurement than the limiter's own
	/// causal online estimator.
	fn measure_true_peak(l: &[f32], r: &[f32]) -> f32 {
		const TAPS: usize = 96; // per side, i.e. a 193-tap sinc per phase
		const OS: usize = 4;
		let kernel = |phase_offset: f64| -> Vec<f64> {
			(-(TAPS as isize)..=TAPS as isize)
				.map(|i| {
					let x = i as f64 - phase_offset;
					let sinc = if x.abs() < 1e-9 {
						1.0
					} else {
						(PI * x).sin() / (PI * x)
					};
					// Hann window for stop-band rejection.
					let t = (i as f64 + TAPS as f64) / (2.0 * TAPS as f64);
					let win = 0.5 - 0.5 * (2.0 * PI * t).cos();
					sinc * win
				})
				.collect()
		};
		let kernels: Vec<Vec<f64>> = (0..OS).map(|p| kernel(p as f64 / OS as f64)).collect();
		let mut peak = 0.0f32;
		for chan in [l, r] {
			for n in 0..chan.len() {
				for k in &kernels {
					let mut acc = 0.0f64;
					for (j, &c) in k.iter().enumerate() {
						let idx = n as isize + j as isize - TAPS as isize;
						if idx >= 0 && (idx as usize) < chan.len() {
							acc += c * chan[idx as usize] as f64;
						}
					}
					peak = peak.max(acc.abs() as f32);
				}
			}
		}
		peak
	}

	#[test]
	fn true_peak_stays_under_ceiling_for_hot_dense_program() {
		let sr = 48_000.0;
		let mut lim = Limiter::new();
		lim.prepare(sr, 256);
		lim.set_param(P_INPUT, 12.0);
		let ceiling_db = -1.0;
		lim.set_param(P_CEILING, ceiling_db);
		lim.reset();

		let n = 48_000;
		let mut rng = crate::util::Rng::new(42);
		// Dense, hot program with classic inter-sample-peak offenders: a pair
		// of fs/4 tones 45 degrees apart (worst case for a sample-peak
		// detector) plus other partials and noise, summed and scaled so the
		// sample peak sits around 0 dBFS before the limiter's +12 dB drive.
		let mut l: Vec<f32> = (0..n)
			.map(|i| {
				let t = i as f64;
				let a = (std::f64::consts::FRAC_PI_2 * t + std::f64::consts::FRAC_PI_4).sin();
				let b = (std::f64::consts::FRAC_PI_2 * t * 1.001 + 0.9).sin();
				let c = (2.0 * PI * 3_000.0 / sr as f64 * t).sin();
				let noise = rng.bipolar() as f64 * 0.2;
				(0.45 * a + 0.35 * b + 0.25 * c + noise) as f32
			})
			.collect();
		let mut r: Vec<f32> = l
			.iter()
			.enumerate()
			.map(|(i, &x)| {
				let t = i as f64;
				x * 0.9 + 0.1 * (2.0 * PI * 3_050.0 / sr as f64 * t).sin() as f32
			})
			.collect();

		let mut pos = 0;
		let mut size = 1;
		while pos < l.len() {
			let end = (pos + size).min(l.len());
			lim.process(
				&mut l[pos..end],
				&mut r[pos..end],
				&[],
				&Transport::default(),
			);
			pos = end;
			size = size % 377 + 11;
		}

		let tp = measure_true_peak(&l, &r);
		let tp_db = gain_to_db(tp);
		let limit_db = ceiling_db + 0.1;
		assert!(
			tp_db <= limit_db,
			"measured true peak {tp_db} dBTP exceeds ceiling {ceiling_db} + 0.1 dB"
		);
	}

	fn run_blocks(lim: &mut Limiter, l: &mut [f32], r: &mut [f32]) {
		let mut pos = 0;
		let mut size = 1;
		while pos < l.len() {
			let end = (pos + size).min(l.len());
			lim.process(
				&mut l[pos..end],
				&mut r[pos..end],
				&[],
				&Transport::default(),
			);
			pos = end;
			size = size % 377 + 11;
		}
	}

	#[test]
	fn oversampled_impulse_comes_out_after_the_reported_latency() {
		for (mode, factor) in [(1.0, 2), (2.0, 4)] {
			let mut lim = Limiter::new();
			lim.prepare(48_000.0, 1024);
			lim.set_param(P_OVERSAMPLE, mode);
			assert_eq!(lim.latency(), 240 + OS_LATENCY as u32);
			assert_eq!(lim.latency(), limiter_latency_samples(mode, 48_000.0));
			// A band-limited click (Hann-windowed 2 kHz burst) keeps its shape and place.
			let mut l = vec![0.0_f32; 2048];
			for (i, x) in l[100..148].iter_mut().enumerate() {
				let w = 0.5 - 0.5 * (2.0 * PI * i as f64 / 48.0).cos();
				*x = (0.5 * w * (2.0 * PI * 2_000.0 * i as f64 / 48_000.0).sin()) as f32;
			}
			let input = l.clone();
			let mut r = vec![0.0_f32; 2048];
			run_blocks(&mut lim, &mut l, &mut r);
			let la = lim.latency() as usize;
			let err = input[..2048 - la]
				.iter()
				.zip(&l[la..])
				.fold(0.0_f32, |m, (a, b)| m.max((a - b).abs()));
			assert!(err < 2e-3, "{factor}x: {err}");
		}
	}

	/// True peak relative to the -1 dB ceiling (dB, > 0 = over), measured by the
	/// independent meter; also checks the sample peak never passes the ceiling.
	fn true_peak_over(mode: f32, signal: &dyn Fn(usize) -> f32) -> f32 {
		let mut lim = Limiter::new();
		lim.prepare(48_000.0, 256);
		lim.set_param(P_INPUT, 24.0);
		lim.set_param(P_CEILING, -1.0);
		lim.set_param(P_RELEASE, 1.0);
		lim.set_param(P_OVERSAMPLE, mode);
		lim.reset();
		let n = 24_000;
		let mut l: Vec<f32> = (0..n).map(signal).collect();
		let mut r = l.clone();
		run_blocks(&mut lim, &mut l, &mut r);
		let skip = lim.latency() as usize + 64;
		let peak = l.iter().chain(&r).fold(0.0_f32, |m, x| m.max(x.abs()));
		assert!(peak <= db_to_gain(-1.0) * 1.000_01, "sample peak {peak}");
		gain_to_db(measure_true_peak(&l[skip..], &r[skip..])) + 1.0
	}

	#[test]
	fn oversampling_keeps_inter_sample_peaks_closer_to_the_ceiling() {
		// Driven 24 dB with a 1 ms release, inter-sample-peak offenders: a fs/4 pair
		// at 45 degrees under a fast gate with 11 kHz, two partials beating at 9.1 and
		// 15 kHz over noise, a 17 kHz tone gated every 97 samples, and a 22.7/23 kHz
		// pair whose peaks fall between the samples.
		let mut rng = crate::util::Rng::new(3);
		let noise: Vec<f32> = (0..24_000).map(|_| rng.bipolar()).collect();
		let sine = |hz: f64, t: f64, phase: f64| (2.0 * PI * hz / 48_000.0 * t + phase).sin();
		let signals: [&dyn Fn(usize) -> f32; 4] = [
			&|i| {
				let t = i as f64;
				let gate = if (i / 800) % 2 == 0 { 1.0 } else { 0.25 };
				let quarter = (std::f64::consts::FRAC_PI_2 * t + std::f64::consts::FRAC_PI_4).sin();
				(gate * (0.5 * sine(11_000.0, t, 0.0) + 0.4 * quarter)) as f32
			},
			&|i| {
				let t = i as f64;
				let gate = if (i / 300) % 2 == 0 { 1.0 } else { 0.1 };
				(gate * 0.5 * (sine(15_000.0, t, 0.0) + sine(9_100.0, t, 0.0))) as f32
					+ noise[i] * 0.3
			},
			&|i| {
				let gate = if (i / 97) % 2 == 0 { 1.0 } else { 0.05 };
				(0.9 * gate * sine(17_000.0, i as f64, 0.0)) as f32
			},
			&|i| {
				let t = i as f64;
				(0.5 * (sine(23_000.0, t, 0.0) + sine(22_700.0, t, 0.3))) as f32
			},
		];
		let worst = |mode: f32| {
			signals
				.iter()
				.map(|s| true_peak_over(mode, s))
				.fold(f32::MIN, f32::max)
		};
		let (x1, x4) = (worst(0.0), worst(2.0));
		assert!(
			x4 < x1,
			"worst true peak vs ceiling: 4x {x4} dB, 1x {x1} dB"
		);
		assert!(x4 <= 0.0 && x1 <= 0.1, "4x {x4} dB, 1x {x1} dB");
		assert!(worst(1.0) <= 0.1);
	}
}
