//! VST3 editors (`IPlugView` embedded with `X11EmbedWindowID`), the plug
//! frame / Linux `IRunLoop` they use, and the controller side of parameter
//! feedback (GUI edits reported, host values mirrored into the controller).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use vst3::Steinberg::Linux::{
	FileDescriptor, IEventHandler, IEventHandlerTrait, IRunLoop, IRunLoopTrait, ITimerHandler,
	ITimerHandlerTrait, TimerInterval,
};
use vst3::Steinberg::Vst::IEditControllerTrait;
use vst3::Steinberg::{
	IPlugFrame, IPlugFrameTrait, IPlugView, IPlugViewTrait, ViewRect, kInvalidArgument,
	kResultFalse, kResultOk, kResultTrue, tresult,
};
use vst3::{Class, ComPtr, ComRef, ComWrapper};

use super::Vst3State;
use super::com::{RawEdit, guard};
use super::plugin::Vst3Plugin;
use crate::editor::{EditorEvent, EditorShared};
use crate::main_thread::MainState;

/// Shortest timer period granted to plugins.
const MIN_TIMER_PERIOD: Duration = Duration::from_millis(5);

// ----------------------------------------------------------------- mirror

/// Normalized values the audio thread sent to the processor on the host's
/// behalf (automation, Cadenza's controls, undo), to be mirrored into the
/// controller so the plugin's GUI shows them. Lock-free: the audio thread
/// stores, the main thread takes.
pub(crate) struct ParamMirror {
	values: Vec<AtomicU64>,
	dirty: Vec<AtomicBool>,
	any: AtomicBool,
}

impl ParamMirror {
	pub fn new(count: usize) -> Self {
		Self {
			values: (0..count).map(|_| AtomicU64::new(0)).collect(),
			dirty: (0..count).map(|_| AtomicBool::new(false)).collect(),
			any: AtomicBool::new(false),
		}
	}

	/// Realtime-safe.
	pub fn set(&self, index: usize, normalized: f64) {
		if let (Some(v), Some(d)) = (self.values.get(index), self.dirty.get(index)) {
			v.store(normalized.to_bits(), Ordering::Relaxed);
			d.store(true, Ordering::Release);
			self.any.store(true, Ordering::Release);
		}
	}

	/// Calls `f(index, normalized)` for every value set since the last call.
	pub fn take(&self, mut f: impl FnMut(usize, f64)) {
		if !self.any.swap(false, Ordering::AcqRel) {
			return;
		}
		for (i, d) in self.dirty.iter().enumerate() {
			if d.swap(false, Ordering::AcqRel) {
				f(i, f64::from_bits(self.values[i].load(Ordering::Relaxed)));
			}
		}
	}
}

/// What connects a `Vst3Plugin` to its processor's editor handles.
pub(crate) struct Attachment {
	pub shared: Arc<EditorShared>,
	pub mirror: Arc<ParamMirror>,
	/// VST3 param id per descriptor index.
	pub ids: Vec<u32>,
	/// Descriptor index per VST3 param id.
	pub index: HashMap<u32, u32>,
}

impl Attachment {
	pub fn new(shared: Arc<EditorShared>, mirror: Arc<ParamMirror>, ids: Vec<u32>) -> Self {
		let index = ids
			.iter()
			.enumerate()
			.map(|(i, &id)| (id, i as u32))
			.collect();
		Self {
			shared,
			mirror,
			ids,
			index,
		}
	}
}

// ------------------------------------------------------------- plug frame

struct TimerEntry {
	handler: ComPtr<ITimerHandler>,
	period: Duration,
	next: Instant,
}

#[derive(Default)]
struct FrameState {
	timers: Vec<TimerEntry>,
	handlers: Vec<(ComPtr<IEventHandler>, FileDescriptor)>,
	/// Our window and its current client size.
	window: u32,
	size: (u32, u32),
	#[cfg(all(unix, not(target_os = "macos")))]
	display: Option<std::rc::Rc<crate::editor::x11::Display>>,
}

/// `IPlugFrame` + `IRunLoop` given to a view (through `setFrame`; plugins
/// query the run loop from the frame). Lives on the plugin main thread.
pub(crate) struct PlugFrame {
	state: Mutex<FrameState>,
}

impl Class for PlugFrame {
	type Interfaces = (IPlugFrame, IRunLoop);
}

