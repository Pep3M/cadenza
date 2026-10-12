use std::path::PathBuf;

/// Errors produced by `cadenza-audio-io`. Messages are written to be actionable
/// (they may be shown to a user or read by an AI agent).
#[derive(Debug, thiserror::Error)]
pub enum AudioIoError {
	#[error("cannot access audio file '{path}': {source}")]
	Io {
		path: PathBuf,
		#[source]
		source: std::io::Error,
	},
	#[error(
		"unsupported audio file '{path}': {reason}. Supported: WAV, AIFF, FLAC, MP3, OGG/Vorbis, M4A (AAC/ALAC), CAF"
	)]
	Unsupported { path: PathBuf, reason: String },
	#[error("audio file '{path}' contains no decodable audio track")]
	NoAudioTrack { path: PathBuf },
	#[error("failed to decode '{path}': {reason}")]
	Decode { path: PathBuf, reason: String },
	#[error("resampling failed: {0}")]
	Resample(String),
	#[error("encoding failed: {0}")]
	Encode(String),
	#[error("time-stretch failed: {0}")]
	Stretch(String),
	#[error("invalid argument: {0}")]
	InvalidArgument(String),
}

pub type Result<T, E = AudioIoError> = std::result::Result<T, E>;
