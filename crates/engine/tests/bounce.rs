//! Freeze / bounce renders: a track rendered alone before its fader, played back through
//! its fader, sounds like the original.

mod common;

use cadenza_engine::{BounceScope, render_offline, render_track};
use cadenza_model::{Device, Freeze};
use common::*;

#[test]
fn a_frozen_track_sounds_like_the_original() {
	let mut lead = synth_track("lead", &[(60, 0.0, 2.0), (64, 2.0, 2.0)], 4.0);
	lead.channel
		.inserts
		.push(Device::builtin("eq", "EQ").with_param("low_gain", 6.0));
	lead.channel.volume_db = -4.0;
	lead.channel.pan = 0.3;
	let other = synth_track("other", &[(48, 0.0, 4.0)], 4.0);
	let id = lead.id;
	let p = project(vec![lead, other]);
	let original = render_offline(&p, &opts(6.0)).unwrap();

	let file = tmp_path("frozen-lead.wav");
	let report = render_track(
		&p,
		&opts(6.0),
		id,
		&BounceScope::Track,
		cadenza_core::Tick::ZERO,
		&file,
	)
	.unwrap();
	assert!(report.master.peak_db > -40.0, "the render has the track");
	let mut frozen = p.clone();
	frozen.tracks[0].freeze = Some(Freeze { file: file.clone() });
	let played = render_offline(&frozen, &opts(6.0)).unwrap();
	let diff = max_diff(&original, &played);
	assert!(diff < 1e-3, "frozen playback differs by {diff}");

	// Only some clips, without devices (consolidation): the render has them.
	let clip = p.tracks[1].clips[0].id;
	let clips = tmp_path("clips-other.wav");
	let report = render_track(
		&p,
		&opts(6.0),
		p.tracks[1].id,
		&BounceScope::Clips(vec![clip]),
		cadenza_core::Tick::ZERO,
		&clips,
	)
	.unwrap();
	assert!(report.master.peak_db > -40.0);
	let _ = std::fs::remove_file(file);
	let _ = std::fs::remove_file(clips);
}
