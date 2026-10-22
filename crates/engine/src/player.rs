//! Realtime playback through the default cpal output device.
//!
//! Threading: [`Player::load`] compiles the project on the calling thread and hands the
//! boxed [`Graph`] to the audio callback through a wait-free `rtrb` ring buffer. The
//! callback swaps it in and sends the previous graph back through a second ring buffer, so
//! graphs are always dropped on the control thread (never on the audio thread). The callback
//! only swaps when the return ring has room, so it never has to drop anything itself.
//! Transport commands use a third ring buffer; position and play state are atomics.
//!
//! The callback runs the exact same [`Graph::process`] used by offline renders and does not
//! allocate.
//!
//! Live MIDI input ([`Player::set_midi_input`], [`Player::set_monitor_track`]): the `midir`
//! callback pushes timestamped messages into an `rtrb` queue whose consumer lives on the
//! audio thread (handed over through another ring, and handed back to be dropped on the
//! control side). Each callback drains it into a pre-allocated buffer and feeds note
//! events to the monitored track's instrument at the start of the block, so input latency
//! is at most one output buffer. Monitoring sounds while the transport runs and, as a
//! preview of the monitored instrument only, while it is stopped.
//!
//! Note preview ([`Player::preview_note_on`] / [`Player::preview_note_off`]): editors
//! play single notes through a track's instrument the same way, also while stopped (the
//! previewed track becomes the monitored one).
//!
//! Plugin devices are sandboxed by default (see [`crate::hosting`]): a crashing or hanging
//! plugin is bypassed on the audio thread by its sandbox proxy, and the control side
//! notices it through the sandbox monitors ([`Player::plugin_crashes`],
//! [`Player::take_plugin_crashes`]). Crashed plugins are restarted by the next
//! [`Player::load`], which re-creates every device from the project.
//!
//! Audio input and recording ([`Player::set_audio_input`], [`Player::start_recording`]):
//! the input stream feeds the output callback through a lock-free ring (see
//! [`crate::input`]); each callback reads one input frame per output frame, lends them to
//! the graph for input monitoring and, while recording, hands them with the timeline
//! position they were heard at to the recording tap (see [`crate::record`]). Arming,
//! the metronome and the count-in are plain commands; the tap and input readers are
//! handed over boxed through their own rings and come back to be dropped here.
//! [`VirtualDevice`] drives the very same callback without hardware (tests).
//!
//! Plugin editors ([`Player::open_editor`]) show the GUI of a plugin device of the loaded
//! project, in the process that runs it. [`Player::poll_editors`] returns what the user
//! did there (parameter gestures and values, state changes, closed windows); the caller
//! turns them into project commands. Editors stay open across [`Player::load`]: the new
//! instance's editor replaces the old one, and plugin state changed in an editor (e.g. a
//! patch loaded from the plugin's own browser) is carried over to the new instance and
//! reported by [`Player::take_editor_states`] so it can be stored in the project.

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicI64, AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use std::collections::HashSet;

use std::collections::HashMap;

use crate::midi_map::{MapTarget, MidiMapMonitor, MidiMapTable};
use cadenza_core::{DeviceId, TempoMap, Tick, TrackId};
use cadenza_dsp::{Listen, MidiEvent};
use cadenza_midi_io::{MappedEvent, MidiInputConnection, PortInfo, TimedMessage};
use cadenza_model::{AutomationLane, Clip, Command, Document, Project};
use cadenza_plugin_host::{EditorEvent, PluginCatalog};
use cpal::traits::{DeviceTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};
use rtrb::{Consumer, Producer, RingBuffer};

use crate::automation::{AutoTarget, AutomationSet, Resolver};
use crate::compile_hosted;
use crate::error::EngineError;
use crate::graph::{Audition, Graph, ScheduleSwap};
use crate::hosting::{PluginCrash, PluginEditorSlot, PluginHosting, PluginWatch, PluginXruns};
use crate::input::{InputFifo, InputInfo, InputMode, InputReader, InputStats, MAX_INPUT_CHANNELS};
use crate::meter::MeterBank;
use crate::metronome::MetronomeSettings;
use crate::probe::DeviceProbe;
use crate::record::{RecordSession, RecordSetup, RecordTap, Recording, RecordingMonitor};
use crate::schedule::MidiSchedule;

const GRAPH_SLOTS: usize = 4;
const GARBAGE_SLOTS: usize = 8;
const COMMAND_SLOTS: usize = 256;
/// Automation sets in flight (and coming back to be dropped).
const AUTOMATION_SLOTS: usize = 4;
const AUTOMATION_GARBAGE_SLOTS: usize = 8;
/// Rescheduled tracks in flight (and coming back to be dropped).
const SCHEDULE_SLOTS: usize = 16;
const SCHEDULE_GARBAGE_SLOTS: usize = 32;
const MIDI_INPUT_SLOTS: usize = 2;
/// Capacity of the hardware MIDI queue, and of the per-block drain buffer (so draining
/// can never outgrow it).
const MIDI_QUEUE: usize = 512;
/// Block start passed to `drain_into_block`: later than any `midir` timestamp, so every
/// queued message is due now (offset 0). `midir` and cpal share no clock, so finer
/// placement is not possible yet.
const DRAIN_ALL: u64 = u64::MAX / 2;
/// Frames rendered per inner step of the callback (scratch buffer size).
const SCRATCH_FRAMES: usize = 1024;
/// Audio input readers / recording taps in flight (and coming back to be dropped).
const HANDOVER_SLOTS: usize = 2;
/// Tracks that can be armed at once.
const ARM_SLOTS: usize = 256;
/// Level of the settings dialog's test tone (dBFS).
const TEST_TONE_DB: f32 = -20.0;

/// Output (and optional input) device configuration of a [`Player`].
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct AudioConfig {
	/// Output device name (`None`: the system default).
	pub output: Option<String>,
	/// Open an audio input (for recording and input monitoring).
	pub input_enabled: bool,
	/// Input device name (`None`: the system default input).
	pub input: Option<String>,
	/// Sample rate to ask the output for (`None`: the device default).
	pub sample_rate: Option<u32>,
	/// Buffer size in frames (`None`: the device default).
	pub buffer_frames: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Cmd {
	Play,
	Stop,
	Seek(Tick),
	Monitor(Option<TrackId>),
	DetachMidi,
	/// Live fader move: `track: None` = master.
	Volume(Option<TrackId>, f32),
	Pan(Option<TrackId>, f32),
	/// Live parameter change: device, parameter index, plain value.
	Param(DeviceId, u32, f32),
	/// Live send level (dB).
	SendLevel(DeviceId, f32),
	/// A note for `track`'s instrument (it becomes the monitored track).
	Preview(TrackId, MidiEvent),
	/// Loop range (song ticks), kept across loads.
	Loop(Option<(Tick, Tick)>),
	/// Arm / disarm a track for recording (kept across loads).
	Arm(TrackId, bool),
	DisarmAll,
	Metronome(MetronomeSettings),
	/// Take the reader waiting in the input ring.
	AttachInput,
	DetachInput,
	/// Take the recording tap waiting in its ring.
	StartRecord,
	/// Hand the recording tap back.
	EndRecord,
	/// Play after a count-in of this many bars.
	PlayCountIn(u32),
	TestTone(bool),
	/// Device followed by the probe (kept across loads).
	Probe(Option<DeviceId>),
	/// Monitoring switches on the device output (kept across loads).
	MonitorFx(MonitorFx),
	/// Device playing a listen mode (kept across loads).
	Listen(Option<(DeviceId, Listen)>),
}

/// Monitoring-only switches applied to what the output device plays, after the master
/// meters and the master tap (they show the real mix) and after the metronome: not part of
/// the project, never in offline renders or exports (see [`Player::set_monitor`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MonitorFx {
	/// Sum left and right to mono (both channels play `(l + r) / 2`).
	pub mono: bool,
	/// Turn the output down by [`DIM_DB`].
	pub dim: bool,
}

/// Level change of [`MonitorFx::dim`], dB.
pub const DIM_DB: f32 = -20.0;

impl MonitorFx {
	pub fn is_off(self) -> bool {
		!self.mono && !self.dim
	}

	/// Applies the switches to one stereo block in place. Realtime safe.
	pub fn apply(self, left: &mut [f32], right: &mut [f32]) {
		if self.is_off() {
			return;
		}
		let gain = if self.dim {
			cadenza_core::db_to_gain(DIM_DB)
		} else {
			1.0
		};
		for (l, r) in left.iter_mut().zip(right.iter_mut()) {
			if self.mono {
				let m = 0.5 * (*l + *r);
				*l = m;
				*r = m;
			}
			*l *= gain;
			*r *= gain;
		}
	}
}

/// Something that happened in the editor of a plugin device (see [`Player::poll_editors`]).
#[derive(Debug, Clone, PartialEq)]
pub struct EditorUpdate {
	pub device: DeviceId,
	pub event: EditorEvent,
}

/// The live state of a plugin device whose editor changed it (base64, for
/// `Command::SetDeviceState`; see [`Player::take_editor_states`]).
#[derive(Debug, Clone, PartialEq)]
pub struct EditorState {
	pub track: Option<TrackId>,
	pub device: DeviceId,
	pub state: String,
}

/// The tracks an audition plays (see [`Player::set_audition`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AuditionScope {
	pub tracks: Vec<TrackId>,
}

#[derive(Debug, Default)]
struct Shared {
	position: AtomicI64,
	playing: AtomicBool,
	/// Buffer under/overruns the audio device reported (cpal `Xrun` errors).
	device_xruns: AtomicU64,
	/// Kernel id of the output callback thread, published by its first callback (0 =
	/// not yet) so a helper thread can raise its priority.
	audio_thread: AtomicI32,
	/// The running count-in: beats heard (f64 bits), length in beats, beats per bar
	/// (length 0 = none).
	count_in_heard: AtomicU64,
	count_in_beats: AtomicU32,
	count_in_per_bar: AtomicU32,
}

/// Audio-thread side: owns the current graph.
struct AudioCore {
	graph: Option<Box<Graph>>,
	graphs: Consumer<Box<Graph>>,
	garbage: Producer<Box<Graph>>,
	commands: Consumer<Cmd>,
	automations: Consumer<Box<AutomationSet>>,
	automation_garbage: Producer<Box<AutomationSet>>,
	/// A set for a graph that is queued but not swapped in yet.
	pending_automation: Option<Box<AutomationSet>>,
	schedules: Consumer<Box<ScheduleSwap>>,
	schedule_garbage: Producer<Box<ScheduleSwap>>,
	/// A schedule for a graph that is queued but not swapped in yet.
	pending_schedule: Option<Box<ScheduleSwap>>,
	shared: Arc<Shared>,
	/// The callback thread's id was published to `shared`.
	thread_published: bool,
	playing: bool,
	position: Tick,
	loop_range: Option<(Tick, Tick)>,
	probe_target: Option<DeviceId>,
	listen: Option<(DeviceId, Listen)>,
	scratch_l: Vec<f32>,
	scratch_r: Vec<f32>,
	/// Live MIDI input queue, if a port is attached.
	midi: Option<Consumer<TimedMessage>>,
	midi_inputs: Consumer<Consumer<TimedMessage>>,
	midi_garbage: Producer<Consumer<TimedMessage>>,
	mapped: Vec<MappedEvent>,
	/// Hardware controller assignments (kept across graph swaps).
	midi_map: Option<Box<MidiMapTable>>,
	midi_maps: Consumer<Box<MidiMapTable>>,
	midi_map_garbage: Producer<Box<MidiMapTable>>,
	midi_monitor: Arc<MidiMapMonitor>,
	monitor: Option<TrackId>,
	/// Previewed pitches held down (bit per pitch) and the track they play on: live
	/// events go to it until they are released, then back to `monitor`.
	preview_held: [u64; 2],
	preview_track: Option<TrackId>,
	sample_rate: f64,
	input: Option<Box<InputReader>>,
	inputs: Consumer<Box<InputReader>>,
	input_garbage: Producer<Box<InputReader>>,
	fifo: InputFifo,
	/// The block's input frames, one buffer per channel.
	in_frames: Vec<Vec<f32>>,
	armed: Vec<TrackId>,
	metronome: MetronomeSettings,
	tap: Option<Box<RecordTap>>,
	taps: Consumer<Box<RecordTap>>,
	tap_garbage: Producer<Box<RecordTap>>,
	/// Test tone phase (radians) while it plays.
	test_tone: Option<f32>,
	monitor_fx: MonitorFx,
	/// When the first frame of the current block will be heard (backend clock).
	playback_ns: Option<u64>,
}

/// Control side: everything except the cpal stream, so it can be tested without a device.
struct Control {
	graphs: Producer<Box<Graph>>,
	garbage: Consumer<Box<Graph>>,
	commands: Producer<Cmd>,
	automations: Producer<Box<AutomationSet>>,
	automation_garbage: Consumer<Box<AutomationSet>>,
	schedules: Producer<Box<ScheduleSwap>>,
	schedule_garbage: Consumer<Box<ScheduleSwap>>,
	shared: Arc<Shared>,
	midi_inputs: Producer<Consumer<TimedMessage>>,
	midi_garbage: Consumer<Consumer<TimedMessage>>,
	midi_maps: Producer<Box<MidiMapTable>>,
	midi_map_garbage: Consumer<Box<MidiMapTable>>,
	midi_monitor: Arc<MidiMapMonitor>,
	inputs: Producer<Box<InputReader>>,
	input_garbage: Consumer<Box<InputReader>>,
	taps: Producer<Box<RecordTap>>,
	tap_garbage: Consumer<Box<RecordTap>>,
}

