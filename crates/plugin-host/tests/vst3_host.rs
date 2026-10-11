//! End-to-end tests of the VST3 host against the `vst3_gain` example of
//! `cadenza-test-plugins` (separate controller, latency, stepped and
//! read-only params, framed state).

mod common;

use std::time::Duration;

use cadenza_dsp::{DeviceRole, Transport};
use cadenza_plugin_host::scanner::scan_in_subprocess;
use cadenza_plugin_host::{PluginCatalog, PluginDescriptor, PluginFormat, PluginRole, instantiate};
use common::{VST3_GAIN_ID, scanner, vst3_bundle};

const SR: f32 = 48_000.0;
const BLOCK: usize = 256;

fn descriptor() -> PluginDescriptor {
	let descs = scan_in_subprocess(&scanner(), vst3_bundle(), Duration::from_secs(30))
		.expect("scan VST3 test bundle");
	assert_eq!(descs.len(), 1, "{descs:#?}");
	descs.into_iter().next().unwrap()
}

/// Feeds a constant 0.5 through `p` and returns the last output sample.
fn level(p: &mut dyn cadenza_dsp::Processor) -> f32 {
	let mut l = vec![0.5f32; BLOCK];
	let mut r = vec![0.5f32; BLOCK];
	p.process(&mut l, &mut r, &[], &Transport::default());
	assert_eq!(l, r);
	l[BLOCK - 1]
}

#[test]
fn scanner_describes_the_vst3_gain() {
	let d = descriptor();
	assert_eq!(d.format, PluginFormat::Vst3);
	assert_eq!(d.id, VST3_GAIN_ID);
	assert_eq!(d.name, "Cadenza Test VST3 Gain");
	assert_eq!(d.vendor, "Cadenza");
	assert_eq!(d.version, "1.0.0");
	assert_eq!(d.role, PluginRole::Effect);
	assert_eq!(d.features, ["audio-effect", "dynamics"]);
	assert_eq!((d.audio_inputs, d.audio_outputs), (2, 2));
	assert!(!d.note_input);
	assert_eq!(d.latency_samples, 32);
	assert_eq!(d.path, vst3_bundle());
	// The read-only "Meter" is not exposed.
	let names: Vec<&str> = d.params.iter().map(|p| p.name.as_str()).collect();
	assert_eq!(names, ["Gain", "Mute"]);
	let gain = &d.params[0];
	assert_eq!(gain.id, "0");
	assert_eq!((gain.min, gain.max, gain.default), (-60.0, 12.0, 0.0));
	assert!(gain.automatable && !gain.stepped && !gain.bypass);
	let mute = &d.params[1];
	assert!(mute.stepped);
	assert_eq!((mute.min, mute.max), (0.0, 1.0));
	assert!(
		d.description.contains("VST3 audio effect"),
		"{}",
		d.description
	);
}

#[test]
fn processes_sets_params_and_round_trips_state() {
	let d = descriptor();
	let mut p = instantiate(&d, SR, BLOCK, None).unwrap();
	assert_eq!(p.info().role, DeviceRole::Effect);
	assert_eq!(p.info().params.len(), 2);
	assert_eq!(p.latency(), 32);
	assert!((level(p.as_mut()) - 0.5).abs() < 1e-6);

	// -6.0206 dB halves the signal: plain → normalized on the audio thread.
	p.set_param(0, -6.0206);
	assert!((level(p.as_mut()) - 0.25).abs() < 1e-4);
	// Out-of-range values clamp to the plain range.
	p.set_param(0, 100.0);
	let max = level(p.as_mut());
	assert!((max - 0.5 * 10f32.powf(0.6)).abs() < 1e-3, "{max}");
	p.set_param(1, 0.8);
	assert_eq!(level(p.as_mut()), 0.0);
	p.set_param(1, 0.0);
	p.set_param(0, -20.0);
	// Pending changes are flushed into the state before saving.
	let state = p.save_state().unwrap();
	assert_eq!(&state[..6], b"CDZV3\x01");
	assert!(state.ends_with(b"CTRL"));
	assert!((level(p.as_mut()) - 0.05).abs() < 1e-4);
	drop(p);

	let mut restored = instantiate(&d, SR, BLOCK, Some(&state)).unwrap();
	assert!((level(restored.as_mut()) - 0.05).abs() < 1e-4);
	assert!(restored.load_state(&state));
	assert!(!restored.load_state(b"garbage"));
	assert!(instantiate(&d, SR, BLOCK, Some(b"CDZV3\x01")).is_err());

	// Re-activation at a new configuration keeps working.
	restored.prepare(44_100.0, 64);
	restored.reset();
	let mut l = vec![0.5f32; 64];
	let mut r = vec![0.5f32; 64];
	restored.process(&mut l, &mut r, &[], &Transport::default());
	assert!((l[63] - 0.05).abs() < 1e-4);
}

#[test]
fn instances_survive_cross_thread_create_and_drop() {
	let d = descriptor();
	let handles: Vec<_> = (0..4)
		.map(|_| {
			let d = d.clone();
			std::thread::spawn(move || {
				let mut p = instantiate(&d, SR, BLOCK, None).unwrap();
				level(p.as_mut());
				p
			})
		})
		.collect();
	let devices: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
	drop(devices);
	// The module is unloaded after the last instance and loads again cleanly.
	std::thread::sleep(Duration::from_millis(50));
	let mut p = instantiate(&d, SR, BLOCK, None).unwrap();
	assert!((level(p.as_mut()) - 0.5).abs() < 1e-6);
}

#[test]
fn catalog_finds_vst3_bundles() {
	let dir = tempfile::tempdir().unwrap();
	let cache = dir.path().join("plugins.json");
	let root = vst3_bundle().parent().unwrap().to_path_buf();
	let mut cat = PluginCatalog::load_or_default(&cache);
	let report = cat.scan(std::slice::from_ref(&root), &scanner()).unwrap();
	assert_eq!(report.added, [VST3_GAIN_ID]);
	assert!(report.blocked.is_empty());
	assert_eq!(cat.get(VST3_GAIN_ID).unwrap().format, PluginFormat::Vst3);
	// A bundle without a binary for this platform is blocklisted.
	let broken = root.join("Broken.vst3/Contents");
	std::fs::create_dir_all(&broken).unwrap();
	let report = cat.scan(std::slice::from_ref(&root), &scanner()).unwrap();
	assert_eq!(report.blocked.len(), 1);
	assert!(
		report.blocked[0].reason.contains("no binary"),
		"{}",
		report.blocked[0].reason
	);
	std::fs::remove_dir_all(root.join("Broken.vst3")).unwrap();
}
