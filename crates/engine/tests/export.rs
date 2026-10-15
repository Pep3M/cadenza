//! Rendering to WAV (16/24/32f), FLAC and MP3 chosen from the file extension.

mod common;

use cadenza_engine::{
	ExportFormat, FlacBitDepth, MP3_SUPPORTED, WavSampleFormat, export_options_for_path,
	render_offline, render_to_file,
};
use common::*;
use std::path::Path;

fn song() -> cadenza_model::Project {
	project(vec![synth_track(
		"lead",
		&[(60, 0.0, 2.0), (67, 1.0, 1.0)],
		2.0,
	)])
}

#[test]
fn format_follows_extension_and_bit_depth() {
	let f = |p: &str, d| export_options_for_path(Path::new(p), d, None).map(|o| o.format);
	assert_eq!(
		f("a.wav", None).unwrap(),
		ExportFormat::Wav {
			sample_format: WavSampleFormat::Float32
		}
	);
	assert_eq!(
		f("a.WAV", Some(16)).unwrap(),
		ExportFormat::Wav {
			sample_format: WavSampleFormat::Int16
		}
	);
	assert_eq!(
		f("a.flac", None).unwrap(),
		ExportFormat::Flac {
			bit_depth: FlacBitDepth::Bits24
		}
	);
	assert!(
		f("a.flac", Some(32))
			.unwrap_err()
			.to_string()
			.contains("16 or 24")
	);
	assert!(
		f("a.wav", Some(8))
			.unwrap_err()
			.to_string()
			.contains("16, 24 or 32")
	);
	assert!(
		f("a.ogg", None)
			.unwrap_err()
			.to_string()
			.contains(".wav, .flac or .mp3")
	);
	let mp3 = f("a.mp3", None);
	if MP3_SUPPORTED {
		assert_eq!(mp3.unwrap(), ExportFormat::Mp3 { bitrate_kbps: 320 });
	} else {
		assert!(mp3.unwrap_err().to_string().contains("mp3"));
	}
}

#[test]
fn every_format_round_trips_through_the_decoder() {
	let p = song();
	let reference = render_offline(&p, &opts(2.0)).unwrap();
	let mut cases = vec![
		("exp16.wav", Some(16), 1e-3),
		("exp24.wav", Some(24), 1e-5),
		("exp32.wav", None, 1e-7),
		("exp16.flac", Some(16), 1e-3),
		("exp24.flac", None, 1e-5),
	];
	if MP3_SUPPORTED {
		cases.push(("exp.mp3", None, f32::INFINITY));
	}
	for (name, depth, tolerance) in cases {
		let path = tmp_path(name);
		let opts_file = export_options_for_path(&path, depth, None).unwrap();
		let report = render_to_file(&p, &opts(2.0), &path, &opts_file).unwrap();
		assert!((report.duration_seconds - 1.0).abs() < 1e-6, "{name}");
		let decoded = cadenza_audio_io::decode_file(&path).unwrap();
		assert_eq!(decoded.sample_rate(), 48_000, "{name}");
		assert_eq!(decoded.num_channels(), 2, "{name}");
		if tolerance.is_finite() {
			assert_eq!(decoded.frames(), reference.len(), "{name}");
			let err = decoded
				.channel(0)
				.iter()
				.zip(&reference.left)
				.fold(0.0f32, |m, (a, b)| m.max((a - b).abs()));
			assert!(err < tolerance, "{name}: max error {err}");
		} else {
			assert!(decoded.peak() > 0.01, "{name}: decoded MP3 is silent");
		}
	}
}
