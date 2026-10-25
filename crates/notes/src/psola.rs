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

/// One grain of a render: mark `mark` laid at output sample `o`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Grain {
	pub o: f64,
	pub mark: usize,
}

/// No grain reaches further than this from its center, seconds (two periods read at half
/// speed).
pub const REACH_SECONDS: f64 = 0.1;

/// Renders every channel of `channels` (same length) through the grains of `marks`.
pub fn psola(
	channels: &[&[f32]],
	rate: u32,
	marks: &Marks,
	curves: &Curves,
	map: &TimeMap,
) -> Vec<Vec<f32>> {
	let out_len = output_len(channels, rate, map);
	let grains = plan(rate, marks, curves, map, out_len);
	synth(channels, rate, marks, curves, map, &grains, 0..out_len)
}

/// Samples `range` of the render laid out by `grains` (its [`plan`]): the same samples as
/// [`psola`] gives.
pub fn psola_range(
	channels: &[&[f32]],
	rate: u32,
	marks: &Marks,
	curves: &Curves,
	map: &TimeMap,
	grains: &[Grain],
	range: std::ops::Range<usize>,
) -> Vec<Vec<f32>> {
	synth(channels, rate, marks, curves, map, grains, range)
}

/// Length of the render of `channels` through `map`, samples.
pub fn output_len(channels: &[&[f32]], rate: u32, map: &TimeMap) -> usize {
	let len = channels.first().map_or(0, |c| c.len());
	if map.is_identity() {
		len
	} else {
		(map.to_output(len as f64 / f64::from(rate)) * f64::from(rate))
			.round()
			.max(0.0) as usize
	}
}

/// Marks where an identity render falls back on the source's own grid: the third of a run
/// of unpitched marks. Two renders that differ only before such a mark lay the same grains
/// after it, so an edit changes the render only up to the next run.
fn resync(marks: &Marks, k: usize) -> bool {
	k >= 2 && !marks.voiced[k] && !marks.voiced[k - 1] && !marks.voiced[k - 2]
}

/// Where every grain of the render goes (cheap: no audio is touched). Grains sit one
/// source period / pitch ratio apart, each taken from the mark nearest to the source
/// position its output position maps to.
pub fn plan(
	rate: u32,
	marks: &Marks,
	curves: &Curves,
	map: &TimeMap,
	out_len: usize,
) -> Vec<Grain> {
	let rate = f64::from(rate);
	let pos = &marks.positions;
	let mut grains = Vec::new();
	if pos.is_empty() || out_len == 0 {
		return grains;
	}
	let identity = map.is_identity();
	let last = pos.len() - 1;
	let mut o = 0.0f64;
	loop {
		let source = if identity {
			o
		} else {
			map.to_source(o / rate) * rate
		};
		let k = marks.nearest(source);
		if identity && resync(marks, k) {
			o = pos[k] as f64;
		}
		grains.push(Grain { o, mark: k });
		let (ratio, _) = factors(marks, curves, k);
		let right = if k < last {
			(pos[k + 1] - pos[k]) as f64
		} else {
			0.0
		};
		let left = if k > 0 {
			(pos[k] - pos[k - 1]) as f64
		} else {
			0.0
		};
		let step = if right > 0.0 { right } else { left.max(1.0) };
		o += step / ratio;
		if o >= out_len as f64 {
			break;
		}
	}
	grains
}

/// Output samples (start, end) where two plans of the same audio lay different grains,
/// widened by the grains' reach; `None` when they are the same.
pub fn changed_span(rate: u32, old: &[Grain], new: &[Grain]) -> Option<(f64, f64)> {
	let prefix = old.iter().zip(new).take_while(|(a, b)| a == b).count();
	if prefix == old.len() && prefix == new.len() {
		return None;
	}
	let suffix = old
		.iter()
		.rev()
		.zip(new.iter().rev())
		.take_while(|(a, b)| a == b)
		.count()
		.min(old.len() - prefix)
		.min(new.len() - prefix);
	let reach = REACH_SECONDS * f64::from(rate);
	let at = |g: &[Grain], i: usize| g.get(i).map(|g| g.o);
	let first = [at(old, prefix), at(new, prefix)]
		.into_iter()
		.flatten()
		.fold(f64::INFINITY, f64::min);
	let last = [
		at(old, old.len() - suffix - 1),
		at(new, new.len() - suffix - 1),
		at(old, old.len() - suffix),
		at(new, new.len() - suffix),
	]
	.into_iter()
	.flatten()
	.fold(f64::NEG_INFINITY, f64::max);
	Some(((first - reach).max(0.0), last + reach))
}

fn factors(marks: &Marks, curves: &Curves, k: usize) -> (f64, f64) {
	if marks.voiced[k] {
		let at = marks.positions[k] as f64;
		(
			2f64.powf(curves.shift_at(at) / 12.0),
			2f64.powf(curves.formant_at(at) / 1200.0),
		)
	} else {
		(1.0, 1.0)
	}
}

/// Output samples `range` of the render laid out by `grains`.
fn synth(
	channels: &[&[f32]],
	rate: u32,
	marks: &Marks,
	curves: &Curves,
	map: &TimeMap,
	grains: &[Grain],
	range: std::ops::Range<usize>,
) -> Vec<Vec<f32>> {
	let rate = f64::from(rate);
	let from = range.start;
	let len = range.end.saturating_sub(range.start);
	let mut out = vec![vec![0.0f64; len]; channels.len()];
	let pos = &marks.positions;
	let reach = REACH_SECONDS * rate;
	let last = pos.len().saturating_sub(1);
	let first = grains.partition_point(|g| g.o + reach < from as f64);
	for g in &grains[first..] {
		if g.o - reach > range.end as f64 {
			break;
		}
		let k = g.mark;
		let (_, formant) = factors(marks, curves, k);
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
			add_grain(
				buf,
				ch,
				g.o - from as f64,
				pos[k] as f64,
				left,
				right,
				formant,
			);
		}
	}
	let identity = map.is_identity();
	out.into_iter()
		.map(|c| {
			c.iter()
				.enumerate()
				.map(|(i, x)| {
					let n = (from + i) as f64;
					let source = if identity {
						n
					} else {
						map.to_source(n / rate) * rate
					};
					(x * curves.gain_at(source)) as f32
				})
				.collect()
		})
		.collect()
}

/// Adds the grain of the mark at source sample `at` (half lengths `left`/`right` in
/// source samples) centered on output sample `o`, read `formant` times faster.
fn add_grain(out: &mut [f64], src: &[f32], o: f64, at: f64, left: f64, right: f64, formant: f64) {
	let from = (o - left / formant).ceil().max(0.0) as usize;
	let end = (o + right / formant).floor();
	if end < 0.0 || out.is_empty() {
		return;
	}
	let to = (end as usize).min(out.len() - 1);
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
