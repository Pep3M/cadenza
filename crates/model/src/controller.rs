//! MIDI controller lanes of a MIDI clip: control changes (mod wheel, sustain pedal, any
//! CC#), pitch bend and channel pressure, as clip-relative events.
//!
//! Each point is one MIDI event: its value holds until the next point (MIDI semantics,
//! like FL Studio's event editor). Values are normalized: CC and pressure `0.0..=1.0`
//! (MIDI 0..=127), pitch bend `-1.0..=1.0` (0 = center).

use std::fmt;
use std::str::FromStr;

use cadenza_core::Tick;
use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// What a controller lane drives. JSON: `"cc1"`..`"cc127"` (and `"cc0"`), `"pitch_bend"`,
/// `"channel_pressure"`; names such as `"mod_wheel"`, `"sustain"`, `"expression"`,
/// `"bend"`, `"pressure"`, `"aftertouch"` are accepted on input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Controller {
	Cc(u8),
	PitchBend,
	ChannelPressure,
}

impl Controller {
	pub const MOD_WHEEL: Controller = Controller::Cc(1);
	pub const SUSTAIN: Controller = Controller::Cc(64);

	/// Value range of the lane.
	pub fn range(self) -> (f32, f32) {
		match self {
			Controller::PitchBend => (-1.0, 1.0),
			_ => (0.0, 1.0),
		}
	}

	/// The value a lane rests at before its first point (and is reset to when playback
	/// jumps before it): center for pitch bend, full for expression (CC11), 100 for
	/// volume (CC7), center for pan (CC10), else 0.
	pub fn default_value(self) -> f32 {
		match self {
			Controller::Cc(7) => 100.0 / 127.0,
			Controller::Cc(10) => 0.5,
			Controller::Cc(11) => 1.0,
			_ => 0.0,
		}
	}

	/// Short display name ("Mod wheel", "Sustain", "CC 74", "Pitch bend").
	pub fn label(self) -> String {
		match self {
			Controller::Cc(1) => "Mod wheel".into(),
			Controller::Cc(2) => "Breath".into(),
			Controller::Cc(7) => "Volume (CC 7)".into(),
			Controller::Cc(10) => "Pan (CC 10)".into(),
			Controller::Cc(11) => "Expression".into(),
			Controller::Cc(64) => "Sustain".into(),
			Controller::Cc(n) => format!("CC {n}"),
			Controller::PitchBend => "Pitch bend".into(),
			Controller::ChannelPressure => "Pressure".into(),
		}
	}

	/// Whether the lane is a switch (sustain-like pedals: on at >= 0.5).
	pub fn is_switch(self) -> bool {
		matches!(self, Controller::Cc(64..=69))
	}
}

impl fmt::Display for Controller {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Controller::Cc(n) => write!(f, "cc{n}"),
			Controller::PitchBend => f.write_str("pitch_bend"),
			Controller::ChannelPressure => f.write_str("channel_pressure"),
		}
	}
}

impl FromStr for Controller {
	type Err = String;

	fn from_str(s: &str) -> Result<Self, Self::Err> {
		let t = s.trim().to_ascii_lowercase().replace([' ', '-'], "_");
		let named = match t.as_str() {
			"pitch_bend" | "pitchbend" | "bend" | "pb" => Some(Controller::PitchBend),
			"channel_pressure" | "pressure" | "aftertouch" | "after_touch" | "at" => {
				Some(Controller::ChannelPressure)
			}
			"mod_wheel" | "modwheel" | "modulation" | "mod" => Some(Controller::Cc(1)),
			"breath" => Some(Controller::Cc(2)),
			"volume" => Some(Controller::Cc(7)),
			"pan" => Some(Controller::Cc(10)),
			"expression" => Some(Controller::Cc(11)),
			"sustain" | "sustain_pedal" | "damper" | "pedal" => Some(Controller::Cc(64)),
			_ => None,
		};
		if let Some(c) = named {
			return Ok(c);
		}
		let digits = t.strip_prefix("cc").map(|d| d.trim_start_matches('_'));
		match digits.and_then(|d| d.parse::<u8>().ok()) {
			Some(n) if n <= 127 => Ok(Controller::Cc(n)),
			_ => Err(format!(
				"unknown controller {s:?}; use \"cc0\"..\"cc127\" (e.g. \"cc1\" mod wheel, \"cc64\" sustain, \"cc11\" expression), \"pitch_bend\" or \"channel_pressure\""
			)),
		}
	}
}

