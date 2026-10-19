//! The Drum Sampler editor body (design `17 Instruments`, 17d): the KIT · ALL PADS
//! column (gain, velocity, polyphony, kit tune; a folder dropped here or picked with
//! "Map folder…" maps a whole kit onto the pads by name), the 4x4 pads (pad 1
//! bottom-left; name, note, a mini waveform in the pad colour, tags for choke group,
//! extra output and layers / round-robin; click = audition + select, higher = harder;
//! drag onto another pad = swap, Alt-drag = copy; right-click = choke group, output,
//! reverse, clear, locate file; double-click an empty pad = load a sample; a file dragged
//! over a pad shows a blue dashed "Drop to replace") and the selected pad: its file menu,
//! velocity layer chips, round-robin, reverse, the waveform with start / end flags (drag
//! ⇔, double-click resets to the sample bounds; wheel zooms, Shift-wheel scrolls) under
//! the A/D envelope the processor plays (drag the nodes), tune / gain / pan / attack /
//! decay knobs and the choke group / output pickers. Pads flash when the engine triggers
//! them (the processor's trigger counters, read through the probe); keys 1–8 (Shift:
//! 9–16) audition while the editor has the focus. Below 1080 px the pads go above the
//! selected pad, below 700 px the compact form takes over (also the inspector's). Every
//! gesture is one `SetSampleZones` undo step; the pure logic lives in
//! [`crate::state::drum_sampler_editor`] and [`crate::state::samples`].

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use cadenza_core::TrackId;
use cadenza_dsp::instruments::drum_sampler::PADS;
use cadenza_model::{Device, SampleZone};
use egui::{
	Align2, Color32, CornerRadius, CursorIcon, Id, Key, Layout, Pos2, Rect, Sense, Shape, Stroke,
	StrokeKind, Ui, UiBuilder, pos2, vec2,
};

use crate::jobs::Peaks;
use crate::state::AppState;
use crate::state::device::KnobSpec;
use crate::state::dnd::Payload;
use crate::state::drum_sampler_editor::{
	self as ds, OUTPUTS, PadFlashes, WaveWindow, envelope, ms_label, pad_layers, pad_tags,
};
use crate::state::samples::{self as sm, Marker};
use crate::state::selection::DeviceRef;
use crate::theme;
use crate::ui::device_editor::BodyCx;
use crate::ui::device_kit::{self, Knob, KnobSize};
use crate::ui::devices::{self, FOOTER_H, KnobDef};
use crate::ui::widgets::Gesture;

const GAP: f32 = 8.0;
const PAD_GAP: f32 = 4.0;
/// One row from this width on; narrower docks put the pads above the selected pad.
const ROW_MIN: f32 = 1080.0;
/// Below this width the dock shows the compact form.
const COMPACT_BELOW: f32 = 700.0;
const KIT_W: f32 = 196.0;
const KIT_W_NARROW: f32 = 164.0;
const HEAD_H: f32 = 24.0;
const WHEEL_ZOOM: f64 = 0.85;

const KIT_BG: Color32 = Color32::from_rgb(0x11, 0x12, 0x15);
const KIT_RING: Color32 = Color32::from_rgb(0x1d, 0x1f, 0x23);
const PAD_BG: Color32 = Color32::from_rgb(0x1c, 0x1d, 0x21);
const PAD_BG_SELECTED: Color32 = Color32::from_rgb(0x26, 0x28, 0x2d);
const PAD_BG_EMPTY: Color32 = Color32::from_rgb(0x11, 0x12, 0x15);
const PAD_RING: Color32 = Color32::from_rgb(0x24, 0x26, 0x2b);
const EMPTY_DOT: Color32 = Color32::from_rgb(0x36, 0x39, 0x40);
const NAME: Color32 = Color32::from_rgb(0xf4, 0xf5, 0xf7);
const DROP_BG: Color32 = Color32::from_rgb(0x16, 0x24, 0x3a);
const DROP_TEXT: Color32 = Color32::from_rgb(0x7f, 0xb4, 0xff);
const CHOKE_BG: Color32 = Color32::from_rgb(0x3a, 0x2c, 0x17);
const CHOKE_FG: Color32 = Color32::from_rgb(0xf2, 0xa9, 0x44);
const OUT_BG: Color32 = Color32::from_rgb(0x1d, 0x2a, 0x3d);
const LAYER_BG: Color32 = Color32::from_rgb(0x24, 0x26, 0x2b);
const LAYER_FG: Color32 = Color32::from_rgb(0xc4, 0xc7, 0xcd);
const FIELD_BG: Color32 = Color32::from_rgb(0x0e, 0x0f, 0x11);
const SHADE: Color32 = Color32::from_rgba_premultiplied(0x08, 0x08, 0x0a, 0xb8);
const END_FG: Color32 = Color32::from_rgb(0xff, 0x8a, 0x87);
const END_BG: Color32 = Color32::from_rgb(0x3a, 0x1d, 0x1f);
const ENVELOPE: Color32 = Color32::from_rgb(0xf4, 0xf5, 0xf7);
const FOOTER: &str = "Pad tags: C1 choke group \u{b7} \u{2192}3 extra output \u{b7} L3 RR3 layers and \
	round-robin. Blue dashed pad = file being dragged over it. Keys 1\u{2013}8 (Shift: 9\u{2013}16) \
	play the pads.";

fn pad_color(note: u8) -> Color32 {
	theme::BAND_COLORS[ds::pad_color(note)]
}

// --- Hit areas (drops, tests) and held keys ----------------------------------------

/// What the dock's drum sampler body leaves for the drop plumbing and the tests.
#[derive(Default)]
pub struct PadHits {
	/// Pads of the last frame.
	pub pads: Vec<(Rect, u8)>,
	/// Where a folder drop maps a kit (kit column and pads).
	pub kit: Option<Rect>,
	/// The selected pad's waveform.
	pub wave: Option<Rect>,
	/// The 4x4 grid.
	pub grid: Option<Rect>,
	/// Pads sounding from computer keys (released on key up).
	keys: Vec<(Key, TrackId, u8)>,
}

impl PadHits {
	fn clear(&mut self) {
		self.pads.clear();
		self.kit = None;
		self.wave = None;
		self.grid = None;
	}

	/// Releases the pads still held by computer keys.
	pub fn release(&mut self, state: &mut AppState) {
		for (_, track, note) in self.keys.drain(..) {
			state.preview_note_off(track, note);
		}
	}
}

// --- View state (egui memory) --------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Node {
	Attack,
	Decay,
}

#[derive(Default)]
struct SamplerView {
	flashes: PadFlashes,
	/// A flash held at this level (screenshots).
	pinned: Option<(usize, f32)>,
	/// The note a pad press sounds until the button goes up.
	held: Option<(TrackId, u8)>,
	/// A pad being dragged onto another.
	drag: Option<u8>,
	/// A flag being dragged and where (sample seconds).
	flag: Option<(Marker, f64)>,
	/// An envelope node being dragged and its value (ms; decay `None` = full).
	node: Option<(Node, Option<f32>)>,
	/// The waveform's zoom, per sample file.
	window: Option<(PathBuf, WaveWindow)>,
}

type Shared = Arc<Mutex<SamplerView>>;

fn view(ctx: &egui::Context, salt: &str, r: DeviceRef) -> Shared {
	let id = Id::new(("drum-sampler-editor", salt, r.device));
	ctx.data_mut(|d| {
		d.get_temp_mut_or_insert_with(id, || Arc::new(Mutex::new(SamplerView::default())))
			.clone()
	})
}

fn lock(view: &Shared) -> std::sync::MutexGuard<'_, SamplerView> {
	view.lock().unwrap_or_else(|e| e.into_inner())
}

/// Holds pad `pad` (index into [`PADS`]) lit at `level` as if the engine had just
/// triggered it (screenshots); `None` lets the counters drive the flashes again.
pub fn set_flash(ctx: &egui::Context, salt: &str, r: DeviceRef, flash: Option<(usize, f32)>) {
	lock(&view(ctx, salt, r)).pinned = flash;
}

/// Keys 1–8 audition pads 1–8, Shift + 1–8 pads 9–16, while the editor has the focus.
pub fn shortcuts(ctx: &egui::Context, state: &mut AppState, r: DeviceRef, hits: &mut PadHits) {
	const DIGITS: [Key; 8] = [
		Key::Num1,
		Key::Num2,
		Key::Num3,
		Key::Num4,
		Key::Num5,
		Key::Num6,
		Key::Num7,
		Key::Num8,
	];
	let Some(track) = r.track else {
		return;
	};
	for (k, key) in DIGITS.iter().enumerate() {
		let shift = ctx.input(|i| i.modifiers.shift);
		let pressed = ctx.input_mut(|i| {
			i.consume_key(egui::Modifiers::NONE, *key)
				|| i.consume_key(egui::Modifiers::SHIFT, *key)
		});
		if pressed
			&& !hits.keys.iter().any(|(held, ..)| held == key)
			&& let Some(pad) = ds::key_pad(k as u8 + 1, shift)
		{
			let note = PADS[pad].0;
			state.preview_note_on(track, note, 0.85);
			hits.keys.push((*key, track, note));
			state.view.instrument.pad = note;
			state.view.instrument.layer = 0;
		}
	}
}

