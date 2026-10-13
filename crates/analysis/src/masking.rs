//! Cross-track masking: two parts that are loud in the same octave band at the same time,
//! at similar levels, blur each other (kick vs bass at 63 Hz, keys vs vocal at 2 kHz). The
//! report ranks pairs by how much of the time they collide and names which one to cut.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::roles::Role;
use crate::spectrum::OCTAVE_HZ;

/// Octave bands checked (63 Hz .. 8 kHz) and how much a collision there matters.
const BANDS: [(usize, f32); 8] = [
	(1, 1.0),
	(2, 1.0),
	(3, 0.9),
	(4, 0.7),
	(5, 0.7),
	(6, 0.9),
	(7, 0.8),
	(8, 0.5),
];

/// A band counts as "played" by a track above this level (dBFS).
const PRESENT_DB: f32 = -70.0;
/// Both tracks must be within this of the band's total to matter there.
const CONTRIBUTOR_DB: f32 = 10.0;
/// ... and within this of each other to mask each other.
const CLOSE_DB: f32 = 6.0;
/// Pairs scoring below this are not reported.
pub const MIN_SCORE: f32 = 0.15;

/// One track's input to the masking report.
#[derive(Debug, Clone, Copy)]
pub struct MaskInput<'a> {
	pub role: Role,
	/// Octave-band levels per hop ([`crate::spectrum::Timeline::frames`]).
	pub frames: &'a [[f32; 10]],
	/// Mean octave levels ([`crate::spectrum::Spectrum::octave_db`]).
	pub octave_db: &'a [f32],
}

/// Two tracks colliding in one band. `a`/`b` index the inputs; `cut` is the one that should
/// make room (lower ownership of the band for its role, else less of its energy there).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MaskingPair {
	pub a: usize,
	pub b: usize,
	/// Center of the octave band, Hz.
	pub band_hz: f32,
	/// Share of the time both play in that band where they collide (0..1).
	pub overlap: f32,
	/// Mean level of `a` minus `b` in the band while colliding, dB.
	pub level_diff_db: f32,
	/// Ranking score (overlap x how often both play there x band weight).
	pub score: f32,
	pub cut: usize,
}

/// Ranks masking pairs (best band per pair), highest score first.
pub fn masking(tracks: &[MaskInput<'_>]) -> Vec<MaskingPair> {
	let frames = tracks.iter().map(|t| t.frames.len()).min().unwrap_or(0);
	if tracks.len() < 2 || frames == 0 {
		return Vec::new();
	}
	// Total power of all inputs per (frame, band).
	let mut total = vec![[0.0f64; 10]; frames];
	for t in tracks {
		for (sum, row) in total.iter_mut().zip(t.frames) {
			for (s, v) in sum.iter_mut().zip(row) {
				*s += 10f64.powf(f64::from(*v) / 10.0);
			}
		}
	}
	let total_db: Vec<[f32; 10]> = total
		.iter()
		.map(|row| {
			row.map(|p| {
				if p > 0.0 {
					(10.0 * p.log10()) as f32
				} else {
					-144.0
				}
			})
		})
		.collect();
	let mut pairs = Vec::new();
	for a in 0..tracks.len() {
		for b in a + 1..tracks.len() {
			let mut best: Option<MaskingPair> = None;
			for (band, weight) in BANDS {
				let (mut both, mut hits, mut diff) = (0u32, 0u32, 0.0f32);
				for ((fa, fb), total) in
					tracks[a].frames.iter().zip(tracks[b].frames).zip(&total_db)
				{
					let (la, lb) = (fa[band], fb[band]);
					if la < PRESENT_DB || lb < PRESENT_DB {
						continue;
					}
					both += 1;
					let floor = total[band] - CONTRIBUTOR_DB;
					if la >= floor && lb >= floor && (la - lb).abs() <= CLOSE_DB {
						hits += 1;
						diff += la - lb;
					}
				}
				if hits == 0 {
					continue;
				}
				let overlap = hits as f32 / both as f32;
				let presence = (both as f32 / frames as f32).sqrt();
				let score = overlap * presence * weight;
				if best.as_ref().is_none_or(|p| score > p.score) {
					let cut = cut_candidate(&tracks[a], &tracks[b], band, a, b);
					best = Some(MaskingPair {
						a,
						b,
						band_hz: OCTAVE_HZ[band],
						overlap,
						level_diff_db: diff / hits as f32,
						score,
						cut,
					});
				}
			}
			if let Some(p) = best.filter(|p| p.score >= MIN_SCORE) {
				pairs.push(p);
			}
		}
	}
	pairs.sort_by(|x, y| y.score.total_cmp(&x.score));
	pairs
}

fn cut_candidate(ta: &MaskInput<'_>, tb: &MaskInput<'_>, band: usize, a: usize, b: usize) -> usize {
	let (oa, ob) = (ta.role.ownership(band), tb.role.ownership(band));
	if (oa - ob).abs() > 0.05 {
		return if oa < ob { a } else { b };
	}
	// Same claim: cut the one for which this band matters least (relative to its peak).
	let share = |t: &MaskInput<'_>| {
		let peak = t.octave_db.iter().copied().fold(f32::MIN, f32::max);
		t.octave_db.get(band).copied().unwrap_or(-144.0) - peak
	};
	if share(ta) <= share(tb) { a } else { b }
}

