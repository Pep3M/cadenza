//! The start screen (first run and every launch without a project): a full page instead
//! of a modal. Describe a song to the producer, start from a library style, an empty
//! project or the demo, or reopen a recent project; an unclean shutdown shows a recovery
//! banner on top.

use crate::i18n::{tr, trf, trn};
use std::path::PathBuf;
use std::time::SystemTime;

use cadenza_library::Item;
use egui::{
	Align, Align2, CornerRadius, FontId, Frame, Id, Key, Layout, Margin, Modifiers, Pos2, Rect,
	RichText, Sense, Stroke, StrokeKind, Ui, pos2, vec2,
};

use crate::jobs::FileAction;
use crate::state::AppState;
use crate::state::layout::PanelKind;
use crate::state::safety::{RecoveryReason, age};
use crate::state::start::{RecentInfo, THUMB_ROWS, when};
use crate::theme;

/// Widest the page content gets (centered beyond it).
const PAGE_WIDTH: f32 = 1240.0;
/// Styles listed before "All n styles".
const STYLES_SHOWN: usize = 3;
const RECENT_SHOWN: usize = 8;

pub fn show(ui: &mut Ui, state: &mut AppState) {
	let full = ui.available_rect_before_wrap();
	ui.painter().rect_filled(full, 0.0, theme::CANVAS);
	let recent: Vec<PathBuf> = state.recent.iter().take(RECENT_SHOWN).cloned().collect();
	if state.start.update(&recent) {
		ui.ctx()
			.request_repaint_after(std::time::Duration::from_millis(50));
	}
	egui::ScrollArea::vertical()
		.id_salt("start-page")
		.auto_shrink([false, false])
		.show(ui, |ui| {
			let width = ui.available_width().min(PAGE_WIDTH) - 48.0;
			let side = ((ui.available_width() - width) / 2.0).max(24.0);
			ui.horizontal(|ui| {
				ui.add_space(side);
				ui.vertical(|ui| {
					ui.set_width(width);
					ui.add_space(40.0);
					recovery_banner(ui, state);
					ui.label(
						RichText::new(tr("Start a song"))
							.font(theme::semibold(28.0))
							.color(theme::TEXT),
					);
					ui.label(
						RichText::new(tr(
							"Pick a starting point. Everything can be changed later.",
						))
						.color(theme::TEXT_DIM),
					);
					ui.add_space(24.0);
					cards(ui, state, width);
					ui.add_space(32.0);
					recent_list(ui, state, &recent, width);
					ui.add_space(40.0);
					footer(ui, state);
					ui.add_space(24.0);
				});
			});
		});
}

/// An unclean shutdown or a newer autosave: offered here instead of a modal.
fn recovery_banner(ui: &mut Ui, state: &mut AppState) {
	let Some(r) = state.recovery().cloned() else {
		return;
	};
	let what = r
		.project
		.as_deref()
		.map(cadenza_model::files::project_stem)
		.unwrap_or_else(|| tr("An unsaved project").into());
	let why = match r.reason {
		RecoveryReason::Crashed => tr("wasn't closed cleanly."),
		RecoveryReason::Newer => tr("has newer autosaved changes."),
	};
	let saved = age(r.modified, SystemTime::now());
	Frame::new()
		.fill(theme::WARNING_SOFT)
		.stroke(Stroke::new(1.0, theme::WARNING.gamma_multiply(0.6)))
		.corner_radius(theme::RADIUS_LG)
		.inner_margin(Margin::symmetric(14, 10))
		.show(ui, |ui| {
			ui.set_width(ui.available_width());
			ui.horizontal(|ui| {
				ui.label(RichText::new("⚠").color(theme::WARNING));
				ui.label(
					RichText::new(&what)
						.font(theme::semibold(12.0))
						.color(theme::TEXT),
				);
				ui.label(
					RichText::new(trf(
						"{why} The last autosave is from {saved}.",
						&[("why", &why), ("saved", &saved)],
					))
					.color(theme::TEXT),
				);
				ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
					let discard = if r.project.is_some() {
						tr("Open saved version")
					} else {
						tr("Discard")
					};
					if ui.button(discard).clicked() {
						state.dismiss_recovery();
					}
					let recover = egui::Button::new(
						RichText::new(tr("Recover autosave"))
							.font(theme::medium(12.0))
							.color(theme::TEXT_ON_COLOR),
					)
					.fill(theme::WARNING);
					if ui.add(recover).clicked() && state.recover() {
						state.view.start_screen = false;
					}
				});
			});
		});
	ui.add_space(20.0);
}

