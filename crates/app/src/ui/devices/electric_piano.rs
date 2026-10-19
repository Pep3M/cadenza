//! The Electric Piano editor body (design `17 Instruments`, 17b): an instrument face
//! with few big controls. Left, the tone picture (the partials of a held C4, the bell
//! partials pale) over a Mellow ↔ Bright slider bound to brightness; centre, the face
//! card with Brightness / Bell / Decay as XL knobs and Velocity / Release / Gain small;
//! right, one note over three seconds under the stereo tremolo (left channel up, right
//! down, the LFO lines over both) with the tremolo knobs. The shared on-screen keyboard
//! runs along the bottom (A–K while the editor has the focus). Below 1000 px the card
//! takes the tremolo knobs and the two pictures stack beside it; below 560 px
//! everything stacks and scrolls. Compact (inspector): the
//! tone picture, the three main knobs, the tremolo picture and S knobs. The pure logic
//! lives in [`crate::state::electric_piano_editor`].

use egui::{
	Align, Align2, Color32, CornerRadius, CursorIcon, Id, Layout, Rect, Sense, Shape, Stroke,
	StrokeKind, Ui, UiBuilder, pos2, vec2,
};

use crate::state::AppState;
use crate::state::electric_piano_editor::{
	self as ep, BELL, BRIGHTNESS, DECAY, EpValues, GAIN, RELEASE, TREM_DEPTH, TREM_RATE, VEL_SENS,
};
use crate::theme;
use crate::ui::device_editor::BodyCx;
use crate::ui::device_kit::{self, KnobSize};
use crate::ui::devices::keyboard::{self, SynthKeys};
use crate::ui::devices::{self, KnobDef};

/// The instrument's own colours (17b): tine amber and the pale bell / LFO cream.
const AMBER: Color32 = Color32::from_rgb(0xe3, 0xb2, 0x4f);
const AMBER_DARK: Color32 = Color32::from_rgb(0x5a, 0x3f, 0x17);
const CREAM: Color32 = Color32::from_rgb(0xff, 0xf1, 0xc9);
const CARD_BG: Color32 = Color32::from_rgb(0x1a, 0x1b, 0x1f);
const CARD_RING: Color32 = Color32::from_rgb(0x2a, 0x2c, 0x32);
const GAP: f32 = 8.0;
/// One row from this width on; narrower docks put the card above the pictures.
const ROW_MIN: f32 = 1000.0;
/// Below this width the two pictures stack too.
const STACK_BELOW: f32 = 560.0;
const CARD_MIN_H: f32 = 270.0;
const PICTURE_MIN_H: f32 = 210.0;
const SUBTITLE: &str = "tine \u{b7} 1974 suitcase voicing";
const TONE_CAPTION: &str = "Bell lifts the 7th\u{2013}9th partials (pale bars): the tine \u{201c}ping\u{201d} on hard hits.";

fn values(state: &AppState, cx: &BodyCx) -> EpValues {
	EpValues::from_fn(|p| devices::shown(state, cx.r, cx.info, p.id))
}

/// The body; `keys` is the dock's keyboard state (`None`: the inspector's compact form).
pub fn electric_piano_body(
	ui: &mut Ui,
	state: &mut AppState,
	cx: &BodyCx,
	keys: Option<&mut SynthKeys>,
) {
	match keys {
		None => {
			let height = compact_height(cx.rect.width());
			devices::compact_scroll(ui, cx, height, |ui, rect| {
				compact(ui, state, cx, rect);
			});
		}
		Some(keys) => {
			keys.begin(ui, state, cx.r);
			dock(ui, state, cx, keys);
		}
	}
}

