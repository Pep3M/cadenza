//! Voicing and voice-leading sanity tests.

use cadenza_theory::voicing::{VoicingStyle, bass_note, voice};
use cadenza_theory::{Chord, PitchClass};

#[test]
fn close_voicing_stays_near_center() {
	let c = Chord::parse("C").unwrap();
	let voiced = voice(&c, VoicingStyle::Close, 60, None);
	assert_eq!(voiced.len(), 3);
	for &p in &voiced {
		assert!((50..=72).contains(&p), "pitch {p} far from center");
	}
}

#[test]
fn shell_voicing_keeps_root_third_and_seventh() {
	let cmaj7 = Chord::parse("Cmaj7").unwrap();
	let voiced = voice(&cmaj7, VoicingStyle::Shell, 60, None);
	assert_eq!(voiced.len(), 3);
	let classes: Vec<u8> = voiced.iter().map(|&p| p % 12).collect();
	assert!(classes.contains(&0)); // root
	assert!(classes.contains(&4)); // third
	assert!(classes.contains(&11)); // seventh
}

#[test]
fn rootless_voicing_omits_the_root_when_extensions_exist() {
	let cm9 = Chord::parse("Cm9").unwrap();
	let voiced = voice(&cm9, VoicingStyle::Rootless, 60, None);
	let classes: Vec<u8> = voiced.iter().map(|&p| p % 12).collect();
	assert!(!classes.contains(&0));
}

#[test]
fn drop2_moves_the_second_highest_tone_down_an_octave() {
	let cmaj7 = Chord::parse("Cmaj7").unwrap();
	let close = voice(&cmaj7, VoicingStyle::Close, 60, None);
	let drop2 = voice(&cmaj7, VoicingStyle::Drop2, 60, None);
	assert_eq!(close.len(), drop2.len());
	// Drop2 should span a wider range than close position.
	let close_span = close.last().unwrap() - close.first().unwrap();
	let drop2_span = drop2.last().unwrap() - drop2.first().unwrap();
	assert!(drop2_span >= close_span);
}

#[test]
fn bass_note_respects_slash_chords() {
	let am7_over_g = Chord::parse("Am7/G").unwrap();
	assert_eq!(bass_note(&am7_over_g, 2) % 12, 7); // G
	let plain_c = Chord::parse("C").unwrap();
	assert_eq!(bass_note(&plain_c, 2) % 12, 0); // root C
}

#[test]
fn voice_leading_minimizes_movement_across_a_progression() {
	let progression = ["C", "F", "G", "C"];
	let chords: Vec<Chord> = progression
		.iter()
		.map(|s| Chord::parse(s).unwrap())
		.collect();

	let mut previous: Option<Vec<u8>> = None;
	let mut total_movement: i64 = 0;
	let mut voicings = Vec::new();
	for chord in &chords {
		let voiced = voice(chord, VoicingStyle::Close, 60, previous.as_deref());
		if let Some(prev) = &previous {
			let mut a = prev.clone();
			let mut b = voiced.clone();
			a.sort_unstable();
			b.sort_unstable();
			let n = a.len().max(b.len());
			for i in 0..n {
				let pa = *a.get(i).unwrap_or_else(|| a.last().unwrap()) as i64;
				let pb = *b.get(i).unwrap_or_else(|| b.last().unwrap()) as i64;
				total_movement += (pa - pb).abs();
			}
		}
		voicings.push(voiced.clone());
		previous = Some(voiced);
	}

	// Three transitions (C->F, F->G, G->C) in close position with voice
	// leading should stay well under a "jump every note by an octave"
	// worst case (3 transitions * 3 notes * 12 semitones = 108).
	assert!(
		total_movement < 30,
		"total movement was {total_movement}: {voicings:?}"
	);
}

#[test]
fn voicing_of_empty_chord_is_empty() {
	let empty = Chord::new(PitchClass(0), vec![], None);
	assert!(voice(&empty, VoicingStyle::Close, 60, None).is_empty());
}
