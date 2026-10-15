mod common;

use std::path::Path;

use cadenza_audio_io::{
	AudioFile, AudioFormat, AudioIoError, ExportOptions, FlacBitDepth, decode_file,
	decode_to_buffer, export, probe,
};
use common::*;

fn write_int_wav(path: &Path, bits: u16, channels: u16, rate: u32, frames: usize) {
	let spec = hound::WavSpec {
		channels,
		sample_rate: rate,
		bits_per_sample: bits,
		sample_format: hound::SampleFormat::Int,
	};
	let mut w = hound::WavWriter::create(path, spec).unwrap();
	let full = (1i64 << (bits - 1)) as f64;
	for i in 0..frames {
		let v = 0.5 * (2.0 * std::f64::consts::PI * 440.0 * i as f64 / rate as f64).sin();
		for c in 0..channels {
			// Second channel inverted to check channel order.
			let s = if c == 1 { -v } else { v };
			let q = (s * full).round() as i32;
			if bits == 8 {
				w.write_sample(q as i8).unwrap();
			} else {
				w.write_sample(q).unwrap();
			}
		}
	}
	w.finalize().unwrap();
}

#[test]
fn decodes_wav_at_every_common_bit_depth() {
	let dir = tempfile::tempdir().unwrap();
	for bits in [8u16, 16, 24, 32] {
		let p = dir.path().join(format!("s{bits}.wav"));
		write_int_wav(&p, bits, 2, 44_100, 4410);
		let info = probe(&p).unwrap();
		assert_eq!(info.format, AudioFormat::Wav);
		assert_eq!(
			(info.sample_rate, info.channels, info.frames),
			(44_100, 2, 4410)
		);
		assert_eq!(info.bits_per_sample, Some(u32::from(bits)));
		assert!((info.duration_seconds - 0.1).abs() < 1e-9);
		let buf = decode_file(&p).unwrap();
		assert_eq!(buf.frames(), 4410);
		let tol = if bits == 8 { 1e-2 } else { 1e-4 };
		for i in [0usize, 100, 1234, 4409] {
			let want = 0.5 * (2.0 * std::f64::consts::PI * 440.0 * i as f64 / 44_100.0).sin();
			assert!(
				(buf.channel(0)[i] as f64 - want).abs() < tol,
				"{bits} bit, frame {i}"
			);
			assert!(
				(buf.channel(1)[i] as f64 + want).abs() < tol,
				"{bits} bit, frame {i}"
			);
		}
	}
	// 32-bit float.
	let p = dir.path().join("f32.wav");
	let src = sine(48_000, 1000.0, 0.9, 4800, 1);
	write_float_wav(&p, &src);
	let buf = decode_file(&p).unwrap();
	assert_eq!(buf, src);
}

#[test]
fn decode_to_buffer_resamples() {
	let dir = tempfile::tempdir().unwrap();
	let p = dir.path().join("a.wav");
	write_float_wav(&p, &sine(44_100, 1000.0, 0.5, 44_100, 2));
	let buf = decode_to_buffer(&p, 48_000).unwrap();
	assert_eq!(buf.sample_rate(), 48_000);
	assert_eq!(buf.frames(), 48_000);
	let f = estimate_freq(&buf.channel(0)[1000..47_000], 48_000);
	assert!((f - 1000.0).abs() < 0.05, "{f}");
}

#[test]
fn multichannel_is_kept_and_folds_to_stereo() {
	let dir = tempfile::tempdir().unwrap();
	let p = dir.path().join("51.wav");
	write_int_wav(&p, 24, 6, 48_000, 480);
	let buf = decode_file(&p).unwrap();
	assert_eq!(buf.num_channels(), 6);
	assert_eq!(buf.to_stereo().num_channels(), 2);
}

