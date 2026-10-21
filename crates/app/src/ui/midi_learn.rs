//! MIDI learn in the views (T36): the control menu items, the pulsing ring of a control
//! waiting for a controller, the "CC 74" badge of a mapped control and the "MIDI
//! mappings" window. Logic in [`crate::state::midi_learn`].

use cadenza_core::ParamAddress;
use cadenza_model::{MapMode, MidiMapping, Takeover};
use egui::{Align2, Color32, CornerRadius, Id, Rect, RichText, Stroke, StrokeKind, Ui, pos2, vec2};

use crate::i18n::{tr, trf};
use crate::state::AppState;
use crate::state::automation;
use crate::state::midi_learn::{mappable, mapping_for};
use crate::theme;

/// "MIDI learn" / "Cancel MIDI learn" / "Forget MIDI (CC 74)" for a control's menu.
pub fn menu_items(ui: &mut Ui, state: &mut AppState, address: &ParamAddress) {
	if !mappable(state.project(), address) {
		return;
	}
	if state.is_learning(address) {
		if ui.button(tr("Cancel MIDI learn")).clicked() {
			state.cancel_midi_learn();
			ui.close();
		}
	} else if ui.button(tr("MIDI learn")).clicked() {
		state.start_midi_learn(address.clone());
		ui.close();
	}
	if let Some(tag) = mapping_for(state.project(), address).map(MidiMapping::tag)
		&& ui
			.button(trf("Forget MIDI ({tag})", &[("tag", &tag)]))
			.clicked()
	{
		state.forget_midi(address);
		ui.close();
	}
}

/// The learn ring (pulsing while the control waits for a controller) and the mapped
/// badge, drawn over a control.
pub fn decorate(response: &egui::Response, state: &AppState, address: &ParamAddress) {
	let ctx = &response.ctx;
	let rect = response.rect;
	let p = ctx.layer_painter(response.layer_id);
	if state.is_learning(address) {
		let t = ctx.input(|i| i.time) as f32;
		let pulse = 0.5 + 0.5 * (t * std::f32::consts::TAU * 1.2).sin();
		let alpha = (90.0 + 165.0 * pulse) as u8;
		let c = Color32::from_rgba_unmultiplied(
			theme::LEARN.r(),
			theme::LEARN.g(),
			theme::LEARN.b(),
			alpha,
		);
		p.rect_stroke(
			rect.expand(3.0 + 2.0 * pulse),
			CornerRadius::same(theme::RADIUS_MD + 3),
			Stroke::new(2.0, c),
			StrokeKind::Outside,
		);
		ctx.request_repaint();
	}
	if let Some(m) = mapping_for(state.project(), address) {
		badge(&p, rect, &m.tag());
	}
}

/// The small "CC 74" tag at the bottom-left corner of a mapped control.
fn badge(p: &egui::Painter, rect: Rect, text: &str) {
	let galley = p.layout_no_wrap(text.to_owned(), theme::mono_medium(8.5), theme::LEARN);
	let size = galley.size() + vec2(6.0, 1.0);
	let r = Rect::from_min_size(pos2(rect.left() - 2.0, rect.bottom() - size.y + 2.0), size);
	p.rect(
		r,
		CornerRadius::same(theme::RADIUS),
		theme::LEARN_SOFT,
		Stroke::new(1.0, theme::LEARN),
		StrokeKind::Inside,
	);
	p.galley(r.center() - galley.size() * 0.5, galley, theme::LEARN);
}

/// Per frame: Esc cancels a learn, a hint says what to do, and the mappings window.
pub fn show(ctx: &egui::Context, state: &mut AppState) {
	if state.midi_learning().is_some() {
		if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
			state.cancel_midi_learn();
		} else {
			learn_hint(ctx, state);
		}
	}
	if state.midi.window_open {
		window(ctx, state);
	}
}

fn learn_hint(ctx: &egui::Context, state: &AppState) {
	let Some(learning) = state.midi_learning() else {
		return;
	};
	let name = automation::meta(state.project(), None, &learning.target)
		.map_or_else(|| learning.target.to_string(), |m| m.name);
	egui::Area::new(Id::new("midi-learn-hint"))
		.anchor(Align2::CENTER_BOTTOM, vec2(0.0, -40.0))
		.order(egui::Order::Foreground)
		.interactable(false)
		.show(ctx, |ui| {
			egui::Frame::new()
				.fill(theme::BG_RAISED)
				.stroke(Stroke::new(1.0, theme::LEARN))
				.corner_radius(theme::RADIUS_MD)
				.inner_margin(egui::Margin::symmetric(12, 6))
				.show(ui, |ui| {
					ui.label(
						RichText::new(trf(
							"MIDI learn: move a knob or fader on your controller for {name} (Esc cancels)",
							&[("name", &name)],
						))
						.color(theme::TEXT),
					);
				});
		});
}

