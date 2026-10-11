//! Verifies that a hosted VST3 plugin's `process`, `set_param` and `reset`
//! never touch the host (Rust) heap. The plugin's own C++ allocations go
//! through libc directly and are not counted. Ignored by default (needs the
//! LSP VST3 bundle): run with `-- --ignored`.

mod common;

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use cadenza_dsp::{MidiEvent, Transport};
use cadenza_plugin_host::instantiate;
use cadenza_plugin_host::scanner::scan_in_subprocess;

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

const LSP: &str = "/usr/lib/vst3/lsp-plugins.vst3";

#[test]
#[ignore = "needs the LSP VST3 bundle in /usr/lib/vst3"]
fn vst3_process_and_set_param_never_allocate() {
	if !Path::new(LSP).exists() {
		println!("LSP VST3 not installed; skipping");
		return;
	}
	let (probe, n) = counted(|| Vec::<u8>::with_capacity(16));
	assert!(n > 0 && probe.capacity() >= 16);

	let descs = scan_in_subprocess(&common::scanner(), Path::new(LSP), Duration::from_secs(120))
		.expect("scan LSP");
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
	};

	for name in ["Parametric Equalizer x8 Stereo", "Compressor Stereo"] {
		let desc = descs.iter().find(|d| d.name == name).expect(name);
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
					if p.bypass {
						continue;
					}
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
		assert_eq!(
			total, 0,
			"{name} allocated {total} times on the audio thread"
		);
		println!(
			"{name}: 0 host allocations across {} params",
			desc.params.len()
		);
	}
}
