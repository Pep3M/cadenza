//! Plugin delay compensation: paths with and without latency must stay time-aligned.

mod common;

use cadenza_engine::{analyze_mix, compile, render_offline};
use cadenza_model::{Device, Output};
use common::*;

/// Limiter set so it never acts (signal peaks far below a 0 dB ceiling): pure latency.
fn transparent_limiter() -> Device {
	Device::builtin("limiter", "Lim")
		.with_param("input_gain", 0.0)
		.with_param("ceiling", 0.0)
}

fn inverter() -> Device {
	Device::builtin("gain", "Invert")
		.with_param("invert_left", 1.0)
		.with_param("invert_right", 1.0)
}

const NOTES: &[(u8, f64, f64)] = &[
	(60, 0.0, 1.0),
	(64, 1.25, 0.5),
	(67, 2.0, 1.5),
	(72, 3.1, 0.6),
];

#[test]
fn latent_path_nulls_against_inverted_dry_path() {
	let mut wet = synth_track("wet", NOTES, 4.0);
	wet.channel.inserts.push(transparent_limiter());
	let mut dry = synth_track("dry", NOTES, 4.0);
	dry.channel.inserts.push(inverter());
	let p = project(vec![wet.clone(), dry]);
	let graph = compile(&p, 48_000, 512).unwrap();
	assert_eq!(graph.latency_samples(), 240, "5 ms lookahead at 48 kHz");

	let wet_alone = render_offline(&project(vec![wet]), &opts(4.0)).unwrap();
	assert!(peak(&wet_alone.left) > 0.05 && peak(&wet_alone.left) < 0.9);
	let sum = render_offline(&p, &opts(4.0)).unwrap();
	let residual = peak(&sum.left).max(peak(&sum.right));
	assert!(residual < 1e-5, "residual {residual}");
}

#[test]
fn null_through_buses_and_master_latency_is_trimmed() {
	// wet: track -> bus(limiter) -> master; dry: inverted track -> master.
	let mut group = bus("latent bus");
	group.channel.inserts.push(transparent_limiter());
	let mut wet = synth_track("wet", NOTES, 4.0);
	wet.channel.output = Output::Bus(group.id);
	let mut dry = synth_track("dry", NOTES, 4.0);
	dry.channel.inserts.push(inverter());
	let p = project(vec![wet.clone(), dry.clone(), group.clone()]);
	let sum = render_offline(&p, &opts(4.0)).unwrap();
	assert!(peak(&sum.left).max(peak(&sum.right)) < 1e-5);

	// A master limiter adds latency for everything; the render is still timeline-aligned.
	let mut plain = project(vec![synth_track("dry", NOTES, 4.0)]);
	let reference = render_offline(&plain, &opts(4.0)).unwrap();
	plain.master.inserts.push(transparent_limiter());
	let delayed = render_offline(&plain, &opts(4.0)).unwrap();
	assert!(max_diff(&reference, &delayed) < 1e-6);
	let report = analyze_mix(&plain, &opts(4.0)).unwrap();
	assert_eq!(report.latency_samples, 240);
	// src -> "dry" bus (inverter, no latency) and a send -> "fx" bus (limiter): the dry bus
	// is delayed at the master to meet the latent send path.
	let mut fx = bus("fx");
	fx.channel.inserts.push(transparent_limiter());
	let mut dry_bus = bus("dry");
	dry_bus.channel.inserts.push(inverter());
	let mut src = synth_track("src", NOTES, 4.0);
	src.channel.sends.push(send(&fx, 0.0, false));
	src.channel.output = Output::Bus(dry_bus.id);
	let both = render_offline(&project(vec![src, fx, dry_bus]), &opts(4.0)).unwrap();
	assert!(
		peak(&both.left).max(peak(&both.right)) < 1e-5,
		"inverted dry + send must null"
	);
}
