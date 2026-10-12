//! Third-party CLAP plugins inside the engine: parameters, latency compensation, plugin
//! instruments, state restore and graceful bypass of missing plugins. Plugins run
//! sandboxed (the engine default); `sandbox.rs` covers parity with in-process hosting.

mod common;
mod plugin_support;

use cadenza_engine::{RenderOptions, analyze_mix, compile_hosted, render_offline};
use cadenza_model::{Device, DeviceKind, Track, TrackKind};
use common::*;
use plugin_support::{GAIN_ID, SINE_ID, catalog, plugin_path, sandboxed};

const NOTES: &[(u8, f64, f64)] = &[
	(60, 0.0, 1.0),
	(64, 1.25, 0.5),
	(67, 2.0, 1.5),
	(72, 3.1, 0.6),
];

fn plugin_device(id: &str, name: &str) -> Device {
	let mut d = Device::builtin("unused", name);
	d.kind = DeviceKind::Clap {
		plugin_id: id.into(),
		path: plugin_path(id),
	};
	d
}

fn plugin_gain(db: Option<f32>) -> Device {
	let d = plugin_device(GAIN_ID, "Test Gain");
	match db {
		Some(db) => d.with_param("0", db),
		None => d,
	}
}

fn with_plugins(end_beats: f64) -> RenderOptions {
	RenderOptions {
		plugins: Some(catalog()),
		hosting: sandboxed(),
		..opts(end_beats)
	}
}

fn inverter() -> Device {
	Device::builtin("gain", "Invert")
		.with_param("invert_left", 1.0)
		.with_param("invert_right", 1.0)
}

#[test]
fn plugin_gain_changes_track_level() {
	let dry = render_offline(&project(vec![synth_track("s", NOTES, 4.0)]), &opts(4.0)).unwrap();
	let mut t = synth_track("s", NOTES, 4.0);
	t.channel.inserts.push(plugin_gain(Some(-12.0)));
	let p = project(vec![t]);
	let wet = render_offline(&p, &with_plugins(4.0)).unwrap();
	assert_finite(&wet);
	let ratio = rms(&wet.left) / rms(&dry.left);
	let expected = 10f64.powf(-12.0 / 20.0);
	assert!(
		(ratio - expected).abs() < 0.01,
		"ratio {ratio}, expected {expected}"
	);
	let report = analyze_mix(&p, &with_plugins(4.0)).unwrap();
	assert!(report.warnings.is_empty(), "{:?}", report.warnings);
	assert_eq!(report.latency_samples, 64);
}

#[test]
fn plugin_latency_is_compensated() {
	let mut wet = synth_track("wet", NOTES, 4.0);
	wet.channel.inserts.push(plugin_gain(Some(0.0)));
	let mut dry = synth_track("dry", NOTES, 4.0);
	dry.channel.inserts.push(inverter());
	let p = project(vec![wet.clone(), dry]);
	let graph = compile_hosted(&p, 48_000, 512, Some(&catalog()), &sandboxed()).unwrap();
	// Realtime graphs batch 4 quanta per sandbox round trip (3 quanta of latency).
	assert_eq!(graph.latency_samples(), 64 + 96);
	assert!(graph.warnings().is_empty(), "{:?}", graph.warnings());

	let wet_alone = render_offline(&project(vec![wet]), &with_plugins(4.0)).unwrap();
	assert!(peak(&wet_alone.left) > 0.05);
	let sum = render_offline(&p, &with_plugins(4.0)).unwrap();
	let residual = peak(&sum.left).max(peak(&sum.right));
	assert!(residual < 1e-5, "residual {residual}");

	// The render is timeline-aligned: identical to the plugin-free render.
	let reference =
		render_offline(&project(vec![synth_track("s", NOTES, 4.0)]), &opts(4.0)).unwrap();
	assert!(max_diff(&reference, &wet_alone) < 1e-6);
}

#[test]
fn plugin_render_is_block_size_independent() {
	let mut t = synth_track("s", NOTES, 4.0);
	t.channel.inserts.push(plugin_gain(Some(-3.0)));
	let p = project(vec![t]);
	let a = render_offline(
		&p,
		&RenderOptions {
			block_size: 64,
			..with_plugins(4.0)
		},
	)
	.unwrap();
	let b = render_offline(
		&p,
		&RenderOptions {
			block_size: 1000,
			..with_plugins(4.0)
		},
	)
	.unwrap();
	assert_eq!(max_diff(&a, &b), 0.0);
}

