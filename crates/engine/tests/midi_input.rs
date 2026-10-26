//! An effect fed another track's MIDI (`Device.midi_input`): `pitch_correct` on a voice
//! follows the melody track's held notes and falls back on its scale between them.

mod common;

use cadenza_dsp::util::pitch::detect_pitch;
use cadenza_model::{Clip, Device, Track, TrackKind};
use common::*;

const SR: u32 = 48_000;

/// A voice-like tone at a steady `midi` pitch, `seconds` long, written as a WAV.
fn voice(dir: &std::path::Path, midi: f64, seconds: f64) -> std::path::PathBuf {
	let path = dir.join("voice.wav");
	let spec = hound::WavSpec {
		channels: 1,
		sample_rate: SR,
		bits_per_sample: 32,
		sample_format: hound::SampleFormat::Float,
	};
	let mut w = hound::WavWriter::create(&path, spec).unwrap();
	let hz = 440.0 * 2f64.powf((midi - 69.0) / 12.0);
	for n in 0..(seconds * f64::from(SR)) as usize {
		let t = n as f64 / f64::from(SR);
		let v: f64 = (1..=6)
			.map(|h| (std::f64::consts::TAU * hz * h as f64 * t).sin() / h as f64)
			.sum();
		w.write_sample((0.25 * v) as f32).unwrap();
	}
	w.finalize().unwrap();
	path
}

fn pitch(x: &[f32], from: f64, to: f64) -> f64 {
	let part = &x[(from * f64::from(SR)) as usize..(to * f64::from(SR)) as usize];
	69.0 + 12.0 * (f64::from(detect_pitch(part, SR as f32).unwrap()) / 440.0).log2()
}

#[test]
fn a_melody_track_names_the_notes_a_voice_is_tuned_to() {
	let dir = tempfile::tempdir().unwrap();
	let wav = voice(dir.path(), 61.3, 4.0);
	// The melody holds C#3 for the first two seconds (120 bpm), then nothing; it has no
	// instrument, so it is only a guide.
	let mut melody = Track::new("Melody", TrackKind::Instrument);
	melody.clips.push(
		Clip::midi("Melody", beats(0.0), beats(8.0)).with_notes([cadenza_model::Note::new(
			49,
			beats(0.0),
			beats(4.0),
			0.8,
		)]),
	);
	let mut vox = Track::new("Vox", TrackKind::Audio);
	vox.clips
		.push(Clip::audio("voice", beats(0.0), beats(8.0), &wav));
	let mut tuner = Device::builtin("pitch_correct", "Tune")
		.with_param("scale", 1.0)
		.with_param("retune", 0.0);
	tuner.midi_input = Some(melody.id);
	vox.channel.inserts.push(tuner);
	let p = project(vec![melody, vox]);
	let out = cadenza_engine::render_offline(&p, &opts(8.0)).unwrap();
	let held = pitch(&out.left, 0.5, 1.8);
	let after = pitch(&out.left, 2.5, 3.8);
	assert!((held - 61.0).abs() < 0.04, "held C#: {held}");
	assert!((after - 62.0).abs() < 0.04, "C major after it: {after}");
	// Started in the middle of the note (a seek): it is chased.
	let mut from_middle = opts(8.0);
	from_middle.start = beats(2.0);
	let out = cadenza_engine::render_offline(&p, &from_middle).unwrap();
	let held = pitch(&out.left, 0.3, 0.9);
	assert!((held - 61.0).abs() < 0.04, "chased C#: {held}");
}
