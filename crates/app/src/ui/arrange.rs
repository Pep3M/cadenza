//! Arrange view: ruler, markers, track headers and clip lanes.
//!
//! Gestures are previewed locally (ghost clips, insertion line) and become one command
//! through [`AppState::apply`] when the mouse is released.

use cadenza_core::{ClipId, PPQ, Tick, TrackId};
use cadenza_model::{
	ChannelPatch, Clip, ClipContent, Command, Marker, Project, Track, TrackKind, TrackPatch,
};
use egui::{
	Align2, Color32, CornerRadius, CursorIcon, FontId, Id, Painter, Rect, Sense, Shape, Stroke,
	StrokeKind, Ui, pos2, vec2,
};

use crate::jobs::Peaks;
use crate::state::edits::{self, TRACK_PALETTE};
use crate::state::selection::DeviceRef;
use crate::state::timeline::Timeline;
use crate::state::{AppState, MAX_TRACK_HEIGHT, MIN_TRACK_HEIGHT};
use crate::theme;
use crate::ui::browser::DraggedDevice;
use crate::ui::widgets;

/// Width of the clip edge that resizes instead of moving.
const RESIZE_HANDLE: f32 = 7.0;
const CLIP_HEADER: f32 = 14.0;

#[derive(Debug, Clone, Copy, PartialEq)]
enum DragKind {
	Move,
	Resize,
}

#[derive(Debug, Clone)]
struct ClipDrag {
	kind: DragKind,
	anchor: ClipId,
	clips: Vec<ClipId>,
	/// Anchor start (move) or end (resize) when the drag began.
	origin: Tick,
	press_x: f32,
	/// Snapped offset of the gesture.
	delta: Tick,
	/// Track under the pointer when it differs from the anchor's.
	target: Option<TrackId>,
}

#[derive(Debug, Clone)]
struct TrackDrag {
	track: TrackId,
	from: usize,
	to: usize,
}

/// View-local state of the arrange view.
#[derive(Default)]
pub struct ArrangeUi {
	drag: Option<ClipDrag>,
	track_drag: Option<TrackDrag>,
	rename: Option<(TrackId, String)>,
	/// Width of the lanes area in the last frame (for "zoom to fit" and following).
	pub lanes_width: f32,
}

/// Geometry shared by the painting helpers.
struct Layout {
	header: Rect,
	lanes: Rect,
	ruler: Rect,
	markers: Rect,
	body_top: f32,
}

impl Layout {
	fn track_top(&self, index: usize, height: f32, scroll: f32) -> f32 {
		self.body_top + index as f32 * height - scroll
	}
}

impl ArrangeUi {
	pub fn show(&mut self, ui: &mut Ui, state: &mut AppState) {
		let full = ui.available_rect_before_wrap();
		ui.allocate_rect(full, Sense::hover());
		let top = full.top() + theme::RULER_HEIGHT + theme::MARKER_LANE_HEIGHT;
		let layout = Layout {
			header: Rect::from_min_max(
				pos2(full.left(), top),
				pos2(full.left() + theme::HEADER_WIDTH, full.bottom()),
			),
			lanes: Rect::from_min_max(pos2(full.left() + theme::HEADER_WIDTH, top), full.max),
			ruler: Rect::from_min_max(
				pos2(full.left() + theme::HEADER_WIDTH, full.top()),
				pos2(full.right(), full.top() + theme::RULER_HEIGHT),
			),
			markers: Rect::from_min_max(
				pos2(
					full.left() + theme::HEADER_WIDTH,
					full.top() + theme::RULER_HEIGHT,
				),
				pos2(full.right(), top),
			),
			body_top: top,
		};
		self.lanes_width = layout.lanes.width();
		self.handle_wheel(ui, state, &layout, full);

		let playing = state.is_playing();
		let position = state.position();
		if playing && state.view.follow_playhead {
			state
				.view
				.timeline
				.follow(position.beats(), layout.lanes.width());
		}

		let painter = ui.painter_at(full);
		painter.rect_filled(full, 0.0, theme::LANE);

		self.lanes(ui, state, &layout);
		self.headers(ui, state, &layout);
		self.corner(ui, state, full);
		ruler(ui, state, &layout);
		markers(ui, state, &layout);

		// Playhead over everything in the timeline.
		let x = layout.lanes.left() + state.view.timeline.tick_to_x(position);
		if (layout.lanes.left()..=layout.lanes.right()).contains(&x) {
			let p = ui.painter_at(Rect::from_min_max(
				pos2(layout.lanes.left(), full.top()),
				full.max,
			));
			p.vline(
				x,
				layout.ruler.top()..=full.bottom(),
				Stroke::new(1.5, theme::PLAYHEAD),
			);
			let tip = layout.ruler.bottom();
			p.add(Shape::convex_polygon(
				vec![
					pos2(x - 6.0, tip - 9.0),
					pos2(x + 6.0, tip - 9.0),
					pos2(x, tip),
				],
				theme::PLAYHEAD,
				Stroke::NONE,
			));
		}
	}

