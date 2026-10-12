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
pub mod export;
pub mod factory;
mod graph;
pub mod hosting;
mod meter;
mod pdc;
mod player;
mod render;
mod samples;
mod schedule;

pub use analysis::{BandEnergy, LevelStats, MixReport, TrackLevel};
pub use automation::AutomationBindings;
pub use cadenza_audio_io::Metadata;
pub use cadenza_audio_io::{Dither, ExportFormat, ExportOptions, FlacBitDepth, WavSampleFormat};
pub use cadenza_midi_io::PortInfo as MidiPort;
pub use cadenza_plugin_host::PluginCatalog;
pub use cadenza_plugin_host::{CrashReason, HostingMode, SandboxOptions, SandboxStats};
pub use cadenza_plugin_host::{EDITORS_SUPPORTED, EditorEvent};
pub use compile::{compile, compile_hosted, compile_with};
pub use devices::{capture_state, plugin_presets, preset_state};
pub use error::EngineError;
pub use export::{
	ExportProgress, ExportReport, ExportSpec, ExportedFile, Normalize, StemSet, export_project,
	format_extension, normalize_gain, stem_tracks,
};
pub use graph::{Graph, ScheduleSwap};
pub use hosting::{HOSTING_ENV, PluginCrash, PluginHosting, PluginXruns};
pub use meter::{MeterBank, MeterReading};
pub use player::{
	AuditionScope, EditorState, EditorUpdate, Player, PlayerClock, find_midi_input,
	midi_input_ports,
};
pub use render::{
	EXPORT_EXTENSIONS, MP3_SUPPORTED, RenderOptions, StereoBuffer, analyze_mix,
	export_options_for_path, render_offline, render_to_file, render_to_wav,
};
pub use samples::{auto_zone, detect_root};

/// Internal processing quantum in samples: devices always process this many samples at a
/// time, and automation and transport changes are applied at quantum boundaries.
pub const QUANTUM: usize = 32;

/// Live MIDI events (monitoring) a graph buffers between two quanta; extra events in a
/// burst are dropped.
pub const LIVE_EVENT_CAPACITY: usize = 512;

/// How long a stopped graph keeps rendering the monitored instrument after the last live
/// note was released (on top of the project's longest device tail, see
/// [`crate::Graph::push_live_event`]).
pub const PREVIEW_RELEASE_SECONDS: f32 = 1.5;

/// Finite stand-in for -inf dB (silence) so reports stay valid JSON.
pub const FLOOR_DB: f32 = -144.0;

/// Upper bound on the effect tail appended to renders with `end: None`.
pub const MAX_TAIL_SECONDS: f32 = 10.0;
