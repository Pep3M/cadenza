//! Notes mode of the audio editor (NE-6): the clip's detected notes as blobs on a pitch
//! grid (thickness = level), the resulting pitch curve over them, sibilants hatched,
//! doubtful notes outlined amber with a one-click split, tools (retune / move, vibrato,
//! drift, formant, level, split) and the macros column (key, Correct Pitch, Quantize Time,
//! Leveling, sibilants, detection). Every gesture is one command through `AppState`.

use std::collections::BTreeSet;

use cadenza_core::{AudioNoteId, ClipId};
use cadenza_model::{AudioNoteFlag, NoteEdits};
use cadenza_theory::Scale;
use egui::{
	Align2, Color32, CornerRadius, CursorIcon, Id, Mesh, PointerButton, Pos2, Rect, RichText,
	Sense, Shape, Stroke, StrokeKind, Ui, pos2, vec2,
};

use crate::i18n::{tr, tr_str, trf, trn};
use crate::state::AppState;
use crate::state::audio_edit::Region;
use crate::state::note_editor::{self as ne, KeyChoice, NoteTool};
use crate::theme;

/// Width of the key column.
pub const KEYS: f32 = 40.0;
/// Width of the macros column.
pub const MACROS: f32 = 262.0;
/// Narrowest canvas the macros column leaves.
const MIN_CANVAS: f32 = 420.0;
/// Pixels before a Main-tool drag picks its axis.
const AXIS_LOCK: f32 = 4.0;
const ROOTS: [&str; 12] = [
	"C", "C#", "D", "Eb", "E", "F", "F#", "G", "Ab", "A", "Bb", "B",
];
const GRIDS: [(f64, &str); 4] = [(1.0, "1/4"), (0.5, "1/8"), (0.25, "1/16"), (0.125, "1/32")];

#[derive(Debug, Clone, Copy, PartialEq)]
enum Axis {
	Pitch,
	Time,
}

#[derive(Debug, Clone)]
enum Gesture {
	/// Notes being dragged with the tool (Main: axis chosen after a few pixels).
	Notes {
		ids: BTreeSet<AudioNoteId>,
		axis: Option<Axis>,
	},
	/// Rubber band from the press.
	Band { add: bool },
}

#[derive(Debug, Clone)]
struct Drag {
	press: Pos2,
	gesture: Gesture,
	moved: bool,
}

/// Layout of the notes mode inside the editor's area below the toolbar.
#[derive(Debug, Clone, Copy)]
pub struct Layout {
	pub ruler: Rect,
	pub keys: Rect,
	pub canvas: Rect,
	pub macros: Option<Rect>,
}

impl Layout {
	/// `area` = everything under the toolbar (ruler included, `ruler_h` high).
	pub fn of(area: Rect, ruler_h: f32, macros: bool) -> Self {
		let side = (macros && area.width() >= KEYS + MIN_CANVAS + MACROS)
			.then(|| Rect::from_min_max(pos2(area.right() - MACROS, area.top()), area.max));
		let right = side.map_or(area.right(), |r| r.left());
		let ruler = Rect::from_min_max(
			pos2(area.left() + KEYS, area.top()),
			pos2(right, area.top() + ruler_h),
		);
		let keys = Rect::from_min_max(
			pos2(area.left(), ruler.bottom()),
			pos2(area.left() + KEYS, area.bottom()),
		);
		let canvas = Rect::from_min_max(
			pos2(keys.right(), ruler.bottom()),
			pos2(right, area.bottom()),
		);
		Self {
			ruler,
			keys,
			canvas,
			macros: side,
		}
	}
}

/// View-local state of the notes mode.
#[derive(Default)]
pub struct NoteEditorUi {
	drag: Option<Drag>,
	/// Middle-button pan in progress.
	pan: bool,
	/// The resulting pitch curve and the edits it was made for.
	curve: Option<(ClipId, NoteEdits, Vec<Option<f64>>)>,
	/// Sibilant level while its slider is dragged.
	sibilants: Option<f32>,
	/// Canvas in the last frame (tests).
	pub canvas: Option<Rect>,
}

struct View<'a> {
	canvas: Rect,
	low: f64,
	high: f64,
	scroll: f64,
	pps: f64,
	r: &'a Region,
}

impl View<'_> {
	fn x(&self, s: f64) -> f32 {
		self.canvas.left() + ((s - self.scroll) * self.pps) as f32
	}
	fn s(&self, x: f32) -> f64 {
		self.scroll + f64::from(x - self.canvas.left()) / self.pps
	}
	fn row(&self) -> f32 {
		self.canvas.height() / (self.high - self.low).max(1.0) as f32
	}
	fn y(&self, pitch: f64) -> f32 {
		self.canvas.bottom() - ((pitch - self.low) as f32) * self.row()
	}
	fn pitch(&self, y: f32) -> f64 {
		self.low + f64::from((self.canvas.bottom() - y) / self.row())
	}
}

fn scale_label(s: &Scale) -> String {
	cadenza_mcp::tools::notes::scale_name(s)
}

