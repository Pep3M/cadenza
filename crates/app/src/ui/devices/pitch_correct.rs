//! The Pitch Correct editor body: the live pitch display (the last 4 s of the sung pitch
//! faint, the corrected pitch in the accent, over semitone rows where the key and scale
//! notes are lit and the target note glows), the readout of the note now, the key strip
//! (click a note to make it the key; lit = in the scale), the scale grid, and Retune /
//! Humanize / Amount / Transpose / Reference. Compact: the same, smaller. The pure logic
//! lives in [`crate::state::pitch_correct_editor`].

use std::sync::{Arc, Mutex};

use cadenza_dsp::readings;
use egui::{
	Align2, Color32, CornerRadius, Id, Rect, Sense, Shape, Stroke, StrokeKind, Ui, UiBuilder, pos2,
	vec2,
};

use crate::i18n::{mark, tr, tr_str};
use crate::state::AppState;
use crate::state::pitch_correct_editor::{
	self as pc, AMOUNT, HUMANIZE, KEY, KEY_NAMES, PcValues, PitchPoint, PitchTrace, REFERENCE,
	RETUNE, SCALE, SCALE_LABELS, TRANSPOSE,
};
use crate::state::selection::DeviceRef;
use crate::theme;
use crate::ui::device_editor::BodyCx;
use crate::ui::device_kit::{self, KnobSize};
use crate::ui::devices::{self, FOOTER_H, KnobDef};

/// The design's natural width; wider docks centre it.
pub const NATURAL_WIDTH: f32 = 1500.0;
const SCALE_W: f32 = 390.0;
const KNOBS_W: f32 = 240.0;
const GAP: f32 = 12.0;
const READOUT_H: f32 = 26.0;
const KEYS_H: f32 = 40.0;
const SCALE_CELL: egui::Vec2 = vec2(124.0, 24.0);
const SCALE_COLS: usize = 3;
const LABEL_W: f32 = 34.0;
/// Range easing per frame.
const EASE: f32 = 0.12;

struct PcView {
	trace: PitchTrace,
	synthetic: bool,
}

type Shared = Arc<Mutex<PcView>>;

fn view_id(salt: &str, r: DeviceRef) -> Id {
	Id::new(("pitch-correct-editor", salt, r.device))
}

fn view(ctx: &egui::Context, id: Id) -> Shared {
	ctx.data_mut(|d| {
		d.get_temp_mut_or_insert_with(id, || {
			Arc::new(Mutex::new(PcView {
				trace: PitchTrace::default(),
				synthetic: false,
			}))
		})
		.clone()
	})
}

fn lock(view: &Shared) -> std::sync::MutexGuard<'_, PcView> {
	view.lock().unwrap_or_else(|e| e.into_inner())
}

/// Shows `points` as if the probe had delivered them (screenshots): times relative to
/// the newest point.
pub fn set_display(ctx: &egui::Context, salt: &str, r: DeviceRef, points: &[PitchPoint]) {
	let v = view(ctx, view_id(salt, r));
	let mut v = lock(&v);
	v.trace = PitchTrace::default();
	for p in points {
		v.trace.push(*p);
	}
	v.trace.range = None;
	v.synthetic = true;
}

fn values(state: &AppState, r: DeviceRef) -> PcValues {
	PcValues::from_fn(|p| {
		state
			.param_value(r.track, r.device, p.id)
			.unwrap_or(p.default)
	})
}

pub fn pitch_correct_body(ui: &mut Ui, state: &mut AppState, cx: &BodyCx) {
	let shared = view(ui.ctx(), view_id(cx.salt, cx.r));
	let mut v = lock(&shared);
	listen(ui, state, cx, &mut v);
	if cx.compact {
		let height = compact_height(cx.rect.width());
		devices::compact_scroll(ui, cx, height, |ui, rect| {
			compact(ui, state, cx, &mut v, rect);
		});
	} else {
		dock(ui, state, cx, &mut v);
	}
}

