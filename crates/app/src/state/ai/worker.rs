//! The AI worker: one thread with its own tokio runtime that owns the conversation
//! ([`SessionAgent`]) and everything that may block (model requests, the secret store).
//! The UI talks to it through channels and polls its events once per frame, so it never
//! waits on the network or the keyring. The agent edits the app's shared session one
//! tool call at a time; the app picks those edits up with its per-frame sync.

use crate::i18n::{tr, trf};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};

use cadenza_agent::observer::{AgentEvent, AgentObserver};
use cadenza_agent::provider::ProviderError;
use cadenza_agent::session::{RequestStatus, actionable_error};
use cadenza_agent::{
	AppContext, CancelToken, EngineerBrief, LlmProvider, SessionAgent, TuningBrief,
};
use cadenza_mcp::SharedSession;

use super::secrets::{KeySource, SecretStore, resolve_key};
use super::settings::AiSettings;
use crate::audio::Waker;

/// Builds the provider for a request: `(settings, api key)`. The app uses
/// [`default_factory`]; tests inject scripted providers.
pub type ProviderFactory =
	Arc<dyn Fn(&AiSettings, Option<String>) -> Result<Box<dyn LlmProvider>, String> + Send + Sync>;

/// Real providers from the settings.
pub fn default_factory() -> ProviderFactory {
	Arc::new(|settings, key| {
		settings
			.provider_config(key)
			.build()
			.map_err(|e| e.to_string())
	})
}

/// What kind of request a run is.
pub enum Pass {
	/// The chat request itself.
	Chat,
	/// A mix pass by the sound engineer.
	Engineer(EngineerBrief),
	/// A read-only explain request (the text sent; the run's `request` is what the chat
	/// shows).
	Explain(String),
	/// A tuning pass by the tuning engineer on one audio clip.
	Tuning(TuningBrief),
}

pub enum Command {
	Run {
		request: String,
		pass: Pass,
		context: Box<AppContext>,
		settings: AiSettings,
	},
	/// Starts a new conversation.
	Clear,
	/// Stores (`Some`) or forgets (`None`) the key of the settings' provider.
	StoreKey {
		settings: AiSettings,
		key: Option<String>,
	},
	/// Reports where the key of the settings' provider comes from.
	CheckKey(AiSettings),
}

/// How a request ended, for the chat.
#[derive(Debug, Clone, PartialEq)]
pub struct Finished {
	pub status: RequestStatus,
	/// Actionable error text (no key, network, budget...).
	pub error: Option<String>,
	/// The error is about credentials: offer the settings.
	pub needs_settings: bool,
	pub edited: bool,
	pub undo_label: String,
	pub cost_usd: f64,
	pub estimated: bool,
	pub total_usd: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
	TextDelta(String),
	ThinkingDelta(String),
	Text(String),
	Thinking(String),
	ToolCall {
		name: String,
		args: String,
	},
	ToolResult {
		name: String,
		ok: bool,
		summary: String,
	},
	/// Spending of the running request so far.
	Cost {
		request_usd: f64,
		estimated: bool,
	},
	/// A retry restarts the turn: drop its partial text.
	Retry(String),
	Notice(String),
	Finished(Finished),
	KeyStatus(KeySource),
	KeyStored(Result<String, String>),
}

/// Forwards agent events to the UI thread.
struct Forward {
	tx: Sender<Event>,
	waker: Option<Waker>,
}

impl Forward {
	fn send(&self, event: Event) {
		let _ = self.tx.send(event);
		if let Some(w) = &self.waker {
			w();
		}
	}
}

impl AgentObserver for Forward {
	fn on_event(&self, event: &AgentEvent<'_>) {
		let e = match event {
			AgentEvent::TextDelta(t) => Event::TextDelta((*t).to_owned()),
			AgentEvent::ThinkingDelta(t) => Event::ThinkingDelta((*t).to_owned()),
			AgentEvent::Text(t) => Event::Text((*t).to_owned()),
			AgentEvent::Thinking(t) => Event::Thinking((*t).to_owned()),
			AgentEvent::ToolCall { name, args } => Event::ToolCall {
				name: (*name).to_owned(),
				args: (*args).to_owned(),
			},
			AgentEvent::ToolResult { name, ok, summary } => Event::ToolResult {
				name: (*name).to_owned(),
				ok: *ok,
				summary: (*summary).to_owned(),
			},
			AgentEvent::Usage {
				total_cost_usd,
				estimated,
				..
			} => Event::Cost {
				request_usd: *total_cost_usd,
				estimated: *estimated,
			},
			AgentEvent::Retry { reason, .. } => Event::Retry((*reason).to_owned()),
			AgentEvent::Notice(t) => Event::Notice((*t).to_owned()),
			AgentEvent::TurnStarted { .. } | AgentEvent::Final(_) => return,
		};
		self.send(e);
	}
}

pub struct Worker {
	tx: Sender<Command>,
	rx: Receiver<Event>,
}

pub struct WorkerSetup {
	pub session: SharedSession,
	pub factory: ProviderFactory,
	pub store: Arc<dyn SecretStore>,
	/// Where `.env` lookups start.
	pub env_dir: PathBuf,
	/// Folder of this chat session (transcript; files of unsaved projects).
	pub session_dir: PathBuf,
	pub cancel: CancelToken,
	pub waker: Option<Waker>,
}

