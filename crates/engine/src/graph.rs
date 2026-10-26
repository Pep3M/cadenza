//! The compiled render graph. Everything here runs on the audio thread (or the offline
//! render loop, which uses exactly the same code): no allocation, locks or I/O after
//! [`crate::compile`] returns.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use cadenza_core::{DeviceId, PPQ, TempoMap, Tick, TrackId};
use cadenza_dsp::{Listen, MidiEvent, Processor, Transport};
use cadenza_model::{AutomationLane, MonitorMode};

use crate::QUANTUM;
use crate::audio_clip::{self, ClipPlayback};
use crate::automation::{AutoTarget, AutomationBindings, Binding, DeviceSlot, Resolver};
use crate::hosting::{PluginCrash, PluginEditorSlot, PluginWatch, PluginXruns};
use crate::input::{InputFifo, MAX_INPUT_CHANNELS};
use crate::meter::{MeterBank, Meters};
use crate::metronome::{
	Click, MAX_CLICKS, MeterSegment, Metronome, MetronomeSettings, clicks_between, meter_at,
	meter_clicks,
};
use crate::modulation::Modulation;
use crate::probe::DeviceProbe;
use crate::schedule::MidiSchedule;
use cadenza_model::modulation::MAX_MODULATORS;

/// Which tracks are heard while auditioning (see [`Graph::set_audition`]): written by the
/// control side, read by the audio thread once per quantum. Wait-free and allocation
/// free on both sides; a change lands at the next quantum.
#[derive(Debug)]
pub(crate) struct Audition {
	active: AtomicBool,
	nodes: Box<[AtomicBool]>,
}

impl Audition {
	pub fn new(nodes: usize) -> Self {
		Self {
			active: AtomicBool::new(false),
			nodes: (0..nodes).map(|_| AtomicBool::new(false)).collect(),
		}
	}

	/// `None` plays the song as mixed; `Some(mask)` (one flag per node in processing
	/// order) plays only the flagged nodes.
	pub fn set(&self, mask: Option<&[bool]>) {
		match mask {
			None => self.active.store(false, Ordering::Release),
			Some(mask) => {
				for (node, &on) in self.nodes.iter().zip(mask) {
					node.store(on, Ordering::Relaxed);
				}
				self.active.store(true, Ordering::Release);
			}
		}
	}

	fn active(&self) -> bool {
		self.active.load(Ordering::Acquire)
	}

	fn contains(&self, node: usize) -> bool {
		self.nodes
			.get(node)
			.is_some_and(|n| n.load(Ordering::Relaxed))
	}
}

/// Where an audio track's live input comes from (indexes into the input channels).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct NodeInput {
	pub left: usize,
	/// Same as `left` for a mono input.
	pub right: usize,
	pub mode: MonitorMode,
}

/// A metronome count-in in progress (the transport waits while it runs).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct CountIn {
	/// Output frames still to count in.
	pub left: u64,
	pub total: u64,
	/// Frames rendered so far (quanta run ahead of the output).
	pub rendered: u64,
	/// Frames per click.
	pub beat_frames: f64,
	pub per_bar: u32,
}

/// Most segments one [`Graph::process`] call produces (a block split by a wrap).
pub(crate) const MAX_SEGMENTS: usize = 4;

/// A stretch of a processed block and the timeline samples it plays: output frames
/// `offset..offset + len` of the block were heard as timeline samples `tag..tag + len`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Segment {
	pub offset: usize,
	pub tag: i64,
	pub len: usize,
}

/// A loop wrap inside (or at the start of) the quantum being rendered.
#[derive(Debug, Clone, Copy)]
struct Wrap {
	/// Offset in the quantum where playback jumps.
	at: usize,
	/// Timeline sample playback leaves (the loop end)...
	from: i64,
	/// ...and the one it continues from (the loop start).
	to: i64,
}

/// A track's clips rescheduled off the audio thread for [`Graph::swap_schedule`]: only
/// MIDI notes changed, so the running instrument keeps playing.
pub struct ScheduleSwap {
	pub(crate) load_id: u64,
	pub(crate) node: usize,
	pub(crate) schedule: MidiSchedule,
	pub(crate) events: Vec<MidiEvent>,
}

/// Stereo scratch buffer of exactly [`QUANTUM`] samples.
#[derive(Debug, Clone)]
pub(crate) struct StereoBuf {
	pub l: Vec<f32>,
	pub r: Vec<f32>,
}

impl StereoBuf {
	pub fn new() -> Self {
		Self {
			l: vec![0.0; QUANTUM],
			r: vec![0.0; QUANTUM],
		}
	}

	fn clear(&mut self) {
		self.l.fill(0.0);
		self.r.fill(0.0);
	}
}

/// Fixed delay (PDC) on one connection.
#[derive(Debug, Clone)]
pub(crate) struct StereoDelay {
	l: Vec<f32>,
	r: Vec<f32>,
	pos: usize,
}

impl StereoDelay {
	pub fn new(samples: u32) -> Self {
		Self {
			l: vec![0.0; samples as usize],
			r: vec![0.0; samples as usize],
			pos: 0,
		}
	}

	fn clear(&mut self) {
		self.l.fill(0.0);
		self.r.fill(0.0);
		self.pos = 0;
	}

	/// Delays `buf` in place.
	fn process(&mut self, buf: &mut StereoBuf) {
		let len = self.l.len();
		if len == 0 {
			return;
		}
		for i in 0..QUANTUM {
			let (yl, yr) = (self.l[self.pos], self.r[self.pos]);
			self.l[self.pos] = buf.l[i];
			self.r[self.pos] = buf.r[i];
			buf.l[i] = yl;
			buf.r[i] = yr;
			self.pos += 1;
			if self.pos == len {
				self.pos = 0;
			}
		}
	}

	/// Adds `l`/`r` (at most one quantum) to `dst` through the delay, at a constant gain.
	fn mix(&mut self, l: &[f32], r: &[f32], gain: f32, dst: &mut StereoBuf) {
		self.mix_ramp(l, r, gain, gain, dst);
	}

	/// Like [`StereoDelay::mix`], with the gain going linearly from `from` to `to` over
	/// the quantum (click-free level changes).
	fn mix_ramp(&mut self, l: &[f32], r: &[f32], from: f32, to: f32, dst: &mut StereoBuf) {
		let n = l.len().min(r.len()).min(QUANTUM);
		let step = (to - from) / QUANTUM as f32;
		let gain = |i: usize| {
			if from == to {
				to
			} else {
				from + step * (i + 1) as f32
			}
		};
		if self.l.is_empty() {
			for i in 0..n {
				let g = gain(i);
				dst.l[i] += l[i] * g;
				dst.r[i] += r[i] * g;
			}
			return;
		}
		let len = self.l.len();
		for i in 0..QUANTUM {
			let (yl, yr) = (self.l[self.pos], self.r[self.pos]);
			self.l[self.pos] = if i < n { l[i] } else { 0.0 };
			self.r[self.pos] = if i < n { r[i] } else { 0.0 };
			self.pos += 1;
			if self.pos == len {
				self.pos = 0;
			}
			let g = gain(i);
			dst.l[i] += yl * g;
			dst.r[i] += yr * g;
		}
	}
}

/// A connection from a node to a summing accumulator (bus input or master input).
#[derive(Debug, Clone)]
pub(crate) struct Edge {
	pub accumulator: usize,
	/// The send this edge plays (`None`: the track's main output).
	pub send: Option<DeviceId>,
	/// Gain of the last quantum, ramped towards `target_gain` (live/automated send levels).
	pub gain: f32,
	pub target_gain: f32,
	pub pre_fader: bool,
	pub delay: StereoDelay,
}

impl Edge {
	fn mix(&mut self, src: &StereoBuf, dst: &mut StereoBuf) {
		let (from, to) = (self.gain, self.target_gain);
		self.delay.mix_ramp(&src.l, &src.r, from, to, dst);
		self.gain = to;
	}

	fn set_level_db(&mut self, db: f32) {
		if db.is_finite() {
			self.target_gain = cadenza_core::db_to_gain(db);
		}
	}
}

/// An extra instrument output (multi-output plugins) mixed into the instrument's own
/// track or into a bus accumulator.
pub(crate) struct AuxEdge {
	/// 0-based extra output index (`OutputRoute.port - 1`).
	pub port: usize,
	/// `None`: the node's own signal, right after the instrument.
	pub accumulator: Option<usize>,
	pub delay: StereoDelay,
}

/// A device's sidechain input: where the key comes from and how it is aligned.
pub(crate) struct Sidechain {
	/// Node (index in processing order) whose signal keys the device.
	pub source: usize,
	pub pre_fader: bool,
	/// The processor has a key input (false: only an envelope follower listens).
	pub feeds: bool,
	/// Scratch copy of the key for the current quantum.
	pub key: StereoBuf,
	pub key_delay: StereoDelay,
	/// Delay of the chain signal before the device (inserts only).
	pub main_delay: StereoDelay,
}

/// Volume + pan stage with per-sample one-pole smoothing (5 ms) of both channel gains.
///
/// Mono sources (audio tracks whose clips are all mono) use a constant-power pan with a
/// -3 dB center; stereo sources use pan as a balance control (unity at center, the
/// opposite channel fades with a quarter-cosine).
#[derive(Debug, Clone)]
pub(crate) struct Fader {
	volume_db: f32,
	pan: f32,
	mono_source: bool,
	target: (f32, f32),
	gain: (f32, f32),
	coeff: f32,
}

