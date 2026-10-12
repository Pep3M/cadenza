//! Piano roll: edits the notes of one MIDI clip (opened by double-clicking it, F7
//! toggles the panel). Vertical keyboard (click to hear), note grid on the song's time
//! axis with the clip boundaries, scale highlighting, ghost notes of other clips and a
//! velocity lane. Every gesture is previewed locally and committed as one command
//! (see [`crate::state::piano`]); notes are auditioned through the track's instrument,
//! also while the transport is stopped.

use std::collections::{BTreeSet, HashMap};

use cadenza_core::{NoteId, PPQ, Tick, TrackId};
use cadenza_model::{Clip, Note};
use cadenza_theory::{PitchClass, Scale, ScaleKind};
use egui::{
	Align2, Color32, CornerRadius, CursorIcon, FontId, Id, Key, KeyboardShortcut, Modifiers,
	Painter, PointerButton, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, Ui, pos2, vec2,
};

use crate::state::AppState;
use crate::state::piano::{self, Ghosts, MIN_NOTE_LENGTH, PianoView, Tool};
use crate::state::timeline;
use crate::theme;
use crate::ui::arrange;
use crate::ui::widgets;

const TOOLBAR: f32 = 30.0;
const RULER: f32 = 22.0;
const VELOCITY_LANE: f32 = 78.0;
/// Pixels at a note's end that resize instead of moving.
const EDGE: f32 = 6.0;

#[derive(Debug, Clone)]
enum Gesture {
	/// A new note: start (relative to the clip), pitch, current end (relative).
	Draw {
		start: Tick,
		pitch: u8,
		end: Tick,
		moved: bool,
	},
	Move {
		ids: BTreeSet<NoteId>,
		anchor_start: Tick,
		press_tick: Tick,
		press_pitch: u8,
		dt: Tick,
		dp: i32,
		moved: bool,
		/// The pressed note.
		note: NoteId,
		/// Shift/Ctrl held at the press.
		add: bool,
		/// The pressed note was already selected.
		was_selected: bool,
	},
	Resize {
		ids: BTreeSet<NoteId>,
		anchor_end: Tick,
		press_tick: Tick,
		delta: Tick,
	},
	Rubber {
		from: (Tick, f32),
		to: Pos2,
		add: bool,
	},
}

#[derive(Debug, Clone)]
struct VelocityDrag {
	values: HashMap<NoteId, f32>,
	/// Relative edit of the whole selection, anchored on this note's original velocity.
	relative: Option<(f32, f32)>,
}

/// View-local state of the piano roll.
#[derive(Default)]
pub struct PianoUi {
	gesture: Option<Gesture>,
	velocity: Option<VelocityDrag>,
	/// Key held on the keyboard (auditioning).
	key: Option<u8>,
	/// Pitch sounding for the current note gesture.
	sounding: Option<u8>,
	pan: bool,
	/// Grid width/height of the last frame (keyboard navigation, framing).
	width: f32,
	height: f32,
	/// Note grid of the last frame (tests drive the mouse with it).
	pub grid_rect: Option<Rect>,
}

#[derive(Debug, Clone, Copy)]
struct Geometry {
	toolbar: Rect,
	keys: Rect,
	ruler: Rect,
	grid: Rect,
	velocity: Option<Rect>,
	hbar: Rect,
	vbar: Rect,
}

impl Geometry {
	fn new(full: Rect, velocity_lane: bool) -> Self {
		let x0 = full.left() + theme::KEYBOARD_WIDTH;
		let right = full.right() - theme::SCROLLBAR;
		let bottom = full.bottom() - theme::SCROLLBAR;
		let top = full.top() + TOOLBAR + RULER;
		let vel_h = if velocity_lane {
			VELOCITY_LANE.min((bottom - top) * 0.35)
		} else {
			0.0
		};
		let grid_bottom = bottom - vel_h;
		Self {
			toolbar: Rect::from_min_max(full.min, pos2(full.right(), full.top() + TOOLBAR)),
			keys: Rect::from_min_max(pos2(full.left(), top), pos2(x0, grid_bottom)),
			ruler: Rect::from_min_max(pos2(x0, full.top() + TOOLBAR), pos2(right, top)),
			grid: Rect::from_min_max(pos2(x0, top), pos2(right, grid_bottom)),
			velocity: velocity_lane
				.then(|| Rect::from_min_max(pos2(x0, grid_bottom), pos2(right, bottom))),
			hbar: Rect::from_min_max(pos2(x0, bottom), pos2(right, full.bottom())),
			vbar: Rect::from_min_max(pos2(right, top), pos2(full.right(), grid_bottom)),
		}
	}
}

/// The clip being edited, with its track id and color.
struct Target {
	track: TrackId,
	color: Color32,
	clip: Clip,
}

fn target(state: &AppState) -> Option<Target> {
	let (track, clip) = state.project().find_clip(state.view.piano.clip?)?;
	clip.is_midi().then(|| Target {
		track: track.id,
		color: theme::track_color(track.color.as_deref()),
		clip: clip.clone(),
	})
}

/// Grid step for snapping (None: snap off or Alt held).
fn step(state: &AppState, ui: &Ui) -> Option<Tick> {
	let free = ui.input(|i| i.modifiers.alt);
	(state.view.snap && !free).then(|| {
		state
			.view
			.piano
			.timeline
			.grid(state.project().time_signature)
			.step
	})
}

impl PianoUi {
	pub fn show(&mut self, ui: &mut Ui, state: &mut AppState) {
		let full = ui.available_rect_before_wrap();
		ui.allocate_rect(full, Sense::hover());
		ui.painter().rect_filled(full, 0.0, theme::BG_DEEP);
		let g = Geometry::new(full, state.view.piano.velocity_lane);
		self.width = g.grid.width();
		self.height = g.grid.height();
		self.grid_rect = Some(g.grid);
		let Some(t) = target(state) else {
			self.toolbar(ui, state, &g, None);
			empty(ui, full.with_min_y(g.toolbar.bottom()));
			return;
		};
		if state.view.piano.framed != Some(t.clip.id) {
			state
				.view
				.piano
				.frame(&t.clip, g.grid.width(), g.grid.height());
		}
		self.navigate(ui, state, &g);
		if state.is_playing() && self.gesture.is_none() {
			state.view.piano.timeline.follow_mode(
				state.view.follow,
				state.position().beats(),
				g.grid.width(),
			);
		}
		let v = &mut state.view.piano;
		v.scroll_y = timeline::clamp_scroll(v.scroll_y, v.content_height(), g.grid.height());

		self.grid(ui, state, &g, &t);
		self.keyboard(ui, state, &g, &t);
		if let Some(r) = g.velocity {
			self.velocity_lane(ui, state, r, &g, &t);
		}
		ruler(ui, state, &g, &t);
		self.scrollbars(ui, state, &g, &t);
		self.toolbar(ui, state, &g, Some(&t));
		// Playhead.
		let x = g.grid.left() + state.view.piano.timeline.tick_to_x(state.position());
		if (g.grid.left()..=g.grid.right()).contains(&x) {
			let bottom = g.velocity.map_or(g.grid.bottom(), |r| r.bottom());
			ui.painter()
				.vline(x, g.ruler.top()..=bottom, Stroke::new(1.5, theme::PLAYHEAD));
		}
	}

