//! Transport bar, grouped in the order it is used: transport buttons, Pattern/Song, loop /
//! click / count-in, the position cluster (position, time, tempo, signature), recording and
//! automation modes, follow, then audio status and the master meter.

use crate::i18n::{tr, trf};
use cadenza_core::{Tick, TimeSignature};
use egui::{
	Align, Align2, CornerRadius, FontFamily, FontId, Id, Layout, Rect, RichText, Sense, Shape,
	Stroke, StrokeKind, Ui, Vec2, pos2, vec2,
};

use crate::state::timeline::{format_bbt, format_time};
use crate::state::transport::PlayMode;
use crate::state::{AppState, AudioStatus};
use crate::theme;
use crate::ui::widgets::{self, Gesture};

/// Space between transport groups.
const GROUP_GAP: f32 = 12.0;
/// Transport button size (`h-transport`).
const BUTTON: Vec2 = vec2(34.0, 30.0);
/// Height of the labelled controls (caption above).
const FIELD: f32 = 22.0;

pub fn show(ui: &mut Ui, state: &mut AppState) {
	let layout = state.view.responsive.transport;
	ui.horizontal_centered(|ui| {
		ui.spacing_mut().item_spacing.x = 6.0;
		ui.add_space(4.0);
		// Transport buttons as one joined group.
		ui.scope(|ui| {
			ui.spacing_mut().item_spacing.x = 2.0;
			let playing = state.is_playing();
			if icon_button(ui, Icon::ToStart, false)
				.on_hover_text(tr("Return to start (Home)"))
				.clicked()
			{
				state.seek(Tick::ZERO);
			}
			if icon_button(ui, Icon::Stop, false)
				.on_hover_text(tr("Stop"))
				.clicked()
			{
				state.stop();
			}
			if icon_button(ui, Icon::Play, playing)
				.on_hover_text(tr("Play / stop (Space)"))
				.clicked()
			{
				state.toggle_play();
			}
			record_button(ui, state);
		});
		ui.add_space(GROUP_GAP);
		labeled(ui, layout.captions.then_some(tr("Mode")), |ui| {
			mode_switch(ui, state, layout.long_mode)
		});
		ui.add_space(GROUP_GAP);
		loop_button(ui, state, layout.loop_label);
		if !layout.modes_folded {
			metronome(ui, state);
			labeled(ui, Some(tr("Count-in")), |ui| count_in(ui, state));
		}
		ui.add_space(GROUP_GAP);
		position_display(ui, state, layout);
		tempo(ui, state, layout.captions);
		time_signature(ui, state, layout.captions);
		ui.add_space(GROUP_GAP);
		if layout.modes_folded {
			modes_menu(ui, state);
		} else {
			labeled(ui, Some(tr("MIDI record")), |ui| midi_record(ui, state));
			labeled(ui, Some(tr("Punch")), |ui| punch(ui, state));
			labeled(ui, Some(tr("Automation")), |ui| automation_mode(ui, state));
			labeled(ui, Some(tr("Follow")), |ui| {
				super::arrange::follow_menu(ui, state)
			});
		}

		ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
			ui.add_space(6.0);
			let levels = state.meters.get(None);
			let (r, resp) = ui.allocate_exact_size(vec2(layout.meter, 18.0), Sense::click());
			paint_horizontal_meter(ui, r, levels);
			if resp
				.on_hover_text(tr("Master level (click to reset the clip lamp)"))
				.clicked()
			{
				state.meters.reset_clip(None);
			}
			ui.add_space(8.0);
			let (text, color) = match &state.audio_status {
				AudioStatus::Ready { sample_rate } => (
					format!("{:.1} kHz", *sample_rate as f32 / 1000.0),
					theme::PLAYING,
				),
				AudioStatus::Starting => (tr("Starting audio…").to_owned(), theme::TEXT_DIM),
				AudioStatus::Unavailable(_) => (tr("No audio device").to_owned(), theme::ERROR),
				AudioStatus::Off => (tr("Audio off").to_owned(), theme::WARNING),
			};
			let r = if layout.audio_text {
				ui.label(RichText::new(&text).font(theme::mono(11.0)).color(color))
			} else {
				let (dot, r) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
				ui.painter().circle_filled(dot.center(), 3.5, color);
				r.on_hover_text(&text)
			};
			if layout.audio_text {
				let (dot, _) = ui.allocate_exact_size(vec2(8.0, 8.0), Sense::hover());
				ui.painter().circle_filled(dot.center(), 3.5, color);
			}
			if let AudioStatus::Unavailable(e) = &state.audio_status {
				r.on_hover_text(e);
			}
			xrun_indicator(ui, state);
		});
	});
}

