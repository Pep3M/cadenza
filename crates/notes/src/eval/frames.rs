//! Frame-level scores of a pitch track (mir_eval melody rules, voiced frames only):
//! raw pitch accuracy (within the frame's tolerance, 50 cents for exact truth), raw chroma
//! accuracy (octave errors forgiven), voicing recall / false alarm, octave and gross error
//! rates, median absolute error.

use serde::Serialize;

use crate::PitchFrame;

/// Truth frames are on this grid (seconds).
pub const TRUTH_HOP: f64 = 0.005;

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub enum FrameState {
	/// Sung at `pitch` (fractional MIDI); an estimate within `tolerance_cents` is right.
	Voiced {
		pitch: f64,
		tolerance_cents: f64,
	},
	Unvoiced,
	/// Not scored: attacks, releases, reverb tails, labeled notes outside their cores.
	Ignore,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct TruthFrame {
	pub time: f64,
	pub state: FrameState,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct FrameCounts {
	pub voiced: usize,
	pub unvoiced: usize,
	/// Truth voiced and estimate voiced.
	pub hits: usize,
	/// Truth unvoiced, estimate voiced.
	pub false_alarms: usize,
	pub pitch_ok: usize,
	pub chroma_ok: usize,
	pub octave: usize,
	pub gross: usize,
	/// Absolute error in cents of every hit.
	pub errors: Vec<f64>,
}

impl FrameCounts {
	pub fn pooled(all: &[FrameCounts]) -> FrameCounts {
		let mut t = FrameCounts::default();
		for c in all {
			t.voiced += c.voiced;
			t.unvoiced += c.unvoiced;
			t.hits += c.hits;
			t.false_alarms += c.false_alarms;
			t.pitch_ok += c.pitch_ok;
			t.chroma_ok += c.chroma_ok;
			t.octave += c.octave;
			t.gross += c.gross;
			t.errors.extend_from_slice(&c.errors);
		}
		t
	}
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct FrameScore {
	pub voiced_frames: usize,
	/// Correct pitch / truth voiced frames.
	pub raw_pitch: f64,
	/// Correct pitch class / truth voiced frames.
	pub raw_chroma: f64,
	pub voicing_recall: f64,
	pub voicing_false_alarm: f64,
	/// Off by whole octaves / hits.
	pub octave_rate: f64,
	/// Wrong and not an octave / hits.
	pub gross_rate: f64,
	pub median_cents: f64,
	pub p95_cents: f64,
}

impl FrameScore {
	pub fn from_counts(c: &FrameCounts) -> Self {
		let mut e = c.errors.clone();
		e.sort_by(f64::total_cmp);
		let pick = |q: f64| {
			if e.is_empty() {
				0.0
			} else {
				e[((e.len() - 1) as f64 * q).round() as usize]
			}
		};
		let ratio = |n: usize, d: usize| if d == 0 { 0.0 } else { n as f64 / d as f64 };
		Self {
			voiced_frames: c.voiced,
			raw_pitch: ratio(c.pitch_ok, c.voiced),
			raw_chroma: ratio(c.chroma_ok, c.voiced),
			voicing_recall: ratio(c.hits, c.voiced),
			voicing_false_alarm: ratio(c.false_alarms, c.unvoiced),
			octave_rate: ratio(c.octave, c.hits),
			gross_rate: ratio(c.gross, c.hits),
			median_cents: pick(0.5),
			p95_cents: pick(0.95),
		}
	}
}

/// Scores `estimate` against `truth`; each truth frame takes the nearest estimate frame.
pub fn count(truth: &[TruthFrame], estimate: &[PitchFrame]) -> FrameCounts {
	let mut c = FrameCounts::default();
	if estimate.is_empty() {
		for t in truth {
			match t.state {
				FrameState::Voiced { .. } => c.voiced += 1,
				FrameState::Unvoiced => c.unvoiced += 1,
				FrameState::Ignore => {}
			}
		}
		return c;
	}
	let mut j = 0;
	for t in truth {
		while j + 1 < estimate.len()
			&& (estimate[j + 1].time - t.time).abs() <= (estimate[j].time - t.time).abs()
		{
			j += 1;
		}
		let est = estimate[j].pitch;
		match t.state {
			FrameState::Ignore => {}
			FrameState::Unvoiced => {
				c.unvoiced += 1;
				if est.is_some() {
					c.false_alarms += 1;
				}
			}
			FrameState::Voiced {
				pitch,
				tolerance_cents,
			} => {
				c.voiced += 1;
				let Some(p) = est else { continue };
				c.hits += 1;
				let error = (p - pitch).abs() * 100.0;
				c.errors.push(error);
				let octaves = (error / 1200.0).round();
				let folded = (error - octaves * 1200.0).abs();
				if error <= tolerance_cents {
					c.pitch_ok += 1;
				}
				if folded <= tolerance_cents {
					c.chroma_ok += 1;
					if octaves >= 1.0 {
						c.octave += 1;
					}
				} else {
					c.gross += 1;
				}
			}
		}
	}
	c
}

#[cfg(test)]
mod tests {
	use super::*;

	fn voiced(time: f64, pitch: f64) -> TruthFrame {
		TruthFrame {
			time,
			state: FrameState::Voiced {
				pitch,
				tolerance_cents: 50.0,
			},
		}
	}

	fn est(time: f64, pitch: Option<f64>) -> PitchFrame {
		PitchFrame {
			time,
			pitch,
			voicing: 1.0,
		}
	}

	#[test]
	fn counts_hits_octaves_gross_and_false_alarms() {
		let truth = [
			voiced(0.0, 60.0),
			voiced(0.005, 60.0),
			voiced(0.01, 60.0),
			voiced(0.015, 60.0),
			TruthFrame {
				time: 0.02,
				state: FrameState::Unvoiced,
			},
			TruthFrame {
				time: 0.025,
				state: FrameState::Ignore,
			},
		];
		// Estimate on a slightly different grid.
		let estimate = [
			est(0.0004, Some(60.2)),
			est(0.0051, Some(72.1)),
			est(0.0099, Some(63.0)),
			est(0.0151, None),
			est(0.0202, Some(50.0)),
			est(0.0249, Some(10.0)),
		];
		let s = FrameScore::from_counts(&count(&truth, &estimate));
		assert_eq!(s.raw_pitch, 0.25);
		assert_eq!(s.raw_chroma, 0.5);
		assert_eq!(s.voicing_recall, 0.75);
		assert_eq!(s.voicing_false_alarm, 1.0);
		assert!((s.octave_rate - 1.0 / 3.0).abs() < 1e-12);
		assert!((s.gross_rate - 1.0 / 3.0).abs() < 1e-12);
	}
}
