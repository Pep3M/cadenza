//! The AI panel's Mix tab (design 06): a range / target / Analyze bar, the report as a
//! story (Master loudness against the target, Issues with Fix and Fix all, then the
//! evidence: Tone, Channels, Masking; each section folds), a pinned AI mix pass footer
//! (reference, brief, cost) and the pass review (before -> after against the targets, Keep /
//! Revert). Logic lives in [`crate::state::mix`].

use crate::i18n::{tr, trf, trn};
use cadenza_analysis::issues::{Issue, IssueKind, Severity};
use cadenza_analysis::spectrum::OCTAVE_HZ;
use cadenza_core::TrackId;
use cadenza_mcp::tools::mix::MixAnalysis;
use egui::{
	Align, Align2, Color32, CornerRadius, FontId, Frame, Layout, Margin, Mesh, Rect, RichText,
	ScrollArea, Sense, Shape, Stroke, StrokeKind, Ui, pos2, vec2,
};

use crate::jobs::FileAction;
use crate::state::AppState;
use crate::state::ai::ChatItem;
use crate::state::mix::{self, MixSection, PassPhase, Purpose, TARGET_PRESETS, delta, signed};
use crate::theme;

/// The loudness bar of the Master card spans this range (LUFS).
const MASTER_RANGE: (f32, f32) = (-30.0, -6.0);
/// Channel loudness bars span this range (LUFS).
const CHANNEL_RANGE: (f32, f32) = (-42.0, -6.0);
/// The tone chart's mix line.
const TONE: Color32 = theme::LOOP;

pub fn show(ui: &mut Ui, state: &mut AppState) {
	// Sections fold by default when the panel is short (see `MixSection::COMPACT_HEIGHT`).
	let height = ui.available_height();
	egui::Panel::bottom("mix-pass-footer")
		.frame(Frame::new().inner_margin(Margin {
			left: 0,
			right: 0,
			top: 8,
			bottom: 4,
		}))
		.show_separator_line(true)
		.show(ui, |ui| footer(ui, state));
	top_bar(ui, state);
	if let Some(e) = &state.mix.error {
		ui.label(RichText::new(e).font(theme::small()).color(theme::ERROR));
	}
	// A finished pass is reviewed on its own page (design 06b); Keep / Revert returns to the
	// report.
	let reviewing = state
		.mix
		.pass
		.as_ref()
		.is_some_and(|p| p.phase == PassPhase::Review);
	ScrollArea::vertical()
		.id_salt(if reviewing {
			"mix-pass-review"
		} else {
			"mix-assistant"
		})
		.auto_shrink([false, false])
		.show(ui, |ui| {
			// Room for the scroll bar, which floats over the content.
			ui.set_width(ui.available_width() - 10.0);
			if reviewing {
				review(ui, state);
				return;
			}
			match state.mix.report.clone() {
				Some(report) => {
					let dim = state.mix.job.is_some();
					ui.scope(|ui| {
						if dim {
							ui.multiply_opacity(0.45);
						}
						report_ui(ui, state, &report, height);
					});
				}
				None if state.mix.job.is_none() => empty(ui, state),
				None => {
					ui.add_space(28.0);
					ui.vertical_centered(|ui| {
						ui.spinner();
						ui.add_space(6.0);
						ui.label(faint(tr(
							"Rendering every channel offline; the report appears here.",
						)));
					});
				}
			}
		});
}

// -- small pieces -------------------------------------------------------------------------

fn faint(text: impl Into<String>) -> RichText {
	RichText::new(text)
		.font(theme::small())
		.color(theme::TEXT_FAINT)
}

fn track_color(state: &AppState, track: Option<TrackId>) -> Color32 {
	match track {
		None => theme::ACCENT,
		Some(id) => state.project().track(id).map_or(theme::DEFAULT_TRACK, |t| {
			theme::track_color(t.color.as_deref())
		}),
	}
}

/// The accent-filled primary button of the design.
fn primary(text: &str, font: FontId) -> egui::Button<'_> {
	egui::Button::new(RichText::new(text).font(font).color(theme::TEXT_ON_ACCENT))
		.fill(theme::ACCENT)
		.stroke(Stroke::NONE)
		.corner_radius(CornerRadius::same(theme::RADIUS_MD))
}

fn quiet(text: &str) -> egui::Button<'_> {
	egui::Button::new(
		RichText::new(text)
			.font(theme::medium(12.0))
			.color(theme::TEXT),
	)
	.fill(theme::BG_WIDGET)
	.stroke(Stroke::NONE)
	.corner_radius(CornerRadius::same(theme::RADIUS_MD))
}

/// A section of the report: a divider, then a foldable title row with (right) a summary
/// that keeps the gist visible while folded. Returns whether the section is open and the
/// title row's rect.
fn section(
	ui: &mut Ui,
	state: &mut AppState,
	which: MixSection,
	title: &str,
	count: Option<usize>,
	summary: (&str, Color32),
	height: f32,
) -> (bool, Rect) {
	ui.add_space(6.0);
	let w = ui.available_width();
	let (line, _) = ui.allocate_exact_size(vec2(w, 1.0), Sense::hover());
	ui.painter().hline(
		line.x_range(),
		line.center().y,
		Stroke::new(1.0, theme::BORDER_SUBTLE),
	);
	ui.add_space(6.0);
	let open = !state.mix.is_folded(which, height);
	let (rect, resp) = ui.allocate_exact_size(vec2(w, 22.0), Sense::click());
	resp.widget_info(|| {
		egui::WidgetInfo::selected(egui::WidgetType::CollapsingHeader, true, open, title)
	});
	let p = ui.painter_at(rect);
	let color = if resp.hovered() {
		theme::TEXT
	} else {
		theme::TEXT_DIM
	};
	super::widgets::paint_disclosure(&p, pos2(rect.left() + 4.0, rect.center().y), open, color);
	let title_rect = p.text(
		pos2(rect.left() + 14.0, rect.center().y),
		Align2::LEFT_CENTER,
		title,
		theme::heading(),
		theme::TEXT,
	);
	if let Some(n) = count {
		p.text(
			pos2(title_rect.right() + 6.0, rect.center().y),
			Align2::LEFT_CENTER,
			n.to_string(),
			theme::small(),
			theme::TEXT_FAINT,
		);
	}
	if !summary.0.is_empty() {
		p.text(
			pos2(rect.right() - 2.0, rect.center().y),
			Align2::RIGHT_CENTER,
			summary.0,
			theme::small(),
			summary.1,
		);
	}
	if resp
		.on_hover_text(if open { tr("Fold") } else { tr("Unfold") })
		.clicked()
	{
		state.mix.toggle_section(which, height);
	}
	ui.add_space(4.0);
	(open, rect)
}

