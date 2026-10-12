//! Sandboxed (out-of-process) plugin hosting against the test plugins:
//! output parity with in-process hosting, state, latency, crash and hang
//! isolation, restart, and a round-trip benchmark (ignored).

mod common;

use std::time::{Duration, Instant};

use cadenza_dsp::{MidiEvent, Processor, Transport};
use cadenza_plugin_host::{
	CrashReason, HostingMode, PluginError, ProcessMode, SandboxOptions, instantiate,
	instantiate_hosted, instantiate_sandboxed, instantiate_with_mode,
};
use common::{
	CRASH_ID, GAIN_ID, SINE_ID, crash_descriptor, descriptor, sandbox_exe, sandbox_options,
};

const SR: f32 = 48_000.0;
const BLOCK: usize = 512;

fn transport(beat: f64) -> Transport {
	Transport {
		bpm: 120.0,
		playing: true,
		beat_position: beat,
	}
}

/// Feeds a deterministic signal through `p` in blocks of varying size, with
/// notes and parameter automation, and returns the concatenated output.
fn exercise(p: &mut dyn Processor, sizes: &[usize], automate: bool) -> Vec<f32> {
	let mut out = Vec::new();
	let mut l = vec![0.0f32; BLOCK * 2];
	let mut r = vec![0.0f32; BLOCK * 2];
	let mut seed = 7u32;
	let mut beat = 0.0;
	for (b, &n) in sizes.iter().cycle().take(sizes.len() * 6).enumerate() {
		for (x, y) in l[..n].iter_mut().zip(r[..n].iter_mut()) {
			seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
			*x = (seed >> 9) as f32 / (1u32 << 23) as f32 - 0.5;
			*y = -0.5 * *x;
		}
		let events: &[MidiEvent] = match b % 6 {
			0 => &[
				MidiEvent::NoteOn {
					offset: 0,
					pitch: 60,
					velocity: 0.8,
				},
				MidiEvent::NoteOn {
					offset: 3,
					pitch: 67,
					velocity: 0.5,
				},
			],
			3 => &[MidiEvent::NoteOff {
				offset: 1,
				pitch: 60,
			}],
			5 => &[MidiEvent::AllNotesOff { offset: 0 }],
			_ => &[],
		};
		if automate && b % 4 == 2 {
			p.set_param(0, -3.0 * (b % 5) as f32);
		}
		p.process(&mut l[..n], &mut r[..n], events, &transport(beat));
		out.extend_from_slice(&l[..n]);
		out.extend_from_slice(&r[..n]);
		beat += n as f64 / f64::from(SR) * 2.0;
	}
	out
}

fn sandboxed(id: &str, state: Option<&[u8]>) -> cadenza_plugin_host::SandboxedProcessor {
	instantiate_sandboxed(
		descriptor(id),
		SR,
		BLOCK,
		state,
		ProcessMode::Realtime,
		sandbox_options(),
	)
	.unwrap()
}

#[test]
fn sandboxed_output_matches_in_process_bit_for_bit() {
	let sizes = [1, 7, 32, 256, 512, 100];
	for id in [GAIN_ID, SINE_ID] {
		let mut local = instantiate(descriptor(id), SR, BLOCK, None).unwrap();
		let mut remote = sandboxed(id, None);
		let a = exercise(local.as_mut(), &sizes, true);
		let b = exercise(&mut remote, &sizes, true);
		assert_eq!(a.len(), b.len());
		assert!(a.iter().any(|&x| x != 0.0), "{id} produced silence");
		let max_diff = a
			.iter()
			.zip(&b)
			.map(|(x, y)| (x - y).abs())
			.fold(0f32, f32::max);
		assert_eq!(
			max_diff, 0.0,
			"{id}: sandboxed output differs by {max_diff}"
		);

		// Reset and re-activation behave the same as well.
		local.reset();
		remote.reset();
		local.prepare(44_100.0, 256);
		remote.prepare(44_100.0, 256);
		let a = exercise(local.as_mut(), &[256, 64], false);
		let b = exercise(&mut remote, &[256, 64], false);
		assert_eq!(a, b, "{id} after reset/prepare");
		assert!(remote.crashed().is_none());
	}
}

