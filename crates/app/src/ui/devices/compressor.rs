//! The Compressor editor body (design `15 EQ and Compressor`, 15c dock / 15d compact):
//! the level history (input, output, gain reduction, key, draggable threshold), the
//! transfer curve (threshold dot, ratio above the knee, knee on the wheel, live level
//! dot), the gain reduction meter, the knobs, the auto makeup suggestion and the
//! sidechain key picker. The gesture -> parameter mapping lives in
//! [`crate::state::compressor_editor`].

use std::sync::{Arc, Mutex};

use cadenza_core::{ParamAddress, TrackId};
use cadenza_dsp::readings;
use egui::{
	Align, Align2, Color32, CornerRadius, CursorIcon, Id, Layout, Mesh, Pos2, Rect, Sense, Shape,
	Stroke, StrokeKind, Ui, UiBuilder, pos2, vec2,
};

use crate::state::AppState;
use crate::state::compressor_editor::{
	self as comp, ATTACK, CompValues, Hit, KNEE, MAKEUP, MIX, RATIO, RELEASE, THRESHOLD,
};
use crate::state::device_display::{HistoryColumn, LevelHistory};
use crate::state::meters::FLOOR_DB;
use crate::state::selection::DeviceRef;
use crate::theme;
use crate::ui::device_editor::BodyCx;
use crate::ui::device_kit::{self, HistoryKey, HistoryLine, KnobSize, NOTCH, ParamKnob};
use crate::ui::widgets::Gesture;

/// Wheel notches over the knee are committed as one step once the wheel rests this long.
const WHEEL_REST: f64 = 0.5;
const HISTORY_COLUMNS: usize = 300;
/// History lengths of the footer (seconds).
const HISTORY_SECONDS: [f32; 3] = [2.0, 4.0, 8.0];
const GR_RANGE_DB: f32 = 24.0;
/// The history's gain reduction scale: 24 dB reach 42 % of the height (design 15c).
const HISTORY_GR_DB: f32 = GR_RANGE_DB / 0.42;
/// Level meters fall this fast once the probe goes quiet.
const FALL_DB_PER_S: f32 = 24.0;
const FOOTER_H: f32 = 33.0;
const SETTINGS_H: f32 = 32.0;
const GR_W: f32 = 72.0;
const KNOBS_W: f32 = 340.0;
/// Below this body width the dock shows one display at a time (Transfer · History tabs).
const TABS_BELOW: f32 = 1100.0;
const CHIP_H: f32 = 22.0;
const COMPACT_HISTORY_H: f32 = 64.0;
const COMPACT_KNOBS_H: f32 = 86.0;
const COMPACT_ROW_H: f32 = 30.0;

/// Knee wheel notches not committed yet.
#[derive(Debug, Clone, Copy)]
struct WheelEdit {
	start: f32,
	notches: f32,
	value: f32,
	last: f64,
}

/// A transfer display gesture in progress.
#[derive(Debug, Clone, Copy)]
struct Drag {
	hit: Hit,
	start: CompValues,
	/// Input level under the press (the ratio drag keeps it).
	grab_in: f32,
	press_x: f32,
}

/// View state of one compressor editor (dock or inspector), kept in egui memory.
struct CompView {
	history: LevelHistory,
	seconds: f32,
	rate: u32,
	range60: bool,
	key_trace: bool,
	footer: bool,
	/// Narrow dock: the history (true) or the transfer curve is shown.
	history_tab: bool,
	/// Latest gain reduction (positive dB) and input peak (dBFS).
	gr_db: f32,
	input_db: f32,
	/// Fed by [`set_display`] (screenshots): live, never decays.
	synthetic: bool,
	wheel: Option<WheelEdit>,
	drag: Option<Drag>,
}

impl Default for CompView {
	fn default() -> Self {
		Self {
			history: LevelHistory::new(HISTORY_COLUMNS, 4.0, 48_000),
			seconds: 4.0,
			rate: 48_000,
			range60: true,
			key_trace: true,
			footer: false,
			history_tab: true,
			gr_db: 0.0,
			input_db: FLOOR_DB,
			synthetic: false,
			wheel: None,
			drag: None,
		}
	}
}

impl CompView {
	fn resize_history(&mut self, seconds: f32, rate: u32) {
		if seconds != self.seconds || rate != self.rate {
			self.seconds = seconds;
			self.rate = rate;
			self.history = LevelHistory::new(HISTORY_COLUMNS, seconds, rate);
		}
	}

	fn range_db(&self) -> f32 {
		if self.range60 { -60.0 } else { -30.0 }
	}
}

type Shared = Arc<Mutex<CompView>>;

fn view_id(salt: &str, r: DeviceRef) -> Id {
	Id::new(("compressor-editor", salt, r.device))
}

fn view(ctx: &egui::Context, id: Id) -> Shared {
	ctx.data_mut(|d| {
		d.get_temp_mut_or_insert_with(id, || Arc::new(Mutex::new(CompView::default())))
			.clone()
	})
}

fn lock(view: &Shared) -> std::sync::MutexGuard<'_, CompView> {
	view.lock().unwrap_or_else(|e| e.into_inner())
}

/// Shows `history` and the levels in an editor as if the probe had measured them, live
/// (screenshots).
pub fn set_display(
	ctx: &egui::Context,
	salt: &str,
	r: DeviceRef,
	history: LevelHistory,
	input_db: f32,
	gr_db: f32,
) {
	let v = view(ctx, view_id(salt, r));
	let mut v = lock(&v);
	v.history = history;
	v.input_db = input_db;
	v.gr_db = gr_db;
	v.synthetic = true;
}

/// Whether the compact editor of `r` in `salt` shows its extra section.
pub fn footer_open(ctx: &egui::Context, salt: &str, r: DeviceRef) -> bool {
	lock(&view(ctx, view_id(salt, r))).footer
}

fn shown_values(state: &AppState, r: DeviceRef) -> CompValues {
	CompValues::from_fn(|p| {
		state
			.param_value(r.track, r.device, p.id)
			.unwrap_or(p.default)
	})
}

fn model_value(state: &AppState, r: DeviceRef, id: &str) -> f32 {
	state
		.project()
		.device(r.track, r.device)
		.and_then(|d| d.params.get(id).copied())
		.or_else(|| comp::info().param(id).map(|p| p.default))
		.unwrap_or(0.0)
}

fn differs(a: f32, b: f32) -> bool {
	(a - b).abs() > 1e-4 * a.abs().max(1.0)
}

