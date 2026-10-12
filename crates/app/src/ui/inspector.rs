//! Inspector: properties of the focused device, clip, track or master, and the undo
//! history.

use cadenza_core::{ParamAddress, Tick, TrackId};
use cadenza_model::{
	ChannelPatch, ClipContent, ClipPatch, Command, DeviceKind, TrackKind, TrackPatch,
};
use egui::{CornerRadius, Grid, Id, RichText, Sense, Stroke, StrokeKind, Ui, vec2};

use crate::state::AppState;
use crate::state::edits::{TRACK_PALETTE, kind_name};
use crate::state::selection::{DeviceRef, Focus};
use crate::state::timeline::format_bbt;
use crate::theme;
use crate::ui::widgets::{self, Gesture};

/// One editable device parameter (plain values).
struct ParamRow {
	id: String,
	name: String,
	min: f32,
	max: f32,
	default: f32,
	unit: String,
}

/// Plugin parameters shown before "Show all" (plugins can expose thousands).
const MAX_PLUGIN_PARAMS: usize = 64;
/// Above this many parameters the generic editor offers a filter.
const FILTER_FROM: usize = 12;

pub fn show(ui: &mut Ui, state: &mut AppState) {
	ui.add_space(4.0);
	ui.label(
		RichText::new("INSPECTOR")
			.font(theme::small())
			.color(theme::TEXT_DIM),
	);
	ui.add_space(2.0);
	let history_h = 150.0;
	egui::ScrollArea::vertical()
		.id_salt("inspector")
		.max_height((ui.available_height() - history_h).max(100.0))
		.auto_shrink([false, false])
		.show(ui, |ui| match state.selection.focus() {
			Focus::Device(r) => device(ui, state, r),
			Focus::Clip(c) => clip(ui, state, c),
			Focus::Track(t) => track(ui, state, t),
			Focus::Master => channel_controls(ui, state, None),
			Focus::Nothing => {
				ui.label(
					RichText::new("Select a track, clip or device to edit its properties.")
						.color(theme::TEXT_FAINT),
				);
			}
		});
	ui.separator();
	history(ui, state);
}

fn heading(ui: &mut Ui, title: &str, subtitle: &str) {
	ui.label(RichText::new(title).heading().color(theme::TEXT));
	if !subtitle.is_empty() {
		ui.label(
			RichText::new(subtitle)
				.font(theme::small())
				.color(theme::TEXT_DIM),
		);
	}
	ui.add_space(4.0);
}

fn track(ui: &mut Ui, state: &mut AppState, id: TrackId) {
	let Some(t) = state.project().track(id).cloned() else {
		return;
	};
	heading(
		ui,
		&t.name,
		&format!("{} track · {} clips", kind_name(t.kind), t.clips.len()),
	);
	let mut name = t.name.clone();
	ui.horizontal(|ui| {
		ui.label("Name");
		let r = ui.text_edit_singleline(&mut name);
		if r.lost_focus() && name.trim() != t.name && !name.trim().is_empty() {
			state.apply(Command::UpdateTrack {
				track: id,
				patch: TrackPatch {
					name: Some(name.trim().to_owned()),
					color: None,
					input: None,
				},
			});
		}
	});
	ui.horizontal_wrapped(|ui| {
		ui.spacing_mut().item_spacing = vec2(3.0, 3.0);
		for hex in TRACK_PALETTE {
			let (r, resp) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::click());
			ui.painter()
				.rect_filled(r, CornerRadius::same(2), theme::track_color(Some(hex)));
			if t.color.as_deref() == Some(hex) {
				ui.painter().rect_stroke(
					r,
					CornerRadius::same(2),
					Stroke::new(1.5, egui::Color32::WHITE),
					StrokeKind::Inside,
				);
			}
			if resp.clicked() {
				state.apply(Command::UpdateTrack {
					track: id,
					patch: TrackPatch {
						name: None,
						color: Some(Some(hex.to_owned())),
						input: None,
					},
				});
			}
		}
	});
	ui.add_space(6.0);
	if t.kind == TrackKind::Instrument {
		ui.horizontal(|ui| {
			ui.label("Instrument");
			match &t.instrument {
				Some(d) => {
					if ui.link(&d.name).clicked() {
						state.selection.select_device(DeviceRef {
							track: Some(id),
							device: d.id,
						});
					}
					editor_button(ui, state, d.id);
					sampler_button(
						ui,
						state,
						DeviceRef {
							track: Some(id),
							device: d.id,
						},
					);
				}
				None => {
					ui.label(
						RichText::new("none — double-click one in the browser")
							.color(theme::TEXT_FAINT),
					);
				}
			}
		});
	}
	ui.add_space(4.0);
	channel_controls(ui, state, Some(id));
	ui.add_space(8.0);
	if ui.button("Delete track").clicked() {
		state.apply(Command::RemoveTrack { track: id });
	}
}

