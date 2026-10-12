//! MIDI I/O for Cadenza: standard MIDI file import/export, hardware MIDI input, and a
//! recording helper.
//!
//! Import turns a `.mid` file into an undoable [`cadenza_model::Command::Batch`] (new
//! instrument tracks + clips, or a clip dropped into an existing track), so it goes
//! through the same `Document::execute` path as every other project mutation. Data the
//! project model has no place for yet (control changes, pitch bend, program changes) is
//! kept in [`parsed::ParsedMidiFile::tracks`]`[_].other_events` and reported as a
//! warning rather than silently dropped.
//!
//! Hardware input (`midir`) and the recording helper are deliberately split: the
//! `midir` callback thread only pushes raw, timestamped bytes into a lock-free SPSC
//! queue ([`input::new_queue`]); turning those into `cadenza_dsp::MidiEvent`s with
//! sample-accurate offsets ([`input::drain_into_block`]) is safe to call from a
//! realtime callback (no allocation, no locks). Recording ([`record::Recorder`]) runs
//! off the audio thread and is free to allocate.

pub mod error;
pub mod ids;
pub mod input;
pub mod parsed;
pub mod raw;
pub mod record;
pub mod smf_export;
pub mod smf_import;

pub use error::{MidiIoError, Result};
pub use ids::{IdAllocator, RandomIds};
pub use input::{
	MappedEvent, MidiInputConnection, PortInfo, TimedMessage, drain_into_block, list_input_ports,
	new_queue, open_input_port,
};
pub use parsed::{
	GM_DRUM_CHANNEL, OtherEvent, ParsedMarker, ParsedMidiFile, ParsedNote, ParsedOtherEvent,
	ParsedTempoPoint, ParsedTrack,
};
pub use raw::{RawMessage, RunningStatusDecoder};
pub use record::Recorder;
pub use smf_export::{export_clip, export_project};
pub use smf_import::{
	ImportOptions, ImportResult, ImportTarget, build_import_command, import_smf,
	import_smf_with_ids, parse_smf,
};

#[cfg(test)]
mod tests;
