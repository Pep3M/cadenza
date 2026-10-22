//! Shared AI-facing conversions: pitches, device specs, beats <-> ticks.

use std::borrow::Cow;
use std::collections::BTreeMap;

use cadenza_core::{DeviceId, Tick};
use cadenza_dsp::{DeviceInfo, DeviceRole};
use cadenza_model::{Device, DeviceKind};
use cadenza_plugin_host::{PluginCatalog, PluginDescriptor, PluginFormat, PluginParam};
use schemars::JsonSchema;
use serde::{Deserialize, Deserializer};

use crate::error::McpError;

/// A MIDI pitch accepted either as a note name (`"C4"`, `"F#3"`) or a bare MIDI number
/// (`60`). MIDI 60 is C4.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PitchInput(pub u8);

impl<'de> Deserialize<'de> for PitchInput {
	fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
		let value = serde_json::Value::deserialize(deserializer)?;
		let text: Cow<'_, str> = match &value {
			serde_json::Value::String(s) => Cow::Borrowed(s.as_str()),
			serde_json::Value::Number(n) => Cow::Owned(n.to_string()),
			other => {
				return Err(serde::de::Error::custom(format!(
					"pitch must be a note name string (e.g. \"C4\") or a MIDI number 0..=127, got {other}"
				)));
			}
		};
		cadenza_theory::parse_pitch(&text)
			.map(PitchInput)
			.map_err(serde::de::Error::custom)
	}
}

impl JsonSchema for PitchInput {
	fn schema_name() -> Cow<'static, str> {
		"Pitch".into()
	}

	fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
		schemars::json_schema!({
			"anyOf": [
				{ "type": "string" },
				{ "type": "integer", "minimum": 0, "maximum": 127 }
			],
			"description": "A note name (\"C4\", \"F#3\", MIDI 60 = C4) or a bare MIDI number (0..=127)."
		})
	}
}

/// Prefix of device ids naming a CLAP plugin: `clap:<plugin id>`.
pub const CLAP_PREFIX: &str = "clap:";
/// Prefix of device ids naming a VST3 plugin: `vst3:<class id>`.
pub const VST3_PREFIX: &str = "vst3:";

/// A device chosen by its id, with optional parameter overrides.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct DeviceSpec {
	/// Device id from `list_devices`: a built-in id (`"poly_synth"`, `"reverb"`) or a plugin
	/// id (`"clap:<plugin id>"`, `"vst3:<class id>"`).
	pub device: String,
	/// Plain (unnormalized) parameter values; omitted params keep the device default. Keys
	/// are param ids; for plugins also an exact (case-insensitive) param name or
	/// `"module/name"` (see `get_device_info`).
	#[serde(default)]
	pub params: BTreeMap<String, f32>,
}

/// A resolved device: a built-in registry entry or a scanned plugin.
#[derive(Debug, Clone, Copy)]
pub enum DeviceRef<'a> {
	Builtin(&'static DeviceInfo),
	Plugin(&'a PluginDescriptor),
}

impl DeviceRef<'_> {
	pub fn role(&self) -> DeviceRole {
		match self {
			DeviceRef::Builtin(info) => info.role,
			DeviceRef::Plugin(desc) => desc.role.into(),
		}
	}

	/// The stable id used by `list_devices`/`insert_device`.
	pub fn device_id(&self) -> String {
		match self {
			DeviceRef::Builtin(info) => info.id.to_owned(),
			DeviceRef::Plugin(desc) => plugin_device_id(desc),
		}
	}
}

/// `clap:<plugin id>` / `vst3:<class id>`.
pub fn plugin_device_id(desc: &PluginDescriptor) -> String {
	let prefix = match desc.format {
		PluginFormat::Clap => CLAP_PREFIX,
		PluginFormat::Vst3 => VST3_PREFIX,
	};
	format!("{prefix}{}", desc.id)
}

