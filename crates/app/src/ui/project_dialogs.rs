//! Project-level dialogs: export (mixdown/stems with progress), autosave recovery,
//! "Locate missing files" and the collect prompt after "Save As".

use egui::{Align, Frame, Id, Layout, Margin, RichText, Stroke, Ui, vec2};

use crate::jobs::FileAction;
use crate::state::AppState;
use crate::state::export::{
	FormatChoice, NormalizeChoice, RangeKind, SAMPLE_RATES, StemMode, human_bytes,
};
use crate::state::safety::{RecoveryReason, age};
use crate::theme;

fn modal_frame() -> Frame {
	Frame::new()
		.fill(theme::BG_RAISED)
		.stroke(Stroke::new(1.0, theme::BORDER_STRONG))
		.corner_radius(6)
		.inner_margin(Margin::same(16))
}

fn hint(ui: &mut Ui, text: &str) {
	ui.label(
		RichText::new(text)
			.font(theme::small())
			.color(theme::TEXT_FAINT),
	);
}

/// Every project dialog, in stacking order.
pub fn show(ctx: &egui::Context, state: &mut AppState) {
	export_dialog(ctx, state);
	relink_dialog(ctx, state);
	collect_prompt(ctx, state);
	recovery_prompt(ctx, state);
}

// --- Export ---------------------------------------------------------------------------

fn export_dialog(ctx: &egui::Context, state: &mut AppState) {
	if !state.export.open {
		return;
	}
	let modal = egui::Modal::new(Id::new("export-dialog"))
		.frame(modal_frame())
		.show(ctx, |ui| {
			ui.set_width(620.0);
			ui.horizontal(|ui| {
				ui.heading("Export");
				ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
					let running = state.export.running.is_some();
					if ui
						.add_enabled(!running, egui::Button::new("Close"))
						.clicked()
					{
						state.export.open = false;
					}
				});
			});
			if let Some(running) = &state.export.running {
				let p = running.progress();
				ui.add_space(12.0);
				ui.label(RichText::new(&p.stage).color(theme::TEXT));
				ui.add(
					egui::ProgressBar::new(p.fraction)
						.show_percentage()
						.desired_width(588.0),
				);
				ui.add_space(8.0);
				let cancelling = running.cancelling();
				if ui
					.add_enabled(
						!cancelling,
						egui::Button::new(if cancelling {
							"Cancelling…"
						} else {
							"Cancel"
						}),
					)
					.clicked()
				{
					running.cancel();
				}
				return;
			}
			if let Some(result) = &state.export.result {
				export_result(ui, state, result.clone());
				ui.separator();
			}
			export_settings(ui, state);
		});
	if modal.should_close() && state.export.running.is_none() {
		state.export.open = false;
	}
}

fn export_result(
	ui: &mut Ui,
	state: &mut AppState,
	result: Result<cadenza_engine::ExportReport, String>,
) {
	ui.add_space(6.0);
	match result {
		Ok(report) => {
			ui.label(
				RichText::new(format!(
					"✔ Exported {} file{} · {}",
					report.files.len(),
					if report.files.len() == 1 { "" } else { "s" },
					human_bytes(report.bytes())
				))
				.color(theme::PLAYING),
			);
			egui::Grid::new("export-result")
				.num_columns(5)
				.spacing(vec2(14.0, 3.0))
				.show(ui, |ui| {
					for h in ["File", "LUFS", "True peak", "Gain", "Size"] {
						ui.label(RichText::new(h).font(theme::small()).color(theme::TEXT_DIM));
					}
					ui.end_row();
					for f in report.files.iter().take(12) {
						let name = f
							.path
							.file_name()
							.map(|n| n.to_string_lossy().into_owned())
							.unwrap_or_default();
						ui.label(RichText::new(name).font(theme::small()));
						let mono = |v: String| RichText::new(v).font(theme::mono(11.0));
						ui.label(mono(format!("{:.1}", f.stats.lufs_integrated)));
						ui.label(mono(format!("{:.1} dBTP", f.stats.true_peak_db)));
						ui.label(mono(format!("{:+.1} dB", f.gain_db)));
						ui.label(mono(human_bytes(f.bytes)));
						ui.end_row();
					}
				});
			if report.files.len() > 12 {
				hint(ui, &format!("… and {} more", report.files.len() - 12));
			}
			for n in &report.notes {
				ui.label(RichText::new(n).font(theme::small()).color(theme::WARNING));
			}
			if let Some(dir) = report.files.first().and_then(|f| f.path.parent()) {
				let dir = dir.to_path_buf();
				if ui.button("Open folder").clicked()
					&& let Err(e) = crate::state::export::open_folder(&dir)
				{
					state
						.notices
						.error(format!("Cannot open {}: {e}", dir.display()));
				}
			}
		}
		Err(e) => {
			ui.label(RichText::new(format!("✖ {e}")).color(theme::ERROR));
		}
	}
	ui.add_space(4.0);
}

