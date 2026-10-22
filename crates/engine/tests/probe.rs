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

	// The track's instrument: silent input, sounding output, its voice readings.
	player.set_probe(Some(instrument)).unwrap();
	run(&mut dev, 4);
	frames.clear();
	probe.drain(&mut frames);
	assert!(!frames.is_empty());
	assert!(frames.iter().all(|f| f.input == [0.0; 2]));
	assert!(frames.iter().any(|f| f.output[0].abs() > 1e-3));
	assert_eq!(probe.readings().1, readings::VOICES_END);
}

#[test]
fn probe_carries_held_keys_and_voices_from_playback_and_previews() {
	use cadenza_dsp::instruments::voice_readings::{HeldNotes, decode_voices};
	let notes: Vec<(u8, f64, f64)> = vec![(60, 0.0, 8.0), (64, 0.0, 8.0)];
	let synth = synth_track("Synth", &notes, 8.0);
	let track = synth.id;
	let instrument = synth.instrument.as_ref().unwrap().id;
	let p = project(vec![synth]);
	let (mut player, mut dev) = Player::virtual_device(SR, 0);
	player.load(&p).unwrap();
	player.set_probe(Some(instrument)).unwrap();
	let probe = player.probe().unwrap();
	player.play().unwrap();
	run(&mut dev, 10);
	let (values, n) = probe.readings();
	let held = HeldNotes::decode(&values[..n]);
	assert_eq!(held.iter().collect::<Vec<_>>(), vec![60, 64]);
	let voices: Vec<_> = decode_voices(&values[..n]).collect();
	assert_eq!(voices.len(), 2, "{voices:?}");
	assert!(voices.iter().all(|v| !v.released && v.elapsed_ms > 50.0));

	// Stopped: a preview note is what the instrument holds.
	player.stop().unwrap();
	run(&mut dev, 20);
	assert!(
		!probe.is_active(),
		"a stopped graph runs nothing: stale readings"
	);
	player.preview_note_on(track, 72, 0.8).unwrap();
	run(&mut dev, 4);
	let (values, n) = probe.readings();
	assert_eq!(
		HeldNotes::decode(&values[..n]).iter().collect::<Vec<_>>(),
		vec![72]
	);
	assert_eq!(decode_voices(&values[..n]).next().map(|v| v.note), Some(72));
	assert!(probe.is_active());
	player.preview_note_off(track, 72).unwrap();
	run(&mut dev, 2);
	let (values, n) = probe.readings();
	assert!(!HeldNotes::decode(&values[..n]).any());
}

