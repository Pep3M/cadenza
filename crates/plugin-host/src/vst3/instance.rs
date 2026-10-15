//! A VST3 plugin instance behind Cadenza's real-time `Processor` trait.

use std::sync::Arc;

use cadenza_dsp::{DeviceInfo, MidiEvent, Processor, Transport};
use vst3::ComPtr;
use vst3::ComWrapper;
use vst3::Steinberg::Vst::{
	AudioBusBuffers, AudioBusBuffers__type0, IAudioProcessor, IAudioProcessorTrait, IEventList,
	IParameterChanges, ProcessContext, ProcessContext_, ProcessData, ProcessModes_,
	SymbolicSampleSizes_, kInfiniteTail,
};
use vst3::Steinberg::{kResultFalse, kResultOk};

use super::com::{SendPtr, Vst3Flags, tuid_from_hex};
use super::editor::{Attachment, ParamMirror};
use super::plugin::Vst3Plugin;
use super::rt::{EventList, HeldNotes, ParamCurves, ParameterChanges, iface};
use crate::ProcessMode;
use crate::clap::PortLayout;
use crate::clap::events::ParamQueue;
use crate::clap::instance::{MAX_TAIL_SECONDS, NOTE_EVENT_CAPACITY, default_tail};
use crate::descriptor::{PluginDescriptor, PluginFormat, PluginRole};
use crate::editor::{EditorShared, LocalEditor, PluginEditor};
use crate::error::PluginError;
use crate::main_thread::{post_to_main, run_on_main};
use crate::ports::KeyInput;

/// GUI edits in flight from the controller to the processor.
const GUI_EDIT_CAPACITY: usize = 512;

/// Room for parameter changes the plugin reports back (ignored, but the
/// plugin must have somewhere to put them).
const OUTPUT_PARAM_CAPACITY: usize = 128;

/// Pre-allocated audio buffers and the `AudioBusBuffers` that point into
/// them. Rebuilt only on (re-)activation.
struct Buffers {
	inputs: Vec<Vec<f32>>,
	outputs: Vec<Vec<f32>>,
	/// Channel pointer arrays the bus buffers point into; never read
	/// directly, but they must live as long as `in_buses`/`out_buses`.
	_in_ptrs: Vec<*mut f32>,
	_out_ptrs: Vec<*mut f32>,
	in_buses: Vec<AudioBusBuffers>,
	out_buses: Vec<AudioBusBuffers>,
}

// SAFETY: the raw pointers only point into the heap buffers owned by the same
// struct (stable across moves), and the struct is used by one thread at a time.
unsafe impl Send for Buffers {}

impl Buffers {
	fn empty() -> Self {
		Self::new(&PortLayout::default(), 0)
	}

	fn new(layout: &PortLayout, max_block: usize) -> Self {
		let total = |v: &[u32]| v.iter().map(|&c| c as usize).sum::<usize>();
		let mut inputs = vec![vec![0.0f32; max_block]; total(&layout.inputs)];
		let mut outputs = vec![vec![0.0f32; max_block]; total(&layout.outputs)];
		let mut in_ptrs: Vec<*mut f32> = inputs.iter_mut().map(|b| b.as_mut_ptr()).collect();
		let mut out_ptrs: Vec<*mut f32> = outputs.iter_mut().map(|b| b.as_mut_ptr()).collect();
		let buses = |counts: &[u32], ptrs: &mut Vec<*mut f32>| -> Vec<AudioBusBuffers> {
			let mut first = 0;
			counts
				.iter()
				.map(|&c| {
					// SAFETY: `first <= ptrs.len()`; a zero-channel bus never
					// dereferences its pointer.
					let channels = unsafe { ptrs.as_mut_ptr().add(first) };
					first += c as usize;
					AudioBusBuffers {
						numChannels: c as i32,
						silenceFlags: 0,
						__field0: AudioBusBuffers__type0 {
							channelBuffers32: channels,
						},
					}
				})
				.collect()
		};
		let in_buses = buses(&layout.inputs, &mut in_ptrs);
		let out_buses = buses(&layout.outputs, &mut out_ptrs);
		Self {
			inputs,
			outputs,
			_in_ptrs: in_ptrs,
			_out_ptrs: out_ptrs,
			in_buses,
			out_buses,
		}
	}
}

