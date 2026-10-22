//! Match EQ: the smoothed third-octave difference between a reference and a signal, and a
//! fit of that difference with the bands of the built-in Parametric EQ (low shelf, three
//! bells, high shelf). Pure and offline; the MCP layer turns the fit into EQ params.
//!
//! The band shapes are the RBJ cookbook responses the EQ itself runs (shelves with slope
//! S = 1), evaluated at 48 kHz: the fitted curve and the sound agree well below Nyquist.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::moves::EqKind;
use crate::spectrum::THIRD_OCTAVE_HZ;

/// Sample rate the band shapes are evaluated at.
const MODEL_RATE: f64 = 48_000.0;
/// Third-octave bands this far under a spectrum's loudest band are ignored (noise floor).
const RANGE_DB: f32 = 60.0;
/// The level match uses the bands between these (outside them a reference often differs
/// by its rumble or its air, which the fit should see but not level by).
const LEVEL_FROM_HZ: f32 = 63.0;
const LEVEL_TO_HZ: f32 = 12_500.0;
/// A band is kept only when it lowers the weighted squared error by this share.
const MIN_GAIN_SHARE: f32 = 0.02;
/// Bands quieter than this are dropped.
const MIN_BAND_DB: f32 = 0.3;
const BELL_QS: [f32; 7] = [0.4, 0.6, 0.8, 1.0, 1.4, 2.0, 2.8];
const SHELF_Q: f32 = std::f32::consts::FRAC_1_SQRT_2;

/// How much of the difference to fit and with how many bands.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MatchOptions {
	/// 0..=1: share of the difference applied (0.5 = half way to the reference).
	pub amount: f32,
	/// Largest boost or cut of one band (dB).
	pub max_gain_db: f32,
	/// At most this many bands (low shelf, three bells, high shelf: 1..=5).
	pub max_bands: usize,
}

impl Default for MatchOptions {
	fn default() -> Self {
		Self {
			amount: 1.0,
			max_gain_db: 6.0,
			max_bands: 5,
		}
	}
}

/// One fitted band. `q` is the bell's Q (shelves: 0.707, their fixed slope).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MatchBand {
	pub kind: EqKind,
	#[serde(serialize_with = "crate::short::f32")]
	pub freq_hz: f32,
	#[serde(serialize_with = "crate::short::f32")]
	pub gain_db: f32,
	#[serde(serialize_with = "crate::short::f32")]
	pub q: f32,
}

/// The fit: bands, the curve they make and how far it is from the target.
#[derive(Debug, Clone, PartialEq)]
pub struct MatchFit {
	pub bands: Vec<MatchBand>,
	/// The difference fitted (amount applied), per [`THIRD_OCTAVE_HZ`] band.
	pub target_db: Vec<f32>,
	/// What the bands give at the same frequencies.
	pub fitted_db: Vec<f32>,
	/// Weighted RMS of the target before (`before_db`) and of target - fit (`residual_db`).
	pub before_db: f32,
	pub residual_db: f32,
}

/// Magnitude (dB) of `band` at `hz`.
pub fn band_response_db(band: &MatchBand, hz: f32) -> f32 {
	shape_db(band.kind, band.freq_hz, band.gain_db, band.q, hz)
}

/// Summed magnitude (dB) of `bands` at `hz`.
pub fn response_db(bands: &[MatchBand], hz: f32) -> f32 {
	bands.iter().map(|b| band_response_db(b, hz)).sum()
}

