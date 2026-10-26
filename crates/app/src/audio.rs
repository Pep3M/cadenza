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
//!
//! Recording: the thread owns the devices (audio settings reopen the player with the new
//! [`AudioConfig`], keeping arm states, metronome and MIDI input), starts recordings
//! ([`AudioEvent::RecordingStarted`] hands the UI the live [`RecordingMonitor`]) and stops
//! them ([`AudioEvent::Recorded`] carries the files and passes; the UI turns them into
//! one undoable batch). Device lists are probed here too, never on the UI thread.
//!
//! Device probe: [`AudioEngine::probe`] picks the device whose editor shows live data;
//! the thread answers with [`AudioEvent::Probe`] carrying the player's [`DeviceProbe`]
//! (again after the devices reopen, since a new player has a new ring).

use crate::i18n::{tr, trf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use cadenza_core::{DeviceId, TempoMap, Tick, TrackId};
use cadenza_engine::{
	AudioConfig, AudioDevices, AuditionScope, DeviceProbe, EditorState, EditorUpdate, InputInfo,
	InputStats, Listen, MeterBank, MetronomeSettings, MidiMapMonitor, MonitorFx, Player,
	PlayerClock, PluginCrash, PluginHosting, PluginXruns, RecordSetup, Recording, RecordingMonitor,
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
	/// New MIDI controller assignments (the project's `midi_map`).
	MidiMap(Box<Project>),
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
	/// Reopen the devices with this configuration.
	Configure(AudioConfig),
	/// MIDI input port by name (`None`: closed).
	MidiInput(Option<String>),
	/// The instrument track live MIDI plays through.
	Monitor(Option<TrackId>),
	Arm(TrackId, bool),
	Metronome(MetronomeSettings),
	/// Recording latency offset, frames.
	LatencyOffset(i64),
	TestTone(bool),
	/// Play a snippet once over the output (note audition): channels, rate, gain dB.
	Snippet(Vec<Vec<f32>>, u32, f32),
	StopSnippet,
	/// The device the probe follows (`None`: off).
	Probe(Option<DeviceId>),
	/// Monitoring switches on the output (mono, dim).
	MonitorFx(MonitorFx),
	/// The device playing a listen mode (compressor key, EQ band solo; `None`: off).
	Listen(Option<(DeviceId, Listen)>),
	/// List audio devices and MIDI inputs (reported as `Devices`).
	ListDevices,
	StartRecording(Box<RecordSetup>),
	/// Stops capturing; `keep_playing` leaves the transport running (record toggled off
	/// while playing).
	StopRecording {
		keep_playing: bool,
	},
	Shutdown,
}

/// Where the audio runs, as the settings dialog shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct DeviceStatus {
	pub output: String,
	pub sample_rate: u32,
	pub input: Option<InputInfo>,
	/// Why the input (or the MIDI input) could not be opened.
	pub input_error: Option<String>,
	pub midi_input: Option<String>,
	pub midi_error: Option<String>,
	/// Measured output latency, frames.
	pub output_latency: u64,
	/// Latency compensation the next recording would use, frames.
	pub recording_latency: i64,
	/// How the output callback thread is scheduled (e.g. `SCHED_FIFO 10 (RealtimeKit)`).
	pub audio_thread: Option<String>,
}

