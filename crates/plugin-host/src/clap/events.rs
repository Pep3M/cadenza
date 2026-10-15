//! Real-time event plumbing: a fixed-capacity CLAP input event list and a
//! bounded parameter-change queue. Neither allocates after construction.

use cadenza_dsp::MidiEvent;
use clack_host::events::event_types::{
	MidiEvent as ClapMidiEvent, NoteExpressionEvent, NoteExpressionType, NoteOffEvent, NoteOnEvent,
	ParamValueEvent,
};
use clack_host::events::io::InputEventBuffer;
use clack_host::events::{Event, Match, Pckn, UnknownEvent};
use clack_host::utils::ClapId;

/// One event Cadenza can send to a plugin. Stored by value so the list is a
/// plain pre-allocated `Vec` with no per-event allocation.
#[derive(Clone, Copy)]
enum HostEvent {
	NoteOn(NoteOnEvent),
	NoteOff(NoteOffEvent),
	Midi(ClapMidiEvent),
	Expression(NoteExpressionEvent),
	Param(ParamValueEvent),
}

/// How controller events (CC, pitch bend, pressure) reach a plugin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ControllerRoute {
	/// Raw MIDI 1.0 bytes on the note port (it accepts the MIDI dialect).
	Midi,
	/// CLAP-only note port: pitch bend and pressure become per-note expressions of the
	/// held keys (tuning, pressure); other CCs have no CLAP equivalent and are dropped.
	Expressions,
}

/// Time-ordered input events for one `process` call.
pub(crate) struct EventList {
	events: Vec<HostEvent>,
	/// Keys sounding (CLAP note events), for per-note expressions.
	held: u128,
	/// Channel-wide pitch bend (semitones) and pressure applied to new notes too.
	tuning: f64,
	pressure: f64,
}

/// Pitch bend range used when bends become CLAP tuning expressions (semitones).
const BEND_RANGE: f64 = 2.0;

impl EventList {
	pub fn with_capacity(capacity: usize) -> Self {
		Self {
			events: Vec::with_capacity(capacity),
			held: 0,
			tuning: 0.0,
			pressure: 0.0,
		}
	}

	/// Per-note expression for every held key (CLAP-only plugins).
	fn expression_all(&mut self, time: u32, kind: NoteExpressionType, value: f64) -> bool {
		let mut held = self.held;
		let mut ok = true;
		while held != 0 {
			let key = held.trailing_zeros() as u16;
			held &= held - 1;
			ok &= self.push(HostEvent::Expression(NoteExpressionEvent::new(
				time,
				Pckn::new(0u16, 0u16, key, Match::All),
				kind,
				value,
			)));
		}
		ok
	}

	/// Converts a controller event (CC / pitch bend / pressure).
	pub fn push_controller(&mut self, e: &MidiEvent, frames: u32, route: ControllerRoute) -> bool {
		let time = e.offset().min(frames.saturating_sub(1));
		let midi7 = |v: f32| (v.clamp(0.0, 1.0) * 127.0).round() as u8;
		match (route, *e) {
			(
				ControllerRoute::Midi,
				MidiEvent::ControlChange {
					controller, value, ..
				},
			) => self.push(HostEvent::Midi(ClapMidiEvent::new(
				time,
				0,
				[0xB0, controller.min(127), midi7(value)],
			))),
			(ControllerRoute::Midi, MidiEvent::PitchBend { value, .. }) => {
				let v = ((value.clamp(-1.0, 1.0) + 1.0) * 8191.5)
					.round()
					.clamp(0.0, 16383.0) as u16;
				self.push(HostEvent::Midi(ClapMidiEvent::new(
					time,
					0,
					[0xE0, (v & 0x7f) as u8, (v >> 7) as u8],
				)))
			}
			(ControllerRoute::Midi, MidiEvent::ChannelPressure { value, .. }) => self.push(
				HostEvent::Midi(ClapMidiEvent::new(time, 0, [0xD0, midi7(value), 0])),
			),
			(ControllerRoute::Expressions, MidiEvent::PitchBend { value, .. }) => {
				self.tuning = f64::from(value.clamp(-1.0, 1.0)) * BEND_RANGE;
				self.expression_all(time, NoteExpressionType::Tuning, self.tuning)
			}
			(ControllerRoute::Expressions, MidiEvent::ChannelPressure { value, .. }) => {
				self.pressure = f64::from(value.clamp(0.0, 1.0));
				self.expression_all(time, NoteExpressionType::Pressure, self.pressure)
			}
			_ => true,
		}
	}

