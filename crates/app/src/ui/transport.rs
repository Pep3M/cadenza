//! Transport bar: play/stop, return to start, position, tempo, time signature, master
//! meter and audio status.

use cadenza_core::{Tick, TimeSignature};
use cadenza_model::Command;
use egui::{
	Align, Align2, CornerRadius, Id, Layout, RichText, Sense, Shape, Stroke, Ui, pos2, vec2,
};

use crate::state::timeline::{format_bbt, format_time};
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
		});
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
	let bbt = format_bbt(pos, project.time_signature);
	let time = format_time(pos, &project.tempo);
	let (rect, _) = ui.allocate_exact_size(vec2(210.0, 34.0), Sense::hover());
	let p = ui.painter();
	p.rect_filled(rect, CornerRadius::same(theme::RADIUS), theme::BG_DEEP);
	p.text(
		pos2(rect.left() + 10.0, rect.center().y),
		Align2::LEFT_CENTER,
		bbt,
		theme::mono(19.0),
		theme::ACCENT,
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
	let bpm = state.project().tempo.bpm_at(Tick::ZERO) as f32;
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
		state.apply(Command::SetTempo {
			tick: Tick::ZERO,
			bpm: f64::from(v),
		});
	}
}

fn time_signature(ui: &mut Ui, state: &mut AppState) {
	let ts = state.project().time_signature;
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
	{
		state.apply(Command::SetTimeSignature { time_signature: n });
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