impl Fader {
	pub fn new(volume_db: f32, pan: f32, mono_source: bool, sample_rate: f32) -> Self {
		let mut f = Self {
			volume_db,
			pan,
			mono_source,
			target: (1.0, 1.0),
			gain: (1.0, 1.0),
			coeff: 1.0 - (-1.0 / (0.005 * sample_rate)).exp(),
		};
		f.update_target();
		f.snap();
		f
	}

	fn update_target(&mut self) {
		let vol = cadenza_core::db_to_gain(self.volume_db);
		let pan = if self.pan.is_finite() {
			self.pan.clamp(-1.0, 1.0)
		} else {
			0.0
		};
		// Written with sin() of the distance to the far side so hard pans are exactly 0.
		let quarter = std::f32::consts::FRAC_PI_2;
		let (l, r) = if self.mono_source {
			(
				((1.0 - pan) * quarter * 0.5).sin(),
				((1.0 + pan) * quarter * 0.5).sin(),
			)
		} else if pan >= 0.0 {
			(((1.0 - pan) * quarter).sin(), 1.0)
		} else {
			(1.0, ((1.0 + pan) * quarter).sin())
		};
		self.target = (vol * l, vol * r);
	}

	pub fn set_volume_db(&mut self, db: f32) {
		if db.is_finite() && db != self.volume_db {
			self.volume_db = db;
			self.update_target();
		}
	}

	pub fn set_pan(&mut self, pan: f32) {
		if pan.is_finite() && pan != self.pan {
			self.pan = pan;
			self.update_target();
		}
	}

	pub fn snap(&mut self) {
		self.gain = self.target;
	}

	fn apply(&mut self, buf: &mut StereoBuf) {
		let (tl, tr) = self.target;
		let (mut gl, mut gr) = self.gain;
		if gl == tl && gr == tr {
			for i in 0..QUANTUM {
				buf.l[i] *= gl;
				buf.r[i] *= gr;
			}
			return;
		}
		for i in 0..QUANTUM {
			gl += (tl - gl) * self.coeff;
			gr += (tr - gr) * self.coeff;
			buf.l[i] *= gl;
			buf.r[i] *= gr;
		}
		// Land exactly on the target once inaudibly close, so steady state is bit-exact.
		if (tl - gl).abs() < 1e-7 {
			gl = tl;
		}
		if (tr - gr).abs() < 1e-7 {
			gr = tr;
		}
		self.gain = (gl, gr);
	}
}

pub(crate) struct Device {
	pub id: DeviceId,
	pub processor: Box<dyn Processor>,
	pub sidechain: Option<Box<Sidechain>>,
	pub modulation: Option<Box<Modulation>>,
	/// Effects with a MIDI input: the notes of that track, scheduled like an instrument's.
	pub midi: Option<Box<DeviceMidi>>,
}

/// The MIDI an effect receives from another track (`Device.midi_input` in the model).
pub(crate) struct DeviceMidi {
	pub schedule: MidiSchedule,
	pub events: Vec<MidiEvent>,
}

/// Where the quantum being rendered sits, for collecting scheduled MIDI.
#[derive(Clone, Copy)]
struct QuantumMidi {
	pos: i64,
	wrap: Option<Wrap>,
	all_notes_off: bool,
	preview: bool,
}

impl QuantumMidi {
	/// The quantum's events of `schedule` into `events` (cleared): notes off and chased
	/// after a reset, then the notes, across a loop wrap. Nothing while previewing.
	fn fill(self, schedule: &mut MidiSchedule, events: &mut Vec<MidiEvent>) {
		events.clear();
		if self.all_notes_off {
			events.push(MidiEvent::AllNotesOff { offset: 0 });
		}
		if self.preview {
			return;
		}
		if self.all_notes_off {
			schedule.chase(self.pos, 0, events);
		}
		match self.wrap {
			None => schedule.collect(self.pos, QUANTUM, 0, events),
			Some(w) => {
				let at = w.at as u32;
				schedule.collect(self.pos, w.at, 0, events);
				schedule.release(w.from, at, events);
				schedule.seek(w.to);
				schedule.chase(w.to, at, events);
				schedule.collect(w.to, QUANTUM - w.at, at, events);
			}
		}
	}
}

/// The audio side of the device probe (see [`crate::probe`]): the shared ring the player
/// attached and the device it follows. Allocation free after [`ProbeTap::new`].
pub(crate) struct ProbeTap {
	probe: Option<Arc<DeviceProbe>>,
	target: Option<DeviceId>,
	input: StereoBuf,
	readings: [f32; cadenza_dsp::readings::MAX],
	/// The target ran in the quantum being rendered.
	hit: bool,
}

impl ProbeTap {
	pub fn new() -> Self {
		Self {
			probe: None,
			target: None,
			input: StereoBuf::new(),
			readings: [0.0; cadenza_dsp::readings::MAX],
			hit: false,
		}
	}

	fn wants(&self, device: DeviceId) -> bool {
		self.probe.is_some() && self.target == Some(device)
	}

	fn capture(&mut self, device: &Device, buf: &StereoBuf) {
		let Some(p) = &self.probe else {
			return;
		};
		p.push(&self.input.l, &self.input.r, &buf.l, &buf.r);
		self.readings.fill(0.0);
		let n = device.processor.readings(&mut self.readings);
		p.publish_readings(&self.readings[..n.min(self.readings.len())]);
		let mut mods = [0.0; MAX_MODULATORS];
		let n = device
			.modulation
			.as_ref()
			.map_or(0, |m| m.values(&mut mods));
		p.publish_modulation(&mods[..n]);
		self.hit = true;
	}
}

impl Device {
	/// Runs one quantum: aligns and hands over the key (if any), then processes `buf`.
	fn run(
		&mut self,
		buf: &mut StereoBuf,
		events: &[MidiEvent],
		t: &Transport,
		done: &[Node],
		probe: &mut ProbeTap,
	) {
		if let Some(sc) = &mut self.sidechain
			&& let Some(src) = done.get(sc.source)
		{
			let tap = if sc.pre_fader { &src.pre } else { &src.buf };
			sc.key.l.copy_from_slice(&tap.l);
			sc.key.r.copy_from_slice(&tap.r);
			sc.key_delay.process(&mut sc.key);
			sc.main_delay.process(buf);
			if sc.feeds {
				self.processor.set_sidechain(&sc.key.l, &sc.key.r);
			}
		}
		if let Some(m) = &mut self.modulation {
			if m.needs_key()
				&& let Some(sc) = &self.sidechain
			{
				m.follow(&sc.key.l, &sc.key.r, true);
			}
			if !m.follows_output() {
				m.follow(&buf.l, &buf.r, false);
			}
			m.apply(&mut *self.processor, events, t);
		}
		let probed = probe.wants(self.id);
		if probed {
			probe.input.l.copy_from_slice(&buf.l);
			probe.input.r.copy_from_slice(&buf.r);
		}
		self.processor.process(&mut buf.l, &mut buf.r, events, t);
		if let Some(m) = &mut self.modulation
			&& m.follows_output()
		{
			m.follow(&buf.l, &buf.r, false);
		}
		if probed {
			probe.capture(self, buf);
		}
	}

	/// A parameter write from the graph (initial value, live edit, automation): moves
	/// the modulation base of a modulated parameter, else sets it. Realtime safe.
	pub(crate) fn set_param(&mut self, index: usize, value: f32) {
		if let Some(m) = &mut self.modulation
			&& m.set_base(index, value)
		{
			return;
		}
		self.processor.set_param(index, value);
	}

	/// Runs one quantum of an effect, with the MIDI it receives if any.
	fn run_effect(
		&mut self,
		buf: &mut StereoBuf,
		t: &Transport,
		done: &[Node],
		probe: &mut ProbeTap,
		q: QuantumMidi,
	) {
		match self.midi.take() {
			Some(mut m) => {
				let DeviceMidi { schedule, events } = &mut *m;
				q.fill(schedule, events);
				self.run(buf, &m.events, t, done, probe);
				self.midi = Some(m);
			}
			None => self.run(buf, &[], t, done, probe),
		}
	}

	/// Moves the MIDI it receives to timeline sample `pos`.
	fn seek_midi(&mut self, pos: i64) {
		if let Some(m) = &mut self.midi {
			m.schedule.seek(pos);
		}
	}

	fn reset(&mut self) {
		if let Some(m) = &mut self.modulation {
			m.reset();
		}
		self.processor.reset();
		if let Some(sc) = &mut self.sidechain {
			sc.key_delay.clear();
			sc.main_delay.clear();
		}
	}
}

pub(crate) enum Source {
	Instrument {
		device: Option<Device>,
		schedule: MidiSchedule,
		events: Vec<MidiEvent>,
		/// Send `AllNotesOff` after a reset. False for third-party plugins: their `reset`
		/// already kills every voice (CLAP `reset` contract), and some (e.g. Surge XT)
		/// mishandle the wildcard note-off that `AllNotesOff` becomes.
		notes_off_on_reset: bool,
		/// Samples the notes are played late by, so the instrument meets a sidechain key
		/// that arrives later than the timeline (PDC).
		midi_delay: u32,
		/// Pitches to release at the next quantum (notes a schedule swap removed while
		/// they were sounding).
		release: [u64; 2],
	},
	Audio {
		clips: Vec<ClipPlayback>,
		/// Samples of micro fade-in still to apply (after a loop wrap or a start in the
		/// middle of the song: de-click).
		fade_in: u32,
	},
	Bus {
		accumulator: usize,
	},
}

/// `event` moved to the start of a quantum.
fn at_offset_zero(event: MidiEvent) -> MidiEvent {
	event.with_offset(0)
}