#[test]
fn plugin_instrument_plays_notes() {
	let t = instrument_track(
		"sine",
		plugin_device(SINE_ID, "Test Sine"),
		&[(69, 0.0, 1.0)],
		2.0,
	);
	let p = project(vec![t]);
	let out = render_offline(&p, &with_plugins(2.0)).unwrap();
	assert_finite(&out);
	let beat = 24_000; // 120 bpm at 48 kHz
	let note = rms(&out.left[1_000..beat - 1_000]);
	let after = rms(&out.left[beat + 4_800..]);
	assert!(note > 0.05, "note rms {note}");
	assert!(after < 1e-4, "after release rms {after}");
	let report = analyze_mix(&p, &with_plugins(2.0)).unwrap();
	assert!(report.warnings.is_empty(), "{:?}", report.warnings);
}

#[test]
fn plugin_state_is_restored() {
	let mut state = b"CDZ1".to_vec();
	state.extend_from_slice(&(-20.0f32).to_le_bytes());
	let mut gain = plugin_gain(None);
	gain.state = Some(cadenza_plugin_host::encode_state(&state));
	let mut t = synth_track("s", NOTES, 4.0);
	t.channel.inserts.push(gain.clone());
	let restored = render_offline(&project(vec![t]), &with_plugins(4.0)).unwrap();
	let dry = render_offline(&project(vec![synth_track("s", NOTES, 4.0)]), &opts(4.0)).unwrap();
	let ratio = rms(&restored.left) / rms(&dry.left);
	assert!((ratio - 0.1).abs() < 0.005, "ratio {ratio}");

	// Explicit params win over the stored state.
	let mut t = synth_track("s", NOTES, 4.0);
	t.channel.inserts.push(gain.with_param("0", 0.0));
	let overridden = render_offline(&project(vec![t]), &with_plugins(4.0)).unwrap();
	assert!(max_diff(&overridden, &dry) < 1e-6);
}

#[test]
fn missing_plugins_are_bypassed_with_warnings() {
	let dry = render_offline(&project(vec![synth_track("s", NOTES, 4.0)]), &opts(4.0)).unwrap();

	let mut missing_fx = plugin_gain(Some(-12.0));
	missing_fx.kind = DeviceKind::Clap {
		plugin_id: "com.example.not-installed".into(),
		path: "/nowhere/missing.clap".into(),
	};
	let mut t = synth_track("s", NOTES, 4.0);
	t.channel.inserts.push(missing_fx);
	let mut synth = Track::new("ghost", TrackKind::Instrument);
	synth.instrument = Some(Device {
		kind: DeviceKind::Vst3 {
			class_id: "0123456789ABCDEF0123456789ABCDEF".into(),
			path: "/nowhere/missing.vst3".into(),
		},
		..Device::builtin("unused", "Ghost Synth")
	});
	let p = project(vec![t, synth]);

	// Effects pass audio through; a missing instrument is silent; nothing fails.
	let out = render_offline(&p, &with_plugins(4.0)).unwrap();
	assert!(max_diff(&out, &dry) < 1e-6);
	let report = analyze_mix(&p, &with_plugins(4.0)).unwrap();
	assert_eq!(report.warnings.len(), 2, "{:?}", report.warnings);
	assert!(report.warnings.iter().all(|w| w.contains("bypassed")));
	assert!(
		report.warnings[0].contains("scan_plugins"),
		"{:?}",
		report.warnings
	);

	// Without a catalog plugin devices are bypassed too, with a clear reason.
	let mut t = synth_track("s", NOTES, 4.0);
	t.channel.inserts.push(plugin_gain(Some(-12.0)));
	let report = analyze_mix(&project(vec![t]), &opts(4.0)).unwrap();
	assert!(
		report
			.warnings
			.iter()
			.any(|w| w.contains("no plugin catalog")),
		"{:?}",
		report.warnings
	);
}

#[test]
fn seeking_silences_plugin_instrument_voices() {
	let t = instrument_track(
		"sine",
		plugin_device(SINE_ID, "Test Sine"),
		&[(69, 0.0, 4.0)],
		8.0,
	);
	let p = project(vec![t]);
	let mut graph = compile_hosted(&p, 48_000, 512, Some(&catalog()), &sandboxed()).unwrap();
	let mut l = vec![0.0f32; 4_800];
	let mut r = vec![0.0f32; 4_800];
	graph.play();
	graph.process(&mut l, &mut r);
	assert!(rms(&l[2_400..]) > 0.05, "note sounds before the seek");
	// Past the note: the plugin is reset (no wildcard note-off is sent) and stays silent.
	graph.seek(beats(6.0));
	graph.process(&mut l, &mut r);
	assert!(
		peak(&l[960..]) < 1e-4,
		"voice killed by reset, peak {}",
		peak(&l[960..])
	);
	// Seeking back into the note chases it in again.
	graph.seek(beats(1.0));
	graph.process(&mut l, &mut r);
	assert!(
		rms(&l[2_400..]) > 0.05,
		"chased note sounds after seeking back"
	);
}
