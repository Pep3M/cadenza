//! Frame-wise YIN pitch tracker (de Cheveigné & Kawahara 2002). The difference function is
//! computed from an FFT autocorrelation plus prefix energies, so a frame costs
//! O(W log W) instead of O(W x lags).

use std::sync::Arc;

use realfft::num_complex::Complex;
use realfft::{RealFftPlanner, RealToComplex};

use crate::hz_to_midi;

#[derive(Debug, Clone)]
pub struct YinOptions {
	/// Seconds between frames.
	pub hop_seconds: f64,
	pub min_hz: f64,
	pub max_hz: f64,
	/// Absolute threshold on the cumulative mean normalized difference.
	pub threshold: f64,
	/// A frame whose best dip is above this is unvoiced.
	pub voicing_limit: f64,
	/// Frames quieter than the loudest frame by more than this are unvoiced.
	pub silence_db: f64,
}

impl Default for YinOptions {
	fn default() -> Self {
		Self {
			hop_seconds: 0.005,
			min_hz: 50.0,
			max_hz: 1_500.0,
			threshold: 0.15,
			voicing_limit: 0.35,
			silence_db: 40.0,
		}
	}
}

/// One analysis frame, centered on `time`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Frame {
	pub time: f64,
	/// Fractional MIDI pitch, `None` when unvoiced.
	pub pitch: Option<f64>,
	/// Best normalized difference (0 = perfectly periodic).
	pub aperiodicity: f64,
	pub rms: f64,
}

/// Tracks the pitch of mono `samples`.
pub fn track(samples: &[f32], sample_rate: u32, opts: &YinOptions) -> Vec<Frame> {
	let sr = f64::from(sample_rate.max(1));
	let window = ((sr * 0.04) as usize).next_power_of_two().max(256);
	let max_lag = ((sr / opts.min_hz) as usize).min(window / 2);
	let min_lag = ((sr / opts.max_hz) as usize).max(2);
	let hop = ((sr * opts.hop_seconds).round() as usize).max(1);
	if samples.is_empty() || min_lag + 2 >= max_lag {
		return Vec::new();
	}
	let mut fft = Autocorr::new(window);
	let mut frame = vec![0.0f64; window];
	let mut energy = vec![0.0f64; window + 1];
	let mut acf = vec![0.0f64; window];
	let mut cmnd = vec![0.0f64; max_lag + 1];
	let count = samples.len().div_ceil(hop);
	let mut frames = Vec::with_capacity(count);
	for i in 0..count {
		let center = i * hop;
		for (j, slot) in frame.iter_mut().enumerate() {
			let index = (center + j).checked_sub(window / 2);
			*slot = index
				.and_then(|k| samples.get(k))
				.map_or(0.0, |&v| f64::from(v));
		}
		for j in 0..window {
			energy[j + 1] = energy[j] + frame[j] * frame[j];
		}
		let rms = (energy[window] / window as f64).sqrt();
		fft.compute(&frame, &mut acf);
		// d(tau) = sum_{j < W - tau} (x_j - x_{j+tau})^2, normalized cumulatively.
		cmnd[0] = 1.0;
		let mut running = 0.0;
		for tau in 1..=max_lag {
			let d =
				(energy[window - tau] + (energy[window] - energy[tau]) - 2.0 * acf[tau]).max(0.0);
			running += d;
			cmnd[tau] = if running > 0.0 {
				d * tau as f64 / running
			} else {
				1.0
			};
		}
		let mut best = None;
		let mut tau = min_lag;
		while tau < max_lag {
			if cmnd[tau] < opts.threshold {
				while tau + 1 < max_lag && cmnd[tau + 1] < cmnd[tau] {
					tau += 1;
				}
				best = Some(tau);
				break;
			}
			tau += 1;
		}
		let best = best.unwrap_or_else(|| {
			(min_lag..max_lag)
				.min_by(|&a, &b| cmnd[a].total_cmp(&cmnd[b]))
				.unwrap_or(min_lag)
		});
		let aperiodicity = cmnd[best];
		let shift = if best > min_lag && best + 1 < max_lag {
			let (a, b, c) = (cmnd[best - 1], cmnd[best], cmnd[best + 1]);
			let denom = a - 2.0 * b + c;
			if denom.abs() > 1e-12 {
				(0.5 * (a - c) / denom).clamp(-0.5, 0.5)
			} else {
				0.0
			}
		} else {
			0.0
		};
		let period = best as f64 + shift;
		let pitch = (aperiodicity <= opts.voicing_limit).then(|| hz_to_midi(sr / period));
		frames.push(Frame {
			time: center as f64 / sr,
			pitch,
			aperiodicity,
			rms,
		});
	}
	let loudest = frames.iter().map(|f| f.rms).fold(0.0, f64::max);
	let floor = loudest * 10f64.powf(-opts.silence_db / 20.0);
	for f in &mut frames {
		if f.rms <= floor || loudest <= 0.0 {
			f.pitch = None;
		}
	}
	frames
}