fn shape_db(kind: EqKind, freq: f32, gain_db: f32, q: f32, hz: f32) -> f32 {
	let sr = MODEL_RATE;
	let w0 = std::f64::consts::TAU * f64::from(freq.clamp(10.0, 23_000.0)) / sr;
	let (cos_w, sin_w) = (w0.cos(), w0.sin());
	let a = 10f64.powf(f64::from(gain_db) / 40.0);
	let q = match kind {
		EqKind::Bell => f64::from(q.max(0.05)),
		_ => f64::from(SHELF_Q),
	};
	let alpha = sin_w / (2.0 * q);
	let sq = 2.0 * a.sqrt() * alpha;
	let (b0, b1, b2, a0, a1, a2) = match kind {
		EqKind::Bell => (
			1.0 + alpha * a,
			-2.0 * cos_w,
			1.0 - alpha * a,
			1.0 + alpha / a,
			-2.0 * cos_w,
			1.0 - alpha / a,
		),
		EqKind::LowShelf => (
			a * ((a + 1.0) - (a - 1.0) * cos_w + sq),
			2.0 * a * ((a - 1.0) - (a + 1.0) * cos_w),
			a * ((a + 1.0) - (a - 1.0) * cos_w - sq),
			(a + 1.0) + (a - 1.0) * cos_w + sq,
			-2.0 * ((a - 1.0) + (a + 1.0) * cos_w),
			(a + 1.0) + (a - 1.0) * cos_w - sq,
		),
		EqKind::HighShelf => (
			a * ((a + 1.0) + (a - 1.0) * cos_w + sq),
			-2.0 * a * ((a - 1.0) + (a + 1.0) * cos_w),
			a * ((a + 1.0) + (a - 1.0) * cos_w - sq),
			(a + 1.0) - (a - 1.0) * cos_w + sq,
			2.0 * ((a - 1.0) - (a + 1.0) * cos_w),
			(a + 1.0) - (a - 1.0) * cos_w - sq,
		),
		EqKind::HighPass | EqKind::LowPass => return 0.0,
	};
	let w = std::f64::consts::TAU * f64::from(hz) / sr;
	let (c1, s1) = (w.cos(), -w.sin());
	let (c2, s2) = ((2.0 * w).cos(), -(2.0 * w).sin());
	let num = (b0 + b1 * c1 + b2 * c2).hypot(b1 * s1 + b2 * s2);
	let den = (a0 + a1 * c1 + a2 * c2).hypot(a1 * s1 + a2 * s2);
	(20.0 * (num / den.max(1e-30)).max(1e-30).log10()) as f32
}

/// The tonal difference reference - source per [`THIRD_OCTAVE_HZ`] band (both in dB,
/// e.g. `Spectrum::third_octave_db`): smoothed over about 2/3 octave and level matched
/// (the mean between 63 Hz and 12.5 kHz removed), so only the shape is left. Bands where
/// either signal is near silence give 0.
pub fn match_target(reference: &[f32], source: &[f32]) -> Vec<f32> {
	let n = THIRD_OCTAVE_HZ.len();
	let floor = |s: &[f32]| s.iter().copied().fold(f32::MIN, f32::max) - RANGE_DB;
	let (fr, fs) = (floor(reference), floor(source));
	let valid: Vec<bool> = (0..n)
		.map(|i| {
			matches!((reference.get(i), source.get(i)), (Some(&r), Some(&s))
				if r.is_finite() && s.is_finite() && r > fr && s > fs)
		})
		.collect();
	let raw: Vec<f32> = (0..n)
		.map(|i| {
			if valid[i] {
				reference[i] - source[i]
			} else {
				0.0
			}
		})
		.collect();
	const TAPS: [f32; 5] = [1.0, 2.0, 3.0, 2.0, 1.0];
	let smooth: Vec<f32> = (0..n)
		.map(|i| {
			let (mut sum, mut weight) = (0.0, 0.0);
			for (k, t) in TAPS.iter().enumerate() {
				let j = i as isize + k as isize - 2;
				if j >= 0 && (j as usize) < n && valid[j as usize] {
					sum += raw[j as usize] * t;
					weight += t;
				}
			}
			if valid[i] && weight > 0.0 {
				sum / weight
			} else {
				0.0
			}
		})
		.collect();
	let level: Vec<f32> = (0..n)
		.filter(|&i| valid[i] && (LEVEL_FROM_HZ..=LEVEL_TO_HZ).contains(&THIRD_OCTAVE_HZ[i]))
		.map(|i| smooth[i])
		.collect();
	let mean = if level.is_empty() {
		0.0
	} else {
		level.iter().sum::<f32>() / level.len() as f32
	};
	(0..n)
		.map(|i| if valid[i] { smooth[i] - mean } else { 0.0 })
		.collect()
}

