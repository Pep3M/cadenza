//! ITU-R BS.1770-4 loudness: K-weighting, 400 ms gated blocks (75 % overlap) for integrated
//! loudness, and the maximum 3 s short-term loudness (EBU R128), both in LUFS.

use crate::FLOOR_DB;

#[derive(Debug, Clone, Copy, Default)]
struct Biquad {
	b0: f64,
	b1: f64,
	b2: f64,
	a1: f64,
	a2: f64,
	z1: f64,
	z2: f64,
}

impl Biquad {
	#[inline]
	fn tick(&mut self, x: f64) -> f64 {
		let y = self.b0 * x + self.z1;
		self.z1 = self.b1 * x - self.a1 * y + self.z2;
		self.z2 = self.b2 * x - self.a2 * y;
		y
	}
}

/// The two K-weighting stages (high shelf + RLB high-pass) for any sample rate, derived
/// from the analog prototypes behind the BS.1770 48 kHz coefficients.
fn k_weighting(sample_rate: f64) -> [Biquad; 2] {
	let (f0, g, q) = (1681.974450955533, 3.999843853973347, 0.7071752369554196);
	let k = (std::f64::consts::PI * f0 / sample_rate).tan();
	let vh = 10f64.powf(g / 20.0);
	let vb = vh.powf(0.4996667741545416);
	let a0 = 1.0 + k / q + k * k;
	let shelf = Biquad {
		b0: (vh + vb * k / q + k * k) / a0,
		b1: 2.0 * (k * k - vh) / a0,
		b2: (vh - vb * k / q + k * k) / a0,
		a1: 2.0 * (k * k - 1.0) / a0,
		a2: (1.0 - k / q + k * k) / a0,
		..Biquad::default()
	};
	let (f0, q) = (38.13547087602444, 0.5003270373238773);
	let k = (std::f64::consts::PI * f0 / sample_rate).tan();
	let a0 = 1.0 + k / q + k * k;
	let highpass = Biquad {
		b0: 1.0,
		b1: -2.0,
		b2: 1.0,
		a1: 2.0 * (k * k - 1.0) / a0,
		a2: (1.0 - k / q + k * k) / a0,
		..Biquad::default()
	};
	[shelf, highpass]
}

/// Streaming loudness meter for a stereo signal (channel weights 1.0).
#[derive(Debug, Clone)]
pub struct LoudnessMeter {
	filters: [[Biquad; 2]; 2],
	step_len: usize,
	step_fill: usize,
	step_energy: f64,
	/// Sum over channels of K-weighted energy per 100 ms step.
	steps: Vec<f64>,
	total_energy: f64,
	total_samples: usize,
}

impl LoudnessMeter {
	pub fn new(sample_rate: u32) -> Self {
		let k = k_weighting(sample_rate as f64);
		Self {
			filters: [k, k],
			step_len: ((sample_rate as f64) * 0.1).round().max(1.0) as usize,
			step_fill: 0,
			step_energy: 0.0,
			steps: Vec::new(),
			total_energy: 0.0,
			total_samples: 0,
		}
	}

	pub fn push(&mut self, left: &[f32], right: &[f32]) {
		for (&l, &r) in left.iter().zip(right) {
			let [fl, fr] = &mut self.filters;
			let yl = fl[0].tick(l as f64);
			let yl = fl[1].tick(yl);
			let yr = fr[0].tick(r as f64);
			let yr = fr[1].tick(yr);
			let e = yl * yl + yr * yr;
			self.step_energy += e;
			self.total_energy += e;
			self.total_samples += 1;
			self.step_fill += 1;
			if self.step_fill == self.step_len {
				self.steps.push(self.step_energy);
				self.step_energy = 0.0;
				self.step_fill = 0;
			}
		}
	}

	fn loudness(mean_square_sum: f64) -> f64 {
		-0.691 + 10.0 * mean_square_sum.log10()
	}

	/// Mean-square (summed over channels) of every window of `steps` consecutive steps,
	/// starting at step `from`.
	fn windows(&self, steps: usize, from: usize) -> Vec<f64> {
		let all = &self.steps[from.min(self.steps.len())..];
		if all.len() < steps {
			return Vec::new();
		}
		let len = (steps * self.step_len) as f64;
		all.windows(steps)
			.map(|w| w.iter().sum::<f64>() / len)
			.collect()
	}

	/// Whole-signal mean square, used when the signal is shorter than one window.
	fn whole(&self) -> Option<f64> {
		(self.total_samples > 0).then(|| self.total_energy / self.total_samples as f64)
	}

	/// Gated integrated loudness in LUFS ([`FLOOR_DB`] for silence).
	pub fn integrated(&self) -> f32 {
		self.integrated_since(0)
	}

