//! Cadenza project document: data model, the undoable [`Command`] API, the [`Document`]
//! undo/redo session, JSON persistence and the AI-oriented [`ProjectSummary`].
//!
//! Only this crate mutates [`Project`]; every other crate goes through [`Document::execute`].

mod apply;
pub mod audio_notes;
pub mod command;
pub mod controller;
pub mod document;
pub mod error;
pub mod files;
mod freeze;
pub mod links;
pub mod midi_map;
pub mod modulation;
mod persist;
pub mod project;
pub mod sample;
pub mod summary;
pub mod takes;
pub mod validate;

pub use audio_notes::{
	AudioNote, AudioNoteFlag, AudioNotePatch, AudioNotesSummary, NoteEdits, NoteSpan,
};
pub use command::{
	AudioClipPatch, ChannelPatch, ClipPatch, Command, NotePatch, SendPatch, TrackPatch,
};
pub use controller::{Controller, ControllerLane, ControllerPoint};
pub use document::Document;
pub use error::ModelError;
pub use files::{FileRef, FileUse, ProjectLayout};
pub use midi_map::{MapMode, MidiMapping, Takeover};
pub use modulation::{EnvelopeSource, LfoShape, ModTarget, Modulator, ModulatorKind, Retrigger};
pub use persist::{backup_path, write_atomic};
pub use project::{
	AutomationLane, AutomationPoint, Channel, Clip, ClipContent, ClipFade, Device, DeviceKind,
	FORMAT_VERSION, FadeCurve, Freeze, MAX_NOTE_BEND, MAX_NOTE_BEND_POINTS, Marker, MonitorMode,
	Note, NoteBendPoint, Output, OutputRoute, PointShape, Project, ProjectMeta, Send,
	SidechainSource, Take, Track, TrackInput, TrackKind, automation_value,
};
pub use sample::{LoopMode, SampleRef, SampleZone, root_from_file_name};
pub use summary::{
	ClipSummary, DeviceSummary, MarkerSummary, MasterSummary, ProjectSummary, SendSummary,
	TempoSummary, TimeSignatureSummary, TrackSummary, ZoneSummary, note_name, zone_summaries,
};

#[cfg(test)]
mod tests;
