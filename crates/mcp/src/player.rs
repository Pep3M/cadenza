//! A dedicated OS thread that owns the realtime [`cadenza_engine::Player`].
//!
//! `cpal::Stream` (held inside `Player`) is not `Send` on every platform, so it must stay
//! on the thread that created it. Every interaction goes through a plain command channel
//! instead of sharing the `Player` behind a lock.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;

use cadenza_core::Tick;
use cadenza_engine::Player;
use cadenza_model::Project;
use cadenza_plugin_host::PluginCatalog;

use crate::error::McpError;

enum Command {
	Load(
		Box<Project>,
		Option<Arc<PluginCatalog>>,
		mpsc::Sender<Result<Vec<String>, String>>,
	),
	Play(Option<f64>, mpsc::Sender<Result<(), String>>),
	Stop(mpsc::Sender<Result<(), String>>),
	Position(mpsc::Sender<f64>),
}

fn thread_gone() -> McpError {
	McpError::Invalid("the audio playback thread is no longer running".into())
}

/// Handle to the dedicated playback thread. Cheap to hold (just a channel sender); the
/// thread and its `Player`/`cpal::Stream` live for as long as this handle does.
pub struct PlayerHandle {
	tx: mpsc::Sender<Command>,
	/// Mirrors the last `play`/`stop` we sent; a hint used to decide whether a project
	/// mutation should be pushed to the player, not a live query of the engine (which
	/// would mean a round trip to the audio thread on every mutating tool call).
	playing: Arc<AtomicBool>,
}

impl PlayerHandle {
	/// Opens the default audio output device on a dedicated thread. Returns a clear error
	/// (never panics or crashes the server) when there is no output device.
	pub fn start() -> Result<Self, McpError> {
		let (init_tx, init_rx) = mpsc::channel();
		let (tx, rx) = mpsc::channel::<Command>();

		thread::Builder::new()
			.name("cadenza-player".into())
			.spawn(move || {
				let mut player = match Player::new() {
					Ok(player) => player,
					Err(e) => {
						let _ = init_tx.send(Err(e.to_string()));
						return;
					}
				};
				let _ = init_tx.send(Ok(()));
				for cmd in rx {
					match cmd {
						Command::Load(project, plugins, reply) => {
							player.set_plugins(plugins);
							let _ = reply.send(player.load(&project).map_err(|e| e.to_string()));
						}
						Command::Play(from_beat, reply) => {
							let result = (|| {
								if let Some(beat) = from_beat {
									player
										.seek(Tick::from_beats(beat))
										.map_err(|e| e.to_string())?;
								}
								player.play().map_err(|e| e.to_string())
							})();
							let _ = reply.send(result);
						}
						Command::Stop(reply) => {
							let _ = reply.send(player.stop().map_err(|e| e.to_string()));
						}
						Command::Position(reply) => {
							let _ = reply.send(player.position().beats());
						}
					}
				}
			})
			.map_err(|e| {
				McpError::Invalid(format!("could not start the audio playback thread: {e}"))
			})?;

		match init_rx.recv() {
			Ok(Ok(())) => Ok(Self {
				tx,
				playing: Arc::new(AtomicBool::new(false)),
			}),
			Ok(Err(message)) => Err(McpError::Invalid(format!(
				"could not open an audio output device: {message}"
			))),
			Err(_) => Err(McpError::Invalid(
				"the audio playback thread exited before starting".into(),
			)),
		}
	}

	fn roundtrip<T>(
		&self,
		build: impl FnOnce(mpsc::Sender<Result<T, String>>) -> Command,
	) -> Result<T, McpError> {
		let (reply_tx, reply_rx) = mpsc::channel();
		self.tx.send(build(reply_tx)).map_err(|_| thread_gone())?;
		reply_rx
			.recv()
			.map_err(|_| thread_gone())?
			.map_err(McpError::Invalid)
	}

	/// Compiles and swaps `project` into the player without interrupting the stream.
	/// `plugins` is needed for projects with plugin devices.
	pub fn load(
		&self,
		project: Project,
		plugins: Option<Arc<PluginCatalog>>,
	) -> Result<Vec<String>, McpError> {
		self.roundtrip(|reply| Command::Load(Box::new(project), plugins, reply))
	}

	/// Starts (or resumes) playback, optionally seeking first.
	pub fn play(&self, from_beat: Option<f64>) -> Result<(), McpError> {
		self.roundtrip(|reply| Command::Play(from_beat, reply))?;
		self.playing.store(true, Ordering::Relaxed);
		Ok(())
	}

	pub fn stop(&self) -> Result<(), McpError> {
		self.roundtrip(Command::Stop)?;
		self.playing.store(false, Ordering::Relaxed);
		Ok(())
	}

	pub fn position_beats(&self) -> Result<f64, McpError> {
		let (reply_tx, reply_rx) = mpsc::channel();
		self.tx
			.send(Command::Position(reply_tx))
			.map_err(|_| thread_gone())?;
		reply_rx.recv().map_err(|_| thread_gone())
	}

	/// See the `playing` field's doc comment.
	pub fn is_playing_hint(&self) -> bool {
		self.playing.load(Ordering::Relaxed)
	}
}