/// Volume, pan, mute/solo and inserts of a channel (`None` = master).
fn channel_controls(ui: &mut Ui, state: &mut AppState, track: Option<TrackId>) {
	if track.is_none() {
		heading(ui, "Master", "Main output");
	}
	let Some(ch) = state.project().channel(track).cloned() else {
		return;
	};
	Grid::new(("channel", track))
		.num_columns(2)
		.spacing([8.0, 4.0])
		.show(ui, |ui| {
			ui.label("Volume");
			let (g, vol_resp) = widgets::edit_value(
				ui,
				Id::new(("insp-vol", track)),
				state.volume_db(track),
				|ui, v| {
					ui.add(
						egui::Slider::new(v, -60.0..=6.0)
							.suffix(" dB")
							.max_decimals(1),
					)
				},
			);
			vol_resp.context_menu(|ui| {
				crate::ui::automation::automate_item(ui, state, crate::state::volume_address(track))
			});
			match g {
				Gesture::Preview(v) => state.preview_volume(track, v),
				Gesture::Commit(v) => state.commit_volume(track, v),
				Gesture::Idle => {}
			}
			ui.end_row();
			ui.label("Pan");
			let (g, pan_resp) = widgets::edit_value(
				ui,
				Id::new(("insp-pan", track)),
				state.pan(track),
				|ui, v| ui.add(egui::Slider::new(v, -1.0..=1.0).max_decimals(2)),
			);
			pan_resp.context_menu(|ui| {
				crate::ui::automation::automate_item(ui, state, crate::state::pan_address(track))
			});
			match g {
				Gesture::Preview(v) => state.preview_pan(track, v),
				Gesture::Commit(v) => state.commit_pan(track, v),
				Gesture::Idle => {}
			}
			ui.end_row();
			if let Some(t) = track {
				ui.label("");
				ui.horizontal(|ui| {
					if widgets::toggle(ui, ch.mute, "Mute", theme::MUTE, vec2(44.0, 18.0)).clicked()
					{
						state.apply(Command::SetChannel {
							track: Some(t),
							patch: ChannelPatch {
								mute: Some(!ch.mute),
								..Default::default()
							},
						});
					}
					if widgets::toggle(ui, ch.solo, "Solo", theme::SOLO, vec2(44.0, 18.0)).clicked()
					{
						state.apply(Command::SetChannel {
							track: Some(t),
							patch: ChannelPatch {
								solo: Some(!ch.solo),
								..Default::default()
							},
						});
					}
				});
				ui.end_row();
			}
		});
	ui.add_space(4.0);
	ui.label(
		RichText::new("Inserts")
			.font(theme::small())
			.color(theme::TEXT_DIM),
	);
	if ch.inserts.is_empty() {
		ui.label(
			RichText::new("none")
				.font(theme::small())
				.color(theme::TEXT_FAINT),
		);
	}
	for d in &ch.inserts {
		let r = DeviceRef {
			track,
			device: d.id,
		};
		if ui
			.selectable_label(
				state.selection.device == Some(r),
				format!("{}{}", d.name, if d.bypass { "  (bypassed)" } else { "" }),
			)
			.clicked()
		{
			state.selection.select_device(r);
		}
	}
}

