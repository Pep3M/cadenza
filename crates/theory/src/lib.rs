//! Music-theory primitives for Cadenza's AI arranger.
//!
//! Gives the AI reliable building blocks — pitches, scales, chord symbols,
//! roman numerals and voicings — so it can say "Am7 - Dm9 - G13 - Cmaj7,
//! drop-2 voicing" or "vi-ii-V-I in C" and get correct MIDI pitches back.

pub mod chord;
pub mod error;
pub mod pattern;
pub mod pitch;
pub mod roman;
pub mod scale;
pub mod voicing;

pub use chord::Chord;
pub use error::{Result, TheoryError};
pub use pattern::{ArpDirection, CompPattern, NoteSpec, PatternOptions, progression_to_notes};
pub use pitch::{NoteName, PitchClass, format_pitch, parse_note_name, parse_pitch, pitch_class_of};
pub use roman::{parse_roman, parse_roman_progression};
pub use scale::{Scale, ScaleKind};
pub use voicing::{VoicingStyle, bass_note, voice};