	/// Ctrl+wheel: zoom at the pointer; Shift+wheel: horizontal scroll; Alt+wheel: track
	/// height; wheel: scroll tracks.
	fn handle_wheel(&self, ui: &Ui, state: &mut AppState, layout: &Layout, full: Rect) {
		let Some(pointer) = ui.ctx().pointer_hover_pos() else {
			return;
		};
		if !full.contains(pointer) || !ui.ui_contains_pointer() {
			return;
		}
		let (zoom, scroll, alt) =
			ui.input(|i| (i.zoom_delta(), i.smooth_scroll_delta, i.modifiers.alt));
		let view = &mut state.view;
		if zoom != 1.0 {
			view.timeline
				.zoom_around(zoom, (pointer.x - layout.lanes.left()).max(0.0));
		}
		if alt && scroll.y != 0.0 {
			view.track_height =
				(view.track_height + scroll.y * 0.25).clamp(MIN_TRACK_HEIGHT, MAX_TRACK_HEIGHT);
			return;
		}
		if scroll.x != 0.0 {
			view.timeline.scroll_px(-scroll.x);
		}
		if scroll.y != 0.0 {
			let content =
				state.session.document().project().tracks.len() as f32 * view.track_height;
			let max = (content - layout.lanes.height() + 40.0).max(0.0);
			view.scroll_y = (view.scroll_y - scroll.y).clamp(0.0, max);
		}
	}

