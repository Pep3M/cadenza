//! CLAP editors (`gui` extension) and the main-thread services plugins rely
//! on (timer-support, posix-fd-support), run by the plugin main thread.

use std::ffi::CString;
use std::sync::atomic::Ordering;
use std::time::Instant;

use clack_extensions::gui::{GuiApiType, GuiConfiguration, GuiSize, PluginGui};
use clack_extensions::state::PluginState;
use clack_extensions::timer::{PluginTimer, TimerId};

use super::ClapState;
use super::host::{GUI_CLOSED, GUI_DESTROYED, GUI_HIDE, GUI_SHOW, HostFlags};
use crate::main_thread::MainState;

/// Default editor size when the plugin does not report one.
const FALLBACK_SIZE: GuiSize = GuiSize {
	width: 800,
	height: 600,
};

pub(crate) enum ClapWindow {
	/// The plugin's own top-level window.
	Floating,
	/// Embedded into our top-level X11 window.
	#[cfg_attr(not(all(unix, not(target_os = "macos"))), allow(dead_code))]
	Embedded {
		window: u32,
		size: GuiSize,
		resizable: bool,
	},
}

/// An open CLAP editor.
pub(crate) struct ClapEditor {
	pub window: ClapWindow,
}

const X11_FLOATING: GuiConfiguration<'static> = GuiConfiguration {
	api_type: GuiApiType::X11,
	is_floating: true,
};
#[cfg(all(unix, not(target_os = "macos")))]
const X11_EMBEDDED: GuiConfiguration<'static> = GuiConfiguration {
	api_type: GuiApiType::X11,
	is_floating: false,
};

fn c_title(title: &str) -> CString {
	CString::new(title.replace('\0', " ")).unwrap_or_default()
}

/// Opens (or raises and retitles) the editor of instance `key`.
pub(crate) fn open(main: &mut MainState, key: u64, title: &str) -> Result<(), String> {
	let hosted = main
		.clap
		.instances
		.get_mut(&key)
		.ok_or("the plugin instance no longer exists")?;
	let handle = hosted.instance.plugin_handle();
	let gui = handle
		.get_extension::<PluginGui>()
		.ok_or("the plugin has no editor (it does not implement the CLAP gui extension)")?;
	let ctitle = c_title(title);
	if let Some(editor) = &hosted.editor {
		match &editor.window {
			ClapWindow::Floating => {
				gui.suggest_title(&handle, &ctitle);
				let _ = gui.show(&handle);
			}
			#[cfg(all(unix, not(target_os = "macos")))]
			ClapWindow::Embedded { window, .. } => {
				if let Some(d) = main.gui.connected() {
					d.set_title(*window, title);
					d.show(*window);
				}
			}
			#[cfg(not(all(unix, not(target_os = "macos"))))]
			ClapWindow::Embedded { .. } => {}
		}
		return Ok(());
	}
	if gui.is_api_supported(&handle, X11_FLOATING) {
		gui.create(&handle, X11_FLOATING)
			.map_err(|_| "the plugin failed to create its editor window".to_string())?;
		gui.suggest_title(&handle, &ctitle);
		if gui.show(&handle).is_err() {
			gui.destroy(&handle);
			return Err("the plugin failed to show its editor window".into());
		}
		hosted.editor = Some(ClapEditor {
			window: ClapWindow::Floating,
		});
		return Ok(());
	}
	#[cfg(all(unix, not(target_os = "macos")))]
	{
		if !gui.is_api_supported(&handle, X11_EMBEDDED) {
			return Err(
				"the plugin's editor supports neither X11 embedding nor floating X11 windows, \
				 which is all Cadenza offers on Linux"
					.into(),
			);
		}
		let display = main.gui.display()?;
		gui.create(&handle, X11_EMBEDDED)
			.map_err(|_| "the plugin failed to create its editor".to_string())?;
		let size = gui.get_size(&handle).unwrap_or(FALLBACK_SIZE);
		let resizable = gui.can_resize(&handle);
		let window = match display.create_window(title, size.width, size.height, resizable) {
			Ok(w) => w,
			Err(e) => {
				gui.destroy(&handle);
				return Err(e);
			}
		};
		let parent = clack_extensions::gui::Window::from_x11_handle(window as std::ffi::c_ulong);
		// SAFETY: `window` is a live X11 window that outlives the GUI (it is
		// destroyed only after `gui.destroy`).
		if unsafe { gui.set_parent(&handle, parent) }.is_err() {
			gui.destroy(&handle);
			display.destroy(window);
			return Err("the plugin refused to embed its editor into the Cadenza window".into());
		}
		let _ = gui.show(&handle);
		display.show(window);
		main.gui
			.windows
			.insert(window, crate::main_thread::Owner::Clap(key));
		hosted.editor = Some(ClapEditor {
			window: ClapWindow::Embedded {
				window,
				size,
				resizable,
			},
		});
		Ok(())
	}
	#[cfg(not(all(unix, not(target_os = "macos"))))]
	Err(crate::editor::UNSUPPORTED.into())
}

