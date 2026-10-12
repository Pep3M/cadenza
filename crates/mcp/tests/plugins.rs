//! Plugin-aware device tools against a temporary catalog holding the
//! `cadenza-test-plugins` CLAP binary (a 64-sample-latency gain and a sine instrument).

mod plugin_support;

use cadenza_core::TrackId;
use cadenza_mcp::McpError;
use cadenza_mcp::Session;
use cadenza_mcp::tools::{clips, devices, render, tracks};
use cadenza_model::DeviceKind;
use serde::de::DeserializeOwned;
use serde_json::json;

const GAIN: &str = "clap:dev.cadenza.test.gain";
const SINE: &str = "clap:dev.cadenza.test.sine";

fn args<T: DeserializeOwned>(value: serde_json::Value) -> T {
	serde_json::from_value(value).expect("valid tool args")
}

fn session() -> Session {
	let dir = std::env::temp_dir().join(format!("cadenza-mcp-plugins-{}", std::process::id()));
	let mut s = Session::with_plugin_cache(dir.join("never-written.json"));
	s.set_plugins(plugin_support::catalog());
	s.set_plugin_hosting(plugin_support::sandboxed());
	s
}

fn list(s: &mut Session, a: serde_json::Value) -> devices::ListDevicesOutput {
	let catalog = s.plugins();
	devices::list_devices(Some(&catalog), args(a))
}

#[test]
fn list_devices_searches_and_pages_plugins() {
	let mut s = session();
	let found = list(
		&mut s,
		json!({ "query": "cadenza test", "source": "plugin" }),
	);
	assert_eq!(found.total, 2, "{found:#?}");
	let gain = found.devices.iter().find(|d| d.id == GAIN).unwrap();
	assert_eq!(gain.source, "plugin");
	assert_eq!(gain.format, Some("CLAP"));
	assert_eq!(gain.role, "effect");
	assert_eq!(gain.vendor.as_deref(), Some("Cadenza"));
	assert_eq!(gain.param_count, 1);
	assert_eq!(gain.latency_samples, Some(64));
	assert!(gain.params.is_none(), "plugin entries stay compact");
	assert!(gain.features.iter().any(|f| f == "audio-effect"));

	let instruments = list(
		&mut s,
		json!({ "query": "CADENZA", "role": "instrument", "source": "plugin" }),
	);
	assert_eq!(instruments.total, 1);
	assert_eq!(instruments.devices[0].id, SINE);

	// Built-ins and plugins share one searchable list; built-ins come first.
	let gains = list(&mut s, json!({ "query": "gain" }));
	assert!(
		gains
			.devices
			.iter()
			.any(|d| d.id == "gain" && d.params.is_some())
	);
	assert!(gains.devices.iter().any(|d| d.id == GAIN));
	assert!(gains.note.is_none());

	let page1 = list(&mut s, json!({ "query": "cadenza test", "limit": 1 }));
	assert_eq!((page1.devices.len(), page1.next_offset), (1, Some(1)));
	let page2 = list(
		&mut s,
		json!({ "query": "cadenza test", "limit": 1, "offset": 1 }),
	);
	assert_eq!((page2.devices.len(), page2.next_offset), (1, None));
	assert_ne!(page1.devices[0].id, page2.devices[0].id);

	let builtins = list(&mut s, json!({ "source": "builtin" }));
	assert!(builtins.devices.iter().all(|d| d.source == "builtin"));
}

#[test]
fn get_device_info_filters_plugin_params() {
	let catalog = plugin_support::catalog();
	let info = devices::get_device_info(Some(&catalog), args(json!({ "device": GAIN }))).unwrap();
	assert_eq!((info.param_count, info.matched), (1, 1));
	let p = &info.params[0];
	assert_eq!((p.id.as_str(), p.name.as_str()), ("0", "Gain"));
	assert_eq!((p.min, p.max, p.default), (-60.0, 12.0, 0.0));
	assert_eq!(info.latency_samples, Some(64));
	assert!(info.description.contains("audio effect"));

	let none = devices::get_device_info(
		Some(&catalog),
		args(json!({ "device": GAIN, "param_query": "resonance" })),
	)
	.unwrap();
	assert_eq!((none.matched, none.params.len()), (0, 0));

	let sine = devices::get_device_info(
		Some(&catalog),
		args(json!({ "device": SINE, "param_query": "LEVEL" })),
	)
	.unwrap();
	assert_eq!(sine.role, "instrument");
	assert_eq!(sine.params[0].name, "Level");
}

fn synth_track(s: &mut Session) -> TrackId {
	tracks::create_track(
		s,
		args(
			json!({ "name": "Keys", "kind": "instrument", "instrument": { "device": "poly_synth" } }),
		),
	)
	.unwrap()
	.track_id
}