fn channel_pair(sample_rate: u32) -> (Control, AudioCore) {
	let (graph_tx, graph_rx) = RingBuffer::new(GRAPH_SLOTS);
	let (garbage_tx, garbage_rx) = RingBuffer::new(GARBAGE_SLOTS);
	let (cmd_tx, cmd_rx) = RingBuffer::new(COMMAND_SLOTS);
	let (auto_tx, auto_rx) = RingBuffer::new(AUTOMATION_SLOTS);
	let (auto_garbage_tx, auto_garbage_rx) = RingBuffer::new(AUTOMATION_GARBAGE_SLOTS);
	let (sched_tx, sched_rx) = RingBuffer::new(SCHEDULE_SLOTS);
	let (sched_garbage_tx, sched_garbage_rx) = RingBuffer::new(SCHEDULE_GARBAGE_SLOTS);
	let (midi_tx, midi_rx) = RingBuffer::new(MIDI_INPUT_SLOTS);
	let (midi_garbage_tx, midi_garbage_rx) = RingBuffer::new(MIDI_INPUT_SLOTS + 1);
	let (map_tx, map_rx) = RingBuffer::new(HANDOVER_SLOTS);
	let (map_garbage_tx, map_garbage_rx) = RingBuffer::new(HANDOVER_SLOTS + 1);
	let midi_monitor = Arc::new(MidiMapMonitor::default());
	let (input_tx, input_rx) = RingBuffer::new(HANDOVER_SLOTS);
	let (input_garbage_tx, input_garbage_rx) = RingBuffer::new(HANDOVER_SLOTS + 1);
	let (tap_tx, tap_rx) = RingBuffer::new(HANDOVER_SLOTS);
	let (tap_garbage_tx, tap_garbage_rx) = RingBuffer::new(HANDOVER_SLOTS + 1);
	let shared = Arc::new(Shared::default());
	(
		Control {
			graphs: graph_tx,
			garbage: garbage_rx,
			commands: cmd_tx,
			automations: auto_tx,
			automation_garbage: auto_garbage_rx,
			schedules: sched_tx,
			schedule_garbage: sched_garbage_rx,
			shared: shared.clone(),
			midi_inputs: midi_tx,
			midi_garbage: midi_garbage_rx,
			midi_maps: map_tx,
			midi_map_garbage: map_garbage_rx,
			midi_monitor: midi_monitor.clone(),
			inputs: input_tx,
			input_garbage: input_garbage_rx,
			taps: tap_tx,
			tap_garbage: tap_garbage_rx,
		},
		AudioCore {
			graph: None,
			graphs: graph_rx,
			garbage: garbage_tx,
			commands: cmd_rx,
			automations: auto_rx,
			automation_garbage: auto_garbage_tx,
			pending_automation: None,
			schedules: sched_rx,
			schedule_garbage: sched_garbage_tx,
			pending_schedule: None,
			shared,
			thread_published: false,
			playing: false,
			position: Tick::ZERO,
			loop_range: None,
			probe_target: None,
			listen: None,
			scratch_l: vec![0.0; SCRATCH_FRAMES],
			scratch_r: vec![0.0; SCRATCH_FRAMES],
			midi: None,
			midi_inputs: midi_rx,
			midi_garbage: midi_garbage_tx,
			mapped: Vec::with_capacity(MIDI_QUEUE),
			midi_map: None,
			midi_maps: map_rx,
			midi_map_garbage: map_garbage_tx,
			midi_monitor,
			monitor: None,
			preview_held: [0; 2],
			preview_track: None,
			sample_rate: f64::from(sample_rate),
			input: None,
			inputs: input_rx,
			input_garbage: input_garbage_tx,
			fifo: InputFifo::new(),
			in_frames: (0..MAX_INPUT_CHANNELS)
				.map(|_| vec![0.0; SCRATCH_FRAMES])
				.collect(),
			armed: Vec::with_capacity(ARM_SLOTS),
			metronome: MetronomeSettings::default(),
			tap: None,
			taps: tap_rx,
			tap_garbage: tap_garbage_tx,
			test_tone: None,
			monitor_fx: MonitorFx::default(),
			playback_ns: None,
		},
	)
}

impl AudioCore {
	fn begin_block(&mut self) {
		// Only take a new graph when the old one can be handed back.
		if self.garbage.slots() > 0
			&& let Ok(mut next) = self.graphs.pop()
		{
			next.set_loop(self.loop_range);
			next.continue_probe(self.probe_target);
			next.set_listen(self.listen);
			next.seek(self.position);
			if self.playing {
				next.play();
			}
			next.set_monitor_track(self.preview_track.or(self.monitor));
			for &t in &self.armed {
				next.set_armed(t, true);
			}
			next.set_metronome(self.metronome);
			next.set_recording(self.tap.is_some());
			if let Some(old) = &self.graph {
				next.continue_count_in(old);
			}
			if let Some(old) = self.graph.replace(next) {
				// Cannot fail: a slot was checked above and only this thread pushes.
				let _ = self.garbage.push(old);
			}
		}
		if self.preview_held == [0; 2]
			&& let Some(track) = self.preview_track.take()
			&& self.monitor != Some(track)
			&& let Some(g) = &mut self.graph
		{
			g.set_monitor_track(self.monitor);
		}
		// The newest map replaces the old one only when the old can be handed back
		// (before the commands: live values sent after it must reach it).
		while self.midi_map_garbage.slots() > 0
			&& let Ok(table) = self.midi_maps.pop()
		{
			if let Some(old) = self.midi_map.replace(table) {
				// Cannot fail: a slot was checked above and only this thread pushes.
				let _ = self.midi_map_garbage.push(old);
			}
		}
		while let Ok(cmd) = self.commands.pop() {
			match cmd {
				Cmd::Play => {
					self.playing = true;
					if let Some(g) = &mut self.graph {
						g.play();
					}
				}
				Cmd::Stop => {
					self.playing = false;
					if let Some(g) = &mut self.graph {
						g.stop();
					}
				}
				Cmd::Seek(tick) => {
					self.position = tick;
					if let Some(g) = &mut self.graph {
						g.seek(tick);
					}
				}
				Cmd::Monitor(track) => {
					self.monitor = track;
					// A preview in progress keeps its track until released.
					if self.preview_track.is_none()
						&& let Some(g) = &mut self.graph
					{
						g.set_monitor_track(track);
					}
				}
				Cmd::DetachMidi => self.retire_midi(),
				Cmd::Volume(track, db) => {
					if let Some(t) = &mut self.midi_map {
						t.external(MapTarget::Volume(track), db);
					}
					if let Some(g) = &mut self.graph {
						g.set_channel_volume(track, db);
					}
				}
				Cmd::Pan(track, pan) => {
					if let Some(t) = &mut self.midi_map {
						t.external(MapTarget::Pan(track), pan);
					}
					if let Some(g) = &mut self.graph {
						g.set_channel_pan(track, pan);
					}
				}
				Cmd::Param(device, index, value) => {
					if let Some(t) = &mut self.midi_map {
						let index = index as usize;
						t.external(MapTarget::Param { device, index }, value);
					}
					if let Some(g) = &mut self.graph {
						g.set_device_param(device, index as usize, value);
					}
				}
				Cmd::SendLevel(send, db) => {
					if let Some(t) = &mut self.midi_map {
						t.external(MapTarget::Send(send), db);
					}
					if let Some(g) = &mut self.graph {
						g.set_send_level(send, db);
					}
				}
				Cmd::Loop(range) => {
					self.loop_range = range;
					if let Some(g) = &mut self.graph {
						g.set_loop(range);
					}
				}
				Cmd::Probe(device) => {
					self.probe_target = device;
					if let Some(g) = &mut self.graph {
						g.set_probe_target(device);
					}
				}
				Cmd::Arm(track, on) => {
					if on && !self.armed.contains(&track) && self.armed.len() < ARM_SLOTS {
						self.armed.push(track);
					} else if !on {
						self.armed.retain(|t| *t != track);
					}
					if let Some(g) = &mut self.graph {
						g.set_armed(track, on);
					}
				}
				Cmd::DisarmAll => {
					self.armed.clear();
					if let Some(g) = &mut self.graph {
						g.clear_armed();
					}
				}
				Cmd::Metronome(settings) => {
					self.metronome = settings;
					if let Some(g) = &mut self.graph {
						g.set_metronome(settings);
					}
				}
				Cmd::AttachInput => {
					// Only when the old reader can be handed back.
					if self.input_garbage.slots() > 0
						&& let Ok(reader) = self.inputs.pop()
					{
						self.retire_input();
						self.input = Some(reader);
					}
				}
				Cmd::DetachInput => self.retire_input(),
				Cmd::StartRecord => {
					if self.tap_garbage.slots() > 0
						&& let Ok(tap) = self.taps.pop()
					{
						self.retire_tap();
						self.tap = Some(tap);
						if let Some(g) = &mut self.graph {
							g.set_recording(true);
						}
					}
				}
				Cmd::EndRecord => {
					self.retire_tap();
					if let Some(g) = &mut self.graph {
						g.set_recording(false);
					}
				}
				Cmd::PlayCountIn(bars) => {
					self.playing = true;
					if let Some(g) = &mut self.graph {
						g.play();
						g.start_count_in(bars);
					}
				}
				Cmd::MonitorFx(fx) => self.monitor_fx = fx,
				Cmd::Listen(target) => {
					self.listen = target;
					if let Some(g) = &mut self.graph {
						g.set_listen(target);
					}
				}
				Cmd::TestTone(on) => {
					self.test_tone = match (on, self.test_tone) {
						(true, Some(p)) => Some(p),
						(true, None) => Some(0.0),
						(false, _) => None,
					};
				}
				Cmd::Preview(track, event) => {
					// The previewed track plays (whatever is armed); the monitored track
					// gets the live input back once the preview is released.
					let current = self.preview_track.or(self.monitor);
					if current != Some(track) {
						self.preview_held = [0; 2];
						if let Some(g) = &mut self.graph {
							g.set_monitor_track(Some(track));
						}
					}
					self.preview_track = Some(track);
					match event {
						MidiEvent::NoteOn { pitch, .. } => {
							self.preview_held[usize::from(pitch >> 6) & 1] |= 1 << (pitch & 63);
						}
						MidiEvent::NoteOff { pitch, .. } => {
							self.preview_held[usize::from(pitch >> 6) & 1] &= !(1 << (pitch & 63));
						}
						_ => {}
					}
					if let Some(g) = &mut self.graph {
						g.push_live_event(event);
					}
				}
			}
		}
		self.receive_automation();
		self.receive_schedules();
		// A new input replaces the old one only when the old can be handed back.
		if self.midi_garbage.slots() > 0
			&& let Ok(input) = self.midi_inputs.pop()
		{
			self.retire_midi();
			self.midi = Some(input);
		}
		self.feed_live_midi();
	}

	/// Swaps in automation sets for the current graph. Sets meant for a graph that is
	/// still queued wait in `pending_automation`; stale ones go straight back. Every set
	/// leaves through the garbage ring, so nothing is dropped here.
	fn receive_automation(&mut self) {
		if let Some(set) = self.pending_automation.take() {
			self.apply_automation(set);
		}
		// Room for the popped set and a displaced pending one.
		while self.pending_automation.is_none() && self.automation_garbage.slots() >= 2 {
			let Ok(set) = self.automations.pop() else {
				break;
			};
			self.apply_automation(set);
		}
	}

	fn apply_automation(&mut self, mut set: Box<AutomationSet>) {
		let current = self.graph.as_ref().map(|g| g.load_id);
		match current {
			Some(id) if id == set.load_id => {
				if let Some(g) = &mut self.graph {
					g.swap_automation(&mut set.bindings);
				}
			}
			Some(id) if id < set.load_id => {
				self.pending_automation = Some(set);
				return;
			}
			_ => {}
		}
		if let Err(rtrb::PushError::Full(set)) = self.automation_garbage.push(set) {
			// Only reachable if the control side stopped collecting; keep it alive.
			self.pending_automation = Some(set);
		}
	}

	/// Swaps in rescheduled tracks for the current graph, like
	/// [`AudioCore::receive_automation`]: swaps for a queued graph wait, stale ones go
	/// back, and every box leaves through the garbage ring.
	fn receive_schedules(&mut self) {
		if let Some(swap) = self.pending_schedule.take() {
			self.apply_schedule(swap);
		}
		while self.pending_schedule.is_none() && self.schedule_garbage.slots() >= 2 {
			let Ok(swap) = self.schedules.pop() else {
				break;
			};
			self.apply_schedule(swap);
		}
	}

	fn apply_schedule(&mut self, mut swap: Box<ScheduleSwap>) {
		let current = self.graph.as_ref().map(|g| g.load_id);
		match current {
			Some(id) if id == swap.load_id => {
				if let Some(g) = &mut self.graph {
					g.swap_schedule(&mut swap);
				}
			}
			Some(id) if id < swap.load_id => {
				self.pending_schedule = Some(swap);
				return;
			}
			_ => {}
		}
		if let Err(rtrb::PushError::Full(swap)) = self.schedule_garbage.push(swap) {
			// Only reachable if the control side stopped collecting; keep it alive.
			self.pending_schedule = Some(swap);
		}
	}

