//! Realtime scheduling for the sandbox child's audio thread.
//!
//! Linux is described below. macOS uses the Mach time-constraint policy
//! (`thread_policy_set`, what Core Audio gives its own I/O threads) and Windows
//! `THREAD_PRIORITY_TIME_CRITICAL` (MMCSS only applies to the calling thread,
//! and promotion runs on a helper thread).
//!
//! A plugin process also runs the plugin's GUI and its own worker threads; at
//! normal priority any of them (or anything else on the desktop) can delay the
//! thread that answers the host's audio requests. The child therefore asks for
//! `SCHED_FIFO` the way audio applications do on Linux, failing gracefully:
//!
//! 1. `sched_setscheduler` directly (works with an `rtprio` limit, e.g. the
//!    `audio`/`realtime` groups of most audio-oriented distributions);
//! 2. RealtimeKit (`org.freedesktop.RealtimeKit1` on the system bus, what
//!    PipeWire, PulseAudio and JACK clients use on desktops), through
//!    `busctl` so the sandbox needs no D-Bus library;
//! 3. otherwise a negative nice value (directly, then through RealtimeKit).
//!
//! RealtimeKit requires `RLIMIT_RTTIME`, which also caps how long a realtime
//! thread may run without blocking. The soft limit is half of the hard one
//! and its `SIGXCPU` demotes the audio thread back to normal scheduling, so a
//! plugin spinning in `process` loses its priority instead of freezing a core
//! (the host's deadline logic then reports it hung as usual). All of this runs
//! once, before the audio loop starts.

/// How the audio thread ended up being scheduled (shown in diagnostics).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scheduling {
	Realtime {
		priority: u32,
		via: &'static str,
	},
	/// A platform realtime class without a numeric priority (macOS time
	/// constraint, Windows time-critical).
	TimeCritical {
		policy: &'static str,
		via: &'static str,
	},
	Nice {
		nice: i32,
		via: &'static str,
	},
	Normal {
		reason: String,
	},
}

impl std::fmt::Display for Scheduling {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			Scheduling::Realtime { priority, via } => write!(f, "SCHED_FIFO {priority} ({via})"),
			Scheduling::TimeCritical { policy, via } => write!(f, "{policy} ({via})"),
			Scheduling::Nice { nice, via } => write!(f, "nice {nice} ({via})"),
			Scheduling::Normal { reason } => write!(f, "normal priority ({reason})"),
		}
	}
}

#[cfg(target_os = "linux")]
mod imp {
	use std::io::Read;
	use std::process::{Command, Stdio};
	use std::sync::atomic::{AtomicI32, Ordering};
	use std::time::{Duration, Instant};

	use super::Scheduling;

	/// The promoted thread, for the `SIGXCPU` handler (0 = none).
	static AUDIO_TID: AtomicI32 = AtomicI32::new(0);

	const RTKIT: [&str; 3] = [
		"org.freedesktop.RealtimeKit1",
		"/org/freedesktop/RealtimeKit1",
		"org.freedesktop.RealtimeKit1",
	];

	fn set_fifo(tid: i32, priority: u32) -> bool {
		let param = libc::sched_param {
			sched_priority: priority as i32,
		};
		// SAFETY: plain syscall on our own thread with a valid parameter.
		unsafe {
			libc::sched_setscheduler(tid, libc::SCHED_FIFO | libc::SCHED_RESET_ON_FORK, &param) == 0
		}
	}

	fn set_nice(tid: i32, nice: i32) -> bool {
		// SAFETY: plain syscall; on Linux PRIO_PROCESS with a tid targets one thread.
		unsafe { libc::setpriority(libc::PRIO_PROCESS, tid as libc::id_t, nice) == 0 }
	}

	/// Runs `busctl --system <args>` with a deadline; its stdout on success.
	fn busctl(args: &[String], timeout: Duration) -> Option<String> {
		let mut child = Command::new("busctl")
			.arg("--system")
			.args(args)
			.stdin(Stdio::null())
			.stdout(Stdio::piped())
			.stderr(Stdio::null())
			.spawn()
			.ok()?;
		let deadline = Instant::now() + timeout;
		let status = loop {
			match child.try_wait() {
				Ok(Some(status)) => break status,
				Ok(None) if Instant::now() < deadline => {
					std::thread::sleep(Duration::from_millis(2))
				}
				_ => {
					let _ = child.kill();
					let _ = child.wait();
					return None;
				}
			}
		};
		let mut out = String::new();
		child.stdout.take()?.read_to_string(&mut out).ok()?;
		status.success().then_some(out)
	}

	/// A numeric RealtimeKit property (`busctl` prints e.g. `x 200000`).
	fn rtkit_property(name: &str) -> Option<i64> {
		let mut args: Vec<String> = vec!["get-property".into()];
		args.extend(RTKIT.iter().map(|s| s.to_string()));
		args.push(name.into());
		let out = busctl(&args, Duration::from_secs(1))?;
		out.split_whitespace().nth(1)?.parse().ok()
	}

