//! The browser's Library tab (design 08 Style Library). Wide (the browser widens while
//! the tab is open) it is three columns: facet filters + packs, search with kind tabs over
//! a grid of result cards, and the open document (style, form, progression, groove, or the
//! credits). Narrow it stacks: search, kind tabs, filter combos, one card per row, and a
//! document replaces the list ("‹ Library" goes back).
//!
//! Cards: kind tag, pack · license, name, where it fits, a picture (pattern notes, groove
//! timing, progression numerals, form sections), meta and energy. Click a pattern to
//! audition it (the card plays with a playhead; click again stops), double-click writes it
//! at the playhead, drag it onto a track or below the tracks; click any other card to open
//! it. Documents: forms with Apply, roles as draggable/previewable pattern chips.

use crate::i18n::{tr, trf, trn};
use std::sync::Arc;

use cadenza_library::{Item, ItemKind, Library, LoadedItem, Pattern, Role, Style, card};
use egui::{
	Color32, CornerRadius, CursorIcon, Frame, Id, Margin, Painter, Rect, RichText, Sense, Stroke,
	Ui, UiBuilder, pos2, vec2,
};

use crate::state::AppState;
use crate::state::dnd::Payload;
use crate::state::layout::PanelKind;
use crate::state::library::{self as lib, LibraryView, MiniPicture};
use crate::theme;

/// Below this width the tab stacks (one column).
const WIDE: f32 = 640.0;
/// From this width an open document sits beside the results.
const DOC_BESIDE: f32 = 980.0;
const FILTERS_W: f32 = 188.0;
const CARD_H: f32 = 158.0;
const CARD_MIN_W: f32 = 200.0;
const GAP: f32 = 8.0;
/// Genre chips shown before "+ n more".
const GENRES_SHOWN: usize = 10;

pub fn show(ui: &mut Ui, state: &mut AppState) {
	let library = state.library();
	let mut view = state.library_view.clone();
	if view
		.open
		.as_ref()
		.is_some_and(|id| library.item(id).is_none())
	{
		view.open = None;
	}
	if ui.available_width() >= WIDE {
		wide(ui, state, &mut view, &library);
	} else {
		narrow(ui, state, &mut view, &library);
	}
	state.library_view = view;
}

// --- Looks -----------------------------------------------------------------------------

/// Tag text, its colour and the tag fill of an item's kind.
fn kind_look(item: &LoadedItem) -> (&'static str, Color32) {
	match &item.item {
		Item::Style(_) => (tr("STYLE"), theme::PATTERN),
		Item::Form(_) => (tr("FORM"), theme::ACCENT_TEXT),
		Item::Progression(_) => (tr("PROGRESSION"), theme::MARKER),
		Item::Groove(_) => (tr("GROOVE"), theme::LOOP),
		Item::Pattern(_) => (tr("PATTERN"), theme::BAND_COLORS[0]),
	}
}

fn soft(color: Color32) -> Color32 {
	theme::mix(theme::BG_PANEL, color, 0.18)
}

/// The colour of an arrangement role (pattern pictures, role chips).
fn role_color(role: &str) -> Color32 {
	match role {
		"drums" | "fill" => theme::BAND_COLORS[0],
		"perc" | "percussion" => theme::BAND_COLORS[7],
		"bass" => theme::BAND_COLORS[5],
		"comping" | "keys" | "guitar" | "chords" => theme::MARKER,
		"pad" | "strings" => theme::BAND_COLORS[6],
		"lead" | "hook" | "melody" | "vocal" => theme::BAND_COLORS[1],
		"arp" => theme::BAND_COLORS[4],
		_ => theme::DEFAULT_TRACK,
	}
}

/// The colour of a song section (form strips).
fn section_color(name: &str) -> Color32 {
	match cadenza_library::section_name(name).as_str() {
		"verse" => theme::BAND_COLORS[5],
		"pre" => theme::BAND_COLORS[6],
		"chorus" => theme::MARKER,
		"post_chorus" => theme::BAND_COLORS[2],
		"bridge" => theme::BAND_COLORS[4],
		"drop" => theme::BAND_COLORS[0],
		"break" => theme::BAND_COLORS[7],
		"montuno" => theme::BAND_COLORS[1],
		_ => theme::DEFAULT_TRACK,
	}
}

/// "latin_pop" -> "Latin pop".
fn pretty(s: &str) -> String {
	let s = s.replace('_', " ");
	let mut c = s.chars();
	c.next()
		.map(|f| f.to_uppercase().chain(c).collect())
		.unwrap_or_default()
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

fn license_of(library: &Library, item: &LoadedItem) -> String {
	library
		.pack(&item.pack)
		.map_or_else(String::new, |p| p.manifest.license.clone())
}

// --- Small widgets ---------------------------------------------------------------------

/// A filter chip; `on` = selected (accent tint).
fn chip(ui: &mut Ui, label: &str, on: bool) -> egui::Response {
	let font = theme::medium(11.5);
	let g = ui
		.painter()
		.layout_no_wrap(label.to_owned(), font, theme::TEXT);
	let (rect, resp) = ui.allocate_exact_size(vec2(g.size().x + 14.0, 20.0), Sense::click());
	resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Checkbox, true, on, label));
	let hovered = resp.hovered();
	let (fill, fg, stroke) = if on {
		(theme::ACCENT_DIM, theme::ACCENT_TEXT, theme::ACCENT_BORDER)
	} else if hovered {
		(theme::BG_HOVER, theme::TEXT, Color32::TRANSPARENT)
	} else {
		(theme::BG_WIDGET, theme::TEXT_DIM, Color32::TRANSPARENT)
	};
	let p = ui.painter();
	p.rect(
		rect,
		CornerRadius::same(theme::RADIUS_MD),
		fill,
		Stroke::new(1.0, stroke),
		egui::StrokeKind::Inside,
	);
	p.galley(rect.center() - g.size() * 0.5, g, fg);
	resp.on_hover_cursor(CursorIcon::PointingHand)
}

/// A button: `primary` = accent fill.
fn button(ui: &mut Ui, label: &str, primary: bool, height: f32) -> egui::Response {
	let font = theme::medium(12.0);
	let g = ui
		.painter()
		.layout_no_wrap(label.to_owned(), font, theme::TEXT);
	let (rect, resp) = ui.allocate_exact_size(vec2(g.size().x + 20.0, height), Sense::click());
	resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label));
	let hovered = resp.hovered();
	let (fill, fg) = match (primary, hovered) {
		(true, false) => (theme::ACCENT, theme::TEXT_ON_ACCENT),
		(true, true) => (theme::ACCENT_HOVER, theme::TEXT_ON_ACCENT),
		(false, false) => (theme::BG_WIDGET, theme::TEXT),
		(false, true) => (theme::BG_HOVER, theme::TEXT),
	};
	ui.painter()
		.rect_filled(rect, CornerRadius::same(theme::RADIUS_MD), fill);
	ui.painter().galley(rect.center() - g.size() * 0.5, g, fg);
	resp.on_hover_cursor(CursorIcon::PointingHand)
}

/// Dim text that lights up on hover; `accent` = a link.
fn text_button(ui: &mut Ui, label: &str, accent: bool) -> egui::Response {
	let g = ui
		.painter()
		.layout_no_wrap(label.to_owned(), theme::medium(11.5), theme::TEXT);
	let (rect, resp) = ui.allocate_exact_size(vec2(g.size().x + 8.0, 20.0), Sense::click());
	resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label));
	let color = match (accent, resp.hovered()) {
		(true, false) => theme::ACCENT_TEXT,
		(true, true) => theme::ACCENT_HOVER,
		(false, false) => theme::TEXT_FAINT,
		(false, true) => theme::TEXT,
	};
	if resp.hovered() && !accent {
		ui.painter()
			.rect_filled(rect, CornerRadius::same(theme::RADIUS), theme::BG_HOVER);
	}
	ui.painter().galley(
		pos2(rect.left() + 4.0, rect.center().y - g.size().y * 0.5),
		g,
		color,
	);
	resp.on_hover_cursor(CursorIcon::PointingHand)
}

