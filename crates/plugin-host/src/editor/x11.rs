//! Top-level X11 windows that plugin editors embed into.
//!
//! Plugin GUIs on Linux are X11 children (CLAP `x11` API, VST3
//! `X11EmbedWindowID`), so each editor gets its own top-level window on the X
//! server (XWayland on Wayland desktops): the app's own window is a native
//! Wayland surface and cannot be a parent. The connection is `x11rb`'s pure
//! Rust one (no libX11/libxcb needed; the crate is already part of the GUI
//! stack), owned by the plugin main thread.
//!
//! # The title strip
//!
//! Hyprland (like most wlroots compositors) draws no decorations for
//! XWayland windows, so there is nothing to grab to move a plugin editor.
//! Every top-level window we create is therefore `content_height +
//! [`geometry::STRIP_HEIGHT`]` tall: a host-drawn strip at the top (title,
//! close button) and the plugin's embedded window below it, at
//! [`geometry::content_offset`]. The plugin only ever sees its own content
//! size; `geometry` converts both ways.
//!
//! Dragging the strip moves the window ourselves: the pointer is grabbed for
//! the whole gesture and every motion places the window at the pointer minus
//! the offset where the strip was pressed (`ConfigureWindow`). Nothing depends
//! on positions reported earlier, so a window manager that is late with (or
//! never sends) `ConfigureNotify` cannot make the window jump. Handing the move
//! to the window manager with `_NET_WM_MOVERESIZE` proved unreliable under
//! Hyprland (the drag dropped mid-gesture and fought our own moves). Text is drawn with the
//! tiny built-in bitmap font in `font.rs` rather than X11 core fonts: XWayland
//! here ships none of the classic core font packages, so `OpenFont` cannot be
//! relied on.

use std::cell::RefCell;
use std::collections::HashMap;
use std::os::fd::{AsRawFd, RawFd};

use x11rb::connection::Connection;
use x11rb::properties::{WmSizeHints, WmSizeHintsSpecification};
use x11rb::protocol::Event;
use x11rb::protocol::randr::ConnectionExt as _;
use x11rb::protocol::xproto::{
	AtomEnum, ChangeGCAux, ClientMessageEvent, ConfigureWindowAux, ConnectionExt as _, CreateGCAux,
	CreateWindowAux, EventMask, Gcontext, GrabMode, KeyButMask, PropMode, Rectangle, Segment,
	Window, WindowClass,
};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

use super::{font, geometry};

/// Something the window manager or the user did to one of our windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WindowEvent {
	/// The plugin's content area now has this size (user resize of the
	/// outer window minus the strip, or our own request echoed back).
	Resized { width: u32, height: u32 },
	/// The user asked to close the window (close button, strip's × button).
	CloseRequested,
	/// The window is gone.
	Destroyed,
}

/// Host-drawn strip colors (dark, matching the app's theme).
const STRIP_BG: u32 = 0x1b_1e24;
const CLOSE_HOVER_BG: u32 = 0xaa_3b3b;
const TEXT_COLOR: u32 = 0xd7_dadd;

struct Atoms {
	wm_protocols: u32,
	wm_delete_window: u32,
	net_wm_name: u32,
	utf8_string: u32,
	net_wm_window_type: u32,
	net_wm_window_type_dialog: u32,
	net_wm_pid: u32,
}

/// A pointer-drag of the strip in progress.
#[derive(Debug, Clone, Copy)]
struct DragAnchor {
	/// Pointer position inside our window when the strip was pressed.
	offset: (i32, i32),
}

/// Everything we keep about one of our top-level editor windows, keyed by
/// its id.
struct Strip {
	/// The child window the plugin embeds into (below the strip).
	content: Window,
	gc: Gcontext,
	plugin_id: String,
	title: String,
	/// Outer window width (== content width; the strip spans it).
	width: u32,
	pos: (i32, i32),
	hover_close: bool,
	drag: Option<DragAnchor>,
}

/// A newly created editor window: the outer one (titled, carries the strip).
/// Its embedding child (below the strip) is looked up with
/// [`Display::content_window`] when a plugin is ready to attach to it.
pub(crate) struct EditorWindow {
	pub top: Window,
}

