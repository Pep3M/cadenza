//! The single plugin "main thread" shared by every hosted format.
//!
//! Plugin APIs (CLAP `[main-thread]` functions, VST3 component/controller
//! calls, every GUI call) must run on one stable thread for the life of an
//! instance. A lazily spawned `cadenza-plugin-main` thread owns every
//! non-`Send` plugin object and runs jobs submitted with [`run_on_main`] /
//! [`post_to_main`].
//!
//! Between jobs it is the plugins' event loop: it waits (with `poll`) on a
//! wake-up pipe for new jobs, the X11 connection of editor windows and the
//! file descriptors plugins registered, until the next plugin timer is due
//! (at most [`CALLBACK_POLL`]), then serves `request_callback`, timers, fd
//! events, window events and GUI requests (see [`crate::editor`]).

use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::OnceLock;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use crate::clap::ClapState;
use crate::error::PluginError;
use crate::vst3::Vst3State;

/// The instance whose editor an X11 window holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Owner {
	Clap(u64),
	Vst3(u64),
}

/// Windowing state of the main thread.
#[derive(Default)]
pub(crate) struct GuiState {
	#[cfg(all(unix, not(target_os = "macos")))]
	display: Option<std::rc::Rc<crate::editor::x11::Display>>,
	/// Our top-level editor windows.
	pub windows: HashMap<u32, Owner>,
}

impl GuiState {
	/// The X server connection, opened on first use.
	#[cfg(all(unix, not(target_os = "macos")))]
	pub fn display(&mut self) -> Result<std::rc::Rc<crate::editor::x11::Display>, String> {
		if let Some(d) = &self.display {
			return Ok(d.clone());
		}
		let d = std::rc::Rc::new(crate::editor::x11::Display::connect()?);
		self.display = Some(d.clone());
		Ok(d)
	}

	#[cfg(all(unix, not(target_os = "macos")))]
	pub fn connected(&self) -> Option<std::rc::Rc<crate::editor::x11::Display>> {
		self.display.clone()
	}

	/// Drops a broken connection (the next editor reconnects).
	#[cfg(all(unix, not(target_os = "macos")))]
	pub fn forget_display(&mut self) {
		self.display = None;
	}
}

/// Everything owned by the plugin main thread.
#[derive(Default)]
pub(crate) struct MainState {
	pub clap: ClapState,
	pub vst3: Vst3State,
	pub gui: GuiState,
}

type Job = Box<dyn FnOnce(&mut MainState) + Send>;

/// Longest sleep between two services of plugin callbacks.
const CALLBACK_POLL: Duration = Duration::from_millis(15);

struct MainThread {
	jobs: Sender<Job>,
	#[cfg(unix)]
	wake: std::os::fd::OwnedFd,
}

impl MainThread {
	fn wake(&self) {
		#[cfg(unix)]
		{
			use std::os::fd::AsRawFd;
			let byte = 1u8;
			// SAFETY: one byte from a valid buffer to our own non-blocking pipe;
			// a full pipe (EAGAIN) already guarantees a wake-up.
			unsafe { libc::write(self.wake.as_raw_fd(), (&raw const byte).cast(), 1) };
		}
	}
}

fn main_thread() -> &'static MainThread {
	static THREAD: OnceLock<MainThread> = OnceLock::new();
	THREAD.get_or_init(|| {
		let (tx, rx) = mpsc::channel::<Job>();
		#[cfg(unix)]
		{
			let (read, write) = wake_pipe().expect("create the plugin main thread wake-up pipe");
			std::thread::Builder::new()
				.name("cadenza-plugin-main".into())
				.spawn(move || run(rx, read))
				.expect("spawn plugin main thread");
			MainThread {
				jobs: tx,
				wake: write,
			}
		}
		#[cfg(not(unix))]
		{
			std::thread::Builder::new()
				.name("cadenza-plugin-main".into())
				.spawn(move || run(rx))
				.expect("spawn plugin main thread");
			MainThread { jobs: tx }
		}
	})
}

#[cfg(unix)]
fn wake_pipe() -> std::io::Result<(std::os::fd::OwnedFd, std::os::fd::OwnedFd)> {
	use std::os::fd::FromRawFd;
	let mut fds = [0 as libc::c_int; 2];
	// SAFETY: valid out array of two descriptors.
	#[cfg(target_os = "linux")]
	let r = unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_NONBLOCK | libc::O_CLOEXEC) };
	#[cfg(not(target_os = "linux"))]
	let r = unsafe {
		let r = libc::pipe(fds.as_mut_ptr());
		if r == 0 {
			for fd in fds {
				libc::fcntl(fd, libc::F_SETFL, libc::O_NONBLOCK);
				libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
			}
		}
		r
	};
	if r != 0 {
		return Err(std::io::Error::last_os_error());
	}
	// SAFETY: both descriptors were just created and are owned by us.
	Ok(unsafe {
		(
			std::os::fd::OwnedFd::from_raw_fd(fds[0]),
			std::os::fd::OwnedFd::from_raw_fd(fds[1]),
		)
	})
}

