//! Browser: built-in devices and scanned plugins (search, double-click or drag onto a
//! track / mixer strip / below the tracks), the factory presets of the selected device
//! (drag onto a device or track), and a Files tab (bookmarked folders, audio/MIDI
//! filter; drag files onto the arrange view, double-click to import).

use std::path::PathBuf;

use cadenza_dsp::DeviceRole;
use cadenza_model::DeviceKind;
use egui::{CollapsingHeader, Id, RichText, Sense, Ui};

use crate::state::AppState;
use crate::state::browser::{BrowserItem, Source};
use crate::state::dnd::Payload;
use crate::state::files::{self, FileFilter, FileKind};
use crate::theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Tab {
	#[default]
	Devices,
	Files,
}

#[derive(Default)]
pub struct BrowserUi {
	query: String,
	tab: Tab,
}

impl BrowserUi {
	pub fn show(&mut self, ui: &mut Ui, state: &mut AppState) {
		ui.add_space(4.0);
		ui.horizontal(|ui| {
			for (tab, label) in [(Tab::Devices, "Devices"), (Tab::Files, "Files")] {
				if ui
					.selectable_label(self.tab == tab, RichText::new(label).font(theme::small()))
					.clicked()
				{
					self.tab = tab;
				}
			}
			if self.tab == Tab::Files {
				return;
			}
			ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
				let scanning = state.jobs.scan.is_some();
				if scanning {
					ui.spinner();
				}
				if ui
					.add_enabled(!scanning, egui::Button::new("Rescan").small())
					.on_hover_text("Scan the system plugin folders for new or changed CLAP/VST3 plugins (runs in the background)")
					.clicked()
				{
					state.rescan_plugins();
				}
			});
		});
		if self.tab == Tab::Files {
			files_tab(ui, state);
			return;
		}
		ui.add(
			egui::TextEdit::singleline(&mut self.query)
				.hint_text("Search devices…")
				.desired_width(f32::INFINITY),
		);
		ui.add_space(2.0);
		let items: Vec<BrowserItem> = state
			.browser_items()
			.iter()
			.filter(|i| i.matches(&self.query))
			.cloned()
			.collect();
		let presets_height = 150.0;
		egui::ScrollArea::vertical()
			.id_salt("browser-list")
			.max_height((ui.available_height() - presets_height).max(80.0))
			.auto_shrink([false, false])
			.show(ui, |ui| {
				let groups = [
					("Instruments", Source::Builtin, DeviceRole::Instrument),
					("Effects", Source::Builtin, DeviceRole::Effect),
					("Plugin instruments", Source::Plugin, DeviceRole::Instrument),
					("Plugin effects", Source::Plugin, DeviceRole::Effect),
				];
				for (title, source, role) in groups {
					let list: Vec<&BrowserItem> = items
						.iter()
						.filter(|i| i.source == source && i.role == role)
						.collect();
					if list.is_empty() && source == Source::Plugin {
						continue;
					}
					CollapsingHeader::new(
						RichText::new(format!("{title}  ({})", list.len())).font(theme::small()),
					)
					.id_salt(title)
					.default_open(true)
					.show(ui, |ui| {
						for item in list {
							item_row(ui, state, item);
						}
					});
				}
				if !items.iter().any(|i| i.source == Source::Plugin) {
					ui.add_space(6.0);
					ui.label(
						RichText::new(
							"No plugins in the catalog. Press Rescan to look for CLAP/VST3 plugins.",
						)
						.font(theme::small())
						.color(theme::TEXT_FAINT),
					);
				}
			});
		ui.separator();
		presets(ui, state);
	}
}

fn item_row(ui: &mut Ui, state: &mut AppState, item: &BrowserItem) {
	let id = Id::new(("browser-item", &item.id));
	let glyph = match item.role {
		DeviceRole::Instrument => "♪",
		DeviceRole::Effect => "ƒ",
	};
	let response = ui
		.dnd_drag_source(id, Payload::Device(item.clone()), |ui| {
			ui.horizontal(|ui| {
				ui.label(RichText::new(glyph).color(theme::TEXT_FAINT));
				ui.add(egui::Label::new(&item.name).selectable(false).truncate());
				if !item.detail.is_empty() {
					ui.label(
						RichText::new(&item.detail)
							.font(theme::small())
							.color(theme::TEXT_FAINT),
					);
				}
			})
			.response
		})
		.response;
	let response = ui
		.interact(response.rect, id.with("click"), Sense::click())
		.on_hover_text(format!(
			"{}\n\nDouble-click: {}. Or drag onto a track, a mixer strip or below the tracks.",
			item.description,
			match item.role {
				DeviceRole::Instrument =>
					"set as the selected track's instrument (or create a track)",
				DeviceRole::Effect => "insert on the selected track (or the master)",
			}
		));
	if response.hovered() {
		ui.painter()
			.rect_filled(response.rect, 2.0, theme::BG_HOVER.gamma_multiply(0.5));
	}
	if response.double_clicked() {
		state.use_device(item, None);
	}
}

