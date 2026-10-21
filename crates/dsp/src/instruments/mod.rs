//! Built-in instruments and shared voice management.

pub mod drum_machine;
pub mod drum_sampler;
pub mod electric_piano;
mod percussion;
pub mod poly_synth;
pub mod sample_voice;
pub mod sampler;
pub mod voice_readings;

/// Bookkeeping every polyphonic voice carries for allocation/stealing.
#[derive(Debug, Clone, Copy, Default)]
pub struct VoiceSlot {
	pub note: u8,
	pub active: bool,
	/// Key released (voice is in its release tail).
	pub released: bool,
	/// Monotonic start stamp; lower = older.
	pub started: u64,
	/// Key released while the sustain pedal is down: keeps sounding until the pedal
	/// goes up.
	pub pedal: bool,
	/// Samples since the note-on, or since the release once released (readings).
	pub elapsed: u32,
}

impl VoiceSlot {
	/// The key is released now: the release tail starts.
	pub fn release(&mut self) {
		if !self.released {
			self.released = true;
			self.elapsed = 0;
		}
	}

	/// Counts `samples` rendered.
	pub fn advance(&mut self, samples: usize) {
		self.elapsed = self.elapsed.saturating_add(samples as u32);
	}

	/// This voice as a reading (`level` = its amp envelope).
	pub fn reading(&self, level: f32, sample_rate: f32) -> voice_readings::VoiceReading {
		voice_readings::VoiceReading {
			note: self.note,
			zone: None,
			position: None,
			released: self.released,
			elapsed_ms: voice_readings::samples_to_ms(self.elapsed, sample_rate),
			level,
		}
	}

	/// A key went up: `true` when the voice should release now, `false` when the
	/// sustain pedal holds it (marked for [`VoiceSlot::pedal_up`]).
	pub fn key_up(&mut self, sustain: bool) -> bool {
		if sustain {
			self.pedal = true;
			false
		} else {
			true
		}
	}

	/// The pedal went up: `true` when this voice was held by it and should release now.
	pub fn pedal_up(&mut self) -> bool {
		let held = self.active && !self.released && self.pedal;
		self.pedal = false;
		held
	}
}

/// Chooses which voice should play `note`:
/// 1. a voice already sounding the same note (retrigger, no doubled notes),
/// 2. an idle voice,
/// 3. the oldest released voice,
/// 4. the oldest held voice (steal).
pub fn pick_voice<'a>(slots: impl Iterator<Item = &'a VoiceSlot>, note: u8) -> usize {
	let mut idle = None;
	let mut oldest_released: Option<(usize, u64)> = None;
	let mut oldest: Option<(usize, u64)> = None;
	for (i, s) in slots.enumerate() {
		if s.active && s.note == note {
			return i;
		}
		if !s.active {
			idle.get_or_insert(i);
			continue;
		}
		if s.released && oldest_released.is_none_or(|(_, t)| s.started < t) {
			oldest_released = Some((i, s.started));
		}
		if oldest.is_none_or(|(_, t)| s.started < t) {
			oldest = Some((i, s.started));
		}
	}
	idle.or(oldest_released.map(|(i, _)| i))
		.or(oldest.map(|(i, _)| i))
		.unwrap_or(0)
}

#[cfg(test)]
mod tests {
	use super::*;

	fn slot(note: u8, active: bool, released: bool, started: u64) -> VoiceSlot {
		VoiceSlot {
			note,
			active,
			released,
			started,
			pedal: false,
			elapsed: 0,
		}
	}

	#[test]
	fn allocation_priorities() {
		let slots = [
			slot(60, true, false, 1),
			slot(62, true, true, 5),
			slot(0, false, false, 0),
		];
		assert_eq!(pick_voice(slots.iter(), 60), 0);
		assert_eq!(pick_voice(slots.iter(), 64), 2);
		let full = [
			slot(60, true, false, 1),
			slot(62, true, true, 5),
			slot(63, true, true, 3),
		];
		assert_eq!(pick_voice(full.iter(), 64), 2);
		let held = [slot(60, true, false, 7), slot(62, true, false, 2)];
		assert_eq!(pick_voice(held.iter(), 64), 1);
	}
}
