//! The Parametric EQ editor body (design `15 EQ and Compressor`, 15a dock / 15b compact):
//! the frequency display with the exact response, band fills, pre/post analyzer and
//! seven nodes, the band chips, the selected band's controls and the output gain. The
//! gesture -> parameter mapping lives in [`crate::state::eq_editor`].

use crate::i18n::{mark, tr, tr_str, trf};
use std::sync::{Arc, Mutex};

use cadenza_core::ParamAddress;
use cadenza_dsp::response::EqBands;
use cadenza_dsp::{Listen, readings};
use egui::{
	Align, Color32, CornerRadius, CursorIcon, Id, Layout, Rect, Sense, Stroke, StrokeKind, Ui,
	UiBuilder, pos2, vec2,
};

use crate::state::AppState;
use crate::state::device_display::{self, AnalyzerState, DbRange, SampleWindow, Tap};
use crate::state::eq_editor::{self as eq, BANDS, Change, EqValues, N_BANDS};
use crate::state::match_eq::MatchPhase;
use crate::state::selection::DeviceRef;
use crate::theme;
use crate::ui::device_editor::BodyCx;
use crate::ui::device_kit::{
	self, BandCurve, Bracket, ControlInfo, FreqDisplay, FreqEvent, FreqNode, KnobSize, ParamKnob,
	ReadoutLook, ReadoutState, ValueFmt,
};
use cadenza_mcp::tools::match_eq::MatchReference;

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
/// How much of their colour the other bands keep while one is soloed.
const SOLO_DIM: f32 = 0.3;

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
	/// The analyzer shows the side instead of the mid (mono sum).
	side: bool,
	footer: bool,
	pre_window: SampleWindow,
	post_window: SampleWindow,
	pre_bins: AnalyzerState,
	post_bins: AnalyzerState,
	wheel: Option<WheelEdit>,
	/// Band gain readings shown instead of the probe's (screenshots).
	synthetic: Option<[f32; readings::MAX]>,
	/// Octave band of the collision strip under the pointer (last frame).
	collision_hover: Option<usize>,
}

