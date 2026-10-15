//! VST3 hosting against plugins installed system-wide (Surge XT, Surge XT
//! Effects, LSP). Ignored by default: run with
//! `cargo test -p cadenza-plugin-host --test vst3_system -- --ignored --nocapture`.

mod common;

use std::path::{Path, PathBuf};

use cadenza_dsp::{MidiEvent, Processor, Transport};
use cadenza_plugin_host::{
	PluginCatalog, PluginDescriptor, PluginFormat, PluginRole, ProcessMode, instantiate,
	instantiate_with_mode,
};

const SR: f32 = 48_000.0;
const BLOCK: usize = 512;

const BUNDLES: &[&str] = &[
	"/usr/lib/vst3/Surge XT.vst3",
	"/usr/lib/vst3/Surge XT Effects.vst3",
	"/usr/lib/vst3/lsp-plugins.vst3",
];

fn transport() -> Transport {
	Transport {
		bpm: 120.0,
		playing: true,
		beat_position: 0.0,
		..Default::default()
	}
}

/// Deterministic noise test signal at about -12 dBFS.
fn signal(l: &mut [f32], r: &mut [f32], seed: &mut u32) {
	for (x, y) in l.iter_mut().zip(r.iter_mut()) {
		*seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
		*x = (*seed >> 9) as f32 / (1u32 << 23) as f32 * 0.5 - 0.25;
		*y = -*x;
	}
}

struct Run {
	peak: f32,
	rms: f32,
}

/// Processes `seconds` of audio: a C4 note for instruments, noise for effects.
fn run(p: &mut dyn Processor, role: PluginRole, seconds: f32, note: bool) -> Result<Run, String> {
	let mut l = vec![0.0f32; BLOCK];
	let mut r = vec![0.0f32; BLOCK];
	let mut seed = 1u32;
	let (mut peak, mut sum, mut count) = (0f32, 0f64, 0usize);
	let blocks = (SR * seconds) as usize / BLOCK;
	let mut t = transport();
	for b in 0..blocks {
		if role == PluginRole::Effect {
			signal(&mut l, &mut r, &mut seed);
		} else {
			l.fill(0.0);
			r.fill(0.0);
		}
		let events: &[MidiEvent] = match (note, b) {
			(true, 0) => &[MidiEvent::NoteOn {
				offset: 10,
				pitch: 60,
				velocity: 0.9,
			}],
			(true, b) if b == blocks * 3 / 4 => &[MidiEvent::NoteOff {
				offset: 0,
				pitch: 60,
			}],
			_ => &[],
		};
		p.process(&mut l, &mut r, events, &t);
		t.beat_position += BLOCK as f64 / f64::from(SR) * t.bpm / 60.0;
		if l.iter().chain(&r).any(|x| !x.is_finite()) {
			return Err(format!("NaN/inf in block {b}"));
		}
		for x in l.iter().chain(&r) {
			peak = peak.max(x.abs());
			sum += f64::from(x * x);
		}
		count += 2 * BLOCK;
	}
	Ok(Run {
		peak,
		rms: (sum / count.max(1) as f64).sqrt() as f32,
	})
}

fn installed() -> Vec<PathBuf> {
	BUNDLES
		.iter()
		.map(PathBuf::from)
		.filter(|p| p.exists())
		.collect()
}

fn scan() -> PluginCatalog {
	let cache = std::env::temp_dir().join(format!("cadenza-vst3-test-{}.json", std::process::id()));
	let mut catalog = PluginCatalog::load_or_default(&cache);
	let started = std::time::Instant::now();
	let report = catalog.scan(&installed(), &common::scanner()).unwrap();
	println!(
		"scanned {} bundles out-of-process: {} plugins, {} blocked in {:.1}s",
		report.files_found,
		report.added.len(),
		report.blocked.len(),
		started.elapsed().as_secs_f64()
	);
	for b in &report.blocked {
		println!("BLOCKED {}: {}", b.path.display(), b.reason);
	}
	assert!(report.blocked.is_empty());
	let _ = std::fs::remove_file(cache);
	catalog
}

fn find<'a>(catalog: &'a PluginCatalog, bundle: &str, name: &str) -> &'a PluginDescriptor {
	catalog
		.descriptors()
		.find(|d| d.format == PluginFormat::Vst3 && d.path == Path::new(bundle) && d.name == name)
		.unwrap_or_else(|| panic!("{name} not found in {bundle}"))
}

fn param(desc: &PluginDescriptor, name: &str) -> usize {
	desc.params
		.iter()
		.position(|p| p.name == name)
		.unwrap_or_else(|| panic!("{} has no param '{name}'", desc.name))
}

