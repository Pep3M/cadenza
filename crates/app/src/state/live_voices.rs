//! What the probed instrument plays right now, from its voice readings
//! (`cadenza_dsp::instruments::voice_readings`): held keys (transport playback, live
//! MIDI input and previews alike, as the processor saw them), the newest voices'
//! envelope phase (ADSR voice dots) and sample playheads (zone / pad waveforms).

use cadenza_dsp::instruments::voice_readings::{HeldNotes, VoiceReading, decode_voices};

use super::device_display::VoicePos;

/// Decoded voice readings of one device (empty when it is not probed or not running).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LiveVoices {
	pub held: HeldNotes,
	/// Newest first, at most `readings::MAX_VOICES`.
	pub voices: Vec<VoiceReading>,
}

impl LiveVoices {
	pub fn decode(readings: &[f32]) -> Self {
		Self {
			held: HeldNotes::decode(readings),
			voices: decode_voices(readings).collect(),
		}
	}

	/// Nothing held or sounding: editors can stop repainting for it.
	pub fn is_idle(&self) -> bool {
		!self.held.any() && self.voices.is_empty()
	}

	/// Keys the processor holds, lowest first.
	pub fn held_keys(&self) -> impl Iterator<Item = u8> + '_ {
		self.held.iter()
	}

	/// Envelope dots of the kit ADSR, newest first.
	pub fn envelope_dots(&self) -> Vec<VoicePos> {
		self.voices
			.iter()
			.map(|v| {
				if v.released {
					VoicePos::Released(v.elapsed_ms)
				} else {
					VoicePos::Held(v.elapsed_ms)
				}
			})
			.collect()
	}

	/// Playheads (seconds of the file) of the voices playing zone `zone`, newest first.
	pub fn playheads(&self, zone: usize) -> Vec<f32> {
		self.voices
			.iter()
			.filter(|v| v.zone == Some(zone))
			.filter_map(|v| v.position)
			.collect()
	}

	/// Loudest envelope level of the voices playing any of `zones` (0 = silent).
	pub fn zone_level(&self, zones: &[usize]) -> f32 {
		self.voices
			.iter()
			.filter(|v| v.zone.is_some_and(|z| zones.contains(&z)))
			.map(|v| v.level)
			.fold(0.0, f32::max)
	}

	/// Whether a voice plays zone `zone`.
	pub fn sounds(&self, zone: usize) -> bool {
		self.voices.iter().any(|v| v.zone == Some(zone))
	}
}

/// Seconds of the file -> x in `0..=1` of a waveform view showing `view` (seconds);
/// `None` outside it.
pub fn playhead_x(seconds: f32, view: (f64, f64)) -> Option<f32> {
	let (a, b) = view;
	if b <= a || !(a..=b).contains(&f64::from(seconds)) {
		return None;
	}
	Some(((f64::from(seconds) - a) / (b - a)) as f32)
}

#[cfg(test)]
mod tests {
	use cadenza_dsp::instruments::voice_readings::{NewestVoices, write};
	use cadenza_dsp::readings::MAX;

	use super::*;

	fn readings(held: &[u8], voices: &[VoiceReading]) -> [f32; MAX] {
		let mut h = HeldNotes::default();
		for &k in held {
			h.press(k);
		}
		let mut n = NewestVoices::default();
		for (i, v) in voices.iter().enumerate() {
			// The first given is the newest.
			n.offer((voices.len() - i) as u64, *v);
		}
		let mut out = [0.0; MAX];
		write(&mut out, &h, &n);
		out
	}

	#[test]
	fn readings_map_to_lit_keys_dots_and_playheads() {
		let a = VoiceReading {
			note: 60,
			zone: Some(2),
			position: Some(0.5),
			released: false,
			elapsed_ms: 120.0,
			level: 0.7,
		};
		let b = VoiceReading {
			note: 48,
			zone: Some(0),
			position: Some(1.25),
			released: true,
			elapsed_ms: 30.0,
			level: 0.2,
		};
		let live = LiveVoices::decode(&readings(&[60], &[a, b]));
		assert!(!live.is_idle());
		assert_eq!(live.held_keys().collect::<Vec<_>>(), vec![60]);
		assert_eq!(
			live.envelope_dots(),
			vec![VoicePos::Held(120.0), VoicePos::Released(30.0)]
		);
		assert_eq!(live.playheads(2), vec![0.5]);
		assert_eq!(live.playheads(1), Vec::<f32>::new());
		assert!(live.sounds(0) && !live.sounds(1));
		assert!((live.zone_level(&[0, 2]) - 0.7).abs() < 1e-6);
		assert_eq!(live.zone_level(&[5]), 0.0);
	}

	#[test]
	fn devices_without_voice_readings_are_idle() {
		assert!(LiveVoices::decode(&[]).is_idle());
		assert!(LiveVoices::decode(&[3.0; 16]).is_idle());
	}

	#[test]
	fn playheads_map_into_the_shown_window() {
		assert_eq!(playhead_x(1.0, (0.0, 2.0)), Some(0.5));
		assert_eq!(playhead_x(3.0, (0.0, 2.0)), None);
		assert_eq!(playhead_x(1.0, (1.0, 1.0)), None);
	}
}