fn clip(ui: &mut Ui, state: &mut AppState, id: cadenza_core::ClipId) {
	let Some((track, c)) = state
		.project()
		.find_clip(id)
		.map(|(t, c)| (t.clone(), c.clone()))
	else {
		return;
	};
	let ts = state.project().time_signature;
	let kind = if c.is_midi() {
		"MIDI clip"
	} else {
		"Audio clip"
	};
	heading(ui, &c.name, &format!("{kind} on {}", track.name));
	let mut name = c.name.clone();
	Grid::new("clip-props")
		.num_columns(2)
		.spacing([8.0, 4.0])
		.show(ui, |ui| {
			ui.label("Name");
			let r = ui.text_edit_singleline(&mut name);
			if r.lost_focus() && name.trim() != c.name && !name.trim().is_empty() {
				state.apply(Command::UpdateClip {
					clip: id,
					patch: ClipPatch {
						name: Some(name.trim().to_owned()),
						..ClipPatch::default()
					},
				});
			}
			ui.end_row();
			ui.label("Start");
			ui.label(RichText::new(format_bbt(c.start, ts)).font(theme::mono(12.0)));
			ui.end_row();
			ui.label("Length");
			let (g, _) = widgets::edit_value(
				ui,
				Id::new(("clip-len", id)),
				c.length.beats() as f32,
				|ui, v| {
					ui.add(
						egui::DragValue::new(v)
							.range(0.0625..=4096.0)
							.speed(0.05)
							.suffix(" beats")
							.max_decimals(3),
					)
				},
			);
			if let Gesture::Commit(v) = g
				&& let Some(cmd) = crate::state::edits::resize_clip(
					state.project(),
					id,
					c.start + Tick::from_beats(f64::from(v)),
				) {
				state.apply(cmd);
			}
			ui.end_row();
			match &c.content {
				ClipContent::Midi { notes } => {
					ui.label("Notes");
					ui.label(format!("{}", notes.len()));
					ui.end_row();
				}
				ClipContent::Audio {
					file,
					offset_seconds,
					gain_db,
					fade_in,
					fade_out,
				} => {
					ui.label("File");
					ui.add(
						egui::Label::new(file.file_name().map_or_else(
							|| file.display().to_string(),
							|f| f.to_string_lossy().into_owned(),
						))
						.truncate(),
					)
					.on_hover_text(file.display().to_string());
					ui.end_row();
					ui.label("Offset");
					ui.label(format!("{offset_seconds:.3} s"));
					ui.end_row();
					ui.label("Gain");
					ui.label(format!("{gain_db:+.1} dB"));
					ui.end_row();
					ui.label("Fades");
					ui.label(format!(
						"in {} · out {}",
						fade_in.map_or("—".into(), |f| format!("{:.2} s", f.seconds)),
						fade_out.map_or("—".into(), |f| format!("{:.2} s", f.seconds))
					));
					ui.end_row();
				}
			}
		});
	ui.add_space(8.0);
	if ui.button("Delete clip").clicked() {
		state.apply(Command::RemoveClip { clip: id });
	}
}

