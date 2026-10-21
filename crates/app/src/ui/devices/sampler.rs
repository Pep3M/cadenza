//! The Sampler editor body (design `17 Instruments`, 17e): ZONE MAP | selected zone |
//! GLOBAL · ALL ZONES over a bottom row. The map puts keys across (a piano strip under
//! it, click / drag to audition) and velocity up (labelled axis): zones are coloured
//! boxes ("C3–B3 / 80–127") with a root dot; drag the body to move keys and velocity
//! together, an edge to resize, the dot ⇔ to change the root; wheel zooms the keys,
//! Shift-wheel the velocities; double-click an empty spot = a zone of the last loaded
//! file filling it, double-click a zone = fit it to its neighbours; right-click = auto-map
//! by file name, split velocity, duplicate, delete; ← → ↑ ↓ nudge the selected zone while
//! the editor has the focus; held notes light the map and the keys; dropped files add
//! zones (empty spot), replace a zone's sample (on a zone) or map across the keyboard (on
//! the keys). The selected zone: its waveform with start / end flags, the loop region
//! (flags and the crossfade ramp drag ⇔; wheel zooms, Shift scrolls), Off / Forward /
//! Ping-pong, ⇆ Rev, the root key readout (type a note name) and tune / gain / pan /
//! crossfade knobs. Globals: the amp envelope, the filter (Off / LP / HP / BP and the
//! exact curve with a cutoff / resonance node, wheel = env amount), its knobs or the
//! collapsible filter envelope, and the bottom row (trigger, polyphony, transpose,
//! glide, legato, velocity + curve, key track). Below 1360 px the map goes above the
//! zone and globals, below 560 px everything stacks (both scroll); the inspector shows
//! the compact form (a small map and the zone's main knobs). Every gesture is one undo
//! step; the pure logic lives in [`crate::state::sampler_editor`].

use crate::i18n::{mark, tr, tr_str, trf};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use cadenza_core::TrackId;
use cadenza_model::{Device, LoopMode, SampleZone};
use egui::{
	Align, Align2, Color32, CornerRadius, CursorIcon, Id, Layout, Pos2, Rect, Sense, Shape, Stroke,
	StrokeKind, Ui, UiBuilder, pos2, vec2,
};

use crate::jobs::Peaks;
use crate::state::AppState;
use crate::state::device::KnobSpec;
use crate::state::device_display::{AdsrStage, AdsrValues, DbRange};
use crate::state::dnd::DropTarget;
use crate::state::drum_sampler_editor::{WaveWindow, ms_label};
use crate::state::sampler_editor::{
	self as se, CURVES, CUTOFF, ENV_AMOUNT, Env, FILTER_TYPE, FILTER_TYPES, FILTER_VELOCITY, GLIDE,
	KEY_TRACK, LEGATO, MODE, MapWindow, NodeStart, POLYPHONY, RESONANCE, TRANSPOSE, VEL_CURVE,
	VELOCITY, ZonePart,
};
use crate::state::samples::{self as sm, Marker};
use crate::state::selection::DeviceRef;
use crate::theme;
use crate::ui::device_editor::{BodyCx, DOCK};
use crate::ui::device_kit::{
	self, ControlInfo, FreqDisplay, FreqEvent, FreqNode, Knob, KnobSize, ReadoutLook, ReadoutState,
	ValueFmt,
};
use crate::ui::devices::drum_sampler::{WaveSource, chip_button, dashed_rect, paint_wave};
use crate::ui::devices::{self, KnobDef};
use crate::ui::widgets::Gesture;

const GAP: f32 = 8.0;
const TITLE_H: f32 = 22.0;
const KEYS_H: f32 = 30.0;
const BOTTOM_H: f32 = 34.0;
/// Three columns from this width on; narrower docks put the map above the rest.
const ROW_MIN: f32 = 1360.0;
/// Below this width the sections stack vertically (responsive rule 5).
pub const STACK_BELOW: f32 = 560.0;
const EDGE: f32 = 5.0;
const WHEEL_ZOOM: f32 = 0.85;
const WAVE_ZOOM: f64 = 0.85;
const WHEEL_REST: f64 = 0.5;

const VIOLET: Color32 = theme::BAND_COLORS[6];
const VIOLET_LIGHT: Color32 = Color32::from_rgb(0xc3, 0xb8, 0xff);
const AMP_GREEN: Color32 = theme::PLAYING;
const RING: Color32 = Color32::from_rgb(0x1d, 0x1f, 0x23);
const GRID: Color32 = Color32::from_rgb(0x15, 0x17, 0x1a);
const OCTAVE: Color32 = Color32::from_rgb(0x1f, 0x21, 0x25);
const AXIS: Color32 = Color32::from_rgb(0x55, 0x58, 0x5f);
const KEY_WHITE: Color32 = Color32::from_rgb(0xc4, 0xc7, 0xcd);
const KEY_BLACK: Color32 = Color32::from_rgb(0x1a, 0x1b, 0x1f);
const KEYS_BG: Color32 = Color32::from_rgb(0x0e, 0x0f, 0x11);
const ZONE_TEXT: Color32 = Color32::from_rgb(0xc4, 0xc7, 0xcd);
const NAME: Color32 = Color32::from_rgb(0xf4, 0xf5, 0xf7);
const SHADE: Color32 = Color32::from_rgba_premultiplied(0x08, 0x08, 0x0a, 0xb8);
const END_FG: Color32 = Color32::from_rgb(0xff, 0x8a, 0x87);
const END_BG: Color32 = Color32::from_rgb(0x3a, 0x1d, 0x1f);

fn zone_color(slot: usize) -> Color32 {
	theme::BAND_COLORS[slot]
}

fn note_names() -> &'static [String] {
	static NAMES: OnceLock<Vec<String>> = OnceLock::new();
	NAMES.get_or_init(|| (0..128u8).map(cadenza_model::note_name).collect())
}

// --- Hit areas (drops, tests) -------------------------------------------------------

/// What the sampler body leaves for the drop plumbing and the tests.
#[derive(Default)]
pub struct ZoneHits {
	/// Zone boxes of the last frame (drawn order: the last one is on top).
	pub zones: Vec<(Rect, usize)>,
	/// The map and the window it showed.
	pub map: Option<(Rect, MapWindow)>,
	/// The piano strip under the map.
	pub keys: Option<Rect>,
	/// The selected zone's waveform.
	pub wave: Option<Rect>,
	/// The note a press on the map or the keys holds (released when the dock hides).
	held: Option<(TrackId, u8)>,
}

impl ZoneHits {
	pub fn clear(&mut self) {
		self.zones.clear();
		self.map = None;
		self.keys = None;
		self.wave = None;
	}

	/// Releases the note a press still holds.
	pub fn release(&mut self, state: &mut AppState) {
		if let Some((track, note)) = self.held.take() {
			state.preview_note_off(track, note);
		}
	}

	/// The drop target under `pos`: a zone (replace its sample), the keys (map across the
	/// keyboard) or an empty spot of the map (a new zone filling it).
	pub fn target_at(&self, device: DeviceRef, pos: Pos2) -> Option<DropTarget> {
		if let Some((_, zone)) = self.zones.iter().rev().find(|(r, _)| r.contains(pos)) {
			return Some(DropTarget::Zone {
				device,
				zone: Some(*zone),
			});
		}
		if self.keys.is_some_and(|r| r.contains(pos)) {
			return Some(DropTarget::Zone { device, zone: None });
		}
		let (map, w) = self.map.filter(|(r, _)| r.contains(pos))?;
		let (x, y) = unit(map, pos);
		Some(DropTarget::ZoneArea {
			device,
			key: w.key_index(x),
			vel: w.vel_index(y),
		})
	}

	/// The rect a drop on `target` lands in.
	pub fn target_rect(&self, target: DropTarget) -> Option<Rect> {
		match target {
			DropTarget::Zone { zone: Some(i), .. } => {
				self.zones.iter().find(|(_, z)| *z == i).map(|(r, _)| *r)
			}
			DropTarget::Zone { zone: None, .. } => self.keys,
			DropTarget::ZoneArea { .. } => self.map.map(|(r, _)| r),
			_ => None,
		}
	}
}

fn unit(rect: Rect, pos: Pos2) -> (f32, f32) {
	(
		((pos.x - rect.left()) / rect.width()).clamp(0.0, 0.9999),
		((pos.y - rect.top()) / rect.height()).clamp(0.0, 0.9999),
	)
}

// --- View state (egui memory) --------------------------------------------------------

#[derive(Debug, Clone, Copy)]
struct ZoneDrag {
	index: usize,
	part: ZonePart,
	origin: Pos2,
	keys: i32,
	vels: i32,
}

#[derive(Debug, Clone, Copy)]
struct WheelEdit {
	start: f32,
	notches: f32,
	value: f32,
	last: f64,
}

#[derive(Default)]
struct SamplerView {
	window: Option<MapWindow>,
	/// The zone count the window was fitted to (refit when zones come or go).
	fitted: usize,
	drag: Option<ZoneDrag>,
	/// The part a press on a zone grabbed (until the drag starts).
	pressed: Option<(usize, ZonePart)>,
	flag: Option<(Marker, f64)>,
	xfade: Option<f64>,
	wave: Option<(PathBuf, WaveWindow)>,
	node: Option<NodeStart>,
	wheel: Option<WheelEdit>,
	readout: Option<(&'static str, f32, f32)>,
	/// The note a press on the map or the keys sounds until the button goes up.
	held: Option<(TrackId, u8)>,
	filter_env: bool,
}

type Shared = Arc<Mutex<SamplerView>>;

fn view(ctx: &egui::Context, salt: &str, r: DeviceRef) -> Shared {
	let id = Id::new(("sampler-editor", salt, r.device));
	ctx.data_mut(|d| {
		d.get_temp_mut_or_insert_with(id, || Arc::new(Mutex::new(SamplerView::default())))
			.clone()
	})
}

fn lock(view: &Shared) -> std::sync::MutexGuard<'_, SamplerView> {
	view.lock().unwrap_or_else(|e| e.into_inner())
}

/// Opens or folds the globals' filter envelope (screenshots).
pub fn set_filter_envelope(ctx: &egui::Context, salt: &str, r: DeviceRef, open: bool) {
	lock(&view(ctx, salt, r)).filter_env = open;
}

