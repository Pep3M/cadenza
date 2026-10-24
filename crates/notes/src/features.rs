//! Short-window spectral features on the same frame grid as the pitch track: level,
//! spectral flux (onsets) and high-frequency share (sibilants, breaths). The 23 ms window is
//! half the pitch window, so these see attacks and consonants sharply.

use realfft::RealFftPlanner;
use realfft::num_complex::Complex;

use crate::par::map_frames;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpectralFrame {
	/// RMS of the windowed frame, dBFS (-120 floor).
	pub level_db: f64,
	/// Rise of log magnitudes from the previous frame (half-wave rectified, mean per bin).
	pub flux: f64,
	/// Energy above 4 kHz / total energy, 0..1 (0 when the rate cannot show 4 kHz).
	pub hf_ratio: f64,
}

/// Frames centered on `index * hop` samples, `count` of them.
pub fn spectral_frames(
	samples: &[f32],
	sample_rate: u32,
	hop: usize,
	count: usize,
	progress: &mut dyn FnMut(f64) -> bool,
) -> Option<Vec<SpectralFrame>> {
	let sr = f64::from(sample_rate.max(1));
	let size = ((sr * 0.023) as usize).next_power_of_two().max(128);
	let fft = RealFftPlanner::<f64>::new().plan_fft_forward(size);
	let hann: Vec<f64> = (0..size)
		.map(|i| 0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / size as f64).cos())
		.collect();
	let window_power: f64 = hann.iter().map(|w| w * w).sum::<f64>() / size as f64;
	let hf_bin = (4_000.0 / sr * size as f64).ceil() as usize;
	map_frames(count, progress, |range, tick| {
		let mut input = vec![0.0f64; size];
		let mut spectrum = vec![Complex::new(0.0, 0.0); size / 2 + 1];
		// Log magnitudes of the frame before the chunk, so flux is the same however it is cut.
		let mut previous = vec![0.0f64; spectrum.len()];
		let mut out = Vec::with_capacity(range.len());
		let start = range.start;
		for i in range.start.saturating_sub(1)..range.end {
			if i >= start && (i - start).is_multiple_of(512) && !tick(i - start) {
				return None;
			}
			let center = i * hop;
			let mut power = 0.0;
			for (j, slot) in input.iter_mut().enumerate() {
				let x = (center + j)
					.checked_sub(size / 2)
					.and_then(|k| samples.get(k))
					.map_or(0.0, |&v| f64::from(v));
				power += x * x * hann[j] * hann[j];
				*slot = x * hann[j];
			}
			fft.process(&mut input, &mut spectrum)
				.expect("fft sizes match the plan");
			let mut flux = 0.0;
			let (mut total, mut high) = (0.0, 0.0);
			for (k, (c, prev)) in spectrum.iter().zip(previous.iter_mut()).enumerate() {
				let energy = c.norm_sqr();
				let log = (1.0 + 100.0 * energy.sqrt()).ln();
				flux += (log - *prev).max(0.0);
				*prev = log;
				total += energy;
				if k >= hf_bin {
					high += energy;
				}
			}
			if i < start {
				continue;
			}
			let rms = (power / (size as f64 * window_power)).sqrt();
			out.push(SpectralFrame {
				level_db: if rms > 1e-6 {
					20.0 * rms.log10()
				} else {
					-120.0
				},
				flux: if i == 0 {
					0.0
				} else {
					flux / spectrum.len() as f64
				},
				hf_ratio: if hf_bin < spectrum.len() && total > 0.0 {
					high / total
				} else {
					0.0
				},
			});
		}
		Some(out)
	})
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn level_flux_and_brightness_react_where_they_should() {
		let sr = 22_050u32;
		let hop = 110;
		// 0.2 s silence, 0.2 s of a 220 Hz sine at -6 dBFS peak, 0.2 s of white-ish noise.
		let mut audio = vec![0.0f32; 4_410];
		audio.extend((0..4_410).map(|i| {
			(0.5 * (2.0 * std::f64::consts::PI * 220.0 * i as f64 / 22_050.0).sin()) as f32
		}));
		let mut x = 1u32;
		audio.extend((0..4_410).map(|_| {
			x ^= x << 13;
			x ^= x >> 17;
			x ^= x << 5;
			(x as f32 / u32::MAX as f32 - 0.5) * 0.4
		}));
		let count = audio.len().div_ceil(hop);
		let f = spectral_frames(&audio, sr, hop, count, &mut |_| true).unwrap();
		let at = |t: f64| f[(t * f64::from(sr) / hop as f64) as usize];
		assert!(at(0.1).level_db < -100.0);
		// A sine of peak 0.5 has an RMS of -9 dBFS.
		assert!((at(0.3).level_db + 9.0).abs() < 0.5, "{:?}", at(0.3));
		assert!(at(0.3).hf_ratio < 0.01);
		assert!(at(0.5).hf_ratio > 0.3, "{:?}", at(0.5));
		let onset = f
			.iter()
			.skip(30)
			.take(20)
			.map(|s| s.flux)
			.fold(0.0, f64::max);
		assert!(onset > 10.0 * at(0.35).flux, "{onset} vs {:?}", at(0.35));
	}
}