impl Serialize for Controller {
	fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
		s.collect_str(self)
	}
}

impl<'de> Deserialize<'de> for Controller {
	fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
		let s = String::deserialize(d)?;
		s.parse().map_err(serde::de::Error::custom)
	}
}

impl JsonSchema for Controller {
	fn schema_name() -> std::borrow::Cow<'static, str> {
		"Controller".into()
	}

	fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
		schemars::json_schema!({
			"type": "string",
			"description": "\"cc0\"..\"cc127\" (cc1 mod wheel, cc64 sustain, cc11 expression, cc74 brightness), \"pitch_bend\" or \"channel_pressure\"",
		})
	}
}

/// One controller event: `value` holds from `tick` (clip-relative) to the next point.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ControllerPoint {
	pub tick: Tick,
	pub value: f32,
}

impl ControllerPoint {
	pub fn new(tick: Tick, value: f32) -> Self {
		Self { tick, value }
	}
}

/// The events of one controller in a MIDI clip, sorted by tick (one per tick).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ControllerLane {
	pub controller: Controller,
	pub points: Vec<ControllerPoint>,
}

impl ControllerLane {
	/// The value in effect at `tick` (the last point at or before it), `None` before the
	/// first point.
	pub fn value_at(&self, tick: Tick) -> Option<f32> {
		let i = self.points.partition_point(|p| p.tick <= tick);
		i.checked_sub(1).map(|i| self.points[i].value)
	}
}

/// The lanes cut to `[from, to)` and moved to start at 0 (clip splits and trims): each
/// lane gets a point at 0 with the value in effect at `from`, so the part sounds the same.
pub fn window(lanes: &[ControllerLane], from: Tick, to: Tick) -> Vec<ControllerLane> {
	lanes
		.iter()
		.filter_map(|l| {
			let mut points: Vec<ControllerPoint> = Vec::new();
			if let Some(v) = l.value_at(from)
				&& !l.points.iter().any(|p| p.tick == from)
			{
				points.push(ControllerPoint::new(Tick::ZERO, v));
			}
			points.extend(
				l.points
					.iter()
					.filter(|p| p.tick >= from && p.tick < to)
					.map(|p| ControllerPoint::new(p.tick - from, p.value)),
			);
			(!points.is_empty()).then_some(ControllerLane {
				controller: l.controller,
				points,
			})
		})
		.collect()
}

/// Points of a straight (or curved) ramp from `(from, a)` to `(to, b)`, one every `step`
/// ticks (both ends included), values rounded to the MIDI resolution of `controller`.
/// `shape` bends the ramp: 0 = linear, > 0 = slow start (exponential-like), < 0 = fast
/// start.
pub fn ramp(
	controller: Controller,
	from: Tick,
	a: f32,
	to: Tick,
	b: f32,
	step: Tick,
	shape: f32,
) -> Vec<ControllerPoint> {
	let (lo, hi) = controller.range();
	let quantize = |v: f32| {
		let v = v.clamp(lo, hi);
		match controller {
			Controller::PitchBend => (v * 8191.0).round() / 8191.0,
			_ => (v * 127.0).round() / 127.0,
		}
	};
	let (from, to) = if to < from { (to, from) } else { (from, to) };
	let span = (to - from).0;
	let step = step.0.max(1);
	if span == 0 {
		return vec![ControllerPoint::new(from, quantize(b))];
	}
	let mut out: Vec<ControllerPoint> = Vec::new();
	let mut t = 0;
	loop {
		let x = (t as f32 / span as f32).clamp(0.0, 1.0);
		let x = if shape > 0.0 {
			x.powf(1.0 + shape * 3.0)
		} else if shape < 0.0 {
			1.0 - (1.0 - x).powf(1.0 - shape * 3.0)
		} else {
			x
		};
		let v = quantize(a + (b - a) * x);
		if out.last().is_none_or(|p| p.value != v) || t == span {
			out.push(ControllerPoint::new(Tick(from.0 + t), v));
		}
		if t == span {
			break;
		}
		t = (t + step).min(span);
	}
	out
}

