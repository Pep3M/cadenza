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
//! - **Audio** goes through a shared memory block (layout in `shm.rs`,
//!   `mlock`ed): a header of atomics (request/ack sequence numbers, command,
//!   flags), a table of sub-blocks (frames, transport, sidechain flag, where
//!   their events and parameter changes end), a fixed-capacity event area, a
//!   parameter-change area and in-place audio channels of
//!   `max(max_block, 4096)` frames. The audio thread stages each `process`
//!   call (input, events, sidechain key, pending parameter changes) and, once
//!   a chunk is complete, writes it, bumps the request sequence and wakes the
//!   child (Linux futex), then waits for the acknowledgement with a bounded
//!   deadline. The child runs one `process` call per sub-block, so batching
//!   changes nothing the plugin sees. The audio thread never allocates, locks
//!   or does I/O.
//! - **Batching** ([`SandboxOptions::realtime_chunk`], default 128 frames):
//!   the engine runs devices in 32-frame quanta; one round trip per quantum
//!   means ~16 wake-ups per 512-frame callback, each a chance for the
//!   scheduler to be late. Collecting 4 quanta per round trip costs 3 quanta
//!   (96 frames, 2 ms at 48 kHz) of latency, reported through
//!   `Processor::latency` so delay compensation keeps everything aligned
//!   (bypassed effects stay aligned too: their dry signal goes through the
//!   same delay). Offline renders and blocks of 128+ frames are not batched.
//! - **Control** (create, prepare, state, shutdown) is length-prefixed JSON
//!   over the child's stdin/stdout, off the audio thread, with a deadline.
//! - **Editors** (see [`crate::editor`]) use the same control channel through
//!   a [`crate::PluginEditor`] handle that does not need the processor (which
//!   may be on the audio thread meanwhile): the child creates the window and
//!   runs the GUI on its own plugin main thread, queues edits made there, and
//!   hands them over when the host polls. With an editor open and the
//!   transport stopped, `Processor::idle` sends an idle round trip so the
//!   plugin can flush parameter changes.
//! - **Scheduling**: the child's audio thread asks for `SCHED_FIFO` (directly
//!   or through RealtimeKit; see `rt.rs`), so neither the plugin's own GUI
//!   thread nor a busy desktop can delay its answers.
//!
//! # Failure semantics
//!
//! Late is not dead. In realtime mode a request that misses its deadline
//! (`max(realtime_timeout = 2 ms, the request's audio duration)`; 500 ms for
//! the first request after (re)activation) only costs that chunk: it outputs
//! silence (instruments) or the dry input (effects), the request stays
//! outstanding and its answer is discarded when it comes (the host never
//! reads or writes the shared block before the child acknowledged the
//! request's sequence number), later chunks are skipped without waiting
//! while the child is busy (keeping their note-offs and parameter changes for
//! the next delivered chunk), and [`SandboxStats`] counts it all. Only a
//! child that gives no on-time answer for [`SandboxOptions::hang_timeout`]
//! (2 s) is declared hung. A hung (or offline: late, 30 s) or dead child is
//! marked crashed, `SIGKILL`ed, and from then on bypassed without touching
//! it. `latency`/`tail_seconds` keep their last values so delay compensation
//! stays stable; `save_state` returns the last state saved or loaded, so a
//! project can still be saved.
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
//! weaker isolation), Windows support (named file mappings + events).

#[cfg(unix)]
mod child;
mod protocol;
#[cfg(unix)]
mod rt;
mod shm;
mod sync;

use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use base64::Engine as _;
use cadenza_dsp::{DeviceInfo, MidiEvent, Processor, Transport};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::descriptor::{PluginDescriptor, PluginRole};
use crate::editor::EditorEvent;
use crate::error::PluginError;
use crate::{ProcessMode, encode_state};
use protocol::{Reply, Request};
use shm::{
	Block, CH_AUX, CH_LEFT, CH_RIGHT, CH_SIDECHAIN, CMD_IDLE, CMD_PARAMS, CMD_PROCESS, Layout,
	Region, W_ACK_SEQ, W_REQ_SEQ,
};

use crate::ports::KeyInput;

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

/// Sub-blocks one shared-memory request can carry (batched quanta, or the
/// pieces of a block larger than the shared buffer).
const MAX_SUB_BLOCKS: usize = 16;
/// Note-offs kept from dropped chunks until the plugin can take them.
const HELD_NOTE_OFFS: usize = 256;

/// Deadlines, batching and debugging switches for sandboxed instances.
#[derive(Debug, Clone, PartialEq)]
pub struct SandboxOptions {
	/// Minimum per-request deadline in realtime mode; the effective deadline
	/// is `max(realtime_timeout, duration of the request's audio)`. A request
	/// that misses it is **late**, not fatal: that chunk outputs silence
	/// (instruments) or the dry input (effects), the late reply is discarded
	/// when it arrives, and processing goes on (see [`SandboxStats`]).
	pub realtime_timeout: Duration,
	/// Realtime: how long an instance may go without a single on-time answer
	/// before it is declared hung (killed, bypassed and reported as a crash).
	pub hang_timeout: Duration,
	/// Deadline of the first request after creation, `prepare` or `restart`
	/// (plugins often do lazy setup there).
	pub warmup_timeout: Duration,
	/// Per-request deadline in offline mode (missing it is fatal: an offline
	/// render has no reason to be late).
	pub offline_timeout: Duration,
	/// Deadline for control operations (load, activation, state).
	pub control_timeout: Duration,
	/// Realtime batching: the host collects this many frames (rounded down
	/// to a multiple of the processor's `max_block`, at most 16 blocks) and
	/// sends them to the child in one round trip, as one `process` call per
	/// original block (events, parameter changes and transport stay
	/// sample-accurate). Costs `chunk - max_block` frames of latency,
	/// included in `Processor::latency` so delay compensation absorbs it;
	/// `0` or anything up to `max_block` processes every block synchronously
	/// with no added latency.
	pub realtime_chunk: usize,
	/// Same as [`Self::realtime_chunk`] for offline renders (off by default:
	/// the round trip is cheap and the added latency would show up in
	/// `MixReport::latency_samples`).
	pub offline_chunk: usize,
	/// `SCHED_FIFO` priority requested for the child's audio thread in
	/// realtime mode (directly or through RealtimeKit, falling back to a
	/// negative nice value); `0` keeps normal scheduling.
	pub realtime_priority: u8,
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
			hang_timeout: Duration::from_secs(2),
			warmup_timeout: Duration::from_millis(500),
			offline_timeout: Duration::from_secs(30),
			control_timeout: protocol::CONTROL_TIMEOUT,
			realtime_chunk: 128,
			offline_chunk: 0,
			realtime_priority: 10,
			core_dumps: true,
			executable: None,
		}
	}
}

