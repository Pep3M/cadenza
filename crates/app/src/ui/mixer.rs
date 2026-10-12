//! Mixer: one strip per track (tracks, then buses) and the master strip, right after the
//! last strip (pinned to the right edge once the strips scroll). Inserts can be
//! reordered by dragging, dragged to another strip (Ctrl: copy), and effects from the
//! browser dropped into a slot.

use cadenza_core::TrackId;
use cadenza_dsp::DeviceRole;
use cadenza_model::{Channel, ChannelPatch, Command, Output, Send, Track, TrackKind};
use egui::{
	Align, Align2, CornerRadius, FontId, Id, Layout, Rect, RichText, Sense, Stroke, StrokeKind, Ui,
	pos2, vec2,
};

use crate::state::AppState;
use crate::state::dnd::{DropTarget, Payload};
use crate::state::selection::DeviceRef;
use crate::theme;
use crate::ui::dnd;
use crate::ui::widgets::{self, Gesture};

const INSERT_ROWS: usize = 3;
const SEND_ROWS: usize = 2;
const ROW: f32 = 18.0;

pub fn show(ui: &mut Ui, state: &mut AppState) {
	let full = ui.available_rect_before_wrap();
	ui.painter().rect_filled(full, 0.0, theme::BG_DEEP);

	let mut order: Vec<Track> = state
		.project()
		.tracks
		.iter()
		.filter(|t| t.kind != TrackKind::Bus)
		.cloned()
		.collect();
	order.extend(
		state
			.project()
			.tracks
			.iter()
			.filter(|t| t.kind == TrackKind::Bus)
			.cloned(),
	);
	// The master follows the last strip; once the strips overflow it pins to the right.
	let master_w = theme::STRIP_WIDTH + 6.0;
	let strips_w = 4.0 + order.len() as f32 * (theme::STRIP_WIDTH + 2.0) + 8.0;
	let master_left = (full.left() + strips_w).min(full.right() - master_w);
	let master_rect = Rect::from_min_max(pos2(master_left + 6.0, full.top()), full.max);
	let strips_rect = Rect::from_min_max(full.min, pos2(master_left, full.bottom()));
	// Strips fill the height; the horizontal scrollbar only appears when needed.
	let overflow = strips_w > strips_rect.width();
	let strip_h = full.height() - if overflow { 12.0 } else { 0.0 };

	let mut strips_ui = ui.new_child(egui::UiBuilder::new().max_rect(strips_rect));
	egui::ScrollArea::horizontal()
		.id_salt("mixer-strips")
		.auto_shrink([false, false])
		.show(&mut strips_ui, |ui| {
			ui.horizontal_top(|ui| {
				ui.spacing_mut().item_spacing.x = 2.0;
				ui.add_space(4.0);
				for t in &order {
					strip(ui, state, Some(t), strip_h);
				}
				if order.is_empty() {
					ui.add_space(12.0);
					ui.label(RichText::new("No tracks yet").color(theme::TEXT_FAINT));
				}
			});
		});
	let mut master_ui = ui.new_child(egui::UiBuilder::new().max_rect(master_rect));
	master_ui.painter().vline(
		master_rect.left() - 3.0,
		master_rect.top() + 4.0..=master_rect.bottom() - 4.0,
		Stroke::new(1.0, theme::BORDER_STRONG),
	);
	strip(&mut master_ui, state, None, full.height());
	ui.allocate_rect(full, Sense::hover());
}

