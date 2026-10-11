//! Roman-numeral chord analysis within a key.

use crate::chord::{self, Chord};
use crate::error::{Result, TheoryError};
use crate::pitch::{self, PitchClass};
use crate::scale::{Scale, ScaleKind};

const MAJOR_DEGREES: [i32; 7] = [0, 2, 4, 5, 7, 9, 11];
const NATURAL_MINOR_DEGREES: [i32; 7] = [0, 2, 3, 5, 7, 8, 10];

const NUMERALS: [(&str, u32); 7] = [
	("vii", 7),
	("vi", 6),
	("iv", 4),
	("iii", 3),
	("ii", 2),
	("i", 1),
	("v", 5),
];

struct NumeralToken {
	degree: u32,
	accidental: i32,
	is_upper: bool,
	suffix: String,
}

fn parse_numeral_token(token: &str, input: &str) -> Result<NumeralToken> {
	let trimmed = token.trim();
	if trimmed.is_empty() {
		return Err(TheoryError::InvalidRomanNumeral {
			input: input.to_string(),
			reason: "empty roman numeral".to_string(),
		});
	}
	let (accidental, consumed) = pitch::parse_accidental(trimmed);
	let rest = &trimmed[consumed..];
	let lower = rest.to_ascii_lowercase();

	let mut degree = None;
	let mut numeral_len = 0;
	for (tok, deg) in NUMERALS {
		if lower.starts_with(tok) {
			degree = Some(deg);
			numeral_len = tok.len();
			break;
		}
	}
	let degree = degree.ok_or_else(|| TheoryError::InvalidRomanNumeral {
		input: input.to_string(),
		reason: format!("'{rest}' does not start with a roman numeral I..VII or i..vii"),
	})?;
	let numeral_str = &rest[..numeral_len];
	let is_upper = numeral_str.chars().next().is_some_and(|c| c.is_uppercase());
	let suffix = rest[numeral_len..].to_string();

	Ok(NumeralToken {
		degree,
		accidental,
		is_upper,
		suffix,
	})
}

/// The actual diatonic triad quality (major/minor/diminished/augmented) built
/// from the key's own scale degrees (thirds stacked in the scale), as
/// opposed to the case-based convention. Used to detect degrees whose
/// diatonic triad is diminished (vii in major, ii in natural minor, ...).
fn diatonic_triad_intervals(is_minor_key: bool, degree: u32) -> &'static [u8] {
	let degrees = if is_minor_key {
		&NATURAL_MINOR_DEGREES
	} else {
		&MAJOR_DEGREES
	};
	let idx = ((degree as i32 - 1).rem_euclid(7)) as usize;
	let root = degrees[idx];
	let third_idx = (idx + 2) % 7;
	let third = degrees[third_idx] + if idx + 2 >= 7 { 12 } else { 0 };
	let fifth_idx = (idx + 4) % 7;
	let fifth = degrees[fifth_idx] + if idx + 4 >= 7 { 12 } else { 0 };
	let third_iv = (third - root).rem_euclid(12);
	let fifth_iv = (fifth - root).rem_euclid(12);
	match (third_iv, fifth_iv) {
		(3, 6) => &[0, 3, 6],
		(4, 8) => &[0, 4, 8],
		(3, 7) => &[0, 3, 7],
		_ => &[0, 4, 7],
	}
}

fn degree_root(root: PitchClass, is_minor_key: bool, degree: u32, accidental: i32) -> PitchClass {
	let degrees = if is_minor_key {
		&NATURAL_MINOR_DEGREES
	} else {
		&MAJOR_DEGREES
	};
	let idx = ((degree as i32 - 1).rem_euclid(7)) as usize;
	PitchClass::from_semitone(root.0 as i32 + degrees[idx] + accidental)
}