/// Pads still held by keys that went up (the key-up may arrive without the focus).
fn release_keys(ctx: &egui::Context, state: &mut AppState, hits: &mut PadHits) {
	hits.keys.retain(|(key, track, note)| {
		let down = ctx.input(|i| i.key_down(*key));
		if !down {
			state.preview_note_off(*track, *note);
		}
		down
	});
}

// --- Body ------------------------------------------------------------------------------

pub fn drum_sampler_body(ui: &mut Ui, state: &mut AppState, cx: &BodyCx, hits: &mut PadHits) {
	hits.clear();
	let Some(d) = state.device(cx.r).cloned() else {
		return;
	};
	let shared = view(ui.ctx(), cx.salt, cx.r);
	let mut v = lock(&shared);
	let now = ui.input(|i| i.time);
	listen(ui, state, cx, &mut v, now);
	release_keys(ui.ctx(), state, hits);
	if ui.input(|i| i.pointer.any_released())
		&& let Some((track, note)) = v.held.take()
	{
		state.preview_note_off(track, note);
	}
	if cx.compact || cx.rect.width() < COMPACT_BELOW {
		let height = compact_height(cx.rect.width());
		devices::compact_scroll(ui, cx, height, |ui, rect| {
			compact(ui, state, cx, &mut v, hits, &d, rect, now);
		});
	} else {
		dock(ui, state, cx, &mut v, hits, &d, now);
	}
}

/// Takes the trigger counters of the probed device and keeps the flashes moving.
fn listen(ui: &Ui, state: &AppState, cx: &BodyCx, v: &mut SamplerView, now: f64) {
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

fn flash(v: &SamplerView, i: usize, now: f64) -> f32 {
	match v.pinned {
		Some((p, level)) if p == i => level,
		_ => v.flashes.level(i, now),
	}
}

#[allow(clippy::too_many_arguments)]
fn dock(
	ui: &mut Ui,
	state: &mut AppState,
	cx: &BodyCx,
	v: &mut SamplerView,
	hits: &mut PadHits,
	d: &Device,
	now: f64,
) {
	let rect = cx.rect;
	let content = Rect::from_min_max(rect.min, pos2(rect.right(), rect.bottom() - FOOTER_H - 4.0));
	devices::footer(ui, rect, FOOTER);
	if rect.width() >= ROW_MIN {
		let kit_w = if rect.width() >= 1400.0 {
			KIT_W
		} else {
			KIT_W_NARROW
		};
		let kit = Rect::from_min_size(content.min, vec2(kit_w, content.height()));
		let grid_w = ((rect.width() - kit_w) * 0.4).clamp(380.0, 548.0);
		let grid = Rect::from_min_size(
			pos2(kit.right() + GAP, content.top()),
			vec2(grid_w, content.height()),
		);
		kit_column(ui, state, cx, kit);
		pads(ui, state, cx, v, hits, d, grid, now, false);
		hits.kit = Some(kit.union(grid));
		let panel = Rect::from_min_max(pos2(grid.right() + GAP + 4.0, content.top()), content.max);
		selected_panel(ui, state, cx, v, hits, d, panel);
		drop_overlay(ui, hits);
	} else {
		let top_h = (content.height() * 0.55).clamp(240.0, 300.0);
		let panel_h = 270.0_f32.max(content.height() - top_h - GAP);
		let mut inner = ui.new_child(
			UiBuilder::new()
				.max_rect(content)
				.layout(Layout::top_down(egui::Align::Min)),
		);
		egui::ScrollArea::vertical()
			.id_salt(("drum-sampler-stack", cx.salt, cx.r.device))
			.auto_shrink([false, false])
			.show(&mut inner, |ui| {
				let w = ui.available_width();
				let (area, _) =
					ui.allocate_exact_size(vec2(w, top_h + GAP + panel_h), Sense::hover());
				let kit = Rect::from_min_size(area.min, vec2(KIT_W_NARROW, top_h));
				let grid = Rect::from_min_max(
					pos2(kit.right() + GAP, area.top()),
					pos2(area.right(), area.top() + top_h),
				);
				kit_column(ui, state, cx, kit);
				pads(ui, state, cx, v, hits, d, grid, now, false);
				hits.kit = Some(kit.union(grid));
				let panel = Rect::from_min_max(pos2(area.left(), grid.bottom() + GAP), area.max);
				selected_panel(ui, state, cx, v, hits, d, panel);
				drop_overlay(ui, hits);
			});
	}
}

// --- Kit column -----------------------------------------------------------------------

fn kit_column(ui: &mut Ui, state: &mut AppState, cx: &BodyCx, rect: Rect) {
	ui.painter().rect(
		rect,
		CornerRadius::same(theme::RADIUS_LG),
		KIT_BG,
		Stroke::new(1.0, KIT_RING),
		StrokeKind::Inside,
	);
	let inner = rect.shrink(10.0);
	ui.painter().text(
		inner.left_top(),
		Align2::LEFT_TOP,
		"KIT \u{b7} ALL PADS",
		theme::semibold(10.5),
		theme::TEXT_FAINT,
	);
	let knob_h = devices::knob_height(KnobSize::S);
	let knobs = Rect::from_min_size(
		pos2(inner.left(), inner.top() + 16.0),
		vec2(inner.width(), 2.0 * knob_h),
	);
	devices::knob_grid(
		ui,
		state,
		cx,
		knobs,
		&[
			KnobDef::new("gain", "Gain", KnobSize::S).step(0.5),
			KnobDef::new("velocity_sensitivity", "Velocity", KnobSize::S),
			KnobDef::new("polyphony", "Polyphony", KnobSize::S),
			KnobDef::new("tune", "Kit tune", KnobSize::S),
		],
		2,
	);
	let button = Rect::from_min_size(
		pos2(inner.left(), inner.bottom() - 20.0),
		vec2(inner.width(), 20.0),
	);
	let hint = Rect::from_min_max(
		pos2(inner.left(), knobs.bottom() + 2.0),
		pos2(inner.right(), button.top() - 5.0),
	);
	if hint.height() > 12.0 {
		let g = ui.painter().layout(
			"Drop a folder here to map a whole kit onto pads by name.".to_owned(),
			theme::medium(10.5),
			theme::TEXT_FAINT,
			hint.width(),
		);
		if g.size().y <= hint.height() {
			ui.painter().galley(
				pos2(hint.left(), hint.bottom() - g.size().y),
				g,
				theme::TEXT_FAINT,
			);
		}
	}
	if button.top() > knobs.bottom() {
		let id = Id::new(("kit-folder", cx.salt, cx.r.device));
		let resp = chip_button(ui, id, button, "Map folder\u{2026}", false).on_hover_text(
			"Pick (or drop here) a folder of drum samples: each file goes to the pad its name \
			 names (kick, snare, hat, tom...), one undo step",
		);
		if resp.clicked() {
			state.pick_kit_folder(cx.r);
		}
	}
}

/// A small raised button drawn in `rect`.
pub(crate) fn chip_button(
	ui: &mut Ui,
	id: Id,
	rect: Rect,
	label: &str,
	on: bool,
) -> egui::Response {
	let resp = ui
		.interact(rect, id, Sense::click())
		.on_hover_cursor(CursorIcon::PointingHand);
	resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label));
	let p = ui.painter();
	p.rect(
		rect,
		CornerRadius::same(theme::RADIUS_MD),
		if on {
			theme::BG_PRESSED
		} else if resp.hovered() {
			theme::BG_HOVER
		} else {
			theme::BG_WIDGET
		},
		Stroke::new(
			1.0,
			if on {
				theme::TEXT_FAINT
			} else {
				Color32::TRANSPARENT
			},
		),
		StrokeKind::Inside,
	);
	p.text(
		rect.center(),
		Align2::CENTER_CENTER,
		label,
		theme::medium(11.5),
		if on { theme::TEXT } else { theme::TEXT_DIM },
	);
	resp
}

/// A dashed outline (drop targets).
pub(crate) fn dashed_rect(p: &egui::Painter, rect: Rect, stroke: Stroke) {
	let pts = [
		rect.left_top(),
		rect.right_top(),
		rect.right_bottom(),
		rect.left_bottom(),
		rect.left_top(),
	];
	p.add(Shape::dashed_line(&pts, stroke, 5.0, 3.0));
}

