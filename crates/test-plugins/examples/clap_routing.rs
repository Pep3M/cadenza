//! CLAP plugins exercising multi-port routing and factory presets, built as a
//! separate `cdylib` example (`cadenza-routing.clap`) so the regular test
//! bundle keeps exactly two plugins.
//!
//! - `dev.cadenza.test.ducker`: stereo effect with a second, non-main stereo
//!   input port ("Sidechain"). Output = main input + key input x `key_level`
//!   (dB, -60..12, default 0), so a test can see exactly where the key lands.
//!   Implements params, state, and preset loading for the two factory presets
//!   its preset-discovery provider declares: "Unity" (0 dB) and "Quiet Key"
//!   (-20 dB).
//! - `dev.cadenza.test.multi`: instrument with three output ports: main
//!   (stereo), "Kick" (stereo) and "Snare" (mono). While a note is held it
//!   writes a constant `0.5 x velocity` to one port: pitch 36 -> Kick, 38 ->
//!   Snare, anything else -> main.

use std::ffi::CStr;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicU32, Ordering};

use clack_extensions::audio_ports::{
	AudioPortFlags, AudioPortInfo, AudioPortInfoWriter, AudioPortType, PluginAudioPorts,
	PluginAudioPortsImpl,
};
use clack_extensions::note_ports::{
	NoteDialect, NoteDialects, NotePortInfo, NotePortInfoWriter, PluginNotePorts,
	PluginNotePortsImpl,
};
use clack_extensions::params::{
	ParamDisplayWriter, ParamInfo, ParamInfoFlags, ParamInfoWriter, PluginAudioProcessorParams,
	PluginMainThreadParams, PluginParams,
};
use clack_extensions::preset_discovery::prelude::*;
use clack_extensions::state::{PluginState, PluginStateImpl};
use clack_plugin::entry::prelude::*;
use clack_plugin::events::spaces::CoreEventSpace;
use clack_plugin::prelude::*;
use clack_plugin::stream::{InputStream, OutputStream};
use clack_plugin::utils::Cookie;

pub const DUCKER_ID: &str = "dev.cadenza.test.ducker";
pub const MULTI_ID: &str = "dev.cadenza.test.multi";

fn db_to_gain(db: f32) -> f32 {
	10f32.powf(db / 20.0)
}

// ---------------------------------------------------------------- ducker

const MIN_DB: f32 = -60.0;
const MAX_DB: f32 = 12.0;

pub struct DuckerShared {
	key_level: AtomicU32,
}

impl PluginShared<'_> for DuckerShared {}

impl DuckerShared {
	fn get(&self) -> f32 {
		f32::from_bits(self.key_level.load(Ordering::Relaxed))
	}

	fn set(&self, v: f64) {
		let v = v as f32;
		if !v.is_nan() {
			self.key_level
				.store(v.clamp(MIN_DB, MAX_DB).to_bits(), Ordering::Relaxed);
		}
	}

	fn apply(&self, events: &InputEvents) {
		for e in events {
			if let Some(CoreEventSpace::ParamValue(p)) = e.as_core_event()
				&& p.param_id().map(ClapId::get) == Some(0)
			{
				self.set(p.value());
			}
		}
	}
}

pub struct DuckerMain<'a> {
	shared: &'a DuckerShared,
}

impl<'a> PluginMainThread<'a, DuckerShared> for DuckerMain<'a> {}