fn export_settings(ui: &mut Ui, state: &mut AppState) {
	let project = state.project().clone();
	let ranges = state.export_ranges();
	let d = &mut state.export;
	let mut pick_folder = false;
	egui::Grid::new("export-grid")
		.num_columns(2)
		.spacing(vec2(12.0, 7.0))
		.min_col_width(110.0)
		.show(ui, |ui| {
			ui.label("Range");
			ui.horizontal(|ui| {
				for kind in RangeKind::ALL {
					let available = match kind {
						RangeKind::Loop => ranges.loop_range.is_some(),
						RangeKind::Selection => ranges.selection.is_some(),
						_ => true,
					};
					ui.add_enabled_ui(available, |ui| {
						ui.selectable_value(&mut d.settings.range, kind, kind.label());
					});
				}
			});
			ui.end_row();
			if d.settings.range == RangeKind::Bars {
				ui.label("");
				ui.horizontal(|ui| {
					ui.label("from bar");
					ui.add(egui::DragValue::new(&mut d.settings.from_bar).range(1..=9999));
					ui.label("to bar");
					ui.add(egui::DragValue::new(&mut d.settings.to_bar).range(1..=9999));
				});
				ui.end_row();
			}
			ui.label("");
			ui.checkbox(
				&mut d.settings.tail,
				"Include the effect tail (reverb/delay decay)",
			);
			ui.end_row();

			ui.label("Mixdown");
			ui.checkbox(&mut d.settings.mixdown, "Stereo mix through the master");
			ui.end_row();
			ui.label("Stems");
			egui::ComboBox::from_id_salt("export-stems")
				.selected_text(d.settings.stems.label())
				.width(220.0)
				.show_ui(ui, |ui| {
					for m in StemMode::ALL {
						ui.selectable_value(&mut d.settings.stems, m, m.label());
					}
				});
			ui.end_row();
			if d.settings.stems == StemMode::Selected {
				ui.label("");
				egui::ScrollArea::vertical()
					.id_salt("export-tracks")
					.max_height(110.0)
					.show(ui, |ui| {
						for t in &project.tracks {
							let mut on = d.tracks.contains(&t.id);
							if ui.checkbox(&mut on, &t.name).changed() {
								if on {
									d.tracks.insert(t.id);
								} else {
									d.tracks.remove(&t.id);
								}
							}
						}
					});
				ui.end_row();
			}
			if d.settings.stems != StemMode::Off {
				ui.label("");
				ui.vertical(|ui| {
					ui.checkbox(
						&mut d.settings.stems_through_master,
						"Run stems through the master chain",
					);
					ui.checkbox(
						&mut d.settings.stems_pre_fader,
						"Pre-fader (ignore volume/pan)",
					);
				});
				ui.end_row();
			}

			ui.label("Format");
			ui.horizontal(|ui| {
				egui::ComboBox::from_id_salt("export-format")
					.selected_text(d.settings.format.label())
					.width(160.0)
					.show_ui(ui, |ui| {
						for f in FormatChoice::all() {
							ui.selectable_value(&mut d.settings.format, f, f.label());
						}
					});
				if d.settings.format == FormatChoice::Mp3 {
					ui.add(
						egui::DragValue::new(&mut d.settings.mp3_kbps)
							.range(32..=320)
							.suffix(" kbps"),
					);
				}
				if d.settings.format.ditherable() {
					ui.checkbox(&mut d.settings.dither, "Dither");
				}
			});
			ui.end_row();
			ui.label("Sample rate");
			egui::ComboBox::from_id_salt("export-rate")
				.selected_text(rate_label(d.settings.sample_rate))
				.width(160.0)
				.show_ui(ui, |ui| {
					for r in SAMPLE_RATES {
						ui.selectable_value(&mut d.settings.sample_rate, r, rate_label(r));
					}
				});
			ui.end_row();
			ui.label("Normalize");
			ui.horizontal(|ui| {
				let n = &mut d.settings.normalize;
				ui.selectable_value(n, NormalizeChoice::Off, "Off");
				ui.selectable_value(n, NormalizeChoice::Peak, "Peak");
				ui.selectable_value(n, NormalizeChoice::Loudness, "Loudness");
				match d.settings.normalize {
					NormalizeChoice::Off => {}
					NormalizeChoice::Peak => {
						ui.add(
							egui::DragValue::new(&mut d.settings.peak_db)
								.range(-60.0..=0.0)
								.speed(0.1)
								.suffix(" dBFS"),
						);
					}
					NormalizeChoice::Loudness => {
						ui.add(
							egui::DragValue::new(&mut d.settings.lufs)
								.range(-60.0..=0.0)
								.speed(0.1)
								.suffix(" LUFS"),
						);
						ui.label("ceiling");
						ui.add(
							egui::DragValue::new(&mut d.settings.true_peak_db)
								.range(-20.0..=0.0)
								.speed(0.1)
								.suffix(" dBTP"),
						);
					}
				}
			});
			ui.end_row();
			if d.settings.normalize != NormalizeChoice::Off && d.settings.stems != StemMode::Off {
				ui.label("");
				ui.checkbox(
					&mut d.settings.normalize_stems_individually,
					"Normalize each stem on its own (otherwise stems share the mix gain)",
				);
				ui.end_row();
			}

			ui.label("Title / artist");
			ui.horizontal(|ui| {
				ui.add(
					egui::TextEdit::singleline(&mut d.title)
						.desired_width(200.0)
						.hint_text("Title"),
				);
				ui.add(
					egui::TextEdit::singleline(&mut d.settings.artist)
						.desired_width(200.0)
						.hint_text("Artist"),
				);
			});
			ui.end_row();
			ui.label("Album / genre / year");
			ui.horizontal(|ui| {
				ui.add(
					egui::TextEdit::singleline(&mut d.settings.album)
						.desired_width(150.0)
						.hint_text("Album"),
				);
				ui.add(
					egui::TextEdit::singleline(&mut d.settings.genre)
						.desired_width(120.0)
						.hint_text("Genre"),
				);
				ui.add(
					egui::TextEdit::singleline(&mut d.settings.date)
						.desired_width(80.0)
						.hint_text("Year"),
				);
			});
			ui.end_row();

			ui.label("Folder");
			ui.horizontal(|ui| {
				let folder = d
					.folder
					.as_ref()
					.map(|f| f.display().to_string())
					.unwrap_or_else(|| "(choose)".into());
				ui.add(
					egui::Label::new(
						RichText::new(folder)
							.font(theme::small())
							.color(theme::TEXT_DIM),
					)
					.truncate(),
				);
				if ui.button("Choose…").clicked() {
					pick_folder = true;
				}
			});
			ui.end_row();
			ui.label("File name");
			ui.add(egui::TextEdit::singleline(&mut d.name).desired_width(300.0));
			ui.end_row();
		});
	let planned = d.planned_files(&project);
	if !planned.is_empty() {
		let more = planned.len().saturating_sub(4);
		let mut text = planned.into_iter().take(4).collect::<Vec<_>>().join(", ");
		if more > 0 {
			text.push_str(&format!(" + {more} more"));
		}
		hint(ui, &format!("Writes {text}"));
	}
	if pick_folder {
		state.file_dialog(FileAction::ExportFolder);
	}
	ui.add_space(8.0);
	ui.horizontal(|ui| {
		let ready = state.export.spec(state.project(), state.export_ranges());
		let r = ui.add_enabled(
			ready.is_ok(),
			egui::Button::new(RichText::new("Start export").strong()),
		);
		if r.clicked() {
			state.start_export();
		}
		if let Err(e) = ready {
			ui.label(RichText::new(e).font(theme::small()).color(theme::WARNING));
		}
	});
}

