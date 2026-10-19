//! Custom-painted mixer widgets. Each returns a [`Gesture`] so the caller can preview a
//! value live while dragging and commit exactly one command when the drag ends.

use egui::{
	Align2, Color32, CornerRadius, CursorIcon, Id, Pos2, Rect, Response, Sense, Stroke, StrokeKind,
	Ui, Vec2, pos2, vec2,
};

use crate::state::meters::{FLOOR_DB, MeterLevels};
use crate::theme;

/// Fader range.
pub const FADER_MAX_DB: f32 = 6.0;
pub const FADER_MIN_DB: f32 = -96.0;

/// Outcome of one frame of a draggable control.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Gesture {
	Idle,
	/// Value while dragging (not undoable yet).
	Preview(f32),
	/// Final value: commit as one command.
	Commit(f32),
}

/// Fader position (0..1) for a dB value: fourth root of the gain, so the top of the
/// travel (where mixing happens) gets most of the resolution; 0 dB sits at ~84 %.
pub fn db_to_fader(db: f32) -> f32 {
	if db <= FADER_MIN_DB {
		return 0.0;
	}
	let g = 10f32.powf(db / 20.0) / 10f32.powf(FADER_MAX_DB / 20.0);
	g.clamp(0.0, 1.0).powf(0.25)
}

pub fn fader_to_db(pos: f32) -> f32 {
	let p = pos.clamp(0.0, 1.0);
	if p <= 0.0 {
		return FADER_MIN_DB;
	}
	(20.0 * (p.powi(4) * 10f32.powf(FADER_MAX_DB / 20.0)).log10()).clamp(FADER_MIN_DB, FADER_MAX_DB)
}

/// Meter position (0..1) of a dB level, linear in dB from [`FLOOR_DB`] to +6.
pub fn db_to_meter(db: f32) -> f32 {
	((db - FLOOR_DB) / (FADER_MAX_DB - FLOOR_DB)).clamp(0.0, 1.0)
}

pub fn format_db(db: f32) -> String {
	if db <= FADER_MIN_DB + 0.05 {
		"-inf".into()
	} else if db.abs() < 0.05 {
		"0.0".into()
	} else {
		format!("{db:+.1}")
	}
}

pub fn format_pan(pan: f32) -> String {
	let p = (pan * 100.0).round() as i32;
	match p {
		0 => "C".into(),
		p if p < 0 => format!("L{}", -p),
		p => format!("R{p}"),
	}
}

/// Relative drag on a value stored in egui memory while the gesture lasts.
fn drag_value(response: &Response, id: Id, start: f32, delta: f32) -> Gesture {
	let ctx = &response.ctx;
	if response.double_clicked() {
		return Gesture::Idle;
	}
	if response.drag_started() {
		ctx.data_mut(|d| d.insert_temp(id, start));
	}
	if response.dragged() {
		let current = ctx.data(|d| d.get_temp::<f32>(id)).unwrap_or(start) + delta;
		ctx.data_mut(|d| d.insert_temp(id, current));
		return Gesture::Preview(current);
	}
	if response.drag_stopped() {
		let v = ctx.data_mut(|d| d.remove_temp::<f32>(id)).unwrap_or(start);
		return Gesture::Commit(v);
	}
	Gesture::Idle
}

/// Wraps a standard value widget (`DragValue`, `Slider`) so a drag edits a temporary
/// copy and the change is committed once, when the drag ends (or immediately when typed).
pub fn edit_value(
	ui: &mut Ui,
	id: Id,
	current: f32,
	add: impl FnOnce(&mut Ui, &mut f32) -> Response,
) -> (Gesture, Response) {
	let mut v = ui.data(|d| d.get_temp::<f32>(id)).unwrap_or(current);
	let r = add(ui, &mut v);
	let gesture = if r.drag_stopped() {
		ui.data_mut(|d| d.remove_temp::<f32>(id));
		Gesture::Commit(v)
	} else if r.dragged() {
		ui.data_mut(|d| d.insert_temp(id, v));
		Gesture::Preview(v)
	} else if r.changed()
		&& (r.has_focus() || r.lost_focus() || r.clicked() || r.is_pointer_button_down_on())
	{
		// Only a user edit commits: a widget may also report a change when it rounds or
		// clamps a value shown from elsewhere (e.g. automation at the playhead).
		ui.data_mut(|d| d.remove_temp::<f32>(id));
		Gesture::Commit(v)
	} else {
		Gesture::Idle
	};
	(gesture, r)
}