	/// Wheel, Ctrl+wheel (zoom at the pointer), Alt+wheel (key height), middle-drag.
	fn navigate(&mut self, ui: &Ui, state: &mut AppState, g: &Geometry) {
		let area = Rect::from_min_max(
			g.keys.min,
			pos2(
				g.grid.right(),
				g.velocity.map_or(g.grid.bottom(), |r| r.bottom()),
			),
		)
		.union(g.ruler);
		let hover = ui.ctx().pointer_hover_pos();
		let inside = hover.is_some_and(|p| area.contains(p)) && ui.ui_contains_pointer();
		let v = &mut state.view.piano;
		if inside && let Some(p) = hover {
			let (zoom, scroll, alt) =
				ui.input(|i| (i.zoom_delta(), i.smooth_scroll_delta, i.modifiers.alt));
			if zoom != 1.0 {
				v.timeline.zoom_around(zoom, (p.x - g.grid.left()).max(0.0));
			}
			if alt && scroll.y != 0.0 {
				v.zoom_rows(
					1.0 + scroll.y * 0.004,
					(p.y - g.grid.top()).max(0.0),
					g.grid.height(),
				);
			} else {
				if scroll.x != 0.0 {
					v.timeline.scroll_px(-scroll.x);
				}
				if scroll.y != 0.0 {
					if g.ruler.contains(p) {
						v.timeline.scroll_px(-scroll.y);
					} else {
						v.scroll_y -= scroll.y;
					}
				}
			}
		}
		let (pressed, down, delta) = ui.input(|i| {
			(
				i.pointer.button_pressed(PointerButton::Middle),
				i.pointer.middle_down(),
				i.pointer.delta(),
			)
		});
		if pressed && inside {
			self.pan = true;
		}
		if !down {
			self.pan = false;
		}
		if self.pan {
			v.timeline.scroll_px(-delta.x);
			v.scroll_y -= delta.y;
			ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
		}
		// Auto-scroll while dragging notes or a rubber band near the edges.
		if self.gesture.is_some()
			&& let Some(p) = ui.ctx().pointer_latest_pos()
		{
			let dx = timeline::edge_scroll(p.x, g.grid.left(), g.grid.right());
			let dy = timeline::edge_scroll(p.y, g.grid.top(), g.grid.bottom());
			if dx != 0.0 || dy != 0.0 {
				v.timeline.scroll_px(dx);
				v.scroll_y += dy;
				ui.ctx().request_repaint();
			}
		}
	}

