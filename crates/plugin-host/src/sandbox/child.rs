//! The sandbox child: hosts one plugin in-process (with the regular
//! [`crate::instantiate_with_mode`] path) and serves the host through shared
//! memory (audio) and the stdin/stdout control channel.
//!
//! Threads: the process main thread runs the control loop; a dedicated
//! `cadenza-sandbox-audio` thread waits on the request futex and processes
//! blocks. The host never overlaps a control request with an audio request
//! (both need `&mut` on its side), so the mutex around the processor is never
//! contended. Losing the host (stdin EOF, or `PR_SET_PDEATHSIG` on Linux)
//! ends the process immediately.

use std::ffi::OsString;
use std::io::Write;
use std::process::ExitCode;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use base64::Engine as _;
use cadenza_dsp::{MidiEvent, Processor};

use super::protocol::{Reply, Request, read_frame, write_frame};
use super::shm::{Block, CMD_PROCESS, CMD_RESET, Layout, Region, W_ACK_SEQ, W_REQ_SEQ};
use super::sync;
use crate::ProcessMode;

struct Args {
	shm: String,
	parent: u32,
	core_dumps: bool,
}

fn parse_args(mut args: impl Iterator<Item = OsString>) -> Option<Args> {
	let (mut shm, mut parent, mut core_dumps) = (None, None, true);
	while let Some(a) = args.next() {
		match a.to_str()? {
			"--shm" => shm = args.next()?.into_string().ok(),
			"--parent" => parent = args.next()?.to_str()?.parse().ok(),
			"--no-core-dumps" => core_dumps = false,
			_ => return None,
		}
	}
	Some(Args {
		shm: shm?,
		parent: parent?,
		core_dumps,
	})
}

fn exit_now(code: i32) -> ! {
	// Skip destructors and atexit handlers: unloading plugins is a common
	// source of crashes and hangs, and the host only needs the process gone.
	// SAFETY: `_exit` never returns and is always safe to call.
	unsafe { libc::_exit(code) }
}

#[cfg(unix)]
fn harden(args: &Args) -> Result<std::fs::File, String> {
	use std::os::fd::FromRawFd;

	#[cfg(target_os = "linux")]
	// SAFETY: plain prctl/getppid calls.
	unsafe {
		// Dies with the host thread that spawned it (the host spawns from its
		// long-lived plugin main thread, so in practice: with the host).
		libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
		if !args.core_dumps {
			libc::prctl(libc::PR_SET_DUMPABLE, 0);
		}
	}
	// SAFETY: getppid has no preconditions.
	if unsafe { libc::getppid() } as u32 != args.parent {
		return Err("the host exited before the sandbox started".into());
	}
	if !args.core_dumps {
		let zero = libc::rlimit {
			rlim_cur: 0,
			rlim_max: 0,
		};
		// SAFETY: valid rlimit struct.
		unsafe { libc::setrlimit(libc::RLIMIT_CORE, &zero) };
	}
	// Keep the control channel private: plugins print to stdout freely.
	// SAFETY: duplicating and replacing standard descriptors we own.
	unsafe {
		let proto = libc::fcntl(1, libc::F_DUPFD_CLOEXEC, 3);
		if proto < 0 || libc::dup2(2, 1) < 0 {
			return Err("cannot set up the control channel".into());
		}
		Ok(std::fs::File::from_raw_fd(proto))
	}
}

