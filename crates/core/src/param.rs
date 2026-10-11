//! URI-style addressing for every automatable value, inspired by Studio One's
//! `param:///AudioMixer/{uid}/volume`. The AI addresses parameters with these strings.
//!
//! Grammar:
//! - `track/{track_id}/volume` | `track/{track_id}/pan`
//! - `track/{track_id}/device/{device_id}/{param_id}` (instrument or insert)
//! - `master/volume` | `master/device/{device_id}/{param_id}`

use std::fmt;
use std::str::FromStr;

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::id::{DeviceId, TrackId};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ParamTarget {
	Volume,
	Pan,
	Device { device: DeviceId, param: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ParamAddress {
	/// `None` addresses the master bus.
	pub track: Option<TrackId>,
	pub target: ParamTarget,
}

#[derive(Debug, thiserror::Error)]
#[error("invalid parameter address '{0}'")]
pub struct ParamAddressError(pub String);

impl ParamAddress {
	pub fn track_volume(track: TrackId) -> Self {
		Self { track: Some(track), target: ParamTarget::Volume }
	}

	pub fn track_pan(track: TrackId) -> Self {
		Self { track: Some(track), target: ParamTarget::Pan }
	}

	pub fn device(track: Option<TrackId>, device: DeviceId, param: impl Into<String>) -> Self {
		Self { track, target: ParamTarget::Device { device, param: param.into() } }
	}
}

impl fmt::Display for ParamAddress {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self.track {
			Some(id) => write!(f, "track/{id}/")?,
			None => write!(f, "master/")?,
		}
		match &self.target {
			ParamTarget::Volume => write!(f, "volume"),
			ParamTarget::Pan => write!(f, "pan"),
			ParamTarget::Device { device, param } => write!(f, "device/{device}/{param}"),
		}
	}
}

impl FromStr for ParamAddress {
	type Err = ParamAddressError;

	fn from_str(s: &str) -> Result<Self, Self::Err> {
		let err = || ParamAddressError(s.to_owned());
		let parts: Vec<&str> = s.trim_start_matches('/').split('/').collect();
		let (track, rest) = match parts.as_slice() {
			["master", rest @ ..] => (None, rest),
			["track", id, rest @ ..] => (Some(id.parse().map_err(|_| err())?), rest),
			_ => return Err(err()),
		};
		let target = match rest {
			["volume"] => ParamTarget::Volume,
			["pan"] => ParamTarget::Pan,
			["device", device, param] if !param.is_empty() => ParamTarget::Device {
				device: device.parse().map_err(|_| err())?,
				param: (*param).to_owned(),
			},
			_ => return Err(err()),
		};
		Ok(Self { track, target })
	}
}

impl Serialize for ParamAddress {
	fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
		serializer.collect_str(self)
	}
}

impl<'de> Deserialize<'de> for ParamAddress {
	fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
		let s = String::deserialize(deserializer)?;
		s.parse().map_err(serde::de::Error::custom)
	}
}

impl JsonSchema for ParamAddress {
	fn schema_name() -> std::borrow::Cow<'static, str> {
		"ParamAddress".into()
	}

	fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
		schemars::json_schema!({
			"type": "string",
			"description": "track/{track_id}/volume | track/{track_id}/pan | track/{track_id}/device/{device_id}/{param_id} | master/volume | master/device/{device_id}/{param_id}"
		})
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn round_trip() {
		let addr = ParamAddress::device(Some(TrackId::new()), DeviceId::new(), "cutoff");
		let parsed: ParamAddress = addr.to_string().parse().unwrap();
		assert_eq!(parsed, addr);
		assert_eq!("master/volume".parse::<ParamAddress>().unwrap().target, ParamTarget::Volume);
		assert!("track/nope/volume".parse::<ParamAddress>().is_err());
	}

	#[test]
	fn serde_as_string() {
		let addr = ParamAddress::track_pan(TrackId::new());
		let json = serde_json::to_string(&addr).unwrap();
		assert_eq!(json, format!("\"{addr}\""));
	}
}
