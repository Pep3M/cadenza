//! Automation of volume, pan and device parameters.

mod common;

use cadenza_core::ParamAddress;
use cadenza_engine::render_offline;
use cadenza_model::{AutomationLane, AutomationPoint, Device};
use common::*;

fn seconds(s: f64) -> usize {
	(s * 48_000.0) as usize
}

#[test]
fn volume_automation_fades_in() {
	let a = synth_track("a", &[(60, 0.0, 8.0)], 8.0);
	let mut p = project(vec![a.clone()]);
	p.automation.push(AutomationLane::new(
		ParamAddress::track_volume(a.id),
		vec![
			AutomationPoint::new(beats(0.0), -60.0),
			AutomationPoint::new(beats(8.0), 0.0),
		],
	));
	let out = render_offline(&p, &opts(8.0)).unwrap();
	// 8 beats at 120 bpm = 4 s. Level must rise monotonically, ~60 dB overall.
	let windows: Vec<f64> = (0..8)
		.map(|i| rms(&out.left[seconds(i as f64 * 0.5)..seconds(i as f64 * 0.5 + 0.5)]))
		.collect();
	assert!(windows.windows(2).all(|w| w[1] > w[0]), "{windows:?}");
	let span = 20.0 * (windows[7] / windows[0]).log10();
	assert!(span > 45.0 && span < 75.0, "span {span} dB");
}

#[test]
fn lane_overrides_static_value_and_holds_after_last_point() {
	let mut a = synth_track("a", &[(60, 0.0, 8.0)], 8.0);
	let reference = render_offline(&project(vec![a.clone()]), &opts(8.0)).unwrap();
	a.channel.volume_db = -30.0;
	let mut p = project(vec![a.clone()]);
	p.automation.push(AutomationLane::new(
		ParamAddress::track_volume(a.id),
		vec![AutomationPoint::new(beats(1.0), 0.0)],
	));
	let out = render_offline(&p, &opts(8.0)).unwrap();
	assert!(max_diff(&out, &reference) < 1e-6);
}

#[test]
fn pan_and_device_param_automation() {
	let mut a = synth_track("a", &[(60, 0.0, 8.0)], 8.0);
	let gain = Device::builtin("gain", "Trim");
	a.channel.inserts.push(gain.clone());
	let mut p = project(vec![a.clone()]);
	p.automation.push(AutomationLane::new(
		ParamAddress::track_pan(a.id),
		vec![
			AutomationPoint::new(beats(1.0), -1.0),
			AutomationPoint::new(beats(7.0), 1.0),
		],
	));
	p.automation.push(AutomationLane::new(
		ParamAddress::device(Some(a.id), gain.id, "gain"),
		vec![
			AutomationPoint::new(beats(4.0), 0.0),
			AutomationPoint::new(beats(4.0) + cadenza_core::Tick(1), -40.0),
		],
	));
	let out = render_offline(&p, &opts(8.0)).unwrap();
	let early = seconds(0.2)..seconds(0.4);
	let late = seconds(3.6)..seconds(3.8);
	assert!(rms(&out.left[early.clone()]) > 10.0 * rms(&out.right[early]));
	assert!(rms(&out.right[late.clone()]) > 10.0 * rms(&out.left[late]));
	let before =
		rms(&out.left[seconds(1.6)..seconds(1.9)]) + rms(&out.right[seconds(1.6)..seconds(1.9)]);
	let after =
		rms(&out.left[seconds(2.2)..seconds(2.5)]) + rms(&out.right[seconds(2.2)..seconds(2.5)]);
	assert!(
		before > 30.0 * after,
		"device gain automation: {before} vs {after}"
	);
}

#[test]
fn send_levels_and_point_shapes_are_automated() {
	let mut a = synth_track("a", &[(60, 0.0, 8.0)], 8.0);
	let fx = bus("fx");
	let s = send(&fx, 0.0, true);
	let send_id = s.id;
	a.channel.sends.push(s);
	a.channel.volume_db = -144.0;
	let mut p = project(vec![a.clone(), fx]);
	// Send silent for 4 beats (held), then open.
	p.automation.push(AutomationLane::new(
		ParamAddress::send(a.id, send_id),
		vec![
			AutomationPoint::new(beats(0.0), -144.0).with_shape(cadenza_model::PointShape::Hold),
			AutomationPoint::new(beats(4.0), 0.0),
		],
	));
	let out = render_offline(&p, &opts(8.0)).unwrap();
	let quiet = rms(&out.left[seconds(0.5)..seconds(1.9)]);
	let loud = rms(&out.left[seconds(2.2)..seconds(3.5)]);
	assert!(quiet < 1e-6 && loud > 1e-3, "{quiet} vs {loud}");
}

#[test]
fn automation_swaps_into_a_compiled_graph() {
	let a = synth_track("a", &[(60, 0.0, 8.0)], 8.0);
	let p = project(vec![a.clone()]);
	let mut graph = cadenza_engine::compile(&p, 48_000, 512).unwrap();
	let lanes = [AutomationLane::new(
		ParamAddress::track_volume(a.id),
		vec![AutomationPoint::new(beats(0.0), -80.0)],
	)];
	let mut bindings = graph.prepare_automation(&lanes, &p.tempo);
	graph.play();
	let mut l = vec![0.0f32; 24_000];
	let mut r = vec![0.0f32; 24_000];
	graph.process(&mut l, &mut r);
	let before = rms(&l[12_000..]);
	graph.swap_automation(&mut bindings);
	graph.process(&mut l, &mut r);
	let after = rms(&l[12_000..]);
	assert!(after < before * 1e-3, "{after} vs {before}");
}