impl PlugFrame {
	fn lock(&self) -> MutexGuard<'_, FrameState> {
		self.state.lock().unwrap_or_else(|e| e.into_inner())
	}

	/// Handlers to call for timers due at `now` (rescheduled).
	fn due_timers(&self, now: Instant) -> Vec<ComPtr<ITimerHandler>> {
		let mut state = self.lock();
		let mut due = Vec::new();
		for t in &mut state.timers {
			if t.next <= now {
				due.push(t.handler.clone());
				t.next = (t.next + t.period).max(now + t.period / 2);
			}
		}
		due
	}

	fn next_deadline(&self) -> Option<Instant> {
		self.lock().timers.iter().map(|t| t.next).min()
	}

	fn fds(&self) -> Vec<FileDescriptor> {
		self.lock().handlers.iter().map(|(_, fd)| *fd).collect()
	}

	fn handlers_for(&self, fd: FileDescriptor) -> Vec<ComPtr<IEventHandler>> {
		self.lock()
			.handlers
			.iter()
			.filter(|(_, f)| *f == fd)
			.map(|(h, _)| h.clone())
			.collect()
	}

	/// Drops every registration (the view is gone).
	fn clear(&self) {
		let mut state = self.lock();
		state.timers.clear();
		state.handlers.clear();
	}
}

impl IPlugFrameTrait for PlugFrame {
	unsafe fn resizeView(&self, view: *mut IPlugView, new_size: *mut ViewRect) -> tresult {
		guard(kResultFalse, || {
			if view.is_null() || new_size.is_null() {
				return kInvalidArgument;
			}
			// SAFETY: checked non-null; the plugin passes a valid rect.
			let rect = unsafe { *new_size };
			let (w, h) = rect_size(&rect);
			if w == 0 || h == 0 {
				return kInvalidArgument;
			}
			{
				let mut state = self.lock();
				state.size = (w, h);
				#[cfg(all(unix, not(target_os = "macos")))]
				if let Some(d) = &state.display {
					d.set_size_hints(state.window, w, h, true);
					d.resize(state.window, w, h);
				}
			}
			// The SDK contract: the host resizes its window, then tells the view.
			let mut rect = ViewRect {
				left: 0,
				top: 0,
				right: w as i32,
				bottom: h as i32,
			};
			// SAFETY: the view is alive for the duration of this call.
			if let Some(view) = unsafe { ComRef::from_raw(view) } {
				unsafe { view.onSize(&mut rect) };
			}
			kResultTrue
		})
	}
}

impl IRunLoopTrait for PlugFrame {
	unsafe fn registerEventHandler(
		&self,
		handler: *mut IEventHandler,
		fd: FileDescriptor,
	) -> tresult {
		guard(kResultFalse, || {
			// SAFETY: a live handler from the plugin; we keep our own reference.
			let Some(handler) = (unsafe { ComRef::from_raw(handler) }) else {
				return kInvalidArgument;
			};
			self.lock().handlers.push((handler.to_com_ptr(), fd));
			kResultTrue
		})
	}

	unsafe fn unregisterEventHandler(&self, handler: *mut IEventHandler) -> tresult {
		guard(kResultFalse, || {
			if handler.is_null() {
				return kInvalidArgument;
			}
			self.lock().handlers.retain(|(h, _)| h.as_ptr() != handler);
			kResultTrue
		})
	}

	unsafe fn registerTimer(
		&self,
		handler: *mut ITimerHandler,
		milliseconds: TimerInterval,
	) -> tresult {
		guard(kResultFalse, || {
			// SAFETY: a live handler from the plugin; we keep our own reference.
			let Some(handler) = (unsafe { ComRef::from_raw(handler) }) else {
				return kInvalidArgument;
			};
			let period = Duration::from_millis(milliseconds).max(MIN_TIMER_PERIOD);
			self.lock().timers.push(TimerEntry {
				handler: handler.to_com_ptr(),
				period,
				next: Instant::now() + period,
			});
			kResultTrue
		})
	}

	unsafe fn unregisterTimer(&self, handler: *mut ITimerHandler) -> tresult {
		guard(kResultFalse, || {
			if handler.is_null() {
				return kInvalidArgument;
			}
			self.lock().timers.retain(|t| t.handler.as_ptr() != handler);
			kResultTrue
		})
	}
}

fn rect_size(r: &ViewRect) -> (u32, u32) {
	(
		(r.right - r.left).clamp(0, 16_384) as u32,
		(r.bottom - r.top).clamp(0, 16_384) as u32,
	)
}

