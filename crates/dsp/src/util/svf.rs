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

/// Which of the SVF's three simultaneous outputs to evaluate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SvfOutput {
	Low,
	Band,
	High,
}

/// Minimal complex arithmetic for evaluating the SVF's exact transfer
/// function at `z = e^{-jΩ}`, the same convention `BiquadCoeffs::magnitude_db`
/// uses, so the two can be summed/compared directly.
#[derive(Clone, Copy)]
struct Complex {
	re: f64,
	im: f64,
}

impl Complex {
	fn new(re: f64, im: f64) -> Self {
		Self { re, im }
	}

	fn one() -> Self {
		Self::new(1.0, 0.0)
	}

	fn add(self, o: Self) -> Self {
		Self::new(self.re + o.re, self.im + o.im)
	}

	fn sub(self, o: Self) -> Self {
		Self::new(self.re - o.re, self.im - o.im)
	}

	fn mul(self, o: Self) -> Self {
		Self::new(
			self.re * o.re - self.im * o.im,
			self.re * o.im + self.im * o.re,
		)
	}

	fn scale(self, s: f64) -> Self {
		Self::new(self.re * s, self.im * s)
	}

	fn div(self, o: Self) -> Self {
		let d = o.re * o.re + o.im * o.im;
		Self::new(
			(self.re * o.re + self.im * o.im) / d,
			(self.im * o.re - self.re * o.im) / d,
		)
	}

	fn abs(self) -> f64 {
		(self.re * self.re + self.im * self.im).sqrt()
	}
}

/// Exact magnitude response of the SVF's `output` in dB at `freq`, derived
/// algebraically from the same trapezoidal (TPT) recurrence [`Svf::process`]
/// runs, so the picture and the sound cannot drift. Lets callers sample many
/// frequency points from one set of coefficients without simulating audio.
pub fn svf_magnitude_db(c: &SvfCoeffs, output: SvfOutput, freq: f32, sample_rate: f32) -> f32 {
	let w = 2.0 * PI as f64 * freq as f64 / sample_rate as f64;
	// z^-1, matching `BiquadCoeffs::magnitude_db`'s convention.
	let z_inv = Complex::new(w.cos(), -w.sin());
	let (a1, a2, a3, k) = (c.a1 as f64, c.a2 as f64, c.a3 as f64, c.k as f64);
	let one = Complex::one();
	// `ic1`/`ic2` are one-sample registers: ic[n] = 2*v[n-1] - ic[n-1], i.e.
	// IC(z) = 2*z^-1*V(z) / (1 + z^-1).
	let m = z_inv.scale(2.0).div(one.add(z_inv));
	let denom = one
		.sub(m.scale(1.0 - a3))
		.mul(one.sub(m.scale(a1)))
		.add(m.mul(m).scale(a2 * a2));
	let numer = m.scale(a2 * a2).add(one.sub(m.scale(a1)).scale(a3));
	let h_low = numer.div(denom);
	let h_band = one.sub(m.mul(h_low)).scale(a2).div(one.sub(m.scale(a1)));
	let mag = match output {
		SvfOutput::Low => h_low.abs(),
		SvfOutput::Band => h_band.abs(),
		SvfOutput::High => one.sub(h_band.scale(k)).sub(h_low).abs(),
	};
	(20.0 * mag.max(1e-12).log10()) as f32
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

	/// Runs a sine through the real filter to steady state and measures its
	/// RMS gain (robust to the discrete sampling grid missing a true peak,
	/// unlike peak-picking), for comparison against `svf_magnitude_db`'s
	/// closed form.
	fn simulated_gain_db(c: &SvfCoeffs, output: SvfOutput, freq: f32, sample_rate: f32) -> f32 {
		let mut f = Svf::default();
		let mut sum_sq = 0.0_f64;
		let mut n_measured = 0u64;
		let n_samples = 48_000.max((sample_rate * 4.0) as usize);
		// f64 phase accumulation: at deep attenuation (<-60 dB) the filtered
		// amplitude is small enough that f32 phase rounding error alone would
		// swamp it.
		let step = 2.0 * std::f64::consts::PI * freq as f64 / sample_rate as f64;
		for n in 0..n_samples {
			let x = (step * n as f64).sin() as f32;
			let o = f.process(c, x);
			let y = match output {
				SvfOutput::Low => o.low,
				SvfOutput::Band => o.band,
				SvfOutput::High => o.high,
			};
			if n > n_samples / 2 {
				sum_sq += (y as f64) * (y as f64);
				n_measured += 1;
			}
		}
		let rms = (sum_sq / n_measured as f64).sqrt();
		// Input sine has RMS 1/sqrt(2); gain = output RMS / input RMS.
		super::super::gain_to_db((rms * std::f64::consts::SQRT_2) as f32)
	}

	#[test]
	fn magnitude_formula_matches_simulated_steady_state() {
		let sr = 48_000.0;
		for &(cutoff, resonance) in &[(200.0, 0.0), (1_000.0, 0.5), (4_000.0, 0.9)] {
			let c = SvfCoeffs::new(cutoff, resonance, sr);
			for freq in [50.0, cutoff * 0.5, cutoff, cutoff * 2.0, 8_000.0] {
				for output in [SvfOutput::Low, SvfOutput::Band, SvfOutput::High] {
					let formula = svf_magnitude_db(&c, output, freq, sr);
					let simulated = simulated_gain_db(&c, output, freq, sr);
					assert!(
						(formula - simulated).abs() < 0.3,
						"{output:?} cutoff {cutoff} res {resonance} freq {freq}: \
						 formula {formula}, simulated {simulated}"
					);
				}
			}
		}
	}

	#[test]
	fn lowpass_minus_3db_at_cutoff_with_butterworth_q() {
		let sr = 48_000.0;
		// `resonance` 0 gives k = 2 (Q = 0.5, gentle), not -3 dB at the
		// cutoff; solve for the `resonance` that gives Butterworth's
		// k = sqrt(2) (Q = 1/sqrt(2)) instead, the classic -3 dB point.
		let q_butterworth_resonance = (2.0 - std::f32::consts::SQRT_2) / 1.9;
		let c = SvfCoeffs::new(1_000.0, q_butterworth_resonance, sr);
		assert!((svf_magnitude_db(&c, SvfOutput::Low, 1_000.0, sr) + 3.01).abs() < 0.1);
	}

	#[test]
	fn resonance_peaks_near_cutoff() {
		let sr = 48_000.0;
		let c = SvfCoeffs::new(1_000.0, 0.95, sr);
		let at_cutoff = svf_magnitude_db(&c, SvfOutput::Low, 1_000.0, sr);
		let well_below = svf_magnitude_db(&c, SvfOutput::Low, 100.0, sr);
		let well_above = svf_magnitude_db(&c, SvfOutput::Low, 10_000.0, sr);
		assert!(at_cutoff > well_below && at_cutoff > well_above && at_cutoff > 3.0);
	}
}
