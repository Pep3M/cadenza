//! Chord symbol parsing.
//!
//! Supports triads, sevenths, sixths, extensions (9/11/13), alterations
//! (b5, #5, b9, #9, #11, b13), `add`/`no` modifiers and slash bass notes.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::{Result, TheoryError};
use crate::pitch::{self, PitchClass};

const THIRD_CLASS: [i32; 2] = [3, 4];
const FIFTH_CLASS: [i32; 3] = [6, 7, 8];
const NINTH_CLASS: [i32; 3] = [13, 14, 15];
const ELEVENTH_CLASS: [i32; 2] = [17, 18];
const THIRTEENTH_CLASS: [i32; 3] = [20, 21, 22];

/// A parsed chord: root, intervals (semitones from the root, extensions
/// expanded above the octave, e.g. a 9th is `14`), optional slash bass, and
/// the original symbol text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Chord {
	pub root: PitchClass,
	pub intervals: Vec<u8>,
	pub bass: Option<PitchClass>,
	pub symbol: String,
}

impl Chord {
	/// Builds a chord directly from a root and a set of semitone intervals.
	pub fn new(root: PitchClass, intervals: Vec<u8>, bass: Option<PitchClass>) -> Self {
		let symbol = format!("{}(custom)", root.sharp_name());
		Self {
			root,
			intervals,
			bass,
			symbol,
		}
	}

	/// Parses a chord symbol such as `"Cm7"`, `"G13"`, `"Am7/G"`, `"Bb7alt"`.
	pub fn parse(input: &str) -> Result<Self> {
		let trimmed = input.trim();
		if trimmed.is_empty() {
			return Err(TheoryError::InvalidChord {
				symbol: input.to_string(),
				reason: "chord symbol is empty".to_string(),
			});
		}

		let (letter_semitone, after_letter) =
			pitch::parse_letter(trimmed).ok_or_else(|| TheoryError::InvalidChord {
				symbol: trimmed.to_string(),
				reason: "chord symbol must start with a root letter A-G".to_string(),
			})?;
		let (acc_offset, consumed) = pitch::parse_accidental(after_letter);
		let root = PitchClass::from_semitone(letter_semitone + acc_offset);
		let after_root = &after_letter[consumed..];

		let (intervals, leftover) = parse_quality(after_root, &[0, 4, 7]).map_err(|token| {
			TheoryError::UnrecognizedChordToken {
				symbol: trimmed.to_string(),
				token,
			}
		})?;

		let bass = if let Some(bass_str) = leftover.strip_prefix('/') {
			if bass_str.is_empty() {
				return Err(TheoryError::InvalidChord {
					symbol: trimmed.to_string(),
					reason: "expected a bass note after '/'".to_string(),
				});
			}
			Some(
				PitchClass::parse(bass_str).map_err(|_| TheoryError::InvalidChord {
					symbol: trimmed.to_string(),
					reason: format!(
						"invalid bass note '{bass_str}': expected a letter A-G optionally \
					 followed by # or b"
					),
				})?,
			)
		} else {
			None
		};

		Ok(Self {
			root,
			intervals,
			bass,
			symbol: trimmed.to_string(),
		})
	}

	/// The chord's pitch classes (root first, deduplicated, in stacking
	/// order).
	pub fn pitch_classes(&self) -> Vec<PitchClass> {
		let mut seen = [false; 12];
		let mut out = Vec::new();
		for &iv in &self.intervals {
			let pc = PitchClass::from_semitone(self.root.0 as i32 + iv as i32);
			if !seen[pc.0 as usize] {
				seen[pc.0 as usize] = true;
				out.push(pc);
			}
		}
		out
	}
}

/// Removes the chord-interval degrees contained in `class` from `intervals`.
fn remove_class(intervals: &mut Vec<u8>, class: &[i32]) {
	intervals.retain(|&iv| !class.contains(&(iv as i32)));
}