fn card_frame(accent: bool) -> Frame {
	Frame::new()
		.fill(if accent {
			theme::mix(theme::BG_PANEL, theme::ACCENT, 0.06)
		} else {
			theme::BG_PANEL
		})
		.stroke(Stroke::new(
			1.0,
			if accent {
				theme::ACCENT_BORDER
			} else {
				theme::BORDER
			},
		))
		.corner_radius(theme::RADIUS_LG)
		.inner_margin(Margin::same(16))
}

/// Producer prompt, style list, and the empty / demo cards, in three columns.
fn cards(ui: &mut Ui, state: &mut AppState, width: f32) {
	let gap = 12.0;
	let first = (width - 2.0 * gap) * 0.38;
	let rest = (width - 2.0 * gap - first) / 2.0;
	let height = 214.0;
	ui.horizontal_top(|ui| {
		ui.spacing_mut().item_spacing.x = gap;
		let down = Layout::top_down(Align::Min);
		ui.allocate_ui_with_layout(vec2(first, height), down, |ui| {
			producer_card(ui, state, first, height)
		});
		ui.allocate_ui_with_layout(vec2(rest, height), down, |ui| {
			style_card(ui, state, rest, height)
		});
		ui.allocate_ui_with_layout(vec2(rest, height), down, |ui| {
			ui.spacing_mut().item_spacing.y = gap;
			let h = (height - gap) / 2.0;
			if action_card(
				ui,
				tr("Empty project"),
				"120 bpm · 4/4",
				"Ctrl N",
				vec2(rest, h),
			)
			.clicked()
			{
				state.new_project();
			}
			if action_card(
				ui,
				tr("Demo song"),
				tr("First Light · a finished song to explore"),
				"",
				vec2(rest, h),
			)
			.clicked()
			{
				state.open_demo();
			}
		});
	});
}

/// The producer's avatar ("C" on accent).
pub fn avatar(ui: &mut Ui) {
	let (r, _) = ui.allocate_exact_size(vec2(22.0, 22.0), Sense::hover());
	ui.painter().circle_filled(r.center(), 11.0, theme::ACCENT);
	ui.painter().text(
		r.center(),
		Align2::CENTER_CENTER,
		"C",
		theme::semibold(11.5),
		theme::TEXT_ON_ACCENT,
	);
}

fn producer_card(ui: &mut Ui, state: &mut AppState, width: f32, height: f32) {
	card_frame(true).show(ui, |ui| {
		ui.set_width(width - 32.0);
		ui.set_height(height - 32.0);
		ui.horizontal(|ui| {
			avatar(ui);
			ui.label(
				RichText::new(tr("Describe it to the producer"))
					.font(theme::semibold(13.5))
					.color(theme::TEXT),
			);
		});
		ui.add_space(6.0);
		let id = Id::new("start-prompt");
		let busy = state.ai.busy();
		let focused = ui.memory(|m| m.has_focus(id));
		let enter = focused && ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter));
		ui.add(
			egui::TextEdit::multiline(&mut state.start.prompt)
				.id(id)
				.desired_rows(6)
				.desired_width(f32::INFINITY)
				.hint_text(tr(
					"A chill lo-fi beat at 80 bpm with Rhodes, upright bass and vinyl crackle",
				)),
		);
		ui.add_space(6.0);
		ui.horizontal(|ui| {
			ui.label(
				RichText::new(trf(
					"{current_model} · up to ${max_cost_usd}",
					&[
						("current_model", &state.ai.settings.current_model()),
						(
							"max_cost_usd",
							&crate::i18n::decimal(format!("{:.2}", state.ai.settings.max_cost_usd)),
						),
					],
				))
				.font(theme::mono(10.5))
				.color(theme::TEXT_FAINT),
			);
			ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
				let ready = !busy && !state.start.prompt.trim().is_empty();
				let create =
					egui::Button::new(RichText::new(tr("Create")).font(theme::medium(12.0)).color(
						if ready {
							theme::TEXT_ON_ACCENT
						} else {
							theme::TEXT_FAINT
						},
					))
					.fill(if ready {
						theme::ACCENT
					} else {
						theme::BG_WIDGET
					});
				let clicked = ui
					.add_enabled(ready, create)
					.on_hover_text(tr(
						"A new project, then the producer builds it (one undo step)",
					))
					.clicked();
				if ready && (clicked || enter) {
					let request = std::mem::take(&mut state.start.prompt);
					state.new_project();
					state.view.panels.set(PanelKind::Chat, true);
					state.mix.open = false;
					state.send_ai(request.trim());
				}
			});
		});
	});
}

