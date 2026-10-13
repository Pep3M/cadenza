//! A VST3 component + edit controller pair, driven from the plugin main
//! thread (or the scanner's thread): creation, bus setup, activation,
//! parameter metadata, state and teardown.

use std::rc::Rc;
use std::sync::Arc;

use vst3::Steinberg::Vst::{
	BusDirections_, BusInfo, BusTypes_, IAudioProcessor, IAudioProcessorTrait, IComponent,
	IComponentHandler, IComponentTrait, IConnectionPoint, IConnectionPointTrait, IEditController,
	IEditControllerTrait, IMidiMapping, IMidiMappingTrait, MediaTypes_, ParameterInfo,
	ParameterInfo_, ProcessModes_, ProcessSetup, SpeakerArr, SpeakerArrangement,
	SymbolicSampleSizes_,
};
use vst3::Steinberg::{FUnknown, IPluginBaseTrait, TUID};
use vst3::Steinberg::{kResultOk, kResultTrue};
use vst3::{ComPtr, ComWrapper};

use super::com::{ComponentHandler, MemoryStream, Vst3Flags, from_utf16};
use super::module::Module;
use super::rt::ParamCurves;
use super::state;
use crate::ProcessMode;
use crate::clap::PortLayout;
use crate::descriptor::{PluginParam, PluginRole};

/// Upper bounds that keep a misbehaving plugin from making us allocate
/// absurd buffers.
const MAX_BUSES: i32 = 16;
const MAX_CHANNELS: u32 = 64;

/// Result of (re-)activating an instance.
pub(crate) struct Activation {
	pub layout: PortLayout,
	pub latency: u32,
	pub tail_samples: u32,
	pub event_inputs: bool,
}

/// Bus summary readable before activation (for scanning and role detection).
pub(crate) struct BusSummary {
	pub main_input_channels: u32,
	pub main_output_channels: u32,
	pub event_inputs: bool,
}

/// A created and initialised VST3 plugin. Dropping it tears it down in the
/// order the SDK requires and then releases its module reference.
pub(crate) struct Vst3Plugin {
	component: ComPtr<IComponent>,
	pub processor: ComPtr<IAudioProcessor>,
	controller: Option<ComPtr<IEditController>>,
	/// The controller is a separate object (needs its own terminate).
	separate_controller: bool,
	/// Component and controller connection points, connected both ways.
	connection: Option<(ComPtr<IConnectionPoint>, ComPtr<IConnectionPoint>)>,
	handler: ComWrapper<ComponentHandler>,
	active: bool,
	processing: bool,
	/// The open editor, if any (torn down before the controller).
	pub(crate) editor: Option<super::editor::Vst3Editor>,
	/// Link to the processor's editor handles and parameter mirror.
	pub(crate) attachment: Option<super::editor::Attachment>,
	/// Declared last: the module must outlive every interface above.
	module: Rc<Module>,
}

fn arrangement_for(channels: u32) -> SpeakerArrangement {
	match channels {
		0 => 0,
		1 => SpeakerArr::kMono,
		2 => SpeakerArr::kStereo,
		n => (1u64 << n.min(63)) - 1,
	}
}

impl Vst3Plugin {
	/// Creates class `cid` from `module`, initialises component and
	/// controller, connects them and syncs the controller to the component.
	pub fn create(module: Rc<Module>, cid: &TUID, flags: Arc<Vst3Flags>) -> Result<Self, String> {
		let context = module
			.host
			.as_com_ref::<FUnknown>()
			.map_or(std::ptr::null_mut(), |r| r.as_ptr());
		let component = module
			.create::<IComponent>(cid)
			.ok_or("the factory could not create the audio component")?;
		// SAFETY: valid host context that outlives the instance (owned by the module).
		if unsafe { component.initialize(context) } != kResultOk {
			return Err("IComponent::initialize failed".into());
		}
		let processor = match component.cast::<IAudioProcessor>() {
			Some(p) => p,
			None => {
				// SAFETY: initialised above; terminate before releasing.
				unsafe { component.terminate() };
				return Err("the component does not implement IAudioProcessor".into());
			}
		};
		let handler = ComWrapper::new(ComponentHandler { flags });
		let mut plugin = Vst3Plugin {
			component,
			processor,
			controller: None,
			separate_controller: false,
			connection: None,
			handler,
			active: false,
			processing: false,
			editor: None,
			attachment: None,
			module,
		};
		// From here on `Drop` performs the teardown on any failure.
		// SAFETY: plain processor query.
		if unsafe {
			plugin
				.processor
				.canProcessSampleSize(SymbolicSampleSizes_::kSample32 as i32)
		} != kResultTrue
		{
			return Err("the plugin cannot process 32-bit float audio".into());
		}
		plugin.attach_controller(context);
		Ok(plugin)
	}

