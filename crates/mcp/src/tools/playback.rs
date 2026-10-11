//! Realtime playback through the default audio output device, via the dedicated
//! [`crate::player::PlayerHandle`] thread.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::McpError;
use crate::player::PlayerHandle;
use cadenza_model::Project;

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
}

/// Reloads the current project into the player and starts (or resumes) playback.
pub fn play(
	player: &PlayerHandle,
	project: Project,
	plugins: Option<std::sync::Arc<cadenza_plugin_host::PluginCatalog>>,
	args: PlayArgs,
) -> Result<TransportStatus, McpError> {
	player.load(project, plugins)?;
	player.play(args.from_beat)?;
	Ok(TransportStatus {
		playing: true,
		position_beats: player.position_beats()?,
	})
}

pub fn stop(player: Option<&PlayerHandle>) -> Result<TransportStatus, McpError> {
	match player {
		Some(player) => {
			player.stop()?;
			Ok(TransportStatus {
				playing: false,
				position_beats: player.position_beats()?,
			})
		}
		// Nothing was ever playing, so there is nothing to stop.
		None => Ok(TransportStatus {
			playing: false,
			position_beats: 0.0,
		}),
	}
}

pub fn transport_status(player: Option<&PlayerHandle>) -> Result<TransportStatus, McpError> {
	match player {
		Some(player) => Ok(TransportStatus {
			playing: player.is_playing_hint(),
			position_beats: player.position_beats()?,
		}),
		None => Ok(TransportStatus {
			playing: false,
			position_beats: 0.0,
		}),
	}
}
