//! Crash-isolated ("sandboxed") plugin processing.
//!
//! A [`SandboxedProcessor`] runs one plugin instance inside its own
//! `cadenza-plugin-sandbox` child process, so a plugin that aborts, segfaults
//! or hangs only loses itself: the host marks it crashed, bypasses it
//! (instruments output silence, effects pass audio through) and reports why
//! (see [`SandboxMonitor`]); [`SandboxedProcessor::restart`] brings it back
//! from its last known state.
//!
//! # Transport
//!
//! - **Audio** goes through a shared memory block (layout in `shm.rs`):
//!   a header of atomics (request/ack sequence numbers, command, frame count,
//!   transport), a fixed-capacity event area, a parameter-change area and one
//!   in-place stereo buffer of `max(max_block, 4096)` frames. Per block the
//!   audio thread writes inputs, events and pending parameter changes, bumps
//!   the request sequence and wakes the child (Linux futex), then waits for
//!   the acknowledgement with a bounded timeout. The round trip is synchronous
//!   inside the block, so sandboxing adds no latency in samples, only CPU
//!   time per block. The audio thread never allocates, locks or does I/O.
//! - **Control** (create, prepare, state, shutdown) is length-prefixed JSON
//!   over the child's stdin/stdout, off the audio thread, with a deadline.
//!
//! # Failure semantics
//!
//! If the child dies or misses the deadline (realtime: `max(2 ms, 2 x block
//! duration)`, first block after (re)activation: 500 ms, offline: 30 s) the
//! processor is marked crashed, a hung child is `SIGKILL`ed, and from then on
//! `process` bypasses without touching the child. `latency`/`tail_seconds`
//! keep their last values so delay compensation stays stable; `save_state`
//! returns the last state saved or loaded, so a project can still be saved.
//!
//! # Lifetime
//!
//! Children are spawned from the long-lived plugin main thread so that
//! `PR_SET_PDEATHSIG` (Linux) kills them with the host; on every OS they also
//! exit when their stdin reaches EOF. Dropped children are reaped by a
//! background thread (no zombies), and the shared memory name is unlinked as
//! soon as the child has mapped it.
//!
//! Future work: grouping several plugins per sandbox process (lower overhead,
//! weaker isolation), realtime scheduling of the child audio thread (rtkit),
//! Windows support (named file mappings + events).

#[cfg(unix)]
mod child;
mod protocol;
mod shm;
mod sync;

use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use base64::Engine as _;
use cadenza_dsp::{DeviceInfo, MidiEvent, Processor, Transport};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::descriptor::{PluginDescriptor, PluginRole};
use crate::error::PluginError;
use crate::{ProcessMode, encode_state};
use protocol::{Reply, Request};
use shm::{Block, CMD_PARAMS, CMD_PROCESS, CMD_RESET, Layout, Region, W_ACK_SEQ, W_REQ_SEQ};

/// Environment variable overriding the sandbox executable location.
pub const SANDBOX_ENV: &str = "CADENZA_PLUGIN_SANDBOX";
/// File name of the sandbox executable for this OS.
pub const SANDBOX_FILE_NAME: &str = if cfg!(windows) {
	"cadenza-plugin-sandbox.exe"
} else {
	"cadenza-plugin-sandbox"
};

/// Where the sandbox executable is expected: `$CADENZA_PLUGIN_SANDBOX` if set,
/// else next to the running executable (or its parent directory, as for
/// [`crate::scanner_path`]).
pub fn sandbox_path() -> Result<PathBuf, PluginError> {
	crate::scanner::locate_executable(SANDBOX_ENV, SANDBOX_FILE_NAME)
		.map_err(PluginError::SandboxMissing)
}

/// Entry point of the `cadenza-plugin-sandbox` executable. Not for hosts.
#[doc(hidden)]
pub fn run_sandbox_child() -> std::process::ExitCode {
	#[cfg(unix)]
	{
		child::run(std::env::args_os().skip(1))
	}
	#[cfg(not(unix))]
	{
		eprintln!("cadenza-plugin-sandbox: sandboxed hosting is not implemented on this OS yet");
		std::process::ExitCode::from(2)
	}
}

