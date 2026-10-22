//! Live loudness for device editors and meters (design 16a "LOUDNESS"): BS.1770 short-term
//! loudness every 100 ms, gated integrated loudness and 4x-oversampled true peak since the
//! last reset, plus a 30 s trace of the short-term readings. It runs on the UI thread
//! from drained probe frames (the analysis code allocates, so never on the audio thread).

use std::collections::VecDeque;

use cadenza_analysis::levels::TruePeak;
use cadenza_analysis::loudness::LoudnessMeter;
use cadenza_engine::ProbeFrame;

use crate::state::meters::FLOOR_DB;

/// Seconds between readings.
pub const UPDATE_SECONDS: f32 = 0.1;
/// Seconds of short-term readings kept in [`LiveLoudness::trace`].
pub const TRACE_SECONDS: f32 = 30.0;
const TRACE_LEN: usize = (TRACE_SECONDS / UPDATE_SECONDS) as usize;

/// Which side of a probe frame is measured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
	Input,
	Output,
}

#[derive(Debug, Clone)]
pub struct LiveLoudness {
	rate: u32,
	meter: LoudnessMeter,
	peak: TruePeak,
	update_len: usize,
	/// Meter step the integrated loudness starts at (the last reset).
	since: usize,
	/// Samples pushed since the last reading.
	pending: usize,
	short_term: f32,
	integrated: f32,
	trace: VecDeque<f32>,
	left: Vec<f32>,
	right: Vec<f32>,
}

impl LiveLoudness {
	pub fn new(sample_rate: u32) -> Self {
		let rate = sample_rate.max(1);
		Self {
			rate,
			meter: LoudnessMeter::new(rate),
			peak: TruePeak::new(),
			since: 0,
			update_len: ((rate as f32 * UPDATE_SECONDS).round() as usize).max(1),
			pending: 0,
			short_term: FLOOR_DB,
			integrated: FLOOR_DB,
			trace: VecDeque::with_capacity(TRACE_LEN),
			left: Vec::new(),
			right: Vec::new(),
		}
	}

	pub fn sample_rate(&self) -> u32 {
		self.rate
	}

	/// Starts over at `sample_rate` when it changed (another device, reopened output).
	pub fn set_sample_rate(&mut self, sample_rate: u32) {
		if sample_rate.max(1) != self.rate {
			*self = Self::new(sample_rate);
		}
	}

	/// Stereo samples; a reading is taken at every 100 ms boundary inside them.
	pub fn push(&mut self, left: &[f32], right: &[f32]) {
		let n = left.len().min(right.len());
		let mut at = 0;
		while at < n {
			let take = (self.update_len - self.pending).min(n - at);
			let (l, r) = (&left[at..at + take], &right[at..at + take]);
			self.meter.push(l, r);
			self.peak.push(l, r);
			self.pending += take;
			at += take;
			if self.pending >= self.update_len {
				self.pending = 0;
				self.read();
			}
		}
	}

	/// One side of this frame's drained probe frames.
	pub fn push_frames(&mut self, frames: &[ProbeFrame], side: Side) {
		if frames.is_empty() {
			return;
		}
		let mut left = std::mem::take(&mut self.left);
		let mut right = std::mem::take(&mut self.right);
		left.clear();
		right.clear();
		for f in frames {
			let s = match side {
				Side::Input => f.input,
				Side::Output => f.output,
			};
			left.push(s[0]);
			right.push(s[1]);
		}
		self.push(&left, &right);
		self.left = left;
		self.right = right;
	}

	fn read(&mut self) {
		self.short_term = self.meter.short_term();
		self.integrated = self.meter.integrated_since(self.since);
		if self.trace.len() == TRACE_LEN {
			self.trace.pop_front();
		}
		self.trace.push_back(self.short_term);
	}

	/// Clears the integrated loudness and the true peak; short-term and the trace go on.
	pub fn reset_integrated(&mut self) {
		self.since = self.meter.steps();
		self.peak = TruePeak::new();
		self.integrated = FLOOR_DB;
	}

	/// Short-term loudness (3 s) at the last reading, LUFS; `FLOOR_DB` = nothing yet.
	pub fn short_term(&self) -> f32 {
		self.short_term
	}

