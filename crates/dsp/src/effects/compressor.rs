//! `compressor`: feed-forward, stereo-linked peak compressor with soft knee,
//! parallel (dry/wet) mix and an optional external sidechain (key) input.

use crate::processor::{DeviceInfo, DeviceRole, MidiEvent, ParamInfo, Processor, Transport};
use crate::util::smoother::Smoother;
use crate::util::{ParamValues, db_to_gain, flush, gain_to_db, one_pole_coeff};

const P_THRESHOLD: usize = 0;
const P_RATIO: usize = 1;
const P_ATTACK: usize = 2;
const P_RELEASE: usize = 3;
const P_KNEE: usize = 4;
const P_MAKEUP: usize = 5;
const P_MIX: usize = 6;
const N_PARAMS: usize = 7;

static PARAMS: [ParamInfo; N_PARAMS] = [
	ParamInfo::new(
		"threshold",
		"Threshold",
		-60.0,
		0.0,
		-18.0,
		"dB",
		"Level above which gain reduction starts. Set so peaks get 3-6 dB of reduction for gentle control.",
	),
	ParamInfo::new(
		"ratio",
		"Ratio",
		1.0,
		20.0,
		4.0,
		"ratio",
		"Compression ratio N:1. 1.5-2 bus glue, 3-4 vocals/bass, 6-10 punchy drums, 20 near-limiting.",
	),
	ParamInfo::new(
		"attack",
		"Attack",
		0.1,
		200.0,
		10.0,
		"ms",
		"How fast reduction engages. 10-30 ms lets transients through (punch); 0.1-3 ms tames peaks.",
	),
	ParamInfo::new(
		"release",
		"Release",
		5.0,
		2_000.0,
		120.0,
		"ms",
		"How fast reduction recovers. 50-150 ms for drums, 150-400 for vocals/bus.",
	),
	ParamInfo::new(
		"knee",
		"Knee",
		0.0,
		24.0,
		6.0,
		"dB",
		"Soft-knee width around the threshold; larger = smoother, more transparent onset.",
	),
	ParamInfo::new(
		"makeup",
		"Makeup Gain",
		-12.0,
		24.0,
		0.0,
		"dB",
		"Gain after compression to restore loudness (roughly the typical gain reduction).",
	),
	ParamInfo::new(
		"mix",
		"Mix",
		0.0,
		100.0,
		100.0,
		"%",
		"Dry/compressed blend. 30-60% = parallel ('New York') compression for drums/vocals.",
	),
];

pub static INFO: DeviceInfo = DeviceInfo {
	id: "compressor",
	name: "Compressor",
	role: DeviceRole::Effect,
	description: "Stereo-linked feed-forward compressor with soft knee, makeup and parallel mix. \
		Controls dynamics and adds punch/glue. Typical: VOCALS threshold -20, ratio 3, attack 5, \
		release 100, makeup 4. DRUM BUS ratio 4, attack 20, release 100, mix 50. BASS ratio 4, \
		attack 10, release 150. MIX BUS GLUE ratio 1.5-2, attack 30, release 200, knee 10, aim for \
		1-2 dB of reduction. Place after EQ; put a limiter (not this) last on the master. \
		SIDECHAIN: key it from another track with set_sidechain (e.g. bass or pad ducked by the \
		kick: threshold -30, ratio 8-10, attack 0.5, release 120-200); without a sidechain it \
		reacts to its own input.",
	params: &PARAMS,
};

/// Static gain computer: gain change in dB (<= 0) for an input level in dB.
fn gain_computer(level_db: f32, threshold: f32, ratio: f32, knee: f32) -> f32 {
	let over = level_db - threshold;
	let slope = 1.0 / ratio - 1.0;
	if knee > 0.0 && 2.0 * over.abs() <= knee {
		let x = over + knee * 0.5;
		slope * x * x / (2.0 * knee)
	} else if over > 0.0 {
		slope * over
	} else {
		0.0
	}
}