/// Samples the device's readings once per frame while it is probed and audio passes.
fn listen(ui: &Ui, state: &AppState, cx: &BodyCx, v: &mut PcView) {
	if v.synthetic || state.probe_target() != Some(cx.r.device) || cx.frames.is_empty() {
		return;
	}
	let Some(probe) = state.probe() else {
		return;
	};
	let read = |i| probe.reading(i).unwrap_or(-1.0);
	v.trace.push(PitchPoint::from_readings(
		ui.input(|i| i.time),
		read(readings::PITCH_DETECTED),
		read(readings::PITCH_OUTPUT),
		read(readings::PITCH_TARGET),
	));
	ui.ctx().request_repaint();
}

// --- Pitch display ---------------------------------------------------------------------

fn pitch_display(ui: &mut Ui, cx: &BodyCx, values: &PcValues, v: &mut PcView, rect: Rect) {
	let id = Id::new(("pitch-correct-display", cx.salt, cx.r.device));
	ui.interact(rect, id, Sense::hover());
	if !ui.is_rect_visible(rect) {
		return;
	}
	let (lo, hi) = v.trace.ease_range(if v.synthetic { 1.0 } else { EASE });
	let p = ui.painter_at(rect);
	p.rect_filled(
		rect,
		CornerRadius::same(theme::RADIUS_MD),
		theme::DISPLAY_BG,
	);
	let plot = Rect::from_min_max(pos2(rect.left() + LABEL_W, rect.top()), rect.max);
	let row_h = plot.height() / (hi - lo).max(1.0);
	let y_of = |pitch: f32| plot.bottom() - (pitch - lo) / (hi - lo).max(1.0) * plot.height();
	let target = v.trace.current().and_then(|c| c.target);
	let font = theme::mono(9.5);
	for n in lo.ceil() as i32..=hi.floor() as i32 {
		let pitch = n as f32;
		let pc = n.rem_euclid(12) as u8;
		let row = Rect::from_x_y_ranges(plot.x_range(), y_of(pitch + 0.5)..=y_of(pitch - 0.5));
		let lit = values.in_scale(pc);
		let fill = if target.is_some_and(|t| t.round() as i32 == n) {
			theme::ACCENT.gamma_multiply(0.22)
		} else if lit && pc == values.key && values.scale != 0 {
			Color32::from_white_alpha(14)
		} else if lit {
			Color32::from_white_alpha(7)
		} else {
			Color32::TRANSPARENT
		};
		p.rect_filled(row, 0.0, fill);
		if pc == 0 {
			p.hline(
				plot.x_range(),
				row.bottom(),
				Stroke::new(1.0, theme::BORDER_SUBTLE),
			);
		}
		if lit && row_h >= 10.0 {
			p.text(
				pos2(rect.left() + 4.0, row.center().y),
				Align2::LEFT_CENTER,
				crate::state::poly_synth_editor::note_name(n.clamp(0, 127) as u8),
				font.clone(),
				if pc == values.key {
					theme::TEXT
				} else {
					theme::TEXT_FAINT
				},
			);
		}
	}
	let Some(now) = v.trace.points.back().map(|p| p.time) else {
		hint(&p, plot);
		return;
	};
	if !v.trace.heard() {
		hint(&p, plot);
		return;
	}
	let x_of = |t: f64| plot.right() - ((now - t) / pc::TRACE_SECONDS) as f32 * plot.width();
	let line = |pick: fn(&PitchPoint) -> Option<f32>, stroke: Stroke| {
		let mut run: Vec<egui::Pos2> = Vec::new();
		let mut shapes = Vec::new();
		for point in &v.trace.points {
			match pick(point) {
				Some(pitch) if (lo..=hi).contains(&pitch) => {
					run.push(pos2(x_of(point.time), y_of(pitch)))
				}
				_ => {
					if run.len() > 1 {
						shapes.push(Shape::line(std::mem::take(&mut run), stroke));
					}
					run.clear();
				}
			}
		}
		if run.len() > 1 {
			shapes.push(Shape::line(run, stroke));
		}
		shapes
	};
	p.extend(line(|p| p.detected, Stroke::new(1.2, theme::TEXT_FAINT)));
	p.extend(line(|p| p.output, Stroke::new(2.0, cx.accent)));
	devices::legend(
		&p,
		pos2(plot.left() + 8.0, plot.top() + 10.0),
		&[
			(tr("Sung"), theme::TEXT_FAINT),
			(tr("Corrected"), cx.accent),
		],
	);
}

