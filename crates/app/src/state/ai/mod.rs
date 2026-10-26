//! The AI producer chat: conversation state for the chat panel and the controller that
//! drives the background worker ([`worker`]). No egui here (unit-tested).
//!
//! Editing while the AI works — the decision: the AI's edits of one request form ONE undo
//! step ("AI: <request>", a document undo group closed when the request ends), and while
//! a request runs the user's edit commands are refused (the edit panels are shown
//! read-only; undo/redo, open/new and imports wait), except values coming from plugin
//! editor windows, which are queued and applied right after. Transport stays usable. Stop
//! cancels at the next tool boundary (or aborts the model call in flight) and still
//! closes the group, so whatever was done remains one undo step. Interleaving user edits
//! inside the group would make "undo the AI request" undo user work too, and queueing
//! arbitrary gestures against a project that changes underneath them is fragile.

pub mod markdown;
pub mod secrets;
pub mod settings;
pub mod worker;

use crate::i18n::{tr, trf};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use cadenza_agent::{AppContext, CancelToken, EngineerBrief, RequestStatus, TuningBrief};
use cadenza_mcp::SharedSession;

use crate::audio::Waker;
use secrets::{KeySource, SecretStore, SystemSecretStore};
use settings::AiSettings;
use worker::{Event, Finished, ProviderFactory, Worker, WorkerSetup};

/// One row of the chat.
#[derive(Debug, Clone, PartialEq)]
pub enum ChatItem {
	User(String),
	Assistant {
		text: String,
		streaming: bool,
	},
	Thinking {
		text: String,
		streaming: bool,
	},
	Tool {
		name: String,
		args: String,
		/// `None` while running.
		ok: Option<bool>,
		summary: String,
	},
	Notice(String),
	/// A failure the user can act on; `settings` offers the AI settings.
	Error {
		text: String,
		settings: bool,
	},
	/// End of a request: what it cost and how it ended.
	Footer {
		status: String,
		cost_usd: f64,
		estimated: bool,
		total_usd: f64,
		undo_label: Option<String>,
	},
}

/// The request in flight.
#[derive(Debug, Clone, PartialEq)]
pub struct Running {
	pub request: String,
	pub started: Instant,
	pub cost_usd: f64,
	pub estimated: bool,
	/// Stop was pressed.
	pub stopping: bool,
}

pub struct AiChat {
	pub settings: AiSettings,
	pub items: Vec<ChatItem>,
	/// The input box.
	pub input: String,
	/// Spending of this app session's conversation.
	pub total_usd: f64,
	pub running: Option<Running>,
	/// Where the current provider's key comes from (after a check).
	pub key_source: Option<KeySource>,
	/// Outcome of the last key save/removal.
	pub key_message: Option<Result<String, String>>,
	/// The AI settings dialog is open.
	pub settings_open: bool,
	/// A key typed into the settings dialog (cleared once handed to the worker).
	pub key_input: String,
	session_dir: PathBuf,
	env_dir: PathBuf,
	worker: Option<Worker>,
	cancel: CancelToken,
	factory: ProviderFactory,
	store: Arc<dyn SecretStore>,
	waker: Option<Waker>,
}

/// `sessions` in the state folder (`$XDG_STATE_HOME/cadenza` or `~/.local/state/cadenza` on
/// Linux; see [`cadenza_core::paths`]).
pub fn sessions_dir() -> PathBuf {
	cadenza_core::paths::state_dir().join("sessions")
}

impl AiChat {
	/// The app's chat: real providers, the OS secret store, a new session folder.
	pub fn new(waker: Option<Waker>) -> Self {
		let stamp = std::time::SystemTime::now()
			.duration_since(std::time::UNIX_EPOCH)
			.map(|d| d.as_secs())
			.unwrap_or_default();
		let dir = sessions_dir().join(format!("chat-{stamp}-{}", std::process::id()));
		Self::with_parts(
			worker::default_factory(),
			Arc::new(SystemSecretStore::new()),
			dir,
			std::env::current_dir().unwrap_or_default(),
			waker,
		)
	}

	/// A chat with injected parts (tests: scripted providers, in-memory keys).
	pub fn with_parts(
		factory: ProviderFactory,
		store: Arc<dyn SecretStore>,
		session_dir: PathBuf,
		env_dir: PathBuf,
		waker: Option<Waker>,
	) -> Self {
		Self {
			settings: AiSettings::default(),
			items: Vec::new(),
			input: String::new(),
			total_usd: 0.0,
			running: None,
			key_source: None,
			key_message: None,
			settings_open: false,
			key_input: String::new(),
			session_dir,
			env_dir,
			worker: None,
			cancel: CancelToken::new(),
			factory,
			store,
			waker,
		}
	}

	pub fn busy(&self) -> bool {
		self.running.is_some()
	}

	/// This chat session's folder (transcript, files of unsaved projects).
	pub fn session_dir(&self) -> &Path {
		&self.session_dir
	}

	/// The JSONL transcript (written once the first request runs).
	pub fn transcript_path(&self) -> PathBuf {
		self.session_dir.join("transcript.jsonl")
	}