/// Weight of each third-octave band in the fit: the extremes count less.
fn weight(i: usize) -> f32 {
	match THIRD_OCTAVE_HZ[i] {
		f if f < 35.0 || f > 18_000.0 => 0.4,
		_ => 1.0,
	}
}

fn error(target: &[f32], curve: &[f32]) -> f32 {
	target
		.iter()
		.zip(curve)
		.enumerate()
		.map(|(i, (t, c))| weight(i) * (t - c) * (t - c))
		.sum()
}

fn rms(target: &[f32], curve: &[f32]) -> f32 {
	let w: f32 = (0..target.len()).map(weight).sum();
	(error(target, curve) / w.max(1e-6)).sqrt()
}

fn sample(kind: EqKind, freq: f32, gain: f32, q: f32) -> Vec<f32> {
	THIRD_OCTAVE_HZ
		.iter()
		.map(|&hz| shape_db(kind, freq, gain, q, hz))
		.collect()
}

/// The best band of `kind` at `freq` / `q` for `residual`: its gain (least squares, two
/// passes for the shape's level dependence, clamped) and the error it leaves.
fn best_gain(kind: EqKind, freq: f32, q: f32, residual: &[f32], max: f32) -> (MatchBand, f32) {
	let mut gain = 6.0_f32;
	for _ in 0..3 {
		let probe = if gain.abs() < 0.5 {
			0.5f32.copysign(gain)
		} else {
			gain
		};
		let shape = sample(kind, freq, probe, q);
		let (mut num, mut den) = (0.0, 0.0);
		for (i, (r, s)) in residual.iter().zip(&shape).enumerate() {
			let unit = s / probe;
			num += weight(i) * r * unit;
			den += weight(i) * unit * unit;
		}
		gain = if den > 1e-9 {
			(num / den).clamp(-max, max)
		} else {
			0.0
		};
	}
	let band = MatchBand {
		kind,
		freq_hz: freq,
		gain_db: gain,
		q: if kind == EqKind::Bell { q } else { SHELF_Q },
	};
	let e = error(residual, &sample(kind, freq, gain, band.q));
	(band, e)
}

/// Candidate frequencies of `kind`: sixth-octave steps inside the EQ band's range.
fn grid(kind: EqKind) -> Vec<f32> {
	let (lo, hi) = match kind {
		EqKind::LowShelf => (30.0, 800.0),
		EqKind::HighShelf => (1_500.0, 16_000.0),
		_ => (25.0, 18_000.0),
	};
	(0..)
		.map(|k| 20.0 * 2f32.powf(k as f32 / 6.0))
		.skip_while(|&f| f < lo)
		.take_while(|&f| f <= hi)
		.collect()
}

fn best_band(kind: EqKind, residual: &[f32], max: f32) -> (MatchBand, f32) {
	let qs: &[f32] = if kind == EqKind::Bell {
		&BELL_QS
	} else {
		&[SHELF_Q]
	};
	let mut best: Option<(MatchBand, f32)> = None;
	for f in grid(kind) {
		for &q in qs {
			let cand = best_gain(kind, f, q, residual, max);
			if best.is_none_or(|b| cand.1 < b.1) {
				best = Some(cand);
			}
		}
	}
	best.expect("every grid has frequencies")
}

fn curve_of(bands: &[MatchBand]) -> Vec<f32> {
	THIRD_OCTAVE_HZ
		.iter()
		.map(|&hz| response_db(bands, hz))
		.collect()
}