/// The changes that differ from the document, committed as one undo step.
fn commit(state: &mut AppState, r: DeviceRef, changes: &[comp::Change], label: &str) {
	let real: Vec<(&str, f32)> = changes
		.iter()
		.filter(|(id, v)| differs(model_value(state, r, id), *v))
		.map(|&(id, v)| (id, v))
		.collect();
	for (id, _) in changes {
		if !real.iter().any(|(r, _)| r == id) {
			state.drop_param_preview(r.device, id);
		}
	}
	if !real.is_empty() {
		state.commit_params(r.track, r.device, &real, label);
	}
}

/// The key's track, its colour and whether it taps before the fader.
fn key_of(state: &AppState, r: DeviceRef) -> Option<(TrackId, String, Color32, bool)> {
	let source = state.device(r)?.sidechain?;
	let t = state.project().track(source.track)?;
	Some((
		t.id,
		t.name.clone(),
		theme::track_color(t.color.as_deref()),
		source.pre_fader,
	))
}

pub fn compressor_body(ui: &mut Ui, state: &mut AppState, cx: &BodyCx) {
	let shared = view(ui.ctx(), view_id(cx.salt, cx.r));
	let mut v = lock(&shared);
	update_levels(ui, state, cx, &mut v);
	finish_wheel(ui, state, cx.r, &mut v);
	if cx.compact {
		compact(ui, state, cx, &mut v);
	} else {
		dock(ui, state, cx, &mut v);
	}
}

/// Feeds the history from this frame's probe frames, the GR from the device's reading
/// and the key from its track's meter; levels fall once the probe goes quiet.
fn update_levels(ui: &Ui, state: &AppState, cx: &BodyCx, v: &mut CompView) {
	if v.synthetic {
		return;
	}
	let dt = ui.input(|i| i.stable_dt).min(0.25);
	let ours = state.probe_target() == Some(cx.r.device);
	let probe = state.probe().filter(|_| ours);
	if let Some(p) = probe {
		v.resize_history(v.seconds, p.sample_rate());
	}
	let gr = probe
		.and_then(|p| p.reading(readings::GAIN_REDUCTION_DB))
		.unwrap_or(0.0)
		.abs();
	let fall = FALL_DB_PER_S * dt;
	if ours && !cx.frames.is_empty() {
		let key = key_of(state, cx.r)
			.filter(|_| v.key_trace)
			.map(|(t, _, _, pre)| {
				let meter = state.meters.get(Some(t));
				let volume = state
					.project()
					.track(t)
					.map_or(0.0, |t| t.channel.volume_db);
				comp::key_level(
					meter.peak_db[0].max(meter.peak_db[1]),
					volume,
					pre,
					FLOOR_DB,
				)
			});
		v.history.set_key(key);
		v.history.push_frames(cx.frames, gr);
		let peak = cx
			.frames
			.iter()
			.flat_map(|f| f.input)
			.fold(0.0f32, |m, s| m.max(s.abs()));
		let db = HistoryColumn::peak_db((0.0, peak));
		v.input_db = db.max(v.input_db - fall).max(FLOOR_DB);
		v.gr_db = gr;
	} else {
		v.input_db = (v.input_db - fall).max(FLOOR_DB);
		v.gr_db = if probe.is_some() {
			gr.min((v.gr_db - fall).max(0.0)).max(0.0)
		} else {
			(v.gr_db - fall).max(0.0)
		};
	}
	if v.gr_db > 0.0 || v.input_db > FLOOR_DB {
		ui.ctx().request_repaint();
	}
}

fn stopped(state: &AppState, cx: &BodyCx, v: &CompView) -> bool {
	!v.synthetic && !state.is_playing() && cx.frames.is_empty()
}

/// Commits knee wheel notches once the wheel rests.
fn finish_wheel(ui: &Ui, state: &mut AppState, r: DeviceRef, v: &mut CompView) {
	let Some(w) = v.wheel else {
		return;
	};
	let now = ui.input(|i| i.time);
	let idle = now - w.last;
	if idle >= WHEEL_REST {
		v.wheel = None;
		commit(state, r, &[(KNEE, w.value)], "Compressor knee");
	} else {
		ui.ctx()
			.request_repaint_after(std::time::Duration::from_secs_f64(WHEEL_REST - idle));
	}
}

fn wheel_knee(ui: &Ui, state: &mut AppState, r: DeviceRef, v: &mut CompView, notches: f32) {
	let now = ui.input(|i| i.time);
	let fine = ui.input(|i| i.modifiers.shift);
	let start = state
		.param_value(r.track, r.device, KNEE)
		.unwrap_or_else(|| model_value(state, r, KNEE));
	let w = v.wheel.get_or_insert(WheelEdit {
		start,
		notches: 0.0,
		value: start,
		last: now,
	});
	w.notches += if fine { notches * 0.1 } else { notches };
	w.last = now;
	w.value = comp::wheel_knee(w.start, w.notches, false);
	state.preview_param(r.device, KNEE, w.value);
	ui.ctx()
		.request_repaint_after(std::time::Duration::from_secs_f64(WHEEL_REST));
}

// --- Level history ----------------------------------------------------------------------

fn history(ui: &mut Ui, state: &mut AppState, cx: &BodyCx, v: &CompView, rect: Rect, big: bool) {
	let r = cx.r;
	let threshold = shown_values(state, r).threshold;
	let key = key_of(state, r);
	let time = format!("{:.0} s · now →", v.seconds);
	let mut child = ui.new_child(UiBuilder::new().max_rect(rect));
	let view = device_kit::LevelHistoryView::new(
		Id::new(("comp-history", cx.salt, r.device)),
		rect.size(),
	)
	.range(v.range_db(), 0.0)
	.gr_range(HISTORY_GR_DB)
	.key(
		key.as_ref()
			.filter(|_| v.key_trace && big)
			.map(|(_, name, color, _)| HistoryKey {
				color: *color,
				name,
			}),
	)
	.line(Some(HistoryLine {
		db: threshold,
		default: comp::RESET_THRESHOLD,
		label: "Threshold",
		color: theme::TEXT,
	}))
	.legend(big, big)
	.time_label(big.then_some(time.as_str()))
	.stopped(stopped(state, cx, v));
	let view = if big { view } else { view.bare() };
	let (gesture, _) = view.show(&mut child, &v.history);
	match gesture {
		Gesture::Preview(db) => state.preview_param(r.device, THRESHOLD, round1(db)),
		Gesture::Commit(db) => commit(state, r, &[(THRESHOLD, round1(db))], "Compressor threshold"),
		Gesture::Idle => {}
	}
}

