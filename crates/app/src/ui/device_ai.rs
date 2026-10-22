//! The device editor's AI parts (design 15e): the header's "Ask AI" button with its prompt
//! popover (suggestions per device), and the card listing what the last AI request
//! changed on the open device, with Keep, Undo and "Hold to hear before".

use crate::i18n::{tr, trf, trn};
use cadenza_dsp::{DeviceInfo, ParamInfo};
use egui::{
	Align, Align2, Color32, CornerRadius, CursorIcon, Id, Layout, Rect, Sense, Stroke, StrokeKind,
	Ui, UiBuilder, pos2, vec2,
};

use crate::state::AppState;
use crate::state::ai_changes;
use crate::state::device::{self, ParamControl};
use crate::state::selection::DeviceRef;
use crate::theme;

/// Height of the AI change card (with its gap to the body).
pub const CARD_H: f32 = 70.0;

/// The text being typed into the Ask AI popover.
#[derive(Default)]
pub struct AskAi {
	pub text: String,
	focus: bool,
}

/// Id of the popover's text field.
pub fn input_id(r: DeviceRef) -> Id {
	Id::new(("device-ask-ai-input", r.device))
}

fn popover_id(r: DeviceRef) -> Id {
	Id::new(("device-ask-ai", r.device))
}

fn prefill_id(r: DeviceRef) -> Id {
	Id::new(("device-ask-ai-prefill", r.device))
}

/// Opens the Ask AI popover of `r` with `text` typed in (a control's "Ask AI about
/// this"), opening its editor first when it is not shown.
pub fn ask_about(ctx: &egui::Context, state: &mut AppState, r: DeviceRef, text: String) {
	ctx.data_mut(|d| d.insert_temp(prefill_id(r), text));
	state.open_device_editor(r);
	ctx.request_repaint();
}

/// "Explain" (header, next to Ask AI): asks the AI producer to explain the device's
/// current settings, read only; the answer streams into the chat.
pub fn explain_button(ui: &mut Ui, state: &mut AppState, r: DeviceRef) {
	let font = theme::medium(12.0);
	let label = tr("Explain");
	let g = ui
		.painter()
		.layout_no_wrap(label.to_owned(), font, theme::TEXT_DIM);
	let (rect, resp) = ui.allocate_exact_size(vec2(g.size().x + 16.0, 22.0), Sense::click());
	resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label));
	let p = ui.painter();
	let fill = if resp.hovered() {
		theme::BG_HOVER
	} else {
		theme::BG_RAISED
	};
	p.rect(
		rect,
		CornerRadius::same(theme::RADIUS_MD),
		fill,
		Stroke::new(1.0, theme::ACCENT_BORDER),
		StrokeKind::Inside,
	);
	p.galley(rect.center() - g.size() * 0.5, g, theme::TEXT_DIM);
	let busy = state.ai.busy();
	let resp = resp
		.on_hover_cursor(CursorIcon::PointingHand)
		.on_hover_text(if busy {
			tr("The AI producer is busy with a request")
		} else {
			tr("Ask the AI producer to explain this device's settings (changes nothing)")
		});
	if resp.clicked() && !busy {
		state.explain_device(r);
	}
}

