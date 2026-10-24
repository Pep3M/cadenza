//! The NE-0 harness itself: the truth scores perfectly, and the naive baseline shows the
//! failure later detectors must fix (regions spanning glides between two notes).

use cadenza_notes::baseline::Baseline;
use cadenza_notes::eval::{self, Counts, Score, synth};
use cadenza_notes::{Note, NoteDetector};

#[test]
fn ground_truth_scores_perfectly_on_the_whole_corpus() {
	let cases = synth::corpus(16_000);
	let counts: Vec<Counts> = cases
		.iter()
		.map(|c| {
			let notes: Vec<Note> = c
				.truth
				.iter()
				.map(|t| Note {
					onset: t.onset,
					offset: t.offset,
					center: t.center,
				})
				.collect();
			eval::metrics::count(&c.truth, &notes)
		})
		.collect();
	let s = Score::from_counts(&Counts::pooled(&counts));
	assert_eq!(s.conpoff.f, 1.0);
	assert_eq!(s.center.max_cents, 0.0);
	assert_eq!(
		(s.mixed_rate, s.split_rate, s.merge_rate, s.octave_rate),
		(0.0, 0.0, 0.0, 0.0)
	);
	assert!((s.voicing_precision - 1.0).abs() < 1e-9 && (s.voicing_recall - 1.0).abs() < 1e-9);
}

#[test]
fn baseline_is_exact_on_steady_notes_and_mixes_notes_across_glides() {
	let cases = synth::corpus(16_000);
	let report = eval::evaluate(&Baseline::default(), &cases);
	assert_eq!(report.detector, Baseline::default().name());
	let case = |name: &str| &report.cases.iter().find(|c| c.name == name).unwrap().score;
	for name in [
		"steady_low",
		"steady_mid",
		"steady_high",
		"drift",
		"repeated",
		"octave_leaps",
		"breaths",
	] {
		let s = case(name);
		assert!(s.conpoff.f > 0.95, "{name}: {s:?}");
		assert!(s.center.p95_cents < 5.0, "{name}: {s:?}");
	}
	let glides = case("legato_glide_long");
	assert!(glides.mixed_rate >= 0.3, "{glides:?}");
	assert!(glides.center.median_cents > 50.0, "{glides:?}");
	// Scoops pull the plain mean below the sung pitch.
	assert!(case("scoops_falls").center.median_cents > 20.0);
}
