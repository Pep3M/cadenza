//! Chat Completions backends: OpenRouter, and any other OpenAI-compatible endpoint
//! (OpenAI, a local Ollama or LM Studio...) through the same code: tools, streaming (SSE),
//! reasoning passthrough and retries are identical. OpenRouter adds its own fields —
//! reported costs (`usage.include`), provider routing (`provider.require_parameters`),
//! `reasoning.effort` and, for `anthropic/*` models, explicit prompt cache breakpoints
//! (`cache_control`); the generic flavor sends only standard fields (`reasoning_effort`,
//! `stream_options.include_usage`).

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use futures_util::StreamExt;
use serde_json::{Value, json};

use crate::http::{RetryPolicy, SseParser, parse_retry_after};
use crate::observer::{AgentEvent, AgentObserver, clip};
use crate::provider::{
	Capabilities, ChatRequest, ChatResponse, FinishReason, LlmProvider, ProviderError,
	RequestPreview, Usage,
};
use crate::types::{AssistantTurn, Message, Reasoning, ToolCall, ToolResult};

pub const DEFAULT_BASE_URL: &str = "https://openrouter.ai/api/v1";
/// Default endpoint of the OpenAI-compatible flavor.
pub const OPENAI_BASE_URL: &str = "https://api.openai.com/v1";

/// Which dialect of Chat Completions the endpoint speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiFlavor {
	OpenRouter,
	/// Plain OpenAI Chat Completions (OpenAI, Ollama, LM Studio, vLLM...): standard
	/// fields only, the API key is optional (local servers need none).
	OpenAiCompatible,
}
/// Overrides the endpoint (used by tests against a local mock server).
pub const BASE_URL_ENV: &str = "OPENROUTER_BASE_URL";

#[derive(Clone)]
pub struct OpenRouterConfig {
	pub flavor: ApiFlavor,
	pub base_url: String,
	pub api_key: Option<String>,
	pub model: String,
	/// `reasoning.effort` ("low" | "medium" | "high"); `None` leaves reasoning to the model.
	pub reasoning_effort: Option<String>,
	/// Output cap per response (reasoning included).
	pub max_output_tokens: u32,
	pub stream: bool,
	pub retry: RetryPolicy,
	/// Attribution headers OpenRouter shows in its app rankings.
	pub referer: Option<String>,
	pub title: Option<String>,
	pub request_timeout: Duration,
}

impl OpenRouterConfig {
	/// Defaults: streaming, medium reasoning, 16k output tokens, base URL from
	/// `OPENROUTER_BASE_URL` or the public endpoint.
	pub fn new(model: impl Into<String>, api_key: Option<String>) -> Self {
		Self {
			flavor: ApiFlavor::OpenRouter,
			base_url: std::env::var(BASE_URL_ENV)
				.ok()
				.filter(|u| !u.trim().is_empty())
				.unwrap_or_else(|| DEFAULT_BASE_URL.into()),
			api_key,
			model: model.into(),
			reasoning_effort: Some(crate::config::DEFAULT_REASONING_EFFORT.into()),
			max_output_tokens: 16_000,
			stream: true,
			retry: RetryPolicy::default(),
			referer: Some("https://github.com/Pep3M/cadenza".into()),
			title: Some("Cadenza".into()),
			request_timeout: Duration::from_secs(600),
		}
	}

	/// A generic OpenAI-compatible endpoint (`base_url` ends before `/chat/completions`,
	/// e.g. `http://localhost:11434/v1` for Ollama). No reasoning effort by default: not
	/// every model accepts one.
	pub fn openai_compatible(
		base_url: impl Into<String>,
		model: impl Into<String>,
		api_key: Option<String>,
	) -> Self {
		Self {
			flavor: ApiFlavor::OpenAiCompatible,
			base_url: base_url.into(),
			reasoning_effort: None,
			referer: None,
			title: None,
			..Self::new(model, api_key)
		}
	}
}

impl fmt::Debug for OpenRouterConfig {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("OpenRouterConfig")
			.field("flavor", &self.flavor)
			.field("base_url", &self.base_url)
			.field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
			.field("model", &self.model)
			.field("reasoning_effort", &self.reasoning_effort)
			.field("max_output_tokens", &self.max_output_tokens)
			.field("stream", &self.stream)
			.field("retry", &self.retry)
			.finish_non_exhaustive()
	}
}

