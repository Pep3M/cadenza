//! The Drum Machine editor body (design `17 Instruments`, 17c): one strip per voice
//! (kick, snare, clap, hats, toms, cymbals, percussion) in the kit palette with its
//! notes, a tiny envelope from its own params, its two main params and a level bar
//! (drag ⇔, wheel over the strip in 0.5 dB); clicking a pad auditions its GM note
//! through the preview path and selects it (Ctrl / ⌘-click only selects, double-click
//! resets the voice); ← → move between voices while the editor has the focus. The
//! selected voice gets large knobs and a bigger picture (the kick's waveform with its
//! pitch sweep, the others' amplitude), the KIT column the global tone and gain. Strips
//! flash in their colour when the engine triggers them (the processor's trigger
//! counters, read through the probe). Below 1100 px the strips go above the selected
//! voice, below 700 px the compact form takes over (also the inspector's). The pure
//! logic lives in [`crate::state::drum_machine_editor`].

use std::sync::{Arc, Mutex};

use cadenza_core::TrackId;
use egui::{
	Align2, Color32, CornerRadius, CursorIcon, Id, Key, Layout, Modifiers, Pos2, Rect, Sense,
	Shape, Stroke, StrokeKind, Ui, UiBuilder, Vec2, pos2, vec2,
};

use crate::state::AppState;
use crate::state::drum_machine_editor::{self as dm, Flashes, GAIN, TONE, VOICES, Voice};
use crate::state::selection::DeviceRef;
use crate::theme;
use crate::ui::device_editor::{BodyCx, DOCK};
use crate::ui::device_kit::{self, KnobSize};
use crate::ui::devices::{self, FOOTER_H, KnobDef};

const GAP: f32 = 8.0;
const STRIP_GAP: f32 = 6.0;
/// One row from this width on; narrower docks put the strips above the selected voice.
const ROW_MIN: f32 = 1100.0;
/// Below this width the dock shows the compact form.
const COMPACT_BELOW: f32 = 700.0;
const KIT_W: f32 = 112.0;
const KIT_W_NARROW: f32 = 92.0;
const LEVEL_H: f32 = 14.0;
const WHEEL_REST: f64 = 0.5;
const STRIP_BG: Color32 = Color32::from_rgb(0x18, 0x19, 0x1d);
const STRIP_BG_SELECTED: Color32 = Color32::from_rgb(0x1f, 0x20, 0x24);
const STRIP_RING: Color32 = Color32::from_rgb(0x1f, 0x21, 0x25);
const LEVEL_TRACK: Color32 = Color32::from_rgb(0x0e, 0x0f, 0x11);
const GRID: Color32 = Color32::from_rgb(0x16, 0x18, 0x1b);
const PITCH: Color32 = theme::BAND_COLORS[2];
const ENVELOPE: Color32 = Color32::from_rgb(0xf4, 0xf5, 0xf7);
const FOOTER: &str = "Strip shows the voice's own envelope from its current settings. Pads \
	flash in the voice colour when the engine triggers them.";

fn color(voice: &Voice) -> Color32 {
	theme::BAND_COLORS[voice.color]
}

fn select_key() -> &'static str {
	if cfg!(target_os = "macos") {
		"Cmd"
	} else {
		"Ctrl"
	}
}

// --- View state (egui memory) --------------------------------------------------------

#[derive(Debug, Clone, Copy)]
struct LevelDrag {
	voice: usize,
	start: f32,
	dx: f32,
	width: f32,
}

#[derive(Debug, Clone, Copy)]
struct WheelEdit {
	voice: usize,
	value: f32,
	last: f64,
}

#[derive(Default)]
struct DrumView {
	selected: usize,
	flashes: Flashes,
	/// A flash held at this level (screenshots).
	pinned: Option<(usize, f32)>,
	level: Option<LevelDrag>,
	wheel: Option<WheelEdit>,
	/// The note a pad press sounds until the button goes up.
	held: Option<(TrackId, u8)>,
}

type Shared = Arc<Mutex<DrumView>>;

fn view_id(salt: &str, r: DeviceRef) -> Id {
	Id::new(("drum-machine-editor", salt, r.device))
}

fn view(ctx: &egui::Context, id: Id) -> Shared {
	ctx.data_mut(|d| {
		d.get_temp_mut_or_insert_with(id, || Arc::new(Mutex::new(DrumView::default())))
			.clone()
	})
}

fn lock(view: &Shared) -> std::sync::MutexGuard<'_, DrumView> {
	view.lock().unwrap_or_else(|e| e.into_inner())
}

/// The voice selected in the editor `salt` (index into [`VOICES`]).
pub fn selected_voice(ctx: &egui::Context, salt: &str, r: DeviceRef) -> usize {
	lock(&view(ctx, view_id(salt, r))).selected
}

