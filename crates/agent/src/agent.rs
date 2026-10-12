//! The composition loop: ask the model, run its tool calls, feed the results back, until
//! the song is saved and rendered — or the turn/cost budget runs out, in which case the
//! harness saves and renders whatever exists.
//!
//! The history is append-only. The one deliberate exception is the context checkpoint:
//! when a request's input grows past `compact_at_input_tokens`, the model writes a
//! hand-off note and the conversation restarts from the brief + that note + the current
//! project summary ("simple compaction"). It is provider-agnostic, happens only between
//! tool rounds (never with a call awaiting its result), and is logged in the transcript.
//! Server-side compaction would avoid the extra request but exists only on Anthropic's
//! native API, not through OpenRouter's Chat Completions.

use std::path::PathBuf;

use cadenza_mcp::CadenzaServer;
use serde_json::json;

use crate::budget::Budget;
use crate::config::{AgentConfig, ProviderConfig};
use crate::error::AgentError;
use crate::observer::{AgentEvent, AgentObserver};
use crate::openrouter::OpenRouterProvider;
use crate::prompt::{self, SYSTEM_PROMPT};
use crate::provider::{
	ChatRequest, ChatResponse, FinishReason, LlmProvider, ProviderError, RequestPreview,
};
use crate::tools::{Effect, PathPolicy, ToolBridge};
use crate::transcript::{TeeObserver, Transcript};
use crate::types::{Message, ToolResult, ToolSpec};

/// Why the session ended.
#[derive(Debug, Clone, PartialEq)]
pub enum StopStatus {
	/// The model finished (possibly after the harness completed save/render for it).
	Completed,
	BudgetExhausted,
	MaxTurns,
	/// The model or a safety filter declined to continue.
	Refused,
	/// The provider kept failing or the model could not make progress.
	Failed(String),
}

#[derive(Debug, Clone)]
pub struct AgentOutcome {
	pub status: StopStatus,
	/// The model's closing summary, if it wrote one.
	pub final_message: Option<String>,
	pub project_path: Option<PathBuf>,
	pub wav_path: Option<PathBuf>,
	/// "-14.1 LUFS, true peak -1.2 dBTP, 182 s" of the final render.
	pub loudness: Option<String>,
	/// True when the harness had to save and/or render because the model did not.
	pub finalized_by_harness: bool,
	pub turns: u32,
	pub budget: Budget,
	pub transcript_path: PathBuf,
}

#[derive(Debug, Default)]
struct Deliverables {
	saved: Option<PathBuf>,
	rendered: Option<(PathBuf, String)>,
}

impl Deliverables {
	fn apply(&mut self, effect: &Effect, policy: &PathPolicy) {
		match effect {
			Effect::Saved(path) if path == policy.default_project() => {
				self.saved = Some(path.clone())
			}
			Effect::Rendered { path, summary } if path == policy.default_wav() => {
				self.rendered = Some((path.clone(), summary.clone()))
			}
			Effect::Mutated => {
				self.saved = None;
				self.rendered = None;
			}
			_ => {}
		}
	}

	fn complete(&self) -> bool {
		self.saved.is_some() && self.rendered.is_some()
	}
}

pub struct Agent {
	config: AgentConfig,
	provider: Box<dyn LlmProvider>,
	tools: ToolBridge,
	history: Vec<Message>,
	budget: Budget,
}

impl Agent {
	/// An agent with the provider described by `config` and a fresh Cadenza session.
	pub fn new(config: AgentConfig) -> Result<Self, AgentError> {
		let provider: Box<dyn LlmProvider> = match &config.provider {
			ProviderConfig::OpenRouter(c) => Box::new(OpenRouterProvider::new(c.clone())?),
		};
		Self::with_parts(config, provider, CadenzaServer::new())
	}