fn strip(ui: &mut Ui, state: &mut AppState, track: Option<&Track>, height: f32) {
	let id = track.map(|t| t.id);
	let channel: Channel = match state.project().channel(id) {
		Some(c) => c.clone(),
		None => return,
	};
	let color = track.map_or(theme::ACCENT, |t| theme::track_color(t.color.as_deref()));
	let selected = match id {
		Some(t) => state.selection.track == Some(t),
		None => state.selection.master,
	};
	let (rect, bg) = ui.allocate_exact_size(vec2(theme::STRIP_WIDTH, height - 8.0), Sense::click());
	ui.painter().rect(
		rect,
		CornerRadius::same(theme::RADIUS),
		if selected {
			theme::LANE_SELECTED
		} else {
			theme::BG_PANEL
		},
		Stroke::new(
			1.0,
			if selected {
				theme::BORDER_STRONG
			} else {
				theme::BORDER
			},
		),
		StrokeKind::Inside,
	);
	if bg.clicked() {
		match id {
			Some(t) => state.selection.select_track(t),
			None => state.selection.select_master(),
		}
	}

	// Fixed regions so every strip lines up, top to bottom.
	let inner = rect.shrink2(vec2(5.0, 5.0));
	let w = inner.width();
	let mut y = inner.top();
	let mut take = |h: f32| {
		let r = Rect::from_min_size(pos2(inner.left(), y), vec2(w, h));
		y += h + 3.0;
		r
	};
	let name_r = take(18.0);
	let what_r = take(16.0);
	let inserts_label = take(10.0);
	let inserts_r = take(INSERT_ROWS as f32 * (ROW + 2.0));
	let sends_label = take(10.0);
	let sends_r = take(SEND_ROWS as f32 * 19.0 + 18.0);
	let output_r = take(20.0);
	let top_end = y;
	let mut yb = inner.bottom();
	let mut take_bottom = |h: f32| {
		let r = Rect::from_min_max(pos2(inner.left(), yb - h), pos2(inner.right(), yb));
		yb -= h + 3.0;
		r
	};
	let ms_r = take_bottom(18.0);
	let readout_r = take_bottom(16.0);
	let pan_r = Rect::from_min_size(pos2(inner.left(), top_end + 2.0), vec2(w, 16.0));
	let fader_r = Rect::from_min_max(
		pos2(inner.left(), pan_r.bottom() + 6.0),
		pos2(inner.right(), yb),
	);

	let p = ui.painter().clone();
	let name = track.map_or("Master".to_owned(), |t| t.name.clone());
	p.rect_filled(
		name_r,
		CornerRadius::same(2),
		theme::mix(color, theme::BG_DEEP, 0.2),
	);
	p.with_clip_rect(name_r).text(
		name_r.center(),
		Align2::CENTER_CENTER,
		&name,
		FontId::proportional(11.0),
		theme::text_on(color),
	);

	let what = match track.map(|t| t.kind) {
		Some(TrackKind::Instrument) => track
			.and_then(|t| t.instrument.as_ref())
			.map_or("No instrument".to_owned(), |d| d.name.clone()),
		Some(TrackKind::Audio) => "Audio".into(),
		Some(TrackKind::Bus) => "Bus".into(),
		None => "Main out".into(),
	};
	let wr = ui.interact(what_r, Id::new(("strip-what", id)), Sense::click());
	p.rect_filled(
		what_r,
		CornerRadius::same(2),
		if wr.hovered() {
			theme::BG_WIDGET
		} else {
			theme::BG_RAISED
		},
	);
	p.with_clip_rect(what_r).text(
		what_r.center(),
		Align2::CENTER_CENTER,
		&what,
		theme::small(),
		theme::TEXT_DIM,
	);
	if wr.clicked()
		&& let Some(d) = track.and_then(|t| t.instrument.as_ref())
	{
		state.selection.select_device(DeviceRef {
			track: id,
			device: d.id,
		});
	}
	if let Some(d) = track.and_then(|t| t.instrument.as_ref())
		&& state.is_plugin(d.id)
	{
		if state.plugin_editor_open(d.id) {
			p.circle_filled(
				pos2(what_r.right() - 5.0, what_r.center().y),
				2.5,
				theme::ACCENT,
			);
		}
		let wr = wr.on_hover_text("Double-click: plugin editor");
		if wr.double_clicked() {
			state.toggle_plugin_editor(d.id);
		}
	} else if let Some(d) = track.and_then(|t| t.instrument.as_ref()) {
		let r = DeviceRef {
			track: id,
			device: d.id,
		};
		if state.is_sample_device(r) {
			let wr = wr.on_hover_text("Double-click: pads / zones editor");
			if wr.double_clicked() {
				state.toggle_instrument_editor(r);
			}
		}
	}

	section_label(&p, inserts_label, "INSERTS");
	p.rect_filled(inserts_r, CornerRadius::same(2), theme::BG_DEEP);
	let mut rows = ui.new_child(
		egui::UiBuilder::new()
			.max_rect(inserts_r.shrink(1.0))
			.layout(Layout::top_down(Align::Min)),
	);
	let mut row_rects = Vec::with_capacity(channel.inserts.len());
	egui::ScrollArea::vertical()
		.id_salt(("inserts", id))
		.auto_shrink([false, false])
		.show(&mut rows, |ui| {
			ui.spacing_mut().item_spacing = vec2(2.0, 2.0);
			for d in &channel.inserts {
				row_rects.push(insert_row(ui, state, id, d, w - 2.0));
			}
			insert_menu(ui, state, id, w - 2.0);
		});
	drop_zone(ui, state, id, rect, inserts_r, &row_rects);

	if let Some(t) = track {
		section_label(&p, sends_label, "SENDS");
		let mut sends = ui.new_child(
			egui::UiBuilder::new()
				.max_rect(sends_r)
				.layout(Layout::top_down(Align::Min)),
		);
		egui::ScrollArea::vertical()
			.id_salt(("sends", id))
			.auto_shrink([false, false])
			.show(&mut sends, |ui| {
				ui.spacing_mut().item_spacing = vec2(2.0, 3.0);
				for s in &channel.sends {
					send_row(ui, state, t.id, s, w);
				}
				let buses: Vec<(TrackId, String)> = state
					.project()
					.tracks
					.iter()
					.filter(|b| {
						b.kind == TrackKind::Bus
							&& b.id != t.id && !channel.sends.iter().any(|s| s.target == b.id)
					})
					.map(|b| (b.id, b.name.clone()))
					.collect();
				if !buses.is_empty() {
					ui.menu_button(
						RichText::new("+ Send")
							.font(theme::small())
							.color(theme::TEXT_DIM),
						|ui| {
							for (bus, name) in buses {
								if ui.button(name).clicked() {
									state.apply(Command::AddSend {
										track: t.id,
										send: Send::new(bus, -12.0),
										index: None,
									});
									ui.close();
								}
							}
						},
					);
				}
			});
		let mut out = ui.new_child(egui::UiBuilder::new().max_rect(output_r));
		output_selector(&mut out, state, t, &channel, w);
	}

	let mut pan_ui = ui.new_child(egui::UiBuilder::new().max_rect(pan_r));
	let (gesture, pan_resp) = widgets::pan(
		&mut pan_ui,
		Id::new(("pan", id)),
		state.pan(id),
		pan_r.size(),
	);
	match gesture {
		Gesture::Preview(v) => state.preview_pan(id, v),
		Gesture::Commit(v) => state.commit_pan(id, v),
		Gesture::Idle => {}
	}
	let pan_addr = crate::state::pan_address(id);
	if state.is_automated(&pan_addr) {
		super::automation::automated_badge(ui, pan_r);
	}
	pan_resp.context_menu(|ui| super::automation::automate_item(ui, state, pan_addr));

	let db = state.volume_db(id);
	let levels = state.meters.get(id);
	let fw = 30.0;
	let mw = 16.0;
	let left = fader_r.left() + ((w - fw - mw - 6.0) / 2.0).max(0.0);
	let fader_rect = Rect::from_min_size(pos2(left, fader_r.top()), vec2(fw, fader_r.height()));
	let meter_rect = Rect::from_min_size(
		pos2(fader_rect.right() + 6.0, fader_r.top()),
		vec2(mw, fader_r.height()),
	);
	let mut f_ui = ui.new_child(egui::UiBuilder::new().max_rect(fader_rect));
	let (gesture, fader_resp) =
		widgets::fader(&mut f_ui, Id::new(("fader", id)), db, fader_rect.size());
	match gesture {
		Gesture::Preview(v) => state.preview_volume(id, v),
		Gesture::Commit(v) => state.commit_volume(id, v),
		Gesture::Idle => {}
	}
	let vol_addr = crate::state::volume_address(id);
	if state.is_automated(&vol_addr) {
		super::automation::automated_badge(ui, fader_rect);
	}
	fader_resp.context_menu(|ui| super::automation::automate_item(ui, state, vol_addr));
	let mut m_ui = ui.new_child(egui::UiBuilder::new().max_rect(meter_rect));
	if widgets::meter(
		&mut m_ui,
		Id::new(("clip-lamp", id)),
		levels,
		meter_rect.size(),
	) {
		state.meters.reset_clip(id);
	}

	let half = (w - 3.0) / 2.0;
	let peak_text = if levels.hold_db <= crate::state::meters::FLOOR_DB {
		"-inf".to_owned()
	} else {
		format!("{:.1}", levels.hold_db)
	};
	for (i, (text, c)) in [
		(widgets::format_db(db), theme::TEXT),
		(
			peak_text,
			if levels.hold_db >= -0.1 {
				theme::METER_HIGH
			} else {
				theme::TEXT_DIM
			},
		),
	]
	.into_iter()
	.enumerate()
	{
		let r = Rect::from_min_size(
			pos2(readout_r.left() + i as f32 * (half + 3.0), readout_r.top()),
			vec2(half, readout_r.height()),
		);
		p.rect_filled(r, CornerRadius::same(2), theme::BG_DEEP);
		p.text(
			r.center(),
			Align2::CENTER_CENTER,
			text,
			theme::mono(10.5),
			c,
		);
	}

	if let Some(t) = track {
		let mut ms = ui.new_child(
			egui::UiBuilder::new()
				.max_rect(ms_r)
				.layout(Layout::left_to_right(Align::Center)),
		);
		ms.spacing_mut().item_spacing.x = 3.0;
		let arm = state.can_arm(t.id);
		let half = if arm {
			(ms_r.width() - 6.0) / 3.0
		} else {
			half
		};
		if widgets::toggle(&mut ms, channel.mute, "M", theme::MUTE, vec2(half, 18.0))
			.on_hover_text("Mute")
			.clicked()
		{
			set_channel(
				state,
				Some(t.id),
				ChannelPatch {
					mute: Some(!channel.mute),
					..Default::default()
				},
			);
		}
		if widgets::toggle(&mut ms, channel.solo, "S", theme::SOLO, vec2(half, 18.0))
			.on_hover_text("Solo")
			.clicked()
		{
			set_channel(
				state,
				Some(t.id),
				ChannelPatch {
					solo: Some(!channel.solo),
					..Default::default()
				},
			);
		}
		if arm {
			super::record::mixer_arm(&mut ms, state, t.id, half);
		}
	}
}

