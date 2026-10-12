//! Sampler / drum sampler editor (bottom editor dock, opened from the inspector, a
//! double-click on the instrument in a track header or the mixer). Drum sampler: a 4x4
//! pad grid (click auditions, drop an audio file to load), the selected pad's waveform
//! with start/end markers and its knobs. Sampler: a keyboard zone map (click a zone to
//! select it, keys audition, drop a file on a zone or the keyboard), the zone's waveform
//! with start/end/loop markers, zone knobs and the instrument's own knobs. Every gesture
//! is one command through `AppState`.

use cadenza_core::TrackId;
use cadenza_dsp::instruments::drum_sampler::PADS;
use cadenza_model::{Device, LoopMode, SampleZone};
use egui::{
	Align2, Color32, CornerRadius, CursorIcon, Id, Pos2, Rect, RichText, Sense, Stroke, StrokeKind,
	Ui, pos2, vec2,
};

use crate::jobs::Peaks;
use crate::state::AppState;
use crate::state::dnd::{DropTarget, Payload};
use crate::state::samples::{self as sm, Marker, SampleKind};
use crate::state::selection::DeviceRef;
use crate::theme;
use crate::ui::dnd;
use crate::ui::widgets::{self, Gesture, KnobSpec};

const TOOLBAR: f32 = 30.0;
const PAD: f32 = 62.0;
const PAD_GAP: f32 = 6.0;
const KEYS: f32 = 18.0;
const ZONE_MAP: f32 = 64.0;
const KNOB_ROW: f32 = 54.0;

/// View-local state of the instrument editor.
#[derive(Default)]
pub struct InstrumentEditorUi {
	/// Hit areas of the last frame (drops, OS drops).
	pads: Vec<(Rect, u8)>,
	zones: Vec<(Rect, usize)>,
	keyboard: Option<Rect>,
	device: Option<DeviceRef>,
	/// Note held by a pad/key click (released on mouse up).
	held: Option<(TrackId, u8)>,
	/// Marker being dragged and its current position (seconds).
	marker: Option<(Marker, f64)>,
	/// Waveform area in the last frame (headless tests).
	pub wave_rect: Option<Rect>,
	/// Pad grid area in the last frame (headless tests).
	pub pad_rect: Option<Rect>,
}

impl InstrumentEditorUi {
	/// The drop target under `pos` in the last frame (pads and zones).
	pub fn target_at(&self, pos: Pos2) -> Option<DropTarget> {
		let device = self.device?;
		if let Some((_, note)) = self.pads.iter().find(|(r, _)| r.contains(pos)) {
			return Some(DropTarget::Pad {
				device,
				note: *note,
			});
		}
		if let Some((_, zone)) = self.zones.iter().find(|(r, _)| r.contains(pos)) {
			return Some(DropTarget::Zone {
				device,
				zone: Some(*zone),
			});
		}
		self.keyboard
			.filter(|r| r.contains(pos))
			.map(|_| DropTarget::Zone { device, zone: None })
	}

	/// Releases a note still held by the editor (dock hidden, device changed).
	pub fn release(&mut self, state: &mut AppState) {
		if let Some((track, note)) = self.held.take() {
			state.preview_note_off(track, note);
		}
	}

	pub fn show(&mut self, ui: &mut Ui, state: &mut AppState) {
		let full = ui.available_rect_before_wrap();
		ui.allocate_rect(full, Sense::hover());
		ui.painter().rect_filled(full, 0.0, theme::BG_DEEP);
		self.pads.clear();
		self.zones.clear();
		self.keyboard = None;
		self.wave_rect = None;
		self.pad_rect = None;
		let r = state.view.instrument.device;
		let device = r.and_then(|r| state.device(r).cloned());
		self.device = r.filter(|_| device.is_some());
		let toolbar = Rect::from_min_size(full.min, vec2(full.width(), TOOLBAR));
		let body = Rect::from_min_max(pos2(full.left(), toolbar.bottom()), full.max).shrink(8.0);
		self.toolbar(ui, state, toolbar, r, device.as_ref());
		let (Some(r), Some(device)) = (r, device) else {
			ui.painter().text(
				body.center(),
				Align2::CENTER_CENTER,
				"Add a Sampler or Drum Sampler (browser) and open it here: inspector \"Edit\" or \
				 double-click its name",
				theme::small(),
				theme::TEXT_FAINT,
			);
			return;
		};
		if ui.input(|i| i.pointer.any_released()) {
			self.release(state);
		}
		match sm::kind(&device) {
			Some(SampleKind::Drums) => self.drums(ui, state, r, &device, body),
			Some(SampleKind::Sampler) => self.sampler(ui, state, r, &device, body),
			None => {}
		}
		self.drop_zone(ui, state);
	}

