//! FFT spectrum of a stereo signal: third-octave balance of the mid and side signals, the
//! stereo image (width, low-end correlation) and an octave-band timeline (one row per hop)
//! that the masking report compares across tracks.

use std::sync::Arc;

use realfft::num_complex::Complex;
use realfft::{RealFftPlanner, RealToComplex};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{FLOOR_DB, power_db};

/// ISO third-octave band centers, 20 Hz to 20 kHz.
pub const THIRD_OCTAVE_HZ: [f32; 31] = [
	20.0, 25.0, 31.5, 40.0, 50.0, 63.0, 80.0, 100.0, 125.0, 160.0, 200.0, 250.0, 315.0, 400.0,
	500.0, 630.0, 800.0, 1_000.0, 1_250.0, 1_600.0, 2_000.0, 2_500.0, 3_150.0, 4_000.0, 5_000.0,
	6_300.0, 8_000.0, 10_000.0, 12_500.0, 16_000.0, 20_000.0,
];

/// Octave band centers, 31.5 Hz to 16 kHz (each groups three third-octave bands).
pub const OCTAVE_HZ: [f32; 10] = [
	31.5, 63.0, 125.0, 250.0, 500.0, 1_000.0, 2_000.0, 4_000.0, 8_000.0, 16_000.0,
];

/// Below this the low-end mono check applies (bass and kick fundamentals).
pub const LOW_END_HZ: f32 = 120.0;

/// FFT length (about 170 ms at 48 kHz: fine enough for the lowest third-octave bands).
pub const FRAME: usize = 8192;
/// Hop between frames (50 % overlap).
pub const HOP: usize = FRAME / 2;

/// Octave band index of a third-octave band (20 Hz joins the 31.5 Hz octave).
pub fn octave_of_third(third: usize) -> usize {
	third.saturating_sub(1) / 3
}

/// Third-octave index of `hz` (nearest band in log frequency), `None` outside 18 Hz..22 kHz.
pub fn third_of_hz(hz: f32) -> Option<usize> {
	if !(17.8..22_400.0).contains(&hz) {
		return None;
	}
	let pos = (hz / 20.0).log2() * 3.0;
	Some((pos.round().max(0.0) as usize).min(THIRD_OCTAVE_HZ.len() - 1))
}

/// Mean spectrum and stereo image of a whole signal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Spectrum {
	/// Mean-square level of the mid signal (L+R)/2 per third-octave band, dBFS
	/// ([`THIRD_OCTAVE_HZ`]).
	pub third_octave_db: Vec<f32>,
	/// The same for the side signal (L-R)/2.
	pub side_third_octave_db: Vec<f32>,
	/// Mid level per octave band ([`OCTAVE_HZ`]), dBFS.
	pub octave_db: Vec<f32>,
}

impl Spectrum {
	/// Octave levels relative to their loudest band (0 dB = loudest): the shape, not the
	/// level, for charts and comparisons.
	pub fn octave_shape(&self) -> Vec<f32> {
		let max = self.octave_db.iter().copied().fold(FLOOR_DB, f32::max);
		self.octave_db
			.iter()
			.map(|v| {
				if max <= FLOOR_DB {
					0.0
				} else {
					(v - max).max(-60.0)
				}
			})
			.collect()
	}

	/// Loudest octave band index.
	pub fn peak_octave(&self) -> usize {
		self.octave_db
			.iter()
			.enumerate()
			.max_by(|a, b| a.1.total_cmp(b.1))
			.map(|(i, _)| i)
			.unwrap_or(5)
	}
}

/// Stereo image.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Stereo {
	/// Side energy share, 0 (mono) .. 1 (only side); 0.1-0.3 is a typical wide mix.
	pub width: f32,
	/// L/R correlation below [`LOW_END_HZ`]: should stay near +1 (mono lows). 1 for silence.
	pub low_correlation: f32,
	/// Side minus mid level below [`LOW_END_HZ`], dB: above about -15 the lows are wide.
	pub low_side_db: f32,
}

/// Octave-band mid levels per hop (dBFS), aligned with the render start.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Timeline {
	pub hop_seconds: f64,
	pub frames: Vec<[f32; 10]>,
}

/// Streaming FFT analyzer (offline only: it allocates).
pub struct SpectrumAnalyzer {
	sample_rate: u32,
	fft: Arc<dyn RealToComplex<f32>>,
	window: Vec<f32>,
	/// Normalizes `|X|^2` sums to mean-square levels: 2 / (N * sum w^2).
	scale: f64,
	left: Vec<f32>,
	right: Vec<f32>,
	filled: usize,
	/// Samples pushed since the last processed frame (a partial frame is flushed at the end).
	fresh: usize,
	input: Vec<f32>,
	spec_l: Vec<Complex<f32>>,
	spec_r: Vec<Complex<f32>>,
	scratch: Vec<Complex<f32>>,
	/// Third-octave band of each bin (`u8::MAX` = outside).
	band_of_bin: Vec<u8>,
	low_bins: usize,
	mid: [f64; 31],
	side: [f64; 31],
	low_cross: f64,
	low_ll: f64,
	low_rr: f64,
	frames: u64,
	timeline: Vec<[f32; 10]>,
}