/// A small pill: optional colour dot + text.
fn chip(
	ui: &mut Ui,
	dot: Option<Color32>,
	text: &str,
	font: FontId,
	sense: Sense,
) -> egui::Response {
	let galley = ui
		.painter()
		.layout_no_wrap(text.to_owned(), font, theme::TEXT_DIM);
	let pad = if dot.is_some() { 18.0 } else { 7.0 };
	let size = vec2(galley.size().x + pad + 7.0, 18.0);
	let (rect, resp) = ui.allocate_exact_size(size, sense);
	let p = ui.painter();
	let fill = if resp.hovered() && sense.senses_click() {
		theme::BG_HOVER
	} else {
		theme::BG_WIDGET
	};
	p.rect_filled(rect, CornerRadius::same(theme::RADIUS), fill);
	if let Some(c) = dot {
		p.rect_filled(
			Rect::from_center_size(pos2(rect.left() + 9.0, rect.center().y), vec2(7.0, 7.0)),
			CornerRadius::same(2),
			c,
		);
	}
	let text_color = if resp.hovered() && sense.senses_click() {
		theme::TEXT
	} else {
		theme::TEXT_DIM
	};
	p.galley_with_override_text_color(
		pos2(rect.left() + pad, rect.center().y - galley.size().y * 0.5),
		galley,
		text_color,
	);
	resp
}

fn ago(secs: u64) -> String {
	match secs {
		0..=4 => tr("just now").into(),
		5..=59 => trf("{n} s ago", &[("n", &secs)]),
		60..=3599 => trf("{n} min ago", &[("n", &(secs / 60))]),
		_ => trf("{n} h ago", &[("n", &(secs / 3600))]),
	}
}

fn clock(seconds: f64) -> String {
	let s = seconds.max(0.0).round() as u64;
	format!("{}:{:02}", s / 60, s % 60)
}

// -- top bar ------------------------------------------------------------------------------

fn top_bar(ui: &mut Ui, state: &mut AppState) {
	if let Some((purpose, progress)) = state.mix.analyzing() {
		progress_bar(ui, state, purpose, progress);
		return;
	}
	let busy = state.mix.busy() || state.ai.busy();
	ui.horizontal(|ui| {
		ui.set_min_height(26.0);
		let text = match (&state.mix.report, state.mix.analyzed_at) {
			(Some(r), at) => {
				let mut s = trf(
					"Whole song · {clock}",
					&[("clock", &clock(r.report.duration_seconds))],
				);
				if let Some(at) = at {
					s.push_str(tr(" · analyzed "));
					s.push_str(&ago(at.elapsed().as_secs()));
					ui.ctx()
						.request_repaint_after(std::time::Duration::from_secs(5));
				}
				s
			}
			(None, _) => tr("Not analyzed yet").into(),
		};
		ui.add(
			egui::Label::new(
				RichText::new(text)
					.font(theme::small())
					.color(theme::TEXT_DIM),
			)
			.truncate()
			.selectable(false),
		);
		ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
			if state.mix.report.is_some()
				&& ui
					.add_enabled(!busy, quiet(tr("Analyze")))
					.on_hover_text(tr("Render the mix offline and measure it again"))
					.clicked()
			{
				state.mix_analyze();
			}
			target_menu(ui, state);
		});
	});
}

/// "Target  Streaming −14 ▾": the loudness target (style, presets or a custom value).
fn target_menu(ui: &mut Ui, state: &mut AppState) {
	let style_lufs = state.mix.report.as_ref().map_or(-14.0, |r| {
		if state.mix.target_lufs.is_some() {
			-14.0
		} else {
			r.targets.lufs
		}
	});
	let label = mix::target_label(state.mix.target_lufs, style_lufs);
	let mut job = egui::text::LayoutJob::default();
	job.append(
		tr("Target  "),
		0.0,
		egui::TextFormat::simple(theme::small(), theme::TEXT_FAINT),
	);
	job.append(
		&format!("{label} ▾"),
		0.0,
		egui::TextFormat::simple(theme::medium(11.5), theme::TEXT),
	);
	let button = egui::Button::new(job)
		.fill(theme::BG_WIDGET)
		.stroke(Stroke::NONE)
		.corner_radius(CornerRadius::same(theme::RADIUS_MD));
	let resp = ui
		.add(button)
		.on_hover_text(tr("Loudness the master should reach (integrated LUFS)"));
	egui::Popup::menu(&resp).show(|ui| {
		ui.set_min_width(190.0);
		let current = state.mix.target_lufs;
		if ui
			.selectable_label(current.is_none(), tr("Song style"))
			.on_hover_text(tr(
				"The loudness the song's style asks for (default −14 LUFS)",
			))
			.clicked()
		{
			state.mix.target_lufs = None;
			ui.close();
		}
		for (name, lufs) in TARGET_PRESETS {
			let on = current.is_some_and(|v| (v - lufs).abs() < 0.05);
			if ui
				.selectable_label(
					on,
					format!("{}  {} LUFS", crate::i18n::tr_str(name), signed(lufs, 0)),
				)
				.clicked()
			{
				state.mix.target_lufs = Some(lufs);
				ui.close();
			}
		}
		ui.separator();
		ui.horizontal(|ui| {
			ui.label(faint(tr("Custom")));
			let mut v = current.unwrap_or(style_lufs);
			if ui
				.add(
					egui::DragValue::new(&mut v)
						.range(-30.0..=-5.0)
						.speed(0.1)
						.fixed_decimals(1)
						.suffix(tr(" LUFS")),
				)
				.changed()
			{
				state.mix.target_lufs = Some(v);
			}
		});
	});
}

fn progress_bar(ui: &mut Ui, state: &mut AppState, purpose: Purpose, progress: f32) {
	ui.horizontal(|ui| {
		ui.set_min_height(26.0);
		let cancel_w = 60.0;
		let w = (ui.available_width() - cancel_w - 6.0).max(60.0);
		let (rect, _) = ui.allocate_exact_size(vec2(w, 24.0), Sense::hover());
		let p = ui.painter_at(rect);
		p.rect_filled(rect, CornerRadius::same(theme::RADIUS_MD), theme::BG_DEEP);
		let fill = Rect::from_min_size(
			rect.min,
			vec2(rect.width() * progress.clamp(0.0, 1.0), rect.height()),
		);
		p.rect_filled(
			fill,
			CornerRadius::same(theme::RADIUS_MD),
			theme::ACCENT_DIM,
		);
		p.rect_stroke(
			rect,
			CornerRadius::same(theme::RADIUS_MD),
			Stroke::new(1.0, theme::ACCENT_BORDER),
			StrokeKind::Inside,
		);
		let what = match purpose {
			Purpose::Report => tr("Rendering the mix offline"),
			Purpose::BeforePass => tr("Measuring before the pass"),
			Purpose::AfterPass => tr("Measuring the result"),
		};
		p.text(
			pos2(rect.left() + 10.0, rect.center().y),
			Align2::LEFT_CENTER,
			format!("{what}…"),
			theme::small(),
			theme::TEXT,
		);
		p.text(
			pos2(rect.right() - 10.0, rect.center().y),
			Align2::RIGHT_CENTER,
			format!("{:.0} %", progress * 100.0),
			theme::mono(11.0),
			theme::ACCENT_TEXT,
		);
		if ui
			.add_sized(vec2(cancel_w, 24.0), quiet(tr("Cancel")))
			.clicked()
		{
			state.mix_cancel();
		}
	});
	ui.ctx()
		.request_repaint_after(std::time::Duration::from_millis(100));
}

