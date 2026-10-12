//! MIDI recording helper: off the audio thread, turns a stream of timestamped incoming
//! messages into the notes of a new clip.

use std::collections::{BTreeMap, VecDeque};

use cadenza_core::{TempoMap, Tick};
use cadenza_model::Note;

use crate::ids::IdAllocator;
use crate::raw::RawMessage;

struct ClosedNote {
	pitch: u8,
	velocity: f32,
	start: Tick,
	length: Tick,
}

/// Accumulates recorded MIDI note events (pairing note-on/note-off, including
/// note-on velocity 0) and turns them into [`Note`]s for a new clip once recording
/// stops. Not realtime-safe by design: it's meant to run off the audio thread, fed by
/// whatever drains the hardware-input queue.
pub struct Recorder {
	tempo: TempoMap,
	/// Snaps note starts to this grid if set (e.g. `Tick(PPQ / 4)` for 16th notes).
	quantize: Option<Tick>,
	open: BTreeMap<(u8, u8), VecDeque<(Tick, f32)>>,
	closed: Vec<ClosedNote>,
}

impl Recorder {
	pub fn new(tempo: TempoMap, quantize: Option<Tick>) -> Self {
		Self {
			tempo,
			quantize,
			open: BTreeMap::new(),
			closed: Vec::new(),
		}
	}

	/// Feeds one incoming message, `seconds_since_start` after recording started.
	pub fn push(&mut self, seconds_since_start: f64, message: &RawMessage) {
		let channel = match message.channel() {
			Some(c) => c,
			None => return,
		};
		let tick = self.tempo.seconds_to_tick(seconds_since_start);

		if message.is_note_on() && message.bytes[2] > 0 {
			let pitch = message.bytes[1];
			let velocity = message.bytes[2] as f32 / 127.0;
			self.open
				.entry((channel, pitch))
				.or_default()
				.push_back((tick, velocity));
		} else if message.is_note_off_equivalent() {
			let pitch = message.bytes[1];
			if let Some(start_velocity) = self
				.open
				.get_mut(&(channel, pitch))
				.and_then(|q| q.pop_front())
			{
				let (start, velocity) = start_velocity;
				self.closed.push(ClosedNote {
					pitch,
					velocity,
					start,
					length: Tick((tick.0 - start.0).max(1)),
				});
			}
		}
	}

	/// Stops recording at `end_seconds`, closing any note still held down there, and
	/// returns the finished notes (sorted by start, then pitch) with fresh ids.
	pub fn finish(mut self, ids: &mut dyn IdAllocator, end_seconds: f64) -> Vec<Note> {
		let end_tick = self.tempo.seconds_to_tick(end_seconds);
		for ((_, pitch), queue) in self.open {
			for (start, velocity) in queue {
				self.closed.push(ClosedNote {
					pitch,
					velocity,
					start,
					length: Tick((end_tick.0 - start.0).max(1)),
				});
			}
		}

		if let Some(grid) = self.quantize {
			for note in &mut self.closed {
				note.start = quantize_tick(note.start, grid);
			}
		}

		self.closed.sort_by_key(|n| (n.start, n.pitch));
		self.closed
			.into_iter()
			.map(|n| Note {
				id: ids.note_id(),
				pitch: n.pitch,
				velocity: n.velocity,
				start: n.start,
				length: n.length,
			})
			.collect()
	}
}

fn quantize_tick(tick: Tick, grid: Tick) -> Tick {
	if grid.0 <= 0 {
		return tick;
	}
	Tick(((tick.0 as f64 / grid.0 as f64).round() * grid.0 as f64) as i64)
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::ids::RandomIds;

	fn note_on(pitch: u8, vel: u8) -> RawMessage {
		RawMessage::from_bytes(&[0x90, pitch, vel]).unwrap()
	}

	fn note_off(pitch: u8) -> RawMessage {
		RawMessage::from_bytes(&[0x80, pitch, 0]).unwrap()
	}

	#[test]
	fn pairs_note_on_and_off() {
		let mut recorder = Recorder::new(TempoMap::constant(120.0), None);
		recorder.push(0.0, &note_on(60, 100));
		recorder.push(0.5, &note_off(60));
		let notes = recorder.finish(&mut RandomIds, 0.5);
		assert_eq!(notes.len(), 1);
		assert_eq!(notes[0].pitch, 60);
		assert_eq!(notes[0].start, Tick::ZERO);
		assert_eq!(notes[0].length, Tick(cadenza_core::PPQ)); // 0.5s at 120bpm = 1 beat
	}

	#[test]
	fn note_on_velocity_zero_closes_the_note() {
		let mut recorder = Recorder::new(TempoMap::constant(120.0), None);
		recorder.push(0.0, &note_on(60, 100));
		recorder.push(0.5, &note_on(60, 0));
		let notes = recorder.finish(&mut RandomIds, 0.5);
		assert_eq!(notes.len(), 1);
	}

	#[test]
	fn open_notes_are_closed_at_stop() {
		let mut recorder = Recorder::new(TempoMap::constant(120.0), None);
		recorder.push(0.0, &note_on(60, 100));
		let notes = recorder.finish(&mut RandomIds, 1.0);
		assert_eq!(notes.len(), 1);
		assert_eq!(notes[0].length, Tick(cadenza_core::PPQ * 2));
	}

	#[test]
	fn quantize_snaps_start_to_grid() {
		let grid = Tick(cadenza_core::PPQ / 4);
		let mut recorder = Recorder::new(TempoMap::constant(120.0), Some(grid));
		// Slightly late note-on, should snap back to the grid.
		recorder.push(0.51, &note_on(60, 100));
		recorder.push(1.0, &note_off(60));
		let notes = recorder.finish(&mut RandomIds, 1.0);
		assert_eq!(notes[0].start, Tick(cadenza_core::PPQ));
	}
}