fn round1(v: f32) -> f32 {
	(v * 10.0).round() / 10.0
}

// --- Transfer curve ---------------------------------------------------------------------

/// The transfer display's mapping in `rect` (input on x, output on y, -60..0 dB).
#[derive(Clone, Copy)]
struct Axes {
	rect: Rect,
}

impl Axes {
	fn x(&self, db: f32) -> f32 {
		self.rect.left() + comp::db_to_t(db) * self.rect.width()
	}

	fn y(&self, db: f32) -> f32 {
		self.rect.bottom() - comp::db_to_t(db) * self.rect.height()
	}

	fn pos(&self, in_db: f32, out_db: f32) -> Pos2 {
		pos2(self.x(in_db), self.y(out_db))
	}

	fn in_db(&self, x: f32) -> f32 {
		comp::t_to_db((x - self.rect.left()) / self.rect.width().max(1.0))
	}

	fn out_db(&self, y: f32) -> f32 {
		comp::t_to_db((self.rect.bottom() - y) / self.rect.height().max(1.0))
	}

	/// dB per point on the x axis.
	fn db_per_px(&self) -> f32 {
		-comp::FLOOR_DB / self.rect.width().max(1.0)
	}
}

fn transfer(ui: &mut Ui, state: &mut AppState, cx: &BodyCx, v: &mut CompView, rect: Rect) {
	let r = cx.r;
	let big = rect.width() >= 240.0;
	let axes = Axes { rect };
	let id = Id::new(("comp-transfer", cx.salt, r.device));
	let resp = ui.interact(rect, id, Sense::click_and_drag());
	resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Other, true, "Transfer curve"));
	let values = shown_values(state, r);
	let dot_tol = 11.0 * axes.db_per_px();
	let hover = resp
		.hover_pos()
		.map(|p| comp::hit(&values, axes.in_db(p.x), axes.out_db(p.y), dot_tol));

	// Gestures: drag the dot (threshold, ⇔) or above the knee (ratio, ⇕); one step each.
	let shift = ui.input(|i| i.modifiers.shift);
	// The press position: egui reports the drag only after the pointer has travelled.
	if resp.drag_started()
		&& let Some(p) = ui
			.input(|i| i.pointer.press_origin())
			.or(resp.interact_pointer_pos())
	{
		let hit = comp::hit(&values, axes.in_db(p.x), axes.out_db(p.y), dot_tol);
		v.drag = matches!(hit, Hit::Threshold | Hit::Ratio).then_some(Drag {
			hit,
			start: values,
			grab_in: axes.in_db(p.x),
			press_x: p.x,
		});
	}
	let mut change: Option<comp::Change> = None;
	if let (Some(d), Some(p)) = (v.drag, resp.interact_pointer_pos())
		&& (resp.dragged() || resp.drag_stopped())
	{
		change = Some(match d.hit {
			Hit::Threshold => (
				THRESHOLD,
				comp::drag_threshold(
					d.start.threshold,
					(p.x - d.press_x) * axes.db_per_px(),
					shift,
				),
			),
			_ => {
				let target = comp::ratio_at(&d.start, d.grab_in, axes.out_db(p.y));
				let ratio = if shift {
					d.start.ratio + (target - d.start.ratio) * 0.1
				} else {
					target
				};
				(RATIO, ratio)
			}
		});
	}
	if resp.drag_stopped() {
		if let (Some(d), Some(c)) = (v.drag.take(), change) {
			let label = match d.hit {
				Hit::Threshold => "Compressor threshold",
				_ => "Compressor ratio",
			};
			commit(state, r, &[c], label);
		}
	} else if let Some((id, value)) = change {
		state.preview_param(r.device, id, value);
	}
	if resp.double_clicked() {
		v.drag = None;
		commit(state, r, &comp::reset(), "Reset threshold and ratio");
	}
	let scroll = ui.input(|i| i.smooth_scroll_delta.x + i.smooth_scroll_delta.y);
	if matches!(hover, Some(Hit::Knee | Hit::Threshold)) && scroll != 0.0 && !resp.dragged() {
		ui.input_mut(|i| i.smooth_scroll_delta = egui::Vec2::ZERO);
		wheel_knee(ui, state, r, v, scroll / NOTCH);
	}
	let cursor = match v.drag.map(|d| d.hit).or(hover) {
		Some(Hit::Threshold) => Some(CursorIcon::ResizeHorizontal),
		Some(Hit::Ratio) => Some(CursorIcon::ResizeVertical),
		_ => None,
	};
	let resp = match cursor {
		Some(c) => resp.on_hover_cursor(c),
		None => resp,
	};
	resp.on_hover_text_at_pointer(
		"Drag the dot for the threshold (⇔), above the knee for the ratio (⇕) · wheel over \
		 the knee: knee width · Shift fine · double-click: −24 dB, 4:1",
	);

	let values = shown_values(state, r);
	paint_transfer(ui, axes, &values, v.input_db, big);
}

