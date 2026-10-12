//! The compiled render graph. Everything here runs on the audio thread (or the offline
//! render loop, which uses exactly the same code): no allocation, locks or I/O after
//! [`crate::compile`] returns.

use cadenza_core::{DeviceId, PPQ, TempoMap, Tick, TrackId};
use cadenza_dsp::{MidiEvent, Processor, Transport};

use crate::QUANTUM;
use crate::audio_clip::ClipPlayback;
use crate::automation::{AutoTarget, Binding, DeviceSlot};
use crate::hosting::{PluginCrash, PluginWatch};
use crate::meter::{MeterBank, Meters};
use crate::schedule::MidiSchedule;

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

	/// Adds `l`/`r` (at most one quantum) to `dst` through the delay.
	fn mix(&mut self, l: &[f32], r: &[f32], gain: f32, dst: &mut StereoBuf) {
		let n = l.len().min(r.len()).min(QUANTUM);
		if self.l.is_empty() {
			for i in 0..n {
				dst.l[i] += l[i] * gain;
				dst.r[i] += r[i] * gain;
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
			dst.l[i] += yl * gain;
			dst.r[i] += yr * gain;
		}
	}
}

/// A connection from a node to a summing accumulator (bus input or master input).
#[derive(Debug, Clone)]
pub(crate) struct Edge {
	pub accumulator: usize,
	pub gain: f32,
	pub pre_fader: bool,
	pub delay: StereoDelay,
}

impl Edge {
	fn mix(&mut self, src: &StereoBuf, dst: &mut StereoBuf) {
		self.delay.mix(&src.l, &src.r, self.gain, dst);
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
}

impl Device {
	/// Runs one quantum: aligns and hands over the key (if any), then processes `buf`.
	fn run(&mut self, buf: &mut StereoBuf, events: &[MidiEvent], t: &Transport, done: &[Node]) {
		if let Some(sc) = &mut self.sidechain
			&& let Some(src) = done.get(sc.source)
		{
			let tap = if sc.pre_fader { &src.pre } else { &src.buf };
			sc.key.l.copy_from_slice(&tap.l);
			sc.key.r.copy_from_slice(&tap.r);
			sc.key_delay.process(&mut sc.key);
			sc.main_delay.process(buf);
			self.processor.set_sidechain(&sc.key.l, &sc.key.r);
		}
		self.processor.process(&mut buf.l, &mut buf.r, events, t);
	}

	fn reset(&mut self) {
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
	},
	Audio {
		clips: Vec<ClipPlayback>,
	},
	Bus {
		accumulator: usize,
	},
}

/// `event` moved to the start of a quantum.
fn at_offset_zero(event: MidiEvent) -> MidiEvent {
	match event {
		MidiEvent::NoteOn {
			pitch, velocity, ..
		} => MidiEvent::NoteOn {
			offset: 0,
			pitch,
			velocity,
		},
		MidiEvent::NoteOff { pitch, .. } => MidiEvent::NoteOff { offset: 0, pitch },
		MidiEvent::AllNotesOff { .. } => MidiEvent::AllNotesOff { offset: 0 },
	}
}

pub(crate) struct Node {
	pub track_id: TrackId,
	pub name: String,
	/// Index in `Project::tracks` (display order).
	pub display_index: usize,
	pub source: Source,
	pub inserts: Vec<Device>,
	pub fader: Fader,
	/// False when muted or silenced by another track's solo.
	pub audible: bool,
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
	pub(crate) tempo: Vec<TempoSegment>,
	pub(crate) latency: u32,
	pub(crate) tail_seconds: f32,
	pub(crate) warnings: Vec<String>,
	/// Sandboxed plugin devices; never touched by `process`.
	pub(crate) plugin_watches: Vec<PluginWatch>,
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
	/// Per-track and master levels for the UI.
	pub(crate) meters: Meters,
}

impl Graph {
	pub fn sample_rate(&self) -> u32 {
		self.sample_rate
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

	/// Sets a channel's fader volume (`track: None` = master) without recompiling; the
	/// change is smoothed. Realtime safe. Returns false for an unknown track.
	pub fn set_channel_volume(&mut self, track: Option<TrackId>, db: f32) -> bool {
		match track {
			None => self.master.fader.set_volume_db(db),
			Some(id) => match self.nodes.iter_mut().find(|n| n.track_id == id) {
				Some(n) => n.fader.set_volume_db(db),
				None => return false,
			},
		}
		true
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

	/// The monitored track, if any.
	pub fn monitor_track(&self) -> Option<TrackId> {
		self.monitor.map(|i| self.nodes[i].track_id)
	}

	/// Queues a live event for the monitored instrument; it plays at the start of the next
	/// rendered quantum (its offset is ignored). Returns false when nothing is monitored,
	/// the transport is stopped or the buffer is full. Realtime safe (never allocates).
	pub fn push_live_event(&mut self, event: MidiEvent) -> bool {
		if self.monitor.is_none() || !self.playing || self.live.len() == self.live.capacity() {
			return false;
		}
		self.live.push(event);
		true
	}

	pub fn play(&mut self) {
		self.playing = true;
	}

	/// Stops output; sounding notes are released (AllNotesOff) and tails cleared on resume.
	pub fn stop(&mut self) {
		self.playing = false;
		self.live.clear();
		self.needs_reset = true;
		self.fifo_pos = QUANTUM;
		self.engine_pos = self.out_pos;
		self.discard = self.latency;
	}

	/// Moves the playhead to timeline sample `sample`. Sends AllNotesOff and clears all
	/// device state at the next quantum.
	pub fn seek_sample(&mut self, sample: i64) {
		self.out_pos = sample;
		self.engine_pos = sample;
		self.discard = self.latency;
		self.fifo_pos = QUANTUM;
		self.needs_reset = true;
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
		let n = left.len().min(right.len());
		if !self.playing {
			left.fill(0.0);
			right.fill(0.0);
			self.meters.silence();
			self.meters.publish();
			return;
		}
		while self.discard > 0 {
			if self.fifo_pos == QUANTUM {
				self.render_quantum();
				self.fifo_pos = 0;
			}
			let skip = (self.discard as usize).min(QUANTUM - self.fifo_pos);
			self.fifo_pos += skip;
			self.discard -= skip as u32;
		}
		let mut done = 0;
		while done < n {
			if self.fifo_pos == QUANTUM {
				self.render_quantum();
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
		self.out_pos += n as i64;
		self.meters.publish();
	}

	/// Clears all device and delay state at `engine_pos`, with automation applied first so
	/// that smoothed parameters start at their automated value instead of ramping to it.
	fn reset_state(&mut self) {
		let pos = self.engine_pos;
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
			for d in &mut node.inserts {
				d.reset();
			}
			for e in &mut node.edges {
				e.delay.clear();
			}
			for e in &mut node.aux_edges {
				e.delay.clear();
			}
			node.pre.clear();
			node.fader.snap();
		}
		for d in &mut self.master.inserts {
			d.reset();
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
					Some(i) => self.nodes[i].fader.set_volume_db(v),
					None => self.master.fader.set_volume_db(v),
				},
				AutoTarget::Pan { node } => match node {
					Some(i) => self.nodes[i].fader.set_pan(v),
					None => self.master.fader.set_pan(v),
				},
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
						d.processor.set_param(index, v);
					}
				}
			}
		}
	}