fn empty(ui: &mut Ui, state: &mut AppState) {
	ui.add_space(28.0);
	ui.vertical_centered(|ui| {
		ui.label(
			RichText::new(tr("Measure the mix"))
				.font(theme::semibold(15.0))
				.color(theme::TEXT),
		);
		ui.add_space(6.0);
		ui.label(
			RichText::new(tr(
				"Analyze renders the song offline and checks loudness against the target, \
				 peaks, tone, stereo image, how every part sits and which parts mask each \
				 other. Each issue comes with a one-click fix.",
			))
			.font(theme::small())
			.color(theme::TEXT_DIM),
		);
		ui.add_space(14.0);
		let busy = state.mix.busy() || state.ai.busy();
		let w = ui.available_width().min(260.0);
		if ui
			.add_enabled(
				!busy,
				primary(tr("Analyze"), theme::medium(13.0)).min_size(vec2(w, 34.0)),
			)
			.on_hover_text(tr(
				"Render the mix offline and measure it against the song's targets",
			))
			.clicked()
		{
			state.mix_analyze();
		}
	});
}

// -- AI mix pass footer -------------------------------------------------------------------

fn footer(ui: &mut Ui, state: &mut AppState) {
	let running = state
		.mix
		.pass
		.as_ref()
		.is_some_and(|p| p.phase == PassPhase::Running);
	ui.horizontal(|ui| {
		ui.label(
			RichText::new(tr("AI sound engineer"))
				.font(theme::semibold(12.5))
				.color(theme::TEXT),
		);
		ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
			ui.label(faint(trf(
				"≤ ${max_cost_usd} · session ${total_usd}",
				&[
					(
						"max_cost_usd",
						&crate::i18n::decimal(format!("{:.2}", state.ai.settings.max_cost_usd)),
					),
					(
						"total_usd",
						&crate::i18n::decimal(format!("{:.3}", state.ai.total_usd)),
					),
				],
			)))
			.on_hover_text(tr(
				"Budget of one request and what this conversation has cost so far",
			));
		});
	});
	if running {
		let secs = state
			.ai
			.running
			.as_ref()
			.map_or(0.0, |r| r.started.elapsed().as_secs_f32());
		Frame::new()
			.fill(theme::ACCENT_DIM)
			.stroke(Stroke::new(1.0, theme::ACCENT_BORDER))
			.corner_radius(CornerRadius::same(theme::RADIUS_MD))
			.inner_margin(Margin::symmetric(8, 6))
			.show(ui, |ui| {
				ui.set_width(ui.available_width());
				ui.horizontal(|ui| {
					ui.spinner();
					ui.label(
						RichText::new(trf(
							"The engineer is mixing… {secs} s",
							&[("secs", &crate::i18n::decimal(format!("{:.0}", secs)))],
						))
						.font(theme::small())
						.color(theme::TEXT),
					);
					ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
						let stop = egui::Button::new(
							RichText::new(tr("■ Stop"))
								.font(theme::medium(12.0))
								.color(theme::ERROR),
						)
						.fill(theme::ERROR_SOFT)
						.stroke(Stroke::NONE);
						if ui
							.add(stop)
							.on_hover_text(tr("Stop at the next step; see the chat (Producer tab)"))
							.clicked()
						{
							state.ai.stop();
						}
					});
				});
			});
		ui.ctx()
			.request_repaint_after(std::time::Duration::from_millis(250));
		return;
	}
	let busy = state.mix.busy() || state.ai.busy();
	ui.horizontal(|ui| {
		ui.label(faint(tr("Reference")));
		if let Some(path) = state.mix.reference.clone() {
			let name = path
				.file_name()
				.map_or_else(String::new, |f| f.to_string_lossy().into_owned());
			chip(
				ui,
				Some(theme::PATTERN),
				&name,
				theme::small(),
				Sense::hover(),
			)
			.on_hover_text(path.display().to_string());
			if ui
				.small_button("×")
				.on_hover_text(tr("Forget the reference"))
				.clicked()
			{
				state.mix.reference = None;
			}
		} else if ui
			.add(quiet(tr("Reference…")).small())
			.on_hover_text(tr(
				"A finished track the AI mix pass steers tone and loudness toward",
			))
			.clicked()
		{
			state.file_dialog(FileAction::MixReference);
		}
	});
	ui.add(
		egui::TextEdit::singleline(&mut state.mix.notes)
			.hint_text(tr("Brief (optional): punchier drums, warmer vocal…"))
			.desired_width(f32::INFINITY)
			.margin(Margin::symmetric(8, 5)),
	);
	ui.add_space(2.0);
	let w = ui.available_width();
	if ui
		.add_enabled_ui(!busy, |ui| {
			ui.add_sized(
				vec2(w, 28.0),
				primary(tr("AI mix pass"), theme::medium(12.5)),
			)
		})
		.inner
		.on_hover_text(tr(
			"The AI sound engineer mixes the song (gain staging, EQ, dynamics, space, pan, \
			 loudness) as one undo step, then shows what changed",
		))
		.clicked()
	{
		state.mix_ai_pass();
	}
}

// -- review -------------------------------------------------------------------------------

/// The engineer's last reply (the pass's summary).
fn last_reply(state: &AppState) -> Option<String> {
	state.ai.items.iter().rev().find_map(|i| match i {
		ChatItem::Assistant { text, .. } if !text.trim().is_empty() => Some(text.trim().to_owned()),
		_ => None,
	})
}