impl PluginMainThreadParams for DuckerMain<'_> {
	fn count(&self) -> u32 {
		1
	}

	fn get_info(&self, index: u32, info: &mut ParamInfoWriter) {
		if index == 0 {
			info.set(&ParamInfo {
				id: ClapId::new(0),
				flags: ParamInfoFlags::IS_AUTOMATABLE,
				cookie: Cookie::empty(),
				name: b"Key Level",
				module: b"",
				min_value: MIN_DB as f64,
				max_value: MAX_DB as f64,
				default_value: 0.0,
			});
		}
	}

	fn get_value(&self, id: ClapId) -> Option<f64> {
		(id.get() == 0).then(|| self.shared.get() as f64)
	}

	fn value_to_text(
		&self,
		_id: ClapId,
		value: f64,
		writer: &mut ParamDisplayWriter,
	) -> std::fmt::Result {
		use std::fmt::Write as _;
		write!(writer, "{value:.1} dB")
	}

	fn text_to_value(&self, _id: ClapId, text: &CStr) -> Option<f64> {
		text.to_str()
			.ok()?
			.trim()
			.trim_end_matches("dB")
			.trim()
			.parse()
			.ok()
	}

	fn flush(&self, input: &InputEvents, _output: &mut OutputEvents) {
		self.shared.apply(input);
	}
}

impl PluginStateImpl for DuckerMain<'_> {
	fn save(&self, output: &mut OutputStream) -> Result<(), PluginError> {
		output.write_all(b"DUCK")?;
		output.write_all(&self.shared.get().to_le_bytes())?;
		Ok(())
	}

	fn load(&self, input: &mut InputStream) -> Result<(), PluginError> {
		let mut buf = [0u8; 8];
		input.read_exact(&mut buf)?;
		if &buf[..4] != b"DUCK" {
			return Err(PluginError::Message("not a ducker state"));
		}
		self.shared
			.set(f32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]) as f64);
		Ok(())
	}
}

/// Factory presets: (load key, name, key level dB).
const PRESETS: [(&CStr, &CStr, f32); 2] =
	[(c"unity", c"Unity", 0.0), (c"quiet", c"Quiet Key", -20.0)];

impl PluginPresetLoadImpl for DuckerMain<'_> {
	fn load_from_location(
		&self,
		location: Location,
		load_key: Option<&CStr>,
	) -> Result<(), PluginError> {
		if location != Location::Plugin {
			return Err(PluginError::Message("the ducker only has built-in presets"));
		}
		let (_, _, db) = PRESETS
			.iter()
			.find(|(key, _, _)| Some(*key) == load_key)
			.ok_or(PluginError::Message("unknown preset"))?;
		self.shared.set(*db as f64);
		Ok(())
	}
}

fn stereo_port(writer: &mut AudioPortInfoWriter, id: u32, name: &[u8], main: bool, channels: u32) {
	writer.set(&AudioPortInfo {
		id: ClapId::new(id),
		name,
		channel_count: channels,
		flags: if main {
			AudioPortFlags::IS_MAIN
		} else {
			AudioPortFlags::empty()
		},
		port_type: Some(if channels == 1 {
			AudioPortType::MONO
		} else {
			AudioPortType::STEREO
		}),
		in_place_pair: None,
	});
}

impl PluginAudioPortsImpl for DuckerMain<'_> {
	fn count(&self, is_input: bool) -> u32 {
		if is_input { 2 } else { 1 }
	}

	fn get(&self, index: u32, is_input: bool, writer: &mut AudioPortInfoWriter) {
		match (is_input, index) {
			(true, 0) => stereo_port(writer, 0, b"Input", true, 2),
			(true, 1) => stereo_port(writer, 1, b"Sidechain", false, 2),
			(false, 0) => stereo_port(writer, 0, b"Output", true, 2),
			_ => {}
		}
	}
}

pub struct DuckerPlugin;

impl Plugin for DuckerPlugin {
	type AudioProcessor<'a> = DuckerProcessor<'a>;
	type Shared<'a> = DuckerShared;
	type MainThread<'a> = DuckerMain<'a>;

	fn declare_extensions(builder: &mut PluginExtensions<Self>, _shared: Option<&DuckerShared>) {
		builder
			.register::<PluginAudioPorts>()
			.register::<PluginParams>()
			.register::<PluginState>()
			.register::<PluginPresetLoad>();
	}
}

