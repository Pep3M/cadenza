//! The Gain / Utility editor body (design `16 Effects`, 16f, natural width 900 centred
//! in the dock): the goniometer of the device's output (drag ⇔ pan, ⇕ width;
//! double-click centres), the correlation meter, the big gain knob (0.5 dB wheel steps),
//! pan and width, and the polarity toggles. M = mono while the editor has the focus.
//! Compact: the goniometer, S knobs, toggles. The pure logic lives in
//! [`crate::state::gain_editor`].

use crate::i18n::{mark, tr, trf};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use egui::{
	Align, Align2, Color32, CornerRadius, CursorIcon, Id, Layout, Rect, Sense, Shape, Stroke, Ui,
	UiBuilder, pos2, vec2,
};

use crate::state::AppState;
use crate::state::gain_editor::{
	self as gn, GAIN, GONIO_POINTS, GainValues, INVERT_L, INVERT_R, PAN, Phase, WIDTH,
};
use crate::state::selection::DeviceRef;
use crate::theme;
use crate::ui::device_editor::BodyCx;
use crate::ui::device_kit::{self, KnobSize};
use crate::ui::devices::{self, FOOTER_H, KnobDef};

/// The design's natural width; wider docks centre it.
pub const NATURAL_WIDTH: f32 = 900.0;
const CORR_H: f32 = 22.0;
const GAP: f32 = 12.0;
/// Points fade out this long after the audio stops.
const HOLD: f64 = 0.5;
const SILENCE: f32 = 1e-5;
const DOT: Color32 = theme::LEVEL_OUTPUT;
const CORAL: Color32 = theme::BAND_COLORS[0];

#[derive(Default)]
struct GainView {
	points: VecDeque<[f32; 2]>,
	heard: f64,
	drag: Option<(f32, f32, egui::Vec2)>,
	synthetic: bool,
}

type Shared = Arc<Mutex<GainView>>;

fn view_id(salt: &str, r: DeviceRef) -> Id {
	Id::new(("gain-editor", salt, r.device))
}

fn view(ctx: &egui::Context, id: Id) -> Shared {
	ctx.data_mut(|d| {
		d.get_temp_mut_or_insert_with(id, || Arc::new(Mutex::new(GainView::default())))
			.clone()
	})
}

fn lock(view: &Shared) -> std::sync::MutexGuard<'_, GainView> {
	view.lock().unwrap_or_else(|e| e.into_inner())
}

/// Shows `frames` (output L/R) on the goniometer as if the probe had delivered them
/// (screenshots).
pub fn set_display(ctx: &egui::Context, salt: &str, r: DeviceRef, frames: &[[f32; 2]]) {
	let v = view(ctx, view_id(salt, r));
	let mut v = lock(&v);
	v.points = frames
		.iter()
		.rev()
		.take(GONIO_POINTS)
		.rev()
		.copied()
		.collect();
	v.synthetic = true;
}

fn values(state: &AppState, r: DeviceRef) -> GainValues {
	GainValues::from_fn(|p| {
		state
			.param_value(r.track, r.device, p.id)
			.unwrap_or(p.default)
	})
}

/// M toggles mono (width 0 / 100 %) while the editor has the focus.
pub fn shortcuts(ctx: &egui::Context, state: &mut AppState, r: DeviceRef) {
	if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::M)) {
		let (id, value) = gn::toggle_mono(values(state, r).width);
		state.commit_param(r.track, r.device, id, value);
	}
}

pub fn gain_body(ui: &mut Ui, state: &mut AppState, cx: &BodyCx) {
	let shared = view(ui.ctx(), view_id(cx.salt, cx.r));
	let mut v = lock(&shared);
	let now = ui.input(|i| i.time);
	listen(cx, state, &mut v, now);
	if cx.compact {
		let height = compact_height(cx.rect.width());
		devices::compact_scroll(ui, cx, height, |ui, rect| {
			compact(ui, state, cx, &mut v, now, rect);
		});
	} else {
		dock(ui, state, cx, &mut v, now);
	}
}

fn listen(cx: &BodyCx, state: &AppState, v: &mut GainView, now: f64) {
	if v.synthetic || state.probe_target() != Some(cx.r.device) {
		return;
	}
	if !cx
		.frames
		.iter()
		.any(|f| f.output.iter().any(|s| s.abs() > SILENCE))
	{
		return;
	}
	v.heard = now;
	for f in cx.frames {
		if v.points.len() == GONIO_POINTS {
			v.points.pop_front();
		}
		v.points.push_back(f.output);
	}
}

/// How bright the points are: full while audio passes, fading once it stops.
fn liveness(v: &GainView, now: f64) -> f32 {
	if v.synthetic {
		1.0
	} else {
		(1.0 - ((now - v.heard - HOLD) / HOLD).clamp(0.0, 1.0)) as f32
	}
}

