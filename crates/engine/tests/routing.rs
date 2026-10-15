//! Bus routing, sends (pre/post fader) and constant-power/balance panning.

mod common;

use cadenza_engine::{analyze_mix, render_offline};
use cadenza_model::Output;
use common::*;

#[test]
fn bus_output_applies_bus_fader() {
	let a = synth_track("a", &[(60, 0.0, 4.0)], 4.0);
	let direct = render_offline(&project(vec![a.clone()]), &opts(4.0)).unwrap();

	let mut group = bus("group");
	group.channel.volume_db = -6.0;
	let mut routed = a;
	routed.channel.output = Output::Bus(group.id);
	let out = render_offline(&project(vec![routed, group]), &opts(4.0)).unwrap();
	let g = cadenza_core::db_to_gain(-6.0);
	for (x, y) in out.left.iter().zip(&direct.left) {
		assert!((x - y * g).abs() < 1e-5);
	}
	assert!(peak(&out.left) > 0.01);
}

#[test]
fn pre_fader_send_ignores_track_fader_post_fader_follows_it() {
	let a = synth_track("a", &[(60, 0.0, 4.0)], 4.0);
	let direct = render_offline(&project(vec![a.clone()]), &opts(4.0)).unwrap();

	let fx = bus("fx");
	let mut pre = a.clone();
	pre.channel.volume_db = -144.0;
	pre.channel.sends.push(send(&fx, 0.0, true));
	let out = render_offline(&project(vec![pre, fx.clone()]), &opts(4.0)).unwrap();
	assert!(
		max_diff(&out, &direct) < 1e-5,
		"pre-fader send carries the full signal"
	);

	let mut post = a.clone();
	post.channel.volume_db = -144.0;
	post.channel.sends.push(send(&fx, 0.0, false));
	let out = render_offline(&project(vec![post, fx.clone()]), &opts(4.0)).unwrap();
	assert_eq!(peak(&out.left), 0.0, "post-fader send follows the fader");

	// Track at unity + post send at -6 dB: master = direct * (1 + 0.5).
	let mut both = a;
	both.channel.sends.push(send(&fx, -6.0, false));
	let p = project(vec![both, fx]);
	let out = render_offline(&p, &opts(4.0)).unwrap();
	let g = 1.0 + cadenza_core::db_to_gain(-6.0);
	for (x, y) in out.right.iter().zip(&direct.right) {
		assert!((x - y * g).abs() < 1e-5);
	}
	let report = analyze_mix(&p, &opts(4.0)).unwrap();
	let fx_level = &report.tracks[1];
	assert_eq!(fx_level.name, "fx");
	assert!((report.tracks[0].stats.peak_db - fx_level.stats.peak_db - 6.0).abs() < 0.05);
}

#[test]
fn chained_buses_are_processed_in_topological_order() {
	// Display order lists the downstream bus first.
	let a = synth_track("a", &[(60, 0.0, 4.0)], 4.0);
	let direct = render_offline(&project(vec![a.clone()]), &opts(4.0)).unwrap();
	let master_bus = bus("2-bus");
	let mut sub = bus("sub");
	sub.channel.output = Output::Bus(master_bus.id);
	let mut routed = a;
	routed.channel.output = Output::Bus(sub.id);
	let out = render_offline(&project(vec![master_bus, routed, sub]), &opts(4.0)).unwrap();
	assert!(max_diff(&out, &direct) < 1e-6);
}

#[test]
fn stereo_source_pan_is_balance() {
	let mut a = synth_track("a", &[(60, 0.0, 4.0)], 4.0);
	let center = render_offline(&project(vec![a.clone()]), &opts(4.0)).unwrap();
	a.channel.pan = 1.0;
	let right = render_offline(&project(vec![a]), &opts(4.0)).unwrap();
	assert_eq!(peak(&right.left), 0.0);
	assert!(
		max_diff(
			&cadenza_engine::StereoBuffer {
				left: vec![0.0; center.len()],
				..center.clone()
			},
			&cadenza_engine::StereoBuffer {
				left: vec![0.0; center.len()],
				..right
			}
		) < 1e-6
	);
}
