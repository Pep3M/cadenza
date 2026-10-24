//! Evaluation harness (NE-0): ground truth, corpora and scores.
//!
//! - [`synth`] renders monophonic phrases whose every note boundary, stable core and pitch
//!   center is known exactly (glides, scoops, vibrato, drift, repeated notes, breaths,
//!   noise, reverb...).
//! - [`labels`] reads hand-labeled recordings (`<name>.json` next to the audio).
//! - [`metrics`] scores detected notes against the truth.
//! - [`evaluate`] runs a detector over cases and pools the scores into a [`Report`].

pub mod labels;
pub mod metrics;
pub mod synth;

use serde::{Deserialize, Serialize};

use crate::NoteDetector;
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