/// Holds voice `voice`'s strip lit at `level` (0..1) as if the engine had just
/// triggered it (screenshots); `None` lets the counters drive the flashes again.
pub fn set_flash(ctx: &egui::Context, salt: &str, r: DeviceRef, flash: Option<(usize, f32)>) {
	lock(&view(ctx, view_id(salt, r))).pinned = flash;
}

/// ← → move between the voices while the editor has the focus.
pub fn shortcuts(ctx: &egui::Context, r: DeviceRef) {
	let delta = ctx.input_mut(|i| {
		if i.consume_key(Modifiers::NONE, Key::ArrowLeft) {
			-1
		} else if i.consume_key(Modifiers::NONE, Key::ArrowRight) {
			1
		} else {
			0
		}
	});
	if delta != 0 {
		let shared = view(ctx, view_id(DOCK, r));
		let mut v = lock(&shared);
		v.selected = dm::step_voice(v.selected, delta);
	}
}

fn value(state: &AppState, cx: &BodyCx, id: &str) -> f32 {
	devices::shown(state, cx.r, cx.info, id)
}

/// How lit voice `i` is now.
fn flash(v: &DrumView, i: usize, now: f64) -> f32 {
	match v.pinned {
		Some((p, level)) if p == i => level,
		_ => v.flashes.level(i, now),
	}
}

// --- Body ------------------------------------------------------------------------------

pub fn drum_machine_body(ui: &mut Ui, state: &mut AppState, cx: &BodyCx) {
	let shared = view(ui.ctx(), view_id(cx.salt, cx.r));
	let mut v = lock(&shared);
	let now = ui.input(|i| i.time);
	listen(ui, state, cx, &mut v, now);
	finish_wheel(ui, state, cx, &mut v);
	if ui.input(|i| i.pointer.any_released())
		&& let Some((track, note)) = v.held.take()
	{
		state.preview_note_off(track, note);
	}
	if cx.compact || cx.rect.width() < COMPACT_BELOW {
		let height = compact_height(cx.rect.width());
		devices::compact_scroll(ui, cx, height, |ui, rect| {
			compact(ui, state, cx, &mut v, rect, now);
		});
	} else {
		dock(ui, state, cx, &mut v, now);
	}
}

/// Takes the trigger counters of the probed device and keeps the flashes moving.
fn listen(ui: &Ui, state: &AppState, cx: &BodyCx, v: &mut DrumView, now: f64) {
	if state.probe_target() == Some(cx.r.device)
		&& let Some(probe) = state.probe()
	{
		let (values, n) = probe.readings();
		v.flashes.update(&values[..n], now);
	}
	if v.flashes.any_lit(now) {
		ui.ctx().request_repaint();
	}
}

fn dock(ui: &mut Ui, state: &mut AppState, cx: &BodyCx, v: &mut DrumView, now: f64) {
	let rect = cx.rect;
	let content = Rect::from_min_max(rect.min, pos2(rect.right(), rect.bottom() - FOOTER_H - 4.0));
	devices::footer(ui, rect, FOOTER);
	if rect.width() >= ROW_MIN {
		let strip_w = ((rect.width() * 0.48 - 6.0 * STRIP_GAP) / 7.0).clamp(84.0, 120.0);
		let strips_w = 7.0 * strip_w + 6.0 * STRIP_GAP;
		let strips = Rect::from_min_size(content.min, vec2(strips_w, content.height()));
		strip_row(ui, state, cx, v, strips, now);
		let panel =
			Rect::from_min_max(pos2(strips.right() + GAP + 4.0, content.top()), content.max);
		selected_panel(ui, state, cx, v, panel);
	} else {
		let strips_h = (content.height() * 0.55).clamp(200.0, 260.0);
		let panel_h = 250.0_f32.max(content.height() - strips_h - GAP);
		let mut inner = ui.new_child(
			UiBuilder::new()
				.max_rect(content)
				.layout(Layout::top_down(egui::Align::Min)),
		);
		egui::ScrollArea::vertical()
			.id_salt(("drum-machine-stack", cx.salt, cx.r.device))
			.auto_shrink([false, false])
			.show(&mut inner, |ui| {
				let w = ui.available_width();
				let (area, _) =
					ui.allocate_exact_size(vec2(w, strips_h + GAP + panel_h), Sense::hover());
				let strips = Rect::from_min_size(area.min, vec2(w, strips_h));
				strip_row(ui, state, cx, v, strips, now);
				let panel = Rect::from_min_max(pos2(area.left(), strips.bottom() + GAP), area.max);
				selected_panel(ui, state, cx, v, panel);
			});
	}
}