	/// Hands the audio input reader back to the control thread (never dropped here).
	fn retire_input(&mut self) {
		if let Some(old) = self.input.take()
			&& let Err(rtrb::PushError::Full(old)) = self.input_garbage.push(old)
		{
			self.input = Some(old);
		}
	}

	/// Hands the recording tap back to the control thread (never dropped here).
	fn retire_tap(&mut self) {
		if let Some(old) = self.tap.take()
			&& let Err(rtrb::PushError::Full(old)) = self.tap_garbage.push(old)
		{
			self.tap = Some(old);
		}
	}

	/// Hands the current MIDI queue back to the control thread (never dropped here).
	fn retire_midi(&mut self) {
		if let Some(old) = self.midi.take()
			&& let Err(rtrb::PushError::Full(old)) = self.midi_garbage.push(old)
		{
			// Only reachable if the control side stopped collecting; keep it alive.
			self.midi = Some(old);
		}
	}

	/// Drains pending hardware MIDI into the monitored instrument (or discards it).
	fn feed_live_midi(&mut self) {
		let Some(queue) = &mut self.midi else {
			return;
		};
		self.mapped.clear();
		cadenza_midi_io::drain_into_block(queue, DRAIN_ALL, 1, self.sample_rate, &mut self.mapped);
		// Mapped controllers move their targets and stop here: neither the instrument
		// nor a MIDI recording gets them.
		let (table, graph, monitor) = (&mut self.midi_map, &mut self.graph, &self.midi_monitor);
		self.mapped.retain(|m| {
			monitor.heard(&m.raw);
			!table
				.as_mut()
				.is_some_and(|t| t.apply(&m.raw, graph.as_deref_mut(), monitor))
		});
		if let Some(g) = &mut self.graph {
			for event in self.mapped.iter().filter_map(|m| m.dsp) {
				g.push_live_event(event);
			}
		}
	}

	fn end_block(&mut self) {
		let mut count_in = (0.0, 0, 0);
		if let Some(g) = &self.graph {
			self.position = g.position();
			count_in = g.count_in_progress().unwrap_or(count_in);
		}
		let s = &self.shared;
		s.count_in_heard
			.store(count_in.0.to_bits(), Ordering::Relaxed);
		s.count_in_per_bar.store(count_in.2, Ordering::Relaxed);
		s.count_in_beats.store(count_in.1, Ordering::Relaxed);
		self.shared
			.position
			.store(self.position.0, Ordering::Relaxed);
	}

	/// Reads `frames` input frames (if an input is open) into `in_frames` and the
	/// graph's input FIFO. `at`: frames into the callback (for the latency measurement).
	fn read_input(&mut self, frames: usize, at: usize) {
		let Some(reader) = &mut self.input else {
			return;
		};
		let playback = self
			.playback_ns
			.map(|ns| ns + (at as f64 * 1e9 / self.sample_rate) as u64);
		reader.read(&mut self.in_frames, frames, playback);
		self.fifo.push(&self.in_frames, reader.channels(), frames);
	}

	/// Renders `frames` frames into the scratch buffers, `SCRATCH_FRAMES` at most.
	fn render(&mut self, frames: usize) {
		let (l, r) = (&mut self.scratch_l[..frames], &mut self.scratch_r[..frames]);
		let fifo = self.input.is_some().then_some(&mut self.fifo);
		match &mut self.graph {
			Some(g) => g.process_live(l, r, fifo),
			None => {
				l.fill(0.0);
				r.fill(0.0);
			}
		}
	}

	/// Hands what the block played to the recording tap: the input frames with the
	/// timeline samples they were heard at, and (first chunk only) the MIDI input.
	fn record(&mut self, first_chunk: bool) {
		let (Some(tap), Some(g)) = (&mut self.tap, &self.graph) else {
			return;
		};
		for seg in g.segments() {
			tap.audio(&self.in_frames, seg.offset, seg.len, seg.tag);
		}
		if first_chunk && let Some(seg) = g.segments().first() {
			for m in &self.mapped {
				tap.midi(seg.tag, m.raw);
			}
		}
	}

	/// Adds the settings dialog's test tone (440 Hz) to the scratch buffers.
	fn add_test_tone(&mut self, frames: usize) {
		let Some(phase) = &mut self.test_tone else {
			return;
		};
		let gain = cadenza_core::db_to_gain(TEST_TONE_DB);
		let step = std::f32::consts::TAU * 440.0 / self.sample_rate as f32;
		for i in 0..frames {
			let v = phase.sin() * gain;
			self.scratch_l[i] += v;
			self.scratch_r[i] += v;
			*phase += step;
			if *phase > std::f32::consts::TAU {
				*phase -= std::f32::consts::TAU;
			}
		}
	}

	fn fill_interleaved<T: SizedSample + FromSample<f32>>(
		&mut self,
		data: &mut [T],
		channels: usize,
	) {
		if !self.thread_published {
			self.thread_published = true;
			let tid = cadenza_plugin_host::realtime::current_thread_id();
			self.shared.audio_thread.store(tid, Ordering::Relaxed);
		}
		self.begin_block();
		let channels = channels.max(1);
		let mut at = 0;
		for chunk in data.chunks_mut(SCRATCH_FRAMES * channels) {
			let frames = chunk.len() / channels;
			self.read_input(frames, at);
			self.render(frames);
			self.record(at == 0);
			self.monitor_fx
				.apply(&mut self.scratch_l[..frames], &mut self.scratch_r[..frames]);
			self.add_test_tone(frames);
			at += frames;
			for (i, frame) in chunk.chunks_mut(channels).enumerate() {
				let (l, r) = (self.scratch_l[i], self.scratch_r[i]);
				if channels == 1 {
					frame[0] = T::from_sample(0.5 * (l + r));
					continue;
				}
				for (c, out) in frame.iter_mut().enumerate() {
					*out = T::from_sample(match c {
						0 => l,
						1 => r,
						_ => 0.0,
					});
				}
			}
		}
		self.end_block();
	}
}

impl Control {
	fn collect_garbage(&mut self) {
		while self.garbage.pop().is_ok() {}
		while self.automation_garbage.pop().is_ok() {}
		while self.schedule_garbage.pop().is_ok() {}
		while self.midi_garbage.pop().is_ok() {}
		while self.midi_map_garbage.pop().is_ok() {}
		while self.input_garbage.pop().is_ok() {}
	}

	/// Hands an audio input reader to the audio thread (replacing the current one).
	fn attach_input(&mut self, reader: InputReader) -> Result<(), EngineError> {
		self.collect_garbage();
		self.inputs
			.push(Box::new(reader))
			.map_err(|_| EngineError::Busy)?;
		self.send(Cmd::AttachInput)
	}

	/// Hands a MIDI input queue to the audio thread (replacing the current one).
	fn attach_midi(&mut self, queue: Consumer<TimedMessage>) -> Result<(), EngineError> {
		self.collect_garbage();
		self.midi_inputs.push(queue).map_err(|_| EngineError::Busy)
	}

	fn send(&mut self, cmd: Cmd) -> Result<(), EngineError> {
		self.collect_garbage();
		self.commands.push(cmd).map_err(|_| EngineError::Busy)
	}

	fn send_automation(&mut self, set: AutomationSet) -> Result<(), EngineError> {
		self.collect_garbage();
		self.automations
			.push(Box::new(set))
			.map_err(|_| EngineError::Busy)
	}

	fn send_schedule(&mut self, swap: ScheduleSwap) -> Result<(), EngineError> {
		self.collect_garbage();
		self.schedules
			.push(Box::new(swap))
			.map_err(|_| EngineError::Busy)
	}

	fn load_graph(&mut self, graph: Graph) -> Result<(), EngineError> {
		let mut graph = Box::new(graph);
		let deadline = Instant::now() + Duration::from_secs(2);
		loop {
			self.collect_garbage();
			match self.graphs.push(graph) {
				Ok(()) => return Ok(()),
				Err(rtrb::PushError::Full(g)) => {
					if Instant::now() > deadline {
						return Err(EngineError::Busy);
					}
					graph = g;
					std::thread::sleep(Duration::from_millis(2));
				}
			}
		}
	}
}

/// A count-in in progress (see [`PlayerClock::count_in`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CountInStatus {
	/// Beats counted so far (fractional).
	pub heard_beats: f64,
	/// Length of the count-in in beats.
	pub beats: u32,
	pub per_bar: u32,
}

impl CountInStatus {
	/// Where the count-in is, as a bar before the record start (-2, -1) and a 1-based
	/// beat in it: what a bars.beats display shows for it.
	pub fn bar_beat(&self) -> (i64, u32) {
		let heard = self.heard_beats.max(0.0).floor() as i64;
		let per_bar = i64::from(self.per_bar.max(1));
		let bars = (i64::from(self.beats) + per_bar - 1) / per_bar;
		let bar = -(bars - heard / per_bar);
		(bar.min(-1), (heard % per_bar) as u32 + 1)
	}
}

/// Read-only view of a [`Player`]'s transport that can be sent to and polled from any
/// thread (e.g. a UI drawing the playhead) while the `Player` itself stays on its own
/// thread (its `cpal::Stream` is not `Send` everywhere).
#[derive(Debug, Clone)]
pub struct PlayerClock {
	shared: Arc<Shared>,
	sample_rate: u32,
}

impl PlayerClock {
	/// Current playhead (updated by the audio thread once per callback).
	pub fn position(&self) -> Tick {
		Tick(self.shared.position.load(Ordering::Relaxed))
	}

	pub fn is_playing(&self) -> bool {
		self.shared.playing.load(Ordering::Relaxed)
	}

	/// The count-in before a recording, while it runs (the playhead waits at the record
	/// start meanwhile).
	pub fn count_in(&self) -> Option<CountInStatus> {
		let s = &self.shared;
		let beats = s.count_in_beats.load(Ordering::Relaxed);
		(beats > 0).then(|| CountInStatus {
			heard_beats: f64::from_bits(s.count_in_heard.load(Ordering::Relaxed)),
			beats,
			per_bar: s.count_in_per_bar.load(Ordering::Relaxed).max(1),
		})
	}

	/// Output device sample rate.
	pub fn sample_rate(&self) -> u32 {
		self.sample_rate
	}
}

/// Realtime player on the default output device.
pub struct Player {
	control: Control,
	sample_rate: u32,
	plugins: Option<Arc<PluginCatalog>>,
	hosting: PluginHosting,
	/// Sandboxed devices of the last loaded graph, and whether each crash was taken.
	watches: Vec<(PluginWatch, bool)>,
	midi_input: Option<MidiInputConnection>,
	/// Meters of the last loaded graph.
	meters: Option<Arc<MeterBank>>,
	/// Ring every loaded graph writes the probed device into, and that device.
	probe: Arc<DeviceProbe>,
	probe_target: Option<DeviceId>,
	/// Ring every loaded graph writes the master's post-fader output into.
	master_tap: Arc<DeviceProbe>,
	monitor_fx: MonitorFx,
	listen: Option<(DeviceId, Listen)>,
	/// Address map of the last loaded graph (live parameters and automation).
	resolver: Option<Resolver>,
	/// Targets automated in the running graph.
	automated: HashSet<AutoTarget>,
	/// Id of the last loaded graph.
	load_id: u64,
	/// Audition switch of the last loaded graph, and the scope it plays.
	audition_state: Option<Arc<Audition>>,
	audition: Option<AuditionScope>,
	loop_range: Option<(Tick, Tick)>,
	editors: Editors,
	/// Output device name (`virtual` for a [`VirtualDevice`]).
	output_device: String,
	/// The open audio input stream and what it is.
	input_stream: Option<cpal::Stream>,
	input_info: Option<InputInfo>,
	input_error: Option<String>,
	input_channels: usize,
	input_stats: Arc<InputStats>,
	/// Measured output latency (callback -> playback), frames.
	output_latency: Arc<AtomicU64>,
	/// User latency offset added to the measured recording latency, frames.
	latency_offset: i64,
	metronome: MetronomeSettings,
	armed: Vec<TrackId>,
	recording: Option<RecordSession>,
	/// How the output callback thread is scheduled, once the helper thread raised it.
	audio_scheduling: Arc<Mutex<Option<String>>>,
	_stream: Option<cpal::Stream>,
}

/// Drives a [`Player`]'s audio callback without hardware: tests feed input frames and
/// pull output frames exactly like a duplex sound card would (same clock, no latency).
pub struct VirtualDevice {
	core: AudioCore,
	input: Option<Producer<f32>>,
	input_channels: usize,
}

impl VirtualDevice {
	/// The device reports a buffer under/overrun, like cpal's `Xrun` error (tests).
	pub fn report_xrun(&self) {
		self.core
			.shared
			.device_xruns
			.fetch_add(1, Ordering::Relaxed);
	}

	/// One callback: `input` (interleaved, the device's input channels) arrives, then
	/// `output` (interleaved stereo) is rendered. Never allocates.
	pub fn process(&mut self, output: &mut [f32], input: &[f32]) {
		if let Some(tx) = &mut self.input {
			let n = input.len().min(tx.slots());
			if let Ok(chunk) = tx.write_chunk_uninit(n) {
				chunk.fill_from_iter(input[..n].iter().copied());
			}
		}
		self.core.fill_interleaved(output, 2);
	}

	/// Input channels the device delivers.
	pub fn input_channels(&self) -> usize {
		self.input_channels
	}
}

