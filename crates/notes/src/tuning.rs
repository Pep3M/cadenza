//! Which tone each note should be tuned to (NE-7a), learned from an expert's session on a
//! real take (`docs/design/note-editing-expert-session.md`): a singer drifts sharp or flat
//! as a whole, so rounding each note to its nearest semitone moves a fifth of them to the
//! wrong note, and a major/minor key profile cannot tell the modes apart (E Phrygian's F
//! sung sharp reads as E minor's F#). The scale and the singer's reference offset are fitted
//! together, then a few rules for notes the scale alone gets wrong; every target carries
//! its reason and the runner-up, so a person or the AI engineer can review the doubtful.

use cadenza_theory::{PitchClass, Scale, ScaleKind};
use serde::{Deserialize, Serialize};

/// What tuning needs to know of a note (times in seconds, pitches fractional MIDI).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TuningNote {
	pub onset: f64,
	pub offset: f64,
	pub core_start: f64,
	pub core_end: f64,
	pub center: f64,
	/// Where the pitch ends up over the note's last 30 %, when known (a scoop's landing).
	pub landing: Option<f64>,
}

impl TuningNote {
	fn core(&self) -> f64 {
		(self.core_end - self.core_start).max(0.0)
	}
}

impl From<&crate::segment::SegmentedNote> for TuningNote {
	fn from(n: &crate::segment::SegmentedNote) -> Self {
		Self {
			onset: n.onset,
			offset: n.offset,
			core_start: n.core_start,
			core_end: n.core_end,
			center: n.center,
			landing: None,
		}
	}
}

impl From<&cadenza_model::AudioNote> for TuningNote {
	fn from(n: &cadenza_model::AudioNote) -> Self {
		Self {
			onset: n.start,
			offset: n.end,
			core_start: n.core_start,
			core_end: n.core_end,
			center: n.center,
			landing: None,
		}
	}
}

/// A scale and the singer's reference: the take is sung `offset_cents` away from
/// concert pitch (positive = sharp).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ScaleFit {
	pub scale: Scale,
	pub offset_cents: f64,
	/// Core-weighted distance of the notes from the scale tones after the offset.
	pub rms_cents: f64,
}

/// Pitch sets tried, each with its modes.
const SETS: [ScaleKind; 2] = [ScaleKind::Major, ScaleKind::HarmonicMinor];
const DIATONIC_MODES: [(u8, ScaleKind); 7] = [
	(0, ScaleKind::Major),
	(2, ScaleKind::Dorian),
	(4, ScaleKind::Phrygian),
	(5, ScaleKind::Lydian),
	(7, ScaleKind::Mixolydian),
	(9, ScaleKind::NaturalMinor),
	(11, ScaleKind::Locrian),
];
/// Squared cents charged per unit weight at a 50 c offset: the same tones seen half a
/// semitone over fit nearly as well, so the smaller offset wins a close call.
const OFFSET_PRIOR: f64 = 100.0;
/// The harmonic minor has an augmented second the voice rarely needs; it must fit clearly
/// better than a diatonic set to be chosen.
const HARMONIC_PENALTY: f64 = 1.15;

fn pcs(root: u8, kind: ScaleKind) -> [bool; 12] {
	let mut set = [false; 12];
	for i in kind.intervals() {
		set[usize::from((root + i) % 12)] = true;
	}
	set
}

/// Distance in semitones from `p` to the nearest tone of `set`, and that tone.
fn nearest(set: &[bool; 12], p: f64) -> (f64, f64) {
	let base = p.floor();
	let mut best = (f64::INFINITY, base);
	for k in -6..=7 {
		let t = base + f64::from(k);
		if set[(t as i64).rem_euclid(12) as usize] && (t - p).abs() < best.0 {
			best = ((t - p).abs(), t);
		}
	}
	best
}

/// Fewer notes than this say too little about a scale: no fit.
pub const MIN_NOTES: usize = 6;

