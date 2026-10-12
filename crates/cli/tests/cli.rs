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

/// Answers OpenAI-style chat completions from a script: one JSON body per request.
fn mock_completions(bodies: Vec<String>) -> String {
	use std::io::{Read, Write};
	let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
	let addr = listener.local_addr().unwrap();
	std::thread::spawn(move || {
		for body in bodies {
			let Ok((mut socket, _)) = listener.accept() else {
				return;
			};
			let mut buf = Vec::new();
			let mut chunk = [0u8; 8192];
			loop {
				let n = socket.read(&mut chunk).unwrap_or(0);
				if n == 0 {
					break;
				}
				buf.extend_from_slice(&chunk[..n]);
				let text = String::from_utf8_lossy(&buf);
				if let Some(end) = text.find("\r\n\r\n") {
					let length: usize = text[..end]
						.lines()
						.find_map(|l| {
							let (k, v) = l.split_once(':')?;
							k.eq_ignore_ascii_case("content-length")
								.then(|| v.trim().parse().ok())?
						})
						.unwrap_or(0);
					if buf.len() >= end + 4 + length {
						break;
					}
				}
			}
			let reply = format!(
				"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
				body.len()
			);
			let _ = socket.write_all(reply.as_bytes());
		}
	});
	format!("http://{addr}/v1")
}

#[test]
fn chat_applies_one_request_and_saves_the_project() {
	let dir = tempfile::tempdir().unwrap();
	let path = dir.path().join("song.cadenza.json");
	song(&path);
	let tool = serde_json::json!({
		"id": "a", "model": "local",
		"choices": [{"index": 0, "finish_reason": "tool_calls", "message": {"role": "assistant",
			"content": "Marking the bridge.",
			"tool_calls": [{"id": "c1", "type": "function", "function": {"name": "add_marker",
				"arguments": "{\"beat\":8,\"name\":\"Bridge\"}"}}]}}],
		"usage": {"prompt_tokens": 100, "completion_tokens": 10}
	});
	let done = serde_json::json!({
		"id": "b", "model": "local",
		"choices": [{"index": 0, "finish_reason": "stop", "message": {"role": "assistant",
			"content": "Added a Bridge marker at beat 8."}}],
		"usage": {"prompt_tokens": 120, "completion_tokens": 10}
	});
	let url = mock_completions(vec![tool.to_string(), done.to_string()]);
	let out = Command::new(env!("CARGO_BIN_EXE_cadenza"))
		.args([
			"chat",
			s(&path),
			"add a bridge marker at beat 8",
			"--provider",
			"openai-compatible",
			"--base-url",
			&url,
			"--model",
			"local",
			"--reasoning-effort",
			"none",
		])
		.env("XDG_STATE_HOME", dir.path().join("state"))
		.env_remove("OPENAI_API_KEY")
		.output()
		.unwrap();
	let stderr = String::from_utf8_lossy(&out.stderr);
	assert!(out.status.success(), "{stderr}");
	assert!(String::from_utf8_lossy(&out.stdout).contains("Bridge marker"));
	assert!(stderr.contains("transcript:"), "{stderr}");
	let saved = Project::load_json(&path).unwrap();
	assert_eq!(saved.markers.len(), 1);
	assert_eq!(saved.markers[0].name, "Bridge");

	let out = cadenza(&["chat", s(&path), "x", "--provider", "openai-compatible"]);
	assert!(!out.status.success());
	assert!(String::from_utf8_lossy(&out.stderr).contains("--base-url"));
}

#[test]
fn collect_pack_and_unpack_make_a_portable_project() {
	let dir = tempfile::tempdir().unwrap();
	let take = dir.path().join("external").join("take.wav");
	std::fs::create_dir_all(take.parent().unwrap()).unwrap();
	let buf =
		cadenza_audio_io::AudioBuffer::from_planar(48_000, vec![vec![0.25f32; 4800]]).unwrap();
	cadenza_audio_io::export(
		&buf,
		&take,
		&cadenza_audio_io::ExportOptions::wav(cadenza_audio_io::WavSampleFormat::Int16),
	)
	.unwrap();
	let project = dir.path().join("Song").join("Song.cadenza.json");
	let mut p = Project::new("Song");
	let mut t = Track::new("Vox", TrackKind::Audio);
	t.clips.push(Clip::audio(
		"take",
		Tick::ZERO,
		Tick::from_beats(1.0),
		&take,
	));
	p.tracks.push(t);
	p.save_json(&project).unwrap();

	let out = cadenza(&["collect", s(&project)]);
	assert!(
		out.status.success(),
		"{}",
		String::from_utf8_lossy(&out.stderr)
	);
	let report: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
	assert_eq!(report["copied"].as_array().unwrap().len(), 1);
	let json = std::fs::read_to_string(&project).unwrap();
	assert!(json.contains("\"Audio/take.wav\""), "{json}");

	let archive = dir.path().join("Song.cadenza.zip");
	let out = cadenza(&["pack", s(&project), "-o", s(&archive)]);
	assert!(
		out.status.success(),
		"{}",
		String::from_utf8_lossy(&out.stderr)
	);
	let dest = dir.path().join("elsewhere");
	let out = cadenza(&["unpack", s(&archive), s(&dest)]);
	assert!(
		out.status.success(),
		"{}",
		String::from_utf8_lossy(&out.stderr)
	);
	let opened = String::from_utf8(out.stdout).unwrap();
	let loaded = Project::load_json(opened.trim()).unwrap();
	let file = loaded.file_refs()[0].path.clone();
	assert_eq!(file, dest.join("Song/Audio/take.wav"));
	assert!(file.is_file());
}