// ----------------------------------------------------------------- editor

/// An open VST3 editor: the view, its frame and our window.
pub(crate) struct Vst3Editor {
	view: ComPtr<IPlugView>,
	frame: ComWrapper<PlugFrame>,
	window: u32,
	resizable: bool,
}

impl Vst3Editor {
	/// Detaches and releases the view; `destroy_window`: also destroy our
	/// window (false when it is already gone).
	fn teardown(self, destroy_window: bool) {
		// SAFETY: main thread; the view was attached by us.
		unsafe {
			self.view.removed();
			self.view.setFrame(std::ptr::null_mut());
		}
		self.frame.clear();
		#[cfg(all(unix, not(target_os = "macos")))]
		{
			let display = self.frame.lock().display.take();
			if destroy_window && let Some(d) = display {
				d.destroy(self.window);
			}
		}
		#[cfg(not(all(unix, not(target_os = "macos"))))]
		let _ = destroy_window;
	}
}

impl Drop for Vst3Plugin {
	fn drop(&mut self) {
		// The view must go before the controller is terminated.
		if let Some(editor) = self.editor.take() {
			editor.teardown(true);
		}
		self.teardown();
	}
}

/// Opens (or raises and retitles) the editor of instance `key`.
pub(crate) fn open(main: &mut MainState, key: u64, title: &str) -> Result<(), String> {
	#[cfg(all(unix, not(target_os = "macos")))]
	{
		let plugin = main
			.vst3
			.instances
			.get_mut(&key)
			.ok_or("the plugin instance no longer exists")?;
		if let Some(editor) = &plugin.editor {
			if let Some(d) = main.gui.connected() {
				d.set_title(editor.window, title);
				d.show(editor.window);
			}
			return Ok(());
		}
		let controller = plugin
			.controller()
			.ok_or("the plugin has no edit controller, so it has no editor")?
			.clone();
		// SAFETY: main-thread controller call; the returned view carries one
		// reference, which `from_raw` takes over.
		let view = unsafe { ComPtr::from_raw(controller.createView(c"editor".as_ptr())) }
			.ok_or("the plugin has no editor view")?;
		// SAFETY: plain view queries on the main thread.
		unsafe {
			if view.isPlatformTypeSupported(vst3::Steinberg::kPlatformTypeX11EmbedWindowID)
				!= kResultTrue
			{
				return Err("the plugin's editor cannot be embedded into an X11 window".into());
			}
		}
		let display = main.gui.display()?;
		let mut rect = ViewRect {
			left: 0,
			top: 0,
			right: 0,
			bottom: 0,
		};
		// SAFETY: valid out-parameter.
		let (w, h) = match unsafe { view.getSize(&mut rect) } {
			r if r == kResultOk || r == kResultTrue => rect_size(&rect),
			_ => (0, 0),
		};
		let (w, h) = if w == 0 || h == 0 { (800, 600) } else { (w, h) };
		// SAFETY: plain view query.
		let resizable = unsafe { view.canResize() } == kResultTrue;
		let plugin_id = plugin
			.attachment
			.as_ref()
			.map(|a| a.shared.plugin_id.clone())
			.unwrap_or_else(|| format!("vst3:{key}"));
		let window = display.create_window(title, &plugin_id, w, h, resizable)?;
		let Some(content) = display.content_window(window.top) else {
			display.destroy(window.top);
			return Err(
				"the editor window lost its content area before the plugin could embed".into(),
			);
		};
		let frame = ComWrapper::new(PlugFrame {
			state: Mutex::new(FrameState {
				window: window.top,
				size: (w, h),
				display: Some(display.clone()),
				..FrameState::default()
			}),
		});
		let frame_ptr = frame
			.as_com_ref::<IPlugFrame>()
			.map_or(std::ptr::null_mut(), |r| r.as_ptr());
		// SAFETY: the frame outlives the view's use of it (cleared in teardown);
		// `content` is a live X11 window id, passed as the platform handle.
		let attached = unsafe {
			view.setFrame(frame_ptr);
			view.attached(
				content as usize as *mut std::ffi::c_void,
				vst3::Steinberg::kPlatformTypeX11EmbedWindowID,
			)
		};
		if attached != kResultOk {
			// SAFETY: undo `setFrame` before releasing the view.
			unsafe { view.setFrame(std::ptr::null_mut()) };
			frame.clear();
			display.destroy(window.top);
			return Err("the plugin failed to attach its editor to the Cadenza window".into());
		}
		display.show(window.top);
		main.gui
			.windows
			.insert(window.top, crate::main_thread::Owner::Vst3(key));
		plugin.editor = Some(Vst3Editor {
			view,
			frame,
			window: window.top,
			resizable,
		});
		// Show what the processor is playing right away.
		plugin.sync_mirror();
		Ok(())
	}
	#[cfg(not(all(unix, not(target_os = "macos"))))]
	{
		let _ = (main, key, title);
		Err(crate::editor::UNSUPPORTED.into())
	}
}

