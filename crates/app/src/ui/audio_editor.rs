//! Audio editor (bottom editor dock, opened by double-clicking an audio clip): the
//! clip's source file at full detail (peak levels when zoomed out, the samples
//! themselves when zoomed in), the clip region with draggable start/end handles, fade
//! handles with curve pickers, a clip gain handle, and a selection range for split /
//! trim / delete (clip edits) and processed copies (normalize, reverse, gain,
//! stretch/pitch). Every gesture is one command through `AppState`.

use crate::i18n::{tr, trf};
use cadenza_model::{ClipFade, FadeCurve};
use egui::{
	Align2, Color32, CornerRadius, CursorIcon, Id, PointerButton, Pos2, Rect, RichText, Sense,
	Shape, Stroke, StrokeKind, Ui, pos2, vec2,
};

use crate::jobs::{Peaks, Samples};
use crate::state::AppState;
use crate::state::audio_edit::{self as ae, FADE_CURVES, Process, Region};
use crate::state::note_editor::EditorMode;
use crate::theme;

const TOOLBAR: f32 = 30.0;
const RULER: f32 = 20.0;
/// Pointer distance (px) that grabs a handle.
const HANDLE: f32 = 6.0;

#[derive(Debug, Clone, Copy, PartialEq)]
enum Grab {
	Start,
	End,
	FadeIn,
	FadeOut,
	Gain,
	Select,
}

#[derive(Debug, Clone, Copy)]
struct Drag {
	grab: Grab,
	/// File second (or dB for the gain) at the press.
	origin: f64,
	press: Pos2,
	/// Current value of the gesture.
	value: f64,
}

/// View-local state of the audio editor.
#[derive(Default)]
pub struct AudioEditorUi {
	drag: Option<Drag>,
	/// The Notes mode (NE-6).
	pub notes: super::note_editor::NoteEditorUi,
	/// Waveform area in the last frame (headless tests).
	pub wave_rect: Option<Rect>,
}

impl AudioEditorUi {
	pub fn show(&mut self, ui: &mut Ui, state: &mut AppState) {
		let full = ui.available_rect_before_wrap();
		ui.allocate_rect(full, Sense::hover());
		ui.painter().rect_filled(full, 0.0, theme::BG_DEEP);
		let toolbar = Rect::from_min_size(full.min, vec2(full.width(), TOOLBAR));
		let ruler = Rect::from_min_max(
			pos2(full.left(), toolbar.bottom()),
			pos2(full.right(), toolbar.bottom() + RULER),
		);
		let wave = Rect::from_min_max(pos2(full.left(), ruler.bottom()), full.max);
		self.wave_rect = Some(wave);
		let region = state
			.view
			.audio
			.clip
			.and_then(|c| ae::region(state.project(), c));
		self.toolbar(ui, state, toolbar, region.as_ref());
		if let Some(r) = &region
			&& state.view.audio.mode == EditorMode::Notes
		{
			let area = Rect::from_min_max(pos2(full.left(), toolbar.bottom()), full.max);
			let layout = super::note_editor::Layout::of(area, RULER, state.view.audio.notes.macros);
			self.wave_rect = Some(layout.canvas);
			if state.view.audio.framed != Some(r.clip.id) {
				state
					.view
					.audio
					.frame(r.offset, r.end(), layout.canvas.width());
				state.view.audio.framed = Some(r.clip.id);
			}
			self.notes.show(ui, state, layout, r);
			return;
		}
		let Some(r) = region else {
			ui.painter().text(
				wave.center(),
				Align2::CENTER_CENTER,
				tr("Double-click an audio clip to edit it here"),
				theme::small(),
				theme::TEXT_FAINT,
			);
			return;
		};
		if state.view.audio.framed != Some(r.clip.id) {
			state.view.audio.frame(r.offset, r.end(), wave.width());
			state.view.audio.framed = Some(r.clip.id);
		}
		self.navigate(ui, state, wave.union(ruler), wave.left(), &r);
		ruler_paint(ui, state, ruler);
		self.waveform(ui, state, wave, &r);
		self.handles(ui, state, wave, &r);
		// Playhead (song position mapped into the file).
		let s = r.seconds_of(state.project(), state.position());
		let x = wave.left() + state.view.audio.x_of(s);
		if (wave.left()..=wave.right()).contains(&x) {
			ui.painter().vline(
				x,
				ruler.top()..=wave.bottom(),
				Stroke::new(1.5, theme::PLAYHEAD),
			);
		}
	}