/// What the player thread reports back.
pub enum AudioEvent {
	Ready {
		clock: PlayerClock,
		input: Arc<InputStats>,
		/// The master's post-fader output (live loudness on the master strip).
		master: Arc<DeviceProbe>,
		/// What the mapped MIDI controllers did (MIDI learn).
		midi_map: Arc<MidiMapMonitor>,
		status: DeviceStatus,
	},
	/// Audio devices and MIDI input ports.
	Devices {
		audio: AudioDevices,
		midi: Vec<String>,
	},
	RecordingStarted(Arc<RecordingMonitor>),
	Recorded(Box<Recording>),
	RecordFailed(String),
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
	/// Dropout totals, sent when they change: device under/overruns since the device
	/// opened, and the sandboxed plugins of the loaded graph that were late.
	Xruns {
		device: u64,
		plugins: Vec<PluginXruns>,
	},
	/// The output thread's scheduling became known (after the device opened).
	AudioThread(String),
	/// The probe of the current player (`None` while no device is probed).
	Probe(Option<Arc<DeviceProbe>>),
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
		Self::start_with(AudioConfig::default(), None, waker)
	}

	/// Starts the player thread on `config`'s devices, with `midi` as the MIDI input.
	pub fn start_with(config: AudioConfig, midi: Option<String>, waker: Option<Waker>) -> Self {
		let (tx, rx) = mpsc::channel::<Request>();
		let (event_tx, events) = mpsc::channel::<AudioEvent>();
		let thread = thread::Builder::new()
			.name("cadenza-app-player".into())
			.spawn(move || run(rx, event_tx, waker, config, midi))
			.ok();
		Self { tx, events, thread }
	}

	/// Reopens the devices (the project is reloaded once the player is ready again).
	pub fn configure(&self, config: AudioConfig) {
		self.send(Request::Configure(config));
	}

	pub fn midi_input(&self, port: Option<String>) {
		self.send(Request::MidiInput(port));
	}

	pub fn monitor(&self, track: Option<TrackId>) {
		self.send(Request::Monitor(track));
	}

	pub fn arm(&self, track: TrackId, armed: bool) {
		self.send(Request::Arm(track, armed));
	}

	pub fn metronome(&self, settings: MetronomeSettings) {
		self.send(Request::Metronome(settings));
	}

	pub fn latency_offset(&self, frames: i64) {
		self.send(Request::LatencyOffset(frames));
	}

	pub fn test_tone(&self, on: bool) {
		self.send(Request::TestTone(on));
	}

	/// Plays `channels` (at `rate`) once over the output, replacing the snippet playing:
	/// the note editor's audition of a dragged note.
	pub fn play_snippet(&self, channels: Vec<Vec<f32>>, rate: u32, gain_db: f32) {
		self.send(Request::Snippet(channels, rate, gain_db));
	}

	pub fn stop_snippet(&self) {
		self.send(Request::StopSnippet);
	}

	/// Follows `device` with the probe (answered by [`AudioEvent::Probe`]).
	pub fn probe(&self, device: Option<DeviceId>) {
		self.send(Request::Probe(device));
	}

	/// Mono / dim on the output (monitoring only, never rendered).
	pub fn monitor_fx(&self, fx: MonitorFx) {
		self.send(Request::MonitorFx(fx));
	}

	/// Listen mode of one device (monitoring only, never rendered; `None`: off).
	pub fn listen(&self, target: Option<(DeviceId, Listen)>) {
		self.send(Request::Listen(target));
	}

	pub fn list_devices(&self) {
		self.send(Request::ListDevices);
	}

	pub fn start_recording(&self, setup: RecordSetup) {
		self.send(Request::StartRecording(Box::new(setup)));
	}

	pub fn stop_recording(&self, keep_playing: bool) {
		self.send(Request::StopRecording { keep_playing });
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

	/// Hands the project's MIDI controller assignments to the player (no reload).
	pub fn midi_map(&self, project: Project) {
		self.send(Request::MidiMap(Box::new(project)));
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

/// What the thread re-applies after reopening the devices.
#[derive(Default)]
struct Kept {
	midi: Option<String>,
	midi_error: Option<String>,
	monitor: Option<TrackId>,
	armed: Vec<TrackId>,
	metronome: Option<MetronomeSettings>,
	latency_offset: i64,
	probe: Option<DeviceId>,
	monitor_fx: MonitorFx,
	listen: Option<(DeviceId, Listen)>,
}

/// Points the player's listen mode at `target` (or switches it off).
fn apply_listen(
	player: &mut Player,
	target: Option<(DeviceId, Listen)>,
) -> Result<(), cadenza_engine::EngineError> {
	match (target, player.listen()) {
		(Some((device, mode)), _) => player.set_listen(device, mode),
		(None, Some((device, _))) => player.set_listen(device, Listen::Off),
		(None, None) => Ok(()),
	}
}

fn status(player: &Player, kept: &Kept) -> DeviceStatus {
	DeviceStatus {
		output: player.output_device().to_owned(),
		sample_rate: player.sample_rate(),
		input: player.input_info().cloned(),
		input_error: player.input_error().map(str::to_owned),
		midi_input: player.midi_input().map(str::to_owned),
		midi_error: kept.midi_error.clone(),
		output_latency: player.output_latency(),
		recording_latency: player.recording_latency(),
		audio_thread: player.audio_scheduling(),
	}
}

/// Opens `config`'s devices and re-applies what `kept` remembers.
fn open(config: &AudioConfig, kept: &mut Kept) -> Result<Player, String> {
	let mut player = Player::open(config).map_err(|e| e.to_string())?;
	open_midi(&mut player, kept);
	let _ = player.set_monitor_track(kept.monitor);
	for &t in &kept.armed {
		let _ = player.set_armed(t, true);
	}
	if let Some(m) = kept.metronome {
		let _ = player.set_metronome(m);
	}
	player.set_latency_offset(kept.latency_offset);
	let _ = player.set_probe(kept.probe);
	let _ = player.set_monitor(kept.monitor_fx);
	let _ = apply_listen(&mut player, kept.listen);
	Ok(player)
}

fn open_midi(player: &mut Player, kept: &mut Kept) {
	kept.midi_error = None;
	let result = match &kept.midi {
		None => player.set_midi_input(None),
		Some(name) => cadenza_engine::midi_input_ports().and_then(|ports| {
			let port = ports.iter().find(|p| &p.name == name).ok_or_else(|| {
				cadenza_engine::EngineError::Midi(trf(
					"MIDI input '{name}' is not connected; pick another one in the audio settings",
					&[("name", &name)],
				))
			})?;
			player.set_midi_input(Some(port))
		}),
	};
	if let Err(e) = result {
		kept.midi_error = Some(e.to_string());
	}
}

fn run(
	rx: Receiver<Request>,
	events: Sender<AudioEvent>,
	waker: Option<Waker>,
	config: AudioConfig,
	midi: Option<String>,
) {
	let emit = |event: AudioEvent| {
		let _ = events.send(event);
		if let Some(w) = &waker {
			w();
		}
	};
	let mut kept = Kept {
		midi,
		..Kept::default()
	};
	let opened = open(&config, &mut kept).or_else(|e| {
		// The chosen devices are gone: fall back to the defaults (output only).
		if config == AudioConfig::default() {
			Err(e)
		} else {
			open(&AudioConfig::default(), &mut kept).map_err(|_| e)
		}
	});
	let mut opened = opened;
	let mut player = loop {
		match opened {
			Ok(p) => break p,
			Err(e) => {
				emit(AudioEvent::Unavailable(e));
				// Keep draining so senders never see a closed channel mid-session; new
				// audio settings try again.
				opened = Err(String::new());
				for r in rx.iter() {
					match r {
						Request::Shutdown => return,
						Request::Configure(config) => {
							opened = open(&config, &mut kept);
							break;
						}
						Request::ListDevices => emit(AudioEvent::Devices {
							audio: cadenza_engine::audio_devices(),
							midi: cadenza_engine::midi_input_ports()
								.map(|ports| ports.into_iter().map(|p| p.name).collect())
								.unwrap_or_default(),
						}),
						Request::StartRecording(_) => emit(AudioEvent::RecordFailed(
							tr("there is no audio device; choose one in the audio settings").into(),
						)),
						Request::OpenEditor { device, .. } => emit(AudioEvent::EditorFailed {
							device,
							error: tr(
								"plugin editors need the audio engine, and there is no audio output",
							)
							.into(),
						}),
						Request::CaptureStates {
							reply: Some(tx), ..
						} => {
							let _ = tx.send(Vec::new());
						}
						Request::Probe(device) => kept.probe = device,
						Request::MonitorFx(fx) => kept.monitor_fx = fx,
						Request::Listen(target) => kept.listen = target,
						_ => {}
					}
				}
				if matches!(&opened, Err(e) if e.is_empty()) {
					return;
				}
			}
		}
	};
	emit(AudioEvent::Ready {
		clock: player.clock(),
		input: player.input_stats(),
		master: player.master_tap(),
		midi_map: player.midi_map_monitor(),
		status: status(&player, &kept),
	});
	if kept.probe.is_some() {
		emit(AudioEvent::Probe(player.probe()));
	}
	let mut next_crash_poll = Instant::now() + CRASH_POLL;
	let mut last_xruns: (u64, Vec<PluginXruns>) = (0, Vec::new());
	let mut scheduling_sent = player.audio_scheduling().is_some();
	let mut next_editor_poll = Instant::now() + EDITOR_POLL;
	loop {
		let now = Instant::now();
		if now >= next_crash_poll {
			next_crash_poll = now + CRASH_POLL;
			let crashes = player.take_plugin_crashes();
			if !crashes.is_empty() {
				emit(AudioEvent::PluginCrashes(crashes));
			}
			if !scheduling_sent && let Some(s) = player.audio_scheduling() {
				scheduling_sent = true;
				emit(AudioEvent::AudioThread(s));
			}
			let xruns = (player.device_xruns(), player.plugin_xruns());
			if xruns != last_xruns {
				emit(AudioEvent::Xruns {
					device: xruns.0,
					plugins: xruns.1.clone(),
				});
				last_xruns = xruns;
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
				Request::MidiMap(project) => player.set_midi_map(&project),
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
				Request::Configure(config) => {
					if player.is_recording() {
						emit(AudioEvent::Error(
							tr("stop recording before changing the audio settings").into(),
						));
						continue;
					}
					// The old stream must close before the device can be reopened.
					drop(player);
					player = match open(&config, &mut kept) {
						Ok(p) => p,
						Err(e) => {
							emit(AudioEvent::Error(trf(
								"{e}; back to the default output",
								&[("e", &e)],
							)));
							match open(&AudioConfig::default(), &mut kept) {
								Ok(p) => p,
								Err(e) => {
									emit(AudioEvent::Unavailable(e));
									return;
								}
							}
						}
					};
					scheduling_sent = player.audio_scheduling().is_some();
					emit(AudioEvent::Ready {
						clock: player.clock(),
						input: player.input_stats(),
						master: player.master_tap(),
						midi_map: player.midi_map_monitor(),
						status: status(&player, &kept),
					});
					if kept.probe.is_some() {
						emit(AudioEvent::Probe(player.probe()));
					}
					Ok(())
				}
				Request::MidiInput(port) => {
					kept.midi = port;
					open_midi(&mut player, &mut kept);
					emit(AudioEvent::Ready {
						clock: player.clock(),
						input: player.input_stats(),
						master: player.master_tap(),
						midi_map: player.midi_map_monitor(),
						status: status(&player, &kept),
					});
					Ok(())
				}
				Request::Monitor(track) => {
					kept.monitor = track;
					player.set_monitor_track(track)
				}
				Request::Arm(track, on) => {
					kept.armed.retain(|t| *t != track);
					if on {
						kept.armed.push(track);
					}
					player.set_armed(track, on)
				}
				Request::Metronome(m) => {
					kept.metronome = Some(m);
					player.set_metronome(m)
				}
				Request::LatencyOffset(frames) => {
					kept.latency_offset = frames;
					player.set_latency_offset(frames);
					Ok(())
				}
				Request::TestTone(on) => player.set_test_tone(on),
				Request::Snippet(channels, rate, gain_db) => {
					player.play_snippet(channels, rate, gain_db)
				}
				Request::StopSnippet => player.stop_snippet(),
				Request::MonitorFx(fx) => {
					kept.monitor_fx = fx;
					player.set_monitor(fx)
				}
				Request::Listen(target) => {
					kept.listen = target;
					apply_listen(&mut player, target)
				}
				Request::Probe(device) => {
					kept.probe = device;
					let result = player.set_probe(device);
					emit(AudioEvent::Probe(player.probe()));
					result
				}
				Request::ListDevices => {
					let audio = cadenza_engine::audio_devices();
					let midi = cadenza_engine::midi_input_ports()
						.map(|ports| ports.into_iter().map(|p| p.name).collect())
						.unwrap_or_default();
					emit(AudioEvent::Devices { audio, midi });
					emit(AudioEvent::Ready {
						clock: player.clock(),
						input: player.input_stats(),
						master: player.master_tap(),
						midi_map: player.midi_map_monitor(),
						status: status(&player, &kept),
					});
					Ok(())
				}
				Request::StartRecording(setup) => {
					match player.start_recording(&setup) {
						Ok(monitor) => emit(AudioEvent::RecordingStarted(monitor)),
						Err(e) => emit(AudioEvent::RecordFailed(e.to_string())),
					}
					Ok(())
				}
				Request::StopRecording { keep_playing } => {
					if player.is_recording() {
						let rec = if keep_playing {
							player.end_recording()
						} else {
							player.stop_recording()
						};
						match rec {
							Ok(rec) => emit(AudioEvent::Recorded(Box::new(rec))),
							Err(e) => emit(AudioEvent::RecordFailed(e.to_string())),
						}
					}
					Ok(())
				}
				Request::Shutdown => {
					if player.is_recording() {
						let _ = player.stop_recording();
					}
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