	fn attach_controller(&mut self, context: *mut FUnknown) {
		if let Some(c) = self.component.cast::<IEditController>() {
			self.controller = Some(c);
		} else {
			let mut cid: TUID = [0; 16];
			// SAFETY: valid out-parameter.
			if unsafe { self.component.getControllerClassId(&mut cid) } != kResultOk
				|| cid.iter().all(|&b| b == 0)
			{
				return;
			}
			let Some(c) = self.module.create::<IEditController>(&cid) else {
				return;
			};
			// SAFETY: same host context as the component.
			if unsafe { c.initialize(context) } != kResultOk {
				return;
			}
			self.controller = Some(c);
			self.separate_controller = true;
			if let (Some(a), Some(b)) = (
				self.component.cast::<IConnectionPoint>(),
				self.controller
					.as_ref()
					.and_then(|c| c.cast::<IConnectionPoint>()),
			) {
				// SAFETY: both objects are alive; each side add-refs the other as needed.
				unsafe {
					a.connect(b.as_ptr());
					b.connect(a.as_ptr());
				}
				self.connection = Some((a, b));
			}
		}
		let Some(controller) = &self.controller else {
			return;
		};
		let handler = self
			.handler
			.as_com_ref::<IComponentHandler>()
			.map_or(std::ptr::null_mut(), |r| r.as_ptr());
		// SAFETY: the handler outlives the controller (released after terminate).
		unsafe { controller.setComponentHandler(handler) };
		// Bring the controller in sync with the component's current state.
		let stream = MemoryStream::new();
		// SAFETY: valid stream objects for the duration of the calls.
		unsafe {
			if self.component.getState(MemoryStream::ptr(&stream)) == kResultOk {
				stream.rewind();
				controller.setComponentState(MemoryStream::ptr(&stream));
			}
		}
	}

	fn bus_count(&self, media: u32, dir: u32) -> i32 {
		// SAFETY: plain component query.
		unsafe { self.component.getBusCount(media as i32, dir as i32) }.clamp(0, MAX_BUSES)
	}

	fn bus_info(&self, media: u32, dir: u32, index: i32) -> Option<BusInfo> {
		// SAFETY: BusInfo is plain C data; valid out-parameter.
		unsafe {
			let mut info: BusInfo = std::mem::zeroed();
			(self
				.component
				.getBusInfo(media as i32, dir as i32, index, &mut info)
				== kResultOk)
				.then_some(info)
		}
	}

	/// Channel counts per audio bus in `dir`, plus the main bus index.
	fn audio_buses(&self, dir: u32) -> (Vec<u32>, Option<usize>) {
		let mut channels = Vec::new();
		let mut main = None;
		for i in 0..self.bus_count(MediaTypes_::kAudio as u32, dir) {
			let info = self.bus_info(MediaTypes_::kAudio as u32, dir, i);
			let count = info.map_or(0, |b| (b.channelCount.max(0) as u32).min(MAX_CHANNELS));
			if main.is_none() && info.is_some_and(|b| b.busType == BusTypes_::kMain as i32) {
				main = Some(i as usize);
			}
			channels.push(count);
		}
		if main.is_none() && !channels.is_empty() {
			main = Some(0);
		}
		(channels, main)
	}

