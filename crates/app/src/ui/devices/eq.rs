//! The Parametric EQ editor body (design `15 EQ and Compressor`, 15a dock / 15b compact):
//! the frequency display with the exact response, band fills, pre/post analyzer and
//! seven nodes, the band chips, the selected band's controls and the output gain. The
//! gesture -> parameter mapping lives in [`crate::state::eq_editor`].

use std::sync::{Arc, Mutex};

use cadenza_core::ParamAddress;
use cadenza_dsp::response::EqBands;
use egui::{
	Align, Color32, CornerRadius, CursorIcon, Id, Layout, Rect, Sense, Stroke, Ui, UiBuilder, pos2,
	vec2,
};

use crate::state::AppState;
use crate::state::device_display::{self, AnalyzerState, DbRange, SampleWindow, Tap};
use crate::state::eq_editor::{self as eq, BANDS, Change, EqValues, N_BANDS};
use crate::state::selection::DeviceRef;
use crate::theme;
use crate::ui::device_editor::BodyCx;
use crate::ui::device_kit::{
	self, BandCurve, Bracket, ControlInfo, FreqDisplay, FreqEvent, FreqNode, KnobSize, ParamKnob,
	ReadoutLook, ReadoutState, ValueFmt,
};

/// Wheel notches over a node are committed as one step once the wheel rests this long.
const WHEEL_REST: f64 = 0.5;
const CHIP_H: f32 = 22.0;
const ROW_H: f32 = 33.0;
const FOOTER_H: f32 = 32.0;
const OUTPUT_W: f32 = 112.0;
/// The compact display keeps the design's 344 × 196.
const COMPACT_RATIO: f32 = 196.0 / 344.0;
const COMPACT_KNOBS_H: f32 = 84.0;

/// The band colours: the palette minus sky blue, which reads as the accent.
const COLOR_SLOTS: [usize; N_BANDS] = [0, 1, 2, 3, 4, 6, 7];

pub fn band_color(band: usize) -> Color32 {
	theme::BAND_COLORS[COLOR_SLOTS[band]]
}

/// Wheel notches on a node not committed yet.
#[derive(Debug, Clone, Copy)]
struct WheelEdit {
	band: usize,
	param: &'static str,
	start: f32,
	notches: f32,
	value: f32,
	last: f64,
}

/// View state of one EQ editor (dock or inspector), kept in egui memory.
struct EqView {
	selected: usize,
	range24: bool,
	analyzer: bool,
	pre: bool,
	footer: bool,
	pre_window: SampleWindow,
	post_window: SampleWindow,
	pre_bins: AnalyzerState,
	post_bins: AnalyzerState,
	wheel: Option<WheelEdit>,
}

impl Default for EqView {
	fn default() -> Self {
		Self {
			selected: 3,
			range24: false,
			analyzer: true,
			pre: true,
			footer: false,
			pre_window: SampleWindow::new(device_display::FFT_MAX),
			post_window: SampleWindow::new(device_display::FFT_MAX),
			pre_bins: AnalyzerState::default(),
			post_bins: AnalyzerState::default(),
			wheel: None,
		}
	}
}

type Shared = Arc<Mutex<EqView>>;

fn view_id(salt: &str, r: DeviceRef) -> Id {
	Id::new(("eq-editor", salt, r.device))
}

fn view(ctx: &egui::Context, id: Id) -> Shared {
	ctx.data_mut(|d| {
		d.get_temp_mut_or_insert_with(id, || Arc::new(Mutex::new(EqView::default())))
			.clone()
	})
}

fn lock(view: &Shared) -> std::sync::MutexGuard<'_, EqView> {
	view.lock().unwrap_or_else(|e| e.into_inner())
}

/// Shows `pre` / `post` analyzer bins in an editor as if the probe had measured them
/// (screenshots).
pub fn set_analyzer(ctx: &egui::Context, salt: &str, r: DeviceRef, pre: &[f32], post: &[f32]) {
	let v = view(ctx, view_id(salt, r));
	let mut v = lock(&v);
	v.pre_bins.bins = pre.to_vec();
	v.post_bins.bins = post.to_vec();
}

