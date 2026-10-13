//! End-to-end tests against the `cadenza-test-plugins` CLAP binary: scanning,
//! catalog caching/blocklisting and real-time processing.

mod common;

use std::time::Duration;

use cadenza_dsp::{DeviceRole, MidiEvent, Transport};
use cadenza_plugin_host::scanner::{ScanFailure, scan_in_subprocess};
use cadenza_plugin_host::{PluginCatalog, PluginError, PluginFormat, PluginRole, instantiate};
use common::{GAIN_ID, SINE_ID, descriptor, descriptors, install_into, scanner, test_plugin};

const SR: f32 = 48_000.0;
const BLOCK: usize = 256;

fn transport() -> Transport {
	Transport {
		bpm: 120.0,
		playing: true,
		beat_position: 0.0,
		..Default::default()
	}
}

#[test]
fn scanner_describes_both_test_plugins() {
	let descs = descriptors();
	assert_eq!(descs.len(), 2, "{descs:#?}");

	let gain = descriptor(GAIN_ID);
	assert_eq!(gain.format, PluginFormat::Clap);
	assert_eq!(gain.name, "Cadenza Test Gain");
	assert_eq!(gain.vendor, "Cadenza");
	assert_eq!(gain.role, PluginRole::Effect);
	assert_eq!((gain.audio_inputs, gain.audio_outputs), (2, 2));
	assert!(!gain.note_input);
	assert_eq!(gain.latency_samples, 64);
	assert_eq!(gain.params.len(), 1);
	let p = &gain.params[0];
	assert_eq!((p.id.as_str(), p.name.as_str()), ("0", "Gain"));
	assert_eq!((p.min, p.max, p.default), (-60.0, 12.0, 0.0));
	assert!(p.automatable && !p.stepped);
	assert!(gain.features.contains(&"audio-effect".to_string()));
	assert!(
		gain.description.contains("audio effect"),
		"{}",
		gain.description
	);

	let sine = descriptor(SINE_ID);
	assert_eq!(sine.role, PluginRole::Instrument);
	assert_eq!((sine.audio_inputs, sine.audio_outputs), (0, 2));
	assert!(sine.note_input);
	assert_eq!(sine.latency_samples, 0);
	assert_eq!(sine.params[0].name, "Level");
	assert_eq!(sine.params[0].default, -6.0);
	assert_eq!(sine.path, test_plugin());
}

#[test]
fn scanner_rejects_a_corrupt_file() {
	let dir = tempfile::tempdir().unwrap();
	let bad = dir.path().join("bad.clap");
	std::fs::write(&bad, b"this is not a shared library").unwrap();
	match scan_in_subprocess(&scanner(), &bad, Duration::from_secs(20)) {
		Err(ScanFailure::Plugin(reason)) => {
			// The end of the loader error (macOS dyld lists every path it tried,
			// so the start may be cut off).
			assert!(
				reason.contains("check it is a valid CLAP plugin"),
				"{reason}"
			)
		}
		other => panic!("expected plugin failure, got {other:?}"),
	}
}

#[test]
fn catalog_scans_incrementally_and_persists() {
	let plugins = tempfile::tempdir().unwrap();
	let cache = tempfile::tempdir().unwrap();
	let cache_path = cache.path().join("plugins.json");
	let file = install_into(plugins.path());
	let roots = vec![plugins.path().to_path_buf()];

	let mut cat = PluginCatalog::load_or_default(&cache_path);
	let report = cat.scan(&roots, &scanner()).unwrap();
	let mut added = report.added.clone();
	added.sort();
	assert_eq!(added, vec![GAIN_ID.to_string(), SINE_ID.to_string()]);
	assert_eq!((report.files_found, report.files_scanned), (1, 1));
	assert!(report.blocked.is_empty() && report.removed.is_empty());
	assert_eq!(cat.find(GAIN_ID).unwrap().path, file);
	assert_eq!(cat.find_at(SINE_ID, &file).unwrap().id, SINE_ID);
	assert!(matches!(cat.get("nope"), Err(PluginError::NotFound(_))));

	// Unchanged files are not loaded again.
	let again = cat.scan(&roots, &scanner()).unwrap();
	assert_eq!(again.files_scanned, 0);
	assert!(again.added.is_empty() && again.updated.is_empty() && again.removed.is_empty());

	// The cache survives a reload and still avoids rescans.
	cat.save().unwrap();
	let mut reloaded = PluginCatalog::load_or_default(&cache_path);
	assert_eq!(reloaded.descriptors().count(), 2);
	assert_eq!(reloaded.scan(&roots, &scanner()).unwrap().files_scanned, 0);

	// A modified file is rescanned.
	let f = std::fs::File::options().write(true).open(&file).unwrap();
	f.set_modified(std::time::SystemTime::now() + Duration::from_secs(5))
		.unwrap();
	drop(f);
	let changed = reloaded.scan(&roots, &scanner()).unwrap();
	assert_eq!(changed.files_scanned, 1);
	assert_eq!(changed.updated.len(), 2);

	// A deleted file drops its plugins.
	std::fs::remove_file(&file).unwrap();
	let gone = reloaded.scan(&roots, &scanner()).unwrap();
	assert_eq!(gone.removed.len(), 2);
	assert_eq!(reloaded.descriptors().count(), 0);
}