	fn worker(&mut self, session: &SharedSession) -> &Worker {
		if self.worker.is_none() {
			self.worker = Some(Worker::start(WorkerSetup {
				session: session.clone(),
				factory: self.factory.clone(),
				store: self.store.clone(),
				env_dir: self.env_dir.clone(),
				session_dir: self.session_dir.clone(),
				cancel: self.cancel.clone(),
				waker: self.waker.clone(),
			}));
		}
		self.worker.as_ref().expect("started above")
	}

	/// Sends a request (ignored while one runs or when empty). Returns whether it was sent.
	pub fn send(&mut self, request: &str, context: AppContext, session: &SharedSession) -> bool {
		self.send_with(request, worker::Pass::Chat, context, session)
	}

	/// Sends a read-only explain request (`text`, see `prompt::explain_request`), shown
	/// in the chat as `label`: the agent may only read, so it leaves no undo step.
	pub fn send_explain(
		&mut self,
		label: &str,
		text: String,
		context: AppContext,
		session: &SharedSession,
	) -> bool {
		self.send_with(label, worker::Pass::Explain(text), context, session)
	}

	/// Runs a sound-engineer mix pass (one undo step "AI mix pass"); shown in the chat as
	/// `label`. Returns whether it was sent.
	pub fn send_mix_pass(
		&mut self,
		label: &str,
		brief: EngineerBrief,
		context: AppContext,
		session: &SharedSession,
	) -> bool {
		self.send_with(label, worker::Pass::Engineer(brief), context, session)
	}

	/// Runs a tuning pass on one audio clip (one undo step "AI tuning pass"); shown in the
	/// chat as `label`. Returns whether it was sent.
	pub fn send_tuning_pass(
		&mut self,
		label: &str,
		brief: TuningBrief,
		context: AppContext,
		session: &SharedSession,
	) -> bool {
		self.send_with(label, worker::Pass::Tuning(brief), context, session)
	}

	fn send_with(
		&mut self,
		request: &str,
		pass: worker::Pass,
		context: AppContext,
		session: &SharedSession,
	) -> bool {
		let request = request.trim().to_owned();
		if request.is_empty() || self.busy() {
			return false;
		}
		self.cancel.reset();
		self.items.push(ChatItem::User(request.clone()));
		self.running = Some(Running {
			request: request.clone(),
			started: Instant::now(),
			cost_usd: 0.0,
			estimated: false,
			stopping: false,
		});
		let settings = self.settings.clone();
		let sent = self.worker(session).send(worker::Command::Run {
			request,
			pass,
			context: Box::new(context),
			settings,
		});
		if !sent {
			self.running = None;
			self.items.push(ChatItem::Error {
				text: tr("The AI thread stopped; restart the app to use the chat.").into(),
				settings: false,
			});
		}
		sent
	}

	/// Stops the running request at the next tool boundary.
	pub fn stop(&mut self) {
		if let Some(r) = &mut self.running {
			r.stopping = true;
			self.cancel.cancel();
		}
	}

	/// Starts a new conversation (not while a request runs).
	pub fn clear(&mut self) {
		if self.busy() {
			return;
		}
		self.items.clear();
		if let Some(w) = &self.worker {
			w.send(worker::Command::Clear);
		}
	}

	/// Asks the worker where the selected provider's key comes from.
	pub fn check_key(&mut self, session: &SharedSession) {
		let settings = self.settings.clone();
		self.worker(session)
			.send(worker::Command::CheckKey(settings));
	}

	/// Stores the typed key (`Some`) or removes the stored one (`None`).
	pub fn store_key(&mut self, key: Option<String>, session: &SharedSession) {
		self.key_input.clear();
		let settings = self.settings.clone();
		self.worker(session)
			.send(worker::Command::StoreKey { settings, key });
	}

	/// Applies the worker's events to the chat; returns the requests that finished.
	pub fn poll(&mut self) -> Vec<Finished> {
		let mut finished = Vec::new();
		let Some(w) = &self.worker else {
			return finished;
		};
		let mut events = Vec::new();
		while let Some(e) = w.poll() {
			events.push(e);
		}
		for event in events {
			if let Some(f) = self.apply(event) {
				finished.push(f);
			}
		}
		finished
	}

