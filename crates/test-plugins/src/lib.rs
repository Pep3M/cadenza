//! Minimal CLAP plugins used to test `cadenza-plugin-host`.
//!
//! The library exports a single CLAP entry with two plugins:
//! - `dev.cadenza.test.gain`: stereo gain effect (`gain` dB, -60..12) with a real
//!   64-sample delay that it reports through the latency extension, so delay
//!   compensation can be verified end to end.
//! - `dev.cadenza.test.sine`: polyphonic sine instrument (`level` dB, -60..0)
//!   with a 5 ms attack and 20 ms release.
//!
//! Both implement the params and state extensions. They are deliberately tiny
//! and are never published.

use std::ffi::CStr;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicU32, Ordering};

use clack_extensions::audio_ports::{
	AudioPortFlags, AudioPortInfo, AudioPortInfoWriter, AudioPortType, PluginAudioPorts,
	PluginAudioPortsImpl,
};
use clack_extensions::latency::{PluginLatency, PluginLatencyImpl};
use clack_extensions::note_ports::{
	NoteDialect, NoteDialects, NotePortInfo, NotePortInfoWriter, PluginNotePorts,
	PluginNotePortsImpl,
};
use clack_extensions::params::{
	ParamDisplayWriter, ParamInfo, ParamInfoFlags, ParamInfoWriter, PluginAudioProcessorParams,
	PluginMainThreadParams, PluginParams,
};
use clack_extensions::state::{PluginState, PluginStateImpl};
use clack_plugin::entry::prelude::*;
use clack_plugin::events::spaces::CoreEventSpace;
use clack_plugin::prelude::*;
use clack_plugin::stream::{InputStream, OutputStream};
use clack_plugin::utils::Cookie;

/// Delay introduced (and reported) by the gain plugin.
pub const GAIN_LATENCY: u32 = 64;
const PARAM_ID: u32 = 0;

fn db_to_gain(db: f32) -> f32 {
	10f32.powf(db / 20.0)
}

/// A lock-free `f32` shared between the main thread and the audio processor.
struct AtomicF32(AtomicU32);

impl AtomicF32 {
	fn new(v: f32) -> Self {
		Self(AtomicU32::new(v.to_bits()))
	}
	fn get(&self) -> f32 {
		f32::from_bits(self.0.load(Ordering::Relaxed))
	}
	fn set(&self, v: f32) {
		self.0.store(v.to_bits(), Ordering::Relaxed)
	}
}

/// Single-parameter description shared by both plugins.
struct ParamSpec {
	name: &'static str,
	min: f32,
	max: f32,
	default: f32,
}

/// Plugin-wide state: the one parameter's plain value.
pub struct OneParamShared {
	spec: &'static ParamSpec,
	value: AtomicF32,
}

impl PluginShared<'_> for OneParamShared {}

impl OneParamShared {
	fn new(spec: &'static ParamSpec) -> Self {
		Self {
			spec,
			value: AtomicF32::new(spec.default),
		}
	}

	fn set(&self, v: f64) {
		let v = v as f32;
		if !v.is_nan() {
			self.value.set(v.clamp(self.spec.min, self.spec.max));
		}
	}

	fn apply_events(&self, events: &InputEvents) {
		for e in events {
			if let Some(CoreEventSpace::ParamValue(p)) = e.as_core_event()
				&& p.param_id().map(ClapId::get) == Some(PARAM_ID)
			{
				self.set(p.value());
			}
		}
	}
}

pub struct OneParamMainThread<'a> {
	shared: &'a OneParamShared,
}

impl<'a> PluginMainThread<'a, OneParamShared> for OneParamMainThread<'a> {}

