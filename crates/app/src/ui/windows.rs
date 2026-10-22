//! Floating windows over the workspace (FL Studio-like, T37): each open device editor,
//! track and clip properties window of `state::windows` drawn as an egui `Window` with
//! the Graphite frame (shadow, 28 px title bar: name · where, Dock for devices, Keep on
//! top, ×). Drag the bar to move, any edge to resize, a click brings one to the front
//! and gives it the keyboard; positions and sizes are reported back to the registry,
//! which remembers them in `Prefs`.

use crate::i18n::{tr, trf};
use std::collections::HashMap;

use cadenza_core::DeviceId;
use egui::{
	Align, Align2, CornerRadius, Id, LayerId, Layout, Order, Rect, Sense, Stroke, UiBuilder, pos2,
	vec2,
};

use crate::state::AppState;
use crate::state::dnd::DropTarget;
use crate::state::windows::{WinRect, WindowItem};
use crate::theme;
use crate::ui::device_editor::{self, DeviceEditorUi};

const BAR_H: f32 = 28.0;

/// View state of the floating windows: one device editor per open device window.
#[derive(Default)]
pub struct FloatingUi {
	devices: HashMap<DeviceId, DeviceEditorUi>,
	/// Every window's rect in the last frame, bottom to top (headless tests).
	pub rects: Vec<(WindowItem, Rect)>,
	/// The pointer press of this frame landed in a window (panel focus stays).
	pub clicked_inside: bool,
}

fn to_rect(r: WinRect) -> Rect {
	Rect::from_min_size(pos2(r.x, r.y), vec2(r.w, r.h))
}

fn window_id(item: WindowItem) -> Id {
	Id::new(("float-window", item.id_key()))
}

impl FloatingUi {
	/// The editor of an open device window.
	pub fn device_ui(&self, device: DeviceId) -> Option<&DeviceEditorUi> {
		self.devices.get(&device)
	}

	/// The rect of `item`'s window in the last frame.
	pub fn rect(&self, item: WindowItem) -> Option<Rect> {
		self.rects.iter().find(|(i, _)| *i == item).map(|(_, r)| *r)
	}

	/// The drop target under `pos` (sampler pads / zones of the topmost window there).
	pub fn target_at(&self, pos: egui::Pos2) -> Option<DropTarget> {
		let (item, _) = self.rects.iter().rev().find(|(_, r)| r.contains(pos))?;
		self.devices.get(&item.device()?.device)?.target_at(pos)
	}

