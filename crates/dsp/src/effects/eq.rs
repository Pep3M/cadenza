//! `eq`: 7-band parametric equalizer built from RBJ biquads.

use crate::processor::{DeviceInfo, DeviceRole, MidiEvent, ParamInfo, Processor, Transport};
use crate::util::biquad::{BUTTERWORTH_Q, Biquad, BiquadCoeffs};
use crate::util::smoother::Smoother;
use crate::util::{ParamValues, db_to_gain};

const P_HP_FREQ: usize = 0;
const P_HP_SLOPE: usize = 1;
const P_LS_FREQ: usize = 2;
const P_LS_GAIN: usize = 3;
const P_P1_FREQ: usize = 4;
const P_P1_GAIN: usize = 5;
const P_P1_Q: usize = 6;
const P_P2_FREQ: usize = 7;
const P_P2_GAIN: usize = 8;
const P_P2_Q: usize = 9;
const P_P3_FREQ: usize = 10;
const P_P3_GAIN: usize = 11;
const P_P3_Q: usize = 12;
const P_HS_FREQ: usize = 13;
const P_HS_GAIN: usize = 14;
const P_LP_FREQ: usize = 15;
const P_LP_SLOPE: usize = 16;
const P_OUTPUT: usize = 17;
const N_PARAMS: usize = 18;

const SLOPE_DESC: &str = "0 = off, 1 = 12 dB/oct, 2 = 24 dB/oct, 3 = 48 dB/oct.";
const GAIN_DESC: &str = "Boost (+) or cut (-) of this band. Cuts of 2-6 dB clean up mud; boosts of 1-4 dB add presence.";
const Q_DESC: &str =
	"Bandwidth: 0.5-1 broad and musical, 2-5 focused, 8+ surgical notch for resonances.";

static PARAMS: [ParamInfo; N_PARAMS] = [
	ParamInfo::new(
		"hp_freq",
		"High-Pass Freq",
		20.0,
		2_000.0,
		30.0,
		"Hz",
		"High-pass cutoff. 30-40 for bass/kick (rumble), 80-150 for vocals/guitars/keys, 200-400 for hats and pads in a busy mix.",
	),
	ParamInfo::new("hp_slope", "High-Pass Slope", 0.0, 3.0, 0.0, "", SLOPE_DESC),
	ParamInfo::new(
		"low_shelf_freq",
		"Low Shelf Freq",
		20.0,
		1_000.0,
		100.0,
		"Hz",
		"Low shelf corner frequency.",
	),
	ParamInfo::new(
		"low_shelf_gain",
		"Low Shelf Gain",
		-24.0,
		24.0,
		0.0,
		"dB",
		GAIN_DESC,
	),
	ParamInfo::new(
		"peak1_freq",
		"Peak 1 Freq",
		20.0,
		20_000.0,
		250.0,
		"Hz",
		"Peak band 1 center. 200-500 Hz is the 'mud/boxiness' region.",
	),
	ParamInfo::new(
		"peak1_gain",
		"Peak 1 Gain",
		-24.0,
		24.0,
		0.0,
		"dB",
		GAIN_DESC,
	),
	ParamInfo::new("peak1_q", "Peak 1 Q", 0.1, 18.0, 1.0, "", Q_DESC),
	ParamInfo::new(
		"peak2_freq",
		"Peak 2 Freq",
		20.0,
		20_000.0,
		1_000.0,
		"Hz",
		"Peak band 2 center. 800-1500 Hz is nasal/honky; 2-4 kHz is presence/attack.",
	),
	ParamInfo::new(
		"peak2_gain",
		"Peak 2 Gain",
		-24.0,
		24.0,
		0.0,
		"dB",
		GAIN_DESC,
	),
	ParamInfo::new("peak2_q", "Peak 2 Q", 0.1, 18.0, 1.0, "", Q_DESC),
	ParamInfo::new(
		"peak3_freq",
		"Peak 3 Freq",
		20.0,
		20_000.0,
		4_000.0,
		"Hz",
		"Peak band 3 center. 3-6 kHz presence/harshness, 6-9 kHz sibilance.",
	),
	ParamInfo::new(
		"peak3_gain",
		"Peak 3 Gain",
		-24.0,
		24.0,
		0.0,
		"dB",
		GAIN_DESC,
	),
	ParamInfo::new("peak3_q", "Peak 3 Q", 0.1, 18.0, 1.0, "", Q_DESC),
	ParamInfo::new(
		"high_shelf_freq",
		"High Shelf Freq",
		1_000.0,
		20_000.0,
		8_000.0,
		"Hz",
		"High shelf corner. 8-12 kHz shelf boosts add 'air'.",
	),
	ParamInfo::new(
		"high_shelf_gain",
		"High Shelf Gain",
		-24.0,
		24.0,
		0.0,
		"dB",
		GAIN_DESC,
	),
	ParamInfo::new(
		"lp_freq",
		"Low-Pass Freq",
		1_000.0,
		20_000.0,
		18_000.0,
		"Hz",
		"Low-pass cutoff to tame fizz or create lo-fi/telephone effects.",
	),
	ParamInfo::new("lp_slope", "Low-Pass Slope", 0.0, 3.0, 0.0, "", SLOPE_DESC),
	ParamInfo::new(
		"output_gain",
		"Output Gain",
		-24.0,
		24.0,
		0.0,
		"dB",
		"Level after the EQ.",
	),
];

