//! Arrange view ("playlist"): song overview, ruler, markers, track headers and clip
//! lanes, with scrollbars, wheel/middle-drag navigation and drop targets.
//!
//! Gestures are previewed locally (ghost clips, insertion line, drop highlight) and
//! become one command through [`AppState::apply`] when the mouse is released.

use cadenza_core::{ClipId, MeterMap, PPQ, Tick, TrackId};
use cadenza_model::{
	ChannelPatch, Clip, ClipContent, Command, Marker, Project, Track, TrackKind, TrackPatch,
};
use egui::{
	Align2, Color32, CornerRadius, CursorIcon, FontId, Id, Painter, PointerButton, Pos2, Rect,
	Sense, Shape, Stroke, StrokeKind, Ui, pos2, vec2,
};

use crate::jobs::Peaks;
use crate::state::automation::LANE_HEIGHT;
use crate::state::dnd::{DropTarget, Payload};
use crate::state::edits::{self, TRACK_PALETTE};
use crate::state::files::{FileKind, classify};
use crate::state::layout::FollowMode;
use crate::state::rows::{RowKind, Rows};
use crate::state::selection::DeviceRef;
use crate::state::timeline::{self, LineKind, Overview, Timeline};
use crate::state::{AppState, MAX_TRACK_HEIGHT, MIN_TRACK_HEIGHT};
use crate::theme;
use crate::ui::dnd;
use crate::ui::widgets::{self, Gesture};

/// Width of the clip edge that resizes instead of moving.
const RESIZE_HANDLE: f32 = 7.0;
const CLIP_HEADER: f32 = 14.0;
/// Room below the last track (add buttons and the "new track" drop zone).
const BELOW_TRACKS: f32 = 72.0;

#[derive(Debug, Clone, Copy, PartialEq)]
enum DragKind {
	Move,
	Resize,
	/// Left edge: trims/extends the start, content stays in place.
	TrimStart,
}

#[derive(Debug, Clone)]
struct ClipDrag {
	kind: DragKind,
	anchor: ClipId,
	clips: Vec<ClipId>,
	/// Anchor start (move) or end (resize) when the drag began.
	origin: Tick,
	/// Song position under the pointer at the press (scroll-independent).
	press: Tick,
	/// Snapped offset of the gesture.
	delta: Tick,
	/// Track under the pointer when it differs from the anchor's.
	target: Option<TrackId>,
	/// Ctrl held: the clips are copied, not moved.
	copy: bool,
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
	/// Middle-button pan in progress.
	pan: bool,
	/// Rubber band on empty lane area: press and current pointer (screen), Shift adds.
	band: Option<(Pos2, Pos2, bool)>,
	/// Audio clip overlaps to mark this frame (nested clips that sum).
	overlaps: Vec<Rect>,
	/// Automatic crossfades of this frame (lane rect of the crossfade, the crossfade).
	xfades: Vec<(Rect, edits::Crossfade)>,
	/// Crossfade length handle being dragged: the crossfade and its new end.
	xfade_drag: Option<(edits::Crossfade, Tick)>,
	/// Overview viewport drag: beats between the grab point and the view start.
	overview_grab: Option<f64>,
	/// Loop region drag on the ruler: the song position where it started.
	loop_drag: Option<Tick>,
	/// Size of the lanes area in the last frame (keyboard navigation, zoom to fit).
	pub lanes_width: f32,
	pub lanes_height: f32,
	last: Option<Layout>,
	/// Row layout of this frame (tracks, automation lanes, master).
	pub rows: Rows,
	/// Automation lane gestures.
	pub auto: super::automation::AutoUi,
	/// Alt held this frame (an audio file dropped below the tracks becomes a sampler).
	alt: bool,
}

/// Header indentation per folder level.
const FOLDER_INDENT: f32 = 10.0;

/// Geometry shared by the painting helpers.
#[derive(Debug, Clone, Copy)]
struct Layout {
	header: Rect,
	lanes: Rect,
	overview: Rect,
	ruler: Rect,
	markers: Rect,
	hbar: Rect,
	vbar: Rect,
	body_top: f32,
	/// Tempo lane (pinned under the markers) when shown, timeline part.
	tempo: Option<Rect>,
	/// Time signature lane (above the tempo lane, shown with it), timeline part.
	signature: Option<Rect>,
}

/// Total height of the track list (with the room below the last track).
pub fn content_height(state: &AppState) -> f32 {
	Rows::new(state.project(), &state.view).height + BELOW_TRACKS
}