/// A folder's volume: every track inside moves by the difference. Realtime safe.
fn folder_volume(folders: &mut [FolderGain], nodes: &mut [Node], folder: usize, db: f32) {
	let Some(f) = folders.get_mut(folder) else {
		return;
	};
	let delta = db - f.db;
	if delta == 0.0 {
		return;
	}
	f.db = db;
	for &i in &f.nodes {
		if let Some(n) = nodes.get_mut(i) {
			n.folder_db += delta;
			n.fader.set_volume_db(n.own_db + n.folder_db);
		}
	}
}

/// A folder track's volume and the nodes inside it (at any depth).
pub(crate) struct FolderGain {
	pub track: TrackId,
	pub db: f32,
	pub nodes: Vec<usize>,
}

pub(crate) struct Node {
	pub track_id: TrackId,
	/// The channel's own fader volume and the sum of its folders' volumes (dB).
	pub own_db: f32,
	pub folder_db: f32,
	pub name: String,
	/// Index in `Project::tracks` (display order).
	pub display_index: usize,
	pub source: Source,
	pub inserts: Vec<Device>,
	pub fader: Fader,
	/// False when muted or silenced by another track's solo.
	pub audible: bool,
	/// The channel's mute alone (auditioning ignores solos).
	pub muted: bool,
	pub edges: Vec<Edge>,
	/// Extra instrument outputs in use.
	pub aux_edges: Vec<AuxEdge>,
	/// Post-fader signal of the last quantum (what the track contributes).
	pub buf: StereoBuf,
	/// Whether a device is keyed by this node's pre-fader signal, kept in `pre`.
	pub tap_pre: bool,
	pub pre: StereoBuf,
	/// Latency of this node's output relative to the timeline.
	pub out_latency: u32,
	/// Audio tracks with an input: where it comes from and when it is monitored.
	pub input: Option<NodeInput>,
	/// Armed for recording (auto input monitoring follows it).
	pub armed: bool,
	/// The input replaces the clips in the current block.
	pub monitoring: bool,
}

pub(crate) struct Master {
	pub inserts: Vec<Device>,
	pub fader: Fader,
	pub buf: StereoBuf,
}

/// Tempo segment for sample <-> beat conversion without allocation.
#[derive(Debug, Clone, Copy)]
pub(crate) struct TempoSegment {
	pub start_sample: f64,
	pub start_beat: f64,
	pub bpm: f64,
}

pub(crate) fn tempo_segments(tempo: &TempoMap, sample_rate: f64) -> Vec<TempoSegment> {
	tempo
		.points()
		.iter()
		.map(|p| TempoSegment {
			start_sample: tempo.tick_to_seconds(p.tick) * sample_rate,
			start_beat: p.tick.beats(),
			bpm: p.bpm,
		})
		.collect()
}

/// A project compiled for one sample rate. Create with [`crate::compile`].
///
/// Drive it with [`Graph::process`] for any block length; it renders internally in
/// [`QUANTUM`]-sample steps. Positions passed to [`Graph::seek`] are timeline positions:
/// the graph runs `latency_samples()` ahead of its output and discards that many samples
/// after every start/seek, so the output is aligned with the timeline despite plugin latency.
pub struct Graph {
	pub(crate) sample_rate: u32,
	pub(crate) nodes: Vec<Node>,
	pub(crate) accumulators: Vec<StereoBuf>,
	pub(crate) master: Master,
	pub(crate) automation: Vec<Binding>,
	/// Address -> target map (control side only; never used by `process`).
	pub(crate) resolver: Resolver,
	/// Folder tracks (group volume offsets).
	pub(crate) folders: Vec<FolderGain>,
	/// Which `Player::load` this graph came from (0 offline), to match automation sets.
	pub(crate) load_id: u64,
	pub(crate) tempo: Vec<TempoSegment>,
	pub(crate) latency: u32,
	pub(crate) tail_seconds: f32,
	pub(crate) warnings: Vec<String>,
	/// Sandboxed plugin devices; never touched by `process`.
	pub(crate) plugin_watches: Vec<PluginWatch>,
	/// Editor handles of the plugin devices; never touched by `process`.
	pub(crate) plugin_editors: Vec<PluginEditorSlot>,
	/// Timeline sample fed to the sources in the next quantum to render.
	pub(crate) engine_pos: i64,
	/// Output samples still to drop to compensate the master latency.
	pub(crate) discard: u32,
	/// Timeline sample of the next sample returned by `process`.
	pub(crate) out_pos: i64,
	/// Read position inside `master.buf`; `QUANTUM` = empty.
	pub(crate) fifo_pos: usize,
	pub(crate) playing: bool,
	pub(crate) needs_reset: bool,
	/// Node whose instrument receives live MIDI input (monitoring).
	pub(crate) monitor: Option<usize>,
	/// Previously monitored node that still needs an `AllNotesOff`.
	pub(crate) monitor_release: Option<usize>,
	/// Live events for the next quantum (capacity [`crate::LIVE_EVENT_CAPACITY`]).
	pub(crate) live: Vec<MidiEvent>,
	/// Frames the stopped graph still renders for live notes (note preview).
	pub(crate) preview_frames: u32,
	/// Live pitches held down (bit per MIDI pitch): the preview runs while any is held.
	pub(crate) held: [u64; 2],
	/// A preview rendered since the last stop/seek: the next `play` must reset first.
	pub(crate) previewed: bool,
	/// Per-track and master levels for the UI.
	pub(crate) meters: Meters,
	/// Loop range in timeline samples (at least one quantum long).
	pub(crate) loop_range: Option<(i64, i64)>,
	/// The last quantum ended exactly at the loop end: the next one starts with the wrap.
	pub(crate) wrap_pending: Option<i64>,
	/// The engine wrapped from `.0` to `.1`; the output follows when it reaches `.0`.
	pub(crate) out_wrap: Option<(i64, i64)>,
	pub(crate) audition: Arc<Audition>,
	/// Live input of the quantum being rendered, one quantum per input channel.
	pub(crate) input_block: Box<[[f32; QUANTUM]; MAX_INPUT_CHANNELS]>,
	/// A recording runs (metronome and auto monitoring follow it).
	pub(crate) recording: bool,
	pub(crate) metronome: Metronome,
	/// Clicks per bar and click length in quarter notes (from the time signature).
	/// Time signatures (never empty).
	pub(crate) meter: Vec<MeterSegment>,
	pub(crate) count_in: CountIn,
	/// What the last `process` call played (see [`Graph::segments`]).
	pub(crate) segments: [Segment; MAX_SEGMENTS],
	pub(crate) segment_count: usize,
	pub(crate) probe: ProbeTap,
	/// Device playing a listen mode (monitoring only, see [`Graph::set_listen`]).
	pub(crate) listen: Option<(DeviceId, Listen)>,
	/// Ring the master's post-fader output goes to every quantum (live loudness).
	pub(crate) master_tap: Option<Arc<DeviceProbe>>,
}

impl Graph {
	pub fn sample_rate(&self) -> u32 {
		self.sample_rate
	}

	/// Attaches the probe ring the graph writes the probed device into (control side,
	/// before the graph goes to the audio thread).
	pub fn attach_probe(&mut self, probe: Option<Arc<DeviceProbe>>) {
		self.probe.probe = probe;
	}

	/// Attaches the ring that receives the master's post-fader output (before the
	/// metronome and the player's monitor switches) every rendered quantum, as both the
	/// input and the output of each [`crate::ProbeFrame`]. Control side.
	pub fn attach_master_tap(&mut self, tap: Option<Arc<DeviceProbe>>) {
		self.master_tap = tap;
	}

	/// The device whose input/output and readings go to the attached probe (`None` =
	/// idle). A different target restarts the probe. Realtime safe.
	pub fn set_probe_target(&mut self, device: Option<DeviceId>) {
		if self.probe.target == device {
			return;
		}
		self.probe.target = device;
		if let Some(p) = &self.probe.probe {
			p.restart();
		}
	}

	/// Like [`Graph::set_probe_target`] for a graph replacing one that already followed
	/// `device`: frames not read yet are kept.
	pub(crate) fn continue_probe(&mut self, device: Option<DeviceId>) {
		self.probe.target = device;
	}

	/// Total plugin latency at the master output, in samples (compensated internally).
	pub fn latency_samples(&self) -> u32 {
		self.latency
	}

	/// Longest device tail, capped at [`crate::MAX_TAIL_SECONDS`].
	pub fn tail_seconds(&self) -> f32 {
		self.tail_seconds
	}

	/// Problems found while compiling (unknown devices/params, missing files, ...).
	pub fn warnings(&self) -> &[String] {
		&self.warnings
	}

	/// Sandboxed plugin devices that have crashed or hung so far (they are bypassed).
	/// Not real-time: call it off the audio thread.
	pub fn plugin_crashes(&self) -> Vec<PluginCrash> {
		self.plugin_watches
			.iter()
			.filter_map(|w| w.crash(""))
			.collect()
	}

	/// Sandboxed plugin devices that answered late at least once (non-fatal
	/// dropouts; see [`PluginXruns`]). Cheap: atomic reads only.
	pub fn plugin_xruns(&self) -> Vec<PluginXruns> {
		self.plugin_watches
			.iter()
			.filter_map(PluginWatch::xruns)
			.collect()
	}

	/// Records the engine position of newly crashed plugins (offline loops only; cheap
	/// atomic checks).
	pub(crate) fn note_plugin_crashes(&mut self) {
		let pos = self.engine_pos;
		for w in &mut self.plugin_watches {
			if w.crashed_at.is_none() && w.monitor.is_crashed() {
				w.crashed_at = Some(pos);
			}
		}
	}

