//! Catalogue of built-in devices and their factory.

use crate::effects::{
	chorus, compressor, delay, eq, gain, limiter, pitch_correct, reverb, saturator,
};
use crate::instruments::{drum_machine, drum_sampler, electric_piano, poly_synth, sampler};
use crate::processor::{DeviceInfo, Processor};

static REGISTRY: [&DeviceInfo; 14] = [
	&poly_synth::INFO,
	&drum_machine::INFO,
	&electric_piano::INFO,
	&sampler::INFO,
	&drum_sampler::INFO,
	&eq::INFO,
	&compressor::INFO,
	&limiter::INFO,
	&reverb::INFO,
	&delay::INFO,
	&chorus::INFO,
	&saturator::INFO,
	&gain::INFO,
	&pitch_correct::INFO,
];

/// Every built-in device, instruments first.
pub fn registry() -> &'static [&'static DeviceInfo] {
	&REGISTRY
}

/// Looks up a device description by its stable id.
pub fn device_info(id: &str) -> Option<&'static DeviceInfo> {
	REGISTRY.iter().copied().find(|d| d.id == id)
}

/// Whether changing this built-in param changes the device's `latency()`: hosts
/// must recompile (PDC) instead of sending the value live.
pub fn param_changes_latency(device: &str, param: &str) -> bool {
	matches!((device, param), ("limiter", "oversample"))
}

/// Whether the built-in effect `device` uses MIDI notes fed to it (a MIDI input from
/// another track): `pitch_correct` takes its target notes from them.
pub fn effect_takes_midi(device: &str) -> bool {
	device == "pitch_correct"
}

/// Instantiates a built-in device by id. Call `prepare` before `process`.
pub fn create(id: &str) -> Option<Box<dyn Processor>> {
	Some(match id {
		"poly_synth" => Box::new(poly_synth::PolySynth::new()),
		"drum_machine" => Box::new(drum_machine::DrumMachine::new()),
		"electric_piano" => Box::new(electric_piano::ElectricPiano::new()),
		"sampler" => Box::new(sampler::Sampler::new()),
		"drum_sampler" => Box::new(drum_sampler::DrumSampler::new()),
		"eq" => Box::new(eq::Eq::new()),
		"compressor" => Box::new(compressor::Compressor::new()),
		"limiter" => Box::new(limiter::Limiter::new()),
		"reverb" => Box::new(reverb::Reverb::new()),
		"delay" => Box::new(delay::Delay::new()),
		"chorus" => Box::new(chorus::Chorus::new()),
		"saturator" => Box::new(saturator::Saturator::new()),
		"gain" => Box::new(gain::Gain::new()),
		"pitch_correct" => Box::new(pitch_correct::PitchCorrect::new()),
		_ => return None,
	})
}
