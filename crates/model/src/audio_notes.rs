//! Notes detected in an audio clip and the edits made to them (note editing, NE-5).
//!
//! Times are **seconds of the clip's file** (like `offset_seconds` and fades: they describe
//! the recording and do not move with tempo changes); pitches are fractional MIDI numbers
//! (60.5 = C4 + 50 cents). The detection itself (`cadenza-notes`) fills [`NoteEdits`]; this
//! module only stores, validates and patches it. The rendered result is derived data: it is
//! keyed by the file and these edits and kept outside the project.

use cadenza_core::AudioNoteId;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::ModelError;

/// Largest pitch move from the sung center, semitones.
pub const MAX_SHIFT: f64 = 24.0;
/// Largest formant move, cents.
pub const MAX_FORMANT_CENTS: f32 = 1200.0;
/// Most notes in one clip.
pub const MAX_NOTES: usize = 20_000;

fn one() -> f32 {
	1.0
}

fn is_one(v: &f32) -> bool {
	*v == 1.0
}

fn is_zero(v: &f32) -> bool {
	*v == 0.0
}

/// The notes of an audio clip and how they are edited.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct NoteEdits {
	/// In time order, never overlapping.
	pub notes: Vec<AudioNote>,
	/// Unvoiced, bright stretches (s, sh, t, breaths): never pitch-shifted.
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub sibilants: Vec<NoteSpan>,
	#[serde(default, skip_serializing_if = "is_zero")]
	pub sibilant_gain_db: f32,
	/// Detection sensitivity the notes were found with (1 = default).
	#[serde(default = "one", skip_serializing_if = "is_one")]
	pub sensitivity: f32,
	/// Separations the user made (seconds); a re-detection keeps them.
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub user_splits: Vec<f64>,
	/// Boundaries the user removed (seconds); a re-detection keeps the notes joined.
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub user_merges: Vec<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct NoteSpan {
	pub start: f64,
	pub end: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AudioNote {
	pub id: AudioNoteId,
	/// The note's region, seconds of the file.
	pub start: f64,
	pub end: f64,
	/// Its stable part, where the center comes from.
	pub core_start: f64,
	pub core_end: f64,
	/// Detected pitch center (fractional MIDI).
	pub center: f64,
	/// New pitch center; `None` keeps the sung one.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub pitch: Option<f64>,
	/// Share of the slow pitch drift kept (1 = as sung, 0 = straight), -1..2.
	#[serde(default = "one", skip_serializing_if = "is_one")]
	pub drift: f32,
	/// Share of the modulation (vibrato) kept (1 = as sung, 0 = flat), -1..2.
	#[serde(default = "one", skip_serializing_if = "is_one")]
	pub modulation: f32,
	#[serde(default, skip_serializing_if = "is_zero")]
	pub formant_cents: f32,
	#[serde(default, skip_serializing_if = "is_zero")]
	pub gain_db: f32,
	/// New place of the note, seconds of the file; `None` keeps it.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub time: Option<NoteSpan>,
	/// 0..1: how sure the detection is that this region is exactly one note.
	#[serde(default = "one", skip_serializing_if = "is_one")]
	pub confidence: f32,
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub flags: Vec<AudioNoteFlag>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "flag", rename_all = "snake_case")]
pub enum AudioNoteFlag {
	/// Splitting at `at` (seconds of the file) would give two notes `cents` apart.
	MaybeTwoNotes {
		at: f64,
		cents: f32,
	},
	ShortCore,
	WeakVoicing,
}

impl AudioNote {
	/// A note at `center` over `start..end`, its core the whole region, nothing edited.
	pub fn new(id: AudioNoteId, start: f64, end: f64, center: f64) -> Self {
		Self {
			id,
			start,
			end,
			core_start: start,
			core_end: end,
			center,
			pitch: None,
			drift: 1.0,
			modulation: 1.0,
			formant_cents: 0.0,
			gain_db: 0.0,
			time: None,
			confidence: 1.0,
			flags: Vec::new(),
		}
	}

	/// Whether rendering this note changes anything.
	pub fn is_edited(&self) -> bool {
		self.pitch.is_some_and(|p| p != self.center)
			|| self.drift != 1.0
			|| self.modulation != 1.0
			|| self.formant_cents != 0.0
			|| self.gain_db != 0.0
			|| self
				.time
				.is_some_and(|t| t.start != self.start || t.end != self.end)
	}

