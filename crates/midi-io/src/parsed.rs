//! Intermediate representation of a parsed standard MIDI file.
//!
//! Kept around after import so that data the Cadenza project model cannot represent yet
//! (CC lanes, pitch bend, program changes) is not silently dropped — only reported as a
//! warning. Callers that need it (future automation import, say) can inspect
//! [`ParsedMidiFile::other_events`].

use cadenza_core::{Tick, TimeSignature};

/// GM channel 10 (1-based), i.e. zero-based channel index 9.
pub const GM_DRUM_CHANNEL: u8 = 9;

#[derive(Debug, Clone, PartialEq)]
pub struct ParsedNote {
	pub channel: u8,
	pub pitch: u8,
	pub velocity: f32,
	/// Absolute start, already rescaled to Cadenza's PPQ.
	pub start: Tick,
	pub length: Tick,
}

/// A non-note channel event. Controllers, pitch bend and channel pressure become clip
/// controller lanes on import; program changes are only reported.
#[derive(Debug, Clone, PartialEq)]
pub enum OtherEvent {
	Controller {
		channel: u8,
		controller: u8,
		value: u8,
	},
	PitchBend {
		channel: u8,
		bend: i16,
	},
	ProgramChange {
		channel: u8,
		program: u8,
	},
	ChannelPressure {
		channel: u8,
		value: u8,
	},
}

impl OtherEvent {
	pub fn channel(&self) -> u8 {
		match *self {
			OtherEvent::Controller { channel, .. }
			| OtherEvent::PitchBend { channel, .. }
			| OtherEvent::ProgramChange { channel, .. }
			| OtherEvent::ChannelPressure { channel, .. } => channel,
		}
	}
}

#[derive(Debug, Clone, PartialEq)]
pub struct ParsedOtherEvent {
	pub tick: Tick,
	pub event: OtherEvent,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ParsedTrack {
	pub name: Option<String>,
	pub notes: Vec<ParsedNote>,
	/// True if any note or event on this track used the GM drum channel.
	pub is_drum: bool,
	pub other_events: Vec<ParsedOtherEvent>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ParsedTempoPoint {
	pub tick: Tick,
	pub bpm: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ParsedMarker {
	pub tick: Tick,
	pub name: String,
}

/// Everything extracted from a standard MIDI file, with ticks already rescaled to
/// [`cadenza_core::PPQ`] (960).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ParsedMidiFile {
	pub tracks: Vec<ParsedTrack>,
	pub tempo_map: Vec<ParsedTempoPoint>,
	/// The model has a single, non-time-varying time signature; this is every signature
	/// meta event found, in file order, so the caller can see if there was more than one.
	pub time_signatures: Vec<(Tick, TimeSignature)>,
	pub markers: Vec<ParsedMarker>,
	pub warnings: Vec<String>,
}
