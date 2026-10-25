//! Inspector: tabs Track | Clip | Device over the selection (the tab follows what was
//! selected last; `state::inspector`), and the undo history footer.

use crate::i18n::{tr, trf, trn, trn_with};
use cadenza_core::{ParamAddress, TrackId};
use cadenza_engine::ProbeFrame;
use cadenza_model::{
	ChannelPatch, ClipContent, ClipFade, ClipPatch, Command, DeviceKind, FadeCurve, TrackKind,
	TrackPatch,
};
use egui::{CornerRadius, Id, RichText, Sense, Stroke, StrokeKind, Ui, vec2};

use crate::state::AppState;
use crate::state::edits::{TRACK_PALETTE, kind_name};
use crate::state::inspector::{self, InspectorTab, Targets};
use crate::state::layout::PanelKind;
use crate::state::selection::DeviceRef;
use crate::state::timeline::format_bbt;
use crate::state::windows::WindowItem;
use crate::theme;
use crate::ui::device_kit::{self, ReadoutLook, ReadoutState, ValueFmt};
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

/// Width of the property labels (Name, Start, Volume...).
const LABEL_W: f32 = 72.0;
const ROW_H: f32 = 22.0;
/// The history footer's height (open / folded).
const HISTORY_H: f32 = 168.0;
const HISTORY_FOLDED_H: f32 = 30.0;
const HISTORY_MIN_H: f32 = 84.0;

pub fn show(ui: &mut Ui, state: &mut AppState) {
	ui.add_space(4.0);
	widgets::panel_title(ui, tr("Inspector"), Some("F8"));
	ui.add_space(4.0);
	let tab = state.inspector_tab();
	let targets = state.inspector_targets();
	header(ui, state, tab, &targets);
	ui.add_space(6.0);
	if let Some(pick) = tabs(ui, tab, &targets) {
		state.view.inspector.pick(pick);
	}
	let tab = state.view.inspector.tab;
	ui.add_space(8.0);
	// A fixed footer; a short panel (the mixer docked below it) keeps most of its height
	// for the tab.
	let avail = ui.available_height();
	let history_h = if state.view.inspector.history_folded {
		HISTORY_FOLDED_H
	} else {
		(avail * 0.3).clamp(HISTORY_MIN_H, HISTORY_H)
	};
	egui::ScrollArea::vertical()
		.id_salt("inspector")
		.max_height((avail - history_h).max(100.0))
		.auto_shrink([false, false])
		.show(ui, |ui| {
			ui.spacing_mut().item_spacing.y = 4.0;
			match tab {
				InspectorTab::Track => match targets.track {
					Some(Some(t)) => track(ui, state, t),
					Some(None) => channel_controls(ui, state, None),
					None => project_overview(ui, state),
				},
				InspectorTab::Clip => match targets.clip {
					Some(c) => clip(ui, state, c),
					None => empty(ui, &targets.hint(tab)),
				},
				InspectorTab::Device => match targets.device {
					Some(r) => device(ui, state, r),
					None => empty(ui, &targets.hint(tab)),
				},
			}
		});
	history(ui, state);
}

/// The body of a floating Track / Clip properties window (T37): the item's header and
/// the inspector tab that edits it (the same rows, one command per edit).
pub fn item_window(ui: &mut Ui, state: &mut AppState, item: WindowItem) {
	let (tab, targets) = match item {
		WindowItem::Track(t) => (
			InspectorTab::Track,
			Targets {
				track: Some(t),
				clip: None,
				device: None,
				clips: 0,
			},
		),
		WindowItem::Clip(c) => (
			InspectorTab::Clip,
			Targets {
				track: None,
				clip: Some(c),
				device: None,
				clips: 1,
			},
		),
		WindowItem::Device(_) => return,
	};
	header(ui, state, tab, &targets);
	ui.add_space(4.0);
	egui::ScrollArea::vertical()
		.id_salt(("float-item", item.id_key()))
		.auto_shrink([false, false])
		.show(ui, |ui| {
			ui.spacing_mut().item_spacing.y = 4.0;
			match item {
				WindowItem::Track(Some(t)) => track(ui, state, t),
				WindowItem::Track(None) => channel_controls(ui, state, None),
				WindowItem::Clip(c) => clip(ui, state, c),
				WindowItem::Device(_) => {}
			}
			ui.add_space(6.0);
		});
}

/// Shows `r` in the inspector's Device tab, or in its window while the inspector is
/// hidden (track / clip windows).
fn show_device(state: &mut AppState, r: DeviceRef) {
	state.selection.select_device(r);
	if !state.view.panels.visible(PanelKind::Inspector) && state.has_device_editor(r) {
		state.open_device_editor(r);
	}
}

/// The item the active tab edits: colour chip, name and what it is.
fn header(ui: &mut Ui, state: &AppState, tab: InspectorTab, targets: &Targets) {
	let p = state.project();
	let track_of = |id: TrackId| p.track(id);
	let index_of = |id: TrackId| {
		p.tracks
			.iter()
			.position(|t| t.id == id)
			.map_or(0, |i| i + 1)
	};
	let (color, title, subtitle) = match tab {
		InspectorTab::Device if targets.device.is_some() => {
			let r = targets.device.expect("device");
			let d = state.device(r);
			let where_ = r
				.track
				.and_then(track_of)
				.map_or(tr("Master").to_owned(), |t| t.name.clone());
			let kind = d.map_or("", |d| match d.kind {
				DeviceKind::Builtin { .. } => tr("Built-in"),
				DeviceKind::Clap { .. } => tr("CLAP plugin"),
				DeviceKind::Vst3 { .. } => "VST3 plugin",
			});
			let role = if r
				.track
				.and_then(track_of)
				.and_then(|t| t.instrument.as_ref())
				.is_some_and(|i| i.id == r.device)
			{
				"instrument"
			} else {
				"insert"
			};
			(
				r.track
					.and_then(track_of)
					.map_or(theme::ACCENT, |t| theme::track_color(t.color.as_deref())),
				d.map_or(String::new(), |d| d.name.clone()),
				format!("{kind} {role} on {where_}"),
			)
		}
		InspectorTab::Clip if targets.clip.is_some() => {
			let id = targets.clip.expect("clip");
			match p.find_clip(id) {
				Some((t, c)) => (
					theme::track_color(t.color.as_deref()),
					c.name.clone(),
					if c.is_midi() {
						trf(
							"MIDI clip on {number} · {track}",
							&[("number", &index_of(t.id)), ("track", &t.name)],
						)
					} else {
						trf(
							"Audio clip on {number} · {track}",
							&[("number", &index_of(t.id)), ("track", &t.name)],
						)
					},
				),
				None => (theme::TEXT_FAINT, String::new(), String::new()),
			}
		}
		InspectorTab::Track if targets.track == Some(None) => (
			theme::ACCENT,
			tr("Master").to_owned(),
			tr("Main output").to_owned(),
		),
		InspectorTab::Track if targets.track.is_some() => {
			let id = targets.track.flatten().expect("track");
			match track_of(id) {
				Some(t) => (
					theme::track_color(t.color.as_deref()),
					t.name.clone(),
					trn_with(
						"{kind} track {number} · {n} clip",
						"{kind} track {number} · {n} clips",
						t.clips.len(),
						&[("kind", &kind_name(t.kind)), ("number", &index_of(id))],
					),
				),
				None => (theme::TEXT_FAINT, String::new(), String::new()),
			}
		}
		_ => (theme::TEXT_FAINT, p.name.clone(), tr("Project").to_owned()),
	};
	let width = ui.available_width();
	let (rect, _) = ui.allocate_exact_size(vec2(width, 34.0), Sense::hover());
	let painter = ui.painter();
	let chip = egui::Rect::from_min_size(rect.min + vec2(0.0, 4.0), vec2(10.0, 10.0));
	painter.rect_filled(chip, CornerRadius::same(2), color);
	let text_x = chip.right() + 8.0;
	let font = theme::semibold(14.0);
	let title = crate::ui::mixer::elide(ui, &title, &font, rect.right() - text_x);
	painter.text(
		egui::pos2(text_x, chip.center().y),
		egui::Align2::LEFT_CENTER,
		title,
		font,
		theme::TEXT,
	);
	let small = theme::small();
	let subtitle = crate::ui::mixer::elide(ui, &subtitle, &small, rect.right() - text_x);
	painter.text(
		egui::pos2(text_x, chip.center().y + 17.0),
		egui::Align2::LEFT_CENTER,
		subtitle,
		small,
		theme::TEXT_DIM,
	);
}