fn device(ui: &mut Ui, state: &mut AppState, r: DeviceRef) {
	let Some(d) = state.device(r).cloned() else {
		return;
	};
	let (kind, params): (String, Vec<ParamRow>) = match &d.kind {
		DeviceKind::Builtin { id } => (
			"Built-in".into(),
			cadenza_dsp::device_info(id)
				.map(|info| {
					info.params
						.iter()
						.map(|p| ParamRow {
							id: p.id.to_owned(),
							name: p.name.to_owned(),
							min: p.min,
							max: p.max,
							default: p.default,
							unit: p.unit.to_owned(),
						})
						.collect()
				})
				.unwrap_or_default(),
		),
		DeviceKind::Clap { plugin_id, path }
		| DeviceKind::Vst3 {
			class_id: plugin_id,
			path,
		} => {
			let format = if matches!(d.kind, DeviceKind::Clap { .. }) {
				"CLAP"
			} else {
				"VST3"
			};
			let catalog = state.catalog();
			let params = catalog
				.find_at(plugin_id, path)
				.map(|desc| {
					desc.params
						.iter()
						.filter(|p| p.automatable && !p.bypass)
						.map(|p| ParamRow {
							id: p.id.clone(),
							name: p.name.clone(),
							min: p.min as f32,
							max: p.max as f32,
							default: p.default as f32,
							unit: String::new(),
						})
						.collect()
				})
				.unwrap_or_default();
			(format!("{format} plugin"), params)
		}
	};
	let where_ = match r.track {
		Some(t) => state
			.project()
			.track(t)
			.map_or(String::new(), |t| t.name.clone()),
		None => "Master".into(),
	};
	heading(ui, &d.name, &format!("{kind} · {where_}"));
	let is_instrument = r
		.track
		.and_then(|t| state.project().track(t))
		.and_then(|t| t.instrument.as_ref())
		.is_some_and(|i| i.id == d.id);
	ui.horizontal(|ui| {
		if !is_instrument
			&& widgets::toggle(ui, d.bypass, "Bypass", theme::BYPASS, vec2(54.0, 18.0)).clicked()
		{
			state.apply(Command::SetDeviceBypass {
				track: r.track,
				device: d.id,
				bypass: !d.bypass,
			});
		}
		if ui.small_button("Remove").clicked() {
			state.remove_device(r.track, d.id);
		}
		editor_button(ui, state, d.id);
		sampler_button(ui, state, r);
	});
	if let Some(kind) = crate::state::samples::kind(&d) {
		ui.add_space(4.0);
		let what = match kind {
			crate::state::samples::SampleKind::Drums => "pads",
			crate::state::samples::SampleKind::Sampler => "zones",
		};
		ui.label(
			RichText::new(format!("{} {what} loaded", d.zones.len()))
				.font(theme::small())
				.color(theme::TEXT_DIM),
		);
	}
	ui.add_space(6.0);
	if params.is_empty() {
		ui.label(RichText::new("No editable parameters").color(theme::TEXT_FAINT));
		return;
	}
	let params = filter_params(ui, d.id, params);
	// One row per parameter: fixed-width name, then a slider filling the rest.
	let label_w = 104.0;
	for ParamRow {
		id: pid,
		name,
		min,
		max,
		default,
		unit,
	} in params
	{
		ui.horizontal(|ui| {
			ui.add_sized(
				[label_w, 18.0],
				egui::Label::new(
					RichText::new(&name)
						.font(theme::small())
						.color(theme::TEXT_DIM),
				)
				.truncate(),
			)
			.on_hover_text(&name);
			let current = state.param_value(r.track, d.id, &pid).unwrap_or(default);
			// Room for the value box, which grows with the unit ("0.0 semitones").
			let value_w = 52.0 + 6.0 * unit.chars().count() as f32;
			ui.spacing_mut().slider_width = (ui.available_width() - value_w - 8.0).max(40.0);
			let (g, resp) =
				widgets::edit_value(ui, Id::new(("param", d.id, &pid)), current, |ui, v| {
					let mut s =
						egui::Slider::new(v, min..=max.max(min + f32::EPSILON)).max_decimals(2);
					if !unit.is_empty() {
						s = s.suffix(format!(" {unit}"));
					}
					ui.add(s)
				});
			let address = ParamAddress::device(r.track, d.id, pid.clone());
			if state.is_automated(&address) {
				crate::ui::automation::automated_badge(ui, resp.rect);
			}
			resp.context_menu(|ui| crate::ui::automation::automate_item(ui, state, address));
			if resp.double_clicked() {
				state.apply(Command::ResetParam {
					address: ParamAddress::device(r.track, d.id, pid.clone()),
				});
			} else {
				match g {
					Gesture::Preview(v) => state.preview_param(d.id, &pid, v),
					Gesture::Commit(v) => state.commit_param(r.track, d.id, &pid, v),
					Gesture::Idle => {}
				}
			}
		});
	}
}

/// "Edit" (opens the plugin's own window) / "Close editor" for plugin devices.
fn editor_button(ui: &mut Ui, state: &mut AppState, device: cadenza_core::DeviceId) {
	if !state.is_plugin(device) {
		return;
	}
	let open = state.plugin_editor_open(device);
	let label = if open { "Close editor" } else { "Edit" };
	if ui
		.small_button(label)
		.on_hover_text(
			"The plugin's own editor window (also: double-click the device in the mixer); \
			 changes made there are part of the project and undoable",
		)
		.clicked()
	{
		state.toggle_plugin_editor(device);
	}
}

