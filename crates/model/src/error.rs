use cadenza_core::{ClipId, DeviceId, NoteId, ParamAddress, Tick, TrackId};

use crate::project::TrackKind;

/// Every error the model can return. Messages are read by an LLM, so each one names the
/// offending id/value and says what would be accepted instead.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ModelError {
	#[error("track {0} does not exist; use an id from the project summary")]
	TrackNotFound(TrackId),
	#[error("clip {0} does not exist; use a clip id from the project summary")]
	ClipNotFound(ClipId),
	#[error("note {note} does not exist in clip {clip}")]
	NoteNotFound { clip: ClipId, note: NoteId },
	#[error(
		"device {device} does not exist on {location}; use the instrument or an insert id of that channel"
	)]
	DeviceNotFound { location: String, device: DeviceId },
	#[error("send {send} does not exist on track {track}")]
	SendNotFound { track: TrackId, send: DeviceId },
	#[error("no tempo point at tick {} (beat {}); existing points are listed in the project summary", .0.0, .0.beats())]
	TempoPointNotFound(Tick),
	#[error("no marker at tick {} (beat {})", .0.0, .0.beats())]
	MarkerNotFound(Tick),

	#[error("track id {0} is already used; generate a new unique id")]
	DuplicateTrackId(TrackId),
	#[error("clip id {0} is already used; generate a new unique id")]
	DuplicateClipId(ClipId),
	#[error("device/send id {0} is already used; generate a new unique id")]
	DuplicateDeviceId(DeviceId),
	#[error("note id {note} is used more than once in clip {clip}; generate a new unique id")]
	DuplicateNoteId { clip: ClipId, note: NoteId },
	#[error("a marker already exists at tick {} (beat {}); remove it first or pick another position", .0.0, .0.beats())]
	DuplicateMarker(Tick),

	#[error("pitch {0} is out of range; MIDI pitch must be 0..=127 (60 = C4)")]
	InvalidPitch(u8),
	#[error("velocity {0} is out of range; velocity must be 0.0..=1.0")]
	InvalidVelocity(f32),
	#[error("volume {0} dB is out of range; volume must be -144.0..=12.0 dB")]
	InvalidVolume(f32),
	#[error("pan {0} is out of range; pan must be -1.0 (left)..=1.0 (right)")]
	InvalidPan(f32),
	#[error("{what} length must be > 0 ticks (got {ticks})")]
	InvalidLength { what: &'static str, ticks: i64 },
	#[error("{what} start must be >= 0 ticks (got {ticks})")]
	NegativeStart { what: &'static str, ticks: i64 },
	#[error(
		"note {note} starts at tick {start} but clip {clip} is only {clip_length} ticks long; notes must start inside the clip (0 <= start < clip length)"
	)]
	NoteOutsideClip {
		clip: ClipId,
		note: NoteId,
		start: i64,
		clip_length: i64,
	},
	#[error("tempo {0} bpm is out of range; tempo must be 20..=999 bpm")]
	InvalidTempo(f64),
	#[error(
		"time signature {numerator}/{denominator} is invalid; numerator must be 1..=32 and denominator a power of two in 1..=32"
	)]
	InvalidTimeSignature { numerator: u32, denominator: u32 },
	#[error("the tempo point at tick 0 cannot be removed; change it with set_tempo instead")]
	CannotRemoveFirstTempoPoint,
	#[error("value {value} is invalid for {what}: {reason}")]
	InvalidValue {
		what: String,
		value: String,
		reason: String,
	},

	#[error("track {track} is a {kind:?} track; instruments can only be set on Instrument tracks")]
	InstrumentNotAllowed { track: TrackId, kind: TrackKind },
	#[error(
		"track {track} is a {kind:?} track; {content} clips are only allowed on {allowed} tracks"
	)]
	ClipKindMismatch {
		track: TrackId,
		kind: TrackKind,
		content: &'static str,
		allowed: &'static str,
	},
	#[error("clip {0} is an audio clip; note operations require a MIDI clip")]
	NotAMidiClip(ClipId),
	#[error("track {track} is a {kind:?} track; outputs and sends can only target Bus tracks")]
	NotABus { track: TrackId, kind: TrackKind },
	#[error(
		"routing {from} -> {to} would create a feedback cycle; a bus cannot feed (directly or indirectly) a track that feeds it"
	)]
	RoutingCycle { from: TrackId, to: TrackId },
	#[error("cannot key device {device} from that sidechain source: {reason}")]
	InvalidSidechain { device: DeviceId, reason: String },
	#[error("invalid instrument output route on track {track}: {reason}")]
	InvalidOutputRoute { track: TrackId, reason: String },
	#[error(
		"track {0} has no instrument; load one with set_instrument (create_track instrument) first"
	)]
	NoInstrument(TrackId),
	#[error("the master channel {0}")]
	MasterRestriction(&'static str),
	#[error("cannot move device {device} to index {index}; the chain has {len} devices (valid: 0..={})", .len.saturating_sub(1))]
	InvalidDeviceIndex {
		device: DeviceId,
		index: usize,
		len: usize,
	},
	#[error("index {index} is out of range for {what}; valid range is 0..={max}")]
	InvalidIndex {
		what: &'static str,
		index: usize,
		max: usize,
	},
	#[error("device {0} is the track instrument; remove or replace it with set_instrument")]
	DeviceIsInstrument(DeviceId),
	#[error("automation address {address} is invalid: {reason}")]
	InvalidAutomation {
		address: ParamAddress,
		reason: String,
	},
	#[error(
		"clip {clip} cannot be shortened to {length} ticks: {count} note(s) would start outside it; remove or move those notes first"
	)]
	NotesWouldStartOutsideClip {
		clip: ClipId,
		length: i64,
		count: usize,
	},

	#[error("batch '{label}' is empty; include at least one command")]
	EmptyBatch { label: String },
	#[error("batch '{label}' failed at command #{index} ({op}): {source}; no changes were applied")]
	BatchFailed {
		label: String,
		index: usize,
		op: String,
		source: Box<ModelError>,
	},
	#[error("nothing to undo")]
	NothingToUndo,
	#[error("nothing to redo")]
	NothingToRedo,

	#[error(
		"project format version {found} is not supported; this build reads versions 1..={supported}"
	)]
	UnsupportedFormatVersion { found: u64, supported: u32 },
	#[error("invalid project JSON: {0}")]
	Json(String),
	#[error("I/O error on '{path}': {message}")]
	Io { path: String, message: String },
}

impl ModelError {
	pub(crate) fn invalid(
		what: impl Into<String>,
		value: impl ToString,
		reason: impl Into<String>,
	) -> Self {
		Self::InvalidValue {
			what: what.into(),
			value: value.to_string(),
			reason: reason.into(),
		}
	}
}