fn teardown(main: &mut MainState, key: u64, destroy_window: bool) -> bool {
	let Some(editor) = main
		.vst3
		.instances
		.get_mut(&key)
		.and_then(|p| p.editor.take())
	else {
		return false;
	};
	main.gui.windows.remove(&editor.window);
	editor.teardown(destroy_window);
	true
}

/// Closes the editor at the host's request (no-op without one).
pub(crate) fn close(main: &mut MainState, key: u64) {
	teardown(main, key, true);
}

fn closed_by_user(main: &mut MainState, key: u64, destroy_window: bool) {
	if teardown(main, key, destroy_window)
		&& let Some(a) = main
			.vst3
			.instances
			.get(&key)
			.and_then(|p| p.attachment.as_ref())
	{
		a.shared.closed.store(true, Ordering::Release);
	}
}

#[cfg(all(unix, not(target_os = "macos")))]
pub(crate) fn window_event(main: &mut MainState, key: u64, event: crate::editor::x11::WindowEvent) {
	use crate::editor::x11::WindowEvent;
	match event {
		WindowEvent::CloseRequested => closed_by_user(main, key, true),
		WindowEvent::Destroyed => closed_by_user(main, key, false),
		WindowEvent::Resized { width, height } => {
			let Some(editor) = main
				.vst3
				.instances
				.get_mut(&key)
				.and_then(|p| p.editor.as_mut())
			else {
				return;
			};
			let current = editor.frame.lock().size;
			if (width, height) == current || !editor.resizable {
				return;
			}
			let mut rect = ViewRect {
				left: 0,
				top: 0,
				right: width as i32,
				bottom: height as i32,
			};
			// SAFETY: main-thread view calls with a valid rect.
			unsafe {
				editor.view.checkSizeConstraint(&mut rect);
				let (w, h) = rect_size(&rect);
				if w == 0 || h == 0 {
					return;
				}
				editor.frame.lock().size = (w, h);
				editor.view.onSize(&mut rect);
				if (w, h) != (width, height)
					&& let Some(d) = main.gui.connected()
				{
					d.resize(editor.window, w, h);
				}
			}
		}
	}
}

impl Vst3Plugin {
	/// Pushes host-side values into the controller (so its GUI shows them).
	pub fn sync_mirror(&self) {
		let (Some(a), Some(c)) = (&self.attachment, self.controller()) else {
			return;
		};
		a.mirror.take(|i, v| {
			if let Some(&id) = a.ids.get(i) {
				// SAFETY: main-thread controller call.
				unsafe { c.setParamNormalized(id, v) };
			}
		});
	}

	/// The component + controller state after mirroring every host value
	/// into the controller.
	pub fn save_state_synced(&self) -> Option<Vec<u8>> {
		self.sync_mirror();
		self.save_state()
	}

	/// Reports queued GUI edits and state changes to the editor handles.
	fn report_edits(&self) {
		let edits = self.flags().take_edits();
		let dirty = super::com::Vst3Flags::take(&self.flags().state_dirty);
		let changed = super::com::Vst3Flags::take(&self.flags().params_changed);
		let Some(a) = &self.attachment else {
			return;
		};
		if dirty || changed {
			a.shared.dirty.store(true, Ordering::Release);
		}
		for edit in edits {
			let (id, event) = match edit {
				RawEdit::Begin(id) => (id, None),
				RawEdit::Value(id, v) => (id, Some(v)),
				RawEdit::End(id) => (id, None),
			};
			let Some(param) = a.index.get(&id).and_then(|&i| a.shared.param_id(i)) else {
				continue;
			};
			let param = param.to_string();
			a.shared.push(match (edit, event) {
				(RawEdit::Begin(_), _) => EditorEvent::GestureBegin { param },
				(RawEdit::End(_), _) => EditorEvent::GestureEnd { param },
				(_, v) => {
					let normalized = v.unwrap_or(0.0);
					let plain = self
						.controller()
						// SAFETY: main-thread controller query.
						.map(|c| unsafe { c.normalizedParamToPlain(id, normalized) })
						.filter(|v| v.is_finite())
						.unwrap_or(normalized);
					EditorEvent::Value {
						param,
						value: plain,
					}
				}
			});
		}
	}
}