pub static INFO: DeviceInfo = DeviceInfo {
	id: "eq",
	name: "Parametric EQ",
	role: DeviceRole::Effect,
	description: "Stereo 7-band parametric EQ: switchable high-pass (12/24/48 dB/oct), low shelf, \
		three peaking bands with Q, high shelf and switchable low-pass. The first insert on almost \
		every track: high-pass everything except kick/bass (hp_slope 2, hp_freq 80-150), cut mud \
		around 250-400 Hz, add presence at 2-5 kHz and air with a high shelf at 10 kHz. Prefer \
		cutting over boosting; carve complementary space between competing instruments.",
	params: &PARAMS,
};

/// Continuous parameters smoothed in a perceptual domain (log2 for Hz/Q, dB for gains).
const SMOOTHED: [usize; 16] = [
	P_HP_FREQ, P_LS_FREQ, P_LS_GAIN, P_P1_FREQ, P_P1_GAIN, P_P1_Q, P_P2_FREQ, P_P2_GAIN, P_P2_Q,
	P_P3_FREQ, P_P3_GAIN, P_P3_Q, P_HS_FREQ, P_HS_GAIN, P_LP_FREQ, P_OUTPUT,
];

const CONTROL_BLOCK: usize = 16;
// Filter slots: 0..4 high-pass, 4 low shelf, 5..8 peaks, 8 high shelf, 9..13 low-pass.
const N_FILTERS: usize = 13;

fn is_log(index: usize) -> bool {
	matches!(
		index,
		P_HP_FREQ
			| P_LS_FREQ
			| P_P1_FREQ
			| P_P2_FREQ
			| P_P3_FREQ
			| P_HS_FREQ
			| P_LP_FREQ
			| P_P1_Q | P_P2_Q
			| P_P3_Q
	)
}

fn to_domain(index: usize, v: f32) -> f32 {
	if is_log(index) { v.log2() } else { v }
}

pub struct Eq {
	params: ParamValues<N_PARAMS>,
	sample_rate: f32,
	smoothers: [Smoother; N_PARAMS],
	coeffs: [BiquadCoeffs; N_FILTERS],
	active: [bool; N_FILTERS],
	state: [[Biquad; N_FILTERS]; 2],
	output: f32,
	dirty: bool,
}

impl Default for Eq {
	fn default() -> Self {
		Self::new()
	}
}