/// Plugin editors of the loaded graph and the ones the user opened.
#[derive(Default)]
struct Editors {
	/// Editor handle per plugin device of the loaded graph.
	slots: HashMap<DeviceId, PluginEditorSlot>,
	/// Open editors and their window titles (kept across loads).
	open: HashMap<DeviceId, String>,
	/// Devices whose plugin changed state in its editor since the last capture.
	dirty: std::collections::HashSet<DeviceId>,
	/// Last plain value each editor reported per parameter, to skip echoing it back.
	reported: HashMap<(DeviceId, String), f32>,
	/// Updates produced outside a poll (by a load), returned by the next poll.
	pending: Vec<EditorUpdate>,
	/// States captured from live instances, for the caller to store.
	captured: Vec<EditorState>,
}

/// Lists the MIDI input ports (hardware and virtual).
pub fn midi_input_ports() -> Result<Vec<PortInfo>, EngineError> {
	cadenza_midi_io::list_input_ports().map_err(|e| {
		EngineError::Midi(format!(
			"{e}; check that the MIDI subsystem (ALSA sequencer on Linux) is available"
		))
	})
}

/// Finds an input port by index (`"0"`) or case-insensitive name fragment.
pub fn find_midi_input(query: &str) -> Result<PortInfo, EngineError> {
	let ports = midi_input_ports()?;
	let q = query.trim().to_lowercase();
	let found = match q.parse::<usize>() {
		Ok(i) => ports.iter().find(|p| p.index == i),
		Err(_) => ports.iter().find(|p| p.name.to_lowercase().contains(&q)),
	};
	found.cloned().ok_or_else(|| {
		let list = ports
			.iter()
			.map(|p| format!("{}: {}", p.index, p.name))
			.collect::<Vec<_>>()
			.join(", ");
		EngineError::Midi(format!(
			"no MIDI input matches '{query}'; available: {}",
			if list.is_empty() {
				"none (connect a device)".into()
			} else {
				list
			}
		))
	})
}

/// Ends every playback crash message.
const PLAYBACK_CRASH_HINT: &str = "Playback continues without it. Reload the project into \
	the player (e.g. play again) to restart it from the project's saved state; if it keeps \
	failing, replace this device with another plugin or a built-in device, or bypass it.";

impl Player {
	/// Opens the default output device and starts a (silent) output stream.
	/// Fails with [`EngineError::NoOutputDevice`] when the system has no output device.
	pub fn new() -> Result<Self, EngineError> {
		Self::open(&AudioConfig::default())
	}

	/// Opens the output (and, if enabled, input) devices of `config`. A failing input
	/// does not fail the player: see [`Player::input_error`].
	pub fn open(config: &AudioConfig) -> Result<Self, EngineError> {
		let device = crate::input::find_device(config.output.as_deref(), false)?;
		let output_device = crate::input::device_name(&device);
		let default = device
			.default_output_config()
			.map_err(|e| EngineError::Audio(e.to_string()))?;
		let supported = match config.sample_rate {
			Some(rate) if rate != default.sample_rate() => device
				.supported_output_configs()
				.map_err(|e| EngineError::Audio(e.to_string()))?
				.filter(|c| {
					c.channels() >= 2 && (c.min_sample_rate()..=c.max_sample_rate()).contains(&rate)
				})
				.max_by_key(|c| c.sample_format() == SampleFormat::F32)
				.map(|c| c.with_sample_rate(rate))
				.unwrap_or(default),
			_ => default,
		};
		let format = supported.sample_format();
		let mut config_out: cpal::StreamConfig = supported.config();
		if let Some(frames) = config.buffer_frames.filter(|f| *f > 0) {
			let ok = match supported.buffer_size() {
				cpal::SupportedBufferSize::Range { min, max } => (*min..=*max).contains(&frames),
				cpal::SupportedBufferSize::Unknown => true,
			};
			if ok {
				config_out.buffer_size = cpal::BufferSize::Fixed(frames);
			}
		}
		let channels = config_out.channels as usize;
		let (control, core) = channel_pair(config_out.sample_rate);
		let output_latency = Arc::new(AtomicU64::new(0));
		let lat = output_latency.clone();
		let shared = control.shared.clone();
		let stream = match format {
			SampleFormat::F32 => build::<f32>(&device, config_out, channels, core, lat, shared),
			SampleFormat::F64 => build::<f64>(&device, config_out, channels, core, lat, shared),
			SampleFormat::I16 => build::<i16>(&device, config_out, channels, core, lat, shared),
			SampleFormat::I32 => build::<i32>(&device, config_out, channels, core, lat, shared),
			SampleFormat::U16 => build::<u16>(&device, config_out, channels, core, lat, shared),
			other => Err(EngineError::Audio(format!(
				"unsupported output sample format {other}"
			))),
		}?;
		stream
			.play()
			.map_err(|e| EngineError::Audio(e.to_string()))?;
		let mut player = Self::with_parts(
			control,
			config_out.sample_rate,
			output_device,
			Some(stream),
			output_latency,
		);
		promote_output_thread(
			player.control.shared.clone(),
			player.audio_scheduling.clone(),
		);
		if config.input_enabled
			&& let Err(e) = player.set_audio_input(Some(config.input.as_deref()))
		{
			player.input_error = Some(e.to_string());
		}
		Ok(player)
	}

	/// A player whose callback is driven by hand through the returned [`VirtualDevice`]
	/// (no sound card): `input_channels` channels of input at `sample_rate`, read 1:1
	/// like a duplex device. For tests.
	pub fn virtual_device(sample_rate: u32, input_channels: u16) -> (Self, VirtualDevice) {
		let (control, mut core) = channel_pair(sample_rate);
		let channels = usize::from(input_channels).min(MAX_INPUT_CHANNELS);
		let mut player = Self::with_parts(
			control,
			sample_rate,
			"virtual".into(),
			None,
			Arc::new(AtomicU64::new(0)),
		);
		let mut input = None;
		if channels > 0 {
			let ((tx, _stamps), (rx, srx)) = crate::input::input_rings(channels, sample_rate);
			core.input = Some(Box::new(InputReader::new(
				rx,
				srx,
				channels,
				sample_rate,
				InputMode::Exact,
				Some(0),
				player.input_stats.clone(),
			)));
			input = Some(tx);
			player.input_channels = channels;
			player.input_info = Some(InputInfo {
				device: "virtual".into(),
				channels: input_channels,
				sample_rate,
				mode: InputMode::Exact,
			});
		}
		(
			player,
			VirtualDevice {
				core,
				input,
				input_channels: channels,
			},
		)
	}

	fn with_parts(
		control: Control,
		sample_rate: u32,
		output_device: String,
		stream: Option<cpal::Stream>,
		output_latency: Arc<AtomicU64>,
	) -> Self {
		Self {
			control,
			sample_rate,
			plugins: None,
			hosting: PluginHosting::default(),
			watches: Vec::new(),
			midi_input: None,
			meters: None,
			probe: Arc::new(DeviceProbe::new(sample_rate)),
			probe_target: None,
			master_tap: Arc::new(DeviceProbe::new(sample_rate)),
			monitor_fx: MonitorFx::default(),
			listen: None,
			resolver: None,
			automated: HashSet::new(),
			load_id: 0,
			audition_state: None,
			audition: None,
			loop_range: None,
			editors: Editors::default(),
			output_device,
			input_stream: None,
			input_info: None,
			input_error: None,
			input_channels: 0,
			input_stats: Arc::new(InputStats::default()),
			output_latency,
			latency_offset: 0,
			metronome: MetronomeSettings::default(),
			armed: Vec::new(),
			recording: None,
			audio_scheduling: Arc::new(Mutex::new(None)),
			_stream: stream,
		}
	}

	/// Name of the output device.
	pub fn output_device(&self) -> &str {
		&self.output_device
	}

	/// Opens audio input device `device` (`Some(None)`: the default input) for input
	/// monitoring and recording, or closes the input (`None`). The input follows the
	/// output's clock when it is the same device at the same rate; otherwise it is
	/// resampled with drift correction (see [`crate::input`]).
	pub fn set_audio_input(&mut self, device: Option<Option<&str>>) -> Result<(), EngineError> {
		if self.recording.is_some() {
			return Err(EngineError::Record(
				"stop recording before changing the audio input".into(),
			));
		}
		self.control.send(Cmd::DetachInput)?;
		self.input_stream = None;
		self.input_info = None;
		self.input_channels = 0;
		self.input_error = None;
		let Some(name) = device else {
			return Ok(());
		};
		let open = crate::input::open_input(
			name,
			self.sample_rate,
			&self.output_device,
			self.input_stats.clone(),
		)?;
		let channels = usize::from(open.info.channels).min(MAX_INPUT_CHANNELS);
		let reader = InputReader::new(
			open.ring,
			open.stamps,
			usize::from(open.info.channels),
			open.info.sample_rate,
			open.info.mode,
			None,
			self.input_stats.clone(),
		);
		self.control.attach_input(reader)?;
		self.input_stream = Some(open.stream);
		self.input_channels = channels;
		self.input_info = Some(open.info);
		Ok(())
	}

	/// The open audio input, if any.
	pub fn input_info(&self) -> Option<&InputInfo> {
		self.input_info.as_ref()
	}

	/// Why the input of [`Player::open`] could not be opened.
	pub fn input_error(&self) -> Option<&str> {
		self.input_error.as_deref()
	}

	/// Input levels and dropout counters (lock-free; poll from any thread).
	pub fn input_stats(&self) -> Arc<InputStats> {
		self.input_stats.clone()
	}

	/// How the output callback thread is scheduled (e.g. `SCHED_FIFO 10 (RealtimeKit)`),
	/// once known; `None` before its first callback or without a device.
	pub fn audio_scheduling(&self) -> Option<String> {
		self.audio_scheduling
			.lock()
			.unwrap_or_else(|p| p.into_inner())
			.clone()
	}

	/// Buffer under/overruns the output device reported since it was opened (audible
	/// clicks: the callback was late; a larger buffer helps).
	pub fn device_xruns(&self) -> u64 {
		self.control.shared.device_xruns.load(Ordering::Relaxed)
	}

	/// Measured output latency (callback to playback), frames (0 when unknown).
	pub fn output_latency(&self) -> u64 {
		self.output_latency.load(Ordering::Relaxed)
	}

	/// Extra recording latency compensation, frames (positive moves recordings
	/// earlier). Added to the measured latency.
	pub fn set_latency_offset(&mut self, frames: i64) {
		self.latency_offset = frames;
	}

	pub fn latency_offset(&self) -> i64 {
		self.latency_offset
	}

	/// The recording latency compensation the next recording would use, frames.
	pub fn recording_latency(&self) -> i64 {
		(self.input_stats.delay_frames(self.sample_rate).round() as i64 + self.latency_offset)
			.max(0)
	}

	/// Plays a 440 Hz test tone (-20 dBFS) over everything else.
	pub fn set_test_tone(&mut self, on: bool) -> Result<(), EngineError> {
		self.control.send(Cmd::TestTone(on))
	}

	pub fn set_metronome(&mut self, settings: MetronomeSettings) -> Result<(), EngineError> {
		self.metronome = settings;
		self.control.send(Cmd::Metronome(settings))
	}

	pub fn metronome(&self) -> MetronomeSettings {
		self.metronome
	}

	/// Arms (or disarms) `track` for recording; kept across loads. Audio tracks in auto
	/// monitoring hear their input while armed and stopped or recording.
	pub fn set_armed(&mut self, track: TrackId, armed: bool) -> Result<(), EngineError> {
		if armed && !self.armed.contains(&track) {
			if self.armed.len() >= ARM_SLOTS {
				return Err(EngineError::Record(format!(
					"at most {ARM_SLOTS} tracks can be armed at once"
				)));
			}
			self.armed.push(track);
		} else if !armed {
			self.armed.retain(|t| *t != track);
		}
		self.control.send(Cmd::Arm(track, armed))
	}

	pub fn disarm_all(&mut self) -> Result<(), EngineError> {
		self.armed.clear();
		self.control.send(Cmd::DisarmAll)
	}

	pub fn armed(&self) -> &[TrackId] {
		&self.armed
	}

	/// Starts recording `setup.tracks` and playback from the current playhead (after
	/// the count-in): audio tracks write their input to WAV files in `setup.folder`,
	/// instrument tracks capture the MIDI input. Recording captures from `setup.start`
	/// (punch-in) to `setup.punch_out`. Stop with [`Player::stop_recording`].
	pub fn start_recording(
		&mut self,
		setup: &RecordSetup,
	) -> Result<Arc<RecordingMonitor>, EngineError> {
		if self.recording.is_some() {
			return Err(EngineError::Record("already recording".into()));
		}
		if setup.tracks.is_empty() {
			return Err(EngineError::Record(
				"no track is armed; arm an audio or instrument track first".into(),
			));
		}
		let Some(resolver) = &self.resolver else {
			return Err(EngineError::Record(
				"no project is loaded into the player yet".into(),
			));
		};
		if setup.tracks.iter().any(|t| t.input.is_some()) && self.input_channels == 0 {
			return Err(EngineError::Record(
				"no audio input is open; choose an input device in the audio settings".into(),
			));
		}
		let (tap, mut session) = crate::record::start(
			setup,
			&resolver.tempo,
			self.sample_rate,
			self.input_channels,
		)?;
		self.control.collect_garbage();
		self.control.taps.push(tap).map_err(|_| EngineError::Busy)?;
		self.input_stats.reset_latency();
		session.input_dropouts_at_start = self.input_stats.dropouts();
		let monitor = session.monitor.clone();
		self.recording = Some(session);
		self.control.send(Cmd::StartRecord)?;
		// Already playing: punch in on the fly (capture starts now, no count-in, the
		// transport keeps going).
		if !self.control.shared.playing.swap(true, Ordering::Relaxed) {
			self.control.send(Cmd::PlayCountIn(setup.count_in_bars))?;
		}
		Ok(monitor)
	}

