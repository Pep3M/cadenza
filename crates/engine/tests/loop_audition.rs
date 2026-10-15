//! Loop playback and audition scopes through the public graph API.

mod common;

use cadenza_core::Tick;
use cadenza_engine::{StereoBuffer, compile, render_offline};
use common::*;

fn run(graph: &mut cadenza_engine::Graph, frames: usize, block: usize) -> StereoBuffer {
	let mut out = StereoBuffer {
		sample_rate: 48_000,
		left: vec![0.0; frames],
		right: vec![0.0; frames],
	};
	for (l, r) in out.left.chunks_mut(block).zip(out.right.chunks_mut(block)) {
		graph.process(l, r);
	}
	out
}

#[test]
fn a_note_crossing_the_loop_end_does_not_hang_into_the_next_pass() {
	// Loop [0, 2 beats); the only note starts at beat 1 and lasts 4 beats.
	let p = project(vec![synth_track("lead", &[(69, 1.0, 4.0)], 8.0)]);
	let mut g = compile(&p, 48_000, 512).unwrap();
	g.set_loop(Some((Tick::ZERO, beats(2.0))));
	g.play();
	// Two passes (2 beats = 48_000 samples each), in odd-sized blocks.
	let out = run(&mut g, 96_000, 333);
	let first_half = rms(&out.left[12_000..24_000]);
	let sounding = rms(&out.left[30_000..48_000]);
	let next_pass = rms(&out.left[48_000 + 12_000..48_000 + 24_000]);
	assert_eq!(first_half, 0.0, "nothing before the note");
	assert!(sounding > 0.01, "the note sounds in the loop: {sounding}");
	assert!(
		next_pass < 1e-4,
		"released at the wrap, silent until it starts again: {next_pass}"
	);
	assert!(rms(&out.left[48_000 + 30_000..]) > 0.01, "and plays again");
}

#[test]
fn a_loop_renders_the_same_as_the_straight_song_on_each_pass() {
	// No note crosses the loop end and nothing rings past it, so every pass equals the
	// first beats of a straight render.
	let mut lead = synth_track("lead", &[(60, 0.0, 0.5), (64, 1.0, 0.5)], 4.0);
	lead.instrument = Some(
		synth()
			.with_param("amp_release", 1.0)
			.with_param("amp_sustain", 100.0),
	);
	let p = project(vec![lead]);
	let straight = render_offline(&p, &opts(2.0)).unwrap();
	let mut g = compile(&p, 48_000, 512).unwrap();
	assert_eq!(g.loop_samples(), None, "offline graphs do not loop");
	g.set_loop(Some((Tick::ZERO, beats(2.0))));
	g.play();
	let looped = run(&mut g, 3 * 48_000, 512);
	let pass = |i: usize| StereoBuffer {
		left: looped.left[i * 48_000..(i + 1) * 48_000].to_vec(),
		right: looped.right[i * 48_000..(i + 1) * 48_000].to_vec(),
		..straight.clone()
	};
	assert!(max_diff(&pass(0), &straight) < 1e-6);
	// Later passes restart the same notes on the warm instrument: same timing.
	for i in 1..3 {
		let a = pass(i);
		assert!(rms(&a.left[..20_000]) > 0.01);
		assert!(
			rms(&a.left[13_000..24_000]) < 1e-3,
			"pass {i}: gap before beat 1"
		);
	}
}

#[test]
fn audition_plays_only_the_scope_and_ignores_other_solos() {
	let reverb = bus("Reverb");
	let mut a = synth_track("a", &[(60, 0.0, 2.0)], 2.0);
	a.channel.sends.push(send(&reverb, -6.0, false));
	let mut b = synth_track("b", &[(67, 0.0, 2.0)], 2.0);
	b.channel.solo = true;
	// A pre-fader key from b still reaches a's compressor while b is not heard.
	a.channel.inserts.push(
		cadenza_model::Device::builtin("compressor", "Duck")
			.with_param("threshold", -40.0)
			.with_sidechain(b.id, true),
	);
	let (a_id, b_id) = (a.id, b.id);
	let p = project(vec![a, b, reverb]);

	// Reference: b muted instead (and nothing soloed).
	let mut reference = p.clone();
	reference.tracks[1].channel.solo = false;
	reference.tracks[1].channel.mute = true;
	let expected = render_offline(&reference, &opts(2.0)).unwrap();

	let mut g = compile(&p, 48_000, 512).unwrap();
	let meters = g.meters();
	g.set_audition(Some(&[a_id]));
	g.play();
	let got = run(&mut g, expected.len(), 256);
	assert!(rms(&got.left) > 0.01);
	assert_eq!(max_diff(&got, &expected), 0.0, "bit-identical to muting b");
	assert_eq!(
		meters.track(b_id).unwrap().max_peak(),
		0.0,
		"b is not heard"
	);

	// Back to the song: b is soloed, so only b (and nothing else) plays.
	g.set_audition(None);
	g.seek(Tick::ZERO);
	let song = run(&mut g, expected.len(), 256);
	let solo = render_offline(&p, &opts(2.0)).unwrap();
	assert_eq!(max_diff(&song, &solo), 0.0);

	// A muted track stays muted when auditioned.
	let mut muted = p.clone();
	muted.tracks[0].channel.mute = true;
	let mut g = compile(&muted, 48_000, 512).unwrap();
	g.set_audition(Some(&[a_id]));
	g.play();
	assert_eq!(peak(&run(&mut g, 24_000, 256).left), 0.0);
}