/// The selected band of an editor.
pub fn selected_band(ctx: &egui::Context, salt: &str, r: DeviceRef) -> usize {
	lock(&view(ctx, view_id(salt, r))).selected
}

/// Keys while the dock editor has focus: 1-7 select a band, Del puts it to sleep.
pub fn shortcuts(ctx: &egui::Context, state: &mut AppState, r: DeviceRef) {
	let v = view(ctx, view_id(crate::ui::device_editor::DOCK, r));
	let mut v = lock(&v);
	let keys = [
		egui::Key::Num1,
		egui::Key::Num2,
		egui::Key::Num3,
		egui::Key::Num4,
		egui::Key::Num5,
		egui::Key::Num6,
		egui::Key::Num7,
	];
	let pressed = |k: egui::Key| ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, k));
	for (band, key) in keys.into_iter().enumerate() {
		if pressed(key) {
			v.selected = band;
		}
	}
	if pressed(egui::Key::Delete) || pressed(egui::Key::Backspace) {
		let band = v.selected;
		commit(
			state,
			r,
			&eq::sleep(band),
			&format!("{} off", BANDS[band].name),
		);
	}
}

/// The EQ's parameter values as shown (previews, automation, model).
fn shown_values(state: &AppState, r: DeviceRef) -> EqValues {
	EqValues::from_fn(|p| {
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
		.or_else(|| eq::info().param(id).map(|p| p.default))
		.unwrap_or(0.0)
}

fn differs(a: f32, b: f32) -> bool {
	(a - b).abs() > 1e-4 * a.abs().max(1.0)
}

/// The changes that differ from the document, committed as one undo step.
fn commit(state: &mut AppState, r: DeviceRef, changes: &[Change], label: &str) {
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

/// A band change with its label naming what moved ("Peak 2 freq and gain").
fn commit_band(state: &mut AppState, r: DeviceRef, band: usize, changes: &[Change]) {
	let moved: Vec<Change> = changes
		.iter()
		.copied()
		.filter(|(id, v)| differs(model_value(state, r, id), *v))
		.collect();
	commit(state, r, changes, &eq::label(band, &moved));
}

pub fn eq_body(ui: &mut Ui, state: &mut AppState, cx: &BodyCx) {
	let r = cx.r;
	let shared = view(ui.ctx(), view_id(cx.salt, r));
	let mut v = lock(&shared);
	update_analyzer(ui, state, cx, &mut v);
	finish_wheel(ui, state, r, &mut v);
	if cx.compact {
		compact(ui, state, cx, &mut v);
	} else {
		dock(ui, state, cx, &mut v);
	}
}

fn update_analyzer(ui: &Ui, state: &AppState, cx: &BodyCx, v: &mut EqView) {
	let dt = ui.input(|i| i.stable_dt).min(0.25);
	let ours = state.probe_target() == Some(cx.r.device);
	if v.analyzer && ours && !cx.frames.is_empty() {
		let rate = state.probe().map_or(48_000.0, |p| p.sample_rate() as f32);
		v.post_window.push_frames(cx.frames, Tap::Output);
		let bins = device_display::spectrum_bins(v.post_window.samples(), rate);
		v.post_bins.update(&bins, dt);
		if v.pre {
			v.pre_window.push_frames(cx.frames, Tap::Input);
			let bins = device_display::spectrum_bins(v.pre_window.samples(), rate);
			v.pre_bins.update(&bins, dt);
		} else {
			v.pre_bins.decay(dt);
		}
	} else {
		v.pre_bins.decay(dt);
		v.post_bins.decay(dt);
	}
	if !v.pre_bins.is_empty() || !v.post_bins.is_empty() {
		ui.ctx().request_repaint();
	}
}

/// Commits wheel notches once the wheel rests.
fn finish_wheel(ui: &Ui, state: &mut AppState, r: DeviceRef, v: &mut EqView) {
	let Some(w) = v.wheel else {
		return;
	};
	let now = ui.input(|i| i.time);
	let idle = now - w.last;
	if idle >= WHEEL_REST {
		v.wheel = None;
		commit_band(state, r, w.band, &[(w.param, w.value)]);
	} else {
		ui.ctx()
			.request_repaint_after(std::time::Duration::from_secs_f64(WHEEL_REST - idle));
	}
}

fn wheel(ui: &Ui, state: &mut AppState, r: DeviceRef, v: &mut EqView, band: usize, notches: f32) {
	let Some(param) = eq::wheel_param(band) else {
		return;
	};
	let now = ui.input(|i| i.time);
	if let Some(w) = v.wheel.filter(|w| w.band != band) {
		v.wheel = None;
		commit_band(state, r, w.band, &[(w.param, w.value)]);
	}
	let start = state
		.param_value(r.track, r.device, param)
		.unwrap_or_else(|| model_value(state, r, param));
	let w = v.wheel.get_or_insert(WheelEdit {
		band,
		param,
		start,
		notches: 0.0,
		value: start,
		last: now,
	});
	w.notches += notches;
	w.last = now;
	if let Some(value) = eq::wheel_value(band, w.start, w.notches) {
		w.value = value;
		state.preview_param(r.device, param, value);
	}
	ui.ctx()
		.request_repaint_after(std::time::Duration::from_secs_f64(WHEEL_REST));
}

/// The frequency display and everything its events do.
fn display(ui: &mut Ui, state: &mut AppState, cx: &BodyCx, v: &mut EqView, rect: Rect) {
	let r = cx.r;
	let values = shown_values(state, r);
	let rate = state.probe().map_or(48_000.0, |p| p.sample_rate() as f32);
	let bands = EqBands::new(values.as_slice(), rate);
	let bands = &bands;
	let dormant: Vec<bool> = (0..N_BANDS).map(|b| eq::is_dormant(&values, b)).collect();
	let responses: Vec<Box<dyn Fn(f32) -> f32 + '_>> = (0..N_BANDS)
		.map(|b| Box::new(move |f| bands.band_response_db(b, f)) as Box<dyn Fn(f32) -> f32>)
		.collect();
	let fills: Vec<BandCurve> = (0..N_BANDS)
		.filter(|&b| !dormant[b])
		.map(|b| BandCurve {
			response: &*responses[b],
			color: band_color(b),
			selected: b == v.selected,
		})
		.collect();
	let nodes: Vec<FreqNode> = (0..N_BANDS)
		.map(|b| {
			let (freq, db) = eq::node(&values, b);
			FreqNode {
				label: BANDS[b].label,
				freq,
				db,
				color: band_color(b),
				dormant: dormant[b],
				x_only: BANDS[b].kind.is_pass(),
				ai: BANDS[b]
					.params()
					.any(|p| state.ai_change(r.device, p).is_some()),
			}
		})
		.collect();
	let sum = |f: f32| bands.response_db(f);
	let changes = state.ai_changes_of(r.device);
	let before_bands = (!changes.is_empty()).then(|| {
		let before = EqValues::from_fn(|p| {
			changes
				.iter()
				.find(|c| c.param == p.id)
				.map_or_else(|| values.get(p.id), |c| c.before)
		});
		EqBands::new(before.as_slice(), rate)
	});
	let before_curve = before_bands
		.as_ref()
		.map(|b| move |f: f32| b.response_db(f));
	let grab = |freq: f32| Some(eq::nearest_band(&values, freq));
	let sel = &BANDS[v.selected];
	let bracket = sel
		.q
		.filter(|_| !cx.compact && !dormant[v.selected])
		.map(|q| {
			let (freq, db) = eq::node(&values, v.selected);
			let (low, high) = eq::q_bracket(freq, values.get(q));
			Bracket {
				low,
				high,
				db: db * 0.5,
				color: band_color(v.selected),
			}
		});
	let show_pre = v.analyzer && v.pre && !v.pre_bins.is_empty();
	let show_post = v.analyzer && !v.post_bins.is_empty();
	let mut child = ui.new_child(UiBuilder::new().max_rect(rect));
	let out = FreqDisplay::new(Id::new(("eq-display", cx.salt, r.device)), rect.size())
		.range(if v.range24 {
			DbRange::PM24
		} else {
			DbRange::PM12
		})
		.pre(show_pre.then_some(v.pre_bins.bins.as_slice()))
		.post(show_post.then_some(v.post_bins.bins.as_slice()))
		.bands(&fills)
		.before(before_curve.as_ref().map(|f| f as &dyn Fn(f32) -> f32))
		.sum(&sum, theme::TEXT)
		.nodes(&nodes)
		.selected(Some(v.selected))
		.piano(true)
		.curve_grab(&grab)
		.bracket(bracket);
	let out = if cx.compact { out.small_nodes() } else { out }.show(&mut child);
	for e in out.events {
		match e {
			FreqEvent::Move {
				node,
				freq,
				db,
				commit: done,
			} => {
				v.selected = node;
				let changes = eq::move_node(node, freq, db);
				if done {
					commit_band(state, r, node, &changes);
				} else {
					for (id, value) in changes {
						state.preview_param(r.device, id, value);
					}
				}
			}
			FreqEvent::Click { node } => v.selected = node,
			FreqEvent::Reset { node } => {
				v.selected = node;
				commit_band(state, r, node, &eq::reset_node(&values, node));
			}
			FreqEvent::Wheel { node, notches } => {
				v.selected = node;
				wheel(ui, state, r, v, node, notches);
			}
			FreqEvent::Create { freq, db } => {
				if let Some(band) = eq::nearest_dormant(&values, freq) {
					v.selected = band;
					commit(
						state,
						r,
						&eq::wake(band, freq, db),
						&format!("Wake {}", BANDS[band].name),
					);
				}
			}
		}
	}
	for (band, resp) in out.node_responses.iter().enumerate() {
		node_menu(resp, state, r, band, &mut v.selected);
	}
	out.response.on_hover_text_at_pointer(
		"Drag a node (Shift fine, Alt one axis, Ctrl gain only) · wheel: Q / slope · \
		 double-click a node to reset, empty space to wake a band",
	);
}

/// Right-click on a node: automate its params, reset the band.
fn node_menu(
	resp: &egui::Response,
	state: &mut AppState,
	r: DeviceRef,
	band: usize,
	selected: &mut usize,
) {
	let b = &BANDS[band];
	resp.context_menu(|ui| {
		*selected = band;
		ui.label(
			egui::RichText::new(b.name)
				.font(theme::semibold(12.0))
				.color(band_color(band)),
		);
		for id in [b.gain, Some(b.freq), b.q, b.slope].into_iter().flatten() {
			let address = ParamAddress::device(r.track, r.device, id);
			let word = eq::param_word(id);
			let text = if state.is_automated(&address) {
				format!("Show {word} automation")
			} else {
				format!("Automate {word}")
			};
			if ui.button(text).clicked() {
				state.automate(address);
				ui.close();
			}
		}
		ui.separator();
		if ui.button("Reset band").clicked() {
			commit(
				state,
				r,
				&eq::reset_band(band),
				&format!("Reset {}", b.name),
			);
			ui.close();
		}
	});
}

fn child(ui: &mut Ui, rect: Rect) -> Ui {
	ui.new_child(
		UiBuilder::new()
			.max_rect(rect)
			.layout(Layout::left_to_right(Align::Center)),
	)
}

/// 15a: display + output column over the band row (and the analyzer footer).
fn dock(ui: &mut Ui, state: &mut AppState, cx: &BodyCx, v: &mut EqView) {
	let rect = cx.rect;
	let rows = ROW_H + if v.footer { FOOTER_H } else { 0.0 };
	let top = Rect::from_min_max(rect.min, pos2(rect.right(), rect.bottom() - rows - 4.0));
	let display_rect = top.with_max_x(top.right() - OUTPUT_W - 8.0);
	display(ui, state, cx, v, display_rect);

	let column = top.with_min_x(display_rect.right() + 8.0);
	let mut out = ui.new_child(
		UiBuilder::new()
			.max_rect(column)
			.layout(Layout::top_down(Align::Center)),
	);
	out.add_space((column.height() - 128.0).max(0.0) * 0.5);
	out.label(
		egui::RichText::new("OUTPUT")
			.font(theme::medium(10.5))
			.color(theme::TEXT_FAINT),
	);
	out.add_space(6.0);
	ParamKnob::new(cx.r, eq::OUTPUT)
		.size(KnobSize::L)
		.label("Gain")
		.id_salt(Id::new(cx.salt))
		.show(&mut out, state);

	let band_row = Rect::from_min_size(
		pos2(rect.left() - 8.0, rect.bottom() - rows),
		vec2(rect.width() + 16.0, ROW_H),
	);
	ui.painter().hline(
		band_row.x_range(),
		band_row.top(),
		Stroke::new(1.0, theme::BORDER_SUBTLE),
	);
	let mut row = child(ui, band_row.shrink2(vec2(8.0, 0.0)));
	row.spacing_mut().item_spacing.x = 6.0;
	chips(&mut row, state, cx.r, v, None);
	divider(&mut row);
	band_controls(&mut row, state, cx, v);
	row.with_layout(Layout::right_to_left(Align::Center), |ui| {
		ui.spacing_mut().item_spacing.x = 6.0;
		footer_toggle(ui, v);
		range_selector(ui, cx, v);
		ui.label(
			egui::RichText::new("Range")
				.font(theme::medium(12.0))
				.color(theme::TEXT_FAINT),
		);
	});
	if v.footer {
		let foot = Rect::from_min_size(
			pos2(band_row.left(), band_row.bottom()),
			vec2(band_row.width(), FOOTER_H),
		);
		ui.painter().rect_filled(foot, 0.0, theme::CANVAS);
		ui.painter().hline(
			foot.x_range(),
			foot.top(),
			Stroke::new(1.0, theme::BORDER_SUBTLE),
		);
		let mut row = child(ui, foot.shrink2(vec2(12.0, 0.0)));
		row.spacing_mut().item_spacing.x = 10.0;
		analyzer_toggles(&mut row, v);
		row.with_layout(Layout::right_to_left(Align::Center), |ui| {
			ui.label(
				egui::RichText::new("Display only. No effect on the sound.")
					.font(theme::medium(12.0))
					.color(theme::TEXT_FAINT),
			);
		});
	}
}

/// How tall the compact form is at `width` (the inspector allocates it).
pub fn compact_height(width: f32, footer: bool) -> f32 {
	display_height(width, f32::INFINITY) + compact_rest(footer)
}

/// Everything of the compact form below the display.
fn compact_rest(footer: bool) -> f32 {
	8.0 + 24.0 + 8.0 + 20.0 + COMPACT_KNOBS_H + 30.0 + if footer { 30.0 } else { 0.0 }
}

/// The design's 344 × 196 ratio, shrunk (to 110 px) when the room is short.
fn display_height(width: f32, room: f32) -> f32 {
	(width * COMPACT_RATIO).min(room).clamp(110.0, 260.0)
}

/// Whether the compact editor of `r` in `salt` shows its footer.
pub fn footer_open(ctx: &egui::Context, salt: &str, r: DeviceRef) -> bool {
	lock(&view(ctx, view_id(salt, r))).footer
}

/// 15b: the display at full width, chips, the selected band as S knobs, the rest
/// behind ▸.
fn compact(ui: &mut Ui, state: &mut AppState, cx: &BodyCx, v: &mut EqView) {
	let rect = cx.rect;
	let mut inner = ui.new_child(
		UiBuilder::new()
			.max_rect(rect)
			.layout(Layout::top_down(Align::Min)),
	);
	egui::ScrollArea::vertical()
		.id_salt(("eq-compact", cx.salt, cx.r.device))
		.auto_shrink([false, false])
		.show(&mut inner, |ui| {
			ui.spacing_mut().item_spacing.y = 0.0;
			let width = ui.available_width();
			let room = rect.height() - compact_rest(v.footer);
			let (display_rect, _) =
				ui.allocate_exact_size(vec2(width, display_height(width, room)), Sense::hover());
			display(ui, state, cx, v, display_rect);
			ui.add_space(8.0);
			let (chip_rect, _) = ui.allocate_exact_size(vec2(width, 24.0), Sense::hover());
			let mut row = child(ui, chip_rect);
			row.spacing_mut().item_spacing.x = 3.0;
			chips(&mut row, state, cx.r, v, Some(width));
			ui.add_space(8.0);
			let band = v.selected;
			let (name_rect, _) = ui.allocate_exact_size(vec2(width, 20.0), Sense::hover());
			let mut row = child(ui, name_rect.shrink2(vec2(2.0, 0.0)));
			row.spacing_mut().item_spacing.x = 6.0;
			row.label(
				egui::RichText::new(BANDS[band].name)
					.font(theme::semibold(12.0))
					.color(band_color(band)),
			);
			let freq = state
				.param_value(cx.r.track, cx.r.device, BANDS[band].freq)
				.unwrap_or_else(|| model_value(state, cx.r, BANDS[band].freq));
			row.label(
				egui::RichText::new(note_text(freq))
					.font(theme::mono(11.0))
					.color(theme::TEXT_FAINT),
			);
			let (knob_rect, _) =
				ui.allocate_exact_size(vec2(width, COMPACT_KNOBS_H), Sense::hover());
			compact_knobs(ui, state, cx, band, knob_rect);
			let (foot, _) = ui.allocate_exact_size(vec2(width, 30.0), Sense::hover());
			ui.painter().hline(
				foot.x_range(),
				foot.top(),
				Stroke::new(1.0, theme::BORDER_SUBTLE),
			);
			let mut row = child(ui, foot.shrink2(vec2(2.0, 0.0)));
			row.spacing_mut().item_spacing.x = 6.0;
			footer_toggle(&mut row, v);
			row.with_layout(Layout::right_to_left(Align::Center), |ui| {
				let text = if v.range24 { "±24 dB" } else { "±12 dB" };
				let resp = ui
					.add(
						egui::Label::new(
							egui::RichText::new(text)
								.font(theme::mono(11.0))
								.color(theme::TEXT_FAINT),
						)
						.sense(Sense::click()),
					)
					.on_hover_cursor(CursorIcon::PointingHand)
					.on_hover_text("Display range");
				if resp.clicked() {
					v.range24 = !v.range24;
				}
			});
			if v.footer {
				let (foot, _) = ui.allocate_exact_size(vec2(width, 30.0), Sense::hover());
				let mut row = child(ui, foot.shrink2(vec2(2.0, 0.0)));
				row.spacing_mut().item_spacing.x = 8.0;
				range_selector(&mut row, cx, v);
				analyzer_toggles(&mut row, v);
			}
		});
}

/// The selected band's controls as S knobs (Freq, Gain, Q or Slope) and the output.
fn compact_knobs(ui: &mut Ui, state: &mut AppState, cx: &BodyCx, band: usize, rect: Rect) {
	let b = &BANDS[band];
	let color = band_color(band);
	let mut knobs: Vec<(&'static str, &'static str, Color32)> = vec![(b.freq, "Freq", color)];
	if let Some(g) = b.gain {
		knobs.push((g, "Gain", color));
	}
	if let Some(q) = b.q {
		knobs.push((q, "Q", color));
	}
	if let Some(s) = b.slope {
		knobs.push((s, "Slope", color));
	}
	knobs.push((eq::OUTPUT, "Output", theme::TEXT));
	let slot = rect.width() / knobs.len() as f32;
	for (i, (id, label, color)) in knobs.into_iter().enumerate() {
		let cell = Rect::from_min_size(
			pos2(rect.left() + slot * i as f32, rect.top() + 6.0),
			vec2(slot, rect.height() - 6.0),
		);
		let mut cell_ui = ui.new_child(
			UiBuilder::new()
				.max_rect(cell)
				.layout(Layout::top_down(Align::Center)),
		);
		let mut knob = ParamKnob::new(cx.r, id)
			.size(KnobSize::S)
			.label(label)
			.color(color)
			.id_salt(Id::new(cx.salt));
		if label == "Slope" {
			knob = knob.choices(&eq::SLOPES);
		}
		knob.show(&mut cell_ui, state);
	}
}

fn note_text(freq: f32) -> String {
	let (name, cents) = device_display::freq_to_note(freq);
	match cents {
		0 => name,
		c if c > 0 => format!("{name} +{c}c"),
		c => format!("{name} -{}c", -c),
	}
}

/// HP LS 1 2 3 HS LP: click selects; dormant bands are dim with a hollow dot. With
/// `width` the chips share it equally (compact).
fn chips(ui: &mut Ui, state: &AppState, r: DeviceRef, v: &mut EqView, width: Option<f32>) {
	let values = shown_values(state, r);
	let gap = ui.spacing().item_spacing.x;
	for (band, b) in BANDS.iter().enumerate() {
		let awake = !eq::is_dormant(&values, band);
		let font = theme::medium(if width.is_some() { 11.0 } else { 12.0 });
		let fg = if awake {
			theme::TEXT
		} else {
			theme::TEXT_FAINT
		};
		let galley = ui.painter().layout_no_wrap(b.label.to_owned(), font, fg);
		let size = match width {
			Some(w) => vec2((w - gap * (N_BANDS - 1) as f32) / N_BANDS as f32, 24.0),
			None => vec2(galley.size().x + 30.0, CHIP_H),
		};
		let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
		resp.widget_info(|| {
			egui::WidgetInfo::selected(
				egui::WidgetType::SelectableLabel,
				true,
				v.selected == band,
				b.name,
			)
		});
		let p = ui.painter();
		let bg = if v.selected == band {
			theme::BG_HOVER
		} else if resp.hovered() {
			theme::BG_WIDGET
		} else {
			theme::BG_RAISED
		};
		p.rect_filled(rect, CornerRadius::same(theme::RADIUS_MD), bg);
		let color = band_color(band);
		let dot_r = if width.is_some() { 3.5 } else { 4.0 };
		let content = dot_r * 2.0 + 5.0 + galley.size().x;
		let left = rect.center().x - content * 0.5;
		let dot = pos2(left + dot_r, rect.center().y);
		if awake {
			p.circle_filled(dot, dot_r, color);
		} else {
			p.circle_stroke(dot, dot_r - 0.75, Stroke::new(1.5, color));
		}
		p.galley(
			pos2(
				left + dot_r * 2.0 + 5.0,
				rect.center().y - galley.size().y * 0.5,
			),
			galley,
			fg,
		);
		let resp = resp
			.on_hover_cursor(CursorIcon::PointingHand)
			.on_hover_text(format!("{} · key {}", b.name, band + 1));
		if resp.clicked() {
			v.selected = band;
		}
	}
}

fn divider(ui: &mut Ui) {
	let (rect, _) = ui.allocate_exact_size(vec2(13.0, 18.0), Sense::hover());
	ui.painter().vline(
		rect.center().x,
		rect.y_range(),
		Stroke::new(1.0, theme::BORDER),
	);
}

fn dim_label(ui: &mut Ui, text: &str) {
	ui.label(
		egui::RichText::new(text)
			.font(theme::medium(12.0))
			.color(theme::TEXT_FAINT),
	);
}

/// The dock's selected band: name, Freq (+ note), Gain, Q or the slope, all typable.
fn band_controls(ui: &mut Ui, state: &mut AppState, cx: &BodyCx, v: &mut EqView) {
	let band = v.selected;
	let b = &BANDS[band];
	ui.label(
		egui::RichText::new(b.name)
			.font(theme::semibold(12.0))
			.color(band_color(band)),
	);
	ui.add_space(4.0);
	dim_label(ui, "Freq");
	let freq = value_box(ui, state, cx, band, b.freq, 78.0);
	ui.label(
		egui::RichText::new(note_text(freq))
			.font(theme::mono(11.0))
			.color(theme::TEXT_FAINT),
	);
	if let Some(g) = b.gain {
		ui.add_space(4.0);
		dim_label(ui, "Gain");
		value_box(ui, state, cx, band, g, 76.0);
	}
	if let Some(q) = b.q {
		ui.add_space(4.0);
		dim_label(ui, "Q");
		value_box(ui, state, cx, band, q, 56.0);
	}
	if let Some(s) = b.slope {
		ui.add_space(4.0);
		dim_label(ui, "Slope");
		let value = state
			.param_value(cx.r.track, cx.r.device, s)
			.unwrap_or_else(|| model_value(state, cx.r, s));
		let selected = eq::slope_index(value);
		if let Some(i) = device_kit::segmented(
			ui,
			Id::new(("eq-slope", cx.salt, cx.r.device, band)),
			&eq::SLOPES,
			selected,
		) && i != selected
		{
			commit_band(state, cx.r, band, &[(s, i as f32)]);
		}
	}
}

/// A boxed, typable readout of one band param; returns the shown value.
fn value_box(
	ui: &mut Ui,
	state: &mut AppState,
	cx: &BodyCx,
	band: usize,
	id: &'static str,
	width: f32,
) -> f32 {
	let r = cx.r;
	let Some(info) = eq::info().param(id) else {
		return 0.0;
	};
	let value = state
		.param_value(r.track, r.device, id)
		.unwrap_or(info.default);
	let address = ParamAddress::device(r.track, r.device, id);
	let shown = if state.is_automated(&address) {
		ReadoutState::Automated
	} else if state.ai_change(r.device, id).is_some() {
		ReadoutState::Ai
	} else {
		ReadoutState::Rest
	};
	let (typed, resp) = device_kit::readout(
		ui,
		Id::new(("eq-readout", cx.salt, r.device, id)),
		value,
		info.min,
		info.max,
		ValueFmt::unit(info.unit),
		ReadoutLook::Boxed,
		vec2(width, CHIP_H),
		shown,
	);
	let menu = device_kit::control_menu(
		&resp,
		state,
		address,
		ControlInfo {
			value,
			min: info.min,
			max: info.max,
			default: info.default,
			fmt: ValueFmt::unit(info.unit),
		},
	);
	if let Some(v) = typed.or(menu) {
		commit_band(state, r, band, &[(id, v.clamp(info.min, info.max))]);
	}
	value
}

fn range_selector(ui: &mut Ui, cx: &BodyCx, v: &mut EqView) {
	let selected = usize::from(v.range24);
	if let Some(i) = device_kit::segmented(
		ui,
		Id::new(("eq-range", cx.salt, cx.r.device)),
		&["±12", "±24"],
		selected,
	) {
		v.range24 = i == 1;
	}
}

/// "▸ Analyzer and display": opens the footer.
fn footer_toggle(ui: &mut Ui, v: &mut EqView) {
	let arrow = if v.footer { "▾" } else { "▸" };
	let font = theme::medium(12.0);
	let galley = ui.painter().layout_no_wrap(
		format!("{arrow}  Analyzer and display"),
		font,
		theme::TEXT_DIM,
	);
	let (rect, resp) = ui.allocate_exact_size(vec2(galley.size().x + 16.0, CHIP_H), Sense::click());
	resp.widget_info(|| {
		egui::WidgetInfo::selected(
			egui::WidgetType::Checkbox,
			true,
			v.footer,
			"Analyzer and display",
		)
	});
	let bg = if v.footer || resp.hovered() {
		theme::BG_HOVER
	} else {
		theme::BG_WIDGET
	};
	ui.painter()
		.rect_filled(rect, CornerRadius::same(theme::RADIUS_MD), bg);
	ui.painter()
		.galley(rect.center() - galley.size() * 0.5, galley, theme::TEXT_DIM);
	if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
		v.footer = !v.footer;
	}
}

fn analyzer_toggles(ui: &mut Ui, v: &mut EqView) {
	if device_kit::toggle(ui, v.analyzer, "Analyzer", theme::ACCENT)
		.on_hover_text("The spectrum after the EQ (needs the transport running)")
		.clicked()
	{
		v.analyzer = !v.analyzer;
	}
	if device_kit::toggle(ui, v.pre, "Pre", theme::TEXT_DIM)
		.on_hover_text("Also the spectrum before the EQ")
		.clicked()
	{
		v.pre = !v.pre;
	}
}