/// ← → move the selected zone a key (Shift: an octave), ↑ ↓ a velocity step (Shift: 8),
/// while the editor has the focus; one undo step per press.
pub fn shortcuts(ctx: &egui::Context, state: &mut AppState, r: DeviceRef) {
	let Some(count) = state.device(r).map(|d| d.zones.len()) else {
		return;
	};
	if count == 0 {
		return;
	}
	let index = state.view.instrument.zone.min(count - 1);
	let keys = crate::ui::device_editor::arrows(ctx);
	if keys.h == 0 && keys.v == 0 {
		return;
	}
	if keys.alt {
		// Alt: move the zone (Shift: an octave / 8 velocity steps).
		let (dk, dv) = if keys.shift {
			(keys.h * 12, keys.v * 8)
		} else {
			(keys.h, keys.v)
		};
		let cmd = se::drag_command(state.project(), r, index, ZonePart::Body, dk, dv)
			.map(|c| crate::state::drum_sampler_editor::labeled(c, "Nudge zone".to_owned()));
		state.apply_sample_edit(cmd);
		return;
	}
	// ← → the zone, ↑ ↓ its gain.
	state.view.instrument.zone = (index as i32 + keys.h).clamp(0, count as i32 - 1) as usize;
	if keys.v != 0 {
		let step = if keys.shift { 0.1 } else { 0.5 } * keys.v as f32;
		let cmd = crate::state::samples::edit_zone(state.project(), r, index, |z| {
			z.gain_db = (((z.gain_db + step) * 10.0).round() / 10.0).clamp(-60.0, 24.0);
		})
		.map(|c| crate::state::drum_sampler_editor::labeled(c, "Zone gain".to_owned()));
		state.apply_sample_edit(cmd);
	}
}

fn value(state: &AppState, cx: &BodyCx, id: &str) -> f32 {
	devices::shown(state, cx.r, cx.info, id)
}

// --- Body ------------------------------------------------------------------------------

pub fn sampler_body(ui: &mut Ui, state: &mut AppState, cx: &BodyCx, hits: &mut ZoneHits) {
	hits.clear();
	let Some(d) = state.device(cx.r).cloned() else {
		return;
	};
	let shared = view(ui.ctx(), cx.salt, cx.r);
	let mut v = lock(&shared);
	if !ui.input(|i| i.pointer.any_down())
		&& let Some((track, note)) = v.held.take()
	{
		state.preview_note_off(track, note);
	}
	finish_wheel(ui, state, cx, &mut v);
	if v.window.is_none() || v.fitted != d.zones.len() {
		v.window = Some(MapWindow::fit(&d.zones));
		v.fitted = d.zones.len();
	}
	let rect = cx.rect;
	if cx.salt != DOCK {
		let height = compact_height(rect.width());
		devices::compact_scroll(ui, cx, height, |ui, rect| {
			compact(ui, state, cx, &mut v, hits, &d, rect);
		});
	} else if rect.width() < STACK_BELOW {
		scrolled(ui, cx, stacked_height(), |ui, area| {
			stacked(ui, state, cx, &mut v, hits, &d, area);
		});
	} else if rect.width() < ROW_MIN {
		let h = two_rows_height(rect.height());
		scrolled(ui, cx, h, |ui, area| {
			two_rows(ui, state, cx, &mut v, hits, &d, area);
		});
	} else {
		row(ui, state, cx, &mut v, hits, &d, rect);
	}
	hits.held = v.held;
	if !state.live_voices(cx.r.device).is_idle() {
		ui.ctx().request_repaint();
	}
}

/// `add` in a vertical scroll area over the body, `height` tall.
fn scrolled(ui: &mut Ui, cx: &BodyCx, height: f32, add: impl FnOnce(&mut Ui, Rect)) {
	let mut inner = ui.new_child(
		UiBuilder::new()
			.max_rect(cx.rect)
			.layout(Layout::top_down(Align::Min)),
	);
	egui::ScrollArea::vertical()
		.id_salt(("sampler-scroll", cx.salt, cx.r.device))
		.auto_shrink([false, false])
		.show(&mut inner, |ui| {
			let w = ui.available_width();
			let (area, _) = ui.allocate_exact_size(vec2(w, height), Sense::hover());
			add(ui, area.shrink2(vec2(2.0, 0.0)));
		});
}

/// Wide docks: ZONE MAP | selected zone | GLOBAL over the bottom row.
fn row(
	ui: &mut Ui,
	state: &mut AppState,
	cx: &BodyCx,
	v: &mut SamplerView,
	hits: &mut ZoneHits,
	d: &Device,
	rect: Rect,
) {
	let bottom = Rect::from_min_max(pos2(rect.left(), rect.bottom() - BOTTOM_H), rect.max);
	let top = rect.with_max_y(bottom.top() - 6.0);
	let map_w = (rect.width() * 0.47).clamp(520.0, 900.0);
	let zone_w = (rect.width() * 0.29).clamp(400.0, 560.0);
	let map = top.with_max_x(top.left() + map_w);
	let zone = Rect::from_min_max(
		pos2(map.right() + 10.0, top.top()),
		pos2(map.right() + 10.0 + zone_w, top.bottom()),
	);
	let globals = Rect::from_min_max(pos2(zone.right() + 20.0, top.top()), top.max);
	zone_map_section(ui, state, cx, v, hits, d, map);
	zone_panel(ui, state, cx, v, hits, d, zone);
	ui.painter()
		.vline(zone.right() + 10.0, top.y_range(), Stroke::new(1.0, RING));
	globals_section(ui, state, cx, v, globals);
	ui.painter().hline(
		(rect.left() - 8.0)..=(rect.right() + 8.0),
		bottom.top() - 2.0,
		Stroke::new(1.0, RING),
	);
	bottom_row(ui, state, cx, v, bottom.shrink2(vec2(2.0, 0.0)));
}

const TWO_ROW_MAP: f32 = 232.0;
const TWO_ROW_PANEL: f32 = 270.0;
const WRAP_H: f32 = 64.0;

fn two_rows_height(body: f32) -> f32 {
	(TWO_ROW_MAP + GAP + TWO_ROW_PANEL + GAP + WRAP_H).max(body)
}

/// Mid-width docks: the map on top, the zone | globals under it, the bottom row wrapped.
fn two_rows(
	ui: &mut Ui,
	state: &mut AppState,
	cx: &BodyCx,
	v: &mut SamplerView,
	hits: &mut ZoneHits,
	d: &Device,
	area: Rect,
) {
	let map = Rect::from_min_size(area.min, vec2(area.width(), TWO_ROW_MAP));
	zone_map_section(ui, state, cx, v, hits, d, map);
	let second = Rect::from_min_size(
		pos2(area.left(), map.bottom() + GAP),
		vec2(area.width(), TWO_ROW_PANEL),
	);
	let half = (second.width() - 20.0) * 0.52;
	let zone = second.with_max_x(second.left() + half);
	let globals = second.with_min_x(zone.right() + 20.0);
	zone_panel(ui, state, cx, v, hits, d, zone);
	ui.painter().vline(
		zone.right() + 10.0,
		second.y_range(),
		Stroke::new(1.0, RING),
	);
	globals_section(ui, state, cx, v, globals);
	let bottom = Rect::from_min_max(pos2(area.left(), second.bottom() + GAP), area.max);
	ui.painter()
		.hline(bottom.x_range(), bottom.top() - 4.0, Stroke::new(1.0, RING));
	bottom_row(ui, state, cx, v, bottom);
}

const STACK_MAP: f32 = 210.0;
const STACK_ZONE: f32 = 280.0;
const STACK_GLOBALS: f32 = 300.0;
const STACK_BOTTOM: f32 = 100.0;

fn stacked_height() -> f32 {
	STACK_MAP + STACK_ZONE + STACK_GLOBALS + STACK_BOTTOM + 3.0 * 18.0
}

/// Narrow docks: map / zone / globals / bottom row stacked (scrolls).
fn stacked(
	ui: &mut Ui,
	state: &mut AppState,
	cx: &BodyCx,
	v: &mut SamplerView,
	hits: &mut ZoneHits,
	d: &Device,
	area: Rect,
) {
	let mut y = area.top();
	let mut next = |h: f32| {
		let r = Rect::from_min_size(pos2(area.left(), y), vec2(area.width(), h));
		y += h + 18.0;
		r
	};
	let map = next(STACK_MAP);
	let zone = next(STACK_ZONE);
	let globals = next(STACK_GLOBALS);
	let bottom = next(STACK_BOTTOM);
	for r in [zone, globals, bottom] {
		ui.painter()
			.hline(r.x_range(), r.top() - 9.0, Stroke::new(1.0, RING));
	}
	zone_map_section(ui, state, cx, v, hits, d, map);
	zone_panel(ui, state, cx, v, hits, d, zone);
	globals_section(ui, state, cx, v, globals);
	bottom_row(ui, state, cx, v, bottom);
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

// --- Zone map -------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn zone_map_section(
	ui: &mut Ui,
	state: &mut AppState,
	cx: &BodyCx,
	v: &mut SamplerView,
	hits: &mut ZoneHits,
	d: &Device,
	rect: Rect,
) {
	let t = title(ui, rect, tr("ZONE MAP"));
	let hint =
		tr("x = key \u{b7} y = velocity \u{b7} drag to move, edges to resize, drop files to add");
	let room = rect.right() - t.right() - 12.0;
	if room > 120.0 {
		let hint = crate::ui::mixer::elide(ui, hint, &theme::medium(11.0), room);
		ui.painter().text(
			pos2(t.right() + 10.0, rect.top() + TITLE_H * 0.5),
			Align2::LEFT_CENTER,
			hint,
			theme::medium(11.0),
			theme::TEXT_FAINT,
		);
	}
	let body = Rect::from_min_max(pos2(rect.left(), rect.top() + TITLE_H + 2.0), rect.max);
	zone_map(ui, state, cx, v, hits, d, body, false);
}

