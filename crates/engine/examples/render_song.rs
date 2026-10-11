//! Builds a short song through undoable `Document` commands (exactly as the AI would),
//! renders it to `target/engine_demo.wav` and prints the mix report as JSON.
//!
//! Run with: `cargo run --release -p cadenza-engine --example render_song`

use std::path::PathBuf;
use std::time::Instant;

use cadenza_core::{ParamAddress, Tick};
use cadenza_engine::{RenderOptions, render_offline, render_to_wav};
use cadenza_model::{
	AutomationPoint, ChannelPatch, Clip, Command, Device, Document, Marker, Note, Project, Send,
	Track, TrackKind,
};

fn beats(b: f64) -> Tick {
	Tick::from_beats(b)
}

fn midi_clip(name: &str, bars: u32, notes: impl IntoIterator<Item = (u8, f64, f64, f32)>) -> Clip {
	Clip::midi(name, Tick::ZERO, beats(bars as f64 * 4.0)).with_notes(
		notes
			.into_iter()
			.map(|(pitch, start, len, vel)| Note::new(pitch, beats(start), beats(len), vel)),
	)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
	let bars = 16u32;
	let mut doc = Document::new(Project::new("Engine demo"));
	doc.execute(Command::SetTempo {
		tick: Tick::ZERO,
		bpm: 104.0,
	})?;

	// Progression: Am - F - C - G (one bar each).
	let roots = [57u8, 53, 48, 55];
	let minor = [true, false, false, false];

	let drums = Track::new("Drums", TrackKind::Instrument)
		.with_instrument(Device::builtin("drum_machine", "Kit").with_param("kick_decay", 450.0));
	let bass = Track::new("Bass", TrackKind::Instrument).with_instrument(
		Device::builtin("poly_synth", "Sub Bass")
			.with_param("filter_cutoff", 500.0)
			.with_param("amp_release", 80.0)
			.with_param("gain", -8.0),
	);
	let keys = Track::new("Keys", TrackKind::Instrument)
		.with_instrument(Device::builtin("electric_piano", "Rhodes"));
	let reverb = Track::new("Reverb", TrackKind::Bus);
	let (drums_id, bass_id, keys_id, reverb_id) = (drums.id, bass.id, keys.id, reverb.id);

	let drum_notes = (0..bars).flat_map(|bar| {
		let b = bar as f64 * 4.0;
		let mut n = vec![
			(36, b, 0.5, 1.0),
			(36, b + 2.5, 0.5, 0.8),
			(38, b + 1.0, 0.5, 0.9),
			(38, b + 3.0, 0.5, 0.9),
		];
		n.extend((0..8).map(|e| {
			(
				42,
				b + e as f64 * 0.5,
				0.25,
				if e % 2 == 0 { 0.8 } else { 0.55 },
			)
		}));
		n
	});
	let bass_notes = (0..bars).flat_map(|bar| {
		let root = roots[bar as usize % 4] - 24;
		let b = bar as f64 * 4.0;
		[
			(root, b, 1.5, 0.9),
			(root, b + 1.5, 0.5, 0.7),
			(root + 7, b + 2.0, 1.0, 0.8),
			(root, b + 3.0, 0.9, 0.8),
		]
	});
	let chord_notes = (0..bars).flat_map(|bar| {
		let i = bar as usize % 4;
		let root = roots[i];
		let third = if minor[i] { 3 } else { 4 };
		[0u8, third, 7, 12].map(|iv| (root + iv, bar as f64 * 4.0, 3.75, 0.6))
	});

	doc.execute(Command::Batch {
		label: "Create arrangement".into(),
		commands: vec![
			Command::AddTrack {
				track: drums,
				index: None,
			},
			Command::AddTrack {
				track: bass,
				index: None,
			},
			Command::AddTrack {
				track: keys,
				index: None,
			},
			Command::AddTrack {
				track: reverb,
				index: None,
			},
			Command::AddClip {
				track: drums_id,
				clip: midi_clip("Beat", bars, drum_notes),
			},
			Command::AddClip {
				track: bass_id,
				clip: midi_clip("Bassline", bars, bass_notes),
			},
			Command::AddClip {
				track: keys_id,
				clip: midi_clip("Chords", bars, chord_notes),
			},
			Command::AddMarker {
				marker: Marker::new(Tick::ZERO, "Verse"),
			},
			Command::AddMarker {
				marker: Marker::new(beats(32.0), "Chorus"),
			},
		],
	})?;

	doc.execute(Command::Batch {
		label: "Mix".into(),
		commands: vec![
			Command::InsertDevice {
				track: Some(reverb_id),
				device: Device::builtin("reverb", "Hall")
					.with_param("mix", 100.0)
					.with_param("decay", 2.5),
				index: None,
			},
			Command::InsertDevice {
				track: Some(bass_id),
				device: Device::builtin("compressor", "Bass Comp").with_param("threshold", -20.0),
				index: None,
			},
			Command::AddSend {
				track: keys_id,
				send: Send::new(reverb_id, -8.0),
				index: None,
			},
			Command::AddSend {
				track: drums_id,
				send: Send::new(reverb_id, -20.0),
				index: None,
			},
			Command::SetChannel {
				track: Some(keys_id),
				patch: ChannelPatch {
					volume_db: Some(-9.0),
					pan: Some(-0.25),
					..ChannelPatch::default()
				},
			},
			Command::SetChannel {
				track: Some(drums_id),
				patch: ChannelPatch {
					volume_db: Some(-5.0),
					..ChannelPatch::default()
				},
			},
			Command::SetAutomation {
				address: ParamAddress::track_volume(keys_id),
				points: vec![
					AutomationPoint::new(beats(0.0), -18.0),
					AutomationPoint::new(beats(16.0), -9.0),
				],
			},
			Command::InsertDevice {
				track: None,
				device: Device::builtin("limiter", "Master Limiter")
					.with_param("input_gain", 4.0)
					.with_param("ceiling", -1.0),
				index: None,
			},
		],
	})?;

	let project = doc.project();
	let target = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target");
	let path = target
		.canonicalize()
		.unwrap_or(target)
		.join("engine_demo.wav");

	let started = Instant::now();
	let audio = render_offline(project, &RenderOptions::default())?;
	let render_only = started.elapsed().as_secs_f64();

	let started = Instant::now();
	let report = render_to_wav(project, &RenderOptions::default(), &path)?;
	let elapsed = started.elapsed().as_secs_f64();

	println!("{}", serde_json::to_string_pretty(&report)?);
	eprintln!(
		"render only: {:.1} s of audio in {:.3} s = {:.0}x realtime",
		audio.duration_seconds(),
		render_only,
		audio.duration_seconds() / render_only
	);
	eprintln!(
		"render + WAV + analysis: {:.3} s = {:.0}x realtime -> {}",
		elapsed,
		report.duration_seconds / elapsed,
		path.display()
	);
	Ok(())
}