fn find_plugin<'a>(
	catalog: Option<&'a PluginCatalog>,
	format: PluginFormat,
	id: &str,
) -> Option<&'a PluginDescriptor> {
	catalog?
		.descriptors()
		.find(|d| d.format == format && d.id == id)
}

/// Resolves a device id: `clap:`/`vst3:` ids against the plugin catalog, otherwise the
/// built-in registry, then (leniently) a bare plugin id. `catalog` may be `None` when
/// [`needs_catalog`] is false.
pub fn resolve_device<'a>(
	id: &str,
	expected_role: DeviceRole,
	catalog: Option<&'a PluginCatalog>,
) -> Result<DeviceRef<'a>, McpError> {
	let id = id.trim();
	let found = if let Some(plugin) = id.strip_prefix(CLAP_PREFIX) {
		DeviceRef::Plugin(
			find_plugin(catalog, PluginFormat::Clap, plugin)
				.ok_or_else(|| McpError::UnknownPlugin { id: id.to_owned() })?,
		)
	} else if let Some(plugin) = id.strip_prefix(VST3_PREFIX) {
		DeviceRef::Plugin(
			find_plugin(catalog, PluginFormat::Vst3, plugin)
				.ok_or_else(|| McpError::UnknownPlugin { id: id.to_owned() })?,
		)
	} else if let Some(info) = cadenza_dsp::device_info(id) {
		DeviceRef::Builtin(info)
	} else if let Some(desc) = find_plugin(catalog, PluginFormat::Clap, id)
		.or_else(|| find_plugin(catalog, PluginFormat::Vst3, id))
	{
		DeviceRef::Plugin(desc)
	} else {
		return Err(McpError::UnknownDevice {
			id: id.to_owned(),
			role: role_name(expected_role),
			available: available_ids(expected_role),
		});
	};
	if found.role() != expected_role {
		return Err(McpError::WrongDeviceRole {
			id: id.to_owned(),
			expected: expected_role,
			actual: found.role(),
		});
	}
	Ok(found)
}

/// Whether resolving `id` may need the plugin catalog.
pub fn needs_catalog(id: &str) -> bool {
	cadenza_dsp::device_info(id.trim()).is_none()
}

/// Looks up `id` in the built-in registry, checking its role, and returns its static info.
pub fn lookup_device(id: &str, expected_role: DeviceRole) -> Result<&'static DeviceInfo, McpError> {
	let info = cadenza_dsp::device_info(id).ok_or_else(|| McpError::UnknownDevice {
		id: id.to_owned(),
		role: role_name(expected_role),
		available: available_ids(expected_role),
	})?;
	if info.role != expected_role {
		return Err(McpError::WrongDeviceRole {
			id: id.to_owned(),
			expected: expected_role,
			actual: info.role,
		});
	}
	Ok(info)
}

/// Validates every entry of `params` against `info`'s declared ranges.
pub fn validate_params(
	info: &'static DeviceInfo,
	params: &BTreeMap<String, f32>,
) -> Result<(), McpError> {
	for (id, value) in params {
		check_param_range(Some(info), id, *value)?;
	}
	Ok(())
}

