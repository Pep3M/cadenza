//! The neutral conversation model shared by the agent loop and every provider.
//!
//! The history is append-only: once a [`Message`] is pushed it is never edited, so
//! providers can render it to identical bytes on every request (which is what prompt
//! caches and reasoning-continuity checks key on).

use serde::{Deserialize, Serialize};

/// One turn of the conversation. The system prompt is not a message: it is sent
/// separately and never changes during a session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "snake_case")]
pub enum Message {
	User {
		content: String,
	},
	Assistant(AssistantTurn),
	/// The results of every tool call of the preceding assistant turn, in call order.
	ToolResults {
		results: Vec<ToolResult>,
	},
}

/// What the model produced in one response.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AssistantTurn {
	pub text: Option<String>,
	pub tool_calls: Vec<ToolCall>,
	/// Provider-specific reasoning to echo back unchanged on later requests.
	pub reasoning: Option<Reasoning>,
}

/// A tool call as the model wrote it. `arguments` is the raw JSON text, kept verbatim so
/// the turn can be replayed byte for byte; it is parsed (and may be invalid) at dispatch.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
	pub id: String,
	pub name: String,
	pub arguments: String,
}

/// Reasoning returned alongside an assistant turn.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Reasoning {
	/// Human-readable reasoning or its summary, when the provider returns one.
	pub text: Option<String>,
	/// Opaque structured payload (e.g. OpenRouter `reasoning_details`, with signatures)
	/// that some models need back to continue reasoning across tool calls.
	pub details: Option<serde_json::Value>,
}

/// The outcome of one tool call, fed back to the model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolResult {
	pub call_id: String,
	pub name: String,
	pub content: String,
	pub is_error: bool,
}

/// A tool as offered to the model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSpec {
	pub name: String,
	pub description: String,
	/// JSON Schema of the arguments object.
	pub parameters: serde_json::Value,
}