	/// Gated integrated loudness since the last reset, LUFS.
	pub fn integrated(&self) -> f32 {
		self.integrated
	}

	/// The highest true peak since the last reset, dBTP.
	pub fn true_peak_db(&self) -> f32 {
		let p = self.peak.peak();
		if p <= 1e-7 {
			FLOOR_DB
		} else {
			(20.0 * p.log10()).max(FLOOR_DB)
		}
	}

	/// Short-term readings of the last 30 s, oldest first, one per 100 ms.
	pub fn trace(&self) -> impl ExactSizeIterator<Item = f32> + '_ {
		self.trace.iter().copied()
	}

	pub fn trace_capacity(&self) -> usize {
		TRACE_LEN
	}

	/// No integrated loudness since the last reset.
	pub fn is_empty(&self) -> bool {
		self.integrated <= FLOOR_DB
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	const RATE: u32 = 48_000;

	fn sine(amp: f32, seconds: f32) -> Vec<f32> {
		let n = (seconds * RATE as f32) as usize;
		(0..n)
			.map(|i| amp * (std::f32::consts::TAU * 1000.0 * i as f32 / RATE as f32).sin())
			.collect()
	}

	#[test]
	fn a_minus_20_dbfs_sine_reads_minus_20_lufs() {
		let mut m = LiveLoudness::new(RATE);
		assert!(m.is_empty());
		let s = sine(0.1, 5.0);
		// Pushed in UI-frame sized chunks.
		for (l, r) in s.chunks(800).zip(s.chunks(800)) {
			m.push(l, r);
		}
		// Both channels at 0.1 peak: 1 kHz has +0.7 dB of K-weighting, which the -0.691
		// offset of BS.1770 cancels.
		assert!((m.integrated() + 20.0).abs() < 0.3, "{}", m.integrated());
		assert!((m.short_term() + 20.0).abs() < 0.3, "{}", m.short_term());
		assert!(
			(m.true_peak_db() + 20.0).abs() < 0.1,
			"{}",
			m.true_peak_db()
		);
		assert_eq!(m.trace().len(), 50, "one reading per 100 ms");
	}

	#[test]
	fn short_term_updates_every_100_ms() {
		let mut m = LiveLoudness::new(RATE);
		let s = sine(0.1, 0.099);
		m.push(&s, &s);
		assert_eq!(m.short_term(), FLOOR_DB, "no reading before 100 ms");
		let s = sine(0.1, 0.002);
		m.push(&s, &s);
		assert!(m.short_term() > -21.0);
		assert_eq!(m.trace().len(), 1);
		// Quieter for 4 s: short-term follows, integrated keeps the louder start.
		let q = sine(0.01, 4.0);
		m.push(&q, &q);
		assert!((m.short_term() + 40.0).abs() < 0.3, "{}", m.short_term());
		assert!(m.integrated() < -30.0 && m.integrated() > -40.0);
	}

	#[test]
	fn reset_clears_integrated_and_true_peak() {
		let mut m = LiveLoudness::new(RATE);
		let s = sine(0.5, 1.0);
		m.push(&s, &s);
		assert!(m.integrated() > -10.0);
		m.reset_integrated();
		assert_eq!(m.integrated(), FLOOR_DB);
		assert_eq!(m.true_peak_db(), FLOOR_DB);
		assert!(m.is_empty());
		assert!(m.short_term() > -10.0, "short-term keeps its window");
		assert_eq!(m.trace().len(), 10, "the trace keeps its history");
		let q = sine(0.1, 1.0);
		m.push(&q, &q);
		assert!((m.integrated() + 20.0).abs() < 0.3);
	}

	#[test]
	fn probe_frames_feed_the_chosen_side() {
		let mut m = LiveLoudness::new(RATE);
		let s = sine(0.1, 1.0);
		let frames: Vec<ProbeFrame> = s
			.iter()
			.map(|&x| ProbeFrame {
				input: [x * 10.0, x * 10.0],
				output: [x, x],
			})
			.collect();
		m.push_frames(&frames, Side::Output);
		assert!((m.integrated() + 20.0).abs() < 0.3);
		m.set_sample_rate(44_100);
		assert!(m.is_empty(), "a new rate starts over");
	}
}
