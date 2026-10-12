#![cfg(feature = "stretch")]

mod common;

use cadenza_audio_io::{AudioIoError, pitch_shift, stretch_and_shift, time_stretch};
use common::*;

#[test]
fn time_stretch_changes_length_not_pitch() {
	let src = sine(44_100, 440.0, 0.5, 44_100, 2);
	for ratio in [0.5, 0.8, 1.25, 2.0] {
		let out = time_stretch(&src, ratio).unwrap();
		let want = (44_100.0 * ratio).round() as usize;
		assert_eq!(out.frames(), want, "ratio {ratio}");
		assert_eq!(out.num_channels(), 2);
		let mid = &out.channel(0)[want / 4..want * 3 / 4];
		let f = estimate_freq(mid, 44_100);
		assert!((f - 440.0).abs() < 2.0, "ratio {ratio}: {f} Hz");
		let level = rms(mid);
		assert!(
			(level - 0.5 / 2f64.sqrt()).abs() < 0.05,
			"ratio {ratio}: rms {level}"
		);
	}
}

#[test]
fn pitch_shift_keeps_length_and_transposes() {
	let src = sine(48_000, 440.0, 0.5, 48_000, 1);
	for (semis, want_hz) in [
		(12.0f32, 880.0),
		(-12.0, 220.0),
		(7.0, 440.0 * 2f64.powf(7.0 / 12.0)),
	] {
		let out = pitch_shift(&src, semis).unwrap();
		assert_eq!(out.frames(), 48_000);
		let f = estimate_freq(&out.channel(0)[12_000..36_000], 48_000);
		assert!((f - want_hz).abs() / want_hz < 0.005, "{semis} st: {f} Hz");
	}
}

#[test]
fn short_buffers_and_bad_arguments() {
	let tiny = sine(44_100, 440.0, 0.5, 64, 1);
	assert_eq!(time_stretch(&tiny, 1.5).unwrap().frames(), 96);
	assert!(matches!(
		time_stretch(&tiny, 0.0),
		Err(AudioIoError::InvalidArgument(_))
	));
	assert!(matches!(
		time_stretch(&tiny, f64::NAN),
		Err(AudioIoError::InvalidArgument(_))
	));
	assert!(matches!(
		pitch_shift(&tiny, 100.0),
		Err(AudioIoError::InvalidArgument(_))
	));
	assert_eq!(stretch_and_shift(&tiny, 2.0, 3.0).unwrap().frames(), 128);
}
