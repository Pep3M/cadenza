//! Realtime robustness of a real sandboxed plugin with its editor open: Surge
//! XT (CLAP) driven like the engine drives it (32-frame quanta, batched by the
//! sandbox, callbacks paced at real time into a null sink) while its editor
//! window opens, stays up a few seconds, the host changes parameters and reads
//! the live state, and the window closes. Opening Surge's editor used to stall
//! its audio thread for longer than the old 2 ms deadline, which bypassed the
//! instrument for good.
//!
//! Ignored by default (needs `DISPLAY` and `/usr/lib/clap/Surge XT.clap`; a
//! window appears for a few seconds, no sound is played):
//! `cargo test -p cadenza-plugin-host --release --test sandbox_realtime -- --ignored --nocapture`
//!
//! For comparisons, `CADENZA_TEST_CHUNK` (frames per round trip, 0 = none)
//! and `CADENZA_TEST_RT_PRIORITY` (0 = normal scheduling) override the
//! defaults; `CADENZA_TEST_SECONDS` sets the run length (default 8) and
//! `CADENZA_TEST_STRESS=<threads>` adds spinning threads (a busy desktop).

mod common;

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use cadenza_dsp::{MidiEvent, Processor, Transport};
use cadenza_plugin_host::scanner::scan_in_subprocess;
use cadenza_plugin_host::{ProcessMode, SandboxOptions, instantiate_sandboxed};
use common::{sandbox_options, scanner};

const SR: f32 = 48_000.0;
const QUANTUM: usize = 32;
/// Frames per simulated audio callback (5.3 ms at 48 kHz).
const CALLBACK: usize = 256;

fn env_usize(name: &str) -> Option<usize> {
	std::env::var(name).ok()?.trim().parse().ok()
}