/// The scales that explain `notes` best, best first (at most `count`). Notes weigh by their
/// stable core. Empty below [`MIN_NOTES`].
pub fn fit_scale(notes: &[TuningNote], count: usize) -> Vec<ScaleFit> {
	if notes.len() < MIN_NOTES {
		return Vec::new();
	}
	let (weighted, total) = weights(notes);
	let mut fits: Vec<(f64, u8, ScaleKind, f64, f64)> = Vec::new();
	for kind in SETS {
		for root in 0..12u8 {
			let best = best_offset(&weighted, total, &pcs(root, kind));
			let penalty = if kind == ScaleKind::HarmonicMinor {
				HARMONIC_PENALTY
			} else {
				1.0
			};
			fits.push((best.0 * penalty, root, kind, best.1, best.2));
		}
	}
	fits.sort_by(|a, b| a.0.total_cmp(&b.0));
	fits.into_iter()
		.take(count)
		.map(|(_, root, kind, offset, rms)| ScaleFit {
			scale: tonal_center(notes, root, kind, offset),
			offset_cents: offset,
			rms_cents: rms,
		})
		.collect()
}

/// (Pitch, core weight) of each note and their total weight.
fn weights(notes: &[TuningNote]) -> (Vec<(f64, f64)>, f64) {
	let weighted: Vec<(f64, f64)> = notes
		.iter()
		.map(|n| (n.center, n.core().max(0.01)))
		.collect();
	let total = weighted.iter().map(|w| w.1).sum();
	(weighted, total)
}

/// (Cost, offset in cents, rms cents) of the offset that fits `set` best.
fn best_offset(weighted: &[(f64, f64)], total: f64, set: &[bool; 12]) -> (f64, f64, f64) {
	let mut best = (f64::INFINITY, 0.0, 0.0);
	for o in -50..=50 {
		let off = f64::from(o) / 100.0;
		let sse: f64 = weighted
			.iter()
			.map(|&(c, w)| w * (nearest(set, c - off).0 * 100.0).powi(2))
			.sum();
		let cost = sse + total * OFFSET_PRIOR * (f64::from(o) / 50.0).powi(2);
		if cost < best.0 {
			best = (cost, f64::from(o), (sse / total.max(1e-9)).sqrt());
		}
	}
	best
}

/// The singer's offset under a scale chosen by someone else (the user, the AI picking an
/// alternative mode). `None` without notes.
pub fn fit_for_scale(notes: &[TuningNote], scale: Scale) -> Option<ScaleFit> {
	if notes.is_empty() {
		return None;
	}
	let (weighted, total) = weights(notes);
	let mut set = [false; 12];
	for (pc, slot) in set.iter_mut().enumerate() {
		*slot = scale.contains(pc as u8);
	}
	let (_, offset_cents, rms_cents) = best_offset(&weighted, total, &set);
	Some(ScaleFit {
		scale,
		offset_cents,
		rms_cents,
	})
}

