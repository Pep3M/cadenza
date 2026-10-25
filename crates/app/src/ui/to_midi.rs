//! "Convert to MIDI…" dialog of an audio clip (NE-3): options on the left, a live picture
//! of the notes Convert would write, one undo step.

use egui::{Align, Id, Layout, Rect, RichText, Sense, Stroke, Ui, pos2, vec2};

use cadenza_mcp::tools::notes::{Algorithm, ConvertMode, midi_options};
use cadenza_notes::midi::KeySnap;

use crate::i18n::{tr, trn};
use crate::state::AppState;
use crate::state::to_midi::{ConvertPhase, ConvertPreview, target_tracks};
use crate::theme::{self, close_glyph, dialog_header};
use crate::ui::widgets;

const GRIDS: [(f64, &str); 5] = [
	(0.0, "Off"),
	(1.0, "1/4"),
	(0.5, "1/8"),
	(0.25, "1/16"),
	(0.125, "1/32"),
];
const ROOTS: [&str; 12] = [
	"C", "C#", "D", "Eb", "E", "F", "F#", "G", "Ab", "A", "Bb", "B",
];

fn hint(ui: &mut Ui, text: &str) {
	ui.label(
		RichText::new(text)
			.font(theme::small())
			.color(theme::TEXT_FAINT),
	);
}

fn row(ui: &mut Ui, label: &str, body: impl FnOnce(&mut Ui)) {
	ui.horizontal(|ui| {
		ui.add_sized(
			vec2(110.0, 22.0),
			egui::Label::new(RichText::new(label).color(theme::TEXT_DIM)),
		);
		body(ui);
	});
	ui.add_space(4.0);
}

pub fn show(ctx: &egui::Context, state: &mut AppState) {
	let Some(c) = state.convert_to_midi() else {
		return;
	};
	let name = c.clip_name.clone();
	let preview = state.convert_preview();
	let mut close = false;
	let mut convert = false;
	let modal = egui::Modal::new(Id::new("to-midi-dialog"))
		.frame(theme::dialog_frame())
		.show(ctx, |ui| {
			ui.set_width(640.0);
			dialog_header(ui, tr("Convert to MIDI"), &name, |ui| {
				if close_glyph(ui) {
					close = true;
				}
			});
			let phase = state.convert_to_midi().map(|c| c.phase.clone());
			match phase {
				Some(ConvertPhase::Analyzing) => {
					ui.horizontal(|ui| {
						ui.spinner();
						ui.label(tr("Finding the notes…"));
					});
					ui.add_space(8.0);
				}
				Some(ConvertPhase::Failed(e)) => {
					ui.label(RichText::new(e).color(theme::WARNING));
					ui.add_space(8.0);
				}
				_ => {}
			}
			options(ui, state);
			ui.add_space(6.0);
			theme::section_label(ui, tr("Preview"));
			ui.add_space(4.0);
			match &preview {
				Some(Ok(p)) => preview_panel(ui, p),
				Some(Err(e)) => {
					ui.label(RichText::new(e).font(theme::small()).color(theme::WARNING));
				}
				None => hint(ui, tr("The notes appear here once the clip is analyzed.")),
			}
			ui.add_space(10.0);
			ui.separator();
			ui.add_space(8.0);
			ui.horizontal(|ui| {
				hint(ui, tr("One undo step. The audio clip stays as it is."));
				ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
					let ready = matches!(preview, Some(Ok(_)));
					ui.add_enabled_ui(ready, |ui| {
						if theme::primary_button(ui, tr("Convert")).clicked() {
							convert = true;
						}
					});
					if ui.button(tr("Cancel")).clicked() {
						close = true;
					}
				});
			});
		});
	if convert {
		state.apply_convert_to_midi();
	} else if close || modal.should_close() {
		state.cancel_convert_to_midi();
	}
}

