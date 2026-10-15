//! Cross-process wait/wake on a 32-bit word in shared memory.
//!
//! - **Linux**: `futex(FUTEX_WAIT/FUTEX_WAKE)` without `FUTEX_PRIVATE_FLAG`,
//!   so it works across processes mapping the same memory. No allocation and
//!   no locks; safe to call from the audio thread.
//! - **macOS 14.4+**: `os_sync_wait_on_address_with_timeout` /
//!   `os_sync_wake_by_address_all` with the `SHARED` flags (the same idea),
//!   looked up at runtime with `dlsym` ([`prepare`] resolves them before the
//!   audio thread needs them) so older systems still start.
//! - **Older macOS and other Unix (BSD)**: waiting spins with `yield` and then
//!   sleeps in 50 µs steps until the word changes or the timeout expires;
//!   waking is a no-op. Correct but costs latency and some CPU.
//! - **Windows**: not used (the sandbox is Unix-only; `WaitOnAddress` is
//!   process-local, named events would be needed).

use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

/// Busy-polls `word` until it differs from `current` for at most `spins`
/// iterations. Returns whether it changed.
pub(crate) fn spin_until_changed(word: &AtomicU32, current: u32, spins: u32) -> bool {
	for _ in 0..spins {
		if word.load(Ordering::Acquire) != current {
			return true;
		}
		std::hint::spin_loop();
	}
	word.load(Ordering::Acquire) != current
}

/// Sleeps while `word == current`, for at most `timeout`. May return early
/// (spuriously, on a signal, or when the word changed); callers re-check.
#[cfg(target_os = "linux")]
pub(crate) fn wait(word: &AtomicU32, current: u32, timeout: Duration) {
	let ts = libc::timespec {
		tv_sec: timeout.as_secs().min(i32::MAX as u64) as libc::time_t,
		tv_nsec: timeout.subsec_nanos() as libc::c_long,
	};
	// SAFETY: `word` is a valid, aligned u32 for the duration of the call;
	// FUTEX_WAIT only reads it. EINTR/EAGAIN/ETIMEDOUT are all handled by the
	// caller re-checking the word and its deadline.
	unsafe {
		libc::syscall(
			libc::SYS_futex,
			word.as_ptr(),
			libc::FUTEX_WAIT,
			current,
			&ts as *const libc::timespec,
			std::ptr::null::<u32>(),
			0u32,
		);
	}
}

/// Wakes every waiter on `word`.
#[cfg(target_os = "linux")]
pub(crate) fn wake(word: &AtomicU32) {
	// SAFETY: as in `wait`; FUTEX_WAKE does not access the word's contents.
	unsafe {
		libc::syscall(
			libc::SYS_futex,
			word.as_ptr(),
			libc::FUTEX_WAKE,
			i32::MAX,
			std::ptr::null::<libc::timespec>(),
			std::ptr::null::<u32>(),
			0u32,
		);
	}
}

/// Resolves the platform primitives up front so the first wait on the audio
/// thread does not run the dynamic linker. Idempotent and cheap.
pub(crate) fn prepare() {
	#[cfg(target_os = "macos")]
	let _ = mac::api();
}

#[cfg(target_os = "macos")]
mod mac {
	use std::sync::OnceLock;

	type WaitFn =
		unsafe extern "C" fn(*mut libc::c_void, u64, libc::size_t, u32, u32, u64) -> libc::c_int;
	type WakeFn = unsafe extern "C" fn(*mut libc::c_void, libc::size_t, u32) -> libc::c_int;

	pub(super) struct Api {
		pub wait: WaitFn,
		pub wake: WakeFn,
	}

	/// `os_sync_*` (macOS 14.4+), or `None` on older systems.
	pub(super) fn api() -> Option<&'static Api> {
		static API: OnceLock<Option<Api>> = OnceLock::new();
		API.get_or_init(|| {
			// SAFETY: dlsym with RTLD_DEFAULT on NUL-terminated names; the symbols,
			// when present, have exactly these C signatures (<os/os_sync_wait_on_address.h>).
			unsafe {
				let wait = libc::dlsym(
					libc::RTLD_DEFAULT,
					c"os_sync_wait_on_address_with_timeout".as_ptr(),
				);
				let wake = libc::dlsym(libc::RTLD_DEFAULT, c"os_sync_wake_by_address_all".as_ptr());
				if wait.is_null() || wake.is_null() {
					return None;
				}
				Some(Api {
					wait: std::mem::transmute::<*mut libc::c_void, WaitFn>(wait),
					wake: std::mem::transmute::<*mut libc::c_void, WakeFn>(wake),
				})
			}
		})
		.as_ref()
	}
}

#[cfg(target_os = "macos")]
pub(crate) fn wait(word: &AtomicU32, current: u32, timeout: Duration) {
	match mac::api() {
		// SAFETY: `word` is a valid, aligned u32 for the duration of the call;
		// the call only compares it. Timeouts and spurious wake-ups are handled
		// by the caller re-checking the word and its deadline.
		Some(api) => unsafe {
			(api.wait)(
				word.as_ptr().cast(),
				u64::from(current),
				4,
				libc::OS_SYNC_WAIT_ON_ADDRESS_SHARED,
				libc::OS_CLOCK_MACH_ABSOLUTE_TIME,
				timeout.as_nanos().clamp(1, u128::from(u64::MAX)) as u64,
			);
		},
		None => poll_wait(word, current, timeout),
	}
}

#[cfg(target_os = "macos")]
pub(crate) fn wake(word: &AtomicU32) {
	if let Some(api) = mac::api() {
		// SAFETY: as in `wait`; waking does not access the word's contents.
		unsafe {
			(api.wake)(
				word.as_ptr().cast(),
				4,
				libc::OS_SYNC_WAKE_BY_ADDRESS_SHARED,
			);
		}
	}
}

#[cfg(not(target_os = "linux"))]
fn poll_wait(word: &AtomicU32, current: u32, timeout: Duration) {
	let start = std::time::Instant::now();
	let mut round = 0u32;
	while word.load(Ordering::Acquire) == current && start.elapsed() < timeout {
		if round < 64 {
			std::thread::yield_now();
		} else {
			std::thread::sleep(Duration::from_micros(50).min(timeout));
		}
		round += 1;
	}
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) fn wait(word: &AtomicU32, current: u32, timeout: Duration) {
	poll_wait(word, current, timeout);
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) fn wake(_word: &AtomicU32) {}

#[cfg(test)]
mod tests {
	use super::*;
	use std::sync::Arc;
	use std::time::Instant;

	#[test]
	fn wait_times_out_and_wakes() {
		prepare();
		let word = Arc::new(AtomicU32::new(0));
		let start = Instant::now();
		wait(&word, 0, Duration::from_millis(20));
		assert!(start.elapsed() >= Duration::from_millis(15));
		// Wrong expected value returns immediately.
		let start = Instant::now();
		wait(&word, 1, Duration::from_secs(5));
		assert!(start.elapsed() < Duration::from_secs(1));

		let w = word.clone();
		let t = std::thread::spawn(move || {
			std::thread::sleep(Duration::from_millis(30));
			w.store(1, Ordering::Release);
			wake(&w);
		});
		let start = Instant::now();
		while word.load(Ordering::Acquire) == 0 {
			wait(&word, 0, Duration::from_secs(5));
		}
		assert!(start.elapsed() < Duration::from_secs(2));
		t.join().unwrap();
		assert!(spin_until_changed(&word, 0, 1));
	}
}