/// Narrow windows (design 12a): Click, Count-in, MIDI record, Punch, Automation and Follow
/// in one popover; the dot says one of them is not at its default.
fn modes_menu(ui: &mut Ui, state: &mut AppState) {
	use crate::state::autowrite::WriteMode;
	use crate::state::layout::FollowMode;
	let s = state.record.settings;
	let changed = s.metronome.on_play
		|| s.metronome.on_record
		|| s.count_in_bars != 0
		|| s.punch
		|| state.autowrite.mode != WriteMode::Off
		|| state.view.follow != FollowMode::Page;
	let font = theme::medium(12.0);
	let text = ui
		.painter()
		.layout_no_wrap(tr("Modes").to_owned(), font, theme::TEXT);
	let width = text.size().x + 38.0;
	let (rect, resp) = ui.allocate_exact_size(vec2(width, BUTTON.y), Sense::click());
	resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, tr("Modes")));
	let p = ui.painter();
	let open = egui::Popup::is_id_open(ui.ctx(), Id::new("transport-modes"));
	p.rect_filled(
		rect,
		CornerRadius::same(theme::RADIUS_MD),
		if open || resp.hovered() {
			theme::BG_HOVER
		} else {
			theme::BG_WIDGET
		},
	);
	let y = rect.center().y;
	let ts = text.size();
	p.galley(pos2(rect.left() + 10.0, y - ts.y * 0.5), text, theme::TEXT);
	let x = rect.left() + 10.0 + ts.x + 8.0;
	if changed {
		p.circle_filled(pos2(x, y), 3.0, theme::PATTERN);
	}
	p.text(
		pos2(x + 10.0, y),
		Align2::CENTER_CENTER,
		"▾",
		FontId::proportional(11.0),
		theme::TEXT_DIM,
	);
	let resp = resp.on_hover_text(tr(
		"Click, count-in, MIDI recording, punch, automation and follow (the dot: one of them is \
		 on)",
	));
	egui::Popup::menu(&resp)
		.id(Id::new("transport-modes"))
		.close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
		.show(|ui| {
			ui.set_min_width(230.0);
			egui::Grid::new("transport-modes-grid")
				.num_columns(2)
				.spacing(vec2(12.0, 8.0))
				.show(ui, |ui| {
					let row = |ui: &mut Ui, label: &str| {
						ui.label(
							RichText::new(label)
								.font(theme::small())
								.color(theme::TEXT_DIM),
						);
					};
					row(ui, tr("Click"));
					metronome(ui, state);
					ui.end_row();
					row(ui, tr("Count-in"));
					count_in(ui, state);
					ui.end_row();
					row(ui, tr("MIDI record"));
					midi_record(ui, state);
					ui.end_row();
					row(ui, tr("Punch"));
					punch(ui, state);
					ui.end_row();
					row(ui, tr("Automation"));
					automation_mode(ui, state);
					ui.end_row();
					row(ui, tr("Follow"));
					super::arrange::follow_menu(ui, state);
					ui.end_row();
				});
		});
}