/// Before -> after of an AI mix pass against the targets, with Keep / Revert.
fn review(ui: &mut Ui, state: &mut AppState) {
	let Some(pass) = &state.mix.pass else { return };
	if pass.phase != PassPhase::Review {
		return;
	}
	let (edited, error) = (pass.edited, pass.error.clone());
	let (lufs, ceiling) = pass
		.after
		.as_ref()
		.or(pass.before.as_ref())
		.map_or((-14.0, -1.0), |r| (r.targets.lufs, r.targets.true_peak_db));
	let rows = mix::review_rows(&pass.changes, lufs, ceiling);
	let moved = pass
		.changes
		.iter()
		.filter(|c| c.delta().abs() >= 0.05)
		.count();
	let reply = last_reply(state);
	let channel_color = |name: &str| {
		pass.after
			.as_ref()
			.and_then(|r| r.channels.iter().find(|c| c.name == name))
			.map(|c| c.track_id)
	};
	let colors: Vec<Option<Color32>> = rows
		.iter()
		.map(|r| {
			r.channel
				.then(|| channel_color(&r.label).map(|t| track_color(state, Some(t))))
				.flatten()
		})
		.collect();
	let mut keep = false;
	let mut revert = false;
	Frame::new()
		.fill(theme::mix(theme::BG_PANEL, theme::ACCENT_DIM, 0.8))
		.stroke(Stroke::new(1.0, theme::ACCENT_BORDER))
		.corner_radius(CornerRadius::same(theme::RADIUS_LG))
		.inner_margin(Margin::symmetric(12, 10))
		.show(ui, |ui| {
			ui.set_width(ui.available_width());
			ui.horizontal(|ui| {
				ui.label(
					RichText::new(if edited {
						tr("Mix pass applied")
					} else {
						tr("Mix pass finished")
					})
					.font(theme::semibold(13.5))
					.color(theme::TEXT),
				);
				if edited {
					ui.label(faint(trn("{n} measure moved", "{n} measures moved", moved)));
					ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
						ui.label(faint(tr("1 undo step")));
					});
				}
			});
			if let Some(text) = &reply {
				let mut t: String = text.chars().take(320).collect();
				if text.chars().count() > 320 {
					t.push('…');
				}
				ui.label(RichText::new(t).font(theme::small()).color(theme::TEXT_DIM));
			}
			if let Some(e) = &error {
				ui.label(RichText::new(e).font(theme::small()).color(theme::ERROR));
			}
			if !edited {
				ui.label(faint(tr("The engineer made no changes.")));
			}
			ui.add_space(4.0);
			ui.horizontal(|ui| {
				let w = ((ui.available_width() - 8.0) * 0.5).max(60.0);
				if edited {
					keep = ui
						.add_sized(vec2(w, 28.0), primary(tr("Keep"), theme::medium(12.5)))
						.on_hover_text(tr("Keep the pass (one undo step \"AI mix pass\")"))
						.clicked();
					revert = ui
						.add_sized(vec2(w, 28.0), quiet(tr("Revert")))
						.on_hover_text(tr("Undo the whole pass"))
						.clicked();
				} else {
					keep = ui.add_sized(vec2(w, 28.0), quiet(tr("Close"))).clicked();
				}
			});
		});
	if !rows.is_empty() {
		ui.add_space(8.0);
		let w = ui.available_width();
		let cols = [w * 0.42, w * 0.18, w * 0.06, w * 0.16, w * 0.18];
		let row_ui =
			|ui: &mut Ui, cells: [(String, Color32, bool); 5], dot: Option<Color32>, head: bool| {
				let h = if head { 18.0 } else { 22.0 };
				let (rect, _) = ui.allocate_exact_size(vec2(w, h), Sense::hover());
				let p = ui.painter_at(rect);
				if !head {
					p.hline(
						rect.x_range(),
						rect.top(),
						Stroke::new(1.0, theme::BORDER_SUBTLE),
					);
				}
				let mut x = rect.left();
				for (i, (text, color, mono)) in cells.iter().enumerate() {
					let font = if *mono && !head {
						theme::mono(11.0)
					} else {
						theme::small()
					};
					let cell = Rect::from_min_size(pos2(x, rect.top()), vec2(cols[i], h));
					if i == 0 {
						let mut left = cell.left() + 4.0;
						if let Some(c) = dot {
							p.rect_filled(
								Rect::from_center_size(
									pos2(left + 3.5, cell.center().y),
									vec2(7.0, 7.0),
								),
								CornerRadius::same(2),
								c,
							);
							left += 12.0;
						}
						p.text(
							pos2(left, cell.center().y),
							Align2::LEFT_CENTER,
							text,
							font,
							*color,
						);
					} else if i == 2 {
						p.text(cell.center(), Align2::CENTER_CENTER, text, font, *color);
					} else {
						p.text(
							pos2(cell.right() - 4.0, cell.center().y),
							Align2::RIGHT_CENTER,
							text,
							font,
							*color,
						);
					}
					x += cols[i];
				}
			};
		let f = theme::TEXT_FAINT;
		row_ui(
			ui,
			[
				(tr("Measure").into(), f, false),
				(tr("Before").into(), f, false),
				(String::new(), f, false),
				(tr("After").into(), f, false),
				(tr("Target").into(), f, false),
			],
			None,
			true,
		);
		for (r, dot) in rows.iter().zip(colors) {
			let before = if r.before_bad {
				theme::WARNING
			} else {
				theme::TEXT_DIM
			};
			let after = if r.after_good {
				theme::PLAYING
			} else {
				theme::TEXT
			};
			row_ui(
				ui,
				[
					(crate::i18n::tr_str(&r.label).to_owned(), theme::TEXT, false),
					(r.before.clone(), before, true),
					("→".into(), theme::TEXT_FAINT, false),
					(r.after.clone(), after, true),
					(r.target.clone(), theme::TEXT_FAINT, true),
				],
				dot,
				false,
			);
		}
	}
	if keep {
		state.mix_accept();
	}
	if revert {
		state.mix_revert();
	}
	ui.add_space(6.0);
}

// -- report -------------------------------------------------------------------------------

fn report_ui(ui: &mut Ui, state: &mut AppState, report: &MixAnalysis, height: f32) {
	// The numbers and what to do first; the evidence below folds away in short panels.
	master_card(ui, state, report, height);
	if state.mix_report_stale() {
		ui.add_space(6.0);
		Frame::new()
			.fill(theme::WARNING_SOFT)
			.corner_radius(CornerRadius::same(theme::RADIUS_MD))
			.inner_margin(Margin::symmetric(10, 4))
			.show(ui, |ui| {
				ui.set_width(ui.available_width());
				ui.horizontal(|ui| {
					ui.label(
						RichText::new(tr("⚠ Mix changed since this analysis"))
							.font(theme::small())
							.color(theme::WARNING),
					);
					ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
						if ui
							.add_enabled(
								!state.mix.busy(),
								egui::Button::new(
									RichText::new(tr("Re-analyze"))
										.font(theme::medium(11.5))
										.color(theme::WARNING),
								)
								.fill(Color32::TRANSPARENT)
								.stroke(Stroke::new(1.0, theme::WARNING.gamma_multiply(0.5))),
							)
							.clicked()
						{
							state.mix_analyze();
						}
					});
				});
			});
	}
	issues(ui, state, report, height);
	tone_card(ui, state, report, height);
	channels(ui, state, report, height);
	masking(ui, state, report, height);
	ui.add_space(8.0);
}