impl NoteEditorUi {
	/// The notes part of the toolbar: tools, key, snap, macros toggle, status.
	pub fn toolbar(&mut self, ui: &mut Ui, state: &mut AppState, r: &Region) {
		let tool = state.view.audio.notes.tool;
		for t in NoteTool::ALL {
			let label = format!("{} {}", t.glyph(), tr_str(t.label()));
			if super::arrange::toolbar_chip(
				ui,
				tool == t,
				&label,
				Some(t.key()),
				theme::BG_WIDGET,
				theme::TEXT,
			)
			.on_hover_text(tr_str(t.tip()))
			.clicked()
			{
				state.view.audio.notes.tool = t;
			}
		}
		super::arrange::toolbar_divider(ui);
		let edits = ne::clip_notes(state.project(), r.clip.id).map(|(e, _)| e);
		let snap = state.view.audio.notes.snap;
		if super::arrange::toolbar_chip(
			ui,
			snap,
			tr("Snap"),
			None,
			theme::ACCENT_DIM,
			theme::ACCENT_TEXT,
		)
		.on_hover_text(tr(
			"Pitch moves land on the key's tones (hold Alt to invert)",
		))
		.clicked()
		{
			state.view.audio.notes.snap = !snap;
		}
		if let Some(e) = &edits {
			key_combo(ui, state, e);
		}
		if let Some(e) = &edits {
			let s = e.summary();
			ui.label(
				RichText::new(format!(
					"{} · {} · {}",
					trn("{n} note", "{n} notes", s.count),
					trf("{n} edited", &[("n", &s.edited)]),
					trf(
						"avg {cents} c off",
						&[(
							"cents",
							&crate::i18n::decimal(format!("{:.1}", s.avg_error_cents))
						)]
					),
				))
				.font(theme::small())
				.color(theme::TEXT_DIM),
			);
		}
		if state.note_renders.rendering() == Some(r.clip.id) {
			ui.spinner();
			ui.label(
				RichText::new(tr("rendering…"))
					.font(theme::small())
					.color(theme::TEXT_FAINT),
			)
			.on_hover_text(tr(
				"The edited sound is being rendered; playback uses it as soon as it is ready",
			));
		}
		let macros = state.view.audio.notes.macros;
		if super::arrange::toolbar_chip(
			ui,
			macros,
			tr("Macros"),
			None,
			theme::BG_WIDGET,
			theme::TEXT,
		)
		.on_hover_text(tr(
			"Show the Correct Pitch, Quantize Time and Leveling column",
		))
		.clicked()
		{
			state.view.audio.notes.macros = !macros;
		}
	}

	pub fn show(&mut self, ui: &mut Ui, state: &mut AppState, layout: Layout, r: &Region) {
		let canvas = layout.canvas;
		self.canvas = Some(canvas);
		let clip = r.clip.id;
		let Some((edits, _)) = ne::clip_notes(state.project(), clip) else {
			self.curve = None;
			self.empty(ui, state, layout, r);
			return;
		};
		if state.view.audio.notes.framed != Some(clip) {
			state.view.audio.notes.frame(&edits);
			state.view.audio.notes.framed = Some(clip);
		}
		self.navigate(ui, state, canvas, r);
		super::audio_editor::ruler_seek(ui, state, layout.ruler, canvas.left(), r);
		let analysis = state.note_analyses.get(&r.file);
		if let Some(a) = &analysis {
			let fresh = self
				.curve
				.as_ref()
				.is_some_and(|(c, e, _)| *c == clip && *e == edits);
			if !fresh {
				self.curve = Some((clip, edits.clone(), ne::edited_curve(a, &edits)));
			}
		}
		let v = View {
			canvas,
			low: state.view.audio.notes.low,
			high: state.view.audio.notes.high,
			scroll: state.view.audio.scroll,
			pps: f64::from(state.view.audio.px_per_second),
			r,
		};
		let key = state.view.audio.notes.scale(&edits);
		super::audio_editor::ruler_paint(ui, state, layout.ruler);
		self.keys(ui, &v, layout.keys, key.as_ref());
		let p = ui.painter_at(canvas);
		grid(&p, &v, key.as_ref());
		let preview = self.preview(ui, state, &v, &edits, key.as_ref());
		self.paint_notes(ui, state, &v, &edits, analysis.as_deref(), &preview);
		self.interact(ui, state, &v, &edits, key.as_ref());
		// Playhead.
		let s = r.seconds_of(state.project(), state.position());
		let x = v.x(s);
		if (canvas.left()..=canvas.right()).contains(&x) {
			ui.painter().vline(
				x,
				layout.ruler.top()..=canvas.bottom(),
				Stroke::new(1.5, theme::PLAYHEAD),
			);
		}
		if state.note_analyses.loading(&r.file) {
			status(ui, canvas, tr("analyzing the pitch…"));
		}
		if let Some(m) = layout.macros {
			self.macros(ui, state, m, &edits, analysis.as_deref());
		}
	}

	fn empty(&mut self, ui: &mut Ui, state: &mut AppState, layout: Layout, r: &Region) {
		let area = layout.canvas.union(layout.keys);
		ui.painter().rect_filled(area, 0.0, theme::LANE);
		let busy =
			state.note_analyses.loading(&r.file) || state.note_analyses.detect.contains(&r.clip.id);
		let mut child = ui.new_child(
			egui::UiBuilder::new()
				.max_rect(Rect::from_center_size(area.center(), vec2(360.0, 90.0)))
				.layout(egui::Layout::top_down(egui::Align::Center)),
		);
		if let Some(e) = state.note_analyses.failure(&r.file) {
			child.label(RichText::new(format!("⚠ {e}")).color(theme::WARNING));
			return;
		}
		if busy {
			child.spinner();
			child.label(RichText::new(tr("Finding the notes…")).color(theme::TEXT_DIM));
			return;
		}
		child.label(
			RichText::new(tr(
				"Detect the notes of this clip to retune, move and reshape them",
			))
			.color(theme::TEXT_DIM),
		);
		child.add_space(6.0);
		if theme::primary_button(&mut child, tr("Detect notes")).clicked() {
			state.ensure_note_analysis(r.clip.id);
			state.detect_notes(r.clip.id, None);
		}
	}