	/// Crash warnings for an offline render, with the approximate timeline position.
	pub(crate) fn crash_warnings(&self) -> Vec<String> {
		let sr = f64::from(self.sample_rate);
		self.plugin_watches
			.iter()
			.filter_map(|w| {
				let when = match w.crashed_at {
					Some(pos) => {
						let sample = (pos - i64::from(self.latency)).max(0);
						format!(
							"It happened at about {:.1} s (beat {:.1}); ",
							sample as f64 / sr,
							self.beat_at(sample)
						)
					}
					None => String::new(),
				};
				w.crash(&format!(
					"{when}the rest of the render completed without it. Render again to retry; \
					 if it keeps failing, replace this device with another plugin or a built-in \
					 device, or bypass it."
				))
				.map(|c| c.message)
			})
			.collect()
	}

	pub fn is_playing(&self) -> bool {
		self.playing
	}

	/// Live levels of this graph's tracks and master, published at the end of every
	/// [`Graph::process`] call (see [`MeterBank`]).
	pub fn meters(&self) -> std::sync::Arc<MeterBank> {
		self.meters.bank.clone()
	}

	/// Routes live MIDI ([`Graph::push_live_event`]) to the instrument of `track` (`None`
	/// stops monitoring; notes still held on the previous track are released). Returns
	/// false when `track` is not an instrument track of this graph. Realtime safe.
	pub fn set_monitor_track(&mut self, track: Option<TrackId>) -> bool {
		let next = track.and_then(|id| {
			self.nodes
				.iter()
				.position(|n| n.track_id == id && matches!(n.source, Source::Instrument { .. }))
		});
		if self.monitor != next {
			if self.monitor.is_some() {
				self.monitor_release = self.monitor;
			}
			self.monitor = next;
			self.live.clear();
		}
		track.is_none() || next.is_some()
	}

	/// Sets a channel's fader volume (`track: None` = master; a folder track offsets the
	/// tracks inside) without recompiling; the change is smoothed. Realtime safe. Returns
	/// false for an unknown track.
	pub fn set_channel_volume(&mut self, track: Option<TrackId>, db: f32) -> bool {
		match track {
			None => self.master.fader.set_volume_db(db),
			Some(id) => match self.nodes.iter_mut().find(|n| n.track_id == id) {
				Some(n) => {
					n.own_db = db;
					n.fader.set_volume_db(db + n.folder_db);
				}
				None => match self.folders.iter().position(|f| f.track == id) {
					Some(i) => self.set_folder_volume(i, db),
					None => return false,
				},
			},
		}
		true
	}

	fn set_folder_volume(&mut self, folder: usize, db: f32) {
		folder_volume(&mut self.folders, &mut self.nodes, folder, db);
	}

	/// Like [`Graph::set_channel_volume`], for pan (-1..1).
	pub fn set_channel_pan(&mut self, track: Option<TrackId>, pan: f32) -> bool {
		match track {
			None => self.master.fader.set_pan(pan),
			Some(id) => match self.nodes.iter_mut().find(|n| n.track_id == id) {
				Some(n) => n.fader.set_pan(pan),
				None => return false,
			},
		}
		true
	}

	/// Sets parameter `index` of the device `device` (instrument or insert, any track or
	/// the master) without recompiling; plain value, the device smooths and clamps it.
	/// Realtime safe. Returns false when the graph has no such device (e.g. bypassed).
	pub fn set_device_param(&mut self, device: DeviceId, index: usize, value: f32) -> bool {
		match self.device_mut(device) {
			Some(d) => {
				d.set_param(index, value);
				true
			}
			None => false,
		}
	}

	/// The graph's device `id` (instrument, insert or master insert). Realtime safe.
	fn device_mut(&mut self, id: DeviceId) -> Option<&mut Device> {
		self.nodes
			.iter_mut()
			.flat_map(|n| {
				let instrument = match &mut n.source {
					Source::Instrument { device, .. } => device.as_mut(),
					_ => None,
				};
				instrument.into_iter().chain(n.inserts.iter_mut())
			})
			.chain(self.master.inserts.iter_mut())
			.find(|d| d.id == id)
	}

	/// Monitoring-only listen mode of one device (see [`cadenza_dsp::Listen`]); the
	/// previous target goes back to normal. `None` or [`Listen::Off`] = nobody listens.
	/// Live graphs only (the player); offline renders never call it. Realtime safe.
	pub fn set_listen(&mut self, target: Option<(DeviceId, Listen)>) {
		let target = target.filter(|(_, mode)| *mode != Listen::Off);
		if self.listen == target {
			return;
		}
		if let Some((old, _)) = self.listen.take()
			&& let Some(d) = self.device_mut(old)
		{
			d.processor.set_listen(Listen::Off);
		}
		if let Some((id, mode)) = target
			&& let Some(d) = self.device_mut(id)
		{
			d.processor.set_listen(mode);
		}
		self.listen = target;
	}

	/// Sets a send's level (dB) without recompiling; ramped over one quantum. Realtime
	/// safe. Returns false for a send the graph does not play.
	pub fn set_send_level(&mut self, send: DeviceId, db: f32) -> bool {
		let edge = self
			.nodes
			.iter_mut()
			.flat_map(|n| n.edges.iter_mut())
			.find(|e| e.send == Some(send));
		match edge {
			Some(e) => {
				e.set_level_db(db);
				true
			}
			None => false,
		}
	}

	/// Index of `param` (built-in or plugin param id) on the graph's device `device`, for
	/// [`Graph::set_device_param`]. Not realtime: call it off the audio thread.
	pub fn param_index(&self, device: DeviceId, param: &str) -> Option<usize> {
		self.resolver.param_index(device, param)
	}

	/// Resolves `lanes` against this graph (off the audio thread) for
	/// [`Graph::swap_automation`]. Lanes the graph cannot play are skipped.
	pub fn prepare_automation(
		&self,
		lanes: &[AutomationLane],
		tempo: &TempoMap,
	) -> AutomationBindings {
		AutomationBindings(self.resolver.bind(lanes, tempo, None, &mut Vec::new()))
	}

	/// Replaces the automation with `bindings` (prepared by this graph); the old bindings
	/// are left in it, to be dropped off the audio thread. Every lane is re-applied at the
	/// next quantum. Realtime safe: swaps the vectors, never allocates. A parameter whose
	/// lane is gone keeps its last automated value until the next recompile.
	pub fn swap_automation(&mut self, bindings: &mut AutomationBindings) {
		std::mem::swap(&mut self.automation, &mut bindings.0);
		for b in &mut self.automation {
			b.last = f32::NAN;
		}
	}

	/// The monitored track, if any.
	pub fn monitor_track(&self) -> Option<TrackId> {
		self.monitor.map(|i| self.nodes[i].track_id)
	}

	/// Arms or disarms `track` for recording (auto input monitoring follows). Realtime
	/// safe. Returns false for an unknown track.
	pub fn set_armed(&mut self, track: TrackId, armed: bool) -> bool {
		match self.nodes.iter_mut().find(|n| n.track_id == track) {
			Some(n) => {
				n.armed = armed;
				true
			}
			None => false,
		}
	}

	pub fn clear_armed(&mut self) {
		for n in &mut self.nodes {
			n.armed = false;
		}
	}

	/// Whether a recording runs (metronome "on record", auto input monitoring).
	pub fn set_recording(&mut self, recording: bool) {
		self.recording = recording;
	}

	pub fn set_metronome(&mut self, settings: MetronomeSettings) {
		self.metronome.settings = settings;
	}

	/// Counts in `bars` bars of metronome clicks (at the tempo of the playhead) before
	/// the transport moves; call it right after [`Graph::play`]. Realtime safe.
	pub fn start_count_in(&mut self, bars: u32) {
		if bars == 0 {
			self.count_in = CountIn::default();
			return;
		}
		let seg = self.segment_at(self.out_pos as f64);
		let meter = meter_at(&self.meter, self.beat_at(self.out_pos));
		let beat_frames = meter.unit() * 60.0 / seg.bpm * f64::from(self.sample_rate);
		let total = (f64::from(bars * meter.numerator.max(1)) * beat_frames).round() as u64;
		self.count_in = CountIn {
			left: total,
			total,
			rendered: 0,
			beat_frames,
			per_bar: meter.numerator.max(1),
		};
	}

	/// Whether a count-in is running.
	pub fn counting_in(&self) -> bool {
		self.count_in.left > 0
	}

	/// The running count-in: beats heard so far, its length in beats and beats per bar.
	pub fn count_in_progress(&self) -> Option<(f64, u32, u32)> {
		let c = &self.count_in;
		(c.left > 0 && c.beat_frames > 0.0).then(|| {
			(
				(c.total - c.left) as f64 / c.beat_frames,
				(c.total as f64 / c.beat_frames).round() as u32,
				c.per_bar,
			)
		})
	}

	/// Continues `old`'s count-in in this graph (a reload swapped in while counting in
	/// must not start the song early). Realtime safe.
	pub(crate) fn continue_count_in(&mut self, old: &Graph) {
		if old.count_in.left == 0 {
			return;
		}
		self.count_in = CountIn {
			// The new graph renders from the output position on.
			rendered: old.count_in.total - old.count_in.left,
			..old.count_in
		};
	}

	/// What the last [`Graph::process`] call played: output stretches and the timeline
	/// samples they were heard at (none while stopped or counting in; two around a loop
	/// wrap). Used to tag recorded input.
	pub fn segments(&self) -> &[Segment] {
		&self.segments[..self.segment_count]
	}

