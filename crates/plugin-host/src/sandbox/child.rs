//! The sandbox child: hosts one plugin in-process (with the regular
//! [`crate::instantiate_with_mode`] path) and serves the host through shared
//! memory (audio) and the stdin/stdout control channel.
//!
//! Threads: the process main thread runs the control loop; a dedicated
//! `cadenza-sandbox-audio` thread waits on the request futex and processes
//! blocks; the plugin's own main thread (`cadenza-plugin-main`, as in any
//! host process) owns the instance and runs its editor and event loop. The
//! host never overlaps a processor control request with an audio request
//! (both need `&mut` on its side, and it waits for a late audio request to
//! finish before any control request), so the mutex around the processor is
//! never contended; editor requests may arrive while audio runs and therefore
//! never touch that mutex (they go through the instance's editor handle:
//! window work, state reads and timers run on the plugin main thread, edits
//! reach the host through a wait-free ring, and parameter flushes happen on
//! the audio thread in `process`/`idle`). The audio thread is promoted to
//! realtime scheduling before it serves anything (see `rt.rs`). Losing the
//! host (stdin EOF, or `PR_SET_PDEATHSIG` on Linux) ends the process
//! immediately.

use std::ffi::OsString;
use std::io::Write;
use std::process::ExitCode;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use base64::Engine as _;
use cadenza_dsp::{MidiEvent, Processor};

use super::protocol::WirePreset;
use super::protocol::{Envelope, Reply, Request, read_frame, write_frame};
use super::shm::{
	Block, CH_AUX, CH_LEFT, CH_RIGHT, CH_SIDECHAIN, CMD_IDLE, CMD_PROCESS, FLAG_RESET, Layout,
	Region, W_ACK_SEQ, W_REQ_SEQ, with_offset,
};
use super::{rt, sync};
use crate::{PluginEditor, ProcessMode};

struct Args {
	shm: String,
	parent: u32,
	core_dumps: bool,
	/// `SCHED_FIFO` priority for the audio thread in realtime mode (0: none).
	rt_priority: u32,
}