	/// Renders one quantum starting at `engine_pos` into `master.buf` (and every node's
	/// post-fader `buf`), then advances `engine_pos`.
	pub(crate) fn render_quantum(&mut self) {
		let all_notes_off = self.needs_reset;
		if self.needs_reset {
			self.reset_state();
			self.needs_reset = false;
		}
		self.apply_automation();
		let pos = self.engine_pos;
		let seg = self.segment_at(pos as f64);
		let transport = Transport {
			bpm: seg.bpm,
			playing: true,
			beat_position: self.beat_at(pos).max(0.0),
		};
		for acc in &mut self.accumulators {
			acc.clear();
		}
		let Self {
			nodes,
			accumulators,
			master,
			monitor,
			monitor_release,
			live,
			meters,
			..
		} = self;
		for i in 0..nodes.len() {
			let (done, rest) = nodes.split_at_mut(i);
			let node = &mut rest[0];
			let buf = &mut node.buf;
			buf.clear();
			match &mut node.source {
				Source::Instrument {
					device,
					schedule,
					events,
					notes_off_on_reset,
					midi_delay,
				} => {
					let pos = pos - i64::from(*midi_delay);
					events.clear();
					if all_notes_off {
						if *notes_off_on_reset {
							events.push(MidiEvent::AllNotesOff { offset: 0 });
						}
						// Chase notes that should already be sounding at `pos` so a
						// start/seek landing mid-note doesn't leave it silent.
						schedule.chase(pos, events);
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
					schedule.collect(pos, QUANTUM, events);
					if let Some(d) = device {
						d.run(buf, events, &transport, done);
						for e in &mut node.aux_edges {
							let Some((l, r)) = d.processor.aux_output(e.port) else {
								continue;
							};
							match e.accumulator {
								Some(a) => e.delay.mix(l, r, 1.0, &mut accumulators[a]),
								None => e.delay.mix(l, r, 1.0, buf),
							}
						}
					}
				}
				Source::Audio { clips } => {
					for c in clips.iter() {
						c.mix(pos, &mut buf.l, &mut buf.r);
					}
				}
				Source::Bus { accumulator } => {
					let acc = &accumulators[*accumulator];
					buf.l.copy_from_slice(&acc.l);
					buf.r.copy_from_slice(&acc.r);
				}
			}
			for d in &mut node.inserts {
				d.run(buf, &[], &transport, done);
			}
			if node.tap_pre {
				node.pre.l.copy_from_slice(&buf.l);
				node.pre.r.copy_from_slice(&buf.r);
			}
			if !node.audible {
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
			d.run(&mut master.buf, &[], &transport, nodes);
		}
		master.fader.apply(&mut master.buf);
		meters.measure(None, &master.buf.l, &master.buf.r);
		self.engine_pos += QUANTUM as i64;
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

	#[test]
	fn pdc_delay_shifts_signal() {
		let mut e = Edge {
			accumulator: 0,
			gain: 1.0,
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
