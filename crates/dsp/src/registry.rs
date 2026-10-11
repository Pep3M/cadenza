//! Catalogue of built-in devices and their factory.

use crate::effects::{chorus, compressor, delay, eq, gain, limiter, reverb, saturator};
use crate::instruments::{drum_machine, electric_piano, poly_synth};
use crate::processor::{DeviceInfo, Processor};

static REGISTRY: [&DeviceInfo; 11] = [
	&poly_synth::INFO,
	&drum_machine::INFO,
	&electric_piano::INFO,
	&eq::INFO,
	&compressor::INFO,
	&limiter::INFO,
	&reverb::INFO,
	&delay::INFO,
	&chorus::INFO,
	&saturator::INFO,
	&gain::INFO,
];

/// Every built-in device, instruments first.
pub fn registry() -> &'static [&'static DeviceInfo] {
	&REGISTRY
}

/// Looks up a device description by its stable id.
pub fn device_info(id: &str) -> Option<&'static DeviceInfo> {
	REGISTRY.iter().copied().find(|d| d.id == id)
}

/// Instantiates a built-in device by id. Call `prepare` before `process`.
pub fn create(id: &str) -> Option<Box<dyn Processor>> {
	Some(match id {
		"poly_synth" => Box::new(poly_synth::PolySynth::new()),
		"drum_machine" => Box::new(drum_machine::DrumMachine::new()),
		"electric_piano" => Box::new(electric_piano::ElectricPiano::new()),
		"eq" => Box::new(eq::Eq::new()),
		"compressor" => Box::new(compressor::Compressor::new()),
		"limiter" => Box::new(limiter::Limiter::new()),
		"reverb" => Box::new(reverb::Reverb::new()),
		"delay" => Box::new(delay::Delay::new()),
		"chorus" => Box::new(chorus::Chorus::new()),
		"saturator" => Box::new(saturator::Saturator::new()),
		"gain" => Box::new(gain::Gain::new()),
		_ => return None,
	})
}
