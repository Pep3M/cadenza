//! The MOD section of a built-in device's editor (LFOs and envelope followers) and the
//! glue that lets every kit knob take modulation: drop a modulator's handle on a knob
//! (or right-click it → "Modulate with ▸"), drag the teal ring to set the depth,
//! Alt+click the ring to remove it. Every gesture is one `SetModulators` step
//! (`state::modulation` builds them).

use cadenza_core::{DeviceId, ModulatorId};
use cadenza_dsp::{DeviceInfo, ParamInfo};
use cadenza_model::modulation::{
	ATTACK_MS_RANGE, GAIN_DB_RANGE, MAX_MODULATORS, RATE_HZ_RANGE, RELEASE_MS_RANGE, SYNC_STEPS,
};
use cadenza_model::{Device, EnvelopeSource, LfoShape, ModulatorKind, Retrigger};
use egui::{
	Align2, CornerRadius, Id, Pos2, Rect, Response, Sense, Shape, Stroke, StrokeKind, Ui,
	UiBuilder, pos2, vec2,
};

use crate::i18n::{mark, tr, tr_str, trf};
use crate::state::AppState;
use crate::state::device::KnobSpec;
use crate::state::modulation as m;
use crate::state::selection::DeviceRef;
use crate::theme;
use crate::ui::device_kit::{Knob, KnobSize, ModRingPaint, segmented, toggle};
use crate::ui::widgets::Gesture;

/// Height of the collapsed section (its header strip).
pub const HEADER_H: f32 = 24.0;
/// Height of the open section.
pub const OPEN_H: f32 = 176.0;
const CARD_W: f32 = 318.0;

/// A modulator being dragged onto a knob.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModDrag {
	pub device: DeviceId,
	pub index: usize,
}

fn open_id(device: DeviceId) -> Id {
	Id::new(("mod-section-open", device))
}

fn selected_id(device: DeviceId) -> Id {
	Id::new(("mod-selected", device))
}

/// The modulator a ring drag acts on (the card clicked last).
pub fn selected(ctx: &egui::Context, device: DeviceId) -> usize {
	ctx.data(|d| d.get_temp(selected_id(device))).unwrap_or(0)
}

fn select(ctx: &egui::Context, device: DeviceId, index: usize) {
	ctx.data_mut(|d| d.insert_temp(selected_id(device), index));
}

/// Whether the section is open: the user's choice, else open when the device has
/// modulators.
pub fn is_open(ctx: &egui::Context, device: &Device) -> bool {
	ctx.data(|d| d.get_temp(open_id(device.id)))
		.unwrap_or(!device.modulators.is_empty())
}

/// The section's height for `device` in a body of `height`.
pub fn height(ctx: &egui::Context, device: &Device, height: f32) -> f32 {
	if height < OPEN_H + 240.0 || !is_open(ctx, device) {
		HEADER_H
	} else {
		OPEN_H
	}
}

/// "LFO 2" / "Env 1", translated.
pub fn label(device: &Device, index: usize) -> String {
	let text = m::label(device, index);
	match text.split_once(' ') {
		Some(("LFO", n)) => trf("LFO {n}", &[("n", &n)]),
		Some((_, n)) => trf("Env {n}", &[("n", &n)]),
		None => text,
	}
}

// --- Knob glue ------------------------------------------------------------------------

/// A ring drag in progress: (modulator, depth at the start, depth now).
type RingDrag = (usize, f32, f32);

fn ring_drag_id(device: DeviceId, param: &str) -> Id {
	Id::new(("mod-ring-drag", device, param))
}