	/// Top-left corner above the headers: snap and zoom controls.
	fn corner(&self, ui: &mut Ui, state: &mut AppState, full: Rect) {
		let rect = Rect::from_min_size(
			full.min,
			vec2(
				theme::HEADER_WIDTH,
				theme::RULER_HEIGHT + theme::MARKER_LANE_HEIGHT,
			),
		);
		ui.painter().rect_filled(rect, 0.0, theme::BG_PANEL);
		ui.painter().hline(
			rect.x_range(),
			rect.bottom() - 0.5,
			Stroke::new(1.0, theme::BORDER),
		);
		let grid = state.view.timeline.grid(state.project().time_signature);
		let mut child = ui.new_child(
			egui::UiBuilder::new()
				.max_rect(rect.shrink2(vec2(6.0, 4.0)))
				.layout(egui::Layout::left_to_right(egui::Align::Center)),
		);
		let snap = state.view.snap;
		if child
			.selectable_label(snap, "Snap")
			.on_hover_text("Snap clips to the grid")
			.clicked()
		{
			state.view.snap = !snap;
		}
		child.label(
			egui::RichText::new(grid_name(grid.step, grid.bar))
				.font(theme::small())
				.color(theme::TEXT_DIM),
		);
		child.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
			if ui
				.small_button("Fit")
				.on_hover_text("Zoom to the whole song")
				.clicked()
			{
				let end = state.project().end_tick().beats().max(16.0) + 4.0;
				state.view.timeline.fit(end, self.lanes_width);
			}
			if ui
				.small_button("+")
				.on_hover_text("Zoom in (Ctrl+wheel)")
				.clicked()
			{
				state.view.timeline.zoom_around(1.5, 0.0);
			}
			if ui.small_button("−").on_hover_text("Zoom out").clicked() {
				state.view.timeline.zoom_around(1.0 / 1.5, 0.0);
			}
		});
	}

	fn headers(&mut self, ui: &mut Ui, state: &mut AppState, layout: &Layout) {
		let h = state.view.track_height;
		let scroll = state.view.scroll_y;
		let clip = layout.header;
		ui.painter_at(clip).rect_filled(clip, 0.0, theme::BG_PANEL);
		let tracks: Vec<Track> = state.project().tracks.clone();
		for (i, track) in tracks.iter().enumerate() {
			let y = layout.track_top(i, h, scroll);
			let rect = Rect::from_min_size(pos2(clip.left(), y), vec2(clip.width(), h));
			if rect.bottom() < clip.top() || rect.top() > clip.bottom() {
				continue;
			}
			let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect));
			child.set_clip_rect(rect.intersect(clip));
			self.header(&mut child, state, track, i, rect);
		}
		// Reorder feedback.
		if let Some(d) = &self.track_drag
			&& d.to != d.from
		{
			let row = if d.to > d.from { d.to + 1 } else { d.to };
			let y = layout.track_top(row, h, scroll);
			ui.painter_at(clip)
				.hline(clip.x_range(), y, Stroke::new(2.0, theme::ACCENT));
		}
		// Add-track buttons under the last track.
		let below = layout.track_top(tracks.len(), h, scroll) + 6.0;
		if below + 24.0 < clip.bottom() {
			let rect = Rect::from_min_size(
				pos2(clip.left() + 8.0, below),
				vec2(clip.width() - 16.0, 22.0),
			);
			let mut child = ui.new_child(
				egui::UiBuilder::new()
					.max_rect(rect)
					.layout(egui::Layout::left_to_right(egui::Align::Center)),
			);
			child.set_clip_rect(clip);
			add_track_buttons(&mut child, state);
		}
		ui.painter().vline(
			clip.right() - 0.5,
			clip.y_range(),
			Stroke::new(1.0, theme::BORDER_STRONG),
		);
	}

	fn header(
		&mut self,
		ui: &mut Ui,
		state: &mut AppState,
		track: &Track,
		index: usize,
		rect: Rect,
	) {
		let selected = state.selection.track == Some(track.id);
		let color = theme::track_color(track.color.as_deref());
		let id = Id::new(("track-header", track.id));
		let response = ui.interact(rect, id, Sense::click_and_drag());
		let p = ui.painter().clone();
		p.rect_filled(
			rect,
			0.0,
			if selected {
				theme::LANE_SELECTED
			} else {
				theme::BG_PANEL
			},
		);
		p.rect_filled(
			Rect::from_min_size(rect.min, vec2(5.0, rect.height())),
			0.0,
			color,
		);
		p.hline(
			rect.x_range(),
			rect.bottom() - 0.5,
			Stroke::new(1.0, theme::BORDER),
		);

		// Drop a device from the browser onto the track.
		if let Some(dev) = response.dnd_release_payload::<DraggedDevice>() {
			state.use_device(&dev.0, Some(track.id));
		} else if response.dnd_hover_payload::<DraggedDevice>().is_some() {
			p.rect_stroke(
				rect.shrink(1.0),
				0.0,
				Stroke::new(1.5, theme::ACCENT),
				StrokeKind::Inside,
			);
		}

		let left = rect.left() + 12.0;
		let meter_w = 8.0;
		let right = rect.right() - meter_w - 10.0;
		// Row 1: number, name, mute, solo.
		let row1 = rect.top() + 6.0;
		p.text(
			pos2(left, row1 + 8.0),
			Align2::LEFT_CENTER,
			format!("{}", index + 1),
			theme::small(),
			theme::TEXT_FAINT,
		);
		let solo_rect = Rect::from_min_size(pos2(right - 18.0, row1), vec2(18.0, 16.0));
		let mute_rect = solo_rect.translate(vec2(-21.0, 0.0));
		let name_rect = Rect::from_min_max(
			pos2(left + 16.0, row1),
			pos2(mute_rect.left() - 4.0, row1 + 16.0),
		);
		match &mut self.rename {
			Some((t, text)) if *t == track.id => {
				let edit = ui.put(
					name_rect,
					egui::TextEdit::singleline(text).font(FontId::proportional(12.5)),
				);
				if !edit.has_focus() && !edit.lost_focus() {
					edit.request_focus();
				}
				if edit.lost_focus() {
					let name = text.trim().to_owned();
					self.rename = None;
					let escaped = ui.input(|i| i.key_pressed(egui::Key::Escape));
					if !escaped && !name.is_empty() && name != track.name {
						state.apply(Command::UpdateTrack {
							track: track.id,
							patch: TrackPatch {
								name: Some(name),
								color: None,
							},
						});
					}
				}
			}
			_ => {
				let name = ui.painter_at(name_rect);
				name.text(
					pos2(name_rect.left(), name_rect.center().y),
					Align2::LEFT_CENTER,
					&track.name,
					FontId::proportional(12.5),
					if selected {
						Color32::WHITE
					} else {
						theme::TEXT
					},
				);
			}
		}
		let ch = &track.channel;
		{
			let m = ui.interact(mute_rect, id.with("mute"), Sense::click());
			widgets::paint_toggle(&p, mute_rect, ch.mute, "M", theme::MUTE, m.hovered());
			if m.on_hover_text("Mute").clicked() {
				set_channel(
					state,
					track.id,
					ChannelPatch {
						mute: Some(!ch.mute),
						..ChannelPatch::default()
					},
				);
			}
			let s = ui.interact(solo_rect, id.with("solo"), Sense::click());
			widgets::paint_toggle(&p, solo_rect, ch.solo, "S", theme::SOLO, s.hovered());
			if s.on_hover_text("Solo").clicked() {
				set_channel(
					state,
					track.id,
					ChannelPatch {
						solo: Some(!ch.solo),
						..ChannelPatch::default()
					},
				);
			}
		}

		// Row 2: instrument / kind and volume.
		if rect.height() >= 46.0 {
			let row2 = row1 + 22.0;
			let what = match track.kind {
				TrackKind::Instrument => track
					.instrument
					.as_ref()
					.map_or("No instrument".to_owned(), |d| d.name.clone()),
				TrackKind::Audio => "Audio".to_owned(),
				TrackKind::Bus => "Bus".to_owned(),
			};
			let instrument_rect =
				Rect::from_min_max(pos2(left, row2), pos2(right - 62.0, row2 + 14.0));
			let ir = ui.interact(instrument_rect, id.with("instrument"), Sense::click());
			ui.painter_at(instrument_rect).text(
				pos2(left, row2 + 7.0),
				Align2::LEFT_CENTER,
				format!("{} {what}", kind_glyph(track.kind)),
				theme::small(),
				if ir.hovered() {
					theme::TEXT
				} else {
					theme::TEXT_DIM
				},
			);
			if ir.clicked()
				&& let Some(d) = &track.instrument
			{
				state.selection.select_device(DeviceRef {
					track: Some(track.id),
					device: d.id,
				});
			}
			let vol_rect = Rect::from_min_size(pos2(right - 58.0, row2), vec2(58.0, 14.0));
			widgets::volume_bar(&p, vol_rect, state.volume_db(Some(track.id)), color);
		}
		// Meter along the right edge.
		let meter = Rect::from_min_max(
			pos2(rect.right() - meter_w - 4.0, rect.top() + 4.0),
			pos2(rect.right() - 4.0, rect.bottom() - 4.0),
		);
		widgets::paint_meter(&p, meter, state.meters.get(Some(track.id)));

		// Click selects, double-click renames, drag reorders.
		if response.clicked() {
			state.selection.select_track(track.id);
			state.selection.clear_clips();
		}
		if response.double_clicked() {
			self.rename = Some((track.id, track.name.clone()));
		}
		if response.drag_started() {
			self.track_drag = Some(TrackDrag {
				track: track.id,
				from: index,
				to: index,
			});
		}
		if let Some(d) = &mut self.track_drag
			&& d.track == track.id
		{
			if let Some(pos) = response.interact_pointer_pos() {
				let n = state.project().tracks.len();
				let row = ((pos.y - rect.top()) / rect.height()).floor() as i64 + index as i64;
				d.to = row.clamp(0, n as i64 - 1) as usize;
			}
			if response.drag_stopped() {
				let d = self.track_drag.take().expect("track drag");
				if d.to != d.from {
					state.apply(Command::MoveTrack {
						track: d.track,
						index: d.to,
					});
				}
			}
		}
		response.context_menu(|ui| track_menu(ui, state, track, &mut self.rename));
	}

	fn lanes(&mut self, ui: &mut Ui, state: &mut AppState, layout: &Layout) {
		let area = layout.lanes;
		let h = state.view.track_height;
		let scroll = state.view.scroll_y;
		let tl = state.view.timeline;
		let ts = state.project().time_signature;
		let painter = ui.painter_at(area);
		let tracks: Vec<Track> = state.project().tracks.clone();

		// Lane backgrounds.
		for (i, t) in tracks.iter().enumerate() {
			let y = layout.track_top(i, h, scroll);
			let r = Rect::from_min_size(pos2(area.left(), y), vec2(area.width(), h));
			if !r.intersects(area) {
				continue;
			}
			let selected = state.selection.track == Some(t.id);
			painter.rect_filled(
				r,
				0.0,
				if selected {
					theme::LANE_SELECTED
				} else if i % 2 == 0 {
					theme::LANE
				} else {
					theme::LANE_ALT
				},
			);
		}
		grid(&painter, area, &tl, ts);
		for i in 0..=tracks.len() {
			let y = layout.track_top(i, h, scroll);
			painter.hline(area.x_range(), y - 0.5, Stroke::new(1.0, theme::BORDER));
		}

		// Lane clicks: select / create clips / drop devices.
		for (i, t) in tracks.iter().enumerate() {
			let y = layout.track_top(i, h, scroll);
			let r =
				Rect::from_min_size(pos2(area.left(), y), vec2(area.width(), h)).intersect(area);
			if r.height() <= 0.0 {
				continue;
			}
			let resp = ui.interact(r, Id::new(("lane", t.id)), Sense::click());
			if resp.clicked() {
				state.selection.select_track(t.id);
				state.selection.clear_clips();
			}
			if resp.double_clicked()
				&& t.kind == TrackKind::Instrument
				&& let Some(pos) = resp.interact_pointer_pos()
			{
				let tick = tl.x_to_tick(pos.x - area.left());
				let bar = ts.ticks_per_bar();
				let start = Tick(tick.0.max(0) / bar.0 * bar.0);
				if let Some((cmd, id)) = edits::new_midi_clip(state.project(), t.id, start, bar)
					&& state.apply(cmd)
				{
					state.selection.click_clip(t.id, id, false);
				}
			}
			if let Some(dev) = resp.dnd_release_payload::<DraggedDevice>() {
				state.use_device(&dev.0, Some(t.id));
			}
		}

		// Clips.
		for (i, t) in tracks.iter().enumerate() {
			let y = layout.track_top(i, h, scroll);
			if y > area.bottom() || y + h < area.top() {
				// Still process an in-flight drag of clips scrolled out of view.
				if self.drag.is_none() {
					continue;
				}
			}
			for c in &t.clips {
				self.clip(ui, state, &painter, layout, t, i, c);
			}
		}
	}

	#[allow(clippy::too_many_arguments)]
	fn clip(
		&mut self,
		ui: &mut Ui,
		state: &mut AppState,
		painter: &Painter,
		layout: &Layout,
		track: &Track,
		index: usize,
		clip: &Clip,
	) {
		let area = layout.lanes;
		let h = state.view.track_height;
		let tl = state.view.timeline;
		let ts = state.project().time_signature;
		let y = layout.track_top(index, h, state.view.scroll_y);
		let x0 = area.left() + tl.tick_to_x(clip.start);
		let x1 = area.left() + tl.tick_to_x(clip.end());
		let rect = Rect::from_min_max(pos2(x0, y + 1.0), pos2(x1.max(x0 + 2.0), y + h - 1.0));
		let id = Id::new(("clip", clip.id));
		let visible = rect.intersects(area);
		let response = if visible {
			Some(ui.interact(rect.intersect(area), id, Sense::click_and_drag()))
		} else {
			None
		};

		if let Some(r) = &response {
			let near_end = r
				.hover_pos()
				.is_some_and(|p| p.x > rect.right() - RESIZE_HANDLE);
			if near_end
				|| self
					.drag
					.as_ref()
					.is_some_and(|d| d.anchor == clip.id && d.kind == DragKind::Resize)
			{
				ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
			} else if r.hovered() {
				ui.ctx().set_cursor_icon(CursorIcon::Grab);
			}
			// Pressing an unselected clip selects it; dragging a selected one keeps the group.
			let add = ui.input(|i| i.modifiers.shift || i.modifiers.command);
			if r.clicked() || (r.drag_started() && !state.selection.clips.contains(&clip.id)) {
				state.selection.click_clip(track.id, clip.id, add);
			}
			if r.drag_started()
				&& let Some(press) = r.interact_pointer_pos()
			{
				let kind = if press.x > rect.right() - RESIZE_HANDLE {
					DragKind::Resize
				} else {
					DragKind::Move
				};
				self.drag = Some(ClipDrag {
					kind,
					anchor: clip.id,
					clips: if kind == DragKind::Move {
						state.selection.clips.iter().copied().collect()
					} else {
						vec![clip.id]
					},
					origin: if kind == DragKind::Move {
						clip.start
					} else {
						clip.end()
					},
					press_x: press.x,
					delta: Tick::ZERO,
					target: None,
				});
			}
			r.context_menu(|ui| clip_menu(ui, state, track.id, clip.id));
		}

		// Update an active drag anchored on this clip.
		if let Some(d) = &mut self.drag
			&& d.anchor == clip.id
		{
			if let Some(pos) = ui.ctx().pointer_interact_pos() {
				let raw = d.origin + tl.px_to_ticks(pos.x - d.press_x);
				let snapped = if state.view.snap && !ui.input(|i| i.modifiers.alt) {
					tl.snap(raw, ts)
				} else {
					Tick(raw.0.max(0))
				};
				d.delta = snapped - d.origin;
				if d.kind == DragKind::Move {
					let row = ((pos.y - layout.body_top + state.view.scroll_y) / h).floor();
					let tracks = &state.project().tracks;
					d.target = (row >= 0.0)
						.then(|| tracks.get(row as usize))
						.flatten()
						.filter(|t| t.id != track.id && edits::compatible(track.kind, t.kind))
						.map(|t| t.id);
				}
			}
			let released = ui.input(|i| i.pointer.any_released())
				|| response.as_ref().is_some_and(|r| r.drag_stopped());
			if released {
				let d = self.drag.take().expect("clip drag");
				let cmd = match d.kind {
					DragKind::Move => {
						edits::move_clips(state.project(), &d.clips, d.delta, d.target)
					}
					DragKind::Resize => {
						edits::resize_clip(state.project(), d.anchor, d.origin + d.delta)
					}
				};
				if let Some(cmd) = cmd {
					state.apply(cmd);
				}
				return;
			}
		}

		// Ghost position while dragging.
		let mut draw_rect = rect;
		let mut ghost = false;
		if let Some(d) = &self.drag
			&& d.clips.contains(&clip.id)
		{
			ghost = true;
			match d.kind {
				DragKind::Move => {
					let dx = tl.ticks_to_px(d.delta);
					let earliest = d
						.clips
						.iter()
						.filter_map(|c| state.project().find_clip(*c))
						.map(|(_, c)| c.start)
						.min()
						.unwrap_or(Tick::ZERO);
					let clamped = dx.max(-tl.ticks_to_px(earliest));
					draw_rect = draw_rect.translate(vec2(clamped, 0.0));
					if let Some(target) = d.target
						&& let Some(row) = state.project().track_index(target)
					{
						draw_rect = draw_rect.translate(vec2(0.0, (row as f32 - index as f32) * h));
					}
				}
				DragKind::Resize => {
					let end = area.left() + tl.tick_to_x(d.origin + d.delta);
					draw_rect.max.x = end.max(draw_rect.min.x + 4.0);
				}
			}
		}
		if !draw_rect.intersects(area) {
			return;
		}
		if ghost {
			painter.rect_stroke(
				rect,
				CornerRadius::same(theme::RADIUS),
				Stroke::new(1.0, theme::TEXT_FAINT),
				StrokeKind::Inside,
			);
		}
		let selected = state.selection.clips.contains(&clip.id);
		paint_clip(painter, state, track, clip, draw_rect, selected, area);
	}
}

