//! Piano roll: edits the notes of one MIDI clip (opened by double-clicking it, F7
//! toggles the panel). Vertical keyboard (click/drag to hear), note grid on the song's
//! time axis with the clip boundaries, scale highlighting, ghost notes of other clips
//! and a velocity lane.
//!
//! FL Studio-like editing: the draw tool does nearly everything (click adds a note and
//! keeps dragging it, notes move/resize, right-drag sweeps delete (a right-click on empty
//! space deselects all), Ctrl+drag selects,
//! Shift+drag duplicates); paint (B) and slice (C) tools; Ctrl+B clones the selection.
//! Every gesture is previewed locally and committed as one command (see
//! [`crate::state::piano`]); notes are auditioned through the track's instrument, also
//! while the transport is stopped.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use cadenza_core::{NoteId, PPQ, Tick, TrackId};
use cadenza_model::{Clip, Note};
use cadenza_theory::{PitchClass, Scale, ScaleKind};
use egui::{
	Align2, Color32, CornerRadius, CursorIcon, FontId, Id, Key, KeyboardShortcut, Modifiers,
	Painter, PointerButton, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, Ui, pos2, vec2,
};

use crate::state::AppState;
use crate::state::layout::PanelKind;
use crate::state::piano::{self, Ghosts, MIN_NOTE_LENGTH, PianoSnap, PianoView, Tool};
use crate::state::timeline::{self, LineKind, Timeline};
use crate::theme;
use crate::ui::arrange;
use crate::ui::widgets;

const TOOLBAR: f32 = 30.0;
const RULER: f32 = 22.0;
const VELOCITY_LANE: f32 = 78.0;
/// Pixels at a note's edges that resize instead of moving.
const EDGE: f32 = 6.0;

/// Which part of a note the pointer is over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Edge {
	Body,
	Start,
	End,
}

#[derive(Debug, Clone)]
enum Gesture {
	/// A note placed by a click on empty grid: it follows the pointer (pitch and snapped
	/// time) until the button is released, then it is added.
	Draw {
		/// Start relative to the clip.
		start: Tick,
		pitch: u8,
		/// Absolute start at the press.
		anchor: Tick,
		press_tick: Tick,
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
		/// Shift held at the press: duplicate instead of moving.
		copy: bool,
	},
	Resize {
		ids: BTreeSet<NoteId>,
		/// Absolute end (right edge) or start (left edge) of the pressed note.
		anchor: Tick,
		press_tick: Tick,
		delta: Tick,
		left: bool,
	},
	Rubber {
		from: (Tick, f32),
		to: Pos2,
		add: bool,
		/// Note under a Ctrl+press: a click without a drag toggles it.
		note: Option<NoteId>,
	},
	/// Paint tool: notes at every `spacing` from `first` (absolute), keyed by cell.
	Paint {
		first: Tick,
		spacing: Tick,
		cells: BTreeMap<i64, u8>,
		last: i64,
	},
	/// Slice tool: a vertical cut at `at` (absolute) from the press row to the pointer.
	Slice { at: Tick, from_y: f32, to: Pos2 },
}

/// Right button held on the grid: every note the pointer passes over is deleted on
/// release (one undo step).
#[derive(Debug, Clone)]
struct Erase {
	hits: BTreeSet<NoteId>,
	last: Pos2,
	/// Where the right button went down (a click without hits deselects all).
	start: Pos2,
	/// The pointer left the press point (a sweep, not a click).
	moved: bool,
}

#[derive(Debug, Clone)]
enum VelocityDrag {
	/// Drag over the bars sets them; on a multi-selection the drag is relative,
	/// anchored on this press value.
	Paint {
		values: HashMap<NoteId, f32>,
		relative: Option<f32>,
	},
	/// Alt+drag or right-drag: a straight line (absolute tick, value) to (tick, value).
	Line {
		from: (Tick, f32),
		to: (Tick, f32),
		values: HashMap<NoteId, f32>,
	},
}

impl VelocityDrag {
	fn values(&self) -> &HashMap<NoteId, f32> {
		match self {
			VelocityDrag::Paint { values, .. } | VelocityDrag::Line { values, .. } => values,
		}
	}
}

/// The note properties popover (double-click a note).
#[derive(Debug, Clone)]
struct Props {
	note: Note,
	pos: Pos2,
}