/// The modulation ring of `param` (`base` = the value now without modulation), with a
/// ring drag's preview depth applied. `None` when nothing modulates it and no modulator
/// is dragged over the knob (`over`: its rect last frame).
pub fn knob_ring(
	ctx: &egui::Context,
	state: &AppState,
	r: DeviceRef,
	info: &ParamInfo,
	base: f32,
	over: Option<Rect>,
) -> Option<ModRingPaint> {
	let device = state.device(r)?;
	let mut rings = m::rings(device, info.id);
	let drag: Option<RingDrag> = ctx.data(|d| d.get_temp(ring_drag_id(r.device, info.id)));
	if let Some((index, _, depth)) = drag {
		match rings.iter_mut().find(|x| x.modulator == index) {
			Some(x) => x.depth = depth,
			None => rings.push(m::Ring {
				modulator: index,
				depth,
				bipolar: device.modulators.get(index).is_some_and(|x| x.is_lfo()),
			}),
		}
	}
	let hot = egui::DragAndDrop::payload::<ModDrag>(ctx).is_some_and(|p| p.device == r.device)
		&& over.is_some_and(|rect| ctx.pointer_latest_pos().is_some_and(|p| rect.contains(p)));
	if rings.is_empty() && !hot {
		return None;
	}
	let (lo, hi) = m::extent(base, &rings, info.min, info.max);
	let now = state
		.modulation_values(r.device)
		.filter(|_| !rings.is_empty())
		.map(|values| m::live_value(base, &rings, &values, info.min, info.max));
	if now.is_some() {
		ctx.request_repaint();
	}
	Some(ModRingPaint { lo, hi, now, hot })
}

/// Acts on a knob's modulation gestures: a modulator dropped on the dial, the ring
/// dragged (depth of the selected modulator, else the first one on it) or Alt+clicked
/// (removes it).
pub fn knob_gestures(
	state: &mut AppState,
	r: DeviceRef,
	param: &str,
	dial: &Response,
	ring: Option<&Response>,
) {
	if let Some(p) = dial.dnd_release_payload::<ModDrag>()
		&& p.device == r.device
		&& let Some(cmd) = state
			.device(r)
			.and_then(|d| m::assign(d, r, p.index, param))
	{
		select(&dial.ctx, r.device, p.index);
		state.apply(cmd);
	}
	let Some(ring) = ring else {
		return;
	};
	let ctx = ring.ctx.clone();
	let Some(device) = state.device(r).cloned() else {
		return;
	};
	let rings = m::rings(&device, param);
	let chosen = selected(&ctx, r.device);
	let target = rings
		.iter()
		.find(|x| x.modulator == chosen)
		.or(rings.first())
		.copied();
	let id = ring_drag_id(r.device, param);
	if ring.clicked() && ctx.input(|i| i.modifiers.alt) {
		if let Some(t) = target
			&& let Some(cmd) = m::set_depth(&device, r, t.modulator, param, 0.0)
		{
			state.apply(cmd);
		}
		return;
	}
	if ring.drag_started()
		&& let Some(t) = target
	{
		ctx.data_mut(|d| d.insert_temp::<RingDrag>(id, (t.modulator, t.depth, t.depth)));
	}
	if ring.dragged()
		&& let Some((index, start, _)) = ctx.data(|d| d.get_temp::<RingDrag>(id))
	{
		let dy = ring.total_drag_delta().map_or(0.0, |d| d.y);
		let depth = m::drag_depth(start, dy);
		ctx.data_mut(|d| d.insert_temp::<RingDrag>(id, (index, start, depth)));
		crate::ui::device_kit::floating_value(
			&ctx,
			ring.id,
			ring.rect,
			&format!("{} {:+.0}%", label(&device, index), depth * 100.0),
		);
	}
	if ring.drag_stopped()
		&& let Some((index, _, depth)) = ctx.data_mut(|d| d.remove_temp::<RingDrag>(id))
		&& let Some(cmd) = m::set_depth(&device, r, index, param, depth)
	{
		state.apply(cmd);
	}
}

