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

/// Engine-like use: blocks of `sizes` (cycled) with notes, automation and
/// transport, returning the left channel.
fn quanta(p: &mut dyn Processor, sizes: &[usize], blocks: usize) -> Vec<f32> {
	let mut out = Vec::new();
	let mut seed = 11u32;
	let mut frames = 0;
	for b in 0..blocks {
		let n = sizes[b % sizes.len()];
		let mut l = vec![0.0f32; n];
		let mut r = vec![0.0f32; n];
		for (x, y) in l.iter_mut().zip(r.iter_mut()) {
			seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
			*x = (seed >> 9) as f32 / (1u32 << 23) as f32 - 0.5;
			*y = *x;
		}
		let last = n as u32 - 1;
		let on = [MidiEvent::NoteOn {
			offset: 5.min(last),
			pitch: 64,
			velocity: 0.9,
		}];
		let off = [MidiEvent::NoteOff {
			offset: last,
			pitch: 64,
		}];
		let events: &[MidiEvent] = match b % 9 {
			1 => &on,
			6 => &off,
			_ => &[],
		};
		if b % 3 == 0 {
			p.set_param(0, -((b % 13) as f32));
		}
		let beat = frames as f64 / f64::from(SR) * 2.0;
		p.process(&mut l, &mut r, events, &transport(beat));
		out.extend_from_slice(&l);
		frames += n;
	}
	out
}

#[test]
fn batched_blocks_match_in_process_output_delayed_by_the_reported_latency() {
	// (max_block, chunk, block sizes, added latency): the engine's quanta, and
	// irregular blocks up to max_block.
	let cases: [(usize, usize, &[usize], usize); 2] = [
		(32, 128, &[32], 96),
		(64, 256, &[1, 7, 64, 32, 50, 13, 64], 192),
	];
	for (max_block, chunk, sizes, added) in cases {
		for id in [GAIN_ID, SINE_ID] {
			let mut local = instantiate(descriptor(id), SR, max_block, None).unwrap();
			let opts = SandboxOptions {
				realtime_chunk: chunk,
				..sandbox_options()
			};
			let mut remote = instantiate_sandboxed(
				descriptor(id),
				SR,
				max_block,
				None,
				ProcessMode::Realtime,
				opts,
			)
			.unwrap();
			let extra = (remote.latency() - local.latency()) as usize;
			assert_eq!(extra, added, "{id}: batching adds chunk - max_block");
			let a = quanta(local.as_mut(), sizes, 400);
			let b = quanta(&mut remote, sizes, 400);
			assert!(a.iter().any(|&x| x != 0.0), "{id} produced silence");
			assert!(b[..extra].iter().all(|&x| x == 0.0));
			assert_eq!(
				&a[..a.len() - extra],
				&b[extra..],
				"{id}/{max_block}: not the same audio"
			);
			assert_eq!(remote.monitor().stats(), Default::default());

			// A reset empties the pipeline: same output again after it.
			local.reset();
			remote.reset();
			let a = quanta(local.as_mut(), sizes, 100);
			let b = quanta(&mut remote, sizes, 100);
			assert_eq!(&a[..a.len() - extra], &b[extra..], "{id} after reset");
			assert!(remote.crashed().is_none());
		}
	}
}