/// Finds a plugin parameter by descriptor id (decimal string), exact `module/name` or
/// exact name, all case-insensitive except the id.
pub fn resolve_plugin_param<'a>(
	desc: &'a PluginDescriptor,
	key: &str,
) -> Result<&'a PluginParam, McpError> {
	let key = key.trim();
	if let Some(p) = desc.params.iter().find(|p| p.id == key) {
		return Ok(p);
	}
	let wanted = key.to_lowercase();
	let full = |p: &PluginParam| format!("{}/{}", module_path(p), p.name).to_lowercase();
	if wanted.contains('/')
		&& let Some(p) = desc.params.iter().find(|p| full(p) == wanted)
	{
		return Ok(p);
	}
	let by_name: Vec<&PluginParam> = desc
		.params
		.iter()
		.filter(|p| p.name.to_lowercase() == wanted)
		.collect();
	let device = plugin_device_id(desc);
	match by_name.as_slice() {
		[p] => Ok(p),
		[] => {
			let similar: Vec<String> = desc
				.params
				.iter()
				.filter(|p| full(p).contains(&wanted))
				.take(8)
				.map(describe_param)
				.collect();
			let available = if desc.params.len() <= 12 {
				desc.params
					.iter()
					.map(describe_param)
					.collect::<Vec<_>>()
					.join(", ")
			} else if similar.is_empty() {
				format!(
					"{} parameters; search them with get_device_info {{ device: \"{device}\", param_query }}",
					desc.params.len()
				)
			} else {
				format!(
					"similar: {}; more with get_device_info {{ device: \"{device}\", param_query }}",
					similar.join(", ")
				)
			};
			Err(McpError::UnknownParam {
				device,
				param: key.to_owned(),
				available,
			})
		}
		many => Err(McpError::AmbiguousParam {
			device,
			param: key.to_owned(),
			candidates: many
				.iter()
				.take(8)
				.map(|p| describe_param(p))
				.collect::<Vec<_>>()
				.join(", "),
		}),
	}
}

fn describe_param(p: &PluginParam) -> String {
	let module = module_path(p);
	if module.is_empty() {
		format!("'{}' (id {})", p.name, p.id)
	} else {
		format!("'{module}/{}' (id {})", p.name, p.id)
	}
}

/// A plugin param's group path without leading/trailing separators (some plugins report
/// "/Global & FX/"), as shown by get_device_info and accepted in "module/name".
pub fn module_path(p: &PluginParam) -> &str {
	p.module.trim_matches('/')
}

/// Resolves `key` to the stored param id for `device` and validates `value` against the
/// declared range. Unknown devices (not built-in, plugin not scanned) pass through as-is.
pub fn resolve_param(
	device: Option<DeviceRef<'_>>,
	key: &str,
	value: f32,
) -> Result<String, McpError> {
	match device {
		None => Ok(key.to_owned()),
		Some(DeviceRef::Builtin(info)) => {
			check_param_range(Some(info), key, value)?;
			Ok(key.to_owned())
		}
		Some(DeviceRef::Plugin(desc)) => {
			let p = resolve_plugin_param(desc, key)?;
			let v = f64::from(value);
			let tolerance = 1e-6 * (p.max - p.min).abs().max(1.0);
			if !value.is_finite() || v < p.min - tolerance || v > p.max + tolerance {
				return Err(McpError::ParamOutOfRange {
					device: plugin_device_id(desc),
					param: format!("{} ({})", p.name, p.id),
					value,
					min: p.min as f32,
					max: p.max as f32,
				});
			}
			Ok(p.id.clone())
		}
	}
}

/// Resolves `key` to the stored param id for `device` without checking a value.
pub fn resolve_param_key(device: Option<DeviceRef<'_>>, key: &str) -> Result<String, McpError> {
	match device {
		None => Ok(key.to_owned()),
		Some(DeviceRef::Builtin(info)) => {
			let default = info.param(key).map_or(0.0, |p| p.default);
			check_param_range(Some(info), key, default)?;
			Ok(key.to_owned())
		}
		Some(DeviceRef::Plugin(desc)) => Ok(resolve_plugin_param(desc, key)?.id.clone()),
	}
}

