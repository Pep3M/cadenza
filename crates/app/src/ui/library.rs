//! The browser's Library tab: search the style library with filters (kind, genre, role,
//! section, energy), cards with summary and pack license, click a pattern to preview it
//! (double-click writes it at the playhead, drag it onto a track or below the tracks),
//! open styles, forms, progressions and grooves as read-only documents ("Apply form"),
//! and the credits of every installed pack.

use cadenza_library::{
	Item, ItemKind, Library, LoadedItem, Pattern, Role, RoleValue, Style, card, section_name,
};
use egui::{Color32, Frame, Id, Margin, RichText, Sense, Stroke, Ui};

use crate::state::AppState;
use crate::state::dnd::Payload;
use crate::state::library::{self as lib, LibraryView};
use crate::theme;

pub fn show(ui: &mut Ui, state: &mut AppState) {
	let library = state.library();
	let mut view = state.library_view.clone();
	if view.credits {
		credits(ui, &mut view, &library);
	} else if let Some(id) = view.open.clone() {
		match library.item(&id) {
			Some(item) => document(ui, state, &mut view, &library, item),
			None => view.open = None,
		}
	} else {
		list(ui, state, &mut view, &library);
	}
	state.library_view = view;
}

fn kind_color(item: &LoadedItem) -> Color32 {
	match &item.item {
		Item::Style(_) => theme::MARKER,
		Item::Form(_) => theme::SIGNATURE,
		Item::Progression(_) => theme::PLAYING,
		Item::Groove(_) => theme::AUTOMATION,
		Item::Pattern(p) if lib::is_drum_pattern(p) => theme::PATTERN,
		Item::Pattern(_) => theme::ACCENT,
	}
}

fn caption(text: impl Into<String>) -> RichText {
	RichText::new(text)
		.font(theme::small())
		.color(theme::TEXT_DIM)
}

fn faint(text: impl Into<String>) -> RichText {
	RichText::new(text)
		.font(theme::small())
		.color(theme::TEXT_FAINT)
}

/// A small filter combo: `None` = "Any".
fn filter<T: Clone + PartialEq>(
	ui: &mut Ui,
	id: &str,
	label: &str,
	value: &mut Option<T>,
	choices: &[(T, String)],
) {
	let text = value
		.as_ref()
		.and_then(|v| choices.iter().find(|(c, _)| c == v))
		.map_or_else(|| label.to_owned(), |(_, name)| name.clone());
	let active = value.is_some();
	egui::ComboBox::from_id_salt(id)
		.width(64.0)
		.selected_text(RichText::new(text).font(theme::small()).color(if active {
			theme::ACCENT
		} else {
			theme::TEXT_DIM
		}))
		.show_ui(ui, |ui| {
			ui.selectable_value(value, None, format!("Any {}", label.to_lowercase()));
			for (c, name) in choices {
				ui.selectable_value(value, Some(c.clone()), name);
			}
		});
}

fn list(ui: &mut Ui, state: &mut AppState, view: &mut LibraryView, library: &Library) {
	ui.add(
		egui::TextEdit::singleline(&mut view.query)
			.hint_text("Search library… (latin pop, dembow, bass)")
			.desired_width(f32::INFINITY),
	);
	ui.add_space(2.0);
	ui.horizontal_wrapped(|ui| {
		ui.spacing_mut().item_spacing = egui::vec2(4.0, 4.0);
		let kinds: Vec<(ItemKind, String)> = ItemKind::ALL
			.iter()
			.map(|k| (*k, format!("{}s", k.as_str())))
			.collect();
		filter(ui, "lib-kind", "Kind", &mut view.kind, &kinds);
		let genres: Vec<(String, String)> = lib::genres(library)
			.into_iter()
			.map(|g| (g.clone(), g.replace('_', " ")))
			.collect();
		filter(ui, "lib-genre", "Genre", &mut view.genre, &genres);
		let roles: Vec<(Role, String)> = Role::ALL
			.iter()
			.map(|r| (*r, r.as_str().to_owned()))
			.collect();
		filter(ui, "lib-role", "Role", &mut view.role, &roles);
		let sections: Vec<(String, String)> = lib::SECTIONS
			.iter()
			.map(|s| ((*s).to_owned(), s.replace('_', " ")))
			.collect();
		filter(ui, "lib-section", "Section", &mut view.section, &sections);
		let energies: Vec<(u8, String)> = (1..=5).map(|e| (e, format!("energy {e}"))).collect();
		filter(ui, "lib-energy", "Energy", &mut view.energy, &energies);
	});
	let found = library.search(&view.query());
	ui.horizontal(|ui| {
		ui.label(faint(format!(
			"{} item{}",
			found.len(),
			if found.len() == 1 { "" } else { "s" }
		)));
		ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
			if ui
				.small_button("Credits")
				.on_hover_text("Installed packs, their licenses and attribution")
				.clicked()
			{
				view.credits = true;
			}
			if view.filtered() && ui.small_button("Clear").clicked() {
				view.clear_filters();
			}
		});
	});
	ui.add_space(2.0);
	egui::ScrollArea::vertical()
		.id_salt("library-list")
		.auto_shrink([false, false])
		.show(ui, |ui| {
			if found.is_empty() {
				ui.label(faint(
					"Nothing matches. Try fewer words or clear the filters.",
				));
			}
			for item in found {
				card_ui(ui, state, view, library, item);
			}
		});
}

