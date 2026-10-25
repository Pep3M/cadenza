//! Note edits -> the curves the resynthesis follows (NE-4, research §6).
//!
//! Inside a note's core the sung pitch is `center + drift(t) + mod(t)`: `drift` is the slow
//! part of the deviation from the center (a 0.2 s double box, which removes vibrato), `mod`
//! the rest (vibrato, trills, wobble). An edit asks for
//! `target + a_drift · drift(t) + a_mod · mod(t)`, so the pitch shift of a core frame is
//! `(target − center) + (a_drift − 1) · drift(t) + (a_mod − 1) · mod(t)` semitones.
//! Transitions keep their shape: between two connected notes the shift moves smoothly from
//! the end of one core to the start of the next, so a glide still joins the (new) notes.
//! Attacks, releases and the voiced ring right after a note take the shift of their core.

use cadenza_theory::Scale;
use serde::{Deserialize, Serialize};

use crate::NotesError;
use crate::analysis::Analysis;
use crate::segment::{Segmentation, SegmentedNote};
use crate::timemap::TimeMap;

/// Largest pitch move PSOLA is asked for, semitones.
pub const MAX_SHIFT: f64 = 24.0;
/// Largest formant move, cents.
pub const MAX_FORMANT: f64 = 1200.0;
/// Longest attack kept at its own speed when a note is moved or stretched, seconds.
pub const ATTACK: f64 = 0.08;
/// Width of each pass of the drift filter, seconds.
const DRIFT_WINDOW: f64 = 0.2;

/// What to change on one note. The default changes nothing.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct NoteEdit {
	/// New pitch center, fractional MIDI; `None` keeps the sung one.
	pub pitch: Option<f64>,
	/// Share of the drift kept: 1 = as sung, 0 = straightened, -1..2.
	pub drift: f64,
	/// Share of the modulation (vibrato) kept: 1 = as sung, 0 = flat, -1..2.
	pub modulation: f64,
	/// Formant move, cents (the voice's colour, independent of the pitch).
	pub formant_cents: f64,
	pub gain_db: f64,
	/// New place of the note in the output, seconds; `None` keeps it.
	pub time: Option<TimeEdit>,
}

impl Default for NoteEdit {
	fn default() -> Self {
		Self {
			pitch: None,
			drift: 1.0,
			modulation: 1.0,
			formant_cents: 0.0,
			gain_db: 0.0,
			time: None,
		}
	}
}

impl NoteEdit {
	pub fn is_unchanged(&self) -> bool {
		*self == Self::default()
	}
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TimeEdit {
	pub onset: f64,
	pub offset: f64,
}

/// Edits of a segmentation: `notes[i]` edits `seg.notes[i]` (missing = unchanged).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Edits {
	pub notes: Vec<NoteEdit>,
	/// Gain of the sibilants (s, sh, t, breaths), dB.
	pub sibilant_gain_db: f64,
}

impl Edits {
	pub fn note(&self, i: usize) -> NoteEdit {
		self.notes.get(i).copied().unwrap_or_default()
	}

	pub fn is_unchanged(&self) -> bool {
		self.sibilant_gain_db == 0.0 && self.notes.iter().all(NoteEdit::is_unchanged)
	}

	/// Checks the edits against the notes they apply to; errors name the note.
	pub fn validate(&self, seg: &Segmentation) -> Result<(), NotesError> {
		if self.notes.len() > seg.notes.len() {
			return Err(NotesError::Edit(format!(
				"{} edits for {} notes: edits apply to the notes by index",
				self.notes.len(),
				seg.notes.len()
			)));
		}
		check("sibilant gain", self.sibilant_gain_db, -60.0, 24.0, " dB")?;
		for (i, (e, n)) in self.notes.iter().zip(&seg.notes).enumerate() {
			let at = |what: &str| format!("note {} ({:.2} s) {what}", i + 1, n.onset);
			if let Some(p) = e.pitch {
				check(&at("pitch"), p, 0.0, 127.0, "")?;
				if (p - n.center).abs() > MAX_SHIFT {
					return Err(NotesError::Edit(format!(
						"{}: {:.1} semitones from the sung pitch, at most {MAX_SHIFT} are \
						 rendered",
						at("pitch"),
						(p - n.center).abs()
					)));
				}
			}
			check(&at("drift"), e.drift, -1.0, 2.0, "")?;
			check(&at("modulation"), e.modulation, -1.0, 2.0, "")?;
			check(
				&at("formant"),
				e.formant_cents,
				-MAX_FORMANT,
				MAX_FORMANT,
				" cents",
			)?;
			check(&at("gain"), e.gain_db, -60.0, 24.0, " dB")?;
			if let Some(t) = e.time
				&& (!t.onset.is_finite() || !t.offset.is_finite() || t.offset <= t.onset)
			{
				return Err(NotesError::Edit(format!(
					"{}: the offset must come after the onset",
					at("time")
				)));
			}
		}
		Ok(())
	}
}