/// Entry point of the `cadenza-plugin-sandbox` executable.
#[cfg(unix)]
pub fn run(args: impl Iterator<Item = OsString>) -> ExitCode {
	let Some(args) = parse_args(args) else {
		eprintln!(
			"usage: cadenza-plugin-sandbox --shm <name> --parent <pid> [--no-core-dumps]\n\
			 (started by cadenza-plugin-host; not meant to be run by hand)"
		);
		return ExitCode::from(2);
	};
	// A panic anywhere is a plugin-side crash: report it as one (SIGABRT)
	// instead of leaving a half-dead process the host has to time out.
	std::panic::set_hook(Box::new(|info| {
		eprintln!("cadenza-plugin-sandbox: {info}");
		std::process::abort();
	}));
	let mut out = match harden(&args) {
		Ok(f) => f,
		Err(e) => {
			eprintln!("cadenza-plugin-sandbox: {e}");
			return ExitCode::from(1);
		}
	};
	let region = match Region::open(&args.shm) {
		Ok(r) => Arc::new(r),
		Err(e) => {
			eprintln!("cadenza-plugin-sandbox: {e}");
			return ExitCode::from(1);
		}
	};
	let layout = match Layout::from_header(region.words()) {
		Ok(l) => l,
		Err(e) => {
			eprintln!("cadenza-plugin-sandbox: {e}");
			return ExitCode::from(1);
		}
	};

	let mut stdin = std::io::stdin().lock();
	let processor = match read_frame::<Request>(&mut stdin) {
		Ok(Request::Init {
			descriptor,
			sample_rate,
			max_block,
			offline,
			state,
		}) => {
			let mode = if offline {
				ProcessMode::Offline
			} else {
				ProcessMode::Realtime
			};
			let state = match state
				.as_deref()
				.map(|s| crate::decode_state(&descriptor.id, s))
			{
				Some(Err(e)) => Err(e),
				Some(Ok(bytes)) => Ok(Some(bytes)),
				None => Ok(None),
			};
			match state.and_then(|state| {
				crate::instantiate_with_mode(
					&descriptor,
					sample_rate,
					max_block,
					state.as_deref(),
					mode,
				)
			}) {
				Ok(p) => p,
				Err(e) => {
					let _ = write_frame(
						&mut out,
						&Reply {
							state_rejected: matches!(e, crate::PluginError::State { .. }),
							error: Some(e.to_string()),
							..Reply::default()
						},
					);
					exit_now(1);
				}
			}
		}
		Ok(_) | Err(_) => exit_now(1),
	};
	let processor = Arc::new(Mutex::new(processor));
	{
		let (processor, region) = (processor.clone(), region.clone());
		let spawned = std::thread::Builder::new()
			.name("cadenza-sandbox-audio".into())
			.spawn(move || audio_loop(&region, layout, &processor));
		if spawned.is_err() {
			exit_now(1);
		}
	}
	let ready = reply_ok(&mut lock(&processor));
	if write_frame(&mut out, &ready).is_err() {
		exit_now(1);
	}

	loop {
		let request = match read_frame::<Request>(&mut stdin) {
			Ok(r) => r,
			// EOF: the host dropped us or died.
			Err(_) => exit_now(0),
		};
		let reply = handle(&processor, request);
		if write_frame(&mut out, &reply).is_err() {
			exit_now(0);
		}
	}
}

#[cfg(not(unix))]
pub fn run(_args: impl Iterator<Item = OsString>) -> ExitCode {
	eprintln!("cadenza-plugin-sandbox: sandboxed hosting is not implemented on this OS yet");
	ExitCode::from(2)
}

fn lock(p: &Mutex<Box<dyn Processor>>) -> MutexGuard<'_, Box<dyn Processor>> {
	p.lock().unwrap_or_else(|e| e.into_inner())
}

fn reply_ok(p: &mut MutexGuard<'_, Box<dyn Processor>>) -> Reply {
	Reply {
		latency: p.latency(),
		tail_seconds: p.tail_seconds(),
		..Reply::default()
	}
}

fn handle(processor: &Mutex<Box<dyn Processor>>, request: Request) -> Reply {
	let mut p = lock(processor);
	match request {
		Request::Init { .. } => Reply {
			error: Some("already initialised".into()),
			..reply_ok(&mut p)
		},
		Request::Prepare {
			sample_rate,
			max_block,
		} => {
			p.prepare(sample_rate, max_block);
			reply_ok(&mut p)
		}
		Request::SaveState => {
			let state = p.save_state().map(|s| crate::encode_state(&s));
			Reply {
				state,
				..reply_ok(&mut p)
			}
		}
		Request::LoadState { data } => {
			let accepted = base64::engine::general_purpose::STANDARD
				.decode(data)
				.is_ok_and(|bytes| p.load_state(&bytes));
			Reply {
				accepted,
				..reply_ok(&mut p)
			}
		}
		Request::Shutdown => {
			let _ = std::io::stderr().flush();
			exit_now(0)
		}
	}
}

fn audio_loop(region: &Region, layout: Layout, processor: &Mutex<Box<dyn Processor>>) {
	let Some(block) = Block::new(region.words(), layout) else {
		exit_now(1);
	};
	let mut left = vec![0.0f32; layout.capacity];
	let mut right = vec![0.0f32; layout.capacity];
	let mut events: Vec<MidiEvent> = Vec::with_capacity(layout.event_capacity);
	let req = block.word(W_REQ_SEQ);
	let ack = block.word(W_ACK_SEQ);
	// Start from the last acknowledged request (not the current request
	// number, which the host may already have bumped).
	let mut last = ack.load(Ordering::Acquire);
	loop {
		if !sync::spin_until_changed(req, last, 2_000) {
			sync::wait(req, last, Duration::from_secs(1));
			continue;
		}
		let seq = req.load(Ordering::Acquire);
		last = seq;
		{
			let mut p = lock(processor);
			block.for_each_param(|i, v| p.set_param(i, v));
			match block.cmd() {
				CMD_PROCESS => {
					let n = block.frames();
					block.read_events(&mut events);
					block.read_audio(false, &mut left[..n]);
					block.read_audio(true, &mut right[..n]);
					let transport = block.transport();
					p.process(&mut left[..n], &mut right[..n], &events, &transport);
					block.write_audio(false, &left[..n]);
					block.write_audio(true, &right[..n]);
				}
				CMD_RESET => p.reset(),
				_ => {}
			}
		}
		ack.store(seq, Ordering::Release);
		sync::wake(ack);
	}
}