	/// Gated integrated loudness of the steps from `step` on (a [`LoudnessMeter::steps`]
	/// count taken earlier): a live meter's "reset integrated" that keeps the short-term
	/// window.
	pub fn integrated_since(&self, step: usize) -> f32 {
		let mut blocks = self.windows(4, step);
		if blocks.is_empty() {
			if step == 0 {
				blocks.extend(self.whole());
			} else if step < self.steps.len() {
				let tail = &self.steps[step..];
				blocks.push(tail.iter().sum::<f64>() / (tail.len() * self.step_len) as f64);
			}
		}
		let abs_gated: Vec<f64> = blocks
			.into_iter()
			.filter(|&z| z > 0.0 && Self::loudness(z) > -70.0)
			.collect();
		if abs_gated.is_empty() {
			return FLOOR_DB;
		}
		let mean = abs_gated.iter().sum::<f64>() / abs_gated.len() as f64;
		let relative = Self::loudness(mean) - 10.0;
		let gated: Vec<f64> = abs_gated
			.into_iter()
			.filter(|&z| Self::loudness(z) > relative)
			.collect();
		let mean = gated.iter().sum::<f64>() / gated.len() as f64;
		(Self::loudness(mean) as f32).max(FLOOR_DB)
	}

	/// Loudness of the last 3 s of completed 100 ms steps (fewer while the signal is
	/// shorter): the live short-term reading. [`FLOOR_DB`] before the first step.
	pub fn short_term(&self) -> f32 {
		let n = self.steps.len().min(30);
		if n == 0 {
			return FLOOR_DB;
		}
		let energy: f64 = self.steps[self.steps.len() - n..].iter().sum();
		let mean = energy / (n * self.step_len) as f64;
		if mean <= 0.0 {
			FLOOR_DB
		} else {
			(Self::loudness(mean) as f32).max(FLOOR_DB)
		}
	}

	/// Completed 100 ms steps so far.
	pub fn steps(&self) -> usize {
		self.steps.len()
	}

	/// Highest 3 s short-term loudness; the whole signal when shorter than 3 s.
	pub fn short_term_max(&self) -> f32 {
		let mut windows = self.windows(30, 0);
		if windows.is_empty() {
			windows.extend(self.whole());
		}
		let max = windows.into_iter().fold(0.0f64, f64::max);
		if max <= 0.0 {
			FLOOR_DB
		} else {
			(Self::loudness(max) as f32).max(FLOOR_DB)
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn k_weighting_matches_spec_coefficients_at_48k() {
		let [shelf, hp] = k_weighting(48_000.0);
		assert!((shelf.b0 - 1.53512485958697).abs() < 1e-9);
		assert!((shelf.b1 + 2.69169618940638).abs() < 1e-9);
		assert!((shelf.b2 - 1.19839281085285).abs() < 1e-9);
		assert!((shelf.a1 + 1.69065929318241).abs() < 1e-9);
		assert!((shelf.a2 - 0.73248077421585).abs() < 1e-9);
		assert!((hp.a1 + 1.99004745483398).abs() < 1e-9);
		assert!((hp.a2 - 0.99007225036621).abs() < 1e-9);
	}

	#[test]
	fn short_term_follows_the_last_three_seconds() {
		let rate = 48_000;
		let sine = |amp: f32, n: usize| -> Vec<f32> {
			(0..n)
				.map(|i| amp * (std::f32::consts::TAU * 1000.0 * i as f32 / rate as f32).sin())
				.collect()
		};
		let mut m = LoudnessMeter::new(rate);
		assert_eq!(m.short_term(), FLOOR_DB);
		let loud = sine(0.1, 4 * rate as usize);
		m.push(&loud, &loud);
		assert_eq!(m.steps(), 40);
		assert!((m.short_term() + 20.0).abs() < 0.2, "{}", m.short_term());
		let quiet = sine(0.01, 3 * rate as usize);
		m.push(&quiet, &quiet);
		assert!((m.short_term() + 40.0).abs() < 0.2, "{}", m.short_term());
		assert!((m.integrated_since(40) + 40.0).abs() < 0.2);
		assert_eq!(m.integrated_since(70), FLOOR_DB);
		assert!(
			m.integrated() > -24.0,
			"gated: the quiet part is below -10 LU"
		);
	}

	#[test]
	fn silence_is_floor() {
		let mut m = LoudnessMeter::new(48_000);
		m.push(&[0.0; 48_000], &[0.0; 48_000]);
		assert_eq!(m.integrated(), FLOOR_DB);
		assert_eq!(m.short_term_max(), FLOOR_DB);
	}
}
