//! MIDI clip scheduling: clip-relative notes become absolute, sample-positioned events.
//!
//! Notes are cut at their clip's end. When playback starts (or seeks) in the middle of a
//! note, its regular note-on lies before the start position and is never replayed by
//! [`MidiSchedule::collect`] alone — only its (harmless) note-off would be. To avoid that
//! silence, [`MidiSchedule::chase`] finds notes whose span covers the new position and
//! re-triggers them with a note-on at offset 0; their already-scheduled note-off still
//! fires normally; so they sound for their remaining length instead of not at all.

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

/// A note's absolute timeline span, kept around purely for note-chasing (deciding what
/// should already be sounding at a given sample).
#[derive(Debug, Clone, Copy, PartialEq)]
struct NoteSpan {
	start: i64,
	end: i64,
	pitch: u8,
	velocity: f32,
}

#[derive(Debug, Default)]
pub(crate) struct MidiSchedule {
	events: Vec<ScheduledEvent>,
	cursor: usize,
	/// Sorted by `start`; used only by [`Self::chase`] and [`Self::max_chase_notes`].
	notes: Vec<NoteSpan>,
}

impl MidiSchedule {
	pub fn from_clips(clips: &[Clip], tempo: &TempoMap, sample_rate: f64) -> Self {
		let mut events = Vec::new();
		let mut spans = Vec::new();
		for clip in clips.iter().filter(|c| c.plays()) {
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
				let pitch = note.pitch.min(127);
				let velocity = note.velocity.clamp(0.0, 1.0);
				events.push(ScheduledEvent {
					sample: on,
					kind: EventKind::NoteOn { pitch, velocity },
				});
				events.push(ScheduledEvent {
					sample: off,
					kind: EventKind::NoteOff { pitch },
				});
				spans.push(NoteSpan {
					start: on,
					end: off,
					pitch,
					velocity,
				});
			}
		}
		events.sort_by_key(ScheduledEvent::sort_key);
		spans.sort_by_key(|s| s.start);
		Self {
			events,
			cursor: 0,
			notes: spans,
		}
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

	/// Largest number of notes that can be simultaneously sounding at any instant, i.e. the
	/// worst case for [`Self::chase`]'s output, so its caller can size its buffer once.
	pub fn max_chase_notes(&self) -> usize {
		let mut starts: Vec<i64> = self.notes.iter().map(|n| n.start).collect();
		let mut ends: Vec<i64> = self.notes.iter().map(|n| n.end).collect();
		starts.sort_unstable();
		ends.sort_unstable();
		let (mut i, mut j, mut current, mut best) = (0, 0, 0usize, 0usize);
		while i < starts.len() {
			if starts[i] < ends[j] {
				current += 1;
				i += 1;
				best = best.max(current);
			} else {
				current -= 1;
				j += 1;
			}
		}
		best
	}

	/// Appends a note-on at `offset` for every note that is already sounding at `sample`
	/// (started strictly before it and not yet ended), so a start/seek (or a loop wrap)
	/// landing inside a note re-triggers it instead of leaving it silent for its remaining
	/// length. Does not touch the cursor: call alongside [`Self::seek`], not instead of
	/// it. `self.notes` is sorted by `start`, so the scan can stop as soon as it passes
	/// `sample`; it never allocates.
	pub fn chase(&self, sample: i64, offset: u32, out: &mut Vec<MidiEvent>) {
		for n in &self.notes {
			if n.start >= sample {
				break;
			}
			if n.end > sample {
				if out.len() == out.capacity() {
					break;
				}
				out.push(MidiEvent::NoteOn {
					offset,
					pitch: n.pitch,
					velocity: n.velocity,
				});
			}
		}
	}

	/// Appends a note-off at `offset` for every note that started before `sample` and
	/// has not been released before it (its note-off is at or after `sample`): playback
	/// jumps away from `sample` (a loop wrap) and those offs would never be collected.
	/// Never allocates.
	pub fn release(&self, sample: i64, offset: u32, out: &mut Vec<MidiEvent>) {
		for n in &self.notes {
			if n.start >= sample {
				break;
			}
			if n.end >= sample {
				if out.len() == out.capacity() {
					break;
				}
				out.push(MidiEvent::NoteOff {
					offset,
					pitch: n.pitch,
				});
			}
		}
	}

	/// Pitches sounding at `sample` (bit per MIDI pitch), as in [`Self::chase`]. Never
	/// allocates.
	pub fn sounding(&self, sample: i64) -> [u64; 2] {
		let mut bits = [0u64; 2];
		for n in &self.notes {
			if n.start >= sample {
				break;
			}
			if n.end > sample {
				bits[usize::from(n.pitch >> 6) & 1] |= 1 << (n.pitch & 63);
			}
		}
		bits
	}