impl ArrangeUi {
	pub fn show(&mut self, ui: &mut Ui, state: &mut AppState) {
		self.alt = ui.input(|i| i.modifiers.alt);
		let full = ui.available_rect_before_wrap();
		ui.allocate_rect(full, Sense::hover());
		let x0 = full.left() + theme::HEADER_WIDTH;
		let right = full.right() - theme::SCROLLBAR;
		let bottom = full.bottom() - theme::SCROLLBAR;
		let ov_bottom = full.top() + theme::OVERVIEW_HEIGHT;
		let ruler_bottom = ov_bottom + theme::RULER_HEIGHT;
		let markers_bottom = ruler_bottom + theme::MARKER_LANE_HEIGHT;
		let (tempo_h, sig_h) = if state.view.automation.tempo {
			(LANE_HEIGHT, super::automation::SIGNATURE_LANE_HEIGHT)
		} else {
			(0.0, 0.0)
		};
		let top = markers_bottom + sig_h + tempo_h;
		self.rows = Rows::new(state.project(), &state.view);
		let layout = Layout {
			header: Rect::from_min_max(pos2(full.left(), top), pos2(x0, bottom)),
			lanes: Rect::from_min_max(pos2(x0, top), pos2(right, bottom)),
			overview: Rect::from_min_max(pos2(x0, full.top()), pos2(right, ov_bottom)),
			ruler: Rect::from_min_max(pos2(x0, ov_bottom), pos2(right, ruler_bottom)),
			markers: Rect::from_min_max(pos2(x0, ruler_bottom), pos2(right, markers_bottom)),
			signature: (sig_h > 0.0).then(|| {
				Rect::from_min_max(
					pos2(x0, markers_bottom),
					pos2(right, markers_bottom + sig_h),
				)
			}),
			tempo: (tempo_h > 0.0)
				.then(|| Rect::from_min_max(pos2(x0, markers_bottom + sig_h), pos2(right, top))),
			hbar: Rect::from_min_max(pos2(x0, bottom), pos2(right, full.bottom())),
			vbar: Rect::from_min_max(pos2(right, top), pos2(full.right(), bottom)),
			body_top: top,
		};
		self.lanes_width = layout.lanes.width();
		self.lanes_height = layout.lanes.height();
		self.handle_wheel(ui, state, &layout, full);
		self.handle_pan(ui, state, &layout);

		let playing = state.is_playing();
		let position = state.position();
		if playing && self.drag.is_none() && !self.pan {
			state.view.timeline.follow_mode(
				state.view.follow,
				position.beats(),
				layout.lanes.width(),
			);
		}
		self.auto_scroll(ui, state, &layout);
		state.view.scroll_y = timeline::clamp_scroll(
			state.view.scroll_y,
			content_height(state),
			layout.lanes.height(),
		);

		let painter = ui.painter_at(full);
		painter.rect_filled(full, 0.0, theme::LANE);

		self.lanes(ui, state, &layout);
		self.headers(ui, state, &layout);
		self.corner(ui, state, full);
		self.overview(ui, state, &layout);
		ruler(ui, state, &layout, &mut self.loop_drag);
		markers(ui, state, &layout);
		if let Some(r) = layout.signature {
			let header = Rect::from_min_max(pos2(full.left(), r.top()), pos2(x0, r.bottom()));
			self.auto.signature_lane(ui, state, header, r);
		}
		if let Some(r) = layout.tempo {
			let header = Rect::from_min_max(pos2(full.left(), r.top()), pos2(x0, r.bottom()));
			self.auto.tempo_lane(ui, state, header, r);
		}
		scrollbars(ui, state, &layout, full);
		self.drop_zone(ui, state, &layout);

		loop_overlay(ui, state, &layout);
		// Playhead over everything in the timeline.
		let x = layout.lanes.left() + state.view.timeline.tick_to_x(position);
		if (layout.lanes.left()..=layout.lanes.right()).contains(&x) {
			let p = ui.painter_at(Rect::from_min_max(
				pos2(layout.lanes.left(), layout.ruler.top()),
				pos2(layout.lanes.right(), layout.lanes.bottom()),
			));
			p.vline(
				x,
				layout.ruler.top()..=layout.lanes.bottom(),
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
		self.last = Some(layout);
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
			// Keep the track under the pointer in place.
			let anchor = pointer.y - layout.body_top;
			let rows = (anchor + view.scroll_y) / view.track_height;
			view.track_height =
				(view.track_height + scroll.y * 0.25).clamp(MIN_TRACK_HEIGHT, MAX_TRACK_HEIGHT);
			view.scroll_y = (rows * view.track_height - anchor).max(0.0);
			return;
		}
		if scroll.x != 0.0 {
			view.timeline.scroll_px(-scroll.x);
		}
		if scroll.y != 0.0 && !layout.overview.contains(pointer) && !layout.ruler.contains(pointer)
		{
			view.scroll_y -= scroll.y;
		} else if scroll.y != 0.0 {
			// Over the ruler/overview the wheel scrolls the song.
			view.timeline.scroll_px(-scroll.y);
		}
	}

	/// Middle-button drag pans in both directions (cursor shows a grabbing hand).
	fn handle_pan(&mut self, ui: &Ui, state: &mut AppState, layout: &Layout) {
		let (pressed, down, delta, pos) = ui.input(|i| {
			(
				i.pointer.button_pressed(PointerButton::Middle),
				i.pointer.middle_down(),
				i.pointer.delta(),
				i.pointer.interact_pos(),
			)
		});
		if pressed
			&& pos.is_some_and(|p| {
				layout.lanes.contains(p) || layout.header.contains(p) || layout.ruler.contains(p)
			}) {
			self.pan = true;
		}
		if !down {
			self.pan = false;
		}
		if self.pan {
			state.view.timeline.scroll_px(-delta.x);
			state.view.scroll_y -= delta.y;
			ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
		}
	}

	/// Scrolls while a clip (or something from the browser) is dragged near the edges.
	fn auto_scroll(&self, ui: &Ui, state: &mut AppState, layout: &Layout) {
		let dragging_payload = egui::DragAndDrop::has_payload_of_type::<Payload>(ui.ctx());
		if self.drag.is_none() && !dragging_payload {
			return;
		}
		let Some(pos) = ui.ctx().pointer_latest_pos() else {
			return;
		};
		let inside_x =
			(layout.lanes.top()..=layout.lanes.bottom()).contains(&pos.y) || self.drag.is_some();
		let dx = timeline::edge_scroll(pos.x, layout.lanes.left(), layout.lanes.right());
		let dy = timeline::edge_scroll(pos.y, layout.lanes.top(), layout.lanes.bottom());
		let vertical = self.drag.as_ref().is_none_or(|d| d.kind == DragKind::Move);
		let mut moved = false;
		if inside_x && dx != 0.0 && pos.x > layout.header.right() {
			state.view.timeline.scroll_px(dx);
			moved = true;
		}
		if vertical && dy != 0.0 && (pos.x > layout.header.left()) {
			state.view.scroll_y += dy;
			moved = true;
		}
		if moved {
			ui.ctx().request_repaint();
		}
	}

	/// Top-left corner above the headers: zoom buttons, snap and grid.
	fn corner(&self, ui: &mut Ui, state: &mut AppState, full: Rect) {
		let rect = Rect::from_min_size(
			full.min,
			vec2(
				theme::HEADER_WIDTH,
				theme::OVERVIEW_HEIGHT + theme::RULER_HEIGHT + theme::MARKER_LANE_HEIGHT,
			),
		);
		ui.painter().rect_filled(rect, 0.0, theme::BG_PANEL);
		ui.painter().hline(
			rect.x_range(),
			rect.bottom() - 0.5,
			Stroke::new(1.0, theme::BORDER),
		);
		ui.painter().vline(
			rect.right() - 0.5,
			rect.y_range(),
			Stroke::new(1.0, theme::BORDER_STRONG),
		);
		let grid = state
			.view
			.timeline
			.grid_at(state.position(), state.project().meter());
		let row1 = Rect::from_min_size(
			rect.min + vec2(8.0, 2.0),
			vec2(rect.width() - 16.0, theme::OVERVIEW_HEIGHT),
		);
		let row2 = Rect::from_min_size(
			pos2(rect.left() + 8.0, row1.bottom() + 2.0),
			vec2(rect.width() - 16.0, theme::RULER_HEIGHT),
		);
		let mut top = ui.new_child(
			egui::UiBuilder::new()
				.max_rect(row1)
				.layout(egui::Layout::left_to_right(egui::Align::Center)),
		);
		top.label(
			egui::RichText::new("ARRANGE")
				.font(theme::small())
				.color(theme::TEXT_FAINT),
		);
		top.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
			if ui
				.small_button("−")
				.on_hover_text("Zoom out (Ctrl+wheel)")
				.clicked()
			{
				state.view.timeline.zoom_around(1.0 / 1.5, 0.0);
			}
			if ui
				.small_button("+")
				.on_hover_text("Zoom in (Ctrl+wheel)")
				.clicked()
			{
				state.view.timeline.zoom_around(1.5, 0.0);
			}
			if ui
				.small_button("Sel")
				.on_hover_text("Zoom to the selected clips (Z)")
				.clicked()
			{
				zoom_to_selection(state, self.lanes_width, self.lanes_height);
			}
			if ui
				.small_button("Fit")
				.on_hover_text("Zoom to the whole song (Shift+Z)")
				.clicked()
			{
				zoom_to_fit(state, self.lanes_width);
			}
		});
		let mut child = ui.new_child(
			egui::UiBuilder::new()
				.max_rect(row2)
				.layout(egui::Layout::left_to_right(egui::Align::Center)),
		);
		let snap = state.view.snap;
		if child
			.selectable_label(snap, "Snap")
			.on_hover_text("Snap clips and notes to the grid (Alt while dragging: free)")
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
			follow_menu(ui, state);
		});
	}

	/// The whole song in a strip above the ruler; the visible part is a draggable box.
	fn overview(&mut self, ui: &mut Ui, state: &mut AppState, layout: &Layout) {
		let r = layout.overview;
		let p = ui.painter_at(r);
		p.rect_filled(r, 0.0, theme::BG_DEEP);
		let tl = state.view.timeline;
		let visible = tl.visible_beats(r.width());
		let song_end = state.song_end().beats();
		let total = timeline::content_beats(song_end, tl.scroll_beats, visible);
		let ov = Overview {
			total,
			width: r.width(),
		};
		let tracks = &state.project().tracks;
		let n = tracks.len().max(1) as f32;
		let inner = r.shrink2(vec2(0.0, 3.0));
		let row = (inner.height() / n).max(1.0);
		for (i, t) in tracks.iter().enumerate() {
			let color = theme::track_color(t.color.as_deref());
			let y = inner.top() + (i as f32 * row).min(inner.height() - 1.0);
			for c in &t.clips {
				let x0 = r.left() + ov.to_x(c.start.beats());
				let x1 = r.left() + ov.to_x(c.end().beats());
				p.rect_filled(
					Rect::from_min_max(
						pos2(x0, y),
						pos2(x1.max(x0 + 1.0), y + (row - 0.5).max(1.0)),
					),
					0.0,
					color.gamma_multiply(0.7),
				);
			}
		}
		for m in &state.project().markers {
			let x = r.left() + ov.to_x(m.tick.beats());
			p.vline(
				x,
				r.y_range(),
				Stroke::new(1.0, theme::MARKER.gamma_multiply(0.7)),
			);
		}
		let px = r.left() + ov.to_x(state.position().beats());
		p.vline(px, r.y_range(), Stroke::new(1.0, theme::PLAYHEAD));
		let (vx0, vx1) = ov.thumb(tl.scroll_beats, visible, 6.0);
		let view = Rect::from_x_y_ranges(
			r.left() + vx0..=r.left() + vx1,
			r.top() + 1.0..=r.bottom() - 1.0,
		);
		let resp = ui
			.interact(r, Id::new("arrange-overview"), Sense::click_and_drag())
			.on_hover_text("Song overview: drag the box to scroll, double-click to fit");
		let active = resp.dragged() || resp.hovered();
		p.rect_filled(
			view,
			CornerRadius::same(2),
			Color32::from_white_alpha(if active { 22 } else { 12 }),
		);
		p.rect_stroke(
			view,
			CornerRadius::same(2),
			Stroke::new(
				1.0,
				if active {
					theme::ACCENT
				} else {
					theme::ACCENT_DIM
				},
			),
			StrokeKind::Inside,
		);
		p.hline(
			r.x_range(),
			r.bottom() - 0.5,
			Stroke::new(1.0, theme::BORDER),
		);
		if resp.double_clicked() {
			zoom_to_fit(state, layout.lanes.width());
			return;
		}
		if let Some(pos) = resp.interact_pointer_pos() {
			let beat = ov.to_value(pos.x - r.left());
			if resp.drag_started() || (resp.clicked() && self.overview_grab.is_none()) {
				let inside = (view.left()..=view.right()).contains(&pos.x);
				self.overview_grab = Some(if inside {
					beat - tl.scroll_beats
				} else {
					visible * 0.5
				});
			}
			if let Some(grab) = self.overview_grab
				&& (resp.dragged() || resp.clicked())
			{
				let start = (beat - grab).clamp(0.0, (total - visible).max(0.0));
				state.view.timeline.scroll_beats = start;
			}
		}
		if resp.drag_stopped() || resp.clicked() {
			self.overview_grab = None;
		}
	}

	/// Screen y of the top of track `index`'s group (`tracks.len()`: after the last).
	fn top(&self, layout: &Layout, index: usize, scroll: f32) -> f32 {
		layout.body_top + self.rows.track_top(index) - scroll
	}

	/// Content y (before scrolling) of screen y.
	fn content_y(layout: &Layout, state: &AppState, y: f32) -> f32 {
		y - layout.body_top + state.view.scroll_y
	}

	/// Song position (snapped when snap is on) at screen x inside the lanes.
	fn tick_at(&self, state: &AppState, layout: &Layout, x: f32) -> Tick {
		let tl = state.view.timeline;
		let t = tl.x_to_tick(x - layout.lanes.left());
		if state.view.snap {
			tl.snap_floor(t, state.project().meter())
		} else {
			Tick(t.0.max(0))
		}
	}

	/// Screen rect of the clip row of track `index` in the last drawn frame.
	pub fn track_rect(&self, scroll_y: f32, index: usize) -> Option<Rect> {
		let layout = self.last?;
		let row = self.rows.track(index)?;
		let y = layout.body_top + row.top - scroll_y;
		Some(Rect::from_min_size(
			pos2(layout.lanes.left(), y),
			vec2(layout.lanes.width(), row.height),
		))
	}

	/// Screen rect of the automation lane `address` in the last drawn frame.
	pub fn lane_rect(&self, scroll_y: f32, address: &cadenza_core::ParamAddress) -> Option<Rect> {
		let layout = self.last?;
		let row = self.rows.lane(address)?;
		let y = layout.body_top + row.top - scroll_y;
		Some(Rect::from_min_size(
			pos2(layout.lanes.left(), y),
			vec2(layout.lanes.width(), row.height),
		))
	}

	/// The drop target under `pos` in the last drawn frame (`None` outside the view).
	pub fn target_at(&self, state: &AppState, pos: Pos2) -> Option<DropTarget> {
		let layout = self.last?;
		self.target_in(state, &layout, pos)
	}

	fn target_in(&self, state: &AppState, layout: &Layout, pos: Pos2) -> Option<DropTarget> {
		let in_lanes = layout.lanes.contains(pos);
		if !in_lanes && !layout.header.contains(pos) {
			return None;
		}
		let y = Self::content_y(layout, state, pos.y);
		let at = if in_lanes {
			self.tick_at(state, layout, pos.x)
		} else {
			state.position()
		};
		if let Some((track, _)) = self.rows.track_at(y) {
			return Some(if in_lanes {
				DropTarget::Lane { track, at }
			} else {
				DropTarget::Track(track)
			});
		}
		Some(if self.alt {
			DropTarget::BelowSampler { at }
		} else {
			DropTarget::Below { at }
		})
	}

	/// Highlights the drop target of an in-app drag or an OS file hover, shows the hint
	/// and performs in-app drops on release (OS drops are handled by the window).
	fn drop_zone(&mut self, ui: &mut Ui, state: &mut AppState, layout: &Layout) {
		let ctx = ui.ctx().clone();
		let in_app = egui::DragAndDrop::payload::<Payload>(&ctx).map(|p| (*p).clone());
		let payload = in_app
			.clone()
			.or_else(|| dnd::hovered_file(&ctx).map(Payload::File));
		let Some(payload) = payload else {
			return;
		};
		let Some(pos) = ctx.pointer_latest_pos() else {
			return;
		};
		let Some(target) = self.target_in(state, layout, pos) else {
			return;
		};
		let copy = ctx.input(|i| i.modifiers.command);
		let action = state.drop_action(&payload, target, copy);
		let accepted = action.accepted();
		let color = if accepted {
			theme::ACCENT
		} else {
			theme::ERROR
		};
		let h = state.view.track_height;
		let scroll = state.view.scroll_y;
		let clip = Rect::from_min_max(layout.header.min, layout.lanes.max);
		let p = ui.painter_at(clip);
		let row_rect = |i: usize| {
			let y = self.top(layout, i, scroll);
			Rect::from_min_max(
				pos2(layout.header.left(), y),
				pos2(layout.lanes.right(), y + h),
			)
		};
		let (zone, ghost_row) = match target {
			DropTarget::Track(t) | DropTarget::Lane { track: t, .. } => {
				let i = state.project().track_index(t).unwrap_or(0);
				(row_rect(i), Some(row_rect(i)))
			}
			_ => {
				let y = layout.body_top + self.rows.height - scroll;
				let zone = Rect::from_min_max(
					pos2(layout.header.left(), y),
					pos2(layout.lanes.right(), layout.lanes.bottom().max(y + h)),
				);
				(
					zone,
					Some(Rect::from_min_size(zone.min, vec2(zone.width(), h))),
				)
			}
		};
		p.rect_filled(zone, 0.0, color.gamma_multiply(0.08));
		p.rect_stroke(
			zone.shrink(1.0),
			CornerRadius::same(2),
			Stroke::new(1.5, color),
			StrokeKind::Inside,
		);
		if matches!(
			target,
			DropTarget::Below { .. } | DropTarget::BelowSampler { .. }
		) && accepted
		{
			p.text(
				pos2(layout.header.center().x, zone.top() + h * 0.5),
				Align2::CENTER_CENTER,
				if matches!(target, DropTarget::BelowSampler { .. }) {
					"+ new sampler track"
				} else {
					"+ new track (Alt: sampler)"
				},
				theme::small(),
				theme::ACCENT,
			);
		}
		// Ghost clip where a dropped file will land.
		if let (Payload::File(path), Some(at), Some(row)) = (&payload, target.at(), ghost_row)
			&& accepted
			&& matches!(classify(path), Some(FileKind::Audio | FileKind::Midi))
		{
			let tl = state.view.timeline;
			let x0 = layout.lanes.left() + tl.tick_to_x(at);
			let w = tl
				.ticks_to_px(state.project().meter().bar_start(2))
				.max(60.0);
			let ghost = Rect::from_min_size(pos2(x0, row.top() + 2.0), vec2(w, row.height() - 4.0));
			let gp = ui.painter_at(layout.lanes);
			gp.rect_filled(
				ghost,
				CornerRadius::same(theme::RADIUS),
				theme::ACCENT.gamma_multiply(0.25),
			);
			gp.rect_stroke(
				ghost,
				CornerRadius::same(theme::RADIUS),
				Stroke::new(1.0, theme::ACCENT),
				StrokeKind::Inside,
			);
			gp.text(
				ghost.left_top() + vec2(4.0, 3.0),
				Align2::LEFT_TOP,
				payload.label(),
				theme::small(),
				theme::TEXT,
			);
		}
		// Ghost clip where a dropped library pattern will land (from its bar).
		if let (Payload::Pattern { id, name }, Some(row)) = (&payload, ghost_row)
			&& accepted
		{
			let library = state.library();
			let meter = state.project().meter();
			let at = meter.bar_floor(target.at().unwrap_or(state.position()));
			let bars = library
				.pattern(id)
				.map_or(4, crate::state::library::drop_bars);
			let end = meter.tick_at_bar_position(meter.bar_position(at) + f64::from(bars));
			let tl = state.view.timeline;
			let x0 = layout.lanes.left() + tl.tick_to_x(at);
			let ghost = Rect::from_min_size(
				pos2(x0, row.top() + 2.0),
				vec2(tl.ticks_to_px(end - at).max(24.0), row.height() - 4.0),
			);
			let gp = ui.painter_at(layout.lanes);
			gp.rect_filled(
				ghost,
				CornerRadius::same(theme::RADIUS),
				theme::ACCENT.gamma_multiply(0.25),
			);
			gp.rect_stroke(
				ghost,
				CornerRadius::same(theme::RADIUS),
				Stroke::new(1.0, theme::ACCENT),
				StrokeKind::Inside,
			);
			gp.text(
				ghost.left_top() + vec2(4.0, 3.0),
				Align2::LEFT_TOP,
				name,
				theme::small(),
				theme::TEXT,
			);
		}
		dnd::set_hint(&ctx, action.describe(state.project()), accepted);
		if in_app.is_some() && ctx.input(|i| i.pointer.any_released()) {
			egui::DragAndDrop::clear_payload(&ctx);
			state.drop_payload(&payload, target, copy);
		}
	}

	fn headers(&mut self, ui: &mut Ui, state: &mut AppState, layout: &Layout) {
		let scroll = state.view.scroll_y;
		let clip = layout.header;
		ui.painter_at(clip).rect_filled(clip, 0.0, theme::BG_PANEL);
		let tracks: Vec<Track> = state.project().tracks.clone();
		for row in self.rows.rows.clone() {
			let y = layout.body_top + row.top - scroll;
			let rect = Rect::from_min_size(pos2(clip.left(), y), vec2(clip.width(), row.height));
			if rect.bottom() < clip.top() || rect.top() > clip.bottom() {
				continue;
			}
			let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect));
			child.set_clip_rect(rect.intersect(clip));
			match &row.kind {
				RowKind::Track { index, .. } => {
					self.header(&mut child, state, &tracks[*index], *index, rect)
				}
				RowKind::Lane { address, .. } => {
					self.auto.lane_header(&mut child, state, address, rect)
				}
				RowKind::Take { track, lane } => {
					super::record::take_header(&mut child, state, *track, *lane, rect)
				}
				RowKind::Master => self.auto.master_header(&mut child, state, rect),
			}
		}
		// Reorder feedback.
		if let Some(d) = &self.track_drag
			&& d.to != d.from
		{
			let row = if d.to > d.from { d.to + 1 } else { d.to };
			let y = self.top(layout, row, scroll);
			ui.painter_at(clip)
				.hline(clip.x_range(), y, Stroke::new(2.0, theme::ACCENT));
		}
		// Add-track buttons under the last track.
		let below = layout.body_top + self.rows.height - scroll + 8.0;
		if below + 24.0 < clip.bottom() && below > clip.top() - 24.0 {
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
			if tracks.is_empty() || below + 52.0 < clip.bottom() {
				ui.painter_at(clip).text(
					pos2(clip.left() + 12.0, below + 36.0),
					Align2::LEFT_CENTER,
					"or drop a device or file here",
					theme::small(),
					theme::TEXT_FAINT,
				);
			}
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
		if track.kind == TrackKind::Audio {
			super::record::header_input_meter(&p, state, track, rect);
		}

		// Folder nesting: indentation and one guide line per enclosing folder.
		let folders = state.project().folders_of(track.id);
		for (k, f) in folders.iter().rev().enumerate() {
			let c = state
				.project()
				.track(*f)
				.map_or(theme::BORDER, |f| theme::track_color(f.color.as_deref()));
			let x = rect.left() + 10.0 + k as f32 * FOLDER_INDENT;
			p.vline(
				x,
				rect.top()..=rect.bottom(),
				Stroke::new(2.0, c.gamma_multiply(0.6)),
			);
		}
		let left = rect.left() + 12.0 + folders.len() as f32 * FOLDER_INDENT;
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
		let auto_rect = mute_rect.translate(vec2(-21.0, 0.0));
		let record_left = super::record::header_buttons(ui, state, track, id, auto_rect);
		let mut name_left = left + 16.0;
		if track.kind == TrackKind::Folder {
			// Collapse / expand toggle before the name.
			let open = !state.view.collapsed.contains(&track.id);
			let r = Rect::from_min_size(pos2(name_left - 2.0, row1), vec2(16.0, 16.0));
			let resp = ui.interact(r, id.with("fold"), Sense::click());
			resp.widget_info(|| {
				egui::WidgetInfo::selected(egui::WidgetType::Checkbox, true, open, "Fold")
			});
			widgets::paint_disclosure(
				&p,
				r.center(),
				open,
				if resp.hovered() {
					Color32::WHITE
				} else {
					theme::TEXT_DIM
				},
			);
			if resp.on_hover_text("Collapse / expand the folder").clicked() {
				state.toggle_folder(track.id);
			}
			name_left += 16.0;
		}
		let name_rect =
			Rect::from_min_max(pos2(name_left, row1), pos2(record_left - 4.0, row1 + 16.0));
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
								input: None,
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
		if track.freeze.is_some() {
			let badge =
				Rect::from_min_size(pos2(name_rect.right() - 30.0, row1 + 2.0), vec2(28.0, 12.0));
			freeze_badge(ui, &p, badge, state, track, id.with("frozen"));
		}
		let ch = &track.channel;
		{
			let open = state.view.automation.is_open(Some(track.id));
			let a = ui.interact(auto_rect, id.with("auto"), Sense::click());
			widgets::paint_toggle(&p, auto_rect, open, "A", theme::AUTOMATION, a.hovered());
			if a.on_hover_text("Show / hide automation lanes (A)")
				.clicked()
			{
				let project = state.project().clone();
				state.view.automation.toggle(&project, Some(track.id));
			}
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
				TrackKind::Folder => {
					let n = state.project().folder_contents(track.id).len();
					format!("Folder · {n} track{}", if n == 1 { "" } else { "s" })
				}
			};
			let instrument_rect =
				Rect::from_min_max(pos2(left, row2), pos2(right - 62.0, row2 + 14.0));
			let ir = ui.interact(instrument_rect, id.with("instrument"), Sense::click());
			if track.kind == TrackKind::Audio {
				super::record::input_controls(ui, state, track, id, instrument_rect);
			} else {
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
			}
			if ir.clicked()
				&& let Some(d) = &track.instrument
			{
				state.selection.select_device(DeviceRef {
					track: Some(track.id),
					device: d.id,
				});
			}
			// Double-click the instrument: its editor window (plugins) or the piano roll.
			if ir.double_clicked() {
				let sampler = track.instrument.as_ref().map(|d| DeviceRef {
					track: Some(track.id),
					device: d.id,
				});
				match &track.instrument {
					Some(d) if state.is_plugin(d.id) => state.toggle_plugin_editor(d.id),
					Some(_) if sampler.is_some_and(|r| state.is_sample_device(r)) => {
						state.toggle_instrument_editor(sampler.expect("checked"));
					}
					_ if track.kind == TrackKind::Instrument => {
						state.open_track_in_piano_roll(track.id);
					}
					_ => {}
				}
			}
			let vol_rect = Rect::from_min_size(pos2(right - 58.0, row2), vec2(58.0, 14.0));
			let (gesture, vol) = widgets::volume_slider(
				ui,
				id.with("volume"),
				state.volume_db(Some(track.id)),
				vol_rect,
				color,
			);
			match gesture {
				Gesture::Preview(v) => state.preview_volume(Some(track.id), v),
				Gesture::Commit(v) => state.commit_volume(Some(track.id), v),
				Gesture::Idle => {}
			}
			let vol_addr = crate::state::volume_address(Some(track.id));
			if state.is_automated(&vol_addr) {
				super::automation::automated_badge(ui, vol_rect);
			}
			vol.on_hover_text("Volume: drag (Shift: fine), double-click: 0 dB")
				.context_menu(|ui| super::automation::automate_item(ui, state, vol_addr));
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
		// Double-click: the name renames, the rest of an instrument header opens the
		// piano roll (creating a clip when the track has none to edit).
		if response.double_clicked() {
			let on_name = response
				.interact_pointer_pos()
				.is_some_and(|p| name_rect.contains(p));
			if track.kind == TrackKind::Instrument && !on_name {
				state.open_track_in_piano_roll(track.id);
			} else {
				self.rename = Some((track.id, track.name.clone()));
			}
		}
		if response.drag_started_by(PointerButton::Primary) {
			self.track_drag = Some(TrackDrag {
				track: track.id,
				from: index,
				to: index,
			});
		}
		if let Some(d) = &mut self.track_drag
			&& d.track == track.id
		{
			if let Some(pos) = ui.ctx().pointer_interact_pos() {
				let n = state.project().tracks.len();
				let own = self.rows.track(index).map_or(0.0, |r| r.top);
				let y = own + pos.y - rect.top();
				d.to = match self.rows.track_at(y) {
					Some((_, i)) => i,
					None if y < 0.0 => 0,
					None => n.saturating_sub(1),
				};
			}
			if response.drag_stopped() || ui.input(|i| i.pointer.any_released()) {
				let d = self.track_drag.take().expect("track drag");
				if d.to != d.from {
					// Folders move with their contents; landing under an open folder
					// files the track into it.
					state.move_track_to(d.track, d.to);
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
		let meter = MeterMap::new(state.project().meter());
		let painter = ui.painter_at(area);
		let tracks: Vec<Track> = state.project().tracks.clone();

		// Row backgrounds: clip lanes alternate, automation lanes are a step darker.
		for row in &self.rows.rows {
			let y = layout.body_top + row.top - scroll;
			let r = Rect::from_min_size(pos2(area.left(), y), vec2(area.width(), row.height));
			if !r.intersects(area) {
				continue;
			}
			let fill = match row.kind {
				RowKind::Track { id, index } => {
					if state.selection.track == Some(id) {
						theme::LANE_SELECTED
					} else if index % 2 == 0 {
						theme::LANE
					} else {
						theme::LANE_ALT
					}
				}
				RowKind::Lane { .. } | RowKind::Take { .. } => theme::AUTO_LANE,
				RowKind::Master => theme::BG_PANEL,
			};
			painter.rect_filled(r, 0.0, fill);
		}
		// The area below the tracks is a little darker: it is a drop zone, not a lane.
		let below = layout.body_top + self.rows.height - scroll;
		if below < area.bottom() {
			painter.rect_filled(
				Rect::from_min_max(pos2(area.left(), below), area.max),
				0.0,
				theme::BG_PANEL,
			);
		}
		let grid_area = Rect::from_min_max(area.min, pos2(area.right(), below.min(area.bottom())));
		grid(&painter, grid_area, &tl, meter.view());
		for row in &self.rows.rows {
			let y = layout.body_top + row.top - scroll;
			let c = if matches!(row.kind, RowKind::Lane { .. }) {
				theme::GRID_BEAT
			} else {
				theme::BORDER
			};
			painter.hline(area.x_range(), y - 0.5, Stroke::new(1.0, c));
		}
		painter.hline(area.x_range(), below - 0.5, Stroke::new(1.0, theme::BORDER));

		// Automation lanes.
		for row in self.rows.rows.clone() {
			let RowKind::Lane { address, .. } = &row.kind else {
				continue;
			};
			let y = layout.body_top + row.top - scroll;
			let r = Rect::from_min_size(pos2(area.left(), y), vec2(area.width(), row.height));
			if r.intersects(area) || self.auto.dragging(address) {
				self.auto.lane_body(ui, state, address, r, area);
			}
		}

		// Take lanes (comping).
		for row in self.rows.rows.clone() {
			let RowKind::Take { track, lane } = row.kind else {
				continue;
			};
			let Some(t) = tracks.iter().find(|t| t.id == track) else {
				continue;
			};
			let y = layout.body_top + row.top - scroll;
			let r = Rect::from_min_size(pos2(area.left(), y), vec2(area.width(), row.height));
			super::record::take_lane(ui, state, t, lane, r, area, &tl);
		}

		// Lane clicks: select / create clips.
		for (i, t) in tracks.iter().enumerate() {
			let y = self.top(layout, i, scroll);
			let r =
				Rect::from_min_size(pos2(area.left(), y), vec2(area.width(), h)).intersect(area);
			if r.height() <= 0.0 {
				continue;
			}
			let resp = ui.interact(r, Id::new(("lane", t.id)), Sense::click_and_drag());
			if resp.drag_started_by(PointerButton::Primary)
				&& let Some(press) = ui.input(|i| i.pointer.press_origin())
			{
				let add = ui.input(|i| i.modifiers.shift);
				self.band = Some((press, press, add));
			}
			if resp.clicked() {
				state.selection.select_track(t.id);
				state.selection.clear_clips();
			}
			if resp.double_clicked()
				&& t.kind == TrackKind::Instrument
				&& let Some(pos) = resp.interact_pointer_pos()
			{
				let tick = tl.x_to_tick(pos.x - area.left());
				let bar = meter.view().bar_at(Tick(tick.0.max(0)));
				if let Some((cmd, id)) =
					edits::new_midi_clip(state.project(), t.id, bar.start, bar.length)
					&& state.apply(cmd)
				{
					state.selection.click_clip(t.id, id, false);
				}
			}
		}

		// Overlapping audio clips: automatic crossfades (drawn as fade curves with a length
		// handle); clips nested in another just sum (marked with an X).
		for (i, t) in tracks.iter().enumerate() {
			let y = self.top(layout, i, scroll);
			if y > area.bottom() || y + h < area.top() {
				continue;
			}
			let xfs = edits::crossfades(state.project(), t);
			let row = |s0: Tick, s1: Tick| {
				Rect::from_min_max(
					pos2(area.left() + tl.tick_to_x(s0), y + 1.0),
					pos2(area.left() + tl.tick_to_x(s1), y + h - 1.0),
				)
			};
			for (s0, s1) in edits::overlaps(t) {
				if !xfs.iter().any(|x| x.start == s0 && x.overlap_end == s1) {
					self.overlaps.push(row(s0, s1));
				}
			}
			for xf in xfs {
				self.xfades.push((row(xf.start, xf.overlap_end), xf));
			}
		}

		// Folder rows: a summary of the clips inside (one thin bar per track).
		for (i, t) in tracks.iter().enumerate() {
			if t.kind != TrackKind::Folder {
				continue;
			}
			let y = self.top(layout, i, scroll);
			if y > area.bottom() || y + h < area.top() {
				continue;
			}
			let inside: Vec<&Track> = state
				.project()
				.folder_contents(t.id)
				.iter()
				.filter_map(|id| tracks.iter().find(|x| x.id == *id))
				.filter(|x| x.kind != TrackKind::Folder)
				.collect();
			if inside.is_empty() {
				continue;
			}
			let band = ((h - 10.0) / inside.len() as f32).clamp(2.0, 8.0);
			for (k, c) in inside.iter().enumerate() {
				let color = theme::track_color(c.color.as_deref());
				let cy = y + 5.0 + k as f32 * band;
				if cy + band > y + h - 2.0 {
					break;
				}
				for clip in c.clips.iter().filter(|c| c.plays()) {
					let x0 = area.left() + tl.tick_to_x(clip.start);
					let x1 = area.left() + tl.tick_to_x(clip.end());
					if x1 < area.left() || x0 > area.right() {
						continue;
					}
					painter.rect_filled(
						Rect::from_min_max(pos2(x0, cy), pos2(x1.max(x0 + 2.0), cy + band - 1.0)),
						CornerRadius::same(1),
						color.gamma_multiply(0.7),
					);
				}
			}
		}

		// Clips.
		for (i, t) in tracks.iter().enumerate() {
			let y = self.top(layout, i, scroll);
			if (y > area.bottom() || y + h < area.top()) && self.drag.is_none() {
				// Clips scrolled out of view still process an in-flight drag.
				continue;
			}
			// Takes that do not play live in the take lanes. Later clips draw on top (what
			// plays after a crossfade is the later clip).
			let mut clips: Vec<&Clip> = t.clips.iter().filter(|c| c.plays()).collect();
			clips.sort_by_key(|c| c.start);
			let xfs = if t.kind == TrackKind::Audio {
				edits::crossfades(state.project(), t)
			} else {
				Vec::new()
			};
			for c in clips {
				self.clip(ui, state, &painter, layout, t, i, c, &xfs);
			}
		}
		for r in std::mem::take(&mut self.overlaps) {
			if !r.intersects(area) {
				continue;
			}
			painter.rect_filled(r, 0.0, Color32::from_black_alpha(70));
			let stroke = Stroke::new(1.0, Color32::from_white_alpha(150));
			painter.line_segment([r.left_bottom(), r.right_top()], stroke);
			painter.line_segment([r.left_top(), r.right_bottom()], stroke);
		}
		self.crossfades(ui, state, &painter, area);
		// The clip being recorded, and take badges.
		for (i, t) in tracks.iter().enumerate() {
			let y = self.top(layout, i, scroll);
			if y > area.bottom() || y + h < area.top() {
				continue;
			}
			let lane = Rect::from_min_size(pos2(area.left(), y), vec2(area.width(), h));
			super::record::paint_track_overlay(&painter, state, t, lane, &tl);
		}
		self.rubber_band(ui, state, layout);
	}

	/// Crossfades: the fade-out / fade-in curves over the crossfade and a handle at the crossfade end that sets its
	/// length (drag; double-click: the whole overlap). One command on release.
	fn crossfades(&mut self, ui: &mut Ui, state: &mut AppState, painter: &Painter, area: Rect) {
		let tl = state.view.timeline;
		let xfades = std::mem::take(&mut self.xfades);
		for (r, xf) in xfades {
			if !r.intersects(area) {
				continue;
			}
			let dragged = self
				.xfade_drag
				.filter(|(d, _)| d.later == xf.later)
				.map(|(_, end)| end);
			let end = dragged.unwrap_or(xf.end);
			let x_end = (area.left() + tl.tick_to_x(end)).clamp(r.left(), r.right());
			let fade = Rect::from_min_max(r.min, pos2(x_end, r.bottom()));
			// The clips under it are drawn as they play (the earlier one shaped by the
			// fade-out and cut at the crossfade end), so only the crossfade itself is marked.
			painter.rect_filled(fade, 0.0, Color32::from_black_alpha(35));
			let curve = crate::state::audio_edit::fade_curve(xf.curve);
			let steps = 24;
			let pts = |out: bool| -> Vec<Pos2> {
				(0..=steps)
					.map(|k| {
						let t = k as f32 / steps as f32;
						let g = if out {
							curve.fade_out(t)
						} else {
							curve.fade_in(t)
						};
						pos2(
							fade.left() + fade.width() * t,
							fade.bottom() - 2.0 - g * (fade.height() - 4.0),
						)
					})
					.collect()
			};
			let stroke = Stroke::new(1.3, Color32::from_white_alpha(190));
			painter.add(Shape::line(pts(true), stroke));
			painter.add(Shape::line(pts(false), stroke));
			// Length handle.
			let handle = Rect::from_center_size(pos2(x_end, r.top() + 6.0), vec2(9.0, 9.0));
			let id = Id::new(("xfade", xf.later));
			let resp = ui
				.interact(handle.expand(2.0), id, Sense::click_and_drag())
				.on_hover_cursor(CursorIcon::ResizeHorizontal)
				.on_hover_text("Crossfade length (drag; double-click: the whole overlap)");
			painter.rect_filled(
				handle,
				CornerRadius::same(2),
				if resp.hovered() || resp.dragged() {
					Color32::WHITE
				} else {
					theme::TEXT_DIM
				},
			);
			if resp.dragged()
				&& let Some(pos) = resp.interact_pointer_pos()
			{
				let t = tl.x_to_tick(pos.x - area.left());
				let t = if state.view.snap && !ui.input(|i| i.modifiers.alt) {
					tl.snap(t, state.project().meter())
				} else {
					t
				};
				self.xfade_drag = Some((xf, t.clamp(xf.start + Tick(1), xf.overlap_end)));
			}
			if resp.drag_stopped()
				&& let Some((d, end)) = self.xfade_drag.take()
				&& let Some(cmd) = edits::set_crossfade(state.project(), &d, Some(end))
			{
				state.apply(cmd);
			}
			if resp.double_clicked()
				&& xf.custom && let Some(cmd) = edits::set_crossfade(state.project(), &xf, None)
			{
				state.apply(cmd);
			}
		}
	}

	/// Rubber band over empty lane area: selects the clips it touches (Shift adds).
	fn rubber_band(&mut self, ui: &Ui, state: &mut AppState, layout: &Layout) {
		let Some((start, _, add)) = self.band else {
			return;
		};
		let Some(pos) = ui.ctx().pointer_interact_pos() else {
			return;
		};
		self.band = Some((start, pos, add));
		let r = Rect::from_two_pos(start, pos);
		let p = ui.painter_at(layout.lanes);
		p.rect_filled(r, 0.0, theme::ACCENT.gamma_multiply(0.12));
		p.rect_stroke(r, 0.0, Stroke::new(1.0, theme::ACCENT), StrokeKind::Inside);
		if !ui.input(|i| i.pointer.any_released()) {
			return;
		}
		self.band = None;
		let tl = state.view.timeline;
		let t0 = tl.x_to_tick(r.left() - layout.lanes.left());
		let t1 = tl.x_to_tick(r.right() - layout.lanes.left());
		let (y0, y1) = (
			Self::content_y(layout, state, r.top()),
			Self::content_y(layout, state, r.bottom()),
		);
		let tracks: Vec<TrackId> = self
			.rows
			.rows
			.iter()
			.filter(|row| row.bottom() > y0 && row.top < y1)
			.filter_map(|row| match row.kind {
				RowKind::Track { id, .. } => Some(id),
				_ => None,
			})
			.collect();
		let found = edits::clips_in(state.project(), &tracks, t0, t1);
		if !add {
			state.selection.clear_clips();
		}
		state.selection.clips.extend(found);
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
		xfs: &[edits::Crossfade],
	) {
		let area = layout.lanes;
		let h = state.view.track_height;
		let tl = state.view.timeline;
		let meter = MeterMap::new(state.project().meter());
		let y = self.top(layout, index, state.view.scroll_y);
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
			let near_edge = r.hover_pos().is_some_and(|p| {
				p.x > rect.right() - RESIZE_HANDLE || p.x < rect.left() + RESIZE_HANDLE
			});
			if near_edge
				|| self
					.drag
					.as_ref()
					.is_some_and(|d| d.anchor == clip.id && d.kind != DragKind::Move)
			{
				ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
			} else if self.drag.as_ref().is_some_and(|d| d.copy) {
				ui.ctx().set_cursor_icon(CursorIcon::Copy);
			} else if r.hovered() && !self.pan {
				ui.ctx().set_cursor_icon(CursorIcon::Grab);
			}
			// Pressing an unselected clip selects it; dragging a selected one keeps the group.
			let add = ui.input(|i| i.modifiers.shift || i.modifiers.command);
			let drag_started = r.drag_started_by(PointerButton::Primary);
			if r.clicked() && !add {
				// A plain click (no drag) narrows a group selection to this clip.
				state.selection.clear_clips();
			}
			if r.clicked() || (drag_started && !state.selection.clips.contains(&clip.id)) {
				state
					.selection
					.click_clip(track.id, clip.id, add && !drag_started);
			}
			if r.double_clicked() {
				state.open_editor(clip.id);
			}
			if drag_started
				&& let Some(press) = ui
					.input(|i| i.pointer.press_origin())
					.or(r.interact_pointer_pos())
			{
				let kind = if press.x > rect.right() - RESIZE_HANDLE {
					DragKind::Resize
				} else if press.x < rect.left() + RESIZE_HANDLE
					&& rect.width() > 3.0 * RESIZE_HANDLE
				{
					DragKind::TrimStart
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
					origin: if kind == DragKind::Resize {
						clip.end()
					} else {
						clip.start
					},
					press: tl.x_to_tick(press.x - area.left()),
					delta: Tick::ZERO,
					target: None,
					copy: false,
				});
			}
			r.context_menu(|ui| clip_menu(ui, state, track.id, clip));
		}

		// Update an active drag anchored on this clip.
		if let Some(d) = &mut self.drag
			&& d.anchor == clip.id
		{
			if let Some(pos) = ui.ctx().pointer_interact_pos() {
				let now = tl.x_to_tick(pos.x - area.left());
				let raw = d.origin + (now - d.press);
				let (alt, ctrl) = ui.input(|i| (i.modifiers.alt, i.modifiers.command));
				let snapped = if state.view.snap && !alt {
					tl.snap(raw, meter.view())
				} else {
					Tick(raw.0.max(0))
				};
				d.delta = snapped - d.origin;
				d.copy = d.kind == DragKind::Move && ctrl;
				if d.kind == DragKind::Move {
					let y = Self::content_y(layout, state, pos.y);
					let tracks = &state.project().tracks;
					d.target = self
						.rows
						.track_at(y)
						.and_then(|(_, i)| tracks.get(i))
						.filter(|t| t.id != track.id && edits::compatible(track.kind, t.kind))
						.map(|t| t.id);
				}
			}
			let released = ui.input(|i| i.pointer.any_released())
				|| response.as_ref().is_some_and(|r| r.drag_stopped());
			if released {
				let d = self.drag.take().expect("clip drag");
				match d.kind {
					DragKind::Move if d.copy => {
						if let Some((cmd, ids)) =
							edits::copy_clips(state.project(), &d.clips, d.delta, d.target)
							&& state.apply(cmd)
						{
							state.selection.clips = ids.into_iter().collect();
						}
					}
					DragKind::Move => {
						if let Some(cmd) =
							edits::move_clips(state.project(), &d.clips, d.delta, d.target)
						{
							state.apply(cmd);
						}
					}
					DragKind::Resize => {
						if let Some(cmd) =
							edits::resize_clip(state.project(), d.anchor, d.origin + d.delta)
						{
							state.apply(cmd);
						}
					}
					DragKind::TrimStart => {
						if let Some(cmd) =
							edits::trim_start(state.project(), d.anchor, d.origin + d.delta)
						{
							state.apply(cmd);
						}
					}
				}
				return;
			}
		}

		// Ghost position while dragging.
		let mut draw_rect = rect;
		let mut ghost = false;
		let mut copying = false;
		if let Some(d) = &self.drag
			&& d.clips.contains(&clip.id)
		{
			ghost = true;
			copying = d.copy;
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
						let dy = self.rows.track_top(row) - self.rows.track_top(index);
						draw_rect = draw_rect.translate(vec2(0.0, dy));
					}
				}
				DragKind::Resize => {
					let end = area.left() + tl.tick_to_x(d.origin + d.delta);
					draw_rect.max.x = end.max(draw_rect.min.x + 4.0);
				}
				DragKind::TrimStart => {
					let start = area.left() + tl.tick_to_x(d.origin + d.delta);
					draw_rect.min.x = start.min(draw_rect.max.x - 4.0);
				}
			}
		}
		let selected = state.selection.clips.contains(&clip.id);
		let editing = state
			.view
			.panels
			.visible(crate::state::layout::PanelKind::PianoRoll)
			&& if state.view.audio_editor {
				state.view.audio.clip == Some(clip.id)
			} else {
				state.view.piano.clip == Some(clip.id)
			};
		if copying {
			// The original stays; the copy is drawn at the destination.
			if rect.intersects(area) {
				paint_clip(
					painter, state, track, clip, rect, selected, editing, area, xfs,
				);
			}
		} else if ghost && rect.intersects(area) {
			painter.rect_stroke(
				rect,
				CornerRadius::same(theme::RADIUS),
				Stroke::new(1.0, theme::TEXT_FAINT),
				StrokeKind::Inside,
			);
		}
		if !draw_rect.intersects(area) {
			return;
		}
		// A clip being moved or resized no longer crossfades where it was drawn.
		let xfs = if draw_rect == rect { xfs } else { &[] };
		paint_clip(
			painter, state, track, clip, draw_rect, selected, editing, area, xfs,
		);
		if copying {
			painter.text(
				draw_rect.right_top() + vec2(-5.0, 2.0),
				Align2::RIGHT_TOP,
				"+",
				FontId::proportional(13.0),
				Color32::WHITE,
			);
		}
	}
}

