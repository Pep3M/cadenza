//! The Reverb editor body (design `16 Effects`, 16b): the tail picture (early
//! reflections, lows/highs decay, pre-delay/decay/damping nodes), the space box
//! (size/width), the low-cut strip, the wet output meter and the knobs. Compact: the
//! picture at full width, then one row of S knobs. The gesture -> parameter mapping
//! lives in [`crate::state::reverb_editor`].

use std::sync::{Arc, Mutex};

use egui::{
	Align, Align2, Color32, CornerRadius, CursorIcon, Id, Layout, Pos2, Rect, Sense, Shape, Stroke,
	StrokeKind, Ui, UiBuilder, pos2, vec2,
};

use crate::state::AppState;
use crate::state::device_display::DbRange;
use crate::state::meters::{FLOOR_DB, amp_to_db};
use crate::state::reverb_editor::{
	self as rv, DAMPING, LOW_CUT, PICTURE_FLOOR_DB, PRE_DELAY, ReverbValues, SIZE, WIDTH,
};
use crate::state::selection::DeviceRef;
use crate::theme;
use crate::ui::device_editor::BodyCx;
use crate::ui::device_kit::{self, FreqDisplay, FreqEvent, FreqNode, KnobSize, ParamKnob};

const FOOTER_H: f32 = 32.0;
const GAP: f32 = 10.0;
const STRIP_H: f32 = 62.0;
const CAPTION_H: f32 = 18.0;
const METER_W: f32 = 30.0;
const FALL_DB_PER_S: f32 = 24.0;
/// Decay and size are violet, damping teal, timing white (design 16b).
const VIOLET: Color32 = theme::BAND_COLORS[6];
const TEAL: Color32 = theme::STEREO_L;
/// The low-cut strip: deep enough to show the slope, one labelled line each side of 0.
const STRIP_RANGE: DbRange = DbRange {
	min: -24.0,
	max: 12.0,
	step: 12.0,
};
const WHEEL_REST: f64 = 0.5;

/// Which pure parameter a horizontal/vertical node drag or wheel is shaping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Grab {
	PreDelay,
	Decay,
	Damping,
	Size,
	Width,
}

/// A drag in progress: the parameter's value when it started, and the pixel offset
/// accumulated since (converted to the parameter's units each frame).
#[derive(Debug, Clone, Copy)]
struct NodeDrag {
	grab: Grab,
	start: f32,
	px: f32,
}

/// A wheel edit not committed yet (decay over the tail, size over the space box).
#[derive(Debug, Clone, Copy)]
struct WheelEdit {
	grab: Grab,
	start: f32,
	notches: f32,
	value: f32,
	last: f64,
}

struct ReverbView {
	drag: Option<NodeDrag>,
	wheel: Option<WheelEdit>,
	output_db: [f32; 2],
	synthetic: bool,
}

impl Default for ReverbView {
	fn default() -> Self {
		Self {
			drag: None,
			wheel: None,
			output_db: [FLOOR_DB; 2],
			synthetic: false,
		}
	}
}

type Shared = Arc<Mutex<ReverbView>>;

fn view_id(salt: &str, r: DeviceRef) -> Id {
	Id::new(("reverb-editor", salt, r.device))
}

fn view(ctx: &egui::Context, id: Id) -> Shared {
	ctx.data_mut(|d| {
		d.get_temp_mut_or_insert_with(id, || Arc::new(Mutex::new(ReverbView::default())))
			.clone()
	})
}

fn lock(view: &Shared) -> std::sync::MutexGuard<'_, ReverbView> {
	view.lock().unwrap_or_else(|e| e.into_inner())
}

/// Shows the wet output meter in an editor as if the probe had measured it (screenshots).
pub fn set_display(ctx: &egui::Context, salt: &str, r: DeviceRef, output_db: [f32; 2]) {
	let v = view(ctx, view_id(salt, r));
	let mut v = lock(&v);
	v.output_db = output_db;
	v.synthetic = true;
}