fn rate_label(rate: u32) -> String {
	if rate == 0 {
		"Engine rate".into()
	} else {
		format!("{:.1} kHz", rate as f32 / 1000.0)
	}
}

// --- Recovery -------------------------------------------------------------------------

fn recovery_prompt(ctx: &egui::Context, state: &mut AppState) {
	// The start screen shows it as a banner instead.
	if state.view.start_screen {
		return;
	}
	let Some(r) = state.recovery().cloned() else {
		return;
	};
	egui::Modal::new(Id::new("recovery"))
		.frame(modal_frame())
		.show(ctx, |ui| {
			ui.set_width(440.0);
			ui.heading("Recover unsaved work?");
			ui.add_space(4.0);
			let what = r
				.project
				.as_deref()
				.map(|p| format!("'{}'", cadenza_model::files::project_stem(p)))
				.unwrap_or_else(|| "an unsaved project".into());
			let why = match r.reason {
				RecoveryReason::Crashed => "Cadenza did not close properly while editing",
				RecoveryReason::Newer => "There are newer autosaved changes to",
			};
			ui.label(format!(
				"{why} {what}. The last autosave is from {}.",
				age(r.modified, std::time::SystemTime::now())
			));
			ui.add_space(10.0);
			ui.horizontal(|ui| {
				if ui.button(RichText::new("Recover").strong()).clicked() {
					state.recover();
				}
				let keep = if r.project.is_some() {
					"Keep the saved version"
				} else {
					"Discard"
				};
				if ui.button(keep).clicked() {
					state.dismiss_recovery();
				}
			});
		});
}