	/// An agent over a given provider and tool server (embedding, tests).
	pub fn with_parts(
		config: AgentConfig,
		provider: Box<dyn LlmProvider>,
		server: CadenzaServer,
	) -> Result<Self, AgentError> {
		if config.slug.is_empty() || config.slug.contains(['/', '\\']) {
			return Err(AgentError::Config(format!(
				"invalid file name slug '{}'",
				config.slug
			)));
		}
		std::fs::create_dir_all(&config.out_dir).map_err(|e| AgentError::io(&config.out_dir, e))?;
		let policy = PathPolicy::new(
			&config.out_dir,
			&config.project_file_name(),
			&config.wav_file_name(),
		)
		.map_err(|e| AgentError::io(&config.out_dir, e))?;
		let tools = ToolBridge::new(
			server,
			policy,
			config.allow_plugins,
			config.max_tool_result_chars,
		);
		Ok(Self {
			budget: Budget::new(config.max_cost_usd),
			config,
			provider,
			tools,
			history: Vec::new(),
		})
	}

	pub fn config(&self) -> &AgentConfig {
		&self.config
	}

	pub fn provider(&self) -> &dyn LlmProvider {
		self.provider.as_ref()
	}

	pub fn system_prompt(&self) -> &'static str {
		SYSTEM_PROMPT
	}

	pub fn tools(&self) -> &[ToolSpec] {
		self.tools.specs()
	}

	pub fn history(&self) -> &[Message] {
		&self.history
	}

	/// The first request a session for `brief` would send (secrets redacted).
	pub fn preview_first_request(&self, brief: &str) -> RequestPreview {
		let messages = [Message::User {
			content: prompt::initial_user_message(brief, &self.config),
		}];
		self.provider.preview(&ChatRequest {
			system: SYSTEM_PROMPT,
			messages: &messages,
			tools: self.tools.specs(),
		})
	}

	/// Runs a whole session for `brief`. Returns `Err` only if nothing could be done (bad
	/// credentials, unwritable folder, first request rejected); later failures end the
	/// session with a [`StopStatus`] after saving and rendering what exists.
	pub async fn run(
		&mut self,
		brief: &str,
		observer: &dyn AgentObserver,
	) -> Result<AgentOutcome, AgentError> {
		let transcript = Transcript::create(&self.config.transcript_path())?;
		let tee = TeeObserver {
			user: observer,
			transcript: &transcript,
		};
		transcript.record(
			"session_start",
			json!({
				"provider": self.provider.name(),
				"model": self.provider.model(),
				"capabilities": format!("{:?}", self.provider.capabilities()),
				"brief": brief,
				"max_turns": self.config.max_turns,
				"max_cost_usd": self.config.max_cost_usd,
				"target_lufs": self.config.target_lufs,
				"allow_plugins": self.config.allow_plugins,
				"tool_count": self.tools.specs().len(),
				"system_prompt_chars": SYSTEM_PROMPT.len(),
			}),
		);

		let initial = prompt::initial_user_message(brief, &self.config);
		self.history = vec![Message::User {
			content: initial.clone(),
		}];
		let mut deliverables = Deliverables::default();
		let mut turn = 0u32;
		let mut nudges = 0u32;
		let mut length_streak = 0u32;
		let mut wrap_up_sent = false;
		let mut checkpoint_due = false;
		let mut final_message = None;

		let status = loop {
			if turn >= self.config.max_turns {
				break StopStatus::MaxTurns;
			}
			if self.budget.exhausted() {
				break StopStatus::BudgetExhausted;
			}
			if checkpoint_due {
				checkpoint_due = false;
				turn += 1;
				if let Err(err) = self.checkpoint(&initial, turn, &tee, &transcript).await {
					break StopStatus::Failed(err.to_string());
				}
				continue;
			}
			let turns_left = self.config.max_turns - turn;
			if !wrap_up_sent && (self.budget.fraction_used() >= 0.8 || turns_left <= 8) {
				wrap_up_sent = true;
				let note = prompt::wrap_up_note(self.budget.fraction_used(), turns_left);
				tee.on_event(&AgentEvent::Notice(&note));
				self.history.push(Message::User { content: note });
			}

			turn += 1;
			tee.on_event(&AgentEvent::TurnStarted { turn });
			let response = match self.request(turn, &tee, &transcript).await {
				Ok(response) => response,
				Err(err) if turn == 1 => return Err(err.into()),
				Err(err) => break StopStatus::Failed(err.to_string()),
			};
			if let Some(text) = response
				.turn
				.reasoning
				.as_ref()
				.and_then(|r| r.text.as_deref())
			{
				tee.on_event(&AgentEvent::Thinking(text));
			}
			if let Some(text) = &response.turn.text {
				tee.on_event(&AgentEvent::Text(text));
			}

			match response.finish {
				FinishReason::ContentFilter => {
					tee.on_event(&AgentEvent::Notice(
						"the model or a content filter declined to continue",
					));
					break StopStatus::Refused;
				}
				FinishReason::Length => {
					length_streak += 1;
					let calls = response.turn.tool_calls.clone();
					self.history.push(Message::Assistant(response.turn));
					if calls.is_empty() {
						self.history.push(Message::User {
							content: prompt::LENGTH_CONTINUE.into(),
						});
					} else {
						let results = calls
							.iter()
							.map(|c| ToolResult {
								call_id: c.id.clone(),
								name: c.name.clone(),
								content: prompt::LENGTH_TOOL_ERROR.into(),
								is_error: true,
							})
							.collect();
						self.history.push(Message::ToolResults { results });
					}
					if length_streak >= 3 {
						break StopStatus::Failed(
							"the model hit the output limit three times in a row".into(),
						);
					}
					continue;
				}
				_ => length_streak = 0,
			}

			let input_tokens = response.usage.input_tokens;
			let calls = response.turn.tool_calls.clone();
			let text = response.turn.text.clone();
			self.history.push(Message::Assistant(response.turn));

			if !calls.is_empty() {
				let mut results = Vec::with_capacity(calls.len());
				for call in &calls {
					tee.on_event(&AgentEvent::ToolCall {
						name: &call.name,
						args: &call.arguments,
					});
					let outcome = self.tools.call(call).await;
					deliverables.apply(&outcome.effect, self.tools.policy());
					tee.on_event(&AgentEvent::ToolResult {
						name: &call.name,
						ok: !outcome.is_error,
						summary: &outcome.summary,
					});
					results.push(ToolResult {
						call_id: call.id.clone(),
						name: call.name.clone(),
						content: outcome.content,
						is_error: outcome.is_error,
					});
				}
				self.history.push(Message::ToolResults { results });
				checkpoint_due = self
					.config
					.compact_at_input_tokens
					.is_some_and(|limit| input_tokens >= limit);
				continue;
			}

			// No tool calls: the model considers the job done.
			if deliverables.complete() || nudges >= 2 {
				final_message = text;
				break StopStatus::Completed;
			}
			nudges += 1;
			let nudge = prompt::deliverables_nudge(
				&self.config.project_file_name(),
				&self.config.wav_file_name(),
			);
			tee.on_event(&AgentEvent::Notice(&nudge));
			self.history.push(Message::User { content: nudge });
		};

		let finalized_by_harness = !deliverables.complete();
		if finalized_by_harness {
			self.finalize(&mut deliverables, &tee).await;
		}
		if let Some(text) = &final_message {
			tee.on_event(&AgentEvent::Final(text));
		}
		let outcome = AgentOutcome {
			status,
			final_message,
			project_path: deliverables.saved.clone(),
			loudness: deliverables.rendered.as_ref().map(|(_, s)| s.clone()),
			wav_path: deliverables.rendered.map(|(p, _)| p),
			finalized_by_harness,
			turns: turn,
			budget: self.budget.clone(),
			transcript_path: transcript.path().to_path_buf(),
		};
		transcript.record(
			"outcome",
			json!({
				"status": format!("{:?}", outcome.status),
				"turns": outcome.turns,
				"project": outcome.project_path,
				"wav": outcome.wav_path,
				"loudness": outcome.loudness,
				"finalized_by_harness": outcome.finalized_by_harness,
				"budget": outcome.budget,
			}),
		);
		Ok(outcome)
	}

	/// Sends the current history and accounts for the response.
	async fn request(
		&mut self,
		turn: u32,
		observer: &dyn AgentObserver,
		transcript: &Transcript,
	) -> Result<ChatResponse, ProviderError> {
		let request = ChatRequest {
			system: SYSTEM_PROMPT,
			messages: &self.history,
			tools: self.tools.specs(),
		};
		let preview = self.provider.preview(&request);
		transcript.record(
			"request",
			json!({
				"turn": turn,
				"url": preview.url,
				"headers": preview.headers,
				"model": self.provider.model(),
				"messages": self.history.len(),
				"body_bytes": preview.body.to_string().len(),
			}),
		);
		let response = match self.provider.complete(&request, observer).await {
			Ok(response) => response,
			Err(err) => {
				transcript.record(
					"request_failed",
					json!({ "turn": turn, "error": err.to_string() }),
				);
				return Err(err);
			}
		};
		let model = response
			.model
			.clone()
			.unwrap_or_else(|| self.provider.model().to_owned());
		let (cost, estimated) = self.budget.record(&model, &response.usage);
		observer.on_event(&AgentEvent::Usage {
			turn,
			usage: &response.usage,
			turn_cost_usd: cost,
			total_cost_usd: self.budget.spent_usd,
			estimated,
		});
		transcript.record(
			"response",
			json!({
				"turn": turn,
				"id": response.id,
				"model": model,
				"finish": format!("{:?}", response.finish),
				"attempts": response.attempts,
				"usage": response.usage,
				"cost_usd": cost,
				"cost_estimated": estimated,
				"total_cost_usd": self.budget.spent_usd,
				"text_chars": response.turn.text.as_ref().map_or(0, String::len),
				"tool_calls": response.turn.tool_calls.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
			}),
		);
		Ok(response)
	}

	/// Condenses the conversation into a hand-off note (see the module docs).
	async fn checkpoint(
		&mut self,
		initial: &str,
		turn: u32,
		observer: &dyn AgentObserver,
		transcript: &Transcript,
	) -> Result<(), ProviderError> {
		observer.on_event(&AgentEvent::Notice(
			"context checkpoint: condensing the conversation into a hand-off note",
		));
		self.history.push(Message::User {
			content: prompt::HANDOFF_REQUEST.into(),
		});
		let response = self.request(turn, observer, transcript).await?;
		let note = response.turn.text.unwrap_or_default();
		let state = self.tools.dispatch("get_project", json!({})).await.content;
		let note = if note.trim().is_empty() {
			"(no hand-off note was written; rebuild your picture from the project state)".to_owned()
		} else {
			note
		};
		transcript.record(
			"checkpoint",
			json!({ "turn": turn, "replaced_messages": self.history.len(), "note_chars": note.len() }),
		);
		self.history = vec![Message::User {
			content: prompt::resumed_user_message(initial, &note, &state),
		}];
		Ok(())
	}

	/// Saves and renders to the session's files when the model did not.
	async fn finalize(&self, deliverables: &mut Deliverables, observer: &dyn AgentObserver) {
		observer.on_event(&AgentEvent::Notice(
			"saving and rendering the project (the model did not finish that step)",
		));
		let policy = self.tools.policy();
		if deliverables.saved.is_none() {
			let path = policy.default_project().display().to_string();
			let outcome = self
				.tools
				.dispatch("save_project", json!({ "path": path }))
				.await;
			report(observer, "save_project", &outcome);
			deliverables.apply(&outcome.effect, policy);
		}
		if deliverables.rendered.is_none() {
			let path = policy.default_wav().display().to_string();
			let outcome = self.tools.dispatch("render", json!({ "path": path })).await;
			report(observer, "render", &outcome);
			deliverables.apply(&outcome.effect, policy);
		}
	}
}

fn report(observer: &dyn AgentObserver, name: &str, outcome: &crate::tools::ToolOutcome) {
	observer.on_event(&AgentEvent::ToolResult {
		name,
		ok: !outcome.is_error,
		summary: &outcome.summary,
	});
}
