//! `compressor`: feed-forward, stereo-linked peak compressor with soft knee,
//! parallel (dry/wet) mix and an optional external sidechain (key) input.

use crate::processor::{
	DeviceInfo, DeviceRole, Listen, MidiEvent, ParamInfo, Processor, Transport,
};
use crate::util::biquad::{Biquad, BiquadCoeffs};
use crate::util::smoother::Smoother;
use crate::util::{ParamValues, db_to_gain, flush, gain_to_db, one_pole_coeff};

const P_THRESHOLD: usize = 0;
const P_RATIO: usize = 1;
const P_ATTACK: usize = 2;
const P_RELEASE: usize = 3;
const P_KNEE: usize = 4;
const P_MAKEUP: usize = 5;
const P_MIX: usize = 6;
const P_KEY_HP: usize = 7;
const P_KEY_LP: usize = 8;
const N_PARAMS: usize = 9;

/// `key_hp` at (or below) this is off.
pub const KEY_HP_OFF_HZ: f32 = 20.0;
/// `key_lp` at (or above) this is off.
pub const KEY_LP_OFF_HZ: f32 = 20_000.0;
/// Q of the 12 dB/oct key filters (Butterworth).
const KEY_FILTER_Q: f32 = std::f32::consts::FRAC_1_SQRT_2;

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
	ParamInfo::new(
		"key_hp",
		"Key High-Pass",
		KEY_HP_OFF_HZ,
		10_000.0,
		KEY_HP_OFF_HZ,
		"Hz",
		"12 dB/oct high-pass on the detector (key) signal only, never on the audio. 20 = off. \
		 80-150 Hz stops kick/bass from pumping a mix bus or a full track; 3-6 kHz with a high \
		 ratio and fast attack makes a de-esser.",
	),
	ParamInfo::new(
		"key_lp",
		"Key Low-Pass",
		100.0,
		KEY_LP_OFF_HZ,
		KEY_LP_OFF_HZ,
		"Hz",
		"12 dB/oct low-pass on the detector (key) signal only. 20000 = off. 100-200 Hz makes \
		 a sidechain duck react to the kick alone (not the hats) in a full drum key.",
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
		reacts to its own input. KEY FILTER: key_hp/key_lp filter only what the detector hears \
		(own input or sidechain): key_hp 100-150 keeps the low end from pumping a bus.",
	params: &PARAMS,
};