/// Destroys the editor of `key` (no-op without one). `destroyed`: the plugin
/// already destroyed its GUI (`gui.closed(true)`).
fn teardown(main: &mut MainState, key: u64, destroyed: bool) -> bool {
	let Some(hosted) = main.clap.instances.get_mut(&key) else {
		return false;
	};
	let Some(editor) = hosted.editor.take() else {
		return false;
	};
	if !destroyed {
		let handle = hosted.instance.plugin_handle();
		if let Some(gui) = handle.get_extension::<PluginGui>() {
			let _ = gui.hide(&handle);
			gui.destroy(&handle);
		}
	}
	#[cfg(all(unix, not(target_os = "macos")))]
	if let ClapWindow::Embedded { window, .. } = editor.window {
		main.gui.windows.remove(&window);
		if let Some(d) = main.gui.connected() {
			d.destroy(window);
		}
	}
	#[cfg(not(all(unix, not(target_os = "macos"))))]
	let _ = editor;
	true
}

/// Closes the editor at the host's request.
pub(crate) fn close(main: &mut MainState, key: u64) {
	teardown(main, key, false);
}

/// Closes the editor because the user (or the plugin) closed its window.
fn closed_by_user(main: &mut MainState, key: u64, destroyed: bool) {
	if teardown(main, key, destroyed)
		&& let Some(shared) = main
			.clap
			.instances
			.get(&key)
			.and_then(|h| h.shared.as_ref())
	{
		shared.closed.store(true, Ordering::Release);
	}
}

#[cfg(all(unix, not(target_os = "macos")))]
pub(crate) fn window_event(main: &mut MainState, key: u64, event: crate::editor::x11::WindowEvent) {
	use crate::editor::x11::WindowEvent;
	match event {
		WindowEvent::CloseRequested => closed_by_user(main, key, false),
		WindowEvent::Destroyed => {
			// Our window is gone: forget it before the plugin destroys its view.
			if let Some(ClapEditor {
				window: ClapWindow::Embedded { window, .. },
			}) = main
				.clap
				.instances
				.get(&key)
				.and_then(|h| h.editor.as_ref())
			{
				main.gui.windows.remove(window);
			}
			closed_by_user(main, key, false);
		}
		WindowEvent::Resized { width, height } => {
			let display = main.gui.connected();
			let Some(hosted) = main.clap.instances.get_mut(&key) else {
				return;
			};
			let Some(ClapEditor {
				window: ClapWindow::Embedded {
					window,
					size,
					resizable,
				},
			}) = &mut hosted.editor
			else {
				return;
			};
			let wanted = GuiSize { width, height };
			if wanted == *size || !*resizable {
				return;
			}
			let handle = hosted.instance.plugin_handle();
			let Some(gui) = handle.get_extension::<PluginGui>() else {
				return;
			};
			let adjusted = gui.adjust_size(&handle, wanted).unwrap_or(wanted);
			if gui.set_size(&handle, adjusted).is_ok() {
				*size = adjusted;
			}
			if *size != wanted
				&& let Some(d) = display
			{
				d.resize(*window, size.width, size.height);
			}
		}
	}
}

/// Serves GUI requests, timers and state notifications of every instance.
pub(crate) fn service(main: &mut MainState) {
	let now = Instant::now();
	let keys: Vec<u64> = main.clap.instances.keys().copied().collect();
	for key in keys {
		let Some(hosted) = main.clap.instances.get_mut(&key) else {
			continue;
		};
		let due = hosted.flags.registry().due_timers(now);
		if !due.is_empty() {
			let handle = hosted.instance.plugin_handle();
			if let Some(timer) = handle.get_extension::<PluginTimer>() {
				for id in due {
					timer.on_timer(&handle, TimerId(id));
				}
			}
		}
		let flags = hosted.flags.clone();
		if let Some(shared) = &hosted.shared {
			let dirty = HostFlags::take(&flags.state_dirty);
			let rescanned = HostFlags::take(&flags.params_rescan);
			if dirty || rescanned {
				shared.dirty.store(true, Ordering::Release);
			}
		}
		if hosted.editor.is_none() {
			continue;
		}
		match flags.gui_closed.swap(0, Ordering::AcqRel) {
			GUI_DESTROYED => {
				closed_by_user(main, key, true);
				continue;
			}
			GUI_CLOSED => {
				closed_by_user(main, key, false);
				continue;
			}
			_ => {}
		}
		apply_requests(main, key, &flags);
	}
}

