//! Tests for turning a progression into note events.

use cadenza_theory::Chord;
use cadenza_theory::pattern::{ArpDirection, CompPattern, PatternOptions, progression_to_notes};
use cadenza_theory::voicing::VoicingStyle;

#[test]
fn sustain_pattern_emits_one_note_per_voice_plus_bass() {
	let chords = vec![(Chord::parse("C").unwrap(), 4.0)];
	let options = PatternOptions {
		include_bass: true,
		..Default::default()
	};
	let notes = progression_to_notes(&chords, VoicingStyle::Close, CompPattern::Sustain, &options);
	// 3 voiced notes + 1 bass note.
	assert_eq!(notes.len(), 4);
	for note in &notes {
		assert_eq!(note.start_beats, 0.0);
		assert_eq!(note.length_beats, 4.0);
	}
}

#[test]
fn pulse_pattern_repeats_within_the_duration() {
	let chords = vec![(Chord::parse("C").unwrap(), 2.0)];
	let options = PatternOptions {
		include_bass: false,
		..Default::default()
	};
	let notes = progression_to_notes(
		&chords,
		VoicingStyle::Close,
		CompPattern::Pulse { rate_beats: 0.5 },
		&options,
	);
	// 2 beats / 0.5 beat rate = 4 pulses * 3 notes = 12.
	assert_eq!(notes.len(), 12);
}

#[test]
fn arpeggio_cycles_through_voiced_pitches() {
	let chords = vec![(Chord::parse("C").unwrap(), 2.0)];
	let options = PatternOptions {
		include_bass: false,
		..Default::default()
	};
	let notes = progression_to_notes(
		&chords,
		VoicingStyle::Close,
		CompPattern::Arpeggio {
			direction: ArpDirection::Up,
			rate_beats: 0.5,
		},
		&options,
	);
	assert_eq!(notes.len(), 4); // 2 beats / 0.5 beat rate
	let mut starts: Vec<f64> = notes.iter().map(|n| n.start_beats).collect();
	starts.sort_by(|a, b| a.partial_cmp(b).unwrap());
	assert_eq!(starts, vec![0.0, 0.5, 1.0, 1.5]);
}

#[test]
fn progression_advances_the_cursor_between_chords() {
	let chords = vec![
		(Chord::parse("C").unwrap(), 2.0),
		(Chord::parse("G").unwrap(), 2.0),
	];
	let options = PatternOptions {
		include_bass: false,
		..Default::default()
	};
	let notes = progression_to_notes(&chords, VoicingStyle::Close, CompPattern::Sustain, &options);
	let second_chord_starts: Vec<f64> = notes
		.iter()
		.filter(|n| n.start_beats >= 2.0)
		.map(|n| n.start_beats)
		.collect();
	assert_eq!(second_chord_starts.len(), 3);
	assert!(second_chord_starts.iter().all(|&s| s == 2.0));
}