fn dock(ui: &mut Ui, state: &mut AppState, cx: &BodyCx, keys: &mut SynthKeys) {
	let rect = cx.rect;
	let now = ui.input(|i| i.time);
	let kb = Rect::from_min_max(
		pos2(rect.left(), rect.bottom() - keyboard::KEYS_H),
		rect.max,
	);
	let top = Rect::from_min_max(rect.min, pos2(rect.right(), kb.top() - 6.0));
	if rect.width() >= ROW_MIN {
		row(ui, state, cx, top);
	} else if rect.width() >= STACK_BELOW {
		// The card (with the tremolo knobs) beside the two pictures.
		let card_w = (top.width() * 0.52).max(340.0);
		let card = Rect::from_min_size(top.min, vec2(card_w, top.height()));
		face_card(ui, state, cx, card, true);
		let right = Rect::from_min_max(pos2(card.right() + GAP, top.top()), top.max);
		let half = (right.height() - GAP) * 0.5;
		let tone = Rect::from_min_size(right.min, vec2(right.width(), half));
		let trem = Rect::from_min_max(pos2(right.left(), tone.bottom() + GAP), right.max);
		tone_panel(ui, state, cx, tone, false);
		tremolo_picture(ui, state, cx, trem, true);
	} else {
		let mut inner = ui.new_child(
			UiBuilder::new()
				.max_rect(top)
				.layout(Layout::top_down(Align::Min)),
		);
		egui::ScrollArea::vertical()
			.id_salt(("electric-piano-stack", cx.salt, cx.r.device))
			.auto_shrink([false, false])
			.show(&mut inner, |ui| {
				let w = ui.available_width();
				let card_h = top.height().max(CARD_MIN_H);
				let (area, _) = ui.allocate_exact_size(
					vec2(w, card_h + 2.0 * (GAP + PICTURE_MIN_H)),
					Sense::hover(),
				);
				let card = Rect::from_min_size(area.min, vec2(w, card_h));
				face_card(ui, state, cx, card, false);
				let tone = Rect::from_min_size(
					pos2(area.left(), card.bottom() + GAP),
					vec2(w, PICTURE_MIN_H),
				);
				let trem = Rect::from_min_max(pos2(area.left(), tone.bottom() + GAP), area.max);
				tone_panel(ui, state, cx, tone, true);
				tremolo_panel(ui, state, cx, trem, KnobSize::S);
			});
	}
	ui.painter().hline(
		(rect.left() - 8.0)..=(rect.right() + 8.0),
		kb.top(),
		Stroke::new(1.0, theme::BORDER_SUBTLE),
	);
	keyboard::keyboard_row(ui, state, cx, keys, kb, now);
}

/// Tone | face card | tremolo, left to right.
fn row(ui: &mut Ui, state: &mut AppState, cx: &BodyCx, area: Rect) {
	let w = area.width();
	let tone_w = (w * 0.29).clamp(300.0, 560.0);
	let card_w = (w * 0.335).clamp(400.0, 640.0);
	let tone = Rect::from_min_size(area.min, vec2(tone_w, area.height()));
	let card = Rect::from_min_size(
		pos2(tone.right() + GAP, area.top()),
		vec2(card_w, area.height()),
	);
	let trem = Rect::from_min_max(pos2(card.right() + GAP, area.top()), area.max);
	tone_panel(ui, state, cx, tone, true);
	face_card(ui, state, cx, card, false);
	let knob = if area.height() >= 240.0 && w >= 1300.0 {
		KnobSize::M
	} else {
		KnobSize::S
	};
	tremolo_panel(ui, state, cx, trem, knob);
}

// --- Tone -----------------------------------------------------------------------------

/// The partials of a held C4 over the Mellow ↔ Bright slider (and the caption).
fn tone_panel(ui: &mut Ui, state: &mut AppState, cx: &BodyCx, rect: Rect, caption: bool) {
	let v = values(state, cx);
	device_kit::paint_display(ui.painter(), rect);
	let p = ui.painter().with_clip_rect(rect.shrink(1.0));
	p.text(
		rect.left_top() + vec2(8.0, 6.0),
		Align2::LEFT_TOP,
		"Tone \u{b7} partials of a held C4",
		theme::medium(10.5),
		theme::TEXT_FAINT,
	);
	let inner = rect.shrink2(vec2(16.0, 12.0));
	let caption_h = if caption { 22.0 } else { 0.0 };
	let slider_y = inner.bottom() - caption_h - 8.0;
	let bars = Rect::from_min_max(
		pos2(inner.left(), rect.top() + 28.0),
		pos2(inner.right(), slider_y - 16.0),
	);
	partial_bars(&p, bars, &v);
	slider(ui, state, cx, inner.left(), inner.right(), slider_y, &v);
	if caption {
		let r = Rect::from_min_max(pos2(inner.left(), inner.bottom() - caption_h), inner.max);
		let mut c = ui.new_child(
			UiBuilder::new()
				.max_rect(r)
				.layout(Layout::left_to_right(Align::Center)),
		);
		c.add(
			egui::Label::new(
				egui::RichText::new(TONE_CAPTION)
					.font(theme::medium(11.0))
					.color(theme::TEXT_FAINT),
			)
			.truncate(),
		);
	}
}