fn shown_values(state: &AppState, r: DeviceRef) -> ReverbValues {
	ReverbValues::from_fn(|p| {
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
		.or_else(|| rv::info().param(id).map(|p| p.default))
		.unwrap_or(0.0)
}

fn differs(a: f32, b: f32) -> bool {
	(a - b).abs() > 1e-4 * a.abs().max(1.0)
}

/// The changes that differ from the document, committed as one undo step.
fn commit(state: &mut AppState, r: DeviceRef, changes: &[rv::Change], label: &str) {
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

pub fn reverb_body(ui: &mut Ui, state: &mut AppState, cx: &BodyCx) {
	let shared = view(ui.ctx(), view_id(cx.salt, cx.r));
	let mut v = lock(&shared);
	update_meter(ui, state, cx, &mut v);
	finish_wheel(ui, state, cx.r, &mut v);
	if cx.compact {
		compact(ui, state, cx, &mut v);
	} else {
		dock(ui, state, cx, &mut v);
	}
}

/// Feeds the wet output meter from this frame's probe frames; falls once the probe goes
/// quiet.
fn update_meter(ui: &Ui, state: &AppState, cx: &BodyCx, v: &mut ReverbView) {
	if v.synthetic {
		return;
	}
	let dt = ui.input(|i| i.stable_dt).min(0.25);
	let ours = state.probe_target() == Some(cx.r.device);
	let fall = FALL_DB_PER_S * dt;
	let mut peak = [0.0f32; 2];
	if ours && !cx.frames.is_empty() {
		for f in cx.frames {
			for (p, o) in peak.iter_mut().zip(f.output) {
				*p = p.max(o.abs());
			}
		}
	}
	for (db, amp) in v.output_db.iter_mut().zip(peak) {
		*db = amp_to_db(amp).max(*db - fall).max(FLOOR_DB);
	}
	if v.output_db.iter().any(|&d| d > FLOOR_DB) {
		ui.ctx().request_repaint();
	}
}

fn param_of(grab: Grab) -> &'static str {
	match grab {
		Grab::PreDelay => PRE_DELAY,
		Grab::Decay => rv::DECAY,
		Grab::Damping => DAMPING,
		Grab::Size => SIZE,
		Grab::Width => WIDTH,
	}
}

/// The value after a drag of `amount` (the grab's units) from `start`. The damping node
/// starts from its dB on the highs, so it follows the pointer.
fn apply_grab(grab: Grab, start: f32, amount: f32) -> f32 {
	match grab {
		Grab::PreDelay => rv::drag_pre_delay(start, amount),
		Grab::Decay => rv::drag_decay(start, amount),
		Grab::Damping => rv::damping_for_node_db(start + amount),
		Grab::Size => rv::drag_size(start, amount),
		Grab::Width => rv::drag_width(start, amount),
	}
}

fn label_of(grab: Grab) -> &'static str {
	match grab {
		Grab::PreDelay => "Reverb pre-delay",
		Grab::Decay => "Reverb decay",
		Grab::Damping => "Reverb damping",
		Grab::Size => "Reverb size",
		Grab::Width => "Reverb width",
	}
}

fn finish_wheel(ui: &Ui, state: &mut AppState, r: DeviceRef, v: &mut ReverbView) {
	let Some(w) = v.wheel else {
		return;
	};
	let idle = ui.input(|i| i.time) - w.last;
	if idle >= WHEEL_REST {
		v.wheel = None;
		commit(state, r, &[(param_of(w.grab), w.value)], label_of(w.grab));
	} else {
		ui.ctx()
			.request_repaint_after(std::time::Duration::from_secs_f64(WHEEL_REST - idle));
	}
}

fn wheel(
	ui: &Ui,
	state: &mut AppState,
	r: DeviceRef,
	v: &mut ReverbView,
	grab: Grab,
	notches: f32,
) {
	let now = ui.input(|i| i.time);
	if let Some(w) = v.wheel.filter(|w| w.grab != grab) {
		v.wheel = None;
		commit(state, r, &[(param_of(w.grab), w.value)], label_of(w.grab));
	}
	let start = state
		.param_value(r.track, r.device, param_of(grab))
		.unwrap_or_else(|| model_value(state, r, param_of(grab)));
	let w = v.wheel.get_or_insert(WheelEdit {
		grab,
		start,
		notches: 0.0,
		value: start,
		last: now,
	});
	w.notches += notches;
	w.last = now;
	w.value = match grab {
		Grab::Decay => rv::wheel_decay(w.start, w.notches, false),
		Grab::Size => rv::wheel_size(w.start, w.notches),
		_ => w.value,
	};
	state.preview_param(r.device, param_of(grab), w.value);
	ui.ctx()
		.request_repaint_after(std::time::Duration::from_secs_f64(WHEEL_REST));
}

/// Starts, advances or ends a node/box drag, previewing and committing through `apply`.
#[allow(clippy::too_many_arguments)]
fn drag(
	state: &mut AppState,
	r: DeviceRef,
	v: &mut ReverbView,
	resp: &egui::Response,
	grab: Grab,
	axis: f32,
	px_per_unit: f32,
	start_value: f32,
) {
	if resp.drag_started() {
		v.drag = Some(NodeDrag {
			grab,
			start: start_value,
			px: 0.0,
		});
	}
	if resp.dragged()
		&& let Some(d) = v.drag.as_mut().filter(|d| d.grab == grab)
	{
		d.px += axis;
		let value = apply_grab(grab, d.start, d.px / px_per_unit);
		state.preview_param(r.device, param_of(grab), value);
	}
	if resp.drag_stopped()
		&& let Some(d) = v.drag.take_if(|d| d.grab == grab)
	{
		let value = apply_grab(grab, d.start, d.px / px_per_unit);
		commit(state, r, &[(param_of(grab), value)], label_of(grab));
	}
}

fn reset_on_double_click(state: &mut AppState, r: DeviceRef, resp: &egui::Response, grab: Grab) {
	if resp.double_clicked() {
		let change = match grab {
			Grab::PreDelay => rv::reset_pre_delay(),
			Grab::Decay => rv::reset_decay(),
			Grab::Damping => rv::reset_damping(),
			Grab::Size => rv::reset_size(),
			Grab::Width => rv::reset_width(),
		};
		commit(state, r, &[change], &format!("Reset {}", label_of(grab)));
	}
}

// --- Tail picture -----------------------------------------------------------------------

/// Where the tail picture draws: the plot inside the dB gutter, legend and time labels.
struct TailMap {
	plot: Rect,
	span: f32,
}

impl TailMap {
	fn new(rect: Rect, span: f32, compact: bool) -> Self {
		let (left, top, bottom) = if compact {
			(26.0, 20.0, 16.0)
		} else {
			(36.0, 30.0, 20.0)
		};
		let plot = Rect::from_min_max(
			pos2(rect.left() + left, rect.top() + top),
			pos2(rect.right() - 10.0, rect.bottom() - bottom),
		);
		Self { plot, span }
	}

	fn x(&self, t: f32) -> f32 {
		self.plot.left() + rv::time_to_t(t, self.span) * self.plot.width()
	}

	fn y(&self, db: f32) -> f32 {
		let db = db.clamp(PICTURE_FLOOR_DB, 0.0);
		self.plot.top() + db / PICTURE_FLOOR_DB * self.plot.height()
	}

	fn px_per_db(&self) -> f32 {
		self.plot.height() / -PICTURE_FLOOR_DB
	}

	fn px_per_second(&self) -> f32 {
		self.plot.width() / self.span.max(0.01)
	}

	fn points(&self, pts: &[(f32, f32)]) -> Vec<Pos2> {
		pts.iter()
			.map(|&(t, db)| pos2(self.x(t), self.y(db)))
			.collect()
	}
}

/// A time-grid step that keeps labels at least `min_px` apart.
fn time_step(px_per_second: f32, min_px: f32) -> f32 {
	[0.1, 0.25, 0.5, 1.0, 2.0, 5.0]
		.into_iter()
		.find(|s| s * px_per_second >= min_px)
		.unwrap_or(10.0)
}

fn seconds_label(t: f32) -> String {
	if t.fract().abs() < 1e-3 {
		format!("{t:.0} s")
	} else if (t * 10.0).fract().abs() < 1e-3 {
		format!("{t:.1} s")
	} else {
		format!("{t:.2} s")
	}
}

fn node_positions(values: &ReverbValues, m: &TailMap) -> [(Grab, Pos2); 3] {
	let decay_t = rv::decay_node_time(values);
	let damp_t = rv::damping_node_time(values);
	[
		(
			Grab::PreDelay,
			pos2(m.x(values.pre_delay_seconds()), m.y(0.0)),
		),
		(Grab::Decay, pos2(m.x(decay_t), m.y(rv::FLOOR_DB))),
		(
			Grab::Damping,
			pos2(m.x(damp_t), m.y(values.highs_db(damp_t))),
		),
	]
}

/// The swatch legend, right-aligned so the top-left stays free for the pre-delay tag.
fn legend(p: &egui::Painter, right_center: Pos2, font: f32) {
	let items = [
		("Early reflections", theme::TEXT, 2.0),
		("Tail, lows", VIOLET, 6.0),
		("Tail, highs (damped)", TEAL, 6.0),
	];
	let mut x = right_center.x;
	for (label, color, h) in items.into_iter().rev() {
		let r = p.text(
			pos2(x, right_center.y),
			Align2::RIGHT_CENTER,
			label,
			theme::medium(font),
			theme::TEXT_DIM,
		);
		p.rect_filled(
			Rect::from_center_size(pos2(r.left() - 10.0, right_center.y), vec2(10.0, h)),
			CornerRadius::same(1),
			color,
		);
		x = r.left() - 28.0;
	}
}

fn tail(
	ui: &mut Ui,
	state: &mut AppState,
	cx: &BodyCx,
	v: &mut ReverbView,
	rect: Rect,
	compact: bool,
) {
	let r = cx.r;
	let values = shown_values(state, r);
	let m = TailMap::new(rect, values.view_span(), compact);
	let plot = m.plot;
	let nodes = node_positions(&values, &m);

	let resp = ui.interact(
		rect,
		Id::new(("reverb-tail", cx.salt, r.device)),
		Sense::click_and_drag(),
	);
	resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Other, true, "Reverb tail"));
	if ui.is_rect_visible(rect) {
		let p = ui.painter().with_clip_rect(rect);
		device_kit::paint_display(&p, rect);
		let label_font = theme::mono(if compact { 9.0 } else { 10.0 });

		// dB axis: a line every 12 dB, labelled in the left gutter.
		let mut db = 0.0;
		while db >= rv::FLOOR_DB - 0.1 {
			let y = m.y(db);
			p.hline(plot.x_range(), y, Stroke::new(1.0, device_kit::GRID_MAJOR));
			if !compact || db % 24.0 == 0.0 {
				p.text(
					pos2(plot.left() - 6.0, y),
					Align2::RIGHT_CENTER,
					rv::minus(format!("{db:.0}")),
					label_font.clone(),
					theme::TEXT_FAINT,
				);
			}
			db -= 12.0;
		}
		// Time axis.
		let step = time_step(m.px_per_second(), if compact { 44.0 } else { 70.0 });
		let mut t = step;
		while t < m.span - step * 0.3 {
			let x = m.x(t);
			p.vline(
				x,
				plot.top()..=plot.bottom(),
				Stroke::new(1.0, device_kit::GRID_MAJOR),
			);
			p.text(
				pos2(x, rect.bottom() - 3.0),
				Align2::CENTER_BOTTOM,
				seconds_label(t),
				label_font.clone(),
				theme::TEXT_FAINT,
			);
			t += step;
		}
		p.text(
			pos2(plot.left() - 6.0, rect.bottom() - 3.0),
			Align2::RIGHT_BOTTOM,
			"dB",
			label_font.clone(),
			theme::TEXT_FAINT,
		);

		// The pre-delay gap: shaded, with a bracket at 0 dB up to the node.
		let pd_x = m.x(values.pre_delay_seconds());
		let plain = p.with_clip_rect(plot.expand(1.0));
		plain.rect_filled(
			Rect::from_x_y_ranges(plot.left()..=pd_x, plot.y_range()),
			0.0,
			Color32::from_white_alpha(9),
		);
		let y0 = m.y(0.0);
		let bracket = Stroke::new(1.0, theme::TEXT.gamma_multiply(0.5));
		plain.hline(plot.left()..=pd_x, y0, bracket);
		plain.vline(plot.left(), y0 - 5.0..=y0 + 5.0, bracket);

		// The RT60 marker: where the lows reach -60 dB.
		let decay_x = nodes[1].1.x;
		plain.extend(Shape::dashed_line(
			&[pos2(decay_x, plot.top()), pos2(decay_x, plot.bottom())],
			Stroke::new(1.0, VIOLET.gamma_multiply(0.7)),
			4.0,
			3.0,
		));
		plain.hline(
			pd_x..=plot.right(),
			m.y(rv::FLOOR_DB),
			Stroke::new(1.0, VIOLET.gamma_multiply(0.25)),
		);

		let n = (plot.width() / 4.0).clamp(60.0, 260.0) as usize;
		let lows = m.points(&values.tail_points(false, m.span, n));
		let highs = m.points(&values.tail_points(true, m.span, n));
		plain.add(device_kit::fill_to(
			&lows,
			plot.bottom(),
			VIOLET.gamma_multiply(0.22),
		));
		theme::glow_line(&plain, lows, VIOLET);
		plain.add(device_kit::fill_to(
			&highs,
			plot.bottom(),
			TEAL.gamma_multiply(0.18),
		));
		theme::glow_line(&plain, highs, TEAL);

		// Early reflections: discrete bounces rising from the floor, each a little quieter.
		for (i, et) in values.early_reflection_times().into_iter().enumerate() {
			let x = m.x(et);
			plain.vline(
				x,
				m.y(ReverbValues::early_reflection_db(i))..=plot.bottom(),
				Stroke::new(
					if compact { 1.0 } else { 1.5 },
					theme::TEXT.gamma_multiply(0.9),
				),
			);
		}

		if !compact || rect.width() >= 320.0 {
			legend(
				&p,
				pos2(
					rect.right() - 10.0,
					rect.top() + if compact { 10.0 } else { 15.0 },
				),
				if compact { 9.5 } else { 10.5 },
			);
		}

		for (grab, at) in nodes {
			let color = match grab {
				Grab::PreDelay => theme::TEXT,
				Grab::Decay => VIOLET,
				_ => TEAL,
			};
			let radius = if grab == Grab::PreDelay { 6.0 } else { 7.0 };
			let lit = v.drag.is_some_and(|d| d.grab == grab);
			if lit {
				plain.circle_filled(at, radius + 6.0, color.gamma_multiply(0.2));
			}
			let fill = if lit {
				theme::mix(color, Color32::WHITE, 0.3)
			} else {
				color
			};
			plain.circle_filled(at, radius, fill);
			plain.circle_stroke(at, radius, Stroke::new(1.5, theme::DISPLAY_BG));
		}

		if !compact {
			let bounds = plot.with_max_y(rect.bottom() - 18.0);
			let light = |c: Color32| theme::mix(c, Color32::WHITE, 0.45);
			let [(_, pre), (_, decay), (_, damp)] = nodes;
			// Above the plot, clear of the tail's peak right after the gap.
			device_kit::paint_value_tag(
				&p,
				pos2(pre.x + 10.0, rect.top() + 15.0),
				Align2::LEFT_CENTER,
				&rv::pre_delay_label(values.pre_delay),
				theme::TEXT,
				rect,
			);
			let decay_text = rv::decay_label(values.decay);
			let fits_right = decay.x + 140.0 < plot.right();
			device_kit::paint_value_tag(
				&p,
				decay
					+ if fits_right {
						vec2(12.0, -16.0)
					} else {
						vec2(-12.0, -16.0)
					},
				if fits_right {
					Align2::LEFT_BOTTOM
				} else {
					Align2::RIGHT_BOTTOM
				},
				&decay_text,
				light(VIOLET),
				bounds,
			);
			device_kit::paint_value_tag(
				&p,
				damp + vec2(12.0, 10.0),
				Align2::LEFT_TOP,
				&rv::damping_label(values.damping),
				light(TEAL),
				bounds,
			);
		}
	}

	for (grab, at) in nodes {
		let id = Id::new(("reverb-node", cx.salt, r.device, grab as u8));
		let node_rect = Rect::from_center_size(at, egui::Vec2::splat(20.0));
		let (label, cursor) = match grab {
			Grab::PreDelay => ("Pre-delay node", CursorIcon::ResizeHorizontal),
			Grab::Decay => ("Decay node", CursorIcon::ResizeHorizontal),
			_ => ("Damping node", CursorIcon::ResizeVertical),
		};
		let node_resp = ui
			.interact(node_rect, id, Sense::click_and_drag())
			.on_hover_cursor(cursor);
		node_resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Other, true, label));
		let axis = node_resp.drag_delta();
		let (amount, scale, start_value) = match grab {
			Grab::PreDelay => (axis.x, m.px_per_second(), values.pre_delay),
			Grab::Decay => (axis.x, m.px_per_second(), values.decay),
			_ => (
				axis.y,
				-m.px_per_db(),
				values.highs_db(rv::damping_node_time(&values)),
			),
		};
		drag(state, r, v, &node_resp, grab, amount, scale, start_value);
		reset_on_double_click(state, r, &node_resp, grab);
		let hint = match grab {
			Grab::PreDelay => rv::pre_delay_label(values.pre_delay),
			Grab::Decay => format!(
				"{} (RT60: the lows fall 60 dB)",
				rv::decay_label(values.decay)
			),
			_ => format!(
				"{} · drag down for a darker tail",
				rv::damping_label(values.damping)
			),
		};
		node_resp.on_hover_text(hint);
	}

	if resp.hovered()
		&& let Some(notches) = ui.input(|i| {
			let n = i.smooth_scroll_delta.y / 48.0;
			(n.abs() > 0.01).then_some(n)
		}) {
		ui.input_mut(|i| i.smooth_scroll_delta = egui::Vec2::ZERO);
		wheel(ui, state, r, v, Grab::Decay, notches);
	}
}

