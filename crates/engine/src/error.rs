use std::path::PathBuf;

/// Engine failures. Messages are written to be actionable by an LLM client.
#[derive(Debug, thiserror::Error)]
pub enum EngineError {
	#[error("invalid render options: {0}")]
	InvalidOptions(String),
	#[error("could not write WAV file '{path}': {message}")]
	WavWrite { path: PathBuf, message: String },
	#[error("could not write audio file '{path}': {message}")]
	Export { path: PathBuf, message: String },
	#[error("no audio output device available")]
	NoOutputDevice,
	#[error("audio device error: {0}")]
	Audio(String),
	#[error("the audio thread is not consuming updates (is the output stream stalled?)")]
	Busy,
	#[error("{0}")]
	Device(String),
	#[error("the export was cancelled")]
	Cancelled,
	#[error("MIDI input: {0}")]
	Midi(String),
	#[error("recording: {0}")]
	Record(String),
	#[error("track render: {0}")]
	Bounce(String),
}
