//! A note's own bend (`Note::bend`, NE-9h): played by the built-in synths, only on that
//! note, and chased when playback starts inside it.

mod common;

use cadenza_dsp::util::pitch::detect_pitch;
use cadenza_model::{Clip, Note, NoteBendPoint, Track, TrackKind};
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