/// The FRZ badge of a frozen track (header and mixer strip). Out of date (the clips or
/// devices changed after freezing) it turns amber and a click freezes the track again.
pub(super) fn freeze_badge(
	ui: &mut Ui,
	p: &Painter,
	badge: Rect,
	state: &mut AppState,
	track: &Track,
	id: Id,
) {
	let stale = state.document().freeze_stale(track.id);
	let busy = state.bounce_busy();
	let resp = ui.interact(
		badge,
		id,
		if stale && !busy {
			Sense::click()
		} else {
			Sense::hover()
		},
	);
	let label = if stale {
		format!("Refreeze {}", track.name)
	} else {
		format!("{} is frozen", track.name)
	};
	resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, !busy, &label));
	let fill = if stale { theme::WARNING } else { theme::FROZEN };
	p.rect_filled(
		badge,
		CornerRadius::same(2),
		if resp.hovered() && stale {
			theme::mix(fill, Color32::WHITE, 0.25)
		} else {
			fill
		},
	);
	p.text(
		badge.center(),
		Align2::CENTER_CENTER,
		if stale { "FRZ!" } else { "FRZ" },
		FontId::monospace(8.5),
		theme::BG_DEEP,
	);
	if stale {
		if resp
			.on_hover_text(
				"Frozen render out of date: the clips or devices changed after freezing. \
				 Click to refreeze (track menu: Unfreeze)",
			)
			.clicked()
		{
			state.freeze_track(track.id);
		}
	} else {
		resp.on_hover_text(
			"Frozen: the track plays its render; its devices are unloaded (track menu: Unfreeze)",
		);
	}
}

