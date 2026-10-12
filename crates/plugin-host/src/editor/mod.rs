//! Plugin editor windows (CLAP `gui`, VST3 `IPlugView`) and the parameter
//! edits they make.
//!
//! # Where editors live
//!
//! A GUI must be created in the process (and on the thread) that owns the
//! plugin instance: the shared `cadenza-plugin-main` thread, in this process
//! for in-process instances and in the sandbox child for sandboxed ones. The
//! main thread loop (see `main_thread.rs`) polls the X11 connection, the file
//! descriptors plugins register (CLAP posix-fd-support, VST3 `IRunLoop` event
//! handlers) and runs their timers (CLAP timer-support, VST3 `IRunLoop`
//! timers) between jobs, so editors stay responsive while audio runs on its
//! own thread.
//!
//! Windows: CLAP plugins that can open a floating window of their own get
//! one; otherwise (and for every VST3 plugin) the editor is embedded into a
//! top-level X11 window Cadenza creates (`x11.rs`; XWayland on Wayland). Both
//! sides of resizing are handled (plugin requests, user resizes when the
//! plugin allows them) and the close button closes the editor.
//!
//! # Parameter feedback
//!
//! Edits made in a plugin's GUI are reported as [`EditorEvent`]s (gesture
//! begin / value / gesture end, by Cadenza param id and plain value), polled
//! with [`PluginEditor::poll`]:
//! - CLAP plugins emit them as output events of `process` / `params.flush`
//!   on the audio thread; the processor pushes them, allocation-free, into a
//!   wait-free ring buffer read by `poll`.
//! - VST3 plugins call `IComponentHandler::beginEdit/performEdit/endEdit` on
//!   the main thread; the handler forwards the value to the audio processor
//!   (another ring buffer) and queues the event for `poll`.
//! - Changes the plugin does not report as parameters (patch browser, ...)
//!   raise [`EditorEvent::StateChanged`]; [`PluginEditor::save_state`] reads
//!   the live instance's state.
//!
//! Sandboxed instances forward all of this over the sandbox control channel
//! (requests from the host only; the child queues events until polled), so the
//! audio path stays the same shared-memory round trip.

#[cfg(all(unix, not(target_os = "macos")))]
pub(crate) mod x11;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use rtrb::{Consumer, Producer, RingBuffer};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::descriptor::PluginFormat;
use crate::error::PluginError;

/// Something that happened in (or to) a plugin editor since the last poll.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EditorEvent {
	/// The user grabbed a control (knob, slider) of parameter `param`.
	GestureBegin { param: String },
	/// Parameter `param` now has this plain value (inside a gesture, or a
	/// one-off change such as a click or a typed value).
	Value { param: String, value: f64 },
	/// The user released the control of `param`.
	GestureEnd { param: String },
	/// The plugin changed internal state that is not a parameter (e.g. loaded
	/// a patch from its own browser): capture its state to keep it.
	StateChanged,
	/// The editor window was closed by the user (or disappeared with its
	/// plugin process).
	Closed,
}

/// Capacity of the audio-thread → host feedback ring of one instance.
const FEEDBACK_CAPACITY: usize = 1024;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum FeedbackKind {
	Begin,
	Value,
	End,
}

/// A parameter edit as pushed from the audio thread (plain value, descriptor
/// param index).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Feedback {
	pub kind: FeedbackKind,
	pub index: u32,
	pub value: f64,
}

/// Editor state shared between an instance's processor, its main-thread side
/// and its [`PluginEditor`] handles (in the process that owns the instance).
pub(crate) struct EditorShared {
	pub plugin_id: String,
	/// Cadenza param ids in descriptor order.
	param_ids: Vec<String>,
	pub open: AtomicBool,
	/// The user closed the editor; reported once as [`EditorEvent::Closed`].
	pub closed: AtomicBool,
	/// The plugin's state changed outside of parameters.
	pub dirty: AtomicBool,
	/// Events queued on the main thread (VST3 edits).
	queue: Mutex<Vec<EditorEvent>>,
	/// Events pushed by the audio thread (CLAP output events).
	ring: Mutex<Consumer<Feedback>>,
}

impl EditorShared {
	/// The shared state plus the audio-thread producer of its ring.
	pub fn new(plugin_id: &str, param_ids: Vec<String>) -> (Arc<Self>, Producer<Feedback>) {
		let (tx, rx) = RingBuffer::new(FEEDBACK_CAPACITY);
		let shared = Arc::new(Self {
			plugin_id: plugin_id.to_string(),
			param_ids,
			open: AtomicBool::new(false),
			closed: AtomicBool::new(false),
			dirty: AtomicBool::new(false),
			queue: Mutex::new(Vec::new()),
			ring: Mutex::new(rx),
		});
		(shared, tx)
	}

