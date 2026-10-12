//! Host-side callback handlers given to every CLAP instance.
//!
//! Callbacks only record requests in atomic flags (they may be invoked from
//! the audio thread, where we must not lock, allocate or block). The flags are
//! serviced elsewhere: `request_callback` and GUI requests by the plugin main
//! thread loop, restart/latency/port changes on the next `Processor::prepare`,
//! and param flush requests by the next `process` or `idle` (which always
//! deliver queued events). Timer and file descriptor registrations (main
//! thread only) go into a small registry the main thread loop serves.

use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::ThreadId;
use std::time::{Duration, Instant};

use clack_extensions::audio_ports::{AudioPortRescanFlags, HostAudioPorts, HostAudioPortsImpl};
use clack_extensions::gui::{GuiSize, HostGui, HostGuiImpl};
use clack_extensions::latency::{HostLatency, HostLatencyImpl};
use clack_extensions::log::{HostLog, HostLogImpl, LogSeverity};
use clack_extensions::params::{
	HostParams, HostParamsImplMainThread, HostParamsImplShared, ParamClearFlags, ParamRescanFlags,
};
use clack_extensions::preset_discovery::HostPresetLoadImpl;
use clack_extensions::preset_discovery::prelude::{HostPresetLoad, Location};
use clack_extensions::state::{HostState, HostStateImpl};
use clack_extensions::tail::{HostTail, HostTailImpl};
use clack_extensions::thread_check::{HostThreadCheck, HostThreadCheckImpl};
use clack_extensions::timer::{HostTimer, HostTimerImpl, TimerId};
use clack_host::prelude::*;

/// Requests a plugin made through host callbacks, consumed by the host.
#[derive(Debug, Default)]
pub(crate) struct HostFlags {
	pub restart: AtomicBool,
	pub process: AtomicBool,
	pub callback: AtomicBool,
	pub flush: AtomicBool,
	pub latency_changed: AtomicBool,
	pub params_rescan: AtomicBool,
	pub state_dirty: AtomicBool,
	pub tail_changed: AtomicBool,
	/// Editor size the plugin asked for (`GuiSize::pack_to_u64`; 0 = none).
	pub gui_resize: AtomicU64,
	/// The plugin's resize hints or resizability changed.
	pub gui_hints: AtomicBool,
	/// `GUI_SHOW` / `GUI_HIDE` requested by the plugin (0 = none).
	pub gui_visibility: AtomicU8,
	/// The plugin's (floating) window was closed: `GUI_CLOSED` or
	/// `GUI_DESTROYED` (the plugin already destroyed its GUI).
	pub gui_closed: AtomicU8,
	/// Timers and file descriptors registered by the plugin (main thread only;
	/// the mutex is never contended).
	pub registry: Mutex<Registry>,
}

pub(crate) const GUI_SHOW: u8 = 1;
pub(crate) const GUI_HIDE: u8 = 2;
pub(crate) const GUI_CLOSED: u8 = 1;
pub(crate) const GUI_DESTROYED: u8 = 2;

/// Shortest timer period granted (CLAP lets hosts clamp periods).
const MIN_TIMER_PERIOD: Duration = Duration::from_millis(5);

#[derive(Debug)]
pub(crate) struct Timer {
	pub id: u32,
	pub period: Duration,
	pub next: Instant,
}

/// Timers (timer-support) and file descriptors (posix-fd-support) of one
/// instance.
#[derive(Debug, Default)]
pub(crate) struct Registry {
	pub timers: Vec<Timer>,
	next_timer: u32,
	/// fd and `FdFlags` bits.
	pub fds: Vec<(i32, u32)>,
}

impl Registry {
	/// Ids of the timers due at `now`, rescheduling them.
	pub fn due_timers(&mut self, now: Instant) -> Vec<u32> {
		let mut due = Vec::new();
		for t in &mut self.timers {
			if t.next <= now {
				due.push(t.id);
				// Skip missed ticks instead of firing a burst.
				t.next = (t.next + t.period).max(now + t.period / 2);
			}
		}
		due
	}

	pub fn next_deadline(&self) -> Option<Instant> {
		self.timers.iter().map(|t| t.next).min()
	}
}

impl HostFlags {
	pub fn take(flag: &AtomicBool) -> bool {
		flag.swap(false, Ordering::AcqRel)
	}

	pub fn registry(&self) -> MutexGuard<'_, Registry> {
		self.registry.lock().unwrap_or_else(|e| e.into_inner())
	}
}

