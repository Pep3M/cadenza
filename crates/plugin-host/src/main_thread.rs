//! The single plugin "main thread" shared by every hosted format.
//!
//! Plugin APIs (CLAP `[main-thread]` functions, VST3 component/controller
//! calls) must run on one stable thread for the life of an instance. A lazily
//! spawned `cadenza-plugin-main` thread owns every non-`Send` plugin object and
//! runs jobs submitted with [`run_on_main`] / [`post_to_main`]. Between jobs it
//! wakes up periodically to service plugin callbacks.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::OnceLock;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::time::Duration;

use crate::clap::ClapState;
use crate::error::PluginError;
use crate::vst3::Vst3State;

/// Everything owned by the plugin main thread.
#[derive(Default)]
pub(crate) struct MainState {
	pub clap: ClapState,
	pub vst3: Vst3State,
}

type Job = Box<dyn FnOnce(&mut MainState) + Send>;

/// How often the main thread wakes up to serve plugin callbacks.
const CALLBACK_POLL: Duration = Duration::from_millis(15);

fn main_sender() -> &'static Sender<Job> {
	static SENDER: OnceLock<Sender<Job>> = OnceLock::new();
	SENDER.get_or_init(|| {
		let (tx, rx) = mpsc::channel::<Job>();
		std::thread::Builder::new()
			.name("cadenza-plugin-main".into())
			.spawn(move || {
				let mut state = MainState::default();
				loop {
					match rx.recv_timeout(CALLBACK_POLL) {
						Ok(job) => job(&mut state),
						Err(RecvTimeoutError::Timeout) => {}
						Err(RecvTimeoutError::Disconnected) => break,
					}
					let _ = catch_unwind(AssertUnwindSafe(|| state.clap.service_callbacks()));
				}
			})
			.expect("spawn plugin main thread");
		tx
	})
}

/// Runs `f` on the plugin main thread and waits for its result. Must not be
/// called from the audio thread (it blocks) nor from inside another job.
pub(crate) fn run_on_main<R: Send + 'static>(
	f: impl FnOnce(&mut MainState) -> R + Send + 'static,
) -> Result<R, PluginError> {
	let (tx, rx) = mpsc::sync_channel(1);
	main_sender()
		.send(Box::new(move |state| {
			let result = catch_unwind(AssertUnwindSafe(|| f(state)));
			let _ = tx.send(result);
		}))
		.map_err(|_| PluginError::HostThread("the plugin main thread has stopped".into()))?;
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
	let _ = main_sender().send(Box::new(move |state| {
		let _ = catch_unwind(AssertUnwindSafe(|| f(state)));
	}));
}
