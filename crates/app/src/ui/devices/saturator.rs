//! The Saturator editor body (design `16 Effects`, 16e): Tape / Tube / Hard clip cards
//! (each with its own curve; 1/2/3 pick one), the shaping curve at the current drive
//! (drag ⇕ for drive in 0.5 dB steps, double-click = 0 dB; mode changes morph it over
//! 150 ms), a before/after scope (a test sine, or the probe's audio when it passes) and
//! the harmonics a test sine gets from the curve. Compact: mode selector, the curve at
//! full width, harmonics, a row of S knobs. The pure logic lives in
//! [`crate::state::saturator_editor`].

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use cadenza_dsp::response::SaturatorMode as Mode;
use egui::{
	Align, Align2, Color32, CornerRadius, CursorIcon, Id, Layout, Pos2, Rect, Sense, Shape, Stroke,
	StrokeKind, Ui, UiBuilder, pos2, vec2,
};

use crate::state::AppState;
use crate::state::saturator_editor::{
	self as sat, DRIVE, HARMONICS, MIX, MODE, MODES, OUTPUT, SatValues, TONE,
};
use crate::state::selection::DeviceRef;
use crate::theme;
use crate::ui::device_editor::BodyCx;
use crate::ui::device_kit::{self, KnobSize};
use crate::ui::devices::{self, FOOTER_H, KnobDef};

const GAP: f32 = 8.0;
/// The cards draw their curve at this drive, so each mode's character shows.
const CARD_DRIVE: f32 = 12.0;
/// The drag handle sits on the curve at this input.
const HANDLE_X: f32 = 0.3;
/// Scope samples kept per side, and shown.
const SCOPE_KEEP: usize = 4096;
const SCOPE_SHOW: usize = 1200;
const LIVE_FOR: f64 = 0.3;
const SILENCE: f32 = 1e-5;

/// Tape orange, tube coral, hard clip pink (design 16e).
pub fn mode_color(mode: Mode) -> Color32 {
	match mode {
		Mode::Tape => theme::BAND_COLORS[1],
		Mode::Tube => theme::BAND_COLORS[0],
		Mode::Hard => theme::BAND_COLORS[7],
	}
}

fn light(c: Color32) -> Color32 {
	theme::mix(c, Color32::WHITE, 0.18)
}

#[derive(Default)]
struct SatView {
	/// The mode shown last frame, and a morph from the previous one (mode, start time).
	mode: Option<Mode>,
	morph: Option<(Mode, f64)>,
	drag: Option<(f32, f32)>,
	input: VecDeque<f32>,
	output: VecDeque<f32>,
	heard: f64,
}

type Shared = Arc<Mutex<SatView>>;

fn view_id(salt: &str, r: DeviceRef) -> Id {
	Id::new(("saturator-editor", salt, r.device))
}

fn view(ctx: &egui::Context, id: Id) -> Shared {
	ctx.data_mut(|d| {
		d.get_temp_mut_or_insert_with(id, || Arc::new(Mutex::new(SatView::default())))
			.clone()
	})
}

fn lock(view: &Shared) -> std::sync::MutexGuard<'_, SatView> {
	view.lock().unwrap_or_else(|e| e.into_inner())
}

fn values(state: &AppState, r: DeviceRef) -> SatValues {
	SatValues::from_fn(|p| {
		state
			.param_value(r.track, r.device, p.id)
			.unwrap_or(p.default)
	})
}

/// 1/2/3 pick Tape / Tube / Hard clip while the editor has the focus.
pub fn shortcuts(ctx: &egui::Context, state: &mut AppState, r: DeviceRef) {
	for (key, mode) in [
		(egui::Key::Num1, Mode::Tape),
		(egui::Key::Num2, Mode::Tube),
		(egui::Key::Num3, Mode::Hard),
	] {
		if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, key)) {
			pick_mode(state, r, mode);
		}
	}
}

fn pick_mode(state: &mut AppState, r: DeviceRef, mode: Mode) {
	if values(state, r).mode != mode {
		state.commit_param(r.track, r.device, MODE, sat::mode_value(mode));
	}
}

