//! The in-app producer ([`SessionAgent`]) against the scripted mock: it edits a session the
//! "app" shares, folds each request into one undo step, keeps the conversation across
//! requests, and stops, fails and condenses cleanly. No network, no cost.

mod mock;

use std::sync::Mutex;

use cadenza_agent::config::ProviderConfig;
use cadenza_agent::http::RetryPolicy;
use cadenza_agent::observer::{AgentEvent, AgentObserver};
use cadenza_agent::prompt::{SESSION_HANDOFF_REQUEST, SESSION_SYSTEM_PROMPT};
use cadenza_agent::session::actionable_error;
use cadenza_agent::{
	AppContext, CancelToken, OpenRouterConfig, RequestStatus, SessionAgent, SessionConfig,
};
use cadenza_mcp::{Session, SharedSession};
use cadenza_model::{Command, Project, Track, TrackKind};
use mock::{MockServer, Recorded, Recorder, Reply, assert_append_only, streamed, usage};
use serde_json::json;

const MODEL: &str = "anthropic/claude-haiku-5.5";

struct Fixture {
	agent: SessionAgent,
	session: SharedSession,
	dir: tempfile::TempDir,
}

fn provider_config(server: &MockServer, openai: bool) -> ProviderConfig {
	let retry = RetryPolicy {
		max_retries: 1,
		base_delay: std::time::Duration::from_millis(10),
		max_delay: std::time::Duration::from_millis(50),
	};
	if openai {
		let mut c = OpenRouterConfig::openai_compatible(server.base_url(), "local/model", None);
		c.retry = retry;
		ProviderConfig::OpenAiCompatible(c)
	} else {
		let mut c = OpenRouterConfig::new(MODEL, Some("test-key".into()));
		c.base_url = server.base_url();
		c.retry = retry;
		ProviderConfig::OpenRouter(c)
	}
}

/// A shared session holding a project with one instrument track "Keys" (as the app would
/// have it open), and an agent over it.
fn fixture(server: &MockServer, tweak: impl FnOnce(&mut SessionConfig)) -> Fixture {
	fixture_with(server, false, tweak)
}

fn fixture_with(
	server: &MockServer,
	openai: bool,
	tweak: impl FnOnce(&mut SessionConfig),
) -> Fixture {
	let dir = tempfile::tempdir().unwrap();
	let mut session = Session::with_plugin_cache(dir.path().join("plugins.json"));
	let mut project = Project::new("Demo");
	project
		.tracks
		.push(Track::new("Keys", TrackKind::Instrument));
	session.reset(cadenza_model::Document::new(project), None);
	let session = session.into_shared();
	let mut config = SessionConfig::new(
		provider_config(server, openai),
		dir.path().join("state/transcript.jsonl"),
	);
	tweak(&mut config);
	let agent = SessionAgent::new(config, session.clone()).unwrap();
	Fixture {
		agent,
		session,
		dir,
	}
}

fn context(f: &Fixture) -> AppContext {
	AppContext {
		files_root: f.dir.path().join("project").display().to_string(),
		selected_tracks: vec!["Keys (instrument)".into()],
		playhead_beats: 8.0,
		..AppContext::default()
	}
}

fn first_user_text(request: &Recorded) -> String {
	let user = request
		.messages()
		.iter()
		.rev()
		.find(|m| m["role"] == "user")
		.unwrap();
	Recorded::text_of(user)
}

fn track_names(session: &SharedSession) -> Vec<String> {
	session
		.lock()
		.unwrap()
		.document()
		.project()
		.tracks
		.iter()
		.map(|t| t.name.clone())
		.collect()
}