fn strip_row(
	ui: &mut Ui,
	state: &mut AppState,
	cx: &BodyCx,
	v: &mut DrumView,
	rect: Rect,
	now: f64,
) {
	let w = (rect.width() - 6.0 * STRIP_GAP) / 7.0;
	for (i, voice) in VOICES.iter().enumerate() {
		let strip = Rect::from_min_size(
			pos2(rect.left() + i as f32 * (w + STRIP_GAP), rect.top()),
			vec2(w, rect.height()),
		);
		voice_strip(ui, state, cx, v, i, voice, strip, now);
	}
}

// --- Strips ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn voice_strip(
	ui: &mut Ui,
	state: &mut AppState,
	cx: &BodyCx,
	v: &mut DrumView,
	i: usize,
	voice: &Voice,
	rect: Rect,
	now: f64,
) {
	let c = color(voice);
	let selected = v.selected == i;
	let lit = flash(v, i, now);
	// The strip itself: click selects, wheel = level.
	let body = ui.interact(
		rect,
		Id::new(("drum-strip", cx.salt, cx.r.device, i)),
		Sense::click(),
	);
	if body.clicked() {
		v.selected = i;
	}
	strip_wheel(ui, state, cx, v, i, voice, &body);
	let p = ui.painter();
	p.rect(
		rect,
		CornerRadius::same(theme::RADIUS_LG),
		if selected {
			STRIP_BG_SELECTED
		} else {
			STRIP_BG
		},
		if selected {
			Stroke::new(1.5, c)
		} else {
			Stroke::new(1.0, STRIP_RING)
		},
		StrokeKind::Inside,
	);
	let inner = rect.shrink(6.0);
	let pad_h = (rect.height() * 0.34).clamp(60.0, 96.0);
	let pad_rect = Rect::from_min_size(inner.min, vec2(inner.width(), pad_h));
	pad(ui, state, cx, v, i, voice, pad_rect, lit, false);
	let env_h = (rect.height() * 0.17).clamp(30.0, 52.0);
	let env = Rect::from_min_size(
		pos2(inner.left(), pad_rect.bottom() + 6.0),
		vec2(inner.width(), env_h),
	);
	strip_envelope(ui.painter(), state, cx, i, env, c);
	let mut y = env.bottom() + 8.0;
	let font = if inner.width() < 90.0 { 10.0 } else { 11.0 };
	for sp in voice.summary {
		let text = device_kit::param_info(state, cx.r, sp.id).map_or_else(String::new, |info| {
			dm::strip_value(value(state, cx, sp.id), info.unit)
		});
		let row = Rect::from_min_size(pos2(inner.left(), y), vec2(inner.width(), 15.0));
		let p = ui.painter();
		p.text(
			row.left_center(),
			Align2::LEFT_CENTER,
			sp.label,
			theme::medium(font),
			theme::TEXT_DIM,
		);
		p.text(
			row.right_center(),
			Align2::RIGHT_CENTER,
			text,
			theme::mono(font),
			theme::TEXT,
		);
		y += 18.0;
	}
	let bar = Rect::from_min_max(pos2(inner.left(), inner.bottom() - LEVEL_H), inner.max);
	if voice.summary.iter().all(|sp| sp.id != voice.level) && bar.top() - 18.0 > y {
		let db = crate::state::device::format_value(value(state, cx, voice.level), "dB");
		let p = ui.painter();
		p.text(
			pos2(inner.left(), bar.top() - 2.0),
			Align2::LEFT_BOTTOM,
			"Level",
			theme::medium(font),
			theme::TEXT_FAINT,
		);
		p.text(
			pos2(inner.right(), bar.top() - 2.0),
			Align2::RIGHT_BOTTOM,
			db,
			theme::mono(font),
			theme::TEXT_DIM,
		);
	}
	level_bar(ui, state, cx, v, i, voice, bar);
}