#[test]
fn delay_feedback_and_tap_automation_reach_the_processor() {
	// One short note into a free 250 ms delay with no repeats of its own: a feedback lane
	// makes the echoes ring on, a tap lane adds a second read head's echo.
	let mut a = synth_track("a", &[(60, 0.0, 0.25)], 8.0);
	let delay = Device::builtin("delay", "Echo")
		.with_param("sync", 0.0)
		.with_param("time", 250.0)
		.with_param("feedback", 0.0)
		.with_param("mix", 100.0)
		.with_param("taps", 2.0)
		.with_param("tap_spread", 100.0)
		.with_param("tap2_level", 0.0);
	a.channel.inserts.push(delay.clone());
	let plain = render_offline(&project(vec![a.clone()]), &opts(8.0)).unwrap();
	let lane = |param: &str, value: f32| {
		AutomationLane::new(
			ParamAddress::device(Some(a.id), delay.id, param),
			vec![AutomationPoint::new(beats(0.0), value)],
		)
	};
	let mut p = project(vec![a.clone()]);
	p.automation.push(lane("feedback", 90.0));
	let ringing = render_offline(&p, &opts(8.0)).unwrap();
	let tail = seconds(1.5)..seconds(2.5);
	let (dry_tail, wet_tail) = (rms(&plain.left[tail.clone()]), rms(&ringing.left[tail]));
	assert!(
		dry_tail < 1e-5 && wet_tail > 1e-3,
		"feedback lane: {dry_tail} vs {wet_tail}"
	);
	// Tap 2 at 2 x 250 ms = 500 ms: silent at level 0, an echo once a lane raises it.
	let mut p = project(vec![a.clone()]);
	p.automation.push(lane("tap2_level", 100.0));
	let tapped = render_offline(&p, &opts(8.0)).unwrap();
	let tap = seconds(0.5)..seconds(0.62);
	let (off, on) = (rms(&plain.left[tap.clone()]), rms(&tapped.left[tap]));
	assert!(on > 10.0 * off.max(1e-6), "tap lane: {off} vs {on}");
}

/// Regression (demo-techno): playing from the middle of the song through an automated
/// synth into a tube saturator burst to ~500x full scale on the first samples, because the
/// reset left the saturator's anti-aliasing state inconsistent. Starting anywhere must
/// sound like the same spot reached from the start.
#[test]
fn starting_mid_song_has_no_transient() {
	let mut a = synth_track(
		"lead",
		&[
			(57, 0.0, 1.5),
			(60, 2.0, 1.5),
			(64, 4.0, 1.5),
			(62, 6.0, 1.5),
		],
		8.0,
	);
	let synth_id = a.instrument.as_ref().unwrap().id;
	a.channel.inserts.push(
		Device::builtin("saturator", "Drive")
			.with_param("mode", 1.0)
			.with_param("drive", 11.0),
	);
	let mut p = project(vec![a.clone()]);
	p.automation.push(AutomationLane::new(
		ParamAddress::device(Some(a.id), synth_id, "filter_cutoff"),
		vec![
			AutomationPoint::new(beats(0.0), 200.0),
			AutomationPoint::new(beats(8.0), 4000.0),
		],
	));
	let full = render_offline(&p, &opts(8.0)).unwrap();
	let steady = peak(&full.left);
	for start in [1.0, 2.0, 3.0, 4.0, 6.0] {
		let mid = render_offline(
			&p,
			&cadenza_engine::RenderOptions {
				start: beats(start),
				..opts(8.0)
			},
		)
		.unwrap();
		let first = peak(&mid.left[..seconds(0.05)]).max(peak(&mid.right[..seconds(0.05)]));
		assert!(
			first <= steady * 1.5,
			"start at beat {start}: first 50 ms peak {first} vs whole song peak {steady}"
		);
	}
}

#[test]
fn render_follows_the_segment_tension() {
	let a = synth_track("a", &[(60, 0.0, 8.0)], 8.0);
	let level_at_quarter = |tension: f32| {
		let mut p = project(vec![a.clone()]);
		p.automation.push(AutomationLane::new(
			ParamAddress::track_volume(a.id),
			vec![
				AutomationPoint::new(beats(0.0), -60.0).with_tension(tension),
				AutomationPoint::new(beats(8.0), 0.0),
			],
		));
		let out = render_offline(&p, &opts(8.0)).unwrap();
		20.0 * rms(&out.left[seconds(0.95)..seconds(1.05)]).log10()
	};
	let (concave, straight, convex) = (
		level_at_quarter(0.8),
		level_at_quarter(0.0),
		level_at_quarter(-0.8),
	);
	// A quarter of the way in, the model says -45 dB straight; the bent curves sit
	// well above (fast start) and below (slow start).
	let model = |k: f32| cadenza_model::PointShape::Linear.interpolate(k, -60.0, 0.0, 0.25);
	assert!(concave > straight + 20.0, "{concave} vs {straight}");
	assert!(convex < straight - 5.0, "{convex} vs {straight}");
	assert!(
		((concave - straight) - f64::from(model(0.8) - model(0.0))).abs() < 3.0,
		"render follows the model curve"
	);
}