/// Non-fatal realtime trouble of one sandboxed instance (cumulative since it
/// was created). Every late request is an audible dropout of this plugin
/// only; the rest of the graph keeps its timing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SandboxStats {
	/// Requests that missed their deadline.
	pub late_blocks: u64,
	/// Frames output as silence (instruments) or dry input (effects) because
	/// the plugin was late or still busy with an earlier request.
	pub dropped_frames: u64,
	/// Longest time the plugin went without an on-time answer.
	pub longest_stall_ms: u64,
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
	/// How the child's audio thread is scheduled, as it reported.
	scheduling: Option<String>,
}

/// Crash status shared between a processor and its monitors.
struct Status {
	state: AtomicU8,
	timeout_us: AtomicU64,
	late_blocks: AtomicU64,
	dropped_frames: AtomicU64,
	longest_stall_us: AtomicU64,
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

	/// Late requests and dropped audio so far. Wait-free (atomics only).
	pub fn stats(&self) -> SandboxStats {
		let s = &self.0;
		SandboxStats {
			late_blocks: s.late_blocks.load(Ordering::Relaxed),
			dropped_frames: s.dropped_frames.load(Ordering::Relaxed),
			longest_stall_ms: s.longest_stall_us.load(Ordering::Relaxed).div_ceil(1000),
		}
	}