	/// Wheel scrolls, Ctrl+wheel zooms at the pointer.
	fn navigate(&mut self, ui: &Ui, state: &mut AppState, area: Rect, left: f32, r: &Region) {
		let Some(p) = ui.ctx().pointer_hover_pos() else {
			return;
		};
		if !area.contains(p) || !ui.ui_contains_pointer() {
			return;
		}
		let (zoom, scroll) = ui.input(|i| (i.zoom_delta(), i.smooth_scroll_delta));
		let v = &mut state.view.audio;
		if zoom != 1.0 {
			v.zoom_around(zoom, (p.x - left).max(0.0));
		}
		let dx = if scroll.x != 0.0 { scroll.x } else { scroll.y };
		if dx != 0.0 {
			v.scroll = (v.scroll - f64::from(dx) / f64::from(v.px_per_second))
				.clamp(0.0, (r.end() + 1.0).max(0.0));
		}
	}

	fn toolbar(&mut self, ui: &mut Ui, state: &mut AppState, rect: Rect, r: Option<&Region>) {
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
		ui.spacing_mut().item_spacing.x = 5.0;
		super::widgets::panel_title(ui, tr("Audio editor"), Some("F7"));
		let Some(r) = r else {
			return;
		};
		let color = state
			.project()
			.track(r.track)
			.map_or(theme::ACCENT, |t| theme::track_color(t.color.as_deref()));
		let (dot, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
		ui.painter().rect_filled(dot, CornerRadius::same(2), color);
		ui.label(RichText::new(&r.clip.name).strong().color(theme::TEXT));
		ui.label(
			RichText::new(
				r.file
					.file_name()
					.map(|f| f.to_string_lossy().into_owned())
					.unwrap_or_default(),
			)
			.font(theme::small())
			.color(theme::TEXT_DIM),
		)
		.on_hover_text(r.file.display().to_string());
		ui.separator();
		let modes = [tr("Waveform"), tr("Notes")];
		let mode = state.view.audio.mode;
		if let Some(i) = super::widgets::segmented_sized(
			ui,
			&modes,
			usize::from(mode == EditorMode::Notes),
			150.0,
		) {
			if i == 1 {
				state.open_note_editor(r.clip.id);
			} else {
				state.view.audio.mode = EditorMode::Waveform;
			}
		}
		ui.separator();
		if state.view.audio.mode == EditorMode::Notes {
			self.notes.toolbar(ui, state, r);
			ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
				if ui
					.add(
						egui::Button::new(RichText::new("×").size(15.0).color(theme::TEXT_DIM))
							.frame(false),
					)
					.on_hover_text(tr("Close the audio editor (F7)"))
					.clicked()
				{
					state.close_piano_roll();
				}
				if ui
					.small_button(tr("Fit"))
					.on_hover_text(tr("Show the whole clip"))
					.clicked()
				{
					state.view.audio.framed = None;
					state.view.audio.notes.framed = None;
				}
			});
			return;
		}
		let clip = r.clip.id;
		let sel = state.view.audio.selection.map(|s| r.clamp(s));
		let has_sel = sel.is_some_and(|(a, b)| b - a > 1e-6);
		let cursor = sel.map(|s| s.0);
		if ui
			.add_enabled(cursor.is_some(), egui::Button::new(tr("Split")))
			.on_hover_text(tr("Split the clip at the selection start (S)"))
			.clicked()
			&& let Some(at) = cursor
			&& let Some((cmd, _)) = ae::split_at(state.project(), clip, at)
		{
			state.apply(cmd);
		}
		if ui
			.add_enabled(has_sel, egui::Button::new(tr("Trim")))
			.on_hover_text(tr("Keep only the selection (non-destructive)"))
			.clicked()
			&& let Some(s) = sel
		{
			let cmd = ae::trim_to(state.project(), clip, s);
			if state.audio_edit(cmd) {
				state.view.audio.selection = None;
			}
		}
		if ui
			.add_enabled(has_sel, egui::Button::new(tr("Delete")))
			.on_hover_text(tr("Delete the selected range, leaving a gap (Del)"))
			.clicked()
			&& let Some(s) = sel
		{
			delete_range(state, clip, s);
		}
		ui.separator();
		let scope = if has_sel {
			"selection"
		} else {
			tr("whole clip")
		};
		let busy = state.jobs.process.is_some();
		ui.add_enabled_ui(!busy, |ui| {
			if ui
				.button(tr("Normalize"))
				.on_hover_text(trf(
					"Peak to {normalize_dbfs} dBFS ({scope}); renders a new file, undoable",
					&[("normalize_dbfs", &ae::NORMALIZE_DBFS), ("scope", &scope)],
				))
				.clicked()
			{
				state.process_audio(Process::Normalize);
			}
			if ui
				.button(tr("Reverse"))
				.on_hover_text(trf(
					"Reverse the {scope}; renders a new file, undoable",
					&[("scope", &scope)],
				))
				.clicked()
			{
				state.process_audio(Process::Reverse);
			}
			let db = state.view.audio.gain_db;
			if ui
				.button(trf(
					"Gain {db} dB",
					&[("db", &crate::i18n::decimal(format!("{:+.1}", db)))],
				))
				.on_hover_text(trf(
					"Apply the gain to the {scope}; renders a new file",
					&[("scope", &scope)],
				))
				.clicked()
			{
				state.process_audio(Process::Gain(db));
			}
			ui.add(
				egui::DragValue::new(&mut state.view.audio.gain_db)
					.range(-48.0..=24.0)
					.speed(0.1)
					.suffix(" dB")
					.max_decimals(1),
			);
			ui.menu_button(tr("Stretch / Pitch"), |ui| {
				let enabled = cfg!(feature = "stretch");
				if !enabled {
					ui.label(
						RichText::new(crate::i18n::tr_str(ae::STRETCH_HINT))
							.font(theme::small())
							.color(theme::WARNING),
					);
					ui.separator();
				}
				ui.add_enabled_ui(enabled, |ui| {
					ui.horizontal(|ui| {
						ui.add(
							egui::DragValue::new(&mut state.view.audio.stretch_ratio)
								.range(0.25..=4.0)
								.speed(0.01)
								.prefix("×"),
						);
						if ui.button(tr("Time-stretch")).clicked() {
							state.process_audio(Process::Stretch(state.view.audio.stretch_ratio));
							ui.close();
						}
					});
					ui.horizontal(|ui| {
						ui.add(
							egui::DragValue::new(&mut state.view.audio.pitch_semitones)
								.range(-24.0..=24.0)
								.speed(0.1)
								.suffix(" st"),
						);
						if ui.button(tr("Pitch-shift")).clicked() {
							state.process_audio(Process::Pitch(state.view.audio.pitch_semitones));
							ui.close();
						}
					});
				});
			});
		});
		if busy {
			ui.spinner();
		}
		ui.separator();
		fade_picker(ui, state, clip, r, true);
		fade_picker(ui, state, clip, r, false);
		ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
			if ui
				.add(
					egui::Button::new(RichText::new("×").size(15.0).color(theme::TEXT_DIM))
						.frame(false),
				)
				.on_hover_text(tr("Close the audio editor (F7)"))
				.clicked()
			{
				state.close_piano_roll();
			}
			if ui
				.small_button(tr("Fit"))
				.on_hover_text(tr("Show the whole clip"))
				.clicked()
			{
				state.view.audio.framed = None;
			}
			ui.label(
				RichText::new(trf(
					"gain {gain_db} dB",
					&[(
						"gain_db",
						&crate::i18n::decimal(format!("{:+.1}", r.gain_db)),
					)],
				))
				.font(theme::small())
				.color(theme::TEXT_DIM),
			);
			if let Some((a, b)) = sel {
				ui.label(
					RichText::new(trf(
						"sel {from}–{to} s ({length} s)",
						&[
							("from", &crate::i18n::decimal(format!("{:.3}", a))),
							("to", &crate::i18n::decimal(format!("{:.3}", b))),
							("length", &crate::i18n::decimal(format!("{:.3}", b - a))),
						],
					))
					.font(theme::mono(10.5))
					.color(theme::ACCENT),
				);
			}
		});
	}

	fn waveform(&mut self, ui: &mut Ui, state: &mut AppState, wave: Rect, r: &Region) {
		let p = ui.painter_at(wave);
		let v = state.view.audio.clone();
		let color = state
			.project()
			.track(r.track)
			.map_or(theme::ACCENT, |t| theme::track_color(t.color.as_deref()));
		let xs = |s: f64| wave.left() + v.x_of(s);
		// Clip region (outside is shaded).
		let region = Rect::from_x_y_ranges(xs(r.offset)..=xs(r.end()), wave.y_range());
		p.rect_filled(region, 0.0, theme::LANE);
		// Selection.
		if let Some((a, b)) = v.selection.map(|s| r.clamp(s)) {
			let sel = Rect::from_x_y_ranges(xs(a)..=xs(b).max(xs(a) + 1.0), wave.y_range());
			p.rect_filled(sel, 0.0, theme::ACCENT.gamma_multiply(0.18));
			p.vline(xs(a), wave.y_range(), Stroke::new(1.0, theme::ACCENT));
			if b > a {
				p.vline(xs(b), wave.y_range(), Stroke::new(1.0, theme::ACCENT));
			}
		}
		let peaks = state.peaks.get(&r.file);
		let peaks = match peaks {
			Peaks::Ready(p) => p,
			Peaks::Pending => {
				p.text(
					wave.center(),
					Align2::CENTER_CENTER,
					tr("loading waveform…"),
					theme::small(),
					theme::TEXT_FAINT,
				);
				return;
			}
			Peaks::Failed(e) => {
				p.text(
					wave.center(),
					Align2::CENTER_CENTER,
					format!("⚠ {e}"),
					theme::small(),
					theme::WARNING,
				);
				return;
			}
		};
		let sr = f64::from(peaks.sample_rate);
		let fpp = sr / f64::from(v.px_per_second);
		let finest = peaks.levels.first().map_or(1, |l| l.frames_per_bucket);
		// Zoomed in past the finest peak level: draw from the samples.
		let samples = if fpp < f64::from(finest) {
			match state.peaks.samples(&r.file) {
				Samples::Ready(b) => Some(b),
				_ => None,
			}
		} else {
			None
		};
		let channels = usize::from(peaks.num_channels).max(1);
		let lane_h = wave.height() / channels as f32;
		let gain = cadenza_core::db_to_gain(r.gain_db);
		let level = peaks.level_for(fpp.max(1.0));
		for c in 0..channels {
			let lane = Rect::from_min_size(
				pos2(wave.left(), wave.top() + c as f32 * lane_h),
				vec2(wave.width(), lane_h),
			);
			let mid = lane.center().y;
			let half = lane.height() * 0.5 - 2.0;
			p.hline(wave.x_range(), mid, Stroke::new(1.0, theme::GRID_BEAT));
			if c > 0 {
				p.hline(wave.x_range(), lane.top(), Stroke::new(1.0, theme::BORDER));
			}
			let inside = |s: f64| s >= r.offset && s <= r.end();
			let wave_color = |s: f64| {
				if inside(s) {
					theme::mix(color, Color32::WHITE, 0.25)
				} else {
					theme::TEXT_FAINT
				}
			};
			let fade = |s: f64| fade_gain(r, s);
			if let Some(buf) = &samples
				&& fpp <= 4.0
			{
				// A few samples per pixel at most: a line through the samples.
				let ch = buf.channel(c.min(buf.num_channels() - 1));
				let s0 = (v.scroll * sr).floor().max(0.0) as usize;
				let s1 = ((v.seconds_at(wave.width()) * sr).ceil() as usize + 1).min(ch.len());
				let pts: Vec<Pos2> = (s0..s1)
					.map(|i| {
						let s = i as f64 / sr;
						let g = if inside(s) { gain * fade(s) } else { 1.0 };
						pos2(xs(s), mid - (ch[i] * g).clamp(-1.0, 1.0) * half)
					})
					.collect();
				if pts.len() > 1 {
					p.add(Shape::line(
						pts.clone(),
						Stroke::new(1.0, wave_color(v.scroll)),
					));
				}
				if v.px_per_second as f64 / sr > 6.0 {
					for q in pts {
						p.circle_filled(q, 1.5, theme::TEXT);
					}
				}
				continue;
			}
			let mut x = wave.left().floor();
			while x < wave.right() {
				let s0 = v.seconds_at(x - wave.left());
				let s1 = v.seconds_at(x + 1.0 - wave.left());
				let f0 = (s0 * sr).max(0.0) as u64;
				let f1 = ((s1 * sr).max(0.0) as u64).max(f0 + 1);
				if f0 >= peaks.frames {
					break;
				}
				let mm = match &samples {
					Some(buf) => {
						let ch = buf.channel(c.min(buf.num_channels() - 1));
						let (a, b) = (f0 as usize, (f1 as usize).min(ch.len()));
						ch.get(a..b).unwrap_or(&[]).iter().fold(
							cadenza_audio_io::MinMax::EMPTY,
							|m, &x| cadenza_audio_io::MinMax {
								min: m.min.min(x),
								max: m.max.max(x),
							},
						)
					}
					None => level.range(c, f0, f1),
				};
				if !mm.is_empty() {
					let g = if inside(s0) { gain * fade(s0) } else { 1.0 };
					let top = mid - (mm.max * g).clamp(-1.0, 1.0) * half;
					let bottom = mid - (mm.min * g).clamp(-1.0, 1.0) * half;
					p.vline(
						x + 0.5,
						top..=bottom.max(top + 1.0),
						Stroke::new(1.0, wave_color(s0)),
					);
				}
				x += 1.0;
			}
		}
		// Shade outside the clip region.
		let shade = Color32::from_black_alpha(110);
		p.rect_filled(
			Rect::from_x_y_ranges(wave.left()..=xs(r.offset), wave.y_range()),
			0.0,
			shade,
		);
		p.rect_filled(
			Rect::from_x_y_ranges(xs(r.end())..=wave.right(), wave.y_range()),
			0.0,
			shade,
		);
		// Fade curves.
		let curve = |from: f64, to: f64, rising: bool, kind: FadeCurve| {
			let n = 32;
			(0..=n)
				.map(|i| {
					let t = i as f64 / n as f64;
					let g = fade_shape(kind, if rising { t } else { 1.0 - t });
					pos2(
						xs(from + (to - from) * t),
						wave.bottom() - 2.0 - g as f32 * (wave.height() - 4.0),
					)
				})
				.collect::<Vec<_>>()
		};
		let stroke = Stroke::new(1.5, Color32::from_white_alpha(170));
		if let Some(f) = r.fade_in {
			p.add(Shape::line(
				curve(r.offset, r.offset + f.seconds, true, f.curve),
				stroke,
			));
		}
		if let Some(f) = r.fade_out {
			p.add(Shape::line(
				curve(r.end() - f.seconds, r.end(), false, f.curve),
				stroke,
			));
		}
	}

	/// Region, fade and gain handles plus the selection gesture.
	fn handles(&mut self, ui: &mut Ui, state: &mut AppState, wave: Rect, r: &Region) {
		let v = state.view.audio.clone();
		let xs = |s: f64| wave.left() + v.x_of(s);
		let secs = |x: f32| v.seconds_at(x - wave.left());
		let fade_in = r.fade_in.map_or(0.0, |f| f.seconds);
		let fade_out = r.fade_out.map_or(0.0, |f| f.seconds);
		let fin_pos = pos2(xs(r.offset + fade_in), wave.top() + 6.0);
		let fout_pos = pos2(xs(r.end() - fade_out), wave.top() + 6.0);
		let gain_y = wave.top() + 18.0;
		let gain_pos = pos2((xs(r.offset) + xs(r.end())) * 0.5, gain_y);
		let p = ui.painter_at(wave);
		// Region edges.
		for x in [xs(r.offset), xs(r.end())] {
			p.vline(x, wave.y_range(), Stroke::new(2.0, theme::ACCENT));
		}
		for c in [fin_pos, fout_pos] {
			p.rect(
				Rect::from_center_size(c, vec2(9.0, 9.0)),
				CornerRadius::same(1),
				theme::BG_RAISED,
				Stroke::new(1.5, Color32::WHITE),
				StrokeKind::Middle,
			);
		}
		let gain_text = format!("{:+.1} dB", self.gain_value(r));
		let galley = p.layout_no_wrap(gain_text, theme::small(), theme::TEXT);
		let pill = Rect::from_center_size(gain_pos, galley.size() + vec2(12.0, 4.0));
		p.rect(
			pill,
			CornerRadius::same(7),
			theme::BG_RAISED,
			Stroke::new(1.0, theme::BORDER_STRONG),
			StrokeKind::Inside,
		);
		p.galley(pill.min + vec2(6.0, 2.0), galley, theme::TEXT);

		let resp = ui.interact(wave, Id::new("audio-editor-wave"), Sense::click_and_drag());
		let hit = |pos: Pos2| -> Grab {
			if pill.expand(2.0).contains(pos) {
				Grab::Gain
			} else if pos.distance(fin_pos) <= HANDLE + 2.0 {
				Grab::FadeIn
			} else if pos.distance(fout_pos) <= HANDLE + 2.0 {
				Grab::FadeOut
			} else if (pos.x - xs(r.offset)).abs() <= HANDLE {
				Grab::Start
			} else if (pos.x - xs(r.end())).abs() <= HANDLE {
				Grab::End
			} else {
				Grab::Select
			}
		};
		if let Some(pos) = resp.hover_pos() {
			let icon = match self.drag.map(|d| d.grab).unwrap_or_else(|| hit(pos)) {
				Grab::Start | Grab::End | Grab::FadeIn | Grab::FadeOut => {
					CursorIcon::ResizeHorizontal
				}
				Grab::Gain => CursorIcon::ResizeVertical,
				Grab::Select => CursorIcon::Text,
			};
			ui.ctx().set_cursor_icon(icon);
		}
		if resp.drag_started_by(PointerButton::Primary)
			&& let Some(press) = ui
				.input(|i| i.pointer.press_origin())
				.or(resp.interact_pointer_pos())
		{
			let grab = hit(press);
			let origin = match grab {
				Grab::Start => r.offset,
				Grab::End => r.end(),
				Grab::FadeIn => r.offset + fade_in,
				Grab::FadeOut => r.end() - fade_out,
				Grab::Gain => f64::from(r.gain_db),
				Grab::Select => secs(press.x).clamp(r.offset, r.end()),
			};
			self.drag = Some(Drag {
				grab,
				origin,
				press,
				value: origin,
			});
		}
		if let Some(d) = &mut self.drag {
			if let Some(pos) = ui.ctx().pointer_interact_pos() {
				d.value = match d.grab {
					Grab::Gain => {
						(d.origin + f64::from(d.press.y - pos.y) * 0.1).clamp(-60.0, 12.0)
					}
					_ => d.origin + secs(pos.x) - secs(d.press.x),
				};
				if d.grab == Grab::Select {
					let a = d.origin;
					let b = d.value.clamp(r.offset, r.end());
					state.view.audio.selection = Some((a.min(b), a.max(b)));
				}
			}
			// Preview of the dragged handle.
			let preview = |x: f32| {
				p.vline(x, wave.y_range(), Stroke::new(1.0, Color32::WHITE));
			};
			match d.grab {
				Grab::Start | Grab::End | Grab::FadeIn | Grab::FadeOut => preview(xs(d.value)),
				_ => {}
			}
			if ui.input(|i| i.pointer.any_released()) {
				let d = self.drag.take().expect("drag");
				let clip = r.clip.id;
				let project = state.project();
				let cmd = match d.grab {
					Grab::Start => ae::set_region_start(project, clip, d.value),
					Grab::End => ae::set_region_end(project, clip, d.value),
					Grab::FadeIn => ae::set_fades(
						project,
						clip,
						Some(ClipFade::new(
							(d.value - r.offset).max(0.0),
							r.fade_in.map_or(FadeCurve::Linear, |f| f.curve),
						)),
						r.fade_out,
					),
					Grab::FadeOut => ae::set_fades(
						project,
						clip,
						r.fade_in,
						Some(ClipFade::new(
							(r.end() - d.value).max(0.0),
							r.fade_out.map_or(FadeCurve::Linear, |f| f.curve),
						)),
					),
					Grab::Gain => ae::set_gain(project, clip, d.value as f32),
					Grab::Select => None,
				};
				state.audio_edit(cmd);
			}
		}
		if resp.clicked()
			&& let Some(pos) = resp.interact_pointer_pos()
			&& hit(pos) == Grab::Select
		{
			// A click places the cursor (empty selection) and moves the playhead there.
			let s = secs(pos.x).clamp(r.offset, r.end());
			state.view.audio.selection = Some((s, s));
			let tick = r.tick_of(state.project(), s);
			state.seek(tick);
		}
		if resp.double_clicked() {
			state.view.audio.selection = Some((r.offset, r.end()));
		}
	}

	/// Clip gain shown on the pill (the dragged value while dragging it).
	fn gain_value(&self, r: &Region) -> f32 {
		match self.drag {
			Some(d) if d.grab == Grab::Gain => d.value as f32,
			_ => r.gain_db,
		}
	}

	/// Keyboard shortcuts while the audio editor has the focus. Returns true when a
	/// key was handled.
	pub fn shortcuts(&mut self, ctx: &egui::Context, state: &mut AppState) {
		if state.view.audio.mode == EditorMode::Notes {
			self.notes.shortcuts(ctx, state);
			return;
		}
		let Some(clip) = state.view.audio.clip else {
			return;
		};
		let Some(r) = ae::region(state.project(), clip) else {
			return;
		};
		let pressed = |k: egui::Key| ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, k));
		let sel = state.view.audio.selection.map(|s| r.clamp(s));
		if (pressed(egui::Key::Delete) || pressed(egui::Key::Backspace))
			&& let Some(s) = sel.filter(|(a, b)| b - a > 1e-6)
		{
			delete_range(state, clip, s);
		}
		if pressed(egui::Key::S)
			&& let Some((cmd, _)) = sel.and_then(|s| ae::split_at(state.project(), clip, s.0))
		{
			state.apply(cmd);
		}
		if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::A)) {
			state.view.audio.selection = Some((r.offset, r.end()));
		}
		if pressed(egui::Key::Escape) {
			state.view.audio.selection = None;
		}
	}
}