fn style_card(ui: &mut Ui, state: &mut AppState, width: f32, height: f32) {
	let library = state.library();
	let mut styles: Vec<_> = library
		.items()
		.filter_map(|i| match &i.item {
			Item::Style(s) => Some(s),
			_ => None,
		})
		.collect();
	styles.sort_by(|a, b| a.name.cmp(&b.name));
	card_frame(false).show(ui, |ui| {
		ui.set_width(width - 32.0);
		ui.set_height(height - 32.0);
		ui.label(
			RichText::new(tr("From a style"))
				.font(theme::semibold(13.5))
				.color(theme::TEXT),
		);
		ui.label(
			RichText::new(tr(
				"Tempo, song form and the style guide the producer follows.",
			))
			.font(theme::small())
			.color(theme::TEXT_DIM),
		);
		ui.add_space(6.0);
		let shown = if state.start.all_styles {
			styles.len()
		} else {
			STYLES_SHOWN
		};
		let mut chosen = None;
		egui::ScrollArea::vertical()
			.id_salt("start-styles")
			.max_height(height - 32.0 - 72.0)
			.show(ui, |ui| {
				ui.spacing_mut().item_spacing.y = 3.0;
				for (i, s) in styles.iter().take(shown).enumerate() {
					let color = theme::track_color(Some(theme::TRACK_COLORS[i % 16]));
					let (rect, resp) =
						ui.allocate_exact_size(vec2(ui.available_width(), 26.0), Sense::click());
					resp.widget_info(|| {
						egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &s.name)
					});
					let p = ui.painter();
					p.rect_filled(
						rect,
						CornerRadius::same(theme::RADIUS),
						if resp.hovered() {
							theme::BG_HOVER
						} else {
							theme::BG_RAISED
						},
					);
					p.rect_filled(
						Rect::from_center_size(
							pos2(rect.left() + 12.0, rect.center().y),
							vec2(7.0, 7.0),
						),
						CornerRadius::same(2),
						color,
					);
					p.text(
						pos2(rect.left() + 24.0, rect.center().y),
						Align2::LEFT_CENTER,
						&s.name,
						FontId::proportional(12.0),
						theme::TEXT,
					);
					p.text(
						pos2(rect.right() - 8.0, rect.center().y),
						Align2::RIGHT_CENTER,
						format!("{:.0}–{:.0}", s.tempo.min, s.tempo.max),
						theme::mono(10.5),
						theme::TEXT_FAINT,
					);
					if resp.on_hover_text(&s.summary).clicked() {
						chosen = Some((s.id.clone(), s.forms.first().cloned()));
					}
				}
			});
		if styles.len() > STYLES_SHOWN {
			let label = if state.start.all_styles {
				tr("Fewer styles").to_owned()
			} else {
				trf("All {styles} styles →", &[("styles", &styles.len())])
			};
			if ui
				.add(
					egui::Label::new(
						RichText::new(label)
							.font(theme::small())
							.color(theme::ACCENT_TEXT),
					)
					.sense(Sense::click()),
				)
				.clicked()
			{
				state.start.all_styles = !state.start.all_styles;
			}
		}
		if let Some((style, form)) = chosen {
			state.new_project();
			match form {
				Some(form) => {
					state.apply_library_form(&form, Some(&style));
				}
				None => state
					.notices
					.warn(trf("{style} lists no song form", &[("style", &style)])),
			}
		}
	});
}

