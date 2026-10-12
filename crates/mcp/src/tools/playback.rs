//! Realtime playback through the default audio output device, via the dedicated
//! [`crate::player::PlayerHandle`] thread.
//!
//! Plugin devices are sandboxed by default: a plugin that crashes or hangs during playback
//! is bypassed while the rest keeps playing, and shows up in [`TransportStatus`]'s
//! `plugin_crashes`. Reloading the project into the player (`play`, `restart_plugins`, or
//! any edit while playing) re-creates every plugin, which restarts crashed ones.

use std::sync::Arc;

use cadenza_engine::{PluginCrash, PluginHosting};
use cadenza_model::Project;
use cadenza_plugin_host::PluginCatalog;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::McpError;
use crate::player::PlayerHandle;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct PlayArgs {
	/// Beat to start playback from; omit to continue from the current position.
	#[serde(default)]
	pub from_beat: Option<f64>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct TransportStatus {
	pub playing: bool,
	pub position_beats: f64,
	/// Plugins that crashed or hung during playback and are now bypassed (each with a
	/// message saying what to do). Call `restart_plugins` to reload them.
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub plugin_crashes: Vec<PluginCrash>,
	/// Problems found while (re)loading the project into the player.
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub warnings: Vec<String>,
}

fn status(player: &PlayerHandle, warnings: Vec<String>) -> Result<TransportStatus, McpError> {
	Ok(TransportStatus {
		playing: player.is_playing_hint(),
		position_beats: player.position_beats()?,
		plugin_crashes: player.plugin_crashes()?,
		warnings,
	})
}

fn stopped() -> TransportStatus {
	TransportStatus {
		playing: false,
		position_beats: 0.0,
		plugin_crashes: Vec::new(),
		warnings: Vec::new(),
	}
}

/// Reloads the current project into the player and starts (or resumes) playback.
pub fn play(
	player: &PlayerHandle,
	project: Project,
	plugins: Option<Arc<PluginCatalog>>,
	hosting: PluginHosting,
	args: PlayArgs,
) -> Result<TransportStatus, McpError> {
	let warnings = player.load(project, plugins, hosting)?;
	player.play(args.from_beat)?;
	status(player, warnings)
}

pub fn stop(player: Option<&PlayerHandle>) -> Result<TransportStatus, McpError> {
	match player {
		Some(player) => {
			player.stop()?;
			status(player, Vec::new())
		}
		// Nothing was ever playing, so there is nothing to stop.
		None => Ok(stopped()),
	}
}

pub fn transport_status(player: Option<&PlayerHandle>) -> Result<TransportStatus, McpError> {
	match player {
		Some(player) => status(player, Vec::new()),
		None => Ok(stopped()),
	}
}

/// Reloads the current project into the player without changing the transport, which
/// re-creates every plugin device (restarting crashed ones from the project's saved state).
pub fn restart_plugins(
	player: Option<&PlayerHandle>,
	project: Project,
	plugins: Option<Arc<PluginCatalog>>,
	hosting: PluginHosting,
) -> Result<TransportStatus, McpError> {
	match player {
		Some(player) => {
			let warnings = player.load(project, plugins, hosting)?;
			status(player, warnings)
		}
		// Plugins only crash while playing; renders create fresh instances every time.
		None => Ok(stopped()),
	}
}
