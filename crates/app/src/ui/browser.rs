//! Browser: built-in devices and scanned plugins (search, double-click or drag onto a
//! track / mixer strip / below the tracks), the factory presets of the selected device
//! (drag onto a device or track), a Files tab (bookmarked folders, audio/MIDI filter;
//! drag files onto the arrange view, double-click to import) and the style Library tab
//! (`ui::library`).

use crate::i18n::{tr, tr_str, trf};
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
	Library,
}

#[derive(Default)]
pub struct BrowserUi {
	query: String,
	tab: Tab,
}

impl BrowserUi {
	/// The Library tab is open: the browser widens into filters, results and document
	/// (design 08).
	pub fn library_open(&self) -> bool {
		self.tab == Tab::Library
	}

	/// The browser as a 40 px icon rail (narrow windows, design 12a): an icon opens the
	/// overlay on its tab, the open tab's icon closes it.
	pub fn rail(&mut self, ui: &mut Ui, state: &mut AppState) {
		ui.add_space(6.0);
		ui.vertical_centered(|ui| {
			ui.spacing_mut().item_spacing.y = 4.0;
			for (tab, glyph, name) in [
				(Tab::Devices, "▤", tr("Devices")),
				(Tab::Files, "♪", tr("Files")),
				(Tab::Library, "☰", tr("Library")),
			] {
				let open = state.view.panels.browser_overlay && self.tab == tab;
				let (rect, resp) = ui.allocate_exact_size(egui::vec2(30.0, 30.0), Sense::click());
				let bg = if open {
					theme::BG_HOVER
				} else if resp.hovered() {
					theme::BG_WIDGET
				} else {
					egui::Color32::TRANSPARENT
				};
				ui.painter()
					.rect_filled(rect, egui::CornerRadius::same(theme::RADIUS_MD), bg);
				ui.painter().text(
					rect.center(),
					egui::Align2::CENTER_CENTER,
					glyph,
					egui::FontId::proportional(15.0),
					if open { theme::TEXT } else { theme::TEXT_DIM },
				);
				resp.widget_info(|| {
					egui::WidgetInfo::selected(
						egui::WidgetType::Button,
						true,
						open,
						trf("Browser {name}", &[("name", &name)]),
					)
				});
				if resp
					.on_hover_text(trf(
						"{name} (F6): opens the browser over the arrange",
						&[("name", &name)],
					))
					.clicked()
				{
					let p = &mut state.view.panels;
					if open {
						p.browser_overlay = false;
					} else {
						self.tab = tab;
						p.browser_overlay = true;
						p.focus = Some(crate::state::layout::PanelKind::Browser);
					}
				}
			}
		});
	}

