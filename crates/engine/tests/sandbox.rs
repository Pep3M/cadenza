//! Sandboxed plugin hosting inside the engine: parity with in-process hosting, crash and
//! hang isolation during renders and realtime graphs, and the in-process fallback when the
//! sandbox executable is missing.

// Plugin sandboxing is Unix-only (`SANDBOX_SUPPORTED`).
#![cfg(unix)]

mod common;
mod plugin_support;

use std::time::Duration;

use cadenza_core::ParamAddress;
use cadenza_engine::{
	CrashReason, PluginHosting, RenderOptions, analyze_mix, compile_hosted, render_offline,
	render_to_wav,
};
use cadenza_model::{AutomationLane, AutomationPoint, Device, DeviceKind, Project};
use common::*;
use plugin_support::{CRASH_ID, GAIN_ID, SINE_ID, catalog, crash_catalog, plugin_path, sandboxed};

const NOTES: &[(u8, f64, f64)] = &[(60, 0.0, 1.0), (64, 1.25, 0.5), (67, 2.0, 2.0)];

fn clap(id: &str, name: &str, path: std::path::PathBuf) -> Device {
	let mut d = Device::builtin("unused", name);
	d.kind = DeviceKind::Clap {
		plugin_id: id.into(),
		path,
	};
	d
}

fn crash_fx() -> Device {
	let path = crash_catalog()
		.find(CRASH_ID)
		.expect("scanned")
		.path
		.clone();
	clap(CRASH_ID, "Crash FX", path)
}

/// Synth -> plugin gain, plus a plugin sine instrument, so both plugin roles are covered.
fn plugin_project() -> Project {
	let mut lead = synth_track("lead", NOTES, 4.0);
	lead.channel
		.inserts
		.push(clap(GAIN_ID, "Test Gain", plugin_path(GAIN_ID)).with_param("0", -6.0));
	let sine = instrument_track(
		"sine",
		clap(SINE_ID, "Test Sine", plugin_path(SINE_ID)),
		&[(69, 0.5, 1.0), (72, 2.0, 1.5)],
		4.0,
	);
	project(vec![lead, sine])
}

fn hosted(
	hosting: PluginHosting,
	plugins: std::sync::Arc<cadenza_engine::PluginCatalog>,
) -> RenderOptions {
	RenderOptions {
		plugins: Some(plugins),
		hosting,
		..opts(4.0)
	}
}

#[test]
fn sandboxed_render_is_bit_identical_to_in_process() {
	let p = plugin_project();
	let local = render_offline(&p, &hosted(PluginHosting::in_process(), catalog())).unwrap();
	let remote = render_offline(&p, &hosted(sandboxed(), catalog())).unwrap();
	assert!(peak(&local.left) > 0.05, "the reference render is silent");
	assert_eq!(max_diff(&local, &remote), 0.0);

	let report = analyze_mix(&p, &hosted(sandboxed(), catalog())).unwrap();
	assert!(report.warnings.is_empty(), "{:?}", report.warnings);
	assert_eq!(report.latency_samples, 64);
}

#[test]
fn a_crashing_plugin_is_bypassed_and_the_render_completes_with_a_warning() {
	let dry = render_offline(&project(vec![synth_track("lead", NOTES, 4.0)]), &opts(4.0)).unwrap();

	// crash_now (param "0") flips to 1 at beat 2 (1 s): before, the plugin halves the
	// signal (gain param "2" = 0.5); after, it is gone and the effect slot passes through.
	let fx = crash_fx();
	let mut lead = synth_track("lead", NOTES, 4.0);
	lead.channel.inserts.push(fx.clone());
	let mut p = project(vec![lead.clone()]);
	p.automation.push(AutomationLane::new(
		ParamAddress::device(Some(lead.id), fx.id, "0"),
		vec![
			AutomationPoint::new(beats(2.0), 0.0),
			AutomationPoint::new(beats(2.0) + cadenza_core::Tick(1), 1.0),
		],
	));

	let path = tmp_path("sandbox-crash.wav");
	let report = render_to_wav(&p, &hosted(sandboxed(), crash_catalog()), &path).unwrap();
	assert!((report.duration_seconds - 2.0).abs() < 1e-3);
	assert_eq!(report.warnings.len(), 1, "{:?}", report.warnings);
	let w = &report.warnings[0];
	for needle in [
		"track 'lead'",
		"Crash FX",
		CRASH_ID,
		"crashed (SIGABRT)",
		"passes audio through",
		"at about 1.0 s (beat 2.0)",
		"render completed",
	] {
		assert!(w.contains(needle), "missing {needle:?} in {w}");
	}

	let out = render_offline(&p, &hosted(sandboxed(), crash_catalog())).unwrap();
	// 120 bpm at 48 kHz: one beat is 24 000 samples.
	let before = 2_000..22_000;
	let after = 50_000..94_000;
	let ratio = rms(&out.left[before.clone()]) / rms(&dry.left[before]);
	assert!(
		(ratio - 0.5).abs() < 0.01,
		"processed before the crash: {ratio}"
	);
	let bypassed = &out.left[after.clone()];
	let reference = &dry.left[after];
	let diff = bypassed
		.iter()
		.zip(reference)
		.map(|(a, b)| (a - b).abs())
		.fold(0f32, f32::max);
	assert!(diff < 1e-6, "passes audio through after the crash ({diff})");
	let _ = std::fs::remove_file(path);
}