/// Zooms (and scrolls) the arrange view to the whole song.
pub fn zoom_to_fit(state: &mut AppState, width: f32) {
	let end = state.song_end().beats();
	state.view.timeline.zoom_to_range(0.0, end, width);
	state.view.timeline.scroll_beats = 0.0;
}

/// Zooms to the selected clips (time) and scrolls their first track into view; without
/// a selection, zooms to fit.
pub fn zoom_to_selection(state: &mut AppState, width: f32, height: f32) {
	let p = state.project();
	let rows = Rows::new(p, &state.view);
	let found: Vec<(usize, Tick, Tick)> = state
		.selection
		.clips
		.iter()
		.filter_map(|c| {
			let (t, clip) = p.find_clip(*c)?;
			Some((p.track_index(t.id)?, clip.start, clip.end()))
		})
		.collect();
	let (Some(start), Some(end), Some(row)) = (
		found.iter().map(|f| f.1).min(),
		found.iter().map(|f| f.2).max(),
		found.iter().map(|f| f.0).min(),
	) else {
		zoom_to_fit(state, width);
		return;
	};
	state
		.view
		.timeline
		.zoom_to_range(start.beats(), end.beats(), width);
	let h = state.view.track_height;
	let top = rows.track_top(row);
	if top < state.view.scroll_y || top + h > state.view.scroll_y + height {
		state.view.scroll_y = (top - h * 0.5).max(0.0);
	}
}

