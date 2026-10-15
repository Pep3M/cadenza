//! Sidechains (built-in compressor and a CLAP effect with a sidechain port, with delay
//! compensation), multi-output instruments routed to buses, and plugin state/preset
//! capture — in process and sandboxed.

mod common;
mod plugin_support;

use cadenza_core::Tick;
use cadenza_engine::{
	PluginHosting, RenderOptions, analyze_mix, capture_state, compile_hosted, plugin_presets,
	preset_state, render_offline,
};
use cadenza_model::{Clip, Device, Note, OutputRoute, Project, Track, TrackKind};
use common::*;
use plugin_support::{DUCKER_ID, GAIN_ID, MULTI_ID, routing_catalog, routing_device, sandboxed};

const SR: usize = 48_000;
/// Samples per beat at 120 bpm.
const BEAT: usize = SR / 2;

fn hosting(sandbox: bool) -> PluginHosting {
	if sandbox {
		sandboxed()
	} else {
		PluginHosting::in_process()
	}
}

fn with_routing(end_beats: f64, sandbox: bool) -> RenderOptions {
	RenderOptions {
		plugins: Some(routing_catalog()),
		hosting: hosting(sandbox),
		..opts(end_beats)
	}
}

/// Instrument track playing `(pitch, start beat, length beats)` at full velocity.
fn track_with(name: &str, device: Device, notes: &[(u8, f64, f64)]) -> Track {
	let mut t = Track::new(name, TrackKind::Instrument).with_instrument(device);
	t.clips.push(
		Clip::midi(name, Tick::ZERO, beats(8.0)).with_notes(
			notes
				.iter()
				.map(|&(p, s, l)| Note::new(p, beats(s), beats(l), 1.0)),
		),
	);
	t
}

fn multi(name: &str, notes: &[(u8, f64, f64)]) -> Track {
	track_with(name, routing_device(MULTI_ID, "Multi"), notes)
}

fn latency_gain() -> Device {
	routing_device(GAIN_ID, "Latency").with_param("0", 0.0)
}

fn assert_clean(p: &Project, opts: &RenderOptions) {
	let graph = compile_hosted(p, 48_000, 512, opts.plugins.as_deref(), &opts.hosting).unwrap();
	assert!(graph.warnings().is_empty(), "{:?}", graph.warnings());
}

/// Track A plays a 0.5 DC from beat 1 and runs the ducker (out = main + key) keyed by the
/// muted track K, which plays the same DC; `latent_key` puts a 64-sample plugin on K,
/// otherwise on A before the ducker. Either way the output must step from 0 to exactly
/// 1.0 at beat 1: key and main are aligned by delay compensation.
fn check_aligned_key(latent_key: bool, sandbox: bool) {
	let mut key = multi("K", &[(60, 1.0, 2.0)]);
	key.channel.mute = true;
	let mut a = multi("A", &[(60, 1.0, 2.0)]);
	if latent_key {
		key.channel.inserts.push(latency_gain());
	} else {
		a.channel.inserts.push(latency_gain());
	}
	a.channel
		.inserts
		.push(routing_device(DUCKER_ID, "Ducker").with_sidechain(key.id, true));
	let p = project(vec![a, key]);
	let opts = with_routing(4.0, sandbox);
	assert_clean(&p, &opts);
	let out = render_offline(&p, &opts).unwrap();
	assert_eq!(out.left[BEAT - 1], 0.0);
	for i in [BEAT, BEAT + 1, BEAT + 63, BEAT + 64, BEAT + 500] {
		assert!(
			(out.left[i] - 1.0).abs() < 1e-6,
			"sample {i}: {}",
			out.left[i]
		);
	}

	// Post-fader keys follow the source's mute: the key is gone.
	let mut p = p;
	p.tracks[0].channel.inserts.last_mut().unwrap().sidechain =
		Some(cadenza_model::SidechainSource {
			track: p.tracks[1].id,
			pre_fader: false,
		});
	let out = render_offline(&p, &opts).unwrap();
	assert!(
		(out.left[BEAT + 500] - 0.5).abs() < 1e-6,
		"{}",
		out.left[BEAT + 500]
	);
}