/// The step grids of a pattern, for its tooltip.
fn grid_text(p: &Pattern) -> String {
	let width = p.lanes.keys().map(str::len).max().unwrap_or(0);
	p.lanes
		.iter()
		.map(|(k, v)| format!("{k:>width$}  {v}"))
		.collect::<Vec<_>>()
		.join("\n")
}

fn card_ui(
	ui: &mut Ui,
	state: &mut AppState,
	view: &mut LibraryView,
	library: &Library,
	item: &LoadedItem,
) {
	let c = card(item);
	let (glyph, label) = lib::badge(item);
	let color = kind_color(item);
	let selected = view.selected.as_deref() == Some(item.id.as_str());
	let previewing = state.pattern_preview() == Some(item.id.as_str());
	let license = library
		.pack(&item.pack)
		.map_or_else(String::new, |p| p.manifest.license.clone());
	let id = Id::new(("library-card", &item.id));
	let body =
		|ui: &mut Ui| {
			Frame::new()
				.fill(if selected {
					theme::BG_WIDGET
				} else {
					theme::BG_RAISED
				})
				.stroke(Stroke::new(
					1.0,
					if previewing {
						theme::PLAYING
					} else if selected {
						theme::ACCENT_DIM
					} else {
						theme::BORDER
					},
				))
				.corner_radius(theme::RADIUS)
				.inner_margin(Margin::symmetric(6, 4))
				.show(ui, |ui| {
					ui.set_width(ui.available_width());
					ui.horizontal(|ui| {
						ui.label(RichText::new(glyph).color(color));
						ui.add(
							egui::Label::new(RichText::new(&c.name).color(theme::TEXT))
								.selectable(false)
								.truncate(),
						);
						ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
							let tag = if previewing {
								"▶ playing".to_owned()
							} else {
								label
							};
							ui.label(RichText::new(tag).font(theme::small()).color(
								if previewing {
									theme::PLAYING
								} else {
									color.gamma_multiply(0.8)
								},
							));
						});
					});
					ui.add(
						egui::Label::new(caption(&c.summary))
							.selectable(false)
							.wrap(),
					);
					ui.horizontal(|ui| {
						ui.add(
							egui::Label::new(faint(&c.facts))
								.selectable(false)
								.truncate(),
						);
					});
					ui.label(faint(format!("{} · {license}", item.pack)));
				})
				.response
		};
	let response = match &item.item {
		Item::Pattern(p) => {
			ui.dnd_drag_source(
				id,
				Payload::Pattern {
					id: item.id.clone(),
					name: p.name.clone(),
				},
				body,
			)
			.response
		}
		_ => body(ui),
	};
	let hover = match &item.item {
		Item::Pattern(p) => format!(
			"{}\n\n{}\n\nClick: preview at the song tempo · Double-click: write it at the \
			 playhead · Drag: onto an instrument track or below the tracks",
			p.summary,
			grid_text(p)
		),
		_ => format!("{}\n\nClick to open", c.summary),
	};
	let response = ui
		.interact(response.rect, id.with("click"), Sense::click())
		.on_hover_text(RichText::new(hover).font(theme::mono(11.0)));
	if response.hovered() && !selected {
		ui.painter()
			.rect_filled(response.rect, 3.0, theme::BG_HOVER.gamma_multiply(0.25));
	}
	if response.double_clicked() {
		if item.item.kind() == ItemKind::Pattern {
			state.stop_pattern_preview();
			let track = state.selection.track.filter(|t| {
				state
					.project()
					.track(*t)
					.is_some_and(|t| t.kind == cadenza_model::TrackKind::Instrument)
			});
			state.add_library_pattern(&item.id, track, None);
		}
	} else if response.clicked() {
		view.selected = Some(item.id.clone());
		if item.item.kind() == ItemKind::Pattern {
			state.preview_pattern(&item.id);
		} else {
			view.open = Some(item.id.clone());
		}
	}
	ui.add_space(4.0);
}