/// The document's close cross.
fn close_button(ui: &mut Ui) -> egui::Response {
	let (rect, resp) = ui.allocate_exact_size(vec2(20.0, 20.0), Sense::click());
	resp.widget_info(|| {
		egui::WidgetInfo::labeled(egui::WidgetType::Button, true, tr("Close document"))
	});
	let color = if resp.hovered() {
		ui.painter()
			.rect_filled(rect, CornerRadius::same(theme::RADIUS), theme::BG_HOVER);
		theme::TEXT
	} else {
		theme::TEXT_FAINT
	};
	let c = rect.center();
	let s = Stroke::new(1.4, color);
	ui.painter()
		.line_segment([c + vec2(-4.0, -4.0), c + vec2(4.0, 4.0)], s);
	ui.painter()
		.line_segment([c + vec2(-4.0, 4.0), c + vec2(4.0, -4.0)], s);
	resp.on_hover_text(tr("Close document"))
		.on_hover_cursor(CursorIcon::PointingHand)
}

/// A kind tag (filled, small caps).
fn paint_tag(p: &Painter, at: egui::Pos2, text: &str, color: Color32) -> Rect {
	let g = p.layout_no_wrap(text.to_owned(), theme::semibold(9.5), color);
	let rect = Rect::from_min_size(at, vec2(g.size().x + 10.0, 15.0));
	p.rect_filled(rect, CornerRadius::same(theme::RADIUS), soft(color));
	p.galley(
		pos2(rect.left() + 5.0, rect.center().y - g.size().y * 0.5),
		g,
		color,
	);
	rect
}

fn tag(ui: &mut Ui, text: &str, color: Color32) {
	let g = ui
		.painter()
		.layout_no_wrap(text.to_owned(), theme::semibold(9.5), color);
	let (rect, _) = ui.allocate_exact_size(vec2(g.size().x + 10.0, 15.0), Sense::hover());
	paint_tag(ui.painter(), rect.min, text, color);
}

fn search_box(ui: &mut Ui, query: &mut String) {
	let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::hover());
	let id = Id::new("library-search");
	let focused = ui.memory(|m| m.has_focus(id));
	ui.painter().rect(
		rect,
		CornerRadius::same(theme::RADIUS_MD),
		theme::BG_DEEP,
		Stroke::new(
			1.0,
			if focused {
				theme::ACCENT
			} else {
				theme::BORDER_STRONG
			},
		),
		egui::StrokeKind::Inside,
	);
	// A magnifier.
	let c = pos2(rect.left() + 14.0, rect.center().y - 1.0);
	let s = Stroke::new(1.3, theme::TEXT_FAINT);
	ui.painter().circle_stroke(c, 4.5, s);
	ui.painter()
		.line_segment([c + vec2(3.2, 3.2), c + vec2(6.5, 6.5)], s);
	let right = if query.is_empty() { 8.0 } else { 28.0 };
	let edit = Rect::from_min_max(
		pos2(rect.left() + 26.0, rect.top() + 6.0),
		pos2(rect.right() - right, rect.bottom() - 4.0),
	);
	ui.put(
		edit,
		egui::TextEdit::singleline(query)
			.id(id)
			.frame(Frame::NONE)
			.font(theme::medium(12.5))
			.hint_text(tr(
				"Search styles, forms, patterns… (latin pop, dembow, bass)",
			))
			.desired_width(edit.width()),
	);
	if !query.is_empty() {
		let x =
			Rect::from_center_size(pos2(rect.right() - 15.0, rect.center().y), vec2(20.0, 20.0));
		let r = ui
			.interact(x, id.with("clear"), Sense::click())
			.on_hover_text(tr("Clear the search"));
		let color = if r.hovered() {
			theme::TEXT
		} else {
			theme::TEXT_FAINT
		};
		let c = x.center();
		let s = Stroke::new(1.3, color);
		ui.painter()
			.line_segment([c + vec2(-3.5, -3.5), c + vec2(3.5, 3.5)], s);
		ui.painter()
			.line_segment([c + vec2(-3.5, 3.5), c + vec2(3.5, -3.5)], s);
		if r.clicked() {
			query.clear();
		}
	}
}

/// All | Styles | Forms | Progr. | Grooves | Patterns with counts; picks `view.kind`.
fn kind_tabs(ui: &mut Ui, view: &mut LibraryView, counts: &[(ItemKind, usize)]) {
	let total: usize = counts.iter().map(|(_, n)| n).sum();
	let count = |k: ItemKind| counts.iter().find(|(c, _)| *c == k).map_or(0, |(_, n)| *n);
	let tabs: [(Option<ItemKind>, &str, usize); 6] = [
		(None, tr("All"), total),
		(Some(ItemKind::Style), tr("Styles"), count(ItemKind::Style)),
		(Some(ItemKind::Form), tr("Forms"), count(ItemKind::Form)),
		(
			Some(ItemKind::Progression),
			tr("Progr."),
			count(ItemKind::Progression),
		),
		(
			Some(ItemKind::Groove),
			tr("Grooves"),
			count(ItemKind::Groove),
		),
		(
			Some(ItemKind::Pattern),
			tr("Patterns"),
			count(ItemKind::Pattern),
		),
	];
	let w = ui.available_width();
	let even = w >= 6.0 * 70.0;
	let (strip, _) = ui.allocate_exact_size(vec2(w, if even { 26.0 } else { 0.0 }), Sense::hover());
	if even {
		ui.painter()
			.rect_filled(strip, CornerRadius::same(theme::RADIUS_MD), theme::BG_DEEP);
	}
	let mut add =
		|ui: &mut Ui, rect: Option<Rect>, kind: Option<ItemKind>, name: &str, n: usize| {
			let on = view.kind == kind;
			let name_g =
				ui.painter()
					.layout_no_wrap(name.to_owned(), theme::medium(11.5), theme::TEXT);
			let n_g =
				ui.painter()
					.layout_no_wrap(n.to_string(), theme::mono(10.5), theme::TEXT_FAINT);
			let width = name_g.size().x + n_g.size().x + 20.0;
			let (rect, resp) = match rect {
				Some(r) => (
					r,
					ui.interact(r, Id::new(("lib-kind-tab", name)), Sense::click()),
				),
				None => ui.allocate_exact_size(vec2(width, 22.0), Sense::click()),
			};
			resp.widget_info(|| {
				egui::WidgetInfo::selected(egui::WidgetType::Button, true, on, name)
			});
			let fill = if on {
				theme::BG_HOVER
			} else if resp.hovered() {
				theme::BG_WIDGET
			} else if rect.height() < 24.0 && !on {
				theme::BG_RAISED
			} else {
				Color32::TRANSPARENT
			};
			let p = ui.painter();
			p.rect_filled(rect.shrink(2.0), CornerRadius::same(theme::RADIUS), fill);
			let total_w = name_g.size().x + 5.0 + n_g.size().x;
			let x = rect.center().x - total_w * 0.5;
			let ny = rect.center().y - name_g.size().y * 0.5;
			let cy = rect.center().y - n_g.size().y * 0.5;
			let nx = x + name_g.size().x + 5.0;
			p.galley(
				pos2(x, ny),
				name_g,
				if on { theme::TEXT } else { theme::TEXT_DIM },
			);
			p.galley(pos2(nx, cy), n_g, theme::TEXT_FAINT);
			if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
				view.kind = kind;
			}
		};
	if even {
		let cw = w / 6.0;
		for (i, (kind, name, n)) in tabs.iter().enumerate() {
			let r = Rect::from_min_size(
				strip.min + vec2(i as f32 * cw, 0.0),
				vec2(cw, strip.height()),
			);
			add(ui, Some(r), *kind, name, *n);
		}
	} else {
		ui.horizontal_wrapped(|ui| {
			ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
			for (kind, name, n) in tabs {
				add(ui, None, kind, name, n);
			}
		});
	}
}

/// A small filter combo for the narrow layout: `None` = "Any".
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
			theme::ACCENT_TEXT
		} else {
			theme::TEXT_DIM
		}))
		.show_ui(ui, |ui| {
			ui.selectable_value(
				value,
				None,
				trf("Any {name}", &[("name", &label.to_lowercase())]),
			);
			for (c, name) in choices {
				ui.selectable_value(value, Some(c.clone()), name);
			}
		});
}

// --- Layouts ---------------------------------------------------------------------------