fn check(what: &str, v: f64, lo: f64, hi: f64, unit: &str) -> Result<(), NotesError> {
	if v.is_finite() && (lo..=hi).contains(&v) {
		Ok(())
	} else {
		Err(NotesError::Edit(format!(
			"{what} {v}{unit} is outside {lo}..{hi}{unit}"
		)))
	}
}

/// Per-frame curves on the analysis grid, looked up by source sample.
#[derive(Debug, Clone, PartialEq)]
pub struct Curves {
	rate: f64,
	start: f64,
	hop: f64,
	/// Semitones.
	pub shift: Vec<f64>,
	/// Cents.
	pub formant: Vec<f64>,
	/// Linear gain.
	pub gain: Vec<f64>,
}

impl Curves {
	/// No change anywhere.
	pub fn flat(analysis: &Analysis) -> Self {
		let n = analysis.frames.len();
		Self {
			rate: f64::from(analysis.sample_rate),
			start: analysis.frames.first().map_or(0.0, |f| f.time),
			hop: analysis.hop_seconds(),
			shift: vec![0.0; n],
			formant: vec![0.0; n],
			gain: vec![1.0; n],
		}
	}

	pub fn shift_at(&self, sample: f64) -> f64 {
		self.at(&self.shift, sample, 0.0)
	}

	pub fn formant_at(&self, sample: f64) -> f64 {
		self.at(&self.formant, sample, 0.0)
	}

	pub fn gain_at(&self, sample: f64) -> f64 {
		self.at(&self.gain, sample, 1.0)
	}

	fn at(&self, curve: &[f64], sample: f64, outside: f64) -> f64 {
		if curve.is_empty() {
			return outside;
		}
		let x = ((sample / self.rate - self.start) / self.hop).clamp(0.0, (curve.len() - 1) as f64);
		let i = x.floor() as usize;
		let j = (i + 1).min(curve.len() - 1);
		let w = x - x.floor();
		curve[i] * (1.0 - w) + curve[j] * w
	}
}

/// The curves `edits` ask for. `edits` must be valid for `seg` ([`Edits::validate`]).
pub fn curves(analysis: &Analysis, seg: &Segmentation, edits: &Edits) -> Curves {
	let mut c = Curves::flat(analysis);
	let notes = &seg.notes;
	let shift: Vec<Vec<f64>> = notes
		.iter()
		.enumerate()
		.map(|(i, n)| core_shift(analysis, n, &edits.note(i)))
		.collect();
	c.shift = fill(analysis, notes, |i, k| shift[i][k], 0.0);
	c.formant = fill(analysis, notes, |i, _| edits.note(i).formant_cents, 0.0);
	let mut gain_db = fill(analysis, notes, |i, _| edits.note(i).gain_db, 0.0);
	if edits.sibilant_gain_db != 0.0 {
		for (f, g) in analysis.frames.iter().zip(gain_db.iter_mut()) {
			if seg
				.sibilants
				.iter()
				.any(|s| (s.start..=s.end).contains(&f.time))
			{
				*g += edits.sibilant_gain_db;
			}
		}
	}
	c.gain = gain_db.iter().map(|db| 10f64.powf(db / 20.0)).collect();
	c
}

/// Frames `a..b` of the analysis inside `start..=end` seconds.
fn frames_in(analysis: &Analysis, start: f64, end: f64) -> (usize, usize) {
	let f = &analysis.frames;
	let a = f.partition_point(|x| x.time < start);
	let b = f.partition_point(|x| x.time <= end);
	(a, b.max(a))
}

