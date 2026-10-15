//! CLAP hosting through `clack-host`.
//!
//! # Thread model
//!
//! CLAP splits plugin functions into `[main-thread]`, `[audio-thread]` and
//! `[thread-safe]`. The main thread must be one stable thread for the life of
//! an instance (many plugins, e.g. JUCE-based ones, bind their message loop to
//! the first thread that touches them). Cadenza satisfies this as follows:
//!
//! - **Main thread**: a dedicated, lazily spawned `cadenza-plugin-main` thread
//!   owns every `PluginInstance` (which is `!Send` in clack). Creation,
//!   activation/deactivation, state save/load, latency queries, destruction and
//!   `on_main_thread` callbacks all run there, submitted as jobs through
//!   [`crate::main_thread`] (shared with VST3 hosting). Callers block until the job finishes; none of this is
//!   real-time. The out-of-process scanner instead uses its own process main
//!   thread directly (see [`scan_file`]).
//! - **Audio thread**: the `Processor` returned by `instantiate` owns the
//!   plugin's audio processor (which clack makes `Send`) and is moved to
//!   whichever thread drives `process`. Exclusive `&mut self` access
//!   guarantees audio-thread calls (`start_processing`, `process`, `reset`,
//!   `stop_processing`, tail queries, `params.flush`) never run concurrently.
//!   When the engine prepares, resets or drops the processor on a non-audio
//!   thread, that thread *is* the audio thread for the duration of the call,
//!   which is what CLAP requires (the two roles are never active at once).
//! - **Host callbacks** only flip atomic flags (see `host.rs`), so a plugin can
//!   call them from any thread without blocking. Timer and posix-fd
//!   registrations (main thread) are served by the main thread's event loop.
//! - **Editors** (`gui` extension) live on the main thread too (see
//!   `editor.rs` and [`crate::editor`]); the parameter events a plugin outputs
//!   from `process`/`params.flush` (GUI edits) are pushed by the audio thread
//!   into a wait-free ring read by the editor handle.
//!
//! A plugin that crashes or panics across the FFI boundary takes the whole
//! process down; in-process hosting cannot prevent that. Scanning is always
//! out-of-process, and processing can be too (see [`crate::sandbox`]).

pub(crate) mod editor;
pub(crate) mod events;
pub(crate) mod host;
pub(crate) mod instance;
pub(crate) mod presets;

use std::collections::HashMap;
use std::ffi::CString;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use clack_extensions::audio_ports::{AudioPortFlags, AudioPortInfoBuffer, PluginAudioPorts};
use clack_extensions::latency::PluginLatency;
use clack_extensions::note_ports::{NoteDialects, NotePortInfoBuffer, PluginNotePorts};
use clack_extensions::params::{ParamInfoBuffer, ParamInfoFlags, PluginParams};
use clack_host::prelude::*;

use crate::descriptor::{PluginDescriptor, PluginFormat, PluginParam};
use crate::error::PluginError;
use host::{CadenzaHost, HostAudioProcessor, HostFlags, HostMainThread, HostShared, host_info};

pub(crate) use crate::ports::PortLayout;

pub use instance::ClapProcessor;

/// Sample rate and block size used to briefly activate plugins while scanning
/// (some only report latency once active).
const SCAN_SAMPLE_RATE: f64 = 48_000.0;
const SCAN_MAX_BLOCK: u32 = 512;

pub(crate) fn load_entry(path: &Path) -> Result<PluginEntry, PluginError> {
	// SAFETY: loading a CLAP binary runs its foreign initialisation code. That
	// is inherent to in-process hosting; untrusted files are first loaded by the
	// out-of-process scanner, which blocklists anything that crashes.
	unsafe { PluginEntry::load(path) }.map_err(|e| PluginError::Load {
		path: path.to_path_buf(),
		format: "CLAP",
		reason: e.to_string(),
	})
}

fn lossy(bytes: &[u8]) -> String {
	String::from_utf8_lossy(bytes).trim().to_string()
}

/// What the main-thread extensions report about an instance.
pub(crate) struct Capabilities {
	pub params: Vec<PluginParam>,
	pub ports: PortLayout,
	pub note_input: bool,
	/// The note port only understands raw MIDI.
	pub midi_dialect: bool,
	/// The note port accepts raw MIDI (controllers go as MIDI bytes).
	pub midi_controllers: bool,
}