/// The voice's pad: click = audition + select (Ctrl / ⌘: select only), double-click =
/// reset the voice. `mini` = the compact grid's smaller pad.
#[allow(clippy::too_many_arguments)]
fn pad(
	ui: &mut Ui,
	state: &mut AppState,
	cx: &BodyCx,
	v: &mut DrumView,
	i: usize,
	voice: &Voice,
	rect: Rect,
	lit: f32,
	mini: bool,
) {
	let c = color(voice);
	let selected = v.selected == i;
	let resp = ui
		.interact(
			rect,
			Id::new(("drum-pad", cx.salt, cx.r.device, i)),
			Sense::click_and_drag(),
		)
		.on_hover_cursor(CursorIcon::PointingHand);
	let label = format!("{} pad", voice.name);
	resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &label));
	if resp.is_pointer_button_down_on() && v.held.is_none() && !resp.dragged() {
		let pressed = ui.input(|inp| inp.pointer.any_pressed());
		if pressed {
			v.selected = i;
			let quiet = ui.input(|inp| {
				inp.modifiers.command
					|| inp.events.iter().any(|e| {
						matches!(e, egui::Event::PointerButton { pressed: true, modifiers, .. }
							if modifiers.command)
					})
			});
			if !quiet && let Some(track) = cx.r.track {
				state.preview_note_on(track, voice.audition, 0.85);
				v.held = Some((track, voice.audition));
			}
		}
	}
	if resp.double_clicked() {
		devices::commit_changes(state, cx.r, cx.info, &dm::reset_voice(voice), |_| {
			format!("Reset {}", voice.name.to_lowercase())
		});
	}
	let _ = resp.on_hover_text(format!(
		"{}: click to audition ({} plays it), {}-click to select without playing, \
		 double-click resets the voice",
		voice.name,
		voice.note_label,
		select_key()
	));
	if !ui.is_rect_visible(rect) {
		return;
	}
	let p = ui.painter();
	let base = if selected || lit > 0.0 { 0.2 } else { 0.08 };
	let fill = c.gamma_multiply((base + 0.5 * lit).min(1.0));
	if lit > 0.0 {
		p.rect_filled(
			rect.expand(3.0),
			CornerRadius::same(theme::RADIUS_LG + 2),
			c.gamma_multiply(0.18 * lit),
		);
	}
	p.rect(
		rect,
		CornerRadius::same(5),
		fill,
		Stroke::new(
			1.0,
			c.gamma_multiply(if selected { 0.67_f32 } else { 0.27 }.max(lit)),
		),
		StrokeKind::Inside,
	);
	let inner = rect.shrink(if mini { 6.0 } else { 8.0 });
	p.rect_filled(
		Rect::from_min_size(inner.min + vec2(0.0, 1.0), vec2(8.0, 8.0)),
		2.0,
		c,
	);
	let narrow = mini || rect.width() < 90.0;
	let note = if narrow {
		voice.note_label.split(" \u{b7} ").last().unwrap_or("")
	} else {
		voice.note_label
	};
	p.text(
		pos2(inner.right(), inner.top()),
		Align2::RIGHT_TOP,
		note,
		theme::mono(if mini { 9.5 } else { 10.5 }),
		if lit > 0.5 {
			theme::TEXT_ON_COLOR
		} else {
			theme::TEXT_DIM
		},
	);
	let font = theme::semibold(if narrow { 12.0 } else { 14.0 });
	let fits = p
		.layout_no_wrap(voice.name.to_owned(), font.clone(), theme::TEXT)
		.size()
		.x <= inner.width();
	p.text(
		pos2(inner.left(), inner.bottom()),
		Align2::LEFT_BOTTOM,
		if fits { voice.name } else { voice.short },
		font,
		Color32::from_rgb(0xf4, 0xf5, 0xf7),
	);
}

/// The level bar: drag ⇔ over the whole range (one step on release).
#[allow(clippy::too_many_arguments)]
fn level_bar(
	ui: &mut Ui,
	state: &mut AppState,
	cx: &BodyCx,
	v: &mut DrumView,
	i: usize,
	voice: &Voice,
	rect: Rect,
) {
	let resp = ui
		.interact(
			rect,
			Id::new(("drum-level", cx.salt, cx.r.device, i)),
			Sense::click_and_drag(),
		)
		.on_hover_cursor(CursorIcon::ResizeHorizontal);
	let label = format!("{} level", voice.name);
	resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Slider, true, &label));
	if resp.drag_started() {
		v.selected = i;
		v.level = Some(LevelDrag {
			voice: i,
			start: value(state, cx, voice.level),
			dx: 0.0,
			width: rect.width(),
		});
	}
	if resp.dragged()
		&& let Some(d) = v.level.as_mut().filter(|d| d.voice == i)
	{
		d.dx += resp.drag_delta().x;
		let db = dm::drag_level(voice.level, d.start, d.dx, d.width);
		state.preview_param(cx.r.device, voice.level, db);
	}
	if resp.drag_stopped()
		&& let Some(d) = v.level.take().filter(|d| d.voice == i)
	{
		let db = dm::drag_level(voice.level, d.start, d.dx, d.width);
		devices::commit_changes(state, cx.r, cx.info, &[(voice.level, db)], |_| {
			format!("{} level", voice.name)
		});
	}
	if resp.double_clicked() {
		let db = dm::param(voice.level).default;
		devices::commit_changes(state, cx.r, cx.info, &[(voice.level, db)], |_| {
			format!("Reset {} level", voice.name.to_lowercase())
		});
	}
	let db = value(state, cx, voice.level);
	let resp = resp.on_hover_text(format!(
		"{} level {}: drag sideways, double-click resets",
		voice.name,
		crate::state::device::format_value(db, "dB")
	));
	let c = color(voice);
	let p = ui.painter();
	let track = Rect::from_center_size(rect.center() + vec2(0.0, 3.0), vec2(rect.width(), 4.0));
	p.rect_filled(track, 2.0, LEVEL_TRACK);
	let w = dm::level_fraction(voice.level, db) * track.width();
	p.rect_filled(
		Rect::from_min_size(track.min, vec2(w, track.height())),
		2.0,
		c,
	);
	// 0 dB tick.
	let zero = track.left() + dm::level_fraction(voice.level, 0.0) * track.width();
	p.vline(
		zero,
		(track.top() - 2.0)..=(track.bottom() + 2.0),
		Stroke::new(1.0, theme::TEXT_FAINT.gamma_multiply(0.6)),
	);
	if resp.dragged() || resp.hovered() {
		device_kit::paint_value_tag(
			p,
			pos2(track.left() + w, track.top() - 6.0),
			Align2::CENTER_BOTTOM,
			&crate::state::device::format_value(db, "dB"),
			theme::TEXT,
			ui.clip_rect(),
		);
	}
}

