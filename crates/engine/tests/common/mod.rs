//! Shared builders for the engine integration tests.
#![allow(dead_code)]

use std::path::PathBuf;

use cadenza_core::{PPQ, Tick};
use cadenza_engine::{RenderOptions, StereoBuffer};
use cadenza_model::{Clip, Device, Note, Project, Send, Track, TrackKind};

pub fn beats(b: f64) -> Tick {
	Tick::from_beats(b)
}

pub fn synth() -> Device {
	Device::builtin("poly_synth", "Synth")
		.with_param("amp_attack", 0.0)
		.with_param("amp_sustain", 100.0)
		.with_param("gain", -12.0)
}

/// Instrument track with one clip of sustained notes `(pitch, start beat, length beats)`.
pub fn synth_track(name: &str, notes: &[(u8, f64, f64)], clip_beats: f64) -> Track {
	instrument_track(name, synth(), notes, clip_beats)
}

pub fn instrument_track(
	name: &str,
	device: Device,
	notes: &[(u8, f64, f64)],
	clip_beats: f64,
) -> Track {
	let mut t = Track::new(name, TrackKind::Instrument).with_instrument(device);
	t.clips.push(
		Clip::midi(format!("{name} clip"), Tick::ZERO, beats(clip_beats)).with_notes(
			notes
				.iter()
				.map(|&(p, s, l)| Note::new(p, beats(s), beats(l), 0.9)),
		),
	);
	t
}

pub fn bus(name: &str) -> Track {
	Track::new(name, TrackKind::Bus)
}

pub fn send(target: &Track, level_db: f32, pre_fader: bool) -> Send {
	let mut s = Send::new(target.id, level_db);
	s.pre_fader = pre_fader;
	s
}

pub fn project(tracks: Vec<Track>) -> Project {
	let mut p = Project::new("test");
	p.tracks = tracks;
	p
}

/// Options rendering exactly `[0, end_beats)` at 48 kHz.
pub fn opts(end_beats: f64) -> RenderOptions {
	RenderOptions {
		end: Some(beats(end_beats)),
		..RenderOptions::default()
	}
}

pub fn rms(x: &[f32]) -> f64 {
	if x.is_empty() {
		return 0.0;
	}
	(x.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / x.len() as f64).sqrt()
}

pub fn peak(x: &[f32]) -> f32 {
	x.iter().fold(0.0f32, |m, v| m.max(v.abs()))
}

pub fn max_diff(a: &StereoBuffer, b: &StereoBuffer) -> f32 {
	assert_eq!(a.len(), b.len());
	a.left
		.iter()
		.zip(&b.left)
		.chain(a.right.iter().zip(&b.right))
		.fold(0.0f32, |m, (x, y)| m.max((x - y).abs()))
}

pub fn assert_finite(buf: &StereoBuffer) {
	assert!(buf.left.iter().chain(&buf.right).all(|x| x.is_finite()));
}

pub fn tmp_path(name: &str) -> PathBuf {
	PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name)
}

/// Writes a sine WAV fixture and returns its path.
pub fn write_sine_wav(
	name: &str,
	sample_rate: u32,
	channels: u16,
	(format, bits): (hound::SampleFormat, u16),
	freq: f64,
	amplitude: f64,
	seconds: f64,
) -> PathBuf {
	let path = tmp_path(name);
	let spec = hound::WavSpec {
		channels,
		sample_rate,
		bits_per_sample: bits,
		sample_format: format,
	};
	let mut w = hound::WavWriter::create(&path, spec).unwrap();
	let frames = (sample_rate as f64 * seconds) as usize;
	for i in 0..frames {
		let v =
			amplitude * (2.0 * std::f64::consts::PI * freq * i as f64 / sample_rate as f64).sin();
		for _ in 0..channels {
			match format {
				hound::SampleFormat::Float => w.write_sample(v as f32).unwrap(),
				hound::SampleFormat::Int => {
					let max = ((1i64 << (bits - 1)) - 1) as f64;
					w.write_sample((v * max).round() as i32).unwrap()
				}
			}
		}
	}
	w.finalize().unwrap();
	path
}

pub const PPQ_I: i64 = PPQ;

/// 8-bar mini song: drums, bass, chords (with automation), reverb send bus, master limiter.
pub fn mini_song() -> Project {
	use cadenza_core::ParamAddress;
	use cadenza_model::{AutomationLane, AutomationPoint};

	let bars = 8;
	let mut drum_notes = Vec::new();
	for bar in 0..bars {
		let b = bar as f64 * 4.0;
		drum_notes.extend([
			(36, b, 0.5),
			(36, b + 2.5, 0.5),
			(38, b + 1.0, 0.5),
			(38, b + 3.0, 0.5),
		]);
		for eighth in 0..8 {
			drum_notes.push((42, b + eighth as f64 * 0.5, 0.25));
		}
	}
	let mut drums = instrument_track(
		"Drums",
		Device::builtin("drum_machine", "Kit"),
		&drum_notes,
		bars as f64 * 4.0,
	);
	drums.channel.volume_db = -4.0;

	let roots = [45u8, 41, 48, 43];
	let bass_notes: Vec<(u8, f64, f64)> = (0..bars)
		.flat_map(|bar| {
			let root = roots[bar % 4] - 12;
			(0..4).map(move |q| (root, bar as f64 * 4.0 + q as f64, 0.9))
		})
		.collect();
	let mut bass = instrument_track(
		"Bass",
		Device::builtin("poly_synth", "Bass")
			.with_param("filter_cutoff", 600.0)
			.with_param("gain", -6.0),
		&bass_notes,
		bars as f64 * 4.0,
	);
	bass.channel
		.inserts
		.push(Device::builtin("compressor", "Comp").with_param("threshold", -18.0));

	let chord_notes: Vec<(u8, f64, f64)> = (0..bars)
		.flat_map(|bar| {
			let root = roots[bar % 4] + 12;
			[0u8, 3, 7].map(move |i| (root + i, bar as f64 * 4.0, 3.8))
		})
		.collect();
	let reverb = {
		let mut b = bus("Reverb");
		b.channel.inserts.push(
			Device::builtin("reverb", "Hall")
				.with_param("mix", 100.0)
				.with_param("decay", 2.0),
		);
		b
	};
	let mut chords = instrument_track(
		"Chords",
		Device::builtin("electric_piano", "Keys"),
		&chord_notes,
		bars as f64 * 4.0,
	);
	chords.channel.volume_db = -8.0;
	chords.channel.pan = -0.3;
	chords.channel.sends.push(send(&reverb, -6.0, false));
	drums.channel.sends.push(send(&reverb, -18.0, false));

	let chords_id = chords.id;
	let mut p = project(vec![drums, bass, chords, reverb]);
	p.master.inserts.push(
		Device::builtin("limiter", "Limiter")
			.with_param("input_gain", 4.0)
			.with_param("ceiling", -1.0),
	);
	p.automation.push(AutomationLane::new(
		ParamAddress::track_volume(chords_id),
		vec![
			AutomationPoint::new(beats(0.0), -20.0),
			AutomationPoint::new(beats(8.0), -8.0),
		],
	));
	p
}
