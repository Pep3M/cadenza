//! MIDI clip scheduling: clip-relative notes become absolute, sample-positioned events.
//!
//! Notes are cut at their clip's end. When playback starts (or seeks) in the middle of a
//! note, its regular note-on lies before the start position and is never replayed by
//! [`MidiSchedule::collect`] alone — only its (harmless) note-off would be. To avoid that
//! silence, [`MidiSchedule::chase`] finds notes whose span covers the new position and
//! re-triggers them with a note-on at offset 0; their already-scheduled note-off still
//! fires normally; so they sound for their remaining length instead of not at all.

use cadenza_core::{TempoMap, Tick};
use cadenza_dsp::{MidiEvent, NoteExpressionKind};
use cadenza_model::{Clip, ClipContent, Controller, NoteExpression};

use crate::QUANTUM;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum EventKind {
	NoteOff {
		pitch: u8,
	},
	Control {
		controller: Controller,
		value: f32,
	},
	/// A note's own bend (before its note-on at its start, then at each point).
	NoteBend {
		pitch: u8,
		semitones: f32,
	},
	/// A note's own pressure or timbre, scheduled like its bend.
	NoteExpression {
		pitch: u8,
		kind: NoteExpressionKind,
		value: f32,
	},
	NoteOn {
		pitch: u8,
		velocity: f32,
	},
}

/// The realtime event of a controller value.
fn control_event(controller: Controller, value: f32, offset: u32) -> MidiEvent {
	match controller {
		Controller::Cc(n) => MidiEvent::ControlChange {
			offset,
			controller: n.min(127),
			value: value.clamp(0.0, 1.0),
		},
		Controller::PitchBend => MidiEvent::PitchBend {
			offset,
			value: value.clamp(-1.0, 1.0),
		},
		Controller::ChannelPressure => MidiEvent::ChannelPressure {
			offset,
			value: value.clamp(0.0, 1.0),
		},
	}
}

