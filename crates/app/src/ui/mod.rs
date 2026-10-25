//! egui views. [`CadenzaApp`] lays out the panels (dock-style: menu, transport, browser
//! left, inspector right, mixer and piano roll at the bottom, arrange center; F5–F9 toggle
//! them, F11 maximizes the focused one), routes shortcuts to the focused panel and
//! handles drops from the OS file manager; the views only read [`AppState`] and call its
//! methods, so every edit becomes a `Command`.

pub mod arrange;
pub mod audio_editor;
pub mod automation;
pub mod browser;
pub mod chat;
pub mod device_ai;
pub mod device_editor;
pub mod device_kit;
pub mod devices;
pub mod dnd;
pub mod inspector;
pub mod instrument_editor;
pub mod library;
pub mod midi_learn;
pub mod mix_assistant;
pub mod mixer;
pub mod note_editor;
pub mod piano_roll;
pub mod project_dialogs;
pub mod record;
pub mod start;
pub mod to_midi;
pub mod transport;
pub mod widgets;
pub mod windows;

use crate::i18n::{tr, trf, trn};
use std::path::PathBuf;
use std::time::Instant;

use cadenza_model::TrackKind;
use egui::{
	Align, Align2, Frame, Key, KeyboardShortcut, Layout, Margin, Modifiers, Panel, Rect, RichText,
	Ui, ViewportCommand, vec2,
};

use crate::jobs::FileAction;
use crate::state::arrange_tool::ArrangeTool;
use crate::state::dnd::{DropTarget, Payload};
use crate::state::layout::{PanelKind, SHORTCUTS};
use crate::state::notices::Level;
use crate::state::{AppState, Pending, edits, playlist};
use crate::theme;

/// Key of the per-user preferences in the eframe storage.
pub const PREFS_KEY: &str = "cadenza-prefs";

