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

use crate::i18n::{tr, trf};
use std::collections::{BTreeMap, BTreeSet, HashMap};

use cadenza_core::{NoteId, PPQ, Tick, TrackId};
use cadenza_model::{Clip, Controller, ControllerPoint, Note};
use cadenza_theory::{PitchClass, Scale, ScaleKind};
use egui::{
	Align2, Color32, CornerRadius, CursorIcon, FontId, Id, Key, KeyboardShortcut, Modifiers,
	Painter, PointerButton, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, Ui, pos2, vec2,
};

use crate::state::AppState;
use crate::state::layout::PanelKind;
use crate::state::piano::{self, EventLane, Ghosts, MIN_NOTE_LENGTH, PianoSnap, PianoView, Tool};
use crate::state::timeline::{self, LineKind, Timeline};
use crate::theme;
use crate::ui::arrange;
use crate::ui::widgets;

const TOOLBAR: f32 = 34.0;
const RULER: f32 = 24.0;
/// The lane tabs row above the event lane.
const LANE_TABS: f32 = 28.0;
const VELOCITY_LANE: f32 = 110.0;
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

/// A gesture in a controller lane (clip-relative ticks).
#[derive(Debug, Clone)]
enum ControlDrag {
	/// Pencil: one value per grid cell the pointer crosses.
	Paint {
		cells: BTreeMap<Tick, f32>,
		last: Option<(Tick, f32)>,
	},
	/// Alt+drag or right-drag: a straight ramp; a right-click without moving deletes the
	/// event under the pointer instead.
	Line {
		from: (Tick, f32),
		to: (Tick, f32),
		press: Pos2,
		moved: bool,
	},
}

/// The note properties popover (double-click a note).
#[derive(Debug, Clone)]
struct Props {
	note: Note,
	pos: Pos2,
}

/// Alt+wheel over notes (FL Studio): their velocities move by `delta`, previewed until
/// the wheel rests, then committed as one command.
#[derive(Debug, Clone)]
struct WheelVelocity {
	clip: cadenza_core::ClipId,
	ids: BTreeSet<NoteId>,
	delta: f32,
	/// Input time of the last wheel movement.
	last: f64,
}

/// The wheel rests this long (seconds) before its velocity change is committed.
const WHEEL_SETTLE: f64 = 0.6;

/// View-local state of the piano roll.
#[derive(Default)]
pub struct PianoUi {
	gesture: Option<Gesture>,
	wheel_velocity: Option<WheelVelocity>,
	erase: Option<Erase>,
	velocity: Option<VelocityDrag>,
	control: Option<ControlDrag>,
	props: Option<Props>,
	/// Key held on the keyboard (auditioning).
	key: Option<u8>,
	/// Pitch sounding for the current note gesture.
	sounding: Option<u8>,
	/// Row under the pointer in the grid this frame (the keyboard highlights its key).
	hover_pitch: Option<u8>,
	pan: bool,
	/// Grid width/height of the last frame (keyboard navigation, framing).
	width: f32,
	height: f32,
	/// Note grid of the last frame (tests drive the mouse with it).
	pub grid_rect: Option<Rect>,
	/// Event lane of the last frame (tests).
	pub lane_rect: Option<Rect>,
}