/// A hosted VST3 plugin. Created by [`crate::instantiate`].
pub struct Vst3Processor {
	key: u64,
	id: String,
	info: &'static DeviceInfo,
	role: PluginRole,
	mode: ProcessMode,
	/// VST3 `ParamID` for each `set_param` index (descriptor order).
	param_ids: Vec<u32>,
	param_ranges: Vec<(f64, f64, bool)>,
	curves: ParamCurves,
	/// Pending plain values from `set_param`.
	queue: ParamQueue,
	/// Normalized values sent to the processor on the host's behalf, mirrored
	/// into the controller off the audio thread (GUI, state saves).
	mirror: Arc<ParamMirror>,
	/// Edits made in the plugin's GUI (normalized), for the processor.
	gui_edits: rtrb::Consumer<(u32, f64)>,
	/// GUI edits delivered by an idle (zero-sample) flush, sent again with the
	/// next block: not every plugin applies changes in a zero-sample call.
	gui_resend: Vec<(u32, f64)>,
	editor: Arc<EditorShared>,
	events: ComWrapper<EventList>,
	out_events: ComWrapper<EventList>,
	changes: ComWrapper<ParameterChanges>,
	out_changes: ComWrapper<ParameterChanges>,
	held: HeldNotes,
	release_held: bool,
	/// `IMidiMapping`: the parameter of each controller (0..=127 CC, 128 pressure,
	/// 129 pitch bend); empty = the plugin maps none (controllers are dropped).
	midi_map: Vec<Option<u32>>,
	context: ProcessContext,
	audio: Option<SendPtr<ComPtr<IAudioProcessor>>>,
	flags: Arc<Vst3Flags>,
	layout: PortLayout,
	buffers: Buffers,
	sidechain: KeyInput,
	/// Frames of the last successful `process` (for `aux_output`).
	last_frames: usize,
	event_inputs: bool,
	sample_rate: f32,
	max_block: usize,
	latency: u32,
	tail_seconds: f32,
	default_tail: f32,
	continuous_samples: i64,
	/// The plugin failed to activate or returned an error from `process`; it
	/// now outputs silence (instruments) or passes audio through (effects).
	failed: bool,
}