/// Drop target of the strip: an insert slot over the inserts area, the strip elsewhere.
fn drop_zone(
	ui: &Ui,
	state: &mut AppState,
	track: Option<TrackId>,
	strip: Rect,
	inserts: Rect,
	rows: &[Rect],
) {
	let ctx = ui.ctx().clone();
	let Some(payload) = egui::DragAndDrop::payload::<Payload>(&ctx).map(|p| (*p).clone()) else {
		return;
	};
	let Some(pos) = ctx.pointer_latest_pos() else {
		return;
	};
	if !strip.contains(pos) || !ui.clip_rect().contains(pos) {
		return;
	}
	let slot_target = matches!(&payload, Payload::Insert { .. })
		|| matches!(&payload, Payload::Device(i) if i.role == DeviceRole::Effect);
	let target = if slot_target && inserts.contains(pos) {
		let slot = rows.iter().filter(|r| r.center().y < pos.y).count();
		DropTarget::InsertSlot { track, slot }
	} else {
		DropTarget::Strip(track)
	};
	let copy = ctx.input(|i| i.modifiers.command);
	let action = state.drop_action(&payload, target, copy);
	let color = if action.accepted() {
		theme::ACCENT
	} else {
		theme::ERROR
	};
	let p = ui.painter();
	p.rect_stroke(
		strip,
		CornerRadius::same(theme::RADIUS),
		Stroke::new(1.5, color),
		StrokeKind::Inside,
	);
	if let DropTarget::InsertSlot { slot, .. } = target {
		let y = match (
			rows.get(slot),
			slot.checked_sub(1).and_then(|i| rows.get(i)),
		) {
			(Some(r), _) => r.top() - 1.0,
			(None, Some(r)) => r.bottom() + 1.0,
			(None, None) => inserts.top() + 2.0,
		};
		p.hline(
			inserts.left() + 2.0..=inserts.right() - 2.0,
			y.clamp(inserts.top() + 1.0, inserts.bottom() - 1.0),
			Stroke::new(2.0, color),
		);
	}
	dnd::set_hint(&ctx, action.describe(state.project()), action.accepted());
	if ctx.input(|i| i.pointer.any_released()) {
		egui::DragAndDrop::clear_payload(&ctx);
		state.drop_payload(&payload, target, copy);
	}
}

