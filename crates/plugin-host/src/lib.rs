//! Third-party plugin hosting for Cadenza.
//!
//! - [`discovery`]: where plugins live on each OS.
//! - [`PluginCatalog`]: incremental, crash-safe scanning through the
//!   `cadenza-plugin-scanner` subprocess, cached as JSON, with a blocklist.
//! - [`instantiate`]: turns a [`PluginDescriptor`] into a real-time
//!   [`cadenza_dsp::Processor`] the engine can run like any built-in device.
//!
//! CLAP ([`clap`]) and VST3 ([`vst3`]) are peer formats behind the same
//! descriptor, catalog, scanner and `instantiate` API. Both share one plugin
//! main thread (`cadenza-plugin-main`, see `main_thread.rs`).

pub mod catalog;
pub mod clap;
pub mod descriptor;
pub mod discovery;
mod error;
pub(crate) mod main_thread;
pub mod scanner;
pub mod vst3;

use base64::Engine as _;
use cadenza_dsp::Processor;

pub use catalog::{BlockedPlugin, FileStamp, PluginCatalog, ScanReport, default_cache_path};
pub use descriptor::{PluginDescriptor, PluginFormat, PluginParam, PluginRole};
pub use discovery::{default_search_paths, find_clap_files, find_plugin_files, find_vst3_files};
pub use error::PluginError;
pub use scanner::{DEFAULT_SCAN_TIMEOUT, SCANNER_ENV, scanner_path};

/// How the plugin will be driven, passed to plugins that distinguish it
/// (VST3 `ProcessModes`). CLAP plugins currently always run in realtime mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProcessMode {
	/// Live playback: the plugin must keep up with the audio clock.
	#[default]
	Realtime,
	/// Offline rendering: the plugin may use slower, higher-quality paths.
	Offline,
}

/// Creates, activates and (optionally) restores a plugin instance in
/// [`ProcessMode::Realtime`]; see [`instantiate_with_mode`].
///
/// `sample_rate`/`max_block` are used for the initial activation; calling
/// `prepare` later with different values re-activates the plugin. `state` is
/// opaque data previously returned by `Processor::save_state` (see
/// [`decode_state`] for the base64 form stored in projects).
///
/// Not real-time: loads code and blocks on the plugin main thread. The
/// returned processor may be moved to the audio thread; drop it off the audio
/// thread.
pub fn instantiate(
	desc: &PluginDescriptor,
	sample_rate: f32,
	max_block: usize,
	state: Option<&[u8]>,
) -> Result<Box<dyn Processor>, PluginError> {
	instantiate_with_mode(desc, sample_rate, max_block, state, ProcessMode::Realtime)
}

/// Like [`instantiate`], with an explicit [`ProcessMode`] (e.g. `Offline`
/// for bounces).
pub fn instantiate_with_mode(
	desc: &PluginDescriptor,
	sample_rate: f32,
	max_block: usize,
	state: Option<&[u8]>,
	mode: ProcessMode,
) -> Result<Box<dyn Processor>, PluginError> {
	match desc.format {
		PluginFormat::Clap => Ok(Box::new(clap::ClapProcessor::create(
			desc,
			sample_rate,
			max_block,
			state,
		)?)),
		PluginFormat::Vst3 => Ok(Box::new(vst3::Vst3Processor::create(
			desc,
			sample_rate,
			max_block,
			state,
			mode,
		)?)),
	}
}

/// Encodes plugin state for `Device.state` (standard base64).
pub fn encode_state(data: &[u8]) -> String {
	base64::engine::general_purpose::STANDARD.encode(data)
}

/// Decodes `Device.state`, with an LLM-actionable error.
pub fn decode_state(plugin_id: &str, encoded: &str) -> Result<Vec<u8>, PluginError> {
	base64::engine::general_purpose::STANDARD
		.decode(encoded.trim())
		.map_err(|e| PluginError::State {
			id: plugin_id.to_string(),
			reason: format!("state is not valid base64 ({e})"),
		})
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn state_round_trips_through_base64() {
		let data = [0u8, 1, 2, 255, 128];
		assert_eq!(decode_state("x", &encode_state(&data)).unwrap(), data);
		assert!(decode_state("x", "@@@").is_err());
	}
}
