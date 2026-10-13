//! Comparison of the mix against a reference track: loudness, dynamics, width and the
//! spectral curve (both normalized to the same total level, so only the shape differs).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::detail::Detail;
use crate::moves::{EqKind, MixMove, hz};
use crate::spectrum::OCTAVE_HZ;

/// Octave deviations smaller than this (dB) are not worth an EQ move.
const SHAPE_TOLERANCE_DB: f32 = 3.0;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ReferenceComparison {
	/// Mix minus reference, LU.
	pub lufs_delta: f32,
	pub true_peak_delta_db: f32,
	/// Mix minus reference crest factor: negative = the mix is more compressed.
	pub crest_delta_db: f32,
	/// Mix minus reference stereo width, percentage points.
	pub width_delta_pct: f32,
	/// Mix minus reference per octave band ([`OCTAVE_HZ`]) after matching total levels.
	pub octave_delta_db: Vec<f32>,
	/// Plain findings, biggest first.
	pub findings: Vec<String>,
	/// Master moves that bring the mix closer (EQ on the biggest deviations, loudness).
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub moves: Vec<MixMove>,
}

fn shape(d: &Detail) -> Vec<f32> {
	let total: f64 = d
		.spectrum
		.octave_db
		.iter()
		.map(|v| 10f64.powf(f64::from(*v) / 10.0))
		.sum();
	let total_db = if total > 0.0 {
		(10.0 * total.log10()) as f32
	} else {
		0.0
	};
	d.spectrum
		.octave_db
		.iter()
		.map(|v| (v - total_db).max(-80.0))
		.collect()
}

pub fn compare(mix: &Detail, reference: &Detail) -> ReferenceComparison {
	let (m, r) = (&mix.stats, &reference.stats);
	let octave_delta_db: Vec<f32> = shape(mix)
		.iter()
		.zip(shape(reference))
		.map(|(a, b)| ((a - b) * 10.0).round() / 10.0)
		.collect();
	let lufs_delta = m.lufs_integrated - r.lufs_integrated;
	let crest_delta_db = m.crest_factor_db - r.crest_factor_db;
	let width_delta_pct = (mix.stereo.width - reference.stereo.width) * 100.0;
	let mut findings: Vec<(f32, String)> = Vec::new();
	let mut moves = Vec::new();
	if lufs_delta.abs() > 1.0 {
		findings.push((
			lufs_delta.abs(),
			format!(
				"the mix is {:.1} LU {} than the reference ({:.1} vs {:.1} LUFS)",
				lufs_delta.abs(),
				if lufs_delta > 0.0 {
					"louder"
				} else {
					"quieter"
				},
				m.lufs_integrated,
				r.lufs_integrated
			),
		));
	}
	if crest_delta_db.abs() > 2.0 {
		findings.push((
			crest_delta_db.abs() * 0.8,
			format!(
				"the mix is {} ({:.1} dB crest factor vs {:.1})",
				if crest_delta_db < 0.0 {
					"more compressed than the reference"
				} else {
					"more dynamic than the reference"
				},
				m.crest_factor_db,
				r.crest_factor_db
			),
		));
	}
	if width_delta_pct.abs() > 5.0 {
		findings.push((
			width_delta_pct.abs() / 3.0,
			format!(
				"the mix is {} than the reference (width {:.0}% vs {:.0}%)",
				if width_delta_pct < 0.0 {
					"narrower"
				} else {
					"wider"
				},
				mix.stereo.width * 100.0,
				reference.stereo.width * 100.0
			),
		));
	}
	// The biggest spectral deviations (skipping the extreme octaves, where references
	// differ the most for reasons an EQ should not chase).
	let mut bands: Vec<(usize, f32)> = (1..9)
		.map(|i| (i, octave_delta_db[i]))
		.filter(|(_, d)| d.abs() > SHAPE_TOLERANCE_DB)
		.collect();
	bands.sort_by(|a, b| b.1.abs().total_cmp(&a.1.abs()));
	for &(i, d) in bands.iter().take(3) {
		let freq = OCTAVE_HZ[i];
		findings.push((
			d.abs(),
			format!(
				"{} has {:.1} dB {} than the reference",
				hz(freq),
				d.abs(),
				if d > 0.0 {
					"more energy"
				} else {
					"less energy"
				}
			),
		));
		let kind = match i {
			1 => EqKind::LowShelf,
			8 => EqKind::HighShelf,
			_ => EqKind::Bell,
		};
		let gain = (-d / 2.0).clamp(-4.0, 3.0);
		moves.push(MixMove::Eq {
			track_id: None,
			kind,
			freq_hz: if kind == EqKind::LowShelf {
				100.0
			} else {
				freq
			},
			gain_db: (gain * 2.0).round() / 2.0,
			q: (kind == EqKind::Bell).then_some(1.0),
		});
	}
	findings.sort_by(|a, b| b.0.total_cmp(&a.0));
	ReferenceComparison {
		lufs_delta,
		true_peak_delta_db: m.true_peak_db - r.true_peak_db,
		crest_delta_db,
		width_delta_pct,
		octave_delta_db,
		findings: findings.into_iter().map(|(_, f)| f).collect(),
		moves,
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::detail::analyze;

	fn noise(seed: u32, len: usize) -> Vec<f32> {
		let mut s = seed;
		(0..len)
			.map(|_| {
				s ^= s << 13;
				s ^= s >> 17;
				s ^= s << 5;
				(s as f32 / u32::MAX as f32) * 2.0 - 1.0
			})
			.collect()
	}

	#[test]
	fn a_darker_quieter_mix_than_its_reference() {
		let sr = 48_000;
		let len = sr as usize * 4;
		let reference = noise(1, len);
		let rn = noise(2, len);
		let reference_r: Vec<f32> = reference
			.iter()
			.zip(&rn)
			.map(|(a, b)| a * 0.7 + b * 0.3)
			.collect();
		// The mix: the same noise one-pole low-passed (darker) and 6 dB down.
		let mut y = 0.0f32;
		let mix: Vec<f32> = reference
			.iter()
			.map(|x| {
				y += 0.2 * (x - y);
				y * 0.5
			})
			.collect();
		let mut y = 0.0f32;
		let mix_r: Vec<f32> = reference_r
			.iter()
			.map(|x| {
				y += 0.2 * (x - y);
				y * 0.5
			})
			.collect();
		let c = compare(
			&analyze(sr, &mix, &mix_r),
			&analyze(sr, &reference, &reference_r),
		);
		assert!(c.lufs_delta < -6.0, "{c:?}");
		assert!(c.findings[0].contains("quieter"), "{:?}", c.findings);
		assert!(
			c.octave_delta_db[8] < c.octave_delta_db[2] - 10.0,
			"darker: {c:?}"
		);
		assert!(c.findings.iter().any(|f| f.contains("more energy")));
		assert!(
			c.moves.iter().any(|m| matches!(
				m,
				MixMove::Eq {
					kind: EqKind::LowShelf,
					gain_db,
					..
				} if *gain_db < 0.0
			)),
			"{:?}",
			c.moves
		);
		let same = analyze(sr, &reference, &reference_r);
		let c = compare(&same, &same);
		assert!(c.findings.is_empty() && c.moves.is_empty());
	}
}