#[test]
fn catalog_blocklists_corrupt_files() {
	let plugins = tempfile::tempdir().unwrap();
	let cache = tempfile::tempdir().unwrap();
	let bad = plugins.path().join("broken.clap");
	std::fs::write(&bad, b"\x7fELF definitely not a plugin").unwrap();
	install_into(plugins.path());
	let roots = vec![plugins.path().to_path_buf()];

	let mut cat = PluginCatalog::load_or_default(cache.path().join("p.json"));
	let report = cat.scan(&roots, &scanner()).unwrap();
	assert_eq!(report.added.len(), 2, "good plugins still load");
	assert_eq!(report.blocked.len(), 1);
	assert_eq!(report.blocked[0].path, bad);
	assert!(!report.blocked[0].reason.is_empty());
	assert_eq!(cat.blocked().count(), 1);

	// Blocked files are not retried until they change or are unblocked.
	assert_eq!(cat.scan(&roots, &scanner()).unwrap().files_scanned, 0);
	cat.save().unwrap();
	let mut cat = PluginCatalog::load_or_default(cache.path().join("p.json"));
	assert_eq!(cat.blocked().count(), 1);
	assert!(cat.unblock(&bad));
	assert!(!cat.unblock(&bad));
	let retry = cat.scan(&roots, &scanner()).unwrap();
	assert_eq!(retry.files_scanned, 1);
	assert_eq!(retry.blocked.len(), 1);
}

#[cfg(unix)]
#[test]
fn catalog_blocklists_hanging_scans_and_honours_scanner_env() {
	use std::os::unix::fs::PermissionsExt;

	let plugins = tempfile::tempdir().unwrap();
	install_into(plugins.path());
	let tools = tempfile::tempdir().unwrap();
	let fake = tools.path().join("hanging-scanner");
	std::fs::write(&fake, "#!/bin/sh\nexec sleep 30\n").unwrap();
	std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();

	// SAFETY: no other test in this binary reads or writes this variable.
	unsafe { std::env::set_var(cadenza_plugin_host::SCANNER_ENV, &fake) };
	let exe = cadenza_plugin_host::scanner_path().unwrap();
	unsafe { std::env::remove_var(cadenza_plugin_host::SCANNER_ENV) };
	assert_eq!(exe, fake);

	let cache = tempfile::tempdir().unwrap();
	let mut cat = PluginCatalog::load_or_default(cache.path().join("p.json"));
	cat.set_timeout(Duration::from_millis(500));
	let started = std::time::Instant::now();
	let report = cat.scan(&[plugins.path().to_path_buf()], &exe).unwrap();
	assert!(started.elapsed() < Duration::from_secs(10));
	assert_eq!(report.blocked.len(), 1);
	assert!(
		report.blocked[0].reason.contains("timed out"),
		"{:?}",
		report.blocked
	);
	assert_eq!(cat.descriptors().count(), 0);
}

#[test]
fn missing_scanner_is_reported() {
	let mut cat = PluginCatalog::load_or_default(std::env::temp_dir().join("never-saved.json"));
	let err = cat
		.scan(&[], std::path::Path::new("/no/such/scanner"))
		.unwrap_err();
	assert!(matches!(err, PluginError::ScannerMissing(_)));
}