	fn rtkit_call(method: &str, signature: &str, tid: i32, value: i64) -> bool {
		let mut args: Vec<String> = vec!["call".into()];
		args.extend(RTKIT.iter().map(|s| s.to_string()));
		args.extend([
			method.to_string(),
			signature.to_string(),
			std::process::id().to_string(),
			tid.to_string(),
			value.to_string(),
		]);
		busctl(&args, Duration::from_secs(2)).is_some()
	}

	extern "C" fn on_sigxcpu(_: libc::c_int) {
		let tid = AUDIO_TID.load(Ordering::Relaxed);
		if tid != 0 {
			let param = libc::sched_param { sched_priority: 0 };
			// SAFETY: async-signal-safe syscalls only.
			// (Unprivileged threads cannot clear SCHED_RESET_ON_FORK, which
			// both promotion paths set, so it is kept.)
			unsafe {
				let policy = libc::SCHED_OTHER | libc::SCHED_RESET_ON_FORK;
				if libc::sched_setscheduler(tid, policy, &param) == 0 {
					let msg = b"cadenza: an audio thread used too much realtime CPU without \
						blocking; it is back at normal priority\n";
					libc::write(2, msg.as_ptr().cast(), msg.len());
				}
			}
		}
	}

	/// Caps realtime CPU time without blocking at `hard_us` (soft limit: half,
	/// handled by demoting the audio thread). Needed by RealtimeKit.
	fn limit_rttime(hard_us: u64) -> bool {
		// SAFETY: installing a handler that only makes async-signal-safe calls;
		// zeroed sigaction is a valid starting point.
		unsafe {
			let mut action: libc::sigaction = std::mem::zeroed();
			action.sa_sigaction = on_sigxcpu as *const () as libc::sighandler_t;
			action.sa_flags = libc::SA_RESTART;
			libc::sigemptyset(&mut action.sa_mask);
			libc::sigaction(libc::SIGXCPU, &action, std::ptr::null_mut());
			let limit = libc::rlimit {
				rlim_cur: (hard_us / 2) as libc::rlim_t,
				rlim_max: hard_us as libc::rlim_t,
			};
			libc::setrlimit(libc::RLIMIT_RTTIME, &limit) == 0
		}
	}

	pub(super) fn current_tid() -> i32 {
		// SAFETY: gettid has no preconditions.
		unsafe { libc::gettid() }
	}

	pub(super) fn promote(tid: i32, priority: u32) -> Scheduling {
		AUDIO_TID.store(tid, Ordering::Relaxed);
		if set_fifo(tid, priority) {
			limit_rttime(200_000);
			return Scheduling::Realtime {
				priority,
				via: "sched_setscheduler",
			};
		}
		if let Some(rttime_max) = rtkit_property("RTTimeUSecMax") {
			// RealtimeKit only serves processes whose limit is at most this.
			limit_rttime(rttime_max.max(2) as u64);
			let max = rtkit_property("MaxRealtimePriority").unwrap_or(i64::from(priority));
			let priority = priority.min(max.clamp(1, 99) as u32);
			if rtkit_call("MakeThreadRealtimeWithPID", "ttu", tid, i64::from(priority)) {
				return Scheduling::Realtime {
					priority,
					via: "RealtimeKit",
				};
			}
		}
		const NICE: i32 = -11;
		if set_nice(tid, NICE) {
			return Scheduling::Nice {
				nice: NICE,
				via: "setpriority",
			};
		}
		let min = rtkit_property("MinNiceLevel").map(|v| v.clamp(-20, 0) as i32);
		if let Some(min) = min {
			let nice = NICE.max(min);
			if rtkit_call("MakeThreadHighPriorityWithPID", "tti", tid, i64::from(nice)) {
				return Scheduling::Nice {
					nice,
					via: "RealtimeKit",
				};
			}
		}
		AUDIO_TID.store(0, Ordering::Relaxed);
		Scheduling::Normal {
			reason: "no rtprio limit and RealtimeKit refused or is not running".into(),
		}
	}
}

#[cfg(target_os = "macos")]
mod imp {
	use super::Scheduling;

	/// `mach_timebase_info_data_t` (libc's binding is deprecated in favor of `mach2`).
	#[repr(C)]
	struct Timebase {
		numer: u32,
		denom: u32,
	}

	unsafe extern "C" {
		fn mach_timebase_info(info: *mut Timebase) -> libc::c_int;
	}

	/// The Mach port of the calling thread (valid for the whole process, so a
	/// helper thread can change its policy).
	pub(super) fn current_tid() -> i32 {
		// SAFETY: no preconditions; the port belongs to the running thread.
		unsafe { libc::pthread_mach_thread_np(libc::pthread_self()) as i32 }
	}