// --- Goniometer -----------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn goniometer(
	ui: &mut Ui,
	state: &mut AppState,
	cx: &BodyCx,
	v: &mut GainView,
	rect: Rect,
	now: f64,
) {
	let r = cx.r;
	let values = values(state, r);
	let c = rect.center();
	let radius = rect.width().min(rect.height()) * 0.5 - 10.0;
	let resp = ui
		.interact(
			rect,
			Id::new(("gain-gonio", cx.salt, r.device)),
			Sense::click_and_drag(),
		)
		.on_hover_cursor(CursorIcon::Move);
	resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Other, true, tr("Goniometer")));
	let per_pct = radius / 100.0;
	if resp.drag_started() {
		v.drag = Some((values.pan, values.width, egui::Vec2::ZERO));
	}
	if resp.dragged()
		&& let Some((pan, width, delta)) = v.drag.as_mut()
	{
		*delta += resp.drag_delta();
		let (p, w) = gn::drag_gonio((*pan, *width), delta.x, delta.y, per_pct, per_pct);
		state.preview_param(r.device, PAN, p);
		state.preview_param(r.device, WIDTH, w);
	}
	if resp.drag_stopped()
		&& let Some((pan, width, delta)) = v.drag.take()
	{
		let (p, w) = gn::drag_gonio((pan, width), delta.x, delta.y, per_pct, per_pct);
		devices::commit_changes(state, r, cx.info, &[(PAN, p), (WIDTH, w)], |ids| {
			gn::image_label(ids.contains(&PAN), ids.contains(&WIDTH)).to_owned()
		});
	}
	if resp.double_clicked() {
		devices::commit_changes(state, r, cx.info, &gn::reset_image(), |_| {
			"Reset gain pan and width".to_owned()
		});
	}
	let resp = resp.on_hover_text(tr(
		"Drag sideways for pan, up/down for width; double-click centres (width 100 %)",
	));
	if !ui.is_rect_visible(rect) {
		return;
	}
	let values = self::values(state, r);
	let p = ui.painter().with_clip_rect(rect);
	device_kit::paint_display(&p, rect);
	let inner = p.with_clip_rect(rect.shrink(1.0));
	let at = |x: f32, y: f32| pos2(c.x + x * radius, c.y - y * radius);
	let (top, right, bottom, left) = (at(0.0, 1.0), at(1.0, 0.0), at(0.0, -1.0), at(-1.0, 0.0));
	let axis = Stroke::new(1.0, device_kit::GRID_MAJOR);
	inner.line_segment([top, bottom], axis);
	inner.line_segment([left, right], axis);
	inner.line_segment([at(-0.5, 0.5), at(0.5, -0.5)], axis);
	inner.line_segment([at(0.5, 0.5), at(-0.5, -0.5)], axis);
	inner.add(Shape::closed_line(
		vec![top, right, bottom, left],
		Stroke::new(1.0, theme::BORDER),
	));

	// Where the controls put a centred source: the pan needle and the width bracket.
	let image = Stroke::new(1.0, theme::TEXT.gamma_multiply(0.22));
	let needle = (values.pan * 0.01 * 45.0).to_radians();
	inner.extend(Shape::dashed_line(
		&[
			c,
			pos2(
				c.x + radius * 0.92 * needle.sin(),
				c.y - radius * 0.92 * needle.cos(),
			),
		],
		image,
		3.0,
		3.0,
	));
	let half_w = values.width / 200.0 * radius * 0.6;
	if half_w > 1.0 {
		inner.hline(c.x - half_w..=c.x + half_w, c.y, image);
		for x in [c.x - half_w, c.x + half_w] {
			inner.vline(x, c.y - 4.0..=c.y + 4.0, image);
		}
	}

	let alive = liveness(v, now);
	if alive > 0.0 && !v.points.is_empty() {
		let frames = v.points.make_contiguous();
		let scale = gn::gonio_scale(frames);
		let color = DOT.gamma_multiply(0.55 * alive);
		let mut mesh = egui::Mesh::default();
		for f in frames.iter() {
			let (x, y) = gn::gonio_point(f[0], f[1], scale);
			let pt = at(x, y);
			mesh.add_colored_rect(Rect::from_min_size(pt, vec2(1.6, 1.6)), color);
		}
		inner.add(Shape::mesh(mesh));
		if alive < 1.0 {
			ui.ctx().request_repaint();
		}
	} else if !resp.dragged() {
		p.text(
			c + vec2(0.0, radius * 0.5),
			Align2::CENTER_CENTER,
			tr("No signal"),
			theme::medium(10.5),
			theme::TEXT_FAINT,
		);
	}

	let font = theme::medium(10.5);
	p.text(
		top + vec2(0.0, 9.0),
		Align2::CENTER_CENTER,
		"M",
		font.clone(),
		theme::TEXT_FAINT,
	);
	p.text(
		right - vec2(9.0, 0.0),
		Align2::CENTER_CENTER,
		"S",
		font.clone(),
		theme::TEXT_FAINT,
	);
	p.text(
		at(-0.5, 0.5) + vec2(-9.0, -9.0),
		Align2::CENTER_CENTER,
		"L",
		font.clone(),
		theme::STEREO_L,
	);
	p.text(
		at(0.5, 0.5) + vec2(9.0, -9.0),
		Align2::CENTER_CENTER,
		"R",
		font,
		theme::STEREO_R,
	);
	if v.drag.is_some()
		&& let Some(pointer) = resp.interact_pointer_pos()
	{
		device_kit::paint_value_tag(
			&p,
			pointer + vec2(14.0, -14.0),
			Align2::LEFT_BOTTOM,
			&trf(
				"Pan {pan_text} · Width {width} %",
				&[
					("pan_text", &gn::pan_text(values.pan)),
					(
						"width",
						&crate::i18n::decimal(format!("{:.0}", values.width)),
					),
				],
			),
			theme::TEXT,
			rect,
		);
	}
}