/// The map (zones over the key × velocity grid) with its piano strip at the bottom of
/// `rect`; `mini` = the inspector's (no labels inside small zones, a thinner strip).
#[allow(clippy::too_many_arguments)]
fn zone_map(
	ui: &mut Ui,
	state: &mut AppState,
	cx: &BodyCx,
	v: &mut SamplerView,
	hits: &mut ZoneHits,
	d: &Device,
	rect: Rect,
	mini: bool,
) {
	let keys_h = if mini { 18.0 } else { KEYS_H };
	let map = Rect::from_min_max(rect.min, pos2(rect.right(), rect.bottom() - keys_h));
	let strip = Rect::from_min_max(pos2(rect.left(), map.bottom()), rect.max);
	let mut w = v.window.unwrap_or_else(|| MapWindow::fit(&d.zones));
	// Lit: previews on the track and the keys / zones the processor plays (playback,
	// live MIDI input).
	let live = state.live_voices(cx.r.device);
	let held = state.lit_keys(cx.r, &live);
	let previews = state.lit_keys(cx.r, &Default::default());
	let selected = state
		.view
		.instrument
		.zone
		.min(d.zones.len().saturating_sub(1));
	// The background: audition, new zones, zoom.
	let bg = ui.interact(
		map,
		Id::new(("smp-map", cx.salt, cx.r.device)),
		Sense::click_and_drag(),
	);
	if let Some(pos) = ui.ctx().pointer_latest_pos()
		&& rect.contains(pos)
	{
		let scroll = ui.input(|i| i.smooth_scroll_delta.x + i.smooth_scroll_delta.y);
		if scroll != 0.0 {
			ui.input_mut(|i| i.smooth_scroll_delta = egui::Vec2::ZERO);
			let factor = WHEEL_ZOOM.powf(scroll / device_kit::NOTCH);
			let (x, y) = unit(map, pos);
			w = if ui.input(|i| i.modifiers.shift) {
				w.zoom_vels(w.vel_at(y), factor)
			} else {
				w.zoom_keys(w.key_at(x), factor)
			};
		}
	}
	v.window = Some(w);
	hits.map = Some((map, w));
	hits.keys = Some(strip);
	let key_px = map.width() / w.keys();
	let vel_px = map.height() / w.vels();
	let x_of = |k: f32| map.left() + w.x(k) * map.width();
	let y_of = |vel: f32| map.top() + w.y(vel) * map.height();
	let p = ui.painter().with_clip_rect(map);
	p.rect(
		map,
		CornerRadius {
			nw: theme::RADIUS_MD,
			ne: theme::RADIUS_MD,
			..CornerRadius::ZERO
		},
		theme::DISPLAY_BG,
		Stroke::new(1.0, RING),
		StrokeKind::Inside,
	);
	let first = w.key_from.floor() as i32;
	let last = w.key_to.ceil() as i32;
	for k in first..=last {
		let oct = k % 12 == 0;
		if !oct && key_px < 5.0 {
			continue;
		}
		p.vline(
			x_of(k as f32),
			map.y_range(),
			Stroke::new(1.0, if oct { OCTAVE } else { GRID }),
		);
	}
	for vel in [32.0, 64.0, 96.0] {
		if vel > w.vel_from && vel < w.vel_to {
			p.hline(map.x_range(), y_of(vel), Stroke::new(1.0, GRID));
		}
	}
	for n in &held {
		let col = Rect::from_x_y_ranges(
			x_of(f32::from(*n))..=x_of(f32::from(*n) + 1.0),
			map.y_range(),
		);
		p.rect_filled(col, 0.0, theme::ACCENT.gamma_multiply(0.14));
	}
	if bg.is_pointer_button_down_on()
		&& v.held.is_none()
		&& ui.input(|i| i.pointer.primary_pressed())
		&& let (Some(pos), Some(track)) = (ui.ctx().pointer_interact_pos(), cx.r.track)
	{
		let (x, y) = unit(map, pos);
		let vel = (f32::from(w.vel_index(y)) / 127.0).max(0.05);
		let key = w.key_index(x);
		state.preview_note_on(track, key, vel);
		v.held = Some((track, key));
	}
	if bg.double_clicked()
		&& let Some(pos) = bg.interact_pointer_pos()
	{
		let (x, y) = unit(map, pos);
		match state.zone_source(cx.r) {
			Some(path) => {
				state.add_zone_at(cx.r, &path, w.key_index(x), w.vel_index(y));
			}
			None => state
				.notices
				.info(tr("Drop an audio file on the map to make the first zone")),
		}
	}
	let _ = bg.on_hover_text_at_pointer(tr(
		"Click to hear this key at this velocity \u{b7} double-click: a new zone of the last \
		 loaded file \u{b7} wheel zooms the keys, Shift+wheel the velocities",
	));
	let colors = se::zone_colors(&d.zones);
	let mut order: Vec<usize> = (0..d.zones.len()).filter(|i| *i != selected).collect();
	if selected < d.zones.len() {
		order.push(selected);
	}
	for i in order {
		let zone = match v.drag.filter(|g| g.index == i) {
			Some(g) => se::drag_zone(&d.zones[i], g.part, g.keys, g.vels),
			None => d.zones[i].clone(),
		};
		let b = w.zone_box(&zone);
		let zr = Rect::from_min_max(
			pos2(
				map.left() + b.left * map.width(),
				map.top() + b.top * map.height(),
			),
			pos2(
				map.left() + b.right * map.width(),
				map.top() + b.bottom * map.height(),
			),
		)
		.shrink(1.0);
		let shown = zr.intersect(map);
		if shown.width() <= 1.0 || shown.height() <= 1.0 {
			continue;
		}
		let c = zone_color(colors[i]);
		let root_x = (map.left() + w.key_center(zone.root_key) * map.width())
			.clamp(zr.left() + 5.0, (zr.right() - 5.0).max(zr.left() + 5.0));
		let root = pos2(root_x, zr.bottom() - 6.5);
		// The processor's voices say which velocity layer sounds; previews light the
		// key range at once (before the probe answers).
		let lit = live.sounds(i)
			|| previews
				.iter()
				.any(|n| (zone.key_low..=zone.key_high).contains(n));
		zone_box(&p, ui, &zone, zr, root, c, i == selected, lit, mini);
		let resp = ui.interact(
			shown,
			Id::new(("smp-zone", cx.salt, cx.r.device, i)),
			Sense::click_and_drag(),
		);
		hits.zones.push((shown, i));
		let label = trf("Zone {n}", &[("n", &(i + 1))]);
		resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &label));
		let hover_part = ui
			.ctx()
			.pointer_latest_pos()
			.map(|pos| part_at(pos, zr, root));
		if let Some(part) = v
			.drag
			.filter(|g| g.index == i)
			.map(|g| g.part)
			.or(hover_part)
			&& (resp.hovered() || resp.dragged())
		{
			ui.ctx().set_cursor_icon(match part {
				ZonePart::Low | ZonePart::High | ZonePart::Root => CursorIcon::ResizeHorizontal,
				ZonePart::Top | ZonePart::Bottom => CursorIcon::ResizeVertical,
				ZonePart::Body if resp.dragged() => CursorIcon::Grabbing,
				ZonePart::Body => CursorIcon::Grab,
			});
		}
		if resp.is_pointer_button_down_on() && ui.input(|i| i.pointer.any_pressed()) {
			state.view.instrument.zone = i;
			let origin = ui
				.input(|i| i.pointer.press_origin())
				.unwrap_or(zr.center());
			v.pressed = Some((i, part_at(origin, zr, root)));
		}
		if resp.drag_started()
			&& let Some((pi, part)) = v.pressed.filter(|(pi, _)| *pi == i)
		{
			let origin = ui
				.input(|i| i.pointer.press_origin())
				.unwrap_or(zr.center());
			v.drag = Some(ZoneDrag {
				index: pi,
				part,
				origin,
				keys: 0,
				vels: 0,
			});
		}
		if resp.dragged()
			&& let (Some(g), Some(pos)) = (v.drag.as_mut(), ui.ctx().pointer_interact_pos())
			&& g.index == i
		{
			let delta = pos - g.origin;
			g.keys = (delta.x / key_px).round() as i32;
			g.vels = match g.part {
				ZonePart::Low | ZonePart::High | ZonePart::Root => 0,
				_ => (-delta.y / vel_px).round() as i32,
			};
			if matches!(g.part, ZonePart::Top | ZonePart::Bottom) {
				g.keys = 0;
			}
			ui.ctx().request_repaint();
		}
		if resp.drag_stopped()
			&& let Some(g) = v.drag.take()
		{
			v.pressed = None;
			let cmd = se::drag_command(state.project(), cx.r, g.index, g.part, g.keys, g.vels);
			state.apply_sample_edit(cmd);
		}
		if resp.double_clicked() {
			let cmd = se::fit_to_neighbours(&d.zones, i)
				.and_then(|zs| se::set(state.project(), cx.r, zs, tr("Fit zone to neighbours")));
			state.apply_sample_edit(cmd);
		}
		resp.context_menu(|ui| zone_menu(ui, state, cx.r, d, i));
		let _ = resp.on_hover_text(trf("Zone {zone}: {display_name} — {zone_title}, root {note_name}\nDrag to move · edges resize · the dot sets the root · double-click fits it to its neighbours · right-click for more", &[("zone", &(i + 1)), ("display_name", &zone.sample.display_name()), ("zone_title", &se::zone_title(&zone)), ("note_name", &cadenza_model::note_name(zone.root_key))]));
	}
	if !ui.input(|i| i.pointer.any_down()) {
		v.pressed = None;
		v.drag = None;
	}
	// Velocity axis.
	if !mini {
		for vel in w.vel_labels() {
			let y = (y_of(f32::from(vel) + 1.0) + 2.0).clamp(map.top() + 2.0, map.bottom() - 12.0);
			p.text(
				pos2(map.right() - 6.0, y),
				Align2::RIGHT_TOP,
				vel.to_string(),
				theme::mono(10.0),
				AXIS,
			);
		}
	}
	if d.zones.is_empty() {
		p.text(
			map.center(),
			Align2::CENTER_CENTER,
			tr(
				"No zones \u{2014} drop audio files here (a zone each) or on the keys (across the \
			 keyboard)",
			),
			theme::medium(11.5),
			theme::TEXT_FAINT,
		);
	}
	keys_strip(ui, state, cx, v, d, &w, strip, &held, selected, &colors);
}

/// What a press at `pos` on zone box `zr` grabs.
fn part_at(pos: Pos2, zr: Rect, root: Pos2) -> ZonePart {
	if pos.distance(root) <= 6.0 {
		return ZonePart::Root;
	}
	let edge = EDGE.min(zr.width() * 0.25);
	if pos.x <= zr.left() + edge {
		return ZonePart::Low;
	}
	if pos.x >= zr.right() - edge {
		return ZonePart::High;
	}
	let vedge = 4.0_f32.min(zr.height() * 0.2);
	if pos.y <= zr.top() + vedge {
		return ZonePart::Top;
	}
	if pos.y >= zr.bottom() - vedge {
		return ZonePart::Bottom;
	}
	ZonePart::Body
}