fn master_card(ui: &mut Ui, state: &mut AppState, r: &MixAnalysis, height: f32) {
	let s = &r.master.stats;
	let silent = s.lufs_integrated <= -70.0;
	let gap = s.lufs_integrated - r.targets.lufs;
	let on_target = gap.abs() <= cadenza_analysis::targets::LOUDNESS_TOLERANCE_LU;
	let summary = if silent {
		(tr("Nothing to analyze yet").to_owned(), theme::TEXT_FAINT)
	} else if on_target {
		(
			trf(
				"✔ on target ({signed} LUFS)",
				&[("signed", &signed(r.targets.lufs, 0))],
			),
			theme::PLAYING,
		)
	} else {
		(
			if gap > 0.0 {
				trf(
					"▲ {abs} LU over target",
					&[("abs", &crate::i18n::decimal(format!("{:.1}", gap.abs())))],
				)
			} else {
				trf(
					"▼ {abs} LU under target",
					&[("abs", &crate::i18n::decimal(format!("{:.1}", gap.abs())))],
				)
			},
			theme::WARNING,
		)
	};
	if !section(
		ui,
		state,
		MixSection::Master,
		tr("Master"),
		None,
		(&summary.0, summary.1),
		height,
	)
	.0
	{
		return;
	}
	if silent {
		ui.label(
			RichText::new(tr(
				"Nothing to analyze yet; add some clips or press Play once.",
			))
			.font(theme::small())
			.color(theme::TEXT_DIM),
		);
		return;
	}
	let peak_over = s.true_peak_db > r.targets.true_peak_db;
	let low_corr = r.master.stereo.low_correlation;
	ui.horizontal_wrapped(|ui| {
		ui.spacing_mut().item_spacing.x = 18.0;
		tile(
			ui,
			&signed(s.lufs_integrated, 1),
			tr("LUFS integrated"),
			theme::mono_medium(26.0),
			theme::TEXT,
			"",
		);
		tile(
			ui,
			&signed(s.true_peak_db, 1),
			tr("dBTP peak"),
			theme::mono(15.0),
			if peak_over {
				theme::WARNING
			} else {
				theme::TEXT
			},
			&trf(
				"Ceiling {signed} dBTP",
				&[("signed", &signed(r.targets.true_peak_db, 1))],
			),
		);
		tile(
			ui,
			&format!("{:.1}", s.crest_factor_db),
			tr("dB crest"),
			theme::mono(15.0),
			theme::TEXT,
			tr("Peak to RMS: lower means more compressed"),
		);
		tile(
			ui,
			&format!("{:.0}", r.master.stereo.width * 100.0),
			tr("% width"),
			theme::mono(15.0),
			theme::TEXT,
			tr("Side share of the signal"),
		);
		tile(
			ui,
			&format!("{low_corr:.2}"),
			tr("lows mono"),
			theme::mono(15.0),
			if low_corr < 0.7 {
				theme::WARNING
			} else {
				theme::TEXT
			},
			tr("Correlation below 120 Hz: 1 = mono (good), under 0.7 the low end is wide"),
		);
	});
	ui.add_space(6.0);
	loudness_bar(ui, s.lufs_integrated, r.targets.lufs, on_target);
	ui.add_space(2.0);
	ui.label(faint(trf(
		"Target {signed} LUFS · {peak} dBTP · from {source}",
		&[
			("signed", &signed(r.targets.lufs, 1)),
			("peak", &signed(r.targets.true_peak_db, 1)),
			("source", &r.targets.source),
		],
	)));
}

/// A number over its caption.
fn tile(ui: &mut Ui, value: &str, caption: &str, font: FontId, color: Color32, hint: &str) {
	let resp = ui
		.vertical(|ui| {
			ui.spacing_mut().item_spacing.y = 0.0;
			ui.add(
				egui::Label::new(RichText::new(value).font(font).color(color)).selectable(false),
			);
			ui.add(egui::Label::new(faint(caption)).selectable(false));
		})
		.response;
	if !hint.is_empty() {
		resp.on_hover_text(hint);
	}
}

/// Integrated loudness as a bar on a fixed scale with the target tick.
fn loudness_bar(ui: &mut Ui, lufs: f32, target: f32, on_target: bool) {
	let (lo, hi) = MASTER_RANGE;
	let w = ui.available_width();
	let (rect, _) = ui.allocate_exact_size(vec2(w, 26.0), Sense::hover());
	let p = ui.painter_at(rect);
	let bar = Rect::from_min_size(rect.min + vec2(0.0, 3.0), vec2(w, 8.0));
	let x = |db: f32| bar.left() + ((db - lo) / (hi - lo)).clamp(0.0, 1.0) * bar.width();
	p.rect_filled(bar, CornerRadius::same(2), theme::BG_DEEP);
	let end = x(lufs);
	let split = x(target).min(end);
	// Up to the target the bar runs green -> yellow; past it amber.
	let mut mesh = Mesh::default();
	let seg = |mesh: &mut Mesh, x0: f32, x1: f32, c0: Color32, c1: Color32| {
		if x1 <= x0 {
			return;
		}
		let i = mesh.vertices.len() as u32;
		mesh.colored_vertex(pos2(x0, bar.top()), c0);
		mesh.colored_vertex(pos2(x1, bar.top()), c1);
		mesh.colored_vertex(pos2(x1, bar.bottom()), c1);
		mesh.colored_vertex(pos2(x0, bar.bottom()), c0);
		mesh.add_triangle(i, i + 1, i + 2);
		mesh.add_triangle(i, i + 2, i + 3);
	};
	seg(
		&mut mesh,
		bar.left(),
		split,
		theme::METER_LOW,
		if on_target {
			theme::METER_LOW
		} else {
			theme::mix(theme::METER_LOW, theme::METER_MID, 0.7)
		},
	);
	let over = if on_target {
		theme::METER_LOW
	} else {
		theme::WARNING
	};
	seg(&mut mesh, split, end, over, over);
	p.add(Shape::mesh(mesh));
	let t = x(target);
	p.vline(
		t,
		(bar.top() - 3.0)..=(bar.bottom() + 3.0),
		Stroke::new(2.0, theme::TEXT),
	);
	let y = bar.bottom() + 9.0;
	let label = |p: &egui::Painter, x: f32, align: Align2, text: String| {
		p.text(pos2(x, y), align, text, theme::mono(9.5), theme::TEXT_FAINT);
	};
	label(&p, bar.left(), Align2::LEFT_CENTER, signed(lo, 0));
	label(&p, bar.right(), Align2::RIGHT_CENTER, signed(hi, 0));
	label(&p, t, Align2::CENTER_CENTER, signed(target, 0));
}

fn severity_color(s: Severity) -> Color32 {
	match s {
		Severity::High => theme::ERROR,
		Severity::Medium => theme::WARNING,
		Severity::Low => theme::ACCENT_TEXT,
	}
}

fn issues(ui: &mut Ui, state: &mut AppState, r: &MixAnalysis, height: f32) {
	let fixable = r
		.issues
		.iter()
		.filter(|i| !i.moves.is_empty() && !state.mix.fixed.contains(&i.id))
		.count();
	let (open, header) = section(
		ui,
		state,
		MixSection::Issues,
		tr("Issues"),
		Some(r.issues.len()),
		("", theme::TEXT_FAINT),
		height,
	);
	// Fix all sits on the header, so it works folded too.
	if fixable > 1 {
		let label = trf("Fix all · {fixable}", &[("fixable", &fixable)]);
		let galley_w = ui
			.painter()
			.layout_no_wrap(label.clone(), theme::medium(11.5), theme::TEXT)
			.size()
			.x;
		let button = Rect::from_min_max(
			pos2(header.right() - galley_w - 20.0, header.top()),
			pos2(header.right(), header.bottom()),
		);
		let enabled = !state.mix.busy() && !state.ai.busy();
		let fix_all = ui.add_enabled_ui(enabled, |ui| {
			ui.put(button, primary(&label, theme::medium(11.5)))
				.on_hover_text(tr("Apply every fix as one undo step"))
				.clicked()
		});
		if fix_all.inner {
			state.mix_fix_all();
		}
	}
	if !open {
		return;
	}
	if r.issues.is_empty() {
		ui.label(
			RichText::new(tr("✔ No issues: the mix meets its targets"))
				.font(theme::small())
				.color(theme::PLAYING),
		);
		return;
	}
	let names = |t: Option<TrackId>| r.name_of(t);
	for issue in &r.issues {
		issue_card(ui, state, issue, &names);
		ui.add_space(4.0);
	}
}