pub fn saturator_body(ui: &mut Ui, state: &mut AppState, cx: &BodyCx) {
	let shared = view(ui.ctx(), view_id(cx.salt, cx.r));
	let mut v = lock(&shared);
	let now = ui.input(|i| i.time);
	listen(cx, state, &mut v, now);
	let shown = values(state, cx.r);
	if v.mode != Some(shown.mode) {
		if let Some(old) = v.mode {
			v.morph = Some((old, now));
		}
		v.mode = Some(shown.mode);
	}
	if let Some((_, start)) = v.morph {
		if now - start >= sat::MORPH_SECONDS {
			v.morph = None;
		} else {
			ui.ctx().request_repaint();
		}
	}
	if cx.compact {
		let height = compact_height(cx.rect.width());
		devices::compact_scroll(ui, cx, height, |ui, rect| {
			compact(ui, state, cx, &mut v, now, rect);
		});
	} else {
		dock(ui, state, cx, &mut v, now);
	}
}

/// Keeps the probe's latest audio for the scope.
fn listen(cx: &BodyCx, state: &AppState, v: &mut SatView, now: f64) {
	if state.probe_target() != Some(cx.r.device) || cx.frames.is_empty() {
		return;
	}
	let loud = cx
		.frames
		.iter()
		.any(|f| f.input.iter().any(|s| s.abs() > SILENCE));
	if !loud {
		return;
	}
	v.heard = now;
	for f in cx.frames {
		for (buf, s) in [(&mut v.input, f.input), (&mut v.output, f.output)] {
			if buf.len() == SCOPE_KEEP {
				buf.pop_front();
			}
			buf.push_back((s[0] + s[1]) * 0.5);
		}
	}
}

/// The curve as shown now: mid-morph between the previous mode and the current one.
fn curve_fn(values: &SatValues, v: &SatView, now: f64) -> impl Fn(f32) -> f32 {
	let to = values.mode;
	let drive = values.drive;
	let (from, t) = match v.morph {
		Some((from, start)) => (from, sat::morph_progress(now - start)),
		None => (to, 1.0),
	};
	move |x| sat::morph_curve(from, to, t, drive, x)
}

// --- Mode cards ---------------------------------------------------------------------

fn paint_curve_in(
	p: &egui::Painter,
	rect: Rect,
	color: Color32,
	width: f32,
	f: &dyn Fn(f32) -> f32,
) {
	let n = (rect.width() / 2.0).clamp(24.0, 160.0) as usize;
	let pts: Vec<Pos2> = (0..=n)
		.map(|i| {
			let x = -1.0 + 2.0 * i as f32 / n as f32;
			pos2(
				rect.center().x + x * rect.width() * 0.5,
				rect.center().y - f(x).clamp(-1.0, 1.0) * rect.height() * 0.5,
			)
		})
		.collect();
	p.add(Shape::line(pts, Stroke::new(width, color)));
}