/// Every match of the text and facet filters (any kind) and the ones of the chosen kind.
fn search<'a>(
	view: &LibraryView,
	library: &'a Library,
) -> (Vec<(ItemKind, usize)>, Vec<&'a LoadedItem>) {
	let mut q = view.query();
	q.kind = None;
	let all = library.search(&q);
	let counts = lib::kind_counts(&all);
	let found = all
		.into_iter()
		.filter(|i| view.kind.is_none_or(|k| i.item.kind() == k))
		.collect();
	(counts, found)
}

fn wide(ui: &mut Ui, state: &mut AppState, view: &mut LibraryView, library: &Library) {
	let full = ui.available_rect_before_wrap();
	ui.allocate_rect(full, Sense::hover());
	let doc_open = view.credits || view.open.is_some();
	let beside = doc_open && full.width() >= DOC_BESIDE;
	let doc_w = if beside {
		((full.width() - FILTERS_W) * 0.47).clamp(380.0, 560.0)
	} else {
		0.0
	};
	let filters = Rect::from_min_size(full.min, vec2(FILTERS_W, full.height()));
	let mid = Rect::from_min_max(
		pos2(filters.right(), full.top()),
		pos2(full.right() - doc_w, full.bottom()),
	);
	let doc = Rect::from_min_max(pos2(mid.right(), full.top()), full.max);
	let line = Stroke::new(1.0, theme::BORDER);
	ui.painter().vline(filters.right(), full.y_range(), line);
	column(
		ui,
		"lib-filters",
		filters.with_max_x(filters.right() - 10.0),
		|ui| filter_column(ui, view, library),
	);
	let mid_inner = Rect::from_min_max(
		pos2(mid.left() + 10.0, mid.top()),
		pos2(mid.right() - if beside { 10.0 } else { 0.0 }, mid.bottom()),
	);
	column(ui, "lib-results", mid_inner, |ui| {
		if doc_open && !beside {
			document_or_credits(ui, state, view, library, false);
		} else {
			results(ui, state, view, library);
		}
	});
	if beside {
		ui.painter().vline(doc.left(), full.y_range(), line);
		column(ui, "lib-doc", doc.with_min_x(doc.left() + 14.0), |ui| {
			document_or_credits(ui, state, view, library, false)
		});
	}
}

/// A column of the wide layout: content wider than `rect` is clipped instead of widening
/// the browser panel.
fn column(ui: &mut Ui, salt: &str, rect: Rect, add: impl FnOnce(&mut Ui)) {
	let mut child = ui.new_child(UiBuilder::new().id_salt(salt).max_rect(rect));
	child.set_clip_rect(rect.intersect(ui.clip_rect()));
	add(&mut child);
}

fn narrow(ui: &mut Ui, state: &mut AppState, view: &mut LibraryView, library: &Library) {
	if view.credits || view.open.is_some() {
		document_or_credits(ui, state, view, library, true);
		return;
	}
	search_box(ui, &mut view.query);
	ui.add_space(6.0);
	let (counts, found) = search(view, library);
	kind_tabs(ui, view, &counts);
	ui.add_space(4.0);
	ui.horizontal_wrapped(|ui| {
		ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
		let genres: Vec<(String, String)> = lib::genres(library)
			.into_iter()
			.map(|g| (g.clone(), pretty(&g)))
			.collect();
		filter(ui, "lib-genre", tr("Genre"), &mut view.genre, &genres);
		let roles: Vec<(Role, String)> =
			Role::ALL.iter().map(|r| (*r, pretty(r.as_str()))).collect();
		filter(ui, "lib-role", tr("Role"), &mut view.role, &roles);
		let sections: Vec<(String, String)> = lib::SECTIONS
			.iter()
			.map(|s| ((*s).to_owned(), pretty(s)))
			.collect();
		filter(
			ui,
			"lib-section",
			tr("Section"),
			&mut view.section,
			&sections,
		);
		let energies: Vec<(u8, String)> = (1..=5)
			.map(|e| (e, trf("Energy {e}", &[("e", &e)])))
			.collect();
		filter(ui, "lib-energy", tr("Energy"), &mut view.energy, &energies);
	});
	ui.horizontal(|ui| {
		ui.label(faint(results_text(found.len())));
		ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
			if text_button(ui, tr("Credits and licenses"), true)
				.on_hover_text(tr("Installed packs, their licenses and attribution"))
				.clicked()
			{
				view.credits = true;
			}
			if view.filtered() && text_button(ui, tr("Clear"), false).clicked() {
				view.clear_filters();
			}
		});
	});
	ui.add_space(4.0);
	card_grid(ui, state, view, library, &found);
}

fn results_text(n: usize) -> String {
	trn(
		"{n} result · sorted by fit",
		"{n} results · sorted by fit",
		n,
	)
}

fn results(ui: &mut Ui, state: &mut AppState, view: &mut LibraryView, library: &Library) {
	ui.add_space(4.0);
	search_box(ui, &mut view.query);
	ui.add_space(8.0);
	let (counts, found) = search(view, library);
	kind_tabs(ui, view, &counts);
	ui.add_space(6.0);
	ui.horizontal(|ui| {
		ui.label(faint(results_text(found.len())));
		ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
			ui.label(faint(tr("Click = audition · drag to arrange")));
		});
	});
	ui.add_space(4.0);
	card_grid(ui, state, view, library, &found);
}

fn facet_title(ui: &mut Ui, title: &str) {
	ui.add_space(10.0);
	ui.label(
		RichText::new(title)
			.font(theme::semibold(11.5))
			.color(theme::TEXT_DIM),
	);
	ui.add_space(3.0);
}

fn chips<T: Clone + PartialEq>(ui: &mut Ui, value: &mut Option<T>, choices: &[(T, String)]) {
	ui.horizontal_wrapped(|ui| {
		ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
		for (c, name) in choices {
			let on = value.as_ref() == Some(c);
			if chip(ui, name, on).clicked() {
				*value = if on { None } else { Some(c.clone()) };
			}
		}
	});
}

fn filter_column(ui: &mut Ui, view: &mut LibraryView, library: &Library) {
	egui::ScrollArea::vertical()
		.id_salt("library-filters")
		.auto_shrink([false, false])
		.show(ui, |ui| {
			ui.horizontal(|ui| {
				facet_title(ui, tr("Genre"));
				if view.filtered() {
					ui.with_layout(egui::Layout::right_to_left(egui::Align::Max), |ui| {
						if text_button(ui, tr("Clear"), false)
							.on_hover_text(tr("Clear every filter"))
							.clicked()
						{
							view.clear_filters();
						}
					});
				}
			});
			// The most used genres; the rest behind "More" (the chosen one always shows).
			let more_id = Id::new("library-more-genres");
			let more = ui.data(|d| d.get_temp::<bool>(more_id).unwrap_or(false));
			let all = lib::genres(library);
			let hidden = all.len().saturating_sub(GENRES_SHOWN);
			let genres: Vec<(String, String)> = all
				.into_iter()
				.enumerate()
				.filter(|(i, g)| more || *i < GENRES_SHOWN || view.genre.as_ref() == Some(g))
				.map(|(_, g)| (g.clone(), pretty(&g)))
				.collect();
			chips(ui, &mut view.genre, &genres);
			if hidden > 0 {
				let label = if more {
					tr("Fewer genres").to_owned()
				} else {
					trf("+ {hidden} more", &[("hidden", &hidden)])
				};
				if text_button(ui, &label, true).clicked() {
					ui.data_mut(|d| d.insert_temp(more_id, !more));
				}
			}
			facet_title(ui, tr("Role"));
			let roles: Vec<(Role, String)> =
				Role::ALL.iter().map(|r| (*r, pretty(r.as_str()))).collect();
			chips(ui, &mut view.role, &roles);
			facet_title(ui, tr("Section"));
			let sections: Vec<(String, String)> = lib::SECTIONS
				.iter()
				.map(|s| ((*s).to_owned(), pretty(s)))
				.collect();
			chips(ui, &mut view.section, &sections);
			facet_title(ui, tr("Energy"));
			let w = ui.available_width();
			let (strip, _) = ui.allocate_exact_size(vec2(w, 22.0), Sense::hover());
			let cw = (w - 4.0 * 3.0) / 5.0;
			for e in 1..=5u8 {
				let r = Rect::from_min_size(
					strip.min + vec2(f32::from(e - 1) * (cw + 3.0), 0.0),
					vec2(cw, 22.0),
				);
				let label = e.to_string();
				let resp = ui.interact(r, Id::new(("lib-energy", e)), Sense::click());
				let on = view.energy == Some(e);
				resp.widget_info(|| {
					egui::WidgetInfo::selected(
						egui::WidgetType::Checkbox,
						true,
						on,
						trf("Energy {label}", &[("label", &label)]),
					)
				});
				let fill = if on {
					theme::ACCENT_DIM
				} else if resp.hovered() {
					theme::BG_HOVER
				} else {
					theme::BG_WIDGET
				};
				ui.painter()
					.rect_filled(r, CornerRadius::same(theme::RADIUS), fill);
				ui.painter().text(
					r.center(),
					egui::Align2::CENTER_CENTER,
					label,
					theme::mono(11.0),
					if on {
						theme::ACCENT_TEXT
					} else {
						theme::TEXT_DIM
					},
				);
				let resp = resp
					.on_hover_text(tr("1 = sparse … 5 = full"))
					.on_hover_cursor(CursorIcon::PointingHand);
				if resp.clicked() {
					view.energy = if on { None } else { Some(e) };
				}
			}
			facet_title(ui, tr("Packs"));
			for pack in library.packs() {
				let m = &pack.manifest;
				ui.add(
					egui::Label::new(faint(trf(
						"{id} · {pack} items · {license}",
						&[
							("id", &m.id),
							("pack", &pack.items.len()),
							("license", &m.license),
						],
					)))
					.truncate(),
				);
			}
			ui.add_space(2.0);
			if text_button(ui, tr("Credits and licenses"), true)
				.on_hover_text(tr("Installed packs, their licenses and attribution"))
				.clicked()
			{
				view.open = None;
				view.credits = true;
			}
		});
}