// --- Pads ---------------------------------------------------------------------------

/// The file being dragged (in the app or from the OS) and whether it is a folder.
pub(crate) fn dragged_file(ctx: &egui::Context) -> Option<PathBuf> {
	let in_app = egui::DragAndDrop::payload::<Payload>(ctx).and_then(|p| match &*p {
		Payload::File(path) => Some(path.clone()),
		_ => None,
	});
	in_app.or_else(|| crate::ui::dnd::hovered_file(ctx))
}

#[allow(clippy::too_many_arguments)]
fn pads(
	ui: &mut Ui,
	state: &mut AppState,
	cx: &BodyCx,
	v: &mut SamplerView,
	hits: &mut PadHits,
	d: &Device,
	grid: Rect,
	now: f64,
	mini: bool,
) {
	hits.grid = Some(grid);
	let w = (grid.width() - 3.0 * PAD_GAP) / 4.0;
	let h = (grid.height() - 3.0 * PAD_GAP) / 4.0;
	let file = dragged_file(ui.ctx()).filter(|p| !p.is_dir());
	let pointer = ui.ctx().pointer_latest_pos();
	for (i, &(note, _)) in PADS.iter().enumerate() {
		let (row, col) = ds::grid_cell(i);
		let rect = Rect::from_min_size(
			pos2(
				grid.left() + col as f32 * (w + PAD_GAP),
				grid.top() + row as f32 * (h + PAD_GAP),
			),
			vec2(w, h),
		);
		hits.pads.push((rect, note));
		let drop = file.is_some() && pointer.is_some_and(|p| rect.contains(p));
		let lit = flash(v, i, now);
		pad(ui, state, cx, v, d, note, rect, lit, drop, mini);
	}
	pad_drag(ui, state, cx, v, hits);
}

#[allow(clippy::too_many_arguments)]
fn pad(
	ui: &mut Ui,
	state: &mut AppState,
	cx: &BodyCx,
	v: &mut SamplerView,
	d: &Device,
	note: u8,
	rect: Rect,
	lit: f32,
	drop: bool,
	mini: bool,
) {
	let layers = pad_layers(d, note);
	let loaded = !layers.is_empty();
	let selected = state.view.instrument.pad == note;
	let c = pad_color(note);
	let name = sm::pad_label(note);
	let resp = ui
		.interact(
			rect,
			Id::new(("drum-sampler-pad", cx.salt, cx.r.device, note)),
			Sense::click_and_drag(),
		)
		.on_hover_cursor(CursorIcon::PointingHand);
	let label = format!("{name} pad");
	resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &label));
	if resp.is_pointer_button_down_on()
		&& v.held.is_none()
		&& ui.input(|i| i.pointer.primary_pressed())
	{
		if state.view.instrument.pad != note {
			state.view.instrument.layer = 0;
		}
		state.view.instrument.pad = note;
		if loaded && let Some(track) = cx.r.track {
			// Higher on the pad = harder hit, like FPC.
			let y = ui.ctx().pointer_interact_pos().map_or(0.5, |p| {
				((rect.bottom() - p.y) / rect.height()).clamp(0.0, 1.0)
			});
			state.preview_note_on(track, note, 0.35 + 0.65 * y);
			v.held = Some((track, note));
		}
	}
	if resp.drag_started() {
		v.drag = Some(note);
	}
	if resp.double_clicked() && !loaded {
		state.pick_pad_sample(cx.r, note, false);
	}
	resp.context_menu(|ui| pad_menu(ui, state, cx.r, d, note));
	let resp = resp.on_hover_text(if loaded {
		format!(
			"{name} \u{b7} {} ({note}): click to hear it (higher = harder), drag onto another \
			 pad to swap (Alt: copy), right-click for choke group, output, reverse",
			ds::note_label(note)
		)
	} else {
		format!(
			"{name} \u{b7} {} ({note}): empty \u{2014} drop an audio file on it or double-click \
			 to pick one",
			ds::note_label(note)
		)
	});
	if !ui.is_rect_visible(rect) {
		return;
	}
	let dragging_onto = v.drag.is_some_and(|f| f != note) && resp.contains_pointer();
	let p = ui.painter();
	let fill = if drop {
		DROP_BG
	} else if !loaded {
		if resp.hovered() {
			theme::BG_RAISED
		} else {
			PAD_BG_EMPTY
		}
	} else if lit > 0.0 {
		theme::mix(PAD_BG, c, 0.12 + 0.2 * lit)
	} else if selected {
		PAD_BG_SELECTED
	} else if resp.hovered() {
		theme::mix(PAD_BG, PAD_BG_SELECTED, 0.6)
	} else {
		PAD_BG
	};
	if lit > 0.0 && !drop {
		p.rect_filled(
			rect.expand(3.0),
			CornerRadius::same(8),
			c.gamma_multiply(0.2 * lit),
		);
	}
	p.rect_filled(rect, CornerRadius::same(5), fill);
	if drop {
		dashed_rect(p, rect.shrink(0.75), Stroke::new(1.5, theme::ACCENT));
	} else if dragging_onto {
		p.rect_stroke(
			rect,
			CornerRadius::same(5),
			Stroke::new(1.5, theme::TEXT),
			StrokeKind::Inside,
		);
	} else {
		let ring = if selected && loaded {
			Stroke::new(1.5, c)
		} else if lit > 0.0 {
			Stroke::new(1.0, c)
		} else if selected {
			Stroke::new(1.5, theme::TEXT_FAINT)
		} else {
			Stroke::new(1.0, PAD_RING)
		};
		p.rect_stroke(rect, CornerRadius::same(5), ring, StrokeKind::Inside);
	}
	let inner = rect.shrink2(vec2(if mini { 5.0 } else { 7.0 }, 5.0));
	let top_y = inner.top() + 7.0;
	p.circle_filled(
		pos2(inner.left() + 3.0, top_y),
		3.0,
		if loaded { c } else { EMPTY_DOT },
	);
	let name_font = theme::semibold(if mini { 10.5 } else { 12.0 });
	let fits = |text: &str, w: f32| {
		p.layout_no_wrap(text.to_owned(), name_font.clone(), NAME)
			.size()
			.x <= w
	};
	// Narrow pads drop the note, then shorten the name.
	let note_g = p.layout_no_wrap(ds::note_label(note), theme::mono(10.0), theme::TEXT_FAINT);
	let room = inner.width() - 11.0;
	let full = if loaded {
		name.clone()
	} else {
		"Empty".to_owned()
	};
	let short = if loaded {
		ds::pad_short(note).to_owned()
	} else {
		"\u{2013}".to_owned()
	};
	let with_note = room - note_g.size().x - 6.0;
	let (shown_name, show_note) = if fits(&full, with_note) {
		(full, true)
	} else if fits(&short, with_note) {
		(short, true)
	} else if fits(&full, room) {
		(full, false)
	} else {
		(short, false)
	};
	if show_note {
		let w = note_g.size().x;
		p.galley(
			pos2(inner.right() - w, top_y - note_g.size().y * 0.5),
			note_g,
			theme::TEXT_FAINT,
		);
	}
	let shown_name = crate::ui::mixer::elide(ui, &shown_name, &name_font, room);
	let p = ui.painter();
	p.text(
		pos2(inner.left() + 11.0, top_y),
		Align2::LEFT_CENTER,
		shown_name,
		name_font,
		if loaded { NAME } else { theme::TEXT_FAINT },
	);
	let mid = Rect::from_min_max(
		pos2(inner.left(), top_y + 9.0),
		pos2(
			inner.right(),
			inner.bottom() - if mini { 2.0 } else { 18.0 },
		),
	);
	if drop || !loaded {
		let (text, color) = if drop {
			(
				if loaded {
					"Drop to replace"
				} else {
					"Drop sample"
				},
				DROP_TEXT,
			)
		} else {
			("Drop sample", theme::TEXT_FAINT)
		};
		if mid.height() >= 12.0 {
			p.text(
				pos2(inner.left(), mid.center().y),
				Align2::LEFT_CENTER,
				text,
				theme::medium(if mini { 10.0 } else { 11.0 }),
				color,
			);
		}
	} else if let Some(&first) = layers.first()
		&& mid.height() >= 8.0
	{
		let wave = Rect::from_center_size(
			mid.center(),
			vec2(
				mid.width(),
				mid.height().min(if mini { 14.0 } else { 18.0 }),
			),
		);
		mini_wave(ui, state, &d.zones[first], wave, c.gamma_multiply(0.6));
	}
	if !mini && loaded {
		let mut x = inner.left();
		let y = inner.bottom() - 15.0;
		let tags = pad_tags(d, note);
		let p = ui.painter();
		for t in tags.labels() {
			let (bg, fg) = if t.starts_with('C') {
				(CHOKE_BG, CHOKE_FG)
			} else if t.starts_with('\u{2192}') {
				(OUT_BG, DROP_TEXT)
			} else {
				(LAYER_BG, LAYER_FG)
			};
			let g = p.layout_no_wrap(t, theme::mono(9.5), fg);
			let r = Rect::from_min_size(pos2(x, y), vec2(g.size().x + 8.0, 15.0));
			if r.right() > inner.right() {
				break;
			}
			p.rect_filled(r, CornerRadius::same(2), bg);
			p.galley(r.center() - g.size() * 0.5, g, fg);
			x = r.right() + 3.0;
		}
	}
}