fn partial_bars(p: &egui::Painter, rect: Rect, v: &EpValues) {
	if rect.height() < 8.0 {
		return;
	}
	let parts = ep::partials(v.brightness, v.bell);
	let gap = if rect.width() > 300.0 { 5.0 } else { 3.0 };
	let bar_w = (rect.width() - gap * (ep::PARTIALS as f32 - 1.0)) / ep::PARTIALS as f32;
	for (i, part) in parts.iter().enumerate() {
		let x = rect.left() + i as f32 * (bar_w + gap);
		let h = (part.level * rect.height()).max(2.0);
		let bar = Rect::from_min_max(pos2(x, rect.bottom() - h), pos2(x + bar_w, rect.bottom()));
		let (color, alpha) = if part.bell {
			(CREAM, 0.9)
		} else {
			(AMBER, (1.0 - i as f32 * 0.03).max(0.35))
		};
		let round = CornerRadius {
			nw: 2,
			ne: 2,
			sw: 0,
			se: 0,
		};
		if part.bell {
			p.rect_filled(bar.expand(2.0), round, CREAM.gamma_multiply(0.08));
		}
		p.rect_filled(bar, round, color.gamma_multiply(alpha));
	}
	p.hline(
		rect.x_range(),
		rect.bottom() + 0.5,
		Stroke::new(1.0, theme::BORDER),
	);
}

/// The Mellow ↔ Bright gradient bound to brightness: press or drag anywhere on it =
/// preview, release = one step.
fn slider(
	ui: &mut Ui,
	state: &mut AppState,
	cx: &BodyCx,
	left: f32,
	right: f32,
	cy: f32,
	v: &EpValues,
) {
	let font = theme::medium(11.0);
	let mellow = ui.painter().text(
		pos2(left, cy),
		Align2::LEFT_CENTER,
		"Mellow",
		font.clone(),
		theme::TEXT_DIM,
	);
	let bright = ui.painter().text(
		pos2(right, cy),
		Align2::RIGHT_CENTER,
		"Bright",
		font,
		theme::TEXT_DIM,
	);
	let track = Rect::from_min_max(
		pos2(mellow.right() + 10.0, cy - 3.0),
		pos2(bright.left() - 10.0, cy + 3.0),
	);
	if track.width() < 20.0 {
		return;
	}
	let resp = ui
		.interact(
			track.expand2(vec2(4.0, 8.0)),
			Id::new(("ep-brightness-slider", cx.salt, cx.r.device)),
			Sense::click_and_drag(),
		)
		.on_hover_cursor(CursorIcon::ResizeHorizontal);
	resp.widget_info(|| {
		egui::WidgetInfo::labeled(egui::WidgetType::Slider, true, "Brightness slider")
	});
	let at = |x: f32| ep::slider_brightness((x - track.left()) / track.width());
	if (resp.is_pointer_button_down_on() || resp.dragged())
		&& let Some(pos) = ui.ctx().pointer_interact_pos()
	{
		state.preview_param(cx.r.device, BRIGHTNESS, at(pos.x));
	}
	if (resp.drag_stopped() || resp.clicked())
		&& let Some(pos) = resp.interact_pointer_pos()
	{
		state.commit_param(cx.r.track, cx.r.device, BRIGHTNESS, at(pos.x));
	}
	let resp = resp.on_hover_text("Brightness: drag from mellow to bright");
	let p = ui.painter();
	// The gradient as one mesh: dark amber -> amber -> cream.
	let mut mesh = egui::Mesh::default();
	let stops = [(0.0, AMBER_DARK), (0.5, AMBER), (1.0, CREAM)];
	for (i, (t, c)) in stops.iter().enumerate() {
		let x = track.left() + t * track.width();
		mesh.colored_vertex(pos2(x, track.top()), *c);
		mesh.colored_vertex(pos2(x, track.bottom()), *c);
		if i > 0 {
			let k = i as u32 * 2;
			mesh.add_triangle(k - 2, k - 1, k);
			mesh.add_triangle(k - 1, k, k + 1);
		}
	}
	p.add(Shape::mesh(mesh));
	let x = track.left() + ep::slider_pos(v.brightness) * track.width();
	let thumb = Rect::from_center_size(pos2(x, cy), vec2(3.0, 14.0));
	p.rect_filled(thumb.expand(2.0), 2.0, theme::DISPLAY_BG);
	p.rect_filled(
		thumb,
		1.0,
		if resp.hovered() || resp.dragged() {
			Color32::WHITE
		} else {
			Color32::from_rgb(0xf4, 0xf5, 0xf7)
		},
	);
	if resp.dragged() || resp.hovered() {
		device_kit::paint_value_tag(
			p,
			pos2(x, track.top() - 8.0),
			Align2::CENTER_BOTTOM,
			&format!("{:.0} %", v.brightness),
			theme::TEXT,
			ui.clip_rect(),
		);
	}
}