	/// Time-constraint policy sized for a 128-frame quantum at 48 kHz (about
	/// 2.7 ms): may compute up to half of every period, finished within it.
	pub(super) fn promote(tid: i32, _priority: u32) -> Scheduling {
		let mut timebase = Timebase { numer: 0, denom: 0 };
		// SAFETY: valid out-parameter.
		if unsafe { mach_timebase_info(&mut timebase) } != 0 || timebase.numer == 0 {
			return Scheduling::Normal {
				reason: "mach_timebase_info failed".into(),
			};
		}
		let ns_to_abs =
			|ns: u64| (ns * u64::from(timebase.denom) / u64::from(timebase.numer)) as u32;
		let period = 2_666_667;
		let mut policy = libc::thread_time_constraint_policy {
			period: ns_to_abs(period),
			computation: ns_to_abs(period / 2),
			constraint: ns_to_abs(period),
			preemptible: 1,
		};
		// SAFETY: `tid` is a thread port of this process from `current_tid`;
		// the policy struct matches the flavor and count.
		let r = unsafe {
			libc::thread_policy_set(
				tid as libc::thread_t,
				libc::THREAD_TIME_CONSTRAINT_POLICY as libc::thread_policy_flavor_t,
				(&raw mut policy).cast(),
				libc::THREAD_TIME_CONSTRAINT_POLICY_COUNT,
			)
		};
		if r == libc::KERN_SUCCESS {
			Scheduling::TimeCritical {
				policy: "time constraint",
				via: "thread_policy_set",
			}
		} else {
			Scheduling::Normal {
				reason: format!("thread_policy_set failed ({r})"),
			}
		}
	}
}

#[cfg(windows)]
mod imp {
	use super::Scheduling;
	use std::ffi::c_void;

	const THREAD_SET_INFORMATION: u32 = 0x0020;
	const THREAD_QUERY_INFORMATION: u32 = 0x0040;
	const THREAD_PRIORITY_TIME_CRITICAL: i32 = 15;

	#[link(name = "kernel32")]
	unsafe extern "system" {
		fn GetCurrentThreadId() -> u32;
		fn OpenThread(access: u32, inherit: i32, thread_id: u32) -> *mut c_void;
		fn SetThreadPriority(thread: *mut c_void, priority: i32) -> i32;
		fn CloseHandle(handle: *mut c_void) -> i32;
	}

	pub(super) fn current_tid() -> i32 {
		// SAFETY: no preconditions.
		unsafe { GetCurrentThreadId() as i32 }
	}

	pub(super) fn promote(tid: i32, _priority: u32) -> Scheduling {
		// SAFETY: plain Win32 calls; the handle is closed before returning.
		unsafe {
			let thread = OpenThread(
				THREAD_SET_INFORMATION | THREAD_QUERY_INFORMATION,
				0,
				tid as u32,
			);
			if thread.is_null() {
				return Scheduling::Normal {
					reason: format!("OpenThread failed ({})", std::io::Error::last_os_error()),
				};
			}
			let ok = SetThreadPriority(thread, THREAD_PRIORITY_TIME_CRITICAL) != 0;
			let err = std::io::Error::last_os_error();
			CloseHandle(thread);
			if ok {
				Scheduling::TimeCritical {
					policy: "time critical",
					via: "SetThreadPriority",
				}
			} else {
				Scheduling::Normal {
					reason: format!("SetThreadPriority failed ({err})"),
				}
			}
		}
	}
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
mod imp {
	use super::Scheduling;

	pub(super) fn current_tid() -> i32 {
		0
	}

	pub(super) fn promote(_tid: i32, _priority: u32) -> Scheduling {
		Scheduling::Normal {
			reason: "not implemented on this OS".into(),
		}
	}
}

/// Raises the calling thread to `SCHED_FIFO` `priority` (or the best the
/// system allows). `priority == 0` leaves it alone.
#[cfg_attr(not(unix), allow(dead_code))]
pub(crate) fn promote_current_thread(priority: u32) -> Scheduling {
	promote_thread(current_thread_id(), priority)
}

/// The calling thread's id (Linux `gettid`, macOS Mach port, Windows thread id; 0
/// elsewhere). One syscall, no
/// allocation: an audio callback may publish it for [`promote_thread`].
pub fn current_thread_id() -> i32 {
	imp::current_tid()
}

/// Raises thread `tid` of this process (from [`current_thread_id`]) to `SCHED_FIFO`
/// `priority`, or the best the system allows (RealtimeKit, then a negative nice value;
/// the platform's realtime class on macOS/Windows, where `priority` only switches it on).
/// May block on the system bus for a moment: call it from a helper thread, never from
/// the audio thread itself. `priority == 0` or `tid == 0` leaves it alone. Only one
/// thread per process is demoted by the `RLIMIT_RTTIME` guard (the last promoted).
pub fn promote_thread(tid: i32, priority: u32) -> Scheduling {
	if priority == 0 || tid == 0 {
		return Scheduling::Normal {
			reason: if priority == 0 {
				"disabled".into()
			} else {
				"not supported on this OS".into()
			},
		};
	}
	imp::promote(tid, priority.min(99))
}

/// Faults in `buf`'s pages now instead of on the first audio request (a fresh
/// zeroed allocation may still be unmapped copy-on-write zero pages).
#[cfg_attr(not(unix), allow(dead_code))]
pub(crate) fn prefault(buf: &mut [f32]) {
	for chunk in buf.chunks_mut(1024) {
		chunk[0] = std::hint::black_box(0.0);
	}
}