	fn toolbar(&mut self, ui: &mut Ui, state: &mut AppState, g: &Geometry, t: Option<&Target>) {
		let r = g.toolbar;
		ui.painter().rect_filled(r, 0.0, theme::BG_PANEL);
		ui.painter().hline(
			r.x_range(),
			r.bottom() - 0.5,
			Stroke::new(1.0, theme::BORDER),
		);
		let mut bar = ui.new_child(
			egui::UiBuilder::new()
				.max_rect(r.shrink2(vec2(8.0, 3.0)))
				.layout(egui::Layout::left_to_right(egui::Align::Center)),
		);
		let ui = &mut bar;
		ui.spacing_mut().item_spacing.x = 5.0;
		ui.label(
			RichText::new("PIANO ROLL")
				.font(theme::small())
				.color(theme::TEXT_FAINT),
		);
		if let Some(t) = t {
			let (dot, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
			ui.painter()
				.rect_filled(dot, CornerRadius::same(2), t.color);
			let track = state
				.project()
				.track(t.track)
				.map(|t| t.name.clone())
				.unwrap_or_default();
			ui.label(RichText::new(&t.clip.name).strong().color(theme::TEXT));
			ui.label(
				RichText::new(format!("on {track}"))
					.font(theme::small())
					.color(theme::TEXT_DIM),
			);
		}
		ui.separator();
		let v = &mut state.view.piano;
		for (tool, label, key, tip) in [
			(
				Tool::Draw,
				"✏ Draw",
				"P",
				"Click to add notes, drag to set the length",
			),
			(
				Tool::Select,
				"⬚ Select",
				"E",
				"Click to select, drag a rubber band",
			),
		] {
			if ui
				.selectable_label(v.tool == tool, label)
				.on_hover_text(format!("{tip} ({key})"))
				.clicked()
			{
				v.tool = tool;
			}
		}
		ui.separator();
		let snap = state.view.snap;
		if ui
			.selectable_label(snap, "Snap")
			.on_hover_text(
				"Snap to the grid (shared with the arrange view; Alt while dragging: free)",
			)
			.clicked()
		{
			state.view.snap = !snap;
		}
		let grid = state
			.view
			.piano
			.timeline
			.grid(state.project().time_signature);
		ui.label(
			RichText::new(arrange::grid_name(grid.step, grid.bar))
				.font(theme::small())
				.color(theme::TEXT_DIM),
		);
		ui.separator();
		scale_picker(ui, &mut state.view.piano);
		let v = &mut state.view.piano;
		egui::ComboBox::from_id_salt("piano-ghosts")
			.width(120.0)
			.selected_text(RichText::new(v.ghosts.name()).font(theme::small()))
			.show_ui(ui, |ui| {
				for gh in [Ghosts::Off, Ghosts::SameTrack, Ghosts::AllTracks] {
					ui.selectable_value(&mut v.ghosts, gh, gh.name());
				}
			})
			.response
			.on_hover_text("Show the notes of other clips dimmed behind this one");
		if ui
			.selectable_label(v.velocity_lane, "Velocity")
			.on_hover_text("Show the velocity lane")
			.clicked()
		{
			v.velocity_lane = !v.velocity_lane;
		}
		ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
			if ui
				.add(
					egui::Button::new(RichText::new("×").size(15.0).color(theme::TEXT_DIM))
						.frame(false),
				)
				.on_hover_text("Close the piano roll (F7)")
				.clicked()
			{
				state.close_piano_roll();
			}
			if let Some(t) = t {
				if ui
					.small_button("Fit")
					.on_hover_text("Zoom to the clip")
					.clicked()
				{
					state.view.piano.frame(&t.clip, self.width, self.height);
				}
				if ui
					.small_button("+")
					.on_hover_text("Zoom in (Ctrl+wheel)")
					.clicked()
				{
					state.view.piano.timeline.zoom_around(1.5, self.width * 0.5);
				}
				if ui.small_button("−").on_hover_text("Zoom out").clicked() {
					state
						.view
						.piano
						.timeline
						.zoom_around(1.0 / 1.5, self.width * 0.5);
				}
				ui.separator();
				if ui
					.button("Quantize")
					.on_hover_text("Move the selected notes (or all) onto the grid (Q)")
					.clicked()
				{
					quantize(state);
				}
				let n = state.view.piano.selected.len();
				ui.label(
					RichText::new(if n == 0 {
						format!("{} notes", t.clip.notes().len())
					} else {
						format!("{n} of {} selected", t.clip.notes().len())
					})
					.font(theme::small())
					.color(theme::TEXT_FAINT),
				);
			}
		});
	}

	fn grid(&mut self, ui: &mut Ui, state: &mut AppState, g: &Geometry, t: &Target) {
		let area = g.grid;
		let p = ui.painter_at(area);
		let v = state.view.piano.clone();
		let tl = v.timeline;
		let ts = state.project().time_signature;
		let clip = &t.clip;

		// Rows: black keys darker, out-of-scale rows dimmed, roots tinted.
		let top_pitch = v.y_to_pitch(0.0);
		let bottom_pitch = v.y_to_pitch(area.height());
		for pitch in bottom_pitch..=top_pitch {
			let y = area.top() + v.pitch_to_y(pitch);
			let row = Rect::from_min_size(pos2(area.left(), y), vec2(area.width(), v.row_height));
			p.rect_filled(row, 0.0, row_color(pitch, v.scale));
			if pitch % 12 == 0 {
				p.hline(
					area.x_range(),
					row.bottom() - 0.5,
					Stroke::new(1.0, theme::GRID_BAR),
				);
			} else if v.row_height >= 9.0 {
				p.hline(
					area.x_range(),
					row.bottom() - 0.5,
					Stroke::new(1.0, theme::GRID_FINE),
				);
			}
		}
		arrange::grid(&p, area, &tl, ts);
		// Outside the clip: shaded; its edges marked.
		let cx0 = area.left() + tl.tick_to_x(clip.start);
		let cx1 = area.left() + tl.tick_to_x(clip.end());
		let shade = Color32::from_black_alpha(110);
		if cx0 > area.left() {
			p.rect_filled(
				Rect::from_min_max(area.min, pos2(cx0.min(area.right()), area.bottom())),
				0.0,
				shade,
			);
		}
		if cx1 < area.right() {
			p.rect_filled(
				Rect::from_min_max(pos2(cx1.max(area.left()), area.top()), area.max),
				0.0,
				shade,
			);
		}
		for x in [cx0, cx1] {
			p.vline(
				x,
				area.y_range(),
				Stroke::new(1.5, t.color.gamma_multiply(0.8)),
			);
		}

		// Ghost notes of other clips.
		if v.ghosts != Ghosts::Off {
			for track in &state.project().tracks {
				if v.ghosts == Ghosts::SameTrack && track.id != t.track {
					continue;
				}
				let color = theme::track_color(track.color.as_deref());
				for c in track
					.clips
					.iter()
					.filter(|c| c.id != clip.id && c.is_midi())
				{
					for n in c.notes() {
						let r = note_rect(&v, area, c.start, n.start, n.length, n.pitch);
						if r.intersects(area) {
							p.rect_filled(r, CornerRadius::same(2), color.gamma_multiply(0.18));
							p.rect_stroke(
								r,
								CornerRadius::same(2),
								Stroke::new(1.0, color.gamma_multiply(0.35)),
								StrokeKind::Inside,
							);
						}
					}
				}
			}
		}

		let response = ui.interact(area, Id::new("piano-grid"), Sense::click_and_drag());
		let pointer = ui.ctx().pointer_latest_pos();
		let hover = response.hover_pos();
		let snap = step(state, ui);
		let (pressed, down, released, shift, ctrl) = ui.input(|i| {
			(
				i.pointer.primary_pressed(),
				i.pointer.primary_down(),
				i.pointer.primary_released(),
				i.modifiers.shift,
				i.modifiers.command,
			)
		});
		let hit = |pos: Pos2| -> Option<(Note, bool)> {
			let abs = tl.x_to_tick(pos.x - area.left());
			let pitch = v.y_to_pitch(pos.y - area.top());
			let n = piano::note_at(clip, abs - clip.start, pitch)?;
			let end_x = area.left() + tl.tick_to_x(clip.start + n.end());
			let near_end = pos.x > end_x - EDGE.min(note_px(&v, n.length) * 0.4);
			Some((n.clone(), near_end))
		};

		// Cursor feedback.
		if let Some(pos) = hover
			&& self.gesture.is_none()
			&& !self.pan
		{
			match hit(pos) {
				Some((_, true)) => ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal),
				Some(_) => ui.ctx().set_cursor_icon(CursorIcon::Grab),
				None if v.tool == Tool::Draw && !shift => {
					ui.ctx().set_cursor_icon(CursorIcon::Crosshair)
				}
				None => {}
			}
		}

		// Press: start a gesture.
		if pressed
			&& response.hovered()
			&& let Some(pos) = pointer
		{
			let abs = tl.x_to_tick(pos.x - area.left());
			let pitch = v.y_to_pitch(pos.y - area.top());
			match hit(pos) {
				Some((n, near_end)) => {
					let in_sel = v.selected.contains(&n.id);
					if !in_sel {
						state.view.piano.select([n.id], shift || ctrl);
					}
					let ids = state.view.piano.selected.clone();
					if near_end {
						self.gesture = Some(Gesture::Resize {
							ids,
							anchor_end: clip.start + n.end(),
							press_tick: abs,
							delta: Tick::ZERO,
						});
					} else {
						self.gesture = Some(Gesture::Move {
							ids,
							anchor_start: clip.start + n.start,
							press_tick: abs,
							press_pitch: pitch,
							dt: Tick::ZERO,
							dp: 0,
							moved: false,
							note: n.id,
							add: shift || ctrl,
							was_selected: in_sel,
						});
						self.sound(state, t.track, n.pitch, n.velocity);
					}
					state.view.piano.last_length = n.length;
					state.view.piano.last_velocity = n.velocity;
				}
				None if v.tool == Tool::Draw && !shift && !ctrl => {
					let start = piano::snap_floor(abs, snap) - clip.start;
					state.view.piano.selected.clear();
					self.gesture = Some(Gesture::Draw {
						start,
						pitch,
						end: start + v.last_length,
						moved: false,
					});
					self.sound(state, t.track, pitch, v.last_velocity);
				}
				None => {
					self.gesture = Some(Gesture::Rubber {
						from: (abs, pos.y - area.top() + v.scroll_y),
						to: pos,
						add: shift || ctrl,
					});
				}
			}
		}

		// Drag: update the gesture.
		if down && let (Some(pos), Some(gesture)) = (pointer, self.gesture.as_mut()) {
			let abs = tl.x_to_tick(pos.x - area.left());
			let pitch = v.y_to_pitch(pos.y - area.top());
			let mut sound = None;
			match gesture {
				Gesture::Draw {
					start, end, moved, ..
				} => {
					let start_abs = clip.start + *start;
					let min = snap.unwrap_or(MIN_NOTE_LENGTH);
					if (abs - start_abs).0.abs() > tl.px_to_ticks(3.0).0 {
						*moved = true;
					}
					if *moved {
						let e = piano::snap_abs(abs, snap);
						*end = Tick((e - clip.start).0.max(start.0 + min.0));
					}
				}
				Gesture::Move {
					ids,
					anchor_start,
					press_tick,
					press_pitch,
					dt,
					dp,
					moved,
					..
				} => {
					let raw = *anchor_start + (abs - *press_tick);
					let snapped = piano::snap_abs(raw, snap);
					let vertical_only = shift && *moved;
					let (t_off, p_off) = piano::clamp_move(
						clip,
						ids,
						if vertical_only {
							Tick::ZERO
						} else {
							snapped - *anchor_start
						},
						i32::from(pitch) - i32::from(*press_pitch),
					);
					if t_off != *dt || p_off != *dp {
						*moved = true;
						if p_off != *dp {
							sound = Some(p_off);
						}
					}
					*dt = t_off;
					*dp = p_off;
				}
				Gesture::Resize {
					anchor_end,
					press_tick,
					delta,
					..
				} => {
					let end = piano::snap_abs(*anchor_end + (abs - *press_tick), snap);
					*delta = end - *anchor_end;
				}
				Gesture::Rubber { to, .. } => *to = pos,
			}
			if let Some(dp) = sound
				&& let Some(Gesture::Move { ids, .. }) = &self.gesture
				&& let Some(n) = clip.notes().iter().find(|n| ids.contains(&n.id))
			{
				let pitch = (i32::from(n.pitch) + dp).clamp(0, 127) as u8;
				self.sound(state, t.track, pitch, n.velocity);
			}
		}

		// Release: commit one command.
		if released && let Some(gesture) = self.gesture.take() {
			self.silence(state, t.track);
			if let Gesture::Rubber { from, to, add } = gesture {
				let (t0, y0) = from;
				let t1 = tl.x_to_tick(to.x - area.left());
				let y1 = to.y - area.top() + v.scroll_y;
				let pitch = |y: f32| (127.0 - (y / v.row_height).floor()).clamp(0.0, 127.0) as u8;
				let ids = piano::notes_in_rect(
					clip,
					t0 - clip.start,
					t1 - clip.start,
					pitch(y0),
					pitch(y1),
				);
				state.view.piano.select(ids, add);
			} else {
				commit(state, clip, gesture, &v);
			}
		}

		// Right-click deletes the note under the pointer (or the selection it belongs to).
		if response.secondary_clicked()
			&& let Some(pos) = response.interact_pointer_pos()
			&& let Some((n, _)) = hit(pos)
		{
			let ids = if v.selected.contains(&n.id) {
				v.selected.clone()
			} else {
				[n.id].into_iter().collect()
			};
			if let Some(cmd) = piano::delete_notes(state.project(), clip.id, &ids) {
				state.apply(cmd);
			}
		}

		// Notes (with the gesture preview applied).
		let v = &state.view.piano;
		let (move_off, resize_off) = match &self.gesture {
			Some(Gesture::Move { ids, dt, dp, .. }) => (Some((ids, *dt, *dp)), None),
			Some(Gesture::Resize { ids, delta, .. }) => (None, Some((ids, *delta))),
			_ => (None, None),
		};
		let rubber = match &self.gesture {
			Some(Gesture::Rubber { from, to, .. }) => {
				let a = pos2(
					area.left() + tl.tick_to_x(from.0),
					area.top() + from.1 - v.scroll_y,
				);
				Some(Rect::from_two_pos(a, *to))
			}
			_ => None,
		};
		let in_rubber = |r: Rect| rubber.is_some_and(|rb| rb.intersects(r));
		let pr = ui.painter_at(area);
		for n in clip.notes() {
			let mut start = n.start;
			let mut pitch = n.pitch;
			let mut length = n.length;
			let mut dragged = false;
			if let Some((ids, dt, dp)) = move_off
				&& ids.contains(&n.id)
			{
				start = start + dt;
				pitch = (i32::from(pitch) + dp).clamp(0, 127) as u8;
				dragged = dt.0 != 0 || dp != 0;
			}
			if let Some((ids, d)) = resize_off
				&& ids.contains(&n.id)
			{
				length = Tick((length + d).0.max(MIN_NOTE_LENGTH.0));
			}
			if dragged {
				let orig = note_rect(v, area, clip.start, n.start, n.length, n.pitch);
				pr.rect_stroke(
					orig,
					CornerRadius::same(2),
					Stroke::new(1.0, theme::TEXT_FAINT),
					StrokeKind::Inside,
				);
			}
			let r = note_rect(v, area, clip.start, start, length, pitch);
			if !r.intersects(area) {
				continue;
			}
			let selected = v.selected.contains(&n.id) || in_rubber(r);
			paint_note(&pr, r, t.color, n.velocity, selected, pitch, v.row_height);
		}
		// The note being drawn.
		if let Some(Gesture::Draw {
			start, pitch, end, ..
		}) = &self.gesture
		{
			let r = note_rect(v, area, clip.start, *start, *end - *start, *pitch);
			paint_note(&pr, r, t.color, v.last_velocity, true, *pitch, v.row_height);
		}
		if let Some(rb) = rubber {
			pr.rect_filled(rb, 0.0, theme::ACCENT.gamma_multiply(0.12));
			pr.rect_stroke(rb, 0.0, Stroke::new(1.0, theme::ACCENT), StrokeKind::Inside);
		}
	}

	/// Starts auditioning `pitch` (releases the previous one).
	fn sound(&mut self, state: &mut AppState, track: TrackId, pitch: u8, velocity: f32) {
		if self.sounding == Some(pitch) {
			return;
		}
		self.silence(state, track);
		state.preview_note_on(track, pitch, velocity);
		self.sounding = Some(pitch);
	}

	fn silence(&mut self, state: &mut AppState, track: TrackId) {
		if let Some(p) = self.sounding.take() {
			state.preview_note_off(track, p);
		}
	}

	fn keyboard(&mut self, ui: &mut Ui, state: &mut AppState, g: &Geometry, t: &Target) {
		let area = g.keys;
		let p = ui.painter_at(area);
		p.rect_filled(area, 0.0, theme::BG_PANEL);
		let v = state.view.piano.clone();
		let response = ui
			.interact(area, Id::new("piano-keys"), Sense::click_and_drag())
			.on_hover_cursor(CursorIcon::PointingHand);
		let pitch_at = |y: f32| v.y_to_pitch(y - area.top());
		let down = ui.input(|i| i.pointer.primary_down());
		let held = if response.is_pointer_button_down_on() || (self.key.is_some() && down) {
			ui.ctx().pointer_latest_pos().map(|pos| pitch_at(pos.y))
		} else {
			None
		};
		if held != self.key {
			if let Some(old) = self.key {
				state.preview_note_off(t.track, old);
			}
			if let Some(new) = held {
				state.preview_note_on(t.track, new, 0.8);
			}
			self.key = held;
		}
		let white_w = area.width() - 2.0;
		let black_w = white_w * 0.6;
		let top_pitch = v.y_to_pitch(0.0).saturating_add(1).min(127);
		let bottom_pitch = v.y_to_pitch(area.height()).saturating_sub(1);
		let row_top = |pitch: u8| area.top() + v.pitch_to_y(pitch);
		let pressed = |pitch: u8| {
			self.key == Some(pitch) || state.previewing.iter().any(|(_, p)| *p == pitch)
		};
		// White keys span their row plus half of each neighbouring black row, like a
		// real keyboard; black keys are drawn on top.
		p.rect_filled(
			Rect::from_min_max(area.min, pos2(area.left() + white_w, area.bottom())),
			0.0,
			theme::KEY_WHITE,
		);
		for pitch in bottom_pitch..=top_pitch {
			if piano::is_black(pitch) {
				continue;
			}
			let above = if pitch < 127 && piano::is_black(pitch + 1) {
				row_top(pitch + 1) + v.row_height * 0.5
			} else {
				row_top(pitch)
			};
			let below = if pitch > 0 && piano::is_black(pitch - 1) {
				row_top(pitch - 1) + v.row_height * 0.5
			} else {
				row_top(pitch) + v.row_height
			};
			let key =
				Rect::from_min_max(pos2(area.left(), above), pos2(area.left() + white_w, below));
			let in_scale = v.scale.is_none_or(|s| s.contains(pitch));
			if pressed(pitch) {
				p.rect_filled(key, 0.0, theme::ACCENT);
			} else if !in_scale {
				p.rect_filled(key, 0.0, theme::mix(theme::KEY_WHITE, theme::BG_DEEP, 0.3));
			}
			p.hline(
				key.x_range(),
				below - 0.5,
				Stroke::new(1.0, Color32::from_rgb(0x9a, 0xa1, 0xab)),
			);
			if pitch % 12 == 0 && v.row_height >= 7.0 {
				p.text(
					pos2(key.right() - 4.0, row_top(pitch) + v.row_height * 0.5),
					Align2::RIGHT_CENTER,
					cadenza_theory::format_pitch(pitch, false),
					FontId::proportional((v.row_height * 0.75).clamp(8.0, 11.0)),
					Color32::from_rgb(0x3a, 0x40, 0x48),
				);
			}
		}
		for pitch in bottom_pitch..=top_pitch {
			if !piano::is_black(pitch) {
				continue;
			}
			let r = Rect::from_min_size(
				pos2(area.left(), row_top(pitch)),
				vec2(black_w, v.row_height),
			);
			p.rect_filled(
				r,
				CornerRadius {
					ne: 2,
					se: 2,
					..Default::default()
				},
				if pressed(pitch) {
					theme::ACCENT
				} else {
					theme::KEY_BLACK
				},
			);
			if v.scale.is_some_and(|s| s.contains(pitch)) {
				// In-scale black keys get a small mark, so the scale reads on the keys.
				p.rect_filled(
					Rect::from_min_size(pos2(r.right() - 5.0, r.center().y - 1.0), vec2(3.0, 2.0)),
					0.0,
					theme::ACCENT_DIM,
				);
			}
		}
		p.vline(
			area.right() - 0.5,
			area.y_range(),
			Stroke::new(1.0, theme::BORDER_STRONG),
		);
	}

	fn velocity_lane(
		&mut self,
		ui: &mut Ui,
		state: &mut AppState,
		lane: Rect,
		g: &Geometry,
		t: &Target,
	) {
		let label = Rect::from_min_max(
			pos2(g.keys.left(), lane.top()),
			pos2(lane.left(), lane.bottom()),
		);
		let lp = ui.painter_at(label);
		lp.rect_filled(label, 0.0, theme::BG_PANEL);
		lp.text(
			label.center(),
			Align2::CENTER_CENTER,
			"VELOCITY",
			FontId::proportional(9.0),
			theme::TEXT_FAINT,
		);
		lp.vline(
			label.right() - 0.5,
			label.y_range(),
			Stroke::new(1.0, theme::BORDER_STRONG),
		);
		let p = ui.painter_at(lane);
		p.rect_filled(lane, 0.0, theme::BG_PANEL);
		p.hline(
			lane.x_range(),
			lane.top() + 0.5,
			Stroke::new(1.0, theme::BORDER_STRONG),
		);
		let inner = lane.shrink2(vec2(0.0, 6.0));
		for frac in [0.25, 0.5, 0.75] {
			p.hline(
				lane.x_range(),
				inner.bottom() - inner.height() * frac,
				Stroke::new(1.0, theme::GRID_FINE),
			);
		}
		let v = state.view.piano.clone();
		let tl = v.timeline;
		let clip = &t.clip;
		let value_at = |y: f32| ((inner.bottom() - y) / inner.height()).clamp(0.0, 1.0);
		let response = ui.interact(lane, Id::new("piano-velocity"), Sense::click_and_drag());
		let (pressed, down, released) = ui.input(|i| {
			(
				i.pointer.primary_pressed(),
				i.pointer.primary_down(),
				i.pointer.primary_released(),
			)
		});
		let bar_x = |n: &Note| lane.left() + tl.tick_to_x(clip.start + n.start);
		let near = |n: &Note, x: f32| {
			let x0 = bar_x(n);
			let x1 = x0 + note_px(&v, n.length).max(6.0);
			(x0 - 3.0..=x1).contains(&x)
		};
		if pressed
			&& response.hovered()
			&& let Some(pos) = ui.ctx().pointer_latest_pos()
		{
			let anchor = clip
				.notes()
				.iter()
				.filter(|n| near(n, pos.x))
				.min_by(|a, b| {
					(bar_x(a) - pos.x)
						.abs()
						.total_cmp(&(bar_x(b) - pos.x).abs())
				});
			let relative = anchor
				.filter(|n| v.selected.contains(&n.id) && v.selected.len() > 1)
				.map(|n| (n.velocity, value_at(pos.y)));
			self.velocity = Some(VelocityDrag {
				values: HashMap::new(),
				relative,
			});
		}
		if down
			&& let (Some(drag), Some(pos)) = (self.velocity.as_mut(), ui.ctx().pointer_latest_pos())
		{
			let value = value_at(pos.y);
			match drag.relative {
				Some((_, press)) => {
					for n in clip.notes().iter().filter(|n| v.selected.contains(&n.id)) {
						drag.values
							.insert(n.id, (n.velocity + value - press).clamp(0.0, 1.0));
					}
				}
				None => {
					for n in clip.notes().iter().filter(|n| {
						near(n, pos.x) && (v.selected.is_empty() || v.selected.contains(&n.id))
					}) {
						drag.values.insert(n.id, value);
					}
				}
			}
		}
		if released && let Some(drag) = self.velocity.take() {
			if let Some(cmd) = piano::set_velocities(state.project(), clip.id, &drag.values) {
				state.apply(cmd);
			}
			if let Some(last) = drag.values.values().last() {
				state.view.piano.last_velocity = *last;
			}
		}
		let preview = self.velocity.as_ref().map(|d| &d.values);
		for n in clip.notes() {
			let x = bar_x(n);
			if x < lane.left() - 4.0 || x > lane.right() {
				continue;
			}
			let vel = preview
				.and_then(|m| m.get(&n.id))
				.copied()
				.unwrap_or(n.velocity);
			let selected = v.selected.contains(&n.id);
			let top = inner.bottom() - vel * inner.height();
			let color = if selected {
				Color32::WHITE
			} else {
				theme::mix(t.color, Color32::WHITE, 0.2)
			};
			p.vline(
				x + 1.0,
				top..=inner.bottom(),
				Stroke::new(2.0, color.gamma_multiply(0.8)),
			);
			p.circle_filled(pos2(x + 1.0, top), 3.0, color);
		}
		if response.hovered() {
			ui.ctx().set_cursor_icon(CursorIcon::ResizeVertical);
		}
	}

	fn scrollbars(&mut self, ui: &mut Ui, state: &mut AppState, g: &Geometry, t: &Target) {
		ui.painter().rect_filled(
			Rect::from_min_max(
				pos2(g.keys.left(), g.hbar.top()),
				pos2(g.vbar.right(), g.hbar.bottom()),
			),
			0.0,
			theme::BG_PANEL,
		);
		ui.painter().rect_filled(
			Rect::from_min_max(g.vbar.min, pos2(g.vbar.right(), g.hbar.top())),
			0.0,
			theme::BG_PANEL,
		);
		let end_song = state.song_end().beats();
		let v = &mut state.view.piano;
		let visible = v.timeline.visible_beats(g.grid.width());
		let end = t.clip.end().beats().max(end_song);
		let total = timeline::content_beats(end, v.timeline.scroll_beats, visible);
		if let Some(x) = widgets::scrollbar(
			ui,
			Id::new("piano-hbar"),
			g.hbar,
			false,
			total,
			visible,
			v.timeline.scroll_beats,
		) {
			v.timeline.scroll_beats = x;
		}
		if let Some(y) = widgets::scrollbar(
			ui,
			Id::new("piano-vbar"),
			g.vbar,
			true,
			f64::from(v.content_height()),
			f64::from(g.grid.height()),
			f64::from(v.scroll_y),
		) {
			v.scroll_y = y as f32;
		}
	}

	/// Piano-roll keys (only while the piano roll has the focus). Consumes what it uses.
	pub fn shortcuts(&mut self, ctx: &egui::Context, state: &mut AppState) {
		if ctx.egui_wants_keyboard_input() {
			return;
		}
		let Some(t) = target(state) else {
			return;
		};
		let pressed = |m: Modifiers, k: Key| {
			ctx.input_mut(|i| i.consume_shortcut(&KeyboardShortcut::new(m, k)))
		};
		let clip = t.clip.id;
		let ts = state.project().time_signature;
		let grid_step = state.view.piano.timeline.grid(ts).step;
		let sel = state.view.piano.selected.clone();
		if pressed(Modifiers::NONE, Key::P) || pressed(Modifiers::NONE, Key::B) {
			state.view.piano.tool = Tool::Draw;
		}
		if pressed(Modifiers::NONE, Key::E) {
			state.view.piano.tool = Tool::Select;
		}
		if pressed(Modifiers::COMMAND, Key::A) {
			state
				.view
				.piano
				.select(t.clip.notes().iter().map(|n| n.id), false);
		}
		if pressed(Modifiers::NONE, Key::Escape) {
			state.view.piano.selected.clear();
		}
		if pressed(Modifiers::COMMAND, Key::C) {
			state.view.piano.clipboard = piano::copy_notes(&t.clip, &sel);
			if !sel.is_empty() {
				state
					.notices
					.info(format!("Copied {} notes", state.view.piano.clipboard.len()));
			}
		}
		if pressed(Modifiers::COMMAND, Key::V) {
			let at = state.position() - t.clip.start;
			let board = state.view.piano.clipboard.clone();
			if let Some((cmd, ids)) = piano::paste_notes(state.project(), clip, &board, at)
				&& state.apply(cmd)
			{
				state.view.piano.select(ids, false);
			}
		}
		if pressed(Modifiers::COMMAND, Key::D)
			&& let Some((cmd, ids)) = piano::duplicate_notes(state.project(), clip, &sel)
			&& state.apply(cmd)
		{
			state.view.piano.select(ids, false);
		}
		if (pressed(Modifiers::NONE, Key::Delete) || pressed(Modifiers::NONE, Key::Backspace))
			&& let Some(cmd) = piano::delete_notes(state.project(), clip, &sel)
		{
			state.apply(cmd);
		}
		if pressed(Modifiers::NONE, Key::Q) {
			quantize(state);
		}
		let moves = [
			(Modifiers::SHIFT, Key::ArrowUp, Tick::ZERO, 12),
			(Modifiers::SHIFT, Key::ArrowDown, Tick::ZERO, -12),
			(Modifiers::NONE, Key::ArrowUp, Tick::ZERO, 1),
			(Modifiers::NONE, Key::ArrowDown, Tick::ZERO, -1),
			(Modifiers::NONE, Key::ArrowLeft, Tick(-grid_step.0), 0),
			(Modifiers::NONE, Key::ArrowRight, grid_step, 0),
		];
		for (m, k, dt, dp) in moves {
			if pressed(m, k)
				&& let Some(cmd) = piano::move_notes(state.project(), clip, &sel, dt, dp)
			{
				state.apply(cmd);
				// Audition the transposed selection's lowest note.
				if dp != 0
					&& let Some(n) = t
						.clip
						.notes()
						.iter()
						.filter(|n| sel.contains(&n.id))
						.min_by_key(|n| n.pitch)
				{
					let pitch = (i32::from(n.pitch) + dp).clamp(0, 127) as u8;
					state.release_previews();
					state.preview_note_on(t.track, pitch, n.velocity);
					self.sounding = None;
				}
			}
		}
		if ctx.input(|i| {
			!i.keys_down.contains(&Key::ArrowUp) && !i.keys_down.contains(&Key::ArrowDown)
		}) && self.gesture.is_none()
			&& self.key.is_none()
			&& self.sounding.is_none()
			&& !state.previewing.is_empty()
		{
			state.release_previews();
		}
	}

	/// Releases anything the piano roll is sounding (panel hidden, clip changed).
	pub fn release(&mut self, state: &mut AppState) {
		self.gesture = None;
		self.velocity = None;
		self.key = None;
		self.sounding = None;
		state.release_previews();
	}
}