#[test]
#[ignore = "needs DISPLAY and Surge XT; opens a window for a few seconds"]
fn surge_xt_survives_its_editor_while_playing_in_real_time() {
	let file = "/usr/lib/clap/Surge XT.clap";
	if std::env::var_os("DISPLAY").is_none() || !Path::new(file).exists() {
		println!("needs DISPLAY and {file}; skipping");
		return;
	}
	let desc = scan_in_subprocess(&scanner(), Path::new(file), Duration::from_secs(120))
		.expect("scan Surge XT")
		.into_iter()
		.find(|d| d.name == "Surge XT")
		.expect("Surge XT in its CLAP file");
	let defaults = SandboxOptions::default();
	let options = SandboxOptions {
		realtime_chunk: env_usize("CADENZA_TEST_CHUNK").unwrap_or(defaults.realtime_chunk),
		realtime_priority: env_usize("CADENZA_TEST_RT_PRIORITY")
			.map_or(defaults.realtime_priority, |p| p as u8),
		..sandbox_options()
	};
	let seconds = env_usize("CADENZA_TEST_SECONDS").unwrap_or(8) as u64;
	let mut p = instantiate_sandboxed(
		&desc,
		SR,
		QUANTUM,
		None,
		ProcessMode::Realtime,
		options.clone(),
	)
	.unwrap();
	let monitor = p.monitor();
	let editor = p.editor();
	println!(
		"Surge XT sandboxed: chunk {} frames, added latency {} frames, audio thread {}",
		options.realtime_chunk,
		p.latency(),
		monitor.scheduling().unwrap_or_default()
	);
	// A few automatable parameters to move from the host side.
	let params: Vec<usize> = desc
		.params
		.iter()
		.enumerate()
		.filter(|(_, d)| d.automatable && !d.stepped && d.max > d.min)
		.map(|(i, _)| i)
		.take(8)
		.collect();
	assert!(!params.is_empty());
	let ranges: Vec<(f32, f32)> = params
		.iter()
		.map(|&i| (desc.params[i].min as f32, desc.params[i].max as f32))
		.collect();

	let stop = Arc::new(AtomicBool::new(false));
	// Optional CPU contention (a busy desktop): one spinning thread per core.
	let stress: Vec<_> = (0..env_usize("CADENZA_TEST_STRESS").unwrap_or(0))
		.map(|_| {
			let stop = stop.clone();
			std::thread::spawn(move || {
				let mut x = 0u64;
				while !stop.load(Ordering::Relaxed) {
					x = std::hint::black_box(x.wrapping_mul(6364136223846793005).wrapping_add(1));
				}
			})
		})
		.collect();

	// The "audio thread": callbacks at real time, output discarded.
	let audio = {
		let stop = stop.clone();
		std::thread::spawn(move || {
			let period = Duration::from_secs_f64(CALLBACK as f64 / f64::from(SR));
			let (mut l, mut r) = ([0.0f32; QUANTUM], [0.0f32; QUANTUM]);
			let mut t = Transport {
				bpm: 120.0,
				playing: true,
				beat_position: 0.0,
				..Default::default()
			};
			let (mut callbacks, mut overruns, mut slowest) = (0u64, 0u64, Duration::ZERO);
			let mut quantum = 0u64;
			let mut next = Instant::now();
			while !stop.load(Ordering::Relaxed) {
				let started = Instant::now();
				for _ in 0..CALLBACK / QUANTUM {
					// A chord every half second, released after 0.4 s.
					let pos = quantum % (SR as u64 / 2 / QUANTUM as u64);
					let chord = [57u8, 60, 64].map(|pitch| MidiEvent::NoteOn {
						offset: 3,
						pitch,
						velocity: 0.7,
					});
					let off = [57u8, 60, 64].map(|pitch| MidiEvent::NoteOff { offset: 9, pitch });
					let events: &[MidiEvent] = match pos {
						0 => &chord,
						600 => &off,
						_ => &[],
					};
					// Slow host-side automation of a few parameters.
					if quantum.is_multiple_of(16) {
						let k = (quantum / 16) as usize % params.len();
						let (lo, hi) = ranges[k];
						let phase = (quantum as f32 / 3000.0).sin() * 0.5 + 0.5;
						p.set_param(params[k], lo + (hi - lo) * (0.3 + 0.4 * phase));
					}
					l.fill(0.0);
					r.fill(0.0);
					p.process(&mut l, &mut r, events, &t);
					t.beat_position += QUANTUM as f64 / f64::from(SR) * t.bpm / 60.0;
					quantum += 1;
				}
				let took = started.elapsed();
				slowest = slowest.max(took);
				if took > period {
					overruns += 1;
				}
				callbacks += 1;
				next += period;
				let now = Instant::now();
				if next > now {
					std::thread::sleep(next - now);
				} else {
					next = now;
				}
			}
			(p, callbacks, overruns, slowest)
		})
	};

	let title = "Surge XT — realtime sandbox test";
	std::thread::sleep(Duration::from_secs(1));
	editor.open(title).expect("open the editor");
	let open_for = Duration::from_secs(seconds.saturating_sub(3).max(2));
	let opened = Instant::now();
	let mut events = 0;
	let mut states = 0;
	while opened.elapsed() < open_for {
		events += editor.poll().len();
		if opened.elapsed() > open_for / 2 && states < 2 {
			// Live state while the editor is up and audio runs.
			editor.save_state().expect("live state");
			states += 1;
		}
		std::thread::sleep(Duration::from_millis(30));
	}
	editor.close().expect("close the editor");
	std::thread::sleep(Duration::from_secs(1));
	stop.store(true, Ordering::Relaxed);
	let (p, callbacks, overruns, slowest) = audio.join().unwrap();
	for s in stress {
		s.join().unwrap();
	}

	let stats = monitor.stats();
	println!(
		"{callbacks} callbacks of {CALLBACK} frames, {overruns} over their period, slowest {slowest:?}; \
		 editor events {events}; sandbox {stats:?}"
	);
	assert!(p.crashed().is_none(), "{:?}", p.crashed());
	assert!(
		stats.late_blocks <= 3,
		"too many late blocks for an idle-ish synth: {stats:?}"
	);
	drop(p);
}
