//! Smoke test against plugins installed on this machine (Surge XT, LSP, ...).
//! Ignored by default: run with
//! `cargo test -p cadenza-plugin-host --test system_plugins -- --ignored --nocapture`.
//! Set `CADENZA_TEST_ALL_PLUGINS=1` to process every scanned plugin instead of
//! a representative subset.

mod common;

use cadenza_dsp::{MidiEvent, Transport};
use cadenza_plugin_host::{
	PluginCatalog, PluginDescriptor, PluginRole, default_search_paths, instantiate,
};

const REPRESENTATIVE: &[&str] = &[
	"org.surge-synth-team.surge-xt",
	"org.surge-synth-team.surge-xt-fx",
	"in.lsp-plug.compressor_stereo",
	"in.lsp-plug.para_equalizer_x8_stereo",
	"in.lsp-plug.limiter_stereo",
	"in.lsp-plug.room_builder_stereo",
];

fn exercise(desc: &PluginDescriptor) -> Result<String, String> {
	let sr = 48_000.0;
	let block = 512;
	let mut p = instantiate(desc, sr, block, None).map_err(|e| e.to_string())?;
	if std::env::var_os("CADENZA_TEST_SET_DEFAULTS").is_some() {
		for (i, param) in desc.params.iter().enumerate() {
			p.set_param(i, param.default as f32);
		}
	}
	p.reset();
	let transport = Transport {
		bpm: 120.0,
		playing: true,
		beat_position: 0.0,
	};
	let mut l = vec![0.0f32; block];
	let mut r = vec![0.0f32; block];
	let mut peak = 0f32;
	let mut diff = 0f32;
	let mut seed = 1u32;
	let blocks = (sr as usize) / block;
	for b in 0..blocks {
		if desc.role == PluginRole::Effect {
			for (x, y) in l.iter_mut().zip(r.iter_mut()) {
				seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
				*x = (seed >> 9) as f32 / (1u32 << 23) as f32 * 0.5 - 0.25;
				*y = *x;
			}
		}
		let events: &[MidiEvent] = match b {
			0 => &[MidiEvent::NoteOn {
				offset: 0,
				pitch: 60,
				velocity: 0.9,
			}],
			60 => &[MidiEvent::NoteOff {
				offset: 0,
				pitch: 60,
			}],
			_ => &[],
		};
		let dry = l.clone();
		p.process(&mut l, &mut r, events, &transport);
		diff = dry
			.iter()
			.zip(&l)
			.fold(diff, |m, (a, b)| m.max((a - b).abs()));
		if l.iter().chain(&r).any(|x| !x.is_finite()) {
			return Err("produced NaN/inf".into());
		}
		peak = l.iter().chain(&r).fold(peak, |m, x| m.max(x.abs()));
	}
	let state = p.save_state();
	let restored = match &state {
		Some(s) => p.load_state(s),
		None => false,
	};
	Ok(format!(
		"{:<11} params={:<4} latency={:<5} tail={:.2}s peak={:.3} changed={:.3} state={} restore={}",
		format!("{:?}", desc.role),
		desc.params.len(),
		p.latency(),
		p.tail_seconds(),
		peak,
		diff,
		state.map_or("none".into(), |s| format!("{}B", s.len())),
		restored
	))
}

#[test]
#[ignore = "needs CLAP plugins installed system-wide"]
fn scan_and_run_installed_plugins() {
	let cache = tempfile::tempdir().unwrap();
	let mut catalog = PluginCatalog::load_or_default(cache.path().join("plugins.json"));
	let roots = default_search_paths();
	let started = std::time::Instant::now();
	let report = catalog.scan(&roots, &common::scanner()).unwrap();
	println!(
		"scanned {:?}: {} files, {} plugins added, {} blocked in {:.1}s",
		roots,
		report.files_found,
		report.added.len(),
		report.blocked.len(),
		started.elapsed().as_secs_f64()
	);
	for b in &report.blocked {
		println!("BLOCKED {}: {}", b.path.display(), b.reason);
	}
	let all = std::env::var_os("CADENZA_TEST_ALL_PLUGINS").is_some();
	let chosen: Vec<&PluginDescriptor> = catalog
		.descriptors()
		.filter(|d| all || REPRESENTATIVE.contains(&d.id.as_str()))
		.collect();
	if chosen.is_empty() {
		println!("no matching plugins installed; nothing to run");
		return;
	}
	let mut failures = Vec::new();
	for desc in chosen {
		match exercise(desc) {
			Ok(line) => println!("OK   {:<45} {line}", desc.id),
			Err(e) => {
				println!("FAIL {:<45} {e}", desc.id);
				failures.push(desc.id.clone());
			}
		}
	}
	assert!(failures.is_empty(), "failed: {failures:?}");
}