fn issue_card(
	ui: &mut Ui,
	state: &mut AppState,
	issue: &Issue,
	names: &dyn Fn(Option<TrackId>) -> String,
) {
	let fixed = state.mix.fixed.contains(&issue.id);
	let color = severity_color(issue.severity);
	let (head, detail) = mix::issue_parts(&issue.message);
	let who = names(issue.track_id);
	let chip_color = track_color(state, issue.track_id);
	let busy = state.mix.busy() || state.ai.busy();
	let mut open_window = false;
	let mut fix = false;
	Frame::new()
		.fill(if fixed {
			theme::BG_PANEL
		} else {
			theme::BG_RAISED
		})
		.stroke(Stroke::new(1.0, theme::BORDER_SUBTLE))
		.corner_radius(CornerRadius::same(theme::RADIUS_MD))
		.inner_margin(Margin {
			left: 10,
			right: 8,
			top: 8,
			bottom: 8,
		})
		.show(ui, |ui| {
			ui.set_width(ui.available_width());
			if fixed {
				ui.multiply_opacity(0.6);
			}
			ui.horizontal_top(|ui| {
				// Severity glyph: a warning triangle, or an "i" for notes.
				let (g, _) = ui.allocate_exact_size(vec2(14.0, 16.0), Sense::hover());
				let p = ui.painter();
				if issue.severity == Severity::Low {
					p.circle_stroke(g.center(), 5.5, Stroke::new(1.2, color));
					p.text(
						g.center(),
						Align2::CENTER_CENTER,
						"i",
						theme::semibold(9.5),
						color,
					);
				} else {
					let c = g.center() + vec2(0.0, 0.5);
					p.add(Shape::convex_polygon(
						vec![c + vec2(0.0, -6.0), c + vec2(6.5, 5.0), c + vec2(-6.5, 5.0)],
						color,
						Stroke::NONE,
					));
					p.text(
						c + vec2(0.0, 1.0),
						Align2::CENTER_CENTER,
						"!",
						theme::semibold(9.0),
						theme::TEXT_ON_COLOR,
					);
				}
				let action_w = if fixed { 58.0 } else { 40.0 };
				let text_w = (ui.available_width() - action_w - 6.0).max(80.0);
				ui.allocate_ui_with_layout(
					vec2(text_w, 16.0),
					Layout::top_down(Align::Min),
					|ui| {
						ui.spacing_mut().item_spacing.y = 3.0;
						ui.add(
							egui::Label::new(
								RichText::new(&head)
									.font(theme::medium(12.5))
									.color(theme::TEXT),
							)
							.wrap()
							.selectable(false),
						);
						if let Some(d) = &detail {
							ui.add(
								egui::Label::new(
									RichText::new(d).font(theme::small()).color(theme::TEXT_DIM),
								)
								.wrap()
								.selectable(false),
							);
						}
						if issue.kind == IssueKind::EngineWarning {
							ui.label(faint(tr("render warning")));
						}
						ui.add_space(1.0);
						ui.horizontal_wrapped(|ui| {
							ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
							let hint = match issue.track_id {
								Some(_) => trf("Open {who}'s track window", &[("who", &who)]),
								None => tr("Open the master channel's window").to_owned(),
							};
							if chip(ui, Some(chip_color), &who, theme::small(), Sense::click())
								.on_hover_text(hint)
								.clicked()
							{
								open_window = true;
							}
							let prefix = format!("{who}: ");
							for m in &issue.moves {
								let text = m.describe(names);
								let text = text.strip_prefix(&prefix).unwrap_or(&text).to_owned();
								chip(ui, None, &text, theme::mono(10.0), Sense::hover());
							}
						});
					},
				);
				ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
					if fixed {
						ui.label(
							RichText::new(tr("✔ Fixed"))
								.font(theme::medium(11.5))
								.color(theme::PLAYING),
						)
						.on_hover_text(tr("Fixed (one undo step)"));
					} else if !issue.moves.is_empty() {
						let moves: Vec<String> =
							issue.moves.iter().map(|m| m.describe(names)).collect();
						fix = ui
							.add_enabled(!busy, quiet(tr("Fix")).min_size(vec2(36.0, 22.0)))
							.on_hover_text(moves.join("\n"))
							.clicked();
					}
				});
			});
		});
	if open_window {
		state.open_track_window(issue.track_id);
	}
	if fix {
		state.mix_fix(&issue.id);
	}
}

