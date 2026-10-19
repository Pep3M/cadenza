//! The device probe: one device's input/output and readings reach the UI side.

mod common;

use cadenza_dsp::readings;
use cadenza_engine::{Player, ProbeFrame};
use cadenza_model::{Clip, Device, Track, TrackKind};
use common::*;

const SR: u32 = 48_000;

fn run(dev: &mut cadenza_engine::VirtualDevice, blocks: usize) {
	let mut out = vec![0.0f32; 2 * 512];
	for _ in 0..blocks {
		dev.process(&mut out, &[]);
	}
}

#[test]
fn probe_taps_a_gain_device_input_and_output() {
	let wav = write_sine_wav(
		"probe-sine.wav",
		SR,
		2,
		(hound::SampleFormat::Float, 32),
		440.0,
		0.5,
		2.0,
	);
	let mut audio = Track::new("Audio", TrackKind::Audio);
	audio
		.clips
		.push(Clip::audio("sine", beats(0.0), beats(4.0), &wav));
	let gain = Device::builtin("gain", "Gain").with_param("gain", -6.0);
	let gain_id = gain.id;
	audio.channel.inserts.push(gain);
	audio.channel.volume_db = -20.0;
	let p = project(vec![audio]);

	let (mut player, mut dev) = Player::virtual_device(SR, 0);
	assert!(player.probe().is_none());
	player.load(&p).unwrap();
	player.set_probe(Some(gain_id)).unwrap();
	let probe = player.probe().expect("probe while a device is probed");
	assert_eq!(probe.sample_rate(), SR);
	assert!(probe.capacity() >= SR as usize / 4);
	player.play().unwrap();

	let mut frames: Vec<ProbeFrame> = Vec::new();
	for _ in 0..40 {
		run(&mut dev, 1);
		probe.drain(&mut frames);
	}
	assert!(probe.is_active());
	assert_eq!(probe.dropped_frames(), 0);
	// 40 blocks of 512 frames, minus nothing: the probe sees every rendered quantum.
	assert!(frames.len() >= 40 * 512 - 64, "{} frames", frames.len());
	let g = 10f32.powf(-6.0 / 20.0);
	let tail = &frames[frames.len() / 2..];
	let in_peak = tail.iter().fold(0.0f32, |m, f| m.max(f.input[0].abs()));
	assert!((in_peak - 0.5).abs() < 0.01, "input peak {in_peak}");
	for f in tail {
		for c in 0..2 {
			assert!(
				(f.output[c] - f.input[c] * g).abs() < 1e-4,
				"{f:?} (gain {g})"
			);
		}
	}
	// Gain publishes no readings.
	assert_eq!(probe.readings().1, 0);

	// A reload keeps following the device.
	player.load(&p).unwrap();
	frames.clear();
	run(&mut dev, 4);
	probe.drain(&mut frames);
	assert!(!frames.is_empty());
	assert!(probe.is_active());

	// A device that is not in the graph: idle.
	player
		.set_probe(Some(cadenza_core::DeviceId::new()))
		.unwrap();
	run(&mut dev, 2);
	frames.clear();
	probe.drain(&mut frames);
	assert!(frames.is_empty(), "{} stale frames", frames.len());
	assert!(!probe.is_active());

	player.set_probe(None).unwrap();
	assert!(player.probe().is_none());
}

#[test]
fn probe_reads_compressor_gain_reduction_on_the_master_and_instruments() {
	let notes: Vec<(u8, f64, f64)> = (0..4).map(|i| (48 + i * 4, 0.0, 8.0)).collect();
	let mut synth = synth_track("Synth", &notes, 8.0);
	let instrument = synth.instrument.as_ref().unwrap().id;
	synth.channel.volume_db = 0.0;
	let mut p = project(vec![synth]);
	let comp = Device::builtin("compressor", "Comp")
		.with_param("threshold", -50.0)
		.with_param("ratio", 10.0)
		.with_param("attack", 0.1);
	let comp_id = comp.id;
	p.master.inserts.insert(0, comp);

	let (mut player, mut dev) = Player::virtual_device(SR, 0);
	player.load(&p).unwrap();
	player.set_probe(Some(comp_id)).unwrap();
	let probe = player.probe().unwrap();
	player.play().unwrap();
	let mut frames = Vec::new();
	for _ in 0..20 {
		run(&mut dev, 1);
		probe.drain(&mut frames);
	}
	assert!(probe.is_active());
	let gr = probe
		.reading(readings::GAIN_REDUCTION_DB)
		.expect("compressor publishes gain reduction");
	assert!(gr > 1.0, "gain reduction {gr} dB");

	// The track's instrument: silent input, sounding output, no readings.
	player.set_probe(Some(instrument)).unwrap();
	run(&mut dev, 4);
	frames.clear();
	probe.drain(&mut frames);
	assert!(!frames.is_empty());
	assert!(frames.iter().all(|f| f.input == [0.0; 2]));
	assert!(frames.iter().any(|f| f.output[0].abs() > 1e-3));
	assert_eq!(probe.reading(readings::GAIN_REDUCTION_DB), None);
}