pub(crate) fn query_ports(instance: &mut PluginInstance<CadenzaHost>) -> PortLayout {
	let handle = instance.plugin_handle();
	let mut layout = PortLayout::default();
	let Some(ports) = handle.get_extension::<PluginAudioPorts>() else {
		return layout;
	};
	let mut buf = AudioPortInfoBuffer::new();
	for is_input in [true, false] {
		let mut channels = Vec::new();
		let mut names = Vec::new();
		let mut main = None;
		for i in 0..ports.count(&handle, is_input).min(64) {
			let Some(info) = ports.get(&handle, i, is_input, &mut buf) else {
				channels.push(0);
				names.push(String::new());
				continue;
			};
			if info.flags.contains(AudioPortFlags::IS_MAIN) && main.is_none() {
				main = Some(i as usize);
			}
			channels.push(info.channel_count.min(64));
			names.push(lossy(info.name));
		}
		if main.is_none() && !channels.is_empty() {
			main = Some(0);
		}
		if is_input {
			layout.inputs = channels;
			layout.main_input = main;
		} else {
			layout.outputs = channels;
			layout.output_names = names;
			layout.main_output = main;
		}
	}
	layout.sidechain_input = layout.first_aux_input();
	layout
}

pub(crate) fn query_capabilities(instance: &mut PluginInstance<CadenzaHost>) -> Capabilities {
	let ports = query_ports(instance);
	let handle = instance.plugin_handle();

	let mut params = Vec::new();
	if let Some(ext) = handle.get_extension::<PluginParams>() {
		let mut buf = ParamInfoBuffer::new();
		for i in 0..ext.count(&handle) {
			let Some(info) = ext.get_info(&handle, i, &mut buf) else {
				continue;
			};
			if info
				.flags
				.intersects(ParamInfoFlags::IS_HIDDEN | ParamInfoFlags::IS_READONLY)
			{
				continue;
			}
			let (min, max) = if info.min_value <= info.max_value {
				(info.min_value, info.max_value)
			} else {
				(info.max_value, info.min_value)
			};
			params.push(PluginParam {
				id: info.id.get().to_string(),
				name: lossy(info.name),
				module: lossy(info.module),
				min,
				max,
				default: info.default_value.clamp(min, max),
				stepped: info.flags.contains(ParamInfoFlags::IS_STEPPED),
				automatable: info.flags.contains(ParamInfoFlags::IS_AUTOMATABLE),
				bypass: info.flags.contains(ParamInfoFlags::IS_BYPASS),
			});
		}
	}

	let mut note_input = false;
	let mut midi_dialect = false;
	let mut midi_controllers = false;
	if let Some(ext) = handle.get_extension::<PluginNotePorts>() {
		let mut buf = NotePortInfoBuffer::new();
		if ext.count(&handle, true) > 0 {
			note_input = true;
			if let Some(info) = ext.get(&handle, 0, true, &mut buf) {
				midi_dialect = !info.supported_dialects.contains(NoteDialects::CLAP)
					&& info.supported_dialects.contains(NoteDialects::MIDI);
				midi_controllers = info.supported_dialects.contains(NoteDialects::MIDI);
			}
		}
	}

	Capabilities {
		params,
		ports,
		note_input,
		midi_dialect,
		midi_controllers,
	}
}

/// Latency as reported by the latency extension. Only valid while (being)
/// activated, per the CLAP spec.
pub(crate) fn query_latency(instance: &mut PluginInstance<CadenzaHost>) -> u32 {
	let handle = instance.plugin_handle();
	handle
		.get_extension::<PluginLatency>()
		.map_or(0, |ext| ext.get(&handle))
}

pub(crate) fn new_instance(
	entry: &PluginEntry,
	id: &str,
	path: &Path,
	flags: Arc<HostFlags>,
) -> Result<PluginInstance<CadenzaHost>, PluginError> {
	let err = |reason: String| PluginError::Instantiate {
		id: id.to_string(),
		path: path.to_path_buf(),
		reason,
	};
	let cid = CString::new(id).map_err(|_| err("plugin id contains a NUL byte".into()))?;
	let plugin = id.to_string();
	PluginInstance::<CadenzaHost>::new(
		move |_| HostShared::new(flags, plugin),
		|shared| HostMainThread::new(shared),
		entry,
		&cid,
		&host_info(),
	)
	.map_err(|e| err(e.to_string()))
}

pub(crate) fn audio_config(sample_rate: f64, max_block: u32) -> PluginAudioConfiguration {
	PluginAudioConfiguration {
		sample_rate,
		min_frames_count: 1,
		max_frames_count: max_block.max(1),
	}
}

struct FactoryEntry {
	id: String,
	name: String,
	vendor: String,
	version: String,
	description: String,
	features: Vec<String>,
}