fn strip_wheel(
	ui: &Ui,
	state: &mut AppState,
	cx: &BodyCx,
	v: &mut DrumView,
	i: usize,
	voice: &Voice,
	resp: &egui::Response,
) {
	let scroll = ui.input(|inp| inp.smooth_scroll_delta.x + inp.smooth_scroll_delta.y);
	if !resp.hovered() || scroll == 0.0 {
		return;
	}
	ui.input_mut(|inp| inp.smooth_scroll_delta = Vec2::ZERO);
	let now = ui.input(|inp| inp.time);
	if v.wheel.is_some_and(|w| w.voice != i) {
		finish_wheel_now(state, cx, v);
	}
	let start = value(state, cx, voice.level);
	let w = v.wheel.get_or_insert(WheelEdit {
		voice: i,
		value: start,
		last: now,
	});
	let p = dm::param(voice.level);
	w.value = (w.value + scroll / device_kit::NOTCH * 0.5).clamp(p.min, p.max);
	w.last = now;
	state.preview_param(cx.r.device, voice.level, (w.value * 2.0).round() / 2.0);
	ui.ctx()
		.request_repaint_after(std::time::Duration::from_secs_f64(WHEEL_REST));
}

fn finish_wheel_now(state: &mut AppState, cx: &BodyCx, v: &mut DrumView) {
	if let Some(w) = v.wheel.take() {
		let voice = &VOICES[w.voice];
		devices::commit_changes(
			state,
			cx.r,
			cx.info,
			&[(voice.level, (w.value * 2.0).round() / 2.0)],
			|_| format!("{} level", voice.name),
		);
	}
}

fn finish_wheel(ui: &Ui, state: &mut AppState, cx: &BodyCx, v: &mut DrumView) {
	let Some(w) = v.wheel else {
		return;
	};
	let idle = ui.input(|i| i.time) - w.last;
	if idle >= WHEEL_REST {
		finish_wheel_now(state, cx, v);
	} else {
		ui.ctx()
			.request_repaint_after(std::time::Duration::from_secs_f64(WHEEL_REST - idle));
	}
}

// --- Pictures -------------------------------------------------------------------------

/// A deterministic 0..1 texture per point (the pictures' "waveform" grain).
fn grain(seed: usize, i: usize) -> f32 {
	let mut x = (seed as u32).wrapping_mul(0x9e37_79b9) ^ (i as u32).wrapping_mul(0x85eb_ca6b);
	x ^= x >> 15;
	x = x.wrapping_mul(0x2c1b_3c6d);
	x ^= x >> 12;
	(x & 0xffff) as f32 / 65_535.0
}

/// Whether the voice is noisy (snare wires, clap, metal) rather than a tone.
fn noisy(voice: usize) -> bool {
	matches!(voice, 1 | 2 | 3 | 5)
}

/// The mirrored amplitude area of voice `i` over `rect` (`open` = the open hat).
fn envelope_area(
	p: &egui::Painter,
	get: &dyn Fn(&str) -> f32,
	i: usize,
	rect: Rect,
	color: Color32,
	open: bool,
	points: usize,
) {
	let span = VOICES[i].span_ms;
	let cy = rect.center().y;
	let half = rect.height() * 0.5 - 1.0;
	let jitter = if noisy(i) { 0.45 } else { 0.18 };
	let mut top = Vec::with_capacity(points + 1);
	let mut bottom = Vec::with_capacity(points + 1);
	for k in 0..=points {
		let t = k as f32 / points as f32;
		let a = dm::display_amplitude(i, get, t * span, open);
		let g = 1.0 - jitter + jitter * grain(i * 7 + open as usize, k);
		let x = rect.left() + t * rect.width();
		top.push(pos2(x, cy - a * g * half));
		bottom.push(pos2(x, cy + a * g * half));
	}
	p.add(device_kit::fill_to(&top, cy, color));
	p.add(device_kit::fill_to(&bottom, cy, color));
}