/// "Ask AI" (accent-tinted) and its popover: a text field (Enter sends) and suggestion
/// chips that send at once. Sending opens the chat so the reply shows.
pub fn ask_button(ui: &mut Ui, state: &mut AppState, r: DeviceRef, ask: &mut AskAi) {
	let font = theme::medium(12.0);
	let label = tr("Ask AI");
	let g = ui
		.painter()
		.layout_no_wrap(label.to_owned(), font, theme::ACCENT_TEXT);
	let (rect, resp) = ui.allocate_exact_size(vec2(g.size().x + 16.0, 22.0), Sense::click());
	resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label));
	let p = ui.painter();
	let fill = if resp.hovered() {
		theme::mix(theme::ACCENT_DIM, theme::ACCENT, 0.18)
	} else {
		theme::ACCENT_DIM
	};
	p.rect(
		rect,
		CornerRadius::same(theme::RADIUS_MD),
		fill,
		Stroke::new(1.0, theme::ACCENT_BORDER),
		StrokeKind::Inside,
	);
	p.galley(rect.center() - g.size() * 0.5, g, theme::ACCENT_TEXT);
	let resp = resp
		.on_hover_cursor(CursorIcon::PointingHand)
		.on_hover_text(tr("Ask the AI producer to change this device"));
	if resp.clicked() {
		ask.focus = true;
	}
	let ctx = ui.ctx().clone();
	if let Some(text) = ctx.data_mut(|d| d.remove_temp::<String>(prefill_id(r))) {
		ask.text = text;
		ask.focus = true;
		egui::Popup::open_id(&ctx, popover_id(r));
	}
	let Some(d) = state.device(r) else {
		return;
	};
	let name = d.name.clone();
	let suggestions = d
		.builtin_id()
		.map_or(ai_changes::GENERIC, ai_changes::suggestions);
	egui::Popup::menu(&resp)
		.id(popover_id(r))
		.close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
		.width(340.0)
		.show(|ui| {
			ui.set_width(340.0);
			ui.spacing_mut().item_spacing = vec2(6.0, 8.0);
			ui.label(
				egui::RichText::new(trf("Ask AI about {name}", &[("name", &name)]))
					.font(theme::semibold(12.5))
					.color(theme::TEXT),
			);
			let edit = egui::TextEdit::singleline(&mut ask.text)
				.id(input_id(r))
				.hint_text(tr("What should change? (Enter sends)"))
				.desired_width(f32::INFINITY);
			let out = ui.add(edit);
			if ask.focus {
				out.request_focus();
				// The caret after a prefilled question, ready to type the rest.
				if let Some(mut st) = egui::TextEdit::load_state(ui.ctx(), input_id(r)) {
					let end = egui::text::CCursor::new(ask.text.chars().count());
					st.cursor
						.set_char_range(Some(egui::text::CCursorRange::one(end)));
					st.store(ui.ctx(), input_id(r));
				}
				ask.focus = false;
			}
			let enter = out.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
			let mut send: Option<String> = None;
			if enter && !ask.text.trim().is_empty() {
				send = Some(ask.text.clone());
			}
			ui.horizontal_wrapped(|ui| {
				ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
				for s in suggestions.iter().chain(
					ai_changes::GENERIC
						.iter()
						.filter(|_| suggestions != ai_changes::GENERIC)
						.take(1),
				) {
					let s = crate::i18n::tr_str(s);
					if chip(ui, s).clicked() {
						send = Some(s.to_owned());
					}
				}
			});
			ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
				let ok = !ask.text.trim().is_empty();
				if ui.add_enabled(ok, egui::Button::new(tr("Send"))).clicked() {
					send = Some(ask.text.clone());
				}
				ui.label(
					egui::RichText::new(tr("The reply shows in the chat (F10)"))
						.font(theme::small())
						.color(theme::TEXT_FAINT),
				);
			});
			if let Some(text) = send
				&& state.ask_ai_about_device(r, &text)
			{
				ask.text.clear();
				ui.close();
			}
		});
}

fn chip(ui: &mut Ui, label: &str) -> egui::Response {
	let font = theme::medium(11.5);
	let g = ui
		.painter()
		.layout_no_wrap(label.to_owned(), font, theme::TEXT_DIM);
	let (rect, resp) = ui.allocate_exact_size(vec2(g.size().x + 16.0, 22.0), Sense::click());
	resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label));
	let hovered = resp.hovered();
	let p = ui.painter();
	p.rect(
		rect,
		CornerRadius::same(11),
		if hovered {
			theme::BG_HOVER
		} else {
			theme::BG_WIDGET
		},
		Stroke::new(1.0, theme::BORDER),
		StrokeKind::Inside,
	);
	p.galley(
		rect.center() - g.size() * 0.5,
		g,
		if hovered {
			theme::TEXT
		} else {
			theme::TEXT_DIM
		},
	);
	resp.on_hover_cursor(CursorIcon::PointingHand)
}