impl PluginMainThreadParams for OneParamMainThread<'_> {
	fn count(&self) -> u32 {
		1
	}

	fn get_info(&self, param_index: u32, info: &mut ParamInfoWriter) {
		if param_index != 0 {
			return;
		}
		let s = self.shared.spec;
		info.set(&ParamInfo {
			id: ClapId::new(PARAM_ID),
			flags: ParamInfoFlags::IS_AUTOMATABLE,
			cookie: Cookie::empty(),
			name: s.name.as_bytes(),
			module: b"",
			min_value: s.min as f64,
			max_value: s.max as f64,
			default_value: s.default as f64,
		});
	}

	fn get_value(&self, param_id: ClapId) -> Option<f64> {
		(param_id.get() == PARAM_ID).then(|| self.shared.value.get() as f64)
	}

	fn value_to_text(
		&self,
		_param_id: ClapId,
		value: f64,
		writer: &mut ParamDisplayWriter,
	) -> std::fmt::Result {
		use std::fmt::Write as _;
		write!(writer, "{value:.1} dB")
	}

	fn text_to_value(&self, _param_id: ClapId, text: &CStr) -> Option<f64> {
		text.to_str()
			.ok()?
			.trim()
			.trim_end_matches("dB")
			.trim()
			.parse()
			.ok()
	}

	fn flush(&self, input: &InputEvents, _output: &mut OutputEvents) {
		self.shared.apply_events(input);
	}
}

const STATE_MAGIC: &[u8; 4] = b"CDZ1";

impl PluginStateImpl for OneParamMainThread<'_> {
	fn save(&self, output: &mut OutputStream) -> Result<(), PluginError> {
		output.write_all(STATE_MAGIC)?;
		output.write_all(&self.shared.value.get().to_le_bytes())?;
		Ok(())
	}

	fn load(&self, input: &mut InputStream) -> Result<(), PluginError> {
		let mut buf = [0u8; 8];
		input.read_exact(&mut buf)?;
		if &buf[..4] != STATE_MAGIC {
			return Err(PluginError::Message("not a cadenza test plugin state"));
		}
		let v = f32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]);
		self.shared.set(v as f64);
		Ok(())
	}
}

fn write_stereo_port(writer: &mut AudioPortInfoWriter, name: &[u8], in_place: bool) {
	writer.set(&AudioPortInfo {
		id: ClapId::new(0),
		name,
		channel_count: 2,
		flags: AudioPortFlags::IS_MAIN,
		port_type: Some(AudioPortType::STEREO),
		in_place_pair: in_place.then(|| ClapId::new(0)),
	});
}

// ---------------------------------------------------------------- gain

static GAIN_SPEC: ParamSpec = ParamSpec {
	name: "Gain",
	min: -60.0,
	max: 12.0,
	default: 0.0,
};

pub struct GainPlugin;

impl Plugin for GainPlugin {
	type AudioProcessor<'a> = GainProcessor<'a>;
	type Shared<'a> = OneParamShared;
	type MainThread<'a> = OneParamMainThread<'a>;

	fn declare_extensions(builder: &mut PluginExtensions<Self>, _shared: Option<&OneParamShared>) {
		builder
			.register::<PluginAudioPorts>()
			.register::<PluginParams>()
			.register::<PluginState>()
			.register::<PluginLatency>();
	}
}

impl DefaultPluginFactory for GainPlugin {
	fn get_descriptor() -> PluginDescriptor {
		use clack_plugin::plugin::features::*;
		PluginDescriptor::new("dev.cadenza.test.gain", "Cadenza Test Gain")
			.with_vendor("Cadenza")
			.with_version("0.1.0")
			.with_description("Stereo gain with a fixed 64-sample delay (test plugin).")
			.with_features([AUDIO_EFFECT, UTILITY, STEREO])
	}

	fn new_shared(_host: HostSharedHandle<'_>) -> Result<OneParamShared, PluginError> {
		Ok(OneParamShared::new(&GAIN_SPEC))
	}

	fn new_main_thread<'a>(
		_host: HostMainThreadHandle<'a>,
		shared: &'a OneParamShared,
	) -> Result<OneParamMainThread<'a>, PluginError> {
		Ok(OneParamMainThread { shared })
	}
}

