//! Live voice state of the built-in instruments, published through
//! [`Processor::readings`](crate::Processor::readings) at the indices of
//! [`readings`](crate::readings) (`HELD_NOTES_START`, `VOICE_COUNT`, `VOICES_START`):
//! the held keys and the newest sounding voices (key, zone, playhead, gate phase,
//! level). Encoding is realtime safe; the decoders are for editors.

use crate::processor::MidiEvent;
use crate::processor::readings::{
	HELD_NOTE_WORDS, HELD_NOTES_START, MAX_VOICES, VOICE_COUNT, VOICE_ELAPSED_MS, VOICE_FIELDS,
	VOICE_LEVEL, VOICE_NOTE, VOICE_POSITION, VOICE_RELEASED, VOICE_ZONE, VOICES_END, VOICES_START,
};

/// The keys an instrument holds down: 128 bits.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HeldNotes([u16; HELD_NOTE_WORDS]);

impl HeldNotes {
	pub fn press(&mut self, pitch: u8) {
		let p = usize::from(pitch.min(127));
		self.0[p / 16] |= 1 << (p % 16);
	}

	pub fn release(&mut self, pitch: u8) {
		let p = usize::from(pitch.min(127));
		self.0[p / 16] &= !(1 << (p % 16));
	}

	pub fn clear(&mut self) {
		self.0 = [0; HELD_NOTE_WORDS];
	}

	pub fn is_held(&self, pitch: u8) -> bool {
		let p = usize::from(pitch.min(127));
		self.0[p / 16] & (1 << (p % 16)) != 0
	}

	pub fn any(&self) -> bool {
		self.0.iter().any(|&w| w != 0)
	}

	/// Held keys, lowest first.
	pub fn iter(&self) -> impl Iterator<Item = u8> + '_ {
		(0..=127u8).filter(|&p| self.is_held(p))
	}

	/// Follows the key events of `ev` (a zero-velocity note-on is a note-off).
	pub fn apply(&mut self, ev: &MidiEvent) {
		match *ev {
			MidiEvent::NoteOn {
				pitch, velocity, ..
			} if velocity > 0.0 => self.press(pitch),
			MidiEvent::NoteOn { pitch, .. } | MidiEvent::NoteOff { pitch, .. } => {
				self.release(pitch)
			}
			MidiEvent::AllNotesOff { .. } => self.clear(),
			_ => {}
		}
	}

	/// Reads the mask from a device's readings (empty when it publishes none).
	pub fn decode(readings: &[f32]) -> Self {
		let mut out = Self::default();
		if let Some(words) = readings.get(HELD_NOTES_START..HELD_NOTES_START + HELD_NOTE_WORDS) {
			for (o, &w) in out.0.iter_mut().zip(words) {
				*o = if w.is_finite() {
					w.clamp(0.0, f32::from(u16::MAX)) as u16
				} else {
					0
				};
			}
		}
		out
	}
}

/// One sounding voice as published.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct VoiceReading {
	pub note: u8,
	/// Zone index (sample built-ins).
	pub zone: Option<usize>,
	/// Playhead in seconds of the zone's file (sample built-ins).
	pub position: Option<f32>,
	/// The key is up: the voice is in its release tail.
	pub released: bool,
	/// Milliseconds since the note-on while held, since the release once released.
	pub elapsed_ms: f32,
	/// Amp envelope level, 0..1.
	pub level: f32,
}

/// Keeps the newest [`MAX_VOICES`] voices offered (allocation free).
#[derive(Debug, Clone, Copy, Default)]
pub struct NewestVoices {
	items: [(u64, VoiceReading); MAX_VOICES],
	len: usize,
}

impl NewestVoices {
	/// Offers a voice started at `started` (monotonic stamp, higher = newer).
	pub fn offer(&mut self, started: u64, voice: VoiceReading) {
		let at = self.items[..self.len]
			.iter()
			.position(|&(s, _)| started > s)
			.unwrap_or(self.len);
		if at >= MAX_VOICES {
			return;
		}
		let end = self.len.min(MAX_VOICES - 1);
		self.items.copy_within(at..end, at + 1);
		self.items[at] = (started, voice);
		self.len = (self.len + 1).min(MAX_VOICES);
	}

	pub fn len(&self) -> usize {
		self.len
	}

	pub fn is_empty(&self) -> bool {
		self.len == 0
	}

	/// Newest first.
	pub fn iter(&self) -> impl Iterator<Item = &VoiceReading> {
		self.items[..self.len].iter().map(|(_, v)| v)
	}
}

