//! Probabilistic YIN (Mauch & Dixon 2014), candidate form.
//!
//! Stage 1: instead of one YIN threshold, a Beta distribution over 100 thresholds. For each
//! threshold the first dip of the difference function below it gets that threshold's
//! probability; a frame thus yields a few period candidates (typically the true period and
//! its multiples/submultiples) with probabilities, and the total is its voicing mass.
//!
//! Stage 2: the most likely path through the candidates plus an unvoiced state (Viterbi):
//! emissions are the candidate probabilities (unvoiced: the remaining mass), transitions
//! cost in proportion to the pitch move per frame (capped, so a real leap stays possible)
//! and a fixed price for switching voicing. Working on candidates rather than a dense pitch
//! grid keeps it O(frames x candidates^2): a 3-minute take decodes in milliseconds, and other
//! pitch sources (a neural estimator) can add candidates to the same lattice.

use crate::par::map_frames;
use crate::yin::Cmnd;
use crate::{PitchFrame, PitchTracker};

#[derive(Debug, Clone)]
pub struct PyinOptions {
	pub hop_seconds: f64,
	pub min_hz: f64,
	pub max_hz: f64,
	/// Beta(alpha, beta) prior over the YIN threshold. pYIN's paper default has mean 0.1;
	/// mean 0.2 keeps noisy and fast passages voiced without octave errors (tuned on the
	/// NE-0 corpus: raw pitch 93.7 % -> 98.5 %, octave errors 0).
	pub beta: (f64, f64),
	/// Share of a threshold's probability given to the global minimum when no dip is below
	/// it (pYIN's "absolute minimum" rule).
	pub no_dip_probability: f64,
	pub max_candidates: usize,
	/// A dip below the threshold is passed over for a later one this many times deeper: a
	/// strong second harmonic makes a shallow dip at half the period (an octave up), while a
	/// true period leaves its subharmonic dips about as deep as its own.
	pub deeper_ratio: f64,
	/// ...only when that dip is itself this aperiodic: a clean period has tiny dips everywhere.
	pub deeper_above: f64,
	/// Frames quieter than the loudest frame by more than this are unvoiced.
	pub silence_db: f64,
	/// Fade-in of the voicing mass over this many dB above the silence floor.
	pub silence_knee_db: f64,
	/// Transition cost per semitone of pitch move between consecutive frames (nats).
	pub move_cost: f64,
	/// Largest cost of one move, so octave leaps between notes stay possible.
	pub max_move_cost: f64,
	/// Cost of switching between voiced and unvoiced (pYIN's 0.01 switch probability).
	pub switch_cost: f64,
	/// Extra cost of the unvoiced state in frames that have candidates (nats): a frame with a
	/// clear dip is more often voice than not, even when the dip is shallow (noise, reverb).
	pub unvoiced_bias: f64,
}

impl Default for PyinOptions {
	fn default() -> Self {
		Self {
			hop_seconds: 0.005,
			min_hz: 50.0,
			max_hz: 1_600.0,
			beta: (2.0, 8.0),
			no_dip_probability: 0.01,
			max_candidates: 8,
			deeper_ratio: 0.25,
			deeper_above: 0.1,
			silence_db: 50.0,
			silence_knee_db: 10.0,
			move_cost: 1.0,
			max_move_cost: 8.0,
			switch_cost: 4.6,
			unvoiced_bias: 1.0,
		}
	}
}

#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Candidate {
	/// Fractional MIDI pitch.
	pub pitch: f64,
	/// Probability that the frame is voiced at this pitch.
	pub probability: f64,
	/// Difference-function value of its dip (0 = perfectly periodic).
	pub aperiodicity: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CandidateFrame {
	pub time: f64,
	pub rms: f64,
	pub candidates: Vec<Candidate>,
}

impl CandidateFrame {
	pub fn voiced_mass(&self) -> f64 {
		self.candidates
			.iter()
			.map(|c| c.probability)
			.sum::<f64>()
			.min(1.0)
	}
}

/// Probabilities of the 100 thresholds 0.01..=1.00 under Beta(a, b).
fn threshold_prior(a: f64, b: f64) -> [f64; 100] {
	let mut p = [0.0; 100];
	for (i, slot) in p.iter_mut().enumerate() {
		let x = (i as f64 + 0.5) / 100.0;
		*slot = x.powf(a - 1.0) * (1.0 - x).powf(b - 1.0);
	}
	let sum: f64 = p.iter().sum();
	for v in &mut p {
		*v /= sum;
	}
	p
}

