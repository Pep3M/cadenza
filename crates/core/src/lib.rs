//! Shared vocabulary used by every Cadenza crate.

pub mod id;
pub mod param;
pub mod tempo;
pub mod time;
pub mod units;

pub use id::{ClipId, DeviceId, NoteId, TrackId};
pub use param::{ParamAddress, ParamAddressError, ParamTarget};
pub use tempo::{TempoMap, TempoPoint, TimeSignature};
pub use time::{PPQ, Tick};
pub use units::{db_to_gain, gain_to_db};