// --- Face card ------------------------------------------------------------------------

/// The face card; `tremolo` adds the tremolo knobs to its small row (when the tremolo
/// picture has no room for its own).
fn face_card(ui: &mut Ui, state: &mut AppState, cx: &BodyCx, rect: Rect, tremolo: bool) {
	let p = ui.painter();
	p.rect(
		rect,
		CornerRadius::same(theme::RADIUS_LG),
		CARD_BG,
		Stroke::new(1.0, CARD_RING),
		StrokeKind::Inside,
	);
	p.rect_filled(
		Rect::from_min_size(rect.min, vec2(rect.width(), 4.0)),
		CornerRadius {
			nw: theme::RADIUS_LG,
			ne: theme::RADIUS_LG,
			sw: 0,
			se: 0,
		},
		AMBER,
	);
	let title = p.text(
		pos2(rect.left() + 18.0, rect.top() + 13.0),
		Align2::LEFT_TOP,
		"Electric Piano",
		theme::semibold(20.0),
		theme::TEXT,
	);
	if title.right() + 10.0 + 180.0 < rect.right() {
		p.text(
			pos2(title.right() + 10.0, title.bottom() - 3.0),
			Align2::LEFT_BOTTOM,
			SUBTITLE,
			theme::medium(12.0),
			theme::TEXT_FAINT,
		);
	}
	let small_h = devices::knob_height(KnobSize::S) + 6.0;
	let divider = rect.bottom() - small_h;
	let big_area = Rect::from_min_max(
		pos2(rect.left() + 10.0, title.bottom()),
		pos2(rect.right() - 10.0, divider),
	);
	let big = [KnobSize::XL, KnobSize::L, KnobSize::M]
		.into_iter()
		.find(|s| {
			devices::knob_height(*s) <= big_area.height() && s.diameter() * 3.6 <= big_area.width()
		})
		.unwrap_or(KnobSize::S);
	devices::knob_grid(
		ui,
		state,
		cx,
		big_area,
		&[
			KnobDef::new(BRIGHTNESS, "Brightness", big).color(AMBER),
			KnobDef::new(BELL, "Bell", big).color(AMBER),
			KnobDef::new(DECAY, "Decay", big).color(AMBER),
		],
		3,
	);
	ui.painter().hline(
		(rect.left() + 1.0)..=(rect.right() - 1.0),
		divider,
		Stroke::new(1.0, theme::BORDER),
	);
	let s = KnobSize::S;
	let mut knobs = vec![
		KnobDef::new(VEL_SENS, "Velocity", s),
		KnobDef::new(RELEASE, "Release", s),
		KnobDef::new(GAIN, "Gain", s).step(0.5),
	];
	if tremolo {
		knobs.push(KnobDef::new(TREM_RATE, "Trem rate", s).color(CREAM));
		knobs.push(KnobDef::new(TREM_DEPTH, "Trem depth", s).color(CREAM));
	}
	let small_w = (rect.width() * 0.6)
		.max(knobs.len() as f32 * 64.0)
		.min(rect.width() - 8.0);
	let small = Rect::from_min_max(
		pos2(rect.center().x - small_w * 0.5, divider + 2.0),
		pos2(rect.center().x + small_w * 0.5, rect.bottom() - 2.0),
	);
	let cols = knobs.len();
	devices::knob_grid(ui, state, cx, small, &knobs, cols);
}