	fn apply(&mut self, event: Event) -> Option<Finished> {
		match event {
			Event::TextDelta(t) => match self.items.last_mut() {
				Some(ChatItem::Assistant {
					text,
					streaming: true,
				}) => text.push_str(&t),
				_ => self.items.push(ChatItem::Assistant {
					text: t,
					streaming: true,
				}),
			},
			Event::ThinkingDelta(t) => match self.items.last_mut() {
				Some(ChatItem::Thinking {
					text,
					streaming: true,
				}) => text.push_str(&t),
				_ => self.items.push(ChatItem::Thinking {
					text: t,
					streaming: true,
				}),
			},
			Event::Thinking(full) => self.finish_streamed(full, true),
			Event::Text(full) => self.finish_streamed(full, false),
			Event::ToolCall { name, args } => {
				self.close_streams();
				self.items.push(ChatItem::Tool {
					name,
					args,
					ok: None,
					summary: String::new(),
				});
			}
			Event::ToolResult { name, ok, summary } => {
				let slot = self.items.iter_mut().rev().find_map(|item| match item {
					ChatItem::Tool {
						name: n,
						ok: o @ None,
						summary: s,
						..
					} if *n == name => Some((o, s)),
					_ => None,
				});
				match slot {
					Some((o, s)) => {
						*o = Some(ok);
						*s = summary;
					}
					None => self.items.push(ChatItem::Tool {
						name,
						args: String::new(),
						ok: Some(ok),
						summary,
					}),
				}
			}
			Event::Cost {
				request_usd,
				estimated,
			} => {
				if let Some(r) = &mut self.running {
					r.cost_usd = request_usd;
					r.estimated |= estimated;
				}
			}
			Event::Retry(reason) => {
				// The turn starts over: its partial text would be repeated.
				if matches!(
					self.items.last(),
					Some(
						ChatItem::Assistant {
							streaming: true,
							..
						} | ChatItem::Thinking {
							streaming: true,
							..
						}
					)
				) {
					self.items.pop();
				}
				self.items.push(ChatItem::Notice(trf(
					"retrying: {reason}",
					&[("reason", &reason)],
				)));
			}
			Event::Notice(text) => self.items.push(ChatItem::Notice(text)),
			Event::KeyStatus(source) => self.key_source = Some(source),
			Event::KeyStored(result) => self.key_message = Some(result),
			Event::Finished(f) => {
				self.close_streams();
				self.running = None;
				self.total_usd += f.cost_usd;
				if let Some(text) = &f.error {
					self.items.push(ChatItem::Error {
						text: text.clone(),
						settings: f.needs_settings,
					});
					if f.needs_settings {
						self.settings_open = true;
					}
				}
				let status = match &f.status {
					RequestStatus::Completed => "done",
					RequestStatus::Cancelled => "stopped",
					RequestStatus::BudgetExhausted => tr("budget reached"),
					RequestStatus::MaxTurns => tr("turn limit reached"),
					RequestStatus::Refused => "declined",
					RequestStatus::Failed(_) => "failed",
				};
				self.items.push(ChatItem::Footer {
					status: status.into(),
					cost_usd: f.cost_usd,
					estimated: f.estimated,
					total_usd: self.total_usd,
					undo_label: f.edited.then(|| f.undo_label.clone()),
				});
				return Some(f);
			}
		}
		None
	}

	/// The complete text of a turn replaces what streamed in (or is added if nothing did).
	fn finish_streamed(&mut self, full: String, thinking: bool) {
		let pos = self.items.iter().rposition(|item| match item {
			ChatItem::Thinking { streaming, .. } if thinking => *streaming,
			ChatItem::Assistant { streaming, .. } if !thinking => *streaming,
			_ => false,
		});
		match pos {
			Some(i) => {
				self.items[i] = if thinking {
					ChatItem::Thinking {
						text: full,
						streaming: false,
					}
				} else {
					ChatItem::Assistant {
						text: full,
						streaming: false,
					}
				}
			}
			None if thinking => self.items.push(ChatItem::Thinking {
				text: full,
				streaming: false,
			}),
			None => self.items.push(ChatItem::Assistant {
				text: full,
				streaming: false,
			}),
		}
	}

	fn close_streams(&mut self) {
		for item in &mut self.items {
			if let ChatItem::Assistant { streaming, .. } | ChatItem::Thinking { streaming, .. } =
				item
			{
				*streaming = false;
			}
		}
	}
}

/// Quick requests offered above the input, built from what is selected.
pub fn quick_actions(
	project: &cadenza_model::Project,
	selected_track: Option<cadenza_core::TrackId>,
	selected_clip: Option<cadenza_core::ClipId>,
) -> Vec<(String, String)> {
	let mut out = Vec::new();
	if let Some(track) = selected_track.and_then(|t| project.track(t)) {
		match track.kind {
			cadenza_model::TrackKind::Instrument => {
				out.push((
					trf("Bassline for {name}", &[("name", &track.name)]),
					trf(
						"Write a bassline for the selected track \"{name}\" that fits the song",
						&[("name", &track.name)],
					),
				));
			}
			_ => out.push((
				trf("Mix {name}", &[("name", &track.name)]),
				trf(
					"Improve the mix of the selected track \"{name}\"",
					&[("name", &track.name)],
				),
			)),
		}
	}
	if let Some((_, clip)) = selected_clip.and_then(|c| project.find_clip(c)) {
		out.push((
			trf("Vary {name}", &[("name", &clip.name)]),
			trf(
				"Write a variation of the selected clip \"{name}\"",
				&[("name", &clip.name)],
			),
		));
	}
	out.push((tr("Mix the project").into(), tr("Mix the project").into()));
	out.push((
		tr("Continue the arrangement").into(),
		tr("Continue the arrangement: add the next section after the last one").into(),
	));
	out
}