#[test]
fn a_hanging_plugin_times_out_and_the_render_completes() {
	let mut lead = synth_track("lead", NOTES, 4.0);
	lead.channel
		.inserts
		.push(crash_fx().with_param("1", 2000.0));
	let p = project(vec![lead]);
	let mut hosting = sandboxed();
	hosting.sandbox.offline_timeout = Duration::from_millis(200);
	hosting.sandbox.warmup_timeout = Duration::from_millis(200);
	let report = analyze_mix(&p, &hosted(hosting, crash_catalog())).unwrap();
	assert_eq!(report.warnings.len(), 1, "{:?}", report.warnings);
	assert!(
		report.warnings[0].contains("hung (no audio within 200 ms)"),
		"{:?}",
		report.warnings
	);
	assert!(report.master.peak_db > -40.0, "the dry signal still plays");
}

#[test]
fn late_plugin_blocks_are_xruns_not_crashes() {
	// Every 300th plugin call stalls 20 ms: far past the 2 ms deadline, far below the
	// hang timeout.
	let fx = crash_fx().with_param("1", 20.0).with_param("3", 300.0);
	let mut lead = synth_track("lead", NOTES, 4.0);
	lead.channel.inserts.push(fx.clone());
	let p = project(vec![lead.clone()]);
	let mut hosting = sandboxed();
	hosting.sandbox.realtime_timeout = Duration::from_millis(2);
	let mut graph = compile_hosted(&p, 48_000, 512, Some(&crash_catalog()), &hosting).unwrap();
	graph.play();
	let mut l = vec![0.0f32; 512];
	let mut r = vec![0.0f32; 512];
	// Paced like an audio callback, so the stalled plugin catches up in between.
	for _ in 0..200 {
		graph.process(&mut l, &mut r);
		std::thread::sleep(Duration::from_millis(3));
	}
	assert!(
		graph.plugin_crashes().is_empty(),
		"{:?}",
		graph.plugin_crashes()
	);
	let xruns = graph.plugin_xruns();
	assert_eq!(xruns.len(), 1, "{xruns:?}");
	assert_eq!(xruns[0].device_id, fx.id);
	assert_eq!(xruns[0].track_id, Some(lead.id));
	assert!(xruns[0].stats.late_blocks >= 2, "{xruns:?}");
	assert!(xruns[0].stats.dropped_frames > 0, "{xruns:?}");
}

#[test]
fn realtime_graphs_expose_crashes_off_the_audio_thread() {
	let fx = crash_fx().with_param("0", 1.0);
	let mut lead = synth_track("lead", NOTES, 4.0);
	lead.channel.inserts.push(fx.clone());
	let p = project(vec![lead.clone()]);
	let mut graph = compile_hosted(&p, 48_000, 512, Some(&crash_catalog()), &sandboxed()).unwrap();
	assert!(graph.warnings().is_empty(), "{:?}", graph.warnings());
	assert!(graph.plugin_crashes().is_empty());
	graph.play();
	let mut l = vec![0.0f32; 4_800];
	let mut r = vec![0.0f32; 4_800];
	graph.process(&mut l, &mut r);
	assert!(rms(&l[1_000..]) > 0.01, "the dry signal keeps playing");
	let crashes = graph.plugin_crashes();
	assert_eq!(crashes.len(), 1);
	let crash = &crashes[0];
	assert_eq!(crash.track_id, Some(lead.id));
	assert_eq!(crash.device_id, fx.id);
	assert_eq!(crash.device_name, "Crash FX");
	assert_eq!(crash.plugin_id, CRASH_ID);
	assert!(matches!(&crash.reason, CrashReason::Signal { name, .. } if name == "SIGABRT"));
	assert!(
		crash.message.starts_with("track 'lead': CLAP plugin"),
		"{}",
		crash.message
	);

	// In-process hosting has nothing to watch.
	let graph = compile_hosted(
		&project(vec![synth_track("lead", NOTES, 4.0)]),
		48_000,
		512,
		Some(&crash_catalog()),
		&PluginHosting::in_process(),
	)
	.unwrap();
	assert!(graph.plugin_crashes().is_empty());
}