pub struct OpenRouterProvider {
	config: OpenRouterConfig,
	client: reqwest::Client,
	/// For tool calls that arrive without an id (some upstream providers omit them).
	synthetic_ids: AtomicU64,
}

impl OpenRouterProvider {
	pub fn new(config: OpenRouterConfig) -> Result<Self, ProviderError> {
		let client = reqwest::Client::builder()
			.timeout(config.request_timeout)
			.connect_timeout(Duration::from_secs(30))
			.build()
			.map_err(|e| ProviderError::Network(e.to_string()))?;
		Ok(Self {
			config,
			client,
			synthetic_ids: AtomicU64::new(1),
		})
	}

	pub fn config(&self) -> &OpenRouterConfig {
		&self.config
	}

	fn url(&self) -> String {
		format!(
			"{}/chat/completions",
			self.config.base_url.trim_end_matches('/')
		)
	}

	fn caches(&self) -> bool {
		self.config.flavor == ApiFlavor::OpenRouter && self.config.model.starts_with("anthropic/")
	}

	fn headers(&self, redact: bool) -> Vec<(String, String)> {
		let auth = match (&self.config.api_key, redact) {
			(Some(_), true) => Some("Bearer <redacted>".to_owned()),
			(Some(key), false) => Some(format!("Bearer {key}")),
			(None, _) if self.config.flavor == ApiFlavor::OpenRouter => {
				Some("Bearer <missing>".to_owned())
			}
			(None, _) => None,
		};
		let mut headers = Vec::new();
		if let Some(auth) = auth {
			headers.push(("Authorization".to_owned(), auth));
		}
		headers.push(("Content-Type".to_owned(), "application/json".to_owned()));
		if let Some(referer) = &self.config.referer {
			headers.push(("HTTP-Referer".to_owned(), referer.clone()));
		}
		if let Some(title) = &self.config.title {
			headers.push(("X-Title".to_owned(), title.clone()));
		}
		headers
	}

	/// The JSON body for `request` (deterministic: same history, same bytes).
	pub fn build_body(&self, request: &ChatRequest<'_>) -> Value {
		let cache = self.caches();
		let tools: Vec<Value> = request
			.tools
			.iter()
			.map(|t| {
				json!({
					"type": "function",
					"function": {
						"name": t.name,
						"description": t.description,
						"parameters": t.parameters,
					}
				})
			})
			.collect();
		let mut body = json!({
			"model": self.config.model,
			"messages": render_messages(request.system, request.messages, cache),
			"max_tokens": self.config.max_output_tokens,
			"stream": self.config.stream,
		});
		match self.config.flavor {
			ApiFlavor::OpenRouter => {
				body["usage"] = json!({ "include": true });
				body["provider"] = json!({ "require_parameters": true });
				if let Some(effort) = &self.config.reasoning_effort {
					body["reasoning"] = json!({ "effort": effort });
				}
			}
			ApiFlavor::OpenAiCompatible => {
				if self.config.stream {
					body["stream_options"] = json!({ "include_usage": true });
				}
				if let Some(effort) = &self.config.reasoning_effort {
					body["reasoning_effort"] = json!(effort);
				}
			}
		}
		if !tools.is_empty() {
			body["tools"] = Value::Array(tools);
			body["tool_choice"] = json!("auto");
		}
		body
	}

	async fn send_once(
		&self,
		body: &[u8],
		observer: &dyn AgentObserver,
	) -> Result<ChatResponse, ProviderError> {
		let mut builder = self.client.post(self.url()).body(body.to_vec());
		for (name, value) in self.headers(false) {
			builder = builder.header(name, value);
		}
		let response = builder
			.send()
			.await
			.map_err(|e| ProviderError::Network(e.without_url().to_string()))?;
		let status = response.status();
		if !status.is_success() {
			let retry_after = parse_retry_after(response.headers());
			let text = response.text().await.unwrap_or_default();
			let message = serde_json::from_str::<Value>(&text)
				.ok()
				.and_then(|v| v["error"]["message"].as_str().map(str::to_owned))
				.unwrap_or_else(|| clip(&text, 500));
			return Err(ProviderError::Http {
				status: status.as_u16(),
				message,
				retry_after,
			});
		}
		let is_sse = response
			.headers()
			.get(reqwest::header::CONTENT_TYPE)
			.and_then(|v| v.to_str().ok())
			.is_some_and(|v| v.contains("text/event-stream"));
		if is_sse {
			let mut state = StreamState::default();
			let mut parser = SseParser::new();
			let mut stream = response.bytes_stream();
			while let Some(chunk) = stream.next().await {
				let chunk =
					chunk.map_err(|e| ProviderError::Network(e.without_url().to_string()))?;
				for data in parser.push(&chunk) {
					if state.apply_data(&data, observer)? {
						return state.finish(self);
					}
				}
			}
			for data in parser.finish() {
				state.apply_data(&data, observer)?;
			}
			state.finish(self)
		} else {
			let text = response
				.text()
				.await
				.map_err(|e| ProviderError::Network(e.without_url().to_string()))?;
			let value: Value = serde_json::from_str(text.trim())
				.map_err(|e| ProviderError::Decode(format!("{e}: {}", clip(&text, 300))))?;
			parse_completion(&value, self)
		}
	}