	pub fn param_id(&self, index: u32) -> Option<&str> {
		self.param_ids.get(index as usize).map(String::as_str)
	}

	fn queue(&self) -> MutexGuard<'_, Vec<EditorEvent>> {
		self.queue.lock().unwrap_or_else(|e| e.into_inner())
	}

	/// Queues an event from the main thread.
	pub fn push(&self, event: EditorEvent) {
		self.queue().push(event);
	}

	/// Converts a feedback record into an event (unknown params are dropped).
	pub fn event(&self, f: Feedback) -> Option<EditorEvent> {
		let param = self.param_id(f.index)?.to_string();
		Some(match f.kind {
			FeedbackKind::Begin => EditorEvent::GestureBegin { param },
			FeedbackKind::Value => EditorEvent::Value {
				param,
				value: f.value,
			},
			FeedbackKind::End => EditorEvent::GestureEnd { param },
		})
	}

	/// Everything since the last call, in order per source (the audio thread
	/// ring, then main-thread events, then state/close notifications).
	pub fn drain(&self) -> Vec<EditorEvent> {
		let mut out = Vec::new();
		{
			let mut ring = self.ring.lock().unwrap_or_else(|e| e.into_inner());
			while let Ok(f) = ring.pop() {
				out.extend(self.event(f));
			}
		}
		out.append(&mut self.queue());
		if self.dirty.swap(false, Ordering::AcqRel) {
			out.push(EditorEvent::StateChanged);
		}
		if self.closed.swap(false, Ordering::AcqRel) {
			self.open.store(false, Ordering::Release);
			out.push(EditorEvent::Closed);
		}
		out
	}
}

/// Pushes parameter feedback from the audio thread: wait-free, never
/// allocates; events beyond the ring's capacity are dropped.
pub(crate) struct FeedbackSender {
	ring: Producer<Feedback>,
}

impl FeedbackSender {
	pub fn new(ring: Producer<Feedback>) -> Self {
		Self { ring }
	}

	pub fn send(&mut self, kind: FeedbackKind, index: u32, value: f64) {
		let _ = self.ring.push(Feedback { kind, index, value });
	}
}

/// The in-process side of an editor handle: the instance key on the plugin
/// main thread.
pub(crate) struct LocalEditor {
	pub format: PluginFormat,
	pub key: u64,
	pub shared: Arc<EditorShared>,
}

impl LocalEditor {
	fn error(&self, reason: impl Into<String>) -> PluginError {
		PluginError::Editor {
			id: self.shared.plugin_id.clone(),
			reason: reason.into(),
		}
	}

	pub fn open(&self, title: &str) -> Result<(), PluginError> {
		let (format, key, title) = (self.format, self.key, title.to_string());
		let result = crate::main_thread::run_on_main(move |main| match format {
			PluginFormat::Clap => crate::clap::editor::open(main, key, &title),
			PluginFormat::Vst3 => crate::vst3::editor::open(main, key, &title),
		})?;
		result.map_err(|e| self.error(e))?;
		self.shared.closed.store(false, Ordering::Release);
		self.shared.open.store(true, Ordering::Release);
		Ok(())
	}

	pub fn close(&self) -> Result<(), PluginError> {
		let (format, key) = (self.format, self.key);
		crate::main_thread::run_on_main(move |main| match format {
			PluginFormat::Clap => crate::clap::editor::close(main, key),
			PluginFormat::Vst3 => crate::vst3::editor::close(main, key),
		})?;
		self.shared.open.store(false, Ordering::Release);
		Ok(())
	}

	pub fn save_state(&self) -> Result<Option<Vec<u8>>, PluginError> {
		let (format, key) = (self.format, self.key);
		crate::main_thread::run_on_main(move |main| match format {
			PluginFormat::Clap => crate::clap::editor::save_state(main, key),
			PluginFormat::Vst3 => main
				.vst3
				.instances
				.get(&key)
				.and_then(|p| p.save_state_synced()),
		})
	}
}

enum Link {
	Local(LocalEditor),
	Sandboxed(crate::sandbox::EditorLink),
}

