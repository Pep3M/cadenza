//! TD-PSOLA (NE-4): pitch, formant and time edits by overlap-adding two-period grains cut
//! around the pitch marks.
//!
//! Each grain spans from the previous mark to the next one with complementary half-Hann
//! slopes (sin² rising, cos² falling), so grains laid back at their own marks sum to the
//! input exactly: an edit state without changes is a null render. Grains are laid out one
//! source period divided by the pitch ratio apart (pitch), read faster or slower (formant:
//! the grain's spectral envelope moves, the period spacing does not), and taken from the
//! source position the time map gives for the output position (repeated or skipped grains).

use std::f64::consts::FRAC_PI_2;

use crate::edit::Curves;
use crate::marks::Marks;
use crate::timemap::TimeMap;

/// Renders every channel of `channels` (same length) through the grains of `marks`.
pub fn psola(
	channels: &[&[f32]],
	rate: u32,
	marks: &Marks,
	curves: &Curves,
	map: &TimeMap,
) -> Vec<Vec<f32>> {
	let len = channels.first().map_or(0, |c| c.len());
	let rate = f64::from(rate);
	let out_len = if map.is_identity() {
		len
	} else {
		(map.to_output(len as f64 / rate) * rate).round().max(0.0) as usize
	};
	let mut out = vec![vec![0.0f64; out_len]; channels.len()];
	let pos = &marks.positions;
	if pos.is_empty() || out_len == 0 {
		return out
			.into_iter()
			.map(|c| c.into_iter().map(|x| x as f32).collect())
			.collect();
	}
	let last = pos.len() - 1;
	let mut o = 0.0f64;
	loop {
		let source = if map.is_identity() {
			o
		} else {
			map.to_source(o / rate) * rate
		};
		let k = marks.nearest(source);
		let at = pos[k] as f64;
		let (ratio, formant) = if marks.voiced[k] {
			(
				2f64.powf(curves.shift_at(at) / 12.0),
				2f64.powf(curves.formant_at(at) / 1200.0),
			)
		} else {
			(1.0, 1.0)
		};
		let left = if k > 0 {
			(pos[k] - pos[k - 1]) as f64
		} else {
			0.0
		};
		let right = if k < last {
			(pos[k + 1] - pos[k]) as f64
		} else {
			0.0
		};
		for (ch, buf) in channels.iter().zip(out.iter_mut()) {
			add_grain(buf, ch, o, at, left, right, formant);
		}
		let step = if right > 0.0 { right } else { left.max(1.0) };
		o += step / ratio;
		if o >= out_len as f64 {
			break;
		}
	}
	let gains: Vec<f64> = (0..out_len)
		.map(|n| {
			let source = if map.is_identity() {
				n as f64
			} else {
				map.to_source(n as f64 / rate) * rate
			};
			curves.gain_at(source)
		})
		.collect();
	out.into_iter()
		.map(|c| c.iter().zip(&gains).map(|(x, g)| (x * g) as f32).collect())
		.collect()
}

/// Adds the grain of the mark at source sample `at` (half lengths `left`/`right` in
/// source samples) centered on output sample `o`, read `formant` times faster.
fn add_grain(out: &mut [f64], src: &[f32], o: f64, at: f64, left: f64, right: f64, formant: f64) {
	let from = (o - left / formant).ceil().max(0.0) as usize;
	let to = ((o + right / formant).floor() as usize).min(out.len().saturating_sub(1));
	for (n, y) in out.iter_mut().enumerate().take(to + 1).skip(from) {
		let u = (n as f64 - o) * formant;
		let w = if u < 0.0 {
			if left <= 0.0 {
				continue;
			}
			let s = (FRAC_PI_2 * (1.0 + u / left)).sin();
			s * s
		} else if right > 0.0 {
			let c = (FRAC_PI_2 * u / right).cos();
			c * c
		} else if u == 0.0 {
			1.0
		} else {
			continue;
		};
		*y += w * read(src, at + u);
	}
}

/// `src` at a fractional position (Catmull-Rom; exact on whole samples), 0 outside.
fn read(src: &[f32], x: f64) -> f64 {
	let i = x.floor();
	let t = x - i;
	let i = i as isize;
	let get = |j: isize| -> f64 {
		if j < 0 || j as usize >= src.len() {
			0.0
		} else {
			f64::from(src[j as usize])
		}
	};
	let x0 = get(i);
	if t == 0.0 {
		return x0;
	}
	let (xm, x1, x2) = (get(i - 1), get(i + 1), get(i + 2));
	let a = -0.5 * xm + 1.5 * x0 - 1.5 * x1 + 0.5 * x2;
	let b = xm - 2.5 * x0 + 2.0 * x1 - 0.5 * x2;
	let c = -0.5 * xm + 0.5 * x1;
	((a * t + b) * t + c) * t + x0
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn reading_between_samples_is_smooth_and_exact_on_them() {
		let src = [0.0f32, 1.0, 2.0, 3.0, 4.0];
		assert_eq!(read(&src, 2.0), 2.0);
		assert!((read(&src, 1.5) - 1.5).abs() < 1e-12);
		assert_eq!(read(&src, -3.0), 0.0);
	}
}
