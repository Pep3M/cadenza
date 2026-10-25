//! `pitch_correct`: realtime pitch correction to a key and scale (retune speed, humanize,
//! transpose).
//!
//! Detection: YIN on a decimated mono copy (about 12 kHz) every 4 ms over 40 ms, refined
//! on the full-rate signal. Shifting: streaming TD-PSOLA. The output runs `latency`
//! samples behind the input (half the detection window, so each estimate is centered
//! on the audio it corrects); consecutive synthesis marks `s_j < s_j+1` (spaced by the
//! input period / ratio) each read the input around an analysis mark `a_j` (on the
//! input's period train, the one nearest `s_j`), and the two read heads crossfade
//! cos² / sin² between the marks. The windows sum to one, so ratio 1 is the input itself,
//! and the grains keep the spectral envelope (formants) where it was.

use crate::processor::{DeviceInfo, DeviceRole, MidiEvent, ParamInfo, Processor, Transport};
use crate::util::biquad::{Biquad, BiquadCoeffs};
use crate::util::interp::hermite;
use crate::util::{PI, ParamValues};

const P_KEY: usize = 0;
const P_SCALE: usize = 1;
const P_RETUNE: usize = 2;
const P_HUMANIZE: usize = 3;
const P_AMOUNT: usize = 4;
const P_TRANSPOSE: usize = 5;
const P_REFERENCE: usize = 6;
const N_PARAMS: usize = 7;

/// Lowest fundamental tracked.
pub const MIN_HZ: f32 = 50.0;
/// Highest fundamental tracked.
pub const MAX_HZ: f32 = 1_100.0;
const DETECT_RATE: f32 = 12_000.0;
const WINDOW_S: f32 = 0.04;
const HOP_S: f32 = 0.004;
/// Grain spacing while unpitched (consonants, breaths, silence): passed through.
const UNVOICED_SPACING_S: f64 = 0.005;
/// Largest shift, semitones (correction + transpose).
const MAX_SHIFT: f32 = 24.0;
/// CMND dip that starts a pitched stretch / keeps it going.
const VOICED_ON: f32 = 0.2;
const VOICED_KEEP: f32 = 0.35;
/// Quieter than this (dBFS RMS of the detection window) is unpitched.
const GATE_DB: f32 = -55.0;
/// Humanize leaves sustained notes alone only after they settled this long.
const HUMANIZE_AFTER_S: f32 = 0.1;
/// Humanize at 100 %: deviations slower than this time constant are corrected.
const HUMANIZE_MAX_S: f32 = 0.5;
/// A note keeps its target until another scale note is this much closer (semitones).
const HYSTERESIS: f32 = 0.15;

const fn scale(intervals: &[u8]) -> u16 {
	let mut mask = 0u16;
	let mut i = 0;
	while i < intervals.len() {
		mask |= 1 << intervals[i];
		i += 1;
	}
	mask
}

/// Scale names in `scale` param order (index = value).
pub const SCALE_NAMES: [&str; 12] = [
	"Chromatic",
	"Major",
	"Minor",
	"Harmonic minor",
	"Melodic minor",
	"Dorian",
	"Phrygian",
	"Lydian",
	"Mixolydian",
	"Major pentatonic",
	"Minor pentatonic",
	"Blues",
];

/// Pitch classes above the key of each scale, bit n = n semitones.
pub const SCALE_MASKS: [u16; 12] = [
	0xFFF,
	scale(&[0, 2, 4, 5, 7, 9, 11]),
	scale(&[0, 2, 3, 5, 7, 8, 10]),
	scale(&[0, 2, 3, 5, 7, 8, 11]),
	scale(&[0, 2, 3, 5, 7, 9, 11]),
	scale(&[0, 2, 3, 5, 7, 9, 10]),
	scale(&[0, 1, 3, 5, 7, 8, 10]),
	scale(&[0, 2, 4, 6, 7, 9, 11]),
	scale(&[0, 2, 4, 5, 7, 9, 10]),
	scale(&[0, 2, 4, 7, 9]),
	scale(&[0, 3, 5, 7, 10]),
	scale(&[0, 3, 5, 6, 7, 10]),
];

