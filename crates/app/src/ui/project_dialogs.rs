//! Project-level dialogs: export (mixdown/stems with progress), autosave recovery,
//! "Locate missing files" and the collect prompt after "Save As".

use crate::i18n::{tr, trf, trn, trn_with};
use egui::{Align, Id, Layout, RichText, Ui, vec2};

use crate::jobs::FileAction;
use crate::state::AppState;
use crate::state::export::{
	FormatChoice, NormalizeChoice, RangeKind, SAMPLE_RATES, StemMode, human_bytes,
};
use crate::state::safety::{RecoveryReason, age};
use crate::theme;

use theme::{close_glyph, dialog_header};

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
	let title = state.export.title.clone();
	let modal = egui::Modal::new(Id::new("export-dialog"))
		.frame(theme::dialog_frame())
		.show(ctx, |ui| {
			ui.set_width(900.0);
			if let Some(running) = &state.export.running {
				let p = running.progress();
				let cancelling = running.cancelling();
				dialog_header(
					ui,
					tr("Exporting"),
					&format!("{title} · {}", p.stage),
					|_| {},
				);
				ui.add(
					egui::ProgressBar::new(p.fraction)
						.show_percentage()
						.desired_width(ui.available_width()),
				);
				ui.add_space(10.0);
				ui.separator();
				ui.add_space(8.0);
				ui.horizontal(|ui| {
					hint(
						ui,
						tr("Closing this dialog does not stop the export; reopen it from here."),
					);
					ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
						if theme::danger_button(
							ui,
							if cancelling {
								tr("Cancelling…")
							} else {
								tr("Cancel export")
							},
						)
						.clicked() && !cancelling
						{
							running.cancel();
						}
					});
				});
				return;
			}
			if let Some(result) = state.export.result.clone() {
				dialog_header(ui, tr("Export result"), &title, |ui| {
					if close_glyph(ui) {
						state.export.open = false;
					}
				});
				export_result(ui, state, result);
				ui.add_space(10.0);
				ui.separator();
				ui.add_space(8.0);
				ui.horizontal(|ui| {
					ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
						if theme::primary_button(ui, tr("Done")).clicked() {
							state.export.open = false;
						}
						if ui.button(tr("Export again…")).clicked() {
							state.export.result = None;
						}
					});
				});
				return;
			}
			dialog_header(ui, tr("Export"), &title, |ui| {
				if close_glyph(ui) {
					state.export.open = false;
				}
			});
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
	ui.add_space(2.0);
	match result {
		Ok(report) => {
			ui.horizontal(|ui| {
				ui.label(RichText::new("✓").color(theme::PLAYING));
				ui.label(
					RichText::new(trn_with(
						"Exported {n} file · {size}",
						"Exported {n} files · {size}",
						report.files.len(),
						&[("size", &human_bytes(report.bytes()))],
					))
					.font(theme::semibold(13.0)),
				);
			});
			if !report.notes.is_empty() {
				ui.add_space(4.0);
				egui::Frame::new()
					.fill(theme::WARNING_SOFT)
					.corner_radius(theme::RADIUS_MD)
					.inner_margin(egui::Margin::symmetric(10, 6))
					.show(ui, |ui| {
						ui.horizontal_wrapped(|ui| {
							ui.label(RichText::new("⚠").color(theme::WARNING));
							for (i, n) in report.notes.iter().enumerate() {
								if i > 0 {
									ui.label(RichText::new(";").color(theme::TEXT_DIM));
								}
								ui.label(RichText::new(n).font(theme::small()));
							}
						});
					});
			}
			ui.add_space(6.0);
			egui::Grid::new("export-result")
				.num_columns(6)
				.spacing(vec2(14.0, 3.0))
				.show(ui, |ui| {
					let head = |ui: &mut Ui, t: &str| {
						ui.label(
							RichText::new(t)
								.font(theme::small())
								.color(theme::TEXT_FAINT),
						);
					};
					head(ui, tr("File"));
					head(ui, tr("LUFS-I"));
					head(ui, tr("True peak"));
					head(ui, tr("Gain"));
					head(ui, tr("Size"));
					head(ui, "");
					ui.end_row();
					for (i, f) in report.files.iter().enumerate().take(16) {
						let name = f
							.path
							.file_name()
							.map(|n| n.to_string_lossy().into_owned())
							.unwrap_or_default();
						let mixdown = i == 0;
						if mixdown {
							ui.label(
								RichText::new(name)
									.font(theme::mono(11.0))
									.color(theme::TEXT),
							);
						} else {
							ui.label(RichText::new(name).font(theme::mono(11.0)));
						}
						let mono = |v: String| RichText::new(v).font(theme::mono(11.0));
						ui.label(mono(format!("{:.1}", f.stats.lufs_integrated)));
						let peak_warn = f.stats.true_peak_db > -0.1;
						ui.label(mono(format!("{:.1} dBTP", f.stats.true_peak_db)).color(
							if peak_warn {
								theme::WARNING
							} else {
								theme::TEXT_DIM
							},
						));
						ui.label(mono(format!("{:+.1} dB", f.gain_db)).color(theme::TEXT_DIM));
						ui.label(mono(human_bytes(f.bytes)).color(theme::TEXT_FAINT));
						ui.label("");
						ui.end_row();
					}
				});
			if report.files.len() > 16 {
				hint(
					ui,
					&trf(
						"… and {count} more",
						&[("count", &(report.files.len() - 16))],
					),
				);
			}
			ui.add_space(6.0);
			if let Some(dir) = report.files.first().and_then(|f| f.path.parent()) {
				let dir = dir.to_path_buf();
				if ui.button(tr("Open folder")).clicked()
					&& let Err(e) = crate::state::export::open_folder(&dir)
				{
					state.notices.error(trf(
						"Cannot open {dir}: {e}",
						&[("dir", &dir.display()), ("e", &e)],
					));
				}
			}
		}
		Err(e) => {
			ui.horizontal(|ui| {
				ui.label(RichText::new("✖").color(theme::ERROR));
				ui.label(RichText::new(e).color(theme::ERROR));
			});
		}
	}
	ui.add_space(4.0);
}

