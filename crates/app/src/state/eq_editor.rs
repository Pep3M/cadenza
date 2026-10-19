//! Pure logic of the Parametric EQ editor (`eq`, design `15 EQ and Compressor`): the seven
//! bands (HP, LS, Peak 1-3, HS, LP) as display nodes, and the parameter changes each
//! gesture makes (drag, wheel, double-click, wake, sleep). The view turns the changes
//! into one command per gesture.

use cadenza_dsp::{DeviceInfo, ParamInfo};

pub const N_BANDS: usize = 7;

/// A band's gain at or below this (in dB) leaves it dormant: drawn as a hollow ring.
pub const DORMANT_DB: f32 = 0.05;
/// The slope a high-pass / low-pass gets when woken (24 dB/oct).
pub const WAKE_SLOPE: f32 = 2.0;
/// Smallest gain a woken shelf or peak gets, so it does not stay dormant.
const WAKE_MIN_DB: f32 = 1.0;
/// One wheel notch multiplies Q by this.
const Q_STEP: f32 = 1.1;

/// Slope choices of the high-pass and low-pass (param values 0..=3).
pub const SLOPES: [&str; 4] = ["Off", "12", "24", "48"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BandKind {
	HighPass,
	LowShelf,
	Peak,
	HighShelf,
	LowPass,
}

impl BandKind {
	/// High-pass and low-pass: switched by their slope, no gain (x-only nodes).
	pub fn is_pass(self) -> bool {
		matches!(self, BandKind::HighPass | BandKind::LowPass)
	}
}

/// One band and the parameter ids it owns.
#[derive(Debug, Clone, Copy)]
pub struct Band {
	pub kind: BandKind,
	/// Node and chip label.
	pub label: &'static str,
	pub name: &'static str,
	pub freq: &'static str,
	pub gain: Option<&'static str>,
	pub q: Option<&'static str>,
	pub slope: Option<&'static str>,
}

impl Band {
	/// Every parameter of the band.
	pub fn params(&self) -> impl Iterator<Item = &'static str> {
		[Some(self.freq), self.gain, self.q, self.slope]
			.into_iter()
			.flatten()
	}
}

/// The bands in display order (also [`cadenza_dsp::response::EqBands`] band order).
pub const BANDS: [Band; N_BANDS] = [
	Band {
		kind: BandKind::HighPass,
		label: "HP",
		name: "High-pass",
		freq: "hp_freq",
		gain: None,
		q: None,
		slope: Some("hp_slope"),
	},
	Band {
		kind: BandKind::LowShelf,
		label: "LS",
		name: "Low shelf",
		freq: "low_shelf_freq",
		gain: Some("low_shelf_gain"),
		q: None,
		slope: None,
	},
	Band {
		kind: BandKind::Peak,
		label: "1",
		name: "Peak 1",
		freq: "peak1_freq",
		gain: Some("peak1_gain"),
		q: Some("peak1_q"),
		slope: None,
	},
	Band {
		kind: BandKind::Peak,
		label: "2",
		name: "Peak 2",
		freq: "peak2_freq",
		gain: Some("peak2_gain"),
		q: Some("peak2_q"),
		slope: None,
	},
	Band {
		kind: BandKind::Peak,
		label: "3",
		name: "Peak 3",
		freq: "peak3_freq",
		gain: Some("peak3_gain"),
		q: Some("peak3_q"),
		slope: None,
	},
	Band {
		kind: BandKind::HighShelf,
		label: "HS",
		name: "High shelf",
		freq: "high_shelf_freq",
		gain: Some("high_shelf_gain"),
		q: None,
		slope: None,
	},
	Band {
		kind: BandKind::LowPass,
		label: "LP",
		name: "Low-pass",
		freq: "lp_freq",
		gain: None,
		q: None,
		slope: Some("lp_slope"),
	},
];

pub const OUTPUT: &str = "output_gain";

/// A parameter change: id and plain value.
pub type Change = (&'static str, f32);

pub fn info() -> &'static DeviceInfo {
	cadenza_dsp::device_info("eq").expect("the EQ is a built-in device")
}