/// Frames the shared buffer holds at least (larger `prepare` sizes are
/// processed in chunks of this size).
const MIN_CAPACITY: usize = 4096;
/// MIDI events per round trip; at least what an in-process CLAP instance
/// queues, so both drop the same events on overflow.
const EVENT_CAPACITY: usize = 2048;

/// Deadlines and debugging switches for sandboxed instances.
#[derive(Debug, Clone, PartialEq)]
pub struct SandboxOptions {
	/// Minimum per-block deadline in realtime mode; the effective deadline is
	/// `max(realtime_timeout, 2 x max_block / sample_rate)`.
	pub realtime_timeout: Duration,
	/// Deadline of the first block after creation, `prepare` or `restart`
	/// (plugins often do lazy setup there).
	pub warmup_timeout: Duration,
	/// Per-block deadline in offline mode.
	pub offline_timeout: Duration,
	/// Deadline for control operations (load, activation, state).
	pub control_timeout: Duration,
	/// Let crashing plugins write core dumps (useful for reporting bugs to
	/// vendors; slows down crash detection while the dump is written).
	pub core_dumps: bool,
	/// Sandbox executable; `None` uses [`sandbox_path`].
	pub executable: Option<PathBuf>,
}

impl Default for SandboxOptions {
	fn default() -> Self {
		Self {
			realtime_timeout: Duration::from_millis(2),
			warmup_timeout: Duration::from_millis(500),
			offline_timeout: Duration::from_secs(30),
			control_timeout: protocol::CONTROL_TIMEOUT,
			core_dumps: true,
			executable: None,
		}
	}
}

/// Why a sandboxed plugin stopped.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CrashReason {
	/// The plugin process was killed by a signal (e.g. `SIGSEGV`, `SIGABRT`, `SIGFPE`).
	Signal { signal: i32, name: String },
	/// The plugin process exited on its own.
	Exited { code: i32 },
	/// The plugin did not finish an audio block in time and was killed.
	TimedOut { millis: u64 },
	/// The control channel failed (e.g. a garbled reply or a lost pipe).
	Lost { detail: String },
}

/// A crash report, written to be shown to a user or an LLM.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CrashInfo {
	pub plugin_id: String,
	pub plugin_name: String,
	pub reason: CrashReason,
	/// e.g. "plugin 'Surge XT' crashed (SIGABRT); it was bypassed ..."
	pub message: String,
}

const HEALTHY: u8 = 0;
const TIMED_OUT: u8 = 1;
const DIED: u8 = 2;
const LOST: u8 = 3;

struct StatusInner {
	/// Live (unreaped) child pid; `None` once the owner is reaping it, so a
	/// monitor never inspects a recycled pid.
	pid: Option<u32>,
	info: Option<CrashInfo>,
	lost_detail: String,
	last_state: Option<Vec<u8>>,
}

/// Crash status shared between a processor and its monitors.
struct Status {
	state: AtomicU8,
	timeout_us: AtomicU64,
	plugin_id: String,
	plugin_name: String,
	role: PluginRole,
	inner: Mutex<StatusInner>,
}

