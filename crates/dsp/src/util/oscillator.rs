//! Band-limited (PolyBLEP / PolyBLAMP) oscillators.

use super::TAU;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Waveform {
	Sine,
	Saw,
	Square,
	Triangle,
}

impl Waveform {
	/// Maps an integer-valued parameter (0 sine, 1 saw, 2 square, 3 triangle).
	pub fn from_index(i: usize) -> Self {
		match i {
			0 => Self::Sine,
			1 => Self::Saw,
			2 => Self::Square,
			_ => Self::Triangle,
		}
	}
}

/// Residual of a band-limited unit step (+2 jump), `t` = phase, `dt` = increment.
#[inline]
fn poly_blep(t: f32, dt: f32) -> f32 {
	if t < dt {
		let x = t / dt;
		x + x - x * x - 1.0
	} else if t > 1.0 - dt {
		let x = (t - 1.0) / dt;
		x * x + x + x + 1.0
	} else {
		0.0
	}
}

/// Integrated `poly_blep`: residual of a band-limited slope change of +2 per sample.
#[inline]
fn poly_blamp(t: f32, dt: f32) -> f32 {
	if t < dt {
		let x = t / dt - 1.0;
		-x * x * x / 3.0
	} else if t > 1.0 - dt {
		let x = (t - 1.0) / dt + 1.0;
		x * x * x / 3.0
	} else {
		0.0
	}
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Oscillator {
	phase: f32,
}

impl Oscillator {
	pub fn set_phase(&mut self, phase: f32) {
		self.phase = phase.rem_euclid(1.0);
	}

	/// Renders one sample in `-1..=1` and advances by `inc` cycles/sample.
	#[inline]
	pub fn next(&mut self, wave: Waveform, inc: f32) -> f32 {
		let dt = inc.clamp(1e-7, 0.5);
		let t = self.phase;
		let y = match wave {
			Waveform::Sine => (TAU * t).sin(),
			Waveform::Saw => 2.0 * t - 1.0 - poly_blep(t, dt),
			Waveform::Square => {
				let naive = if t < 0.5 { 1.0 } else { -1.0 };
				naive + poly_blep(t, dt) - poly_blep((t + 0.5).fract(), dt)
			}
			Waveform::Triangle => {
				let naive = if t < 0.5 {
					4.0 * t - 1.0
				} else {
					3.0 - 4.0 * t
				};
				naive + 4.0 * dt * (poly_blamp(t, dt) - poly_blamp((t + 0.5).fract(), dt))
			}
		};
		self.phase += dt;
		if self.phase >= 1.0 {
			self.phase -= 1.0;
		}
		y
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	/// Fraction of signal energy that is NOT at harmonics below Nyquist, i.e.
	/// aliasing. The test frequency makes the analysis window an exact number of
	/// periods so harmonics land on exact correlation frequencies.
	fn alias_ratio(wave: Waveform, corrected: bool) -> f64 {
		let sr = 48_000.0_f64;
		let f = 2_340.0_f64;
		let n = 4_800;
		let inc = (f / sr) as f32;
		let mut osc = Oscillator::default();
		let y: Vec<f64> = (0..n)
			.map(|_| {
				let t = osc.phase;
				if corrected {
					osc.next(wave, inc) as f64
				} else {
					osc.phase = (osc.phase + inc).fract();
					let v = match wave {
						Waveform::Saw => 2.0 * t - 1.0,
						Waveform::Square => {
							if t < 0.5 {
								1.0
							} else {
								-1.0
							}
						}
						_ => {
							if t < 0.5 {
								4.0 * t - 1.0
							} else {
								3.0 - 4.0 * t
							}
						}
					};
					v as f64
				}
			})
			.collect();
		let total: f64 = y.iter().map(|v| v * v).sum();
		let mut harmonic = 0.0;
		let mut k = 1.0;
		while k * f < sr / 2.0 {
			let w = std::f64::consts::TAU * k * f / sr;
			let (mut a, mut b) = (0.0, 0.0);
			for (i, v) in y.iter().enumerate() {
				a += v * (w * i as f64).cos();
				b += v * (w * i as f64).sin();
			}
			harmonic += 2.0 * (a * a + b * b) / n as f64;
			k += 1.0;
		}
		(total - harmonic).max(0.0) / total
	}

	#[test]
	fn blep_reduces_aliasing() {
		for wave in [Waveform::Saw, Waveform::Square, Waveform::Triangle] {
			let naive = alias_ratio(wave, false);
			let blep = alias_ratio(wave, true);
			assert!(blep < naive * 0.3, "{wave:?}: blep {blep} vs naive {naive}");
		}
	}

	#[test]
	fn output_is_bounded() {
		for wave in [
			Waveform::Sine,
			Waveform::Saw,
			Waveform::Square,
			Waveform::Triangle,
		] {
			let mut osc = Oscillator::default();
			for i in 0..10_000 {
				let inc = 0.0001 + (i % 97) as f32 * 0.004;
				let y = osc.next(wave, inc);
				assert!(y.is_finite() && y.abs() <= 1.6, "{wave:?} {y}");
			}
		}
	}
}
