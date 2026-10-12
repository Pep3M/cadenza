//! Cadenza audio engine: compiles a [`cadenza_model::Project`] into a render [`Graph`] and
//! drives it offline ([`render_offline`], [`render_to_wav`], [`render_to_file`]), for mix analysis
//! ([`analyze_mix`], the AI's "ears") and in realtime ([`Player`]).
//!
//! The graph always runs in fixed quanta of [`QUANTUM`] samples on a grid anchored at the
//! render/seek start, whatever block size the host asks for. Automation is evaluated once
//! per quantum, and the output is bit-identical for any host block size.

pub mod analysis;
mod audio_clip;
mod automation;
mod compile;
mod devices;
mod error;
mod graph;
pub mod hosting;
mod pdc;
mod player;
mod render;
mod schedule;

pub use analysis::{BandEnergy, LevelStats, MixReport, TrackLevel};
pub use cadenza_audio_io::{Dither, ExportFormat, ExportOptions, FlacBitDepth, WavSampleFormat};
pub use cadenza_plugin_host::PluginCatalog;
pub use cadenza_plugin_host::{CrashReason, HostingMode, SandboxOptions};
pub use compile::{compile, compile_hosted, compile_with};
pub use devices::{capture_state, plugin_presets, preset_state};
pub use error::EngineError;
pub use graph::Graph;
pub use hosting::{HOSTING_ENV, PluginCrash, PluginHosting};
pub use player::Player;
pub use render::{
	EXPORT_EXTENSIONS, MP3_SUPPORTED, RenderOptions, StereoBuffer, analyze_mix,
	export_options_for_path, render_offline, render_to_file, render_to_wav,
};

/// Internal processing quantum in samples: devices always process this many samples at a
/// time, and automation and transport changes are applied at quantum boundaries.
pub const QUANTUM: usize = 32;

/// Finite stand-in for -inf dB (silence) so reports stay valid JSON.
pub const FLOOR_DB: f32 = -144.0;

/// Upper bound on the effect tail appended to renders with `end: None`.
pub const MAX_TAIL_SECONDS: f32 = 10.0;
