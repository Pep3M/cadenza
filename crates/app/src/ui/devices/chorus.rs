//! The Chorus editor body (design `16 Effects`, 16d): the two voices' modulated delay
//! scrolling at the real LFO rate (drag ⇕ depth, ⇔ rate), the stereo width fan (drag a
//! voice for width) and the knobs. While nothing plays and no audio passes the traces
//! freeze at their last phase and dim. Compact: the traces at full width, then rows of S
//! knobs. The gesture -> parameter mapping lives in [`crate::state::chorus_editor`].

use std::sync::{Arc, Mutex};

use egui::{Align2, Color32, CursorIcon, Id, Pos2, Rect, Sense, Shape, Stroke, Ui, pos2, vec2};

use crate::state::AppState;
use crate::state::chorus_editor::{
	self as ch, ChorusValues, DELAY, DEPTH, FEEDBACK, MIX, RATE, WIDTH,
};
use crate::state::selection::DeviceRef;
use crate::theme;
use crate::ui::device_editor::BodyCx;
use crate::ui::device_kit::{self, KnobSize};
use crate::ui::devices::{self, FOOTER_H, KnobDef};

const GAP: f32 = 8.0;
/// Share of the view that lies in the past (the "now" marker sits at 75 %).
const PAST: f32 = 0.75;
/// Below this the signal counts as silence (the traces freeze).
const SILENCE: f32 = 1e-5;

#[derive(Debug, Clone, Copy)]
struct WaveDrag {
	rate: f32,
	depth: f32,
	delta: egui::Vec2,
}

#[derive(Default)]
struct ChorusView {
	phase: f64,
	wave: Option<WaveDrag>,
	fan: bool,
	/// Screenshots: keep the traces moving without audio.
	synthetic: bool,
}

type Shared = Arc<Mutex<ChorusView>>;

fn view_id(salt: &str, r: DeviceRef) -> Id {
	Id::new(("chorus-editor", salt, r.device))
}

fn view(ctx: &egui::Context, id: Id) -> Shared {
	ctx.data_mut(|d| {
		d.get_temp_mut_or_insert_with(id, || Arc::new(Mutex::new(ChorusView::default())))
			.clone()
	})
}

fn lock(view: &Shared) -> std::sync::MutexGuard<'_, ChorusView> {
	view.lock().unwrap_or_else(|e| e.into_inner())
}

/// Shows the traces at `phase` and moving, as if audio were passing (screenshots).
pub fn set_display(ctx: &egui::Context, salt: &str, r: DeviceRef, phase: f64) {
	let v = view(ctx, view_id(salt, r));
	let mut v = lock(&v);
	v.phase = phase;
	v.synthetic = true;
}

fn values(state: &AppState, r: DeviceRef) -> ChorusValues {
	ChorusValues::from_fn(|p| {
		state
			.param_value(r.track, r.device, p.id)
			.unwrap_or(p.default)
	})
}

pub fn chorus_body(ui: &mut Ui, state: &mut AppState, cx: &BodyCx) {
	let shared = view(ui.ctx(), view_id(cx.salt, cx.r));
	let mut v = lock(&shared);
	if cx.compact {
		let height = compact_height(cx.rect.width());
		devices::compact_scroll(ui, cx, height, |ui, rect| {
			compact(ui, state, cx, &mut v, rect);
		});
	} else {
		dock(ui, state, cx, &mut v);
	}
}

/// Whether the LFO is audibly running: the transport plays or the probe hears audio.
fn running(state: &AppState, cx: &BodyCx, v: &ChorusView) -> bool {
	v.synthetic
		|| state.is_playing()
		|| (state.probe_target() == Some(cx.r.device)
			&& cx
				.frames
				.iter()
				.any(|f| f.output.iter().chain(&f.input).any(|s| s.abs() > SILENCE)))
}

// --- LFO traces ---------------------------------------------------------------------