#[test]
fn plugin_insert_and_params_by_name() {
	let mut s = session();
	let track = synth_track(&mut s);
	let device_id = tracks::insert_device(
		&mut s,
		args(json!({ "track_id": track, "device": GAIN, "params": { "gain": -6.0 } })),
	)
	.unwrap()
	.device_id;
	let d = s
		.document()
		.project()
		.device(Some(track), device_id)
		.unwrap()
		.clone();
	assert_eq!(d.name, "Cadenza Test Gain");
	match &d.kind {
		DeviceKind::Clap { plugin_id, path } => {
			assert_eq!(plugin_id, "dev.cadenza.test.gain");
			assert!(path.ends_with("cadenza-test.clap"));
		}
		other => panic!("expected a CLAP device, got {other:?}"),
	}
	assert_eq!(d.params.get("0"), Some(&-6.0), "names are stored as ids");

	tracks::set_device_params(
		&mut s,
		args(json!({ "track_id": track, "device_id": device_id, "params": { "GAIN": -12.0 } })),
	)
	.unwrap();
	tracks::set_param(
		&mut s,
		args(json!({ "address": format!("track/{track}/device/{device_id}/Gain"), "value": -3.0 })),
	)
	.unwrap();
	let d = s
		.document()
		.project()
		.device(Some(track), device_id)
		.unwrap();
	assert_eq!(d.params.get("0"), Some(&-3.0));
	assert_eq!(d.params.len(), 1);

	tracks::set_automation(
		&mut s,
		args(json!({
			"address": format!("track/{track}/device/{device_id}/gain"),
			"points": [{ "beat": 0.0, "value": -20.0 }, { "beat": 4.0, "value": 0.0 }]
		})),
	)
	.unwrap();
	let lane = &s.document().project().automation[0];
	assert_eq!(
		lane.address.to_string(),
		format!("track/{track}/device/{device_id}/0")
	);

	let err = tracks::set_device_params(
		&mut s,
		args(json!({ "track_id": track, "device_id": device_id, "params": { "0": 50.0 } })),
	)
	.unwrap_err();
	assert!(matches!(err, McpError::ParamOutOfRange { .. }), "{err}");
	let err = tracks::insert_device(
		&mut s,
		args(json!({ "track_id": track, "device": GAIN, "params": { "drive": 1.0 } })),
	)
	.unwrap_err();
	assert!(matches!(err, McpError::UnknownParam { .. }), "{err}");
	assert!(err.to_string().contains("'Gain' (id 0)"), "{err}");

	// Role and catalog checks.
	let err = tracks::create_track(
		&mut s,
		args(json!({ "name": "x", "kind": "instrument", "instrument": { "device": GAIN } })),
	)
	.unwrap_err();
	assert!(matches!(err, McpError::WrongDeviceRole { .. }), "{err}");
	let err = tracks::insert_device(&mut s, args(json!({ "device": "clap:com.example.nope" })))
		.unwrap_err();
	assert!(err.to_string().contains("scan_plugins"), "{err}");
	// A bare plugin id is accepted too.
	tracks::create_track(
		&mut s,
		args(json!({ "name": "Sine", "kind": "instrument", "instrument": { "device": "dev.cadenza.test.sine" } })),
	)
	.unwrap();
}

#[tokio::test]
async fn renders_song_with_plugin_instrument_and_effect() {
	let mut s = session();
	let track = tracks::create_track(
		&mut s,
		args(json!({
			"name": "Sine",
			"kind": "instrument",
			"instrument": { "device": SINE, "params": { "Level": -12.0 } }
		})),
	)
	.unwrap()
	.track_id;
	clips::add_midi_clip(
		&mut s,
		args(json!({
			"track_id": track,
			"start_beat": 0.0,
			"length_beats": 4.0,
			"notes": [
				{ "pitch": "C4", "start": 0.0, "length": 2.0 },
				{ "pitch": "E4", "start": 0.0, "length": 2.0 },
				{ "pitch": "G4", "start": 2.0, "length": 2.0 }
			]
		})),
	)
	.unwrap();
	tracks::insert_device(
		&mut s,
		args(json!({ "track_id": track, "device": GAIN, "params": { "Gain": -6.0 } })),
	)
	.unwrap();
	tracks::insert_device(
		&mut s,
		args(json!({ "device": GAIN, "params": { "0": 3.0 } })),
	)
	.unwrap();

	let plugins = s.plugins_for_project();
	assert!(plugins.is_some());
	let path = std::env::temp_dir().join(format!(
		"cadenza-mcp-plugin-song-{}.wav",
		std::process::id()
	));
	let report = render::render(
		s.document().project().clone(),
		plugins.clone(),
		plugin_support::sandboxed(),
		args(json!({ "path": path.display().to_string(), "end_beat": 4.0 })),
	)
	.await
	.unwrap();
	assert!(report.warnings.is_empty(), "{:?}", report.warnings);
	assert_eq!(report.latency_samples, 128, "track + master plugin latency");
	assert!(
		report.master.peak_db > -40.0 && report.master.peak_db < 0.0,
		"{:?}",
		report.master
	);
	assert!(report.master.lufs_integrated.is_finite());
	let wav = hound::WavReader::open(&path).unwrap();
	assert_eq!(wav.duration(), 96_000);
	std::fs::remove_file(&path).ok();

	// Without the catalog the plugins are bypassed with warnings, never an error.
	let out = render::analyze_mix(
		s.document().project().clone(),
		None,
		plugin_support::sandboxed(),
		args(json!({ "end_beat": 4.0 })),
	)
	.await
	.unwrap();
	assert_eq!(out.report.warnings.len(), 3, "{:?}", out.report.warnings);
}