fn row(desc: &PluginDescriptor, p: &mut dyn Processor, out: &Run) -> usize {
	let state = p.save_state().expect("state");
	assert!(p.load_state(&state), "{}: state reload refused", desc.name);
	println!(
		"| {:<28} | {:>6} | {:>7} | {:>5.2} s | peak {:>6.3} rms {:>6.4} | {:>7} B |",
		desc.name,
		desc.params.len(),
		p.latency(),
		p.tail_seconds(),
		out.peak,
		out.rms,
		state.len()
	);
	state.len()
}

#[test]
#[ignore = "needs Surge XT and LSP VST3 bundles in /usr/lib/vst3"]
fn vst3_system_plugins() {
	if installed().len() != BUNDLES.len() {
		println!("not all VST3 bundles installed; skipping");
		return;
	}
	let catalog = scan();
	println!("| plugin | params | latency | tail | output | state |");

	// Instrument: Surge XT plays a C4.
	let surge = find(&catalog, BUNDLES[0], "Surge XT");
	assert_eq!(surge.role, PluginRole::Instrument);
	assert!(surge.note_input);
	let mut p = instantiate(surge, SR, BLOCK, None).unwrap();
	let out = run(p.as_mut(), surge.role, 1.0, true).unwrap();
	assert!(out.rms > 1e-4, "Surge XT stayed silent (rms {})", out.rms);
	let state = row(surge, p.as_mut(), &out);
	assert!(state > 0);
	drop(p);
	// A second instance restored from state also plays (and offline mode works).
	let saved = {
		let mut p = instantiate(surge, SR, BLOCK, None).unwrap();
		p.save_state().unwrap()
	};
	let mut p =
		instantiate_with_mode(surge, SR, BLOCK, Some(&saved), ProcessMode::Offline).unwrap();
	let again = run(p.as_mut(), surge.role, 0.5, true).unwrap();
	assert!(again.rms > 1e-4);
	drop(p);

	// Effect: Surge XT Effects on noise.
	let fx = find(&catalog, BUNDLES[1], "Surge XT Effects");
	assert_eq!(fx.role, PluginRole::Effect);
	let mut p = instantiate(fx, SR, BLOCK, None).unwrap();
	let out = run(p.as_mut(), fx.role, 1.0, false).unwrap();
	assert!(out.rms > 1e-4, "Surge XT Effects output silent");
	row(fx, p.as_mut(), &out);
	drop(p);

	// LSP compressor.
	let comp = find(&catalog, BUNDLES[2], "Compressor Stereo");
	let mut p = instantiate(comp, SR, BLOCK, None).unwrap();
	let out = run(p.as_mut(), comp.role, 1.0, false).unwrap();
	assert!(out.rms > 1e-4, "LSP compressor output silent");
	row(comp, p.as_mut(), &out);
	drop(p);

	// LSP parametric EQ: "Output gain" (linear, default 1.0) audibly changes
	// the output, survives a state round trip into a fresh instance.
	let eq = find(&catalog, BUNDLES[2], "Parametric Equalizer x8 Stereo");
	let gain = param(eq, "Output gain");
	let mut p = instantiate(eq, SR, BLOCK, None).unwrap();
	let before = run(p.as_mut(), eq.role, 0.5, false).unwrap();
	p.set_param(gain, 0.1);
	run(p.as_mut(), eq.role, 0.25, false).unwrap();
	let after = run(p.as_mut(), eq.role, 0.5, false).unwrap();
	let ratio = after.rms / before.rms;
	println!("LSP EQ output gain 1.0 -> 0.1: rms ratio {ratio:.3}");
	assert!((0.07..0.14).contains(&ratio), "ratio {ratio}");
	p.set_param(gain, 0.5);
	run(p.as_mut(), eq.role, 0.1, false).unwrap();
	row(eq, p.as_mut(), &after);
	let state = p.save_state().unwrap();
	drop(p);
	let mut restored = instantiate(eq, SR, BLOCK, Some(&state)).unwrap();
	let r = run(restored.as_mut(), eq.role, 0.5, false).unwrap();
	let ratio = r.rms / before.rms;
	println!("LSP EQ restored from state: rms ratio {ratio:.3}");
	assert!((0.4..0.6).contains(&ratio), "restored ratio {ratio}");

	// Re-preparing at another rate re-activates without breaking output.
	restored.prepare(44_100.0, 256);
	restored.reset();
	let r = run(restored.as_mut(), eq.role, 0.25, false).unwrap();
	assert!(r.rms > 1e-4);
}
