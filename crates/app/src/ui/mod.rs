//! egui views. [`CadenzaApp`] lays out the panels (dock-style: menu, transport, browser
//! left, inspector right, mixer and piano roll at the bottom, arrange center; F5–F9 toggle
//! them, F11 maximizes the focused one), routes shortcuts to the focused panel and
//! handles drops from the OS file manager; the views only read [`AppState`] and call its
//! methods, so every edit becomes a `Command`.

pub mod arrange;
pub mod automation;
pub mod browser;
pub mod dnd;
pub mod inspector;
pub mod mixer;
pub mod piano_roll;
pub mod transport;
pub mod widgets;

use std::path::PathBuf;
use std::time::Instant;

use cadenza_model::TrackKind;
use egui::{
	Align, Align2, Frame, Key, KeyboardShortcut, Layout, Margin, Modifiers, Panel, Rect, RichText,
	Ui, ViewportCommand, vec2,
};

use crate::jobs::FileAction;
use crate::state::dnd::{DropTarget, Payload};
use crate::state::layout::{PanelKind, SHORTCUTS};
use crate::state::notices::Level;
use crate::state::{AppState, Pending, edits};
use crate::theme;

/// Key of the per-user preferences in the eframe storage.
pub const PREFS_KEY: &str = "cadenza-prefs";

/// The whole application window.
pub struct CadenzaApp {
	pub state: AppState,
	arrange: arrange::ArrangeUi,
	browser: browser::BrowserUi,
	piano: piano_roll::PianoUi,
	title: String,
	themed: bool,
	/// Whether the piano roll was visible last frame (to release previews on hide).
	piano_was_visible: bool,
}