fn hint(p: &egui::Painter, plot: Rect) {
	p.text(
		plot.center(),
		Align2::CENTER_CENTER,
		tr("Play the track: the sung and the corrected pitch show here"),
		theme::medium(12.0),
		theme::TEXT_FAINT,
	);
}

// --- Readout, key strip, scale grid -----------------------------------------------------

fn readout(ui: &mut Ui, values: &PcValues, v: &PcView, rect: Rect) {
	let p = ui.painter_at(rect);
	let mut x = rect.left();
	let mut put = |text: String, color: Color32, font: egui::FontId| {
		let r = p.text(
			pos2(x, rect.center().y),
			Align2::LEFT_CENTER,
			text,
			font,
			color,
		);
		x = r.right() + 14.0;
	};
	match v.trace.current() {
		Some(now) => {
			put(
				pc::pitch_text(now.detected.unwrap_or_default()),
				theme::TEXT_DIM,
				theme::mono_medium(13.0),
			);
			if let Some(t) = now.target {
				put(
					format!(
						"\u{2192} {}",
						crate::state::poly_synth_editor::note_name(
							t.round().clamp(0.0, 127.0) as u8
						)
					),
					theme::TEXT,
					theme::mono_medium(13.0),
				);
			}
			if let Some(c) = pc::correction_cents(now) {
				put(
					format!("{} c", crate::i18n::num(c.round() as f64, 0)),
					theme::TEXT_FAINT,
					theme::mono(11.5),
				);
			}
		}
		None => put(
			"\u{2014}".to_owned(),
			theme::TEXT_FAINT,
			theme::mono_medium(13.0),
		),
	}
	p.text(
		rect.right_center(),
		Align2::RIGHT_CENTER,
		format!(
			"{} \u{b7} A4 {}",
			values.scale_text(),
			pc::reference_text(values.reference)
		),
		theme::medium(12.0),
		theme::TEXT_DIM,
	);
}

/// One octave of notes: click = key; lit = in the scale, the key outlined.
fn key_strip(ui: &mut Ui, state: &mut AppState, cx: &BodyCx, values: &PcValues, rect: Rect) {
	let w = rect.width() / 12.0;
	for (i, name) in KEY_NAMES.iter().enumerate() {
		let cell = Rect::from_min_size(rect.min + vec2(i as f32 * w, 0.0), vec2(w, rect.height()))
			.shrink(1.0);
		let id = Id::new(("pitch-correct-key", cx.salt, cx.r.device, i));
		let resp = ui
			.interact(cell, id, Sense::click())
			.on_hover_text(tr("Click to make this note the key"));
		resp.widget_info(|| {
			egui::WidgetInfo::selected(
				egui::WidgetType::SelectableLabel,
				true,
				i == values.key as usize,
				format!("{} {name}", tr("Key")),
			)
		});
		let black = matches!(i, 1 | 3 | 6 | 8 | 10);
		let lit = values.in_scale(i as u8);
		let is_key = i == values.key as usize;
		let fill = match (lit, black) {
			(true, _) if is_key && values.scale != 0 => cx.accent.gamma_multiply(0.55),
			(true, false) => theme::BG_RAISED,
			(true, true) => theme::BG_HOVER,
			(false, _) => theme::BG_DEEP,
		};
		let p = ui.painter();
		p.rect(
			cell,
			CornerRadius::same(theme::RADIUS),
			if resp.hovered() {
				fill.gamma_multiply(1.3)
			} else {
				fill
			},
			Stroke::new(
				1.0,
				if is_key {
					cx.accent
				} else {
					theme::BORDER_SUBTLE
				},
			),
			StrokeKind::Inside,
		);
		p.text(
			cell.center(),
			Align2::CENTER_CENTER,
			*name,
			theme::mono_medium(11.0),
			if lit { theme::TEXT } else { theme::TEXT_FAINT },
		);
		if resp.clicked() && !is_key {
			state.commit_param(cx.r.track, cx.r.device, KEY, i as f32);
		}
	}
}

