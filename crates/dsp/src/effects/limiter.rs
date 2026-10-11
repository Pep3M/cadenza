//! `limiter`: lookahead brickwall peak limiter.
//!
//! Gain path: per-sample required gain → sliding minimum over `L + 1`
//! samples → moving average over `L` samples → release follower, with the
//! audio delayed by `L`. The moving average of a window-minimum that covers
//! the delayed sample guarantees the applied gain is never above what that
//! sample needs, so peaks cannot overshoot, and the attack is a smooth ramp.

use crate::processor::{DeviceInfo, DeviceRole, MidiEvent, ParamInfo, Processor, Transport};
use crate::util::delay_line::DelayLine;
use crate::util::smoother::Smoother;
use crate::util::{ParamValues, db_to_gain, gain_to_db, one_pole_coeff};

const P_INPUT: usize = 0;
const P_CEILING: usize = 1;
const P_RELEASE: usize = 2;
const N_PARAMS: usize = 3;

const LOOKAHEAD_MS: f32 = 5.0;

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
];

pub static INFO: DeviceInfo = DeviceInfo {
	id: "limiter",
	name: "Limiter",
	role: DeviceRole::Effect,
	description: "Transparent lookahead brickwall limiter (5 ms latency): output never exceeds the \
		ceiling. Put it LAST on the master bus to catch peaks and raise loudness: ceiling -1, \
		release 100, input_gain 2-6 dB (watch that reduction stays modest). Can also guard \
		individual buses against overs.",
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

pub struct Limiter {
	params: ParamValues<N_PARAMS>,
	sample_rate: f32,
	lookahead: usize,
	delay_l: DelayLine,
	delay_r: DelayLine,
	min: SlidingMin,
	avg_buf: Vec<f32>,
	avg_pos: usize,
	avg_sum: f64,
	env: f32,
	release_coeff: f32,
	input: Smoother,
	ceiling: Smoother,
	gr_meter: f32,
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
			delay_l: DelayLine::default(),
			delay_r: DelayLine::default(),
			min: SlidingMin::default(),
			avg_buf: Vec::new(),
			avg_pos: 0,
			avg_sum: 0.0,
			env: 1.0,
			release_coeff: 1.0,
			input: Smoother::new(db_to_gain(params.get(P_INPUT))),
			ceiling: Smoother::new(db_to_gain(params.get(P_CEILING))),
			gr_meter: 0.0,
			params,
		}
	}

	/// Largest gain reduction during the last `process` call, positive dB.
	pub fn gain_reduction_db(&self) -> f32 {
		self.gr_meter
	}
}

impl Processor for Limiter {
	fn info(&self) -> &'static DeviceInfo {
		&INFO
	}

	fn prepare(&mut self, sample_rate: f32, _max_block: usize) {
		self.sample_rate = sample_rate.max(1.0);
		self.lookahead = ((LOOKAHEAD_MS * 0.001 * self.sample_rate).round() as usize).max(1);
		self.delay_l.allocate(self.lookahead + 1);
		self.delay_r.allocate(self.lookahead + 1);
		self.min.allocate(self.lookahead + 1);
		self.avg_buf.clear();
		self.avg_buf.resize(self.lookahead, 1.0);
		self.input.prepare(self.sample_rate, 20.0);
		self.ceiling.prepare(self.sample_rate, 20.0);
		self.release_coeff = one_pole_coeff(self.params.get(P_RELEASE), self.sample_rate);
		self.reset();
	}

	fn set_param(&mut self, index: usize, value: f32) {
		let Some(v) = self.params.set(&INFO, index, value) else {
			return;
		};
		match index {
			P_INPUT => self.input.set_target(db_to_gain(v)),
			P_CEILING => self.ceiling.set_target(db_to_gain(v)),
			P_RELEASE => self.release_coeff = one_pole_coeff(v, self.sample_rate),
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
		if self.avg_buf.is_empty() {
			// Not prepared: behave as a safe clipper rather than doing nothing.
			let c = self.ceiling.value();
			for x in left.iter_mut().chain(right.iter_mut()) {
				*x = x.clamp(-c, c);
			}
			return;
		}
		let la = self.lookahead;
		let inv_len = 1.0 / la as f64;
		let mut min_gain: f32 = 1.0;
		for (l, r) in left.iter_mut().zip(right.iter_mut()) {
			let input = self.input.tick();
			let ceiling = self.ceiling.tick();
			let xl = *l * input;
			let xr = *r * input;
			let peak = xl.abs().max(xr.abs());
			let need = if peak > ceiling { ceiling / peak } else { 1.0 };
			let held = self.min.push(need);
			self.avg_sum += held as f64 - self.avg_buf[self.avg_pos] as f64;
			self.avg_buf[self.avg_pos] = held;
			self.avg_pos = (self.avg_pos + 1) % la;
			let smooth = ((self.avg_sum * inv_len) as f32).min(1.0);
			self.env = if smooth < self.env {
				smooth
			} else {
				self.env + (smooth - self.env) * self.release_coeff
			};
			min_gain = min_gain.min(self.env);

			self.delay_l.push(xl);
			self.delay_r.push(xr);
			let dl = self.delay_l.read_int(la + 1);
			let dr = self.delay_r.read_int(la + 1);
			*l = (dl * self.env).clamp(-ceiling, ceiling);
			*r = (dr * self.env).clamp(-ceiling, ceiling);
		}
		// Re-anchor the running sum each block so float drift cannot accumulate.
		self.avg_sum = self.avg_buf.iter().map(|&v| v as f64).sum();
		self.gr_meter = -gain_to_db(min_gain);
	}

	fn reset(&mut self) {
		self.delay_l.clear();
		self.delay_r.clear();
		self.min.clear();
		self.avg_buf.fill(1.0);
		self.avg_pos = 0;
		self.avg_sum = self.avg_buf.len() as f64;
		self.env = 1.0;
		self.gr_meter = 0.0;
		self.input.snap();
		self.ceiling.snap();
	}

	fn latency(&self) -> u32 {
		self.lookahead as u32
	}

	fn tail_seconds(&self) -> f32 {
		LOOKAHEAD_MS * 0.001
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
}
