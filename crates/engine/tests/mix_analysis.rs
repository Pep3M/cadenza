//! Detailed mix analysis: per-channel spectrum, stereo image and the masking report computed
//! from a real render.

mod common;

use cadenza_analysis::issues::{TrackInput, track_masking};
use cadenza_analysis::roles::Role;
use cadenza_engine::{EngineError, analyze_mix, analyze_mix_detailed};
use cadenza_model::Device;
use common::*;

#[test]
fn channels_get_spectra_and_overlapping_parts_mask_each_other() {
	// Two sustained low notes an octave apart (A1, A2) and a high one: the low pair collides.
	let bass = synth_track("Bass", &[(33, 0.0, 16.0)], 16.0);
	let low_keys = synth_track("Keys", &[(45, 0.0, 16.0)], 16.0);
	let mut lead = synth_track("Lead", &[(84, 0.0, 16.0)], 16.0);
	lead.channel.pan = 0.8;
	let p = project(vec![bass, low_keys, lead]);
	let o = opts(16.0);
	let mut calls = 0;
	let mix = analyze_mix_detailed(&p, &o, &mut |f| {
		calls += 1;
		assert!((0.0..=1.0).contains(&f));
		true
	})
	.unwrap();
	assert!(calls > 0);
	// The detailed report keeps the plain levels identical to analyze_mix.
	assert_eq!(mix.report, analyze_mix(&p, &o).unwrap());
	assert_eq!(mix.tracks.len(), 3);
	assert_eq!(mix.tracks[0].name, "Bass");
	let peak = |i: usize| mix.tracks[i].detail.spectrum.peak_octave();
	assert!(peak(0) <= 2, "A1 (55 Hz) and its harmonics: {}", peak(0));
	assert_eq!(peak(2), 5, "C6 (1047 Hz) is in the 1 kHz octave");
	assert!(
		mix.tracks[2].detail.stereo.width > 0.1,
		"panned lead is wide"
	);
	assert!(
		mix.tracks[0].detail.stereo.width < 0.01,
		"centered bass is mono"
	);
	assert!(mix.master.stereo.low_correlation > 0.95);
	let frames = mix.tracks[0].detail.timeline.frames.len();
	assert!(frames > 50, "8 s at 85 ms hops: {frames}");

	let roles = [Role::Bass, Role::Keys, Role::Lead];
	let inputs: Vec<TrackInput<'_>> = mix
		.tracks
		.iter()
		.zip(roles)
		.map(|(t, role)| TrackInput {
			id: t.track_id,
			name: &t.name,
			role,
			is_bus: false,
			detail: &t.detail,
			pan: 0.0,
			high_pass_hz: None,
		})
		.collect();
	let pairs = track_masking(&inputs);
	assert!(!pairs.is_empty());
	let top = &pairs[0];
	assert_eq!((top.a, top.b), (0, 1), "{pairs:?}");
	// The lows belong to the bass, the mids more to the keys.
	let expected_cut = if top.band_hz <= 125.0 { 1 } else { 0 };
	assert_eq!(top.cut, expected_cut, "{pairs:?}");
	assert!(!pairs.iter().any(|p| p.a == 2 || p.b == 2 && p.score > 0.5));
}

#[test]
fn detailed_analysis_can_be_cancelled() {
	let p = project(vec![instrument_track(
		"Drums",
		Device::builtin("drum_machine", "Drums"),
		&[(36, 0.0, 0.5), (75, 1.0, 0.5), (63, 2.0, 0.5)],
		4.0,
	)]);
	let err = analyze_mix_detailed(&p, &opts(64.0), &mut |_| false).unwrap_err();
	assert!(matches!(err, EngineError::Cancelled));
}