impl Eq {
	pub fn new() -> Self {
		let params = ParamValues::new(&INFO);
		let mut smoothers = [Smoother::new(0.0); N_PARAMS];
		for &i in &SMOOTHED {
			smoothers[i] = Smoother::new(to_domain(i, params.get(i)));
		}
		let mut eq = Self {
			params,
			sample_rate: 48_000.0,
			smoothers,
			coeffs: [BiquadCoeffs::IDENTITY; N_FILTERS],
			active: [false; N_FILTERS],
			state: [[Biquad::default(); N_FILTERS]; 2],
			output: 1.0,
			dirty: true,
		};
		eq.update_coeffs();
		eq
	}

	fn value(&self, index: usize) -> f32 {
		let v = self.smoothers[index].value();
		if is_log(index) { v.exp2() } else { v }
	}

	fn update_coeffs(&mut self) {
		let sr = self.sample_rate;
		let hp_slope = self.params.choice(P_HP_SLOPE).min(3);
		let lp_slope = self.params.choice(P_LP_SLOPE).min(3);
		let hp_f = self.value(P_HP_FREQ);
		let lp_f = self.value(P_LP_FREQ);
		for k in 0..4 {
			self.active[k] = false;
			self.active[9 + k] = false;
		}
		if hp_slope > 0 {
			for (k, &q) in BUTTERWORTH_Q[hp_slope - 1].iter().enumerate() {
				self.coeffs[k] = BiquadCoeffs::highpass(hp_f, q, sr);
				self.active[k] = true;
			}
		}
		if lp_slope > 0 {
			for (k, &q) in BUTTERWORTH_Q[lp_slope - 1].iter().enumerate() {
				self.coeffs[9 + k] = BiquadCoeffs::lowpass(lp_f, q, sr);
				self.active[9 + k] = true;
			}
		}
		self.coeffs[4] = BiquadCoeffs::low_shelf(self.value(P_LS_FREQ), self.value(P_LS_GAIN), sr);
		for (slot, (f, g, q)) in [
			(P_P1_FREQ, P_P1_GAIN, P_P1_Q),
			(P_P2_FREQ, P_P2_GAIN, P_P2_Q),
			(P_P3_FREQ, P_P3_GAIN, P_P3_Q),
		]
		.into_iter()
		.enumerate()
		{
			self.coeffs[5 + slot] =
				BiquadCoeffs::peaking(self.value(f), self.value(q), self.value(g), sr);
		}
		self.coeffs[8] = BiquadCoeffs::high_shelf(self.value(P_HS_FREQ), self.value(P_HS_GAIN), sr);
		for k in 4..9 {
			self.active[k] = true;
		}
		self.output = db_to_gain(self.value(P_OUTPUT));
		// Sections that just got switched off must restart from silence.
		for k in 0..N_FILTERS {
			if !self.active[k] {
				self.state[0][k].reset();
				self.state[1][k].reset();
			}
		}
		self.dirty = false;
	}

	/// Current static magnitude response of the whole EQ at `freq`, in dB.
	pub fn response_db(&self, freq: f32) -> f32 {
		let mut db = crate::util::gain_to_db(self.output);
		for (c, &on) in self.coeffs.iter().zip(&self.active) {
			if on {
				db += c.magnitude_db(freq, self.sample_rate);
			}
		}
		db
	}
}

