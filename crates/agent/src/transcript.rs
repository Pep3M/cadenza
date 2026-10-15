//! `session.jsonl`: one JSON object per line recording requests (metadata only), responses
//! (finish reason, usage, cost), tool calls, retries and the outcome — for debugging and
//! cost audits. Never contains credentials.

use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use crate::error::AgentError;
use crate::observer::{AgentEvent, AgentObserver, clip};

pub struct Transcript {
	path: PathBuf,
	file: Mutex<File>,
}

impl Transcript {
	/// Creates (truncates) the transcript file.
	pub fn create(path: &Path) -> Result<Self, AgentError> {
		let file = File::create(path).map_err(|e| AgentError::io(path, e))?;
		Ok(Self {
			path: path.to_path_buf(),
			file: Mutex::new(file),
		})
	}

	pub fn path(&self) -> &Path {
		&self.path
	}

	/// Appends one record. Write failures are ignored: the transcript is diagnostic and
	/// must never abort a session that is otherwise working.
	pub fn record(&self, kind: &str, mut data: Value) {
		let ts = SystemTime::now()
			.duration_since(UNIX_EPOCH)
			.map(|d| d.as_secs_f64())
			.unwrap_or_default();
		if let Value::Object(map) = &mut data {
			map.insert("ts".into(), json!(ts));
			map.insert("kind".into(), json!(kind));
		} else {
			data = json!({ "ts": ts, "kind": kind, "data": data });
		}
		let mut line = data.to_string();
		line.push('\n');
		let mut file = self.file.lock().unwrap_or_else(|p| p.into_inner());
		let _ = file.write_all(line.as_bytes());
		let _ = file.flush();
	}
}

/// Forwards events to the caller's observer and records the auditable ones.
pub(crate) struct TeeObserver<'a> {
	pub user: &'a dyn AgentObserver,
	pub transcript: &'a Transcript,
}

impl AgentObserver for TeeObserver<'_> {
	fn on_event(&self, event: &AgentEvent<'_>) {
		match event {
			AgentEvent::ToolCall { name, args } => self.transcript.record(
				"tool_call",
				json!({ "name": name, "args": clip(args, 4000) }),
			),
			AgentEvent::ToolResult { name, ok, summary } => self.transcript.record(
				"tool_result",
				json!({ "name": name, "ok": ok, "summary": summary }),
			),
			AgentEvent::Retry {
				attempt,
				wait_ms,
				reason,
			} => self.transcript.record(
				"retry",
				json!({ "attempt": attempt, "wait_ms": wait_ms, "reason": reason }),
			),
			AgentEvent::Notice(text) => self.transcript.record("notice", json!({ "text": text })),
			AgentEvent::Thinking(text) => self
				.transcript
				.record("reasoning", json!({ "text": clip(text, 4000) })),
			AgentEvent::Text(text) => self.transcript.record("text", json!({ "text": text })),
			AgentEvent::Final(text) => self.transcript.record("final", json!({ "text": text })),
			AgentEvent::TurnStarted { .. }
			| AgentEvent::Usage { .. }
			| AgentEvent::TextDelta(_)
			| AgentEvent::ThinkingDelta(_) => {}
		}
		self.user.on_event(event);
	}
}
