//! The AI panel's Mix tab: Analyze (worker with progress), the report card (master
//! numbers, octave spectrum, tone vs the target, per-channel loudness bars, masking pairs,
//! issues with Fix buttons) and the AI mix pass with its before/after numbers and Keep /
//! Revert. Logic lives in [`crate::state::mix`].

use cadenza_analysis::issues::{Issue, IssueKind, Severity};
use cadenza_analysis::moves::hz;
use cadenza_analysis::spectrum::OCTAVE_HZ;
use cadenza_mcp::tools::mix::{MetricChange, MixAnalysis};
use egui::{
	Align, Align2, Color32, CornerRadius, FontId, Frame, Layout, Margin, Rect, RichText,
	ScrollArea, Sense, Stroke, Ui, pos2, vec2,
};

use crate::jobs::FileAction;
use crate::state::AppState;
use crate::state::mix::{PassPhase, Purpose};
use crate::theme;

/// Loudness bars span this range (LUFS).
const BAR_FLOOR: f32 = -48.0;

pub fn show(ui: &mut Ui, state: &mut AppState) {
	toolbar(ui, state);
	ui.add_space(2.0);
	status(ui, state);
	ScrollArea::vertical()
		.id_salt("mix-assistant")
		.auto_shrink([false, false])
		.show(ui, |ui| {
			ui.set_width(ui.available_width());
			review(ui, state);
			match state.mix.report.clone() {
				Some(report) => report_ui(ui, state, &report),
				None if state.mix.job.is_none() => empty(ui),
				None => {}
			}
		});
}

fn section(ui: &mut Ui, text: &str) {
	ui.add_space(6.0);
	ui.label(
		RichText::new(text)
			.font(theme::small())
			.strong()
			.color(theme::TEXT_DIM),
	);
}

fn faint(text: impl Into<String>) -> RichText {
	RichText::new(text)
		.font(theme::small())
		.color(theme::TEXT_FAINT)
}

fn card<R>(ui: &mut Ui, stroke: Color32, body: impl FnOnce(&mut Ui) -> R) -> R {
	Frame::new()
		.fill(theme::BG_RAISED)
		.stroke(Stroke::new(1.0, stroke))
		.corner_radius(theme::RADIUS)
		.inner_margin(Margin::symmetric(8, 6))
		.show(ui, |ui| {
			ui.set_width(ui.available_width());
			body(ui)
		})
		.inner
}

fn toolbar(ui: &mut Ui, state: &mut AppState) {
	let busy = state.mix.busy() || state.ai.busy();
	ui.horizontal(|ui| {
		if ui
			.add_enabled(!busy, egui::Button::new("Analyze"))
			.on_hover_text("Render the mix offline and measure it against the song's targets")
			.clicked()
		{
			state.mix_analyze();
		}
		let pass = egui::Button::new(RichText::new("AI mix pass").color(theme::ACCENT))
			.stroke(Stroke::new(1.0, theme::ACCENT_DIM));
		if ui
			.add_enabled(!busy, pass)
			.on_hover_text(
				"The AI sound engineer mixes the song (gain staging, EQ, dynamics, space, pan, \
				 loudness) as one undo step, then shows what changed",
			)
			.clicked()
		{
			state.mix_ai_pass();
		}
	});
	ui.horizontal(|ui| {
		ui.label(faint("Target"));
		let mut custom = state.mix.target_lufs.is_some();
		let style_lufs = state.mix.report.as_ref().map_or(-14.0, |r| r.targets.lufs);
		if ui
			.checkbox(&mut custom, "")
			.on_hover_text("Override the loudness target of the song's style")
			.changed()
		{
			state.mix.target_lufs = custom.then_some(style_lufs);
		}
		match &mut state.mix.target_lufs {
			Some(lufs) => {
				ui.add(
					egui::DragValue::new(lufs)
						.range(-30.0..=-5.0)
						.speed(0.1)
						.fixed_decimals(1)
						.suffix(" LUFS"),
				);
			}
			None => {
				ui.label(RichText::new("style").color(theme::TEXT_DIM));
			}
		}
		ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
			if let Some(path) = state.mix.reference.clone() {
				if ui
					.small_button("×")
					.on_hover_text("Forget the reference")
					.clicked()
				{
					state.mix.reference = None;
				}
				let name = path
					.file_name()
					.map_or_else(String::new, |f| f.to_string_lossy().into_owned());
				ui.add(
					egui::Label::new(RichText::new(name).font(theme::small()))
						.truncate()
						.selectable(false),
				)
				.on_hover_text(path.display().to_string());
			} else if ui
				.small_button("Reference…")
				.on_hover_text("A finished track the AI mix pass steers tone and loudness toward")
				.clicked()
			{
				state.file_dialog(FileAction::MixReference);
			}
		});
	});
}

