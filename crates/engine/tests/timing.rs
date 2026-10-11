//! Sample-accurate MIDI timing, including across tempo changes and block sizes.

mod common;

use cadenza_core::Tick;
use cadenza_engine::{RenderOptions, render_offline};
use cadenza_model::{Device, Project};
use common::*;

fn kick_project(at: Tick, tempo_change: bool) -> Project {
	let mut t = instrument_track("drums", Device::builtin("drum_machine", "Drums"), &[], 64.0);
	if let cadenza_model::ClipContent::Midi { notes } = &mut t.clips[0].content {
		notes.push(cadenza_model::Note::new(36, at, beats(1.0), 1.0));
	}
	let mut p = project(vec![t]);
	if tempo_change {
		p.tempo.set_point(beats(4.0), 90.0);
		p.tempo.set_point(beats(6.0) + Tick(13), 157.5);
	}
	p
}

#[test]
fn note_onsets_are_sample_accurate_across_tempo_changes() {
	let reference = render_offline(&kick_project(Tick::ZERO, false), &opts(2.0)).unwrap();
	let first = reference
		.left
		.iter()
		.position(|x| *x != 0.0)
		.expect("kick sounds");
	let window = 2_000;
	// Odd tick positions on both sides of (and exactly at) the tempo changes.
	for at in [
		beats(1.0) + Tick(7),
		beats(5.0) + Tick(331),
		beats(6.0) + Tick(13),
		beats(9.5) + Tick(1),
	] {
		let p = kick_project(at, true);
		let expected = p.tempo.tick_to_sample(at, 48_000.0) as usize;
		for block_size in [1, 64, 333, 512] {
			let out = render_offline(
				&p,
				&RenderOptions {
					block_size,
					..opts(12.0)
				},
			)
			.unwrap();
			let onset = out.left.iter().position(|x| *x != 0.0).unwrap();
			assert_eq!(onset - first, expected, "tick {at:?}, block {block_size}");
			// Same waveform as the reference hit, shifted by exactly the expected samples.
			assert_eq!(
				&out.left[expected..expected + window],
				&reference.left[..window],
				"tick {at:?}, block {block_size}"
			);
		}
	}
}

#[test]
fn render_starting_mid_song_is_aligned_with_full_render() {
	let p = kick_project(beats(3.0) + Tick(5), true);
	let full = render_offline(&p, &opts(8.0)).unwrap();
	let start = beats(2.0) + Tick(17);
	let partial = render_offline(&p, &RenderOptions { start, ..opts(8.0) }).unwrap();
	let offset = p.tempo.tick_to_sample(start, 48_000.0) as usize;
	assert_eq!(partial.len(), full.len() - offset);
	assert_eq!(&partial.left[..], &full.left[offset..]);
}

/// Note chasing: starting/seeking in the middle of a sustained note must make it sound
/// immediately, and a plain from-the-start render must be exactly as before (chasing only
/// kicks in when a start/seek lands strictly inside a note's span).
#[test]
fn render_starting_mid_note_chases_it_and_full_render_is_unaffected() {
	let t = synth_track("pad", &[(60, 0.0, 8.0)], 8.0);
	let p = project(vec![t]);

	// A couple of cycles of the lowest note involved, generous enough that "immediately"
	// can't be mistaken for "once the waveform happens to cross zero".
	let window = 2_000;

	// From the start, the note's regular note-on at sample 0 covers it; nothing to chase.
	// This must render exactly as it did before note chasing existed, so it is the baseline
	// "sounding immediately" level a mid-note seek must match.
	let full = render_offline(&p, &opts(8.0)).unwrap();
	let full_peak = peak(&full.left[..window]);
	assert!(
		full_peak > 0.01,
		"sanity check: the note itself must be audible at all, peak {full_peak}"
	);

	// Seeking to the middle of that same note must *also* sound immediately: before note
	// chasing this window would have been exact silence (its note-on lies before `start`
	// and is never replayed), which this asserts is no longer the case.
	let start = beats(4.0);
	let partial = render_offline(&p, &RenderOptions { start, ..opts(8.0) }).unwrap();
	let partial_peak = peak(&partial.left[..window]);
	assert!(
		partial_peak > full_peak * 0.5,
		"a mid-note seek must chase the note so it sounds immediately, \
		 like the from-the-start render does: full {full_peak}, partial {partial_peak}"
	);
}