/// Parses a single roman numeral such as `"vi"`, `"bVII"`, `"V7/V"` against
/// `key`. Minor keys are analyzed as natural minor; secondary dominants
/// (`"X/Y"`) resolve `Y` in `key`, then `X` as if `Y` were a major tonic.
///
/// A lowercase numeral with no explicit quality follows the case-based
/// convention (minor triad), except when the key's own diatonic triad at
/// that degree is actually diminished (`vii` in a major key, `ii` in a
/// natural-minor key, ...): then the real diminished triad is used, and a
/// bare `"7"` on that degree yields a half-diminished seventh (`m7b5`),
/// e.g. `"vii7"` in C major or `"ii7"` in A minor is B\u{f8}7 / Bm7b5.
pub fn parse_roman(input: &str, key: &Scale) -> Result<Chord> {
	let trimmed = input.trim();
	let is_minor_key = !matches!(key.kind, ScaleKind::Major);

	let (main_part, target_part) = match trimmed.split_once('/') {
		Some((main, target)) => (main, Some(target)),
		None => (trimmed, None),
	};

	let main = parse_numeral_token(main_part, trimmed)?;

	let chord_root = if let Some(target_part) = target_part {
		let target = parse_numeral_token(target_part, trimmed)?;
		let target_root = degree_root(key.root, is_minor_key, target.degree, target.accidental);
		// Secondary numerals are resolved as if the target degree were a
		// major tonic (standard applied-chord analysis).
		degree_root(target_root, false, main.degree, main.accidental)
	} else {
		degree_root(key.root, is_minor_key, main.degree, main.accidental)
	};

	let default_base: &[u8] = if main.is_upper {
		&[0, 4, 7]
	} else {
		&[0, 3, 7]
	};
	let suffix = main.suffix.trim();

	// A lowercase numeral with no explicit quality normally defaults to a
	// minor triad (case-based convention), but when it is analyzed in a
	// primary (non-secondary) context and the key's own diatonic triad at
	// that degree is actually diminished (vii in major, ii in natural
	// minor, ...), use the real diatonic quality instead.
	let diminished_diatonic = target_part.is_none()
		&& !main.is_upper
		&& diatonic_triad_intervals(is_minor_key, main.degree) == [0, 3, 6];

	let intervals = if diminished_diatonic && (suffix.is_empty() || suffix == "7") {
		if suffix.is_empty() {
			vec![0, 3, 6]
		} else {
			vec![0, 3, 6, 10]
		}
	} else if suffix.is_empty() {
		default_base.to_vec()
	} else if suffix == "7" {
		// Bare "7" means "the diatonic seventh for this case": a dominant
		// seventh on an uppercase numeral, a minor seventh on a lowercase
		// one (e.g. "V7" is dominant, "ii7" is minor7).
		if main.is_upper {
			vec![0, 4, 7, 10]
		} else {
			vec![0, 3, 7, 10]
		}
	} else {
		let (intervals, leftover) =
			chord::parse_quality(suffix, default_base).map_err(|token| {
				TheoryError::InvalidRomanNumeral {
					input: trimmed.to_string(),
					reason: format!("unrecognized quality token '{token}'"),
				}
			})?;
		if !leftover.is_empty() {
			return Err(TheoryError::InvalidRomanNumeral {
				input: trimmed.to_string(),
				reason: format!("unexpected trailing text '{leftover}'"),
			});
		}
		intervals
	};

	Ok(Chord {
		root: chord_root,
		intervals,
		bass: None,
		symbol: trimmed.to_string(),
	})
}

/// Parses a roman-numeral progression such as `"I-V-vi-IV"` or
/// `"ii7 V7 Imaj7"`. Accepted separators: whitespace, `-`, `|`, `,`.
pub fn parse_roman_progression(input: &str, key: &Scale) -> Result<Vec<Chord>> {
	let cleaned: String = input
		.chars()
		.map(|c| if matches!(c, '-' | '|' | ',') { ' ' } else { c })
		.collect();
	let tokens: Vec<&str> = cleaned.split_whitespace().collect();
	if tokens.is_empty() {
		return Err(TheoryError::EmptyProgression);
	}
	tokens.into_iter().map(|t| parse_roman(t, key)).collect()
}