/// Stage 1: period candidates per frame. `progress` gets 0..1 and returns false to cancel
/// (then `None`).
pub fn candidates(
	samples: &[f32],
	sample_rate: u32,
	opts: &PyinOptions,
	progress: &mut dyn FnMut(f64) -> bool,
) -> Option<Vec<CandidateFrame>> {
	let Some(probe) = Cmnd::new(sample_rate, opts.hop_seconds, opts.min_hz, opts.max_hz) else {
		return Some(Vec::new());
	};
	if samples.is_empty() {
		return Some(Vec::new());
	}
	let prior = threshold_prior(opts.beta.0, opts.beta.1);
	let count = probe.frame_count(samples.len());
	let mut frames = map_frames(count, progress, |range, tick| {
		let mut cmnd = Cmnd::new(sample_rate, opts.hop_seconds, opts.min_hz, opts.max_hz)?;
		let start = range.start;
		let mut out = Vec::with_capacity(range.len());
		let mut dips: Vec<(usize, f64)> = Vec::new();
		let mut mass: Vec<f64> = Vec::new();
		for i in range {
			if (i - start).is_multiple_of(256) && !tick(i - start) {
				return None;
			}
			let rms = cmnd.compute(samples, i);
			out.push(CandidateFrame {
				time: cmnd.time(i),
				rms,
				candidates: frame_candidates(&cmnd, &prior, opts, &mut dips, &mut mass),
			});
		}
		Some(out)
	})?;
	// Quiet frames lose voicing mass smoothly: below the floor they cannot be voiced.
	let loudest = frames.iter().map(|f| f.rms).fold(0.0, f64::max);
	for f in &mut frames {
		let below = if loudest > 0.0 && f.rms > 0.0 {
			20.0 * (loudest / f.rms).log10()
		} else {
			f64::INFINITY
		};
		let keep = ((opts.silence_db - below) / opts.silence_knee_db).clamp(0.0, 1.0);
		if keep <= 0.0 {
			f.candidates.clear();
		} else {
			for c in &mut f.candidates {
				c.probability *= keep;
			}
		}
	}
	Some(frames)
}

/// Candidates of the frame whose difference function `cmnd` holds.
fn frame_candidates(
	cmnd: &Cmnd,
	prior: &[f64; 100],
	opts: &PyinOptions,
	dips: &mut Vec<(usize, f64)>,
	mass: &mut Vec<f64>,
) -> Vec<Candidate> {
	let v = &cmnd.values;
	dips.clear();
	for tau in cmnd.min_lag..cmnd.max_lag {
		let left = if tau > cmnd.min_lag {
			v[tau - 1]
		} else {
			f64::INFINITY
		};
		if v[tau] < left && v[tau] <= v[tau + 1] {
			dips.push((tau, v[tau]));
		}
	}
	if dips.is_empty() {
		return Vec::new();
	}
	mass.clear();
	mass.resize(dips.len(), 0.0);
	let global = dips
		.iter()
		.enumerate()
		.min_by(|a, b| a.1.1.total_cmp(&b.1.1))
		.map_or(0, |(k, _)| k);
	for (t, p) in prior.iter().enumerate() {
		let threshold = (t + 1) as f64 / 100.0;
		match dips.iter().position(|d| d.1 < threshold) {
			Some(mut k) => {
				while let Some(j) = (dips[k].1 > opts.deeper_above)
					.then(|| {
						(k + 1..dips.len()).find(|&j| dips[j].1 < opts.deeper_ratio * dips[k].1)
					})
					.flatten()
				{
					k = j;
				}
				mass[k] += p;
			}
			None => mass[global] += p * opts.no_dip_probability,
		}
	}
	let mut candidates: Vec<Candidate> = dips
		.iter()
		.zip(mass.iter())
		.filter(|(_, m)| **m > 1e-4)
		.map(|(&(tau, value), &m)| Candidate {
			pitch: cmnd.pitch_of(tau),
			probability: m,
			aperiodicity: value,
		})
		.collect();
	candidates.sort_by(|a, b| b.probability.total_cmp(&a.probability));
	candidates.truncate(opts.max_candidates);
	candidates
}