	/// Keys of the focused window: Esc closes it; a device window gets its editor's
	/// keys (A/B, synth keys, EQ bands...). Returns whether a window has the keyboard.
	pub fn shortcuts(&mut self, ctx: &egui::Context, state: &mut AppState) -> bool {
		let Some(item) = state.view.windows.focus else {
			return false;
		};
		if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
			if let Some(r) = item.device()
				&& let Some(ui) = self.devices.get_mut(&r.device)
			{
				ui.release(state);
			}
			state.close_window(item);
			return true;
		}
		if let Some(r) = item.device()
			&& let Some(ui) = self.devices.get_mut(&r.device)
		{
			ui.shortcuts(ctx, state, r);
		}
		true
	}

	/// Draws every open window (bottom to top) and tracks which one was clicked.
	pub fn show(&mut self, ctx: &egui::Context, state: &mut AppState, locked: bool) {
		let screen = ctx.content_rect();
		state.view.windows.screen =
			WinRect::new(screen.left(), screen.top(), screen.width(), screen.height());
		// Editors of closed windows let go of their notes.
		let open: Vec<DeviceId> = state
			.view
			.windows
			.open
			.iter()
			.filter_map(|w| w.item.device().map(|r| r.device))
			.collect();
		let closed: Vec<DeviceId> = self
			.devices
			.keys()
			.filter(|d| !open.contains(d))
			.copied()
			.collect();
		for d in closed {
			if let Some(mut ui) = self.devices.remove(&d) {
				ui.release(state);
			}
		}
		self.rects.clear();
		let mut layers = Vec::new();
		let windows = state.view.windows.open.clone();
		for w in windows {
			let item = w.item;
			let id = window_id(item);
			let order = if w.on_top {
				Order::Foreground
			} else {
				Order::Middle
			};
			let Some((title, place)) = titles(state, item) else {
				continue;
			};
			let frame = egui::Frame::new()
				.fill(theme::BG_PANEL)
				.stroke(Stroke::new(1.0, theme::BORDER_STRONG))
				.corner_radius(CornerRadius::same(theme::RADIUS_XL))
				.shadow(theme::FLOATING_SHADOW);
			// A shrinking main window shrinks and moves the floating ones into it.
			let max = (screen.size() - vec2(16.0, 16.0)).max(vec2(120.0, 80.0));
			let min = if item.device().is_some() {
				vec2(360.0, 220.0)
			} else {
				vec2(260.0, 220.0)
			}
			.min(max);
			let shown = egui::Window::new(format!("{title} · {place}"))
				.id(id)
				.title_bar(false)
				.collapsible(false)
				.resizable(true)
				.scroll(false)
				.default_rect(to_rect(w.placed))
				.min_size(min)
				.max_size(max)
				.order(order)
				.frame(frame)
				.constrain_to(screen)
				.show(ctx, |ui| {
					if locked {
						ui.disable();
					}
					let full = ui.available_rect_before_wrap();
					ui.allocate_rect(full, Sense::hover());
					let bar = Rect::from_min_size(full.min, vec2(full.width(), BAR_H));
					let body = Rect::from_min_max(pos2(full.left(), bar.bottom()), full.max);
					// Only the title bar moves the window, not a body's empty display.
					ui.interact(body, id.with("body"), Sense::drag());
					self.bar(ui, state, item, w.on_top, bar, &title, &place);
					match item {
						WindowItem::Device(r) => {
							let mut inner = ui.new_child(UiBuilder::new().max_rect(body));
							self.devices
								.entry(r.device)
								.or_default()
								.show_window(&mut inner, state, r);
						}
						_ => {
							let inner_rect = body.shrink2(vec2(10.0, 8.0));
							let mut inner = ui.new_child(
								UiBuilder::new()
									.max_rect(inner_rect)
									.layout(Layout::top_down(Align::Min)),
							);
							inner.set_clip_rect(inner_rect.intersect(ui.clip_rect()));
							crate::ui::inspector::item_window(&mut inner, state, item);
						}
					}
				});
			let Some(shown) = shown else {
				continue;
			};
			let rect = shown.response.rect;
			self.rects.push((item, rect));
			layers.push((shown.response.layer_id, item));
			let now = WinRect::new(rect.left(), rect.top(), rect.width(), rect.height());
			let saved = state.view.windows.saved.item(&item.id_key());
			if saved != Some(now) {
				let kind = item.kind_key(state.project());
				state.view.windows.moved(item, kind, now);
			}
			if state.view.windows.raise == Some(item) {
				state.view.windows.raise = None;
			}
			// Bottom to top every frame: windows stay above the side overlays (design 12).
			ctx.move_to_top(LayerId::new(order, id));
		}
		self.clicked_inside = false;
		if ctx.input(|i| i.pointer.any_pressed())
			&& let Some(pos) = ctx.pointer_interact_pos()
		{
			let layer = ctx.layer_id_at(pos);
			let hit = layer.and_then(|l| layers.iter().find(|(id, _)| *id == l).map(|(_, i)| *i));
			match hit {
				Some(item) => {
					self.clicked_inside = true;
					if state.view.windows.focus != Some(item) {
						state.view.windows.clicked(item);
						state.sync_editor_probe();
					}
				}
				// Panels: the workspace takes the keyboard back. A press in a popup (a
				// window's menu) keeps it.
				None if layer.is_none_or(|l| l.order == Order::Background) => {
					if state.view.windows.focus.is_some() {
						state.view.windows.blur();
						state.sync_editor_probe();
					}
				}
				None => self.clicked_inside = true,
			}
		}
	}

	/// The title bar: "<Name> · <where>", then Dock (devices), Keep on top and ×.
	#[allow(clippy::too_many_arguments)]
	fn bar(
		&mut self,
		ui: &mut egui::Ui,
		state: &mut AppState,
		item: WindowItem,
		on_top: bool,
		bar: Rect,
		name: &str,
		place: &str,
	) {
		let radius = theme::RADIUS_XL.saturating_sub(1);
		let focused = state.view.windows.focus == Some(item);
		ui.painter().rect_filled(
			bar,
			CornerRadius {
				nw: radius,
				ne: radius,
				..CornerRadius::ZERO
			},
			if focused {
				theme::BG_RAISED
			} else {
				theme::BG_PANEL
			},
		);
		ui.painter().hline(
			bar.x_range(),
			bar.bottom() - 0.5,
			Stroke::new(1.0, theme::BORDER),
		);
		// Room for the buttons on the right.
		let buttons = if item.device().is_some() {
			170.0
		} else {
			120.0
		};
		let room = (bar.width() - 20.0 - buttons).max(40.0);
		let p = ui.painter().with_clip_rect(Rect::from_min_size(
			bar.min,
			vec2(10.0 + room, bar.height()),
		));
		let color = if focused {
			theme::TEXT
		} else {
			theme::TEXT_DIM
		};
		let g = p.layout_no_wrap(name.to_owned(), theme::semibold(12.5), color);
		let w = g.size().x;
		let cy = bar.center().y;
		p.galley(pos2(bar.left() + 10.0, cy - g.size().y * 0.5), g, color);
		p.text(
			pos2(bar.left() + 10.0 + w + 6.0, cy),
			Align2::LEFT_CENTER,
			format!("· {place}"),
			theme::medium(12.0),
			theme::TEXT_FAINT,
		);
		let mut row = device_editor::row(ui, bar.shrink2(vec2(6.0, 0.0)));
		row.with_layout(Layout::right_to_left(Align::Center), |ui| {
			ui.spacing_mut().item_spacing.x = 4.0;
			if device_editor::close_button_labeled(ui, 22.0, tr("Close window"), tr("Close (Esc)"))
				.clicked()
			{
				if let Some(r) = item.device()
					&& let Some(e) = self.devices.get_mut(&r.device)
				{
					e.release(state);
				}
				state.close_window(item);
			}
			let hint = tr("Keep this window above the other windows");
			if device_editor::small_button(ui, tr("Keep on top"), Some(on_top), hint).clicked()
				&& let Some(w) = state.view.windows.get_mut(item)
			{
				w.on_top = !on_top;
			}
			if let Some(r) = item.device() {
				let hint = tr("Put the editor into the bottom dock");
				if device_editor::small_button(ui, tr("Dock"), None, hint).clicked() {
					if let Some(e) = self.devices.get_mut(&r.device) {
						e.release(state);
					}
					state.dock_device_window(r);
				}
			}
		});
	}
}

/// The title and the "where" of `item` (None: gone).
fn titles(state: &AppState, item: WindowItem) -> Option<(String, String)> {
	let p = state.project();
	match item {
		WindowItem::Device(r) => {
			let d = state.device(r)?;
			Some((d.name.clone(), device_editor::channel_name(state, r)))
		}
		WindowItem::Track(Some(t)) => Some((p.track(t)?.name.clone(), tr("Track").to_owned())),
		WindowItem::Track(None) => Some((tr("Master").to_owned(), tr("Channel").to_owned())),
		WindowItem::Clip(c) => {
			let (t, clip) = p.find_clip(c)?;
			let copies = cadenza_model::links::link_count(p, c);
			let linked = if copies > 1 {
				trf(" · linked ×{copies}", &[("copies", &copies)])
			} else {
				String::new()
			};
			Some((
				clip.name.clone(),
				trf(
					"Clip on {name}{linked}",
					&[("name", &t.name), ("linked", &linked)],
				),
			))
		}
	}
}