/// A caption above `add`: dropdowns and fields read "label above, value inside".
/// `None` (narrow windows): the control alone.
fn labeled<R>(ui: &mut Ui, label: Option<&str>, add: impl FnOnce(&mut Ui) -> R) -> R {
	let Some(label) = label else {
		return add(ui);
	};
	ui.vertical(|ui| {
		ui.spacing_mut().item_spacing.y = 1.0;
		ui.label(
			RichText::new(label)
				.font(FontId::new(10.5, FontFamily::Proportional))
				.color(theme::TEXT_FAINT),
		);
		add(ui)
	})
	.inner
}

/// A toggle with an icon and a word (and an optional dim detail), on = the state's soft
/// fill, outline and color. `min_width` 0 = fit the text.
fn state_button(
	ui: &mut Ui,
	on: bool,
	icon: &str,
	label: &str,
	detail: Option<&str>,
	color: egui::Color32,
	min_width: f32,
) -> egui::Response {
	let font = theme::medium(12.0);
	let fg = if on { color } else { theme::TEXT };
	let text = ui
		.painter()
		.layout_no_wrap(label.to_owned(), font.clone(), fg);
	let detail = detail.map(|d| {
		ui.painter()
			.layout_no_wrap(d.to_owned(), theme::small(), theme::TEXT_FAINT)
	});
	let detail_w = detail.as_ref().map_or(0.0, |g| g.size().x + 5.0);
	let width = if label.is_empty() {
		BUTTON.x
	} else {
		(28.0 + text.size().x + detail_w + 10.0).max(min_width)
	};
	let (rect, resp) = ui.allocate_exact_size(vec2(width, BUTTON.y), Sense::click());
	let p = ui.painter();
	let r = CornerRadius::same(theme::RADIUS_MD);
	let bg = if on {
		theme::mix(theme::BG_PANEL, color, 0.2)
	} else if resp.hovered() {
		theme::BG_HOVER
	} else {
		theme::BG_WIDGET
	};
	p.rect_filled(rect, r, bg);
	if on {
		p.rect_stroke(
			rect,
			r,
			Stroke::new(1.0, color.gamma_multiply(0.5)),
			StrokeKind::Inside,
		);
	}
	let y = rect.center().y;
	let icon_x = if label.is_empty() {
		rect.center().x
	} else {
		rect.left() + 16.0
	};
	p.text(
		pos2(icon_x, y),
		Align2::CENTER_CENTER,
		icon,
		FontId::proportional(15.0),
		if on { color } else { theme::TEXT_DIM },
	);
	let x = rect.left() + 28.0;
	let text_size = text.size();
	p.galley(pos2(x, y - text_size.y * 0.5), text, fg);
	if let Some(d) = detail {
		let h = d.size().y;
		p.galley(
			pos2(x + text_size.x + 5.0, y - h * 0.5),
			d,
			theme::TEXT_FAINT,
		);
	}
	resp.widget_info(|| {
		egui::WidgetInfo::selected(
			egui::WidgetType::Checkbox,
			true,
			on,
			if label.is_empty() { icon } else { label },
		)
	});
	resp
}

/// Dropouts since the last reset (audio device under/overruns, late plugin blocks):
/// hidden while there are none, amber for a few seconds after a new one; the tooltip
/// says what dropped out, a click resets the count.
fn xrun_indicator(ui: &mut Ui, state: &mut AppState) {
	let count = state.xruns.count();
	if count == 0 {
		return;
	}
	ui.add_space(6.0);
	let now = std::time::Instant::now();
	let recent = state.xruns.recent(now);
	let text = format!("⚠ {count}");
	let galley = ui.painter().layout_no_wrap(
		text.clone(),
		theme::mono(10.0),
		if recent {
			theme::BG_DEEP
		} else {
			theme::WARNING
		},
	);
	let size = vec2(galley.size().x + 10.0, 16.0);
	let (r, resp) = ui.allocate_exact_size(size, Sense::click());
	resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, tr("Dropouts")));
	let p = ui.painter();
	if recent {
		p.rect_filled(r, CornerRadius::same(3), theme::WARNING);
		ui.ctx().request_repaint_after(crate::state::xruns::RECENT);
	} else {
		p.rect_stroke(
			r,
			CornerRadius::same(3),
			Stroke::new(1.0, theme::WARNING.gamma_multiply(0.7)),
			StrokeKind::Inside,
		);
	}
	p.galley(r.center() - galley.size() * 0.5, galley, theme::WARNING);
	let mut tip = state.xruns.lines().join("\n");
	tip.push_str(tr("\nClick to reset the count"));
	if resp.on_hover_text(tip).clicked() {
		state.xruns.reset();
	}
}

