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