// --- Cards -----------------------------------------------------------------------------

fn card_grid(
	ui: &mut Ui,
	state: &mut AppState,
	view: &mut LibraryView,
	library: &Library,
	found: &[&LoadedItem],
) {
	egui::ScrollArea::vertical()
		.id_salt("library-list")
		.auto_shrink([false, false])
		.show(ui, |ui| {
			if found.is_empty() {
				ui.add_space(12.0);
				ui.label(faint(tr(
					"Nothing matches. Try fewer words or clear the filters.",
				)));
				return;
			}
			let w = ui.available_width();
			let cols = (((w + GAP) / (CARD_MIN_W + GAP)).floor() as usize).clamp(1, 4);
			let cw = (w - GAP * (cols - 1) as f32) / cols as f32;
			let rows = found.len().div_ceil(cols);
			let (area, _) =
				ui.allocate_exact_size(vec2(w, rows as f32 * (CARD_H + GAP)), Sense::hover());
			for (i, item) in found.iter().enumerate() {
				let rect = Rect::from_min_size(
					area.min
						+ vec2(
							(i % cols) as f32 * (cw + GAP),
							(i / cols) as f32 * (CARD_H + GAP),
						),
					vec2(cw, CARD_H),
				);
				if ui.is_rect_visible(rect) {
					card_ui(ui, state, view, library, item, rect);
				}
			}
		});
}

/// What a card's picture shows, gathered before painting (the drag body cannot borrow
/// the state).
enum Picture {
	Pattern(Arc<MiniPicture>, Color32),
	Numerals(Vec<String>),
	Groove(Vec<f64>, Vec<f64>),
	Sections(Vec<(String, u32)>, Option<String>),
}

struct CardLook {
	name: String,
	subtitle: String,
	meta: String,
	license: String,
	energy: Option<u8>,
	picture: Picture,
	selected: bool,
	playing: bool,
	progress: Option<f32>,
	pattern: bool,
}

/// The pattern's picture, realized once and kept in egui's memory.
fn mini(ctx: &egui::Context, p: &Pattern) -> Arc<MiniPicture> {
	let id = Id::new(("library-mini", &p.id));
	if let Some(v) = ctx.data(|d| d.get_temp::<Arc<MiniPicture>>(id)) {
		return v;
	}
	let v = Arc::new(lib::mini_picture(p));
	ctx.data_mut(|d| d.insert_temp(id, v.clone()));
	v
}

fn form_sections(library: &Library, id: &str) -> Vec<(String, u32)> {
	match library.item(id).map(|i| &i.item) {
		Some(Item::Form(f)) => f
			.sections
			.iter()
			.map(|s| (s.name.clone(), s.bars))
			.collect(),
		_ => Vec::new(),
	}
}

fn numerals(library: &Library, id: &str) -> Option<String> {
	match library.item(id).map(|i| &i.item) {
		Some(Item::Progression(p)) => Some(p.numerals.clone()),
		_ => None,
	}
}

fn picture(ctx: &egui::Context, library: &Library, item: &LoadedItem) -> Picture {
	match &item.item {
		Item::Pattern(p) => Picture::Pattern(mini(ctx, p), role_color(p.role.as_str())),
		Item::Progression(p) => {
			Picture::Numerals(p.numerals.split_whitespace().map(str::to_owned).collect())
		}
		Item::Groove(g) => Picture::Groove(g.timing.clone(), g.velocity.clone()),
		Item::Form(f) => Picture::Sections(
			f.sections
				.iter()
				.map(|s| (s.name.clone(), s.bars))
				.collect(),
			None,
		),
		Item::Style(s) => Picture::Sections(
			s.forms
				.first()
				.map(|f| form_sections(library, f))
				.unwrap_or_default(),
			s.harmony.loops.first().and_then(|l| numerals(library, l)),
		),
	}
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
	rect: Rect,
) {
	let playing = state.pattern_preview() == Some(item.id.as_str());
	let look = CardLook {
		name: item.item.common().name.to_owned(),
		subtitle: lib::card_subtitle(item),
		meta: lib::card_meta(item),
		license: format!("{} · {}", item.pack, license_of(library, item)),
		energy: lib::card_energy(item),
		picture: picture(ui.ctx(), library, item),
		selected: view.selected.as_deref() == Some(item.id.as_str()),
		playing,
		progress: playing.then(|| state.pattern_preview_progress()).flatten(),
		pattern: item.item.kind() == ItemKind::Pattern,
	};
	let (kind, kind_color) = kind_look(item);
	let id = Id::new(("library-card", &item.id));
	let body = |ui: &mut Ui| {
		let (r, _) = ui.allocate_exact_size(rect.size(), Sense::hover());
		paint_card(ui, r, &look, kind, kind_color);
		r
	};
	let mut child = ui.new_child(UiBuilder::new().id_salt(id).max_rect(rect));
	let drawn = match &item.item {
		Item::Pattern(p) => {
			child
				.dnd_drag_source(
					id,
					Payload::Pattern {
						id: item.id.clone(),
						name: p.name.clone(),
					},
					body,
				)
				.inner
		}
		_ => body(&mut child),
	};
	let c = card(item);
	let hover = match &item.item {
		Item::Pattern(p) => format!(
			"{}\n{}\n\n{}\n\n{}",
			p.summary,
			c.facts,
			grid_text(p),
			tr(
				"Click: preview at the song tempo · Double-click: write it at the playhead · \
			    Drag: onto an instrument track or below the tracks"
			)
		),
		_ => format!("{}\n{}\n\n{}", c.summary, c.facts, tr("Click to open")),
	};
	let response = ui
		.interact(drawn, id.with("click"), Sense::click())
		.on_hover_text(RichText::new(hover).font(theme::mono(11.0)))
		.on_hover_cursor(CursorIcon::PointingHand);
	if response.hovered() && !look.selected && !look.playing {
		ui.painter().rect_stroke(
			drawn,
			CornerRadius::same(theme::RADIUS_LG),
			Stroke::new(1.0, theme::BORDER_STRONG),
			egui::StrokeKind::Inside,
		);
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
			view.credits = false;
			view.open = Some(item.id.clone());
		}
	}
}