/// Loads a `.clap` file **in this process** and describes every plugin in its
/// factory, briefly instantiating and activating each one. This is what the
/// `cadenza-plugin-scanner` executable runs; hosts should call it only through
/// that subprocess (see `PluginCatalog::scan`) because a broken plugin can
/// crash or hang the calling process.
pub fn scan_file(path: &Path) -> Result<Vec<PluginDescriptor>, PluginError> {
	let entry = load_entry(path)?;
	let factory = entry
		.get_plugin_factory()
		.ok_or_else(|| PluginError::Load {
			path: path.to_path_buf(),
			format: "CLAP",
			reason: "the entry exposes no plugin factory".into(),
		})?;
	let text = |s: Option<&std::ffi::CStr>| s.map(|s| lossy(s.to_bytes())).unwrap_or_default();
	let listed: Vec<FactoryEntry> = factory
		.plugin_descriptors()
		.filter_map(|d| {
			let id = text(d.id());
			(!id.is_empty()).then(|| FactoryEntry {
				name: text(d.name()),
				vendor: text(d.vendor()),
				version: text(d.version()),
				description: text(d.description()),
				features: d.features().map(|f| lossy(f.to_bytes())).collect(),
				id,
			})
		})
		.collect();

	let mut out = Vec::new();
	let mut first_error = None;
	for meta in &listed {
		match describe(&entry, meta, path) {
			Ok(d) => out.push(d),
			Err(e) => {
				eprintln!("cadenza-plugin-scanner: skipping '{}': {e}", meta.id);
				first_error.get_or_insert(e);
			}
		}
	}
	match first_error {
		Some(e) if out.is_empty() => Err(e),
		_ => Ok(out),
	}
}

fn describe(
	entry: &PluginEntry,
	meta: &FactoryEntry,
	path: &Path,
) -> Result<PluginDescriptor, PluginError> {
	let mut instance = new_instance(entry, &meta.id, path, Arc::default())?;
	let caps = query_capabilities(&mut instance);
	let latency = match instance.activate(
		|shared, _| HostAudioProcessor::new(shared),
		audio_config(SCAN_SAMPLE_RATE, SCAN_MAX_BLOCK),
	) {
		Ok(processor) => {
			let latency = query_latency(&mut instance);
			instance.deactivate(processor);
			latency
		}
		Err(e) => {
			eprintln!(
				"cadenza-plugin-scanner: '{}' failed to activate ({e}); latency unknown",
				meta.id
			);
			0
		}
	};
	drop(instance);

	let audio_inputs = caps.ports.main_input_channels();
	let mut d = PluginDescriptor {
		format: PluginFormat::Clap,
		id: meta.id.clone(),
		name: if meta.name.is_empty() {
			meta.id.clone()
		} else {
			meta.name.clone()
		},
		vendor: meta.vendor.clone(),
		version: meta.version.clone(),
		path: path.to_path_buf(),
		role: PluginDescriptor::role_from(&meta.features, caps.note_input, audio_inputs),
		features: meta.features.clone(),
		description: String::new(),
		params: caps.params,
		audio_inputs,
		audio_outputs: caps.ports.main_output_channels(),
		note_input: caps.note_input,
		latency_samples: latency,
		sidechain_channels: caps.ports.sidechain_channels(),
		aux_outputs: caps.ports.aux_outputs(),
	};
	d.description = d.compose_description(&meta.description);
	Ok(d)
}

// ------------------------------------------------------------- main thread

/// A plugin instance owned by the main thread, plus its callback flags.
pub(crate) struct Hosted {
	pub instance: PluginInstance<CadenzaHost>,
	pub flags: Arc<HostFlags>,
	/// The open editor, if any.
	pub editor: Option<editor::ClapEditor>,
	/// Editor notifications shared with the instance's `PluginEditor` handles.
	pub shared: Option<Arc<crate::editor::EditorShared>>,
}

/// CLAP state owned by the plugin main thread (see [`crate::main_thread`]).
#[derive(Default)]
pub(crate) struct ClapState {
	pub instances: HashMap<u64, Hosted>,
	next_key: u64,
	/// Loaded binaries stay resident for the process lifetime: unloading code a
	/// plugin may still reference (threads, atexit hooks) is a common crash.
	entries: HashMap<PathBuf, PluginEntry>,
}

impl ClapState {
	pub fn entry(&mut self, path: &Path) -> Result<PluginEntry, PluginError> {
		if let Some(e) = self.entries.get(path) {
			return Ok(e.clone());
		}
		let e = load_entry(path)?;
		self.entries.insert(path.to_path_buf(), e.clone());
		Ok(e)
	}

	pub fn insert(&mut self, hosted: Hosted) -> u64 {
		self.next_key += 1;
		self.instances.insert(self.next_key, hosted);
		self.next_key
	}

	pub(crate) fn service_callbacks(&mut self) {
		for h in self.instances.values_mut() {
			if HostFlags::take(&h.flags.callback) {
				h.instance.call_on_main_thread_callback();
			}
		}
	}
}

/// Runs `f` with the CLAP state on the plugin main thread and waits for it.
pub(crate) fn run_on_main<R: Send + 'static>(
	f: impl FnOnce(&mut ClapState) -> R + Send + 'static,
) -> Result<R, PluginError> {
	crate::main_thread::run_on_main(move |state| f(&mut state.clap))
}