/// "Modulate with ▸" in a knob's context menu: toggles each modulator's target on the
/// param, or adds a new LFO / envelope already targeting it.
pub fn modulate_menu(ui: &mut Ui, state: &mut AppState, r: DeviceRef, param: &str) {
	let Some(device) = state.device(r).cloned() else {
		return;
	};
	ui.menu_button(tr("Modulate with"), |ui| {
		for i in 0..device.modulators.len() {
			let on = device.modulators[i]
				.targets
				.iter()
				.any(|t| t.param == param);
			if ui.checkbox(&mut on.clone(), label(&device, i)).clicked() {
				let cmd = if on {
					m::set_depth(&device, r, i, param, 0.0)
				} else {
					m::assign(&device, r, i, param)
				};
				if let Some(cmd) = cmd {
					select(ui.ctx(), r.device, i);
					state.apply(cmd);
				}
				ui.close();
			}
		}
		if !device.modulators.is_empty() {
			ui.separator();
		}
		let full = device.modulators.len() >= MAX_MODULATORS;
		for (lfo, text) in [(true, tr("New LFO")), (false, tr("New envelope"))] {
			if ui.add_enabled(!full, egui::Button::new(text)).clicked() {
				let mut d = device.clone();
				if let Some(cadenza_model::Command::SetModulators { modulators, .. }) =
					m::add(&d, r, lfo, ModulatorId::new())
				{
					d.modulators = modulators;
					let index = d.modulators.len() - 1;
					if let Some(cmd) = m::assign(&d, r, index, param) {
						select(ui.ctx(), r.device, index);
						ui.ctx()
							.data_mut(|x| x.insert_temp(open_id(r.device), true));
						state.apply(cmd);
					}
				}
				ui.close();
			}
		}
	});
}

// --- The section -----------------------------------------------------------------------

/// Draws the MOD section of `r` into `rect` (see [`height`]).
pub fn section(ui: &mut Ui, state: &mut AppState, r: DeviceRef, info: &DeviceInfo, rect: Rect) {
	let Some(device) = state.device(r).cloned() else {
		return;
	};
	let ctx = ui.ctx().clone();
	let p = ui.painter().clone();
	p.rect_filled(
		rect,
		CornerRadius::same(theme::RADIUS_MD),
		theme::DISPLAY_BG,
	);
	let header = Rect::from_min_size(rect.min, vec2(rect.width(), HEADER_H));
	let mut hui = ui.new_child(
		UiBuilder::new()
			.max_rect(header.shrink2(vec2(8.0, 2.0)))
			.layout(egui::Layout::left_to_right(egui::Align::Center)),
	);
	let open = is_open(&ctx, &device);
	let chevron = if open { "▾" } else { "▸" };
	let title = format!("{chevron} {}", tr("MOD"));
	let title = hui.add(
		egui::Label::new(
			egui::RichText::new(title)
				.font(theme::semibold(11.5))
				.color(theme::MOD),
		)
		.sense(Sense::click()),
	);
	if title.clicked() {
		ctx.data_mut(|d| d.insert_temp(open_id(r.device), !open));
	}
	if !device.modulators.is_empty() {
		hui.label(
			egui::RichText::new(device.modulators.len().to_string())
				.font(theme::mono(10.5))
				.color(theme::TEXT_DIM),
		);
	}
	let full = device.modulators.len() >= MAX_MODULATORS;
	for (lfo, text) in [(true, tr("+ LFO")), (false, tr("+ Envelope"))] {
		if hui
			.add_enabled(
				!full,
				egui::Button::new(egui::RichText::new(text).font(theme::small())),
			)
			.clicked()
			&& let Some(cmd) = m::add(&device, r, lfo, ModulatorId::new())
		{
			select(&ctx, r.device, device.modulators.len());
			ctx.data_mut(|d| d.insert_temp(open_id(r.device), true));
			state.apply(cmd);
		}
	}
	let dragging = egui::DragAndDrop::has_payload_of_type::<ModDrag>(&ctx);
	let hint = if dragging {
		tr("Drop on a knob to modulate it")
	} else if device.modulators.is_empty() {
		tr("Add a source, then drag its handle onto any knob")
	} else {
		tr("Drag a handle onto a knob · drag the teal ring for depth · Alt+click removes")
	};
	p.text(
		pos2(header.right() - 10.0, header.center().y),
		Align2::RIGHT_CENTER,
		hint,
		theme::small(),
		if dragging {
			theme::MOD_TEXT
		} else {
			theme::TEXT_FAINT
		},
	);
	if rect.height() <= HEADER_H + 1.0 || !is_open(&ctx, &device) {
		return;
	}
	let body =
		Rect::from_min_max(pos2(rect.left(), header.bottom()), rect.max).shrink2(vec2(8.0, 4.0));
	let values = state.modulation_values(r.device);
	let mut sui = ui.new_child(UiBuilder::new().max_rect(body));
	egui::ScrollArea::horizontal()
		.id_salt(("mod-cards", r.device))
		.show(&mut sui, |ui| {
			ui.horizontal(|ui| {
				for i in 0..device.modulators.len() {
					let value = values.as_ref().and_then(|v| v.get(i).copied());
					card(ui, state, r, info, &device, i, value, body.height());
				}
			});
		});
}

