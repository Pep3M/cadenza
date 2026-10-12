//! Preset, plugin-state, sidechain and multi-output tools.

mod plugin_support;

use cadenza_core::{DeviceId, TrackId};
use cadenza_mcp::Session;
use cadenza_mcp::tools::{devices, presets, project, tracks};
use serde::de::DeserializeOwned;
use serde_json::json;

const DUCKER: &str = "clap:dev.cadenza.test.ducker";
const MULTI: &str = "clap:dev.cadenza.test.multi";

fn args<T: DeserializeOwned>(value: serde_json::Value) -> T {
	serde_json::from_value(value).expect("valid tool args")
}

fn session(dir: &tempfile::TempDir, plugins: bool) -> Session {
	let mut s = Session::with_plugin_cache(dir.path().join("never-written.json"));
	s.set_preset_dir(dir.path().join("presets"));
	if plugins {
		s.set_plugins(plugin_support::routing_catalog());
		s.set_plugin_hosting(plugin_support::sandboxed());
	}
	s
}

fn track(
	s: &mut Session,
	name: &str,
	kind: &str,
	instrument: Option<&str>,
) -> (TrackId, Option<DeviceId>) {
	let mut a = json!({ "name": name, "kind": kind });
	if let Some(i) = instrument {
		a["instrument"] = json!({ "device": i });
	}
	let out = tracks::create_track(s, args(a)).unwrap();
	(out.track_id, out.instrument_device_id)
}

fn insert(s: &mut Session, track: Option<TrackId>, device: &str) -> DeviceId {
	tracks::insert_device(s, args(json!({ "track_id": track, "device": device })))
		.unwrap()
		.device_id
}

#[test]
fn builtin_factory_and_user_presets() {
	let dir = tempfile::tempdir().unwrap();
	let mut s = session(&dir, false);
	let (pad, synth) = track(&mut s, "Pad", "instrument", Some("poly_synth"));
	let synth = synth.unwrap();
	let list = presets::list_presets(&mut s, args(json!({ "track_id": pad, "device_id": synth })))
		.unwrap();
	assert_eq!(list.device, "poly_synth");
	let names: Vec<&str> = list.presets.iter().map(|p| p.name.as_str()).collect();
	assert_eq!(names, ["Warm Pad", "Pluck Lead", "Sub Bass"]);

	let loaded = presets::load_preset(
		&mut s,
		args(json!({ "track_id": pad, "device_id": synth, "name": "warm pad" })),
	)
	.unwrap();
	assert_eq!(loaded.name, "Warm Pad");
	assert!(!loaded.has_state);
	let device = |s: &Session| {
		s.document()
			.project()
			.device(Some(pad), synth)
			.unwrap()
			.clone()
	};
	assert_eq!(device(&s).params.get("unison"), Some(&3.0));
	assert_eq!(
		s.document().history_labels().last().copied(),
		Some("Load device preset")
	);

	// Tweak and keep it as a user preset; it shows up next to the factory ones.
	tracks::set_device_params(
		&mut s,
		args(json!({ "track_id": pad, "device_id": synth, "params": { "gain": -3.0 } })),
	)
	.unwrap();
	let saved = presets::save_preset(
		&mut s,
		args(json!({ "track_id": pad, "device_id": synth, "name": "My Pad" })),
	)
	.unwrap();
	assert!(
		saved.path.ends_with("poly_synth/My Pad.json"),
		"{}",
		saved.path
	);
	let err = presets::save_preset(
		&mut s,
		args(json!({ "track_id": pad, "device_id": synth, "name": "My Pad" })),
	)
	.unwrap_err();
	assert!(err.to_string().contains("overwrite"), "{err}");

	presets::load_preset(
		&mut s,
		args(json!({ "track_id": pad, "device_id": synth, "name": "Sub Bass" })),
	)
	.unwrap();
	assert_eq!(device(&s).params.get("gain"), None);
	let mine = presets::load_preset(
		&mut s,
		args(json!({ "track_id": pad, "device_id": synth, "name": "my pad" })),
	)
	.unwrap();
	assert_eq!(mine.source, presets::PresetSource::User);
	assert_eq!(device(&s).params.get("gain"), Some(&-3.0));
	assert_eq!(device(&s).params.get("unison"), Some(&3.0));

	let err = presets::load_preset(
		&mut s,
		args(json!({ "track_id": pad, "device_id": synth, "name": "Nope" })),
	)
	.unwrap_err();
	assert!(err.to_string().contains("list_presets"), "{err}");

	// A preset file written for another device is refused with an explanation.
	let comp = insert(&mut s, Some(pad), "compressor");
	let folder = dir.path().join("presets/compressor");
	std::fs::create_dir_all(&folder).unwrap();
	std::fs::write(
		folder.join("Odd.json"),
		json!({ "format_version": 1, "name": "Odd", "device": "eq", "params": {} }).to_string(),
	)
	.unwrap();
	let err = presets::load_preset(
		&mut s,
		args(json!({ "track_id": pad, "device_id": comp, "name": "Odd" })),
	)
	.unwrap_err();
	assert!(err.to_string().contains("saved for device 'eq'"), "{err}");
}

