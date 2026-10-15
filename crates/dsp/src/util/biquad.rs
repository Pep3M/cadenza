//! RBJ "Audio EQ Cookbook" biquads in transposed direct form II.

use super::{PI, flush};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BiquadCoeffs {
	pub b0: f32,
	pub b1: f32,
	pub b2: f32,
	pub a1: f32,
	pub a2: f32,
}

impl Default for BiquadCoeffs {
	fn default() -> Self {
		Self::IDENTITY
	}
}

/// Intermediate values shared by all cookbook formulas.
struct Proto {
	cos_w: f64,
	alpha: f64,
}

fn proto(freq: f32, q: f32, sample_rate: f32) -> Proto {
	let nyquist_safe = (freq as f64).clamp(1.0, sample_rate as f64 * 0.49);
	let w = 2.0 * std::f64::consts::PI * nyquist_safe / sample_rate as f64;
	Proto {
		cos_w: w.cos(),
		alpha: w.sin() / (2.0 * (q as f64).max(0.01)),
	}
}

impl BiquadCoeffs {
	pub const IDENTITY: Self = Self {
		b0: 1.0,
		b1: 0.0,
		b2: 0.0,
		a1: 0.0,
		a2: 0.0,
	};

	fn normalized(b0: f64, b1: f64, b2: f64, a0: f64, a1: f64, a2: f64) -> Self {
		let inv = 1.0 / a0;
		Self {
			b0: (b0 * inv) as f32,
			b1: (b1 * inv) as f32,
			b2: (b2 * inv) as f32,
			a1: (a1 * inv) as f32,
			a2: (a2 * inv) as f32,
		}
	}

	pub fn lowpass(freq: f32, q: f32, sample_rate: f32) -> Self {
		let Proto { cos_w, alpha } = proto(freq, q, sample_rate);
		let b1 = 1.0 - cos_w;
		Self::normalized(
			b1 * 0.5,
			b1,
			b1 * 0.5,
			1.0 + alpha,
			-2.0 * cos_w,
			1.0 - alpha,
		)
	}

	pub fn highpass(freq: f32, q: f32, sample_rate: f32) -> Self {
		let Proto { cos_w, alpha } = proto(freq, q, sample_rate);
		let b1 = 1.0 + cos_w;
		Self::normalized(
			b1 * 0.5,
			-b1,
			b1 * 0.5,
			1.0 + alpha,
			-2.0 * cos_w,
			1.0 - alpha,
		)
	}

	/// Constant 0 dB peak gain band-pass.
	pub fn bandpass(freq: f32, q: f32, sample_rate: f32) -> Self {
		let Proto { cos_w, alpha } = proto(freq, q, sample_rate);
		Self::normalized(alpha, 0.0, -alpha, 1.0 + alpha, -2.0 * cos_w, 1.0 - alpha)
	}

	pub fn peaking(freq: f32, q: f32, gain_db: f32, sample_rate: f32) -> Self {
		let Proto { cos_w, alpha } = proto(freq, q, sample_rate);
		let a = 10.0_f64.powf(gain_db as f64 / 40.0);
		Self::normalized(
			1.0 + alpha * a,
			-2.0 * cos_w,
			1.0 - alpha * a,
			1.0 + alpha / a,
			-2.0 * cos_w,
			1.0 - alpha / a,
		)
	}

	/// Shelf with slope S = 1 (the steepest without overshoot).
	pub fn low_shelf(freq: f32, gain_db: f32, sample_rate: f32) -> Self {
		let a = 10.0_f64.powf(gain_db as f64 / 40.0);
		let Proto { cos_w, alpha } = proto(freq, std::f32::consts::FRAC_1_SQRT_2, sample_rate);
		let sq = 2.0 * a.sqrt() * alpha;
		Self::normalized(
			a * ((a + 1.0) - (a - 1.0) * cos_w + sq),
			2.0 * a * ((a - 1.0) - (a + 1.0) * cos_w),
			a * ((a + 1.0) - (a - 1.0) * cos_w - sq),
			(a + 1.0) + (a - 1.0) * cos_w + sq,
			-2.0 * ((a - 1.0) + (a + 1.0) * cos_w),
			(a + 1.0) + (a - 1.0) * cos_w - sq,
		)
	}

	pub fn high_shelf(freq: f32, gain_db: f32, sample_rate: f32) -> Self {
		let a = 10.0_f64.powf(gain_db as f64 / 40.0);
		let Proto { cos_w, alpha } = proto(freq, std::f32::consts::FRAC_1_SQRT_2, sample_rate);
		let sq = 2.0 * a.sqrt() * alpha;
		Self::normalized(
			a * ((a + 1.0) + (a - 1.0) * cos_w + sq),
			-2.0 * a * ((a - 1.0) + (a + 1.0) * cos_w),
			a * ((a + 1.0) + (a - 1.0) * cos_w - sq),
			(a + 1.0) - (a - 1.0) * cos_w + sq,
			2.0 * ((a - 1.0) - (a + 1.0) * cos_w),
			(a + 1.0) - (a - 1.0) * cos_w - sq,
		)
	}