#[test]
fn late_key_is_aligned_in_process() {
	check_aligned_key(true, false);
}

#[test]
fn late_key_is_aligned_sandboxed() {
	check_aligned_key(true, true);
}

#[test]
fn late_main_signal_is_aligned_in_process() {
	check_aligned_key(false, false);
}

#[test]
fn late_main_signal_is_aligned_sandboxed() {
	check_aligned_key(false, true);
}

#[test]
fn keys_from_buses_and_into_the_master_follow_processing_order() {
	// The key source is a bus fed by K; the keyed ducker sits on the master. The bus is
	// processed after K and before the master, so the ducker hears K's DC as its key and
	// the bus's own output: 0.5 (bus) + 0.5 (key) = 1.0.
	let sum = bus("Sum");
	let mut k = multi("K", &[(60, 1.0, 2.0)]);
	k.channel.output = cadenza_model::Output::Bus(sum.id);
	let mut p = project(vec![k, sum.clone()]);
	p.master
		.inserts
		.push(routing_device(DUCKER_ID, "Master Duck").with_sidechain(sum.id, false));
	let opts = with_routing(4.0, false);
	assert_clean(&p, &opts);
	let out = render_offline(&p, &opts).unwrap();
	assert!(
		(out.left[BEAT + 100] - 1.0).abs() < 1e-6,
		"{}",
		out.left[BEAT + 100]
	);

	// A bus keying a device on a track that is processed later (an instrument track
	// listed after it) still works: the processing order follows the key.
	let mut a = multi("A", &[(60, 1.0, 2.0)]);
	a.channel
		.inserts
		.push(routing_device(DUCKER_ID, "Duck").with_sidechain(sum.id, false));
	let mut p = project(vec![p.tracks[0].clone(), sum.clone(), a]);
	p.tracks[0].channel.mute = false;
	let out = render_offline(&p, &opts).unwrap();
	// K via Sum (0.5) + A (0.5 + key 0.5).
	assert!(
		(out.left[BEAT + 100] - 1.5).abs() < 1e-6,
		"{}",
		out.left[BEAT + 100]
	);
}

#[test]
fn builtin_compressor_ducks_from_another_track() {
	let kicks: Vec<(u8, f64, f64)> = (0..8).map(|b| (36, b as f64, 0.25)).collect();
	let mut kick = track_with(
		"Kick",
		Device::builtin("drum_machine", "Kit").with_param("gain", 12.0),
		&kicks,
	);
	let pad_notes = [(48, 0.0, 8.0), (55, 0.0, 8.0), (64, 0.0, 8.0)];
	let stats = |p: &Project| {
		let report = analyze_mix(p, &opts(8.0)).unwrap();
		assert!(report.warnings.is_empty(), "{:?}", report.warnings);
		report
			.tracks
			.iter()
			.find(|t| t.name == "Pad")
			.unwrap()
			.stats
	};
	// The threshold sits above the pad's own peaks, so only the (hot) kick compresses it.
	let dry = stats(&project(vec![synth_track("Pad", &pad_notes, 8.0)]));
	let pad = |key: Option<(&Track, bool)>| {
		let mut t = synth_track("Pad", &pad_notes, 8.0);
		let mut comp = Device::builtin("compressor", "Duck")
			.with_param("threshold", (dry.peak_db + 1.0).min(0.0))
			.with_param("ratio", 20.0)
			.with_param("knee", 0.0)
			.with_param("attack", 0.1)
			.with_param("release", 80.0);
		if let Some((k, pre)) = key {
			comp = comp.with_sidechain(k.id, pre);
		}
		t.channel.inserts.push(comp);
		t
	};
	let unkeyed = stats(&project(vec![kick.clone(), pad(None)])).rms_db;
	assert!(
		(unkeyed - dry.rms_db).abs() < 0.01,
		"{unkeyed} vs {}",
		dry.rms_db
	);
	let keyed = stats(&project(vec![kick.clone(), pad(Some((&kick, false)))])).rms_db;
	assert!(
		keyed < unkeyed - 3.0,
		"keyed {keyed} dB vs unkeyed {unkeyed} dB"
	);

	kick.channel.mute = true;
	let ghost = stats(&project(vec![kick.clone(), pad(Some((&kick, true)))])).rms_db;
	assert!(
		(ghost - keyed).abs() < 0.01,
		"a muted kick still keys pre-fader: {ghost} vs {keyed}"
	);
	let silent_key = stats(&project(vec![kick.clone(), pad(Some((&kick, false)))])).rms_db;
	assert!(
		(silent_key - unkeyed).abs() < 0.01,
		"a muted kick does not key post-fader: {silent_key} vs {unkeyed}"
	);
}