/// A whole-card button: title, one line under it and a shortcut hint.
fn action_card(
	ui: &mut Ui,
	title: &str,
	detail: &str,
	key: &str,
	size: egui::Vec2,
) -> egui::Response {
	let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
	resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, title));
	let p = ui.painter();
	let r = CornerRadius::same(theme::RADIUS_LG);
	p.rect_filled(
		rect,
		r,
		if resp.hovered() {
			theme::BG_RAISED
		} else {
			theme::BG_PANEL
		},
	);
	p.rect_stroke(
		rect,
		r,
		Stroke::new(
			1.0,
			if resp.hovered() {
				theme::BORDER_STRONG
			} else {
				theme::BORDER
			},
		),
		StrokeKind::Inside,
	);
	let left = rect.left() + 16.0;
	p.text(
		pos2(left, rect.top() + 26.0),
		Align2::LEFT_CENTER,
		title,
		theme::semibold(13.5),
		theme::TEXT,
	);
	p.text(
		pos2(left, rect.top() + 50.0),
		Align2::LEFT_CENTER,
		detail,
		FontId::proportional(12.0),
		theme::TEXT_DIM,
	);
	if !key.is_empty() {
		p.text(
			pos2(rect.right() - 16.0, rect.top() + 26.0),
			Align2::RIGHT_CENTER,
			key,
			theme::mono(10.5),
			theme::TEXT_FAINT,
		);
	}
	resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn recent_list(ui: &mut Ui, state: &mut AppState, recent: &[PathBuf], width: f32) {
	ui.horizontal(|ui| {
		ui.label(
			RichText::new(tr("Recent"))
				.font(theme::semibold(15.0))
				.color(theme::TEXT),
		);
		ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
			let open = egui::Button::new(RichText::new(tr("Open…")).font(theme::medium(12.0)))
				.right_text(
					RichText::new("Ctrl O")
						.font(theme::mono(10.5))
						.color(theme::TEXT_FAINT),
				);
			if ui.add(open).clicked() {
				state.view.start_screen = false;
				state.file_dialog(FileAction::Open);
			}
		});
	});
	ui.add_space(6.0);
	if recent.is_empty() {
		ui.label(
			RichText::new(tr("Projects you open or save show up here."))
				.font(theme::small())
				.color(theme::TEXT_FAINT),
		);
		return;
	}
	let now = SystemTime::now();
	let mut open = None;
	ui.spacing_mut().item_spacing.y = 0.0;
	for path in recent {
		let exists = path.exists();
		let name = cadenza_model::files::project_stem(path);
		let folder = path
			.parent()
			.map(crate::state::files::display_path)
			.unwrap_or_default();
		let (rect, resp) = ui.allocate_exact_size(
			vec2(width, 54.0),
			if exists {
				Sense::click()
			} else {
				Sense::hover()
			},
		);
		resp.widget_info(|| {
			egui::WidgetInfo::labeled(
				egui::WidgetType::Button,
				exists,
				format!("{name} — {folder}"),
			)
		});
		let p = ui.painter();
		if resp.hovered() && exists {
			p.rect_filled(rect, 0.0, theme::BG_RAISED);
			p.vline(
				rect.left() + 1.0,
				rect.y_range(),
				Stroke::new(2.0, theme::ACCENT),
			);
		}
		p.hline(
			rect.x_range(),
			rect.bottom() - 0.5,
			Stroke::new(1.0, theme::BORDER_SUBTLE),
		);
		let info = state.start.info(path).and_then(|r| r.as_ref().ok());
		let thumb = Rect::from_min_size(
			pos2(rect.left() + 16.0, rect.top() + 9.0),
			vec2(200.0, 36.0),
		);
		paint_thumbnail(p, thumb, info);
		let text_left = thumb.right() + 16.0;
		let dim = if exists { 1.0 } else { 0.5 };
		p.text(
			pos2(text_left, rect.top() + 18.0),
			Align2::LEFT_CENTER,
			&name,
			theme::semibold(12.5),
			theme::TEXT.gamma_multiply(dim),
		);
		p.text(
			pos2(text_left, rect.top() + 36.0),
			Align2::LEFT_CENTER,
			if exists {
				folder.clone()
			} else {
				trf("{folder} · missing", &[("folder", &folder)])
			},
			theme::small(),
			if exists {
				theme::TEXT_FAINT
			} else {
				theme::WARNING
			},
		);
		let col = |f: f32| rect.left() + width * f;
		if let Some(info) = info {
			let tempo = match &info.style {
				Some(s) => format!("{:.0} · {} · {}", info.bpm, info.signature, short_style(s)),
				None => format!("{:.0} · {}", info.bpm, info.signature),
			};
			p.text(
				pos2(col(0.6), rect.center().y),
				Align2::LEFT_CENTER,
				tempo,
				theme::mono(11.0),
				theme::TEXT_DIM,
			);
			p.text(
				pos2(col(0.78), rect.center().y),
				Align2::LEFT_CENTER,
				trn("{n} track", "{n} tracks", info.tracks),
				theme::small(),
				theme::TEXT_DIM,
			);
			if let Some(m) = info.modified {
				p.text(
					pos2(rect.right() - 16.0, rect.center().y),
					Align2::RIGHT_CENTER,
					when(m, now),
					theme::small(),
					theme::TEXT_FAINT,
				);
			}
		}
		if resp.on_hover_text(path.display().to_string()).clicked() {
			open = Some(path.clone());
		}
	}
	if let Some(path) = open {
		state.open(&path);
	}
}

