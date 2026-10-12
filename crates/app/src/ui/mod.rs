//! egui views. [`CadenzaApp`] lays out the panels (dock-style: menu, transport, browser
//! left, inspector right, mixer bottom, arrange center) and routes shortcuts; the views
//! only read [`AppState`] and call its methods, so every edit becomes a `Command`.

pub mod arrange;
pub mod browser;
pub mod inspector;
pub mod mixer;
pub mod transport;
pub mod widgets;

use std::path::PathBuf;
use std::time::Instant;

use cadenza_model::TrackKind;
use egui::{
	Align, Align2, Frame, Key, KeyboardShortcut, Layout, Margin, Modifiers, Panel, RichText, Ui,
	ViewportCommand, vec2,
};

use crate::jobs::FileAction;
use crate::state::notices::Level;
use crate::state::{AppState, Pending, edits};
use crate::theme;

/// The whole application window.
pub struct CadenzaApp {
	pub state: AppState,
	arrange: arrange::ArrangeUi,
	browser: browser::BrowserUi,
	title: String,
	themed: bool,
}

impl CadenzaApp {
	/// The real app: audio engine on, repaints woken by background threads.
	pub fn new(ctx: &egui::Context, project: Option<PathBuf>) -> Self {
		let waker_ctx = ctx.clone();
		let state = AppState::new(Some(std::sync::Arc::new(move || {
			waker_ctx.request_repaint()
		})));
		let mut app = Self::with_state(state);
		if let Some(path) = project {
			app.state.open(&path);
		}
		theme::apply(ctx);
		app.themed = true;
		app
	}

	/// Around an existing state (tests, headless rendering).
	pub fn with_state(state: AppState) -> Self {
		Self {
			state,
			arrange: arrange::ArrangeUi::default(),
			browser: browser::BrowserUi::default(),
			title: String::new(),
			themed: false,
		}
	}

	/// Per-frame logic that does not draw (eframe calls it before [`Self::ui`]).
	pub fn logic(&mut self, ctx: &egui::Context) {
		let animating = self.state.tick(Instant::now());
		if animating {
			ctx.request_repaint();
		}
		let title = self.state.title();
		if title != self.title {
			ctx.send_viewport_cmd(ViewportCommand::Title(title.clone()));
			self.title = title;
		}
		// Closing with unsaved changes asks first.
		if ctx.input(|i| i.viewport().close_requested()) && !self.state.quit_confirmed {
			if self.state.dirty {
				ctx.send_viewport_cmd(ViewportCommand::CancelClose);
				self.state.request(Pending::Quit);
			} else {
				self.state.stop();
			}
		}
		if self.state.quit_confirmed {
			self.state.stop();
			ctx.send_viewport_cmd(ViewportCommand::Close);
		}
	}

	/// Draws the whole window into `ui`.
	pub fn ui(&mut self, ui: &mut Ui) {
		if !self.themed {
			theme::apply(ui.ctx());
			self.themed = true;
		}
		self.shortcuts(ui.ctx());
		let state = &mut self.state;

		Panel::top("menu")
			.frame(
				Frame::new()
					.fill(theme::BG_DEEP)
					.inner_margin(Margin::symmetric(6, 2)),
			)
			.show(ui, |ui| menu_bar(ui, state));
		Panel::top("transport")
			.exact_size(theme::TRANSPORT_HEIGHT)
			.frame(
				Frame::new()
					.fill(theme::BG_RAISED)
					.inner_margin(Margin::symmetric(6, 4))
					.stroke(egui::Stroke::new(1.0, theme::BORDER)),
			)
			.show(ui, |ui| transport::show(ui, state));
		Panel::bottom("status")
			.exact_size(22.0)
			.frame(
				Frame::new()
					.fill(theme::BG_DEEP)
					.inner_margin(Margin::symmetric(8, 2)),
			)
			.show(ui, |ui| status_bar(ui, state));
		if state.view.show_mixer {
			Panel::bottom("mixer")
				.resizable(true)
				.default_size(430.0)
				.size_range(300.0..=700.0)
				.frame(
					Frame::new()
						.fill(theme::BG_DEEP)
						.inner_margin(Margin::symmetric(0, 4)),
				)
				.show(ui, |ui| mixer::show(ui, state));
		}
		if state.view.show_browser {
			Panel::left("browser")
				.resizable(true)
				.default_size(230.0)
				.size_range(180.0..=420.0)
				.frame(
					Frame::new()
						.fill(theme::BG_PANEL)
						.inner_margin(Margin::symmetric(8, 4)),
				)
				.show(ui, |ui| self.browser.show(ui, state));
		}
		if state.view.show_inspector {
			Panel::right("inspector")
				.resizable(true)
				.default_size(270.0)
				.size_range(220.0..=460.0)
				.frame(
					Frame::new()
						.fill(theme::BG_PANEL)
						.inner_margin(Margin::symmetric(8, 4)),
				)
				.show(ui, |ui| inspector::show(ui, state));
		}
		egui::CentralPanel::no_frame().show(ui, |ui| self.arrange.show(ui, state));

		toasts(ui.ctx(), state);
		unsaved_prompt(ui.ctx(), state);
		busy_overlay(ui.ctx(), state);
	}