/// A param value in its units, choices by name.
pub fn value_text(info: &DeviceInfo, p: &ParamInfo, v: f32) -> String {
	match device::param_control(info.id, p) {
		ParamControl::Segmented(labels) | ParamControl::Grid(labels, _) => {
			let i = (v - p.min).round().max(0.0) as usize;
			labels
				.get(i.min(labels.len().saturating_sub(1)))
				.cloned()
				.unwrap_or_default()
		}
		ParamControl::Toggle(_) => if v >= 0.5 { "on" } else { "off" }.to_owned(),
		ParamControl::Knob => device::format_value(v, p.unit),
	}
}

/// Whether the card shows for `r` (the last AI request changed it).
pub fn has_card(state: &AppState, r: DeviceRef) -> bool {
	!state.ai_changes_of(r.device).is_empty()
}

/// "AI changed N params · “request”", one entry per change (name, before → after), and
/// Hold to hear before / Undo / Keep.
pub fn card(
	ui: &mut Ui,
	state: &mut AppState,
	r: DeviceRef,
	info: &'static DeviceInfo,
	rect: Rect,
) {
	let changes = state.ai_changes_of(r.device).to_vec();
	let Some(all) = state.ai_changes.as_ref() else {
		return;
	};
	let request = all.request.clone();
	let elsewhere = all.count() - changes.len();
	let last = state.ai_step_is_last();
	// The card's own background takes the pointer (nothing under it reacts).
	ui.interact(
		rect,
		Id::new(("ai-card", r.device)),
		Sense::click_and_drag(),
	);
	let p = ui.painter();
	p.rect(
		rect,
		CornerRadius::same(theme::RADIUS_LG),
		theme::mix(theme::BG_PANEL, theme::ACCENT, 0.07),
		Stroke::new(1.0, theme::ACCENT_BORDER),
		StrokeKind::Inside,
	);
	let inner = rect.shrink2(vec2(10.0, 6.0));
	let top = Rect::from_min_size(inner.min, vec2(inner.width(), 24.0));
	let mut row = ui.new_child(
		UiBuilder::new()
			.max_rect(top)
			.layout(Layout::left_to_right(Align::Center)),
	);
	row.spacing_mut().item_spacing.x = 8.0;
	ai_badge(&mut row);
	let n = changes.len();
	let mut title = trn("AI changed {n} param", "AI changed {n} params", n);
	if elsewhere > 0 {
		title.push_str(&trf(
			" (+{elsewhere} on other devices)",
			&[("elsewhere", &elsewhere)],
		));
	}
	row.label(
		egui::RichText::new(title)
			.font(theme::semibold(12.5))
			.color(theme::TEXT),
	);
	let quote = crate::ui::mixer::elide(
		&row,
		&format!("“{request}”"),
		&theme::medium(12.0),
		(top.width() - 620.0).max(80.0),
	);
	row.label(
		egui::RichText::new(quote)
			.font(theme::medium(12.0))
			.color(theme::TEXT_DIM),
	);
	row.with_layout(Layout::right_to_left(Align::Center), |ui| {
		ui.spacing_mut().item_spacing.x = 6.0;
		if card_button(ui, tr("Keep"), true, true)
			.on_hover_text(tr("Keep the AI's changes and clear the blue rings"))
			.clicked()
		{
			state.keep_ai_changes();
		}
		let key = if cfg!(target_os = "macos") {
			"⌘Z"
		} else {
			"Ctrl+Z"
		};
		let undo = card_button(ui, &trf("Undo  {key}", &[("key", &key)]), false, last);
		let undo = if last {
			undo.on_hover_text(trf(
				"Undo \"{all_label}\" (one step)",
				&[("all_label", &all_label(state))],
			))
		} else {
			undo.on_hover_text(tr(
				"Other edits came after the AI step: undo them first (Ctrl+Z) or use Keep",
			))
		};
		if undo.clicked() {
			state.undo_ai_changes();
		}
		let held_now = state.hearing_before() == Some(r.device);
		let label = if held_now {
			tr("Hearing before…")
		} else {
			tr("Hold to hear before")
		};
		let hold = card_button(ui, label, held_now, true).on_hover_text(tr(
			"Plays the values from before the AI request while pressed",
		));
		let held = hold.is_pointer_button_down_on();
		if held {
			ui.ctx().request_repaint();
		}
		state.hear_before(held.then_some(r.device));
	});
	let lines = Rect::from_min_max(pos2(inner.left(), top.bottom() + 4.0), inner.max);
	let mut list = ui.new_child(
		UiBuilder::new()
			.max_rect(lines)
			.layout(Layout::left_to_right(Align::Center).with_main_wrap(true)),
	);
	list.set_clip_rect(lines.intersect(ui.clip_rect()));
	list.spacing_mut().item_spacing = vec2(18.0, 4.0);
	for c in &changes {
		let Some(p) = info.param(&c.param) else {
			continue;
		};
		let text = format!(
			"{} → {}",
			value_text(info, p, c.before),
			value_text(info, p, c.after)
		);
		list.horizontal(|ui| {
			ui.spacing_mut().item_spacing.x = 6.0;
			ui.label(
				egui::RichText::new(p.name)
					.font(theme::medium(11.5))
					.color(theme::TEXT_DIM),
			);
			ui.label(
				egui::RichText::new(text)
					.font(theme::mono(11.5))
					.color(theme::ACCENT_TEXT),
			);
		});
	}
	if info.id == "eq" {
		list.label(
			egui::RichText::new(tr("Dashed curve = before"))
				.font(theme::medium(11.0))
				.color(theme::TEXT_FAINT),
		);
	}
}