fn param(id: &str) -> &'static ParamInfo {
	info().param(id).expect("EQ parameter ids are fixed")
}

/// `value` clamped to the range of `id`.
pub fn clamp(id: &str, value: f32) -> f32 {
	let p = param(id);
	value.clamp(p.min, p.max)
}

/// Every EQ parameter value, in the device's parameter order (what
/// [`cadenza_dsp::response::EqBands::new`] takes).
#[derive(Debug, Clone, PartialEq)]
pub struct EqValues {
	values: Vec<f32>,
}

impl Default for EqValues {
	fn default() -> Self {
		Self::from_fn(|p| p.default)
	}
}

impl EqValues {
	pub fn from_fn(f: impl FnMut(&'static ParamInfo) -> f32) -> Self {
		Self {
			values: info().params.iter().map(f).collect(),
		}
	}

	fn index(id: &str) -> usize {
		info()
			.params
			.iter()
			.position(|p| p.id == id)
			.expect("EQ parameter ids are fixed")
	}

	pub fn get(&self, id: &str) -> f32 {
		self.values[Self::index(id)]
	}

	pub fn set(&mut self, id: &str, value: f32) {
		self.values[Self::index(id)] = value;
	}

	pub fn apply(&mut self, changes: &[Change]) {
		for &(id, v) in changes {
			self.set(id, v);
		}
	}

	pub fn as_slice(&self) -> &[f32] {
		&self.values
	}
}

/// The slope index (0 off .. 3 = 48 dB/oct) of a slope value.
pub fn slope_index(value: f32) -> usize {
	(value.round().max(0.0) as usize).min(SLOPES.len() - 1)
}

/// Where `band`'s node sits: (frequency, dB). High-pass and low-pass sit on 0 dB.
pub fn node(v: &EqValues, band: usize) -> (f32, f32) {
	let b = &BANDS[band];
	(v.get(b.freq), b.gain.map_or(0.0, |g| v.get(g)))
}

/// A band that does nothing: high-pass / low-pass off, or a shelf / peak at 0 dB.
pub fn is_dormant(v: &EqValues, band: usize) -> bool {
	let b = &BANDS[band];
	match (b.gain, b.slope) {
		(Some(g), _) => v.get(g).abs() <= DORMANT_DB,
		(None, Some(s)) => slope_index(v.get(s)) == 0,
		(None, None) => false,
	}
}

/// The changes that put `band`'s node at `freq` / `db` (high-pass and low-pass move in
/// frequency only), clamped to the parameter ranges.
pub fn move_node(band: usize, freq: f32, db: f32) -> Vec<Change> {
	let b = &BANDS[band];
	let mut out = vec![(b.freq, clamp(b.freq, freq))];
	if let Some(g) = b.gain {
		out.push((g, clamp(g, db)));
	}
	out
}

/// Distance in octaves between two frequencies.
fn octaves(a: f32, b: f32) -> f32 {
	(a.max(1.0) / b.max(1.0)).log2().abs()
}

/// The band a drag on the curve at `freq` moves: the nearest awake band (in octaves),
/// or the nearest of all when every band sleeps.
pub fn nearest_band(v: &EqValues, freq: f32) -> usize {
	let nearest = |awake_only: bool| {
		(0..N_BANDS)
			.filter(|&b| !awake_only || !is_dormant(v, b))
			.min_by(|&a, &b| octaves(node(v, a).0, freq).total_cmp(&octaves(node(v, b).0, freq)))
	};
	nearest(true).or_else(|| nearest(false)).unwrap_or(0)
}

/// The dormant band a double-click at `freq` wakes: the nearest dormant band whose
/// frequency range holds `freq`, else the nearest dormant band; `None` when all are on.
pub fn nearest_dormant(v: &EqValues, freq: f32) -> Option<usize> {
	let nearest = |fits_only: bool| {
		(0..N_BANDS)
			.filter(|&b| is_dormant(v, b))
			.filter(|&b| {
				let p = param(BANDS[b].freq);
				!fits_only || (p.min..=p.max).contains(&freq)
			})
			.min_by(|&a, &b| octaves(node(v, a).0, freq).total_cmp(&octaves(node(v, b).0, freq)))
	};
	nearest(true).or_else(|| nearest(false))
}

/// Wakes `band` at the pointer: its frequency goes there; a shelf or peak takes the
/// pointer's gain (at least ±1 dB), a high-pass / low-pass switches on at 24 dB/oct.
pub fn wake(band: usize, freq: f32, db: f32) -> Vec<Change> {
	let b = &BANDS[band];
	let mut out = vec![(b.freq, clamp(b.freq, freq))];
	if let Some(g) = b.gain {
		let db = if db.abs() < WAKE_MIN_DB {
			WAKE_MIN_DB.copysign(if db == 0.0 { 1.0 } else { db })
		} else {
			db
		};
		out.push((g, clamp(g, db)));
	}
	if let Some(s) = b.slope {
		out.push((s, WAKE_SLOPE));
	}
	out
}

/// Puts `band` to sleep: gain to 0 dB, or the slope off.
pub fn sleep(band: usize) -> Vec<Change> {
	let b = &BANDS[band];
	match (b.gain, b.slope) {
		(Some(g), _) => vec![(g, 0.0)],
		(None, Some(s)) => vec![(s, 0.0)],
		(None, None) => Vec::new(),
	}
}

/// Double-click on a node: a shelf or peak's gain to 0 dB; a high-pass / low-pass
/// toggles off ↔ 24 dB/oct.
pub fn reset_node(v: &EqValues, band: usize) -> Vec<Change> {
	let b = &BANDS[band];
	match (b.gain, b.slope) {
		(Some(g), _) => vec![(g, param(g).default)],
		(None, Some(s)) => {
			let on = slope_index(v.get(s)) > 0;
			vec![(s, if on { 0.0 } else { WAKE_SLOPE })]
		}
		(None, None) => Vec::new(),
	}
}

/// Every parameter of `band` back to its default.
pub fn reset_band(band: usize) -> Vec<Change> {
	BANDS[band]
		.params()
		.map(|id| (id, param(id).default))
		.collect()
}

/// The parameter the wheel changes over `band`'s node: Q on peaks, the slope on
/// high-pass / low-pass, nothing on shelves.
pub fn wheel_param(band: usize) -> Option<&'static str> {
	let b = &BANDS[band];
	b.q.or(b.slope)
}