/// Shift (semitones) of every frame of the note's core.
fn core_shift(analysis: &Analysis, n: &SegmentedNote, e: &NoteEdit) -> Vec<f64> {
	let (a, b) = frames_in(analysis, n.core_start, n.core_end);
	let base = e.pitch.map_or(0.0, |p| p - n.center);
	if e.drift == 1.0 && e.modulation == 1.0 {
		return vec![base; b - a];
	}
	let deviation: Vec<f64> = analysis.frames[a..b]
		.iter()
		.map(|f| f.pitch.map_or(0.0, |p| p - n.center))
		.collect();
	let width = ((DRIFT_WINDOW / analysis.hop_seconds()).round() as usize).max(1);
	let drift = box_filter(&box_filter(&deviation, width), width);
	deviation
		.iter()
		.zip(&drift)
		.map(|(d, slow)| base + (e.drift - 1.0) * slow + (e.modulation - 1.0) * (d - slow))
		.collect()
}

/// Centered moving average of `width` frames (shorter at the ends).
fn box_filter(x: &[f64], width: usize) -> Vec<f64> {
	let n = x.len();
	let mut prefix = vec![0.0; n + 1];
	for (i, v) in x.iter().enumerate() {
		prefix[i + 1] = prefix[i] + v;
	}
	let half = width / 2;
	(0..n)
		.map(|i| {
			let (lo, hi) = (i.saturating_sub(half), (i + half + 1).min(n));
			(prefix[hi] - prefix[lo]) / (hi - lo) as f64
		})
		.collect()
}

fn smoothstep(x: f64) -> f64 {
	let x = x.clamp(0.0, 1.0);
	x * x * (3.0 - 2.0 * x)
}

/// A curve over every frame from per-core values `core(note, k)` (k-th frame of its core):
/// cores take their values, the rest of a note its core's nearest value, transitions
/// between connected notes a smooth blend, and pitched frames touching a note (ring,
/// pre-voicing, the onset and decay the tracker calls unvoiced: [`crate::marks::pitched`])
/// the value next to them. Everything else is `outside`.
fn fill(
	analysis: &Analysis,
	notes: &[SegmentedNote],
	core: impl Fn(usize, usize) -> f64,
	outside: f64,
) -> Vec<f64> {
	let frames = &analysis.frames;
	let hop = analysis.hop_seconds();
	let mut out: Vec<Option<f64>> = vec![None; frames.len()];
	let cores: Vec<(usize, usize)> = notes
		.iter()
		.map(|n| frames_in(analysis, n.core_start, n.core_end))
		.collect();
	let first = |i: usize| {
		if cores[i].1 > cores[i].0 {
			core(i, 0)
		} else {
			outside
		}
	};
	let last = |i: usize| {
		let (a, b) = cores[i];
		if b > a { core(i, b - a - 1) } else { outside }
	};
	let connected =
		|i: usize| i + 1 < notes.len() && notes[i + 1].onset - notes[i].offset <= 2.0 * hop;
	let blend = |i: usize, t: f64| {
		let (from, to) = (notes[i].core_end, notes[i + 1].core_start);
		let w = if to > from {
			smoothstep((t - from) / (to - from))
		} else {
			0.5
		};
		last(i) * (1.0 - w) + first(i + 1) * w
	};
	for (i, n) in notes.iter().enumerate() {
		let (ca, cb) = cores[i];
		let (ra, rb) = frames_in(analysis, n.onset, n.offset);
		for (k, slot) in out.iter_mut().enumerate().take(rb).skip(ra) {
			let t = frames[k].time;
			*slot = Some(if (ca..cb).contains(&k) {
				core(i, k - ca)
			} else if t < n.core_start {
				if i > 0 && connected(i - 1) {
					blend(i - 1, t)
				} else {
					first(i)
				}
			} else if connected(i) {
				blend(i, t)
			} else {
				last(i)
			});
		}
	}
	let pitched = crate::marks::pitched(analysis);
	for k in 1..frames.len() {
		if out[k].is_none() && pitched[k].is_some() && pitched[k - 1].is_some() {
			out[k] = out[k - 1];
		}
	}
	for k in (0..frames.len().saturating_sub(1)).rev() {
		if out[k].is_none() && pitched[k].is_some() && pitched[k + 1].is_some() {
			out[k] = out[k + 1];
		}
	}
	out.into_iter().map(|v| v.unwrap_or(outside)).collect()
}

