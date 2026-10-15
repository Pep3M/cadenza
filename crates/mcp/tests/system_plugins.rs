//! Real-world smoke test against plugins installed on this machine: Surge XT (CLAP) as
//! the instrument and the LSP compressor (VST3) on the master.
//! Ignored by default: `cargo test -p cadenza-mcp --release --test system_plugins -- --ignored --nocapture`.

mod plugin_support;

use cadenza_mcp::Session;
use cadenza_mcp::tools::{compose, devices, render, tracks};
use cadenza_plugin_host::SCANNER_ENV;
use serde::de::DeserializeOwned;
use serde_json::json;

fn args<T: DeserializeOwned>(value: serde_json::Value) -> T {
	serde_json::from_value(value).expect("valid tool args")
}

#[tokio::test]
#[ignore = "needs Surge XT (CLAP) and LSP (VST3) installed in /usr/lib/{clap,vst3}"]
async fn surge_chord_through_lsp_compressor() {
	// SAFETY: the only test in this binary, set before any other thread reads the env.
	unsafe { std::env::set_var(SCANNER_ENV, plugin_support::scanner()) };
	let cache = tempfile::tempdir().unwrap();
	let session = std::sync::Mutex::new(Session::with_plugin_cache(cache.path().join("p.json")));

	let started = std::time::Instant::now();
	let scan = devices::scan_plugins(
		&session,
		args(json!({ "paths": ["/usr/lib/clap", "/usr/lib/vst3"] })),
	)
	.await
	.unwrap();
	eprintln!(
		"scan: {} files, {} plugins ({} instruments, {} effects), {} blocked, {:.1} s",
		scan.files_found,
		scan.total_plugins,
		scan.instruments,
		scan.effects,
		scan.blocked.len(),
		started.elapsed().as_secs_f64()
	);
	for b in &scan.blocked {
		eprintln!("  blocked {}: {}", b.path, b.reason);
	}

	let mut s = session.into_inner().unwrap();
	let catalog = s.plugins();
	let surge = devices::list_devices(
		Some(&catalog),
		args(json!({ "query": "surge xt", "role": "instrument", "source": "plugin" })),
	);
	let surge = surge
		.devices
		.iter()
		.find(|d| d.name == "Surge XT")
		.unwrap_or_else(|| panic!("Surge XT not found: {surge:#?}"));
	eprintln!("instrument: {} ({} params)", surge.id, surge.param_count);
	// The VST3 build exposes thousands of non-automatable MIDI CC params: hidden by default.
	let surge_vst3 = format!(
		"vst3:{}",
		catalog
			.descriptors()
			.find(|d| d.name == "Surge XT" && d.format == cadenza_plugin_host::PluginFormat::Vst3)
			.expect("Surge XT VST3 scanned")
			.id
	);
	let shown =
		devices::get_device_info(Some(&catalog), args(json!({ "device": surge_vst3 }))).unwrap();
	let all = devices::get_device_info(
		Some(&catalog),
		args(json!({ "device": surge_vst3, "include_non_automatable": true })),
	)
	.unwrap();
	eprintln!(
		"Surge XT VST3 params: {} automatable of {}",
		shown.param_count, all.param_count
	);
	assert!(shown.param_count < all.param_count);
	assert!(shown.params.iter().all(|p| p.automatable));
	let comps = devices::list_devices(
		Some(&catalog),
		args(json!({ "query": "lsp compressor stereo", "role": "effect", "source": "plugin" })),
	);
	let comp = comps
		.devices
		.iter()
		.filter(|d| d.format == Some("VST3") && d.name.to_lowercase().contains("compressor"))
		.min_by_key(|d| d.name.len())
		.unwrap_or_else(|| panic!("no LSP compressor: {comps:#?}"));
	eprintln!(
		"master compressor: {} '{}' ({} params)",
		comp.id, comp.name, comp.param_count
	);

	let track = tracks::create_track(
		&mut s,
		args(
			json!({ "name": "Surge", "kind": "instrument", "instrument": { "device": surge.id } }),
		),
	)
	.unwrap()
	.track_id;
	compose::write_chord_progression(
		&mut s,
		args(json!({
			"track_id": track,
			"start_beat": 0.0,
			"progression": "C F G C",
			"beats_per_chord": 2.0
		})),
	)
	.unwrap();
	tracks::insert_device(&mut s, args(json!({ "device": comp.id }))).unwrap();

	let path = std::env::temp_dir().join("cadenza-surge-system-test.wav");
	let report = render::render(
		s.document().project().clone(),
		s.plugins_for_project(),
		s.plugin_hosting().clone(),
		args(json!({ "path": path.display().to_string() })),
	)
	.await
	.unwrap()
	.report;
	eprintln!("{}", serde_json::to_string_pretty(&report.master).unwrap());
	eprintln!(
		"duration {:.2} s, latency {} samples, warnings {:?}",
		report.duration_seconds, report.latency_samples, report.warnings
	);
	assert!(report.warnings.is_empty(), "{:?}", report.warnings);
	assert!(
		report.master.peak_db > -40.0,
		"silent render: {:?}",
		report.master
	);
	assert!(report.master.lufs_integrated > -60.0);

	let mut wav = hound::WavReader::open(&path).unwrap();
	let samples: Vec<f32> = wav.samples::<f32>().map(Result::unwrap).collect();
	assert!(samples.iter().all(|x| x.is_finite()), "NaN/inf in output");
	assert!(samples.iter().any(|x| x.abs() > 0.01));
	std::fs::remove_file(&path).ok();
}