impl Processor for Eq {
	fn info(&self) -> &'static DeviceInfo {
		&INFO
	}

	fn prepare(&mut self, sample_rate: f32, _max_block: usize) {
		self.sample_rate = sample_rate.max(1.0);
		for &i in &SMOOTHED {
			self.smoothers[i].prepare(self.sample_rate, 20.0);
		}
		self.reset();
	}

	fn set_param(&mut self, index: usize, value: f32) {
		let Some(v) = self.params.set(&INFO, index, value) else {
			return;
		};
		if SMOOTHED.contains(&index) {
			self.smoothers[index].set_target(to_domain(index, v));
		}
		self.dirty = true;
	}

	fn process(
		&mut self,
		left: &mut [f32],
		right: &mut [f32],
		_events: &[MidiEvent],
		_transport: &Transport,
	) {
		let len = left.len().min(right.len());
		let mut pos = 0;
		while pos < len {
			let n = CONTROL_BLOCK.min(len - pos);
			if self.dirty || SMOOTHED.iter().any(|&i| !self.smoothers[i].is_settled()) {
				for &i in &SMOOTHED {
					self.smoothers[i].advance(n);
				}
				self.update_coeffs();
				// Keep recomputing until every smoother has landed.
				self.dirty = SMOOTHED.iter().any(|&i| !self.smoothers[i].is_settled());
			}
			for (ch, buf) in [&mut left[pos..pos + n], &mut right[pos..pos + n]]
				.into_iter()
				.enumerate()
			{
				let states = &mut self.state[ch];
				for x in buf.iter_mut() {
					let mut y = *x;
					for ((state, c), _) in states
						.iter_mut()
						.zip(&self.coeffs)
						.zip(&self.active)
						.filter(|(_, on)| **on)
					{
						y = state.process(c, y);
					}
					*x = y * self.output;
				}
			}
			pos += n;
		}
	}

	fn reset(&mut self) {
		for &i in &SMOOTHED {
			self.smoothers[i].snap();
		}
		self.state = [[Biquad::default(); N_FILTERS]; 2];
		self.update_coeffs();
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn eq_with(params: &[(&str, f32)]) -> Eq {
		let mut eq = Eq::new();
		eq.prepare(48_000.0, 512);
		for &(id, v) in params {
			eq.set_param(INFO.param_index(id).unwrap(), v);
		}
		eq.reset();
		eq
	}

	#[test]
	fn flat_by_default() {
		let eq = eq_with(&[]);
		for f in [30.0, 100.0, 1_000.0, 10_000.0] {
			assert!(eq.response_db(f).abs() < 0.05, "{f}: {}", eq.response_db(f));
		}
	}

	#[test]
	fn bands_hit_their_gain_at_center() {
		let eq = eq_with(&[
			("peak1_freq", 300.0),
			("peak1_gain", -6.0),
			("peak1_q", 4.0),
			("peak2_freq", 2_000.0),
			("peak2_gain", 5.0),
			("peak2_q", 4.0),
			("peak3_freq", 9_000.0),
			("peak3_gain", 8.0),
			("peak3_q", 6.0),
		]);
		assert!((eq.response_db(300.0) + 6.0).abs() < 0.3);
		assert!((eq.response_db(2_000.0) - 5.0).abs() < 0.3);
		assert!((eq.response_db(9_000.0) - 8.0).abs() < 0.3);
	}

	#[test]
	fn filters_slope() {
		let eq = eq_with(&[
			("hp_freq", 100.0),
			("hp_slope", 2.0),
			("lp_freq", 5_000.0),
			("lp_slope", 3.0),
		]);
		assert!((eq.response_db(100.0) + 3.0).abs() < 0.2);
		// 24 dB/oct: two octaves below ≈ -48 dB.
		assert!((eq.response_db(25.0) + 48.0).abs() < 2.0);
		assert!((eq.response_db(5_000.0) + 3.0).abs() < 0.3);
		assert!(eq.response_db(10_000.0) < -40.0);
	}

	#[test]
	fn processed_sine_matches_response() {
		let mut eq = eq_with(&[
			("peak2_freq", 1_000.0),
			("peak2_gain", 9.0),
			("peak2_q", 2.0),
		]);
		let sr = 48_000.0;
		let mut l: Vec<f32> = (0..48_000)
			.map(|n| (std::f32::consts::TAU * 1_000.0 * n as f32 / sr).sin() * 0.1)
			.collect();
		let mut r = l.clone();
		for (cl, cr) in l.chunks_mut(256).zip(r.chunks_mut(256)) {
			eq.process(cl, cr, &[], &Transport::default());
		}
		let peak = l[24_000..].iter().fold(0.0_f32, |m, x| m.max(x.abs()));
		assert!((crate::util::gain_to_db(peak / 0.1) - 9.0).abs() < 0.2);
	}
}