/// The Track | Clip | Device strip; tabs with nothing to show are dimmed and say why.
fn tabs(ui: &mut Ui, active: InspectorTab, targets: &Targets) -> Option<InspectorTab> {
	let height = 26.0;
	let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
	let p = ui.painter().clone();
	p.rect_filled(rect, CornerRadius::same(theme::RADIUS_MD), theme::BG_DEEP);
	let w = rect.width() / InspectorTab::ALL.len() as f32;
	let mut clicked = None;
	for (i, tab) in InspectorTab::ALL.into_iter().enumerate() {
		let r = egui::Rect::from_min_size(
			egui::pos2(rect.left() + i as f32 * w, rect.top()),
			vec2(w, height),
		)
		.shrink(2.0);
		let resp = ui.interact(r, Id::new(("inspector-tab", i)), Sense::click());
		let on = tab == active;
		let has = targets.has(tab);
		let label = format!("{} tab", tab.label());
		resp.widget_info(|| {
			egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, on, &label)
		});
		let (bg, fg) = if on {
			(
				theme::BG_HOVER,
				if has { theme::TEXT } else { theme::TEXT_DIM },
			)
		} else if resp.hovered() {
			(theme::BG_WIDGET, theme::TEXT)
		} else if has {
			(theme::BG_DEEP, theme::TEXT_DIM)
		} else {
			(
				theme::BG_DEEP,
				theme::mix(theme::TEXT_FAINT, theme::BG_DEEP, 0.35),
			)
		};
		p.rect_filled(r, CornerRadius::same(theme::RADIUS), bg);
		p.text(
			r.center(),
			egui::Align2::CENTER_CENTER,
			tab.label(),
			theme::medium(12.0),
			fg,
		);
		let resp = if has {
			resp
		} else {
			resp.on_hover_text(targets.hint(tab))
		};
		if resp.clicked() {
			clicked = Some(tab);
		}
	}
	clicked
}

/// An empty tab: what to select to fill it.
fn empty(ui: &mut Ui, hint: &str) {
	egui::Frame::new()
		.fill(theme::BG_DEEP)
		.corner_radius(CornerRadius::same(theme::RADIUS_MD))
		.inner_margin(egui::Margin::symmetric(10, 12))
		.show(ui, |ui| {
			ui.set_width(ui.available_width());
			ui.label(
				RichText::new(hint)
					.font(theme::small())
					.color(theme::TEXT_FAINT),
			);
		});
}

/// A property row: dim label, then the value widget filling the rest.
fn row<R>(ui: &mut Ui, label: &str, add: impl FnOnce(&mut Ui) -> R) -> R {
	ui.horizontal(|ui| {
		ui.spacing_mut().item_spacing.x = 4.0;
		let (r, _) = ui.allocate_exact_size(vec2(LABEL_W, ROW_H), Sense::hover());
		ui.painter().text(
			r.left_center(),
			egui::Align2::LEFT_CENTER,
			label,
			theme::small(),
			theme::TEXT_DIM,
		);
		add(ui)
	})
	.inner
}

/// A small section title ("SENDS", "INSERTS").
fn section(ui: &mut Ui, title: &str) {
	ui.add_space(6.0);
	ui.label(
		RichText::new(title)
			.font(theme::semibold(10.0))
			.color(theme::TEXT_FAINT),
	);
}

/// A read-only value in a field (elided; `hover` = the whole text).
fn value_box(ui: &mut Ui, text: &str, hover: Option<&str>) -> egui::Response {
	let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 20.0), Sense::hover());
	let font = theme::mono(11.5);
	let shown = crate::ui::mixer::elide(ui, text, &font, rect.width() - 12.0);
	let p = ui.painter();
	p.rect(
		rect,
		CornerRadius::same(theme::RADIUS),
		theme::DISPLAY_BG,
		Stroke::new(1.0, theme::BG_WIDGET),
		StrokeKind::Inside,
	);
	p.text(
		egui::pos2(rect.right() - 7.0, rect.center().y),
		egui::Align2::RIGHT_CENTER,
		shown,
		font,
		theme::TEXT_DIM,
	);
	match hover {
		Some(h) => resp.on_hover_text(h),
		None => resp,
	}
}

/// A one-line text field that commits on Enter / clicking away (Esc cancels); the typed
/// text lives in egui memory while it has the focus.
fn text_field(ui: &mut Ui, id: Id, current: &str, mono: bool) -> Option<String> {
	let buf_id = id.with("buf");
	let mut buf: String = ui
		.data(|d| d.get_temp(buf_id))
		.unwrap_or_else(|| current.to_owned());
	let font = if mono {
		theme::mono(11.5)
	} else {
		theme::medium(12.0)
	};
	let out = egui::TextEdit::singleline(&mut buf)
		.id(id)
		.font(font)
		.desired_width(ui.available_width())
		.horizontal_align(if mono {
			egui::Align::RIGHT
		} else {
			egui::Align::LEFT
		})
		.show(ui);
	let r = out.response;
	if r.has_focus() {
		ui.data_mut(|d| d.insert_temp(buf_id, buf));
		return None;
	}
	ui.data_mut(|d| d.remove_temp::<String>(buf_id));
	let cancelled = ui.input(|i| i.key_pressed(egui::Key::Escape));
	let text = buf.trim();
	(r.lost_focus() && !cancelled && !text.is_empty() && text != current.trim())
		.then(|| text.to_owned())
}

/// The track's name field (one `UpdateTrack` when committed).
fn name_row(ui: &mut Ui, state: &mut AppState, t: &cadenza_model::Track) {
	let name = row(ui, tr("Name"), |ui| {
		text_field(ui, Id::new(("insp-track-name", t.id)), &t.name, false)
	});
	if let Some(name) = name {
		state.apply(Command::UpdateTrack {
			track: t.id,
			patch: TrackPatch {
				name: Some(name),
				color: None,
				input: None,
			},
		});
	}
}