fn status(ui: &mut Ui, state: &mut AppState) {
	if let Some((purpose, progress)) = state.mix.analyzing() {
		card(ui, theme::ACCENT_DIM, |ui| {
			ui.horizontal(|ui| {
				ui.label(
					RichText::new(match purpose {
						Purpose::Report => "Analyzing the mix…",
						Purpose::BeforePass => "Measuring before the pass…",
						Purpose::AfterPass => "Measuring the result…",
					})
					.font(theme::small())
					.color(theme::TEXT),
				);
				ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
					if ui.small_button("Cancel").clicked() {
						state.mix_cancel();
					}
				});
			});
			ui.add(
				egui::ProgressBar::new(progress)
					.show_percentage()
					.desired_height(12.0),
			);
		});
		ui.ctx()
			.request_repaint_after(std::time::Duration::from_millis(100));
	} else if state
		.mix
		.pass
		.as_ref()
		.is_some_and(|p| p.phase == PassPhase::Running)
	{
		let running = state.ai.running.clone();
		card(ui, theme::ACCENT_DIM, |ui| {
			ui.horizontal(|ui| {
				ui.spinner();
				let secs = running
					.as_ref()
					.map_or(0.0, |r| r.started.elapsed().as_secs_f32());
				ui.label(
					RichText::new(format!("The engineer is mixing… {secs:.0} s"))
						.font(theme::small())
						.color(theme::TEXT),
				);
				ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
					let stop = egui::Button::new(RichText::new("■ Stop").color(theme::ERROR))
						.stroke(Stroke::new(1.0, theme::ERROR.gamma_multiply(0.6)));
					if ui
						.add(stop)
						.on_hover_text("Stop at the next step; see the chat (Producer tab)")
						.clicked()
					{
						state.ai.stop();
					}
				});
			});
		});
		ui.ctx()
			.request_repaint_after(std::time::Duration::from_millis(250));
	}
	if let Some(e) = &state.mix.error {
		ui.label(RichText::new(e).font(theme::small()).color(theme::ERROR));
	}
}

fn empty(ui: &mut Ui) {
	ui.add_space(12.0);
	ui.label(
		RichText::new(
			"Analyze measures the mix offline: loudness against the song's target, peaks, \
			 tone, stereo image, how every part sits, and which parts mask each other. Each \
			 issue comes with a one-click fix; an AI mix pass works through them like a \
			 sound engineer.",
		)
		.color(theme::TEXT_DIM),
	);
}