	/// How the sandbox child's audio thread is scheduled (e.g.
	/// `SCHED_FIFO 10 (RealtimeKit)`), as reported by the child.
	pub fn scheduling(&self) -> Option<String> {
		self.0.lock().scheduling.clone()
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

/// The control pipes of one sandbox child.
struct Pipes {
	stdin: ChildStdin,
	stdout: ChildStdout,
	next_id: u64,
}

impl Pipes {
	#[cfg(unix)]
	fn request(&mut self, req: &Request, timeout: Duration) -> std::io::Result<Reply> {
		self.next_id += 1;
		let id = self.next_id;
		protocol::write_frame(&mut self.stdin, &protocol::Envelope { id, body: req })?;
		protocol::read_reply_until(&mut self.stdout, id, Instant::now() + timeout)
	}

	#[cfg(not(unix))]
	fn request(&mut self, _req: &Request, _timeout: Duration) -> std::io::Result<Reply> {
		Err(std::io::Error::other(
			"sandboxed hosting is not implemented on this OS",
		))
	}
}

/// The control channel of the current child, shared by its processor and its
/// editor handles (never used on the audio thread).
struct Link {
	pipes: Mutex<Option<Pipes>>,
	/// An editor is open (as far as the host knows).
	editor_open: AtomicBool,
	status: Arc<Status>,
	control_timeout: Duration,
}

impl Link {
	fn lock(&self) -> MutexGuard<'_, Option<Pipes>> {
		self.pipes.lock().unwrap_or_else(|e| e.into_inner())
	}

	fn request(&self, req: &Request, timeout: Duration) -> std::io::Result<Reply> {
		match self.lock().as_mut() {
			Some(p) => p.request(req, timeout),
			None => Err(std::io::Error::new(
				std::io::ErrorKind::NotConnected,
				"the sandbox process is gone",
			)),
		}
	}

	/// Asks the child to exit and drops the pipes (EOF on its stdin).
	fn shutdown(&self) {
		if let Some(mut p) = self.lock().take() {
			p.next_id += 1;
			let _ = protocol::write_frame(
				&mut p.stdin,
				&protocol::Envelope {
					id: p.next_id,
					body: &Request::Shutdown,
				},
			);
		}
	}
}

/// One live sandbox child and its shared memory.
struct Connection {
	child: Child,
	pid: u32,
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
	) -> Result<(Connection, Pipes, Reply), PluginError> {
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
			// Every sub-block may change every parameter.
			param_capacity: (desc.params.len() * MAX_SUB_BLOCKS).min(shm::MAX_PARAMS as usize),
			aux_outputs: desc.aux_outputs.len().min(shm::MAX_AUX_OUTPUTS as usize),
			block_capacity: MAX_SUB_BLOCKS,
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
		if options.realtime_priority > 0 {
			cmd.arg("--rt-priority")
				.arg(options.realtime_priority.to_string());
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
		let mut pipes = Pipes {
			stdin,
			stdout,
			next_id: 0,
		};
		let mut conn = Connection {
			child,
			pid,
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
		let reply = pipes.request(&init, options.control_timeout);
		// The child has mapped the region (or is gone): drop the name.
		conn.region.unlink();
		let reply = match reply {
			Ok(r) => r,
			Err(e) => {
				let detail = describe_spawn_failure(&mut conn.child, e);
				drop(pipes);
				reap_later(conn.child);
				return Err(fail(detail));
			}
		};
		if let Some(error) = reply.error.clone() {
			drop(pipes);
			reap_later(conn.child);
			return Err(if reply.state_rejected {
				PluginError::State {
					id: desc.id.clone(),
					reason: error,
				}
			} else {
				fail(error)
			});
		}
		Ok((conn, pipes, reply))
	}
}

/// Editor side of a sandboxed instance (see [`crate::PluginEditor`]).
pub(crate) struct EditorLink {
	link: Arc<Link>,
}

/// How long an editor poll may take before it is skipped.
const EDITOR_POLL_TIMEOUT: Duration = Duration::from_secs(2);
/// How long closing an editor may take.
const EDITOR_CLOSE_TIMEOUT: Duration = Duration::from_secs(5);

impl EditorLink {
	pub fn plugin_id(&self) -> &str {
		&self.link.status.plugin_id
	}

	fn error(&self, reason: impl Into<String>) -> PluginError {
		PluginError::Editor {
			id: self.plugin_id().to_string(),
			reason: reason.into(),
		}
	}

	fn alive(&self) -> Result<(), PluginError> {
		if self.link.status.state.load(Ordering::Acquire) != HEALTHY {
			return Err(self
				.error("the plugin's sandbox process crashed; reload the project to restart it"));
		}
		Ok(())
	}

	pub fn open(&self, title: &str) -> Result<(), PluginError> {
		self.alive()?;
		let reply = self
			.link
			.request(
				&Request::EditorOpen {
					title: title.to_string(),
				},
				self.link.control_timeout,
			)
			.map_err(|e| self.error(format!("the sandbox process did not answer ({e})")))?;
		if let Some(e) = reply.error {
			return Err(self.error(e));
		}
		self.link.editor_open.store(true, Ordering::Release);
		Ok(())
	}

	pub fn close(&self) -> Result<(), PluginError> {
		if !self.link.editor_open.swap(false, Ordering::AcqRel) {
			return Ok(());
		}
		self.link
			.request(&Request::EditorClose, EDITOR_CLOSE_TIMEOUT)
			.map(|_| ())
			.map_err(|e| self.error(format!("the sandbox process did not answer ({e})")))
	}

	pub fn is_open(&self) -> bool {
		self.link.editor_open.load(Ordering::Acquire)
	}

	pub fn poll(&self) -> Vec<EditorEvent> {
		if !self.is_open() {
			return Vec::new();
		}
		let lost = || {
			// The window disappeared with the process.
			self.link.editor_open.store(false, Ordering::Release);
			vec![EditorEvent::Closed]
		};
		if self.link.status.state.load(Ordering::Acquire) != HEALTHY {
			return lost();
		}
		match self.link.request(&Request::EditorPoll, EDITOR_POLL_TIMEOUT) {
			Ok(reply) => {
				if reply.events.contains(&EditorEvent::Closed) {
					self.link.editor_open.store(false, Ordering::Release);
				}
				reply.events
			}
			Err(e) if e.kind() == std::io::ErrorKind::TimedOut => Vec::new(),
			Err(_) => lost(),
		}
	}

	pub fn save_state(&self) -> Result<Option<Vec<u8>>, PluginError> {
		self.alive()?;
		let reply = self
			.link
			.request(&Request::EditorState, self.link.control_timeout)
			.map_err(|e| self.error(format!("the sandbox process did not answer ({e})")))?;
		if let Some(e) = reply.error {
			return Err(self.error(e));
		}
		reply
			.state
			.map(|s| {
				base64::engine::general_purpose::STANDARD
					.decode(s)
					.map_err(|e| self.error(format!("bad state from the sandbox ({e})")))
			})
			.transpose()
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

/// Outcome of one shared-memory round trip.
enum Wait {
	Done,
	TimedOut,
	Died,
}

/// Why the child cannot take a request right now.
enum NotReady {
	/// Still working on a late request: skip this one.
	Busy,
	Died,
	/// No on-time answer for this long (at least the hang timeout).
	Hung(Duration),
}

/// What became of a group of staged sub-blocks.
enum Sent {
	/// Processed in time: its output is in the shared buffer.
	OnTime,
	/// Delivered, but the answer missed the deadline (output lost).
	Late,
	/// Not delivered (the child was busy, or is gone).
	Undelivered,
}

/// Input collected for the next round trip: up to `chunk` frames made of
/// sub-blocks (one per `process` call, or per shared-buffer-sized piece of a
/// larger call). Event offsets are relative to the chunk start.
#[derive(Default)]
struct Staging {
	chunk: usize,
	frames: usize,
	left: Vec<f32>,
	right: Vec<f32>,
	key_left: Vec<f32>,
	key_right: Vec<f32>,
	events: Vec<MidiEvent>,
	/// (parameter index, plain value) in the order they were set.
	params: Vec<(u32, f32)>,
	blocks: Vec<shm::SubBlock>,
}

impl Staging {
	fn new(chunk: usize, params: usize) -> Self {
		Self {
			chunk,
			frames: 0,
			left: vec![0.0; chunk],
			right: vec![0.0; chunk],
			key_left: vec![0.0; chunk],
			key_right: vec![0.0; chunk],
			events: Vec::with_capacity(EVENT_CAPACITY),
			params: Vec::with_capacity(params),
			blocks: Vec::with_capacity(MAX_SUB_BLOCKS),
		}
	}

	fn clear(&mut self) {
		self.frames = 0;
		self.events.clear();
		self.params.clear();
		self.blocks.clear();
	}

	/// Index of the first event / parameter change of sub-block `b`.
	fn starts(&self, b: usize) -> (usize, usize) {
		match b.checked_sub(1).and_then(|p| self.blocks.get(p)) {
			Some(prev) => (prev.events_end as usize, prev.params_end as usize),
			None => (0, 0),
		}
	}

	/// First frame of sub-block `b`.
	fn frame_of(&self, b: usize) -> usize {
		self.blocks[..b.min(self.blocks.len())]
			.iter()
			.map(|s| s.frames as usize)
			.sum()
	}
}

/// Processed output waiting to be played: a ring of `channels` x `capacity`
/// frames (main left/right, then the extra outputs).
#[derive(Default)]
struct Fifo {
	chans: Vec<Vec<f32>>,
	head: usize,
	len: usize,
}

impl Fifo {
	fn new(channels: usize, capacity: usize) -> Self {
		Self {
			chans: vec![vec![0.0; capacity.max(1)]; channels],
			head: 0,
			len: 0,
		}
	}

	fn capacity(&self) -> usize {
		self.chans.first().map_or(1, Vec::len)
	}

	/// Empties the ring, then queues `silence` frames of silence (the batch
	/// latency).
	fn reset(&mut self, silence: usize) {
		for c in &mut self.chans {
			c.fill(0.0);
		}
		self.head = 0;
		self.len = silence.min(self.capacity());
	}

	/// The (up to) two ring ranges `n` frames long starting `at` frames after
	/// the head.
	fn ranges(&self, at: usize, n: usize) -> [std::ops::Range<usize>; 2] {
		let cap = self.capacity();
		let start = (self.head + at) % cap;
		let first = n.min(cap - start);
		[start..start + first, 0..n - first]
	}

	/// Appends `n` frames; `fill(channel, offset, dst)` writes frames
	/// `offset..offset + dst.len()` of the new data into `dst`.
	fn push(&mut self, n: usize, mut fill: impl FnMut(usize, usize, &mut [f32])) {
		let n = n.min(self.capacity() - self.len);
		let ranges = self.ranges(self.len, n);
		for (c, chan) in self.chans.iter_mut().enumerate() {
			let mut offset = 0;
			for r in ranges.clone() {
				let len = r.len();
				fill(c, offset, &mut chan[r]);
				offset += len;
			}
		}
		self.len += n;
	}

	/// Removes `n` frames; `take(channel, offset, src)` receives frames
	/// `offset..offset + src.len()`. Missing frames read as silence.
	fn pop(&mut self, n: usize, mut take: impl FnMut(usize, usize, &[f32])) {
		let have = n.min(self.len);
		let ranges = self.ranges(0, have);
		for (c, chan) in self.chans.iter().enumerate() {
			let mut offset = 0;
			for r in ranges.clone() {
				let len = r.len();
				take(c, offset, &chan[r]);
				offset += len;
			}
		}
		self.head = (self.head + have) % self.capacity();
		self.len -= have;
	}
}

/// A plugin instance running in a `cadenza-plugin-sandbox` child process.
/// Created by [`crate::instantiate_sandboxed`] /
/// [`crate::instantiate_hosted`].
///
/// Realtime flow per `process` call: the input, events, sidechain key and
/// pending parameter changes are staged; once a chunk is full (see
/// [`SandboxOptions::realtime_chunk`]) it goes to the child in one round
/// trip; the call's output is taken from a FIFO that runs `chunk - max_block`
/// frames behind (that latency is reported). A late round trip costs that
/// chunk's audio only; while the child is still busy later chunks are skipped
/// without waiting; the stall is fatal only after
/// [`SandboxOptions::hang_timeout`].
pub struct SandboxedProcessor {
	desc: PluginDescriptor,
	info: &'static DeviceInfo,
	options: SandboxOptions,
	mode: ProcessMode,
	status: Arc<Status>,
	conn: Option<Connection>,
	link: Arc<Link>,
	crashed: bool,
	sample_rate: f32,
	max_block: usize,
	latency: u32,
	tail_seconds: f32,
	/// Per-request deadline once warmed up.
	block_timeout: Duration,
	warmup: bool,
	/// Queued parameter changes (NaN = none) and their indices, in order.
	pending: Vec<f32>,
	dirty: Vec<u32>,
	/// Last value set per parameter (NaN = never), replayed after a restart.
	current: Vec<f32>,
	sidechain: KeyInput,
	/// Extra outputs of the last block: left/right per output, `max_block` long.
	aux: Vec<Vec<f32>>,
	/// Frames of the last successful `process` (for `aux_output`).
	last_frames: usize,
	staging: Staging,
	fifo: Fifo,
	/// Frames the batching adds to the plugin's own latency.
	batch_latency: u32,
	/// Sequence number of a request that missed its deadline and is still
	/// unanswered: the shared block belongs to the child until it answers.
	outstanding: Option<u32>,
	/// Since when no request was answered in time.
	stall_since: Option<Instant>,
	/// `reset` was called: the next request tells the child to reset first.
	reset_pending: bool,
	/// Note-offs of chunks that never reached the plugin.
	held: Vec<MidiEvent>,
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
		let (conn, pipes, reply) =
			Connection::spawn(desc, sample_rate, max_block, state, mode, &options)?;
		let n = desc.params.len();
		let status = Arc::new(Status {
			state: AtomicU8::new(HEALTHY),
			timeout_us: AtomicU64::new(0),
			late_blocks: AtomicU64::new(0),
			dropped_frames: AtomicU64::new(0),
			longest_stall_us: AtomicU64::new(0),
			plugin_id: desc.id.clone(),
			plugin_name: desc.name.clone(),
			role: desc.role,
			inner: Mutex::new(StatusInner {
				pid: Some(conn.pid),
				info: None,
				lost_detail: String::new(),
				last_state: state.map(<[u8]>::to_vec),
				scheduling: reply.scheduling.clone(),
			}),
		});
		let link = Arc::new(Link {
			pipes: Mutex::new(Some(pipes)),
			editor_open: AtomicBool::new(false),
			status: status.clone(),
			control_timeout: options.control_timeout,
		});
		let mut p = Self {
			desc: desc.clone(),
			info: desc.device_info(),
			options,
			mode,
			status,
			conn: Some(conn),
			link,
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
			sidechain: KeyInput::default(),
			aux: Vec::new(),
			last_frames: 0,
			staging: Staging::default(),
			fifo: Fifo::default(),
			batch_latency: 0,
			outstanding: None,
			stall_since: None,
			reset_pending: false,
			held: Vec::with_capacity(HELD_NOTE_OFFS),
		};
		p.apply_reply(&reply);
		p.allocate_buffers();
		Ok(p)
	}

	/// Frames of the shared buffer (one request carries at most this many).
	fn capacity(&self) -> usize {
		self.conn
			.as_ref()
			.map_or(MIN_CAPACITY, |c| c.layout.capacity)
	}

	/// Staging, FIFO, sidechain and extra-output buffers and the deadline for
	/// the current `max_block` and mode; empties the pipeline.
	fn allocate_buffers(&mut self) {
		let target = match self.mode {
			ProcessMode::Realtime => self.options.realtime_chunk,
			ProcessMode::Offline => self.options.offline_chunk,
		};
		// Batch only blocks that fit the shared buffer several times over.
		let k = if self.max_block * 2 <= self.capacity() {
			(target / self.max_block).clamp(1, MAX_SUB_BLOCKS)
		} else {
			1
		};
		let chunk = k * self.max_block;
		// No more changes than one request can carry (the rest stay queued).
		let params = self
			.conn
			.as_ref()
			.map_or(0, |c| c.layout.param_capacity)
			.min(self.pending.len() * MAX_SUB_BLOCKS);
		self.staging = Staging::new(chunk, params);
		self.batch_latency = (chunk - self.max_block) as u32;
		self.sidechain.allocate(self.max_block);
		let aux = self
			.conn
			.as_ref()
			.map_or(0, |c| c.layout.aux_outputs.min(self.desc.aux_outputs.len()));
		self.aux = vec![vec![0.0; self.max_block]; 2 * aux];
		self.fifo = Fifo::new(2 + 2 * aux, chunk + self.max_block);
		self.fifo.reset(self.batch_latency as usize);
		self.last_frames = 0;
		let audio = Duration::from_secs_f64(
			self.chunk_frames_per_request() as f64 / f64::from(self.sample_rate.max(1.0)),
		);
		self.block_timeout = self.options.realtime_timeout.max(audio);
		self.warmup = true;
	}

	/// Frames in one full request.
	fn chunk_frames_per_request(&self) -> usize {
		self.staging.chunk.min(self.capacity())
	}

	/// A cloneable handle to this instance's crash status.
	pub fn monitor(&self) -> SandboxMonitor {
		SandboxMonitor(self.status.clone())
	}

	/// The crash report, or `None` while healthy. Not realtime.
	pub fn crashed(&self) -> Option<CrashInfo> {
		self.status.crash_info()
	}

	/// A handle to the plugin's editor, which runs in the sandbox process.
	pub fn editor(&self) -> crate::PluginEditor {
		crate::PluginEditor::sandboxed(EditorLink {
			link: self.link.clone(),
		})
	}

	/// Name of the shared memory block of the running child (unlinked once
	/// the child mapped it). For leak tests.
	#[doc(hidden)]
	pub fn shm_name(&self) -> Option<String> {
		self.conn.as_ref().map(|c| c.region.name().to_string())
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
			self.link.shutdown();
			reap_later(conn.child);
		}
		self.link.editor_open.store(false, Ordering::Release);
		self.crashed = true;
		self.outstanding = None;
		self.stall_since = None;
		self.reset_pending = false;
		let state = self.status.lock().last_state.clone();
		let spawned = Connection::spawn(
			&self.desc,
			self.sample_rate,
			self.max_block,
			state.as_deref(),
			self.mode,
			&self.options,
		);
		let (conn, pipes, reply) = match spawned {
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
			inner.scheduling = reply.scheduling.clone();
		}
		*self.link.lock() = Some(pipes);
		self.status.state.store(HEALTHY, Ordering::Release);
		self.conn = Some(conn);
		self.crashed = false;
		self.apply_reply(&reply);
		self.allocate_buffers();
		self.held.clear();
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

	fn timeout(&self) -> Duration {
		match (self.mode, self.warmup) {
			(ProcessMode::Offline, _) => self.options.offline_timeout,
			(ProcessMode::Realtime, true) => self.options.warmup_timeout.max(self.block_timeout),
			(ProcessMode::Realtime, false) => self.block_timeout,
		}
	}

	/// Marks the instance crashed, reporting `reported` as how long it hung
	/// for `Wait::TimedOut`. Realtime-safe (atomics and `kill` only).
	fn fail(&mut self, how: Wait, reported: Duration) {
		self.crashed = true;
		self.outstanding = None;
		let state = match how {
			Wait::TimedOut => {
				self.status
					.timeout_us
					.store(reported.as_micros() as u64, Ordering::Relaxed);
				self.status
					.longest_stall_us
					.fetch_max(reported.as_micros() as u64, Ordering::Relaxed);
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
		self.outstanding = None;
		self.status.lock().lost_detail = detail;
		if let Some(conn) = &self.conn {
			let dead = peek_exit(conn.pid).is_some();
			kill_now(conn.pid);
			self.status
				.state
				.store(if dead { DIED } else { LOST }, Ordering::Release);
		}
	}

	/// Whether the shared block is ours to write. A late request answered
	/// meanwhile is retired here: its output belongs to audio that was
	/// already replaced, so it is never read. Realtime-safe.
	fn ready(&mut self) -> Result<(), NotReady> {
		let Some(seq) = self.outstanding else {
			return Ok(());
		};
		let Some(conn) = self.conn.as_ref() else {
			return Err(NotReady::Died);
		};
		if conn.block().word(W_ACK_SEQ).load(Ordering::Acquire) == seq {
			self.outstanding = None;
			return Ok(());
		}
		if peek_exit(conn.pid).is_some() {
			return Err(NotReady::Died);
		}
		let stalled = self.stall_since.map_or(Duration::ZERO, |s| s.elapsed());
		if stalled >= self.options.hang_timeout {
			return Err(NotReady::Hung(stalled));
		}
		Err(NotReady::Busy)
	}

	/// Handles a child that cannot take a request: `true` if it is merely
	/// busy (skip), `false` once it was declared dead or hung.
	fn not_ready(&mut self, why: NotReady) -> bool {
		match why {
			NotReady::Busy => true,
			NotReady::Died => {
				self.fail(Wait::Died, Duration::ZERO);
				false
			}
			NotReady::Hung(stalled) => {
				self.fail(Wait::TimedOut, stalled);
				false
			}
		}
	}

	/// Posts the request already written to the shared block and waits for
	/// the answer until `timeout`. Realtime-safe: no allocation, no locks.
	fn exchange(&mut self, timeout: Duration) -> (Wait, u32, Instant) {
		let start = Instant::now();
		let Some(conn) = self.conn.as_mut() else {
			return (Wait::Died, 0, start);
		};
		let seq = conn.seq.wrapping_add(1);
		conn.seq = seq;
		self.reset_pending = false;
		let block = Block {
			words: conn.region.words(),
			layout: conn.layout,
		};
		let (req, ack) = (block.word(W_REQ_SEQ), block.word(W_ACK_SEQ));
		req.store(seq, Ordering::Release);
		sync::wake(req);

		let slice = match self.mode {
			ProcessMode::Realtime => Duration::from_micros(500),
			ProcessMode::Offline => Duration::from_millis(10),
		};
		let pid = conn.pid;
		sync::spin_until_changed(ack, seq.wrapping_sub(1), 4_000);
		loop {
			let seen = ack.load(Ordering::Acquire);
			if seen == seq {
				return (Wait::Done, seq, start);
			}
			let elapsed = start.elapsed();
			if elapsed >= timeout {
				return (Wait::TimedOut, seq, start);
			}
			sync::wait(ack, seen, (timeout - elapsed).min(slice));
			if ack.load(Ordering::Acquire) == seq {
				return (Wait::Done, seq, start);
			}
			if peek_exit(pid).is_some() {
				return (Wait::Died, seq, start);
			}
		}
	}

	/// Posts the written request with the processing deadline and applies
	/// the late/hang policy. Realtime-safe.
	fn send(&mut self) -> Sent {
		let timeout = self.timeout();
		match self.exchange(timeout) {
			(Wait::Done, ..) => {
				self.warmup = false;
				if let Some(since) = self.stall_since.take() {
					self.status
						.longest_stall_us
						.fetch_max(since.elapsed().as_micros() as u64, Ordering::Relaxed);
				}
				Sent::OnTime
			}
			(Wait::TimedOut, seq, posted) => {
				if self.mode == ProcessMode::Offline {
					self.fail(Wait::TimedOut, timeout);
					return Sent::Undelivered;
				}
				self.status.late_blocks.fetch_add(1, Ordering::Relaxed);
				self.outstanding = Some(seq);
				let since = *self.stall_since.get_or_insert(posted);
				let stalled = since.elapsed();
				if stalled >= self.options.hang_timeout {
					self.fail(Wait::TimedOut, stalled);
				}
				Sent::Late
			}
			(Wait::Died, ..) => {
				self.fail(Wait::Died, Duration::ZERO);
				Sent::Undelivered
			}
		}
	}

	fn request_flags(&self) -> u32 {
		if self.reset_pending {
			shm::FLAG_RESET
		} else {
			0
		}
	}

	/// Writes a request without audio (`cmd`) carrying the queued (unstaged)
	/// parameter changes. `false` without a child.
	fn write_params_request(&mut self, cmd: u32) -> bool {
		let flags = self.request_flags();
		let Some(block) = self.conn.as_ref().map(Connection::block) else {
			return false;
		};
		let mut slot = 0;
		let mut kept = 0;
		for k in 0..self.dirty.len() {
			let i = self.dirty[k];
			let v = self.pending[i as usize];
			if block.write_param(slot, i, v) {
				slot += 1;
				self.pending[i as usize] = f32::NAN;
			} else {
				self.dirty[kept] = i;
				kept += 1;
			}
		}
		self.dirty.truncate(kept);
		block.set_param_count(slot);
		block.set_request(cmd, flags, 0);
		true
	}

	/// Queues a parameter change again (unless a newer one is queued).
	fn requeue(&mut self, index: u32, value: f32) {
		let i = index as usize;
		if i < self.pending.len()
			&& self.pending[i].is_nan()
			&& self.dirty.len() < self.dirty.capacity()
		{
			self.dirty.push(index);
			self.pending[i] = value;
		}
	}

	/// Keeps the note-offs of events that never reached the plugin, so no
	/// note hangs after a dropped chunk.
	fn hold_note_offs(&mut self, from: usize) {
		for k in from..self.staging.events.len() {
			let e = self.staging.events[k];
			if matches!(e, MidiEvent::NoteOn { .. }) {
				continue;
			}
			if self.held.len() == self.held.capacity() {
				self.held.clear();
				self.held.push(MidiEvent::AllNotesOff { offset: 0 });
				return;
			}
			self.held.push(shm::with_offset(e, 0));
		}
	}

	/// Appends one `process` call's input to the staging area.
	fn stage(&mut self, left: &[f32], right: &[f32], events: &[MidiEvent], t: &Transport) {
		let keyed = self.sidechain.take_active();
		let n = left.len().min(right.len());
		let base = self.staging.frames;
		let st = &mut self.staging;
		if base == 0 && !self.held.is_empty() {
			for e in self.held.drain(..) {
				if st.events.len() < st.events.capacity() {
					st.events.push(e);
				}
			}
		}
		let mut kept = 0;
		for k in 0..self.dirty.len() {
			let i = self.dirty[k];
			if st.params.len() < st.params.capacity() {
				st.params.push((i, self.pending[i as usize]));
				self.pending[i as usize] = f32::NAN;
			} else {
				self.dirty[kept] = i;
				kept += 1;
			}
		}
		self.dirty.truncate(kept);

		let piece = self.conn.as_ref().map_or(n, |c| c.layout.capacity).max(1);
		let (kl, kr) = self.sidechain.buffers();
		let mut start = 0;
		let mut rest = events;
		while start < n && st.blocks.len() < st.blocks.capacity() {
			let frames = piece.min(n - start);
			let end = start + frames;
			// Events are sorted; the last piece takes any late offsets too.
			let split = if end < n {
				rest.partition_point(|e| (e.offset() as usize) < end)
			} else {
				rest.len()
			};
			let (piece_events, r) = rest.split_at(split);
			rest = r;
			for &e in piece_events {
				if st.events.len() < st.events.capacity() {
					st.events
						.push(shm::with_offset(e, e.offset() + base as u32));
				}
			}
			let at = base + start;
			st.left[at..at + frames].copy_from_slice(&left[start..end]);
			st.right[at..at + frames].copy_from_slice(&right[start..end]);
			if keyed {
				let range = start.min(kl.len())..end.min(kl.len());
				let len = range.len();
				st.key_left[at..at + len].copy_from_slice(&kl[range.clone()]);
				st.key_right[at..at + len].copy_from_slice(&kr[range]);
				st.key_left[at + len..at + frames].fill(0.0);
				st.key_right[at + len..at + frames].fill(0.0);
			}
			let mut transport = *t;
			if start > 0 {
				transport.beat_position +=
					start as f64 / f64::from(self.sample_rate.max(1.0)) * t.bpm / 60.0;
			}
			st.blocks.push(shm::SubBlock {
				frames: frames as u32,
				transport,
				keyed,
				events_end: st.events.len() as u32,
				params_end: st.params.len() as u32,
			});
			start = end;
		}
		st.frames += start;
		// A block too large for the sub-block table: the rest stays as is
		// (dry), and the frame count still advances so the FIFO stays aligned.
		if start < n {
			let gap = n - start;
			let at = base + start;
			st.left[at..at + gap].copy_from_slice(&left[start..n]);
			st.right[at..at + gap].copy_from_slice(&right[start..n]);
			st.frames += gap;
		}
	}

	/// Sends sub-blocks `b0..b1` (at most one shared buffer) to the child.
	fn send_group(&mut self, b0: usize, b1: usize) -> Sent {
		if let Err(why) = self.ready() {
			self.not_ready(why);
			return Sent::Undelivered;
		}
		let flags = self.request_flags();
		let Some(conn) = self.conn.as_ref() else {
			return Sent::Undelivered;
		};
		let block = conn.block();
		let st = &self.staging;
		let (ev0, p0) = st.starts(b0);
		let frame0 = st.frame_of(b0);
		let mut frames = 0;
		let mut keyed = false;
		for (i, sub) in st.blocks[b0..b1].iter().enumerate() {
			block.write_sub_block(
				i,
				&shm::SubBlock {
					events_end: sub.events_end - ev0 as u32,
					params_end: sub.params_end - p0 as u32,
					..*sub
				},
			);
			frames += sub.frames as usize;
			keyed |= sub.keyed;
		}
		let (ev1, p1) = st.starts(b1);
		block.write_events_shifted(&st.events[ev0..ev1], frame0 as u32);
		for (slot, &(i, v)) in st.params[p0..p1].iter().enumerate() {
			block.write_param(slot, i, v);
		}
		block.set_param_count(p1 - p0);
		let range = frame0..frame0 + frames;
		block.write_audio_at(CH_LEFT, 0, &st.left[range.clone()]);
		block.write_audio_at(CH_RIGHT, 0, &st.right[range.clone()]);
		if keyed {
			block.write_audio_at(CH_SIDECHAIN, 0, &st.key_left[range.clone()]);
			block.write_audio_at(CH_SIDECHAIN + 1, 0, &st.key_right[range]);
		}
		block.set_request(CMD_PROCESS, flags, b1 - b0);
		self.send()
	}

	/// Sends the staged chunk and queues its output (or its bypass) in the
	/// FIFO. Realtime-safe.
	fn flush(&mut self) {
		let total = self.staging.frames;
		if total == 0 {
			return;
		}
		let capacity = self.capacity();
		let blocks = self.staging.blocks.len();
		// Sub-blocks that reached the child, frames whose output came back.
		let (mut delivered, mut done) = (0, 0);
		let mut b0 = 0;
		while !self.crashed && b0 < blocks {
			let mut b1 = b0;
			let mut frames = 0;
			while b1 < blocks
				&& (b1 == b0 || frames + self.staging.blocks[b1].frames as usize <= capacity)
			{
				frames += self.staging.blocks[b1].frames as usize;
				b1 += 1;
			}
			match self.send_group(b0, b1) {
				Sent::OnTime => {
					delivered = b1;
					let Some(conn) = self.conn.as_ref() else {
						break;
					};
					let block = conn.block();
					self.fifo.push(frames, |c, offset, dst| {
						let ch = if c < 2 { c } else { CH_AUX + c - 2 };
						block.read_audio_at(ch, offset, dst);
					});
					done += frames;
					b0 = b1;
				}
				Sent::Late => {
					delivered = b1;
					break;
				}
				Sent::Undelivered => break,
			}
		}
		if done < total {
			if !self.crashed {
				self.status
					.dropped_frames
					.fetch_add((total - done) as u64, Ordering::Relaxed);
			}
			self.push_bypass(done, total - done);
			let (ev, p) = self.staging.starts(delivered);
			for k in p..self.staging.params.len() {
				let (i, v) = self.staging.params[k];
				self.requeue(i, v);
			}
			self.hold_note_offs(ev);
		}
		self.staging.clear();
	}

	/// Queues staged frames `from..from + n` as bypassed output: silence for
	/// instruments and extra outputs, the dry input for effects.
	fn push_bypass(&mut self, from: usize, n: usize) {
		let effect = self.desc.role == PluginRole::Effect;
		let (left, right) = (&self.staging.left, &self.staging.right);
		self.fifo.push(n, |c, offset, dst| {
			let at = from + offset;
			match c {
				0 if effect => dst.copy_from_slice(&left[at..at + dst.len()]),
				1 if effect => dst.copy_from_slice(&right[at..at + dst.len()]),
				_ => dst.fill(0.0),
			}
		});
	}

	/// Turns whatever is staged into bypassed output (keeping the FIFO in
	/// step) and requeues its parameter changes. For non-realtime calls that
	/// break the stream anyway.
	fn abandon_staging(&mut self) {
		let total = self.staging.frames;
		if total > 0 {
			self.push_bypass(0, total);
		}
		for k in 0..self.staging.params.len() {
			let (i, v) = self.staging.params[k];
			self.requeue(i, v);
		}
		self.hold_note_offs(0);
		self.staging.clear();
	}

	/// Waits (off the audio thread) for a late request to finish, so the
	/// shared block and the child's processor are free again.
	fn settle(&mut self) -> bool {
		let deadline = Instant::now() + self.options.control_timeout;
		loop {
			match self.ready() {
				Ok(()) => return true,
				Err(NotReady::Busy) if Instant::now() < deadline => {
					if let (Some(conn), Some(seq)) = (self.conn.as_ref(), self.outstanding) {
						let ack = conn.block().word(W_ACK_SEQ);
						let seen = ack.load(Ordering::Acquire);
						if seen != seq {
							sync::wait(ack, seen, Duration::from_millis(10));
						}
					}
				}
				Err(NotReady::Busy) => {
					let stalled = self.stall_since.map_or(Duration::ZERO, |s| s.elapsed());
					self.fail(Wait::TimedOut, stalled);
					return false;
				}
				Err(why) => {
					self.not_ready(why);
					return false;
				}
			}
		}
	}

	/// Sends a non-realtime request; `None` if the instance is (or just
	/// became) crashed.
	fn control(&mut self, req: &Request) -> Option<Reply> {
		if self.crashed || self.conn.is_none() || !self.settle() {
			return None;
		}
		self.abandon_staging();
		if (!self.dirty.is_empty() || self.reset_pending) && self.write_params_request(CMD_PARAMS) {
			match self.exchange(self.options.control_timeout) {
				(Wait::Done, ..) => {}
				(how, ..) => {
					let timeout = self.options.control_timeout;
					self.fail(how, timeout);
					return None;
				}
			}
		}
		let timeout = self.options.control_timeout;
		match self.link.request(req, timeout) {
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
		self.control(&Request::Prepare {
			sample_rate,
			max_block,
		});
		self.sample_rate = sample_rate;
		self.max_block = max_block.max(1);
		self.allocate_buffers();
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
		let total = left.len().min(right.len());
		self.last_frames = 0;
		// Frames past `max_block` (a caller bug) are bypassed in place.
		let n = total.min(self.max_block);
		if n < total {
			self.sidechain.clear();
			if self.desc.role == PluginRole::Instrument {
				left[n..total].fill(0.0);
				right[n..total].fill(0.0);
			}
		}
		if n == 0 {
			self.sidechain.clear();
			return;
		}
		self.stage(&left[..n], &right[..n], events, transport);
		if self.staging.frames + self.max_block > self.staging.chunk {
			self.flush();
		}
		let aux = &mut self.aux;
		self.fifo.pop(n, |c, offset, src| {
			let dst = match c {
				0 => &mut left[offset..offset + src.len()],
				1 => &mut right[offset..offset + src.len()],
				_ => match aux.get_mut(c - 2) {
					Some(buf) => &mut buf[offset..offset + src.len()],
					None => return,
				},
			};
			dst.copy_from_slice(src);
		});
		self.last_frames = n;
	}

	fn has_sidechain(&self) -> bool {
		self.desc.sidechain_channels > 0
	}

	fn set_sidechain(&mut self, left: &[f32], right: &[f32]) {
		self.sidechain.set(left, right);
	}

	fn aux_output_count(&self) -> usize {
		self.aux.len() / 2
	}

	fn aux_output(&self, index: usize) -> Option<(&[f32], &[f32])> {
		let n = self.last_frames;
		let l = self.aux.get(2 * index)?;
		let r = self.aux.get(2 * index + 1)?;
		Some((&l[..n], &r[..n]))
	}

	fn presets(&mut self) -> Vec<cadenza_dsp::PresetInfo> {
		self.control(&Request::ListPresets)
			.map(|reply| {
				reply
					.presets
					.into_iter()
					.map(|p| cadenza_dsp::PresetInfo {
						key: p.key,
						name: p.name,
						category: p.category,
					})
					.collect()
			})
			.unwrap_or_default()
	}

	fn load_preset(&mut self, key: &str) -> bool {
		let accepted = self
			.control(&Request::LoadPreset {
				key: key.to_string(),
			})
			.is_some_and(|r| r.accepted);
		if accepted {
			// Keep the restart state in sync with what the plugin now holds.
			self.save_state();
		}
		accepted
	}

	fn idle(&mut self) {
		// Only while an editor is open: that is when the plugin has parameter
		// changes to flush on its own.
		if self.crashed || !self.link.editor_open.load(Ordering::Acquire) {
			return;
		}
		if let Err(why) = self.ready() {
			self.not_ready(why);
			return;
		}
		if self.write_params_request(CMD_IDLE) {
			self.send();
		}
	}

	fn reset(&mut self) {
		// Deferred to the next request (the child resets before anything
		// else in it), so a reset never waits for the child.
		for k in 0..self.staging.params.len() {
			let (i, v) = self.staging.params[k];
			self.requeue(i, v);
		}
		self.hold_note_offs(0);
		self.staging.clear();
		self.fifo.reset(self.batch_latency as usize);
		self.sidechain.clear();
		self.reset_pending = true;
	}

	fn latency(&self) -> u32 {
		self.latency + self.batch_latency
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
		self.link.editor_open.store(false, Ordering::Release);
		self.link.shutdown();
		if let Some(conn) = self.conn.take() {
			reap_later(conn.child);
		}
	}
}
