//! The sample-based built-ins in the graph: factory content, sample loading at compile
//! time, missing files, aux outputs routed to buses and persistence.

mod common;

use cadenza_engine::factory;
use cadenza_engine::{analyze_mix, render_offline};
use cadenza_model::{Device, OutputRoute, Project, SampleZone};
use common::*;

/// Factory samples rendered once per test binary into the target folder.
fn factory_dir() -> std::path::PathBuf {
	static DIR: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
	DIR.get_or_init(|| {
		let dir = tmp_path("factory-samples");
		// SAFETY: set once, before any thread of this binary reads it (OnceLock).
		unsafe { std::env::set_var(factory::FACTORY_SAMPLES_ENV, &dir) };
		factory::ensure_factory_samples().unwrap()
	})
	.clone()
}

fn kit() -> Device {
	factory_dir();
	let content = factory::default_content("drum_sampler").unwrap().unwrap();
	let mut d = Device::builtin("drum_sampler", "Drums").with_zones(content.zones);
	d.params = content.params;
	d
}

fn beat_project(device: Device) -> Project {
	let notes: Vec<(u8, f64, f64)> = (0..8)
		.flat_map(|i| {
			let b = i as f64 * 0.5;
			[
				(if i % 4 == 2 { 38 } else { 36 }, b, 0.25),
				(42, b + 0.25, 0.25),
			]
		})
		.collect();
	project(vec![instrument_track("Drums", device, &notes, 4.0)])
}

#[test]
fn drum_sampler_plays_the_factory_kit() {
	let p = beat_project(kit());
	let out = render_offline(&p, &opts(4.0)).unwrap();
	assert_finite(&out);
	assert!(peak(&out.left) > 0.2, "kit is audible: {}", peak(&out.left));
	let report = analyze_mix(&p, &opts(4.0)).unwrap();
	assert!(report.warnings.is_empty(), "{:?}", report.warnings);
}

#[test]
fn sampler_plays_the_factory_multisample() {
	factory_dir();
	let content = factory::default_content("sampler").unwrap().unwrap();
	assert_eq!(content.zones.len(), 4);
	let mut d = Device::builtin("sampler", "Pad").with_zones(content.zones);
	d.params = content.params;
	let p = project(vec![instrument_track(
		"Pad",
		d,
		&[(48, 0.0, 4.0), (55, 0.0, 4.0), (64, 0.0, 4.0)],
		4.0,
	)]);
	let out = render_offline(&p, &opts(4.0)).unwrap();
	// Sustains through the loop (the samples are 3 s, the chord 2 s at 120 bpm + release).
	let late = &out.left[(1.8 * 48_000.0) as usize..(2.0 * 48_000.0) as usize];
	assert!(rms(late) > 0.05, "{}", rms(late));
}

#[test]
fn missing_samples_warn_and_stay_silent() {
	let mut device = kit();
	device.zones = vec![
		SampleZone::pad("/definitely/missing/kick.wav", 36),
		SampleZone::pad(factory_dir().join("Closed Hat.wav"), 42),
	];
	let p = beat_project(device);
	let report = analyze_mix(&p, &opts(4.0)).unwrap();
	assert_eq!(report.warnings.len(), 1, "{:?}", report.warnings);
	let w = &report.warnings[0];
	assert!(
		w.contains("kick.wav") && w.contains("zone is silent") && w.contains("Drums"),
		"{w}"
	);
	// The hat still plays.
	assert!(report.master.peak_db > -40.0);
}

#[test]
fn pads_on_extra_outputs_reach_their_bus() {
	let mut device = kit();
	for z in device.zones.iter_mut().filter(|z| z.key_low == 38) {
		z.output = 2;
	}
	let mut p = beat_project(device);
	let snare_bus = bus("Snare Bus");
	let bus_id = snare_bus.id;
	p.tracks.push(snare_bus);
	let level = |p: &Project, id| {
		analyze_mix(p, &opts(4.0))
			.unwrap()
			.tracks
			.into_iter()
			.find(|t| t.track_id == id)
			.unwrap()
			.stats
			.peak_db
	};
	// Unrouted, output 2 is silent; routed, the snare lands on the bus.
	assert!(level(&p, bus_id) < -100.0);
	p.tracks[0].instrument.as_mut().unwrap().aux_outputs = vec![OutputRoute {
		port: 2,
		bus: Some(bus_id),
	}];
	assert!(level(&p, bus_id) > -30.0);
}

#[test]
fn projects_with_samples_round_trip() {
	let p = beat_project(kit());
	let path = tmp_path("samplers-roundtrip.cadenza.json");
	p.save_json(&path).unwrap();
	let loaded = Project::load_json(&path).unwrap();
	assert_eq!(loaded, p);
	let a = render_offline(&p, &opts(2.0)).unwrap();
	let b = render_offline(&loaded, &opts(2.0)).unwrap();
	assert_eq!(max_diff(&a, &b), 0.0);
}