/// Factory presets of the selected built-in device.
fn presets(ui: &mut Ui, state: &mut AppState) {
	ui.label(
		RichText::new("PRESETS")
			.font(theme::small())
			.color(theme::TEXT_DIM),
	);
	let Some(r) = state.selection.device else {
		ui.label(
			RichText::new("Select a device to see its presets")
				.font(theme::small())
				.color(theme::TEXT_FAINT),
		);
		return;
	};
	let Some(device) = state.device(r) else {
		return;
	};
	let DeviceKind::Builtin { id } = &device.kind else {
		ui.label(
			RichText::new("Plugin presets: use the MCP preset tools for now")
				.font(theme::small())
				.color(theme::TEXT_FAINT),
		);
		return;
	};
	let list = cadenza_dsp::factory_presets(id);
	let id = id.clone();
	let name = device.name.clone();
	ui.label(RichText::new(name).font(theme::small()).color(theme::TEXT));
	egui::ScrollArea::vertical()
		.id_salt("presets")
		.auto_shrink([false, false])
		.show(ui, |ui| {
			if list.is_empty() {
				ui.label(
					RichText::new("No factory presets")
						.font(theme::small())
						.color(theme::TEXT_FAINT),
				);
			}
			let device_id = id.clone();
			for preset in list {
				let payload = Payload::Preset {
					device: device_id.clone(),
					preset,
				};
				let drag_id = Id::new(("preset", &device_id, preset.name));
				let resp = ui
					.dnd_drag_source(drag_id, payload, |ui| {
						ui.add(egui::Label::new(preset.name).selectable(false))
					})
					.response;
				let resp = ui
					.interact(resp.rect, drag_id.with("click"), Sense::click())
					.on_hover_text(format!(
						"{}\n\nClick to load (undoable), or drag onto another track or device.",
						preset.description
					));
				if resp.hovered() {
					ui.painter()
						.rect_filled(resp.rect, 2.0, theme::BG_HOVER.gamma_multiply(0.5));
				}
				if resp.clicked() {
					state.load_factory_preset(r, preset);
				}
			}
		});
}

/// Files tab: bookmarks, the current folder (filtered) and its files.
fn files_tab(ui: &mut Ui, state: &mut AppState) {
	if state.files.dir.is_none() {
		state.files.dir = state
			.view
			.bookmarks
			.first()
			.cloned()
			.or_else(|| files::default_bookmarks().into_iter().next());
	}
	ui.label(
		RichText::new("BOOKMARKS")
			.font(theme::small())
			.color(theme::TEXT_DIM),
	);
	let mut remove = None;
	for (i, b) in state.view.bookmarks.clone().iter().enumerate() {
		let current = state.files.dir.as_deref() == Some(b.as_path());
		let r = ui
			.selectable_label(current, format!("★ {}", files::display_path(b)))
			.on_hover_text(b.display().to_string());
		if r.clicked() {
			state.files.open(b.clone());
		}
		r.context_menu(|ui| {
			if ui.button("Remove bookmark").clicked() {
				remove = Some(i);
				ui.close();
			}
		});
	}
	if let Some(i) = remove {
		state.view.bookmarks.remove(i);
	}
	ui.separator();
	let dir = state.files.dir.clone();
	ui.horizontal(|ui| {
		if ui
			.small_button("⬆")
			.on_hover_text("Parent folder")
			.clicked()
		{
			state.files.up();
		}
		if ui.small_button("⟳").on_hover_text("Refresh").clicked() {
			state.files.refresh();
		}
		let bookmarked = dir
			.as_ref()
			.is_some_and(|d| state.view.bookmarks.contains(d));
		if let Some(d) = &dir
			&& !bookmarked
			&& ui
				.small_button("☆")
				.on_hover_text("Bookmark this folder")
				.clicked()
		{
			state.view.bookmarks.push(d.clone());
		}
		ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
			for f in [FileFilter::Midi, FileFilter::Audio, FileFilter::All] {
				if ui
					.selectable_label(
						state.files.filter == f,
						RichText::new(f.name()).font(theme::small()),
					)
					.clicked()
				{
					state.files.filter = f;
				}
			}
		});
	});
	if let Some(d) = &dir {
		ui.add(
			egui::Label::new(
				RichText::new(files::display_path(d))
					.font(theme::small())
					.color(theme::TEXT_FAINT),
			)
			.truncate(),
		);
	}
	let entries: Result<Vec<files::FileEntry>, String> = state
		.files
		.entries()
		.map(<[_]>::to_vec)
		.map_err(str::to_owned);
	egui::ScrollArea::vertical()
		.id_salt("files-list")
		.auto_shrink([false, false])
		.show(ui, |ui| match entries {
			Err(e) => {
				ui.label(RichText::new(e).font(theme::small()).color(theme::WARNING));
			}
			Ok(list) if list.is_empty() => {
				ui.label(
					RichText::new("No audio, MIDI or project files here")
						.font(theme::small())
						.color(theme::TEXT_FAINT),
				);
			}
			Ok(list) => {
				let mut open: Option<PathBuf> = None;
				for e in list {
					if e.kind == FileKind::Folder {
						let r = ui.add(
							egui::Label::new(format!("{}  {}", e.kind.glyph(), e.name))
								.selectable(false)
								.truncate()
								.sense(Sense::click()),
						);
						if r.hovered() {
							ui.painter().rect_filled(
								r.rect,
								2.0,
								theme::BG_HOVER.gamma_multiply(0.5),
							);
						}
						if r.clicked() {
							open = Some(e.path.clone());
						}
						continue;
					}
					file_row(ui, state, &e);
				}
				if let Some(p) = open {
					state.files.open(p);
				}
			}
		});
}

