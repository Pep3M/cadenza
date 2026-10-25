//! Pitch marks (NE-4): one mark per period on voiced stretches, at the same point of each
//! cycle, plus evenly spaced marks elsewhere so the marks tile the whole recording.
//!
//! PSOLA cuts a two-period grain around every mark, so the marks must sit at the same
//! point of every cycle, and that point must be the cycle's pulse (glottal closure, where
//! the harmonics line up): grains centered elsewhere interpolate the spectrum between the
//! harmonics out of phase and lose level when re-spaced. Each cycle is predicted one period
//! (from the analysis pitch) ahead; the peak of the fundamental nearest to it, measured from
//! its phase (absolute, so nothing drifts; narrow band, so noise barely moves it), keeps
//! the time, and the mark sits at the pulse — the peak of the period-rate component of the
//! derivative's energy — through its offset from that peak (a weak pulse, jittering in
//! noise, takes the median offset of the cycles around it). Without clear pulses (a
//! near-sine) the mark is the fundamental's peak. Peak picking on the raw waveform jumps
//! between look-alike peaks of one cycle (strong second harmonic, formant ringing).

use crate::analysis::Analysis;

/// Spacing of the marks on unvoiced stretches, seconds.
pub const UNVOICED_SPACING: f64 = 0.005;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Marks {
	/// Sample positions, strictly increasing; the first is 0 and the last `len - 1`.
	pub positions: Vec<usize>,
	/// Whether each mark sits on a voiced stretch (the only ones a pitch shift applies to).
	pub voiced: Vec<bool>,
}

impl Marks {
	pub fn len(&self) -> usize {
		self.positions.len()
	}

	pub fn is_empty(&self) -> bool {
		self.positions.is_empty()
	}

	/// Index of the mark nearest to `sample`.
	pub fn nearest(&self, sample: f64) -> usize {
		let p = &self.positions;
		let i = p.partition_point(|&m| (m as f64) < sample);
		if i == 0 {
			return 0;
		}
		if i == p.len() {
			return p.len() - 1;
		}
		if sample - p[i - 1] as f64 <= p[i] as f64 - sample {
			i - 1
		} else {
			i
		}
	}
}

/// Marks of `samples` (mono, the signal the analysis was made from).
pub fn pitch_marks(samples: &[f32], analysis: &Analysis) -> Marks {
	let len = samples.len();
	let mut marks = Marks::default();
	if len < 2 {
		marks.positions = (0..len).collect();
		marks.voiced = vec![false; len];
		return marks;
	}
	let rate = f64::from(analysis.sample_rate);
	let spacing = (UNVOICED_SPACING * rate).max(1.0);
	let pitch = pitched(analysis);
	let mut voiced: Vec<usize> = Vec::new();
	let mut regions: Vec<(usize, usize)> = Vec::new();
	for (a, b) in voiced_frames(&pitch) {
		let start = analysis.frames[a].time - analysis.hop_seconds() / 2.0;
		let end = analysis.frames[b - 1].time + analysis.hop_seconds() / 2.0;
		let (start, end) = (
			((start * rate).max(0.0) as usize).min(len - 1),
			((end * rate) as usize).min(len - 1),
		);
		let first = voiced.len();
		let period = |sample: f64| period_at(analysis, &pitch, (a, b), sample, rate);
		mark_region(samples, &period, (start, end), &mut voiced);
		if voiced.len() - first < 3 {
			voiced.truncate(first);
		} else {
			regions.push((first, voiced.len()));
		}
	}

	let push = |m: &mut Marks, at: usize, v: bool| {
		if m.positions.last().is_none_or(|&last| at > last) {
			m.positions.push(at);
			m.voiced.push(v);
		}
	};
	let fill = |m: &mut Marks, from: usize, to: usize| {
		let gap = (to - from) as f64;
		let count = (gap / spacing).ceil().max(1.0) as usize;
		for j in 1..count {
			push(
				m,
				from + (gap * j as f64 / count as f64).round() as usize,
				false,
			);
		}
	};
	push(&mut marks, 0, false);
	for &(a, b) in &regions {
		let from = *marks.positions.last().unwrap_or(&0);
		fill(&mut marks, from, voiced[a]);
		for &v in &voiced[a..b] {
			push(&mut marks, v, true);
		}
	}
	let from = *marks.positions.last().unwrap_or(&0);
	fill(&mut marks, from, len - 1);
	push(&mut marks, len - 1, false);
	marks
}