fn card(ui: &mut Ui, state: &mut AppState, cx: &BodyCx, rect: Rect, mode: Mode, selected: bool) {
	let name = sat::mode_name(mode);
	let resp = ui
		.interact(
			rect,
			Id::new((
				"saturator-card",
				cx.salt,
				cx.r.device,
				sat::mode_index(mode),
			)),
			Sense::click(),
		)
		.on_hover_cursor(CursorIcon::PointingHand);
	resp.widget_info(|| {
		egui::WidgetInfo::selected(egui::WidgetType::RadioButton, true, selected, name)
	});
	if resp.clicked() {
		pick_mode(state, cx.r, mode);
	}
	let resp = resp.on_hover_text(format!(
		"{name}: {} (key {})",
		sat::mode_description(mode),
		sat::mode_index(mode) + 1
	));
	if !ui.is_rect_visible(rect) {
		return;
	}
	let color = mode_color(mode);
	let p = ui.painter();
	let (fill, ring, width) = if selected {
		(theme::BG_HOVER, color, 1.5)
	} else if resp.hovered() {
		(theme::BG_WIDGET, theme::BORDER_STRONG, 1.0)
	} else {
		(theme::BG_RAISED, theme::BORDER, 1.0)
	};
	p.rect(
		rect,
		CornerRadius::same(theme::RADIUS_LG),
		fill,
		Stroke::new(width, ring),
		StrokeKind::Inside,
	);
	let side = (rect.height() - 14.0).clamp(28.0, 56.0);
	let pic = Rect::from_min_size(
		pos2(rect.left() + 10.0, rect.center().y - side * 0.5),
		vec2(side, side),
	);
	p.rect_filled(pic, CornerRadius::same(theme::RADIUS_MD), theme::DISPLAY_BG);
	let inner = pic.shrink(4.0);
	let pp = p.with_clip_rect(pic);
	pp.line_segment(
		[inner.left_bottom(), inner.right_top()],
		Stroke::new(1.0, theme::BORDER),
	);
	paint_curve_in(
		&pp,
		inner,
		if selected {
			color
		} else {
			color.gamma_multiply(0.75)
		},
		1.75,
		&|x| sat::shown_curve(mode, CARD_DRIVE, x),
	);
	let text_x = pic.right() + 10.0;
	let compact = rect.height() < 50.0;
	p.text(
		pos2(text_x, rect.center().y - if compact { 0.0 } else { 9.0 }),
		Align2::LEFT_CENTER,
		name,
		theme::semibold(13.0),
		if selected { color } else { theme::TEXT },
	);
	if !compact {
		let desc = p.layout(
			sat::mode_description(mode).to_owned(),
			theme::medium(11.0),
			theme::TEXT_DIM,
			(rect.right() - text_x - 8.0).max(40.0),
		);
		p.galley(pos2(text_x, rect.center().y + 1.0), desc, theme::TEXT_DIM);
	}
}

fn cards(ui: &mut Ui, state: &mut AppState, cx: &BodyCx, rect: Rect, current: Mode) {
	let h = (rect.height() - 2.0 * 6.0) / 3.0;
	for (i, mode) in MODES.into_iter().enumerate() {
		let r = Rect::from_min_size(
			pos2(rect.left(), rect.top() + i as f32 * (h + 6.0)),
			vec2(rect.width(), h),
		);
		card(ui, state, cx, r, mode, mode == current);
	}
}

