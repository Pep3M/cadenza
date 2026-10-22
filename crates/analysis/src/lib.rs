//! Offline mix analysis: the measurements Cadenza's sound engineer (the AI and the mix
//! assistant) reads to make mixing decisions. Pure code over sample buffers: the engine feeds
//! it from its offline render path, never from the audio thread.
//!
//! - [`Analyzer`] -> [`LevelStats`]: peaks, true peak, RMS, BS.1770 loudness, five bands,
//!   correlation (every render reports these).
//! - [`detail::DetailAnalyzer`] -> [`detail::Detail`]: third-octave spectrum, stereo width,
//!   low-end mono check and an octave-band timeline for masking.
//! - [`masking`], [`issues`], [`targets`], [`reference`]: what the numbers mean for a mix.
//!
//! All dB values are finite: silence is reported as [`FLOOR_DB`] (-144).

pub mod bands;
pub mod detail;
pub mod issues;
pub mod levels;
pub mod loudness;
pub mod masking;
pub mod match_eq;
pub mod moves;
pub mod reference;
pub mod roles;
pub mod short;
pub mod spectrum;
pub mod targets;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use bands::BandMeter;
use levels::{Levels, TruePeak};
use loudness::LoudnessMeter;

/// Level reported for silence (dBFS / LUFS).
pub const FLOOR_DB: f32 = -144.0;

/// Mean-square energy per frequency band of the mid (L+R)/2 signal, dB re full scale.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct BandEnergy {
	/// Below 150 Hz: kick, bass fundamentals, rumble.
	pub low: f32,
	/// 150-500 Hz: body, warmth, "mud" when excessive.
	pub low_mid: f32,
	/// 500 Hz-2 kHz: core of most instruments and vocals.
	pub mid: f32,
	/// 2-6 kHz: presence, attack, harshness when excessive.
	pub high_mid: f32,
	/// Above 6 kHz: air, cymbals, sibilance.
	pub high: f32,
}

/// Level measurements of one stereo signal.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct LevelStats {
	/// Highest absolute sample value, dBFS.
	pub peak_db: f32,
	/// Inter-sample peak estimated with 4x oversampling, dBTP.
	pub true_peak_db: f32,
	/// RMS over both channels, dBFS.
	pub rms_db: f32,
	/// Gated integrated loudness (ITU-R BS.1770-4), LUFS. Streaming targets ~ -14.
	pub lufs_integrated: f32,
	/// Loudest 3 s short-term loudness, LUFS.
	pub lufs_short_term_max: f32,
	/// peak_db - rms_db: high = dynamic/punchy, low (< 8) = heavily compressed.
	pub crest_factor_db: f32,
	/// Samples (per channel) at or above full scale.
	pub clipped_samples: u64,
	pub band_energy_db: BandEnergy,
	/// L/R correlation: +1 mono, ~0 wide, negative = phase problems. 0 for silence.
	pub stereo_correlation: f32,
}

pub fn amplitude_db(x: f64) -> f32 {
	if x <= 0.0 {
		FLOOR_DB
	} else {
		((20.0 * x.log10()) as f32).max(FLOOR_DB)
	}
}

pub fn power_db(x: f64) -> f32 {
	if x <= 0.0 {
		FLOOR_DB
	} else {
		((10.0 * x.log10()) as f32).max(FLOOR_DB)
	}
}

/// Streaming analyzer producing [`LevelStats`].
#[derive(Debug, Clone)]
pub struct Analyzer {
	levels: Levels,
	true_peak: TruePeak,
	loudness: LoudnessMeter,
	bands: BandMeter,
}

impl Analyzer {
	pub fn new(sample_rate: u32) -> Self {
		Self {
			levels: Levels::default(),
			true_peak: TruePeak::new(),
			loudness: LoudnessMeter::new(sample_rate),
			bands: BandMeter::new(sample_rate),
		}
	}

	pub fn push(&mut self, left: &[f32], right: &[f32]) {
		self.levels.push(left, right);
		self.true_peak.push(left, right);
		self.loudness.push(left, right);
		self.bands.push(left, right);
	}

	pub fn finish(&self) -> LevelStats {
		let peak_db = amplitude_db(self.levels.peak as f64);
		let rms_db = amplitude_db(self.levels.rms());
		let true_peak = self.true_peak.peak().max(self.levels.peak);
		let [low, low_mid, mid, high_mid, high] = self.bands.energy_db();
		LevelStats {
			peak_db,
			true_peak_db: amplitude_db(true_peak as f64),
			rms_db,
			lufs_integrated: self.loudness.integrated(),
			lufs_short_term_max: self.loudness.short_term_max(),
			crest_factor_db: if self.levels.peak > 0.0 {
				peak_db - rms_db
			} else {
				0.0
			},
			clipped_samples: self.levels.clipped,
			band_energy_db: BandEnergy {
				low,
				low_mid,
				mid,
				high_mid,
				high,
			},
			stereo_correlation: self.levels.correlation(),
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn silence_reports_finite_floor() {
		let mut a = Analyzer::new(48_000);
		a.push(&[0.0; 4800], &[0.0; 4800]);
		let s = a.finish();
		assert_eq!(s.peak_db, FLOOR_DB);
		assert_eq!(s.rms_db, FLOOR_DB);
		assert_eq!(s.lufs_integrated, FLOOR_DB);
		assert_eq!(s.crest_factor_db, 0.0);
		assert!(serde_json_like(&s));
	}

	fn serde_json_like(s: &LevelStats) -> bool {
		[
			s.peak_db,
			s.true_peak_db,
			s.rms_db,
			s.lufs_integrated,
			s.lufs_short_term_max,
		]
		.iter()
		.all(|v| v.is_finite())
	}

	#[test]
	fn sine_crest_factor_is_3db() {
		let x: Vec<f32> = (0..48_000)
			.map(|i| (2.0 * std::f64::consts::PI * 440.0 * i as f64 / 48_000.0).sin() as f32 * 0.5)
			.collect();
		let mut a = Analyzer::new(48_000);
		a.push(&x, &x);
		let s = a.finish();
		assert!((s.peak_db + 6.02).abs() < 0.01);
		assert!((s.crest_factor_db - 3.01).abs() < 0.02);
		assert!((s.stereo_correlation - 1.0).abs() < 1e-6);
		assert_eq!(s.clipped_samples, 0);
	}
}