/// View-local state of the piano roll.
#[derive(Default)]
pub struct PianoUi {
	gesture: Option<Gesture>,
	erase: Option<Erase>,
	velocity: Option<VelocityDrag>,
	props: Option<Props>,
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

/// The piano roll's snap step (its own "magnet"; `Main` follows the global snap).
pub fn snap_step(state: &AppState) -> Option<Tick> {
	let ts = signature(state);
	let main = state
		.view
		.snap
		.then(|| state.view.piano.timeline.grid(ts).step);
	state.view.piano.snap.step(ts, main)
}

/// The time signature the piano roll's bar/beat steps use: the one at the edited clip's
/// start (else at the playhead).
fn signature(state: &AppState) -> cadenza_core::TimeSignature {
	let at = state
		.view
		.piano
		.clip
		.and_then(|id| state.project().find_clip(id))
		.map_or(state.position(), |(_, c)| c.start);
	state.project().meter().signature_at(at)
}

/// Grid step for a mouse gesture (None: snap off or Alt held).
fn step(state: &AppState, ui: &Ui) -> Option<Tick> {
	if ui.input(|i| i.modifiers.alt) {
		None
	} else {
		snap_step(state)
	}
}

/// The step keyboard nudges and quantize use: the snap, else the visible grid.
fn key_step(state: &AppState) -> Tick {
	snap_step(state).unwrap_or_else(|| state.view.piano.timeline.grid(signature(state)).step)
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
			empty(ui, state, full.with_min_y(g.toolbar.bottom()));
			return;
		};
		if state.view.piano.framed != Some(t.clip.id) {
			state
				.view
				.piano
				.frame(&t.clip, g.grid.width(), g.grid.height());
		}
		self.navigate(ui, state, &g);
		if state.is_playing() && self.gesture.is_none() && self.erase.is_none() {
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
		// Playhead (wraps with the pattern loop).
		let x = g.grid.left() + state.view.piano.timeline.tick_to_x(state.position());
		if (g.grid.left()..=g.grid.right()).contains(&x) {
			let bottom = g.velocity.map_or(g.grid.bottom(), |r| r.bottom());
			ui.painter()
				.vline(x, g.ruler.top()..=bottom, Stroke::new(1.5, theme::PLAYHEAD));
		}
		self.properties(ui, state, &t);
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
		// Auto-scroll while dragging notes, painting, erasing or a rubber band near the
		// edges.
		if (self.gesture.is_some() || self.erase.is_some())
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
			clip_selector(ui, state, t);
			if ui
				.small_button("+")
				.on_hover_text("New empty clip on this track (opens it here)")
				.clicked()
			{
				let at = state.position();
				state.new_clip_in_piano_roll(t.track, at);
			}
			ui.label(
				RichText::new(format!("on {track}"))
					.font(theme::small())
					.color(theme::TEXT_DIM),
			);
		}
		ui.separator();
		let v = &mut state.view.piano;
		for tool in Tool::ALL {
			if ui
				.selectable_label(v.tool == tool, tool.label())
				.on_hover_text(format!("{} ({})", tool.tip(), tool.key()))
				.clicked()
			{
				v.tool = tool;
			}
		}
		ui.separator();
		ui.label(
			RichText::new("Snap:")
				.font(theme::small())
				.color(theme::TEXT_FAINT),
		);
		let main_name = {
			let grid = state.view.piano.timeline.grid(signature(state));
			if state.view.snap {
				format!("Main ({})", arrange::grid_name(grid.step, grid.bar))
			} else {
				"Main (off)".to_string()
			}
		};
		let v = &mut state.view.piano;
		let name = |s: PianoSnap| {
			if s == PianoSnap::Main {
				main_name.clone()
			} else {
				s.name().to_string()
			}
		};
		egui::ComboBox::from_id_salt("piano-snap")
			.width(96.0)
			.selected_text(RichText::new(name(v.snap)).font(theme::small()))
			.show_ui(ui, |ui| {
				for s in PianoSnap::ALL {
					ui.selectable_value(&mut v.snap, s, name(s));
				}
			})
			.response
			.on_hover_text(
				"Piano roll snap (FL's magnet). Main follows the arrange snap; Alt while \
				 dragging: free",
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
			let maximized = state.view.panels.maximized == Some(PanelKind::PianoRoll);
			if ui
				.add(
					egui::Button::new(
						RichText::new(if maximized { "🗗" } else { "🗖" })
							.size(13.0)
							.color(theme::TEXT_DIM),
					)
					.frame(false),
				)
				.on_hover_text(if maximized {
					"Restore the layout (F11)"
				} else {
					"Maximize the piano roll (F11)"
				})
				.clicked()
			{
				state
					.view
					.panels
					.toggle_maximize_panel(PanelKind::PianoRoll);
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
					.on_hover_text("Move the selected notes (or all) onto the snap grid (Ctrl+Q)")
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
		let meter = cadenza_core::MeterMap::new(state.project().meter());
		let fine = snap_step(state);
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
		piano_grid(&p, area, &tl, meter.view(), fine);
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
		let visible_pitch = |pitch: u8| (bottom_pitch..=top_pitch).contains(&pitch);

		// Ghost notes of other clips (dim, behind).
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
					let (c0, c1) = (tl.tick_to_x(c.start), tl.tick_to_x(c.end()));
					if c1 < 0.0 || c0 > area.width() {
						continue;
					}
					for n in c.notes().iter().filter(|n| visible_pitch(n.pitch)) {
						let r = note_rect(&v, area, c.start, n.start, n.length, n.pitch);
						if r.intersects(area) {
							p.rect_filled(r, CornerRadius::same(2), color.gamma_multiply(0.16));
							p.rect_stroke(
								r,
								CornerRadius::same(2),
								Stroke::new(1.0, color.gamma_multiply(0.32)),
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
		let (pressed, down, released, r_pressed, r_down, mods) = ui.input(|i| {
			(
				i.pointer.primary_pressed(),
				i.pointer.primary_down(),
				i.pointer.primary_released(),
				i.pointer.button_pressed(PointerButton::Secondary),
				i.pointer.secondary_down(),
				i.modifiers,
			)
		});
		let hit = |pos: Pos2| -> Option<(Note, Edge)> {
			let abs = tl.x_to_tick(pos.x - area.left());
			let pitch = v.y_to_pitch(pos.y - area.top());
			let n = piano::note_at(clip, abs - clip.start, pitch)?;
			let x0 = area.left() + tl.tick_to_x(clip.start + n.start);
			let w = note_px(&v, n.length);
			let edge = if pos.x > x0 + w - EDGE.min(w * 0.4) {
				Edge::End
			} else if w >= EDGE * 4.0 && pos.x < x0 + EDGE.min(w * 0.25) {
				Edge::Start
			} else {
				Edge::Body
			};
			Some((n.clone(), edge))
		};

		// Cursor feedback.
		if let Some(pos) = hover
			&& self.gesture.is_none()
			&& self.erase.is_none()
			&& !self.pan
		{
			let icon = match (hit(pos), v.tool) {
				_ if mods.command => CursorIcon::Default,
				(Some(_), Tool::Slice) | (None, Tool::Draw | Tool::Paint | Tool::Slice) => {
					CursorIcon::Crosshair
				}
				(Some((_, Edge::Start | Edge::End)), _) => CursorIcon::ResizeHorizontal,
				(Some(_), _) if mods.shift => CursorIcon::Copy,
				(Some(_), _) => CursorIcon::Move,
				(None, Tool::Select) => CursorIcon::Default,
			};
			ui.ctx().set_cursor_icon(icon);
		}

		// Double-click a note: its properties.
		if response.double_clicked()
			&& let Some(pos) = response.interact_pointer_pos()
			&& let Some((n, _)) = hit(pos)
		{
			self.gesture = None;
			self.props = Some(Props {
				note: n,
				pos: pos + vec2(12.0, 12.0),
			});
		}

		// Press: start a gesture.
		if pressed
			&& response.hovered()
			&& self.erase.is_none()
			&& let Some(pos) = pointer
		{
			self.props = None;
			let abs = tl.x_to_tick(pos.x - area.left());
			let pitch = v.y_to_pitch(pos.y - area.top());
			let under = hit(pos);
			if mods.command {
				// Ctrl: select (rubber band; a click toggles the note under the pointer).
				self.gesture = Some(Gesture::Rubber {
					from: (abs, pos.y - area.top() + v.scroll_y),
					to: pos,
					add: true,
					note: under.map(|(n, _)| n.id),
				});
			} else if v.tool == Tool::Slice {
				self.gesture = Some(Gesture::Slice {
					at: piano::snap_abs(abs, snap),
					from_y: pos.y - area.top() + v.scroll_y,
					to: pos,
				});
			} else if let Some((n, edge)) = under {
				let in_sel = v.selected.contains(&n.id);
				if !in_sel {
					state.view.piano.select([n.id], false);
				}
				let ids = state.view.piano.selected.clone();
				self.gesture = Some(match edge {
					Edge::End | Edge::Start if !mods.shift => Gesture::Resize {
						ids,
						anchor: clip.start + if edge == Edge::End { n.end() } else { n.start },
						press_tick: abs,
						delta: Tick::ZERO,
						left: edge == Edge::Start,
					},
					_ => Gesture::Move {
						ids,
						anchor_start: clip.start + n.start,
						press_tick: abs,
						press_pitch: pitch,
						dt: Tick::ZERO,
						dp: 0,
						moved: false,
						note: n.id,
						copy: mods.shift,
					},
				});
				self.sound(state, t.track, n.pitch, n.velocity);
				state.view.piano.last_length = n.length;
				state.view.piano.last_velocity = n.velocity;
			} else {
				match v.tool {
					Tool::Draw => {
						let anchor = piano::snap_floor(abs, snap);
						state.view.piano.selected.clear();
						self.gesture = Some(Gesture::Draw {
							start: Tick((anchor - clip.start).0.max(0)),
							pitch,
							anchor,
							press_tick: abs,
						});
						self.sound(state, t.track, pitch, v.last_velocity);
					}
					Tool::Paint => {
						let spacing = match snap {
							Some(s) => {
								Tick(((v.last_length.0 + s.0 - 1) / s.0.max(1)).max(1) * s.0.max(1))
							}
							None => v.last_length,
						};
						let first = piano::snap_floor(abs, snap);
						state.view.piano.selected.clear();
						self.gesture = Some(Gesture::Paint {
							first,
							spacing: Tick(spacing.0.max(MIN_NOTE_LENGTH.0)),
							cells: [(0, pitch)].into_iter().collect(),
							last: 0,
						});
						self.sound(state, t.track, pitch, v.last_velocity);
					}
					Tool::Select | Tool::Slice => {
						self.gesture = Some(Gesture::Rubber {
							from: (abs, pos.y - area.top() + v.scroll_y),
							to: pos,
							add: mods.shift,
							note: None,
						});
					}
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
					start,
					pitch: p,
					anchor,
					press_tick,
				} => {
					let at = piano::snap_abs(*anchor + (abs - *press_tick), snap);
					*start = Tick((at - clip.start).0.max(0));
					if *p != pitch {
						*p = pitch;
						sound = Some((pitch, v.last_velocity));
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
					note,
					..
				} => {
					let raw = *anchor_start + (abs - *press_tick);
					let snapped = piano::snap_abs(raw, snap);
					let (t_off, p_off) = piano::clamp_move(
						clip,
						ids,
						snapped - *anchor_start,
						i32::from(pitch) - i32::from(*press_pitch),
					);
					if t_off != *dt || p_off != *dp {
						*moved = true;
						if p_off != *dp
							&& let Some(n) = clip.notes().iter().find(|n| n.id == *note)
						{
							let p = (i32::from(n.pitch) + p_off).clamp(0, 127) as u8;
							sound = Some((p, n.velocity));
						}
					}
					*dt = t_off;
					*dp = p_off;
				}
				Gesture::Resize {
					anchor,
					press_tick,
					delta,
					..
				} => {
					let at = piano::snap_abs(*anchor + (abs - *press_tick), snap);
					*delta = at - *anchor;
				}
				Gesture::Rubber { to, .. } | Gesture::Slice { to, .. } => *to = pos,
				Gesture::Paint {
					first,
					spacing,
					cells,
					last,
				} => {
					let k = piano::paint_cell(*first, *spacing, abs);
					let (lo, hi) = (k.min(*last), k.max(*last));
					for cell in lo..=hi {
						if !cells.contains_key(&cell)
							&& *first + Tick(cell * spacing.0) >= clip.start
						{
							cells.insert(cell, pitch);
							sound = Some((pitch, v.last_velocity));
						}
					}
					*last = k;
				}
			}
			if let Some((pitch, velocity)) = sound {
				self.sound(state, t.track, pitch, velocity);
			}
		}

		// Release: commit one command.
		if released && let Some(gesture) = self.gesture.take() {
			self.silence(state, t.track);
			match gesture {
				Gesture::Rubber {
					from,
					to,
					add,
					note,
				} => {
					let a = pos2(
						area.left() + tl.tick_to_x(from.0),
						area.top() + from.1 - v.scroll_y,
					);
					let clicked = a.distance(to) < 3.0;
					if clicked {
						match note {
							Some(id) if v.selected.contains(&id) => {
								state.view.piano.selected.remove(&id);
							}
							Some(id) => state.view.piano.select([id], true),
							None if !add => state.view.piano.selected.clear(),
							None => {}
						}
					} else {
						let t1 = tl.x_to_tick(to.x - area.left());
						let y1 = to.y - area.top() + v.scroll_y;
						let pitch =
							|y: f32| (127.0 - (y / v.row_height).floor()).clamp(0.0, 127.0) as u8;
						let ids = piano::notes_in_rect(
							clip,
							from.0 - clip.start,
							t1 - clip.start,
							pitch(from.1),
							pitch(y1),
						);
						state.view.piano.select(ids, add);
					}
				}
				Gesture::Slice { at, from_y, to } => {
					let y1 = to.y - area.top() + v.scroll_y;
					let pitch =
						|y: f32| (127.0 - (y / v.row_height).floor()).clamp(0.0, 127.0) as u8;
					if let Some(cmd) = piano::slice_notes(
						state.project(),
						clip.id,
						at - clip.start,
						pitch(from_y),
						pitch(y1),
					) {
						state.apply(cmd);
					}
				}
				gesture => commit(state, clip, gesture, &v),
			}
		}

		// Right button: delete every note the pointer passes over (one step on release).
		if r_pressed
			&& response.hovered()
			&& self.gesture.is_none()
			&& let Some(pos) = pointer
		{
			let a = (pos.x - area.left(), pos.y - area.top());
			self.erase = Some(Erase {
				hits: piano::sweep_hits(clip, &v, a, a),
				last: pos,
				start: pos,
				moved: false,
			});
		}
		if let Some(e) = self.erase.as_mut() {
			if let Some(pos) = pointer {
				if area.contains(pos) && area.contains(e.last) {
					let a = (e.last.x - area.left(), e.last.y - area.top());
					let b = (pos.x - area.left(), pos.y - area.top());
					e.hits.extend(piano::sweep_hits(clip, &v, a, b));
				}
				e.last = pos;
				e.moved |= pos.distance(e.start) > 4.0;
			}
			if !r_down {
				let hits = std::mem::take(&mut e.hits);
				let click = !e.moved;
				self.erase = None;
				// FL Studio: a right-click on empty space deselects everything.
				if hits.is_empty() && click {
					state.view.piano.selected.clear();
				}
				state.view.piano.selected.retain(|id| !hits.contains(id));
				if let Some(cmd) = piano::delete_notes(state.project(), clip.id, &hits) {
					state.apply(cmd);
				}
			}
		}

		// Notes (with the gesture preview applied), culled to the visible rows/columns.
		let v = &state.view.piano;
		let (move_off, resize_off) = match &self.gesture {
			Some(Gesture::Move {
				ids, dt, dp, copy, ..
			}) => (Some((ids, *dt, *dp, *copy)), None),
			Some(Gesture::Resize {
				ids, delta, left, ..
			}) => (None, Some((ids, *delta, *left))),
			_ => (None, None),
		};
		let erased = self.erase.as_ref().map(|e| &e.hits);
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
		let (t_lo, t_hi) = (
			tl.x_to_tick(0.0) - clip.start,
			tl.x_to_tick(area.width()) - clip.start,
		);
		for n in clip.notes() {
			if erased.is_some_and(|h| h.contains(&n.id)) {
				continue;
			}
			let mut start = n.start;
			let mut pitch = n.pitch;
			let mut length = n.length;
			let mut ghost_origin = false;
			if let Some((ids, dt, dp, copy)) = move_off
				&& ids.contains(&n.id)
				&& (dt.0 != 0 || dp != 0)
			{
				if copy {
					// Duplicate drag: the original stays, the copy is drawn moved.
					let r = note_rect(v, area, clip.start, n.start, n.length, n.pitch);
					if r.intersects(area) {
						paint_note(&pr, r, t.color, n.velocity, false, n.pitch, v.row_height);
					}
				} else {
					ghost_origin = true;
				}
				start = start + dt;
				pitch = (i32::from(pitch) + dp).clamp(0, 127) as u8;
			}
			if let Some((ids, d, left)) = resize_off
				&& ids.contains(&n.id)
			{
				if left {
					let s = Tick(
						(n.start + d)
							.0
							.clamp(0, (n.end() - MIN_NOTE_LENGTH).0.max(0)),
					);
					length = n.end() - s;
					start = s;
				} else {
					length = Tick((length + d).0.max(MIN_NOTE_LENGTH.0));
				}
			}
			if !visible_pitch(pitch) || start > t_hi || start + length < t_lo {
				continue;
			}
			if ghost_origin {
				let orig = note_rect(v, area, clip.start, n.start, n.length, n.pitch);
				pr.rect_stroke(
					orig,
					CornerRadius::same(2),
					Stroke::new(1.0, theme::TEXT_FAINT),
					StrokeKind::Inside,
				);
			}
			let r = note_rect(v, area, clip.start, start, length, pitch);
			let selected = v.selected.contains(&n.id) || in_rubber(r);
			paint_note(&pr, r, t.color, n.velocity, selected, pitch, v.row_height);
		}
		match &self.gesture {
			// The note being placed.
			Some(Gesture::Draw { start, pitch, .. }) => {
				let r = note_rect(v, area, clip.start, *start, v.last_length, *pitch);
				paint_note(&pr, r, t.color, v.last_velocity, true, *pitch, v.row_height);
			}
			Some(Gesture::Paint {
				first,
				spacing,
				cells,
				..
			}) => {
				for (k, pitch) in cells {
					let s = *first + Tick(k * spacing.0) - clip.start;
					let r = note_rect(v, area, clip.start, s, v.last_length, *pitch);
					paint_note(&pr, r, t.color, v.last_velocity, true, *pitch, v.row_height);
				}
			}
			Some(Gesture::Slice { at, from_y, to }) => {
				let x = area.left() + tl.tick_to_x(*at);
				let y0 = area.top() + from_y - v.scroll_y;
				let (a, b) = (y0.min(to.y), y0.max(to.y));
				let (a, b) = (
					a - (a - area.top() + v.scroll_y).rem_euclid(v.row_height),
					b - (b - area.top() + v.scroll_y).rem_euclid(v.row_height) + v.row_height,
				);
				pr.vline(x, a..=b, Stroke::new(2.0, theme::NOTE_SELECTED));
			}
			_ => {}
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

	/// The note properties popover (double-click a note); each finished edit is one
	/// command.
	fn properties(&mut self, ui: &mut Ui, state: &mut AppState, t: &Target) {
		let Some(props) = self.props.as_mut() else {
			return;
		};
		if !t.clip.notes().iter().any(|n| n.id == props.note.id) {
			self.props = None;
			return;
		}
		let mut commit = false;
		let mut close = false;
		let area = egui::Area::new(Id::new("piano-note-props"))
			.order(egui::Order::Foreground)
			.fixed_pos(props.pos)
			.show(ui.ctx(), |ui| {
				egui::Frame::popup(ui.style()).show(ui, |ui| {
					ui.set_max_width(180.0);
					ui.horizontal(|ui| {
						ui.label(
							RichText::new("NOTE")
								.font(theme::small())
								.color(theme::TEXT_FAINT),
						);
						ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
							close = ui.small_button("×").on_hover_text("Close (Esc)").clicked();
						});
					});
					let n = &mut props.note;
					let mut done = |r: egui::Response| {
						commit |= (r.changed() && !r.dragged()) || r.drag_stopped();
					};
					egui::Grid::new("piano-note-props-grid")
						.num_columns(2)
						.spacing(vec2(8.0, 4.0))
						.show(ui, |ui| {
							ui.label("Pitch");
							done(
								ui.add(
									egui::DragValue::new(&mut n.pitch)
										.range(0..=127)
										.custom_formatter(|p, _| {
											cadenza_theory::format_pitch(p as u8, false)
										}),
								),
							);
							ui.end_row();
							ui.label("Velocity");
							let mut vel = (n.velocity * 100.0).round();
							let r = ui.add(
								egui::DragValue::new(&mut vel)
									.range(0.0..=100.0)
									.suffix(" %"),
							);
							n.velocity = vel / 100.0;
							done(r);
							ui.end_row();
							ui.label("Start");
							let mut start = n.start.beats();
							let r = ui.add(
								egui::DragValue::new(&mut start)
									.range(0.0..=100_000.0)
									.speed(0.0625)
									.max_decimals(3)
									.suffix(" beats"),
							);
							n.start = Tick::from_beats(start);
							done(r);
							ui.end_row();
							ui.label("Length");
							let mut length = n.length.beats();
							let r = ui.add(
								egui::DragValue::new(&mut length)
									.range(MIN_NOTE_LENGTH.beats()..=10_000.0)
									.speed(0.0625)
									.max_decimals(3)
									.suffix(" beats"),
							);
							n.length = Tick::from_beats(length);
							done(r);
							ui.end_row();
						});
				});
			});
		let outside = ui.input(|i| i.pointer.any_pressed())
			&& !area.response.contains_pointer()
			&& !ui.ctx().is_being_dragged(area.response.id);
		if commit {
			let note = props.note.clone();
			if let Some(cmd) = piano::set_note(state.project(), t.clip.id, &note) {
				state.apply(cmd);
			}
			if let Some(n) = state
				.project()
				.find_clip(t.clip.id)
				.and_then(|(_, c)| c.notes().iter().find(|n| n.id == note.id))
				.cloned()
			{
				state.view.piano.last_length = n.length;
				state.view.piano.last_velocity = n.velocity;
				if let Some(p) = self.props.as_mut() {
					p.note = n;
				}
			}
		}
		if close || (outside && !commit) {
			self.props = None;
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
		// Dragging along the keys glides from note to note.
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
		let (pressed, r_pressed, down, alt) = ui.input(|i| {
			(
				i.pointer.primary_pressed(),
				i.pointer.button_pressed(PointerButton::Secondary),
				i.pointer.primary_down() || i.pointer.secondary_down(),
				i.modifiers.alt,
			)
		});
		let bar_x = |n: &Note| lane.left() + tl.tick_to_x(clip.start + n.start);
		let near = |n: &Note, x: f32| {
			let x0 = bar_x(n);
			let x1 = x0 + note_px(&v, n.length).max(6.0);
			(x0 - 3.0..=x1).contains(&x)
		};
		if (pressed || r_pressed)
			&& response.hovered()
			&& let Some(pos) = ui.ctx().pointer_latest_pos()
		{
			let at = (tl.x_to_tick(pos.x - lane.left()), value_at(pos.y));
			self.velocity = Some(if r_pressed || alt {
				VelocityDrag::Line {
					from: at,
					to: at,
					values: HashMap::new(),
				}
			} else {
				let anchor = clip
					.notes()
					.iter()
					.filter(|n| near(n, pos.x))
					.min_by(|a, b| {
						(bar_x(a) - pos.x)
							.abs()
							.total_cmp(&(bar_x(b) - pos.x).abs())
					});
				VelocityDrag::Paint {
					values: HashMap::new(),
					relative: anchor
						.filter(|n| v.selected.contains(&n.id) && v.selected.len() > 1)
						.map(|_| value_at(pos.y)),
				}
			});
		}
		if let (Some(drag), Some(pos)) = (self.velocity.as_mut(), ui.ctx().pointer_latest_pos()) {
			let value = value_at(pos.y);
			match drag {
				VelocityDrag::Paint {
					values,
					relative: Some(press),
				} => {
					for n in clip.notes().iter().filter(|n| v.selected.contains(&n.id)) {
						values.insert(n.id, (n.velocity + value - *press).clamp(0.0, 1.0));
					}
				}
				VelocityDrag::Paint { values, .. } => {
					for n in clip.notes().iter().filter(|n| {
						near(n, pos.x) && (v.selected.is_empty() || v.selected.contains(&n.id))
					}) {
						values.insert(n.id, value);
					}
				}
				VelocityDrag::Line { from, to, values } => {
					*to = (tl.x_to_tick(pos.x - lane.left()), value);
					*values = piano::velocity_line(clip, &v.selected, *from, *to);
				}
			}
		}
		if !down && let Some(drag) = self.velocity.take() {
			let values = drag.values();
			if let Some(cmd) = piano::set_velocities(state.project(), clip.id, values) {
				state.apply(cmd);
			}
			if let Some(last) = values.values().last() {
				state.view.piano.last_velocity = *last;
			}
		}
		let preview = self.velocity.as_ref().map(VelocityDrag::values);
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
				theme::NOTE_SELECTED
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
		if let Some(VelocityDrag::Line { from, to, .. }) = &self.velocity {
			let y = |v: f32| inner.bottom() - v * inner.height();
			p.line_segment(
				[
					pos2(lane.left() + tl.tick_to_x(from.0), y(from.1)),
					pos2(lane.left() + tl.tick_to_x(to.0), y(to.1)),
				],
				Stroke::new(1.5, theme::NOTE_SELECTED),
			);
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

	/// Piano-roll keys (only while the piano roll has the focus), FL Studio defaults.
	/// Consumes what it uses (so Ctrl+D / Ctrl+L / Ctrl+B mean the piano roll's here).
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
		let cmd = Modifiers::COMMAND;
		if self.props.is_some() && pressed(Modifiers::NONE, Key::Escape) {
			self.props = None;
			return;
		}
		let clip = t.clip.id;
		let sel = state.view.piano.selected.clone();
		for tool in Tool::ALL {
			let key = match tool {
				Tool::Draw => Key::P,
				Tool::Paint => Key::B,
				Tool::Select => Key::E,
				Tool::Slice => Key::C,
			};
			if pressed(Modifiers::NONE, key) {
				state.view.piano.tool = tool;
			}
		}
		if pressed(cmd, Key::A) {
			state
				.view
				.piano
				.select(t.clip.notes().iter().map(|n| n.id), false);
		}
		if pressed(cmd, Key::D) || pressed(Modifiers::NONE, Key::Escape) {
			state.view.piano.selected.clear();
		}
		let playhead = state.position();
		if pressed(cmd, Key::C) && !sel.is_empty() {
			state.view.piano.clipboard = piano::clipboard(&t.clip, &sel, playhead);
			state.notices.info(format!("Copied {} notes", sel.len()));
		}
		if pressed(cmd, Key::X) && !sel.is_empty() {
			state.view.piano.clipboard = piano::clipboard(&t.clip, &sel, playhead);
			if let Some(cmd) = piano::cut_notes(state.project(), clip, &sel) {
				state.apply(cmd);
			}
		}
		if pressed(cmd, Key::V) {
			let board = state.view.piano.clipboard.clone();
			let at = board.paste_at(playhead, state.is_playing()) - t.clip.start;
			if let Some((cmd, ids)) = piano::paste_notes(state.project(), clip, &board.notes, at)
				&& state.apply(cmd)
			{
				state.view.piano.select(ids, false);
			}
		}
		if pressed(cmd, Key::B) {
			let step = snap_step(state);
			match piano::clone_notes(state.project(), clip, &sel, step) {
				Some((cmd, ids)) => {
					if state.apply(cmd) {
						state.view.piano.select(ids, false);
					}
				}
				None => state
					.notices
					.info("Select notes to clone (Ctrl+A selects all)"),
			}
		}
		if (pressed(Modifiers::NONE, Key::Delete) || pressed(Modifiers::NONE, Key::Backspace))
			&& let Some(cmd) = piano::delete_notes(state.project(), clip, &sel)
		{
			state.apply(cmd);
		}
		if pressed(cmd, Key::Q) || pressed(Modifiers::NONE, Key::Q) {
			quantize(state);
		}
		if pressed(cmd, Key::U) {
			match piano::glue_notes(state.project(), clip, &sel) {
				Some(cmd) => {
					state.apply(cmd);
				}
				None => state
					.notices
					.info("Glue joins selected notes of the same pitch: select two or more"),
			}
		}
		if pressed(cmd, Key::L) {
			let ids = piano::targets(&t.clip, &sel);
			if let Some(cmd) = piano::legato_notes(state.project(), clip, &ids) {
				state.apply(cmd);
			}
		}
		let step = key_step(state);
		let bar = signature(state).ticks_per_bar();
		// Shifted (and Ctrl) variants first: a plain arrow also matches Shift+arrow.
		let moves = [
			(Modifiers::SHIFT, Key::ArrowUp, Tick::ZERO, 12),
			(Modifiers::SHIFT, Key::ArrowDown, Tick::ZERO, -12),
			(cmd, Key::ArrowUp, Tick::ZERO, 12),
			(cmd, Key::ArrowDown, Tick::ZERO, -12),
			(Modifiers::NONE, Key::ArrowUp, Tick::ZERO, 1),
			(Modifiers::NONE, Key::ArrowDown, Tick::ZERO, -1),
			(Modifiers::SHIFT, Key::ArrowLeft, Tick(-bar.0), 0),
			(Modifiers::SHIFT, Key::ArrowRight, bar, 0),
			(Modifiers::NONE, Key::ArrowLeft, Tick(-step.0), 0),
			(Modifiers::NONE, Key::ArrowRight, step, 0),
		];
		let ids = piano::targets(&t.clip, &sel);
		for (m, k, dt, dp) in moves {
			if pressed(m, k)
				&& let Some(cmd) = piano::move_notes(state.project(), clip, &ids, dt, dp)
			{
				state.apply(cmd);
				// Audition the transposed notes' lowest one.
				if dp != 0
					&& let Some(n) = t
						.clip
						.notes()
						.iter()
						.filter(|n| ids.contains(&n.id))
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
		self.erase = None;
		self.velocity = None;
		self.props = None;
		self.key = None;
		self.sounding = None;
		state.release_previews();
	}
}

/// Commits a finished note gesture as one command.
fn commit(state: &mut AppState, clip: &Clip, gesture: Gesture, v: &PianoView) {
	let id = clip.id;
	match gesture {
		Gesture::Draw { start, pitch, .. } => {
			if let Some((cmd, note)) = piano::add_note(
				state.project(),
				id,
				start,
				pitch,
				v.last_length,
				v.last_velocity,
			) && state.apply(cmd)
			{
				state.view.piano.select([note], false);
			}
		}
		Gesture::Paint {
			first,
			spacing,
			cells,
			..
		} => {
			let cells: Vec<(Tick, u8)> = cells
				.iter()
				.map(|(k, pitch)| (first + Tick(k * spacing.0) - clip.start, *pitch))
				.collect();
			if let Some((cmd, ids)) =
				piano::paint_notes(state.project(), id, &cells, v.last_length, v.last_velocity)
				&& state.apply(cmd)
			{
				state.view.piano.select(ids, false);
			}
		}
		Gesture::Move {
			ids,
			dt,
			dp,
			moved,
			note,
			copy,
			..
		} => {
			if !moved {
				// A click on a note of a group narrows the selection to it.
				state.view.piano.select([note], false);
				return;
			}
			if copy {
				if let Some((cmd, copies)) = piano::copy_moved(state.project(), id, &ids, dt, dp)
					&& state.apply(cmd)
				{
					state.view.piano.select(copies, false);
				}
			} else if let Some(cmd) = piano::move_notes(state.project(), id, &ids, dt, dp) {
				state.apply(cmd);
			}
		}
		Gesture::Resize {
			ids, delta, left, ..
		} => {
			let cmd = if left {
				piano::resize_start_notes(state.project(), id, &ids, delta)
			} else {
				piano::resize_notes(state.project(), id, &ids, delta)
			};
			if let Some(cmd) = cmd
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
		Gesture::Rubber { .. } | Gesture::Slice { .. } => {}
	}
}

/// Quantizes the selection (or the whole clip) to the snap (else the visible grid).
pub fn quantize(state: &mut AppState) {
	let Some(clip) = state.view.piano.clip else {
		return;
	};
	let step = key_step(state);
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
	// Brighter for louder notes; selected notes turn warm red (FL Studio).
	let base = if selected {
		theme::NOTE_SELECTED
	} else {
		theme::mix(color, Color32::WHITE, 0.15)
	};
	let fill = theme::mix(
		theme::mix(base, theme::BG_DEEP, 0.55),
		base,
		0.35 + 0.65 * velocity,
	);
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
			Stroke::new(1.5, theme::mix(theme::NOTE_SELECTED, Color32::WHITE, 0.6))
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
	let meter = cadenza_core::MeterMap::new(state.project().meter());
	let p = ui.painter_at(r);
	p.rect_filled(r, 0.0, theme::BG_RAISED);
	let x0 = r.left() + tl.tick_to_x(t.clip.start);
	let x1 = r.left() + tl.tick_to_x(t.clip.end());
	p.rect_filled(
		Rect::from_min_max(pos2(x0, r.bottom() - 4.0), pos2(x1, r.bottom())),
		0.0,
		t.color,
	);
	// Pattern mode: the span Play loops (the clip, at least a bar).
	let config = state.transport_config().0;
	if config.pattern == Some(t.clip.id)
		&& let Some((s, e)) = config.loop_range
	{
		let band = Rect::from_min_max(
			pos2(r.left() + tl.tick_to_x(s), r.top() + 1.0),
			pos2(r.left() + tl.tick_to_x(e), r.top() + 6.0),
		);
		p.rect_filled(band, CornerRadius::same(2), theme::PATTERN);
	}
	let (from, to) = (tl.x_to_tick(0.0), tl.x_to_tick(r.width()));
	let shortest = meter
		.view()
		.segments_between(Tick(from.0.max(0)), to)
		.map(|(_, ts)| ts.ticks_per_bar())
		.min()
		.unwrap_or(Tick(PPQ * 4));
	let bar_px = tl.ticks_to_px(shortest);
	let every = ((40.0 / bar_px).ceil().max(1.0) as u64).next_power_of_two() as i64;
	for line in tl.grid_lines(meter.view(), from, to, None) {
		let x = r.left() + tl.tick_to_x(line.tick);
		match line.kind {
			LineKind::Bar(index) => {
				p.vline(
					x,
					r.top() + 8.0..=r.bottom(),
					Stroke::new(1.0, theme::TEXT_FAINT),
				);
				if index % every == 0 {
					p.text(
						pos2(x + 4.0, r.top() + 2.0),
						Align2::LEFT_TOP,
						format!("{}", index + 1),
						theme::mono(10.0),
						theme::TEXT_DIM,
					);
				}
			}
			LineKind::Beat => {
				let (bar, beat_no, _) = timeline::bbt(line.tick, meter.view());
				p.vline(
					x,
					r.bottom() - 5.0..=r.bottom(),
					Stroke::new(1.0, theme::GRID_BAR),
				);
				if bar_px > 160.0 {
					p.text(
						pos2(x + 3.0, r.top() + 4.0),
						Align2::LEFT_TOP,
						format!("{bar}.{beat_no}"),
						theme::mono(8.5),
						theme::TEXT_FAINT,
					);
				}
			}
			LineKind::Step => {}
		}
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
			tl.snap(tick, meter.view())
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

/// The track's MIDI clips (in time order, with their bar): switches the edited clip.
fn clip_selector(ui: &mut Ui, state: &mut AppState, t: &Target) {
	let meter = cadenza_core::MeterMap::new(state.project().meter());
	let clips: Vec<(cadenza_core::ClipId, String)> = state
		.project()
		.track(t.track)
		.map(|track| {
			let mut c: Vec<&Clip> = track.clips.iter().filter(|c| c.is_midi()).collect();
			c.sort_by_key(|c| c.start);
			c.into_iter()
				.map(|c| {
					let bar = meter.view().bar_index(c.start) + 1;
					(c.id, format!("{} · bar {bar}", c.name))
				})
				.collect()
		})
		.unwrap_or_default();
	let current = clips
		.iter()
		.position(|(id, _)| *id == t.clip.id)
		.unwrap_or(0);
	let mut pick = None;
	egui::ComboBox::from_id_salt("piano-clip")
		.width(150.0)
		.selected_text(RichText::new(&t.clip.name).strong().color(theme::TEXT))
		.show_ui(ui, |ui| {
			for (i, (id, label)) in clips.iter().enumerate() {
				if ui.selectable_label(i == current, label).clicked() {
					pick = Some(*id);
				}
			}
		})
		.response
		.on_hover_text("Clips of this track: pick one to edit it here");
	if let Some(id) = pick.filter(|id| *id != t.clip.id) {
		state.open_piano_roll(id);
	}
}

fn empty(ui: &mut Ui, state: &mut AppState, rect: Rect) {
	let p = ui.painter_at(rect);
	p.text(
		rect.center() - vec2(0.0, 22.0),
		Align2::CENTER_CENTER,
		"No clip open",
		FontId::proportional(14.0),
		theme::TEXT_DIM,
	);
	p.text(
		rect.center(),
		Align2::CENTER_CENTER,
		"Press F7 or double-click an instrument track to open the piano roll \
		 (double-click a MIDI clip to edit that one)",
		theme::small(),
		theme::TEXT_FAINT,
	);
	let track = state.selection.track.and_then(|id| {
		let t = state.project().track(id)?;
		(t.kind == cadenza_model::TrackKind::Instrument).then(|| (id, t.name.clone()))
	});
	if let Some((id, name)) = track {
		let button = Rect::from_center_size(rect.center() + vec2(0.0, 30.0), vec2(240.0, 24.0));
		if ui
			.put(
				button,
				egui::Button::new(format!("Open a clip of '{name}'")),
			)
			.clicked()
		{
			state.open_track_in_piano_roll(id);
		}
	}
}

/// The note grid (FL Studio-like): bars shaded alternately (a discreet zebra), and lines
/// by hierarchy: bar lines strongest, beat lines medium, step lines (the snap grid, or
/// the zoom grid) faint.
fn piano_grid(
	p: &Painter,
	area: Rect,
	tl: &Timeline,
	meter: cadenza_core::Meter<'_>,
	step: Option<Tick>,
) {
	let (from, to) = (tl.x_to_tick(0.0), tl.x_to_tick(area.width()));
	for bar in meter.bar_lines(meter.bar_floor(Tick(from.0.max(0))), Tick(to.0 + 1)) {
		if bar.index % 2 == 1 {
			let x0 = (area.left() + tl.tick_to_x(bar.start)).max(area.left());
			let x1 = (area.left() + tl.tick_to_x(bar.end())).min(area.right());
			if x1 > x0 {
				p.rect_filled(
					Rect::from_min_max(pos2(x0, area.top()), pos2(x1, area.bottom())),
					0.0,
					theme::PIANO_ZEBRA,
				);
			}
		}
	}
	for line in tl.grid_lines(meter, from, to, step) {
		let x = area.left() + tl.tick_to_x(line.tick);
		let stroke = match line.kind {
			LineKind::Bar(_) => Stroke::new(1.5, theme::PIANO_GRID_BAR),
			LineKind::Beat => Stroke::new(1.0, theme::PIANO_GRID_BEAT),
			LineKind::Step => Stroke::new(1.0, theme::PIANO_GRID_STEP),
		};
		p.vline(x, area.y_range(), stroke);
	}
}