	fn synthetic_id(&self) -> String {
		format!(
			"call_cadenza_{}",
			self.synthetic_ids.fetch_add(1, Ordering::Relaxed)
		)
	}
}

#[async_trait]
impl LlmProvider for OpenRouterProvider {
	fn name(&self) -> &str {
		match self.config.flavor {
			ApiFlavor::OpenRouter => "openrouter",
			ApiFlavor::OpenAiCompatible => "openai-compatible",
		}
	}

	fn model(&self) -> &str {
		&self.config.model
	}

	fn capabilities(&self) -> Capabilities {
		Capabilities {
			supports_tools: true,
			supports_parallel_tools: true,
			supports_reasoning: self.config.reasoning_effort.is_some(),
			supports_prompt_cache: self.caches(),
		}
	}

	async fn complete(
		&self,
		request: &ChatRequest<'_>,
		observer: &dyn AgentObserver,
	) -> Result<ChatResponse, ProviderError> {
		if self.config.api_key.is_none() && self.config.flavor == ApiFlavor::OpenRouter {
			return Err(ProviderError::Credentials(format!(
				"no OpenRouter API key: set {} in the environment or in a .env file",
				crate::config::OPENROUTER_KEY_ENV
			)));
		}
		let body = serde_json::to_vec(&self.build_body(request))
			.map_err(|e| ProviderError::Decode(e.to_string()))?;
		let mut attempt = 0;
		loop {
			attempt += 1;
			match self.send_once(&body, observer).await {
				Ok(mut response) => {
					response.attempts = attempt;
					return Ok(response);
				}
				Err(err) if err.is_retryable() && attempt <= self.config.retry.max_retries => {
					let wait = self.config.retry.delay(attempt - 1, err.retry_after());
					observer.on_event(&AgentEvent::Retry {
						attempt,
						wait_ms: wait.as_millis() as u64,
						reason: &err.to_string(),
					});
					tokio::time::sleep(wait).await;
				}
				Err(err) => return Err(err),
			}
		}
	}

	fn preview(&self, request: &ChatRequest<'_>) -> RequestPreview {
		RequestPreview {
			method: "POST",
			url: self.url(),
			headers: self.headers(true),
			body: self.build_body(request),
		}
	}
}

/// Renders the neutral history as Chat Completions messages. With `cache`, user/tool/system
/// contents are always arrays of text parts (so a message renders identically whether or
/// not it is the latest) and the breakpoints sit on the system part and on the last
/// message's part.
pub fn render_messages(system: &str, messages: &[Message], cache: bool) -> Vec<Value> {
	let marker = || json!({ "type": "ephemeral" });
	let content = |text: &str, mark: bool| -> Value {
		if !cache {
			return Value::String(text.to_owned());
		}
		let mut part = json!({ "type": "text", "text": text });
		if mark {
			part["cache_control"] = marker();
		}
		Value::Array(vec![part])
	};
	let mut out = vec![json!({ "role": "system", "content": content(system, true) })];
	let last = messages.len().saturating_sub(1);
	for (i, message) in messages.iter().enumerate() {
		let is_last = i == last;
		match message {
			Message::User { content: text } => {
				out.push(json!({ "role": "user", "content": content(text, is_last) }));
			}
			Message::Assistant(turn) => out.push(render_assistant(turn)),
			Message::ToolResults { results } => {
				let n = results.len();
				for (j, result) in results.iter().enumerate() {
					out.push(json!({
						"role": "tool",
						"tool_call_id": result.call_id,
						"content": content(&tool_text(result), is_last && j + 1 == n),
					}));
				}
			}
		}
	}
	out
}

