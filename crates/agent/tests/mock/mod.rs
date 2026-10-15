//! A tiny scripted HTTP/1.1 server speaking the OpenRouter (OpenAI Chat Completions) wire
//! format, so agent sessions can be tested end to end without network access or cost.

#![allow(dead_code)]

use std::net::SocketAddr;
use std::path::Path;
use std::sync::{Arc, Mutex};

use cadenza_agent::config::{AgentConfig, ProviderConfig};
use cadenza_agent::http::RetryPolicy;
use cadenza_agent::observer::{AgentEvent, AgentObserver};
use cadenza_agent::{Agent, OpenRouterConfig, OpenRouterProvider};
use cadenza_mcp::{CadenzaServer, Session};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[derive(Debug, Clone)]
pub struct Recorded {
	pub path: String,
	/// Header names lowercased.
	pub headers: Vec<(String, String)>,
	pub body: Value,
}

impl Recorded {
	pub fn header(&self, name: &str) -> Option<&str> {
		self.headers
			.iter()
			.find(|(n, _)| n == name)
			.map(|(_, v)| v.as_str())
	}

	pub fn messages(&self) -> &Vec<Value> {
		self.body["messages"].as_array().expect("messages array")
	}

	/// Text of a message's content (string or array of text parts).
	pub fn text_of(message: &Value) -> String {
		match &message["content"] {
			Value::String(s) => s.clone(),
			Value::Array(parts) => parts
				.iter()
				.filter_map(|p| p["text"].as_str())
				.collect::<Vec<_>>()
				.join(""),
			_ => String::new(),
		}
	}

	/// The content of the tool message answering `call_id`.
	pub fn tool_result(&self, call_id: &str) -> String {
		let message = self
			.messages()
			.iter()
			.find(|m| m["role"] == "tool" && m["tool_call_id"] == call_id)
			.unwrap_or_else(|| panic!("no tool result for {call_id}"));
		Self::text_of(message)
	}
}

pub struct Reply {
	pub status: u16,
	pub headers: Vec<(String, String)>,
	pub body: String,
}

impl Reply {
	pub fn sse(chunks: &[Value]) -> Self {
		let mut body = String::from(": OPENROUTER PROCESSING\n\n");
		for chunk in chunks {
			body.push_str(&format!("data: {chunk}\n\n"));
		}
		body.push_str("data: [DONE]\n\n");
		Self {
			status: 200,
			headers: vec![("content-type".into(), "text/event-stream".into())],
			body,
		}
	}

	pub fn json(value: Value) -> Self {
		Self {
			status: 200,
			headers: vec![("content-type".into(), "application/json".into())],
			body: value.to_string(),
		}
	}

	pub fn status(status: u16, headers: &[(&str, &str)], body: Value) -> Self {
		let mut all: Vec<(String, String)> = headers
			.iter()
			.map(|(k, v)| (k.to_string(), v.to_string()))
			.collect();
		all.push(("content-type".into(), "application/json".into()));
		Self {
			status,
			headers: all,
			body: body.to_string(),
		}
	}
}

type Handler = dyn Fn(&Recorded, usize) -> Reply + Send + Sync;

pub struct MockServer {
	pub addr: SocketAddr,
	requests: Arc<Mutex<Vec<Recorded>>>,
}

impl MockServer {
	pub async fn start(
		handler: impl Fn(&Recorded, usize) -> Reply + Send + Sync + 'static,
	) -> Self {
		let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
		let addr = listener.local_addr().unwrap();
		let requests = Arc::new(Mutex::new(Vec::new()));
		let handler: Arc<Handler> = Arc::new(handler);
		let recorded = requests.clone();
		tokio::spawn(async move {
			loop {
				let Ok((mut socket, _)) = listener.accept().await else {
					return;
				};
				let Some(request) = read_request(&mut socket).await else {
					continue;
				};
				let index = {
					let mut all = recorded.lock().unwrap();
					all.push(request.clone());
					all.len() - 1
				};
				let reply = handler(&request, index);
				let mut head = format!(
					"HTTP/1.1 {} X\r\ncontent-length: {}\r\nconnection: close\r\n",
					reply.status,
					reply.body.len()
				);
				for (k, v) in &reply.headers {
					head.push_str(&format!("{k}: {v}\r\n"));
				}
				head.push_str("\r\n");
				let _ = socket.write_all(head.as_bytes()).await;
				let _ = socket.write_all(reply.body.as_bytes()).await;
				let _ = socket.shutdown().await;
			}
		});
		Self { addr, requests }
	}

