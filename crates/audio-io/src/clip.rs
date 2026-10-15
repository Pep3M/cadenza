//! Realtime-safe clip DSP: fade curves, gain, crossfades and reading a clip region
//! into an output block. Nothing in this module allocates, locks or does I/O.

use std::f32::consts::{FRAC_PI_2, PI};

use serde::{Deserialize, Serialize};

use crate::buffer::{AudioBuffer, stereo_downmix_gains};

/// Shape of a fade. Every curve goes 0 -> 1 for a fade-in; a fade-out is the
/// time-mirrored fade-in.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FadeCurve {
	/// Gain changes linearly; in a crossfade the two sides sum to unity amplitude
	/// (right for correlated material, e.g. edits within the same take).
	#[default]
	Linear,
	/// `sin`/`cos` quarter wave; in a crossfade the two sides sum to unity power
	/// (right for uncorrelated material, avoids the mid-fade dip).
	EqualPower,
	/// Linear in dB over a 60 dB range: sounds even to the ear for long fade-outs.
	Exponential,
	/// Raised cosine: slow start and end, smooth (no slope discontinuity).
	SCurve,
}

impl FadeCurve {
	/// Fade-in gain at normalised position `t` (clamped to `[0, 1]`).
	#[inline]
	pub fn fade_in(self, t: f32) -> f32 {
		let t = t.clamp(0.0, 1.0);
		match self {
			FadeCurve::Linear => t,
			FadeCurve::EqualPower => (t * FRAC_PI_2).sin(),
			FadeCurve::Exponential => {
				const FLOOR: f32 = 1e-3; // -60 dB
				((10.0f32).powf(3.0 * (t - 1.0)) - FLOOR) / (1.0 - FLOOR)
			}
			FadeCurve::SCurve => 0.5 - 0.5 * (t * PI).cos(),
		}
	}

	/// Fade-out gain at normalised position `t` (1 at `t = 0`, 0 at `t = 1`).
	#[inline]
	pub fn fade_out(self, t: f32) -> f32 {
		self.fade_in(1.0 - t)
	}
}

/// A fade of `frames` length with a given curve; `frames == 0` means no fade.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Fade {
	pub frames: u64,
	pub curve: FadeCurve,
}

impl Fade {
	pub const NONE: Fade = Fade {
		frames: 0,
		curve: FadeCurve::Linear,
	};

	pub fn new(frames: u64, curve: FadeCurve) -> Self {
		Self { frames, curve }
	}
}

/// Which way a fade goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FadeDirection {
	In,
	Out,
}

/// Multiplies `samples` by a linear `gain`.
#[inline]
pub fn apply_gain(samples: &mut [f32], gain: f32) {
	if gain == 1.0 {
		return;
	}
	for s in samples {
		*s *= gain;
	}
}

/// Multiplies `samples` by a linear ramp starting at `from` and reaching `to` on the
/// sample just after the block, so consecutive blocks chain seamlessly.
#[inline]
pub fn apply_gain_ramp(samples: &mut [f32], from: f32, to: f32) {
	let n = samples.len();
	if n == 0 {
		return;
	}
	let step = (to - from) / n as f32;
	for (i, s) in samples.iter_mut().enumerate() {
		*s *= from + step * i as f32;
	}
}

/// Applies part of a fade to `samples`. `position` is the index within the fade of
/// `samples[0]`; samples past the fade end are left unchanged for a fade-in and
/// silenced for a fade-out.
pub fn apply_fade(samples: &mut [f32], fade: Fade, direction: FadeDirection, position: u64) {
	if fade.frames == 0 {
		return;
	}
	let len = fade.frames as f32;
	for (i, s) in samples.iter_mut().enumerate() {
		let p = position + i as u64;
		let g = if p >= fade.frames {
			match direction {
				FadeDirection::In => 1.0,
				FadeDirection::Out => 0.0,
			}
		} else {
			let t = p as f32 / len;
			match direction {
				FadeDirection::In => fade.curve.fade_in(t),
				FadeDirection::Out => fade.curve.fade_out(t),
			}
		};
		*s *= g;
	}
}

