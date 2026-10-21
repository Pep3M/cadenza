//! Modulators (LFO, envelope follower) move built-in parameters per quantum.

mod common;

use cadenza_core::ModulatorId;
use cadenza_engine::Player;
use cadenza_model::{
	Clip, Device, EnvelopeSource, LfoShape, ModTarget, Modulator, ModulatorKind, Project, Track,
	TrackKind,
};
use common::*;

const SR: u32 = 48_000;

fn sine_track(name: &str, start: f64, len: f64) -> Track {
	let wav = write_sine_wav(
		name,
		SR,
		2,
		(hound::SampleFormat::Float, 32),
		440.0,
		0.5,
		8.0,
	);
	let mut audio = Track::new("Audio", TrackKind::Audio);
	audio
		.clips
		.push(Clip::audio("sine", beats(start), beats(len), &wav));
	audio
}

fn square_lfo(rate_hz: f32, depth: f32) -> Modulator {
	Modulator {
		id: ModulatorId::new(),
		kind: ModulatorKind::Lfo {
			shape: LfoShape::Square,
			rate_hz,
			sync_beats: None,
			phase: 0.0,
			retrigger: Default::default(),
		},
		targets: vec![ModTarget {
			param: "gain".into(),
			depth,
		}],
	}
}

fn with_gain(mut track: Track, modulators: Vec<Modulator>) -> (Project, cadenza_core::DeviceId) {
	let mut gain = Device::builtin("gain", "Gain");
	gain.modulators = modulators;
	let id = gain.id;
	track.channel.inserts.push(gain);
	(project(vec![track]), id)
}

/// RMS of `x` in consecutive windows of `n` frames.
fn windows(x: &[f32], n: usize) -> Vec<f64> {
	x.chunks(n).map(rms).collect()
}

#[test]
fn an_lfo_moves_a_param_in_an_offline_render() {
	// 1 Hz square, depth 0.1 of the gain range (84 dB): +8.4 dB for half a second, then
	// -8.4 dB (120 bpm: 2 beats per second).
	let (p, _) = with_gain(
		sine_track("mod-lfo.wav", 0.0, 8.0),
		vec![square_lfo(1.0, 0.1)],
	);
	let buf = cadenza_engine::render_offline(&p, &opts(8.0)).unwrap();
	let w = windows(&buf.left, SR as usize / 4);
	let (dry, _) = with_gain(sine_track("mod-dry.wav", 0.0, 8.0), vec![]);
	let dry = rms(&cadenza_engine::render_offline(&dry, &opts(8.0))
		.unwrap()
		.left);
	let db = |v: f64| 20.0 * (v / dry).log10();
	// Windows of 250 ms: 0-1 high, 2-3 low, 4-5 high...
	for (i, v) in w.iter().take(12).enumerate().skip(1) {
		let expect = if (i / 2) % 2 == 0 { 8.4 } else { -8.4 };
		if i % 2 == 0 {
			// Window holding the edge (smoothing): skip.
			continue;
		}
		assert!(
			(db(*v) - expect).abs() < 0.5,
			"window {i}: {:.2} dB, want {expect}",
			db(*v)
		);
	}
}

#[test]
fn the_envelope_follower_follows_the_level() {
	// Silence for 2 beats, then the sine: the follower ducks the gain by up to 21 dB.
	let mut env = Modulator::envelope(ModulatorId::new());
	if let ModulatorKind::Envelope { source, .. } = &mut env.kind {
		*source = EnvelopeSource::OwnInput;
	}
	env.targets.push(ModTarget {
		param: "gain".into(),
		depth: -0.25,
	});
	let (p, gain_id) = with_gain(sine_track("mod-env.wav", 2.0, 6.0), vec![env]);
	let (dry, _) = with_gain(sine_track("mod-env-dry.wav", 2.0, 6.0), vec![]);
	let wet = cadenza_engine::render_offline(&p, &opts(8.0)).unwrap();
	let dry = cadenza_engine::render_offline(&dry, &opts(8.0)).unwrap();
	let tail = SR as usize * 2..SR as usize * 3;
	let ratio = rms(&wet.left[tail.clone()]) / rms(&dry.left[tail]);
	// The sine peaks at 0.5 (-6 dBFS): value 0.9 -> -0.25 * 0.9 * 84 = -18.9 dB.
	let db = 20.0 * ratio.log10();
	assert!((db + 18.9).abs() < 1.5, "ducked by {db:.1} dB");

	// Live: the probe publishes the follower's value (0 in the silence, ~0.9 later).
	let (mut player, mut dev) = Player::virtual_device(SR, 0);
	player.load(&p).unwrap();
	player.set_probe(Some(gain_id)).unwrap();
	let probe = player.probe().unwrap();
	player.play().unwrap();
	let mut out = vec![0.0f32; 2 * 512];
	for _ in 0..20 {
		dev.process(&mut out, &[]);
	}
	let (values, n) = probe.modulation();
	assert_eq!(n, 1);
	assert!(values[0] < 0.01, "silence: {}", values[0]);
	for _ in 0..150 {
		dev.process(&mut out, &[]);
	}
	let (values, _) = probe.modulation();
	assert!((values[0] - 0.9).abs() < 0.05, "sine: {}", values[0]);
}

#[test]
fn offline_render_matches_realtime_playback() {
	let mut sync = square_lfo(1.0, 0.05);
	if let ModulatorKind::Lfo {
		shape, sync_beats, ..
	} = &mut sync.kind
	{
		*shape = LfoShape::Sine;
		*sync_beats = Some(0.5);
	}
	let (p, _) = with_gain(
		sine_track("mod-match.wav", 0.0, 8.0),
		vec![square_lfo(3.0, 0.1), sync],
	);
	let offline = cadenza_engine::render_offline(&p, &opts(4.0)).unwrap();
	let (mut player, mut dev) = Player::virtual_device(SR, 0);
	player.load(&p).unwrap();
	player.play().unwrap();
	let mut live = Vec::new();
	let mut out = vec![0.0f32; 2 * 256];
	while live.len() < SR as usize {
		dev.process(&mut out, &[]);
		live.extend(out.chunks(2).map(|f| f[0]));
	}
	let n = SR as usize;
	let diff = offline.left[..n]
		.iter()
		.zip(&live[..n])
		.fold(0.0f32, |m, (a, b)| m.max((a - b).abs()));
	assert!(diff < 1e-5, "offline and live differ by {diff}");
}
