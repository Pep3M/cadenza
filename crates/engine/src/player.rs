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
//! Plugin devices are sandboxed by default (see [`crate::hosting`]): a crashing or hanging
//! plugin is bypassed on the audio thread by its sandbox proxy, and the control side
//! notices it through the sandbox monitors ([`Player::plugin_crashes`],
//! [`Player::take_plugin_crashes`]). Crashed plugins are restarted by the next
//! [`Player::load`], which re-creates every device from the project.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::time::{Duration, Instant};

use cadenza_core::Tick;
use cadenza_model::Project;
use cadenza_plugin_host::PluginCatalog;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};
use rtrb::{Consumer, Producer, RingBuffer};

use crate::compile_hosted;
use crate::error::EngineError;
use crate::graph::Graph;
use crate::hosting::{PluginCrash, PluginHosting, PluginWatch};

const GRAPH_SLOTS: usize = 4;
const GARBAGE_SLOTS: usize = 8;
const COMMAND_SLOTS: usize = 256;
/// Frames rendered per inner step of the callback (scratch buffer size).
const SCRATCH_FRAMES: usize = 1024;

#[derive(Debug, Clone, Copy, PartialEq)]
enum Cmd {
	Play,
	Stop,
	Seek(Tick),
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
	shared: Arc<Shared>,
	playing: bool,
	position: Tick,
	scratch_l: Vec<f32>,
	scratch_r: Vec<f32>,
}

/// Control side: everything except the cpal stream, so it can be tested without a device.
struct Control {
	graphs: Producer<Box<Graph>>,
	garbage: Consumer<Box<Graph>>,
	commands: Producer<Cmd>,
	shared: Arc<Shared>,
}

fn channel_pair() -> (Control, AudioCore) {
	let (graph_tx, graph_rx) = RingBuffer::new(GRAPH_SLOTS);
	let (garbage_tx, garbage_rx) = RingBuffer::new(GARBAGE_SLOTS);
	let (cmd_tx, cmd_rx) = RingBuffer::new(COMMAND_SLOTS);
	let shared = Arc::new(Shared::default());
	(
		Control {
			graphs: graph_tx,
			garbage: garbage_rx,
			commands: cmd_tx,
			shared: shared.clone(),
		},
		AudioCore {
			graph: None,
			graphs: graph_rx,
			garbage: garbage_tx,
			commands: cmd_rx,
			shared,
			playing: false,
			position: Tick::ZERO,
			scratch_l: vec![0.0; SCRATCH_FRAMES],
			scratch_r: vec![0.0; SCRATCH_FRAMES],
		},
	)
}

impl AudioCore {
	fn begin_block(&mut self) {
		// Only take a new graph when the old one can be handed back.
		if self.garbage.slots() > 0
			&& let Ok(mut next) = self.graphs.pop()
		{
			next.seek(self.position);
			if self.playing {
				next.play();
			}
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
	}

	fn send(&mut self, cmd: Cmd) -> Result<(), EngineError> {
		self.collect_garbage();
		self.commands.push(cmd).map_err(|_| EngineError::Busy)
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

/// Realtime player on the default output device.
pub struct Player {
	control: Control,
	sample_rate: u32,
	plugins: Option<Arc<PluginCatalog>>,
	hosting: PluginHosting,
	/// Sandboxed devices of the last loaded graph, and whether each crash was taken.
	watches: Vec<(PluginWatch, bool)>,
	_stream: cpal::Stream,
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
		let (control, core) = channel_pair();
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
			_stream: stream,
		})
	}

	/// Output device sample rate; projects are compiled at this rate.
	pub fn sample_rate(&self) -> u32 {
		self.sample_rate
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
		let graph = compile_hosted(
			project,
			self.sample_rate,
			crate::QUANTUM,
			self.plugins.as_deref(),
			&self.hosting,
		)?;
		let warnings = graph.warnings().to_vec();
		let watches = graph.plugin_watches.clone();
		self.control.load_graph(graph)?;
		self.watches = watches.into_iter().map(|w| (w, false)).collect();
		Ok(warnings)
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
		let (mut control, mut core) = channel_pair();
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
		let (mut control, mut core) = channel_pair();
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
	fn mono_and_multichannel_layouts() {
		let (mut control, mut core) = channel_pair();
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
}
