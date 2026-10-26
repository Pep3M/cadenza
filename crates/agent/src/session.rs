//! The in-app producer: a conversation about the project the user has open.
//!
//! Unlike [`crate::agent::Agent`] (a brief in, a finished song out), a [`SessionAgent`]
//! works on a [`SharedSession`] that the desktop app edits too, one request at a time,
//! keeping the conversation across requests:
//!
//! - Every edit of one request lands in a single undo step "AI: <request>" (a document
//!   undo group opened before the first model call and closed however the request ends:
//!   completed, stopped, failed or out of budget).
//! - Tools lock the session per call, so the app stays responsive and sees each edit as
//!   it happens; tools that would replace the document are hidden, file tools are
//!   confined to the project's folder, nothing is saved or rendered unless asked.
//! - Each request message carries the app context (selection, playhead, loop, open clip)
//!   and the project summary when the project changed since the model last saw it.
//! - The history is append-only across requests (prompt caches stay warm); when a
//!   request's input grows past `compact_at_input_tokens` the conversation is condensed
//!   into a hand-off note (between tool rounds or between requests).
//! - A [`CancelToken`] stops the request at the next tool boundary (or aborts the model
//!   call in flight); the edits made so far stay, as one undo step.

use std::path::{Path, PathBuf};

use cadenza_mcp::{CadenzaServer, ChangeKey, SharedSession};
use serde_json::json;

use crate::agent::send_request;
use crate::budget::{Budget, Price};
use crate::cancel::CancelToken;
use crate::config::ProviderConfig;
use crate::error::AgentError;
use crate::observer::{AgentEvent, AgentObserver, clip};
use crate::prompt::{self, AppContext, SESSION_SYSTEM_PROMPT};
use crate::provider::{ChatResponse, FinishReason, LlmProvider, ProviderError};
use crate::tools::{PathPolicy, SESSION_HIDDEN_TOOLS, ToolBridge};
use crate::transcript::{TeeObserver, Transcript};
use crate::types::{AssistantTurn, Message, ToolResult, ToolSpec};

pub const DEFAULT_SESSION_MAX_COST_USD: f64 = 0.25;
/// Undo step of a [`SessionAgent::run_engineer_pass`].
pub const ENGINEER_UNDO_LABEL: &str = "AI mix pass";
/// Undo step of a [`SessionAgent::run_tuning_pass`].
pub const TUNING_UNDO_LABEL: &str = "AI tuning pass";
/// The only tools a read-only request ([`SessionAgent::run_explain`]) may call.
pub const EXPLAIN_TOOLS: &[&str] = &[
	"get_project",
	"get_device_info",
	"list_devices",
	"list_presets",
	"get_clip_notes",
	"get_controller_events",
	"history",
	"scale_info",
	"analyze_mix",
	"analyze_track",
	"mix_targets",
	"get_audio_info",
	"analyze_vocal",
	"get_audio_notes",
	"library_search",
	"library_get",
];
pub const DEFAULT_SESSION_MAX_TURNS: u32 = 40;
/// Condense the conversation once a request's input reaches this many tokens.
pub const DEFAULT_SESSION_COMPACT_AT: u64 = 60_000;

/// Settings of an in-app conversation.
#[derive(Debug, Clone)]
pub struct SessionConfig {
	pub provider: ProviderConfig,
	/// Spending cap per request, in USD.
	pub max_cost_usd: f64,
	/// Model responses per request.
	pub max_turns: u32,
	pub allow_plugins: bool,
	pub max_tool_result_chars: usize,
	pub compact_at_input_tokens: Option<u64>,
	/// The conversation's JSONL transcript (created on the first request).
	pub transcript_path: PathBuf,
	/// Price used for cost estimates when the provider reports none.
	pub price: Option<Price>,
}

impl SessionConfig {
	pub fn new(provider: ProviderConfig, transcript_path: impl Into<PathBuf>) -> Self {
		Self {
			provider,
			max_cost_usd: DEFAULT_SESSION_MAX_COST_USD,
			max_turns: DEFAULT_SESSION_MAX_TURNS,
			allow_plugins: false,
			max_tool_result_chars: 16_000,
			compact_at_input_tokens: Some(DEFAULT_SESSION_COMPACT_AT),
			transcript_path: transcript_path.into(),
			price: None,
		}
	}
}

