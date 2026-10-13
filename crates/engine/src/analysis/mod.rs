//! Mix analysis of renders: per-track and master [`LevelStats`] (measured by
//! `cadenza-analysis`) plus the engine's report types.

pub use cadenza_analysis::{Analyzer, BandEnergy, LevelStats, bands, levels, loudness};

use cadenza_core::TrackId;
use cadenza_model::TrackKind;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Levels of one track's post-fader output (what it contributes to the mix).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TrackLevel {
	pub track_id: TrackId,
	pub name: String,
	/// Lets consumers (e.g. the AI's mix analysis) tell source tracks apart from bus returns,
	/// which legitimately sit far quieter or louder than the rest of the mix.
	pub kind: TrackKind,
	pub stats: LevelStats,
}

/// Result of a render/analysis: master levels, per-track levels and compile warnings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MixReport {
	pub sample_rate: u32,
	pub duration_seconds: f64,
	/// Plugin latency compensated by the engine (output is already time-aligned).
	pub latency_samples: u32,
	/// Problems found while building the graph (unknown devices, missing files...).
	pub warnings: Vec<String>,
	pub master: LevelStats,
	/// In project display order.
	pub tracks: Vec<TrackLevel>,
}