/// The time map of the edits: moved or stretched notes keep their attack (up to
/// [`ATTACK`]) at speed 1 and stretch the rest; unedited notes stay where they are, so the
/// material between an edited note and its neighbours is what stretches.
pub fn time_map(seg: &Segmentation, edits: &Edits) -> Result<TimeMap, NotesError> {
	if edits.notes.iter().all(|e| e.time.is_none()) {
		return Ok(TimeMap::identity());
	}
	let mut anchors = Vec::new();
	for (i, n) in seg.notes.iter().enumerate() {
		match edits.note(i).time {
			Some(t) => {
				let attack = (n.core_start - n.onset)
					.clamp(0.0, ATTACK)
					.min(0.5 * (n.offset - n.onset))
					.min(0.5 * (t.offset - t.onset));
				anchors.push((n.onset, t.onset));
				if attack > 0.0 {
					anchors.push((n.onset + attack, t.onset + attack));
				}
				anchors.push((n.offset, t.offset));
			}
			None => {
				anchors.push((n.onset, n.onset));
				anchors.push((n.offset, n.offset));
			}
		}
	}
	TimeMap::new(anchors)
}

/// Correct Pitch macro: how far to pull each note.
#[derive(Debug, Clone, PartialEq)]
pub struct Correction {
	/// Scale the notes snap to; `None` = nearest semitone.
	pub key: Option<Scale>,
	/// 0..1 of the way from the sung center to the scale tone.
	pub center: f64,
	/// 0..1 of the drift straightened.
	pub drift: f64,
	/// Notes closer than this to their tone keep their center, cents.
	pub threshold_cents: f64,
}

impl Default for Correction {
	fn default() -> Self {
		Self {
			key: None,
			center: 1.0,
			drift: 0.0,
			threshold_cents: 0.0,
		}
	}
}

/// The scale tone a sung center snaps to.
pub fn snap_target(key: Option<&Scale>, center: f64) -> f64 {
	match key {
		Some(k) => f64::from(crate::midi::nearest_in_key(k, center)),
		None => center.round(),
	}
}