/// Stage 2: the most likely pitch path. One entry per frame: the chosen candidate's index,
/// or `None` for unvoiced.
pub fn decode(frames: &[CandidateFrame], opts: &PyinOptions) -> Vec<Option<usize>> {
	const FLOOR: f64 = 1e-9;
	if frames.is_empty() {
		return Vec::new();
	}
	// State 0 = unvoiced, k + 1 = candidate k.
	let emission = |f: &CandidateFrame, state: usize| -> f64 {
		if state == 0 {
			-(1.0 - f.voiced_mass()).max(FLOOR).ln()
				+ if f.candidates.is_empty() {
					0.0
				} else {
					opts.unvoiced_bias
				}
		} else {
			-f.candidates[state - 1].probability.max(FLOOR).ln()
		}
	};
	let mut cost: Vec<f64> = (0..=frames[0].candidates.len())
		.map(|s| emission(&frames[0], s))
		.collect();
	let mut back: Vec<Vec<usize>> = Vec::with_capacity(frames.len());
	back.push(vec![0; cost.len()]);
	for t in 1..frames.len() {
		let (prev, cur) = (&frames[t - 1], &frames[t]);
		let mut next = Vec::with_capacity(cur.candidates.len() + 1);
		let mut from = Vec::with_capacity(cur.candidates.len() + 1);
		for s in 0..=cur.candidates.len() {
			let mut best = (f64::INFINITY, 0);
			for (r, &c) in cost.iter().enumerate() {
				let step = match (r, s) {
					(0, 0) => 0.0,
					(0, _) | (_, 0) => opts.switch_cost,
					(r, s) => {
						let semitones =
							(cur.candidates[s - 1].pitch - prev.candidates[r - 1].pitch).abs();
						(semitones * opts.move_cost).min(opts.max_move_cost)
					}
				};
				if c + step < best.0 {
					best = (c + step, r);
				}
			}
			next.push(best.0 + emission(cur, s));
			from.push(best.1);
		}
		cost = next;
		back.push(from);
	}
	let mut state = cost
		.iter()
		.enumerate()
		.min_by(|a, b| a.1.total_cmp(b.1))
		.map_or(0, |(s, _)| s);
	let mut path = vec![None; frames.len()];
	for t in (0..frames.len()).rev() {
		path[t] = state.checked_sub(1);
		state = back[t][state];
	}
	path
}

/// pYIN as a [`PitchTracker`].
#[derive(Debug, Clone, Default)]
pub struct Pyin(pub PyinOptions);

impl Pyin {
	/// Candidates and the decoded path in one go.
	pub fn analyze(
		&self,
		samples: &[f32],
		sample_rate: u32,
	) -> (Vec<CandidateFrame>, Vec<Option<usize>>) {
		let frames = candidates(samples, sample_rate, &self.0, &mut |_| true).unwrap_or_default();
		let path = decode(&frames, &self.0);
		(frames, path)
	}
}

impl PitchTracker for Pyin {
	fn name(&self) -> &'static str {
		"pyin"
	}

	fn track(&self, samples: &[f32], sample_rate: u32) -> Vec<PitchFrame> {
		let (frames, path) = self.analyze(samples, sample_rate);
		frames
			.iter()
			.zip(path)
			.map(|(f, choice)| PitchFrame {
				time: f.time,
				pitch: choice.map(|k| f.candidates[k].pitch),
				voicing: f.voiced_mass(),
			})
			.collect()
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::midi_to_hz;

	fn tone(midi: f64, sr: u32, seconds: f64) -> Vec<f32> {
		let hz = midi_to_hz(midi);
		(0..(f64::from(sr) * seconds) as usize)
			.map(|i| {
				let t = i as f64 / f64::from(sr);
				(0.3 * (2.0 * std::f64::consts::PI * hz * t).sin()
					+ 0.2 * (4.0 * std::f64::consts::PI * hz * t).sin()
					+ 0.1 * (6.0 * std::f64::consts::PI * hz * t).sin()) as f32
			})
			.collect()
	}

	#[test]
	fn prior_is_a_distribution_peaking_near_a_tenth() {
		let p = threshold_prior(2.0, 18.0);
		assert!((p.iter().sum::<f64>() - 1.0).abs() < 1e-12);
		let peak = p
			.iter()
			.enumerate()
			.max_by(|a, b| a.1.total_cmp(b.1))
			.unwrap()
			.0;
		assert!((4..=7).contains(&peak), "{peak}");
	}

	#[test]
	fn a_periodic_frame_is_almost_surely_voiced_at_its_pitch() {
		let frames = candidates(
			&tone(57.0, 22_050, 0.4),
			22_050,
			&PyinOptions::default(),
			&mut |_| true,
		)
		.unwrap();
		let mid = &frames[frames.len() / 2];
		assert!(mid.voiced_mass() > 0.95, "{mid:?}");
		assert!((mid.candidates[0].pitch - 57.0).abs() < 0.01);
	}

	#[test]
	fn decodes_a_leap_and_silence() {
		let sr = 22_050;
		let mut audio = tone(50.0, sr, 0.3);
		audio.extend(tone(62.0, sr, 0.3));
		audio.extend(std::iter::repeat_n(0.0, sr as usize / 5));
		let frames = Pyin::default().track(&audio, sr);
		let at = |t: f64| {
			frames
				.iter()
				.min_by(|a, b| (a.time - t).abs().total_cmp(&(b.time - t).abs()))
				.unwrap()
		};
		assert!((at(0.15).pitch.unwrap() - 50.0).abs() < 0.02);
		assert!((at(0.45).pitch.unwrap() - 62.0).abs() < 0.02);
		assert!(at(0.75).pitch.is_none());
	}

	#[test]
	fn cancel_stops_the_analysis() {
		let audio = tone(60.0, 22_050, 3.0);
		assert!(candidates(&audio, 22_050, &PyinOptions::default(), &mut |p| p < 0.3).is_none());
	}
}