	fn shortcuts(&mut self, ctx: &egui::Context) {
		let typing = ctx.egui_wants_keyboard_input();
		let cmd = Modifiers::COMMAND;
		let shift_cmd = Modifiers::COMMAND | Modifiers::SHIFT;
		let s = &mut self.state;
		let pressed = |m: Modifiers, k: Key| {
			ctx.input_mut(|i| i.consume_shortcut(&KeyboardShortcut::new(m, k)))
		};
		// More specific (shifted) shortcuts first: Ctrl+Z also matches Ctrl+Shift+Z.
		if pressed(shift_cmd, Key::Z) || pressed(cmd, Key::Y) {
			s.redo();
		} else if pressed(cmd, Key::Z) {
			s.undo();
		}
		if pressed(shift_cmd, Key::S) {
			s.file_dialog(FileAction::SaveAs);
		} else if pressed(cmd, Key::S) {
			s.save();
		}
		if pressed(cmd, Key::O) {
			s.request(Pending::Open(None));
		}
		if pressed(cmd, Key::N) {
			s.request(Pending::New);
		}
		if pressed(cmd, Key::E) {
			s.file_dialog(FileAction::Export);
		}
		if pressed(cmd, Key::I) {
			s.file_dialog(FileAction::ImportAudio);
		}
		if pressed(Modifiers::NONE, Key::F3) {
			s.view.show_mixer = !s.view.show_mixer;
		}
		if pressed(Modifiers::NONE, Key::F4) {
			s.view.show_inspector = !s.view.show_inspector;
		}
		if pressed(Modifiers::NONE, Key::F5) {
			s.view.show_browser = !s.view.show_browser;
		}
		if typing {
			return;
		}
		if pressed(cmd, Key::D) {
			duplicate_selection(s);
		}
		if pressed(Modifiers::NONE, Key::Space) {
			s.toggle_play();
		}
		if pressed(Modifiers::NONE, Key::Home) || pressed(Modifiers::NONE, Key::Comma) {
			s.seek(cadenza_core::Tick::ZERO);
		}
		if pressed(Modifiers::NONE, Key::Delete) || pressed(Modifiers::NONE, Key::Backspace) {
			delete_selection(s);
		}
		if pressed(Modifiers::NONE, Key::S) {
			split_at_playhead(s);
		}
		if pressed(Modifiers::NONE, Key::Escape) {
			s.selection.clear_clips();
		}
	}
}

impl eframe::App for CadenzaApp {
	fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
		CadenzaApp::logic(self, ctx);
	}

	fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
		CadenzaApp::ui(self, ui);
	}
}

/// Deletes the selected clips (one undo step).
pub fn delete_selection(state: &mut AppState) {
	let clips: Vec<_> = state.selection.clips.iter().copied().collect();
	if let Some(cmd) = edits::delete_clips(&clips) {
		state.apply(cmd);
	}
}

/// Duplicates the selected clips after the selection and selects the copies.
pub fn duplicate_selection(state: &mut AppState) {
	let clips: Vec<_> = state.selection.clips.iter().copied().collect();
	if let Some((cmd, ids)) = edits::duplicate_clips(state.project(), &clips)
		&& state.apply(cmd)
	{
		state.selection.clips = ids.into_iter().collect();
	}
}

/// Splits every selected clip under the playhead (one undo step).
pub fn split_at_playhead(state: &mut AppState) {
	let at = state.position();
	let mut commands: Vec<_> = state
		.selection
		.clips
		.iter()
		.filter_map(|&c| edits::split_clip(state.project(), c, at))
		.map(|(cmd, _)| cmd)
		.collect();
	match commands.len() {
		0 => state
			.notices
			.info("Nothing to split: select clips under the playhead"),
		1 => {
			state.apply(commands.remove(0));
		}
		_ => {
			state.apply(cadenza_model::Command::Batch {
				label: "Split clips".into(),
				commands,
			});
		}
	}
}

