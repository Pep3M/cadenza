//! A CLAP plugin instance behind Cadenza's real-time `Processor` trait.

use std::sync::Arc;

use cadenza_dsp::{DeviceInfo, MidiEvent, Processor, Transport};
use clack_extensions::params::PluginParams;
use clack_extensions::state::PluginState;
use clack_extensions::tail::{PluginTail, TailLength};
use clack_host::events::EventFlags;
use clack_host::events::EventHeader;
use clack_host::events::event_types::{TransportEvent, TransportFlags};
use clack_host::events::io::{InputEvents, OutputEventBuffer, OutputEvents, TryPushError};
use clack_host::events::spaces::CoreEventSpace;
use clack_host::prelude::*;
use clack_host::utils::BeatTime;

use super::events::{EventList, ParamQueue};
use super::host::{CadenzaHost, HostAudioProcessor, HostFlags};
use super::{
	Hosted, PortLayout, audio_config, new_instance, query_latency, query_ports, run_on_main,
};
use crate::descriptor::{PluginDescriptor, PluginFormat, PluginRole};
use crate::editor::{EditorShared, FeedbackKind, FeedbackSender, LocalEditor, PluginEditor};
use crate::error::PluginError;
use crate::ports::KeyInput;

/// Room for MIDI events per block on top of one slot per parameter.
pub(crate) const NOTE_EVENT_CAPACITY: usize = 1024;
/// Upper bound for a plugin-reported tail (an "infinite" tail would otherwise
/// make offline renders never end).
pub(crate) const MAX_TAIL_SECONDS: f32 = 30.0;

/// Default tail when the plugin has no tail extension: instruments release
/// notes after note-off; time-based effects ring out.
pub(crate) fn default_tail(desc: &PluginDescriptor) -> f32 {
	match desc.role {
		PluginRole::Instrument => 0.5,
		PluginRole::Effect => {
			let ringing = ["reverb", "delay"];
			if desc.features.iter().any(|f| ringing.contains(&f.as_str())) {
				3.0
			} else {
				0.0
			}
		}
	}
}

/// Collects the parameter events a plugin outputs (edits made in its GUI)
/// into the editor feedback ring. Realtime-safe: lookups in a sorted slice,
/// pushes into a wait-free ring, no allocation.
struct ParamOutput<'a> {
	/// (CLAP param id, descriptor index), sorted by id.
	lookup: &'a [(u32, u32)],
	sender: &'a mut FeedbackSender,
}

impl ParamOutput<'_> {
	fn send(&mut self, kind: FeedbackKind, id: Option<ClapId>, value: f64) {
		let Some(id) = id else {
			return;
		};
		if let Ok(i) = self.lookup.binary_search_by_key(&id.get(), |&(id, _)| id) {
			self.sender.send(kind, self.lookup[i].1, value);
		}
	}
}

impl OutputEventBuffer for ParamOutput<'_> {
	fn try_push(&mut self, event: &UnknownEvent) -> Result<(), TryPushError> {
		match event.as_core_event() {
			Some(CoreEventSpace::ParamValue(e)) => {
				self.send(FeedbackKind::Value, e.param_id(), e.value());
			}
			Some(CoreEventSpace::ParamGestureBegin(e)) => {
				self.send(FeedbackKind::Begin, e.param_id(), 0.0);
			}
			Some(CoreEventSpace::ParamGestureEnd(e)) => {
				self.send(FeedbackKind::End, e.param_id(), 0.0);
			}
			_ => {}
		}
		Ok(())
	}
}