fn file_row(ui: &mut Ui, state: &mut AppState, e: &files::FileEntry) {
	let id = Id::new(("browser-file", &e.path));
	let color = match e.kind {
		FileKind::Audio => theme::PLAYING,
		FileKind::Midi => theme::ACCENT,
		FileKind::Project => theme::MARKER,
		FileKind::Folder => theme::TEXT_DIM,
	};
	let response = ui
		.dnd_drag_source(id, Payload::File(e.path.clone()), |ui| {
			ui.horizontal(|ui| {
				ui.label(RichText::new(e.kind.glyph()).color(color));
				ui.add(egui::Label::new(&e.name).selectable(false).truncate());
			})
			.response
		})
		.response;
	let response = ui
		.interact(response.rect, id.with("click"), Sense::click())
		.on_hover_text(format!(
			"{}\n\nDrag onto the arrange view (a track or below the tracks; Alt below the tracks: \
			 new sampler), a sampler pad or zone, or double-click to {}.",
			e.path.display(),
			match e.kind {
				FileKind::Project => "open it",
				_ => "import it at the playhead",
			}
		));
	if response.hovered() {
		ui.painter()
			.rect_filled(response.rect, 2.0, theme::BG_HOVER.gamma_multiply(0.5));
	}
	if e.kind == FileKind::Audio {
		response.context_menu(|ui| {
			if ui.button("Import at the playhead").clicked() {
				state.import_audio(&e.path);
				ui.close();
			}
			if ui.button("Load into new sampler").clicked() {
				state.new_sampler_track(&e.path);
				ui.close();
			}
			let editor = state
				.view
				.instrument
				.device
				.filter(|r| state.is_sample_device(*r));
			if let Some(r) = editor {
				let drums = state.device(r).and_then(crate::state::samples::kind)
					== Some(crate::state::samples::SampleKind::Drums);
				let label = if drums {
					format!(
						"Load into pad {}",
						crate::state::samples::pad_label(state.view.instrument.pad)
					)
				} else {
					"Map across the open sampler".to_owned()
				};
				if ui.button(label).clicked() {
					let slot = if drums {
						crate::state::samples::SampleSlot::Pad(state.view.instrument.pad)
					} else {
						crate::state::samples::SampleSlot::Keyboard
					};
					state.load_sample(r, &e.path, slot);
					ui.close();
				}
			}
		});
	}
	if response.double_clicked() {
		match e.kind {
			FileKind::Audio => state.import_audio(&e.path),
			FileKind::Midi => {
				let at = Some(state.position());
				state.import_midi_at(&e.path, None, at);
			}
			FileKind::Project => state.request(crate::state::Pending::Open(Some(e.path.clone()))),
			FileKind::Folder => {}
		}
	}
}
