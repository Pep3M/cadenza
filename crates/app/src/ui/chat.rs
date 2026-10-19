//! The AI producer panel (F10): conversation, streaming replies, compact tool rows,
//! per-request cost, Stop, quick actions and the input box; plus the AI settings dialog.
//! All logic lives in [`crate::state::ai`]; this only draws and forwards clicks.

use egui::text::LayoutJob;
use egui::{
	Align, Color32, FontFamily, FontId, Frame, Id, Key, Layout, Margin, Modifiers, RichText,
	ScrollArea, Stroke, TextFormat, Ui, vec2,
};

use crate::state::AppState;
use crate::state::ai::ChatItem;
use crate::state::ai::markdown::{self, Block, Span};
use crate::state::ai::secrets::mask;
use crate::state::ai::settings::{Effort, ProviderKind};
use crate::theme;

/// Id of the input box (tests focus it).
pub const INPUT_ID: &str = "ai-chat-input";

/// UI-only state of the panel.
#[derive(Default)]
pub struct ChatUi {
	/// Provider whose key status was last requested while the settings were open.
	key_checked: Option<ProviderKind>,
}

impl ChatUi {
	pub fn show(&mut self, ui: &mut Ui, state: &mut AppState) {
		ui.spacing_mut().item_spacing.y = 5.0;
		header(ui, state);
		ui.separator();
		if state.mix.open {
			crate::ui::mix_assistant::show(ui, state);
			self.settings_dialog(ui.ctx(), state);
			return;
		}
		let busy = state.ai.busy();
		// Bottom-up: input, chips, status; the conversation takes the rest.
		egui::Panel::bottom("ai-chat-bottom")
			.frame(Frame::new().inner_margin(Margin::symmetric(0, 4)))
			.show_separator_line(false)
			.show(ui, |ui| {
				if busy {
					running_bar(ui, state);
				} else {
					chips(ui, state);
				}
				input(ui, state);
				ui.horizontal(|ui| {
					ui.label(
						RichText::new(format!(
							"{} · ${:.2}/request · session ${:.4}",
							state.ai.settings.current_model(),
							state.ai.settings.max_cost_usd,
							state.ai.total_usd
						))
						.font(theme::small())
						.color(theme::TEXT_FAINT),
					);
				});
			});
		ScrollArea::vertical()
			.auto_shrink([false, false])
			.stick_to_bottom(true)
			.show(ui, |ui| {
				ui.set_width(ui.available_width());
				if state.ai.items.is_empty() {
					empty(ui);
				}
				let mut open_settings = false;
				let mut after_user = false;
				for (i, item) in state.ai.items.iter().enumerate() {
					open_settings |= item_ui(ui, i, item, after_user);
					after_user = match item {
						ChatItem::User(_) => true,
						ChatItem::Notice(_) => after_user,
						_ => false,
					};
				}
				if open_settings {
					state.ai.settings_open = true;
				}
			});
		self.settings_dialog(ui.ctx(), state);
	}

