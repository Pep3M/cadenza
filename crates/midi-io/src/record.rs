//! MIDI recording helper: off the audio thread, turns a stream of timestamped incoming
//! messages into the notes of a new clip.
//!
//! Messages are placed either by seconds since the recording started ([`Recorder::push`])
//! or directly on the song timeline ([`Recorder::push_at`], absolute ticks, as the engine
//! tags them). Loop recording closes held notes at the loop end
//! ([`Recorder::release_all`]) and collects each pass with [`Recorder::take_notes`].

use std::collections::{BTreeMap, VecDeque};

use cadenza_core::{TempoMap, Tick};
use cadenza_model::{Controller, ControllerLane, ControllerPoint, Note};

use crate::ids::IdAllocator;
use crate::raw::RawMessage;

/// A recorded note without an id yet (start in the recorder's time base).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RecordedNote {
	pub pitch: u8,
	pub velocity: f32,
	pub start: Tick,
	pub length: Tick,
}

type ClosedNote = RecordedNote;

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
	/// Controller events (CC, pitch bend, channel pressure) in arrival order.
	controls: Vec<RecordedControl>,
}

/// A recorded controller event (absolute tick, model controller, normalized value).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RecordedControl {
	pub tick: Tick,
	pub controller: Controller,
	pub value: f32,
}

impl Recorder {
	pub fn new(tempo: TempoMap, quantize: Option<Tick>) -> Self {
		Self {
			tempo,
			quantize,
			open: BTreeMap::new(),
			closed: Vec::new(),
			controls: Vec::new(),
		}
	}

	/// Feeds one incoming message, `seconds_since_start` after recording started.
	pub fn push(&mut self, seconds_since_start: f64, message: &RawMessage) {
		let tick = self.tempo.seconds_to_tick(seconds_since_start);
		self.push_at(tick, message);
	}

	/// Feeds one incoming message at `tick` (e.g. its song position).
	pub fn push_at(&mut self, tick: Tick, message: &RawMessage) {
		let channel = match message.channel() {
			Some(c) => c,
			None => return,
		};

		if let Some((controller, value)) = control(message) {
			self.controls.push(RecordedControl {
				tick,
				controller,
				value,
			});
			return;
		}
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
		self.release_all(end_tick);
		self.take_notes(ids)
	}

	/// Closes every note still held at `tick` (a loop end, the punch-out, stop).
	pub fn release_all(&mut self, tick: Tick) {
		for ((_, pitch), queue) in std::mem::take(&mut self.open) {
			for (start, velocity) in queue {
				self.closed.push(ClosedNote {
					pitch,
					velocity,
					start,
					length: Tick((tick.0 - start.0).max(1)),
				});
			}
		}
	}

	/// Whether any note is held down.
	pub fn has_held_notes(&self) -> bool {
		self.open.values().any(|q| !q.is_empty())
	}

	/// The notes finished so far (quantized, sorted by start then pitch, fresh ids);
	/// held notes stay open for later.
	pub fn take_notes(&mut self, ids: &mut dyn IdAllocator) -> Vec<Note> {
		let mut closed = std::mem::take(&mut self.closed);
		if let Some(grid) = self.quantize {
			for note in &mut closed {
				note.start = quantize_tick(note.start, grid);
			}
		}
		closed.sort_by_key(|n| (n.start, n.pitch));
		closed
			.into_iter()
			.map(|n| Note {
				id: ids.note_id(),
				pitch: n.pitch,
				velocity: n.velocity,
				start: n.start,
				length: n.length,
				bend: Vec::new(),
				pressure: Vec::new(),
				timbre: Vec::new(),
			})
			.collect()
	}

	/// The controller events recorded so far (sorted by tick, not quantized).
	pub fn take_controls(&mut self) -> Vec<RecordedControl> {
		let mut c = std::mem::take(&mut self.controls);
		c.sort_by_key(|e| e.tick);
		c
	}

	/// What has been played so far, for drawing while recording: finished notes plus
	/// held ones reaching `now` (not quantized).
	pub fn preview(&self, now: Tick) -> Vec<RecordedNote> {
		let mut out = self.closed.clone();
		for ((_, pitch), queue) in &self.open {
			for &(start, velocity) in queue {
				out.push(RecordedNote {
					pitch: *pitch,
					velocity,
					start,
					length: Tick((now.0 - start.0).max(1)),
				});
			}
		}
		out.sort_by_key(|n| (n.start, n.pitch));
		out
	}
}

/// The model controller and value of a CC / pitch bend / channel pressure message.
fn control(message: &RawMessage) -> Option<(Controller, f32)> {
	message.channel()?;
	let b = |i: usize| message.bytes[i] & 0x7f;
	match message.status() & 0xF0 {
		0xB0 if message.len == 3 && b(1) < 120 => {
			Some((Controller::Cc(b(1)), f32::from(b(2)) / 127.0))
		}
		0xE0 if message.len == 3 => {
			let v = i32::from(b(1)) | (i32::from(b(2)) << 7);
			let v = v - 8192;
			let norm = if v < 0 {
				v as f32 / 8192.0
			} else {
				v as f32 / 8191.0
			};
			Some((Controller::PitchBend, norm.clamp(-1.0, 1.0)))
		}
		0xD0 if message.len >= 2 => Some((Controller::ChannelPressure, f32::from(b(1)) / 127.0)),
		_ => None,
	}
}

/// Recorded controller events as clip lanes relative to `origin` (events before it are
/// dropped, one point per tick: the last wins).
pub fn control_lanes(events: &[RecordedControl], origin: Tick) -> Vec<ControllerLane> {
	let mut lanes: BTreeMap<Controller, Vec<ControllerPoint>> = BTreeMap::new();
	for e in events.iter().filter(|e| e.tick >= origin) {
		let points = lanes.entry(e.controller).or_default();
		let tick = e.tick - origin;
		match points.iter_mut().find(|p| p.tick == tick) {
			Some(p) => p.value = e.value,
			None => points.push(ControllerPoint::new(tick, e.value)),
		}
	}
	lanes
		.into_iter()
		.map(|(controller, mut points)| {
			points.sort_by_key(|p| p.tick);
			ControllerLane { controller, points }
		})
		.collect()
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
	fn loop_passes_close_held_notes_and_split_into_takes() {
		let mut recorder = Recorder::new(TempoMap::constant(120.0), None);
		let beat = cadenza_core::PPQ;
		recorder.push_at(Tick(beat), &note_on(60, 100));
		recorder.push_at(Tick(beat * 2), &note_off(60));
		recorder.push_at(Tick(beat * 3), &note_on(64, 64));
		assert!(recorder.has_held_notes());
		assert_eq!(recorder.preview(Tick(beat * 4)).len(), 2);
		// The loop wraps at beat 4: the held note ends there.
		recorder.release_all(Tick(beat * 4));
		let first = recorder.take_notes(&mut RandomIds);
		assert_eq!(first.len(), 2);
		assert_eq!(first[1].length, Tick(beat));
		recorder.push_at(Tick(beat / 2), &note_on(67, 127));
		let second = recorder.finish(&mut RandomIds, 1.0);
		assert_eq!(second.len(), 1);
		assert_eq!(second[0].pitch, 67);
		assert_eq!(second[0].length, Tick(beat * 2 - beat / 2));
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
