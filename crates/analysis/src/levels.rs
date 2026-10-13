//! Sample peak, 4x-oversampled true peak, RMS, clipping and stereo correlation.

use std::f64::consts::PI;

const PHASES: usize = 4;
const TAPS_PER_PHASE: usize = 16;

/// Polyphase 4x interpolator used to estimate inter-sample (true) peaks, in the spirit of
/// BS.1770-4 Annex 2: a 64-tap Kaiser-windowed sinc with each phase normalized to unity DC
/// gain.
#[derive(Debug, Clone)]
pub struct TruePeak {
	kernel: [[f32; TAPS_PER_PHASE]; PHASES],
	/// Each channel's history is stored twice so a window is always contiguous.
	history: [[f32; 2 * TAPS_PER_PHASE]; 2],
	pos: usize,
	peak: f32,
}

fn bessel_i0(x: f64) -> f64 {
	let mut sum = 1.0;
	let mut term = 1.0;
	for k in 1..50 {
		term *= (x / 2.0) / k as f64;
		sum += term * term;
	}
	sum
}

impl Default for TruePeak {
	fn default() -> Self {
		Self::new()
	}
}

impl TruePeak {
	pub fn new() -> Self {
		let n = PHASES * TAPS_PER_PHASE;
		let center = (n - 1) as f64 / 2.0;
		let beta = 8.0;
		let mut kernel = [[0.0f32; TAPS_PER_PHASE]; PHASES];
		for (p, phase) in kernel.iter_mut().enumerate() {
			let mut taps = [0.0f64; TAPS_PER_PHASE];
			for (j, tap) in taps.iter_mut().enumerate() {
				let k = j * PHASES + p;
				let x = (k as f64 - center) / PHASES as f64;
				let sinc = if x.abs() < 1e-12 {
					1.0
				} else {
					(PI * x).sin() / (PI * x)
				};
				let r = 2.0 * k as f64 / (n - 1) as f64 - 1.0;
				let w = bessel_i0(beta * (1.0 - r * r).max(0.0).sqrt()) / bessel_i0(beta);
				*tap = sinc * w;
			}
			let sum: f64 = taps.iter().sum();
			for (dst, src) in phase.iter_mut().zip(taps) {
				*dst = (src / sum) as f32;
			}
		}
		Self {
			kernel,
			history: [[0.0; 2 * TAPS_PER_PHASE]; 2],
			pos: 0,
			peak: 0.0,
		}
	}

	pub fn push(&mut self, left: &[f32], right: &[f32]) {
		for (&l, &r) in left.iter().zip(right) {
			self.pos = (self.pos + TAPS_PER_PHASE - 1) % TAPS_PER_PHASE;
			for (h, x) in self.history.iter_mut().zip([l, r]) {
				h[self.pos] = x;
				h[self.pos + TAPS_PER_PHASE] = x;
			}
			for h in &self.history {
				let window = &h[self.pos..self.pos + TAPS_PER_PHASE];
				for phase in &self.kernel {
					let acc: f32 = phase.iter().zip(window).map(|(c, x)| c * x).sum();
					self.peak = self.peak.max(acc.abs());
				}
			}
		}
	}

	pub fn peak(&self) -> f32 {
		self.peak
	}
}

/// Accumulates simple level statistics over a stereo signal.
#[derive(Debug, Clone, Default)]
pub struct Levels {
	pub peak: f32,
	pub sum_sq_l: f64,
	pub sum_sq_r: f64,
	pub sum_lr: f64,
	pub samples: u64,
	pub clipped: u64,
}

impl Levels {
	pub fn push(&mut self, left: &[f32], right: &[f32]) {
		for (&l, &r) in left.iter().zip(right) {
			let a = l.abs().max(r.abs());
			self.peak = self.peak.max(a);
			if l.abs() >= 1.0 {
				self.clipped += 1;
			}
			if r.abs() >= 1.0 {
				self.clipped += 1;
			}
			self.sum_sq_l += (l as f64) * (l as f64);
			self.sum_sq_r += (r as f64) * (r as f64);
			self.sum_lr += (l as f64) * (r as f64);
			self.samples += 1;
		}
	}

	/// RMS over both channels (mean of the per-channel mean squares).
	pub fn rms(&self) -> f64 {
		if self.samples == 0 {
			return 0.0;
		}
		((self.sum_sq_l + self.sum_sq_r) / (2.0 * self.samples as f64)).sqrt()
	}

	/// Pearson correlation of L and R (zero-mean assumed): +1 mono, 0 unrelated, -1 out of
	/// phase. Silence (or one silent side) reports 0.
	pub fn correlation(&self) -> f32 {
		let denom = (self.sum_sq_l * self.sum_sq_r).sqrt();
		if denom <= 1e-20 {
			0.0
		} else {
			(self.sum_lr / denom).clamp(-1.0, 1.0) as f32
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn true_peak_finds_inter_sample_overs() {
		// fs/4 sine sampled at 45 degrees: sample peak 0.707, true peak 1.0.
		let x: Vec<f32> = (0..4800)
			.map(|i| (PI / 2.0 * i as f64 + PI / 4.0).sin() as f32)
			.collect();
		let mut tp = TruePeak::new();
		tp.push(&x, &x);
		let sample_peak = x.iter().fold(0.0f32, |m, v| m.max(v.abs()));
		assert!((sample_peak - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-3);
		assert!((tp.peak() - 1.0).abs() < 0.02, "true peak {}", tp.peak());
	}

	#[test]
	fn correlation_extremes() {
		let x: Vec<f32> = (0..1000).map(|i| (i as f32 * 0.1).sin()).collect();
		let neg: Vec<f32> = x.iter().map(|v| -v).collect();
		let mut same = Levels::default();
		same.push(&x, &x);
		assert!((same.correlation() - 1.0).abs() < 1e-6);
		let mut opposite = Levels::default();
		opposite.push(&x, &neg);
		assert!((opposite.correlation() + 1.0).abs() < 1e-6);
		assert_eq!(Levels::default().correlation(), 0.0);
	}
}