impl Status {
	fn lock(&self) -> MutexGuard<'_, StatusInner> {
		self.inner.lock().unwrap_or_else(|e| e.into_inner())
	}

	/// Resolves (and caches) the crash report. Non-realtime.
	fn crash_info(&self) -> Option<CrashInfo> {
		let mut inner = self.lock();
		let mut state = self.state.load(Ordering::Acquire);
		if state == HEALTHY {
			// Notice a child that died while no block was being processed.
			match inner.pid.and_then(peek_exit) {
				Some(_) => {
					state = DIED;
					self.state.store(DIED, Ordering::Release);
				}
				None => return None,
			}
		}
		if let Some(info) = &inner.info {
			return Some(info.clone());
		}
		let exit = inner.pid.and_then(|pid| {
			// A killed or aborting process may take a moment to become a zombie.
			let deadline = Instant::now() + Duration::from_millis(500);
			loop {
				if let Some(e) = peek_exit(pid) {
					return Some(e);
				}
				if Instant::now() >= deadline {
					return None;
				}
				std::thread::sleep(Duration::from_millis(2));
			}
		});
		let reason = match (state, exit) {
			(TIMED_OUT, _) => CrashReason::TimedOut {
				millis: self.timeout_us.load(Ordering::Relaxed).div_ceil(1000),
			},
			(_, Some(ExitKind::Signal(signal))) => CrashReason::Signal {
				signal,
				name: signal_name(signal),
			},
			(_, Some(ExitKind::Code(code))) => CrashReason::Exited { code },
			(LOST, None) => CrashReason::Lost {
				detail: inner.lost_detail.clone(),
			},
			_ => CrashReason::Lost {
				detail: "the sandbox process stopped responding".into(),
			},
		};
		let what = match &reason {
			CrashReason::Signal { name, .. } => format!("crashed ({name})"),
			CrashReason::Exited { code } => format!("exited unexpectedly (code {code})"),
			CrashReason::TimedOut { millis } => {
				format!("hung (no audio within {millis} ms) and was stopped")
			}
			CrashReason::Lost { detail } => format!("stopped working ({detail})"),
		};
		let bypass = match self.role {
			PluginRole::Instrument => "it was bypassed and now outputs silence",
			PluginRole::Effect => "it was bypassed and now passes audio through unchanged",
		};
		let info = CrashInfo {
			plugin_id: self.plugin_id.clone(),
			plugin_name: self.plugin_name.clone(),
			message: format!(
				"plugin '{}' {what}; {bypass}. The rest of the project kept playing; \
				 restart the plugin to reload it from its last saved state, or replace it",
				self.plugin_name
			),
			reason,
		};
		inner.info = Some(info.clone());
		Some(info)
	}
}

/// Thread-safe, cloneable view of a sandboxed instance's health, for the
/// engine/UI/MCP to poll while the processor itself lives on the audio thread.
#[derive(Clone)]
pub struct SandboxMonitor(Arc<Status>);

impl SandboxMonitor {
	/// Cheap atomic check (fine on the audio thread). Only reflects failures
	/// seen by `process`/`reset`; [`SandboxMonitor::crashed`] also notices an
	/// idle child that died.
	pub fn is_crashed(&self) -> bool {
		self.0.state.load(Ordering::Acquire) != HEALTHY
	}

	/// The crash report, or `None` while the plugin is healthy. Not
	/// realtime (may wait briefly for the dead process' exit status).
	pub fn crashed(&self) -> Option<CrashInfo> {
		self.0.crash_info()
	}

	/// The last state saved from or loaded into the plugin (or the state it
	/// was created with), to re-create it after a crash.
	pub fn last_state(&self) -> Option<Vec<u8>> {
		self.0.lock().last_state.clone()
	}

	pub fn plugin_id(&self) -> &str {
		&self.0.plugin_id
	}
}

// ------------------------------------------------------------ child exit

enum ExitKind {
	Signal(i32),
	Code(i32),
}

/// Exit status of an exited-but-unreaped child, without reaping it.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn peek_exit(pid: u32) -> Option<ExitKind> {
	// SAFETY: zeroed siginfo is a valid out-parameter; WNOWAIT leaves the
	// child reapable by its `std::process::Child`; WNOHANG never blocks.
	unsafe {
		let mut info: libc::siginfo_t = std::mem::zeroed();
		let r = libc::waitid(
			libc::P_PID,
			pid as libc::id_t,
			&mut info,
			libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
		);
		if r != 0 {
			return None;
		}
		#[cfg(target_os = "linux")]
		let (who, status) = (info.si_pid(), info.si_status());
		#[cfg(target_os = "macos")]
		let (who, status) = (info.si_pid, info.si_status);
		if who == 0 {
			return None;
		}
		Some(if info.si_code == libc::CLD_EXITED {
			ExitKind::Code(status)
		} else {
			ExitKind::Signal(status)
		})
	}
}

/// Other systems detect dead children by the block deadline only.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn peek_exit(_pid: u32) -> Option<ExitKind> {
	None
}