/// A hosted CLAP plugin. Created by [`crate::instantiate`].
pub struct ClapProcessor {
	key: u64,
	id: String,
	path: std::path::PathBuf,
	info: &'static DeviceInfo,
	role: PluginRole,
	/// CLAP param id for each `set_param` index (descriptor order).
	param_ids: Vec<ClapId>,
	param_ranges: Vec<(f64, f64, bool)>,
	queue: ParamQueue,
	events: EventList,
	audio: Option<PluginAudioProcessor<CadenzaHost>>,
	flags: Arc<HostFlags>,
	midi_dialect: bool,
	ports: PortLayout,
	in_bufs: Vec<Vec<f32>>,
	out_bufs: Vec<Vec<f32>>,
	in_ports: AudioPorts,
	out_ports: AudioPorts,
	sidechain: KeyInput,
	/// Frames of the last successful `process` (for `aux_output`).
	last_frames: usize,
	sample_rate: f32,
	max_block: usize,
	latency: u32,
	tail_seconds: f32,
	default_tail: f32,
	steady_time: u64,
	/// The plugin refused to start or failed a process call; it now outputs
	/// silence (instruments) or passes audio through (effects).
	failed: bool,
	/// (CLAP param id, descriptor index), sorted by id.
	param_lookup: Vec<(u32, u32)>,
	feedback: FeedbackSender,
	editor: Arc<EditorShared>,
}

impl ClapProcessor {
	pub(crate) fn create(
		desc: &PluginDescriptor,
		sample_rate: f32,
		max_block: usize,
		state: Option<&[u8]>,
	) -> Result<Self, PluginError> {
		let flags = Arc::new(HostFlags::default());
		let (editor, ring) =
			EditorShared::new(&desc.id, desc.params.iter().map(|p| p.id.clone()).collect());
		let (id, path, host_flags) = (desc.id.clone(), desc.path.clone(), flags.clone());
		let shared = editor.clone();
		let (key, midi_dialect) = run_on_main(move |main| -> Result<_, PluginError> {
			let entry = main.entry(&path)?;
			let mut instance = new_instance(&entry, &id, &path, host_flags.clone())?;
			let caps = super::query_capabilities(&mut instance);
			let key = main.insert(Hosted {
				instance,
				flags: host_flags,
				editor: None,
				shared: Some(shared),
			});
			Ok((key, caps.midi_dialect))
		})??;

		let param_ids: Vec<ClapId> = desc
			.params
			.iter()
			.map(|p| {
				p.id.parse::<u32>()
					.ok()
					.and_then(ClapId::from_raw)
					.unwrap_or(ClapId::new(u32::MAX - 1))
			})
			.collect();
		let mut param_lookup: Vec<(u32, u32)> = param_ids
			.iter()
			.enumerate()
			.map(|(i, id)| (id.get(), i as u32))
			.collect();
		param_lookup.sort_unstable();
		let mut processor = Self {
			key,
			id: desc.id.clone(),
			path: desc.path.clone(),
			info: desc.device_info(),
			role: desc.role,
			param_ids,
			param_ranges: desc
				.params
				.iter()
				.map(|p| (p.min, p.max, p.stepped))
				.collect(),
			queue: ParamQueue::new(desc.params.len()),
			events: EventList::with_capacity(desc.params.len() + NOTE_EVENT_CAPACITY),
			audio: None,
			flags,
			midi_dialect,
			ports: PortLayout::default(),
			in_bufs: Vec::new(),
			out_bufs: Vec::new(),
			in_ports: AudioPorts::with_capacity(0, 0),
			out_ports: AudioPorts::with_capacity(0, 0),
			sidechain: KeyInput::default(),
			last_frames: 0,
			sample_rate,
			max_block,
			latency: 0,
			tail_seconds: default_tail(desc),
			default_tail: default_tail(desc),
			steady_time: 0,
			failed: false,
			param_lookup,
			feedback: FeedbackSender::new(ring),
			editor,
		};
		if let Some(state) = state
			&& !processor.load_state(state)
		{
			return Err(PluginError::State {
				id: desc.id.clone(),
				reason: "the plugin's state extension refused the data".into(),
			});
		}
		processor.activate(sample_rate, max_block)?;
		Ok(processor)
	}

