//! Where third-party plugin devices run, and how their crashes are reported.
//!
//! By default every CLAP/VST3 device runs in its own `cadenza-plugin-sandbox` process
//! ([`HostingMode::Sandboxed`]): a plugin that crashes or hangs is bypassed (instruments go
//! silent, effects pass audio through) and reported, while the render, analysis or playback
//! keeps going. [`HostingMode::InProcess`] is kept for debugging and benchmarks, and is the
//! default where sandboxing is not available ([`cadenza_plugin_host::SANDBOX_SUPPORTED`],
//! i.e. Windows for now).

use std::path::PathBuf;

use cadenza_core::{DeviceId, TrackId};
use cadenza_plugin_host::{
	CrashReason, HostingMode, PluginEditor, PluginError, SandboxMonitor, SandboxOptions,
	SandboxStats,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Environment variable choosing the default hosting mode: `in-process` or `sandboxed`
/// (the default; unknown values also mean `sandboxed`). Ignored where sandboxing is not
/// supported: plugins run in-process there.
pub const HOSTING_ENV: &str = "CADENZA_PLUGIN_HOSTING";

/// How third-party plugin devices are hosted.
#[derive(Debug, Clone, PartialEq)]
pub struct PluginHosting {
	pub mode: HostingMode,
	/// Sandbox deadlines and executable (`executable: None` finds `cadenza-plugin-sandbox`
	/// through `CADENZA_PLUGIN_SANDBOX` or next to the running executable).
	pub sandbox: SandboxOptions,
}

impl PluginHosting {
	/// Every plugin in its own sandbox process.
	pub fn sandboxed() -> Self {
		Self {
			mode: HostingMode::Sandboxed,
			sandbox: SandboxOptions::default(),
		}
	}

	/// Plugins loaded into this process (fastest; a plugin crash kills the host).
	pub fn in_process() -> Self {
		Self {
			mode: HostingMode::InProcess,
			sandbox: SandboxOptions::default(),
		}
	}

	/// [`PluginHosting::sandboxed`] unless [`HOSTING_ENV`] says `in-process` or this OS
	/// cannot sandbox plugins (logged once).
	pub fn from_env() -> Self {
		if !cadenza_plugin_host::SANDBOX_SUPPORTED {
			static LOGGED: std::sync::Once = std::sync::Once::new();
			LOGGED.call_once(|| {
				eprintln!(
					"cadenza: plugin sandboxing is not available on this OS yet; CLAP/VST3 \
					 plugins run inside Cadenza (a crashing plugin takes the session down)"
				);
			});
			return Self::in_process();
		}
		let value = std::env::var(HOSTING_ENV).unwrap_or_default();
		match value.trim().to_ascii_lowercase().replace('_', "-").as_str() {
			"in-process" | "inprocess" => Self::in_process(),
			_ => Self::sandboxed(),
		}
	}
}

impl Default for PluginHosting {
	/// [`PluginHosting::from_env`].
	fn default() -> Self {
		Self::from_env()
	}
}

/// Why plugins cannot be sandboxed here, or `None` if they can. Not real-time.
pub(crate) fn sandbox_unavailable(options: &SandboxOptions) -> Option<String> {
	if !cadenza_plugin_host::SANDBOX_SUPPORTED {
		return Some(
			"plugin sandboxing is not available on this operating system yet, so third-party \
			 plugins run inside Cadenza: a crashing plugin would abort the whole render or session"
				.into(),
		);
	}
	let missing = match &options.executable {
		Some(p) if p.is_file() => return None,
		Some(p) => p.clone(),
		None => match cadenza_plugin_host::sandbox_path() {
			Ok(_) => return None,
			Err(PluginError::SandboxMissing(p)) => p,
			Err(e) => PathBuf::from(e.to_string()),
		},
	};
	Some(format!(
		"the plugin sandbox executable was not found at '{}', so third-party plugins run \
		 inside Cadenza: a crashing plugin would abort the whole render or session. Fix: build \
		 it with `cargo build --release` (it must sit next to the cadenza executable) or set {} \
		 to its path; set {HOSTING_ENV}=in-process to run plugins in-process on purpose and \
		 silence this warning",
		missing.display(),
		cadenza_plugin_host::SANDBOX_ENV,
	))
}

/// A sandboxed plugin device that crashed or hung and is now bypassed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PluginCrash {
	/// Track holding the device; `None` for the master channel.
	pub track_id: Option<TrackId>,
	/// e.g. "track 'Lead'" or "master".
	pub location: String,
	pub device_id: DeviceId,
	pub device_name: String,
	/// Plugin id (CLAP id or VST3 class id).
	pub plugin_id: String,
	pub reason: CrashReason,
	/// Human/LLM-readable summary with what happened and what to do next.
	pub message: String,
}