fn signal_name(signal: i32) -> String {
	#[cfg(unix)]
	{
		let known = [
			(libc::SIGABRT, "SIGABRT"),
			(libc::SIGSEGV, "SIGSEGV"),
			(libc::SIGFPE, "SIGFPE"),
			(libc::SIGBUS, "SIGBUS"),
			(libc::SIGILL, "SIGILL"),
			(libc::SIGKILL, "SIGKILL"),
			(libc::SIGTERM, "SIGTERM"),
			(libc::SIGTRAP, "SIGTRAP"),
			(libc::SIGPIPE, "SIGPIPE"),
		];
		if let Some((_, name)) = known.iter().find(|(s, _)| *s == signal) {
			return (*name).to_string();
		}
	}
	format!("signal {signal}")
}

#[cfg(unix)]
fn kill_now(pid: u32) {
	// SAFETY: async-signal-safe syscall, no allocation; the pid is our
	// unreaped child, so it cannot have been recycled.
	unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
}

#[cfg(not(unix))]
fn kill_now(_pid: u32) {}

/// Reaps dropped children in the background: gives them a grace period to
/// exit after their stdin closes, then kills them.
fn reap_later(child: Child) {
	use std::sync::OnceLock;
	use std::sync::mpsc::{self, RecvTimeoutError, Sender};
	static REAPER: OnceLock<Mutex<Sender<Child>>> = OnceLock::new();
	let sender = REAPER.get_or_init(|| {
		let (tx, rx) = mpsc::channel::<Child>();
		let spawned = std::thread::Builder::new()
			.name("cadenza-sandbox-reaper".into())
			.spawn(move || {
				let mut pending: Vec<(Child, Instant)> = Vec::new();
				loop {
					let wait = if pending.is_empty() {
						Duration::from_secs(3600)
					} else {
						Duration::from_millis(20)
					};
					match rx.recv_timeout(wait) {
						Ok(c) => pending.push((c, Instant::now() + Duration::from_secs(2))),
						Err(RecvTimeoutError::Timeout) => {}
						Err(RecvTimeoutError::Disconnected) => break,
					}
					pending.retain_mut(|(c, deadline)| match c.try_wait() {
						Ok(Some(_)) | Err(_) => false,
						Ok(None) if Instant::now() >= *deadline => {
							let _ = c.kill();
							let _ = c.wait();
							false
						}
						Ok(None) => true,
					});
				}
			});
		if spawned.is_err() {
			eprintln!("cadenza-plugin-host: cannot start the sandbox reaper thread");
		}
		Mutex::new(tx)
	});
	let tx = sender.lock().unwrap_or_else(|e| e.into_inner());
	if let Err(mpsc::SendError(mut child)) = tx.send(child) {
		let _ = child.kill();
		let _ = child.wait();
	}
}

// ------------------------------------------------------------ connection

/// One live sandbox child and its channels.
struct Connection {
	child: Child,
	pid: u32,
	stdin: ChildStdin,
	stdout: ChildStdout,
	region: Region,
	layout: Layout,
	seq: u32,
}