#[allow(clippy::too_many_arguments)]
fn card(
	ui: &mut Ui,
	state: &mut AppState,
	r: DeviceRef,
	info: &DeviceInfo,
	device: &Device,
	index: usize,
	value: Option<f32>,
	height: f32,
) {
	let modulator = &device.modulators[index];
	let ctx = ui.ctx().clone();
	let chosen = selected(&ctx, r.device) == index;
	let (rect, _) = ui.allocate_exact_size(vec2(CARD_W, height), Sense::hover());
	let p = ui.painter().clone();
	p.rect(
		rect,
		CornerRadius::same(theme::RADIUS_MD),
		theme::BG_PANEL,
		Stroke::new(1.0, if chosen { theme::MOD } else { theme::BORDER }),
		StrokeKind::Inside,
	);
	let mut cui = ui.new_child(
		UiBuilder::new()
			.max_rect(rect.shrink(6.0))
			.layout(egui::Layout::top_down(egui::Align::Min)),
	);
	cui.set_clip_rect(rect.intersect(ui.clip_rect()));
	cui.spacing_mut().item_spacing = vec2(6.0, 3.0);
	// Top line: handle, live value, targets, remove.
	cui.horizontal(|ui| {
		let handle = ui
			.dnd_drag_source(
				Id::new(("mod-handle", r.device, index)),
				ModDrag {
					device: r.device,
					index,
				},
				|ui| handle_chip(ui, &label(device, index)),
			)
			.response;
		if handle.clicked() || handle.drag_started() {
			select(&ctx, r.device, index);
		}
		handle.on_hover_text(tr("Drag onto a knob to modulate it"));
		value_bar(ui, modulator.is_lfo(), value);
		ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
			if ui
				.add(egui::Button::new("×").small())
				.on_hover_text(tr("Remove"))
				.clicked() && let Some(cmd) = m::remove(device, r, index)
			{
				state.apply(cmd);
			}
		});
	});
	let set_kind = |state: &mut AppState, kind: ModulatorKind| {
		if let Some(cmd) = m::set_kind(device, r, index, kind) {
			select(&ctx, r.device, index);
			state.apply(cmd);
		}
	};
	match modulator.kind.clone() {
		ModulatorKind::Lfo {
			shape,
			rate_hz,
			sync_beats,
			phase,
			retrigger,
		} => {
			cui.horizontal(|ui| {
				ui.vertical(|ui| {
					ui.horizontal(|ui| {
						ui.spacing_mut().item_spacing.x = 2.0;
						for s in LfoShape::ALL {
							if shape_button(ui, s, s == shape).clicked() {
								let mut k = modulator.kind.clone();
								if let ModulatorKind::Lfo { shape, .. } = &mut k {
									*shape = s;
								}
								set_kind(state, k);
							}
						}
					});
					let labels = [tr("Free"), tr("Note"), tr("Song")];
					let at = match retrigger {
						Retrigger::Free => 0,
						Retrigger::Note => 1,
						Retrigger::Transport => 2,
					};
					if let Some(i) =
						segmented(ui, Id::new(("mod-retrig", r.device, index)), &labels, at)
					{
						let mut k = modulator.kind.clone();
						if let ModulatorKind::Lfo { retrigger, .. } = &mut k {
							*retrigger =
								[Retrigger::Free, Retrigger::Note, Retrigger::Transport][i];
						}
						set_kind(state, k);
					}
					ui.horizontal(|ui| {
						let synced = sync_beats.is_some();
						if toggle(ui, synced, mark("Sync"), theme::MOD).clicked() {
							let mut k = modulator.kind.clone();
							if let ModulatorKind::Lfo { sync_beats, .. } = &mut k {
								*sync_beats = if synced { None } else { Some(1.0) };
							}
							set_kind(state, k);
						}
						if let Some(beats) = sync_beats {
							let current = m::sync_index(beats)
								.map_or_else(|| format!("{beats}"), |i| SYNC_STEPS[i].0.to_owned());
							egui::ComboBox::from_id_salt(("mod-sync", r.device, index))
								.width(70.0)
								.selected_text(current)
								.show_ui(ui, |ui| {
									for (name, b) in SYNC_STEPS {
										if ui
											.selectable_label((b - beats).abs() < 1e-4, name)
											.clicked()
										{
											let mut k = modulator.kind.clone();
											if let ModulatorKind::Lfo { sync_beats, .. } = &mut k {
												*sync_beats = Some(b);
											}
											set_kind(state, k);
										}
									}
								});
						}
					});
				});
				if sync_beats.is_none() {
					let spec = KnobSpec::for_param(RATE_HZ_RANGE.0, RATE_HZ_RANGE.1, 1.0, "Hz");
					if let Some(v) = small_knob(
						ui,
						("mod-rate", r.device, index),
						rate_hz,
						spec,
						mark("Rate"),
						"Hz",
					) {
						let mut k = modulator.kind.clone();
						if let ModulatorKind::Lfo { rate_hz, .. } = &mut k {
							*rate_hz = v;
						}
						set_kind(state, k);
					}
				}
				let spec = KnobSpec::new(0.0, 360.0, 0.0);
				if let Some(v) = small_knob(
					ui,
					("mod-phase", r.device, index),
					phase * 360.0,
					spec,
					mark("Phase"),
					"°",
				) {
					let mut k = modulator.kind.clone();
					if let ModulatorKind::Lfo { phase, .. } = &mut k {
						*phase = (v / 360.0).clamp(0.0, 1.0);
					}
					set_kind(state, k);
				}
			});
		}
		ModulatorKind::Envelope {
			source,
			attack_ms,
			release_ms,
			gain_db,
		} => {
			cui.horizontal(|ui| {
				ui.vertical(|ui| {
					let has_key = device.sidechain.is_some();
					let at = usize::from(source == EnvelopeSource::Sidechain);
					let labels = [tr("Input"), tr("Key")];
					let picked = crate::ui::device_kit::segmented_with(
						ui,
						Id::new(("mod-source", r.device, index)),
						&labels,
						at,
						&[true, has_key || at == 1],
						&theme::mono(11.5),
					);
					if let Some(i) = picked {
						let mut k = modulator.kind.clone();
						if let ModulatorKind::Envelope { source, .. } = &mut k {
							*source = if i == 1 {
								EnvelopeSource::Sidechain
							} else {
								EnvelopeSource::OwnInput
							};
						}
						set_kind(state, k);
					}
					if !has_key {
						ui.label(
							egui::RichText::new(tr("Key: set a sidechain first"))
								.font(theme::small())
								.color(theme::TEXT_FAINT),
						);
					}
				});
				let knobs = [
					(
						"mod-attack",
						attack_ms,
						ATTACK_MS_RANGE,
						10.0,
						mark("Attack"),
						"ms",
					),
					(
						"mod-release",
						release_ms,
						RELEASE_MS_RANGE,
						150.0,
						mark("Release"),
						"ms",
					),
					("mod-gain", gain_db, GAIN_DB_RANGE, 0.0, mark("Gain"), "dB"),
				];
				for (k_id, v, (lo, hi), def, name, unit) in knobs {
					let spec = KnobSpec::for_param(lo, hi, def, unit);
					if let Some(v) = small_knob(ui, (k_id, r.device, index), v, spec, name, unit) {
						let mut k = modulator.kind.clone();
						if let ModulatorKind::Envelope {
							attack_ms,
							release_ms,
							gain_db,
							..
						} = &mut k
						{
							match k_id {
								"mod-attack" => *attack_ms = v,
								"mod-release" => *release_ms = v,
								_ => *gain_db = v,
							}
						}
						set_kind(state, k);
					}
				}
			});
		}
	}
	// Targets: chips with the depth; Alt+click removes one.
	cui.horizontal_wrapped(|ui| {
		ui.spacing_mut().item_spacing = vec2(4.0, 2.0);
		if modulator.targets.is_empty() {
			ui.label(
				egui::RichText::new(tr("No targets yet"))
					.font(theme::small())
					.color(theme::TEXT_FAINT),
			);
		}
		for t in &modulator.targets {
			let name = info
				.param(&t.param)
				.map_or(t.param.as_str(), |p| tr_str(p.name));
			let text = format!(" {name} {:+.0}% ", t.depth * 100.0);
			let chip = ui.add(
				egui::Label::new(
					egui::RichText::new(text)
						.font(theme::mono(10.0))
						.color(theme::MOD_TEXT)
						.background_color(theme::MOD_SOFT),
				)
				.sense(Sense::click()),
			);
			let chip = chip.on_hover_text(tr("Alt+click removes"));
			if chip.clicked() {
				if ui.input(|i| i.modifiers.alt) {
					if let Some(cmd) = m::set_depth(device, r, index, &t.param, 0.0) {
						state.apply(cmd);
					}
				} else {
					select(&ctx, r.device, index);
				}
			}
		}
	});
}