fn scale_grid(ui: &mut Ui, state: &mut AppState, cx: &BodyCx, values: &PcValues, rect: Rect) {
	let mut child = ui.new_child(UiBuilder::new().max_rect(rect));
	let cols = ((rect.width() / SCALE_CELL.x).floor() as usize).clamp(2, SCALE_COLS);
	let cell = vec2((rect.width() - 4.0) / cols as f32, SCALE_CELL.y);
	let id = Id::new(("pitch-correct-scale", cx.salt, cx.r.device));
	if let Some(i) =
		device_kit::grid_selector(&mut child, id, &SCALE_LABELS, values.scale, cols, cell)
		&& i != values.scale
	{
		state.commit_param(cx.r.track, cx.r.device, SCALE, i as f32);
	}
}

fn scale_grid_height(width: f32) -> f32 {
	let cols = ((width / SCALE_CELL.x).floor() as usize).clamp(2, SCALE_COLS);
	SCALE_LABELS.len().div_ceil(cols) as f32 * SCALE_CELL.y + 4.0
}

fn caption(ui: &Ui, at: egui::Pos2, text: &str) {
	ui.painter().text(
		at,
		Align2::LEFT_BOTTOM,
		tr_str(text),
		theme::semibold(10.5),
		theme::TEXT_FAINT,
	);
}

// --- Dock --------------------------------------------------------------------------------

const KNOBS: [KnobDef; 5] = [
	KnobDef::new(RETUNE, mark("Retune"), KnobSize::M),
	KnobDef::new(HUMANIZE, mark("Humanize"), KnobSize::M),
	KnobDef::new(AMOUNT, mark("Amount"), KnobSize::M),
	KnobDef::new(TRANSPOSE, mark("Transpose"), KnobSize::M),
	KnobDef::new(REFERENCE, mark("Reference"), KnobSize::M).step(1.0),
];