/// The pad's right-click menu.
fn pad_menu(ui: &mut Ui, state: &mut AppState, r: DeviceRef, d: &Device, note: u8) {
	let layers = pad_layers(d, note);
	let first = layers.first().map(|&i| d.zones[i].clone());
	ui.set_min_width(170.0);
	if let Some(z) = &first {
		ui.menu_button("Choke group", |ui| {
			for g in 0..=8u8 {
				let label = if g == 0 {
					"None".to_owned()
				} else {
					format!("Group {g}")
				};
				if ui.radio(z.choke == g, label).clicked() {
					let cmd = sm::edit_pad(state.project(), r, note, |z| z.choke = g);
					state.apply_sample_edit(cmd);
					ui.close();
				}
			}
		});
		ui.menu_button("Output", |ui| {
			for o in 0..=OUTPUTS {
				let label = if o == 0 {
					"Main".to_owned()
				} else {
					format!("Extra out {o}")
				};
				if ui.radio(z.output == o, label).clicked() {
					let cmd = sm::edit_pad(state.project(), r, note, |z| z.output = o);
					state.apply_sample_edit(cmd);
					ui.close();
				}
			}
		});
		let mut reverse = z.reverse;
		if ui.checkbox(&mut reverse, "Reverse").clicked() {
			let cmd = sm::edit_pad(state.project(), r, note, |z| z.reverse = reverse);
			state.apply_sample_edit(cmd);
			ui.close();
		}
		ui.separator();
	}
	if ui
		.button(if first.is_some() {
			"Replace sample\u{2026}"
		} else {
			"Load sample\u{2026}"
		})
		.clicked()
	{
		state.pick_pad_sample(r, note, false);
		ui.close();
	}
	if first.is_some() {
		if ui.button("Add layer\u{2026}").clicked() {
			state.pick_pad_sample(r, note, true);
			ui.close();
		}
		if ui.button("Clear").clicked() {
			let cmd = sm::clear_pad(state.project(), r, note);
			state.apply_sample_edit(cmd);
			ui.close();
		}
	}
	if let Some(z) = &first
		&& ui.button("Locate file").clicked()
	{
		state.locate_file(&z.sample.path);
		ui.close();
	}
}

/// A pad dragged onto another: the ghost while dragging, the swap / copy on release.
fn pad_drag(ui: &mut Ui, state: &mut AppState, cx: &BodyCx, v: &mut SamplerView, hits: &PadHits) {
	let Some(from) = v.drag else {
		return;
	};
	let pointer = ui.ctx().pointer_latest_pos();
	let target = pointer.and_then(|p| {
		hits.pads
			.iter()
			.find(|(r, _)| r.contains(p))
			.map(|(_, n)| *n)
	});
	let copy = ui.input(|i| i.modifiers.alt);
	if ui.input(|i| i.pointer.any_released()) {
		v.drag = None;
		if let Some(to) = target.filter(|t| *t != from) {
			let cmd = ds::move_pad(state.project(), cx.r, from, to, copy);
			if state.apply_sample_edit(cmd) {
				state.view.instrument.pad = to;
				state.view.instrument.layer = 0;
			}
		}
		return;
	}
	if let Some(pos) = pointer {
		let text = match target.filter(|t| *t != from) {
			Some(to) if copy => format!("Copy {} to {}", sm::pad_label(from), sm::pad_label(to)),
			Some(to) => format!(
				"Swap {} \u{2194} {}",
				sm::pad_label(from),
				sm::pad_label(to)
			),
			None => sm::pad_label(from),
		};
		let layer = egui::LayerId::new(egui::Order::Tooltip, Id::new("drum-sampler-pad-ghost"));
		let p = ui.ctx().layer_painter(layer);
		let g = p.layout_no_wrap(text, theme::medium(11.5), theme::TEXT);
		let r = Rect::from_min_size(pos + vec2(12.0, 10.0), g.size() + vec2(14.0, 8.0));
		p.rect(
			r,
			CornerRadius::same(theme::RADIUS_MD),
			theme::BG_WIDGET,
			Stroke::new(1.0, pad_color(from)),
			StrokeKind::Inside,
		);
		p.galley(r.min + vec2(7.0, 4.0), g, theme::TEXT);
		ui.ctx().request_repaint();
	}
}

/// A dashed blue frame over the kit while a folder is dragged over it.
fn drop_overlay(ui: &Ui, hits: &PadHits) {
	let (Some(path), Some(kit), Some(pos)) = (
		dragged_file(ui.ctx()),
		hits.kit,
		ui.ctx().pointer_latest_pos(),
	) else {
		return;
	};
	if !kit.contains(pos) || !path.is_dir() {
		return;
	}
	let p = ui.painter();
	p.rect_filled(
		kit.expand(2.0),
		CornerRadius::same(theme::RADIUS_LG),
		DROP_BG.gamma_multiply(0.97),
	);
	dashed_rect(p, kit.expand(2.0), Stroke::new(1.5, theme::ACCENT));
	let name = path.file_name().unwrap_or_default().to_string_lossy();
	let name = crate::ui::mixer::elide(ui, &name, &theme::semibold(13.0), kit.width() - 80.0);
	p.text(
		kit.center() - vec2(0.0, 52.0),
		Align2::CENTER_CENTER,
		format!("Map \u{201c}{name}\u{201d}"),
		theme::semibold(13.0),
		DROP_TEXT,
	);
	p.text(
		kit.center() - vec2(0.0, 32.0),
		Align2::CENTER_CENTER,
		"onto the pads by file name",
		theme::medium(11.5),
		DROP_TEXT.gamma_multiply(0.8),
	);
}

// --- Waveforms ------------------------------------------------------------------------

/// Where a waveform's columns come from: the peak levels, or the decoded samples once
/// a pixel spans fewer frames than the finest peak bucket.
#[derive(Clone, Copy)]
pub(crate) enum WaveSource<'a> {
	Peaks(&'a cadenza_audio_io::WaveformPeaks),
	Samples(&'a cadenza_audio_io::AudioBuffer),
}

impl WaveSource<'_> {
	pub(crate) fn rate(&self) -> f64 {
		f64::from(match self {
			WaveSource::Peaks(p) => p.sample_rate.max(1),
			WaveSource::Samples(b) => b.sample_rate().max(1),
		})
	}

	/// Largest absolute value over frames `f0..f1` (`fpp` = frames per pixel).
	fn peak(&self, f0: u64, f1: u64, fpp: f64) -> Option<f32> {
		match self {
			WaveSource::Peaks(p) => {
				let level = p.level_for(fpp);
				let mut mm = cadenza_audio_io::MinMax::EMPTY;
				for ch in 0..usize::from(p.num_channels).max(1) {
					mm = mm.merge(level.range(ch, f0, f1));
				}
				(!mm.is_empty()).then(|| mm.max.abs().max(mm.min.abs()))
			}
			WaveSource::Samples(b) => {
				let n = b.frames() as u64;
				let (f0, f1) = (f0.min(n) as usize, f1.min(n) as usize);
				(f1 > f0).then(|| {
					(0..b.num_channels())
						.flat_map(|c| b.channel(c)[f0..f1].iter())
						.fold(0.0_f32, |m, v| m.max(v.abs()))
				})
			}
		}
	}
}

/// The sample between `from` and `to` seconds, mirrored around the centre line
/// (columns quieter than `floor` of the height are left out).
#[allow(clippy::too_many_arguments)]
pub(crate) fn paint_wave(
	p: &egui::Painter,
	rect: Rect,
	src: WaveSource<'_>,
	length: f64,
	from: f64,
	to: f64,
	gain: f32,
	color: Color32,
	reverse: bool,
	floor: f32,
) {
	let sr = src.rate();
	let span = (to - from).max(1e-9);
	let fpp = (span * sr / f64::from(rect.width().max(1.0))).max(1.0);
	let mid = rect.center().y;
	let half = rect.height() * 0.5;
	let mut x = rect.left();
	while x < rect.right() {
		let t0 = from + f64::from((x - rect.left()) / rect.width()) * span;
		let t1 = t0 + span / f64::from(rect.width());
		let (t0, t1) = if reverse {
			(length - t1, length - t0)
		} else {
			(t0, t1)
		};
		let f0 = (t0.max(0.0) * sr) as u64;
		let f1 = ((t1.max(0.0) * sr) as u64).max(f0 + 1);
		if let Some(v) = src.peak(f0, f1, fpp) {
			let a = (v * gain).min(1.0);
			if a > floor {
				p.vline(
					x + 0.5,
					(mid - a * half)..=(mid + a * half),
					Stroke::new(1.0, color),
				);
			}
		}
		x += 1.0;
	}
}

