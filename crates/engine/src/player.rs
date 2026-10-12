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

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::time::{Duration, Instant};

use std::collections::HashSet;

use cadenza_core::{DeviceId, TempoMap, Tick, TrackId};
use cadenza_dsp::MidiEvent;
use cadenza_midi_io::{MappedEvent, MidiInputConnection, PortInfo, TimedMessage};
use cadenza_model::{AutomationLane, Clip, Project};
use cadenza_plugin_host::PluginCatalog;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};
use rtrb::{Consumer, Producer, RingBuffer};

use crate::automation::{AutoTarget, AutomationSet, Resolver};
use crate::compile_hosted;
use crate::error::EngineError;
use crate::graph::{Audition, Graph, ScheduleSwap};
use crate::hosting::{PluginCrash, PluginHosting, PluginWatch};
use crate::meter::MeterBank;
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
	playing: bool,
	position: Tick,
	loop_range: Option<(Tick, Tick)>,
	scratch_l: Vec<f32>,
	scratch_r: Vec<f32>,
	/// Live MIDI input queue, if a port is attached.
	midi: Option<Consumer<TimedMessage>>,
	midi_inputs: Consumer<Consumer<TimedMessage>>,
	midi_garbage: Producer<Consumer<TimedMessage>>,
	mapped: Vec<MappedEvent>,
	monitor: Option<TrackId>,
	sample_rate: f64,
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
			playing: false,
			position: Tick::ZERO,
			loop_range: None,
			scratch_l: vec![0.0; SCRATCH_FRAMES],
			scratch_r: vec![0.0; SCRATCH_FRAMES],
			midi: None,
			midi_inputs: midi_rx,
			midi_garbage: midi_garbage_tx,
			mapped: Vec::with_capacity(MIDI_QUEUE),
			monitor: None,
			sample_rate: f64::from(sample_rate),
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
			next.seek(self.position);
			if self.playing {
				next.play();
			}
			next.set_monitor_track(self.monitor);
			if let Some(old) = self.graph.replace(next) {
				// Cannot fail: a slot was checked above and only this thread pushes.
				let _ = self.garbage.push(old);
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
					if let Some(g) = &mut self.graph {
						g.set_monitor_track(track);
					}
				}
				Cmd::DetachMidi => self.retire_midi(),
				Cmd::Volume(track, db) => {
					if let Some(g) = &mut self.graph {
						g.set_channel_volume(track, db);
					}
				}
				Cmd::Pan(track, pan) => {
					if let Some(g) = &mut self.graph {
						g.set_channel_pan(track, pan);
					}
				}
				Cmd::Param(device, index, value) => {
					if let Some(g) = &mut self.graph {
						g.set_device_param(device, index as usize, value);
					}
				}
				Cmd::SendLevel(send, db) => {
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
				Cmd::Preview(track, event) => {
					if self.monitor != Some(track) {
						self.monitor = Some(track);
						if let Some(g) = &mut self.graph {
							g.set_monitor_track(Some(track));
						}
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
		if let Some(g) = &mut self.graph {
			for event in self.mapped.iter().filter_map(|m| m.dsp) {
				g.push_live_event(event);
			}
		}
	}

	fn end_block(&mut self) {
		if let Some(g) = &self.graph {
			self.position = g.position();
		}
		self.shared
			.position
			.store(self.position.0, Ordering::Relaxed);
	}

	/// Renders `frames` frames into the scratch buffers, `SCRATCH_FRAMES` at most.
	fn render(&mut self, frames: usize) {
		let (l, r) = (&mut self.scratch_l[..frames], &mut self.scratch_r[..frames]);
		match &mut self.graph {
			Some(g) => g.process(l, r),
			None => {
				l.fill(0.0);
				r.fill(0.0);
			}
		}
	}

	fn fill_interleaved<T: SizedSample + FromSample<f32>>(
		&mut self,
		data: &mut [T],
		channels: usize,
	) {
		self.begin_block();
		let channels = channels.max(1);
		for chunk in data.chunks_mut(SCRATCH_FRAMES * channels) {
			let frames = chunk.len() / channels;
			self.render(frames);
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
	_stream: cpal::Stream,
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
		let host = cpal::default_host();
		let device = host
			.default_output_device()
			.ok_or(EngineError::NoOutputDevice)?;
		let supported = device
			.default_output_config()
			.map_err(|e| EngineError::Audio(e.to_string()))?;
		let format = supported.sample_format();
		let config: cpal::StreamConfig = supported.config();
		let channels = config.channels as usize;
		let (control, core) = channel_pair(config.sample_rate);
		let stream = match format {
			SampleFormat::F32 => build::<f32>(&device, config, channels, core),
			SampleFormat::F64 => build::<f64>(&device, config, channels, core),
			SampleFormat::I16 => build::<i16>(&device, config, channels, core),
			SampleFormat::I32 => build::<i32>(&device, config, channels, core),
			SampleFormat::U16 => build::<u16>(&device, config, channels, core),
			other => Err(EngineError::Audio(format!(
				"unsupported output sample format {other}"
			))),
		}?;
		stream
			.play()
			.map_err(|e| EngineError::Audio(e.to_string()))?;
		Ok(Self {
			control,
			sample_rate: config.sample_rate,
			plugins: None,
			hosting: PluginHosting::default(),
			watches: Vec::new(),
			midi_input: None,
			meters: None,
			resolver: None,
			automated: HashSet::new(),
			load_id: 0,
			audition_state: None,
			audition: None,
			loop_range: None,
			_stream: stream,
		})
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
			.and_then(|r| r.param_index(device, param))
		else {
			return Ok(false);
		};
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
		let mut graph = compile_hosted(
			project,
			self.sample_rate,
			crate::QUANTUM,
			self.plugins.as_deref(),
			&self.hosting,
		)?;
		let warnings = graph.warnings().to_vec();
		let watches = graph.plugin_watches.clone();
		let meters = graph.meters();
		let resolver = graph.resolver.clone();
		let automated = crate::automation::targets(&graph.automation);
		graph.load_id = self.load_id + 1;
		let audition = graph.audition.clone();
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
		Ok(warnings)
	}

	/// Live per-track and master levels of the loaded project, updated by the audio
	/// thread once per callback without locks or allocation (`None` before the first
	/// [`Player::load`]). The bank changes on every load; keep the `Arc` only until then.
	pub fn meters(&self) -> Option<Arc<MeterBank>> {
		self.meters.clone()
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

fn build<T: SizedSample + FromSample<f32>>(
	device: &cpal::Device,
	config: cpal::StreamConfig,
	channels: usize,
	mut core: AudioCore,
) -> Result<cpal::Stream, EngineError> {
	device
		.build_output_stream(
			config,
			move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
				core.fill_interleaved(data, channels)
			},
			|_err| {},
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
		if let cadenza_model::ClipContent::Midi { notes } = &mut edited[0].content {
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