/// Commits a finished gesture as one command.
fn commit(state: &mut AppState, clip: &Clip, gesture: Gesture, v: &PianoView) {
	let id = clip.id;
	match gesture {
		Gesture::Draw {
			start,
			pitch,
			end,
			moved,
		} => {
			let length = if moved { end - start } else { v.last_length };
			if let Some((cmd, note)) =
				piano::add_note(state.project(), id, start, pitch, length, v.last_velocity)
				&& state.apply(cmd)
			{
				state.view.piano.last_length = length;
				state.view.piano.select([note], false);
			}
		}
		Gesture::Move {
			ids,
			dt,
			dp,
			moved,
			note,
			add,
			was_selected,
			..
		} => {
			if !moved {
				// A click: Shift/Ctrl toggles an already selected note, a plain click on a
				// note of a group narrows the selection to it.
				if add && was_selected {
					state.view.piano.selected.remove(&note);
				} else if !add {
					state.view.piano.select([note], false);
				}
				return;
			}
			if let Some(cmd) = piano::move_notes(state.project(), id, &ids, dt, dp) {
				state.apply(cmd);
			}
		}
		Gesture::Resize { ids, delta, .. } => {
			if let Some(cmd) = piano::resize_notes(state.project(), id, &ids, delta)
				&& state.apply(cmd)
				&& let Some(n) = state
					.project()
					.find_clip(id)
					.map(|(_, c)| c)
					.and_then(|c| c.notes().iter().find(|n| ids.contains(&n.id)))
			{
				state.view.piano.last_length = n.length;
			}
		}
		Gesture::Rubber { .. } => {}
	}
}