/// No selection: the project at a glance instead of an empty panel.
fn project_overview(ui: &mut Ui, state: &AppState) {
	let p = state.project();
	let end = p
		.tracks
		.iter()
		.flat_map(|t| t.clips.iter().map(|c| c.end()))
		.max()
		.unwrap_or(cadenza_core::Tick::ZERO);
	let meter = p.meter();
	let length_bars = meter.bar_at(meter.bar_ceil(end)).index;
	let rows = [
		(
			tr("Tempo"),
			format!("{:.2} BPM", p.tempo.bpm_at(cadenza_core::Tick::ZERO)),
		),
		(
			tr("Signature"),
			format!(
				"{}/{}",
				p.time_signature.numerator, p.time_signature.denominator
			),
		),
		(tr("Tracks"), p.tracks.len().to_string()),
		(
			tr("Length"),
			trf("{length_bars} bars", &[("length_bars", &length_bars)]),
		),
		(
			tr("Style"),
			p.meta.style.clone().unwrap_or_else(|| "—".to_owned()),
		),
	];
	for (label, value) in rows {
		row(ui, label, |ui| value_box(ui, &value, None));
	}
	ui.add_space(8.0);
	ui.label(
		RichText::new(tr("Select a track, clip or device to edit its properties."))
			.font(theme::small())
			.color(theme::TEXT_FAINT),
	);
}

fn track(ui: &mut Ui, state: &mut AppState, id: TrackId) {
	let Some(t) = state.project().track(id).cloned() else {
		return;
	};
	name_row(ui, state, &t);
	let picked = row(ui, tr("Colour"), |ui| {
		colour_swatches(ui, t.color.as_deref())
	});
	if let Some(hex) = picked {
		state.apply(Command::UpdateTrack {
			track: id,
			patch: TrackPatch {
				name: None,
				color: Some(Some(hex.to_owned())),
				input: None,
			},
		});
	}
	row(ui, tr("Kind"), |ui| value_box(ui, kind_name(t.kind), None));
	if t.kind == TrackKind::Instrument {
		row(ui, tr("Instrument"), |ui| match &t.instrument {
			Some(d) => {
				let r = DeviceRef {
					track: Some(id),
					device: d.id,
				};
				if ui
					.link(RichText::new(&d.name).font(theme::medium(12.0)))
					.on_hover_text(tr(
						"Show it (Device tab, or its window with the inspector hidden)",
					))
					.clicked()
				{
					show_device(state, r);
				}
				editor_button(ui, state, d.id);
				device_editor_button(ui, state, r);
			}
			None => {
				ui.label(
					RichText::new(tr("none: double-click one in the browser"))
						.font(theme::small())
						.color(theme::TEXT_FAINT),
				);
			}
		});
	}
	if t.kind == TrackKind::Audio {
		row(ui, tr("Input"), |ui| {
			let (rect, _) = ui.allocate_exact_size(vec2(84.0, 20.0), Sense::hover());
			crate::ui::record::input_controls(ui, state, &t, Id::new(("insp-input", id)), rect);
		});
	}
	if t.kind != TrackKind::Folder {
		let channel = t.channel.clone();
		row(ui, tr("Output"), |ui| {
			let w = ui.available_width();
			crate::ui::mixer::output_selector(ui, state, &t, &channel, w);
		});
	}
	section(
		ui,
		if t.kind == TrackKind::Folder {
			"FOLDER (VCA)"
		} else {
			tr("CHANNEL")
		},
	);
	channel_controls(ui, state, Some(id));
	ui.add_space(10.0);
	if ui
		.add(egui::Button::new(RichText::new(tr("Delete track")).font(theme::small())).small())
		.clicked()
	{
		state.apply(Command::RemoveTrack { track: id });
	}
}

/// The 16 track colours in two rows; returns the clicked one.
fn colour_swatches(ui: &mut Ui, current: Option<&str>) -> Option<&'static str> {
	let size = ((ui.available_width() - 7.0 * 3.0) / 8.0).clamp(12.0, 18.0);
	let mut picked = None;
	ui.vertical(|ui| {
		ui.spacing_mut().item_spacing = vec2(3.0, 3.0);
		for chunk in TRACK_PALETTE.chunks(8) {
			ui.horizontal(|ui| {
				for hex in chunk {
					let (r, resp) = ui.allocate_exact_size(vec2(size, size), Sense::click());
					ui.painter().rect_filled(
						r,
						CornerRadius::same(3),
						theme::track_color(Some(hex)),
					);
					if current == Some(*hex) {
						ui.painter().rect_stroke(
							r.expand(1.0),
							CornerRadius::same(3),
							Stroke::new(1.5, theme::TEXT),
							StrokeKind::Outside,
						);
					} else if resp.hovered() {
						ui.painter().rect_stroke(
							r,
							CornerRadius::same(3),
							Stroke::new(1.0, theme::TEXT_DIM),
							StrokeKind::Inside,
						);
					}
					if resp.clicked() {
						picked = Some(*hex);
					}
				}
			});
		}
	});
	picked
}