/// "Pattern | Song" segmented switch (L).
fn mode_switch(ui: &mut Ui, state: &mut AppState, long: bool) {
	use crate::state::layout::transport_widths::{MODE_LONG, MODE_SHORT};
	let mode = state.view.transport.mode;
	let w = if long { MODE_LONG } else { MODE_SHORT };
	let (rect, _) = ui.allocate_exact_size(vec2(w, FIELD), Sense::hover());
	ui.painter()
		.rect_filled(rect, CornerRadius::same(theme::RADIUS_MD), theme::BG_DEEP);
	let half = rect.width() / 2.0;
	for (i, (m, label, tip)) in [
		(
			PlayMode::Pattern,
			tr("Pattern"),
			tr("Pattern mode (L): Play loops the clip open in the piano roll, heard alone"),
		),
		(
			PlayMode::Song,
			tr("Song"),
			tr("Song mode (L): Play runs the whole arrangement"),
		),
	]
	.into_iter()
	.enumerate()
	{
		let r = Rect::from_min_size(
			pos2(rect.left() + i as f32 * half, rect.top()),
			vec2(half, rect.height()),
		)
		.shrink(2.0);
		let resp = ui
			.interact(r, Id::new(("play-mode", label)), Sense::click())
			.on_hover_text(tip);
		let on = mode == m;
		// Song on = a raised segment; Pattern on = its violet, so the mode reads at a glance.
		let (bg, fg) = match (on, m) {
			(true, PlayMode::Pattern) => (theme::PATTERN_SOFT, theme::PATTERN),
			(true, PlayMode::Song) => (theme::BG_HOVER, theme::TEXT),
			(false, _) if resp.hovered() => (theme::BG_WIDGET, theme::TEXT),
			(false, _) => (theme::BG_DEEP, theme::TEXT_DIM),
		};
		ui.painter()
			.rect_filled(r, CornerRadius::same(theme::RADIUS), bg);
		ui.painter().text(
			r.center(),
			Align2::CENTER_CENTER,
			if long || m == PlayMode::Song {
				label
			} else {
				tr("Pat")
			},
			theme::medium(11.5),
			fg,
		);
		if resp.clicked() {
			state.set_play_mode(m);
		}
	}
}

/// Loop region on/off (Ctrl+L).
fn loop_button(ui: &mut Ui, state: &mut AppState, label: bool) {
	let on = state.view.transport.loop_on;
	if state_button(
		ui,
		on,
		"⟲",
		if label { tr("Loop") } else { "" },
		None,
		theme::LOOP,
		0.0,
	)
	.on_hover_text(tr(
		"Loop the region marked on the arrange ruler (Ctrl+L); drag the ruler's top \
			 half to set it",
	))
	.clicked()
	{
		state.toggle_loop();
	}
}

