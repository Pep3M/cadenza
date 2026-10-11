//! Device discovery: the built-in instrument/effect registry, described for an AI
//! choosing and configuring devices.

use cadenza_dsp::DeviceRole;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RoleFilter {
	Instrument,
	Effect,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListDevicesArgs {
	/// Only list devices of this role; omit to list every built-in device.
	#[serde(default)]
	pub role: Option<RoleFilter>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ParamOut {
	pub id: String,
	pub name: String,
	pub min: f32,
	pub max: f32,
	pub default: f32,
	pub unit: String,
	pub description: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct DeviceOut {
	pub id: String,
	pub name: String,
	/// "instrument" or "effect".
	pub role: &'static str,
	pub description: String,
	pub params: Vec<ParamOut>,
}

pub fn list_devices(args: ListDevicesArgs) -> Vec<DeviceOut> {
	let wanted = args.role.map(|r| match r {
		RoleFilter::Instrument => DeviceRole::Instrument,
		RoleFilter::Effect => DeviceRole::Effect,
	});
	cadenza_dsp::registry()
		.iter()
		.filter(|d| wanted.is_none_or(|w| d.role == w))
		.map(|d| DeviceOut {
			id: d.id.to_owned(),
			name: d.name.to_owned(),
			role: match d.role {
				DeviceRole::Instrument => "instrument",
				DeviceRole::Effect => "effect",
			},
			description: d.description.to_owned(),
			params: d
				.params
				.iter()
				.map(|p| ParamOut {
					id: p.id.to_owned(),
					name: p.name.to_owned(),
					min: p.min,
					max: p.max,
					default: p.default,
					unit: p.unit.to_owned(),
					description: p.description.to_owned(),
				})
				.collect(),
		})
		.collect()
}
