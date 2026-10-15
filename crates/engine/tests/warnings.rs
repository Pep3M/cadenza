//! Unsupported or unknown devices and params are reported, never fatal.

mod common;

use cadenza_core::{DeviceId, ParamAddress};
use cadenza_engine::{analyze_mix, compile, render_offline};
use cadenza_model::{AutomationLane, AutomationPoint, Device, DeviceKind};
use common::*;

#[test]
fn unknown_devices_and_params_become_warnings() {
	let mut a = synth_track("lead", &[(60, 0.0, 2.0)], 2.0);
	a.channel
		.inserts
		.push(Device::builtin("flux_capacitor", "Mystery"));
	a.channel
		.inserts
		.push(Device::builtin("eq", "EQ").with_param("warp_factor", 9.0));
	a.channel.inserts.push(Device {
		kind: DeviceKind::Clap {
			plugin_id: "com.example.verb".into(),
			path: "/plugins/verb.clap".into(),
		},
		..Device::builtin("x", "Fancy Verb")
	});
	a.channel
		.inserts
		.push(Device::builtin("poly_synth", "Synth as effect"));
	let mut bypassed = Device::builtin("flux_capacitor", "Bypassed");
	bypassed.bypass = true;
	a.channel.inserts.push(bypassed);
	let mut p = project(vec![a.clone()]);
	p.automation.push(AutomationLane::new(
		ParamAddress::device(Some(a.id), DeviceId::new(), "gain"),
		vec![AutomationPoint::new(beats(0.0), 0.0)],
	));

	let graph = compile(&p, 48_000, 512).unwrap();
	let w = graph.warnings();
	assert_eq!(w.len(), 5, "{w:#?}");
	assert!(
		w.iter()
			.any(|m| m.contains("unknown built-in device 'flux_capacitor'"))
	);
	assert!(w.iter().any(|m| m.contains("no parameter 'warp_factor'")));
	assert!(
		w.iter()
			.any(|m| m.contains("CLAP plugin 'com.example.verb'"))
	);
	assert!(w.iter().any(|m| m.contains("cannot be used as an effect")));
	assert!(w.iter().any(|m| m.contains("device that does not exist")));

	let out = render_offline(&p, &opts(2.0)).unwrap();
	assert!(
		peak(&out.left) > 0.01,
		"the rest of the chain still renders"
	);
	let report = analyze_mix(&p, &opts(2.0)).unwrap();
	assert_eq!(report.warnings, w);
}