	/// The AI settings dialog (provider, model, effort, budget, key).
	pub fn settings_dialog(&mut self, ctx: &egui::Context, state: &mut AppState) {
		if !state.ai.settings_open {
			self.key_checked = None;
			return;
		}
		if self.key_checked != Some(state.ai.settings.provider) {
			self.key_checked = Some(state.ai.settings.provider);
			let session = state.session.clone();
			state.ai.check_key(&session);
		}
		let modal = egui::Modal::new(Id::new("ai-settings"))
			.frame(
				Frame::new()
					.fill(theme::BG_RAISED)
					.stroke(Stroke::new(1.0, theme::BORDER_STRONG))
					.corner_radius(6)
					.inner_margin(Margin::same(16)),
			)
			.show(ctx, |ui| {
				ui.set_width(460.0);
				ui.horizontal(|ui| {
					ui.heading("AI settings");
					ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
						if ui.button("Close").clicked() {
							state.ai.settings_open = false;
						}
					});
				});
				ui.add_space(6.0);
				let s = &mut state.ai.settings;
				egui::Grid::new("ai-settings-grid")
					.num_columns(2)
					.spacing(vec2(12.0, 8.0))
					.show(ui, |ui| {
						ui.label("Provider");
						egui::ComboBox::from_id_salt("ai-provider")
							.selected_text(s.provider.name())
							.width(220.0)
							.show_ui(ui, |ui| {
								for p in ProviderKind::ALL {
									ui.selectable_value(&mut s.provider, p, p.name());
								}
							});
						ui.end_row();
						if s.provider == ProviderKind::OpenAiCompatible {
							ui.label("Base URL");
							ui.add(
								egui::TextEdit::singleline(&mut s.base_url)
									.desired_width(280.0)
									.hint_text("http://localhost:11434/v1"),
							);
							ui.end_row();
						}
						ui.label("Model");
						ui.vertical(|ui| {
							let presets = s.presets();
							let model = match s.provider {
								ProviderKind::OpenRouter => &mut s.model,
								ProviderKind::OpenAiCompatible => &mut s.compatible_model,
							};
							ui.add(egui::TextEdit::singleline(model).desired_width(280.0));
							if !presets.is_empty() {
								egui::ComboBox::from_id_salt("ai-presets")
									.selected_text("Presets…")
									.width(280.0)
									.show_ui(ui, |ui| {
										for (id, note) in presets {
											let r = ui.selectable_label(
												model == id,
												format!("{id}\n{note}"),
											);
											if r.clicked() {
												*model = (*id).to_owned();
											}
										}
									});
							}
						});
						ui.end_row();
						ui.label("Reasoning");
						egui::ComboBox::from_id_salt("ai-effort")
							.selected_text(s.reasoning.name())
							.show_ui(ui, |ui| {
								for e in Effort::ALL {
									ui.selectable_value(&mut s.reasoning, e, e.name());
								}
							});
						ui.end_row();
						ui.label("Budget per request");
						ui.add(
							egui::DragValue::new(&mut s.max_cost_usd)
								.range(0.01..=20.0)
								.speed(0.01)
								.prefix("$")
								.max_decimals(2),
						);
						ui.end_row();
						if s.provider == ProviderKind::OpenAiCompatible {
							ui.label("Price estimate");
							ui.horizontal(|ui| {
								let mut custom = s.custom_price.is_some();
								if ui.checkbox(&mut custom, "custom").changed() {
									s.custom_price = custom.then_some((0.0, 0.0));
								}
								if let Some((input, output)) = &mut s.custom_price {
									ui.add(
										egui::DragValue::new(input)
											.range(0.0..=100.0)
											.speed(0.01)
											.prefix("in $"),
									);
									ui.add(
										egui::DragValue::new(output)
											.range(0.0..=200.0)
											.speed(0.01)
											.prefix("out $"),
									);
									ui.label(
										RichText::new("/MTok")
											.font(theme::small())
											.color(theme::TEXT_FAINT),
									);
								}
							});
							ui.end_row();
						}
						ui.label("Plugins");
						ui.checkbox(&mut s.allow_plugins, "let the AI use installed CLAP/VST3");
						ui.end_row();
					});
				ui.add_space(8.0);
				ui.separator();
				key_section(ui, state);
			});
		if modal.should_close() {
			state.ai.settings_open = false;
		}
	}
}

fn key_section(ui: &mut Ui, state: &mut AppState) {
	let provider = state.ai.settings.provider;
	ui.label(
		RichText::new("API KEY")
			.font(theme::small())
			.color(theme::ACCENT),
	);
	let status = state
		.ai
		.key_source
		.as_ref()
		.map(|s| s.describe())
		.unwrap_or_else(|| "checking…".into());
	ui.label(
		RichText::new(format!("{} key: {status}", provider.name()))
			.font(theme::small())
			.color(theme::TEXT_DIM),
	);
	ui.horizontal(|ui| {
		ui.add(
			egui::TextEdit::singleline(&mut state.ai.key_input)
				.password(true)
				.desired_width(260.0)
				.hint_text("paste a key to store it"),
		);
		let has_input = !state.ai.key_input.trim().is_empty();
		let session = state.session.clone();
		if ui
			.add_enabled(has_input, egui::Button::new("Save"))
			.clicked()
		{
			let key = std::mem::take(&mut state.ai.key_input);
			state.ai.store_key(Some(key), &session);
		}
		if ui.button("Remove stored").clicked() {
			state.ai.store_key(None, &session);
		}
	});
	if !state.ai.key_input.is_empty() {
		ui.label(
			RichText::new(format!("new key {}", mask(&state.ai.key_input)))
				.font(theme::small())
				.color(theme::TEXT_FAINT),
		);
	}
	if let Some(message) = &state.ai.key_message {
		let (text, color) = match message {
			Ok(t) => (t.as_str(), theme::PLAYING),
			Err(e) => (e.as_str(), theme::ERROR),
		};
		ui.label(RichText::new(text).font(theme::small()).color(color));
	}
	ui.label(
		RichText::new(format!(
			"Order: {} in the environment, then a .env file, then the stored key (system \
			 keyring; a 0600 file when no keyring runs).{}",
			provider.key_env(),
			if provider.needs_key() {
				""
			} else {
				" Local servers usually need no key."
			}
		))
		.font(theme::small())
		.color(theme::TEXT_FAINT),
	);
}

