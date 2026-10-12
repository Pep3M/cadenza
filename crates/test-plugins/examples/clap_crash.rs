//! A deliberately misbehaving CLAP effect for the sandbox tests
//! (`dev.cadenza.test.crash`). Built as a separate `cdylib` example so the
//! regular test bundle keeps exactly two plugins.
//!
//! Params (plain values):
//! - 0 `crash_now` (0..1): above 0.5, `process` aborts the whole process;
//! - 1 `hang_ms` (0..5000): `process` sleeps this long every block;
//! - 2 `gain` (0..1, default 0.5): linear output gain, so processed audio is
//!   distinguishable from a host-side pass-through bypass.
//!
//! The state extension stores `gain` only.

use std::ffi::CStr;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicU32, Ordering};

use clack_extensions::audio_ports::{
	AudioPortFlags, AudioPortInfo, AudioPortInfoWriter, AudioPortType, PluginAudioPorts,
	PluginAudioPortsImpl,
};
use clack_extensions::params::{
	ParamDisplayWriter, ParamInfo, ParamInfoFlags, ParamInfoWriter, PluginAudioProcessorParams,
	PluginMainThreadParams, PluginParams,
};
use clack_extensions::state::{PluginState, PluginStateImpl};
use clack_plugin::entry::SinglePluginEntry;
use clack_plugin::events::spaces::CoreEventSpace;
use clack_plugin::prelude::*;
use clack_plugin::stream::{InputStream, OutputStream};
use clack_plugin::utils::Cookie;

const CRASH: u32 = 0;
const HANG: u32 = 1;
const GAIN: u32 = 2;

struct Spec {
	name: &'static str,
	min: f32,
	max: f32,
	default: f32,
}

const SPECS: [Spec; 3] = [
	Spec {
		name: "crash_now",
		min: 0.0,
		max: 1.0,
		default: 0.0,
	},
	Spec {
		name: "hang_ms",
		min: 0.0,
		max: 5000.0,
		default: 0.0,
	},
	Spec {
		name: "gain",
		min: 0.0,
		max: 1.0,
		default: 0.5,
	},
];

pub struct Shared {
	values: [AtomicU32; 3],
}

impl PluginShared<'_> for Shared {}

impl Shared {
	fn get(&self, id: u32) -> f32 {
		f32::from_bits(self.values[id as usize].load(Ordering::Relaxed))
	}

	fn set(&self, id: u32, v: f64) {
		if let Some(spec) = SPECS.get(id as usize) {
			let v = v as f32;
			if !v.is_nan() {
				self.values[id as usize]
					.store(v.clamp(spec.min, spec.max).to_bits(), Ordering::Relaxed);
			}
		}
	}

	fn apply(&self, events: &InputEvents) {
		for e in events {
			if let Some(CoreEventSpace::ParamValue(p)) = e.as_core_event()
				&& let Some(id) = p.param_id()
			{
				self.set(id.get(), p.value());
			}
		}
	}
}

pub struct MainThread<'a> {
	shared: &'a Shared,
}

impl<'a> PluginMainThread<'a, Shared> for MainThread<'a> {}

impl PluginMainThreadParams for MainThread<'_> {
	fn count(&self) -> u32 {
		SPECS.len() as u32
	}

	fn get_info(&self, index: u32, info: &mut ParamInfoWriter) {
		let Some(s) = SPECS.get(index as usize) else {
			return;
		};
		info.set(&ParamInfo {
			id: ClapId::new(index),
			flags: ParamInfoFlags::IS_AUTOMATABLE,
			cookie: Cookie::empty(),
			name: s.name.as_bytes(),
			module: b"",
			min_value: s.min as f64,
			max_value: s.max as f64,
			default_value: s.default as f64,
		});
	}

	fn get_value(&self, id: ClapId) -> Option<f64> {
		((id.get() as usize) < SPECS.len()).then(|| self.shared.get(id.get()) as f64)
	}

	fn value_to_text(
		&self,
		_id: ClapId,
		value: f64,
		writer: &mut ParamDisplayWriter,
	) -> std::fmt::Result {
		use std::fmt::Write as _;
		write!(writer, "{value:.2}")
	}

	fn text_to_value(&self, _id: ClapId, text: &CStr) -> Option<f64> {
		text.to_str().ok()?.trim().parse().ok()
	}

	fn flush(&self, input: &InputEvents, _output: &mut OutputEvents) {
		self.shared.apply(input);
	}
}