fn grid_name(step: Tick, bar: Tick) -> String {
	if step.0 >= bar.0 {
		let bars = step.0 / bar.0.max(1);
		return if bars == 1 {
			"Grid 1 bar".into()
		} else {
			format!("Grid {bars} bars")
		};
	}
	let denom = (PPQ * 4) / step.0.max(1);
	format!("Grid 1/{denom}")
}

fn kind_glyph(kind: TrackKind) -> &'static str {
	match kind {
		TrackKind::Instrument => "♪",
		TrackKind::Audio => "~",
		TrackKind::Bus => "»",
	}
}

fn set_channel(state: &mut AppState, track: TrackId, patch: ChannelPatch) {
	state.apply(Command::SetChannel {
		track: Some(track),
		patch,
	});
}

pub fn add_track_buttons(ui: &mut Ui, state: &mut AppState) {
	for (label, kind) in [
		("+ Instrument", TrackKind::Instrument),
		("+ Audio", TrackKind::Audio),
		("+ Bus", TrackKind::Bus),
	] {
		if ui.small_button(label).clicked() {
			add_track(state, kind);
		}
	}
}

pub fn add_track(state: &mut AppState, kind: TrackKind) {
	let project = state.project();
	let track = edits::new_track(project, kind);
	let id = track.id;
	let cmd = edits::add_track(project, track, state.selection.track);
	if state.apply(cmd) {
		state.selection.select_track(id);
		state.selection.clear_clips();
	}
}

