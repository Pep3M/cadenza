//! UI-side meter ballistics over the engine's lock-free [`MeterBank`]: peak fall-off,
//! peak hold and clip latch. Read once per frame for every strip (reading resets the
//! engine's peak accumulators, so there must be a single reader).

use std::collections::HashMap;

use cadenza_core::TrackId;
use cadenza_engine::{MeterBank, MeterReading};

/// Lowest level shown.
pub const FLOOR_DB: f32 = -60.0;
/// Peak bar fall-off speed.
const FALL_DB_PER_S: f32 = 24.0;
/// How long the peak-hold line stays before falling.
const HOLD_S: f32 = 1.5;

pub fn amp_to_db(amp: f32) -> f32 {
	if amp > 0.0 {
		(20.0 * amp.log10()).max(FLOOR_DB)
	} else {
		FLOOR_DB
	}
}

/// What a meter draws, per channel (dBFS, >= [`FLOOR_DB`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MeterLevels {
	pub peak_db: [f32; 2],
	pub rms_db: [f32; 2],
	/// Peak-hold line (both channels).
	pub hold_db: f32,
	hold_age: f32,
	/// Latched when a sample reached 0 dBFS; cleared by the user.
	pub clipped: bool,
}

impl Default for MeterLevels {
	fn default() -> Self {
		Self {
			peak_db: [FLOOR_DB; 2],
			rms_db: [FLOOR_DB; 2],
			hold_db: FLOOR_DB,
			hold_age: 0.0,
			clipped: false,
		}
	}
}

impl MeterLevels {
	/// Advances the ballistics by `dt` seconds with a new reading.
	pub fn update(&mut self, reading: MeterReading, dt: f32) {
		let fall = FALL_DB_PER_S * dt;
		for c in 0..2 {
			let db = amp_to_db(reading.peak[c]);
			self.peak_db[c] = db.max(self.peak_db[c] - fall).max(FLOOR_DB);
			self.rms_db[c] = amp_to_db(reading.rms[c]);
		}
		let top = self.peak_db[0].max(self.peak_db[1]);
		if top >= self.hold_db {
			self.hold_db = top;
			self.hold_age = 0.0;
		} else {
			self.hold_age += dt;
			if self.hold_age > HOLD_S {
				self.hold_db = (self.hold_db - fall).max(top);
			}
		}
		if reading.max_peak() >= 1.0 {
			self.clipped = true;
		}
	}

	/// Whether anything above the floor is still shown (keeps repainting while decaying).
	pub fn is_active(&self) -> bool {
		self.hold_db > FLOOR_DB || self.peak_db.iter().any(|&d| d > FLOOR_DB)
	}
}

/// Ballistics for every strip; `None` = master.
#[derive(Debug, Default)]
pub struct Meters {
	strips: HashMap<Option<TrackId>, MeterLevels>,
}

impl Meters {
	/// Reads the bank (if any) for `tracks` and the master, and advances the ballistics.
	pub fn update(
		&mut self,
		bank: Option<&MeterBank>,
		tracks: impl IntoIterator<Item = TrackId>,
		dt: f32,
	) {
		let silent = MeterReading::default();
		for id in tracks {
			let reading = bank.and_then(|b| b.track(id)).unwrap_or(silent);
			self.strips.entry(Some(id)).or_default().update(reading, dt);
		}
		let master = bank.map_or(silent, MeterBank::master);
		self.strips.entry(None).or_default().update(master, dt);
	}

	pub fn get(&self, strip: Option<TrackId>) -> MeterLevels {
		self.strips.get(&strip).copied().unwrap_or_default()
	}

	pub fn reset_clip(&mut self, strip: Option<TrackId>) {
		if let Some(m) = self.strips.get_mut(&strip) {
			m.clipped = false;
		}
	}

	pub fn any_active(&self) -> bool {
		self.strips.values().any(MeterLevels::is_active)
	}

	/// Drops strips of deleted tracks.
	pub fn retain(&mut self, keep: impl Fn(TrackId) -> bool) {
		self.strips.retain(|k, _| k.is_none_or(&keep));
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn reading(peak: f32) -> MeterReading {
		MeterReading {
			peak: [peak, peak * 0.5],
			rms: [peak * 0.7, peak * 0.35],
		}
	}

	#[test]
	fn peaks_fall_and_hold() {
		let mut m = MeterLevels::default();
		m.update(reading(0.5), 0.016);
		let db = amp_to_db(0.5);
		assert!((m.peak_db[0] - db).abs() < 1e-4);
		assert_eq!(m.hold_db, m.peak_db[0]);
		// Silence: the bar falls at 24 dB/s, the hold line stays for 1.5 s.
		m.update(reading(0.0), 0.5);
		assert!((m.peak_db[0] - (db - 12.0)).abs() < 1e-3);
		assert!((m.hold_db - db).abs() < 1e-4);
		assert_eq!(m.rms_db, [FLOOR_DB; 2], "RMS follows the engine directly");
		for _ in 0..10 {
			m.update(reading(0.0), 0.5);
		}
		assert_eq!(m.peak_db[0], FLOOR_DB);
		assert!(m.hold_db < db - 1.0, "hold line falls after its hold time");
		for _ in 0..20 {
			m.update(reading(0.0), 0.5);
		}
		assert!(!m.is_active());
	}

	#[test]
	fn clip_latches_until_reset() {
		let mut meters = Meters::default();
		let id = TrackId::new();
		meters.update(None, [id], 0.016);
		assert!(!meters.get(Some(id)).clipped);
		meters
			.strips
			.get_mut(&None)
			.unwrap()
			.update(reading(1.2), 0.016);
		assert!(meters.get(None).clipped);
		meters.update(None, [id], 0.016);
		assert!(meters.get(None).clipped, "latched");
		meters.reset_clip(None);
		assert!(!meters.get(None).clipped);
		meters.retain(|_| false);
		assert_eq!(meters.strips.len(), 1, "master is kept");
	}
}