/// One slope step per (rounded) notch, up = steeper.
pub fn step_slope(slope: f32, notches: f32) -> f32 {
	(slope_index(slope) as f32 + notches.round()).clamp(0.0, (SLOPES.len() - 1) as f32)
}

/// The value of [`wheel_param`] after `notches` from `start` (Q: ×1.1 per notch).
pub fn wheel_value(band: usize, start: f32, notches: f32) -> Option<f32> {
	let b = &BANDS[band];
	if let Some(q) = b.q {
		return Some(clamp(q, start * Q_STEP.powf(notches)));
	}
	b.slope.map(|_| step_slope(start, notches))
}

/// The -3 dB edges (Hz) of a peak at `freq` with `q` (RBJ bandwidth in octaves).
pub fn q_bracket(freq: f32, q: f32) -> (f32, f32) {
	let octaves = 2.0 / std::f32::consts::LN_2 * (1.0 / (2.0 * q.max(1e-3))).asinh();
	let half = 2f32.powf(octaves * 0.5);
	(freq / half, freq * half)
}

/// What a parameter is called inside its band ("gain", "freq", "Q", "slope", "output").
pub fn param_word(id: &str) -> &'static str {
	if id.ends_with("_freq") {
		"freq"
	} else if id.ends_with("_gain") && id != OUTPUT {
		"gain"
	} else if id.ends_with("_q") {
		"Q"
	} else if id.ends_with("_slope") {
		"slope"
	} else {
		"output"
	}
}