fn dock(ui: &mut Ui, state: &mut AppState, cx: &BodyCx, v: &mut PcView) {
	let values = values(state, cx.r);
	let body = cx.rect;
	let width = body.width().min(NATURAL_WIDTH);
	let content = Rect::from_center_size(body.center(), vec2(width, body.height()));
	let area = Rect::from_min_max(
		content.min,
		pos2(content.right(), content.bottom() - FOOTER_H - 4.0),
	);
	readout(
		ui,
		&values,
		v,
		Rect::from_min_size(area.min, vec2(area.width(), READOUT_H)),
	);
	let below = Rect::from_min_max(pos2(area.left(), area.top() + READOUT_H + 4.0), area.max);
	// Display | key + scale | knobs; the display takes what is left.
	let knobs_w = KNOBS_W.min(below.width() * 0.25);
	let scale_w = SCALE_W.min(below.width() * 0.36);
	let knobs = Rect::from_min_max(pos2(below.right() - knobs_w, below.top()), below.max);
	let side = Rect::from_min_max(
		pos2(knobs.left() - GAP - scale_w, below.top()),
		pos2(knobs.left() - GAP, below.bottom()),
	);
	let display = Rect::from_min_max(below.min, pos2(side.left() - GAP, below.bottom()));
	pitch_display(ui, cx, &values, v, display);

	let mut y = side.top() + 12.0;
	caption(ui, pos2(side.left(), y), mark("KEY"));
	let keys = Rect::from_min_size(pos2(side.left(), y + 2.0), vec2(side.width(), KEYS_H));
	key_strip(ui, state, cx, &values, keys);
	y = keys.bottom() + 16.0;
	caption(ui, pos2(side.left(), y), mark("SCALE"));
	let grid = Rect::from_min_size(
		pos2(side.left(), y + 2.0),
		vec2(side.width(), scale_grid_height(side.width())),
	);
	scale_grid(ui, state, cx, &values, grid);

	let top_h = devices::knob_height(KnobSize::M);
	let rows_h = top_h + devices::knob_height(KnobSize::S);
	let y0 = knobs.top() + ((knobs.height() - rows_h) * 0.5).max(0.0);
	devices::knob_grid(
		ui,
		state,
		cx,
		Rect::from_min_size(pos2(knobs.left(), y0), vec2(knobs.width(), top_h)),
		&KNOBS[..2],
		2,
	);
	let small: Vec<KnobDef> = KNOBS[2..].iter().map(|k| k.sized(KnobSize::S)).collect();
	devices::knob_grid(
		ui,
		state,
		cx,
		Rect::from_min_size(
			pos2(knobs.left(), y0 + top_h),
			vec2(knobs.width(), devices::knob_height(KnobSize::S)),
		),
		&small,
		3,
	);
	devices::footer(
		ui,
		body,
		tr(
			"Retune 0 ms = the hard tuned effect. A singer sharp or flat throughout: set \
			 Reference to their tuning so notes are not pulled to the wrong neighbour.",
		),
	);
}

// --- Compact -----------------------------------------------------------------------------

const DISPLAY_COMPACT_H: f32 = 110.0;

/// How tall the compact form is at `width`.
pub fn compact_height(width: f32) -> f32 {
	READOUT_H
		+ DISPLAY_COMPACT_H
		+ 8.0 + 14.0
		+ KEYS_H
		+ 8.0 + 14.0
		+ scale_grid_height(width)
		+ 8.0 + devices::small_rows_height(width, KNOBS.len())
		+ 4.0
}

fn compact(ui: &mut Ui, state: &mut AppState, cx: &BodyCx, v: &mut PcView, rect: Rect) {
	let values = values(state, cx.r);
	let w = rect.width();
	let read = Rect::from_min_size(rect.min, vec2(w, READOUT_H));
	readout(ui, &values, v, read);
	let display = Rect::from_min_size(pos2(rect.left(), read.bottom()), vec2(w, DISPLAY_COMPACT_H));
	pitch_display(ui, cx, &values, v, display);
	let mut y = display.bottom() + 8.0 + 14.0;
	caption(ui, pos2(rect.left(), y), mark("KEY"));
	let keys = Rect::from_min_size(pos2(rect.left(), y), vec2(w, KEYS_H));
	key_strip(ui, state, cx, &values, keys);
	y = keys.bottom() + 8.0 + 14.0;
	caption(ui, pos2(rect.left(), y), mark("SCALE"));
	let grid = Rect::from_min_size(pos2(rect.left(), y), vec2(w, scale_grid_height(w)));
	scale_grid(ui, state, cx, &values, grid);
	let knobs: Vec<KnobDef> = KNOBS.iter().map(|k| k.sized(KnobSize::S)).collect();
	let k = Rect::from_min_size(
		pos2(rect.left(), grid.bottom() + 8.0),
		vec2(w, devices::small_rows_height(w, knobs.len())),
	);
	let cols = devices::small_columns(w, knobs.len());
	devices::knob_grid(ui, state, cx, k, &knobs, cols);
}