// --- Space box, low cut, meter -----------------------------------------------------------

/// The room picture: a box in perspective whose size grows with Size and whose width
/// stretches with Width, the speakers' spread, and the two drag handles.
fn space_box(ui: &mut Ui, state: &mut AppState, cx: &BodyCx, v: &mut ReverbView, rect: Rect) {
	let r = cx.r;
	let values = shown_values(state, r);
	let area = Rect::from_min_max(
		pos2(rect.left() + 12.0, rect.top() + 24.0),
		pos2(rect.right() - 12.0, rect.bottom() - 28.0),
	);
	let shape = rv::room_shape(values.size, values.width);
	let c = area.center();
	let front = Rect::from_center_size(
		c + vec2(0.0, area.height() * 0.06),
		vec2(shape.w * area.width(), shape.h * area.height()),
	);
	let back = Rect::from_center_size(
		front.center() - vec2(0.0, front.height() * 0.16),
		front.size() * shape.depth,
	);
	let corner_at = front.right_bottom();
	let edge_at = front.right_center();

	let corner_id = Id::new(("reverb-size", cx.salt, r.device));
	let corner = ui
		.interact(
			Rect::from_center_size(corner_at, vec2(20.0, 20.0)),
			corner_id,
			Sense::click_and_drag(),
		)
		.on_hover_cursor(CursorIcon::ResizeNwSe)
		.on_hover_text("Room size: drag the corner, double-click resets");
	corner.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Other, true, "Size corner"));
	let edge_id = Id::new(("reverb-width", cx.salt, r.device));
	let edge = ui
		.interact(
			Rect::from_center_size(edge_at, vec2(14.0, (front.height() - 26.0).max(12.0))),
			edge_id,
			Sense::click_and_drag(),
		)
		.on_hover_cursor(CursorIcon::ResizeHorizontal)
		.on_hover_text("Stereo width: drag the side, double-click resets");
	edge.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Other, true, "Width edge"));

	if ui.is_rect_visible(rect) {
		let p = ui.painter().with_clip_rect(rect);
		device_kit::paint_display(&p, rect);
		let p = p.with_clip_rect(rect.shrink(1.0));
		// Echo rings spreading from the room (bigger room, wider rings).
		for k in 1..=3 {
			let radius = front.width().max(front.height()) * (0.45 + 0.28 * k as f32);
			p.circle_stroke(
				front.center(),
				radius,
				Stroke::new(1.0, VIOLET.gamma_multiply(0.3 / k as f32)),
			);
		}
		// Floor, then walls joined in perspective.
		p.add(Shape::convex_polygon(
			vec![
				back.left_bottom(),
				back.right_bottom(),
				front.right_bottom(),
				front.left_bottom(),
			],
			VIOLET.gamma_multiply(0.10),
			Stroke::NONE,
		));
		let wall = Stroke::new(1.0, VIOLET.gamma_multiply(0.45));
		p.rect(
			back,
			CornerRadius::same(2),
			VIOLET.gamma_multiply(0.05),
			wall,
			StrokeKind::Middle,
		);
		for (a, b) in [
			(back.left_top(), front.left_top()),
			(back.right_top(), front.right_top()),
			(back.left_bottom(), front.left_bottom()),
			(back.right_bottom(), front.right_bottom()),
		] {
			p.line_segment([a, b], wall);
		}
		p.rect(
			front,
			CornerRadius::same(4),
			VIOLET.gamma_multiply(0.06),
			Stroke::new(1.5, VIOLET.gamma_multiply(0.8)),
			StrokeKind::Middle,
		);
		// Speakers on the back wall, the listener near the front, the stereo spread
		// between them.
		let half = shape.spread * front.width() * 0.5;
		let speaker_y = back.center().y + back.height() * 0.1;
		let sl = pos2(c.x - half, speaker_y);
		let sr = pos2(c.x + half, speaker_y);
		let listener = pos2(c.x, front.bottom() - front.height() * 0.16);
		p.add(Shape::convex_polygon(
			vec![listener, sl, sr],
			Color32::from_white_alpha(14),
			Stroke::NONE,
		));
		p.circle_filled(sl, 4.5, theme::STEREO_L);
		p.circle_filled(sr, 4.5, theme::STEREO_R);
		p.circle_filled(listener, 5.0, theme::TEXT);

		for (at, resp, grab) in [
			(corner_at, &corner, Grab::Size),
			(edge_at, &edge, Grab::Width),
		] {
			let lit = resp.hovered() || v.drag.is_some_and(|d| d.grab == grab);
			let radius = if lit { 5.5 } else { 4.0 };
			if lit {
				p.circle_filled(at, radius + 5.0, VIOLET.gamma_multiply(0.25));
			}
			p.circle_filled(at, radius, theme::TEXT);
			p.circle_stroke(at, radius, Stroke::new(1.5, theme::DISPLAY_BG));
		}

		p.text(
			rect.left_top() + vec2(10.0, 8.0),
			Align2::LEFT_TOP,
			"Space",
			theme::medium(10.5),
			theme::TEXT_FAINT,
		);
		p.text(
			pos2(rect.center().x, rect.bottom() - 9.0),
			Align2::CENTER_BOTTOM,
			rv::space_label(values.size, values.width),
			theme::mono(11.0),
			theme::TEXT_DIM,
		);
	}

	let delta = corner.drag_delta();
	drag(
		state,
		r,
		v,
		&corner,
		Grab::Size,
		(delta.x + delta.y) * 0.5,
		rv::size_px_per_pct(area.width(), area.height(), values.width),
		values.size,
	);
	reset_on_double_click(state, r, &corner, Grab::Size);
	drag(
		state,
		r,
		v,
		&edge,
		Grab::Width,
		edge.drag_delta().x,
		rv::width_px_per_pct(area.width(), values.size),
		values.width,
	);
	reset_on_double_click(state, r, &edge, Grab::Width);

	let box_resp = ui.interact(
		rect,
		Id::new(("reverb-space-hover", cx.salt, r.device)),
		Sense::hover(),
	);
	if box_resp.hovered() || corner.hovered() || edge.hovered() {
		let notches = ui.input(|i| i.smooth_scroll_delta.y / 48.0);
		if notches.abs() > 0.01 {
			ui.input_mut(|i| i.smooth_scroll_delta = egui::Vec2::ZERO);
			wheel(ui, state, r, v, Grab::Size, notches);
		}
	}
}