#[test]
fn blocks_larger_than_the_shared_buffer_are_chunked() {
	// prepare() above the initial 4096-frame capacity: processed in chunks,
	// which for these plugins is still sample-exact.
	let big = 6000;
	let mut local = instantiate(descriptor(SINE_ID), SR, BLOCK, None).unwrap();
	let mut remote = sandboxed(SINE_ID, None);
	local.prepare(SR, big);
	remote.prepare(SR, big);
	let on = [
		MidiEvent::NoteOn {
			offset: 10,
			pitch: 69,
			velocity: 1.0,
		},
		MidiEvent::NoteOff {
			offset: 5000,
			pitch: 69,
		},
	];
	let mut a = (vec![0.0; big], vec![0.0; big]);
	let mut b = (vec![0.0; big], vec![0.0; big]);
	local.process(&mut a.0, &mut a.1, &on, &transport(0.0));
	remote.process(&mut b.0, &mut b.1, &on, &transport(0.0));
	assert!(a.0[4500..4600].iter().any(|&x| x != 0.0));
	assert_eq!(a, b);
}

#[test]
fn latency_tail_and_info_are_reported() {
	let gain = sandboxed(GAIN_ID, None);
	assert_eq!(gain.latency(), 64);
	assert_eq!(gain.tail_seconds(), 0.0);
	assert_eq!(gain.info().id, GAIN_ID);
	assert!(std::ptr::eq(gain.info(), descriptor(GAIN_ID).device_info()));
	let sine = sandboxed(SINE_ID, None);
	assert_eq!(sine.latency(), 0);
	assert_eq!(sine.tail_seconds(), 0.5);

	// The generic entry point finds the sandbox next to the test binary.
	let hosted = instantiate_hosted(
		descriptor(GAIN_ID),
		SR,
		BLOCK,
		None,
		ProcessMode::Offline,
		HostingMode::Sandboxed,
	)
	.unwrap();
	assert_eq!(hosted.latency(), 64);
}

#[test]
fn state_round_trips_through_the_sandbox() {
	let db = |x: f32| 10f32.powf(x / 20.0);
	let mut a = sandboxed(GAIN_ID, None);
	a.set_param(0, -12.0);
	let state = a.save_state().expect("gain has state");
	assert_eq!(a.monitor().last_state().as_deref(), Some(&state[..]));

	// Sandboxed and in-process states are interchangeable.
	let mut b = instantiate(descriptor(GAIN_ID), SR, BLOCK, Some(&state)).unwrap();
	let mut c = sandboxed(GAIN_ID, Some(&state));
	let mut d = sandboxed(GAIN_ID, None);
	assert!(d.load_state(&state));
	assert!(!d.load_state(b"garbage!"));
	for p in [b.as_mut(), &mut c as &mut dyn Processor, &mut d] {
		let mut l = vec![1.0f32; 500];
		let mut r = vec![1.0f32; 500];
		p.process(&mut l, &mut r, &[], &transport(0.0));
		assert!((l[499] - db(-12.0)).abs() < 1e-5, "{}", l[499]);
	}

	match instantiate_sandboxed(
		descriptor(GAIN_ID),
		SR,
		BLOCK,
		Some(b"bad"),
		ProcessMode::Realtime,
		sandbox_options(),
	) {
		Err(PluginError::State { id, .. }) => assert_eq!(id, GAIN_ID),
		Err(e) => panic!("unexpected error {e}"),
		Ok(_) => panic!("bad state accepted"),
	}
}

fn crash_param(name: &str) -> usize {
	crash_descriptor()
		.param_index(
			&crash_descriptor()
				.params
				.iter()
				.find(|p| p.name == name)
				.unwrap()
				.id,
		)
		.unwrap()
}

fn process_ones(p: &mut dyn Processor, n: usize) -> (Vec<f32>, Duration) {
	let mut l = vec![1.0f32; n];
	let mut r = vec![-1.0f32; n];
	let started = Instant::now();
	p.process(&mut l, &mut r, &[], &transport(0.0));
	(l, started.elapsed())
}