fn export_settings(ui: &mut Ui, state: &mut AppState) {
	let project = state.project().clone();
	let ranges = state.export_ranges();
	let planned = state.export.planned_files(&project);
	// Header and footer stay on screen: the body scrolls in a short window. A
	// painted divider instead of `ui.separator()`, which would grow to the
	// height the scroll area offers and stretch the dialog off-screen.
	let max_body = (ui.ctx().content_rect().height() - 190.0).max(160.0);
	egui::ScrollArea::vertical()
		.id_salt("export-settings-body")
		.max_height(max_body)
		.auto_shrink([false, true])
		.show(ui, |ui| {
			ui.horizontal_top(|ui| {
				let left = ui
					.vertical(|ui| {
						ui.set_width(ui.available_width() - 260.0);
						export_fields(ui, state, &project, ranges);
					})
					.response
					.rect;
				ui.add_space(4.0);
				let (divider, _) =
					ui.allocate_exact_size(egui::vec2(1.0, left.height()), egui::Sense::hover());
				ui.painter().vline(
					divider.center().x,
					divider.y_range(),
					egui::Stroke::new(1.0, theme::BORDER),
				);
				ui.add_space(4.0);
				ui.vertical(|ui| {
					ui.set_width(ui.available_width());
					will_write_panel(ui, &planned);
				});
			});
		});
	ui.add_space(10.0);
	ui.separator();
	ui.add_space(8.0);
	let ready = state.export.spec(&project, ranges);
	let n = planned.len();
	let label = if n == 1 {
		tr("Export 1 file").to_owned()
	} else {
		trf("Export {n} files", &[("n", &n)])
	};
	let mut start = false;
	ui.horizontal(|ui| {
		if let Err(e) = &ready {
			ui.label(RichText::new(e).font(theme::small()).color(theme::WARNING));
		} else {
			hint(
				ui,
				tr("Renders offline, faster than real time. You can keep working."),
			);
		}
		ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
			if ready.is_err() {
				ui.disable();
			}
			let r = theme::primary_button(ui, label);
			if r.clicked() {
				start = true;
			}
		});
	});
	if start {
		state.start_export();
	}
}