fn header(ui: &mut Ui, state: &mut AppState) {
	ui.horizontal(|ui| {
		ui.spacing_mut().item_spacing.x = 2.0;
		for (mix, label, hint) in [
			(false, "Producer", "Chat with the AI producer"),
			(
				true,
				"Mix",
				"Mix assistant: analysis, fixes and the AI mix pass",
			),
		] {
			let on = state.mix.open == mix;
			let button =
				egui::Button::new(RichText::new(label).font(theme::medium(12.5)).color(if on {
					theme::TEXT
				} else {
					theme::TEXT_DIM
				}))
				.fill(if on {
					theme::BG_HOVER
				} else {
					egui::Color32::TRANSPARENT
				})
				.min_size(vec2(0.0, 24.0));
			let resp = ui.add(button).on_hover_text(hint);
			resp.widget_info(|| {
				egui::WidgetInfo::selected(
					egui::WidgetType::SelectableLabel,
					true,
					on,
					if mix { "MIX" } else { "AI PRODUCER" },
				)
			});
			if resp.clicked() {
				state.mix.open = mix;
			}
		}
		ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
			ui.menu_button("☰", |ui| {
				ui.set_min_width(260.0);
				if ui
					.add_enabled(!state.ai.busy(), egui::Button::new("New conversation"))
					.clicked()
				{
					state.ai.clear();
					ui.close();
				}
				if ui.button("AI settings…").clicked() {
					state.ai.settings_open = true;
					ui.close();
				}
				ui.separator();
				let path = state.ai.transcript_path();
				ui.label(
					RichText::new("Transcript (JSONL, no secrets)")
						.font(theme::small())
						.color(theme::TEXT_DIM),
				);
				if ui
					.button(RichText::new(path.display().to_string()).font(theme::mono(10.0)))
					.on_hover_text("Copy the path")
					.clicked()
				{
					ui.ctx().copy_text(path.display().to_string());
					ui.close();
				}
			});
			if ui.button("⚙").on_hover_text("AI settings").clicked() {
				state.ai.settings_open = true;
			}
			ui.label(
				RichText::new(format!("session ${:.4}", state.ai.total_usd))
					.font(theme::mono(10.5))
					.color(theme::TEXT_FAINT),
			)
			.on_hover_text("What this conversation has cost so far");
		});
	});
}

fn empty(ui: &mut Ui) {
	ui.add_space(12.0);
	ui.label(
		RichText::new(
			"Ask for changes to the open project: \"add a bridge after the second chorus\", \
			 \"make the drums punchier\", \"write a bassline for the selected clip\", \"mix \
			 this\". Each request becomes one undo step.",
		)
		.color(theme::TEXT_DIM),
	);
}