impl Connection {
	fn block(&self) -> Block<'_> {
		// The region was created with exactly this layout.
		Block {
			words: self.region.words(),
			layout: self.layout,
		}
	}

	fn spawn(
		desc: &PluginDescriptor,
		sample_rate: f32,
		max_block: usize,
		state: Option<&[u8]>,
		mode: ProcessMode,
		options: &SandboxOptions,
	) -> Result<(Connection, Reply), PluginError> {
		let fail = |reason: String| PluginError::Sandbox {
			id: desc.id.clone(),
			reason,
		};
		let exe = match &options.executable {
			Some(p) => p.clone(),
			None => sandbox_path()?,
		};
		let layout = Layout {
			capacity: max_block.max(MIN_CAPACITY).min(shm::MAX_CAPACITY as usize),
			event_capacity: EVENT_CAPACITY,
			param_capacity: desc.params.len().min(shm::MAX_PARAMS as usize),
		};
		let region = Region::create(layout.bytes()).map_err(fail)?;
		layout.write_header(region.words());

		let mut cmd = Command::new(&exe);
		cmd.arg("--shm")
			.arg(region.name())
			.arg("--parent")
			.arg(std::process::id().to_string())
			.stdin(Stdio::piped())
			.stdout(Stdio::piped())
			.stderr(Stdio::inherit());
		if !options.core_dumps {
			cmd.arg("--no-core-dumps");
		}
		// Spawned from the plugin main thread, which lives as long as the
		// host: PR_SET_PDEATHSIG fires when the *spawning thread* exits.
		let mut child = crate::main_thread::run_on_main(move |_| cmd.spawn())?
			.map_err(|e| fail(format!("cannot start '{}': {e}", exe.display())))?;
		let pid = child.id();
		let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
			reap_later(child);
			return Err(fail("the sandbox process has no control pipes".into()));
		};
		let mut conn = Connection {
			child,
			pid,
			stdin,
			stdout,
			region,
			layout,
			seq: 0,
		};
		let init = Request::Init {
			descriptor: Box::new(desc.clone()),
			sample_rate,
			max_block,
			offline: mode == ProcessMode::Offline,
			state: state.map(encode_state),
		};
		let reply = conn.request(&init, options.control_timeout);
		// The child has mapped the region (or is gone): drop the name.
		conn.region.unlink();
		let reply = match reply {
			Ok(r) => r,
			Err(e) => {
				let detail = describe_spawn_failure(&mut conn.child, e);
				conn.close();
				return Err(fail(detail));
			}
		};
		if let Some(error) = reply.error.clone() {
			conn.close();
			return Err(if reply.state_rejected {
				PluginError::State {
					id: desc.id.clone(),
					reason: error,
				}
			} else {
				fail(error)
			});
		}
		Ok((conn, reply))
	}

	#[cfg(unix)]
	fn request(&mut self, req: &Request, timeout: Duration) -> std::io::Result<Reply> {
		protocol::write_frame(&mut self.stdin, req)?;
		protocol::read_frame_until(&mut self.stdout, Instant::now() + timeout)
	}

	#[cfg(not(unix))]
	fn request(&mut self, _req: &Request, _timeout: Duration) -> std::io::Result<Reply> {
		Err(std::io::Error::other(
			"sandboxed hosting is not implemented on this OS",
		))
	}

	/// Asks the child to exit and hands it to the reaper.
	fn close(self) {
		let Connection {
			child, mut stdin, ..
		} = self;
		let _ = protocol::write_frame(&mut stdin, &Request::Shutdown);
		drop(stdin);
		reap_later(child);
	}
}

fn describe_spawn_failure(child: &mut Child, e: std::io::Error) -> String {
	// Give a crashing child a moment to finish dying for a precise report.
	let deadline = Instant::now() + Duration::from_millis(500);
	while Instant::now() < deadline {
		if let Some(exit) = peek_exit(child.id()) {
			return match exit {
				ExitKind::Signal(s) => {
					format!("the plugin crashed while loading ({})", signal_name(s))
				}
				ExitKind::Code(c) => format!("the sandbox process exited while loading (code {c})"),
			};
		}
		if e.kind() == std::io::ErrorKind::TimedOut {
			break;
		}
		std::thread::sleep(Duration::from_millis(5));
	}
	e.to_string()
}

// ------------------------------------------------------------- processor

enum Wait {
	Done,
	TimedOut,
	Died,
}

/// A plugin instance running in a `cadenza-plugin-sandbox` child process.
/// Created by [`crate::instantiate_sandboxed`] /
/// [`crate::instantiate_hosted`].
pub struct SandboxedProcessor {
	desc: PluginDescriptor,
	info: &'static DeviceInfo,
	options: SandboxOptions,
	mode: ProcessMode,
	status: Arc<Status>,
	conn: Option<Connection>,
	crashed: bool,
	sample_rate: f32,
	max_block: usize,
	latency: u32,
	tail_seconds: f32,
	/// Per-block deadline once warmed up.
	block_timeout: Duration,
	warmup: bool,
	/// Queued parameter changes (NaN = none) and their indices, in order.
	pending: Vec<f32>,
	dirty: Vec<u32>,
	/// Last value set per parameter (NaN = never), replayed after a restart.
	current: Vec<f32>,
}