fn options(ui: &mut Ui, state: &mut AppState) {
	let targets = target_tracks(state.project());
	let Some(c) = state.convert_to_midi_mut() else {
		return;
	};
	let effective = midi_options(&c.args).unwrap_or_default();
	row(ui, tr("Detection"), |ui| {
		let kinds = [
			(Algorithm::Auto, tr("Auto")),
			(Algorithm::Mono, tr("Melody (one note)")),
			(Algorithm::Poly, tr("Chords (polyphonic)")),
			(Algorithm::Percussive, tr("Drums")),
		];
		let labels: Vec<&str> = kinds.iter().map(|k| k.1).collect();
		let selected = kinds
			.iter()
			.position(|k| k.0 == c.args.algorithm)
			.unwrap_or(0);
		if let Some(i) = widgets::segmented_sized(ui, &labels, selected, 480.0) {
			c.args.algorithm = kinds[i].0;
		}
	});
	ui.label(
		RichText::new(tr(
			"Melody: voice, bass or lead, with exact timing and pitch bends. Chords: piano, guitar or keys, several notes at once (no bends). Drums: kick, snare, hats, toms and cymbals as drum notes. Auto picks drums or chords when the recording is mostly that",
		))
		.font(theme::small())
		.color(theme::TEXT_FAINT),
	);
	ui.add_space(4.0);
	row(ui, tr("Material"), |ui| {
		let modes = [
			tr("Recording (as played)"),
			tr("Humming (quantized, in key)"),
		];
		let selected = usize::from(c.args.mode == ConvertMode::Hum);
		if let Some(i) = widgets::segmented_sized(ui, &modes, selected, 420.0) {
			c.set_mode(if i == 1 {
				ConvertMode::Hum
			} else {
				ConvertMode::Audio
			});
		}
	});
	row(ui, tr("Quantize"), |ui| {
		let current = effective.quantize.unwrap_or(0.0);
		let labels: Vec<&str> = GRIDS
			.iter()
			.map(|(_, l)| if *l == "Off" { tr("Off") } else { *l })
			.collect();
		let selected = GRIDS
			.iter()
			.position(|(g, _)| (g - current).abs() < 1e-9)
			.unwrap_or(0);
		if let Some(i) = widgets::segmented_sized(ui, &labels, selected, 260.0) {
			c.args.quantize_beats = Some(GRIDS[i].0);
		}
		if current > 0.0 {
			let mut strength = (effective.quantize_strength * 100.0) as f32;
			if ui
				.add(
					egui::Slider::new(&mut strength, 0.0..=100.0)
						.suffix(" %")
						.text(tr("strength")),
				)
				.changed()
			{
				c.args.quantize_strength = Some(f64::from(strength) / 100.0);
			}
		}
	});
	row(ui, tr("Key"), |ui| {
		let text = match &effective.key {
			KeySnap::Off => tr("Off (nearest semitone)").to_owned(),
			KeySnap::Auto => tr("Detect").to_owned(),
			KeySnap::Scale(s) => cadenza_mcp::tools::notes::scale_name(s),
		};
		egui::ComboBox::from_id_salt("to-midi-key")
			.width(220.0)
			.selected_text(text)
			.show_ui(ui, |ui| {
				if ui
					.selectable_label(effective.key == KeySnap::Off, tr("Off (nearest semitone)"))
					.clicked()
				{
					c.args.key = Some("off".into());
				}
				if ui
					.selectable_label(effective.key == KeySnap::Auto, tr("Detect"))
					.clicked()
				{
					c.args.key = Some("auto".into());
				}
				ui.separator();
				for kind in ["major", "minor"] {
					for root in ROOTS {
						let key = format!("{root} {kind}");
						if ui.selectable_label(false, &key).clicked() {
							c.args.key = Some(key);
						}
					}
				}
			});
	});
	row(ui, tr("Pitch bend"), |ui| {
		let mut bends = effective.bends;
		if widgets::checkbox(ui, &mut bends, tr("Replay slides and vibrato")).changed() {
			c.args.bends = Some(bends);
		}
		if bends {
			let mut range = effective.bend_range as f32;
			if ui
				.add(
					egui::DragValue::new(&mut range)
						.range(1.0..=24.0)
						.speed(0.1),
				)
				.changed()
			{
				c.args.bend_range = Some(f64::from(range));
			}
			ui.label(RichText::new(tr("semitone range")).color(theme::TEXT_FAINT));
		}
	});
	row(ui, tr("Octave"), |ui| {
		let labels = ["−2", "−1", "0", "+1", "+2"];
		let selected = (effective.octave_shift + 2).clamp(0, 4) as usize;
		if let Some(i) = widgets::segmented_sized(ui, &labels, selected, 220.0) {
			c.args.octave_shift = Some(i as i32 - 2);
		}
	});
	row(ui, tr("Sensitivity"), |ui| {
		let mut s = c.sensitivity() as f32;
		let r = ui.add(egui::Slider::new(&mut s, 0.5..=2.0).fixed_decimals(2));
		r.clone()
			.on_hover_text(tr("Above 1 finds more notes, below 1 fewer"));
		if r.drag_stopped() || (r.changed() && !r.dragged()) {
			c.args.sensitivity = Some(f64::from(s));
		}
	});
	row(ui, tr("Write to"), |ui| {
		let current = c
			.args
			.track_id
			.and_then(|id| targets.iter().find(|(t, _)| *t == id))
			.map_or_else(|| tr("New track below").to_owned(), |(_, n)| n.clone());
		egui::ComboBox::from_id_salt("to-midi-target")
			.width(220.0)
			.selected_text(current)
			.show_ui(ui, |ui| {
				if ui
					.selectable_label(c.args.track_id.is_none(), tr("New track below"))
					.clicked()
				{
					c.args.track_id = None;
				}
				for (id, name) in &targets {
					if ui
						.selectable_label(c.args.track_id == Some(*id), name)
						.clicked()
					{
						c.args.track_id = Some(*id);
					}
				}
			});
	});
}