static PARAMS: [ParamInfo; N_PARAMS] = [
	ParamInfo::new(
		"key",
		"Key",
		0.0,
		11.0,
		0.0,
		"",
		"Root of the scale: 0 = C, 1 = C#, 2 = D ... 11 = B.",
	),
	ParamInfo::new(
		"scale",
		"Scale",
		0.0,
		11.0,
		0.0,
		"",
		"Notes the voice is pulled to: 0 chromatic (every semitone), 1 major, 2 minor, 3 harmonic \
		 minor, 4 melodic minor, 5 dorian, 6 phrygian, 7 lydian, 8 mixolydian, 9 major \
		 pentatonic, 10 minor pentatonic, 11 blues.",
	),
	ParamInfo::new(
		"retune",
		"Retune",
		0.0,
		400.0,
		40.0,
		"ms",
		"How fast the pitch moves to the target note (time constant). 0 = instant (the hard, \
		 robotic tuned effect), 10-30 tight modern pop, 40-80 natural, 120+ gentle.",
	),
	ParamInfo::new(
		"humanize",
		"Humanize",
		0.0,
		100.0,
		0.0,
		"%",
		"Keeps the singer's natural movement inside sustained notes: only their average is \
		 corrected while short notes still snap. 0 = off, 30-60 natural with a fast retune.",
	),
	ParamInfo::new(
		"amount",
		"Amount",
		0.0,
		100.0,
		100.0,
		"%",
		"Share of the correction applied (100 = to the note, 50 = halfway).",
	),
	ParamInfo::new(
		"transpose",
		"Transpose",
		-12.0,
		12.0,
		0.0,
		"st",
		"Shifts the output by semitones after correction (formants stay).",
	),
	ParamInfo::new(
		"reference",
		"Reference",
		415.0,
		466.0,
		440.0,
		"Hz",
		"Tuning of A4 the notes are measured against (440 standard, 432 / 442 alternatives).",
	),
];

pub static INFO: DeviceInfo = DeviceInfo {
	id: "pitch_correct",
	name: "Pitch Correct",
	role: DeviceRole::Effect,
	description: "Realtime pitch correction for a solo voice or monophonic instrument (not \
		chords): pulls each sung note to the nearest note of a key and scale while it plays, \
		keeping formants. Put it first on a lead vocal track. For per-note offline tuning use \
		the audio note tools instead. Recipes - NATURAL: retune 50, humanize 40. MODERN POP: \
		retune 15, humanize 20. HARD TUNE EFFECT: retune 0, humanize 0. Set key and scale to \
		the song's (chromatic when unsure). A singer who is consistently sharp or flat everywhere (e.g. +30 cents) is pulled to the \
		nearest notes of that tuning when reference is set to match (448 Hz for +30 c), instead \
		of flipping between neighbours. Adds about 20 ms of latency (compensated).",
	params: &PARAMS,
};

/// The nearest note of `key` / `mask` to `pitch` (MIDI, fractional).
pub fn nearest_in_scale(pitch: f32, key: u8, mask: u16) -> f32 {
	let mask = if mask & 0xFFF == 0 { 0xFFF } else { mask };
	let center = pitch.round() as i32;
	let mut best = center as f32;
	let mut distance = f32::INFINITY;
	for n in center - 6..=center + 6 {
		if mask & (1 << (n - key as i32).rem_euclid(12)) == 0 {
			continue;
		}
		let d = (n as f32 - pitch).abs();
		if d < distance {
			distance = d;
			best = n as f32;
		}
	}
	best
}

fn allowed(note: f32, key: u8, mask: u16) -> bool {
	mask & 0xFFF == 0 || mask & (1 << (note as i32 - key as i32).rem_euclid(12)) != 0
}

