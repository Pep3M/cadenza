//! Scales and modes.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::{Result, TheoryError};
use crate::pitch::PitchClass;

/// The supported scale/mode families.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ScaleKind {
	Major,
	NaturalMinor,
	HarmonicMinor,
	MelodicMinor,
	Dorian,
	Phrygian,
	Lydian,
	Mixolydian,
	Locrian,
	MajorPentatonic,
	MinorPentatonic,
	Blues,
	WholeTone,
	Chromatic,
}

impl ScaleKind {
	/// Ascending semitone intervals from the root, within one octave.
	pub fn intervals(&self) -> &'static [u8] {
		match self {
			ScaleKind::Major => &[0, 2, 4, 5, 7, 9, 11],
			ScaleKind::NaturalMinor => &[0, 2, 3, 5, 7, 8, 10],
			ScaleKind::HarmonicMinor => &[0, 2, 3, 5, 7, 8, 11],
			ScaleKind::MelodicMinor => &[0, 2, 3, 5, 7, 9, 11],
			ScaleKind::Dorian => &[0, 2, 3, 5, 7, 9, 10],
			ScaleKind::Phrygian => &[0, 1, 3, 5, 7, 8, 10],
			ScaleKind::Lydian => &[0, 2, 4, 6, 7, 9, 11],
			ScaleKind::Mixolydian => &[0, 2, 4, 5, 7, 9, 10],
			ScaleKind::Locrian => &[0, 1, 3, 5, 6, 8, 10],
			ScaleKind::MajorPentatonic => &[0, 2, 4, 7, 9],
			ScaleKind::MinorPentatonic => &[0, 3, 5, 7, 10],
			ScaleKind::Blues => &[0, 3, 5, 6, 7, 10],
			ScaleKind::WholeTone => &[0, 2, 4, 6, 8, 10],
			ScaleKind::Chromatic => &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11],
		}
	}

	/// Parses the "kind" portion of a scale name, e.g. "harmonic minor".
	fn parse(input: &str) -> Result<Self> {
		let normalized = input.trim().to_ascii_lowercase();
		let normalized = normalized.split_whitespace().collect::<Vec<_>>().join(" ");
		Ok(match normalized.as_str() {
			"major" | "ionian" => ScaleKind::Major,
			"minor" | "natural minor" | "aeolian" => ScaleKind::NaturalMinor,
			"harmonic minor" => ScaleKind::HarmonicMinor,
			"melodic minor" => ScaleKind::MelodicMinor,
			"dorian" => ScaleKind::Dorian,
			"phrygian" => ScaleKind::Phrygian,
			"lydian" => ScaleKind::Lydian,
			"mixolydian" => ScaleKind::Mixolydian,
			"locrian" => ScaleKind::Locrian,
			"major pentatonic" | "pentatonic major" => ScaleKind::MajorPentatonic,
			"minor pentatonic" | "pentatonic minor" | "pentatonic" => ScaleKind::MinorPentatonic,
			"blues" => ScaleKind::Blues,
			"whole tone" | "wholetone" | "whole-tone" => ScaleKind::WholeTone,
			"chromatic" => ScaleKind::Chromatic,
			_ => {
				return Err(TheoryError::InvalidScale {
					input: input.to_string(),
				});
			}
		})
	}
}

/// A scale: a root pitch class plus a kind (interval pattern).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Scale {
	pub root: PitchClass,
	pub kind: ScaleKind,
}

impl Scale {
	pub fn new(root: PitchClass, kind: ScaleKind) -> Self {
		Self { root, kind }
	}

	/// Parses `"<root> <kind>"`, e.g. `"C major"`, `"F# dorian"`,
	/// `"Eb harmonic minor"`.
	pub fn parse(input: &str) -> Result<Self> {
		let trimmed = input.trim();
		let mut parts = trimmed.splitn(2, char::is_whitespace);
		let root_str = parts.next().ok_or_else(|| TheoryError::InvalidScale {
			input: input.to_string(),
		})?;
		let kind_str = parts.next().ok_or_else(|| TheoryError::InvalidScale {
			input: input.to_string(),
		})?;
		let root = PitchClass::parse(root_str).map_err(|_| TheoryError::InvalidScale {
			input: input.to_string(),
		})?;
		let kind = ScaleKind::parse(kind_str)?;
		Ok(Self { root, kind })
	}