#[test]
fn flac_roundtrip_is_lossless() {
	let dir = tempfile::tempdir().unwrap();
	let p = dir.path().join("a.flac");
	let src = sine(44_100, 440.0, 0.5, 30_000, 2);
	let opts = ExportOptions {
		dither: cadenza_audio_io::Dither::None,
		..ExportOptions::flac(FlacBitDepth::Bits24)
	};
	export(&src, &p, &opts).unwrap();
	let file = AudioFile::open(&p).unwrap();
	assert_eq!(file.info().format, AudioFormat::Flac);
	assert_eq!(file.info().frames, 30_000);
	assert_eq!(file.info().bits_per_sample, Some(24));
	let buf = file.decode().unwrap();
	let q = (1 << 23) as f32;
	for (a, b) in buf.channel(0).iter().zip(src.channel(0)) {
		assert_eq!(*a, (b * q).round() / q);
	}
}

#[test]
fn errors_are_actionable() {
	let dir = tempfile::tempdir().unwrap();
	let missing = dir.path().join("nope.wav");
	assert!(matches!(probe(&missing), Err(AudioIoError::Io { .. })));
	let junk = dir.path().join("junk.wav");
	std::fs::write(&junk, b"this is not audio at all, just some text").unwrap();
	let err = probe(&junk).unwrap_err();
	assert!(matches!(err, AudioIoError::Unsupported { .. }), "{err}");
	assert!(err.to_string().contains("Supported: WAV"), "{err}");
}

/// Compressed / non-WAV formats are produced with ffmpeg when it is installed.
fn check_ffmpeg_format(ext: &str, args: &[&str], format: AudioFormat, lossy: bool) {
	let dir = tempfile::tempdir().unwrap();
	let wav = dir.path().join("src.wav");
	let src = sine(44_100, 440.0, 0.5, 44_100, 2);
	write_float_wav(&wav, &src);
	let dst = dir.path().join(format!("out.{ext}"));
	if !ffmpeg_encode(&wav, &dst, args) {
		return;
	}
	let info = probe(&dst).unwrap();
	assert_eq!(info.format, format, "{ext}: {info:?}");
	assert_eq!((info.sample_rate, info.channels), (44_100, 2), "{ext}");
	let buf = decode_file(&dst).unwrap();
	assert_eq!(buf.frames() as u64, info.frames, "{ext}");
	// Gapless decoders give the exact length; allow a frame of slack otherwise.
	let slack = if lossy { 2048 } else { 0 };
	assert!(
		(buf.frames() as i64 - 44_100).unsigned_abs() <= slack,
		"{ext}: {} frames",
		buf.frames()
	);
	let mid = &buf.channel(0)[8000..36_000];
	let f = estimate_freq(mid, 44_100);
	assert!((f - 440.0).abs() < 0.5, "{ext}: {f} Hz");
	let level = rms(mid);
	assert!(
		(level - 0.5 / 2f64.sqrt()).abs() < 0.02,
		"{ext}: rms {level}"
	);
}

#[test]
fn decodes_aiff() {
	check_ffmpeg_format("aiff", &["-c:a", "pcm_s24be"], AudioFormat::Aiff, false);
}

#[test]
fn decodes_mp3() {
	check_ffmpeg_format(
		"mp3",
		&["-c:a", "libmp3lame", "-b:a", "192k"],
		AudioFormat::Mp3,
		true,
	);
}

#[test]
fn decodes_ogg_vorbis() {
	check_ffmpeg_format(
		"ogg",
		&["-c:a", "libvorbis", "-q:a", "6"],
		AudioFormat::OggVorbis,
		true,
	);
}

#[test]
fn decodes_m4a_aac() {
	check_ffmpeg_format(
		"m4a",
		&["-c:a", "aac", "-b:a", "192k"],
		AudioFormat::Aac,
		true,
	);
}

#[test]
fn decodes_m4a_alac() {
	check_ffmpeg_format("m4a", &["-c:a", "alac"], AudioFormat::Alac, false);
}

#[test]
fn decodes_ffmpeg_flac() {
	check_ffmpeg_format(
		"flac",
		&["-c:a", "flac", "-sample_fmt", "s16"],
		AudioFormat::Flac,
		false,
	);
}