	/// Plain wheel = pitch scroll, Shift = time, Ctrl = time zoom, Alt = pitch zoom,
	/// middle-drag pans both ways.
	fn navigate(&mut self, ui: &Ui, state: &mut AppState, canvas: Rect, r: &Region) {
		let (pressed, down, delta) = ui.input(|i| {
			(
				i.pointer.button_pressed(PointerButton::Middle),
				i.pointer.middle_down(),
				i.pointer.delta(),
			)
		});
		let hover = ui.ctx().pointer_hover_pos();
		if pressed && hover.is_some_and(|p| canvas.contains(p)) && ui.ui_contains_pointer() {
			self.pan = true;
		}
		if !down {
			self.pan = false;
		}
		if self.pan {
			let audio = &mut state.view.audio;
			let rows = canvas.height() / (audio.notes.high - audio.notes.low).max(1.0) as f32;
			audio.scroll = (audio.scroll - f64::from(delta.x) / f64::from(audio.px_per_second))
				.clamp(0.0, (r.end() + 1.0).max(0.0));
			audio.notes.scroll(f64::from(delta.y / rows));
			ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
			return;
		}
		let Some(p) = hover else {
			return;
		};
		if !canvas.contains(p) || !ui.ui_contains_pointer() {
			return;
		}
		let (zoom, scroll, mods) =
			ui.input(|i| (i.zoom_delta(), i.smooth_scroll_delta, i.modifiers));
		let audio = &mut state.view.audio;
		if zoom != 1.0 {
			audio.zoom_around(zoom, (p.x - canvas.left()).max(0.0));
			return;
		}
		let rows = canvas.height() / (audio.notes.high - audio.notes.low).max(1.0) as f32;
		if mods.alt && scroll.y != 0.0 {
			let pitch = audio.notes.low + f64::from((canvas.bottom() - p.y) / rows);
			audio
				.notes
				.zoom(if scroll.y > 0.0 { 0.9 } else { 1.1 }, pitch);
		} else if mods.shift || scroll.x != 0.0 {
			let dx = if scroll.x != 0.0 { scroll.x } else { scroll.y };
			audio.scroll = (audio.scroll - f64::from(dx) / f64::from(audio.px_per_second))
				.clamp(0.0, (r.end() + 1.0).max(0.0));
		} else if scroll.y != 0.0 {
			audio.notes.scroll(f64::from(scroll.y / rows));
		}
	}

	fn keys(&self, ui: &Ui, v: &View, rect: Rect, key: Option<&Scale>) {
		let p = ui.painter_at(rect);
		p.rect_filled(rect, 0.0, theme::BG_PANEL);
		let row = v.row();
		for k in v.low.floor() as i32..=v.high.ceil() as i32 {
			if !(0..=127).contains(&k) {
				continue;
			}
			let pitch = k as u8;
			let y = v.y(f64::from(k));
			let r = Rect::from_x_y_ranges(rect.x_range(), (y - row / 2.0)..=(y + row / 2.0));
			let black = crate::state::piano::is_black(pitch);
			let fill = if black {
				theme::KEY_BLACK
			} else {
				theme::KEY_WHITE
			};
			let w = if black {
				rect.width() * 0.62
			} else {
				rect.width()
			};
			p.rect_filled(
				Rect::from_min_size(r.min, vec2(w, r.height())).shrink2(vec2(0.0, 0.5)),
				0.0,
				fill,
			);
			let in_key = key.is_none_or(|s| s.contains(pitch));
			if pitch.is_multiple_of(12) || (row >= 18.0 && !black && in_key) {
				let name = cadenza_theory::format_pitch(pitch, false);
				p.text(
					pos2(rect.right() - 3.0, y),
					Align2::RIGHT_CENTER,
					name,
					theme::mono(9.5),
					if black {
						theme::TEXT_DIM
					} else {
						theme::TEXT_ON_COLOR
					},
				);
			}
		}
		p.vline(
			rect.right() - 0.5,
			rect.y_range(),
			Stroke::new(1.0, theme::BORDER),
		);
	}

	/// Where the notes being dragged are drawn: (id, pitch, time shift).
	fn preview(
		&self,
		ui: &Ui,
		state: &AppState,
		v: &View,
		edits: &NoteEdits,
		key: Option<&Scale>,
	) -> Vec<(AudioNoteId, f64, f64)> {
		let Some(d) = &self.drag else {
			return Vec::new();
		};
		let Gesture::Notes { ids, axis } = &d.gesture else {
			return Vec::new();
		};
		let Some(pos) = ui.ctx().pointer_latest_pos() else {
			return Vec::new();
		};
		let tool = state.view.audio.notes.tool;
		match (tool, axis) {
			(NoteTool::Main, Some(Axis::Pitch)) => {
				let delta = f64::from((d.press.y - pos.y) / v.row());
				let snap = snap_choice(ui, state, key);
				ne::moved_pitches(edits, ids, delta, snap.as_ref().map(|k| k.as_ref()))
					.into_iter()
					.map(|(id, p)| (id, p, 0.0))
					.collect()
			}
			(NoteTool::Main, Some(Axis::Time)) => {
				let dt = f64::from(pos.x - d.press.x) / v.pps;
				ids.iter()
					.filter_map(|id| edits.note(*id).map(|n| (*id, n.result_pitch(), dt)))
					.collect()
			}
			_ => Vec::new(),
		}
	}