fn lfo(
	ui: &mut Ui,
	state: &mut AppState,
	cx: &BodyCx,
	v: &mut ChorusView,
	rect: Rect,
	compact: bool,
) {
	let r = cx.r;
	let info = cx.info;
	let moving = running(state, cx, v);
	let visible = ui.is_rect_visible(rect);
	let values = values(state, r);
	if moving && visible {
		let dt = ui.input(|i| i.stable_dt).min(0.1);
		v.phase = ch::advance(v.phase, values.rate, dt);
		ui.ctx().request_repaint();
	}
	let lit =
		ui.ctx()
			.animate_bool_with_time(Id::new(("chorus-lit", cx.salt, r.device)), moving, 0.25);
	let dim = 0.5 + 0.5 * lit;

	let (top, bottom) = if compact { (22.0, 18.0) } else { (28.0, 22.0) };
	let plot = Rect::from_min_max(
		pos2(rect.left() + 1.0, rect.top() + top),
		pos2(rect.right() - 1.0, rect.bottom() - bottom),
	);
	let span = values.view_seconds();
	let past = span * PAST;
	let future = span - past;
	let x_of = |t: f32| plot.left() + (t + past) / span * plot.width();
	let cy = plot.center().y;
	let half = plot.height() * 0.5;
	let amp = values.trace_amplitude() * half;
	let px_per_pct = ch::TRACE_SCALE * half / 100.0;

	let resp = ui
		.interact(
			rect,
			Id::new(("chorus-lfo", cx.salt, r.device)),
			Sense::click_and_drag(),
		)
		.on_hover_cursor(CursorIcon::Move);
	resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Other, true, "Chorus LFO"));
	if resp.drag_started() {
		v.wave = Some(WaveDrag {
			rate: values.rate,
			depth: values.depth,
			delta: egui::Vec2::ZERO,
		});
	}
	if resp.dragged()
		&& let Some(d) = v.wave.as_mut()
	{
		d.delta += resp.drag_delta();
		let (rate, depth) = ch::drag_wave((d.rate, d.depth), d.delta.x, d.delta.y, px_per_pct);
		state.preview_param(r.device, RATE, rate);
		state.preview_param(r.device, DEPTH, depth);
	}
	if resp.drag_stopped()
		&& let Some(d) = v.wave.take()
	{
		let (rate, depth) = ch::drag_wave((d.rate, d.depth), d.delta.x, d.delta.y, px_per_pct);
		devices::commit_changes(state, r, info, &[(RATE, rate), (DEPTH, depth)], |ids| {
			ch::wave_label(ids.contains(&RATE), ids.contains(&DEPTH)).to_owned()
		});
	}
	if resp.double_clicked() {
		devices::commit_changes(state, r, info, &ch::reset_wave(), |_| {
			"Reset chorus rate and depth".to_owned()
		});
	}
	let resp = resp.on_hover_text("Drag up/down for depth, sideways for rate; double-click resets");

	if !visible {
		return;
	}
	let p = ui.painter().with_clip_rect(rect);
	device_kit::paint_display(&p, rect);
	let inner = p.with_clip_rect(rect.shrink(1.0));
	let fade = |c: Color32| c.gamma_multiply(dim);

	// The swing of the read point, and the base delay through the middle.
	inner.rect_filled(
		Rect::from_x_y_ranges(plot.x_range(), cy - amp..=cy + amp),
		0.0,
		Color32::from_white_alpha(6),
	);
	for t in ch::quarter_lines(v.phase, values.rate, past, future) {
		inner.vline(
			x_of(t),
			plot.y_range(),
			Stroke::new(1.0, device_kit::GRID_MAJOR),
		);
	}
	inner.hline(plot.x_range(), cy, Stroke::new(1.0, theme::BG_HOVER));
	if amp > 2.0 {
		for y in [cy - amp, cy + amp] {
			inner.extend(Shape::dashed_line(
				&[pos2(plot.left(), y), pos2(plot.right(), y)],
				Stroke::new(1.0, theme::BORDER_STRONG),
				3.0,
				4.0,
			));
		}
	}

	let n = (plot.width() / 3.0).clamp(80.0, 600.0) as usize;
	let offset = values.offset_cycles();
	for (voice, color) in [(0usize, theme::STEREO_L), (1, theme::STEREO_R)] {
		let pts: Vec<Pos2> = ch::trace(v.phase, values.rate, voice, offset, past, future, n)
			.into_iter()
			.map(|(t, val)| pos2(x_of(t), cy - val * amp))
			.collect();
		theme::glow_line(&inner, pts, fade(color));
	}

	// Now: the marker and where each voice reads.
	let now_x = x_of(0.0);
	inner.vline(
		now_x,
		rect.top() + 1.0..=rect.bottom() - 1.0,
		Stroke::new(1.0, Color32::from_white_alpha(64)),
	);
	for (voice, color) in [(0usize, theme::STEREO_L), (1, theme::STEREO_R)] {
		let at = pos2(now_x, cy - ch::voice_value(v.phase, voice, offset) * amp);
		inner.circle_filled(at, 6.0, fade(color));
		inner.circle_stroke(at, 6.0, Stroke::new(1.5, theme::DISPLAY_BG));
	}

	let label = theme::mono(if compact { 9.5 } else { 10.5 });
	let edge = if compact { 7.0 } else { 9.0 };
	let y_top = rect.top() + if compact { 11.0 } else { 14.0 };
	let y_bottom = rect.bottom() - if compact { 9.0 } else { 11.0 };
	let apart = ch::apart_label(&values);
	let apart_short = format!("Voice R \u{b7} {:.0}\u{b0}", values.offset_degrees());
	let legend_right = devices::legend(
		&p,
		pos2(rect.left() + edge, y_top),
		&[
			("Voice L", theme::STEREO_L),
			(if compact { &apart_short } else { &apart }, theme::STEREO_R),
		],
	);
	let excursion = ch::excursion_label(&values);
	let ex_galley = p.layout_no_wrap(excursion.clone(), label.clone(), theme::TEXT_FAINT);
	if legend_right + ex_galley.size().x < rect.right() - edge {
		p.galley(
			pos2(
				rect.right() - edge - ex_galley.size().x,
				y_top - ex_galley.size().y * 0.5,
			),
			ex_galley,
			theme::TEXT_FAINT,
		);
	}
	let rate = p.text(
		pos2(rect.left() + edge, y_bottom),
		Align2::LEFT_CENTER,
		ch::rate_label(&values),
		label.clone(),
		theme::TEXT_FAINT,
	);
	if rate.right() + 8.0 < now_x + 8.0 {
		p.text(
			pos2(now_x + 8.0, y_bottom),
			Align2::LEFT_CENTER,
			"now",
			label,
			theme::TEXT_FAINT,
		);
	}
	if v.wave.is_some()
		&& resp.dragged()
		&& let Some(at) = resp.interact_pointer_pos()
	{
		device_kit::paint_value_tag(
			&p,
			at + vec2(14.0, -14.0),
			Align2::LEFT_BOTTOM,
			&format!(
				"{} \u{b7} depth {:.0} %",
				crate::state::device::format_value(values.rate, "Hz"),
				values.depth
			),
			theme::TEXT,
			rect,
		);
	}
}