impl std::fmt::Debug for SpectrumAnalyzer {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("SpectrumAnalyzer")
			.field("sample_rate", &self.sample_rate)
			.field("frames", &self.frames)
			.finish_non_exhaustive()
	}
}

impl SpectrumAnalyzer {
	pub fn new(sample_rate: u32) -> Self {
		let fft = RealFftPlanner::<f32>::new().plan_fft_forward(FRAME);
		let window: Vec<f32> = (0..FRAME)
			.map(|n| {
				let x = std::f32::consts::TAU * n as f32 / FRAME as f32;
				0.5 - 0.5 * x.cos()
			})
			.collect();
		let w2: f64 = window.iter().map(|w| f64::from(w * w)).sum();
		let bins = FRAME / 2 + 1;
		let sr = sample_rate.max(1) as f32;
		let band_of_bin = (0..bins)
			.map(|k| {
				let hz = k as f32 * sr / FRAME as f32;
				if k == 0 {
					u8::MAX
				} else {
					third_of_hz(hz).map_or(u8::MAX, |b| b as u8)
				}
			})
			.collect();
		let low_bins = ((LOW_END_HZ * FRAME as f32 / sr) as usize).clamp(1, bins - 1);
		Self {
			sample_rate,
			scratch: fft.make_scratch_vec(),
			spec_l: fft.make_output_vec(),
			spec_r: fft.make_output_vec(),
			input: fft.make_input_vec(),
			fft,
			window,
			scale: 2.0 / (FRAME as f64 * w2),
			left: vec![0.0; FRAME],
			right: vec![0.0; FRAME],
			filled: 0,
			fresh: 0,
			band_of_bin,
			low_bins,
			mid: [0.0; 31],
			side: [0.0; 31],
			low_cross: 0.0,
			low_ll: 0.0,
			low_rr: 0.0,
			frames: 0,
			timeline: Vec::new(),
		}
	}

	pub fn push(&mut self, left: &[f32], right: &[f32]) {
		let n = left.len().min(right.len());
		let mut i = 0;
		while i < n {
			let take = (FRAME - self.filled).min(n - i);
			self.left[self.filled..self.filled + take].copy_from_slice(&left[i..i + take]);
			self.right[self.filled..self.filled + take].copy_from_slice(&right[i..i + take]);
			self.filled += take;
			self.fresh += take;
			i += take;
			if self.filled == FRAME {
				self.process_frame();
				self.left.copy_within(HOP.., 0);
				self.right.copy_within(HOP.., 0);
				self.filled = FRAME - HOP;
				self.fresh = 0;
			}
		}
	}

	fn transform(&mut self, right: bool) {
		let src = if right { &self.right } else { &self.left };
		for ((dst, x), w) in self.input.iter_mut().zip(src).zip(&self.window) {
			*dst = x * w;
		}
		let out = if right {
			&mut self.spec_r
		} else {
			&mut self.spec_l
		};
		// Lengths come from the plan, so this cannot fail.
		let _ = self
			.fft
			.process_with_scratch(&mut self.input, out, &mut self.scratch);
	}

	fn process_frame(&mut self) {
		self.transform(false);
		self.transform(true);
		let mut mid = [0.0f64; 31];
		let mut side = [0.0f64; 31];
		for (k, (l, r)) in self.spec_l.iter().zip(&self.spec_r).enumerate() {
			let band = self.band_of_bin[k];
			let (l, r) = (
				Complex::new(f64::from(l.re), f64::from(l.im)),
				Complex::new(f64::from(r.re), f64::from(r.im)),
			);
			if k > 0 && k <= self.low_bins {
				self.low_cross += (l * r.conj()).re;
				self.low_ll += l.norm_sqr();
				self.low_rr += r.norm_sqr();
			}
			if band == u8::MAX {
				continue;
			}
			let m = (l + r) * 0.5;
			let s = (l - r) * 0.5;
			mid[band as usize] += m.norm_sqr();
			side[band as usize] += s.norm_sqr();
		}
		let mut octaves = [0.0f64; 10];
		for b in 0..31 {
			let (m, s) = (mid[b] * self.scale, side[b] * self.scale);
			self.mid[b] += m;
			self.side[b] += s;
			octaves[octave_of_third(b).min(9)] += m;
		}
		self.timeline.push(octaves.map(power_db));
		self.frames += 1;
	}