/// Edits that correct every note of `seg` (vibrato and transitions untouched).
pub fn correct_pitch(seg: &Segmentation, c: &Correction) -> Vec<NoteEdit> {
	seg.notes
		.iter()
		.map(|n| {
			let tone = snap_target(c.key.as_ref(), n.center);
			let off = tone - n.center;
			let pitch = (off.abs() * 100.0 > c.threshold_cents && c.center > 0.0)
				.then(|| n.center + c.center.clamp(0.0, 1.0) * off);
			NoteEdit {
				pitch,
				drift: 1.0 - c.drift.clamp(0.0, 1.0),
				..NoteEdit::default()
			}
		})
		.collect()
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::analysis::AnalysisFrame;

	fn analysis(pitches: &[Option<f64>]) -> Analysis {
		Analysis {
			version: 0,
			sample_rate: 1000,
			hop: 5,
			duration: pitches.len() as f64 * 0.005,
			frames: pitches
				.iter()
				.enumerate()
				.map(|(i, p)| AnalysisFrame {
					time: i as f64 * 0.005,
					pitch: *p,
					voicing: 1.0,
					aperiodicity: 0.0,
					level_db: -20.0,
					flux: 0.0,
					hf_ratio: 0.0,
					candidates: Vec::new(),
				})
				.collect(),
		}
	}

	fn note(onset: f64, core: (f64, f64), offset: f64, center: f64) -> SegmentedNote {
		SegmentedNote {
			onset,
			offset,
			core_start: core.0,
			core_end: core.1,
			center,
			drift: 0.0,
			vibrato: None,
			confidence: 1.0,
			flags: Vec::new(),
		}
	}

	#[test]
	fn legato_transitions_blend_and_rings_keep_the_shift() {
		// 0.5 s at 60, a glide, 0.5 s at 62, then a voiced ring and silence.
		let mut p: Vec<Option<f64>> = (0..240)
			.map(|i| {
				let t = i as f64 * 0.005;
				Some(if t < 0.5 {
					60.2
				} else if t < 0.6 {
					60.2 + (t - 0.5) * 18.0
				} else {
					62.0
				})
			})
			.collect();
		p.extend(vec![None; 20]);
		let a = analysis(&p);
		let seg = Segmentation {
			notes: vec![
				note(0.0, (0.02, 0.5), 0.55, 60.2),
				note(0.55, (0.6, 1.1), 1.15, 62.0),
			],
			sibilants: Vec::new(),
		};
		let edits = Edits {
			notes: vec![
				NoteEdit {
					pitch: Some(60.0),
					..NoteEdit::default()
				},
				NoteEdit {
					pitch: Some(63.0),
					..NoteEdit::default()
				},
			],
			sibilant_gain_db: 0.0,
		};
		edits.validate(&seg).unwrap();
		let c = curves(&a, &seg, &edits);
		let at = |t: f64| c.shift[(t / 0.005).round() as usize];
		assert!((at(0.3) + 0.2).abs() < 1e-9);
		assert!((at(0.8) - 1.0).abs() < 1e-9);
		let mid = at(0.55);
		assert!(mid > -0.2 && mid < 1.0, "{mid}");
		// The ring after the second note (1.15..1.2 s, still voiced) keeps its shift.
		assert!((at(1.18) - 1.0).abs() < 1e-9);
		assert_eq!(at(1.25), 0.0);
	}

	#[test]
	fn flattening_the_modulation_cancels_the_vibrato() {
		let p: Vec<Option<f64>> = (0..200)
			.map(|i| {
				let t = i as f64 * 0.005;
				Some(64.0 + 0.5 * (2.0 * std::f64::consts::PI * 5.0 * t).sin())
			})
			.collect();
		let a = analysis(&p);
		let seg = Segmentation {
			notes: vec![note(0.0, (0.0, 0.995), 1.0, 64.0)],
			sibilants: Vec::new(),
		};
		let edits = Edits {
			notes: vec![NoteEdit {
				modulation: 0.0,
				..NoteEdit::default()
			}],
			sibilant_gain_db: 0.0,
		};
		let c = curves(&a, &seg, &edits);
		// Away from the edges, pitch + shift is flat.
		for (k, (pitch, shift)) in p.iter().zip(&c.shift).enumerate().take(150).skip(50) {
			let result = pitch.unwrap() + shift;
			assert!((result - 64.0).abs() < 0.06, "{k}: {result}");
		}
	}

	#[test]
	fn correct_pitch_snaps_to_the_key_with_a_threshold() {
		let seg = Segmentation {
			notes: vec![
				note(0.0, (0.0, 0.4), 0.5, 61.3),
				note(0.5, (0.5, 0.9), 1.0, 64.04),
			],
			sibilants: Vec::new(),
		};
		let key = Scale::parse("C major").unwrap();
		let e = correct_pitch(
			&seg,
			&Correction {
				key: Some(key),
				threshold_cents: 5.0,
				..Correction::default()
			},
		);
		// C# is not in C major: 61.3 goes to the nearer D (62) rather than C (60).
		assert_eq!(e[0].pitch, Some(62.0));
		assert_eq!(e[1].pitch, None);
	}

	#[test]
	fn moved_notes_keep_their_attack_and_crossing_moves_are_refused() {
		let seg = Segmentation {
			notes: vec![
				note(1.0, (1.05, 1.4), 1.5, 60.0),
				note(2.0, (2.05, 2.4), 2.5, 62.0),
			],
			sibilants: Vec::new(),
		};
		let mut edits = Edits {
			notes: vec![NoteEdit {
				time: Some(TimeEdit {
					onset: 1.2,
					offset: 1.9,
				}),
				..NoteEdit::default()
			}],
			sibilant_gain_db: 0.0,
		};
		let m = time_map(&seg, &edits).unwrap();
		assert!((m.to_output(1.0) - 1.2).abs() < 1e-12);
		assert!((m.to_output(1.05) - 1.25).abs() < 1e-12);
		assert!((m.to_output(1.5) - 1.9).abs() < 1e-12);
		assert!((m.to_output(2.0) - 2.0).abs() < 1e-12);
		edits.notes[0].time = Some(TimeEdit {
			onset: 1.2,
			offset: 2.2,
		});
		assert!(time_map(&seg, &edits).is_err());
	}
}
