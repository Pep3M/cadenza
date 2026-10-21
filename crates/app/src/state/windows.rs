//! Floating windows over the workspace (FL Studio-like, T37): device editors, a track's
//! and a clip's properties. Several are open at once; the last one clicked has the
//! keyboard (and the device probe). Pure registry: open / focus / close / z-order,
//! cascade, clamping to the screen, and the rects remembered per item and per kind
//! (persisted in `Prefs`). The views draw each entry as an egui `Window`.

use cadenza_core::{ClipId, TrackId};
use cadenza_model::Project;
use serde::{Deserialize, Serialize};

use super::selection::DeviceRef;

/// Offset between cascaded windows.
pub const CASCADE: f32 = 28.0;
/// Rects remembered per item (per kind there is one each).
const MAX_ITEMS: usize = 64;
/// Default sizes (nothing remembered yet).
pub const DEVICE_SIZE: (f32, f32) = (1040.0, 440.0);
pub const TRACK_SIZE: (f32, f32) = (320.0, 560.0);
pub const CLIP_SIZE: (f32, f32) = (320.0, 600.0);
/// Part of a window that must stay on screen.
const MARGIN: f32 = 8.0;

/// What a floating window shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowItem {
	/// A built-in device's editor.
	Device(DeviceRef),
	/// A track's properties (`None` = the master channel).
	Track(Option<TrackId>),
	/// A clip's properties.
	Clip(ClipId),
}

impl WindowItem {
	/// Key of the rect remembered for this very item.
	pub fn id_key(&self) -> String {
		match self {
			WindowItem::Device(r) => format!("device:{}", r.device),
			WindowItem::Track(Some(t)) => format!("track:{t}"),
			WindowItem::Track(None) => "track:master".to_owned(),
			WindowItem::Clip(c) => format!("clip:{c}"),
		}
	}

	/// Key of the rect remembered for its kind (`device/eq`, `track`, `clip`).
	pub fn kind_key(&self, project: &Project) -> String {
		match self {
			WindowItem::Device(r) => {
				let id = super::samples::device(project, *r)
					.and_then(|d| d.builtin_id())
					.unwrap_or("device");
				format!("device/{id}")
			}
			WindowItem::Track(_) => "track".to_owned(),
			WindowItem::Clip(_) => "clip".to_owned(),
		}
	}

	/// Whether the item still exists in `project`.
	pub fn alive(&self, project: &Project) -> bool {
		match self {
			WindowItem::Device(r) => {
				super::samples::device(project, *r).is_some_and(|d| d.builtin_id().is_some())
			}
			WindowItem::Track(Some(t)) => project.track(*t).is_some(),
			WindowItem::Track(None) => true,
			WindowItem::Clip(c) => project.find_clip(*c).is_some(),
		}
	}

	pub fn device(&self) -> Option<DeviceRef> {
		match self {
			WindowItem::Device(r) => Some(*r),
			_ => None,
		}
	}
}

/// A window rect in screen points.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct WinRect {
	pub x: f32,
	pub y: f32,
	pub w: f32,
	pub h: f32,
}

impl WinRect {
	pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
		Self { x, y, w, h }
	}

	/// `size` centred in `screen`, a little above the middle.
	pub fn centered(screen: WinRect, w: f32, h: f32) -> Self {
		Self::new(
			screen.x + (screen.w - w) * 0.5,
			screen.y + (screen.h - h) * 0.5 - 40.0,
			w,
			h,
		)
	}

	/// Kept inside `screen`: no larger than it, and fully on it.
	pub fn clamped(self, screen: WinRect) -> Self {
		let w = self.w.min(screen.w - 2.0 * MARGIN).max(1.0);
		let h = self.h.min(screen.h - 2.0 * MARGIN).max(1.0);
		let max_x = (screen.x + screen.w - MARGIN - w).max(screen.x + MARGIN);
		let max_y = (screen.y + screen.h - MARGIN - h).max(screen.y + MARGIN);
		Self::new(
			self.x.clamp(screen.x + MARGIN, max_x),
			self.y.clamp(screen.y + MARGIN, max_y),
			w,
			h,
		)
	}

	/// Close enough to hide `other`'s title bar.
	fn near(&self, other: &WinRect) -> bool {
		(self.x - other.x).abs() < CASCADE * 0.75 && (self.y - other.y).abs() < CASCADE * 0.75
	}
}

/// Window rects remembered across sessions (in `Prefs`): per kind and per item.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedWindows {
	pub kinds: std::collections::BTreeMap<String, WinRect>,
	/// Most recently moved last; at most [`MAX_ITEMS`].
	pub items: Vec<(String, WinRect)>,
}

impl SavedWindows {
	pub fn item(&self, key: &str) -> Option<WinRect> {
		self.items.iter().find(|(k, _)| k == key).map(|(_, r)| *r)
	}

