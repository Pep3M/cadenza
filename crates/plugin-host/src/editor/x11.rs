//! Top-level X11 windows that plugin editors embed into.
//!
//! Plugin GUIs on Linux are X11 children (CLAP `x11` API, VST3
//! `X11EmbedWindowID`), so each editor gets its own top-level window on the X
//! server (XWayland on Wayland desktops): the app's own window is a native
//! Wayland surface and cannot be a parent. The connection is `x11rb`'s pure
//! Rust one (no libX11/libxcb needed; the crate is already part of the GUI
//! stack), owned by the plugin main thread.

use std::os::fd::{AsRawFd, RawFd};

use x11rb::connection::Connection;
use x11rb::properties::WmSizeHints;
use x11rb::protocol::Event;
use x11rb::protocol::xproto::{
	AtomEnum, ClientMessageEvent, ConfigureWindowAux, ConnectionExt as _, CreateWindowAux,
	EventMask, PropMode, Window, WindowClass,
};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

/// Something the window manager or the user did to one of our windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WindowEvent {
	/// The window now has this client size (user resize, or our own request).
	Resized { width: u32, height: u32 },
	/// The user asked to close the window (close button).
	CloseRequested,
	/// The window is gone.
	Destroyed,
}

struct Atoms {
	wm_protocols: u32,
	wm_delete_window: u32,
	net_wm_name: u32,
	utf8_string: u32,
	net_wm_window_type: u32,
	net_wm_window_type_dialog: u32,
	net_wm_pid: u32,
}

/// One connection to the X server.
pub(crate) struct Display {
	conn: RustConnection,
	root: Window,
	root_depth: u8,
	root_visual: u32,
	atoms: Atoms,
}

impl Display {
	/// Connects to `$DISPLAY`.
	pub fn connect() -> Result<Self, String> {
		let (conn, screen) = x11rb::connect(None).map_err(|e| {
			format!(
				"cannot connect to the X server ({e}); plugin editors are X11 windows: on Wayland, \
				 make sure XWayland is running and DISPLAY is set"
			)
		})?;
		let s = &conn.setup().roots[screen];
		let (root, root_depth, root_visual) = (s.root, s.root_depth, s.root_visual);
		let intern = |name: &[u8]| -> Result<u32, String> {
			conn.intern_atom(false, name)
				.map_err(|e| e.to_string())?
				.reply()
				.map(|r| r.atom)
				.map_err(|e| e.to_string())
		};
		let atoms = Atoms {
			wm_protocols: intern(b"WM_PROTOCOLS")?,
			wm_delete_window: intern(b"WM_DELETE_WINDOW")?,
			net_wm_name: intern(b"_NET_WM_NAME")?,
			utf8_string: intern(b"UTF8_STRING")?,
			net_wm_window_type: intern(b"_NET_WM_WINDOW_TYPE")?,
			net_wm_window_type_dialog: intern(b"_NET_WM_WINDOW_TYPE_DIALOG")?,
			net_wm_pid: intern(b"_NET_WM_PID")?,
		};
		Ok(Self {
			conn,
			root,
			root_depth,
			root_visual,
			atoms,
		})
	}

	pub fn fd(&self) -> RawFd {
		self.conn.stream().as_raw_fd()
	}

	/// Creates (unmapped) a top-level window of `width` x `height` pixels.
	pub fn create_window(
		&self,
		title: &str,
		width: u32,
		height: u32,
		resizable: bool,
	) -> Result<Window, String> {
		let err = |e: &dyn std::fmt::Display| format!("cannot create the editor window: {e}");
		let id = self.conn.generate_id().map_err(|e| err(&e))?;
		let (w, h) = (clamp_dim(width), clamp_dim(height));
		self.conn
			.create_window(
				self.root_depth,
				id,
				self.root,
				0,
				0,
				w,
				h,
				0,
				WindowClass::INPUT_OUTPUT,
				self.root_visual,
				&CreateWindowAux::new()
					.background_pixel(0)
					.event_mask(EventMask::STRUCTURE_NOTIFY),
			)
			.map_err(|e| err(&e))?;
		let a = &self.atoms;
		let setup = || -> Result<(), x11rb::errors::ConnectionError> {
			self.conn.change_property32(
				PropMode::REPLACE,
				id,
				a.wm_protocols,
				AtomEnum::ATOM,
				&[a.wm_delete_window],
			)?;
			// Dialog windows float in tiling window managers (Hyprland, sway).
			self.conn.change_property32(
				PropMode::REPLACE,
				id,
				a.net_wm_window_type,
				AtomEnum::ATOM,
				&[a.net_wm_window_type_dialog],
			)?;
			self.conn.change_property32(
				PropMode::REPLACE,
				id,
				a.net_wm_pid,
				AtomEnum::CARDINAL,
				&[std::process::id()],
			)?;
			self.conn.change_property8(
				PropMode::REPLACE,
				id,
				AtomEnum::WM_CLASS,
				AtomEnum::STRING,
				b"cadenza-plugin-editor\0Cadenza\0",
			)?;
			Ok(())
		};
		setup().map_err(|e| err(&e))?;
		self.set_title(id, title);
		self.set_size_hints(id, width, height, resizable);
		let _ = self.conn.flush();
		Ok(id)
	}