pub struct Compressor {
	params: ParamValues<N_PARAMS>,
	sample_rate: f32,
	attack_coeff: f32,
	release_coeff: f32,
	env_db: f32,
	gr_meter: f32,
	threshold: Smoother,
	makeup: Smoother,
	mix: Smoother,
	/// Sidechain signal for the next block (allocated in `prepare`).
	key_l: Vec<f32>,
	key_r: Vec<f32>,
	key_len: usize,
	key_active: bool,
}

impl Default for Compressor {
	fn default() -> Self {
		Self::new()
	}
}

impl Compressor {
	pub fn new() -> Self {
		let params = ParamValues::new(&INFO);
		let mut c = Self {
			sample_rate: 48_000.0,
			attack_coeff: 1.0,
			release_coeff: 1.0,
			env_db: 0.0,
			gr_meter: 0.0,
			threshold: Smoother::new(params.get(P_THRESHOLD)),
			makeup: Smoother::new(db_to_gain(params.get(P_MAKEUP))),
			mix: Smoother::new(params.get(P_MIX) * 0.01),
			params,
			key_l: Vec::new(),
			key_r: Vec::new(),
			key_len: 0,
			key_active: false,
		};
		c.update_times();
		c
	}

	fn update_times(&mut self) {
		self.attack_coeff = one_pole_coeff(self.params.get(P_ATTACK), self.sample_rate);
		self.release_coeff = one_pole_coeff(self.params.get(P_RELEASE), self.sample_rate);
	}

	/// Largest gain reduction applied during the last `process` call, as a
	/// positive number of dB (0 = no compression). For metering and AI feedback.
	pub fn gain_reduction_db(&self) -> f32 {
		self.gr_meter
	}
}