/// Replaces the points of `lane` in `[from, to]` with `new` (a drawn stroke), keeping the
/// rest; returns the sorted result.
pub fn splice(
	old: &[ControllerPoint],
	from: Tick,
	to: Tick,
	new: &[ControllerPoint],
) -> Vec<ControllerPoint> {
	let (from, to) = if to < from { (to, from) } else { (from, to) };
	let mut out: Vec<ControllerPoint> = old
		.iter()
		.filter(|p| p.tick < from || p.tick > to)
		.copied()
		.chain(new.iter().copied())
		.collect();
	out.sort_by_key(|p| p.tick);
	out.dedup_by_key(|p| p.tick);
	out
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn names_parse_and_print() {
		assert_eq!("cc64".parse::<Controller>(), Ok(Controller::SUSTAIN));
		assert_eq!("Sustain".parse::<Controller>(), Ok(Controller::SUSTAIN));
		assert_eq!("mod wheel".parse::<Controller>(), Ok(Controller::MOD_WHEEL));
		assert_eq!("CC 74".parse::<Controller>(), Ok(Controller::Cc(74)));
		assert_eq!("bend".parse::<Controller>(), Ok(Controller::PitchBend));
		assert!("cc128".parse::<Controller>().is_err());
		assert_eq!(Controller::Cc(74).to_string(), "cc74");
		let json = serde_json::to_string(&Controller::PitchBend).unwrap();
		assert_eq!(json, "\"pitch_bend\"");
		assert_eq!(
			serde_json::from_str::<Controller>("\"aftertouch\"").unwrap(),
			Controller::ChannelPressure
		);
	}

	#[test]
	fn windows_carry_the_value_in_effect() {
		let lane = ControllerLane {
			controller: Controller::MOD_WHEEL,
			points: vec![
				ControllerPoint::new(Tick(0), 0.2),
				ControllerPoint::new(Tick(100), 0.6),
				ControllerPoint::new(Tick(300), 1.0),
			],
		};
		let w = window(&[lane], Tick(150), Tick(400));
		assert_eq!(
			w[0].points,
			vec![
				ControllerPoint::new(Tick(0), 0.6),
				ControllerPoint::new(Tick(150), 1.0)
			]
		);
	}

	#[test]
	fn ramps_and_splices() {
		let r = ramp(
			Controller::MOD_WHEEL,
			Tick(0),
			0.0,
			Tick(960),
			1.0,
			Tick(240),
			0.0,
		);
		assert_eq!(r.len(), 5);
		assert_eq!(r[2].value, (0.5f32 * 127.0).round() / 127.0);
		assert_eq!(r.last().unwrap().value, 1.0);
		let curved = ramp(
			Controller::PitchBend,
			Tick(0),
			0.0,
			Tick(960),
			1.0,
			Tick(240),
			1.0,
		);
		assert!(curved[2].value < 0.5, "slow start");
		let old = [
			ControllerPoint::new(Tick(0), 0.1),
			ControllerPoint::new(Tick(500), 0.2),
			ControllerPoint::new(Tick(2000), 0.3),
		];
		let s = splice(&old, Tick(400), Tick(1000), &r[1..3]);
		let ticks: Vec<i64> = s.iter().map(|p| p.tick.0).collect();
		assert_eq!(ticks, vec![0, 240, 480, 2000]);
	}
}