	pub fn buses(&self) -> BusSummary {
		let (ins, main_in) = self.audio_buses(BusDirections_::kInput as u32);
		let (outs, main_out) = self.audio_buses(BusDirections_::kOutput as u32);
		BusSummary {
			main_input_channels: main_in.map_or(0, |i| ins[i]),
			main_output_channels: main_out.map_or(0, |i| outs[i]),
			event_inputs: self.bus_count(MediaTypes_::kEvent as u32, BusDirections_::kInput as u32)
				> 0,
		}
	}

	fn arrangement(&self, dir: u32, index: usize, fallback: u32) -> SpeakerArrangement {
		let mut arr: SpeakerArrangement = 0;
		// SAFETY: valid out-parameter.
		if unsafe {
			self.processor
				.getBusArrangement(dir as i32, index as i32, &mut arr)
		} == kResultOk
		{
			arr
		} else {
			arrangement_for(fallback)
		}
	}

	/// Stops processing and deactivates (no-op when inactive).
	pub fn deactivate(&mut self) {
		// SAFETY: state transitions in the order the SDK requires.
		unsafe {
			if self.processing {
				self.processor.setProcessing(0);
				self.processing = false;
			}
			if self.active {
				self.component.setActive(0);
				self.active = false;
			}
		}
	}

	/// (Re-)configures buses and processing, then activates. Effects get a
	/// stereo main input and output, instruments a stereo main output (their
	/// inputs keep the plugin's default and receive silence); if the plugin
	/// refuses, its default arrangement is used and mono buses are handled by
	/// the caller like the CLAP wrapper does.
	pub fn activate(
		&mut self,
		sample_rate: f32,
		max_block: usize,
		role: PluginRole,
		mode: ProcessMode,
	) -> Result<Activation, String> {
		self.deactivate();
		let (in_channels, main_in) = self.audio_buses(BusDirections_::kInput as u32);
		let (out_channels, main_out) = self.audio_buses(BusDirections_::kOutput as u32);
		let input = BusDirections_::kInput as u32;
		let output = BusDirections_::kOutput as u32;
		let mut ins: Vec<SpeakerArrangement> = in_channels
			.iter()
			.enumerate()
			.map(|(i, &c)| self.arrangement(input, i, c))
			.collect();
		let mut outs: Vec<SpeakerArrangement> = out_channels
			.iter()
			.enumerate()
			.map(|(i, &c)| self.arrangement(output, i, c))
			.collect();
		let (default_ins, default_outs) = (ins.clone(), outs.clone());
		if role == PluginRole::Effect
			&& let Some(i) = main_in
		{
			ins[i] = SpeakerArr::kStereo;
		}
		if let Some(o) = main_out {
			outs[o] = SpeakerArr::kStereo;
		}
		if ins != default_ins || outs != default_outs {
			// SAFETY: arrays sized to the bus counts we pass.
			let accepted = unsafe {
				self.processor.setBusArrangements(
					ins.as_mut_ptr(),
					ins.len() as i32,
					outs.as_mut_ptr(),
					outs.len() as i32,
				)
			} == kResultTrue;
			if !accepted {
				let (mut di, mut d_o) = (default_ins, default_outs);
				// SAFETY: as above.
				unsafe {
					self.processor.setBusArrangements(
						di.as_mut_ptr(),
						di.len() as i32,
						d_o.as_mut_ptr(),
						d_o.len() as i32,
					)
				};
			}
		}
		// Re-read: the plugin may have adapted the arrangement it accepted.
		let channels = |dir: u32, counts: &[u32]| -> Vec<u32> {
			counts
				.iter()
				.enumerate()
				.map(|(i, &c)| self.arrangement(dir, i, c).count_ones().min(MAX_CHANNELS))
				.collect()
		};
		let inputs = channels(input, &in_channels);
		let sidechain_input = (0..inputs.len()).find(|&i| {
			Some(i) != main_in
				&& inputs[i] > 0
				&& self
					.bus_info(MediaTypes_::kAudio as u32, input, i as i32)
					.is_some_and(|b| b.busType == BusTypes_::kAux as i32)
		});
		let output_names = (0..out_channels.len())
			.map(|o| {
				self.bus_info(MediaTypes_::kAudio as u32, output, o as i32)
					.map(|b| from_utf16(&b.name))
					.unwrap_or_default()
			})
			.collect();
		let layout = PortLayout {
			inputs,
			outputs: channels(output, &out_channels),
			main_input: main_in,
			main_output: main_out,
			sidechain_input,
			output_names,
		};

		let mut setup = ProcessSetup {
			processMode: match mode {
				ProcessMode::Realtime => ProcessModes_::kRealtime,
				ProcessMode::Offline => ProcessModes_::kOffline,
			} as i32,
			symbolicSampleSize: SymbolicSampleSizes_::kSample32 as i32,
			maxSamplesPerBlock: i32::try_from(max_block.max(1)).unwrap_or(i32::MAX),
			sampleRate: f64::from(sample_rate),
		};
		// SAFETY: valid setup struct; the plugin is inactive.
		if unsafe { self.processor.setupProcessing(&mut setup) } != kResultOk {
			return Err("IAudioProcessor::setupProcessing failed".into());
		}

		let audio = MediaTypes_::kAudio as i32;
		let event = MediaTypes_::kEvent as i32;
		let event_inputs = self.bus_count(event as u32, input) > 0;
		// SAFETY: bus indices come from the plugin's own counts.
		unsafe {
			for i in 0..layout.inputs.len() {
				let on = (role == PluginRole::Effect && Some(i) == main_in)
					|| Some(i) == layout.sidechain_input;
				self.component
					.activateBus(audio, input as i32, i as i32, u8::from(on));
			}
			// Every output bus with channels is active: extra outputs of
			// multi-output instruments may be routed to buses.
			for o in 0..layout.outputs.len() {
				let on = Some(o) == main_out || layout.outputs[o] > 0;
				self.component
					.activateBus(audio, output as i32, o as i32, u8::from(on));
			}
			if event_inputs {
				self.component.activateBus(event, input as i32, 0, 1);
			}
			if self.component.setActive(1) != kResultOk {
				return Err("IComponent::setActive(true) failed".into());
			}
			self.active = true;
			// Some plugins return kNotImplemented here; that is not fatal.
			self.processor.setProcessing(1);
			self.processing = true;
			Ok(Activation {
				layout,
				latency: self.processor.getLatencySamples(),
				tail_samples: self.processor.getTailSamples(),
				event_inputs,
			})
		}
	}

