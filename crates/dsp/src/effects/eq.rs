//! `eq`: 7-band parametric equalizer built from RBJ biquads, with optional
//! dynamics on the shelves and peaks (dynamic EQ).
//!
//! Dynamic law: a band with `<band>_dyn` on sits at 0 dB while the detector (the
//! band's own isolated region of the input, what Listen plays; or the sidechain when
//! keyed) stays below `<band>_threshold`. Above it the band moves from 0 dB toward its
//! gain by `(level - threshold) * (1 - 1 / ratio)` dB (6 dB soft knee, the compressor's
//! gain computer), smoothed by `dyn_attack` / `dyn_release`, and never past the gain:
//! the gain is the depth (a cut = de-essing / resonance taming, a boost = upward
//! enhancement when the region gets loud).
//!
//! Mid/side: every band has a channel mode `<band>_ms` (0 stereo, 1 mid, 2 side). While
//! any band is not stereo the processor works on M = (L+R)/2 and S = (L-R)/2 (a stereo
//! band filters both, a mid band only M, a side band only S) and decodes L = M+S,
//! R = M-S; with every band stereo the path is the plain L/R one (bit-identical).

use crate::effects::compressor::gain_computer;
use crate::processor::{
	DeviceInfo, DeviceRole, Listen, MidiEvent, ParamInfo, Processor, Transport,
};
use crate::util::biquad::{BUTTERWORTH_Q, Biquad, BiquadCoeffs};
use crate::util::smoother::Smoother;
use crate::util::{ParamValues, db_to_gain, flush, gain_to_db, one_pole_coeff};

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
/// First dynamic param: per dynamic band (in [`DYN_BANDS`] order) `_dyn`,
/// `_threshold`, `_ratio`; then `dyn_attack`, `dyn_release`.
const P_DYN_START: usize = 18;
const P_DYN_ATTACK: usize = P_DYN_START + 3 * N_DYN;
const P_DYN_RELEASE: usize = P_DYN_ATTACK + 1;
/// First channel mode param (`<band>_ms`, one per band in [`N_BANDS`] order).
const P_MS_START: usize = P_DYN_RELEASE + 1;
const N_PARAMS: usize = P_MS_START + N_BANDS;

/// Which channel a band filters (`<band>_ms`: 0 stereo, 1 mid, 2 side).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EqChannel {
	#[default]
	Stereo,
	Mid,
	Side,
}

impl EqChannel {
	/// The mode of a `<band>_ms` param value.
	pub fn from_value(v: f32) -> Self {
		match v.round() as i32 {
			1 => Self::Mid,
			2 => Self::Side,
			_ => Self::Stereo,
		}
	}

	/// Whether a band in this mode filters channel `ch` of the M/S pair (0 mid, 1 side).
	fn filters(self, ch: usize) -> bool {
		match self {
			Self::Stereo => true,
			Self::Mid => ch == 0,
			Self::Side => ch == 1,
		}
	}
}

/// Param index of `band`'s channel mode (`<band>_ms`).
pub fn channel_param_index(band: usize) -> Option<usize> {
	(band < N_BANDS).then_some(P_MS_START + band)
}

/// The band (in [`N_BANDS`] order) that filter slot `k` belongs to.
const SLOT_BAND: [usize; N_FILTERS] = [0, 0, 0, 0, 1, 2, 3, 4, 5, 6, 6, 6, 6];

/// Bands (in [`N_BANDS`] order) that can be dynamic: low shelf, peaks 1-3, high shelf.
pub const DYN_BANDS: [usize; N_DYN] = [1, 2, 3, 4, 5];
const N_DYN: usize = 5;
/// Soft knee of the dynamic bands' gain computer.
const DYN_KNEE_DB: f32 = 6.0;
/// Gain param of each dynamic band.
const DYN_GAIN: [usize; N_DYN] = [P_LS_GAIN, P_P1_GAIN, P_P2_GAIN, P_P3_GAIN, P_HS_GAIN];

/// Param indices (`_dyn`, `_threshold`, `_ratio`) of `band` (in [`N_BANDS`] order) when
/// it can be dynamic.
pub fn dynamic_param_indices(band: usize) -> Option<[usize; 3]> {
	let slot = DYN_BANDS.iter().position(|&b| b == band)?;
	let first = P_DYN_START + 3 * slot;
	Some([first, first + 1, first + 2])
}

/// The gain a dynamic band plays (dB) for its static `gain_db` when the detector asks
/// for `amount_db` (>= 0) of movement: from 0 dB toward the gain, never past it.
pub fn dynamic_band_gain(gain_db: f32, amount_db: f32) -> f32 {
	gain_db.signum() * amount_db.max(0.0).min(gain_db.abs())
}

/// Steady-state gain (dB) of a dynamic band for a detector level (dBFS): the law in the
/// module docs without attack/release.
pub fn dynamic_band_gain_at(gain_db: f32, threshold_db: f32, ratio: f32, level_db: f32) -> f32 {
	dynamic_band_gain(
		gain_db,
		-gain_computer(level_db, threshold_db, ratio.max(1.0), DYN_KNEE_DB),
	)
}

const SLOPE_DESC: &str = "0 = off, 1 = 12 dB/oct, 2 = 24 dB/oct, 3 = 48 dB/oct.";
const GAIN_DESC: &str = "Boost (+) or cut (-) of this band. Cuts of 2-6 dB clean up mud; boosts of 1-4 dB add presence.";
const DYN_DESC: &str = "1 = dynamic band: it sits at 0 dB until its frequency region \
	(or the sidechain) passes the band's threshold, then moves toward its gain (the gain is \
	the depth: -6 = cut up to 6 dB when loud). 0 = static band (default).";
const DYN_THRESHOLD_DESC: &str = "Dynamic band only: level (dBFS) of the band's own \
	frequency region above which the band starts moving toward its gain. Set it a few dB \
	under the loud moments (sibilance, boomy notes) you want to tame.";