	/// Mean spectrum, stereo image and timeline. A trailing partial frame is zero-padded.
	pub fn finish(mut self) -> (Spectrum, Stereo, Timeline) {
		if self.fresh > 0 {
			self.left[self.filled..].fill(0.0);
			self.right[self.filled..].fill(0.0);
			self.process_frame();
		}
		let frames = self.frames.max(1) as f64;
		let mid: Vec<f32> = self.mid.iter().map(|e| power_db(e / frames)).collect();
		let side: Vec<f32> = self.side.iter().map(|e| power_db(e / frames)).collect();
		let mut octaves = [0.0f64; 10];
		for (b, e) in self.mid.iter().enumerate() {
			octaves[octave_of_third(b).min(9)] += e / frames;
		}
		let (mid_total, side_total): (f64, f64) = (self.mid.iter().sum(), self.side.iter().sum());
		let width = if mid_total + side_total > 0.0 {
			(side_total / (mid_total + side_total)) as f32
		} else {
			0.0
		};
		let low_correlation = if self.low_ll > 0.0 && self.low_rr > 0.0 {
			(self.low_cross / (self.low_ll * self.low_rr).sqrt()) as f32
		} else {
			1.0
		};
		let low_bands = third_of_hz(LOW_END_HZ).unwrap_or(8);
		let (low_mid, low_side): (f64, f64) = (
			self.mid[..low_bands].iter().sum(),
			self.side[..low_bands].iter().sum(),
		);
		let low_side_db = if low_mid > 0.0 {
			(power_db(low_side / frames) - power_db(low_mid / frames)).clamp(-60.0, 60.0)
		} else if low_side > 0.0 {
			60.0
		} else {
			-60.0
		};
		(
			Spectrum {
				third_octave_db: mid,
				side_third_octave_db: side,
				octave_db: octaves.iter().map(|e| power_db(*e)).collect(),
			},
			Stereo {
				width,
				low_correlation,
				low_side_db,
			},
			Timeline {
				hop_seconds: HOP as f64 / self.sample_rate.max(1) as f64,
				frames: self.timeline,
			},
		)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	const SR: u32 = 48_000;

	fn sine(hz: f32, amp: f32, seconds: f32) -> Vec<f32> {
		(0..(SR as f32 * seconds) as usize)
			.map(|n| (std::f32::consts::TAU * hz * n as f32 / SR as f32).sin() * amp)
			.collect()
	}

	fn analyze(l: &[f32], r: &[f32]) -> (Spectrum, Stereo, Timeline) {
		let mut a = SpectrumAnalyzer::new(SR);
		for (l, r) in l.chunks(1000).zip(r.chunks(1000)) {
			a.push(l, r);
		}
		a.finish()
	}

	#[test]
	fn a_sine_lands_in_its_band_at_its_mean_square_level() {
		let x = sine(1_000.0, 0.5, 2.0);
		let (s, stereo, timeline) = analyze(&x, &x);
		let band = third_of_hz(1_000.0).unwrap();
		assert_eq!(THIRD_OCTAVE_HZ[band], 1_000.0);
		// 0.5 amplitude sine: mean square 0.125 = -9.03 dBFS (the zero-padded last frame
		// costs a little).
		assert!((s.third_octave_db[band] + 9.03).abs() < 1.0, "{:?}", s);
		let others = s
			.third_octave_db
			.iter()
			.enumerate()
			.filter(|(b, _)| b.abs_diff(band) > 1)
			.map(|(_, v)| *v)
			.fold(FLOOR_DB, f32::max);
		assert!(others < -50.0, "leakage {others}");
		assert_eq!(s.peak_octave(), 5);
		assert_eq!(s.octave_shape()[5], 0.0);
		assert!(stereo.width < 1e-6, "identical channels are mono");
		assert!(!timeline.frames.is_empty() && timeline.hop_seconds > 0.08);
	}

	#[test]
	fn stereo_width_and_low_end_mono_check() {
		// Wide lows: a 60 Hz sine in opposite polarity on L/R is all side.
		let low = sine(60.0, 0.5, 2.0);
		let inv: Vec<f32> = low.iter().map(|v| -v).collect();
		let (_, wide, _) = analyze(&low, &inv);
		assert!(wide.width > 0.99);
		assert!(wide.low_correlation < -0.99);
		assert!(wide.low_side_db > 50.0);
		// Mono lows with uncorrelated highs: wide image, mono low end.
		let hl = sine(5_000.0, 0.3, 2.0);
		let hr = sine(7_000.0, 0.3, 2.0);
		let l: Vec<f32> = low.iter().zip(&hl).map(|(a, b)| a + b).collect();
		let r: Vec<f32> = low.iter().zip(&hr).map(|(a, b)| a + b).collect();
		let (_, s, _) = analyze(&l, &r);
		assert!(s.low_correlation > 0.99, "{s:?}");
		assert!(s.low_side_db < -30.0, "{s:?}");
		assert!(s.width > 0.1 && s.width < 0.5, "{s:?}");
	}

	#[test]
	fn silence_is_finite() {
		let z = vec![0.0; 20_000];
		let (s, stereo, _) = analyze(&z, &z);
		assert!(s.third_octave_db.iter().all(|v| *v == FLOOR_DB));
		assert_eq!(stereo.low_correlation, 1.0);
		assert_eq!(s.octave_shape(), vec![0.0; 10]);
	}

	#[test]
	fn band_mapping() {
		assert_eq!(third_of_hz(63.0), Some(5));
		assert_eq!(octave_of_third(5), 1);
		assert_eq!(octave_of_third(0), 0);
		assert_eq!(octave_of_third(30), 9);
		assert_eq!(third_of_hz(10.0), None);
		assert_eq!(
			OCTAVE_HZ[octave_of_third(third_of_hz(1_000.0).unwrap())],
			1_000.0
		);
	}
}