#[allow(clippy::too_many_arguments)]
fn zone_box(
	p: &egui::Painter,
	ui: &Ui,
	z: &SampleZone,
	zr: Rect,
	root: Pos2,
	c: Color32,
	selected: bool,
	lit: bool,
	mini: bool,
) {
	let upper = se::vel_step(z.vel_low) >= 64;
	let alpha = if selected {
		0.25
	} else if upper {
		0.14
	} else {
		0.09
	} + if lit { 0.12 } else { 0.0 };
	let r = CornerRadius::same(theme::RADIUS);
	p.rect_filled(zr, r, c.gamma_multiply(alpha));
	if selected {
		p.rect_stroke(
			zr.expand(1.0),
			CornerRadius::same(theme::RADIUS + 1),
			Stroke::new(1.0, theme::DISPLAY_BG),
			StrokeKind::Outside,
		);
		p.rect_stroke(zr, r, Stroke::new(1.5, c), StrokeKind::Inside);
	} else {
		p.rect_stroke(
			zr,
			r,
			Stroke::new(1.0, c.gamma_multiply(if lit { 0.8 } else { 0.4 })),
			StrokeKind::Inside,
		);
	}
	let inside = (z.key_low..=z.key_high).contains(&z.root_key);
	if inside {
		p.circle(root, 3.5, c, Stroke::new(1.5, theme::DISPLAY_BG));
	} else {
		p.circle_stroke(root, 3.0, Stroke::new(1.25, c));
	}
	let room = zr.width() - 10.0;
	if room < 22.0 || zr.height() < 18.0 || (mini && zr.height() < 26.0) {
		return;
	}
	let font = theme::mono(10.5);
	let fits = |t: &str| {
		ui.painter()
			.layout_no_wrap(t.to_owned(), font.clone(), ZONE_TEXT)
			.size()
			.x <= room
	};
	let full = se::key_range_label(z);
	let low = cadenza_model::note_name(z.key_low);
	let text = if fits(&full) {
		full
	} else if fits(&low) {
		low
	} else {
		return;
	};
	p.text(
		zr.left_top() + vec2(6.0, 4.0),
		Align2::LEFT_TOP,
		text,
		font,
		if selected { NAME } else { ZONE_TEXT },
	);
	let vel = se::vel_range_label(z);
	let vel_fits = ui
		.painter()
		.layout_no_wrap(vel.clone(), theme::medium(10.5), ZONE_TEXT)
		.size()
		.x <= room;
	if zr.height() >= 36.0 && vel_fits {
		p.text(
			zr.left_top() + vec2(6.0, 18.0),
			Align2::LEFT_TOP,
			vel,
			theme::medium(10.5),
			theme::TEXT_DIM,
		);
	}
}

/// The zone's right-click menu.
fn zone_menu(ui: &mut Ui, state: &mut AppState, r: DeviceRef, d: &Device, i: usize) {
	ui.set_min_width(190.0);
	if ui.button(tr("Auto-map by file name")).clicked() {
		let cmd = se::set(
			state.project(),
			r,
			se::auto_map(&d.zones),
			tr("Auto-map zones by name"),
		);
		if !state.apply_sample_edit(cmd) {
			state
				.notices
				.info(tr("The zones already sit where their file names put them"));
		}
		ui.close();
	}
	let split = se::split_velocity(&d.zones, i);
	if ui
		.add_enabled(split.is_some(), egui::Button::new(tr("Split velocity")))
		.clicked()
	{
		let cmd = split.and_then(|zs| se::set(state.project(), r, zs, tr("Split zone velocity")));
		state.apply_sample_edit(cmd);
		ui.close();
	}
	if ui.button(tr("Duplicate")).clicked() {
		let cmd = se::duplicate(&d.zones, i)
			.and_then(|zs| se::set(state.project(), r, zs, tr("Duplicate zone")));
		if state.apply_sample_edit(cmd) {
			state.view.instrument.zone = i + 1;
		}
		ui.close();
	}
	if ui.button(tr("Fit to neighbours")).clicked() {
		let cmd = se::fit_to_neighbours(&d.zones, i)
			.and_then(|zs| se::set(state.project(), r, zs, tr("Fit zone to neighbours")));
		state.apply_sample_edit(cmd);
		ui.close();
	}
	let z = &d.zones[i];
	ui.menu_button(tr("Round-robin group"), |ui| {
		for g in 0..=8u8 {
			let label = if g == 0 {
				tr("Off").to_owned()
			} else {
				trf("Group {g}", &[("g", &g)])
			};
			if ui.radio(z.round_robin == g, label).clicked() {
				let cmd = se::edit(state.project(), r, i, tr("Zone round-robin"), |z| {
					z.round_robin = g
				});
				state.apply_sample_edit(cmd);
				ui.close();
			}
		}
	});
	if ui.button(tr("Locate file")).clicked() {
		state.locate_file(&z.sample.path);
		ui.close();
	}
	ui.separator();
	if ui.button(tr("Delete")).clicked() {
		let cmd = sm::remove_zone(state.project(), r, i)
			.map(|c| crate::state::drum_sampler_editor::labeled(c, "Delete zone".to_owned()));
		if state.apply_sample_edit(cmd) {
			state.view.instrument.zone = i.saturating_sub(1);
		}
		ui.close();
	}
}

/// The piano strip under the map (same key columns): click / drag auditions, held notes
/// light, the selected zone's range is marked along its top.
#[allow(clippy::too_many_arguments)]
fn keys_strip(
	ui: &mut Ui,
	state: &mut AppState,
	cx: &BodyCx,
	v: &mut SamplerView,
	d: &Device,
	w: &MapWindow,
	strip: Rect,
	held: &[u8],
	selected: usize,
	colors: &[usize],
) {
	let x_of = |k: f32| strip.left() + w.x(k) * strip.width();
	let key_px = strip.width() / w.keys();
	let p = ui.painter().with_clip_rect(strip);
	p.rect_filled(
		strip,
		CornerRadius {
			sw: theme::RADIUS_MD,
			se: theme::RADIUS_MD,
			..CornerRadius::ZERO
		},
		KEYS_BG,
	);
	let first = w.key_from.floor().max(0.0) as u8;
	let last = (w.key_to.ceil() as i32).min(127) as u8;
	for k in first..=last {
		let black = crate::state::poly_synth_editor::is_black(k);
		let on = held.contains(&k);
		let kr = Rect::from_x_y_ranges(
			x_of(f32::from(k))..=(x_of(f32::from(k) + 1.0) - 1.0),
			strip.y_range(),
		);
		let fill = match (black, on) {
			(_, true) => theme::ACCENT,
			(true, false) => KEY_BLACK,
			(false, false) => KEY_WHITE,
		};
		p.rect_filled(kr, 0.0, fill);
		if k % 12 == 0 && key_px * 12.0 >= 26.0 {
			p.text(
				pos2(kr.left() + 1.0, strip.bottom() - 1.0),
				Align2::LEFT_BOTTOM,
				cadenza_model::note_name(k),
				theme::medium(8.5),
				if on { theme::TEXT_ON_ACCENT } else { AXIS },
			);
		}
	}
	if let Some(z) = d.zones.get(selected) {
		let bar = Rect::from_x_y_ranges(
			x_of(f32::from(z.key_low))..=x_of(f32::from(z.key_high) + 1.0),
			strip.top()..=strip.top() + 2.0,
		);
		p.rect_filled(bar, 0.0, zone_color(colors[selected]));
	}
	let resp = ui
		.interact(
			strip,
			Id::new(("smp-keys", cx.salt, cx.r.device)),
			Sense::click_and_drag(),
		)
		.on_hover_cursor(CursorIcon::PointingHand);
	resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, tr("Zone keys")));
	if resp.is_pointer_button_down_on()
		&& let (Some(pos), Some(track)) = (ui.ctx().pointer_interact_pos(), cx.r.track)
	{
		let (x, _) = unit(strip, pos);
		let k = w.key_index(x);
		if v.held.map(|(_, n)| n) != Some(k) {
			if let Some((t, n)) = v.held.take() {
				state.preview_note_off(t, n);
			}
			state.preview_note_on(track, k, 0.8);
			v.held = Some((track, k));
		}
	}
	let _ = resp.on_hover_text(tr(
		"Click or drag to play \u{b7} drop an audio file to map it across the keyboard",
	));
}

// --- Selected zone --------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn zone_panel(
	ui: &mut Ui,
	state: &mut AppState,
	cx: &BodyCx,
	v: &mut SamplerView,
	hits: &mut ZoneHits,
	d: &Device,
	rect: Rect,
) {
	let Some(index) = d
		.zones
		.len()
		.checked_sub(1)
		.map(|n| state.view.instrument.zone.min(n))
	else {
		let p = ui.painter();
		p.rect_filled(
			rect,
			CornerRadius::same(theme::RADIUS_MD),
			theme::DISPLAY_BG,
		);
		dashed_rect(p, rect.shrink(6.0), Stroke::new(1.0, theme::BG_PRESSED));
		p.text(
			rect.center(),
			Align2::CENTER_CENTER,
			tr("No zone selected \u{2014} drop an audio file on the map"),
			theme::medium(12.0),
			theme::TEXT_FAINT,
		);
		return;
	};
	let zone = d.zones[index].clone();
	let c = zone_color(se::zone_colors(&d.zones)[index]);
	let head = Rect::from_min_size(rect.min, vec2(rect.width(), TITLE_H));
	zone_header(ui, state, cx, index, &zone, head, c);
	let knob_h = devices::knob_height(KnobSize::S);
	let knobs = Rect::from_min_max(pos2(rect.left(), rect.bottom() - knob_h), rect.max);
	let wave = Rect::from_min_max(
		pos2(rect.left(), head.bottom() + 6.0),
		pos2(rect.right(), (knobs.top() - 4.0).max(head.bottom() + 46.0)),
	);
	wave_editor(ui, state, cx, v, hits, index, &zone, wave, c);
	zone_knobs(ui, state, cx, index, &zone, knobs, c);
}

