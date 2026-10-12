//! Cadenza project document: data model, the undoable [`Command`] API, the [`Document`]
//! undo/redo session, JSON persistence and the AI-oriented [`ProjectSummary`].
//!
//! Only this crate mutates [`Project`]; every other crate goes through [`Document::execute`].

mod apply;
pub mod command;
pub mod document;
pub mod error;
mod persist;
pub mod project;
pub mod sample;
pub mod summary;
pub mod validate;

pub use command::{
	AudioClipPatch, ChannelPatch, ClipPatch, Command, NotePatch, SendPatch, TrackPatch,
};
pub use document::Document;
pub use error::ModelError;
pub use project::{
	AutomationLane, AutomationPoint, Channel, Clip, ClipContent, ClipFade, Device, DeviceKind,
	FORMAT_VERSION, FadeCurve, Marker, Note, Output, OutputRoute, PointShape, Project, Send,
	SidechainSource, Track, TrackKind, automation_value,
};
pub use sample::{LoopMode, SampleRef, SampleZone, root_from_file_name};
pub use summary::{
	ClipSummary, DeviceSummary, MarkerSummary, MasterSummary, ProjectSummary, SendSummary,
	TempoSummary, TrackSummary, ZoneSummary, note_name, zone_summaries,
};

#[cfg(test)]
mod tests;
