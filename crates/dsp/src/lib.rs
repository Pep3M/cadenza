//! Real-time DSP for Cadenza: the [`Processor`] contract plus a registry of
//! built-in instruments and effects, sufficient to produce a complete song
//! without third-party plugins.
//!
//! Every device is zero-allocation inside [`Processor::process`], smooths its
//! parameters, and renders sample-accurately around MIDI events.

pub mod effects;
pub mod instruments;
mod presets;
mod processor;
mod registry;
pub mod response;
pub mod sample;
pub mod util;

pub use effects::compressor::Compressor;
pub use effects::limiter::Limiter;
pub use presets::{
	FactoryPreset, FactoryZone, PRESET_CATEGORIES, PresetLfoShape, PresetModSource,
	PresetModulator, PresetRetrigger, factory_presets,
};
/// File names of the factory samples the built-in presets use.
pub mod factory_samples {
	pub use crate::presets::{
		CLAP, CLOSED_HAT, CRASH, HIGH_TOM, KICK, LOW_TOM, MID_TOM, OPEN_HAT, RIDE, RIM, SINE_PAD,
		SINE_PAD_LOOP, SNARE,
	};
}
pub use processor::{
	DeviceInfo, DeviceRole, Listen, MidiEvent, NoteExpressionKind, ParamInfo, PresetInfo,
	Processor, Transport, cc, readings,
};
pub use registry::{create, device_info, effect_takes_midi, param_changes_latency, registry};
pub use sample::{LoopMode, SampleBuffer, SampleData, Zone, uses_samples};

#[cfg(test)]
mod tests;
