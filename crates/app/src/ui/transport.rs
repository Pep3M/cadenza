//! Transport bar: play/stop, record (with record mode), return to start, metronome,
//! count-in and punch, position, tempo, time signature, master meter and audio status.

use cadenza_core::{Tick, TimeSignature};
use egui::{
	Align, Align2, CornerRadius, Id, Layout, RichText, Sense, Shape, Stroke, StrokeKind, Ui, pos2,
	vec2,
};

use crate::state::timeline::{format_bbt, format_time};
use crate::state::transport::PlayMode;
use crate::state::{AppState, AudioStatus};
use crate::theme;
use crate::ui::widgets::{self, Gesture};

pub fn show(ui: &mut Ui, state: &mut AppState) {
	ui.horizontal_centered(|ui| {
		ui.spacing_mut().item_spacing.x = 6.0;
		ui.add_space(4.0);
		let playing = state.is_playing();
		if icon_button(ui, Icon::ToStart, false)
			.on_hover_text("Return to start (Home)")
			.clicked()
		{
			state.seek(Tick::ZERO);
		}
		if icon_button(ui, Icon::Stop, false)
			.on_hover_text("Stop")
			.clicked()
		{
			state.stop();
		}
		if icon_button(ui, Icon::Play, playing)
			.on_hover_text("Play / stop (Space)")
			.clicked()
		{
			state.toggle_play();
		}
		record_button(ui, state);
		ui.add_space(4.0);
		mode_switch(ui, state);
		loop_button(ui, state);
		record_controls(ui, state);
		automation_mode(ui, state);
		ui.add_space(8.0);
		position_display(ui, state);
		ui.add_space(8.0);
		tempo(ui, state);
		time_signature(ui, state);
		ui.add_space(8.0);
		super::arrange::follow_menu(ui, state);

		ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
			ui.add_space(6.0);
			let levels = state.meters.get(None);
			let (r, resp) = ui.allocate_exact_size(vec2(120.0, 22.0), Sense::click());
			paint_horizontal_meter(ui, r, levels);
			if resp
				.on_hover_text("Master level (click to reset the clip lamp)")
				.clicked()
			{
				state.meters.reset_clip(None);
			}
			ui.label(
				RichText::new("MASTER")
					.font(theme::small())
					.color(theme::TEXT_FAINT),
			);
			ui.add_space(10.0);
			let (text, color) = match &state.audio_status {
				AudioStatus::Ready { sample_rate } => (
					format!("{:.1} kHz", *sample_rate as f32 / 1000.0),
					theme::PLAYING,
				),
				AudioStatus::Starting => ("starting audio…".to_owned(), theme::TEXT_DIM),
				AudioStatus::Unavailable(_) => ("no audio device".to_owned(), theme::ERROR),
				AudioStatus::Off => ("audio off".to_owned(), theme::TEXT_FAINT),
			};
			let r = ui.label(RichText::new(text).font(theme::small()).color(color));
			let (dot, _) = ui.allocate_exact_size(vec2(8.0, 8.0), Sense::hover());
			ui.painter().circle_filled(dot.center(), 3.5, color);
			if let AudioStatus::Unavailable(e) = &state.audio_status {
				r.on_hover_text(e);
			}
			xrun_indicator(ui, state);
		});
	});
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
	resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Dropouts"));
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
	tip.push_str("\nClick to reset the count");
	if resp.on_hover_text(tip).clicked() {
		state.xruns.reset();
	}
}

