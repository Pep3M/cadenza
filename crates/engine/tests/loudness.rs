//! BS.1770-4 loudness reference values.
//!
//! Expected value: a sine of peak amplitude A has mean square A^2/2 per channel. The
//! K-weighting gain at 997 Hz is +0.691 dB, which the -0.691 offset of the loudness formula
//! cancels, so for two channels L = 10*log10(2 * A^2/2) = 20*log10(A). A -20 dBFS-peak sine
//! (A = 0.1) on both channels therefore reads -20.0 LUFS (and a full-scale one 0 LUFS; one
//! channel alone would read -3.01 LUFS as in the spec's reference).

mod common;

use cadenza_engine::analysis::Analyzer;
use cadenza_engine::{RenderOptions, analyze_mix};
use cadenza_model::{Clip, Track, TrackKind};
use common::*;

fn sine(freq: f64, amp: f64, sr: u32, seconds: f64) -> Vec<f32> {
	(0..(sr as f64 * seconds) as usize)
		.map(|i| (amp * (2.0 * std::f64::consts::PI * freq * i as f64 / sr as f64).sin()) as f32)
		.collect()
}

#[test]
fn sine_997hz_at_minus_20_dbfs_reads_minus_20_lufs() {
	for sr in [44_100, 48_000, 96_000] {
		let x = sine(997.0, 0.1, sr, 10.0);
		let mut a = Analyzer::new(sr);
		a.push(&x, &x);
		let s = a.finish();
		assert!(
			(s.lufs_integrated + 20.0).abs() < 0.1,
			"{sr} Hz: {}",
			s.lufs_integrated
		);
		assert!(
			(s.lufs_short_term_max + 20.0).abs() < 0.1,
			"{sr} Hz: {}",
			s.lufs_short_term_max
		);
		assert!((s.peak_db + 20.0).abs() < 0.01);
		assert!((s.rms_db + 23.01).abs() < 0.02);
	}
	// One channel only: -3.01 LU lower.
	let x = sine(997.0, 1.0, 48_000, 5.0);
	let mut a = Analyzer::new(48_000);
	a.push(&x, &vec![0.0; x.len()]);
	assert!((a.finish().lufs_integrated + 3.01).abs() < 0.1);
}

#[test]
fn relative_gate_ignores_quiet_passages() {
	// 10 s at -20 LUFS followed by 10 s at -50 LUFS (below the -10 LU relative gate).
	let mut x = sine(997.0, 0.1, 48_000, 10.0);
	x.extend(sine(997.0, 0.1 * 10f64.powf(-30.0 / 20.0), 48_000, 10.0));
	let mut a = Analyzer::new(48_000);
	a.push(&x, &x);
	assert!((a.finish().lufs_integrated + 20.0).abs() < 0.2);
}

#[test]
fn engine_measures_wav_fixture_at_reference_level() {
	let path = write_sine_wav(
		"lufs_997.wav",
		48_000,
		2,
		(hound::SampleFormat::Float, 32),
		997.0,
		0.1,
		6.0,
	);
	let mut t = Track::new("tone", TrackKind::Audio);
	t.clips
		.push(Clip::audio("tone", beats(0.0), beats(12.0), path));
	let p = project(vec![t]);
	let report = analyze_mix(&p, &RenderOptions::default()).unwrap();
	assert!(report.warnings.is_empty(), "{:?}", report.warnings);
	assert!(
		(report.master.lufs_integrated + 20.0).abs() < 0.1,
		"{}",
		report.master.lufs_integrated
	);
	assert!((report.duration_seconds - 6.0).abs() < 1e-9);
	assert_eq!(report.tracks[0].stats, report.master);
}