/// "core/lofi_hip_hop" -> "lofi hip hop".
fn short_style(id: &str) -> String {
	id.rsplit('/').next().unwrap_or(id).replace('_', " ")
}

/// The clips of a project as thin bars, one row per track (a dark well while loading).
fn paint_thumbnail(p: &egui::Painter, rect: Rect, info: Option<&RecentInfo>) {
	p.rect_filled(rect, CornerRadius::same(theme::RADIUS), theme::BG_DEEP);
	let Some(info) = info.filter(|i| i.rows > 0) else {
		return;
	};
	let inner = rect.shrink2(vec2(4.0, 4.0));
	let rows = info.rows.clamp(1, THUMB_ROWS) as f32;
	let row_h = inner.height() / rows;
	let bar = (row_h - 1.0).clamp(1.0, 3.0);
	for c in &info.clips {
		let y = inner.top() + c.row as f32 * row_h + (row_h - bar) / 2.0;
		let x0 = inner.left() + c.start * inner.width();
		let x1 = (inner.left() + c.end * inner.width()).max(x0 + 1.0);
		p.rect_filled(
			Rect::from_min_max(Pos2::new(x0, y), Pos2::new(x1 - 1.0, y + bar)),
			0.0,
			theme::track_color(c.color.as_deref()),
		);
	}
}

fn footer(ui: &mut Ui, state: &mut AppState) {
	let plugins = state.catalog().descriptors().count();
	ui.horizontal(|ui| {
		ui.spacing_mut().item_spacing.x = 18.0;
		let link = |ui: &mut Ui, text: &str| {
			ui.add(
				egui::Label::new(
					RichText::new(text)
						.font(theme::small())
						.color(theme::TEXT_FAINT),
				)
				.sense(Sense::click()),
			)
		};
		if link(ui, tr("F1 Keyboard shortcuts")).clicked() {
			state.view.help = true;
		}
		let scanning = state.jobs.scan.is_some();
		let plugins_text = if scanning {
			tr("Plugins: scanning…").to_owned()
		} else {
			trf(
				"Plugins: {plugins} CLAP / VST3 found · Rescan",
				&[("plugins", &plugins)],
			)
		};
		if link(ui, &plugins_text)
			.on_hover_text(tr("Scan the system plugin folders for CLAP/VST3 plugins"))
			.clicked()
			&& !scanning
		{
			state.rescan_plugins();
		}
		ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
			ui.label(
				RichText::new(format!("v{}", env!("CARGO_PKG_VERSION")))
					.font(theme::mono(10.5))
					.color(theme::TEXT_FAINT),
			);
		});
	});
}
