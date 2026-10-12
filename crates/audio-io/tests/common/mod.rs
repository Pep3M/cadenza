#![allow(dead_code)]

use std::f64::consts::PI;
use std::path::Path;
use std::process::Command;

use cadenza_audio_io::AudioBuffer;

pub fn sine(rate: u32, freq: f64, amp: f64, frames: usize, channels: usize) -> AudioBuffer {
	let ch: Vec<f32> = (0..frames)
		.map(|i| (amp * (2.0 * PI * freq * i as f64 / rate as f64).sin()) as f32)
		.collect();
	AudioBuffer::from_planar(rate, vec![ch; channels]).unwrap()
}

/// Frequency estimate from positive-going zero crossings (linear interpolation).
pub fn estimate_freq(x: &[f32], rate: u32) -> f64 {
	let mut crossings = Vec::new();
	for i in 1..x.len() {
		if x[i - 1] < 0.0 && x[i] >= 0.0 {
			let t = x[i - 1] as f64 / (x[i - 1] as f64 - x[i] as f64);
			crossings.push(i as f64 - 1.0 + t);
		}
	}
	let n = crossings.len();
	assert!(n >= 2, "not enough zero crossings");
	(n - 1) as f64 * rate as f64 / (crossings[n - 1] - crossings[0])
}

pub fn rms(x: &[f32]) -> f64 {
	(x.iter().map(|&s| (s as f64).powi(2)).sum::<f64>() / x.len() as f64).sqrt()
}

pub fn ffmpeg_available() -> bool {
	Command::new("ffmpeg")
		.arg("-version")
		.output()
		.is_ok_and(|o| o.status.success())
}

/// Encodes `src` into `dst` with ffmpeg (format chosen from the extension plus `args`).
/// Returns false when ffmpeg is missing or cannot produce that format.
pub fn ffmpeg_encode(src: &Path, dst: &Path, args: &[&str]) -> bool {
	if !ffmpeg_available() {
		eprintln!("ffmpeg not found; skipping");
		return false;
	}
	let out = Command::new("ffmpeg")
		.args(["-hide_banner", "-loglevel", "error", "-y", "-i"])
		.arg(src)
		.args(args)
		.arg(dst)
		.output()
		.expect("run ffmpeg");
	if !out.status.success() {
		eprintln!(
			"ffmpeg could not encode {}: {}; skipping",
			dst.display(),
			String::from_utf8_lossy(&out.stderr)
		);
		return false;
	}
	true
}

pub fn write_float_wav(path: &Path, buffer: &AudioBuffer) {
	let spec = hound::WavSpec {
		channels: buffer.num_channels() as u16,
		sample_rate: buffer.sample_rate(),
		bits_per_sample: 32,
		sample_format: hound::SampleFormat::Float,
	};
	let mut w = hound::WavWriter::create(path, spec).unwrap();
	for s in buffer.to_interleaved() {
		w.write_sample(s).unwrap();
	}
	w.finalize().unwrap();
}