	fn toolbar(
		&mut self,
		ui: &mut Ui,
		state: &mut AppState,
		rect: Rect,
		r: Option<DeviceRef>,
		device: Option<&Device>,
	) {
		ui.painter().rect_filled(rect, 0.0, theme::BG_PANEL);
		ui.painter().hline(
			rect.x_range(),
			rect.bottom() - 0.5,
			Stroke::new(1.0, theme::BORDER),
		);
		let mut bar = ui.new_child(
			egui::UiBuilder::new()
				.max_rect(rect.shrink2(vec2(8.0, 3.0)))
				.layout(egui::Layout::left_to_right(egui::Align::Center)),
		);
		let ui = &mut bar;
		ui.spacing_mut().item_spacing.x = 6.0;
		let kind = device.and_then(sm::kind);
		ui.label(
			RichText::new(match kind {
				Some(SampleKind::Drums) => "DRUM SAMPLER",
				_ => "SAMPLER",
			})
			.font(theme::small())
			.color(theme::TEXT_FAINT),
		);
		let (Some(r), Some(d)) = (r, device) else {
			return;
		};
		let track = r.track.and_then(|t| state.project().track(t)).cloned();
		let color = track
			.as_ref()
			.map_or(theme::ACCENT, |t| theme::track_color(t.color.as_deref()));
		let (dot, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
		ui.painter().rect_filled(dot, CornerRadius::same(2), color);
		ui.label(RichText::new(&d.name).strong().color(theme::TEXT));
		if let Some(t) = &track {
			ui.label(
				RichText::new(&t.name)
					.font(theme::small())
					.color(theme::TEXT_DIM),
			);
		}
		ui.separator();
		let id = d.builtin_id().unwrap_or_default().to_owned();
		ui.menu_button("Presets", |ui| {
			for preset in cadenza_dsp::factory_presets(&id) {
				if ui
					.button(preset.name)
					.on_hover_text(preset.description)
					.clicked()
				{
					state.load_factory_preset(r, preset);
					ui.close();
				}
			}
		});
		ui.label(
			RichText::new(match kind {
				Some(SampleKind::Drums) => {
					"Click a pad to hear it · drop audio files on pads (browser or file manager)"
				}
				_ => {
					"Keys audition · drop audio on a zone (replace) or the keyboard (map across it)"
				}
			})
			.font(theme::small())
			.color(theme::TEXT_FAINT),
		);
		ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
			if ui
				.small_button("Piano roll")
				.on_hover_text("Show the piano roll in this dock again")
				.clicked()
			{
				self.release(state);
				state.close_instrument_editor();
			}
		});
	}

	// --- Drum sampler ------------------------------------------------------------

	fn drums(&mut self, ui: &mut Ui, state: &mut AppState, r: DeviceRef, d: &Device, body: Rect) {
		let grid_w = 4.0 * PAD + 3.0 * PAD_GAP;
		let grid_h = 4.0 * PAD + 3.0 * PAD_GAP;
		let grid = Rect::from_min_size(body.min, vec2(grid_w, grid_h.min(body.height())));
		self.pad_rect = Some(grid);
		let color = r
			.track
			.and_then(|t| state.project().track(t))
			.map_or(theme::ACCENT, |t| theme::track_color(t.color.as_deref()));
		let pad_size = ((grid.height() - 3.0 * PAD_GAP) / 4.0).min(PAD);
		for (i, &(note, _)) in PADS.iter().enumerate() {
			// Pad 1 bottom-left, rows upwards (FPC / MPC layout).
			let (row, col) = (i / 4, i % 4);
			let min = pos2(
				grid.left() + col as f32 * (pad_size + PAD_GAP),
				grid.top() + (3 - row) as f32 * (pad_size + PAD_GAP),
			);
			let rect = Rect::from_min_size(min, vec2(pad_size, pad_size));
			self.pads.push((rect, note));
			self.pad(ui, state, r, d, rect, note, color);
		}
		let detail = Rect::from_min_max(pos2(grid.right() + 14.0, body.top()), body.max);
		self.pad_detail(ui, state, r, d, detail);
	}

	#[allow(clippy::too_many_arguments)]
	fn pad(
		&mut self,
		ui: &mut Ui,
		state: &mut AppState,
		r: DeviceRef,
		d: &Device,
		rect: Rect,
		note: u8,
		color: Color32,
	) {
		let resp = ui.interact(rect, Id::new(("pad", r.device, note)), Sense::click());
		let layers = sm::pad_zones(d, note);
		let loaded = !layers.is_empty();
		let selected = state.view.instrument.pad == note;
		let held = self.held.is_some_and(|(_, n)| n == note);
		if resp.is_pointer_button_down_on()
			&& !held && let Some(track) = r.track
		{
			// Higher on the pad = harder hit, like FPC.
			let y = ui.ctx().pointer_interact_pos().map_or(0.5, |p| {
				((rect.bottom() - p.y) / rect.height()).clamp(0.0, 1.0)
			});
			self.release(state);
			state.view.instrument.pad = note;
			if loaded {
				state.preview_note_on(track, note, 0.35 + 0.65 * y);
				self.held = Some((track, note));
			}
		}
		let fill = if held {
			theme::mix(color, Color32::WHITE, 0.25)
		} else if loaded {
			theme::mix(
				theme::BG_WIDGET,
				color,
				if resp.hovered() { 0.45 } else { 0.32 },
			)
		} else if resp.hovered() {
			theme::BG_HOVER
		} else {
			theme::BG_RAISED
		};
		let p = ui.painter();
		p.rect_filled(rect, CornerRadius::same(4), fill);
		p.rect_stroke(
			rect,
			CornerRadius::same(4),
			Stroke::new(
				if selected { 2.0 } else { 1.0 },
				if selected {
					theme::ACCENT
				} else {
					theme::BORDER_STRONG
				},
			),
			StrokeKind::Inside,
		);
		let text = if loaded {
			theme::text_on(fill)
		} else {
			theme::TEXT_FAINT
		};
		let name = crate::ui::mixer::elide(
			ui,
			&sm::pad_label(note),
			&egui::FontId::proportional(10.5),
			rect.width() - 26.0,
		);
		let p = ui.painter();
		p.text(
			rect.left_top() + vec2(5.0, 4.0),
			Align2::LEFT_TOP,
			name,
			egui::FontId::proportional(10.5),
			text,
		);
		let sample = layers.first().map(|&i| d.zones[i].sample.stem());
		let caption = match (&sample, layers.len()) {
			(Some(s), 1) => {
				crate::ui::mixer::elide(ui, s, &egui::FontId::proportional(9.5), rect.width() - 8.0)
			}
			(Some(_), n) => format!("{n} layers"),
			(None, _) => "empty".into(),
		};
		p.text(
			rect.left_bottom() + vec2(5.0, -4.0),
			Align2::LEFT_BOTTOM,
			caption,
			egui::FontId::proportional(9.5),
			text.gamma_multiply(0.85),
		);
		p.text(
			rect.right_top() + vec2(-4.0, 4.0),
			Align2::RIGHT_TOP,
			note.to_string(),
			egui::FontId::monospace(9.0),
			text.gamma_multiply(0.6),
		);
		resp.on_hover_text(format!(
			"{} — note {} ({note})\nClick to hear it (higher = harder) · drop an audio file to load it",
			sm::pad_label(note),
			cadenza_model::note_name(note)
		));
	}

	fn pad_detail(
		&mut self,
		ui: &mut Ui,
		state: &mut AppState,
		r: DeviceRef,
		d: &Device,
		area: Rect,
	) {
		let note = state.view.instrument.pad;
		let layers = sm::pad_zones(d, note);
		let p = ui.painter();
		p.text(
			area.left_top(),
			Align2::LEFT_TOP,
			format!(
				"{} · {}",
				sm::pad_label(note),
				cadenza_model::note_name(note)
			),
			egui::FontId::proportional(13.0),
			theme::TEXT,
		);
		let Some(&first) = layers.first() else {
			p.text(
				area.center(),
				Align2::CENTER_CENTER,
				"Empty pad — drop an audio file on it",
				theme::small(),
				theme::TEXT_FAINT,
			);
			self.globals(
				ui,
				state,
				r,
				d,
				Rect::from_min_max(pos2(area.left(), area.bottom() - KNOB_ROW), area.max),
			);
			return;
		};
		let zone = d.zones[first].clone();
		p.text(
			area.left_top() + vec2(0.0, 17.0),
			Align2::LEFT_TOP,
			match layers.len() {
				1 => zone.sample.display_name(),
				n => format!("{} (+{} layers)", zone.sample.display_name(), n - 1),
			},
			theme::small(),
			theme::TEXT_DIM,
		);
		let knobs_h = 2.0 * KNOB_ROW;
		let wave = Rect::from_min_max(
			pos2(area.left(), area.top() + 36.0),
			pos2(
				area.right(),
				(area.bottom() - knobs_h - 6.0).max(area.top() + 60.0),
			),
		);
		self.waveform(ui, state, r, first, &zone, wave, false);
		let row1 = Rect::from_min_size(
			pos2(area.left(), wave.bottom() + 6.0),
			vec2(area.width(), KNOB_ROW),
		);
		let mut child = ui.new_child(
			egui::UiBuilder::new()
				.max_rect(row1)
				.layout(egui::Layout::left_to_right(egui::Align::Min)),
		);
		let ui2 = &mut child;
		ui2.spacing_mut().item_spacing.x = 4.0;
		let edit = |state: &mut AppState, f: &dyn Fn(&mut SampleZone)| {
			let cmd = sm::edit_pad(state.project(), r, note, f);
			state.apply_sample_edit(cmd);
		};
		let id = |s: &str| Id::new(("pad-knob", r.device, note, s.to_owned()));
		if let Some(v) = commit(widgets::knob(
			ui2,
			id("gain"),
			"Gain",
			zone.gain_db,
			KnobSpec::new(-60.0, 24.0, 0.0),
			db,
		)) {
			edit(state, &|z| z.gain_db = v);
		}
		if let Some(v) = commit(widgets::knob(
			ui2,
			id("pan"),
			"Pan",
			zone.pan,
			KnobSpec::new(-1.0, 1.0, 0.0),
			widgets::format_pan,
		)) {
			edit(state, &|z| z.pan = v);
		}
		if let Some(v) = commit(widgets::knob(
			ui2,
			id("tune"),
			"Tune",
			zone.tune,
			KnobSpec::new(-24.0, 24.0, 0.0),
			semis,
		)) {
			edit(state, &|z| z.tune = (v * 100.0).round() / 100.0);
		}
		if let Some(v) = commit(widgets::knob(
			ui2,
			id("attack"),
			"Attack",
			zone.attack_ms,
			KnobSpec::new(0.0, 500.0, 0.0),
			ms,
		)) {
			edit(state, &|z| z.attack_ms = v.round());
		}
		let decay = zone.decay_ms.unwrap_or(0.0);
		if let Some(v) = commit(widgets::knob(
			ui2,
			id("decay"),
			"Decay",
			decay,
			KnobSpec::new(0.0, 4000.0, 0.0),
			|v| {
				if v < 1.0 { "full".into() } else { ms(v) }
			},
		)) {
			edit(state, &|z| z.decay_ms = (v >= 1.0).then_some(v.round()));
		}
		if let Some(v) = commit(widgets::knob(
			ui2,
			id("choke"),
			"Choke",
			f32::from(zone.choke),
			KnobSpec::new(0.0, 8.0, 0.0).integer(),
			|v| {
				if v < 0.5 {
					"off".into()
				} else {
					format!("group {v:.0}")
				}
			},
		)) {
			edit(state, &|z| z.choke = v as u8);
		}
		if let Some(v) = commit(widgets::knob(
			ui2,
			id("output"),
			"Output",
			f32::from(zone.output),
			KnobSpec::new(0.0, 8.0, 0.0).integer(),
			|v| {
				if v < 0.5 {
					"main".into()
				} else {
					format!("out {v:.0}")
				}
			},
		)) {
			edit(state, &|z| z.output = v as u8);
		}
		ui2.vertical(|ui| {
			ui.add_space(4.0);
			if widgets::toggle(ui, zone.reverse, "Reverse", theme::ACCENT, vec2(58.0, 18.0))
				.on_hover_text("Play the pad backwards")
				.clicked()
			{
				let on = !zone.reverse;
				edit(state, &|z| z.reverse = on);
			}
			if ui.small_button("Clear pad").clicked() {
				let cmd = sm::clear_pad(state.project(), r, note);
				state.apply_sample_edit(cmd);
			}
		});
		let row2 = Rect::from_min_size(
			pos2(area.left(), row1.bottom()),
			vec2(area.width(), KNOB_ROW),
		);
		self.globals(ui, state, r, d, row2);
	}

	// --- Sampler -----------------------------------------------------------------

	fn sampler(&mut self, ui: &mut Ui, state: &mut AppState, r: DeviceRef, d: &Device, body: Rect) {
		let map = Rect::from_min_size(body.min, vec2(body.width(), ZONE_MAP + KEYS));
		self.zone_map(ui, state, r, d, map);
		let index = state
			.view
			.instrument
			.zone
			.min(d.zones.len().saturating_sub(1));
		let rest = Rect::from_min_max(pos2(body.left(), map.bottom() + 8.0), body.max);
		let knobs_h = 2.0 * KNOB_ROW;
		let Some(zone) = d.zones.get(index).cloned() else {
			ui.painter().text(
				rest.center() - vec2(0.0, knobs_h * 0.5),
				Align2::CENTER_CENTER,
				"No samples — drop an audio file on the keyboard to map it across",
				theme::small(),
				theme::TEXT_FAINT,
			);
			self.globals(
				ui,
				state,
				r,
				d,
				Rect::from_min_max(pos2(rest.left(), rest.bottom() - knobs_h), rest.max),
			);
			return;
		};
		let wave = Rect::from_min_max(
			rest.min,
			pos2(
				rest.right(),
				(rest.bottom() - knobs_h - 6.0).max(rest.top() + 40.0),
			),
		);
		self.waveform(ui, state, r, index, &zone, wave, true);
		let row1 = Rect::from_min_size(
			pos2(rest.left(), wave.bottom() + 6.0),
			vec2(rest.width(), KNOB_ROW),
		);
		self.zone_knobs(ui, state, r, index, &zone, row1);
		let row2 = Rect::from_min_size(
			pos2(rest.left(), row1.bottom()),
			vec2(rest.width(), KNOB_ROW),
		);
		self.globals(ui, state, r, d, row2);
	}

	fn zone_map(
		&mut self,
		ui: &mut Ui,
		state: &mut AppState,
		r: DeviceRef,
		d: &Device,
		rect: Rect,
	) {
		let p = ui.painter().clone();
		p.rect_filled(rect, CornerRadius::same(3), theme::BG_PANEL);
		let key_w = rect.width() / 128.0;
		let x_of = |k: u8| rect.left() + f32::from(k) * key_w;
		let lanes = Rect::from_min_max(rect.min, pos2(rect.right(), rect.bottom() - KEYS));
		// Octave lines.
		for c in (0..128u8).step_by(12) {
			p.vline(x_of(c), lanes.y_range(), Stroke::new(1.0, theme::GRID_BEAT));
			p.text(
				pos2(x_of(c) + 2.0, lanes.top() + 1.0),
				Align2::LEFT_TOP,
				cadenza_model::note_name(c),
				egui::FontId::proportional(9.0),
				theme::TEXT_FAINT,
			);
		}
		// Zones stacked in up to 3 rows when they overlap.
		let rows = 3usize;
		let row_h = (lanes.height() - 12.0) / rows as f32;
		let selected = state.view.instrument.zone;
		for (i, z) in d.zones.iter().enumerate() {
			let row = i % rows;
			let zr = Rect::from_min_max(
				pos2(
					x_of(z.key_low) + 0.5,
					lanes.top() + 12.0 + row as f32 * row_h + 1.0,
				),
				pos2(
					x_of(z.key_high) + key_w - 0.5,
					lanes.top() + 12.0 + (row + 1) as f32 * row_h - 1.0,
				),
			);
			self.zones.push((zr, i));
			let resp = ui.interact(zr, Id::new(("zone", r.device, i)), Sense::click());
			if resp.clicked() {
				state.view.instrument.zone = i;
			}
			let on = i == selected;
			let fill = if on {
				theme::ACCENT_DIM
			} else if resp.hovered() {
				theme::BG_HOVER
			} else {
				theme::BG_WIDGET
			};
			p.rect_filled(zr, CornerRadius::same(2), fill);
			p.rect_stroke(
				zr,
				CornerRadius::same(2),
				Stroke::new(
					1.0,
					if on {
						theme::ACCENT
					} else {
						theme::BORDER_STRONG
					},
				),
				StrokeKind::Inside,
			);
			// Root key mark.
			let rx = x_of(z.root_key) + key_w * 0.5;
			if zr.x_range().contains(rx) {
				p.vline(rx, zr.y_range(), Stroke::new(1.5, theme::TEXT_DIM));
			}
			let label = crate::ui::mixer::elide(
				ui,
				&z.sample.stem(),
				&egui::FontId::proportional(9.5),
				zr.width() - 6.0,
			);
			p.text(
				zr.left_center() + vec2(3.0, 0.0),
				Align2::LEFT_CENTER,
				label,
				egui::FontId::proportional(9.5),
				theme::TEXT,
			);
			resp.on_hover_text(format!(
				"Zone {}: {} — {}..{}, root {}\nClick to edit · drop an audio file to replace its sample",
				i + 1,
				z.sample.display_name(),
				cadenza_model::note_name(z.key_low),
				cadenza_model::note_name(z.key_high),
				cadenza_model::note_name(z.root_key)
			));
		}
		// Keyboard strip (click auditions, drop maps a file across it).
		let keys = Rect::from_min_max(pos2(rect.left(), rect.bottom() - KEYS), rect.max);
		self.keyboard = Some(lanes.union(keys));
		p.rect_filled(keys, 0.0, theme::KEY_WHITE);
		for k in 0..128u8 {
			let black = matches!(k % 12, 1 | 3 | 6 | 8 | 10);
			let held = self.held.is_some_and(|(_, n)| n == k);
			let x0 = x_of(k);
			if black {
				let kr = Rect::from_min_max(
					pos2(x0, keys.top()),
					pos2(x0 + key_w, keys.top() + keys.height() * 0.62),
				);
				p.rect_filled(
					kr,
					0.0,
					if held {
						theme::ACCENT
					} else {
						theme::KEY_BLACK
					},
				);
			} else {
				if held {
					p.rect_filled(
						Rect::from_min_max(pos2(x0, keys.top()), pos2(x0 + key_w, keys.bottom())),
						0.0,
						theme::ACCENT,
					);
				}
				// White keys meet without a black key between them at E-F and B-C.
				if matches!(k % 12, 0 | 5) {
					p.vline(x0, keys.y_range(), Stroke::new(1.0, theme::BORDER_STRONG));
				}
			}
		}
		p.hline(
			keys.x_range(),
			keys.top(),
			Stroke::new(1.0, theme::BORDER_STRONG),
		);
		let kresp = ui.interact(
			keys,
			Id::new(("zone-keys", r.device)),
			Sense::click_and_drag(),
		);
		if kresp.is_pointer_button_down_on()
			&& let (Some(pos), Some(track)) = (ui.ctx().pointer_interact_pos(), r.track)
		{
			let k = (((pos.x - rect.left()) / key_w).floor() as i32).clamp(0, 127) as u8;
			if self.held.map(|(_, n)| n) != Some(k) {
				self.release(state);
				state.preview_note_on(track, k, 0.8);
				self.held = Some((track, k));
			}
		}
	}

	fn zone_knobs(
		&mut self,
		ui: &mut Ui,
		state: &mut AppState,
		r: DeviceRef,
		index: usize,
		zone: &SampleZone,
		row: Rect,
	) {
		let mut child = ui.new_child(
			egui::UiBuilder::new()
				.max_rect(row)
				.layout(egui::Layout::left_to_right(egui::Align::Min)),
		);
		let ui = &mut child;
		ui.spacing_mut().item_spacing.x = 4.0;
		let edit = |state: &mut AppState, f: &dyn Fn(&mut SampleZone)| {
			let cmd = sm::edit_zone(state.project(), r, index, f);
			state.apply_sample_edit(cmd);
		};
		let id = |s: &str| Id::new(("zone-knob", r.device, index, s.to_owned()));
		let note = |v: f32| cadenza_model::note_name(v.round().clamp(0.0, 127.0) as u8);
		let keys = KnobSpec::new(0.0, 127.0, 60.0).integer();
		if let Some(v) = commit(widgets::knob(
			ui,
			id("root"),
			"Root",
			f32::from(zone.root_key),
			keys,
			note,
		)) {
			edit(state, &|z| z.root_key = v as u8);
		}
		if let Some(v) = commit(widgets::knob(
			ui,
			id("low"),
			"Low key",
			f32::from(zone.key_low),
			KnobSpec {
				default: 0.0,
				..keys
			},
			note,
		)) {
			edit(state, &|z| {
				z.key_low = v as u8;
				z.key_high = z.key_high.max(z.key_low);
			});
		}
		if let Some(v) = commit(widgets::knob(
			ui,
			id("high"),
			"High key",
			f32::from(zone.key_high),
			KnobSpec {
				default: 127.0,
				..keys
			},
			note,
		)) {
			edit(state, &|z| {
				z.key_high = v as u8;
				z.key_low = z.key_low.min(z.key_high);
			});
		}
		if let Some(v) = commit(widgets::knob(
			ui,
			id("tune"),
			"Tune",
			zone.tune,
			KnobSpec::new(-24.0, 24.0, 0.0),
			semis,
		)) {
			edit(state, &|z| z.tune = (v * 100.0).round() / 100.0);
		}
		if let Some(v) = commit(widgets::knob(
			ui,
			id("gain"),
			"Gain",
			zone.gain_db,
			KnobSpec::new(-60.0, 24.0, 0.0),
			db,
		)) {
			edit(state, &|z| z.gain_db = v);
		}
		if let Some(v) = commit(widgets::knob(
			ui,
			id("pan"),
			"Pan",
			zone.pan,
			KnobSpec::new(-1.0, 1.0, 0.0),
			widgets::format_pan,
		)) {
			edit(state, &|z| z.pan = v);
		}
		if let Some(v) = commit(widgets::knob(
			ui,
			id("vel"),
			"Vel low",
			zone.vel_low,
			KnobSpec::new(0.0, 1.0, 0.0),
			|v| format!("{v:.2}"),
		)) {
			edit(state, &|z| z.vel_low = v.min(z.vel_high));
		}
		if let Some(v) = commit(widgets::knob(
			ui,
			id("xfade"),
			"X-fade",
			zone.loop_crossfade as f32,
			KnobSpec::new(0.0, 0.5, 0.0),
			|v| format!("{:.0} ms", v * 1000.0),
		)) {
			edit(state, &|z| z.loop_crossfade = f64::from(v));
		}
		if let Some(v) = commit(widgets::knob(
			ui,
			id("rr"),
			"Rnd robin",
			f32::from(zone.round_robin),
			KnobSpec::new(0.0, 8.0, 0.0).integer(),
			|v| {
				if v < 0.5 {
					"off".into()
				} else {
					format!("group {v:.0}")
				}
			},
		)) {
			edit(state, &|z| z.round_robin = v as u8);
		}
		ui.vertical(|ui| {
			ui.add_space(2.0);
			ui.horizontal(|ui| {
				ui.spacing_mut().item_spacing.x = 2.0;
				for (mode, label) in [
					(LoopMode::Off, "No loop"),
					(LoopMode::Forward, "Loop"),
					(LoopMode::PingPong, "Ping-pong"),
				] {
					if widgets::toggle(
						ui,
						zone.loop_mode == mode,
						label,
						theme::LOOP,
						vec2(58.0, 18.0),
					)
					.clicked()
					{
						edit(state, &|z| z.loop_mode = mode);
					}
				}
			});
			ui.horizontal(|ui| {
				ui.spacing_mut().item_spacing.x = 2.0;
				if widgets::toggle(ui, zone.reverse, "Reverse", theme::ACCENT, vec2(58.0, 18.0))
					.clicked()
				{
					let on = !zone.reverse;
					edit(state, &|z| z.reverse = on);
				}
				if ui.small_button("Remove zone").clicked() {
					let cmd = sm::remove_zone(state.project(), r, index);
					state.apply_sample_edit(cmd);
				}
			});
		});
	}

	// --- Shared ------------------------------------------------------------------

	/// The instrument's own parameters as knobs (live while dragging, one command each).
	fn globals(&mut self, ui: &mut Ui, state: &mut AppState, r: DeviceRef, d: &Device, row: Rect) {
		let Some(info) = d.builtin_id().and_then(cadenza_dsp::device_info) else {
			return;
		};
		let mut child = ui.new_child(
			egui::UiBuilder::new()
				.max_rect(row)
				.layout(egui::Layout::left_to_right(egui::Align::Min)),
		);
		let ui = &mut child;
		ui.spacing_mut().item_spacing.x = 4.0;
		for param in info.params {
			if ui.available_width() < 48.0 {
				break;
			}
			let value = state
				.param_value(r.track, d.id, param.id)
				.unwrap_or(param.default);
			let mut spec = KnobSpec::new(param.min, param.max, param.default);
			if param.unit == "ms" && param.min > 0.0 || param.unit == "Hz" {
				spec = spec.log();
			}
			let switch =
				param.max - param.min <= 3.0 && param.min.fract() == 0.0 && param.unit.is_empty();
			if switch || param.unit == "voices" {
				spec = spec.integer();
			}
			let unit = param.unit;
			let (g, resp) = widgets::knob(
				ui,
				Id::new(("param-knob", d.id, param.id)),
				&short(param.name),
				value,
				spec,
				|v| match unit {
					"" => format!("{v:.0}"),
					"Hz" if v >= 1000.0 => format!("{:.1} kHz", v / 1000.0),
					"%" | "dB" | "ms" | "Hz" | "voices" => format!("{v:.0} {unit}"),
					_ => format!("{v:.1} {unit}"),
				},
			);
			let resp = resp.on_hover_text(param.description);
			let address = cadenza_core::ParamAddress::device(r.track, d.id, param.id);
			resp.context_menu(|ui| crate::ui::automation::automate_item(ui, state, address));
			match g {
				Gesture::Preview(v) => state.preview_param(d.id, param.id, v),
				Gesture::Commit(v) => state.commit_param(r.track, d.id, param.id, v),
				Gesture::Idle => {}
			}
		}
	}

	/// The zone's sample with its markers (drag them; one command on release).
	#[allow(clippy::too_many_arguments)]
	fn waveform(
		&mut self,
		ui: &mut Ui,
		state: &mut AppState,
		r: DeviceRef,
		index: usize,
		zone: &SampleZone,
		rect: Rect,
		loops: bool,
	) {
		self.wave_rect = Some(rect);
		let p = ui.painter().clone();
		p.rect_filled(rect, CornerRadius::same(3), theme::METER_BG);
		let peaks = match state.peaks.get(&zone.sample.path) {
			Peaks::Ready(peaks) => peaks,
			Peaks::Pending => {
				p.text(
					rect.center(),
					Align2::CENTER_CENTER,
					"loading waveform…",
					theme::small(),
					theme::TEXT_FAINT,
				);
				return;
			}
			Peaks::Failed(e) => {
				p.text(
					rect.center(),
					Align2::CENTER_CENTER,
					format!("⚠ {e} — drop another file to replace it"),
					theme::small(),
					theme::WARNING,
				);
				return;
			}
		};
		let sr = f64::from(peaks.sample_rate.max(1));
		let length = peaks.frames as f64 / sr;
		if length <= 0.0 {
			return;
		}
		let x_of = |s: f64| rect.left() + (s / length) as f32 * rect.width();
		let s_of = |x: f32| f64::from((x - rect.left()) / rect.width()) * length;
		// Shown zone (with the marker being dragged applied).
		let mut shown = zone.clone();
		if let Some((m, s)) = self.marker {
			m.set(&mut shown, s, length);
		}
		let start = Marker::Start.get(&shown, length);
		let end = Marker::End.get(&shown, length);
		// Outside the played region shaded.
		let mid = rect.center().y;
		let half = rect.height() * 0.5 - 2.0;
		let fpp = (length * sr / f64::from(rect.width())).max(1.0);
		let level = peaks.level_for(fpp);
		let channels = usize::from(peaks.num_channels).max(1);
		let gain = cadenza_core::db_to_gain(shown.gain_db).min(4.0);
		let mut x = rect.left();
		while x < rect.right() {
			let f0 = (s_of(x) * sr) as u64;
			let f1 = ((s_of(x + 1.0) * sr) as u64).max(f0 + 1);
			let mut mm = cadenza_audio_io::MinMax::EMPTY;
			for c in 0..channels {
				mm = mm.merge(level.range(c, f0, f1));
			}
			if !mm.is_empty() {
				let s = s_of(x);
				let inside = s >= start && s <= end;
				let color = if inside {
					theme::AUTOMATION
				} else {
					theme::TEXT_FAINT
				};
				let top = mid - (mm.max * gain).clamp(-1.0, 1.0) * half;
				let bottom = mid - (mm.min * gain).clamp(-1.0, 1.0) * half;
				p.vline(
					x + 0.5,
					top..=bottom.max(top + 1.0),
					Stroke::new(1.0, color),
				);
			}
			x += 1.0;
		}
		p.hline(rect.x_range(), mid, Stroke::new(1.0, theme::BORDER));
		if shown.reverse {
			p.text(
				rect.right_top() + vec2(-6.0, 4.0),
				Align2::RIGHT_TOP,
				"◀ reversed",
				theme::small(),
				theme::TEXT_DIM,
			);
		}
		let mut markers = vec![(Marker::Start, theme::PLAYING), (Marker::End, theme::ERROR)];
		let looping = loops && !shown.loop_mode.is_off() && !shown.reverse;
		if looping {
			let ls = Marker::LoopStart.get(&shown, length);
			let le = Marker::LoopEnd.get(&shown, length);
			p.rect_filled(
				Rect::from_x_y_ranges(x_of(ls)..=x_of(le), rect.y_range()),
				0.0,
				theme::LOOP.gamma_multiply(0.12),
			);
			markers.push((Marker::LoopStart, theme::LOOP));
			markers.push((Marker::LoopEnd, theme::LOOP));
		}
		for (n, (m, color)) in markers.into_iter().enumerate() {
			let s = m.get(&shown, length);
			let x = x_of(s);
			p.vline(x, rect.y_range(), Stroke::new(1.5, color));
			let flag_y = if matches!(m, Marker::LoopStart | Marker::LoopEnd) {
				rect.bottom() - 14.0
			} else {
				rect.top()
			};
			let flag = Rect::from_min_size(pos2(x - 6.0, flag_y), vec2(12.0, 14.0));
			p.rect_filled(flag, CornerRadius::same(2), color);
			p.text(
				flag.center(),
				Align2::CENTER_CENTER,
				m.label(),
				egui::FontId::proportional(9.0),
				theme::BG_DEEP,
			);
			let handle = Rect::from_x_y_ranges(x - 5.0..=x + 5.0, rect.y_range());
			let resp = ui
				.interact(
					handle,
					Id::new(("marker", r.device, index, n)),
					Sense::drag(),
				)
				.on_hover_cursor(CursorIcon::ResizeHorizontal);
			if resp.dragged()
				&& let Some(pos) = ui.ctx().pointer_interact_pos()
			{
				self.marker = Some((m, s_of(pos.x.clamp(rect.left(), rect.right()))));
			}
			if resp.drag_stopped()
				&& let Some((m, s)) = self.marker.take()
			{
				let kind = state.device(r).and_then(sm::kind);
				let cmd = match kind {
					Some(SampleKind::Drums) => {
						let note = zone.key_low;
						sm::edit_pad(state.project(), r, note, |z| m.set(z, s, length))
					}
					_ => sm::edit_zone(state.project(), r, index, |z| m.set(z, s, length)),
				};
				state.apply_sample_edit(cmd);
			}
		}
		let info = p.layout_no_wrap(
			format!("{:.3} s – {:.3} s of {:.2} s", start, end, length),
			egui::FontId::monospace(9.5),
			theme::TEXT_DIM,
		);
		let at = rect.left_top() + vec2(16.0, 2.0);
		p.rect_filled(
			Rect::from_min_size(at - vec2(3.0, 1.0), info.size() + vec2(6.0, 2.0)),
			CornerRadius::same(2),
			theme::BG_DEEP.gamma_multiply(0.85),
		);
		p.galley(at, info, theme::TEXT_DIM);
	}

	/// Highlights the pad / zone under an in-app drag or an OS file hover, shows the hint
	/// and performs in-app drops on release (OS drops are handled by the window).
	fn drop_zone(&mut self, ui: &Ui, state: &mut AppState) {
		let ctx = ui.ctx().clone();
		let in_app = egui::DragAndDrop::payload::<Payload>(&ctx).map(|p| (*p).clone());
		let payload = in_app
			.clone()
			.or_else(|| dnd::hovered_file(&ctx).map(Payload::File));
		let (Some(payload), Some(pos)) = (payload, ctx.pointer_latest_pos()) else {
			return;
		};
		let Some(target) = self.target_at(pos) else {
			return;
		};
		let action = state.drop_action(&payload, target, false);
		let color = if action.accepted() {
			theme::ACCENT
		} else {
			theme::ERROR
		};
		let rect = match target {
			DropTarget::Pad { note, .. } => {
				self.pads.iter().find(|(_, n)| *n == note).map(|(r, _)| *r)
			}
			DropTarget::Zone { zone: Some(i), .. } => {
				self.zones.iter().find(|(_, z)| *z == i).map(|(r, _)| *r)
			}
			_ => self.keyboard,
		};
		if let Some(rect) = rect {
			ui.painter().rect_stroke(
				rect.expand(1.0),
				CornerRadius::same(4),
				Stroke::new(2.0, color),
				StrokeKind::Outside,
			);
		}
		dnd::set_hint(&ctx, action.describe(state.project()), action.accepted());
		if in_app.is_some() && ctx.input(|i| i.pointer.any_released()) {
			egui::DragAndDrop::clear_payload(&ctx);
			state.drop_payload(&payload, target, false);
		}
	}
}

