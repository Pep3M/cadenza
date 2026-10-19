//! The Poly Synth editor body (design `17 Instruments`, 17a), laid out left to right as
//! the signal flows: OSC 1 + OSC 2 + NOISE (wave cards, osc 2 semitones / cents dragged
//! ⇕, unison voices, detune / mix / noise) → FILTER (12 / 24 dB, the exact response with
//! one node: ⇔ cutoff, ⇕ resonance, wheel = env amount, dashed = the env peak) → AMP and
//! FILTER ENVELOPE (kit ADSR editors) → OUTPUT (trigger-aligned scope of the probe,
//! velocity / glide / gain), over an on-screen keyboard (A–K on the computer keyboard
//! while the editor has the focus, Z / X octave). Below 1000 px the sections wrap into
//! two rows, below 560 px they stack; both scroll. Compact (inspector): wave cards, the filter display, both envelopes small and
//! S knobs. The pure logic lives in [`crate::state::poly_synth_editor`].

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use egui::{
	Align, Align2, Color32, CornerRadius, CursorIcon, Id, Layout, Pos2, Rect, Sense, Shape, Stroke,
	StrokeKind, Ui, UiBuilder, pos2, vec2,
};

use crate::state::AppState;
use crate::state::device_display::{self, AdsrStage, DbRange, VoicePos};
use crate::state::poly_synth_editor::{
	self as ps, CUTOFF, ENV_AMOUNT, Env, GAIN, GLIDE, KEY_TRACK, NOISE, NodeStart, OSC_MIX,
	OSC2_CENTS, OSC2_SEMI, RESONANCE, SLOPE, UNISON, UNISON_DETUNE, VEL_SENS, WAVE_NAMES,
	WAVE_PARAMS,
};
use crate::state::selection::DeviceRef;
use crate::theme;
use crate::ui::device_editor::BodyCx;
use crate::ui::device_kit::{
	self, ControlInfo, FreqDisplay, FreqEvent, FreqNode, KnobSize, ReadoutLook, ReadoutState,
	ValueFmt, Wave,
};
pub use crate::ui::devices::keyboard::SynthKeys;
use crate::ui::devices::{self, KnobDef, keyboard};
use crate::ui::widgets::Gesture;

const GAP: f32 = 8.0;
const TITLE_H: f32 = 22.0;
/// Below this width the sections stack vertically (responsive rule 5).
pub const STACK_BELOW: f32 = 560.0;
/// The single row needs this much; narrower docks wrap it into two rows.
const ROW_MIN: f32 = 1000.0;
/// Below this width the knobs shrink and the arrows narrow.
const SMALL_BELOW: f32 = 1100.0;
/// Scope: the probe's output, every `DECIMATE`th sample (25 ms on 300 points at 48 kHz).
const DECIMATE: usize = 4;
const SCOPE_KEEP: usize = 2048;
const LIVE_FOR: f64 = 0.3;
const SILENCE: f32 = 1e-5;
const WHEEL_REST: f64 = 0.5;

const VIOLET: Color32 = theme::BAND_COLORS[6];
const VIOLET_LIGHT: Color32 = Color32::from_rgb(0xc3, 0xb8, 0xff);
const AMP_GREEN: Color32 = theme::PLAYING;
const SCOPE_GREEN: Color32 = theme::BAND_COLORS[3];
const SCOPE_LIGHT: Color32 = Color32::from_rgb(0xa6, 0xf0, 0xa3);
const ARROW: Color32 = Color32::from_rgb(0x3c, 0x3f, 0x46);
const CELL_BG: Color32 = Color32::from_rgb(0x11, 0x12, 0x15);

fn env_color(env: Env) -> Color32 {
	match env {
		Env::Amp => AMP_GREEN,
		Env::Filter => VIOLET,
	}
}

fn wave(i: usize) -> Wave {
	[Wave::Sine, Wave::Saw, Wave::Square, Wave::Triangle][i.min(3)]
}

// --- View state (egui memory) --------------------------------------------------------

#[derive(Debug, Clone, Copy)]
struct WheelEdit {
	start: f32,
	notches: f32,
	value: f32,
	last: f64,
}

#[derive(Default)]
struct SynthView {
	scope: VecDeque<f32>,
	decimate: usize,
	heard: f64,
	/// Test frames: the scope stays live without a probe.
	synthetic: bool,
	node: Option<NodeStart>,
	tune: Option<(&'static str, f32, f32)>,
	wheel: Option<WheelEdit>,
}

type Shared = Arc<Mutex<SynthView>>;

fn view_id(salt: &str, r: DeviceRef) -> Id {
	Id::new(("poly-synth-editor", salt, r.device))
}

fn view(ctx: &egui::Context, id: Id) -> Shared {
	ctx.data_mut(|d| {
		d.get_temp_mut_or_insert_with(id, || Arc::new(Mutex::new(SynthView::default())))
			.clone()
	})
}

fn lock(view: &Shared) -> std::sync::MutexGuard<'_, SynthView> {
	view.lock().unwrap_or_else(|e| e.into_inner())
}

/// Feeds the scope as if the probe had heard `samples` (mono, at the probe's rate):
/// screenshots and tests.
pub fn set_scope(ctx: &egui::Context, salt: &str, r: DeviceRef, samples: &[f32]) {
	let v = view(ctx, view_id(salt, r));
	let mut v = lock(&v);
	v.scope = samples.iter().step_by(DECIMATE).copied().collect();
	while v.scope.len() > SCOPE_KEEP {
		v.scope.pop_front();
	}
	v.synthetic = true;
}

