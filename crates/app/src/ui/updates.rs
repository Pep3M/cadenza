//! The status-bar update chip and the update window (`state::update`).

use cadenza_update::{CURRENT_VERSION, InstallKind, Prepared};
use egui::{Id, RichText, Ui, vec2};

use crate::i18n::{tr, trf};
use crate::state::{AppState, Pending};
use crate::theme;

/// Shown only when there is something to do: an offered version, a download running or
/// an update waiting for the restart.
pub fn chip(ui: &mut Ui, state: &mut AppState) {
	let text = if state.update.ready.is_some() {
		tr("Restart to update").to_string()
	} else if let Some(p) = state.update.downloading() {
		let pct = p.fraction().map_or(0.0, |f| f * 100.0);
		trf(
			"Updating… {percent}%",
			&[("percent", &crate::i18n::decimal(format!("{pct:.0}")))],
		)
	} else if let Some(v) = state.update.prefs.offered(CURRENT_VERSION) {
		trf("Update {version}", &[("version", &v)])
	} else {
		return;
	};
	let r = ui
		.add(
			egui::Button::new(
				RichText::new(format!("⬆ {text}"))
					.font(theme::small())
					.color(theme::ACCENT_TEXT),
			)
			.fill(theme::ACCENT_DIM)
			.small(),
		)
		.on_hover_text(tr("A new version of Cadenza is available"));
	if r.clicked() {
		state.open_update_window();
	}
	ui.add_space(6.0);
}

pub fn window(ctx: &egui::Context, state: &mut AppState) {
	if !state.update.window {
		return;
	}
	let mut open = true;
	let mut close = false;
	egui::Window::new(tr("Update Cadenza"))
		.id(Id::new("update-window"))
		.open(&mut open)
		.frame(theme::dialog_frame())
		.default_width(460.0)
		.default_pos(ctx.content_rect().center() - vec2(230.0, 180.0))
		.resizable(false)
		.collapsible(false)
		.show(ctx, |ui| {
			close = body(ui, state);
		});
	if !open || close {
		state.update.window = false;
	}
}

/// Returns true when the window should close.
fn body(ui: &mut Ui, state: &mut AppState) -> bool {
	if state.update.checking() && state.update.release.is_none() {
		ui.horizontal(|ui| {
			ui.spinner();
			ui.label(tr("Checking for updates…"));
		});
		return false;
	}
	let Some(release) = state.update.available().cloned() else {
		if let Some(e) = &state.update.error {
			ui.label(RichText::new(e).color(theme::ERROR));
		} else if state.update.release.is_some() {
			ui.label(trf(
				"Cadenza {version} is up to date",
				&[("version", &CURRENT_VERSION)],
			));
		}
		ui.add_space(8.0);
		return ui.button(tr("Close")).clicked();
	};
	let latest = release.version().map(|v| v.to_string()).unwrap_or_default();
	ui.label(
		RichText::new(trf(
			"Cadenza {latest} is available (you have {current}).",
			&[("latest", &latest), ("current", &CURRENT_VERSION)],
		))
		.strong(),
	);
	ui.add_space(6.0);
	let (notes, _) = release.notes_excerpt(2000);
	if !notes.is_empty() {
		egui::Frame::new()
			.fill(theme::BG_DEEP)
			.corner_radius(4)
			.inner_margin(8)
			.show(ui, |ui| {
				egui::ScrollArea::vertical()
					.max_height(220.0)
					.show(ui, |ui| {
						ui.set_width(ui.available_width());
						ui.label(RichText::new(notes).color(theme::TEXT_DIM));
					});
			});
	}
	if !release.html_url.is_empty() && ui.link(tr("Release notes on GitHub")).clicked() {
		open_url(state, &release.html_url);
	}
	ui.add_space(8.0);
	if let Some(e) = &state.update.error {
		ui.label(RichText::new(e).color(theme::ERROR));
		ui.add_space(6.0);
	}
	let mut close = false;
	if let Some(ready) = state.update.ready.clone() {
		let (text, button) = match ready {
			Prepared::RunInstaller { .. } => (
				tr(
					"The installer is ready. Cadenza closes while it upgrades and starts again afterwards.",
				),
				tr("Install and restart"),
			),
			_ => (
				tr("The update is installed. Restart Cadenza to use it."),
				tr("Restart now"),
			),
		};
		ui.label(text);
		ui.add_space(8.0);
		ui.horizontal(|ui| {
			if theme::primary_button(ui, button).clicked() {
				state.request(Pending::Update);
				close = true;
			}
			if ui.button(tr("Later")).clicked() {
				close = true;
			}
		});
		return close;
	}
	if let Some(p) = state.update.downloading() {
		let bar = match p.fraction() {
			Some(f) => egui::ProgressBar::new(f).show_percentage(),
			None => egui::ProgressBar::new(0.0).animate(true),
		};
		ui.add(bar.desired_width(ui.available_width()));
		ui.add_space(6.0);
		if ui.button(tr("Cancel")).clicked() {
			state.update.cancel_download();
		}
		return false;
	}
	let kind = state.update.kind().clone();
	if let InstallKind::Manual(reason) = kind {
		ui.label(crate::state::update::manual_advice(reason));
		ui.add_space(8.0);
	}
	ui.horizontal(|ui| {
		if kind.can_download() {
			if theme::primary_button(ui, tr("Download and install")).clicked() {
				state.download_update();
			}
		} else if theme::primary_button(ui, tr("Open the download page")).clicked() {
			open_url(state, &release.html_url);
		}
		if ui.button(tr("Later")).clicked() {
			close = true;
		}
		if ui.button(tr("Skip this version")).clicked() {
			state.update.skip();
		}
	});
	close
}

pub(crate) fn open_url(state: &mut AppState, url: &str) {
	if let Err(e) = cadenza_update::open_in_browser(url) {
		state.notices.error(trf(
			"Could not open the browser: {e}",
			&[("e", &e.to_string())],
		));
	}
}
