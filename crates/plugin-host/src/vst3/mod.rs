//! VST3 hosting through the raw `vst3` COM bindings (coupler-rs/vst3-rs).
//!
//! # Thread model
//!
//! Mirrors the CLAP host (see [`crate::clap`]):
//!
//! - **Main thread**: every `IComponent`/`IEditController`/`IConnectionPoint`
//!   call (creation, `initialize`, bus setup, `setupProcessing`,
//!   `setActive`, parameter metadata, state, `terminate`) runs on the shared
//!   `cadenza-plugin-main` thread, which owns the [`plugin::Vst3Plugin`]s and
//!   their refcounted [`module::Module`]s. Callers block on
//!   [`crate::main_thread::run_on_main`]; nothing there is real-time. The
//!   out-of-process scanner uses its own process main thread instead.
//! - **Audio thread**: the `Processor` returned by `instantiate` holds its own
//!   reference to the plugin's `IAudioProcessor` and calls `process` (and
//!   `setProcessing(false/true)` for `reset`, which the SDK allows from the
//!   audio thread). `&mut self` guarantees these never overlap with each other
//!   or with main-thread jobs the processor itself issues (`prepare`, state).
//! - **Host callbacks** (`IComponentHandler`) only flip atomic flags that the
//!   next `prepare` consumes, so plugins may call them from any thread.
//! - Every host-implemented COM method catches panics at the FFI boundary.
//!
//! # Parameters
//!
//! Cadenza speaks plain values; VST3 automation is normalized `0..=1`.
//! `set_param` clamps the plain value to the descriptor range and queues it;
//! `process` converts it on the audio thread with a precomputed per-parameter
//! curve (see [`rt::ParamCurves`]) and sends one point per changed parameter
//! per block. `save_state` instead converts pending values exactly with the
//! controller's `plainParamToNormalized` on the main thread, flushes them with
//! a zero-sample `process`, and mirrors every value sent so far into the
//! controller (`setParamNormalized`) before saving. Output parameter changes
//! and output events from the plugin are accepted and ignored.
//!
//! No editor (`IPlugView`) yet: plugin GUIs are future work.

// VST3 enum constants are `c_uint` on Unix but `c_int` on Windows, so casts
// that are no-ops on one platform are required on the other.
#![allow(clippy::unnecessary_cast)]

pub(crate) mod com;
pub(crate) mod instance;
pub(crate) mod module;
pub(crate) mod moduleinfo;
pub(crate) mod plugin;
pub(crate) mod rt;
pub(crate) mod state;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use crate::ProcessMode;
use crate::descriptor::{PluginDescriptor, PluginFormat, PluginRole};
use crate::discovery::vst3_moduleinfo;
use crate::error::PluginError;
use com::tuid_to_hex;
use module::{AUDIO_MODULE_CLASS, ClassInfo, Module};
use moduleinfo::ModuleInfo;
use plugin::Vst3Plugin;

pub use instance::Vst3Processor;

/// Sample rate and block size used to briefly activate plugins while scanning.
const SCAN_SAMPLE_RATE: f32 = 48_000.0;
const SCAN_MAX_BLOCK: usize = 512;

/// VST3 state owned by the plugin main thread.
#[derive(Default)]
pub(crate) struct Vst3State {
	/// Loaded modules, shared by their instances; a module is unloaded
	/// (`ModuleExit`) when its last instance goes away.
	modules: HashMap<PathBuf, Rc<Module>>,
	pub instances: HashMap<u64, Vst3Plugin>,
	next_key: u64,
}

impl Vst3State {
	pub fn module(&mut self, path: &Path) -> Result<Rc<Module>, PluginError> {
		if let Some(m) = self.modules.get(path) {
			return Ok(m.clone());
		}
		let m = Rc::new(Module::load(path)?);
		self.modules.insert(path.to_path_buf(), m.clone());
		Ok(m)
	}

	pub fn insert(&mut self, plugin: Vst3Plugin) -> u64 {
		self.next_key += 1;
		self.instances.insert(self.next_key, plugin);
		self.next_key
	}

	/// Tears an instance down and unloads its module if it was the last user.
	pub fn remove(&mut self, key: u64) {
		let Some(plugin) = self.instances.remove(&key) else {
			return;
		};
		let path = plugin.module().path().to_path_buf();
		drop(plugin);
		self.release_unused(&path);
	}

	/// Drops the cached module at `path` if no instance uses it.
	pub fn release_unused(&mut self, path: &Path) {
		if self
			.modules
			.get(path)
			.is_some_and(|m| Rc::strong_count(m) == 1)
		{
			self.modules.remove(path);
		}
	}
}

/// Classifies a VST3 class from its sub-categories (e.g. "Instrument|Synth",
/// "Fx|Reverb"), falling back to its buses. Returns the role and Cadenza
/// feature tags (lowercased sub-categories plus "instrument"/"audio-effect").
pub(crate) fn classify(
	sub_categories: &str,
	event_inputs: bool,
	audio_inputs: u32,
) -> (PluginRole, Vec<String>) {
	let parts: Vec<String> = sub_categories
		.split('|')
		.map(|s| s.trim().to_ascii_lowercase())
		.filter(|s| !s.is_empty())
		.collect();
	let is = |p: &str| parts.iter().any(|x| x == p);
	let mut features = Vec::new();
	if is("instrument") || is("synth") {
		features.push("instrument".to_string());
	} else if is("fx") {
		features.push("audio-effect".to_string());
	}
	features.extend(
		parts
			.iter()
			.filter(|p| !matches!(p.as_str(), "fx" | "instrument"))
			.cloned(),
	);
	let role = PluginDescriptor::role_from(&features, event_inputs, audio_inputs);
	(role, features)
}