/// A handle to one plugin instance's editor, usable from any thread (not the
/// audio thread: every call may block on the plugin main thread or the
/// sandbox process). Clones share the same editor.
#[derive(Clone)]
pub struct PluginEditor(Arc<Link>);

impl std::fmt::Debug for PluginEditor {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("PluginEditor")
			.field("plugin", &self.plugin_id())
			.field("open", &self.is_open())
			.finish()
	}
}

impl PluginEditor {
	pub(crate) fn local(editor: LocalEditor) -> Self {
		Self(Arc::new(Link::Local(editor)))
	}

	pub(crate) fn sandboxed(link: crate::sandbox::EditorLink) -> Self {
		Self(Arc::new(Link::Sandboxed(link)))
	}

	pub fn plugin_id(&self) -> &str {
		match &*self.0 {
			Link::Local(l) => &l.shared.plugin_id,
			Link::Sandboxed(s) => s.plugin_id(),
		}
	}

	/// Opens (or raises) the editor window titled `title`. Fails with an
	/// actionable message when the plugin has no GUI, no supported windowing
	/// API, or no X server is reachable.
	pub fn open(&self, title: &str) -> Result<(), PluginError> {
		match &*self.0 {
			Link::Local(l) => l.open(title),
			Link::Sandboxed(s) => s.open(title),
		}
	}

	/// Closes the editor (destroys the plugin's view). No-op when closed.
	pub fn close(&self) -> Result<(), PluginError> {
		match &*self.0 {
			Link::Local(l) => l.close(),
			Link::Sandboxed(s) => s.close(),
		}
	}

	/// Whether the editor is open (as far as the last open/close/poll knows).
	pub fn is_open(&self) -> bool {
		match &*self.0 {
			Link::Local(l) => l.shared.open.load(Ordering::Acquire),
			Link::Sandboxed(s) => s.is_open(),
		}
	}

	/// Parameter edits, state changes and closes since the last poll. A lost
	/// sandbox process reports [`EditorEvent::Closed`] once (if an editor was
	/// open).
	pub fn poll(&self) -> Vec<EditorEvent> {
		match &*self.0 {
			Link::Local(l) => l.shared.drain(),
			Link::Sandboxed(s) => s.poll(),
		}
	}

	/// The live instance's state (as `Processor::save_state` would return it),
	/// including changes made in the editor. `Ok(None)`: the plugin has no
	/// state to save.
	pub fn save_state(&self) -> Result<Option<Vec<u8>>, PluginError> {
		match &*self.0 {
			Link::Local(l) => l.save_state(),
			Link::Sandboxed(s) => s.save_state(),
		}
	}
}

// ------------------------------------------------------- main thread loop

use crate::main_thread::{MainState, Owner};

/// One main-thread service round: window events, then every instance's GUI
/// requests, timers and edits.
pub(crate) fn service(main: &mut MainState) {
	#[cfg(all(unix, not(target_os = "macos")))]
	service_windows(main);
	crate::clap::editor::service(main);
	crate::vst3::editor::service(main);
}

#[cfg(all(unix, not(target_os = "macos")))]
fn service_windows(main: &mut MainState) {
	use x11::WindowEvent;
	let Some(display) = main.gui.connected() else {
		return;
	};
	let events = match display.events() {
		Ok(events) => events,
		Err(_) => {
			// The X server went away: every embedded editor is gone.
			let windows: Vec<(u32, Owner)> = main.gui.windows.drain().collect();
			main.gui.forget_display();
			windows
				.into_iter()
				.map(|(w, _)| (w, WindowEvent::Destroyed))
				.collect()
		}
	};
	for (window, event) in events {
		match main.gui.windows.get(&window).copied() {
			Some(Owner::Clap(key)) => crate::clap::editor::window_event(main, key, event),
			Some(Owner::Vst3(key)) => crate::vst3::editor::window_event(main, key, event),
			None => {}
		}
	}
}

/// When the next plugin timer is due.
pub(crate) fn next_deadline(main: &MainState) -> Option<std::time::Instant> {
	[
		crate::clap::editor::next_deadline(&main.clap),
		crate::vst3::editor::next_deadline(&main.vst3),
	]
	.into_iter()
	.flatten()
	.min()
}

