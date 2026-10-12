//! The realtime [`Player`] on its own thread, driven without ever blocking the UI.
//!
//! Like `cadenza_mcp::player::PlayerHandle`, the `Player` (whose `cpal::Stream` is not
//! `Send` everywhere) lives on a dedicated thread and is driven through a command
//! channel. Unlike the MCP handle, nothing here waits for a reply: results come back as
//! [`AudioEvent`]s that the UI polls once per frame, and a burst of project reloads is
//! coalesced into the latest one, because compiling a project (decoding audio, starting
//! plugin sandboxes) can take far longer than a frame.
//!
//! The playhead and play state are read lock-free through a [`PlayerClock`], and levels
//! through the loaded graph's [`MeterBank`].

use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use cadenza_core::{DeviceId, TempoMap, Tick, TrackId};
use cadenza_engine::{MeterBank, Player, PlayerClock, PluginCrash, PluginHosting};
use cadenza_model::{AutomationLane, Project};
use cadenza_plugin_host::PluginCatalog;

/// How often the player thread polls sandboxed plugins for crashes.
const CRASH_POLL: Duration = Duration::from_millis(500);

/// Wakes the UI when an event arrives (e.g. `egui::Context::request_repaint`).
pub type Waker = Arc<dyn Fn() + Send + Sync>;

enum Request {
	Load {
		project: Box<Project>,
		plugins: Option<Arc<PluginCatalog>>,
		hosting: PluginHosting,
		generation: u64,
	},
	Play,
	Stop,
	Seek(Tick),
	Volume(Option<TrackId>, f32),
	Pan(Option<TrackId>, f32),
	/// Live device parameter (device, param id, plain value).
	Param(DeviceId, String, f32),
	/// Live send level (dB).
	Send(DeviceId, f32),
	/// New automation lanes for the running graph.
	Automation {
		lanes: Vec<AutomationLane>,
		tempo: TempoMap,
	},
	/// Note preview: `Some(velocity)` = note on, `None` = note off.
	Preview(TrackId, u8, Option<f32>),
	Shutdown,
}

/// What the player thread reports back.
pub enum AudioEvent {
	Ready {
		clock: PlayerClock,
	},
	/// No output device (or it failed to open): the app keeps working without sound.
	Unavailable(String),
	Loaded {
		generation: u64,
		warnings: Vec<String>,
		meters: Option<Arc<MeterBank>>,
	},
	LoadFailed {
		generation: u64,
		error: String,
	},
	PluginCrashes(Vec<PluginCrash>),
	/// A live change could not reach the running graph (e.g. an automation lane was
	/// removed): the project must be reloaded.
	ReloadNeeded,
	Error(String),
}

/// Handle to the player thread. Dropping it stops playback and closes the device.
pub struct AudioEngine {
	tx: Sender<Request>,
	events: Receiver<AudioEvent>,
	thread: Option<JoinHandle<()>>,
}

impl AudioEngine {
	/// Starts the player thread; the device is opened there and reported through
	/// [`AudioEvent::Ready`] or [`AudioEvent::Unavailable`].
	pub fn start(waker: Option<Waker>) -> Self {
		let (tx, rx) = mpsc::channel::<Request>();
		let (event_tx, events) = mpsc::channel::<AudioEvent>();
		let thread = thread::Builder::new()
			.name("cadenza-app-player".into())
			.spawn(move || run(rx, event_tx, waker))
			.ok();
		Self { tx, events, thread }
	}

	pub fn load(
		&self,
		project: Project,
		plugins: Option<Arc<PluginCatalog>>,
		hosting: PluginHosting,
		generation: u64,
	) {
		self.send(Request::Load {
			project: Box::new(project),
			plugins,
			hosting,
			generation,
		});
	}

	pub fn play(&self) {
		self.send(Request::Play);
	}

	pub fn stop(&self) {
		self.send(Request::Stop);
	}

	pub fn seek(&self, tick: Tick) {
		self.send(Request::Seek(tick));
	}

	pub fn live_volume(&self, track: Option<TrackId>, db: f32) {
		self.send(Request::Volume(track, db));
	}

	pub fn live_pan(&self, track: Option<TrackId>, pan: f32) {
		self.send(Request::Pan(track, pan));
	}

	pub fn live_param(&self, device: DeviceId, param: &str, value: f32) {
		self.send(Request::Param(device, param.to_owned(), value));
	}

	pub fn live_send(&self, send: DeviceId, db: f32) {
		self.send(Request::Send(send, db));
	}