/// FL Studio-like "PAT | SONG" switch (L).
fn mode_switch(ui: &mut Ui, state: &mut AppState) {
	let mode = state.view.transport.mode;
	let (rect, _) = ui.allocate_exact_size(vec2(92.0, 28.0), Sense::hover());
	let half = rect.width() / 2.0;
	for (i, (m, label, tip)) in [
		(
			PlayMode::Pattern,
			"PAT",
			"Pattern mode (L): Play loops the clip open in the piano roll, heard alone",
		),
		(
			PlayMode::Song,
			"SONG",
			"Song mode (L): Play runs the whole arrangement",
		),
	]
	.into_iter()
	.enumerate()
	{
		let r = egui::Rect::from_min_size(
			pos2(rect.left() + i as f32 * half, rect.top()),
			vec2(half, rect.height()),
		);
		let resp = ui
			.interact(r, Id::new(("play-mode", label)), Sense::click())
			.on_hover_text(tip);
		let on = mode == m;
		let color = match m {
			PlayMode::Pattern => theme::PATTERN,
			PlayMode::Song => theme::PLAYING,
		};
		let corner = if i == 0 {
			CornerRadius {
				nw: theme::RADIUS,
				sw: theme::RADIUS,
				..CornerRadius::ZERO
			}
		} else {
			CornerRadius {
				ne: theme::RADIUS,
				se: theme::RADIUS,
				..CornerRadius::ZERO
			}
		};
		let bg = if on {
			theme::mix(color, theme::BG_DEEP, 0.6)
		} else if resp.hovered() {
			theme::BG_HOVER
		} else {
			theme::BG_WIDGET
		};
		ui.painter().rect_filled(r, corner, bg);
		ui.painter().text(
			r.center(),
			Align2::CENTER_CENTER,
			label,
			theme::mono(11.0),
			if on { color } else { theme::TEXT_DIM },
		);
		if resp.clicked() {
			state.set_play_mode(m);
		}
	}
	ui.painter().vline(
		rect.center().x,
		rect.y_range(),
		Stroke::new(1.0, theme::BG_DEEP),
	);
}

/// Loop region on/off (Ctrl+L).
fn loop_button(ui: &mut Ui, state: &mut AppState) {
	let on = state.view.transport.loop_on;
	let (rect, resp) = ui.allocate_exact_size(vec2(32.0, 28.0), Sense::click());
	let p = ui.painter();
	let bg = if on {
		theme::mix(theme::LOOP, theme::BG_DEEP, 0.55)
	} else if resp.hovered() {
		theme::BG_HOVER
	} else {
		theme::BG_WIDGET
	};
	p.rect_filled(rect, CornerRadius::same(theme::RADIUS), bg);
	p.text(
		rect.center(),
		Align2::CENTER_CENTER,
		"⟲",
		egui::FontId::proportional(17.0),
		if on { theme::LOOP } else { theme::TEXT },
	);
	if resp
		.on_hover_text(
			"Loop the region marked on the arrange ruler (Ctrl+L); drag the ruler's top \
			 half to set it",
		)
		.clicked()
	{
		state.toggle_loop();
	}
}

/// The red record button: filled while recording, outlined in record mode (R).
fn record_button(ui: &mut Ui, state: &mut AppState) {
	let (rect, resp) = ui.allocate_exact_size(vec2(32.0, 28.0), Sense::click());
	resp.widget_info(|| {
		egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Record (Ctrl+R)")
	});
	let recording = state.is_recording();
	let p = ui.painter();
	let bg = if recording {
		theme::mix(theme::RECORD, theme::BG_DEEP, 0.45)
	} else if resp.hovered() {
		theme::BG_HOVER
	} else {
		theme::BG_WIDGET
	};
	p.rect_filled(rect, CornerRadius::same(theme::RADIUS), bg);
	if state.record.mode && !recording {
		p.rect_stroke(
			rect.shrink(1.0),
			CornerRadius::same(theme::RADIUS),
			Stroke::new(1.5, theme::RECORD),
			egui::StrokeKind::Inside,
		);
	}
	let c = rect.center();
	p.circle_filled(c, 6.0, theme::RECORD);
	if recording {
		p.circle_stroke(c, 8.5, Stroke::new(1.0, theme::RECORD));
	}
	let resp = resp.on_hover_text(
		"Record the armed tracks (Ctrl+R; again to stop). R: record mode, so Play records",
	);
	if resp.clicked() {
		state.toggle_record();
	}
	resp.context_menu(|ui| {
		if ui
			.checkbox(&mut state.record.mode, "Record mode (R): Play records")
			.clicked()
		{
			ui.close();
		}
	});
}