// --- Stereo width fan -----------------------------------------------------------------

/// A point on the fan: `degrees` from the centre line, clockwise.
fn fan_point(center: Pos2, radius: f32, degrees: f32) -> Pos2 {
	let a = degrees.to_radians();
	pos2(center.x + radius * a.sin(), center.y - radius * a.cos())
}

fn arc(center: Pos2, radius: f32, from: f32, to: f32) -> Vec<Pos2> {
	let n = (((to - from).abs() / 3.0).ceil() as usize).max(2);
	(0..=n)
		.map(|i| fan_point(center, radius, from + (to - from) * i as f32 / n as f32))
		.collect()
}

fn fan(ui: &mut Ui, state: &mut AppState, cx: &BodyCx, v: &mut ChorusView, rect: Rect) {
	let r = cx.r;
	let values = values(state, r);
	let radius = (rect.width() * 0.5 - 22.0)
		.min(rect.height() - 92.0)
		.max(30.0);
	let center = pos2(rect.center().x, rect.top() + 30.0 + radius);
	let resp = ui
		.interact(
			rect,
			Id::new(("chorus-fan", cx.salt, r.device)),
			Sense::click_and_drag(),
		)
		.on_hover_cursor(CursorIcon::ResizeHorizontal);
	resp.widget_info(|| {
		egui::WidgetInfo::labeled(egui::WidgetType::Other, true, "Stereo width fan")
	});
	if resp.drag_started() {
		v.fan = true;
	}
	let pointer_width = resp
		.interact_pointer_pos()
		.map(|at| ch::fan_width(at.x - center.x, radius));
	if resp.dragged()
		&& v.fan
		&& let Some(w) = pointer_width
	{
		state.preview_param(r.device, WIDTH, w);
	}
	if resp.drag_stopped() && std::mem::take(&mut v.fan) {
		let w = pointer_width.unwrap_or(values.width);
		devices::commit_changes(state, r, cx.info, &[(WIDTH, w)], |_| {
			"Chorus width".to_owned()
		});
	}
	if resp.double_clicked() {
		devices::commit_changes(state, r, cx.info, &[ch::reset_width()], |_| {
			"Reset chorus width".to_owned()
		});
	}
	let resp =
		resp.on_hover_text("Drag a voice sideways for the stereo width; double-click resets");
	if !ui.is_rect_visible(rect) {
		return;
	}
	let p = ui.painter().with_clip_rect(rect);
	device_kit::paint_display(&p, rect);
	let inner = p.with_clip_rect(rect.shrink(1.0));
	let al = ch::fan_angle(values.width, 0);
	let ar = ch::fan_angle(values.width, 1);
	inner.add(Shape::line(
		arc(center, radius, -90.0, 90.0),
		Stroke::new(1.0, theme::BORDER),
	));
	let mut wedge = vec![center];
	wedge.extend(arc(center, radius, al, ar));
	if ar - al > 0.5 {
		let mut mesh = egui::Mesh::default();
		let fill = Color32::from_white_alpha(18);
		mesh.colored_vertex(center, fill);
		for (i, pt) in wedge.iter().skip(1).enumerate() {
			mesh.colored_vertex(*pt, fill);
			if i > 0 {
				let k = i as u32 + 1;
				mesh.add_triangle(0, k - 1, k);
			}
		}
		inner.add(Shape::mesh(mesh));
		inner.add(Shape::line(
			arc(center, radius, al, ar),
			Stroke::new(2.0, Color32::from_white_alpha(150)),
		));
	}
	inner.extend(Shape::dashed_line(
		&[center, pos2(center.x, center.y - radius - 6.0)],
		Stroke::new(1.0, theme::BORDER),
		3.0,
		3.0,
	));
	let hot = resp.hovered() || v.fan;
	for (a, color) in [(al, theme::STEREO_L), (ar, theme::STEREO_R)] {
		let at = fan_point(center, radius, a);
		if hot {
			inner.circle_filled(at, 12.0, color.gamma_multiply(0.18));
		}
		inner.circle_filled(at, 7.0, color);
	}
	inner.circle_filled(center, 5.0, theme::TEXT);
	p.text(
		rect.left_top() + vec2(9.0, 8.0),
		Align2::LEFT_TOP,
		"Stereo width",
		theme::medium(10.5),
		theme::TEXT_FAINT,
	);
	for (x, text) in [(center.x - radius, "L"), (center.x + radius, "R")] {
		p.text(
			pos2(x, center.y + 14.0),
			Align2::CENTER_CENTER,
			text,
			theme::medium(11.0),
			theme::TEXT_FAINT,
		);
	}
	p.text(
		pos2(rect.center().x, rect.bottom() - 12.0),
		Align2::CENTER_CENTER,
		format!("{:.0} %", values.width),
		theme::mono(11.0),
		theme::TEXT_DIM,
	);
}

