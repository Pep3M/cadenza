//! High-quality offline sample-rate conversion (rubato, band-limited sinc).

use rubato::audioadapter::Adapter;
use rubato::audioadapter_buffers::direct::SequentialSliceOfVecs;
use rubato::{Fft, FixedSync, Resampler, WindowFunction};

use crate::buffer::AudioBuffer;
use crate::error::{AudioIoError, Result};

/// Requested FFT block (rounded up by rubato to a multiple of the rate ratio's
/// minimal block). It is also the length of the windowed-sinc anti-aliasing filter.
const CHUNK: usize = 2048;

/// Converts `buffer` to `target_rate`, keeping all channels. Output length is
/// `ceil(frames * target / source)` and the filter delay is compensated, so the
/// result is time-aligned with the input.
///
/// Uses rubato's synchronous FFT resampler: the exact rational ratio of the two rates
/// with a ~2048-tap Blackman-Harris windowed-sinc anti-aliasing filter applied in the
/// frequency domain. Unlike the asynchronous sinc interpolator its delay is a whole
/// number of output frames, so the compensation is exact (no fractional-sample shift).
/// Alias rejection is beyond 100 dB and the passband is flat to about 0.98 x the lower
/// Nyquist frequency.
pub fn resample(buffer: &AudioBuffer, target_rate: u32) -> Result<AudioBuffer> {
	if target_rate == 0 {
		return Err(AudioIoError::InvalidArgument(
			"target sample rate must be greater than 0".into(),
		));
	}
	let source_rate = buffer.sample_rate();
	if source_rate == target_rate {
		return Ok(buffer.clone());
	}
	let n_ch = buffer.num_channels();
	let frames = buffer.frames();
	if frames == 0 {
		return AudioBuffer::silence(target_rate, n_ch, 0);
	}
	let mut resampler = Fft::<f32>::new_custom(
		source_rate as usize,
		target_rate as usize,
		CHUNK,
		1,
		n_ch,
		WindowFunction::BlackmanHarris2,
		FixedSync::Both,
	)
	.map_err(|e| AudioIoError::Resample(e.to_string()))?;
	let input = SequentialSliceOfVecs::new(buffer.channels(), n_ch, frames)
		.map_err(|e| AudioIoError::Resample(e.to_string()))?;
	let output = resampler
		.process_all(&input, frames, None)
		.map_err(|e| AudioIoError::Resample(e.to_string()))?;
	// Exact integer length; rubato's float ratio can overshoot by one frame.
	let out_frames = output
		.frames()
		.min((frames as u128 * u128::from(target_rate)).div_ceil(u128::from(source_rate)) as usize);
	let mut channels: Vec<Vec<f32>> = (0..n_ch).map(|_| Vec::with_capacity(out_frames)).collect();
	let data = output.take_data();
	for frame in data.chunks_exact(n_ch).take(out_frames) {
		for (ch, &s) in channels.iter_mut().zip(frame) {
			ch.push(s);
		}
	}
	AudioBuffer::from_planar(target_rate, channels)
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::f64::consts::PI;

	fn sine(rate: u32, freq: f64, amp: f64, frames: usize) -> AudioBuffer {
		let ch: Vec<f32> = (0..frames)
			.map(|i| (amp * (2.0 * PI * freq * i as f64 / rate as f64).sin()) as f32)
			.collect();
		AudioBuffer::from_planar(rate, vec![ch.clone(), ch]).unwrap()
	}

	/// Amplitude of the `freq` component via a single-bin DFT (Hann windowed).
	fn tone_amplitude(x: &[f32], rate: u32, freq: f64) -> f64 {
		let n = x.len();
		let (mut re, mut im, mut wsum) = (0.0, 0.0, 0.0);
		for (i, &s) in x.iter().enumerate() {
			let w = 0.5 - 0.5 * (2.0 * PI * i as f64 / (n - 1) as f64).cos();
			let ph = 2.0 * PI * freq * i as f64 / rate as f64;
			re += s as f64 * w * ph.cos();
			im -= s as f64 * w * ph.sin();
			wsum += w;
		}
		2.0 * (re * re + im * im).sqrt() / wsum
	}

	#[test]
	fn sine_44k1_to_48k_keeps_frequency_level_and_alignment() {
		let input = sine(44_100, 1000.0, 0.5, 44_100);
		let out = resample(&input, 48_000).unwrap();
		assert_eq!(out.sample_rate(), 48_000);
		assert_eq!(out.frames(), 48_000);
		// Sample-wise comparison against the ideal 48 kHz sine (also checks the delay
		// is compensated and the frequency preserved).
		let max_err = (500..out.frames() - 500)
			.map(|i| {
				let want = 0.5 * (2.0 * PI * 1000.0 * i as f64 / 48_000.0).sin();
				(out.channel(0)[i] as f64 - want).abs()
			})
			.fold(0.0f64, f64::max);
		// Residual vs. the ideal sine is about -110 dB.
		assert!(max_err < 1e-5, "max error {max_err}");
		let amp = tone_amplitude(&out.channel(1)[1000..47_000], 48_000, 1000.0);
		assert!((amp - 0.5).abs() < 0.005, "amplitude {amp}");
	}

	#[test]
	fn passband_is_flat_up_to_20khz() {
		let input = sine(44_100, 20_000.0, 0.5, 44_100);
		let out = resample(&input, 48_000).unwrap();
		let max_err = (3000..45_000)
			.map(|i| {
				let want = 0.5 * (2.0 * PI * 20_000.0 * i as f64 / 48_000.0).sin();
				(out.channel(0)[i] as f64 - want).abs()
			})
			.fold(0.0f64, f64::max);
		assert!(max_err < 1e-4, "max error {max_err}");
	}

	#[test]
	fn downsampling_rejects_content_above_new_nyquist() {
		// 30 kHz tone at 96 kHz would alias to 18 kHz at 48 kHz.
		let input = sine(96_000, 30_000.0, 1.0, 96_000);
		let out = resample(&input, 48_000).unwrap();
		let core = &out.channel(0)[2000..out.frames() - 2000];
		let alias = tone_amplitude(core, 48_000, 18_000.0);
		let alias_db = 20.0 * alias.max(1e-12).log10();
		assert!(alias_db < -90.0, "alias at {alias_db:.1} dB");
		let peak = core.iter().fold(0.0f32, |m, s| m.max(s.abs()));
		assert!(peak < 1e-3, "residual peak {peak}");
	}

	#[test]
	fn same_rate_is_identity() {
		let input = sine(48_000, 440.0, 0.3, 1000);
		assert_eq!(resample(&input, 48_000).unwrap(), input);
	}
}