fn strip_envelope(
	p: &egui::Painter,
	state: &AppState,
	cx: &BodyCx,
	i: usize,
	rect: Rect,
	c: Color32,
) {
	device_kit::paint_display(p, rect);
	let p = p.with_clip_rect(rect.shrink(1.0));
	let get = |id: &str| value(state, cx, id);
	let area = rect.shrink2(vec2(2.0, 2.0));
	let n = (area.width() as usize).clamp(40, 120);
	if i == 3 {
		envelope_area(&p, &get, i, area, c.gamma_multiply(0.25), true, n);
	}
	envelope_area(&p, &get, i, area, c.gamma_multiply(0.55), false, n);
}

/// The selected voice's picture: the kick's waveform (the real sweep and body) with its
/// pitch line, the others' amplitude, and the amplitude envelope dashed over both.
fn big_picture(ui: &Ui, state: &AppState, cx: &BodyCx, i: usize, rect: Rect) {
	if !ui.is_rect_visible(rect) {
		return;
	}
	let voice = &VOICES[i];
	let c = color(voice);
	device_kit::paint_display(ui.painter(), rect);
	let p = ui.painter().with_clip_rect(rect.shrink(1.0));
	let get = |id: &str| value(state, cx, id);
	let mut x = rect.left() + 50.0;
	while x < rect.right() {
		p.vline(x, rect.y_range(), Stroke::new(1.0, GRID));
		x += 50.0;
	}
	let plot = Rect::from_min_max(
		pos2(rect.left() + 1.0, rect.top() + 24.0),
		pos2(rect.right() - 1.0, rect.bottom() - 22.0),
	);
	let cy = plot.center().y;
	p.hline(rect.x_range(), cy, Stroke::new(1.0, device_kit::GRID_MAJOR));
	let half = plot.height() * 0.5;
	let span = voice.span_ms;
	let n = (plot.width() as usize * 2).clamp(200, 1600);
	if i == 0 {
		// The body's waveform at its real (swept) frequency.
		let mut phase = 0.0_f32;
		let dt = span / n as f32;
		let mut wave = Vec::with_capacity(n + 1);
		let mut pitch = Vec::with_capacity(n / 6 + 1);
		let fmax = dm::kick_frequency_max();
		let pitch_h = (plot.height() * 0.32).min(80.0);
		for k in 0..=n {
			let t = k as f32 * dt;
			let f = dm::kick_frequency(&get, t);
			let a = dm::display_amplitude(0, &get, t, false);
			let px = plot.left() + k as f32 / n as f32 * plot.width();
			wave.push(pos2(px, cy - a * phase.sin() * half * 0.9));
			phase += std::f32::consts::TAU * f * dt * 0.001;
			if k % 6 == 0 {
				let fy = (f.ln() - 20.0_f32.ln()) / (fmax.ln() - 20.0_f32.ln());
				pitch.push(pos2(px, plot.bottom() - 4.0 - fy.clamp(0.0, 1.0) * pitch_h));
			}
		}
		p.add(Shape::line(
			wave.clone(),
			Stroke::new(4.0, c.gamma_multiply(0.12)),
		));
		p.add(Shape::line(wave, Stroke::new(1.4, c)));
		p.add(Shape::line(
			pitch,
			Stroke::new(1.25, PITCH.gamma_multiply(0.8)),
		));
	} else {
		let area = plot.shrink2(vec2(0.0, 2.0));
		if i == 3 {
			envelope_area(&p, &get, i, area, c.gamma_multiply(0.22), true, n / 2);
		}
		envelope_area(&p, &get, i, area, c.gamma_multiply(0.5), false, n / 2);
	}
	let env: Vec<Pos2> = (0..=120)
		.map(|k| {
			let t = k as f32 / 120.0;
			let a = dm::display_amplitude(i, &get, t * span, false);
			pos2(plot.left() + t * plot.width(), cy - a * half * 0.95)
		})
		.collect();
	p.add(Shape::dashed_line(
		&env,
		Stroke::new(1.25, ENVELOPE.gamma_multiply(0.7)),
		4.0,
		3.0,
	));
	let mut legend: Vec<(&str, Color32)> = vec![("Amp envelope", ENVELOPE.gamma_multiply(0.7))];
	if i == 0 {
		legend.push(("Pitch sweep", PITCH));
	}
	if i == 3 {
		legend = vec![("Closed", c), ("Open", c.gamma_multiply(0.45))];
	}
	let out = p.text(
		rect.left_top() + vec2(8.0, 6.0),
		Align2::LEFT_TOP,
		"Output",
		theme::medium(10.5),
		theme::TEXT_DIM,
	);
	devices::legend(&p, pos2(out.right() + 14.0, out.center().y), &legend);
	p.text(
		rect.right_bottom() - vec2(8.0, 6.0),
		Align2::RIGHT_BOTTOM,
		dm::span_label(span),
		theme::mono(10.5),
		theme::TEXT_FAINT,
	);
	if i == 0 {
		let f0 = dm::kick_frequency(&get, 0.0);
		let f1 = dm::kick_frequency(&get, span);
		p.text(
			rect.left_bottom() + vec2(8.0, -6.0),
			Align2::LEFT_BOTTOM,
			format!("{f0:.0} \u{2192} {f1:.0} Hz"),
			theme::mono(10.5),
			PITCH.gamma_multiply(0.85),
		);
	}
}

