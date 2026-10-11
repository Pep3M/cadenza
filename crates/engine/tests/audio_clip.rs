//! Audio clip playback: WAV formats, resampling, offset, length, gain, missing files.

mod common;

use cadenza_engine::{analyze_mix, render_offline};
use cadenza_model::{Clip, ClipContent, Track, TrackKind};
use common::*;
use hound::SampleFormat;

fn audio_track(clip: Clip) -> Track {
	let mut t = Track::new("audio", TrackKind::Audio);
	t.clips.push(clip);
	t
}

/// Counts upward zero crossings to estimate frequency.
fn frequency(x: &[f32], sr: f64) -> f64 {
	let crossings = x.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count();
	crossings as f64 * sr / x.len() as f64
}

#[test]
fn mono_44k_clip_is_resampled_positioned_and_panned() {
	let path = write_sine_wav(
		"mono44.wav",
		44_100,
		1,
		(SampleFormat::Int, 16),
		441.0,
		0.5,
		2.0,
	);
	// Clip at beat 1 (0.5 s), 1 beat long (0.5 s), starting 0.25 s into the file, -6 dB.
	let mut clip = Clip::audio("c", beats(1.0), beats(1.0), &path);
	clip.content = ClipContent::Audio {
		file: path,
		offset_seconds: 0.25,
		gain_db: -6.0,
	};
	let out = render_offline(&project(vec![audio_track(clip)]), &opts(4.0)).unwrap();
	assert!(
		out.left[..24_000].iter().all(|x| *x == 0.0),
		"silent before the clip"
	);
	assert!(
		out.left[48_000..].iter().all(|x| *x == 0.0),
		"silent after the clip end"
	);
	let body = &out.left[24_100..47_900];
	assert!((frequency(body, 48_000.0) - 441.0).abs() < 3.0);
	// 0.5 amplitude * -6 dB * -3 dB center pan law (mono source).
	let expected = 0.5 * cadenza_core::db_to_gain(-6.0) * std::f32::consts::FRAC_1_SQRT_2;
	assert!((peak(body) - expected).abs() < 0.01, "peak {}", peak(body));
	assert_eq!(out.left, out.right);
	// offset 0.25 s = 11025 source samples: the sine phase at the clip start is sin(2*pi*441*0.25).
	let phase = (2.0 * std::f64::consts::PI * 441.0 * 0.25).sin() as f32 * expected;
	assert!((out.left[24_000] - phase).abs() < 0.01);
}

#[test]
fn stereo_24bit_and_float_files_load() {
	for (name, format, bits) in [
		("s24.wav", SampleFormat::Int, 24),
		("s32.wav", SampleFormat::Int, 32),
		("f32.wav", SampleFormat::Float, 32),
	] {
		let path = write_sine_wav(name, 96_000, 2, (format, bits), 1_000.0, 0.25, 1.0);
		let clip = Clip::audio("c", beats(0.0), beats(2.0), path);
		let out = render_offline(&project(vec![audio_track(clip)]), &opts(2.0)).unwrap();
		let body = &out.right[1_000..47_000];
		// Stereo source: center pan is unity (balance law).
		assert!((peak(body) - 0.25).abs() < 0.005, "{name}: {}", peak(body));
		assert!((frequency(body, 48_000.0) - 1_000.0).abs() < 3.0, "{name}");
	}
}

#[test]
fn missing_file_warns_and_renders_silence() {
	let clip = Clip::audio("c", beats(0.0), beats(2.0), tmp_path("does_not_exist.wav"));
	let p = project(vec![audio_track(clip)]);
	let out = render_offline(&p, &opts(2.0)).unwrap();
	assert!(out.left.iter().all(|x| *x == 0.0));
	let report = analyze_mix(&p, &opts(2.0)).unwrap();
	assert_eq!(report.warnings.len(), 1);
	assert!(
		report.warnings[0].contains("does_not_exist.wav"),
		"{}",
		report.warnings[0]
	);
}
