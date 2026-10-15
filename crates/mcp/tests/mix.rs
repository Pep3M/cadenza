//! The sound engineer's tools end to end over the in-process API: analyze, fix with the
//! issues' own moves, re-analyze; one channel in depth; a reference comparison; the style's
//! targets; and the token budgets of their outputs.

use std::sync::Arc;

use cadenza_core::Tick;
use cadenza_library::Library;
use cadenza_mcp::{CadenzaServer, Session};
use cadenza_model::{Clip, Command, Device, Note, ProjectMeta, Track, TrackKind};
use serde_json::{Value, json};

fn synth(name: &str, pitch: u8, gain: f32) -> Track {
	let mut t = Track::new(name, TrackKind::Instrument).with_instrument(
		Device::builtin("poly_synth", "Synth")
			.with_param("amp_attack", 0.0)
			.with_param("amp_sustain", 100.0)
			.with_param("gain", gain),
	);
	t.clips.push(
		Clip::midi(format!("{name} clip"), Tick::ZERO, Tick::from_beats(16.0)).with_notes(
			(0..4).map(|i| {
				Note::new(
					pitch,
					Tick::from_beats(f64::from(i) * 4.0),
					Tick::from_beats(3.5),
					0.9,
				)
			}),
		),
	);
	t
}

fn kit() -> Track {
	let mut t = Track::new("Drums", TrackKind::Instrument)
		.with_instrument(Device::builtin("drum_machine", "Drums"));
	t.clips.push(
		Clip::midi("Beat", Tick::ZERO, Tick::from_beats(16.0)).with_notes((0..16).flat_map(|b| {
			let at = Tick::from_beats(f64::from(b));
			[
				Note::new(36, at, Tick::from_beats(0.25), 1.0),
				Note::new(42, at, Tick::from_beats(0.25), 0.7),
				Note::new(63, at + Tick::from_beats(0.5), Tick::from_beats(0.25), 0.8),
			]
		})),
	);
	t
}

fn server_with(tracks: Vec<Track>, style: Option<&str>) -> CadenzaServer {
	let mut s = Session::new();
	s.set_library(Arc::new(Library::core()));
	for track in tracks {
		s.document_mut()
			.execute(Command::AddTrack { track, index: None })
			.unwrap();
	}
	if let Some(style) = style {
		s.document_mut()
			.execute(Command::SetProjectMeta {
				meta: ProjectMeta {
					style: Some(style.into()),
				},
			})
			.unwrap();
	}
	CadenzaServer::with_session(s)
}

async fn call(server: &CadenzaServer, tool: &str, args: Value) -> Value {
	server
		.call_tool_json(tool, args)
		.await
		.unwrap_or_else(|e| panic!("{tool}: {e}"))
}

