//! Note-level scores.
//!
//! - COn / COnP / COnPOff: mir_eval note transcription rules (onset within 50 ms, center
//!   within 50 cents, offset within max(50 ms, 20 % of the true length)), one-to-one
//!   maximum matching.
//! - Center error: for each true note, the detected region covering at least half of its
//!   stable core; the distance between that region's center and the sung center, in cents.
//!   This is what a listener hears once the region is snapped to the scale.
//! - Mixed regions: a detected region that significantly overlaps the cores of two true
//!   notes of different pitch. Its center is then somewhere between them and tuning it
//!   cannot work (the Melodyne failure this roadmap is about).
//! - Splits / merges / octave errors / voicing overlap.

use serde::Serialize;

use super::TruthNote;
use crate::Note;

pub const ONSET_TOLERANCE: f64 = 0.05;
pub const PITCH_TOLERANCE_CENTS: f64 = 50.0;
pub const OFFSET_RATIO: f64 = 0.2;
pub const OFFSET_MIN_TOLERANCE: f64 = 0.05;

/// Raw tallies of one case; [`Counts::pooled`] adds cases up.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Counts {
	pub truth: usize,
	pub detected: usize,
	pub matched_on: usize,
	pub matched_on_pitch: usize,
	pub matched_on_pitch_off: usize,
	/// Cents, one per true note covered by a detected region.
	pub center_errors: Vec<f64>,
	pub uncovered: usize,
	pub mixed: usize,
	pub splits: usize,
	pub merges: usize,
	pub octave: usize,
	pub detected_time: f64,
	pub truth_time: f64,
	pub overlap_time: f64,
}