fn preview_panel(ui: &mut Ui, p: &ConvertPreview) {
	let s = &p.summary;
	let mut line = trn("{n} note", "{n} notes", s.note_count);
	line.push_str(" · ");
	line.push_str(match s.algorithm {
		Algorithm::Poly => tr("chords"),
		Algorithm::Percussive => tr("drums"),
		_ => tr("melody"),
	});
	if let Some(key) = &s.key {
		line.push_str(&format!(" · {key}"));
	}
	if let Some(range) = &s.range {
		line.push_str(&format!(" · {range}"));
	}
	if s.bend_points > 0 {
		line.push_str(&format!(
			" · {}",
			trn("{n} bend event", "{n} bend events", s.bend_points)
		));
	}
	ui.label(RichText::new(line).color(theme::TEXT));
	for w in &s.warnings {
		ui.label(RichText::new(w).font(theme::small()).color(theme::WARNING));
	}
	ui.add_space(4.0);
	let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 120.0), Sense::hover());
	let painter = ui.painter_at(rect);
	painter.rect_filled(rect, 4.0, theme::DISPLAY_BG);
	let (from, to) = (p.span.0, p.span.1.max(p.span.0 + 1.0));
	let low = p
		.notes
		.iter()
		.map(|n| n.2)
		.min()
		.unwrap_or(60)
		.saturating_sub(1);
	let high = p
		.notes
		.iter()
		.map(|n| n.2)
		.max()
		.unwrap_or(60)
		.saturating_add(1)
		.max(low + 12);
	let inner = rect.shrink2(vec2(6.0, 6.0));
	let x = |beat: f64| inner.left() + ((beat - from) / (to - from)) as f32 * inner.width();
	let row_h = inner.height() / f32::from(high - low + 1);
	let y = |pitch: u8| inner.bottom() - f32::from(pitch - low + 1) * row_h;
	for beat in (from.ceil() as i64)..=(to.floor() as i64) {
		let bx = x(beat as f64);
		let c = if beat % 4 == 0 {
			theme::GRID_BAR
		} else {
			theme::GRID_BEAT
		};
		painter.vline(bx, inner.y_range(), Stroke::new(1.0, c));
	}
	for &(start, length, pitch) in &p.notes {
		let r = Rect::from_min_max(
			pos2(x(start), y(pitch)),
			pos2(
				x(start + length).max(x(start) + 2.0),
				y(pitch) + row_h.max(2.0),
			),
		);
		painter.rect_filled(r, 1.5, theme::ACCENT);
	}
}