/// The red record button: filled solid while actually recording; mode (R) on its own,
/// with nothing recording, only dims the dot and outlines the button — distinct from an
/// active recording, so "lit" never means "stuck".
fn record_button(ui: &mut Ui, state: &mut AppState) {
	let (rect, resp) = ui.allocate_exact_size(BUTTON, Sense::click());
	resp.widget_info(|| {
		egui::WidgetInfo::labeled(egui::WidgetType::Button, true, tr("Record (Ctrl+R)"))
	});
	let recording = state.is_recording();
	let mode_only = state.record.mode && !recording;
	let p = ui.painter();
	let r = CornerRadius::same(theme::RADIUS_MD);
	let bg = if recording {
		theme::RECORD_SOFT
	} else if resp.hovered() {
		theme::BG_HOVER
	} else {
		theme::BG_WIDGET
	};
	p.rect_filled(rect, r, bg);
	if mode_only || recording {
		p.rect_stroke(rect, r, Stroke::new(1.0, theme::RECORD), StrokeKind::Inside);
	}
	let c = rect.center();
	// Mode-only is a dimmer dot (armed, not firing); an active recording is full red
	// with a ring around it.
	let dot = if mode_only {
		theme::RECORD.gamma_multiply(0.55)
	} else {
		theme::RECORD
	};
	p.circle_filled(c, 5.5, dot);
	if recording {
		p.circle_stroke(c, 8.0, Stroke::new(1.0, theme::RECORD));
	}
	let resp = resp.on_hover_text(if recording {
		tr("Stop recording (Ctrl+R)")
	} else if mode_only {
		tr("Record mode is on; click to turn it off (R). Play would start recording")
	} else {
		tr("Record the armed tracks (Ctrl+R; again to stop). R: record mode, so Play records")
	});
	if resp.clicked() {
		state.toggle_record();
	}
	resp.context_menu(|ui| {
		if crate::ui::widgets::checkbox(
			ui,
			&mut state.record.mode,
			tr("Record mode (R): Play records"),
		)
		.clicked()
		{
			ui.close();
		}
	});
}

/// Metronome: the button switches the click on/off for playing and recording alike (the
/// menu can still limit it to one of them, which the button then says).
fn metronome(ui: &mut Ui, state: &mut AppState) {
	let mut s = state.record.settings;
	let on = s.metronome.on_play || s.metronome.on_record;
	let detail = match (s.metronome.on_play, s.metronome.on_record) {
		(true, false) => Some(tr("on play")),
		(false, true) => Some(tr("on rec")),
		_ => None,
	};
	let resp = state_button(ui, on, "♩", tr("Click"), detail, theme::ACCENT, 0.0);
	resp.widget_info(|| {
		egui::WidgetInfo::selected(egui::WidgetType::Checkbox, true, on, tr("Metronome"))
	});
	let resp = resp.on_hover_text(tr(
		"Metronome on/off (right-click: only while playing / recording, level)",
	));
	if resp.clicked() {
		s.metronome.on_play = !on;
		s.metronome.on_record = !on;
	}
	resp.context_menu(|ui| {
		crate::ui::widgets::checkbox(ui, &mut s.metronome.on_play, tr("Click while playing"));
		crate::ui::widgets::checkbox(
			ui,
			&mut s.metronome.on_record,
			tr("Click while recording / counting in"),
		);
		ui.add(
			egui::Slider::new(&mut s.metronome.level_db, -40.0..=0.0)
				.text(tr("Level"))
				.suffix(" dB"),
		);
	});
	state.set_record_settings(s);
}

fn count_in(ui: &mut Ui, state: &mut AppState) {
	let mut s = state.record.settings;
	egui::ComboBox::from_id_salt("count-in")
		.width(64.0)
		.selected_text(match s.count_in_bars {
			0 => tr("Off").to_owned(),
			1 => "1 bar".to_owned(),
			n => trf("{n} bars", &[("n", &n)]),
		})
		.show_ui(ui, |ui| {
			for n in crate::state::record::COUNT_IN_CHOICES {
				let label = match n {
					0 => tr("None").to_owned(),
					1 => "1 bar".to_owned(),
					n => trf("{n} bars", &[("n", &n)]),
				};
				ui.selectable_value(&mut s.count_in_bars, n, label);
			}
		})
		.response
		.on_hover_text(tr("Metronome bars before recording starts"));
	state.set_record_settings(s);
}

fn punch(ui: &mut Ui, state: &mut AppState) {
	let mut s = state.record.settings;
	let label = if s.punch { tr("On") } else { tr("Off") };
	if widgets::toggle(ui, s.punch, label, theme::RECORD, vec2(40.0, FIELD))
		.on_hover_text(tr(
			"Punch in/out: record only inside the loop region (with the loop on: a take per \
			 pass)",
		))
		.clicked()
	{
		s.punch = !s.punch;
	}
	state.set_record_settings(s);
}