/// The undo label of `changes` to `band`: "Peak 2 gain", "Peak 2 freq and gain".
pub fn label(band: usize, changes: &[Change]) -> String {
	let words: Vec<&str> = changes.iter().map(|(id, _)| param_word(id)).collect();
	let what = match words.as_slice() {
		[] => return BANDS[band].name.to_owned(),
		[one] => (*one).to_owned(),
		[rest @ .., last] => format!("{} and {last}", rest.join(", ")),
	};
	format!("{} {what}", BANDS[band].name)
}

#[cfg(test)]
mod tests {
	use super::*;

	fn values(changes: &[Change]) -> EqValues {
		let mut v = EqValues::default();
		v.apply(changes);
		v
	}

	#[test]
	fn bands_name_real_params_in_response_order() {
		for b in &BANDS {
			for id in b.params() {
				assert!(info().param(id).is_some(), "{id}");
			}
		}
		assert_eq!(BANDS[0].kind, BandKind::HighPass);
		assert_eq!(BANDS[6].kind, BandKind::LowPass);
		assert!(BANDS[0].kind.is_pass() && !BANDS[3].kind.is_pass());
	}

	#[test]
	fn nodes_read_freq_and_gain_and_pass_bands_sit_on_zero() {
		let v = values(&[
			("peak2_freq", 2800.0),
			("peak2_gain", 2.5),
			("hp_freq", 80.0),
		]);
		assert_eq!(node(&v, 3), (2800.0, 2.5));
		assert_eq!(node(&v, 0), (80.0, 0.0));
		// Moving a pass band ignores the gain; values clamp to the ranges.
		assert_eq!(move_node(0, 90.0, 6.0), vec![("hp_freq", 90.0)]);
		assert_eq!(
			move_node(3, 30_000.0, -40.0),
			vec![("peak2_freq", 20_000.0), ("peak2_gain", -24.0)]
		);
		assert_eq!(move_node(1, 5000.0, 3.0)[0], ("low_shelf_freq", 1000.0));
	}

	#[test]
	fn dormancy_follows_gain_and_slope() {
		let v = EqValues::default();
		assert!(
			(0..N_BANDS).all(|b| is_dormant(&v, b)),
			"defaults do nothing"
		);
		let v = values(&[
			("hp_slope", 1.0),
			("peak1_gain", -0.5),
			("high_shelf_gain", 0.04),
		]);
		assert!(!is_dormant(&v, 0));
		assert!(!is_dormant(&v, 2));
		assert!(is_dormant(&v, 5));
		assert!(is_dormant(&v, 6));
	}

	#[test]
	fn the_curve_moves_the_nearest_awake_band() {
		let v = values(&[
			("peak1_gain", -3.0),
			("peak1_freq", 300.0),
			("peak3_gain", 2.0),
			("peak3_freq", 5000.0),
		]);
		assert_eq!(nearest_band(&v, 400.0), 2);
		assert_eq!(nearest_band(&v, 3000.0), 4);
		// Peak 2 sits right there but sleeps.
		assert_eq!(nearest_band(&v, 1000.0), 2);
		// Nothing awake: the nearest of all.
		assert_eq!(nearest_band(&EqValues::default(), 1000.0), 3);
	}