/// "Edit" for the built-in samplers: their pad / zone editor in the bottom dock.
fn sampler_button(ui: &mut Ui, state: &mut AppState, r: DeviceRef) {
	let Some(kind) = state.device(r).and_then(crate::state::samples::kind) else {
		return;
	};
	let label = match kind {
		crate::state::samples::SampleKind::Drums => "Edit pads",
		crate::state::samples::SampleKind::Sampler => "Edit zones",
	};
	if ui
		.small_button(label)
		.on_hover_text(
			"Pads / zones, waveform and knobs in the bottom editor dock (also: double-click \
			 the instrument in its track header or the mixer)",
		)
		.clicked()
	{
		state.toggle_instrument_editor(r);
	}
}

/// The generic editor's rows: everything for small devices; a name filter and the
/// first [`MAX_PLUGIN_PARAMS`] matches (or all, on request) for large plugins.
fn filter_params(
	ui: &mut Ui,
	device: cadenza_core::DeviceId,
	params: Vec<ParamRow>,
) -> Vec<ParamRow> {
	if params.len() <= FILTER_FROM {
		return params;
	}
	let filter_id = Id::new(("param-filter", device));
	let all_id = Id::new(("param-all", device));
	let mut filter: String = ui.data_mut(|d| d.get_temp(filter_id).unwrap_or_default());
	let mut all: bool = ui.data_mut(|d| d.get_temp(all_id).unwrap_or(false));
	let total = params.len();
	ui.horizontal(|ui| {
		ui.label(
			RichText::new(format!("{total} parameters"))
				.font(theme::small())
				.color(theme::TEXT_DIM),
		);
		ui.add(
			egui::TextEdit::singleline(&mut filter)
				.hint_text("filter")
				.desired_width(ui.available_width().min(140.0)),
		);
	});
	let needle = filter.trim().to_lowercase();
	let matching: Vec<ParamRow> = params
		.into_iter()
		.filter(|p| needle.is_empty() || p.name.to_lowercase().contains(&needle))
		.collect();
	let found = matching.len();
	let shown: Vec<ParamRow> = if all {
		matching
	} else {
		matching.into_iter().take(MAX_PLUGIN_PARAMS).collect()
	};
	if found > MAX_PLUGIN_PARAMS {
		let label = if all {
			format!("Show the first {MAX_PLUGIN_PARAMS}")
		} else {
			format!("Show all {found}")
		};
		if ui.small_button(label).clicked() {
			all = !all;
		}
	}
	ui.data_mut(|d| {
		d.insert_temp(filter_id, filter);
		d.insert_temp(all_id, all);
	});
	if shown.is_empty() {
		ui.label(
			RichText::new("No parameter matches")
				.font(theme::small())
				.color(theme::TEXT_FAINT),
		);
	}
	shown
}

fn history(ui: &mut Ui, state: &mut AppState) {
	ui.horizontal(|ui| {
		ui.label(
			RichText::new("HISTORY")
				.font(theme::small())
				.color(theme::TEXT_DIM),
		);
		ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
			if ui
				.add_enabled(
					state.document().can_redo(),
					egui::Button::new("Redo").small(),
				)
				.clicked()
			{
				state.redo();
			}
			if ui
				.add_enabled(
					state.document().can_undo(),
					egui::Button::new("Undo").small(),
				)
				.clicked()
			{
				state.undo();
			}
		});
	});
	let done: Vec<String> = state
		.document()
		.history_labels()
		.iter()
		.map(|s| (*s).to_owned())
		.collect();
	let undone: Vec<String> = state
		.document()
		.redo_labels()
		.iter()
		.map(|s| (*s).to_owned())
		.collect();
	egui::ScrollArea::vertical()
		.id_salt("history")
		.auto_shrink([false, false])
		.stick_to_bottom(true)
		.show(ui, |ui| {
			if done.is_empty() && undone.is_empty() {
				ui.label(
					RichText::new("No edits yet")
						.font(theme::small())
						.color(theme::TEXT_FAINT),
				);
			}
			for label in &done {
				ui.label(RichText::new(label).font(theme::small()).color(theme::TEXT));
			}
			for label in undone.iter() {
				ui.label(
					RichText::new(label)
						.font(theme::small())
						.color(theme::TEXT_FAINT)
						.italics(),
				);
			}
		});
}
