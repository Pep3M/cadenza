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
pub mod summary;
pub mod validate;

pub use command::{ChannelPatch, ClipPatch, Command, NotePatch, SendPatch, TrackPatch};
pub use document::Document;
pub use error::ModelError;
pub use project::{
	AutomationLane, AutomationPoint, Channel, Clip, ClipContent, Device, DeviceKind,
	FORMAT_VERSION, Marker, Note, Output, OutputRoute, Project, Send, SidechainSource, Track,
	TrackKind,
};
pub use summary::{
	ClipSummary, DeviceSummary, MarkerSummary, MasterSummary, ProjectSummary, SendSummary,
	TempoSummary, TrackSummary, note_name,
};

#[cfg(test)]
mod tests;