impl Worker {
	pub fn start(setup: WorkerSetup) -> Self {
		let (tx, commands) = channel::<Command>();
		let (events, rx) = channel::<Event>();
		std::thread::Builder::new()
			.name("cadenza-ai".into())
			.spawn(move || run(setup, commands, events))
			.expect("spawning the AI thread");
		Self { tx, rx }
	}

	pub fn send(&self, command: Command) -> bool {
		self.tx.send(command).is_ok()
	}

	pub fn poll(&self) -> Option<Event> {
		self.rx.try_recv().ok()
	}
}

fn run(setup: WorkerSetup, commands: Receiver<Command>, events: Sender<Event>) {
	let forward = Forward {
		tx: events,
		waker: setup.waker.clone(),
	};
	let runtime = match tokio::runtime::Builder::new_current_thread()
		.enable_all()
		.build()
	{
		Ok(r) => r,
		Err(e) => {
			forward.send(Event::Finished(failure(trf(
				"the AI thread could not start: {e}",
				&[("e", &e)],
			))));
			return;
		}
	};
	let transcript = setup.session_dir.join("transcript.jsonl");
	let mut agent: Option<SessionAgent> = None;
	while let Ok(command) = commands.recv() {
		match command {
			Command::Run {
				request,
				pass,
				context,
				settings,
			} => {
				let (key, source) =
					resolve_key(settings.provider, &setup.env_dir, setup.store.as_ref());
				if key.is_none() && settings.provider.needs_key() {
					let mut f = failure(trf(
						"No API key for {name}: add one in AI settings, or set {key_env} in the environment or a .env file.",
						&[
							("name", &settings.provider.name()),
							("key_env", &settings.provider.key_env()),
						],
					));
					f.needs_settings = true;
					forward.send(Event::KeyStatus(source));
					forward.send(Event::Finished(f));
					continue;
				}
				let config = settings.session_config(key.clone(), transcript.clone());
				let provider = match (setup.factory)(&settings, key) {
					Ok(p) => p,
					Err(e) => {
						forward.send(Event::Finished(failure(trf(
							"Cannot set up the provider: {e}",
							&[("e", &e)],
						))));
						continue;
					}
				};
				let ready = match agent.as_mut() {
					Some(a) => a.reconfigure(config, provider).map(|_| ()),
					None => SessionAgent::with_provider(config, provider, setup.session.clone())
						.map(|a| agent = Some(a)),
				};
				if let Err(e) = ready {
					forward.send(Event::Finished(failure(e.to_string())));
					continue;
				}
				let a = agent.as_mut().expect("set above");
				let result = runtime.block_on(async {
					match &pass {
						Pass::Engineer(brief) => {
							a.run_engineer_pass(brief, &context, &forward, &setup.cancel)
								.await
						}
						Pass::Tuning(brief) => {
							a.run_tuning_pass(brief, &context, &forward, &setup.cancel)
								.await
						}
						Pass::Explain(text) => {
							a.run_explain(text, &context, &forward, &setup.cancel).await
						}
						Pass::Chat => {
							a.run_request(&request, &context, &forward, &setup.cancel)
								.await
						}
					}
				});
				let finished = match result {
					Ok(outcome) => {
						let needs_settings = matches!(
							outcome.error,
							Some(ProviderError::Credentials(_))
								| Some(ProviderError::Http {
									status: 401 | 403 | 404,
									..
								})
						);
						let error = match (&outcome.status, &outcome.error) {
							(_, Some(err)) => Some(actionable_error(err)),
							(RequestStatus::Failed(why), None) => Some(why.clone()),
							(RequestStatus::Refused, _) => {
								Some(tr("The model declined to continue.").into())
							}
							_ => None,
						};
						Finished {
							status: outcome.status,
							error,
							needs_settings,
							edited: outcome.edited,
							undo_label: outcome.undo_label,
							cost_usd: outcome.budget.spent_usd,
							estimated: outcome.budget.estimated_usd > 0.0,
							total_usd: outcome.total_usd,
						}
					}
					Err(e) => failure(e.to_string()),
				};
				forward.send(Event::Finished(finished));
			}
			Command::Clear => {
				if let Some(a) = agent.as_mut() {
					a.clear();
				}
			}
			Command::StoreKey { settings, key } => {
				let account = settings.provider.account();
				let result = match key {
					Some(k) => setup
						.store
						.set(account, k.trim())
						.map(|_| trf("Key saved in {name}", &[("name", &setup.store.name())])),
					None => setup
						.store
						.delete(account)
						.map(|_| tr("Stored key removed").to_owned()),
				};
				forward.send(Event::KeyStored(result));
				let (_, source) =
					resolve_key(settings.provider, &setup.env_dir, setup.store.as_ref());
				forward.send(Event::KeyStatus(source));
			}
			Command::CheckKey(settings) => {
				let (_, source) =
					resolve_key(settings.provider, &setup.env_dir, setup.store.as_ref());
				forward.send(Event::KeyStatus(source));
			}
		}
	}
}

fn failure(message: String) -> Finished {
	Finished {
		status: RequestStatus::Failed(message.clone()),
		error: Some(message),
		needs_settings: false,
		edited: false,
		undo_label: String::new(),
		cost_usd: 0.0,
		estimated: false,
		total_usd: 0.0,
	}
}