	/// Parameters the AI may set: hidden and read-only ones are skipped
	/// (except the bypass switch, which is exposed and marked). Ranges are
	/// plain values from `normalizedParamToPlain(0/1)`.
	pub fn params(&self) -> Vec<PluginParam> {
		let Some(c) = &self.controller else {
			return Vec::new();
		};
		use ParameterInfo_::ParameterFlags_ as F;
		let mut out = Vec::new();
		// SAFETY: plain controller queries with valid out-parameters.
		unsafe {
			for i in 0..c.getParameterCount().max(0) {
				let mut info: ParameterInfo = std::mem::zeroed();
				if c.getParameterInfo(i, &mut info) != kResultOk {
					continue;
				}
				let bypass = info.flags & F::kIsBypass != 0;
				if !bypass && info.flags & (F::kIsHidden | F::kIsReadOnly) != 0 {
					continue;
				}
				let plain = |n: f64| {
					let v = c.normalizedParamToPlain(info.id, n);
					if v.is_finite() { v } else { n }
				};
				let (a, b) = (plain(0.0), plain(1.0));
				let (min, max) = if a <= b { (a, b) } else { (b, a) };
				let mut name = from_utf16(&info.title);
				if name.is_empty() {
					name = from_utf16(&info.shortTitle);
				}
				out.push(PluginParam {
					id: info.id.to_string(),
					name,
					module: String::new(),
					min,
					max,
					default: plain(info.defaultNormalizedValue.clamp(0.0, 1.0)).clamp(min, max),
					stepped: info.stepCount > 0,
					automatable: info.flags & F::kCanAutomate != 0,
					bypass,
				});
			}
		}
		out
	}