fn paint_transfer(ui: &Ui, axes: Axes, v: &CompValues, input_db: f32, big: bool) {
	let rect = axes.rect;
	let base = ui.painter().with_clip_rect(rect);
	device_kit::paint_display(&base, rect);
	let p = base.with_clip_rect(rect.shrink(1.0));
	for db in [-48.0, -36.0, -24.0, -12.0] {
		p.vline(axes.x(db), rect.y_range(), Stroke::new(1.0, GRID));
		p.hline(rect.x_range(), axes.y(db), Stroke::new(1.0, GRID));
	}
	let (k_lo, k_hi) = v.knee_span();
	p.rect_filled(
		Rect::from_x_y_ranges(axes.x(k_lo)..=axes.x(k_hi), rect.y_range()),
		0.0,
		Color32::from_white_alpha(6),
	);
	p.add(Shape::dashed_line(
		&[axes.pos(comp::FLOOR_DB, comp::FLOOR_DB), axes.pos(0.0, 0.0)],
		Stroke::new(1.0, theme::BG_HOVER),
		3.0,
		3.0,
	));
	let steps = 120;
	let ins: Vec<f32> = (0..=steps)
		.map(|i| comp::FLOOR_DB + i as f32 / steps as f32 * -comp::FLOOR_DB)
		.collect();
	let curve: Vec<Pos2> = ins.iter().map(|&d| axes.pos(d, v.curve(d))).collect();
	// The gain reduction area: between the uncompressed line (+ makeup) and the curve.
	let fill: Vec<(Pos2, Pos2)> = ins
		.iter()
		.filter(|&&d| d >= k_lo)
		.map(|&d| (axes.pos(d, d + v.makeup), axes.pos(d, v.curve(d))))
		.collect();
	p.add(strip(&fill, theme::GAIN_REDUCTION.gamma_multiply(0.07)));
	let width = if big { 1.6 } else { 1.5 };
	if big {
		p.add(Shape::line(
			curve.clone(),
			Stroke::new(7.0, Color32::from_white_alpha(14)),
		));
	}
	p.add(Shape::line(curve, Stroke::new(width, theme::TEXT)));

	if input_db > comp::FLOOR_DB + 0.5 {
		let lp = axes.pos(input_db, v.curve(input_db));
		p.add(Shape::dashed_line(
			&[pos2(lp.x, rect.bottom()), lp, pos2(rect.left(), lp.y)],
			Stroke::new(1.0, theme::GAIN_REDUCTION.gamma_multiply(0.55)),
			2.0,
			3.0,
		));
		p.circle(
			lp,
			if big { 4.5 } else { 3.5 },
			theme::GAIN_REDUCTION,
			Stroke::new(1.5, theme::DISPLAY_BG),
		);
	}
	let dot = axes.pos(v.threshold, v.curve(v.threshold));
	p.circle(
		dot,
		if big { 7.0 } else { 5.5 },
		theme::TEXT,
		Stroke::new(1.5, theme::DISPLAY_BG),
	);
	if !big {
		return;
	}
	// The ratio handle: a diamond where the curve leaves the display.
	let edge = axes.pos(-0.6, v.curve(-0.6)) - vec2(6.0, 0.0);
	let d = 6.0;
	p.add(Shape::convex_polygon(
		vec![
			edge + vec2(0.0, -d),
			edge + vec2(d, 0.0),
			edge + vec2(0.0, d),
			edge + vec2(-d, 0.0),
		],
		theme::DISPLAY_BG,
		Stroke::new(1.5, theme::TEXT),
	));
	let tag = comp::transfer_tag(v);
	let g = p.layout_no_wrap(tag, theme::mono(11.0), theme::TEXT);
	let size = g.size() + vec2(12.0, 4.0);
	let mut at = dot + vec2(12.0, 6.0);
	at.x = at.x.min(rect.right() - size.x - 4.0);
	at.y = at.y.min(rect.bottom() - size.y - 4.0);
	let r = Rect::from_min_size(at, size);
	p.rect(
		r,
		CornerRadius::same(theme::RADIUS),
		theme::BG_WIDGET,
		Stroke::new(1.0, theme::BG_PRESSED),
		StrokeKind::Inside,
	);
	p.galley(r.min + vec2(6.0, 2.0), g, theme::TEXT);
	let font = theme::medium(10.5);
	p.text(
		rect.left_top() + vec2(8.0, 6.0),
		Align2::LEFT_TOP,
		"Transfer",
		font.clone(),
		theme::TEXT_FAINT,
	);
	p.text(
		rect.right_bottom() + vec2(-8.0, -6.0),
		Align2::RIGHT_BOTTOM,
		"in →",
		font.clone(),
		theme::TEXT_FAINT,
	);
	p.text(
		rect.right_top() + vec2(-20.0, 6.0),
		Align2::RIGHT_TOP,
		"ratio ⇕",
		font,
		theme::TEXT_FAINT,
	);
}

const GRID: Color32 = Color32::from_rgb(0x17, 0x19, 0x1c);

/// A filled band between pairs of (top, bottom) points.
fn strip(pairs: &[(Pos2, Pos2)], color: Color32) -> Shape {
	let mut mesh = Mesh::default();
	for (i, (a, b)) in pairs.iter().enumerate() {
		mesh.colored_vertex(*a, color);
		mesh.colored_vertex(*b, color);
		if i > 0 {
			let k = (i * 2) as u32;
			mesh.add_triangle(k - 2, k - 1, k);
			mesh.add_triangle(k - 1, k + 1, k);
		}
	}
	Shape::mesh(mesh)
}

// --- Gain reduction ---------------------------------------------------------------------

/// The dock's GR column: label, big readout, vertical meter with its scale.
fn gr_column(ui: &mut Ui, cx: &BodyCx, v: &CompView, rect: Rect) {
	let p = ui.painter();
	p.text(
		pos2(rect.center().x, rect.top() + 2.0),
		Align2::CENTER_TOP,
		"GR",
		theme::medium(10.5),
		theme::TEXT_FAINT,
	);
	p.text(
		pos2(rect.center().x, rect.top() + 18.0),
		Align2::CENTER_TOP,
		comp::gr_text(v.gr_db),
		theme::mono_medium(20.0),
		theme::GAIN_REDUCTION_TEXT,
	);
	let meter = Rect::from_min_max(
		pos2(rect.center().x - 16.0, rect.top() + 50.0),
		pos2(rect.center().x + 4.0, rect.bottom() - 8.0),
	);
	let mut child = ui.new_child(UiBuilder::new().max_rect(meter));
	device_kit::gr_meter(
		&mut child,
		Id::new(("comp-gr", cx.salt, cx.r.device)),
		v.gr_db,
		GR_RANGE_DB,
		meter.size(),
	)
	.on_hover_text("Gain reduction · the white tick holds the largest for 1.5 s");
	let p = ui.painter();
	for db in [0.0, 3.0, 6.0, 9.0, 12.0, 18.0, 24.0] {
		let y = meter.top() + db / GR_RANGE_DB * meter.height();
		p.text(
			pos2(meter.right() + 6.0, y),
			Align2::LEFT_CENTER,
			format!("{db:.0}"),
			theme::mono(10.0),
			theme::TEXT_FAINT,
		);
	}
}

// --- Knobs and chips --------------------------------------------------------------------

fn knob(
	ui: &mut Ui,
	state: &mut AppState,
	cx: &BodyCx,
	cell: Rect,
	id: &'static str,
	label: &'static str,
	size: KnobSize,
) {
	let mut cell_ui = ui.new_child(
		UiBuilder::new()
			.max_rect(cell)
			.layout(Layout::top_down(Align::Center)),
	);
	ParamKnob::new(cx.r, id)
		.size(size)
		.label(label)
		.color(knob_color(id))
		.id_salt(Id::new(cx.salt))
		.show(&mut cell_ui, state);
}