#[tokio::test]
async fn requests_edit_the_shared_session_as_one_undo_step_each() {
	let server = MockServer::start(|_, index| match index {
		0 => streamed(
			Some("Bass under the keys."),
			Some("Adding a bass track and a bridge marker."),
			&[
				(
					"c1",
					"create_track",
					r#"{"name":"Bass","kind":"instrument","instrument":{"device":"poly_synth"}}"#,
				),
				("c2", "add_marker", r#"{"beat":32,"name":"Bridge"}"#),
			],
			"tool_calls",
			usage(3000, 100, 0, 0.002),
		),
		1 => streamed(
			None,
			Some("Added **Bass** and a Bridge marker at beat 32."),
			&[],
			"stop",
			usage(3300, 40, 2900, 0.001),
		),
		2 => streamed(
			None,
			Some("Nothing to change."),
			&[],
			"stop",
			usage(3500, 10, 3200, 0.001),
		),
		_ => streamed(
			None,
			Some("Seen."),
			&[],
			"stop",
			usage(3700, 10, 3400, 0.001),
		),
	})
	.await;
	let mut f = fixture(&server, |_| {});
	let recorder = Recorder::default();
	let cancel = CancelToken::new();
	let ctx = context(&f);

	let outcome = f
		.agent
		.run_request("add a bass", &ctx, &recorder, &cancel)
		.await
		.unwrap();
	assert_eq!(
		outcome.status,
		RequestStatus::Completed,
		"{:?}",
		recorder.lines()
	);
	assert!(outcome.edited);
	assert_eq!(outcome.undo_label, "AI: add a bass");
	assert_eq!(
		outcome.final_message.as_deref(),
		Some("Added **Bass** and a Bridge marker at beat 32.")
	);
	assert!((outcome.budget.spent_usd - 0.003).abs() < 1e-9);
	assert_eq!(track_names(&f.session), vec!["Keys", "Bass"]);
	{
		let session = f.session.lock().unwrap();
		let doc = session.document();
		assert_eq!(doc.history_labels(), vec!["AI: add a bass"]);
		assert!(!doc.in_group());
		assert!(!session.is_clean(), "edited, not saved");
	}

	// The wire: session system prompt, app context + project summary in the request
	// message, document-replacing and playback tools hidden.
	let requests = server.requests();
	let first = &requests[0];
	assert_eq!(
		Recorded::text_of(&first.messages()[0]),
		SESSION_SYSTEM_PROMPT
	);
	let message = first_user_text(first);
	assert!(message.starts_with("Request: add a bass"), "{message}");
	assert!(message.contains("Selected tracks: Keys (instrument)"));
	assert!(message.contains("Playhead: beat 8.00"));
	assert!(
		message.contains("\"Keys\""),
		"project summary included: {message}"
	);
	let tools: Vec<&str> = first.body["tools"]
		.as_array()
		.unwrap()
		.iter()
		.map(|t| t["function"]["name"].as_str().unwrap())
		.collect();
	for hidden in ["new_project", "open_project", "play", "stop"] {
		assert!(!tools.contains(&hidden), "{hidden} offered");
	}
	assert!(tools.contains(&"get_project") && tools.contains(&"undo"));

	// Second request: the conversation continues; the project is unchanged since the
	// model's own edits, so no summary is resent.
	let outcome = f
		.agent
		.run_request("anything else?", &ctx, &recorder, &cancel)
		.await
		.unwrap();
	assert_eq!(outcome.status, RequestStatus::Completed);
	assert!(!outcome.edited);
	assert!(first_user_text(&server.requests()[2]).contains("unchanged since your last reply"));

	// The user edits in between: the next request carries the fresh summary.
	{
		let mut session = f.session.lock().unwrap();
		let track = session.document().project().tracks[0].id;
		session
			.document_mut()
			.execute(Command::UpdateTrack {
				track,
				patch: cadenza_model::TrackPatch {
					name: Some("Rhodes".into()),
					..Default::default()
				},
			})
			.unwrap();
	}
	f.agent
		.run_request("and now?", &ctx, &recorder, &cancel)
		.await
		.unwrap();
	let requests = server.requests();
	assert!(first_user_text(&requests[3]).contains("\"Rhodes\""));
	assert_append_only(&requests);
	assert!((f.agent.total().spent_usd - 0.005).abs() < 1e-9);

	// One Ctrl+Z takes the whole first request back.
	let mut session = f.session.lock().unwrap();
	let doc = session.document_mut();
	assert_eq!(doc.history_labels().len(), 2);
	assert_eq!(doc.history_labels()[0], "AI: add a bass");
	doc.undo().unwrap();
	doc.undo().unwrap();
	assert_eq!(doc.project().tracks.len(), 1);
	assert!(doc.project().markers.is_empty());

	// The transcript records the conversation without secrets.
	let transcript = std::fs::read_to_string(f.agent.transcript_path()).unwrap();
	assert!(transcript.contains("\"request_outcome\""));
	assert!(!transcript.contains("test-key"));
}

/// Cancels the token as soon as the first tool result arrives.
struct StopAfterFirstTool {
	cancel: CancelToken,
	seen: Mutex<Vec<String>>,
}

impl AgentObserver for StopAfterFirstTool {
	fn on_event(&self, event: &AgentEvent<'_>) {
		if let AgentEvent::ToolResult { name, .. } = event {
			self.seen.lock().unwrap().push(name.to_string());
			self.cancel.cancel();
		}
	}
}

#[tokio::test]
async fn stop_ends_at_the_next_tool_boundary_and_keeps_one_undo_step() {
	let server = MockServer::start(|_, index| match index {
		0 => streamed(
			None,
			None,
			&[
				("c1", "add_marker", r#"{"beat":0,"name":"Intro"}"#),
				("c2", "add_marker", r#"{"beat":16,"name":"Verse"}"#),
			],
			"tool_calls",
			usage(3000, 50, 0, 0.001),
		),
		_ => streamed(None, Some("ok"), &[], "stop", usage(3100, 5, 0, 0.001)),
	})
	.await;
	let mut f = fixture(&server, |_| {});
	let cancel = CancelToken::new();
	let observer = StopAfterFirstTool {
		cancel: cancel.clone(),
		seen: Mutex::new(Vec::new()),
	};
	let outcome = f
		.agent
		.run_request("mark the sections", &context(&f), &observer, &cancel)
		.await
		.unwrap();
	assert_eq!(outcome.status, RequestStatus::Cancelled);
	assert_eq!(*observer.seen.lock().unwrap(), vec!["add_marker"]);
	assert!(outcome.edited);
	{
		let session = f.session.lock().unwrap();
		assert_eq!(session.document().project().markers.len(), 1);
		assert_eq!(
			session.document().history_labels(),
			vec!["AI: mark the sections"]
		);
		assert!(!session.document().in_group());
	}

	// The next request carries a result for the skipped call and continues normally.
	cancel.reset();
	let outcome = f
		.agent
		.run_request("go on", &context(&f), &Recorder::default(), &cancel)
		.await
		.unwrap();
	assert_eq!(outcome.status, RequestStatus::Completed);
	let requests = server.requests();
	assert!(requests[1].tool_result("c2").contains("stopped"));
	assert_append_only(&requests);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stop_during_the_model_call_aborts_it() {
	let server = MockServer::start(|_, _| {
		std::thread::sleep(std::time::Duration::from_millis(1500));
		streamed(None, Some("late"), &[], "stop", usage(10, 1, 0, 0.0))
	})
	.await;
	let mut f = fixture(&server, |_| {});
	let cancel = CancelToken::new();
	let stopper = cancel.clone();
	tokio::spawn(async move {
		tokio::time::sleep(std::time::Duration::from_millis(100)).await;
		stopper.cancel();
	});
	let started = std::time::Instant::now();
	let outcome = f
		.agent
		.run_request("slow", &context(&f), &Recorder::default(), &cancel)
		.await
		.unwrap();
	assert_eq!(outcome.status, RequestStatus::Cancelled);
	assert!(started.elapsed() < std::time::Duration::from_millis(1400));
	assert!(!outcome.edited);
	assert!(!f.session.lock().unwrap().document().in_group());
}

#[tokio::test]
async fn failures_are_actionable_and_the_conversation_survives_them() {
	let server = MockServer::start(|_, index| match index {
		0 => Reply::status(
			401,
			&[],
			json!({"error": {"code": 401, "message": "bad key"}}),
		),
		_ => streamed(None, Some("Hello."), &[], "stop", usage(100, 5, 0, 0.0001)),
	})
	.await;
	let mut f = fixture(&server, |_| {});
	let cancel = CancelToken::new();
	let outcome = f
		.agent
		.run_request("hi", &context(&f), &Recorder::default(), &cancel)
		.await
		.unwrap();
	assert!(matches!(outcome.status, RequestStatus::Failed(_)));
	let message = actionable_error(outcome.error.as_ref().unwrap());
	assert!(message.contains("API key"), "{message}");
	assert!(!f.session.lock().unwrap().document().in_group());

	let outcome = f
		.agent
		.run_request("hi again", &context(&f), &Recorder::default(), &cancel)
		.await
		.unwrap();
	assert_eq!(outcome.status, RequestStatus::Completed);
	// roles still alternate: user, assistant placeholder, user
	let roles: Vec<String> = server.requests()[1]
		.messages()
		.iter()
		.map(|m| m["role"].as_str().unwrap().to_owned())
		.collect();
	assert_eq!(roles, vec!["system", "user", "assistant", "user"]);
}

#[tokio::test]
async fn the_request_budget_stops_runaway_requests() {
	let server = MockServer::start(|_, index| {
		let args = format!(r#"{{"beat":{},"name":"M{index}"}}"#, index * 4);
		streamed(
			None,
			None,
			&[("c", "add_marker", &args)],
			"tool_calls",
			usage(3000, 50, 0, 0.1),
		)
	})
	.await;
	let mut f = fixture(&server, |c| c.max_cost_usd = 0.25);
	let recorder = Recorder::default();
	let outcome = f
		.agent
		.run_request("loop forever", &context(&f), &recorder, &CancelToken::new())
		.await
		.unwrap();
	assert_eq!(outcome.status, RequestStatus::BudgetExhausted);
	assert_eq!(outcome.turns, 3);
	assert!(recorder.lines().iter().any(|l| l.contains("budget")));
	assert_eq!(
		f.session.lock().unwrap().document().history_labels(),
		vec!["AI: loop forever"]
	);
}

#[tokio::test]
async fn long_conversations_are_condensed_between_requests() {
	let server = MockServer::start(|request, _| {
		let last = mock::Recorded::text_of(request.messages().last().unwrap());
		if last.contains("Context checkpoint") {
			streamed(
				None,
				Some("NOTE: keys track exists."),
				&[],
				"stop",
				usage(500, 20, 0, 0.001),
			)
		} else {
			streamed(
				None,
				Some("Done."),
				&[],
				"stop",
				usage(80_000, 10, 0, 0.001),
			)
		}
	})
	.await;
	let mut f = fixture(&server, |c| c.compact_at_input_tokens = Some(50_000));
	let cancel = CancelToken::new();
	for request in ["one", "two"] {
		let outcome = f
			.agent
			.run_request(request, &context(&f), &Recorder::default(), &cancel)
			.await
			.unwrap();
		assert_eq!(outcome.status, RequestStatus::Completed);
	}
	let requests = server.requests();
	assert_eq!(requests.len(), 3, "one, checkpoint, two");
	assert!(first_user_text(&requests[1]).contains(SESSION_HANDOFF_REQUEST));
	let resumed = first_user_text(&requests[2]);
	assert!(resumed.contains("NOTE: keys track exists."));
	assert!(resumed.contains("Request: two"));
	assert_eq!(
		requests[2].messages().len(),
		2,
		"system + the condensed request"
	);
}

#[tokio::test]
async fn the_openai_compatible_flavor_speaks_plain_chat_completions() {
	let server =
		MockServer::start(|_, _| streamed(None, Some("Hi."), &[], "stop", usage(100, 5, 0, 0.0)))
			.await;
	let mut f = fixture_with(&server, true, |c| {
		c.price = Some(cadenza_agent::budget::Price {
			input: 0.0,
			output: 0.0,
		})
	});
	let outcome = f
		.agent
		.run_request(
			"hello",
			&context(&f),
			&Recorder::default(),
			&CancelToken::new(),
		)
		.await
		.unwrap();
	assert_eq!(outcome.status, RequestStatus::Completed);
	let request = &server.requests()[0];
	assert!(request.header("authorization").is_none());
	assert!(request.body.get("provider").is_none() && request.body.get("usage").is_none());
	assert_eq!(request.body["stream_options"]["include_usage"], true);
	assert_eq!(request.body["model"], "local/model");
}