#[tokio::test]
async fn analyze_fix_and_reanalyze() {
	// A hot bass and a loud lead clipping the master.
	let server = server_with(
		vec![
			kit(),
			synth("Bass", 33, 6.0),
			synth("Keys", 45, -14.0),
			synth("Lead", 84, 0.0),
		],
		Some("core/pop"),
	);
	let before = call(&server, "analyze_mix", json!({})).await;
	assert_eq!(before["targets"]["source"], "style core/pop");
	assert_eq!(before["targets"]["lufs"], -10.0);
	let tracks = before["tracks"].as_array().unwrap();
	assert_eq!(tracks.len(), 4);
	assert_eq!(tracks[0]["role"], "drums");
	assert_eq!(tracks[1]["role"], "bass");
	assert_eq!(before["master"]["octaves_db"].as_array().unwrap().len(), 10);
	let issues = before["issues"].as_array().unwrap();
	assert!(!issues.is_empty(), "{before}");
	assert_eq!(issues[0]["id"], "i1");
	let kinds: Vec<&str> = issues.iter().filter_map(|i| i["kind"].as_str()).collect();
	assert!(
		kinds.contains(&"clipping") || kinds.contains(&"true_peak"),
		"{kinds:?}"
	);
	assert!(
		issues.iter().all(|i| i["message"].as_str().is_some()),
		"{before}"
	);

	// Apply every issue's own moves as one pass.
	let moves: Vec<Value> = issues
		.iter()
		.flat_map(|i| i["moves"].as_array().cloned().unwrap_or_default())
		.collect();
	assert!(!moves.is_empty());
	let applied = call(&server, "apply_mix_moves", json!({ "moves": moves })).await;
	assert_eq!(applied["undo_label"], "AI mix pass");
	let history = call(&server, "history", json!({})).await;
	assert!(history.to_string().contains("AI mix pass"), "{history}");

	let after = call(&server, "analyze_mix", json!({})).await;
	let high = |v: &Value| {
		v["issues"]
			.as_array()
			.unwrap()
			.iter()
			.filter(|i| i["severity"] == "high")
			.count()
	};
	assert!(high(&after) < high(&before), "{after}");
	assert!(
		!after["issues"]
			.as_array()
			.unwrap()
			.iter()
			.any(|i| i["kind"] == "clipping"),
		"{after}"
	);
	assert!(
		after["master"]["true_peak_db"].as_f64().unwrap() < 0.0,
		"limited: {}",
		after["master"]
	);

	let bass_id = tracks[1]["id"].as_str().unwrap();
	let track = call(&server, "analyze_track", json!({ "track_id": bass_id })).await;
	assert_eq!(track["name"], "Bass");
	assert_eq!(track["third_octave_db"].as_array().unwrap().len(), 31);
	assert!(track["peak_band_hz"].as_f64().unwrap() <= 125.0, "{track}");
	assert!(track["role_defaults"]["balance_db"].is_number());
	let err = server
		.call_tool_json(
			"analyze_track",
			json!({ "track_id": "00000000-0000-0000-0000-000000000000" }),
		)
		.await
		.unwrap_err();
	assert!(err.contains("does not exist"), "{err}");

	let targets = call(&server, "mix_targets", json!({})).await;
	assert_eq!(targets["profile"], "pop");
	assert!(targets["notes"].as_str().unwrap().contains("Vocal space"));
	// The four tracks, plus the reverb return a "too narrow" fix creates.
	let names: Vec<&str> = targets["tracks"]
		.as_array()
		.unwrap()
		.iter()
		.map(|t| t["name"].as_str().unwrap())
		.collect();
	assert!(
		names.len() == 4 || (names.len() == 5 && names[4] == "Reverb"),
		"{names:?}"
	);
}

#[tokio::test]
async fn reference_comparison_of_the_mix_with_its_own_render() {
	let dir = tempfile::tempdir().unwrap();
	let server = server_with(vec![kit(), synth("Bass", 33, -6.0)], None);
	let wav = dir.path().join("ref.wav");
	call(
		&server,
		"render",
		json!({ "path": wav, "tail": false, "normalize": {"mode": "peak", "peak_db": -1.0} }),
	)
	.await;
	let out = call(&server, "compare_to_reference", json!({ "file": wav })).await;
	let c = &out["comparison"];
	assert!(
		c["octave_delta_db"]
			.as_array()
			.unwrap()
			.iter()
			.all(|d| d.as_f64().unwrap().abs() < 1.5),
		"same song, same shape: {out}"
	);
	let missing = server
		.call_tool_json("compare_to_reference", json!({ "file": "/nope/ref.wav" }))
		.await
		.unwrap_err();
	assert!(
		missing.contains("cannot access reference file"),
		"{missing}"
	);
}

#[tokio::test]
async fn outputs_stay_within_their_token_budgets() {
	// 24 channels: the summary must stay compact enough to read after every pass.
	let mut tracks = vec![kit()];
	for i in 0..23u8 {
		tracks.push(synth(&format!("Synth {i}"), 36 + i * 2, -12.0));
	}
	let server = server_with(tracks, Some("core/house"));
	let summary = call(&server, "analyze_mix", json!({})).await.to_string();
	assert!(
		summary.len() < 9_000,
		"analyze_mix: {} chars",
		summary.len()
	);
	let id = serde_json::from_str::<Value>(&summary).unwrap()["tracks"][3]["id"]
		.as_str()
		.unwrap()
		.to_owned();
	let track = call(&server, "analyze_track", json!({ "track_id": id }))
		.await
		.to_string();
	assert!(track.len() < 3_000, "analyze_track: {} chars", track.len());
	let targets = call(&server, "mix_targets", json!({})).await.to_string();
	assert!(
		targets.len() < 5_000,
		"mix_targets: {} chars",
		targets.len()
	);
}
