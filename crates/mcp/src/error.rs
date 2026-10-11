//! Errors returned by `cadenza-mcp` tools.
//!
//! Every variant is written to be read by an LLM: it names the offending id/value and
//! says what would be accepted instead, matching the convention used by `cadenza-model`
//! and `cadenza-theory`.

use rmcp::ErrorData;

#[derive(Debug, thiserror::Error)]
pub enum McpError {
	#[error(transparent)]
	Model(#[from] cadenza_model::ModelError),
	#[error(transparent)]
	Theory(#[from] cadenza_theory::TheoryError),
	#[error(transparent)]
	Engine(#[from] cadenza_engine::EngineError),
	#[error(transparent)]
	Plugin(#[from] cadenza_plugin_host::PluginError),
	#[error(
		"device '{id}' is not a known device id; built-in {role} ids: {available}. Third-party \
		 plugins use ids like 'clap:<plugin id>': search them with list_devices {{ query, source: \"plugin\" }} \
		 (run scan_plugins first if no plugins are listed)"
	)]
	UnknownDevice {
		id: String,
		role: &'static str,
		available: String,
	},
	#[error(
		"plugin device '{id}' is not in the plugin catalog; run scan_plugins, then find its id with \
		 list_devices {{ query, source: \"plugin\" }}"
	)]
	UnknownPlugin { id: String },
	#[error(
		"parameter '{param}' matches several parameters of device '{device}': {candidates}; \
		 use the numeric id or the exact 'module/name' (see get_device_info)"
	)]
	AmbiguousParam {
		device: String,
		param: String,
		candidates: String,
	},
	#[error(
		"parameter '{param}' is not valid for device '{device}'; valid parameters: {available}"
	)]
	UnknownParam {
		device: String,
		param: String,
		available: String,
	},
	#[error(
		"parameter '{param}' value {value} is out of range for device '{device}'; must be {min}..={max}"
	)]
	ParamOutOfRange {
		device: String,
		param: String,
		value: f32,
		min: f32,
		max: f32,
	},
	#[error("device '{id}' is a {actual:?} device; expected a {expected:?} device for this slot")]
	WrongDeviceRole {
		id: String,
		expected: cadenza_dsp::DeviceRole,
		actual: cadenza_dsp::DeviceRole,
	},
	#[error("no project path known; pass 'path' or open/save with a path first")]
	NoPath,
	#[error("I/O error on '{path}': {message}")]
	Io { path: String, message: String },
	#[error("{0}")]
	Invalid(String),
}

/// Failures starting or running the stdio server.
#[derive(Debug, thiserror::Error)]
pub enum ServeError {
	#[error(transparent)]
	Init(#[from] Box<rmcp::service::ServerInitializeError>),
	#[error("server task panicked: {0}")]
	Join(#[from] tokio::task::JoinError),
}

impl From<McpError> for ErrorData {
	fn from(err: McpError) -> Self {
		match err {
			McpError::Io { .. } => ErrorData::internal_error(err.to_string(), None),
			McpError::Engine(cadenza_engine::EngineError::InvalidOptions(_)) => {
				ErrorData::invalid_params(err.to_string(), None)
			}
			McpError::Engine(_) => ErrorData::internal_error(err.to_string(), None),
			_ => ErrorData::invalid_params(err.to_string(), None),
		}
	}
}