fn back(ui: &mut Ui, view: &mut LibraryView) {
	if ui
		.small_button("‹ Library")
		.on_hover_text("Back to the list")
		.clicked()
	{
		view.open = None;
		view.credits = false;
	}
}

fn heading(ui: &mut Ui, title: &str) {
	ui.add_space(6.0);
	ui.label(caption(title.to_uppercase()));
}

/// A pattern id as a chip: click previews, drag writes it onto a track.
fn pattern_chip(ui: &mut Ui, state: &mut AppState, library: &Library, id: &str) {
	let Some(p) = library.pattern(id) else {
		ui.label(faint(id));
		return;
	};
	let previewing = state.pattern_preview() == Some(p.id.as_str());
	let drag = Id::new(("library-chip", &p.id, ui.next_auto_id()));
	let r = ui
		.dnd_drag_source(
			drag,
			Payload::Pattern {
				id: p.id.clone(),
				name: p.name.clone(),
			},
			|ui| {
				Frame::new()
					.fill(theme::BG_WIDGET)
					.stroke(Stroke::new(
						1.0,
						if previewing {
							theme::PLAYING
						} else {
							theme::BORDER
						},
					))
					.corner_radius(theme::RADIUS)
					.inner_margin(Margin::symmetric(5, 1))
					.show(ui, |ui| {
						ui.add(
							egui::Label::new(
								RichText::new(&p.name)
									.font(theme::small())
									.color(theme::TEXT),
							)
							.selectable(false),
						);
					})
					.response
			},
		)
		.response;
	let r = ui
		.interact(r.rect, drag.with("click"), Sense::click())
		.on_hover_text(format!(
			"{}\n{}\n\nClick: preview · Drag: onto a track",
			p.id, p.summary
		));
	if r.clicked() {
		state.preview_pattern(&p.id);
	}
}

fn document(
	ui: &mut Ui,
	state: &mut AppState,
	view: &mut LibraryView,
	library: &Library,
	item: &LoadedItem,
) {
	let (glyph, label) = lib::badge(item);
	let common = item.item.common();
	ui.horizontal(|ui| {
		back(ui, view);
		ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
			ui.label(
				RichText::new(label)
					.font(theme::small())
					.color(kind_color(item)),
			);
		});
	});
	ui.horizontal(|ui| {
		ui.label(RichText::new(glyph).color(kind_color(item)));
		ui.label(RichText::new(common.name).strong().color(theme::TEXT));
	});
	let license = library
		.pack(&item.pack)
		.map_or_else(String::new, |p| p.manifest.license.clone());
	ui.label(faint(format!("{} · {license}", item.id)));
	ui.add_space(2.0);
	ui.label(RichText::new(common.summary).color(theme::TEXT_DIM));
	egui::ScrollArea::vertical()
		.id_salt(("library-doc", &item.id))
		.auto_shrink([false, false])
		.show(ui, |ui| match &item.item {
			Item::Style(s) => style_doc(ui, state, library, s),
			Item::Form(f) => {
				let style = styles_listing(library, &item.id);
				apply_form_button(ui, state, &item.id, style.as_deref());
				form_table(ui, f);
			}
			Item::Progression(p) => {
				heading(ui, "Numerals");
				ui.label(
					RichText::new(&p.numerals)
						.font(theme::mono(13.0))
						.color(theme::TEXT),
				);
				ui.label(faint(format!(
					"{} · {} beats per chord",
					p.mode, p.beats_per_chord
				)));
				for root in ["A", "C", "E"] {
					if let Ok(key) = cadenza_theory::Scale::parse(&format!("{root} {}", p.mode))
						&& let Ok(chords) =
							cadenza_theory::parse_roman_progression(&p.numerals, &key)
					{
						let names: Vec<String> = chords
							.iter()
							.map(cadenza_mcp::tools::library::chord_name)
							.collect();
						ui.label(caption(format!(
							"in {root} {}: {}",
							p.mode,
							names.join(" ")
						)));
					}
				}
			}
			Item::Groove(g) => {
				heading(ui, "Timing (fraction of a step) · velocity");
				for (i, t) in g.timing.iter().enumerate() {
					let v = g.velocity.get(i).copied().unwrap_or(1.0);
					ui.label(
						RichText::new(format!("step {:>2}  {:+.2}  ×{:.2}", i + 1, t, v))
							.font(theme::mono(11.0))
							.color(theme::TEXT_DIM),
					);
				}
			}
			Item::Pattern(p) => {
				heading(ui, "Lanes");
				ui.label(
					RichText::new(grid_text(p))
						.font(theme::mono(11.0))
						.color(theme::TEXT),
				);
			}
		});
}