fn correlation_meter(ui: &mut Ui, cx: &BodyCx, v: &mut GainView, rect: Rect, now: f64) {
	let raw = gn::correlation(v.points.make_contiguous());
	let c = ui.ctx().animate_value_with_time(
		Id::new(("gain-corr", cx.salt, cx.r.device)),
		raw,
		if v.synthetic { 0.0 } else { 0.15 },
	);
	let alive = liveness(v, now) > 0.0 && !v.points.is_empty();
	let resp = ui.interact(
		rect,
		Id::new(("gain-corr-hover", cx.salt, cx.r.device)),
		Sense::hover(),
	);
	resp.on_hover_text(tr(
		"Phase correlation: +1 mono, 0 wide, below 0 parts cancel in mono",
	));
	if !ui.is_rect_visible(rect) {
		return;
	}
	let p = ui.painter();
	let font = theme::mono(10.0);
	let l = p.text(
		rect.left_center(),
		Align2::LEFT_CENTER,
		"\u{2212}1",
		font.clone(),
		theme::TEXT_FAINT,
	);
	let value_w = 40.0;
	let r = p.text(
		pos2(rect.right() - value_w - 6.0, rect.center().y),
		Align2::RIGHT_CENTER,
		"+1",
		font.clone(),
		theme::TEXT_FAINT,
	);
	let bar = Rect::from_min_max(
		pos2(l.right() + 6.0, rect.center().y - 3.0),
		pos2(r.left() - 6.0, rect.center().y + 3.0),
	);
	p.rect_filled(bar, CornerRadius::same(2), theme::BG_RAISED);
	let mid = bar.center().x;
	let color = match gn::phase_of(c) {
		Phase::Fine => theme::METER_LOW,
		Phase::Wide => theme::WARNING,
		Phase::Cancelling => CORAL,
	};
	if alive {
		let end = mid + c * bar.width() * 0.5;
		p.rect_filled(
			Rect::from_x_y_ranges(mid.min(end)..=mid.max(end), bar.y_range()),
			CornerRadius::same(2),
			color,
		);
	}
	p.vline(
		mid,
		bar.top() - 2.0..=bar.bottom() + 2.0,
		Stroke::new(1.0, theme::BORDER_STRONG),
	);
	p.text(
		rect.right_center(),
		Align2::RIGHT_CENTER,
		if alive {
			gn::correlation_text(c)
		} else {
			"\u{2014}".to_owned()
		},
		font,
		if alive && c < 0.0 { color } else { theme::TEXT },
	);
}

fn toggles(ui: &mut Ui, state: &mut AppState, cx: &BodyCx, rect: Rect) {
	let values = values(state, cx.r);
	let mut row = ui.new_child(
		UiBuilder::new()
			.max_rect(rect)
			.layout(Layout::left_to_right(Align::Center)),
	);
	let labels = [
		(INVERT_L, values.invert_left, tr("\u{d8} Invert L")),
		(INVERT_R, values.invert_right, tr("\u{d8} Invert R")),
	]
	.map(|(id, on, name)| {
		let label = if on {
			format!("{name} \u{b7} on")
		} else {
			name.to_owned()
		};
		(id, label)
	});
	// Toggles are their label plus 18 px (kit `toggle`).
	let total: f32 = labels
		.iter()
		.map(|(_, l)| {
			row.painter()
				.layout_no_wrap(l.clone(), theme::medium(12.0), theme::TEXT)
				.size()
				.x + 18.0
		})
		.sum::<f32>()
		+ 8.0;
	row.add_space(((rect.width() - total) * 0.5).max(0.0));
	row.spacing_mut().item_spacing.x = 8.0;
	for (id, label) in labels {
		device_kit::param_toggle(&mut row, state, cx.r, id, &label, theme::WARNING)
			.on_hover_text(tr("Flip this channel's polarity"));
	}
}