pub struct PitchCorrect {
	params: ParamValues<N_PARAMS>,
	sample_rate: f32,
	latency: usize,
	/// Full-rate input rings (left, right, mono), length `mask + 1`.
	rings: [Vec<f32>; 3],
	mask: usize,
	/// Absolute index of the next input sample written.
	write: u64,
	// Detection on the decimated mono signal.
	factor: usize,
	lowpass: BiquadCoeffs,
	filters: [Biquad; 2],
	phase: usize,
	decimated: Vec<f32>,
	dec_mask: usize,
	dec_write: u64,
	hop: usize,
	since_hop: usize,
	window: usize,
	min_lag: usize,
	max_lag: usize,
	frame: Vec<f32>,
	cmnd: Vec<f32>,
	// Tracking.
	voiced: bool,
	/// Input period, full-rate samples.
	period: f64,
	/// Detected pitch (MIDI against the reference).
	detected: f32,
	/// The detected pitch humanize corrects (a slow average on sustained notes).
	anchor: f32,
	held: f32,
	/// Target scale note, NaN while unpitched.
	target: f32,
	/// Shift applied now, semitones.
	shift: f32,
	// Synthesis marks (absolute input sample positions).
	s_cur: f64,
	s_next: f64,
	a_cur: f64,
	a_next: f64,
}

impl Default for PitchCorrect {
	fn default() -> Self {
		Self::new()
	}
}

impl PitchCorrect {
	pub fn new() -> Self {
		let mut p = Self {
			params: ParamValues::new(&INFO),
			sample_rate: 0.0,
			latency: 0,
			rings: Default::default(),
			mask: 0,
			write: 0,
			factor: 1,
			lowpass: BiquadCoeffs::default(),
			filters: Default::default(),
			phase: 0,
			decimated: Vec::new(),
			dec_mask: 0,
			dec_write: 0,
			hop: 1,
			since_hop: 0,
			window: 1,
			min_lag: 2,
			max_lag: 3,
			frame: Vec::new(),
			cmnd: Vec::new(),
			voiced: false,
			period: 1.0,
			detected: 0.0,
			anchor: 0.0,
			held: 0.0,
			target: f32::NAN,
			shift: 0.0,
			s_cur: 0.0,
			s_next: 0.0,
			a_cur: 0.0,
			a_next: 0.0,
		};
		p.prepare(48_000.0, 512);
		p
	}

	/// Latency at `sample_rate`: half the detection window (so the estimate lines up with
	/// the output: on a real take the median error fell from 6 to 2 cents and p90 from 38
	/// to 8 against half the longest period, 10 ms), plus the interpolation taps.
	pub fn latency_for(sample_rate: f32) -> usize {
		(0.5 * sample_rate * WINDOW_S)
			.max(0.5 * sample_rate / MIN_HZ)
			.ceil() as usize
			+ 4
	}

	fn read(&self, ring: usize, pos: f64) -> f32 {
		let i = pos.floor();
		let t = (pos - i) as f32;
		let i = i as u64 as usize;
		let r = &self.rings[ring];
		let m = self.mask;
		hermite(
			r[i.wrapping_sub(1) & m],
			r[i & m],
			r[(i + 1) & m],
			r[(i + 2) & m],
			t,
		)
	}

	fn ratio(&self) -> f64 {
		2f64.powf(self.shift as f64 / 12.0)
	}

	/// Moves to the next pair of marks once the output reaches `s_next`.
	fn advance(&mut self) {
		self.s_cur = self.s_next;
		self.a_cur = self.a_next;
		if !self.voiced {
			self.s_next = self.s_cur + UNVOICED_SPACING_S * self.sample_rate as f64;
			self.a_next = self.s_next;
			return;
		}
		let pa = self.period;
		self.s_next = self.s_cur + pa / self.ratio();
		let m = ((self.s_next - self.a_cur) / pa).round().max(0.0);
		let mut a = self.a_cur + m * pa;
		// The read head may run ahead of the output by at most the latency.
		let ahead = (self.latency - 4) as f64;
		while a - self.s_next > ahead {
			a -= pa;
		}
		self.a_next = a;
	}

	fn push_detection(&mut self, mono: f32) {
		let mut x = mono;
		if self.factor > 1 {
			for f in &mut self.filters {
				x = f.process(&self.lowpass, x);
			}
			self.phase += 1;
			if self.phase < self.factor {
				return;
			}
			self.phase = 0;
		}
		self.decimated[self.dec_write as usize & self.dec_mask] = x;
		self.dec_write += 1;
		self.since_hop += 1;
		if self.since_hop >= self.hop {
			self.since_hop = 0;
			self.detect();
		}
	}

