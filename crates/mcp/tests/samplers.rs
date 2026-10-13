//! Sampler tools: factory content, load_sample, set_zone, set_pad, presets with zones and
//! write_drum_pattern on a drum_sampler, through the same JSON path the AI uses.

use std::path::{Path, PathBuf};

use cadenza_mcp::{CadenzaServer, Session};
use cadenza_model::TrackKind;
use serde_json::{Value, json};

fn factory_dir() -> PathBuf {
	static DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
	DIR.get_or_init(|| {
		let dir = std::env::temp_dir().join(format!("cadenza-mcp-factory-{}", std::process::id()));
		// SAFETY: set once, before any other thread of this test binary reads it.
		unsafe { std::env::set_var(cadenza_engine::factory::FACTORY_SAMPLES_ENV, &dir) };
		dir
	})
	.clone()
}

fn write_tone(path: &Path, freq: f64) {
	let spec = hound::WavSpec {
		channels: 1,
		sample_rate: 44_100,
		bits_per_sample: 16,
		sample_format: hound::SampleFormat::Int,
	};
	let mut w = hound::WavWriter::create(path, spec).unwrap();
	for i in 0..22_050 {
		let v = (std::f64::consts::TAU * freq * i as f64 / 44_100.0).sin() * 12_000.0;
		w.write_sample(v as i16).unwrap();
	}
	w.finalize().unwrap();
}

async fn call(server: &CadenzaServer, tool: &str, args: Value) -> Result<Value, String> {
	server.call_tool_json(tool, args).await
}

async fn track(server: &CadenzaServer, device: &str) -> (Value, Value) {
	let out = call(
		server,
		"create_track",
		json!({"name": device, "kind": "instrument", "instrument": {"device": device}}),
	)
	.await
	.unwrap();
	(out["track_id"].clone(), out["instrument_device_id"].clone())
}

#[tokio::test]
async fn drum_sampler_starts_with_the_kit_and_pads_are_editable() {
	factory_dir();
	let server = CadenzaServer::with_session(Session::new());
	let (track_id, _) = track(&server, "drum_sampler").await;
	let project = call(&server, "get_project", json!({})).await.unwrap();
	let zones = project["tracks"][0]["instrument"]["zones"]
		.as_array()
		.unwrap()
		.clone();
	assert!(zones.len() >= 12, "factory kit loaded: {zones:?}");

	let listed = call(&server, "list_factory_samples", json!({}))
		.await
		.unwrap();
	let snare = listed["samples"]
		.as_array()
		.unwrap()
		.iter()
		.find(|s| s["name"] == "Snare.wav")
		.unwrap()["path"]
		.clone();
	assert!(listed["presets"].to_string().contains("Synth Kit"));

	// Snare sample on the cowbell pad, then shape it and send it to output 3.
	let out = call(
		&server,
		"load_sample",
		json!({"track_id": track_id, "path": snare, "pad": "cowbell"}),
	)
	.await
	.unwrap();
	assert!(out["loaded"].as_str().unwrap().contains("(56)"), "{out}");
	let out = call(
		&server,
		"set_pad",
		json!({"track_id": track_id, "pad": 56, "tune": -5, "decay_ms": 120, "output": 3, "choke": 2}),
	)
	.await
	.unwrap();
	assert!(out["notes"].to_string().contains("extra output 3"), "{out}");
	let project = call(&server, "get_project", json!({})).await.unwrap();
	let inst = &project["tracks"][0]["instrument"];
	let pad = inst["zones"]
		.as_array()
		.unwrap()
		.iter()
		.find(|z| z["keys"] == "G#3 (56)")
		.unwrap();
	let settings = pad["settings"].as_str().unwrap();
	assert!(
		settings.contains("tune=-5") && settings.contains("output=3"),
		"{settings}"
	);
	assert_eq!(inst["aux_outputs"][0]["port"], 3);
	// Routing it to a bus is accepted (the drum_sampler has 8 extra outputs).
	let bus = call(
		&server,
		"create_track",
		json!({"name": "Perc", "kind": "bus"}),
	)
	.await
	.unwrap()["track_id"]
		.clone();
	call(
		&server,
		"set_output_routes",
		json!({"track_id": track_id, "routes": [{"port": 3, "bus_track_id": bus}]}),
	)
	.await
	.unwrap();
	let err = call(
		&server,
		"set_output_routes",
		json!({"track_id": track_id, "routes": [{"port": 9}]}),
	)
	.await
	.unwrap_err();
	assert!(err.contains("Out 8"), "{err}");

	// Errors an LLM can act on.
	let err = call(
		&server,
		"load_sample",
		json!({"track_id": track_id, "path": snare, "pad": "tuba"}),
	)
	.await
	.unwrap_err();
	assert!(err.contains("kick, snare"), "{err}");
	let err = call(
		&server,
		"set_pad",
		json!({"track_id": track_id, "pad": "C0", "gain_db": -3}),
	)
	.await
	.unwrap_err();
	assert!(err.contains("load_sample"), "{err}");
	let missing = std::env::temp_dir().join("nope").join("missing.wav");
	let err = call(
		&server,
		"load_sample",
		json!({"track_id": track_id, "path": missing, "pad": "kick"}),
	)
	.await
	.unwrap_err();
	assert!(err.contains("cannot use"), "{err}");

	// The drum writer plays the pads and names lanes without a sample.
	call(
		&server,
		"set_pad",
		json!({"track_id": track_id, "pad": "ride", "clear": true}),
	)
	.await
	.unwrap();
	let out = call(
		&server,
		"write_drum_pattern",
		json!({"track_id": track_id, "start_beat": 0, "bars": 1,
			"lanes": {"kick": "X...x...", "cowbell": "..x...x.", "ride": "x.x.x.x."}}),
	)
	.await
	.unwrap();
	let warnings = out["warnings"].to_string();
	assert!(
		warnings.contains("ride") && !warnings.contains("cowbell"),
		"{warnings}"
	);
	let report = call(&server, "analyze_mix", json!({})).await.unwrap();
	assert!(
		report["master"]["peak_db"].as_f64().unwrap() > -30.0,
		"{report}"
	);
}