impl CadenzaApp {
	/// The real app: audio engine on, repaints woken by background threads, preferences
	/// restored from `storage`.
	pub fn new(
		ctx: &egui::Context,
		storage: Option<&dyn eframe::Storage>,
		project: Option<PathBuf>,
	) -> Self {
		let waker_ctx = ctx.clone();
		let state = AppState::new(Some(std::sync::Arc::new(move || {
			waker_ctx.request_repaint()
		})));
		let mut app = Self::with_state(state);
		let prefs = storage
			.and_then(|s| eframe::get_value::<crate::state::layout::Prefs>(s, PREFS_KEY))
			.unwrap_or_default();
		app.state.apply_prefs(prefs);
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
			piano: piano_roll::PianoUi::default(),
			title: String::new(),
			themed: false,
			piano_was_visible: false,
		}
	}

	/// Automation lane `address` in the arrange view in the last frame (headless tests).
	pub fn lane_rect(&self, address: &cadenza_core::ParamAddress) -> Option<Rect> {
		self.arrange.lane_rect(self.state.view.scroll_y, address)
	}

	/// Note grid of the piano roll in the last frame (headless tests).
	pub fn piano_grid(&self) -> Option<Rect> {
		self.piano.grid_rect
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
		let focus_before = state.view.panels.focus;

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

		let panels = state.view.panels.clone();
		let center = panels.center();
		let docked =
			|p: PanelKind| panels.maximized.is_none() && panels.visible(p) && center != Some(p);
		let mut rects: Vec<(PanelKind, Rect)> = Vec::new();
		if docked(PanelKind::Mixer) {
			let r = Panel::bottom("mixer")
				.resizable(true)
				.default_size(470.0)
				.size_range(260.0..=760.0)
				.frame(
					Frame::new()
						.fill(theme::BG_DEEP)
						.inner_margin(Margin::symmetric(0, 4)),
				)
				.show(ui, |ui| mixer::show(ui, state));
			rects.push((PanelKind::Mixer, r.response.rect));
		}
		if docked(PanelKind::PianoRoll) {
			let piano = &mut self.piano;
			let r = Panel::bottom("piano-roll")
				.resizable(true)
				.default_size(360.0)
				.size_range(200.0..=900.0)
				.frame(Frame::new().fill(theme::BG_DEEP))
				.show(ui, |ui| piano.show(ui, state));
			rects.push((PanelKind::PianoRoll, r.response.rect));
		}
		if docked(PanelKind::Browser) {
			let browser = &mut self.browser;
			let r = Panel::left("browser")
				.resizable(true)
				.default_size(240.0)
				.size_range(180.0..=460.0)
				.frame(
					Frame::new()
						.fill(theme::BG_PANEL)
						.inner_margin(Margin::symmetric(8, 4)),
				)
				.show(ui, |ui| browser.show(ui, state));
			rects.push((PanelKind::Browser, r.response.rect));
		}
		if docked(PanelKind::Inspector) {
			let r = Panel::right("inspector")
				.resizable(true)
				.default_size(270.0)
				.size_range(220.0..=480.0)
				.frame(
					Frame::new()
						.fill(theme::BG_PANEL)
						.inner_margin(Margin::symmetric(8, 4)),
				)
				.show(ui, |ui| inspector::show(ui, state));
			rects.push((PanelKind::Inspector, r.response.rect));
		}
		let arrange = &mut self.arrange;
		let piano = &mut self.piano;
		let browser = &mut self.browser;
		let central = egui::CentralPanel::no_frame().show(ui, |ui| match center {
			Some(PanelKind::Arrange) => arrange.show(ui, state),
			Some(PanelKind::PianoRoll) => piano.show(ui, state),
			Some(PanelKind::Mixer) => {
				Frame::new()
					.fill(theme::BG_DEEP)
					.inner_margin(Margin::symmetric(0, 4))
					.show(ui, |ui| mixer::show(ui, state));
			}
			Some(PanelKind::Browser) => {
				Frame::new()
					.fill(theme::BG_PANEL)
					.inner_margin(Margin::symmetric(8, 4))
					.show(ui, |ui| browser.show(ui, state));
			}
			Some(PanelKind::Inspector) => {
				Frame::new()
					.fill(theme::BG_PANEL)
					.inner_margin(Margin::symmetric(8, 4))
					.show(ui, |ui| inspector::show(ui, state));
			}
			None => nothing_open(ui),
		});
		if let Some(c) = center {
			rects.push((c, central.response.rect));
		}
		// A view that moved the focus itself this frame (double-click opening the piano
		// roll) wins over the click position.
		if state.view.panels.focus == focus_before {
			track_focus(ui.ctx(), state, &rects);
		}

		let piano_visible = state.view.panels.visible(PanelKind::PianoRoll);
		if self.piano_was_visible && !piano_visible {
			self.piano.release(state);
		}
		self.piano_was_visible = piano_visible;

		self.os_drops(ui.ctx());
		let state = &mut self.state;
		dnd::paint_hint(ui.ctx());
		toasts(ui.ctx(), state);
		unsaved_prompt(ui.ctx(), state);
		help_overlay(ui.ctx(), state);
		busy_overlay(ui.ctx(), state);
	}

	/// Files dropped from the OS file manager: imported where they land in the arrange
	/// view (or at the playhead), projects are opened (asking about unsaved changes).
	fn os_drops(&mut self, ctx: &egui::Context) {
		let dropped: Vec<PathBuf> = ctx.input(|i| {
			i.raw
				.dropped_files
				.iter()
				.map(|f| f.path().to_path_buf())
				.collect()
		});
		if dropped.is_empty() {
			return;
		}
		let target = ctx
			.pointer_latest_pos()
			.and_then(|p| self.arrange.target_at(&self.state, p))
			.filter(|_| self.state.view.panels.visible(PanelKind::Arrange))
			.unwrap_or(DropTarget::Window);
		for path in dropped {
			self.state.drop_payload(&Payload::File(path), target, false);
		}
	}

	fn shortcuts(&mut self, ctx: &egui::Context) {
		let typing = ctx.egui_wants_keyboard_input();
		let cmd = Modifiers::COMMAND;
		let shift_cmd = Modifiers::COMMAND | Modifiers::SHIFT;
		let pressed = |m: Modifiers, k: Key| {
			ctx.input_mut(|i| i.consume_shortcut(&KeyboardShortcut::new(m, k)))
		};
		{
			let s = &mut self.state;
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
			// Windows (FL Studio-like function keys).
			if pressed(Modifiers::NONE, Key::F1) {
				s.view.help = !s.view.help;
			}
			for panel in PanelKind::ALL {
				let key = match panel {
					PanelKind::Arrange => Key::F5,
					PanelKind::Browser => Key::F6,
					PanelKind::PianoRoll => Key::F7,
					PanelKind::Inspector => Key::F8,
					PanelKind::Mixer => Key::F9,
				};
				if pressed(Modifiers::NONE, key)
					|| (panel == PanelKind::Browser && pressed(Modifiers::ALT, Key::F8))
				{
					toggle_panel(s, panel);
				}
			}
			if pressed(Modifiers::NONE, Key::F11) || pressed(cmd, Key::Enter) {
				s.view.panels.toggle_maximize();
			}
			if pressed(Modifiers::NONE, Key::F12) {
				s.view.panels.close_tools();
			}
		}
		if typing {
			return;
		}
		if self.state.view.help {
			if pressed(Modifiers::NONE, Key::Escape) {
				self.state.view.help = false;
			}
			return;
		}
		let piano_focused = self.state.view.panels.focus == Some(PanelKind::PianoRoll)
			&& self.state.view.panels.visible(PanelKind::PianoRoll);
		if piano_focused {
			self.piano.shortcuts(ctx, &mut self.state);
		}
		let s = &mut self.state;
		if pressed(Modifiers::NONE, Key::Space) {
			s.toggle_play();
		}
		if pressed(Modifiers::NONE, Key::Home) || pressed(Modifiers::NONE, Key::Comma) {
			s.seek(cadenza_core::Tick::ZERO);
			s.view.timeline.scroll_beats = 0.0;
			s.view.piano.timeline.scroll_beats = 0.0;
		}
		if pressed(Modifiers::NONE, Key::End) {
			let end = s.song_end();
			s.seek(end);
			s.view
				.timeline
				.reveal(end.beats(), self.arrange.lanes_width);
		}
		let (width, height) = (self.arrange.lanes_width, self.arrange.lanes_height);
		let page = |s: &mut AppState, dir: f32| {
			if piano_focused {
				s.view.piano.timeline.page(dir, width);
			} else {
				s.view.timeline.page(dir, width);
			}
		};
		if pressed(Modifiers::SHIFT, Key::PageUp) {
			s.view.scroll_y = (s.view.scroll_y - height * 0.9).max(0.0);
		} else if pressed(Modifiers::NONE, Key::PageUp) {
			page(s, -1.0);
		}
		if pressed(Modifiers::SHIFT, Key::PageDown) {
			s.view.scroll_y += height * 0.9;
		} else if pressed(Modifiers::NONE, Key::PageDown) {
			page(s, 1.0);
		}
		if pressed(Modifiers::SHIFT, Key::Z) {
			arrange::zoom_to_fit(s, width);
		} else if pressed(Modifiers::NONE, Key::Z) {
			arrange::zoom_to_selection(s, width, height);
		}
		if piano_focused {
			return;
		}
		if pressed(cmd, Key::D) {
			duplicate_selection(s);
		}
		if (pressed(Modifiers::NONE, Key::Delete) || pressed(Modifiers::NONE, Key::Backspace))
			&& !s.delete_selected_points()
		{
			delete_selection(s);
		}
		if pressed(Modifiers::NONE, Key::A) {
			s.toggle_lanes();
		}
		if pressed(Modifiers::NONE, Key::S) {
			split_at_playhead(s);
		}
		if pressed(Modifiers::NONE, Key::Escape) {
			s.selection.clear_clips();
			s.view.automation.selected.clear();
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

	fn save(&mut self, storage: &mut dyn eframe::Storage) {
		eframe::set_value(storage, PREFS_KEY, &self.state.prefs());
	}
}

/// F5..F9 / View menu: toggles a panel. Showing the piano roll without an open clip
/// opens the selected MIDI clip (if any).
pub fn toggle_panel(state: &mut AppState, panel: PanelKind) {
	if panel == PanelKind::PianoRoll && !state.view.panels.visible(panel) {
		let selected = state.selection.clips.iter().copied().find(|c| {
			state
				.project()
				.find_clip(*c)
				.is_some_and(|(_, c)| c.is_midi())
		});
		if let Some(clip) = selected.filter(|_| state.view.piano.clip.is_none()) {
			state.open_piano_roll(clip);
			return;
		}
		state.view.panels.open_piano_roll();
		return;
	}
	state.view.panels.toggle(panel);
}

/// The last panel pressed with the mouse gets the keyboard focus (piano roll edits,
/// maximize).
fn track_focus(ctx: &egui::Context, state: &mut AppState, rects: &[(PanelKind, Rect)]) {
	if !ctx.input(|i| i.pointer.any_pressed()) {
		return;
	}
	let Some(pos) = ctx.pointer_interact_pos() else {
		return;
	};
	if let Some((panel, _)) = rects.iter().find(|(_, r)| r.contains(pos)) {
		state.view.panels.focus = Some(*panel);
	}
}

fn nothing_open(ui: &mut Ui) {
	let rect = ui.available_rect_before_wrap();
	ui.painter().rect_filled(rect, 0.0, theme::BG_DEEP);
	ui.painter().text(
		rect.center(),
		Align2::CENTER_CENTER,
		"All panels are hidden — F5 shows the arrange view, F1 lists the shortcuts",
		theme::small(),
		theme::TEXT_FAINT,
	);
}

/// Help → Keyboard shortcuts (F1): every shortcut, grouped by section in two columns.
fn help_overlay(ctx: &egui::Context, state: &mut AppState) {
	if !state.view.help {
		return;
	}
	let modal = egui::Modal::new(egui::Id::new("help-shortcuts"))
		.frame(
			Frame::new()
				.fill(theme::BG_RAISED)
				.stroke(egui::Stroke::new(1.0, theme::BORDER_STRONG))
				.corner_radius(6)
				.inner_margin(Margin::same(18)),
		)
		.show(ctx, |ui| {
			ui.set_width(860.0);
			ui.horizontal(|ui| {
				ui.heading("Keyboard shortcuts");
				ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
					if ui.button("Close").clicked() {
						state.view.help = false;
					}
					ui.label(
						RichText::new("F1 or Esc to close")
							.font(theme::small())
							.color(theme::TEXT_FAINT),
					);
				});
			});
			ui.add_space(8.0);
			let mut sections: Vec<&str> = Vec::new();
			for s in SHORTCUTS {
				if !sections.contains(&s.section) {
					sections.push(s.section);
				}
			}
			let half = sections.len().div_ceil(2);
			ui.columns(2, |cols| {
				for (c, col) in cols.iter_mut().enumerate() {
					for section in sections.iter().skip(c * half).take(half) {
						col.label(
							RichText::new(section.to_uppercase())
								.font(theme::small())
								.color(theme::ACCENT),
						);
						egui::Grid::new(("help", *section))
							.num_columns(2)
							.min_col_width(200.0)
							.spacing(vec2(14.0, 3.0))
							.show(col, |ui| {
								for s in SHORTCUTS.iter().filter(|s| s.section == *section) {
									ui.label(
										RichText::new(s.keys)
											.font(theme::mono(11.0))
											.color(theme::TEXT),
									);
									ui.label(
										RichText::new(s.action)
											.font(theme::small())
											.color(theme::TEXT_DIM),
									);
									ui.end_row();
								}
							});
						col.add_space(10.0);
					}
				}
			});
		});
	if modal.should_close() || ctx.input(|i| i.key_pressed(Key::Escape)) {
		state.view.help = false;
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
			ui.set_min_width(240.0);
			for panel in PanelKind::ALL {
				let on = state.view.panels.visible(panel);
				if check_item(ui, on, panel.name(), panel.shortcut()) {
					toggle_panel(state, panel);
				}
			}
			ui.separator();
			let maximized = state.view.panels.maximized.is_some();
			if check_item(ui, maximized, "Maximize focused panel", "F11") {
				state.view.panels.toggle_maximize();
			}
			if check_item(ui, false, "Close tool panels", "F12") {
				state.view.panels.close_tools();
			}
			ui.separator();
			if check_item(ui, state.view.snap, "Snap to grid", "") {
				state.view.snap = !state.view.snap;
			}
			ui.menu_button("      Follow playhead", |ui| {
				use crate::state::layout::FollowMode;
				for m in [FollowMode::Off, FollowMode::Page, FollowMode::Continuous] {
					if check_item(ui, state.view.follow == m, m.name(), "") {
						state.view.follow = m;
					}
				}
			});
			if check_item(ui, false, "Zoom to fit", "Shift+Z") {
				arrange::zoom_to_fit(state, 1200.0);
			}
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
		ui.menu_button("Help", |ui| {
			if shortcut_button(ui, "Keyboard shortcuts", "F1") {
				state.view.help = true;
			}
		});
	});
}

/// Menu entry with a checkmark and a shortcut hint.
fn check_item(ui: &mut Ui, on: bool, label: &str, shortcut: &str) -> bool {
	let text = format!("{}  {label}", if on { "✔" } else { "   " });
	ui.add(egui::Button::new(text).shortcut_text(shortcut))
		.clicked()
}

fn shortcut_button(ui: &mut Ui, label: &str, shortcut: &str) -> bool {
	ui.add(egui::Button::new(label).shortcut_text(shortcut))
		.clicked()
}

fn status_bar(ui: &mut Ui, state: &mut AppState) {
	ui.horizontal_centered(|ui| {
		let dirty = if state.dirty {
			"unsaved changes"
		} else {
			"saved"
		};
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
							ui.set_max_width(440.0);
							ui.with_layout(Layout::left_to_right(Align::Min), |ui| {
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
	egui::Modal::new(egui::Id::new("unsaved"))
		.frame(
			Frame::new()
				.fill(theme::BG_RAISED)
				.stroke(egui::Stroke::new(1.0, theme::BORDER_STRONG))
				.corner_radius(6)
				.inner_margin(Margin::same(16)),
		)
		.show(ctx, |ui| {
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