fn running_bar(ui: &mut Ui, state: &mut AppState) {
	let Some(running) = state.ai.running.clone() else {
		return;
	};
	Frame::new()
		.fill(theme::ACCENT_DIM)
		.stroke(Stroke::new(1.0, theme::ACCENT_BORDER))
		.corner_radius(theme::RADIUS_LG)
		.inner_margin(Margin::symmetric(10, 6))
		.show(ui, |ui| {
			ui.horizontal(|ui| {
				ui.spinner();
				let text = if running.stopping {
					"Stopping…".to_owned()
				} else {
					format!(
						"Producer working · {:.0} s · ${:.4}{} so far",
						running.started.elapsed().as_secs_f32(),
						running.cost_usd,
						if running.estimated { " est." } else { "" }
					)
				};
				ui.label(
					RichText::new(text)
						.font(theme::small())
						.color(theme::ACCENT_TEXT),
				);
				ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
					let stop = egui::Button::new(RichText::new("■ Stop").color(theme::ERROR))
						.fill(theme::ERROR_SOFT)
						.stroke(Stroke::new(1.0, theme::ERROR.gamma_multiply(0.6)));
					if ui
						.add_enabled(!running.stopping, stop)
						.on_hover_text("Stop at the next step; edits so far stay as one undo step")
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

fn chips(ui: &mut Ui, state: &mut AppState) {
	let actions = state.ai_quick_actions();
	ui.horizontal_wrapped(|ui| {
		ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
		for (label, request) in actions {
			let chip = egui::Button::new(
				RichText::new(label)
					.font(theme::small())
					.color(theme::ACCENT_TEXT),
			)
			.fill(theme::BG_PANEL)
			.stroke(Stroke::new(1.0, theme::ACCENT_BORDER))
			.corner_radius(99);
			if ui.add(chip).on_hover_text(&request).clicked() {
				state.send_ai(&request);
			}
		}
	});
}

fn input(ui: &mut Ui, state: &mut AppState) {
	let id = Id::new(INPUT_ID);
	let focused = ui.memory(|m| m.has_focus(id));
	// Enter sends, Shift+Enter is a new line (the text box keeps that one).
	let send =
		focused && !state.ai.busy() && ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter));
	let busy = state.ai.busy();
	ui.horizontal(|ui| {
		let edit = egui::TextEdit::multiline(&mut state.ai.input)
			.id(id)
			.desired_rows(3)
			.desired_width(ui.available_width() - 54.0)
			.hint_text(if busy {
				"The AI is working… (Stop to take over)"
			} else {
				"Ask the producer…  Enter sends, Shift+Enter new line"
			});
		ui.add(edit);
		let can_send = !busy && !state.ai.input.trim().is_empty();
		let clicked = ui
			.add_enabled(
				can_send,
				egui::Button::new(RichText::new("Send").font(theme::medium(12.0)).color(
					if can_send {
						theme::TEXT_ON_ACCENT
					} else {
						theme::TEXT_FAINT
					},
				))
				.fill(if can_send {
					theme::ACCENT
				} else {
					theme::BG_WIDGET
				})
				.min_size(vec2(44.0, 36.0)),
			)
			.on_hover_text("Send (Enter)")
			.clicked();
		if (send || clicked) && can_send {
			let text = std::mem::take(&mut state.ai.input);
			state.send_ai(&text);
		}
	});
}

/// Draws one chat row; returns true when its "AI settings" button was clicked.
/// The producer's avatar and name over its first reply to a request.
fn producer_header(ui: &mut Ui) {
	ui.add_space(4.0);
	ui.horizontal(|ui| {
		let (r, _) = ui.allocate_exact_size(vec2(18.0, 18.0), egui::Sense::hover());
		ui.painter().circle_filled(r.center(), 9.0, theme::ACCENT);
		ui.painter().text(
			r.center(),
			egui::Align2::CENTER_CENTER,
			"C",
			theme::semibold(10.5),
			theme::TEXT_ON_ACCENT,
		);
		ui.label(
			RichText::new("Producer")
				.font(theme::semibold(12.0))
				.color(theme::TEXT),
		);
	});
}

fn item_ui(ui: &mut Ui, index: usize, item: &ChatItem, after_user: bool) -> bool {
	let mut open_settings = false;
	match item {
		ChatItem::User(text) => {
			ui.add_space(6.0);
			ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
				Frame::new()
					.fill(theme::ACCENT_DIM)
					.stroke(Stroke::new(1.0, theme::ACCENT_BORDER))
					.corner_radius(theme::RADIUS_LG)
					.inner_margin(Margin::symmetric(10, 7))
					.show(ui, |ui| {
						ui.set_max_width(ui.available_width() * 0.85);
						ui.add(egui::Label::new(RichText::new(text).color(theme::TEXT)).wrap());
					});
			});
		}
		ChatItem::Assistant { text, streaming } => {
			if after_user {
				producer_header(ui);
			}
			markdown_ui(ui, text);
			if *streaming {
				ui.label(RichText::new("▍").color(theme::ACCENT));
			}
		}
		ChatItem::Thinking { text, streaming } => {
			if after_user {
				producer_header(ui);
			}
			egui::CollapsingHeader::new(
				RichText::new(if *streaming {
					"thinking…"
				} else {
					"thinking"
				})
				.font(theme::small())
				.italics()
				.color(theme::TEXT_FAINT),
			)
			.id_salt(("ai-thinking", index))
			.default_open(false)
			.show(ui, |ui| {
				ui.label(
					RichText::new(text)
						.font(theme::small())
						.color(theme::TEXT_FAINT),
				);
			});
		}
		ChatItem::Tool {
			name,
			args,
			ok,
			summary,
		} => {
			if after_user {
				producer_header(ui);
			}
			let (icon, color) = match ok {
				None => ("○", theme::ACCENT),
				Some(true) => ("✔", theme::PLAYING),
				Some(false) => ("✖", theme::ERROR),
			};
			let headline = tool_headline(name, args, *ok, summary);
			let id = ui.make_persistent_id(("ai-tool", index));
			egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, false)
				.show_header(ui, |ui| {
					ui.label(RichText::new(icon).font(theme::small()).color(color));
					ui.add(
						egui::Label::new(RichText::new(headline).font(theme::small()).color(
							match ok {
								Some(false) => theme::ERROR,
								Some(true) => theme::TEXT,
								None => theme::ACCENT_TEXT,
							},
						))
						.truncate(),
					);
				})
				.body(|ui| {
					if !args.is_empty() {
						code_block(ui, &pretty(args));
					}
					if !summary.is_empty() {
						ui.label(
							RichText::new(summary)
								.font(theme::mono(10.5))
								.color(theme::TEXT_DIM),
						);
					}
				});
		}
		ChatItem::Notice(text) => {
			ui.label(
				RichText::new(text)
					.font(theme::small())
					.italics()
					.color(theme::TEXT_FAINT),
			);
		}
		ChatItem::Error { text, settings } => {
			Frame::new()
				.fill(theme::ERROR_SOFT)
				.stroke(Stroke::new(1.0, theme::ERROR.gamma_multiply(0.6)))
				.corner_radius(theme::RADIUS_LG)
				.inner_margin(Margin::symmetric(10, 7))
				.show(ui, |ui| {
					ui.add(egui::Label::new(RichText::new(text).color(theme::TEXT)).wrap());
					if *settings && ui.button("AI settings…").clicked() {
						open_settings = true;
					}
				});
		}
		ChatItem::Footer {
			status,
			cost_usd,
			estimated,
			total_usd,
			undo_label,
		} => {
			// The result of a request: what it cost and the one undo step it became.
			let ok = status == "done";
			Frame::new()
				.fill(theme::BG_RAISED)
				.stroke(Stroke::new(
					1.0,
					if ok {
						theme::ACCENT_BORDER
					} else {
						theme::BORDER
					},
				))
				.corner_radius(theme::RADIUS_LG)
				.inner_margin(Margin::symmetric(10, 6))
				.show(ui, |ui| {
					ui.set_width(ui.available_width());
					ui.horizontal_wrapped(|ui| {
						ui.spacing_mut().item_spacing.x = 6.0;
						let (mark, color) = if ok {
							("✔", theme::PLAYING)
						} else {
							("■", theme::WARNING)
						};
						ui.label(RichText::new(mark).font(theme::small()).color(color));
						ui.label(
							RichText::new(status)
								.font(theme::semibold(11.5))
								.color(theme::TEXT),
						);
						ui.label(
							RichText::new(format!(
								"${cost_usd:.4}{} · session ${total_usd:.4}",
								if *estimated { " est." } else { "" }
							))
							.font(theme::mono(10.5))
							.color(theme::TEXT_FAINT),
						);
					});
					if let Some(label) = undo_label {
						// One label (one widget) so the whole line reads together.
						let mut job = LayoutJob::default();
						let part = |job: &mut LayoutJob, text: &str, font: egui::FontId, color| {
							job.append(
								text,
								0.0,
								egui::TextFormat {
									font_id: font,
									color,
									..Default::default()
								},
							);
						};
						part(&mut job, "Undo step: ", theme::small(), theme::TEXT_FAINT);
						part(&mut job, label, theme::small(), theme::ACCENT_TEXT);
						part(&mut job, "  Ctrl+Z", theme::mono(10.0), theme::TEXT_FAINT);
						ui.add(egui::Label::new(job).wrap());
					}
				});
			ui.add_space(6.0);
		}
	}
	open_settings
}