/// Plugin-initiated resize / show / hide / hint changes.
fn apply_requests(main: &mut MainState, key: u64, flags: &HostFlags) {
	#[cfg(all(unix, not(target_os = "macos")))]
	let display = main.gui.connected();
	let Some(hosted) = main.clap.instances.get_mut(&key) else {
		return;
	};
	let handle = hosted.instance.plugin_handle();
	let Some(gui) = handle.get_extension::<PluginGui>() else {
		return;
	};
	let Some(editor) = &mut hosted.editor else {
		return;
	};
	let resize = flags.gui_resize.swap(0, Ordering::AcqRel);
	let visibility = flags.gui_visibility.swap(0, Ordering::AcqRel);
	let hints = HostFlags::take(&flags.gui_hints);
	match &mut editor.window {
		ClapWindow::Floating => match visibility {
			GUI_SHOW => {
				let _ = gui.show(&handle);
			}
			GUI_HIDE => {
				let _ = gui.hide(&handle);
			}
			_ => {}
		},
		#[cfg(all(unix, not(target_os = "macos")))]
		ClapWindow::Embedded {
			window,
			size,
			resizable,
		} => {
			let Some(d) = display else {
				return;
			};
			if resize != 0 {
				let wanted = GuiSize::unpack_from_u64(resize);
				if wanted != *size {
					*size = wanted;
					if !*resizable {
						d.set_size_hints(*window, wanted.width, wanted.height, false);
					}
					d.resize(*window, wanted.width, wanted.height);
				}
			}
			if hints {
				*resizable = gui.can_resize(&handle);
				d.set_size_hints(*window, size.width, size.height, *resizable);
			}
			match visibility {
				GUI_SHOW => d.show(*window),
				GUI_HIDE => d.hide(*window),
				_ => {}
			}
		}
		#[cfg(not(all(unix, not(target_os = "macos"))))]
		ClapWindow::Embedded { .. } => {
			let _ = (resize, hints);
		}
	}
}

pub(crate) fn next_deadline(clap: &ClapState) -> Option<Instant> {
	clap.instances
		.values()
		.filter_map(|h| h.flags.registry().next_deadline())
		.min()
}

#[cfg(unix)]
pub(crate) fn collect_fds(
	clap: &ClapState,
	fds: &mut Vec<libc::pollfd>,
	owners: &mut Vec<crate::main_thread::Owner>,
) {
	use clack_extensions::posix_fd::FdFlags;
	for (&key, h) in &clap.instances {
		for &(fd, flags) in &h.flags.registry().fds {
			let flags = FdFlags::from_bits_truncate(flags);
			let mut events = 0;
			if flags.contains(FdFlags::READ) {
				events |= libc::POLLIN;
			}
			if flags.contains(FdFlags::WRITE) {
				events |= libc::POLLOUT;
			}
			fds.push(libc::pollfd {
				fd,
				events,
				revents: 0,
			});
			owners.push(crate::main_thread::Owner::Clap(key));
		}
	}
}

#[cfg(unix)]
pub(crate) fn on_fd(main: &mut MainState, key: u64, fd: i32, revents: i16) {
	use clack_extensions::posix_fd::{FdFlags, PluginPosixFd};
	let Some(hosted) = main.clap.instances.get_mut(&key) else {
		return;
	};
	// Only fds still registered (a callback may have unregistered it).
	if !hosted.flags.registry().fds.iter().any(|&(f, _)| f == fd) {
		return;
	}
	let mut flags = FdFlags::empty();
	if revents & libc::POLLIN != 0 {
		flags |= FdFlags::READ;
	}
	if revents & libc::POLLOUT != 0 {
		flags |= FdFlags::WRITE;
	}
	if revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
		flags |= FdFlags::ERROR;
	}
	let handle = hosted.instance.plugin_handle();
	if let Some(ext) = handle.get_extension::<PluginPosixFd>() {
		ext.on_fd(&handle, fd, flags);
	}
}

/// The live state of instance `key` (main thread; the audio thread may be
/// processing, which CLAP allows for `state.save`).
pub(crate) fn save_state(main: &mut MainState, key: u64) -> Option<Vec<u8>> {
	let hosted = main.clap.instances.get_mut(&key)?;
	let handle = hosted.instance.plugin_handle();
	let state = handle.get_extension::<PluginState>()?;
	let mut data = Vec::new();
	state.save(&handle, &mut data).ok()?;
	Some(data)
}
