//! Errors that end a session before it can produce an outcome.

use crate::provider::ProviderError;

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
	#[error("I/O error on '{path}': {message}")]
	Io { path: String, message: String },
	#[error(transparent)]
	Provider(#[from] ProviderError),
	#[error("invalid configuration: {0}")]
	Config(String),
}

impl AgentError {
	pub(crate) fn io(path: &std::path::Path, err: std::io::Error) -> Self {
		Self::Io {
			path: path.display().to_string(),
			message: err.to_string(),
		}
	}
}