fn caption(ui: &Ui, rect: Rect, left: &str, right: &str) {
	let p = ui.painter();
	p.text(
		rect.left_center(),
		Align2::LEFT_CENTER,
		left,
		theme::medium(10.5),
		theme::TEXT_FAINT,
	);
	p.text(
		rect.right_center(),
		Align2::RIGHT_CENTER,
		right,
		theme::mono(10.5),
		theme::TEXT_DIM,
	);
}

fn low_cut_strip(ui: &mut Ui, state: &mut AppState, cx: &BodyCx, rect: Rect) {
	let r = cx.r;
	let values = shown_values(state, r);
	let cap = Rect::from_min_size(rect.min, vec2(rect.width(), CAPTION_H));
	caption(
		ui,
		cap,
		"Low cut on the wet signal",
		&crate::state::device::format_value(values.low_cut, "Hz"),
	);
	let display_rect = Rect::from_min_size(
		pos2(rect.left(), cap.bottom() + 2.0),
		vec2(rect.width(), STRIP_H),
	);
	let mut child = ui.new_child(UiBuilder::new().max_rect(display_rect));
	let node = FreqNode {
		label: "HP",
		freq: values.low_cut,
		db: 0.0,
		color: theme::TEXT,
		dormant: false,
		x_only: true,
		ai: state.ai_change(r.device, LOW_CUT).is_some(),
	};
	let nodes = [node];
	let low_cut = values.low_cut;
	let response = move |f: f32| rv::low_cut_response_db(low_cut, f);
	let out = FreqDisplay::new(
		Id::new(("reverb-lowcut", cx.salt, r.device)),
		display_rect.size(),
	)
	.range(STRIP_RANGE)
	.sum(&response, theme::TEXT)
	.sum_fill(0.07)
	.small_nodes()
	.nodes(&nodes)
	.show(&mut child);
	for e in out.events {
		match e {
			FreqEvent::Move {
				freq, commit: true, ..
			} => {
				commit(state, r, &[(LOW_CUT, freq)], "Reverb low cut");
			}
			FreqEvent::Move {
				freq,
				commit: false,
				..
			} => {
				state.preview_param(r.device, LOW_CUT, freq);
			}
			FreqEvent::Reset { .. } => {
				commit(
					state,
					r,
					&[(LOW_CUT, rv::info().param(LOW_CUT).unwrap().default)],
					"Reset reverb low cut",
				);
			}
			_ => {}
		}
	}
	out.response
		.on_hover_text("Low cut on the wet signal: drag the node, double-click resets");
}