impl DefaultPluginFactory for DuckerPlugin {
	fn get_descriptor() -> PluginDescriptor {
		use clack_plugin::plugin::features::*;
		PluginDescriptor::new(DUCKER_ID, "Cadenza Test Ducker")
			.with_vendor("Cadenza")
			.with_version("0.1.0")
			.with_description("Adds its sidechain input to its main input (test plugin).")
			.with_features([AUDIO_EFFECT, UTILITY, STEREO])
	}

	fn new_shared(_host: HostSharedHandle<'_>) -> Result<DuckerShared, PluginError> {
		Ok(DuckerShared {
			key_level: AtomicU32::new(0f32.to_bits()),
		})
	}

	fn new_main_thread<'a>(
		_host: HostMainThreadHandle<'a>,
		shared: &'a DuckerShared,
	) -> Result<DuckerMain<'a>, PluginError> {
		Ok(DuckerMain { shared })
	}
}

pub struct DuckerProcessor<'a> {
	shared: &'a DuckerShared,
	mix: [Vec<f32>; 2],
}

impl<'a> PluginAudioProcessor<'a, DuckerShared, DuckerMain<'a>> for DuckerProcessor<'a> {
	fn activate(
		_host: HostAudioProcessorHandle<'a>,
		_main_thread: &DuckerMain<'a>,
		shared: &'a DuckerShared,
		config: PluginAudioConfiguration,
	) -> Result<Self, PluginError> {
		let n = config.max_frames_count as usize;
		Ok(Self {
			shared,
			mix: [vec![0.0; n], vec![0.0; n]],
		})
	}

	fn process(
		&mut self,
		_process: Process,
		mut audio: Audio,
		events: Events,
	) -> Result<ProcessStatus, PluginError> {
		self.shared.apply(events.input);
		let gain = db_to_gain(self.shared.get());
		let frames = (audio.frames_count() as usize).min(self.mix[0].len());
		for (c, mix) in self.mix.iter_mut().enumerate() {
			mix[..frames].fill(0.0);
			for (port, g) in [(0, 1.0), (1, gain)] {
				let Some(input) = audio.input_port(port) else {
					continue;
				};
				let Some(channels) = input.channels()?.into_f32() else {
					continue;
				};
				if let Some(x) = channels.channel(c as u32) {
					for (m, x) in mix[..frames].iter_mut().zip(x) {
						*m += x * g;
					}
				}
			}
		}
		if let Some(mut out) = audio.output_port(0)
			&& let Some(mut channels) = out.channels()?.into_f32()
		{
			for (c, mix) in self.mix.iter().enumerate() {
				if let Some(buf) = channels.channel_mut(c as u32) {
					buf[..frames].copy_from_slice(&mix[..frames]);
				}
			}
		}
		Ok(ProcessStatus::Continue)
	}
}

impl PluginAudioProcessorParams for DuckerProcessor<'_> {
	fn flush(&mut self, input: &InputEvents, _output: &mut OutputEvents) {
		self.shared.apply(input);
	}
}

// ---------------------------------------------------------------- multi-output

pub struct MultiShared;

impl PluginShared<'_> for MultiShared {}

pub struct MultiMain;

impl<'a> PluginMainThread<'a, MultiShared> for MultiMain {}

impl PluginAudioPortsImpl for MultiMain {
	fn count(&self, is_input: bool) -> u32 {
		if is_input { 0 } else { 3 }
	}

	fn get(&self, index: u32, is_input: bool, writer: &mut AudioPortInfoWriter) {
		match (is_input, index) {
			(false, 0) => stereo_port(writer, 0, b"Main", true, 2),
			(false, 1) => stereo_port(writer, 1, b"Kick", false, 2),
			(false, 2) => stereo_port(writer, 2, b"Snare", false, 1),
			_ => {}
		}
	}
}

impl PluginNotePortsImpl for MultiMain {
	fn count(&self, is_input: bool) -> u32 {
		u32::from(is_input)
	}