	/// Samples the plain/normalized curve of each parameter (see [`ParamCurves`]).
	pub fn curves(&self, ids: &[u32], stepped: &[bool]) -> ParamCurves {
		ParamCurves::build(ids.len(), stepped, |i, n| match &self.controller {
			// SAFETY: plain controller query.
			Some(c) => unsafe { c.normalizedParamToPlain(ids[i], n) },
			None => n,
		})
	}

	/// The parameter each MIDI controller drives (`IMidiMapping`, bus 0 channel 0):
	/// index 0..=127 = CC, 128 = channel pressure, 129 = pitch bend. Empty when the
	/// plugin has no MIDI mapping.
	pub fn midi_mapping(&self) -> Vec<Option<u32>> {
		let Some(mapping) = self
			.controller
			.as_ref()
			.and_then(|c| c.cast::<IMidiMapping>())
		else {
			return Vec::new();
		};
		(0..130i16)
			.map(|cc| {
				let mut id: u32 = 0;
				// SAFETY: plain query with a valid out-pointer.
				let r = unsafe { mapping.getMidiControllerAssignment(0, 0, cc, &mut id) };
				(r == kResultOk || r == kResultTrue).then_some(id)
			})
			.collect()
	}

	/// Exact plain → normalized conversion through the controller.
	pub fn normalize(&self, id: u32, plain: f64) -> Option<f64> {
		let c = self.controller.as_ref()?;
		// SAFETY: plain controller query.
		let v = unsafe { c.plainParamToNormalized(id, plain) };
		v.is_finite().then(|| v.clamp(0.0, 1.0))
	}

	pub fn controller(&self) -> Option<&ComPtr<IEditController>> {
		self.controller.as_ref()
	}

	pub fn flags(&self) -> &Arc<Vst3Flags> {
		&self.handler.flags
	}

	/// Component + controller state as one framed blob.
	pub fn save_state(&self) -> Option<Vec<u8>> {
		let comp = MemoryStream::new();
		// SAFETY: valid stream for the duration of the call.
		if unsafe { self.component.getState(MemoryStream::ptr(&comp)) } != kResultOk {
			return None;
		}
		let ctrl = MemoryStream::new();
		let ctrl_bytes = match &self.controller {
			// SAFETY: as above.
			Some(c) if unsafe { c.getState(MemoryStream::ptr(&ctrl)) } == kResultOk => ctrl.take(),
			_ => Vec::new(),
		};
		Some(state::encode(&comp.take(), &ctrl_bytes))
	}

	/// Restores a blob from [`Vst3Plugin::save_state`]: component state, then
	/// the controller's view of it, then the controller's own state.
	pub fn load_state(&self, data: &[u8]) -> Result<(), String> {
		let (comp, ctrl) = state::decode(data)?;
		let stream = MemoryStream::with_data(comp.to_vec());
		// SAFETY: valid streams for the duration of each call.
		unsafe {
			if self.component.setState(MemoryStream::ptr(&stream)) != kResultOk {
				return Err("the component refused its state".into());
			}
			if let Some(c) = &self.controller {
				let stream = MemoryStream::with_data(comp.to_vec());
				c.setComponentState(MemoryStream::ptr(&stream));
				if !ctrl.is_empty() {
					let stream = MemoryStream::with_data(ctrl.to_vec());
					c.setState(MemoryStream::ptr(&stream));
				}
			}
		}
		Ok(())
	}

	pub fn module(&self) -> &Rc<Module> {
		&self.module
	}
}

impl Vst3Plugin {
	/// Deactivates and terminates (called from `Drop`, after the editor).
	pub(crate) fn teardown(&mut self) {
		self.deactivate();
		// SAFETY: teardown in SDK order; each object is terminated once.
		unsafe {
			if let Some((a, b)) = self.connection.take() {
				a.disconnect(b.as_ptr());
				b.disconnect(a.as_ptr());
			}
			if let Some(c) = self.controller.take()
				&& self.separate_controller
			{
				c.terminate();
			}
			self.component.terminate();
		}
		// Remaining interface pointers are released as fields drop, before
		// `module` (declared last) can unload the code.
	}
}