fn track_menu(
	ui: &mut Ui,
	state: &mut AppState,
	track: &Track,
	rename: &mut Option<(TrackId, String)>,
) {
	if ui.button("Rename").clicked() {
		*rename = Some((track.id, track.name.clone()));
		ui.close();
	}
	ui.horizontal_wrapped(|ui| {
		ui.spacing_mut().item_spacing = vec2(3.0, 3.0);
		for hex in TRACK_PALETTE {
			let c = theme::track_color(Some(hex));
			let (r, resp) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::click());
			ui.painter().rect_filled(r, CornerRadius::same(2), c);
			if track.color.as_deref() == Some(hex) {
				ui.painter().rect_stroke(
					r,
					CornerRadius::same(2),
					Stroke::new(1.5, Color32::WHITE),
					StrokeKind::Inside,
				);
			}
			if resp.clicked() {
				state.apply(Command::UpdateTrack {
					track: track.id,
					patch: TrackPatch {
						name: None,
						color: Some(Some(hex.to_owned())),
					},
				});
				ui.close();
			}
		}
	});
	ui.separator();
	if ui.button("Move up").clicked() {
		if let Some(cmd) = edits::move_track(state.project(), track.id, -1) {
			state.apply(cmd);
		}
		ui.close();
	}
	if ui.button("Move down").clicked() {
		if let Some(cmd) = edits::move_track(state.project(), track.id, 1) {
			state.apply(cmd);
		}
		ui.close();
	}
	ui.separator();
	if ui.button("Delete track").clicked() {
		state.apply(Command::RemoveTrack { track: track.id });
		ui.close();
	}
}