const DYN_RATIO_DESC: &str = "Dynamic band only: how quickly the band reaches its gain \
	above the threshold (each dB over moves it 1 - 1/ratio dB). 2-3 gentle, 8+ fast.";
const MS_DESC: &str = "Which channel the band shapes: 0 = stereo (both, default), 1 = mid \
	(the center: vocal, kick, bass), 2 = side (the stereo edges: reverb, wide pads). E.g. \
	high-pass the side at 120-200 Hz for a mono low end, a high shelf +2 dB on the side for \
	width, a peak cut on the mid to make room for the vocal.";
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
	ParamInfo::new(
		"low_shelf_dyn",
		"Low Shelf Dynamic",
		0.0,
		1.0,
		0.0,
		"",
		DYN_DESC,
	),
	ParamInfo::new(
		"low_shelf_threshold",
		"Low Shelf Threshold",
		-60.0,
		0.0,
		-24.0,
		"dB",
		DYN_THRESHOLD_DESC,
	),
	ParamInfo::new(
		"low_shelf_ratio",
		"Low Shelf Ratio",
		1.0,
		20.0,
		3.0,
		"ratio",
		DYN_RATIO_DESC,
	),
	ParamInfo::new("peak1_dyn", "Peak 1 Dynamic", 0.0, 1.0, 0.0, "", DYN_DESC),
	ParamInfo::new(
		"peak1_threshold",
		"Peak 1 Threshold",
		-60.0,
		0.0,
		-24.0,
		"dB",
		DYN_THRESHOLD_DESC,
	),
	ParamInfo::new(
		"peak1_ratio",
		"Peak 1 Ratio",
		1.0,
		20.0,
		3.0,
		"ratio",
		DYN_RATIO_DESC,
	),
	ParamInfo::new("peak2_dyn", "Peak 2 Dynamic", 0.0, 1.0, 0.0, "", DYN_DESC),
	ParamInfo::new(
		"peak2_threshold",
		"Peak 2 Threshold",
		-60.0,
		0.0,
		-24.0,
		"dB",
		DYN_THRESHOLD_DESC,
	),
	ParamInfo::new(
		"peak2_ratio",
		"Peak 2 Ratio",
		1.0,
		20.0,
		3.0,
		"ratio",
		DYN_RATIO_DESC,
	),
	ParamInfo::new("peak3_dyn", "Peak 3 Dynamic", 0.0, 1.0, 0.0, "", DYN_DESC),
	ParamInfo::new(
		"peak3_threshold",
		"Peak 3 Threshold",
		-60.0,
		0.0,
		-24.0,
		"dB",
		DYN_THRESHOLD_DESC,
	),
	ParamInfo::new(
		"peak3_ratio",
		"Peak 3 Ratio",
		1.0,
		20.0,
		3.0,
		"ratio",
		DYN_RATIO_DESC,
	),
	ParamInfo::new(
		"high_shelf_dyn",
		"High Shelf Dynamic",
		0.0,
		1.0,
		0.0,
		"",
		DYN_DESC,
	),
	ParamInfo::new(
		"high_shelf_threshold",
		"High Shelf Threshold",
		-60.0,
		0.0,
		-24.0,
		"dB",
		DYN_THRESHOLD_DESC,
	),
	ParamInfo::new(
		"high_shelf_ratio",
		"High Shelf Ratio",
		1.0,
		20.0,
		3.0,
		"ratio",
		DYN_RATIO_DESC,
	),
	ParamInfo::new(
		"dyn_attack",
		"Dynamic Attack",
		0.5,
		200.0,
		10.0,
		"ms",
		"How fast dynamic bands move toward their gain. 1-5 ms for sibilance and harsh peaks, 10-30 ms keeps transients.",
	),
	ParamInfo::new(
		"dyn_release",
		"Dynamic Release",
		5.0,
		2_000.0,
		150.0,
		"ms",
		"How fast dynamic bands return to 0 dB. 50-150 ms for de-essing, 200-500 ms for smooth tone control.",
	),
	ParamInfo::new("hp_ms", "High-Pass Channel", 0.0, 2.0, 0.0, "", MS_DESC),
	ParamInfo::new(
		"low_shelf_ms",
		"Low Shelf Channel",
		0.0,
		2.0,
		0.0,
		"",
		MS_DESC,
	),
	ParamInfo::new("peak1_ms", "Peak 1 Channel", 0.0, 2.0, 0.0, "", MS_DESC),
	ParamInfo::new("peak2_ms", "Peak 2 Channel", 0.0, 2.0, 0.0, "", MS_DESC),
	ParamInfo::new("peak3_ms", "Peak 3 Channel", 0.0, 2.0, 0.0, "", MS_DESC),
	ParamInfo::new(
		"high_shelf_ms",
		"High Shelf Channel",
		0.0,
		2.0,
		0.0,
		"",
		MS_DESC,
	),
	ParamInfo::new("lp_ms", "Low-Pass Channel", 0.0, 2.0, 0.0, "", MS_DESC),
];

