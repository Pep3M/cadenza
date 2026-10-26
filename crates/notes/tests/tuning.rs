//! NE-7a: the tones an expert chose for a real take (labels of
//! `docs/design/note-editing-expert-session.md`: sung centers and the expert's targets; the
//! recording itself is not in the repository).

use cadenza_notes::tuning::{self, TargetOptions, TuningNote};
use serde_json::Value;

fn expert() -> (Vec<TuningNote>, Vec<f64>) {
	let labels: Value =
		serde_json::from_str(include_str!("data/expert_take.json")).expect("labels");
	labels["notes"]
		.as_array()
		.expect("notes")
		.iter()
		.map(|n| {
			let f = |k: &str| n[k].as_f64().expect(k);
			(
				TuningNote {
					onset: f("onset"),
					offset: f("offset"),
					core_start: f("core_start"),
					core_end: f("core_end"),
					center: f("pitch"),
					landing: None,
				},
				f("nominal"),
			)
		})
		.unzip()
}

#[test]
fn fitted_scale_and_offset_pick_the_experts_tones() {
	let (notes, chosen) = expert();
	let fits = tuning::fit_scale(&notes, 3);
	let fit = fits[0];
	// The expert tuned to E Phrygian; the singer was about a third of a semitone sharp.
	assert_eq!(fit.scale.root.0, 4, "{fits:?}");
	assert_eq!(
		fit.scale.kind,
		cadenza_theory::ScaleKind::Phrygian,
		"{fits:?}"
	);
	assert!((25.0..=45.0).contains(&fit.offset_cents), "{fit:?}");
	let targets = tuning::targets(&notes, &fit, &TargetOptions::default());
	let agree = |t: &dyn Fn(usize) -> f64| (0..notes.len()).filter(|&i| t(i) == chosen[i]).count();
	let ours = agree(&|i| targets[i].pitch);
	let nearest = agree(&|i| notes[i].center.round());
	eprintln!(
		"expert targets matched: {ours}/{} (nearest semitone {nearest})",
		notes.len()
	);
	for (i, t) in targets.iter().enumerate() {
		if t.pitch != chosen[i] {
			eprintln!(
				"  {:.3} sung {:.2}: ours {} ({:?}, runner-up {}), expert {}",
				notes[i].onset, notes[i].center, t.pitch, t.reason, t.runner_up, chosen[i]
			);
		}
	}
	assert!(ours >= 49, "{ours}/{}", notes.len());
	assert!(nearest <= 35);
}