	/// Swaps the running graph's automation for `lanes` (reloads when it cannot).
	pub fn automation(&self, lanes: Vec<AutomationLane>, tempo: TempoMap) {
		self.send(Request::Automation { lanes, tempo });
	}

	/// Plays `pitch` through `track`'s instrument, also while stopped.
	pub fn preview_on(&self, track: TrackId, pitch: u8, velocity: f32) {
		self.send(Request::Preview(track, pitch, Some(velocity)));
	}

	pub fn preview_off(&self, track: TrackId, pitch: u8) {
		self.send(Request::Preview(track, pitch, None));
	}

	/// Next pending event, if any (never blocks).
	pub fn poll(&self) -> Option<AudioEvent> {
		self.events.try_recv().ok()
	}

	fn send(&self, request: Request) {
		// A dead thread already reported why; nothing else to do here.
		let _ = self.tx.send(request);
	}
}

impl Drop for AudioEngine {
	fn drop(&mut self) {
		self.send(Request::Shutdown);
		if let Some(t) = self.thread.take() {
			let _ = t.join();
		}
	}
}

fn run(rx: Receiver<Request>, events: Sender<AudioEvent>, waker: Option<Waker>) {
	let emit = |event: AudioEvent| {
		let _ = events.send(event);
		if let Some(w) = &waker {
			w();
		}
	};
	let mut player = match Player::new() {
		Ok(p) => p,
		Err(e) => {
			emit(AudioEvent::Unavailable(e.to_string()));
			// Keep draining so senders never see a closed channel mid-session.
			for r in rx {
				if matches!(r, Request::Shutdown) {
					break;
				}
			}
			return;
		}
	};
	emit(AudioEvent::Ready {
		clock: player.clock(),
	});
	loop {
		let first = match rx.recv_timeout(CRASH_POLL) {
			Ok(r) => r,
			Err(RecvTimeoutError::Timeout) => {
				let crashes = player.take_plugin_crashes();
				if !crashes.is_empty() {
					emit(AudioEvent::PluginCrashes(crashes));
				}
				continue;
			}
			Err(RecvTimeoutError::Disconnected) => break,
		};
		let mut batch = vec![first];
		batch.extend(rx.try_iter());
		// Only the newest load matters; commands keep their order around it.
		let last_load = batch
			.iter()
			.rposition(|r| matches!(r, Request::Load { .. }));
		let last_automation = batch
			.iter()
			.rposition(|r| matches!(r, Request::Automation { .. }));
		for (i, request) in batch.into_iter().enumerate() {
			let result = match request {
				Request::Load { .. } if Some(i) != last_load => Ok(()),
				// A later load (or automation set) already carries these lanes.
				Request::Automation { .. }
					if Some(i) != last_automation || last_load.is_some_and(|l| l > i) =>
				{
					Ok(())
				}
				Request::Load {
					project,
					plugins,
					hosting,
					generation,
				} => {
					player.set_plugins(plugins);
					player.set_hosting(hosting);
					match player.load(&project) {
						Ok(warnings) => emit(AudioEvent::Loaded {
							generation,
							warnings,
							meters: player.meters(),
						}),
						Err(e) => emit(AudioEvent::LoadFailed {
							generation,
							error: e.to_string(),
						}),
					}
					Ok(())
				}
				Request::Play => player.play(),
				Request::Stop => player.stop(),
				Request::Seek(tick) => player.seek(tick),
				Request::Volume(track, db) => player.set_live_volume(track, db),
				Request::Pan(track, pan) => player.set_live_pan(track, pan),
				Request::Param(device, param, value) => {
					player.set_live_param(device, &param, value).map(|_| ())
				}
				Request::Send(send, db) => player.set_live_send(send, db).map(|_| ()),
				Request::Automation { lanes, tempo } => match player.set_automation(&lanes, &tempo)
				{
					Ok(true) => Ok(()),
					Ok(false) => {
						emit(AudioEvent::ReloadNeeded);
						Ok(())
					}
					Err(e) => Err(e),
				},
				Request::Preview(track, pitch, Some(velocity)) => {
					player.preview_note_on(track, pitch, velocity)
				}
				Request::Preview(track, pitch, None) => player.preview_note_off(track, pitch),
				Request::Shutdown => {
					let _ = player.stop();
					return;
				}
			};
			if let Err(e) = result {
				emit(AudioEvent::Error(e.to_string()));
			}
		}
	}
}
