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
	let p = mini_song();
	let mut graph = compile(&p, 48_000, 1024).unwrap();
	let mut left = vec![0.0f32; 1024];
	let mut right = vec![0.0f32; 1024];
	graph.play();

	TRACKING.with(|t| t.set(true));
	for block in [1usize, 37, 512, 1024].into_iter().cycle().take(400) {
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
