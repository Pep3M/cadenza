//! Cheap fundamental-frequency estimate for auto-mapping samples (YIN difference
//! function). Not realtime: it allocates and scans a few thousand frames.

/// Frequency in Hz of the clearly pitched part of `samples` (40 Hz..2 kHz), or `None` for
/// noise, drums and silence. Looks at a window after the loudest attack so the
/// transient does not dominate.
pub fn detect_pitch(samples: &[f32], sample_rate: f32) -> Option<f32> {
	const THRESHOLD: f32 = 0.12;
	let sr = sample_rate.max(1.0);
	let max_lag = (sr / 40.0) as usize;
	let min_lag = (sr / 2_000.0).max(2.0) as usize;
	let window = (max_lag * 2).max(2_048);
	let peak =
		samples
			.iter()
			.take((sr * 0.5) as usize)
			.enumerate()
			.fold(
				(0, 0.0f32),
				|m, (i, v)| if v.abs() > m.1 { (i, v.abs()) } else { m },
			);
	if peak.1 < 1e-3 {
		return None;
	}
	let start = (peak.0 + (sr * 0.03) as usize).min(samples.len().saturating_sub(window + max_lag));
	let x = samples.get(start..start + window + max_lag)?;
	let mut d = vec![0.0f32; max_lag + 1];
	for (lag, slot) in d.iter_mut().enumerate().skip(1) {
		*slot = (0..window).map(|i| (x[i] - x[i + lag]).powi(2)).sum();
	}
	// Cumulative mean normalized difference.
	let mut running = 0.0;
	let mut cmnd = vec![1.0f32; max_lag + 1];
	for lag in 1..=max_lag {
		running += d[lag];
		cmnd[lag] = if running > 0.0 {
			d[lag] * lag as f32 / running
		} else {
			1.0
		};
	}
	let mut lag = (min_lag..max_lag).find(|&l| cmnd[l] < THRESHOLD)?;
	while lag + 1 < max_lag && cmnd[lag + 1] < cmnd[lag] {
		lag += 1;
	}
	// Parabolic refinement around the minimum.
	let (a, b, c) = (cmnd[lag - 1], cmnd[lag], cmnd[lag + 1]);
	let denom = a - 2.0 * b + c;
	let shift = if denom.abs() > 1e-12 {
		(0.5 * (a - c) / denom).clamp(-1.0, 1.0)
	} else {
		0.0
	};
	Some(sr / (lag as f32 + shift))
}

/// Nearest MIDI note of `hz` (60 = C4).
pub fn hz_to_midi(hz: f32) -> Option<u8> {
	if !(hz.is_finite() && hz > 0.0) {
		return None;
	}
	let n = (69.0 + 12.0 * (hz / 440.0).log2()).round();
	(0.0..=127.0).contains(&n).then_some(n as u8)
}

#[cfg(test)]
mod tests {
	use super::*;

	fn tone(freq: f32, sr: f32) -> Vec<f32> {
		(0..(sr * 0.6) as usize)
			.map(|n| {
				let t = n as f32 / sr;
				let w = std::f32::consts::TAU * freq * t;
				(w.sin() + 0.4 * (2.0 * w).sin() + 0.2 * (3.0 * w).sin()) * (-t * 2.0).exp()
			})
			.collect()
	}

	#[test]
	fn finds_harmonic_tones_and_rejects_noise() {
		for (freq, note) in [(110.0, 45u8), (261.63, 60), (440.0, 69), (987.77, 83)] {
			let hz = detect_pitch(&tone(freq, 44_100.0), 44_100.0).expect("pitched");
			assert!((hz / freq - 1.0).abs() < 0.01, "{freq}: {hz}");
			assert_eq!(hz_to_midi(hz), Some(note));
		}
		let mut rng = crate::util::Rng::new(3);
		let noise: Vec<f32> = (0..30_000).map(|_| rng.bipolar()).collect();
		assert_eq!(detect_pitch(&noise, 48_000.0), None);
		assert_eq!(detect_pitch(&[0.0; 10_000], 48_000.0), None);
		assert_eq!(detect_pitch(&[0.5; 100], 48_000.0), None);
	}
}