/// MIDI recording options; the button names the current mode.
fn midi_record(ui: &mut Ui, state: &mut AppState) {
	let mut s = state.record.settings;
	let mode = if !s.midi_merge {
		tr("New clip")
	} else if s.midi_overdub {
		tr("Overdub")
	} else {
		tr("Replace")
	};
	ui.menu_button(mode, |ui| {
		ui.set_min_width(240.0);
		crate::ui::widgets::checkbox(
			ui,
			&mut s.midi_merge,
			tr("Merge into the clip under the playhead"),
		);
		ui.add_enabled_ui(s.midi_merge, |ui| {
			crate::ui::widgets::checkbox(
				ui,
				&mut s.midi_overdub,
				tr("Overdub (keep existing notes)"),
			);
		});
		crate::ui::widgets::checkbox(
			ui,
			&mut s.midi_takes,
			tr("Loop recording: a new take per pass"),
		);
		ui.horizontal(|ui| {
			ui.label(tr("Input quantize"));
			egui::ComboBox::from_id_salt("midi-quantize")
				.selected_text(s.quantize.name())
				.show_ui(ui, |ui| {
					for q in crate::state::record::InputQuantize::ALL {
						ui.selectable_value(&mut s.quantize, q, q.name());
					}
				});
		});
	})
	.response
	.on_hover_text(tr(
		"MIDI recording: merge / overdub, takes per loop pass, input quantize",
	));
	state.set_record_settings(s);
}

/// Automation mode: Read (play lanes) / Touch / Latch (moving a control while playing
/// writes its lane). Click cycles, right-click picks.
fn automation_mode(ui: &mut Ui, state: &mut AppState) {
	use crate::state::autowrite::WriteMode;
	let mode = state.autowrite.mode;
	let writing = mode != WriteMode::Off;
	let color = if writing {
		theme::AUTOMATION_WRITE
	} else {
		theme::AUTOMATION
	};
	let label = format!("● {}", mode.name());
	let resp = widgets::toggle(ui, writing, &label, color, vec2(64.0, 22.0));
	resp.widget_info(|| {
		egui::WidgetInfo::labeled(
			egui::WidgetType::Button,
			true,
			trf("Automation: {name}", &[("name", &mode.name())]),
		)
	});
	let resp = resp.on_hover_text(tr(
		"Automation mode. Read: lanes play. Touch: moving a fader, knob or plugin control \
		 while playing writes its lane until you let go. Latch: keeps writing the last value \
		 until playback stops. Each playback pass is one undo step. Click cycles, right-click \
		 picks.",
	));
	if resp.clicked() {
		state.set_write_mode(match mode {
			WriteMode::Off => WriteMode::Touch,
			WriteMode::Touch => WriteMode::Latch,
			WriteMode::Latch => WriteMode::Off,
		});
	}
	resp.context_menu(|ui| {
		for m in [WriteMode::Off, WriteMode::Touch, WriteMode::Latch] {
			if ui.selectable_label(m == mode, m.name()).clicked() {
				state.set_write_mode(m);
				ui.close();
			}
		}
	});
}

#[derive(Clone, Copy)]
enum Icon {
	ToStart,
	Stop,
	Play,
}