fn paint_card(ui: &mut Ui, r: Rect, look: &CardLook, kind: &str, kind_color: Color32) {
	let p = ui.painter().with_clip_rect(r.intersect(ui.clip_rect()));
	let (fill, stroke) = if look.playing {
		(
			theme::mix(theme::BG_RAISED, theme::PLAYING, 0.07),
			theme::PLAYING,
		)
	} else if look.selected {
		(theme::BG_RAISED, theme::ACCENT)
	} else {
		(theme::BG_RAISED, theme::BORDER_SUBTLE)
	};
	p.rect(
		r,
		CornerRadius::same(theme::RADIUS_LG),
		fill,
		Stroke::new(1.0, stroke),
		egui::StrokeKind::Inside,
	);
	let pad = 10.0;
	let inner = r.shrink(pad);
	let tag = paint_tag(&p, inner.min, kind, kind_color);
	p.with_clip_rect(Rect::from_min_max(
		pos2(tag.right() + 6.0, inner.top()),
		pos2(inner.right(), inner.top() + 16.0),
	))
	.text(
		pos2(inner.right(), tag.center().y),
		egui::Align2::RIGHT_CENTER,
		&look.license,
		theme::mono(9.5),
		theme::TEXT_FAINT,
	);
	let name = Rect::from_min_size(
		pos2(inner.left(), inner.top() + 20.0),
		vec2(inner.width(), 18.0),
	);
	ui.scope_builder(
		UiBuilder::new()
			.max_rect(name)
			.layout(egui::Layout::left_to_right(egui::Align::Center)),
		|ui| {
			ui.add(
				egui::Label::new(
					RichText::new(&look.name)
						.font(theme::semibold(13.0))
						.color(theme::TEXT),
				)
				.selectable(false)
				.truncate(),
			);
		},
	);
	p.with_clip_rect(Rect::from_min_size(
		pos2(inner.left(), name.bottom()),
		vec2(inner.width(), 16.0),
	))
	.text(
		pos2(inner.left(), name.bottom() + 1.0),
		egui::Align2::LEFT_TOP,
		&look.subtitle,
		theme::small(),
		theme::TEXT_FAINT,
	);
	let footer_h = 20.0;
	let preview = Rect::from_min_max(
		pos2(inner.left(), name.bottom() + 20.0),
		pos2(inner.right(), inner.bottom() - footer_h - 6.0),
	);
	paint_picture(&p, preview, &look.picture, look.progress);
	// Footer: play / open button, meta, energy.
	let fy = inner.bottom() - footer_h * 0.5;
	let c = pos2(inner.left() + 9.0, fy);
	if look.playing {
		p.circle_filled(c, 9.0, theme::PLAYING);
		p.rect_filled(
			Rect::from_center_size(c, vec2(6.0, 6.0)),
			CornerRadius::same(1),
			theme::TEXT_ON_COLOR,
		);
	} else {
		p.circle_filled(c, 9.0, theme::BG_HOVER);
		if look.pattern {
			p.add(egui::Shape::convex_polygon(
				vec![
					c + vec2(-2.5, -4.0),
					c + vec2(4.0, 0.0),
					c + vec2(-2.5, 4.0),
				],
				theme::TEXT_DIM,
				Stroke::NONE,
			));
		} else {
			p.line(
				vec![
					c + vec2(-1.5, -3.5),
					c + vec2(2.0, 0.0),
					c + vec2(-1.5, 3.5),
				],
				Stroke::new(1.4, theme::TEXT_DIM),
			);
		}
	}
	let dots_w = if look.energy.is_some() { 36.0 } else { 0.0 };
	p.with_clip_rect(Rect::from_min_max(
		pos2(c.x + 14.0, fy - 8.0),
		pos2(inner.right() - dots_w - 4.0, fy + 8.0),
	))
	.text(
		pos2(c.x + 15.0, fy),
		egui::Align2::LEFT_CENTER,
		&look.meta,
		theme::small(),
		theme::TEXT_DIM,
	);
	if let Some(e) = look.energy {
		for i in 0..5u8 {
			let d = pos2(inner.right() - 2.5 - f32::from(4 - i) * 7.0, fy);
			p.circle_filled(
				d,
				2.5,
				if i < e {
					theme::TEXT
				} else {
					theme::BG_PRESSED
				},
			);
		}
	}
}

fn paint_picture(p: &Painter, rect: Rect, picture: &Picture, progress: Option<f32>) {
	if rect.height() < 8.0 {
		return;
	}
	p.rect_filled(
		rect,
		CornerRadius::same(theme::RADIUS_MD),
		theme::DISPLAY_BG,
	);
	let inner = rect.shrink2(vec2(6.0, 6.0));
	match picture {
		Picture::Pattern(pic, color) => {
			for b in 1..pic.bars.max(1) {
				let x = inner.left() + inner.width() * b as f32 / pic.bars as f32;
				p.vline(x, rect.y_range(), Stroke::new(1.0, theme::GRID_BAR));
			}
			let rows = pic.rows.max(1) as f32;
			for n in &pic.notes {
				let x = inner.left() + n.x * inner.width();
				let color = color.gamma_multiply(0.45 + 0.55 * n.level.clamp(0.0, 1.0));
				if pic.drums {
					let h = (inner.height() / rows * 0.55).clamp(2.0, 4.0);
					let y = inner.bottom() - h * 0.5 - n.y * (inner.height() - h);
					let w = (n.w * inner.width() * 0.55).clamp(2.5, 6.0);
					p.rect_filled(
						Rect::from_center_size(pos2(x + w * 0.5, y), vec2(w, h)),
						CornerRadius::same(1),
						color,
					);
				} else {
					let y = inner.bottom() - 1.5 - n.y * (inner.height() - 3.0);
					let w = (n.w * inner.width() - 1.0).max(2.0);
					p.rect_filled(
						Rect::from_min_size(pos2(x, y - 1.5), vec2(w, 3.0)),
						CornerRadius::same(1),
						color,
					);
				}
			}
			if let Some(t) = progress {
				let x = inner.left() + t * inner.width();
				p.vline(x, rect.y_range(), Stroke::new(1.5, theme::PLAYING));
			}
		}
		Picture::Numerals(chords) => {
			let n = chords.len().max(1);
			let cw = rect.width() / n as f32;
			for (i, c) in chords.iter().enumerate() {
				let cell = Rect::from_min_size(
					rect.min + vec2(i as f32 * cw, 0.0),
					vec2(cw, rect.height()),
				);
				if i > 0 {
					p.vline(
						cell.left(),
						cell.y_range().shrink(6.0),
						Stroke::new(1.0, theme::BORDER),
					);
				}
				p.text(
					cell.center(),
					egui::Align2::CENTER_CENTER,
					c,
					theme::mono(12.0),
					theme::TEXT,
				);
			}
		}
		Picture::Groove(timing, velocity) => {
			let n = timing.len().max(1);
			let mid = inner.center().y;
			p.hline(inner.x_range(), mid, Stroke::new(1.0, theme::GRID_BAR));
			for (i, t) in timing.iter().enumerate() {
				let v = velocity.get(i).copied().unwrap_or(1.0) as f32;
				let x = inner.left() + (i as f32 + 0.5 + *t as f32) / n as f32 * inner.width();
				let y = mid - (v - 1.0).clamp(-1.0, 1.0) * inner.height() * 0.45;
				p.circle_filled(pos2(x, y), 2.2, theme::LOOP);
			}
		}
		Picture::Sections(sections, loop_numerals) => {
			let strip = Rect::from_min_size(
				pos2(
					inner.left(),
					inner.center().y - if loop_numerals.is_some() { 14.0 } else { 8.0 },
				),
				vec2(inner.width(), 16.0),
			);
			paint_sections(p, strip, sections, true);
			if let Some(l) = loop_numerals {
				p.text(
					pos2(inner.left(), strip.bottom() + 6.0),
					egui::Align2::LEFT_TOP,
					l,
					theme::mono(11.0),
					theme::TEXT_DIM,
				);
			}
		}
	}
}

/// A form as blocks sized by bars, in section colours, named when they fit.
fn paint_sections(p: &Painter, rect: Rect, sections: &[(String, u32)], labels: bool) {
	let total: u32 = sections.iter().map(|(_, b)| *b).sum();
	if total == 0 {
		return;
	}
	let mut x = rect.left();
	for (name, bars) in sections {
		let w = rect.width() * *bars as f32 / total as f32;
		let block = Rect::from_min_max(pos2(x, rect.top()), pos2(x + w - 1.0, rect.bottom()));
		p.rect_filled(block, CornerRadius::same(2), section_color(name));
		if labels && block.width() > 22.0 {
			p.with_clip_rect(block.shrink2(vec2(3.0, 0.0))).text(
				pos2(block.left() + 3.0, block.center().y),
				egui::Align2::LEFT_CENTER,
				name,
				theme::medium(10.0),
				theme::TEXT_ON_COLOR,
			);
		}
		x += w;
	}
}