#[cfg(test)]
mod tests {
	use super::*;

	fn rows(levels: [f32; 10], n: usize) -> Vec<[f32; 10]> {
		vec![levels; n]
	}

	#[test]
	fn kick_and_bass_collide_in_the_lows_and_the_bass_makes_room_for_nothing() {
		let silent = -144.0;
		let mut kick = [silent; 10];
		kick[1] = -12.0;
		kick[2] = -18.0;
		let mut bass = [silent; 10];
		bass[1] = -14.0;
		bass[2] = -10.0;
		bass[3] = -16.0;
		let mut hats = [silent; 10];
		hats[7] = -20.0;
		hats[8] = -18.0;
		let (k, b, h) = (rows(kick, 100), rows(bass, 100), rows(hats, 100));
		let inputs = [
			MaskInput {
				role: Role::Kick,
				frames: &k,
				octave_db: &kick,
			},
			MaskInput {
				role: Role::Bass,
				frames: &b,
				octave_db: &bass,
			},
			MaskInput {
				role: Role::Drums,
				frames: &h,
				octave_db: &hats,
			},
		];
		let pairs = masking(&inputs);
		assert_eq!(pairs.len(), 1, "hats share no band: {pairs:?}");
		let p = &pairs[0];
		assert_eq!((p.a, p.b), (0, 1));
		assert_eq!(p.band_hz, 63.0, "63 Hz collides all the time, weighted 1.0");
		assert!((p.overlap - 1.0).abs() < 1e-6);
		assert!((p.level_diff_db - 2.0).abs() < 1e-4);
		assert_eq!(p.cut, 1, "the kick owns 63 Hz more than the bass");
	}

	#[test]
	fn keys_make_room_for_the_vocal_and_far_apart_levels_do_not_mask() {
		let silent = -144.0;
		let mut vox = [silent; 10];
		vox[6] = -15.0;
		let mut keys = [silent; 10];
		keys[6] = -17.0;
		let mut quiet = [silent; 10];
		quiet[6] = -40.0;
		let (v, k, q) = (rows(vox, 50), rows(keys, 50), rows(quiet, 50));
		let pairs = masking(&[
			MaskInput {
				role: Role::Vocal,
				frames: &v,
				octave_db: &vox,
			},
			MaskInput {
				role: Role::Keys,
				frames: &k,
				octave_db: &keys,
			},
			MaskInput {
				role: Role::Pad,
				frames: &q,
				octave_db: &quiet,
			},
		]);
		assert_eq!(pairs.len(), 1, "{pairs:?}");
		assert_eq!(pairs[0].band_hz, 2_000.0);
		assert_eq!(pairs[0].cut, 1);
	}

	#[test]
	fn parts_that_alternate_do_not_mask() {
		let silent = -144.0;
		let mut on = [silent; 10];
		on[3] = -12.0;
		let off = [silent; 10];
		let a: Vec<_> = (0..40).map(|i| if i % 2 == 0 { on } else { off }).collect();
		let b: Vec<_> = (0..40).map(|i| if i % 2 == 1 { on } else { off }).collect();
		let pairs = masking(&[
			MaskInput {
				role: Role::Keys,
				frames: &a,
				octave_db: &on,
			},
			MaskInput {
				role: Role::Guitar,
				frames: &b,
				octave_db: &on,
			},
		]);
		assert!(pairs.is_empty());
	}
}