/// Runs every queued job; `false` once the senders are gone.
fn run_jobs(rx: &Receiver<Job>, state: &mut MainState) -> bool {
	loop {
		match rx.try_recv() {
			Ok(job) => job(state),
			Err(mpsc::TryRecvError::Empty) => return true,
			Err(mpsc::TryRecvError::Disconnected) => return false,
		}
	}
}

fn service(state: &mut MainState) {
	let _ = catch_unwind(AssertUnwindSafe(|| {
		state.clap.service_callbacks();
		crate::editor::service(state);
	}));
}

#[cfg(unix)]
fn run(rx: Receiver<Job>, wake: std::os::fd::OwnedFd) {
	use std::os::fd::AsRawFd;
	let mut state = MainState::default();
	let mut fds: Vec<libc::pollfd> = Vec::new();
	let mut owners = Vec::new();
	loop {
		if !run_jobs(&rx, &mut state) {
			break;
		}
		service(&mut state);
		let now = Instant::now();
		let timeout = crate::editor::next_deadline(&state)
			.map_or(CALLBACK_POLL, |t| t.saturating_duration_since(now))
			.min(CALLBACK_POLL);
		fds.clear();
		owners.clear();
		fds.push(libc::pollfd {
			fd: wake.as_raw_fd(),
			events: libc::POLLIN,
			revents: 0,
		});
		crate::editor::collect_fds(&state, &mut fds, &mut owners);
		let ms = timeout.as_millis().min(i32::MAX as u128) as libc::c_int;
		// SAFETY: `fds` is a valid array of `fds.len()` pollfds.
		let n = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, ms) };
		if n <= 0 {
			continue;
		}
		if fds[0].revents != 0 {
			let mut buf = [0u8; 64];
			// SAFETY: draining our own non-blocking pipe into a local buffer.
			while unsafe { libc::read(wake.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) } > 0 {}
		}
		let ready: Vec<_> = fds[1..]
			.iter()
			.zip(&owners)
			.filter(|(p, _)| p.revents != 0)
			.map(|(p, o)| (p.fd, p.revents, *o))
			.collect();
		if !ready.is_empty() {
			let _ = catch_unwind(AssertUnwindSafe(|| {
				crate::editor::dispatch_fds(&mut state, &ready);
			}));
		}
	}
}

#[cfg(not(unix))]
fn run(rx: Receiver<Job>) {
	let mut state = MainState::default();
	loop {
		match rx.recv_timeout(CALLBACK_POLL) {
			Ok(job) => job(&mut state),
			Err(mpsc::RecvTimeoutError::Timeout) => {}
			Err(mpsc::RecvTimeoutError::Disconnected) => break,
		}
		if !run_jobs(&rx, &mut state) {
			break;
		}
		service(&mut state);
	}
}

fn submit(job: Job) -> Result<(), PluginError> {
	let thread = main_thread();
	thread
		.jobs
		.send(job)
		.map_err(|_| PluginError::HostThread("the plugin main thread has stopped".into()))?;
	thread.wake();
	Ok(())
}

/// Runs `f` on the plugin main thread and waits for its result. Must not be
/// called from the audio thread (it blocks) nor from inside another job.
pub(crate) fn run_on_main<R: Send + 'static>(
	f: impl FnOnce(&mut MainState) -> R + Send + 'static,
) -> Result<R, PluginError> {
	let (tx, rx) = mpsc::sync_channel(1);
	submit(Box::new(move |state| {
		let result = catch_unwind(AssertUnwindSafe(|| f(state)));
		let _ = tx.send(result);
	}))?;
	match rx.recv() {
		Ok(Ok(r)) => Ok(r),
		Ok(Err(_)) => Err(PluginError::HostThread(
			"a plugin operation panicked on the main thread".into(),
		)),
		Err(_) => Err(PluginError::HostThread(
			"the plugin main thread stopped while working".into(),
		)),
	}
}

/// Queues `f` on the plugin main thread without waiting.
pub(crate) fn post_to_main(f: impl FnOnce(&mut MainState) + Send + 'static) {
	let _ = submit(Box::new(move |state| {
		let _ = catch_unwind(AssertUnwindSafe(|| f(state)));
	}));
}