/// Loads a `.vst3` bundle **in this process** and describes every audio
/// module class in it, briefly instantiating and activating each one. This is
/// what the `cadenza-plugin-scanner` executable runs; hosts should only call
/// it through that subprocess (see `PluginCatalog::scan`).
///
/// `Contents/Resources/moduleinfo.json`, when present, fills in metadata the
/// factory leaves empty and adds classes the factory did not enumerate; every
/// class is still verified by creating it.
pub fn scan_file(path: &Path) -> Result<Vec<PluginDescriptor>, PluginError> {
	let module = Rc::new(Module::load(path)?);
	let info = vst3_moduleinfo(path).and_then(|p| ModuleInfo::load(&p));
	let mut classes: Vec<ClassInfo> = module
		.classes()
		.into_iter()
		.filter(|c| c.category == AUDIO_MODULE_CLASS)
		.collect();
	if let Some(info) = &info {
		for c in &mut classes {
			if let Some(entry) = info.class(&tuid_to_hex(&c.cid)) {
				let fill = |dst: &mut String, src: &str| {
					if dst.is_empty() {
						*dst = src.to_string();
					}
				};
				fill(&mut c.name, &entry.name);
				fill(&mut c.vendor, &entry.vendor);
				fill(&mut c.version, &entry.version);
				fill(&mut c.sub_categories, &entry.sub_categories.join("|"));
			}
		}
		for entry in info
			.classes
			.iter()
			.filter(|e| e.category == AUDIO_MODULE_CLASS)
		{
			let Some(cid) = com::tuid_from_hex(&entry.cid) else {
				continue;
			};
			if !classes
				.iter()
				.any(|c| tuid_to_hex(&c.cid) == tuid_to_hex(&cid))
			{
				classes.push(ClassInfo {
					cid,
					category: entry.category.clone(),
					name: entry.name.clone(),
					vendor: entry.vendor.clone(),
					version: entry.version.clone(),
					sub_categories: entry.sub_categories.join("|"),
				});
			}
		}
	}
	let module_version = info.as_ref().map(|i| i.version.clone()).unwrap_or_default();

	let mut out = Vec::new();
	let mut first_error = None;
	for class in &classes {
		match describe(&module, class, path, &module_version) {
			Ok(d) => out.push(d),
			Err(e) => {
				eprintln!("cadenza-plugin-scanner: skipping '{}': {e}", class.name);
				first_error.get_or_insert(e);
			}
		}
	}
	match first_error {
		Some(e) if out.is_empty() => Err(e),
		None if out.is_empty() => Err(PluginError::Load {
			path: path.to_path_buf(),
			format: "VST3",
			reason: "the module exports no audio module classes".into(),
		}),
		_ => Ok(out),
	}
}

fn describe(
	module: &Rc<Module>,
	class: &ClassInfo,
	path: &Path,
	module_version: &str,
) -> Result<PluginDescriptor, PluginError> {
	let id = tuid_to_hex(&class.cid);
	let err = |reason: String| PluginError::Instantiate {
		id: id.clone(),
		path: path.to_path_buf(),
		reason,
	};
	let mut plugin = Vst3Plugin::create(module.clone(), &class.cid, Arc::default()).map_err(err)?;
	let buses = plugin.buses();
	let (role, features) = classify(
		&class.sub_categories,
		buses.event_inputs,
		buses.main_input_channels,
	);
	let params = plugin.params();
	let (layout, latency) = match plugin.activate(
		SCAN_SAMPLE_RATE,
		SCAN_MAX_BLOCK,
		role,
		ProcessMode::Realtime,
	) {
		Ok(a) => (Some(a.layout), a.latency),
		Err(e) => {
			eprintln!(
				"cadenza-plugin-scanner: '{}' failed to activate ({e}); latency unknown",
				class.name
			);
			(None, 0)
		}
	};
	drop(plugin);

	let (audio_inputs, audio_outputs) = match &layout {
		Some(l) => (l.main_input_channels(), l.main_output_channels()),
		None => (buses.main_input_channels, buses.main_output_channels),
	};
	let (sidechain_channels, aux_outputs) = layout
		.as_ref()
		.map(|l| (l.sidechain_channels(), l.aux_outputs()))
		.unwrap_or_default();
	let mut d = PluginDescriptor {
		format: PluginFormat::Vst3,
		id: id.clone(),
		name: if class.name.is_empty() {
			id.clone()
		} else {
			class.name.clone()
		},
		vendor: if class.vendor.is_empty() {
			module.factory_vendor.clone()
		} else {
			class.vendor.clone()
		},
		version: if class.version.is_empty() {
			module_version.to_string()
		} else {
			class.version.clone()
		},
		path: path.to_path_buf(),
		role,
		features,
		description: String::new(),
		params,
		audio_inputs,
		audio_outputs,
		note_input: buses.event_inputs,
		latency_samples: latency,
		sidechain_channels,
		aux_outputs,
	};
	d.description = d.compose_description("");
	Ok(d)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn classify_uses_sub_categories_then_buses() {
		let (role, features) = classify("Instrument|Synth|Stereo", true, 2);
		assert_eq!(role, PluginRole::Instrument);
		assert_eq!(features, ["instrument", "synth", "stereo"]);
		let (role, features) = classify("Fx|Reverb", true, 2);
		assert_eq!(role, PluginRole::Effect);
		assert_eq!(features, ["audio-effect", "reverb"]);
		assert_eq!(classify("", true, 0).0, PluginRole::Instrument);
		assert_eq!(classify("Analyzer", false, 2).0, PluginRole::Effect);
	}
}