/// Vertical fader. Drag (Shift: fine), double-click: 0 dB.
pub fn fader(ui: &mut Ui, id: Id, db: f32, size: Vec2) -> (Gesture, Response) {
	let (rect, response) = ui.allocate_exact_size(size, Sense::click_and_drag());
	let response = response.on_hover_cursor(CursorIcon::ResizeVertical);
	let travel = rect.height() - 18.0;
	let fine = if ui.input(|i| i.modifiers.shift) {
		0.2
	} else {
		1.0
	};
	let delta = -response.drag_delta().y / travel.max(1.0) * fine;
	let gesture = match drag_value(&response, id, db_to_fader(db), delta) {
		_ if response.double_clicked() => Gesture::Commit(0.0),
		Gesture::Preview(p) => Gesture::Preview(fader_to_db(p)),
		Gesture::Commit(p) => Gesture::Commit(fader_to_db(p)),
		Gesture::Idle => Gesture::Idle,
	};
	let shown = match gesture {
		Gesture::Preview(v) | Gesture::Commit(v) => v,
		Gesture::Idle => db,
	};
	if ui.is_rect_visible(rect) {
		let p = ui.painter();
		let groove = Rect::from_center_size(rect.center(), vec2(4.0, travel));
		p.rect_filled(groove, CornerRadius::same(2), theme::METER_BG);
		// Scale.
		for mark in [6.0, 0.0, -6.0, -12.0, -24.0, -48.0] {
			let y = groove.bottom() - db_to_fader(mark) * travel;
			let c = if mark == 0.0 {
				theme::TEXT_DIM
			} else {
				theme::TEXT_FAINT
			};
			p.hline(
				groove.left() - 7.0..=groove.left() - 3.0,
				y,
				Stroke::new(1.0, c),
			);
		}
		let y = groove.bottom() - db_to_fader(shown) * travel;
		let cap = Rect::from_center_size(pos2(rect.center().x, y), vec2(rect.width() - 6.0, 14.0));
		let active = response.dragged() || response.hovered();
		p.rect(
			cap,
			CornerRadius::same(2),
			if active {
				Color32::WHITE
			} else {
				theme::KEY_WHITE
			},
			Stroke::new(1.0, theme::BG_DEEP),
			StrokeKind::Outside,
		);
		p.hline(
			cap.left() + 3.0..=cap.right() - 3.0,
			cap.center().y,
			Stroke::new(
				1.5,
				if active {
					theme::ACCENT
				} else {
					theme::BG_DEEP
				},
			),
		);
	}
	(gesture, response)
}

