//! NE-2 floors for the segmenter on the synthetic corpus. Raise them when a change improves
//! the numbers; a change that drops below them is a regression.

use cadenza_notes::baseline::Baseline;
use cadenza_notes::eval::{self, synth};
use cadenza_notes::segment::Segmenter;

#[test]
fn segmenter_keeps_its_floors_at_the_reference_rate() {
	let cases = synth::corpus(22_050);
	let t = eval::evaluate(&Segmenter::default(), &cases).total;
	assert!(t.conpoff.f >= 0.99, "{t:?}");
	// The NE-2 goal: no region holds two notes, so moving it to the scale tunes it.
	assert_eq!(t.mixed_rate, 0.0, "{t:?}");
	assert!(t.split_rate <= 0.005 && t.merge_rate <= 0.005, "{t:?}");
	assert_eq!(t.octave_rate, 0.0, "{t:?}");
	assert!(t.center.median_cents <= 1.0, "{t:?}");
	assert!(t.center.p95_cents <= 5.0, "{t:?}");

	let base = eval::evaluate(&Baseline::default(), &cases).total;
	assert!(t.conpoff.f > base.conpoff.f + 0.2, "{t:?} vs {base:?}");
}

#[test]
fn segmenter_meets_the_plan_targets_at_common_rates() {
	for rate in [16_000, 44_100, 48_000] {
		let t = eval::evaluate(&Segmenter::default(), &synth::corpus(rate)).total;
		assert!(t.conpoff.f >= 0.95, "{rate}: {t:?}");
		assert!(t.mixed_rate < 0.01, "{rate}: {t:?}");
		assert!(t.center.median_cents < 3.0, "{rate}: {t:?}");
		assert!(t.center.p95_cents < 8.0, "{rate}: {t:?}");
	}
}