fn icon_button(ui: &mut Ui, icon: Icon, active: bool) -> egui::Response {
	let (rect, resp) = ui.allocate_exact_size(BUTTON, Sense::click());
	let p = ui.painter();
	let r = CornerRadius::same(theme::RADIUS_MD);
	// Playing = the green soft fill with a green outline, readable from across the room.
	let bg = if active {
		theme::PLAYING_SOFT
	} else if resp.hovered() {
		theme::BG_HOVER
	} else {
		theme::BG_WIDGET
	};
	p.rect_filled(rect, r, bg);
	if active {
		p.rect_stroke(
			rect,
			r,
			Stroke::new(1.0, theme::PLAYING),
			StrokeKind::Inside,
		);
	}
	let c = rect.center();
	let fg = if active { theme::PLAYING } else { theme::TEXT };
	match icon {
		Icon::Play => {
			p.add(Shape::convex_polygon(
				vec![
					pos2(c.x - 4.5, c.y - 6.5),
					pos2(c.x + 6.5, c.y),
					pos2(c.x - 4.5, c.y + 6.5),
				],
				fg,
				Stroke::NONE,
			));
		}
		Icon::Stop => {
			p.rect_filled(
				Rect::from_center_size(c, vec2(10.0, 10.0)),
				CornerRadius::same(1),
				fg,
			);
		}
		Icon::ToStart => {
			p.rect_filled(
				Rect::from_center_size(pos2(c.x - 5.5, c.y), vec2(2.0, 11.0)),
				0.0,
				fg,
			);
			p.add(Shape::convex_polygon(
				vec![
					pos2(c.x + 5.5, c.y - 5.5),
					pos2(c.x + 5.5, c.y + 5.5),
					pos2(c.x - 3.5, c.y),
				],
				fg,
				Stroke::NONE,
			));
		}
	}
	resp
}

fn position_display(
	ui: &mut Ui,
	state: &mut AppState,
	layout: crate::state::layout::TransportLayout,
) {
	let pos = state.position();
	let project = state.project();
	let mut bbt = format_bbt(pos, project.meter());
	let mut time = format_time(pos, &project.tempo);
	// Counting in: the bars before the record start (-2.1, -1.4...), the playhead waits.
	if let Some(c) = state.count_in() {
		let (bar, beat) = c.bar_beat();
		bbt = format!("{bar}.{beat}");
		time = tr("COUNT-IN").into();
		ui.ctx().request_repaint();
	}
	let recording = state.is_recording();
	let (rect, _) = ui.allocate_exact_size(vec2(150.0, 36.0), Sense::hover());
	let p = ui.painter();
	let r = CornerRadius::same(theme::RADIUS_MD);
	p.rect_filled(rect, r, theme::BG_DEEP);
	p.rect_stroke(
		rect,
		r,
		Stroke::new(
			1.0,
			if recording {
				theme::RECORD
			} else {
				theme::BORDER
			},
		),
		StrokeKind::Inside,
	);
	p.text(
		rect.center(),
		Align2::CENTER_CENTER,
		bbt,
		theme::mono_medium(24.0),
		if recording {
			theme::RECORD
		} else {
			theme::TEXT
		},
	);
	if !layout.time {
		return;
	}
	labeled(ui, layout.captions.then_some(tr("Time")), |ui| {
		ui.add_sized(
			[78.0, FIELD],
			egui::Label::new(
				RichText::new(time)
					.font(theme::mono(13.0))
					.color(theme::TEXT_DIM),
			),
		)
	});
}

fn tempo(ui: &mut Ui, state: &mut AppState, captions: bool) {
	// The tempo in effect at the playhead; editing it changes that segment.
	let bpm = state.project().tempo.bpm_at(state.position()) as f32;
	labeled(ui, captions.then_some(tr("Tempo")), |ui| {
		ui.horizontal(|ui| {
			ui.spacing_mut().item_spacing.x = 2.0;
			let (g, _) = widgets::edit_value(ui, Id::new("tempo"), bpm, |ui, v| {
				ui.add_sized(
					[54.0, FIELD],
					egui::DragValue::new(v)
						.range(20.0..=400.0)
						.speed(0.25)
						.fixed_decimals(2),
				)
			});
			if let Gesture::Commit(v) = g
				&& (f64::from(v) - f64::from(bpm)).abs() > 1e-6
			{
				state.set_tempo_at_playhead(f64::from(v));
			}
			let tempo_lane = state.view.automation.tempo;
			if widgets::toggle(ui, tempo_lane, "♩", theme::TEMPO, vec2(20.0, FIELD))
				.on_hover_text(tr("Show the tempo and time signature lanes"))
				.clicked()
			{
				state.view.automation.tempo = !tempo_lane;
			}
		});
	});
}