/// Horizontal pan control (-1..1). Drag, double-click: center.
/// Pan as a small knob with its L/R readout beside it: drag up/right or down/left
/// (Shift: fine), double-click: center.
pub fn pan_knob(ui: &mut Ui, id: Id, pan: f32, size: Vec2) -> (Gesture, Response) {
	let (rect, response) = ui.allocate_exact_size(size, Sense::click_and_drag());
	let response = response.on_hover_cursor(CursorIcon::ResizeVertical);
	let fine = if ui.input(|i| i.modifiers.shift) {
		0.2
	} else {
		1.0
	};
	let d = response.drag_delta();
	let delta = (d.x - d.y) / 80.0 * fine;
	let gesture = match drag_value(&response, id, pan, delta) {
		_ if response.double_clicked() => Gesture::Commit(0.0),
		Gesture::Preview(p) => Gesture::Preview(p.clamp(-1.0, 1.0)),
		Gesture::Commit(p) => Gesture::Commit(p.clamp(-1.0, 1.0)),
		Gesture::Idle => Gesture::Idle,
	};
	let shown = match gesture {
		Gesture::Preview(v) | Gesture::Commit(v) => v,
		Gesture::Idle => pan,
	};
	if ui.is_rect_visible(rect) {
		let p = ui.painter();
		let active = response.dragged() || response.hovered();
		let radius = (rect.height() * 0.5 - 3.0).min(10.0);
		let center = pos2(rect.left() + radius + 4.0, rect.center().y);
		p.circle_filled(
			center,
			radius,
			if active {
				theme::BG_PRESSED
			} else {
				theme::BG_HOVER
			},
		);
		// 270° travel, the arc filling from the center.
		let start = std::f32::consts::PI * 0.75;
		let sweep = std::f32::consts::PI * 1.5;
		let pos = (shown + 1.0) * 0.5;
		let arc = |from: f32, to: f32, color: Color32| {
			let steps = 16;
			let pts: Vec<Pos2> = (0..=steps)
				.map(|i| {
					let a = start + sweep * (from + (to - from) * i as f32 / steps as f32);
					center + vec2(a.cos(), a.sin()) * (radius + 2.5)
				})
				.collect();
			p.add(egui::Shape::line(pts, Stroke::new(1.5, color)));
		};
		arc(0.0, 1.0, theme::BG_DEEP);
		arc(pos.min(0.5), pos.max(0.5), theme::ACCENT);
		let a = start + sweep * pos;
		p.line_segment(
			[
				center + vec2(a.cos(), a.sin()) * 2.0,
				center + vec2(a.cos(), a.sin()) * (radius - 1.5),
			],
			Stroke::new(1.5, if active { theme::ACCENT } else { theme::TEXT }),
		);
		p.text(
			pos2(center.x + radius + 8.0, rect.center().y),
			Align2::LEFT_CENTER,
			format_pan(shown),
			theme::mono(10.5),
			if active { theme::TEXT } else { theme::TEXT_DIM },
		);
	}
	(
		gesture,
		response.on_hover_text("Pan: drag (Shift: fine), double-click: center"),
	)
}

pub fn pan(ui: &mut Ui, id: Id, pan: f32, size: Vec2) -> (Gesture, Response) {
	let (rect, response) = ui.allocate_exact_size(size, Sense::click_and_drag());
	let response = response.on_hover_cursor(CursorIcon::ResizeHorizontal);
	let fine = if ui.input(|i| i.modifiers.shift) {
		0.2
	} else {
		1.0
	};
	let delta = response.drag_delta().x / (rect.width() * 0.5).max(1.0) * fine;
	let gesture = match drag_value(&response, id, pan, delta) {
		_ if response.double_clicked() => Gesture::Commit(0.0),
		Gesture::Preview(p) => Gesture::Preview(p.clamp(-1.0, 1.0)),
		Gesture::Commit(p) => Gesture::Commit(p.clamp(-1.0, 1.0)),
		Gesture::Idle => Gesture::Idle,
	};
	let shown = match gesture {
		Gesture::Preview(v) | Gesture::Commit(v) => v,
		Gesture::Idle => pan,
	};
	if ui.is_rect_visible(rect) {
		let p = ui.painter();
		p.rect_filled(rect, CornerRadius::same(2), theme::METER_BG);
		let cx = rect.center().x;
		let x = cx + shown * (rect.width() * 0.5 - 1.0);
		// Position as a bar along the bottom edge, so the value text stays readable.
		let bar = Rect::from_x_y_ranges(
			cx.min(x)..=cx.max(x).max(cx + 1.0),
			rect.bottom() - 3.0..=rect.bottom(),
		);
		p.rect_filled(bar, 0.0, theme::ACCENT);
		p.vline(
			cx,
			rect.bottom() - 3.0..=rect.bottom(),
			Stroke::new(1.0, theme::TEXT_FAINT),
		);
		p.text(
			rect.center() - vec2(0.0, 1.5),
			Align2::CENTER_CENTER,
			format_pan(shown),
			theme::small(),
			theme::TEXT,
		);
		if response.hovered() {
			p.rect_stroke(
				rect,
				CornerRadius::same(2),
				Stroke::new(1.0, theme::BORDER_STRONG),
				StrokeKind::Inside,
			);
		}
	}
	(gesture, response)
}

fn meter_color(db: f32) -> Color32 {
	if db >= -3.0 {
		theme::METER_HIGH
	} else if db >= -12.0 {
		theme::METER_MID
	} else {
		theme::METER_LOW
	}
}

/// Stereo level meter (RMS bar, lighter peak bar, hold line, clip lamp on top).
/// Returns true when the clip lamp was clicked.
pub fn meter(ui: &mut Ui, id: Id, levels: MeterLevels, size: Vec2) -> bool {
	let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
	let lamp = Rect::from_min_size(rect.min, vec2(rect.width(), 5.0));
	let clicked = ui.interact(lamp, id, Sense::click()).clicked();
	if ui.is_rect_visible(rect) {
		paint_meter(ui.painter(), rect, levels);
	}
	clicked
}