/// Metronome toggle, count-in and punch, and the recording options menu.
fn record_controls(ui: &mut Ui, state: &mut AppState) {
	let mut s = state.record.settings;
	// Metronome: the button switches the click on/off for playing and recording alike
	// (the menu can still limit it to one of them).
	let (rect, resp) = ui.allocate_exact_size(vec2(32.0, 28.0), Sense::click());
	resp.widget_info(|| {
		egui::WidgetInfo::selected(
			egui::WidgetType::Checkbox,
			true,
			s.metronome.on_play || s.metronome.on_record,
			"Metronome",
		)
	});
	let on = s.metronome.on_play || s.metronome.on_record;
	let p = ui.painter();
	let bg = if on {
		theme::mix(theme::ACCENT, theme::BG_DEEP, 0.55)
	} else if resp.hovered() {
		theme::BG_HOVER
	} else {
		theme::BG_WIDGET
	};
	p.rect_filled(rect, CornerRadius::same(theme::RADIUS), bg);
	let c = rect.center();
	let fg = if on { theme::ACCENT } else { theme::TEXT };
	let body = [
		pos2(c.x - 6.0, c.y + 8.0),
		pos2(c.x - 2.5, c.y - 8.0),
		pos2(c.x + 2.5, c.y - 8.0),
		pos2(c.x + 6.0, c.y + 8.0),
	];
	p.add(Shape::closed_line(body.to_vec(), Stroke::new(1.3, fg)));
	p.line_segment(
		[pos2(c.x, c.y + 5.0), pos2(c.x + 5.0, c.y - 6.0)],
		Stroke::new(1.3, fg),
	);
	let resp =
		resp.on_hover_text("Metronome on/off (right-click: only while playing / recording, level)");
	if resp.clicked() {
		s.metronome.on_play = !on;
		s.metronome.on_record = !on;
	}
	resp.context_menu(|ui| {
		ui.checkbox(&mut s.metronome.on_play, "Click while playing");
		ui.checkbox(
			&mut s.metronome.on_record,
			"Click while recording / counting in",
		);
		ui.add(
			egui::Slider::new(&mut s.metronome.level_db, -40.0..=0.0)
				.text("Level")
				.suffix(" dB"),
		);
	});
	egui::ComboBox::from_id_salt("count-in")
		.width(84.0)
		.selected_text(match s.count_in_bars {
			0 => "No count-in".to_owned(),
			1 => "Count-in 1 bar".to_owned(),
			n => format!("Count-in {n} bars"),
		})
		.show_ui(ui, |ui| {
			for n in crate::state::record::COUNT_IN_CHOICES {
				let label = match n {
					0 => "None".to_owned(),
					1 => "1 bar".to_owned(),
					n => format!("{n} bars"),
				};
				ui.selectable_value(&mut s.count_in_bars, n, label);
			}
		})
		.response
		.on_hover_text("Metronome bars before recording starts");
	if widgets::toggle(ui, s.punch, "PUNCH", theme::RECORD, vec2(48.0, 22.0))
		.on_hover_text(
			"Punch in/out: record only inside the loop region (with the loop on: a take per 			 pass)",
		)
		.clicked()
	{
		s.punch = !s.punch;
	}
	ui.menu_button("MIDI rec", |ui| {
		ui.set_min_width(240.0);
		ui.checkbox(&mut s.midi_merge, "Merge into the clip under the playhead");
		ui.add_enabled_ui(s.midi_merge, |ui| {
			ui.checkbox(&mut s.midi_overdub, "Overdub (keep existing notes)");
		});
		ui.checkbox(&mut s.midi_takes, "Loop recording: a new take per pass");
		ui.horizontal(|ui| {
			ui.label("Input quantize");
			egui::ComboBox::from_id_salt("midi-quantize")
				.selected_text(s.quantize.name())
				.show_ui(ui, |ui| {
					for q in crate::state::record::InputQuantize::ALL {
						ui.selectable_value(&mut s.quantize, q, q.name());
					}
				});
		});
	});
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
	let resp = widgets::toggle(
		ui,
		writing,
		&format!("A: {}", mode.name()),
		color,
		vec2(64.0, 22.0),
	);
	let resp = resp.on_hover_text(
		"Automation mode. Read: lanes play. Touch: moving a fader, knob or plugin control 		 while playing writes its lane until you let go. Latch: keeps writing the last value 		 until playback stops. Each playback pass is one undo step. Click cycles, right-click 		 picks.",
	);
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
	let (rect, resp) = ui.allocate_exact_size(vec2(32.0, 28.0), Sense::click());
	let p = ui.painter();
	let bg = if active {
		theme::mix(theme::PLAYING, theme::BG_DEEP, 0.55)
	} else if resp.hovered() {
		theme::BG_HOVER
	} else {
		theme::BG_WIDGET
	};
	p.rect_filled(rect, CornerRadius::same(theme::RADIUS), bg);
	let c = rect.center();
	let fg = if active { theme::PLAYING } else { theme::TEXT };
	match icon {
		Icon::Play => {
			p.add(Shape::convex_polygon(
				vec![
					pos2(c.x - 5.0, c.y - 7.0),
					pos2(c.x + 7.0, c.y),
					pos2(c.x - 5.0, c.y + 7.0),
				],
				fg,
				Stroke::NONE,
			));
		}
		Icon::Stop => {
			p.rect_filled(
				egui::Rect::from_center_size(c, vec2(11.0, 11.0)),
				CornerRadius::same(1),
				fg,
			);
		}
		Icon::ToStart => {
			p.rect_filled(
				egui::Rect::from_center_size(pos2(c.x - 6.0, c.y), vec2(2.5, 12.0)),
				0.0,
				fg,
			);
			p.add(Shape::convex_polygon(
				vec![
					pos2(c.x + 6.0, c.y - 6.0),
					pos2(c.x + 6.0, c.y + 6.0),
					pos2(c.x - 4.0, c.y),
				],
				fg,
				Stroke::NONE,
			));
		}
	}
	resp
}

