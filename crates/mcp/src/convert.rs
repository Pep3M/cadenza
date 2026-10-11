//! Shared AI-facing conversions: pitches, device specs, beats <-> ticks.

use std::borrow::Cow;
use std::collections::BTreeMap;

use cadenza_core::Tick;
use cadenza_dsp::{DeviceInfo, DeviceRole};
use cadenza_model::Device;
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

/// A built-in device chosen by its registry id, with optional parameter overrides.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct DeviceSpec {
	/// Device id from `list_devices`, e.g. `"poly_synth"`, `"reverb"`.
	pub device: String,
	/// Plain (unnormalized) parameter values by id; omitted params use the device default.
	#[serde(default)]
	pub params: BTreeMap<String, f32>,
}

/// Looks up `id` in the registry, checking its role, and returns its static info.
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
		let Some(p) = info.param(id) else {
			return Err(McpError::UnknownParam {
				device: info.id.to_owned(),
				param: id.clone(),
				available: info
					.params
					.iter()
					.map(|p| p.id)
					.collect::<Vec<_>>()
					.join(", "),
			});
		};
		if !value.is_finite() || *value < p.min || *value > p.max {
			return Err(McpError::ParamOutOfRange {
				device: info.id.to_owned(),
				param: id.clone(),
				value: *value,
				min: p.min,
				max: p.max,
			});
		}
	}
	Ok(())
}

/// Builds a model [`Device`] from a [`DeviceSpec`], validating the device id, its role
/// and every parameter value.
pub fn build_device(spec: &DeviceSpec, expected_role: DeviceRole) -> Result<Device, McpError> {
	let info = lookup_device(&spec.device, expected_role)?;
	validate_params(info, &spec.params)?;
	let mut device = Device::builtin(info.id, info.name);
	for (id, value) in &spec.params {
		device.params.insert(id.clone(), *value);
	}
	Ok(device)
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

/// The static info of `device` (on `track`, `None` = master) if it is a built-in device.
pub fn builtin_info_for(
	project: &cadenza_model::Project,
	track: Option<cadenza_core::TrackId>,
	device: cadenza_core::DeviceId,
) -> Option<&'static DeviceInfo> {
	let d = project.device(track, device)?;
	match &d.kind {
		cadenza_model::DeviceKind::Builtin { id } => cadenza_dsp::device_info(id),
		_ => None,
	}
}

/// Validates `value` for `param` against `info`'s declared range, if known. Devices that
/// are not built-ins (CLAP/VST3) are not range-checked here.
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
