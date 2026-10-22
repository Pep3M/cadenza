//! The Limiter editor body (design `16 Effects`, 16a): the waveform history (input +
//! gain, output, gain reduction from the top, the draggable ceiling; dragging the
//! history moves the input gain, the wheel the release), the IN / GR / OUT meters, the
//! live LOUDNESS panel (short-term, integrated, true peak, the last 30 s, target chips
//! that only move the green band) and the knobs. Compact: readouts, the history and one
//! row of S knobs. The gesture -> parameter mapping lives in
//! [`crate::state::limiter_editor`], the loudness in [`crate::state::loudness_meter`].

use crate::i18n::{mark, tr, trf};
use std::sync::{Arc, Mutex};

use cadenza_dsp::readings;
use egui::{
	Align, Align2, Color32, CornerRadius, CursorIcon, Id, Layout, Rect, Sense, Shape, Stroke,
	StrokeKind, Ui, UiBuilder, pos2, vec2,
};

use crate::state::AppState;
use crate::state::device_display::LevelHistory;
use crate::state::limiter_editor::{
	self as lim, CEILING, INPUT_GAIN, LimValues, RELEASE, Target, WAVE_MAX_DB,
};
use crate::state::loudness_meter::{LiveLoudness, Side};
use crate::state::meters::{FLOOR_DB, amp_to_db};
use crate::state::selection::DeviceRef;
use crate::theme;
use crate::ui::device_editor::BodyCx;
use crate::ui::device_kit::{self, HistoryLine, KnobSize, ParamKnob};
use crate::ui::widgets::Gesture;

/// Wheel notches over the history are committed as one step once the wheel rests this long.
const WHEEL_REST: f64 = 0.5;
const HISTORY_COLUMNS: usize = 420;
/// History lengths of the display options (seconds).
const HISTORY_SECONDS: [f32; 3] = [3.0, 6.0, 12.0];
/// The history's gain reduction scale: 12 dB reach 38 % of the height (design 16a).
const HISTORY_GR_DB: f32 = 12.0 / 0.38;
const GR_METER_DB: f32 = 12.0;
const FALL_DB_PER_S: f32 = 24.0;
const FOOTER_H: f32 = 33.0;
const SETTINGS_H: f32 = 32.0;
const METERS_W: f32 = 196.0;
const LOUDNESS_W: f32 = 300.0;
const KNOBS_W: f32 = 340.0;
/// Below this body width the knobs stack in a narrow column.
const NARROW_BELOW: f32 = 1300.0;
const CHIP_H: f32 = 22.0;
const TRACE_H: f32 = 78.0;
const COMPACT_READOUT_H: f32 = 64.0;
const COMPACT_KNOBS_H: f32 = 86.0;
const COMPACT_ROW_H: f32 = 30.0;
const TARGET_GREEN: Color32 = theme::PLAYING;

/// Release wheel notches not committed yet.
#[derive(Debug, Clone, Copy)]
struct WheelEdit {
	start: f32,
	notches: f32,
	value: f32,
	last: f64,
}

/// View state of one limiter editor (dock or inspector), kept in egui memory.
struct LimView {
	history: LevelHistory,
	loudness: LiveLoudness,
	seconds: f32,
	rate: u32,
	target: Target,
	footer: bool,
	/// Latest gain reduction (positive dB) and the IN (+ gain) / OUT peaks (dBFS).
	gr_db: f32,
	input_db: [f32; 2],
	output_db: [f32; 2],
	/// Fed by [`set_display`] (screenshots): live, never decays.
	synthetic: bool,
	/// The input gain when a history drag started.
	gain_drag: Option<f32>,
	wheel: Option<WheelEdit>,
}

impl Default for LimView {
	fn default() -> Self {
		Self {
			history: LevelHistory::new(HISTORY_COLUMNS, 6.0, 48_000),
			loudness: LiveLoudness::new(48_000),
			seconds: 6.0,
			rate: 48_000,
			target: Target::default(),
			footer: false,
			gr_db: 0.0,
			input_db: [FLOOR_DB; 2],
			output_db: [FLOOR_DB; 2],
			synthetic: false,
			gain_drag: None,
			wheel: None,
		}
	}
}

impl LimView {
	fn resize(&mut self, seconds: f32, rate: u32) {
		if seconds != self.seconds || rate != self.rate {
			self.seconds = seconds;
			self.rate = rate;
			self.history = LevelHistory::new(HISTORY_COLUMNS, seconds, rate);
		}
		self.loudness.set_sample_rate(rate);
	}
}

type Shared = Arc<Mutex<LimView>>;

fn view_id(salt: &str, r: DeviceRef) -> Id {
	Id::new(("limiter-editor", salt, r.device))
}

fn view(ctx: &egui::Context, id: Id) -> Shared {
	ctx.data_mut(|d| {
		d.get_temp_mut_or_insert_with(id, || Arc::new(Mutex::new(LimView::default())))
			.clone()
	})
}