fn knob_color(id: &str) -> Color32 {
	match id {
		ATTACK | RELEASE => theme::GAIN_REDUCTION,
		MAKEUP => theme::LEVEL_OUTPUT,
		_ => theme::TEXT,
	}
}

/// "Auto suggests +4.5 · Apply" (or the compact "Auto makeup +4.5"): one undo step.
fn makeup_chip(ui: &mut Ui, state: &mut AppState, r: DeviceRef, compact: bool) {
	let values = shown_values(state, r);
	let Some(suggested) = comp::suggests(&values) else {
		return;
	};
	let text = if compact {
		format!("Auto makeup {}", comp::signed_db(suggested))
	} else {
		format!("Auto suggests {} · Apply", comp::signed_db(suggested))
	};
	let font = theme::medium(11.0);
	let g = ui
		.painter()
		.layout_no_wrap(text.clone(), font, theme::ACCENT_TEXT);
	let (rect, resp) = ui.allocate_exact_size(vec2(g.size().x + 14.0, 20.0), Sense::click());
	resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Auto makeup"));
	let p = ui.painter();
	if !compact || resp.hovered() {
		p.rect(
			rect,
			CornerRadius::same(theme::RADIUS),
			if resp.hovered() {
				theme::mix(theme::ACCENT_DIM, theme::ACCENT, 0.15)
			} else {
				theme::ACCENT_DIM
			},
			Stroke::new(1.0, theme::ACCENT_BORDER),
			StrokeKind::Inside,
		);
	}
	p.galley(rect.center() - g.size() * 0.5, g, theme::ACCENT_TEXT);
	let resp = resp
		.on_hover_cursor(CursorIcon::PointingHand)
		.on_hover_text(
			"Makeup that gives back the gain reduction of a −18 dBFS program level at these \
		 threshold, ratio and knee settings",
		);
	if resp.clicked() {
		commit(
			state,
			r,
			&[(MAKEUP, suggested)],
			&format!("Auto makeup {}", comp::signed_db(suggested)),
		);
	}
}

/// The sidechain picker: "Key from Kick · post-fader send ▾" (compact: "Key ▪ Kick ▾").
fn key_picker(ui: &mut Ui, state: &mut AppState, r: DeviceRef, width: Option<f32>) {
	let key = key_of(state, r);
	let compact = width.is_some();
	let name_font = theme::medium(12.0);
	let (lead, name, tail) = match &key {
		Some((_, name, _, _)) if compact => ("Key".to_owned(), name.clone(), String::new()),
		Some((_, name, _, pre)) => (
			"Key from".to_owned(),
			name.clone(),
			format!("· {}", comp::tap_label(*pre)),
		),
		None => (
			"Key".to_owned(),
			"Own input".to_owned(),
			if compact {
				String::new()
			} else {
				"· no sidechain".to_owned()
			},
		),
	};
	let p = ui.painter();
	let g_lead = p.layout_no_wrap(lead, theme::medium(11.5), theme::TEXT_FAINT);
	let g_name = p.layout_no_wrap(
		name,
		theme::semibold(12.0),
		if key.is_some() {
			theme::TEXT
		} else {
			theme::TEXT_DIM
		},
	);
	let g_tail = p.layout_no_wrap(tail, name_font, theme::TEXT_DIM);
	let content = 8.0
		+ g_lead.size().x
		+ 6.0 + if key.is_some() { 13.0 } else { 0.0 }
		+ g_name.size().x
		+ 6.0 + g_tail.size().x
		+ 22.0;
	let w = width.unwrap_or(content);
	let h = if compact { 24.0 } else { CHIP_H };
	let (rect, resp) = ui.allocate_exact_size(vec2(w, h), Sense::click());
	resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::ComboBox, true, "Key source"));
	let p = ui.painter();
	p.rect(
		rect,
		CornerRadius::same(theme::RADIUS_MD),
		if resp.hovered() {
			theme::BG_HOVER
		} else if compact {
			theme::BG_RAISED
		} else {
			theme::BG_WIDGET
		},
		Stroke::new(
			1.0,
			if compact {
				theme::BG_HOVER
			} else {
				Color32::TRANSPARENT
			},
		),
		StrokeKind::Inside,
	);
	let cy = rect.center().y;
	let mut x = rect.left() + 8.0;
	let lead_w = g_lead.size().x;
	p.galley(
		pos2(x, cy - g_lead.size().y * 0.5),
		g_lead,
		theme::TEXT_FAINT,
	);
	x += lead_w + 6.0;
	if let Some((_, _, color, _)) = &key {
		p.rect_filled(
			Rect::from_center_size(pos2(x + 4.0, cy), vec2(8.0, 8.0)),
			CornerRadius::same(2),
			*color,
		);
		x += 13.0;
	}
	let name_w = g_name.size().x;
	p.galley(pos2(x, cy - g_name.size().y * 0.5), g_name, theme::TEXT);
	x += name_w + 6.0;
	p.galley(pos2(x, cy - g_tail.size().y * 0.5), g_tail, theme::TEXT_DIM);
	p.text(
		pos2(rect.right() - 10.0, cy),
		Align2::CENTER_CENTER,
		"▾",
		theme::medium(11.0),
		theme::TEXT_FAINT,
	);
	let resp = resp
		.on_hover_cursor(CursorIcon::PointingHand)
		.on_hover_text("Sidechain: the track whose level drives the compressor");
	egui::Popup::menu(&resp).show(|ui| {
		ui.set_min_width(200.0);
		let current = key.as_ref().map(|k| k.0);
		let pre = key.as_ref().is_some_and(|k| k.3);
		let mut pick: Option<(Option<TrackId>, bool)> = None;
		if menu_row(
			ui,
			"Key from none",
			"None · own input",
			None,
			current.is_none(),
		) {
			pick = Some((None, false));
		}
		let sources = comp::key_sources(state.project(), r.track, r.device);
		if !sources.is_empty() {
			ui.separator();
		}
		for s in &sources {
			let color = theme::track_color(s.color.as_deref());
			if menu_row(
				ui,
				&format!("Key from {}", s.name),
				&s.name,
				Some(color),
				current == Some(s.track),
			) {
				pick = Some((Some(s.track), pre));
			}
		}
		if let Some(track) = current {
			ui.separator();
			for (label, tap) in [("Post-fader", false), ("Pre-fader", true)] {
				if menu_row(ui, label, label, None, pre == tap) {
					pick = Some((Some(track), tap));
				}
			}
		}
		if let Some((track, tap)) = pick {
			if track != current || (track.is_some() && tap != pre) {
				state.apply(comp::set_key(r.track, r.device, track, tap));
			}
			ui.close();
		}
	});
}