#[test]
fn a_crashing_plugin_is_isolated_bypassed_and_restartable() {
	let desc = crash_descriptor();
	assert_eq!(desc.id, CRASH_ID);
	let (crash, gain) = (crash_param("crash_now"), crash_param("gain"));
	let mut p = instantiate_sandboxed(
		desc,
		SR,
		256,
		None,
		ProcessMode::Realtime,
		sandbox_options(),
	)
	.unwrap();
	let monitor = p.monitor();
	let pid = p.pid().unwrap();

	let (out, _) = process_ones(&mut p, 256);
	assert_eq!(out[0], 0.5, "processed by the plugin");
	p.set_param(gain, 0.25);
	let state = p.save_state().unwrap();
	let (out, _) = process_ones(&mut p, 256);
	assert_eq!(out[0], 0.25);

	p.set_param(crash, 1.0);
	let (out, took) = process_ones(&mut p, 256);
	// We are still alive; the effect passed audio through.
	assert!(
		out.iter().all(|&x| x == 1.0),
		"effect bypass passes through"
	);
	assert!(took < Duration::from_millis(600), "detected in {took:?}");
	assert!(monitor.is_crashed());
	let info = p.crashed().expect("crash reported");
	assert_eq!(info.plugin_id, CRASH_ID);
	match &info.reason {
		CrashReason::Signal { name, .. } => assert_eq!(name, "SIGABRT"),
		other => panic!("unexpected reason {other:?}"),
	}
	assert!(
		info.message
			.contains("'Cadenza Test Crash' crashed (SIGABRT)"),
		"{}",
		info.message
	);
	assert_eq!(monitor.crashed(), Some(info.clone()));

	// Bypassed from now on, without waiting on the dead child.
	let (out, took) = process_ones(&mut p, 256);
	assert!(out.iter().all(|&x| x == 1.0));
	assert!(took < Duration::from_millis(1), "bypass took {took:?}");
	p.reset();
	p.prepare(SR, 256);
	assert_eq!(
		p.save_state().as_deref(),
		Some(&state[..]),
		"last good state"
	);
	assert_eq!(p.latency(), 0);

	// Restart restores the saved state (gain 0.25) in a new process.
	p.restart().unwrap();
	assert_ne!(p.pid(), Some(pid));
	assert!(p.crashed().is_none() && !monitor.is_crashed());
	p.set_param(crash, 0.0);
	let (out, _) = process_ones(&mut p, 256);
	assert_eq!(out[0], 0.25, "state restored after restart");
}

#[test]
fn a_hanging_plugin_times_out_in_realtime_but_not_offline() {
	let desc = crash_descriptor();
	let hang = crash_param("hang_ms");

	let mut p = instantiate_sandboxed(desc, SR, 64, None, ProcessMode::Realtime, sandbox_options())
		.unwrap();
	process_ones(&mut p, 64); // warm-up block
	p.set_param(hang, 300.0);
	let (out, took) = process_ones(&mut p, 64);
	assert!(out.iter().all(|&x| x == 1.0), "bypassed after the timeout");
	assert!(
		took < Duration::from_millis(50),
		"audio thread blocked {took:?}"
	);
	let info = p.crashed().expect("hang reported");
	assert!(
		matches!(info.reason, CrashReason::TimedOut { millis } if millis <= 3),
		"{info:?}"
	);
	assert!(info.message.contains("hung"), "{}", info.message);
	// The hung child was killed, so restarting works immediately.
	p.restart().unwrap();
	p.set_param(hang, 0.0);
	assert_eq!(process_ones(&mut p, 64).0[0], 0.5);

	// Offline renders wait for slow plugins.
	let mut p =
		instantiate_sandboxed(desc, SR, 64, None, ProcessMode::Offline, sandbox_options()).unwrap();
	p.set_param(hang, 100.0);
	let (out, took) = process_ones(&mut p, 64);
	assert!(took >= Duration::from_millis(100));
	assert_eq!(out[0], 0.5);
	assert!(p.crashed().is_none());
}

#[test]
fn an_idle_dead_child_is_noticed_and_instruments_go_silent() {
	let mut p = sandboxed(SINE_ID, None);
	let monitor = p.monitor();
	assert!(monitor.crashed().is_none());
	#[cfg(unix)]
	// SAFETY: killing our own sandbox child.
	unsafe {
		libc::kill(p.pid().unwrap() as i32, libc::SIGTERM);
	}
	let started = Instant::now();
	while monitor.crashed().is_none() {
		assert!(started.elapsed() < Duration::from_secs(5));
		std::thread::sleep(Duration::from_millis(5));
	}
	let info = monitor.crashed().unwrap();
	assert!(matches!(&info.reason, CrashReason::Signal { name, .. } if name == "SIGTERM"));
	assert!(info.message.contains("outputs silence"), "{}", info.message);
	let mut l = vec![0.3f32; 128];
	let mut r = vec![0.3f32; 128];
	p.process(&mut l, &mut r, &[], &transport(0.0));
	assert!(l.iter().chain(&r).all(|&x| x == 0.0));
}