/// The wet output as a vertical L/R pair with peak hold (the kit's level meter).
fn wet_meter(ui: &mut Ui, cx: &BodyCx, v: &ReverbView, rect: Rect) {
	let p = ui.painter();
	p.text(
		pos2(rect.center().x, rect.top()),
		Align2::CENTER_TOP,
		"WET",
		theme::medium(10.0),
		theme::TEXT_FAINT,
	);
	let peak = v.output_db[0].max(v.output_db[1]);
	let value = if peak <= FLOOR_DB {
		"\u{2212}\u{221e}".to_owned()
	} else {
		rv::minus(format!("{peak:.1}"))
	};
	p.text(
		pos2(rect.center().x, rect.bottom()),
		Align2::CENTER_BOTTOM,
		value,
		theme::mono(10.5),
		theme::TEXT_DIM,
	);
	let bar = Rect::from_min_max(
		pos2(rect.center().x - 10.0, rect.top() + 18.0),
		pos2(rect.center().x + 10.0, rect.bottom() - 18.0),
	);
	let mut child = ui.new_child(UiBuilder::new().max_rect(bar));
	device_kit::level_meter(
		&mut child,
		Id::new(("reverb-wet", cx.salt, cx.r.device)),
		&v.output_db,
		bar.size(),
	)
	.on_hover_text("The reverb's output (L / R) · the white tick holds the peak for 1.5 s");
}