	/// The recording in progress, if any (for drawing it live).
	pub fn recording_monitor(&self) -> Option<Arc<RecordingMonitor>> {
		self.recording.as_ref().map(|r| r.monitor.clone())
	}

	pub fn is_recording(&self) -> bool {
		self.recording.is_some()
	}

	/// Stops capturing and the transport; finish with [`Player::finish_recording`].
	pub fn request_stop_recording(&mut self) -> Result<(), EngineError> {
		self.control.shared.playing.store(false, Ordering::Relaxed);
		self.control.send(Cmd::EndRecord)?;
		self.control.send(Cmd::Stop)
	}

	/// Waits (at most `timeout`) for the audio thread to hand the recording back, lets
	/// the writer finish the files and returns what was recorded (build the project
	/// edit with [`crate::record_commands`]).
	pub fn finish_recording(&mut self, timeout: Duration) -> Result<Recording, EngineError> {
		if self.recording.is_none() {
			return Err(EngineError::Record("not recording".into()));
		}
		let deadline = Instant::now() + timeout;
		let tap = loop {
			if let Ok(tap) = self.control.tap_garbage.pop() {
				break tap;
			}
			if Instant::now() > deadline {
				return Err(EngineError::Record(
					"the audio thread did not hand the recording back; is the output device \
					 still running?"
						.into(),
				));
			}
			std::thread::sleep(Duration::from_millis(1));
		};
		let mut session = self.recording.take().expect("recording");
		let stop = tap.next_tag.unwrap_or(session.monitor.start);
		drop(tap);
		let output = session
			.writer
			.take()
			.and_then(|w| w.join().ok())
			.unwrap_or_default();
		let mut warnings = Vec::new();
		if let Some(e) = output.error {
			warnings.push(e);
		}
		let dropouts = self
			.input_stats
			.dropouts()
			.saturating_sub(session.input_dropouts_at_start)
			+ session.stats.dropped_frames();
		if dropouts > 0 {
			warnings.push(format!(
				"{dropouts} frame(s) were lost while recording (dropouts); try a larger buffer \
				 size in the audio settings"
			));
		}
		Ok(Recording {
			sample_rate: self.sample_rate,
			start: session.monitor.start,
			punch_out: session.punch_out,
			stop,
			latency: self.recording_latency(),
			midi_latency: self.output_latency() as i64,
			passes: output.passes,
			audio: output.audio,
			midi_tracks: session.midi_tracks,
			midi: output.midi,
			dropouts,
			warnings,
		})
	}

	/// Stops capturing but keeps the transport playing (punch out on the fly); finish
	/// with [`Player::finish_recording`].
	pub fn request_end_recording(&mut self) -> Result<(), EngineError> {
		self.control.send(Cmd::EndRecord)
	}

	/// [`Player::request_end_recording`] + [`Player::finish_recording`].
	pub fn end_recording(&mut self) -> Result<Recording, EngineError> {
		self.request_end_recording()?;
		self.finish_recording(crate::record::FINISH_TIMEOUT)
	}

	/// [`Player::request_stop_recording`] + [`Player::finish_recording`].
	pub fn stop_recording(&mut self) -> Result<Recording, EngineError> {
		self.request_stop_recording()?;
		self.finish_recording(crate::record::FINISH_TIMEOUT)
	}

	/// Output device sample rate; projects are compiled at this rate.
	pub fn sample_rate(&self) -> u32 {
		self.sample_rate
	}

	/// A thread-safe handle to the playhead and play state.
	pub fn clock(&self) -> PlayerClock {
		PlayerClock {
			shared: self.control.shared.clone(),
			sample_rate: self.sample_rate,
		}
	}

	/// Moves a fader of the loaded graph immediately (smoothed, no recompilation, notes
	/// keep sounding); `track: None` is the master. Use it while the user drags a fader,
	/// alongside the matching project edit, so a later [`Player::load`] agrees. Ignored
	/// for tracks the loaded graph does not have.
	pub fn set_live_volume(&mut self, track: Option<TrackId>, db: f32) -> Result<(), EngineError> {
		self.control.send(Cmd::Volume(track, db))
	}

	/// Like [`Player::set_live_volume`], for pan (-1..1).
	pub fn set_live_pan(&mut self, track: Option<TrackId>, pan: f32) -> Result<(), EngineError> {
		self.control.send(Cmd::Pan(track, pan))
	}

	/// Sets a device parameter (plain value; `param` is the built-in or plugin param id)
	/// in the running graph, without recompiling: built-ins, in-process and sandboxed
	/// plugins alike, so sounding notes keep playing. Use it alongside the matching
	/// project edit. Returns `Ok(false)` when the loaded graph does not have that device
	/// or parameter (e.g. a bypassed device): reload instead.
	pub fn set_live_param(
		&mut self,
		device: DeviceId,
		param: &str,
		value: f32,
	) -> Result<bool, EngineError> {
		let Some(index) = self
			.resolver
			.as_ref()
			.filter(|r| !r.changes_latency(device, param))
			.and_then(|r| r.param_index(device, param))
		else {
			return Ok(false);
		};
		// The plugin's editor reported this very value: it already has it (sending it
		// back could make the editor's control jump through a rounding error).
		if self
			.editors
			.reported
			.remove(&(device, param.to_string()))
			.is_some_and(|v| v == value)
		{
			return Ok(true);
		}
		self.control
			.send(Cmd::Param(device, index as u32, value))
			.map(|()| true)
	}

	/// Sets a send's level (dB) in the running graph, ramped and without recompiling.
	/// Returns `Ok(false)` when the loaded graph does not play that send.
	pub fn set_live_send(&mut self, send: DeviceId, db: f32) -> Result<bool, EngineError> {
		if !self
			.resolver
			.as_ref()
			.is_some_and(|r| r.sends.contains_key(&send))
		{
			return Ok(false);
		}
		self.control.send(Cmd::SendLevel(send, db)).map(|()| true)
	}

	/// Replaces the automation of the running graph with `lanes` (the whole project's
	/// lanes), without recompiling or cutting notes. Returns `Ok(false)` when that is not
	/// possible and the project must be reloaded instead: nothing loaded yet, a different
	/// tempo map, or a lane removed (its parameter must return to its static value).
	pub fn set_automation(
		&mut self,
		lanes: &[AutomationLane],
		tempo: &TempoMap,
	) -> Result<bool, EngineError> {
		let Some(resolver) = &self.resolver else {
			return Ok(false);
		};
		if *tempo != resolver.tempo {
			return Ok(false);
		}
		let bindings = resolver.bind(lanes, tempo, None, &mut Vec::new());
		let targets = crate::automation::targets(&bindings);
		if !self.automated.is_subset(&targets) {
			return Ok(false);
		}
		self.control.send_automation(AutomationSet {
			load_id: self.load_id,
			bindings: crate::automation::AutomationBindings(bindings),
		})?;
		self.automated = targets;
		Ok(true)
	}

	/// Sets the catalog used to load plugin devices on subsequent [`Player::load`] calls
	/// (`None`, the default, plays built-in devices only).
	pub fn set_plugins(&mut self, plugins: Option<Arc<PluginCatalog>>) {
		self.plugins = plugins;
	}

	/// Compiles `project` (on this thread) and swaps it in without interrupting the stream.
	/// Playback keeps its tick position and play state; notes sounding at the swap are cut.
	/// Returns the compile warnings.
	///
	/// Every plugin device is created afresh, so this also restarts crashed plugins.
	pub fn load(&mut self, project: &Project) -> Result<Vec<String>, EngineError> {
		// Plugin state changed in an editor lives only in the running instance so far:
		// carry it over to the new one.
		let captured = self.capture_dirty_states(project, &[]);
		let patched;
		let project = if captured.is_empty() {
			project
		} else {
			let mut doc = Document::new(project.clone());
			for c in &captured {
				let _ = doc.execute(Command::SetDeviceState {
					track: c.track,
					device: c.device,
					state: Some(c.state.clone()),
					params: None,
				});
			}
			patched = doc.into_project();
			&patched
		};
		self.editors.captured.extend(captured);
		let mut graph = compile_hosted(
			project,
			self.sample_rate,
			crate::QUANTUM,
			self.plugins.as_deref(),
			&self.hosting,
		)?;
		graph.attach_probe(Some(self.probe.clone()));
		graph.attach_master_tap(Some(self.master_tap.clone()));
		let warnings = graph.warnings().to_vec();
		let watches = graph.plugin_watches.clone();
		let meters = graph.meters();
		let resolver = graph.resolver.clone();
		let automated = crate::automation::targets(&graph.automation);
		graph.load_id = self.load_id + 1;
		let audition = graph.audition.clone();
		let editor_slots = graph.plugin_editors.clone();
		audition.set(
			self.audition
				.as_ref()
				.map(|a| resolver.audition_mask(&a.tracks))
				.as_deref(),
		);
		self.control.load_graph(graph)?;
		self.load_id += 1;
		self.audition_state = Some(audition);
		self.watches = watches.into_iter().map(|w| (w, false)).collect();
		self.meters = Some(meters);
		self.resolver = Some(resolver);
		self.automated = automated;
		self.swap_editors(editor_slots);
		self.set_midi_map(project)?;
		Ok(warnings)
	}

	/// Replaces the editor handles with the new graph's, moving open editors over to
	/// the new instances (devices that are gone report their editor closed).
	fn swap_editors(&mut self, slots: Vec<PluginEditorSlot>) {
		let old = std::mem::replace(
			&mut self.editors.slots,
			slots.into_iter().map(|s| (s.device_id, s)).collect(),
		);
		self.editors.reported.clear();
		let open: Vec<(DeviceId, String)> = self.editors.open.drain().collect();
		for (device, title) in open {
			if let Some(slot) = old.get(&device) {
				let _ = slot.editor.close();
			}
			let reopened = self
				.editors
				.slots
				.get(&device)
				.is_some_and(|s| s.editor.open(&title).is_ok());
			if reopened {
				self.editors.open.insert(device, title);
			} else {
				self.editors.pending.push(EditorUpdate {
					device,
					event: EditorEvent::Closed,
				});
			}
		}
	}

	/// Live states of the devices whose editors changed their state, plus `extra`
	/// (those still in `project`).
	fn capture_dirty_states(&mut self, project: &Project, extra: &[DeviceId]) -> Vec<EditorState> {
		let mut devices: Vec<DeviceId> = self.editors.dirty.drain().collect();
		for d in extra {
			if !devices.contains(d) {
				devices.push(*d);
			}
		}
		let mut out = Vec::new();
		for device in devices {
			let Some(slot) = self.editors.slots.get(&device) else {
				continue;
			};
			if project.device(slot.track_id, device).is_none() {
				continue;
			}
			if let Ok(Some(state)) = slot.editor.save_state() {
				out.push(EditorState {
					track: slot.track_id,
					device,
					state: cadenza_plugin_host::encode_state(&state),
				});
			}
		}
		out
	}

	/// Whether `device` is a plugin of the loaded project (so it may have an editor).
	pub fn has_editor(&self, device: DeviceId) -> bool {
		self.editors.slots.contains_key(&device)
	}

	/// Opens (or raises) the editor window of plugin device `device`, titled `title`.
	/// Fails when the device is not a plugin of the loaded project (built-in, bypassed,
	/// failed to load, or not loaded yet) or the plugin cannot show an editor.
	pub fn open_editor(&mut self, device: DeviceId, title: &str) -> Result<(), EngineError> {
		let slot = self.editors.slots.get(&device).ok_or_else(|| {
			EngineError::Device(format!(
				"device {device} is not a loaded plugin (built-in devices have no editor; \
				 bypassed or failed plugins are not loaded)"
			))
		})?;
		slot.editor
			.open(title)
			.map_err(|e| EngineError::Device(e.to_string()))?;
		self.editors.open.insert(device, title.to_string());
		Ok(())
	}

	/// Closes the editor of `device` (no-op when it is not open).
	pub fn close_editor(&mut self, device: DeviceId) -> Result<(), EngineError> {
		self.editors.open.remove(&device);
		match self.editors.slots.get(&device) {
			Some(slot) => slot
				.editor
				.close()
				.map_err(|e| EngineError::Device(e.to_string())),
			None => Ok(()),
		}
	}

	/// Devices whose editor is open.
	pub fn open_editors(&self) -> Vec<DeviceId> {
		self.editors.open.keys().copied().collect()
	}

	/// What happened in the open editors since the last call: parameter gestures and
	/// values (plain, by param id), state changes and closed windows (also when a
	/// device disappeared or its sandbox crashed). Call it regularly (e.g. every 30 ms)
	/// from the control thread while editors are open.
	pub fn poll_editors(&mut self) -> Vec<EditorUpdate> {
		let mut out = std::mem::take(&mut self.editors.pending);
		let open: Vec<DeviceId> = self.editors.open.keys().copied().collect();
		for device in open {
			let Some(slot) = self.editors.slots.get(&device) else {
				continue;
			};
			for event in slot.editor.poll() {
				match &event {
					EditorEvent::Value { param, value } => {
						self.editors
							.reported
							.insert((device, param.clone()), *value as f32);
					}
					EditorEvent::StateChanged => {
						self.editors.dirty.insert(device);
					}
					EditorEvent::Closed => {
						self.editors.open.remove(&device);
					}
					_ => {}
				}
				out.push(EditorUpdate { device, event });
			}
		}
		out
	}

