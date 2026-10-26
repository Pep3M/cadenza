//! A note's own bend (`Note::bend`, NE-9h), pressure and timbre (NE-9k): played by the
//! built-in synths, only on that note, and chased when playback starts inside it.

mod common;

use cadenza_dsp::util::pitch::detect_pitch;
use cadenza_model::{Clip, Note, NoteBendPoint, NoteValuePoint, Track, TrackKind};
use common::*;

const SR: u32 = 48_000;

fn pitch(x: &[f32], from: f64, to: f64) -> f64 {
	let part = &x[(from * f64::from(SR)) as usize..(to * f64::from(SR)) as usize];
	69.0 + 12.0 * (f64::from(detect_pitch(part, SR as f32).unwrap()) / 440.0).log2()
}

#[test]
fn a_bent_note_moves_alone_and_is_chased_bent() {
	// A3 held 4 beats (2 s at 120 bpm), bent up a fifth from its second beat; then the same
	// key again, straight.
	let mut bent = Note::new(57, beats(0.0), beats(4.0), 0.8);
	bent.bend = vec![
		NoteBendPoint {
			tick: beats(0.0),
			semitones: 0.0,
		},
		NoteBendPoint {
			tick: beats(2.0),
			semitones: 7.0,
		},
	];
	let straight = Note::new(57, beats(4.0), beats(2.0), 0.8);
	for device in ["poly_synth", "electric_piano"] {
		let mut track = Track::new("Lead", TrackKind::Instrument);
		track.instrument = Some(cadenza_model::Device::builtin(device, "Lead"));
		track.clips.push(
			Clip::midi("Lead", beats(0.0), beats(8.0)).with_notes([bent.clone(), straight.clone()]),
		);
		let p = project(vec![track]);
		let out = cadenza_engine::render_offline(&p, &opts(8.0)).unwrap();
		let before = pitch(&out.left, 0.2, 0.9);
		let after = pitch(&out.left, 1.2, 1.9);
		let again = pitch(&out.left, 2.2, 2.9);
		assert!((before - 57.0).abs() < 0.1, "{device}: {before}");
		assert!((after - 64.0).abs() < 0.1, "{device}: {after}");
		assert!(
			(again - 57.0).abs() < 0.1,
			"{device}: the next note is straight: {again}"
		);
		// Started inside the bent part: the note is re-triggered already bent.
		let mut from_middle = opts(8.0);
		from_middle.start = beats(3.0);
		let out = cadenza_engine::render_offline(&p, &from_middle).unwrap();
		let chased = pitch(&out.left, 0.05, 0.45);
		assert!((chased - 64.0).abs() < 0.1, "{device}: chased {chased}");
	}
}

/// Energy of the first difference per unit of level: how bright a stretch is.
fn brightness(x: &[f32], from: f64, to: f64) -> f64 {
	let part = &x[(from * f64::from(SR)) as usize..(to * f64::from(SR)) as usize];
	let diff: f64 = part
		.windows(2)
		.map(|w| f64::from(w[1] - w[0]).powi(2))
		.sum();
	let level: f64 = part.iter().map(|v| f64::from(*v).powi(2)).sum();
	diff / level.max(1e-12)
}

#[test]
fn a_notes_timbre_darkens_it_alone_and_is_chased() {
	// Two notes of a chord on a poly synth with a mid cutoff; the upper one's timbre drops
	// to 0 at its second beat, its pressure rises at its third.
	let mut track = Track::new("Keys", TrackKind::Instrument);
	let mut synth = cadenza_model::Device::builtin("poly_synth", "Keys");
	synth.params.insert("filter_cutoff".into(), 1500.0);
	track.instrument = Some(synth);
	let mut alone = Note::new(57, beats(0.0), beats(4.0), 0.8);
	let mut shaped = alone.clone();
	shaped.id = cadenza_core::NoteId::new();
	shaped.timbre = vec![NoteValuePoint {
		tick: beats(1.0),
		value: 0.0,
	}];
	shaped.pressure = vec![NoteValuePoint {
		tick: beats(3.0),
		value: 1.0,
	}];
	alone.pitch = 45;
	shaped.pitch = 57;
	let solo = |notes: Vec<Note>| {
		let mut t = track.clone();
		t.clips
			.push(Clip::midi("Keys", beats(0.0), beats(4.0)).with_notes(notes));
		cadenza_engine::render_offline(&project(vec![t]), &opts(4.0)).unwrap()
	};
	let out = solo(vec![shaped.clone()]);
	let open = brightness(&out.left, 0.2, 0.45);
	let dark = brightness(&out.left, 0.7, 0.95);
	let pressed = brightness(&out.left, 1.7, 1.95);
	assert!(dark < open / 1.5, "timbre 0 darkens: {open} -> {dark}");
	assert!(
		pressed > dark * 1.5,
		"pressure opens it again: {dark} -> {pressed}"
	);
	// The other note of the chord keeps its colour: rendered with and without the shaped
	// one's curves, the low note alone sounds the same.
	let plain = Note {
		timbre: Vec::new(),
		pressure: Vec::new(),
		..shaped.clone()
	};
	let with = solo(vec![alone.clone(), shaped.clone()]);
	let plain_shaped = plain.clone();
	let without = solo(vec![alone.clone(), plain]);
	let low = |x: &[f32]| pitch(x, 0.7, 0.95);
	assert!((low(&with.left) - low(&without.left)).abs() < 0.5);
	// Started inside the dark stretch: chased dark (against the same note without curves
	// chased there: both restart their filter envelope).
	let mut from_middle = opts(4.0);
	from_middle.start = beats(1.5);
	let chase = |note: Note| {
		let mut t = track.clone();
		t.clips
			.push(Clip::midi("Keys", beats(0.0), beats(4.0)).with_notes([note]));
		let out = cadenza_engine::render_offline(&project(vec![t]), &from_middle).unwrap();
		brightness(&out.left, 0.1, 0.35)
	};
	let (dark_chase, plain_chase) = (chase(shaped.clone()), chase(plain_shaped));
	assert!(
		dark_chase < plain_chase / 1.5,
		"chased {dark_chase} vs {plain_chase}"
	);
}
