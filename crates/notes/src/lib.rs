//! Note detection on audio recordings (design: `docs/design/note-editing-plan.md`).
//!
//! NE-0 ships the measuring stick before any real detector: [`eval`] renders a synthetic
//! corpus with exact ground truth, loads hand-labeled recordings, and scores any
//! [`NoteDetector`] with note-level metrics (mir_eval rules) plus the ones that matter for
//! tuning: the error of each note's pitch center and the share of regions that mix two
//! real notes. [`baseline`] is the trivial detector later phases must beat.
//!
//! Times are seconds of the analyzed audio; pitches are fractional MIDI numbers (60.0 = C4,
//! 60.5 = C4 + 50 cents).

pub mod analysis;
pub mod baseline;
pub mod eval;
pub mod features;
mod par;
pub mod pyin;
pub mod yin;

use thiserror::Error;

/// A note found in audio: one region of the recording with one perceived pitch.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Note {
	/// Seconds.
	pub onset: f64,
	/// Seconds, after `onset`.
	pub offset: f64,
	/// Pitch center as a fractional MIDI number.
	pub center: f64,
}

impl Note {
	pub fn duration(&self) -> f64 {
		self.offset - self.onset
	}
}

/// Anything that turns mono audio into notes. Implementations must be deterministic.
pub trait NoteDetector {
	/// Short id used on the command line and in reports.
	fn name(&self) -> &'static str;
	fn detect(&self, samples: &[f32], sample_rate: u32) -> Vec<Note>;
}

/// One frame of a pitch track.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PitchFrame {
	/// Seconds (frame center).
	pub time: f64,
	/// Fractional MIDI pitch, `None` when unvoiced.
	pub pitch: Option<f64>,
	/// How sure the tracker is that the frame is voiced, 0..1.
	pub voicing: f64,
}

/// Anything that turns mono audio into a frame-wise pitch track. Deterministic.
pub trait PitchTracker {
	fn name(&self) -> &'static str;
	fn track(&self, samples: &[f32], sample_rate: u32) -> Vec<PitchFrame>;
}

/// Ids accepted by [`tracker`].
pub const TRACKERS: &[&str] = &["pyin", "yin"];

/// The pitch tracker with that id.
pub fn tracker(name: &str) -> Option<Box<dyn PitchTracker>> {
	match name {
		"pyin" => Some(Box::new(pyin::Pyin::default())),
		"yin" => Some(Box::new(yin::Yin::default())),
		_ => None,
	}
}

/// Ids accepted by [`detector`].
pub const DETECTORS: &[&str] = &["baseline"];

/// The detector with that id.
pub fn detector(name: &str) -> Option<Box<dyn NoteDetector>> {
	match name {
		"baseline" => Some(Box::new(baseline::Baseline::default())),
		_ => None,
	}
}

#[derive(Debug, Error)]
pub enum NotesError {
	#[error("invalid note labels: {0}")]
	Labels(String),
}

pub type Result<T> = std::result::Result<T, NotesError>;

/// Frequency in Hz -> fractional MIDI number.
pub fn hz_to_midi(hz: f64) -> f64 {
	69.0 + 12.0 * (hz / 440.0).log2()
}

/// Fractional MIDI number -> frequency in Hz.
pub fn midi_to_hz(midi: f64) -> f64 {
	440.0 * 2f64.powf((midi - 69.0) / 12.0)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn midi_and_hz_round_trip() {
		assert!((hz_to_midi(440.0) - 69.0).abs() < 1e-12);
		assert!((midi_to_hz(60.0) - 261.625_565).abs() < 1e-5);
		assert!((hz_to_midi(midi_to_hz(47.37)) - 47.37).abs() < 1e-9);
	}
}