	pub fn remember(&mut self, kind: String, id: String, rect: WinRect) {
		self.kinds.insert(kind, rect);
		self.items.retain(|(k, _)| *k != id);
		self.items.push((id, rect));
		if self.items.len() > MAX_ITEMS {
			let extra = self.items.len() - MAX_ITEMS;
			self.items.drain(..extra);
		}
	}
}

/// One open window.
#[derive(Debug, Clone, PartialEq)]
pub struct FloatWindow {
	pub item: WindowItem,
	/// Where it opened (the view's default rect; egui keeps it from then on).
	pub placed: WinRect,
	/// Where it was in the last frame.
	pub rect: WinRect,
	/// Stays above the other windows.
	pub on_top: bool,
}

/// The open floating windows, bottom to top.
#[derive(Debug, Clone, PartialEq)]
pub struct Windows {
	pub open: Vec<FloatWindow>,
	/// The window with the keyboard (the last one clicked; `None` after a click
	/// elsewhere).
	pub focus: Option<WindowItem>,
	/// A window the view should bring to the front in the next frame.
	pub raise: Option<WindowItem>,
	pub saved: SavedWindows,
	/// The area windows live in (updated by the view every frame).
	pub screen: WinRect,
}

impl Default for Windows {
	fn default() -> Self {
		Self {
			open: Vec::new(),
			focus: None,
			raise: None,
			saved: SavedWindows::default(),
			screen: WinRect::new(0.0, 0.0, 1600.0, 1000.0),
		}
	}
}

impl Windows {
	pub fn is_open(&self, item: WindowItem) -> bool {
		self.open.iter().any(|w| w.item == item)
	}

	pub fn get(&self, item: WindowItem) -> Option<&FloatWindow> {
		self.open.iter().find(|w| w.item == item)
	}

	pub fn get_mut(&mut self, item: WindowItem) -> Option<&mut FloatWindow> {
		self.open.iter_mut().find(|w| w.item == item)
	}

	/// Opens `item` (`size` when nothing is remembered for it) or, when it is already
	/// open, brings it to the front. Returns whether a new window opened.
	pub fn open(&mut self, item: WindowItem, kind: &str, size: (f32, f32)) -> bool {
		if self.is_open(item) {
			self.focus(item);
			return false;
		}
		let screen = self.screen;
		let mut rect = self
			.saved
			.item(&item.id_key())
			.or_else(|| self.saved.kinds.get(kind).copied())
			.unwrap_or_else(|| WinRect::centered(screen, size.0, size.1))
			.clamped(screen);
		// Cascade from a window already sitting there.
		for _ in 0..16 {
			if !self.open.iter().any(|w| w.rect.near(&rect)) {
				break;
			}
			let next = WinRect {
				x: rect.x + CASCADE,
				y: rect.y + CASCADE,
				..rect
			};
			let clamped = next.clamped(screen);
			rect = if clamped.near(&rect) {
				// Stuck at the bottom-right corner: restart at the top-left.
				WinRect {
					x: screen.x + MARGIN,
					y: screen.y + MARGIN,
					..rect
				}
				.clamped(screen)
			} else {
				clamped
			};
		}
		self.open.push(FloatWindow {
			item,
			placed: rect,
			rect,
			on_top: false,
		});
		self.focus = Some(item);
		self.raise = Some(item);
		true
	}

	/// Gives `item` the keyboard and puts it on top.
	pub fn focus(&mut self, item: WindowItem) {
		if let Some(i) = self.open.iter().position(|w| w.item == item) {
			let w = self.open.remove(i);
			self.open.push(w);
			self.focus = Some(item);
			self.raise = Some(item);
		}
	}

	/// A click landed in `item` (egui already raised it).
	pub fn clicked(&mut self, item: WindowItem) {
		if let Some(i) = self.open.iter().position(|w| w.item == item) {
			let w = self.open.remove(i);
			self.open.push(w);
			self.focus = Some(item);
		}
	}

	/// A click outside every window: the workspace has the keyboard again.
	pub fn blur(&mut self) {
		self.focus = None;
	}

	/// Closes `item`; the keyboard stays with the focused window, if another.
	pub fn close(&mut self, item: WindowItem) -> bool {
		let before = self.open.len();
		self.open.retain(|w| w.item != item);
		if self.focus == Some(item) {
			self.focus = None;
		}
		if self.raise == Some(item) {
			self.raise = None;
		}
		before != self.open.len()
	}

	/// Esc: closes the focused window.
	pub fn close_focused(&mut self) -> Option<WindowItem> {
		let item = self.focus?;
		self.close(item);
		Some(item)
	}

	/// The topmost open device window.
	pub fn top_device(&self) -> Option<DeviceRef> {
		self.open.iter().rev().find_map(|w| w.item.device())
	}