pub(crate) struct CadenzaHost;

pub(crate) struct HostShared {
	pub flags: Arc<HostFlags>,
	/// Plugin id, used to prefix log messages.
	pub plugin: String,
	/// The thread that created the instance: the CLAP "main thread".
	pub main_thread: ThreadId,
}

impl HostShared {
	pub fn new(flags: Arc<HostFlags>, plugin: String) -> Self {
		Self {
			flags,
			plugin,
			main_thread: std::thread::current().id(),
		}
	}

	fn set(flag: &AtomicBool) {
		flag.store(true, Ordering::Release);
	}
}

impl SharedHandler<'_> for HostShared {
	fn request_restart(&self) {
		Self::set(&self.flags.restart);
	}

	fn request_process(&self) {
		Self::set(&self.flags.process);
	}

	fn request_callback(&self) {
		Self::set(&self.flags.callback);
	}
}

impl HostLogImpl for HostShared {
	fn log(&self, severity: LogSeverity, message: &str) {
		// CLAP forbids logging from the audio thread; well-behaved plugins only
		// log on the main thread, where stderr I/O is acceptable.
		if severity >= LogSeverity::Warning || std::env::var_os("CADENZA_PLUGIN_LOG").is_some() {
			eprintln!("[plugin {}] {severity}: {message}", self.plugin);
		}
	}
}

impl HostGuiImpl for HostShared {
	fn resize_hints_changed(&self) {
		Self::set(&self.flags.gui_hints);
	}

	fn request_resize(&self, size: GuiSize) -> Result<(), HostError> {
		if size.width == 0 || size.height == 0 {
			return Err(HostError::Message("empty editor size"));
		}
		self.flags
			.gui_resize
			.store(size.pack_to_u64(), Ordering::Release);
		Ok(())
	}

	fn request_show(&self) -> Result<(), HostError> {
		self.flags.gui_visibility.store(GUI_SHOW, Ordering::Release);
		Ok(())
	}

	fn request_hide(&self) -> Result<(), HostError> {
		self.flags.gui_visibility.store(GUI_HIDE, Ordering::Release);
		Ok(())
	}

	fn closed(&self, was_destroyed: bool) {
		let how = if was_destroyed {
			GUI_DESTROYED
		} else {
			GUI_CLOSED
		};
		self.flags.gui_closed.store(how, Ordering::Release);
	}
}

impl HostParamsImplShared for HostShared {
	fn request_flush(&self) {
		Self::set(&self.flags.flush);
	}
}

impl HostThreadCheckImpl for HostShared {
	fn is_main_thread(&self) -> bool {
		std::thread::current().id() == self.main_thread
	}

	fn is_audio_thread(&self) -> bool {
		std::thread::current().id() != self.main_thread
	}
}

pub(crate) struct HostMainThread<'a> {
	shared: &'a HostShared,
}

impl<'a> HostMainThread<'a> {
	pub fn new(shared: &'a HostShared) -> Self {
		Self { shared }
	}
}

impl<'a> MainThreadHandler<'a> for HostMainThread<'a> {}

impl HostParamsImplMainThread for HostMainThread<'_> {
	fn rescan(&self, flags: ParamRescanFlags) {
		HostShared::set(&self.shared.flags.params_rescan);
		if flags.requires_restart() {
			HostShared::set(&self.shared.flags.restart);
		}
	}

	fn clear(&self, _param_id: ClapId, _flags: ParamClearFlags) {}
}

impl HostStateImpl for HostMainThread<'_> {
	fn mark_dirty(&self) {
		HostShared::set(&self.shared.flags.state_dirty);
	}
}

impl HostPresetLoadImpl for HostMainThread<'_> {
	fn on_error(
		&self,
		_location: Location,
		load_key: Option<&std::ffi::CStr>,
		os_error: i32,
		message: Option<&std::ffi::CStr>,
	) {
		eprintln!(
			"[plugin {}] preset {:?} failed to load (os error {os_error}): {}",
			self.shared.plugin,
			load_key.map(|k| k.to_string_lossy()),
			message.map(|m| m.to_string_lossy()).unwrap_or_default()
		);
	}

	fn loaded(&self, _location: Location, _load_key: Option<&std::ffi::CStr>) {
		HostShared::set(&self.shared.flags.state_dirty);
	}
}