/// The time signature in effect at the playhead; editing it changes that segment (the
/// song's first signature, or the change governing the playhead).
fn time_signature(ui: &mut Ui, state: &mut AppState, captions: bool) {
	let ts = state.project().meter().signature_at(state.position());
	let mut next = None;
	labeled(ui, captions.then_some(tr("Signature")), |ui| {
		ui.horizontal(|ui| {
			ui.spacing_mut().item_spacing.x = 2.0;
			let (g, _) =
				widgets::edit_value(ui, Id::new("ts-num"), ts.numerator as f32, |ui, v| {
					ui.add_sized(
						[26.0, FIELD],
						egui::DragValue::new(v)
							.range(1.0..=32.0)
							.speed(0.1)
							.max_decimals(0),
					)
				});
			if let Gesture::Commit(v) = g {
				next = Some(TimeSignature {
					numerator: v.round() as u32,
					denominator: ts.denominator,
				});
			}
			ui.label(RichText::new("/").color(theme::TEXT_FAINT));
			egui::ComboBox::from_id_salt("ts-den")
				.width(34.0)
				.selected_text(ts.denominator.to_string())
				.show_ui(ui, |ui| {
					for d in [2u32, 4, 8, 16] {
						if ui
							.selectable_label(d == ts.denominator, d.to_string())
							.clicked()
						{
							next = Some(TimeSignature {
								numerator: ts.numerator,
								denominator: d,
							});
						}
					}
				});
		});
	});
	if let Some(n) = next
		&& n != ts
		&& let Some(cmd) = crate::state::meter::set_at(state.project(), state.position(), n)
	{
		state.apply(cmd);
	}
}

fn paint_horizontal_meter(ui: &Ui, rect: egui::Rect, levels: crate::state::meters::MeterLevels) {
	let p = ui.painter();
	p.rect_filled(rect, CornerRadius::same(2), theme::METER_BG);
	let lamp = egui::Rect::from_min_size(
		pos2(rect.right() - 5.0, rect.top()),
		vec2(5.0, rect.height()),
	);
	p.rect_filled(
		lamp,
		0.0,
		if levels.clipped {
			theme::METER_HIGH
		} else {
			theme::BG_WIDGET
		},
	);
	let body = egui::Rect::from_min_max(rect.min, pos2(lamp.left() - 2.0, rect.bottom()));
	let rows = [0usize, 1];
	let h = (body.height() - 3.0) / 2.0;
	for c in rows {
		let row = egui::Rect::from_min_size(
			pos2(body.left(), body.top() + 1.0 + c as f32 * (h + 1.0)),
			vec2(body.width(), h),
		);
		let x = |db: f32| row.left() + widgets::db_to_meter(db) * row.width();
		let peak = levels.peak_db[c];
		p.rect_filled(
			egui::Rect::from_x_y_ranges(row.left()..=x(peak), row.y_range()),
			0.0,
			theme::METER_LOW.gamma_multiply(0.4),
		);
		let rms = levels.rms_db[c];
		let color = if rms > -3.0 {
			theme::METER_HIGH
		} else if rms > -12.0 {
			theme::METER_MID
		} else {
			theme::METER_LOW
		};
		p.rect_filled(
			egui::Rect::from_x_y_ranges(row.left()..=x(rms), row.y_range()),
			0.0,
			color,
		);
	}
	let hold = body.left() + widgets::db_to_meter(levels.hold_db) * body.width();
	if levels.hold_db > crate::state::meters::FLOOR_DB {
		p.vline(hold, body.y_range(), Stroke::new(1.5, theme::TEXT));
	}
	let zero = body.left() + widgets::db_to_meter(0.0) * body.width();
	p.vline(zero, body.y_range(), Stroke::new(1.0, theme::BG_DEEP));
}
