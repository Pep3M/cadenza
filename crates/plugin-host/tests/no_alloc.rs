//! Verifies that hosted CLAP and VST3 plugins' `process`, `set_param` and
//! `reset` never touch the host heap (the test plugins' own allocators are
//! separate and not counted).

mod common;

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};

use cadenza_dsp::{MidiEvent, Transport};
use cadenza_plugin_host::PluginDescriptor;
use cadenza_plugin_host::instantiate;
use cadenza_plugin_host::scanner::scan_in_subprocess;
use common::{GAIN_ID, SINE_ID, descriptor, scanner, vst3_bundle};

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
fn process_and_set_param_never_allocate() {
	let (probe, n) = counted(|| Vec::<u8>::with_capacity(16));
	assert!(n > 0 && probe.capacity() >= 16);

	let max_block = 512;
	let mut left = vec![0.0_f32; max_block];
	let mut right = vec![0.0_f32; max_block];
	let mut events: Vec<MidiEvent> = (0..2000)
		.map(|i| MidiEvent::NoteOn {
			offset: (i % 64) as u32,
			pitch: 40 + (i % 40) as u8,
			velocity: 0.7,
		})
		.collect();
	events.sort_by_key(MidiEvent::offset);
	events.push(MidiEvent::AllNotesOff { offset: 100 });
	let transport = Transport {
		bpm: 97.0,
		playing: true,
		beat_position: 3.5,
		..Default::default()
	};

	let vst3: Vec<PluginDescriptor> = scan_in_subprocess(
		&scanner(),
		vst3_bundle(),
		std::time::Duration::from_secs(30),
	)
	.unwrap();
	let descs = [descriptor(GAIN_ID), descriptor(SINE_ID), &vst3[0]];
	for desc in descs {
		let id = &desc.id;
		let mut device = instantiate(desc, 48_000.0, max_block, None).unwrap();
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
					device.set_param(
						i,
						if round % 2 == 0 {
							p.max as f32
						} else {
							p.min as f32
						},
					);
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
		assert_eq!(total, 0, "{id} allocated {total} times on the audio thread");
	}
}
