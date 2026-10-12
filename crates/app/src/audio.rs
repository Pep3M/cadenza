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
//!
//! Plugin editors are opened and closed through the player too (they belong to the
//! loaded instances); while any is open the thread polls them every [`EDITOR_POLL`] and
//! forwards what happened as [`AudioEvent::Editor`].

use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use cadenza_core::{DeviceId, TempoMap, Tick, TrackId};
use cadenza_engine::{
	AuditionScope, EditorState, EditorUpdate, MeterBank, Player, PlayerClock, PluginCrash,
	PluginHosting,
};
use cadenza_model::{AutomationLane, Clip, Project};
use cadenza_plugin_host::PluginCatalog;

/// How often the player thread polls sandboxed plugins for crashes.
const CRASH_POLL: Duration = Duration::from_millis(500);
/// How often the player thread polls open plugin editors.
pub const EDITOR_POLL: Duration = Duration::from_millis(30);
/// How long saving waits for the live states of open plugin editors.
pub const CAPTURE_TIMEOUT: Duration = Duration::from_secs(5);

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
	/// Loop range (song ticks).
	Loop(Option<(Tick, Tick)>),
	/// Tracks heard alone (pattern mode).
	Audition(Option<Vec<TrackId>>),
	/// A track's MIDI clips changed (reschedule without reloading).
	TrackClips {
		track: TrackId,
		clips: Vec<Clip>,
		tempo: TempoMap,
	},
	OpenEditor {
		device: DeviceId,
		title: String,
	},
	CloseEditor(DeviceId),
	/// Capture live plugin states (reported as `EditorStates`, or through `reply`).
	CaptureStates {
		project: Box<Project>,
		devices: Vec<DeviceId>,
		reply: Option<SyncSender<Vec<EditorState>>>,
	},
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
	/// What happened in open plugin editors.
	Editor(Vec<EditorUpdate>),
	/// Live plugin states to store in the project (captured, or carried over by a load).
	EditorStates(Vec<EditorState>),
	/// An editor could not be opened.
	EditorFailed {
		device: DeviceId,
		error: String,
	},
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

	pub fn set_loop(&self, range: Option<(Tick, Tick)>) {
		self.send(Request::Loop(range));
	}

	pub fn audition(&self, tracks: Option<Vec<TrackId>>) {
		self.send(Request::Audition(tracks));
	}

	/// Reschedules `track`'s MIDI clips in the running graph (reloads when it cannot).
	pub fn track_clips(&self, track: TrackId, clips: Vec<Clip>, tempo: TempoMap) {
		self.send(Request::TrackClips {
			track,
			clips,
			tempo,
		});
	}

	/// Opens (or raises) the editor of plugin device `device`.
	pub fn open_editor(&self, device: DeviceId, title: String) {
		self.send(Request::OpenEditor { device, title });
	}

	pub fn close_editor(&self, device: DeviceId) {
		self.send(Request::CloseEditor(device));
	}

	/// Captures the live state of `devices` (and of every editor that reported a state
	/// change); results come back as [`AudioEvent::EditorStates`].
	pub fn capture_states(&self, project: Project, devices: Vec<DeviceId>) {
		self.send(Request::CaptureStates {
			project: Box::new(project),
			devices,
			reply: None,
		});
	}

	/// Like [`AudioEngine::capture_states`], waiting (at most `timeout`) for the result:
	/// used right before saving.
	pub fn capture_states_now(
		&self,
		project: Project,
		devices: Vec<DeviceId>,
		timeout: Duration,
	) -> Vec<EditorState> {
		let (tx, rx) = mpsc::sync_channel(1);
		self.send(Request::CaptureStates {
			project: Box::new(project),
			devices,
			reply: Some(tx),
		});
		rx.recv_timeout(timeout).unwrap_or_default()
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
				match r {
					Request::Shutdown => break,
					Request::OpenEditor { device, .. } => emit(AudioEvent::EditorFailed {
						device,
						error: "plugin editors need the audio engine, and there is no audio output"
							.into(),
					}),
					Request::CaptureStates {
						reply: Some(tx), ..
					} => {
						let _ = tx.send(Vec::new());
					}
					_ => {}
				}
			}
			return;
		}
	};
	emit(AudioEvent::Ready {
		clock: player.clock(),
	});
	let mut next_crash_poll = Instant::now() + CRASH_POLL;
	let mut next_editor_poll = Instant::now() + EDITOR_POLL;
	loop {
		let now = Instant::now();
		if now >= next_crash_poll {
			next_crash_poll = now + CRASH_POLL;
			let crashes = player.take_plugin_crashes();
			if !crashes.is_empty() {
				emit(AudioEvent::PluginCrashes(crashes));
			}
		}
		let editors = !player.open_editors().is_empty();
		if editors && now >= next_editor_poll {
			next_editor_poll = now + EDITOR_POLL;
			let updates = player.poll_editors();
			if !updates.is_empty() {
				emit(AudioEvent::Editor(updates));
			}
		}
		let next = if editors {
			next_crash_poll.min(next_editor_poll)
		} else {
			next_crash_poll
		};
		let first = match rx.recv_timeout(next.saturating_duration_since(Instant::now())) {
			Ok(r) => r,
			Err(RecvTimeoutError::Timeout) => continue,
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
					// Editor states carried over to the new instances, and editors that
					// lost their device.
					let states = player.take_editor_states();
					if !states.is_empty() {
						emit(AudioEvent::EditorStates(states));
					}
					let updates = player.poll_editors();
					if !updates.is_empty() {
						emit(AudioEvent::Editor(updates));
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
				Request::Loop(range) => player.set_loop(range),
				Request::Audition(tracks) => {
					player.set_audition(tracks.map(|tracks| AuditionScope { tracks }));
					Ok(())
				}
				// A later load already carries these clips.
				Request::TrackClips { .. } if last_load.is_some_and(|l| l > i) => Ok(()),
				Request::TrackClips {
					track,
					clips,
					tempo,
				} => match player.set_track_clips(track, &clips, &tempo) {
					Ok(true) => Ok(()),
					Ok(false) => {
						emit(AudioEvent::ReloadNeeded);
						Ok(())
					}
					Err(e) => Err(e),
				},
				Request::OpenEditor { device, title } => {
					if let Err(e) = player.open_editor(device, &title) {
						emit(AudioEvent::EditorFailed {
							device,
							error: e.to_string(),
						});
					}
					next_editor_poll = Instant::now() + EDITOR_POLL;
					Ok(())
				}
				Request::CloseEditor(device) => player.close_editor(device),
				Request::CaptureStates {
					project,
					devices,
					reply,
				} => {
					player.capture_editor_states(&project, &devices);
					let states = player.take_editor_states();
					match reply {
						Some(tx) => {
							let _ = tx.send(states);
						}
						None if !states.is_empty() => emit(AudioEvent::EditorStates(states)),
						None => {}
					}
					Ok(())
				}
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