fn all_label(state: &AppState) -> String {
	state
		.ai_changes
		.as_ref()
		.map(|c| c.label.clone())
		.unwrap_or_default()
}

fn ai_badge(ui: &mut Ui) {
	let font = theme::mono_medium(10.0);
	let g = ui
		.painter()
		.layout_no_wrap("AI".to_owned(), font, theme::TEXT_ON_COLOR);
	let (rect, _) = ui.allocate_exact_size(vec2(g.size().x + 10.0, 16.0), Sense::hover());
	ui.painter()
		.rect_filled(rect, CornerRadius::same(theme::RADIUS), theme::ACCENT);
	ui.painter()
		.galley(rect.center() - g.size() * 0.5, g, theme::TEXT_ON_COLOR);
}

/// A card button: `primary` = accent fill; disabled ones dim.
fn card_button(ui: &mut Ui, label: &str, primary: bool, enabled: bool) -> egui::Response {
	let font = theme::medium(12.0);
	let g = ui
		.painter()
		.layout_no_wrap(label.to_owned(), font, theme::TEXT);
	let (rect, resp) = ui.allocate_exact_size(
		vec2(g.size().x + 18.0, 22.0),
		if enabled {
			Sense::click()
		} else {
			Sense::hover()
		},
	);
	resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, label));
	let hovered = enabled && resp.hovered();
	let (fill, fg, border) = match (primary, enabled) {
		(_, false) => (theme::BG_WIDGET, theme::TEXT_FAINT, Color32::TRANSPARENT),
		(true, true) => (
			if hovered {
				theme::ACCENT_HOVER
			} else {
				theme::ACCENT
			},
			theme::TEXT_ON_COLOR,
			Color32::TRANSPARENT,
		),
		(false, true) => (
			if hovered {
				theme::BG_HOVER
			} else {
				theme::BG_WIDGET
			},
			theme::TEXT,
			theme::BORDER,
		),
	};
	let p = ui.painter();
	p.rect(
		rect,
		CornerRadius::same(theme::RADIUS_MD),
		fill,
		Stroke::new(1.0, border),
		StrokeKind::Inside,
	);
	p.text(
		rect.center(),
		Align2::CENTER_CENTER,
		label,
		theme::medium(12.0),
		fg,
	);
	resp.on_hover_cursor(if enabled {
		CursorIcon::PointingHand
	} else {
		CursorIcon::NotAllowed
	})
}
