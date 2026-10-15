//! Turning a `Chord` into concrete MIDI pitches.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::chord::Chord;

/// How to spread a chord's tones across pitches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum VoicingStyle {
	/// Root position, tones stacked as close together as possible.
	Close,
	/// Close voicing with alternating tones pushed up an octave.
	Open,
	/// Close voicing with the 2nd-from-top tone dropped an octave.
	Drop2,
	/// Close voicing with the 3rd-from-top tone dropped an octave.
	Drop3,
	/// Root, 3rd and 7th (or 5th if there is no 7th) only.
	Shell,
	/// Every tone except the root (classic jazz comping voicing).
	Rootless,
}

/// Unique pitch classes (0..=11, relative to the root) implied by a chord's
/// intervals, including extensions reduced to their base octave.
fn pitch_class_pool(chord: &Chord) -> Vec<u8> {
	let mut pool: Vec<u8> = chord.intervals.iter().map(|&iv| iv % 12).collect();
	pool.sort_unstable();
	pool.dedup();
	pool
}

fn active_pool(chord: &Chord, style: VoicingStyle) -> Vec<u8> {
	let full = pitch_class_pool(chord);
	match style {
		VoicingStyle::Shell => {
			let mut selected = vec![0u8];
			if let Some(&third) = full.iter().find(|&&c| c == 3 || c == 4) {
				selected.push(third);
			}
			if let Some(&seventh) = full.iter().find(|&&c| matches!(c, 9..=11)) {
				selected.push(seventh);
			} else if let Some(&fifth) = full.iter().find(|&&c| matches!(c, 6..=8)) {
				selected.push(fifth);
			}
			selected.sort_unstable();
			selected.dedup();
			selected
		}
		VoicingStyle::Rootless => {
			let rest: Vec<u8> = full.iter().copied().filter(|&c| c != 0).collect();
			if rest.is_empty() { full } else { rest }
		}
		_ => full,
	}
}

fn next_pitch_with_class(after: i32, target_class: i32) -> i32 {
	let target = target_class.rem_euclid(12);
	let mut candidate = after + 1;
	while candidate.rem_euclid(12) != target {
		candidate += 1;
	}
	candidate
}

/// Builds an ascending close-position stack of `pool`'s tones (read starting
/// at `rotation`, wrapping), anchored as close as possible to `center`.
fn build_stack(root: u8, pool: &[u8], rotation: usize, center: u8) -> Vec<i32> {
	let n = pool.len();
	let rotated: Vec<u8> = (0..n).map(|k| pool[(rotation + k) % n]).collect();

	let first_class = (root as i32 + rotated[0] as i32).rem_euclid(12);
	let octave_shift = ((center as i32 - first_class) as f64 / 12.0).round() as i32;
	let start = first_class + octave_shift * 12;

	let mut pitches = vec![start];
	let mut prev = start;
	for &v in &rotated[1..] {
		let target_class = (root as i32 + v as i32).rem_euclid(12);
		let next = next_pitch_with_class(prev, target_class);
		pitches.push(next);
		prev = next;
	}
	pitches
}

fn apply_style_transform(style: VoicingStyle, stack: &[i32]) -> Vec<i32> {
	let mut v = stack.to_vec();
	match style {
		VoicingStyle::Open => {
			for p in v.iter_mut().skip(1).step_by(2) {
				*p += 12;
			}
			v.sort_unstable();
		}
		VoicingStyle::Drop2 => {
			if v.len() >= 2 {
				let idx = v.len() - 2;
				v[idx] -= 12;
			}
			v.sort_unstable();
		}
		VoicingStyle::Drop3 => {
			if v.len() >= 3 {
				let idx = v.len() - 3;
				v[idx] -= 12;
			} else if v.len() == 2 {
				v[0] -= 12;
			}
			v.sort_unstable();
		}
		VoicingStyle::Close | VoicingStyle::Shell | VoicingStyle::Rootless => {}
	}
	v
}

/// Total absolute pitch movement pairing `prev` against `candidate`
/// (both sorted ascending; the shorter list's edges are reused for any
/// extra notes on the other side).
fn voice_leading_cost(prev: &[u8], candidate: &[i32]) -> i64 {
	let mut prev_sorted: Vec<i32> = prev.iter().map(|&p| p as i32).collect();
	prev_sorted.sort_unstable();
	let mut cand_sorted = candidate.to_vec();
	cand_sorted.sort_unstable();

	if prev_sorted.is_empty() || cand_sorted.is_empty() {
		return 0;
	}
	let n = prev_sorted.len().max(cand_sorted.len());
	let mut total = 0i64;
	for i in 0..n {
		let p = *prev_sorted
			.get(i)
			.unwrap_or_else(|| prev_sorted.last().unwrap());
		let c = *cand_sorted
			.get(i)
			.unwrap_or_else(|| cand_sorted.last().unwrap());
		total += (p - c).unsigned_abs() as i64;
	}
	total
}

/// Voices `chord` using `style`, centered on `center` (a MIDI pitch). When
/// `previous` is given, picks the inversion/octave placement that minimizes
/// total voice movement from it; otherwise picks the placement closest to
/// `center`. Returns pitches sorted ascending.
pub fn voice(chord: &Chord, style: VoicingStyle, center: u8, previous: Option<&[u8]>) -> Vec<u8> {
	let pool = active_pool(chord, style);
	if pool.is_empty() {
		return Vec::new();
	}
	let root = chord.root.0;
	let n = pool.len();

	let mut best: Option<Vec<i32>> = None;
	let mut best_cost = i64::MAX;

	for rotation in 0..n {
		let stack = build_stack(root, &pool, rotation, center);
		let styled = apply_style_transform(style, &stack);
		let cost = match previous {
			Some(prev) if !prev.is_empty() => voice_leading_cost(prev, &styled),
			_ => styled
				.iter()
				.map(|&p| (p - center as i32).unsigned_abs() as i64)
				.sum(),
		};
		if cost < best_cost {
			best_cost = cost;
			best = Some(styled);
		}
	}

	let mut result: Vec<u8> = best
		.unwrap_or_default()
		.into_iter()
		.map(|p| p.clamp(0, 127) as u8)
		.collect();
	result.sort_unstable();
	result
}

/// The bass pitch for `chord` at the given octave, respecting a slash bass
/// if present.
pub fn bass_note(chord: &Chord, octave: i32) -> u8 {
	let pc = chord.bass.unwrap_or(chord.root);
	let midi = (octave + 1) * 12 + pc.0 as i32;
	midi.clamp(0, 127) as u8
}