// --- Dock (16f) ---------------------------------------------------------------------

const KNOBS: [KnobDef; 3] = [
	KnobDef::new(GAIN, mark("Gain"), KnobSize::XL).step(0.5),
	KnobDef::new(PAN, mark("Pan"), KnobSize::M),
	KnobDef::new(WIDTH, mark("Width"), KnobSize::M),
];

const WIDTH_HINT: &str = mark("Width 0 % = mono, 100 % = unchanged, 200 % = side doubled");

fn dock(ui: &mut Ui, state: &mut AppState, cx: &BodyCx, v: &mut GainView, now: f64) {
	let body = cx.rect;
	let width = body.width().min(NATURAL_WIDTH);
	let content = Rect::from_center_size(
		pos2(body.center().x, body.center().y),
		vec2(width, body.height()),
	);
	let top = Rect::from_min_max(
		content.min,
		pos2(content.right(), content.bottom() - FOOTER_H - 4.0),
	);
	let side = (top.height() - CORR_H - 6.0).min(top.width() * 0.45);
	let gonio = Rect::from_min_size(top.min, vec2(side, side));
	goniometer(ui, state, cx, v, gonio, now);
	let corr = Rect::from_min_size(pos2(gonio.left(), gonio.bottom() + 6.0), vec2(side, CORR_H));
	correlation_meter(ui, cx, v, corr, now);

	let col = Rect::from_min_max(pos2(gonio.right() + GAP, top.top()), top.max);
	let knob_h = devices::knob_height(KnobSize::XL);
	let block_h = knob_h + 18.0 + 24.0 + 14.0 + 16.0;
	let y0 = col.center().y - block_h * 0.5;
	let knobs = Rect::from_min_size(pos2(col.left(), y0), vec2(col.width(), knob_h));
	devices::knob_grid(ui, state, cx, knobs, &KNOBS, 3);
	let tog = Rect::from_min_size(
		pos2(col.left(), knobs.bottom() + 18.0),
		vec2(col.width(), 24.0),
	);
	toggles(ui, state, cx, tog);
	ui.painter().text(
		pos2(col.center().x, tog.bottom() + 22.0),
		Align2::CENTER_CENTER,
		crate::i18n::tr_str(WIDTH_HINT),
		theme::medium(11.0),
		theme::TEXT_FAINT,
	);
	devices::footer(
		ui,
		body,
		tr(
			"Correlation turns amber below 0 and coral below \u{2212}0.5. Drag the goniometer \
		 sideways for pan, up/down for width; M = mono.",
		),
	);
}

// --- Compact -------------------------------------------------------------------------

fn compact_side(width: f32) -> f32 {
	width.min(240.0)
}

/// How tall the compact form is at `width`.
pub fn compact_height(width: f32) -> f32 {
	compact_side(width)
		+ 6.0 + CORR_H
		+ 8.0 + devices::small_rows_height(width, KNOBS.len())
		+ 4.0 + 24.0
		+ 4.0
}

fn compact(ui: &mut Ui, state: &mut AppState, cx: &BodyCx, v: &mut GainView, now: f64, rect: Rect) {
	let side = compact_side(rect.width());
	let gonio = Rect::from_min_size(
		pos2(rect.center().x - side * 0.5, rect.top()),
		vec2(side, side),
	);
	goniometer(ui, state, cx, v, gonio, now);
	let corr = Rect::from_min_size(pos2(gonio.left(), gonio.bottom() + 6.0), vec2(side, CORR_H));
	correlation_meter(ui, cx, v, corr, now);
	let knobs: Vec<KnobDef> = KNOBS.iter().map(|k| k.sized(KnobSize::S)).collect();
	let rows_h = devices::small_rows_height(rect.width(), knobs.len());
	let k = Rect::from_min_size(
		pos2(rect.left(), corr.bottom() + 8.0),
		vec2(rect.width(), rows_h),
	);
	let cols = devices::small_columns(rect.width(), knobs.len());
	devices::knob_grid(ui, state, cx, k, &knobs, cols);
	let tog = Rect::from_min_size(
		pos2(rect.left(), k.bottom() + 4.0),
		vec2(rect.width(), 24.0),
	);
	toggles(ui, state, cx, tog);
}