/// Writes the held keys and voices into `out` (readings layout). Returns
/// [`VOICES_END`] (the count of readings a device publishes with them), or 0 when `out`
/// is too short.
pub fn write(out: &mut [f32], held: &HeldNotes, voices: &NewestVoices) -> usize {
	if out.len() < VOICES_END {
		return 0;
	}
	for (o, &w) in out[HELD_NOTES_START..].iter_mut().zip(&held.0) {
		*o = f32::from(w);
	}
	out[VOICE_COUNT] = voices.len() as f32;
	out[VOICES_START..VOICES_END].fill(0.0);
	for (i, v) in voices.iter().enumerate() {
		let r = &mut out[VOICES_START + i * VOICE_FIELDS..][..VOICE_FIELDS];
		r[VOICE_NOTE] = f32::from(v.note);
		r[VOICE_ZONE] = v.zone.map_or(-1.0, |z| z as f32);
		r[VOICE_POSITION] = v.position.unwrap_or(-1.0);
		r[VOICE_RELEASED] = if v.released { 1.0 } else { 0.0 };
		r[VOICE_ELAPSED_MS] = v.elapsed_ms;
		r[VOICE_LEVEL] = v.level;
	}
	VOICES_END
}

/// The voice records of a device's readings, newest first (none when it publishes none).
pub fn decode_voices(readings: &[f32]) -> impl Iterator<Item = VoiceReading> + '_ {
	let count = if readings.len() >= VOICES_END {
		(readings[VOICE_COUNT].max(0.0) as usize).min(MAX_VOICES)
	} else {
		0
	};
	(0..count).map(move |i| {
		let r = &readings[VOICES_START + i * VOICE_FIELDS..][..VOICE_FIELDS];
		VoiceReading {
			note: r[VOICE_NOTE].clamp(0.0, 127.0) as u8,
			zone: (r[VOICE_ZONE] >= 0.0).then_some(r[VOICE_ZONE] as usize),
			position: (r[VOICE_POSITION] >= 0.0).then_some(r[VOICE_POSITION]),
			released: r[VOICE_RELEASED] > 0.5,
			elapsed_ms: r[VOICE_ELAPSED_MS].max(0.0),
			level: r[VOICE_LEVEL].clamp(0.0, 1.0),
		}
	})
}

/// Milliseconds in `samples` at `sample_rate`.
#[inline]
pub fn samples_to_ms(samples: u32, sample_rate: f32) -> f32 {
	samples as f32 * 1000.0 / sample_rate.max(1.0)
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::readings::MAX;

	#[test]
	fn held_notes_follow_key_events_and_round_trip() {
		let mut h = HeldNotes::default();
		for p in [0u8, 15, 16, 60, 127] {
			h.apply(&MidiEvent::NoteOn {
				offset: 0,
				pitch: p,
				velocity: 0.8,
			});
		}
		h.apply(&MidiEvent::NoteOff {
			offset: 0,
			pitch: 15,
		});
		h.apply(&MidiEvent::NoteOn {
			offset: 0,
			pitch: 16,
			velocity: 0.0,
		});
		assert_eq!(h.iter().collect::<Vec<_>>(), vec![0, 60, 127]);
		let mut out = [0.0; MAX];
		assert_eq!(write(&mut out, &h, &NewestVoices::default()), VOICES_END);
		assert_eq!(HeldNotes::decode(&out), h);
		h.apply(&MidiEvent::AllNotesOff { offset: 0 });
		assert!(!h.any());
		assert!(!HeldNotes::decode(&[0.0; 4]).any());
	}

	#[test]
	fn newest_voices_keep_the_latest_four_newest_first() {
		let mut n = NewestVoices::default();
		for (started, note) in [(3u64, 3u8), (1, 1), (5, 5), (2, 2), (4, 4), (0, 0)] {
			n.offer(
				started,
				VoiceReading {
					note,
					..Default::default()
				},
			);
		}
		let notes: Vec<u8> = n.iter().map(|v| v.note).collect();
		assert_eq!(notes, vec![5, 4, 3, 2]);
		let mut out = [0.0; MAX];
		write(&mut out, &HeldNotes::default(), &n);
		let back: Vec<u8> = decode_voices(&out).map(|v| v.note).collect();
		assert_eq!(back, notes);
	}

	#[test]
	fn voice_fields_round_trip() {
		let mut n = NewestVoices::default();
		let v = VoiceReading {
			note: 64,
			zone: Some(3),
			position: Some(1.25),
			released: true,
			elapsed_ms: 120.0,
			level: 0.5,
		};
		n.offer(1, v);
		n.offer(
			0,
			VoiceReading {
				note: 1,
				..Default::default()
			},
		);
		let mut out = [0.0; MAX];
		write(&mut out, &HeldNotes::default(), &n);
		let back: Vec<VoiceReading> = decode_voices(&out).collect();
		assert_eq!(back[0], v);
		assert_eq!(back[1].zone, None);
		assert_eq!(back[1].position, None);
		assert_eq!(decode_voices(&out[..10]).count(), 0);
	}
}