	/// Which audio tracks hear their input now (see `MonitorMode`).
	fn update_monitoring(&mut self) {
		let live = !self.playing || self.recording || self.count_in.left > 0;
		for n in &mut self.nodes {
			n.monitoring = match n.input {
				Some(i) => match i.mode {
					MonitorMode::Off => false,
					MonitorMode::On => true,
					MonitorMode::Auto => n.armed && live,
				},
				None => false,
			};
		}
	}

	fn monitors_input(&self) -> bool {
		self.nodes.iter().any(|n| n.monitoring)
	}

	/// Exact (unrounded) timeline sample of `beat` quarter notes.
	fn sample_at_beat(&self, beat: f64) -> f64 {
		let i = self.tempo.partition_point(|s| s.start_beat <= beat);
		let seg = self.tempo[i.saturating_sub(1)];
		seg.start_sample + (beat - seg.start_beat) * 60.0 / seg.bpm * self.sample_rate as f64
	}

	/// Metronome clicks of the quantum rendered at `pos` (split at a loop wrap).
	fn clicks(&self, pos: i64, wrap: Option<Wrap>) -> ([Click; MAX_CLICKS], usize) {
		let mut out = [Click {
			offset: 0,
			accent: false,
		}; MAX_CLICKS];
		let mut n = 0;
		let parts = match wrap {
			None => [(0, pos, QUANTUM), (0, 0, 0)],
			Some(w) => [(0, pos, w.at), (w.at, w.to, QUANTUM - w.at)],
		};
		for (offset, start, len) in parts {
			if len == 0 {
				continue;
			}
			let (b0, b1) = (self.beat_at(start), self.beat_at(start + len as i64));
			meter_clicks(
				&self.meter,
				b0,
				b1,
				|b| self.sample_at_beat(b).floor() as i64 - start + offset as i64,
				&mut out,
				&mut n,
			);
		}
		(out, n)
	}

	/// Queues a live event for the monitored instrument; it plays at the start of the next
	/// rendered quantum (its offset is ignored). Returns false when nothing is monitored or
	/// the buffer is full. Realtime safe (never allocates).
	///
	/// While the transport is stopped, live events are previewed: the graph renders only
	/// the monitored instrument (and the channels it feeds; no clips, no song notes, the
	/// playhead does not move) while notes are held and for
	/// [`crate::PREVIEW_RELEASE_SECONDS`] plus the device tails after the last release.
	pub fn push_live_event(&mut self, event: MidiEvent) -> bool {
		if self.monitor.is_none() || self.live.len() == self.live.capacity() {
			return false;
		}
		match event {
			MidiEvent::NoteOn { pitch, .. } => {
				self.held[usize::from(pitch >> 6) & 1] |= 1 << (pitch & 63);
			}
			MidiEvent::NoteOff { pitch, .. } => {
				self.held[usize::from(pitch >> 6) & 1] &= !(1 << (pitch & 63));
			}
			MidiEvent::AllNotesOff { .. } => self.held = [0; 2],
			_ => {}
		}
		self.live.push(event);
		if !self.playing {
			self.preview_frames = self.preview_tail_frames();
		}
		true
	}

	fn preview_tail_frames(&self) -> u32 {
		((crate::PREVIEW_RELEASE_SECONDS + self.tail_seconds.min(4.0)) * self.sample_rate as f32)
			as u32
	}

	/// Whether a stopped graph is rendering previewed live notes.
	pub fn is_previewing(&self) -> bool {
		!self.playing && self.preview_frames > 0
	}

	pub fn play(&mut self) {
		if self.previewed {
			// The preview ran the devices outside the song: start from a clean state.
			self.previewed = false;
			self.needs_reset = true;
			self.fifo_pos = QUANTUM;
			self.engine_pos = self.out_pos;
			self.discard = self.latency;
			self.wrap_pending = None;
			self.out_wrap = None;
		}
		self.preview_frames = 0;
		self.playing = true;
	}

	/// Stops output; sounding notes are released (AllNotesOff) and tails cleared on resume.
	pub fn stop(&mut self) {
		self.playing = false;
		self.count_in = CountIn::default();
		self.metronome.reset();
		self.live.clear();
		self.held = [0; 2];
		self.preview_frames = 0;
		self.needs_reset = true;
		self.fifo_pos = QUANTUM;
		self.engine_pos = self.out_pos;
		self.discard = self.latency;
		self.wrap_pending = None;
		self.out_wrap = None;
	}

	/// Moves the playhead to timeline sample `sample`. Sends AllNotesOff and clears all
	/// device state at the next quantum.
	pub fn seek_sample(&mut self, sample: i64) {
		self.metronome.reset();
		self.out_pos = sample;
		self.engine_pos = sample;
		self.discard = self.latency;
		self.fifo_pos = QUANTUM;
		self.needs_reset = true;
		self.wrap_pending = None;
		self.out_wrap = None;
	}

	pub fn seek(&mut self, tick: Tick) {
		let sample = self.tick_to_sample(tick);
		self.seek_sample(sample);
	}

	/// Timeline sample of the next output sample.
	pub fn position_sample(&self) -> i64 {
		self.out_pos
	}

	pub fn position(&self) -> Tick {
		Tick((self.beat_at(self.out_pos) * PPQ as f64).round() as i64)
	}

	fn segment_at(&self, sample: f64) -> TempoSegment {
		let i = self.tempo.partition_point(|s| s.start_sample <= sample);
		self.tempo[i.saturating_sub(1)]
	}

	fn beat_at(&self, sample: i64) -> f64 {
		let s = sample as f64;
		let seg = self.segment_at(s);
		seg.start_beat + (s - seg.start_sample) / self.sample_rate as f64 * seg.bpm / 60.0
	}

	pub(crate) fn tick_to_sample(&self, tick: Tick) -> i64 {
		let beat = tick.beats();
		let i = self.tempo.partition_point(|s| s.start_beat <= beat);
		let seg = self.tempo[i.saturating_sub(1)];
		(seg.start_sample + (beat - seg.start_beat) * 60.0 / seg.bpm * self.sample_rate as f64)
			.round() as i64
	}

