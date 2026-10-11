use std::path::PathBuf;

/// Errors from discovery, scanning and instantiation. Messages are written to
/// be read by an LLM: they say what went wrong and what to do next.
#[derive(Debug, thiserror::Error)]
pub enum PluginError {
	#[error("plugin '{0}' not found in catalog; run scan_plugins and pick an id from list_plugins")]
	NotFound(String),

	#[error(
		"plugin file '{}' could not be loaded: {reason}; check it is a valid {format} plugin for this OS/architecture",
		path.display()
	)]
	Load {
		path: PathBuf,
		format: &'static str,
		reason: String,
	},

	#[error(
		"plugin '{id}' could not be instantiated from '{}': {reason}; it may be blocked or broken, try another plugin",
		path.display()
	)]
	Instantiate {
		id: String,
		path: PathBuf,
		reason: String,
	},

	#[error("plugin '{id}' failed to activate at {sample_rate} Hz / {max_block} frames: {reason}")]
	Activate {
		id: String,
		sample_rate: f32,
		max_block: usize,
		reason: String,
	},

	#[error(
		"plugin '{id}' rejected its saved state: {reason}; the device will use default settings"
	)]
	State { id: String, reason: String },

	#[error(
		"{0} plugins are not supported yet; use a CLAP or VST3 plugin or a built-in device instead"
	)]
	UnsupportedFormat(&'static str),

	#[error(
		"plugin scanner executable not found at '{}'; build it with `cargo build -p cadenza-plugin-host` or set CADENZA_PLUGIN_SCANNER",
		.0.display()
	)]
	ScannerMissing(PathBuf),

	#[error("plugin scan of '{}' failed: {reason}", path.display())]
	Scan { path: PathBuf, reason: String },

	#[error("plugin catalog cache '{}' error: {reason}", path.display())]
	Cache { path: PathBuf, reason: String },

	#[error("plugin host main thread is unavailable: {0}")]
	HostThread(String),
}
