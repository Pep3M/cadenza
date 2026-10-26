//! Frame-parallel helper: analysis frames are independent, so long takes are cut into one
//! contiguous chunk per core. The calling thread works on the first chunk and reports
//! progress from it (chunks are equal, so it tracks the whole); cancelling stops every chunk.

use std::cell::{Cell, RefCell};
use std::ops::Range;
use std::sync::atomic::{AtomicBool, Ordering};

/// Below this many frames one thread does it all.
const MIN_PARALLEL: usize = 4_096;

thread_local! {
	/// Tests pin the thread count to compare chunked and single-threaded results.
	static THREADS: Cell<Option<usize>> = const { Cell::new(None) };
}

/// Runs `f` with `map_frames` using exactly `threads` threads (when there are enough frames).
#[cfg(test)]
pub(crate) fn with_threads<R>(threads: usize, f: impl FnOnce() -> R) -> R {
	THREADS.with(|t| t.set(Some(threads)));
	let r = f();
	THREADS.with(|t| t.set(None));
	r
}

/// Runs `work` over `0..count` and concatenates the chunks in order. `work` gets its range
/// and a `tick(done)` to call every few frames; when it returns false, `work` should
/// return `None`. `None` overall = cancelled.
pub(crate) fn map_frames<T: Send>(
	count: usize,
	progress: &mut dyn FnMut(f64) -> bool,
	work: impl Fn(Range<usize>, &dyn Fn(usize) -> bool) -> Option<Vec<T>> + Sync,
) -> Option<Vec<T>> {
	let threads = match THREADS.with(Cell::get) {
		Some(n) => n.min(count).max(1),
		None => std::thread::available_parallelism()
			.map_or(1, |n| n.get())
			.min(8)
			.min(count / MIN_PARALLEL)
			.max(1),
	};
	let chunk = count.div_ceil(threads);
	let cancelled = AtomicBool::new(false);
	let progress = RefCell::new(progress);
	let first = 0..chunk.min(count);
	let main_tick = |done: usize| {
		let keep = (progress.borrow_mut())(done as f64 / first.len().max(1) as f64);
		if !keep {
			cancelled.store(true, Ordering::Relaxed);
		}
		keep && !cancelled.load(Ordering::Relaxed)
	};
	let worker_tick = |_: usize| !cancelled.load(Ordering::Relaxed);
	let parts: Vec<Option<Vec<T>>> = std::thread::scope(|scope| {
		let handles: Vec<_> = (1..threads)
			.map(|t| {
				let range = (t * chunk).min(count)..((t + 1) * chunk).min(count);
				let (work, tick) = (&work, &worker_tick);
				scope.spawn(move || work(range, tick))
			})
			.collect();
		let mut parts = vec![work(first.clone(), &main_tick)];
		if parts[0].is_none() {
			cancelled.store(true, Ordering::Relaxed);
		}
		parts.extend(
			handles
				.into_iter()
				.map(|h| h.join().expect("analysis thread panicked")),
		);
		parts
	});
	if cancelled.load(Ordering::Relaxed) {
		return None;
	}
	let mut out = Vec::with_capacity(count);
	for part in parts {
		out.extend(part?);
	}
	(progress.into_inner())(1.0);
	Some(out)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn chunks_come_back_in_order_and_cancel_stops_all() {
		let count = 50_000;
		let square = |r: Range<usize>, tick: &dyn Fn(usize) -> bool| {
			let start = r.start;
			let mut out = Vec::with_capacity(r.len());
			for i in r {
				if (i - start).is_multiple_of(100) && !tick(i - start) {
					return None;
				}
				out.push(i * 2);
			}
			Some(out)
		};
		let all = map_frames(count, &mut |_| true, square).unwrap();
		assert_eq!(all, (0..count).map(|i| i * 2).collect::<Vec<_>>());
		assert!(map_frames(count, &mut |p| p < 0.2, square).is_none());
	}
}