	pub fn clear(&mut self) {
		self.events.clear();
	}

	fn push(&mut self, e: HostEvent) -> bool {
		// Never grow on the audio thread: drop events beyond capacity.
		if self.events.len() < self.events.capacity() {
			self.events.push(e);
			true
		} else {
			false
		}
	}

	pub fn push_param(&mut self, time: u32, id: ClapId, value: f64) -> bool {
		self.push(HostEvent::Param(ParamValueEvent::new(
			time,
			id,
			Pckn::match_all(),
			value,
		)))
	}

	/// Converts a Cadenza MIDI event (offset clamped to `frames - 1`). With
	/// `midi_dialect` the plugin gets raw MIDI 1.0 bytes on port 0 channel 1;
	/// otherwise CLAP note events. "All notes off" becomes one note-off per key
	/// (CLAP) or CC 123 (MIDI): the spec-valid wildcard key (-1) makes some plugins
	/// (Surge XT) index out of bounds and abort.
	#[cfg(test)]
	pub fn push_midi(&mut self, e: &MidiEvent, frames: u32, midi_dialect: bool) -> bool {
		self.push_event(e, frames, midi_dialect, ControllerRoute::Midi)
	}

	/// Converts any Cadenza event: notes as [`EventList::push_midi`], controllers by
	/// `controllers`.
	pub fn push_event(
		&mut self,
		e: &MidiEvent,
		frames: u32,
		midi_dialect: bool,
		controllers: ControllerRoute,
	) -> bool {
		if !e.is_note() {
			return self.push_controller(e, frames, controllers);
		}
		let time = e.offset().min(frames.saturating_sub(1));
		if let (MidiEvent::AllNotesOff { .. }, false) = (*e, midi_dialect) {
			self.held = 0;
			return (0u16..128).all(|key| {
				self.push(HostEvent::NoteOff(NoteOffEvent::new(
					time,
					Pckn::new(0u16, 0u16, key, Match::All),
					0.0,
				)))
			});
		}
		if !midi_dialect {
			match *e {
				MidiEvent::NoteOn { pitch, .. } => self.held |= 1u128 << pitch.min(127),
				MidiEvent::NoteOff { pitch, .. } => self.held &= !(1u128 << pitch.min(127)),
				_ => {}
			}
		}
		let event = match (*e, midi_dialect) {
			(
				MidiEvent::NoteOn {
					pitch, velocity, ..
				},
				false,
			) => HostEvent::NoteOn(NoteOnEvent::new(
				time,
				Pckn::new(0u16, 0u16, u16::from(pitch.min(127)), Match::All),
				f64::from(velocity.clamp(0.0, 1.0)),
			)),
			(MidiEvent::NoteOff { pitch, .. }, false) => HostEvent::NoteOff(NoteOffEvent::new(
				time,
				Pckn::new(0u16, 0u16, u16::from(pitch.min(127)), Match::All),
				0.0,
			)),
			(MidiEvent::AllNotesOff { .. }, false) => unreachable!("expanded per key above"),
			(
				MidiEvent::NoteOn {
					pitch, velocity, ..
				},
				true,
			) => {
				let v = (velocity.clamp(0.0, 1.0) * 127.0).round().max(1.0) as u8;
				HostEvent::Midi(ClapMidiEvent::new(time, 0, [0x90, pitch.min(127), v]))
			}
			(MidiEvent::NoteOff { pitch, .. }, true) => {
				HostEvent::Midi(ClapMidiEvent::new(time, 0, [0x80, pitch.min(127), 0]))
			}
			(MidiEvent::AllNotesOff { .. }, true) => {
				HostEvent::Midi(ClapMidiEvent::new(time, 0, [0xB0, 123, 0]))
			}
			_ => return true,
		};
		let ok = self.push(event);
		// A note starting while the channel is bent / pressed gets the same expression.
		if let (MidiEvent::NoteOn { pitch, .. }, false) = (*e, midi_dialect)
			&& controllers == ControllerRoute::Expressions
		{
			let pckn = Pckn::new(0u16, 0u16, u16::from(pitch.min(127)), Match::All);
			if self.tuning != 0.0 {
				self.push(HostEvent::Expression(NoteExpressionEvent::new(
					time,
					pckn,
					NoteExpressionType::Tuning,
					self.tuning,
				)));
			}
			if self.pressure != 0.0 {
				self.push(HostEvent::Expression(NoteExpressionEvent::new(
					time,
					pckn,
					NoteExpressionType::Pressure,
					self.pressure,
				)));
			}
		}
		ok
	}