impl Vst3Processor {
	pub(crate) fn create(
		desc: &PluginDescriptor,
		sample_rate: f32,
		max_block: usize,
		state: Option<&[u8]>,
		mode: ProcessMode,
	) -> Result<Self, PluginError> {
		let inst_err = |reason: String| PluginError::Instantiate {
			id: desc.id.clone(),
			path: desc.path.clone(),
			reason,
		};
		let cid = tuid_from_hex(&desc.id)
			.ok_or_else(|| inst_err("VST3 class ids are 32 hex digits".into()))?;
		let param_ids: Vec<u32> = desc
			.params
			.iter()
			.map(|p| p.id.parse::<u32>().unwrap_or(u32::MAX))
			.collect();
		let stepped: Vec<bool> = desc.params.iter().map(|p| p.stepped).collect();
		let flags = Arc::new(Vst3Flags::default());
		let (gui_tx, gui_edits) = rtrb::RingBuffer::new(GUI_EDIT_CAPACITY);
		*flags.to_audio.lock().unwrap_or_else(|e| e.into_inner()) = Some(gui_tx);
		let (editor, _) =
			EditorShared::new(&desc.id, desc.params.iter().map(|p| p.id.clone()).collect());
		let mirror = Arc::new(ParamMirror::new(desc.params.len()));
		let attachment = Attachment::new(editor.clone(), mirror.clone(), param_ids.clone());

		enum Failure {
			Plugin(PluginError),
			Instantiate(String),
		}
		let (path, role, host_flags, ids) = (
			desc.path.clone(),
			desc.role,
			flags.clone(),
			param_ids.clone(),
		);
		let created = run_on_main(move |main| {
			let module = main.vst3.module(&path).map_err(Failure::Plugin)?;
			let result = Vst3Plugin::create(module, &cid, host_flags);
			let mut plugin = match result {
				Ok(p) => p,
				Err(e) => {
					main.vst3.release_unused(&path);
					return Err(Failure::Instantiate(e));
				}
			};
			let curves = plugin.curves(&ids, &stepped);
			let midi_map = plugin.midi_mapping();
			plugin.attachment = Some(attachment);
			let audio = SendPtr(plugin.processor.clone());
			let key = main.vst3.insert(plugin);
			Ok((key, audio, curves, midi_map))
		})?;
		let (key, audio, curves, midi_map) = created.map_err(|f| match f {
			Failure::Plugin(e) => e,
			Failure::Instantiate(reason) => inst_err(reason),
		})?;

		let count = desc.params.len();
		let mut processor = Self {
			key,
			id: desc.id.clone(),
			info: desc.device_info(),
			role,
			mode,
			param_ids,
			param_ranges: desc
				.params
				.iter()
				.map(|p| (p.min, p.max, p.stepped))
				.collect(),
			curves,
			queue: ParamQueue::new(count),
			mirror,
			gui_edits,
			gui_resend: Vec::with_capacity(GUI_EDIT_CAPACITY),
			editor,
			events: EventList::with_capacity(NOTE_EVENT_CAPACITY + 128),
			out_events: EventList::with_capacity(0),
			changes: ParameterChanges::with_capacity(count),
			out_changes: ParameterChanges::with_capacity(OUTPUT_PARAM_CAPACITY),
			held: HeldNotes::default(),
			release_held: false,
			midi_map,
			// SAFETY: plain C struct; all-zero is valid and fully set per block.
			context: unsafe { std::mem::zeroed() },
			audio: Some(audio),
			flags,
			layout: PortLayout::default(),
			sidechain: KeyInput::default(),
			last_frames: 0,
			buffers: Buffers::empty(),
			event_inputs: false,
			sample_rate,
			max_block,
			latency: 0,
			tail_seconds: default_tail(desc),
			default_tail: default_tail(desc),
			continuous_samples: 0,
			failed: false,
		};
		// On error the processor drops here, which tears the instance down.
		processor.activate(sample_rate, max_block)?;
		// State is restored once active: some plugins (e.g. LSP) derive
		// internal values from the sample rate while loading.
		if let Some(state) = state
			&& !processor.load_state(state)
		{
			return Err(PluginError::State {
				id: desc.id.clone(),
				reason: "the plugin rejected the data (component setState failed or the blob is not a Cadenza VST3 state)".into(),
			});
		}
		Ok(processor)
	}

	/// (Re-)activates the plugin for a new configuration. Not real-time.
	fn activate(&mut self, sample_rate: f32, max_block: usize) -> Result<(), PluginError> {
		let max_block = max_block.max(1);
		let (key, role, mode) = (self.key, self.role, self.mode);
		let result = run_on_main(move |main| {
			let plugin = main
				.vst3
				.instances
				.get_mut(&key)
				.ok_or_else(|| "instance no longer exists".to_string())?;
			plugin.activate(sample_rate, max_block, role, mode)
		})?;
		let activation = result.map_err(|reason| {
			self.failed = true;
			PluginError::Activate {
				id: self.id.clone(),
				sample_rate,
				max_block,
				reason,
			}
		})?;
		self.buffers = Buffers::new(&activation.layout, max_block);
		self.layout = activation.layout;
		self.sidechain.allocate(max_block);
		self.last_frames = 0;
		self.event_inputs = activation.event_inputs;
		self.latency = activation.latency;
		self.tail_seconds = match activation.tail_samples {
			n if n == kInfiniteTail => MAX_TAIL_SECONDS,
			// Many plugins report 0 even though notes release or effects ring:
			// keep the role-based estimate as a floor.
			n => (n as f32 / sample_rate)
				.min(MAX_TAIL_SECONDS)
				.max(self.default_tail),
		};
		self.sample_rate = sample_rate;
		self.max_block = max_block;
		self.continuous_samples = 0;
		self.failed = false;
		Vst3Flags::take(&self.flags.latency_changed);
		Vst3Flags::take(&self.flags.restart);
		Ok(())
	}

	fn bypass(&self, left: &mut [f32], right: &mut [f32]) {
		if self.role == PluginRole::Instrument {
			left.fill(0.0);
			right.fill(0.0);
		}
	}