	/// Captures the live state of `devices` and of every device whose editor reported a
	/// state change (e.g. when an editor closes, or before saving the project); the
	/// results, and states carried over by [`Player::load`], come back from
	/// [`Player::take_editor_states`]. Devices that are not loaded plugins are skipped.
	pub fn capture_editor_states(&mut self, project: &Project, devices: &[DeviceId]) {
		let captured = self.capture_dirty_states(project, devices);
		self.editors.captured.extend(captured);
	}

	/// Live plugin states captured since the last call, to store in the project with
	/// `Command::SetDeviceState { params: None }` (they are what the running instances
	/// already have, so storing them needs no reload).
	pub fn take_editor_states(&mut self) -> Vec<EditorState> {
		std::mem::take(&mut self.editors.captured)
	}

	/// Live per-track and master levels of the loaded project, updated by the audio
	/// thread once per callback without locks or allocation (`None` before the first
	/// [`Player::load`]). The bank changes on every load; keep the `Arc` only until then.
	pub fn meters(&self) -> Option<Arc<MeterBank>> {
		self.meters.clone()
	}

	/// Follows `device` (a built-in or in-process plugin anywhere in the project: track
	/// instrument, inserts, master inserts) with the probe, or stops (`None`). Kept across
	/// loads; while the loaded graph lacks the device the probe is idle.
	pub fn set_probe(&mut self, device: Option<DeviceId>) -> Result<(), EngineError> {
		if self.probe_target == device {
			return Ok(());
		}
		self.control.send(Cmd::Probe(device))?;
		self.probe_target = device;
		Ok(())
	}

	/// The probe ring while a device is probed (one per player: survives loads).
	pub fn probe(&self) -> Option<Arc<DeviceProbe>> {
		self.probe_target.map(|_| self.probe.clone())
	}

	/// The probed device.
	pub fn probe_target(&self) -> Option<DeviceId> {
		self.probe_target
	}

	/// Ring of the master's post-fader output (one per player: survives loads), for live
	/// loudness. Every rendered quantum is written as both sides of a
	/// [`crate::ProbeFrame`]; a full ring drops frames, so drain it regularly (the
	/// first drain after a pause may return up to half a second of older audio).
	pub fn master_tap(&self) -> Arc<DeviceProbe> {
		self.master_tap.clone()
	}

	/// Monitoring switches (mono, dim) on the device output; kept across loads, never
	/// rendered offline.
	pub fn set_monitor(&mut self, fx: MonitorFx) -> Result<(), EngineError> {
		if self.monitor_fx == fx {
			return Ok(());
		}
		self.control.send(Cmd::MonitorFx(fx))?;
		self.monitor_fx = fx;
		Ok(())
	}

	pub fn monitor_fx(&self) -> MonitorFx {
		self.monitor_fx
	}

	/// Makes `device` play a listen mode (compressor key, EQ band solo; see
	/// [`Listen`]) instead of its normal output, or back to normal with [`Listen::Off`].
	/// One device listens at a time (a new one replaces the previous). Monitoring only:
	/// kept across loads, never in the document, offline renders or exports.
	pub fn set_listen(&mut self, device: DeviceId, mode: Listen) -> Result<(), EngineError> {
		let target = (mode != Listen::Off).then_some((device, mode));
		if target.is_none() && self.listen.is_some_and(|(d, _)| d != device) {
			return Ok(());
		}
		if self.listen == target {
			return Ok(());
		}
		self.control.send(Cmd::Listen(target))?;
		self.listen = target;
		Ok(())
	}

	/// The device playing a listen mode, if any.
	pub fn listen(&self) -> Option<(DeviceId, Listen)> {
		self.listen
	}

	/// Sets where plugin devices run on subsequent [`Player::load`] calls (default:
	/// [`PluginHosting::default`], sandboxed unless `CADENZA_PLUGIN_HOSTING=in-process`).
	pub fn set_hosting(&mut self, hosting: PluginHosting) {
		self.hosting = hosting;
	}

	pub fn hosting(&self) -> &PluginHosting {
		&self.hosting
	}

	/// Plugins of the loaded project that crashed or hung and are bypassed. Polls the
	/// sandbox monitors; call it from the control thread (it may wait briefly for a dead
	/// plugin process' exit status).
	pub fn plugin_crashes(&self) -> Vec<PluginCrash> {
		self.watches
			.iter()
			.filter_map(|(w, _)| w.crash(PLAYBACK_CRASH_HINT))
			.collect()
	}

	/// Sandboxed plugins of the loaded project that answered late at least once since it
	/// was loaded: each late answer is a short dropout of that plugin only (silence for
	/// instruments, the dry signal for effects), not a crash. Cheap (atomic reads).
	pub fn plugin_xruns(&self) -> Vec<PluginXruns> {
		self.watches.iter().filter_map(|(w, _)| w.xruns()).collect()
	}

	/// Like [`Player::plugin_crashes`], but returns each crash only once (for
	/// notifications).
	pub fn take_plugin_crashes(&mut self) -> Vec<PluginCrash> {
		let mut new = Vec::new();
		for (w, taken) in &mut self.watches {
			if !*taken && let Some(crash) = w.crash(PLAYBACK_CRASH_HINT) {
				*taken = true;
				new.push(crash);
			}
		}
		new
	}

	/// Listens to a MIDI input port (`None` closes the current one). Notes go to the
	/// monitored track (see [`Player::set_monitor_track`]).
	pub fn set_midi_input(&mut self, port: Option<&PortInfo>) -> Result<(), EngineError> {
		if let Some(old) = self.midi_input.take() {
			old.close();
		}
		let Some(port) = port else {
			return self.control.send(Cmd::DetachMidi);
		};
		let (producer, consumer) = cadenza_midi_io::new_queue(MIDI_QUEUE);
		let connection =
			cadenza_midi_io::open_input_port(port, producer, "cadenza").map_err(|e| {
				EngineError::Midi(format!("cannot open MIDI input '{}': {e}", port.name))
			})?;
		self.control.attach_midi(consumer)?;
		self.midi_input = Some(connection);
		Ok(())
	}

	/// Resolves `project.midi_map` (hardware controller assignments) against the loaded
	/// graph and hands it to the audio thread, without recompiling. [`Player::load`] does
	/// it too; call this when only the map changed. Mapped controllers move their targets
	/// live (published in [`Player::midi_map_monitor`]) and do not reach the instrument.
	pub fn set_midi_map(&mut self, project: &Project) -> Result<(), EngineError> {
		let table = MidiMapTable::build(project, self.resolver.as_ref());
		self.control.collect_garbage();
		self.control
			.midi_maps
			.push(Box::new(table))
			.map_err(|_| EngineError::Busy)
	}

	/// What the MIDI controllers did: the last controller message (MIDI learn) and the
	/// values the mapped ones set (one per player, kept across loads).
	pub fn midi_map_monitor(&self) -> Arc<MidiMapMonitor> {
		self.control.midi_monitor.clone()
	}

	/// Feeds the monitored track (and recordings) from `queue` instead of a hardware
	/// port: virtual keyboards, tests.
	pub fn set_midi_queue(&mut self, queue: Consumer<TimedMessage>) -> Result<(), EngineError> {
		if let Some(old) = self.midi_input.take() {
			old.close();
		}
		self.control.attach_midi(queue)
	}

	/// Name of the open MIDI input port.
	pub fn midi_input(&self) -> Option<&str> {
		self.midi_input.as_ref().map(MidiInputConnection::port_name)
	}

	/// MIDI events lost because the audio thread did not drain them in time.
	pub fn dropped_midi_events(&self) -> u64 {
		self.midi_input
			.as_ref()
			.map_or(0, MidiInputConnection::dropped_events)
	}

	/// Plays live MIDI input through the instrument of `track` (an instrument track of the
	/// loaded project; `None` stops monitoring). Kept across [`Player::load`].
	pub fn set_monitor_track(&mut self, track: Option<TrackId>) -> Result<(), EngineError> {
		self.control.send(Cmd::Monitor(track))
	}

	/// Plays `pitch` (0..=127) through the instrument of `track` until
	/// [`Player::preview_note_off`], also while the transport is stopped (only that
	/// instrument and the channels it feeds are rendered then). `track` becomes the
	/// monitored track (see [`Player::set_monitor_track`]); switching to another track
	/// releases the notes held on the previous one. Velocity 0..=1.
	pub fn preview_note_on(
		&mut self,
		track: TrackId,
		pitch: u8,
		velocity: f32,
	) -> Result<(), EngineError> {
		self.control.send(Cmd::Preview(
			track,
			MidiEvent::NoteOn {
				offset: 0,
				pitch: pitch.min(127),
				velocity: velocity.clamp(0.0, 1.0),
			},
		))
	}

	/// Releases a note started with [`Player::preview_note_on`].
	pub fn preview_note_off(&mut self, track: TrackId, pitch: u8) -> Result<(), EngineError> {
		self.control.send(Cmd::Preview(
			track,
			MidiEvent::NoteOff {
				offset: 0,
				pitch: pitch.min(127),
			},
		))
	}

	/// Releases every previewed note of `track`.
	pub fn preview_all_notes_off(&mut self, track: TrackId) -> Result<(), EngineError> {
		self.control
			.send(Cmd::Preview(track, MidiEvent::AllNotesOff { offset: 0 }))
	}

	/// Loops playback over `[start, end)` in song ticks (`None`: no loop), sample
	/// accurately: notes held at the end are released, notes covering the start are
	/// chased, tempo and automation follow the timeline. Kept across [`Player::load`].
	/// Ranges shorter than a few samples are ignored.
	pub fn set_loop(&mut self, range: Option<(Tick, Tick)>) -> Result<(), EngineError> {
		let range = range.filter(|(s, e)| e > s);
		self.loop_range = range;
		self.control.send(Cmd::Loop(range))
	}

	/// The loop set with [`Player::set_loop`].
	pub fn loop_range(&self) -> Option<(Tick, Tick)> {
		self.loop_range
	}

	/// Plays only the tracks of `scope` and the buses they feed (outputs, sends, extra
	/// outputs), without changing the project's mute/solo (`None`: the whole song as
	/// mixed). The mask is built here, off the audio thread; the switch is a few atomic
	/// stores the callback reads once per quantum. Kept across [`Player::load`].
	pub fn set_audition(&mut self, scope: Option<AuditionScope>) {
		if let (Some(state), Some(resolver)) = (&self.audition_state, &self.resolver) {
			state.set(
				scope
					.as_ref()
					.map(|a| resolver.audition_mask(&a.tracks))
					.as_deref(),
			);
		}
		self.audition = scope;
	}

	/// The scope set with [`Player::set_audition`].
	pub fn audition(&self) -> Option<&AuditionScope> {
		self.audition.as_ref()
	}

	/// Replaces the MIDI clips of instrument track `track` in the running graph without
	/// recompiling (the instrument and its effects keep playing; notes removed under the
	/// playhead are released). `clips` are the track's clips after the edit. Returns
	/// `Ok(false)` when that is not possible and the project must be reloaded: nothing
	/// loaded, a different tempo map, or not an instrument track of the loaded graph.
	pub fn set_track_clips(
		&mut self,
		track: TrackId,
		clips: &[Clip],
		tempo: &TempoMap,
	) -> Result<bool, EngineError> {
		let Some(resolver) = &self.resolver else {
			return Ok(false);
		};
		let Some(&node) = resolver.nodes.get(&track) else {
			return Ok(false);
		};
		if *tempo != resolver.tempo || !resolver.instruments.contains(&node) {
			return Ok(false);
		}
		let schedule = MidiSchedule::from_clips(clips, tempo, resolver.sample_rate);
		let events = Vec::with_capacity(crate::compile::event_capacity(&schedule));
		self.control.send_schedule(ScheduleSwap {
			load_id: self.load_id,
			node,
			schedule,
			events,
		})?;
		Ok(true)
	}

	pub fn play(&mut self) -> Result<(), EngineError> {
		self.control.shared.playing.store(true, Ordering::Relaxed);
		self.control.send(Cmd::Play)
	}

	/// Stops playback; sounding notes receive AllNotesOff.
	pub fn stop(&mut self) -> Result<(), EngineError> {
		self.control.shared.playing.store(false, Ordering::Relaxed);
		self.control.send(Cmd::Stop)
	}

	/// Moves the playhead; sounding notes receive AllNotesOff and effect tails are cleared.
	pub fn seek(&mut self, tick: Tick) -> Result<(), EngineError> {
		self.control
			.shared
			.position
			.store(tick.0, Ordering::Relaxed);
		self.control.send(Cmd::Seek(tick))
	}

	/// Current playhead (updated by the audio thread once per callback).
	pub fn position(&self) -> Tick {
		Tick(self.control.shared.position.load(Ordering::Relaxed))
	}

	pub fn is_playing(&self) -> bool {
		self.control.shared.playing.load(Ordering::Relaxed)
	}
}

impl Drop for Player {
	fn drop(&mut self) {
		for device in self.editors.open.keys() {
			if let Some(slot) = self.editors.slots.get(device) {
				let _ = slot.editor.close();
			}
		}
	}
}