	fn get(&self, index: u32, is_input: bool, writer: &mut NotePortInfoWriter) {
		if is_input && index == 0 {
			writer.set(&NotePortInfo {
				id: ClapId::new(0),
				name: b"Notes",
				supported_dialects: NoteDialects::CLAP,
				preferred_dialect: Some(NoteDialect::Clap),
			});
		}
	}
}

pub struct MultiPlugin;

impl Plugin for MultiPlugin {
	type AudioProcessor<'a> = MultiProcessor;
	type Shared<'a> = MultiShared;
	type MainThread<'a> = MultiMain;

	fn declare_extensions(builder: &mut PluginExtensions<Self>, _shared: Option<&MultiShared>) {
		builder
			.register::<PluginAudioPorts>()
			.register::<PluginNotePorts>();
	}
}

impl DefaultPluginFactory for MultiPlugin {
	fn get_descriptor() -> PluginDescriptor {
		use clack_plugin::plugin::features::*;
		PluginDescriptor::new(MULTI_ID, "Cadenza Test Multi-Out")
			.with_vendor("Cadenza")
			.with_version("0.1.0")
			.with_description("Drum-sampler-like instrument with three outputs (test plugin).")
			.with_features([INSTRUMENT, DRUM_MACHINE, STEREO])
	}

	fn new_shared(_host: HostSharedHandle<'_>) -> Result<MultiShared, PluginError> {
		Ok(MultiShared)
	}

	fn new_main_thread<'a>(
		_host: HostMainThreadHandle<'a>,
		_shared: &'a MultiShared,
	) -> Result<MultiMain, PluginError> {
		Ok(MultiMain)
	}
}

/// Output level of each port while its note is held (0 = silent).
pub struct MultiProcessor {
	levels: [f32; 3],
}

fn port_for(key: i16) -> usize {
	match key {
		36 => 1,
		38 => 2,
		_ => 0,
	}
}

impl<'a> PluginAudioProcessor<'a, MultiShared, MultiMain> for MultiProcessor {
	fn activate(
		_host: HostAudioProcessorHandle<'a>,
		_main_thread: &MultiMain,
		_shared: &'a MultiShared,
		_config: PluginAudioConfiguration,
	) -> Result<Self, PluginError> {
		Ok(Self { levels: [0.0; 3] })
	}

	fn process(
		&mut self,
		_process: Process,
		mut audio: Audio,
		events: Events,
	) -> Result<ProcessStatus, PluginError> {
		let frames = audio.frames_count();
		let mut next = events.input.iter().peekable();
		for i in 0..frames {
			while let Some(e) = next.next_if(|e| e.header().time() <= i) {
				match e.as_core_event() {
					Some(CoreEventSpace::NoteOn(n)) => {
						if let Some(&k) = n.key().as_specific() {
							self.levels[port_for(k as i16)] = 0.5 * n.velocity() as f32;
						}
					}
					Some(CoreEventSpace::NoteOff(n)) => match n.key().as_specific() {
						Some(&k) => self.levels[port_for(k as i16)] = 0.0,
						None => self.levels = [0.0; 3],
					},
					Some(CoreEventSpace::NoteChoke(_)) => self.levels = [0.0; 3],
					_ => {}
				}
			}
			for port in 0..3 {
				let level = self.levels[port];
				let Some(mut out) = audio.output_port(port) else {
					continue;
				};
				let Some(mut channels) = out.channels()?.into_f32() else {
					continue;
				};
				for c in 0..channels.channel_count() {
					if let Some(buf) = channels.channel_mut(c) {
						buf[i as usize] = level;
					}
				}
			}
		}
		Ok(ProcessStatus::Continue)
	}

	fn reset(&mut self) {
		self.levels = [0.0; 3];
	}
}

// ---------------------------------------------------------------- presets

pub struct DuckerPresets;