/// Volume, pan, mute/solo/arm, sends and inserts of a channel (`None` = master).
fn channel_controls(ui: &mut Ui, state: &mut AppState, track: Option<TrackId>) {
	let Some(ch) = state.project().channel(track).cloned() else {
		return;
	};
	let kind = track.and_then(|t| state.project().track(t)).map(|t| t.kind);
	let color = track
		.and_then(|t| state.project().track(t))
		.map_or(theme::ACCENT, |t| theme::track_color(t.color.as_deref()));
	let folder = kind == Some(TrackKind::Folder);
	row(ui, tr("Volume"), |ui| {
		let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 20.0), Sense::hover());
		let (g, resp) = widgets::volume_slider(
			ui,
			Id::new(("insp-vol", track)),
			state.volume_db(track),
			rect,
			color,
		);
		let address = crate::state::volume_address(track);
		if state.is_automated(&address) {
			crate::ui::automation::automated_badge(ui, resp.rect);
		}
		resp.on_hover_text(tr("Drag (Shift: fine), double-click: 0 dB"))
			.context_menu(|ui| crate::ui::automation::automate_item(ui, state, address));
		match g {
			Gesture::Preview(v) => state.preview_volume(track, v),
			Gesture::Commit(v) => state.commit_volume(track, v),
			Gesture::Idle => {}
		}
	});
	if !folder {
		row(ui, tr("Pan"), |ui| {
			let (g, resp) = widgets::pan(
				ui,
				Id::new(("insp-pan", track)),
				state.pan(track),
				vec2(ui.available_width(), 20.0),
			);
			let address = crate::state::pan_address(track);
			if state.is_automated(&address) {
				crate::ui::automation::automated_badge(ui, resp.rect);
			}
			resp.on_hover_text(tr("Drag (Shift: fine), double-click: center"))
				.context_menu(|ui| crate::ui::automation::automate_item(ui, state, address));
			match g {
				Gesture::Preview(v) => state.preview_pan(track, v),
				Gesture::Commit(v) => state.commit_pan(track, v),
				Gesture::Idle => {}
			}
		});
	}
	if let Some(t) = track {
		row(ui, "", |ui| {
			let w = ((ui.available_width() - 8.0) / 3.0).clamp(36.0, 64.0);
			if widgets::toggle(ui, ch.mute, tr("Mute"), theme::MUTE, vec2(w, 20.0)).clicked() {
				state.apply(Command::SetChannel {
					track: Some(t),
					patch: ChannelPatch {
						mute: Some(!ch.mute),
						..Default::default()
					},
				});
			}
			if widgets::toggle(ui, ch.solo, tr("Solo"), theme::SOLO, vec2(w, 20.0)).clicked() {
				state.apply(Command::SetChannel {
					track: Some(t),
					patch: ChannelPatch {
						solo: Some(!ch.solo),
						..Default::default()
					},
				});
			}
			if state.can_arm(t) {
				let armed = state.is_armed(t);
				if widgets::toggle(ui, armed, tr("● Arm"), theme::RECORD, vec2(w, 20.0))
					.on_hover_text(tr("Arm for recording"))
					.clicked()
				{
					state.toggle_arm(t);
				}
			}
		});
	}
	if folder {
		return;
	}
	if let Some(t) = track {
		section(ui, tr("SENDS"));
		let w = ui.available_width();
		for s in &ch.sends {
			crate::ui::mixer::send_row(ui, state, t, s, w + 4.0, "inspector");
		}
		let buses: Vec<(TrackId, String)> = state
			.project()
			.tracks
			.iter()
			.filter(|b| {
				b.kind == TrackKind::Bus && b.id != t && !ch.sends.iter().any(|s| s.target == b.id)
			})
			.map(|b| (b.id, b.name.clone()))
			.collect();
		if !buses.is_empty() {
			ui.menu_button(
				RichText::new(tr("+ Send"))
					.font(theme::small())
					.color(theme::TEXT_DIM),
				|ui| {
					for (bus, name) in buses {
						if ui.button(name).clicked() {
							state.apply(Command::AddSend {
								track: t,
								send: cadenza_model::Send::new(bus, -12.0),
								index: None,
							});
							ui.close();
						}
					}
				},
			);
		} else if ch.sends.is_empty() {
			ui.label(
				RichText::new(tr("No buses to send to"))
					.font(theme::small())
					.color(theme::TEXT_FAINT),
			);
		}
	}
	section(ui, tr("INSERTS"));
	for d in &ch.inserts {
		insert_row(ui, state, track, d);
	}
	let w = ui.available_width();
	crate::ui::mixer::insert_menu(ui, state, track, w + 4.0);
}