#[test]
fn a_missing_sandbox_falls_back_to_in_process_with_an_actionable_warning() {
	let p = plugin_project();
	let mut hosting = PluginHosting::sandboxed();
	hosting.sandbox.executable = Some("/nonexistent/cadenza-plugin-sandbox".into());
	let report = analyze_mix(&p, &hosted(hosting.clone(), catalog())).unwrap();
	assert_eq!(
		report.warnings.len(),
		1,
		"warned once: {:?}",
		report.warnings
	);
	let w = &report.warnings[0];
	for needle in [
		"/nonexistent/cadenza-plugin-sandbox",
		"run inside Cadenza",
		"cargo build --release",
		"CADENZA_PLUGIN_SANDBOX",
		"CADENZA_PLUGIN_HOSTING=in-process",
	] {
		assert!(w.contains(needle), "missing {needle:?} in {w}");
	}
	// The plugins still play, in-process.
	let fallback = render_offline(&p, &hosted(hosting, catalog())).unwrap();
	let local = render_offline(&p, &hosted(PluginHosting::in_process(), catalog())).unwrap();
	assert_eq!(max_diff(&fallback, &local), 0.0);

	// Plugin-free projects never look for the sandbox.
	let mut hosting = PluginHosting::sandboxed();
	hosting.sandbox.executable = Some("/nonexistent/cadenza-plugin-sandbox".into());
	let report = analyze_mix(
		&project(vec![synth_track("lead", NOTES, 4.0)]),
		&hosted(hosting, catalog()),
	)
	.unwrap();
	assert!(report.warnings.is_empty(), "{:?}", report.warnings);
}

#[test]
fn hosting_mode_follows_the_environment_by_default() {
	// Only reads the variable; whichever value the test environment has must map cleanly.
	let expected = match std::env::var(cadenza_engine::HOSTING_ENV).as_deref() {
		Ok("in-process") => cadenza_engine::HostingMode::InProcess,
		_ => cadenza_engine::HostingMode::Sandboxed,
	};
	assert_eq!(PluginHosting::default().mode, expected);
	assert_eq!(RenderOptions::default().hosting.mode, expected);
}

#[test]
fn the_player_reports_crashes_once_and_restarts_plugins_on_load() {
	let mut player = match cadenza_engine::Player::new() {
		Ok(p) => p,
		Err(e) => {
			eprintln!("skipping player crash test: {e}");
			return;
		}
	};
	player.set_plugins(Some(crash_catalog()));
	player.set_hosting(sandboxed());
	// No notes: the effect processes silence, so the test makes no noise.
	let fx = crash_fx();
	let mut lead = synth_track("lead", &[], 4.0);
	lead.channel.inserts.push(fx.clone().with_param("0", 1.0));
	let warnings = player.load(&project(vec![lead.clone()])).unwrap();
	assert!(warnings.is_empty(), "{warnings:?}");
	player.play().unwrap();
	let deadline = std::time::Instant::now() + Duration::from_secs(5);
	let crashes = loop {
		let crashes = player.take_plugin_crashes();
		if !crashes.is_empty() || std::time::Instant::now() > deadline {
			break crashes;
		}
		std::thread::sleep(Duration::from_millis(20));
	};
	assert_eq!(crashes.len(), 1, "crash noticed off the audio thread");
	assert_eq!(crashes[0].device_id, fx.id);
	assert!(
		crashes[0].message.contains("Playback continues"),
		"{}",
		crashes[0].message
	);
	assert!(player.take_plugin_crashes().is_empty(), "taken only once");
	assert_eq!(player.plugin_crashes().len(), 1, "still crashed");

	// Reloading re-creates the plugin: healthy again.
	let mut healthy = lead;
	healthy.channel.inserts[0] = fx;
	player.load(&project(vec![healthy])).unwrap();
	std::thread::sleep(Duration::from_millis(300));
	assert!(player.plugin_crashes().is_empty());
	player.stop().unwrap();
}
