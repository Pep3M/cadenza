//! Cross-track masking: two parts that are loud in the same octave band at the same time,
//! at similar levels, blur each other (kick vs bass at 63 Hz, keys vs vocal at 2 kHz). The
//! report ranks pairs by how much of the time they collide and names which one to cut.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::roles::Role;
use crate::spectrum::{OCTAVE_HZ, THIRD_OCTAVE_HZ};

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
	/// Mean third-octave levels ([`crate::spectrum::Spectrum::third_octave_db`]); only
	/// [`collisions_for`] reads it (the center of a collision), empty is fine.
	pub third_octave_db: &'a [f32],
}

/// Two tracks colliding in one band. `a`/`b` index the inputs; `cut` is the one that should
/// make room (lower ownership of the band for its role, else less of its energy there).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MaskingPair {
	pub a: usize,
	pub b: usize,
	/// Center of the octave band, Hz.
	#[serde(serialize_with = "crate::short::f32")]
	pub band_hz: f32,
	/// Share of the time both play in that band where they collide (0..1).
	#[serde(serialize_with = "crate::short::f32")]
	pub overlap: f32,
	/// Mean level of `a` minus `b` in the band while colliding, dB.
	#[serde(serialize_with = "crate::short::f32")]
	pub level_diff_db: f32,
	/// Ranking score (overlap x how often both play there x band weight).
	#[serde(serialize_with = "crate::short::f32")]
	pub score: f32,
	pub cut: usize,
}

