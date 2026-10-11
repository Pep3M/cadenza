//! A full mini song renders cleanly and produces a sane report.

mod common;

use cadenza_engine::{FLOOR_DB, MixReport, RenderOptions, render_to_wav};
use common::*;

#[test]
fn mini_song_renders_to_wav_with_sane_report() {
	let p = mini_song();
	let path = tmp_path("mini_song.wav");
	let report = render_to_wav(&p, &RenderOptions::default(), &path).unwrap();
	assert!(report.warnings.is_empty(), "{:?}", report.warnings);

	let mut reader = hound::WavReader::open(&path).unwrap();
	let spec = reader.spec();
	assert_eq!(
		(spec.channels, spec.sample_rate, spec.bits_per_sample),
		(2, 48_000, 32)
	);
	assert_eq!(spec.sample_format, hound::SampleFormat::Float);
	let samples: Vec<f32> = reader.samples::<f32>().map(Result::unwrap).collect();
	assert!(samples.iter().all(|x| x.is_finite()), "no NaN/inf");
	let wav_peak = peak(&samples);
	assert!(wav_peak < 1.0 && wav_peak > 0.1, "peak {wav_peak}");

	// 8 bars at 120 bpm = 16 s, plus the reverb tail (capped at 10 s).
	assert!(report.duration_seconds > 16.0 && report.duration_seconds <= 26.0);
	assert_eq!(
		samples.len() / 2,
		(report.duration_seconds * 48_000.0).round() as usize
	);
	assert_eq!(report.latency_samples, 240);
	let m = &report.master;
	assert!(
		m.peak_db < -0.9 && m.peak_db > -20.0,
		"limiter ceiling -1 dB: {}",
		m.peak_db
	);
	assert!((m.peak_db - 20.0 * wav_peak.log10()).abs() < 1e-3);
	assert!(m.true_peak_db >= m.peak_db && m.true_peak_db < 0.5);
	assert!(
		m.lufs_integrated > -40.0 && m.lufs_integrated < -3.0,
		"{}",
		m.lufs_integrated
	);
	assert!(m.lufs_short_term_max >= m.lufs_integrated);
	assert!(m.crest_factor_db > 3.0 && m.crest_factor_db < 30.0);
	assert_eq!(m.clipped_samples, 0);
	assert!(m.stereo_correlation > 0.0 && m.stereo_correlation <= 1.0);
	assert!(
		m.band_energy_db.low > m.band_energy_db.high,
		"bass-heavy pop spectrum"
	);

	let names: Vec<&str> = report.tracks.iter().map(|t| t.name.as_str()).collect();
	assert_eq!(names, ["Drums", "Bass", "Chords", "Reverb"]);
	for t in &report.tracks {
		assert!(
			t.stats.peak_db > -60.0 && t.stats.peak_db < 6.0,
			"{}: {:?}",
			t.name,
			t.stats
		);
		assert!(t.stats.lufs_integrated > FLOOR_DB);
	}

	// The report is what the AI reads: it must round-trip through JSON.
	let json = serde_json::to_string(&report).unwrap();
	let back: MixReport = serde_json::from_str(&json).unwrap();
	assert_eq!(back, report);
	let schema = schemars::schema_for!(MixReport);
	assert!(
		serde_json::to_string(&schema)
			.unwrap()
			.contains("lufs_integrated")
	);
}