/// Linear autocorrelation of a frame through a zero-padded real FFT.
struct Autocorr {
	forward: Arc<dyn RealToComplex<f64>>,
	inverse: Arc<dyn realfft::ComplexToReal<f64>>,
	time: Vec<f64>,
	spectrum: Vec<Complex<f64>>,
}

impl Autocorr {
	fn new(window: usize) -> Self {
		let mut planner = RealFftPlanner::<f64>::new();
		let forward = planner.plan_fft_forward(window * 2);
		let inverse = planner.plan_fft_inverse(window * 2);
		let spectrum = forward.make_output_vec();
		Self {
			forward,
			inverse,
			time: vec![0.0; window * 2],
			spectrum,
		}
	}

	fn compute(&mut self, frame: &[f64], out: &mut [f64]) {
		let n = self.time.len();
		self.time[..frame.len()].copy_from_slice(frame);
		self.time[frame.len()..].fill(0.0);
		self.forward
			.process(&mut self.time, &mut self.spectrum)
			.expect("fft sizes match the plan");
		for c in &mut self.spectrum {
			*c = Complex::new(c.norm_sqr(), 0.0);
		}
		self.inverse
			.process(&mut self.spectrum, &mut self.time)
			.expect("fft sizes match the plan");
		let scale = 1.0 / n as f64;
		for (o, t) in out.iter_mut().zip(&self.time) {
			*o = t * scale;
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::midi_to_hz;

	fn tone(midi: f64, sr: u32, seconds: f64) -> Vec<f32> {
		let hz = midi_to_hz(midi);
		(0..(f64::from(sr) * seconds) as usize)
			.map(|i| {
				let t = i as f64 / f64::from(sr);
				let x = (2.0 * std::f64::consts::PI * hz * t).sin()
					+ 0.5 * (4.0 * std::f64::consts::PI * hz * t).sin();
				(0.4 * x) as f32
			})
			.collect()
	}

	#[test]
	fn tracks_a_steady_tone_to_within_a_cent() {
		for (midi, sr) in [(45.0, 22_050), (60.3, 44_100), (76.0, 48_000)] {
			let frames = track(&tone(midi, sr, 0.5), sr, &YinOptions::default());
			let voiced: Vec<f64> = frames[20..frames.len() - 20]
				.iter()
				.map(|f| f.pitch.expect("voiced"))
				.collect();
			for p in voiced {
				assert!((p - midi).abs() < 0.01, "{p} vs {midi} at {sr}");
			}
		}
	}

	#[test]
	fn silence_is_unvoiced() {
		let mut audio = tone(57.0, 22_050, 0.3);
		audio.extend(std::iter::repeat_n(0.0, 22_050 / 2));
		let frames = track(&audio, 22_050, &YinOptions::default());
		assert!(frames.last().unwrap().pitch.is_none());
		assert!(frames[10].pitch.is_some());
	}
}