/// Paints a meter into `rect` (also used by the arrange-view track headers).
pub fn paint_meter(p: &egui::Painter, rect: Rect, levels: MeterLevels) {
	let lamp = Rect::from_min_size(rect.min, vec2(rect.width(), 5.0));
	p.rect_filled(
		lamp,
		CornerRadius::same(1),
		if levels.clipped {
			theme::METER_HIGH
		} else {
			theme::METER_BG
		},
	);
	let body = Rect::from_min_max(pos2(rect.left(), lamp.bottom() + 2.0), rect.max);
	p.rect_filled(body, 0.0, theme::METER_BG);
	let gap = 1.0;
	let w = (body.width() - gap) / 2.0;
	for c in 0..2 {
		let col = Rect::from_min_size(
			pos2(body.left() + c as f32 * (w + gap), body.top()),
			vec2(w, body.height()),
		);
		let y = |db: f32| col.bottom() - db_to_meter(db) * col.height();
		let peak = levels.peak_db[c];
		if peak > FLOOR_DB {
			let r = Rect::from_x_y_ranges(col.x_range(), y(peak)..=col.bottom());
			p.rect_filled(r, 0.0, meter_color(peak).gamma_multiply(0.45));
		}
		let rms = levels.rms_db[c];
		if rms > FLOOR_DB {
			// Colored by zone so loud passages read at a glance.
			for (lo, hi) in [(FLOOR_DB, -12.0), (-12.0, -3.0), (-3.0, FADER_MAX_DB)] {
				if rms <= lo {
					break;
				}
				let r = Rect::from_x_y_ranges(col.x_range(), y(rms.min(hi))..=y(lo));
				p.rect_filled(r, 0.0, meter_color(lo + 0.01));
			}
		}
		if levels.hold_db > FLOOR_DB {
			p.hline(
				col.x_range(),
				y(levels.hold_db),
				Stroke::new(1.0, meter_color(levels.hold_db)),
			);
		}
	}
	// 0 dB tick.
	let zero = body.bottom() - db_to_meter(0.0) * body.height();
	p.hline(
		body.x_range(),
		zero,
		Stroke::new(1.0, Color32::from_black_alpha(140)),
	);
}

/// Small square toggle (M / S / bypass) colored when on.
pub fn toggle(ui: &mut Ui, on: bool, label: &str, on_color: Color32, size: Vec2) -> Response {
	let (rect, response) = ui.allocate_exact_size(size, Sense::click());
	response.widget_info(|| {
		egui::WidgetInfo::selected(egui::WidgetType::Checkbox, ui.is_enabled(), on, label)
	});
	if ui.is_rect_visible(rect) {
		paint_toggle(ui.painter(), rect, on, label, on_color, response.hovered());
	}
	response
}

pub fn paint_toggle(
	p: &egui::Painter,
	rect: Rect,
	on: bool,
	label: &str,
	on_color: Color32,
	hovered: bool,
) {
	// On = a soft fill of the state color with the label in that color (Graphite `*-soft`).
	let (fill, text) = if on {
		(theme::mix(theme::BG_PANEL, on_color, 0.24), on_color)
	} else if hovered {
		(theme::BG_HOVER, theme::TEXT)
	} else {
		(theme::BG_WIDGET, theme::TEXT_DIM)
	};
	p.rect_filled(rect, CornerRadius::same(theme::RADIUS), fill);
	if on {
		p.rect_stroke(
			rect,
			CornerRadius::same(theme::RADIUS),
			egui::Stroke::new(1.0, on_color.gamma_multiply(0.45)),
			egui::StrokeKind::Inside,
		);
	}
	p.text(
		rect.center(),
		Align2::CENTER_CENTER,
		label,
		theme::semibold(10.5),
		text,
	);
}

