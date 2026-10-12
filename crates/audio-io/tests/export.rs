mod common;

use cadenza_audio_io::{
	AudioBuffer, AudioFormat, Dither, ExportFormat, ExportOptions, FlacBitDepth, WavSampleFormat,
	decode_file, export, probe,
};
use common::*;

fn max_abs_diff(a: &AudioBuffer, b: &AudioBuffer) -> f32 {
	a.channels()
		.iter()
		.zip(b.channels())
		.flat_map(|(x, y)| x.iter().zip(y).map(|(p, q)| (p - q).abs()))
		.fold(0.0, f32::max)
}

#[test]
fn wav_exports_roundtrip_within_quantisation() {
	let dir = tempfile::tempdir().unwrap();
	let src = sine(48_000, 997.0, 0.7, 24_000, 2);
	for (fmt, bits, tol) in [
		(WavSampleFormat::Int16, 16, 2.5 / 32768.0),
		(WavSampleFormat::Int24, 24, 2.5 / 8_388_608.0),
		(WavSampleFormat::Float32, 32, 0.0),
	] {
		let p = dir.path().join(format!("{fmt:?}.wav"));
		export(&src, &p, &ExportOptions::wav(fmt)).unwrap();
		let info = probe(&p).unwrap();
		assert_eq!(info.format, AudioFormat::Wav);
		assert_eq!(info.bits_per_sample, Some(bits));
		assert_eq!(
			(info.frames, info.channels, info.sample_rate),
			(24_000, 2, 48_000)
		);
		let back = decode_file(&p).unwrap();
		let err = max_abs_diff(&back, &src);
		assert!(err <= tol, "{fmt:?}: error {err}");
	}
}

#[test]
fn float_wav_keeps_overs_and_int_clips() {
	let dir = tempfile::tempdir().unwrap();
	let src = AudioBuffer::from_planar(44_100, vec![vec![1.5, -2.0, 0.25]]).unwrap();
	let pf = dir.path().join("f.wav");
	export(&src, &pf, &ExportOptions::wav(WavSampleFormat::Float32)).unwrap();
	assert_eq!(decode_file(&pf).unwrap(), src);
	let pi = dir.path().join("i.wav");
	let opts = ExportOptions {
		dither: Dither::None,
		..ExportOptions::wav(WavSampleFormat::Int16)
	};
	export(&src, &pi, &opts).unwrap();
	let back = decode_file(&pi).unwrap();
	assert!((back.channel(0)[0] - 32767.0 / 32768.0).abs() < 1e-6);
	assert_eq!(back.channel(0)[1], -1.0);
}

#[test]
fn dithered_16_bit_export_has_expected_noise_floor() {
	let dir = tempfile::tempdir().unwrap();
	let silence = AudioBuffer::silence(44_100, 1, 44_100).unwrap();
	let p = dir.path().join("d.wav");
	export(&silence, &p, &ExportOptions::wav(WavSampleFormat::Int16)).unwrap();
	let back = decode_file(&p).unwrap();
	let level_db = 20.0 * rms(back.channel(0)).log10();
	// TPDF +/-1 LSB after rounding: RMS = sqrt(1/4) LSB -> about -96 dBFS.
	assert!((-99.0..-93.0).contains(&level_db), "{level_db} dBFS");
}

#[test]
fn flac_16_bit_export_decodes() {
	let dir = tempfile::tempdir().unwrap();
	let src = sine(44_100, 440.0, 0.5, 44_100, 1);
	let p = dir.path().join("a.flac");
	export(&src, &p, &ExportOptions::flac(FlacBitDepth::Bits16)).unwrap();
	let info = probe(&p).unwrap();
	assert_eq!(
		(info.format, info.bits_per_sample, info.channels),
		(AudioFormat::Flac, Some(16), 1)
	);
	let back = decode_file(&p).unwrap();
	assert!(max_abs_diff(&back, &src) <= 2.5 / 32768.0);
	// Lossless compression of a pure tone must beat raw PCM comfortably.
	let size = std::fs::metadata(&p).unwrap().len();
	assert!(size < 44_100 * 2 / 2, "{size} bytes");
}

#[cfg(feature = "mp3")]
#[test]
fn mp3_export_decodes_back() {
	let dir = tempfile::tempdir().unwrap();
	let src = sine(44_100, 440.0, 0.5, 44_100, 2);
	let p = dir.path().join("a.mp3");
	export(&src, &p, &ExportOptions::mp3(192)).unwrap();
	let info = probe(&p).unwrap();
	assert_eq!(info.format, AudioFormat::Mp3);
	let back = decode_file(&p).unwrap();
	assert!(back.frames() >= 44_100);
	let mid = &back.channel(1)[10_000..30_000];
	assert!((estimate_freq(mid, 44_100) - 440.0).abs() < 0.5);
	assert!((rms(mid) - 0.5 / 2f64.sqrt()).abs() < 0.02);
}

#[cfg(not(feature = "mp3"))]
#[test]
fn mp3_export_without_feature_explains_itself() {
	let dir = tempfile::tempdir().unwrap();
	let src = sine(44_100, 440.0, 0.5, 100, 2);
	let opts = ExportOptions {
		format: ExportFormat::Mp3 { bitrate_kbps: 192 },
		dither: Dither::None,
	};
	let err = export(&src, dir.path().join("a.mp3"), &opts).unwrap_err();
	assert!(err.to_string().contains("`mp3` feature"), "{err}");
}

#[test]
fn export_options_serialize() {
	let opts = ExportOptions::wav(WavSampleFormat::Int24);
	let json = serde_json::to_string(&opts).unwrap();
	assert!(json.contains("\"format\":\"wav\""), "{json}");
	let back: ExportOptions = serde_json::from_str(&json).unwrap();
	assert_eq!(back, opts);
	assert!(matches!(back.format, ExportFormat::Wav { .. }));
}