fn position_display(ui: &mut Ui, state: &mut AppState) {
	let pos = state.position();
	let project = state.project();
	let bbt = format_bbt(pos, project.meter());
	let time = format_time(pos, &project.tempo);
	let (rect, _) = ui.allocate_exact_size(vec2(210.0, 34.0), Sense::hover());
	let p = ui.painter();
	p.rect_filled(rect, CornerRadius::same(theme::RADIUS), theme::BG_DEEP);
	let recording = state.is_recording();
	if recording {
		p.rect_stroke(
			rect,
			CornerRadius::same(theme::RADIUS),
			Stroke::new(1.0, theme::RECORD),
			egui::StrokeKind::Inside,
		);
	}
	p.text(
		pos2(rect.left() + 10.0, rect.center().y),
		Align2::LEFT_CENTER,
		bbt,
		theme::mono(19.0),
		if recording {
			theme::RECORD
		} else {
			theme::ACCENT
		},
	);
	p.text(
		pos2(rect.right() - 8.0, rect.center().y),
		Align2::RIGHT_CENTER,
		time,
		theme::mono(11.5),
		theme::TEXT_DIM,
	);
}

fn tempo(ui: &mut Ui, state: &mut AppState) {
	// The tempo in effect at the playhead; editing it changes that segment.
	let bpm = state.project().tempo.bpm_at(state.position()) as f32;
	ui.label(
		RichText::new("BPM")
			.font(theme::small())
			.color(theme::TEXT_FAINT),
	);
	let (g, _) = widgets::edit_value(ui, Id::new("tempo"), bpm, |ui, v| {
		ui.add_sized(
			[58.0, 22.0],
			egui::DragValue::new(v)
				.range(20.0..=400.0)
				.speed(0.25)
				.max_decimals(2),
		)
	});
	if let Gesture::Commit(v) = g
		&& (f64::from(v) - f64::from(bpm)).abs() > 1e-6
	{
		state.set_tempo_at_playhead(f64::from(v));
	}
	let tempo_lane = state.view.automation.tempo;
	if ui
		.selectable_label(tempo_lane, "♩")
		.on_hover_text("Show the tempo and time signature lanes")
		.clicked()
	{
		state.view.automation.tempo = !tempo_lane;
	}
}

/// The time signature in effect at the playhead; editing it changes that segment (the
/// song's first signature, or the change governing the playhead).
fn time_signature(ui: &mut Ui, state: &mut AppState) {
	let ts = state.project().meter().signature_at(state.position());
	ui.label(
		RichText::new("TIME")
			.font(theme::small())
			.color(theme::TEXT_FAINT),
	);
	let (g, _) = widgets::edit_value(ui, Id::new("ts-num"), ts.numerator as f32, |ui, v| {
		ui.add_sized(
			[30.0, 22.0],
			egui::DragValue::new(v)
				.range(1.0..=32.0)
				.speed(0.1)
				.max_decimals(0),
		)
	});
	let mut next = None;
	if let Gesture::Commit(v) = g {
		next = Some(TimeSignature {
			numerator: v.round() as u32,
			denominator: ts.denominator,
		});
	}
	ui.label("/");
	egui::ComboBox::from_id_salt("ts-den")
		.width(38.0)
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