/// One connection to the X server.
pub(crate) struct Display {
	conn: RustConnection,
	root: Window,
	root_depth: u8,
	root_visual: u32,
	atoms: Atoms,
	strips: RefCell<HashMap<Window, Strip>>,
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
			strips: RefCell::new(HashMap::new()),
		})
	}

	pub fn fd(&self) -> RawFd {
		self.conn.stream().as_raw_fd()
	}

	/// The rectangle of the whole root (fallback when RandR is unavailable).
	fn root_rect(&self) -> geometry::Rect {
		self.conn
			.get_geometry(self.root)
			.ok()
			.and_then(|c| c.reply().ok())
			.map(|g| geometry::Rect {
				x: 0,
				y: 0,
				width: u32::from(g.width),
				height: u32::from(g.height),
			})
			.unwrap_or(geometry::Rect {
				x: 0,
				y: 0,
				width: 1920,
				height: 1080,
			})
	}

	/// The RandR monitor the pointer is currently on, if RandR is usable and
	/// reports any monitor (falls back to the first one when the pointer is
	/// on none of them, e.g. briefly between layout changes).
	fn pointer_monitor(&self) -> Option<geometry::Rect> {
		let pointer = self.conn.query_pointer(self.root).ok()?.reply().ok()?;
		let (px, py) = (i32::from(pointer.root_x), i32::from(pointer.root_y));
		let reply = self
			.conn
			.randr_get_monitors(self.root, true)
			.ok()?
			.reply()
			.ok()?;
		let rects: Vec<geometry::Rect> = reply
			.monitors
			.iter()
			.map(|m| geometry::Rect {
				x: i32::from(m.x),
				y: i32::from(m.y),
				width: u32::from(m.width),
				height: u32::from(m.height),
			})
			.collect();
		rects
			.iter()
			.copied()
			.find(|r| r.contains(px, py))
			.or_else(|| rects.first().copied())
	}

	/// Where a new window of this size should go: the position remembered
	/// for `plugin_id` this session, or centered on the monitor the pointer
	/// is on (whole root if RandR is unavailable).
	fn initial_position(&self, plugin_id: &str, window_w: u32, window_h: u32) -> (i32, i32) {
		if let Some(pos) = super::remembered_position(plugin_id) {
			return pos;
		}
		let monitor = self.pointer_monitor().unwrap_or_else(|| self.root_rect());
		geometry::centered_on(monitor, window_w, window_h)
	}

	/// Creates (unmapped) a top-level window embedding a `width` x `height`
	/// plugin view below a title strip; `plugin_id` keys the remembered
	/// position for this session.
	pub fn create_window(
		&self,
		title: &str,
		plugin_id: &str,
		width: u32,
		height: u32,
		resizable: bool,
	) -> Result<EditorWindow, String> {
		let err = |e: &dyn std::fmt::Display| format!("cannot create the editor window: {e}");
		let top = self.conn.generate_id().map_err(|e| err(&e))?;
		let content = self.conn.generate_id().map_err(|e| err(&e))?;
		let gc = self.conn.generate_id().map_err(|e| err(&e))?;
		let (cw, ch) = (clamp_dim(width), clamp_dim(height));
		let (outer_w, outer_h) = geometry::outer_size(u32::from(cw), u32::from(ch));
		let (ox, oy) = geometry::content_offset();
		let (x, y) = self.initial_position(plugin_id, outer_w, outer_h);

		self.conn
			.create_window(
				self.root_depth,
				top,
				self.root,
				clamp_coord(x),
				clamp_coord(y),
				clamp_dim(outer_w),
				clamp_dim(outer_h),
				0,
				WindowClass::INPUT_OUTPUT,
				self.root_visual,
				&CreateWindowAux::new()
					.background_pixel(STRIP_BG)
					.event_mask(
						EventMask::STRUCTURE_NOTIFY
							| EventMask::EXPOSURE | EventMask::BUTTON_PRESS
							| EventMask::BUTTON_RELEASE
							| EventMask::POINTER_MOTION,
					),
			)
			.map_err(|e| err(&e))?;
		self.conn
			.create_window(
				self.root_depth,
				content,
				top,
				ox as i16,
				oy as i16,
				cw,
				ch,
				0,
				WindowClass::INPUT_OUTPUT,
				self.root_visual,
				&CreateWindowAux::new().background_pixel(0),
			)
			.map_err(|e| err(&e))?;
		self.conn
			.create_gc(
				gc,
				top,
				&CreateGCAux::new().foreground(STRIP_BG).line_width(2),
			)
			.map_err(|e| err(&e))?;

		let a = &self.atoms;
		let setup = || -> Result<(), x11rb::errors::ConnectionError> {
			self.conn.change_property32(
				PropMode::REPLACE,
				top,
				a.wm_protocols,
				AtomEnum::ATOM,
				&[a.wm_delete_window],
			)?;
			// Dialog windows float in tiling window managers (Hyprland, sway).
			self.conn.change_property32(
				PropMode::REPLACE,
				top,
				a.net_wm_window_type,
				AtomEnum::ATOM,
				&[a.net_wm_window_type_dialog],
			)?;
			self.conn.change_property32(
				PropMode::REPLACE,
				top,
				a.net_wm_pid,
				AtomEnum::CARDINAL,
				&[std::process::id()],
			)?;
			self.conn.change_property8(
				PropMode::REPLACE,
				top,
				AtomEnum::WM_CLASS,
				AtomEnum::STRING,
				b"cadenza-plugin-editor\0Cadenza\0",
			)?;
			Ok(())
		};
		setup().map_err(|e| err(&e))?;

		self.strips.borrow_mut().insert(
			top,
			Strip {
				content,
				gc,
				plugin_id: plugin_id.to_string(),
				title: title.to_string(),
				width: outer_w,
				pos: (x, y),
				hover_close: false,
				drag: None,
			},
		);
		self.set_title(top, title);
		self.apply_size_hints(top, width, height, resizable, Some((x, y)));
		let _ = self.conn.map_window(content);
		let _ = self.conn.flush();
		Ok(EditorWindow { top })
	}

	/// The child window `top` embeds a plugin's view into, if `top` is one
	/// of our editor windows.
	pub fn content_window(&self, top: Window) -> Option<Window> {
		self.strips.borrow().get(&top).map(|s| s.content)
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
		if let Some(s) = self.strips.borrow_mut().get_mut(&window) {
			s.title = title.to_string();
		}
		self.redraw(window);
		let _ = self.conn.flush();
	}

	/// Fixed-size windows get min = max hints (the window manager then
	/// refuses user resizes and, in tiling managers, floats the window).
	/// `width`/`height` are the plugin's content size; the strip is added.
	pub fn set_size_hints(&self, window: Window, width: u32, height: u32, resizable: bool) {
		self.apply_size_hints(window, width, height, resizable, None);
	}

	fn apply_size_hints(
		&self,
		window: Window,
		width: u32,
		height: u32,
		resizable: bool,
		position: Option<(i32, i32)>,
	) {
		let (cw, ch) = (clamp_dim(width), clamp_dim(height));
		let (outer_w, outer_h) = geometry::outer_size(u32::from(cw), u32::from(ch));
		let mut hints = WmSizeHints::new();
		let size = (outer_w as i32, outer_h as i32);
		if resizable {
			hints.min_size = Some((120, geometry::STRIP_HEIGHT as i32 + 64));
		} else {
			hints.min_size = Some(size);
			hints.max_size = Some(size);
		}
		if let Some((x, y)) = position {
			hints.position = Some((WmSizeHintsSpecification::UserSpecified, x, y));
		}
		let _ = hints.set_normal_hints(&self.conn, window);
		let _ = self.conn.flush();
	}

	/// Resizes the outer window so its plugin content area becomes `width` x
	/// `height`, keeping the strip on top of it.
	pub fn resize(&self, window: Window, width: u32, height: u32) {
		let (cw, ch) = (clamp_dim(width), clamp_dim(height));
		let (outer_w, outer_h) = geometry::outer_size(u32::from(cw), u32::from(ch));
		let _ = self.conn.configure_window(
			window,
			&ConfigureWindowAux::new()
				.width(u32::from(clamp_dim(outer_w)))
				.height(u32::from(clamp_dim(outer_h))),
		);
		let content = self.strips.borrow().get(&window).map(|s| s.content);
		if let Some(content) = content {
			let _ = self.conn.configure_window(
				content,
				&ConfigureWindowAux::new()
					.width(u32::from(cw))
					.height(u32::from(ch)),
			);
		}
		if let Some(s) = self.strips.borrow_mut().get_mut(&window) {
			s.width = outer_w;
		}
		self.redraw(window);
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
		if let Some(s) = self.strips.borrow_mut().remove(&window) {
			let _ = self.conn.free_gc(s.gc);
		}
		let _ = self.conn.destroy_window(window);
		let _ = self.conn.flush();
	}

	/// Redraws the strip (background, close button, title) of one of our
	/// windows. A no-op for any other window (plugin child windows, etc.).
	fn redraw(&self, window: Window) {
		let Some((gc, title, width, hover_close)) = self
			.strips
			.borrow()
			.get(&window)
			.map(|s| (s.gc, s.title.clone(), s.width, s.hover_close))
		else {
			return;
		};
		let _ = self
			.conn
			.change_gc(gc, &ChangeGCAux::new().foreground(STRIP_BG));
		let _ = self.conn.poly_fill_rectangle(
			window,
			gc,
			&[Rectangle {
				x: 0,
				y: 0,
				width: width as u16,
				height: geometry::STRIP_HEIGHT as u16,
			}],
		);
		let close = geometry::close_button_rect(width);
		if hover_close {
			let _ = self
				.conn
				.change_gc(gc, &ChangeGCAux::new().foreground(CLOSE_HOVER_BG));
			let _ = self.conn.poly_fill_rectangle(
				window,
				gc,
				&[Rectangle {
					x: close.x as i16,
					y: close.y as i16,
					width: close.width as u16,
					height: close.height as u16,
				}],
			);
		}
		let _ = self
			.conn
			.change_gc(gc, &ChangeGCAux::new().foreground(TEXT_COLOR));
		let pad = 7i16;
		let (x0, y0) = (close.x as i16 + pad, close.y as i16 + pad);
		let (x1, y1) = (
			close.x as i16 + close.width as i16 - pad,
			close.y as i16 + close.height as i16 - pad,
		);
		let _ = self.conn.poly_segment(
			window,
			gc,
			&[
				Segment {
					x1: x0,
					y1: y0,
					x2: x1,
					y2: y1,
				},
				Segment {
					x1,
					y1: y0,
					x2: x0,
					y2: y1,
				},
			],
		);
		let text_right = close.x as u32;
		let text_max_width = text_right.saturating_sub(16);
		draw_text(&self.conn, window, gc, 10, &title, text_max_width);
		let _ = self.conn.flush();
	}

	fn begin_drag(&self, window: Window, offset: (i32, i32), time: u32) {
		match self.strips.borrow_mut().get_mut(&window) {
			Some(s) => s.drag = Some(DragAnchor { offset }),
			None => return,
		}
		// An explicit grab keeps motion and the release coming to us even when a
		// fast move leaves the window behind the pointer.
		let _ = self.conn.grab_pointer(
			false,
			window,
			EventMask::BUTTON_RELEASE | EventMask::POINTER_MOTION,
			GrabMode::ASYNC,
			GrabMode::ASYNC,
			x11rb::NONE,
			x11rb::NONE,
			time,
		);
		let _ = self.conn.flush();
	}

	fn end_drag(&self, window: Window, time: u32) {
		if let Some(s) = self.strips.borrow_mut().get_mut(&window)
			&& s.drag.take().is_some()
		{
			let _ = self.conn.ungrab_pointer(time);
			let _ = self.conn.flush();
		}
	}

	/// Position/size bookkeeping common to every `ConfigureNotify` (resize,
	/// move, or both at once): remembers the position per plugin id and
	/// redraws the strip at its (possibly new) width.
	fn on_configure(&self, window: Window, x: i32, y: i32, outer_width: u32) {
		let plugin_id = {
			let mut strips = self.strips.borrow_mut();
			let Some(s) = strips.get_mut(&window) else {
				return;
			};
			s.pos = (x, y);
			s.width = outer_width;
			s.plugin_id.clone()
		};
		super::remember_position(&plugin_id, (x, y));
		self.redraw(window);
	}

	/// Reads every queued event (never blocks) and returns the ones about our
	/// windows that the per-format editor code needs to know about (resizes
	/// in terms of the plugin's content size, close requests, destructions).
	/// Strip drawing, dragging and position memory are handled here directly.
	/// `Err` when the connection to the server is gone.
	pub fn events(&self) -> Result<Vec<(Window, WindowEvent)>, String> {
		let mut out = Vec::new();
		loop {
			let event = match self.conn.poll_for_event() {
				Ok(Some(e)) => e,
				Ok(None) => {
					let _ = self.conn.flush();
					return Ok(out);
				}
				Err(e) => return Err(e.to_string()),
			};
			match event {
				Event::ConfigureNotify(e) => {
					self.on_configure(e.window, e.x.into(), e.y.into(), e.width.into());
					if self.strips.borrow().contains_key(&e.window) {
						let (w, h) = geometry::content_size(e.width.into(), e.height.into());
						out.push((
							e.window,
							WindowEvent::Resized {
								width: w,
								height: h,
							},
						));
					}
				}
				Event::ClientMessage(e) if self.is_delete(&e) => {
					out.push((e.window, WindowEvent::CloseRequested));
				}
				Event::DestroyNotify(e) => {
					self.strips.borrow_mut().remove(&e.window);
					out.push((e.window, WindowEvent::Destroyed));
				}
				Event::Expose(e) if e.count == 0 => self.redraw(e.window),
				Event::ButtonPress(e) if e.detail == 1 => {
					let width = self.strips.borrow().get(&e.event).map(|s| s.width);
					if let Some(width) = width {
						let (x, y) = (i32::from(e.event_x), i32::from(e.event_y));
						if geometry::hit_close_button(width, x, y) {
							out.push((e.event, WindowEvent::CloseRequested));
						} else if geometry::in_strip(y) {
							self.begin_drag(e.event, (x, y), e.time);
						}
					}
				}
				Event::ButtonRelease(e) if e.detail == 1 => self.end_drag(e.event, e.time),
				Event::MotionNotify(e) => {
					let held = e.state.contains(KeyButMask::BUTTON1);
					self.on_motion(e.event, (e.event_x, e.event_y), (e.root_x, e.root_y), held)
				}
				_ => {}
			}
		}
	}

	fn on_motion(
		&self,
		window: Window,
		(event_x, event_y): (i16, i16),
		(root_x, root_y): (i16, i16),
		button_held: bool,
	) {
		let mut strips = self.strips.borrow_mut();
		let Some(s) = strips.get_mut(&window) else {
			return;
		};
		// A release we never saw (focus stolen, grab broken) must not leave the
		// window chasing the pointer.
		if !button_held && s.drag.is_some() {
			drop(strips);
			self.end_drag(window, x11rb::CURRENT_TIME);
			return;
		}
		if let Some(drag) = s.drag {
			let (nx, ny) = (
				i32::from(root_x) - drag.offset.0,
				i32::from(root_y) - drag.offset.1,
			);
			drop(strips);
			let _ = self
				.conn
				.configure_window(window, &ConfigureWindowAux::new().x(nx).y(ny));
			let _ = self.conn.flush();
			return;
		}
		let (x, y) = (i32::from(event_x), i32::from(event_y));
		let hover = geometry::in_strip(y) && geometry::hit_close_button(s.width, x, y);
		if hover != s.hover_close {
			s.hover_close = hover;
			drop(strips);
			self.redraw(window);
		}
	}

	fn is_delete(&self, e: &ClientMessageEvent) -> bool {
		e.type_ == self.atoms.wm_protocols
			&& e.format == 32
			&& e.data.as_data32()[0] == self.atoms.wm_delete_window
	}
}