/// Octave spectrum of the master (filled) over the target tone curve (dashed), the worst
/// band shaded and labelled.
fn tone_card(ui: &mut Ui, state: &mut AppState, r: &MixAnalysis, height: f32) {
	let deviation = mix::tone_deviation(r);
	let (worst_i, (worst_name, worst)) = deviation
		.iter()
		.copied()
		.enumerate()
		.max_by(|a, b| a.1.1.abs().total_cmp(&b.1.1.abs()))
		.unwrap_or((0, ("", 0.0)));
	let summary = format!("{worst_name} {} dB", delta(worst, 1));
	let color = if worst.abs() > 4.0 {
		theme::WARNING
	} else {
		theme::TEXT_FAINT
	};
	if !section(
		ui,
		state,
		MixSection::Tone,
		tr("Tone"),
		None,
		(&summary, color),
		height,
	)
	.0
	{
		return;
	}
	let shape = r.master.spectrum.octave_shape();
	if shape.len() < OCTAVE_HZ.len() {
		return;
	}
	let target = mix::tone_target(&shape, &deviation);
	let w = ui.available_width();
	let (rect, resp) = ui.allocate_exact_size(vec2(w, 112.0), Sense::hover());
	let p = ui.painter_at(rect);
	p.rect_filled(
		rect,
		CornerRadius::same(theme::RADIUS_MD),
		theme::DISPLAY_BG,
	);
	let plot = Rect::from_min_max(rect.min + vec2(0.0, 6.0), rect.max - vec2(0.0, 16.0));
	let (f_lo, f_hi) = (20.0f32.log10(), 20_000.0f32.log10());
	let fx = |f: f32| plot.left() + (f.log10() - f_lo) / (f_hi - f_lo) * plot.width();
	let top = target
		.iter()
		.chain(shape.iter())
		.copied()
		.fold(0.0f32, f32::max)
		+ 3.0;
	let bottom = -36.0f32;
	let fy = |db: f32| plot.top() + ((top - db) / (top - bottom)).clamp(0.0, 1.0) * plot.height();
	for (f, label) in [
		(30.0, "30"),
		(100.0, "100"),
		(300.0, "300"),
		(1_000.0, "1k"),
		(3_000.0, "3k"),
		(10_000.0, "10k"),
	] {
		let x = fx(f);
		p.vline(x, plot.y_range(), Stroke::new(1.0, theme::GRID_BAR));
		p.text(
			pos2(x + 3.0, rect.bottom() - 3.0),
			Align2::LEFT_BOTTOM,
			label,
			theme::mono(9.0),
			theme::TEXT_FAINT,
		);
	}
	// The worst tone band, when it is off by more than the tolerance.
	let edges = [20.0, 150.0, 500.0, 2_000.0, 6_000.0, 20_000.0];
	if worst.abs() > 1.5 {
		let band = if worst_i >= 2 { worst_i + 1 } else { worst_i };
		let (a, b) = (fx(edges[band]), fx(edges[band + 1]));
		let shade = Rect::from_x_y_ranges(a..=b, plot.top() - 6.0..=plot.bottom());
		p.rect_filled(
			shade,
			CornerRadius::ZERO,
			theme::WARNING.gamma_multiply(0.12),
		);
		p.text(
			pos2(a + 4.0, plot.top() - 2.0),
			Align2::LEFT_TOP,
			format!("{} dB", delta(worst, 1)),
			theme::mono(10.0),
			theme::WARNING,
		);
	}
	// Mix: a smooth line through the octave levels, filled to the bottom.
	let points: Vec<egui::Pos2> = sample_curve(&shape, 64)
		.into_iter()
		.map(|(f, v)| pos2(fx(f), fy(v)))
		.collect();
	let mut mesh = Mesh::default();
	let fill = TONE.gamma_multiply(0.22);
	let fill_bottom = TONE.gamma_multiply(0.04);
	for pt in &points {
		mesh.colored_vertex(*pt, fill);
		mesh.colored_vertex(pos2(pt.x, plot.bottom()), fill_bottom);
	}
	for i in 0..points.len().saturating_sub(1) as u32 {
		let a = i * 2;
		mesh.add_triangle(a, a + 1, a + 2);
		mesh.add_triangle(a + 1, a + 3, a + 2);
	}
	p.add(Shape::mesh(mesh));
	p.add(Shape::line(points, Stroke::new(1.5, TONE)));
	// Target: dashed.
	let target_pts: Vec<egui::Pos2> = sample_curve(&target, 64)
		.into_iter()
		.map(|(f, v)| pos2(fx(f), fy(v)))
		.collect();
	p.extend(Shape::dashed_line(
		&target_pts,
		Stroke::new(1.2, theme::TEXT_DIM),
		5.0,
		4.0,
	));
	resp.on_hover_text(trf("Octave spectrum of the master (relative to its loudest band) against the {name} tone curve. Per band: {bands}", &[("name", &crate::i18n::tr_str(r.targets.profile.name())), ("bands", &deviation
			.iter()
			.map(|(n, d)| format!("{n} {} dB", delta(*d, 1)))
			.collect::<Vec<_>>()
			.join(", "))]));
	ui.add_space(4.0);
	ui.horizontal_wrapped(|ui| {
		ui.spacing_mut().item_spacing.x = 12.0;
		legend(ui, TONE, false, tr("Mix"));
		legend(
			ui,
			theme::TEXT_DIM,
			true,
			&trf(
				"Target “{name}”",
				&[("name", &crate::i18n::tr_str(r.targets.profile.name()))],
			),
		);
		for (name, d) in deviation {
			let c = if d.abs() > 4.0 {
				theme::WARNING
			} else {
				theme::TEXT_DIM
			};
			ui.label(
				RichText::new(format!("{name} {}", delta(d, 1)))
					.font(theme::mono(10.0))
					.color(c),
			);
		}
	});
}

/// Points of a smooth curve (Catmull-Rom in log frequency) through octave values.
fn sample_curve(values: &[f32], per_segment: usize) -> Vec<(f32, f32)> {
	let n = values.len().min(OCTAVE_HZ.len());
	if n < 2 {
		return Vec::new();
	}
	let lf: Vec<f32> = OCTAVE_HZ[..n].iter().map(|f| f.log10()).collect();
	let v = |i: isize| values[i.clamp(0, n as isize - 1) as usize];
	let steps = (per_segment / (n - 1)).max(2);
	let mut out = Vec::new();
	for i in 0..n - 1 {
		let (p0, p1, p2, p3) = (
			v(i as isize - 1),
			v(i as isize),
			v(i as isize + 1),
			v(i as isize + 2),
		);
		for s in 0..steps {
			let t = s as f32 / steps as f32;
			let t2 = t * t;
			let t3 = t2 * t;
			let y = 0.5
				* (2.0 * p1
					+ (-p0 + p2) * t
					+ (2.0 * p0 - 5.0 * p1 + 4.0 * p2 - p3) * t2
					+ (-p0 + 3.0 * p1 - 3.0 * p2 + p3) * t3);
			let x = lf[i] + (lf[i + 1] - lf[i]) * t;
			out.push((10f32.powf(x), y));
		}
	}
	out.push((OCTAVE_HZ[n - 1], values[n - 1]));
	out
}

fn legend(ui: &mut Ui, color: Color32, dashed: bool, text: &str) {
	ui.horizontal(|ui| {
		ui.spacing_mut().item_spacing.x = 4.0;
		let (r, _) = ui.allocate_exact_size(vec2(14.0, 10.0), Sense::hover());
		let p = ui.painter();
		if dashed {
			p.extend(Shape::dashed_line(
				&[pos2(r.left(), r.center().y), pos2(r.right(), r.center().y)],
				Stroke::new(1.2, color),
				3.0,
				2.0,
			));
		} else {
			p.rect_filled(
				r.shrink2(vec2(0.0, 1.0)),
				CornerRadius::same(1),
				color.gamma_multiply(0.35),
			);
			p.hline(r.x_range(), r.top() + 1.0, Stroke::new(1.5, color));
		}
		ui.label(faint(text));
	});
}