// --- Shaping curve --------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn curve_display(
	ui: &mut Ui,
	state: &mut AppState,
	cx: &BodyCx,
	v: &mut SatView,
	rect: Rect,
	now: f64,
	compact: bool,
) {
	let r = cx.r;
	let values = values(state, r);
	let color = mode_color(values.mode);
	let resp = ui
		.interact(
			rect,
			Id::new(("saturator-curve", cx.salt, r.device)),
			Sense::click_and_drag(),
		)
		.on_hover_cursor(CursorIcon::ResizeVertical);
	resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Other, true, "Shaping curve"));
	if resp.drag_started() {
		v.drag = Some((values.drive, 0.0));
	}
	if resp.dragged()
		&& let Some((start, dy)) = v.drag.as_mut()
	{
		*dy += resp.drag_delta().y;
		state.preview_param(r.device, DRIVE, sat::drag_drive(*start, *dy));
	}
	if resp.drag_stopped()
		&& let Some((start, dy)) = v.drag.take()
	{
		devices::commit_changes(
			state,
			r,
			cx.info,
			&[(DRIVE, sat::drag_drive(start, dy))],
			|_| "Saturator drive".to_owned(),
		);
	}
	if resp.double_clicked() {
		devices::commit_changes(state, r, cx.info, &[sat::reset_drive()], |_| {
			"Reset saturator drive".to_owned()
		});
	}
	let resp = resp.on_hover_text("Drag up/down for drive (0.5 dB steps); double-click = 0 dB");
	if !ui.is_rect_visible(rect) {
		return;
	}
	let values = self::values(state, r);
	let p = ui.painter().with_clip_rect(rect);
	device_kit::paint_display(&p, rect);
	let inner = p.with_clip_rect(rect.shrink(1.0));
	let plot = rect.shrink(if compact { 10.0 } else { 14.0 });
	for f in [0.25, 0.5, 0.75] {
		let x = plot.left() + plot.width() * f;
		let y = plot.top() + plot.height() * f;
		inner.vline(x, rect.y_range(), Stroke::new(1.0, device_kit::GRID_MAJOR));
		inner.hline(rect.x_range(), y, Stroke::new(1.0, device_kit::GRID_MAJOR));
	}
	inner.extend(Shape::dashed_line(
		&[plot.left_bottom(), plot.right_top()],
		Stroke::new(1.0, theme::BG_HOVER),
		3.0,
		3.0,
	));
	if (values.drive - sat::REFERENCE_DRIVE).abs() > 0.25 {
		let mode = values.mode;
		let n = 80;
		let pts: Vec<Pos2> = (0..=n)
			.map(|i| {
				let x = -1.0 + 2.0 * i as f32 / n as f32;
				pos2(
					plot.center().x + x * plot.width() * 0.5,
					plot.center().y
						- sat::shown_curve(mode, sat::REFERENCE_DRIVE, x) * plot.height() * 0.5,
				)
			})
			.collect();
		inner.extend(Shape::dotted_line(
			&pts,
			color.gamma_multiply(0.45),
			4.0,
			1.0,
		));
	}
	let f = curve_fn(&values, v, now);
	let n = (plot.width() / 2.0).clamp(60.0, 200.0) as usize;
	let pts: Vec<Pos2> = (0..=n)
		.map(|i| {
			let x = -1.0 + 2.0 * i as f32 / n as f32;
			pos2(
				plot.center().x + x * plot.width() * 0.5,
				plot.center().y - f(x).clamp(-1.0, 1.0) * plot.height() * 0.5,
			)
		})
		.collect();
	inner.add(Shape::line(
		pts.clone(),
		Stroke::new(10.0, color.gamma_multiply(0.06)),
	));
	theme::glow_line(&inner, pts, light(color));
	let handle = pos2(
		plot.center().x + HANDLE_X * plot.width() * 0.5,
		plot.center().y - f(HANDLE_X) * plot.height() * 0.5,
	);
	let hot = resp.hovered() || v.drag.is_some();
	if hot {
		inner.circle_filled(handle, 13.0, color.gamma_multiply(0.2));
	}
	inner.circle_filled(handle, 7.0, color);
	inner.circle_stroke(handle, 7.0, Stroke::new(1.5, theme::DISPLAY_BG));

	let caption = theme::medium(if compact { 9.5 } else { 10.5 });
	p.text(
		rect.left_top() + vec2(9.0, 7.0),
		Align2::LEFT_TOP,
		"Shaping curve \u{b7} drag \u{21d5} for drive",
		caption,
		theme::TEXT_FAINT,
	);
	p.text(
		rect.right_bottom() - vec2(9.0, 7.0),
		Align2::RIGHT_BOTTOM,
		"dotted = 6 dB",
		theme::mono(if compact { 9.5 } else { 10.5 }),
		theme::TEXT_FAINT,
	);
	device_kit::paint_value_tag(
		&p,
		handle + vec2(-14.0, 12.0),
		Align2::RIGHT_TOP,
		&sat::drive_label(values.drive),
		if v.drag.is_some() {
			theme::TEXT
		} else {
			light(color)
		},
		rect.shrink2(vec2(0.0, 22.0)),
	);
}

// --- Scope and harmonics --------------------------------------------------------------