/// Writes a crossfade of `outgoing` into `incoming` to `out` (all the same length).
/// `position` is the index within a crossfade of `length` frames of the first sample.
pub fn crossfade(
	out: &mut [f32],
	outgoing: &[f32],
	incoming: &[f32],
	curve: FadeCurve,
	position: u64,
	length: u64,
) {
	let len = length.max(1) as f32;
	for (i, ((o, &a), &b)) in out.iter_mut().zip(outgoing).zip(incoming).enumerate() {
		let t = (position + i as u64) as f32 / len;
		*o = a * curve.fade_out(t) + b * curve.fade_in(t);
	}
}

/// A region of a source buffer placed as a clip. All frame values are at the
/// source buffer's sample rate (the engine resamples sources to its own rate on load).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ClipRegion {
	/// Source frame heard at the clip's first frame (trim start).
	pub source_offset: u64,
	/// Clip length in frames. Frames past the source end render as silence.
	pub length: u64,
	/// Linear gain.
	pub gain: f32,
	pub fade_in: Fade,
	pub fade_out: Fade,
}

impl ClipRegion {
	/// Whole-buffer region at unity gain without fades.
	pub fn whole(source: &AudioBuffer) -> Self {
		Self {
			source_offset: 0,
			length: source.frames() as u64,
			gain: 1.0,
			fade_in: Fade::NONE,
			fade_out: Fade::NONE,
		}
	}

	/// Gain (clip gain x fades) at clip-relative frame `p` (`0 <= p < length`).
	/// Overlapping fade-in and fade-out multiply.
	#[inline]
	pub fn gain_at(&self, p: u64) -> f32 {
		let mut g = self.gain;
		if p < self.fade_in.frames {
			g *= self
				.fade_in
				.curve
				.fade_in(p as f32 / self.fade_in.frames as f32);
		}
		let fade_out_start = self.length.saturating_sub(self.fade_out.frames);
		if self.fade_out.frames > 0 && p >= fade_out_start {
			g *= self
				.fade_out
				.curve
				.fade_out((p - fade_out_start) as f32 / self.fade_out.frames as f32);
		}
		g
	}
}

