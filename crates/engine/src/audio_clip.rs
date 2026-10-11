//! Audio clips: WAV loading (at compile time) and sample-accurate playback.
//!
//! Files are converted once to the engine rate. Resampling uses 4-point cubic Hermite
//! (Catmull-Rom) interpolation; when downsampling, a 64-tap Blackman-windowed sinc low-pass
//! at 0.45 x the target rate is applied first to limit aliasing. That is transparent for
//! typical material but not mastering-grade (no polyphase/band-limited kernel).

use std::f64::consts::PI;
use std::path::Path;
use std::sync::Arc;

/// Decoded audio at the engine rate; one (mono) or two (stereo) channels.
#[derive(Debug)]
pub(crate) struct AudioData {
	pub channels: Vec<Vec<f32>>,
}

impl AudioData {
	pub fn is_stereo(&self) -> bool {
		self.channels.len() > 1
	}

	pub fn len(&self) -> usize {
		self.channels.first().map_or(0, Vec::len)
	}
}

/// Loads a WAV (8/16/24/32-bit int or 32-bit float) and converts it to `target_rate`.
/// Channels beyond the first two are ignored. Errors are human-readable warnings.
pub(crate) fn load_wav(path: &Path, target_rate: u32) -> Result<AudioData, String> {
	let mut reader = hound::WavReader::open(path)
		.map_err(|e| format!("cannot open audio file '{}': {e}", path.display()))?;
	let spec = reader.spec();
	let n_ch = spec.channels.max(1) as usize;
	let samples: Vec<f32> = match spec.sample_format {
		hound::SampleFormat::Float => reader
			.samples::<f32>()
			.collect::<Result<_, _>>()
			.map_err(|e| format!("cannot decode '{}': {e}", path.display()))?,
		hound::SampleFormat::Int => {
			let scale = 1.0 / (1u64 << (spec.bits_per_sample.clamp(1, 32) - 1)) as f32;
			reader
				.samples::<i32>()
				.map(|s| s.map(|v| v as f32 * scale))
				.collect::<Result<_, _>>()
				.map_err(|e| format!("cannot decode '{}': {e}", path.display()))?
		}
	};
	let keep = n_ch.min(2);
	let frames = samples.len() / n_ch;
	let mut channels: Vec<Vec<f32>> = (0..keep).map(|_| Vec::with_capacity(frames)).collect();
	for frame in samples.chunks_exact(n_ch) {
		for (c, ch) in channels.iter_mut().enumerate() {
			ch.push(frame[c]);
		}
	}
	if spec.sample_rate != target_rate && spec.sample_rate > 0 {
		for ch in &mut channels {
			*ch = resample(ch, spec.sample_rate, target_rate);
		}
	}
	Ok(AudioData { channels })
}

/// Converts `input` from `from` Hz to `to` Hz.
pub(crate) fn resample(input: &[f32], from: u32, to: u32) -> Vec<f32> {
	if from == to || input.is_empty() {
		return input.to_vec();
	}
	let filtered;
	let src = if to < from {
		filtered = lowpass(input, 0.45 * to as f64 / from as f64);
		&filtered[..]
	} else {
		input
	};
	let step = from as f64 / to as f64;
	let out_len = ((input.len() as f64) / step).ceil() as usize;
	let at = |i: isize| -> f32 {
		if i < 0 || i as usize >= src.len() {
			0.0
		} else {
			src[i as usize]
		}
	};
	(0..out_len)
		.map(|i| {
			let x = i as f64 * step;
			let idx = x.floor() as isize;
			let t = (x - idx as f64) as f32;
			hermite(at(idx - 1), at(idx), at(idx + 1), at(idx + 2), t)
		})
		.collect()
}

#[inline]
fn hermite(y0: f32, y1: f32, y2: f32, y3: f32, t: f32) -> f32 {
	let c1 = 0.5 * (y2 - y0);
	let c2 = y0 - 2.5 * y1 + 2.0 * y2 - 0.5 * y3;
	let c3 = 0.5 * (y3 - y0) + 1.5 * (y1 - y2);
	((c3 * t + c2) * t + c1) * t + y1
}