/// One controller's values on the timeline (all clips of the track), for chasing.
#[derive(Debug, Clone, PartialEq)]
struct ControlLane {
	controller: Controller,
	/// (absolute sample, value), sorted.
	points: Vec<(i64, f32)>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ScheduledEvent {
	/// Absolute timeline sample.
	pub sample: i64,
	pub kind: EventKind,
}

impl ScheduledEvent {
	/// Note-offs sort before controllers before note-ons at the same sample, so
	/// back-to-back notes retrigger and a pedal/bend applies to the notes starting with it.
	fn sort_key(&self) -> (i64, u8, u8) {
		match self.kind {
			EventKind::NoteOff { pitch } => (self.sample, 0, pitch),
			EventKind::Control { .. }
			| EventKind::NoteBend { .. }
			| EventKind::NoteExpression { .. } => (self.sample, 1, 0),
			EventKind::NoteOn { pitch, .. } => (self.sample, 2, pitch),
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
	/// Its bend, pressure and timbre points in [`MidiSchedule::curves`]: (first index,
	/// count) each.
	curves: [(u32, u32); 3],
}

/// What each of a note's [`NoteSpan::curves`] runs is.
const CURVE_KINDS: [Option<NoteExpressionKind>; 3] = [
	None,
	Some(NoteExpressionKind::Pressure),
	Some(NoteExpressionKind::Timbre),
];

fn model_kind(kind: NoteExpressionKind) -> NoteExpression {
	match kind {
		NoteExpressionKind::Pressure => NoteExpression::Pressure,
		NoteExpressionKind::Timbre => NoteExpression::Timbre,
	}
}

/// The realtime event of a note curve's value (`kind` None = its bend).
fn curve_event(kind: Option<NoteExpressionKind>, pitch: u8, value: f32, offset: u32) -> MidiEvent {
	match kind {
		None => MidiEvent::NoteBend {
			offset,
			pitch,
			semitones: value,
		},
		Some(kind) => MidiEvent::NoteExpression {
			offset,
			pitch,
			kind,
			value,
		},
	}
}

#[derive(Debug, Default)]
pub(crate) struct MidiSchedule {
	events: Vec<ScheduledEvent>,
	cursor: usize,
	/// Sorted by `start`; used only by [`Self::chase`] and [`Self::max_chase_notes`].
	notes: Vec<NoteSpan>,
	/// Controller values per controller, for chasing (sorted by controller).
	controls: Vec<ControlLane>,
	/// Every note's bend (semitones), pressure and timbre (0..1) points (absolute sample,
	/// value), each note's run sorted.
	curves: Vec<(i64, f32)>,
}

impl MidiSchedule {
	pub fn from_clips(clips: &[Clip], tempo: &TempoMap, sample_rate: f64) -> Self {
		let mut events = Vec::new();
		let mut spans = Vec::new();
		let mut controls: Vec<ControlLane> = Vec::new();
		let mut curves: Vec<(i64, f32)> = Vec::new();
		for clip in clips.iter().filter(|c| c.sounds()) {
			let ClipContent::Midi { notes, controllers } = &clip.content else {
				continue;
			};
			for lane in controllers {
				let i = match controls.binary_search_by_key(&lane.controller, |l| l.controller) {
					Ok(i) => i,
					Err(i) => {
						controls.insert(
							i,
							ControlLane {
								controller: lane.controller,
								points: Vec::new(),
							},
						);
						i
					}
				};
				let mut push = |sample: i64, value: f32| {
					events.push(ScheduledEvent {
						sample,
						kind: EventKind::Control {
							controller: lane.controller,
							value,
						},
					});
					controls[i].points.push((sample, value));
				};
				let mut last = None;
				for p in lane
					.points
					.iter()
					.filter(|p| p.tick.0 >= 0 && p.tick < clip.length)
				{
					push(
						tempo.tick_to_sample(clip.start + p.tick, sample_rate) as i64,
						p.value,
					);
					last = Some(p.value);
				}
				// A bend or a held pedal must not outlive its clip.
				let neutral = lane.controller.default_value();
				let stuck = match lane.controller {
					Controller::PitchBend => last.is_some_and(|v| v != 0.0),
					c if c.is_switch() => last.is_some_and(|v| v >= 0.5),
					_ => false,
				};
				if stuck {
					push(
						tempo.tick_to_sample(clip.end(), sample_rate) as i64,
						neutral,
					);
				}
			}
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
				// Each curve from the note-on (the value in effect there) to its end.
				let played = end - note.start;
				let mut runs = [(0u32, 0u32); 3];
				for (run, kind) in runs.iter_mut().zip(CURVE_KINDS) {
					let first = curves.len();
					let points: Vec<(Tick, f32)> = match kind {
						None => note.bend.iter().map(|p| (p.tick, p.semitones)).collect(),
						Some(kind) => note
							.expression(model_kind(kind))
							.iter()
							.map(|p| (p.tick, p.value))
							.collect(),
					};
					if points.is_empty() {
						*run = (first as u32, 0);
						continue;
					}
					let at_start = match kind {
						None => note.bend_at(Tick::ZERO),
						Some(kind) => note.expression_at(model_kind(kind), Tick::ZERO),
					};
					curves.push((on, at_start));
					for &(tick, value) in points.iter().filter(|p| p.0.0 > 0 && p.0 < played) {
						let at = (tempo.tick_to_sample(clip.start + note.start + tick, sample_rate)
							as i64)
							.clamp(on, off - 1);
						curves.push((at, value));
					}
					for &(sample, value) in &curves[first..] {
						events.push(ScheduledEvent {
							sample,
							kind: match kind {
								None => EventKind::NoteBend {
									pitch,
									semitones: value,
								},
								Some(kind) => EventKind::NoteExpression { pitch, kind, value },
							},
						});
					}
					*run = (first as u32, (curves.len() - first) as u32);
				}
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
					curves: runs,
				});
			}
		}
		events.sort_by_key(ScheduledEvent::sort_key);
		spans.sort_by_key(|s| s.start);
		for lane in &mut controls {
			lane.points.sort_by_key(|p| p.0);
		}
		Self {
			events,
			cursor: 0,
			notes: spans,
			controls,
			curves,
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

	/// Worst case of [`Self::chase`]'s output (controllers + notes sounding at once), so
	/// its caller can size its buffer once.
	pub fn max_chase_notes(&self) -> usize {
		// A note is chased with its curves' values before its note-on.
		let per_note = 1 + self
			.notes
			.iter()
			.map(|n| n.curves.iter().filter(|c| c.1 > 0).count())
			.max()
			.unwrap_or(0);
		self.controls.len() + per_note * self.max_sounding()
	}

	fn max_sounding(&self) -> usize {
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
		// Controllers first (their value before `sample`, or their resting value), so
		// re-triggered notes start bent / pedalled like they would have.
		for lane in &self.controls {
			if out.len() == out.capacity() {
				return;
			}
			let i = lane.points.partition_point(|p| p.0 < sample);
			let value = i
				.checked_sub(1)
				.map_or(lane.controller.default_value(), |i| lane.points[i].1);
			out.push(control_event(lane.controller, value, offset));
		}
		for n in &self.notes {
			if n.start >= sample {
				break;
			}
			if n.end > sample {
				for (&(first, count), kind) in n.curves.iter().zip(CURVE_KINDS) {
					let (first, count) = (first as usize, count as usize);
					if count == 0 || out.len() == out.capacity() {
						continue;
					}
					let run = &self.curves[first..first + count];
					let i = run.partition_point(|p| p.0 <= sample).max(1);
					out.push(curve_event(kind, n.pitch, run[i - 1].1, offset));
				}
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
				EventKind::Control { controller, value } => {
					control_event(controller, value, offset)
				}
				EventKind::NoteBend { pitch, semitones } => MidiEvent::NoteBend {
					offset,
					pitch,
					semitones,
				},
				EventKind::NoteExpression { pitch, kind, value } => {
					curve_event(Some(kind), pitch, value, offset)
				}
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

#[cfg(test)]
mod controller_tests {
	use super::*;
	use cadenza_core::{PPQ, Tick};
	use cadenza_model::{ControllerLane, ControllerPoint};

	#[test]
	fn controller_lanes_play_chase_and_reset_at_the_clip_end() {
		let tempo = TempoMap::constant(120.0);
		// One beat = 24_000 samples. Bend up at beat 1, sustain down at 0 (clip of 4 beats).
		let clip = Clip::midi("c", Tick(0), Tick(PPQ * 4)).with_controllers([
			ControllerLane {
				controller: Controller::PitchBend,
				points: vec![ControllerPoint::new(Tick(PPQ), 0.5)],
			},
			ControllerLane {
				controller: Controller::SUSTAIN,
				points: vec![ControllerPoint::new(Tick(0), 1.0)],
			},
		]);
		let mut s = MidiSchedule::from_clips(&[clip], &tempo, 48_000.0);
		let kinds: Vec<(i64, EventKind)> = s.events().iter().map(|e| (e.sample, e.kind)).collect();
		assert_eq!(
			kinds.len(),
			4,
			"two values + bend and pedal reset at the end"
		);
		assert_eq!(kinds[3].0, 96_000);
		let mut out = Vec::with_capacity(8);
		s.seek(0);
		s.collect(24_000, 32, 0, &mut out);
		assert_eq!(
			out,
			vec![MidiEvent::PitchBend {
				offset: 0,
				value: 0.5
			}]
		);
		// Starting at beat 2: the bend and the pedal are chased.
		out.clear();
		s.chase(48_000, 0, &mut out);
		assert!(out.contains(&MidiEvent::PitchBend {
			offset: 0,
			value: 0.5
		}));
		assert!(out.contains(&MidiEvent::ControlChange {
			offset: 0,
			controller: 64,
			value: 1.0
		}));
		// Before the first bend: the lane rests at center.
		out.clear();
		s.chase(10, 0, &mut out);
		assert!(out.contains(&MidiEvent::PitchBend {
			offset: 0,
			value: 0.0
		}));
		assert!(s.max_chase_notes() >= 2);
	}
}