fn delete_range(state: &mut AppState, clip: cadenza_core::ClipId, s: (f64, f64)) {
	if let Some((cmd, _)) = ae::delete_range(state.project(), clip, s)
		&& state.apply(cmd)
	{
		state.view.audio.selection = None;
	}
}

/// Fade-in or fade-out curve picker.
fn fade_picker(
	ui: &mut Ui,
	state: &mut AppState,
	clip: cadenza_core::ClipId,
	r: &Region,
	fade_in: bool,
) {
	let current = if fade_in { r.fade_in } else { r.fade_out };
	let name = |c: FadeCurve| {
		FADE_CURVES
			.iter()
			.find(|(k, _)| *k == c)
			.map_or("?", |(_, n)| crate::i18n::tr_str(n))
	};
	let label = format!(
		"{} {}",
		if fade_in {
			tr("Fade in:")
		} else {
			tr("Fade out:")
		},
		current.map_or(tr("none").to_owned(), |f| format!(
			"{} s {}",
			crate::i18n::num(f.seconds, 2),
			name(f.curve)
		))
	);
	ui.menu_button(RichText::new(label).font(theme::small()), |ui| {
		for (curve, n) in FADE_CURVES {
			let on = current.is_some_and(|f| f.curve == curve);
			if ui.selectable_label(on, crate::i18n::tr_str(n)).clicked() {
				let f = Some(ClipFade::new(
					current.map_or((r.duration * 0.1).min(0.5), |f| f.seconds),
					curve,
				));
				let (fi, fo) = if fade_in {
					(f, r.fade_out)
				} else {
					(r.fade_in, f)
				};
				let cmd = ae::set_fades(state.project(), clip, fi, fo);
				state.audio_edit(cmd);
				ui.close();
			}
		}
		ui.separator();
		if ui
			.add_enabled(current.is_some(), egui::Button::new(tr("No fade")))
			.clicked()
		{
			let (fi, fo) = if fade_in {
				(None, r.fade_out)
			} else {
				(r.fade_in, None)
			};
			let cmd = ae::set_fades(state.project(), clip, fi, fo);
			state.audio_edit(cmd);
			ui.close();
		}
	})
	.response
	.on_hover_text(tr(
		"Drag the white squares on the waveform to change the fade lengths",
	));
}