	/// The focused window's device.
	pub fn focused_device(&self) -> Option<DeviceRef> {
		self.focus.and_then(|i| i.device())
	}

	/// The view reports where `item` is (shown, moved or resized): remembered per item
	/// and per kind for the next time.
	pub fn moved(&mut self, item: WindowItem, kind: String, rect: WinRect) {
		let Some(w) = self.get_mut(item) else {
			return;
		};
		w.rect = rect;
		let id = item.id_key();
		if self.saved.item(&id) != Some(rect) {
			self.saved.remember(kind, id, rect);
		}
	}

	/// Closes the windows whose item is gone (after an edit or undo).
	pub fn prune(&mut self, project: &Project) {
		let gone: Vec<WindowItem> = self
			.open
			.iter()
			.map(|w| w.item)
			.filter(|i| !i.alive(project))
			.collect();
		for item in gone {
			self.close(item);
		}
	}

	/// Closes everything (new / open project).
	pub fn clear(&mut self) {
		self.open.clear();
		self.focus = None;
		self.raise = None;
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use cadenza_core::DeviceId;

	fn dev() -> WindowItem {
		WindowItem::Device(DeviceRef {
			track: Some(TrackId::new()),
			device: DeviceId::new(),
		})
	}

	#[test]
	fn opening_twice_focuses_and_new_windows_cascade() {
		let mut w = Windows::default();
		let (a, b) = (dev(), dev());
		assert!(w.open(a, "device/eq", (800.0, 400.0)));
		assert!(w.open(b, "device/eq", (800.0, 400.0)));
		assert_eq!(w.open.len(), 2);
		let (ra, rb) = (w.open[0].rect, w.open[1].rect);
		assert_eq!((rb.x - ra.x, rb.y - ra.y), (CASCADE, CASCADE));
		assert_eq!(w.focus, Some(b));
		// Opening `a` again focuses it (on top), no duplicate.
		assert!(!w.open(a, "device/eq", (800.0, 400.0)));
		assert_eq!(w.open.len(), 2);
		assert_eq!(w.open[1].item, a);
		assert_eq!((w.focus, w.raise), (Some(a), Some(a)));
		assert_eq!(w.top_device(), a.device());
	}

	#[test]
	fn closing_and_esc_and_blur() {
		let mut w = Windows::default();
		let (a, t) = (dev(), WindowItem::Track(None));
		w.open(a, "device/eq", (800.0, 400.0));
		w.open(t, "track", (300.0, 500.0));
		assert_eq!(w.close_focused(), Some(t));
		assert_eq!(w.focus, None);
		assert_eq!(w.close_focused(), None);
		w.clicked(a);
		assert_eq!(w.focused_device(), a.device());
		w.blur();
		assert_eq!(w.focused_device(), None);
		assert!(w.close(a) && !w.close(a));
		assert!(w.open.is_empty());
	}

	#[test]
	fn rects_are_remembered_per_item_then_per_kind_and_clamped() {
		let mut w = Windows::default();
		let a = dev();
		w.open(a, "device/eq", (800.0, 400.0));
		let moved = WinRect::new(100.0, 120.0, 700.0, 380.0);
		w.moved(a, "device/eq".into(), moved);
		w.close(a);
		// The same item opens where it was.
		w.open(a, "device/eq", (800.0, 400.0));
		assert_eq!(w.get(a).unwrap().rect, moved);
		// Another EQ uses the kind's rect (cascaded off the open one).
		let b = dev();
		w.open(b, "device/eq", (800.0, 400.0));
		let rb = w.get(b).unwrap().rect;
		assert_eq!((rb.x, rb.y, rb.w), (128.0, 148.0, 700.0));
		// Off screen or too large: pulled back inside.
		let c = WindowItem::Clip(ClipId::new());
		w.saved
			.kinds
			.insert("clip".into(), WinRect::new(3000.0, -50.0, 2000.0, 300.0));
		w.open(c, "clip", (300.0, 300.0));
		let rc = w.get(c).unwrap().rect;
		assert_eq!(rc, WinRect::new(8.0, 8.0, 1584.0, 300.0));
		// Serde round trip (Prefs).
		let json = serde_json::to_string(&w.saved).unwrap();
		let back: SavedWindows = serde_json::from_str(&json).unwrap();
		assert_eq!(back, w.saved);
		assert_eq!(back.item(&a.id_key()), Some(moved));
	}

	#[test]
	fn the_item_memory_is_capped() {
		let mut s = SavedWindows::default();
		for i in 0..(MAX_ITEMS + 5) {
			s.remember(
				"clip".into(),
				format!("clip:{i}"),
				WinRect::new(0.0, 0.0, 1.0, 1.0),
			);
		}
		assert_eq!(s.items.len(), MAX_ITEMS);
		assert_eq!(s.items[0].0, "clip:5");
	}
}
