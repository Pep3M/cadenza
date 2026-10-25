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
	let project = dir.path().join("song.cdz");
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

	let imported = dir.path().join("imported.cdz");
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
	let project = dir.path().join("song.cdz");
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
	let path = dir.path().join("song.cdz");
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
fn mix_runs_an_engineer_pass_and_prints_what_changed() {
	let dir = tempfile::tempdir().unwrap();
	let path = dir.path().join("song.cdz");
	song(&path);
	let reference = dir.path().join("ref.wav");
	let tone = cadenza_audio_io::AudioBuffer::from_planar(
		48_000,
		vec![
			(0..96_000)
				.map(|n| (n as f32 * 0.05).sin() * 0.5)
				.collect::<Vec<f32>>();
			2
		],
	)
	.unwrap();
	cadenza_audio_io::export(
		&tone,
		&reference,
		&cadenza_audio_io::ExportOptions::wav(cadenza_audio_io::WavSampleFormat::Float32),
	)
	.unwrap();
	let reply = |content: &str, calls: Option<serde_json::Value>| {
		let mut message = serde_json::json!({"role": "assistant", "content": content});
		if let Some(calls) = calls {
			message["tool_calls"] = calls;
		}
		serde_json::json!({
			"id": "x", "model": "local",
			"choices": [{"index": 0, "finish_reason": "stop", "message": message}],
			"usage": {"prompt_tokens": 100, "completion_tokens": 10}
		})
		.to_string()
	};
	let limiter = serde_json::json!([{"id": "c1", "type": "function", "function": {
		"name": "apply_mix_moves",
		"arguments": "{\"moves\":[{\"move\":\"limiter\",\"ceiling_db\":-1.5,\"input_gain_db\":6}]}"}}]);
	let mix = |url: &str, extra: &[&str]| {
		let mut args = vec![
			"mix",
			s(&path),
			"--target-lufs",
			"-12",
			"--reference",
			s(&reference),
			"--provider",
			"openai-compatible",
			"--base-url",
			url,
			"--model",
			"local",
			"--reasoning-effort",
			"none",
		];
		args.extend(extra);
		Command::new(env!("CARGO_BIN_EXE_cadenza"))
			.args(&args)
			.env("XDG_STATE_HOME", dir.path().join("state"))
			.env("CADENZA_LIBRARY_DIR", dir.path().join("library"))
			.env_remove("OPENAI_API_KEY")
			.output()
			.unwrap()
	};

	// A dry run changes nothing on disk.
	let before = std::fs::read_to_string(&path).unwrap();
	let url = mock_completions(vec![
		reply("Limiting the master.", Some(limiter.clone())),
		reply("Done: louder, peaks under control.", None),
	]);
	let out = mix(&url, &["--dry-run"]);
	let stderr = String::from_utf8_lossy(&out.stderr);
	assert!(out.status.success(), "{stderr}");
	for needle in ["before:", "after:", "changes:", "Loudness", "dry run"] {
		assert!(stderr.contains(needle), "missing {needle}: {stderr}");
	}
	assert_eq!(std::fs::read_to_string(&path).unwrap(), before);

	let url = mock_completions(vec![
		reply("Limiting the master.", Some(limiter)),
		reply("Done.", None),
	]);
	let out = mix(&url, &[]);
	let stderr = String::from_utf8_lossy(&out.stderr);
	assert!(out.status.success(), "{stderr}");
	assert!(stderr.contains("saved:"), "{stderr}");
	assert!(!stderr.contains("API_KEY="), "never print secrets");
	let saved = Project::load_json(&path).unwrap();
	assert_eq!(saved.master.inserts[0].builtin_id(), Some("limiter"));
	assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "Done.");
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
	let project = dir.path().join("Song").join("Song.cdz");
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

	let archive = dir.path().join("Song.cdzpack");
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