#[cfg(target_os = "linux")]
#[test]
fn dropped_instances_leave_no_processes_or_shared_memory() {
	// Checks this test's own shared memory names (other tests running in
	// parallel create and unlink theirs meanwhile).
	let (pids, names): (Vec<u32>, Vec<String>) = (0..3)
		.map(|_| {
			let p = sandboxed(GAIN_ID, None);
			let pid = p.pid().unwrap();
			let name = p.shm_name().unwrap();
			drop(p);
			(pid, name)
		})
		.unzip();
	let started = Instant::now();
	for pid in pids {
		// Reaped: no /proc entry (not even a zombie).
		while std::path::Path::new(&format!("/proc/{pid}")).exists() {
			assert!(
				started.elapsed() < Duration::from_secs(5),
				"sandbox {pid} not reaped"
			);
			std::thread::sleep(Duration::from_millis(10));
		}
	}
	for name in names {
		let path = std::path::Path::new("/dev/shm").join(name.trim_start_matches('/'));
		assert!(!path.exists(), "shared memory {name} left behind");
	}
}

#[test]
fn missing_or_broken_sandbox_executables_fail_cleanly() {
	let opts = SandboxOptions {
		executable: Some("/definitely/not/here/cadenza-plugin-sandbox".into()),
		..sandbox_options()
	};
	let err = instantiate_sandboxed(
		descriptor(GAIN_ID),
		SR,
		BLOCK,
		None,
		ProcessMode::Realtime,
		opts,
	)
	.err()
	.unwrap();
	assert!(matches!(err, PluginError::Sandbox { .. }), "{err}");

	let mut missing = descriptor(GAIN_ID).clone();
	missing.id = "dev.cadenza.test.missing".into();
	let err = instantiate_sandboxed(
		&missing,
		SR,
		BLOCK,
		None,
		ProcessMode::Realtime,
		sandbox_options(),
	)
	.err()
	.unwrap();
	assert!(
		err.to_string().contains("dev.cadenza.test.missing"),
		"{err}"
	);
	assert!(sandbox_exe().is_file());
}

/// Per-block round-trip overhead of sandboxing (gain plugin, realtime mode).
/// `cargo test -p cadenza-plugin-host --release --test sandbox -- --ignored --nocapture`
#[test]
#[ignore = "benchmark"]
fn bench_round_trip_overhead() {
	let desc = descriptor(GAIN_ID);
	println!(
		"| frames | in-process µs/block | sandboxed µs/block | overhead µs | p99 sandboxed µs | cold sandboxed µs |"
	);
	for frames in [32usize, 64, 256, 512] {
		let mut local =
			instantiate_with_mode(desc, SR, frames, None, ProcessMode::Realtime).unwrap();
		let mut remote = instantiate_sandboxed(
			desc,
			SR,
			frames,
			None,
			ProcessMode::Realtime,
			SandboxOptions {
				realtime_timeout: Duration::from_millis(50),
				..sandbox_options()
			},
		)
		.unwrap();
		let mut l = vec![0.1f32; frames];
		let mut r = vec![0.1f32; frames];
		let t = transport(0.0);
		let blocks = 20_000;
		let mut measure = |p: &mut dyn Processor| {
			for _ in 0..500 {
				p.process(&mut l, &mut r, &[], &t);
			}
			let mut times = Vec::with_capacity(blocks);
			for _ in 0..blocks {
				let s = Instant::now();
				p.process(&mut l, &mut r, &[], &t);
				times.push(s.elapsed().as_secs_f64() * 1e6);
			}
			let mean = times.iter().sum::<f64>() / blocks as f64;
			times.sort_by(f64::total_cmp);
			(mean, times[blocks * 99 / 100])
		};
		let (a, _) = measure(local.as_mut());
		let (b, p99) = measure(&mut remote);
		// Cold: the child went back to sleep on its futex (1 ms idle), as
		// for the first block of every audio callback.
		let mut cold = Vec::with_capacity(500);
		for _ in 0..500 {
			std::thread::sleep(Duration::from_millis(1));
			let s = Instant::now();
			remote.process(&mut l, &mut r, &[], &t);
			cold.push(s.elapsed().as_secs_f64() * 1e6);
		}
		let cold_mean = cold.iter().sum::<f64>() / cold.len() as f64;
		assert!(remote.crashed().is_none());
		println!(
			"| {frames:>6} | {a:>19.2} | {b:>18.2} | {:>11.2} | {p99:>16.2} | {cold_mean:>17.2} |",
			b - a
		);
	}
}
