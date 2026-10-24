//! Evaluation harness (NE-0): ground truth, corpora and scores.
//!
//! - [`synth`] renders monophonic phrases whose every note boundary, stable core and pitch
//!   center is known exactly (glides, scoops, vibrato, drift, repeated notes, breaths,
//!   noise, reverb...).
//! - [`labels`] reads hand-labeled recordings (`<name>.json` next to the audio).
//! - [`metrics`] scores detected notes against the truth.
//! - [`evaluate`] runs a detector over cases and pools the scores into a [`Report`].

pub mod frames;
pub mod labels;
pub mod metrics;
pub mod synth;

use serde::{Deserialize, Serialize};

use crate::{NoteDetector, PitchTracker};
pub use frames::{FrameCounts, FrameScore, FrameState, TruthFrame};
pub use metrics::{Counts, Score, score};

/// One real note of the ground truth.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TruthNote {
	/// Seconds. Between legato notes the boundary sits where the glide crosses halfway.
	pub onset: f64,
	pub offset: f64,
	/// Intended note (MIDI number); the singer may be off it.
	pub nominal: f64,
	/// The pitch actually sung over the stable core (fractional MIDI): what a detector's
	/// center must find and what tuning moves.
	pub center: f64,
	/// The stable part, without glides, scoops and falls (seconds).
	pub core_start: f64,
	pub core_end: f64,
}

impl TruthNote {
	pub fn core_length(&self) -> f64 {
		(self.core_end - self.core_start).max(0.0)
	}
}

/// Audio plus its truth.
#[derive(Debug, Clone)]
pub struct Case {
	pub name: String,
	/// What the case exercises ("glide", "vibrato", "noise"...).
	pub tags: Vec<String>,
	pub sample_rate: u32,
	pub samples: Vec<f32>,
	pub truth: Vec<TruthNote>,
	/// Frame truth on the [`frames::TRUTH_HOP`] grid.
	pub frames: Vec<TruthFrame>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CaseReport {
	pub name: String,
	pub tags: Vec<String>,
	pub score: Score,
}

#[derive(Debug, Clone, Serialize)]
pub struct Report {
	pub detector: String,
	pub cases: Vec<CaseReport>,
	/// Every case pooled (counts summed, center errors merged).
	pub total: Score,
}

/// Runs `detector` on every case.
pub fn evaluate(detector: &dyn NoteDetector, cases: &[Case]) -> Report {
	let mut counts = Vec::with_capacity(cases.len());
	let reports = cases
		.iter()
		.map(|case| {
			let detected = detector.detect(&case.samples, case.sample_rate);
			let c = metrics::count(&case.truth, &detected);
			let score = Score::from_counts(&c);
			counts.push(c);
			CaseReport {
				name: case.name.clone(),
				tags: case.tags.clone(),
				score,
			}
		})
		.collect();
	Report {
		detector: detector.name().to_string(),
		cases: reports,
		total: Score::from_counts(&Counts::pooled(&counts)),
	}
}

#[derive(Debug, Clone, Serialize)]
pub struct PitchCaseReport {
	pub name: String,
	pub tags: Vec<String>,
	pub score: FrameScore,
}

#[derive(Debug, Clone, Serialize)]
pub struct PitchReport {
	pub tracker: String,
	pub cases: Vec<PitchCaseReport>,
	pub total: FrameScore,
	/// Seconds of audio analyzed per second of computation.
	pub speed: f64,
}

/// Runs a pitch tracker on every case and scores it frame by frame.
pub fn evaluate_pitch(tracker: &dyn PitchTracker, cases: &[Case]) -> PitchReport {
	let mut counts = Vec::with_capacity(cases.len());
	let (mut audio, mut spent) = (0.0, 0.0);
	let reports = cases
		.iter()
		.map(|case| {
			let started = std::time::Instant::now();
			let track = tracker.track(&case.samples, case.sample_rate);
			spent += started.elapsed().as_secs_f64();
			audio += case.samples.len() as f64 / f64::from(case.sample_rate.max(1));
			let c = frames::count(&case.frames, &track);
			let score = FrameScore::from_counts(&c);
			counts.push(c);
			PitchCaseReport {
				name: case.name.clone(),
				tags: case.tags.clone(),
				score,
			}
		})
		.collect();
	PitchReport {
		tracker: tracker.name().to_string(),
		cases: reports,
		total: FrameScore::from_counts(&FrameCounts::pooled(&counts)),
		speed: if spent > 0.0 { audio / spent } else { 0.0 },
	}
}