/// A popup row with an optional colour square; returns whether it was clicked.
fn menu_row(ui: &mut Ui, label: &str, text: &str, color: Option<Color32>, on: bool) -> bool {
	let g = ui
		.painter()
		.layout_no_wrap(text.to_owned(), theme::medium(12.0), theme::TEXT);
	let w = ui.available_width().max(g.size().x + 40.0);
	let (rect, resp) = ui.allocate_exact_size(vec2(w, 24.0), Sense::click());
	resp.widget_info(|| {
		egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, on, label)
	});
	let p = ui.painter();
	if resp.hovered() || on {
		p.rect_filled(
			rect,
			CornerRadius::same(theme::RADIUS),
			if resp.hovered() {
				theme::BG_HOVER
			} else {
				theme::BG_WIDGET
			},
		);
	}
	let mut x = rect.left() + 8.0;
	if let Some(c) = color {
		p.rect_filled(
			Rect::from_center_size(pos2(x + 4.0, rect.center().y), vec2(8.0, 8.0)),
			CornerRadius::same(2),
			c,
		);
		x += 14.0;
	}
	p.galley(
		pos2(x, rect.center().y - g.size().y * 0.5),
		g,
		if on { theme::TEXT } else { theme::TEXT_DIM },
	);
	resp.on_hover_cursor(CursorIcon::PointingHand).clicked()
}

/// The first automated compressor param, for the footer hint.
fn automated_hint(state: &AppState, r: DeviceRef) -> Option<String> {
	comp::info().params.iter().find_map(|p| {
		let address = ParamAddress::device(r.track, r.device, p.id);
		state.is_automated(&address).then(|| {
			format!(
				"{} is automated (Read) · ring shows the playing value",
				p.name
			)
		})
	})
}

/// "▸ Display: history 4 s · range 60 dB · key trace on" (dock) / "▸ Display" (compact).
fn footer_toggle(ui: &mut Ui, v: &mut CompView, full: bool) {
	let arrow = if v.footer { "▾" } else { "▸" };
	let text = if full {
		format!(
			"{arrow}  Display: history {:.0} s · range {} dB · key trace {}",
			v.seconds,
			if v.range60 { 60 } else { 30 },
			if v.key_trace { "on" } else { "off" }
		)
	} else {
		format!("{arrow}  More")
	};
	let g = ui
		.painter()
		.layout_no_wrap(text, theme::medium(12.0), theme::TEXT_DIM);
	let (rect, resp) = ui.allocate_exact_size(vec2(g.size().x + 16.0, CHIP_H), Sense::click());
	resp.widget_info(|| {
		egui::WidgetInfo::selected(
			egui::WidgetType::Checkbox,
			true,
			v.footer,
			"Display options",
		)
	});
	let bg = if v.footer || resp.hovered() {
		theme::BG_HOVER
	} else if full {
		theme::BG_WIDGET
	} else {
		Color32::TRANSPARENT
	};
	ui.painter()
		.rect_filled(rect, CornerRadius::same(theme::RADIUS_MD), bg);
	ui.painter()
		.galley(rect.center() - g.size() * 0.5, g, theme::TEXT_DIM);
	if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
		v.footer = !v.footer;
	}
}

/// History length, range and key trace (view state only).
fn display_settings(ui: &mut Ui, cx: &BodyCx, v: &mut CompView) {
	let dim = |ui: &mut Ui, t: &str| {
		ui.label(
			egui::RichText::new(t)
				.font(theme::medium(12.0))
				.color(theme::TEXT_FAINT),
		);
	};
	dim(ui, "History");
	let selected = HISTORY_SECONDS
		.iter()
		.position(|&s| s == v.seconds)
		.unwrap_or(1);
	if let Some(i) = device_kit::segmented(
		ui,
		Id::new(("comp-seconds", cx.salt, cx.r.device)),
		&["2 s", "4 s", "8 s"],
		selected,
	) {
		let rate = v.rate;
		v.resize_history(HISTORY_SECONDS[i], rate);
	}
	dim(ui, "Range");
	if let Some(i) = device_kit::segmented(
		ui,
		Id::new(("comp-range", cx.salt, cx.r.device)),
		&["30 dB", "60 dB"],
		usize::from(v.range60),
	) {
		v.range60 = i == 1;
	}
	if device_kit::toggle(ui, v.key_trace, "Key trace", theme::GAIN_REDUCTION)
		.on_hover_text("Draw the sidechain key's level in the history")
		.clicked()
	{
		v.key_trace = !v.key_trace;
	}
}

fn row(ui: &mut Ui, rect: Rect) -> Ui {
	ui.new_child(
		UiBuilder::new()
			.max_rect(rect)
			.layout(Layout::left_to_right(Align::Center)),
	)
}

// --- Dock (15c) -------------------------------------------------------------------------