	fn paint_notes(
		&self,
		ui: &Ui,
		state: &AppState,
		v: &View,
		edits: &NoteEdits,
		analysis: Option<&cadenza_notes::analysis::Analysis>,
		preview: &[(AudioNoteId, f64, f64)],
	) {
		let p = ui.painter_at(v.canvas);
		let color = state
			.project()
			.track(v.r.track)
			.map_or(theme::ACCENT, |t| theme::track_color(t.color.as_deref()));
		let selection = &state.view.audio.notes.selection;
		let row = v.row();
		// Clip region: outside is dimmed.
		for (a, b) in [(f64::MIN, v.r.offset), (v.r.end(), f64::MAX)] {
			let x0 = v.x(a.max(v.scroll - 1.0)).max(v.canvas.left());
			let x1 = v
				.x(b.min(v.s(v.canvas.right()) + 1.0))
				.min(v.canvas.right());
			if x1 > x0 {
				p.rect_filled(
					Rect::from_x_y_ranges(x0..=x1, v.canvas.y_range()),
					0.0,
					Color32::from_black_alpha(90),
				);
			}
		}
		// Sibilants: hatched strips along the bottom.
		for s in &edits.sibilants {
			let (x0, x1) = (v.x(s.start), v.x(s.end));
			if x1 < v.canvas.left() || x0 > v.canvas.right() {
				continue;
			}
			let r = Rect::from_x_y_ranges(
				x0..=x1.max(x0 + 2.0),
				(v.canvas.bottom() - 12.0)..=v.canvas.bottom(),
			);
			p.rect_filled(r, 0.0, theme::WARNING_SOFT);
			let mut x = r.left() - r.height();
			while x < r.right() {
				let a = pos2(x.max(r.left()), r.bottom() - (x.max(r.left()) - x));
				let b = pos2(
					(x + r.height()).min(r.right()),
					r.bottom() - ((x + r.height()).min(r.right()) - x),
				);
				p.line_segment([a, b], Stroke::new(1.0, theme::WARNING.gamma_multiply(0.6)));
				x += 5.0;
			}
		}
		let loud = analysis.map_or(-12.0, loudest);
		let level_at = |s: f64| -> f64 {
			analysis
				.and_then(|a| a.frame_at(s).map(|i| a.frames[i].level_db))
				.unwrap_or(loud - 12.0)
		};
		for n in &edits.notes {
			let moved = preview.iter().find(|(id, _, _)| *id == n.id);
			let (a, b) = ne::note_span(n);
			let dt = moved.map_or(0.0, |m| m.2);
			let (a, b) = (a + dt, b + dt);
			let (x0, x1) = (v.x(a), v.x(b));
			if x1 < v.canvas.left() || x0 > v.canvas.right() {
				continue;
			}
			let pitch = moved.map_or(n.result_pitch(), |m| m.1);
			let y = v.y(pitch);
			let sel = selection.contains(&n.id);
			let fill = if sel {
				theme::NOTE_SELECTED
			} else if n.is_edited() {
				theme::mix(color, Color32::WHITE, 0.3)
			} else {
				color
			};
			// The sung position, when the note was retuned or moved.
			if n.is_edited() && ((n.center - pitch).abs() > 0.02 || (a - n.start).abs() > 1e-3) {
				let ghost = Rect::from_x_y_ranges(
					v.x(n.start)..=v.x(n.end),
					(v.y(n.center) - row * 0.35)..=(v.y(n.center) + row * 0.35),
				);
				p.rect_stroke(
					ghost,
					CornerRadius::same(3),
					Stroke::new(1.0, color.gamma_multiply(0.45)),
					StrokeKind::Inside,
				);
			}
			// Blob: thickness follows the level over time.
			let half_max = (row * 0.46).max(3.0);
			let mut mesh = Mesh::default();
			let step = 2.0f32;
			let mut x = x0.max(v.canvas.left() - step);
			let end = x1.min(v.canvas.right() + step);
			let span = (b - a).max(1e-6);
			while x <= end {
				// Back to the sung time of this point of the note.
				let t = (f64::from(x - x0) / f64::from((x1 - x0).max(1e-3))).clamp(0.0, 1.0);
				let src = n.start + t * (n.end - n.start);
				let level = level_at(src);
				let w = ((level - (loud - 36.0)) / 36.0).clamp(0.12, 1.0) as f32;
				let half = half_max * w;
				let i = mesh.vertices.len() as u32;
				mesh.colored_vertex(pos2(x, y - half), fill.gamma_multiply(0.92));
				mesh.colored_vertex(pos2(x, y + half), fill.gamma_multiply(0.92));
				if i >= 2 {
					mesh.add_triangle(i - 2, i - 1, i);
					mesh.add_triangle(i - 1, i, i + 1);
				}
				x += step;
				let _ = span;
			}
			p.add(Shape::mesh(mesh));
			// Center line.
			p.hline(
				x0.max(v.canvas.left())..=x1.min(v.canvas.right()),
				y,
				Stroke::new(1.0, theme::TEXT_ON_COLOR.gamma_multiply(0.7)),
			);
			let outline = Rect::from_x_y_ranges(x0..=x1, (y - half_max)..=(y + half_max));
			if !n.flags.is_empty() {
				p.rect_stroke(
					outline.expand(1.5),
					CornerRadius::same(4),
					Stroke::new(1.5, theme::WARNING),
					StrokeKind::Outside,
				);
				for f in &n.flags {
					if let AudioNoteFlag::MaybeTwoNotes { at, .. } = f {
						let fx =
							x0 + ((at - n.start) / (n.end - n.start).max(1e-6)) as f32 * (x1 - x0);
						p.text(
							pos2(fx, outline.bottom() + 2.0),
							Align2::CENTER_TOP,
							"✂",
							theme::small(),
							theme::WARNING,
						);
					}
				}
			} else if sel {
				p.rect_stroke(
					outline.expand(1.0),
					CornerRadius::same(4),
					Stroke::new(1.0, Color32::WHITE),
					StrokeKind::Outside,
				);
			}
			if row >= 13.0 && x1 - x0 > 34.0 {
				let name =
					cadenza_theory::format_pitch(pitch.round().clamp(0.0, 127.0) as u8, false);
				let cents = ((pitch - pitch.round()) * 100.0).round();
				let label = if cents.abs() >= 1.0 {
					format!("{name} {cents:+.0}")
				} else {
					name
				};
				p.text(
					pos2(x0.max(v.canvas.left()) + 3.0, y - half_max - 1.0),
					Align2::LEFT_BOTTOM,
					label,
					theme::mono(9.5),
					theme::TEXT_DIM,
				);
			}
		}
		// Resulting pitch curve.
		if let (Some((_, _, curve)), Some(a)) = (&self.curve, analysis) {
			let mut line: Vec<Pos2> = Vec::new();
			let flush = |line: &mut Vec<Pos2>| {
				if line.len() > 1 {
					p.add(Shape::line(
						std::mem::take(line),
						Stroke::new(1.2, theme::TEXT.gamma_multiply(0.75)),
					));
				}
				line.clear();
			};
			let (s0, s1) = (v.s(v.canvas.left()) - 0.05, v.s(v.canvas.right()) + 0.05);
			for (f, c) in a.frames.iter().zip(curve) {
				if f.time < s0 || f.time > s1 {
					continue;
				}
				// Notes moved in time: their curve moves with them.
				let note = edits
					.notes
					.iter()
					.find(|n| (n.start..=n.end).contains(&f.time));
				let (t, shift) = match note {
					Some(n) => {
						let (a, b) = ne::note_span(n);
						let moved = preview.iter().find(|(id, _, _)| *id == n.id);
						let dt = moved.map_or(0.0, |m| m.2);
						let dp = moved.map_or(0.0, |m| m.1 - n.result_pitch());
						let k = (f.time - n.start) / (n.end - n.start).max(1e-6);
						(a + dt + k * (b - a), dp)
					}
					None => (f.time, 0.0),
				};
				match c {
					Some(pitch) => line.push(pos2(v.x(t), v.y(pitch + shift))),
					None => flush(&mut line),
				}
			}
			flush(&mut line);
		}
		// Rubber band.
		if let Some(Drag {
			press,
			gesture: Gesture::Band { .. },
			..
		}) = &self.drag
			&& let Some(pos) = ui.ctx().pointer_latest_pos()
		{
			let band = Rect::from_two_pos(*press, pos);
			p.rect_filled(band, 0.0, theme::ACCENT.gamma_multiply(0.12));
			p.rect_stroke(
				band,
				0.0,
				Stroke::new(1.0, theme::ACCENT),
				StrokeKind::Inside,
			);
		}
	}