impl Processor for Compressor {
	fn info(&self) -> &'static DeviceInfo {
		&INFO
	}

	fn prepare(&mut self, sample_rate: f32, max_block: usize) {
		self.sample_rate = sample_rate.max(1.0);
		self.key_l = vec![0.0; max_block.max(1)];
		self.key_r = vec![0.0; max_block.max(1)];
		self.threshold.prepare(self.sample_rate, 20.0);
		self.makeup.prepare(self.sample_rate, 20.0);
		self.mix.prepare(self.sample_rate, 20.0);
		self.update_times();
		self.reset();
	}

	fn set_param(&mut self, index: usize, value: f32) {
		let Some(v) = self.params.set(&INFO, index, value) else {
			return;
		};
		match index {
			P_THRESHOLD => self.threshold.set_target(v),
			P_MAKEUP => self.makeup.set_target(db_to_gain(v)),
			P_MIX => self.mix.set_target(v * 0.01),
			P_ATTACK | P_RELEASE => self.update_times(),
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
		let ratio = self.params.get(P_RATIO);
		let knee = self.params.get(P_KNEE);
		let mut max_gr: f32 = 0.0;
		let keyed = std::mem::take(&mut self.key_active);
		for (i, (l, r)) in left.iter_mut().zip(right.iter_mut()).enumerate() {
			let threshold = self.threshold.tick();
			let peak = if !keyed {
				l.abs().max(r.abs())
			} else if i < self.key_len {
				self.key_l[i].abs().max(self.key_r[i].abs())
			} else {
				0.0
			};
			let target = gain_computer(gain_to_db(peak), threshold, ratio, knee);
			let coeff = if target < self.env_db {
				self.attack_coeff
			} else {
				self.release_coeff
			};
			self.env_db = flush(self.env_db + (target - self.env_db) * coeff);
			max_gr = max_gr.max(-self.env_db);
			let g = db_to_gain(self.env_db) * self.makeup.tick();
			let mix = self.mix.tick();
			let k = 1.0 - mix + g * mix;
			*l *= k;
			*r *= k;
		}
		self.gr_meter = max_gr;
	}

	fn has_sidechain(&self) -> bool {
		true
	}

	fn set_sidechain(&mut self, left: &[f32], right: &[f32]) {
		let n = left.len().min(right.len()).min(self.key_l.len());
		self.key_l[..n].copy_from_slice(&left[..n]);
		self.key_r[..n].copy_from_slice(&right[..n]);
		self.key_len = n;
		self.key_active = true;
	}

	fn reset(&mut self) {
		self.key_active = false;
		self.env_db = 0.0;
		self.gr_meter = 0.0;
		self.threshold.snap();
		self.makeup.snap();
		self.mix.snap();
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn gain_computer_shape() {
		assert_eq!(gain_computer(-30.0, -20.0, 4.0, 0.0), 0.0);
		assert!((gain_computer(-10.0, -20.0, 4.0, 0.0) + 7.5).abs() < 1e-5);
		// Knee is continuous at its edges.
		let edge = gain_computer(-17.0, -20.0, 4.0, 6.0);
		assert!((edge + 2.25).abs() < 1e-4);
		assert_eq!(gain_computer(-23.0, -20.0, 4.0, 6.0), 0.0);
	}

	#[test]
	fn reduces_level_above_threshold() {
		let mut c = Compressor::new();
		c.prepare(48_000.0, 256);
		for (id, v) in [
			("threshold", -20.0),
			("ratio", 4.0),
			("knee", 0.0),
			("attack", 1.0),
		] {
			c.set_param(INFO.param_index(id).unwrap(), v);
		}
		c.reset();
		let sr = 48_000.0;
		// 0 dBFS sine: 20 dB over → expect ~15 dB reduction at steady state.
		let mut l: Vec<f32> = (0..48_000)
			.map(|n| (std::f32::consts::TAU * 200.0 * n as f32 / sr).sin())
			.collect();
		let mut r = l.clone();
		for (a, b) in l.chunks_mut(128).zip(r.chunks_mut(128)) {
			c.process(a, b, &[], &Transport::default());
		}
		let peak = l[40_000..].iter().fold(0.0_f32, |m, x| m.max(x.abs()));
		let out_db = gain_to_db(peak);
		assert!(out_db < -10.0 && out_db > -18.0, "out {out_db} dB");
		assert!(c.gain_reduction_db() > 10.0);
	}

	#[test]
	fn sidechain_keys_gain_reduction() {
		let mut c = Compressor::new();
		c.prepare(48_000.0, 64);
		for (id, v) in [("threshold", -30.0), ("ratio", 10.0), ("attack", 0.1)] {
			c.set_param(INFO.param_index(id).unwrap(), v);
		}
		c.reset();
		assert!(c.has_sidechain());
		let t = Transport::default();
		// Quiet main signal, loud key: ducked.
		let key = vec![0.9_f32; 64];
		let mut out = 1.0;
		for _ in 0..200 {
			let mut l = vec![0.01_f32; 64];
			let mut r = l.clone();
			c.set_sidechain(&key, &key);
			c.process(&mut l, &mut r, &[], &t);
			out = l[63];
		}
		assert!(out < 0.01 * 0.2, "keyed output {out}");
		// The key only applies to the block after `set_sidechain`: without it
		// the quiet input is left alone again once the envelope recovers.
		for _ in 0..2_000 {
			let mut l = vec![0.01_f32; 64];
			let mut r = l.clone();
			c.process(&mut l, &mut r, &[], &t);
			out = l[63];
		}
		assert!((out - 0.01).abs() < 1e-4, "unkeyed output {out}");
	}

	#[test]
	fn quiet_signal_untouched() {
		let mut c = Compressor::new();
		c.prepare(48_000.0, 64);
		let mut l = vec![0.01_f32; 64];
		let mut r = l.clone();
		for _ in 0..100 {
			l.fill(0.01);
			r.fill(0.01);
			c.process(&mut l, &mut r, &[], &Transport::default());
		}
		assert!((l[63] - 0.01).abs() < 1e-6);
	}
}
