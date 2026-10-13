//! Third-party plugin hosting for Cadenza.
//!
//! - [`discovery`]: where plugins live on each OS.
//! - [`PluginCatalog`]: incremental, crash-safe scanning through the
//!   `cadenza-plugin-scanner` subprocess, cached as JSON, with a blocklist.
//! - [`instantiate`]: turns a [`PluginDescriptor`] into a real-time
//!   [`cadenza_dsp::Processor`] the engine can run like any built-in device.
//!
//! - [`instantiate_hosted`] / [`instantiate_sandboxed`]: the same, optionally
//!   running the plugin in a `cadenza-plugin-sandbox` child process so a
//!   crashing or hanging plugin cannot take the host down (see [`sandbox`]).
//!
//! CLAP ([`clap`]) and VST3 ([`vst3`]) are peer formats behind the same
//! descriptor, catalog, scanner and `instantiate` API. Both share one plugin
//! main thread (`cadenza-plugin-main`, see `main_thread.rs`), which is also
//! the event loop of plugin editor windows ([`editor`], [`PluginEditor`]).
//!
//! Platforms: hosting, scanning and the catalog work everywhere; the sandbox
//! needs Unix ([`SANDBOX_SUPPORTED`]) and editor windows X11 (Linux/BSD,
//! [`EDITORS_SUPPORTED`]).

// Elsewhere the sandbox transport, the editor windows and the fd event loop
// they share are compiled (type-checked) but not wired up.
#![cfg_attr(
	not(all(unix, not(target_os = "macos"))),
	allow(dead_code, unused_imports)
)]

pub mod catalog;
pub mod clap;
pub mod descriptor;
pub mod discovery;
pub mod editor;
mod error;
pub(crate) mod main_thread;
mod ports;
pub mod sandbox;
pub mod scanner;
pub mod vst3;

use base64::Engine as _;
use cadenza_dsp::Processor;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub use catalog::{BlockedPlugin, FileStamp, PluginCatalog, ScanReport, default_cache_path};
pub use descriptor::{AuxOutput, PluginDescriptor, PluginFormat, PluginParam, PluginRole};
pub use discovery::{default_search_paths, find_clap_files, find_plugin_files, find_vst3_files};
pub use editor::{EDITORS_SUPPORTED, EditorEvent, PluginEditor, TITLE_STRIP_HEIGHT};
pub use error::PluginError;
pub use sandbox::{
	CrashInfo, CrashReason, SANDBOX_ENV, SANDBOX_SUPPORTED, SandboxMonitor, SandboxOptions,
	SandboxStats, SandboxedProcessor, sandbox_path,
};
pub use scanner::{DEFAULT_SCAN_TIMEOUT, SCANNER_ENV, scanner_path};

/// Realtime scheduling for audio threads (the sandbox child's and the host's output
/// callback): `SCHED_FIFO` directly, else RealtimeKit, else a negative nice value.
pub mod realtime {
	pub use crate::sandbox::rt::{Scheduling, current_thread_id, promote_thread};
}

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

/// Like [`instantiate_with_mode`], also returning a handle to the instance's
/// editor (see [`editor`]), which stays usable while the processor runs on the
/// audio thread.
pub fn instantiate_with_editor(
	desc: &PluginDescriptor,
	sample_rate: f32,
	max_block: usize,
	state: Option<&[u8]>,
	mode: ProcessMode,
) -> Result<(Box<dyn Processor>, PluginEditor), PluginError> {
	match desc.format {
		PluginFormat::Clap => {
			let p = clap::ClapProcessor::create(desc, sample_rate, max_block, state)?;
			let editor = p.editor();
			Ok((Box::new(p), editor))
		}
		PluginFormat::Vst3 => {
			let p = vst3::Vst3Processor::create(desc, sample_rate, max_block, state, mode)?;
			let editor = p.editor();
			Ok((Box::new(p), editor))
		}
	}
}

/// Where a plugin instance runs.
///
/// The default is [`HostingMode::InProcess`] (what [`instantiate`] has always
/// done); which mode the engine uses for third-party plugins is the
/// integrator's choice. `Sandboxed` costs one process per instance and a
/// futex round trip per processed block (a few µs), and buys crash and hang
/// isolation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum HostingMode {
	/// Load the plugin into this process (fastest; a plugin crash kills the host).
	#[default]
	InProcess,
	/// Run the plugin in its own `cadenza-plugin-sandbox` process.
	Sandboxed,
}

/// Like [`instantiate_with_mode`], choosing where the plugin runs.
/// `Sandboxed` uses [`SandboxOptions::default`]; use
/// [`instantiate_sandboxed`] to keep a [`SandboxMonitor`] or tune deadlines.
pub fn instantiate_hosted(
	desc: &PluginDescriptor,
	sample_rate: f32,
	max_block: usize,
	state: Option<&[u8]>,
	mode: ProcessMode,
	hosting: HostingMode,
) -> Result<Box<dyn Processor>, PluginError> {
	match hosting {
		HostingMode::InProcess => instantiate_with_mode(desc, sample_rate, max_block, state, mode),
		HostingMode::Sandboxed => Ok(Box::new(instantiate_sandboxed(
			desc,
			sample_rate,
			max_block,
			state,
			mode,
			SandboxOptions::default(),
		)?)),
	}
}

/// Creates a plugin instance inside a `cadenza-plugin-sandbox` child process
/// (located with [`sandbox_path`] unless `options.executable` is set).
/// Not real-time: spawns a process and waits for the plugin to load.
pub fn instantiate_sandboxed(
	desc: &PluginDescriptor,
	sample_rate: f32,
	max_block: usize,
	state: Option<&[u8]>,
	mode: ProcessMode,
	options: SandboxOptions,
) -> Result<SandboxedProcessor, PluginError> {
	SandboxedProcessor::create(desc, sample_rate, max_block, state, mode, options)
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