fn scope(ui: &mut Ui, v: &mut SatView, values: &SatValues, rect: Rect, now: f64) {
	if !ui.is_rect_visible(rect) {
		return;
	}
	let color = mode_color(values.mode);
	let p = ui.painter().with_clip_rect(rect);
	device_kit::paint_display(&p, rect);
	let inner = p.with_clip_rect(rect.shrink(1.0));
	let plot = Rect::from_min_max(
		pos2(rect.left() + 1.0, rect.top() + 22.0),
		pos2(rect.right() - 1.0, rect.bottom() - 8.0),
	);
	let cy = plot.center().y;
	let half = plot.height() * 0.5;
	inner.hline(plot.x_range(), cy, Stroke::new(1.0, theme::BG_HOVER));
	let live = now - v.heard < LIVE_FOR && v.input.len() > 16;
	let (before, after): (Vec<Pos2>, Vec<Pos2>) = if live {
		let input = v.input.make_contiguous().to_vec();
		let output = v.output.make_contiguous().to_vec();
		let len = SCOPE_SHOW.min(input.len());
		let start = sat::trigger(&input, len);
		let scale = |s: &[f32]| {
			let peak = s.iter().fold(0.0f32, |m, x| m.max(x.abs())).max(1e-4);
			sat::TEST_LEVEL / peak
		};
		let a = &input[start..start + len];
		let b = &output[start..(start + len).min(output.len())];
		let (sa, sb) = (scale(a), scale(b));
		let x = |i: usize| plot.left() + i as f32 / (len.max(2) - 1) as f32 * plot.width();
		(
			a.iter()
				.enumerate()
				.map(|(i, s)| pos2(x(i), cy - s * sa * half))
				.collect(),
			b.iter()
				.enumerate()
				.map(|(i, s)| pos2(x(i), cy - s * sb * half))
				.collect(),
		)
	} else {
		let f = curve_fn(values, v, now);
		let n = (plot.width() / 2.0).clamp(80.0, 400.0) as usize;
		(0..=n)
			.map(|i| {
				let t = i as f32 / n as f32;
				let x = sat::TEST_LEVEL * (std::f32::consts::TAU * 2.5 * t).sin();
				let px = plot.left() + t * plot.width();
				(
					pos2(px, cy - x * half),
					pos2(px, cy - f(x) * sat::TEST_LEVEL * half),
				)
			})
			.unzip()
	};
	inner.add(Shape::line(before, Stroke::new(1.25, theme::TEXT_FAINT)));
	theme::glow_line(&inner, after, light(color));
	devices::legend(
		&p,
		pos2(rect.left() + 9.0, rect.top() + 12.0),
		&[("Before", theme::TEXT_FAINT), ("After", light(color))],
	);
	p.text(
		pos2(rect.right() - 9.0, rect.top() + 12.0),
		Align2::RIGHT_CENTER,
		if live { "live" } else { "test sine" },
		theme::mono(10.0),
		theme::TEXT_FAINT,
	);
}

fn harmonic_color(i: usize) -> Color32 {
	if i == 0 {
		theme::TEXT_FAINT
	} else if sat::is_even(i) {
		theme::BAND_COLORS[1]
	} else {
		theme::BAND_COLORS[0]
	}
}