/// `text` shortened with an ellipsis to fit `max_width` in `font`.
pub fn elide(ui: &Ui, text: &str, font: &FontId, max_width: f32) -> String {
	let width = |s: &str| {
		ui.fonts_mut(|f| {
			f.layout_no_wrap(s.to_owned(), font.clone(), egui::Color32::WHITE)
				.size()
				.x
		})
	};
	if width(text) <= max_width {
		return text.to_owned();
	}
	let chars: Vec<char> = text.chars().collect();
	let (mut lo, mut hi) = (0, chars.len());
	while lo < hi {
		let mid = (lo + hi).div_ceil(2);
		let candidate: String = chars[..mid].iter().collect::<String>() + "…";
		if width(&candidate) <= max_width {
			lo = mid;
		} else {
			hi = mid - 1;
		}
	}
	chars[..lo].iter().collect::<String>().trim_end().to_owned() + "…"
}

fn section_label(p: &egui::Painter, r: Rect, text: &str) {
	p.text(
		r.left_center(),
		Align2::LEFT_CENTER,
		text,
		FontId::proportional(9.0),
		theme::TEXT_FAINT,
	);
}

fn set_channel(state: &mut AppState, track: Option<TrackId>, patch: ChannelPatch) {
	state.apply(Command::SetChannel { track, patch });
}