/// Before -> after of an AI mix pass, with Keep / Revert.
fn review(ui: &mut Ui, state: &mut AppState) {
	let Some(pass) = &state.mix.pass else { return };
	if pass.phase != PassPhase::Review {
		return;
	}
	let (edited, error, changes) = (pass.edited, pass.error.clone(), pass.changes.clone());
	let mut keep = false;
	let mut revert = false;
	card(ui, theme::ACCENT_DIM, |ui| {
		ui.label(
			RichText::new("AI MIX PASS")
				.font(theme::small())
				.strong()
				.color(theme::ACCENT),
		);
		if let Some(e) = &error {
			ui.label(RichText::new(e).font(theme::small()).color(theme::ERROR));
		}
		if !edited {
			ui.label(faint("The engineer made no changes."));
		}
		let shown: Vec<&MetricChange> = changes
			.iter()
			.filter(|c| c.delta().abs() >= 0.05 || c.label == "Loudness")
			.collect();
		egui::Grid::new("mix-pass-changes")
			.num_columns(3)
			.spacing(vec2(10.0, 2.0))
			.show(ui, |ui| {
				for c in shown {
					ui.label(RichText::new(&c.label).color(theme::TEXT));
					ui.label(
						RichText::new(format!(
							"{} → {} {}",
							number(c.before),
							number(c.after),
							c.unit
						))
						.font(theme::mono(11.0))
						.color(theme::TEXT),
					);
					let d = c.delta();
					ui.label(
						RichText::new(if d.abs() < 0.05 {
							"·".to_owned()
						} else {
							format!("{d:+.1}")
						})
						.font(theme::mono(11.0))
						.color(delta_color(&c.label, d)),
					);
					ui.end_row();
				}
			});
		ui.add_space(4.0);
		ui.horizontal(|ui| {
			if edited {
				keep = ui
					.button(RichText::new("✔ Keep").color(theme::PLAYING))
					.on_hover_text("Keep the pass (one undo step \"AI mix pass\")")
					.clicked();
				revert = ui
					.button("Revert")
					.on_hover_text("Undo the whole pass")
					.clicked();
			} else {
				keep = ui.button("Close").clicked();
			}
		});
	});
	if keep {
		state.mix_accept();
	}
	if revert {
		state.mix_revert();
	}
	ui.add_space(4.0);
}

fn number(v: f64) -> String {
	if v.fract().abs() < 1e-9 && v.abs() < 1000.0 {
		format!("{v:.0}")
	} else {
		format!("{v:.1}")
	}
}

/// Green when a pass moved a number the right way (fewer issues, lower peaks).
fn delta_color(label: &str, d: f64) -> Color32 {
	if d.abs() < 0.05 {
		return theme::TEXT_FAINT;
	}
	let better = match label {
		"Issues" | "Severe issues" | "Masking pairs" | "True peak" => d < 0.0,
		_ => return theme::TEXT_DIM,
	};
	if better {
		theme::PLAYING
	} else {
		theme::WARNING
	}
}

fn report_ui(ui: &mut Ui, state: &mut AppState, report: &MixAnalysis) {
	if state.mix_report_stale() {
		ui.horizontal(|ui| {
			ui.label(
				RichText::new("The project changed since this analysis")
					.font(theme::small())
					.color(theme::WARNING),
			);
			if ui
				.add_enabled(!state.mix.busy(), egui::Button::new("Analyze again"))
				.clicked()
			{
				state.mix_analyze();
			}
		});
	}
	master_card(ui, report);
	channels(ui, state, report);
	masking(ui, report);
	issues(ui, state, report);
}

fn master_card(ui: &mut Ui, r: &MixAnalysis) {
	section(ui, "MASTER");
	let s = &r.master.stats;
	card(ui, theme::BORDER, |ui| {
		ui.horizontal(|ui| {
			let gap = s.lufs_integrated - r.targets.lufs;
			let lufs_color = if s.lufs_integrated <= -70.0 {
				theme::TEXT_FAINT
			} else if gap.abs() <= 1.5 {
				theme::PLAYING
			} else {
				theme::WARNING
			};
			ui.label(
				RichText::new(format!("{:.1}", s.lufs_integrated))
					.font(FontId::proportional(20.0))
					.color(lufs_color),
			);
			ui.vertical(|ui| {
				ui.label(faint("LUFS integrated"));
				ui.label(faint(format!(
					"target {:.1} · {}",
					r.targets.lufs, r.targets.source
				)));
			});
		});
		ui.horizontal_wrapped(|ui| {
			let tp_color = if s.true_peak_db > r.targets.true_peak_db {
				theme::ERROR
			} else {
				theme::TEXT
			};
			stat(
				ui,
				"true peak",
				format!("{:.1} dBTP", s.true_peak_db),
				tp_color,
			);
			stat(
				ui,
				"crest",
				format!("{:.1} dB", s.crest_factor_db),
				theme::TEXT,
			);
			stat(
				ui,
				"width",
				format!("{:.0}%", r.master.stereo.width * 100.0),
				theme::TEXT,
			);
			stat(
				ui,
				"lows mono",
				format!("{:.2}", r.master.stereo.low_correlation),
				if r.master.stereo.low_correlation < 0.7 {
					theme::WARNING
				} else {
					theme::TEXT
				},
			);
		});
		ui.add_space(4.0);
		spectrum(ui, r);
		ui.add_space(2.0);
		tone(ui, r);
	});
}