impl SandboxedProcessor {
	pub(crate) fn create(
		desc: &PluginDescriptor,
		sample_rate: f32,
		max_block: usize,
		state: Option<&[u8]>,
		mode: ProcessMode,
		options: SandboxOptions,
	) -> Result<Self, PluginError> {
		let (conn, reply) = Connection::spawn(desc, sample_rate, max_block, state, mode, &options)?;
		let n = desc.params.len();
		let status = Arc::new(Status {
			state: AtomicU8::new(HEALTHY),
			timeout_us: AtomicU64::new(0),
			plugin_id: desc.id.clone(),
			plugin_name: desc.name.clone(),
			role: desc.role,
			inner: Mutex::new(StatusInner {
				pid: Some(conn.pid),
				info: None,
				lost_detail: String::new(),
				last_state: state.map(<[u8]>::to_vec),
			}),
		});
		let mut p = Self {
			desc: desc.clone(),
			info: desc.device_info(),
			options,
			mode,
			status,
			conn: Some(conn),
			crashed: false,
			sample_rate,
			max_block: max_block.max(1),
			latency: 0,
			tail_seconds: 0.0,
			block_timeout: Duration::ZERO,
			warmup: true,
			pending: vec![f32::NAN; n],
			dirty: Vec::with_capacity(n),
			current: vec![f32::NAN; n],
		};
		p.apply_reply(&reply);
		p.update_timeout();
		Ok(p)
	}

	/// A cloneable handle to this instance's crash status.
	pub fn monitor(&self) -> SandboxMonitor {
		SandboxMonitor(self.status.clone())
	}

	/// The crash report, or `None` while healthy. Not realtime.
	pub fn crashed(&self) -> Option<CrashInfo> {
		self.status.crash_info()
	}

	/// Process id of the sandbox child, if one is running.
	pub fn pid(&self) -> Option<u32> {
		self.conn.as_ref().map(|c| c.pid)
	}

	/// Replaces the sandbox child with a fresh one (even if healthy), restoring
	/// the last saved/loaded state, the current activation and every parameter
	/// value set since. Not realtime: call it while the processor is not on
	/// the audio thread (e.g. on a graph being rebuilt), or re-create the
	/// device from [`SandboxMonitor::last_state`] instead.
	pub fn restart(&mut self) -> Result<(), PluginError> {
		// Settle the old crash report and stop monitors from looking at the
		// old pid before reaping it.
		let _ = self.status.crash_info();
		self.status.lock().pid = None;
		if let Some(conn) = self.conn.take() {
			kill_now(conn.pid);
			conn.close();
		}
		self.crashed = true;
		let state = self.status.lock().last_state.clone();
		let spawned = Connection::spawn(
			&self.desc,
			self.sample_rate,
			self.max_block,
			state.as_deref(),
			self.mode,
			&self.options,
		);
		let (conn, reply) = match spawned {
			Ok(c) => c,
			Err(e) => {
				let mut inner = self.status.lock();
				inner.info = None;
				inner.lost_detail = format!("restart failed: {e}");
				self.status.state.store(LOST, Ordering::Release);
				return Err(e);
			}
		};
		{
			let mut inner = self.status.lock();
			inner.pid = Some(conn.pid);
			inner.info = None;
			inner.lost_detail.clear();
		}
		self.status.state.store(HEALTHY, Ordering::Release);
		self.conn = Some(conn);
		self.crashed = false;
		self.warmup = true;
		self.apply_reply(&reply);
		self.dirty.clear();
		self.pending.fill(f32::NAN);
		for i in 0..self.current.len() {
			let v = self.current[i];
			if !v.is_nan() {
				self.set_param(i, v);
			}
		}
		Ok(())
	}

	fn apply_reply(&mut self, reply: &Reply) {
		// Values come from the untrusted child: keep them sane.
		self.latency = reply.latency.min(10 * 192_000);
		self.tail_seconds = if reply.tail_seconds.is_finite() {
			reply
				.tail_seconds
				.clamp(0.0, crate::clap::instance::MAX_TAIL_SECONDS)
		} else {
			0.0
		};
	}

	fn update_timeout(&mut self) {
		let block = Duration::from_secs_f64(
			2.0 * self.max_block as f64 / f64::from(self.sample_rate.max(1.0)),
		);
		self.block_timeout = self.options.realtime_timeout.max(block);
	}