/// A pad's thumbnail: the played region up to where it falls below -30 dB, scaled to
/// its peak.
fn mini_wave(ui: &Ui, state: &mut AppState, zone: &SampleZone, rect: Rect, color: Color32) {
	let Peaks::Ready(peaks) = state.peaks.get(&zone.sample.path) else {
		return;
	};
	let sr = f64::from(peaks.sample_rate.max(1));
	let length = peaks.frames as f64 / sr;
	let end = zone.end.unwrap_or(length).min(length);
	let start = zone.start.min(end);
	let src = WaveSource::Peaks(&peaks);
	let coarse = (end - start) * sr / 64.0;
	let buckets: Vec<f32> = (0..64)
		.map(|k| {
			let f0 = (start * sr + f64::from(k) * coarse) as u64;
			src.peak(f0, f0 + coarse.max(1.0) as u64, coarse.max(1.0))
				.unwrap_or(0.0)
		})
		.collect();
	let top = buckets.iter().copied().fold(0.0_f32, f32::max);
	if top <= 1e-6 {
		return;
	}
	let audible = buckets
		.iter()
		.rposition(|b| *b > top * 0.03)
		.map_or(64, |k| k + 1);
	let to = start + (end - start) * audible as f64 / 64.0;
	let (from, to) = if zone.reverse {
		(length - to, length - start)
	} else {
		(start, to)
	};
	paint_wave(
		ui.painter(),
		rect,
		src,
		length,
		from,
		to,
		0.95 / top,
		color,
		zone.reverse,
		0.04,
	);
}

// --- Selected pad ---------------------------------------------------------------------

fn selected_panel(
	ui: &mut Ui,
	state: &mut AppState,
	cx: &BodyCx,
	v: &mut SamplerView,
	hits: &mut PadHits,
	d: &Device,
	rect: Rect,
) {
	let note = state.view.instrument.pad;
	let layers = pad_layers(d, note);
	let c = pad_color(note);
	let head = Rect::from_min_size(rect.min, vec2(rect.width(), HEAD_H));
	let body = Rect::from_min_max(pos2(rect.left(), head.bottom() + 6.0), rect.max);
	header(ui, state, cx, d, note, &layers, head);
	let side_w = (body.width() * 0.32).clamp(220.0, 270.0);
	let wave = Rect::from_min_max(body.min, pos2(body.right() - side_w - 10.0, body.bottom()));
	let side = Rect::from_min_max(pos2(wave.right() + 10.0, body.top()), body.max);
	let layer = state
		.view
		.instrument
		.layer
		.min(layers.len().saturating_sub(1));
	match layers.get(layer) {
		Some(&i) => {
			let zone = d.zones[i].clone();
			wave_editor(ui, state, cx, v, hits, note, layer, &zone, wave, c);
			pad_controls(ui, state, cx, note, &zone, side, c);
		}
		None => {
			hits.wave = Some(wave);
			empty_wave(ui, state, cx, note, wave);
		}
	}
}

/// "■ Snare D1 [snare_brush_04.wav ▾] … Velocity layers 1 2 3 + · Round-robin ×3 · ⇆ Reverse".
#[allow(clippy::too_many_arguments)]
fn header(
	ui: &mut Ui,
	state: &mut AppState,
	cx: &BodyCx,
	d: &Device,
	note: u8,
	layers: &[usize],
	rect: Rect,
) {
	let c = pad_color(note);
	let cy = rect.center().y;
	let p = ui.painter();
	p.rect_filled(
		Rect::from_center_size(pos2(rect.left() + 5.0, cy), vec2(10.0, 10.0)),
		2.0,
		c,
	);
	let name = p.text(
		pos2(rect.left() + 18.0, cy),
		Align2::LEFT_CENTER,
		sm::pad_label(note),
		theme::semibold(14.0),
		theme::TEXT,
	);
	let note_r = p.text(
		pos2(name.right() + 8.0, cy),
		Align2::LEFT_CENTER,
		ds::note_label(note),
		theme::mono(12.0),
		theme::TEXT_FAINT,
	);
	let layer = state
		.view
		.instrument
		.layer
		.min(layers.len().saturating_sub(1));
	let zone = layers.get(layer).map(|&i| d.zones[i].clone());
	// Right side, laid out from the right edge.
	let mut right = rect.right();
	if let Some(z) = &zone {
		let rev = Rect::from_min_max(pos2(right - 82.0, rect.top()), pos2(right, rect.bottom()));
		let resp = chip_button(
			ui,
			Id::new(("ds-reverse", cx.salt, cx.r.device)),
			rev,
			"\u{21c6} Reverse",
			z.reverse,
		)
		.on_hover_text("Play the pad backwards");
		if resp.clicked() {
			let on = !z.reverse;
			let cmd = sm::edit_pad(state.project(), cx.r, note, |z| z.reverse = on);
			state.apply_sample_edit(cmd);
		}
		right = rev.left() - 10.0;
		// Round-robin.
		let rr = ds::is_round_robin(d, note);
		let text = if rr {
			format!("\u{d7}{}", layers.len())
		} else {
			"Off".to_owned()
		};
		let g = ui
			.painter()
			.layout_no_wrap(text.clone(), theme::mono(11.0), theme::TEXT);
		let chip = Rect::from_min_max(
			pos2(right - g.size().x - 14.0, cy - 10.0),
			pos2(right, cy + 10.0),
		);
		let resp = chip_button(
			ui,
			Id::new(("ds-rr", cx.salt, cx.r.device)),
			chip,
			&text,
			rr,
		)
		.on_hover_text(if layers.len() < 2 {
			"Round-robin: add a second layer (+) to alternate between samples"
		} else if rr {
			"The layers take turns on every hit; click to split them by velocity instead"
		} else {
			"The layers split the velocity range; click to make them take turns instead"
		});
		if resp.clicked() {
			let cmd = ds::set_round_robin(state.project(), cx.r, note, !rr);
			state.apply_sample_edit(cmd);
		}
		let label = ui.painter().text(
			pos2(chip.left() - 6.0, cy),
			Align2::RIGHT_CENTER,
			"Round-robin",
			theme::medium(12.0),
			theme::TEXT_FAINT,
		);
		right = label.left() - 14.0;
		// Layer chips, right to left: "+" then n..1.
		let plus = Rect::from_min_max(pos2(right - 22.0, cy - 10.0), pos2(right, cy + 10.0));
		let resp = ui
			.interact(
				plus,
				Id::new(("ds-layer-add", cx.salt, cx.r.device)),
				Sense::click(),
			)
			.on_hover_cursor(CursorIcon::PointingHand)
			.on_hover_text("Add a layer: another sample for this pad (velocity or round-robin)");
		resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Add layer"));
		let p = ui.painter();
		dashed_rect(
			p,
			plus.shrink(0.5),
			Stroke::new(
				1.0,
				if resp.hovered() {
					theme::TEXT_FAINT
				} else {
					theme::BG_PRESSED
				},
			),
		);
		p.text(
			plus.center(),
			Align2::CENTER_CENTER,
			"+",
			theme::medium(12.0),
			theme::TEXT_FAINT,
		);
		if resp.clicked() {
			state.pick_pad_sample(cx.r, note, true);
		}
		let mut x = plus.left() - 2.0;
		for k in (0..layers.len()).rev() {
			let chip = Rect::from_min_max(pos2(x - 22.0, cy - 10.0), pos2(x, cy + 10.0));
			x = chip.left() - 2.0;
			let on = k == layer;
			let resp = ui
				.interact(
					chip,
					Id::new(("ds-layer", cx.salt, cx.r.device, k)),
					Sense::click(),
				)
				.on_hover_cursor(CursorIcon::PointingHand);
			let z = &d.zones[layers[k]];
			let label = format!("Layer {}", k + 1);
			resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &label));
			let p = ui.painter();
			p.rect(
				chip,
				CornerRadius::same(theme::RADIUS),
				if on {
					theme::BG_PRESSED
				} else {
					theme::BG_HOVER
				},
				Stroke::new(
					1.0,
					if on {
						theme::TEXT_FAINT
					} else {
						Color32::TRANSPARENT
					},
				),
				StrokeKind::Inside,
			);
			p.text(
				chip.center(),
				Align2::CENTER_CENTER,
				(k + 1).to_string(),
				theme::mono(10.5),
				if on { theme::TEXT } else { theme::TEXT_DIM },
			);
			if resp.clicked() {
				state.view.instrument.layer = k;
			}
			let resp = resp.on_hover_text(format!(
				"{}: velocity {:.0}\u{2013}{:.0}",
				z.sample.display_name(),
				z.vel_low * 127.0,
				z.vel_high * 127.0
			));
			resp.context_menu(|ui| {
				if ui.button("Remove layer").clicked() {
					let cmd = ds::remove_layer(state.project(), cx.r, note, k);
					if state.apply_sample_edit(cmd) {
						state.view.instrument.layer = 0;
					}
					ui.close();
				}
				if ui.button("Locate file").clicked() {
					state.locate_file(&z.sample.path);
					ui.close();
				}
			});
		}
		let text = if rr { "Layers" } else { "Velocity layers" };
		let lr = ui.painter().text(
			pos2(x - 6.0, cy),
			Align2::RIGHT_CENTER,
			text,
			theme::medium(12.0),
			theme::TEXT_FAINT,
		);
		right = lr.left() - 14.0;
	}
	// The file chip with its menu, between the note and the controls.
	let file = zone
		.as_ref()
		.map_or_else(|| "No sample".to_owned(), |z| z.sample.display_name());
	let max_w = (right - note_r.right() - 12.0).min(280.0);
	if max_w < 60.0 {
		return;
	}
	let shown = crate::ui::mixer::elide(ui, &file, &theme::mono(11.0), max_w - 28.0);
	let g = ui
		.painter()
		.layout_no_wrap(shown, theme::mono(11.0), LAYER_FG);
	let chip = Rect::from_min_size(
		pos2(note_r.right() + 10.0, cy - 11.0),
		vec2(g.size().x + 30.0, 22.0),
	);
	let resp = ui
		.interact(
			chip,
			Id::new(("ds-file", cx.salt, cx.r.device)),
			Sense::click(),
		)
		.on_hover_cursor(CursorIcon::PointingHand);
	resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::ComboBox, true, "Pad sample"));
	let p = ui.painter();
	p.rect(
		chip,
		CornerRadius::same(theme::RADIUS),
		if resp.hovered() {
			theme::BG_RAISED
		} else {
			FIELD_BG
		},
		Stroke::new(1.0, PAD_RING),
		StrokeKind::Inside,
	);
	p.galley(pos2(chip.left() + 8.0, cy - g.size().y * 0.5), g, LAYER_FG);
	p.text(
		pos2(chip.right() - 10.0, cy),
		Align2::CENTER_CENTER,
		"\u{25be}",
		theme::medium(11.0),
		theme::TEXT_FAINT,
	);
	let resp = match &zone {
		Some(z) => resp.on_hover_text(z.sample.path.display().to_string()),
		None => resp,
	};
	egui::Popup::menu(&resp).show(|ui| {
		ui.set_min_width(180.0);
		if ui
			.button(if zone.is_some() {
				"Replace sample\u{2026}"
			} else {
				"Load sample\u{2026}"
			})
			.clicked()
		{
			state.pick_pad_sample(cx.r, note, false);
			ui.close();
		}
		if let Some(z) = &zone {
			if ui.button("Locate file").clicked() {
				state.locate_file(&z.sample.path);
				ui.close();
			}
			if ui.button("Clear pad").clicked() {
				let cmd = sm::clear_pad(state.project(), cx.r, note);
				state.apply_sample_edit(cmd);
				ui.close();
			}
		}
	});
}

