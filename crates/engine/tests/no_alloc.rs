//! The graph's process path (shared by offline and realtime) never touches the heap.

mod common;

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};

use cadenza_engine::compile;
use common::*;

struct CountingAlloc;

static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

thread_local! {
	static TRACKING: Cell<bool> = const { Cell::new(false) };
}

fn record() {
	if TRACKING.try_with(Cell::get).unwrap_or(false) {
		ALLOCATIONS.fetch_add(1, Ordering::SeqCst);
	}
}

unsafe impl GlobalAlloc for CountingAlloc {
	unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
		record();
		unsafe { System.alloc(layout) }
	}

	unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
		record();
		unsafe { System.alloc_zeroed(layout) }
	}

	unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
		record();
		unsafe { System.realloc(ptr, layout, new_size) }
	}

	unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
		record();
		unsafe { System.dealloc(ptr, layout) }
	}
}

#[global_allocator]
static GLOBAL: CountingAlloc = CountingAlloc;

#[test]
fn graph_process_does_not_allocate() {
	let mut p = mini_song();
	// Key a compressor on the last track from the first one (pre-fader), so sidechain
	// delivery is covered too.
	let source = p.tracks[0].id;
	let last = p.tracks.len() - 1;
	p.tracks[last]
		.channel
		.inserts
		.push(cadenza_model::Device::builtin("compressor", "Duck").with_sidechain(source, true));
	let mut graph = compile(&p, 48_000, 1024).unwrap();
	assert!(graph.warnings().is_empty(), "{:?}", graph.warnings());
	let mut left = vec![0.0f32; 1024];
	let mut right = vec![0.0f32; 1024];
	graph.play();
	assert!(graph.set_monitor_track(Some(source)));

	TRACKING.with(|t| t.set(true));
	for (i, block) in [1usize, 37, 512, 1024]
		.into_iter()
		.cycle()
		.take(400)
		.enumerate()
	{
		// Live MIDI input (monitoring), including bursts beyond the buffer.
		let pitch = 48 + (i % 24) as u8;
		for _ in 0..if i % 50 == 0 { 600 } else { 1 } {
			graph.push_live_event(cadenza_dsp::MidiEvent::NoteOn {
				offset: 0,
				pitch,
				velocity: 0.7,
			});
		}
		graph.push_live_event(cadenza_dsp::MidiEvent::NoteOff { offset: 0, pitch });
		if i == 200 {
			graph.set_monitor_track(None);
		}
		if i == 250 {
			graph.set_monitor_track(Some(source));
		}
		graph.process(&mut left[..block], &mut right[..block]);
	}
	graph.seek(beats(3.0));
	graph.process(&mut left, &mut right);
	graph.stop();
	graph.process(&mut left, &mut right);
	graph.play();
	graph.process(&mut left, &mut right);
	TRACKING.with(|t| t.set(false));

	assert_eq!(ALLOCATIONS.load(Ordering::SeqCst), 0);
}

#[test]
fn metering_does_not_allocate() {
	let p = mini_song();
	let mut graph = compile(&p, 48_000, 1024).unwrap();
	let meters = graph.meters();
	let ids: Vec<_> = p.tracks.iter().map(|t| t.id).collect();
	let mut left = vec![0.0f32; 1024];
	let mut right = vec![0.0f32; 1024];
	graph.play();

	TRACKING.with(|t| t.set(true));
	let mut loudest = 0.0f32;
	for block in [1usize, 64, 333, 1024].into_iter().cycle().take(200) {
		graph.process(&mut left[..block], &mut right[..block]);
		// The UI side reads (and resets) the levels between callbacks.
		loudest = loudest.max(meters.master().max_peak());
		for &id in &ids {
			let _ = meters.track(id);
		}
	}
	graph.stop();
	graph.process(&mut left, &mut right);
	TRACKING.with(|t| t.set(false));

	assert_eq!(ALLOCATIONS.load(Ordering::SeqCst), 0);
	assert!(loudest > 0.0, "meters saw the song");
	assert_eq!(meters.master().rms, [0.0, 0.0]);
}