	#[test]
	fn double_click_wakes_the_nearest_dormant_band_that_can_reach_the_pointer() {
		let v = values(&[("peak2_gain", 3.0)]);
		// 1.1 kHz: Peak 3 (4 kHz) beats Peak 1 (250 Hz); the low shelf cannot reach it.
		assert_eq!(nearest_dormant(&v, 1100.0), Some(4));
		// 900 Hz: Peak 1 (250 Hz) is nearer than Peak 3 (4 kHz); Peak 2 is awake.
		assert_eq!(nearest_dormant(&v, 900.0), Some(2));
		// 15 kHz: the low-pass at 18 kHz.
		assert_eq!(nearest_dormant(&v, 15_000.0), Some(6));
		// 25 Hz: the high-pass at 30 Hz.
		assert_eq!(nearest_dormant(&v, 25.0), Some(0));
		let all_on = values(&[
			("hp_slope", 1.0),
			("low_shelf_gain", 1.0),
			("peak1_gain", 1.0),
			("peak2_gain", 1.0),
			("peak3_gain", 1.0),
			("high_shelf_gain", 1.0),
			("lp_slope", 1.0),
		]);
		assert_eq!(nearest_dormant(&all_on, 1000.0), None);
		assert_eq!(
			wake(3, 2000.0, 0.2),
			vec![("peak2_freq", 2000.0), ("peak2_gain", 1.0)]
		);
		assert_eq!(wake(3, 2000.0, -6.0)[1], ("peak2_gain", -6.0));
		assert_eq!(wake(3, 2000.0, -0.1)[1], ("peak2_gain", -1.0));
		assert_eq!(
			wake(0, 60.0, 5.0),
			vec![("hp_freq", 60.0), ("hp_slope", 2.0)]
		);
		let mut woken = v.clone();
		woken.apply(&wake(4, 6000.0, 4.0));
		assert!(!is_dormant(&woken, 4));
	}

	#[test]
	fn sleep_and_reset() {
		assert_eq!(sleep(3), vec![("peak2_gain", 0.0)]);
		assert_eq!(sleep(6), vec![("lp_slope", 0.0)]);
		let v = values(&[("peak2_gain", 4.0), ("hp_slope", 3.0)]);
		assert_eq!(reset_node(&v, 3), vec![("peak2_gain", 0.0)]);
		assert_eq!(reset_node(&v, 0), vec![("hp_slope", 0.0)]);
		assert_eq!(reset_node(&EqValues::default(), 0), vec![("hp_slope", 2.0)]);
		let r = reset_band(3);
		assert_eq!(
			r,
			vec![
				("peak2_freq", 1000.0),
				("peak2_gain", 0.0),
				("peak2_q", 1.0)
			]
		);
	}

	#[test]
	fn wheel_changes_q_on_peaks_and_steps_the_slope() {
		assert_eq!(wheel_param(3), Some("peak2_q"));
		assert_eq!(wheel_param(0), Some("hp_slope"));
		assert_eq!(wheel_param(1), None);
		let q = wheel_value(3, 1.0, 2.0).unwrap();
		assert!((q - 1.21).abs() < 1e-4);
		assert_eq!(wheel_value(3, 17.0, 5.0), Some(18.0));
		assert_eq!(wheel_value(1, 1.0, 1.0), None);
		assert_eq!(step_slope(0.0, 1.0), 1.0);
		assert_eq!(step_slope(2.0, -1.0), 1.0);
		assert_eq!(step_slope(3.0, 2.0), 3.0);
		assert_eq!(step_slope(0.0, -1.0), 0.0);
		// Half a notch of smooth scrolling rounds.
		assert_eq!(step_slope(1.0, 0.4), 1.0);
		assert_eq!(wheel_value(6, 1.0, 0.6), Some(2.0));
	}

	#[test]
	fn q_brackets_span_the_bandwidth() {
		// Q 1.41 is about one octave wide.
		let (lo, hi) = q_bracket(1000.0, std::f32::consts::SQRT_2);
		assert!(((hi / lo).log2() - 1.0).abs() < 0.01, "{lo} {hi}");
		assert!(((lo * hi).sqrt() - 1000.0).abs() < 0.5);
		let (lo2, hi2) = q_bracket(1000.0, 8.0);
		assert!(hi2 / lo2 < hi / lo, "higher Q is narrower");
	}

	#[test]
	fn labels_name_the_band_and_params() {
		assert_eq!(label(3, &[("peak2_gain", 2.0)]), "Peak 2 gain");
		assert_eq!(
			label(3, &[("peak2_freq", 2.0), ("peak2_gain", 2.0)]),
			"Peak 2 freq and gain"
		);
		assert_eq!(label(0, &[("hp_slope", 2.0)]), "High-pass slope");
		assert_eq!(label(2, &[("peak1_q", 2.0)]), "Peak 1 Q");
		assert_eq!(param_word(OUTPUT), "output");
	}
}