fn menu_bar(ui: &mut Ui, state: &mut AppState) {
	egui::MenuBar::new().ui(ui, |ui| {
		ui.label(RichText::new("CADENZA").strong().color(theme::ACCENT));
		ui.add_space(6.0);
		ui.menu_button("File", |ui| {
			if shortcut_button(ui, "New", "Ctrl+N") {
				state.request(Pending::New);
			}
			if shortcut_button(ui, "Open…", "Ctrl+O") {
				state.request(Pending::Open(None));
			}
			ui.separator();
			if shortcut_button(ui, "Save", "Ctrl+S") {
				state.save();
			}
			if shortcut_button(ui, "Save As…", "Ctrl+Shift+S") {
				state.file_dialog(FileAction::SaveAs);
			}
			ui.separator();
			if shortcut_button(ui, "Import Audio…", "Ctrl+I") {
				state.file_dialog(FileAction::ImportAudio);
			}
			if shortcut_button(ui, "Import MIDI…", "") {
				state.file_dialog(FileAction::ImportMidi);
			}
			if shortcut_button(ui, "Export Render…", "Ctrl+E") {
				state.file_dialog(FileAction::Export);
			}
			ui.separator();
			if shortcut_button(ui, "Quit", "") {
				state.request(Pending::Quit);
			}
		});
		ui.menu_button("Edit", |ui| {
			let undo = state.undo_label().map(|l| format!("Undo {l}"));
			if ui
				.add_enabled(
					undo.is_some(),
					egui::Button::new(undo.unwrap_or_else(|| "Undo".into()))
						.shortcut_text("Ctrl+Z"),
				)
				.clicked()
			{
				state.undo();
			}
			let redo = state.redo_label().map(|l| format!("Redo {l}"));
			if ui
				.add_enabled(
					redo.is_some(),
					egui::Button::new(redo.unwrap_or_else(|| "Redo".into()))
						.shortcut_text("Ctrl+Shift+Z"),
				)
				.clicked()
			{
				state.redo();
			}
			ui.separator();
			let has_clips = !state.selection.clips.is_empty();
			if ui
				.add_enabled(
					has_clips,
					egui::Button::new("Duplicate").shortcut_text("Ctrl+D"),
				)
				.clicked()
			{
				duplicate_selection(state);
			}
			if ui
				.add_enabled(
					has_clips,
					egui::Button::new("Split at Playhead").shortcut_text("S"),
				)
				.clicked()
			{
				split_at_playhead(state);
			}
			if ui
				.add_enabled(has_clips, egui::Button::new("Delete").shortcut_text("Del"))
				.clicked()
			{
				delete_selection(state);
			}
		});
		ui.menu_button("Track", |ui| {
			for (label, kind) in [
				("Add Instrument Track", TrackKind::Instrument),
				("Add Audio Track", TrackKind::Audio),
				("Add Bus", TrackKind::Bus),
			] {
				if ui.button(label).clicked() {
					arrange::add_track(state, kind);
				}
			}
			ui.separator();
			let sel = state.selection.track;
			if ui
				.add_enabled(sel.is_some(), egui::Button::new("Move Up"))
				.clicked() && let Some(cmd) =
				sel.and_then(|t| edits::move_track(state.project(), t, -1))
			{
				state.apply(cmd);
			}
			if ui
				.add_enabled(sel.is_some(), egui::Button::new("Move Down"))
				.clicked() && let Some(cmd) =
				sel.and_then(|t| edits::move_track(state.project(), t, 1))
			{
				state.apply(cmd);
			}
			if ui
				.add_enabled(sel.is_some(), egui::Button::new("Delete Track"))
				.clicked() && let Some(track) = sel
			{
				state.apply(cadenza_model::Command::RemoveTrack { track });
			}
		});
		ui.menu_button("View", |ui| {
			ui.checkbox(&mut state.view.show_browser, "Browser  (F5)");
			ui.checkbox(&mut state.view.show_inspector, "Inspector  (F4)");
			ui.checkbox(&mut state.view.show_mixer, "Mixer  (F3)");
			ui.separator();
			ui.checkbox(&mut state.view.snap, "Snap to grid");
			ui.checkbox(&mut state.view.follow_playhead, "Follow playhead");
			ui.separator();
			ui.label(
				RichText::new("Track height")
					.font(theme::small())
					.color(theme::TEXT_DIM),
			);
			ui.add(egui::Slider::new(
				&mut state.view.track_height,
				crate::state::MIN_TRACK_HEIGHT..=crate::state::MAX_TRACK_HEIGHT,
			));
		});
	});
}

fn shortcut_button(ui: &mut Ui, label: &str, shortcut: &str) -> bool {
	ui.add(egui::Button::new(label).shortcut_text(shortcut))
		.clicked()
}