#[test]
fn unusable_sidechains_are_ignored_with_warnings() {
	let kick = synth_track("Kick", &[(36, 0.0, 1.0)], 2.0);
	let mut t = synth_track("Pad", &[(60, 0.0, 1.0)], 2.0);
	t.channel
		.inserts
		.push(Device::builtin("eq", "EQ").with_sidechain(kick.id, false));
	let p = project(vec![kick, t]);
	let graph = cadenza_engine::compile(&p, 48_000, 512).unwrap();
	assert_eq!(graph.warnings().len(), 1, "{:?}", graph.warnings());
	assert!(graph.warnings()[0].contains("no sidechain input"));
}

/// Kick (36) at beat 1 and snare (38) at beat 2 on the extra outputs, a main note (60) at
/// beat 3; each holds a 0.5 DC for half a beat.
fn drum_notes() -> Vec<(u8, f64, f64)> {
	vec![(36, 1.0, 0.5), (38, 2.0, 0.5), (60, 3.0, 0.5)]
}

fn check_multi_output(sandbox: bool) {
	let opts = with_routing(4.0, sandbox);
	let at = |out: &cadenza_engine::StereoBuffer, beat: f64| {
		let i = (beat * BEAT as f64) as usize;
		(out.left[i], out.right[i])
	};

	// Default: main output only.
	let p = project(vec![multi("Drums", &drum_notes())]);
	assert_clean(&p, &opts);
	let out = render_offline(&p, &opts).unwrap();
	assert_eq!(at(&out, 1.25), (0.0, 0.0));
	assert_eq!(at(&out, 2.25), (0.0, 0.0));
	assert_eq!(at(&out, 3.25), (0.5, 0.5));

	// Kick to its own bus (at -6 dB), snare mixed into the track; a 64-sample plugin on
	// the drum track delays its main path, and the bus path is compensated to match.
	let mut kick_bus = bus("Kick Bus");
	kick_bus.channel.volume_db = -6.0;
	let mut drums = multi("Drums", &drum_notes());
	drums.channel.inserts.push(latency_gain());
	drums.instrument.as_mut().unwrap().aux_outputs = vec![
		OutputRoute {
			port: 1,
			bus: Some(kick_bus.id),
		},
		OutputRoute { port: 2, bus: None },
	];
	let p = project(vec![drums, kick_bus]);
	assert_clean(&p, &opts);
	let out = render_offline(&p, &opts).unwrap();
	let kick = 0.5 * 10f32.powf(-6.0 / 20.0);
	let (l, r) = at(&out, 1.25);
	assert!(
		(l - kick).abs() < 1e-6 && (r - kick).abs() < 1e-6,
		"{l} {r}"
	);
	assert_eq!(at(&out, 2.25), (0.5, 0.5), "mono snare in both channels");
	assert_eq!(at(&out, 3.25), (0.5, 0.5));
	// Sample-exact onsets on both paths despite the 64-sample plugin.
	assert_eq!(out.left[BEAT - 1], 0.0);
	assert!((out.left[BEAT] - kick).abs() < 1e-6);
	assert_eq!(out.left[3 * BEAT - 1], 0.0);
	assert_eq!(out.left[3 * BEAT], 0.5);

	let report = analyze_mix(&p, &opts).unwrap();
	let bus_peak = report
		.tracks
		.iter()
		.find(|t| t.name == "Kick Bus")
		.unwrap()
		.stats
		.peak_db;
	assert!((bus_peak - 20.0 * kick.log10()).abs() < 0.01, "{bus_peak}");
}

