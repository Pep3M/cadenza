//! A provider that replays canned turns instead of calling a model: offline demos and
//! tests of whatever drives an agent (the app's chat panel) without network or cost.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;

use crate::observer::{AgentEvent, AgentObserver};
use crate::provider::{
	Capabilities, ChatRequest, ChatResponse, FinishReason, LlmProvider, ProviderError,
	RequestPreview, Usage,
};
use crate::types::{AssistantTurn, ToolCall};

/// One canned reply.
#[derive(Debug, Clone)]
pub enum ScriptedTurn {
	/// Text (streamed word by word) and tool calls `(name, json arguments)`.
	Reply {
		text: Option<String>,
		tool_calls: Vec<(String, String)>,
		cost_usd: f64,
	},
	Fail(ProviderError),
}

impl ScriptedTurn {
	pub fn text(text: impl Into<String>) -> Self {
		Self::Reply {
			text: Some(text.into()),
			tool_calls: Vec::new(),
			cost_usd: 0.0,
		}
	}

	pub fn tools(text: Option<&str>, calls: &[(&str, &str)]) -> Self {
		Self::Reply {
			text: text.map(str::to_owned),
			tool_calls: calls
				.iter()
				.map(|(n, a)| ((*n).to_owned(), (*a).to_owned()))
				.collect(),
			cost_usd: 0.0,
		}
	}

	pub fn with_cost(mut self, cost: f64) -> Self {
		if let Self::Reply { cost_usd, .. } = &mut self {
			*cost_usd = cost;
		}
		self
	}
}

/// Clones share the script, so a factory can hand out one provider per request.
#[derive(Clone)]
pub struct ScriptedProvider {
	turns: Arc<Mutex<VecDeque<ScriptedTurn>>>,
	delay: Duration,
	calls: Arc<Mutex<u64>>,
}

impl ScriptedProvider {
	/// Replays `turns` in order; once they run out every request answers "Done.".
	pub fn new(turns: impl IntoIterator<Item = ScriptedTurn>) -> Self {
		Self {
			turns: Arc::new(Mutex::new(turns.into_iter().collect())),
			delay: Duration::ZERO,
			calls: Arc::new(Mutex::new(0)),
		}
	}

	/// Requests answered so far (by this provider and its clones).
	pub fn calls(&self) -> u64 {
		*self.calls.lock().unwrap_or_else(|p| p.into_inner())
	}

	/// Waits this long before each reply (to exercise "in progress" states).
	pub fn with_delay(mut self, delay: Duration) -> Self {
		self.delay = delay;
		self
	}
}

#[async_trait]
impl LlmProvider for ScriptedProvider {
	fn name(&self) -> &str {
		"scripted"
	}

	fn model(&self) -> &str {
		"scripted"
	}

	fn capabilities(&self) -> Capabilities {
		Capabilities {
			supports_tools: true,
			supports_parallel_tools: true,
			supports_reasoning: false,
			supports_prompt_cache: false,
		}
	}

	async fn complete(
		&self,
		_request: &ChatRequest<'_>,
		observer: &dyn AgentObserver,
	) -> Result<ChatResponse, ProviderError> {
		if !self.delay.is_zero() {
			tokio::time::sleep(self.delay).await;
		}
		let next = self
			.turns
			.lock()
			.unwrap_or_else(|p| p.into_inner())
			.pop_front();
		let n = {
			let mut calls = self.calls.lock().unwrap_or_else(|p| p.into_inner());
			*calls += 1;
			*calls
		};
		let (text, tool_calls, cost) = match next.unwrap_or_else(|| ScriptedTurn::text("Done.")) {
			ScriptedTurn::Fail(err) => return Err(err),
			ScriptedTurn::Reply {
				text,
				tool_calls,
				cost_usd,
			} => (text, tool_calls, cost_usd),
		};
		if let Some(text) = &text {
			for (i, word) in text.split_inclusive(' ').enumerate() {
				if i > 0 && !self.delay.is_zero() {
					tokio::time::sleep(self.delay / 20).await;
				}
				observer.on_event(&AgentEvent::TextDelta(word));
			}
		}
		let tool_calls: Vec<ToolCall> = tool_calls
			.into_iter()
			.enumerate()
			.map(|(i, (name, arguments))| ToolCall {
				id: format!("scripted_{n}_{i}"),
				name,
				arguments,
			})
			.collect();
		Ok(ChatResponse {
			finish: if tool_calls.is_empty() {
				FinishReason::Stop
			} else {
				FinishReason::ToolCalls
			},
			turn: AssistantTurn {
				text,
				tool_calls,
				reasoning: None,
			},
			usage: Usage {
				input_tokens: 1000,
				output_tokens: 50,
				cost_usd: Some(cost),
				..Usage::default()
			},
			model: None,
			id: Some(format!("scripted-{n}")),
			attempts: 1,
		})
	}

	fn preview(&self, _request: &ChatRequest<'_>) -> RequestPreview {
		RequestPreview {
			method: "POST",
			url: "scripted://".into(),
			headers: Vec::new(),
			body: serde_json::Value::Null,
		}
	}
}