#[test]
fn set_sidechain_keys_compressors_and_explains_mistakes() {
	let dir = tempfile::tempdir().unwrap();
	let mut s = session(&dir, false);
	let (kick, _) = track(&mut s, "Kick", "instrument", Some("drum_machine"));
	let (bass, _) = track(&mut s, "Bass", "instrument", Some("poly_synth"));
	let comp = insert(&mut s, Some(bass), "compressor");
	let eq = insert(&mut s, Some(bass), "eq");

	tracks::set_sidechain(
		&mut s,
		args(
			json!({ "track_id": bass, "device_id": comp, "source_track_id": kick, "pre_fader": true }),
		),
	)
	.unwrap();
	let summary = project::get_project(&s);
	let inserts = &summary.tracks[1].inserts;
	let key = inserts[0].sidechain.expect("keyed");
	assert_eq!((key.track, key.pre_fader), (kick, true));

	let err = tracks::set_sidechain(
		&mut s,
		args(json!({ "track_id": bass, "device_id": eq, "source_track_id": kick })),
	)
	.unwrap_err();
	assert!(err.to_string().contains("no sidechain input"), "{err}");
	let err = tracks::set_sidechain(
		&mut s,
		args(json!({ "track_id": bass, "device_id": comp, "source_track_id": bass })),
	)
	.unwrap_err();
	assert!(err.to_string().contains("own track"), "{err}");

	tracks::set_sidechain(&mut s, args(json!({ "track_id": bass, "device_id": comp }))).unwrap();
	assert!(
		project::get_project(&s).tracks[1].inserts[0]
			.sidechain
			.is_none()
	);

	// Built-in devices advertise sidechains and presets in get_device_info.
	let info = devices::get_device_info(None, args(json!({ "device": "compressor" }))).unwrap();
	assert!(info.sidechain_input);
	assert!(info.presets.iter().any(|p| p.starts_with("Sidechain Pump")));
}