impl Default for EqView {
	fn default() -> Self {
		Self {
			selected: 3,
			range24: false,
			analyzer: true,
			pre: true,
			side: false,
			footer: false,
			pre_window: SampleWindow::new(device_display::FFT_MAX),
			post_window: SampleWindow::new(device_display::FFT_MAX),
			pre_bins: AnalyzerState::default(),
			post_bins: AnalyzerState::default(),
			wheel: None,
			synthetic: None,
			collision_hover: None,
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

/// Shows `gains` (dB per band, [`N_BANDS`] order) as the bands' live gains, as if the
/// probe had read them (screenshots).
pub fn set_live_gains(ctx: &egui::Context, salt: &str, r: DeviceRef, gains: &[f32]) {
	let v = view(ctx, view_id(salt, r));
	let mut v = lock(&v);
	let mut out = [0.0; readings::MAX];
	let start = readings::EQ_BAND_GAIN_DB_START;
	for (o, g) in out[start..].iter_mut().zip(gains) {
		*o = *g;
	}
	v.synthetic = Some(out);
}

/// The device's band gain readings while it plays (`None`: idle or not probed).
fn live_readings(state: &AppState, cx: &BodyCx, v: &EqView) -> Option<[f32; readings::MAX]> {
	if v.synthetic.is_some() {
		return v.synthetic;
	}
	let ours = state.probe_target() == Some(cx.r.device);
	let probe = state.probe().filter(|_| ours)?;
	if !state.is_playing() && cx.frames.is_empty() {
		return None;
	}
	let (values, n) = probe.readings();
	(n >= readings::EQ_BAND_GAIN_DB_START + readings::EQ_BANDS).then_some(values)
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
	// ← → the band, ↑ ↓ its gain (filters: their frequency).
	let keys = crate::ui::device_editor::arrows(ctx);
	v.selected = (v.selected as i32 + keys.h).clamp(0, N_BANDS as i32 - 1) as usize;
	if keys.v != 0 {
		let band = &BANDS[v.selected];
		let param = band.gain.unwrap_or(band.freq);
		drop(v);
		crate::ui::device_editor::nudge_param(state, r, param, keys.v as f32, keys.shift);
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
	if let Some(mode) = eq::listen_follow(state.listen_mode(r.device), v.selected) {
		state.set_listen(r.device, mode);
	}
}

/// "🎧 Listen": hear the selected band isolated (monitoring only).
fn listen_toggle(ui: &mut Ui, state: &mut AppState, r: DeviceRef, band: usize) {
	let on = state.listen_mode(r.device) == Listen::Band(band as u8);
	let resp = device_kit::glyph_toggle(
		ui,
		on,
		device_kit::Glyph::Headphones,
		tr("Listen"),
		theme::SOLO,
	)
	.on_hover_text(tr(
		"Hear only this band of the input (Alt+click a node). Monitoring only: not saved, never rendered",
	));
	if resp.clicked() {
		state.set_listen(
			r.device,
			eq::toggle_listen(state.listen_mode(r.device), band),
		);
	}
}

fn update_analyzer(ui: &Ui, state: &AppState, cx: &BodyCx, v: &mut EqView) {
	let dt = ui.input(|i| i.stable_dt).min(0.25);
	let ours = state.probe_target() == Some(cx.r.device);
	if v.analyzer && ours && !cx.frames.is_empty() {
		let rate = state.probe().map_or(48_000.0, |p| p.sample_rate() as f32);
		v.post_window.push_channel(cx.frames, Tap::Output, v.side);
		let bins = device_display::spectrum_bins(v.post_window.samples(), rate);
		v.post_bins.update(&bins, dt);
		if v.pre {
			v.pre_window.push_channel(cx.frames, Tap::Input, v.side);
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
	// Dynamic bands: the live curve (static + each band's movement) over the static one.
	let live = live_readings(state, cx, v);
	let offsets = eq::live_offsets(&values, live.as_ref().map(|r| &r[..]));
	let live_bands = EqBands::with_offsets(values.as_slice(), rate, &offsets);
	let moving = offsets.iter().any(|o| o.abs() > 0.05);
	let static_sum = |f: f32| bands.response_db(f);
	let dormant: Vec<bool> = (0..N_BANDS).map(|b| eq::is_dormant(&values, b)).collect();
	let responses: Vec<Box<dyn Fn(f32) -> f32 + '_>> = (0..N_BANDS)
		.map(|b| Box::new(move |f| bands.band_response_db(b, f)) as Box<dyn Fn(f32) -> f32>)
		.collect();
	// While a band is soloed (Listen), the other bands' curves step back.
	let soloed = match state.listen_mode(r.device) {
		Listen::Band(b) => Some(usize::from(b)),
		_ => None,
	};
	let fills: Vec<BandCurve> = (0..N_BANDS)
		.filter(|&b| !dormant[b])
		.map(|b| BandCurve {
			response: &*responses[b],
			color: if soloed.is_some_and(|s| s != b) {
				band_color(b).gamma_multiply(SOLO_DIM)
			} else {
				band_color(b)
			},
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
				dynamic: eq::is_dynamic(&values, b).then(|| {
					let activity = live.map_or(0.0, |_| eq::activity(&values, b, offsets[b]));
					(db + offsets[b], activity)
				}),
			}
		})
		.collect();
	let split = eq::uses_mid_side(&values);
	let sum = |f: f32| {
		if split {
			live_bands.channel_response_db(false, f)
		} else {
			live_bands.response_db(f)
		}
	};
	let side_sum = |f: f32| live_bands.channel_response_db(true, f);
	let badges: Vec<Option<&str>> = (0..N_BANDS).map(|b| eq::badge(&values, b)).collect();
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
	// Match EQ: the proposal dashed, the EQ input and the reference spectra.
	let matching = state
		.match_eq()
		.filter(|m| m.device == r)
		.and_then(|m| match &m.phase {
			MatchPhase::Ready(sp) => Some(sp.clone()),
			_ => None,
		});
	let proposal = matching.as_ref().and_then(|_| state.match_eq_proposal());
	// Collision view: a suggested cut replaces the match proposal on the dashed curve.
	let collisions = super::collisions::layer(state, r);
	let suggested = state.collision_suggestion(r).map(|s| s.changes.clone());
	let proposed_bands = suggested
		.as_deref()
		.or(proposal.as_ref().map(|(_, changes)| changes.as_slice()))
		.map(|changes| {
			let pv = crate::state::match_eq::proposed_values(&values, changes);
			EqBands::new(pv.as_slice(), rate)
		});
	let proposed_curve = proposed_bands
		.as_ref()
		.map(|b| move |f: f32| b.response_db(f));
	let match_bins = matching
		.as_ref()
		.map(crate::state::match_eq::display_spectra);
	let show_pre = v.analyzer
		&& v.pre
		&& !v.pre_bins.is_empty()
		&& match_bins.is_none()
		&& collisions.is_none();
	let show_post = v.analyzer && !v.post_bins.is_empty() && match_bins.is_none();
	let collision_spectra = collisions.as_ref().map(|c| c.spectra()).unwrap_or_default();
	let hot = super::collisions::hot_band(state, r, v.collision_hover);
	let heat = collisions.as_ref().map(|c| c.heat(hot)).unwrap_or_default();
	if live.is_some() && (0..N_BANDS).any(|b| eq::is_dynamic(&values, b)) {
		ui.ctx().request_repaint();
	}
	let mut child = ui.new_child(UiBuilder::new().max_rect(rect));
	let out = FreqDisplay::new(Id::new(("eq-display", cx.salt, r.device)), rect.size())
		.range(if v.range24 {
			DbRange::PM24
		} else {
			DbRange::PM12
		})
		.pre(
			match_bins
				.as_ref()
				.map(|(input, _)| input.as_slice())
				.or(collisions.as_ref().map(|c| c.own.as_slice()))
				.or(show_pre.then_some(v.pre_bins.bins.as_slice())),
		)
		.spectra(&collision_spectra)
		.heat(&heat)
		.post(show_post.then_some(v.post_bins.bins.as_slice()))
		.reference(match_bins.as_ref().map(|(_, r)| r.as_slice()))
		.proposal(proposed_curve.as_ref().map(|f| f as &dyn Fn(f32) -> f32))
		.bands(&fills)
		.before(before_curve.as_ref().map(|f| f as &dyn Fn(f32) -> f32))
		.ghost(moving.then_some(&static_sum as &dyn Fn(f32) -> f32))
		.sum(&sum, theme::EQ_MID)
		.sum2(
			split.then_some(&side_sum as &dyn Fn(f32) -> f32),
			theme::EQ_SIDE,
		)
		.sum_tags(split.then_some(["M", "S"]))
		.badges(&badges)
		.nodes(&nodes)
		.selected(Some(v.selected))
		.piano(true)
		.curve_grab(&grab)
		.bracket(bracket);
	let mut out = if cx.compact { out.small_nodes() } else { out }.show(&mut child);
	super::modulation::eq_overlay(
		ui,
		state,
		r,
		cx.info,
		out.plot,
		if v.range24 {
			DbRange::PM24
		} else {
			DbRange::PM12
		},
	);
	for e in std::mem::take(&mut out.events) {
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
			FreqEvent::Click { node } => {
				v.selected = node;
				if ui.input(|i| i.modifiers.alt) {
					let mode = eq::toggle_listen(state.listen_mode(r.device), node);
					state.set_listen(r.device, mode);
				}
			}
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
						&trf("Wake {name}", &[("name", &tr_str(BANDS[band].name))]),
					);
				}
			}
		}
	}
	for (band, resp) in out.node_responses.iter().enumerate() {
		node_menu(resp, state, r, band, &mut v.selected);
	}
	v.collision_hover =
		super::collisions::interact(ui, state, r, collisions.as_ref(), &out, cx.salt);
	if state.collision_suggestion(r).is_some() {
		super::collisions::card(ui, state, r, rect);
	} else if state.match_eq().is_some_and(|m| m.device == r) {
		match_card(ui, state, rect);
	}
	if v.collision_hover.is_some() {
		return;
	}
	out.response.on_hover_text_at_pointer(tr(
		"Drag a node (Shift fine, Alt one axis, Ctrl gain only) · wheel: Q / slope · \
		 double-click a node to reset, empty space to wake a band",
	));
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
			egui::RichText::new(tr_str(b.name))
				.font(theme::semibold(12.0))
				.color(band_color(band)),
		);
		for id in [b.gain, Some(b.freq), b.q, b.slope].into_iter().flatten() {
			let address = ParamAddress::device(r.track, r.device, id);
			let word = eq::param_word(id);
			let text = if state.is_automated(&address) {
				trf("Show {word} automation", &[("word", &word)])
			} else {
				trf("Automate {word}", &[("word", &word)])
			};
			if ui.button(text).clicked() {
				state.automate(address);
				ui.close();
			}
		}
		ui.separator();
		if ui.button(tr("Reset band")).clicked() {
			commit(
				state,
				r,
				&eq::reset_band(band),
				&trf("Reset {name}", &[("name", &tr_str(b.name))]),
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
		egui::RichText::new(tr("OUTPUT"))
			.font(theme::medium(10.5))
			.color(theme::TEXT_FAINT),
	);
	out.add_space(6.0);
	ParamKnob::new(cx.r, eq::OUTPUT)
		.size(KnobSize::L)
		.label(tr("Gain"))
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
		match_button(ui, state, cx.r);
		range_selector(ui, cx, v);
		// The caption goes first when the row is tight (long translations, narrow docks).
		let caption = egui::RichText::new(tr("Range"))
			.font(theme::medium(12.0))
			.color(theme::TEXT_FAINT);
		let w = ui
			.painter()
			.layout_no_wrap(
				tr("Range").to_owned(),
				theme::medium(12.0),
				theme::TEXT_FAINT,
			)
			.size()
			.x;
		if ui.available_width() >= w + 12.0 {
			ui.label(caption);
		}
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
		super::collisions::toggle(&mut row, state, cx.r);
		row.with_layout(Layout::right_to_left(Align::Center), |ui| {
			ui.label(
				egui::RichText::new(tr("Display only. No effect on the sound."))
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
	8.0 + 24.0 + 8.0 + 24.0 + 28.0 + COMPACT_KNOBS_H + 30.0 + if footer { 30.0 } else { 0.0 }
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
			let (name_rect, _) = ui.allocate_exact_size(vec2(width, 24.0), Sense::hover());
			let mut row = child(ui, name_rect.shrink2(vec2(2.0, 0.0)));
			row.spacing_mut().item_spacing.x = 6.0;
			row.label(
				egui::RichText::new(tr_str(BANDS[band].name))
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
			row.with_layout(Layout::right_to_left(Align::Center), |ui| {
				listen_toggle(ui, state, cx.r, band);
				dynamic_controls(ui, state, cx, band, true);
			});
			let (ms_rect, _) = ui.allocate_exact_size(vec2(width, 28.0), Sense::hover());
			let mut row = child(ui, ms_rect.shrink2(vec2(2.0, 2.0)));
			row.spacing_mut().item_spacing.x = 6.0;
			dim_label(&mut row, tr("Channel"));
			channel_selector(&mut row, state, cx, band);
			row.with_layout(Layout::right_to_left(Align::Center), |ui| {
				match_button(ui, state, cx.r);
			});
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
					.on_hover_text(tr("Display range"));
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
				super::collisions::toggle(&mut row, state, cx.r);
			}
		});
}

/// The selected band's controls as S knobs (Freq, Gain, Q or Slope) and the output.
fn compact_knobs(ui: &mut Ui, state: &mut AppState, cx: &BodyCx, band: usize, rect: Rect) {
	let b = &BANDS[band];
	let color = band_color(band);
	let mut knobs: Vec<(&'static str, &'static str, Color32)> = vec![(b.freq, mark("Freq"), color)];
	if let Some(g) = b.gain {
		knobs.push((g, mark("Gain"), color));
	}
	if let Some(q) = b.q {
		knobs.push((q, "Q", color));
	}
	if let Some(s) = b.slope {
		knobs.push((s, mark("Slope"), color));
	}
	if let Some(d) = b
		.dynamic()
		.filter(|_| eq::is_dynamic(&shown_values(state, cx.r), band))
	{
		knobs.push((d.threshold, mark("Thr"), theme::GAIN_REDUCTION));
		knobs.push((d.ratio, mark("Ratio"), theme::GAIN_REDUCTION));
	}
	// Six value tags need about 64 px each; narrower, the output stays in the dock.
	if rect.width() / (knobs.len() + 1) as f32 >= 64.0 {
		knobs.push((eq::OUTPUT, mark("Output"), theme::TEXT));
	}
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
	let listened = state.listen_mode(r.device);
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
				tr_str(b.name),
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
		if listened == Listen::Band(band as u8) {
			p.rect_stroke(
				rect,
				CornerRadius::same(theme::RADIUS_MD),
				Stroke::new(1.0, theme::SOLO),
				StrokeKind::Inside,
			);
		}
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
		egui::RichText::new(tr_str(b.name))
			.font(theme::semibold(12.0))
			.color(band_color(band)),
	);
	ui.add_space(4.0);
	dim_label(ui, tr("Freq"));
	let freq = value_box(ui, state, cx, band, b.freq, 78.0);
	ui.label(
		egui::RichText::new(note_text(freq))
			.font(theme::mono(11.0))
			.color(theme::TEXT_FAINT),
	);
	if let Some(g) = b.gain {
		ui.add_space(4.0);
		dim_label(ui, tr("Gain"));
		value_box(ui, state, cx, band, g, 76.0);
	}
	if let Some(q) = b.q {
		ui.add_space(4.0);
		dim_label(ui, "Q");
		value_box(ui, state, cx, band, q, 56.0);
	}
	if let Some(s) = b.slope {
		ui.add_space(4.0);
		dim_label(ui, tr("Slope"));
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
	ui.add_space(4.0);
	channel_selector(ui, state, cx, band);
	ui.add_space(4.0);
	listen_toggle(ui, state, cx.r, band);
	if b.dynamic().is_some() {
		dynamic_controls(ui, state, cx, band, false);
	}
}

/// Stereo | Mid | Side of the selected band (one undo step per click).
fn channel_selector(ui: &mut Ui, state: &mut AppState, cx: &BodyCx, band: usize) {
	let values = shown_values(state, cx.r);
	let selected = eq::channel_index(&values, band);
	let labels = &eq::CHANNELS;
	if let Some(i) = device_kit::segmented(
		ui,
		Id::new(("eq-channel", cx.salt, cx.r.device, band)),
		labels,
		selected,
	) && i != selected
	{
		commit_band(state, cx.r, band, &eq::set_channel(band, i));
	}
}

/// The selected band's "Dynamic" toggle and, while on, its threshold and ratio (dock:
/// also the shared attack / release when the row has room).
fn dynamic_controls(ui: &mut Ui, state: &mut AppState, cx: &BodyCx, band: usize, compact: bool) {
	let Some(d) = BANDS[band].dynamic() else {
		return;
	};
	let values = shown_values(state, cx.r);
	let on = eq::is_dynamic(&values, band);
	let resp = device_kit::toggle(ui, on, tr("Dynamic"), theme::GAIN_REDUCTION).on_hover_text(tr(
		"Dynamic band: it stays at 0 dB until its own frequency region (or the key) passes \
		 the threshold, then moves toward its gain",
	));
	if resp.clicked() {
		let changes = eq::toggle_dynamic(&values, band);
		let label = if on {
			trf("{name} static", &[("name", &tr_str(BANDS[band].name))])
		} else {
			trf("{name} dynamic", &[("name", &tr_str(BANDS[band].name))])
		};
		commit(state, cx.r, &changes, &label);
	}
	if !on || compact {
		return;
	}
	ui.add_space(2.0);
	dim_label(ui, tr("Thr"));
	value_box(ui, state, cx, band, d.threshold, 70.0);
	dim_label(ui, tr("Ratio"));
	value_box(ui, state, cx, band, d.ratio, 56.0);
	// Attack and release are shared by every dynamic band: shown when there is room
	// (Range and the footer toggle still need theirs on the right).
	if ui.available_width() > 2.0 * (64.0 + 40.0) + 260.0 {
		dim_label(ui, tr("Att"));
		value_box(ui, state, cx, band, eq::DYN_ATTACK, 64.0);
		dim_label(ui, tr("Rel"));
		value_box(ui, state, cx, band, eq::DYN_RELEASE, 64.0);
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
			tip: Some(info.description),
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
		trf("{arrow}  Analyzer and display", &[("arrow", &arrow)]),
		font,
		theme::TEXT_DIM,
	);
	let (rect, resp) = ui.allocate_exact_size(vec2(galley.size().x + 16.0, CHIP_H), Sense::click());
	resp.widget_info(|| {
		egui::WidgetInfo::selected(
			egui::WidgetType::Checkbox,
			true,
			v.footer,
			tr("Analyzer and display"),
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
	if device_kit::toggle(ui, v.analyzer, tr("Analyzer"), theme::ACCENT)
		.on_hover_text(tr(
			"The spectrum after the EQ (needs the transport running)",
		))
		.clicked()
	{
		v.analyzer = !v.analyzer;
	}
	if device_kit::toggle(ui, v.pre, tr("Pre"), theme::TEXT_DIM)
		.on_hover_text(tr("Also the spectrum before the EQ"))
		.clicked()
	{
		v.pre = !v.pre;
	}
	if device_kit::toggle(ui, v.side, tr("Side"), theme::EQ_SIDE)
		.on_hover_text(tr("Analyze the side (L − R) instead of the mid"))
		.clicked()
	{
		v.side = !v.side;
		v.pre_window.clear();
		v.post_window.clear();
	}
}

/// "Match…": the reference picker (an audio file or another track).
fn match_button(ui: &mut Ui, state: &mut AppState, r: DeviceRef) {
	let busy = state.match_eq().is_some_and(|m| m.device == r);
	let resp = ui
		.add_enabled(
			!busy,
			egui::Button::new(egui::RichText::new(tr("Match…")).font(theme::medium(12.0)))
				.min_size(vec2(0.0, CHIP_H)),
		)
		.on_hover_text(tr(
			"Match EQ: fit the bands so this track's tone moves toward a reference (an audio file or another track)",
		));
	egui::Popup::menu(&resp).show(|ui| {
		ui.set_min_width(200.0);
		if ui.button(tr("Audio file…")).clicked() {
			state.match_eq_file_dialog(r);
			ui.close();
		}
		let tracks = state.match_reference_tracks(r);
		if !tracks.is_empty() {
			ui.separator();
			ui.label(
				egui::RichText::new(tr("Another track"))
					.font(theme::medium(11.0))
					.color(theme::TEXT_FAINT),
			);
			for (id, name) in tracks {
				if ui.button(name).clicked() {
					state.start_match_eq(r, MatchReference::Track(id));
					ui.close();
				}
			}
		}
	});
}

/// The match card over the display: progress, or the amount / max gain, the tonal
/// difference before -> after and Apply / Cancel.
fn match_card(ui: &mut Ui, state: &mut AppState, display: Rect) {
	let Some(m) = state.match_eq() else {
		return;
	};
	let name = m.reference_name.clone();
	let phase = m.phase.clone();
	let (mut amount, mut max_gain) = (m.amount, m.max_gain_db);
	let width = (display.width() - 16.0).min(560.0);
	let card = Rect::from_min_size(
		pos2(display.right() - width - 8.0, display.top() + 8.0),
		vec2(width, 62.0),
	);
	ui.painter()
		.rect_filled(card, CornerRadius::same(theme::RADIUS_MD), theme::BG_RAISED);
	ui.painter().rect_stroke(
		card,
		CornerRadius::same(theme::RADIUS_MD),
		Stroke::new(1.0, theme::ACCENT.gamma_multiply(0.6)),
		StrokeKind::Inside,
	);
	let inner = card.shrink2(vec2(10.0, 5.0));
	let half = inner.height() * 0.5;
	let mut top = child(ui, inner.with_max_y(inner.top() + half));
	let mut bottom = child(ui, inner.with_min_y(inner.top() + half));
	top.spacing_mut().item_spacing.x = 8.0;
	bottom.spacing_mut().item_spacing.x = 8.0;
	top.label(
		egui::RichText::new(trf("Match to {name}", &[("name", &name)]))
			.font(theme::semibold(12.0))
			.color(theme::ACCENT),
	);
	let mut cancel = false;
	let mut apply = false;
	let buttons = |ui: &mut Ui, ready: bool, apply: &mut bool, cancel: &mut bool| {
		ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
			let close = if ready { tr("Cancel") } else { tr("Close") };
			*cancel = ui.button(close).clicked();
			if ready {
				*apply = theme::primary_button(ui, tr("Apply")).clicked();
			}
		});
	};
	match phase {
		MatchPhase::Running(f) => {
			top.add(
				egui::ProgressBar::new(f)
					.desired_width(160.0)
					.text(tr("Analyzing…")),
			);
			ui.ctx().request_repaint();
			bottom.with_layout(Layout::right_to_left(Align::Center), |ui| {
				cancel = ui.button(tr("Cancel")).clicked();
			});
		}
		MatchPhase::Failed(e) => {
			top.add(
				egui::Label::new(
					egui::RichText::new(e)
						.font(theme::medium(11.5))
						.color(theme::WARNING),
				)
				.truncate(),
			);
			buttons(&mut bottom, false, &mut apply, &mut cancel);
		}
		MatchPhase::Ready(_) => {
			if let Some((fit, _)) = state.match_eq_proposal() {
				top.label(
					egui::RichText::new(crate::i18n::decimal(format!(
						"{:.1} dB → {:.1} dB",
						fit.before_db, fit.residual_db
					)))
					.font(theme::mono(11.0))
					.color(theme::TEXT_DIM),
				)
				.on_hover_text(tr(
					"Tonal difference from the reference before → after the proposed bands",
				));
			}
			let ui = &mut bottom;
			ui.label(
				egui::RichText::new(tr("Amount"))
					.font(theme::medium(11.5))
					.color(theme::TEXT_FAINT),
			);
			ui.add(
				egui::Slider::new(&mut amount, 0.0..=100.0)
					.suffix(" %")
					.fixed_decimals(0)
					.show_value(true),
			);
			ui.label(
				egui::RichText::new(tr("Max"))
					.font(theme::medium(11.5))
					.color(theme::TEXT_FAINT),
			);
			ui.add(
				egui::DragValue::new(&mut max_gain)
					.range(1.0..=12.0)
					.speed(0.1)
					.fixed_decimals(1)
					.suffix(" dB"),
			);
			buttons(ui, true, &mut apply, &mut cancel);
		}
	}
	if let Some(m) = state.match_eq_mut() {
		m.amount = amount;
		m.max_gain_db = max_gain;
	}
	if apply {
		state.apply_match_eq();
	} else if cancel {
		state.cancel_match_eq();
	}
}