/// Horizontal volume fader for compact places (track headers): same curve as the mixer
/// fader. Drag (Shift: fine), double-click: 0 dB.
pub fn volume_slider(
	ui: &mut Ui,
	id: Id,
	db: f32,
	rect: Rect,
	color: Color32,
) -> (Gesture, Response) {
	let response = ui
		.interact(rect, id, Sense::click_and_drag())
		.on_hover_cursor(CursorIcon::ResizeHorizontal);
	let fine = if ui.input(|i| i.modifiers.shift) {
		0.2
	} else {
		1.0
	};
	let travel = (rect.width() - 4.0).max(1.0);
	let delta = response.drag_delta().x / travel * fine;
	let gesture = match drag_value(&response, id, db_to_fader(db), delta) {
		_ if response.double_clicked() => Gesture::Commit(0.0),
		Gesture::Preview(p) => Gesture::Preview(fader_to_db(p)),
		Gesture::Commit(p) => Gesture::Commit(fader_to_db(p)),
		Gesture::Idle => Gesture::Idle,
	};
	let shown = match gesture {
		Gesture::Preview(v) | Gesture::Commit(v) => v,
		Gesture::Idle => db,
	};
	if ui.is_rect_visible(rect) {
		let p = ui.painter();
		let active = response.dragged() || response.hovered();
		let r = CornerRadius::same(theme::RADIUS);
		p.rect_filled(rect, r, theme::BG_DEEP);
		let x = rect.left() + 2.0 + db_to_fader(shown) * travel;
		p.rect_filled(
			Rect::from_min_max(rect.min, pos2(x, rect.bottom())),
			r,
			if active {
				theme::mix(theme::BG_HOVER, color, 0.3)
			} else {
				theme::BG_WIDGET
			},
		);
		let zero = rect.left() + 2.0 + db_to_fader(0.0) * travel;
		p.vline(
			zero,
			rect.bottom() - 3.0..=rect.bottom(),
			Stroke::new(1.0, theme::TEXT_FAINT),
		);
		// The unit goes before the value ever truncates.
		let value = format_db(shown);
		let text = if rect.width() >= 100.0 {
			format!("{value} dB")
		} else {
			value
		};
		p.text(
			pos2(rect.right() - 5.0, rect.center().y),
			Align2::RIGHT_CENTER,
			text,
			theme::mono(10.5),
			theme::TEXT,
		);
		if active {
			p.rect_stroke(
				rect,
				r,
				Stroke::new(1.0, theme::BORDER_STRONG),
				StrokeKind::Inside,
			);
		}
	}
	(gesture, response)
}

/// Thin themed scrollbar over `rect` for a view showing `visible` of `total` starting
/// at `offset` (any unit). Drag the thumb, click the track to page. Returns the new
/// offset when the user moved it.
pub fn scrollbar(
	ui: &mut Ui,
	id: Id,
	rect: Rect,
	vertical: bool,
	total: f64,
	visible: f64,
	offset: f64,
) -> Option<f64> {
	let response = ui.interact(rect, id, Sense::click_and_drag());
	let along = |p: Pos2| {
		if vertical {
			p.y - rect.top()
		} else {
			p.x - rect.left()
		}
	};
	let length = if vertical {
		rect.height()
	} else {
		rect.width()
	};
	let map = crate::state::timeline::Overview {
		total: total.max(visible).max(1e-9),
		width: length,
	};
	let (t0, t1) = map.thumb(offset, visible, 24.0);
	let thumb = if vertical {
		Rect::from_x_y_ranges(
			rect.left() + 2.0..=rect.right() - 2.0,
			rect.top() + t0..=rect.top() + t1,
		)
	} else {
		Rect::from_x_y_ranges(
			rect.left() + t0..=rect.left() + t1,
			rect.top() + 2.0..=rect.bottom() - 2.0,
		)
	};
	let grab_id = id.with("grab");
	let mut new = None;
	if let Some(pos) = response.interact_pointer_pos() {
		if response.drag_started() || response.is_pointer_button_down_on() && !response.dragged() {
			let grab = if thumb.contains(pos) {
				along(pos) - t0
			} else {
				// Clicked the track: center the thumb there.
				(t1 - t0) * 0.5
			};
			ui.data_mut(|d| d.insert_temp(grab_id, grab));
		}
		if let Some(grab) = ui.data(|d| d.get_temp::<f32>(grab_id)) {
			let v = map.offset_for_thumb(along(pos) - grab, visible);
			if (v - offset).abs() > 1e-9 {
				new = Some(v);
			}
		}
	}
	if !response.is_pointer_button_down_on() && !response.dragged() {
		ui.data_mut(|d| d.remove_temp::<f32>(grab_id));
	}
	if ui.is_rect_visible(rect) {
		let p = ui.painter();
		let active = response.dragged() || response.is_pointer_button_down_on();
		let fill = if active {
			theme::ACCENT_DIM
		} else if response.hovered() {
			theme::BG_HOVER
		} else {
			theme::BG_WIDGET
		};
		if visible < total - 1e-9 {
			p.rect_filled(thumb, CornerRadius::same(3), fill);
		}
	}
	new
}