fn insert_row(
	ui: &mut Ui,
	state: &mut AppState,
	track: Option<TrackId>,
	d: &cadenza_model::Device,
	w: f32,
) -> Rect {
	let r = DeviceRef {
		track,
		device: d.id,
	};
	let (rect, _) = ui.allocate_exact_size(vec2(w - 4.0, ROW), Sense::hover());
	let dragging_me = matches!(
		egui::DragAndDrop::payload::<Payload>(ui.ctx()).as_deref(),
		Some(Payload::Insert { device, .. }) if *device == d.id
	);
	let selected = state.selection.device == Some(r);
	let p = ui.painter();
	p.rect_filled(
		rect,
		CornerRadius::same(2),
		if selected {
			theme::ACCENT_DIM
		} else {
			theme::BG_WIDGET
		},
	);
	let power = Rect::from_min_size(rect.min, vec2(16.0, ROW));
	let close = Rect::from_min_size(pos2(rect.right() - 14.0, rect.top()), vec2(14.0, ROW));
	let name = Rect::from_min_max(
		pos2(power.right(), rect.top()),
		pos2(close.left(), rect.bottom()),
	);
	let pr = ui
		.interact(power, Id::new(("bypass", d.id)), Sense::click())
		.on_hover_text(if d.bypass { "Enable" } else { "Bypass" });
	p.circle_filled(
		power.center(),
		3.5,
		if d.bypass {
			theme::BYPASS
		} else {
			theme::PLAYING
		},
	);
	if pr.clicked() {
		state.apply(Command::SetDeviceBypass {
			track,
			device: d.id,
			bypass: !d.bypass,
		});
	}
	let nr = ui.interact(name, Id::new(("insert", d.id)), Sense::click_and_drag());
	let font = FontId::proportional(10.5);
	let shown = elide(ui, &d.name, &font, name.width() - 4.0);
	let plugin = state.is_plugin(d.id);
	let hint = if plugin {
		"Double-click: plugin editor. Drag to reorder or onto another strip (Ctrl: copy)"
	} else {
		"Drag to reorder or onto another strip (Ctrl: copy)"
	};
	let nr = if shown != d.name {
		nr.on_hover_text(format!("{}\n\n{hint}", d.name))
	} else {
		nr.on_hover_text(hint)
	};
	if plugin && nr.double_clicked() {
		state.toggle_plugin_editor(d.id);
	}
	if nr.drag_started_by(egui::PointerButton::Primary) {
		egui::DragAndDrop::set_payload(
			ui.ctx(),
			Payload::Insert {
				track,
				device: d.id,
			},
		);
	}
	let p = ui.painter();
	if dragging_me || state.plugin_editor_open(d.id) {
		p.rect_stroke(
			rect,
			CornerRadius::same(2),
			Stroke::new(1.0, theme::ACCENT),
			StrokeKind::Inside,
		);
	}
	p.with_clip_rect(name).text(
		pos2(name.left() + 2.0, name.center().y),
		Align2::LEFT_CENTER,
		shown,
		font,
		if d.bypass || dragging_me {
			theme::TEXT_FAINT
		} else {
			theme::TEXT
		},
	);
	if nr.clicked() {
		state.selection.select_device(r);
	}
	let cr = ui
		.interact(close, Id::new(("remove", d.id)), Sense::click())
		.on_hover_text("Remove");
	p.text(
		close.center(),
		Align2::CENTER_CENTER,
		"×",
		FontId::proportional(12.0),
		if cr.hovered() {
			theme::ERROR
		} else {
			theme::TEXT_FAINT
		},
	);
	if cr.clicked() {
		state.remove_device(track, d.id);
	}
	rect
}