/// Environment variable with the `SCHED_FIFO` priority of the output callback thread
/// (default [`AUDIO_RT_PRIORITY`]; `0` keeps normal scheduling).
pub const AUDIO_RT_PRIORITY_ENV: &str = "CADENZA_AUDIO_RT_PRIORITY";

/// The output thread's default realtime priority: the sandboxed plugins' audio threads
/// use the same, so neither starves the other while they hand blocks back and forth.
pub const AUDIO_RT_PRIORITY: u32 = 10;

/// Raises the output callback thread to realtime scheduling from a helper thread (the
/// promotion may wait on the system bus, which the callback itself must never do). Waits
/// for the first callback to publish its thread id; failing is fine: playback works at
/// normal priority, just with less headroom. Logs one line either way.
fn promote_output_thread(shared: Arc<Shared>, result: Arc<Mutex<Option<String>>>) {
	let priority = std::env::var(AUDIO_RT_PRIORITY_ENV)
		.ok()
		.and_then(|v| v.trim().parse::<u32>().ok())
		.unwrap_or(AUDIO_RT_PRIORITY);
	let spawned = std::thread::Builder::new()
		.name("cadenza-audio-rt".into())
		.spawn(move || {
			let deadline = Instant::now() + Duration::from_secs(5);
			let tid = loop {
				let tid = shared.audio_thread.load(Ordering::Relaxed);
				if tid != 0 {
					break tid;
				}
				if Instant::now() > deadline || Arc::strong_count(&shared) == 1 {
					return;
				}
				std::thread::sleep(Duration::from_millis(5));
			};
			let scheduling = cadenza_plugin_host::realtime::promote_thread(tid, priority);
			eprintln!("cadenza: audio output thread: {scheduling}");
			*result.lock().unwrap_or_else(|p| p.into_inner()) = Some(scheduling.to_string());
		});
	if let Err(e) = spawned {
		eprintln!("cadenza: audio output thread keeps normal priority ({e})");
	}
}

