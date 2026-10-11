//! MIDI clip scheduling: clip-relative notes become absolute, sample-positioned events.
//!
//! Notes are cut at their clip's end. When playback starts (or seeks) in the middle of a
//! note, that note stays silent: its note-on lies before the start position and is never
//! replayed; only its (harmless) note-off is delivered.

use cadenza_core::TempoMap;
use cadenza_dsp::MidiEvent;
use cadenza_model::{Clip, ClipContent};

use crate::QUANTUM;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum EventKind {
	NoteOff { pitch: u8 },
	NoteOn { pitch: u8, velocity: f32 },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ScheduledEvent {
	/// Absolute timeline sample.
	pub sample: i64,
	pub kind: EventKind,
}

impl ScheduledEvent {
	/// Note-offs sort before note-ons at the same sample so back-to-back notes retrigger.
	fn sort_key(&self) -> (i64, u8, u8) {
		match self.kind {
			EventKind::NoteOff { pitch } => (self.sample, 0, pitch),
			EventKind::NoteOn { pitch, .. } => (self.sample, 1, pitch),
		}
	}
}

#[derive(Debug, Default)]
pub(crate) struct MidiSchedule {
	events: Vec<ScheduledEvent>,
	cursor: usize,
}

impl MidiSchedule {
	pub fn from_clips(clips: &[Clip], tempo: &TempoMap, sample_rate: f64) -> Self {
		let mut events = Vec::new();
		for clip in clips {
			let ClipContent::Midi { notes } = &clip.content else {
				continue;
			};
			for note in notes {
				if note.start.0 < 0 || note.start >= clip.length {
					continue;
				}
				let end = note.end().min(clip.length);
				if end <= note.start {
					continue;
				}
				let on = tempo.tick_to_sample(clip.start + note.start, sample_rate) as i64;
				let off = (tempo.tick_to_sample(clip.start + end, sample_rate) as i64).max(on + 1);
				events.push(ScheduledEvent {
					sample: on,
					kind: EventKind::NoteOn {
						pitch: note.pitch.min(127),
						velocity: note.velocity.clamp(0.0, 1.0),
					},
				});
				events.push(ScheduledEvent {
					sample: off,
					kind: EventKind::NoteOff {
						pitch: note.pitch.min(127),
					},
				});
			}
		}
		events.sort_by_key(ScheduledEvent::sort_key);
		Self { events, cursor: 0 }
	}

	#[cfg(test)]
	pub fn events(&self) -> &[ScheduledEvent] {
		&self.events
	}

	/// Most events that can fall inside one quantum, so the per-block event buffer can be
	/// allocated once and never grow on the audio thread.
	pub fn max_events_per_quantum(&self) -> usize {
		let mut best = 0;
		let mut lo = 0;
		for hi in 0..self.events.len() {
			while self.events[hi].sample - self.events[lo].sample >= QUANTUM as i64 {
				lo += 1;
			}
			best = best.max(hi - lo + 1);
		}
		best
	}

	/// Positions the cursor at the first event at or after `sample`.
	pub fn seek(&mut self, sample: i64) {
		self.cursor = self.events.partition_point(|e| e.sample < sample);
	}

	/// Appends the events in `[start, start + len)` with block-relative offsets. Never
	/// grows `out` beyond its capacity.
	pub fn collect(&mut self, start: i64, len: usize, out: &mut Vec<MidiEvent>) {
		let end = start + len as i64;
		while let Some(e) = self.events.get(self.cursor) {
			if e.sample >= end {
				break;
			}
			self.cursor += 1;
			if e.sample < start || out.len() == out.capacity() {
				continue;
			}
			let offset = (e.sample - start) as u32;
			out.push(match e.kind {
				EventKind::NoteOn { pitch, velocity } => MidiEvent::NoteOn {
					offset,
					pitch,
					velocity,
				},
				EventKind::NoteOff { pitch } => MidiEvent::NoteOff { offset, pitch },
			});
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use cadenza_core::{PPQ, Tick};
	use cadenza_model::Note;

	fn clip_with(start: i64, length: i64, notes: &[(u8, i64, i64)]) -> Clip {
		Clip::midi("c", Tick(start), Tick(length)).with_notes(
			notes
				.iter()
				.map(|&(p, s, l)| Note::new(p, Tick(s), Tick(l), 0.8)),
		)
	}

	#[test]
	fn notes_cut_at_clip_end_and_sorted() {
		let tempo = TempoMap::constant(120.0);
		let clip = clip_with(PPQ, PPQ * 2, &[(60, 0, PPQ * 4), (62, PPQ * 3, PPQ)]);
		let s = MidiSchedule::from_clips(&[clip], &tempo, 48_000.0);
		// Second note starts after the clip end: dropped. First is cut at 3 beats absolute.
		assert_eq!(s.events().len(), 2);
		assert_eq!(s.events()[0].sample, 24_000);
		assert_eq!(s.events()[1].sample, 72_000);
	}

	#[test]
	fn collect_splits_across_blocks_with_offsets() {
		let tempo = TempoMap::constant(120.0);
		// 1 tick at 120 bpm / 48 kHz = 25 samples.
		let clip = clip_with(0, PPQ, &[(60, 1, 2), (64, 2, 1)]);
		let mut s = MidiSchedule::from_clips(&[clip], &tempo, 48_000.0);
		let mut out = Vec::with_capacity(8);
		s.seek(0);
		s.collect(0, 32, &mut out);
		assert_eq!(
			out,
			vec![MidiEvent::NoteOn {
				offset: 25,
				pitch: 60,
				velocity: 0.8
			}]
		);
		out.clear();
		s.collect(32, 32, &mut out);
		assert_eq!(
			out,
			vec![MidiEvent::NoteOn {
				offset: 18,
				pitch: 64,
				velocity: 0.8
			}]
		);
		out.clear();
		s.collect(64, 32, &mut out);
		assert_eq!(
			out,
			vec![
				MidiEvent::NoteOff {
					offset: 11,
					pitch: 60
				},
				MidiEvent::NoteOff {
					offset: 11,
					pitch: 64
				},
			]
		);
	}

	#[test]
	fn seek_into_a_note_skips_its_note_on() {
		let tempo = TempoMap::constant(120.0);
		let clip = clip_with(0, PPQ * 4, &[(60, 0, PPQ * 2)]);
		let mut s = MidiSchedule::from_clips(&[clip], &tempo, 48_000.0);
		s.seek(10_000);
		let mut out = Vec::with_capacity(4);
		s.collect(10_000, 48_000, &mut out);
		assert_eq!(
			out,
			vec![MidiEvent::NoteOff {
				offset: 38_000,
				pitch: 60
			}]
		);
	}

	#[test]
	fn max_events_window() {
		let tempo = TempoMap::constant(120.0);
		let clip = clip_with(
			0,
			PPQ * 4,
			&[(60, 0, PPQ), (62, 0, PPQ), (64, PPQ * 2, PPQ)],
		);
		let s = MidiSchedule::from_clips(&[clip], &tempo, 48_000.0);
		assert_eq!(s.max_events_per_quantum(), 2);
	}
}