fn clip_menu(ui: &mut Ui, state: &mut AppState, track: TrackId, clip: ClipId) {
	if !state.selection.clips.contains(&clip) {
		state.selection.click_clip(track, clip, false);
	}
	if ui.button("Duplicate      Ctrl+D").clicked() {
		super::duplicate_selection(state);
		ui.close();
	}
	if ui.button("Split at playhead   S").clicked() {
		super::split_at_playhead(state);
		ui.close();
	}
	if ui.button("Delete            Del").clicked() {
		super::delete_selection(state);
		ui.close();
	}
}

/// Vertical grid lines (bars strong, beats and subdivisions fainter).
fn grid(p: &Painter, area: Rect, tl: &Timeline, ts: cadenza_core::TimeSignature) {
	let g = tl.grid(ts);
	let beat = Tick(PPQ * 4 / i64::from(ts.denominator.max(1)));
	let first = tl.x_to_tick(0.0).0.max(0) / g.step.0 * g.step.0;
	let mut t = first;
	let end = tl.x_to_tick(area.width()).0;
	while t <= end {
		let x = area.left() + tl.tick_to_x(Tick(t));
		let c = if t % g.bar.0 == 0 {
			theme::GRID_BAR
		} else if t % beat.0 == 0 {
			theme::GRID_BEAT
		} else {
			theme::GRID_FINE
		};
		p.vline(x, area.y_range(), Stroke::new(1.0, c));
		t += g.step.0;
	}
}

fn ruler(ui: &mut Ui, state: &mut AppState, layout: &Layout) {
	let r = layout.ruler;
	let tl = state.view.timeline;
	let ts = state.project().time_signature;
	let p = ui.painter_at(r);
	p.rect_filled(r, 0.0, theme::BG_RAISED);
	let g = tl.grid(ts);
	// Bar numbers: every bar unless too dense.
	let bar_px = tl.ticks_to_px(g.bar);
	let every = (48.0 / bar_px).ceil().max(1.0) as u64;
	let label_step = g.bar.0 * every.next_power_of_two() as i64;
	let mut t = tl.x_to_tick(0.0).0.max(0) / g.step.0 * g.step.0;
	let end = tl.x_to_tick(r.width()).0;
	while t <= end {
		let x = r.left() + tl.tick_to_x(Tick(t));
		if t % g.bar.0 == 0 {
			p.vline(
				x,
				r.top() + 10.0..=r.bottom(),
				Stroke::new(1.0, theme::TEXT_FAINT),
			);
			if t % label_step == 0 {
				p.text(
					pos2(x + 4.0, r.top() + 3.0),
					Align2::LEFT_TOP,
					format!("{}", t / g.bar.0 + 1),
					theme::mono(10.5),
					theme::TEXT_DIM,
				);
			}
		} else {
			p.vline(
				x,
				r.bottom() - 5.0..=r.bottom(),
				Stroke::new(1.0, theme::GRID_BAR),
			);
		}
		t += g.step.0;
	}
	p.hline(
		r.x_range(),
		r.bottom() - 0.5,
		Stroke::new(1.0, theme::BORDER),
	);
	let resp = ui.interact(r, Id::new("ruler"), Sense::click_and_drag());
	let resp = resp.on_hover_cursor(CursorIcon::PointingHand);
	if (resp.clicked() || resp.dragged())
		&& let Some(pos) = resp.interact_pointer_pos()
	{
		let tick = tl.x_to_tick(pos.x - r.left());
		let tick = if state.view.snap {
			tl.snap(tick, ts)
		} else {
			tick
		};
		state.seek(tick);
	}
}