/// "■ Felt upright F#3 hard.wav  C3–B3 · vel 80–127 … [Off | Forward | Ping-pong] ⇆ Rev".
#[allow(clippy::too_many_arguments)]
fn zone_header(
	ui: &mut Ui,
	state: &mut AppState,
	cx: &BodyCx,
	index: usize,
	zone: &SampleZone,
	rect: Rect,
	c: Color32,
) {
	let cy = rect.center().y;
	let rev = Rect::from_min_max(
		pos2(rect.right() - 58.0, cy - 10.0),
		pos2(rect.right(), cy + 10.0),
	);
	let resp = chip_button(
		ui,
		Id::new(("smp-rev", cx.salt, cx.r.device)),
		rev,
		tr("\u{21c6} Rev"),
		zone.reverse,
	)
	.on_hover_text(tr("Play the zone backwards (loops are ignored then)"));
	if resp.clicked() {
		let on = !zone.reverse;
		let cmd = se::edit(state.project(), cx.r, index, tr("Zone reverse"), |z| {
			z.reverse = on
		});
		state.apply_sample_edit(cmd);
	}
	let seg_rect = Rect::from_min_max(
		pos2(rect.left(), rect.top() - 1.0),
		pos2(rev.left() - 8.0, rect.bottom() + 1.0),
	);
	let mut child = ui.new_child(
		UiBuilder::new()
			.max_rect(seg_rect)
			.layout(Layout::right_to_left(Align::Center)),
	);
	let modes = [LoopMode::Off, LoopMode::Forward, LoopMode::PingPong];
	let current = modes.iter().position(|m| *m == zone.loop_mode).unwrap_or(0);
	if let Some(i) = device_kit::segmented(
		&mut child,
		Id::new(("smp-loop", cx.salt, cx.r.device)),
		&[tr("Off"), tr("Forward"), tr("Ping-pong")],
		current,
	) && i != current
	{
		let mode = modes[i];
		let cmd = se::edit(state.project(), cx.r, index, tr("Zone loop mode"), |z| {
			z.loop_mode = mode
		});
		state.apply_sample_edit(cmd);
	}
	let seg_left = child.min_rect().left();
	let p = ui.painter();
	p.rect_filled(
		Rect::from_center_size(pos2(rect.left() + 5.0, cy), vec2(10.0, 10.0)),
		2.0,
		c,
	);
	let range = se::zone_title(zone);
	let range_g = p.layout_no_wrap(range, theme::mono(11.0), theme::TEXT_FAINT);
	let room = seg_left - rect.left() - 18.0 - 12.0;
	let full = zone.sample.display_name();
	let full_w = p
		.layout_no_wrap(full.clone(), theme::semibold(13.0), theme::TEXT)
		.size()
		.x;
	// The full name first: the range is on the map too.
	let show_range = full_w + 8.0 + range_g.size().x <= room;
	let name = crate::ui::mixer::elide(ui, &full, &theme::semibold(13.0), room.max(0.0));
	let name_r = Rect::from_min_size(
		pos2(rect.left() + 18.0, rect.top()),
		vec2(
			ui.painter()
				.layout_no_wrap(name.clone(), theme::semibold(13.0), theme::TEXT)
				.size()
				.x,
			rect.height(),
		),
	);
	let p = ui.painter();
	p.text(
		name_r.left_center(),
		Align2::LEFT_CENTER,
		&name,
		theme::semibold(13.0),
		theme::TEXT,
	);
	if show_range {
		p.galley(
			pos2(name_r.right() + 8.0, cy - range_g.size().y * 0.5),
			range_g,
			theme::TEXT_FAINT,
		);
	}
	let resp = ui
		.interact(
			name_r,
			Id::new(("smp-zone-file", cx.salt, cx.r.device)),
			Sense::click(),
		)
		.on_hover_cursor(CursorIcon::PointingHand)
		.on_hover_text(format!(
			"{}\n{}",
			zone.sample.path.display(),
			tr("Click for the file menu \u{b7} drop another file on the zone to replace it")
		));
	egui::Popup::menu(&resp).show(|ui| {
		ui.set_min_width(170.0);
		if ui.button(tr("Locate file")).clicked() {
			state.locate_file(&zone.sample.path);
			ui.close();
		}
		if ui.button(tr("Delete zone")).clicked() {
			let cmd = sm::remove_zone(state.project(), cx.r, index)
				.map(|c| crate::state::drum_sampler_editor::labeled(c, "Delete zone".to_owned()));
			if state.apply_sample_edit(cmd) {
				state.view.instrument.zone = index.saturating_sub(1);
			}
			ui.close();
		}
	});
}