fn status_bar(ui: &mut Ui, state: &mut AppState) {
	ui.horizontal_centered(|ui| {
		let dirty = if state.dirty { "● unsaved" } else { "saved" };
		ui.label(
			RichText::new(dirty)
				.font(theme::small())
				.color(if state.dirty {
					theme::WARNING
				} else {
					theme::TEXT_FAINT
				}),
		);
		ui.separator();
		if let Some(n) = state.notices.last() {
			let color = match n.level {
				Level::Info => theme::TEXT_DIM,
				Level::Warning => theme::WARNING,
				Level::Error => theme::ERROR,
			};
			ui.add(
				egui::Label::new(RichText::new(&n.text).font(theme::small()).color(color))
					.truncate(),
			);
		}
		ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
			let warnings = state.load_warnings.len();
			if warnings > 0 {
				let r = ui.label(
					RichText::new(format!(
						"⚠ {warnings} engine warning{}",
						if warnings == 1 { "" } else { "s" }
					))
					.font(theme::small())
					.color(theme::WARNING),
				);
				r.on_hover_text(state.load_warnings.join("\n"));
			}
			let p = state.project();
			ui.label(
				RichText::new(format!(
					"{} tracks · {} clips · {} undo steps",
					p.tracks.len(),
					p.tracks.iter().map(|t| t.clips.len()).sum::<usize>(),
					state.document().history_labels().len()
				))
				.font(theme::small())
				.color(theme::TEXT_FAINT),
			);
		});
	});
}

fn toasts(ctx: &egui::Context, state: &AppState) {
	let now = Instant::now();
	let list = state.notices.toasts(now, 4);
	if list.is_empty() {
		return;
	}
	egui::Area::new(egui::Id::new("toasts"))
		.anchor(Align2::RIGHT_BOTTOM, vec2(-14.0, -34.0))
		.order(egui::Order::Foreground)
		.interactable(false)
		.show(ctx, |ui| {
			ui.with_layout(Layout::top_down(Align::Max), |ui| {
				for n in list {
					let (accent, icon) = match n.level {
						Level::Info => (theme::ACCENT, "ℹ"),
						Level::Warning => (theme::WARNING, "⚠"),
						Level::Error => (theme::ERROR, "✖"),
					};
					Frame::new()
						.fill(theme::BG_RAISED)
						.stroke(egui::Stroke::new(1.0, accent.gamma_multiply(0.7)))
						.corner_radius(4)
						.inner_margin(Margin::symmetric(10, 6))
						.show(ui, |ui| {
							ui.set_max_width(420.0);
							ui.horizontal(|ui| {
								ui.label(RichText::new(icon).color(accent));
								ui.add(
									egui::Label::new(RichText::new(&n.text).color(theme::TEXT))
										.wrap(),
								);
							});
						});
					ui.add_space(4.0);
				}
			});
		});
	ctx.request_repaint_after(std::time::Duration::from_millis(250));
}

fn unsaved_prompt(ctx: &egui::Context, state: &mut AppState) {
	let Some(pending) = state.confirm.clone() else {
		return;
	};
	let action = match pending {
		Pending::New => "creating a new project",
		Pending::Open(_) => "opening another project",
		Pending::Quit => "quitting",
	};
	egui::Modal::new(egui::Id::new("unsaved")).show(ctx, |ui| {
		ui.set_width(380.0);
		ui.heading("Unsaved changes");
		ui.add_space(4.0);
		ui.label(format!(
			"'{}' has changes that are not saved. Save them before {action}?",
			state.project().name
		));
		ui.add_space(10.0);
		ui.horizontal(|ui| {
			if ui.button(RichText::new("Save").strong()).clicked() {
				state.save_then(pending.clone());
			}
			if ui.button("Don't save").clicked() {
				state.dirty = false;
				state.proceed(pending.clone());
			}
			if ui.button("Cancel").clicked() {
				state.confirm = None;
			}
		});
	});
}

/// Spinner while an export or plugin scan runs in the background.
fn busy_overlay(ctx: &egui::Context, state: &AppState) {
	let jobs: Vec<String> = [
		state
			.jobs
			.export
			.as_ref()
			.map(|j| format!("Rendering… {:.0} s", j.started.elapsed().as_secs_f32())),
		state.jobs.scan.as_ref().map(|j| {
			format!(
				"Scanning plugins… {:.0} s",
				j.started.elapsed().as_secs_f32()
			)
		}),
	]
	.into_iter()
	.flatten()
	.collect();
	if jobs.is_empty() {
		return;
	}
	egui::Area::new(egui::Id::new("busy"))
		.anchor(Align2::CENTER_BOTTOM, vec2(0.0, -34.0))
		.order(egui::Order::Foreground)
		.interactable(false)
		.show(ctx, |ui| {
			Frame::new()
				.fill(theme::BG_RAISED)
				.stroke(egui::Stroke::new(1.0, theme::BORDER_STRONG))
				.corner_radius(4)
				.inner_margin(Margin::symmetric(12, 6))
				.show(ui, |ui| {
					for j in jobs {
						ui.horizontal(|ui| {
							ui.spinner();
							ui.label(j);
						});
					}
				});
		});
}