fn markers(ui: &mut Ui, state: &mut AppState, layout: &Layout) {
	let r = layout.markers;
	let tl = state.view.timeline;
	let p = ui.painter_at(r);
	p.rect_filled(r, 0.0, theme::BG_PANEL);
	p.hline(
		r.x_range(),
		r.bottom() - 0.5,
		Stroke::new(1.0, theme::BORDER),
	);
	let resp = ui.interact(r, Id::new("markers"), Sense::click());
	let list: Vec<Marker> = state.project().markers.clone();
	for (i, m) in list.iter().enumerate() {
		let x = r.left() + tl.tick_to_x(m.tick);
		let next = list
			.get(i + 1)
			.map_or(r.right(), |n| r.left() + tl.tick_to_x(n.tick));
		if next < r.left() || x > r.right() {
			continue;
		}
		let flag = Rect::from_min_max(
			pos2(x, r.top() + 2.0),
			pos2(next.min(x + 160.0) - 2.0, r.bottom() - 2.0),
		);
		p.rect_filled(
			flag,
			CornerRadius::same(2),
			theme::MARKER.gamma_multiply(0.25),
		);
		p.vline(x, r.y_range(), Stroke::new(2.0, theme::MARKER));
		p.with_clip_rect(flag.intersect(r)).text(
			pos2(x + 5.0, r.center().y),
			Align2::LEFT_CENTER,
			&m.name,
			theme::small(),
			theme::TEXT,
		);
		let mr = ui.interact(
			flag.intersect(r),
			Id::new(("marker", m.tick.0)),
			Sense::click(),
		);
		if mr.clicked() {
			state.seek(m.tick);
		}
		mr.context_menu(|ui| {
			if ui.button("Remove marker").clicked() {
				state.apply(Command::RemoveMarker { tick: m.tick });
				ui.close();
			}
		});
	}
	if resp.double_clicked()
		&& let Some(pos) = resp.interact_pointer_pos()
	{
		let ts = state.project().time_signature;
		let tick = tl.snap(tl.x_to_tick(pos.x - r.left()), ts);
		let name = format!("Marker {}", list.len() + 1);
		state.apply(Command::AddMarker {
			marker: Marker::new(tick, name),
		});
	}
	resp.on_hover_text("Double-click to add a marker");
}

#[allow(clippy::too_many_arguments)]
fn paint_clip(
	p: &Painter,
	state: &mut AppState,
	track: &Track,
	clip: &Clip,
	rect: Rect,
	selected: bool,
	area: Rect,
) {
	let color = theme::track_color(track.color.as_deref());
	let body = theme::mix(color, theme::BG_DEEP, 0.62);
	let r = CornerRadius::same(theme::RADIUS);
	let p = p.with_clip_rect(area.intersect(rect.expand(1.0)));
	p.rect_filled(rect, r, body);
	let header = Rect::from_min_size(rect.min, vec2(rect.width(), CLIP_HEADER.min(rect.height())));
	p.rect_filled(
		header,
		CornerRadius {
			nw: theme::RADIUS,
			ne: theme::RADIUS,
			sw: 0,
			se: 0,
		},
		theme::mix(color, theme::BG_DEEP, 0.15),
	);
	let text_x = rect.left().max(area.left()) + 4.0;
	p.with_clip_rect(header.intersect(area)).text(
		pos2(text_x, header.center().y),
		Align2::LEFT_CENTER,
		&clip.name,
		FontId::proportional(10.5),
		theme::text_on(color),
	);
	let content = Rect::from_min_max(
		pos2(rect.left(), header.bottom() + 1.0),
		rect.max - vec2(0.0, 2.0),
	);
	if content.height() > 4.0 {
		match &clip.content {
			ClipContent::Midi { notes } => {
				paint_notes(&p, clip, notes, content, area, color, &state.view.timeline)
			}
			ClipContent::Audio {
				file,
				offset_seconds,
				gain_db,
				fade_in,
				fade_out,
			} => {
				let peaks = state.peaks.get(file);
				paint_waveform(
					&p,
					state.project(),
					clip,
					&peaks,
					*offset_seconds,
					*gain_db,
					content,
					area,
					color,
				);
				paint_fades(
					&p,
					state.project(),
					clip,
					fade_in.map(|f| f.seconds),
					fade_out.map(|f| f.seconds),
					rect,
				);
			}
		}
	}
	p.rect_stroke(
		rect,
		r,
		if selected {
			Stroke::new(1.5, Color32::WHITE)
		} else {
			Stroke::new(1.0, theme::mix(color, theme::BG_DEEP, 0.35))
		},
		StrokeKind::Inside,
	);
}

