//! Fractional-position sample reading: cubic Hermite for unity/downward playback rates and
//! a table-based windowed sinc whose cutoff follows the rate when reading faster than the
//! source (transposing up), so high transpositions do not fold back as aliasing.

use std::sync::OnceLock;

use super::PI;

/// Zero crossings of the sinc kernel on each side (at unity cutoff).
const ZEROS: usize = 8;
/// Table points per zero crossing.
const RES: usize = 256;
/// Highest playback rate the sinc low-pass follows; faster reads keep this cutoff.
pub const MAX_SINC_RATIO: f64 = 4.0;
/// Rates up to this use Hermite (its mild aliasing is inaudible this close to unity).
pub const HERMITE_MAX_RATIO: f64 = 1.25;

/// Windowed (Blackman) sinc, sampled on `0..ZEROS` at `RES` points per crossing.
pub struct SincTable {
	table: Vec<f32>,
}

impl SincTable {
	fn new() -> Self {
		let n = ZEROS * RES + 2;
		let table = (0..n)
			.map(|i| {
				let x = i as f64 / RES as f64;
				if x >= ZEROS as f64 {
					return 0.0;
				}
				let sinc = if x == 0.0 {
					1.0
				} else {
					(f64::from(PI) * x).sin() / (f64::from(PI) * x)
				};
				// Blackman window over -ZEROS..ZEROS.
				let w = 2.0 * f64::from(PI) * (x / (2.0 * ZEROS as f64) + 0.5);
				let window = 0.42 - 0.5 * w.cos() + 0.08 * (2.0 * w).cos();
				(sinc * window) as f32
			})
			.collect();
		Self { table }
	}

	/// The process-wide table. Call once off the audio thread (device constructors do) so
	/// the first realtime use does not build it.
	pub fn global() -> &'static SincTable {
		static TABLE: OnceLock<SincTable> = OnceLock::new();
		TABLE.get_or_init(SincTable::new)
	}

	/// Kernel value at `x` zero crossings from the center.
	#[inline]
	fn kernel(&self, x: f64) -> f32 {
		let p = x.abs() * RES as f64;
		let i = p as usize;
		if i + 1 >= self.table.len() {
			return 0.0;
		}
		let f = (p - i as f64) as f32;
		self.table[i] + (self.table[i + 1] - self.table[i]) * f
	}
}

#[inline]
fn at(data: &[f32], i: i64) -> f32 {
	if i < 0 {
		0.0
	} else {
		data.get(i as usize).copied().unwrap_or(0.0)
	}
}

/// Catmull-Rom / cubic Hermite through `xm1, x0, x1, x2` at `t` in `0..1`.
#[inline]
pub fn hermite(xm1: f32, x0: f32, x1: f32, x2: f32, t: f32) -> f32 {
	let c1 = 0.5 * (x1 - xm1);
	let c2 = xm1 - 2.5 * x0 + 2.0 * x1 - 0.5 * x2;
	let c3 = 0.5 * (x2 - xm1) + 1.5 * (x0 - x1);
	((c3 * t + c2) * t + c1) * t + x0
}

/// `data` read at fractional frame `pos` while advancing `ratio` frames per output
/// sample. Frames outside `data` read as silence. Realtime safe.
#[inline]
pub fn read(data: &[f32], pos: f64, ratio: f64) -> f32 {
	let i = pos.floor();
	let t = pos - i;
	let i = i as i64;
	if ratio <= HERMITE_MAX_RATIO {
		return hermite(
			at(data, i - 1),
			at(data, i),
			at(data, i + 1),
			at(data, i + 2),
			t as f32,
		);
	}
	let table = SincTable::global();
	// Low-pass at the output's Nyquist, expressed in source frames.
	let fc = 1.0 / ratio.min(MAX_SINC_RATIO);
	let half = (ZEROS as f64 / fc).ceil() as i64;
	let mut acc = 0.0f32;
	let mut norm = 0.0f32;
	for k in (1 - half)..=half {
		let d = k as f64 - t;
		let w = table.kernel(d * fc);
		acc += w * at(data, i + k);
		norm += w;
	}
	if norm.abs() > 1e-6 { acc / norm } else { 0.0 }
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn hermite_hits_the_samples_and_is_smooth() {
		assert_eq!(hermite(0.0, 1.0, 2.0, 3.0, 0.0), 1.0);
		assert!((hermite(0.0, 1.0, 2.0, 3.0, 0.5) - 1.5).abs() < 1e-6);
		assert!((hermite(0.0, 1.0, 2.0, 3.0, 1.0) - 2.0).abs() < 1e-6);
	}

	#[test]
	fn sinc_reads_dc_and_slow_sines_faithfully() {
		let dc = vec![0.5f32; 400];
		assert!((read(&dc, 200.3, 3.0) - 0.5).abs() < 1e-3);
		let sine: Vec<f32> = (0..2000).map(|n| (n as f32 * 0.01).sin()).collect();
		for pos in [500.25, 777.9, 1234.5] {
			let want = (pos as f32 * 0.01).sin();
			assert!((read(&sine, pos, 2.0) - want).abs() < 2e-3, "{pos}");
			assert!((read(&sine, pos, 1.0) - want).abs() < 1e-3, "{pos}");
		}
	}

	#[test]
	fn sinc_suppresses_content_above_the_output_nyquist() {
		// A tone at 0.4 cycles/frame read 3x faster would alias; the kernel removes it.
		let hf: Vec<f32> = (0..4000)
			.map(|n| (2.0 * PI * 0.4 * n as f32).sin())
			.collect();
		let peak = (0..300)
			.map(|k| read(&hf, 1000.0 + k as f64 * 3.0, 3.0).abs())
			.fold(0.0f32, f32::max);
		assert!(peak < 0.05, "aliasing {peak}");
	}

	#[test]
	fn outside_reads_silence() {
		assert_eq!(read(&[1.0, 1.0], -10.0, 1.0), 0.0);
		assert_eq!(read(&[1.0, 1.0], 50.0, 2.0), 0.0);
	}
}
