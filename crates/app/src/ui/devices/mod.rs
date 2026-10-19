//! Dedicated device editor bodies (one module per built-in).

pub mod chorus;
pub mod compressor;
pub mod delay;
pub mod drum_machine;
pub mod drum_sampler;
pub mod electric_piano;
pub mod eq;
pub mod gain;
pub mod keyboard;
pub mod limiter;
pub mod poly_synth;
pub mod reverb;
pub mod sampler;
pub mod saturator;

use cadenza_dsp::DeviceInfo;
use egui::{Align, Color32, Id, Layout, Rect, Stroke, Ui, UiBuilder, pos2, vec2};

use crate::state::AppState;
use crate::state::selection::DeviceRef;
use crate::theme;
use crate::ui::device_editor::BodyCx;
use crate::ui::device_kit::{KnobSize, ParamKnob};

/// The footer strip under a dock body.
pub const FOOTER_H: f32 = 32.0;

/// The value shown for `id` now (preview, automation, document, else the default).
pub fn shown(state: &AppState, r: DeviceRef, info: &DeviceInfo, id: &str) -> f32 {
	state
		.param_value(r.track, r.device, id)
		.or_else(|| info.param(id).map(|p| p.default))
		.unwrap_or(0.0)
}

/// The value stored in the document for `id` (else the default).
pub fn stored(state: &AppState, r: DeviceRef, info: &DeviceInfo, id: &str) -> f32 {
	state
		.project()
		.device(r.track, r.device)
		.and_then(|d| d.params.get(id).copied())
		.or_else(|| info.param(id).map(|p| p.default))
		.unwrap_or(0.0)
}

fn differs(a: f32, b: f32) -> bool {
	(a - b).abs() > 1e-4 * a.abs().max(1.0)
}