fn row(ui: &mut Ui, rect: Rect) -> Ui {
	ui.new_child(
		UiBuilder::new()
			.max_rect(rect)
			.layout(Layout::left_to_right(Align::Center)),
	)
}

const KNOBS: [(&str, &str); 6] = [
	(SIZE, "Size"),
	(rv::DECAY, "Decay"),
	(DAMPING, "Damping"),
	(PRE_DELAY, "Pre-delay"),
	(WIDTH, "Width"),
	(LOW_CUT, "Low cut"),
];

fn knob_color(id: &str) -> Color32 {
	match id {
		SIZE | rv::DECAY => VIOLET,
		DAMPING => TEAL,
		_ => theme::TEXT,
	}
}

/// The six knobs, three per row, centred in their cells.
fn knob_grid(ui: &mut Ui, state: &mut AppState, cx: &BodyCx, rect: Rect) {
	let cols = 3;
	let rows = KNOBS.len().div_ceil(cols);
	let cell = vec2(rect.width() / cols as f32, rect.height() / rows as f32);
	for (i, (id, label)) in KNOBS.into_iter().enumerate() {
		let at = rect.min + vec2((i % cols) as f32 * cell.x, (i / cols) as f32 * cell.y);
		let mut cell_ui = ui.new_child(
			UiBuilder::new()
				.max_rect(Rect::from_min_size(at, cell))
				.layout(Layout::top_down(Align::Center)),
		);
		ParamKnob::new(cx.r, id)
			.size(KnobSize::S)
			.label(label)
			.color(knob_color(id))
			.id_salt(Id::new(cx.salt))
			.show(&mut cell_ui, state);
	}
}

