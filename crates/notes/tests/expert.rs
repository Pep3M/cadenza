//! NE-7a floors on a real take an expert segmented by hand (labels in
//! `tests/data/expert_take.json`, see `docs/design/note-editing-expert-session.md`). The
//! recording is not in the repository: point `CADENZA_NOTES_EXPERT_TAKE` at it to run.
//! Boundaries there are the segmenter's own after the expert's splits, so the case scores
//! which notes exist more than where an ear would cut them.

use cadenza_audio_io::decode_file;
use cadenza_notes::eval::{self, Case, labels};
use cadenza_notes::segment::Segmenter;

#[test]
fn the_segmenter_finds_most_notes_the_expert_cut() {
	let Some(path) = std::env::var_os("CADENZA_NOTES_EXPERT_TAKE") else {
		eprintln!("CADENZA_NOTES_EXPERT_TAKE not set: skipped");
		return;
	};
	let truth = labels::parse(include_str!("data/expert_take.json")).expect("labels");
	let mono = decode_file(&path).expect("decoding the take").to_mono();
	let case = Case {
		name: "expert_take".into(),
		tags: vec!["real".into()],
		sample_rate: mono.sample_rate(),
		samples: mono.channel(0).to_vec(),
		frames: labels::frames(&truth, mono.duration_seconds()),
		truth,
	};
	let t = eval::evaluate(&Segmenter::default(), &[case]).total;
	eprintln!("{t:?}");
	// Before NE-7a: COnPOff 70.1 %, mixed regions 20 %, 3 metronome blips as notes.
	assert!(t.conpoff.f >= 0.83, "{t:?}");
	assert!(t.mixed_rate <= 0.12, "{t:?}");
	assert_eq!(t.octave_rate, 0.0, "{t:?}");
}