fn run(
	device: &mut dyn cadenza_dsp::Processor,
	input: f32,
	frames: usize,
	events_at_start: &[MidiEvent],
) -> (Vec<f32>, Vec<f32>) {
	let mut out_l = Vec::with_capacity(frames);
	let mut out_r = Vec::with_capacity(frames);
	let mut l = vec![0.0; BLOCK];
	let mut r = vec![0.0; BLOCK];
	let mut done = 0;
	while done < frames {
		let n = BLOCK.min(frames - done);
		l[..n].fill(input);
		r[..n].fill(-input);
		let ev = if done == 0 { events_at_start } else { &[] };
		device.process(&mut l[..n], &mut r[..n], ev, &transport());
		out_l.extend_from_slice(&l[..n]);
		out_r.extend_from_slice(&r[..n]);
		done += n;
	}
	(out_l, out_r)
}

fn db(x: f32) -> f32 {
	10f32.powf(x / 20.0)
}

#[test]
fn gain_applies_level_and_reports_real_latency() {
	let desc = descriptor(GAIN_ID);
	let mut gain = instantiate(desc, SR, BLOCK, None).unwrap();
	assert_eq!(gain.latency(), 64);
	assert_eq!(gain.info().id, GAIN_ID);
	assert_eq!(gain.info().role, DeviceRole::Effect);
	assert_eq!(gain.info().param_index("0"), Some(0));
	assert_eq!(gain.tail_seconds(), 0.0);

	// Default 0 dB: unity after exactly 64 samples of delay.
	let (l, r) = run(gain.as_mut(), 1.0, 1000, &[]);
	assert!(l[..64].iter().all(|&x| x == 0.0), "{:?}", &l[..70]);
	assert!((l[64] - 1.0).abs() < 1e-6 && (r[64] + 1.0).abs() < 1e-6);

	// Automation through set_param (plain dB values, clamped).
	gain.set_param(0, -6.0);
	let (l, _) = run(gain.as_mut(), 1.0, 1000, &[]);
	assert!((l[999] - db(-6.0)).abs() < 1e-5, "{}", l[999]);
	// The change itself is delayed by the plugin's 64-sample line.
	assert!((l[10] - 1.0).abs() < 1e-6);
	gain.set_param(0, -1000.0);
	gain.set_param(0, f32::NAN);
	let (l, _) = run(gain.as_mut(), 1.0, 500, &[]);
	assert!((l[499] - db(-60.0)).abs() < 1e-6);

	// Reset clears the delay line.
	gain.reset();
	let (l, _) = run(gain.as_mut(), 0.0, 64, &[]);
	assert!(l.iter().all(|&x| x == 0.0));
}

#[test]
fn gain_reactivates_on_new_config() {
	let mut gain = instantiate(descriptor(GAIN_ID), SR, 64, None).unwrap();
	gain.set_param(0, 6.0);
	gain.prepare(44_100.0, 512);
	let (l, _) = run(gain.as_mut(), 0.25, 1024, &[]);
	assert!((l[1000] - 0.25 * db(6.0)).abs() < 1e-5, "{}", l[1000]);
	// Same config again is a no-op (no re-activation, delay line kept).
	gain.prepare(44_100.0, 512);
	let (l, _) = run(gain.as_mut(), 0.0, 64, &[]);
	assert!((l[0] - 0.25 * db(6.0)).abs() < 1e-5);
}

fn zero_crossing_hz(x: &[f32]) -> f32 {
	let crossings: Vec<usize> = (1..x.len())
		.filter(|&i| x[i - 1] <= 0.0 && x[i] > 0.0)
		.collect();
	let (first, last) = (crossings[0], *crossings.last().unwrap());
	(crossings.len() - 1) as f32 * SR / (last - first) as f32
}