/// "+" button listing insertable effects.
fn insert_menu(ui: &mut Ui, state: &mut AppState, track: Option<TrackId>, w: f32) {
	let button = egui::Button::new(RichText::new("+").color(theme::TEXT_FAINT))
		.fill(theme::BG_DEEP)
		.min_size(vec2(w - 4.0, ROW));
	egui::containers::menu::MenuButton::from_button(button).ui(ui, |ui| {
		ui.set_min_width(180.0);
		let items: Vec<_> = state
			.browser_items()
			.iter()
			.filter(|i| i.role == DeviceRole::Effect)
			.cloned()
			.collect();
		egui::ScrollArea::vertical()
			.max_height(320.0)
			.show(ui, |ui| {
				for item in items {
					if ui
						.button(&item.name)
						.on_hover_text(&item.description)
						.clicked()
					{
						if track.is_none() {
							state.selection.select_master();
						}
						state.use_device(&item, track);
						ui.close();
					}
				}
			});
	});
}

fn send_row(ui: &mut Ui, state: &mut AppState, track: TrackId, s: &Send, w: f32) {
	let target = state
		.project()
		.track(s.target)
		.map_or("?".to_owned(), |b| b.name.clone());
	ui.horizontal(|ui| {
		ui.spacing_mut().item_spacing.x = 2.0;
		ui.add_sized(
			[w - 50.0, 16.0],
			egui::Label::new(
				RichText::new(target)
					.font(theme::small())
					.color(theme::TEXT_DIM),
			)
			.truncate(),
		)
		.context_menu(|ui| {
			if ui.button("Remove send").clicked() {
				state.apply(Command::RemoveSend { track, send: s.id });
				ui.close();
			}
		});
		let level = state.send_level(track, s.id);
		let (gesture, resp) = widgets::edit_value(ui, Id::new(("send", s.id)), level, |ui, v| {
			ui.add_sized(
				[44.0, 16.0],
				egui::DragValue::new(v)
					.range(-96.0..=6.0)
					.speed(0.2)
					.suffix(" dB")
					.max_decimals(1),
			)
		});
		let address = cadenza_core::ParamAddress::send(track, s.id);
		if state.is_automated(&address) {
			super::automation::automated_badge(ui, resp.rect);
		}
		resp.context_menu(|ui| super::automation::automate_item(ui, state, address));
		match gesture {
			Gesture::Preview(db) => state.preview_send(s.id, db),
			Gesture::Commit(db) => state.commit_send(track, s.id, db),
			Gesture::Idle => {}
		}
	});
}

fn output_selector(ui: &mut Ui, state: &mut AppState, track: &Track, channel: &Channel, w: f32) {
	let name_of = |o: Output, state: &AppState| match o {
		Output::Master => "Master".to_owned(),
		Output::Bus(b) => state
			.project()
			.track(b)
			.map_or("?".to_owned(), |t| t.name.clone()),
	};
	let current = name_of(channel.output, state);
	let mut choice = None;
	egui::ComboBox::from_id_salt(("output", track.id))
		.width(w - 4.0)
		.selected_text(RichText::new(current).font(theme::small()))
		.show_ui(ui, |ui| {
			let mut options = vec![Output::Master];
			options.extend(
				state
					.project()
					.tracks
					.iter()
					.filter(|b| b.kind == TrackKind::Bus && b.id != track.id)
					.map(|b| Output::Bus(b.id)),
			);
			for o in options {
				if ui
					.selectable_label(o == channel.output, name_of(o, state))
					.clicked()
				{
					choice = Some(o);
				}
			}
		});
	if let Some(o) = choice
		&& o != channel.output
	{
		set_channel(
			state,
			Some(track.id),
			ChannelPatch {
				output: Some(o),
				..Default::default()
			},
		);
	}
}