fn empty_wave(ui: &mut Ui, state: &mut AppState, cx: &BodyCx, note: u8, rect: Rect) {
	let p = ui.painter();
	p.rect_filled(
		rect,
		CornerRadius::same(theme::RADIUS_MD),
		theme::DISPLAY_BG,
	);
	dashed_rect(p, rect.shrink(6.0), Stroke::new(1.0, theme::BG_PRESSED));
	p.text(
		rect.center() - vec2(0.0, 18.0),
		Align2::CENTER_CENTER,
		format!(
			"{} is empty \u{2014} drop an audio file on its pad",
			sm::pad_label(note)
		),
		theme::medium(12.0),
		theme::TEXT_FAINT,
	);
	let b = Rect::from_center_size(rect.center() + vec2(0.0, 14.0), vec2(120.0, 24.0));
	if chip_button(
		ui,
		Id::new(("ds-load", cx.salt, cx.r.device)),
		b,
		"Load sample\u{2026}",
		false,
	)
	.clicked()
	{
		state.pick_pad_sample(cx.r, note, false);
	}
}

/// The selected layer's waveform: start / end flags, the A/D envelope, zoom.
#[allow(clippy::too_many_arguments)]
fn wave_editor(
	ui: &mut Ui,
	state: &mut AppState,
	cx: &BodyCx,
	v: &mut SamplerView,
	hits: &mut PadHits,
	note: u8,
	layer: usize,
	zone: &SampleZone,
	rect: Rect,
	c: Color32,
) {
	hits.wave = Some(rect);
	ui.painter().rect(
		rect,
		CornerRadius::same(theme::RADIUS_MD),
		theme::DISPLAY_BG,
		Stroke::new(1.0, KIT_RING),
		StrokeKind::Inside,
	);
	let peaks = match state.peaks.get(&zone.sample.path) {
		Peaks::Ready(p) => p,
		Peaks::Pending => {
			ui.painter().text(
				rect.center(),
				Align2::CENTER_CENTER,
				"loading waveform\u{2026}",
				theme::small(),
				theme::TEXT_FAINT,
			);
			return;
		}
		Peaks::Failed(e) => {
			ui.painter().text(
				rect.center(),
				Align2::CENTER_CENTER,
				format!("\u{26a0} {e} \u{2014} drop another file on the pad"),
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
	let mut window = match &v.window {
		Some((p, w)) if *p == path => w.clamped(length),
		_ => WaveWindow::full(length),
	};
	let area = rect.shrink(1.0);
	let x_of =
		|s: f64, w: &WaveWindow| area.left() + ((s - w.from) / w.span()) as f32 * area.width();
	let s_of =
		|x: f32, w: &WaveWindow| w.from + f64::from((x - area.left()) / area.width()) * w.span();
	// Wheel: zoom at the pointer; Shift: scroll.
	let bg = ui.interact(
		rect,
		Id::new(("ds-wave", cx.salt, cx.r.device)),
		Sense::hover(),
	);
	let scroll = ui.input(|i| i.smooth_scroll_delta.x + i.smooth_scroll_delta.y);
	if bg.hovered() && scroll != 0.0 {
		ui.input_mut(|i| i.smooth_scroll_delta = egui::Vec2::ZERO);
		let notches = f64::from(scroll / device_kit::NOTCH);
		if ui.input(|i| i.modifiers.shift) {
			window = window.scroll(-notches * window.span() * 0.15, length);
		} else if let Some(pos) = ui.ctx().pointer_latest_pos() {
			window = window.zoom(s_of(pos.x, &window), WHEEL_ZOOM.powf(notches), length);
		}
	}
	v.window = Some((path, window));
	// The zone with the gesture being dragged applied.
	let mut shown = zone.clone();
	if let Some((m, s)) = v.flag {
		m.set(&mut shown, s, length);
	}
	match v.node {
		Some((Node::Attack, Some(a))) => shown.attack_ms = a,
		Some((Node::Decay, dms)) => shown.decay_ms = dms,
		_ => {}
	}
	let start = Marker::Start.get(&shown, length);
	let end = Marker::End.get(&shown, length).max(start);
	let p = ui.painter().with_clip_rect(area);
	p.hline(
		area.x_range(),
		area.center().y,
		Stroke::new(1.0, Color32::from_rgb(0x1f, 0x21, 0x25)),
	);
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
	// The envelope: from the start flag forwards (reversed: from the end flag back).
	let kit_tune = devices::shown(state, cx.r, cx.info, "tune");
	let rate = ds::playback_rate(shown.tune, kit_tune);
	let top = area.top() + 18.0;
	let bottom = area.bottom() - 14.0;
	let dir = if shown.reverse { -1.0 } else { 1.0 };
	let origin = if shown.reverse { end } else { start };
	let x_at_ms = |ms: f32| x_of(origin + dir * f64::from(ms) * 0.001 * rate, &window);
	let y_of = |a: f32| bottom - a.max(0.0).sqrt() * (bottom - top);
	let region_ms = ((end - start) / rate * 1000.0) as f32;
	let n = 160;
	let points: Vec<Pos2> = (0..=n)
		.map(|k| {
			let t = k as f32 / n as f32 * region_ms;
			pos2(
				x_at_ms(t),
				y_of(envelope(shown.attack_ms, shown.decay_ms, t)),
			)
		})
		.collect();
	p.add(Shape::line(
		points.clone(),
		Stroke::new(6.0, ENVELOPE.gamma_multiply(0.12)),
	));
	p.add(Shape::line(points, Stroke::new(1.5, ENVELOPE)));
	let a_ms = shown.attack_ms.min(region_ms);
	let d_ms = shown
		.decay_ms
		.map(|d| a_ms + d * ds::DECAY_NODE)
		.filter(|t| *t < region_ms);
	let a_pos = pos2(x_at_ms(a_ms), top);
	let d_pos = match d_ms {
		Some(t) => pos2(
			x_at_ms(t),
			y_of(envelope(shown.attack_ms, shown.decay_ms, t)),
		),
		None => pos2(x_at_ms(region_ms) - dir as f32 * 8.0, top),
	};
	// Flags (drawn first so the nodes win near the top).
	for (m, x, color) in [
		(Marker::Start, sx, theme::PLAYING),
		(Marker::End, ex, theme::ERROR),
	] {
		p.vline(x, area.y_range(), Stroke::new(1.5, color));
		let handle = Rect::from_x_y_ranges(x - 5.0..=x + 5.0, area.y_range());
		let resp = ui
			.interact(
				handle,
				Id::new(("ds-flag", cx.salt, cx.r.device, m.label())),
				Sense::click_and_drag(),
			)
			.on_hover_cursor(CursorIcon::ResizeHorizontal);
		let label = if m == Marker::Start {
			"Start flag"
		} else {
			"End flag"
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
			let cmd = ds::edit_layer(state.project(), cx.r, note, layer, |z| m.set(z, s, length));
			let name = if m == Marker::Start { "start" } else { "end" };
			state.apply_sample_edit(
				cmd.map(|c| ds::labeled(c, format!("{} {name}", sm::pad_label(note)))),
			);
		}
		if resp.double_clicked() {
			v.flag = None;
			let cmd = ds::edit_layer(state.project(), cx.r, note, layer, |z| match m {
				Marker::Start => z.start = 0.0,
				_ => z.end = None,
			});
			state.apply_sample_edit(cmd);
		}
		let _ = resp.on_hover_text(if m == Marker::Start {
			"Start: drag where the pad starts playing, double-click resets to the sample start"
		} else {
			"End: drag where the pad stops, double-click resets to the sample end"
		});
	}
	// Readouts on the flags.
	let tag = |p: &egui::Painter, x: f32, right: bool, text: String, fg: Color32, bg: Color32| {
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
		&p,
		sx,
		false,
		format!("Start {}", ms_label(start)),
		theme::PLAYING,
		theme::PLAYING_SOFT,
	);
	let e_text = format!("End {}", ms_label(end));
	let e_w = p
		.layout_no_wrap(e_text.clone(), theme::mono(10.5), END_FG)
		.size()
		.x + 10.0;
	let right_aligned = ex - 4.0 - e_w > s_tag.right() + 6.0;
	if right_aligned || ex + 4.0 + e_w < area.right() {
		tag(&p, ex, right_aligned, e_text, END_FG, END_BG);
	}
	// Envelope nodes.
	for (node, pos) in [(Node::Attack, a_pos), (Node::Decay, d_pos)] {
		let hit = Rect::from_center_size(pos, vec2(16.0, 16.0));
		let resp = ui
			.interact(
				hit,
				Id::new(("ds-node", cx.salt, cx.r.device, node as u8)),
				Sense::click_and_drag(),
			)
			.on_hover_cursor(CursorIcon::ResizeHorizontal);
		let label = if node == Node::Attack {
			"Attack node"
		} else {
			"Decay node"
		};
		resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Slider, true, label));
		let active = resp.dragged() || resp.hovered();
		p.circle(
			pos,
			if active { 7.0 } else { 6.0 },
			ENVELOPE,
			Stroke::new(1.5, theme::DISPLAY_BG),
		);
		if resp.dragged()
			&& let Some(at) = ui.ctx().pointer_interact_pos()
		{
			let ms = (dir * (s_of(at.x, &window) - origin) / rate * 1000.0) as f32;
			let value = match node {
				Node::Attack => Some(ms.clamp(0.0, 500.0).round()),
				// Dragged out to the end of the sample: no decay.
				Node::Decay if ms >= region_ms - 1.0 => None,
				Node::Decay => Some(ds::decay_from_node(shown.attack_ms, ms).round()),
			};
			v.node = Some((node, value));
			ui.ctx().request_repaint();
		}
		if resp.drag_stopped()
			&& let Some((node, value)) = v.node.take()
		{
			let cmd = sm::edit_pad(state.project(), cx.r, note, |z| match node {
				Node::Attack => z.attack_ms = value.unwrap_or(0.0),
				Node::Decay => z.decay_ms = value,
			});
			let what = if node == Node::Attack {
				"attack"
			} else {
				"decay"
			};
			state.apply_sample_edit(
				cmd.map(|c| ds::labeled(c, format!("{} {what}", sm::pad_label(note)))),
			);
		}
		if resp.double_clicked() {
			let cmd = sm::edit_pad(state.project(), cx.r, note, |z| match node {
				Node::Attack => z.attack_ms = 0.0,
				Node::Decay => z.decay_ms = None,
			});
			state.apply_sample_edit(cmd);
		}
		let _ = resp.on_hover_text(if node == Node::Attack {
			"Attack: drag sideways (fade-in time), double-click resets to none"
		} else {
			"Decay: drag sideways (time to fade out); out to the end = the whole sample, \
			 double-click resets"
		});
	}
	let a_text = format!("A {}", ms_label(f64::from(shown.attack_ms) * 0.001));
	let d_text = shown.decay_ms.map_or_else(
		|| "D full".to_owned(),
		|d| format!("D {}", ms_label(f64::from(d) * 0.001)),
	);
	let label_at = |pos: Pos2, text: String, below: bool| {
		let g = p.layout_no_wrap(text, theme::mono(10.5), LAYER_FG);
		let mut at = pos
			+ vec2(
				10.0 * dir as f32,
				if below { 4.0 } else { -6.0 - g.size().y * 0.5 },
			);
		if dir < 0.0 {
			at.x -= g.size().x;
		}
		at.x =
			at.x.clamp(area.left() + 2.0, area.right() - g.size().x - 2.0);
		at.y =
			at.y.clamp(area.top() + 2.0, area.bottom() - g.size().y - 24.0);
		p.galley(at, g, LAYER_FG);
	};
	label_at(a_pos + vec2(0.0, 6.0), a_text, false);
	label_at(d_pos - vec2(0.0, 14.0), d_text, false);
	if !window.is_full(length) {
		p.text(
			area.right_top() + vec2(-6.0, 4.0),
			Align2::RIGHT_TOP,
			format!("{}\u{2013}{}", ms_label(window.from), ms_label(window.to)),
			theme::mono(10.0),
			theme::TEXT_FAINT,
		);
	}
	let _ = bg.on_hover_text_at_pointer(
		"Wheel zooms, Shift+wheel scrolls; drag the green / red flags and the envelope nodes",
	);
}

/// Tune / gain / pan / attack / decay knobs and the choke group / output pickers.
fn pad_controls(
	ui: &mut Ui,
	state: &mut AppState,
	cx: &BodyCx,
	note: u8,
	zone: &SampleZone,
	rect: Rect,
	c: Color32,
) {
	let knob_h = devices::knob_height(KnobSize::S);
	let knobs = Rect::from_min_size(rect.min, vec2(rect.width(), 2.0 * knob_h + 10.0));
	pad_knobs(ui, state, cx, note, zone, knobs, 3, c);
	let pick_h = 40.0;
	let picks = Rect::from_min_max(
		pos2(
			rect.left(),
			(knobs.bottom() + 6.0).max(rect.bottom() - pick_h),
		),
		pos2(rect.right(), rect.bottom()),
	);
	if picks.top() + pick_h > rect.bottom() + 0.5 {
		return;
	}
	let half = (picks.width() - 6.0) * 0.5;
	let choke = Rect::from_min_size(picks.min, vec2(half, pick_h));
	let out = Rect::from_min_size(pos2(choke.right() + 6.0, picks.top()), vec2(half, pick_h));
	let choke_text = if zone.choke == 0 {
		"None".to_owned()
	} else {
		format!("Group {}", zone.choke)
	};
	picker(
		ui,
		state,
		cx,
		note,
		choke,
		"Choke group",
		&choke_text,
		|ui, state| {
			for g in 0..=8u8 {
				let label = if g == 0 {
					"None".to_owned()
				} else {
					format!("Group {g}")
				};
				if ui.radio(zone.choke == g, label).clicked() {
					let cmd = sm::edit_pad(state.project(), cx.r, note, |z| z.choke = g);
					state.apply_sample_edit(cmd);
					ui.close();
				}
			}
		},
	);
	let out_text = if zone.output == 0 {
		"Main".to_owned()
	} else {
		format!("Extra out {}", zone.output)
	};
	picker(
		ui,
		state,
		cx,
		note,
		out,
		"Output",
		&out_text,
		|ui, state| {
			for o in 0..=OUTPUTS {
				let label = if o == 0 {
					"Main".to_owned()
				} else {
					format!("Extra out {o}")
				};
				if ui.radio(zone.output == o, label).clicked() {
					let cmd = sm::edit_pad(state.project(), cx.r, note, |z| z.output = o);
					state.apply_sample_edit(cmd);
					ui.close();
				}
			}
		},
	);
}

/// A labelled dropdown field.
#[allow(clippy::too_many_arguments)]
fn picker(
	ui: &mut Ui,
	state: &mut AppState,
	cx: &BodyCx,
	note: u8,
	rect: Rect,
	label: &str,
	value: &str,
	menu: impl FnOnce(&mut Ui, &mut AppState),
) {
	ui.painter().text(
		rect.left_top(),
		Align2::LEFT_TOP,
		label,
		theme::medium(10.5),
		theme::TEXT_FAINT,
	);
	let field = Rect::from_min_max(pos2(rect.left(), rect.bottom() - 24.0), rect.max);
	let resp = ui
		.interact(
			field,
			Id::new(("ds-pick", cx.salt, cx.r.device, note, label)),
			Sense::click(),
		)
		.on_hover_cursor(CursorIcon::PointingHand);
	resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::ComboBox, true, label));
	let p = ui.painter();
	p.rect(
		field,
		CornerRadius::same(theme::RADIUS),
		if resp.hovered() {
			theme::BG_RAISED
		} else {
			FIELD_BG
		},
		Stroke::new(1.0, PAD_RING),
		StrokeKind::Inside,
	);
	p.text(
		field.left_center() + vec2(8.0, 0.0),
		Align2::LEFT_CENTER,
		value,
		theme::mono(11.5),
		theme::TEXT,
	);
	p.text(
		field.right_center() - vec2(10.0, 0.0),
		Align2::CENTER_CENTER,
		"\u{25be}",
		theme::medium(11.0),
		theme::TEXT_FAINT,
	);
	egui::Popup::menu(&resp).show(|ui| {
		ui.set_min_width(field.width());
		menu(ui, state);
	});
}