/// The mode of a pitch set: its tone the notes dwell on most (cores in seconds, the last
/// note counting double: phrases end home).
fn tonal_center(notes: &[TuningNote], root: u8, kind: ScaleKind, offset: f64) -> Scale {
	let set = pcs(root, kind);
	let mut weight = [0.0f64; 12];
	for (i, n) in notes.iter().enumerate() {
		let tone = nearest(&set, n.center - offset / 100.0).1;
		let w = n.core().max(0.01) * if i + 1 == notes.len() { 2.0 } else { 1.0 };
		weight[(tone as i64).rem_euclid(12) as usize] += w;
	}
	if kind != ScaleKind::Major {
		// Harmonic minor: named after its own root (its modes have no common names).
		return Scale::new(PitchClass(root), kind);
	}
	let (degree, mode) = DIATONIC_MODES
		.iter()
		.copied()
		.max_by(|a, b| {
			let w = |d: u8| weight[usize::from((root + d) % 12)];
			w(a.0).total_cmp(&w(b.0))
		})
		.expect("seven modes");
	Scale::new(PitchClass((root + degree) % 12), mode)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetReason {
	/// The scale tone nearest to the note once the singer's offset is taken out.
	InScale,
	/// A long note sung clearly on a tone outside the scale: kept (a blue note, a
	/// chromatic passing tone the singer meant).
	Chromatic,
	/// A short plateau sliding into the next note at nearly its pitch: tuned with it.
	JoinsNext,
	/// A short core inside a big scoop: tuned to where the scoop lands.
	ScoopLanding,
	/// The note a reference melody (MIDI) plays there.
	Reference,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Target {
	/// Whole MIDI note.
	pub pitch: f64,
	pub reason: TargetReason,
	/// The other tone it could be.
	pub runner_up: f64,
	/// Close to halfway between the two: worth a listen.
	pub ambiguous: bool,
}

#[derive(Debug, Clone)]
pub struct TargetOptions {
	/// A note with at least this much core (seconds) within `chromatic_cents` of a tone
	/// outside the scale keeps that tone.
	pub chromatic_core: f64,
	pub chromatic_cents: f64,
	/// A core this short (seconds) running legato into a note less than `join_cents` away
	/// takes that note's target.
	pub join_core: f64,
	pub join_cents: f64,
	/// A core this short (seconds) whose landing is `landing_cents` above it is a scoop.
	pub scoop_core: f64,
	pub landing_cents: f64,
	/// Within this many cents of halfway between two tones a target is ambiguous.
	pub ambiguous_cents: f64,
}

impl Default for TargetOptions {
	fn default() -> Self {
		Self {
			chromatic_core: 0.12,
			chromatic_cents: 25.0,
			join_core: 0.12,
			join_cents: 60.0,
			scoop_core: 0.08,
			landing_cents: 30.0,
			ambiguous_cents: 15.0,
		}
	}
}

/// A target for every note of `notes` (in order) under `fit`.
pub fn targets(notes: &[TuningNote], fit: &ScaleFit, o: &TargetOptions) -> Vec<Target> {
	let set = {
		let mut s = [false; 12];
		for (pc, slot) in s.iter_mut().enumerate() {
			*slot = fit.scale.contains(pc as u8);
		}
		s
	};
	let off = fit.offset_cents / 100.0;
	let in_scale = |p: f64| {
		let (_, tone) = nearest(&set, p);
		// The tone on the other side of `p`.
		let other = {
			let below = (1..=12)
				.map(|k| tone - f64::from(k))
				.find(|t| set[(*t as i64).rem_euclid(12) as usize])
				.unwrap_or(tone - 1.0);
			let above = (1..=12)
				.map(|k| tone + f64::from(k))
				.find(|t| set[(*t as i64).rem_euclid(12) as usize])
				.unwrap_or(tone + 1.0);
			if p >= tone { above } else { below }
		};
		let half = (tone + other) / 2.0;
		Target {
			pitch: tone,
			reason: TargetReason::InScale,
			runner_up: other,
			ambiguous: (p - half).abs() * 100.0 < o.ambiguous_cents,
		}
	};
	let mut out: Vec<Target> = notes
		.iter()
		.map(|n| {
			let p = n.center - off;
			if let Some(land) = n.landing
				&& n.core() < o.scoop_core
				&& (land - n.center) * 100.0 >= o.landing_cents
			{
				let t = in_scale(land - off);
				return Target {
					reason: TargetReason::ScoopLanding,
					runner_up: in_scale(p).pitch,
					..t
				};
			}
			let semitone = p.round();
			let outside = !set[(semitone as i64).rem_euclid(12) as usize];
			if outside
				&& n.core() >= o.chromatic_core
				&& (p - semitone).abs() * 100.0 <= o.chromatic_cents
			{
				return Target {
					pitch: semitone,
					reason: TargetReason::Chromatic,
					runner_up: in_scale(p).pitch,
					ambiguous: false,
				};
			}
			in_scale(p)
		})
		.collect();
	// Joins, right to left so a chain of short plateaus follows its last note.
	for i in (0..notes.len().saturating_sub(1)).rev() {
		let (n, next) = (&notes[i], &notes[i + 1]);
		if n.core() < o.join_core
			&& (next.onset - n.offset).abs() < 1e-3
			&& (next.center - n.center).abs() * 100.0 < o.join_cents
			&& out[i].pitch != out[i + 1].pitch
		{
			out[i] = Target {
				pitch: out[i + 1].pitch,
				reason: TargetReason::JoinsNext,
				runner_up: out[i].pitch,
				ambiguous: false,
			};
		}
	}
	out
}

/// The fitted scale of a segmentation and a target per note (`analysis` gives the scoop
/// landings). `None` without notes.
pub fn auto_targets(
	seg: &crate::segment::Segmentation,
	analysis: Option<&crate::analysis::Analysis>,
) -> Option<(ScaleFit, Vec<Target>)> {
	let notes: Vec<TuningNote> = seg
		.notes
		.iter()
		.map(|n| TuningNote {
			landing: analysis.and_then(|a| landing(a, n.onset, n.offset)),
			..TuningNote::from(n)
		})
		.collect();
	let fit = *fit_scale(&notes, 1).first()?;
	Some((fit, targets(&notes, &fit, &TargetOptions::default())))
}

/// Where the pitch of `n` ends up (median of its last 30 %), for [`TuningNote::landing`].
pub fn landing(analysis: &crate::analysis::Analysis, onset: f64, offset: f64) -> Option<f64> {
	let from = offset - 0.3 * (offset - onset);
	let mut p: Vec<f64> = analysis
		.frames
		.iter()
		.filter(|f| f.time >= from && f.time <= offset)
		.filter_map(|f| f.pitch)
		.collect();
	if p.is_empty() {
		return None;
	}
	p.sort_by(f64::total_cmp);
	Some(p[p.len() / 2])
}

#[cfg(test)]
mod tests {
	use super::*;

	fn note(onset: f64, offset: f64, center: f64) -> TuningNote {
		TuningNote {
			onset,
			offset,
			core_start: onset + 0.02,
			core_end: offset - 0.02,
			center,
			landing: None,
		}
	}

	#[test]
	fn a_sharp_singer_is_tuned_down_to_the_scale_not_to_the_nearest_semitone() {
		// C major sung 40 c sharp: E F G A G F E, the F at 65.45 rounds to F#.
		let pitches = [64.4, 65.45, 67.35, 69.4, 67.42, 65.38, 64.41, 60.4];
		let notes: Vec<TuningNote> = pitches
			.iter()
			.enumerate()
			.map(|(i, &p)| note(i as f64 * 0.5, i as f64 * 0.5 + 0.45, p))
			.collect();
		let fit = fit_scale(&notes, 3)[0];
		assert!((fit.offset_cents - 40.0).abs() <= 5.0, "{fit:?}");
		let t = targets(&notes, &fit, &TargetOptions::default());
		let got: Vec<f64> = t.iter().map(|t| t.pitch).collect();
		assert_eq!(got, [64.0, 65.0, 67.0, 69.0, 67.0, 65.0, 64.0, 60.0]);
		// Ends on C, dwells on E-F-G: the set is C major's, the mode follows the notes.
		assert!(fit.scale.contains(65) && !fit.scale.contains(66), "{fit:?}");
	}

	#[test]
	fn a_chosen_scale_gets_its_own_offset() {
		let pitches = [64.4, 65.45, 67.35, 69.4, 67.42, 65.38, 64.41, 60.4];
		let notes: Vec<TuningNote> = pitches
			.iter()
			.enumerate()
			.map(|(i, &p)| note(i as f64 * 0.5, i as f64 * 0.5 + 0.45, p))
			.collect();
		let a_minor = Scale::new(PitchClass(9), ScaleKind::NaturalMinor);
		let fit = fit_for_scale(&notes, a_minor).unwrap();
		assert_eq!(fit.scale, a_minor);
		assert!((fit.offset_cents - 40.0).abs() <= 5.0, "{fit:?}");
		// E major has G#: the G sung 35 c sharp sits between G and G#, a worse fit.
		let e_major = fit_for_scale(&notes, Scale::new(PitchClass(4), ScaleKind::Major)).unwrap();
		assert!(e_major.rms_cents > fit.rms_cents + 10.0, "{e_major:?}");
		assert!(fit_for_scale(&[], a_minor).is_none());
	}

	#[test]
	fn chromatic_notes_short_joins_and_scoops_have_their_reasons() {
		let fit = ScaleFit {
			scale: Scale::new(PitchClass(0), ScaleKind::Major),
			offset_cents: 0.0,
			rms_cents: 0.0,
		};
		let mut notes = vec![
			// A long Eb sung on pitch: kept.
			note(0.0, 0.4, 62.95),
			// A short flat plateau sliding into the next C.
			TuningNote {
				core_end: 0.48,
				..note(0.4, 0.5, 59.45)
			},
			note(0.5, 1.0, 59.95),
			// A short core inside a scoop landing on E.
			TuningNote {
				core_start: 1.1,
				core_end: 1.15,
				landing: Some(64.0),
				..note(1.05, 1.4, 62.8)
			},
		];
		let t = targets(&notes, &fit, &TargetOptions::default());
		assert_eq!(
			(t[0].pitch, t[0].reason, t[0].runner_up),
			(63.0, TargetReason::Chromatic, 62.0)
		);
		assert_eq!((t[1].pitch, t[1].reason), (60.0, TargetReason::JoinsNext));
		assert_eq!(
			(t[3].pitch, t[3].reason),
			(64.0, TargetReason::ScoopLanding)
		);
		// Halfway between E and F: ambiguous.
		notes[2].center = 64.5;
		assert!(targets(&notes, &fit, &TargetOptions::default())[2].ambiguous);
	}
}
