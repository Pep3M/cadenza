use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::time::{PPQ, Tick};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TempoPoint {
	pub tick: Tick,
	pub bpm: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TimeSignature {
	pub numerator: u32,
	pub denominator: u32,
}

impl TimeSignature {
	/// Length of one bar in quarter notes.
	pub fn quarters_per_bar(&self) -> f64 {
		self.numerator as f64 * 4.0 / self.denominator as f64
	}

	pub fn ticks_per_bar(&self) -> Tick {
		Tick::from_beats(self.quarters_per_bar())
	}
}

impl Default for TimeSignature {
	fn default() -> Self {
		Self { numerator: 4, denominator: 4 }
	}
}

/// Step tempo map: each point holds its tempo until the next one.
/// Points are kept sorted and the first point is always at tick 0.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TempoMap {
	points: Vec<TempoPoint>,
}

impl TempoMap {
	pub fn constant(bpm: f64) -> Self {
		Self { points: vec![TempoPoint { tick: Tick::ZERO, bpm }] }
	}

	pub fn points(&self) -> &[TempoPoint] {
		&self.points
	}

	pub fn set_point(&mut self, tick: Tick, bpm: f64) {
		match self.points.binary_search_by_key(&tick, |p| p.tick) {
			Ok(i) => self.points[i].bpm = bpm,
			Err(i) => self.points.insert(i, TempoPoint { tick, bpm }),
		}
	}

	pub fn remove_point(&mut self, tick: Tick) -> Option<TempoPoint> {
		if tick == Tick::ZERO {
			return None;
		}
		let i = self.points.binary_search_by_key(&tick, |p| p.tick).ok()?;
		Some(self.points.remove(i))
	}

	pub fn bpm_at(&self, tick: Tick) -> f64 {
		let i = self.points.partition_point(|p| p.tick <= tick);
		self.points[i.saturating_sub(1)].bpm
	}

	pub fn tick_to_seconds(&self, tick: Tick) -> f64 {
		let mut seconds = 0.0;
		for (i, point) in self.points.iter().enumerate() {
			if point.tick >= tick {
				break;
			}
			let end = self.points.get(i + 1).map_or(tick, |next| next.tick.min(tick));
			seconds += seconds_for_ticks(end.0 - point.tick.0, point.bpm);
		}
		seconds
	}

	pub fn seconds_to_tick(&self, seconds: f64) -> Tick {
		let mut elapsed = 0.0;
		for (i, point) in self.points.iter().enumerate() {
			let segment = self.points.get(i + 1).map(|next| seconds_for_ticks(next.tick.0 - point.tick.0, point.bpm));
			match segment {
				Some(len) if elapsed + len <= seconds => elapsed += len,
				_ => {
					let ticks = (seconds - elapsed) * point.bpm / 60.0 * PPQ as f64;
					return Tick(point.tick.0 + ticks.round() as i64);
				}
			}
		}
		unreachable!("tempo map always has a point at tick 0")
	}

	pub fn tick_to_sample(&self, tick: Tick, sample_rate: f64) -> u64 {
		(self.tick_to_seconds(tick) * sample_rate).round() as u64
	}
}

impl Default for TempoMap {
	fn default() -> Self {
		Self::constant(120.0)
	}
}

fn seconds_for_ticks(ticks: i64, bpm: f64) -> f64 {
	ticks as f64 / PPQ as f64 * 60.0 / bpm
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn constant_tempo() {
		let map = TempoMap::constant(120.0);
		assert!((map.tick_to_seconds(Tick(PPQ * 4)) - 2.0).abs() < 1e-9);
		assert_eq!(map.seconds_to_tick(2.0), Tick(PPQ * 4));
		assert_eq!(map.tick_to_sample(Tick(PPQ), 48_000.0), 24_000);
	}

	#[test]
	fn tempo_change_round_trip() {
		let mut map = TempoMap::constant(120.0);
		map.set_point(Tick(PPQ * 4), 60.0);
		// 4 beats at 120 = 2s, then 2 beats at 60 = 2s
		assert!((map.tick_to_seconds(Tick(PPQ * 6)) - 4.0).abs() < 1e-9);
		assert_eq!(map.seconds_to_tick(4.0), Tick(PPQ * 6));
		assert_eq!(map.bpm_at(Tick(PPQ * 5)), 60.0);
	}
}