fn handle_chip(ui: &mut Ui, text: &str) {
	let galley =
		ui.painter()
			.layout_no_wrap(text.to_owned(), theme::semibold(11.0), theme::MOD_TEXT);
	let size = vec2(galley.size().x + 26.0, 18.0);
	let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
	let p = ui.painter();
	p.rect(
		rect,
		CornerRadius::same(9),
		theme::MOD_SOFT,
		Stroke::new(1.0, theme::MOD),
		StrokeKind::Inside,
	);
	p.circle_filled(pos2(rect.left() + 9.0, rect.center().y), 4.0, theme::MOD);
	p.galley(
		pos2(rect.left() + 18.0, rect.center().y - galley.size().y * 0.5),
		galley,
		theme::MOD_TEXT,
	);
}

/// The modulator's value now: a centred bar for an LFO, a fill for an envelope.
fn value_bar(ui: &mut Ui, bipolar: bool, value: Option<f32>) {
	let (rect, _) = ui.allocate_exact_size(vec2(64.0, 6.0), Sense::hover());
	let p = ui.painter();
	p.rect_filled(rect, CornerRadius::same(3), theme::BG_DEEP);
	let Some(v) = value else {
		return;
	};
	let (a, b) = if bipolar {
		let c = rect.center().x;
		let x = c + v.clamp(-1.0, 1.0) * rect.width() * 0.5;
		(c.min(x), c.max(x))
	} else {
		(rect.left(), rect.left() + v.clamp(0.0, 1.0) * rect.width())
	};
	p.rect_filled(
		Rect::from_x_y_ranges(a..=b.max(a + 1.0), rect.y_range()),
		CornerRadius::same(3),
		theme::MOD,
	);
}