/// The committed value of a knob gesture.
fn commit((g, _): (Gesture, egui::Response)) -> Option<f32> {
	match g {
		Gesture::Commit(v) => Some(v),
		_ => None,
	}
}

fn db(v: f32) -> String {
	format!("{v:+.1} dB")
}

fn semis(v: f32) -> String {
	format!("{v:+.2} st")
}

fn ms(v: f32) -> String {
	if v >= 1000.0 {
		format!("{:.2} s", v / 1000.0)
	} else {
		format!("{v:.0} ms")
	}
}

/// Knob caption: param names without the section word when it is obvious.
fn short(name: &str) -> String {
	match name {
		"Filter Type" => "Filter".into(),
		"Filter Cutoff" => "Cutoff".into(),
		"Filter Resonance" => "Reso".into(),
		"Filter Env Amount" => "F.Env".into(),
		"Filter Velocity" => "F.Vel".into(),
		"Filter Key Tracking" => "F.Key".into(),
		"Velocity Sensitivity" => "Velocity".into(),
		"Velocity Curve" => "Vel curve".into(),
		"Trigger Mode" => "One-shot".into(),
		other => match (other.strip_prefix("Filter "), other.strip_prefix("Amp ")) {
			(Some(n), _) => format!("F.{n}"),
			(_, Some(n)) => n.to_owned(),
			_ => other.to_owned(),
		},
	}
}