	fn interact(
		&mut self,
		ui: &mut Ui,
		state: &mut AppState,
		v: &View,
		edits: &NoteEdits,
		key: Option<&Scale>,
	) {
		let clip = v.r.clip.id;
		let resp = ui.interact(
			v.canvas,
			Id::new("note-editor-canvas"),
			Sense::click_and_drag(),
		);
		let tool = state.view.audio.notes.tool;
		let hover = resp.hover_pos();
		let hit = |pos: Pos2| ne::note_at(edits, v.s(pos.x), v.pitch(pos.y), 0.6);
		let flag_hit = |pos: Pos2| -> Option<(AudioNoteId, f64)> {
			edits.notes.iter().find_map(|n| {
				n.flags.iter().find_map(|f| match f {
					AudioNoteFlag::MaybeTwoNotes { at, .. } => {
						let (x, y) = (
							v.x(*at),
							v.y(n.result_pitch()) + (v.row() * 0.46).max(3.0) + 8.0,
						);
						(pos.distance(pos2(x, y)) < 9.0).then_some((n.id, *at))
					}
					_ => None,
				})
			})
		};
		if let Some(pos) = hover {
			if let Some((_, at)) = flag_hit(pos) {
				ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
				resp.clone().on_hover_text(trf(
					"May hold two notes: click to split at {at} s",
					&[("at", &crate::i18n::decimal(format!("{at:.3}")))],
				));
			} else if let Some(id) = hit(pos)
				&& let Some(n) = edits.note(id)
			{
				ui.ctx().set_cursor_icon(match tool {
					NoteTool::Separation => CursorIcon::Text,
					NoteTool::Main => CursorIcon::Grab,
					_ => CursorIcon::ResizeVertical,
				});
				if self.drag.is_none() {
					resp.clone().on_hover_text(note_tip(n));
				}
			}
		}
		let mods = ui.input(|i| i.modifiers);
		if resp.drag_started_by(PointerButton::Primary)
			&& let Some(press) = resp.interact_pointer_pos()
		{
			let gesture = match hit(press) {
				Some(id) if tool != NoteTool::Separation => {
					let sel = &mut state.view.audio.notes.selection;
					if mods.shift {
						sel.insert(id);
					} else if !sel.contains(&id) {
						sel.clear();
						sel.insert(id);
					}
					Gesture::Notes {
						ids: sel.clone(),
						axis: (tool != NoteTool::Main).then_some(Axis::Pitch),
					}
				}
				_ => Gesture::Band { add: mods.shift },
			};
			self.drag = Some(Drag {
				press,
				gesture,
				moved: false,
			});
		}
		if let Some(d) = &mut self.drag
			&& let Some(pos) = ui.ctx().pointer_latest_pos()
		{
			let dv = pos - d.press;
			if dv.length() > AXIS_LOCK {
				d.moved = true;
			}
			if let Gesture::Notes {
				axis: axis @ None, ..
			} = &mut d.gesture
				&& dv.length() > AXIS_LOCK
			{
				*axis = Some(if dv.x.abs() > dv.y.abs() {
					Axis::Time
				} else {
					Axis::Pitch
				});
			}
			if let Gesture::Notes { axis: Some(a), .. } = &d.gesture {
				ui.ctx().set_cursor_icon(match a {
					Axis::Pitch => CursorIcon::ResizeVertical,
					Axis::Time => CursorIcon::ResizeHorizontal,
				});
			}
		}
		if resp.drag_stopped()
			&& let Some(d) = self.drag.take()
			&& let Some(pos) = ui.ctx().pointer_latest_pos()
		{
			match d.gesture {
				Gesture::Band { add } => {
					let found = ne::notes_in(
						edits,
						(v.s(d.press.x), v.s(pos.x)),
						(v.pitch(d.press.y), v.pitch(pos.y)),
					);
					let sel = &mut state.view.audio.notes.selection;
					if !add {
						sel.clear();
					}
					sel.extend(found);
				}
				Gesture::Notes { ids, axis } if d.moved => {
					let semis = f64::from((d.press.y - pos.y) / v.row());
					match (tool, axis) {
						(NoteTool::Main, Some(Axis::Pitch)) => {
							let snap = snap_choice(ui, state, key);
							let cmd = ne::move_pitch(
								clip,
								edits,
								&ids,
								semis,
								snap.as_ref().map(|k| k.as_ref()),
							);
							state.note_edit(Ok(cmd));
						}
						(NoteTool::Main, Some(Axis::Time)) => {
							let dt = f64::from(pos.x - d.press.x) / v.pps;
							state.note_edit(ne::move_time(clip, edits, &ids, dt));
						}
						(_, Some(_)) => {
							state.note_edit(Ok(ne::tool_edit(
								clip,
								edits,
								&ids,
								tool,
								Some(semis),
							)));
						}
						_ => {}
					}
				}
				Gesture::Notes { .. } => {}
			}
		}
		if resp.clicked()
			&& let Some(pos) = resp.interact_pointer_pos()
		{
			if let Some((id, at)) = flag_hit(pos) {
				state.split_audio_note(clip, id, at);
			} else {
				match (hit(pos), tool) {
					(Some(id), NoteTool::Separation) => {
						state.split_audio_note(clip, id, v.s(pos.x));
					}
					(Some(id), _) => {
						let sel = &mut state.view.audio.notes.selection;
						if mods.shift || mods.command {
							if !sel.remove(&id) {
								sel.insert(id);
							}
						} else {
							sel.clear();
							sel.insert(id);
						}
					}
					(None, _) => state.view.audio.notes.selection.clear(),
				}
			}
		}
		if resp.double_clicked()
			&& let Some(pos) = resp.interact_pointer_pos()
		{
			let s = v.s(pos.x);
			let boundary = ne::boundary_at(edits, s, 4.0 / v.pps);
			match (tool, boundary, hit(pos)) {
				(NoteTool::Main | NoteTool::Separation, Some(first), _) => {
					state.merge_audio_notes(clip, first);
				}
				(NoteTool::Main, None, Some(id)) => {
					state.split_audio_note(clip, id, s);
				}
				(NoteTool::Separation, None, _) => {}
				(_, _, Some(id)) => {
					let one = BTreeSet::from([id]);
					state.note_edit(Ok(ne::tool_edit(clip, edits, &one, tool, None)));
				}
				_ => {}
			}
		}
		let menu_note = resp.interact_pointer_pos().and_then(hit);
		resp.context_menu(|ui| {
			let sel = state.view.audio.notes.selection.clone();
			let target: BTreeSet<AudioNoteId> = if !sel.is_empty() {
				sel
			} else {
				menu_note.into_iter().collect()
			};
			if ui
				.add_enabled(!target.is_empty(), egui::Button::new(tr("Correct pitch")))
				.clicked()
			{
				let view = state.view.audio.notes.clone();
				state.note_edit(Ok(ne::correct_pitch(clip, edits, &target, &view)));
				ui.close();
			}
			if ui
				.add_enabled(
					!target.is_empty(),
					egui::Button::new(tr("Reset to as sung")),
				)
				.clicked()
			{
				state.note_edit(Ok(ne::reset(clip, edits, &target)));
				ui.close();
			}
			if let Some(id) = menu_note {
				ui.separator();
				if ui.button(tr("Merge with the next note")).clicked() {
					state.merge_audio_notes(clip, id);
					ui.close();
				}
			}
			ui.separator();
			if ui.button(tr("Select all notes")).clicked() {
				state.view.audio.notes.selection = edits.notes.iter().map(|n| n.id).collect();
				ui.close();
			}
		});
	}