impl HostTimerImpl for HostMainThread<'_> {
	fn register_timer(&self, period_ms: u32) -> Result<TimerId, HostError> {
		let mut registry = self.shared.flags.registry();
		registry.next_timer = registry.next_timer.wrapping_add(1).max(1);
		let id = registry.next_timer;
		let period = Duration::from_millis(u64::from(period_ms)).max(MIN_TIMER_PERIOD);
		registry.timers.push(Timer {
			id,
			period,
			next: Instant::now() + period,
		});
		Ok(TimerId(id))
	}

	fn unregister_timer(&self, timer_id: TimerId) -> Result<(), HostError> {
		let mut registry = self.shared.flags.registry();
		let before = registry.timers.len();
		registry.timers.retain(|t| t.id != timer_id.0);
		if registry.timers.len() == before {
			return Err(HostError::Message("unknown timer"));
		}
		Ok(())
	}
}

#[cfg(unix)]
impl clack_extensions::posix_fd::HostPosixFdImpl for HostMainThread<'_> {
	fn register_fd(
		&self,
		fd: std::os::fd::RawFd,
		flags: clack_extensions::posix_fd::FdFlags,
	) -> Result<(), HostError> {
		let mut registry = self.shared.flags.registry();
		if registry.fds.iter().any(|&(f, _)| f == fd) {
			return Err(HostError::Message("file descriptor already registered"));
		}
		registry.fds.push((fd, flags.bits()));
		Ok(())
	}

	fn modify_fd(
		&self,
		fd: std::os::fd::RawFd,
		flags: clack_extensions::posix_fd::FdFlags,
	) -> Result<(), HostError> {
		let mut registry = self.shared.flags.registry();
		match registry.fds.iter_mut().find(|(f, _)| *f == fd) {
			Some(entry) => {
				entry.1 = flags.bits();
				Ok(())
			}
			None => Err(HostError::Message("unknown file descriptor")),
		}
	}

	fn unregister_fd(&self, fd: std::os::fd::RawFd) -> Result<(), HostError> {
		let mut registry = self.shared.flags.registry();
		let before = registry.fds.len();
		registry.fds.retain(|&(f, _)| f != fd);
		if registry.fds.len() == before {
			return Err(HostError::Message("unknown file descriptor"));
		}
		Ok(())
	}
}

impl HostLatencyImpl for HostMainThread<'_> {
	fn changed(&self) {
		HostShared::set(&self.shared.flags.latency_changed);
	}
}

impl HostAudioPortsImpl for HostMainThread<'_> {
	fn is_rescan_flag_supported(&self, _flag: AudioPortRescanFlags) -> bool {
		// Every port change is handled the same way: by re-activating.
		true
	}

	fn rescan(&self, _flags: AudioPortRescanFlags) {
		HostShared::set(&self.shared.flags.restart);
	}
}

pub(crate) struct HostAudioProcessor<'a> {
	shared: &'a HostShared,
}

impl<'a> HostAudioProcessor<'a> {
	pub fn new(shared: &'a HostShared) -> Self {
		Self { shared }
	}
}

impl<'a> AudioProcessorHandler<'a> for HostAudioProcessor<'a> {}

impl HostTailImpl for HostAudioProcessor<'_> {
	fn changed(&mut self) {
		HostShared::set(&self.shared.flags.tail_changed);
	}
}

impl HostHandlers for CadenzaHost {
	type Shared<'a> = HostShared;
	type MainThread<'a> = HostMainThread<'a>;
	type AudioProcessor<'a> = HostAudioProcessor<'a>;

	// The note-ports host extension is deliberately not registered: clack-host
	// 0.2.0 exposes it through a double reference (`&&clap_host_note_ports`),
	// so a plugin calling it would read a bogus vtable.
	fn declare_extensions(builder: &mut HostExtensions<Self>, _shared: &Self::Shared<'_>) {
		builder
			.register::<HostLog>()
			.register::<HostParams>()
			.register::<HostState>()
			.register::<HostLatency>()
			.register::<HostAudioPorts>()
			.register::<HostTail>()
			.register::<HostThreadCheck>()
			.register::<HostPresetLoad>()
			.register::<HostGui>()
			.register::<HostTimer>();
		#[cfg(unix)]
		builder.register::<clack_extensions::posix_fd::HostPosixFd>();
	}
}

pub(crate) fn host_info() -> HostInfo {
	HostInfo::new(
		"Cadenza",
		"Cadenza",
		"https://github.com/Pep3M/cadenza",
		env!("CARGO_PKG_VERSION"),
	)
	.expect("static host info has no interior NUL")
}