/// Static gain computer: gain change in dB (<= 0) for an input level in dB.
pub(crate) fn gain_computer(level_db: f32, threshold: f32, ratio: f32, knee: f32) -> f32 {
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

/// Static compressor transfer curve: output level in dB for a steady input
/// level in dB (after attack/release have settled), including makeup gain.
/// Uses [`gain_computer`], the processor's own gain-reduction formula.
pub fn compressor_curve_db(
	threshold: f32,
	ratio: f32,
	knee: f32,
	makeup: f32,
	input_db: f32,
) -> f32 {
	input_db + gain_computer(input_db, threshold, ratio, knee) + makeup
}

/// The detector's key filters for `key_hp` / `key_lp` (plain Hz): `None` when that
/// filter is off (at its range end). Shared by the processor and
/// [`compressor_key_response_db`], so the editor's key-filter strip is the real filter.
pub fn key_filter_coeffs(key_hp: f32, key_lp: f32, sample_rate: f32) -> [Option<BiquadCoeffs>; 2] {
	let sr = sample_rate.max(1.0);
	let nyquist = sr * 0.49;
	[
		(key_hp > KEY_HP_OFF_HZ + 0.5)
			.then(|| BiquadCoeffs::highpass(key_hp.min(nyquist), KEY_FILTER_Q, sr)),
		(key_lp < KEY_LP_OFF_HZ - 0.5 && key_lp < nyquist)
			.then(|| BiquadCoeffs::lowpass(key_lp, KEY_FILTER_Q, sr)),
	]
}

/// Magnitude response (dB) of the compressor's key filters at `freq_hz` (0 dB when
/// both are off).
pub fn compressor_key_response_db(key_hp: f32, key_lp: f32, sample_rate: f32, freq_hz: f32) -> f32 {
	key_filter_coeffs(key_hp, key_lp, sample_rate)
		.iter()
		.flatten()
		.map(|c| c.magnitude_db(freq_hz, sample_rate.max(1.0)))
		.sum()
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
	/// Detector key level (linear peak with a fall, see `readings::KEY_LEVEL_DB`).
	key_env: f32,
	key_fall: f32,
	/// Output the key instead of the compressed signal (monitoring only).
	listen_key: bool,
	/// Key filters (high-pass, low-pass) and their state per channel.
	key_filters: [Option<BiquadCoeffs>; 2],
	key_state: [[Biquad; 2]; 2],
}

/// Fall of the published key level: 20 dB in 300 ms.
const KEY_FALL_DB_PER_S: f32 = 20.0 / 0.3;

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
			key_env: 0.0,
			key_fall: 1.0,
			listen_key: false,
			key_filters: [None, None],
			key_state: [[Biquad::default(); 2]; 2],
		};
		c.update_times();
		c
	}

	fn update_times(&mut self) {
		self.attack_coeff = one_pole_coeff(self.params.get(P_ATTACK), self.sample_rate);
		self.release_coeff = one_pole_coeff(self.params.get(P_RELEASE), self.sample_rate);
		self.key_fall = db_to_gain(-KEY_FALL_DB_PER_S / self.sample_rate);
		let filters = key_filter_coeffs(
			self.params.get(P_KEY_HP),
			self.params.get(P_KEY_LP),
			self.sample_rate,
		);
		for (k, f) in filters.iter().enumerate() {
			if f.is_none() {
				self.key_state[0][k].reset();
				self.key_state[1][k].reset();
			}
		}
		self.key_filters = filters;
	}

	/// Level of the detector's key signal, dBFS (see `readings::KEY_LEVEL_DB`).
	pub fn key_level_db(&self) -> f32 {
		gain_to_db(self.key_env).max(-144.0)
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
			P_ATTACK | P_RELEASE | P_KEY_HP | P_KEY_LP => self.update_times(),
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
			let (mut kl, mut kr) = if !keyed {
				(*l, *r)
			} else if i < self.key_len {
				(self.key_l[i], self.key_r[i])
			} else {
				(0.0, 0.0)
			};
			for (k, c) in self.key_filters.iter().enumerate() {
				if let Some(c) = c {
					kl = self.key_state[0][k].process(c, kl);
					kr = self.key_state[1][k].process(c, kr);
				}
			}
			let peak = kl.abs().max(kr.abs());
			self.key_env = flush(peak.max(self.key_env * self.key_fall));
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
			if self.listen_key {
				*l = kl;
				*r = kr;
			} else {
				*l *= k;
				*r *= k;
			}
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

	fn readings(&self, out: &mut [f32]) -> usize {
		let values = [self.gr_meter, self.key_level_db()];
		let n = values.len().min(out.len());
		out[..n].copy_from_slice(&values[..n]);
		n
	}

	fn set_listen(&mut self, mode: Listen) -> bool {
		match mode {
			Listen::Off | Listen::Key => {
				self.listen_key = mode == Listen::Key;
				true
			}
			Listen::Band(_) => false,
		}
	}

	fn reset(&mut self) {
		self.key_state = [[Biquad::default(); 2]; 2];
		self.key_active = false;
		self.key_env = 0.0;
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
		let mut readings = [0.0; crate::readings::MAX];
		assert_eq!(c.readings(&mut readings), 2);
		assert!(
			readings[crate::readings::KEY_LEVEL_DB].abs() < 0.1,
			"0 dBFS key"
		);
		assert_eq!(
			readings[crate::readings::GAIN_REDUCTION_DB],
			c.gain_reduction_db()
		);
	}

	#[test]
	fn compressor_curve_matches_settled_sine() {
		let (threshold, ratio, knee, makeup) = (-18.0, 4.0, 6.0, 3.0);
		for input_amp in [0.025, 0.1, 0.25, 0.5, 1.0] {
			let mut c = Compressor::new();
			c.prepare(48_000.0, 256);
			for (id, v) in [
				("threshold", threshold),
				("ratio", ratio),
				("knee", knee),
				("makeup", makeup),
				("attack", 1.0),
				("release", 50.0),
			] {
				c.set_param(INFO.param_index(id).unwrap(), v);
			}
			c.reset();
			let sr = 48_000.0;
			let mut l: Vec<f32> = (0..48_000)
				.map(|n| (std::f32::consts::TAU * 200.0 * n as f32 / sr).sin() * input_amp)
				.collect();
			let mut r = l.clone();
			for (a, b) in l.chunks_mut(128).zip(r.chunks_mut(128)) {
				c.process(a, b, &[], &Transport::default());
			}
			let peak = l[40_000..].iter().fold(0.0_f32, |m, x| m.max(x.abs()));
			let measured = gain_to_db(peak);
			let input_db = gain_to_db(input_amp);
			let formula = compressor_curve_db(threshold, ratio, knee, makeup, input_db);
			assert!(
				(formula - measured).abs() < 0.5,
				"input {input_db} dB: formula {formula}, measured {measured}"
			);
		}
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
	fn key_level_and_listen_key_follow_the_sidechain() {
		let mut c = Compressor::new();
		c.prepare(48_000.0, 64);
		let t = Transport::default();
		let key: Vec<f32> = (0..64)
			.map(|i| if i % 2 == 0 { 0.5 } else { -0.5 })
			.collect();
		let mut l = vec![0.01_f32; 64];
		let mut r = l.clone();
		c.set_sidechain(&key, &key);
		c.process(&mut l, &mut r, &[], &t);
		assert!((c.key_level_db() - gain_to_db(0.5)).abs() < 0.1);
		assert!(
			l[0] > 0.0 && l[0] <= 0.01,
			"not listening: the main signal plays"
		);
		assert!(c.set_listen(Listen::Key));
		c.set_sidechain(&key, &key);
		c.process(&mut l, &mut r, &[], &t);
		assert_eq!(l, key, "listening: the key plays");
		assert_eq!(r, key);
		// Without a sidechain the detector hears the input, so listening passes it dry.
		let mut l = vec![0.25_f32; 64];
		let mut r = l.clone();
		c.process(&mut l, &mut r, &[], &t);
		assert!(l.iter().all(|&x| x == 0.25));
		// The level falls ~20 dB in 300 ms of silence.
		assert!(c.set_listen(Listen::Off));
		let before = c.key_level_db();
		for _ in 0..225 {
			let mut l = vec![0.0_f32; 64];
			let mut r = l.clone();
			c.process(&mut l, &mut r, &[], &t);
		}
		let fell = before - c.key_level_db();
		assert!((fell - 20.0).abs() < 0.5, "fell {fell} dB");
		assert!(!c.set_listen(Listen::Band(2)));
	}

	/// Gain reduction (dB) at the end of a second of a 60 Hz + 3 kHz mix.
	fn bass_heavy_gr(key_hp: f32, key_lp: f32) -> f32 {
		let mut c = Compressor::new();
		c.prepare(48_000.0, 128);
		for (id, v) in [
			("threshold", -20.0),
			("ratio", 8.0),
			("knee", 0.0),
			("key_hp", key_hp),
			("key_lp", key_lp),
		] {
			c.set_param(INFO.param_index(id).unwrap(), v);
		}
		c.reset();
		let sr = 48_000.0;
		let tau = std::f32::consts::TAU;
		let mut l: Vec<f32> = (0..48_000)
			.map(|n| {
				let t = n as f32 / sr;
				0.8 * (tau * 60.0 * t).sin() + 0.05 * (tau * 3_000.0 * t).sin()
			})
			.collect();
		let mut r = l.clone();
		for (a, b) in l.chunks_mut(128).zip(r.chunks_mut(128)) {
			c.process(a, b, &[], &Transport::default());
		}
		c.gain_reduction_db()
	}

	#[test]
	fn key_filter_changes_gain_reduction_on_a_bass_heavy_signal() {
		let open = bass_heavy_gr(KEY_HP_OFF_HZ, KEY_LP_OFF_HZ);
		assert!(open > 10.0, "the bass drives the detector: {open}");
		let hp = bass_heavy_gr(1_000.0, KEY_LP_OFF_HZ);
		assert!(
			hp < open - 10.0,
			"high-passed key ignores the bass: {hp} vs {open}"
		);
		// A low-pass keeps the bass in the key.
		let lp = bass_heavy_gr(KEY_HP_OFF_HZ, 200.0);
		assert!((lp - open).abs() < 2.0, "{lp} vs {open}");
		// The picture is the filter.
		assert_eq!(
			compressor_key_response_db(KEY_HP_OFF_HZ, KEY_LP_OFF_HZ, 48_000.0, 50.0),
			0.0
		);
		let at = compressor_key_response_db(1_000.0, KEY_LP_OFF_HZ, 48_000.0, 1_000.0);
		assert!((at + 3.0).abs() < 0.2, "-3 dB at the corner: {at}");
		assert!(compressor_key_response_db(1_000.0, KEY_LP_OFF_HZ, 48_000.0, 60.0) < -40.0);
	}

	#[test]
	fn listen_key_hears_the_filtered_key() {
		let mut c = Compressor::new();
		c.prepare(48_000.0, 256);
		c.set_param(P_KEY_HP, 2_000.0);
		assert!(c.set_listen(Listen::Key));
		let sr = 48_000.0;
		let mut l: Vec<f32> = (0..24_000)
			.map(|n| 0.5 * (std::f32::consts::TAU * 80.0 * n as f32 / sr).sin())
			.collect();
		let mut r = l.clone();
		for (a, b) in l.chunks_mut(256).zip(r.chunks_mut(256)) {
			c.process(a, b, &[], &Transport::default());
		}
		let peak = l[12_000..].iter().fold(0.0_f32, |m, x| m.max(x.abs()));
		assert!(peak < 0.5 * 0.01, "80 Hz removed from the key: {peak}");
		assert!(c.key_level_db() < -40.0);
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