fn paint_notes(
	p: &Painter,
	clip: &Clip,
	notes: &[cadenza_model::Note],
	content: Rect,
	area: Rect,
	color: Color32,
	tl: &Timeline,
) {
	if notes.is_empty() {
		return;
	}
	let (lo, hi) = notes.iter().fold((127u8, 0u8), |(lo, hi), n| {
		(lo.min(n.pitch), hi.max(n.pitch))
	});
	let span = f32::from(hi - lo).max(11.0);
	let pad = 3.0;
	let usable = content.height() - 2.0 * pad;
	let row = (usable / (span + 1.0)).clamp(1.0, 6.0);
	let note_color = theme::mix(color, Color32::WHITE, 0.35);
	let left = area.left() - content.left();
	for n in notes {
		let x0 = content.left() + tl.ticks_to_px(n.start);
		let x1 = content.left()
			+ tl.ticks_to_px(n.start + n.length)
				.min(tl.ticks_to_px(clip.length));
		if x1 < area.left() || x0 > area.right() || x1 < content.left() + left.max(0.0) - 1.0 {
			continue;
		}
		let rel = f32::from(n.pitch - lo) / span;
		let y = content.bottom() - pad - rel * (usable - row) - row;
		p.rect_filled(
			Rect::from_min_max(pos2(x0, y), pos2(x1.max(x0 + 1.5), y + row)),
			0.0,
			note_color,
		);
	}
}

#[allow(clippy::too_many_arguments)]
fn paint_waveform(
	p: &Painter,
	project: &Project,
	clip: &Clip,
	peaks: &Peaks,
	offset_seconds: f64,
	gain_db: f32,
	content: Rect,
	area: Rect,
	color: Color32,
) {
	let wave_color = theme::mix(color, Color32::WHITE, 0.25);
	let peaks = match peaks {
		Peaks::Ready(p) => p,
		Peaks::Pending => {
			p.text(
				content.center(),
				Align2::CENTER_CENTER,
				"loading waveform…",
				theme::small(),
				theme::TEXT_FAINT,
			);
			return;
		}
		Peaks::Failed(e) => {
			p.text(
				pos2(content.left().max(area.left()) + 4.0, content.center().y),
				Align2::LEFT_CENTER,
				format!("⚠ {e}"),
				theme::small(),
				theme::WARNING,
			);
			return;
		}
	};
	let tempo = &project.tempo;
	let clip_start_s = tempo.tick_to_seconds(clip.start);
	let sr = f64::from(peaks.sample_rate);
	let gain = 10f32.powf(gain_db / 20.0);
	let mid = content.center().y;
	let half = content.height() * 0.5 - 1.0;
	let x_start = content.left().max(area.left()).floor();
	let x_end = content.right().min(area.right()).ceil();
	let mut x = x_start;
	let channels = usize::from(peaks.num_channels).max(1);
	// Seconds into the clip at each pixel via the tempo map.
	let px_to_s = |x: f32, timeline_px_per_beat: f32| -> f64 {
		let beats = f64::from(x - content.left()) / f64::from(timeline_px_per_beat);
		tempo.tick_to_seconds(clip.start + Tick::from_beats(beats)) - clip_start_s
	};
	let ppb = content.width() / clip.length.beats().max(1e-9) as f32;
	let fpp = (px_to_s(content.left() + 1.0, ppb) * sr).max(1.0);
	let level = peaks.level_for(fpp);
	let mut points_top = Vec::new();
	while x < x_end {
		let s0 = px_to_s(x, ppb) + offset_seconds;
		let s1 = px_to_s(x + 1.0, ppb) + offset_seconds;
		let f0 = (s0 * sr).max(0.0) as u64;
		let f1 = ((s1 * sr).max(0.0) as u64).max(f0 + 1);
		if f0 >= peaks.frames {
			break;
		}
		let mut mm = cadenza_audio_io::MinMax::EMPTY;
		for c in 0..channels {
			mm = mm.merge(level.range(c, f0, f1));
		}
		if !mm.is_empty() {
			let top = mid - (mm.max * gain).clamp(-1.0, 1.0) * half;
			let bottom = mid - (mm.min * gain).clamp(-1.0, 1.0) * half;
			points_top.push((x, top, bottom));
		}
		x += 1.0;
	}
	for (x, top, bottom) in points_top {
		p.vline(
			x + 0.5,
			top..=bottom.max(top + 1.0),
			Stroke::new(1.0, wave_color),
		);
	}
	p.hline(
		content.x_range(),
		mid,
		Stroke::new(1.0, wave_color.gamma_multiply(0.3)),
	);
}

fn paint_fades(
	p: &Painter,
	project: &Project,
	clip: &Clip,
	fade_in: Option<f64>,
	fade_out: Option<f64>,
	rect: Rect,
) {
	let tempo = &project.tempo;
	let start_s = tempo.tick_to_seconds(clip.start);
	let end_s = tempo.tick_to_seconds(clip.end());
	let x_of = |s: f64| {
		let t = tempo.seconds_to_tick(s);
		rect.left()
			+ ((t - clip.start).beats() / clip.length.beats().max(1e-9)) as f32 * rect.width()
	};
	let stroke = Stroke::new(1.0, Color32::from_white_alpha(140));
	let shade = Color32::from_black_alpha(90);
	if let Some(f) = fade_in {
		let x = x_of(start_s + f);
		let tri = vec![rect.left_top(), pos2(x, rect.top()), rect.left_bottom()];
		p.add(Shape::convex_polygon(tri, shade, Stroke::NONE));
		p.line_segment([rect.left_bottom(), pos2(x, rect.top())], stroke);
	}
	if let Some(f) = fade_out {
		let x = x_of(end_s - f);
		let tri = vec![pos2(x, rect.top()), rect.right_top(), rect.right_bottom()];
		p.add(Shape::convex_polygon(tri, shade, Stroke::NONE));
		p.line_segment([pos2(x, rect.top()), rect.right_bottom()], stroke);
	}
}
