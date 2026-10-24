//! Hand labels for real recordings: `<name>.json` next to the audio file.
//!
//! ```json
//! { "notes": [
//!   { "onset": 1.20, "offset": 1.62, "pitch": "A3" },
//!   { "onset": 1.62, "offset": 2.10, "pitch": 59.2, "core_start": 1.70, "core_end": 2.02 }
//! ] }
//! ```
//!
//! `pitch` is a note name or a fractional MIDI number (the sung center when known, else
//! the intended note); `nominal` defaults to it rounded. Without `core_start`/`core_end`
//! the core is the note minus 25 % of its length (at most 60 ms) at each end.

use serde::{Deserialize, Serialize};

use super::TruthNote;
use crate::{NotesError, Result};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LabelFile {
	pub notes: Vec<LabelNote>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LabelNote {
	pub onset: f64,
	pub offset: f64,
	pub pitch: LabelPitch,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub nominal: Option<f64>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub core_start: Option<f64>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub core_end: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum LabelPitch {
	Midi(f64),
	Name(String),
}

/// Parses a label file into truth notes, sorted by onset.
pub fn parse(json: &str) -> Result<Vec<TruthNote>> {
	let file: LabelFile =
		serde_json::from_str(json).map_err(|e| NotesError::Labels(e.to_string()))?;
	let mut notes = file
		.notes
		.iter()
		.enumerate()
		.map(|(i, n)| to_truth(n).map_err(|e| NotesError::Labels(format!("note {}: {e}", i + 1))))
		.collect::<Result<Vec<_>>>()?;
	notes.sort_by(|a, b| a.onset.total_cmp(&b.onset));
	Ok(notes)
}

/// Writes truth notes as a label file (used to save the synthetic corpus).
pub fn to_json(truth: &[TruthNote]) -> String {
	let file = LabelFile {
		notes: truth
			.iter()
			.map(|t| LabelNote {
				onset: t.onset,
				offset: t.offset,
				pitch: LabelPitch::Midi(t.center),
				nominal: Some(t.nominal),
				core_start: Some(t.core_start),
				core_end: Some(t.core_end),
			})
			.collect(),
	};
	serde_json::to_string_pretty(&file).expect("label structs serialize")
}

fn to_truth(n: &LabelNote) -> std::result::Result<TruthNote, String> {
	if !(n.onset >= 0.0 && n.offset > n.onset) {
		return Err(format!(
			"offset {} must be after onset {} (seconds, >= 0)",
			n.offset, n.onset
		));
	}
	let center = match &n.pitch {
		LabelPitch::Midi(m) if (0.0..=127.0).contains(m) => *m,
		LabelPitch::Midi(m) => return Err(format!("pitch {m} is outside MIDI 0..127")),
		LabelPitch::Name(s) => {
			f64::from(cadenza_theory::parse_pitch(s).map_err(|e| e.to_string())?)
		}
	};
	let trim = (0.25 * (n.offset - n.onset)).min(0.06);
	let core_start = n.core_start.unwrap_or(n.onset + trim);
	let core_end = n.core_end.unwrap_or(n.offset - trim);
	if !(n.onset <= core_start && core_start < core_end && core_end <= n.offset) {
		return Err(format!(
			"core {core_start}..{core_end} must lie inside the note {}..{}",
			n.onset, n.offset
		));
	}
	Ok(TruthNote {
		onset: n.onset,
		offset: n.offset,
		nominal: n.nominal.unwrap_or(center.round()),
		center,
		core_start,
		core_end,
	})
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn parses_names_numbers_and_default_cores() {
		let notes = parse(
			r#"{"notes":[
				{"onset":1.62,"offset":2.1,"pitch":59.2,"core_start":1.7,"core_end":2.0},
				{"onset":1.2,"offset":1.62,"pitch":"A3"}
			]}"#,
		)
		.unwrap();
		assert_eq!(notes[0].center, 57.0);
		assert!((notes[0].core_start - 1.26).abs() < 1e-9);
		assert_eq!(notes[1].nominal, 59.0);
		assert_eq!(notes[1].core_end, 2.0);
	}

	#[test]
	fn errors_name_the_note() {
		let err = parse(
			r#"{"notes":[{"onset":1,"offset":2,"pitch":"C4"},{"onset":3,"offset":2,"pitch":60}]}"#,
		)
		.unwrap_err()
		.to_string();
		assert!(err.contains("note 2"), "{err}");
	}

	#[test]
	fn round_trips_truth() {
		let truth = vec![TruthNote {
			onset: 0.1,
			offset: 0.6,
			nominal: 60.0,
			center: 60.12,
			core_start: 0.15,
			core_end: 0.55,
		}];
		assert_eq!(parse(&to_json(&truth)).unwrap(), truth);
	}
}
