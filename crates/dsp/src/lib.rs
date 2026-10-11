//! Real-time DSP for Cadenza: the [`Processor`] contract plus a registry of
//! built-in instruments and effects, sufficient to produce a complete song
//! without third-party plugins.
//!
//! Every device is zero-allocation inside [`Processor::process`], smooths its
//! parameters, and renders sample-accurately around MIDI events.

pub mod effects;
pub mod instruments;
mod processor;
mod registry;
pub mod util;

pub use effects::compressor::Compressor;
pub use effects::limiter::Limiter;
pub use processor::{DeviceInfo, DeviceRole, MidiEvent, ParamInfo, Processor, Transport};
pub use registry::{create, device_info, registry};

#[cfg(test)]
mod tests;