	/// Magnitude response in dB at `freq` (used by tests and analysis).
	pub fn magnitude_db(&self, freq: f32, sample_rate: f32) -> f32 {
		let w = 2.0 * PI as f64 * freq as f64 / sample_rate as f64;
		let (c1, s1) = (w.cos(), -w.sin());
		let (c2, s2) = ((2.0 * w).cos(), -(2.0 * w).sin());
		let num_re = self.b0 as f64 + self.b1 as f64 * c1 + self.b2 as f64 * c2;
		let num_im = self.b1 as f64 * s1 + self.b2 as f64 * s2;
		let den_re = 1.0 + self.a1 as f64 * c1 + self.a2 as f64 * c2;
		let den_im = self.a1 as f64 * s1 + self.a2 as f64 * s2;
		let mag =
			((num_re * num_re + num_im * num_im) / (den_re * den_re + den_im * den_im)).sqrt();
		(20.0 * mag.max(1e-12).log10()) as f32
	}
}

/// Filter state for one channel.
#[derive(Debug, Clone, Copy, Default)]
pub struct Biquad {
	z1: f32,
	z2: f32,
}

impl Biquad {
	#[inline]
	pub fn process(&mut self, c: &BiquadCoeffs, x: f32) -> f32 {
		let y = c.b0 * x + self.z1;
		self.z1 = flush(c.b1 * x - c.a1 * y + self.z2);
		self.z2 = flush(c.b2 * x - c.a2 * y);
		y
	}

	pub fn reset(&mut self) {
		*self = Self::default();
	}
}

/// Butterworth section Qs for cascades of 1, 2 and 4 biquads (12/24/48 dB/oct).
pub const BUTTERWORTH_Q: [&[f32]; 3] = [
	&[std::f32::consts::FRAC_1_SQRT_2],
	&[0.541_196_1, 1.306_563],
	&[0.509_795_6, 0.601_344_9, 0.899_976_2, 2.562_915_4],
];

#[cfg(test)]
mod tests {
	use super::*;

	const SR: f32 = 48_000.0;

	#[test]
	fn peaking_hits_gain_at_center() {
		for g in [-12.0, -3.0, 6.0, 15.0] {
			let c = BiquadCoeffs::peaking(1000.0, 1.0, g, SR);
			assert!((c.magnitude_db(1000.0, SR) - g).abs() < 0.05);
			assert!(c.magnitude_db(20.0, SR).abs() < 0.3);
		}
	}

	#[test]
	fn shelves_reach_gain() {
		let lo = BiquadCoeffs::low_shelf(200.0, 9.0, SR);
		assert!((lo.magnitude_db(20.0, SR) - 9.0).abs() < 0.3);
		assert!((lo.magnitude_db(200.0, SR) - 4.5).abs() < 0.2);
		assert!(lo.magnitude_db(10_000.0, SR).abs() < 0.1);
		let hi = BiquadCoeffs::high_shelf(4000.0, -6.0, SR);
		assert!((hi.magnitude_db(20_000.0, SR) + 6.0).abs() < 0.3);
		assert!(hi.magnitude_db(100.0, SR).abs() < 0.1);
	}

	#[test]
	fn lowpass_highpass_cutoff_is_minus_3db() {
		let lp = BiquadCoeffs::lowpass(1000.0, std::f32::consts::FRAC_1_SQRT_2, SR);
		let hp = BiquadCoeffs::highpass(1000.0, std::f32::consts::FRAC_1_SQRT_2, SR);
		assert!((lp.magnitude_db(1000.0, SR) + 3.01).abs() < 0.05);
		assert!((hp.magnitude_db(1000.0, SR) + 3.01).abs() < 0.05);
		assert!(lp.magnitude_db(10_000.0, SR) < -35.0);
		assert!(hp.magnitude_db(100.0, SR) < -35.0);
	}

	#[test]
	fn time_domain_matches_response() {
		let c = BiquadCoeffs::peaking(1000.0, 2.0, 12.0, SR);
		let mut f = Biquad::default();
		let mut peak: f32 = 0.0;
		for n in 0..48_000 {
			let x = (2.0 * PI * 1000.0 * n as f32 / SR).sin();
			let y = f.process(&c, x);
			if n > 24_000 {
				peak = peak.max(y.abs());
			}
		}
		assert!((super::super::gain_to_db(peak) - 12.0).abs() < 0.1);
	}
}