	fn macros(
		&mut self,
		ui: &mut Ui,
		state: &mut AppState,
		rect: Rect,
		edits: &NoteEdits,
		analysis: Option<&cadenza_notes::analysis::Analysis>,
	) {
		ui.painter().rect_filled(rect, 0.0, theme::BG_PANEL);
		ui.painter().vline(
			rect.left() + 0.5,
			rect.y_range(),
			Stroke::new(1.0, theme::BORDER),
		);
		let clip = state.view.audio.clip;
		let Some(clip) = clip else { return };
		let mut child = ui.new_child(
			egui::UiBuilder::new()
				.max_rect(rect.shrink2(vec2(10.0, 8.0)))
				.layout(egui::Layout::top_down(egui::Align::Min)),
		);
		egui::ScrollArea::vertical()
			.id_salt("note-macros")
			.auto_shrink([false, false])
			.show(&mut child, |ui| {
				ui.spacing_mut().slider_width = 84.0;
				let sel = state.view.audio.notes.selection.clone();
				let scope = if sel.is_empty() {
					tr("all notes").to_owned()
				} else {
					trn("{n} selected note", "{n} selected notes", sel.len())
				};
				ui.label(
					RichText::new(trf("Applies to {scope}", &[("scope", &scope)]))
						.font(theme::small())
						.color(theme::TEXT_FAINT),
				);
				ui.add_space(4.0);

				theme::section_label(ui, tr("Correct pitch"));
				let v = &mut state.view.audio.notes;
				let mut center = v.correct_center * 100.0;
				ui.add(
					egui::Slider::new(&mut center, 0.0..=100.0)
						.suffix(" %")
						.text(tr("center")),
				);
				v.correct_center = center / 100.0;
				let mut drift = v.correct_drift * 100.0;
				ui.add(
					egui::Slider::new(&mut drift, 0.0..=100.0)
						.suffix(" %")
						.text(tr("drift")),
				);
				v.correct_drift = drift / 100.0;
				ui.add(
					egui::Slider::new(&mut v.correct_threshold, 0.0..=50.0)
						.suffix(" c")
						.text(tr("leave within")),
				)
				.on_hover_text(tr("Notes this close to their tone keep their pitch"));
				if ui.button(tr("Correct pitch")).clicked() {
					let view = state.view.audio.notes.clone();
					state.note_edit(Ok(ne::correct_pitch(clip, edits, &sel, &view)));
				}
				ui.add_space(8.0);

				theme::section_label(ui, tr("Quantize time"));
				let v = &mut state.view.audio.notes;
				ui.horizontal(|ui| {
					for (g, name) in GRIDS {
						if ui
							.selectable_label((v.quantize_grid - g).abs() < 1e-9, name)
							.clicked()
						{
							v.quantize_grid = g;
						}
					}
				});
				let mut strength = v.quantize_strength * 100.0;
				ui.add(
					egui::Slider::new(&mut strength, 0.0..=100.0)
						.suffix(" %")
						.text(tr("strength")),
				);
				v.quantize_strength = strength / 100.0;
				if ui.button(tr("Quantize time")).clicked() {
					let view = state.view.audio.notes.clone();
					let cmd = ne::quantize_time(state.project(), clip, edits, &sel, &view);
					state.note_edit(cmd);
				}
				ui.add_space(8.0);

				theme::section_label(ui, tr("Leveling"));
				let v = &mut state.view.audio.notes;
				let mut level = v.leveling * 100.0;
				ui.add(
					egui::Slider::new(&mut level, 0.0..=100.0)
						.suffix(" %")
						.text(tr("evenness")),
				);
				v.leveling = level / 100.0;
				if ui
					.add_enabled(analysis.is_some(), egui::Button::new(tr("Level notes")))
					.clicked() && let Some(a) = analysis
				{
					let view = state.view.audio.notes.clone();
					state.note_edit(Ok(ne::leveling(clip, edits, a, &sel, &view)));
				}
				ui.add_space(8.0);

				theme::section_label(ui, tr("Sibilants"));
				let mut db = self.sibilants.unwrap_or(edits.sibilant_gain_db);
				let r = ui.add_enabled(
					!edits.sibilants.is_empty(),
					egui::Slider::new(&mut db, -24.0..=12.0)
						.suffix(" dB")
						.text(tr("level")),
				);
				if r.changed() {
					self.sibilants = Some(db);
				}
				if (r.drag_stopped() || (r.changed() && !r.dragged()))
					&& let Some(db) = self.sibilants.take()
				{
					state.note_edit(Ok(ne::sibilant_gain(clip, edits, db)));
				}
				ui.add_space(8.0);

				theme::section_label(ui, tr("Detection"));
				let v = &mut state.view.audio.notes;
				ui.add(
					egui::Slider::new(&mut v.sensitivity, 0.5..=2.0)
						.fixed_decimals(2)
						.text(tr("sensitivity")),
				)
				.on_hover_text(tr("Above 1 finds more notes, below 1 fewer"));
				ui.horizontal(|ui| {
					if ui
						.button(tr("Detect again"))
						.on_hover_text(tr("Keeps your edits, splits and merges"))
						.clicked()
					{
						let s = state.view.audio.notes.sensitivity;
						state.detect_notes(clip, Some(s));
					}
					if ui
						.button(tr("Remove notes"))
						.on_hover_text(tr("The clip plays as recorded again (undoable)"))
						.clicked()
					{
						state.remove_audio_notes(clip);
					}
				});
			});
	}