	/// (Re-)activates the plugin for a new configuration. Not real-time.
	fn activate(&mut self, sample_rate: f32, max_block: usize) -> Result<(), PluginError> {
		let max_block = max_block.max(1);
		let previous = self.audio.take().map(|mut a| {
			a.ensure_processing_stopped();
			a.into_stopped()
		});
		let key = self.key;
		let config = audio_config(
			f64::from(sample_rate),
			u32::try_from(max_block).unwrap_or(u32::MAX),
		);
		let result = run_on_main(move |main| {
			let hosted = main
				.instances
				.get_mut(&key)
				.ok_or_else(|| "instance no longer exists".to_string())?;
			if let Some(previous) = previous {
				hosted.instance.deactivate(previous);
			}
			let ports = query_ports(&mut hosted.instance);
			let processor = hosted
				.instance
				.activate(|shared, _| HostAudioProcessor::new(shared), config)
				.map_err(|e| e.to_string())?;
			let latency = query_latency(&mut hosted.instance);
			Ok::<_, String>((processor, ports, latency))
		})?;
		let (processor, ports, latency) = result.map_err(|reason| PluginError::Activate {
			id: self.id.clone(),
			sample_rate,
			max_block,
			reason,
		})?;

		let total_in: usize = ports.inputs.iter().map(|&c| c as usize).sum();
		let total_out: usize = ports.outputs.iter().map(|&c| c as usize).sum();
		self.in_bufs = vec![vec![0.0; max_block]; total_in];
		self.out_bufs = vec![vec![0.0; max_block]; total_out];
		self.in_ports = AudioPorts::with_capacity(total_in, ports.inputs.len());
		self.out_ports = AudioPorts::with_capacity(total_out, ports.outputs.len());
		self.ports = ports;
		self.sidechain.allocate(max_block);
		self.last_frames = 0;
		self.latency = latency;
		self.sample_rate = sample_rate;
		self.max_block = max_block;
		self.steady_time = 0;
		self.failed = false;
		HostFlags::take(&self.flags.latency_changed);
		HostFlags::take(&self.flags.restart);

		let mut audio: PluginAudioProcessor<CadenzaHost> = processor.into();
		self.tail_seconds = {
			let handle = audio.plugin_handle();
			match handle.get_extension::<PluginTail>().map(|t| t.get(&handle)) {
				Some(TailLength::Finite(n)) => (n as f32 / sample_rate).min(MAX_TAIL_SECONDS),
				Some(TailLength::Infinite) => MAX_TAIL_SECONDS,
				None => self.default_tail,
			}
		};
		self.audio = Some(audio);
		Ok(())
	}

	/// A handle to this instance's editor (usable while the processor runs on
	/// the audio thread).
	pub fn editor(&self) -> PluginEditor {
		PluginEditor::local(LocalEditor {
			format: PluginFormat::Clap,
			key: self.key,
			shared: self.editor.clone(),
		})
	}

	/// Delivers queued parameter changes without processing audio (CLAP
	/// `params.flush`), collecting the plugin's own parameter events (GUI
	/// edits). Realtime-safe; `force` flushes even with nothing queued (the
	/// plugin asked for it).
	fn flush_params(&mut self, force: bool) {
		let Some(audio) = self.audio.as_mut() else {
			return;
		};
		if self.queue.is_empty() && !force {
			return;
		}
		self.events.clear();
		let (ids, events) = (&self.param_ids, &mut self.events);
		self.queue.drain(|i, v| {
			events.push_param(0, ids[i], v);
		});
		let mut handle = audio.plugin_handle();
		if let Some(params) = handle.get_extension::<PluginParams>() {
			let mut output = ParamOutput {
				lookup: &self.param_lookup,
				sender: &mut self.feedback,
			};
			params.flush_active(
				&mut handle,
				&InputEvents::from_buffer(&self.events),
				&mut OutputEvents::from_buffer(&mut output),
			);
		}
		self.events.clear();
	}

	fn bypass(&self, left: &mut [f32], right: &mut [f32]) {
		if self.role == PluginRole::Instrument {
			left.fill(0.0);
			right.fill(0.0);
		}
	}
}