	/// One YIN estimate over the latest window, then the target and shift.
	fn detect(&mut self) {
		let (w, max_lag) = (self.window, self.max_lag);
		let len = w + max_lag;
		let start = self.dec_write - len as u64;
		for (i, slot) in self.frame.iter_mut().enumerate() {
			*slot = self.decimated[(start + i as u64) as usize & self.dec_mask];
		}
		let x = &self.frame;
		let tail = &x[max_lag..];
		let rms = (tail.iter().map(|v| v * v).sum::<f32>() / w as f32).sqrt();
		let mut period = None;
		if 20.0 * rms.max(1e-9).log10() > GATE_DB {
			let mut running = 0.0;
			self.cmnd[0] = 1.0;
			for lag in 1..=max_lag {
				let mut d = 0.0;
				for i in 0..w {
					let e = x[i] - x[i + lag];
					d += e * e;
				}
				running += d;
				self.cmnd[lag] = if running > 0.0 {
					d * lag as f32 / running
				} else {
					1.0
				};
			}
			let threshold = if self.voiced { VOICED_KEEP } else { VOICED_ON };
			let c = &self.cmnd;
			if let Some(mut lag) = (self.min_lag..max_lag).find(|&l| c[l] < threshold) {
				while lag + 1 < max_lag && c[lag + 1] < c[lag] {
					lag += 1;
				}
				let (a, b, cc) = (c[lag - 1], c[lag], c[lag + 1]);
				let denom = a - 2.0 * b + cc;
				let shift = if denom.abs() > 1e-12 {
					(0.5 * (a - cc) / denom).clamp(-1.0, 1.0)
				} else {
					0.0
				};
				period = Some(self.refine((lag as f32 + shift) * self.factor as f32));
			}
		}
		self.track(period);
	}

	/// The full-rate period near `estimate` (difference function + parabola).
	fn refine(&self, estimate: f32) -> f64 {
		if self.factor == 1 {
			return estimate as f64;
		}
		let f = self.factor as i64;
		let lo = (estimate.floor() as i64 - f).max(2);
		let hi = estimate.ceil() as i64 + f;
		let span = (self.window * self.factor) as u64;
		let end = self.write;
		let base = end - span - hi as u64 - 2;
		let m = &self.rings[2];
		let at = |i: u64| m[i as usize & self.mask];
		let diff = |lag: i64| -> f32 {
			let mut d = 0.0;
			for i in 0..span {
				let e = at(base + i) - at(base + i + lag as u64);
				d += e * e;
			}
			d
		};
		let mut best = (lo, f32::INFINITY);
		let mut prev = diff(lo - 1);
		let mut values = (f32::NAN, f32::NAN, f32::NAN);
		let mut cur = diff(lo);
		for lag in lo..=hi {
			let next = diff(lag + 1);
			if cur < best.1 {
				best = (lag, cur);
				values = (prev, cur, next);
			}
			prev = cur;
			cur = next;
		}
		let (a, b, c) = values;
		let denom = a - 2.0 * b + c;
		let shift = if denom.abs() > 1e-20 {
			(0.5 * (a - c) / denom).clamp(-1.0, 1.0)
		} else {
			0.0
		};
		best.0 as f64 + shift as f64
	}

	fn track(&mut self, period: Option<f64>) {
		let hop_s = self.hop as f32 * self.factor as f32 / self.sample_rate;
		let Some(period) = period else {
			self.voiced = false;
			self.target = f32::NAN;
			self.held = 0.0;
			return;
		};
		let reference = self.params.get(P_REFERENCE);
		let key = self.params.get(P_KEY) as u8;
		let mask = SCALE_MASKS[(self.params.get(P_SCALE) as usize).min(SCALE_MASKS.len() - 1)];
		let transpose = self.params.get(P_TRANSPOSE);
		let detected = 69.0 + 12.0 * (self.sample_rate / period as f32 / reference).log2();
		let starting = !self.voiced;
		let mut target = self.target;
		let nearest = nearest_in_scale(detected, key, mask);
		if target.is_nan()
			|| !allowed(target, key, mask)
			|| (detected - target).abs() > (detected - nearest).abs() + HYSTERESIS
		{
			target = nearest;
		}
		if starting || target != self.target {
			self.held = 0.0;
			self.anchor = detected;
		} else {
			self.held += hop_s;
			let tau = self.params.get(P_HUMANIZE) * 0.01 * HUMANIZE_MAX_S;
			if self.held > HUMANIZE_AFTER_S && tau > 0.0 {
				self.anchor += (detected - self.anchor) * (1.0 - (-hop_s / tau).exp());
			} else {
				self.anchor = detected;
			}
		}
		let amount = self.params.get(P_AMOUNT) * 0.01;
		let desired = (amount * (target - self.anchor) + transpose).clamp(-MAX_SHIFT, MAX_SHIFT);
		if starting {
			self.shift = transpose;
		}
		let retune = self.params.get(P_RETUNE) * 0.001;
		self.shift += if retune > 0.0 {
			(desired - self.shift) * (1.0 - (-hop_s / retune).exp())
		} else {
			desired - self.shift
		};
		self.voiced = true;
		self.period = period;
		self.detected = detected;
		self.target = target;
	}
}