/// Fits up to `opts.max_bands` EQ bands (one low shelf, three bells, one high shelf) to
/// `target` (dB per [`THIRD_OCTAVE_HZ`] band, e.g. from [`match_target`]) scaled by
/// `opts.amount`: greedy (the band that lowers the error most, while it helps), then a few
/// passes refining each band around its frequency and Q with the others fixed. Gains stay
/// within ±`opts.max_gain_db`.
pub fn fit(target: &[f32], opts: &MatchOptions) -> MatchFit {
	let amount = opts.amount.clamp(0.0, 1.0);
	let max = opts.max_gain_db.abs().max(0.1);
	let target: Vec<f32> = (0..THIRD_OCTAVE_HZ.len())
		.map(|i| target.get(i).copied().unwrap_or(0.0) * amount)
		.collect();
	let zero = vec![0.0; target.len()];
	let initial = error(&target, &zero);
	let mut bands: Vec<MatchBand> = Vec::new();
	let limit = opts.max_bands.min(5);
	while bands.len() < limit && initial > 1e-6 {
		let curve = curve_of(&bands);
		let residual: Vec<f32> = target.iter().zip(&curve).map(|(t, c)| t - c).collect();
		let current = error(&residual, &zero);
		let used = |k: EqKind| bands.iter().filter(|b| b.kind == k).count();
		let mut kinds = Vec::new();
		if used(EqKind::LowShelf) == 0 {
			kinds.push(EqKind::LowShelf);
		}
		if used(EqKind::Bell) < 3 {
			kinds.push(EqKind::Bell);
		}
		if used(EqKind::HighShelf) == 0 {
			kinds.push(EqKind::HighShelf);
		}
		let Some((band, e)) = kinds
			.into_iter()
			.map(|k| best_band(k, &residual, max))
			.min_by(|a, b| a.1.total_cmp(&b.1))
		else {
			break;
		};
		if current - e < MIN_GAIN_SHARE * initial || band.gain_db.abs() < MIN_BAND_DB {
			break;
		}
		bands.push(band);
	}
	for _ in 0..3 {
		for j in 0..bands.len() {
			let others: Vec<MatchBand> = bands
				.iter()
				.enumerate()
				.filter(|&(k, _)| k != j)
				.map(|(_, b)| *b)
				.collect();
			let curve = curve_of(&others);
			let residual: Vec<f32> = target.iter().zip(&curve).map(|(t, c)| t - c).collect();
			let b = bands[j];
			let mut best = (
				b,
				error(&residual, &sample(b.kind, b.freq_hz, b.gain_db, b.q)),
			);
			let (lo, hi) = match b.kind {
				EqKind::LowShelf => (20.0, 1_000.0),
				EqKind::HighShelf => (1_000.0, 20_000.0),
				_ => (20.0, 20_000.0),
			};
			let qs: &[f32] = if b.kind == EqKind::Bell {
				&[0.7, 0.85, 1.0, 1.2, 1.4]
			} else {
				&[1.0]
			};
			for m in -4..=4 {
				let f = (b.freq_hz * 2f32.powf(m as f32 / 12.0)).clamp(lo, hi);
				for &qm in qs {
					let q = (b.q * qm).clamp(0.1, 18.0);
					let cand = best_gain(b.kind, f, q, &residual, max);
					if cand.1 < best.1 {
						best = cand;
					}
				}
			}
			bands[j] = best.0;
		}
	}
	bands.retain(|b| b.gain_db.abs() >= MIN_BAND_DB);
	bands.sort_by(|a, b| {
		let rank = |k: EqKind| match k {
			EqKind::LowShelf => 0,
			EqKind::HighShelf => 2,
			_ => 1,
		};
		rank(a.kind)
			.cmp(&rank(b.kind))
			.then(a.freq_hz.total_cmp(&b.freq_hz))
	});
	let fitted = curve_of(&bands);
	MatchFit {
		before_db: rms(&target, &zero),
		residual_db: rms(&target, &fitted),
		bands,
		target_db: target,
		fitted_db: fitted,
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn band(kind: EqKind, freq_hz: f32, gain_db: f32, q: f32) -> MatchBand {
		MatchBand {
			kind,
			freq_hz,
			gain_db,
			q,
		}
	}

	fn octaves(a: f32, b: f32) -> f32 {
		(a / b).log2().abs()
	}

	#[test]
	fn synthetic_spectra_recover_their_bands() {
		let truth = [
			band(EqKind::LowShelf, 120.0, -3.0, SHELF_Q),
			band(EqKind::Bell, 1_000.0, 4.0, 1.4),
			band(EqKind::HighShelf, 8_000.0, 2.5, SHELF_Q),
		];
		let target = curve_of(&truth);
		let fit = fit(&target, &MatchOptions::default());
		assert!(fit.residual_db < 0.3, "residual {}", fit.residual_db);
		assert!(fit.before_db > 1.5, "before {}", fit.before_db);
		let find = |kind: EqKind, near: f32| {
			fit.bands
				.iter()
				.filter(|b| b.kind == kind)
				.min_by(|a, b| octaves(a.freq_hz, near).total_cmp(&octaves(b.freq_hz, near)))
				.copied()
				.unwrap_or_else(|| panic!("no {kind:?} in {:?}", fit.bands))
		};
		let bell = find(EqKind::Bell, 1_000.0);
		assert!(octaves(bell.freq_hz, 1_000.0) < 0.34, "{bell:?}");
		assert!((bell.gain_db - 4.0).abs() < 0.8, "{bell:?}");
		let low = find(EqKind::LowShelf, 120.0);
		assert!((low.gain_db + 3.0).abs() < 1.0, "{low:?}");
		let high = find(EqKind::HighShelf, 8_000.0);
		assert!((high.gain_db - 2.5).abs() < 1.0, "{high:?}");
		assert!(fit.bands.len() <= 5);
	}

	#[test]
	fn amount_and_max_gain_scale_and_clamp_the_fit() {
		let target = curve_of(&[band(EqKind::Bell, 2_000.0, 4.0, 1.0)]);
		let half = fit(
			&target,
			&MatchOptions {
				amount: 0.5,
				..MatchOptions::default()
			},
		);
		let peak = half.bands.iter().map(|b| b.gain_db).fold(0.0, f32::max);
		assert!((peak - 2.0).abs() < 0.4, "{:?}", half.bands);
		let none = fit(
			&target,
			&MatchOptions {
				amount: 0.0,
				..MatchOptions::default()
			},
		);
		assert!(none.bands.is_empty());
		let loud = curve_of(&[band(EqKind::Bell, 500.0, -14.0, 1.0)]);
		let clamped = fit(
			&loud,
			&MatchOptions {
				max_gain_db: 6.0,
				max_bands: 1,
				..MatchOptions::default()
			},
		);
		assert_eq!(clamped.bands.len(), 1);
		assert!(clamped.bands[0].gain_db >= -6.0 - 1e-4);
		assert!((clamped.bands[0].gain_db + 6.0).abs() < 0.01);
		assert!(fit(&[0.0; 31], &MatchOptions::default()).bands.is_empty());
	}

	#[test]
	fn the_target_is_the_level_matched_shape() {
		let source: Vec<f32> = THIRD_OCTAVE_HZ.iter().map(|_| -30.0).collect();
		// Reference: 10 dB louder with a tilt of +1 dB per octave above 1 kHz.
		let tilt = |hz: f32| (hz / 1_000.0).log2().max(0.0);
		let mut reference: Vec<f32> = THIRD_OCTAVE_HZ.iter().map(|&f| -20.0 + tilt(f)).collect();
		reference[0] = -140.0; // silent band
		let target = match_target(&reference, &source);
		assert_eq!(target[0], 0.0);
		// The loudness offset is gone; the tilt stays (minus its mean).
		let low = target[THIRD_OCTAVE_HZ.iter().position(|&f| f == 200.0).unwrap()];
		let high = target[THIRD_OCTAVE_HZ.iter().position(|&f| f == 8_000.0).unwrap()];
		assert!(low.abs() < 2.0, "{low}");
		assert!((high - low - 3.0).abs() < 0.5, "{high} - {low}");
		// Identical spectra: nothing to match.
		assert!(
			match_target(&source, &source)
				.iter()
				.all(|d| d.abs() < 1e-6)
		);
	}
}