/// Song scrollbar under the lanes and track scrollbar to their right.
fn scrollbars(ui: &mut Ui, state: &mut AppState, layout: &Layout, full: Rect) {
	let tl = state.view.timeline;
	let visible = tl.visible_beats(layout.lanes.width());
	let total = timeline::content_beats(state.song_end().beats(), tl.scroll_beats, visible);
	ui.painter().rect_filled(
		Rect::from_min_max(pos2(full.left(), layout.hbar.top()), full.max),
		0.0,
		theme::BG_PANEL,
	);
	if let Some(v) = widgets::scrollbar(
		ui,
		Id::new("arrange-hbar"),
		layout.hbar,
		false,
		total,
		visible,
		tl.scroll_beats,
	) {
		state.view.timeline.scroll_beats = v;
	}
	ui.painter().rect_filled(layout.vbar, 0.0, theme::BG_PANEL);
	let content = f64::from(content_height(state));
	if let Some(v) = widgets::scrollbar(
		ui,
		Id::new("arrange-vbar"),
		layout.vbar,
		true,
		content.max(f64::from(layout.lanes.height())),
		f64::from(layout.lanes.height()),
		f64::from(state.view.scroll_y),
	) {
		state.view.scroll_y = v as f32;
	}
}

/// Follow-playhead mode selector (Off / Page / Continuous).
pub fn follow_menu(ui: &mut Ui, state: &mut AppState) {
	let mode = state.view.follow;
	egui::ComboBox::from_id_salt("follow-mode")
		.width(78.0)
		.selected_text(
			egui::RichText::new(format!("Follow: {}", mode.name()))
				.font(theme::small())
				.color(if mode == FollowMode::Off {
					theme::TEXT_FAINT
				} else {
					theme::TEXT_DIM
				}),
		)
		.show_ui(ui, |ui| {
			for m in [FollowMode::Off, FollowMode::Page, FollowMode::Continuous] {
				ui.selectable_value(&mut state.view.follow, m, m.name());
			}
		})
		.response
		.on_hover_text("Follow the playhead while playing: off, page by page, or continuously");
}