fn lock(view: &Shared) -> std::sync::MutexGuard<'_, LimView> {
	view.lock().unwrap_or_else(|e| e.into_inner())
}

/// Shows `history`, `loudness` and the levels in an editor as if the probe had measured
/// them, live (screenshots).
#[allow(clippy::too_many_arguments)]
pub fn set_display(
	ctx: &egui::Context,
	salt: &str,
	r: DeviceRef,
	history: LevelHistory,
	loudness: LiveLoudness,
	gr_db: f32,
	input_db: [f32; 2],
	output_db: [f32; 2],
) {
	let v = view(ctx, view_id(salt, r));
	let mut v = lock(&v);
	v.history = history;
	v.loudness = loudness;
	v.gr_db = gr_db;
	v.input_db = input_db;
	v.output_db = output_db;
	v.synthetic = true;
}

/// Whether the compact editor of `r` in `salt` shows its extra section.
pub fn footer_open(ctx: &egui::Context, salt: &str, r: DeviceRef) -> bool {
	lock(&view(ctx, view_id(salt, r))).footer
}

/// R while the dock has focus: reset the integrated loudness.
pub fn shortcuts(ctx: &egui::Context, r: DeviceRef) {
	if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::R)) {
		let v = view(ctx, view_id(crate::ui::device_editor::DOCK, r));
		lock(&v).loudness.reset_integrated();
	}
}