fn will_write_panel(ui: &mut Ui, planned: &[String]) {
	ui.horizontal(|ui| {
		theme::section_label(ui, tr("Will write"));
		ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
			hint(ui, &trn("{n} file", "{n} files", planned.len()));
		});
	});
	ui.add_space(6.0);
	egui::ScrollArea::vertical()
		.id_salt("export-will-write")
		.max_height(260.0)
		.show(ui, |ui| {
			for (i, name) in planned.iter().enumerate() {
				let bg = if i == 0 {
					theme::ACCENT_DIM
				} else {
					egui::Color32::TRANSPARENT
				};
				egui::Frame::new()
					.fill(bg)
					.corner_radius(theme::RADIUS)
					.inner_margin(egui::Margin::symmetric(6, 3))
					.show(ui, |ui| {
						ui.label(RichText::new(name).font(theme::mono(11.0)));
					});
			}
		});
}

fn export_fields(
	ui: &mut Ui,
	state: &mut AppState,
	project: &cadenza_model::Project,
	ranges: crate::state::export::RangeContext,
) {
	let d = &mut state.export;
	let mut pick_folder = false;
	let grid = |id: &str| {
		egui::Grid::new(id)
			.num_columns(2)
			.spacing(vec2(12.0, 7.0))
			.min_col_width(90.0)
	};

	theme::section_label(ui, tr("Range"));
	ui.add_space(2.0);
	grid("export-range").show(ui, |ui| {
		ui.label("");
		let labels: Vec<&str> = RangeKind::ALL.iter().map(|k| k.label()).collect();
		let available: Vec<bool> = RangeKind::ALL
			.iter()
			.map(|k| match k {
				RangeKind::Loop => ranges.loop_range.is_some(),
				RangeKind::Selection => ranges.selection.is_some(),
				_ => true,
			})
			.collect();
		let at = RangeKind::ALL
			.iter()
			.position(|k| *k == d.settings.range)
			.unwrap_or(0);
		if let Some(i) = crate::ui::device_kit::segmented_with(
			ui,
			egui::Id::new("export-range-kind"),
			&labels,
			at,
			&available,
			&theme::medium(12.0),
		) {
			d.settings.range = RangeKind::ALL[i];
		}
		ui.end_row();
		if d.settings.range == RangeKind::Bars {
			ui.label("");
			ui.horizontal(|ui| {
				ui.label(tr("from bar"));
				ui.add(egui::DragValue::new(&mut d.settings.from_bar).range(1..=9999));
				ui.label(tr("to bar"));
				ui.add(egui::DragValue::new(&mut d.settings.to_bar).range(1..=9999));
			});
			ui.end_row();
		}
		ui.label("");
		crate::ui::widgets::checkbox(
			ui,
			&mut d.settings.tail,
			tr("Include the effect tail (reverb/delay decay)"),
		);
		ui.end_row();
	});

	ui.add_space(10.0);
	theme::section_label(ui, tr("What"));
	ui.add_space(2.0);
	grid("export-what").show(ui, |ui| {
		ui.label(tr("Mixdown"));
		crate::ui::widgets::checkbox(
			ui,
			&mut d.settings.mixdown,
			tr("Stereo mix through the master"),
		);
		ui.end_row();
		ui.label(tr("Stems"));
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
						if crate::ui::widgets::checkbox(ui, &mut on, &t.name).changed() {
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
				crate::ui::widgets::checkbox(
					ui,
					&mut d.settings.stems_through_master,
					tr("Run stems through the master chain"),
				);
				crate::ui::widgets::checkbox(
					ui,
					&mut d.settings.stems_pre_fader,
					tr("Pre-fader (ignore volume/pan)"),
				);
			});
			ui.end_row();
		}
	});

	ui.add_space(10.0);
	theme::section_label(ui, tr("Format"));
	ui.add_space(2.0);
	grid("export-format").show(ui, |ui| {
		ui.label("");
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
		});
		ui.end_row();
		ui.label(tr("Sample rate"));
		egui::ComboBox::from_id_salt("export-rate")
			.selected_text(rate_label(d.settings.sample_rate))
			.width(160.0)
			.show_ui(ui, |ui| {
				for r in SAMPLE_RATES {
					ui.selectable_value(&mut d.settings.sample_rate, r, rate_label(r));
				}
			});
		ui.end_row();
		ui.label("");
		ui.add_enabled_ui(d.settings.format.ditherable(), |ui| {
			crate::ui::widgets::checkbox(ui, &mut d.settings.dither, tr("Dither"));
			if !d.settings.format.ditherable() {
				hint(ui, tr("on automatically for 16-bit"));
			}
		});
		ui.end_row();
	});

	ui.add_space(10.0);
	theme::section_label(ui, tr("Loudness"));
	ui.add_space(2.0);
	grid("export-loudness").show(ui, |ui| {
		ui.label("");
		ui.horizontal(|ui| {
			const CHOICES: [NormalizeChoice; 3] = [
				NormalizeChoice::Off,
				NormalizeChoice::Peak,
				NormalizeChoice::Loudness,
			];
			let at = CHOICES
				.iter()
				.position(|c| *c == d.settings.normalize)
				.unwrap_or(0);
			if let Some(i) = crate::ui::device_kit::segmented_with(
				ui,
				egui::Id::new("export-normalize"),
				&[tr("Off"), tr("Peak"), tr("Loudness")],
				at,
				&[],
				&theme::medium(12.0),
			) {
				d.settings.normalize = CHOICES[i];
			}
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
							.suffix(tr(" LUFS")),
					);
					hint(ui, tr("Spotify, YouTube, Apple Music"));
				}
			}
		});
		ui.end_row();
		if d.settings.normalize == NormalizeChoice::Loudness {
			ui.label(tr("True-peak ceiling"));
			ui.add(
				egui::DragValue::new(&mut d.settings.true_peak_db)
					.range(-20.0..=0.0)
					.speed(0.1)
					.suffix(" dBTP"),
			);
			ui.end_row();
		}
		if d.settings.normalize != NormalizeChoice::Off && d.settings.stems != StemMode::Off {
			ui.label("");
			crate::ui::widgets::checkbox(
				ui,
				&mut d.settings.normalize_stems_individually,
				tr("Normalize each stem on its own"),
			);
			ui.end_row();
		}
	});

	ui.add_space(10.0);
	theme::section_label(ui, tr("Metadata"));
	ui.add_space(2.0);
	grid("export-metadata").show(ui, |ui| {
		// Two rows that share the column's width, so nothing is cut at 1024 px.
		ui.label("");
		let w = (ui.available_width() - 8.0).max(160.0);
		ui.vertical(|ui| {
			ui.spacing_mut().item_spacing = vec2(8.0, 6.0);
			ui.horizontal(|ui| {
				ui.add(
					egui::TextEdit::singleline(&mut d.title)
						.desired_width(w * 0.5)
						.hint_text(tr("Title")),
				);
				ui.add(
					egui::TextEdit::singleline(&mut d.settings.artist)
						.desired_width(w * 0.5 - 8.0)
						.hint_text(tr("Artist")),
				);
			});
			ui.horizontal(|ui| {
				ui.add(
					egui::TextEdit::singleline(&mut d.settings.album)
						.desired_width(w * 0.5)
						.hint_text(tr("Album")),
				);
				ui.add(
					egui::TextEdit::singleline(&mut d.settings.genre)
						.desired_width(w * 0.3 - 8.0)
						.hint_text(tr("Genre")),
				);
				ui.add(
					egui::TextEdit::singleline(&mut d.settings.date)
						.desired_width(w * 0.2 - 8.0)
						.hint_text(tr("Year")),
				);
			});
		});
		ui.end_row();
	});

	ui.add_space(10.0);
	theme::section_label(ui, tr("Save to"));
	ui.add_space(2.0);
	grid("export-save").show(ui, |ui| {
		ui.label("");
		ui.horizontal(|ui| {
			let folder = d
				.folder
				.as_ref()
				.map(|f| f.display().to_string())
				.unwrap_or_else(|| tr("(choose)").into());
			ui.add(
				egui::Label::new(
					RichText::new(folder)
						.font(theme::mono(11.0))
						.color(theme::TEXT_DIM),
				)
				.truncate(),
			);
			if ui.button(tr("Choose…")).clicked() {
				pick_folder = true;
			}
		});
		ui.end_row();
		ui.label(tr("File names"));
		ui.horizontal(|ui| {
			ui.add(
				egui::TextEdit::singleline(&mut d.name)
					.desired_width(220.0)
					.font(theme::mono(11.5)),
			);
			hint(ui, tr("If a file exists: add a number"));
		});
		ui.end_row();
	});

	if pick_folder {
		state.file_dialog(FileAction::ExportFolder);
	}
}