fn build<T: SizedSample + FromSample<f32>>(
	device: &cpal::Device,
	config: cpal::StreamConfig,
	channels: usize,
	mut core: AudioCore,
	output_latency: Arc<AtomicU64>,
	shared: Arc<Shared>,
) -> Result<cpal::Stream, EngineError> {
	let rate = f64::from(config.sample_rate);
	device
		.build_output_stream(
			config,
			move |data: &mut [T], info: &cpal::OutputCallbackInfo| {
				let ts = info.timestamp();
				let playback = u64::try_from(ts.playback.as_nanos()).unwrap_or(0);
				core.playback_ns = (playback > 0).then_some(playback);
				if let Some(d) = ts.playback.checked_duration_since(ts.callback) {
					let frames = (d.as_secs_f64() * rate) as u64;
					output_latency.store(frames, Ordering::Relaxed);
				}
				core.fill_interleaved(data, channels)
			},
			move |err: cpal::Error| {
				// Counted for the UI's xrun indicator; recovery is cpal's job.
				if err.kind() == cpal::ErrorKind::Xrun {
					shared.device_xruns.fetch_add(1, Ordering::Relaxed);
				}
			},
			None,
		)
		.map_err(|e| EngineError::Audio(e.to_string()))
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::compile;
	use cadenza_core::PPQ;
	use cadenza_model::{Clip, Device, Note, Track, TrackKind};
	use rtrb::Producer;

	fn project() -> Project {
		let mut p = Project::new("t");
		let mut t = Track::new("synth", TrackKind::Instrument)
			.with_instrument(Device::builtin("poly_synth", "Synth"));
		t.clips.push(
			Clip::midi("c", Tick::ZERO, Tick(PPQ * 8)).with_notes([Note::new(
				60,
				Tick::ZERO,
				Tick(PPQ * 8),
				0.8,
			)]),
		);
		p.tracks.push(t);
		p
	}

	#[test]
	fn graphs_are_swapped_and_returned_off_the_audio_thread() {
		let (mut control, mut core) = channel_pair(48_000);
		let p = project();
		control
			.load_graph(compile(&p, 48_000, 256).unwrap())
			.unwrap();
		let mut out = vec![0.0f32; 512];
		core.fill_interleaved(&mut out, 2);
		assert!(core.graph.is_some());
		assert!(
			out.iter().all(|&x| x == 0.0),
			"stopped player must be silent"
		);

		control.send(Cmd::Play).unwrap();
		core.fill_interleaved(&mut out, 2);
		assert!(out.iter().any(|&x| x != 0.0), "playing produces sound");
		// 256 frames at 120 bpm / 48 kHz = 10.24 ticks.
		assert_eq!(control.shared.position.load(Ordering::Relaxed), 10);

		control
			.load_graph(compile(&p, 48_000, 256).unwrap())
			.unwrap();
		core.fill_interleaved(&mut out, 2);
		// The old graph came back for dropping on the control thread.
		assert_eq!(control.garbage.slots(), 1);
		control.collect_garbage();
		assert!(core.graph.as_ref().unwrap().is_playing());
	}

	#[test]
	fn the_first_callback_publishes_its_thread_for_promotion() {
		let (player, mut dev) = Player::virtual_device(48_000, 0);
		assert_eq!(
			player.control.shared.audio_thread.load(Ordering::Relaxed),
			0
		);
		let mut out = vec![0.0f32; 256];
		dev.process(&mut out, &[]);
		assert_eq!(
			player.control.shared.audio_thread.load(Ordering::Relaxed),
			cadenza_plugin_host::realtime::current_thread_id()
		);
		assert_eq!(
			player.audio_scheduling(),
			None,
			"no device: nothing promoted"
		);
		// Promotion can be switched off; it never touches a thread then.
		let off = cadenza_plugin_host::realtime::promote_thread(
			cadenza_plugin_host::realtime::current_thread_id(),
			0,
		);
		assert_eq!(off.to_string(), "normal priority (disabled)");
	}

	#[test]
	fn a_preview_plays_its_track_then_gives_the_live_input_back() {
		let mut p = project();
		let mut b = Track::new("b", TrackKind::Instrument)
			.with_instrument(Device::builtin("electric_piano", "EP"));
		b.clips.clear();
		let (a_id, b_id) = (p.tracks[0].id, b.id);
		p.tracks.push(b);
		let (mut control, mut core) = channel_pair(48_000);
		control
			.load_graph(compile(&p, 48_000, 256).unwrap())
			.unwrap();
		let mut out = vec![0.0f32; 512];
		core.fill_interleaved(&mut out, 2);
		let node = |core: &AudioCore, id| {
			let g = core.graph.as_ref().unwrap();
			g.monitor.map(|i| g.nodes[i].track_id) == Some(id)
		};
		// Track a is armed (monitored); the piano roll previews on track b.
		control.send(Cmd::Monitor(Some(a_id))).unwrap();
		core.fill_interleaved(&mut out, 2);
		assert!(node(&core, a_id));
		let on = MidiEvent::NoteOn {
			offset: 0,
			pitch: 60,
			velocity: 0.8,
		};
		control.send(Cmd::Preview(b_id, on)).unwrap();
		core.fill_interleaved(&mut out, 2);
		assert!(node(&core, b_id), "the previewed track plays");
		assert!(out.iter().any(|x| *x != 0.0));
		// An arm change meanwhile waits for the preview to end.
		control.send(Cmd::Monitor(Some(a_id))).unwrap();
		core.fill_interleaved(&mut out, 2);
		assert!(node(&core, b_id));
		let off = MidiEvent::NoteOff {
			offset: 0,
			pitch: 60,
		};
		control.send(Cmd::Preview(b_id, off)).unwrap();
		core.fill_interleaved(&mut out, 2);
		core.fill_interleaved(&mut out, 2);
		assert!(node(&core, a_id), "live MIDI goes back to the armed track");
	}

	#[test]
	fn device_xruns_are_counted_for_the_ui() {
		let (player, dev) = Player::virtual_device(48_000, 0);
		assert_eq!(player.device_xruns(), 0);
		dev.report_xrun();
		dev.report_xrun();
		assert_eq!(player.device_xruns(), 2);
	}

	#[test]
	fn seek_and_stop_update_position() {
		let (mut control, mut core) = channel_pair(48_000);
		control
			.load_graph(compile(&project(), 48_000, 256).unwrap())
			.unwrap();
		control.send(Cmd::Seek(Tick(PPQ * 2))).unwrap();
		control.send(Cmd::Play).unwrap();
		let mut out = vec![0.0f32; 2 * 4800];
		core.fill_interleaved(&mut out, 2);
		// 4800 frames at 120 bpm / 48 kHz = 0.2 beats.
		let pos = control.shared.position.load(Ordering::Relaxed);
		assert_eq!(pos, PPQ * 2 + PPQ / 5);
		control.send(Cmd::Stop).unwrap();
		core.fill_interleaved(&mut out, 2);
		assert!(out.iter().all(|&x| x == 0.0));
		assert_eq!(control.shared.position.load(Ordering::Relaxed), pos);
	}

	#[test]
	fn meters_follow_the_audio_thread() {
		let p = project();
		let track = p.tracks[0].id;
		let (mut control, mut core) = channel_pair(48_000);
		let graph = compile(&p, 48_000, 256).unwrap();
		let meters = graph.meters();
		control.load_graph(graph).unwrap();
		let mut out = vec![0.0f32; 2 * 4800];
		core.fill_interleaved(&mut out, 2);
		assert_eq!(meters.master().max_peak(), 0.0, "stopped: silent meters");

		control.send(Cmd::Play).unwrap();
		core.fill_interleaved(&mut out, 2);
		let master = meters.master();
		let out_peak = out.iter().fold(0.0f32, |m, x| m.max(x.abs()));
		assert!(master.max_peak() > 0.0 && master.rms[0] > 0.0);
		assert!(
			(master.max_peak() - out_peak).abs() < 0.05,
			"{master:?} vs {out_peak}"
		);
		assert!(meters.track(track).unwrap().max_peak() > 0.0);

		control.send(Cmd::Stop).unwrap();
		core.fill_interleaved(&mut out, 2);
		let after = meters.master();
		assert_eq!(after.rms, [0.0, 0.0], "stopping clears the levels");
	}

	#[test]
	fn live_volume_changes_the_level_without_reloading() {
		let p = project();
		let track = p.tracks[0].id;
		let (mut control, mut core) = channel_pair(48_000);
		control
			.load_graph(compile(&p, 48_000, 256).unwrap())
			.unwrap();
		control.send(Cmd::Play).unwrap();
		let mut out = vec![0.0f32; 2 * 4800];
		core.fill_interleaved(&mut out, 2);
		let before = energy(&out);
		control.send(Cmd::Volume(Some(track), -40.0)).unwrap();
		core.fill_interleaved(&mut out, 2);
		core.fill_interleaved(&mut out, 2);
		assert!(energy(&out) < before * 1e-3, "{} vs {before}", energy(&out));
		control.send(Cmd::Volume(None, -200.0)).unwrap();
		control.send(Cmd::Pan(Some(TrackId::new()), 1.0)).unwrap();
		core.fill_interleaved(&mut out, 2);
		core.fill_interleaved(&mut out, 2);
		assert!(energy(&out) < 1e-9);
	}

	/// A synth through a `gain` insert and a pre-fader send to a bus.
	fn gain_project() -> (Project, DeviceId, DeviceId) {
		let mut p = project();
		let gain = Device::builtin("gain", "Trim");
		let id = gain.id;
		p.tracks[0].channel.inserts.push(gain);
		let bus = Track::new("bus", TrackKind::Bus);
		let mut send = cadenza_model::Send::new(bus.id, 0.0);
		send.pre_fader = true;
		let send_id = send.id;
		p.tracks[0].channel.sends.push(send);
		p.tracks[0].channel.volume_db = -144.0;
		p.tracks.push(bus);
		(p, id, send_id)
	}

	#[test]
	fn live_params_and_send_levels_change_the_running_graph() {
		let (p, gain, send) = gain_project();
		let (mut control, mut core) = channel_pair(48_000);
		let graph = compile(&p, 48_000, 256).unwrap();
		let index = graph.param_index(gain, "gain").unwrap() as u32;
		control.load_graph(graph).unwrap();
		control.send(Cmd::Play).unwrap();
		let mut out = vec![0.0f32; 2 * 4800];
		core.fill_interleaved(&mut out, 2);
		// Only the send is audible (the track fader is down).
		let before = energy(&out);
		assert!(before > 0.0);
		control.send(Cmd::SendLevel(send, -40.0)).unwrap();
		core.fill_interleaved(&mut out, 2);
		core.fill_interleaved(&mut out, 2);
		let quieter = energy(&out);
		assert!(quieter < before * 1e-3, "{quieter} vs {before}");
		control.send(Cmd::SendLevel(send, 0.0)).unwrap();
		control.send(Cmd::Param(gain, index, -40.0)).unwrap();
		core.fill_interleaved(&mut out, 2);
		core.fill_interleaved(&mut out, 2);
		assert!(energy(&out) < before * 1e-3, "gain insert follows live");
		// Unknown ids are ignored, and the graph kept playing (no reload, no reset).
		control.send(Cmd::Param(DeviceId::new(), 0, 1.0)).unwrap();
		control.send(Cmd::SendLevel(DeviceId::new(), 1.0)).unwrap();
		core.fill_interleaved(&mut out, 2);
		assert!(core.graph.as_ref().unwrap().is_playing());
		assert_eq!(control.garbage.slots(), 0, "no graph swapped");
	}

	#[test]
	fn automation_sets_swap_without_reloading() {
		let (mut p, gain, _) = gain_project();
		p.tracks[0].channel.volume_db = 0.0;
		let track = p.tracks[0].id;
		let (mut control, mut core) = channel_pair(48_000);
		let graph = compile(&p, 48_000, 256).unwrap();
		let lanes = [AutomationLane::new(
			cadenza_core::ParamAddress::device(Some(track), gain, "gain"),
			vec![cadenza_model::AutomationPoint::new(Tick::ZERO, -60.0)],
		)];
		let bindings = graph.prepare_automation(&lanes, &p.tempo);
		control.load_graph(graph).unwrap();
		control.send(Cmd::Play).unwrap();
		let mut out = vec![0.0f32; 2 * 4800];
		core.fill_interleaved(&mut out, 2);
		let before = energy(&out);
		control
			.send_automation(AutomationSet {
				load_id: 0,
				bindings,
			})
			.unwrap();
		for _ in 0..3 {
			core.fill_interleaved(&mut out, 2);
		}
		assert!(energy(&out) < before * 1e-4, "{} vs {before}", energy(&out));
		// The old (empty) set comes back to be dropped here.
		assert_eq!(control.automation_garbage.slots(), 1);
		control.collect_garbage();

		// A set for an older graph is returned unused; one for a newer graph waits.
		control
			.send_automation(AutomationSet {
				load_id: 7,
				bindings: Default::default(),
			})
			.unwrap();
		core.fill_interleaved(&mut out, 2);
		assert!(core.pending_automation.is_some());
		let mut next = compile(&p, 48_000, 256).unwrap();
		next.load_id = 7;
		control.load_graph(next).unwrap();
		core.fill_interleaved(&mut out, 2);
		assert!(core.pending_automation.is_none());
		assert_eq!(core.graph.as_ref().unwrap().load_id, 7);
		assert!(core.graph.as_ref().unwrap().automation.is_empty());
		control
			.send_automation(AutomationSet {
				load_id: 3,
				bindings: Default::default(),
			})
			.unwrap();
		core.fill_interleaved(&mut out, 2);
		assert!(core.pending_automation.is_none());
		assert!(control.automation_garbage.slots() > 0, "stale set returned");
	}

	#[test]
	fn mono_and_multichannel_layouts() {
		let (mut control, mut core) = channel_pair(48_000);
		control
			.load_graph(compile(&project(), 48_000, 256).unwrap())
			.unwrap();
		control.send(Cmd::Play).unwrap();
		let mut mono = vec![0i16; 300];
		core.fill_interleaved(&mut mono, 1);
		let mut quad = vec![0.0f32; 4 * 300];
		core.fill_interleaved(&mut quad, 4);
		assert!(quad.chunks(4).all(|f| f[2] == 0.0 && f[3] == 0.0));
	}

	fn live_project() -> (Project, TrackId, TrackId) {
		let mut p = Project::new("live");
		let keys = Track::new("keys", TrackKind::Instrument)
			.with_instrument(Device::builtin("poly_synth", "Synth"));
		let other = Track::new("other", TrackKind::Instrument)
			.with_instrument(Device::builtin("poly_synth", "Synth"));
		let ids = (keys.id, other.id);
		p.tracks.push(keys);
		p.tracks.push(other);
		(p, ids.0, ids.1)
	}

	fn note(producer: &mut Producer<TimedMessage>, bytes: &[u8]) {
		producer
			.push(TimedMessage {
				host_micros: 0,
				message: cadenza_midi_io::RawMessage::from_bytes(bytes).unwrap(),
			})
			.unwrap();
	}

	fn energy(out: &[f32]) -> f32 {
		out.iter().map(|x| x * x).sum()
	}

	#[test]
	fn live_midi_plays_through_the_monitored_instrument() {
		let (p, keys, other) = live_project();
		let (mut control, mut core) = channel_pair(48_000);
		control
			.load_graph(compile(&p, 48_000, 256).unwrap())
			.unwrap();
		let (mut producer, consumer) = cadenza_midi_io::new_queue(MIDI_QUEUE);
		control.attach_midi(consumer).unwrap();
		control.send(Cmd::Monitor(Some(keys))).unwrap();
		control.send(Cmd::Play).unwrap();
		let mut out = vec![0.0f32; 512];
		core.fill_interleaved(&mut out, 2);
		assert_eq!(energy(&out), 0.0, "no input, no sound");

		note(&mut producer, &[0x90, 60, 100]);
		core.fill_interleaved(&mut out, 2);
		core.fill_interleaved(&mut out, 2);
		assert!(energy(&out) > 0.0, "a live note sounds");
		let graph = core.graph.as_ref().unwrap();
		assert_eq!(graph.monitor_track(), Some(keys));

		// Reloading keeps the monitor; the new graph receives the input.
		control
			.load_graph(compile(&p, 48_000, 256).unwrap())
			.unwrap();
		core.fill_interleaved(&mut out, 2);
		assert_eq!(core.graph.as_ref().unwrap().monitor_track(), Some(keys));
		note(&mut producer, &[0x90, 64, 100]);
		for _ in 0..4 {
			core.fill_interleaved(&mut out, 2);
		}
		assert!(energy(&out) > 0.0);

		// Switching tracks releases the notes held on the old one.
		control.send(Cmd::Monitor(Some(other))).unwrap();
		for _ in 0..200 {
			core.fill_interleaved(&mut out, 2);
		}
		assert!(energy(&out) < 1e-6, "old notes released: {}", energy(&out));

		// Detaching hands the queue back to the control side.
		control.send(Cmd::DetachMidi).unwrap();
		core.fill_interleaved(&mut out, 2);
		assert!(core.midi.is_none());
		assert_eq!(
			control.midi_garbage.slots(),
			1,
			"queue returned for dropping"
		);
		control.collect_garbage();
		assert!(producer.is_abandoned());
	}

	#[test]
	fn live_midi_is_discarded_while_unmonitored_and_previewed_while_stopped() {
		let (p, keys, _) = live_project();
		let (mut control, mut core) = channel_pair(48_000);
		control
			.load_graph(compile(&p, 48_000, 256).unwrap())
			.unwrap();
		let (mut producer, consumer) = cadenza_midi_io::new_queue(MIDI_QUEUE);
		control.attach_midi(consumer).unwrap();
		let mut out = vec![0.0f32; 512];
		// Playing, but nothing monitored.
		control.send(Cmd::Play).unwrap();
		note(&mut producer, &[0x90, 60, 100]);
		core.fill_interleaved(&mut out, 2);
		core.fill_interleaved(&mut out, 2);
		assert_eq!(energy(&out), 0.0);
		// Monitored and stopped: the instrument is previewed, the playhead stays.
		control.send(Cmd::Monitor(Some(keys))).unwrap();
		control.send(Cmd::Stop).unwrap();
		let pos = control.shared.position.load(Ordering::Relaxed);
		note(&mut producer, &[0x90, 62, 100]);
		core.fill_interleaved(&mut out, 2);
		core.fill_interleaved(&mut out, 2);
		assert_eq!(producer.slots(), MIDI_QUEUE, "queue drained");
		assert!(energy(&out) > 0.0, "stopped monitoring previews the note");
		assert_eq!(control.shared.position.load(Ordering::Relaxed), pos);
		// Playing again starts clean: the previewed note is cut, the song is empty.
		control.send(Cmd::Play).unwrap();
		for _ in 0..200 {
			core.fill_interleaved(&mut out, 2);
		}
		assert!(energy(&out) < 1e-6, "{}", energy(&out));
	}

	#[test]
	fn note_preview_plays_while_stopped_and_releases() {
		let p = project();
		let track = p.tracks[0].id;
		let (mut control, mut core) = channel_pair(48_000);
		control
			.load_graph(compile(&p, 48_000, 256).unwrap())
			.unwrap();
		let mut out = vec![0.0f32; 512];
		core.fill_interleaved(&mut out, 2);
		assert_eq!(energy(&out), 0.0, "stopped: silent");
		control
			.send(Cmd::Preview(
				track,
				MidiEvent::NoteOn {
					offset: 0,
					pitch: 72,
					velocity: 0.9,
				},
			))
			.unwrap();
		core.fill_interleaved(&mut out, 2);
		core.fill_interleaved(&mut out, 2);
		assert!(energy(&out) > 0.0, "previewed note sounds while stopped");
		assert_eq!(control.shared.position.load(Ordering::Relaxed), 0);
		assert_eq!(core.graph.as_ref().unwrap().monitor_track(), Some(track));
		// Held: keeps sounding well past the release window.
		for _ in 0..400 {
			core.fill_interleaved(&mut out, 2);
		}
		assert!(core.graph.as_ref().unwrap().is_previewing());
		assert!(energy(&out) > 0.0);
		control
			.send(Cmd::Preview(
				track,
				MidiEvent::NoteOff {
					offset: 0,
					pitch: 72,
				},
			))
			.unwrap();
		// 3 s of 256-frame callbacks: the release and the tail end, then silence.
		for _ in 0..600 {
			core.fill_interleaved(&mut out, 2);
		}
		assert!(!core.graph.as_ref().unwrap().is_previewing());
		assert_eq!(energy(&out), 0.0);
		// The song still plays from the playhead afterwards (chased note at tick 0).
		control.send(Cmd::Play).unwrap();
		core.fill_interleaved(&mut out, 2);
		core.fill_interleaved(&mut out, 2);
		assert!(energy(&out) > 0.0);
	}

	#[test]
	fn the_loop_survives_reloads_and_wraps_the_position() {
		let p = project();
		let (mut control, mut core) = channel_pair(48_000);
		control
			.load_graph(compile(&p, 48_000, 256).unwrap())
			.unwrap();
		// One beat = 24_000 frames at 120 bpm.
		control
			.send(Cmd::Loop(Some((Tick::ZERO, Tick(PPQ)))))
			.unwrap();
		control.send(Cmd::Play).unwrap();
		let mut out = vec![0.0f32; 2 * 1000];
		for _ in 0..30 {
			core.fill_interleaved(&mut out, 2);
		}
		// 30_000 frames played: 6_000 into the second pass (= 240 ticks).
		assert_eq!(control.shared.position.load(Ordering::Relaxed), PPQ / 4);
		control
			.load_graph(compile(&p, 48_000, 256).unwrap())
			.unwrap();
		for _ in 0..20 {
			core.fill_interleaved(&mut out, 2);
		}
		let graph = core.graph.as_ref().unwrap();
		assert_eq!(graph.loop_samples(), Some((0, 24_000)), "kept on reload");
		assert!(control.shared.position.load(Ordering::Relaxed) < PPQ);
		control.send(Cmd::Loop(None)).unwrap();
		core.fill_interleaved(&mut out, 2);
		assert_eq!(core.graph.as_ref().unwrap().loop_samples(), None);
	}

	#[test]
	fn rescheduled_tracks_swap_in_without_reloading() {
		let p = project();
		let track = p.tracks[0].id;
		let (mut control, mut core) = channel_pair(48_000);
		let graph = compile(&p, 48_000, 256).unwrap();
		let mut edited = p.tracks[0].clips.clone();
		if let cadenza_model::ClipContent::Midi { notes, .. } = &mut edited[0].content {
			notes.clear();
		}
		let swap = graph.prepare_schedule(track, &edited, &p.tempo).unwrap();
		control.load_graph(graph).unwrap();
		control.send(Cmd::Play).unwrap();
		let mut out = vec![0.0f32; 2 * 4800];
		core.fill_interleaved(&mut out, 2);
		assert!(energy(&out) > 0.0);
		control.send_schedule(swap).unwrap();
		// The removed note is released (no reload: the graph keeps playing).
		for _ in 0..20 {
			core.fill_interleaved(&mut out, 2);
		}
		assert!(energy(&out) < 1e-9, "{}", energy(&out));
		assert_eq!(control.garbage.slots(), 0, "no graph swapped");
		assert_eq!(control.schedule_garbage.slots(), 1, "old schedule returned");
		control.collect_garbage();

		// A swap for a graph still queued waits for it; a stale one goes back.
		let mut next = compile(&p, 48_000, 256).unwrap();
		next.load_id = 5;
		let mut swap = next.prepare_schedule(track, &edited, &p.tempo).unwrap();
		swap.load_id = 5;
		control.send_schedule(swap).unwrap();
		core.fill_interleaved(&mut out, 2);
		assert!(core.pending_schedule.is_some());
		control.load_graph(next).unwrap();
		core.fill_interleaved(&mut out, 2);
		assert!(core.pending_schedule.is_none());
		let stale = compile(&p, 48_000, 256)
			.unwrap()
			.prepare_schedule(track, &edited, &p.tempo)
			.unwrap();
		control.send_schedule(stale).unwrap();
		core.fill_interleaved(&mut out, 2);
		assert!(control.schedule_garbage.slots() > 0);
	}

	/// Needs a MIDI input device; run with `cargo test -- --ignored`.
	#[test]
	#[ignore]
	fn hardware_midi_input_opens() {
		let ports = midi_input_ports().unwrap();
		let Some(port) = ports.first() else { return };
		let mut player = Player::new().unwrap();
		player.set_midi_input(Some(port)).unwrap();
		assert_eq!(player.midi_input(), Some(port.name.as_str()));
		player.set_midi_input(None).unwrap();
	}
}
