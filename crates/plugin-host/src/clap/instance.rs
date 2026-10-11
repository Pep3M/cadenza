//! A CLAP plugin instance behind Cadenza's real-time `Processor` trait.

use std::sync::Arc;

use cadenza_dsp::{DeviceInfo, MidiEvent, Processor, Transport};
use clack_extensions::params::PluginParams;
use clack_extensions::state::PluginState;
use clack_extensions::tail::{PluginTail, TailLength};
use clack_host::events::EventFlags;
use clack_host::events::EventHeader;
use clack_host::events::event_types::{TransportEvent, TransportFlags};
use clack_host::events::io::{InputEvents, OutputEvents};
use clack_host::prelude::*;
use clack_host::utils::BeatTime;

use super::events::{EventList, ParamQueue};
use super::host::{CadenzaHost, HostAudioProcessor, HostFlags};
use super::{
	Hosted, PortLayout, audio_config, new_instance, post_to_main, query_latency, query_ports,
	run_on_main,
};
use crate::descriptor::{PluginDescriptor, PluginRole};
use crate::error::PluginError;

/// Room for MIDI events per block on top of one slot per parameter.
const NOTE_EVENT_CAPACITY: usize = 1024;
/// Upper bound for a plugin-reported tail (an "infinite" tail would otherwise
/// make offline renders never end).
const MAX_TAIL_SECONDS: f32 = 30.0;

/// Default tail when the plugin has no tail extension: instruments release
/// notes after note-off; time-based effects ring out.
fn default_tail(desc: &PluginDescriptor) -> f32 {
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

/// A hosted CLAP plugin. Created by [`crate::instantiate`].
pub struct ClapProcessor {
	key: u64,
	id: String,
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
	sample_rate: f32,
	max_block: usize,
	latency: u32,
	tail_seconds: f32,
	default_tail: f32,
	steady_time: u64,
	/// The plugin refused to start or failed a process call; it now outputs
	/// silence (instruments) or passes audio through (effects).
	failed: bool,
}

impl ClapProcessor {
	pub(crate) fn create(
		desc: &PluginDescriptor,
		sample_rate: f32,
		max_block: usize,
		state: Option<&[u8]>,
	) -> Result<Self, PluginError> {
		let flags = Arc::new(HostFlags::default());
		let (id, path, host_flags) = (desc.id.clone(), desc.path.clone(), flags.clone());
		let (key, midi_dialect) = run_on_main(move |main| -> Result<_, PluginError> {
			let entry = main.entry(&path)?;
			let mut instance = new_instance(&entry, &id, &path, host_flags.clone())?;
			let caps = super::query_capabilities(&mut instance);
			let key = main.insert(Hosted {
				instance,
				flags: host_flags,
			});
			Ok((key, caps.midi_dialect))
		})??;

		let param_ids = desc
			.params
			.iter()
			.map(|p| {
				p.id.parse::<u32>()
					.ok()
					.and_then(ClapId::from_raw)
					.unwrap_or(ClapId::new(u32::MAX - 1))
			})
			.collect();
		let mut processor = Self {
			key,
			id: desc.id.clone(),
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
			sample_rate,
			max_block,
			latency: 0,
			tail_seconds: default_tail(desc),
			default_tail: default_tail(desc),
			steady_time: 0,
			failed: false,
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

	/// Delivers queued parameter changes without processing audio, so that a
	/// state save right after `set_param` reflects them. Not real-time.
	fn flush_params(&mut self) {
		let Some(audio) = self.audio.as_mut() else {
			return;
		};
		if self.queue.is_empty() {
			return;
		}
		self.events.clear();
		let (ids, events) = (&self.param_ids, &mut self.events);
		self.queue.drain(|i, v| {
			events.push_param(0, ids[i], v);
		});
		let mut handle = audio.plugin_handle();
		if let Some(params) = handle.get_extension::<PluginParams>() {
			params.flush_active(
				&mut handle,
				&InputEvents::from_buffer(&self.events),
				&mut OutputEvents::void(),
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
	let mut flags = TransportFlags::HAS_TEMPO | TransportFlags::HAS_BEATS_TIMELINE;
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
		bar_start: Default::default(),
		bar_number: 0,
		time_signature_numerator: 4,
		time_signature_denominator: 4,
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
		if n == 0 {
			return;
		}
		if self.failed || self.audio.is_none() {
			self.bypass(left, right);
			return;
		}
		let Some(audio) = self.audio.as_mut() else {
			return;
		};
		let Ok(started) = audio.ensure_processing_started() else {
			self.failed = true;
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

		// Inputs: the main port gets the track's stereo signal (down-mixed for
		// mono ports); every other channel is silent.
		let mut channel = 0;
		for (port, &count) in self.ports.inputs.iter().enumerate() {
			for c in 0..count as usize {
				let buf = &mut self.in_bufs[channel + c][..n];
				let is_main =
					Some(port) == self.ports.main_input && self.role == PluginRole::Effect;
				match (is_main, count, c) {
					(true, 1, 0) => {
						for ((o, l), r) in buf.iter_mut().zip(&*left).zip(&*right) {
							*o = 0.5 * (l + r);
						}
					}
					(true, _, 0) => buf.copy_from_slice(&left[..n]),
					(true, _, 1) => buf.copy_from_slice(&right[..n]),
					_ => buf.fill(0.0),
				}
			}
			channel += count as usize;
		}

		let transport_event = transport_event(transport);
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
				&mut OutputEvents::void(),
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

		let Some(main) = self.ports.main_output else {
			left[..n].fill(0.0);
			right[..n].fill(0.0);
			return;
		};
		let first: usize = self.ports.outputs[..main].iter().map(|&c| c as usize).sum();
		match self.ports.outputs[main] {
			0 => {
				left[..n].fill(0.0);
				right[..n].fill(0.0);
			}
			1 => {
				left[..n].copy_from_slice(&self.out_bufs[first][..n]);
				right[..n].copy_from_slice(&self.out_bufs[first][..n]);
			}
			_ => {
				left[..n].copy_from_slice(&self.out_bufs[first][..n]);
				right[..n].copy_from_slice(&self.out_bufs[first + 1][..n]);
			}
		}
	}

	fn reset(&mut self) {
		if let Some(audio) = self.audio.as_mut() {
			audio.reset();
		}
	}

	fn latency(&self) -> u32 {
		self.latency
	}

	fn tail_seconds(&self) -> f32 {
		self.tail_seconds
	}

	fn save_state(&mut self) -> Option<Vec<u8>> {
		self.flush_params();
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
}

impl Drop for ClapProcessor {
	fn drop(&mut self) {
		let key = self.key;
		let processor = self.audio.take().map(|mut a| {
			a.ensure_processing_stopped();
			a.into_stopped()
		});
		post_to_main(move |main| {
			if let Some(mut hosted) = main.instances.remove(&key) {
				if let Some(processor) = processor {
					hosted.instance.deactivate(processor);
				}
				drop(hosted);
			}
		});
	}
}