/// Gain of the clip fades at file second `s` (for drawing).
fn fade_gain(r: &Region, s: f64) -> f32 {
	let mut g = 1.0;
	if let Some(f) = r.fade_in
		&& s < r.offset + f.seconds
	{
		g *= fade_shape(f.curve, ((s - r.offset) / f.seconds).clamp(0.0, 1.0));
	}
	if let Some(f) = r.fade_out
		&& s > r.end() - f.seconds
	{
		g *= fade_shape(f.curve, ((r.end() - s) / f.seconds).clamp(0.0, 1.0));
	}
	g as f32
}

/// Fade gain at position `t` (0 = silent end, 1 = full level).
fn fade_shape(curve: FadeCurve, t: f64) -> f64 {
	let t = t.clamp(0.0, 1.0);
	match curve {
		FadeCurve::Linear => t,
		FadeCurve::EqualPower => (t * std::f64::consts::FRAC_PI_2).sin(),
		FadeCurve::Exponential => t * t,
		FadeCurve::SCurve => 0.5 - 0.5 * (std::f64::consts::PI * t).cos(),
	}
}

/// Seconds ruler of the audio editor.
pub(crate) fn ruler_paint(ui: &Ui, state: &AppState, r: Rect) {
	let p = ui.painter_at(r);
	p.rect_filled(r, 0.0, theme::BG_RAISED);
	p.hline(
		r.x_range(),
		r.bottom() - 0.5,
		Stroke::new(1.0, theme::BORDER),
	);
	let v = &state.view.audio;
	// A step of 1, 2 or 5 x 10^n seconds giving >= 70 px between labels.
	let min = 70.0 / f64::from(v.px_per_second);
	let mut step = 10f64.powf(min.log10().floor());
	for k in [1.0, 2.0, 5.0, 10.0] {
		if step * k >= min {
			step *= k;
			break;
		}
	}
	let first = (v.scroll / step).floor() as i64;
	let last = (v.seconds_at(r.width()) / step).ceil() as i64;
	for i in first..=last {
		let s = i as f64 * step;
		let x = r.left() + v.x_of(s);
		p.vline(
			x,
			r.top() + 8.0..=r.bottom(),
			Stroke::new(1.0, theme::TEXT_FAINT),
		);
		let text = if step < 0.01 {
			format!("{s:.4}s")
		} else if step < 1.0 {
			format!("{s:.2}s")
		} else {
			format!("{s:.0}s")
		};
		p.text(
			pos2(x + 3.0, r.top() + 2.0),
			Align2::LEFT_TOP,
			text,
			theme::mono(10.0),
			theme::TEXT_DIM,
		);
	}
}