impl PluginAudioPortsImpl for OneParamMainThread<'_> {
	fn count(&self, is_input: bool) -> u32 {
		// The sine instrument has no audio input.
		if is_input && std::ptr::eq(self.shared.spec, &SINE_SPEC) {
			0
		} else {
			1
		}
	}

	fn get(&self, index: u32, is_input: bool, writer: &mut AudioPortInfoWriter) {
		if index == 0 && PluginAudioPortsImpl::count(self, is_input) == 1 {
			let name: &[u8] = if is_input { b"Input" } else { b"Output" };
			write_stereo_port(writer, name, !std::ptr::eq(self.shared.spec, &SINE_SPEC));
		}
	}
}

impl PluginLatencyImpl for OneParamMainThread<'_> {
	fn get(&self) -> u32 {
		if std::ptr::eq(self.shared.spec, &GAIN_SPEC) {
			GAIN_LATENCY
		} else {
			0
		}
	}
}

pub struct GainProcessor<'a> {
	shared: &'a OneParamShared,
	delay: [[f32; GAIN_LATENCY as usize]; 2],
	pos: usize,
}

impl<'a> PluginAudioProcessor<'a, OneParamShared, OneParamMainThread<'a>> for GainProcessor<'a> {
	fn activate(
		_host: HostAudioProcessorHandle<'a>,
		_main_thread: &OneParamMainThread<'a>,
		shared: &'a OneParamShared,
		_audio_config: PluginAudioConfiguration,
	) -> Result<Self, PluginError> {
		Ok(Self {
			shared,
			delay: [[0.0; GAIN_LATENCY as usize]; 2],
			pos: 0,
		})
	}

	fn process(
		&mut self,
		_process: Process,
		mut audio: Audio,
		events: Events,
	) -> Result<ProcessStatus, PluginError> {
		self.shared.apply_events(events.input);
		let gain = db_to_gain(self.shared.value.get());
		let frames = audio.frames_count() as usize;
		let Some(mut port) = audio.port_pair(0) else {
			return Ok(ProcessStatus::Continue);
		};
		let Some(mut channels) = port.channels()?.into_f32() else {
			return Ok(ProcessStatus::Continue);
		};
		let start = self.pos;
		for (ch, pair) in channels.iter_mut().enumerate().take(2) {
			let line = &mut self.delay[ch];
			let mut pos = start;
			let mut step = |x: f32| {
				let y = line[pos];
				line[pos] = x * gain;
				pos = (pos + 1) % GAIN_LATENCY as usize;
				y
			};
			match pair {
				ChannelPair::InPlace(buf) => {
					for s in buf.iter_mut().take(frames) {
						*s = step(*s);
					}
				}
				ChannelPair::InputOutput(input, output) => {
					for (o, i) in output.iter_mut().zip(input.iter()).take(frames) {
						*o = step(*i);
					}
				}
				ChannelPair::OutputOnly(output) => {
					for o in output.iter_mut().take(frames) {
						*o = step(0.0);
					}
				}
				ChannelPair::InputOnly(_) => {}
			}
		}
		self.pos = (start + frames) % GAIN_LATENCY as usize;
		Ok(ProcessStatus::Continue)
	}

	fn reset(&mut self) {
		self.delay = [[0.0; GAIN_LATENCY as usize]; 2];
		self.pos = 0;
	}
}

impl PluginAudioProcessorParams for GainProcessor<'_> {
	fn flush(&mut self, input: &InputEvents, _output: &mut OutputEvents) {
		self.shared.apply_events(input);
	}
}

// ---------------------------------------------------------------- sine

static SINE_SPEC: ParamSpec = ParamSpec {
	name: "Level",
	min: -60.0,
	max: 0.0,
	default: -6.0,
};

pub struct SinePlugin;

impl Plugin for SinePlugin {
	type AudioProcessor<'a> = SineProcessor<'a>;
	type Shared<'a> = OneParamShared;
	type MainThread<'a> = OneParamMainThread<'a>;