#[test]
fn sine_plays_the_right_pitch_and_stops_after_note_off() {
	let desc = descriptor(SINE_ID);
	let mut sine = instantiate(desc, SR, BLOCK, None).unwrap();
	assert_eq!(sine.info().role, DeviceRole::Instrument);
	assert_eq!(sine.latency(), 0);
	assert_eq!(sine.tail_seconds(), 0.5);

	// Silence before any note (instrument output overwrites the input).
	let (l, _) = run(sine.as_mut(), 0.7, BLOCK, &[]);
	assert!(l.iter().all(|&x| x == 0.0));

	let on = [MidiEvent::NoteOn {
		offset: 10,
		pitch: 69,
		velocity: 1.0,
	}];
	let (l, r) = run(sine.as_mut(), 0.0, 24_000, &on);
	assert!(
		l[..10].iter().all(|&x| x == 0.0),
		"note starts at its offset"
	);
	assert_eq!(l, r);
	let hz = zero_crossing_hz(&l[1000..]);
	assert!((hz - 440.0).abs() < 2.0, "measured {hz} Hz");
	let peak = l[1000..].iter().fold(0f32, |m, x| m.max(x.abs()));
	assert!((peak - db(-6.0)).abs() < 0.01, "peak {peak}");

	// Level automation.
	sine.set_param(0, -20.0);
	let (l, _) = run(sine.as_mut(), 0.0, 4800, &[]);
	let peak = l[100..].iter().fold(0f32, |m, x| m.max(x.abs()));
	assert!((peak - db(-20.0)).abs() < 0.01, "peak {peak}");

	let off = [MidiEvent::NoteOff {
		offset: 0,
		pitch: 69,
	}];
	let (l, _) = run(sine.as_mut(), 0.0, 4800, &off);
	assert!(
		l[..100].iter().any(|&x| x != 0.0),
		"release is not instantaneous"
	);
	assert!(
		l[2000..].iter().all(|&x| x == 0.0),
		"silent after the 20 ms release"
	);

	// All-notes-off stops a chord.
	let chord = [60u8, 64, 67].map(|pitch| MidiEvent::NoteOn {
		offset: 0,
		pitch,
		velocity: 0.5,
	});
	run(sine.as_mut(), 0.0, 2400, &chord);
	let (l, _) = run(
		sine.as_mut(),
		0.0,
		4800,
		&[MidiEvent::AllNotesOff { offset: 0 }],
	);
	assert!(l[2000..].iter().all(|&x| x == 0.0));
}

#[test]
fn state_round_trips_between_instances() {
	let desc = descriptor(GAIN_ID);
	let mut a = instantiate(desc, SR, BLOCK, None).unwrap();
	a.set_param(0, -12.0);
	let state = a.save_state().expect("gain implements the state extension");
	assert!(!state.is_empty());

	let mut b = instantiate(desc, SR, BLOCK, Some(&state)).unwrap();
	let (l, _) = run(b.as_mut(), 1.0, 500, &[]);
	assert!((l[499] - db(-12.0)).abs() < 1e-5, "{}", l[499]);

	// load_state on a live instance.
	let mut c = instantiate(desc, SR, BLOCK, None).unwrap();
	assert!(c.load_state(&state));
	let (l, _) = run(c.as_mut(), 1.0, 500, &[]);
	assert!((l[499] - db(-12.0)).abs() < 1e-5);
	assert!(!c.load_state(b"garbage!"));

	match instantiate(desc, SR, BLOCK, Some(b"bad")) {
		Err(PluginError::State { id, .. }) => assert_eq!(id, GAIN_ID),
		Err(e) => panic!("unexpected error {e}"),
		Ok(_) => panic!("bad state accepted"),
	}
}

#[test]
fn unknown_plugin_and_vst3_fail_with_actionable_errors() {
	let mut missing = descriptor(GAIN_ID).clone();
	missing.id = "dev.cadenza.test.missing".into();
	let err = instantiate(&missing, SR, BLOCK, None).err().unwrap();
	assert!(matches!(err, PluginError::Instantiate { .. }), "{err}");

	let mut vst = descriptor(GAIN_ID).clone();
	vst.format = PluginFormat::Vst3;
	let err = instantiate(&vst, SR, BLOCK, None).err().unwrap();
	assert!(err.to_string().contains("VST3"));
}

#[test]
fn many_instances_can_be_created_and_dropped_across_threads() {
	let handles: Vec<_> = (0..4)
		.map(|i| {
			std::thread::spawn(move || {
				let id = if i % 2 == 0 { GAIN_ID } else { SINE_ID };
				let mut p = instantiate(descriptor(id), SR, BLOCK, None).unwrap();
				run(p.as_mut(), 0.1, 2048, &[]);
				p
			})
		})
		.collect();
	let devices: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
	// Processors were created on one thread and are dropped on another.
	drop(devices);
}