/// Frames a voice's pitch can be shifted on: the voiced ones, plus up to [`EDGE_FRAMES`]
/// on each side of a voiced stretch that still hold a period close to its edge and are not
/// much quieter (a note's onset and decay outlast what the tracker calls voiced; left
/// unshifted, a short note keeps sounding at its old pitch at both ends).
pub fn pitched(analysis: &Analysis) -> Vec<Option<f64>> {
	let frames = &analysis.frames;
	let mut out: Vec<Option<f64>> = frames.iter().map(|f| f.pitch).collect();
	let extend = |out: &mut Vec<Option<f64>>, edge: usize, step: isize| {
		let (Some(mut pitch), level) = (out[edge], frames[edge].level_db) else {
			return;
		};
		let mut k = edge as isize;
		for _ in 0..EDGE_FRAMES {
			k += step;
			if k < 0 || k as usize >= frames.len() || out[k as usize].is_some() {
				return;
			}
			let f = &frames[k as usize];
			let Some(c) = f
				.candidates
				.iter()
				.filter(|c| (c.pitch - pitch).abs() < EDGE_SEMITONES && c.aperiodicity < 0.5)
				.min_by(|x, y| (x.pitch - pitch).abs().total_cmp(&(y.pitch - pitch).abs()))
			else {
				return;
			};
			if f.level_db < level - EDGE_DROP_DB {
				return;
			}
			pitch = c.pitch;
			out[k as usize] = Some(pitch);
		}
	};
	for (a, b) in voiced_frames(&frames.iter().map(|f| f.pitch).collect::<Vec<_>>()) {
		extend(&mut out, a, -1);
		extend(&mut out, b - 1, 1);
	}
	out
}

/// Most frames added at each end of a voiced stretch by [`pitched`].
pub const EDGE_FRAMES: usize = 3;
const EDGE_SEMITONES: f64 = 1.0;
const EDGE_DROP_DB: f64 = 30.0;

/// Ranges `a..b` of consecutive frames with a pitch.
fn voiced_frames(pitch: &[Option<f64>]) -> Vec<(usize, usize)> {
	let mut out = Vec::new();
	let mut start = None;
	for (i, p) in pitch.iter().enumerate() {
		match (p.is_some(), start) {
			(true, None) => start = Some(i),
			(false, Some(s)) => {
				out.push((s, i));
				start = None;
			}
			_ => {}
		}
	}
	if let Some(s) = start {
		out.push((s, pitch.len()));
	}
	out
}

/// Period in samples at `sample`, from the pitch of the frames `a..b`.
fn period_at(
	analysis: &Analysis,
	pitch: &[Option<f64>],
	(a, b): (usize, usize),
	sample: f64,
	rate: f64,
) -> f64 {
	let frames = &analysis.frames;
	let hop = analysis.hop_seconds();
	let t = sample / rate;
	let x = ((t - frames[a].time) / hop).clamp(0.0, (b - a - 1) as f64);
	let i = a + x.floor() as usize;
	let j = (i + 1).min(b - 1);
	let w = x - x.floor();
	let p = pitch[i].unwrap_or(60.0) * (1.0 - w) + pitch[j].unwrap_or(60.0) * w;
	rate / crate::midi_to_hz(p)
}

fn mark_region(
	samples: &[f32],
	period: &dyn Fn(f64) -> f64,
	(start, end): (usize, usize),
	out: &mut Vec<usize>,
) {
	if end <= start {
		return;
	}
	let pulses: Vec<f32> = (start..=end)
		.map(|i| {
			let d = samples[i] - samples[i.saturating_sub(1).max(start)];
			d * d
		})
		.collect();
	// The fundamental's peaks keep the time (narrow band: robust in noise); the pulse sits
	// at a slowly changing offset from them, measured per cycle and median-smoothed.
	// (fundamental peak, period, pulse offset and strength) per cycle.
	type Cycle = (f64, f64, Option<(f64, f64)>);
	let mut cycles: Vec<Cycle> = Vec::new();
	let mut at = start as f64 + 0.5 * period(start as f64);
	while at <= end as f64 {
		let p = period(at);
		let peak = match cycle_peak(samples, at, p) {
			Some((peak, strength)) if strength >= FUNDAMENTAL_STRENGTH => peak,
			_ => at,
		};
		let offset = cycle_peak(&pulses, peak - start as f64, p)
			.filter(|(_, strength)| *strength >= PULSE_STRENGTH)
			.map(|(pulse, strength)| {
				let d = pulse + start as f64 - peak;
				(d - p * (d / p).round(), strength)
			});
		cycles.push((peak, p, offset));
		at = peak + p;
	}
	for (k, &(peak, p, own)) in cycles.iter().enumerate() {
		let offset = match own {
			Some((o, strength)) if strength >= CLEAR_PULSE => o,
			_ => {
				let mut near: Vec<f64> = cycles
					[k.saturating_sub(SMOOTH)..(k + SMOOTH + 1).min(cycles.len())]
					.iter()
					.filter_map(|c| c.2.map(|(o, _)| o / c.1))
					.collect();
				if near.is_empty() {
					0.0
				} else {
					near.sort_by(f64::total_cmp);
					near[near.len() / 2] * p
				}
			}
		};
		let mark = (peak + offset).round().clamp(start as f64, end as f64) as usize;
		if out.last().is_none_or(|&last| mark > last) {
			out.push(mark);
		}
	}
}