#[test]
fn extra_outputs_route_to_buses_in_process() {
	check_multi_output(false);
}

#[test]
fn extra_outputs_route_to_buses_sandboxed() {
	check_multi_output(true);
}

#[test]
fn unavailable_extra_outputs_are_reported() {
	let mut drums = multi("Drums", &drum_notes());
	drums.instrument.as_mut().unwrap().aux_outputs = vec![OutputRoute { port: 5, bus: None }];
	let mut synth = synth_track("Synth", &[(60, 0.0, 1.0)], 2.0);
	synth.instrument.as_mut().unwrap().aux_outputs = vec![OutputRoute { port: 1, bus: None }];
	let p = project(vec![drums, synth]);
	let opts = with_routing(2.0, false);
	let graph = compile_hosted(&p, 48_000, 512, opts.plugins.as_deref(), &opts.hosting).unwrap();
	let w = graph.warnings();
	assert_eq!(w.len(), 2, "{w:?}");
	assert!(w[0].contains("2 extra output(s); output 5"), "{w:?}");
	assert!(w[1].contains("0 extra output(s); output 1"), "{w:?}");
}

/// Ducker on a silent track keyed by K (0.5 DC): the output is the key at `key_level`.
fn ducked_level(ducker: Device, sandbox: bool) -> f32 {
	let mut key = multi("K", &[(60, 0.0, 2.0)]);
	key.channel.mute = true;
	let mut t = Track::new("A", TrackKind::Audio);
	t.channel.inserts.push(ducker.with_sidechain(key.id, true));
	let p = project(vec![t, key]);
	let out = render_offline(&p, &with_routing(1.0, sandbox)).unwrap();
	out.left[BEAT / 2]
}

fn check_state_capture(sandbox: bool) {
	let catalog = routing_catalog();
	let h = hosting(sandbox);
	let ducker = routing_device(DUCKER_ID, "Ducker").with_param("0", -6.0);
	let state = capture_state(&ducker, Some(&catalog), &h)
		.unwrap()
		.expect("the ducker saves state");
	// The captured state alone (no explicit params) reproduces the sound.
	let mut restored = routing_device(DUCKER_ID, "Ducker");
	restored.state = Some(state.clone());
	let expected = 0.5 * 10f32.powf(-6.0 / 20.0);
	assert!((ducked_level(restored.clone(), sandbox) - expected).abs() < 1e-5);
	assert!((ducked_level(ducker, sandbox) - expected).abs() < 1e-5);
	// Capturing again from the restored device is stable.
	assert_eq!(
		capture_state(&restored, Some(&catalog), &h).unwrap(),
		Some(state)
	);
	// Built-ins have no opaque state.
	assert_eq!(
		capture_state(&Device::builtin("compressor", "C"), Some(&catalog), &h).unwrap(),
		None
	);
}

#[test]
fn plugin_state_is_captured_in_process() {
	check_state_capture(false);
}

#[test]
fn plugin_state_is_captured_sandboxed() {
	check_state_capture(true);
}

fn check_presets(sandbox: bool) {
	let catalog = routing_catalog();
	let h = hosting(sandbox);
	let ducker = routing_device(DUCKER_ID, "Ducker").with_param("0", 6.0);
	let presets = plugin_presets(&ducker, Some(&catalog), &h).unwrap();
	let quiet = presets.iter().find(|p| p.name == "Quiet Key").unwrap();
	let state = preset_state(&ducker, &quiet.key, Some(&catalog), &h).unwrap();
	let mut loaded = routing_device(DUCKER_ID, "Ducker");
	loaded.state = Some(state);
	assert!((ducked_level(loaded, sandbox) - 0.05).abs() < 1e-5);
	let err = preset_state(&ducker, "plugin\u{1f}nope", Some(&catalog), &h).unwrap_err();
	assert!(err.to_string().contains("refused the preset"), "{err}");
	assert!(
		plugin_presets(&ducker, None, &h)
			.unwrap_err()
			.to_string()
			.contains("scan_plugins")
	);
}

#[test]
fn factory_presets_load_in_process() {
	check_presets(false);
}

#[test]
fn factory_presets_load_sandboxed() {
	check_presets(true);
}