	fn declare_extensions(builder: &mut PluginExtensions<Self>, _shared: Option<&OneParamShared>) {
		builder
			.register::<PluginAudioPorts>()
			.register::<PluginNotePorts>()
			.register::<PluginParams>()
			.register::<PluginState>();
	}
}

impl DefaultPluginFactory for SinePlugin {
	fn get_descriptor() -> PluginDescriptor {
		use clack_plugin::plugin::features::*;
		PluginDescriptor::new("dev.cadenza.test.sine", "Cadenza Test Sine")
			.with_vendor("Cadenza")
			.with_version("0.1.0")
			.with_description("Polyphonic sine synth with short attack/release (test plugin).")
			.with_features([INSTRUMENT, SYNTHESIZER, STEREO])
	}

	fn new_shared(_host: HostSharedHandle<'_>) -> Result<OneParamShared, PluginError> {
		Ok(OneParamShared::new(&SINE_SPEC))
	}

	fn new_main_thread<'a>(
		_host: HostMainThreadHandle<'a>,
		shared: &'a OneParamShared,
	) -> Result<OneParamMainThread<'a>, PluginError> {
		Ok(OneParamMainThread { shared })
	}
}

impl PluginNotePortsImpl for OneParamMainThread<'_> {
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

const VOICES: usize = 16;
const ATTACK_SECONDS: f32 = 0.005;
const RELEASE_SECONDS: f32 = 0.02;

#[derive(Clone, Copy)]
struct Voice {
	key: i16,
	phase: f32,
	inc: f32,
	env: f32,
	velocity: f32,
	releasing: bool,
}

impl Voice {
	const IDLE: Voice = Voice {
		key: -1,
		phase: 0.0,
		inc: 0.0,
		env: 0.0,
		velocity: 0.0,
		releasing: false,
	};

	fn active(&self) -> bool {
		self.key >= 0
	}
}

pub struct SineProcessor<'a> {
	shared: &'a OneParamShared,
	sample_rate: f32,
	voices: [Voice; VOICES],
}

impl SineProcessor<'_> {
	fn note_on(&mut self, key: i16, velocity: f32) {
		let slot = self
			.voices
			.iter()
			.position(|v| !v.active())
			.unwrap_or_else(|| {
				// Steal the quietest voice.
				let mut best = 0;
				for (i, v) in self.voices.iter().enumerate() {
					if v.env < self.voices[best].env {
						best = i;
					}
				}
				best
			});
		let freq = 440.0 * 2f32.powf((key as f32 - 69.0) / 12.0);
		self.voices[slot] = Voice {
			key,
			phase: 0.0,
			inc: freq / self.sample_rate,
			env: 0.0,
			velocity,
			releasing: false,
		};
	}

	fn note_off(&mut self, key: Option<i16>) {
		for v in self.voices.iter_mut().filter(|v| v.active()) {
			if key.is_none_or(|k| k == v.key) {
				v.releasing = true;
			}
		}
	}

	fn render(&mut self, level: f32) -> f32 {
		let attack = 1.0 / (ATTACK_SECONDS * self.sample_rate);
		let release = 1.0 / (RELEASE_SECONDS * self.sample_rate);
		let mut out = 0.0;
		for v in self.voices.iter_mut().filter(|v| v.active()) {
			if v.releasing {
				v.env -= release;
				if v.env <= 0.0 {
					*v = Voice::IDLE;
					continue;
				}
			} else if v.env < 1.0 {
				v.env = (v.env + attack).min(1.0);
			}
			out += (v.phase * std::f32::consts::TAU).sin() * v.env * v.velocity;
			v.phase += v.inc;
			if v.phase >= 1.0 {
				v.phase -= 1.0;
			}
		}
		out * level
	}
}

