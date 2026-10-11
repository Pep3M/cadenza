//! Integration test: runs `CadenzaServer` over an in-memory duplex transport and drives
//! it with a real (minimal) MCP client, exercising `tools/list` and one `tools/call`.

use cadenza_mcp::CadenzaServer;
use rmcp::ServiceExt;
use rmcp::model::CallToolRequestParams;

#[tokio::test]
async fn lists_and_calls_a_tool_over_a_real_mcp_connection() {
	let (client_io, server_io) = tokio::io::duplex(64 * 1024);

	let server_task = tokio::spawn(async move {
		let running = CadenzaServer::new()
			.serve(server_io)
			.await
			.expect("server failed to initialize");
		running.waiting().await.expect("server task panicked");
	});

	let client = ().serve(client_io).await.expect("client failed to initialize");

	// tools/list: every tool from the spec should be advertised, with a non-empty
	// description and a non-trivial input schema.
	let tools = client.list_tools(None).await.expect("tools/list failed");
	let names: Vec<&str> = tools.tools.iter().map(|t| t.name.as_ref()).collect();
	for expected in [
		"new_project",
		"get_project",
		"create_track",
		"insert_device",
		"add_midi_clip",
		"write_chord_progression",
		"write_drum_pattern",
		"set_tempo",
		"undo",
	] {
		assert!(
			names.contains(&expected),
			"missing tool '{expected}' in tools/list: {names:?}"
		);
	}
	let create_track = tools
		.tools
		.iter()
		.find(|t| t.name == "create_track")
		.unwrap();
	assert!(
		create_track
			.description
			.as_deref()
			.is_some_and(|d| !d.is_empty())
	);

	// tools/call: create a new project, then a track, then write a chord progression,
	// exercising a full JSON-RPC request/response round trip.
	let new_project = client
		.call_tool(
			CallToolRequestParams::new("new_project").with_arguments(
				serde_json::json!({"name": "Integration Song", "bpm": 110.0})
					.as_object()
					.unwrap()
					.clone(),
			),
		)
		.await
		.expect("new_project call failed");
	assert_ne!(
		new_project.is_error,
		Some(true),
		"new_project returned an error: {new_project:?}"
	);
	let summary = new_project
		.structured_content
		.expect("new_project should return structured content");
	assert_eq!(summary["name"], "Integration Song");

	let create_track = client
		.call_tool(
			CallToolRequestParams::new("create_track").with_arguments(
				serde_json::json!({"name": "Lead", "kind": "instrument"})
					.as_object()
					.unwrap()
					.clone(),
			),
		)
		.await
		.expect("create_track call failed");
	assert_ne!(
		create_track.is_error,
		Some(true),
		"create_track returned an error: {create_track:?}"
	);
	let track_out = create_track
		.structured_content
		.expect("create_track should return structured content");
	let track_id = track_out["track_id"]
		.as_str()
		.expect("track_id should be a string")
		.to_owned();

	let bad_call = client
		.call_tool(
			CallToolRequestParams::new("create_track")
				.with_arguments(serde_json::json!({"name": "Bad", "kind": "instrument", "instrument": {"device": "not_a_real_device"}}).as_object().unwrap().clone()),
		)
		.await;
	let err = bad_call.expect_err("expected an unknown-device error");
	assert!(err.to_string().contains("not_a_real_device"), "{err}");

	let progression = client
		.call_tool(
			CallToolRequestParams::new("write_chord_progression").with_arguments(
				serde_json::json!({
					"track_id": track_id,
					"start_beat": 0.0,
					"progression": "Cmaj7 G7",
					"beats_per_chord": 2.0,
				})
				.as_object()
				.unwrap()
				.clone(),
			),
		)
		.await
		.expect("write_chord_progression call failed");
	assert_ne!(
		progression.is_error,
		Some(true),
		"write_chord_progression returned an error: {progression:?}"
	);
	let chords = progression
		.structured_content
		.expect("expected structured content")["chords"]
		.as_array()
		.expect("chords should be an array")
		.len();
	assert_eq!(chords, 2);

	client.cancel().await.expect("client cancel failed");
	server_task.await.expect("server task join failed");
}
