//! Pluggable id generation for import, so callers can supply deterministic ids
//! (e.g. in tests, or to keep ids stable across a re-import) instead of random ones.

use cadenza_core::{ClipId, NoteId, TrackId};

pub trait IdAllocator {
	fn track_id(&mut self) -> TrackId;
	fn clip_id(&mut self) -> ClipId;
	fn note_id(&mut self) -> NoteId;
}

/// Generates fresh random (v4) ids, as most callers want.
#[derive(Debug, Default, Clone, Copy)]
pub struct RandomIds;

impl IdAllocator for RandomIds {
	fn track_id(&mut self) -> TrackId {
		TrackId::new()
	}

	fn clip_id(&mut self) -> ClipId {
		ClipId::new()
	}

	fn note_id(&mut self) -> NoteId {
		NoteId::new()
	}
}
