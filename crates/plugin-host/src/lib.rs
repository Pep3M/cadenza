//! Third-party plugin hosting for Cadenza.
//!
//! - [`discovery`]: where plugins live on each OS.
//! - [`PluginCatalog`]: incremental, crash-safe scanning through the
//!   `cadenza-plugin-scanner` subprocess, cached as JSON, with a blocklist.
//! - [`instantiate`]: turns a [`PluginDescriptor`] into a real-time
//!   [`cadenza_dsp::Processor`] the engine can run like any built-in device.
//!
//! Only CLAP is hostable today; the descriptor/catalog layer is
//! format-agnostic so VST3 can be added behind the same API.

pub mod catalog;
pub mod clap;
pub mod descriptor;
pub mod discovery;
mod error;
pub mod scanner;

use base64::Engine as _;
use cadenza_dsp::Processor;

pub use catalog::{BlockedPlugin, FileStamp, PluginCatalog, ScanReport, default_cache_path};
pub use descriptor::{PluginDescriptor, PluginFormat, PluginParam, PluginRole};
pub use discovery::{default_search_paths, find_clap_files};
pub use error::PluginError;
pub use scanner::{DEFAULT_SCAN_TIMEOUT, SCANNER_ENV, scanner_path};

/// Creates, activates and (optionally) restores a plugin instance.
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
	match desc.format {
		PluginFormat::Clap => Ok(Box::new(clap::ClapProcessor::create(
			desc,
			sample_rate,
			max_block,
			state,
		)?)),
		PluginFormat::Vst3 => Err(PluginError::UnsupportedFormat("VST3")),
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