/// Mixes (adds) a clip into a stereo output block. Realtime safe.
///
/// `clip_frame` is the clip-relative frame of `left[0]` (negative when the block
/// starts before the clip). Mono sources feed both sides; sources with more than two
/// channels are folded down on the fly with [`stereo_downmix_gains`].
/// `left` and `right` must have the same length.
pub fn render_clip(
	source: &AudioBuffer,
	region: &ClipRegion,
	clip_frame: i64,
	left: &mut [f32],
	right: &mut [f32],
) {
	let n = left.len().min(right.len()) as i64;
	let length = i64::try_from(region.length).unwrap_or(i64::MAX);
	let first = (-clip_frame).clamp(0, n);
	let last = (length - clip_frame).clamp(0, n);
	if first >= last {
		return;
	}
	let src_len = source.frames() as u64;
	let n_ch = source.num_channels();
	for i in first..last {
		let p = (clip_frame + i) as u64;
		let src = region.source_offset + p;
		if src >= src_len {
			break;
		}
		let src = src as usize;
		let g = region.gain_at(p);
		let (l, r) = match n_ch {
			1 => {
				let m = source.channel(0)[src];
				(m, m)
			}
			2 => (source.channel(0)[src], source.channel(1)[src]),
			_ => {
				let (mut l, mut r) = (0.0, 0.0);
				for c in 0..n_ch {
					let s = source.channel(c)[src];
					let (gl, gr) = stereo_downmix_gains(n_ch, c);
					l += s * gl;
					r += s * gr;
				}
				(l, r)
			}
		};
		let i = i as usize;
		left[i] += l * g;
		right[i] += r * g;
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	const CURVES: [FadeCurve; 4] = [
		FadeCurve::Linear,
		FadeCurve::EqualPower,
		FadeCurve::Exponential,
		FadeCurve::SCurve,
	];

	#[test]
	fn curves_hit_endpoints_and_are_monotonic() {
		for c in CURVES {
			assert!(c.fade_in(0.0).abs() < 1e-6, "{c:?}");
			assert!((c.fade_in(1.0) - 1.0).abs() < 1e-6, "{c:?}");
			assert!((c.fade_out(0.0) - 1.0).abs() < 1e-6, "{c:?}");
			let mut prev = -1.0;
			for i in 0..=100 {
				let g = c.fade_in(i as f32 / 100.0);
				assert!(g >= prev, "{c:?} not monotonic");
				prev = g;
			}
		}
	}

	#[test]
	fn equal_power_crossfade_keeps_power_and_linear_keeps_amplitude() {
		for i in 0..=64 {
			let t = i as f32 / 64.0;
			let (a, b) = (
				FadeCurve::EqualPower.fade_out(t),
				FadeCurve::EqualPower.fade_in(t),
			);
			assert!((a * a + b * b - 1.0).abs() < 1e-5);
			let (a, b) = (FadeCurve::Linear.fade_out(t), FadeCurve::Linear.fade_in(t));
			assert!((a + b - 1.0).abs() < 1e-6);
		}
	}

	#[test]
	fn exponential_midpoint_is_about_minus_30_db() {
		let g = FadeCurve::Exponential.fade_in(0.5);
		let db = 20.0 * g.log10();
		assert!((db + 30.0).abs() < 0.5, "{db}");
	}

	#[test]
	fn crossfade_of_identical_signals_is_transparent_with_linear() {
		let a = [0.5f32; 16];
		let mut out = [0.0f32; 16];
		crossfade(&mut out, &a, &a, FadeCurve::Linear, 0, 16);
		assert!(out.iter().all(|&s| (s - 0.5).abs() < 1e-6));
	}

	#[test]
	fn split_fade_equals_whole_fade() {
		let fade = Fade::new(100, FadeCurve::SCurve);
		let mut whole = [1.0f32; 120];
		apply_fade(&mut whole, fade, FadeDirection::Out, 0);
		let mut parts = [1.0f32; 120];
		let (a, b) = parts.split_at_mut(37);
		apply_fade(a, fade, FadeDirection::Out, 0);
		apply_fade(b, fade, FadeDirection::Out, 37);
		assert_eq!(whole, parts);
		assert_eq!(whole[110], 0.0);
	}

	#[test]
	fn render_clip_handles_offset_bounds_and_fades() {
		let src: Vec<f32> = (0..100).map(|i| i as f32).collect();
		let source = AudioBuffer::from_planar(48_000, vec![src]).unwrap();
		let region = ClipRegion {
			source_offset: 10,
			length: 50,
			gain: 2.0,
			fade_in: Fade::new(4, FadeCurve::Linear),
			fade_out: Fade::NONE,
		};
		// Block starts 2 frames before the clip.
		let mut l = [0.0f32; 8];
		let mut r = [0.0f32; 8];
		render_clip(&source, &region, -2, &mut l, &mut r);
		assert_eq!(&l[..2], &[0.0, 0.0]);
		// clip frame 0 -> src 10, fade gain 0.
		assert_eq!(l[2], 0.0);
		// clip frame 1 -> src 11 * 2 * 0.25
		assert!((l[3] - 5.5).abs() < 1e-6);
		// clip frame 4 -> past fade: src 14 * 2
		assert!((l[6] - 28.0).abs() < 1e-6);
		assert_eq!(l, r);

		// Block crossing the clip end: frames >= 50 untouched.
		let mut l = [1.0f32; 8];
		let mut r = [1.0f32; 8];
		render_clip(&source, &region, 46, &mut l, &mut r);
		assert!((l[0] - (1.0 + 56.0 * 2.0)).abs() < 1e-4);
		assert_eq!(l[4], 1.0);
	}

	#[test]
	fn render_clip_stops_at_source_end() {
		let source = AudioBuffer::from_planar(48_000, vec![vec![1.0; 4], vec![-1.0; 4]]).unwrap();
		let region = ClipRegion {
			length: 10,
			..ClipRegion::whole(&source)
		};
		let mut l = [0.0f32; 10];
		let mut r = [0.0f32; 10];
		render_clip(&source, &region, 0, &mut l, &mut r);
		assert_eq!(l[3], 1.0);
		assert_eq!(r[3], -1.0);
		assert_eq!(l[4], 0.0);
	}

	#[test]
	fn fade_out_reaches_silence_at_clip_end() {
		let source = AudioBuffer::from_planar(48_000, vec![vec![1.0; 64]]).unwrap();
		let region = ClipRegion {
			fade_out: Fade::new(16, FadeCurve::EqualPower),
			..ClipRegion::whole(&source)
		};
		assert!((region.gain_at(47) - 1.0).abs() < 1e-6);
		assert!((region.gain_at(48) - 1.0).abs() < 1e-6);
		assert!(region.gain_at(63) < 0.1);
	}
}