	/// Keys while the editor has focus: tools, select all, retune, reset.
	pub fn shortcuts(&mut self, ctx: &egui::Context, state: &mut AppState) {
		let Some(clip) = state.view.audio.clip else {
			return;
		};
		let Some((edits, _)) = ne::clip_notes(state.project(), clip) else {
			return;
		};
		let pressed = |m: egui::Modifiers, k: egui::Key| ctx.input_mut(|i| i.consume_key(m, k));
		for (t, k) in NoteTool::ALL.iter().zip([
			egui::Key::Num1,
			egui::Key::Num2,
			egui::Key::Num3,
			egui::Key::Num4,
			egui::Key::Num5,
			egui::Key::Num6,
		]) {
			if pressed(egui::Modifiers::NONE, k) {
				state.view.audio.notes.tool = *t;
			}
		}
		if pressed(egui::Modifiers::COMMAND, egui::Key::A) {
			state.view.audio.notes.selection = edits.notes.iter().map(|n| n.id).collect();
		}
		if pressed(egui::Modifiers::NONE, egui::Key::Escape) {
			state.view.audio.notes.selection.clear();
		}
		let sel = state.view.audio.notes.selection.clone();
		if sel.is_empty() {
			return;
		}
		let key = state.view.audio.notes.scale(&edits);
		let snap = state.view.audio.notes.snap.then_some(key.as_ref());
		for (m, k, d) in [
			(egui::Modifiers::NONE, egui::Key::ArrowUp, 1.0),
			(egui::Modifiers::NONE, egui::Key::ArrowDown, -1.0),
			(egui::Modifiers::SHIFT, egui::Key::ArrowUp, 12.0),
			(egui::Modifiers::SHIFT, egui::Key::ArrowDown, -12.0),
		] {
			if pressed(m, k) {
				state.note_edit(Ok(ne::move_pitch(clip, &edits, &sel, d, snap)));
			}
		}
		if pressed(egui::Modifiers::NONE, egui::Key::Delete)
			|| pressed(egui::Modifiers::NONE, egui::Key::Backspace)
		{
			state.note_edit(Ok(ne::reset(clip, &edits, &sel)));
		}
	}
}

/// Snap target of a pitch drag: `Some(key)` snaps (key `None` = semitones), `None` = free.
fn snap_choice(ui: &Ui, state: &AppState, key: Option<&Scale>) -> Option<Option<Scale>> {
	let alt = ui.input(|i| i.modifiers.alt);
	(state.view.audio.notes.snap != alt).then(|| key.cloned())
}