pub(crate) fn grid_name(step: Tick, bar: Tick) -> String {
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
		TrackKind::Folder => "",
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
		("+ Folder", TrackKind::Folder),
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
	if track.kind == TrackKind::Instrument {
		if ui
			.add(egui::Button::new("Open in piano roll").shortcut_text("F7"))
			.clicked()
		{
			state.open_track_in_piano_roll(track.id);
			ui.close();
		}
		if ui.button("New clip in piano roll").clicked() {
			let at = state.position();
			state.new_clip_in_piano_roll(track.id, at);
			ui.close();
		}
		ui.separator();
	}
	if ui.button("Rename").clicked() {
		*rename = Some((track.id, track.name.clone()));
		ui.close();
	}
	if crate::state::bounce::can_freeze(track) {
		let busy = state.bounce_busy();
		if track.freeze.is_some() {
			if state.document().freeze_stale(track.id)
				&& ui
					.add_enabled(!busy, egui::Button::new("Refreeze (render is out of date)"))
					.clicked()
			{
				state.freeze_track(track.id);
				ui.close();
			}
			if ui.button("Unfreeze").clicked() {
				state.unfreeze_track(track.id);
				ui.close();
			}
		} else if ui
			.add_enabled(!busy, egui::Button::new("Freeze (render, unload devices)"))
			.clicked()
		{
			state.freeze_track(track.id);
			ui.close();
		}
		if ui
			.add_enabled(!busy, egui::Button::new("Bounce in place"))
			.on_hover_text("Render the track to a new audio track and mute this one")
			.clicked()
		{
			state.bounce_in_place(track.id);
			ui.close();
		}
		ui.separator();
	}
	if ui.button("Group into new folder").clicked() {
		state.group_into_folder(&[track.id]);
		ui.close();
	}
	if track.folder.is_some() && ui.button("Take out of folder").clicked() {
		state.take_out_of_folder(track.id);
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
						input: None,
					},
				});
				ui.close();
			}
		}
	});
	ui.separator();
	let open = state.view.automation.is_open(Some(track.id));
	if ui
		.add(
			egui::Button::new(if open {
				"Hide automation"
			} else {
				"Show automation"
			})
			.shortcut_text("A"),
		)
		.clicked()
	{
		let project = state.project().clone();
		state.view.automation.toggle(&project, Some(track.id));
		ui.close();
	}
	ui.menu_button("Automate", |ui| {
		let choices = state.lane_choices(Some(track.id));
		egui::ScrollArea::vertical()
			.max_height(360.0)
			.show(ui, |ui| {
				for c in choices {
					if ui.button(format!("{} · {}", c.group, c.name)).clicked() {
						state.automate(c.address);
						ui.close();
					}
				}
			});
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

fn clip_menu(ui: &mut Ui, state: &mut AppState, track: TrackId, clip: &Clip) {
	if !state.selection.clips.contains(&clip.id) {
		state.selection.click_clip(track, clip.id, false);
	}
	if clip.is_midi() {
		if ui
			.add(egui::Button::new("Open in piano roll").shortcut_text("Double-click"))
			.clicked()
		{
			state.open_piano_roll(clip.id);
			ui.close();
		}
		ui.separator();
	} else {
		if ui
			.add(egui::Button::new("Open in audio editor").shortcut_text("Double-click"))
			.clicked()
		{
			state.open_audio_editor(clip.id);
			ui.close();
		}
		ui.separator();
	}
	if ui.button("Duplicate      Ctrl+D").clicked() {
		super::duplicate_selection(state);
		ui.close();
	}
	if ui.button("Split at playhead   S").clicked() {
		super::split_at_playhead(state);
		ui.close();
	}
	if ui
		.add_enabled(
			state.selection.clips.len() > 1,
			egui::Button::new("Consolidate selected clips"),
		)
		.on_disabled_hover_text("Select two or more clips of one track")
		.clicked()
	{
		state.consolidate_selection();
		ui.close();
	}
	if ui.button("Delete            Del").clicked() {
		super::delete_selection(state);
		ui.close();
	}
}

/// Vertical grid lines following the time signatures (bars strong, beats and
/// subdivisions fainter).
pub(crate) fn grid(p: &Painter, area: Rect, tl: &Timeline, meter: cadenza_core::Meter<'_>) {
	let (from, to) = (tl.x_to_tick(0.0), tl.x_to_tick(area.width()));
	for line in tl.grid_lines(meter, from, to, None) {
		let x = area.left() + tl.tick_to_x(line.tick);
		let c = match line.kind {
			LineKind::Bar(_) => theme::GRID_BAR,
			LineKind::Beat => theme::GRID_BEAT,
			LineKind::Step => theme::GRID_FINE,
		};
		p.vline(x, area.y_range(), Stroke::new(1.0, c));
	}
}

/// Ruler: bar numbers; click/drag the lower half moves the playhead, drag the upper half
/// (or Ctrl+drag anywhere) sets the loop region, a click on the region toggles the loop.
fn ruler(ui: &mut Ui, state: &mut AppState, layout: &Layout, loop_drag: &mut Option<Tick>) {
	let r = layout.ruler;
	let tl = state.view.timeline;
	let meter = MeterMap::new(state.project().meter());
	let p = ui.painter_at(r);
	p.rect_filled(r, 0.0, theme::BG_RAISED);
	// Bar numbers: every bar unless too dense (then every 2^n bars).
	let (from, to) = (tl.x_to_tick(0.0), tl.x_to_tick(r.width()));
	let shortest = meter
		.view()
		.segments_between(Tick(from.0.max(0)), to)
		.map(|(_, ts)| ts.ticks_per_bar())
		.min()
		.unwrap_or(Tick(PPQ * 4));
	let every =
		((48.0 / tl.ticks_to_px(shortest)).ceil().max(1.0) as u64).next_power_of_two() as i64;
	for line in tl.grid_lines(meter.view(), from, to, None) {
		let x = r.left() + tl.tick_to_x(line.tick);
		match line.kind {
			LineKind::Bar(index) => {
				p.vline(
					x,
					r.top() + 10.0..=r.bottom(),
					Stroke::new(1.0, theme::TEXT_FAINT),
				);
				if index % every == 0 {
					p.text(
						pos2(x + 4.0, r.top() + 3.0),
						Align2::LEFT_TOP,
						format!("{}", index + 1),
						theme::mono(10.5),
						theme::TEXT_DIM,
					);
				}
			}
			LineKind::Beat | LineKind::Step => {
				let h = if line.kind == LineKind::Beat {
					5.0
				} else {
					3.0
				};
				p.vline(
					x,
					r.bottom() - h..=r.bottom(),
					Stroke::new(1.0, theme::GRID_BAR),
				);
			}
		}
	}
	// Time signature changes: a small tag at their bar line.
	for (_, tick, ts) in meter.view().change_ticks() {
		let x = r.left() + tl.tick_to_x(tick);
		if (r.left() - 40.0..=r.right()).contains(&x) {
			p.text(
				pos2(x + 4.0, r.bottom() - 2.0),
				Align2::LEFT_BOTTOM,
				format!("{}/{}", ts.numerator, ts.denominator),
				theme::small(),
				theme::SIGNATURE,
			);
		}
	}
	p.hline(
		r.x_range(),
		r.bottom() - 0.5,
		Stroke::new(1.0, theme::BORDER),
	);
	let band = Rect::from_min_max(r.min, pos2(r.right(), r.center().y));
	paint_loop_band(&p, state, r, &tl);
	let resp = ui.interact(r, Id::new("ruler"), Sense::click_and_drag());
	let (ctrl, alt) = ui.input(|i| (i.modifiers.command, i.modifiers.alt));
	let hover_band = resp.hover_pos().is_some_and(|p| band.contains(p) || ctrl);
	let resp = resp.on_hover_cursor(if hover_band {
		CursorIcon::ResizeHorizontal
	} else {
		CursorIcon::PointingHand
	});
	let tick_at = |x: f32| tl.x_to_tick(x - r.left());
	let step = (state.view.snap && !alt).then(|| tl.grid_at(state.position(), meter.view()).step);
	if resp.drag_started()
		&& let Some(press) = ui.input(|i| i.pointer.press_origin())
		&& (band.contains(press) || ctrl)
	{
		*loop_drag = Some(tick_at(press.x));
	}
	if let Some(from) = *loop_drag {
		if let Some(pos) = resp.interact_pointer_pos() {
			let range = crate::state::transport::loop_from_drag(from, tick_at(pos.x), step);
			if range.is_some() && range != state.view.transport.loop_range {
				state.set_loop_range(range);
			}
		}
		if !resp.dragged() {
			*loop_drag = None;
		}
		return;
	}
	if resp.clicked()
		&& let Some(pos) = resp.interact_pointer_pos()
		&& band.contains(pos)
		&& let Some((s, e)) = state.view.transport.loop_range
		&& (s..e).contains(&tick_at(pos.x))
	{
		state.toggle_loop();
		return;
	}
	if (resp.clicked() || resp.dragged())
		&& let Some(pos) = resp.interact_pointer_pos()
	{
		let tick = tick_at(pos.x);
		let tick = if state.view.snap && !alt {
			tl.snap(tick, meter.view())
		} else {
			tick
		};
		state.seek(tick);
	}
}

/// The loop region on the ruler's upper half (dim while off), and in pattern mode the
/// looped clip span along the ruler's bottom edge.
fn paint_loop_band(p: &Painter, state: &AppState, r: Rect, tl: &Timeline) {
	if let Some((s, e)) = state.view.transport.loop_range {
		let on = state.view.transport.loop_on;
		let x0 = r.left() + tl.tick_to_x(s);
		let x1 = r.left() + tl.tick_to_x(e);
		let band = Rect::from_min_max(pos2(x0, r.top() + 1.0), pos2(x1, r.center().y));
		let color = if on {
			theme::LOOP
		} else {
			theme::LOOP.gamma_multiply(0.35)
		};
		p.rect_filled(band, CornerRadius::same(2), color.gamma_multiply(0.45));
		p.rect_stroke(
			band,
			CornerRadius::same(2),
			Stroke::new(1.0, color),
			StrokeKind::Inside,
		);
	}
	let config = state.transport_config().0;
	if let (Some(_), Some((s, e))) = (config.pattern, config.loop_range) {
		let x0 = r.left() + tl.tick_to_x(s);
		let x1 = r.left() + tl.tick_to_x(e);
		p.rect_filled(
			Rect::from_min_max(pos2(x0, r.bottom() - 4.0), pos2(x1, r.bottom())),
			0.0,
			theme::PATTERN,
		);
	}
}

/// The active loop region over the lanes: tinted, with its edges.
fn loop_overlay(ui: &Ui, state: &AppState, layout: &Layout) {
	let t = &state.view.transport;
	let Some((s, e)) = t.loop_range.filter(|_| t.loop_on) else {
		return;
	};
	if state.transport_config().0.pattern.is_some() {
		return;
	}
	let tl = &state.view.timeline;
	let area = Rect::from_min_max(
		pos2(layout.lanes.left(), layout.ruler.bottom()),
		layout.lanes.max,
	);
	let x0 = area.left() + tl.tick_to_x(s);
	let x1 = area.left() + tl.tick_to_x(e);
	let p = ui.painter_at(area);
	p.rect_filled(
		Rect::from_min_max(pos2(x0, area.top()), pos2(x1, area.bottom())),
		0.0,
		theme::LOOP.gamma_multiply(0.06),
	);
	for x in [x0, x1] {
		p.vline(
			x,
			area.y_range(),
			Stroke::new(1.0, theme::LOOP.gamma_multiply(0.6)),
		);
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
		let tick = tl.snap(tl.x_to_tick(pos.x - r.left()), state.project().meter());
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
	editing: bool,
	area: Rect,
	xfs: &[edits::Crossfade],
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
			ClipContent::Midi { notes, .. } => {
				paint_notes(&p, clip, notes, content, area, color, &state.view.timeline)
			}
			ClipContent::Audio {
				file,
				offset_seconds,
				gain_db,
				fade_in,
				fade_out,
			} => {
				// Draw what plays: crossfaded clips are shaped by the crossfade gain and the
				// earlier clip stops at the crossfade end.
				let peaks = state.peaks.get(file);
				let early_peaks: Vec<(edits::Crossfade, Peaks)> = xfs
					.iter()
					.filter(|x| x.later == clip.id)
					.filter_map(|xf| {
						let (_, early) = state.project().find_clip(xf.earlier)?;
						let ClipContent::Audio { file, .. } = &early.content else {
							return None;
						};
						let file = file.clone();
						Some((*xf, state.peaks.get(&file)))
					})
					.collect();
				let project = state.project();
				let gain = |t: Tick| edits::crossfade_gain(project, xfs, clip.id, t);
				paint_waveform(
					&p,
					project,
					clip,
					&peaks,
					*offset_seconds,
					*gain_db,
					content,
					area,
					theme::mix(color, Color32::WHITE, 0.25),
					&gain,
				);
				// Under the later clip of a crossfade: the earlier one fading out.
				for (xf, peaks) in &early_peaks {
					let Some((_, early)) = project.find_clip(xf.earlier) else {
						continue;
					};
					let ClipContent::Audio {
						offset_seconds,
						gain_db,
						..
					} = &early.content
					else {
						continue;
					};
					if !matches!(peaks, Peaks::Ready(_)) {
						continue;
					}
					let px_per_tick = content.width() / clip.length.0.max(1) as f32;
					let left = content.left() + (early.start.0 - clip.start.0) as f32 * px_per_tick;
					let early_content = Rect::from_min_max(
						pos2(left, content.top()),
						pos2(left + early.length.0 as f32 * px_per_tick, content.bottom()),
					);
					let gain = |t: Tick| {
						(t < xf.end)
							.then(|| edits::crossfade_gain(project, xfs, early.id, t))
							.flatten()
					};
					paint_waveform(
						&p,
						project,
						early,
						peaks,
						*offset_seconds,
						*gain_db,
						early_content,
						area.intersect(content),
						theme::mix(color, Color32::WHITE, 0.6).gamma_multiply(0.55),
						&gain,
					);
				}
				let crossfaded_in = xfs.iter().any(|x| x.later == clip.id);
				paint_fades(
					&p,
					project,
					clip,
					fade_in.filter(|_| !crossfaded_in).map(|f| f.seconds),
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
	if editing {
		// Open in the piano roll: a small pencil badge in the header.
		p.text(
			pos2(rect.right() - 5.0, header.center().y),
			Align2::RIGHT_CENTER,
			"✏",
			FontId::proportional(10.0),
			theme::text_on(color),
		);
	}
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
	wave_color: Color32,
	gain_at: &dyn Fn(Tick) -> Option<f32>,
) {
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
		let tick =
			clip.start + Tick::from_beats(f64::from(x + 0.5 - content.left()) / f64::from(ppb));
		let Some(shape) = gain_at(tick) else {
			x += 1.0;
			continue;
		};
		let gain = gain * shape;
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