	#[cfg(test)]
	pub fn len(&self) -> usize {
		self.events.len()
	}
}

impl InputEventBuffer for EventList {
	fn len(&self) -> u32 {
		u32::try_from(self.events.len()).unwrap_or(u32::MAX)
	}

	fn get(&self, index: u32) -> Option<&UnknownEvent> {
		Some(match self.events.get(index as usize)? {
			HostEvent::NoteOn(e) => e.as_unknown(),
			HostEvent::NoteOff(e) => e.as_unknown(),
			HostEvent::Midi(e) => e.as_unknown(),
			HostEvent::Expression(e) => e.as_unknown(),
			HostEvent::Param(e) => e.as_unknown(),
		})
	}
}

/// Latest-value-wins queue of parameter changes, one slot per parameter.
/// `set` is O(1) and the queue can never overflow, so automation is never lost
/// (only superseded by a newer value for the same parameter).
pub(crate) struct ParamQueue {
	pending: Vec<f64>,
	dirty: Vec<u32>,
}

impl ParamQueue {
	pub fn new(count: usize) -> Self {
		Self {
			pending: vec![f64::NAN; count],
			dirty: Vec::with_capacity(count),
		}
	}

	pub fn set(&mut self, index: usize, value: f64) {
		let Some(slot) = self.pending.get_mut(index) else {
			return;
		};
		if slot.is_nan() {
			// `dirty` has capacity for every parameter and each index appears at
			// most once, so this push never reallocates.
			self.dirty.push(index as u32);
		}
		*slot = value;
	}

	pub fn is_empty(&self) -> bool {
		self.dirty.is_empty()
	}