fn transport_event(t: &Transport) -> TransportEvent {
	let mut flags = TransportFlags::HAS_TEMPO
		| TransportFlags::HAS_BEATS_TIMELINE
		| TransportFlags::HAS_TIME_SIGNATURE;
	if t.playing {
		flags |= TransportFlags::IS_PLAYING;
	}
	TransportEvent {
		header: EventHeader::new_core(0, EventFlags::empty()),
		flags,
		song_pos_beats: BeatTime::from_float(t.beat_position),
		song_pos_seconds: Default::default(),
		tempo: t.bpm,
		tempo_inc: 0.0,
		loop_start_beats: Default::default(),
		loop_end_beats: Default::default(),
		loop_start_seconds: Default::default(),
		loop_end_seconds: Default::default(),
		bar_start: BeatTime::from_float(t.bar_start),
		bar_number: t.bar_number,
		time_signature_numerator: t.time_sig_numerator.max(1),
		time_signature_denominator: t.time_sig_denominator.max(1),
	}
}

impl Processor for ClapProcessor {
	fn info(&self) -> &'static DeviceInfo {
		self.info
	}

	fn prepare(&mut self, sample_rate: f32, max_block: usize) {
		let restart = HostFlags::take(&self.flags.restart);
		if self.audio.is_some()
			&& !restart
			&& sample_rate == self.sample_rate
			&& max_block.max(1) == self.max_block
		{
			return;
		}
		if let Err(e) = self.activate(sample_rate, max_block) {
			eprintln!("cadenza-plugin-host: {e}");
		}
	}

	fn set_param(&mut self, index: usize, value: f32) {
		let Some(&(min, max, stepped)) = self.param_ranges.get(index) else {
			return;
		};
		if value.is_nan() {
			return;
		}
		let mut v = f64::from(value).clamp(min, max);
		if stepped {
			v = v.round();
		}
		self.queue.set(index, v);
	}

	fn process(
		&mut self,
		left: &mut [f32],
		right: &mut [f32],
		events: &[MidiEvent],
		transport: &Transport,
	) {
		let n = left.len().min(right.len()).min(self.max_block);
		self.last_frames = 0;
		if n == 0 {
			self.sidechain.clear();
			return;
		}
		if self.failed || self.audio.is_none() {
			self.sidechain.clear();
			self.bypass(left, right);
			return;
		}
		let Some(audio) = self.audio.as_mut() else {
			return;
		};
		let Ok(started) = audio.ensure_processing_started() else {
			self.failed = true;
			self.sidechain.clear();
			self.bypass(left, right);
			return;
		};

		self.events.clear();
		{
			let (ids, list) = (&self.param_ids, &mut self.events);
			self.queue.drain(|i, v| {
				list.push_param(0, ids[i], v);
			});
		}
		for e in events {
			self.events.push_midi(e, n as u32, self.midi_dialect);
		}

		// Inputs: the main port gets the track's signal (effects), the
		// sidechain port the key; every other channel is silent.
		let main = (self.role == PluginRole::Effect).then_some((&left[..n], &right[..n]));
		let key = self.sidechain.take();
		self.ports.fill_inputs(&mut self.in_bufs, n, main, key);

		let transport_event = transport_event(transport);
		HostFlags::take(&self.flags.flush);
		let mut output = ParamOutput {
			lookup: &self.param_lookup,
			sender: &mut self.feedback,
		};
		let status = {
			let mut rest: &mut [Vec<f32>] = &mut self.in_bufs;
			let inputs = self
				.in_ports
				.with_input_buffers(self.ports.inputs.iter().map(|&count| {
					let (head, tail) = std::mem::take(&mut rest).split_at_mut(count as usize);
					rest = tail;
					AudioPortBuffer {
						latency: 0,
						channels: AudioPortBufferType::f32_input_only(
							head.iter_mut().map(|b| InputChannel::variable(&mut b[..n])),
						),
					}
				}));
			let mut rest: &mut [Vec<f32>] = &mut self.out_bufs;
			let mut outputs = self
				.out_ports
				.with_output_buffers(self.ports.outputs.iter().map(|&count| {
					let (head, tail) = std::mem::take(&mut rest).split_at_mut(count as usize);
					rest = tail;
					AudioPortBuffer {
						latency: 0,
						channels: AudioPortBufferType::f32_output_only(
							head.iter_mut().map(|b| &mut b[..n]),
						),
					}
				}));
			started.process(
				&inputs,
				&mut outputs,
				&InputEvents::from_buffer(&self.events),
				&mut OutputEvents::from_buffer(&mut output),
				Some(self.steady_time),
				Some(&transport_event),
			)
		};
		self.steady_time += n as u64;
		if status.is_err() {
			self.failed = true;
			self.bypass(left, right);
			return;
		}

		self.last_frames = n;
		match self
			.ports
			.main_output
			.and_then(|main| self.ports.output_pair(&self.out_bufs, main, n))
		{
			Some((l, r)) => {
				left[..n].copy_from_slice(l);
				right[..n].copy_from_slice(r);
			}
			None => {
				left[..n].fill(0.0);
				right[..n].fill(0.0);
			}
		}
	}

	fn has_sidechain(&self) -> bool {
		self.ports.sidechain_input.is_some()
	}

	fn set_sidechain(&mut self, left: &[f32], right: &[f32]) {
		self.sidechain.set(left, right);
	}

	fn aux_output_count(&self) -> usize {
		self.ports.aux_output_ports().count()
	}

	fn aux_output(&self, index: usize) -> Option<(&[f32], &[f32])> {
		self.ports.aux_pair(&self.out_bufs, index, self.last_frames)
	}

	fn reset(&mut self) {
		if let Some(audio) = self.audio.as_mut() {
			audio.reset();
		}
	}

	fn idle(&mut self) {
		if self.failed {
			return;
		}
		let requested = HostFlags::take(&self.flags.flush);
		self.flush_params(requested);
	}

	fn latency(&self) -> u32 {
		self.latency
	}

	fn tail_seconds(&self) -> f32 {
		self.tail_seconds
	}

	fn save_state(&mut self) -> Option<Vec<u8>> {
		self.flush_params(false);
		let key = self.key;
		run_on_main(move |main| {
			let hosted = main.instances.get_mut(&key)?;
			let handle = hosted.instance.plugin_handle();
			let state = handle.get_extension::<PluginState>()?;
			let mut data = Vec::new();
			state.save(&handle, &mut data).ok()?;
			Some(data)
		})
		.ok()
		.flatten()
	}

	fn load_state(&mut self, data: &[u8]) -> bool {
		let key = self.key;
		let data = data.to_vec();
		run_on_main(move |main| {
			let Some(hosted) = main.instances.get_mut(&key) else {
				return false;
			};
			let handle = hosted.instance.plugin_handle();
			let Some(state) = handle.get_extension::<PluginState>() else {
				return false;
			};
			state.load(&handle, &mut data.as_slice()).is_ok()
		})
		.unwrap_or(false)
	}

	fn presets(&mut self) -> Vec<cadenza_dsp::PresetInfo> {
		let (path, id) = (self.path.clone(), self.id.clone());
		run_on_main(move |main| {
			main.entry(&path)
				.map(|entry| super::presets::discover(&entry, &id))
				.unwrap_or_default()
		})
		.unwrap_or_default()
	}

	fn load_preset(&mut self, preset: &str) -> bool {
		let key = self.key;
		let preset = preset.to_string();
		run_on_main(move |main| {
			main.instances
				.get_mut(&key)
				.is_some_and(|h| super::presets::load(&mut h.instance, &preset))
		})
		.unwrap_or(false)
	}
}

impl Drop for ClapProcessor {
	fn drop(&mut self) {
		let key = self.key;
		let processor = self.audio.take().map(|mut a| {
			a.ensure_processing_stopped();
			a.into_stopped()
		});
		crate::main_thread::post_to_main(move |main| {
			// The editor goes first: its view must not outlive the instance.
			super::editor::close(main, key);
			if let Some(mut hosted) = main.clap.instances.remove(&key) {
				if let Some(processor) = processor {
					hosted.instance.deactivate(processor);
				}
				drop(hosted);
			}
		});
	}
}