/// Zero-phase (centered) windowed-sinc low-pass; `cutoff` in cycles per sample.
fn lowpass(input: &[f32], cutoff: f64) -> Vec<f32> {
	const TAPS: usize = 64;
	let center = (TAPS - 1) as f64 / 2.0;
	let mut kernel: Vec<f64> = (0..TAPS)
		.map(|k| {
			let x = k as f64 - center;
			let sinc = if x == 0.0 {
				2.0 * cutoff
			} else {
				(2.0 * PI * cutoff * x).sin() / (PI * x)
			};
			let w = 0.42 - 0.5 * (2.0 * PI * k as f64 / (TAPS - 1) as f64).cos()
				+ 0.08 * (4.0 * PI * k as f64 / (TAPS - 1) as f64).cos();
			sinc * w
		})
		.collect();
	let sum: f64 = kernel.iter().sum();
	kernel.iter_mut().for_each(|k| *k /= sum);
	let half = TAPS as isize / 2;
	(0..input.len() as isize)
		.map(|n| {
			let mut acc = 0.0f64;
			for (k, &h) in kernel.iter().enumerate() {
				let i = n + half - k as isize;
				if i >= 0 && (i as usize) < input.len() {
					acc += h * input[i as usize] as f64;
				}
			}
			acc as f32
		})
		.collect()
}

/// One audio clip placed on the timeline (all positions in engine samples).
#[derive(Debug, Clone)]
pub(crate) struct ClipPlayback {
	pub data: Arc<AudioData>,
	pub start: i64,
	pub end: i64,
	/// Source sample played at `start`.
	pub offset: i64,
	pub gain: f32,
}

impl ClipPlayback {
	/// Adds the clip's contribution for the block starting at timeline sample `pos`.
	/// Mono files feed both channels.
	pub fn mix(&self, pos: i64, left: &mut [f32], right: &mut [f32]) {
		let n = left.len() as i64;
		let from = self.start.max(pos);
		let to = self.end.min(pos + n);
		if from >= to {
			return;
		}
		let len = self.data.len() as i64;
		let l = &self.data.channels[0];
		let r = self.data.channels.get(1).unwrap_or(l);
		for s in from..to {
			let src = s - self.start + self.offset;
			if src < 0 || src >= len {
				continue;
			}
			let i = (s - pos) as usize;
			left[i] += l[src as usize] * self.gain;
			right[i] += r[src as usize] * self.gain;
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn hermite_passes_through_points() {
		assert_eq!(hermite(0.0, 1.0, 2.0, 3.0, 0.0), 1.0);
		assert!((hermite(0.0, 1.0, 2.0, 3.0, 0.5) - 1.5).abs() < 1e-6);
	}

	#[test]
	fn resampled_sine_keeps_frequency_and_level() {
		let from = 44_100;
		let to = 48_000;
		let input: Vec<f32> = (0..from)
			.map(|i| (2.0 * PI * 1000.0 * i as f64 / from as f64).sin() as f32 * 0.5)
			.collect();
		let out = resample(&input, from, to);
		assert_eq!(out.len(), to as usize);
		let max_err = (100..out.len() - 100)
			.map(|i| {
				let want = (2.0 * PI * 1000.0 * i as f64 / to as f64).sin() as f32 * 0.5;
				(out[i] - want).abs()
			})
			.fold(0.0f32, f32::max);
		assert!(max_err < 2e-3, "max error {max_err}");
	}

	#[test]
	fn downsampling_attenuates_above_new_nyquist() {
		let from = 96_000;
		let input: Vec<f32> = (0..from)
			.map(|i| (2.0 * PI * 30_000.0 * i as f64 / from as f64).sin() as f32)
			.collect();
		let out = resample(&input, from, 48_000);
		let peak = out[1000..out.len() - 1000]
			.iter()
			.fold(0.0f32, |m, x| m.max(x.abs()));
		assert!(peak < 0.05, "alias peak {peak}");
	}
}
