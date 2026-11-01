//! Help → Report a problem…: title + description, then a prefilled GitHub issue in the
//! browser (`state::report`).

use egui::{Id, RichText, vec2};

use crate::i18n::tr;
use crate::state::AppState;
use crate::state::report::issue_url;
use crate::theme;

pub fn window(ctx: &egui::Context, state: &mut AppState) {
	if !state.report.open {
		return;
	}
	let mut open = true;
	let mut close = false;
	let info = state.system_info();
	egui::Window::new(tr("Report a problem"))
		.id(Id::new("report-window"))
		.open(&mut open)
		.frame(theme::dialog_frame())
		.default_width(480.0)
		.default_pos(ctx.content_rect().center() - vec2(240.0, 200.0))
		.resizable(false)
		.collapsible(false)
		.show(ctx, |ui| {
			ui.label(
				RichText::new(tr(
					"Opens a prefilled GitHub issue in your browser. You need a GitHub account to send it.",
				))
				.color(theme::TEXT_DIM),
			);
			ui.add_space(8.0);
			ui.label(tr("Title"));
			ui.add(
				egui::TextEdit::singleline(&mut state.report.title).desired_width(f32::INFINITY),
			);
			ui.add_space(6.0);
			ui.label(tr("What happened? Steps to reproduce help a lot."));
			ui.add(
				egui::TextEdit::multiline(&mut state.report.description)
					.desired_rows(6)
					.desired_width(f32::INFINITY),
			);
			ui.add_space(8.0);
			ui.label(
				RichText::new(tr(
					"Included automatically (no project content, file paths or API keys):",
				))
				.font(theme::small())
				.color(theme::TEXT_DIM),
			);
			ui.label(
				RichText::new(info.text())
					.font(theme::mono(11.0))
					.color(theme::TEXT_DIM),
			);
			ui.add_space(10.0);
			ui.horizontal(|ui| {
				let ready = !state.report.title.trim().is_empty();
				if ui
					.add_enabled_ui(ready, |ui| {
						theme::primary_button(ui, tr("Open the report on GitHub"))
					})
					.inner
					.clicked()
				{
					let url = issue_url(&state.report.title, &state.report.description, &info);
					super::updates::open_url(state, &url);
					state.report = Default::default();
					close = true;
				}
				if ui.button(tr("Copy system info")).clicked() {
					ui.ctx().copy_text(info.text());
					state.notices.info(tr("System info copied"));
				}
				if ui.button(tr("Cancel")).clicked() {
					close = true;
				}
			});
		});
	if !open || close {
		state.report.open = false;
	}
}