/// How one request ended.
#[derive(Debug, Clone, PartialEq)]
pub enum RequestStatus {
	/// The model replied without further tool calls.
	Completed,
	/// The user stopped it.
	Cancelled,
	BudgetExhausted,
	MaxTurns,
	Refused,
	Failed(String),
}

#[derive(Debug, Clone)]
pub struct RequestOutcome {
	pub status: RequestStatus,
	/// The provider error behind [`RequestStatus::Failed`], when there was one.
	pub error: Option<ProviderError>,
	pub final_message: Option<String>,
	/// Whether the project changed (the request left an undo step).
	pub edited: bool,
	/// Label of that undo step.
	pub undo_label: String,
	pub turns: u32,
	/// This request's spending.
	pub budget: Budget,
	/// Spending of the whole conversation so far.
	pub total_usd: f64,
}

pub struct SessionAgent {
	config: SessionConfig,
	provider: Box<dyn LlmProvider>,
	tools: ToolBridge,
	session: SharedSession,
	history: Vec<Message>,
	total: Budget,
	transcript: Option<Transcript>,
	/// Project state the model last saw (its own edits included).
	seen: Option<ChangeKey>,
	/// A condensed earlier conversation waiting to prefix the next request.
	preamble: Option<String>,
	/// Input tokens of the latest response (drives condensing).
	last_input_tokens: u64,
	requests: u32,
}

impl SessionAgent {
	/// An agent over `session`, with the provider described by `config`.
	pub fn new(config: SessionConfig, session: SharedSession) -> Result<Self, AgentError> {
		let provider = config.provider.build()?;
		Self::with_provider(config, provider, session)
	}

	/// An agent over a given provider (embedding, tests).
	pub fn with_provider(
		config: SessionConfig,
		provider: Box<dyn LlmProvider>,
		session: SharedSession,
	) -> Result<Self, AgentError> {
		let tools = Self::bridge(&config, &session)?;
		Ok(Self {
			total: Budget::new(f64::INFINITY),
			config,
			provider,
			tools,
			session,
			history: Vec::new(),
			transcript: None,
			seen: None,
			preamble: None,
			last_input_tokens: 0,
			requests: 0,
		})
	}

	fn bridge(config: &SessionConfig, session: &SharedSession) -> Result<ToolBridge, AgentError> {
		// A placeholder root until the first request names the project's folder.
		let root = config
			.transcript_path
			.parent()
			.map(Path::to_path_buf)
			.unwrap_or_else(std::env::temp_dir);
		std::fs::create_dir_all(&root).map_err(|e| AgentError::io(&root, e))?;
		let policy = PathPolicy::new(&root, "project.cdz", "mix.wav")
			.map_err(|e| AgentError::io(&root, e))?;
		Ok(ToolBridge::with_hidden(
			CadenzaServer::with_shared_session(session.clone()),
			policy,
			config.allow_plugins,
			config.max_tool_result_chars,
			SESSION_HIDDEN_TOOLS,
		))
	}

	/// New settings (model, limits...) for the next requests; the conversation is kept.
	/// Reasoning payloads of another model are dropped (they are model-specific).
	pub fn reconfigure(
		&mut self,
		config: SessionConfig,
		provider: Box<dyn LlmProvider>,
	) -> Result<(), AgentError> {
		if provider.model() != self.provider.model() || provider.name() != self.provider.name() {
			for message in &mut self.history {
				if let Message::Assistant(turn) = message {
					turn.reasoning = None;
				}
			}
		}
		if config.allow_plugins != self.config.allow_plugins
			|| config.max_tool_result_chars != self.config.max_tool_result_chars
		{
			self.tools = Self::bridge(&config, &self.session)?;
		}
		let transcript_path = self.config.transcript_path.clone();
		self.config = config;
		// One conversation, one transcript.
		if self.transcript.is_some() {
			self.config.transcript_path = transcript_path;
		}
		self.provider = provider;
		Ok(())
	}

	pub fn config(&self) -> &SessionConfig {
		&self.config
	}

	pub fn provider(&self) -> &dyn LlmProvider {
		self.provider.as_ref()
	}

