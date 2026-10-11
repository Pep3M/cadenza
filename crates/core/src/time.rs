use std::ops::{Add, Sub};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Pulses per quarter note. 960 divides cleanly by 2, 3, 4, 5, 6, 8, 16, 32 and 64.
pub const PPQ: i64 = 960;

/// Musical position or duration in ticks (1 quarter note = [`PPQ`] ticks).
#[derive(
	Debug,
	Clone,
	Copy,
	PartialEq,
	Eq,
	Hash,
	PartialOrd,
	Ord,
	Default,
	Serialize,
	Deserialize,
	JsonSchema,
)]
#[serde(transparent)]
pub struct Tick(pub i64);

impl Tick {
	pub const ZERO: Tick = Tick(0);

	pub fn from_beats(beats: f64) -> Self {
		Tick((beats * PPQ as f64).round() as i64)
	}

	pub fn beats(self) -> f64 {
		self.0 as f64 / PPQ as f64
	}

	/// Position from 1-based bar/beat in a constant time signature.
	pub fn from_bar_beat(bar: u32, beat: f64, beats_per_bar: u32) -> Self {
		let bars = bar.saturating_sub(1) as f64;
		Tick::from_beats(bars * beats_per_bar as f64 + (beat - 1.0))
	}
}

impl Add for Tick {
	type Output = Tick;
	fn add(self, rhs: Tick) -> Tick {
		Tick(self.0 + rhs.0)
	}
}

impl Sub for Tick {
	type Output = Tick;
	fn sub(self, rhs: Tick) -> Tick {
		Tick(self.0 - rhs.0)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn bar_beat_conversion() {
		assert_eq!(Tick::from_bar_beat(1, 1.0, 4), Tick(0));
		assert_eq!(Tick::from_bar_beat(2, 1.0, 4), Tick(4 * PPQ));
		assert_eq!(Tick::from_bar_beat(1, 2.5, 4), Tick(PPQ + PPQ / 2));
	}
}