pub use crate::state::device::KnobSpec;

/// FL-style rotary knob with its label under it and the value in a tooltip / while
/// dragging. Vertical drag (Shift: fine), double-click: default. One commit per drag.
pub fn knob(
	ui: &mut Ui,
	id: Id,
	label: &str,
	value: f32,
	spec: KnobSpec,
	format: impl Fn(f32) -> String,
) -> (Gesture, Response) {
	let size = vec2(46.0, 50.0);
	let (rect, response) = ui.allocate_exact_size(size, Sense::click_and_drag());
	let response = response.on_hover_cursor(CursorIcon::ResizeVertical);
	let fine = if ui.input(|i| i.modifiers.shift) {
		0.2
	} else {
		1.0
	};
	let delta = -response.drag_delta().y / 140.0 * fine;
	let gesture = match drag_value(&response, id, spec.to_pos(value), delta) {
		_ if response.double_clicked() => Gesture::Commit(spec.default),
		Gesture::Preview(p) => Gesture::Preview(spec.from_pos(p)),
		Gesture::Commit(p) => Gesture::Commit(spec.from_pos(p)),
		Gesture::Idle => Gesture::Idle,
	};
	let shown = match gesture {
		Gesture::Preview(v) | Gesture::Commit(v) => v,
		Gesture::Idle => value,
	};
	let text = format(shown);
	let response = response.on_hover_text(format!("{label}: {text}"));
	if ui.is_rect_visible(rect) {
		let p = ui.painter();
		let center = pos2(rect.center().x, rect.top() + 17.0);
		let radius = 14.0;
		let active = response.dragged() || response.hovered();
		p.circle_filled(
			center,
			radius,
			if active {
				theme::BG_HOVER
			} else {
				theme::BG_WIDGET
			},
		);
		p.circle_stroke(center, radius, Stroke::new(1.0, theme::BORDER_STRONG));
		// 270° travel from bottom-left to bottom-right; the arc shows the value.
		let start = std::f32::consts::PI * 0.75;
		let sweep = std::f32::consts::PI * 1.5;
		let pos = spec.to_pos(shown);
		let arc = |from: f32, to: f32, color: Color32| {
			let steps = 24;
			let pts: Vec<Pos2> = (0..=steps)
				.map(|i| {
					let a = start + sweep * (from + (to - from) * i as f32 / steps as f32);
					center + vec2(a.cos(), a.sin()) * (radius + 3.0)
				})
				.collect();
			p.add(egui::Shape::line(pts, Stroke::new(2.0, color)));
		};
		arc(0.0, 1.0, theme::METER_BG);
		// Bipolar knobs (pan, tune) fill from the center.
		let origin = if spec.min < 0.0 && spec.max > 0.0 {
			spec.to_pos(0.0)
		} else {
			0.0
		};
		arc(origin.min(pos), origin.max(pos), theme::ACCENT);
		let a = start + sweep * pos;
		p.line_segment(
			[
				center + vec2(a.cos(), a.sin()) * 4.0,
				center + vec2(a.cos(), a.sin()) * (radius - 2.0),
			],
			Stroke::new(2.0, if active { theme::ACCENT } else { theme::TEXT }),
		);
		let caption = if response.dragged() {
			text.as_str()
		} else {
			label
		};
		p.text(
			pos2(rect.center().x, rect.bottom() - 6.0),
			Align2::CENTER_CENTER,
			caption,
			egui::FontId::proportional(10.0),
			if response.dragged() {
				theme::TEXT
			} else {
				theme::TEXT_DIM
			},
		);
	}
	(gesture, response)
}