impl Processor for PitchCorrect {
	fn info(&self) -> &'static DeviceInfo {
		&INFO
	}

	fn prepare(&mut self, sample_rate: f32, _max_block: usize) {
		let sr = sample_rate.max(8_000.0);
		self.sample_rate = sr;
		self.latency = Self::latency_for(sr);
		let size = ((sr * 0.15) as usize).next_power_of_two();
		self.mask = size - 1;
		for r in &mut self.rings {
			*r = vec![0.0; size];
		}
		self.factor = ((sr / DETECT_RATE).round() as usize).max(1);
		let rate = sr / self.factor as f32;
		self.lowpass = BiquadCoeffs::lowpass(0.25 * rate, std::f32::consts::FRAC_1_SQRT_2, sr);
		self.hop = ((HOP_S * rate) as usize).max(1);
		self.window = (WINDOW_S * rate) as usize;
		self.max_lag = (rate / MIN_HZ).ceil() as usize;
		self.min_lag = ((rate / MAX_HZ).floor() as usize).max(2);
		let dec = (self.window + self.max_lag + 1).next_power_of_two();
		self.decimated = vec![0.0; dec];
		self.dec_mask = dec - 1;
		self.frame = vec![0.0; self.window + self.max_lag];
		self.cmnd = vec![1.0; self.max_lag + 2];
		self.reset();
	}

	fn set_param(&mut self, index: usize, value: f32) {
		let Some(v) = self.params.set(&INFO, index, value) else {
			return;
		};
		if matches!(index, P_KEY | P_SCALE | P_TRANSPOSE) && v.fract() != 0.0 {
			self.params.set(&INFO, index, v.round());
		}
	}

	fn process(
		&mut self,
		left: &mut [f32],
		right: &mut [f32],
		_events: &[MidiEvent],
		_transport: &Transport,
	) {
		for (l, r) in left.iter_mut().zip(right.iter_mut()) {
			let idx = self.write as usize & self.mask;
			let mono = 0.5 * (*l + *r);
			self.rings[0][idx] = *l;
			self.rings[1][idx] = *r;
			self.rings[2][idx] = mono;
			self.write += 1;
			self.push_detection(mono);
			let t = (self.write - 1 - self.latency as u64) as f64;
			while t >= self.s_next {
				self.advance();
			}
			let frac = ((t - self.s_cur) / (self.s_next - self.s_cur)) as f32;
			let fade = (0.5 * PI * frac).cos();
			let (wa, wb) = (fade * fade, 1.0 - fade * fade);
			let pa = self.a_cur + (t - self.s_cur);
			let pb = self.a_next + (t - self.s_next);
			*l = wa * self.read(0, pa) + wb * self.read(0, pb);
			*r = wa * self.read(1, pa) + wb * self.read(1, pb);
		}
	}

	fn reset(&mut self) {
		for r in &mut self.rings {
			r.fill(0.0);
		}
		self.decimated.fill(0.0);
		for f in &mut self.filters {
			f.reset();
		}
		// Start a full ring in, so every read position stays positive.
		self.write = (self.mask + 1) as u64;
		self.dec_write = (self.dec_mask + 1) as u64;
		self.phase = 0;
		self.since_hop = 0;
		self.voiced = false;
		self.target = f32::NAN;
		self.held = 0.0;
		self.shift = 0.0;
		let t = (self.write - self.latency as u64) as f64;
		self.s_cur = t;
		self.s_next = t;
		self.a_cur = t;
		self.a_next = t;
	}

	fn latency(&self) -> u32 {
		self.latency as u32
	}

	fn readings(&self, out: &mut [f32]) -> usize {
		let values = if self.voiced {
			[
				self.detected,
				self.detected + self.shift,
				self.target + self.params.get(P_TRANSPOSE),
			]
		} else {
			[-1.0; 3]
		};
		let n = values.len().min(out.len());
		out[..n].copy_from_slice(&values[..n]);
		n
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::util::pitch::detect_pitch;

	const SR: f32 = 48_000.0;

	/// A voice-like tone (decaying harmonics) following `pitch(t)` (MIDI).
	fn voice(seconds: f32, pitch: impl Fn(f32) -> f32) -> Vec<f32> {
		let mut phase = 0.0f32;
		(0..(seconds * SR) as usize)
			.map(|n| {
				let t = n as f32 / SR;
				phase = (phase + 440.0 * 2f32.powf((pitch(t) - 69.0) / 12.0) / SR).fract();
				let w = std::f32::consts::TAU * phase;
				0.3 * (1..=8)
					.map(|h| (w * h as f32).sin() / h as f32)
					.sum::<f32>()
			})
			.collect()
	}

	fn run(p: &mut PitchCorrect, input: &[f32]) -> Vec<f32> {
		let mut l = input.to_vec();
		let mut r = input.to_vec();
		for (a, b) in l.chunks_mut(32).zip(r.chunks_mut(32)) {
			p.process(a, b, &[], &Transport::default());
		}
		l
	}

	fn device(params: &[(usize, f32)]) -> PitchCorrect {
		let mut p = PitchCorrect::new();
		p.prepare(SR, 512);
		for &(i, v) in params {
			p.set_param(i, v);
		}
		p
	}

	/// Sung pitch (MIDI) of `out` between `from` and `to` seconds.
	fn pitch_of(out: &[f32], from: f32, to: f32) -> f32 {
		let part = &out[(from * SR) as usize..(to * SR) as usize];
		let hz = detect_pitch(part, SR).expect("pitched");
		69.0 + 12.0 * (hz / 440.0).log2()
	}

	fn rms(x: &[f32]) -> f32 {
		(x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt()
	}

	#[test]
	fn scales_snap_to_their_nearest_note() {
		assert_eq!(nearest_in_scale(61.3, 0, SCALE_MASKS[1]), 62.0);
		assert_eq!(nearest_in_scale(61.3, 0, SCALE_MASKS[0]), 61.0);
		assert_eq!(
			nearest_in_scale(66.4, 4, SCALE_MASKS[6]),
			67.0,
			"E phrygian: G"
		);
		assert_eq!(
			nearest_in_scale(56.2, 9, SCALE_MASKS[10]),
			57.0,
			"A minor pentatonic"
		);
	}

	#[test]
	fn no_correction_is_the_input_delayed() {
		let input = voice(1.0, |t| 60.3 + 0.4 * (t * 9.0).sin());
		let mut p = device(&[(P_AMOUNT, 0.0)]);
		let out = run(&mut p, &input);
		let lat = p.latency;
		let err: f32 = out[lat..]
			.iter()
			.zip(&input)
			.map(|(a, b)| (a - b).abs())
			.fold(0.0, f32::max);
		assert!(err < 1e-5, "{err}");
		assert_eq!(p.latency(), PitchCorrect::latency_for(SR) as u32);
	}

	#[test]
	fn a_sharp_note_lands_on_the_note_with_its_level() {
		for (sung, scale, expected) in [(69.3, 0.0, 69.0), (57.4, 0.0, 57.0), (61.3, 1.0, 62.0)] {
			let input = voice(1.0, |_| sung);
			let mut p = device(&[(P_SCALE, scale), (P_RETUNE, 0.0)]);
			let out = run(&mut p, &input);
			let got = pitch_of(&out, 0.4, 0.9);
			assert!(
				(got - expected).abs() < 0.03,
				"{sung} -> {got}, expected {expected}"
			);
			let (a, b) = (rms(&input[19_000..43_000]), rms(&out[19_500..43_500]));
			assert!((20.0 * (b / a).log10()).abs() < 1.0, "level {a} -> {b}");
		}
	}

	#[test]
	fn transpose_and_reference_move_the_output() {
		let input = voice(1.0, |_| 64.0);
		let mut p = device(&[(P_TRANSPOSE, 3.0)]);
		let got = pitch_of(&run(&mut p, &input), 0.5, 0.9);
		assert!((got - 67.0).abs() < 0.03, "{got}");
		// Sung at 442 Hz tuning: in tune there, so nothing to correct.
		let input = voice(1.0, |_| 69.0 + 12.0 * (442.0f32 / 440.0).log2());
		let mut p = device(&[(P_REFERENCE, 442.0)]);
		let got = pitch_of(&run(&mut p, &input), 0.5, 0.9);
		assert!((got - 69.0786).abs() < 0.02, "{got}");
	}

	#[test]
	fn retune_speed_sets_how_fast_the_correction_arrives() {
		let input = voice(1.2, |_| 60.4);
		let mut slow = device(&[(P_RETUNE, 300.0)]);
		let out = run(&mut slow, &input);
		let early = pitch_of(&out, 0.08, 0.16);
		let late = pitch_of(&out, 1.0, 1.15);
		assert!(
			early > 60.15,
			"slow retune still near the sung pitch: {early}"
		);
		assert!((late - 60.0).abs() < 0.05, "{late}");
	}

	#[test]
	fn humanize_keeps_vibrato_on_a_sustained_note() {
		// 60.1 with a 5.5 Hz, ±35 cent vibrato (never nearer 59 or 61).
		let input = voice(2.0, |t| {
			60.1 + 0.35 * (std::f32::consts::TAU * 5.5 * t).sin()
		});
		let swing = |humanize: f32| {
			let mut p = device(&[(P_RETUNE, 5.0), (P_HUMANIZE, humanize)]);
			let mut l = input.clone();
			let mut r = input.clone();
			let (mut lo, mut hi, mut sum, mut n) = (f32::MAX, f32::MIN, 0.0, 0);
			let mut reading = [0.0; 3];
			for (i, (a, b)) in l.chunks_mut(32).zip(r.chunks_mut(32)).enumerate() {
				p.process(a, b, &[], &Transport::default());
				p.readings(&mut reading);
				if i * 32 > SR as usize && reading[1] > 0.0 {
					lo = lo.min(reading[1]);
					hi = hi.max(reading[1]);
					sum += reading[1];
					n += 1;
				}
			}
			(hi - lo, sum / n as f32)
		};
		let (hard, hard_mean) = swing(0.0);
		let (human, human_mean) = swing(80.0);
		assert!(hard < 0.15, "a fast retune flattens it: {hard}");
		assert!(human > 0.5, "humanize keeps it: {human}");
		assert!((hard_mean - 60.0).abs() < 0.05 && (human_mean - 60.0).abs() < 0.1);
	}

	#[test]
	fn melody_and_noise_stay_clean() {
		let notes = [60.2, 62.3, 64.1, 59.6];
		let mut rng = crate::util::Rng::new(9);
		let input: Vec<f32> = voice(1.6, |t| notes[((t / 0.4) as usize).min(3)])
			.into_iter()
			.chain((0..24_000).map(|_| rng.bipolar() * 0.2))
			.collect();
		let mut p = device(&[(P_RETUNE, 10.0)]);
		let out = run(&mut p, &input);
		for (i, &n) in notes.iter().enumerate() {
			let got = pitch_of(&out, 0.4 * i as f32 + 0.15, 0.4 * i as f32 + 0.38);
			assert!((got - n.round()).abs() < 0.05, "note {i}: {got}");
		}
		// No clicks: no sample jumps far beyond the input's own steepest step.
		let step = |x: &[f32]| {
			x.windows(2)
				.map(|w| (w[1] - w[0]).abs())
				.fold(0.0, f32::max)
		};
		assert!(step(&out[..76_800]) < 1.6 * step(&input[..76_800]));
		let lat = p.latency;
		let noise_in = rms(&input[80_000..98_000]);
		let noise_out = rms(&out[80_000 + lat..98_000 + lat]);
		assert!(
			(noise_out / noise_in - 1.0).abs() < 0.05,
			"{noise_in} {noise_out}"
		);
	}
}