/// GUI edits, mirrored values and run-loop timers of every instance.
pub(crate) fn service(main: &mut MainState) {
	let now = Instant::now();
	let keys: Vec<u64> = main.vst3.instances.keys().copied().collect();
	for key in keys {
		let Some(plugin) = main.vst3.instances.get(&key) else {
			continue;
		};
		plugin.report_edits();
		let Some(editor) = &plugin.editor else {
			continue;
		};
		plugin.sync_mirror();
		let due = editor.frame.due_timers(now);
		for handler in due {
			// SAFETY: a live handler (we hold a reference), main thread.
			unsafe { handler.onTimer() };
		}
	}
}

pub(crate) fn next_deadline(vst3: &Vst3State) -> Option<Instant> {
	vst3.instances
		.values()
		.filter_map(|p| p.editor.as_ref()?.frame.next_deadline())
		.min()
}

#[cfg(unix)]
pub(crate) fn collect_fds(
	vst3: &Vst3State,
	fds: &mut Vec<libc::pollfd>,
	owners: &mut Vec<crate::main_thread::Owner>,
) {
	for (&key, p) in &vst3.instances {
		let Some(editor) = &p.editor else {
			continue;
		};
		for fd in editor.frame.fds() {
			fds.push(libc::pollfd {
				fd,
				events: libc::POLLIN,
				revents: 0,
			});
			owners.push(crate::main_thread::Owner::Vst3(key));
		}
	}
}

#[cfg(unix)]
pub(crate) fn on_fd(main: &mut MainState, key: u64, fd: i32) {
	let Some(editor) = main
		.vst3
		.instances
		.get(&key)
		.and_then(|p| p.editor.as_ref())
	else {
		return;
	};
	for handler in editor.frame.handlers_for(fd) {
		// SAFETY: a live handler (we hold a reference), main thread.
		unsafe { handler.onFDIsSet(fd) };
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn mirror_reports_each_changed_value_once() {
		let m = ParamMirror::new(3);
		m.set(2, 0.5);
		m.set(0, 0.1);
		m.set(2, 0.75);
		m.set(9, 1.0);
		let mut seen = Vec::new();
		m.take(|i, v| seen.push((i, v)));
		assert_eq!(seen, vec![(0, 0.1), (2, 0.75)]);
		seen.clear();
		m.take(|i, v| seen.push((i, v)));
		assert!(seen.is_empty());
	}

	#[test]
	fn run_loop_registrations_round_trip() {
		struct Tick(std::sync::atomic::AtomicU32);
		impl Class for Tick {
			type Interfaces = (ITimerHandler,);
		}
		impl ITimerHandlerTrait for Tick {
			unsafe fn onTimer(&self) {
				self.0.fetch_add(1, Ordering::Relaxed);
			}
		}
		let frame = ComWrapper::new(PlugFrame {
			state: Mutex::new(FrameState::default()),
		});
		let run_loop: ComRef<IRunLoop> = frame.as_com_ref().unwrap();
		let tick = ComWrapper::new(Tick(Default::default()));
		let handler = tick.as_com_ref::<ITimerHandler>().unwrap().as_ptr();
		unsafe {
			assert_eq!(run_loop.registerTimer(handler, 1), kResultTrue);
		}
		let later = Instant::now() + Duration::from_millis(50);
		for h in frame.due_timers(later) {
			unsafe { h.onTimer() };
		}
		assert_eq!(tick.0.load(Ordering::Relaxed), 1);
		assert!(frame.next_deadline().is_some_and(|t| t > later));
		unsafe {
			assert_eq!(run_loop.unregisterTimer(handler), kResultTrue);
		}
		assert!(frame.next_deadline().is_none());
		// The frame answers queryInterface for both interfaces.
		assert!(
			frame
				.as_com_ref::<IPlugFrame>()
				.unwrap()
				.cast::<IRunLoop>()
				.is_some()
		);
	}
}
