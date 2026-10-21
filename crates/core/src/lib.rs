//! Shared vocabulary used by every Cadenza crate.

pub mod id;
pub mod meter;
pub mod param;
pub mod paths;
pub mod tempo;
pub mod time;
pub mod units;

pub use id::{ClipId, DeviceId, LinkId, ModulatorId, NoteId, TakeGroupId, TrackId};
pub use meter::{BarInfo, Meter, MeterChange, MeterMap, beat_ticks};
pub use param::{ParamAddress, ParamAddressError, ParamTarget};
pub use tempo::{TempoMap, TempoPoint, TimeSignature};
pub use time::{PPQ, Tick};
pub use units::{db_to_gain, gain_to_db};
