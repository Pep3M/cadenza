//! Topology-preserving state-variable filter (Simper/Cytomic). Unlike biquads
//! it stays stable and artifact-free under fast cutoff modulation, which makes
//! it the right choice for synth filters swept by envelopes.

use super::{PI, flush};

#[derive(Debug, Clone, Copy, Default)]
pub struct SvfCoeffs {
	a1: f32,
	a2: f32,
	a3: f32,
	k: f32,
}

impl SvfCoeffs {
	/// `resonance` in `0..=1`; 0 is a gentle Q of 0.5, 1 is a Q of 10.
	#[inline]
	pub fn new(cutoff: f32, resonance: f32, sample_rate: f32) -> Self {
		let fc = cutoff.clamp(10.0, sample_rate * 0.45);
		let g = (PI * fc / sample_rate).tan();
		let k = 2.0 - 1.9 * resonance.clamp(0.0, 1.0);
		let a1 = 1.0 / (1.0 + g * (g + k));
		let a2 = g * a1;
		Self {
			a1,
			a2,
			a3: g * a2,
			k,
		}
	}
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SvfOutputs {
	pub low: f32,
	pub band: f32,
	pub high: f32,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Svf {
	ic1: f32,
	ic2: f32,
}

impl Svf {
	#[inline]
	pub fn process(&mut self, c: &SvfCoeffs, x: f32) -> SvfOutputs {
		let v3 = x - self.ic2;
		let v1 = c.a1 * self.ic1 + c.a2 * v3;
		let v2 = self.ic2 + c.a2 * self.ic1 + c.a3 * v3;
		self.ic1 = flush(2.0 * v1 - self.ic1);
		self.ic2 = flush(2.0 * v2 - self.ic2);
		SvfOutputs {
			low: v2,
			band: v1,
			high: x - c.k * v1 - v2,
		}
	}

	#[inline]
	pub fn lowpass(&mut self, c: &SvfCoeffs, x: f32) -> f32 {
		self.process(c, x).low
	}

	pub fn reset(&mut self) {
		*self = Self::default();
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn tone_gain(freq: f32, cutoff: f32) -> f32 {
		let sr = 48_000.0;
		let c = SvfCoeffs::new(cutoff, 0.0, sr);
		let mut f = Svf::default();
		let mut peak: f32 = 0.0;
		for n in 0..24_000 {
			let y = f.lowpass(&c, (2.0 * PI * freq * n as f32 / sr).sin());
			if n > 12_000 {
				peak = peak.max(y.abs());
			}
		}
		peak
	}

	#[test]
	fn lowpass_passes_low_and_cuts_high() {
		assert!((tone_gain(100.0, 2_000.0) - 1.0).abs() < 0.02);
		assert!(tone_gain(16_000.0, 1_000.0) < 0.01);
	}
}