	fn update_context(&mut self, t: &Transport) {
		use ProcessContext_::StatesAndFlags_ as S;
		let sr = f64::from(self.sample_rate);
		let c = &mut self.context;
		let mut state = S::kTempoValid
			| S::kProjectTimeMusicValid
			| S::kTimeSigValid
			| S::kContTimeValid
			| S::kBarPositionValid;
		if t.playing {
			state |= S::kPlaying;
		}
		c.state = state as u32;
		c.sampleRate = sr;
		c.tempo = t.bpm;
		c.projectTimeMusic = t.beat_position;
		c.barPositionMusic = t.bar_start;
		c.projectTimeSamples = if t.bpm > 0.0 {
			(t.beat_position * 60.0 / t.bpm * sr).round() as i64
		} else {
			0
		};
		c.continousTimeSamples = self.continuous_samples;
		c.timeSigNumerator = i32::from(t.time_sig_numerator.max(1));
		c.timeSigDenominator = i32::from(t.time_sig_denominator.max(1));
	}

	/// Calls `IAudioProcessor::process` with the prepared lists. `frames == 0`
	/// is a parameter flush (no audio buses), as the SDK allows.
	fn call_process(&mut self, frames: usize) -> bool {
		let Some(audio) = &self.audio else {
			return false;
		};
		let (inputs, outputs) = (self.buffers.in_buses.len(), self.buffers.out_buses.len());
		let mut data = ProcessData {
			processMode: match self.mode {
				ProcessMode::Realtime => ProcessModes_::kRealtime,
				ProcessMode::Offline => ProcessModes_::kOffline,
			} as i32,
			symbolicSampleSize: SymbolicSampleSizes_::kSample32 as i32,
			numSamples: frames as i32,
			numInputs: inputs as i32,
			numOutputs: outputs as i32,
			inputs: if inputs == 0 {
				std::ptr::null_mut()
			} else {
				self.buffers.in_buses.as_mut_ptr()
			},
			outputs: if outputs == 0 {
				std::ptr::null_mut()
			} else {
				self.buffers.out_buses.as_mut_ptr()
			},
			inputParameterChanges: iface::<_, IParameterChanges>(&self.changes),
			outputParameterChanges: iface::<_, IParameterChanges>(&self.out_changes),
			inputEvents: if self.event_inputs {
				iface::<_, IEventList>(&self.events)
			} else {
				std::ptr::null_mut()
			},
			outputEvents: iface::<_, IEventList>(&self.out_events),
			processContext: &mut self.context,
		};
		// SAFETY: every pointer in `data` refers to memory owned by `self`
		// that stays valid and untouched by the host for the whole call.
		let status = unsafe { audio.0.process(&mut data) };
		self.out_changes.clear();
		self.out_events.clear();
		// kResultFalse is used by some plugins for "nothing to do"; only
		// genuine error codes disable the plugin.
		status == kResultOk || status == kResultFalse
	}

	/// A handle to this instance's editor (usable while the processor runs on
	/// the audio thread).
	pub fn editor(&self) -> PluginEditor {
		PluginEditor::local(LocalEditor {
			format: PluginFormat::Vst3,
			key: self.key,
			shared: self.editor.clone(),
		})
	}

	/// Moves queued plain values into the input parameter changes using the
	/// audio-thread curves, then the GUI's edits (re-sent ones first, so a
	/// newer value always wins). `idle`: a zero-sample flush; GUI edits are
	/// kept to be sent again with the next block.
	fn queue_param_changes(&mut self, idle: bool) {
		for &(id, v) in &self.gui_resend {
			self.changes.push(id, v);
		}
		if !idle {
			self.gui_resend.clear();
		}
		let (ids, curves, changes, mirror) =
			(&self.param_ids, &self.curves, &self.changes, &self.mirror);
		self.queue.drain(|i, plain| {
			let normalized = curves.normalize(i, plain);
			changes.push(ids[i], normalized);
			mirror.set(i, normalized);
		});
		while let Ok((id, v)) = self.gui_edits.pop() {
			self.changes.push(id, v);
			if idle && self.gui_resend.len() < self.gui_resend.capacity() {
				self.gui_resend.push((id, v));
			}
		}
	}