// --- Dock (16b) ---------------------------------------------------------------------------

fn dock(ui: &mut Ui, state: &mut AppState, cx: &BodyCx, v: &mut ReverbView) {
	let rect = cx.rect;
	let top = Rect::from_min_max(rect.min, pos2(rect.right(), rect.bottom() - FOOTER_H - 4.0));
	let col_w = if rect.width() >= 1400.0 { 300.0 } else { 240.0 };
	let space_w = if rect.width() >= 1250.0 { 240.0 } else { 190.0 };
	let tail_w = (top.width() - METER_W - col_w - space_w - GAP * 3.0).max(280.0);
	let tail_rect = top.with_max_x(top.left() + tail_w);
	tail(ui, state, cx, v, tail_rect, false);

	let space_rect = Rect::from_min_size(
		pos2(tail_rect.right() + GAP, top.top()),
		vec2(space_w, top.height()),
	);
	space_box(ui, state, cx, v, space_rect);

	let col = Rect::from_min_size(
		pos2(space_rect.right() + GAP, top.top()),
		vec2(col_w, top.height()),
	);
	low_cut_strip(ui, state, cx, col);
	let knobs = col.with_min_y(col.top() + CAPTION_H + 2.0 + STRIP_H + 8.0);
	knob_grid(ui, state, cx, knobs);

	let meter_rect = Rect::from_min_max(
		pos2(col.right() + GAP, top.top()),
		pos2(top.right(), top.bottom()),
	);
	wet_meter(ui, cx, v, meter_rect);

	let foot = Rect::from_min_size(
		pos2(rect.left() - 8.0, rect.bottom() - FOOTER_H),
		vec2(rect.width() + 16.0, FOOTER_H),
	);
	ui.painter().hline(
		foot.x_range(),
		foot.top(),
		Stroke::new(1.0, theme::BORDER_SUBTLE),
	);
	let mut bar = row(ui, foot.shrink2(vec2(10.0, 0.0)));
	bar.add(
		egui::Label::new(
			egui::RichText::new(
				"Drag the violet node for decay, the teal node for damping, the white node for \
				 pre-delay; the room's corner for size, its side for width.",
			)
			.font(theme::medium(12.0))
			.color(theme::TEXT_DIM),
		)
		.truncate(),
	);
}