/// The one style listing `form`, if exactly one does.
fn styles_listing(library: &Library, form: &str) -> Option<String> {
	let ids: Vec<&str> = library
		.items()
		.filter_map(|i| match &i.item {
			Item::Style(s) if s.forms.iter().any(|f| f == form) => Some(i.id.as_str()),
			_ => None,
		})
		.collect();
	(ids.len() == 1).then(|| ids[0].to_owned())
}

fn apply_form_button(ui: &mut Ui, state: &mut AppState, form: &str, style: Option<&str>) {
	ui.add_space(4.0);
	let text = match style {
		Some(s) => format!(
			"Lays out the form's sections as markers from bar 1 and sets the song's style \
			 ({s}) and its typical tempo. One undo step."
		),
		None => "Lays out the form's sections as markers from bar 1. One undo step.".into(),
	};
	if ui.button("Apply form").on_hover_text(text).clicked() {
		state.apply_library_form(form, style);
	}
}

fn form_table(ui: &mut Ui, f: &cadenza_library::Form) {
	heading(ui, &format!("Sections · {} bars", f.total_bars()));
	egui::Grid::new(("form-sections", &f.id))
		.num_columns(3)
		.spacing(egui::vec2(8.0, 2.0))
		.striped(true)
		.show(ui, |ui| {
			for s in &f.sections {
				ui.label(RichText::new(&s.name).color(theme::TEXT));
				ui.label(caption(format!("{} bars", s.bars)));
				energy_meter(ui, s.energy).on_hover_text(format!(
					"energy {}\nroles: {}{}",
					s.energy,
					s.roles.join(", "),
					s.ending
						.map(|e| format!("\nending: {e:?}").to_lowercase())
						.unwrap_or_default()
				));
				ui.end_row();
			}
		});
}

/// Five small blocks, `energy` of them lit.
fn energy_meter(ui: &mut Ui, energy: u8) -> egui::Response {
	let (rect, response) = ui.allocate_exact_size(egui::vec2(5.0 * 7.0, 9.0), Sense::hover());
	for i in 0..5u8 {
		let block = egui::Rect::from_min_size(
			rect.min + egui::vec2(f32::from(i) * 7.0, 1.0),
			egui::vec2(5.0, 7.0),
		);
		let color = if i < energy {
			theme::PATTERN
		} else {
			theme::BG_WIDGET
		};
		ui.painter().rect_filled(block, 1.0, color);
	}
	response
}