	fn timeout(&self) -> Duration {
		match (self.mode, self.warmup) {
			(ProcessMode::Offline, _) => self.options.offline_timeout,
			(ProcessMode::Realtime, true) => self.options.warmup_timeout.max(self.block_timeout),
			(ProcessMode::Realtime, false) => self.block_timeout,
		}
	}

	/// Marks the instance crashed. Realtime-safe (atomics and `kill` only).
	fn fail(&mut self, how: Wait, timeout: Duration) {
		self.crashed = true;
		let state = match how {
			Wait::TimedOut => {
				self.status
					.timeout_us
					.store(timeout.as_micros() as u64, Ordering::Relaxed);
				TIMED_OUT
			}
			_ => DIED,
		};
		if let Some(conn) = &self.conn {
			// A hung child must not keep running (or touching the shared
			// block) behind our back.
			kill_now(conn.pid);
		}
		self.status.state.store(state, Ordering::Release);
	}

	/// Non-realtime failure on the control channel.
	fn lose(&mut self, detail: String) {
		self.crashed = true;
		self.status.lock().lost_detail = detail;
		if let Some(conn) = &self.conn {
			let dead = peek_exit(conn.pid).is_some();
			kill_now(conn.pid);
			self.status
				.state
				.store(if dead { DIED } else { LOST }, Ordering::Release);
		}
	}

	/// Posts one request through shared memory and waits for the child.
	/// Realtime-safe: no allocation, no locks.
	fn round_trip(&mut self, cmd: u32, frames: usize, transport: &Transport) -> Wait {
		let timeout = self.timeout();
		let Some(conn) = self.conn.as_mut() else {
			return Wait::Died;
		};
		let block = Block {
			words: conn.region.words(),
			layout: conn.layout,
		};
		block.set_request(cmd, frames, transport);
		let mut slot = 0;
		for &i in &self.dirty {
			let v = std::mem::replace(&mut self.pending[i as usize], f32::NAN);
			if block.write_param(slot, i, v) {
				slot += 1;
			}
		}
		self.dirty.clear();
		block.set_param_count(slot);

		let seq = conn.seq.wrapping_add(1);
		conn.seq = seq;
		let (req, ack) = (block.word(W_REQ_SEQ), block.word(W_ACK_SEQ));
		req.store(seq, Ordering::Release);
		sync::wake(req);

		let slice = match self.mode {
			ProcessMode::Realtime => Duration::from_micros(500),
			ProcessMode::Offline => Duration::from_millis(10),
		};
		let start = Instant::now();
		let pid = conn.pid;
		sync::spin_until_changed(ack, seq.wrapping_sub(1), 4_000);
		loop {
			let seen = ack.load(Ordering::Acquire);
			if seen == seq {
				return Wait::Done;
			}
			let elapsed = start.elapsed();
			if elapsed >= timeout {
				return Wait::TimedOut;
			}
			sync::wait(ack, seen, (timeout - elapsed).min(slice));
			if ack.load(Ordering::Acquire) == seq {
				return Wait::Done;
			}
			if peek_exit(pid).is_some() {
				return Wait::Died;
			}
		}
	}

	fn bypass(&self, left: &mut [f32], right: &mut [f32]) {
		if self.desc.role == PluginRole::Instrument {
			left.fill(0.0);
			right.fill(0.0);
		}
	}

	/// Sends a non-realtime request; `None` if the instance is (or just
	/// became) crashed.
	fn control(&mut self, req: &Request) -> Option<Reply> {
		if self.crashed || self.conn.is_none() {
			return None;
		}
		if !self.dirty.is_empty() {
			let t = Transport::default();
			match self.round_trip(CMD_PARAMS, 0, &t) {
				Wait::Done => {}
				how => {
					let timeout = self.timeout();
					self.fail(how, timeout);
					return None;
				}
			}
		}
		let timeout = self.options.control_timeout;
		let conn = self.conn.as_mut()?;
		match conn.request(req, timeout) {
			Ok(reply) => {
				self.apply_reply(&reply);
				Some(reply)
			}
			Err(e) => {
				self.lose(e.to_string());
				None
			}
		}
	}