impl Counts {
	pub fn pooled(all: &[Counts]) -> Counts {
		let mut total = Counts::default();
		for c in all {
			total.truth += c.truth;
			total.detected += c.detected;
			total.matched_on += c.matched_on;
			total.matched_on_pitch += c.matched_on_pitch;
			total.matched_on_pitch_off += c.matched_on_pitch_off;
			total.center_errors.extend_from_slice(&c.center_errors);
			total.uncovered += c.uncovered;
			total.mixed += c.mixed;
			total.splits += c.splits;
			total.merges += c.merges;
			total.octave += c.octave;
			total.detected_time += c.detected_time;
			total.truth_time += c.truth_time;
			total.overlap_time += c.overlap_time;
		}
		total
	}
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Prf {
	pub precision: f64,
	pub recall: f64,
	pub f: f64,
}

impl Prf {
	fn new(matched: usize, detected: usize, truth: usize) -> Self {
		let precision = ratio(matched, detected);
		let recall = ratio(matched, truth);
		let f = if precision + recall > 0.0 {
			2.0 * precision * recall / (precision + recall)
		} else {
			0.0
		};
		Self {
			precision,
			recall,
			f,
		}
	}
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct CenterStats {
	/// True notes with a covering region.
	pub covered: usize,
	pub median_cents: f64,
	pub p95_cents: f64,
	pub max_cents: f64,
	/// Share of covered notes more than 10 cents off.
	pub over_10: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Score {
	pub truth: usize,
	pub detected: usize,
	pub con: Prf,
	pub conp: Prf,
	pub conpoff: Prf,
	pub center: CenterStats,
	/// Mixed regions / detected regions.
	pub mixed_rate: f64,
	/// True notes cut into several regions / true notes.
	pub split_rate: f64,
	/// Regions holding several true notes / detected regions.
	pub merge_rate: f64,
	/// Octave errors / covered notes.
	pub octave_rate: f64,
	pub voicing_precision: f64,
	pub voicing_recall: f64,
}

impl Score {
	pub fn from_counts(c: &Counts) -> Self {
		let mut errors = c.center_errors.clone();
		errors.sort_by(f64::total_cmp);
		let center = CenterStats {
			covered: errors.len(),
			median_cents: percentile(&errors, 0.5),
			p95_cents: percentile(&errors, 0.95),
			max_cents: errors.last().copied().unwrap_or(0.0),
			over_10: ratio(errors.iter().filter(|&&e| e > 10.0).count(), errors.len()),
		};
		Self {
			truth: c.truth,
			detected: c.detected,
			con: Prf::new(c.matched_on, c.detected, c.truth),
			conp: Prf::new(c.matched_on_pitch, c.detected, c.truth),
			conpoff: Prf::new(c.matched_on_pitch_off, c.detected, c.truth),
			center,
			mixed_rate: ratio(c.mixed, c.detected),
			split_rate: ratio(c.splits, c.truth),
			merge_rate: ratio(c.merges, c.detected),
			octave_rate: ratio(c.octave, errors.len()),
			voicing_precision: if c.detected_time > 0.0 {
				c.overlap_time / c.detected_time
			} else {
				0.0
			},
			voicing_recall: if c.truth_time > 0.0 {
				c.overlap_time / c.truth_time
			} else {
				0.0
			},
		}
	}
}

/// Scores one case.
pub fn score(truth: &[TruthNote], detected: &[Note]) -> Score {
	Score::from_counts(&count(truth, detected))
}

/// Tallies one case.
pub fn count(truth: &[TruthNote], detected: &[Note]) -> Counts {
	let onset_ok = |t: &TruthNote, d: &Note| (t.onset - d.onset).abs() <= ONSET_TOLERANCE;
	let pitch_ok =
		|t: &TruthNote, d: &Note| (t.center - d.center).abs() * 100.0 <= PITCH_TOLERANCE_CENTS;
	let offset_ok = |t: &TruthNote, d: &Note| {
		let tolerance = (OFFSET_RATIO * (t.offset - t.onset)).max(OFFSET_MIN_TOLERANCE);
		(t.offset - d.offset).abs() <= tolerance
	};
	let matched_on = max_matching(truth, detected, |t, d| onset_ok(t, d));
	let matched_on_pitch = max_matching(truth, detected, |t, d| onset_ok(t, d) && pitch_ok(t, d));
	let matched_on_pitch_off = max_matching(truth, detected, |t, d| {
		onset_ok(t, d) && pitch_ok(t, d) && offset_ok(t, d)
	});

	let mut c = Counts {
		truth: truth.len(),
		detected: detected.len(),
		matched_on,
		matched_on_pitch,
		matched_on_pitch_off,
		..Counts::default()
	};
	for t in truth {
		let core = t.core_length();
		let best = detected
			.iter()
			.map(|d| (overlap(t.core_start, t.core_end, d.onset, d.offset), d))
			.max_by(|a, b| a.0.total_cmp(&b.0));
		match best {
			Some((ov, d)) if core > 0.0 && ov >= 0.5 * core => {
				let error = (d.center - t.center).abs() * 100.0;
				let octaves = (error / 1200.0).round();
				if octaves >= 1.0 && (error - octaves * 1200.0).abs() <= PITCH_TOLERANCE_CENTS {
					c.octave += 1;
				}
				c.center_errors.push(error);
			}
			_ => c.uncovered += 1,
		}
		let pieces = detected
			.iter()
			.filter(|d| significant(overlap(t.core_start, t.core_end, d.onset, d.offset), core))
			.count();
		if pieces >= 2 {
			c.splits += 1;
		}
	}
	for d in detected {
		let held: Vec<&TruthNote> = truth
			.iter()
			.filter(|t| {
				significant(
					overlap(t.core_start, t.core_end, d.onset, d.offset),
					t.core_length(),
				)
			})
			.collect();
		if held.len() >= 2 {
			c.merges += 1;
			let pitches_differ = held.iter().any(|a| {
				held.iter()
					.any(|b| (a.center - b.center).abs() * 100.0 > PITCH_TOLERANCE_CENTS)
			});
			if pitches_differ {
				c.mixed += 1;
			}
		}
	}
	let detected_spans = union(detected.iter().map(|d| (d.onset, d.offset)));
	let truth_spans = union(truth.iter().map(|t| (t.onset, t.offset)));
	c.detected_time = detected_spans.iter().map(|s| s.1 - s.0).sum();
	c.truth_time = truth_spans.iter().map(|s| s.1 - s.0).sum();
	c.overlap_time = intersection(&detected_spans, &truth_spans);
	c
}

/// An overlap long enough to say the region holds part of that core: 30 ms or 30 % of
/// the core, whichever is smaller.
fn significant(overlap: f64, core: f64) -> bool {
	core > 0.0 && overlap > 0.0 && overlap >= (0.03f64).min(0.3 * core)
}

fn overlap(a0: f64, a1: f64, b0: f64, b1: f64) -> f64 {
	(a1.min(b1) - a0.max(b0)).max(0.0)
}

fn ratio(n: usize, d: usize) -> f64 {
	if d == 0 { 0.0 } else { n as f64 / d as f64 }
}

fn percentile(sorted: &[f64], q: f64) -> f64 {
	if sorted.is_empty() {
		return 0.0;
	}
	let pos = q * (sorted.len() - 1) as f64;
	let (lo, hi) = (pos.floor() as usize, pos.ceil() as usize);
	sorted[lo] + (sorted[hi] - sorted[lo]) * (pos - lo as f64)
}

/// Maximum bipartite matching (Kuhn's augmenting paths; cases have tens of notes).
fn max_matching(
	truth: &[TruthNote],
	detected: &[Note],
	edge: impl Fn(&TruthNote, &Note) -> bool,
) -> usize {
	let adjacency: Vec<Vec<usize>> = truth
		.iter()
		.map(|t| {
			(0..detected.len())
				.filter(|&j| edge(t, &detected[j]))
				.collect()
		})
		.collect();
	let mut owner: Vec<Option<usize>> = vec![None; detected.len()];
	fn augment(
		i: usize,
		adjacency: &[Vec<usize>],
		owner: &mut [Option<usize>],
		seen: &mut [bool],
	) -> bool {
		for &j in &adjacency[i] {
			if seen[j] {
				continue;
			}
			seen[j] = true;
			if owner[j].is_none_or(|k| augment(k, adjacency, owner, seen)) {
				owner[j] = Some(i);
				return true;
			}
		}
		false
	}
	let mut matched = 0;
	for i in 0..truth.len() {
		let mut seen = vec![false; detected.len()];
		if augment(i, &adjacency, &mut owner, &mut seen) {
			matched += 1;
		}
	}
	matched
}

fn union(spans: impl Iterator<Item = (f64, f64)>) -> Vec<(f64, f64)> {
	let mut spans: Vec<(f64, f64)> = spans.filter(|s| s.1 > s.0).collect();
	spans.sort_by(|a, b| a.0.total_cmp(&b.0));
	let mut out: Vec<(f64, f64)> = Vec::with_capacity(spans.len());
	for s in spans {
		match out.last_mut() {
			Some(last) if s.0 <= last.1 => last.1 = last.1.max(s.1),
			_ => out.push(s),
		}
	}
	out
}

fn intersection(a: &[(f64, f64)], b: &[(f64, f64)]) -> f64 {
	let (mut i, mut j, mut total) = (0, 0, 0.0);
	while i < a.len() && j < b.len() {
		total += overlap(a[i].0, a[i].1, b[j].0, b[j].1);
		if a[i].1 < b[j].1 {
			i += 1;
		} else {
			j += 1;
		}
	}
	total
}

#[cfg(test)]
mod tests {
	use super::*;

	fn truth(onset: f64, offset: f64, center: f64, glide: f64) -> TruthNote {
		TruthNote {
			onset,
			offset,
			nominal: center.round(),
			center,
			core_start: onset + glide,
			core_end: offset - glide,
		}
	}

	fn note(onset: f64, offset: f64, center: f64) -> Note {
		Note {
			onset,
			offset,
			center,
		}
	}

	#[test]
	fn perfect_detection_scores_one_and_zero_error() {
		let t = [truth(0.0, 0.5, 60.1, 0.05), truth(0.5, 1.0, 62.0, 0.05)];
		let d: Vec<Note> = t
			.iter()
			.map(|t| note(t.onset, t.offset, t.center))
			.collect();
		let s = score(&t, &d);
		assert_eq!(s.conpoff.f, 1.0);
		assert_eq!(s.center.max_cents, 0.0);
		assert_eq!(s.mixed_rate, 0.0);
		assert_eq!(s.voicing_precision, 1.0);
		assert_eq!(s.voicing_recall, 1.0);
	}

	#[test]
	fn one_region_over_two_pitches_is_mixed_and_off_center() {
		let t = [truth(0.0, 0.5, 60.0, 0.05), truth(0.5, 1.0, 62.0, 0.05)];
		let d = [note(0.0, 1.0, 61.0)];
		let s = score(&t, &d);
		assert_eq!(s.mixed_rate, 1.0);
		assert_eq!(s.merge_rate, 1.0);
		assert_eq!(s.conp.recall, 0.0);
		// It covers both cores, so both notes are 100 cents off.
		assert_eq!(s.center.covered, 2);
		assert!((s.center.median_cents - 100.0).abs() < 1e-9);
	}

	#[test]
	fn repeated_notes_merged_are_merges_but_not_mixed() {
		let t = [truth(0.0, 0.4, 60.0, 0.02), truth(0.45, 0.9, 60.05, 0.02)];
		let d = [note(0.0, 0.9, 60.02)];
		let s = score(&t, &d);
		assert_eq!(s.merge_rate, 1.0);
		assert_eq!(s.mixed_rate, 0.0);
	}

	#[test]
	fn splits_and_octave_errors_are_counted() {
		let t = [truth(0.0, 1.0, 57.0, 0.05)];
		let d = [note(0.0, 0.6, 69.0), note(0.6, 1.0, 57.0)];
		let s = score(&t, &d);
		assert_eq!(s.split_rate, 1.0);
		// The longer piece covers most of the core and is an octave off.
		assert_eq!(s.octave_rate, 1.0);
	}

	#[test]
	fn matching_is_one_to_one_and_maximal() {
		// Two detections near both onsets: a greedy match could take the wrong pair.
		let t = [truth(0.0, 0.2, 60.0, 0.02), truth(0.04, 0.3, 64.0, 0.02)];
		let d = [note(0.03, 0.25, 60.0), note(0.0, 0.2, 60.0)];
		assert_eq!(count(&t, &d).matched_on, 2);
		assert_eq!(count(&t, &d).matched_on_pitch, 1);
	}
}