	/// Calls `f(index, value)` for every pending change, then empties the queue.
	pub fn drain(&mut self, mut f: impl FnMut(usize, f64)) {
		for &i in &self.dirty {
			let i = i as usize;
			f(i, self.pending[i]);
			self.pending[i] = f64::NAN;
		}
		self.dirty.clear();
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use clack_host::events::io::InputEvents;
	use clack_host::events::spaces::CoreEventSpace;

	#[test]
	fn param_queue_keeps_latest_value_once() {
		let mut q = ParamQueue::new(3);
		q.set(1, 0.5);
		q.set(1, 0.7);
		q.set(0, 1.0);
		q.set(9, 1.0);
		let mut seen = Vec::new();
		q.drain(|i, v| seen.push((i, v)));
		assert_eq!(seen, vec![(1, 0.7), (0, 1.0)]);
		assert!(q.is_empty());
	}

	#[test]
	fn event_list_converts_and_bounds() {
		let mut list = EventList::with_capacity(2);
		assert!(list.push_midi(
			&MidiEvent::NoteOn {
				offset: 99,
				pitch: 60,
				velocity: 0.5
			},
			16,
			false
		));
		assert!(list.push_param(0, ClapId::new(3), 0.25));
		assert!(!list.push_midi(&MidiEvent::AllNotesOff { offset: 0 }, 16, false));
		assert_eq!(list.len(), 2);
		let input = InputEvents::from_buffer(&list);
		match input.get(0).unwrap().as_core_event() {
			Some(CoreEventSpace::NoteOn(e)) => {
				assert_eq!(e.header().time(), 15);
				assert_eq!(e.key(), Match::Specific(60));
				assert_eq!(e.velocity(), 0.5);
			}
			_ => panic!("expected note on"),
		}
		match input.get(1).unwrap().as_core_event() {
			Some(CoreEventSpace::ParamValue(e)) => {
				assert_eq!(e.param_id(), Some(ClapId::new(3)));
				assert_eq!(e.value(), 0.25);
			}
			_ => panic!("expected param"),
		}
	}

	#[test]
	fn midi_dialect_bytes() {
		let mut list = EventList::with_capacity(4);
		list.push_midi(
			&MidiEvent::NoteOn {
				offset: 0,
				pitch: 64,
				velocity: 1.0,
			},
			8,
			true,
		);
		let input = InputEvents::from_buffer(&list);
		match input.get(0).unwrap().as_core_event() {
			Some(CoreEventSpace::Midi(e)) => assert_eq!(e.data(), [0x90, 64, 127]),
			_ => panic!("expected midi"),
		}
	}

	#[test]
	fn controllers_as_midi_bytes_or_note_expressions() {
		let mut list = EventList::with_capacity(8);
		let cc = MidiEvent::ControlChange {
			offset: 0,
			controller: 64,
			value: 1.0,
		};
		let bend = MidiEvent::PitchBend {
			offset: 1,
			value: 1.0,
		};
		list.push_event(&cc, 8, false, ControllerRoute::Midi);
		list.push_event(&bend, 8, false, ControllerRoute::Midi);
		{
			let input = InputEvents::from_buffer(&list);
			match input.get(0).unwrap().as_core_event() {
				Some(CoreEventSpace::Midi(e)) => assert_eq!(e.data(), [0xB0, 64, 127]),
				_ => panic!("expected a CC"),
			}
			match input.get(1).unwrap().as_core_event() {
				Some(CoreEventSpace::Midi(e)) => assert_eq!(e.data(), [0xE0, 0x7f, 0x7f]),
				_ => panic!("expected a pitch bend"),
			}
		}
		// CLAP-only: a bend tunes the held keys (and notes started while bent).
		let mut list = EventList::with_capacity(8);
		let on = |pitch| MidiEvent::NoteOn {
			offset: 0,
			pitch,
			velocity: 1.0,
		};
		list.push_event(&on(60), 8, false, ControllerRoute::Expressions);
		list.push_event(&bend, 8, false, ControllerRoute::Expressions);
		list.push_event(&cc, 8, false, ControllerRoute::Expressions);
		list.push_event(&on(64), 8, false, ControllerRoute::Expressions);
		assert_eq!(list.len(), 4, "note, tuning, (CC dropped), note, tuning");
		let input = InputEvents::from_buffer(&list);
		match input.get(1).unwrap().as_core_event() {
			Some(CoreEventSpace::NoteExpression(e)) => {
				assert_eq!(e.key(), Match::Specific(60));
				assert_eq!(e.value(), 2.0);
			}
			_ => panic!("expected a tuning expression"),
		}
		match input.get(3).unwrap().as_core_event() {
			Some(CoreEventSpace::NoteExpression(e)) => assert_eq!(e.key(), Match::Specific(64)),
			_ => panic!("expected a tuning expression for the new note"),
		}
	}
}