/// The zone's waveform: start / end flags, the loop region with its flags and the
/// crossfade ramp, zoom.
#[allow(clippy::too_many_arguments)]
fn wave_editor(
	ui: &mut Ui,
	state: &mut AppState,
	cx: &BodyCx,
	v: &mut SamplerView,
	hits: &mut ZoneHits,
	index: usize,
	zone: &SampleZone,
	rect: Rect,
	c: Color32,
) {
	hits.wave = Some(rect);
	ui.painter().rect(
		rect,
		CornerRadius::same(theme::RADIUS_MD),
		theme::DISPLAY_BG,
		Stroke::new(1.0, RING),
		StrokeKind::Inside,
	);
	let peaks = match state.peaks.get(&zone.sample.path) {
		Peaks::Ready(p) => p,
		Peaks::Pending => {
			ui.painter().text(
				rect.center(),
				Align2::CENTER_CENTER,
				tr("loading waveform\u{2026}"),
				theme::small(),
				theme::TEXT_FAINT,
			);
			return;
		}
		Peaks::Failed(e) => {
			ui.painter().text(
				rect.center(),
				Align2::CENTER_CENTER,
				trf("⚠ {e} — drop another file on the zone", &[("e", &e)]),
				theme::small(),
				theme::WARNING,
			);
			return;
		}
	};
	let length = peaks.frames as f64 / f64::from(peaks.sample_rate.max(1));
	if length <= 0.0 {
		return;
	}
	let path = zone.sample.path.clone();
	let mut window = match &v.wave {
		Some((p, w)) if *p == path => w.clamped(length),
		_ => WaveWindow::full(length),
	};
	let area = rect.shrink(1.0);
	let x_of =
		|s: f64, w: &WaveWindow| area.left() + ((s - w.from) / w.span()) as f32 * area.width();
	let s_of =
		|x: f32, w: &WaveWindow| w.from + f64::from((x - area.left()) / area.width()) * w.span();
	let bg = ui.interact(
		rect,
		Id::new(("smp-wave", cx.salt, cx.r.device)),
		Sense::hover(),
	);
	let scroll = ui.input(|i| i.smooth_scroll_delta.x + i.smooth_scroll_delta.y);
	if bg.hovered() && scroll != 0.0 {
		ui.input_mut(|i| i.smooth_scroll_delta = egui::Vec2::ZERO);
		let notches = f64::from(scroll / device_kit::NOTCH);
		if ui.input(|i| i.modifiers.shift) {
			window = window.scroll(-notches * window.span() * 0.15, length);
		} else if let Some(pos) = ui.ctx().pointer_latest_pos() {
			window = window.zoom(s_of(pos.x, &window), WAVE_ZOOM.powf(notches), length);
		}
	}
	v.wave = Some((path, window));
	let mut shown = zone.clone();
	if let Some((m, s)) = v.flag {
		m.set(&mut shown, s, length);
	}
	if let Some(xf) = v.xfade {
		shown.loop_crossfade = xf;
	}
	let start = Marker::Start.get(&shown, length);
	let end = Marker::End.get(&shown, length).max(start);
	let looping = !shown.loop_mode.is_off() && !shown.reverse;
	let ls = Marker::LoopStart.get(&shown, length);
	let le = Marker::LoopEnd.get(&shown, length);
	let p = ui.painter().with_clip_rect(area);
	if looping {
		p.rect_filled(
			Rect::from_x_y_ranges(x_of(ls, &window)..=x_of(le, &window), area.y_range()),
			0.0,
			theme::ACCENT.gamma_multiply(0.09),
		);
	}
	p.hline(area.x_range(), area.center().y, Stroke::new(1.0, OCTAVE));
	let wave = Rect::from_center_size(area.center(), vec2(area.width(), area.height() * 0.86));
	let fpp = window.span() * f64::from(peaks.sample_rate.max(1)) / f64::from(area.width());
	let finest = peaks.levels.first().map_or(1, |l| l.frames_per_bucket);
	let samples = match (fpp < f64::from(finest)).then(|| state.peaks.samples(&zone.sample.path)) {
		Some(crate::jobs::Samples::Ready(b)) => Some(b),
		_ => None,
	};
	let src = samples
		.as_deref()
		.map_or(WaveSource::Peaks(&peaks), WaveSource::Samples);
	paint_wave(
		&p,
		wave,
		src,
		length,
		window.from,
		window.to,
		cadenza_core::db_to_gain(shown.gain_db).clamp(0.1, 2.0),
		c.gamma_multiply(0.55),
		false,
		0.004,
	);
	let (sx, ex) = (x_of(start, &window), x_of(end, &window));
	p.rect_filled(
		Rect::from_min_max(area.min, pos2(sx.max(area.left()), area.bottom())),
		0.0,
		SHADE,
	);
	p.rect_filled(
		Rect::from_min_max(pos2(ex.min(area.right()), area.top()), area.max),
		0.0,
		SHADE,
	);
	let xf = se::effective_crossfade(&shown, length);
	let (lsx, lex) = (x_of(ls, &window), x_of(le, &window));
	let foot = pos2(x_of(le - xf, &window), area.bottom() - 6.0);
	if looping && shown.loop_mode == LoopMode::Forward && xf > 0.0 {
		p.add(Shape::convex_polygon(
			vec![
				foot,
				pos2(lex, area.top() + 20.0),
				pos2(lex, area.bottom() - 6.0),
			],
			theme::ACCENT.gamma_multiply(0.16),
			Stroke::new(1.25, theme::ACCENT_TEXT),
		));
	}
	let heads: Vec<f32> = state
		.live_voices(cx.r.device)
		.playheads(index)
		.into_iter()
		.map(|s| x_of(f64::from(s), &window))
		.collect();
	devices::playheads(&p, area, &heads, false);
	let mut flags = vec![
		(Marker::Start, sx, theme::PLAYING),
		(Marker::End, ex, theme::ERROR),
	];
	if looping {
		flags.push((Marker::LoopStart, lsx, theme::ACCENT));
		flags.push((Marker::LoopEnd, lex, theme::ACCENT));
	}
	for (m, x, color) in flags {
		p.vline(x, area.y_range(), Stroke::new(1.5, color));
		let handle = Rect::from_x_y_ranges(x - 5.0..=x + 5.0, area.y_range());
		let resp = ui
			.interact(
				handle,
				Id::new(("smp-flag", cx.salt, cx.r.device, se::marker_label(m))),
				Sense::click_and_drag(),
			)
			.on_hover_cursor(CursorIcon::ResizeHorizontal);
		let label = match m {
			Marker::Start => tr("Start flag"),
			Marker::End => "End flag",
			Marker::LoopStart => tr("Loop start flag"),
			Marker::LoopEnd => tr("Loop end flag"),
		};
		resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Slider, true, label));
		if resp.dragged()
			&& let Some(pos) = ui.ctx().pointer_interact_pos()
		{
			v.flag = Some((m, s_of(pos.x.clamp(area.left(), area.right()), &window)));
			ui.ctx().request_repaint();
		}
		if resp.drag_stopped()
			&& let Some((m, s)) = v.flag.take()
		{
			let cmd = se::edit(state.project(), cx.r, index, se::marker_label(m), |z| {
				m.set(z, s, length)
			});
			state.apply_sample_edit(cmd);
		}
		if resp.double_clicked() {
			v.flag = None;
			let cmd = se::edit(
				state.project(),
				cx.r,
				index,
				se::marker_label(m),
				|z| match m {
					Marker::Start => z.start = 0.0,
					Marker::End => z.end = None,
					Marker::LoopStart => z.loop_start = z.start,
					Marker::LoopEnd => z.loop_end = None,
				},
			);
			state.apply_sample_edit(cmd);
		}
		let _ = resp.on_hover_text(match m {
			Marker::Start => tr("Start: drag where the zone starts playing, double-click resets"),
			Marker::End => tr("End: drag where it stops, double-click resets to the sample end"),
			Marker::LoopStart => tr("Loop start: drag, double-click puts it on the start flag"),
			Marker::LoopEnd => tr("Loop end: drag, double-click puts it on the end flag"),
		});
	}
	if looping && shown.loop_mode == LoopMode::Forward {
		let hit = Rect::from_center_size(foot, vec2(14.0, 14.0));
		let resp = ui
			.interact(
				hit,
				Id::new(("smp-xfade", cx.salt, cx.r.device)),
				Sense::click_and_drag(),
			)
			.on_hover_cursor(CursorIcon::ResizeHorizontal);
		resp.widget_info(|| {
			egui::WidgetInfo::labeled(egui::WidgetType::Slider, true, tr("Crossfade"))
		});
		let active = resp.hovered() || resp.dragged();
		p.circle(
			foot,
			if active { 5.0 } else { 4.0 },
			theme::ACCENT_TEXT,
			Stroke::new(1.5, theme::DISPLAY_BG),
		);
		if resp.dragged()
			&& let Some(pos) = ui.ctx().pointer_interact_pos()
		{
			v.xfade = Some(se::crossfade_drag(zone, s_of(pos.x, &window), length));
			ui.ctx().request_repaint();
		}
		if resp.drag_stopped()
			&& let Some(xf) = v.xfade.take()
		{
			let cmd = se::edit(state.project(), cx.r, index, tr("Loop crossfade"), |z| {
				z.loop_crossfade = xf
			});
			state.apply_sample_edit(cmd);
		}
		if resp.double_clicked() {
			v.xfade = None;
			let cmd = se::edit(state.project(), cx.r, index, tr("Loop crossfade"), |z| {
				z.loop_crossfade = 0.0
			});
			state.apply_sample_edit(cmd);
		}
		let _ = resp.on_hover_text(tr(
			"Crossfade: drag the ramp's foot sideways (blends the loop end into its start), \
			 double-click removes it",
		));
	}
	if !ui.input(|i| i.pointer.any_down()) {
		v.flag = None;
		v.xfade = None;
	}
	// Tags: the loop on top, start / end at the bottom.
	if let Some(text) = se::loop_label(&shown, length) {
		let g = p.layout_no_wrap(text, theme::mono(10.5), theme::ACCENT_TEXT);
		let size = g.size() + vec2(10.0, 2.0);
		let left = (lsx + 4.0).clamp(
			area.left() + 2.0,
			(area.right() - size.x - 2.0).max(area.left() + 2.0),
		);
		let r = Rect::from_min_size(pos2(left, area.top() + 4.0), size);
		p.rect_filled(r, CornerRadius::same(2), theme::ACCENT_DIM);
		p.galley(r.min + vec2(5.0, 1.0), g, theme::ACCENT_TEXT);
	}
	let tag = |x: f32, right: bool, text: String, fg: Color32, bg: Color32| {
		let g = p.layout_no_wrap(text, theme::mono(10.5), fg);
		let size = g.size() + vec2(10.0, 2.0);
		let left = if right { x - 4.0 - size.x } else { x + 4.0 };
		let left = left.clamp(area.left() + 2.0, area.right() - size.x - 2.0);
		let r = Rect::from_min_size(pos2(left, area.bottom() - 6.0 - size.y), size);
		p.rect_filled(r, CornerRadius::same(2), bg);
		p.galley(r.min + vec2(5.0, 1.0), g, fg);
		r
	};
	let s_tag = tag(
		sx,
		false,
		trf("Start {ms_label}", &[("ms_label", &ms_label(start))]),
		theme::PLAYING,
		theme::PLAYING_SOFT,
	);
	let e_text = trf("End {ms_label}", &[("ms_label", &ms_label(end))]);
	let e_w = p
		.layout_no_wrap(e_text.clone(), theme::mono(10.5), END_FG)
		.size()
		.x + 10.0;
	if ex - 4.0 - e_w > s_tag.right() + 6.0 {
		tag(ex, true, e_text, END_FG, END_BG);
	}
	if shown.reverse {
		p.text(
			area.right_top() + vec2(-6.0, 4.0),
			Align2::RIGHT_TOP,
			tr("\u{25c0} reversed"),
			theme::small(),
			theme::TEXT_DIM,
		);
	} else if !window.is_full(length) {
		p.text(
			area.right_top() + vec2(-6.0, 4.0),
			Align2::RIGHT_TOP,
			format!("{}\u{2013}{}", ms_label(window.from), ms_label(window.to)),
			theme::mono(10.0),
			theme::TEXT_FAINT,
		);
	}
	let _ = bg.on_hover_text_at_pointer(tr(
		"Wheel zooms, Shift+wheel scrolls; drag the start / end and loop flags and the \
		 crossfade foot",
	));
}

/// Root key readout + tune / gain / pan / crossfade knobs in the zone colour.
#[allow(clippy::too_many_arguments)]
fn zone_knobs(
	ui: &mut Ui,
	state: &mut AppState,
	cx: &BodyCx,
	index: usize,
	zone: &SampleZone,
	rect: Rect,
	c: Color32,
) {
	let cell_w = rect.width() / 5.0;
	let root_cell = Rect::from_min_size(rect.min, vec2(cell_w, rect.height()));
	root_readout(ui, state, cx, index, zone, root_cell);
	let knob_rect = Rect::from_min_max(pos2(root_cell.right(), rect.top()), rect.max);
	zone_knob_grid(ui, state, cx, index, zone, knob_rect, 4, c, KnobSize::S);
}

fn root_readout(
	ui: &mut Ui,
	state: &mut AppState,
	cx: &BodyCx,
	index: usize,
	zone: &SampleZone,
	cell: Rect,
) {
	let names: Vec<&str> = note_names().iter().map(String::as_str).collect();
	let field = Rect::from_center_size(
		pos2(cell.center().x, cell.top() + 4.0 + 20.0),
		vec2(56.0, 24.0),
	);
	let mut child = ui.new_child(UiBuilder::new().max_rect(field));
	let (typed, resp) = device_kit::readout(
		&mut child,
		Id::new(("smp-root", cx.salt, cx.r.device, index)),
		f32::from(zone.root_key),
		0.0,
		127.0,
		ValueFmt::choices(&names),
		ReadoutLook::Boxed,
		field.size(),
		ReadoutState::Rest,
	);
	let _ = resp.on_hover_text(tr(
		"The key that plays the sample at its recorded pitch: click and type a note (F#3) \
		 or a MIDI number, or drag the zone's dot",
	));
	if let Some(t) = typed {
		let root = t.round().clamp(0.0, 127.0) as u8;
		let cmd = se::edit(state.project(), cx.r, index, tr("Zone root key"), |z| {
			z.root_key = root
		});
		state.apply_sample_edit(cmd);
	}
	ui.painter().text(
		pos2(cell.center().x, field.bottom() + 12.0),
		Align2::CENTER_CENTER,
		tr("Root key"),
		theme::medium(11.0),
		theme::TEXT_DIM,
	);
}