// --- Tremolo --------------------------------------------------------------------------

fn tremolo_panel(ui: &mut Ui, state: &mut AppState, cx: &BodyCx, rect: Rect, knob: KnobSize) {
	let knobs_h = devices::knob_height(knob) + 4.0;
	let display = Rect::from_min_max(rect.min, pos2(rect.right(), rect.bottom() - knobs_h - 4.0));
	tremolo_picture(ui, state, cx, display, true);
	let w = (rect.width() * 0.7).min(320.0);
	let knobs = Rect::from_min_max(
		pos2(rect.center().x - w * 0.5, rect.bottom() - knobs_h),
		pos2(rect.center().x + w * 0.5, rect.bottom()),
	);
	devices::knob_grid(
		ui,
		state,
		cx,
		knobs,
		&[
			KnobDef::new(TREM_RATE, "Tremolo rate", knob).color(CREAM),
			KnobDef::new(TREM_DEPTH, "Tremolo depth", knob).color(CREAM),
		],
		2,
	);
}

/// One note over three seconds: the left channel's envelope × tremolo above the centre
/// line, the right one's below, with each channel's LFO line along its edge.
fn tremolo_picture(ui: &Ui, state: &AppState, cx: &BodyCx, rect: Rect, labels: bool) {
	if !ui.is_rect_visible(rect) {
		return;
	}
	let v = values(state, cx);
	device_kit::paint_display(ui.painter(), rect);
	let p = ui.painter().with_clip_rect(rect.shrink(1.0));
	let head = if labels { 24.0 } else { 6.0 };
	let foot = if labels { 18.0 } else { 6.0 };
	let plot = Rect::from_min_max(
		pos2(rect.left() + 1.0, rect.top() + head),
		pos2(rect.right() - 1.0, rect.bottom() - foot),
	);
	let cy = plot.center().y;
	let half = plot.height() * 0.5;
	let x_at = |t: f32| plot.left() + t / ep::NOTE_SPAN * plot.width();
	// Second grid lines.
	for s in 1..ep::NOTE_SPAN as usize {
		let x = x_at(s as f32);
		p.vline(x, plot.y_range(), Stroke::new(1.0, device_kit::GRID_MINOR));
	}
	p.hline(plot.x_range(), cy, Stroke::new(1.0, device_kit::GRID_MAJOR));
	let n = (plot.width() as usize).clamp(120, 600);
	let mut top = Vec::with_capacity(n + 1);
	let mut bottom = Vec::with_capacity(n + 1);
	let mut lfo_l = Vec::with_capacity(n + 1);
	let mut lfo_r = Vec::with_capacity(n + 1);
	let lfo_span = (half * 0.2).min(36.0);
	for i in 0..=n {
		let t = i as f32 / n as f32 * ep::NOTE_SPAN;
		let env = ep::note_envelope(&v, t).sqrt();
		let [gl, gr] = ep::tremolo_gains(&v, t);
		let x = x_at(t);
		top.push(pos2(x, cy - env * gl * half * 0.76));
		bottom.push(pos2(x, cy + env * gr * half * 0.76));
		lfo_l.push(pos2(x, plot.top() + 2.0 + (1.0 - gl) * lfo_span));
		lfo_r.push(pos2(x, plot.bottom() - 2.0 - (1.0 - gr) * lfo_span));
	}
	p.add(device_kit::fill_to(&top, cy, AMBER.gamma_multiply(0.32)));
	p.add(device_kit::fill_to(&bottom, cy, AMBER.gamma_multiply(0.24)));
	theme::glow_line(&p, top, AMBER);
	p.add(Shape::line(
		bottom,
		Stroke::new(1.25, AMBER.gamma_multiply(0.7)),
	));
	if v.trem_depth > 0.5 {
		p.add(Shape::line(lfo_l, Stroke::new(1.5, CREAM)));
		p.add(Shape::line(
			lfo_r,
			Stroke::new(1.5, CREAM.gamma_multiply(0.5)),
		));
	}
	// The key goes up.
	let up = x_at(ep::NOTE_HOLD);
	p.add(Shape::dashed_line(
		&[pos2(up, plot.top()), pos2(up, plot.bottom())],
		Stroke::new(1.0, theme::TEXT_FAINT.gamma_multiply(0.6)),
		4.0,
		3.0,
	));
	if !labels {
		return;
	}
	p.text(
		pos2(up + 5.0, cy - 3.0),
		Align2::LEFT_BOTTOM,
		"key up",
		theme::medium(10.0),
		theme::TEXT_FAINT,
	);
	p.text(
		rect.left_top() + vec2(8.0, 6.0),
		Align2::LEFT_TOP,
		"Tremolo \u{b7} one note over 3 s \u{b7} line = LFO",
		theme::medium(10.5),
		theme::TEXT_FAINT,
	);
	p.text(
		pos2(rect.right() - 8.0, rect.top() + 6.0),
		Align2::RIGHT_TOP,
		ep::tremolo_label(&v),
		theme::mono(10.5),
		theme::TEXT_FAINT,
	);
	let last = ep::NOTE_SPAN as usize;
	for s in 0..=last {
		let (x, align) = match s {
			0 => (x_at(0.0) + 6.0, Align2::LEFT_BOTTOM),
			s if s == last => (x_at(s as f32) - 6.0, Align2::RIGHT_BOTTOM),
			s => (x_at(s as f32), Align2::CENTER_BOTTOM),
		};
		p.text(
			pos2(x, rect.bottom() - 3.0),
			align,
			format!("{s} s"),
			theme::mono(10.0),
			theme::TEXT_FAINT,
		);
	}
	for (label, y) in [("L", cy - 10.0), ("R", cy + 10.0)] {
		p.text(
			pos2(plot.right() - 8.0, y),
			Align2::RIGHT_CENTER,
			label,
			theme::mono_medium(10.0),
			theme::TEXT_FAINT,
		);
	}
}