// --- Selected voice -------------------------------------------------------------------

fn voice_knobs(voice: &Voice, size: KnobSize) -> Vec<KnobDef> {
	let c = color(voice);
	voice
		.params
		.iter()
		.map(|p| {
			let k = KnobDef::new(p.id, p.label, size).color(c);
			if p.id == voice.level { k.step(0.5) } else { k }
		})
		.collect()
}

fn selected_panel(ui: &mut Ui, state: &mut AppState, cx: &BodyCx, v: &mut DrumView, rect: Rect) {
	let i = v.selected.min(VOICES.len() - 1);
	let voice = &VOICES[i];
	let c = color(voice);
	let kit_w = if rect.width() < 800.0 {
		KIT_W_NARROW
	} else {
		KIT_W
	};
	let kit = Rect::from_min_max(pos2(rect.right() - kit_w, rect.top()), rect.max);
	let main = Rect::from_min_max(rect.min, pos2(kit.left() - GAP, rect.bottom()));
	// Header: swatch, name, note, hint.
	let head_h = 24.0;
	let p = ui.painter();
	p.rect_filled(
		Rect::from_center_size(
			pos2(main.left() + 5.0, main.top() + head_h * 0.5),
			vec2(10.0, 10.0),
		),
		2.0,
		c,
	);
	let name = p.text(
		pos2(main.left() + 18.0, main.top() + head_h * 0.5),
		Align2::LEFT_CENTER,
		voice.name,
		theme::semibold(15.0),
		theme::TEXT,
	);
	let note = p.text(
		pos2(name.right() + 8.0, main.top() + head_h * 0.5),
		Align2::LEFT_CENTER,
		voice.note_label,
		theme::mono(11.5),
		theme::TEXT_FAINT,
	);
	let hint = format!(
		"Click a pad to audition \u{b7} {}-click to select without playing",
		select_key()
	);
	let g = p.layout_no_wrap(hint, theme::medium(11.0), theme::TEXT_FAINT);
	if note.right() + 16.0 + g.size().x < main.right() {
		p.galley(
			pos2(
				main.right() - g.size().x,
				main.top() + (head_h - g.size().y) * 0.5,
			),
			g,
			theme::TEXT_FAINT,
		);
	}
	let body = Rect::from_min_max(pos2(main.left(), main.top() + head_h + 6.0), main.max);
	let knob_size = if body.height() >= devices::knob_height(KnobSize::L) * 2.0 + 12.0 {
		KnobSize::L
	} else {
		KnobSize::M
	};
	let knobs = voice_knobs(voice, knob_size);
	let cols = if knobs.len() > 2 { 2 } else { 1 };
	let cell_w = knob_size.diameter() + 40.0;
	let knobs_w = cols as f32 * cell_w;
	let picture_w = (body.width() - knobs_w - 12.0).clamp(200.0, 560.0);
	let picture = Rect::from_min_size(body.min, vec2(picture_w, body.height()));
	big_picture(ui, state, cx, i, picture);
	let k = Rect::from_min_max(pos2(picture.right() + 12.0, body.top()), body.max);
	let rows = knobs.len().div_ceil(cols);
	let grid_h = (rows as f32 * (devices::knob_height(knob_size) + 12.0)).min(k.height());
	let k = Rect::from_center_size(k.center(), vec2(k.width(), grid_h));
	devices::knob_grid(ui, state, cx, k, &knobs, cols);
	kit_column(ui, state, cx, kit);
}