/// A cycle-energy component weaker than this (relative to its mean) is no pulse train.
const PULSE_STRENGTH: f64 = 0.25;
/// A fundamental weaker than this (relative to the mean magnitude) does not keep the time.
const FUNDAMENTAL_STRENGTH: f64 = 0.2;
/// A pulse at least this strong places its own mark; weaker ones (noise) take the median
/// offset of their neighbours.
const CLEAR_PULSE: f64 = 0.5;
/// Cycles on each side of the median that smooths the pulse offset.
const SMOOTH: usize = 5;

/// Peak, nearest to `at`, of the component of `signal` at the period `p` samples, from its
/// phase over a two-period Hann window: an absolute, sub-sample position (no drift from one
/// mark to the next; the window's zeros fall on the other harmonics), with the component's
/// strength relative to the signal's mean magnitude. `None` when the window is silent.
fn cycle_peak(signal: &[f32], at: f64, p: f64) -> Option<(f64, f64)> {
	let half = p.round() as isize;
	let center = at.round() as isize;
	let omega = std::f64::consts::TAU / p;
	let (mut re, mut im, mut sum, mut weight) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
	for j in -half..=half {
		let n = center + j;
		if n < 0 || n as usize >= signal.len() {
			continue;
		}
		let x = f64::from(signal[n as usize]);
		let u = n as f64 - at;
		let w = 0.5 + 0.5 * (std::f64::consts::PI * u / p).cos();
		re += w * x * (omega * u).cos();
		im -= w * x * (omega * u).sin();
		sum += w * x.abs();
		weight += w;
	}
	if sum <= 1e-20 || weight <= 0.0 {
		return None;
	}
	// x ≈ |C| cos(ω u + φ): its peak nearest u = 0 is at u = -φ / ω.
	let strength = 2.0 * (re * re + im * im).sqrt() / sum;
	Some((at - im.atan2(re) / omega, strength))
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::analysis::{AnalysisOptions, analyze};

	#[test]
	fn marks_follow_the_period_and_tile_the_recording() {
		let rate = 22_050u32;
		let hz = 220.0;
		let samples: Vec<f32> = (0..rate as usize)
			.map(|i| {
				let t = i as f64 / f64::from(rate);
				let env = if (0.2..0.8).contains(&t) { 1.0 } else { 0.0 };
				let ph = 2.0 * std::f64::consts::PI * hz * t;
				(env * (ph.sin() + 0.5 * (2.0 * ph).sin() + 0.25 * (3.0 * ph).sin())) as f32 * 0.5
			})
			.collect();
		let analysis = analyze(&samples, rate, &AnalysisOptions::default(), &mut |_| true).unwrap();
		let marks = pitch_marks(&samples, &analysis);
		assert_eq!(marks.positions[0], 0);
		assert_eq!(*marks.positions.last().unwrap(), samples.len() - 1);
		assert!(marks.positions.windows(2).all(|w| w[1] > w[0]));
		let voiced: Vec<usize> = marks
			.positions
			.iter()
			.zip(&marks.voiced)
			.filter(|(_, v)| **v)
			.map(|(p, _)| *p)
			.collect();
		let period = f64::from(rate) / hz;
		// About 0.6 s of periods (the analysis window smears the edges a little), each one
		// period apart.
		assert!(
			(voiced.len() as f64 - 0.6 * hz).abs() < 12.0,
			"{}",
			voiced.len()
		);
		// Inside the tone (marks in the smeared silence around it have nothing to follow).
		let inside = |m: usize| (0.21..0.79).contains(&(m as f64 / f64::from(rate)));
		for w in voiced.windows(2).filter(|w| inside(w[0]) && inside(w[1])) {
			assert!(((w[1] - w[0]) as f64 - period).abs() <= 1.0, "{w:?}");
		}
		// Every unvoiced gap is at most the unvoiced spacing (+ rounding).
		let spacing = UNVOICED_SPACING * f64::from(rate);
		for (w, v) in marks.positions.windows(2).zip(marks.voiced.windows(2)) {
			if !v[0] || !v[1] {
				assert!(((w[1] - w[0]) as f64) <= spacing.max(period) + 1.0, "{w:?}");
			}
		}
	}
}