// --- Documents -------------------------------------------------------------------------

fn document_or_credits(
	ui: &mut Ui,
	state: &mut AppState,
	view: &mut LibraryView,
	library: &Library,
	back: bool,
) {
	if view.credits {
		credits(ui, view, library, back);
	} else if let Some(id) = view.open.clone()
		&& let Some(item) = library.item(&id)
	{
		document(ui, state, view, library, item, back);
	}
}

/// "‹ Library" (narrow) or the close cross (wide) on the left / right of the header.
fn doc_header(ui: &mut Ui, view: &mut LibraryView, back: bool, add: impl FnOnce(&mut Ui)) {
	ui.add_space(6.0);
	ui.horizontal(|ui| {
		if back
			&& text_button(ui, tr("‹ Library"), false)
				.on_hover_text(tr("Back to the list"))
				.clicked()
		{
			view.open = None;
			view.credits = false;
		}
		add(ui);
		if !back {
			ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
				if close_button(ui).clicked() {
					view.open = None;
					view.credits = false;
				}
			});
		}
	});
}

fn section_title(ui: &mut Ui, title: &str) {
	ui.add_space(14.0);
	ui.label(
		RichText::new(title)
			.font(theme::semibold(13.0))
			.color(theme::TEXT),
	);
	ui.add_space(4.0);
}

/// A key / value row: the key in a fixed dim column, the value wrapping.
fn kv(ui: &mut Ui, key: &str, value: impl FnOnce(&mut Ui)) {
	let width = ui.available_width();
	ui.horizontal_top(|ui| {
		let (r, _) = ui.allocate_exact_size(vec2(84.0, 16.0), Sense::hover());
		ui.painter().text(
			r.left_center(),
			egui::Align2::LEFT_CENTER,
			key,
			theme::medium(11.5),
			theme::TEXT_FAINT,
		);
		let rest = (width - 84.0 - ui.spacing().item_spacing.x).max(60.0);
		ui.allocate_ui_with_layout(
			vec2(rest, 0.0),
			egui::Layout::top_down(egui::Align::Min),
			|ui| {
				ui.set_max_width(rest);
				value(ui);
			},
		);
	});
	ui.add_space(2.0);
}

fn value_text(text: impl Into<String>) -> RichText {
	RichText::new(text)
		.font(theme::medium(12.0))
		.color(theme::TEXT)
}

fn document(
	ui: &mut Ui,
	state: &mut AppState,
	view: &mut LibraryView,
	library: &Library,
	item: &LoadedItem,
	back: bool,
) {
	let common = item.item.common();
	let (kind, kind_color) = kind_look(item);
	let license = license_of(library, item);
	doc_header(ui, view, back, |ui| {
		tag(ui, kind, kind_color);
		ui.add(egui::Label::new(faint(format!("{} · {license}", item.id))).truncate());
	});
	egui::ScrollArea::vertical()
		.id_salt(("library-doc", &item.id))
		.auto_shrink([false, false])
		.show(ui, |ui| {
			ui.set_width(ui.available_width() - 6.0);
			ui.add_space(4.0);
			let mut title = egui::text::LayoutJob::default();
			let (main, alias) = match common.name.split_once(" (") {
				Some((m, a)) => (m, Some(format!(" ({a}"))),
				None => (common.name, None),
			};
			title.append(
				main,
				0.0,
				egui::TextFormat::simple(theme::semibold(17.0), theme::TEXT),
			);
			if let Some(a) = alias {
				title.append(
					&a,
					0.0,
					egui::TextFormat::simple(theme::medium(17.0), theme::TEXT_FAINT),
				);
			}
			ui.add(egui::Label::new(title).wrap());
			ui.add_space(2.0);
			ui.add(
				egui::Label::new(
					RichText::new(common.summary)
						.font(theme::medium(12.0))
						.color(theme::TEXT_DIM),
				)
				.wrap(),
			);
			match &item.item {
				Item::Style(s) => style_doc(ui, state, library, s),
				Item::Form(f) => {
					let style = styles_listing(library, &item.id);
					ui.add_space(10.0);
					apply_form_button(ui, state, &item.id, style.as_deref());
					form_doc(
						ui,
						f,
						style.as_deref().and_then(|s| style_tempo(library, s)),
					);
				}
				Item::Progression(p) => progression_doc(ui, p),
				Item::Groove(g) => groove_doc(ui, g),
				Item::Pattern(p) => {
					section_title(ui, tr("Pattern"));
					let (r, _) =
						ui.allocate_exact_size(vec2(ui.available_width(), 110.0), Sense::hover());
					paint_picture(
						ui.painter(),
						r,
						&Picture::Pattern(mini(ui.ctx(), p), role_color(p.role.as_str())),
						None,
					);
					section_title(ui, tr("Lanes"));
					ui.label(
						RichText::new(grid_text(p))
							.font(theme::mono(11.0))
							.color(theme::TEXT),
					);
				}
			}
			ui.add_space(12.0);
		});
}

fn style_tempo(library: &Library, style: &str) -> Option<(f64, String)> {
	match library.item(style).map(|i| &i.item) {
		Some(Item::Style(s)) => Some((s.tempo.typical, s.meter.clone())),
		_ => None,
	}
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
	let text = match style {
		Some(s) => trf(
			"Lays out the form's sections as markers from bar 1 and sets the song's style ({s}) and its typical tempo. One undo step.",
			&[("s", &s)],
		),
		None => tr("Lays out the form's sections as markers from bar 1. One undo step.").into(),
	};
	if button(ui, tr("Apply form"), true, 26.0)
		.on_hover_text(text)
		.clicked()
	{
		state.apply_library_form(form, style);
	}
}

fn form_doc(ui: &mut Ui, f: &cadenza_library::Form, tempo: Option<(f64, String)>) {
	let length = match &tempo {
		Some((bpm, meter)) => lib::form_length(f.total_bars(), *bpm, meter),
		None => trf("{total_bars} bars", &[("total_bars", &f.total_bars())]),
	};
	section_title(ui, &trf("Sections · {length}", &[("length", &length)]));
	let sections: Vec<(String, u32)> = f
		.sections
		.iter()
		.map(|s| (s.name.clone(), s.bars))
		.collect();
	let (strip, _) = ui.allocate_exact_size(vec2(ui.available_width(), 22.0), Sense::hover());
	paint_sections(ui.painter(), strip, &sections, true);
	ui.add_space(8.0);
	let mut bar = 1;
	egui::Grid::new(("form-sections", &f.id))
		.num_columns(5)
		.spacing(vec2(12.0, 5.0))
		.show(ui, |ui| {
			for s in &f.sections {
				ui.horizontal(|ui| {
					let (r, _) = ui.allocate_exact_size(vec2(8.0, 8.0), Sense::hover());
					ui.painter()
						.rect_filled(r, CornerRadius::same(2), section_color(&s.name));
					ui.label(value_text(&s.name));
				});
				ui.label(faint(trf(
					"bars {from}–{to}",
					&[("from", &bar), ("to", &(bar + s.bars - 1))],
				)));
				bar += s.bars;
				ui.label(caption(trf("{bars} bars", &[("bars", &s.bars)])));
				energy_meter(ui, s.energy).on_hover_text(trf(
					"energy {energy}{ending}",
					&[
						("energy", &s.energy),
						(
							"ending",
							&s.ending
								.map(|e| format!("\nending: {e:?}").to_lowercase())
								.unwrap_or_default(),
						),
					],
				));
				ui.add(egui::Label::new(faint(s.roles.join(", "))).truncate());
				ui.end_row();
			}
		});
}

/// Five small blocks, `energy` of them lit.
fn energy_meter(ui: &mut Ui, energy: u8) -> egui::Response {
	let (rect, response) = ui.allocate_exact_size(vec2(5.0 * 9.0, 10.0), Sense::hover());
	for i in 0..5u8 {
		let block = Rect::from_min_size(rect.min + vec2(f32::from(i) * 9.0, 1.0), vec2(7.0, 8.0));
		let color = if i < energy {
			theme::mix(theme::ACCENT_DIM, theme::ACCENT, 0.4 + 0.15 * f32::from(i))
		} else {
			theme::BG_WIDGET
		};
		ui.painter()
			.rect_filled(block, CornerRadius::same(1), color);
	}
	response
}