	pub fn show(&mut self, ui: &mut Ui, state: &mut AppState) {
		ui.add_space(4.0);
		ui.horizontal(|ui| {
			super::widgets::panel_title(ui, tr("Browser"), Some("F6"));
			if self.tab != Tab::Devices {
				return;
			}
			ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
				let scanning = state.jobs.scan.is_some();
				if scanning {
					ui.spinner();
				}
				if ui
					.add_enabled(!scanning, egui::Button::new(tr("Rescan")).small())
					.on_hover_text(tr("Scan the system plugin folders for new or changed CLAP/VST3 plugins (runs in the background)"))
					.clicked()
				{
					state.rescan_plugins();
				}
			});
		});
		ui.add_space(2.0);
		if std::mem::take(&mut state.view.instrument.files_tab) {
			self.tab = Tab::Files;
		}
		let tabs = [Tab::Devices, Tab::Files, Tab::Library];
		let current = tabs.iter().position(|t| *t == self.tab).unwrap_or(0);
		if let Some(i) =
			super::widgets::segmented(ui, &[tr("Devices"), tr("Files"), tr("Library")], current)
		{
			self.tab = tabs[i];
		}
		ui.add_space(4.0);
		match self.tab {
			Tab::Files => return files_tab(ui, state),
			Tab::Library => return super::library::show(ui, state),
			Tab::Devices => {}
		}
		ui.add(
			egui::TextEdit::singleline(&mut self.query)
				.hint_text(tr("Search devices…"))
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
					(tr("Instruments"), Source::Builtin, DeviceRole::Instrument),
					(tr("Effects"), Source::Builtin, DeviceRole::Effect),
					(
						tr("Plugin instruments"),
						Source::Plugin,
						DeviceRole::Instrument,
					),
					(tr("Plugin effects"), Source::Plugin, DeviceRole::Effect),
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
						RichText::new(format!("{title}  ·  {}", list.len()))
							.font(theme::semibold(11.5))
							.color(theme::TEXT_DIM),
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
						RichText::new(tr(
							"No plugins in the catalog. Press Rescan to look for CLAP/VST3 plugins.",
						))
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
	let (badge_fg, badge_bg) = match item.role {
		DeviceRole::Instrument => (theme::ACCENT_TEXT, theme::ACCENT_DIM),
		DeviceRole::Effect => (theme::AUTOMATION, theme::AUTOMATION_SOFT),
	};
	let width = ui.available_width();
	let name_font = egui::FontId::proportional(12.0);
	let mut elided = false;
	let response = ui
		.dnd_drag_source(id, Payload::Device(item.clone()), |ui| {
			let (rect, response) = ui.allocate_exact_size(egui::vec2(width, 24.0), Sense::hover());
			let p = ui.painter();
			let badge = egui::Rect::from_min_size(
				egui::pos2(rect.left() + 2.0, rect.center().y - 9.0),
				egui::vec2(24.0, 18.0),
			);
			p.rect_filled(badge, theme::RADIUS, badge_bg);
			p.text(
				badge.center(),
				egui::Align2::CENTER_CENTER,
				initials(&item.name),
				theme::semibold(9.5),
				badge_fg,
			);
			let text = egui::Rect::from_min_max(
				egui::pos2(badge.right() + 8.0, rect.top()),
				egui::pos2(rect.right() - 4.0, rect.bottom()),
			);
			// Reserve the detail text's own width on the right so a long device name
			// can never be drawn under it; elide the name to what is left.
			let name_max_width = if item.detail.is_empty() {
				text.width()
			} else {
				let detail_width = p
					.layout_no_wrap(item.detail.clone(), theme::small(), theme::TEXT_FAINT)
					.size()
					.x;
				p.text(
					text.right_center(),
					egui::Align2::RIGHT_CENTER,
					&item.detail,
					theme::small(),
					theme::TEXT_FAINT,
				);
				(text.width() - detail_width - 6.0).max(0.0)
			};
			let (shown_name, name_elided) =
				elide_text(ui, &item.name, name_font.clone(), name_max_width);
			elided = name_elided;
			p.text(
				text.left_center(),
				egui::Align2::LEFT_CENTER,
				shown_name,
				name_font.clone(),
				theme::TEXT,
			);
			response
		})
		.response;
	let name_line = if elided {
		format!("{}\n", item.name)
	} else {
		String::new()
	};
	let response = ui
		.interact(response.rect, id.with("click"), Sense::click())
		.on_hover_text(format!(
			"{name_line}{}\n\n{}",
			item.description,
			match item.role {
				DeviceRole::Instrument => tr(
					"Double-click: set as the selected track's instrument (or create a track). Or \
					 drag onto a track, a mixer strip or below the tracks."
				),
				DeviceRole::Effect => tr(
					"Double-click: insert on the selected track (or the master). Or drag onto a \
					 track, a mixer strip or below the tracks."
				),
			}
		));
	if response.hovered() {
		ui.painter().rect_filled(
			response.rect,
			theme::RADIUS,
			theme::BG_RAISED.gamma_multiply(0.8),
		);
	}
	if response.double_clicked() {
		state.use_device(item, None);
	}
}

