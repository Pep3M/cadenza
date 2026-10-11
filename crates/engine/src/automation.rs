//! Automation lanes resolved to sample positions.
//!
//! Values are interpolated linearly in time between points (linear in samples, which equals
//! linear in ticks within a constant-tempo segment) and held before the first and after the
//! last point. The engine evaluates every lane once per [`crate::QUANTUM`] samples.

use cadenza_core::TempoMap;
use cadenza_model::AutomationLane;

#[derive(Debug, Clone)]
pub(crate) struct Curve {
	/// `(sample, value)`, strictly increasing in sample.
	points: Vec<(i64, f32)>,
}

impl Curve {
	pub fn from_lane(lane: &AutomationLane, tempo: &TempoMap, sample_rate: f64) -> Option<Self> {
		let mut points: Vec<(i64, f32)> = Vec::with_capacity(lane.points.len());
		let mut sorted = lane.points.clone();
		sorted.sort_by_key(|p| p.tick);
		for p in sorted {
			if !p.value.is_finite() {
				continue;
			}
			let sample =
				tempo.tick_to_sample(p.tick.max(cadenza_core::Tick::ZERO), sample_rate) as i64;
			match points.last_mut() {
				// Two ticks rounding to the same sample: the later point wins.
				Some(last) if last.0 == sample => last.1 = p.value,
				_ => points.push((sample, p.value)),
			}
		}
		(!points.is_empty()).then_some(Self { points })
	}

	pub fn value_at(&self, sample: i64) -> f32 {
		let i = self.points.partition_point(|p| p.0 <= sample);
		if i == 0 {
			return self.points[0].1;
		}
		if i == self.points.len() {
			return self.points[i - 1].1;
		}
		let (s0, v0) = self.points[i - 1];
		let (s1, v1) = self.points[i];
		let t = (sample - s0) as f64 / (s1 - s0) as f64;
		(v0 as f64 + (v1 - v0) as f64 * t) as f32
	}
}

/// Which slot of a channel's device chain a parameter lane drives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeviceSlot {
	Instrument,
	Insert(usize),
}

/// `None` node = master channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AutoTarget {
	Volume {
		node: Option<usize>,
	},
	Pan {
		node: Option<usize>,
	},
	Param {
		node: Option<usize>,
		slot: DeviceSlot,
		index: usize,
	},
}

#[derive(Debug, Clone)]
pub(crate) struct Binding {
	pub curve: Curve,
	pub target: AutoTarget,
	/// Last value sent, to avoid redundant `set_param` calls; NaN forces a send.
	pub last: f32,
}

#[cfg(test)]
mod tests {
	use super::*;
	use cadenza_core::{PPQ, ParamAddress, Tick, TrackId};
	use cadenza_model::AutomationPoint;

	#[test]
	fn interpolates_and_holds() {
		let lane = AutomationLane::new(
			ParamAddress::track_volume(TrackId::new()),
			vec![
				AutomationPoint::new(Tick(PPQ), -10.0),
				AutomationPoint::new(Tick(PPQ * 2), 0.0),
			],
		);
		let c = Curve::from_lane(&lane, &TempoMap::constant(120.0), 48_000.0).unwrap();
		assert_eq!(c.value_at(0), -10.0);
		assert_eq!(c.value_at(24_000), -10.0);
		assert!((c.value_at(36_000) + 5.0).abs() < 1e-6);
		assert_eq!(c.value_at(48_000), 0.0);
		assert_eq!(c.value_at(1_000_000), 0.0);
	}

	#[test]
	fn empty_lane_has_no_curve() {
		let lane = AutomationLane::new(ParamAddress::track_pan(TrackId::new()), vec![]);
		assert!(Curve::from_lane(&lane, &TempoMap::constant(120.0), 48_000.0).is_none());
	}
}