fn shape_button(ui: &mut Ui, shape: LfoShape, on: bool) -> Response {
	let (rect, response) = ui.allocate_exact_size(vec2(30.0, 20.0), Sense::click());
	let p = ui.painter();
	p.rect(
		rect,
		CornerRadius::same(theme::RADIUS),
		if on { theme::MOD_SOFT } else { theme::BG_DEEP },
		Stroke::new(1.0, if on { theme::MOD } else { theme::BORDER }),
		StrokeKind::Inside,
	);
	let inner = rect.shrink2(vec2(5.0, 5.0));
	let n = 40;
	let pts: Vec<Pos2> = (0..=n)
		.map(|i| {
			let t = i as f64 / n as f64;
			let v = cadenza_engine::modulation::lfo_value(shape, t * 1.5, 3);
			pos2(
				inner.left() + t as f32 * inner.width(),
				inner.center().y - v * inner.height() * 0.5,
			)
		})
		.collect();
	p.add(Shape::line(
		pts,
		Stroke::new(1.3, if on { theme::MOD } else { theme::TEXT_DIM }),
	));
	response.on_hover_text(tr_str(match shape {
		LfoShape::Sine => mark("Sine"),
		LfoShape::Triangle => mark("Triangle"),
		LfoShape::Saw => mark("Saw"),
		LfoShape::Square => mark("Square"),
		LfoShape::Random => mark("Random"),
	}))
}