#[test]
fn isolated_late_blocks_drop_audio_but_a_sustained_stall_is_a_hang() {
	const Q: usize = 32;
	let desc = crash_descriptor();
	let (hang, every) = (crash_param("hang_ms"), crash_param("stall_every"));
	let opts = SandboxOptions {
		hang_timeout: Duration::from_millis(500),
		..sandbox_options()
	};
	let mut p = instantiate_sandboxed(desc, SR, Q, None, ProcessMode::Realtime, opts).unwrap();
	let monitor = p.monitor();
	let latency = p.latency() as usize;
	assert_eq!(latency, 96);
	// Every 400th plugin call sleeps 20 ms: ten times the deadline, far less
	// than the hang timeout.
	p.set_param(hang, 20.0);
	p.set_param(every, 400.0);
	let mut input = Vec::new();
	let mut output = Vec::new();
	let started = Instant::now();
	let mut i = 0u32;
	while started.elapsed() < Duration::from_millis(1500) {
		let mut l = [0.0f32; Q];
		for x in &mut l {
			i += 1;
			*x = (i % 1000) as f32 / 1000.0 + 0.001;
		}
		let mut r = l;
		input.extend_from_slice(&l);
		let t = transport(0.0);
		p.process(&mut l, &mut r, &[], &t);
		output.extend_from_slice(&l);
		std::thread::sleep(Duration::from_micros(200));
	}
	assert!(p.crashed().is_none(), "isolated stalls are not a hang");
	let stats = monitor.stats();
	assert!(stats.late_blocks >= 3, "{stats:?}");
	assert!(stats.longest_stall_ms < 100, "{stats:?}");
	// Every frame is either processed (gain 0.5) or the dry input, in its
	// place: a late answer never leaks into a later block.
	let (mut processed, mut dry) = (0, 0);
	for (t, &y) in output.iter().enumerate().skip(latency) {
		let x = input[t - latency];
		if y == x * 0.5 {
			processed += 1;
		} else if y == x {
			dry += 1;
		} else {
			panic!("frame {t}: {y} is neither {x} nor {}", x * 0.5);
		}
	}
	// The last dropped frames may still wait in the latency FIFO.
	let dropped = stats.dropped_frames as usize;
	assert!(
		(dropped.saturating_sub(latency)..=dropped).contains(&dry),
		"{dry} dry frames, {stats:?}"
	);
	assert!(processed > 3 * dry, "processed {processed}, dry {dry}");

	// Every call stalling: no on-time answer any more, hung after 500 ms.
	p.set_param(every, 1.0);
	let started = Instant::now();
	while !monitor.is_crashed() {
		assert!(started.elapsed() < Duration::from_secs(5), "never hung");
		let (_, took) = process_ones(&mut p, Q);
		assert!(took < Duration::from_millis(30), "blocked {took:?}");
		std::thread::sleep(Duration::from_millis(1));
	}
	assert!(started.elapsed() >= Duration::from_millis(450));
	assert!(matches!(
		p.crashed().unwrap().reason,
		CrashReason::TimedOut { .. }
	));
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
fn a_hanging_plugin_is_late_first_and_hung_after_a_sustained_stall() {
	let desc = crash_descriptor();
	let hang = crash_param("hang_ms");

	let opts = SandboxOptions {
		hang_timeout: Duration::from_millis(300),
		realtime_chunk: 0,
		..sandbox_options()
	};
	let mut p = instantiate_sandboxed(desc, SR, 64, None, ProcessMode::Realtime, opts).unwrap();
	let monitor = p.monitor();
	process_ones(&mut p, 64); // warm-up block
	p.set_param(hang, 1000.0);
	let (out, took) = process_ones(&mut p, 64);
	assert!(out.iter().all(|&x| x == 1.0), "a late block passes through");
	assert!(
		took < Duration::from_millis(50),
		"audio thread blocked {took:?}"
	);
	assert!(p.crashed().is_none(), "one late block is not a hang");
	assert_eq!(monitor.stats().late_blocks, 1);
	// The child is still busy: later blocks are skipped without waiting.
	let (out, took) = process_ones(&mut p, 64);
	assert!(out.iter().all(|&x| x == 1.0));
	assert!(took < Duration::from_millis(1), "skipping took {took:?}");
	assert_eq!(monitor.stats().late_blocks, 1, "skipped, not late");
	assert_eq!(monitor.stats().dropped_frames, 128);

	let started = Instant::now();
	while !monitor.is_crashed() {
		assert!(started.elapsed() < Duration::from_secs(5), "never hung");
		let (out, took) = process_ones(&mut p, 64);
		assert!(out.iter().all(|&x| x == 1.0));
		assert!(took < Duration::from_millis(50), "blocked {took:?}");
		std::thread::sleep(Duration::from_millis(2));
	}
	let info = p.crashed().expect("hang reported");
	assert!(
		matches!(info.reason, CrashReason::TimedOut { millis } if (300..1000).contains(&millis)),
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

/// Round-trip overhead of sandboxing (gain plugin, realtime mode), per 512
/// frames (one audio callback) processed in blocks of `block` frames, with
/// and without batching blocks into chunks of `chunk` frames per round trip.
/// "cold" sleeps 1 ms between callbacks, so the child is back asleep on its
/// futex as at the start of every real callback.
/// `cargo test -p cadenza-plugin-host --release --test sandbox -- --ignored --nocapture bench`
#[test]
#[ignore = "benchmark"]
fn bench_round_trip_overhead() {
	const CALLBACK: usize = 512;
	let desc = descriptor(GAIN_ID);
	println!(
		"| block | chunk | added latency | in-process µs/512 | sandboxed µs/512 | overhead µs | p99 sandboxed µs | cold sandboxed µs |"
	);
	println!("|---|---|---|---|---|---|---|---|");
	for (block, chunk) in [
		(32usize, 0usize),
		(32, 128),
		(32, 256),
		(64, 0),
		(256, 0),
		(512, 0),
	] {
		let mut local =
			instantiate_with_mode(desc, SR, block, None, ProcessMode::Realtime).unwrap();
		let mut remote = instantiate_sandboxed(
			desc,
			SR,
			block,
			None,
			ProcessMode::Realtime,
			SandboxOptions {
				realtime_timeout: Duration::from_millis(50),
				realtime_chunk: chunk,
				..sandbox_options()
			},
		)
		.unwrap();
		let added = remote.latency() - local.latency();
		let mut l = vec![0.1f32; block];
		let mut r = vec![0.1f32; block];
		let t = transport(0.0);
		let mut callback = |p: &mut dyn Processor| {
			let s = Instant::now();
			for _ in 0..CALLBACK / block {
				p.process(&mut l, &mut r, &[], &t);
			}
			s.elapsed().as_secs_f64() * 1e6
		};
		// Cold first: the hot loop below never lets the child block, so a
		// realtime-promoted child is demoted during it (RLIMIT_RTTIME).
		let cold: Vec<f64> = (0..300)
			.map(|_| {
				std::thread::sleep(Duration::from_millis(1));
				callback(&mut remote)
			})
			.collect();
		let cold_mean = cold.iter().sum::<f64>() / cold.len() as f64;
		let callbacks = 4_000;
		let mut measure = |p: &mut dyn Processor| {
			for _ in 0..100 {
				callback(p);
			}
			let mut times: Vec<f64> = (0..callbacks).map(|_| callback(p)).collect();
			let mean = times.iter().sum::<f64>() / callbacks as f64;
			times.sort_by(f64::total_cmp);
			(mean, times[callbacks * 99 / 100])
		};
		let (a, _) = measure(local.as_mut());
		let (b, p99) = measure(&mut remote);
		assert!(remote.crashed().is_none(), "{:?}", remote.crashed());
		assert_eq!(remote.monitor().stats().late_blocks, 0);
		println!(
			"| {block} | {chunk} | {added} | {a:.2} | {b:.2} | {:.2} | {p99:.2} | {cold_mean:.2} |",
			b - a
		);
	}
}