#[derive(Debug, Clone, Copy)]
struct Geometry {
	toolbar: Rect,
	keys: Rect,
	ruler: Rect,
	grid: Rect,
	/// The lane tabs row (full width) above the event lane.
	tabs: Option<Rect>,
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
			VELOCITY_LANE.min((bottom - top) * 0.25)
		} else {
			0.0
		};
		let tabs_h = if velocity_lane { LANE_TABS } else { 0.0 };
		let lane_top = bottom - vel_h;
		let grid_bottom = lane_top - tabs_h;
		Self {
			toolbar: Rect::from_min_max(full.min, pos2(full.right(), full.top() + TOOLBAR)),
			keys: Rect::from_min_max(pos2(full.left(), top), pos2(x0, grid_bottom)),
			ruler: Rect::from_min_max(pos2(x0, full.top() + TOOLBAR), pos2(right, top)),
			grid: Rect::from_min_max(pos2(x0, top), pos2(right, grid_bottom)),
			tabs: velocity_lane.then(|| {
				Rect::from_min_max(pos2(full.left(), grid_bottom), pos2(full.right(), lane_top))
			}),
			velocity: velocity_lane
				.then(|| Rect::from_min_max(pos2(x0, lane_top), pos2(right, bottom))),
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
		self.lane_rect = g.velocity;
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
		self.wheel_velocity(ui, state, &g, &t);
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
		if t.clip.notes().is_empty() && self.gesture.is_none() {
			let p = ui.painter_at(g.grid);
			p.text(
				g.grid.center() - vec2(0.0, 10.0),
				Align2::CENTER_BOTTOM,
				tr("Empty clip"),
				theme::semibold(13.0),
				theme::TEXT_DIM,
			);
			p.text(
				g.grid.center(),
				Align2::CENTER_TOP,
				tr("Press P and click to draw notes, or ask the producer (F10) to write a part"),
				theme::small(),
				theme::TEXT_FAINT,
			);
		}
		self.keyboard(ui, state, &g, &t);
		if let (Some(r), Some(tabs)) = (g.velocity, g.tabs) {
			lane_tabs(ui, state, tabs, &t);
			lane_label(ui, state, r, &g, &t);
			match state.view.piano.event_lane {
				EventLane::Velocity => self.velocity_lane(ui, state, r, &t),
				EventLane::Controller(c) => self.controller_lane(ui, state, c, r, &t),
			}
		}
		ruler(ui, state, &g, &t);
		self.scrollbars(ui, state, &g, &t);
		self.toolbar(ui, state, &g, Some(&t));
		// Playhead (wraps with the pattern loop).
		let x = g.grid.left() + state.view.piano.timeline.tick_to_x(state.position());
		if (g.grid.left()..=g.grid.right()).contains(&x) {
			let p = ui.painter_at(g.ruler.union(g.grid));
			arrange::paint_playhead(&p, x, g.ruler.top()..=g.grid.bottom(), g.ruler.bottom());
			if let Some(lane) = g.velocity {
				ui.painter_at(lane)
					.vline(x, lane.y_range(), Stroke::new(1.0, theme::PLAYHEAD));
			}
		}
		self.properties(ui, state, &t);
	}

	/// Alt+wheel over a note: velocity of the note, or of the selection it belongs to
	/// (FL Studio). Previewed while the wheel turns; one command once it rests for
	/// [`WHEEL_SETTLE`] or Alt is released.
	fn wheel_velocity(&mut self, ui: &Ui, state: &mut AppState, g: &Geometry, t: &Target) {
		let (now, scroll, alt) = ui.input(|i| (i.time, i.smooth_scroll_delta.y, i.modifiers.alt));
		if let Some(w) = &self.wheel_velocity
			&& (!alt || (now - w.last > WHEEL_SETTLE && scroll == 0.0) || w.clip != t.clip.id)
		{
			let w = self.wheel_velocity.take().expect("checked");
			if let Some((_, clip)) = state.project().find_clip(w.clip) {
				let values = piano::nudge_velocities(clip, &w.ids, w.delta);
				if let Some(cmd) = piano::set_velocities(state.project(), w.clip, &values) {
					state.apply(cmd);
				}
			}
		}
		if !alt || scroll == 0.0 {
			return;
		}
		let Some(p) = ui
			.ctx()
			.pointer_hover_pos()
			.filter(|p| g.grid.contains(*p) && ui.ui_contains_pointer())
		else {
			return;
		};
		if self.wheel_velocity.is_none() {
			let v = &state.view.piano;
			let abs = v.timeline.x_to_tick(p.x - g.grid.left());
			let pitch = v.y_to_pitch(p.y - g.grid.top());
			let Some(n) = piano::note_at(&t.clip, abs - t.clip.start, pitch) else {
				return;
			};
			self.wheel_velocity = Some(WheelVelocity {
				clip: t.clip.id,
				ids: piano::wheel_targets(&v.selected, n.id),
				delta: 0.0,
				last: now,
			});
		}
		if let Some(w) = self.wheel_velocity.as_mut() {
			// One wheel notch (~40 points of scroll) = one velocity step.
			w.delta = (w.delta + scroll / 40.0 * piano::WHEEL_VELOCITY_STEP).clamp(-1.0, 1.0);
			w.last = now;
			ui.ctx()
				.request_repaint_after(std::time::Duration::from_secs_f64(WHEEL_SETTLE + 0.05));
		}
	}

	/// The previewed velocities of an Alt+wheel change on `clip`.
	fn wheel_preview(&self, clip: &Clip) -> Option<HashMap<NoteId, f32>> {
		self.wheel_velocity
			.as_ref()
			.filter(|w| w.clip == clip.id)
			.map(|w| piano::nudge_velocities(clip, &w.ids, w.delta))
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
			if alt && self.wheel_velocity.is_some() {
				// Alt+wheel over notes changes their velocity instead.
			} else if alt && scroll.y != 0.0 {
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

	/// Folds the piano roll toolbar to the width actually available ([`piano_fold`]), not a
	/// window-size tier, so any language's labels fit.
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
				.max_rect(r.shrink2(vec2(10.0, 0.0)))
				.layout(egui::Layout::left_to_right(egui::Align::Center)),
		);
		bar.set_clip_rect(r);
		bar.spacing_mut().item_spacing.x = 6.0;
		let fold = piano_fold(&bar, r.width());
		self.toolbar_rest(&mut bar, state, t, fold);
	}

	/// The piano roll toolbar's content at `fold` (picked by [`piano_fold`]).
	fn toolbar_rest(&self, ui: &mut Ui, state: &mut AppState, t: Option<&Target>, fold: PianoFold) {
		let compact = fold.compact;
		widgets::panel_title(ui, tr("Piano roll"), (!compact).then_some("F7"));
		if let Some(t) = t {
			clip_selector(ui, state, t, compact);
			if arrange::toolbar_chip(ui, false, "+", None, theme::BG_WIDGET, theme::TEXT)
				.on_hover_text(tr("New empty clip on this track (opens it here)"))
				.clicked()
			{
				let at = state.position();
				state.new_clip_in_piano_roll(t.track, at);
			}
		}
		arrange::toolbar_divider(ui);
		segment_group(ui, |ui| {
			for tool in Tool::ALL {
				let on = state.view.piano.tool == tool;
				let (label, key) = fold
					.tools
					.label_and_key(tool.glyph(), tool.name(), tool.key());
				if arrange::toolbar_chip(ui, on, &label, key, theme::BG_HOVER, theme::TEXT)
					.on_hover_text(format!("{}: {} ({})", tool.name(), tool.tip(), tool.key()))
					.clicked()
				{
					state.view.piano.tool = tool;
				}
			}
		});
		snap_chip(ui, state);
		key_chip(ui, &mut state.view.piano, compact);
		let v = &mut state.view.piano;
		if v.scale.is_some() {
			let on = v.scale_rows;
			if arrange::toolbar_chip(
				ui,
				on,
				if compact { "▤" } else { tr("▤ Scale rows") },
				None,
				theme::ACCENT_DIM,
				theme::ACCENT_TEXT,
			)
			.on_hover_text(tr(
				"Shade the rows outside the scale (in-scale rows one step lighter)",
			))
			.clicked()
			{
				v.scale_rows = !on;
			}
		}
		let ghosts = match v.ghosts {
			Ghosts::Off => tr("Off"),
			Ghosts::SameTrack => tr("This track"),
			Ghosts::AllTracks => tr("All tracks"),
		};
		chip_menu(
			ui,
			"piano-ghosts",
			(!compact).then_some(tr("Ghosts")),
			ghosts,
			tr("Show the notes of other clips dimmed behind this one"),
			|ui| {
				for gh in [Ghosts::Off, Ghosts::SameTrack, Ghosts::AllTracks] {
					ui.selectable_value(&mut v.ghosts, gh, gh.name());
				}
			},
		);
		let lanes = v.velocity_lane;
		if arrange::toolbar_chip(
			ui,
			lanes,
			tr("Events"),
			None,
			theme::ACCENT_DIM,
			theme::ACCENT_TEXT,
		)
		.on_hover_text(tr(
			"Show the event lane under the notes (velocity, mod wheel, sustain, pitch bend, any CC)",
		))
		.clicked()
		{
			v.velocity_lane = !lanes;
		}
		ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
			ui.spacing_mut().item_spacing.x = 4.0;
			if ui
				.add(
					egui::Button::new(RichText::new("×").size(15.0).color(theme::TEXT_DIM))
						.frame(false),
				)
				.on_hover_text(tr("Close the piano roll (F7)"))
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
					tr("Restore the layout (F11)")
				} else {
					tr("Maximize the piano roll (F11)")
				})
				.clicked()
			{
				state
					.view
					.panels
					.toggle_maximize_panel(PanelKind::PianoRoll);
			}
			let Some(t) = t else {
				return;
			};
			arrange::toolbar_divider(ui);
			let zoom = |ui: &mut Ui, label: &str, tip: &str| {
				arrange::toolbar_chip(ui, false, label, None, theme::BG_WIDGET, theme::TEXT)
					.on_hover_text(tip)
					.clicked()
			};
			if zoom(ui, "+", tr("Zoom in (Ctrl+wheel)")) {
				state.view.piano.timeline.zoom_around(1.5, self.width * 0.5);
			}
			if zoom(ui, "−", tr("Zoom out (Ctrl+wheel)")) {
				state
					.view
					.piano
					.timeline
					.zoom_around(1.0 / 1.5, self.width * 0.5);
			}
			if zoom(ui, tr("Fit"), tr("Zoom to the clip")) {
				state.view.piano.frame(&t.clip, self.width, self.height);
			}
			arrange::toolbar_divider(ui);
			if filled_chip(ui, tr("Quantize"), Some("Q"))
				.on_hover_text(tr(
					"Move the selected notes (or all) onto the snap grid (Q / Ctrl+Q)",
				))
				.clicked()
			{
				quantize(state);
			}
			if !compact {
				let n = state.view.piano.selected.len();
				let total = t.clip.notes().len();
				ui.label(
					RichText::new(if n == 0 {
						trf("{total} notes", &[("total", &total)])
					} else {
						trf("{n} of {total} selected", &[("n", &n), ("total", &total)])
					})
					.font(theme::small())
					.color(theme::TEXT_DIM),
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
			let scale = v.scale.filter(|_| v.scale_rows);
			p.rect_filled(row, 0.0, row_color(pitch, scale));
			if pitch % 12 == 0 {
				p.hline(
					area.x_range(),
					row.bottom() - 0.5,
					Stroke::new(1.0, theme::PIANO_GRID_BAR),
				);
			} else if v.row_height >= 9.0 {
				p.hline(
					area.x_range(),
					row.bottom() - 0.5,
					Stroke::new(1.0, theme::PIANO_ROW_LINE),
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
							// Outlined in their own colour, so they never look editable.
							p.rect_stroke(
								r.shrink2(vec2(0.0, 1.0)),
								CornerRadius::same(2),
								Stroke::new(1.0, color.gamma_multiply(0.45)),
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
		self.hover_pitch = hover.map(|pos| v.y_to_pitch(pos.y - area.top()));
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

		// Velocity tooltips: live while Alt+wheel changes it, else on a resting pointer.
		if let Some(values) = self.wheel_preview(clip) {
			let (lo, hi) = values
				.values()
				.fold((1.0f32, 0.0f32), |(lo, hi), v| (lo.min(*v), hi.max(*v)));
			let text = if values.len() == 1 || (hi - lo) < 0.005 {
				trf("Velocity {v}%", &[("v", &(hi * 100.0).round())])
			} else {
				trf(
					"Velocity {lo}–{hi}% ({n} notes)",
					&[
						("lo", &(lo * 100.0).round()),
						("hi", &(hi * 100.0).round()),
						("n", &values.len()),
					],
				)
			};
			egui::Tooltip::for_widget(&response)
				.at_pointer()
				.gap(12.0)
				.show(|ui| ui.label(text));
		} else if let Some(pos) = hover
			&& self.gesture.is_none()
			&& self.erase.is_none()
			&& let Some((n, _)) = hit(pos)
		{
			response.clone().on_hover_text_at_pointer(trf(
				"{note_name} · velocity {velocity}% (Alt+wheel)",
				&[
					("note_name", &cadenza_model::note_name(n.pitch)),
					("velocity", &(n.velocity * 100.0).round()),
				],
			));
		}

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
		let wheel = self.wheel_preview(clip);
		let vel = |n: &Note| {
			wheel
				.as_ref()
				.and_then(|w| w.get(&n.id))
				.copied()
				.unwrap_or(n.velocity)
		};
		let pr = ui.painter_at(area);
		let flats = piano::prefers_flats(v.scale);
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
						paint_note(
							&pr,
							r,
							t.color,
							vel(n),
							false,
							n.pitch,
							(v.row_height, flats),
						);
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
			paint_note(
				&pr,
				r,
				t.color,
				vel(n),
				selected,
				pitch,
				(v.row_height, flats),
			);
			if !n.bend.is_empty() {
				paint_bend(&pr, v, area, clip.start + start, length, r, &n.bend);
			}
		}
		match &self.gesture {
			// The note being placed.
			Some(Gesture::Draw { start, pitch, .. }) => {
				let r = note_rect(v, area, clip.start, *start, v.last_length, *pitch);
				paint_note(
					&pr,
					r,
					t.color,
					v.last_velocity,
					true,
					*pitch,
					(v.row_height, flats),
				);
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
					paint_note(
						&pr,
						r,
						t.color,
						v.last_velocity,
						true,
						*pitch,
						(v.row_height, flats),
					);
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
							RichText::new(tr("NOTE"))
								.font(theme::small())
								.color(theme::TEXT_FAINT),
						);
						ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
							close = ui
								.small_button("×")
								.on_hover_text(tr("Close (Esc)"))
								.clicked();
						});
					});
					let n = &mut props.note;
					let mut straighten = false;
					let mut done = |r: egui::Response| {
						commit |= (r.changed() && !r.dragged()) || r.drag_stopped();
					};
					egui::Grid::new("piano-note-props-grid")
						.num_columns(2)
						.spacing(vec2(8.0, 4.0))
						.show(ui, |ui| {
							ui.label(tr("Pitch"));
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
							ui.label(tr("Velocity"));
							let mut vel = (n.velocity * 100.0).round();
							let r = ui.add(
								egui::DragValue::new(&mut vel)
									.range(0.0..=100.0)
									.suffix(" %"),
							);
							n.velocity = vel / 100.0;
							done(r);
							ui.end_row();
							ui.label(tr("Start"));
							let mut start = n.start.beats();
							let r = ui.add(
								egui::DragValue::new(&mut start)
									.range(0.0..=100_000.0)
									.speed(0.0625)
									.max_decimals(3)
									.suffix(tr(" beats")),
							);
							n.start = Tick::from_beats(start);
							done(r);
							ui.end_row();
							ui.label(tr("Length"));
							let mut length = n.length.beats();
							let r = ui.add(
								egui::DragValue::new(&mut length)
									.range(MIN_NOTE_LENGTH.beats()..=10_000.0)
									.speed(0.0625)
									.max_decimals(3)
									.suffix(tr(" beats")),
							);
							n.length = Tick::from_beats(length);
							done(r);
							ui.end_row();
							if !n.bend.is_empty() {
								ui.label(tr("Bend"));
								if ui
									.small_button(tr("Straighten"))
									.on_hover_text(tr("Remove this note's own pitch curve"))
									.clicked()
								{
									n.bend.clear();
									straighten = true;
								}
								ui.end_row();
							}
						});
					commit |= straighten;
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
		// One row per key, aligned with the grid rows (design 04): white keys light,
		// black keys dark, C labelled; the hovered row (keys or grid) is tinted and held
		// or previewed keys light up.
		let top_pitch = v.y_to_pitch(0.0);
		let bottom_pitch = v.y_to_pitch(area.height());
		let hovered = response
			.hover_pos()
			.map(|pos| pitch_at(pos.y))
			.or(self.hover_pitch);
		let pressed = |pitch: u8| {
			self.key == Some(pitch) || state.previewing.iter().any(|(_, p)| *p == pitch)
		};
		let flats = piano::prefers_flats(v.scale);
		for pitch in bottom_pitch..=top_pitch {
			let y = area.top() + v.pitch_to_y(pitch);
			let row = Rect::from_min_size(pos2(area.left(), y), vec2(area.width(), v.row_height));
			let black = piano::is_black(pitch);
			let in_scale = v.scale.is_none_or(|s| s.contains(pitch));
			let base = match (black, in_scale) {
				(false, true) => theme::KEY_WHITE,
				(false, false) => theme::mix(theme::KEY_WHITE, theme::BG_DEEP, 0.08),
				(true, _) => theme::KEY_BLACK,
			};
			let fill = if pressed(pitch) {
				theme::ACCENT
			} else if hovered == Some(pitch) {
				theme::mix(base, theme::ACCENT, if black { 0.35 } else { 0.3 })
			} else {
				base
			};
			p.rect_filled(row, 0.0, fill);
			let line = if pitch % 12 == 0 {
				Color32::from_rgb(0x8a, 0x8e, 0x96)
			} else {
				theme::BORDER_SUBTLE
			};
			p.hline(row.x_range(), row.bottom() - 0.5, Stroke::new(1.0, line));
			if v.scale.is_some_and(|s| s.contains(pitch)) && black {
				// In-scale black keys get a small mark, so the scale reads on the keys.
				p.rect_filled(
					Rect::from_min_size(
						pos2(area.right() - 9.0, row.center().y - 1.0),
						vec2(4.0, 2.0),
					),
					0.0,
					theme::ACCENT,
				);
			}
			let label = pitch % 12 == 0 || (hovered == Some(pitch) && v.row_height >= 9.0);
			if label && v.row_height >= 7.0 {
				let on_dark = black || pressed(pitch);
				p.text(
					pos2(row.right() - 5.0, row.center().y),
					Align2::RIGHT_CENTER,
					cadenza_theory::format_pitch(pitch, flats),
					theme::mono((v.row_height * 0.72).clamp(8.0, 10.0)),
					if on_dark {
						theme::TEXT_DIM
					} else {
						Color32::from_rgb(0x3a, 0x3d, 0x44)
					},
				);
			}
		}
		p.vline(
			area.right() - 0.5,
			area.y_range(),
			Stroke::new(1.0, theme::BORDER),
		);
	}

	fn velocity_lane(&mut self, ui: &mut Ui, state: &mut AppState, lane: Rect, t: &Target) {
		let p = ui.painter_at(lane);
		p.rect_filled(lane, 0.0, theme::CANVAS);
		let inner = Rect::from_min_max(
			pos2(lane.left(), lane.top() + 10.0),
			pos2(lane.right(), lane.bottom() - 6.0),
		);
		p.hline(
			lane.x_range(),
			inner.center().y,
			Stroke::new(1.0, theme::BORDER_SUBTLE),
		);
		lane_bar_lines(&p, state, lane);
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
		let wheel = self.wheel_preview(clip);
		let preview = self
			.velocity
			.as_ref()
			.map(VelocityDrag::values)
			.or(wheel.as_ref());
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
			// Stems in the track colour; the selection's are white (design 04).
			let color = if selected { theme::TEXT } else { t.color };
			p.vline(x + 1.0, top..=inner.bottom(), Stroke::new(2.0, color));
			p.circle_filled(pos2(x + 1.0, top), 3.5, color);
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

	/// A controller lane (FL Studio event editor): the value as a step graph, pencil
	/// drawing per grid cell, Alt/right-drag ramps, right-click deletes an event.
	fn controller_lane(
		&mut self,
		ui: &mut Ui,
		state: &mut AppState,
		controller: Controller,
		lane: Rect,
		t: &Target,
	) {
		let p = ui.painter_at(lane);
		p.rect_filled(lane, 0.0, theme::CANVAS);
		lane_bar_lines(&p, state, lane);
		let inner = lane.shrink2(vec2(0.0, 8.0));
		let (lo, hi) = controller.range();
		let y_of = |v: f32| inner.bottom() - (v - lo) / (hi - lo) * inner.height();
		let value_at = |y: f32| {
			piano::controller_value(
				controller,
				lo + ((inner.bottom() - y) / inner.height()).clamp(0.0, 1.0) * (hi - lo),
			)
		};
		for frac in [0.25, 0.5, 0.75] {
			let center = controller == Controller::PitchBend && frac == 0.5;
			p.hline(
				lane.x_range(),
				inner.bottom() - inner.height() * frac,
				Stroke::new(
					1.0,
					if center {
						theme::BORDER
					} else {
						theme::BORDER_SUBTLE
					},
				),
			);
		}
		let v = state.view.piano.clone();
		let tl = v.timeline;
		let clip = &t.clip;
		let x_of = |tick: Tick| lane.left() + tl.tick_to_x(clip.start + tick);
		let tick_at = |x: f32| tl.x_to_tick(x - lane.left()) - clip.start;
		let step = key_step(state).max(Tick(PPQ / 64));
		let cell = |tick: Tick| Tick(tick.0.max(0).div_euclid(step.0) * step.0);
		let old: Vec<ControllerPoint> = clip
			.controller(controller)
			.map(|l| l.points.clone())
			.unwrap_or_default();

		let response = ui.interact(lane, Id::new("piano-controller"), Sense::click_and_drag());
		let (pressed, r_pressed, down, alt) = ui.input(|i| {
			(
				i.pointer.primary_pressed(),
				i.pointer.button_pressed(PointerButton::Secondary),
				i.pointer.primary_down() || i.pointer.secondary_down(),
				i.modifiers.alt,
			)
		});
		if (pressed || r_pressed)
			&& response.hovered()
			&& let Some(pos) = ui.ctx().pointer_latest_pos()
		{
			let at = (tick_at(pos.x), value_at(pos.y));
			self.control = Some(if r_pressed || alt {
				ControlDrag::Line {
					from: at,
					to: at,
					press: pos,
					moved: false,
				}
			} else {
				ControlDrag::Paint {
					cells: BTreeMap::new(),
					last: None,
				}
			});
		}
		if let (Some(drag), Some(pos)) = (self.control.as_mut(), ui.ctx().pointer_latest_pos()) {
			let (tick, value) = (tick_at(pos.x), value_at(pos.y));
			match drag {
				ControlDrag::Paint { cells, last } => {
					let c = cell(tick);
					// Fill the cells between the last pointer position and this one.
					if let Some((lt, lv)) = *last {
						let (a, b) = if lt <= c { (lt, c) } else { (c, lt) };
						let mut k = a;
						while k <= b {
							let f = if b == a {
								1.0
							} else {
								(k - lt).0 as f32 / (c - lt).0 as f32
							};
							let f = f.clamp(0.0, 1.0);
							cells.insert(
								k,
								piano::controller_value(controller, lv + (value - lv) * f),
							);
							k = k + step;
						}
					}
					cells.insert(c, value);
					*last = Some((c, value));
				}
				ControlDrag::Line {
					to, press, moved, ..
				} => {
					*to = (tick, value);
					*moved |= pos.distance(*press) > 4.0;
				}
			}
		}
		if !down && let Some(drag) = self.control.take() {
			let points = match drag {
				ControlDrag::Paint { cells, .. } => piano::controller_paint(&old, &cells),
				ControlDrag::Line {
					from, to, moved, ..
				} if moved => {
					let (a, b) = if from.0 <= to.0 {
						(from, to)
					} else {
						(to, from)
					};
					let ramp = cadenza_model::controller::ramp(
						controller,
						cell(a.0),
						a.1,
						cell(b.0),
						b.1,
						step,
						0.0,
					);
					Some(cadenza_model::controller::splice(
						&old,
						cell(a.0),
						cell(b.0),
						&ramp,
					))
				}
				ControlDrag::Line { press, .. } => {
					// A plain right-click: delete the event nearest the pointer.
					old.iter()
						.map(|p| (p.tick, (x_of(p.tick) - press.x).abs()))
						.filter(|(_, d)| *d <= 6.0)
						.min_by(|a, b| a.1.total_cmp(&b.1))
						.map(|(tick, _)| piano::controller_delete(&old, tick))
				}
			};
			if let Some(points) = points
				&& let Some(cmd) =
					piano::set_controller(state.project(), clip.id, controller, points)
			{
				state.apply(cmd);
			}
		}

		// The lane as it would be with the gesture applied.
		let shown: Vec<ControllerPoint> = match &self.control {
			Some(ControlDrag::Paint { cells, .. }) => {
				piano::controller_paint(&old, cells).unwrap_or_else(|| old.clone())
			}
			_ => old.clone(),
		};
		let base = y_of(controller.default_value().clamp(lo, hi));
		let clip_x0 = x_of(Tick::ZERO).max(lane.left());
		let clip_x1 = x_of(clip.length).min(lane.right());
		// Controllers are violet like automation (design 04): a step graph over a soft
		// fill, each event a ring.
		let color = theme::AUTOMATION;
		let fill = color.gamma_multiply(0.12);
		let mut rings = Vec::new();
		let mut value = controller.default_value();
		let mut x = x_of(Tick::ZERO);
		let mut line = Vec::new();
		for pt in shown.iter().filter(|p| p.tick < clip.length) {
			let nx = x_of(pt.tick);
			if nx > lane.left() && x < lane.right() {
				let (a, b) = (x.max(clip_x0), nx.min(clip_x1));
				if b > a {
					p.rect_filled(
						Rect::from_two_pos(pos2(a, y_of(value)), pos2(b, base)),
						0.0,
						fill,
					);
				}
			}
			line.push(pos2(nx, y_of(value)));
			line.push(pos2(nx, y_of(pt.value)));
			value = pt.value;
			x = nx;
			if (lane.left() - 4.0..=lane.right() + 4.0).contains(&nx) {
				rings.push(pos2(nx, y_of(pt.value)));
			}
		}
		if clip_x1 > x.max(clip_x0) {
			p.rect_filled(
				Rect::from_two_pos(pos2(x.max(clip_x0), y_of(value)), pos2(clip_x1, base)),
				0.0,
				fill,
			);
		}
		line.push(pos2(clip_x1, y_of(value)));
		if line.len() > 1 {
			p.add(egui::Shape::line(line, Stroke::new(1.5, color)));
		}
		for c in rings {
			p.circle_filled(c, 3.5, theme::CANVAS);
			p.circle_stroke(c, 3.0, Stroke::new(1.5, color));
		}
		if let Some(ControlDrag::Line {
			from, to, moved, ..
		}) = &self.control
			&& *moved
		{
			p.line_segment(
				[
					pos2(x_of(from.0), y_of(from.1)),
					pos2(x_of(to.0), y_of(to.1)),
				],
				Stroke::new(1.5, theme::NOTE_SELECTED),
			);
		}
		if let Some(pos) = response.hover_pos() {
			ui.ctx().set_cursor_icon(CursorIcon::Crosshair);
			let text = piano::controller_text(controller, value_at(pos.y));
			p.text(
				pos + vec2(10.0, -10.0),
				Align2::LEFT_BOTTOM,
				text,
				theme::small(),
				theme::TEXT,
			);
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
			state
				.notices
				.info(trf("Copied {n} notes", &[("n", &sel.len())]));
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
					.info(tr("Select notes to clone (Ctrl+A selects all)")),
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
				None => state.notices.info(tr(
					"Glue joins selected notes of the same pitch: select two or more",
				)),
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
		None => state.notices.info(tr("Already on the grid")),
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
	let inset = if v.row_height >= 8.0 { 1.0 } else { 0.5 };
	Rect::from_min_size(
		pos2(x0, y + inset),
		vec2(w, (v.row_height - 2.0 * inset).max(2.0)),
	)
}

/// A note (design 04): the track colour, more opaque the louder it is, its name when
/// there is room and a release grip on the right edge; selected notes turn pale
/// ([`theme::NOTE_SELECTED`]) with a light outline.
fn paint_note(
	p: &Painter,
	r: Rect,
	color: Color32,
	velocity: f32,
	selected: bool,
	pitch: u8,
	look: (f32, bool),
) {
	let (row_height, flats) = look;
	let fill = if selected {
		theme::NOTE_SELECTED
	} else {
		theme::mix(
			theme::ROW_WHITE,
			color,
			0.35 + 0.65 * velocity.clamp(0.0, 1.0),
		)
	};
	let radius = CornerRadius::same(2);
	p.rect_filled(r, radius, fill);
	if selected {
		p.rect_stroke(
			r,
			radius,
			Stroke::new(1.0, theme::TEXT),
			StrokeKind::Outside,
		);
	} else {
		p.rect_stroke(
			r,
			radius,
			Stroke::new(1.0, Color32::from_black_alpha(70)),
			StrokeKind::Inside,
		);
	}
	// Release grip: the edge that resizes.
	if r.width() >= 12.0 && r.height() >= 6.0 {
		p.rect_filled(
			Rect::from_min_max(
				pos2(r.right() - 4.0, r.top() + 2.0),
				pos2(r.right() - 1.0, r.bottom() - 2.0),
			),
			CornerRadius::same(1),
			Color32::from_rgba_premultiplied(8, 8, 10, 90),
		);
	}
	if row_height >= 11.0 && r.width() > 30.0 {
		p.with_clip_rect(r.shrink(1.0)).text(
			pos2(r.left() + 4.0, r.center().y),
			Align2::LEFT_CENTER,
			cadenza_theory::format_pitch(pitch, flats),
			theme::mono_medium((row_height * 0.66).clamp(8.0, 10.0)),
			theme::text_on(fill),
		);
	}
}

/// A note's own bend (NE-9h) drawn over it: the held values as steps from the note's
/// middle, a semitone per row, cut at the note's end.
fn paint_bend(
	p: &Painter,
	v: &PianoView,
	area: Rect,
	start: Tick,
	length: Tick,
	r: Rect,
	bend: &[cadenza_model::NoteBendPoint],
) {
	let x = |tick: Tick| area.left() + v.timeline.tick_to_x(start + tick);
	let y = |semitones: f32| r.center().y - semitones * v.row_height;
	let end = x(length);
	let mut points = vec![pos2(r.left(), y(0.0))];
	for b in bend.iter().take_while(|b| b.tick < length) {
		let at = x(b.tick).max(r.left());
		let last = points.last().map_or(y(0.0), |q| q.y);
		points.push(pos2(at, last));
		points.push(pos2(at, y(b.semitones)));
	}
	let last = points.last().map_or(y(0.0), |q| q.y);
	points.push(pos2(end, last));
	p.add(egui::Shape::line(
		points.clone(),
		Stroke::new(3.0, Color32::from_black_alpha(110)),
	));
	p.add(egui::Shape::line(points, Stroke::new(1.5, theme::TEXT)));
}

/// Row fill (design 04): with scale rows, in-scale rows one step lighter than the
/// rest and the root lighter still; without, black-key rows darker.
fn row_color(pitch: u8, scale: Option<Scale>) -> Color32 {
	match scale {
		Some(s) if !s.contains(pitch) => theme::ROW_OUT_OF_SCALE,
		Some(s) if (i32::from(pitch) - i32::from(s.root.0)).rem_euclid(12) == 0 => theme::ROW_ROOT,
		Some(_) => theme::ROW_WHITE,
		None if piano::is_black(pitch) => theme::ROW_BLACK,
		None => theme::ROW_WHITE,
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
	p.rect_filled(r, 0.0, theme::BG_PANEL);
	// The clip's span: a band in its track colour along the bottom edge.
	let x0 = r.left() + tl.tick_to_x(t.clip.start);
	let x1 = r.left() + tl.tick_to_x(t.clip.end());
	p.rect_filled(
		Rect::from_min_max(pos2(x0, r.bottom() - 3.0), pos2(x1, r.bottom())),
		0.0,
		t.color.gamma_multiply(0.85),
	);
	// Pattern mode: the span Play loops (the clip, at least a bar), purple along the top.
	let config = state.transport_config().0;
	if config.pattern == Some(t.clip.id)
		&& let Some((s, e)) = config.loop_range
	{
		let (a, b) = (r.left() + tl.tick_to_x(s), r.left() + tl.tick_to_x(e));
		p.rect_filled(
			Rect::from_min_max(pos2(a, r.top()), pos2(b, r.top() + 4.0)),
			0.0,
			theme::PATTERN.gamma_multiply(0.9),
		);
		for (x, dir) in [(a, 1.0), (b, -1.0)] {
			let tab = Rect::from_two_pos(pos2(x, r.top()), pos2(x + dir * 3.0, r.top() + 9.0));
			p.rect_filled(tab, CornerRadius::same(1), theme::PATTERN);
		}
	}
	// The same scale as the arrange ruler: tall numbered bar lines, beat and step ticks
	// by zoom, beat numbers when bars are wide.
	let (from, to) = (tl.x_to_tick(0.0), tl.x_to_tick(r.width()));
	let shortest = meter
		.view()
		.segments_between(Tick(from.0.max(0)), to)
		.map(|(_, ts)| ts.ticks_per_bar())
		.min()
		.unwrap_or(Tick(PPQ * 4));
	let bar_px = tl.ticks_to_px(shortest);
	let every = ((48.0 / bar_px).ceil().max(1.0) as u64).next_power_of_two() as i64;
	let beat_px = tl.ticks_to_px(Tick(PPQ));
	for line in tl.grid_lines(meter.view(), from, to, None) {
		let x = (r.left() + tl.tick_to_x(line.tick)).round() + 0.5;
		match line.kind {
			LineKind::Bar(index) => {
				let numbered = index % every == 0;
				let top = if numbered {
					r.top() + 5.0
				} else {
					r.bottom() - 9.0
				};
				p.vline(x, top..=r.bottom(), Stroke::new(1.0, theme::BORDER_STRONG));
				if numbered {
					p.text(
						pos2(x + 4.0, r.top() + 6.0),
						Align2::LEFT_TOP,
						format!("{}", index + 1),
						theme::mono_medium(11.0),
						theme::TEXT_DIM,
					);
				}
			}
			LineKind::Beat if beat_px >= 10.0 => {
				p.vline(
					x,
					r.bottom() - 7.0..=r.bottom(),
					Stroke::new(1.0, theme::BORDER_STRONG),
				);
				if bar_px > 220.0 {
					let (bar, beat_no, _) = timeline::bbt(line.tick, meter.view());
					p.text(
						pos2(x + 3.0, r.top() + 7.0),
						Align2::LEFT_TOP,
						format!("{bar}.{beat_no}"),
						theme::mono(9.5),
						theme::TEXT_FAINT,
					);
				}
			}
			LineKind::Step if beat_px >= 48.0 => {
				p.vline(
					x,
					r.bottom() - 3.0..=r.bottom(),
					Stroke::new(1.0, theme::BORDER),
				);
			}
			_ => {}
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

/// The piano roll toolbar's fold: the tool chips' [`arrange::ToolFold`], and whether
/// everything else (the F7 key, the clip selector, scale/ghost labels, the selected-note
/// count) also drops to its narrowest form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PianoFold {
	tools: arrange::ToolFold,
	compact: bool,
}

/// Picks the piano roll toolbar's fold from the width actually available, in this
/// language's own metrics, not a window-size tier: below 1320 px the toolbar's other labels
/// (the F7 key, the clip selector, scale/ghost labels, the selected-note count) drop to
/// their narrowest form; independently, the tool chips (Draw/Paint/Select/Slice) measure
/// their own "glyph name" width ([`arrange::chip_width`]) and fold their key, then their
/// name, when a translation needs more room than is left for them. Pure text measurement —
/// nothing here is drawn, so trying a fold never touches the accessibility tree.
fn piano_fold(ui: &Ui, width: f32) -> PianoFold {
	let compact = width < 1320.0;
	// Conservative estimate of everything else in the row (title, clip selector, snap/key
	// chips, ghosts/events, zoom, Quantize, note count) at its narrowest or widest form, so
	// in practice only the tools' own measured width decides whether they fold further.
	let reserve = if compact { 260.0 } else { 520.0 };
	let available = (width - reserve).max(0.0);
	let spacing = ui.spacing().item_spacing.x.max(2.0);
	let tools: [(&str, &str, &str); 4] = Tool::ALL.map(|t| (t.glyph(), t.name(), t.key()));
	for tools_fold in [arrange::ToolFold::Full, arrange::ToolFold::NoKey] {
		let w: f32 = tools
			.iter()
			.map(|&(g, n, k)| {
				let (label, key) = tools_fold.label_and_key(g, n, k);
				arrange::chip_width(ui, &label, key) + spacing
			})
			.sum();
		if w <= available {
			return PianoFold {
				tools: tools_fold,
				compact,
			};
		}
	}
	PianoFold {
		tools: arrange::ToolFold::Glyph,
		compact,
	}
}

/// A recessed segmented group (tools, snap): its chips sit on the deep background.
fn segment_group(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
	egui::Frame::new()
		.fill(theme::BG_DEEP)
		.corner_radius(CornerRadius::same(4))
		.inner_margin(egui::Margin::same(2))
		.show(ui, |ui| {
			ui.horizontal(|ui| {
				ui.spacing_mut().item_spacing.x = 2.0;
				add(ui);
			});
		});
}

/// A filled toolbar chip: optional faint `prefix`, the `value`, then a faint `suffix`
/// (a key or ▾). `swatch` paints a colour square first (the clip chip).
fn chip(
	ui: &mut Ui,
	swatch: Option<Color32>,
	prefix: Option<&str>,
	value: &str,
	suffix: Option<&str>,
) -> egui::Response {
	let font = theme::medium(12.0);
	let faint = theme::small();
	let width = |text: &str, font: &FontId| {
		ui.fonts_mut(|f| {
			f.layout_no_wrap(text.into(), font.clone(), Color32::WHITE)
				.size()
				.x
		})
	};
	let gap = 6.0;
	let mut w = 16.0 + width(value, &font);
	if swatch.is_some() {
		w += 9.0 + gap;
	}
	if let Some(p) = prefix {
		w += width(p, &faint) + gap;
	}
	if let Some(s) = suffix {
		w += width(s, &faint) + gap;
	}
	let (rect, resp) = ui.allocate_exact_size(vec2(w, 22.0), Sense::click());
	let p = ui.painter();
	p.rect_filled(
		rect,
		CornerRadius::same(4),
		if resp.hovered() {
			theme::BG_HOVER
		} else {
			theme::BG_WIDGET
		},
	);
	let mut x = rect.left() + 8.0;
	let y = rect.center().y;
	if let Some(c) = swatch {
		p.rect_filled(
			Rect::from_center_size(pos2(x + 4.5, y), vec2(9.0, 9.0)),
			CornerRadius::same(2),
			c,
		);
		x += 9.0 + gap;
	}
	if let Some(t) = prefix {
		x = p
			.text(
				pos2(x, y),
				Align2::LEFT_CENTER,
				t,
				faint.clone(),
				theme::TEXT_FAINT,
			)
			.right() + gap;
	}
	x =
		p.text(pos2(x, y), Align2::LEFT_CENTER, value, font, theme::TEXT)
			.right() + gap;
	if let Some(t) = suffix {
		p.text(pos2(x, y), Align2::LEFT_CENTER, t, faint, theme::TEXT_FAINT);
	}
	let label = match prefix {
		Some(p) => format!("{p} {value}"),
		None => value.to_string(),
	};
	resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &label));
	resp
}

/// A filled chip with a key hint (Quantize Q).
fn filled_chip(ui: &mut Ui, label: &str, key: Option<&str>) -> egui::Response {
	chip(ui, None, None, label, key)
}

/// A chip that opens a menu below it: "<prefix> <value> ▾".
fn chip_menu(
	ui: &mut Ui,
	id: &str,
	prefix: Option<&str>,
	value: &str,
	tip: &str,
	add: impl FnOnce(&mut Ui),
) {
	let resp = chip(ui, None, prefix, value, Some("▾")).on_hover_text(tip);
	egui::Popup::menu(&resp).id(Id::new(id)).show(|ui| {
		ui.set_min_width(150.0);
		add(ui);
	});
}

/// The piano roll's magnet: a Snap toggle (off <-> the last snap) and the step menu.
fn snap_chip(ui: &mut Ui, state: &mut AppState) {
	let ts = signature(state);
	let main_grid = state.view.piano.timeline.grid(ts);
	let main = state.view.snap.then_some(main_grid.step);
	let snap = state.view.piano.snap;
	let step = snap.step(ts, main);
	let short = |s: PianoSnap| -> String {
		match (s, s.step(ts, main)) {
			(PianoSnap::Main, Some(step)) => {
				trf("Main {grid}", &[("grid", &grid(step, main_grid.bar))])
			}
			(PianoSnap::Main, None) => tr("Main (off)").into(),
			(PianoSnap::Off, _) => tr("Off").into(),
			(_, Some(step)) => grid(step, ts.ticks_per_bar()),
			(s, None) => s.name().into(),
		}
	};
	fn grid(step: Tick, bar: Tick) -> String {
		let name = arrange::grid_name(step, bar);
		name.trim_start_matches("Grid ").to_string()
	}
	let memory = Id::new("piano-snap-last");
	segment_group(ui, |ui| {
		let on = step.is_some();
		if arrange::toolbar_chip(
			ui,
			on,
			tr("Snap"),
			None,
			theme::ACCENT_DIM,
			theme::ACCENT_TEXT,
		)
		.on_hover_text(tr(
			"Piano roll snap (FL's magnet): click turns it off and back on; Alt while \
			 dragging: free",
		))
		.clicked()
		{
			state.view.piano.snap = if snap == PianoSnap::Off {
				ui.data(|d| d.get_temp(memory)).unwrap_or(PianoSnap::Step)
			} else {
				ui.data_mut(|d| d.insert_temp(memory, snap));
				PianoSnap::Off
			};
		}
		let resp = chip_value(ui, &format!("{} ▾", short(snap)))
			.on_hover_text(tr("Snap step. Main follows the arrange snap"));
		egui::Popup::menu(&resp)
			.id(Id::new("piano-snap"))
			.show(|ui| {
				ui.set_min_width(150.0);
				for s in PianoSnap::ALL {
					let name = if s == PianoSnap::Main {
						trf(
							"Main ({short})",
							&[("short", &short(s).trim_start_matches("Main "))],
						)
					} else {
						s.name().to_string()
					};
					ui.selectable_value(&mut state.view.piano.snap, s, name);
				}
			});
	});
}

/// The value half of a segmented chip (mono, transparent until hovered).
fn chip_value(ui: &mut Ui, text: &str) -> egui::Response {
	let font = theme::mono(11.0);
	let w = ui.fonts_mut(|f| {
		f.layout_no_wrap(text.into(), font.clone(), Color32::WHITE)
			.size()
			.x
	});
	let (rect, resp) = ui.allocate_exact_size(vec2(w + 14.0, 22.0), Sense::click());
	if resp.hovered() {
		ui.painter()
			.rect_filled(rect, CornerRadius::same(3), theme::BG_HOVER);
	}
	ui.painter().text(
		pos2(rect.left() + 7.0, rect.center().y),
		Align2::LEFT_CENTER,
		text,
		font,
		theme::TEXT_DIM,
	);
	resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, text));
	resp
}

/// "Key C minor ▾": root and scale kind ("No scale" turns highlighting off).
fn key_chip(ui: &mut Ui, v: &mut PianoView, compact: bool) {
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
	let flats = piano::prefers_flats(v.scale);
	let value = v.scale.map_or(tr("None").to_string(), |s| {
		format!(
			"{} {}",
			PitchClass(s.root.0).name(flats),
			crate::state::scale_kind_name(s.kind)
		)
	});
	let root = v.scale.map_or(0, |s| s.root.0);
	let kind = v.scale.map_or(ScaleKind::Major, |s| s.kind);
	chip_menu(
		ui,
		"piano-key",
		(!compact || v.scale.is_none()).then_some(tr("Key")),
		if compact && v.scale.is_none() {
			"—"
		} else {
			&value
		},
		tr("Key of the scale highlighting (scale rows, keyboard marks, note names)"),
		|ui| {
			ui.label(
				RichText::new(tr("Root"))
					.font(theme::small())
					.color(theme::TEXT_FAINT),
			);
			egui::Grid::new("piano-key-roots")
				.spacing(vec2(2.0, 2.0))
				.show(ui, |ui| {
					for pc in 0..12u8 {
						if ui
							.selectable_label(
								v.scale.is_some() && root == pc,
								PitchClass(pc).name(flats),
							)
							.clicked()
						{
							v.scale = Some(Scale::new(PitchClass(pc), kind));
						}
						if pc % 6 == 5 {
							ui.end_row();
						}
					}
				});
			ui.separator();
			if ui
				.selectable_label(v.scale.is_none(), tr("No scale"))
				.clicked()
			{
				v.scale = None;
			}
			for k in KINDS {
				if ui
					.selectable_label(
						v.scale.is_some_and(|s| s.kind == k),
						crate::state::scale_kind_name(k),
					)
					.clicked()
				{
					v.scale = Some(Scale::new(PitchClass(root), k));
				}
			}
		},
	);
}

/// "■ Chords on Keys ▾": the track's MIDI clips (in time order, with their bar);
/// picking one edits it here.
fn clip_selector(ui: &mut Ui, state: &mut AppState, t: &Target, compact: bool) {
	let meter = cadenza_core::MeterMap::new(state.project().meter());
	let (track_name, clips): (String, Vec<(cadenza_core::ClipId, String)>) = state
		.project()
		.track(t.track)
		.map(|track| {
			let mut c: Vec<&Clip> = track.clips.iter().filter(|c| c.is_midi()).collect();
			c.sort_by_key(|c| c.start);
			let clips = c
				.into_iter()
				.map(|c| {
					let bar = meter.view().bar_index(c.start) + 1;
					(c.id, format!("{} · bar {bar}", c.name))
				})
				.collect();
			(track.name.clone(), clips)
		})
		.unwrap_or_default();
	let on = format!("on {track_name} ▾");
	let copies = cadenza_model::links::link_count(state.project(), t.clip.id);
	let name = if copies > 1 {
		trf(
			"{name} · linked ×{copies}",
			&[("name", &t.clip.name), ("copies", &copies)],
		)
	} else {
		t.clip.name.clone()
	};
	let resp = chip(
		ui,
		Some(t.color),
		None,
		&name,
		Some(if compact { "▾" } else { &on }),
	)
	.on_hover_text(tr("Clips of this track: pick one to edit it here"));
	let mut pick = None;
	egui::Popup::menu(&resp)
		.id(Id::new("piano-clip"))
		.show(|ui| {
			ui.set_min_width(170.0);
			for (id, label) in &clips {
				if ui.selectable_label(*id == t.clip.id, label).clicked() {
					pick = Some(*id);
				}
			}
		});
	if let Some(id) = pick.filter(|id| *id != t.clip.id) {
		state.open_piano_roll(id);
	}
}

fn empty(ui: &mut Ui, state: &mut AppState, rect: Rect) {
	let p = ui.painter_at(rect);
	p.text(
		rect.center() - vec2(0.0, 22.0),
		Align2::CENTER_CENTER,
		tr("No clip open"),
		FontId::proportional(14.0),
		theme::TEXT_DIM,
	);
	p.text(
		rect.center(),
		Align2::CENTER_CENTER,
		tr(
			"Press F7 or double-click an instrument track to open the piano roll \
		 (double-click a MIDI clip to edit that one)",
		),
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
				egui::Button::new(trf("Open a clip of '{name}'", &[("name", &name)])),
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

/// Bar lines through an event lane, so its events line up with the notes above.
fn lane_bar_lines(p: &Painter, state: &AppState, lane: Rect) {
	let tl = state.view.piano.timeline;
	let meter = cadenza_core::MeterMap::new(state.project().meter());
	let (from, to) = (tl.x_to_tick(0.0), tl.x_to_tick(lane.width()));
	for line in tl.grid_lines(meter.view(), from, to, None) {
		if let LineKind::Bar(_) = line.kind {
			let x = (lane.left() + tl.tick_to_x(line.tick)).round() + 0.5;
			p.vline(x, lane.y_range(), Stroke::new(1.0, theme::BORDER_SUBTLE));
		}
	}
}

/// The lane tabs row above the event lane (design 04): velocity and the common
/// controllers as tabs (a dot marks lanes with data), "+ CC" for any other controller,
/// and the gesture hint on the right.
fn lane_tabs(ui: &mut Ui, state: &mut AppState, rect: Rect, t: &Target) {
	let p = ui.painter_at(rect);
	p.rect_filled(rect, 0.0, theme::BG_PANEL);
	p.hline(
		rect.x_range(),
		rect.top() + 0.5,
		Stroke::new(1.0, theme::BORDER),
	);
	p.hline(
		rect.x_range(),
		rect.bottom() - 0.5,
		Stroke::new(1.0, theme::BORDER_SUBTLE),
	);
	p.text(
		pos2(rect.left() + 8.0, rect.center().y),
		Align2::LEFT_CENTER,
		tr("Lanes"),
		theme::small(),
		theme::TEXT_FAINT,
	);
	let current = state.view.piano.event_lane;
	let mut bar = ui.new_child(
		egui::UiBuilder::new()
			.max_rect(Rect::from_min_max(
				pos2(rect.left() + theme::KEYBOARD_WIDTH, rect.top()),
				pos2(rect.right() - 8.0, rect.bottom()),
			))
			.layout(egui::Layout::left_to_right(egui::Align::Center)),
	);
	bar.set_clip_rect(rect);
	let ui = &mut bar;
	ui.spacing_mut().item_spacing.x = 2.0;
	let mut pick = None;
	for lane in EventLane::tabs(&t.clip, current) {
		let data = match lane {
			EventLane::Velocity => false,
			EventLane::Controller(c) => t.clip.controller(c).is_some(),
		};
		if lane_tab(ui, &lane.label(), lane == current, data)
			.on_hover_text(match lane {
				EventLane::Velocity => tr("Note velocities").to_string(),
				EventLane::Controller(c) => {
					trf("{label} events of this clip", &[("label", &c.label())])
				}
			})
			.clicked()
		{
			pick = Some(lane);
		}
	}
	ui.add_space(4.0);
	let resp = lane_tab(ui, "+ CC", false, false).on_hover_text(tr(
		"Edit another controller: any CC number, pressure, volume…",
	));
	egui::Popup::menu(&resp)
		.id(Id::new("piano-event-lane"))
		.show(|ui| {
			ui.set_min_width(170.0);
			for l in EventLane::COMMON {
				let has = matches!(l, EventLane::Controller(c) if t.clip.controller(c).is_some());
				let text = if has {
					format!("● {}", l.label())
				} else {
					format!("   {}", l.label())
				};
				if ui.selectable_label(l == current, text).clicked() {
					pick = Some(l);
				}
			}
			ui.separator();
			ui.horizontal(|ui| {
				ui.label("CC #");
				let mut n = match current {
					EventLane::Controller(Controller::Cc(n)) => n,
					_ => 1,
				};
				if ui
					.add(egui::DragValue::new(&mut n).range(0..=119))
					.changed()
				{
					pick = Some(EventLane::Controller(Controller::Cc(n)));
				}
			});
		});
	let hint = match current {
		EventLane::Velocity => tr("Drag = set · Alt or right drag = line · Alt+wheel on notes"),
		EventLane::Controller(_) => {
			tr("Drag = draw · Alt or right drag = ramp · right-click = delete")
		}
	};
	let font = theme::small();
	let hint_w = ui.fonts_mut(|f| {
		f.layout_no_wrap(hint.into(), font.clone(), Color32::WHITE)
			.size()
			.x
	});
	if ui.available_width() > hint_w + 24.0 {
		p.text(
			pos2(rect.right() - 10.0, rect.center().y),
			Align2::RIGHT_CENTER,
			hint,
			font,
			theme::TEXT_FAINT,
		);
	}
	if let Some(l) = pick {
		state.view.piano.event_lane = l;
	}
}

/// One lane tab: filled when current, a small dot when the lane has data.
fn lane_tab(ui: &mut Ui, label: &str, on: bool, data: bool) -> egui::Response {
	let font = theme::small();
	let w = ui.fonts_mut(|f| {
		f.layout_no_wrap(label.into(), font.clone(), Color32::WHITE)
			.size()
			.x
	});
	let dot = if data { 9.0 } else { 0.0 };
	let (rect, resp) = ui.allocate_exact_size(vec2(w + 18.0 + dot, 20.0), Sense::click());
	let p = ui.painter();
	let fill = if on {
		theme::BG_HOVER
	} else if resp.hovered() {
		theme::BG_WIDGET
	} else {
		Color32::TRANSPARENT
	};
	p.rect_filled(rect, CornerRadius::same(3), fill);
	let color = if on || resp.hovered() {
		theme::TEXT
	} else {
		theme::TEXT_DIM
	};
	p.text(
		pos2(rect.left() + 9.0, rect.center().y),
		Align2::LEFT_CENTER,
		label,
		font,
		color,
	);
	if data {
		p.circle_filled(
			pos2(rect.right() - 9.0, rect.center().y),
			2.5,
			theme::AUTOMATION,
		);
	}
	resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, true, on, label));
	resp
}

/// The event lane's left column (under the keyboard): its name over its value scale,
/// or a controller's value at the playhead.
fn lane_label(ui: &mut Ui, state: &AppState, lane: Rect, g: &Geometry, t: &Target) {
	let label = Rect::from_min_max(
		pos2(g.keys.left(), lane.top()),
		pos2(lane.left(), lane.bottom()),
	);
	let p = ui.painter_at(label);
	p.rect_filled(label, 0.0, theme::BG_PANEL);
	p.vline(
		label.right() - 0.5,
		label.y_range(),
		Stroke::new(1.0, theme::BORDER),
	);
	let current = state.view.piano.event_lane;
	p.text(
		label.left_top() + vec2(7.0, 6.0),
		Align2::LEFT_TOP,
		current.short(),
		theme::small(),
		theme::TEXT_DIM,
	);
	let bottom = label.left_bottom() + vec2(7.0, -6.0);
	match current {
		EventLane::Velocity => {
			p.text(
				bottom - vec2(0.0, 12.0),
				Align2::LEFT_BOTTOM,
				"127",
				theme::mono(10.0),
				theme::TEXT_FAINT,
			);
			p.text(
				bottom,
				Align2::LEFT_BOTTOM,
				"0",
				theme::mono(10.0),
				theme::TEXT_FAINT,
			);
		}
		EventLane::Controller(c) => {
			let at = state.position() - t.clip.start;
			let value = t
				.clip
				.controller(c)
				.and_then(|l| l.value_at(at))
				.unwrap_or_else(|| c.default_value());
			p.text(
				bottom,
				Align2::LEFT_BOTTOM,
				piano::controller_text(c, value),
				theme::mono(10.5),
				theme::AUTOMATION,
			);
		}
	}
}
