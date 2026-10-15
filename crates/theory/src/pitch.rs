//! Pitch classes, note-name parsing and MIDI formatting.
//!
//! MIDI 60 is C4 (the convention used by the rest of Cadenza).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::{Result, TheoryError};

/// One of the 12 pitch classes, `0` = C .. `11` = B.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct PitchClass(pub u8);

const SHARP_NAMES: [&str; 12] = [
	"C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
];
const FLAT_NAMES: [&str; 12] = [
	"C", "Db", "D", "Eb", "E", "F", "Gb", "G", "Ab", "A", "Bb", "B",
];

impl PitchClass {
	/// Builds a pitch class from any integer, reducing it modulo 12.
	pub fn from_semitone(semitone: i32) -> Self {
		Self(semitone.rem_euclid(12) as u8)
	}

	/// Spelling using sharps (e.g. `C#`).
	pub fn sharp_name(&self) -> &'static str {
		SHARP_NAMES[self.0 as usize % 12]
	}

	/// Spelling using flats (e.g. `Db`).
	pub fn flat_name(&self) -> &'static str {
		FLAT_NAMES[self.0 as usize % 12]
	}

	/// Spelling using the caller's preference.
	pub fn name(&self, prefer_flat: bool) -> &'static str {
		if prefer_flat {
			self.flat_name()
		} else {
			self.sharp_name()
		}
	}

	/// Parses a bare pitch class such as `"C"`, `"F#"`, `"Bb"` (no octave).
	pub fn parse(input: &str) -> Result<Self> {
		let trimmed = input.trim();
		let (letter_semitone, rest) =
			parse_letter(trimmed).ok_or_else(|| TheoryError::InvalidPitchClass {
				input: input.to_string(),
			})?;
		let accidental = parse_accidental(rest);
		if accidental.1 != rest.len() {
			return Err(TheoryError::InvalidPitchClass {
				input: input.to_string(),
			});
		}
		Ok(Self::from_semitone(letter_semitone + accidental.0))
	}
}

/// Parses a leading note letter (`A`..`G`, case-insensitive), returning its
/// natural semitone value (relative to C) and the remaining slice.
pub(crate) fn parse_letter(s: &str) -> Option<(i32, &str)> {
	let mut chars = s.chars();
	let first = chars.next()?;
	let semitone = match first.to_ascii_uppercase() {
		'C' => 0,
		'D' => 2,
		'E' => 4,
		'F' => 5,
		'G' => 7,
		'A' => 9,
		'B' => 11,
		_ => return None,
	};
	Some((semitone, chars.as_str()))
}

/// Parses leading `#`/`♯` (sharp) or `b`/`♭` (flat) characters, returning the
/// total semitone offset and how many bytes were consumed.
pub(crate) fn parse_accidental(s: &str) -> (i32, usize) {
	let mut offset = 0;
	let mut consumed = 0;
	for ch in s.chars() {
		match ch {
			'#' | '♯' => offset += 1,
			'b' | '♭' => offset -= 1,
			_ => break,
		}
		consumed += ch.len_utf8();
	}
	(offset, consumed)
}

/// A fully spelled note name: letter + accidental + octave (e.g. `F#3`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NoteName {
	pub letter_semitone: i32,
	pub accidental: i32,
	pub octave: i32,
}

impl NoteName {
	/// Converts to an absolute MIDI pitch (C4 = 60), if in range.
	pub fn to_midi(&self) -> Result<u8> {
		let midi = (self.octave + 1) * 12 + self.letter_semitone + self.accidental;
		if (0..=127).contains(&midi) {
			Ok(midi as u8)
		} else {
			Err(TheoryError::PitchOutOfRange { pitch: midi })
		}
	}
}

/// Parses a full note name such as `"C4"`, `"F#3"`, `"Bb2"`, `"Cb4"`,
/// `"E#4"`, `"C-1"`.
pub fn parse_note_name(input: &str) -> Result<NoteName> {
	let trimmed = input.trim();
	let invalid = |reason: &str| TheoryError::InvalidNoteName {
		input: input.to_string(),
		reason: reason.to_string(),
	};

	let (letter_semitone, rest) =
		parse_letter(trimmed).ok_or_else(|| invalid("must start with a letter A-G"))?;
	let (accidental, consumed) = parse_accidental(rest);
	let octave_str = &rest[consumed..];
	if octave_str.is_empty() {
		return Err(invalid("missing octave number"));
	}
	let octave: i32 = octave_str
		.parse()
		.map_err(|_| invalid("octave must be an integer, e.g. 4 or -1"))?;

	Ok(NoteName {
		letter_semitone,
		accidental,
		octave,
	})
}

/// Parses a note name or a bare MIDI number ("60") into a MIDI pitch.
pub fn parse_pitch(input: &str) -> Result<u8> {
	let trimmed = input.trim();
	if let Ok(n) = trimmed.parse::<i32>() {
		return if (0..=127).contains(&n) {
			Ok(n as u8)
		} else {
			Err(TheoryError::PitchOutOfRange { pitch: n })
		};
	}
	parse_note_name(trimmed)?.to_midi()
}

/// Formats a MIDI pitch as a note name, e.g. `60 -> "C4"`.
pub fn format_pitch(midi: u8, prefer_flat: bool) -> String {
	let octave = (midi as i32) / 12 - 1;
	let pc = PitchClass::from_semitone(midi as i32);
	format!("{}{}", pc.name(prefer_flat), octave)
}

/// The pitch class (0..=11) of a MIDI pitch.
pub fn pitch_class_of(midi: u8) -> PitchClass {
	PitchClass::from_semitone(midi as i32)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn middle_c_is_sixty() {
		assert_eq!(parse_pitch("C4").unwrap(), 60);
	}

	#[test]
	fn bare_numbers_are_accepted() {
		assert_eq!(parse_pitch("60").unwrap(), 60);
		assert_eq!(parse_pitch("0").unwrap(), 0);
		assert_eq!(parse_pitch("127").unwrap(), 127);
		assert!(parse_pitch("128").is_err());
	}

	#[test]
	fn sharp_flat_and_negative_octave_round_trip() {
		for (name, prefer_flat) in [
			("C4", false),
			("F#3", false),
			("Bb2", true),
			("C-1", false),
			("G9", false),
		] {
			let midi = parse_pitch(name).unwrap();
			assert_eq!(
				format_pitch(midi, prefer_flat),
				name,
				"round trip for {name}"
			);
		}
	}

	#[test]
	fn unusual_spellings_resolve_to_the_right_midi_pitch() {
		// E#4 is enharmonically F4 (65); Cb4 is enharmonically B3 (59).
		assert_eq!(parse_pitch("E#4").unwrap(), 65);
		assert_eq!(parse_pitch("Cb4").unwrap(), 59);
	}

	#[test]
	fn invalid_note_names_are_rejected() {
		assert!(parse_pitch("H4").is_err());
		assert!(parse_pitch("C").is_err());
		assert!(parse_pitch("").is_err());
	}
}
