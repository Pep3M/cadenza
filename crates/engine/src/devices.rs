//! Standalone device instances (outside a render graph): capturing a plugin's current
//! state for the project, and listing/loading its factory presets. Not real-time: every
//! call loads the plugin (in its sandbox unless hosting says otherwise) and drops it.
//!
//! State capture is explicit: the AI tools call [`capture_state`] after loading a preset
//! or editing a plugin, and on project save, then store the result through
//! `Command::SetDeviceState` so it is undoable and reloads exactly.

use cadenza_dsp::{PresetInfo, Processor};
use cadenza_model::{Device, DeviceKind};
use cadenza_plugin_host::{
	PluginCatalog, PluginDescriptor, PluginEditor, PluginError, PluginFormat, ProcessMode,
	SandboxMonitor,
};

use crate::QUANTUM;
use crate::error::EngineError;
use crate::hosting::{PluginHosting, sandbox_unavailable};

/// Sample rate standalone instances are activated at (state does not depend on it).
const STANDALONE_SAMPLE_RATE: f32 = 48_000.0;

/// The descriptor of a plugin device: the exact file first, else the same id anywhere.
pub(crate) fn find_plugin<'c>(
	catalog: &'c PluginCatalog,
	format: PluginFormat,
	id: &str,
	path: &std::path::Path,
) -> Option<&'c PluginDescriptor> {
	catalog
		.find_at(id, path)
		.filter(|d| d.format == format)
		.or_else(|| {
			catalog
				.descriptors()
				.find(|d| d.id == id && d.format == format)
		})
}

/// A plugin instance with its editor handle and, when sandboxed, the monitor to watch it.
pub(crate) struct CreatedPlugin {
	pub processor: Box<dyn Processor>,
	pub monitor: Option<SandboxMonitor>,
	pub editor: PluginEditor,
}

/// Creates a plugin instance, sandboxed when `sandboxed`.
pub(crate) fn create_plugin(
	desc: &PluginDescriptor,
	sample_rate: f32,
	state: Option<&[u8]>,
	mode: ProcessMode,
	hosting: &PluginHosting,
	sandboxed: bool,
) -> Result<CreatedPlugin, PluginError> {
	if sandboxed {
		let p = cadenza_plugin_host::instantiate_sandboxed(
			desc,
			sample_rate,
			QUANTUM,
			state,
			mode,
			hosting.sandbox.clone(),
		)?;
		Ok(CreatedPlugin {
			monitor: Some(p.monitor()),
			editor: p.editor(),
			processor: Box::new(p),
		})
	} else {
		let (processor, editor) =
			cadenza_plugin_host::instantiate_with_editor(desc, sample_rate, QUANTUM, state, mode)?;
		Ok(CreatedPlugin {
			processor,
			monitor: None,
			editor,
		})
	}
}

fn plugin_ref(device: &Device) -> Option<(PluginFormat, &str, &std::path::Path)> {
	match &device.kind {
		DeviceKind::Builtin { .. } => None,
		DeviceKind::Clap { plugin_id, path } => Some((PluginFormat::Clap, plugin_id, path)),
		DeviceKind::Vst3 { class_id, path } => Some((PluginFormat::Vst3, class_id, path)),
	}
}

fn device_error(device: &Device, reason: impl std::fmt::Display) -> EngineError {
	EngineError::Device(format!(
		"device '{}' ({}): {reason}",
		device.name, device.id
	))
}

/// Loads `device` standalone. `with_settings`: restore its stored state and explicit
/// params (as a render would); otherwise start from the plugin's defaults.
fn open_plugin(
	device: &Device,
	plugins: Option<&PluginCatalog>,
	hosting: &PluginHosting,
	with_settings: bool,
) -> Result<(Box<dyn Processor>, &'static str), EngineError> {
	let Some((format, id, path)) = plugin_ref(device) else {
		return Err(device_error(device, "is a built-in device, not a plugin"));
	};
	let desc = plugins
		.and_then(|c| find_plugin(c, format, id, path))
		.ok_or_else(|| {
			device_error(
				device,
				format!(
					"{} plugin '{id}' is not installed or not scanned (expected at '{}'); run scan_plugins",
					format.as_str(),
					path.display()
				),
			)
		})?;
	let state = match device.state.as_deref() {
		Some(encoded) if with_settings => Some(
			cadenza_plugin_host::decode_state(id, encoded).map_err(|e| device_error(device, e))?,
		),
		_ => None,
	};
	let sandboxed = hosting.mode == cadenza_plugin_host::HostingMode::Sandboxed
		&& sandbox_unavailable(&hosting.sandbox).is_none();
	let mut processor = create_plugin(
		desc,
		STANDALONE_SAMPLE_RATE,
		state.as_deref(),
		ProcessMode::Realtime,
		hosting,
		sandboxed,
	)
	.map_err(|e| device_error(device, format!("failed to load: {e}")))?
	.processor;
	if with_settings {
		let info = processor.info();
		for (param, &value) in &device.params {
			if let Some(i) = info.param_index(param) {
				processor.set_param(i, value);
			}
		}
	}
	Ok((processor, format.as_str()))
}

/// The current state of a plugin device (stored state + explicit params, as the plugin
/// itself serializes them), base64-encoded for `Device.state`. `Ok(None)` for built-in
/// devices (their params are their whole state) and plugins without a state extension.
pub fn capture_state(
	device: &Device,
	plugins: Option<&PluginCatalog>,
	hosting: &PluginHosting,
) -> Result<Option<String>, EngineError> {
	if plugin_ref(device).is_none() {
		return Ok(None);
	}
	let (mut processor, _) = open_plugin(device, plugins, hosting, true)?;
	Ok(processor
		.save_state()
		.map(|s| cadenza_plugin_host::encode_state(&s)))
}

/// Factory presets of a plugin device (empty for plugins without any). Built-in presets
/// come from [`cadenza_dsp::factory_presets`].
pub fn plugin_presets(
	device: &Device,
	plugins: Option<&PluginCatalog>,
	hosting: &PluginHosting,
) -> Result<Vec<PresetInfo>, EngineError> {
	let (mut processor, _) = open_plugin(device, plugins, hosting, false)?;
	Ok(processor.presets())
}

/// Loads factory preset `key` (from [`plugin_presets`]) into a fresh instance of the
/// plugin and returns the resulting state (base64), to be stored with cleared params.
pub fn preset_state(
	device: &Device,
	key: &str,
	plugins: Option<&PluginCatalog>,
	hosting: &PluginHosting,
) -> Result<String, EngineError> {
	let (mut processor, format) = open_plugin(device, plugins, hosting, false)?;
	if !processor.load_preset(key) {
		return Err(device_error(
			device,
			format!("the {format} plugin refused the preset; list them again with list_presets"),
		));
	}
	let state = processor.save_state().ok_or_else(|| {
		device_error(
			device,
			"the plugin cannot save its state, so the preset cannot be stored in the project",
		)
	})?;
	Ok(cadenza_plugin_host::encode_state(&state))
}
