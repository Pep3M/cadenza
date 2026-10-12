//! The provider abstraction: one chat-completion request in, one assistant turn out.

use async_trait::async_trait;

use crate::observer::AgentObserver;
use crate::types::{AssistantTurn, Message, ToolSpec};

/// What a backend/model combination can do; the agent adapts to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities {
	pub supports_tools: bool,
	pub supports_parallel_tools: bool,
	pub supports_reasoning: bool,
	/// Whether the provider places explicit prompt-cache breakpoints for this model.
	pub supports_prompt_cache: bool,
}

/// One request: the frozen system prompt, the append-only history and the tool set.
#[derive(Debug, Clone, Copy)]
pub struct ChatRequest<'a> {
	pub system: &'a str,
	pub messages: &'a [Message],
	pub tools: &'a [ToolSpec],
}

/// Why the model stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FinishReason {
	/// The turn contains tool calls to run.
	ToolCalls,
	/// The model finished its reply.
	Stop,
	/// The output limit cut the reply off.
	Length,
	/// A safety filter (or the model) declined.
	ContentFilter,
	Other(String),
}

/// Token accounting for one response.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub struct Usage {
	/// Prompt tokens, including cached ones.
	pub input_tokens: u64,
	pub output_tokens: u64,
	pub cache_read_tokens: u64,
	pub cache_write_tokens: u64,
	pub reasoning_tokens: u64,
	/// Cost reported by the provider, when it reports one.
	pub cost_usd: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct ChatResponse {
	pub turn: AssistantTurn,
	pub finish: FinishReason,
	pub usage: Usage,
	/// The model that actually served the request (routers may substitute).
	pub model: Option<String>,
	pub id: Option<String>,
	/// HTTP attempts it took (1 = no retry).
	pub attempts: u32,
}

/// What would go over the wire, for dry runs and the transcript. Secrets are redacted.
#[derive(Debug, Clone, serde::Serialize)]
pub struct RequestPreview {
	pub method: &'static str,
	pub url: String,
	pub headers: Vec<(String, String)>,
	pub body: serde_json::Value,
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum ProviderError {
	#[error("missing credentials: {0}")]
	Credentials(String),
	#[error("HTTP {status}: {message}")]
	Http {
		status: u16,
		message: String,
		retry_after: Option<std::time::Duration>,
	},
	#[error("API error {code}: {message}")]
	Api { code: i64, message: String },
	#[error("network error: {0}")]
	Network(String),
	#[error("could not decode the response: {0}")]
	Decode(String),
}

impl ProviderError {
	/// Rate limits, overloads, server errors and dropped connections are worth retrying;
	/// other client errors are not.
	pub fn is_retryable(&self) -> bool {
		let retryable_status =
			|s: i64| s == 408 || s == 409 || s == 429 || (500..=599).contains(&s);
		match self {
			Self::Http { status, .. } => retryable_status(i64::from(*status)),
			Self::Api { code, .. } => retryable_status(*code),
			Self::Network(_) => true,
			Self::Credentials(_) | Self::Decode(_) => false,
		}
	}

	pub fn retry_after(&self) -> Option<std::time::Duration> {
		match self {
			Self::Http { retry_after, .. } => *retry_after,
			_ => None,
		}
	}
}

#[async_trait]
pub trait LlmProvider: Send + Sync {
	/// Short backend name ("openrouter").
	fn name(&self) -> &str;
	/// The model requested.
	fn model(&self) -> &str;
	fn capabilities(&self) -> Capabilities;
	/// Sends one request (retrying transient failures, reporting retries to `observer`).
	async fn complete(
		&self,
		request: &ChatRequest<'_>,
		observer: &dyn AgentObserver,
	) -> Result<ChatResponse, ProviderError>;
	/// The exact request [`LlmProvider::complete`] would send, with secrets redacted.
	fn preview(&self, request: &ChatRequest<'_>) -> RequestPreview;
}
