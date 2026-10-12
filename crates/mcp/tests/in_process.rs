//! The in-process tool API used by the embedded agent: same router, no MCP transport.

use cadenza_mcp::{CadenzaServer, Session};
use serde_json::json;

fn server() -> (CadenzaServer, tempfile::TempDir) {
	let dir = tempfile::tempdir().unwrap();
	let session = Session::with_plugin_cache(dir.path().join("plugins.json"));
	(CadenzaServer::with_session(session), dir)
}

#[test]
fn tool_definitions_cover_the_router_sorted_with_object_schemas() {
	let (server, _dir) = server();
	let defs = server.tool_definitions();
	assert!(defs.len() >= 40, "only {} tools", defs.len());
	let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
	let mut sorted = names.clone();
	sorted.sort();
	assert_eq!(names, sorted);
	for name in [
		"new_project",
		"render",
		"analyze_mix",
		"write_chord_progression",
	] {
		assert!(names.contains(&name), "missing {name}");
	}
	for def in &defs {
		assert!(
			!def.description.is_empty(),
			"{} has no description",
			def.name
		);
		assert_eq!(def.input_schema["type"], "object", "{}", def.name);
	}
}

#[tokio::test]
async fn call_tool_json_runs_tools_and_reports_errors_as_text() {
	let (server, _dir) = server();
	let summary = server
		.call_tool_json("new_project", json!({"name": "In Process", "bpm": 90.0}))
		.await
		.unwrap();
	assert_eq!(summary["name"], "In Process");

	let track = server
		.call_tool_json(
			"create_track",
			json!({"name": "Keys", "kind": "instrument", "instrument": {"device": "electric_piano"}}),
		)
		.await
		.unwrap();
	let track_id = track["track_id"].as_str().unwrap().to_owned();

	// a unit-returning tool
	let ok = server
		.call_tool_json(
			"set_channel",
			json!({"track_id": track_id, "volume_db": -6.0}),
		)
		.await
		.unwrap();
	assert_eq!(ok, json!({"ok": true}));

	// a tool without parameters, called with null args
	let project = server
		.call_tool_json("get_project", serde_json::Value::Null)
		.await
		.unwrap();
	assert_eq!(project["tracks"].as_array().unwrap().len(), 1);

	let bad_args = server
		.call_tool_json("create_track", json!({"kind": "instrument"}))
		.await
		.unwrap_err();
	assert!(bad_args.contains("name"), "{bad_args}");

	let bad_device = server
		.call_tool_json(
			"create_track",
			json!({"name": "X", "kind": "instrument", "instrument": {"device": "nope"}}),
		)
		.await
		.unwrap_err();
	assert!(bad_device.contains("nope"), "{bad_device}");

	let unknown = server
		.call_tool_json("frobnicate", json!({}))
		.await
		.unwrap_err();
	assert!(unknown.contains("frobnicate"), "{unknown}");

	let not_object = server
		.call_tool_json("get_project", json!([1]))
		.await
		.unwrap_err();
	assert!(not_object.contains("object"), "{not_object}");
}