fn stat(ui: &mut Ui, label: &str, value: String, color: Color32) {
	ui.label(faint(label));
	ui.label(RichText::new(value).font(theme::mono(11.0)).color(color));
	ui.add_space(4.0);
}

/// Octave-band shape of the master: bars from the loudest band down to -36 dB.
fn spectrum(ui: &mut Ui, r: &MixAnalysis) {
	let w = ui.available_width();
	let (rect, _) = ui.allocate_exact_size(vec2(w, 58.0), Sense::hover());
	let p = ui.painter_at(rect);
	p.rect_filled(rect, CornerRadius::same(theme::RADIUS), theme::METER_BG);
	let shape = r.master.spectrum.octave_shape();
	let n = shape.len() as f32;
	let label_h = 12.0;
	let plot = Rect::from_min_max(rect.min + vec2(4.0, 4.0), rect.max - vec2(4.0, label_h));
	let bw = plot.width() / n;
	for (i, v) in shape.iter().enumerate() {
		let f = ((v + 36.0) / 36.0).clamp(0.0, 1.0);
		let x0 = plot.left() + i as f32 * bw;
		let bar = Rect::from_min_max(
			pos2(x0 + 2.0, plot.bottom() - f * plot.height()),
			pos2(x0 + bw - 2.0, plot.bottom()),
		);
		let color = theme::mix(theme::ACCENT_DIM, theme::ACCENT, f);
		p.rect_filled(bar, CornerRadius::same(1), color);
		if i % 2 == 1 {
			p.text(
				pos2(x0 + bw * 0.5, rect.bottom() - 1.0),
				Align2::CENTER_BOTTOM,
				short_hz(OCTAVE_HZ[i]),
				FontId::proportional(9.0),
				theme::TEXT_FAINT,
			);
		}
	}
}

fn short_hz(f: f32) -> String {
	if f >= 1_000.0 {
		format!("{:.0}k", f / 1_000.0)
	} else {
		format!("{f:.0}")
	}
}

/// Band balance against the target profile: centered bars, + = too much.
fn tone(ui: &mut Ui, r: &MixAnalysis) {
	let b = &r.master.stats.band_energy_db;
	let c = r.targets.profile.curve();
	let rows = [
		("lows", b.low - b.mid - c.low),
		("low-mids", b.low_mid - b.mid - c.low_mid),
		("high-mids", b.high_mid - b.mid - c.high_mid),
		("highs", b.high - b.mid - c.high),
	];
	ui.label(faint(format!(
		"tone vs {} target",
		r.targets.profile.name()
	)));
	for (name, d) in rows {
		ui.horizontal(|ui| {
			ui.add_sized(
				vec2(58.0, 12.0),
				egui::Label::new(faint(name)).selectable(false),
			);
			let w = (ui.available_width() - 44.0).max(40.0);
			let (rect, _) = ui.allocate_exact_size(vec2(w, 8.0), Sense::hover());
			let p = ui.painter_at(rect);
			p.rect_filled(rect, CornerRadius::same(2), theme::METER_BG);
			let mid = rect.center().x;
			p.vline(mid, rect.y_range(), Stroke::new(1.0, theme::BORDER_STRONG));
			let f = (d / 12.0).clamp(-1.0, 1.0);
			let end = mid + f * rect.width() * 0.5;
			let color = if d.abs() <= 4.0 {
				theme::METER_LOW
			} else if d.abs() <= 7.0 {
				theme::METER_MID
			} else {
				theme::METER_HIGH
			};
			p.rect_filled(
				Rect::from_x_y_ranges(mid.min(end)..=mid.max(end), rect.y_range()),
				CornerRadius::same(2),
				color,
			);
			ui.label(
				RichText::new(format!("{d:+.1}"))
					.font(theme::mono(10.0))
					.color(theme::TEXT_DIM),
			);
		});
	}
}