#[test]
fn export_writes_mixdown_and_stems_like_the_dialog() {
	let dir = tempfile::tempdir().unwrap();
	let project = dir.path().join("Song").join("Song.cdz");
	std::fs::create_dir_all(project.parent().unwrap()).unwrap();
	song(&project);
	let out = cadenza(&[
		"export",
		s(&project),
		"--stems",
		"tracks",
		"--range",
		"bars:1-1",
		"--format",
		"flac",
		"--bit-depth",
		"16",
		"--normalize",
		"peak:-1",
		"--output-rate",
		"44100",
		"--artist",
		"Tests",
	]);
	assert!(
		out.status.success(),
		"{}",
		String::from_utf8_lossy(&out.stderr)
	);
	let report: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
	let files = report["files"].as_array().unwrap();
	let names: Vec<&str> = files.iter().map(|f| f["name"].as_str().unwrap()).collect();
	assert_eq!(names, ["Song", "Keys", "Bass"]);
	let mix = std::path::Path::new(files[0]["path"].as_str().unwrap());
	assert_eq!(mix, project.parent().unwrap().join("Renders/Song.flac"));
	let info = cadenza_audio_io::probe(mix).unwrap();
	assert_eq!((info.sample_rate, info.bits_per_sample), (44_100, Some(16)));
	assert!((files[0]["peak_db"].as_f64().unwrap() + 1.0).abs() < 0.15);

	let bad = cadenza(&["export", s(&project), "--range", "bars:3"]);
	assert!(!bad.status.success());
	assert!(String::from_utf8_lossy(&bad.stderr).contains("A-B"));
	let bad = cadenza(&["export", s(&project), "--stems", "Drums"]);
	assert!(String::from_utf8_lossy(&bad.stderr).contains("no track named"));
}

#[test]
fn library_import_validate_and_list() {
	let dir = tempfile::tempdir().unwrap();
	// A tresillo bass over Am | F, as a one-clip MIDI file.
	let notes = [
		(45, 0.0, 1.5),
		(45, 1.5, 1.5),
		(52, 3.0, 0.5),
		(41, 4.0, 1.5),
		(41, 5.5, 1.5),
		(48, 7.0, 0.5),
	];
	let clip = Clip::midi("bass", Tick::ZERO, Tick::from_beats(8.0)).with_notes(
		notes
			.iter()
			.map(|&(p, s, l)| Note::new(p, Tick::from_beats(s), Tick::from_beats(l), 0.8)),
	);
	let project = Project::new("x");
	let bytes =
		cadenza_midi_io::export_clip(&clip, &project.tempo, &project.time_signature).unwrap();
	let mid = dir.path().join("my bass.mid");
	std::fs::write(&mid, bytes).unwrap();
	let library = dir.path().join("library");
	let pack = library.join("mine");
	let import = |extra: &[&str]| {
		let mut args = vec![
			"library",
			"import",
			s(&mid),
			"--pack",
			s(&pack),
			"--chords",
			"Am F",
			"--key",
			"A minor",
		];
		args.extend_from_slice(extra);
		cadenza(&args)
	};
	let out = import(&[]);
	assert!(
		out.status.success(),
		"{}",
		String::from_utf8_lossy(&out.stderr)
	);
	let stdout = String::from_utf8_lossy(&out.stdout);
	assert!(
		stdout.contains("imported mine/my_bass: bass relative to the chords"),
		"{stdout}"
	);
	let pattern: serde_json::Value = serde_json::from_str(
		&std::fs::read_to_string(pack.join("patterns/bass/my_bass.json")).unwrap(),
	)
	.unwrap();
	assert_eq!(pattern["harmony"], "chord");
	assert_eq!(
		pattern["lanes"]["R"],
		"x=== ==x= ==== .... x=== ==x= ==== ...."
	);
	assert!(pack.join("pack.json").is_file());
	assert!(pack.join("midi/my_bass.mid").is_file());
	// Same id again needs --force.
	let out = import(&[]);
	assert!(!out.status.success());
	assert!(String::from_utf8_lossy(&out.stderr).contains("--force"));
	assert!(import(&["--force"]).status.success());

	let out = cadenza(&["library", "validate", s(&library)]);
	assert!(
		out.status.success(),
		"{}",
		String::from_utf8_lossy(&out.stderr)
	);
	let out = Command::new(env!("CARGO_BIN_EXE_cadenza"))
		.args(["library", "list", "--json"])
		.env("CADENZA_LIBRARY_DIR", &library)
		.output()
		.unwrap();
	let packs: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
	let ids: Vec<&str> = packs
		.as_array()
		.unwrap()
		.iter()
		.map(|p| p["id"].as_str().unwrap())
		.collect();
	assert_eq!(ids, ["core", "mine"]);

	std::fs::write(pack.join("patterns/bass/broken.json"), "{").unwrap();
	let out = cadenza(&["library", "validate", s(&pack)]);
	assert!(!out.status.success());
	assert!(String::from_utf8_lossy(&out.stderr).contains("broken.json"));
}