/// Shortens `text` with a trailing "…" so it lays out within `max_width` at `font`;
/// returns the text to show and whether it was shortened.
fn elide_text(ui: &Ui, text: &str, font: egui::FontId, max_width: f32) -> (String, bool) {
	let painter = ui.painter();
	let width = |s: &str| {
		painter
			.layout_no_wrap(s.to_owned(), font.clone(), theme::TEXT)
			.size()
			.x
	};
	if width(text) <= max_width {
		return (text.to_owned(), false);
	}
	let mut chars: Vec<char> = text.chars().collect();
	while !chars.is_empty() {
		chars.pop();
		let candidate: String = chars.iter().collect::<String>() + "…";
		if width(&candidate) <= max_width {
			return (candidate, true);
		}
	}
	("…".to_owned(), true)
}

/// Two-letter badge of a device name: "Drum Machine" -> "DM", "Chorus" -> "CH".
fn initials(name: &str) -> String {
	let words: Vec<&str> = name
		.split(|c: char| !c.is_alphanumeric())
		.filter(|w| !w.is_empty())
		.collect();
	let letters: String = match words.as_slice() {
		[] => "?".into(),
		[one] => one.chars().take(2).collect(),
		[a, b, ..] => a.chars().take(1).chain(b.chars().take(1)).collect(),
	};
	letters.to_uppercase()
}