/// Quantizes the selection (or the whole clip) to the current grid.
pub fn quantize(state: &mut AppState) {
	let Some(clip) = state.view.piano.clip else {
		return;
	};
	let step = state
		.view
		.piano
		.timeline
		.grid(state.project().time_signature)
		.step;
	let sel = state.view.piano.selected.clone();
	match piano::quantize_notes(state.project(), clip, &sel, step, 1.0) {
		Some(cmd) => {
			state.apply(cmd);
		}
		None => state.notices.info("Already on the grid"),
	}
}

fn note_px(v: &PianoView, length: Tick) -> f32 {
	v.timeline.ticks_to_px(length)
}

fn note_rect(
	v: &PianoView,
	area: Rect,
	clip_start: Tick,
	start: Tick,
	length: Tick,
	pitch: u8,
) -> Rect {
	let x0 = area.left() + v.timeline.tick_to_x(clip_start + start);
	let w = note_px(v, length).max(3.0);
	let y = area.top() + v.pitch_to_y(pitch);
	Rect::from_min_size(pos2(x0, y + 0.5), vec2(w, (v.row_height - 1.0).max(2.0)))
}

fn paint_note(
	p: &Painter,
	r: Rect,
	color: Color32,
	velocity: f32,
	selected: bool,
	pitch: u8,
	row_height: f32,
) {
	// Brighter for louder notes; selected notes are lighter with a white outline.
	let base = theme::mix(color, Color32::WHITE, 0.15);
	let fill = theme::mix(
		theme::mix(base, theme::BG_DEEP, 0.55),
		base,
		0.35 + 0.65 * velocity,
	);
	let fill = if selected {
		theme::mix(fill, Color32::WHITE, 0.35)
	} else {
		fill
	};
	p.rect_filled(r, CornerRadius::same(2), fill);
	// Velocity tick along the top edge.
	let vw = (r.width() - 2.0).max(0.0) * velocity;
	p.hline(
		r.left() + 1.0..=r.left() + 1.0 + vw,
		r.top() + 1.0,
		Stroke::new(1.0, Color32::from_white_alpha(110)),
	);
	p.rect_stroke(
		r,
		CornerRadius::same(2),
		if selected {
			Stroke::new(1.5, Color32::WHITE)
		} else {
			Stroke::new(1.0, theme::mix(color, theme::BG_DEEP, 0.5))
		},
		StrokeKind::Inside,
	);
	if row_height >= 11.0 && r.width() > 26.0 {
		p.with_clip_rect(r.shrink(1.0)).text(
			pos2(r.left() + 4.0, r.center().y),
			Align2::LEFT_CENTER,
			cadenza_theory::format_pitch(pitch, false),
			FontId::proportional((row_height * 0.68).clamp(8.0, 10.5)),
			theme::text_on(fill),
		);
	}
}