/// "add_track Bass" / "add_notes: error…": the tool and the gist of its arguments.
fn tool_headline(name: &str, args: &str, ok: Option<bool>, summary: &str) -> String {
	if ok == Some(false) {
		return format!("{name}: {}", summary.lines().next().unwrap_or_default());
	}
	let value: Option<serde_json::Value> = serde_json::from_str(args).ok();
	let gist = value
		.as_ref()
		.and_then(|v| {
			["name", "device", "progression", "path", "query"]
				.iter()
				.find_map(|k| v.get(*k).and_then(|x| x.as_str()))
				.map(str::to_owned)
				.or_else(|| {
					v.get("notes")
						.and_then(|n| n.as_array())
						.map(|n| format!("{} notes", n.len()))
				})
		})
		.unwrap_or_default();
	if gist.is_empty() {
		name.to_owned()
	} else {
		format!("{name} {gist}")
	}
}

fn pretty(args: &str) -> String {
	serde_json::from_str::<serde_json::Value>(args)
		.ok()
		.and_then(|v| serde_json::to_string_pretty(&v).ok())
		.unwrap_or_else(|| args.to_owned())
}

fn code_block(ui: &mut Ui, text: &str) {
	Frame::new()
		.fill(theme::BG_DEEP)
		.corner_radius(theme::RADIUS)
		.inner_margin(Margin::symmetric(6, 4))
		.show(ui, |ui| {
			ui.set_width(ui.available_width());
			ui.add(
				egui::Label::new(
					RichText::new(text)
						.font(theme::mono(10.5))
						.color(theme::TEXT),
				)
				.wrap(),
			);
		});
}