	fn intervals(&self) -> &'static [u8] {
		self.kind.intervals()
	}

	/// True if `pitch` is a member of the scale (any octave).
	pub fn contains(&self, pitch: u8) -> bool {
		let within = (pitch as i32 - self.root.0 as i32).rem_euclid(12);
		self.intervals().contains(&(within as u8))
	}

	/// The 1-based scale degree of `pitch`, if it belongs to the scale.
	pub fn degree_of(&self, pitch: u8) -> Option<u32> {
		let within = (pitch as i32 - self.root.0 as i32).rem_euclid(12) as u8;
		self.intervals()
			.iter()
			.position(|&iv| iv == within)
			.map(|i| (i + 1) as u32)
	}

	/// All scale pitches within `[low, high]` inclusive.
	pub fn pitches_in_range(&self, low: u8, high: u8) -> Vec<u8> {
		(low..=high).filter(|p| self.contains(*p)).collect()
	}

	/// Snaps `pitch` to the nearest scale tone. Ties round down.
	pub fn snap(&self, pitch: u8) -> u8 {
		let target = pitch as i32 - self.root.0 as i32;
		let within = target.rem_euclid(12);
		let octave_rel = target.div_euclid(12);
		let intervals = self.intervals();
		let mut best_pos = intervals[0] as i32;
		let mut best_dist = i32::MAX;
		for delta_octave in -1..=1 {
			for &iv in intervals {
				let pos = delta_octave * 12 + iv as i32;
				let dist = (pos - within).abs();
				if dist < best_dist || (dist == best_dist && pos < best_pos) {
					best_dist = dist;
					best_pos = pos;
				}
			}
		}
		(self.root.0 as i32 + octave_rel * 12 + best_pos).clamp(0, 127) as u8
	}

	/// The pitch for a 1-based scale `degree` (may be <1 or >len, wrapping
	/// into neighboring octaves) within the given MIDI `octave` (C octave 4
	/// contains MIDI 60..=71).
	pub fn degree_pitch(&self, degree: i32, octave: i32) -> u8 {
		let intervals = self.intervals();
		let len = intervals.len() as i32;
		let step = degree - 1;
		let idx = step.rem_euclid(len);
		let octave_shift = step.div_euclid(len);
		let pitch = (octave + 1) * 12
			+ self.root.0 as i32
			+ intervals[idx as usize] as i32
			+ octave_shift * 12;
		pitch.clamp(0, 127) as u8
	}

	/// Diatonic step index of `pitch` relative to this scale's intervals,
	/// treating the scale as infinite across octaves. `pitch` is snapped to
	/// the scale first if it is not already a member.
	fn diatonic_step(&self, pitch: u8) -> i64 {
		let snapped = self.snap(pitch);
		let intervals = self.intervals();
		let len = intervals.len() as i64;
		let rel = snapped as i64 - self.root.0 as i64;
		let within = rel.rem_euclid(12);
		let octave_rel = rel.div_euclid(12);
		let idx = intervals
			.iter()
			.position(|&iv| iv as i64 == within)
			.unwrap_or(0) as i64;
		octave_rel * len + idx
	}

	/// Moves `pitch` by `steps` scale degrees (diatonic transposition),
	/// snapping it onto the scale first if necessary.
	pub fn transpose(&self, pitch: u8, steps: i32) -> u8 {
		let intervals = self.intervals();
		let len = intervals.len() as i64;
		let step = self.diatonic_step(pitch) + steps as i64;
		let idx = step.rem_euclid(len);
		let octave_shift = step.div_euclid(len);
		let pitch = self.root.0 as i64 + octave_shift * 12 + intervals[idx as usize] as i64;
		pitch.clamp(0, 127) as u8
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn parses_common_scale_names() {
		assert_eq!(Scale::parse("C major").unwrap().kind, ScaleKind::Major);
		assert_eq!(
			Scale::parse("A minor").unwrap().kind,
			ScaleKind::NaturalMinor
		);
		assert_eq!(Scale::parse("F# dorian").unwrap().kind, ScaleKind::Dorian);
		assert_eq!(
			Scale::parse("Eb harmonic minor").unwrap().kind,
			ScaleKind::HarmonicMinor
		);
		assert_eq!(
			Scale::parse("C# melodic minor").unwrap().kind,
			ScaleKind::MelodicMinor
		);
		assert_eq!(
			Scale::parse("Bb major pentatonic").unwrap().kind,
			ScaleKind::MajorPentatonic
		);
		assert_eq!(Scale::parse("A blues").unwrap().kind, ScaleKind::Blues);
		assert_eq!(
			Scale::parse("C whole tone").unwrap().kind,
			ScaleKind::WholeTone
		);
		assert_eq!(
			Scale::parse("C chromatic").unwrap().kind,
			ScaleKind::Chromatic
		);
	}

	#[test]
	fn invalid_scale_name_is_rejected() {
		assert!(Scale::parse("C bogus").is_err());
		assert!(Scale::parse("major").is_err());
	}

	#[test]
	fn contains_and_degree_of() {
		let c_major = Scale::parse("C major").unwrap();
		assert!(c_major.contains(60)); // C4
		assert!(c_major.contains(64)); // E4
		assert!(!c_major.contains(63)); // Eb4
		assert_eq!(c_major.degree_of(60), Some(1));
		assert_eq!(c_major.degree_of(67), Some(5)); // G4
		assert_eq!(c_major.degree_of(63), None);
	}

	#[test]
	fn pitches_in_range() {
		let c_major = Scale::parse("C major").unwrap();
		let pitches = c_major.pitches_in_range(60, 72);
		assert_eq!(pitches, vec![60, 62, 64, 65, 67, 69, 71, 72]);
	}

	#[test]
	fn snap_rounds_down_on_ties() {
		let c_major = Scale::parse("C major").unwrap();
		// C major has no D# (63); nearest tones are D(62) and E(64), a tie:
		// must round down to D.
		assert_eq!(c_major.snap(63), 62);
		assert_eq!(c_major.snap(60), 60);
		assert_eq!(c_major.snap(66), 65); // F#(66) nearer to F(65) than G(67)
	}

	#[test]
	fn degree_pitch_wraps_octaves() {
		let c_major = Scale::parse("C major").unwrap();
		assert_eq!(c_major.degree_pitch(1, 4), 60);
		assert_eq!(c_major.degree_pitch(5, 4), 67);
		// Degree 8 wraps to the next octave's root.
		assert_eq!(c_major.degree_pitch(8, 4), 72);
		// Degree 0 wraps down to the 7th degree of the octave below.
		assert_eq!(c_major.degree_pitch(0, 4), 59);
	}

	#[test]
	fn diatonic_transpose_stays_in_scale() {
		let c_major = Scale::parse("C major").unwrap();
		assert_eq!(c_major.transpose(60, 1), 62); // C -> D
		assert_eq!(c_major.transpose(60, 2), 64); // C -> E
		assert_eq!(c_major.transpose(60, 7), 72); // C -> C (octave up)
		assert_eq!(c_major.transpose(62, -1), 60); // D -> C
	}
}