/// One cell per numeral in a row.
fn numeral_cells(ui: &mut Ui, numerals: &str, width: f32, height: f32) -> egui::Response {
	let chords: Vec<&str> = numerals.split_whitespace().collect();
	let (rect, resp) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
	let n = chords.len().max(1);
	let cw = (rect.width() - 3.0 * (n - 1) as f32) / n as f32;
	for (i, c) in chords.iter().enumerate() {
		let cell = Rect::from_min_size(
			rect.min + vec2(i as f32 * (cw + 3.0), 0.0),
			vec2(cw, rect.height()),
		);
		ui.painter()
			.rect_filled(cell, CornerRadius::same(theme::RADIUS), theme::BG_WIDGET);
		ui.painter().text(
			cell.center(),
			egui::Align2::CENTER_CENTER,
			*c,
			theme::mono(12.5),
			theme::TEXT,
		);
	}
	resp
}

fn chord_names(numerals: &str, key: &str) -> Option<String> {
	let scale = cadenza_theory::Scale::parse(key).ok()?;
	let chords = cadenza_theory::parse_roman_progression(numerals, &scale).ok()?;
	Some(
		chords
			.iter()
			.map(cadenza_mcp::tools::library::chord_name)
			.collect::<Vec<_>>()
			.join(" "),
	)
}

fn progression_doc(ui: &mut Ui, p: &cadenza_library::Progression) {
	section_title(ui, tr("Numerals"));
	numeral_cells(ui, &p.numerals, ui.available_width(), 34.0);
	ui.add_space(4.0);
	ui.label(faint(trf(
		"{mode} · {beats_per_chord} beats per chord",
		&[("mode", &p.mode), ("beats_per_chord", &p.beats_per_chord)],
	)));
	section_title(ui, tr("In a key"));
	for root in ["A", "C", "E"] {
		let key = format!("{root} {}", p.mode);
		if let Some(names) = chord_names(&p.numerals, &key) {
			kv(ui, &key, |ui| {
				ui.label(
					RichText::new(names)
						.font(theme::mono(12.0))
						.color(theme::TEXT),
				);
			});
		}
	}
}

fn groove_doc(ui: &mut Ui, g: &cadenza_library::Groove) {
	section_title(ui, tr("Timing and velocity"));
	let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 90.0), Sense::hover());
	paint_picture(
		ui.painter(),
		r,
		&Picture::Groove(g.timing.clone(), g.velocity.clone()),
		None,
	);
	ui.add_space(2.0);
	ui.label(faint(tr(
		"Dots: early/late within each step (x), louder/softer (y).",
	)));
	section_title(ui, tr("Steps"));
	for (i, t) in g.timing.iter().enumerate() {
		let v = g.velocity.get(i).copied().unwrap_or(1.0);
		ui.label(
			RichText::new(format!("step {:>2}  {:+.2}  ×{:.2}", i + 1, t, v))
				.font(theme::mono(11.0))
				.color(theme::TEXT_DIM),
		);
	}
}

/// A pattern id as a chip with its role colour: click previews, drag writes it.
fn pattern_chip(
	ui: &mut Ui,
	state: &mut AppState,
	library: &Library,
	id: &str,
	sections: &[String],
	color: Color32,
) {
	let Some(p) = library.pattern(id) else {
		ui.label(faint(id));
		return;
	};
	let previewing = state.pattern_preview() == Some(p.id.as_str());
	let drag = Id::new(("library-chip", &p.id, ui.next_auto_id()));
	// Room is taken in the wrapping row first so the chips wrap (a drag source is a
	// scope of its own, which never wraps).
	let g = ui
		.painter()
		.layout_no_wrap(p.name.clone(), theme::medium(11.5), theme::TEXT);
	let size = vec2(g.size().x + 16.0, 20.0);
	let (slot, _) = ui.allocate_exact_size(size, Sense::hover());
	let mut child = ui.new_child(UiBuilder::new().id_salt(drag).max_rect(slot));
	let r = child
		.dnd_drag_source(
			drag,
			Payload::Pattern {
				id: p.id.clone(),
				name: p.name.clone(),
			},
			|ui| {
				let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
				let painter = ui.painter();
				painter.rect(
					rect,
					CornerRadius::same(theme::RADIUS),
					if previewing {
						theme::PLAYING_SOFT
					} else {
						theme::BG_WIDGET
					},
					Stroke::new(
						1.0,
						if previewing {
							theme::PLAYING
						} else {
							Color32::TRANSPARENT
						},
					),
					egui::StrokeKind::Inside,
				);
				painter.rect_filled(
					Rect::from_min_size(rect.min, vec2(2.5, rect.height())),
					CornerRadius {
						nw: theme::RADIUS,
						sw: theme::RADIUS,
						..Default::default()
					},
					color,
				);
				painter.galley(
					pos2(rect.left() + 9.0, rect.center().y - g.size().y * 0.5),
					g,
					theme::TEXT,
				);
				rect
			},
		)
		.inner;
	let r = ui
		.interact(r, drag.with("click"), Sense::click())
		.on_hover_text(format!(
			"{}\n{}\nsections: {}\n\nClick: preview · Drag: onto a track",
			p.id,
			p.summary,
			sections
				.iter()
				.map(|s| s.replace('_', " "))
				.collect::<Vec<_>>()
				.join(", ")
		))
		.on_hover_cursor(CursorIcon::PointingHand);
	r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &p.name));
	if r.clicked() {
		state.preview_pattern(&p.id);
	}
}

