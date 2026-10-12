//! Browser: built-in devices and scanned plugins (search, double-click or drag onto a
//! track / mixer strip), plus the factory presets of the selected device.

use cadenza_dsp::DeviceRole;
use cadenza_model::DeviceKind;
use egui::{CollapsingHeader, Id, RichText, Sense, Ui};

use crate::state::AppState;
use crate::state::browser::{BrowserItem, Source};
use crate::theme;

/// Drag-and-drop payload: a device dragged out of the browser.
#[derive(Debug, Clone)]
pub struct DraggedDevice(pub BrowserItem);

#[derive(Default)]
pub struct BrowserUi {
	query: String,
}

impl BrowserUi {
	pub fn show(&mut self, ui: &mut Ui, state: &mut AppState) {
		ui.add_space(4.0);
		ui.horizontal(|ui| {
			ui.label(RichText::new("BROWSER").font(theme::small()).color(theme::TEXT_DIM));
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
		.dnd_drag_source(id, DraggedDevice(item.clone()), |ui| {
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
			"{}\n\nDouble-click: {}. Or drag onto a track or mixer strip.",
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
			for preset in list {
				let resp = ui
					.add(egui::Button::new(preset.name).frame(false))
					.on_hover_text(format!(
						"{}\n\nClick to load (undoable).",
						preset.description
					));
				if resp.clicked() {
					state.load_factory_preset(r, preset);
				}
			}
		});
}