fn parse_args(mut args: impl Iterator<Item = OsString>) -> Option<Args> {
	let (mut shm, mut parent, mut core_dumps, mut rt_priority) = (None, None, true, 0);
	while let Some(a) = args.next() {
		match a.to_str()? {
			"--shm" => shm = args.next()?.into_string().ok(),
			"--parent" => parent = args.next()?.to_str()?.parse().ok(),
			"--no-core-dumps" => core_dumps = false,
			"--rt-priority" => rt_priority = args.next()?.to_str()?.parse().ok()?,
			_ => return None,
		}
	}
	Some(Args {
		shm: shm?,
		parent: parent?,
		core_dumps,
		rt_priority,
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
			"usage: cadenza-plugin-sandbox --shm <name> --parent <pid> [--no-core-dumps] \
			 [--rt-priority <1-99>]\n\
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
	let (init_id, init) = match read_frame::<Envelope<Request>>(&mut stdin) {
		Ok(e) => (e.id, e.body),
		Err(_) => exit_now(1),
	};
	let reply =
		|out: &mut std::fs::File, id: u64, body: &Reply| write_frame(out, &Envelope { id, body });
	let realtime;
	let (processor, editor) = match init {
		Request::Init {
			descriptor,
			sample_rate,
			max_block,
			offline,
			state,
		} => {
			realtime = !offline;
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
				crate::instantiate_with_editor(
					&descriptor,
					sample_rate,
					max_block,
					state.as_deref(),
					mode,
				)
			}) {
				Ok(p) => p,
				Err(e) => {
					let _ = reply(
						&mut out,
						init_id,
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
		_ => exit_now(1),
	};
	let processor = Arc::new(Mutex::new(processor));
	let (promoted_tx, promoted) = std::sync::mpsc::channel();
	{
		let (processor, region) = (processor.clone(), region.clone());
		let priority = if realtime { args.rt_priority } else { 0 };
		let spawned = std::thread::Builder::new()
			.name("cadenza-sandbox-audio".into())
			.spawn(move || {
				// Before the first request: promotion may take a few ms.
				let _ = promoted_tx.send(rt::promote_current_thread(priority));
				audio_loop(&region, layout, &processor)
			});
		if spawned.is_err() {
			exit_now(1);
		}
	}
	let scheduling = promoted
		.recv_timeout(Duration::from_secs(5))
		.map_or_else(|_| "unknown".to_string(), |s| s.to_string());
	if std::env::var_os("CADENZA_PLUGIN_LOG").is_some() {
		eprintln!("cadenza-plugin-sandbox: audio thread: {scheduling}");
	}
	let ready = Reply {
		scheduling: Some(scheduling),
		..reply_ok(&mut lock(&processor))
	};
	if reply(&mut out, init_id, &ready).is_err() {
		exit_now(1);
	}

	loop {
		let request = match read_frame::<Envelope<Request>>(&mut stdin) {
			Ok(r) => r,
			// EOF: the host dropped us or died.
			Err(_) => exit_now(0),
		};
		let body = match request.body {
			Request::EditorOpen { .. }
			| Request::EditorClose
			| Request::EditorPoll
			| Request::EditorState => handle_editor(&editor, request.body),
			other => handle(&processor, other),
		};
		if reply(&mut out, request.id, &body).is_err() {
			exit_now(0);
		}
	}
}

/// Editor requests: served through the instance's editor handle on the
/// plugin main thread, without the processor (busy on the audio thread).
fn handle_editor(editor: &PluginEditor, request: Request) -> Reply {
	let error = |e: crate::PluginError| Reply {
		error: Some(e.to_string()),
		..Reply::default()
	};
	match request {
		Request::EditorOpen { title } => match editor.open(&title) {
			Ok(()) => Reply::default(),
			Err(e) => error(e),
		},
		Request::EditorClose => match editor.close() {
			Ok(()) => Reply::default(),
			Err(e) => error(e),
		},
		Request::EditorPoll => Reply {
			events: editor.poll(),
			..Reply::default()
		},
		Request::EditorState => match editor.save_state() {
			Ok(state) => Reply {
				state: state.map(|s| crate::encode_state(&s)),
				..Reply::default()
			},
			Err(e) => error(e),
		},
		_ => Reply {
			error: Some("not an editor request".into()),
			..Reply::default()
		},
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
		Request::ListPresets => {
			let presets = p
				.presets()
				.into_iter()
				.map(|p| WirePreset {
					key: p.key,
					name: p.name,
					category: p.category,
				})
				.collect();
			Reply {
				presets,
				..reply_ok(&mut p)
			}
		}
		Request::LoadPreset { key } => {
			let accepted = p.load_preset(&key);
			Reply {
				accepted,
				..reply_ok(&mut p)
			}
		}
		Request::Shutdown => {
			let _ = std::io::stderr().flush();
			exit_now(0)
		}
		Request::EditorOpen { .. }
		| Request::EditorClose
		| Request::EditorPoll
		| Request::EditorState => Reply {
			error: Some("editor requests are not processor requests".into()),
			..reply_ok(&mut p)
		},
	}
}

/// Serves audio requests: one `Processor::process` call per sub-block, with
/// that sub-block's parameter changes applied first and its events shifted to
/// its own start. Allocation-free after setup.
fn audio_loop(region: &Region, layout: Layout, processor: &Mutex<Box<dyn Processor>>) {
	let Some(block) = Block::new(region.words(), layout) else {
		exit_now(1);
	};
	let buffer = || {
		let mut b = vec![0.0f32; layout.capacity];
		rt::prefault(&mut b);
		b
	};
	let (mut left, mut right) = (buffer(), buffer());
	let (mut key_left, mut key_right) = (buffer(), buffer());
	let zeros = buffer();
	let mut events: Vec<MidiEvent> = Vec::with_capacity(layout.event_capacity);
	let mut sub_events: Vec<MidiEvent> = Vec::with_capacity(layout.event_capacity);
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
			if block.flags() & FLAG_RESET != 0 {
				p.reset();
			}
			let mut param = 0;
			match block.cmd() {
				CMD_PROCESS => {
					block.read_events(&mut events);
					let mut event = 0;
					let mut start = 0;
					for b in 0..block.blocks() {
						let sub = block.sub_block(b);
						let n = (sub.frames as usize).min(layout.capacity - start);
						let params_end = sub.params_end as usize;
						block.for_each_param(param, params_end, |i, v| p.set_param(i, v));
						param = param.max(params_end.min(block.param_count()));
						let events_end = (sub.events_end as usize).clamp(event, events.len());
						sub_events.clear();
						sub_events.extend(
							events[event..events_end]
								.iter()
								.map(|&e| with_offset(e, e.offset().saturating_sub(start as u32))),
						);
						event = events_end;
						if n == 0 {
							continue;
						}
						block.read_audio_at(CH_LEFT, start, &mut left[..n]);
						block.read_audio_at(CH_RIGHT, start, &mut right[..n]);
						if sub.keyed {
							block.read_audio_at(CH_SIDECHAIN, start, &mut key_left[..n]);
							block.read_audio_at(CH_SIDECHAIN + 1, start, &mut key_right[..n]);
							p.set_sidechain(&key_left[..n], &key_right[..n]);
						}
						p.process(&mut left[..n], &mut right[..n], &sub_events, &sub.transport);
						block.write_audio_at(CH_LEFT, start, &left[..n]);
						block.write_audio_at(CH_RIGHT, start, &right[..n]);
						for k in 0..layout.aux_outputs {
							let (l, r) = p.aux_output(k).unwrap_or((&zeros[..n], &zeros[..n]));
							let (l, r) = (&l[..l.len().min(n)], &r[..r.len().min(n)]);
							let (cl, cr) = (CH_AUX + 2 * k, CH_AUX + 2 * k + 1);
							block.write_audio_at(cl, start, l);
							block.write_audio_at(cr, start, r);
							block.write_audio_at(cl, start + l.len(), &zeros[l.len()..n]);
							block.write_audio_at(cr, start + r.len(), &zeros[r.len()..n]);
						}
						start += n;
					}
					block.for_each_param(param, usize::MAX, |i, v| p.set_param(i, v));
				}
				CMD_IDLE => {
					block.for_each_param(0, usize::MAX, |i, v| p.set_param(i, v));
					p.idle();
				}
				_ => block.for_each_param(0, usize::MAX, |i, v| p.set_param(i, v)),
			}
		}
		ack.store(seq, Ordering::Release);
		sync::wake(ack);
	}
}