/// Builds a model [`Device`] from a [`DeviceSpec`], validating the device id, its role
/// and every parameter value. Plugin parameter names are stored as their ids.
pub fn build_device(
	spec: &DeviceSpec,
	expected_role: DeviceRole,
	catalog: Option<&PluginCatalog>,
) -> Result<Device, McpError> {
	let resolved = resolve_device(&spec.device, expected_role, catalog)?;
	let mut params = BTreeMap::new();
	for (key, value) in &spec.params {
		let id = resolve_param(Some(resolved), key, *value)?;
		if params.insert(id.clone(), *value).is_some() {
			return Err(McpError::Invalid(format!(
				"parameter '{id}' of device '{}' is given more than once (by id and by name?)",
				spec.device
			)));
		}
	}
	let (kind, name) = match resolved {
		DeviceRef::Builtin(info) => (
			DeviceKind::Builtin {
				id: info.id.to_owned(),
			},
			info.name.to_owned(),
		),
		DeviceRef::Plugin(desc) => (
			match desc.format {
				PluginFormat::Clap => DeviceKind::Clap {
					plugin_id: desc.id.clone(),
					path: desc.path.clone(),
				},
				PluginFormat::Vst3 => DeviceKind::Vst3 {
					class_id: desc.id.clone(),
					path: desc.path.clone(),
				},
			},
			desc.name.clone(),
		),
	};
	// Samplers start with their default factory content so they sound out of the box;
	// explicit params apply on top of the preset's.
	let mut zones = Vec::new();
	if let DeviceKind::Builtin { id } = &kind {
		match cadenza_engine::factory::default_content(id) {
			Ok(Some(content)) => {
				let mut merged = content.params;
				merged.extend(params);
				params = merged;
				zones = content.zones;
			}
			Ok(None) => {}
			Err(e) => tracing::warn!("factory samples unavailable: {e}"),
		}
	}
	Ok(Device {
		id: DeviceId::new(),
		kind,
		name,
		bypass: false,
		params,
		state: None,
		sidechain: None,
		aux_outputs: Vec::new(),
		zones,
		modulators: Vec::new(),
	})
}

fn role_name(role: DeviceRole) -> &'static str {
	match role {
		DeviceRole::Instrument => "instrument",
		DeviceRole::Effect => "effect",
	}
}

fn available_ids(role: DeviceRole) -> String {
	cadenza_dsp::registry()
		.iter()
		.filter(|d| d.role == role)
		.map(|d| d.id)
		.collect::<Vec<_>>()
		.join(", ")
}

/// Whether `device` (on `track`, `None` = master) is a third-party plugin.
pub fn is_plugin_device(
	project: &cadenza_model::Project,
	track: Option<cadenza_core::TrackId>,
	device: DeviceId,
) -> bool {
	project
		.device(track, device)
		.is_some_and(|d| !matches!(d.kind, DeviceKind::Builtin { .. }))
}

/// What is known about an existing device in the project: its built-in info, or its
/// plugin descriptor when the plugin is in `catalog`.
pub fn device_ref_for<'a>(
	project: &cadenza_model::Project,
	track: Option<cadenza_core::TrackId>,
	device: DeviceId,
	catalog: Option<&'a PluginCatalog>,
) -> Option<DeviceRef<'a>> {
	let d = project.device(track, device)?;
	match &d.kind {
		DeviceKind::Builtin { id } => cadenza_dsp::device_info(id).map(DeviceRef::Builtin),
		DeviceKind::Clap { plugin_id, path } => catalog?
			.find_at(plugin_id, path)
			.filter(|p| p.format == PluginFormat::Clap)
			.map(DeviceRef::Plugin),
		DeviceKind::Vst3 { class_id, path } => catalog?
			.find_at(class_id, path)
			.filter(|p| p.format == PluginFormat::Vst3)
			.map(DeviceRef::Plugin),
	}
}

/// The catalog descriptor of a plugin device (exact file first, then the same id).
pub fn plugin_ref_of<'a>(
	device: &Device,
	catalog: &'a PluginCatalog,
) -> Option<&'a PluginDescriptor> {
	let (format, id, path) = match &device.kind {
		DeviceKind::Builtin { .. } => return None,
		DeviceKind::Clap { plugin_id, path } => (PluginFormat::Clap, plugin_id, path),
		DeviceKind::Vst3 { class_id, path } => (PluginFormat::Vst3, class_id, path),
	};
	catalog
		.find_at(id, path)
		.filter(|d| d.format == format)
		.or_else(|| find_plugin(Some(catalog), format, id))
}