fn dock(ui: &mut Ui, state: &mut AppState, cx: &BodyCx, v: &mut CompView) {
	let rect = cx.rect;
	let rows = FOOTER_H + if v.footer { SETTINGS_H } else { 0.0 };
	let top = Rect::from_min_max(rect.min, pos2(rect.right(), rect.bottom() - rows - 4.0));
	let side = top.height();
	let knobs_w = KNOBS_W.min(top.width() * 0.3);
	let displays = top.with_max_x(top.right() - knobs_w - GR_W - 16.0);
	if displays.width() < TABS_BELOW - KNOBS_W - GR_W {
		// Narrow: one display at a time under a Transfer · History tab strip.
		let tabs = Rect::from_min_size(displays.min, vec2(displays.width(), 24.0));
		let mut tab_row = row(ui, tabs);
		if let Some(i) = device_kit::segmented(
			&mut tab_row,
			Id::new(("comp-tabs", cx.salt, cx.r.device)),
			&["History", "Transfer"],
			usize::from(!v.history_tab),
		) {
			v.history_tab = i == 0;
		}
		let area = displays.with_min_y(tabs.bottom() + 6.0);
		if v.history_tab {
			history(ui, state, cx, v, area, true);
		} else {
			let s = area.width().min(area.height());
			transfer(ui, state, cx, v, Rect::from_min_size(area.min, vec2(s, s)));
		}
	} else {
		let history_rect = displays.with_max_x(displays.right() - side - 8.0);
		history(ui, state, cx, v, history_rect, true);
		let t = Rect::from_min_size(
			pos2(history_rect.right() + 8.0, top.top()),
			vec2(side, side),
		);
		transfer(ui, state, cx, v, t);
	}
	let gr = Rect::from_min_size(pos2(displays.right() + 8.0, top.top()), vec2(GR_W, side));
	gr_column(ui, cx, v, gr);

	// Knobs: Threshold, Ratio, Knee (L) over Attack, Release, Makeup (M) + the chip.
	let knobs = Rect::from_min_max(pos2(gr.right() + 8.0, top.top()), top.max);
	let col = knobs.width() / 3.0;
	let gap = (knobs.height() - 112.0 - 122.0).clamp(4.0, 18.0);
	let block = 112.0 + gap + 122.0;
	let y0 = knobs.top() + ((knobs.height() - block) * 0.5).max(0.0);
	for (i, (id, label)) in [(THRESHOLD, "Threshold"), (RATIO, "Ratio"), (KNEE, "Knee")]
		.into_iter()
		.enumerate()
	{
		let cell = Rect::from_min_size(pos2(knobs.left() + col * i as f32, y0), vec2(col, 112.0));
		knob(ui, state, cx, cell, id, label, KnobSize::L);
	}
	let y1 = y0 + 112.0 + gap;
	for (i, (id, label)) in [(ATTACK, "Attack"), (RELEASE, "Release"), (MAKEUP, "Makeup")]
		.into_iter()
		.enumerate()
	{
		let cell = Rect::from_min_size(pos2(knobs.left() + col * i as f32, y1), vec2(col, 94.0));
		knob(ui, state, cx, cell, id, label, KnobSize::M);
	}
	let chip = Rect::from_min_size(pos2(knobs.left(), y1 + 100.0), vec2(knobs.width(), 22.0));
	let mut chip_ui = ui.new_child(
		UiBuilder::new()
			.max_rect(chip)
			.layout(Layout::right_to_left(Align::Center)),
	);
	makeup_chip(&mut chip_ui, state, cx.r, false);

	// Footer: key picker, automation hint, display options.
	let foot = Rect::from_min_size(
		pos2(rect.left() - 8.0, rect.bottom() - rows),
		vec2(rect.width() + 16.0, FOOTER_H),
	);
	ui.painter().hline(
		foot.x_range(),
		foot.top(),
		Stroke::new(1.0, theme::BORDER_SUBTLE),
	);
	let mut bar = row(ui, foot.shrink2(vec2(10.0, 0.0)));
	bar.spacing_mut().item_spacing.x = 12.0;
	key_picker(&mut bar, state, cx.r, None);
	if let Some(hint) = automated_hint(state, cx.r) {
		bar.label(
			egui::RichText::new(hint)
				.font(theme::medium(12.0))
				.color(theme::TEXT_FAINT),
		);
	}
	bar.with_layout(Layout::right_to_left(Align::Center), |ui| {
		footer_toggle(ui, v, true);
	});
	if v.footer {
		let settings = Rect::from_min_size(
			pos2(foot.left(), foot.bottom()),
			vec2(foot.width(), SETTINGS_H),
		);
		ui.painter().rect_filled(settings, 0.0, theme::CANVAS);
		ui.painter().hline(
			settings.x_range(),
			settings.top(),
			Stroke::new(1.0, theme::BORDER_SUBTLE),
		);
		let mut r = row(ui, settings.shrink2(vec2(12.0, 0.0)));
		r.spacing_mut().item_spacing.x = 8.0;
		display_settings(&mut r, cx, v);
		r.with_layout(Layout::right_to_left(Align::Center), |ui| {
			ui.label(
				egui::RichText::new("Display only. No effect on the sound.")
					.font(theme::medium(12.0))
					.color(theme::TEXT_FAINT),
			);
		});
	}
}

// --- Compact (15d) ----------------------------------------------------------------------

/// How tall the compact form is at `width` (the inspector allocates it).
pub fn compact_height(width: f32, footer: bool) -> f32 {
	transfer_side(width) + compact_rest(footer)
}

fn transfer_side(width: f32) -> f32 {
	(width * 0.51).clamp(120.0, 220.0)
}

fn compact_rest(footer: bool) -> f32 {
	8.0 + COMPACT_HISTORY_H
		+ COMPACT_KNOBS_H
		+ COMPACT_ROW_H
		+ if footer {
			COMPACT_KNOBS_H + COMPACT_ROW_H
		} else {
			0.0
		}
}

fn compact(ui: &mut Ui, state: &mut AppState, cx: &BodyCx, v: &mut CompView) {
	let rect = cx.rect;
	let mut inner = ui.new_child(
		UiBuilder::new()
			.max_rect(rect)
			.layout(Layout::top_down(Align::Min)),
	);
	egui::ScrollArea::vertical()
		.id_salt(("comp-compact", cx.salt, cx.r.device))
		.auto_shrink([false, false])
		.show(&mut inner, |ui| {
			ui.spacing_mut().item_spacing.y = 0.0;
			let width = ui.available_width();
			if width >= WIDE_COMPACT {
				wide_compact(ui, state, cx, v, width, rect.height());
				return;
			}
			let side = transfer_side(width);
			let (top, _) = ui.allocate_exact_size(vec2(width, side), Sense::hover());
			let t = Rect::from_min_size(top.min, vec2(side, side));
			transfer(ui, state, cx, v, t);
			gr_panel(ui, state, cx, v, top.with_min_x(t.right() + 8.0));
			ui.add_space(8.0);
			let (h, _) = ui.allocate_exact_size(vec2(width, COMPACT_HISTORY_H), Sense::hover());
			history(ui, state, cx, v, h, false);
			// Five S knobs need about 64 px each; narrower, Makeup moves behind ▸.
			let wide = width >= 320.0;
			let slots = if wide { 5 } else { 4 };
			let mut main = vec![
				(THRESHOLD, "Thresh"),
				(RATIO, "Ratio"),
				(ATTACK, "Attack"),
				(RELEASE, "Release"),
			];
			if wide {
				main.push((MAKEUP, "Makeup"));
			}
			knob_row(ui, state, cx, width, slots, &main);
			compact_footer(ui, state, cx, v, width);
			if v.footer {
				let mut rest = vec![(KNEE, "Knee")];
				if !wide {
					rest.push((MAKEUP, "Makeup"));
				}
				if cx.info.param(MIX).is_some() {
					rest.push((MIX, "Mix"));
				}
				knob_row(ui, state, cx, width, slots, &rest);
				settings_row(ui, cx, v, width);
			}
		});
}

