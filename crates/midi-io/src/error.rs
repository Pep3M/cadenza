//! Error type shared by every module in this crate.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum MidiIoError {
	#[error("failed to parse standard MIDI file: {0}")]
	Parse(String),
	#[error("unsupported SMF timing format (only metrical/PPQ files are supported)")]
	UnsupportedTiming,
	#[error("empty standard MIDI file (no tracks)")]
	EmptyFile,
	#[error("track {0} not found")]
	TrackNotFound(usize),
	#[error("failed to write standard MIDI file: {0}")]
	Write(String),
	#[error("MIDI input port not found: {0}")]
	PortNotFound(String),
	#[error("MIDI input backend error: {0}")]
	Input(String),
	#[error("MIDI input queue is full, event dropped")]
	QueueFull,
}

pub type Result<T> = std::result::Result<T, MidiIoError>;