/// Where a mapping's target lives ("Keys · Synth: Cutoff").
fn target_name(state: &AppState, m: &MidiMapping) -> String {
	let p = state.project();
	let what =
		automation::meta(p, None, &m.target).map_or_else(|| m.target.to_string(), |x| x.name);
	let on = match m.target.track.and_then(|t| p.track(t)) {
		Some(t) => t.name.clone(),
		None => tr("Master").to_owned(),
	};
	format!("{on} · {what}")
}

/// The "MIDI mappings" window: every assignment with its range, mode, takeover and a
/// delete button; each edit is one `SetMidiMap` step (drags commit on release).
fn window(ctx: &egui::Context, state: &mut AppState) {
	let mut open = true;
	let draft_id = Id::new("midi-map-draft");
	// (map the draft was made from, draft): an outside change (undo, AI) resets it.
	let current = state.project().midi_map.clone();
	let mut draft = match ctx.data(|d| d.get_temp::<(Vec<MidiMapping>, Vec<MidiMapping>)>(draft_id))
	{
		Some((base, draft)) if base == current => draft,
		_ => current.clone(),
	};
	let mut commit = false;
	let mut remove = None;
	egui::Window::new(tr("MIDI mappings"))
		.id(Id::new("midi-mappings"))
		.open(&mut open)
		.frame(theme::dialog_frame())
		.default_width(640.0)
		.default_pos(ctx.content_rect().center() - vec2(320.0, 140.0))
		.resizable(false)
		.collapsible(false)
		.show(ctx, |ui| {
			if draft.is_empty() {
				ui.label(
					RichText::new(tr(
						"No MIDI assignments yet. Right-click a knob, fader or pan and choose MIDI learn, then move a control on your MIDI controller.",
					))
					.color(theme::TEXT_DIM),
				);
				return;
			}
			egui::Grid::new("midi-map-grid")
				.num_columns(7)
				.spacing([10.0, 6.0])
				.striped(true)
				.show(ui, |ui| {
					for h in [
						tr("Control"),
						tr("Target"),
						tr("Min"),
						tr("Max"),
						tr("Mode"),
						tr("Takeover"),
						"",
					] {
						ui.label(RichText::new(h).font(theme::small()).color(theme::TEXT_FAINT));
					}
					ui.end_row();
					for (i, m) in draft.iter_mut().enumerate() {
						ui.label(RichText::new(m.tag()).font(theme::mono_medium(11.0)).color(theme::LEARN));
						ui.label(target_name(state, m));
						let meta = automation::meta(state.project(), None, &m.target);
						let (lo, hi) = meta.as_ref().map_or((m.min.min(m.max), m.min.max(m.max)), |x| {
							(x.min, x.max)
						});
						let speed = (hi - lo) / 300.0;
						for v in [&mut m.min, &mut m.max] {
							let r = ui.add(
								egui::DragValue::new(v)
									.range(lo..=hi)
									.speed(speed)
									.max_decimals(2),
							);
							if r.drag_stopped() || (r.changed() && !r.dragged()) {
								commit = true;
							}
						}
						let relative_ok = matches!(m.controller, cadenza_model::Controller::Cc(_));
						egui::ComboBox::from_id_salt(("midi-map-mode", i))
							.selected_text(match m.mode {
								MapMode::Absolute => tr("Absolute"),
								MapMode::Relative => tr("Relative"),
							})
							.show_ui(ui, |ui| {
								commit |= ui
									.selectable_value(&mut m.mode, MapMode::Absolute, tr("Absolute"))
									.clicked();
								if relative_ok {
									commit |= ui
										.selectable_value(&mut m.mode, MapMode::Relative, tr("Relative"))
										.on_hover_text(tr("Endless encoders (two's complement steps)"))
										.clicked();
								}
							});
						egui::ComboBox::from_id_salt(("midi-map-takeover", i))
							.selected_text(match m.takeover {
								Takeover::Jump => tr("Jump"),
								Takeover::Pickup => tr("Pickup"),
							})
							.show_ui(ui, |ui| {
								commit |= ui
									.selectable_value(&mut m.takeover, Takeover::Jump, tr("Jump"))
									.clicked();
								commit |= ui
									.selectable_value(&mut m.takeover, Takeover::Pickup, tr("Pickup"))
									.on_hover_text(tr("Nothing moves until the control passes the current value"))
									.clicked();
							});
						if ui
							.add(egui::Button::new("×").frame(false))
							.on_hover_text(tr("Forget this assignment"))
							.clicked()
						{
							remove = Some(i);
						}
						ui.end_row();
					}
				});
		});
	if let Some(i) = remove {
		draft.remove(i);
		commit = true;
	}
	if commit {
		state.set_midi_map(draft.clone());
		draft = state.project().midi_map.clone();
	}
	let base = state.project().midi_map.clone();
	ctx.data_mut(|d| d.insert_temp(draft_id, (base, draft)));
	if !open {
		state.midi.window_open = false;
		ctx.data_mut(|d| d.remove::<(Vec<MidiMapping>, Vec<MidiMapping>)>(draft_id));
	}
}
