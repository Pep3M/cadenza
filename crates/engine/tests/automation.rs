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