	/// The pitch the note ends up at.
	pub fn result_pitch(&self) -> f64 {
		self.pitch.unwrap_or(self.center)
	}
}

impl NoteEdits {
	/// Whether rendering changes the audio at all.
	pub fn is_edited(&self) -> bool {
		self.sibilant_gain_db != 0.0 && !self.sibilants.is_empty()
			|| self.notes.iter().any(AudioNote::is_edited)
	}

	pub fn note(&self, id: AudioNoteId) -> Option<&AudioNote> {
		self.notes.iter().find(|n| n.id == id)
	}

	pub fn summary(&self) -> AudioNotesSummary {
		let edited = self.notes.iter().filter(|n| n.is_edited()).count();
		let errors: Vec<f64> = self
			.notes
			.iter()
			.map(|n| {
				let p = n.result_pitch();
				(p - p.round()).abs() * 100.0
			})
			.collect();
		let avg = errors.iter().sum::<f64>() / errors.len().max(1) as f64;
		AudioNotesSummary {
			count: self.notes.len(),
			edited,
			avg_error_cents: (avg * 10.0).round() / 10.0,
			flagged: self.notes.iter().filter(|n| !n.flags.is_empty()).count(),
		}
	}
}

/// What `get_project` shows of a clip's notes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AudioNotesSummary {
	pub count: usize,
	pub edited: usize,
	/// Mean distance of the resulting pitches from the nearest semitone.
	pub avg_error_cents: f64,
	/// Notes the detection is unsure about.
	pub flagged: usize,
}

/// Changes to one note; fields left out are unchanged.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AudioNotePatch {
	pub id: AudioNoteId,
	/// `null` = back to the sung pitch.
	#[serde(
		default,
		deserialize_with = "crate::command::double_option",
		skip_serializing_if = "Option::is_none"
	)]
	pub pitch: Option<Option<f64>>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub drift: Option<f32>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub modulation: Option<f32>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub formant_cents: Option<f32>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub gain_db: Option<f32>,
	/// `null` = back to where it was sung.
	#[serde(
		default,
		deserialize_with = "crate::command::double_option",
		skip_serializing_if = "Option::is_none"
	)]
	pub time: Option<Option<NoteSpan>>,
}

impl AudioNotePatch {
	pub fn is_empty(&self) -> bool {
		self.pitch.is_none()
			&& self.drift.is_none()
			&& self.modulation.is_none()
			&& self.formant_cents.is_none()
			&& self.gain_db.is_none()
			&& self.time.is_none()
	}

	/// Applies the patch, returning the one that undoes it.
	pub(crate) fn apply(&self, n: &mut AudioNote) -> AudioNotePatch {
		AudioNotePatch {
			id: n.id,
			pitch: self.pitch.map(|v| std::mem::replace(&mut n.pitch, v)),
			drift: self.drift.map(|v| std::mem::replace(&mut n.drift, v)),
			modulation: self
				.modulation
				.map(|v| std::mem::replace(&mut n.modulation, v)),
			formant_cents: self
				.formant_cents
				.map(|v| std::mem::replace(&mut n.formant_cents, v)),
			gain_db: self.gain_db.map(|v| std::mem::replace(&mut n.gain_db, v)),
			time: self.time.map(|v| std::mem::replace(&mut n.time, v)),
		}
	}
}

fn bad(what: String, value: impl ToString, reason: &str) -> ModelError {
	ModelError::invalid(what, value, reason)
}

fn range(what: String, v: f64, lo: f64, hi: f64) -> Result<(), ModelError> {
	if v.is_finite() && (lo..=hi).contains(&v) {
		Ok(())
	} else {
		Err(bad(what, v, &format!("use {lo}..{hi}")))
	}
}

fn span(what: String, s: &NoteSpan) -> Result<(), ModelError> {
	if s.start.is_finite() && s.end.is_finite() && s.start >= 0.0 && s.end > s.start {
		Ok(())
	} else {
		Err(bad(
			what,
			format!("{}..{}", s.start, s.end),
			"use seconds of the file with end > start >= 0",
		))
	}
}