/// A small knob for a modulator setting; returns the committed value.
fn small_knob(
	ui: &mut Ui,
	id: impl std::hash::Hash + std::fmt::Debug,
	value: f32,
	spec: KnobSpec,
	label: &str,
	unit: &str,
) -> Option<f32> {
	let (gesture, _) = Knob::new(Id::new(id), value, spec)
		.size(KnobSize::S)
		.label(label)
		.unit(unit)
		.color(theme::MOD)
		.show(ui);
	match gesture {
		Gesture::Commit(v) => Some(v),
		_ => None,
	}
}

/// The EQ display's modulation marks: a teal bar through each band node over the gain
/// (vertical) or frequency (horizontal) range its modulators sweep, with the live dot.
pub fn eq_overlay(
	ui: &Ui,
	state: &AppState,
	r: DeviceRef,
	info: &DeviceInfo,
	plot: Rect,
	range: crate::state::device_display::DbRange,
) {
	use crate::state::device_display::{db_to_y, freq_to_x};
	let Some(device) = state.device(r) else {
		return;
	};
	if device.modulators.is_empty() {
		return;
	}
	let values = state.modulation_values(r.device);
	let x_of = |f: f32| plot.left() + freq_to_x(f, plot.width());
	let y_of = |db: f32| plot.top() + db_to_y(db.clamp(range.min, range.max), range, plot.height());
	let p = ui.painter_at(plot);
	let span = |id: &str| -> Option<(f32, f32, f32, Option<f32>)> {
		let param = info.param(id)?;
		let rings = m::rings(device, id);
		if rings.is_empty() {
			return None;
		}
		let base = super::shown(state, r, info, id);
		let (lo, hi) = m::extent(base, &rings, param.min, param.max);
		let now = values
			.as_ref()
			.map(|v| m::live_value(base, &rings, v, param.min, param.max));
		Some((base, lo, hi, now))
	};
	for band in crate::state::eq_editor::BANDS {
		let freq = super::shown(state, r, info, band.freq);
		let gain = band.gain.map_or(0.0, |g| super::shown(state, r, info, g));
		let (x, y) = (x_of(freq), y_of(gain));
		if let Some(g) = band.gain
			&& let Some((_, lo, hi, now)) = span(g)
		{
			p.line_segment(
				[pos2(x, y_of(lo)), pos2(x, y_of(hi))],
				Stroke::new(3.0, theme::MOD.gamma_multiply(0.8)),
			);
			if let Some(v) = now {
				p.circle(
					pos2(x, y_of(v)),
					4.0,
					theme::MOD_TEXT,
					Stroke::new(1.5, theme::DISPLAY_BG),
				);
			}
		}
		if let Some((_, lo, hi, now)) = span(band.freq) {
			p.line_segment(
				[pos2(x_of(lo), y), pos2(x_of(hi), y)],
				Stroke::new(3.0, theme::MOD.gamma_multiply(0.8)),
			);
			if let Some(v) = now {
				p.circle(
					pos2(x_of(v), y),
					4.0,
					theme::MOD_TEXT,
					Stroke::new(1.5, theme::DISPLAY_BG),
				);
			}
		}
	}
	if values.is_some() {
		ui.ctx().request_repaint();
	}
}