#[tokio::test]
async fn sampler_auto_maps_zones_and_presets_carry_samples() {
	factory_dir();
	let dir = tempfile::tempdir().unwrap();
	let mut session = Session::new();
	session.set_preset_dir(dir.path().join("presets"));
	let server = CadenzaServer::with_session(session);
	let (track_id, device_id) = track(&server, "sampler").await;
	let a3 = dir.path().join("Lead A3.wav");
	write_tone(&a3, 220.0);
	let unnamed = dir.path().join("lead.wav");
	write_tone(&unnamed, 440.0);

	// Auto-map: root from the name, then from the detected pitch.
	let out = call(
		&server,
		"load_sample",
		json!({"track_id": track_id, "path": a3}),
	)
	.await
	.unwrap();
	assert_eq!(out["root"], "A3 (57)");
	assert_eq!(out["zones"].as_array().unwrap().len(), 1);
	let out = call(
		&server,
		"load_sample",
		json!({"track_id": track_id, "path": unnamed, "keys": "C5-C6"}),
	)
	.await
	.unwrap();
	assert_eq!(out["root"], "A4 (69)");
	assert_eq!(out["zones"][1]["keys"], "C5..C6");

	let out = call(
		&server,
		"set_zone",
		json!({"track_id": track_id, "zone": 0, "key_high": "B4", "loop_mode": "forward",
			"loop_start": 0.1, "loop_end": 0.4, "loop_crossfade": 0.05, "vel_low": 0.2}),
	)
	.await
	.unwrap();
	let s = out["zones"][0]["settings"].as_str().unwrap();
	assert!(
		s.contains("loop=forward") && s.contains("loop_crossfade=0.05"),
		"{s}"
	);
	let err = call(
		&server,
		"set_zone",
		json!({"track_id": track_id, "zone": 0, "loop_start": 0.5, "loop_end": 0.2}),
	)
	.await
	.unwrap_err();
	assert!(err.contains("loop_end"), "{err}");
	let err = call(
		&server,
		"set_zone",
		json!({"track_id": track_id, "zone": 7}),
	)
	.await
	.unwrap_err();
	assert!(err.contains("2 zones"), "{err}");

	// A user preset keeps the zones; factory presets bring theirs back.
	call(
		&server,
		"save_preset",
		json!({"track_id": track_id, "device_id": device_id, "name": "My Lead"}),
	)
	.await
	.unwrap();
	let out = call(
		&server,
		"load_preset",
		json!({"track_id": track_id, "device_id": device_id, "name": "Sine Pluck"}),
	)
	.await
	.unwrap();
	assert_eq!(out["zones"], 4);
	let out = call(
		&server,
		"load_preset",
		json!({"track_id": track_id, "device_id": device_id, "name": "My Lead"}),
	)
	.await
	.unwrap();
	assert_eq!(out["zones"], 2);
	let history = call(&server, "history", json!({})).await.unwrap();
	assert!(
		history.to_string().contains("Load preset 'My Lead'"),
		"{history}"
	);
	// Pads belong to the drum_sampler.
	let err = call(
		&server,
		"set_pad",
		json!({"track_id": track_id, "pad": "kick"}),
	)
	.await
	.unwrap_err();
	assert!(err.contains("set_zone"), "{err}");
	let synth = call(
		&server,
		"create_track",
		json!({"name": "Synth", "kind": "instrument", "instrument": {"device": "poly_synth"}}),
	)
	.await
	.unwrap()["track_id"]
		.clone();
	let err = call(
		&server,
		"load_sample",
		json!({"track_id": synth, "path": a3}),
	)
	.await
	.unwrap_err();
	assert!(err.contains("does not play samples"), "{err}");
	let p = server.shared_session();
	let kinds: Vec<TrackKind> = p
		.lock()
		.unwrap()
		.document()
		.project()
		.tracks
		.iter()
		.map(|t| t.kind)
		.collect();
	assert_eq!(kinds.len(), 2);
}