fn tool_text(result: &ToolResult) -> String {
	if result.is_error {
		format!("Error: {}", result.content)
	} else {
		result.content.clone()
	}
}

fn render_assistant(turn: &AssistantTurn) -> Value {
	let mut message = json!({ "role": "assistant" });
	let text = turn.text.clone().unwrap_or_default();
	message["content"] = if text.is_empty() && !turn.tool_calls.is_empty() {
		Value::Null
	} else {
		Value::String(text)
	};
	if !turn.tool_calls.is_empty() {
		message["tool_calls"] = turn
			.tool_calls
			.iter()
			.map(|c| {
				json!({
					"id": c.id,
					"type": "function",
					"function": { "name": c.name, "arguments": c.arguments },
				})
			})
			.collect();
	}
	if let Some(reasoning) = &turn.reasoning {
		if let Some(text) = &reasoning.text {
			message["reasoning"] = Value::String(text.clone());
		}
		if let Some(details) = &reasoning.details {
			message["reasoning_details"] = details.clone();
		}
	}
	message
}

fn api_error(error: &Value) -> ProviderError {
	let message = error["message"]
		.as_str()
		.unwrap_or("unknown error")
		.to_owned();
	let code = match &error["code"] {
		Value::Number(n) => n.as_i64().unwrap_or(500),
		Value::String(s) => s
			.parse()
			.unwrap_or(if s.contains("rate") { 429 } else { 500 }),
		_ => 500,
	};
	ProviderError::Api { code, message }
}

fn parse_usage(usage: &Value) -> Usage {
	let n = |v: &Value| v.as_u64().unwrap_or(0);
	Usage {
		input_tokens: n(&usage["prompt_tokens"]),
		output_tokens: n(&usage["completion_tokens"]),
		cache_read_tokens: n(&usage["prompt_tokens_details"]["cached_tokens"]),
		cache_write_tokens: n(&usage["prompt_tokens_details"]["cache_write_tokens"]),
		reasoning_tokens: n(&usage["completion_tokens_details"]["reasoning_tokens"]),
		cost_usd: usage["cost"].as_f64(),
	}
}

fn finish_reason(raw: Option<&str>, has_tool_calls: bool) -> FinishReason {
	match raw {
		Some("length" | "max_tokens") => FinishReason::Length,
		Some("content_filter" | "refusal") => FinishReason::ContentFilter,
		_ if has_tool_calls => FinishReason::ToolCalls,
		Some("stop" | "end_turn" | "tool_calls") | None => FinishReason::Stop,
		Some(other) => FinishReason::Other(other.to_owned()),
	}
}

fn arguments_text(value: &Value) -> String {
	match value {
		Value::String(s) => s.clone(),
		Value::Null => String::new(),
		other => other.to_string(),
	}
}

fn reasoning(text: String, details: Vec<Value>) -> Option<Reasoning> {
	let text = (!text.is_empty()).then_some(text);
	let details = (!details.is_empty()).then_some(Value::Array(details));
	(text.is_some() || details.is_some()).then_some(Reasoning { text, details })
}

/// Parses a non-streaming completion body.
fn parse_completion(
	value: &Value,
	provider: &OpenRouterProvider,
) -> Result<ChatResponse, ProviderError> {
	if !value["error"].is_null() {
		return Err(api_error(&value["error"]));
	}
	let choice = value["choices"].get(0).ok_or_else(|| {
		ProviderError::Decode(format!("no choices in {}", clip(&value.to_string(), 300)))
	})?;
	let message = &choice["message"];
	let tool_calls: Vec<ToolCall> = message["tool_calls"]
		.as_array()
		.into_iter()
		.flatten()
		.map(|c| ToolCall {
			id: c["id"]
				.as_str()
				.filter(|s| !s.is_empty())
				.map(str::to_owned)
				.unwrap_or_else(|| provider.synthetic_id()),
			name: c["function"]["name"]
				.as_str()
				.unwrap_or_default()
				.to_owned(),
			arguments: arguments_text(&c["function"]["arguments"]),
		})
		.collect();
	let details = message["reasoning_details"]
		.as_array()
		.cloned()
		.unwrap_or_default();
	let text = message["content"]
		.as_str()
		.filter(|s| !s.is_empty())
		.map(str::to_owned);
	Ok(ChatResponse {
		finish: finish_reason(choice["finish_reason"].as_str(), !tool_calls.is_empty()),
		turn: AssistantTurn {
			text,
			reasoning: reasoning(
				message["reasoning"].as_str().unwrap_or_default().to_owned(),
				details,
			),
			tool_calls,
		},
		usage: parse_usage(&value["usage"]),
		model: value["model"].as_str().map(str::to_owned),
		id: value["id"].as_str().map(str::to_owned),
		attempts: 1,
	})
}