	pub fn base_url(&self) -> String {
		format!("http://{}/api/v1", self.addr)
	}

	pub fn requests(&self) -> Vec<Recorded> {
		self.requests.lock().unwrap().clone()
	}
}

async fn read_request(socket: &mut tokio::net::TcpStream) -> Option<Recorded> {
	let mut buf = Vec::new();
	let mut chunk = [0u8; 8192];
	let header_end = loop {
		let n = socket.read(&mut chunk).await.ok()?;
		if n == 0 {
			return None;
		}
		buf.extend_from_slice(&chunk[..n]);
		if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
			break pos + 4;
		}
	};
	let head = String::from_utf8_lossy(&buf[..header_end]).into_owned();
	let mut lines = head.lines();
	let path = lines.next()?.split_whitespace().nth(1)?.to_owned();
	let headers: Vec<(String, String)> = lines
		.filter_map(|l| l.split_once(':'))
		.map(|(k, v)| (k.trim().to_lowercase(), v.trim().to_owned()))
		.collect();
	let length: usize = headers
		.iter()
		.find(|(k, _)| k == "content-length")
		.and_then(|(_, v)| v.parse().ok())
		.unwrap_or(0);
	while buf.len() < header_end + length {
		let n = socket.read(&mut chunk).await.ok()?;
		if n == 0 {
			break;
		}
		buf.extend_from_slice(&chunk[..n]);
	}
	let body = serde_json::from_slice(&buf[header_end..header_end + length]).unwrap_or(Value::Null);
	Some(Recorded {
		path,
		headers,
		body,
	})
}

// -- response builders -----------------------------------------------------------------

pub fn usage(prompt: u64, completion: u64, cached: u64, cost: f64) -> Value {
	json!({
		"prompt_tokens": prompt,
		"completion_tokens": completion,
		"total_tokens": prompt + completion,
		"cost": cost,
		"prompt_tokens_details": { "cached_tokens": cached },
	})
}

/// One streamed response: optional reasoning and text, tool calls (each split into two
/// argument fragments to exercise accumulation), a finish reason and a usage chunk.
pub fn streamed(
	reasoning: Option<&str>,
	text: Option<&str>,
	calls: &[(&str, &str, &str)],
	finish: &str,
	usage: Value,
) -> Reply {
	let mut chunks = vec![
		json!({"id": "gen-mock", "model": "mock/served", "choices": [{"index": 0, "delta": {"role": "assistant"}}]}),
	];
	if let Some(r) = reasoning {
		chunks.push(json!({"choices": [{"index": 0, "delta": {"reasoning": r,
			"reasoning_details": [{"type": "reasoning.text", "index": 0, "text": r, "signature": "sig-mock"}]}}]}));
	}
	if let Some(t) = text {
		let (a, b) = t.split_at(t.len() / 2);
		chunks.push(json!({"choices": [{"index": 0, "delta": {"content": a}}]}));
		chunks.push(json!({"choices": [{"index": 0, "delta": {"content": b}}]}));
	}
	for (i, (id, name, args)) in calls.iter().enumerate() {
		let split = args
			.char_indices()
			.nth(args.chars().count() / 2)
			.map_or(0, |(i, _)| i);
		let (a, b) = args.split_at(split);
		chunks.push(json!({"choices": [{"index": 0, "delta": {"tool_calls": [
			{"index": i, "id": id, "type": "function", "function": {"name": name, "arguments": a}}]}}]}));
		chunks.push(json!({"choices": [{"index": 0, "delta": {"tool_calls": [
			{"index": i, "function": {"arguments": b}}]}}]}));
	}
	chunks.push(json!({"choices": [{"index": 0, "delta": {}, "finish_reason": finish}]}));
	chunks.push(json!({"choices": [], "usage": usage}));
	Reply::sse(&chunks)
}