	fn remember_state(&self, data: &[u8]) {
		self.status.lock().last_state = Some(data.to_vec());
	}
}

impl Processor for SandboxedProcessor {
	fn info(&self) -> &'static DeviceInfo {
		self.info
	}

	fn prepare(&mut self, sample_rate: f32, max_block: usize) {
		self.sample_rate = sample_rate;
		self.max_block = max_block.max(1);
		self.update_timeout();
		self.warmup = true;
		self.control(&Request::Prepare {
			sample_rate,
			max_block,
		});
	}

	fn set_param(&mut self, index: usize, value: f32) {
		if index >= self.pending.len() || value.is_nan() {
			return;
		}
		if self.pending[index].is_nan() && self.dirty.len() < self.dirty.capacity() {
			self.dirty.push(index as u32);
		}
		self.pending[index] = value;
		self.current[index] = value;
	}

	fn process(
		&mut self,
		left: &mut [f32],
		right: &mut [f32],
		events: &[MidiEvent],
		transport: &Transport,
	) {
		let n = left.len().min(right.len());
		if n == 0 {
			return;
		}
		if self.crashed {
			self.bypass(left, right);
			return;
		}
		let Some(capacity) = self.conn.as_ref().map(|c| c.layout.capacity) else {
			self.bypass(left, right);
			return;
		};
		let mut start = 0;
		let mut remaining_events = events;
		while start < n {
			let frames = capacity.min(n - start);
			let end = start + frames;
			// Events are sorted; the last chunk takes any late offsets too.
			let split = if end < n {
				remaining_events.partition_point(|e| (e.offset() as usize) < end)
			} else {
				remaining_events.len()
			};
			let (chunk_events, rest) = remaining_events.split_at(split);
			remaining_events = rest;
			let Some(block) = self.conn.as_ref().map(Connection::block) else {
				return;
			};
			block.write_events_shifted(chunk_events, start as u32);
			block.write_audio(false, &left[start..end]);
			block.write_audio(true, &right[start..end]);
			let mut chunk_transport = *transport;
			if start > 0 {
				chunk_transport.beat_position +=
					start as f64 / f64::from(self.sample_rate.max(1.0)) * transport.bpm / 60.0;
			}
			match self.round_trip(CMD_PROCESS, frames, &chunk_transport) {
				Wait::Done => {
					self.warmup = false;
					let Some(block) = self.conn.as_ref().map(Connection::block) else {
						return;
					};
					block.read_audio(false, &mut left[start..end]);
					block.read_audio(true, &mut right[start..end]);
				}
				how => {
					let timeout = self.timeout();
					self.fail(how, timeout);
					self.bypass(left, right);
					return;
				}
			}
			start = end;
		}
	}

	fn reset(&mut self) {
		if self.crashed {
			return;
		}
		let t = Transport::default();
		match self.round_trip(CMD_RESET, 0, &t) {
			Wait::Done => {}
			how => {
				let timeout = self.timeout();
				self.fail(how, timeout);
			}
		}
	}

	fn latency(&self) -> u32 {
		self.latency
	}

	fn tail_seconds(&self) -> f32 {
		self.tail_seconds
	}

	fn save_state(&mut self) -> Option<Vec<u8>> {
		match self.control(&Request::SaveState) {
			Some(reply) => {
				let data = base64::engine::general_purpose::STANDARD
					.decode(reply.state?)
					.ok()?;
				self.remember_state(&data);
				Some(data)
			}
			None => self.status.lock().last_state.clone(),
		}
	}

	fn load_state(&mut self, data: &[u8]) -> bool {
		let accepted = self
			.control(&Request::LoadState {
				data: encode_state(data),
			})
			.is_some_and(|r| r.accepted);
		if accepted || self.crashed {
			// After a crash, keep it for `restart`.
			self.remember_state(data);
		}
		accepted
	}
}

impl Drop for SandboxedProcessor {
	fn drop(&mut self) {
		self.status.lock().pid = None;
		if let Some(conn) = self.conn.take() {
			conn.close();
		}
	}
}