pub static INFO: DeviceInfo = DeviceInfo {
	id: "eq",
	name: "Parametric EQ",
	role: DeviceRole::Effect,
	description: "Stereo 7-band parametric EQ: switchable high-pass (12/24/48 dB/oct), low shelf, \
		three peaking bands with Q, high shelf and switchable low-pass. The first insert on almost \
		every track: high-pass everything except kick/bass (hp_slope 2, hp_freq 80-150), cut mud \
		around 250-400 Hz, add presence at 2-5 kHz and air with a high shelf at 10 kHz. Prefer \
		cutting over boosting; carve complementary space between competing instruments. \
		DYNAMIC EQ: set <band>_dyn 1 on a shelf or peak (low_shelf, peak1-3, high_shelf) and \
		it only applies its gain while that region is loud (threshold/ratio per band, shared \
		dyn_attack/dyn_release): de-ess with peak3 at 6-8 kHz, gain -6, threshold -30, ratio 4, \
		attack 2; tame a boomy bass note with peak1 at its frequency. Keyed by set_sidechain, \
		the detector hears the sidechain instead (duck a pad's mids under the vocal). \
		MID/SIDE: <band>_ms 1 = the band shapes only the mid, 2 = only the side (hp_ms 2 + \
		hp_freq 150 = mono low end; match_eq fits the bands to a reference's tone).",
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

/// Band indices for [`EqBands::band_response_db`] / [`eq_band_response_db`]:
/// 0 high-pass, 1 low shelf, 2-4 peaking bands 1-3, 5 high shelf, 6 low-pass.
pub const N_BANDS: usize = 7;

/// Coefficients for every section, built once from plain parameter values
/// (in [`INFO`]'s order) so a UI can sample many frequency points without
/// recomputing any of them. Built by the same code [`Eq::update_coeffs`]
/// uses, so the picture and the sound cannot drift.
pub struct EqBands {
	coeffs: [BiquadCoeffs; N_FILTERS],
	active: [bool; N_FILTERS],
	modes: [EqChannel; N_BANDS],
	output_db: f32,
	sample_rate: f32,
}

impl EqBands {
	/// `params` holds the plain (unnormalized) values in [`INFO`]'s param
	/// order; a short or missing slice falls back to each parameter's default.
	pub fn new(params: &[f32], sample_rate: f32) -> Self {
		Self::with_offsets(params, sample_rate, &[0.0; N_BANDS])
	}

	/// Like [`EqBands::new`] with `offsets_db` (in [`N_BANDS`] order) added to the gain
	/// of each shelf / peak: the live curve of dynamic bands (static gain + dynamic
	/// offset, e.g. from `readings::EQ_BAND_GAIN_DB_START` minus the static gain).
	/// Offsets of the high-pass / low-pass are ignored.
	pub fn with_offsets(params: &[f32], sample_rate: f32, offsets_db: &[f32; N_BANDS]) -> Self {
		let sr = sample_rate.max(1.0);
		let p = |i: usize| {
			let v = params.get(i).copied().unwrap_or(PARAMS[i].default);
			match DYN_GAIN.iter().position(|&g| g == i) {
				Some(slot) => v + offsets_db[DYN_BANDS[slot]],
				None => v,
			}
		};
		let mut coeffs = [BiquadCoeffs::IDENTITY; N_FILTERS];
		let mut active = [false; N_FILTERS];
		let hp_slope = (p(P_HP_SLOPE).round().max(0.0) as usize).min(3);
		let lp_slope = (p(P_LP_SLOPE).round().max(0.0) as usize).min(3);
		if hp_slope > 0 {
			for (k, &q) in BUTTERWORTH_Q[hp_slope - 1].iter().enumerate() {
				coeffs[k] = BiquadCoeffs::highpass(p(P_HP_FREQ), q, sr);
				active[k] = true;
			}
		}
		if lp_slope > 0 {
			for (k, &q) in BUTTERWORTH_Q[lp_slope - 1].iter().enumerate() {
				coeffs[9 + k] = BiquadCoeffs::lowpass(p(P_LP_FREQ), q, sr);
				active[9 + k] = true;
			}
		}
		coeffs[4] = BiquadCoeffs::low_shelf(p(P_LS_FREQ), p(P_LS_GAIN), sr);
		active[4] = true;
		for (slot, (f, g, q)) in [
			(P_P1_FREQ, P_P1_GAIN, P_P1_Q),
			(P_P2_FREQ, P_P2_GAIN, P_P2_Q),
			(P_P3_FREQ, P_P3_GAIN, P_P3_Q),
		]
		.into_iter()
		.enumerate()
		{
			coeffs[5 + slot] = BiquadCoeffs::peaking(p(f), p(q), p(g), sr);
			active[5 + slot] = true;
		}
		coeffs[8] = BiquadCoeffs::high_shelf(p(P_HS_FREQ), p(P_HS_GAIN), sr);
		active[8] = true;
		let modes = std::array::from_fn(|b| EqChannel::from_value(p(P_MS_START + b)));
		Self {
			coeffs,
			active,
			modes,
			output_db: p(P_OUTPUT),
			sample_rate: sr,
		}
	}

	/// Summed magnitude response of the whole EQ at `freq_hz`, in dB
	/// (includes output gain).
	pub fn response_db(&self, freq_hz: f32) -> f32 {
		let mut db = self.output_db;
		for (c, &on) in self.coeffs.iter().zip(&self.active) {
			if on {
				db += c.magnitude_db(freq_hz, self.sample_rate);
			}
		}
		db
	}

	/// Channel mode of `band` ([`N_BANDS`] order).
	pub fn channel(&self, band: usize) -> EqChannel {
		self.modes.get(band).copied().unwrap_or_default()
	}

	/// Whether any band filters only the mid or only the side.
	pub fn uses_mid_side(&self) -> bool {
		self.modes.iter().any(|&m| m != EqChannel::Stereo)
	}

	/// Summed response (dB, output gain included) the mid (`side` false) or the side
	/// (`side` true) gets: the stereo bands plus that channel's bands.
	pub fn channel_response_db(&self, side: bool, freq_hz: f32) -> f32 {
		let ch = usize::from(side);
		let mut db = self.output_db;
		for (k, (c, &on)) in self.coeffs.iter().zip(&self.active).enumerate() {
			if on && self.modes[SLOT_BAND[k]].filters(ch) {
				db += c.magnitude_db(freq_hz, self.sample_rate);
			}
		}
		db
	}

	/// Magnitude response of one band alone (output gain excluded). See
	/// [`N_BANDS`] for the band order.
	pub fn band_response_db(&self, band: usize, freq_hz: f32) -> f32 {
		let slots: &[usize] = match band {
			0 => &[0, 1, 2, 3],
			1 => &[4],
			2 => &[5],
			3 => &[6],
			4 => &[7],
			5 => &[8],
			6 => &[9, 10, 11, 12],
			_ => &[],
		};
		slots
			.iter()
			.filter(|&&k| self.active[k])
			.map(|&k| self.coeffs[k].magnitude_db(freq_hz, self.sample_rate))
			.sum()
	}
}

/// Summed magnitude response of the EQ at `freq_hz`, in dB (output gain
/// included). `params` are the plain values in [`INFO`]'s param order.
pub fn eq_response_db(params: &[f32], sample_rate: f32, freq_hz: f32) -> f32 {
	EqBands::new(params, sample_rate).response_db(freq_hz)
}

/// Summed magnitude response (dB) with per-band gain offsets (dynamic bands' live
/// movement); see [`EqBands::with_offsets`]. Zero offsets = [`eq_response_db`].
pub fn eq_response_with_offsets_db(
	params: &[f32],
	offsets_db: &[f32; N_BANDS],
	sample_rate: f32,
	freq_hz: f32,
) -> f32 {
	EqBands::with_offsets(params, sample_rate, offsets_db).response_db(freq_hz)
}

/// Magnitude response of one band alone; see [`EqBands::band_response_db`].
pub fn eq_band_response_db(params: &[f32], band: usize, sample_rate: f32, freq_hz: f32) -> f32 {
	EqBands::new(params, sample_rate).band_response_db(band, freq_hz)
}

/// Filters that isolate `band` of the input for listening ([`Listen::Band`]): a
/// constant 0 dB peak band-pass at a peaking band's frequency and Q; for the shelves the
/// region they shape (low shelf: 24 dB/oct low-pass at its corner, high shelf: high-pass);
/// for the cut filters what they remove (high-pass band: low-pass at its cutoff, low-pass
/// band: high-pass), so a solo finds the rumble or hiss being cut. `params` as in
/// [`EqBands::new`]; unused sections are [`BiquadCoeffs::IDENTITY`].
pub fn listen_coeffs(params: &[f32], band: usize, sample_rate: f32) -> [BiquadCoeffs; 2] {
	let sr = sample_rate.max(1.0);
	let p = |i: usize| params.get(i).copied().unwrap_or(PARAMS[i].default);
	let steep = BUTTERWORTH_Q[1];
	let low = |f: f32| [0, 1].map(|k| BiquadCoeffs::lowpass(f, steep[k], sr));
	let high = |f: f32| [0, 1].map(|k| BiquadCoeffs::highpass(f, steep[k], sr));
	let bell = |f: usize, q: usize| {
		[
			BiquadCoeffs::bandpass(p(f), p(q), sr),
			BiquadCoeffs::IDENTITY,
		]
	};
	match band {
		0 => low(p(P_HP_FREQ)),
		1 => low(p(P_LS_FREQ)),
		2 => bell(P_P1_FREQ, P_P1_Q),
		3 => bell(P_P2_FREQ, P_P2_Q),
		4 => bell(P_P3_FREQ, P_P3_Q),
		5 => high(p(P_HS_FREQ)),
		6 => high(p(P_LP_FREQ)),
		_ => [BiquadCoeffs::IDENTITY; 2],
	}
}

pub struct Eq {
	/// Band being listened to (monitoring only) and its isolating filters.
	listen: Option<usize>,
	listen_coeffs: [BiquadCoeffs; 2],
	listen_state: [[Biquad; 2]; 2],
	params: ParamValues<N_PARAMS>,
	sample_rate: f32,
	smoothers: [Smoother; N_PARAMS],
	coeffs: [BiquadCoeffs; N_FILTERS],
	active: [bool; N_FILTERS],
	/// Sections each channel runs: L/R alike when every band is stereo, else M / S.
	chan_active: [[bool; N_FILTERS]; 2],
	/// Some band is mid or side: process in M/S.
	mid_side: bool,
	modes: [EqChannel; N_BANDS],
	state: [[Biquad; N_FILTERS]; 2],
	output: f32,
	dirty: bool,
	/// Dynamic bands: detector filters (the band's listen filters), their state per
	/// channel, and the movement envelope (dB of movement toward the gain, >= 0).
	dyn_detect: [[BiquadCoeffs; 2]; N_DYN],
	dyn_state: [[[Biquad; 2]; N_DYN]; 2],
	dyn_env: [f32; N_DYN],
	dyn_attack: f32,
	dyn_release: f32,
	/// Gain each band plays now (dB, [`N_BANDS`] order; 0 for HP / LP): the readings.
	band_gain: [f32; N_BANDS],
	/// Sidechain for the next block (allocated in `prepare`).
	key_l: Vec<f32>,
	key_r: Vec<f32>,
	key_len: usize,
	key_active: bool,
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
			listen: None,
			listen_coeffs: [BiquadCoeffs::IDENTITY; 2],
			listen_state: [[Biquad::default(); 2]; 2],
			params,
			sample_rate: 48_000.0,
			smoothers,
			coeffs: [BiquadCoeffs::IDENTITY; N_FILTERS],
			active: [false; N_FILTERS],
			chan_active: [[false; N_FILTERS]; 2],
			mid_side: false,
			modes: [EqChannel::Stereo; N_BANDS],
			state: [[Biquad::default(); N_FILTERS]; 2],
			output: 1.0,
			dirty: true,
			dyn_detect: [[BiquadCoeffs::IDENTITY; 2]; N_DYN],
			dyn_state: [[[Biquad::default(); 2]; N_DYN]; 2],
			dyn_env: [0.0; N_DYN],
			dyn_attack: 1.0,
			dyn_release: 1.0,
			band_gain: [0.0; N_BANDS],
			key_l: Vec::new(),
			key_r: Vec::new(),
			key_len: 0,
			key_active: false,
		};
		eq.update_times();
		eq.update_coeffs();
		eq
	}

	fn update_times(&mut self) {
		self.dyn_attack = one_pole_coeff(self.params.get(P_DYN_ATTACK), self.sample_rate);
		self.dyn_release = one_pole_coeff(self.params.get(P_DYN_RELEASE), self.sample_rate);
	}

	fn is_dynamic(&self, slot: usize) -> bool {
		self.params.flag(P_DYN_START + 3 * slot)
	}

	fn any_dynamic(&self) -> bool {
		(0..N_DYN).any(|k| self.is_dynamic(k))
	}

	/// Runs the dynamic bands' detectors over `n` samples from `pos` (the block's input,
	/// or the key when keyed) and updates their envelopes.
	fn detect(&mut self, left: &[f32], right: &[f32], keyed: bool, pos: usize) {
		for (k, &band) in DYN_BANDS.iter().enumerate() {
			if !self.is_dynamic(k) {
				continue;
			}
			let threshold = self.params.get(P_DYN_START + 3 * k + 1);
			let ratio = self.params.get(P_DYN_START + 3 * k + 2);
			let [a, b] = self.dyn_detect[k];
			let mut env = self.dyn_env[k];
			let [mut sl, mut sr] = [self.dyn_state[0][k], self.dyn_state[1][k]];
			for i in 0..left.len() {
				let (xl, xr) = if !keyed {
					match self.modes[band] {
						EqChannel::Stereo => (left[i], right[i]),
						EqChannel::Mid => {
							let m = (left[i] + right[i]) * 0.5;
							(m, m)
						}
						EqChannel::Side => {
							let s = (left[i] - right[i]) * 0.5;
							(s, s)
						}
					}
				} else if pos + i < self.key_len {
					(self.key_l[pos + i], self.key_r[pos + i])
				} else {
					(0.0, 0.0)
				};
				let yl = sl[0].process(&a, xl);
				let yl = sl[1].process(&b, yl);
				let yr = sr[0].process(&a, xr);
				let yr = sr[1].process(&b, yr);
				let level = gain_to_db(yl.abs().max(yr.abs()));
				let target = -gain_computer(level, threshold, ratio, DYN_KNEE_DB);
				let coeff = if target > env {
					self.dyn_attack
				} else {
					self.dyn_release
				};
				env = flush(env + (target - env) * coeff);
			}
			self.dyn_env[k] = env;
			self.dyn_state[0][k] = sl;
			self.dyn_state[1][k] = sr;
		}
	}

	fn value(&self, index: usize) -> f32 {
		let v = self.smoothers[index].value();
		if is_log(index) { v.exp2() } else { v }
	}

	fn update_coeffs(&mut self) {
		// Current plain values (smoothers already settled toward their
		// targets); built through `EqBands` so the picture and the sound use
		// exactly the same coefficient math.
		let mut plain = [0.0_f32; N_PARAMS];
		for (i, slot) in plain.iter_mut().enumerate() {
			*slot = if SMOOTHED.contains(&i) {
				self.value(i)
			} else {
				self.params.get(i)
			};
		}
		if let Some(band) = self.listen {
			self.listen_coeffs = listen_coeffs(&plain, band, self.sample_rate);
		}
		let mut offsets = [0.0_f32; N_BANDS];
		self.band_gain = [0.0; N_BANDS];
		for (k, &band) in DYN_BANDS.iter().enumerate() {
			let gain = plain[DYN_GAIN[k]];
			self.band_gain[band] = gain;
			if self.is_dynamic(k) {
				self.dyn_detect[k] = listen_coeffs(&plain, band, self.sample_rate);
				let live = dynamic_band_gain(gain, self.dyn_env[k]);
				offsets[band] = live - gain;
				self.band_gain[band] = live;
			} else {
				self.dyn_env[k] = 0.0;
			}
		}
		let bands = EqBands::with_offsets(&plain, self.sample_rate, &offsets);
		self.coeffs = bands.coeffs;
		self.active = bands.active;
		self.output = db_to_gain(bands.output_db);
		let mid_side = bands.uses_mid_side();
		if mid_side != self.mid_side {
			// L/R <-> M/S: the filter states hold the other domain's history.
			self.state = [[Biquad::default(); N_FILTERS]; 2];
			self.listen_state = [[Biquad::default(); 2]; 2];
			self.mid_side = mid_side;
		}
		self.modes = bands.modes;
		for ch in 0..2 {
			for (k, &band) in SLOT_BAND.iter().enumerate() {
				let on = self.active[k] && (!mid_side || self.modes[band].filters(ch));
				self.chan_active[ch][k] = on;
				// Sections that just got switched off must restart from silence.
				if !on {
					self.state[ch][k].reset();
				}
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

	fn prepare(&mut self, sample_rate: f32, max_block: usize) {
		self.sample_rate = sample_rate.max(1.0);
		self.key_l = vec![0.0; max_block.max(1)];
		self.key_r = vec![0.0; max_block.max(1)];
		self.update_times();
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
		if index == P_DYN_ATTACK || index == P_DYN_RELEASE {
			self.update_times();
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
		let keyed = std::mem::take(&mut self.key_active);
		let dynamic = self.any_dynamic();
		let mut pos = 0;
		while pos < len {
			let n = CONTROL_BLOCK.min(len - pos);
			if dynamic {
				self.detect(&left[pos..pos + n], &right[pos..pos + n], keyed, pos);
			}
			if self.dirty || dynamic || SMOOTHED.iter().any(|&i| !self.smoothers[i].is_settled()) {
				for &i in &SMOOTHED {
					self.smoothers[i].advance(n);
				}
				self.update_coeffs();
				// Keep recomputing until every smoother has landed.
				self.dirty = SMOOTHED.iter().any(|&i| !self.smoothers[i].is_settled());
			}
			let (l, r) = (&mut left[pos..pos + n], &mut right[pos..pos + n]);
			if self.mid_side {
				for (a, b) in l.iter_mut().zip(r.iter_mut()) {
					let (m, s) = ((*a + *b) * 0.5, (*a - *b) * 0.5);
					*a = m;
					*b = s;
				}
			}
			for (ch, buf) in [&mut *l, &mut *r].into_iter().enumerate() {
				let states = &mut self.state[ch];
				let listen = &mut self.listen_state[ch];
				for x in buf.iter_mut() {
					let input = *x;
					let mut y = input;
					for ((state, c), _) in states
						.iter_mut()
						.zip(&self.coeffs)
						.zip(&self.chan_active[ch])
						.filter(|(_, on)| **on)
					{
						y = state.process(c, y);
					}
					*x = if self.listen.is_some() {
						let [a, b] = &self.listen_coeffs;
						let solo = listen[0].process(a, input);
						listen[1].process(b, solo)
					} else {
						y * self.output
					};
				}
			}
			if self.mid_side {
				// Listening to a mid (side) band hears only that channel's solo.
				let only = self.listen.map(|b| self.modes[b]);
				for (a, b) in l.iter_mut().zip(r.iter_mut()) {
					let (m, s) = match only {
						Some(EqChannel::Mid) => (*a, 0.0),
						Some(EqChannel::Side) => (0.0, *b),
						_ => (*a, *b),
					};
					*a = m + s;
					*b = m - s;
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
		self.listen_state = [[Biquad::default(); 2]; 2];
		self.dyn_state = [[[Biquad::default(); 2]; N_DYN]; 2];
		self.dyn_env = [0.0; N_DYN];
		self.key_active = false;
		self.update_coeffs();
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
		let start = crate::readings::EQ_BAND_GAIN_DB_START;
		let n = N_BANDS.min(out.len().saturating_sub(start));
		out[start..start + n].copy_from_slice(&self.band_gain[..n]);
		start + n
	}

	fn set_listen(&mut self, mode: Listen) -> bool {
		let band = match mode {
			Listen::Off => None,
			Listen::Band(b) if (b as usize) < N_BANDS => Some(b as usize),
			_ => return false,
		};
		if band != self.listen {
			self.listen = band;
			self.listen_state = [[Biquad::default(); 2]; 2];
			self.dirty = true;
		}
		true
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	/// Steady-state gain (dB) of `eq` for a sine at `freq`.
	fn sine_gain_db(eq: &mut Eq, freq: f32) -> f32 {
		let sr = 48_000.0;
		let mut l: Vec<f32> = (0..24_000)
			.map(|n| (std::f32::consts::TAU * freq * n as f32 / sr).sin() * 0.1)
			.collect();
		let mut r = l.clone();
		for (cl, cr) in l.chunks_mut(256).zip(r.chunks_mut(256)) {
			eq.process(cl, cr, &[], &Transport::default());
		}
		let peak = l[12_000..].iter().fold(0.0_f32, |m, x| m.max(x.abs()));
		crate::util::gain_to_db(peak / 0.1)
	}

	#[test]
	fn listen_isolates_the_band() {
		let mut eq = eq_with(&[
			("peak2_freq", 1_000.0),
			("peak2_gain", 9.0),
			("peak2_q", 2.0),
			("hp_freq", 200.0),
			("hp_slope", 2.0),
			("output_gain", -6.0),
		]);
		assert!(eq.set_listen(Listen::Band(3)));
		let center = sine_gain_db(&mut eq, 1_000.0);
		assert!(
			center.abs() < 0.3,
			"bell solo passes its center at 0 dB: {center}"
		);
		assert!(sine_gain_db(&mut eq, 100.0) < -20.0);
		assert!(sine_gain_db(&mut eq, 10_000.0) < -20.0);
		// The high-pass band plays what it cuts.
		assert!(eq.set_listen(Listen::Band(0)));
		assert!(sine_gain_db(&mut eq, 50.0).abs() < 0.5);
		assert!(sine_gain_db(&mut eq, 2_000.0) < -30.0);
		// Off: the EQ again (+9 dB bell, -6 dB output).
		assert!(eq.set_listen(Listen::Off));
		assert!((sine_gain_db(&mut eq, 1_000.0) - 3.0).abs() < 0.3);
		assert!(!eq.set_listen(Listen::Key));
		assert!(!eq.set_listen(Listen::Band(7)));
	}

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

	/// Builds `EqBands` straight from plain param values (bypassing the
	/// smoothers entirely) and compares `eq_response_db` to a real processed
	/// sine's steady-state RMS gain, for a few bands and frequencies.
	#[test]
	fn eq_response_db_matches_processed_sine() {
		let sr = 48_000.0;
		let settings: &[(&str, f32)] = &[
			("hp_freq", 100.0),
			("hp_slope", 2.0),
			("low_shelf_freq", 150.0),
			("low_shelf_gain", 4.0),
			("peak1_freq", 400.0),
			("peak1_gain", -6.0),
			("peak1_q", 2.0),
			("peak3_freq", 6_000.0),
			("peak3_gain", 5.0),
			("peak3_q", 3.0),
			("output_gain", -3.0),
		];
		let mut params = [0.0_f32; N_PARAMS];
		for (i, p) in params.iter_mut().enumerate() {
			*p = PARAMS[i].default;
		}
		for &(id, v) in settings {
			params[INFO.param_index(id).unwrap()] = v;
		}
		for freq in [50.0, 150.0, 400.0, 1_000.0, 6_000.0, 15_000.0] {
			let formula = eq_response_db(&params, sr, freq);
			let amp = 0.1;
			let mut l: Vec<f32> = (0..48_000)
				.map(|n| (std::f32::consts::TAU * freq * n as f32 / sr).sin() * amp)
				.collect();
			let mut r = l.clone();
			let mut eq = eq_with(settings);
			for (cl, cr) in l.chunks_mut(256).zip(r.chunks_mut(256)) {
				eq.process(cl, cr, &[], &Transport::default());
			}
			let settled = &l[24_000..];
			let sum_sq: f64 = settled.iter().map(|&x| (x as f64) * (x as f64)).sum();
			let rms = (sum_sq / settled.len() as f64).sqrt() as f32;
			let input_rms = amp * std::f32::consts::FRAC_1_SQRT_2;
			let measured = crate::util::gain_to_db(rms / input_rms);
			assert!(
				(formula - measured).abs() < 0.3,
				"freq {freq}: formula {formula}, measured {measured}"
			);
		}
	}

	#[test]
	fn eq_band_response_db_matches_band_alone() {
		let sr = 48_000.0;
		let mut params = [0.0_f32; N_PARAMS];
		for (i, p) in params.iter_mut().enumerate() {
			*p = PARAMS[i].default;
		}
		params[P_P2_FREQ] = 2_000.0;
		params[P_P2_GAIN] = 7.0;
		params[P_P2_Q] = 3.0;
		params[P_OUTPUT] = 6.0; // excluded from the per-band response
		assert!((eq_band_response_db(&params, 3, sr, 2_000.0) - 7.0).abs() < 0.1);
		assert!(eq_band_response_db(&params, 3, sr, 50.0).abs() < 0.3);
		// Other bands are flat (defaults): their response is 0 dB everywhere.
		assert!(eq_band_response_db(&params, 1, sr, 150.0).abs() < 0.05);
	}

	/// Old static EQ: the chain of `EqBands` sections run by hand.
	fn static_reference(params: &[f32], input: &[f32]) -> Vec<f32> {
		let bands = EqBands::new(params, 48_000.0);
		let mut state = [Biquad::default(); N_FILTERS];
		let out = db_to_gain(bands.output_db);
		input
			.iter()
			.map(|&x| {
				let mut y = x;
				for ((st, c), _) in state
					.iter_mut()
					.zip(&bands.coeffs)
					.zip(&bands.active)
					.filter(|(_, on)| **on)
				{
					y = st.process(c, y);
				}
				y * out
			})
			.collect()
	}

	#[test]
	fn defaults_are_bit_identical_to_the_static_eq() {
		let settings: &[(&str, f32)] = &[
			("hp_freq", 90.0),
			("hp_slope", 2.0),
			("peak1_gain", -5.0),
			("peak2_freq", 2_500.0),
			("peak2_gain", 4.0),
			("high_shelf_gain", 3.0),
			("output_gain", -2.0),
		];
		let mut params: Vec<f32> = PARAMS.iter().map(|p| p.default).collect();
		for &(id, v) in settings {
			params[INFO.param_index(id).unwrap()] = v;
		}
		// The processor reads log-smoothed params back through exp2(log2(v)).
		for (i, p) in params.iter_mut().enumerate() {
			if is_log(i) {
				*p = p.log2().exp2();
			}
		}
		let mut rng = crate::util::Rng::new(7);
		let input: Vec<f32> = (0..9_000).map(|_| rng.bipolar() * 0.5).collect();
		let expected = static_reference(&params, &input);
		let mut eq = eq_with(settings);
		let mut l = input.clone();
		let mut r = input.clone();
		for (a, b) in l.chunks_mut(300).zip(r.chunks_mut(300)) {
			eq.process(a, b, &[], &Transport::default());
		}
		assert_eq!(
			l, expected,
			"dynamic params at their defaults change nothing"
		);
		let mut readings = [0.0; crate::readings::MAX];
		assert_eq!(eq.readings(&mut readings), crate::readings::EQ_BANDS);
		assert_eq!(readings[2], -5.0, "static gains are published");
		assert_eq!(readings[0], 0.0);
	}

	/// Gain (dB) of Peak 2 at 1 kHz after a second of a 1 kHz sine at `amp`, and the
	/// published band gain.
	fn dynamic_peak(amp: f32) -> (f32, f32) {
		let mut eq = eq_with(&[
			("peak2_freq", 1_000.0),
			("peak2_gain", -9.0),
			("peak2_q", 2.0),
			("peak2_dyn", 1.0),
			("peak2_threshold", -30.0),
			("peak2_ratio", 20.0),
			("dyn_attack", 1.0),
		]);
		let sr = 48_000.0;
		let mut l: Vec<f32> = (0..48_000)
			.map(|n| (std::f32::consts::TAU * 1_000.0 * n as f32 / sr).sin() * amp)
			.collect();
		let mut r = l.clone();
		for (a, b) in l.chunks_mut(256).zip(r.chunks_mut(256)) {
			eq.process(a, b, &[], &Transport::default());
		}
		let peak = l[40_000..].iter().fold(0.0_f32, |m, x| m.max(x.abs()));
		let mut readings = [0.0; crate::readings::MAX];
		eq.readings(&mut readings);
		(crate::util::gain_to_db(peak / amp), readings[3])
	}

	#[test]
	fn dynamic_band_moves_only_above_its_threshold() {
		// -50 dBFS: below the threshold, the band sits at 0 dB.
		let (quiet, quiet_reading) = dynamic_peak(0.003);
		assert!(quiet.abs() < 0.3, "quiet: {quiet}");
		assert!(quiet_reading.abs() < 0.1);
		// -6 dBFS: 24 dB over, the cut reaches its full -9 dB depth, never more.
		let (loud, loud_reading) = dynamic_peak(0.5);
		assert!((loud + 9.0).abs() < 0.5, "loud: {loud}");
		assert!((loud_reading + 9.0).abs() < 0.1);
		// In between the movement follows the law.
		let (mid, mid_reading) = dynamic_peak(db_to_gain(-26.0));
		let law = dynamic_band_gain_at(-9.0, -30.0, 20.0, -26.0);
		assert!(law < -1.0 && law > -9.0, "{law}");
		assert!((mid_reading - law).abs() < 1.0, "{mid_reading} vs {law}");
		assert!((mid - law).abs() < 1.0, "{mid} vs {law}");
		// The live curve with the offset is what plays.
		let mut params: Vec<f32> = PARAMS.iter().map(|p| p.default).collect();
		params[P_P2_FREQ] = 1_000.0;
		params[P_P2_GAIN] = -9.0;
		params[P_P2_Q] = 2.0;
		let mut offsets = [0.0; N_BANDS];
		offsets[3] = 9.0 - 4.0;
		let live = eq_response_with_offsets_db(&params, &offsets, 48_000.0, 1_000.0);
		assert!((live + 4.0).abs() < 0.1, "{live}");
		assert_eq!(
			eq_response_with_offsets_db(&params, &[0.0; N_BANDS], 48_000.0, 700.0),
			eq_response_db(&params, 48_000.0, 700.0)
		);
		assert_eq!(dynamic_param_indices(3), Some([24, 25, 26]));
		assert_eq!(INFO.params[24].id, "peak2_dyn");
		assert_eq!(dynamic_param_indices(0), None);
	}

	#[test]
	fn dynamic_band_follows_the_sidechain() {
		let mut eq = eq_with(&[
			("peak2_freq", 1_000.0),
			("peak2_gain", -9.0),
			("peak2_dyn", 1.0),
			("peak2_threshold", -30.0),
			("peak2_ratio", 20.0),
		]);
		assert!(eq.has_sidechain());
		let sr = 48_000.0;
		let key: Vec<f32> = (0..256)
			.map(|n| (std::f32::consts::TAU * 1_000.0 * n as f32 / sr).sin() * 0.5)
			.collect();
		let mut readings = [0.0; crate::readings::MAX];
		for _ in 0..200 {
			let mut l = vec![0.0_f32; 256];
			let mut r = l.clone();
			eq.set_sidechain(&key, &key);
			eq.process(&mut l, &mut r, &[], &Transport::default());
		}
		eq.readings(&mut readings);
		assert!(readings[3] < -8.5, "keyed: {}", readings[3]);
	}

	/// Steady-state gains (dB) at `freq` of a mid-only and of a side-only input.
	fn mid_side_gains(eq: &mut Eq, freq: f32) -> (f32, f32) {
		let sr = 48_000.0;
		let sine = |n: usize| (std::f32::consts::TAU * freq * n as f32 / sr).sin() * 0.1;
		let mut run = |sign: f32| {
			eq.reset();
			let mut l: Vec<f32> = (0..24_000).map(sine).collect();
			let mut r: Vec<f32> = l.iter().map(|x| x * sign).collect();
			for (a, b) in l.chunks_mut(256).zip(r.chunks_mut(256)) {
				eq.process(a, b, &[], &Transport::default());
			}
			let peak_l = l[12_000..].iter().fold(0.0_f32, |m, x| m.max(x.abs()));
			let peak_r = r[12_000..].iter().fold(0.0_f32, |m, x| m.max(x.abs()));
			assert!((peak_l - peak_r).abs() < 1e-3, "{peak_l} vs {peak_r}");
			crate::util::gain_to_db(peak_l / 0.1)
		};
		(run(1.0), run(-1.0))
	}

	#[test]
	fn mid_side_bands_shape_only_their_channel() {
		let mut eq = eq_with(&[
			("peak2_freq", 1_000.0),
			("peak2_gain", 9.0),
			("peak2_q", 2.0),
			("peak2_ms", 2.0),
			("peak1_freq", 300.0),
			("peak1_gain", -6.0),
			("peak1_q", 2.0),
			("peak1_ms", 1.0),
			("output_gain", -2.0),
		]);
		let (mid, side) = mid_side_gains(&mut eq, 1_000.0);
		assert!(
			(mid + 2.0).abs() < 0.3,
			"the mid skips the side band: {mid}"
		);
		assert!((side - 7.0).abs() < 0.3, "the side gets it: {side}");
		let (mid, side) = mid_side_gains(&mut eq, 300.0);
		assert!((mid + 8.0).abs() < 0.4, "mid band on the mid: {mid}");
		assert!((side + 2.0).abs() < 0.4, "not on the side: {side}");
		// A stereo band filters both channels.
		let mut eq = eq_with(&[
			("peak3_freq", 5_000.0),
			("peak3_gain", 6.0),
			("peak2_ms", 1.0),
		]);
		let (mid, side) = mid_side_gains(&mut eq, 5_000.0);
		assert!(
			(mid - 6.0).abs() < 0.3 && (side - 6.0).abs() < 0.3,
			"{mid} {side}"
		);
		// The picture agrees.
		let mut params: Vec<f32> = PARAMS.iter().map(|p| p.default).collect();
		params[P_P2_FREQ] = 1_000.0;
		params[P_P2_GAIN] = 9.0;
		params[channel_param_index(3).unwrap()] = 2.0;
		let bands = EqBands::new(&params, 48_000.0);
		assert!(bands.uses_mid_side());
		assert_eq!(bands.channel(3), EqChannel::Side);
		assert!(bands.channel_response_db(false, 1_000.0).abs() < 0.05);
		assert!((bands.channel_response_db(true, 1_000.0) - 9.0).abs() < 0.1);
		assert_eq!(INFO.params[P_MS_START].id, "hp_ms");
		assert_eq!(INFO.params[N_PARAMS - 1].id, "lp_ms");
	}

	#[test]
	fn listening_to_a_side_band_hears_only_the_side() {
		let mut eq = eq_with(&[("peak2_freq", 1_000.0), ("peak2_ms", 2.0)]);
		assert!(eq.set_listen(Listen::Band(3)));
		let (mid, side) = mid_side_gains(&mut eq, 1_000.0);
		assert!(mid < -60.0, "{mid}");
		assert!(side.abs() < 0.3, "{side}");
	}
}
