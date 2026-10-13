//! Plugin crashes through the MCP server: renders and analyses complete and name the
//! crashed plugin in their warnings, which is what the LLM reads.

// Plugin sandboxing is Unix-only (`SANDBOX_SUPPORTED`).
#![cfg(unix)]

mod plugin_support;

use cadenza_mcp::{CadenzaServer, Session};
use serde_json::{Value, json};

fn server() -> CadenzaServer {
	let dir = std::env::temp_dir().join(format!("cadenza-mcp-sandbox-{}", std::process::id()));
	let mut session = Session::with_plugin_cache(dir.join("never-written.json"));
	session.set_plugins(plugin_support::crash_catalog());
	session.set_plugin_hosting(plugin_support::sandboxed());
	CadenzaServer::with_session(session)
}

fn assert_crash_warning(warnings: &Value) {
	let warnings: Vec<&str> = warnings
		.as_array()
		.expect("warnings array")
		.iter()
		.filter_map(Value::as_str)
		.collect();
	assert_eq!(warnings.len(), 1, "{warnings:?}");
	for needle in [
		"track 'Lead'",
		plugin_support::CRASH_ID,
		"crashed (SIGABRT)",
		"bypassed",
		"render completed",
	] {
		assert!(
			warnings[0].contains(needle),
			"missing {needle:?} in {warnings:?}"
		);
	}
}

#[tokio::test]
async fn render_and_analyze_report_a_crashing_plugin_and_still_complete() {
	let server = server();
	server
		.call_tool_json("new_project", json!({ "name": "Crashy", "bpm": 120.0 }))
		.await
		.unwrap();
	let track = server
		.call_tool_json(
			"create_track",
			json!({ "name": "Lead", "kind": "instrument", "instrument": { "device": "poly_synth" } }),
		)
		.await
		.unwrap();
	let track_id = track["track_id"].as_str().unwrap().to_owned();
	server
		.call_tool_json(
			"add_midi_clip",
			json!({
				"track_id": track_id,
				"start_beat": 0.0,
				"length_beats": 4.0,
				"notes": [{ "pitch": "C4", "start": 0.0, "length": 4.0 }]
			}),
		)
		.await
		.unwrap();
	server
		.call_tool_json(
			"insert_device",
			json!({
				"track_id": track_id,
				"device": format!("clap:{}", plugin_support::CRASH_ID),
				"params": { "crash_now": 1.0 }
			}),
		)
		.await
		.unwrap();

	let path = std::env::temp_dir().join(format!(
		"cadenza-mcp-sandbox-crash-{}.wav",
		std::process::id()
	));
	let report = server
		.call_tool_json(
			"render",
			json!({ "path": path.display().to_string(), "end_beat": 4.0 }),
		)
		.await
		.unwrap();
	assert_crash_warning(&report["warnings"]);
	assert!(report["master"]["peak_db"].as_f64().unwrap() > -40.0);
	assert_eq!(hound::WavReader::open(&path).unwrap().duration(), 96_000);
	std::fs::remove_file(&path).ok();

	let analysis = server
		.call_tool_json("analyze_mix", json!({ "end_beat": 4.0 }))
		.await
		.unwrap();
	assert_crash_warning(&analysis["warnings"]);

	// Nothing is playing, so there is nothing to restart.
	let status = server
		.call_tool_json("restart_plugins", json!({}))
		.await
		.unwrap();
	assert_eq!(status["playing"], false);
	assert!(status.get("plugin_crashes").is_none(), "{status}");
}