/// A disclosure triangle (pointing down when `open`, right when folded).
pub fn paint_disclosure(p: &egui::Painter, center: egui::Pos2, open: bool, color: Color32) {
	let s = 4.0;
	let pts = if open {
		vec![
			center + egui::vec2(-s, -s * 0.6),
			center + egui::vec2(s, -s * 0.6),
			center + egui::vec2(0.0, s * 0.7),
		]
	} else {
		vec![
			center + egui::vec2(-s * 0.6, -s),
			center + egui::vec2(-s * 0.6, s),
			center + egui::vec2(s * 0.7, 0.0),
		]
	};
	p.add(egui::Shape::convex_polygon(pts, color, egui::Stroke::NONE));
}

/// A panel or section title: 13 / 600 text with its shortcut key (if any) faint beside it.
pub fn panel_title(ui: &mut Ui, title: &str, key: Option<&str>) -> Response {
	ui.horizontal(|ui| {
		ui.spacing_mut().item_spacing.x = 6.0;
		ui.label(
			egui::RichText::new(title)
				.font(theme::heading())
				.color(theme::TEXT),
		);
		if let Some(key) = key {
			ui.label(
				egui::RichText::new(key)
					.font(theme::mono(10.5))
					.color(theme::TEXT_FAINT),
			);
		}
	})
	.response
}

/// Equal-width segmented control over the available width; returns the clicked segment.
pub fn segmented(ui: &mut Ui, labels: &[&str], selected: usize) -> Option<usize> {
	let width = ui.available_width();
	segmented_sized(ui, labels, selected, width)
}

/// [`segmented`] with a fixed total width.
pub fn segmented_sized(ui: &mut Ui, labels: &[&str], selected: usize, width: f32) -> Option<usize> {
	let height = 24.0;
	let (rect, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
	let p = ui.painter().clone();
	p.rect_filled(rect, CornerRadius::same(theme::RADIUS_MD), theme::BG_DEEP);
	let w = rect.width() / labels.len().max(1) as f32;
	let mut clicked = None;
	for (i, label) in labels.iter().enumerate() {
		let r = Rect::from_min_size(
			pos2(rect.left() + i as f32 * w, rect.top()),
			vec2(w, height),
		)
		.shrink(2.0);
		let resp = ui.interact(r, ui.id().with(("segment", *label)), Sense::click());
		let on = i == selected;
		resp.widget_info(|| {
			egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, on, *label)
		});
		let (bg, fg) = if on {
			(theme::BG_HOVER, theme::TEXT)
		} else if resp.hovered() {
			(theme::BG_WIDGET, theme::TEXT)
		} else {
			(theme::BG_DEEP, theme::TEXT_DIM)
		};
		p.rect_filled(r, CornerRadius::same(theme::RADIUS), bg);
		p.text(
			r.center(),
			Align2::CENTER_CENTER,
			*label,
			theme::medium(12.0),
			fg,
		);
		if resp.clicked() {
			clicked = Some(i);
		}
	}
	clicked
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn fader_law_round_trips_and_spreads_the_top() {
		for db in [-60.0f32, -24.0, -6.0, 0.0, 3.0, 6.0] {
			let back = fader_to_db(db_to_fader(db));
			assert!((back - db).abs() < 1e-3, "{db} -> {back}");
		}
		assert_eq!(db_to_fader(FADER_MIN_DB), 0.0);
		assert_eq!(db_to_fader(6.0), 1.0);
		assert_eq!(fader_to_db(0.0), FADER_MIN_DB);
		let zero = db_to_fader(0.0);
		assert!(zero > 0.8 && zero < 0.86, "{zero}");
		// The mixing range (0 to -12 dB) gets a quarter of the travel.
		assert!(db_to_fader(0.0) - db_to_fader(-12.0) > 0.24);
	}

	#[test]
	fn labels() {
		assert_eq!(format_db(0.01), "0.0");
		assert_eq!(format_db(-3.26), "-3.3");
		assert_eq!(format_db(FADER_MIN_DB), "-inf");
		assert_eq!(format_db(4.0), "+4.0");
		assert_eq!(format_pan(0.0), "C");
		assert_eq!(format_pan(-0.25), "L25");
		assert_eq!(format_pan(1.0), "R100");
		assert_eq!(db_to_meter(FLOOR_DB), 0.0);
		assert_eq!(db_to_meter(6.0), 1.0);
	}
}