// --- Compact (inspector) --------------------------------------------------------------

const COMPACT_TONE_H: f32 = 120.0;
const COMPACT_TREM_H: f32 = 96.0;

fn compact_knobs() -> [KnobDef; 5] {
	let s = KnobSize::S;
	[
		KnobDef::new(VEL_SENS, "Velocity", s),
		KnobDef::new(RELEASE, "Release", s),
		KnobDef::new(TREM_RATE, "Trem rate", s).color(CREAM),
		KnobDef::new(TREM_DEPTH, "Trem depth", s).color(CREAM),
		KnobDef::new(GAIN, "Gain", s).step(0.5),
	]
}

/// How tall the compact form is at `width`.
pub fn compact_height(width: f32) -> f32 {
	COMPACT_TONE_H
		+ GAP + devices::knob_height(KnobSize::M)
		+ GAP + COMPACT_TREM_H
		+ GAP + devices::small_rows_height(width, compact_knobs().len())
}

fn compact(ui: &mut Ui, state: &mut AppState, cx: &BodyCx, rect: Rect) {
	let tone = Rect::from_min_size(rect.min, vec2(rect.width(), COMPACT_TONE_H));
	tone_panel(ui, state, cx, tone, false);
	let knobs = Rect::from_min_size(
		pos2(rect.left(), tone.bottom() + GAP),
		vec2(rect.width(), devices::knob_height(KnobSize::M)),
	);
	let m = KnobSize::M;
	devices::knob_grid(
		ui,
		state,
		cx,
		knobs,
		&[
			KnobDef::new(BRIGHTNESS, "Brightness", m).color(AMBER),
			KnobDef::new(BELL, "Bell", m).color(AMBER),
			KnobDef::new(DECAY, "Decay", m).color(AMBER),
		],
		3,
	);
	let trem = Rect::from_min_size(
		pos2(rect.left(), knobs.bottom() + GAP),
		vec2(rect.width(), COMPACT_TREM_H),
	);
	tremolo_picture(ui, state, cx, trem, false);
	let knobs = compact_knobs();
	let cols = devices::small_columns(rect.width(), knobs.len());
	let k = Rect::from_min_max(pos2(rect.left(), trem.bottom() + GAP), rect.max);
	devices::knob_grid(ui, state, cx, k, &knobs, cols);
}