fn listen(cx: &BodyCx, state: &AppState, v: &mut SynthView, now: f64) {
	if state.probe_target() != Some(cx.r.device) || cx.frames.is_empty() {
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
	v.synthetic = false;
	for f in cx.frames {
		v.decimate = (v.decimate + 1) % DECIMATE;
		if v.decimate != 0 {
			continue;
		}
		if v.scope.len() == SCOPE_KEEP {
			v.scope.pop_front();
		}
		v.scope.push_back((f.output[0] + f.output[1]) * 0.5);
	}
}

fn value(state: &AppState, cx: &BodyCx, id: &str) -> f32 {
	devices::shown(state, cx.r, cx.info, id)
}

// --- Body ------------------------------------------------------------------------------

/// The body; `keys` is the dock's keyboard state (`None`: the inspector's compact form).
pub fn poly_synth_body(
	ui: &mut Ui,
	state: &mut AppState,
	cx: &BodyCx,
	keys: Option<&mut SynthKeys>,
) {
	let shared = view(ui.ctx(), view_id(cx.salt, cx.r));
	let mut v = lock(&shared);
	let now = ui.input(|i| i.time);
	listen(cx, state, &mut v, now);
	finish_wheel(ui, state, cx, &mut v);
	match keys {
		None => {
			let height = compact_height(cx.rect.width());
			devices::compact_scroll(ui, cx, height, |ui, rect| {
				compact(ui, state, cx, &mut v, rect);
			});
		}
		Some(keys) => {
			keys.begin(ui, state, cx.r);
			dock(ui, state, cx, &mut v, keys, now);
		}
	}
}

fn dock(
	ui: &mut Ui,
	state: &mut AppState,
	cx: &BodyCx,
	v: &mut SynthView,
	keys: &mut SynthKeys,
	now: f64,
) {
	let rect = cx.rect;
	let kb = Rect::from_min_max(
		pos2(rect.left(), rect.bottom() - keyboard::KEYS_H),
		rect.max,
	);
	let top = Rect::from_min_max(rect.min, pos2(rect.right(), kb.top() - 6.0));
	let voices = keys.voices(now);
	if rect.width() < STACK_BELOW {
		let mut inner = ui.new_child(
			UiBuilder::new()
				.max_rect(top)
				.layout(Layout::top_down(Align::Min)),
		);
		egui::ScrollArea::vertical()
			.id_salt(("poly-synth-stack", cx.salt, cx.r.device))
			.auto_shrink([false, false])
			.show(&mut inner, |ui| {
				let w = ui.available_width();
				let (area, _) = ui.allocate_exact_size(vec2(w, stacked_height()), Sense::hover());
				stacked(ui, state, cx, v, &voices, area, now);
			});
	} else if rect.width() < ROW_MIN {
		let mut inner = ui.new_child(
			UiBuilder::new()
				.max_rect(top)
				.layout(Layout::top_down(Align::Min)),
		);
		egui::ScrollArea::vertical()
			.id_salt(("poly-synth-wrap", cx.salt, cx.r.device))
			.auto_shrink([false, false])
			.show(&mut inner, |ui| {
				let w = ui.available_width();
				let row_h = top.height().max(WRAP_ROW_MIN);
				let (area, _) = ui.allocate_exact_size(vec2(w, 2.0 * row_h + 18.0), Sense::hover());
				wrapped(ui, state, cx, v, &voices, area, row_h, now);
			});
	} else {
		row(ui, state, cx, v, &voices, top, now);
	}
	ui.painter().hline(
		(rect.left() - 8.0)..=(rect.right() + 8.0),
		kb.top(),
		Stroke::new(1.0, theme::BORDER_SUBTLE),
	);
	keyboard::keyboard_row(ui, state, cx, keys, kb, now);
}

/// The sections left to right with → between them.
fn row(
	ui: &mut Ui,
	state: &mut AppState,
	cx: &BodyCx,
	v: &mut SynthView,
	voices: &[VoicePos],
	area: Rect,
	now: f64,
) {
	let small = area.width() < SMALL_BELOW;
	let arrow = if small { 18.0 } else { 28.0 };
	let osc_w = (area.width() * 0.225).clamp(344.0, 430.0);
	let rest = area.width() - osc_w - 3.0 * arrow;
	let filter_w = rest * 0.33;
	let env_w = rest * 0.43;
	let out_w = rest - filter_w - env_w;
	let knob = KnobSize::S;
	let display_h = (area.height() - TITLE_H - 2.0 * GAP - devices::knob_height(knob)).max(80.0);
	let mut x = area.left();
	let mut next = |w: f32| {
		let r = Rect::from_min_size(pos2(x, area.top()), vec2(w, area.height()));
		x += w;
		r
	};
	let osc = next(osc_w);
	let a1 = next(arrow);
	let filter = next(filter_w);
	let a2 = next(arrow);
	let env = next(env_w);
	let a3 = next(arrow);
	let out = next(out_w);
	for a in [a1, a2, a3] {
		arrow_at(
			ui,
			pos2(a.center().x, area.top() + TITLE_H + GAP + display_h * 0.5),
		);
	}
	osc_section(ui, state, cx, v, osc, small);
	filter_section(ui, state, cx, v, filter, display_h, knob);
	envelopes(ui, state, cx, env, display_h, voices);
	let knob = output_knob(small, area.height(), display_h);
	output_section(ui, state, cx, v, out, display_h * 0.7, knob, now);
}

fn arrow_at(ui: &Ui, center: Pos2) {
	ui.painter().text(
		center,
		Align2::CENTER_CENTER,
		"\u{2192}",
		egui::FontId::proportional(18.0),
		ARROW,
	);
}

/// AMP and FILTER ENVELOPE side by side in `rect`.
#[allow(clippy::too_many_arguments)]
fn envelopes(
	ui: &mut Ui,
	state: &mut AppState,
	cx: &BodyCx,
	rect: Rect,
	display_h: f32,
	voices: &[VoicePos],
) {
	let gap = if rect.width() > 500.0 { 16.0 } else { 12.0 };
	let col = (rect.width() - gap) * 0.5;
	for (i, e) in Env::ALL.into_iter().enumerate() {
		let r = Rect::from_min_size(
			pos2(rect.left() + i as f32 * (col + gap), rect.top()),
			vec2(col, rect.height()),
		);
		env_section(ui, state, cx, e, r, display_h, voices);
	}
}

fn output_knob(small: bool, height: f32, display_h: f32) -> KnobSize {
	if small || height - TITLE_H - display_h * 0.7 < devices::knob_height(KnobSize::M) + 2.0 * GAP {
		KnobSize::S
	} else {
		KnobSize::M
	}
}

/// Rows never get shorter than this when the sections wrap.
const WRAP_ROW_MIN: f32 = 240.0;

/// Mid-width docks: OSC → FILTER on one row, the envelopes → OUTPUT on the next (scrolls).
#[allow(clippy::too_many_arguments)]
fn wrapped(
	ui: &mut Ui,
	state: &mut AppState,
	cx: &BodyCx,
	v: &mut SynthView,
	voices: &[VoicePos],
	area: Rect,
	row_h: f32,
	now: f64,
) {
	let area = area.shrink2(vec2(2.0, 0.0));
	let arrow = 18.0;
	let knob = KnobSize::S;
	let display_h = (row_h - TITLE_H - 2.0 * GAP - devices::knob_height(knob)).max(80.0);
	let first = Rect::from_min_size(area.min, vec2(area.width(), row_h));
	let second = Rect::from_min_size(
		pos2(area.left(), first.bottom() + 18.0),
		vec2(area.width(), row_h),
	);
	ui.painter().hline(
		second.x_range(),
		second.top() - 9.0,
		Stroke::new(1.0, theme::BORDER_SUBTLE),
	);
	let osc_w = ((area.width() - arrow) * 0.48).clamp(344.0, 430.0);
	let osc = first.with_max_x(first.left() + osc_w);
	let filter = first.with_min_x(osc.right() + arrow);
	arrow_at(
		ui,
		pos2(
			osc.right() + arrow * 0.5,
			first.top() + TITLE_H + GAP + display_h * 0.5,
		),
	);
	osc_section(ui, state, cx, v, osc, true);
	filter_section(ui, state, cx, v, filter, display_h, knob);
	let env = second.with_max_x(second.left() + (area.width() - arrow) * 0.62);
	let out = second.with_min_x(env.right() + arrow);
	arrow_at(
		ui,
		pos2(
			env.right() + arrow * 0.5,
			second.top() + TITLE_H + GAP + display_h * 0.5,
		),
	);
	envelopes(ui, state, cx, env, display_h, voices);
	output_section(
		ui,
		state,
		cx,
		v,
		out,
		display_h * 0.7,
		output_knob(true, row_h, display_h),
		now,
	);
}

const STACK_OSC_H: f32 = 214.0;
const STACK_FILTER_DISPLAY: f32 = 150.0;
const STACK_ENV_DISPLAY: f32 = 110.0;
const STACK_SCOPE: f32 = 96.0;

fn stack_heights() -> [f32; 4] {
	let knob = devices::knob_height(KnobSize::S);
	[
		STACK_OSC_H,
		TITLE_H + GAP + STACK_FILTER_DISPLAY + GAP + knob,
		TITLE_H + GAP + STACK_ENV_DISPLAY + GAP + ENV_READOUT_2X2_H,
		TITLE_H + GAP + STACK_SCOPE + GAP + knob,
	]
}

fn stacked_height() -> f32 {
	stack_heights().iter().sum::<f32>() + 3.0 * 18.0 + 8.0
}

/// Narrow docks: OSC / FILTER / AMP + FILTER ENVELOPE / OUTPUT stacked (scrolls).
fn stacked(
	ui: &mut Ui,
	state: &mut AppState,
	cx: &BodyCx,
	v: &mut SynthView,
	voices: &[VoicePos],
	area: Rect,
	now: f64,
) {
	let [osc_h, filter_h, env_h, out_h] = stack_heights();
	let area = area.shrink2(vec2(2.0, 0.0));
	let mut y = area.top();
	let mut next = |h: f32| {
		let r = Rect::from_min_size(pos2(area.left(), y), vec2(area.width(), h));
		y += h + 18.0;
		r
	};
	let osc = next(osc_h);
	let filter = next(filter_h);
	let env = next(env_h);
	let out = next(out_h);
	for r in [filter, env, out] {
		ui.painter().hline(
			r.x_range(),
			r.top() - 9.0,
			Stroke::new(1.0, theme::BORDER_SUBTLE),
		);
	}
	osc_section(ui, state, cx, v, osc, true);
	filter_section(ui, state, cx, v, filter, STACK_FILTER_DISPLAY, KnobSize::S);
	envelopes(ui, state, cx, env, STACK_ENV_DISPLAY, voices);
	output_section(ui, state, cx, v, out, STACK_SCOPE, KnobSize::S, now);
}

fn title(ui: &Ui, rect: Rect, text: &str) -> Rect {
	ui.painter().text(
		pos2(rect.left(), rect.top() + TITLE_H * 0.5),
		Align2::LEFT_CENTER,
		text,
		theme::semibold(10.5),
		theme::TEXT_FAINT,
	)
}

fn knob_cells(ui: &mut Ui, state: &mut AppState, cx: &BodyCx, rect: Rect, knobs: &[KnobDef]) {
	devices::knob_grid(ui, state, cx, rect, knobs, knobs.len());
}

// --- OSC ------------------------------------------------------------------------------

const OSC_NAME_W: f32 = 46.0;
const READOUT_W: f32 = 50.0;

fn osc_section(
	ui: &mut Ui,
	state: &mut AppState,
	cx: &BodyCx,
	v: &mut SynthView,
	rect: Rect,
	small: bool,
) {
	title(ui, rect, "OSC 1 + OSC 2 + NOISE");
	let knob_h = devices::knob_height(KnobSize::S);
	let rows_top = rect.top() + TITLE_H + GAP;
	let row_h = ((rect.height() - TITLE_H - 2.0 * GAP - knob_h - 6.0) * 0.5).clamp(34.0, 46.0);
	let readouts_w = 2.0 * READOUT_W + 12.0;
	for (osc, param) in WAVE_PARAMS.into_iter().enumerate() {
		let row = Rect::from_min_size(
			pos2(rect.left(), rows_top + osc as f32 * (row_h + 6.0)),
			vec2(rect.width(), row_h),
		);
		ui.painter().text(
			pos2(row.left(), row.center().y),
			Align2::LEFT_CENTER,
			format!("Osc {}", osc + 1),
			theme::semibold(12.0),
			theme::TEXT,
		);
		let cards_w = row.width() - OSC_NAME_W - readouts_w - 6.0;
		let card_w = (cards_w / 4.0 - 6.0).clamp(36.0, 52.0);
		let selected = ps::wave_index(value(state, cx, param));
		for w in 0..4 {
			let card = Rect::from_min_size(
				pos2(
					row.left() + OSC_NAME_W + w as f32 * (card_w + 6.0),
					row.top() + 2.0,
				),
				vec2(card_w, row_h - 4.0),
			);
			wave_card(
				ui,
				state,
				cx,
				card,
				osc,
				w,
				w == selected,
				!small || card_w >= 44.0,
			);
		}
		if osc == 1 {
			let right = row.right();
			for (i, (id, label)) in [(OSC2_SEMI, "Semi"), (OSC2_CENTS, "Cents")]
				.into_iter()
				.enumerate()
			{
				let x = right - (2 - i) as f32 * (READOUT_W + 6.0);
				let r = Rect::from_min_size(pos2(x, row.center().y - 18.0), vec2(READOUT_W, 24.0));
				tune_readout(ui, state, cx, v, r, id);
				ui.painter().text(
					pos2(r.center().x, r.bottom() + 8.0),
					Align2::CENTER_CENTER,
					label,
					theme::medium(10.5),
					theme::TEXT_FAINT,
				);
			}
		}
	}
	let knobs = Rect::from_min_max(pos2(rect.left(), rect.bottom() - knob_h), rect.max);
	let cell = knobs.width() / 4.0;
	let at = |i: usize| {
		Rect::from_min_size(
			pos2(knobs.left() + i as f32 * cell, knobs.top()),
			vec2(cell, knob_h),
		)
	};
	knob_cells(
		ui,
		state,
		cx,
		at(0),
		&[KnobDef::new(OSC_MIX, "Osc mix", KnobSize::S)],
	);
	unison(ui, state, cx, at(1));
	knob_cells(
		ui,
		state,
		cx,
		at(2),
		&[KnobDef::new(UNISON_DETUNE, "Detune", KnobSize::S)],
	);
	knob_cells(
		ui,
		state,
		cx,
		at(3),
		&[KnobDef::new(NOISE, "Noise", KnobSize::S)],
	);
}

#[allow(clippy::too_many_arguments)]
fn wave_card(
	ui: &mut Ui,
	state: &mut AppState,
	cx: &BodyCx,
	rect: Rect,
	osc: usize,
	w: usize,
	selected: bool,
	caption: bool,
) {
	let name = WAVE_NAMES[w];
	let label = format!("Osc {} {name}", osc + 1);
	let resp = ui
		.interact(
			rect,
			Id::new(("poly-synth-wave", cx.salt, cx.r.device, osc, w)),
			Sense::click(),
		)
		.on_hover_cursor(CursorIcon::PointingHand);
	resp.widget_info(|| {
		egui::WidgetInfo::selected(egui::WidgetType::RadioButton, true, selected, &label)
	});
	let param = WAVE_PARAMS[osc];
	if resp.clicked() && !selected {
		state.commit_param(cx.r.track, cx.r.device, param, w as f32);
	}
	resp.context_menu(|ui| {
		if ui.button("Copy to other osc").clicked() {
			let (other, wave) = ps::copy_wave(osc, value(state, cx, param));
			state.commit_param(cx.r.track, cx.r.device, other, wave);
			ui.close();
		}
	});
	let resp = resp.on_hover_text(format!("{name} (right-click: copy to the other osc)"));
	if !ui.is_rect_visible(rect) {
		return;
	}
	let ai = state.ai_change(cx.r.device, param).is_some() && selected;
	let p = ui.painter();
	let (fill, ring, width) = if selected {
		(
			theme::BG_HOVER,
			if ai { theme::ACCENT } else { VIOLET },
			1.5,
		)
	} else if resp.hovered() {
		(theme::BG_WIDGET, theme::BORDER_STRONG, 1.0)
	} else {
		(theme::BG_RAISED, theme::BORDER, 1.0)
	};
	p.rect(
		rect,
		CornerRadius::same(5),
		fill,
		Stroke::new(width, ring),
		StrokeKind::Inside,
	);
	let ink = if selected {
		VIOLET_LIGHT
	} else {
		theme::TEXT_FAINT
	};
	let pic_h = if caption {
		rect.height() * 0.5
	} else {
		rect.height() - 10.0
	};
	let pic = Rect::from_min_size(
		pos2(rect.left() + 5.0, rect.top() + 5.0),
		vec2(rect.width() - 10.0, pic_h),
	);
	device_kit::paint_wave(&p.with_clip_rect(rect), pic, wave(w), ink);
	if caption {
		p.text(
			pos2(rect.center().x, rect.bottom() - 8.0),
			Align2::CENTER_CENTER,
			name,
			theme::medium(10.0),
			if selected {
				theme::TEXT
			} else {
				theme::TEXT_FAINT
			},
		);
	}
}

/// Osc 2 semitones / cents: a boxed readout dragged ⇕ (Shift fine on cents), click to
/// type, right-click for the control menu.
fn tune_readout(
	ui: &mut Ui,
	state: &mut AppState,
	cx: &BodyCx,
	v: &mut SynthView,
	rect: Rect,
	id: &'static str,
) {
	let Some(info) = cx.info.param(id) else {
		return;
	};
	let r = cx.r;
	let current = value(state, cx, id);
	let base = Id::new(("poly-synth-tune", cx.salt, r.device, id));
	let shown = if v.tune.is_some_and(|(p, _, _)| p == id) {
		ReadoutState::Active
	} else if state.ai_change(r.device, id).is_some() {
		ReadoutState::Ai
	} else {
		ReadoutState::Rest
	};
	let mut child = ui.new_child(UiBuilder::new().max_rect(rect));
	let (typed, resp) = device_kit::readout(
		&mut child,
		base,
		current,
		info.min,
		info.max,
		ValueFmt::unit(info.unit),
		ReadoutLook::Boxed,
		rect.size(),
		shown,
	);
	if let Some(t) = typed {
		state.commit_param(r.track, r.device, id, t);
	}
	let menu = device_kit::control_menu(
		&resp,
		state,
		cadenza_core::ParamAddress::device(r.track, r.device, id),
		ControlInfo {
			value: current,
			min: info.min,
			max: info.max,
			default: info.default,
			fmt: ValueFmt::unit(info.unit),
		},
	);
	if let Some(m) = menu {
		state.commit_param(r.track, r.device, id, m);
	}
	let drag = ui
		.interact(rect, base.with("drag"), Sense::drag())
		.on_hover_cursor(CursorIcon::ResizeVertical);
	let fine = ui.input(|i| i.modifiers.shift);
	let to = |start: f32, dy: f32| {
		if id == OSC2_SEMI {
			ps::drag_semitones(start, dy)
		} else {
			ps::drag_cents(start, dy, fine)
		}
	};
	if drag.drag_started() {
		v.tune = Some((id, current, 0.0));
	}
	if drag.dragged()
		&& let Some((_, start, dy)) = v.tune.as_mut().filter(|(p, _, _)| *p == id)
	{
		*dy += drag.drag_delta().y;
		state.preview_param(r.device, id, to(*start, *dy));
	}
	if drag.drag_stopped()
		&& let Some((_, start, dy)) = v.tune.take().filter(|(p, _, _)| *p == id)
	{
		let name = info.name;
		devices::commit_changes(state, r, cx.info, &[(id, to(start, dy))], |_| {
			name.to_owned()
		});
	}
}

fn unison(ui: &mut Ui, state: &mut AppState, cx: &BodyCx, cell: Rect) {
	let seg = Rect::from_center_size(
		pos2(cell.center().x, cell.top() + 4.0 + 20.0 + 12.0),
		vec2(124.0, 24.0),
	);
	unison_selector(ui, state, cx, seg);
	ui.painter().text(
		pos2(cell.center().x, seg.bottom() + 12.0),
		Align2::CENTER_CENTER,
		"Unison voices",
		theme::medium(11.0),
		theme::TEXT_DIM,
	);
}

/// The 1–4 unison voices segmented, drawn from `rect`'s left edge.
fn unison_selector(ui: &mut Ui, state: &mut AppState, cx: &BodyCx, rect: Rect) {
	let current = value(state, cx, UNISON).round().clamp(1.0, 4.0) as usize - 1;
	let mut child = ui.new_child(
		UiBuilder::new()
			.max_rect(rect)
			.layout(Layout::left_to_right(Align::Center)),
	);
	if let Some(i) = device_kit::segmented(
		&mut child,
		Id::new(("poly-synth-unison", cx.salt, cx.r.device)),
		&["1", "2", "3", "4"],
		current,
	) && i != current
	{
		state.commit_param(cx.r.track, cx.r.device, UNISON, (i + 1) as f32);
	}
}

// --- Filter ---------------------------------------------------------------------------

fn finish_wheel(ui: &Ui, state: &mut AppState, cx: &BodyCx, v: &mut SynthView) {
	let Some(w) = v.wheel else {
		return;
	};
	let idle = ui.input(|i| i.time) - w.last;
	if idle >= WHEEL_REST {
		v.wheel = None;
		devices::commit_changes(state, cx.r, cx.info, &[(ENV_AMOUNT, w.value)], |_| {
			"Filter env amount".to_owned()
		});
	} else {
		ui.ctx()
			.request_repaint_after(std::time::Duration::from_secs_f64(WHEEL_REST - idle));
	}
}

fn wheel_env(ui: &Ui, state: &mut AppState, cx: &BodyCx, v: &mut SynthView, notches: f32) {
	let now = ui.input(|i| i.time);
	let start = value(state, cx, ENV_AMOUNT);
	let w = v.wheel.get_or_insert(WheelEdit {
		start,
		notches: 0.0,
		value: start,
		last: now,
	});
	w.notches += notches;
	w.last = now;
	w.value = ps::wheel_env(w.start, w.notches);
	state.preview_param(cx.r.device, ENV_AMOUNT, w.value);
	ui.ctx()
		.request_repaint_after(std::time::Duration::from_secs_f64(WHEEL_REST));
}

fn slope_selector(ui: &mut Ui, state: &mut AppState, cx: &BodyCx, right_center: Pos2) {
	let slope = value(state, cx, SLOPE).round().clamp(0.0, 1.0) as usize;
	let rect = Rect::from_min_max(
		pos2(right_center.x - 130.0, right_center.y - 12.0),
		pos2(right_center.x, right_center.y + 12.0),
	);
	let mut child = ui.new_child(
		UiBuilder::new()
			.max_rect(rect)
			.layout(Layout::right_to_left(Align::Center)),
	);
	if let Some(i) = device_kit::segmented(
		&mut child,
		Id::new(("poly-synth-slope", cx.salt, cx.r.device)),
		&["12 dB", "24 dB"],
		slope,
	) && i != slope
	{
		state.commit_param(cx.r.track, cx.r.device, SLOPE, i as f32);
	}
}

/// The filter response with its node; `caption` adds the gesture hint at the bottom.
fn filter_display(
	ui: &mut Ui,
	state: &mut AppState,
	cx: &BodyCx,
	v: &mut SynthView,
	rect: Rect,
	caption: bool,
) {
	let r = cx.r;
	let cutoff = value(state, cx, CUTOFF);
	let res = value(state, cx, RESONANCE);
	let slope = value(state, cx, SLOPE) >= 0.5;
	let env = value(state, cx, ENV_AMOUNT);
	let response = |f: f32| ps::response_db(cutoff, res, slope, f);
	let node_db = response(cutoff);
	let ai = [CUTOFF, RESONANCE, ENV_AMOUNT]
		.iter()
		.any(|p| state.ai_change(r.device, p).is_some());
	let nodes = [FreqNode {
		label: "F",
		freq: cutoff,
		db: node_db,
		color: VIOLET,
		dormant: false,
		x_only: false,
		ai,
	}];
	let range = DbRange::FILTER;
	let mut child = ui.new_child(UiBuilder::new().max_rect(rect));
	let mut display = FreqDisplay::new(
		Id::new(("poly-synth-filter", cx.salt, r.device)),
		rect.size(),
	)
	.range(range)
	.sum(&response, VIOLET_LIGHT)
	.sum_fill(0.12)
	.nodes(&nodes);
	if cx.salt != crate::ui::device_editor::DOCK {
		display = display.small_nodes();
	}
	let out = display.show(&mut child);
	for e in out.events {
		match e {
			FreqEvent::Move {
				freq, db, commit, ..
			} => {
				let start = *v.node.get_or_insert(NodeStart {
					resonance: res,
					db: node_db,
				});
				let changes = ps::node_drag(start, freq, db);
				if commit {
					v.node = None;
					devices::commit_changes(state, r, cx.info, &changes, |ids| {
						if ids.len() == 2 {
							"Filter cutoff and resonance".to_owned()
						} else if ids == [CUTOFF] {
							"Filter cutoff".to_owned()
						} else {
							"Filter resonance".to_owned()
						}
					});
				} else {
					for (id, x) in changes {
						state.preview_param(r.device, id, x);
					}
				}
			}
			FreqEvent::Reset { .. } => {
				v.node = None;
				devices::commit_changes(state, r, cx.info, &ps::reset_node(), |_| {
					"Reset filter".to_owned()
				});
			}
			FreqEvent::Wheel { notches, .. } => wheel_env(ui, state, cx, v, notches),
			_ => {}
		}
	}
	if out.dragging.is_none() && !ui.input(|i| i.pointer.any_down()) {
		v.node = None;
	}
	out.response.on_hover_text_at_pointer(
		"Drag the node: \u{21d4} cutoff, \u{21d5} resonance (Shift fine) \u{b7} wheel over it: \
		 env amount \u{b7} double-click resets",
	);
	if !ui.is_rect_visible(rect) {
		return;
	}
	// The same mapping as the display's plot (no piano strip).
	let plot = rect.shrink(1.0);
	let x_of = |f: f32| plot.left() + device_display::freq_to_x(f, plot.width());
	let y_of = |db: f32| plot.top() + device_display::db_to_y(db, range, plot.height());
	let p = ui.painter().with_clip_rect(plot);
	if env.abs() >= 0.5 {
		let peak = ps::env_peak_cutoff(cutoff, env);
		let n = 120;
		let pts: Vec<Pos2> = (0..=n)
			.map(|i| {
				let f = 20.0 * 1000f32.powf(i as f32 / n as f32);
				pos2(
					x_of(f),
					y_of(
						ps::response_db(peak, res, slope, f)
							.clamp(range.min - 6.0, range.max + 6.0),
					),
				)
			})
			.collect();
		p.extend(Shape::dashed_line(
			&pts,
			Stroke::new(1.25, VIOLET.gamma_multiply(0.6)),
			4.0,
			4.0,
		));
	}
	let node = pos2(
		x_of(cutoff),
		y_of(node_db.clamp(range.min, range.max)).clamp(plot.top() + 10.0, plot.bottom() - 10.0),
	);
	let tag = ps::filter_tag(cutoff, res);
	let (anchor, align) = if node.x > plot.right() - 170.0 {
		(node + vec2(-16.0, -14.0), Align2::RIGHT_BOTTOM)
	} else {
		(node + vec2(16.0, -14.0), Align2::LEFT_BOTTOM)
	};
	device_kit::paint_value_tag(
		&p,
		anchor,
		align,
		&tag,
		if out.dragging.is_some() {
			theme::TEXT
		} else {
			VIOLET_LIGHT
		},
		Rect::from_min_max(plot.min + vec2(30.0, 24.0), plot.max - vec2(0.0, 14.0)),
	);
	if caption {
		let text = if env.abs() >= 0.5 {
			format!(
				"\u{21d4} cutoff \u{b7} \u{21d5} resonance \u{b7} dashed = env peak {}",
				ps::env_label(env)
			)
		} else {
			"\u{21d4} cutoff \u{b7} \u{21d5} resonance \u{b7} wheel = env amount".to_owned()
		};
		let g = p.layout_no_wrap(text, theme::medium(10.5), theme::TEXT_FAINT);
		if g.size().x < plot.width() * 0.62 {
			p.galley(
				pos2(plot.right() - 8.0 - g.size().x, plot.top() + 6.0),
				g,
				theme::TEXT_FAINT,
			);
		}
	}
}

fn filter_section(
	ui: &mut Ui,
	state: &mut AppState,
	cx: &BodyCx,
	v: &mut SynthView,
	rect: Rect,
	display_h: f32,
	knob: KnobSize,
) {
	title(
		ui,
		rect,
		if rect.width() >= 330.0 {
			"FILTER \u{b7} LOW-PASS"
		} else {
			"FILTER"
		},
	);
	slope_selector(
		ui,
		state,
		cx,
		pos2(rect.right(), rect.top() + TITLE_H * 0.5),
	);
	let display = Rect::from_min_size(
		pos2(rect.left(), rect.top() + TITLE_H + GAP),
		vec2(rect.width(), display_h),
	);
	filter_display(ui, state, cx, v, display, true);
	let knobs = Rect::from_min_max(pos2(rect.left(), display.bottom() + GAP), rect.max);
	let mut defs = vec![
		KnobDef::new(ENV_AMOUNT, "Env amount", knob).color(VIOLET_LIGHT),
		KnobDef::new(KEY_TRACK, "Key track", knob).color(VIOLET_LIGHT),
	];
	if rect.width() >= 250.0 {
		defs.insert(
			0,
			KnobDef::new(RESONANCE, "Resonance", knob).color(VIOLET_LIGHT),
		);
		defs.insert(0, KnobDef::new(CUTOFF, "Cutoff", knob).color(VIOLET_LIGHT));
	}
	knob_cells(ui, state, cx, knobs, &defs);
}

// --- Envelopes ------------------------------------------------------------------------

/// Readouts as one row of 4 cells (label over value).
const ENV_READOUT_ROW_H: f32 = 38.0;
/// ... or a 2 × 2 grid of one-line cells in a narrow column.
const ENV_READOUT_2X2_H: f32 = 42.0;

#[allow(clippy::too_many_arguments)]
fn env_section(
	ui: &mut Ui,
	state: &mut AppState,
	cx: &BodyCx,
	env: Env,
	rect: Rect,
	display_h: f32,
	voices: &[VoicePos],
) {
	title(ui, rect, env.title());
	let display = Rect::from_min_size(
		pos2(rect.left(), rect.top() + TITLE_H + GAP),
		vec2(rect.width(), display_h),
	);
	let shown = envelope(ui, state, cx, env, display, voices);
	let hint = match env {
		Env::Amp => "drag nodes \u{b7} double-click resets",
		Env::Filter => "drives cutoff by env amount",
	};
	let g = ui
		.painter()
		.layout_no_wrap(hint.to_owned(), theme::medium(10.5), theme::TEXT_FAINT);
	if g.size().x < display.width() * 0.6 {
		ui.painter().galley(
			pos2(display.right() - 8.0 - g.size().x, display.top() + 6.0),
			g,
			theme::TEXT_FAINT,
		);
	}
	let reads = Rect::from_min_size(
		pos2(rect.left(), display.bottom() + GAP),
		vec2(rect.width(), ENV_READOUT_ROW_H),
	);
	env_readouts(ui, state, cx, env, &shown, reads);
}

/// The kit ADSR bound to `env`'s params: previews while dragging, one step per node
/// gesture (decay + sustain together as one Batch). Returns the values shown.
fn envelope(
	ui: &mut Ui,
	state: &mut AppState,
	cx: &BodyCx,
	env: Env,
	rect: Rect,
	voices: &[VoicePos],
) -> crate::state::device_display::AdsrValues {
	let r = cx.r;
	let ranges = env.ranges();
	let values = env.values(|id| value(state, cx, id));
	let mut child = ui.new_child(UiBuilder::new().max_rect(rect));
	let out = device_kit::adsr(
		&mut child,
		Id::new(("poly-synth-env", cx.salt, r.device, env == Env::Amp)),
		values,
		&ranges,
		env_color(env),
		rect.size(),
		voices,
	);
	let mut commits: Vec<(&'static str, f32)> = Vec::new();
	let mut stages = Vec::new();
	let mut shown = values;
	for stage in AdsrStage::ALL {
		match out.gesture(stage) {
			Gesture::Preview(x) => {
				state.preview_param(r.device, env.param(stage), x);
				shown.set(stage, x);
			}
			Gesture::Commit(x) => {
				commits.push((env.param(stage), x));
				stages.push(stage);
				shown.set(stage, x);
			}
			Gesture::Idle => {}
		}
	}
	if !commits.is_empty() {
		devices::commit_changes(state, r, cx.info, &commits, |_| env.label(&stages));
	}
	shown
}

fn env_readouts(
	ui: &mut Ui,
	state: &AppState,
	cx: &BodyCx,
	env: Env,
	v: &crate::state::device_display::AdsrValues,
	rect: Rect,
) {
	let reads = ps::readouts(v);
	let p = ui.painter();
	let color = |stage: AdsrStage| {
		if state.ai_change(cx.r.device, env.param(stage)).is_some() {
			theme::ACCENT_TEXT
		} else {
			theme::TEXT
		}
	};
	if rect.width() >= 230.0 {
		let cell_w = (rect.width() - 12.0) / 4.0;
		for (i, ((label, text), stage)) in reads.iter().zip(AdsrStage::ALL).enumerate() {
			let cell = Rect::from_min_size(
				pos2(rect.left() + i as f32 * (cell_w + 4.0), rect.top()),
				vec2(cell_w, ENV_READOUT_ROW_H),
			);
			p.rect_filled(cell, CornerRadius::same(theme::RADIUS), CELL_BG);
			p.text(
				pos2(cell.center().x, cell.top() + 11.0),
				Align2::CENTER_CENTER,
				*label,
				theme::medium(10.5),
				theme::TEXT_FAINT,
			);
			p.text(
				pos2(cell.center().x, cell.bottom() - 11.0),
				Align2::CENTER_CENTER,
				text,
				theme::mono(12.0),
				color(stage),
			);
		}
	} else {
		let cell_w = (rect.width() - 4.0) / 2.0;
		let short = cell_w < 124.0;
		for (i, ((label, text), stage)) in reads.iter().zip(AdsrStage::ALL).enumerate() {
			let label = if short { &label[..1] } else { label };
			let cell = Rect::from_min_size(
				pos2(
					rect.left() + (i % 2) as f32 * (cell_w + 4.0),
					rect.top() + (i / 2) as f32 * 21.0,
				),
				vec2(cell_w, 19.0),
			);
			p.rect_filled(cell, CornerRadius::same(theme::RADIUS), CELL_BG);
			p.text(
				pos2(cell.left() + 6.0, cell.center().y),
				Align2::LEFT_CENTER,
				label,
				theme::medium(10.0),
				theme::TEXT_FAINT,
			);
			p.text(
				pos2(cell.right() - 6.0, cell.center().y),
				Align2::RIGHT_CENTER,
				text,
				theme::mono(11.0),
				color(stage),
			);
		}
	}
}

// --- Output ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn output_section(
	ui: &mut Ui,
	state: &mut AppState,
	cx: &BodyCx,
	v: &mut SynthView,
	rect: Rect,
	scope_h: f32,
	knob: KnobSize,
	now: f64,
) {
	title(ui, rect, "OUTPUT");
	let scope_rect = Rect::from_min_size(
		pos2(rect.left(), rect.top() + TITLE_H + GAP),
		vec2(rect.width(), scope_h),
	);
	let held: Vec<u8> =
		cx.r.track
			.map(|t| {
				state
					.previewing
					.iter()
					.filter(|(tr, _)| *tr == t)
					.map(|(_, n)| *n)
					.collect()
			})
			.unwrap_or_default();
	scope(ui, v, scope_rect, &ps::scope_label(&held), now);
	let knobs = Rect::from_min_max(pos2(rect.left(), scope_rect.bottom() + GAP), rect.max);
	knob_cells(
		ui,
		state,
		cx,
		knobs.with_max_y(knobs.top() + devices::knob_height(knob) + 4.0),
		&[
			KnobDef::new(VEL_SENS, "Velocity", knob),
			KnobDef::new(GLIDE, "Glide", knob),
			KnobDef::new(GAIN, "Gain", knob).step(0.5),
		],
	);
}

fn scope(ui: &mut Ui, v: &mut SynthView, rect: Rect, label: &str, now: f64) {
	if !ui.is_rect_visible(rect) {
		return;
	}
	let p = ui.painter().with_clip_rect(rect);
	device_kit::paint_display(&p, rect);
	let inner = p.with_clip_rect(rect.shrink(1.0));
	let cy = rect.center().y + 6.0;
	let half = (rect.height() - 28.0) * 0.5;
	inner.hline(rect.x_range(), cy, Stroke::new(1.0, theme::BORDER));
	let live = (v.synthetic || now - v.heard < LIVE_FOR) && v.scope.len() > 16;
	if live {
		let samples = v.scope.make_contiguous();
		let window = ps::scope_window(samples, ps::SCOPE_POINTS);
		let gain = ps::scope_gain(&window);
		let n = window.len().max(2) - 1;
		let pts: Vec<Pos2> = window
			.iter()
			.enumerate()
			.map(|(i, s)| {
				pos2(
					rect.left() + 1.0 + i as f32 / n as f32 * (rect.width() - 2.0),
					cy - s * gain * half,
				)
			})
			.collect();
		inner.add(Shape::line(
			pts.clone(),
			Stroke::new(5.0, SCOPE_GREEN.gamma_multiply(0.2)),
		));
		inner.add(Shape::line(pts, Stroke::new(1.5, SCOPE_LIGHT)));
		if !v.synthetic {
			ui.ctx().request_repaint();
		}
	} else {
		inner.hline(
			(rect.left() + 1.0)..=(rect.right() - 1.0),
			cy,
			Stroke::new(1.25, SCOPE_LIGHT.gamma_multiply(0.5)),
		);
	}
	p.text(
		rect.left_top() + vec2(8.0, 7.0),
		Align2::LEFT_TOP,
		label,
		theme::medium(10.5),
		theme::TEXT_FAINT,
	);
}

// --- Compact (inspector) --------------------------------------------------------------

const COMPACT_ROW_H: f32 = 34.0;
const COMPACT_ENV_H: f32 = 84.0;
const COMPACT_UNISON_H: f32 = 30.0;

fn compact_filter_h(width: f32) -> f32 {
	(width * 0.5).clamp(110.0, 170.0)
}

fn compact_knobs() -> [KnobDef; 10] {
	let s = KnobSize::S;
	[
		KnobDef::new(OSC2_SEMI, "Semi", s),
		KnobDef::new(OSC2_CENTS, "Cents", s),
		KnobDef::new(OSC_MIX, "Osc mix", s),
		KnobDef::new(UNISON_DETUNE, "Detune", s),
		KnobDef::new(NOISE, "Noise", s),
		KnobDef::new(ENV_AMOUNT, "Env amt", s).color(VIOLET_LIGHT),
		KnobDef::new(KEY_TRACK, "Key track", s).color(VIOLET_LIGHT),
		KnobDef::new(VEL_SENS, "Velocity", s),
		KnobDef::new(GLIDE, "Glide", s),
		KnobDef::new(GAIN, "Gain", s).step(0.5),
	]
}

/// How tall the compact form is at `width`.
pub fn compact_height(width: f32) -> f32 {
	2.0 * COMPACT_ROW_H
		+ COMPACT_UNISON_H
		+ 6.0 + TITLE_H
		+ compact_filter_h(width)
		+ 6.0 + TITLE_H
		+ COMPACT_ENV_H
		+ 8.0 + devices::small_rows_height(width, compact_knobs().len())
}

fn compact(ui: &mut Ui, state: &mut AppState, cx: &BodyCx, v: &mut SynthView, rect: Rect) {
	let mut y = rect.top();
	for (osc, param) in WAVE_PARAMS.into_iter().enumerate() {
		let row = Rect::from_min_size(pos2(rect.left(), y), vec2(rect.width(), COMPACT_ROW_H));
		ui.painter().text(
			pos2(row.left(), row.center().y),
			Align2::LEFT_CENTER,
			format!("Osc {}", osc + 1),
			theme::semibold(11.5),
			theme::TEXT,
		);
		let name_w = 42.0;
		let card_w = (row.width() - name_w) / 4.0 - 4.0;
		let selected = ps::wave_index(value(state, cx, param));
		for w in 0..4 {
			let card = Rect::from_min_size(
				pos2(
					row.left() + name_w + w as f32 * (card_w + 4.0),
					row.top() + 2.0,
				),
				vec2(card_w, COMPACT_ROW_H - 4.0),
			);
			wave_card(ui, state, cx, card, osc, w, w == selected, false);
		}
		y = row.bottom();
	}
	let uni = Rect::from_min_size(pos2(rect.left(), y), vec2(rect.width(), COMPACT_UNISON_H));
	ui.painter().text(
		pos2(uni.left(), uni.center().y),
		Align2::LEFT_CENTER,
		"Unison",
		theme::medium(11.0),
		theme::TEXT_DIM,
	);
	unison_selector(
		ui,
		state,
		cx,
		Rect::from_min_size(
			pos2(uni.left() + 42.0, uni.center().y - 12.0),
			vec2(124.0, 24.0),
		),
	);
	y = uni.bottom() + 6.0;
	let head = Rect::from_min_size(pos2(rect.left(), y), vec2(rect.width(), TITLE_H));
	title(ui, head, "FILTER");
	slope_selector(ui, state, cx, pos2(head.right(), head.center().y));
	let display = Rect::from_min_size(
		pos2(rect.left(), head.bottom()),
		vec2(rect.width(), compact_filter_h(rect.width())),
	);
	filter_display(ui, state, cx, v, display, false);
	y = display.bottom() + 6.0;
	let col = (rect.width() - 8.0) * 0.5;
	for (i, env) in Env::ALL.into_iter().enumerate() {
		let x = rect.left() + i as f32 * (col + 8.0);
		let head = Rect::from_min_size(pos2(x, y), vec2(col, TITLE_H));
		title(
			ui,
			head,
			match env {
				Env::Amp => "AMP ENV",
				Env::Filter => "FILTER ENV",
			},
		);
		let r = Rect::from_min_size(pos2(x, head.bottom()), vec2(col, COMPACT_ENV_H));
		envelope(ui, state, cx, env, r, &[]);
	}
	y += TITLE_H + COMPACT_ENV_H + 8.0;
	let knobs = compact_knobs();
	let cols = devices::small_columns(rect.width(), knobs.len());
	let k = Rect::from_min_max(pos2(rect.left(), y), rect.max);
	devices::knob_grid(ui, state, cx, k, &knobs, cols);
}
