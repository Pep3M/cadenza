//! NE-8: the polyphonic detector on the synthetic (monophonic) corpus: it must read as one
//! line, so `auto` keeps sung and played lines on the mono path.

use cadenza_notes::eval::synth::corpus;
use cadenza_notes::poly::{PolyOptions, polyphony, transcribe};

#[test]
fn the_monophonic_corpus_reads_as_one_line() {
	let mut worst = (0.0, String::new());
	for case in corpus(22_050) {
		let (_, notes) = transcribe(&case.samples, 22_050, &PolyOptions::default(), &mut |_| {
			true
		})
		.expect("not cancelled");
		let share = polyphony(&notes);
		eprintln!(
			"{:<28} {:>3} notes, polyphony {share:.3}",
			case.name,
			notes.len()
		);
		if share > worst.0 {
			worst = (share, case.name.clone());
		}
	}
	assert!(worst.0 < 0.2, "{worst:?}");
}