/// The whole application window.
pub struct CadenzaApp {
	pub state: AppState,
	arrange: arrange::ArrangeUi,
	browser: browser::BrowserUi,
	piano: piano_roll::PianoUi,
	audio_editor: audio_editor::AudioEditorUi,
	device: device_editor::DeviceEditorUi,
	windows: windows::FloatingUi,
	chat: chat::ChatUi,
	title: String,
	/// Whether the piano roll was visible last frame (to release previews on hide).
	piano_was_visible: bool,
	/// A drag and drop payload was in flight last frame (the browser overlay closes after
	/// a drop).
	dragging: bool,
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
		app.state.enable_safety(crate::state::safety::state_dir());
		match project {
			Some(path) => {
				app.state.open(&path);
			}
			None => {
				app.state.view.start_screen = true;
				app.state.check_untitled_recovery();
			}
		}
		if let Some(notice) = theme::apply(ctx) {
			app.state.notices.warn(notice);
		}
		app
	}

	/// Around an existing state (tests, headless rendering).
	pub fn with_state(state: AppState) -> Self {
		Self {
			state,
			arrange: arrange::ArrangeUi::default(),
			browser: browser::BrowserUi::default(),
			piano: piano_roll::PianoUi::default(),
			audio_editor: audio_editor::AudioEditorUi::default(),
			device: device_editor::DeviceEditorUi::default(),
			windows: windows::FloatingUi::default(),
			chat: chat::ChatUi::default(),
			title: String::new(),
			piano_was_visible: false,
			dragging: false,
		}
	}

	/// Automation lane `address` in the arrange view in the last frame (headless tests).
	pub fn lane_rect(&self, address: &cadenza_core::ParamAddress) -> Option<Rect> {
		self.arrange.lane_rect(self.state.view.scroll_y, address)
	}

	/// Clip row of track `index` in the arrange view in the last frame (headless tests).
	pub fn track_rect(&self, index: usize) -> Option<Rect> {
		self.arrange.track_rect(self.state.view.scroll_y, index)
	}

	/// Waveform area of the audio editor in the last frame (headless tests).
	pub fn audio_wave(&self) -> Option<Rect> {
		self.audio_editor.wave_rect
	}

	/// The device editor in front ([`AppState::device_editor_target`]): its window's,
	/// else the dock's.
	fn active_device_ui(&self) -> &device_editor::DeviceEditorUi {
		self.state
			.device_editor_target()
			.and_then(|r| self.windows.device_ui(r.device))
			.unwrap_or(&self.device)
	}

	/// Waveform of the instrument editor in the last frame (headless tests).
	pub fn instrument_wave(&self) -> Option<Rect> {
		self.active_device_ui().instrument.wave_rect
	}

	/// The header IN / OUT meter levels of the device editor in front (headless tests).
	pub fn device_io_levels(&self) -> crate::state::device_display::IoLevels {
		self.active_device_ui().levels()
	}

	/// The device editor's body in the last frame (headless tests).
	pub fn device_body_rect(&self) -> Option<Rect> {
		self.active_device_ui().body_rect
	}

	/// The sampler editor's zone map and the window it showed in the last frame
	/// (headless tests).
	pub fn zone_map(&self) -> Option<(Rect, crate::state::sampler_editor::MapWindow)> {
		self.active_device_ui().instrument.zones.map
	}

	/// Pad grid of the drum sampler editor in the last frame (headless tests).
	pub fn pad_grid(&self) -> Option<Rect> {
		self.active_device_ui().instrument.pad_rect
	}

	/// The floating window of `item` in the last frame (headless tests).
	pub fn window_rect(&self, item: crate::state::windows::WindowItem) -> Option<Rect> {
		self.windows.rect(item)
	}

	/// Note grid of the piano roll in the last frame (headless tests).
	pub fn piano_grid(&self) -> Option<Rect> {
		self.piano.grid_rect
	}

	/// The piano roll's event lane (velocity or controller) of the last frame.
	pub fn piano_event_lane(&self) -> Option<Rect> {
		self.piano.lane_rect
	}

	/// Per-frame logic that does not draw (eframe calls it before [`Self::ui`]).
	pub fn logic(&mut self, ctx: &egui::Context) {
		let animating = self.state.tick(Instant::now());
		if animating {
			ctx.request_repaint();
		} else if self.state.midi_listening() {
			// Hardware controllers do not wake the UI: poll what they did.
			ctx.request_repaint_after(std::time::Duration::from_millis(40));
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
				self.state.shutdown();
			}
		}
		if self.state.quit_confirmed {
			self.state.stop();
			self.state.shutdown();
			ctx.send_viewport_cmd(ViewportCommand::Close);
		}
	}

	/// Draws the whole window into `ui`.
	pub fn ui(&mut self, ui: &mut Ui) {
		if theme::needs_apply(ui.ctx()) {
			if let Some(notice) = theme::apply(ui.ctx()) {
				self.state.notices.warn(notice);
			}
			// The weight families exist from the next pass on: draw nothing with this one.
			ui.ctx().request_discard("Cadenza fonts");
			return;
		}
		let screen = ui.ctx().content_rect();
		self.state.view.responsive = crate::state::layout::Responsive::for_window(
			screen.width(),
			screen.height(),
			self.state.view.panels.chat_pinned,
		);
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
		// The start screen is a page of its own: no workspace behind it.
		if state.view.start_screen {
			egui::CentralPanel::no_frame().show(ui, |ui| start::show(ui, state));
			toasts(ui.ctx(), state);
			unsaved_prompt(ui.ctx(), state);
			project_dialogs::show(ui.ctx(), state);
			record::audio_settings(ui.ctx(), state);
			help_overlay(ui.ctx(), state);
			busy_overlay(ui.ctx(), state);
			return;
		}
		Panel::top("transport")
			.exact_size(theme::TRANSPORT_HEIGHT)
			.frame(
				Frame::new()
					.fill(theme::BG_PANEL)
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

		// While the AI edits, the editing panels are read-only (see `state::ai`).
		let locked = state.ai.busy();
		let panels = state.view.panels.clone();
		let resp = state.view.responsive;
		let center = panels.center();
		let docked =
			|p: PanelKind| panels.maximized.is_none() && panels.visible(p) && center != Some(p);
		// Side panels drawn as overlays this frame (design 12: narrow windows).
		let overlay = |p: PanelKind| docked(p) && resp.overlay(p);
		let side_docked = |p: PanelKind| docked(p) && !resp.overlay(p);
		let mut rects: Vec<(PanelKind, Rect)> = Vec::new();
		// Room the other docked side panels take at least (the arrange keeps its minimum).
		let side_min = |except: PanelKind| -> f32 {
			use crate::state::layout::{BROWSER_WIDTH, CHAT_WIDTH, INSPECTOR_WIDTH, RAIL_WIDTH};
			let mut w = 0.0;
			if except != PanelKind::Browser && docked(PanelKind::Browser) {
				w += if resp.browser_rail {
					RAIL_WIDTH
				} else {
					BROWSER_WIDTH.0
				};
			}
			if except != PanelKind::Inspector && side_docked(PanelKind::Inspector) {
				w += INSPECTOR_WIDTH.0;
			}
			if except != PanelKind::Chat && side_docked(PanelKind::Chat) {
				w += CHAT_WIDTH.0;
			}
			w
		};
		// The overlays span from under the transport to the status bar.
		let body = ui.available_rect_before_wrap();
		// Docked right panels run the full height (the inspector was squeezed above the
		// mixer before), the bottom docks fill the rest.
		if side_docked(PanelKind::Chat) {
			let chat = &mut self.chat;
			let (min, default, max) =
				resp.side_range(crate::state::layout::CHAT_WIDTH, side_min(PanelKind::Chat));
			let wanted = if state.mix.open {
				crate::state::layout::CHAT_MIX_WIDTH.min(max)
			} else {
				default
			};
			let r = Panel::right("ai-chat")
				.resizable(true)
				.default_size(wanted)
				.size_range(min..=max)
				.frame(
					Frame::new()
						.fill(theme::BG_PANEL)
						.inner_margin(Margin::symmetric(8, 4))
						.stroke(egui::Stroke::new(1.0, theme::BORDER)),
				)
				.show(ui, |ui| chat.show(ui, state));
			rects.push((PanelKind::Chat, r.response.rect));
		} else if !overlay(PanelKind::Chat) {
			// The settings dialog also opens from errors and menus with the panel hidden.
			self.chat.settings_dialog(ui.ctx(), state);
		}
		if side_docked(PanelKind::Inspector) {
			let (min, default, max) = resp.side_range(
				crate::state::layout::INSPECTOR_WIDTH,
				side_min(PanelKind::Inspector),
			);
			let r = Panel::right("inspector")
				.resizable(true)
				.default_size(default)
				.size_range(min..=max)
				.frame(
					Frame::new()
						.fill(theme::BG_PANEL)
						.inner_margin(Margin::symmetric(8, 4)),
				)
				.show(ui, |ui| {
					if locked {
						ui.disable();
					}
					inspector::show(ui, state)
				});
			rects.push((PanelKind::Inspector, r.response.rect));
		}
		let bottom_h = ui.available_rect_before_wrap().height();
		if docked(PanelKind::Mixer) {
			let (min, default, max) = crate::state::layout::Responsive::mixer_height(bottom_h);
			let r = Panel::bottom("mixer")
				.resizable(true)
				.default_size(default)
				.size_range(min..=max)
				.frame(
					Frame::new()
						.fill(theme::BG_DEEP)
						.inner_margin(Margin::symmetric(0, 4)),
				)
				.show(ui, |ui| {
					if locked {
						ui.disable();
					}
					mixer::show(ui, state)
				});
			rects.push((PanelKind::Mixer, r.response.rect));
		}
		if docked(PanelKind::PianoRoll) {
			let piano = &mut self.piano;
			let audio = &mut self.audio_editor;
			let device = &mut self.device;
			let (min, default, max) = crate::state::layout::Responsive::editor_height(bottom_h);
			let r = Panel::bottom("piano-roll")
				.resizable(true)
				.default_size(default)
				.size_range(min..=max)
				.frame(Frame::new().fill(theme::BG_DEEP))
				.show(ui, |ui| {
					if locked {
						ui.disable();
					}
					if state.view.device_editor {
						device.show(ui, state)
					} else if state.view.audio_editor {
						audio.show(ui, state)
					} else {
						piano.show(ui, state)
					}
				});
			rects.push((PanelKind::PianoRoll, r.response.rect));
		}
		let mut rail_rect = None;
		if docked(PanelKind::Browser) && resp.browser_rail {
			let browser = &mut self.browser;
			let r = Panel::left("browser-rail")
				.resizable(false)
				.exact_size(crate::state::layout::RAIL_WIDTH)
				.frame(
					Frame::new()
						.fill(theme::BG_PANEL)
						.stroke(egui::Stroke::new(1.0, theme::BORDER)),
				)
				.show(ui, |ui| {
					if locked {
						ui.disable();
					}
					browser.rail(ui, state)
				});
			rail_rect = Some(r.response.rect);
			rects.push((PanelKind::Browser, r.response.rect));
		} else if side_docked(PanelKind::Browser) {
			let browser = &mut self.browser;
			// The Library tab is a wide panel of its own (its size is remembered apart).
			let full = ui.available_width();
			let room = (full - crate::state::layout::ARRANGE_MIN_WIDTH).max(260.0);
			let (id, default, min, max) = if browser.library_open() {
				(
					"browser-library",
					(full * 0.7).min(1140.0).min(room),
					260.0,
					room.max(460.0f32.min(room)),
				)
			} else {
				let (min, default, max) = resp.side_range(
					crate::state::layout::BROWSER_WIDTH,
					side_min(PanelKind::Browser),
				);
				("browser", default, min, max)
			};
			let r = Panel::left(id)
				.resizable(true)
				.default_size(default)
				.size_range(min..=max)
				.frame(
					Frame::new()
						.fill(theme::BG_PANEL)
						.inner_margin(Margin::symmetric(8, 4)),
				)
				.show(ui, |ui| {
					if locked {
						ui.disable();
					}
					browser.show(ui, state)
				});
			rects.push((PanelKind::Browser, r.response.rect));
		}
		let arrange = &mut self.arrange;
		let piano = &mut self.piano;
		let audio = &mut self.audio_editor;
		let device = &mut self.device;
		let browser = &mut self.browser;
		let chat = &mut self.chat;
		let central = egui::CentralPanel::no_frame().show(ui, |ui| {
			if locked && center != Some(PanelKind::Chat) {
				ui.disable();
			}
			match center {
				Some(PanelKind::Arrange) => arrange.show(ui, state),
				Some(PanelKind::PianoRoll) if state.view.device_editor => device.show(ui, state),
				Some(PanelKind::PianoRoll) if state.view.audio_editor => audio.show(ui, state),
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
				Some(PanelKind::Chat) => {
					Frame::new()
						.fill(theme::BG_PANEL)
						.inner_margin(Margin::symmetric(8, 4))
						.show(ui, |ui| chat.show(ui, state));
				}
				None => nothing_open(ui),
			}
		});
		if let Some(c) = center {
			rects.push((c, central.response.rect));
		}
		let overlays = OverlayPanels {
			browser: overlay(PanelKind::Browser) && panels.browser_overlay,
			inspector: overlay(PanelKind::Inspector),
			chat: overlay(PanelKind::Chat),
		};
		self.overlays(ui.ctx(), body, rail_rect, overlays, locked, &mut rects);
		let state = &mut self.state;
		self.windows.show(ui.ctx(), state, locked);
		// A view that moved the focus itself this frame (double-click opening the piano
		// roll) wins over the click position; a click in a floating window is its own.
		if state.view.panels.focus == focus_before && !self.windows.clicked_inside {
			track_focus(ui.ctx(), state, &rects);
		}

		let piano_visible = state.view.panels.visible(PanelKind::PianoRoll);
		if self.piano_was_visible && !piano_visible {
			self.piano.release(state);
			self.device.release(state);
		}
		self.piano_was_visible = piano_visible;

		self.os_drops(ui.ctx());
		let state = &mut self.state;
		dnd::paint_hint(ui.ctx());
		toasts(ui.ctx(), state);
		unsaved_prompt(ui.ctx(), state);
		project_dialogs::show(ui.ctx(), state);
		to_midi::show(ui.ctx(), state);
		record::audio_settings(ui.ctx(), state);
		midi_learn::show(ui.ctx(), state);
		help_overlay(ui.ctx(), state);
		busy_overlay(ui.ctx(), state);
	}

	/// The side panels drawn over the workspace in narrow windows (design 12a): the browser
	/// next to its rail, the inspector and the AI drawer from the right. `body` is the area
	/// under the transport, above the status bar.
	fn overlays(
		&mut self,
		ctx: &egui::Context,
		body: Rect,
		rail: Option<Rect>,
		which: OverlayPanels,
		locked: bool,
		rects: &mut Vec<(PanelKind, Rect)>,
	) {
		use crate::state::layout::{BROWSER_WIDTH, CHAT_MIX_WIDTH, CHAT_WIDTH, INSPECTOR_WIDTH};
		let resp = self.state.view.responsive;
		let dragging = egui::DragAndDrop::has_any_payload(ctx);
		// The browser overlay closes after a drop (the payload is gone after a release).
		if self.dragging && !dragging && self.state.view.panels.browser_overlay {
			self.state.view.panels.browser_overlay = false;
		}
		self.dragging = dragging;
		let frame = Frame::new()
			.fill(theme::BG_PANEL)
			.stroke(egui::Stroke::new(1.0, theme::BORDER_STRONG))
			.shadow(theme::FLOATING_SHADOW)
			.inner_margin(Margin::symmetric(8, 4));
		let show = |ctx: &egui::Context, id: &str, rect: Rect, add: &mut dyn FnMut(&mut Ui)| {
			egui::Area::new(egui::Id::new(("overlay", id)))
				.order(egui::Order::Middle)
				.fixed_pos(rect.min)
				.constrain(false)
				// Like a docked panel: no click sense of its own over the widgets inside.
				.sense(egui::Sense::hover())
				.show(ctx, |ui| {
					frame.show(ui, |ui| {
						let inner = rect.size() - vec2(16.0, 8.0) - vec2(2.0, 2.0);
						ui.set_min_size(inner);
						ui.set_max_size(inner);
						if locked {
							ui.disable();
						}
						add(ui);
					});
				})
				.response
				.rect
		};
		let mut right = body.right();
		if which.chat {
			let wanted = if self.state.mix.open {
				CHAT_MIX_WIDTH
			} else {
				CHAT_WIDTH.1
			};
			let w = resp.overlay_width(wanted, CHAT_WIDTH.0);
			let rect = Rect::from_min_max(
				egui::pos2(right - w, body.top()),
				egui::pos2(right, body.bottom()),
			);
			let chat = &mut self.chat;
			let state = &mut self.state;
			let r = show(ctx, "chat", rect, &mut |ui| chat.show(ui, state));
			right = rect.left();
			rects.insert(0, (PanelKind::Chat, r));
		}
		if which.inspector {
			let w = resp.overlay_width(INSPECTOR_WIDTH.1, INSPECTOR_WIDTH.0);
			let rect = Rect::from_min_max(
				egui::pos2(right - w, body.top()),
				egui::pos2(right, body.bottom()),
			);
			let state = &mut self.state;
			let r = show(ctx, "inspector", rect, &mut |ui| inspector::show(ui, state));
			rects.insert(0, (PanelKind::Inspector, r));
		}
		if which.browser {
			let left = rail.map_or(body.left(), |r| r.right());
			let wanted = if self.browser.library_open() {
				900.0
			} else {
				BROWSER_WIDTH.1
			};
			let w = resp.overlay_width(wanted, BROWSER_WIDTH.0);
			let rect = Rect::from_min_max(
				egui::pos2(left, body.top()),
				egui::pos2(left + w, body.bottom()),
			);
			let browser = &mut self.browser;
			let state = &mut self.state;
			let r = show(ctx, "browser", rect, &mut |ui| browser.show(ui, state));
			rects.insert(0, (PanelKind::Browser, r));
			// A press on the workspace (not the rail, a popup or a window) closes it.
			if !dragging
				&& ctx.input(|i| i.pointer.any_pressed())
				&& let Some(pos) = ctx.pointer_interact_pos()
				&& !r.contains(pos)
				&& !rail.is_some_and(|rr| rr.contains(pos))
				&& ctx
					.layer_id_at(pos)
					.is_none_or(|l| l.order == egui::Order::Background)
			{
				self.state.view.panels.browser_overlay = false;
			}
		}
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
		let pointer = ctx.pointer_latest_pos();
		let dock_shown =
			self.state.view.device_editor && self.state.view.panels.visible(PanelKind::PianoRoll);
		let editor_target = pointer.and_then(|p| {
			self.windows
				.target_at(p)
				.or_else(|| self.device.target_at(p).filter(|_| dock_shown))
		});
		let arrange_target = pointer
			.and_then(|p| self.arrange.target_at(&self.state, p))
			.filter(|_| self.state.view.panels.visible(PanelKind::Arrange));
		let target = editor_target
			.or(arrange_target)
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
				s.open_export_dialog();
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
					PanelKind::Chat => Key::F10,
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
		// A focused floating window has the keyboard (Esc closes it).
		let window_focused = self.windows.shortcuts(ctx, &mut self.state);
		if !window_focused {
			close_overlay_on_escape(ctx, &mut self.state);
		}
		let piano_focused = !window_focused
			&& self.state.view.panels.focus == Some(PanelKind::PianoRoll)
			&& self.state.view.panels.visible(PanelKind::PianoRoll);
		if piano_focused && !self.state.view.device_editor {
			if self.state.view.audio_editor {
				self.audio_editor.shortcuts(ctx, &mut self.state);
			} else {
				self.piano.shortcuts(ctx, &mut self.state);
			}
		} else if piano_focused && let Some(r) = self.state.view.instrument.device {
			self.device.shortcuts(ctx, &mut self.state, r);
		}
		// Ctrl+V pastes onto the track under the pointer when it is over a lane.
		let paste_anchor = ctx.pointer_hover_pos().and_then(|pos| {
			match self.arrange.target_at(&self.state, pos) {
				Some(DropTarget::Lane { track, .. }) => Some(track),
				_ => None,
			}
		});
		let s = &mut self.state;
		if pressed(Modifiers::NONE, Key::Space) {
			s.toggle_play();
		}
		if pressed(cmd, Key::R) {
			s.toggle_record();
		} else if pressed(Modifiers::NONE, Key::R) {
			s.toggle_record_mode();
		}
		if pressed(cmd, Key::L) {
			s.toggle_loop();
		} else if pressed(Modifiers::NONE, Key::L) {
			s.toggle_play_mode();
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
		if piano_focused || window_focused {
			return;
		}
		// Copies of MIDI clips are linked (FL pattern instances); Shift = independent.
		if pressed(shift_cmd, Key::D) {
			duplicate_selection(s, false);
		} else if pressed(cmd, Key::D) {
			duplicate_selection(s, true);
		}
		if pressed(cmd, Key::C) {
			copy_clips(s, false);
		}
		if pressed(cmd, Key::X) {
			copy_clips(s, true);
		}
		if pressed(shift_cmd, Key::V) {
			paste_clips(s, paste_anchor, false);
		} else if pressed(cmd, Key::V) {
			paste_clips(s, paste_anchor, true);
		}
		if pressed(shift_cmd, Key::B) {
			clone_selection(s, false);
		} else if pressed(cmd, Key::B) {
			clone_selection(s, true);
		}
		if pressed(cmd, Key::G) {
			s.consolidate_selection();
		}
		if pressed(Modifiers::NONE, Key::Q) {
			quantize_clip_starts(s);
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
		for (key, tool) in [
			(Key::V, ArrangeTool::Select),
			(Key::D, ArrangeTool::Draw),
			(Key::B, ArrangeTool::Paint),
			(Key::C, ArrangeTool::Split),
			(Key::T, ArrangeTool::Mute),
		] {
			if pressed(Modifiers::NONE, key) {
				s.view.arrange_tool = tool;
			}
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

	fn on_exit(&mut self) {
		self.state.shutdown();
	}
}

/// F5..F9 / View menu: toggles a panel. F7 opens the piano roll for the selected
/// instrument track (see [`AppState::toggle_piano_roll`]).
pub fn toggle_panel(state: &mut AppState, panel: PanelKind) {
	if panel == PanelKind::PianoRoll {
		state.toggle_piano_roll();
		return;
	}
	toggle_panel_plain(state, panel);
}

/// Toggles a panel without the piano roll's clip logic; in rail mode the browser key opens
/// and closes the overlay (showing the rail if it was hidden).
fn toggle_panel_plain(state: &mut AppState, panel: PanelKind) {
	let p = &mut state.view.panels;
	if panel == PanelKind::Browser && state.view.responsive.browser_rail && p.maximized.is_none() {
		if p.browser && p.browser_overlay {
			p.browser_overlay = false;
		} else {
			p.browser = true;
			p.browser_overlay = true;
			p.focus = Some(PanelKind::Browser);
		}
		return;
	}
	p.toggle(panel);
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
		tr("All panels are hidden — F5 shows the arrange view, F1 lists the shortcuts"),
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
			ui.set_width((ctx.content_rect().width() - 80.0).clamp(420.0, 1100.0));
			ui.horizontal(|ui| {
				ui.heading(tr("Keyboard shortcuts"));
				ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
					if ui.button(tr("Close")).clicked() {
						state.view.help = false;
					}
					ui.label(
						RichText::new(tr("F1 or Esc to close"))
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
			// Two columns of about the same height (sections are kept whole).
			let rows = |sec: &str| SHORTCUTS.iter().filter(|s| s.section == sec).count() + 2;
			let total: usize = sections.iter().map(|s| rows(s)).sum();
			let mut split = sections.len();
			let mut acc = 0;
			for (i, sec) in sections.iter().enumerate() {
				acc += rows(sec);
				if acc * 2 >= total {
					split = i + 1;
					break;
				}
			}
			let max_h = (ui.ctx().content_rect().height() - 140.0).max(200.0);
			egui::ScrollArea::vertical()
				.max_height(max_h)
				.min_scrolled_height(max_h)
				.auto_shrink([false, true])
				.show(ui, |ui| {
					let two = ui.available_width() >= 760.0;
					ui.columns(if two { 2 } else { 1 }, |cols| {
						for (c, col) in cols.iter_mut().enumerate() {
							let list = if !two {
								&sections[..]
							} else if c == 0 {
								&sections[..split]
							} else {
								&sections[split..]
							};
							for section in list {
								col.label(
									RichText::new(crate::i18n::tr_str(section).to_uppercase())
										.font(theme::small())
										.color(theme::ACCENT),
								);
								// Keys in a fixed column, both sides wrap (long translations).
								let key_w = (col.available_width() * 0.42).clamp(110.0, 230.0);
								for s in SHORTCUTS.iter().filter(|s| s.section == *section) {
									col.horizontal_top(|ui| {
										ui.allocate_ui_with_layout(
											vec2(key_w, 0.0),
											Layout::top_down(Align::Min),
											|ui| {
												ui.set_width(key_w);
												ui.add(
													egui::Label::new(
														RichText::new(crate::i18n::tr_str(s.keys))
															.font(theme::mono(11.0))
															.color(theme::TEXT),
													)
													.wrap(),
												);
											},
										);
										ui.add(
											egui::Label::new(
												RichText::new(crate::i18n::tr_str(s.action))
													.font(theme::small())
													.color(theme::TEXT_DIM),
											)
											.wrap(),
										);
									});
								}
								col.add_space(10.0);
							}
						}
					});
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

/// Duplicates the selected clips after the selection and selects the copies (`linked`:
/// MIDI copies share their content).
pub fn duplicate_selection(state: &mut AppState, linked: bool) {
	let clips: Vec<_> = state.selection.clips.iter().copied().collect();
	if let Some((cmd, ids)) = edits::duplicate_clips(state.project(), &clips, linked)
		&& state.apply(cmd)
	{
		state.selection.clips = ids.into_iter().collect();
	}
}

/// Ctrl+C / Ctrl+X: the selected clips to the arrange clipboard (a cut also deletes
/// them, one undo step).
pub fn copy_clips(state: &mut AppState, cut: bool) {
	let clips: Vec<_> = state.selection.clips.iter().copied().collect();
	if clips.is_empty() {
		return;
	}
	let board = playlist::clipboard(state.project(), &clips, state.position(), cut);
	let n = board.clips.len();
	state.view.clip_board = board;
	if cut {
		if let Some(cmd) = edits::delete_clips(&clips) {
			state.apply(cmd);
		}
	} else {
		state
			.notices
			.info(trn("Copied {n} clip", "Copied {n} clips", n));
	}
}

/// Ctrl+V: pastes the arrange clipboard (at the playhead when it moved since the copy,
/// else after the copied span; onto `anchor` when the pointer is over a lane) and selects
/// the copies (one undo step).
pub fn paste_clips(state: &mut AppState, anchor: Option<cadenza_core::TrackId>, linked: bool) {
	if state.view.clip_board.is_empty() {
		return;
	}
	let playhead = state.position();
	let at = state.view.clip_board.paste_at(playhead, state.is_playing());
	match playlist::paste_clips(state.project(), &state.view.clip_board, at, anchor, linked) {
		Some((cmd, ids)) => {
			if state.apply(cmd) {
				state.view.clip_board.pasted(at, playhead);
				state.selection.clips = ids.into_iter().collect();
			}
		}
		None => state
			.notices
			.info(tr("Nothing to paste here: the copied clips fit no track")),
	}
}

/// The grid step at `at` when snap is on.
pub fn grid_step(state: &AppState, at: cadenza_core::Tick) -> Option<cadenza_core::Tick> {
	state.view.snap.then(|| {
		state
			.view
			.timeline
			.grid_at(at, state.project().meter())
			.step
	})
}

/// Ctrl+B: clones the selection right after itself (grid / whole bars) and selects the
/// copies, so pressing it again keeps appending.
pub fn clone_selection(state: &mut AppState, linked: bool) {
	let clips: Vec<_> = state.selection.clips.iter().copied().collect();
	let start = clips
		.iter()
		.filter_map(|c| state.project().find_clip(*c))
		.map(|(_, c)| c.start)
		.min();
	let Some(start) = start else {
		return;
	};
	let step = grid_step(state, start);
	if let Some((cmd, ids)) = playlist::clone_clips(state.project(), &clips, step, linked)
		&& state.apply(cmd)
	{
		state.selection.clips = ids.into_iter().collect();
	}
}

/// "Make unique": unlinks the clip (and the rest of the selection) from its linked copies.
pub fn make_clips_unique(state: &mut AppState, clip: cadenza_core::ClipId) {
	let mut clips: Vec<_> = state.selection.clips.iter().copied().collect();
	if !clips.contains(&clip) {
		clips = vec![clip];
	}
	match playlist::make_unique(state.project(), &clips) {
		Some(cmd) => {
			state.apply(cmd);
		}
		None => state.notices.info(tr("The clip is not linked")),
	}
}

/// "Chop into…": splits the clip into equal parts (or per beat / bar) and selects them.
pub fn chop_clip(state: &mut AppState, clip: cadenza_core::ClipId, chop: playlist::Chop) {
	match playlist::chop(state.project(), clip, chop) {
		Some((cmd, parts)) => {
			if state.apply(cmd) {
				state.selection.clips = parts.into_iter().collect();
			}
		}
		None => state
			.notices
			.info(tr("The clip is too short to chop that finely")),
	}
}

/// Q: moves the selected clips' starts to the nearest grid line (one undo step).
pub fn quantize_clip_starts(state: &mut AppState) {
	let clips: Vec<_> = state.selection.clips.iter().copied().collect();
	let tl = state.view.timeline;
	let meter = state.project().meter();
	match playlist::quantize_starts(state.project(), &clips, |t| tl.snap(t, meter)) {
		Some(cmd) => {
			state.apply(cmd);
		}
		None if clips.is_empty() => state.notices.info(tr("Select clips to quantize")),
		None => state
			.notices
			.info(tr("The selected clips already start on the grid")),
	}
}

/// Mutes / unmutes the selected clips (or `clip`), one undo step.
pub fn toggle_mute_clips(state: &mut AppState, clip: cadenza_core::ClipId) {
	let mut clips: Vec<_> = state.selection.clips.iter().copied().collect();
	if !clips.contains(&clip) {
		clips = vec![clip];
	} else {
		// The clicked clip decides the new state.
		clips.retain(|c| *c != clip);
		clips.insert(0, clip);
	}
	if let Some(cmd) = playlist::toggle_mute(state.project(), &clips) {
		state.apply(cmd);
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
			.info(tr("Nothing to split: select clips under the playhead")),
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
		ui.menu_button(tr("File"), |ui| {
			ui.set_min_width(250.0);
			if shortcut_button(ui, tr("New"), "Ctrl+N") {
				state.request(Pending::New);
			}
			if shortcut_button(ui, tr("Open…"), "Ctrl+O") {
				state.request(Pending::Open(None));
			}
			ui.menu_button(tr("Open Recent"), |ui| {
				ui.set_min_width(280.0);
				if state.recent.is_empty() {
					ui.label(RichText::new(tr("No recent projects")).color(theme::TEXT_FAINT));
				}
				for path in state.recent.clone() {
					let label = cadenza_model::files::project_stem(&path);
					let r = ui
						.add_enabled(path.exists(), egui::Button::new(label))
						.on_hover_text(path.display().to_string());
					if r.clicked() {
						state.request(Pending::Open(Some(path)));
					}
				}
				if !state.recent.is_empty() {
					ui.separator();
					if ui.button(tr("Clear list")).clicked() {
						state.recent.clear();
					}
				}
			});
			if shortcut_button(ui, tr("Start Screen"), "") {
				state.view.start_screen = true;
			}
			ui.separator();
			if shortcut_button(ui, tr("Save"), "Ctrl+S") {
				state.save();
			}
			if shortcut_button(ui, tr("Save As…"), "Ctrl+Shift+S") {
				state.file_dialog(FileAction::SaveAs);
			}
			ui.separator();
			if shortcut_button(ui, tr("Import Audio…"), "Ctrl+I") {
				state.file_dialog(FileAction::ImportAudio);
			}
			if shortcut_button(ui, tr("Import MIDI…"), "") {
				state.file_dialog(FileAction::ImportMidi);
			}
			if shortcut_button(ui, tr("Export…"), "Ctrl+E") {
				state.open_export_dialog();
			}
			ui.separator();
			if shortcut_button(ui, tr("Collect Project Files…"), "") {
				state.collect_files();
			}
			if shortcut_button(ui, tr("Locate Missing Files…"), "") {
				state.open_relink();
			}
			let saved = state.path().is_some();
			if ui
				.add_enabled(saved, egui::Button::new(tr("Export Project Archive…")))
				.on_disabled_hover_text(tr("Save the project first"))
				.clicked()
			{
				state.file_dialog(FileAction::PackArchive);
			}
			ui.menu_button(tr("Autosave"), |ui| {
				for m in [0, 1, 2, 5, 10] {
					let label = if m == 0 {
						tr("Off").to_owned()
					} else {
						trf("Every {m} min", &[("m", &m)])
					};
					if check_item(ui, state.safety.minutes == m, &label, "") {
						state.safety.minutes = m;
					}
				}
			});
			if shortcut_button(ui, tr("Audio Settings…"), "") {
				state.open_audio_settings();
			}
			if shortcut_button(ui, tr("MIDI Mappings…"), "") {
				state.midi.window_open = true;
			}
			ui.separator();
			if shortcut_button(ui, tr("Quit"), "") {
				state.request(Pending::Quit);
			}
		});
		ui.menu_button(tr("Edit"), |ui| {
			let undo = state.undo_label().map(|l| trf("Undo {l}", &[("l", &l)]));
			if ui
				.add_enabled(
					undo.is_some(),
					egui::Button::new(undo.unwrap_or_else(|| tr("Undo").into()))
						.shortcut_text("Ctrl+Z"),
				)
				.clicked()
			{
				state.undo();
			}
			let redo = state.redo_label().map(|l| trf("Redo {l}", &[("l", &l)]));
			if ui
				.add_enabled(
					redo.is_some(),
					egui::Button::new(redo.unwrap_or_else(|| tr("Redo").into()))
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
					egui::Button::new(tr("Duplicate")).shortcut_text("Ctrl+D"),
				)
				.clicked()
			{
				duplicate_selection(state, true);
			}
			if ui
				.add_enabled(
					has_clips,
					egui::Button::new(tr("Split at Playhead")).shortcut_text("S"),
				)
				.clicked()
			{
				split_at_playhead(state);
			}
			if ui
				.add_enabled(
					has_clips,
					egui::Button::new(tr("Delete")).shortcut_text("Del"),
				)
				.clicked()
			{
				delete_selection(state);
			}
		});
		ui.menu_button(tr("Track"), |ui| {
			for (label, kind) in [
				(tr("Add Instrument Track"), TrackKind::Instrument),
				(tr("Add Audio Track"), TrackKind::Audio),
				(tr("Add Bus"), TrackKind::Bus),
			] {
				if ui.button(label).clicked() {
					arrange::add_track(state, kind);
				}
			}
			ui.separator();
			let sel = state.selection.track;
			if ui
				.add_enabled(sel.is_some(), egui::Button::new(tr("Move Up")))
				.clicked() && let Some(cmd) =
				sel.and_then(|t| edits::move_track(state.project(), t, -1))
			{
				state.apply(cmd);
			}
			if ui
				.add_enabled(sel.is_some(), egui::Button::new(tr("Move Down")))
				.clicked() && let Some(cmd) =
				sel.and_then(|t| edits::move_track(state.project(), t, 1))
			{
				state.apply(cmd);
			}
			if ui
				.add_enabled(sel.is_some(), egui::Button::new(tr("Delete Track")))
				.clicked() && let Some(track) = sel
			{
				state.apply(cadenza_model::Command::RemoveTrack { track });
			}
		});
		ui.menu_button(tr("View"), |ui| {
			ui.set_min_width(240.0);
			for panel in PanelKind::ALL {
				let on = panel_open(state, panel);
				if check_item(ui, on, panel.name(), panel.shortcut()) {
					toggle_panel(state, panel);
				}
			}
			ui.separator();
			let maximized = state.view.panels.maximized.is_some();
			if check_item(ui, maximized, tr("Maximize focused panel"), "F11") {
				state.view.panels.toggle_maximize();
			}
			if check_item(ui, false, tr("Close tool panels"), "F12") {
				state.view.panels.close_tools();
			}
			ui.separator();
			if check_item(ui, state.view.snap, tr("Snap to grid"), "") {
				state.view.snap = !state.view.snap;
			}
			ui.menu_button(format!("      {}", tr("Follow playhead")), |ui| {
				use crate::state::layout::FollowMode;
				for m in [FollowMode::Off, FollowMode::Page, FollowMode::Continuous] {
					if check_item(ui, state.view.follow == m, m.name(), "") {
						state.view.follow = m;
					}
				}
			});
			if check_item(ui, false, tr("Zoom to fit"), "Shift+Z") {
				arrange::zoom_to_fit(state, 1200.0);
			}
			ui.menu_button(format!("      {}", tr("Language")), |ui| {
				ui.set_min_width(200.0);
				for choice in crate::i18n::LanguageChoice::all() {
					if check_item(ui, state.language == choice, &choice.label(), "") {
						state.set_language(choice);
						ui.close();
					}
				}
			});
			ui.separator();
			ui.label(
				RichText::new(tr("Track height"))
					.font(theme::small())
					.color(theme::TEXT_DIM),
			);
			ui.add(egui::Slider::new(
				&mut state.view.track_height,
				crate::state::MIN_TRACK_HEIGHT..=crate::state::MAX_TRACK_HEIGHT,
			));
		});
		ui.menu_button("AI", |ui| {
			ui.set_min_width(220.0);
			let on = state.view.panels.visible(PanelKind::Chat);
			if check_item(ui, on && !state.mix.open, tr("AI producer chat"), "F10") {
				if on && state.mix.open {
					state.mix.open = false;
				} else {
					state.mix.open = false;
					toggle_panel(state, PanelKind::Chat);
				}
			}
			if check_item(ui, on && state.mix.open, tr("Mix assistant"), "") {
				if on && state.mix.open {
					toggle_panel(state, PanelKind::Chat);
				} else {
					state.mix.open = true;
					if !on {
						toggle_panel(state, PanelKind::Chat);
					}
				}
			}
			if shortcut_button(ui, tr("AI settings…"), "") {
				state.ai.settings_open = true;
			}
			if ui
				.add_enabled(state.ai.busy(), egui::Button::new(tr("Stop the AI")))
				.clicked()
			{
				state.ai.stop();
			}
		});
		ui.menu_button(tr("Help"), |ui| {
			if shortcut_button(ui, tr("Keyboard shortcuts"), "F1") {
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
	let layout = state.view.responsive.status;
	let full = ui.available_rect_before_wrap();
	// The right side first: the message on the left gets what is left (truncated).
	let right = ui
		.scope_builder(
			egui::UiBuilder::new()
				.max_rect(full)
				.layout(Layout::right_to_left(Align::Center)),
			|ui| {
				let p = state.project();
				let counts = layout.counts(
					p.tracks.len(),
					p.tracks.iter().map(|t| t.clips.len()).sum::<usize>(),
					state.document().history_labels().len(),
				);
				if let Some(counts) = counts {
					ui.label(
						RichText::new(counts)
							.font(theme::small())
							.color(theme::TEXT_FAINT),
					);
				}
				let warnings = state.load_warnings.len();
				if warnings > 0 {
					let text = if layout.chip_names() {
						trn("⚠ {n} engine warning", "⚠ {n} engine warnings", warnings)
					} else {
						format!("⚠ {warnings}")
					};
					let r = ui.label(
						RichText::new(text)
							.font(theme::small())
							.color(theme::WARNING),
					);
					r.on_hover_text(state.load_warnings.join("\n"));
				}
				ui.add_space(8.0);
				// The F-key panels as chips, filled while open (right to left).
				for (panel, key, name) in [
					(PanelKind::Chat, "F10", "AI"),
					(PanelKind::Mixer, "F9", tr("Mixer")),
					(PanelKind::Inspector, "F8", tr("Inspector")),
					(PanelKind::PianoRoll, "F7", tr("Editor")),
					(PanelKind::Browser, "F6", tr("Browser")),
					(PanelKind::Arrange, "F5", tr("Arrange")),
				] {
					let open = panel_open(state, panel);
					let label = if layout.chip_names() { name } else { "" };
					if panel_chip(ui, open, key, label, name)
						.on_hover_text(trf(
							"Show / hide the {name} panel ({key})",
							&[("name", &name), ("key", &key)],
						))
						.clicked()
					{
						toggle_panel_plain(state, panel);
					}
				}
			},
		)
		.response
		.rect;
	let left = Rect::from_min_max(full.min, egui::pos2(right.left() - 8.0, full.max.y));
	ui.scope_builder(
		egui::UiBuilder::new()
			.max_rect(left)
			.layout(Layout::left_to_right(Align::Center)),
		|ui| {
			ui.set_clip_rect(left.intersect(ui.clip_rect()));
			let (dirty, color) = if state.dirty {
				(tr("● Unsaved"), theme::WARNING)
			} else {
				(tr("● Saved"), theme::PLAYING)
			};
			ui.label(RichText::new(dirty).font(theme::small()).color(color));
			ui.add_space(6.0);
			if let Some(n) = state.notices.last() {
				let color = match n.level {
					Level::Info => theme::TEXT_DIM,
					Level::Warning => theme::WARNING,
					Level::Error => theme::ERROR,
				};
				let label = ui.add(
					egui::Label::new(
						RichText::new(crate::state::notices::short_paths(&n.text))
							.font(theme::small())
							.color(color),
					)
					.truncate(),
				);
				if label.hovered() {
					label.on_hover_text(&n.text);
				}
			}
		},
	);
}

/// Esc closes the topmost side overlay (browser, then inspector, then the AI drawer).
fn close_overlay_on_escape(ctx: &egui::Context, state: &mut AppState) {
	let r = state.view.responsive;
	let p = &mut state.view.panels;
	if p.maximized.is_some() {
		return;
	}
	let browser = r.browser_rail && p.browser && p.browser_overlay;
	let inspector = r.inspector_overlay && p.inspector;
	let chat = r.chat_overlay && p.chat;
	if !(browser || inspector || chat)
		|| !ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape))
	{
		return;
	}
	if browser {
		p.browser_overlay = false;
	} else if inspector {
		p.set(PanelKind::Inspector, false);
	} else {
		p.set(PanelKind::Chat, false);
	}
}

/// Which side panels are overlays this frame.
#[derive(Clone, Copy)]
struct OverlayPanels {
	browser: bool,
	inspector: bool,
	chat: bool,
}

/// Whether a panel reads as open in the status bar / View menu (the browser overlay in
/// rail mode).
pub fn panel_open(state: &AppState, panel: PanelKind) -> bool {
	let panels = &state.view.panels;
	if panel == PanelKind::Browser
		&& state.view.responsive.browser_rail
		&& panels.maximized.is_none()
	{
		return panels.browser && panels.browser_overlay;
	}
	panels.visible(panel)
}

/// A status bar chip naming a panel and its key; accent-tinted while the panel is open.
fn panel_chip(ui: &mut Ui, open: bool, key: &str, name: &str, a11y: &str) -> egui::Response {
	let key_g = ui
		.painter()
		.layout_no_wrap(key.to_owned(), theme::mono(10.0), theme::TEXT_FAINT);
	let name_g = ui
		.painter()
		.layout_no_wrap(name.to_owned(), theme::small(), theme::TEXT_DIM);
	let name_w = if name.is_empty() {
		-4.0
	} else {
		name_g.size().x
	};
	let size = vec2(key_g.size().x + name_w + 14.0, 16.0);
	let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
	resp.widget_info(|| {
		egui::WidgetInfo::selected(
			egui::WidgetType::Checkbox,
			true,
			open,
			trf("{a11y} panel", &[("a11y", &a11y)]),
		)
	});
	let p = ui.painter();
	let fill = if open {
		theme::ACCENT_DIM
	} else if resp.hovered() {
		theme::BG_WIDGET
	} else {
		egui::Color32::TRANSPARENT
	};
	p.rect_filled(rect, egui::CornerRadius::same(theme::RADIUS), fill);
	let (kc, nc) = if open {
		(theme::ACCENT_TEXT, theme::TEXT)
	} else {
		(theme::TEXT_FAINT, theme::TEXT_FAINT)
	};
	let y = rect.center().y;
	let kx = rect.left() + 5.0;
	p.galley(egui::pos2(kx, y - key_g.size().y * 0.5), key_g.clone(), kc);
	p.galley(
		egui::pos2(kx + key_g.size().x + 4.0, y - name_g.size().y * 0.5),
		name_g,
		nc,
	);
	resp
}

fn toasts(ctx: &egui::Context, state: &AppState) {
	let now = Instant::now();
	// Info notices live in the status bar only; toasts are for what needs attention.
	let mut list = state.notices.toasts(now, 4);
	list.retain(|n| n.level != Level::Info);
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
						.corner_radius(theme::RADIUS_LG)
						.shadow(ui.visuals().popup_shadow)
						.inner_margin(Margin::symmetric(10, 6))
						.show(ui, |ui| {
							ui.set_max_width(440.0);
							ui.with_layout(Layout::left_to_right(Align::Min), |ui| {
								ui.label(RichText::new(icon).color(accent));
								ui.add(
									egui::Label::new(
										RichText::new(crate::state::notices::short_paths(&n.text))
											.color(theme::TEXT),
									)
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
		Pending::New => tr("creating a new project"),
		Pending::Open(_) => tr("opening another project"),
		Pending::Quit => "quitting",
		Pending::Collect => tr("collecting its files"),
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
			ui.heading(tr("Unsaved changes"));
			ui.add_space(4.0);
			ui.label(trf(
				"'{project}' has changes that are not saved. Save them before {action}?",
				&[("project", &state.project().name), ("action", &action)],
			));
			ui.add_space(10.0);
			ui.horizontal(|ui| {
				if ui.button(RichText::new(tr("Save")).strong()).clicked() {
					state.save_then(pending.clone());
				}
				if ui.button(tr("Don't save")).clicked() {
					state.discard_changes();
					state.proceed(pending.clone());
				}
				if ui.button(tr("Cancel")).clicked() {
					state.confirm = None;
				}
			});
		});
}

/// Spinner while an export or plugin scan runs in the background.
fn busy_overlay(ctx: &egui::Context, state: &AppState) {
	let export = state
		.export
		.running
		.as_ref()
		.filter(|_| !state.export.open)
		.map(|r| {
			let p = r.progress();
			trf(
				"Exporting… {percent}% — {stage}",
				&[
					(
						"percent",
						&crate::i18n::decimal(format!("{:.0}", p.fraction * 100.0)),
					),
					("stage", &p.stage),
				],
			)
		});
	let jobs: Vec<String> = [
		export,
		state.jobs.scan.as_ref().map(|j| {
			trf(
				"Scanning plugins… {elapsed} s",
				&[(
					"elapsed",
					&crate::i18n::decimal(format!("{:.0}", j.started.elapsed().as_secs_f32())),
				)],
			)
		}),
	]
	.into_iter()
	.flatten()
	.chain(project_dialogs::busy_rows(state))
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