// --- Compact --------------------------------------------------------------------------

const COMPACT_KNOB_ROW_H: f32 = 78.0;

/// How tall the compact form is at `width`.
pub fn compact_height(width: f32) -> f32 {
	compact_tail_h(width) + 8.0 + COMPACT_KNOB_ROW_H * 2.0
}

fn compact_tail_h(width: f32) -> f32 {
	(width * 0.5).clamp(130.0, 200.0)
}

fn compact(ui: &mut Ui, state: &mut AppState, cx: &BodyCx, v: &mut ReverbView) {
	let rect = cx.rect;
	let mut inner = ui.new_child(
		UiBuilder::new()
			.max_rect(rect)
			.layout(Layout::top_down(Align::Min)),
	);
	egui::ScrollArea::vertical()
		.id_salt(("reverb-compact", cx.salt, cx.r.device))
		.auto_shrink([false, false])
		.show(&mut inner, |ui| {
			ui.spacing_mut().item_spacing.y = 0.0;
			let width = ui.available_width();
			let (t, _) = ui.allocate_exact_size(vec2(width, compact_tail_h(width)), Sense::hover());
			tail(ui, state, cx, v, t, true);
			ui.add_space(8.0);
			let (k, _) =
				ui.allocate_exact_size(vec2(width, COMPACT_KNOB_ROW_H * 2.0), Sense::hover());
			knob_grid(ui, state, cx, k);
		});
}
