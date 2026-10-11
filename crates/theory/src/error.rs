//! Errors returned by `cadenza-theory`.
//!
//! Every message is written to be read by an LLM: it names the offending
//! input and lists accepted forms, so the model can retry with a corrected
//! argument instead of giving up.

use thiserror::Error;

/// All errors produced while parsing or manipulating music-theory data.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum TheoryError {
	#[error(
		"invalid note name '{input}': {reason}. Accepted forms: a letter A-G, an optional \
		 accidental (# or b, repeatable), and an octave number (may be negative), e.g. \"C4\", \
		 \"F#3\", \"Bb2\", \"Cb4\", \"E#4\", \"C-1\"; a bare MIDI number such as \"60\" is also \
		 accepted"
	)]
	InvalidNoteName { input: String, reason: String },

	#[error("pitch {pitch} is out of the MIDI range 0..=127")]
	PitchOutOfRange { pitch: i32 },

	#[error(
		"invalid pitch class '{input}': expected a letter A-G optionally followed by # or b, \
		 e.g. \"C\", \"F#\", \"Bb\""
	)]
	InvalidPitchClass { input: String },

	#[error(
		"invalid scale '{input}': expected \"<root> <kind>\", e.g. \"C major\", \"A minor\", \
		 \"F# dorian\", \"Eb harmonic minor\". Known kinds: major, minor (natural minor), \
		 harmonic minor, melodic minor, dorian, phrygian, lydian, mixolydian, locrian, major \
		 pentatonic, minor pentatonic, blues, whole tone, chromatic"
	)]
	InvalidScale { input: String },

	#[error(
		"invalid chord symbol '{symbol}': {reason}. Examples: C, Cm, Cdim, Caug, Csus4, C7, \
		 Cmaj7, Cm7, Cm7b5, Cdim7, C9, Cm9, C13, Cadd9, Csus2, C/E, Am7/G"
	)]
	InvalidChord { symbol: String, reason: String },

	#[error(
		"unrecognized token '{token}' in chord symbol '{symbol}'. Accepted quality tokens \
		 include: m/min/-, dim/\u{b0}, aug/+, sus2, sus4, 5, 6, m6, 6/9, 7, maj7/M7/\u{394}7, \
		 m7/min7/-7, m(maj7), dim7, m7b5/\u{f8}, 7sus4, aug7, 9, maj9, m9, 11, maj11, m11, 13, \
		 maj13, m13, add9, add11, add13, add6, add2, no3, no5, b5, #5, b9, #9, #11, b13, and \
		 a trailing \"/<bass note>\""
	)]
	UnrecognizedChordToken { symbol: String, token: String },

	#[error(
		"invalid roman numeral '{input}': {reason}. Accepted forms: I..VII or i..vii, optional \
		 leading accidental (b/#), optional quality suffix (7, maj7, m7, \u{f8}7, \u{b0}7, \
		 sus4, 9...), optional secondary dominant \"X/Y\" (e.g. \"V7/V\")"
	)]
	InvalidRomanNumeral { input: String, reason: String },

	#[error(
		"empty chord progression: provide at least one chord or roman numeral, separated by \
		 spaces, '-', '|' or ','"
	)]
	EmptyProgression,

	#[error("voicing requested for a chord with no pitch classes")]
	EmptyChord,
}

pub type Result<T> = std::result::Result<T, TheoryError>;