	pub fn system_prompt(&self) -> &'static str {
		SESSION_SYSTEM_PROMPT
	}

	pub fn tools(&self) -> &[ToolSpec] {
		self.tools.specs()
	}

	pub fn history(&self) -> &[Message] {
		&self.history
	}

	/// Spending of the whole conversation.
	pub fn total(&self) -> &Budget {
		&self.total
	}

	pub fn transcript_path(&self) -> &Path {
		&self.config.transcript_path
	}

	/// Forgets the conversation (the transcript keeps a record of the reset).
	pub fn clear(&mut self) {
		self.history.clear();
		self.preamble = None;
		self.seen = None;
		self.last_input_tokens = 0;
		if let Some(t) = &self.transcript {
			t.record("conversation_cleared", json!({}));
		}
	}

	fn transcript(&mut self) -> Result<&Transcript, AgentError> {
		if self.transcript.is_none() {
			let path = &self.config.transcript_path;
			if let Some(dir) = path.parent() {
				std::fs::create_dir_all(dir).map_err(|e| AgentError::io(dir, e))?;
			}
			let transcript = Transcript::create(path)?;
			transcript.record(
				"session_start",
				json!({
					"mode": "app",
					"provider": self.provider.name(),
					"model": self.provider.model(),
					"tool_count": self.tools.specs().len(),
					"system_prompt_chars": SESSION_SYSTEM_PROMPT.len(),
				}),
			);
			self.transcript = Some(transcript);
		}
		Ok(self.transcript.as_ref().expect("created above"))
	}

	fn change_key(&self) -> ChangeKey {
		lock(&self.session).change_key()
	}

	/// Points the file tools at the project's folder.
	fn update_policy(&mut self, context: &AppContext) -> Result<(), AgentError> {
		let root = PathBuf::from(&context.files_root);
		std::fs::create_dir_all(&root).map_err(|e| AgentError::io(&root, e))?;
		let project_file = context
			.project_path
			.as_deref()
			.and_then(|p| Path::new(p).file_name())
			.map(|f| f.to_string_lossy().into_owned())
			.unwrap_or_else(|| {
				let name = lock(&self.session).document().project().name.clone();
				format!("{}.cdz", crate::config::slugify(&name))
			});
		let stem = project_file
			.strip_suffix(".cdz")
			.unwrap_or(&project_file)
			.to_owned();
		let mut policy = PathPolicy::new(&root, &project_file, &format!("{stem}.wav"))
			.map_err(|e| AgentError::io(&root, e))?;
		if let Some(reference) = &context.reference_file {
			policy.allow_read(Path::new(reference));
		}
		self.tools.set_policy(policy);
		Ok(())
	}

	/// Runs one request to completion (or until stopped/out of budget). `Err` only for
	/// setup problems (unwritable folders); provider failures end the request with
	/// [`RequestStatus::Failed`] and the conversation stays usable.
	pub async fn run_request(
		&mut self,
		request: &str,
		context: &AppContext,
		observer: &dyn AgentObserver,
		cancel: &CancelToken,
	) -> Result<RequestOutcome, AgentError> {
		let label = format!("AI: {}", clip(request.trim(), 60));
		self.run_labeled(request, label, false, context, observer, cancel)
			.await
	}

	/// A read-only request ("Explain this device"): the model may only read the project
	/// ([`EXPLAIN_TOOLS`]); any other tool call is refused, and whatever changed anyway is
	/// reverted, so the request leaves no undo step. `request` is usually
	/// [`prompt::explain_request`].
	pub async fn run_explain(
		&mut self,
		request: &str,
		context: &AppContext,
		observer: &dyn AgentObserver,
		cancel: &CancelToken,
	) -> Result<RequestOutcome, AgentError> {
		let label = format!("AI: {}", clip(request.trim(), 60));
		self.run_labeled(request, label, true, context, observer, cancel)
			.await
	}

	/// A mix pass by the sound engineer (`cadenza mix`, the app's "AI mix pass"): the
	/// request is [`prompt::engineer_request`] and the whole pass is ONE undo step
	/// [`ENGINEER_UNDO_LABEL`].
	pub async fn run_engineer_pass(
		&mut self,
		brief: &prompt::EngineerBrief,
		context: &AppContext,
		observer: &dyn AgentObserver,
		cancel: &CancelToken,
	) -> Result<RequestOutcome, AgentError> {
		let request = prompt::engineer_request(brief);
		self.run_labeled(
			&request,
			ENGINEER_UNDO_LABEL.to_owned(),
			false,
			context,
			observer,
			cancel,
		)
		.await
	}

	/// A tuning pass by the tuning engineer on one audio clip (the app's "Tune with AI",
	/// `cadenza tune`): the request is [`prompt::tuning_request`] and the whole pass is ONE
	/// undo step [`TUNING_UNDO_LABEL`].
	pub async fn run_tuning_pass(
		&mut self,
		brief: &prompt::TuningBrief,
		context: &AppContext,
		observer: &dyn AgentObserver,
		cancel: &CancelToken,
	) -> Result<RequestOutcome, AgentError> {
		let request = prompt::tuning_request(brief);
		self.run_labeled(
			&request,
			TUNING_UNDO_LABEL.to_owned(),
			false,
			context,
			observer,
			cancel,
		)
		.await
	}

	async fn run_labeled(
		&mut self,
		request: &str,
		undo_label: String,
		read_only: bool,
		context: &AppContext,
		observer: &dyn AgentObserver,
		cancel: &CancelToken,
	) -> Result<RequestOutcome, AgentError> {
		self.update_policy(context)?;
		self.transcript()?;
		let transcript = self.transcript.take().expect("created above");
		let result = self
			.run_request_inner(
				request,
				undo_label,
				read_only,
				context,
				observer,
				cancel,
				&transcript,
			)
			.await;
		self.transcript = Some(transcript);
		Ok(result)
	}

	#[allow(clippy::too_many_arguments)]
	async fn run_request_inner(
		&mut self,
		request: &str,
		undo_label: String,
		read_only: bool,
		context: &AppContext,
		observer: &dyn AgentObserver,
		cancel: &CancelToken,
		transcript: &Transcript,
	) -> RequestOutcome {
		let tee = TeeObserver {
			user: observer,
			transcript,
		};
		self.requests += 1;
		transcript.record(
			"request_start",
			json!({
				"request": request,
				"index": self.requests,
				"model": self.provider.model(),
				"max_cost_usd": self.config.max_cost_usd,
				"max_turns": self.config.max_turns,
				"context": context.render(),
			}),
		);
		let mut budget = Budget::new(self.config.max_cost_usd);
		budget.price = self.config.price;
		let mut turn = 0u32;
		let mut error = None;

		// Condense between requests when the last one was already long.
		if self.compact_due() && !self.history.is_empty() {
			turn += 1;
			if let Err(err) = self
				.condense(None, turn, &mut budget, &tee, transcript, cancel)
				.await
			{
				tee.on_event(&AgentEvent::Notice(&format!(
					"could not condense the conversation ({err}); continuing with it as is"
				)));
			}
		}

		let key = self.change_key();
		let summary = if self.seen != Some(key) {
			Some(self.tools.dispatch("get_project", json!({})).await.content)
		} else {
			None
		};
		let mut message = prompt::session_request_message(
			request,
			context,
			self.config.max_cost_usd,
			summary.as_deref(),
		);
		if let Some(preamble) = self.preamble.take() {
			message = format!("{preamble}\n\n{message}");
		}
		close_dangling(&mut self.history);
		self.history.push(Message::User { content: message });

		lock(&self.session)
			.document_mut()
			.begin_group(undo_label.clone());
		let mut wrap_up_sent = false;
		let mut length_streak = 0u32;
		let mut final_message = None;

		let status = loop {
			if cancel.is_cancelled() {
				break RequestStatus::Cancelled;
			}
			if turn >= self.config.max_turns {
				break RequestStatus::MaxTurns;
			}
			if budget.exhausted() {
				break RequestStatus::BudgetExhausted;
			}
			if self.compact_due()
				&& matches!(self.history.last(), Some(Message::ToolResults { .. }))
			{
				turn += 1;
				if let Err(err) = self
					.condense(Some(request), turn, &mut budget, &tee, transcript, cancel)
					.await
				{
					error = Some(err.clone());
					break RequestStatus::Failed(err.to_string());
				}
				continue;
			}
			if !wrap_up_sent && budget.fraction_used() >= 0.8 {
				wrap_up_sent = true;
				let note = prompt::session_wrap_up_note(budget.fraction_used());
				tee.on_event(&AgentEvent::Notice(&note));
				append_note(&mut self.history, note);
			}

			turn += 1;
			tee.on_event(&AgentEvent::TurnStarted { turn });
			let response = tokio::select! {
				r = send_request(
					self.provider.as_ref(),
					SESSION_SYSTEM_PROMPT,
					&self.history,
					self.tools.specs(),
					&mut budget,
					turn,
					&tee,
					transcript,
				) => Some(r),
				_ = cancel.cancelled() => None,
			};
			let Some(response) = response else {
				self.history.push(Message::Assistant(AssistantTurn {
					text: Some(prompt::STOPPED_BY_USER.into()),
					..AssistantTurn::default()
				}));
				break RequestStatus::Cancelled;
			};
			let response: ChatResponse = match response {
				Ok(r) => r,
				Err(err) => {
					error = Some(err.clone());
					break RequestStatus::Failed(err.to_string());
				}
			};
			self.last_input_tokens = response.usage.input_tokens;
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
					self.history.push(Message::Assistant(response.turn));
					tee.on_event(&AgentEvent::Notice(
						"the model or a content filter declined to continue",
					));
					break RequestStatus::Refused;
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
						self.history.push(Message::ToolResults {
							results: calls
								.iter()
								.map(|c| ToolResult {
									call_id: c.id.clone(),
									name: c.name.clone(),
									content: prompt::LENGTH_TOOL_ERROR.into(),
									is_error: true,
								})
								.collect(),
						});
					}
					if length_streak >= 3 {
						break RequestStatus::Failed(
							"the model hit the output limit three times in a row".into(),
						);
					}
					continue;
				}
				_ => length_streak = 0,
			}

			let calls = response.turn.tool_calls.clone();
			let text = response.turn.text.clone();
			self.history.push(Message::Assistant(response.turn));
			if calls.is_empty() {
				final_message = text;
				break RequestStatus::Completed;
			}
			let mut results = Vec::with_capacity(calls.len());
			let mut stopped = false;
			for call in &calls {
				if stopped || cancel.is_cancelled() {
					stopped = true;
					results.push(ToolResult {
						call_id: call.id.clone(),
						name: call.name.clone(),
						content: prompt::NOT_EXECUTED_STOPPED.into(),
						is_error: true,
					});
					continue;
				}
				tee.on_event(&AgentEvent::ToolCall {
					name: &call.name,
					args: &call.arguments,
				});
				let outcome = if read_only && !EXPLAIN_TOOLS.contains(&call.name.as_str()) {
					self.tools.refusal(format!(
						"`{}` is not available: this is a read-only explain request. Do not change the project; answer from what get_project / get_device_info show, and describe any suggestion in words instead of applying it.",
						call.name
					))
				} else {
					self.tools.call(call).await
				};
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
			if stopped {
				break RequestStatus::Cancelled;
			}
		};

		let edited = {
			let mut session = lock(&self.session);
			let edited = if read_only {
				// Nothing a read-only request did may stay (nor leave an undo step).
				let _ = session.document_mut().abort_group();
				false
			} else {
				session.document_mut().end_group()
			};
			self.seen = Some(session.change_key());
			edited
		};
		self.total.absorb(&budget);
		match &status {
			RequestStatus::Cancelled => tee.on_event(&AgentEvent::Notice(
				"stopped: the edits made so far are kept as one undo step",
			)),
			RequestStatus::BudgetExhausted => tee.on_event(&AgentEvent::Notice(&format!(
				"stopped: this request reached its ${:.2} budget",
				self.config.max_cost_usd
			))),
			RequestStatus::MaxTurns => tee.on_event(&AgentEvent::Notice(&format!(
				"stopped: this request reached its limit of {} model responses",
				self.config.max_turns
			))),
			_ => {}
		}
		if let Some(text) = &final_message {
			tee.on_event(&AgentEvent::Final(text));
		}
		transcript.record(
			"request_outcome",
			json!({
				"status": format!("{status:?}"),
				"turns": turn,
				"edited": edited,
				"undo_label": undo_label,
				"budget": budget,
				"total_usd": self.total.spent_usd,
			}),
		);
		RequestOutcome {
			status,
			error,
			final_message,
			edited,
			undo_label,
			turns: turn,
			budget,
			total_usd: self.total.spent_usd,
		}
	}

	fn compact_due(&self) -> bool {
		self.config
			.compact_at_input_tokens
			.is_some_and(|limit| self.last_input_tokens >= limit)
	}

	/// Condenses the conversation into a hand-off note. Mid-request (`current` is the
	/// request being worked on) the history restarts from the note; between requests the
	/// note waits to prefix the next request's message.
	async fn condense(
		&mut self,
		current: Option<&str>,
		turn: u32,
		budget: &mut Budget,
		observer: &dyn AgentObserver,
		transcript: &Transcript,
		cancel: &CancelToken,
	) -> Result<(), ProviderError> {
		observer.on_event(&AgentEvent::Notice(
			"condensing the conversation to keep requests small",
		));
		close_dangling(&mut self.history);
		append_note(&mut self.history, prompt::SESSION_HANDOFF_REQUEST.into());
		let response = tokio::select! {
			r = send_request(
				self.provider.as_ref(),
				SESSION_SYSTEM_PROMPT,
				&self.history,
				self.tools.specs(),
				budget,
				turn,
				observer,
				transcript,
			) => Some(r?),
			_ = cancel.cancelled() => None,
		};
		let Some(response) = response else {
			// Leave the history as it was (minus the request for a note).
			self.history.pop();
			return Ok(());
		};
		let note = response
			.turn
			.text
			.filter(|t| !t.trim().is_empty())
			.unwrap_or_else(|| {
				"(no hand-off note was written; rebuild your picture from the project state)"
					.to_owned()
			});
		let state = self.tools.dispatch("get_project", json!({})).await.content;
		transcript.record(
			"checkpoint",
			json!({ "turn": turn, "replaced_messages": self.history.len(), "note_chars": note.len() }),
		);
		let resumed = prompt::session_resumed_message(&note, &state);
		self.last_input_tokens = 0;
		self.seen = Some(self.change_key());
		match current {
			Some(request) => {
				self.history = vec![Message::User {
					content: format!("{resumed}\n\nContinue the current request: {request}"),
				}];
			}
			None => {
				self.history.clear();
				self.preamble = Some(resumed);
			}
		}
		Ok(())
	}
}