#[derive(Debug, Default)]
struct PartialCall {
	index: Option<u64>,
	id: String,
	name: String,
	arguments: String,
}

/// Accumulates streamed chunks into one response.
#[derive(Debug, Default)]
struct StreamState {
	id: Option<String>,
	model: Option<String>,
	text: String,
	reasoning: String,
	details: Vec<Value>,
	calls: Vec<PartialCall>,
	finish: Option<String>,
	usage: Option<Value>,
	done: bool,
}

impl StreamState {
	/// Applies one SSE `data` payload; returns true at `[DONE]`. Text and reasoning
	/// fragments are forwarded to `observer` as they arrive.
	fn apply_data(
		&mut self,
		data: &str,
		observer: &dyn AgentObserver,
	) -> Result<bool, ProviderError> {
		let data = data.trim();
		if data.is_empty() {
			return Ok(false);
		}
		if data == "[DONE]" {
			self.done = true;
			return Ok(true);
		}
		let chunk: Value = serde_json::from_str(data).map_err(|e| {
			ProviderError::Decode(format!("bad stream chunk ({e}): {}", clip(data, 200)))
		})?;
		self.apply(&chunk, observer)?;
		Ok(false)
	}

	fn apply(&mut self, chunk: &Value, observer: &dyn AgentObserver) -> Result<(), ProviderError> {
		if !chunk["error"].is_null() {
			return Err(api_error(&chunk["error"]));
		}
		if let Some(id) = chunk["id"].as_str() {
			self.id.get_or_insert_with(|| id.to_owned());
		}
		if let Some(model) = chunk["model"].as_str() {
			self.model.get_or_insert_with(|| model.to_owned());
		}
		if chunk["usage"].is_object() {
			self.usage = Some(chunk["usage"].clone());
		}
		for choice in chunk["choices"].as_array().into_iter().flatten() {
			let delta = &choice["delta"];
			if let Some(text) = delta["content"].as_str() {
				self.text.push_str(text);
				if !text.is_empty() {
					observer.on_event(&AgentEvent::TextDelta(text));
				}
			}
			if let Some(text) = delta["reasoning"].as_str() {
				self.reasoning.push_str(text);
				if !text.is_empty() {
					observer.on_event(&AgentEvent::ThinkingDelta(text));
				}
			}
			for item in delta["reasoning_details"].as_array().into_iter().flatten() {
				merge_reasoning_detail(&mut self.details, item);
			}
			for call in delta["tool_calls"].as_array().into_iter().flatten() {
				self.merge_call(call);
			}
			if let Some(reason) = choice["finish_reason"].as_str() {
				self.finish = Some(reason.to_owned());
			}
		}
		Ok(())
	}

	fn merge_call(&mut self, call: &Value) {
		let index = call["index"].as_u64();
		let id = call["id"].as_str().filter(|s| !s.is_empty());
		let existing = match index {
			Some(i) => self.calls.iter().position(|c| c.index == Some(i)),
			// no index: a new id starts a new call, otherwise it continues the last one
			None => match id {
				Some(id) if self.calls.last().is_none_or(|c| c.id != id) => None,
				_ => self.calls.len().checked_sub(1),
			},
		};
		let slot = match existing {
			Some(pos) => &mut self.calls[pos],
			None => {
				self.calls.push(PartialCall {
					index,
					..PartialCall::default()
				});
				self.calls.last_mut().expect("just pushed")
			}
		};
		if let Some(id) = id {
			slot.id = id.to_owned();
		}
		if let Some(name) = call["function"]["name"].as_str() {
			slot.name.push_str(name);
		}
		match &call["function"]["arguments"] {
			Value::String(fragment) => slot.arguments.push_str(fragment),
			Value::Null => {}
			other => slot.arguments.push_str(&other.to_string()),
		}
	}