impl<'a> PluginAudioProcessor<'a, OneParamShared, OneParamMainThread<'a>> for SineProcessor<'a> {
	fn activate(
		_host: HostAudioProcessorHandle<'a>,
		_main_thread: &OneParamMainThread<'a>,
		shared: &'a OneParamShared,
		audio_config: PluginAudioConfiguration,
	) -> Result<Self, PluginError> {
		Ok(Self {
			shared,
			sample_rate: audio_config.sample_rate as f32,
			voices: [Voice::IDLE; VOICES],
		})
	}

	fn process(
		&mut self,
		_process: Process,
		mut audio: Audio,
		events: Events,
	) -> Result<ProcessStatus, PluginError> {
		let frames = audio.frames_count();
		let Some(mut port) = audio.output_port(0) else {
			return Ok(ProcessStatus::Continue);
		};
		let Some(mut channels) = port.channels()?.into_f32() else {
			return Ok(ProcessStatus::Continue);
		};
		let mut next = events.input.iter().peekable();
		for i in 0..frames {
			while let Some(e) = next.next_if(|e| e.header().time() <= i) {
				match e.as_core_event() {
					Some(CoreEventSpace::NoteOn(n)) => {
						if let Some(&k) = n.key().as_specific() {
							self.note_on(k as i16, n.velocity() as f32);
						}
					}
					Some(CoreEventSpace::NoteOff(n)) => {
						self.note_off(n.key().as_specific().map(|&k| k as i16));
					}
					Some(CoreEventSpace::NoteChoke(_)) => self.voices = [Voice::IDLE; VOICES],
					Some(CoreEventSpace::ParamValue(p))
						if p.param_id().map(ClapId::get) == Some(PARAM_ID) =>
					{
						self.shared.set(p.value());
					}
					_ => {}
				}
			}
			let level = db_to_gain(self.shared.value.get());
			let s = self.render(level);
			for c in 0..channels.channel_count() {
				if let Some(buf) = channels.channel_mut(c) {
					buf[i as usize] = s;
				}
			}
		}
		Ok(ProcessStatus::Continue)
	}

	fn reset(&mut self) {
		self.voices = [Voice::IDLE; VOICES];
	}
}

impl PluginAudioProcessorParams for SineProcessor<'_> {
	fn flush(&mut self, input: &InputEvents, _output: &mut OutputEvents) {
		self.shared.apply_events(input);
	}
}

// ---------------------------------------------------------------- entry

pub struct TestEntry {
	factory: PluginFactoryWrapper<TestFactory>,
}

pub struct TestFactory {
	gain: PluginDescriptor,
	sine: PluginDescriptor,
}

impl PluginFactoryImpl for TestFactory {
	fn plugin_count(&self) -> u32 {
		2
	}

	fn plugin_descriptor(&self, index: u32) -> Option<&PluginDescriptor> {
		match index {
			0 => Some(&self.gain),
			1 => Some(&self.sine),
			_ => None,
		}
	}

	fn create_plugin<'a>(
		&'a self,
		host_info: HostInfo<'a>,
		plugin_id: &CStr,
	) -> Option<PluginInstance<'a>> {
		if Some(plugin_id) == self.gain.id() {
			Some(PluginInstance::new::<GainPlugin>(
				host_info,
				&self.gain,
				GainPlugin::new_shared,
				GainPlugin::new_main_thread,
			))
		} else if Some(plugin_id) == self.sine.id() {
			Some(PluginInstance::new::<SinePlugin>(
				host_info,
				&self.sine,
				SinePlugin::new_shared,
				SinePlugin::new_main_thread,
			))
		} else {
			None
		}
	}
}

impl Entry for TestEntry {
	fn new(_bundle_path: Option<&CStr>) -> Result<Self, EntryLoadError> {
		Ok(Self {
			factory: PluginFactoryWrapper::new(TestFactory {
				gain: GainPlugin::get_descriptor(),
				sine: SinePlugin::get_descriptor(),
			}),
		})
	}

	fn declare_factories<'a>(&'a self, builder: &mut EntryFactories<'a>) {
		builder.register_factory(&self.factory);
	}
}

clack_export_entry!(TestEntry);