fn lock(session: &SharedSession) -> std::sync::MutexGuard<'_, cadenza_mcp::Session> {
	session
		.lock()
		.unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// A request that failed or was stopped before the model answered leaves the history
/// ending in a user message; close it with a placeholder turn so roles keep alternating.
fn close_dangling(history: &mut Vec<Message>) {
	if matches!(history.last(), Some(Message::User { .. })) {
		history.push(Message::Assistant(AssistantTurn {
			text: Some(prompt::STOPPED_BY_USER.into()),
			..AssistantTurn::default()
		}));
	}
}

/// Adds a harness note as a user message (merged into the previous user message when
/// there is one, so roles keep alternating).
fn append_note(history: &mut Vec<Message>, note: String) {
	match history.last_mut() {
		Some(Message::User { content }) => {
			content.push_str("\n\n");
			content.push_str(&note);
		}
		_ => history.push(Message::User { content: note }),
	}
}

/// A user-facing explanation of a provider failure, with what to do about it.
pub fn actionable_error(err: &ProviderError) -> String {
	match err {
		ProviderError::Credentials(_) => "No API key is configured for this provider. Add one in \
			 AI settings (or set the environment variable / .env entry)."
			.into(),
		ProviderError::Http { status: 401, .. } | ProviderError::Http { status: 403, .. } => {
			format!("The provider rejected the API key ({err}). Check it in AI settings.")
		}
		ProviderError::Http { status: 402, .. } => {
			format!("The provider account has no credit left ({err}).")
		}
		ProviderError::Http { status: 404, .. } => format!(
			"The endpoint or model was not found ({err}). Check the model id and base URL in \
			 AI settings."
		),
		ProviderError::Http { status: 429, .. } => {
			format!("Rate limited by the provider ({err}). Wait a moment and try again.")
		}
		ProviderError::Network(_) => {
			format!("Could not reach the provider ({err}). Check your connection and the base URL.")
		}
		_ => format!("The model request failed: {err}"),
	}
}
