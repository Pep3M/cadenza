//! Verifies that a sandboxed processor's audio-thread path (`process`,
//! `set_param`, `reset`, `latency`, `tail_seconds`) never touches the host
//! heap, including when the child crashes or hangs mid-stream. The child's
//! allocations happen in another process and are not counted.

// Plugin sandboxing is Unix-only (`SANDBOX_SUPPORTED`).
#![cfg(unix)]

mod common;

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};

use cadenza_dsp::{MidiEvent, Processor, Transport};
use cadenza_plugin_host::{ProcessMode, instantiate_sandboxed};
use common::{GAIN_ID, SINE_ID, crash_descriptor, descriptor, sandbox_options};

struct CountingAlloc;

static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

thread_local! {
	static TRACKING: Cell<bool> = const { Cell::new(false) };
}

fn note() {
	if TRACKING.try_with(Cell::get).unwrap_or(false) {
		ALLOCATIONS.fetch_add(1, Ordering::SeqCst);
	}
}

unsafe impl GlobalAlloc for CountingAlloc {
	unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
		note();
		unsafe { System.alloc(layout) }
	}

	unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
		note();
		unsafe { System.alloc_zeroed(layout) }
	}

	unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
		note();
		unsafe { System.realloc(ptr, layout, new_size) }
	}

	unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
		note();
		unsafe { System.dealloc(ptr, layout) }
	}
}

#[global_allocator]
static GLOBAL: CountingAlloc = CountingAlloc;

fn counted<R>(f: impl FnOnce() -> R) -> (R, usize) {
	let before = ALLOCATIONS.load(Ordering::SeqCst);
	TRACKING.with(|t| t.set(true));
	let r = f();
	TRACKING.with(|t| t.set(false));
	(r, ALLOCATIONS.load(Ordering::SeqCst) - before)
}

#[test]
fn sandboxed_audio_path_never_allocates() {
	let (probe, n) = counted(|| Vec::<u8>::with_capacity(16));
	assert!(n > 0 && probe.capacity() >= 16);

	let max_block = 512;
	let mut left = vec![0.0_f32; max_block];
	let mut right = vec![0.0_f32; max_block];
	let mut events: Vec<MidiEvent> = (0..3000)
		.map(|i| MidiEvent::NoteOn {
			offset: (i % 64) as u32,
			pitch: 40 + (i % 40) as u8,
			velocity: 0.7,
		})
		.collect();
	// Controllers too (CC as MIDI / note expressions / VST3 mapped params).
	events.extend([
		MidiEvent::ControlChange {
			offset: 3,
			controller: 64,
			value: 1.0,
		},
		MidiEvent::PitchBend {
			offset: 4,
			value: 0.5,
		},
		MidiEvent::ChannelPressure {
			offset: 5,
			value: 0.3,
		},
	]);
	events.sort_by_key(MidiEvent::offset);
	events.push(MidiEvent::AllNotesOff { offset: 100 });
	let transport = Transport {
		bpm: 97.0,
		playing: true,
		beat_position: 3.5,
		..Default::default()
	};

	for desc in [descriptor(GAIN_ID), descriptor(SINE_ID)] {
		let mut device = instantiate_sandboxed(
			desc,
			48_000.0,
			max_block,
			None,
			ProcessMode::Realtime,
			sandbox_options(),
		)
		.unwrap();
		let mut total = 0;
		for (round, &block) in [1usize, 7, 64, 333, 512, 128].iter().enumerate() {
			left.fill(0.25);
			right.fill(-0.25);
			let ev: &[MidiEvent] = match round {
				2 => &events,
				4 => &events[..3],
				_ => &[],
			};
			let ((), n) = counted(|| {
				for (i, p) in desc.params.iter().enumerate() {
					device.set_param(i, if round % 2 == 0 { p.max } else { p.min } as f32);
				}
				for _ in 0..4 {
					device.process(&mut left[..block], &mut right[..block], ev, &transport);
				}
				if round == 3 {
					device.reset();
				}
				let _ = device.latency();
				let _ = device.tail_seconds();
			});
			total += n;
		}
		assert!(device.crashed().is_none());
		assert_eq!(
			total, 0,
			"{} allocated {total} times on the audio thread",
			desc.id
		);
	}

	// Batched quanta (staging, sub-blocks, latency FIFO) as the engine runs them.
	for desc in [descriptor(GAIN_ID), descriptor(SINE_ID)] {
		let quantum = 32;
		let mut device = instantiate_sandboxed(
			desc,
			48_000.0,
			quantum,
			None,
			ProcessMode::Realtime,
			sandbox_options(),
		)
		.unwrap();
		assert!(device.latency() >= 96, "batched by default");
		let ((), n) = counted(|| {
			for q in 0..200 {
				let ev: &[MidiEvent] = if q % 5 == 0 { &events[..40] } else { &[] };
				device.set_param(0, (q % 7) as f32 - 6.0);
				device.process(&mut left[..quantum], &mut right[..quantum], ev, &transport);
				if q == 101 {
					device.reset();
				}
			}
		});
		assert!(device.crashed().is_none());
		assert_eq!(n, 0, "{}: batched path allocated {n} times", desc.id);
	}

	// Crash, late-block and hang handling on the audio thread are
	// allocation-free too.
	let crash = crash_descriptor();
	for (param, value) in [("crash_now", 1.0), ("hang_ms", 200.0)] {
		let index = crash.params.iter().position(|p| p.name == param).unwrap();
		let mut device = instantiate_sandboxed(
			crash,
			48_000.0,
			max_block,
			None,
			ProcessMode::Realtime,
			cadenza_plugin_host::SandboxOptions {
				hang_timeout: std::time::Duration::from_millis(50),
				..sandbox_options()
			},
		)
		.unwrap();
		device.process(&mut left, &mut right, &[], &transport);
		let monitor = device.monitor();
		let ((), n) = counted(|| {
			device.set_param(index, value);
			for _ in 0..200 {
				device.process(&mut left, &mut right, &[], &transport);
				if monitor.is_crashed() {
					break;
				}
				std::thread::sleep(std::time::Duration::from_millis(2));
			}
			device.reset();
		});
		assert!(monitor.is_crashed(), "{param}");
		assert_eq!(n, 0, "{param}: failure path allocated {n} times");
	}
}