/// The zone's knobs in `cols` columns: tune, gain, pan (zone colour), crossfade (blue,
/// forward loops only).
#[allow(clippy::too_many_arguments)]
fn zone_knob_grid(
	ui: &mut Ui,
	state: &mut AppState,
	cx: &BodyCx,
	index: usize,
	zone: &SampleZone,
	rect: Rect,
	cols: usize,
	c: Color32,
	size: KnobSize,
) {
	struct K {
		name: &'static str,
		value: f32,
		spec: KnobSpec,
		unit: &'static str,
		color: Color32,
		enabled: bool,
		apply: fn(&mut SampleZone, f32),
	}
	let forward = zone.loop_mode == LoopMode::Forward && !zone.reverse;
	let knobs = [
		K {
			name: mark("Tune"),
			value: zone.tune,
			spec: KnobSpec::new(-24.0, 24.0, 0.0),
			unit: "semitones",
			color: c,
			enabled: true,
			apply: |z, v| z.tune = (v * 100.0).round() / 100.0,
		},
		K {
			name: mark("Gain"),
			value: zone.gain_db,
			spec: KnobSpec::new(-60.0, 24.0, 0.0),
			unit: "dB",
			color: c,
			enabled: true,
			apply: |z, v| z.gain_db = (v * 10.0).round() / 10.0,
		},
		K {
			name: mark("Pan"),
			value: zone.pan * 100.0,
			spec: KnobSpec::new(-100.0, 100.0, 0.0),
			unit: "pan%",
			color: c,
			enabled: true,
			apply: |z, v| z.pan = (v.round() / 100.0).clamp(-1.0, 1.0),
		},
		K {
			name: mark("Crossfade"),
			value: (zone.loop_crossfade * 1000.0) as f32,
			spec: KnobSpec::new(0.0, 500.0, 0.0),
			unit: "ms",
			color: theme::ACCENT_TEXT,
			enabled: forward,
			apply: |z, v| z.loop_crossfade = f64::from(v.round()) / 1000.0,
		},
	];
	let cols = cols.clamp(1, knobs.len());
	let rows = knobs.len().div_ceil(cols);
	let cell = vec2(rect.width() / cols as f32, rect.height() / rows as f32);
	for (i, k) in knobs.iter().enumerate() {
		let at = rect.min + vec2((i % cols) as f32 * cell.x, (i / cols) as f32 * cell.y);
		let h = devices::knob_height(size);
		let cell_rect = Rect::from_min_size(at + vec2(0.0, (cell.y - h) * 0.5), vec2(cell.x, h));
		let mut child = ui.new_child(
			UiBuilder::new()
				.max_rect(cell_rect)
				.layout(Layout::top_down(Align::Center)),
		);
		let mut knob = Knob::new(
			Id::new(("smp-knob", cx.salt, cx.r.device, index, k.name)),
			k.value,
			k.spec,
		)
		.size(size)
		.label(k.name)
		.unit(k.unit)
		.color(k.color)
		.enabled(k.enabled);
		if k.name == "Gain" {
			knob = knob.wheel_step(Some(0.5));
		}
		let (g, resp) = knob.show(&mut child);
		if !k.enabled {
			let _ = resp.on_hover_text(tr("The crossfade blends forward loops; pick Forward"));
		}
		if let Gesture::Commit(value) = g {
			let apply = k.apply;
			let label = trf("Zone {name}", &[("name", &tr_str(k.name).to_lowercase())]);
			let cmd = se::edit(state.project(), cx.r, index, &label, |z| apply(z, value));
			state.apply_sample_edit(cmd);
		}
	}
}

// --- Globals --------------------------------------------------------------------------

fn globals_section(
	ui: &mut Ui,
	state: &mut AppState,
	cx: &BodyCx,
	v: &mut SamplerView,
	rect: Rect,
) {
	title(ui, rect, tr("GLOBAL \u{b7} ALL ZONES"));
	let body = Rect::from_min_max(pos2(rect.left(), rect.top() + TITLE_H + 2.0), rect.max);
	let knob_h = devices::knob_height(KnobSize::S);
	let lower_h = if v.filter_env {
		(body.height() * 0.46).max(knob_h)
	} else {
		knob_h
	};
	let upper = body.with_max_y(body.bottom() - lower_h - 6.0);
	let lower = Rect::from_min_max(pos2(body.left(), upper.bottom() + 6.0), body.max);
	let col = (upper.width() - 8.0) * 0.5;
	let amp = upper.with_max_x(upper.left() + col);
	let filter = upper.with_min_x(amp.right() + 8.0);
	envelope_block(ui, state, cx, Env::Amp, tr("Amp envelope"), amp);
	filter_block(ui, state, cx, v, filter);
	if v.filter_env {
		let knobs_w = 120.0_f32.min(lower.width() * 0.4);
		let env = lower.with_max_x(lower.right() - knobs_w - 8.0);
		envelope_block(ui, state, cx, Env::Filter, tr("Filter envelope"), env);
		let knobs = Rect::from_min_max(pos2(env.right() + 8.0, lower.top()), lower.max);
		devices::knob_grid(
			ui,
			state,
			cx,
			knobs,
			&[
				KnobDef::new(ENV_AMOUNT, tr("Env amt"), KnobSize::S).color(VIOLET_LIGHT),
				KnobDef::new(FILTER_VELOCITY, tr("Flt vel"), KnobSize::S).color(VIOLET_LIGHT),
			],
			if knobs.height() >= 2.0 * knob_h { 1 } else { 2 },
		);
	} else {
		devices::knob_grid(
			ui,
			state,
			cx,
			lower,
			&[
				KnobDef::new(CUTOFF, tr("Cutoff"), KnobSize::S).color(VIOLET_LIGHT),
				KnobDef::new(RESONANCE, tr("Reso"), KnobSize::S).color(VIOLET_LIGHT),
				KnobDef::new(ENV_AMOUNT, tr("Env amt"), KnobSize::S).color(VIOLET_LIGHT),
				KnobDef::new(FILTER_VELOCITY, tr("Flt vel"), KnobSize::S).color(VIOLET_LIGHT),
			],
			4,
		);
	}
}