impl PluginStateImpl for MainThread<'_> {
	fn save(&self, output: &mut OutputStream) -> Result<(), PluginError> {
		output.write_all(b"CRSH")?;
		output.write_all(&self.shared.get(GAIN).to_le_bytes())?;
		Ok(())
	}

	fn load(&self, input: &mut InputStream) -> Result<(), PluginError> {
		let mut buf = [0u8; 8];
		input.read_exact(&mut buf)?;
		if &buf[..4] != b"CRSH" {
			return Err(PluginError::Message("not a crash test plugin state"));
		}
		let gain = f32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]);
		self.shared.set(GAIN, gain as f64);
		Ok(())
	}
}

impl PluginAudioPortsImpl for MainThread<'_> {
	fn count(&self, _is_input: bool) -> u32 {
		1
	}

	fn get(&self, index: u32, is_input: bool, writer: &mut AudioPortInfoWriter) {
		if index == 0 {
			writer.set(&AudioPortInfo {
				id: ClapId::new(0),
				name: if is_input { b"Input" } else { b"Output" },
				channel_count: 2,
				flags: AudioPortFlags::IS_MAIN,
				port_type: Some(AudioPortType::STEREO),
				in_place_pair: Some(ClapId::new(0)),
			});
		}
	}
}

pub struct CrashPlugin;

impl Plugin for CrashPlugin {
	type AudioProcessor<'a> = Processor<'a>;
	type Shared<'a> = Shared;
	type MainThread<'a> = MainThread<'a>;

	fn declare_extensions(builder: &mut PluginExtensions<Self>, _shared: Option<&Shared>) {
		builder
			.register::<PluginAudioPorts>()
			.register::<PluginParams>()
			.register::<PluginState>();
	}
}

impl DefaultPluginFactory for CrashPlugin {
	fn get_descriptor() -> PluginDescriptor {
		use clack_plugin::plugin::features::*;
		PluginDescriptor::new("dev.cadenza.test.crash", "Cadenza Test Crash")
			.with_vendor("Cadenza")
			.with_version("0.1.0")
			.with_description("Effect that crashes or hangs on demand (sandbox test plugin).")
			.with_features([AUDIO_EFFECT, UTILITY, STEREO])
	}

	fn new_shared(_host: HostSharedHandle<'_>) -> Result<Shared, PluginError> {
		Ok(Shared {
			values: SPECS.map(|s| AtomicU32::new(s.default.to_bits())),
		})
	}

	fn new_main_thread<'a>(
		_host: HostMainThreadHandle<'a>,
		shared: &'a Shared,
	) -> Result<MainThread<'a>, PluginError> {
		Ok(MainThread { shared })
	}
}

pub struct Processor<'a> {
	shared: &'a Shared,
}

impl<'a> PluginAudioProcessor<'a, Shared, MainThread<'a>> for Processor<'a> {
	fn activate(
		_host: HostAudioProcessorHandle<'a>,
		_main_thread: &MainThread<'a>,
		shared: &'a Shared,
		_audio_config: PluginAudioConfiguration,
	) -> Result<Self, PluginError> {
		Ok(Self { shared })
	}

	fn process(
		&mut self,
		_process: Process,
		mut audio: Audio,
		events: Events,
	) -> Result<ProcessStatus, PluginError> {
		self.shared.apply(events.input);
		if self.shared.get(CRASH) > 0.5 {
			std::process::abort();
		}
		let hang = self.shared.get(HANG);
		if hang > 0.0 {
			std::thread::sleep(std::time::Duration::from_secs_f32(hang / 1000.0));
		}
		let gain = self.shared.get(GAIN);
		let frames = audio.frames_count() as usize;
		let Some(mut port) = audio.port_pair(0) else {
			return Ok(ProcessStatus::Continue);
		};
		let Some(mut channels) = port.channels()?.into_f32() else {
			return Ok(ProcessStatus::Continue);
		};
		for pair in channels.iter_mut() {
			match pair {
				ChannelPair::InPlace(buf) => buf.iter_mut().take(frames).for_each(|s| *s *= gain),
				ChannelPair::InputOutput(input, output) => {
					for (o, i) in output.iter_mut().zip(input.iter()).take(frames) {
						*o = i * gain;
					}
				}
				ChannelPair::OutputOnly(output) => output.iter_mut().for_each(|s| *s = 0.0),
				ChannelPair::InputOnly(_) => {}
			}
		}
		Ok(ProcessStatus::Continue)
	}
}

impl PluginAudioProcessorParams for Processor<'_> {
	fn flush(&mut self, input: &InputEvents, _output: &mut OutputEvents) {
		self.shared.apply(input);
	}
}

clack_export_entry!(SinglePluginEntry<CrashPlugin>);