/// One insert: power dot (bypass), name (click: Device tab; double-click: its editor),
/// × on hover (remove).
fn insert_row(
	ui: &mut Ui,
	state: &mut AppState,
	track: Option<TrackId>,
	d: &cadenza_model::Device,
) {
	let r = DeviceRef {
		track,
		device: d.id,
	};
	let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 22.0), Sense::hover());
	let selected = state.selection.device == Some(r);
	let hovered = ui.rect_contains_pointer(rect);
	let p = ui.painter().clone();
	p.rect_filled(
		rect,
		CornerRadius::same(theme::RADIUS),
		if selected {
			theme::ACCENT_DIM
		} else if hovered {
			theme::BG_HOVER
		} else {
			theme::BG_WIDGET
		},
	);
	let power = egui::Rect::from_min_size(rect.min, vec2(20.0, rect.height()));
	let close = egui::Rect::from_min_size(
		egui::pos2(rect.right() - 18.0, rect.top()),
		vec2(18.0, rect.height()),
	);
	let name = egui::Rect::from_min_max(
		egui::pos2(power.right(), rect.top()),
		egui::pos2(close.left(), rect.bottom()),
	);
	let pr = ui
		.interact(power, Id::new(("insp-bypass", d.id)), Sense::click())
		.on_hover_text(if d.bypass { tr("Enable") } else { tr("Bypass") });
	p.circle_filled(
		power.center(),
		3.5,
		if d.bypass {
			theme::BYPASS
		} else {
			theme::PLAYING
		},
	);
	if pr.clicked() {
		state.apply(Command::SetDeviceBypass {
			track,
			device: d.id,
			bypass: !d.bypass,
		});
	}
	let plugin = state.is_plugin(d.id);
	let nr = ui.interact(name, Id::new(("insp-insert", d.id)), Sense::click());
	nr.widget_info(|| {
		egui::WidgetInfo::labeled(
			egui::WidgetType::Button,
			true,
			trf("{name} insert", &[("name", &d.name)]),
		)
	});
	let nr = nr.on_hover_text(if plugin {
		tr("Click: edit in the Device tab. Double-click: the plugin's window")
	} else {
		tr("Click: edit in the Device tab. Double-click: the device editor")
	});
	let font = theme::medium(12.0);
	let shown = crate::ui::mixer::elide(ui, &d.name, &font, name.width() - 4.0);
	let color = if d.bypass {
		theme::TEXT_FAINT
	} else {
		theme::TEXT
	};
	let galley = p.layout_no_wrap(shown, font, color);
	let pos = egui::pos2(name.left(), name.center().y - galley.size().y * 0.5);
	let text_w = galley.size().x;
	p.galley(pos, galley, color);
	if d.bypass {
		p.hline(
			pos.x..=pos.x + text_w,
			name.center().y,
			Stroke::new(1.0, theme::TEXT_FAINT),
		);
	}
	if state.device_editor_shown(r) || state.plugin_editor_open(d.id) {
		p.rect_stroke(
			rect,
			CornerRadius::same(theme::RADIUS),
			Stroke::new(1.0, theme::ACCENT),
			StrokeKind::Inside,
		);
	}
	if nr.double_clicked() {
		if plugin {
			state.toggle_plugin_editor(d.id);
		} else if state.has_device_editor(r) {
			state.toggle_device_editor(r);
		}
	} else if nr.clicked() {
		state.selection.select_device(r);
	}
	let cr = ui
		.interact(close, Id::new(("insp-remove", d.id)), Sense::click())
		.on_hover_text(tr("Remove"));
	if hovered {
		p.text(
			close.center(),
			egui::Align2::CENTER_CENTER,
			"×",
			theme::medium(13.0),
			if cr.hovered() {
				theme::ERROR
			} else {
				theme::TEXT_FAINT
			},
		);
	}
	if cr.clicked() {
		state.remove_device(track, d.id);
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
	let meter = cadenza_core::MeterMap::new(state.project().meter());
	let ts = state.project().meter().signature_at(c.start);
	let name = row(ui, tr("Name"), |ui| {
		text_field(ui, Id::new(("insp-clip-name", id)), &c.name, false)
	});
	if let Some(name) = name {
		state.apply(Command::UpdateClip {
			clip: id,
			patch: ClipPatch {
				name: Some(name),
				..ClipPatch::default()
			},
		});
	}
	let start = row(ui, tr("Start"), |ui| {
		text_field(
			ui,
			Id::new(("insp-clip-start", id)),
			&format_bbt(c.start, meter.view()),
			true,
		)
	});
	if let Some(text) = start {
		match inspector::parse_position(&text, meter.view()) {
			Some(at) => {
				if let Some(cmd) =
					crate::state::edits::move_clips(state.project(), &[id], at - c.start, None)
				{
					state.apply(cmd);
				}
			}
			None => state.notices.info(trf(
				"'{text}' is not a position: type bar.beat.tick, e.g. 5.1.000",
				&[("text", &text)],
			)),
		}
	}
	let length = row(ui, tr("Length"), |ui| {
		text_field(
			ui,
			Id::new(("insp-clip-length", id)),
			&inspector::format_length(c.length, ts),
			true,
		)
		.map(|t| (t.clone(), inspector::parse_length(&t, ts)))
	});
	match length {
		Some((_, Some(len))) => {
			if let Some(cmd) = crate::state::edits::resize_clip(state.project(), id, c.start + len)
			{
				state.apply(cmd);
			}
		}
		Some((text, None)) => state.notices.info(trf(
			"'{text}' is not a length: type bars.beats.ticks, e.g. 4.0.000",
			&[("text", &text)],
		)),
		None => {}
	}
	let index = state
		.project()
		.tracks
		.iter()
		.position(|t| t.id == track.id)
		.map_or(0, |i| i + 1);
	row(ui, tr("Track"), |ui| {
		value_box(ui, &format!("{index} · {}", track.name), None)
	});
	ui.add_space(2.0);
	clip_preview(ui, state, &track, &c);
	ui.add_space(2.0);
	match &c.content {
		ClipContent::Midi { .. } => {
			let inspector::NoteStats {
				count,
				pitch,
				velocity,
			} = inspector::note_stats(&c.content).unwrap_or_default();
			row(ui, tr("Notes"), |ui| {
				value_box(ui, &count.to_string(), None)
			});
			let name = crate::state::poly_synth_editor::note_name;
			row(ui, tr("Pitch"), |ui| {
				value_box(
					ui,
					&pitch.map_or("—".to_owned(), |(lo, hi)| {
						if lo == hi {
							name(lo)
						} else {
							format!("{} – {}", name(lo), name(hi))
						}
					}),
					None,
				)
			});
			row(ui, tr("Velocity"), |ui| {
				value_box(
					ui,
					&velocity.map_or("—".to_owned(), |(lo, hi)| {
						let v = |x: f32| (x * 127.0).round() as i32;
						if v(lo) == v(hi) {
							v(lo).to_string()
						} else {
							format!("{} – {}", v(lo), v(hi))
						}
					}),
					Some(tr(
						"MIDI velocity (1-127) of the softest and the loudest note",
					)),
				)
			});
			ui.add_space(6.0);
			ui.horizontal(|ui| {
				if ui
					.button(RichText::new(tr("Open in piano roll  F7")).font(theme::small()))
					.clicked()
				{
					state.open_piano_roll(id);
				}
				let copies = cadenza_model::links::link_count(state.project(), id);
				if copies > 1
					&& ui
						.button(
							RichText::new(trf(
								"Make unique (linked ×{copies})",
								&[("copies", &copies)],
							))
							.font(theme::small()),
						)
						.on_hover_text(tr(
							"This clip shares its notes with its linked copies: editing one edits \
							 all. Make unique gives it its own notes.",
						))
						.clicked()
				{
					crate::ui::make_clips_unique(state, id);
				}
				delete_clip_button(ui, state, id);
			});
			// The track's instrument inline (design 01), so the sound is one glance away.
			if let Some(d) = &track.instrument {
				let r = DeviceRef {
					track: Some(track.id),
					device: d.id,
				};
				ui.add_space(6.0);
				ui.horizontal(|ui| {
					ui.label(
						RichText::new(&d.name)
							.font(theme::semibold(12.5))
							.color(theme::TEXT),
					);
					ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
						let docked = state.view.panels.visible(PanelKind::Inspector);
						let (label, hint) = if docked {
							(
								tr("Device tab"),
								tr("Select the instrument and show it in the Device tab"),
							)
						} else {
							(tr("Open"), tr("Open the instrument's window"))
						};
						if ui.small_button(label).on_hover_text(hint).clicked() {
							show_device(state, r);
						}
					});
				});
				device_body(ui, state, r);
			}
		}
		ClipContent::Audio {
			file,
			offset_seconds,
			gain_db,
			fade_in,
			fade_out,
			..
		} => {
			let file_name = file.file_name().map_or_else(
				|| file.display().to_string(),
				|f| f.to_string_lossy().into_owned(),
			);
			row(ui, tr("File"), |ui| {
				value_box(ui, &file_name, Some(&file.display().to_string()))
			});
			row(ui, tr("Offset"), |ui| {
				value_box(ui, &format!("{offset_seconds:.3} s"), None)
			});
			let gain = row(ui, tr("Gain"), |ui| {
				readout_field(
					ui,
					Id::new(("insp-clip-gain", id)),
					*gain_db,
					-60.0,
					12.0,
					"dB",
				)
			});
			if let Some(db) = gain
				&& let Some(cmd) = crate::state::audio_edit::set_gain(state.project(), id, db)
			{
				state.apply(cmd);
			}
			let duration = state.project().tempo.tick_to_seconds(c.end())
				- state.project().tempo.tick_to_seconds(c.start);
			let max = duration.max(0.001) as f32;
			let fade = |f: Option<ClipFade>| f.map_or(0.0, |f| f.seconds as f32);
			let typed_in = row(ui, tr("Fade in"), |ui| {
				readout_field(
					ui,
					Id::new(("insp-fade-in", id)),
					fade(*fade_in),
					0.0,
					max,
					"s",
				)
			});
			let typed_out = row(ui, tr("Fade out"), |ui| {
				readout_field(
					ui,
					Id::new(("insp-fade-out", id)),
					fade(*fade_out),
					0.0,
					max,
					"s",
				)
			});
			let set = |f: Option<ClipFade>, s: f32| {
				Some(ClipFade {
					seconds: f64::from(s),
					curve: f.map_or(FadeCurve::default(), |f| f.curve),
				})
			};
			let fades = match (typed_in, typed_out) {
				(Some(s), _) => Some((set(*fade_in, s), *fade_out)),
				(None, Some(s)) => Some((*fade_in, set(*fade_out, s))),
				(None, None) => None,
			};
			if let Some((fi, fo)) = fades
				&& let Some(cmd) = crate::state::audio_edit::set_fades(state.project(), id, fi, fo)
			{
				state.apply(cmd);
			}
			ui.add_space(6.0);
			ui.horizontal(|ui| {
				if ui
					.button(RichText::new(tr("Open audio editor")).font(theme::small()))
					.clicked()
				{
					state.open_audio_editor(id);
				}
				if ui
					.button(RichText::new(tr("Edit notes")).font(theme::small()))
					.clicked()
				{
					state.open_note_editor(id);
				}
				delete_clip_button(ui, state, id);
			});
			if let Some((n, _)) = crate::state::note_editor::clip_notes(state.project(), id) {
				let s = n.summary();
				ui.label(
					RichText::new(format!(
						"{} · {}",
						trn("{n} note", "{n} notes", s.count),
						trf("{n} edited", &[("n", &s.edited)])
					))
					.font(theme::small())
					.color(theme::TEXT_DIM),
				);
			}
		}
	}
}