fn row_color(pitch: u8, scale: Option<Scale>) -> Color32 {
	match scale {
		Some(s) if !s.contains(pitch) => theme::ROW_OUT_OF_SCALE,
		Some(s) if (i32::from(pitch) - i32::from(s.root.0)).rem_euclid(12) == 0 => theme::ROW_ROOT,
		_ if piano::is_black(pitch) => theme::ROW_BLACK,
		_ => theme::ROW_WHITE,
	}
}

/// Bar/beat ruler on the piano roll's time axis (click/drag moves the playhead); the
/// clip's span is marked in its track color.
fn ruler(ui: &mut Ui, state: &mut AppState, g: &Geometry, t: &Target) {
	let r = g.ruler;
	let corner = Rect::from_min_max(pos2(g.keys.left(), r.top()), pos2(r.left(), r.bottom()));
	ui.painter().rect_filled(corner, 0.0, theme::BG_PANEL);
	let tl = state.view.piano.timeline;
	let ts = state.project().time_signature;
	let p = ui.painter_at(r);
	p.rect_filled(r, 0.0, theme::BG_RAISED);
	let x0 = r.left() + tl.tick_to_x(t.clip.start);
	let x1 = r.left() + tl.tick_to_x(t.clip.end());
	p.rect_filled(
		Rect::from_min_max(pos2(x0, r.bottom() - 4.0), pos2(x1, r.bottom())),
		0.0,
		t.color,
	);
	let grid = tl.grid(ts);
	let beat = Tick(PPQ * 4 / i64::from(ts.denominator.max(1)));
	let bar_px = tl.ticks_to_px(grid.bar);
	let every = (40.0 / bar_px).ceil().max(1.0) as i64;
	let step = if tl.ticks_to_px(beat) >= 6.0 {
		beat.0.min(grid.step.0.max(beat.0))
	} else {
		grid.bar.0
	};
	let mut tick = tl.x_to_tick(0.0).0.max(0) / step * step;
	let end = tl.x_to_tick(r.width()).0;
	while tick <= end {
		let x = r.left() + tl.tick_to_x(Tick(tick));
		if tick % grid.bar.0 == 0 {
			let bar = tick / grid.bar.0;
			p.vline(
				x,
				r.top() + 8.0..=r.bottom(),
				Stroke::new(1.0, theme::TEXT_FAINT),
			);
			if bar % every == 0 {
				p.text(
					pos2(x + 4.0, r.top() + 2.0),
					Align2::LEFT_TOP,
					format!("{}", bar + 1),
					theme::mono(10.0),
					theme::TEXT_DIM,
				);
			}
		} else {
			let (bar, beat_no, _) = timeline::bbt(Tick(tick), ts);
			p.vline(
				x,
				r.bottom() - 5.0..=r.bottom(),
				Stroke::new(1.0, theme::GRID_BAR),
			);
			if bar_px > 160.0 && tick % beat.0 == 0 {
				p.text(
					pos2(x + 3.0, r.top() + 4.0),
					Align2::LEFT_TOP,
					format!("{bar}.{beat_no}"),
					theme::mono(8.5),
					theme::TEXT_FAINT,
				);
			}
		}
		tick += step;
	}
	p.hline(
		r.x_range(),
		r.bottom() - 0.5,
		Stroke::new(1.0, theme::BORDER),
	);
	let resp = ui
		.interact(r, Id::new("piano-ruler"), Sense::click_and_drag())
		.on_hover_cursor(CursorIcon::PointingHand);
	if (resp.clicked() || resp.dragged())
		&& let Some(pos) = resp.interact_pointer_pos()
	{
		let tick = tl.x_to_tick(pos.x - r.left());
		let tick = if state.view.snap && !ui.input(|i| i.modifiers.alt) {
			tl.snap(tick, ts)
		} else {
			tick
		};
		state.seek(tick);
	}
}