pub fn completion(
	text: Option<&str>,
	calls: &[(&str, &str, &str)],
	finish: &str,
	usage: Value,
) -> Reply {
	let tool_calls: Vec<Value> = calls
		.iter()
		.map(|(id, name, args)| json!({"id": id, "type": "function", "function": {"name": name, "arguments": args}}))
		.collect();
	let mut message = json!({"role": "assistant", "content": text});
	if !tool_calls.is_empty() {
		message["tool_calls"] = Value::Array(tool_calls);
	}
	Reply::json(json!({
		"id": "gen-json", "model": "mock/served",
		"choices": [{"index": 0, "finish_reason": finish, "message": message}],
		"usage": usage,
	}))
}

// -- agent setup ---------------------------------------------------------------------------

pub struct Setup {
	pub agent: Agent,
	pub out: tempfile::TempDir,
	_plugins: tempfile::TempDir,
}

pub fn setup(
	server: &MockServer,
	model: &str,
	stream: bool,
	tweak: impl FnOnce(&mut AgentConfig),
) -> Setup {
	let out = tempfile::tempdir().unwrap();
	let plugins = tempfile::tempdir().unwrap();
	let mut provider = OpenRouterConfig::new(model, Some("test-key".into()));
	provider.base_url = server.base_url();
	provider.stream = stream;
	provider.retry = RetryPolicy {
		max_retries: 3,
		base_delay: std::time::Duration::from_millis(10),
		max_delay: std::time::Duration::from_secs(2),
	};
	let mut config = AgentConfig::new(
		ProviderConfig::OpenRouter(provider.clone()),
		out.path(),
		"rain",
	);
	tweak(&mut config);
	let provider = config.provider.chat_config().clone();
	let session = Session::with_plugin_cache(plugins.path().join("plugins.json"));
	let agent = Agent::with_parts(
		config,
		Box::new(OpenRouterProvider::new(provider).unwrap()),
		CadenzaServer::with_session(session),
	)
	.unwrap();
	Setup {
		agent,
		out,
		_plugins: plugins,
	}
}

/// Records every event as a line of text.
#[derive(Default)]
pub struct Recorder {
	pub events: Mutex<Vec<String>>,
}

impl Recorder {
	pub fn lines(&self) -> Vec<String> {
		self.events.lock().unwrap().clone()
	}
}

impl AgentObserver for Recorder {
	fn on_event(&self, event: &AgentEvent<'_>) {
		let line = match event {
			AgentEvent::TurnStarted { turn } => format!("turn {turn}"),
			AgentEvent::Thinking(t) => format!("thinking {t}"),
			AgentEvent::Text(t) => format!("text {t}"),
			AgentEvent::ToolCall { name, args } => format!("call {name} {args}"),
			AgentEvent::ToolResult { name, ok, summary } => format!("result {name} {ok} {summary}"),
			AgentEvent::Usage { total_cost_usd, .. } => format!("usage {total_cost_usd:.4}"),
			AgentEvent::Retry {
				attempt, wait_ms, ..
			} => format!("retry {attempt} {wait_ms}"),
			AgentEvent::Notice(t) => format!("notice {t}"),
			AgentEvent::Final(t) => format!("final {t}"),
			// Streaming fragments are covered by the provider tests.
			AgentEvent::TextDelta(_) | AgentEvent::ThinkingDelta(_) => return,
		};
		self.events.lock().unwrap().push(line);
	}
}

/// Removes every `cache_control` key, recursively.
pub fn strip_cache_control(value: &Value) -> Value {
	match value {
		Value::Object(map) => Value::Object(
			map.iter()
				.filter(|(k, _)| k.as_str() != "cache_control")
				.map(|(k, v)| (k.clone(), strip_cache_control(v)))
				.collect(),
		),
		Value::Array(items) => Value::Array(items.iter().map(strip_cache_control).collect()),
		other => other.clone(),
	}
}

/// Asserts each request's messages start with the previous request's messages, byte for
/// byte once the rolling cache marker is ignored.
pub fn assert_append_only(requests: &[Recorded]) {
	for pair in requests.windows(2) {
		let before: Vec<String> = pair[0]
			.messages()
			.iter()
			.map(|m| strip_cache_control(m).to_string())
			.collect();
		let after: Vec<String> = pair[1]
			.messages()
			.iter()
			.map(|m| strip_cache_control(m).to_string())
			.collect();
		assert!(after.len() > before.len(), "history did not grow");
		assert_eq!(
			&after[..before.len()],
			&before[..],
			"history prefix changed"
		);
	}
}

pub fn file_exists(path: &Path) -> bool {
	std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.len() > 0)
}