	/// Fills `left`/`right` (equal length, any size) with the next output samples. When
	/// stopped, writes silence and does not advance.
	pub fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
		self.process_live(left, right, None);
	}

	/// [`Graph::process`] with the live input of this block (realtime playback): audio
	/// tracks that monitor their input hear `input` instead of their clips.
	pub(crate) fn process_live(
		&mut self,
		left: &mut [f32],
		right: &mut [f32],
		mut input: Option<&mut InputFifo>,
	) {
		let n = left.len().min(right.len());
		self.segment_count = 0;
		self.update_monitoring();
		if !self.playing {
			self.idle_devices();
		}
		if !self.playing && (self.preview_frames > 0 || self.monitors_input()) {
			self.process_preview(&mut left[..n], &mut right[..n], input);
			return;
		}
		if !self.playing {
			left.fill(0.0);
			right.fill(0.0);
			self.meters.silence();
			self.meters.publish();
			// Nothing ran: readers must not take the last readings as live.
			if let Some(p) = &self.probe.probe {
				p.set_active(false);
			}
			return;
		}
		let mut start = 0;
		if self.count_in.left > 0 {
			let k = (self.count_in.left as usize).min(n);
			self.process_count_in(&mut left[..k], &mut right[..k], input.as_deref_mut());
			if k == n {
				self.meters.publish();
				return;
			}
			start = k;
		}
		let (left, right) = (&mut left[start..n], &mut right[start..n]);
		let n = left.len();
		while self.discard > 0 {
			if self.fifo_pos == QUANTUM {
				self.render_quantum_inner(false, input.as_deref_mut());
				self.fifo_pos = 0;
			}
			let skip = (self.discard as usize).min(QUANTUM - self.fifo_pos);
			self.fifo_pos += skip;
			self.discard -= skip as u32;
		}
		let mut done = 0;
		while done < n {
			if self.fifo_pos == QUANTUM {
				self.render_quantum_inner(false, input.as_deref_mut());
				self.fifo_pos = 0;
			}
			let take = (n - done).min(QUANTUM - self.fifo_pos);
			left[done..done + take]
				.copy_from_slice(&self.master.buf.l[self.fifo_pos..self.fifo_pos + take]);
			right[done..done + take]
				.copy_from_slice(&self.master.buf.r[self.fifo_pos..self.fifo_pos + take]);
			self.fifo_pos += take;
			done += take;
		}
		let first = match self.out_wrap {
			Some((from, _)) if self.out_pos + n as i64 >= from => {
				(from - self.out_pos).clamp(0, n as i64) as usize
			}
			_ => n,
		};
		self.push_segment(start, self.out_pos, first);
		if first < n
			&& let Some((_, to)) = self.out_wrap
		{
			self.push_segment(start + first, to, n - first);
		}
		self.out_pos += n as i64;
		if let Some((from, to)) = self.out_wrap
			&& self.out_pos >= from
		{
			self.out_pos = to + (self.out_pos - from);
			self.out_wrap = None;
		}
		self.meters.publish();
	}

	fn push_segment(&mut self, offset: usize, tag: i64, len: usize) {
		if len > 0 && self.segment_count < MAX_SEGMENTS {
			self.segments[self.segment_count] = Segment { offset, tag, len };
			self.segment_count += 1;
		}
	}

	/// The count-in: the transport waits while the metronome counts (live input and
	/// notes still play through their tracks). When it ends the song starts cleanly
	/// from the playhead.
	fn process_count_in(
		&mut self,
		left: &mut [f32],
		right: &mut [f32],
		mut input: Option<&mut InputFifo>,
	) {
		if !self.previewed {
			self.previewed = true;
			self.fifo_pos = QUANTUM;
		}
		let n = left.len();
		let mut done = 0;
		while done < n {
			if self.fifo_pos == QUANTUM {
				self.render_quantum_inner(true, input.as_deref_mut());
				let c = self.count_in;
				let mut clicks = [Click {
					offset: 0,
					accent: false,
				}; MAX_CLICKS];
				let mut count = 0;
				if c.beat_frames > 0.0 {
					let b0 = c.rendered as f64 / c.beat_frames;
					let b1 = (c.rendered + QUANTUM as u64).min(c.total) as f64 / c.beat_frames;
					clicks_between(
						b0,
						b1,
						0.0,
						1.0,
						c.per_bar,
						|b| (b * c.beat_frames).floor() as i64 - c.rendered as i64,
						&mut clicks,
						&mut count,
					);
				}
				self.count_in.rendered += QUANTUM as u64;
				let Self {
					metronome, master, ..
				} = self;
				metronome.mix_direct(&clicks[..count], &mut master.buf.l, &mut master.buf.r);
				self.fifo_pos = 0;
			}
			let take = (n - done).min(QUANTUM - self.fifo_pos);
			left[done..done + take]
				.copy_from_slice(&self.master.buf.l[self.fifo_pos..self.fifo_pos + take]);
			right[done..done + take]
				.copy_from_slice(&self.master.buf.r[self.fifo_pos..self.fifo_pos + take]);
			self.fifo_pos += take;
			done += take;
		}
		self.count_in.left = self.count_in.left.saturating_sub(n as u64);
		if self.count_in.left == 0 {
			// Like `play` after a preview: start from a clean state at the playhead.
			self.count_in = CountIn::default();
			self.previewed = false;
			self.needs_reset = true;
			self.fifo_pos = QUANTUM;
			self.engine_pos = self.out_pos;
			self.discard = self.latency;
			self.wrap_pending = None;
			self.out_wrap = None;
			self.metronome.reset();
		}
	}

	/// The editor handle of plugin device `device` (`None` for built-ins and plugins that
	/// are bypassed or failed to load). Usable while the graph runs on the audio thread.
	pub fn plugin_editor(&self, device: DeviceId) -> Option<cadenza_plugin_host::PluginEditor> {
		self.plugin_editors
			.iter()
			.find(|s| s.device_id == device)
			.map(|s| s.editor.clone())
	}

	/// Lets plugin devices exchange parameter changes with their editors while the
	/// transport is stopped (`Processor::idle`). Realtime-safe.
	fn idle_devices(&mut self) {
		for n in &mut self.nodes {
			if let Source::Instrument {
				device: Some(d), ..
			} = &mut n.source
			{
				d.processor.idle();
			}
			for d in &mut n.inserts {
				d.processor.idle();
			}
		}
		for d in &mut self.master.inserts {
			d.processor.idle();
		}
	}

	/// Stopped transport with live notes (or monitored input): renders quanta without
	/// song content and without moving the playhead.
	fn process_preview(
		&mut self,
		left: &mut [f32],
		right: &mut [f32],
		mut input: Option<&mut InputFifo>,
	) {
		if !self.previewed {
			self.previewed = true;
			self.fifo_pos = QUANTUM;
		}
		let n = left.len();
		let mut done = 0;
		while done < n {
			if self.fifo_pos == QUANTUM {
				self.render_quantum_inner(true, input.as_deref_mut());
				self.fifo_pos = 0;
			}
			let take = (n - done).min(QUANTUM - self.fifo_pos);
			left[done..done + take]
				.copy_from_slice(&self.master.buf.l[self.fifo_pos..self.fifo_pos + take]);
			right[done..done + take]
				.copy_from_slice(&self.master.buf.r[self.fifo_pos..self.fifo_pos + take]);
			self.fifo_pos += take;
			done += take;
		}
		if self.held == [0; 2] {
			self.preview_frames = self.preview_frames.saturating_sub(n as u32);
		} else {
			self.preview_frames = self.preview_tail_frames();
		}
		self.meters.publish();
	}

	/// Clears all device and delay state at `engine_pos`, with automation applied first so
	/// that smoothed parameters start at their automated value instead of ramping to it.
	fn reset_state(&mut self) {
		let pos = self.engine_pos;
		let micro = audio_clip::micro_frames(self.sample_rate);
		for b in &mut self.automation {
			b.last = f32::NAN;
		}
		self.apply_automation();
		for node in &mut self.nodes {
			if let Source::Instrument {
				device,
				schedule,
				midi_delay,
				..
			} = &mut node.source
			{
				if let Some(d) = device {
					d.reset();
				}
				schedule.seek(pos - i64::from(*midi_delay));
			}
			// Starting in the middle of the song: fade audio in (de-click).
			if let Source::Audio { fade_in, .. } = &mut node.source {
				*fade_in = if pos > 0 { micro } else { 0 };
			}
			for d in &mut node.inserts {
				d.reset();
				d.seek_midi(pos);
			}
			for e in &mut node.edges {
				e.delay.clear();
				e.gain = e.target_gain;
			}
			for e in &mut node.aux_edges {
				e.delay.clear();
			}
			node.pre.clear();
			node.fader.snap();
		}
		for d in &mut self.master.inserts {
			d.reset();
			d.seek_midi(pos);
		}
		self.master.fader.snap();
	}

	fn apply_automation(&mut self) {
		let pos = self.engine_pos;
		for b in &mut self.automation {
			let v = b.curve.value_at(pos);
			if v == b.last {
				continue;
			}
			b.last = v;
			match b.target {
				AutoTarget::Volume { node } => match node {
					Some(i) => {
						let n = &mut self.nodes[i];
						n.own_db = v;
						n.fader.set_volume_db(v + n.folder_db);
					}
					None => self.master.fader.set_volume_db(v),
				},
				AutoTarget::FolderVolume { folder } => {
					folder_volume(&mut self.folders, &mut self.nodes, folder, v)
				}
				AutoTarget::Pan { node } => match node {
					Some(i) => self.nodes[i].fader.set_pan(v),
					None => self.master.fader.set_pan(v),
				},
				AutoTarget::Send { node, edge } => {
					if let Some(e) = self.nodes[node].edges.get_mut(edge) {
						e.set_level_db(v);
					}
				}
				AutoTarget::Param { node, slot, index } => {
					let device = match (node, slot) {
						(Some(i), DeviceSlot::Instrument) => match &mut self.nodes[i].source {
							Source::Instrument { device, .. } => device.as_mut(),
							_ => None,
						},
						(Some(i), DeviceSlot::Insert(k)) => self.nodes[i].inserts.get_mut(k),
						(None, DeviceSlot::Insert(k)) => self.master.inserts.get_mut(k),
						(None, DeviceSlot::Instrument) => None,
					};
					if let Some(d) = device {
						d.set_param(index, v);
					}
				}
			}
		}
	}

	/// Renders one quantum starting at `engine_pos` into `master.buf` (and every node's
	/// post-fader `buf`), then advances `engine_pos`.
	pub(crate) fn render_quantum(&mut self) {
		self.render_quantum_inner(false, None);
	}

	/// The loop wrap falling in the quantum that starts at `engine_pos`, if any.
	fn take_wrap(&mut self) -> Option<Wrap> {
		if let Some(from) = self.wrap_pending.take() {
			return Some(Wrap {
				at: 0,
				from,
				to: self.engine_pos,
			});
		}
		let (start, end) = self.loop_range?;
		let pos = self.engine_pos;
		(pos < end && pos + QUANTUM as i64 > end).then(|| Wrap {
			at: (end - pos) as usize,
			from: end,
			to: start,
		})
	}

	/// `preview`: stopped-transport note preview; only live events play (no clip notes,
	/// no audio clips, no chase) and `engine_pos` stays put. `input`: the live input
	/// (realtime only), read one quantum per call.
	fn render_quantum_inner(&mut self, preview: bool, input: Option<&mut InputFifo>) {
		if let Some(fifo) = input {
			fifo.read_quantum(&mut self.input_block[..]);
		}
		let all_notes_off = self.needs_reset;
		if self.needs_reset {
			self.reset_state();
			self.needs_reset = false;
		}
		self.apply_automation();
		let wrap = if preview { None } else { self.take_wrap() };
		let pos = self.engine_pos;
		let micro = audio_clip::micro_frames(self.sample_rate);
		let click_on = !preview && self.metronome.enabled(self.recording);
		let (clicks, click_count) = if click_on {
			self.clicks(pos, wrap)
		} else {
			(
				[Click {
					offset: 0,
					accent: false,
				}; MAX_CLICKS],
				0,
			)
		};
		let seg = self.segment_at(pos as f64);
		let beat = self.beat_at(pos).max(0.0);
		let meter = meter_at(&self.meter, beat);
		let (bar, bar_start) = meter.bar_at(beat);
		let transport = Transport {
			bpm: seg.bpm,
			playing: !preview,
			beat_position: beat,
			time_sig_numerator: meter.numerator as u16,
			time_sig_denominator: meter.denominator as u16,
			bar_start,
			bar_number: bar.clamp(0, i64::from(i32::MAX)) as i32,
		};
		for acc in &mut self.accumulators {
			acc.clear();
		}
		let auditioning = self.audition.active();
		let quantum_midi = QuantumMidi {
			pos,
			wrap,
			all_notes_off,
			preview,
		};
		let Self {
			nodes,
			accumulators,
			master,
			monitor,
			monitor_release,
			live,
			meters,
			audition,
			input_block,
			metronome,
			probe,
			master_tap,
			..
		} = self;
		probe.hit = false;
		for i in 0..nodes.len() {
			let (done, rest) = nodes.split_at_mut(i);
			let node = &mut rest[0];
			// Outside the auditioned tracks: still rendered (it may key a sidechain) but
			// never mixed into a bus or the master.
			let scoped = !auditioning || audition.contains(i);
			let buf = &mut node.buf;
			buf.clear();
			match &mut node.source {
				Source::Instrument {
					device,
					schedule,
					events,
					notes_off_on_reset,
					midi_delay,
					release,
				} => {
					let delay = i64::from(*midi_delay);
					let pos = pos - delay;
					events.clear();
					if all_notes_off {
						*release = [0; 2];
						if *notes_off_on_reset {
							events.push(MidiEvent::AllNotesOff { offset: 0 });
						}
						// Chase notes that should already be sounding at `pos` so a
						// start/seek landing mid-note doesn't leave it silent.
						if !preview {
							schedule.chase(pos, 0, events);
						}
					}
					if *release != [0; 2] {
						for pitch in 0..128u8 {
							if release[usize::from(pitch >> 6)] & (1 << (pitch & 63)) != 0
								&& events.len() < events.capacity()
							{
								events.push(MidiEvent::NoteOff { offset: 0, pitch });
							}
						}
						*release = [0; 2];
					}
					if *monitor_release == Some(i) {
						*monitor_release = None;
						events.push(MidiEvent::AllNotesOff { offset: 0 });
					}
					if *monitor == Some(i) {
						for e in live.drain(..) {
							if events.len() == events.capacity() {
								break;
							}
							events.push(at_offset_zero(e));
						}
					}
					if !preview {
						match wrap {
							None => schedule.collect(pos, QUANTUM, 0, events),
							Some(w) => {
								// Up to the loop end, release what is held there, then
								// continue from the loop start (chasing notes that cover it).
								let at = w.at as u32;
								schedule.collect(pos, w.at, 0, events);
								schedule.release(w.from - delay, at, events);
								schedule.seek(w.to - delay);
								schedule.chase(w.to - delay, at, events);
								schedule.collect(w.to - delay, QUANTUM - w.at, at, events);
							}
						}
					}
					if let Some(d) = device {
						d.run(buf, events, &transport, done, probe);
						for e in &mut node.aux_edges {
							let Some((l, r)) = d.processor.aux_output(e.port) else {
								continue;
							};
							match e.accumulator {
								Some(a) if scoped => e.delay.mix(l, r, 1.0, &mut accumulators[a]),
								Some(_) => {}
								None => e.delay.mix(l, r, 1.0, buf),
							}
						}
					}
				}
				Source::Audio { .. } if node.monitoring => {
					if let Some(i) = node.input {
						buf.l.copy_from_slice(&input_block[i.left]);
						buf.r.copy_from_slice(&input_block[i.right]);
					}
				}
				Source::Audio { clips, fade_in } if !preview => match wrap {
					None => {
						for c in clips.iter() {
							c.mix(pos, &mut buf.l, &mut buf.r);
						}
						audio_clip::fade_in_head(&mut buf.l, &mut buf.r, fade_in, micro);
					}
					Some(w) => {
						let (l0, l1) = buf.l.split_at_mut(w.at);
						let (r0, r1) = buf.r.split_at_mut(w.at);
						for c in clips.iter() {
							if w.at > 0 {
								c.mix(pos, l0, r0);
							}
							c.mix(w.to, l1, r1);
						}
						// Micro-fades around the jump (de-click the loop seam).
						audio_clip::fade_in_head(l0, r0, fade_in, micro);
						audio_clip::fade_out_tail(l0, r0, micro as usize);
						*fade_in = micro;
						audio_clip::fade_in_head(l1, r1, fade_in, micro);
					}
				},
				Source::Audio { .. } => {}
				Source::Bus { accumulator } => {
					let acc = &accumulators[*accumulator];
					buf.l.copy_from_slice(&acc.l);
					buf.r.copy_from_slice(&acc.r);
				}
			}
			for d in &mut node.inserts {
				d.run_effect(buf, &transport, done, probe, quantum_midi);
			}
			if node.tap_pre {
				node.pre.l.copy_from_slice(&buf.l);
				node.pre.r.copy_from_slice(&buf.r);
			}
			if !scoped {
				if node.audible {
					node.fader.apply(buf);
				} else {
					buf.clear();
				}
				meters.measure_silence(i);
				continue;
			}
			// Auditioned tracks play whatever is soloed elsewhere; mute still holds.
			let audible = if auditioning {
				!node.muted
			} else {
				node.audible
			};
			if !audible {
				buf.clear();
				meters.measure_silence(i);
				continue;
			}
			for e in node.edges.iter_mut().filter(|e| e.pre_fader) {
				e.mix(buf, &mut accumulators[e.accumulator]);
			}
			node.fader.apply(buf);
			meters.measure(Some(i), &buf.l, &buf.r);
			for e in node.edges.iter_mut().filter(|e| !e.pre_fader) {
				e.mix(buf, &mut accumulators[e.accumulator]);
			}
		}
		let master_in = accumulators.last().expect("master accumulator");
		master.buf.l.copy_from_slice(&master_in.l);
		master.buf.r.copy_from_slice(&master_in.r);
		for d in &mut master.inserts {
			d.run_effect(&mut master.buf, &transport, nodes, probe, quantum_midi);
		}
		if let Some(p) = &probe.probe {
			p.set_active(probe.hit);
		}
		master.fader.apply(&mut master.buf);
		meters.measure(None, &master.buf.l, &master.buf.r);
		if let Some(tap) = master_tap {
			tap.push(&master.buf.l, &master.buf.r, &master.buf.l, &master.buf.r);
			tap.set_active(true);
		}
		if preview {
			return;
		}
		metronome.process(
			click_on,
			&clicks[..click_count],
			&mut master.buf.l,
			&mut master.buf.r,
		);
		match wrap {
			Some(w) => {
				self.engine_pos = w.to + (QUANTUM - w.at) as i64;
				if w.at > 0 || self.out_wrap.is_none() {
					self.out_wrap = Some((w.from, w.to));
				}
			}
			None => {
				self.engine_pos += QUANTUM as i64;
				if let Some((start, end)) = self.loop_range
					&& self.engine_pos == end
				{
					// Ended exactly on the loop end: wrap at the start of the next one.
					self.engine_pos = start;
					self.wrap_pending = Some(end);
					self.out_wrap = Some((end, start));
				}
			}
		}
	}

	/// Loops playback over `[start, end)` (song ticks; `None` plays straight on). The
	/// wrap is sample accurate: notes held at the loop end are released there, notes
	/// covering the loop start are chased, effect tails ring on. Playback that is past
	/// the end when the loop is set continues without wrapping. Ranges shorter than one
	/// quantum are ignored. Realtime safe.
	pub fn set_loop(&mut self, range: Option<(Tick, Tick)>) {
		self.loop_range = range.and_then(|(start, end)| {
			let (s, e) = (
				self.tick_to_sample(start.max(Tick::ZERO)),
				self.tick_to_sample(end),
			);
			(e - s >= QUANTUM as i64).then_some((s, e))
		});
		if self.loop_range.is_none() {
			self.wrap_pending = None;
		}
	}

	/// The loop range in timeline samples, if looping.
	pub fn loop_samples(&self) -> Option<(i64, i64)> {
		self.loop_range
	}

	/// Plays only `tracks` (and the buses they feed, through outputs, sends and extra
	/// instrument outputs) without touching the project's mute/solo: auditioned tracks
	/// ignore other tracks' solos, a muted one stays muted. Tracks outside the scope are
	/// still rendered (sidechain keys keep working) but not heard. `None` plays the
	/// song as mixed. Not realtime (builds the mask); the [`crate::Player`] prepares it on
	/// the control side and the audio thread only reads atomics.
	pub fn set_audition(&mut self, tracks: Option<&[TrackId]>) {
		let mask = tracks.map(|t| self.resolver.audition_mask(t));
		self.audition.set(mask.as_deref());
	}

	/// Reschedules `clips` (the track's clips after an edit that only changed MIDI
	/// clips/notes) for [`Graph::swap_schedule`]. `None` when `track` is not an instrument
	/// track of this graph or the tempo map differs: recompile instead. Not realtime.
	pub fn prepare_schedule(
		&self,
		track: TrackId,
		clips: &[cadenza_model::Clip],
		tempo: &TempoMap,
	) -> Option<ScheduleSwap> {
		let node = *self.resolver.nodes.get(&track)?;
		if *tempo != self.resolver.tempo || !self.resolver.instruments.contains(&node) {
			return None;
		}
		let schedule = MidiSchedule::from_clips(clips, tempo, f64::from(self.sample_rate));
		let events = Vec::with_capacity(crate::compile::event_capacity(&schedule));
		Some(ScheduleSwap {
			load_id: self.load_id,
			node,
			schedule,
			events,
		})
	}

	/// Replaces a track's MIDI schedule (prepared off the audio thread) without touching
	/// its instrument: sounding notes that the new schedule no longer has at the playhead
	/// are released at the next quantum, the others keep sounding. The old schedule is
	/// left in `swap` to be dropped elsewhere. Realtime safe.
	pub fn swap_schedule(&mut self, swap: &mut ScheduleSwap) -> bool {
		let pos = self.engine_pos;
		let Some(Source::Instrument {
			schedule,
			events,
			midi_delay,
			release,
			..
		}) = self.nodes.get_mut(swap.node).map(|n| &mut n.source)
		else {
			return false;
		};
		let pos = pos - i64::from(*midi_delay);
		let before = schedule.sounding(pos);
		let after = swap.schedule.sounding(pos);
		std::mem::swap(schedule, &mut swap.schedule);
		std::mem::swap(events, &mut swap.events);
		schedule.seek(pos);
		release[0] |= before[0] & !after[0];
		release[1] |= before[1] & !after[1];
		true
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn mono_pan_is_constant_power_with_minus_3db_center() {
		let f = Fader::new(0.0, 0.0, true, 48_000.0);
		assert!((f.target.0 - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-6);
		assert!((f.target.0.powi(2) + f.target.1.powi(2) - 1.0).abs() < 1e-6);
		let hard = Fader::new(0.0, 1.0, true, 48_000.0);
		assert!(hard.target.0.abs() < 1e-6 && (hard.target.1 - 1.0).abs() < 1e-6);
	}

	#[test]
	fn stereo_pan_is_balance() {
		let center = Fader::new(-6.0, 0.0, false, 48_000.0);
		let g = cadenza_core::db_to_gain(-6.0);
		assert_eq!(center.target, (g, g));
		let right = Fader::new(0.0, 0.5, false, 48_000.0);
		assert_eq!(right.target.1, 1.0);
		assert!(right.target.0 < 1.0 && right.target.0 > 0.0);
	}

	use cadenza_model::{Clip, Device, Note, Project, Track, TrackKind};

	/// One synth track with `(pitch, start tick, length ticks)` notes in a 4-beat clip.
	fn song(notes: &[(u8, i64, i64)]) -> Project {
		let mut p = Project::new("loop");
		let mut t = Track::new("synth", TrackKind::Instrument)
			.with_instrument(Device::builtin("poly_synth", "Synth"));
		t.clips.push(
			Clip::midi("c", Tick::ZERO, Tick(PPQ * 4)).with_notes(
				notes
					.iter()
					.map(|&(p, s, l)| Note::new(p, Tick(s), Tick(l), 0.8)),
			),
		);
		p.tracks.push(t);
		p
	}

	/// Note events of the first node over `quanta` quanta, as (stream sample, event):
	/// the stream sample counts output samples from the start, across wraps.
	fn played(g: &mut Graph, quanta: usize) -> Vec<(i64, MidiEvent)> {
		let mut out = Vec::new();
		for q in 0..quanta {
			g.render_quantum();
			let Source::Instrument { events, .. } = &g.nodes[0].source else {
				unreachable!()
			};
			for e in events {
				let offset = e.offset();
				if !matches!(e, MidiEvent::AllNotesOff { .. }) {
					out.push(((q * QUANTUM) as i64 + i64::from(offset), *e));
				}
			}
		}
		out
	}

	fn on(pitch: u8) -> impl Fn(&(i64, MidiEvent)) -> Option<i64> {
		move |(t, e)| matches!(e, MidiEvent::NoteOn { pitch: p, .. } if *p == pitch).then_some(*t)
	}

	fn off(pitch: u8) -> impl Fn(&(i64, MidiEvent)) -> Option<i64> {
		move |(t, e)| matches!(e, MidiEvent::NoteOff { pitch: p, .. } if *p == pitch).then_some(*t)
	}

	/// 1 tick = 25 samples at 120 bpm / 48 kHz. Loop ends of 961 ticks (24_025 samples,
	/// mid-quantum) and 960 ticks (24_000, on a quantum boundary).
	#[test]
	fn loop_wrap_releases_held_notes_and_replays_boundary_notes() {
		for end in [961i64, 960] {
			let len = end * 25;
			let p = song(&[
				// Starts at the loop start and crosses the loop end.
				(60, 0, PPQ * 2),
				// Starts exactly at the loop end: never plays.
				(64, end, PPQ),
				// Ends exactly at the loop end.
				(67, PPQ / 2, end - PPQ / 2),
			]);
			let mut g = crate::compile(&p, 48_000, 256).unwrap();
			g.set_loop(Some((Tick::ZERO, Tick(end))));
			g.play();
			let quanta = (3 * len as usize).div_ceil(QUANTUM) - 1;
			let ev = played(&mut g, quanta);
			let ons: Vec<i64> = ev.iter().filter_map(on(60)).collect();
			let offs: Vec<i64> = ev.iter().filter_map(off(60)).collect();
			assert_eq!(ons, vec![0, len, 2 * len], "end {end}");
			assert_eq!(offs, vec![len, 2 * len], "released at the wrap, end {end}");
			assert!(ev.iter().filter_map(on(64)).next().is_none(), "end {end}");
			let ons: Vec<i64> = ev.iter().filter_map(on(67)).collect();
			let offs: Vec<i64> = ev.iter().filter_map(off(67)).collect();
			assert_eq!(ons, vec![12_000, len + 12_000, 2 * len + 12_000]);
			assert_eq!(offs, vec![len, 2 * len], "one note-off per pass, end {end}");
			// The off of a held note precedes the next on of the same pitch at the wrap.
			let at_wrap: Vec<&MidiEvent> = ev
				.iter()
				.filter(|(t, _)| *t == len)
				.map(|(_, e)| e)
				.collect();
			let first_on = at_wrap
				.iter()
				.position(|e| matches!(e, MidiEvent::NoteOn { pitch: 60, .. }))
				.unwrap();
			let first_off = at_wrap
				.iter()
				.position(|e| matches!(e, MidiEvent::NoteOff { pitch: 60, .. }))
				.unwrap();
			assert!(first_off < first_on);
			assert!(g.engine_pos < len, "engine stays inside the loop");
		}
	}

	#[test]
	fn loop_wrap_chases_notes_covering_the_loop_start() {
		// Loop [1 beat, 2 beats): a note from beat 0 to 3 covers the whole loop.
		let p = song(&[(60, 0, PPQ * 3)]);
		let mut g = crate::compile(&p, 48_000, 256).unwrap();
		g.set_loop(Some((Tick(PPQ), Tick(PPQ * 2))));
		g.seek(Tick(PPQ));
		g.play();
		let ev = played(&mut g, 24_000 * 2 / QUANTUM + 4);
		let ons: Vec<i64> = ev.iter().filter_map(on(60)).collect();
		let offs: Vec<i64> = ev.iter().filter_map(off(60)).collect();
		// Chased at the start, then released and chased at each wrap.
		assert_eq!(ons, vec![0, 24_000, 48_000]);
		assert_eq!(offs, vec![24_000, 48_000]);
	}

	#[test]
	fn output_position_wraps_with_the_loop_and_tempo_changes() {
		let mut p = song(&[(60, 0, PPQ)]);
		// 120 bpm for a beat, then 60 bpm: beat 2 is at 24_000 + 48_000 samples.
		p.tempo.set_point(Tick(PPQ), 60.0);
		let mut g = crate::compile(&p, 48_000, 256).unwrap();
		g.set_loop(Some((Tick::ZERO, Tick(PPQ * 2))));
		assert_eq!(g.loop_samples(), Some((0, 72_000)));
		g.play();
		let mut l = vec![0.0; 1000];
		let mut r = vec![0.0; 1000];
		let mut max = 0;
		for _ in 0..200 {
			g.process(&mut l, &mut r);
			max = max.max(g.position_sample());
		}
		assert!(max < 72_000);
		// 200_000 samples played: 2 passes of 72_000 and 56_000 into the third.
		assert_eq!(g.position_sample(), 200_000 - 2 * 72_000);
		// Clearing the loop plays on from there.
		g.set_loop(None);
		g.process(&mut l, &mut r);
		assert_eq!(g.position_sample(), 57_000);
		// Too short to loop: ignored.
		g.set_loop(Some((Tick::ZERO, Tick(1))));
		assert_eq!(g.loop_samples(), None);
	}

	#[test]
	fn schedule_swaps_release_only_removed_notes() {
		let p = song(&[(60, 0, PPQ * 4), (64, 0, PPQ * 4)]);
		let track = p.tracks[0].id;
		let mut g = crate::compile(&p, 48_000, 256).unwrap();
		g.play();
		played(&mut g, 100);
		// Remove 64, keep 60.
		let mut edited = p.tracks[0].clips.clone();
		if let cadenza_model::ClipContent::Midi { notes, .. } = &mut edited[0].content {
			notes.retain(|n| n.pitch == 60);
		}
		let mut swap = g.prepare_schedule(track, &edited, &p.tempo).unwrap();
		assert!(g.swap_schedule(&mut swap));
		let ev = played(&mut g, 2);
		assert_eq!(
			ev.iter().map(|(_, e)| *e).collect::<Vec<_>>(),
			vec![MidiEvent::NoteOff {
				offset: 0,
				pitch: 64
			}],
			"60 keeps sounding, 64 is released"
		);
		// Another tempo or a non-instrument track cannot be swapped.
		assert!(
			g.prepare_schedule(track, &edited, &TempoMap::constant(90.0))
				.is_none()
		);
		assert!(
			g.prepare_schedule(TrackId::new(), &edited, &p.tempo)
				.is_none()
		);
	}

	#[test]
	fn pdc_delay_shifts_signal() {
		let mut e = Edge {
			accumulator: 0,
			send: None,
			gain: 1.0,
			target_gain: 1.0,
			pre_fader: false,
			delay: StereoDelay::new(3),
		};
		let mut src = StereoBuf::new();
		src.l[0] = 1.0;
		let mut dst = StereoBuf::new();
		e.mix(&src, &mut dst);
		assert_eq!(dst.l[3], 1.0);
		assert_eq!(dst.l.iter().sum::<f32>(), 1.0);
	}
}