	/// Delivers queued parameter changes with exact normalization (through
	/// the controller, on the main thread) in a zero-sample process call (the
	/// SDK's parameter flush), so that a following state save reflects them
	/// where the plugin supports it. Not real-time.
	fn flush_params(&mut self) {
		if self.queue.is_empty() || self.audio.is_none() || self.failed {
			return;
		}
		let mut pending = Vec::new();
		self.queue.drain(|i, v| pending.push((i, v)));
		let key = self.key;
		let ids: Vec<(u32, f64)> = pending
			.iter()
			.map(|&(i, v)| (self.param_ids[i], v))
			.collect();
		let exact = run_on_main(move |main| {
			let plugin = main.vst3.instances.get(&key)?;
			Some(
				ids.iter()
					.map(|&(id, v)| plugin.normalize(id, v))
					.collect::<Vec<_>>(),
			)
		})
		.ok()
		.flatten();
		self.changes.clear();
		for (k, &(i, plain)) in pending.iter().enumerate() {
			let normalized = exact
				.as_ref()
				.and_then(|e| e[k])
				.unwrap_or_else(|| self.curves.normalize(i, plain));
			self.changes.push(self.param_ids[i], normalized);
			self.mirror.set(i, normalized);
		}
		self.events.clear();
		self.call_process(0);
		self.changes.clear();
		// Not every plugin applies changes in a zero-sample call (LSP does
		// not); re-sending the same values with the next block is harmless.
		for (i, plain) in pending {
			self.queue.set(i, plain);
		}
	}
}

impl Processor for Vst3Processor {
	fn info(&self) -> &'static DeviceInfo {
		self.info
	}

	fn prepare(&mut self, sample_rate: f32, max_block: usize) {
		let restart = Vst3Flags::take(&self.flags.restart);
		if !self.failed
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

		self.changes.clear();
		self.events.clear();
		self.queue_param_changes(false);
		if self.release_held {
			self.held.release_all(&self.events, 0);
			self.release_held = false;
		}
		for e in events {
			if let Some((slot, value)) = controller_slot(e) {
				// Controllers drive the parameters the plugin maps to them (one value per
				// block: the latest).
				if let Some(Some(id)) = self.midi_map.get(slot) {
					self.changes.push(*id, value);
				}
			} else {
				self.held.push(&self.events, e, n as u32);
			}
		}

		// Inputs: the main bus gets the track's signal (effects), the
		// sidechain bus the key; every other channel is silent.
		let main = (self.role == PluginRole::Effect).then_some((&left[..n], &right[..n]));
		let key = self.sidechain.take();
		self.layout
			.fill_inputs(&mut self.buffers.inputs, n, main, key);
		for b in self
			.buffers
			.in_buses
			.iter_mut()
			.chain(self.buffers.out_buses.iter_mut())
		{
			b.silenceFlags = 0;
		}
		self.update_context(transport);

		let ok = self.call_process(n);
		self.continuous_samples += n as i64;
		if !ok {
			self.failed = true;
			self.bypass(left, right);
			return;
		}

		self.last_frames = n;
		match self
			.layout
			.main_output
			.and_then(|main| self.layout.output_pair(&self.buffers.outputs, main, n))
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
		self.layout.sidechain_input.is_some()
	}

	fn set_sidechain(&mut self, left: &[f32], right: &[f32]) {
		self.sidechain.set(left, right);
	}

	fn aux_output_count(&self) -> usize {
		self.layout.aux_output_ports().count()
	}

	fn aux_output(&self, index: usize) -> Option<(&[f32], &[f32])> {
		self.layout
			.aux_pair(&self.buffers.outputs, index, self.last_frames)
	}

	fn reset(&mut self) {
		// VST3 has no reset call; toggling processing is the SDK's way to
		// clear voices and delay lines, and is allowed on the audio thread.
		if let Some(audio) = &self.audio
			&& !self.failed
		{
			// SAFETY: the instance is active; exclusive access via `&mut self`.
			unsafe {
				audio.0.setProcessing(0);
				audio.0.setProcessing(1);
			}
		}
		self.release_held = self.held != HeldNotes::default();
	}

	fn idle(&mut self) {
		if self.failed || self.audio.is_none() {
			return;
		}
		if self.queue.is_empty() && self.gui_edits.is_empty() {
			return;
		}
		self.changes.clear();
		self.events.clear();
		self.queue_param_changes(true);
		self.call_process(0);
		self.changes.clear();
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
		run_on_main(move |main| main.vst3.instances.get(&key)?.save_state_synced())
			.ok()
			.flatten()
	}

	/// Factory programs (`IUnitInfo` program lists). Key: `vst3-program:<param>:<index>:
	/// <count>`; category: the list's name.
	fn presets(&mut self) -> Vec<cadenza_dsp::PresetInfo> {
		let key = self.key;
		let lists = run_on_main(move |main| {
			main.vst3
				.instances
				.get(&key)
				.map(|p| p.program_lists())
				.unwrap_or_default()
		})
		.unwrap_or_default();
		lists
			.into_iter()
			.flat_map(|list| {
				let count = list.programs.len();
				list.programs.into_iter().enumerate().map(move |(i, name)| {
					cadenza_dsp::PresetInfo {
						key: format!("{PROGRAM_KEY}{}:{i}:{count}", list.param),
						name,
						category: list.name.clone(),
					}
				})
			})
			.collect()
	}

	/// Selects a factory program: the program-change parameter goes to the controller
	/// (which updates its parameters) and, in a zero-sample process call, to the
	/// processor. Not real-time.
	fn load_preset(&mut self, preset: &str) -> bool {
		let Some((param, index, count)) = parse_program_key(preset) else {
			return false;
		};
		let value = if count > 1 {
			index as f64 / (count - 1) as f64
		} else {
			0.0
		};
		let key = self.key;
		let known = run_on_main(move |main| {
			let Some(plugin) = main.vst3.instances.get(&key) else {
				return false;
			};
			let ok = plugin
				.program_lists()
				.iter()
				.any(|l| l.param == param && l.programs.len() == count);
			if ok && let Some(c) = plugin.controller() {
				// SAFETY: plain controller call on the main thread.
				unsafe {
					vst3::Steinberg::Vst::IEditControllerTrait::setParamNormalized(c, param, value)
				};
			}
			ok
		})
		.unwrap_or(false);
		if !known || self.audio.is_none() || self.failed {
			return false;
		}
		self.flush_params();
		self.changes.clear();
		self.changes.push(param, value);
		self.events.clear();
		self.call_process(0);
		self.changes.clear();
		true
	}

	fn load_state(&mut self, data: &[u8]) -> bool {
		let key = self.key;
		let data = data.to_vec();
		run_on_main(move |main| {
			main.vst3
				.instances
				.get(&key)
				.is_some_and(|p| p.load_state(&data).is_ok())
		})
		.unwrap_or(false)
	}
}