fn harmonics(ui: &mut Ui, cx: &BodyCx, values: &SatValues, rect: Rect, bare: bool) {
	let resp = ui.interact(
		rect,
		Id::new(("saturator-harmonics", cx.salt, cx.r.device)),
		Sense::hover(),
	);
	if !ui.is_rect_visible(rect) {
		return;
	}
	let db = sat::harmonics_db(&sat::harmonics(values.mode, values.drive));
	let p = ui.painter().with_clip_rect(rect);
	device_kit::paint_display(&p, rect);
	let (top, bottom, side) = if bare {
		(20.0, 16.0, 10.0)
	} else {
		(26.0, 20.0, 14.0)
	};
	let area = Rect::from_min_max(
		pos2(rect.left() + side, rect.top() + top),
		pos2(rect.right() - side, rect.bottom() - bottom),
	);
	let gap = if area.width() > 300.0 { 6.0 } else { 3.0 };
	let bar_w = (area.width() - gap * (HARMONICS - 1) as f32) / HARMONICS as f32;
	let hover = resp.hover_pos();
	let mut tip = None;
	for (i, d) in db.iter().enumerate() {
		let x = area.left() + i as f32 * (bar_w + gap);
		let h = sat::bar_height(*d) * area.height();
		let bar = Rect::from_min_max(pos2(x, area.bottom() - h), pos2(x + bar_w, area.bottom()));
		let color = harmonic_color(i);
		p.rect_filled(
			bar,
			CornerRadius {
				nw: 2,
				ne: 2,
				sw: 0,
				se: 0,
			},
			color.gamma_multiply(if i == 0 { 0.7 } else { 0.9 }),
		);
		let label = format!("H{}", i + 1);
		p.text(
			pos2(bar.center().x, area.bottom() + bottom * 0.5),
			Align2::CENTER_CENTER,
			&label,
			theme::mono(if bare { 8.5 } else { 10.0 }),
			theme::TEXT_FAINT,
		);
		if !bare && i > 0 && bar_w >= 30.0 && *d > -sat::HARMONIC_RANGE_DB + 4.0 {
			p.text(
				pos2(bar.center().x, bar.top() - 7.0),
				Align2::CENTER_CENTER,
				crate::state::device::minus(format!("{d:.0}")),
				theme::mono(9.0),
				theme::TEXT_DIM,
			);
		}
		if hover.is_some_and(|h| h.x >= x && h.x <= x + bar_w + gap) {
			tip = Some(if i == 0 {
				format!("{label}: the fundamental")
			} else if *d <= -sat::HARMONIC_RANGE_DB {
				format!("{label}: none")
			} else {
				format!("{label}: {:.1} dB below the fundamental", -d)
			});
		}
	}
	// Title with the swatches drawn (not glyphs): "Harmonics added · even ■ odd ■".
	let font = theme::medium(if bare { 9.5 } else { 10.5 });
	let y = rect.top() + if bare { 10.0 } else { 13.0 };
	let t = p.text(
		pos2(rect.left() + 9.0, y),
		Align2::LEFT_CENTER,
		"Harmonics added \u{b7} even",
		font.clone(),
		theme::TEXT_DIM,
	);
	let sw = |x: f32, c: Color32| {
		p.rect_filled(
			Rect::from_center_size(pos2(x, y), vec2(7.0, 7.0)),
			CornerRadius::same(1),
			c,
		);
	};
	sw(t.right() + 8.0, theme::BAND_COLORS[1]);
	let t2 = p.text(
		pos2(t.right() + 17.0, y),
		Align2::LEFT_CENTER,
		"odd",
		font,
		theme::TEXT_DIM,
	);
	sw(t2.right() + 8.0, theme::BAND_COLORS[0]);
	if let Some(tip) = tip {
		resp.on_hover_text(tip);
	}
}

// --- Dock (16e) ---------------------------------------------------------------------

fn knob_defs(mode: Mode, size: KnobSize) -> [KnobDef; 3] {
	[
		KnobDef::new(DRIVE, "Drive", KnobSize::XL)
			.color(mode_color(mode))
			.step(sat::DRIVE_STEP),
		KnobDef::new(TONE, "Tone", size),
		KnobDef::new(OUTPUT, "Output", size),
	]
}