fn rate_label(rate: u32) -> String {
	if rate == 0 {
		tr("Engine rate").into()
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
		.frame(theme::dialog_frame())
		.show(ctx, |ui| {
			ui.set_width(440.0);
			dialog_header(ui, tr("Recover unsaved work?"), "", |_| {});
			let what = r
				.project
				.as_deref()
				.map(|p| format!("'{}'", cadenza_model::files::project_stem(p)))
				.unwrap_or_else(|| tr("an unsaved project").into());
			let why = match r.reason {
				RecoveryReason::Crashed => tr("Cadenza did not close properly while editing"),
				RecoveryReason::Newer => tr("There are newer autosaved changes to"),
			};
			ui.label(trf(
				"{why} {what}. The last autosave is from {age}.",
				&[
					("why", &why),
					("what", &what),
					("age", &age(r.modified, std::time::SystemTime::now())),
				],
			));
			ui.add_space(10.0);
			ui.horizontal(|ui| {
				if theme::primary_button(ui, tr("Recover")).clicked() {
					state.recover();
				}
				let keep = if r.project.is_some() {
					tr("Keep the saved version")
				} else {
					tr("Discard")
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
		.frame(theme::dialog_frame())
		.show(ctx, |ui| {
			ui.set_width(560.0);
			dialog_header(ui, tr("Locate missing files"), "", |ui| {
				if close_glyph(ui) {
					state.relink = None;
				}
			});
			if let Some((folder, n)) = &dialog.last {
				hint(
					ui,
					&trf(
						"Found {n} in {folder}",
						&[("n", &n), ("folder", &folder.display())],
					),
				);
			}
			if dialog.missing.is_empty() {
				ui.label(RichText::new(tr("✔ Every file is in place.")).color(theme::PLAYING));
				return;
			}
			ui.label(trn(
				"{n} file could not be found. Pick a folder to search (subfolders included): \
				 every file found by its original name is relinked at once (one undo step).",
				"{n} files could not be found. Pick a folder to search (subfolders included): \
				 every file found by its original name is relinked at once (one undo step).",
				dialog.missing.len(),
			));
			ui.add_space(6.0);
			egui::ScrollArea::vertical()
				.max_height(220.0)
				.show(ui, |ui| {
					for m in &dialog.missing {
						ui.horizontal(|ui| {
							let kind = if m.is_sample() {
								tr("sample")
							} else {
								tr("audio")
							};
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
					.add_enabled_ui(!searching, |ui| {
						theme::primary_button(ui, tr("Search folder…"))
					})
					.inner
					.clicked()
				{
					state.file_dialog(FileAction::RelinkFolder);
				}
				if searching {
					ui.spinner();
					ui.label(tr("Searching…"));
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
		.frame(theme::dialog_frame())
		.show(ctx, |ui| {
			ui.set_width(420.0);
			dialog_header(ui, tr("Collect project files?"), "", |_| {});
			ui.label(tr(
				"This project uses audio or samples stored outside its folder. Copy them into \
				 Audio/ and Samples/ so the folder can be moved or shared on its own?",
			));
			ui.add_space(10.0);
			ui.horizontal(|ui| {
				if theme::primary_button(ui, tr("Collect files")).clicked() {
					state.collect_files();
				}
				if ui.button(tr("Not now")).clicked() {
					state.collect_prompt = false;
				}
			});
		});
}

/// Anchors of the busy overlay rows for background project-file work.
pub fn busy_rows(state: &AppState) -> Vec<String> {
	state.files_jobs.labels()
}