fn markdown_ui(ui: &mut Ui, text: &str) {
	for block in markdown::parse(text) {
		match block {
			Block::Heading(spans) => {
				ui.add_space(2.0);
				ui.label(job(&spans, 13.5, theme::TEXT, true));
			}
			Block::Paragraph(spans) => {
				ui.add(egui::Label::new(job(&spans, 12.5, theme::TEXT, false)).wrap());
			}
			Block::Item {
				marker,
				level,
				spans,
			} => {
				ui.horizontal_top(|ui| {
					ui.add_space(4.0 + level as f32 * 14.0);
					ui.label(RichText::new(marker).color(theme::ACCENT));
					ui.add(egui::Label::new(job(&spans, 12.5, theme::TEXT, false)).wrap());
				});
			}
			Block::Code(code) => code_block(ui, &code),
		}
	}
}

fn job(spans: &[Span], size: f32, color: Color32, heading: bool) -> LayoutJob {
	let mut job = LayoutJob::default();
	for span in spans {
		let font = if span.code {
			FontId::new(size - 1.0, FontFamily::Monospace)
		} else {
			FontId::new(size, FontFamily::Proportional)
		};
		let strong = span.bold || heading;
		let format = TextFormat {
			font_id: font,
			color: if strong {
				Color32::WHITE
			} else if span.code {
				theme::AUTOMATION
			} else {
				color
			},
			background: if span.code {
				theme::BG_DEEP
			} else {
				Color32::TRANSPARENT
			},
			italics: span.italic,
			..TextFormat::default()
		};
		job.append(&span.text, 0.0, format);
	}
	job.wrap.max_width = f32::INFINITY;
	job
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn tool_headlines_name_the_gist() {
		assert_eq!(
			tool_headline(
				"create_track",
				r#"{"name":"Bass","kind":"instrument"}"#,
				Some(true),
				""
			),
			"create_track Bass"
		);
		assert_eq!(
			tool_headline("add_notes", r#"{"notes":[{},{}]}"#, None, ""),
			"add_notes 2 notes"
		);
		assert_eq!(
			tool_headline(
				"add_notes",
				"{}",
				Some(false),
				"clip x does not exist\nmore"
			),
			"add_notes: clip x does not exist"
		);
	}
}