/// Validates `value` for `param` against `info`'s declared range, if known.
pub fn check_param_range(
	info: Option<&'static DeviceInfo>,
	param: &str,
	value: f32,
) -> Result<(), McpError> {
	let Some(info) = info else { return Ok(()) };
	let Some(p) = info.param(param) else {
		return Err(McpError::UnknownParam {
			device: info.id.to_owned(),
			param: param.to_owned(),
			available: info
				.params
				.iter()
				.map(|p| p.id)
				.collect::<Vec<_>>()
				.join(", "),
		});
	};
	if !value.is_finite() || value < p.min || value > p.max {
		return Err(McpError::ParamOutOfRange {
			device: info.id.to_owned(),
			param: param.to_owned(),
			value,
			min: p.min,
			max: p.max,
		});
	}
	Ok(())
}

pub fn beats_to_tick(beats: f64) -> Tick {
	Tick::from_beats(beats)
}

pub fn tick_to_beats(tick: Tick) -> f64 {
	(tick.beats() * 1000.0).round() / 1000.0
}

#[cfg(test)]
mod plugin_param_tests {
	use super::*;
	use cadenza_plugin_host::PluginRole;

	fn param(id: &str, module: &str, name: &str) -> PluginParam {
		PluginParam {
			id: id.into(),
			name: name.into(),
			module: module.into(),
			min: 0.0,
			max: 1.0,
			default: 0.5,
			stepped: false,
			automatable: true,
			bypass: false,
		}
	}

	fn synth() -> PluginDescriptor {
		PluginDescriptor {
			format: PluginFormat::Clap,
			id: "com.example.synth".into(),
			name: "Synth".into(),
			vendor: "Example".into(),
			version: "1".into(),
			path: "/x.clap".into(),
			role: PluginRole::Instrument,
			features: vec!["instrument".into()],
			description: String::new(),
			params: vec![
				param("10", "/A Filter 1/", "Cutoff"),
				param("11", "/B Filter 1/", "Cutoff"),
				param("12", "/Global/", "Global Volume"),
			],
			audio_inputs: 0,
			audio_outputs: 2,
			note_input: true,
			latency_samples: 0,
			sidechain_channels: 0,
			aux_outputs: Vec::new(),
		}
	}

	#[test]
	fn params_resolve_by_id_name_and_module_path() {
		let d = synth();
		assert_eq!(resolve_plugin_param(&d, "11").unwrap().id, "11");
		assert_eq!(resolve_plugin_param(&d, "global volume").unwrap().id, "12");
		assert_eq!(
			resolve_plugin_param(&d, "b filter 1/CUTOFF").unwrap().id,
			"11"
		);
		let err = resolve_plugin_param(&d, "cutoff").unwrap_err();
		assert!(matches!(err, McpError::AmbiguousParam { .. }), "{err}");
		assert!(
			err.to_string().contains("'A Filter 1/Cutoff' (id 10)"),
			"{err}"
		);
		let err = resolve_plugin_param(&d, "resonance").unwrap_err();
		assert!(matches!(err, McpError::UnknownParam { .. }), "{err}");
	}

	#[test]
	fn plugin_values_are_range_checked() {
		let d = synth();
		let r = Some(DeviceRef::Plugin(&d));
		assert_eq!(resolve_param(r, "Global Volume", 0.25).unwrap(), "12");
		assert!(matches!(
			resolve_param(r, "12", 1.5),
			Err(McpError::ParamOutOfRange { .. })
		));
		assert!(resolve_param(r, "12", f32::NAN).is_err());
		assert_eq!(plugin_device_id(&d), "clap:com.example.synth");
	}
}
