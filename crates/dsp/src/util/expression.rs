//! Channel-wide performance controls shared by the built-in instruments: pitch bend,
//! mod wheel (vibrato), channel pressure and the sustain pedal, plus each key's own bend
//! ([`MidiEvent::NoteBend`]: a note's pitch curve).

use crate::processor::{MidiEvent, cc};

/// Default pitch bend range, semitones each way.
pub const BEND_RANGE: f32 = 2.0;
/// Vibrato rate and the depth the mod wheel reaches at full (semitones each way).
const VIBRATO_HZ: f32 = 5.5;
const VIBRATO_DEPTH: f32 = 0.5;

/// What an event changed that the instrument must act on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExpressionAction {
	None,
	/// The sustain pedal went up: release the notes it held.
	PedalUp,
	/// All notes off / all sound off by controller.
	AllNotesOff,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Expression {
	/// -1..=1.
	pub bend: f32,
	/// Mod wheel 0..=1.
	pub modulation: f32,
	/// Channel pressure 0..=1.
	pub pressure: f32,
	pub sustain: bool,
	pub bend_range: f32,
	phase: f32,
	/// Each key's own bend, semitones; kept through the release.
	note_bend: [f32; 128],
	/// A bend came for the key since its last note-off (its next note-on starts there).
	bend_set: [bool; 128],
}

impl Default for Expression {
	fn default() -> Self {
		Self {
			bend: 0.0,
			modulation: 0.0,
			pressure: 0.0,
			sustain: false,
			bend_range: BEND_RANGE,
			phase: 0.0,
			note_bend: [0.0; 128],
			bend_set: [false; 128],
		}
	}
}

impl Expression {
	/// Applies a controller event or a key's bend; note-ons and note-offs only update which
	/// key bends are fresh (call it for every event).
	pub fn apply(&mut self, ev: &MidiEvent) -> ExpressionAction {
		match *ev {
			MidiEvent::NoteBend {
				pitch, semitones, ..
			} => {
				let k = usize::from(pitch.min(127));
				self.note_bend[k] = semitones.clamp(-48.0, 48.0);
				self.bend_set[k] = true;
				ExpressionAction::None
			}
			MidiEvent::NoteOn { pitch, .. } => {
				let k = usize::from(pitch.min(127));
				if !self.bend_set[k] {
					self.note_bend[k] = 0.0;
				}
				self.bend_set[k] = false;
				ExpressionAction::None
			}
			MidiEvent::NoteOff { pitch, .. } => {
				self.bend_set[usize::from(pitch.min(127))] = false;
				ExpressionAction::None
			}
			MidiEvent::PitchBend { value, .. } => {
				self.bend = value.clamp(-1.0, 1.0);
				ExpressionAction::None
			}
			MidiEvent::ChannelPressure { value, .. } => {
				self.pressure = value.clamp(0.0, 1.0);
				ExpressionAction::None
			}
			MidiEvent::ControlChange {
				controller, value, ..
			} => match controller {
				cc::MOD_WHEEL => {
					self.modulation = value.clamp(0.0, 1.0);
					ExpressionAction::None
				}
				cc::SUSTAIN => {
					let down = value >= 0.5;
					let was = std::mem::replace(&mut self.sustain, down);
					if was && !down {
						ExpressionAction::PedalUp
					} else {
						ExpressionAction::None
					}
				}
				cc::RESET_ALL_CONTROLLERS => {
					let was = self.sustain;
					self.reset_controllers();
					if was {
						ExpressionAction::PedalUp
					} else {
						ExpressionAction::None
					}
				}
				cc::ALL_SOUND_OFF | cc::ALL_NOTES_OFF => ExpressionAction::AllNotesOff,
				_ => ExpressionAction::None,
			},
			MidiEvent::AllNotesOff { .. } => {
				// Transport stops/seeks also lift the pedal.
				self.sustain = false;
				ExpressionAction::None
			}
		}
	}

	pub fn reset_controllers(&mut self) {
		*self = Self {
			bend_range: self.bend_range,
			phase: self.phase,
			note_bend: self.note_bend,
			bend_set: self.bend_set,
			..Self::default()
		};
	}

	/// Key `pitch`'s own bend, semitones.
	pub fn note_bend(&self, pitch: u8) -> f32 {
		self.note_bend[usize::from(pitch.min(127))]
	}

	/// Pitch offset from the bend wheel, semitones.
	pub fn bend_semitones(&self) -> f32 {
		self.bend * self.bend_range
	}

	/// Pitch offset (bend + mod wheel vibrato) for the next `samples`, advancing the
	/// vibrato LFO; call once per rendered chunk.
	pub fn pitch_offset(&mut self, samples: usize, sample_rate: f32) -> f32 {
		let vib = if self.modulation > 0.0 {
			(self.phase * std::f32::consts::TAU).sin() * VIBRATO_DEPTH * self.modulation
		} else {
			0.0
		};
		self.phase = (self.phase + VIBRATO_HZ * samples as f32 / sample_rate.max(1.0)).fract();
		self.bend_semitones() + vib
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn pedal_and_bend() {
		let mut e = Expression::default();
		let cc = |controller, value| MidiEvent::ControlChange {
			offset: 0,
			controller,
			value,
		};
		assert_eq!(e.apply(&cc(cc::SUSTAIN, 1.0)), ExpressionAction::None);
		assert!(e.sustain);
		assert_eq!(e.apply(&cc(cc::SUSTAIN, 0.0)), ExpressionAction::PedalUp);
		e.apply(&MidiEvent::PitchBend {
			offset: 0,
			value: -0.5,
		});
		assert_eq!(e.bend_semitones(), -1.0);
		assert_eq!(e.pitch_offset(64, 48_000.0), -1.0, "no vibrato without mod");
		e.apply(&cc(cc::MOD_WHEEL, 1.0));
		let mut moved = false;
		for _ in 0..200 {
			moved |= (e.pitch_offset(64, 48_000.0) + 1.0).abs() > 0.1;
		}
		assert!(moved, "the mod wheel adds vibrato");
		e.apply(&cc(cc::RESET_ALL_CONTROLLERS, 1.0));
		assert_eq!(e.bend, 0.0);
	}
}