/// A small picture of the clip: its notes (pitch range fitted) or its waveform with the
/// fades.
fn clip_preview(
	ui: &mut Ui,
	state: &mut AppState,
	track: &cadenza_model::Track,
	c: &cadenza_model::Clip,
) {
	let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 56.0), Sense::hover());
	let color = theme::track_color(track.color.as_deref());
	let p = ui.painter().clone();
	p.rect_filled(
		rect,
		CornerRadius::same(theme::RADIUS_MD),
		theme::mix(theme::DISPLAY_BG, color, 0.12),
	);
	p.rect_stroke(
		rect,
		CornerRadius::same(theme::RADIUS_MD),
		Stroke::new(1.0, color.gamma_multiply(0.45)),
		StrokeKind::Inside,
	);
	let inner = rect.shrink2(vec2(4.0, 5.0));
	match &c.content {
		ClipContent::Midi { notes, .. } => {
			let Some((lo, hi)) = inspector::note_stats(&c.content).and_then(|s| s.pitch) else {
				p.text(
					rect.center(),
					egui::Align2::CENTER_CENTER,
					tr("No notes yet"),
					theme::small(),
					theme::TEXT_FAINT,
				);
				return;
			};
			let (lo, hi) = (f32::from(lo) - 0.5, f32::from(hi) + 0.5);
			let span = (hi - lo).max(12.0);
			let mid = (hi + lo) * 0.5;
			let row_h = (inner.height() / span).clamp(1.5, 5.0);
			let len = c.length.0.max(1) as f32;
			for n in notes {
				let x0 = inner.left() + n.start.0 as f32 / len * inner.width();
				let x1 = inner.left() + (n.start + n.length).0 as f32 / len * inner.width();
				let y = inner.center().y - (f32::from(n.pitch) - mid) / span * inner.height();
				let r = egui::Rect::from_min_max(
					egui::pos2(x0, y - row_h * 0.5),
					egui::pos2(x1.min(inner.right()).max(x0 + 1.5), y + row_h * 0.5),
				);
				p.rect_filled(
					r,
					CornerRadius::same(1),
					color.gamma_multiply(0.45 + 0.55 * n.velocity),
				);
			}
		}
		ClipContent::Audio {
			file,
			offset_seconds,
			gain_db,
			fade_in,
			fade_out,
			..
		} => {
			let peaks = state.peaks.get(file);
			let project = state.project();
			crate::ui::arrange::paint_waveform(
				&p,
				project,
				c,
				&peaks,
				*offset_seconds,
				*gain_db,
				inner,
				inner,
				color,
				&|_| Some(1.0),
			);
			crate::ui::arrange::paint_fades(
				&p,
				project,
				c,
				fade_in.map(|f| f.seconds),
				fade_out.map(|f| f.seconds),
				rect,
			);
		}
	}
}

fn delete_clip_button(ui: &mut Ui, state: &mut AppState, id: cadenza_core::ClipId) {
	if ui
		.button(RichText::new(tr("Delete clip")).font(theme::small()))
		.clicked()
	{
		state.apply(Command::RemoveClip { clip: id });
	}
}

/// A boxed kit readout over the row's width (click to type, units accepted).
fn readout_field(ui: &mut Ui, id: Id, value: f32, min: f32, max: f32, unit: &str) -> Option<f32> {
	let (typed, _) = device_kit::readout(
		ui,
		id,
		value,
		min,
		max,
		ValueFmt::unit(unit),
		ReadoutLook::Boxed,
		vec2(ui.available_width(), 20.0),
		ReadoutState::Rest,
	);
	typed.map(|v| v.clamp(min, max))
}

fn device(ui: &mut Ui, state: &mut AppState, r: DeviceRef) {
	let Some(d) = state.device(r).cloned() else {
		return;
	};
	let params: Vec<ParamRow> = match &d.kind {
		DeviceKind::Builtin { id } => cadenza_dsp::device_info(id)
			.map(|info| {
				info.params
					.iter()
					.map(|p| ParamRow {
						id: p.id.to_owned(),
						name: crate::i18n::tr_str(p.name).to_owned(),
						min: p.min,
						max: p.max,
						default: p.default,
						unit: p.unit.to_owned(),
					})
					.collect()
			})
			.unwrap_or_default(),
		DeviceKind::Clap { plugin_id, path }
		| DeviceKind::Vst3 {
			class_id: plugin_id,
			path,
		} => {
			let catalog = state.catalog();
			catalog
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
				.unwrap_or_default()
		}
	};
	let is_instrument = r
		.track
		.and_then(|t| state.project().track(t))
		.and_then(|t| t.instrument.as_ref())
		.is_some_and(|i| i.id == d.id);
	ui.horizontal(|ui| {
		if !is_instrument
			&& widgets::toggle(ui, d.bypass, tr("Bypass"), theme::BYPASS, vec2(54.0, 18.0))
				.clicked()
		{
			state.apply(Command::SetDeviceBypass {
				track: r.track,
				device: d.id,
				bypass: !d.bypass,
			});
		}
		if ui.small_button(tr("Remove")).clicked() {
			state.remove_device(r.track, d.id);
		}
		editor_button(ui, state, d.id);
		device_editor_button(ui, state, r);
	});
	if let Some(kind) = crate::state::samples::kind(&d) {
		ui.add_space(4.0);
		let what = match kind {
			crate::state::samples::SampleKind::Drums => "pads",
			crate::state::samples::SampleKind::Sampler => "zones",
		};
		ui.label(
			RichText::new(trf(
				"{n} {what} loaded",
				&[("n", &d.zones.len()), ("what", &what)],
			))
			.font(theme::small())
			.color(theme::TEXT_DIM),
		);
	}
	ui.add_space(6.0);
	if device_body(ui, state, r) {
		return;
	}
	generic_params(ui, state, r, params);
}

/// The compact editor of a built-in with a dedicated body; false for the others.
fn device_body(ui: &mut Ui, state: &mut AppState, r: DeviceRef) -> bool {
	let Some(d) = state.device(r).cloned() else {
		return false;
	};
	if d.builtin_id() == Some("eq") {
		compact_eq(ui, state, r);
		return true;
	}
	if d.builtin_id() == Some("compressor") {
		compact_compressor(ui, state, r);
		return true;
	}
	if d.builtin_id() == Some("limiter") {
		compact_limiter(ui, state, r);
		return true;
	}
	if d.builtin_id() == Some("reverb") {
		compact_reverb(ui, state, r);
		return true;
	}
	if d.builtin_id() == Some("delay") {
		compact_delay(ui, state, r);
		return true;
	}
	if let Some(id @ ("chorus" | "saturator" | "gain")) = d.builtin_id() {
		compact_modulation(ui, state, r, id);
		return true;
	}
	if let Some(
		id @ ("poly_synth" | "electric_piano" | "drum_machine" | "drum_sampler" | "sampler"),
	) = d.builtin_id()
	{
		compact_instrument(ui, state, r, id);
		return true;
	}
	false
}