	pub fn set_title(&self, window: Window, title: &str) {
		let a = &self.atoms;
		let _ = self.conn.change_property8(
			PropMode::REPLACE,
			window,
			a.net_wm_name,
			a.utf8_string,
			title.as_bytes(),
		);
		// Legacy WM_NAME (Latin-1) for window managers without EWMH.
		let latin: Vec<u8> = title
			.chars()
			.map(|c| u8::try_from(u32::from(c)).unwrap_or(b'?'))
			.collect();
		let _ = self.conn.change_property8(
			PropMode::REPLACE,
			window,
			AtomEnum::WM_NAME,
			AtomEnum::STRING,
			&latin,
		);
		let _ = self.conn.flush();
	}

	/// Fixed-size windows get min = max hints (the window manager then refuses
	/// user resizes and, in tiling managers, floats the window).
	pub fn set_size_hints(&self, window: Window, width: u32, height: u32, resizable: bool) {
		let mut hints = WmSizeHints::new();
		let size = (i32::from(clamp_dim(width)), i32::from(clamp_dim(height)));
		if resizable {
			hints.min_size = Some((64, 64));
		} else {
			hints.min_size = Some(size);
			hints.max_size = Some(size);
		}
		let _ = hints.set_normal_hints(&self.conn, window);
		let _ = self.conn.flush();
	}

	pub fn resize(&self, window: Window, width: u32, height: u32) {
		let _ = self.conn.configure_window(
			window,
			&ConfigureWindowAux::new()
				.width(u32::from(clamp_dim(width)))
				.height(u32::from(clamp_dim(height))),
		);
		let _ = self.conn.flush();
	}

	pub fn show(&self, window: Window) {
		let _ = self.conn.map_window(window);
		let _ = self.conn.flush();
	}

	pub fn hide(&self, window: Window) {
		let _ = self.conn.unmap_window(window);
		let _ = self.conn.flush();
	}

	pub fn destroy(&self, window: Window) {
		let _ = self.conn.destroy_window(window);
		let _ = self.conn.flush();
	}

	/// Reads every queued event (never blocks) and returns the ones about our
	/// windows. `Err` when the connection to the server is gone.
	pub fn events(&self) -> Result<Vec<(Window, WindowEvent)>, String> {
		let mut out = Vec::new();
		loop {
			let event = match self.conn.poll_for_event() {
				Ok(Some(e)) => e,
				Ok(None) => return Ok(out),
				Err(e) => return Err(e.to_string()),
			};
			match event {
				Event::ConfigureNotify(e) => out.push((
					e.window,
					WindowEvent::Resized {
						width: u32::from(e.width),
						height: u32::from(e.height),
					},
				)),
				Event::ClientMessage(e) if self.is_delete(&e) => {
					out.push((e.window, WindowEvent::CloseRequested));
				}
				Event::DestroyNotify(e) => out.push((e.window, WindowEvent::Destroyed)),
				_ => {}
			}
		}
	}

	fn is_delete(&self, e: &ClientMessageEvent) -> bool {
		e.type_ == self.atoms.wm_protocols
			&& e.format == 32
			&& e.data.as_data32()[0] == self.atoms.wm_delete_window
	}
}

fn clamp_dim(v: u32) -> u16 {
	v.clamp(1, 16_384) as u16
}

/// The client size of the window titled exactly `title` on `$DISPLAY`, if
/// one exists (searches the root's children and their children: window
/// managers often reparent clients into frames). For tests and diagnostics.
pub fn find_window_titled(title: &str) -> Result<Option<(u32, u32)>, String> {
	let display = Display::connect()?;
	let conn = &display.conn;
	let tree = |w: Window| -> Vec<Window> {
		conn.query_tree(w)
			.ok()
			.and_then(|c| c.reply().ok())
			.map(|r| r.children)
			.unwrap_or_default()
	};
	let name_of = |w: Window| -> Option<String> {
		let reply = conn
			.get_property(
				false,
				w,
				display.atoms.net_wm_name,
				display.atoms.utf8_string,
				0,
				1024,
			)
			.ok()?
			.reply()
			.ok()?;
		(!reply.value.is_empty()).then(|| String::from_utf8_lossy(&reply.value).into_owned())
	};
	let mut stack = tree(display.root);
	let mut depth = 0;
	while !stack.is_empty() && depth < 3 {
		let mut next = Vec::new();
		for w in stack {
			if name_of(w).as_deref() == Some(title) {
				let size = conn
					.get_geometry(w)
					.ok()
					.and_then(|c| c.reply().ok())
					.map_or((0, 0), |g| (u32::from(g.width), u32::from(g.height)));
				return Ok(Some(size));
			}
			next.extend(tree(w));
		}
		stack = next;
		depth += 1;
	}
	Ok(None)
}