/// "Amp envelope" over the kit ADSR (previews while dragging, one step per node gesture)
/// and its one-line summary.
fn envelope_block(
	ui: &mut Ui,
	state: &mut AppState,
	cx: &BodyCx,
	env: Env,
	label: &str,
	rect: Rect,
) {
	let p = ui.painter();
	p.text(
		rect.left_top() + vec2(0.0, 1.0),
		Align2::LEFT_TOP,
		label,
		theme::medium(10.5),
		theme::TEXT_DIM,
	);
	let display = Rect::from_min_max(
		pos2(rect.left(), rect.top() + 18.0),
		pos2(rect.right(), rect.bottom() - 17.0),
	);
	let r = cx.r;
	let ranges = env.ranges();
	let values = env.values(|id| value(state, cx, id));
	let color = match env {
		Env::Amp => AMP_GREEN,
		Env::Filter => VIOLET,
	};
	let mut child = ui.new_child(UiBuilder::new().max_rect(display));
	let out = device_kit::adsr(
		&mut child,
		Id::new(("smp-env", cx.salt, r.device, env == Env::Amp)),
		values,
		&ranges,
		color,
		display.size(),
		&state.live_voices(r.device).envelope_dots(),
	);
	let mut commits: Vec<(&'static str, f32)> = Vec::new();
	let mut stages = Vec::new();
	let mut shown: AdsrValues = values;
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
	summary(ui, rect, &se::env_summary(&shown));
}

fn finish_wheel(ui: &Ui, state: &mut AppState, cx: &BodyCx, v: &mut SamplerView) {
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

fn wheel_env(ui: &Ui, state: &mut AppState, cx: &BodyCx, v: &mut SamplerView, notches: f32) {
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
	w.value = se::wheel_env(w.start, w.notches);
	state.preview_param(cx.r.device, ENV_AMOUNT, w.value);
	ui.ctx()
		.request_repaint_after(std::time::Duration::from_secs_f64(WHEEL_REST));
}

/// "Filter [Off | LP | HP | BP]" over the exact response with its node and the summary.
fn filter_block(ui: &mut Ui, state: &mut AppState, cx: &BodyCx, v: &mut SamplerView, rect: Rect) {
	let r = cx.r;
	ui.painter().text(
		rect.left_top() + vec2(0.0, 1.0),
		Align2::LEFT_TOP,
		tr("Filter"),
		theme::medium(10.5),
		theme::TEXT_DIM,
	);
	let kind = se::filter_type(value(state, cx, FILTER_TYPE));
	let seg = Rect::from_min_max(
		pos2(rect.left() + 40.0, rect.top() - 4.0),
		pos2(rect.right(), rect.top() + 16.0),
	);
	let mut child = ui.new_child(
		UiBuilder::new()
			.max_rect(seg)
			.layout(Layout::right_to_left(Align::Center)),
	);
	child.spacing_mut().interact_size.y = 18.0;
	if let Some(i) = device_kit::segmented(
		&mut child,
		Id::new(("smp-filter-type", cx.salt, r.device)),
		&FILTER_TYPES,
		kind,
	) && i != kind
	{
		state.commit_param(r.track, r.device, FILTER_TYPE, i as f32);
	}
	let display = Rect::from_min_max(
		pos2(rect.left(), rect.top() + 22.0),
		pos2(rect.right(), rect.bottom() - 17.0),
	);
	let cutoff = value(state, cx, CUTOFF);
	let res = value(state, cx, RESONANCE);
	let env = value(state, cx, ENV_AMOUNT);
	let response = |f: f32| se::response_db(kind, cutoff, res, f);
	let node_db = response(cutoff);
	let ai = [CUTOFF, RESONANCE, ENV_AMOUNT]
		.iter()
		.any(|p| state.ai_change(r.device, p).is_some());
	let nodes = [FreqNode {
		label: "F",
		freq: cutoff,
		db: node_db,
		color: VIOLET,
		dormant: kind == 0,
		x_only: false,
		ai,
		dynamic: None,
	}];
	let mut child = ui.new_child(UiBuilder::new().max_rect(display));
	let out = FreqDisplay::new(Id::new(("smp-filter", cx.salt, r.device)), display.size())
		.range(DbRange::FILTER)
		.sum(
			&response,
			if kind == 0 {
				theme::TEXT_FAINT
			} else {
				VIOLET_LIGHT
			},
		)
		.sum_fill(if kind == 0 { 0.0 } else { 0.12 })
		.nodes(&nodes)
		.small_nodes()
		.show(&mut child);
	for e in out.events {
		match e {
			FreqEvent::Move {
				freq, db, commit, ..
			} => {
				let start = *v.node.get_or_insert(NodeStart {
					resonance: res,
					db: node_db,
				});
				let changes = se::node_drag(start, freq, db);
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
				devices::commit_changes(state, r, cx.info, &se::reset_node(), |_| {
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
	out.response.on_hover_text_at_pointer(tr(
		"Drag the node: \u{21d4} cutoff, \u{21d5} resonance (Shift fine) \u{b7} wheel over it: \
		 env amount \u{b7} double-click resets",
	));
	summary(ui, rect, &se::filter_summary(kind, cutoff, res, env));
}

/// The one-line values under a globals display.
fn summary(ui: &Ui, rect: Rect, text: &str) {
	let font = theme::medium(11.0);
	let text = crate::ui::mixer::elide(ui, text, &font, rect.width());
	ui.painter().text(
		pos2(rect.left(), rect.bottom() - 7.0),
		Align2::LEFT_CENTER,
		text,
		font,
		theme::TEXT_DIM,
	);
}

// --- Bottom row -----------------------------------------------------------------------

/// Trigger, polyphony, transpose, glide, legato, velocity + curve, key track and the
/// filter envelope fold; wraps when narrow.
fn bottom_row(ui: &mut Ui, state: &mut AppState, cx: &BodyCx, v: &mut SamplerView, rect: Rect) {
	let mut child = ui.new_child(
		UiBuilder::new()
			.max_rect(rect)
			.layout(Layout::left_to_right(Align::Center).with_main_wrap(true)),
	);
	let ui = &mut child;
	ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
	let label = |ui: &mut Ui, text: &str| {
		ui.label(
			egui::RichText::new(text)
				.font(theme::medium(12.0))
				.color(theme::TEXT_DIM),
		);
	};
	let gap = |ui: &mut Ui| ui.add_space(10.0);
	label(ui, tr("Trigger"));
	let mode = (value(state, cx, MODE) >= 0.5) as usize;
	if let Some(i) = device_kit::segmented(
		ui,
		Id::new(("smp-trigger", cx.salt, cx.r.device)),
		&[tr("Gate"), tr("One-shot")],
		mode,
	) && i != mode
	{
		state.commit_param(cx.r.track, cx.r.device, MODE, i as f32);
	}
	gap(ui);
	for (id, name, w) in [
		(POLYPHONY, tr("Polyphony"), 34.0),
		(TRANSPOSE, tr("Transpose"), 44.0),
		(GLIDE, tr("Glide"), 56.0),
	] {
		ui.horizontal(|ui| {
			ui.spacing_mut().item_spacing.x = 4.0;
			label(ui, name);
			row_readout(ui, state, cx, v, id, w);
		});
		gap(ui);
	}
	ui.horizontal(|ui| {
		ui.spacing_mut().item_spacing.x = 4.0;
		label(ui, tr("Legato"));
		let on = value(state, cx, LEGATO) >= 0.5;
		if device_kit::toggle(ui, on, if on { "on" } else { "off" }, theme::ACCENT)
			.on_hover_text(tr(
				"Monophonic legato: overlapping notes glide without restarting",
			))
			.clicked()
		{
			state.commit_param(cx.r.track, cx.r.device, LEGATO, if on { 0.0 } else { 1.0 });
		}
	});
	gap(ui);
	ui.horizontal(|ui| {
		ui.spacing_mut().item_spacing.x = 4.0;
		label(ui, tr("Velocity"));
		row_readout(ui, state, cx, v, VELOCITY, 46.0);
		label(ui, tr("\u{b7} curve"));
		curve_menu(ui, state, cx);
	});
	gap(ui);
	ui.horizontal(|ui| {
		ui.spacing_mut().item_spacing.x = 4.0;
		label(ui, tr("Key track"));
		row_readout(ui, state, cx, v, KEY_TRACK, 46.0);
	});
	let text = if v.filter_env {
		tr("\u{25be} Filter envelope")
	} else {
		tr("\u{25b8} Filter envelope")
	};
	let bw = 128.0;
	let cur = ui.cursor();
	let r = if rect.right() - cur.left() >= bw + 24.0 {
		Rect::from_min_size(pos2(rect.right() - bw, cur.top() + 1.0), vec2(bw, 22.0))
	} else {
		gap(ui);
		ui.allocate_exact_size(vec2(bw, 22.0), Sense::hover()).0
	};
	if chip_button(
		ui,
		Id::new(("smp-filter-env", cx.salt, cx.r.device)),
		r,
		text,
		v.filter_env,
	)
	.on_hover_text(tr(
		"Show the filter envelope (attack, decay, sustain, release) in the globals",
	))
	.clicked()
	{
		v.filter_env = !v.filter_env;
	}
}

/// A typed value in the bottom row: click to type, drag ⇕ to change, right-click for
/// Automate / Reset.
fn row_readout(
	ui: &mut Ui,
	state: &mut AppState,
	cx: &BodyCx,
	v: &mut SamplerView,
	id: &'static str,
	width: f32,
) {
	let Some(info) = cx.info.param(id) else {
		return;
	};
	let r = cx.r;
	let current = value(state, cx, id);
	let base = Id::new(("smp-readout", cx.salt, r.device, id));
	let shown = if v.readout.is_some_and(|(p, _, _)| p == id) {
		ReadoutState::Active
	} else if state.ai_change(r.device, id).is_some() {
		ReadoutState::Ai
	} else {
		ReadoutState::Rest
	};
	let (typed, resp) = device_kit::readout(
		ui,
		base,
		current,
		info.min,
		info.max,
		ValueFmt::unit(info.unit),
		ReadoutLook::Plain,
		vec2(width, 22.0),
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
			tip: Some(info.description),
		},
	);
	if let Some(m) = menu {
		state.commit_param(r.track, r.device, id, m);
	}
	let drag = ui
		.interact(resp.rect, base.with("drag"), Sense::drag())
		.on_hover_cursor(CursorIcon::ResizeVertical);
	if drag.drag_started() {
		v.readout = Some((id, current, 0.0));
	}
	if drag.dragged()
		&& let Some((_, start, dy)) = v.readout.as_mut().filter(|(p, _, _)| *p == id)
	{
		*dy += drag.drag_delta().y;
		state.preview_param(r.device, id, se::drag_readout(id, *start, *dy));
	}
	if drag.drag_stopped()
		&& let Some((_, start, dy)) = v.readout.take().filter(|(p, _, _)| *p == id)
	{
		let name = info.name;
		devices::commit_changes(
			state,
			r,
			cx.info,
			&[(id, se::drag_readout(id, start, dy))],
			|_| name.to_owned(),
		);
	}
}

/// "soft ▾": the velocity curve presets (soft notes louder / linear / harder).
fn curve_menu(ui: &mut Ui, state: &mut AppState, cx: &BodyCx) {
	let current = value(state, cx, VEL_CURVE);
	let text = format!("{} \u{25be}", se::curve_word(current));
	let g = ui
		.painter()
		.layout_no_wrap(text.clone(), theme::medium(12.0), theme::TEXT);
	let (rect, _) = ui.allocate_exact_size(vec2(g.size().x + 12.0, 22.0), Sense::hover());
	let resp = ui
		.interact(
			rect,
			Id::new(("smp-curve", cx.salt, cx.r.device)),
			Sense::click(),
		)
		.on_hover_cursor(CursorIcon::PointingHand)
		.on_hover_text(trf(
			"Velocity curve {current} %: soft = quiet notes louder, hard = more dynamic",
			&[("current", &crate::i18n::decimal(format!("{:.0}", current)))],
		));
	resp.widget_info(|| {
		egui::WidgetInfo::labeled(egui::WidgetType::ComboBox, true, tr("Velocity curve"))
	});
	if resp.hovered() {
		ui.painter()
			.rect_filled(rect, CornerRadius::same(theme::RADIUS), theme::BG_HOVER);
	}
	ui.painter().galley(
		pos2(rect.left() + 6.0, rect.center().y - g.size().y * 0.5),
		g,
		theme::TEXT,
	);
	egui::Popup::menu(&resp).show(|ui| {
		ui.set_min_width(150.0);
		for (word, v) in CURVES {
			let label = format!("{word} ({})", crate::state::device::format_value(v, "%"));
			if ui.radio(se::curve_word(current) == word, label).clicked() {
				state.commit_param(cx.r.track, cx.r.device, VEL_CURVE, v);
				ui.close();
			}
		}
	});
}

// --- Compact (inspector) --------------------------------------------------------------

const COMPACT_MAP: f32 = 118.0;
const COMPACT_TITLE: f32 = 24.0;
const COMPACT_LOOP: f32 = 30.0;

/// How tall the compact form is at `width`.
pub fn compact_height(width: f32) -> f32 {
	COMPACT_MAP + GAP + COMPACT_TITLE + COMPACT_LOOP + devices::small_rows_height(width, 4) + 4.0
}

#[allow(clippy::too_many_arguments)]
fn compact(
	ui: &mut Ui,
	state: &mut AppState,
	cx: &BodyCx,
	v: &mut SamplerView,
	hits: &mut ZoneHits,
	d: &Device,
	rect: Rect,
) {
	let map = Rect::from_min_size(rect.min, vec2(rect.width(), COMPACT_MAP));
	zone_map(ui, state, cx, v, hits, d, map, true);
	let Some(index) = d
		.zones
		.len()
		.checked_sub(1)
		.map(|n| state.view.instrument.zone.min(n))
	else {
		return;
	};
	let zone = d.zones[index].clone();
	let c = zone_color(se::zone_colors(&d.zones)[index]);
	let y = map.bottom() + GAP;
	let p = ui.painter();
	p.rect_filled(
		Rect::from_center_size(
			pos2(rect.left() + 5.0, y + COMPACT_TITLE * 0.5),
			vec2(8.0, 8.0),
		),
		2.0,
		c,
	);
	let range = p.text(
		pos2(rect.right(), y + COMPACT_TITLE * 0.5),
		Align2::RIGHT_CENTER,
		se::zone_title(&zone),
		theme::mono(10.5),
		theme::TEXT_FAINT,
	);
	let name = crate::ui::mixer::elide(
		ui,
		&zone.sample.display_name(),
		&theme::semibold(12.0),
		(range.left() - rect.left() - 26.0).max(0.0),
	);
	ui.painter().text(
		pos2(rect.left() + 16.0, y + COMPACT_TITLE * 0.5),
		Align2::LEFT_CENTER,
		name,
		theme::semibold(12.0),
		theme::TEXT,
	);
	let loop_row = Rect::from_min_size(
		pos2(rect.left(), y + COMPACT_TITLE),
		vec2(rect.width(), COMPACT_LOOP),
	);
	let mut child = ui.new_child(
		UiBuilder::new()
			.max_rect(loop_row)
			.layout(Layout::left_to_right(Align::Center)),
	);
	let modes = [LoopMode::Off, LoopMode::Forward, LoopMode::PingPong];
	let current = modes.iter().position(|m| *m == zone.loop_mode).unwrap_or(0);
	if let Some(i) = device_kit::segmented(
		&mut child,
		Id::new(("smp-loop", cx.salt, cx.r.device)),
		&[tr("Off"), tr("Forward"), tr("Ping-pong")],
		current,
	) && i != current
	{
		let mode = modes[i];
		let cmd = se::edit(state.project(), cx.r, index, tr("Zone loop mode"), |z| {
			z.loop_mode = mode
		});
		state.apply_sample_edit(cmd);
	}
	let knobs = Rect::from_min_max(pos2(rect.left(), loop_row.bottom() + 4.0), rect.max);
	let cols = devices::small_columns(rect.width(), 4);
	zone_knob_grid(ui, state, cx, index, &zone, knobs, cols, c, KnobSize::S);
}