/// Descriptors the main thread waits on besides its wake-up pipe: the X11
/// connection and every fd plugins registered.
#[cfg(unix)]
pub(crate) fn collect_fds(
	main: &MainState,
	fds: &mut Vec<libc::pollfd>,
	owners: &mut Vec<FdOwner>,
) {
	#[cfg(all(unix, not(target_os = "macos")))]
	if let Some(d) = main.gui.connected() {
		fds.push(libc::pollfd {
			fd: d.fd(),
			events: libc::POLLIN,
			revents: 0,
		});
		owners.push(FdOwner::Display);
	}
	let mut plugin_owners = Vec::new();
	crate::clap::editor::collect_fds(&main.clap, fds, &mut plugin_owners);
	crate::vst3::editor::collect_fds(&main.vst3, fds, &mut plugin_owners);
	owners.extend(plugin_owners.into_iter().map(FdOwner::Plugin));
}

/// Who asked for an fd to be watched.
#[derive(Debug, Clone, Copy)]
pub(crate) enum FdOwner {
	/// The X11 connection (its events are read by the next service round).
	Display,
	Plugin(Owner),
}

/// Hands ready descriptors to the plugins that registered them.
#[cfg(unix)]
pub(crate) fn dispatch_fds(main: &mut MainState, ready: &[(i32, i16, FdOwner)]) {
	for &(fd, revents, owner) in ready {
		match owner {
			FdOwner::Display => {}
			FdOwner::Plugin(Owner::Clap(key)) => crate::clap::editor::on_fd(main, key, fd, revents),
			FdOwner::Plugin(Owner::Vst3(key)) => crate::vst3::editor::on_fd(main, key, fd),
		}
	}
}

/// Whether plugin editors can be shown on this OS at all.
pub const EDITORS_SUPPORTED: bool = cfg!(all(unix, not(target_os = "macos")));

/// Why editors cannot be shown here (other OSes).
#[cfg(not(all(unix, not(target_os = "macos"))))]
pub(crate) const UNSUPPORTED: &str = "plugin editor windows are only implemented on Linux (X11/XWayland) so far; \
	 use the generic parameter controls instead";

/// The size of the top-level window titled exactly `title` on the X server
/// (`$DISPLAY`), or `None` when there is none. For tests and diagnostics;
/// `Err` without an X server.
pub fn find_window(title: &str) -> Result<Option<(u32, u32)>, String> {
	#[cfg(all(unix, not(target_os = "macos")))]
	{
		x11::find_window_titled(title)
	}
	#[cfg(not(all(unix, not(target_os = "macos"))))]
	{
		let _ = title;
		Err(UNSUPPORTED.into())
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn feedback_becomes_events_in_order() {
		let (shared, tx) = EditorShared::new("p", vec!["10".into(), "20".into()]);
		let mut tx = FeedbackSender::new(tx);
		tx.send(FeedbackKind::Begin, 1, 0.0);
		tx.send(FeedbackKind::Value, 1, 0.25);
		tx.send(FeedbackKind::Value, 7, 1.0);
		tx.send(FeedbackKind::End, 1, 0.0);
		shared.push(EditorEvent::Value {
			param: "10".into(),
			value: 3.0,
		});
		shared.dirty.store(true, Ordering::Release);
		shared.open.store(true, Ordering::Release);
		shared.closed.store(true, Ordering::Release);
		let events = shared.drain();
		assert_eq!(
			events,
			vec![
				EditorEvent::GestureBegin { param: "20".into() },
				EditorEvent::Value {
					param: "20".into(),
					value: 0.25
				},
				EditorEvent::GestureEnd { param: "20".into() },
				EditorEvent::Value {
					param: "10".into(),
					value: 3.0
				},
				EditorEvent::StateChanged,
				EditorEvent::Closed,
			]
		);
		assert!(!shared.open.load(Ordering::Acquire));
		assert!(shared.drain().is_empty());
	}

	#[test]
	fn feedback_ring_drops_instead_of_growing() {
		let (shared, tx) = EditorShared::new("p", vec!["0".into()]);
		let mut tx = FeedbackSender::new(tx);
		for i in 0..FEEDBACK_CAPACITY + 10 {
			tx.send(FeedbackKind::Value, 0, i as f64);
		}
		assert_eq!(shared.drain().len(), FEEDBACK_CAPACITY);
	}

	#[test]
	fn events_serialize_with_a_kind_tag() {
		let json = serde_json::to_string(&EditorEvent::Value {
			param: "3".into(),
			value: 0.5,
		})
		.unwrap();
		assert_eq!(json, r#"{"kind":"value","param":"3","value":0.5}"#);
		let back: EditorEvent = serde_json::from_str(&json).unwrap();
		assert_eq!(
			back,
			EditorEvent::Value {
				param: "3".into(),
				value: 0.5
			}
		);
	}
}