fn left_sine_project(name: &str) -> cadenza_model::Project {
	let wav = write_sine_wav(
		name,
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
	audio.channel.volume_db = 0.0;
	audio.channel.pan = -1.0;
	project(vec![audio])
}

/// Peaks of the second half of an interleaved stereo stream.
fn out_peaks(out: &[f32]) -> [f32; 2] {
	let tail = &out[out.len() / 2..];
	let mut p = [0.0f32; 2];
	for f in tail.chunks(2) {
		p[0] = p[0].max(f[0].abs());
		p[1] = p[1].max(f[1].abs());
	}
	p
}

#[test]
fn master_tap_shows_the_mix_and_monitor_switches_only_change_the_output() {
	let p = left_sine_project("master-tap-sine.wav");
	let (mut player, mut dev) = Player::virtual_device(SR, 0);
	player.load(&p).unwrap();
	let tap = player.master_tap();
	player.play().unwrap();
	let mut out = vec![0.0f32; 2 * 512];
	let mut heard = Vec::new();
	let mut frames: Vec<ProbeFrame> = Vec::new();
	for _ in 0..20 {
		dev.process(&mut out, &[]);
		heard.extend_from_slice(&out);
		tap.drain(&mut frames);
	}
	assert!(tap.is_active());
	assert!(frames.len() >= 20 * 512 - 64, "{} frames", frames.len());
	let tapped = |frames: &[ProbeFrame]| {
		frames[frames.len() / 2..].iter().fold([0.0f32; 2], |m, f| {
			[m[0].max(f.output[0].abs()), m[1].max(f.output[1].abs())]
		})
	};
	let dry = out_peaks(&heard);
	let t = tapped(&frames);
	assert!(dry[0] > 0.3 && dry[1] < 1e-6, "hard left: {dry:?}");
	assert!(
		(t[0] - dry[0]).abs() < 1e-3 && t[1] < 1e-6,
		"{t:?} vs {dry:?}"
	);

	// Mono + dim: the output is summed and 20 dB down, the tap (and meters) are not.
	player
		.set_monitor(cadenza_engine::MonitorFx {
			mono: true,
			dim: true,
		})
		.unwrap();
	heard.clear();
	frames.clear();
	for _ in 0..20 {
		dev.process(&mut out, &[]);
		heard.extend_from_slice(&out);
		tap.drain(&mut frames);
	}
	assert!(heard.chunks(2).all(|f| f[0] == f[1]), "mono");
	let wet = out_peaks(&heard);
	let expected = 0.5 * dry[0] * 10f32.powf(cadenza_engine::DIM_DB / 20.0);
	assert!((wet[0] - expected).abs() < 1e-3, "{wet:?} vs {expected}");
	let t = tapped(&frames);
	assert!(
		(t[0] - dry[0]).abs() < 1e-3 && t[1] < 1e-6,
		"tap unchanged: {t:?}"
	);

	// Kept across loads.
	player.load(&p).unwrap();
	heard.clear();
	for _ in 0..8 {
		dev.process(&mut out, &[]);
		heard.extend_from_slice(&out);
	}
	assert!(
		heard.chunks(2).all(|f| f[0] == f[1]),
		"still mono after a load"
	);
	assert!(player.monitor_fx().dim);

	// Offline renders never go through the monitor switches.
	let buf = cadenza_engine::render_offline(&p, &opts(4.0)).unwrap();
	assert!(peak(&buf.left) > 0.3 && peak(&buf.right) < 1e-6);
}

#[test]
fn compressor_publishes_its_key_level() {
	let notes: Vec<(u8, f64, f64)> = (0..4).map(|i| (48 + i * 4, 0.0, 8.0)).collect();
	let synth = synth_track("Synth", &notes, 8.0);
	let synth_id = synth.id;
	let mut p = left_sine_project("key-level-sine.wav");
	let comp = Device::builtin("compressor", "Comp").with_sidechain(synth_id, true);
	let comp_id = comp.id;
	p.tracks[0].channel.inserts.push(comp);
	p.tracks.push(synth);
	let (mut player, mut dev) = Player::virtual_device(SR, 0);
	player.load(&p).unwrap();
	player.set_probe(Some(comp_id)).unwrap();
	let probe = player.probe().unwrap();
	player.play().unwrap();
	run(&mut dev, 20);
	let key = probe.reading(readings::KEY_LEVEL_DB).expect("key level");
	assert!(key > -40.0 && key < 6.0, "key level {key} dB");
}

#[test]
fn listen_changes_only_the_live_output_and_survives_loads() {
	let mut p = left_sine_project("listen-sine.wav");
	// The high-pass band's listen plays what it cuts (below 30 Hz): the 440 Hz sine
	// all but disappears.
	let eq = Device::builtin("eq", "EQ");
	let eq_id = eq.id;
	p.tracks[0].channel.inserts.push(eq);
	let (mut player, mut dev) = Player::virtual_device(SR, 0);
	player.load(&p).unwrap();
	player.play().unwrap();
	let mut out = vec![0.0f32; 2 * 512];
	let mut listen = |dev: &mut cadenza_engine::VirtualDevice| {
		let mut heard = Vec::new();
		for _ in 0..20 {
			dev.process(&mut out, &[]);
			heard.extend_from_slice(&out);
		}
		out_peaks(&heard)[0]
	};
	let normal = listen(&mut dev);
	assert!(normal > 0.3, "{normal}");
	player
		.set_listen(eq_id, cadenza_engine::Listen::Band(0))
		.unwrap();
	let solo = listen(&mut dev);
	assert!(solo < normal * 0.01, "{solo} vs {normal}");
	player.load(&p).unwrap();
	assert!(listen(&mut dev) < normal * 0.01, "kept across loads");
	assert_eq!(
		player.listen(),
		Some((eq_id, cadenza_engine::Listen::Band(0)))
	);
	// Offline renders never listen.
	let buf = cadenza_engine::render_offline(&p, &opts(4.0)).unwrap();
	assert!(peak(&buf.left) > 0.3, "offline: {}", peak(&buf.left));
	player
		.set_listen(eq_id, cadenza_engine::Listen::Off)
		.unwrap();
	assert!(listen(&mut dev) > 0.3);
	assert_eq!(player.listen(), None);
}
