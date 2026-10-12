//! End-to-end checks of the `cadenza` binary's file commands.

use std::path::Path;
use std::process::Command;

use cadenza_core::Tick;
use cadenza_model::{Clip, Device, Note, Project, Track, TrackKind};

fn cadenza(args: &[&str]) -> std::process::Output {
	Command::new(env!("CARGO_BIN_EXE_cadenza"))
		.args(args)
		.env("CADENZA_PLUGIN_HOSTING", "in-process")
		.output()
		.unwrap()
}

fn song(path: &Path) {
	let mut p = Project::new("Song");
	for (name, pitch) in [("Keys", 60), ("Bass", 36)] {
		let mut t = Track::new(name, TrackKind::Instrument)
			.with_instrument(Device::builtin("poly_synth", "Synth"));
		t.clips.push(
			Clip::midi("c", Tick::ZERO, Tick::from_beats(4.0)).with_notes((0..4).map(|i| {
				Note::new(
					pitch + i,
					Tick::from_beats(f64::from(i)),
					Tick::from_beats(1.0),
					0.8,
				)
			})),
		);
		p.tracks.push(t);
	}
	p.save_json(path).unwrap();
}

fn s(p: &Path) -> &str {
	p.to_str().unwrap()
}

#[test]
fn midi_export_and_import_round_trip() {
	let dir = tempfile::tempdir().unwrap();
	let project = dir.path().join("song.cadenza.json");
	song(&project);
	let mid = dir.path().join("song.mid");
	let out = cadenza(&["export-midi", s(&project), "-o", s(&mid)]);
	assert!(
		out.status.success(),
		"{}",
		String::from_utf8_lossy(&out.stderr)
	);
	let bass = dir.path().join("bass.mid");
	let out = cadenza(&[
		"export-midi",
		s(&project),
		"-o",
		s(&bass),
		"--track",
		"bass",
	]);
	assert!(
		out.status.success(),
		"{}",
		String::from_utf8_lossy(&out.stderr)
	);

	let imported = dir.path().join("imported.cadenza.json");
	let out = cadenza(&["import-midi", s(&mid), "-o", s(&imported)]);
	assert!(
		out.status.success(),
		"{}",
		String::from_utf8_lossy(&out.stderr)
	);
	let p = Project::load_json(&imported).unwrap();
	assert_eq!(p.tracks.len(), 2);
	assert!(p.tracks.iter().all(|t| t.clips[0].notes().len() == 4));

	// Into an existing project's track, in place.
	let out = cadenza(&[
		"import-midi",
		s(&bass),
		"--into",
		s(&imported),
		"--track",
		"Keys",
		"--at-beats",
		"8",
	]);
	assert!(
		out.status.success(),
		"{}",
		String::from_utf8_lossy(&out.stderr)
	);
	let p = Project::load_json(&imported).unwrap();
	let keys = p.tracks.iter().find(|t| t.name == "Keys").unwrap();
	assert_eq!(keys.clips.len(), 2);
	assert_eq!(keys.clips[1].start, Tick::from_beats(8.0));

	let out = cadenza(&["import-midi", s(&mid)]);
	assert!(!out.status.success(), "-o or --into is required");
	let out = cadenza(&["export-midi", s(&project), "-o", s(&mid), "--track", "Nope"]);
	assert!(String::from_utf8_lossy(&out.stderr).contains("no track named 'Nope'"));
}

#[test]
fn render_writes_flac_and_16_bit_wav() {
	let dir = tempfile::tempdir().unwrap();
	let project = dir.path().join("song.cadenza.json");
	song(&project);
	for (name, extra) in [("mix.flac", vec![]), ("mix.wav", vec!["--bit-depth", "16"])] {
		let path = dir.path().join(name);
		let mut args = vec!["render", s(&project), "-o", s(&path)];
		args.extend(extra);
		let out = cadenza(&args);
		assert!(
			out.status.success(),
			"{}",
			String::from_utf8_lossy(&out.stderr)
		);
		let info = cadenza_audio_io::probe(&path).unwrap();
		assert_eq!(info.channels, 2, "{name}");
	}
	let bad = dir.path().join("mix.wav");
	let out = cadenza(&["render", s(&project), "-o", s(&bad), "--bit-depth", "12"]);
	assert!(String::from_utf8_lossy(&out.stderr).contains("16, 24 or 32"));
}