/// Checks a clip's notes: ordering, ranges, unique ids.
pub fn validate(edits: &NoteEdits) -> Result<(), ModelError> {
	if edits.notes.len() > MAX_NOTES {
		return Err(bad(
			"audio notes".into(),
			edits.notes.len(),
			&format!("at most {MAX_NOTES} notes per clip"),
		));
	}
	range(
		"sibilant_gain_db".into(),
		f64::from(edits.sibilant_gain_db),
		-60.0,
		24.0,
	)?;
	range(
		"sensitivity".into(),
		f64::from(edits.sensitivity),
		0.1,
		10.0,
	)?;
	for (i, s) in edits.sibilants.iter().enumerate() {
		span(format!("sibilant {}", i + 1), s)?;
	}
	for t in edits.user_splits.iter().chain(&edits.user_merges) {
		range("user split/merge time".into(), *t, 0.0, f64::MAX)?;
	}
	let mut ids = std::collections::BTreeSet::new();
	let mut previous_end = f64::NEG_INFINITY;
	for (i, n) in edits.notes.iter().enumerate() {
		let at = |what: &str| format!("audio note {} ({}) {what}", i + 1, n.id);
		if !ids.insert(n.id) {
			return Err(bad(at("id"), n.id, "note ids must be unique"));
		}
		span(
			at("region"),
			&NoteSpan {
				start: n.start,
				end: n.end,
			},
		)?;
		if !(n.core_start >= n.start && n.core_end <= n.end && n.core_end >= n.core_start) {
			return Err(bad(
				at("core"),
				format!("{}..{}", n.core_start, n.core_end),
				"the core must lie inside the note's region",
			));
		}
		if n.start < previous_end - 1e-9 {
			return Err(bad(
				at("start"),
				n.start,
				"notes must be in time order without overlapping",
			));
		}
		previous_end = n.end;
		range(at("center"), n.center, 0.0, 127.0)?;
		if let Some(p) = n.pitch {
			range(at("pitch"), p, 0.0, 127.0)?;
			if (p - n.center).abs() > MAX_SHIFT {
				return Err(bad(
					at("pitch"),
					p,
					&format!("at most {MAX_SHIFT} semitones from the sung pitch"),
				));
			}
		}
		range(at("drift"), f64::from(n.drift), -1.0, 2.0)?;
		range(at("modulation"), f64::from(n.modulation), -1.0, 2.0)?;
		let f = f64::from(MAX_FORMANT_CENTS);
		range(at("formant_cents"), f64::from(n.formant_cents), -f, f)?;
		range(at("gain_db"), f64::from(n.gain_db), -60.0, 24.0)?;
		range(at("confidence"), f64::from(n.confidence), 0.0, 1.0)?;
		if let Some(t) = &n.time {
			span(at("time"), t)?;
		}
	}
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;

	fn edits() -> NoteEdits {
		NoteEdits {
			notes: vec![
				AudioNote::new(AudioNoteId::new(), 0.1, 0.5, 60.2),
				AudioNote::new(AudioNoteId::new(), 0.5, 0.9, 62.9),
			],
			sibilants: Vec::new(),
			sibilant_gain_db: 0.0,
			sensitivity: 1.0,
			user_splits: Vec::new(),
			user_merges: Vec::new(),
		}
	}

	#[test]
	fn validation_names_the_note_and_the_fix() {
		let mut e = edits();
		validate(&e).unwrap();
		e.notes[1].start = 0.4;
		let err = validate(&e).unwrap_err().to_string();
		assert!(
			err.contains("audio note 2") && err.contains("time order"),
			"{err}"
		);
		let mut e = edits();
		e.notes[0].pitch = Some(90.0);
		assert!(
			validate(&e)
				.unwrap_err()
				.to_string()
				.contains("24 semitones")
		);
	}

	#[test]
	fn patches_undo_themselves_and_minimal_json() {
		let mut e = edits();
		assert!(!e.is_edited());
		let p = AudioNotePatch {
			id: e.notes[0].id,
			pitch: Some(Some(60.0)),
			gain_db: Some(-3.0),
			..Default::default()
		};
		let undo = p.apply(&mut e.notes[0]);
		assert!(e.is_edited());
		assert_eq!(e.summary().edited, 1);
		undo.apply(&mut e.notes[0]);
		assert_eq!(e, edits_with_ids(&e));
		let json = serde_json::to_string(&e.notes[0]).unwrap();
		assert!(!json.contains("drift") && !json.contains("pitch"), "{json}");
	}

	fn edits_with_ids(e: &NoteEdits) -> NoteEdits {
		let mut fresh = edits();
		for (a, b) in fresh.notes.iter_mut().zip(&e.notes) {
			a.id = b.id;
		}
		fresh
	}
}