	fn finish(self, provider: &OpenRouterProvider) -> Result<ChatResponse, ProviderError> {
		if self.finish.is_none() && !self.done {
			return Err(ProviderError::Network(
				"the stream ended before the response was complete".into(),
			));
		}
		if self.finish.as_deref() == Some("error") {
			return Err(ProviderError::Api {
				code: 502,
				message: "the upstream provider failed mid-response".into(),
			});
		}
		let tool_calls: Vec<ToolCall> = self
			.calls
			.into_iter()
			.filter(|c| !c.name.is_empty() || !c.arguments.is_empty())
			.map(|c| ToolCall {
				id: if c.id.is_empty() {
					provider.synthetic_id()
				} else {
					c.id
				},
				name: c.name,
				arguments: c.arguments,
			})
			.collect();
		Ok(ChatResponse {
			finish: finish_reason(self.finish.as_deref(), !tool_calls.is_empty()),
			turn: AssistantTurn {
				text: (!self.text.is_empty()).then_some(self.text),
				reasoning: reasoning(self.reasoning, self.details),
				tool_calls,
			},
			usage: self.usage.as_ref().map(parse_usage).unwrap_or_default(),
			model: self.model,
			id: self.id,
			attempts: 1,
		})
	}
}

/// Streamed `reasoning_details` arrive as fragments of a few entries; fragments of the
/// same entry share `index` (and `type`): their text-like fields concatenate, other fields
/// are taken as they arrive.
fn merge_reasoning_detail(details: &mut Vec<Value>, item: &Value) {
	let same = |d: &Value| {
		!item["index"].is_null() && d["index"] == item["index"] && d["type"] == item["type"]
	};
	match details.iter_mut().find(|d| same(d)) {
		Some(existing) => {
			let (Some(target), Some(source)) = (existing.as_object_mut(), item.as_object()) else {
				return;
			};
			for (key, value) in source {
				match (key.as_str(), value, target.get_mut(key)) {
					(
						"text" | "summary" | "data",
						Value::String(add),
						Some(Value::String(have)),
					) => have.push_str(add),
					(_, Value::Null, _) => {}
					_ => {
						target.insert(key.clone(), value.clone());
					}
				}
			}
		}
		None => details.push(item.clone()),
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::observer::NullObserver;
	use crate::types::ToolSpec;

	fn provider(model: &str) -> OpenRouterProvider {
		let mut config = OpenRouterConfig::new(model, Some("sk-test".into()));
		config.base_url = "http://localhost:1/api/v1/".into();
		OpenRouterProvider::new(config).unwrap()
	}

	fn history() -> Vec<Message> {
		vec![
			Message::User {
				content: "brief".into(),
			},
			Message::Assistant(AssistantTurn {
				text: None,
				tool_calls: vec![
					ToolCall {
						id: "a".into(),
						name: "new_project".into(),
						arguments: "{\"name\":\"x\"}".into(),
					},
					ToolCall {
						id: "b".into(),
						name: "get_project".into(),
						arguments: String::new(),
					},
				],
				reasoning: Some(Reasoning {
					text: Some("plan".into()),
					details: Some(
						json!([{"type": "reasoning.text", "text": "plan", "signature": "sig"}]),
					),
				}),
			}),
			Message::ToolResults {
				results: vec![
					ToolResult {
						call_id: "a".into(),
						name: "new_project".into(),
						content: "{}".into(),
						is_error: false,
					},
					ToolResult {
						call_id: "b".into(),
						name: "get_project".into(),
						content: "boom".into(),
						is_error: true,
					},
				],
			},
		]
	}

	#[test]
	fn anthropic_models_get_cache_breakpoints_on_system_and_last_message() {
		let messages = render_messages("sys", &history(), true);
		assert_eq!(messages.len(), 5);
		assert_eq!(
			messages[0]["content"][0]["cache_control"]["type"],
			"ephemeral"
		);
		assert!(messages[1]["content"][0]["cache_control"].is_null());
		assert!(messages[3]["content"][0]["cache_control"].is_null());
		assert_eq!(
			messages[4]["content"][0]["cache_control"]["type"],
			"ephemeral"
		);
		assert_eq!(messages[4]["content"][0]["text"], "Error: boom");
		assert_eq!(messages[4]["tool_call_id"], "b");
		let text = serde_json::to_string(&messages).unwrap();
		assert_eq!(text.matches("cache_control").count(), 2);
	}

	#[test]
	fn other_models_get_plain_string_contents() {
		let messages = render_messages("sys", &history(), false);
		assert_eq!(messages[0]["content"], "sys");
		assert_eq!(messages[1]["content"], "brief");
		assert!(
			!serde_json::to_string(&messages)
				.unwrap()
				.contains("cache_control")
		);
		let assistant = &messages[2];
		assert!(assistant["content"].is_null());
		assert_eq!(assistant["tool_calls"][1]["function"]["arguments"], "");
		assert_eq!(assistant["reasoning_details"][0]["signature"], "sig");
		assert_eq!(assistant["reasoning"], "plan");
	}

	#[test]
	fn request_body_has_tools_reasoning_usage_and_routing() {
		let p = provider("anthropic/claude-haiku-5.5");
		let tools = [ToolSpec {
			name: "get_project".into(),
			description: "d".into(),
			parameters: json!({"type": "object", "properties": {}}),
		}];
		let messages = history();
		let request = ChatRequest {
			system: "sys",
			messages: &messages,
			tools: &tools,
		};
		let body = p.build_body(&request);
		assert_eq!(body["model"], "anthropic/claude-haiku-5.5");
		assert_eq!(body["tools"][0]["type"], "function");
		assert_eq!(body["tools"][0]["function"]["parameters"]["type"], "object");
		assert_eq!(body["tool_choice"], "auto");
		assert_eq!(body["usage"]["include"], true);
		assert_eq!(body["provider"]["require_parameters"], true);
		assert_eq!(body["reasoning"]["effort"], "medium");
		assert_eq!(body["stream"], true);
		let preview = p.preview(&request);
		assert_eq!(preview.url, "http://localhost:1/api/v1/chat/completions");
		assert!(preview.headers.iter().all(|(_, v)| !v.contains("sk-test")));
		assert!(!format!("{:?}", p.config()).contains("sk-test"));
	}

	#[test]
	fn openai_compatible_bodies_use_standard_fields_only() {
		let mut config = OpenRouterConfig::openai_compatible(
			"http://localhost:11434/v1/",
			"anthropic/claude-like",
			None,
		);
		let p = OpenRouterProvider::new(config.clone()).unwrap();
		assert_eq!(p.name(), "openai-compatible");
		let messages = history();
		let request = ChatRequest {
			system: "sys",
			messages: &messages,
			tools: &[],
		};
		let body = p.build_body(&request);
		for field in [
			"usage",
			"provider",
			"reasoning",
			"reasoning_effort",
			"tools",
		] {
			assert!(body.get(field).is_none(), "{field} in {body}");
		}
		assert_eq!(body["stream_options"]["include_usage"], true);
		assert!(
			!body.to_string().contains("cache_control"),
			"no cache markers"
		);
		assert!(!p.capabilities().supports_prompt_cache);
		let preview = p.preview(&request);
		assert_eq!(preview.url, "http://localhost:11434/v1/chat/completions");
		assert!(preview.headers.iter().all(|(k, _)| k != "Authorization"));

		config.reasoning_effort = Some("low".into());
		config.api_key = Some("sk-secret".into());
		let p = OpenRouterProvider::new(config).unwrap();
		let body = p.build_body(&request);
		assert_eq!(body["reasoning_effort"], "low");
		let preview = p.preview(&request);
		assert!(
			preview
				.headers
				.iter()
				.any(|(k, v)| k == "Authorization" && v.contains("redacted"))
		);
	}

	#[test]
	fn streamed_text_is_forwarded_as_deltas() {
		#[derive(Default)]
		struct Deltas(std::sync::Mutex<Vec<String>>);
		impl AgentObserver for Deltas {
			fn on_event(&self, event: &AgentEvent<'_>) {
				match event {
					AgentEvent::TextDelta(t) => self.0.lock().unwrap().push(format!("t:{t}")),
					AgentEvent::ThinkingDelta(t) => self.0.lock().unwrap().push(format!("r:{t}")),
					_ => {}
				}
			}
		}
		let deltas = Deltas::default();
		let mut state = StreamState::default();
		for chunk in [
			json!({"choices": [{"delta": {"reasoning": "hm"}}]}),
			json!({"choices": [{"delta": {"content": "Hel"}}]}),
			json!({"choices": [{"delta": {"content": ""}}]}),
			json!({"choices": [{"delta": {"content": "lo"}, "finish_reason": "stop"}]}),
		] {
			state.apply_data(&chunk.to_string(), &deltas).unwrap();
		}
		assert_eq!(*deltas.0.lock().unwrap(), vec!["r:hm", "t:Hel", "t:lo"]);
	}

	#[test]
	fn streamed_tool_calls_and_reasoning_accumulate() {
		let p = provider("x/y");
		let mut state = StreamState::default();
		let chunks = [
			json!({"id": "gen-1", "model": "x/y", "choices": [{"delta": {"reasoning": "Let me ", "reasoning_details": [{"type": "reasoning.text", "index": 0, "text": "Let me "}]}}]}),
			json!({"choices": [{"delta": {"reasoning": "think", "reasoning_details": [{"type": "reasoning.text", "index": 0, "text": "think", "signature": "s1"}]}}]}),
			json!({"choices": [{"delta": {"content": "Plan."}}]}),
			json!({"choices": [{"delta": {"tool_calls": [{"index": 0, "id": "c1", "type": "function", "function": {"name": "new_project", "arguments": "{\"na"}}]}}]}),
			json!({"choices": [{"delta": {"tool_calls": [{"index": 1, "id": "c2", "function": {"name": "get_project", "arguments": ""}}, {"index": 0, "function": {"arguments": "me\":\"x\"}"}}]}}]}),
			json!({"choices": [{"delta": {}, "finish_reason": "tool_calls"}]}),
			json!({"choices": [], "usage": {"prompt_tokens": 100, "completion_tokens": 20, "cost": 0.0012, "prompt_tokens_details": {"cached_tokens": 80}}}),
		];
		for chunk in &chunks {
			state.apply_data(&chunk.to_string(), &NullObserver).unwrap();
		}
		assert!(state.apply_data("[DONE]", &NullObserver).unwrap());
		let response = state.finish(&p).unwrap();
		assert_eq!(response.finish, FinishReason::ToolCalls);
		assert_eq!(response.turn.text.as_deref(), Some("Plan."));
		let calls = &response.turn.tool_calls;
		assert_eq!(calls.len(), 2);
		assert_eq!(
			(calls[0].id.as_str(), calls[0].arguments.as_str()),
			("c1", "{\"name\":\"x\"}")
		);
		assert_eq!(calls[1].name, "get_project");
		let reasoning = response.turn.reasoning.unwrap();
		assert_eq!(reasoning.text.as_deref(), Some("Let me think"));
		assert_eq!(
			reasoning.details.unwrap(),
			json!([{"type": "reasoning.text", "index": 0, "text": "Let me think", "signature": "s1"}])
		);
		assert_eq!(response.usage.cache_read_tokens, 80);
		assert_eq!(response.usage.cost_usd, Some(0.0012));
		assert_eq!(response.id.as_deref(), Some("gen-1"));
	}

	#[test]
	fn stream_errors_and_truncation_are_reported() {
		let p = provider("x/y");
		let mut state = StreamState::default();
		let err = state
			.apply_data(
				&json!({"error": {"code": 502, "message": "upstream down"}}).to_string(),
				&NullObserver,
			)
			.unwrap_err();
		assert!(err.is_retryable(), "{err}");
		let truncated = StreamState::default().finish(&p).unwrap_err();
		assert!(truncated.is_retryable());
		let mut filtered = StreamState::default();
		filtered
			.apply_data(
				&json!({"choices": [{"delta": {"content": "no"}, "finish_reason": "content_filter"}]})
					.to_string(),
				&NullObserver,
			)
			.unwrap();
		assert_eq!(
			filtered.finish(&p).unwrap().finish,
			FinishReason::ContentFilter
		);
	}

	#[test]
	fn non_streaming_bodies_parse_including_object_arguments_and_errors() {
		let p = provider("x/y");
		let body = json!({
			"id": "gen-2", "model": "x/y",
			"choices": [{"finish_reason": "stop", "message": {"role": "assistant", "content": "",
				"tool_calls": [{"id": "", "function": {"name": "get_project", "arguments": {"a": 1}}}]}}],
			"usage": {"prompt_tokens": 5, "completion_tokens": 1}
		});
		let response = parse_completion(&body, &p).unwrap();
		assert_eq!(response.finish, FinishReason::ToolCalls);
		assert!(response.turn.text.is_none());
		assert!(response.turn.tool_calls[0].id.starts_with("call_cadenza_"));
		assert_eq!(response.turn.tool_calls[0].arguments, "{\"a\":1}");
		assert_eq!(response.usage.cost_usd, None);

		let err =
			parse_completion(&json!({"error": {"code": 400, "message": "bad"}}), &p).unwrap_err();
		assert!(!err.is_retryable());
		let err = parse_completion(
			&json!({"error": {"code": "rate_limited", "message": "slow"}}),
			&p,
		)
		.unwrap_err();
		assert!(err.is_retryable());
	}
}
