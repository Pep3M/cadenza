//! Sandboxed hosting of real plugins installed on this machine: Surge XT
//! (CLAP instrument) and LSP (VST3 effects) must sound the same sandboxed as
//! in-process. Ignored by default: run with
//! `cargo test -p cadenza-plugin-host --release --test sandbox_system -- --ignored --nocapture`.

// Plugin sandboxing is Unix-only (`SANDBOX_SUPPORTED`).
#![cfg(unix)]

mod common;

use std::path::Path;
use std::time::{Duration, Instant};

use cadenza_dsp::{MidiEvent, Processor, Transport};
use cadenza_plugin_host::scanner::scan_in_subprocess;
use cadenza_plugin_host::{
	PluginDescriptor, PluginRole, ProcessMode, instantiate_sandboxed, instantiate_with_mode,
};
use common::{sandbox_options, scanner};

const SR: f32 = 48_000.0;
/// The engine's device quantum.
const BLOCK: usize = 32;

fn find(file: &str, name: &str) -> Option<PluginDescriptor> {
	if !Path::new(file).exists() {
		return None;
	}
	scan_in_subprocess(&scanner(), Path::new(file), Duration::from_secs(120))
		.unwrap_or_else(|e| panic!("scan of {file} failed: {e:?}"))
		.into_iter()
		.find(|d| d.name == name)
}

/// One second of audio: a C major chord for instruments, noise for effects.
fn render(p: &mut dyn Processor, role: PluginRole) -> (Vec<f32>, Duration) {
	let mut out = Vec::with_capacity(SR as usize * 2);
	let (mut l, mut r) = (vec![0.0f32; BLOCK], vec![0.0f32; BLOCK]);
	let mut seed = 1u32;
	let mut t = Transport {
		bpm: 120.0,
		playing: true,
		beat_position: 0.0,
		..Default::default()
	};
	let chord: Vec<MidiEvent> = [60u8, 64, 67]
		.map(|pitch| MidiEvent::NoteOn {
			offset: 5,
			pitch,
			velocity: 0.8,
		})
		.to_vec();
	let off = [MidiEvent::AllNotesOff { offset: 0 }];
	let blocks = SR as usize / BLOCK;
	let mut busy = Duration::ZERO;
	for b in 0..blocks {
		for (x, y) in l.iter_mut().zip(r.iter_mut()) {
			if role == PluginRole::Effect {
				seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
				*x = (seed >> 9) as f32 / (1u32 << 23) as f32 * 0.5 - 0.25;
				*y = -*x;
			} else {
				(*x, *y) = (0.0, 0.0);
			}
		}
		let events: &[MidiEvent] = match b {
			0 => &chord,
			b if b == blocks * 3 / 4 => &off,
			_ => &[],
		};
		let s = Instant::now();
		p.process(&mut l, &mut r, events, &t);
		busy += s.elapsed();
		t.beat_position += BLOCK as f64 / f64::from(SR) * 2.0;
		out.extend_from_slice(&l);
		out.extend_from_slice(&r);
	}
	(out, busy / blocks as u32)
}

fn max_diff(a: &[f32], b: &[f32]) -> f32 {
	a.iter()
		.zip(b)
		.map(|(x, y)| (x - y).abs())
		.fold(0.0, f32::max)
}

fn compare(desc: &PluginDescriptor) {
	let create_local =
		|| instantiate_with_mode(desc, SR, BLOCK, None, ProcessMode::Offline).unwrap();
	let (a, local_us) = render(create_local().as_mut(), desc.role);
	let (b, _) = render(create_local().as_mut(), desc.role);
	let mut remote = instantiate_sandboxed(
		desc,
		SR,
		BLOCK,
		None,
		ProcessMode::Offline,
		sandbox_options(),
	)
	.unwrap();
	assert_eq!(remote.latency(), create_local().latency());
	let (s, remote_us) = render(&mut remote, desc.role);
	assert!(remote.crashed().is_none(), "{:?}", remote.crashed());
	let rms = (a.iter().map(|x| f64::from(x * x)).sum::<f64>() / a.len() as f64).sqrt();
	let (baseline, sandboxed) = (max_diff(&a, &b), max_diff(&a, &s));
	println!(
		"| {:<32} | rms {rms:.4} | in-proc vs in-proc {baseline:.2e} | in-proc vs sandbox {sandboxed:.2e} | {:>6.1} µs vs {:>6.1} µs per {BLOCK}-frame block |",
		desc.name,
		local_us.as_secs_f64() * 1e6,
		remote_us.as_secs_f64() * 1e6,
	);
	assert!(rms > 1e-4, "{} is silent", desc.name);
	if baseline == 0.0 {
		assert!(
			sandboxed <= 1e-6,
			"{}: sandboxed output differs by {sandboxed}",
			desc.name
		);
	} else {
		// Not deterministic even in-process (e.g. Surge XT's random
		// oscillator phases): compare loudness instead.
		let rms_s = (s.iter().map(|x| f64::from(x * x)).sum::<f64>() / s.len() as f64).sqrt();
		println!("  (non-deterministic plugin: rms in-process {rms:.4}, sandboxed {rms_s:.4})");
		assert!(
			(rms_s / rms - 1.0).abs() < 0.05,
			"{}: rms {rms_s} vs {rms}",
			desc.name
		);
	}

	// Realtime mode keeps up with the engine's 32-frame quantum (no timeouts).
	let mut rt = instantiate_sandboxed(
		desc,
		SR,
		BLOCK,
		None,
		ProcessMode::Realtime,
		sandbox_options(),
	)
	.unwrap();
	for _ in 0..3 {
		render(&mut rt, desc.role);
	}
	assert!(rt.crashed().is_none(), "{:?}", rt.crashed());
	// State survives the process boundary.
	let state = remote.save_state().expect("state");
	assert!(remote.load_state(&state));
}

#[test]
#[ignore = "needs Surge XT (CLAP) and LSP (VST3) installed system-wide"]
fn real_plugins_sound_the_same_sandboxed() {
	let plugins = [
		find("/usr/lib/clap/Surge XT.clap", "Surge XT"),
		find("/usr/lib/vst3/lsp-plugins.vst3", "Compressor Stereo"),
		find(
			"/usr/lib/vst3/lsp-plugins.vst3",
			"Parametric Equalizer x8 Stereo",
		),
	];
	if plugins.iter().any(Option::is_none) {
		println!("Surge XT CLAP or LSP VST3 not installed; skipping");
		return;
	}
	for desc in plugins.iter().flatten() {
		compare(desc);
	}
}