/// Factory presets of the selected built-in device.
fn presets(ui: &mut Ui, state: &mut AppState) {
	let device_name = state
		.selection
		.device
		.and_then(|r| state.device(r))
		.map(|d| d.name.clone());
	match &device_name {
		Some(name) => {
			super::widgets::panel_title(ui, &trf("Presets · {name}", &[("name", &name)]), None)
		}
		None => super::widgets::panel_title(ui, tr("Presets"), None),
	};
	let Some(r) = state.selection.device else {
		ui.label(
			RichText::new(tr("Select a device to see its presets"))
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
			RichText::new(tr("Plugin presets: use the MCP preset tools for now"))
				.font(theme::small())
				.color(theme::TEXT_FAINT),
		);
		return;
	};
	let list = cadenza_dsp::factory_presets(id);
	let id = id.clone();
	let loaded = state.preset_status(r).name;
	let filter_id = Id::new(("preset-filter", &id));
	let mut filter: String = ui.data(|d| d.get_temp(filter_id)).unwrap_or_default();
	if list.len() > 12 {
		ui.add(
			egui::TextEdit::singleline(&mut filter)
				.hint_text(tr("Search presets"))
				.desired_width(f32::INFINITY),
		);
		ui.data_mut(|d| d.insert_temp(filter_id, filter.clone()));
	}
	let words: Vec<String> = filter.split_whitespace().map(str::to_lowercase).collect();
	let shown: Vec<&'static cadenza_dsp::FactoryPreset> = list
		.iter()
		.filter(|p| {
			let text = format!("{} {} {}", p.name, p.category, p.description).to_lowercase();
			words.iter().all(|w| text.contains(w.as_str()))
		})
		.collect();
	egui::ScrollArea::vertical()
		.id_salt("presets")
		.auto_shrink([false, false])
		.show(ui, |ui| {
			if list.is_empty() {
				ui.label(
					RichText::new(tr("No factory presets"))
						.font(theme::small())
						.color(theme::TEXT_FAINT),
				);
			} else if shown.is_empty() {
				ui.label(
					RichText::new(tr("No preset matches"))
						.font(theme::small())
						.color(theme::TEXT_FAINT),
				);
			}
			let mut row = |ui: &mut Ui, preset: &'static cadenza_dsp::FactoryPreset| {
				let payload = Payload::Preset {
					device: id.clone(),
					preset,
				};
				let drag_id = Id::new(("preset", &id, preset.name));
				let current = loaded.as_deref() == Some(preset.name);
				let resp = ui
					.dnd_drag_source(drag_id, payload, |ui| {
						let text = RichText::new(preset.name).color(if current {
							theme::ACCENT
						} else {
							theme::TEXT
						});
						ui.add(egui::Label::new(text).selectable(false))
					})
					.response;
				let resp = ui
					.interact(resp.rect, drag_id.with("click"), Sense::click())
					.on_hover_text(format!(
						"{}\n\n{}",
						preset.description,
						tr("Click to load (undoable), or drag onto another track or device.")
					));
				if resp.hovered() {
					ui.painter()
						.rect_filled(resp.rect, 2.0, theme::BG_HOVER.gamma_multiply(0.5));
				}
				if resp.clicked() {
					state.load_factory_preset(r, preset);
				}
			};
			if shown.iter().all(|p| p.category.is_empty()) {
				for preset in shown {
					row(ui, preset);
				}
				return;
			}
			for category in cadenza_dsp::PRESET_CATEGORIES {
				let group: Vec<_> = shown.iter().filter(|p| p.category == category).collect();
				if group.is_empty() {
					continue;
				}
				let holds_loaded = group.iter().any(|p| loaded.as_deref() == Some(p.name));
				egui::CollapsingHeader::new(format!("{} · {}", tr_str(category), group.len()))
					.id_salt(("preset-category", &id, category))
					.default_open(holds_loaded)
					.open((!words.is_empty()).then_some(true))
					.show(ui, |ui| {
						for preset in group {
							row(ui, preset);
						}
					});
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
		RichText::new(tr("BOOKMARKS"))
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
			if ui.button(tr("Remove bookmark")).clicked() {
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
			.on_hover_text(tr("Parent folder"))
			.clicked()
		{
			state.files.up();
		}
		if ui.small_button("⟳").on_hover_text(tr("Refresh")).clicked() {
			state.files.refresh();
		}
		let bookmarked = dir
			.as_ref()
			.is_some_and(|d| state.view.bookmarks.contains(d));
		if let Some(d) = &dir
			&& !bookmarked
			&& ui
				.small_button("☆")
				.on_hover_text(tr("Bookmark this folder"))
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
					RichText::new(tr("No audio, MIDI or project files here"))
						.font(theme::small())
						.color(theme::TEXT_FAINT),
				);
			}
			Ok(list) => {
				let mut open: Option<PathBuf> = None;
				for e in list {
					if e.kind == FileKind::Folder {
						// Folders drag too: onto the drum sampler they map a whole kit.
						let id = Id::new(("browser-folder", &e.path));
						let r = ui
							.dnd_drag_source(id, Payload::File(e.path.clone()), |ui| {
								ui.add(
									egui::Label::new(format!("{}  {}", e.kind.glyph(), e.name))
										.selectable(false)
										.truncate(),
								)
							})
							.response;
						let r = ui
							.interact(r.rect, id.with("click"), Sense::click())
							.on_hover_text(tr(
								"Click to open; drag onto the drum sampler to map the kit in it onto its pads by name",
							));
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
			"{}\n\n{}",
			e.path.display(),
			match e.kind {
				FileKind::Project => tr(
					"Drag onto the arrange view (a track or below the tracks; Alt below the \
					 tracks: new sampler), a sampler pad or zone, or double-click to open it."
				),
				_ => tr(
					"Drag onto the arrange view (a track or below the tracks; Alt below the \
					 tracks: new sampler), a sampler pad or zone, or double-click to import it at \
					 the playhead."
				),
			}
		));
	if response.hovered() {
		ui.painter()
			.rect_filled(response.rect, 2.0, theme::BG_HOVER.gamma_multiply(0.5));
	}
	if e.kind == FileKind::Audio {
		response.context_menu(|ui| {
			if ui.button(tr("Import at the playhead")).clicked() {
				state.import_audio(&e.path);
				ui.close();
			}
			if ui.button(tr("Load into new sampler")).clicked() {
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
					trf(
						"Load into pad {pad_label}",
						&[(
							"pad_label",
							&crate::state::samples::pad_label(state.view.instrument.pad),
						)],
					)
				} else {
					tr("Map across the open sampler").to_owned()
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