/// The editor handle of a plugin device in a compiled graph.
#[derive(Clone)]
pub(crate) struct PluginEditorSlot {
	pub device_id: DeviceId,
	/// Track holding the device; `None` for the master channel.
	pub track_id: Option<TrackId>,
	pub editor: PluginEditor,
}

/// A sandboxed device in a compiled graph, watched for crashes off the audio thread.
#[derive(Clone)]
pub(crate) struct PluginWatch {
	pub monitor: SandboxMonitor,
	pub track_id: Option<TrackId>,
	pub location: String,
	pub device_id: DeviceId,
	pub device_name: String,
	/// e.g. "CLAP plugin 'com.u-he.diva' ('Diva')".
	pub label: String,
	pub instrument: bool,
	/// Engine sample at which the offline render loop first saw the crash.
	pub crashed_at: Option<i64>,
}

impl PluginWatch {
	/// The crash, if any; `context` says where it happened and what to do (it ends the
	/// message). Not real-time.
	pub fn crash(&self, context: &str) -> Option<PluginCrash> {
		let info = self.monitor.crashed()?;
		let bypass = if self.instrument {
			"this instrument outputs silence"
		} else {
			"this effect passes audio through unchanged"
		};
		let mut message = format!(
			"{}: {} {}; it was bypassed from then on ({bypass}).",
			self.location,
			self.label,
			describe(&info.reason),
		);
		if !context.is_empty() {
			message.push(' ');
			message.push_str(context);
		}
		Some(PluginCrash {
			track_id: self.track_id,
			location: self.location.clone(),
			device_id: self.device_id,
			device_name: self.device_name.clone(),
			plugin_id: info.plugin_id,
			reason: info.reason,
			message,
		})
	}

	/// The device's realtime dropouts, if it had any. Wait-free.
	pub fn xruns(&self) -> Option<PluginXruns> {
		let stats = self.monitor.stats();
		(stats.late_blocks > 0 || stats.dropped_frames > 0).then(|| PluginXruns {
			track_id: self.track_id,
			location: self.location.clone(),
			device_id: self.device_id,
			device_name: self.device_name.clone(),
			plugin_id: self.monitor.plugin_id().to_string(),
			stats,
		})
	}
}

/// Non-fatal realtime trouble of a sandboxed plugin device: it answered late (or was
/// still busy) a few times, and that audio was replaced by silence (instruments) or the
/// dry signal (effects) while the rest of the mix kept playing. Plugins that stop
/// answering for good are reported as [`PluginCrash`] instead.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PluginXruns {
	/// Track holding the device; `None` for the master channel.
	pub track_id: Option<TrackId>,
	/// e.g. "track 'Lead'" or "master".
	pub location: String,
	pub device_id: DeviceId,
	pub device_name: String,
	pub plugin_id: String,
	pub stats: SandboxStats,
}

fn describe(reason: &CrashReason) -> String {
	match reason {
		CrashReason::Signal { name, .. } => format!("crashed ({name})"),
		CrashReason::Exited { code } => format!("exited unexpectedly (code {code})"),
		CrashReason::TimedOut { millis } => {
			format!("hung (no audio within {millis} ms) and was stopped")
		}
		CrashReason::Lost { detail } => format!("stopped working ({detail})"),
	}
}