impl<'a> ProviderImpl<'a> for DuckerPresets {
	fn get_metadata(
		&mut self,
		location: Location,
		receiver: &mut MetadataReceiver,
	) -> Result<(), PluginError> {
		if location != Location::Plugin {
			return Ok(());
		}
		for (key, name, _) in PRESETS {
			receiver
				.begin_preset(Some(name), Some(key))
				.map_err(|_| PluginError::Message("begin_preset failed"))?
				.add_plugin_id(UniversalPluginId::clap(c"dev.cadenza.test.ducker"))
				.set_flags(Flags::IS_FACTORY_CONTENT);
		}
		Ok(())
	}
}

pub struct PresetFactory {
	descriptor: ProviderDescriptor,
}

impl PresetDiscoveryFactoryImpl for PresetFactory {
	fn provider_count(&self) -> u32 {
		1
	}

	fn provider_descriptor(&self, index: u32) -> Option<&ProviderDescriptor> {
		(index == 0).then_some(&self.descriptor)
	}

	fn create_provider<'a>(
		&'a self,
		indexer: IndexerInfo<'a>,
		provider_id: &CStr,
	) -> Option<ProviderInstance<'a>> {
		if Some(provider_id) != self.descriptor.id() {
			return None;
		}
		Some(ProviderInstance::new(
			indexer,
			&self.descriptor,
			|mut indexer: Indexer<'a>| {
				indexer
					.declare_location(LocationInfo {
						name: c"Factory",
						flags: Flags::IS_FACTORY_CONTENT,
						location: Location::Plugin,
					})
					.map_err(|_| PluginError::Message("declare_location failed"))?;
				Ok(DuckerPresets)
			},
		))
	}
}

// ---------------------------------------------------------------- entry

pub struct RoutingFactory {
	ducker: PluginDescriptor,
	multi: PluginDescriptor,
}

impl PluginFactoryImpl for RoutingFactory {
	fn plugin_count(&self) -> u32 {
		2
	}

	fn plugin_descriptor(&self, index: u32) -> Option<&PluginDescriptor> {
		match index {
			0 => Some(&self.ducker),
			1 => Some(&self.multi),
			_ => None,
		}
	}

	fn create_plugin<'a>(
		&'a self,
		host_info: HostInfo<'a>,
		plugin_id: &CStr,
	) -> Option<PluginInstance<'a>> {
		if Some(plugin_id) == self.ducker.id() {
			Some(PluginInstance::new::<DuckerPlugin>(
				host_info,
				&self.ducker,
				DuckerPlugin::new_shared,
				DuckerPlugin::new_main_thread,
			))
		} else if Some(plugin_id) == self.multi.id() {
			Some(PluginInstance::new::<MultiPlugin>(
				host_info,
				&self.multi,
				MultiPlugin::new_shared,
				MultiPlugin::new_main_thread,
			))
		} else {
			None
		}
	}
}

pub struct RoutingEntry {
	plugins: PluginFactoryWrapper<RoutingFactory>,
	presets: PresetDiscoveryFactoryWrapper<PresetFactory>,
}

impl Entry for RoutingEntry {
	fn new(_bundle_path: Option<&CStr>) -> Result<Self, EntryLoadError> {
		Ok(Self {
			plugins: PluginFactoryWrapper::new(RoutingFactory {
				ducker: DuckerPlugin::get_descriptor(),
				multi: MultiPlugin::get_descriptor(),
			}),
			presets: PresetDiscoveryFactoryWrapper::new(PresetFactory {
				descriptor: ProviderDescriptor::new(
					"dev.cadenza.test.presets",
					"Cadenza Test Presets",
				)
				.with_vendor("Cadenza"),
			}),
		})
	}

	fn declare_factories<'a>(&'a self, builder: &mut EntryFactories<'a>) {
		builder
			.register_factory(&self.plugins)
			.register_factory(&self.presets);
	}
}

clack_export_entry!(RoutingEntry);