fn channels(ui: &mut Ui, state: &mut AppState, r: &MixAnalysis, height: f32) {
	let rows = mix::channel_rows(r);
	let off = rows.iter().filter(|c| c.out_db != 0.0).count();
	let summary = if off == 0 {
		tr("integrated LUFS · band = suggested").to_owned()
	} else {
		trf("{off} outside the suggested band", &[("off", &off)])
	};
	if !section(
		ui,
		state,
		MixSection::Channels,
		tr("Channels"),
		None,
		(
			&summary,
			if off == 0 {
				theme::TEXT_FAINT
			} else {
				theme::WARNING
			},
		),
		height,
	)
	.0
	{
		return;
	}
	let (lo, hi) = CHANNEL_RANGE;
	for row in rows {
		let c = &r.channels[row.index];
		let color = track_color(state, Some(c.track_id));
		let s = &c.detail.stats;
		let w = ui.available_width();
		let (rect, resp) = ui.allocate_exact_size(vec2(w, 22.0), Sense::click());
		let p = ui.painter_at(rect);
		if resp.hovered() {
			p.rect_filled(rect, CornerRadius::same(theme::RADIUS), theme::BG_RAISED);
		}
		let cy = rect.center().y;
		p.rect_filled(
			Rect::from_center_size(pos2(rect.left() + 6.0, cy), vec2(8.0, 8.0)),
			CornerRadius::same(2),
			color,
		);
		let name_w = (w * 0.24).clamp(60.0, 110.0);
		let name = p.layout(c.name.clone(), theme::small(), theme::TEXT, name_w - 18.0);
		let name_rows = name.rows.len();
		let galley = if name_rows > 1 {
			p.layout_no_wrap(
				format!("{}…", c.name.chars().take(10).collect::<String>()),
				theme::small(),
				theme::TEXT,
			)
		} else {
			name
		};
		p.galley(
			pos2(rect.left() + 16.0, cy - galley.size().y * 0.5),
			galley,
			theme::TEXT,
		);
		let value_w = 92.0;
		let bar = Rect::from_min_max(
			pos2(rect.left() + name_w, cy - 4.0),
			pos2(rect.right() - value_w, cy + 4.0),
		);
		let x = |db: f32| bar.left() + ((db - lo) / (hi - lo)).clamp(0.0, 1.0) * bar.width();
		p.rect_filled(bar, CornerRadius::same(2), theme::BG_DEEP);
		if let Some((b0, b1)) = row.band {
			let band =
				Rect::from_x_y_ranges(x(b0)..=x(b1), (bar.top() - 2.0)..=(bar.bottom() + 2.0));
			p.rect_filled(band, CornerRadius::same(2), theme::BG_HOVER);
			p.rect_stroke(
				band,
				CornerRadius::same(2),
				Stroke::new(1.0, theme::BORDER_STRONG),
				StrokeKind::Inside,
			);
		}
		let level = if row.out_db != 0.0 {
			theme::WARNING
		} else {
			theme::METER_LOW
		};
		if !row.silent {
			p.rect_filled(
				Rect::from_x_y_ranges(bar.left()..=x(row.lufs), bar.y_range()),
				CornerRadius::same(2),
				level.gamma_multiply(0.9),
			);
			p.vline(
				x(row.peak_db),
				(bar.top() - 2.0)..=(bar.bottom() + 2.0),
				Stroke::new(1.5, theme::TEXT),
			);
		}
		p.text(
			pos2(rect.right() - 40.0, cy),
			Align2::RIGHT_CENTER,
			if row.silent {
				"silent".to_owned()
			} else {
				signed(row.lufs, 1)
			},
			theme::mono(11.0),
			theme::TEXT_DIM,
		);
		let (status, status_color) = if row.silent {
			(String::new(), theme::TEXT_FAINT)
		} else if row.band.is_none() {
			(
				if c.kind == cadenza_model::TrackKind::Bus {
					"bus".into()
				} else {
					String::new()
				},
				theme::TEXT_FAINT,
			)
		} else if row.out_db == 0.0 {
			("ok".into(), theme::PLAYING)
		} else {
			(delta(row.out_db, 1), theme::WARNING)
		};
		p.text(
			pos2(rect.right() - 2.0, cy),
			Align2::RIGHT_CENTER,
			status,
			theme::mono(11.0),
			status_color,
		);
		let band_text = row.band.map_or_else(String::new, |(b0, b1)| {
			trf(
				"\nsuggested {from} to {to} LUFS for a {role}",
				&[
					("from", &signed(b0, 1)),
					("to", &signed(b1, 1)),
					("role", &crate::i18n::tr_str(c.role.name())),
				],
			)
		});
		let resp = resp.on_hover_text(format!(
			"{} ({})\n{}{band_text}\n{}",
			c.name,
			crate::i18n::tr_str(c.role.name()),
			trf(
				"{lufs} LUFS · peak {peak} dBFS (tick) · crest {crest} dB · width {width}%",
				&[
					("lufs", &crate::i18n::num(f64::from(s.lufs_integrated), 1)),
					("peak", &crate::i18n::num(f64::from(s.peak_db), 1)),
					("crest", &crate::i18n::num(f64::from(s.crest_factor_db), 1)),
					(
						"width",
						&crate::i18n::num(f64::from(c.detail.stereo.width * 100.0), 0)
					),
				],
			),
			tr("Click: track window"),
		));
		if resp.clicked() {
			state.open_track_window(Some(c.track_id));
		}
	}
}

fn masking(ui: &mut Ui, state: &mut AppState, r: &MixAnalysis, height: f32) {
	if r.masking.is_empty() {
		return;
	}
	let shown = r.masking.len().min(5);
	let summary = trn("{n} pair", "{n} pairs", shown);
	if !section(
		ui,
		state,
		MixSection::Masking,
		tr("Masking"),
		None,
		(&summary, theme::TEXT_FAINT),
		height,
	)
	.0
	{
		return;
	}
	for m in r.masking.iter().take(5) {
		let (a, b, cut) = (&r.channels[m.a], &r.channels[m.b], &r.channels[m.cut]);
		let (steps, word) = mix::masking_level(m.overlap);
		let word_color = match word {
			"High" => theme::WARNING,
			"Medium" => theme::METER_MID,
			_ => theme::TEXT_DIM,
		};
		let word = crate::i18n::tr_str(word);
		let w = ui.available_width();
		let (rect, resp) = ui.allocate_exact_size(vec2(w, 24.0), Sense::hover());
		let p = ui.painter_at(rect);
		let cy = rect.center().y;
		let mut x = rect.left();
		for (i, ch) in [a, b].into_iter().enumerate() {
			if i == 1 {
				let r = p.text(
					pos2(x, cy),
					Align2::LEFT_CENTER,
					"×",
					theme::small(),
					theme::TEXT_FAINT,
				);
				x = r.right() + 5.0;
			}
			p.rect_filled(
				Rect::from_center_size(pos2(x + 4.0, cy), vec2(8.0, 8.0)),
				CornerRadius::same(2),
				track_color(state, Some(ch.track_id)),
			);
			let r = p.text(
				pos2(x + 12.0, cy),
				Align2::LEFT_CENTER,
				&ch.name,
				theme::small(),
				theme::TEXT,
			);
			x = r.right() + 6.0;
		}
		let meter_w = 5.0 * 14.0;
		let word_w = 52.0;
		let meter_x = rect.right() - word_w - meter_w;
		if x + 70.0 < meter_x {
			p.text(
				pos2(x + 2.0, cy),
				Align2::LEFT_CENTER,
				mix::octave_range(m.band_hz),
				theme::mono(10.0),
				theme::TEXT_FAINT,
			);
		}
		for i in 0..5 {
			let r = Rect::from_min_size(pos2(meter_x + i as f32 * 14.0, cy - 2.5), vec2(11.0, 5.0));
			p.rect_filled(
				r,
				CornerRadius::same(1),
				if i < steps {
					word_color
				} else {
					theme::BG_WIDGET
				},
			);
		}
		p.text(
			pos2(rect.right() - 2.0, cy),
			Align2::RIGHT_CENTER,
			word,
			theme::small(),
			word_color,
		);
		resp.on_hover_text(trf(
			"{a} and {b} collide around {band} {percent}% of the time; {cut} should make room",
			&[
				("a", &a.name),
				("b", &b.name),
				("band", &mix::octave_range(m.band_hz)),
				(
					"percent",
					&crate::i18n::decimal(format!("{:.0}", m.overlap * 100.0)),
				),
				("cut", &cut.name),
			],
		));
	}
}