/// Root + scale kind pickers ("No scale" turns highlighting off).
fn scale_picker(ui: &mut Ui, v: &mut PianoView) {
	const KINDS: [ScaleKind; 9] = [
		ScaleKind::Major,
		ScaleKind::NaturalMinor,
		ScaleKind::HarmonicMinor,
		ScaleKind::MelodicMinor,
		ScaleKind::Dorian,
		ScaleKind::Mixolydian,
		ScaleKind::MajorPentatonic,
		ScaleKind::MinorPentatonic,
		ScaleKind::Blues,
	];
	let root = v.scale.map_or(0, |s| s.root.0);
	egui::ComboBox::from_id_salt("piano-root")
		.width(44.0)
		.selected_text(
			RichText::new(if v.scale.is_some() {
				PitchClass(root).name(false)
			} else {
				"Key"
			})
			.font(theme::small()),
		)
		.show_ui(ui, |ui| {
			for pc in 0..12u8 {
				let kind = v.scale.map_or(ScaleKind::Major, |s| s.kind);
				if ui
					.selectable_label(v.scale.is_some() && root == pc, PitchClass(pc).name(false))
					.clicked()
				{
					v.scale = Some(Scale::new(PitchClass(pc), kind));
				}
			}
		})
		.response
		.on_hover_text("Key of the scale highlighting");
	egui::ComboBox::from_id_salt("piano-scale")
		.width(108.0)
		.selected_text(
			RichText::new(
				v.scale
					.map_or("No scale", |s| crate::state::scale_kind_name(s.kind)),
			)
			.font(theme::small()),
		)
		.show_ui(ui, |ui| {
			if ui.selectable_label(v.scale.is_none(), "No scale").clicked() {
				v.scale = None;
			}
			for kind in KINDS {
				if ui
					.selectable_label(
						v.scale.is_some_and(|s| s.kind == kind),
						crate::state::scale_kind_name(kind),
					)
					.clicked()
				{
					v.scale = Some(Scale::new(PitchClass(root), kind));
				}
			}
		})
		.response
		.on_hover_text("Rows outside the scale are dimmed");
}

fn empty(ui: &mut Ui, rect: Rect) {
	let p = ui.painter_at(rect);
	p.text(
		rect.center() - vec2(0.0, 10.0),
		Align2::CENTER_CENTER,
		"No clip open",
		FontId::proportional(14.0),
		theme::TEXT_DIM,
	);
	p.text(
		rect.center() + vec2(0.0, 12.0),
		Align2::CENTER_CENTER,
		"Double-click a MIDI clip in the arrange view to edit its notes here",
		theme::small(),
		theme::TEXT_FAINT,
	);
}