fn shown_values(state: &AppState, r: DeviceRef) -> LimValues {
	LimValues::from_fn(|p| {
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
		.or_else(|| lim::info().param(id).map(|p| p.default))
		.unwrap_or(0.0)
}

/// The changes that differ from the document, committed as one undo step.
fn commit(state: &mut AppState, r: DeviceRef, changes: &[lim::Change], label: &str) {
	let differs = |a: f32, b: f32| (a - b).abs() > 1e-4 * a.abs().max(1.0);
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

pub fn limiter_body(ui: &mut Ui, state: &mut AppState, cx: &BodyCx) {
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

/// Feeds the history (input times the gain, as the limiter hears it) and the loudness
/// (output, after the ceiling) from this frame's probe frames; meters fall once the
/// probe goes quiet, the loudness holds.
fn update_levels(ui: &Ui, state: &AppState, cx: &BodyCx, v: &mut LimView) {
	if v.synthetic {
		return;
	}
	let dt = ui.input(|i| i.stable_dt).min(0.25);
	let ours = state.probe_target() == Some(cx.r.device);
	let probe = state.probe().filter(|_| ours);
	if let Some(p) = probe {
		v.resize(v.seconds, p.sample_rate());
	}
	let gr = probe
		.and_then(|p| p.reading(readings::GAIN_REDUCTION_DB))
		.unwrap_or(0.0)
		.abs();
	let fall = FALL_DB_PER_S * dt;
	let mut peaks = [[0.0f32; 2]; 2];
	if ours && !cx.frames.is_empty() {
		let gain = 10f32.powf(shown_values(state, cx.r).input_gain / 20.0);
		for f in cx.frames {
			let input = [f.input[0] * gain, f.input[1] * gain];
			v.history.push(input, f.output, gr);
			for c in 0..2 {
				peaks[0][c] = peaks[0][c].max(input[c].abs());
				peaks[1][c] = peaks[1][c].max(f.output[c].abs());
			}
		}
		v.loudness.push_frames(cx.frames, Side::Output);
		v.gr_db = gr;
	} else {
		v.gr_db = (v.gr_db - fall).max(0.0);
	}
	for (levels, peak) in [&mut v.input_db, &mut v.output_db].into_iter().zip(peaks) {
		for (db, amp) in levels.iter_mut().zip(peak) {
			*db = amp_to_db(amp).max(*db - fall).max(FLOOR_DB);
		}
	}
	if v.gr_db > 0.0 || v.input_db.iter().chain(&v.output_db).any(|&d| d > FLOOR_DB) {
		ui.ctx().request_repaint();
	}
}

fn stopped(state: &AppState, cx: &BodyCx, v: &LimView) -> bool {
	!v.synthetic && !state.is_playing() && cx.frames.is_empty()
}

/// Commits release wheel notches once the wheel rests.
fn finish_wheel(ui: &Ui, state: &mut AppState, r: DeviceRef, v: &mut LimView) {
	let Some(w) = v.wheel else {
		return;
	};
	let idle = ui.input(|i| i.time) - w.last;
	if idle >= WHEEL_REST {
		v.wheel = None;
		commit(state, r, &[(RELEASE, w.value)], tr("Limiter release"));
	} else {
		ui.ctx()
			.request_repaint_after(std::time::Duration::from_secs_f64(WHEEL_REST - idle));
	}
}

fn wheel_release(ui: &Ui, state: &mut AppState, r: DeviceRef, v: &mut LimView, notches: f32) {
	let now = ui.input(|i| i.time);
	let start = shown_values(state, r).release;
	let w = v.wheel.get_or_insert(WheelEdit {
		start,
		notches: 0.0,
		value: start,
		last: now,
	});
	w.notches += notches;
	w.last = now;
	w.value = lim::wheel_release(w.start, w.notches);
	state.preview_param(r.device, RELEASE, w.value);
	ui.ctx()
		.request_repaint_after(std::time::Duration::from_secs_f64(WHEEL_REST));
}

// --- Level history ----------------------------------------------------------------------

fn history(ui: &mut Ui, state: &mut AppState, cx: &BodyCx, v: &mut LimView, rect: Rect, big: bool) {
	let r = cx.r;
	let values = shown_values(state, r);
	let time = trf(
		"{seconds} s · now →",
		&[(
			"seconds",
			&crate::i18n::decimal(format!("{:.0}", v.seconds)),
		)],
	);
	let mut child = ui.new_child(UiBuilder::new().max_rect(rect));
	let out =
		device_kit::LevelHistoryView::new(Id::new(("lim-history", cx.salt, r.device)), rect.size())
			.waveform(WAVE_MAX_DB)
			.range(-24.0, 0.0)
			.gr_range(HISTORY_GR_DB)
			.input_label(tr("Input + gain"))
			.line(Some(HistoryLine {
				db: values.ceiling,
				default: lim::RESET_CEILING,
				label: tr("Ceiling"),
				color: theme::TEXT,
			}))
			.legend(big, big)
			.time_label(big.then_some(time.as_str()))
			.body_drag(true)
			.wheel(big)
			.stopped(stopped(state, cx, v))
			.show_full(&mut child, &v.history);
	let tip =
		tr("Drag up or down for input gain · wheel for release · double-click resets the gain");
	out.response.on_hover_text(tip);
	match out.line {
		Gesture::Preview(db) => state.preview_param(r.device, CEILING, lim::drag_ceiling(db)),
		Gesture::Commit(db) => commit(
			state,
			r,
			&[(CEILING, lim::drag_ceiling(db))],
			tr("Limiter ceiling"),
		),
		Gesture::Idle => {}
	}
	match out.body {
		Gesture::Preview(dy) => {
			let start = *v.gain_drag.get_or_insert(values.input_gain);
			state.preview_param(r.device, INPUT_GAIN, lim::drag_gain(start, dy));
		}
		Gesture::Commit(dy) => {
			let start = v.gain_drag.take().unwrap_or(values.input_gain);
			commit(
				state,
				r,
				&[(INPUT_GAIN, lim::drag_gain(start, dy))],
				tr("Limiter input gain"),
			);
		}
		Gesture::Idle => {}
	}
	if out.body_reset {
		v.gain_drag = None;
		commit(
			state,
			r,
			&[lim::reset_gain()],
			tr("Reset limiter input gain"),
		);
	}
	if out.wheel != 0.0 {
		wheel_release(ui, state, r, v, out.wheel);
	}
}

// --- Meters -----------------------------------------------------------------------------

/// IN (+ gain), GR and OUT meters with peak holds and their readouts.
fn meters(ui: &mut Ui, cx: &BodyCx, v: &LimView, rect: Rect) {
	let label_h = 18.0;
	let value_h = 22.0;
	let bar_top = rect.top() + label_h;
	let bar_bottom = rect.bottom() - value_h;
	let peak = |db: [f32; 2]| db[0].max(db[1]);
	let in_peak = peak(v.input_db);
	let items: [(&str, f32, String, Color32); 3] = [
		(
			tr("IN"),
			30.0,
			level_text(in_peak),
			if in_peak > 0.0 {
				theme::METER_HIGH
			} else {
				theme::TEXT
			},
		),
		(
			"GR",
			14.0,
			lim::gr_text(v.gr_db),
			theme::GAIN_REDUCTION_TEXT,
		),
		(tr("OUT"), 30.0, level_text(peak(v.output_db)), theme::TEXT),
	];
	let gap = 26.0;
	let total: f32 = items.iter().map(|i| i.1).sum::<f32>() + gap * 2.0;
	let mut x = rect.center().x - total * 0.5;
	for (i, (label, w, value, color)) in items.into_iter().enumerate() {
		let cx_ = x + w * 0.5;
		let p = ui.painter();
		p.text(
			pos2(cx_, rect.top() + 2.0),
			Align2::CENTER_TOP,
			label,
			theme::medium(10.5),
			theme::TEXT_FAINT,
		);
		p.text(
			pos2(cx_, rect.bottom() - 2.0),
			Align2::CENTER_BOTTOM,
			value,
			theme::mono(12.0),
			color,
		);
		let bar = Rect::from_min_max(pos2(x, bar_top), pos2(x + w, bar_bottom));
		let mut child = ui.new_child(UiBuilder::new().max_rect(bar));
		let id = Id::new(("lim-meter", cx.salt, cx.r.device, i));
		match i {
			0 => {
				device_kit::level_meter(&mut child, id, &v.input_db, bar.size())
					.on_hover_text(tr("Input after the gain · the white tick holds for 1.5 s"));
			}
			1 => {
				device_kit::gr_meter(&mut child, id, v.gr_db, GR_METER_DB, bar.size())
					.on_hover_text(tr(
						"Gain reduction · the white tick holds the largest for 1.5 s",
					));
			}
			_ => {
				device_kit::level_meter(&mut child, id, &v.output_db, bar.size())
					.on_hover_text(tr("Output, never above the ceiling"));
			}
		}
		x += w + gap;
	}
}

fn level_text(db: f32) -> String {
	if db <= FLOOR_DB {
		"\u{2212}\u{221e}".to_owned()
	} else {
		lim::signed_db(db)
	}
}

// --- Loudness ---------------------------------------------------------------------------

/// The LOUDNESS panel: short-term (big), integrated and true peak cells, the last 30 s
/// against the target band, and the target chips.
fn loudness_panel(
	ui: &mut Ui,
	state: &mut AppState,
	cx: &BodyCx,
	v: &mut LimView,
	rect: Rect,
	chips: bool,
) {
	let values = shown_values(state, cx.r);
	let dim = stopped(state, cx, v);
	let p = ui.painter().clone();
	let p = if dim {
		let mut p = p;
		p.multiply_opacity(0.5);
		p
	} else {
		p
	};
	let l = &v.loudness;
	p.text(
		rect.left_top(),
		Align2::LEFT_TOP,
		tr("LOUDNESS"),
		theme::medium(10.5),
		theme::TEXT_FAINT,
	);
	let big = p.layout_no_wrap(
		lim::lufs_text(l.short_term()),
		theme::mono_medium(34.0),
		theme::TEXT,
	);
	let big_w = big.size().x;
	let y = rect.top() + 18.0;
	p.galley(pos2(rect.left(), y), big, theme::TEXT);
	p.text(
		pos2(rect.left() + big_w + 8.0, y + 34.0),
		Align2::LEFT_BOTTOM,
		tr("LUFS short-term"),
		theme::medium(12.0),
		theme::TEXT_DIM,
	);
	// Integrated | True peak.
	let cells_y = y + 50.0;
	let cell_w = (rect.width() - 6.0) * 0.5;
	let tp = l.true_peak_db();
	let ok = lim::true_peak_ok(tp, values.ceiling);
	for (i, (name, value, unit, color)) in [
		(
			tr("Integrated"),
			lim::lufs_text(l.integrated()),
			tr("LUFS"),
			theme::TEXT,
		),
		(
			tr("True peak"),
			lim::lufs_text(tp),
			if ok { "dBTP ✓" } else { "dBTP" },
			if ok {
				TARGET_GREEN
			} else if tp > values.ceiling + 0.05 {
				theme::WARNING
			} else {
				theme::TEXT
			},
		),
	]
	.into_iter()
	.enumerate()
	{
		let cell = Rect::from_min_size(
			pos2(rect.left() + i as f32 * (cell_w + 6.0), cells_y),
			vec2(cell_w, 44.0),
		);
		p.rect_filled(cell, CornerRadius::same(theme::RADIUS_MD), theme::CANVAS);
		p.text(
			cell.left_top() + vec2(8.0, 6.0),
			Align2::LEFT_TOP,
			name,
			theme::medium(11.0),
			theme::TEXT_FAINT,
		);
		let g = p.layout_no_wrap(value, theme::mono(15.0), color);
		let w = g.size().x;
		p.galley(cell.left_top() + vec2(8.0, 21.0), g, color);
		p.text(
			cell.left_top() + vec2(13.0 + w, 36.0),
			Align2::LEFT_BOTTOM,
			unit,
			theme::medium(11.0),
			if ok && i == 1 {
				TARGET_GREEN
			} else {
				theme::TEXT_FAINT
			},
		);
	}
	let trace_rect = Rect::from_min_size(
		pos2(rect.left(), cells_y + 54.0),
		vec2(
			rect.width(),
			TRACE_H.min(rect.bottom() - cells_y - 54.0 - 34.0).max(40.0),
		),
	);
	trace(&p, l, v.target, trace_rect);
	if chips {
		let row_rect = Rect::from_min_size(
			pos2(rect.left(), trace_rect.bottom() + 10.0),
			vec2(rect.width(), 24.0),
		);
		target_chips(ui, cx, v, row_rect);
	}
}

/// "last 30 s": the short-term readings against the target band.
fn trace(p: &egui::Painter, l: &LiveLoudness, target: Target, rect: Rect) {
	device_kit::paint_display(p, rect);
	let range = target.trace_range(l.trace());
	let y = |lufs: f32| rect.bottom() - lim::trace_t(lufs, range) * rect.height();
	let (lo, hi) = target.band();
	p.rect_filled(
		Rect::from_x_y_ranges(rect.x_range(), y(hi)..=y(lo)),
		0.0,
		TARGET_GREEN.gamma_multiply(0.10),
	);
	let ty = y(target.lufs());
	p.add(Shape::dashed_line(
		&[pos2(rect.left(), ty), pos2(rect.right(), ty)],
		Stroke::new(1.0, TARGET_GREEN.gamma_multiply(0.6)),
		3.0,
		3.0,
	));
	let cap = l.trace_capacity().max(2);
	let n = l.trace().len();
	let dx = rect.width() / (cap - 1) as f32;
	let pts: Vec<_> = l
		.trace()
		.enumerate()
		.filter(|(_, v)| *v > FLOOR_DB)
		.map(|(i, v)| pos2(rect.right() - (n - 1 - i) as f32 * dx, y(v)))
		.collect();
	if pts.len() >= 2 {
		p.with_clip_rect(rect.shrink(1.0))
			.add(Shape::line(pts, Stroke::new(1.4, theme::TEXT)));
	}
	p.text(
		rect.left_top() + vec2(6.0, 4.0),
		Align2::LEFT_TOP,
		"last 30 s",
		theme::medium(10.0),
		theme::TEXT_FAINT,
	);
	p.text(
		pos2(rect.right() - 6.0, ty - 3.0),
		Align2::RIGHT_BOTTOM,
		lim::minus(format!("{:.0}", target.lufs())),
		theme::mono(10.0),
		TARGET_GREEN,
	);
}

/// Streaming −14 · Loud −9 · Broadcast −23: view state only.
fn target_chips(ui: &mut Ui, cx: &BodyCx, v: &mut LimView, rect: Rect) {
	let gap = 4.0;
	let w = (rect.width() - gap * 2.0) / 3.0;
	for (i, t) in Target::ALL.into_iter().enumerate() {
		let chip = Rect::from_min_size(
			pos2(rect.left() + i as f32 * (w + gap), rect.top()),
			vec2(w, rect.height()),
		);
		let label = t.label();
		let on = v.target == t;
		let resp = ui.interact(
			chip,
			Id::new(("lim-target", cx.salt, cx.r.device, i)),
			Sense::click(),
		);
		resp.widget_info(|| {
			egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, on, &label)
		});
		let p = ui.painter();
		let (bg, fg, stroke) = if on {
			(
				theme::PLAYING_SOFT,
				TARGET_GREEN,
				Stroke::new(1.0, Color32::from_rgb(0x2d, 0x5a, 0x40)),
			)
		} else if resp.hovered() {
			(theme::BG_HOVER, theme::TEXT, Stroke::NONE)
		} else {
			(theme::BG_WIDGET, theme::TEXT_DIM, Stroke::NONE)
		};
		p.rect(
			chip,
			CornerRadius::same(theme::RADIUS_MD),
			bg,
			stroke,
			StrokeKind::Inside,
		);
		p.text(
			chip.center(),
			Align2::CENTER_CENTER,
			&label,
			if on {
				theme::medium(11.5)
			} else {
				theme::small()
			},
			fg,
		);
		if resp
			.on_hover_cursor(CursorIcon::PointingHand)
			.on_hover_text(tr(
				"Loudness target: only moves the green band, never the sound",
			))
			.clicked()
		{
			v.target = t;
		}
	}
}

// --- Knobs, buttons ---------------------------------------------------------------------

fn knob(
	ui: &mut Ui,
	state: &mut AppState,
	cx: &BodyCx,
	cell: Rect,
	(id, label): (&'static str, &'static str),
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
		.color(match id {
			CEILING => theme::LEVEL_OUTPUT,
			RELEASE => theme::GAIN_REDUCTION,
			_ => theme::TEXT,
		})
		.id_salt(Id::new(cx.salt))
		.show(&mut cell_ui, state);
}

const KNOBS: [(&str, &str); 3] = [
	(INPUT_GAIN, mark("Input gain")),
	(CEILING, mark("Ceiling")),
	(RELEASE, mark("Release")),
];

/// A small text button; returns whether it was clicked.
fn chip_button(ui: &mut Ui, text: &str, name: &str, on: bool, tip: &str) -> bool {
	let g = ui
		.painter()
		.layout_no_wrap(text.to_owned(), theme::medium(12.0), theme::TEXT_DIM);
	let (rect, resp) = ui.allocate_exact_size(vec2(g.size().x + 16.0, CHIP_H), Sense::click());
	resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, true, on, name));
	let bg = if on || resp.hovered() {
		theme::BG_HOVER
	} else {
		theme::BG_WIDGET
	};
	ui.painter()
		.rect_filled(rect, CornerRadius::same(theme::RADIUS_MD), bg);
	ui.painter()
		.galley(rect.center() - g.size() * 0.5, g, theme::TEXT_DIM);
	resp.on_hover_cursor(CursorIcon::PointingHand)
		.on_hover_text(tip)
		.clicked()
}

fn reset_button(ui: &mut Ui, v: &mut LimView) {
	if chip_button(
		ui,
		tr("Reset integrated"),
		tr("Reset integrated"),
		false,
		tr("Start the integrated loudness and true peak over (R)"),
	) {
		v.loudness.reset_integrated();
	}
}

fn display_toggle(ui: &mut Ui, v: &mut LimView, more: bool) {
	let arrow = if v.footer { "▾" } else { "▸" };
	let text = format!("{arrow}  {}", if more { tr("More") } else { tr("Display") });
	let name = if more {
		tr("More options")
	} else {
		tr("Display options")
	};
	if chip_button(
		ui,
		&text,
		name,
		v.footer,
		tr("Display options (no effect on the sound)"),
	) {
		v.footer = !v.footer;
	}
}

fn display_settings(ui: &mut Ui, cx: &BodyCx, v: &mut LimView) {
	ui.label(
		egui::RichText::new(tr("History"))
			.font(theme::medium(12.0))
			.color(theme::TEXT_FAINT),
	);
	let selected = HISTORY_SECONDS
		.iter()
		.position(|&s| s == v.seconds)
		.unwrap_or(1);
	if let Some(i) = device_kit::segmented(
		ui,
		Id::new(("lim-seconds", cx.salt, cx.r.device)),
		&["3 s", "6 s", "12 s"],
		selected,
	) {
		let rate = v.rate;
		v.resize(HISTORY_SECONDS[i], rate);
	}
}

/// "Oversampling 1x | 2x | 4x · 5.7 ms latency" (the latency is PDC'd: a reload), read
/// left to right in either layout direction.
fn oversample_selector(ui: &mut Ui, state: &mut AppState, cx: &BodyCx, label: bool) {
	let r = cx.r;
	let current = state
		.param_value(r.track, r.device, lim::OVERSAMPLE)
		.unwrap_or(0.0);
	let selected = lim::oversample_index(current);
	let rate = state
		.probe()
		.map(|p| p.sample_rate() as f32)
		.unwrap_or(48_000.0);
	let rtl = ui.layout().main_dir() == egui::Direction::RightToLeft;
	let mut parts = [0, 1, 2];
	if rtl {
		parts.reverse();
	}
	for part in parts {
		match part {
			0 if label => {
				ui.label(
					egui::RichText::new(tr("Oversampling"))
						.font(theme::medium(12.0))
						.color(theme::TEXT_FAINT),
				);
			}
			1 => {
				if let Some(i) = device_kit::segmented(
					ui,
					Id::new(("lim-oversample", cx.salt, r.device)),
					&lim::OVERSAMPLE_LABELS,
					selected,
				) && i != selected
				{
					commit(
						state,
						r,
						&[(lim::OVERSAMPLE, i as f32)],
						tr("Limiter oversampling"),
					);
				}
			}
			2 => {
				ui.label(
					egui::RichText::new(lim::latency_text(current, rate))
						.font(theme::mono(11.0))
						.color(theme::TEXT_DIM),
				)
				.on_hover_text(tr(
					"2x / 4x run the limiter at a higher rate: tighter true peaks, less aliasing, more CPU \
					 and latency (compensated)",
				));
			}
			_ => {}
		}
	}
}

fn row(ui: &mut Ui, rect: Rect) -> Ui {
	ui.new_child(
		UiBuilder::new()
			.max_rect(rect)
			.layout(Layout::left_to_right(Align::Center)),
	)
}

// --- Dock (16a) -------------------------------------------------------------------------

fn dock(ui: &mut Ui, state: &mut AppState, cx: &BodyCx, v: &mut LimView) {
	let rect = cx.rect;
	let rows = FOOTER_H + if v.footer { SETTINGS_H } else { 0.0 };
	let top = Rect::from_min_max(rect.min, pos2(rect.right(), rect.bottom() - rows - 4.0));
	let narrow = rect.width() < NARROW_BELOW;
	let knobs_w = if narrow { 110.0 } else { KNOBS_W };
	let meters_w = if narrow { 150.0 } else { METERS_W };
	let history_w = (top.width() - meters_w - LOUDNESS_W - knobs_w - 24.0).max(160.0);
	let h = Rect::from_min_size(top.min, vec2(history_w, top.height()));
	history(ui, state, cx, v, h, true);
	let m = Rect::from_min_max(
		pos2(h.right() + 8.0, top.top() + 4.0),
		pos2(h.right() + 8.0 + meters_w, top.bottom() - 8.0),
	);
	meters(ui, cx, v, m);
	let panel = Rect::from_min_max(
		pos2(m.right() + 8.0, top.top()),
		pos2(m.right() + 8.0 + LOUDNESS_W, top.bottom()),
	);
	let line = Stroke::new(1.0, theme::BORDER_SUBTLE);
	ui.painter().vline(panel.left(), panel.y_range(), line);
	ui.painter().vline(panel.right(), panel.y_range(), line);
	loudness_panel(ui, state, cx, v, panel.shrink2(vec2(12.0, 4.0)), true);

	let knobs = Rect::from_min_max(pos2(panel.right() + 8.0, top.top()), top.max);
	if narrow {
		let cell_h = (knobs.height() / 3.0).min(110.0);
		let y0 = knobs.top() + (knobs.height() - cell_h * 3.0) * 0.5;
		for (i, k) in KNOBS.into_iter().enumerate() {
			let cell = Rect::from_min_size(
				pos2(knobs.left(), y0 + cell_h * i as f32),
				vec2(knobs.width(), cell_h),
			);
			knob(ui, state, cx, cell, k, KnobSize::S);
		}
	} else {
		let sizes = [
			(KnobSize::XL, 150.0),
			(KnobSize::L, 118.0),
			(KnobSize::M, 100.0),
		];
		let total: f32 = sizes.iter().map(|s| s.0.diameter() + 30.0).sum();
		let space = (knobs.width() - total) / 4.0;
		let mut x = knobs.left() + space;
		for (k, (size, h)) in KNOBS.into_iter().zip(sizes) {
			let w = size.diameter() + 30.0;
			let cell =
				Rect::from_min_size(pos2(x, knobs.center().y - h * 0.5 - 6.0), vec2(w, h + 12.0));
			knob(ui, state, cx, cell, k, size);
			x += w + space;
		}
	}

	// Footer: the note, Reset integrated, ▸ Display.
	let foot = Rect::from_min_size(
		pos2(rect.left() - 8.0, rect.bottom() - rows),
		vec2(rect.width() + 16.0, FOOTER_H),
	);
	ui.painter().hline(foot.x_range(), foot.top(), line);
	let mut bar = row(ui, foot.shrink2(vec2(10.0, 0.0)));
	bar.spacing_mut().item_spacing.x = 8.0;
	if !narrow {
		bar.label(
			egui::RichText::new(tr(
				"Loudness meters are measured after the ceiling. Target chip only sets the \
				 green band, it never changes the sound.",
			))
			.font(theme::medium(12.0))
			.color(theme::TEXT_DIM),
		);
	}
	bar.with_layout(Layout::right_to_left(Align::Center), |ui| {
		display_toggle(ui, v, false);
		reset_button(ui, v);
		ui.add_space(8.0);
		oversample_selector(ui, state, cx, true);
	});
	if v.footer {
		let settings = Rect::from_min_size(
			pos2(foot.left(), foot.bottom()),
			vec2(foot.width(), SETTINGS_H),
		);
		ui.painter().rect_filled(settings, 0.0, theme::CANVAS);
		ui.painter().hline(settings.x_range(), settings.top(), line);
		let mut r = row(ui, settings.shrink2(vec2(12.0, 0.0)));
		r.spacing_mut().item_spacing.x = 8.0;
		display_settings(&mut r, cx, v);
		r.with_layout(Layout::right_to_left(Align::Center), |ui| {
			ui.label(
				egui::RichText::new(tr("Display only. No effect on the sound."))
					.font(theme::medium(12.0))
					.color(theme::TEXT_FAINT),
			);
		});
	}
}

// --- Compact ----------------------------------------------------------------------------

/// How tall the compact form is at `width` (the inspector allocates it).
pub fn compact_height(width: f32, footer: bool) -> f32 {
	COMPACT_READOUT_H
		+ compact_history_h(width)
		+ COMPACT_KNOBS_H
		+ COMPACT_ROW_H * 2.0
		+ if footer {
			COMPACT_ROW_H * 2.0 + 8.0
		} else {
			0.0
		}
}

fn compact_history_h(width: f32) -> f32 {
	(width * 0.42).clamp(110.0, 180.0)
}

fn compact(ui: &mut Ui, state: &mut AppState, cx: &BodyCx, v: &mut LimView) {
	let rect = cx.rect;
	let mut inner = ui.new_child(
		UiBuilder::new()
			.max_rect(rect)
			.layout(Layout::top_down(Align::Min)),
	);
	egui::ScrollArea::vertical()
		.id_salt(("lim-compact", cx.salt, cx.r.device))
		.auto_shrink([false, false])
		.show(&mut inner, |ui| {
			ui.spacing_mut().item_spacing.y = 0.0;
			let width = ui.available_width();
			let (top, _) = ui.allocate_exact_size(vec2(width, COMPACT_READOUT_H), Sense::hover());
			readouts(ui, state, cx, v, top);
			let (h, _) =
				ui.allocate_exact_size(vec2(width, compact_history_h(width)), Sense::hover());
			history(ui, state, cx, v, h, false);
			let (k, _) = ui.allocate_exact_size(vec2(width, COMPACT_KNOBS_H), Sense::hover());
			let slot = (width / 3.0).min(96.0);
			for (i, knob_) in KNOBS.into_iter().enumerate() {
				let cell = Rect::from_min_size(
					pos2(k.left() + slot * i as f32, k.top() + 8.0),
					vec2(slot, k.height() - 8.0),
				);
				let label = if knob_.0 == INPUT_GAIN {
					(INPUT_GAIN, tr("Gain"))
				} else {
					knob_
				};
				knob(ui, state, cx, cell, label, KnobSize::S);
			}
			let (os, _) = ui.allocate_exact_size(vec2(width, COMPACT_ROW_H), Sense::hover());
			let mut os_row = row(ui, os.shrink2(vec2(2.0, 0.0)));
			os_row.spacing_mut().item_spacing.x = 6.0;
			oversample_selector(&mut os_row, state, cx, false);
			let (foot, _) = ui.allocate_exact_size(vec2(width, COMPACT_ROW_H), Sense::hover());
			ui.painter().hline(
				foot.x_range(),
				foot.top(),
				Stroke::new(1.0, theme::BORDER_SUBTLE),
			);
			let mut bar = row(ui, foot.shrink2(vec2(2.0, 0.0)));
			bar.spacing_mut().item_spacing.x = 6.0;
			display_toggle(&mut bar, v, true);
			bar.with_layout(Layout::right_to_left(Align::Center), |ui| {
				reset_button(ui, v);
			});
			if v.footer {
				let (chips, _) =
					ui.allocate_exact_size(vec2(width, COMPACT_ROW_H + 8.0), Sense::hover());
				target_chips(
					ui,
					cx,
					v,
					Rect::from_min_size(chips.min + vec2(0.0, 6.0), vec2(width, 24.0)),
				);
				let (settings, _) =
					ui.allocate_exact_size(vec2(width, COMPACT_ROW_H), Sense::hover());
				let mut r = row(ui, settings.shrink2(vec2(2.0, 0.0)));
				r.spacing_mut().item_spacing.x = 6.0;
				display_settings(&mut r, cx, v);
			}
		});
}

/// The compact form's top row: GR and short-term loudness, then integrated and true peak.
fn readouts(ui: &mut Ui, state: &AppState, cx: &BodyCx, v: &LimView, rect: Rect) {
	let ceiling = shown_values(state, cx.r).ceiling;
	let l = &v.loudness;
	let p = ui.painter();
	let mut dimmed = p.clone();
	if stopped(state, cx, v) {
		dimmed.multiply_opacity(0.5);
	}
	let half = rect.width() * 0.4;
	for (i, (name, value, unit, color)) in [
		(
			tr("GAIN REDUCTION"),
			lim::gr_text(v.gr_db),
			"dB",
			theme::GAIN_REDUCTION_TEXT,
		),
		(
			tr("LOUDNESS"),
			lim::lufs_text(l.short_term()),
			tr("LUFS short-term"),
			theme::TEXT,
		),
	]
	.into_iter()
	.enumerate()
	{
		let p = if i == 0 { p } else { &dimmed };
		let x = rect.left() + 2.0 + half * i as f32;
		p.text(
			pos2(x, rect.top() + 2.0),
			Align2::LEFT_TOP,
			name,
			theme::medium(10.5),
			theme::TEXT_FAINT,
		);
		let g = p.layout_no_wrap(value, theme::mono_medium(20.0), color);
		let gw = g.size().x;
		p.galley(pos2(x, rect.top() + 16.0), g, color);
		p.text(
			pos2(x + gw + 5.0, rect.top() + 36.0),
			Align2::LEFT_BOTTOM,
			unit,
			theme::medium(11.0),
			theme::TEXT_FAINT,
		);
	}
	let tp = l.true_peak_db();
	let ok = lim::true_peak_ok(tp, ceiling);
	let y = rect.bottom() - 4.0;
	dimmed.text(
		pos2(rect.left() + 2.0, y),
		Align2::LEFT_BOTTOM,
		trf(
			"Integrated {lufs_text} LUFS",
			&[("lufs_text", &lim::lufs_text(l.integrated()))],
		),
		theme::small(),
		theme::TEXT_DIM,
	);
	dimmed.text(
		pos2(rect.right() - 2.0, y),
		Align2::RIGHT_BOTTOM,
		trf(
			"True peak {lufs_text} dBTP{ok}",
			&[
				("lufs_text", &lim::lufs_text(tp)),
				("ok", &if ok { " ✓" } else { "" }),
			],
		),
		theme::small(),
		if ok { TARGET_GREEN } else { theme::TEXT_DIM },
	);
}