fn style_doc(ui: &mut Ui, state: &mut AppState, library: &Library, s: &Style) {
	heading(ui, "Feel");
	ui.label(caption(format!(
		"{:.0}-{:.0} bpm (typical {:.0}) · {} · {}",
		s.tempo.min, s.tempo.max, s.tempo.typical, s.feel, s.meter
	)));
	if !s.keys.is_empty() {
		ui.label(faint(s.keys.join(" · ")));
	}
	heading(ui, "Harmony");
	for id in &s.harmony.loops {
		let text = match library.item(id).map(|i| &i.item) {
			Some(Item::Progression(p)) => format!("{} ({})", p.numerals, p.mode),
			_ => id.clone(),
		};
		ui.label(
			RichText::new(text)
				.font(theme::mono(11.5))
				.color(theme::TEXT),
		);
	}
	if !s.harmony.notes.is_empty() {
		ui.add(egui::Label::new(faint(&s.harmony.notes)).wrap());
	}
	heading(ui, "Forms");
	for id in &s.forms {
		ui.horizontal(|ui| {
			let name = library
				.item(id)
				.map_or(id.as_str(), |i| i.item.common().name);
			ui.label(RichText::new(name).color(theme::TEXT));
			ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
				if ui
					.small_button("Apply")
					.on_hover_text(format!(
						"Markers for every section from bar 1, tempo {:.0} bpm and this style as \
						 the song's style. One undo step.",
						s.tempo.typical
					))
					.clicked()
				{
					state.apply_library_form(id, Some(&s.id));
				}
			});
		});
	}
	heading(ui, "Roles");
	for (role, sections) in s.roles.iter() {
		ui.label(RichText::new(role).color(theme::TEXT));
		for (section, value) in sections.iter() {
			match value {
				RoleValue::Ids(ids) if ids.is_empty() => {}
				RoleValue::Ids(ids) => {
					ui.horizontal_wrapped(|ui| {
						ui.spacing_mut().item_spacing = egui::vec2(4.0, 3.0);
						ui.label(faint(section_name(section).replace('_', " ")));
						for id in ids {
							pattern_chip(ui, state, library, id);
						}
					});
				}
				RoleValue::Text(t) => {
					ui.add(egui::Label::new(faint(format!("{section}: {t}"))).wrap());
				}
			}
		}
	}
	if !s.sounds.is_empty() {
		heading(ui, "Sounds");
		for (k, v) in s.sounds.iter() {
			let text = match v {
				RoleValue::Text(t) => t.clone(),
				RoleValue::Ids(ids) => ids.join(", "),
			};
			ui.add(egui::Label::new(caption(format!("{k}: {text}"))).wrap());
		}
	}
	heading(ui, &format!("Mix · {:.0} LUFS", s.mix.lufs));
	ui.add(egui::Label::new(caption(&s.mix.notes)).wrap());
	for (title, list, color) in [
		("Do", &s.dos, theme::PLAYING),
		("Don't", &s.dont, theme::ERROR),
	] {
		if list.is_empty() {
			continue;
		}
		heading(ui, title);
		for line in list {
			ui.horizontal_wrapped(|ui| {
				ui.label(RichText::new("•").color(color));
				ui.add(egui::Label::new(caption(line)).wrap());
			});
		}
	}
}

fn credits(ui: &mut Ui, view: &mut LibraryView, library: &Library) {
	ui.horizontal(|ui| back(ui, view));
	ui.label(RichText::new("Credits").strong().color(theme::TEXT));
	ui.add(
		egui::Label::new(faint(
			"Attribution applies to redistributing a pack, not to songs you make with it.",
		))
		.wrap(),
	);
	egui::ScrollArea::vertical()
		.id_salt("library-credits")
		.auto_shrink([false, false])
		.show(ui, |ui| {
			for pack in library.packs() {
				let m = &pack.manifest;
				ui.add_space(6.0);
				Frame::new()
					.fill(theme::BG_RAISED)
					.stroke(Stroke::new(1.0, theme::BORDER))
					.corner_radius(theme::RADIUS)
					.inner_margin(Margin::symmetric(6, 4))
					.show(ui, |ui| {
						ui.set_width(ui.available_width());
						ui.label(RichText::new(&m.name).color(theme::TEXT));
						ui.label(caption(format!(
							"{} · v{} · {} · {} items",
							m.id,
							m.version,
							m.license,
							pack.items.len()
						)));
						if !m.attribution.is_empty() {
							ui.add(
								egui::Label::new(caption(format!("by {}", m.attribution))).wrap(),
							);
						}
						for link in m.homepage.iter().chain(m.source.iter()) {
							ui.add(egui::Label::new(faint(link)).wrap());
						}
						if !m.redistributable {
							ui.label(
								RichText::new("Not redistributable: for your own use only")
									.font(theme::small())
									.color(theme::WARNING),
							);
						}
						let location = match &pack.origin {
							cadenza_library::PackOrigin::Embedded => "built in".to_owned(),
							cadenza_library::PackOrigin::Dir(d) => d.display().to_string(),
						};
						ui.add(egui::Label::new(faint(location)).truncate());
					});
			}
			let problems: Vec<_> = library
				.reports()
				.iter()
				.filter(|r| !r.issues.is_empty())
				.collect();
			if !problems.is_empty() {
				heading(ui, "Problems");
				for r in problems {
					ui.label(
						RichText::new(format!(
							"{}{}",
							r.pack.as_deref().unwrap_or(&r.location),
							if r.loaded { "" } else { " (not loaded)" }
						))
						.font(theme::small())
						.color(theme::WARNING),
					);
					for i in &r.issues {
						ui.add(
							egui::Label::new(faint(format!("{}: {}", i.file, i.message))).wrap(),
						);
					}
				}
			}
		});
}