// --- Dock (16d) ---------------------------------------------------------------------

const WIDE_KNOBS: [KnobDef; 6] = [
	KnobDef::new(RATE, "Rate", KnobSize::L),
	KnobDef::new(DEPTH, "Depth", KnobSize::L),
	KnobDef::new(DELAY, "Delay", KnobSize::M),
	KnobDef::new(FEEDBACK, "Feedback", KnobSize::M),
	KnobDef::new(WIDTH, "Width", KnobSize::M),
	KnobDef::new(MIX, "Mix", KnobSize::M),
];

fn small(knobs: &[KnobDef]) -> Vec<KnobDef> {
	knobs.iter().map(|k| k.sized(KnobSize::S)).collect()
}

fn dock(ui: &mut Ui, state: &mut AppState, cx: &BodyCx, v: &mut ChorusView) {
	let rect = cx.rect;
	let top = Rect::from_min_max(rect.min, pos2(rect.right(), rect.bottom() - FOOTER_H - 4.0));
	let wide = rect.width() >= 1500.0;
	let knobs_w = if wide { 580.0 } else { 270.0 };
	let fan_w = if rect.width() >= 1300.0 {
		320.0
	} else {
		(top.height() * 0.8).clamp(200.0, 260.0)
	};
	let lfo_w = (top.width() - knobs_w - fan_w - GAP * 2.0).max(300.0);
	let lfo_rect = top.with_max_x(top.left() + lfo_w);
	lfo(ui, state, cx, v, lfo_rect, false);
	let fan_rect = Rect::from_min_size(
		pos2(lfo_rect.right() + GAP, top.top()),
		vec2(fan_w, top.height()),
	);
	fan(ui, state, cx, v, fan_rect);
	let knobs = Rect::from_min_max(pos2(fan_rect.right() + GAP, top.top()), top.max);
	if wide {
		devices::knob_grid(ui, state, cx, knobs, &WIDE_KNOBS, 6);
	} else {
		devices::knob_grid(ui, state, cx, knobs, &small(&WIDE_KNOBS), 3);
	}
	let moving = running(state, cx, v);
	devices::footer(
		ui,
		rect,
		if moving {
			"Drag the wave up or down for depth, sideways for rate; drag a voice on the fan for \
			 the stereo width."
		} else {
			"Transport stopped: the traces keep moving if audio is passing, otherwise they \
			 freeze at their last phase and dim to 50 %."
		},
	);
}

// --- Compact -------------------------------------------------------------------------

fn compact_lfo_h(width: f32) -> f32 {
	(width * 0.45).clamp(120.0, 170.0)
}

/// How tall the compact form is at `width`.
pub fn compact_height(width: f32) -> f32 {
	compact_lfo_h(width) + 8.0 + devices::small_rows_height(width, WIDE_KNOBS.len())
}

fn compact(ui: &mut Ui, state: &mut AppState, cx: &BodyCx, v: &mut ChorusView, rect: Rect) {
	let lfo_rect = Rect::from_min_size(rect.min, vec2(rect.width(), compact_lfo_h(rect.width())));
	lfo(ui, state, cx, v, lfo_rect, true);
	let knobs = small(&WIDE_KNOBS);
	let cols = devices::small_columns(rect.width(), knobs.len());
	let k = Rect::from_min_max(pos2(rect.left(), lfo_rect.bottom() + 8.0), rect.max);
	devices::knob_grid(ui, state, cx, k, &knobs, cols);
}
