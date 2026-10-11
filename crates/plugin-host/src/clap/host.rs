//! Host-side callback handlers given to every CLAP instance.
//!
//! Callbacks only record requests in atomic flags (they may be invoked from
//! the audio thread, where we must not lock, allocate or block). The flags are
//! serviced elsewhere: `request_callback` by the plugin main thread loop,
//! restart/latency/port changes on the next `Processor::prepare`, and param
//! flush requests by the next `process` (which always delivers queued events).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::ThreadId;

use clack_extensions::audio_ports::{AudioPortRescanFlags, HostAudioPorts, HostAudioPortsImpl};
use clack_extensions::latency::{HostLatency, HostLatencyImpl};
use clack_extensions::log::{HostLog, HostLogImpl, LogSeverity};
use clack_extensions::params::{
	HostParams, HostParamsImplMainThread, HostParamsImplShared, ParamClearFlags, ParamRescanFlags,
};
use clack_extensions::state::{HostState, HostStateImpl};
use clack_extensions::tail::{HostTail, HostTailImpl};
use clack_extensions::thread_check::{HostThreadCheck, HostThreadCheckImpl};
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
}

impl HostFlags {
	pub fn take(flag: &AtomicBool) -> bool {
		flag.swap(false, Ordering::AcqRel)
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
			.register::<HostThreadCheck>();
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
