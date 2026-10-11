//! Five-band energy split of the mid (L+R)/2 signal using 4th-order Linkwitz-Riley
//! crossovers at 150 Hz, 500 Hz, 2 kHz and 6 kHz.

use std::f64::consts::PI;

#[derive(Debug, Clone, Copy, Default)]
struct Biquad {
	b: [f64; 3],
	a: [f64; 2],
	z: [f64; 2],
}

impl Biquad {
	/// RBJ cookbook Butterworth (Q = 1/sqrt 2) low- or high-pass.
	fn butterworth(freq: f64, sample_rate: f64, highpass: bool) -> Self {
		let freq = freq.min(sample_rate * 0.45);
		let w = 2.0 * PI * freq / sample_rate;
		let (sin, cos) = w.sin_cos();
		let alpha = sin / (2.0 * std::f64::consts::FRAC_1_SQRT_2);
		let a0 = 1.0 + alpha;
		let b = if highpass {
			[(1.0 + cos) / 2.0, -(1.0 + cos), (1.0 + cos) / 2.0]
		} else {
			[(1.0 - cos) / 2.0, 1.0 - cos, (1.0 - cos) / 2.0]
		};
		Self {
			b: b.map(|v| v / a0),
			a: [-2.0 * cos / a0, (1.0 - alpha) / a0],
			z: [0.0; 2],
		}
	}

	#[inline]
	fn tick(&mut self, x: f64) -> f64 {
		let y = self.b[0] * x + self.z[0];
		self.z[0] = self.b[1] * x - self.a[0] * y + self.z[1];
		self.z[1] = self.b[2] * x - self.a[1] * y;
		y
	}
}

/// LR4 = two cascaded Butterworth sections.
#[derive(Debug, Clone, Copy)]
struct Lr4([Biquad; 2]);

impl Lr4 {
	fn new(freq: f64, sample_rate: f64, highpass: bool) -> Self {
		let s = Biquad::butterworth(freq, sample_rate, highpass);
		Self([s, s])
	}

	#[inline]
	fn tick(&mut self, x: f64) -> f64 {
		let y = self.0[0].tick(x);
		self.0[1].tick(y)
	}
}

const CROSSOVERS: [f64; 4] = [150.0, 500.0, 2_000.0, 6_000.0];

#[derive(Debug, Clone)]
pub struct BandMeter {
	/// Per band: optional high-pass at the lower edge, optional low-pass at the upper edge.
	filters: [(Option<Lr4>, Option<Lr4>); 5],
	energy: [f64; 5],
	samples: u64,
}

impl BandMeter {
	pub fn new(sample_rate: u32) -> Self {
		let sr = sample_rate as f64;
		let filters = std::array::from_fn(|band| {
			let hp = (band > 0).then(|| Lr4::new(CROSSOVERS[band - 1], sr, true));
			let lp = (band < 4).then(|| Lr4::new(CROSSOVERS[band], sr, false));
			(hp, lp)
		});
		Self {
			filters,
			energy: [0.0; 5],
			samples: 0,
		}
	}

	pub fn push(&mut self, left: &[f32], right: &[f32]) {
		for (&l, &r) in left.iter().zip(right) {
			let mid = 0.5 * (l as f64 + r as f64);
			for (band, (hp, lp)) in self.filters.iter_mut().enumerate() {
				let mut y = mid;
				if let Some(f) = hp {
					y = f.tick(y);
				}
				if let Some(f) = lp {
					y = f.tick(y);
				}
				self.energy[band] += y * y;
			}
			self.samples += 1;
		}
	}

	/// Mean-square energy per band in dB relative to full scale.
	pub fn energy_db(&self) -> [f32; 5] {
		self.energy.map(|e| {
			if self.samples == 0 {
				crate::FLOOR_DB
			} else {
				crate::analysis::power_db(e / self.samples as f64)
			}
		})
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn bands_for(freq: f64) -> [f32; 5] {
		let sr = 48_000;
		let x: Vec<f32> = (0..sr)
			.map(|i| (2.0 * PI * freq * i as f64 / sr as f64).sin() as f32)
			.collect();
		let mut m = BandMeter::new(sr);
		m.push(&x, &x);
		m.energy_db()
	}

	#[test]
	fn tones_land_in_their_band() {
		for (freq, band) in [
			(60.0, 0),
			(300.0, 1),
			(1_000.0, 2),
			(4_000.0, 3),
			(10_000.0, 4),
		] {
			let e = bands_for(freq);
			let best = (0..5).max_by(|&a, &b| e[a].total_cmp(&e[b])).unwrap();
			assert_eq!(best, band, "{freq} Hz -> {e:?}");
			// A full-scale sine has mean square 0.5 = -3 dB; narrow bands lose a little at the
			// crossover skirts.
			assert!((e[band] + 3.0).abs() < 2.0, "{freq} Hz -> {e:?}");
		}
	}
}