fn dock(ui: &mut Ui, state: &mut AppState, cx: &BodyCx, v: &mut SatView, now: f64) {
	let rect = cx.rect;
	let values = values(state, cx.r);
	let top = Rect::from_min_max(rect.min, pos2(rect.right(), rect.bottom() - FOOTER_H - 4.0));
	let wide = rect.width() >= 1500.0;
	let cards_w = if rect.width() >= 1250.0 { 210.0 } else { 176.0 };
	let curve_w = top.height();
	let knobs_w = if wide { 440.0 } else { 170.0 };
	let scopes_w = (top.width() - cards_w - curve_w - knobs_w - GAP * 3.0).clamp(240.0, 960.0);
	let knobs_w = top.width() - cards_w - curve_w - scopes_w - GAP * 3.0;

	let cards_rect = top.with_max_x(top.left() + cards_w);
	cards(ui, state, cx, cards_rect, values.mode);
	let curve_rect = Rect::from_min_size(
		pos2(cards_rect.right() + GAP, top.top()),
		vec2(curve_w, top.height()),
	);
	curve_display(ui, state, cx, v, curve_rect, now, false);
	let col = Rect::from_min_size(
		pos2(curve_rect.right() + GAP, top.top()),
		vec2(scopes_w, top.height()),
	);
	let half = (col.height() - GAP) * 0.5;
	let values = self::values(state, cx.r);
	scope(ui, v, &values, col.with_max_y(col.top() + half), now);
	harmonics(ui, cx, &values, col.with_min_y(col.bottom() - half), false);
	let knobs = Rect::from_min_max(pos2(col.right() + GAP, top.top()), top.max);
	if wide || knobs_w >= 300.0 {
		devices::knob_grid(
			ui,
			state,
			cx,
			knobs,
			&knob_defs(values.mode, KnobSize::M),
			3,
		);
	} else {
		let [drive, tone, output] = knob_defs(values.mode, KnobSize::S);
		let split = knobs.top() + devices::knob_height(KnobSize::XL) + 8.0;
		devices::knob_grid(ui, state, cx, knobs.with_max_y(split), &[drive], 1);
		devices::knob_grid(ui, state, cx, knobs.with_min_y(split), &[tone, output], 2);
	}
	devices::footer(
		ui,
		rect,
		"Mode cards morph the curve over 150 ms (keys 1/2/3). Harmonics are computed from the \
		 shaping curve with a test sine, so they also show when the transport is stopped.",
	);
}

// --- Compact -------------------------------------------------------------------------

const SELECTOR_H: f32 = 28.0;
const HARMONICS_H: f32 = 64.0;

fn compact_curve_h(width: f32) -> f32 {
	(width * 0.55).clamp(140.0, 210.0)
}

const COMPACT_KNOBS: usize = 4;

/// How tall the compact form is at `width`.
pub fn compact_height(width: f32) -> f32 {
	SELECTOR_H
		+ compact_curve_h(width)
		+ 6.0 + HARMONICS_H
		+ 8.0 + devices::small_rows_height(width, COMPACT_KNOBS)
}

fn compact(ui: &mut Ui, state: &mut AppState, cx: &BodyCx, v: &mut SatView, now: f64, rect: Rect) {
	let values = values(state, cx.r);
	let sel = Rect::from_min_size(rect.min, vec2(rect.width(), SELECTOR_H));
	let mut row = ui.new_child(
		UiBuilder::new()
			.max_rect(sel)
			.layout(Layout::left_to_right(Align::Min)),
	);
	let labels: Vec<&str> = MODES.iter().map(|m| sat::mode_name(*m)).collect();
	if let Some(i) = device_kit::segmented(
		&mut row,
		Id::new(("saturator-modes", cx.salt, cx.r.device)),
		&labels,
		sat::mode_index(values.mode),
	) {
		pick_mode(state, cx.r, MODES[i]);
	}
	let curve = Rect::from_min_size(
		pos2(rect.left(), sel.bottom()),
		vec2(rect.width(), compact_curve_h(rect.width())),
	);
	curve_display(ui, state, cx, v, curve, now, true);
	let values = self::values(state, cx.r);
	let harm = Rect::from_min_size(
		pos2(rect.left(), curve.bottom() + 6.0),
		vec2(rect.width(), HARMONICS_H),
	);
	harmonics(ui, cx, &values, harm, true);
	let mut knobs: Vec<KnobDef> = knob_defs(values.mode, KnobSize::S)
		.into_iter()
		.map(|k| k.sized(KnobSize::S))
		.collect();
	knobs.push(KnobDef::new(MIX, "Mix", KnobSize::S));
	let cols = devices::small_columns(rect.width(), knobs.len());
	let k = Rect::from_min_max(pos2(rect.left(), harm.bottom() + 8.0), rect.max);
	devices::knob_grid(ui, state, cx, k, &knobs, cols);
}