fn channels(ui: &mut Ui, state: &AppState, r: &MixAnalysis) {
	section(ui, "CHANNELS");
	card(ui, theme::BORDER, |ui| {
		ui.spacing_mut().item_spacing.y = 3.0;
		for c in &r.channels {
			let color = state
				.project()
				.track(c.track_id)
				.map_or(theme::DEFAULT_TRACK, |t| {
					theme::track_color(t.color.as_deref())
				});
			let silent = c.detail.is_silent();
			let s = &c.detail.stats;
			ui.horizontal(|ui| {
				let (chip, _) = ui.allocate_exact_size(vec2(4.0, 14.0), Sense::hover());
				ui.painter().rect_filled(chip, CornerRadius::same(1), color);
				ui.add_sized(
					vec2(78.0, 14.0),
					egui::Label::new(
						RichText::new(&c.name)
							.font(theme::small())
							.color(theme::TEXT),
					)
					.truncate()
					.selectable(false),
				)
				.on_hover_text(format!(
					"{} ({})\n{:.1} LUFS · peak {:.1} dBFS · crest {:.1} dB · width {:.0}%",
					c.name,
					c.role.name(),
					s.lufs_integrated,
					s.peak_db,
					s.crest_factor_db,
					c.detail.stereo.width * 100.0
				));
				ui.add_sized(
					vec2(38.0, 14.0),
					egui::Label::new(faint(c.role.name())).selectable(false),
				);
				let w = (ui.available_width() - 40.0).max(30.0);
				let (rect, _) = ui.allocate_exact_size(vec2(w, 8.0), Sense::hover());
				let p = ui.painter_at(rect);
				p.rect_filled(rect, CornerRadius::same(2), theme::METER_BG);
				if !silent {
					let pos = |db: f32| {
						rect.left() + ((db - BAR_FLOOR) / -BAR_FLOOR).clamp(0.0, 1.0) * rect.width()
					};
					let x = pos(s.lufs_integrated);
					let level = if s.peak_db > -0.5 {
						theme::METER_HIGH
					} else if s.peak_db > -6.0 {
						theme::METER_MID
					} else {
						theme::METER_LOW
					};
					p.rect_filled(
						Rect::from_x_y_ranges(rect.left()..=x, rect.y_range()),
						CornerRadius::same(2),
						level.gamma_multiply(0.85),
					);
					p.vline(
						pos(s.peak_db),
						rect.y_range(),
						Stroke::new(1.5, theme::TEXT),
					);
				}
				ui.label(
					RichText::new(if silent {
						"silent".to_owned()
					} else {
						format!("{:.1}", s.lufs_integrated)
					})
					.font(theme::mono(10.0))
					.color(theme::TEXT_DIM),
				);
			});
		}
		ui.label(faint("bar: loudness (LUFS) · tick: peak"));
	});
}

fn masking(ui: &mut Ui, r: &MixAnalysis) {
	if r.masking.is_empty() {
		return;
	}
	section(ui, "MASKING");
	card(ui, theme::BORDER, |ui| {
		for p in r.masking.iter().take(5) {
			let (a, b, cut) = (
				&r.channels[p.a].name,
				&r.channels[p.b].name,
				&r.channels[p.cut].name,
			);
			ui.horizontal(|ui| {
				ui.label(
					RichText::new(format!("{a} ↔ {b}"))
						.font(theme::small())
						.color(theme::TEXT),
				);
				ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
					ui.label(faint(format!("cut {cut}")));
					ui.label(
						RichText::new(format!("{} · {:.0}%", hz(p.band_hz), p.overlap * 100.0))
							.font(theme::mono(10.0))
							.color(theme::WARNING),
					);
				});
			});
		}
	});
}