/// From this width the compact form puts the history beside the transfer curve and
/// every knob in one row (a short, wide dock).
const WIDE_COMPACT: f32 = 520.0;

fn wide_compact(
	ui: &mut Ui,
	state: &mut AppState,
	cx: &BodyCx,
	v: &mut CompView,
	width: f32,
	height: f32,
) {
	let rest = COMPACT_KNOBS_H + COMPACT_ROW_H + 8.0;
	let side = (height - rest).clamp(120.0, 220.0);
	let (top, _) = ui.allocate_exact_size(vec2(width, side), Sense::hover());
	let t = Rect::from_min_size(top.min, vec2(side, side));
	transfer(ui, state, cx, v, t);
	let panel = Rect::from_min_size(pos2(t.right() + 8.0, top.top()), vec2(170.0, side));
	gr_panel(ui, state, cx, v, panel);
	let h = top.with_min_x(panel.right() + 8.0);
	history(ui, state, cx, v, h, h.width() >= 360.0);
	let mut knobs = vec![
		(THRESHOLD, "Thresh"),
		(RATIO, "Ratio"),
		(KNEE, "Knee"),
		(ATTACK, "Attack"),
		(RELEASE, "Release"),
		(MAKEUP, "Makeup"),
	];
	if cx.info.param(MIX).is_some() {
		knobs.push((MIX, "Mix"));
	}
	knob_row(ui, state, cx, width.min(80.0 * 7.0), 7, &knobs);
	compact_footer(ui, state, cx, v, width);
	if v.footer {
		settings_row(ui, cx, v, width);
	}
}

/// "▸ More" and the auto makeup chip.
fn compact_footer(ui: &mut Ui, state: &mut AppState, cx: &BodyCx, v: &mut CompView, width: f32) {
	let (foot, _) = ui.allocate_exact_size(vec2(width, COMPACT_ROW_H), Sense::hover());
	ui.painter().hline(
		foot.x_range(),
		foot.top(),
		Stroke::new(1.0, theme::BORDER_SUBTLE),
	);
	let mut bar = row(ui, foot.shrink2(vec2(2.0, 0.0)));
	bar.spacing_mut().item_spacing.x = 6.0;
	footer_toggle(&mut bar, v, false);
	bar.with_layout(Layout::right_to_left(Align::Center), |ui| {
		makeup_chip(ui, state, cx.r, true);
	});
}

fn settings_row(ui: &mut Ui, cx: &BodyCx, v: &mut CompView, width: f32) {
	let (settings, _) = ui.allocate_exact_size(vec2(width, COMPACT_ROW_H), Sense::hover());
	let mut r = row(ui, settings.shrink2(vec2(2.0, 0.0)));
	r.spacing_mut().item_spacing.x = 6.0;
	display_settings(&mut r, cx, v);
}

/// One row of S knobs in `slots` equal slots of `width` (left aligned).
fn knob_row(
	ui: &mut Ui,
	state: &mut AppState,
	cx: &BodyCx,
	width: f32,
	slots: usize,
	knobs: &[(&'static str, &'static str)],
) {
	let (rect, _) = ui.allocate_exact_size(vec2(width, COMPACT_KNOBS_H), Sense::hover());
	let slot = width / slots as f32;
	for (i, &(id, label)) in knobs.iter().enumerate() {
		let cell = Rect::from_min_size(
			pos2(rect.left() + slot * i as f32, rect.top() + 8.0),
			vec2(slot, rect.height() - 8.0),
		);
		knob(ui, state, cx, cell, id, label, KnobSize::S);
	}
}

/// 15d's right column: GAIN REDUCTION, the readout, a horizontal meter and the key chip.
fn gr_panel(ui: &mut Ui, state: &mut AppState, cx: &BodyCx, v: &CompView, rect: Rect) {
	let p = ui.painter();
	p.text(
		rect.left_top() + vec2(2.0, 4.0),
		Align2::LEFT_TOP,
		"GAIN REDUCTION",
		theme::medium(10.5),
		theme::TEXT_FAINT,
	);
	let g = p.layout_no_wrap(
		comp::gr_text(v.gr_db),
		theme::mono_medium(24.0),
		theme::GAIN_REDUCTION_TEXT,
	);
	let w = g.size().x;
	let readout_y = rect.top() + 22.0;
	p.galley(
		pos2(rect.left() + 2.0, readout_y),
		g,
		theme::GAIN_REDUCTION_TEXT,
	);
	p.text(
		pos2(rect.left() + 8.0 + w, readout_y + 22.0),
		Align2::LEFT_BOTTOM,
		"dB",
		theme::mono(12.0),
		theme::TEXT_FAINT,
	);
	let bar = Rect::from_min_size(
		pos2(rect.left() + 2.0, readout_y + 36.0),
		vec2(rect.width() - 4.0, 8.0),
	);
	let mut child = ui.new_child(UiBuilder::new().max_rect(bar));
	device_kit::gr_meter(
		&mut child,
		Id::new(("comp-gr", cx.salt, cx.r.device)),
		v.gr_db,
		GR_RANGE_DB,
		bar.size(),
	);
	let p = ui.painter();
	for db in [0.0, 6.0, 12.0, 24.0] {
		let x = bar.left() + db / GR_RANGE_DB * bar.width();
		let align = if db == 0.0 {
			Align2::LEFT_TOP
		} else if db == GR_RANGE_DB {
			Align2::RIGHT_TOP
		} else {
			Align2::CENTER_TOP
		};
		p.text(
			pos2(x, bar.bottom() + 4.0),
			align,
			format!("{db:.0}"),
			theme::mono(10.0),
			theme::TEXT_FAINT,
		);
	}
	let chip = Rect::from_min_size(
		pos2(rect.left(), rect.bottom() - 24.0),
		vec2(rect.width(), 24.0),
	);
	let mut chip_ui = row(ui, chip);
	key_picker(&mut chip_ui, state, cx.r, Some(rect.width()));
}