impl Drop for Vst3Processor {
	fn drop(&mut self) {
		let key = self.key;
		let audio = self.audio.take();
		post_to_main(move |main| {
			// Release the audio-thread reference before the teardown; the
			// editor goes first.
			drop(audio);
			super::editor::close(main, key);
			main.vst3.remove(key);
		});
	}
}

/// Prefix of the preset keys of VST3 factory programs.
const PROGRAM_KEY: &str = "vst3-program:";

/// `(param, index, count)` of a program key.
fn parse_program_key(key: &str) -> Option<(u32, usize, usize)> {
	let mut parts = key.strip_prefix(PROGRAM_KEY)?.split(':');
	let param = parts.next()?.parse().ok()?;
	let index: usize = parts.next()?.parse().ok()?;
	let count: usize = parts.next()?.parse().ok()?;
	(index < count && parts.next().is_none()).then_some((param, index, count))
}

/// `IMidiMapping` slot and normalized value of a controller event (`None` for notes).
fn controller_slot(e: &MidiEvent) -> Option<(usize, f64)> {
	match *e {
		MidiEvent::ControlChange {
			controller, value, ..
		} => Some((
			usize::from(controller.min(127)),
			f64::from(value.clamp(0.0, 1.0)),
		)),
		MidiEvent::ChannelPressure { value, .. } => Some((128, f64::from(value.clamp(0.0, 1.0)))),
		MidiEvent::PitchBend { value, .. } => {
			Some((129, f64::from((value.clamp(-1.0, 1.0) + 1.0) * 0.5)))
		}
		_ => None,
	}
}