fn style_doc(ui: &mut Ui, state: &mut AppState, library: &Library, s: &Style) {
	ui.add_space(10.0);
	ui.horizontal(|ui| {
		if let Some(first) = s.forms.first()
			&& button(ui, tr("Start a song in this style"), true, 26.0)
				.on_hover_text(trf("Applies its first form (markers from bar 1), tempo {typical} bpm and this style as the song's style. One undo step.", &[("typical", &crate::i18n::decimal(format!("{:.0}", s.tempo.typical)))]))
				.clicked()
		{
			state.apply_library_form(first, Some(&s.id));
		}
		if button(ui, tr("Ask producer…"), false, 26.0)
			.on_hover_text(tr("Opens the AI producer with a request about this style to finish"))
			.clicked()
		{
			state.ai.input = format!("Write a song in the {} style ({}): ", s.name, s.id);
			state.view.panels.set(PanelKind::Chat, true);
			state.mix.open = false;
		}
	});

	section_title(ui, tr("Feel"));
	let bpm = state.project().tempo.bpm_at(state.position());
	kv(ui, tr("Tempo"), |ui| {
		ui.horizontal_wrapped(|ui| {
			ui.spacing_mut().item_spacing.x = 6.0;
			ui.label(value_text(trf(
				"{min}–{max} bpm, typical {typical}",
				&[
					("min", &crate::i18n::decimal(format!("{:.0}", s.tempo.min))),
					("max", &crate::i18n::decimal(format!("{:.0}", s.tempo.max))),
					(
						"typical",
						&crate::i18n::decimal(format!("{:.0}", s.tempo.typical)),
					),
				],
			)));
			let fits = (s.tempo.min..=s.tempo.max).contains(&bpm);
			ui.label(
				RichText::new(if fits {
					trf(
						"✓ project {bpm}",
						&[("bpm", &crate::i18n::decimal(format!("{:.0}", bpm)))],
					)
				} else {
					trf(
						"project {bpm}",
						&[("bpm", &crate::i18n::decimal(format!("{:.0}", bpm)))],
					)
				})
				.font(theme::small())
				.color(if fits {
					theme::PLAYING
				} else {
					theme::TEXT_FAINT
				}),
			);
		});
	});
	kv(ui, tr("Groove"), |ui| {
		ui.label(value_text(format!("{} · {}", pretty(&s.feel), s.meter)));
	});
	if !s.keys.is_empty() {
		kv(ui, tr("Keys"), |ui| {
			ui.add(egui::Label::new(value_text(s.keys.join(", "))).wrap());
		});
	}

	section_title(ui, tr("Harmony"));
	for id in &s.harmony.loops {
		match library.item(id).map(|i| &i.item) {
			Some(Item::Progression(p)) => {
				ui.horizontal(|ui| {
					let w = (ui.available_width() - 60.0).max(120.0);
					let names = chord_names(
						&p.numerals,
						if p.mode == "major" {
							"C major"
						} else {
							"A minor"
						},
					);
					numeral_cells(ui, &p.numerals, w, 24.0).on_hover_text(format!(
						"{}\n{}{}",
						p.name,
						p.summary,
						names.map(|n| format!("\n{n}")).unwrap_or_default()
					));
					ui.label(faint(&p.mode));
				});
				ui.add_space(3.0);
			}
			_ => {
				ui.label(faint(id));
			}
		}
	}
	if !s.harmony.notes.is_empty() {
		ui.add_space(2.0);
		ui.add(egui::Label::new(caption(&s.harmony.notes)).wrap());
	}

	section_title(ui, tr("Forms"));
	for id in &s.forms {
		let Some(Item::Form(f)) = library.item(id).map(|i| &i.item) else {
			ui.label(faint(id));
			continue;
		};
		ui.horizontal(|ui| {
			ui.label(value_text(&f.name));
			ui.label(faint(lib::form_length(
				f.total_bars(),
				s.tempo.typical,
				&s.meter,
			)));
			ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
				if button(ui, tr("Apply"), false, 20.0)
					.on_hover_text(trf("Markers for every section from bar 1, tempo {typical} bpm and this style as the song's style. One undo step.", &[("typical", &crate::i18n::decimal(format!("{:.0}", s.tempo.typical)))]))
					.clicked()
				{
					state.apply_library_form(id, Some(&s.id));
				}
			});
		});
		let sections: Vec<(String, u32)> = f
			.sections
			.iter()
			.map(|x| (x.name.clone(), x.bars))
			.collect();
		let (strip, _) = ui.allocate_exact_size(vec2(ui.available_width(), 18.0), Sense::hover());
		paint_sections(ui.painter(), strip, &sections, true);
		ui.add_space(8.0);
	}

	section_title(ui, tr("Roles"));
	for row in lib::role_rows(s) {
		let color = role_color(&row.role);
		kv(ui, &pretty(&row.role), |ui| {
			if !row.patterns.is_empty() {
				ui.horizontal_wrapped(|ui| {
					ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
					for (id, sections) in &row.patterns {
						pattern_chip(ui, state, library, id, sections, color);
					}
				});
			}
			for (section, text) in &row.notes {
				ui.add(
					egui::Label::new(faint(format!("{}: {text}", section.replace('_', " "))))
						.wrap(),
				);
			}
		});
	}

	if !s.sounds.is_empty() {
		section_title(ui, tr("Sounds"));
		for (k, v) in s.sounds.iter() {
			let text = match v {
				cadenza_library::RoleValue::Text(t) => t.clone(),
				cadenza_library::RoleValue::Ids(ids) => ids.join(", "),
			};
			kv(ui, &pretty(k), |ui| {
				ui.add(egui::Label::new(caption(text)).wrap());
			});
		}
	}

	section_title(ui, tr("Mix targets"));
	ui.horizontal_wrapped(|ui| {
		ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
		target_tile(ui, tr("Master"), &format!("{:.0} LUFS", s.mix.lufs));
		target_tile(
			ui,
			tr("True peak"),
			&format!("{:.1} dBTP", s.mix.true_peak.unwrap_or(-1.0)),
		);
		if let Some(p) = &s.mix.profile {
			target_tile(ui, tr("Tone"), &pretty(p));
		}
		for (role, db) in s.mix.levels.iter() {
			target_tile(ui, &pretty(role), &format!("{db:+.0} dB"));
		}
	});
	if !s.mix.notes.is_empty() {
		ui.add_space(4.0);
		ui.add(egui::Label::new(caption(&s.mix.notes)).wrap());
	}

	if !s.dos.is_empty() || !s.dont.is_empty() {
		let lists = [
			(tr("Do"), &s.dos, "✓", theme::PLAYING),
			(tr("Don't"), &s.dont, "×", theme::ERROR),
		];
		let column =
			|ui: &mut Ui, (title, list, mark, color): (&str, &Vec<String>, &str, Color32)| {
				if list.is_empty() {
					return;
				}
				section_title(ui, title);
				for line in list {
					ui.horizontal_top(|ui| {
						ui.label(RichText::new(mark).font(theme::small()).color(color));
						ui.add(egui::Label::new(caption(line)).wrap());
					});
				}
			};
		if ui.available_width() >= 440.0 {
			ui.columns(2, |cols| {
				column(&mut cols[0], lists[0]);
				column(&mut cols[1], lists[1]);
			});
		} else {
			for l in lists {
				column(ui, l);
			}
		}
	}
}

fn target_tile(ui: &mut Ui, key: &str, value: &str) {
	Frame::new()
		.fill(theme::BG_RAISED)
		.stroke(Stroke::new(1.0, theme::BORDER_SUBTLE))
		.corner_radius(theme::RADIUS_MD)
		.inner_margin(Margin::symmetric(9, 6))
		.show(ui, |ui| {
			ui.set_min_width(96.0);
			ui.vertical(|ui| {
				ui.spacing_mut().item_spacing.y = 1.0;
				ui.label(faint(key));
				ui.label(
					RichText::new(value)
						.font(theme::mono_medium(12.5))
						.color(theme::TEXT),
				);
			});
		});
}

fn credits(ui: &mut Ui, view: &mut LibraryView, library: &Library, back: bool) {
	doc_header(ui, view, back, |ui| {
		tag(ui, tr("CREDITS"), theme::ACCENT_TEXT)
	});
	egui::ScrollArea::vertical()
		.id_salt("library-credits")
		.auto_shrink([false, false])
		.show(ui, |ui| {
			ui.set_width(ui.available_width() - 6.0);
			ui.add_space(4.0);
			ui.label(
				RichText::new(tr("Credits"))
					.font(theme::semibold(17.0))
					.color(theme::TEXT),
			);
			ui.add_space(2.0);
			ui.add(
				egui::Label::new(caption(tr(
					"Every installed pack with its license. Attribution applies to redistributing a \
					 pack, not to songs you make with it.",
				)))
				.wrap(),
			);
			for pack in library.packs() {
				let m = &pack.manifest;
				ui.add_space(8.0);
				Frame::new()
					.fill(theme::BG_RAISED)
					.stroke(Stroke::new(1.0, theme::BORDER_SUBTLE))
					.corner_radius(theme::RADIUS_LG)
					.inner_margin(Margin::symmetric(12, 10))
					.show(ui, |ui| {
						ui.set_width(ui.available_width());
						ui.horizontal(|ui| {
							ui.label(
								RichText::new(&m.name)
									.font(theme::semibold(13.0))
									.color(theme::TEXT),
							);
							ui.with_layout(
								egui::Layout::right_to_left(egui::Align::Center),
								|ui| {
									tag(ui, &m.license, theme::PLAYING);
									if !m.redistributable {
										tag(ui, tr("PRIVATE"), theme::WARNING);
									}
								},
							);
						});
						ui.label(faint(trf(
							"{id} · v{version} · {pack} items",
							&[
								("id", &m.id),
								("version", &m.version),
								("pack", &pack.items.len()),
							],
						)));
						if !m.attribution.is_empty() {
							ui.add(
								egui::Label::new(caption(format!("by {}", m.attribution))).wrap(),
							);
						}
						for link in m.homepage.iter().chain(m.source.iter()) {
							ui.add(
								egui::Label::new(
									RichText::new(link)
										.font(theme::small())
										.color(theme::ACCENT_TEXT),
								)
								.wrap(),
							);
						}
						if !m.redistributable {
							ui.label(
								RichText::new(tr("Not redistributable: for your own use only"))
									.font(theme::small())
									.color(theme::WARNING),
							);
						}
						let location = match &pack.origin {
							cadenza_library::PackOrigin::Embedded => tr("built in").to_owned(),
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
				section_title(ui, tr("Problems"));
				for r in problems {
					ui.label(
						RichText::new(format!(
							"{}{}",
							r.pack.as_deref().unwrap_or(&r.location),
							if r.loaded { "" } else { tr(" (not loaded)") }
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
