//! End-to-end agent sessions against a scripted OpenRouter mock: real Cadenza tools, no
//! network, no cost.

mod mock;

use cadenza_agent::StopStatus;
use cadenza_agent::prompt::{HANDOFF_REQUEST, LENGTH_CONTINUE};
use mock::{
	MockServer, Recorded, Recorder, Reply, assert_append_only, completion, file_exists, setup,
	streamed, usage,
};
use serde_json::{Value, json};

const MODEL: &str = "anthropic/claude-haiku-5.5";

fn track_id_from(request: &Recorded, call_id: &str) -> String {
	let result: Value = serde_json::from_str(&request.tool_result(call_id)).unwrap();
	result["track_id"].as_str().unwrap().to_owned()
}

#[tokio::test]
async fn full_session_writes_parts_and_renders_with_correct_wire_format() {
	let server = MockServer::start(|request, index| match index {
		0 => streamed(
			Some("Sad lo-fi in A minor at 72 bpm."),
			Some("I'll write a sad lo-fi ballad."),
			&[
				("c1", "new_project", r#"{"name":"Rain","bpm":72}"#),
				("c2", "add_marker", r#"{"beat":0,"name":"Intro"}"#),
			],
			"tool_calls",
			usage(3000, 120, 0, 0.001),
		),
		1 => streamed(
			None,
			None,
			&[
				(
					"c3",
					"create_track",
					r#"{"name":"Keys","kind":"instrument","instrument":{"device":"electric_piano"}}"#,
				),
				("c4", "create_track", r#"{"name": "Pad", "#),
				("c5", "save_project", r#"{"path":"/etc/evil.cadenza.json"}"#),
				(
					"c6",
					"create_track",
					r#"{"name":"P","kind":"instrument","instrument":{"device":"clap:org.surge-synth-team.surge-xt"}}"#,
				),
			],
			"tool_calls",
			usage(3500, 200, 2900, 0.001),
		),
		2 => {
			let track = track_id_from(request, "c3");
			let args = json!({"track_id": track, "start_beat": 0, "progression": "Am F C G", "beats_per_chord": 4}).to_string();
			streamed(None, None, &[("c7", "write_chord_progression", &args)], "tool_calls", usage(4200, 90, 3400, 0.001))
		}
		3 => streamed(None, Some("Now I will save and"), &[], "length", usage(4600, 16000, 4100, 0.001)),
		4 => streamed(
			None,
			None,
			&[
				("c8", "save_project", r#"{"path":"rain.cadenza.json"}"#),
				("c9", "render", r#"{"path":"rain.wav"}"#),
			],
			"tool_calls",
			usage(4700, 80, 4500, 0.001),
		),
		_ => streamed(
			None,
			Some("Rain: a 4-bar sad ballad in A minor at 72 bpm on electric piano."),
			&[],
			"stop",
			usage(5000, 40, 4600, 0.001),
		),
	})
	.await;
	let mut s = setup(&server, MODEL, true, |_| {});
	let recorder = Recorder::default();
	let outcome = s.agent.run("sad lo-fi ballad", &recorder).await.unwrap();

	assert_eq!(outcome.status, StopStatus::Completed);
	assert!(!outcome.finalized_by_harness);
	assert_eq!(outcome.turns, 6);
	assert_eq!(
		outcome.final_message.as_deref(),
		Some("Rain: a 4-bar sad ballad in A minor at 72 bpm on electric piano.")
	);
	let out = s.out.path().canonicalize().unwrap();
	assert_eq!(
		outcome.wav_path.as_deref(),
		Some(out.join("rain.wav").as_path())
	);
	assert_eq!(
		outcome.project_path.as_deref(),
		Some(out.join("rain.cadenza.json").as_path())
	);
	assert!(file_exists(&out.join("rain.wav")));
	assert!(file_exists(&out.join("rain.cadenza.json")));
	assert!(
		outcome
			.loudness
			.as_deref()
			.is_some_and(|l| l.contains("LUFS"))
	);
	assert!((outcome.budget.spent_usd - 0.006).abs() < 1e-9);
	assert_eq!(outcome.budget.estimated_usd, 0.0);
	assert_eq!(
		outcome.budget.cache_read_tokens,
		2900 + 3400 + 4100 + 4500 + 4600
	);
	assert!(!out.join("..").join("evil.cadenza.json").exists());

	let requests = server.requests();
	assert_eq!(requests.len(), 6);

	// headers and request shape
	let first = &requests[0];
	assert_eq!(first.path, "/api/v1/chat/completions");
	assert_eq!(first.header("authorization"), Some("Bearer test-key"));
	assert_eq!(first.header("x-title"), Some("Cadenza"));
	assert_eq!(
		first.header("http-referer"),
		Some("https://github.com/Pep3M/cadenza")
	);
	let body = &first.body;
	assert_eq!(body["model"], MODEL);
	assert_eq!(body["stream"], true);
	assert_eq!(body["usage"]["include"], true);
	assert_eq!(body["reasoning"]["effort"], "medium");
	assert_eq!(body["provider"]["require_parameters"], true);
	assert_eq!(body["tool_choice"], "auto");
	let tools = body["tools"].as_array().unwrap();
	assert_eq!(tools.len(), s.agent.tools().len());
	assert!(tools.len() >= 40);
	for tool in tools {
		assert_eq!(tool["type"], "function");
		let name = tool["function"]["name"].as_str().unwrap();
		assert!(
			!["play", "stop", "transport_status", "scan_plugins"].contains(&name),
			"{name} offered"
		);
		assert_eq!(tool["function"]["parameters"]["type"], "object", "{name}");
		let schema = tool["function"]["parameters"].to_string();
		assert!(
			!schema.contains("$ref") && !schema.contains("$schema"),
			"{name}: {schema}"
		);
	}

	// cache breakpoints: the system part and the latest message, nothing else
	for request in &requests {
		let messages = request.messages();
		assert_eq!(request.body.to_string().matches("cache_control").count(), 2);
		assert_eq!(messages[0]["role"], "system");
		assert_eq!(
			messages[0]["content"][0]["cache_control"]["type"],
			"ephemeral"
		);
		let last = messages.last().unwrap();
		assert_eq!(last["content"][0]["cache_control"]["type"], "ephemeral");
	}

	// parallel calls: assistant turn, then one tool message per call, in call order
	let m = requests[1].messages();
	let roles: Vec<&str> = m.iter().map(|x| x["role"].as_str().unwrap()).collect();
	assert_eq!(roles, ["system", "user", "assistant", "tool", "tool"]);
	assert_eq!(m[2]["tool_calls"][0]["id"], "c1");
	assert_eq!(
		m[2]["tool_calls"][0]["function"]["arguments"],
		r#"{"name":"Rain","bpm":72}"#
	);
	assert_eq!(m[2]["reasoning_details"][0]["signature"], "sig-mock");
	assert_eq!(m[2]["content"], "I'll write a sad lo-fi ballad.");
	assert_eq!(
		(m[3]["tool_call_id"].as_str(), m[4]["tool_call_id"].as_str()),
		(Some("c1"), Some("c2"))
	);
	assert!(Recorded::text_of(&m[3]).contains("\"name\":\"Rain\""));

	// errors go back as error results, and nothing outside the folder is written
	let r = &requests[2];
	assert!(!r.tool_result("c3").starts_with("Error"));
	assert!(
		r.tool_result("c4")
			.starts_with("Error: tool arguments are not valid JSON"),
		"{}",
		r.tool_result("c4")
	);
	assert!(
		r.tool_result("c5").contains("outside the session folder"),
		"{}",
		r.tool_result("c5")
	);
	assert!(
		r.tool_result("c6").contains("plugins are disabled"),
		"{}",
		r.tool_result("c6")
	);
	assert!(requests[3].tool_result("c7").contains("\"clip_id\""));

	// output limit without tool calls: the turn is kept and the model is asked to continue
	let m = requests[4].messages();
	assert_eq!(m[m.len() - 2]["content"], "Now I will save and");
	assert_eq!(Recorded::text_of(m.last().unwrap()), LENGTH_CONTINUE);
	assert!(requests[5].tool_result("c9").contains("lufs_integrated"));

	assert_append_only(&requests);

	let lines = recorder.lines();
	assert!(lines.iter().any(|l| l.starts_with("thinking Sad lo-fi")));
	assert!(lines.iter().any(|l| l.starts_with("call new_project")));
	assert!(
		lines
			.iter()
			.any(|l| l.starts_with("result render true") && l.contains("LUFS"))
	);
	assert!(lines.iter().any(|l| l.starts_with("final Rain:")));

	let transcript = std::fs::read_to_string(&outcome.transcript_path).unwrap();
	assert!(!transcript.contains("test-key"));
	let kinds: Vec<String> = transcript
		.lines()
		.map(|l| {
			serde_json::from_str::<Value>(l).unwrap()["kind"]
				.as_str()
				.unwrap()
				.to_owned()
		})
		.collect();
	assert_eq!(kinds.first().map(String::as_str), Some("session_start"));
	assert_eq!(kinds.last().map(String::as_str), Some("outcome"));
	assert_eq!(kinds.iter().filter(|k| *k == "response").count(), 6);
	assert!(transcript.contains("\"cache_read_tokens\":2900"));
}

#[tokio::test]
async fn transient_failures_are_retried_and_fatal_ones_end_the_session_with_outputs() {
	let server = MockServer::start(|_, index| match index {
		0 => Reply::status(
			429,
			&[("retry-after", "0.2")],
			json!({"error": {"code": 429, "message": "slow down"}}),
		),
		1 => Reply::json(json!({"error": {"code": 502, "message": "upstream overloaded"}})),
		2 => completion(
			Some("Starting."),
			&[("c1", "new_project", r#"{"name":"Rain","bpm":80}"#)],
			"tool_calls",
			usage(100, 10, 0, 0.0001),
		),
		_ => Reply::status(
			400,
			&[],
			json!({"error": {"code": 400, "message": "invalid request"}}),
		),
	})
	.await;
	let mut s = setup(&server, "google/gemini-3.8-flash", false, |_| {});
	let recorder = Recorder::default();
	let outcome = s.agent.run("anything", &recorder).await.unwrap();

	let requests = server.requests();
	assert_eq!(requests.len(), 4, "400 must not be retried");
	assert_eq!(requests[0].body["stream"], false);
	assert!(
		!requests[0].body.to_string().contains("cache_control"),
		"no breakpoints for non-anthropic models"
	);
	let retries: Vec<String> = recorder
		.lines()
		.into_iter()
		.filter(|l| l.starts_with("retry"))
		.collect();
	assert_eq!(retries.len(), 2, "{retries:?}");
	assert_eq!(retries[0], "retry 1 200", "retry-after is honored");
	assert!(retries[1].starts_with("retry 2 "), "{retries:?}");
	assert!(
		matches!(&outcome.status, StopStatus::Failed(m) if m.contains("400")),
		"{:?}",
		outcome.status
	);
	assert!(outcome.finalized_by_harness);
	let project = outcome.project_path.expect("harness saved the project");
	let saved: Value = serde_json::from_str(&std::fs::read_to_string(project).unwrap()).unwrap();
	assert_eq!(saved["name"], "Rain");
}

#[tokio::test]
async fn reported_cost_triggers_a_wrap_up_note_and_then_stops_the_session() {
	let server = MockServer::start(|_, index| {
		let (call, cost) = match index {
			0 => (("c1", "new_project", r#"{"name":"Rain"}"#), 0.45),
			1 => (("c2", "add_marker", r#"{"beat":0,"name":"Intro"}"#), 0.40),
			_ => (("c3", "get_project", ""), 0.30),
		};
		streamed(None, None, &[call], "tool_calls", usage(1000, 50, 0, cost))
	})
	.await;
	let mut s = setup(&server, MODEL, true, |c| c.max_cost_usd = 1.0);
	let recorder = Recorder::default();
	let outcome = s.agent.run("anything", &recorder).await.unwrap();

	assert_eq!(outcome.status, StopStatus::BudgetExhausted);
	let requests = server.requests();
	assert_eq!(requests.len(), 3);
	let last = Recorded::text_of(requests[2].messages().last().unwrap());
	assert!(
		last.starts_with("Session note: about 85% of the budget"),
		"{last}"
	);
	assert!((outcome.budget.spent_usd - 1.15).abs() < 1e-9);
	assert!(outcome.finalized_by_harness);
	assert!(outcome.project_path.is_some());
	assert_append_only(&requests);
}

#[tokio::test]
async fn content_filter_stops_without_running_the_declined_turn() {
	let server = MockServer::start(|_, index| match index {
		0 => streamed(
			None,
			None,
			&[("c1", "new_project", r#"{"name":"Rain"}"#)],
			"tool_calls",
			usage(10, 5, 0, 0.0),
		),
		_ => streamed(
			None,
			Some("I can't"),
			&[(
				"c2",
				"create_track",
				r#"{"name":"Keys","kind":"instrument"}"#,
			)],
			"content_filter",
			usage(10, 5, 0, 0.0),
		),
	})
	.await;
	let mut s = setup(&server, MODEL, true, |_| {});
	let outcome = s.agent.run("anything", &Recorder::default()).await.unwrap();
	assert_eq!(outcome.status, StopStatus::Refused);
	assert_eq!(server.requests().len(), 2);
	let saved: Value =
		serde_json::from_str(&std::fs::read_to_string(outcome.project_path.unwrap()).unwrap())
			.unwrap();
	assert_eq!(
		saved["tracks"].as_array().unwrap().len(),
		0,
		"the declined call must not run"
	);
}

#[tokio::test]
async fn truncated_tool_calls_get_error_results_and_unfinished_sessions_are_nudged() {
	let server = MockServer::start(|_, index| match index {
		0 => streamed(
			None,
			None,
			&[("c1", "new_project", r#"{"name":"Ra"#)],
			"length",
			usage(10, 5, 0, 0.0),
		),
		_ => streamed(None, Some("All done."), &[], "stop", usage(10, 5, 0, 0.0)),
	})
	.await;
	let mut s = setup(&server, MODEL, true, |_| {});
	let outcome = s.agent.run("anything", &Recorder::default()).await.unwrap();

	let requests = server.requests();
	assert_eq!(
		requests.len(),
		4,
		"one retry of the cut-off call, then two nudges"
	);
	assert!(
		requests[1]
			.tool_result("c1")
			.starts_with("Error: Not executed")
	);
	for request in &requests[2..] {
		let last = Recorded::text_of(request.messages().last().unwrap());
		assert!(
			last.contains("still needs to be saved as \"rain.cadenza.json\""),
			"{last}"
		);
	}
	assert_eq!(outcome.status, StopStatus::Completed);
	assert!(outcome.finalized_by_harness);
	assert_eq!(outcome.final_message.as_deref(), Some("All done."));
	assert!(outcome.project_path.is_some());
	assert_append_only(&requests);
}

#[tokio::test]
async fn long_contexts_are_condensed_into_a_hand_off_note() {
	let server = MockServer::start(|_, index| match index {
		0 => streamed(
			None,
			None,
			&[("c1", "new_project", r#"{"name":"Rain","bpm":70}"#)],
			"tool_calls",
			usage(5000, 10, 0, 0.0),
		),
		1 => streamed(
			None,
			Some("NOTE: project Rain at 70 bpm, nothing else yet."),
			&[],
			"stop",
			usage(5100, 30, 0, 0.0),
		),
		2 => streamed(
			None,
			None,
			&[
				("c2", "save_project", r#"{}"#),
				("c3", "render", r#"{"path":"rain.wav"}"#),
			],
			"tool_calls",
			usage(900, 10, 0, 0.0),
		),
		_ => streamed(None, Some("Done."), &[], "stop", usage(950, 5, 0, 0.0)),
	})
	.await;
	let mut s = setup(&server, MODEL, true, |c| {
		c.compact_at_input_tokens = Some(1000)
	});
	let outcome = s.agent.run("anything", &Recorder::default()).await.unwrap();

	let requests = server.requests();
	assert_eq!(requests.len(), 4);
	assert_eq!(
		Recorded::text_of(requests[1].messages().last().unwrap()),
		HANDOFF_REQUEST
	);
	let resumed = requests[2].messages();
	assert_eq!(resumed.len(), 2, "system + one condensed user message");
	let text = Recorded::text_of(&resumed[1]);
	assert!(text.starts_with("Brief: anything"));
	assert!(text.contains("NOTE: project Rain at 70 bpm"));
	assert!(text.contains("Current project state") && text.contains("\"name\":\"Rain\""));
	assert_eq!(outcome.status, StopStatus::Completed);
	assert!(
		!outcome.finalized_by_harness,
		"save_project without a path uses the session file"
	);
	let transcript = std::fs::read_to_string(&outcome.transcript_path).unwrap();
	assert!(transcript.contains("\"kind\":\"checkpoint\""));
}

#[tokio::test]
async fn missing_credentials_fail_before_any_request() {
	let server = MockServer::start(|_, _| Reply::json(json!({}))).await;
	let s = setup(&server, MODEL, true, |_| {});
	// rebuild with no key
	let mut config = s.agent.config().clone();
	let cadenza_agent::ProviderConfig::OpenRouter(mut provider) = config.provider.clone();
	provider.api_key = None;
	config.provider = cadenza_agent::ProviderConfig::OpenRouter(provider.clone());
	let mut agent = cadenza_agent::Agent::with_parts(
		config,
		Box::new(cadenza_agent::OpenRouterProvider::new(provider).unwrap()),
		cadenza_mcp::CadenzaServer::new(),
	)
	.unwrap();
	let err = agent
		.run("anything", &Recorder::default())
		.await
		.unwrap_err();
	assert!(err.to_string().contains("OPENROUTER_API_KEY"), "{err}");
	assert!(server.requests().is_empty());
}

#[tokio::test]
async fn dry_run_preview_shows_the_first_request_without_secrets() {
	let server = MockServer::start(|_, _| Reply::json(json!({}))).await;
	let s = setup(&server, MODEL, true, |c| c.allow_plugins = true);
	let preview = s.agent.preview_first_request("sad piano");
	assert_eq!(preview.method, "POST");
	assert!(
		preview
			.headers
			.iter()
			.any(|(k, v)| k == "Authorization" && v == "Bearer <redacted>")
	);
	let body = preview.body.to_string();
	assert!(!body.contains("test-key"));
	let tools = preview.body["tools"].as_array().unwrap();
	assert!(
		tools
			.iter()
			.any(|t| t["function"]["name"] == "scan_plugins"),
		"plugins allowed"
	);
	assert_eq!(
		preview.body["messages"][0]["content"][0]["text"],
		s.agent.system_prompt()
	);
	assert!(server.requests().is_empty());
}