fn severity_color(s: Severity) -> Color32 {
	match s {
		Severity::High => theme::ERROR,
		Severity::Medium => theme::WARNING,
		Severity::Low => theme::TEXT_DIM,
	}
}

fn issues(ui: &mut Ui, state: &mut AppState, r: &MixAnalysis) {
	let fixable = r
		.issues
		.iter()
		.filter(|i| !i.moves.is_empty() && !state.mix.fixed.contains(&i.id))
		.count();
	ui.add_space(6.0);
	ui.horizontal(|ui| {
		ui.label(
			RichText::new(format!("ISSUES ({})", r.issues.len()))
				.font(theme::small())
				.strong()
				.color(theme::TEXT_DIM),
		);
		ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
			if ui
				.add_enabled(
					fixable > 1 && !state.mix.busy() && !state.ai.busy(),
					egui::Button::new(RichText::new("Fix all").font(theme::small())),
				)
				.on_hover_text("Apply every fix as one undo step")
				.clicked()
			{
				state.mix_fix_all();
			}
		});
	});
	if r.issues.is_empty() {
		card(ui, theme::BORDER, |ui| {
			ui.label(
				RichText::new("✔ No issues: the mix meets its targets")
					.font(theme::small())
					.color(theme::PLAYING),
			);
		});
		return;
	}
	let names = |t: Option<cadenza_core::TrackId>| r.name_of(t);
	for issue in &r.issues {
		issue_row(ui, state, issue, &names);
	}
}

fn issue_row(
	ui: &mut Ui,
	state: &mut AppState,
	issue: &Issue,
	names: &dyn Fn(Option<cadenza_core::TrackId>) -> String,
) {
	let fixed = state.mix.fixed.contains(&issue.id);
	let color = severity_color(issue.severity);
	Frame::new()
		.fill(theme::BG_RAISED)
		.stroke(Stroke::new(1.0, theme::BORDER))
		.corner_radius(theme::RADIUS)
		.inner_margin(Margin::symmetric(6, 4))
		.show(ui, |ui| {
			ui.set_width(ui.available_width());
			ui.horizontal(|ui| {
				let (dot, _) = ui.allocate_exact_size(vec2(8.0, 14.0), Sense::hover());
				ui.painter().circle_filled(dot.center(), 3.5, color);
				let action_w = 44.0;
				let text_w = (ui.available_width() - action_w).max(80.0);
				ui.allocate_ui_with_layout(
					vec2(text_w, 14.0),
					Layout::top_down(Align::Min),
					|ui| {
						ui.add(
							egui::Label::new(
								RichText::new(capitalize(&issue.message))
									.font(theme::small())
									.color(if fixed {
										theme::TEXT_FAINT
									} else {
										theme::TEXT
									}),
							)
							.wrap()
							.selectable(false),
						);
						if issue.kind == IssueKind::EngineWarning {
							ui.label(faint("render warning"));
						}
					},
				);
				ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
					if fixed {
						ui.label(
							RichText::new("✔")
								.font(theme::small())
								.color(theme::PLAYING),
						)
						.on_hover_text("Fixed (one undo step)");
					} else if !issue.moves.is_empty() {
						let moves: Vec<String> =
							issue.moves.iter().map(|m| m.describe(names)).collect();
						if ui
							.add_enabled(
								!state.mix.busy() && !state.ai.busy(),
								egui::Button::new(RichText::new("Fix").font(theme::small())),
							)
							.on_hover_text(moves.join("\n"))
							.clicked()
						{
							state.mix_fix(&issue.id);
						}
					}
				});
			});
		});
}

fn capitalize(s: &str) -> String {
	let mut c = s.chars();
	match c.next() {
		Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
		None => String::new(),
	}
}