#[test]
fn notes_corpus_written_to_disk_scores_like_the_synthetic_one() {
	let dir = tempfile::tempdir().unwrap();
	let corpus = dir.path().join("corpus");
	let s = |p: &Path| p.to_str().unwrap().to_string();
	let out = cadenza(&["notes", "corpus", &s(&corpus)]);
	assert!(
		out.status.success(),
		"{}",
		String::from_utf8_lossy(&out.stderr)
	);
	assert!(corpus.join("legato_glide_long.wav").is_file());
	assert!(corpus.join("legato_glide_long.json").is_file());

	let eval = |extra: &[&str]| {
		let mut args = vec!["notes", "eval", "--json", "--case", "legato_glide_long"];
		args.extend_from_slice(extra);
		let out = cadenza(&args);
		assert!(
			out.status.success(),
			"{}",
			String::from_utf8_lossy(&out.stderr)
		);
		serde_json::from_slice::<serde_json::Value>(&out.stdout).unwrap()
	};
	let synthetic = eval(&[]);
	let from_disk = eval(&["--no-synthetic", "--corpus", &s(&corpus)]);
	assert_eq!(from_disk["cases"][0]["tags"][0], "real");
	assert_eq!(
		synthetic["total"]["detected"],
		from_disk["total"]["detected"]
	);
	assert_eq!(
		synthetic["total"]["mixed_rate"],
		from_disk["total"]["mixed_rate"]
	);

	let out = cadenza(&["notes", "eval", "--detector", "nope"]);
	assert!(String::from_utf8_lossy(&out.stderr).contains("baseline"));
}

#[test]
fn notes_analyze_and_eval_pitch() {
	let dir = tempfile::tempdir().unwrap();
	let s = |p: &Path| p.to_str().unwrap().to_string();
	let out = cadenza(&["notes", "corpus", &s(dir.path())]);
	assert!(
		out.status.success(),
		"{}",
		String::from_utf8_lossy(&out.stderr)
	);
	let wav = dir.path().join("vibrato.wav");
	let run = |extra: &[&str]| {
		let mut args = vec!["notes", "analyze", wav.to_str().unwrap()];
		args.extend_from_slice(extra);
		Command::new(env!("CARGO_BIN_EXE_cadenza"))
			.args(&args)
			.env("XDG_CACHE_HOME", dir.path().join("cache"))
			.output()
			.unwrap()
	};
	let out = run(&["--json"]);
	assert!(
		out.status.success(),
		"{}",
		String::from_utf8_lossy(&out.stderr)
	);
	let a: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
	assert_eq!(a["sample_rate"], 22_050);
	assert!(a["frames"].as_array().unwrap().len() > 1_000);
	assert!(
		dir.path()
			.join("cache/cadenza/analysis")
			.read_dir()
			.unwrap()
			.count() == 1
	);
	let summary = String::from_utf8_lossy(&run(&[]).stdout).to_string();
	assert!(
		summary.contains("voiced:") && summary.contains("pitch:"),
		"{summary}"
	);

	let out = cadenza(&["notes", "eval-pitch", "--json", "--case", "steady_mid"]);
	assert!(
		out.status.success(),
		"{}",
		String::from_utf8_lossy(&out.stderr)
	);
	let r: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
	assert_eq!(r["tracker"], "pyin");
	assert!(r["total"]["raw_pitch"].as_f64().unwrap() > 0.95);
}