/// The `changes` that differ from the document as ONE undo step named `label` (the
/// others only drop their preview). Returns the ids that changed.
pub fn commit_changes(
	state: &mut AppState,
	r: DeviceRef,
	info: &DeviceInfo,
	changes: &[(&'static str, f32)],
	label: impl FnOnce(&[&str]) -> String,
) -> Vec<&'static str> {
	let real: Vec<(&'static str, f32)> = changes
		.iter()
		.copied()
		.filter(|&(id, v)| differs(stored(state, r, info, id), v))
		.collect();
	for (id, _) in changes {
		if !real.iter().any(|(c, _)| c == id) {
			state.drop_param_preview(r.device, id);
		}
	}
	let ids: Vec<&'static str> = real.iter().map(|(id, _)| *id).collect();
	if !real.is_empty() {
		let label = label(&ids);
		let values: Vec<(&str, f32)> = real.iter().map(|&(id, v)| (id, v)).collect();
		state.commit_params(r.track, r.device, &values, &label);
	}
	ids
}

/// One knob of a row or grid.
#[derive(Debug, Clone, Copy)]
pub struct KnobDef {
	pub id: &'static str,
	pub label: &'static str,
	pub size: KnobSize,
	pub color: Color32,
	/// Plain units per wheel notch (0.5 dB), else 1/100 of the travel.
	pub step: Option<f32>,
}

impl KnobDef {
	pub const fn new(id: &'static str, label: &'static str, size: KnobSize) -> Self {
		Self {
			id,
			label,
			size,
			color: theme::TEXT,
			step: None,
		}
	}

	pub const fn color(mut self, color: Color32) -> Self {
		self.color = color;
		self
	}

	pub const fn step(mut self, step: f32) -> Self {
		self.step = Some(step);
		self
	}

	pub const fn sized(mut self, size: KnobSize) -> Self {
		self.size = size;
		self
	}
}

/// The height a kit knob takes with its label and readout.
pub fn knob_height(size: KnobSize) -> f32 {
	let label = if size == KnobSize::S { 15.0 } else { 16.0 };
	4.0 + size.diameter() + 4.0 + label + 18.0
}

fn knob_in(ui: &mut Ui, state: &mut AppState, cx: &BodyCx, cell: Rect, k: &KnobDef) {
	let h = knob_height(k.size);
	let top = cell.center().y - h * 0.5;
	let rect = Rect::from_min_max(pos2(cell.left(), top), pos2(cell.right(), top + h));
	let mut child = ui.new_child(
		UiBuilder::new()
			.max_rect(rect)
			.layout(Layout::top_down(Align::Center)),
	);
	let mut knob = ParamKnob::new(cx.r, k.id)
		.size(k.size)
		.label(k.label)
		.color(k.color)
		.id_salt(Id::new(cx.salt));
	if let Some(step) = k.step {
		knob = knob.wheel_step(step);
	}
	knob.show(&mut child, state);
}

/// `knobs` in a grid of `cols` columns filling `rect`, each centred in its cell.
pub fn knob_grid(
	ui: &mut Ui,
	state: &mut AppState,
	cx: &BodyCx,
	rect: Rect,
	knobs: &[KnobDef],
	cols: usize,
) {
	let cols = cols.clamp(1, knobs.len().max(1));
	let rows = knobs.len().div_ceil(cols).max(1);
	let cell = vec2(rect.width() / cols as f32, rect.height() / rows as f32);
	for (i, k) in knobs.iter().enumerate() {
		let at = rect.min + vec2((i % cols) as f32 * cell.x, (i / cols) as f32 * cell.y);
		knob_in(ui, state, cx, Rect::from_min_size(at, cell), k);
	}
}

/// Columns of S knobs that fit `width` (a 56 px knob plus breathing room), balanced so
/// the rows hold about as many each (6 in two rows = 3 + 3).
pub fn small_columns(width: f32, count: usize) -> usize {
	let count = count.max(1);
	let max = ((width / 60.0).floor() as usize).clamp(1, count);
	count.div_ceil(count.div_ceil(max))
}

/// The height of `count` S knobs laid out by [`small_columns`] at `width`.
pub fn small_rows_height(width: f32, count: usize) -> f32 {
	let rows = count.div_ceil(small_columns(width, count));
	rows as f32 * (knob_height(KnobSize::S) + 4.0)
}

/// The footer strip at the bottom of a dock body with a one-line note.
pub fn footer(ui: &mut Ui, body: Rect, text: &str) {
	let foot = Rect::from_min_size(
		pos2(body.left() - 8.0, body.bottom() - FOOTER_H),
		vec2(body.width() + 16.0, FOOTER_H),
	);
	ui.painter().hline(
		foot.x_range(),
		foot.top(),
		Stroke::new(1.0, theme::BORDER_SUBTLE),
	);
	let mut bar = ui.new_child(
		UiBuilder::new()
			.max_rect(foot.shrink2(vec2(10.0, 0.0)))
			.layout(Layout::left_to_right(Align::Center)),
	);
	bar.add(
		egui::Label::new(
			egui::RichText::new(text)
				.font(theme::medium(12.0))
				.color(theme::TEXT_DIM),
		)
		.truncate(),
	);
}

/// The swatch + label legend entries left to right from `left_center`; returns the x
/// after the last one.
pub fn legend(p: &egui::Painter, left_center: egui::Pos2, items: &[(&str, Color32)]) -> f32 {
	let mut x = left_center.x;
	for (label, color) in items {
		p.rect_filled(
			Rect::from_center_size(pos2(x + 5.0, left_center.y), vec2(10.0, 2.0)),
			0.0,
			*color,
		);
		let r = p.text(
			pos2(x + 15.0, left_center.y),
			egui::Align2::LEFT_CENTER,
			label,
			theme::medium(10.5),
			theme::TEXT_DIM,
		);
		x = r.right() + 14.0;
	}
	x
}

/// Runs a compact form of `height` in the body: as is when it fits, else inside a
/// vertical scroll area (a short dock).
pub fn compact_scroll(ui: &mut Ui, cx: &BodyCx, height: f32, add: impl FnOnce(&mut Ui, Rect)) {
	if height <= cx.rect.height() + 0.5 {
		add(ui, cx.rect);
		return;
	}
	let mut inner = ui.new_child(
		UiBuilder::new()
			.max_rect(cx.rect)
			.layout(Layout::top_down(Align::Min)),
	);
	egui::ScrollArea::vertical()
		.id_salt(("device-compact", cx.salt, cx.r.device))
		.auto_shrink([false, false])
		.show(&mut inner, |ui| {
			let (rect, _) =
				ui.allocate_exact_size(vec2(ui.available_width(), height), egui::Sense::hover());
			add(ui, rect);
		});
}