/// The generic editor: one slider per parameter (plugins, simple built-ins).
fn generic_params(ui: &mut Ui, state: &mut AppState, r: DeviceRef, params: Vec<ParamRow>) {
	let Some(d) = state.device(r).cloned() else {
		return;
	};
	if params.is_empty() {
		ui.label(RichText::new(tr("No editable parameters")).color(theme::TEXT_FAINT));
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

/// This frame's drained probe samples of `r`, when the inspector is the one entitled to
/// them: `AppState::sync_editor_probe` points the probe at the dock/popped-out device
/// editor's device first and only falls back to the inspector's when none is open, so this
/// returns nothing while an editor holds it — only one consumer may drain the probe ring
/// per frame.
fn probe_frames(state: &AppState, r: DeviceRef) -> Vec<ProbeFrame> {
	if state.device_editor_target().is_some() || state.probe_target() != Some(r.device) {
		return Vec::new();
	}
	let Some(probe) = state.probe() else {
		return Vec::new();
	};
	let mut frames = Vec::new();
	probe.drain(&mut frames);
	frames
}

/// The Parametric EQ's compact editor (design 15b) instead of the slider list.
fn compact_eq(ui: &mut Ui, state: &mut AppState, r: DeviceRef) {
	use crate::ui::devices::eq;
	const SALT: &str = "inspector";
	let Some(info) = cadenza_dsp::device_info("eq") else {
		return;
	};
	// Clear of the inspector's scroll bar.
	let width = ui.available_width() - 8.0;
	let height = eq::compact_height(width, eq::footer_open(ui.ctx(), SALT, r));
	let (rect, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
	let accent = r
		.track
		.and_then(|t| state.project().track(t))
		.map_or(theme::ACCENT, |t| theme::track_color(t.color.as_deref()));
	let frames = probe_frames(state, r);
	let cx = crate::ui::device_editor::BodyCx {
		r,
		info,
		rect,
		frames: &frames,
		compact: true,
		accent,
		salt: SALT,
	};
	eq::eq_body(ui, state, &cx);
}

/// The Compressor's compact editor (design 15d) instead of the slider list.
fn compact_compressor(ui: &mut Ui, state: &mut AppState, r: DeviceRef) {
	use crate::ui::devices::compressor;
	const SALT: &str = "inspector";
	let Some(info) = cadenza_dsp::device_info("compressor") else {
		return;
	};
	let width = ui.available_width() - 8.0;
	let height = compressor::compact_height(width, compressor::footer_open(ui.ctx(), SALT, r));
	let (rect, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
	let accent = r
		.track
		.and_then(|t| state.project().track(t))
		.map_or(theme::ACCENT, |t| theme::track_color(t.color.as_deref()));
	let frames = probe_frames(state, r);
	let cx = crate::ui::device_editor::BodyCx {
		r,
		info,
		rect,
		frames: &frames,
		compact: true,
		accent,
		salt: SALT,
	};
	compressor::compressor_body(ui, state, &cx);
}

/// The Limiter's compact editor (readouts, history, knobs) instead of the slider list.
fn compact_limiter(ui: &mut Ui, state: &mut AppState, r: DeviceRef) {
	use crate::ui::devices::limiter;
	const SALT: &str = "inspector";
	let Some(info) = cadenza_dsp::device_info("limiter") else {
		return;
	};
	let width = ui.available_width() - 8.0;
	let height = limiter::compact_height(width, limiter::footer_open(ui.ctx(), SALT, r));
	let (rect, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
	let accent = r
		.track
		.and_then(|t| state.project().track(t))
		.map_or(theme::ACCENT, |t| theme::track_color(t.color.as_deref()));
	let frames = probe_frames(state, r);
	let cx = crate::ui::device_editor::BodyCx {
		r,
		info,
		rect,
		frames: &frames,
		compact: true,
		accent,
		salt: SALT,
	};
	limiter::limiter_body(ui, state, &cx);
}

/// The Reverb's compact editor (design 16b) instead of the slider list.
fn compact_reverb(ui: &mut Ui, state: &mut AppState, r: DeviceRef) {
	use crate::ui::devices::reverb;
	const SALT: &str = "inspector";
	let Some(info) = cadenza_dsp::device_info("reverb") else {
		return;
	};
	let width = ui.available_width() - 8.0;
	let height = reverb::compact_height(width);
	let (rect, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
	let accent = r
		.track
		.and_then(|t| state.project().track(t))
		.map_or(theme::ACCENT, |t| theme::track_color(t.color.as_deref()));
	let frames = probe_frames(state, r);
	let cx = crate::ui::device_editor::BodyCx {
		r,
		info,
		rect,
		frames: &frames,
		compact: true,
		accent,
		salt: SALT,
	};
	reverb::reverb_body(ui, state, &cx);
}

/// The Delay's compact editor (design 16c) instead of the slider list.
fn compact_delay(ui: &mut Ui, state: &mut AppState, r: DeviceRef) {
	use crate::ui::devices::delay;
	const SALT: &str = "inspector";
	let Some(info) = cadenza_dsp::device_info("delay") else {
		return;
	};
	let width = ui.available_width() - 8.0;
	let height = delay::compact_height(width);
	let (rect, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
	let accent = r
		.track
		.and_then(|t| state.project().track(t))
		.map_or(theme::ACCENT, |t| theme::track_color(t.color.as_deref()));
	let frames = probe_frames(state, r);
	let cx = crate::ui::device_editor::BodyCx {
		r,
		info,
		rect,
		frames: &frames,
		compact: true,
		accent,
		salt: SALT,
	};
	delay::delay_body(ui, state, &cx);
}

/// The Chorus, Saturator and Gain compact editors (design 16d-16f) instead of the slider
/// list.
fn compact_modulation(ui: &mut Ui, state: &mut AppState, r: DeviceRef, id: &str) {
	use crate::ui::devices::{chorus, gain, saturator};
	const SALT: &str = "inspector";
	let Some(info) = cadenza_dsp::device_info(id) else {
		return;
	};
	let width = ui.available_width() - 8.0;
	let height = match id {
		"chorus" => chorus::compact_height(width),
		"saturator" => saturator::compact_height(width),
		_ => gain::compact_height(width),
	};
	let (rect, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
	let accent = r
		.track
		.and_then(|t| state.project().track(t))
		.map_or(theme::ACCENT, |t| theme::track_color(t.color.as_deref()));
	let frames = probe_frames(state, r);
	let cx = crate::ui::device_editor::BodyCx {
		r,
		info,
		rect,
		frames: &frames,
		compact: true,
		accent,
		salt: SALT,
	};
	match id {
		"chorus" => chorus::chorus_body(ui, state, &cx),
		"saturator" => saturator::saturator_body(ui, state, &cx),
		_ => gain::gain_body(ui, state, &cx),
	}
}

/// The compact editor of the built-in instruments with dedicated bodies (Poly Synth,
/// Electric Piano, Drum Machine, Drum Sampler).
fn compact_instrument(ui: &mut Ui, state: &mut AppState, r: DeviceRef, id: &str) {
	use crate::ui::devices::{drum_machine, drum_sampler, electric_piano, poly_synth, sampler};
	let Some(info) = cadenza_dsp::device_info(id) else {
		return;
	};
	let width = ui.available_width() - 8.0;
	let height = match id {
		"electric_piano" => electric_piano::compact_height(width),
		"drum_machine" => drum_machine::compact_height(width),
		"drum_sampler" => drum_sampler::compact_height(width),
		"sampler" => sampler::compact_height(width),
		_ => poly_synth::compact_height(width, poly_synth::wavetable_count(state, r)),
	};
	let (rect, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
	let accent = r
		.track
		.and_then(|t| state.project().track(t))
		.map_or(theme::ACCENT, |t| theme::track_color(t.color.as_deref()));
	let frames = probe_frames(state, r);
	let cx = crate::ui::device_editor::BodyCx {
		r,
		info,
		rect,
		frames: &frames,
		compact: true,
		accent,
		salt: "inspector",
	};
	match id {
		"electric_piano" => electric_piano::electric_piano_body(ui, state, &cx, None),
		"drum_machine" => drum_machine::drum_machine_body(ui, state, &cx),
		"drum_sampler" => {
			let mut hits = drum_sampler::PadHits::default();
			drum_sampler::drum_sampler_body(ui, state, &cx, &mut hits);
		}
		"sampler" => {
			let mut hits = sampler::ZoneHits::default();
			sampler::sampler_body(ui, state, &cx, &mut hits);
		}
		_ => poly_synth::poly_synth_body(ui, state, &cx, None),
	}
}

/// "Edit" (opens the plugin's own window) / "Close editor" for plugin devices.
fn editor_button(ui: &mut Ui, state: &mut AppState, device: cadenza_core::DeviceId) {
	if !state.is_plugin(device) {
		return;
	}
	if !cadenza_engine::EDITORS_SUPPORTED {
		ui.add_enabled(false, egui::Button::new(tr("Edit")).small())
			.on_disabled_hover_text(tr(
				"Plugin editor windows are not supported on this platform yet; use the \
				 parameter controls below",
			));
		return;
	}
	let open = state.plugin_editor_open(device);
	let label = if open { tr("Close editor") } else { tr("Edit") };
	if ui
		.small_button(label)
		.on_hover_text(tr(
			"The plugin's own editor window (also: double-click the device in the mixer); \
			 changes made there are part of the project and undoable",
		))
		.clicked()
	{
		state.toggle_plugin_editor(device);
	}
}

/// "Edit" for built-in devices: the device editor in the bottom dock ("Edit pads" /
/// "Edit zones" for the samplers).
fn device_editor_button(ui: &mut Ui, state: &mut AppState, r: DeviceRef) {
	if !state.has_device_editor(r) {
		return;
	}
	let label = match state.device(r).and_then(crate::state::samples::kind) {
		Some(crate::state::samples::SampleKind::Drums) => tr("Edit pads"),
		Some(crate::state::samples::SampleKind::Sampler) => tr("Edit zones"),
		None => tr("Edit"),
	};
	if ui
		.small_button(label)
		.on_hover_text(tr(
			"The device's floating window (also: click the device in the mixer or \
			 double-click the instrument in its track header)",
		))
		.clicked()
	{
		state.toggle_device_editor(r);
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
			RichText::new(trf("{total} parameters", &[("total", &total)]))
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
			trf("Show the first {n}", &[("n", &MAX_PLUGIN_PARAMS)])
		} else {
			trf("Show all {found}", &[("found", &found)])
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
			RichText::new(tr("No parameter matches"))
				.font(theme::small())
				.color(theme::TEXT_FAINT),
		);
	}
	shown
}

/// The undo history footer: numbered steps (AI steps badged), undone ones faint; the
/// title folds it.
fn history(ui: &mut Ui, state: &mut AppState) {
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
	let rule = ui.available_rect_before_wrap();
	ui.painter()
		.hline(rule.x_range(), rule.top(), Stroke::new(1.0, theme::BORDER));
	ui.add_space(4.0);
	let folded = state.view.inspector.history_folded;
	ui.horizontal(|ui| {
		let title = ui
			.add(
				egui::Label::new(
					RichText::new(trf(
						"{arrow} History",
						&[("arrow", &if folded { "▸" } else { "▾" })],
					))
					.font(theme::heading())
					.color(theme::TEXT),
				)
				.sense(Sense::click()),
			)
			.on_hover_text(if folded {
				tr("Show the history")
			} else {
				tr("Fold the history")
			});
		if title.clicked() {
			state.view.inspector.history_folded = !folded;
		}
		ui.label(
			RichText::new(trn("{n} step", "{n} steps", done.len()))
				.font(theme::small())
				.color(theme::TEXT_FAINT),
		);
		ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
			if ui
				.add_enabled(
					state.document().can_redo(),
					egui::Button::new(tr("Redo")).small(),
				)
				.on_hover_text("Ctrl+Shift+Z")
				.clicked()
			{
				state.redo();
			}
			if ui
				.add_enabled(
					state.document().can_undo(),
					egui::Button::new(tr("Undo")).small(),
				)
				.on_hover_text("Ctrl+Z")
				.clicked()
			{
				state.undo();
			}
		});
	});
	if folded {
		return;
	}
	egui::ScrollArea::vertical()
		.id_salt("history")
		.auto_shrink([false, false])
		.stick_to_bottom(true)
		.show(ui, |ui| {
			ui.spacing_mut().item_spacing.y = 2.0;
			if done.is_empty() && undone.is_empty() {
				ui.label(
					RichText::new(tr("No edits yet"))
						.font(theme::small())
						.color(theme::TEXT_FAINT),
				);
			}
			let last = done.len();
			for (i, label) in done.iter().chain(undone.iter()).enumerate() {
				history_row(ui, i + 1, label, i + 1 > last, i + 1 == last);
			}
		});
}

fn history_row(ui: &mut Ui, n: usize, label: &str, undone: bool, current: bool) {
	let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 18.0), Sense::hover());
	let p = ui.painter();
	if current {
		p.rect_filled(rect, CornerRadius::same(theme::RADIUS), theme::BG_WIDGET);
	}
	let color = if undone {
		theme::TEXT_FAINT
	} else {
		theme::TEXT
	};
	p.text(
		egui::pos2(rect.left() + 18.0, rect.center().y),
		egui::Align2::RIGHT_CENTER,
		n.to_string(),
		theme::mono(10.5),
		theme::TEXT_FAINT,
	);
	let mut x = rect.left() + 26.0;
	let (badge, text) = match label.strip_prefix("AI:") {
		Some(rest) => (true, rest.trim()),
		None => (inspector::is_ai_step(label), label),
	};
	if badge {
		let chip =
			egui::Rect::from_min_size(egui::pos2(x, rect.center().y - 6.5), vec2(18.0, 13.0));
		p.rect_filled(
			chip,
			CornerRadius::same(2),
			if undone {
				theme::BG_WIDGET
			} else {
				theme::ACCENT_DIM
			},
		);
		p.text(
			chip.center(),
			egui::Align2::CENTER_CENTER,
			"AI",
			theme::semibold(9.0),
			if undone {
				theme::TEXT_FAINT
			} else {
				theme::ACCENT_TEXT
			},
		);
		x = chip.right() + 6.0;
	}
	let font = if undone {
		egui::FontId::new(11.0, egui::FontFamily::Proportional)
	} else {
		theme::small()
	};
	let shown = crate::ui::mixer::elide(ui, text, &font, rect.right() - x - 2.0);
	let elided = shown != text;
	let resp = ui.interact(rect, Id::new(("history-row", n)), Sense::hover());
	ui.painter().text(
		egui::pos2(x, rect.center().y),
		egui::Align2::LEFT_CENTER,
		shown,
		font,
		color,
	);
	if undone {
		resp.on_hover_text(trf(
			"{label} (undone: Redo brings it back)",
			&[("label", &label)],
		));
	} else if elided {
		resp.on_hover_text(label);
	}
}