fn loudest(a: &cadenza_notes::analysis::Analysis) -> f64 {
	let mut v: Vec<f64> = a.frames.iter().map(|f| f.level_db).collect();
	if v.is_empty() {
		return -12.0;
	}
	v.sort_by(f64::total_cmp);
	v[(v.len() - 1) * 98 / 100]
}

fn note_tip(n: &cadenza_model::AudioNote) -> String {
	let pitch = n.result_pitch();
	let name = |p: f64| {
		format!(
			"{} {:+.0} c",
			cadenza_theory::format_pitch(p.round().clamp(0.0, 127.0) as u8, false),
			(p - p.round()) * 100.0
		)
	};
	let mut lines = vec![if n.pitch.is_some() {
		trf(
			"sung {sung} → {now}",
			&[("sung", &name(n.center)), ("now", &name(pitch))],
		)
	} else {
		name(pitch)
	}];
	if n.modulation != 1.0 {
		lines.push(trf(
			"vibrato {pct} %",
			&[("pct", &format!("{:.0}", n.modulation * 100.0))],
		));
	}
	if n.drift != 1.0 {
		lines.push(trf(
			"drift {pct} %",
			&[("pct", &format!("{:.0}", n.drift * 100.0))],
		));
	}
	if n.formant_cents != 0.0 {
		lines.push(trf(
			"formant {cents} c",
			&[("cents", &format!("{:+.0}", n.formant_cents))],
		));
	}
	if n.gain_db != 0.0 {
		lines.push(trf(
			"level {db} dB",
			&[("db", &crate::i18n::decimal(format!("{:+.1}", n.gain_db)))],
		));
	}
	for f in &n.flags {
		lines.push(match f {
			AudioNoteFlag::MaybeTwoNotes { cents, .. } => trf(
				"may hold two notes {cents} c apart: click ✂ to split",
				&[("cents", &format!("{cents:.0}"))],
			),
			AudioNoteFlag::ShortCore => tr("very short stable part").to_owned(),
			AudioNoteFlag::WeakVoicing => tr("weak voicing (breathy or noisy)").to_owned(),
		});
	}
	lines.join("\n")
}

fn status(ui: &Ui, canvas: Rect, text: &str) {
	ui.painter().text(
		pos2(canvas.right() - 8.0, canvas.top() + 8.0),
		Align2::RIGHT_TOP,
		text,
		theme::small(),
		theme::TEXT_FAINT,
	);
}

fn grid(p: &egui::Painter, v: &View, key: Option<&Scale>) {
	p.rect_filled(v.canvas, 0.0, theme::LANE);
	let row = v.row();
	for k in v.low.floor() as i32..=v.high.ceil() as i32 {
		if !(0..=127).contains(&k) {
			continue;
		}
		let pitch = k as u8;
		let y = v.y(f64::from(k));
		let r = Rect::from_x_y_ranges(v.canvas.x_range(), (y - row / 2.0)..=(y + row / 2.0));
		let fill = match key {
			Some(s) if !s.contains(pitch) => theme::ROW_OUT_OF_SCALE,
			Some(s) if (i32::from(pitch) - i32::from(s.root.0)).rem_euclid(12) == 0 => {
				theme::ROW_ROOT
			}
			Some(_) => theme::ROW_WHITE,
			None if crate::state::piano::is_black(pitch) => theme::ROW_BLACK,
			None => theme::ROW_WHITE,
		};
		p.rect_filled(r, 0.0, fill);
		p.hline(
			v.canvas.x_range(),
			y + row / 2.0,
			Stroke::new(
				1.0,
				if pitch.is_multiple_of(12) {
					theme::PIANO_GRID_BAR
				} else {
					theme::PIANO_ROW_LINE
				},
			),
		);
	}
	// One line per second of the file.
	let (s0, s1) = (
		v.s(v.canvas.left()).floor() as i64,
		v.s(v.canvas.right()).ceil() as i64,
	);
	if (s1 - s0) < 400 {
		for s in s0..=s1 {
			p.vline(
				v.x(s as f64),
				v.canvas.y_range(),
				Stroke::new(1.0, theme::GRID_BEAT),
			);
		}
	}
}

fn key_combo(ui: &mut Ui, state: &mut AppState, edits: &NoteEdits) {
	let detected = ne::detected_key(edits);
	let current = match &state.view.audio.notes.key {
		KeyChoice::Chromatic => tr("Chromatic").to_owned(),
		KeyChoice::Detected => detected.as_ref().map_or_else(
			|| tr("Detected").to_owned(),
			|s| trf("{key} (detected)", &[("key", &scale_label(s))]),
		),
		KeyChoice::Scale(s) => scale_label(s),
	};
	egui::ComboBox::from_id_salt("note-key")
		.width(150.0)
		.selected_text(current)
		.show_ui(ui, |ui| {
			let v = &mut state.view.audio.notes;
			if ui
				.selectable_label(v.key == KeyChoice::Chromatic, tr("Chromatic"))
				.on_hover_text(tr("Every semitone is a target"))
				.clicked()
			{
				v.key = KeyChoice::Chromatic;
			}
			if let Some(d) = &detected
				&& ui
					.selectable_label(
						v.key == KeyChoice::Detected,
						trf("{key} (detected)", &[("key", &scale_label(d))]),
					)
					.clicked()
			{
				v.key = KeyChoice::Detected;
			}
			ui.separator();
			for kind in ["major", "minor"] {
				for root in ROOTS {
					let name = format!("{root} {kind}");
					if let Ok(s) = Scale::parse(&name)
						&& ui
							.selectable_label(v.key == KeyChoice::Scale(s), &name)
							.clicked()
					{
						v.key = KeyChoice::Scale(s);
					}
				}
			}
		});
}
