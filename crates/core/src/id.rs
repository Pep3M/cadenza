use std::fmt;
use std::str::FromStr;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

macro_rules! typed_id {
	($name:ident) => {
		#[derive(
			Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
		)]
		#[serde(transparent)]
		pub struct $name(pub Uuid);

		impl $name {
			pub fn new() -> Self {
				Self(Uuid::new_v4())
			}
		}

		impl Default for $name {
			fn default() -> Self {
				Self::new()
			}
		}

		impl fmt::Display for $name {
			fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
				self.0.fmt(f)
			}
		}

		impl FromStr for $name {
			type Err = uuid::Error;

			fn from_str(s: &str) -> Result<Self, Self::Err> {
				Uuid::parse_str(s).map(Self)
			}
		}
	};
}

typed_id!(TrackId);
typed_id!(ClipId);
typed_id!(NoteId);
typed_id!(DeviceId);