// --- Missing files --------------------------------------------------------------------

fn relink_dialog(ctx: &egui::Context, state: &mut AppState) {
	let Some(dialog) = state.relink.clone() else {
		return;
	};
	let modal = egui::Modal::new(Id::new("relink"))
		.frame(modal_frame())
		.show(ctx, |ui| {
			ui.set_width(560.0);
			ui.horizontal(|ui| {
				ui.heading("Locate missing files");
				ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
					if ui.button("Close").clicked() {
						state.relink = None;
					}
				});
			});
			if let Some((folder, n)) = &dialog.last {
				hint(ui, &format!("Found {n} in {}", folder.display()));
			}
			if dialog.missing.is_empty() {
				ui.label(RichText::new("✔ Every file is in place.").color(theme::PLAYING));
				return;
			}
			ui.label(format!(
				"{} file{} could not be found. Pick a folder to search (subfolders included): \
				 every file found by its original name is relinked at once (one undo step).",
				dialog.missing.len(),
				if dialog.missing.len() == 1 { "" } else { "s" }
			));
			ui.add_space(6.0);
			egui::ScrollArea::vertical()
				.max_height(220.0)
				.show(ui, |ui| {
					for m in &dialog.missing {
						ui.horizontal(|ui| {
							let kind = if m.is_sample() { "sample" } else { "audio" };
							ui.label(
								RichText::new(kind)
									.font(theme::small())
									.color(theme::TEXT_FAINT),
							);
							ui.label(RichText::new(&m.name).color(theme::TEXT));
							ui.add(
								egui::Label::new(
									RichText::new(m.path.display().to_string())
										.font(theme::small())
										.color(theme::TEXT_FAINT),
								)
								.truncate(),
							);
						});
					}
				});
			ui.add_space(8.0);
			let searching = state.files_jobs.relink.is_some();
			ui.horizontal(|ui| {
				if ui
					.add_enabled(
						!searching,
						egui::Button::new(RichText::new("Search folder…").strong()),
					)
					.clicked()
				{
					state.file_dialog(FileAction::RelinkFolder);
				}
				if searching {
					ui.spinner();
					ui.label("Searching…");
				}
			});
		});
	if modal.should_close() {
		state.relink = None;
	}
}

fn collect_prompt(ctx: &egui::Context, state: &mut AppState) {
	if !state.collect_prompt {
		return;
	}
	egui::Modal::new(Id::new("collect-prompt"))
		.frame(modal_frame())
		.show(ctx, |ui| {
			ui.set_width(420.0);
			ui.heading("Collect project files?");
			ui.add_space(4.0);
			ui.label(
				"This project uses audio or samples stored outside its folder. Copy them into \
				 Audio/ and Samples/ so the folder can be moved or shared on its own?",
			);
			ui.add_space(10.0);
			ui.horizontal(|ui| {
				if ui.button(RichText::new("Collect files").strong()).clicked() {
					state.collect_files();
				}
				if ui.button("Not now").clicked() {
					state.collect_prompt = false;
				}
			});
		});
}

/// Anchors of the busy overlay rows for background project-file work.
pub fn busy_rows(state: &AppState) -> Vec<String> {
	state.files_jobs.labels()
}