/// Draws `text` with the built-in bitmap font (2x scale), starting at `x`
/// inside the strip, truncating whatever does not fit in `max_width` pixels.
fn draw_text(
	conn: &RustConnection,
	window: Window,
	gc: Gcontext,
	x: i16,
	text: &str,
	max_width: u32,
) {
	const SCALE: i16 = 2;
	let y = (geometry::STRIP_HEIGHT as i16 - font::GLYPH_HEIGHT as i16 * SCALE) / 2;
	let advance = font::GLYPH_WIDTH as i16 * SCALE;
	let right = x + max_width as i16;
	let mut rects = Vec::new();
	let mut cursor = x;
	for ch in text.chars() {
		if cursor + advance > right {
			break;
		}
		let bits = font::glyph(ch);
		for (row, byte) in bits.iter().enumerate() {
			for col in 0..8u8 {
				if byte & (1 << col) != 0 {
					rects.push(Rectangle {
						x: cursor + i16::from(col) * SCALE,
						y: y + row as i16 * SCALE,
						width: SCALE as u16,
						height: SCALE as u16,
					});
				}
			}
		}
		cursor += advance;
	}
	if !rects.is_empty() {
		let _ = conn.poly_fill_rectangle(window, gc, &rects);
	}
}

fn clamp_dim(v: u32) -> u16 {
	v.clamp(1, 16_384) as u16
}

fn clamp_coord(v: i32) -> i16 {
	v.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
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