/// Ranks masking pairs (best band per pair), highest score first.
pub fn masking(tracks: &[MaskInput<'_>]) -> Vec<MaskingPair> {
	let frames = tracks.iter().map(|t| t.frames.len()).min().unwrap_or(0);
	if tracks.len() < 2 || frames == 0 {
		return Vec::new();
	}
	let total_db = total_db(tracks, frames);
	let mut pairs = Vec::new();
	for a in 0..tracks.len() {
		for b in a + 1..tracks.len() {
			let mut best: Option<MaskingPair> = None;
			for (band, weight) in BANDS {
				let Some(hit) = band_hit(&tracks[a], &tracks[b], &total_db, band, weight) else {
					continue;
				};
				if best.as_ref().is_none_or(|p| hit.score > p.score) {
					let cut = cut_candidate(&tracks[a], &tracks[b], band, a, b);
					best = Some(MaskingPair {
						a,
						b,
						band_hz: OCTAVE_HZ[band],
						overlap: hit.overlap,
						level_diff_db: hit.level_a - hit.level_b,
						score: hit.score,
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

/// Total level of all inputs per (frame, octave band), dB.
fn total_db(tracks: &[MaskInput<'_>], frames: usize) -> Vec<[f32; 10]> {
	let mut total = vec![[0.0f64; 10]; frames];
	for t in tracks {
		for (sum, row) in total.iter_mut().zip(t.frames) {
			for (s, v) in sum.iter_mut().zip(row) {
				*s += 10f64.powf(f64::from(*v) / 10.0);
			}
		}
	}
	total
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
		.collect()
}

/// How two tracks collide in one octave band.
#[derive(Debug, Clone, Copy)]
struct BandHit {
	overlap: f32,
	score: f32,
	/// Mean levels while colliding, dB.
	level_a: f32,
	level_b: f32,
}

fn band_hit(
	ta: &MaskInput<'_>,
	tb: &MaskInput<'_>,
	total_db: &[[f32; 10]],
	band: usize,
	weight: f32,
) -> Option<BandHit> {
	let (mut both, mut hits, mut sum_a, mut sum_b) = (0u32, 0u32, 0.0f32, 0.0f32);
	for ((fa, fb), total) in ta.frames.iter().zip(tb.frames).zip(total_db) {
		let (la, lb) = (fa[band], fb[band]);
		if la < PRESENT_DB || lb < PRESENT_DB {
			continue;
		}
		both += 1;
		let floor = total[band] - CONTRIBUTOR_DB;
		if la >= floor && lb >= floor && (la - lb).abs() <= CLOSE_DB {
			hits += 1;
			sum_a += la;
			sum_b += lb;
		}
	}
	if hits == 0 {
		return None;
	}
	let overlap = hits as f32 / both as f32;
	let presence = (both as f32 / total_db.len() as f32).sqrt();
	Some(BandHit {
		overlap,
		score: overlap * presence * weight,
		level_a: sum_a / hits as f32,
		level_b: sum_b / hits as f32,
	})
}

/// The bell cut a masking fix proposes: deeper for pairs that collide most of the time.
pub fn cut_gain_db(score: f32) -> f32 {
	if score > 0.5 { -4.0 } else { -3.0 }
}

/// The Q of that bell (about one octave wide).
pub const CUT_Q: f32 = 1.4;

/// One other track colliding with the target in one octave band ([`collisions_for`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Collision {
	/// Index of the other track in the inputs.
	pub other: usize,
	/// Octave band index ([`OCTAVE_HZ`]).
	pub band: usize,
	pub band_hz: f32,
	/// The third-octave inside the band where both are loudest together (`band_hz`
	/// without third-octave levels): where a cut goes.
	pub center_hz: f32,
	pub overlap: f32,
	pub score: f32,
	/// Mean level of the target / the other track in the band while colliding, dBFS.
	pub level_db: f32,
	pub other_level_db: f32,
	/// The masking rule's choice: the target is the one to make room (else the other).
	pub cut_here: bool,
	/// Suggested bell gain ([`cut_gain_db`]).
	pub cut_db: f32,
}

/// Every band where another track collides with `target` (same rule as [`masking`], but
/// per band instead of only each pair's worst one), strongest first; scores below
/// [`MIN_SCORE`] are left out.
pub fn collisions_for(target: usize, tracks: &[MaskInput<'_>]) -> Vec<Collision> {
	let frames = tracks.iter().map(|t| t.frames.len()).min().unwrap_or(0);
	if target >= tracks.len() || tracks.len() < 2 || frames == 0 {
		return Vec::new();
	}
	let total_db = total_db(tracks, frames);
	let t = &tracks[target];
	let mut out = Vec::new();
	for (other, o) in tracks.iter().enumerate() {
		if other == target {
			continue;
		}
		for (band, weight) in BANDS {
			let Some(hit) = band_hit(t, o, &total_db, band, weight) else {
				continue;
			};
			if hit.score < MIN_SCORE {
				continue;
			}
			let cut = cut_candidate(t, o, band, target, other);
			out.push(Collision {
				other,
				band,
				band_hz: OCTAVE_HZ[band],
				center_hz: center_of(t, o, band),
				overlap: hit.overlap,
				score: hit.score,
				level_db: hit.level_a,
				other_level_db: hit.level_b,
				cut_here: cut == target,
				cut_db: cut_gain_db(hit.score),
			});
		}
	}
	out.sort_by(|x, y| y.score.total_cmp(&x.score));
	out
}

/// The third-octave of `band` where the quieter of the two is loudest.
fn center_of(a: &MaskInput<'_>, b: &MaskInput<'_>, band: usize) -> f32 {
	(band * 3 + 1..=band * 3 + 3)
		.filter(|&i| i < THIRD_OCTAVE_HZ.len())
		.filter_map(|i| {
			let la = *a.third_octave_db.get(i)?;
			let lb = *b.third_octave_db.get(i)?;
			Some((i, la.min(lb)))
		})
		.max_by(|x, y| x.1.total_cmp(&y.1))
		.map_or(OCTAVE_HZ[band], |(i, _)| THIRD_OCTAVE_HZ[i])
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
				third_octave_db: &[],
			},
			MaskInput {
				role: Role::Bass,
				frames: &b,
				octave_db: &bass,
				third_octave_db: &[],
			},
			MaskInput {
				role: Role::Drums,
				frames: &h,
				octave_db: &hats,
				third_octave_db: &[],
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
				third_octave_db: &[],
			},
			MaskInput {
				role: Role::Keys,
				frames: &k,
				octave_db: &keys,
				third_octave_db: &[],
			},
			MaskInput {
				role: Role::Pad,
				frames: &q,
				octave_db: &quiet,
				third_octave_db: &[],
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
				third_octave_db: &[],
			},
			MaskInput {
				role: Role::Guitar,
				frames: &b,
				octave_db: &on,
				third_octave_db: &[],
			},
		]);
		assert!(pairs.is_empty());
	}
}