/// The pad's knobs in `cols` columns: tune, gain, pan (pad colour), attack, decay.
#[allow(clippy::too_many_arguments)]
fn pad_knobs(
	ui: &mut Ui,
	state: &mut AppState,
	cx: &BodyCx,
	note: u8,
	zone: &SampleZone,
	rect: Rect,
	cols: usize,
	c: Color32,
) {
	struct K {
		name: &'static str,
		value: f32,
		spec: KnobSpec,
		unit: &'static str,
		color: Color32,
		apply: fn(&mut SampleZone, f32),
	}
	let decay_max = 4000.0;
	let knobs = [
		K {
			name: "Tune",
			value: zone.tune,
			spec: KnobSpec::new(-24.0, 24.0, 0.0),
			unit: "semitones",
			color: c,
			apply: |z, v| z.tune = (v * 100.0).round() / 100.0,
		},
		K {
			name: "Gain",
			value: zone.gain_db,
			spec: KnobSpec::new(-60.0, 24.0, 0.0),
			unit: "dB",
			color: c,
			apply: |z, v| z.gain_db = (v * 10.0).round() / 10.0,
		},
		K {
			name: "Pan",
			value: zone.pan * 100.0,
			spec: KnobSpec::new(-100.0, 100.0, 0.0),
			unit: "pan%",
			color: c,
			apply: |z, v| z.pan = (v.round() / 100.0).clamp(-1.0, 1.0),
		},
		K {
			name: "Attack",
			value: zone.attack_ms,
			spec: KnobSpec::new(0.0, 500.0, 0.0),
			unit: "ms",
			color: ENVELOPE,
			apply: |z, v| z.attack_ms = v.round(),
		},
		K {
			name: "Decay",
			value: zone.decay_ms.unwrap_or(decay_max),
			spec: KnobSpec::new(5.0, decay_max, decay_max).log(),
			unit: "ms",
			color: ENVELOPE,
			apply: |z, v| z.decay_ms = (v < 3999.0).then_some(v.round()),
		},
	];
	let cols = cols.clamp(1, knobs.len());
	let rows = knobs.len().div_ceil(cols);
	let cell = vec2(rect.width() / cols as f32, rect.height() / rows as f32);
	for (i, k) in knobs.iter().enumerate() {
		let at = rect.min + vec2((i % cols) as f32 * cell.x, (i / cols) as f32 * cell.y);
		let h = devices::knob_height(KnobSize::S);
		let cell_rect = Rect::from_min_size(at + vec2(0.0, (cell.y - h) * 0.5), vec2(cell.x, h));
		let mut child = ui.new_child(
			UiBuilder::new()
				.max_rect(cell_rect)
				.layout(Layout::top_down(egui::Align::Center)),
		);
		let mut knob = Knob::new(
			Id::new(("ds-knob", cx.salt, cx.r.device, note, k.name)),
			k.value,
			k.spec,
		)
		.size(KnobSize::S)
		.label(k.name)
		.unit(k.unit)
		.color(k.color);
		if k.name == "Decay" {
			knob = knob.word_from(3999.0, "full");
		}
		if k.name == "Gain" {
			knob = knob.wheel_step(Some(0.5));
		}
		let (g, _) = knob.show(&mut child);
		if let Gesture::Commit(value) = g {
			let apply = k.apply;
			let cmd = sm::edit_pad(state.project(), cx.r, note, |z| apply(z, value));
			state.apply_sample_edit(cmd);
		}
	}
}