enum ModifierAction {
	Add(u8),
	RemoveClass(&'static [i32]),
	Alter(&'static [i32], u8),
}

fn base_token_table() -> Vec<(&'static str, &'static [u8])> {
	let mut tokens: Vec<(&'static str, &'static [u8])> = vec![
		("maj13", &[0, 4, 7, 11, 14, 17, 21]),
		("M13", &[0, 4, 7, 11, 14, 17, 21]),
		("min13", &[0, 3, 7, 10, 14, 17, 21]),
		("m13", &[0, 3, 7, 10, 14, 17, 21]),
		("-13", &[0, 3, 7, 10, 14, 17, 21]),
		("maj11", &[0, 4, 7, 11, 14, 17]),
		("M11", &[0, 4, 7, 11, 14, 17]),
		("min11", &[0, 3, 7, 10, 14, 17]),
		("m11", &[0, 3, 7, 10, 14, 17]),
		("-11", &[0, 3, 7, 10, 14, 17]),
		("maj9", &[0, 4, 7, 11, 14]),
		("M9", &[0, 4, 7, 11, 14]),
		("min9", &[0, 3, 7, 10, 14]),
		("m9", &[0, 3, 7, 10, 14]),
		("-9", &[0, 3, 7, 10, 14]),
		("mmaj7", &[0, 3, 7, 11]),
		("minmaj7", &[0, 3, 7, 11]),
		("m7b5", &[0, 3, 6, 10]),
		("\u{f8}7", &[0, 3, 6, 10]),
		("\u{f8}", &[0, 3, 6, 10]),
		("dim7", &[0, 3, 6, 9]),
		("\u{b0}7", &[0, 3, 6, 9]),
		("aug7", &[0, 4, 8, 10]),
		("+7", &[0, 4, 8, 10]),
		("7sus4", &[0, 5, 7, 10]),
		("maj7", &[0, 4, 7, 11]),
		("M7", &[0, 4, 7, 11]),
		("\u{394}7", &[0, 4, 7, 11]),
		("\u{394}", &[0, 4, 7, 11]),
		("min7", &[0, 3, 7, 10]),
		("m7", &[0, 3, 7, 10]),
		("-7", &[0, 3, 7, 10]),
		("6/9", &[0, 4, 7, 9, 14]),
		("69", &[0, 4, 7, 9, 14]),
		("min6", &[0, 3, 7, 9]),
		("m6", &[0, 3, 7, 9]),
		("-6", &[0, 3, 7, 9]),
		("dim", &[0, 3, 6]),
		("\u{b0}", &[0, 3, 6]),
		("aug", &[0, 4, 8]),
		("sus2", &[0, 2, 7]),
		("sus4", &[0, 5, 7]),
		("sus", &[0, 5, 7]),
		("maj", &[0, 4, 7]),
		("M", &[0, 4, 7]),
		("min", &[0, 3, 7]),
		("m", &[0, 3, 7]),
		("-", &[0, 3, 7]),
		("+", &[0, 4, 8]),
		("13", &[0, 4, 7, 10, 14, 17, 21]),
		("11", &[0, 4, 7, 10, 14, 17]),
		("9", &[0, 4, 7, 10, 14]),
		("7", &[0, 4, 7, 10]),
		("6", &[0, 4, 7, 9]),
		("5", &[0, 7]),
	];
	tokens.sort_by_key(|(tok, _)| std::cmp::Reverse(tok.len()));
	tokens
}

fn modifier_token_table() -> Vec<(&'static str, ModifierAction)> {
	let mut tokens = vec![
		("add13", ModifierAction::Add(21)),
		("add11", ModifierAction::Add(17)),
		("add9", ModifierAction::Add(14)),
		("add6", ModifierAction::Add(9)),
		("add2", ModifierAction::Add(2)),
		("no3", ModifierAction::RemoveClass(&THIRD_CLASS)),
		("no5", ModifierAction::RemoveClass(&FIFTH_CLASS)),
		("#11", ModifierAction::Alter(&ELEVENTH_CLASS, 18)),
		("b13", ModifierAction::Alter(&THIRTEENTH_CLASS, 20)),
		("b9", ModifierAction::Alter(&NINTH_CLASS, 13)),
		("#9", ModifierAction::Alter(&NINTH_CLASS, 15)),
		("b5", ModifierAction::Alter(&FIFTH_CLASS, 6)),
		("#5", ModifierAction::Alter(&FIFTH_CLASS, 8)),
	];
	tokens.sort_by_key(|(tok, _)| std::cmp::Reverse(tok.len()));
	tokens
}

/// Strips spaces/parentheses and normalizes unicode accidentals so
/// `"m(maj7)"`, `"C7(#9)"`, `"C7 b9"` all reduce to a plain token stream.
fn normalize_quality(s: &str) -> String {
	s.chars()
		.filter(|c| !matches!(c, '(' | ')' | ' '))
		.map(|c| match c {
			'\u{266d}' => 'b',
			'\u{266f}' => '#',
			other => other,
		})
		.collect()
}

/// Parses the quality portion of a chord symbol (everything after the root)
/// into an interval set, returning the leftover slice (empty, or starting
/// with `/` for a slash bass). On failure, returns the unrecognized token.
pub(crate) fn parse_quality(
	remainder: &str,
	default_base: &[u8],
) -> std::result::Result<(Vec<u8>, String), String> {
	let normalized = normalize_quality(remainder);
	let mut rest: &str = normalized.as_str();

	let base_tokens = base_token_table();
	let mut intervals: Vec<u8> = default_base.to_vec();
	for (tok, iv) in &base_tokens {
		if rest.starts_with(tok) {
			intervals = iv.to_vec();
			rest = &rest[tok.len()..];
			break;
		}
	}

	let modifier_tokens = modifier_token_table();
	while !rest.is_empty() && !rest.starts_with('/') {
		let mut matched = false;
		for (tok, action) in &modifier_tokens {
			if rest.starts_with(tok) {
				match action {
					ModifierAction::Add(v) => {
						if !intervals.contains(v) {
							intervals.push(*v);
						}
					}
					ModifierAction::RemoveClass(class) => remove_class(&mut intervals, class),
					ModifierAction::Alter(class, v) => {
						remove_class(&mut intervals, class);
						intervals.push(*v);
					}
				}
				rest = &rest[tok.len()..];
				matched = true;
				break;
			}
		}
		if !matched {
			return Err(rest.to_string());
		}
	}

	intervals.sort_unstable();
	intervals.dedup();
	Ok((intervals, rest.to_string()))
}