	/// Appends the events in `[start, start + len)` with offsets relative to `start` plus
	/// `base` (the segment's place in the quantum). Never grows `out` beyond its capacity.
	pub fn collect(&mut self, start: i64, len: usize, base: u32, out: &mut Vec<MidiEvent>) {
		let end = start + len as i64;
		while let Some(e) = self.events.get(self.cursor) {
			if e.sample >= end {
				break;
			}
			self.cursor += 1;
			if e.sample < start || out.len() == out.capacity() {
				continue;
			}
			let offset = base + (e.sample - start) as u32;
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
		s.collect(0, 32, 0, &mut out);
		assert_eq!(
			out,
			vec![MidiEvent::NoteOn {
				offset: 25,
				pitch: 60,
				velocity: 0.8
			}]
		);
		out.clear();
		s.collect(32, 32, 0, &mut out);
		assert_eq!(
			out,
			vec![MidiEvent::NoteOn {
				offset: 18,
				pitch: 64,
				velocity: 0.8
			}]
		);
		out.clear();
		s.collect(64, 32, 0, &mut out);
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
		s.collect(10_000, 48_000, 0, &mut out);
		assert_eq!(
			out,
			vec![MidiEvent::NoteOff {
				offset: 38_000,
				pitch: 60
			}]
		);
	}

	#[test]
	fn chase_retriggers_a_note_that_is_already_sounding() {
		let tempo = TempoMap::constant(120.0);
		let clip = clip_with(0, PPQ * 4, &[(60, 0, PPQ * 2)]);
		let s = MidiSchedule::from_clips(&[clip], &tempo, 48_000.0);
		let mut out = Vec::with_capacity(4);
		// Same position as `seek_into_a_note_skips_its_note_on`: `collect` alone would never
		// replay this note's note-on, so `chase` must supply it instead.
		s.chase(10_000, 0, &mut out);
		assert_eq!(
			out,
			vec![MidiEvent::NoteOn {
				offset: 0,
				pitch: 60,
				velocity: 0.8
			}]
		);
	}

	#[test]
	fn chase_skips_notes_that_start_exactly_at_or_after_the_position() {
		let tempo = TempoMap::constant(120.0);
		// Starts exactly at sample 0: `collect` will deliver its note-on normally, so
		// `chase` must not also fire one (that would double-trigger it).
		let clip = clip_with(0, PPQ * 4, &[(60, 0, PPQ * 2)]);
		let s = MidiSchedule::from_clips(&[clip], &tempo, 48_000.0);
		let mut out = Vec::with_capacity(4);
		s.chase(0, 0, &mut out);
		assert!(out.is_empty(), "{out:?}");
	}

	#[test]
	fn chase_skips_notes_that_have_already_ended() {
		let tempo = TempoMap::constant(120.0);
		let clip = clip_with(0, PPQ * 4, &[(60, 0, PPQ)]); // ends at sample 24_000
		let s = MidiSchedule::from_clips(&[clip], &tempo, 48_000.0);
		let mut out = Vec::with_capacity(4);
		s.chase(30_000, 0, &mut out);
		assert!(out.is_empty(), "{out:?}");
	}

	#[test]
	fn release_ends_every_note_still_held_at_a_jump() {
		let tempo = TempoMap::constant(120.0);
		// 1 beat = 24_000 samples. Ends exactly at the jump, crosses it, ended before,
		// starts at it.
		let clip = clip_with(
			0,
			PPQ * 4,
			&[
				(60, 0, PPQ),
				(62, 0, PPQ * 2),
				(64, 0, PPQ / 2),
				(67, PPQ, PPQ),
			],
		);
		let s = MidiSchedule::from_clips(&[clip], &tempo, 48_000.0);
		let mut out = Vec::with_capacity(8);
		s.release(24_000, 5, &mut out);
		assert_eq!(
			out,
			vec![
				MidiEvent::NoteOff {
					offset: 5,
					pitch: 60
				},
				MidiEvent::NoteOff {
					offset: 5,
					pitch: 62
				},
			]
		);
		let bits = s.sounding(24_000);
		assert_eq!(bits[0], 1 << 62);
		out.clear();
		let mut s = s;
		s.seek(24_000);
		s.collect(24_000, 32, 7, &mut out);
		assert_eq!(
			out,
			vec![
				MidiEvent::NoteOff {
					offset: 7,
					pitch: 60
				},
				MidiEvent::NoteOn {
					offset: 7,
					pitch: 67,
					velocity: 0.8
				},
			]
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
