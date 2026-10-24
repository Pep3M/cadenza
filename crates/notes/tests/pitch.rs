//! NE-1 floors for the pitch tracker on the synthetic corpus. Raise them when a change
//! improves the numbers; a change that drops below them is a regression.

use cadenza_notes::eval::{self, synth};
use cadenza_notes::pyin::Pyin;
use cadenza_notes::yin::Yin;

#[test]
fn pyin_keeps_its_floors_and_beats_plain_yin() {
	let cases = synth::corpus(22_050);
	let pyin = eval::evaluate_pitch(&Pyin::default(), &cases);
	let yin = eval::evaluate_pitch(&Yin::default(), &cases);
	let t = pyin.total;
	assert!(t.raw_pitch >= 0.98, "{t:?}");
	assert!(t.voicing_recall >= 0.98, "{t:?}");
	assert!(t.voicing_false_alarm <= 0.08, "{t:?}");
	assert_eq!(t.octave_rate, 0.0, "{t:?}");
	assert!(t.gross_rate <= 0.002, "{t:?}");
	assert!(t.median_cents <= 1.0, "{t:?}");
	let noisy = pyin
		.cases
		.iter()
		.find(|c| c.name == "noisy_snr10")
		.unwrap()
		.score;
	assert!(noisy.raw_pitch >= 0.9, "{noisy:?}");

	assert!(t.raw_pitch > yin.total.raw_pitch);
	assert!(t.voicing_false_alarm < yin.total.voicing_false_alarm);
	assert!(yin.total.octave_rate > 0.0);
}