fn kit_column(ui: &mut Ui, state: &mut AppState, cx: &BodyCx, rect: Rect) {
	ui.painter().vline(
		rect.left(),
		rect.y_range(),
		Stroke::new(1.0, theme::BORDER_SUBTLE),
	);
	let size = if rect.height() >= 2.0 * devices::knob_height(KnobSize::M) + 40.0 {
		KnobSize::M
	} else {
		KnobSize::S
	};
	let h = 2.0 * devices::knob_height(size) + 28.0;
	let col = Rect::from_center_size(rect.center(), vec2(rect.width(), h.min(rect.height())));
	ui.painter().text(
		pos2(col.center().x, col.top()),
		Align2::CENTER_TOP,
		"KIT",
		theme::semibold(10.5),
		theme::TEXT_FAINT,
	);
	let knobs = Rect::from_min_max(pos2(col.left(), col.top() + 18.0), col.max);
	devices::knob_grid(
		ui,
		state,
		cx,
		knobs,
		&[
			KnobDef::new(TONE, "Tone", size),
			KnobDef::new(GAIN, "Gain", size).step(0.5),
		],
		1,
	);
}

// --- Compact (inspector) --------------------------------------------------------------

const MINI_PAD_H: f32 = 58.0;
const COMPACT_PICTURE_H: f32 = 110.0;
const TITLE_H: f32 = 22.0;

fn compact_cols(width: f32) -> usize {
	if width >= 520.0 { 7 } else { 4 }
}

fn compact_pads_h(width: f32) -> f32 {
	let rows = VOICES.len().div_ceil(compact_cols(width));
	rows as f32 * (MINI_PAD_H + LEVEL_H + 4.0) + (rows - 1) as f32 * 6.0
}

/// The most knobs a voice has (the compact form reserves room for them).
fn most_knobs() -> usize {
	VOICES.iter().map(|v| v.params.len()).max().unwrap_or(1) + 2
}

/// How tall the compact form is at `width`.
pub fn compact_height(width: f32) -> f32 {
	compact_pads_h(width)
		+ GAP + TITLE_H
		+ COMPACT_PICTURE_H
		+ GAP + devices::small_rows_height(width, most_knobs())
}

fn compact(ui: &mut Ui, state: &mut AppState, cx: &BodyCx, v: &mut DrumView, rect: Rect, now: f64) {
	let cols = compact_cols(rect.width());
	let w = (rect.width() - (cols - 1) as f32 * 6.0) / cols as f32;
	for (i, voice) in VOICES.iter().enumerate() {
		let at = pos2(
			rect.left() + (i % cols) as f32 * (w + 6.0),
			rect.top() + (i / cols) as f32 * (MINI_PAD_H + LEVEL_H + 10.0),
		);
		let lit = flash(v, i, now);
		pad(
			ui,
			state,
			cx,
			v,
			i,
			voice,
			Rect::from_min_size(at, vec2(w, MINI_PAD_H)),
			lit,
			true,
		);
		let bar = Rect::from_min_size(at + vec2(0.0, MINI_PAD_H + 2.0), vec2(w, LEVEL_H));
		level_bar(ui, state, cx, v, i, voice, bar);
	}
	let mut y = rect.top() + compact_pads_h(rect.width()) + GAP;
	let i = v.selected.min(VOICES.len() - 1);
	let voice = &VOICES[i];
	let p = ui.painter();
	p.rect_filled(
		Rect::from_center_size(pos2(rect.left() + 5.0, y + TITLE_H * 0.5), vec2(8.0, 8.0)),
		2.0,
		color(voice),
	);
	let name = p.text(
		pos2(rect.left() + 16.0, y + TITLE_H * 0.5),
		Align2::LEFT_CENTER,
		voice.name,
		theme::semibold(12.5),
		theme::TEXT,
	);
	p.text(
		pos2(name.right() + 8.0, y + TITLE_H * 0.5),
		Align2::LEFT_CENTER,
		voice.note_label,
		theme::mono(10.5),
		theme::TEXT_FAINT,
	);
	y += TITLE_H;
	let picture = Rect::from_min_size(pos2(rect.left(), y), vec2(rect.width(), COMPACT_PICTURE_H));
	big_picture(ui, state, cx, i, picture);
	y = picture.bottom() + GAP;
	let mut knobs = voice_knobs(voice, KnobSize::S);
	knobs.push(KnobDef::new(TONE, "Kit tone", KnobSize::S));
	knobs.push(KnobDef::new(GAIN, "Kit gain", KnobSize::S).step(0.5));
	let cols = devices::small_columns(rect.width(), knobs.len());
	let k = Rect::from_min_max(pos2(rect.left(), y), rect.max);
	let rows = knobs.len().div_ceil(cols);
	let k = k.with_max_y(k.top() + rows as f32 * (devices::knob_height(KnobSize::S) + 4.0));
	devices::knob_grid(ui, state, cx, k, &knobs, cols);
}