#[test]
fn plugin_presets_state_capture_and_save() {
	let dir = tempfile::tempdir().unwrap();
	let mut s = session(&dir, true);
	let (bass, _) = track(&mut s, "Bass", "audio", None);
	let duck = insert(&mut s, Some(bass), DUCKER);

	let list = presets::list_presets(&mut s, args(json!({ "track_id": bass, "device_id": duck })))
		.unwrap();
	let names: Vec<&str> = list.presets.iter().map(|p| p.name.as_str()).collect();
	assert_eq!(names, ["Unity", "Quiet Key"]);
	let loaded = presets::load_preset(
		&mut s,
		args(json!({ "track_id": bass, "device_id": duck, "name": "Quiet Key" })),
	)
	.unwrap();
	assert!(loaded.has_state && loaded.params.is_empty());
	let state = |s: &Session| {
		s.document()
			.project()
			.device(Some(bass), duck)
			.unwrap()
			.state
			.clone()
	};
	let quiet = state(&s).expect("preset state stored");

	// Nothing changed since: capturing again is a no-op.
	let out =
		presets::capture_plugin_state(&mut s, args(json!({ "track_id": bass, "device_id": duck })))
			.unwrap();
	assert_eq!(out.captured.len(), 1);
	assert!(!out.captured[0].changed);

	// Param edits are baked into the state by the next capture (one undo step).
	tracks::set_device_params(
		&mut s,
		args(json!({ "track_id": bass, "device_id": duck, "params": { "Key Level": -6.0 } })),
	)
	.unwrap();
	let out = presets::capture_plugin_state(&mut s, args(json!({}))).unwrap();
	assert!(out.captured[0].changed, "{out:?}");
	assert_ne!(state(&s).as_deref(), Some(quiet.as_str()));
	project::undo(&mut s).unwrap();
	assert_eq!(state(&s).as_deref(), Some(quiet.as_str()));

	// save_project captures automatically and writes the state into the file.
	let path = dir.path().join("song.json");
	let saved = project::save_project(&mut s, args(json!({ "path": path }))).unwrap();
	assert_eq!(saved.captured_plugin_states, Some(1));
	assert!(saved.warnings.is_empty(), "{:?}", saved.warnings);
	let reopened = cadenza_model::Project::load_json(&path).unwrap();
	assert_eq!(
		reopened.device(Some(bass), duck).unwrap().state,
		state(&s),
		"the file holds the captured state"
	);

	// User presets of plugins keep the full state.
	let saved = presets::save_preset(
		&mut s,
		args(json!({ "track_id": bass, "device_id": duck, "name": "Half" })),
	)
	.unwrap();
	assert!(
		saved.path.contains("dev.cadenza.test.ducker"),
		"{}",
		saved.path
	);
	let file: presets::UserPreset =
		serde_json::from_slice(&std::fs::read(&saved.path).unwrap()).unwrap();
	assert_eq!(file.device, DUCKER);
	assert!(file.state.is_some());

	let err =
		presets::capture_plugin_state(&mut s, args(json!({ "device_id": duck }))).unwrap_err();
	assert!(err.to_string().contains("does not exist"), "{err}");
}

#[test]
fn output_routes_and_plugin_sidechains() {
	let dir = tempfile::tempdir().unwrap();
	let mut s = session(&dir, true);
	let (kick_bus, _) = track(&mut s, "Kick Bus", "bus", None);
	let (drums, _) = track(&mut s, "Drums", "instrument", Some(MULTI));

	let catalog = s.plugins();
	let info = devices::get_device_info(Some(&catalog), args(json!({ "device": MULTI }))).unwrap();
	assert_eq!(info.aux_outputs, ["1 'Kick' (stereo)", "2 'Snare' (mono)"]);
	let info = devices::get_device_info(Some(&catalog), args(json!({ "device": DUCKER }))).unwrap();
	assert!(info.sidechain_input);

	tracks::set_output_routes(
		&mut s,
		args(json!({ "track_id": drums, "routes": [
			{ "port": 1, "bus_track_id": kick_bus },
			{ "port": 2 }
		] })),
	)
	.unwrap();
	let summary = project::get_project(&s);
	let routes = &summary.tracks[1].instrument.as_ref().unwrap().aux_outputs;
	assert_eq!(routes.len(), 2);
	assert_eq!(routes[0].bus, Some(kick_bus));

	let err = tracks::set_output_routes(
		&mut s,
		args(json!({ "track_id": drums, "routes": [{ "port": 3 }] })),
	)
	.unwrap_err();
	assert!(
		err.to_string()
			.contains("1 'Kick' (2 ch), 2 'Snare' (1 ch)"),
		"{err}"
	);
	let (synth_track, _) = track(&mut s, "Synth", "instrument", Some("poly_synth"));
	let err = tracks::set_output_routes(
		&mut s,
		args(json!({ "track_id": synth_track, "routes": [{ "port": 1 }] })),
	)
	.unwrap_err();
	assert!(err.to_string().contains("no extra outputs"), "{err}");

	// A plugin effect with a sidechain port can be keyed like the built-in compressor.
	let (pad, _) = track(&mut s, "Pad", "audio", None);
	let duck = insert(&mut s, Some(pad), DUCKER);
	tracks::set_sidechain(
		&mut s,
		args(json!({ "track_id": pad, "device_id": duck, "source_track_id": drums })),
	)
	.unwrap();
	// The kick bus is fed by the drums' extra output, so the drums cannot be keyed by it.
	let comp = insert(&mut s, Some(drums), "compressor");
	let err = tracks::set_sidechain(
		&mut s,
		args(json!({ "track_id": drums, "device_id": comp, "source_track_id": kick_bus })),
	)
	.unwrap_err();
	assert!(err.to_string().contains("feedback loop"), "{err}");
}