#[test]
fn notes_segment_finds_the_labeled_notes() {
	let dir = tempfile::tempdir().unwrap();
	let s = |p: &Path| p.to_str().unwrap().to_string();
	let out = cadenza(&["notes", "corpus", &s(dir.path())]);
	assert!(
		out.status.success(),
		"{}",
		String::from_utf8_lossy(&out.stderr)
	);
	let wav = s(&dir.path().join("legato_glide_long.wav"));
	let run = |extra: &[&str]| {
		let mut args = vec!["notes", "segment", wav.as_str()];
		args.extend_from_slice(extra);
		Command::new(env!("CARGO_BIN_EXE_cadenza"))
			.args(&args)
			.env("XDG_CACHE_HOME", dir.path().join("cache"))
			.output()
			.unwrap()
	};
	let labels: serde_json::Value = serde_json::from_str(
		&std::fs::read_to_string(dir.path().join("legato_glide_long.json")).unwrap(),
	)
	.unwrap();
	let truth = labels["notes"].as_array().unwrap();
	let out = run(&["--json"]);
	assert!(
		out.status.success(),
		"{}",
		String::from_utf8_lossy(&out.stderr)
	);
	let found: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
	let notes = found["notes"].as_array().unwrap();
	assert_eq!(notes.len(), truth.len());
	for (n, t) in notes.iter().zip(truth) {
		let cents = (n["center"].as_f64().unwrap() - t["pitch"].as_f64().unwrap()).abs() * 100.0;
		assert!(cents < 5.0, "{n} vs {t}");
	}

	// A forced boundary in the middle of the first note adds one.
	let first = &truth[0];
	let mid = (first["onset"].as_f64().unwrap() + first["offset"].as_f64().unwrap()) / 2.0;
	let out = run(&["--json", "--split", &format!("{mid}")]);
	let split: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
	assert_eq!(split["notes"].as_array().unwrap().len(), truth.len() + 1);

	let table = String::from_utf8_lossy(&run(&[]).stdout).to_string();
	assert!(
		table.contains("center") && table.lines().count() > truth.len(),
		"{table}"
	);
	let bad = run(&["--sensitivity", "0"]);
	assert!(!bad.status.success());
	assert!(String::from_utf8_lossy(&bad.stderr).contains("--sensitivity"));
}

#[test]
fn notes_to_midi_writes_a_midi_file() {
	let dir = tempfile::tempdir().unwrap();
	let s = |p: &Path| p.to_str().unwrap().to_string();
	assert!(
		cadenza(&["notes", "corpus", &s(dir.path())])
			.status
			.success()
	);
	let mid = dir.path().join("out.mid");
	let out = Command::new(env!("CARGO_BIN_EXE_cadenza"))
		.args([
			"notes",
			"to-midi",
			&s(&dir.path().join("legato_glide_long.wav")),
			"-o",
			&s(&mid),
			"--hum",
			"--bends",
		])
		.env("XDG_CACHE_HOME", dir.path().join("cache"))
		.output()
		.unwrap();
	assert!(
		out.status.success(),
		"{}",
		String::from_utf8_lossy(&out.stderr)
	);
	let stdout = String::from_utf8_lossy(&out.stdout);
	assert!(stdout.starts_with("12 notes"), "{stdout}");
	assert!(
		stdout.contains("key:") && stdout.contains("pitch bend:"),
		"{stdout}"
	);
	let bytes = std::fs::read(&mid).unwrap();
	assert_eq!(&bytes[..4], b"MThd");
}

#[test]
fn notes_tune_corrects_a_recording() {
	let dir = tempfile::tempdir().unwrap();
	let s = |p: &Path| p.to_str().unwrap().to_string();
	assert!(
		cadenza(&["notes", "corpus", &s(dir.path())])
			.status
			.success()
	);
	let wav = dir.path().join("tuned.wav");
	let out = Command::new(env!("CARGO_BIN_EXE_cadenza"))
		.args([
			"notes",
			"tune",
			&s(&dir.path().join("steady_mid.wav")),
			"-o",
			&s(&wav),
			"--transpose",
			"-2",
		])
		.env("XDG_CACHE_HOME", dir.path().join("cache"))
		.output()
		.unwrap();
	assert!(
		out.status.success(),
		"{}",
		String::from_utf8_lossy(&out.stderr)
	);
	let stdout = String::from_utf8_lossy(&out.stdout);
	assert!(stdout.contains("10 notes, 10 moved"), "{stdout}");
	let worst: f64 = stdout
		.split("worst after: ")
		.nth(1)
		.and_then(|t| t.split(' ').next())
		.and_then(|t| t.parse().ok())
		.unwrap();
	assert!(worst <= 5.0, "{stdout}");
	assert!(std::fs::metadata(&wav).unwrap().len() > 1000);
	let bad = cadenza(&["notes", "tune", &s(&wav), "--key", "H major"]);
	assert!(!bad.status.success());
	assert!(String::from_utf8_lossy(&bad.stderr).contains("A minor"));
}