// --- Compact (inspector) --------------------------------------------------------------

const MINI_PAD_H: f32 = 44.0;
const TITLE_H: f32 = 24.0;

fn compact_pads_h(width: f32) -> f32 {
	let h = ((width - 3.0 * PAD_GAP) / 4.0 * 0.62).clamp(36.0, MINI_PAD_H + 10.0);
	4.0 * h + 3.0 * PAD_GAP
}

/// How tall the compact form is at `width`.
pub fn compact_height(width: f32) -> f32 {
	compact_pads_h(width) + GAP + TITLE_H + devices::small_rows_height(width, 5) + 4.0
}

#[allow(clippy::too_many_arguments)]
fn compact(
	ui: &mut Ui,
	state: &mut AppState,
	cx: &BodyCx,
	v: &mut SamplerView,
	hits: &mut PadHits,
	d: &Device,
	rect: Rect,
	now: f64,
) {
	let grid = Rect::from_min_size(rect.min, vec2(rect.width(), compact_pads_h(rect.width())));
	pads(ui, state, cx, v, hits, d, grid, now, true);
	hits.kit = Some(grid);
	let mut y = grid.bottom() + GAP;
	let note = state.view.instrument.pad;
	let c = pad_color(note);
	let layers = pad_layers(d, note);
	let p = ui.painter();
	p.rect_filled(
		Rect::from_center_size(pos2(rect.left() + 5.0, y + TITLE_H * 0.5), vec2(8.0, 8.0)),
		2.0,
		c,
	);
	let name = p.text(
		pos2(rect.left() + 16.0, y + TITLE_H * 0.5),
		Align2::LEFT_CENTER,
		sm::pad_label(note),
		theme::semibold(12.5),
		theme::TEXT,
	);
	let note_r = p.text(
		pos2(name.right() + 8.0, y + TITLE_H * 0.5),
		Align2::LEFT_CENTER,
		ds::note_label(note),
		theme::mono(10.5),
		theme::TEXT_FAINT,
	);
	let file = layers
		.first()
		.map_or_else(|| "empty".to_owned(), |&i| d.zones[i].sample.display_name());
	let file = crate::ui::mixer::elide(
		ui,
		&file,
		&theme::mono(10.5),
		rect.right() - note_r.right() - 14.0,
	);
	ui.painter().text(
		pos2(note_r.right() + 10.0, y + TITLE_H * 0.5),
		Align2::LEFT_CENTER,
		file,
		theme::mono(10.5),
		theme::TEXT_DIM,
	);
	y += TITLE_H;
	drop_overlay(ui, hits);
	let Some(&first) = layers.first() else {
		return;
	};
	let zone = d.zones[first].clone();
	let cols = devices::small_columns(rect.width(), 5);
	let k = Rect::from_min_max(
		pos2(rect.left(), y),
		pos2(
			rect.right(),
			y + devices::small_rows_height(rect.width(), 5),
		),
	);
	pad_knobs(ui, state, cx, note, &zone, k, cols, c);
}
