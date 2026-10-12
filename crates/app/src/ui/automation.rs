//! Automation and tempo lanes of the arrange view: lane headers (parameter picker,
//! current value), curve drawing and point editing. Points are added with a click,
//! dragged (snapped; Alt: free; Shift: vertical only), selected with a rubber band
//! (Shift adds), deleted with a right-click or Del, and reshaped with a double-click
//! (linear -> hold -> smooth). Ctrl+drag draws a ramp. Every gesture is previewed
//! locally (and heard live) and committed as one command on release.

use cadenza_core::{ParamAddress, Tick};
use cadenza_model::{AutomationPoint, Command, PointShape};
use egui::{
	Align2, Color32, CornerRadius, CursorIcon, Id, PointerButton, Pos2, Rect, Sense, Shape, Stroke,
	StrokeKind, Ui, pos2, vec2,
};

use crate::state::AppState;
use crate::state::automation as auto;
use crate::state::timeline::format_bbt;
use crate::theme;

/// Pointer distance (px) that grabs a point.
const GRAB: f32 = 6.0;
/// Vertical padding of the curve inside a lane.
const PAD: f32 = 5.0;

#[derive(Debug, Clone)]
struct PointDrag {
	address: ParamAddress,
	/// Original ticks of the dragged (selected) points.
	ticks: Vec<Tick>,
	/// The grabbed point's original tick.
	anchor: Tick,
	press: Pos2,
	dt: Tick,
	dn: f32,
}

#[derive(Debug, Clone)]
struct Band {
	address: ParamAddress,
	start: Pos2,
	end: Pos2,
	add: bool,
}

#[derive(Debug, Clone)]
struct LineDraw {
	address: ParamAddress,
	start: (Tick, f32),
	end: (Tick, f32),
}

#[derive(Debug, Clone)]
struct TempoDrag {
	from: Tick,
	press: Pos2,
	start_bpm: f64,
	to: Tick,
	bpm: f64,
}

/// View-local state of the automation lanes.
#[derive(Default)]
pub struct AutoUi {
	drag: Option<PointDrag>,
	band: Option<Band>,
	line: Option<LineDraw>,
	tempo: Option<TempoDrag>,
}

fn norm_to_y(rect: Rect, n: f32) -> f32 {
	rect.bottom() - PAD - n.clamp(0.0, 1.0) * (rect.height() - 2.0 * PAD)
}

fn y_to_norm(rect: Rect, y: f32) -> f32 {
	((rect.bottom() - PAD - y) / (rect.height() - 2.0 * PAD).max(1.0)).clamp(0.0, 1.0)
}

fn snapped(state: &AppState, tick: Tick, free: bool) -> Tick {
	if state.view.snap && !free {
		state
			.view
			.timeline
			.snap(tick, state.project().time_signature)
	} else {
		Tick(tick.0.max(0))
	}
}

impl AutoUi {
	/// Whether a gesture on `address` is running (keeps processing off-screen).
	pub fn dragging(&self, address: &ParamAddress) -> bool {
		self.drag.as_ref().is_some_and(|d| &d.address == address)
			|| self.band.as_ref().is_some_and(|b| &b.address == address)
			|| self.line.as_ref().is_some_and(|l| &l.address == address)
	}

	/// Header of an automation lane: parameter picker, value at the playhead, hide.
	pub fn lane_header(
		&mut self,
		ui: &mut Ui,
		state: &mut AppState,
		address: &ParamAddress,
		rect: Rect,
	) {
		let p = ui.painter().clone();
		p.rect_filled(rect, 0.0, theme::BG_DEEP);
		let owner_color = address
			.track
			.and_then(|t| state.project().track(t))
			.map_or(theme::ACCENT, |t| theme::track_color(t.color.as_deref()));
		p.rect_filled(
			Rect::from_min_size(rect.min + vec2(14.0, 0.0), vec2(2.0, rect.height())),
			0.0,
			owner_color.gamma_multiply(0.6),
		);
		p.hline(
			rect.x_range(),
			rect.bottom() - 0.5,
			Stroke::new(1.0, theme::BORDER),
		);
		let Some(meta) = state.lane_meta(address) else {
			return;
		};
		let automated = state.is_automated(address);
		// Row 1: picker and hide button.
		let row1 = Rect::from_min_max(
			pos2(rect.left() + 22.0, rect.top() + 5.0),
			pos2(rect.right() - 8.0, rect.top() + 23.0),
		);
		let close = Rect::from_min_size(
			pos2(row1.right() - 16.0, row1.top() + 1.0),
			vec2(16.0, 16.0),
		);
		let close_r = ui.interact(
			close,
			Id::new(("lane-hide", address.to_string())),
			Sense::click(),
		);
		p.text(
			close.center(),
			Align2::CENTER_CENTER,
			"×",
			theme::small(),
			if close_r.hovered() {
				theme::TEXT
			} else {
				theme::TEXT_FAINT
			},
		);
		if close_r
			.on_hover_text("Hide this lane (its points are kept)")
			.clicked()
		{
			state.view.automation.hide(address);
		}
		let picker = Rect::from_min_max(row1.min, pos2(close.left() - 4.0, row1.bottom()));
		let mut child = ui.new_child(egui::UiBuilder::new().max_rect(picker));
		let choices = state.lane_choices(address.track);
		egui::ComboBox::from_id_salt(("lane-param", address.to_string()))
			.width(picker.width() - 6.0)
			.selected_text(egui::RichText::new(&meta.name).font(theme::small()).color(
				if automated {
					theme::AUTOMATION
				} else {
					theme::TEXT_DIM
				},
			))
			.show_ui(&mut child, |ui| {
				ui.set_min_width(200.0);
				let mut group = String::new();
				for c in choices {
					if c.group != group {
						group = c.group.clone();
						ui.label(
							egui::RichText::new(group.to_uppercase())
								.font(theme::small())
								.color(theme::TEXT_FAINT),
						);
					}
					let has = state.is_automated(&c.address);
					let label = if has {
						format!("● {}", c.name)
					} else {
						c.name.clone()
					};
					if ui.selectable_label(&c.address == address, label).clicked() {
						state.view.automation.replace(address, c.address);
					}
				}
			})
			.response
			.on_hover_text("Parameter of this lane (● = automated)");
		// Row 2: value at the playhead.
		let value = state
			.automated(address)
			.or_else(|| state.static_value(address));
		if rect.height() > 34.0
			&& let Some(v) = value
		{
			p.text(
				pos2(rect.left() + 24.0, rect.top() + 36.0),
				Align2::LEFT_CENTER,
				meta.format(v),
				theme::mono(10.5),
				if automated {
					theme::TEXT
				} else {
					theme::TEXT_FAINT
				},
			);
			if !automated {
				p.text(
					pos2(rect.right() - 10.0, rect.top() + 36.0),
					Align2::RIGHT_CENTER,
					"click the lane to add points",
					theme::small(),
					theme::TEXT_FAINT,
				);
			}
		}
		let r = ui.interact(
			rect,
			Id::new(("lane-header", address.to_string())),
			Sense::click(),
		);
		r.context_menu(|ui| lane_menu(ui, state, address));
	}

	/// The master row: name and automation toggle.
	pub fn master_header(&mut self, ui: &mut Ui, state: &mut AppState, rect: Rect) {
		let p = ui.painter().clone();
		p.rect_filled(rect, 0.0, theme::BG_PANEL);
		p.rect_filled(
			Rect::from_min_size(rect.min, vec2(5.0, rect.height())),
			0.0,
			theme::ACCENT,
		);
		p.hline(
			rect.x_range(),
			rect.bottom() - 0.5,
			Stroke::new(1.0, theme::BORDER),
		);
		p.text(
			pos2(rect.left() + 12.0, rect.center().y),
			Align2::LEFT_CENTER,
			"Master",
			egui::FontId::proportional(12.0),
			theme::TEXT,
		);
		let toggle = Rect::from_min_size(
			pos2(rect.right() - 36.0, rect.center().y - 8.0),
			vec2(18.0, 16.0),
		);
		let open = state.view.automation.is_open(None);
		let r = ui.interact(toggle, Id::new("master-auto"), Sense::click());
		super::widgets::paint_toggle(&p, toggle, open, "A", theme::AUTOMATION, r.hovered());
		if r.on_hover_text("Show / hide master automation (A)")
			.clicked()
		{
			let project = state.project().clone();
			state.view.automation.toggle(&project, None);
		}
	}

	/// The curve and points of one lane, with point editing.
	pub fn lane_body(
		&mut self,
		ui: &mut Ui,
		state: &mut AppState,
		address: &ParamAddress,
		rect: Rect,
		area: Rect,
	) {
		let Some(meta) = state.lane_meta(address) else {
			return;
		};
		let tl = state.view.timeline;
		let x_of = |t: Tick| area.left() + tl.tick_to_x(t);
		let tick_at = |x: f32| tl.x_to_tick(x - area.left());
		let model = auto::points(state.project(), address);
		let selected = state.view.automation.selected_in(address);
		// Points as drawn this frame (with a running drag applied).
		let (shown, shown_sel): (Vec<AutomationPoint>, Vec<bool>) = match &self.drag {
			Some(d) if &d.address == address => auto::moved(&model, &meta, &d.ticks, d.dt, d.dn)
				.into_iter()
				.unzip(),
			_ => (
				model.clone(),
				model.iter().map(|p| selected.contains(&p.tick)).collect(),
			),
		};
		let visible = rect.intersect(area);
		let p = ui.painter_at(visible);

		// Curve (sampled through the model's evaluation, so shapes and tempo match the
		// engine) with a soft fill.
		let tempo = state.project().tempo.clone();
		if shown.is_empty() {
			if let Some(v) = state.static_value(address) {
				let y = norm_to_y(rect, meta.to_norm(v));
				p.hline(
					visible.x_range(),
					y,
					Stroke::new(1.0, theme::AUTOMATION.gamma_multiply(0.35)),
				);
			}
		} else {
			let mut line = Vec::new();
			let mut x = visible.left();
			while x <= visible.right() + 2.0 {
				let v = cadenza_model::automation_value(&shown, tick_at(x), &tempo).unwrap_or(0.0);
				line.push(pos2(x, norm_to_y(rect, meta.to_norm(v))));
				x += 2.0;
			}
			let fill = theme::AUTOMATION.gamma_multiply(0.10);
			for w in line.windows(2) {
				p.add(Shape::convex_polygon(
					vec![
						w[0],
						w[1],
						pos2(w[1].x, rect.bottom()),
						pos2(w[0].x, rect.bottom()),
					],
					fill,
					Stroke::NONE,
				));
			}
			p.add(Shape::line(line, Stroke::new(1.5, theme::AUTOMATION)));
		}

		let id = Id::new(("auto-lane", address.to_string()));
		let resp = ui.interact(visible, id, Sense::click_and_drag());
		let hover = resp.hover_pos();
		let near = |pos: Pos2| -> Option<usize> {
			shown
				.iter()
				.enumerate()
				.map(|(i, pt)| {
					let c = pos2(x_of(pt.tick), norm_to_y(rect, meta.to_norm(pt.value)));
					(i, c.distance(pos))
				})
				.filter(|(_, d)| *d <= GRAB)
				.min_by(|a, b| a.1.total_cmp(&b.1))
				.map(|(i, _)| i)
		};
		let hovered_point = hover.and_then(near);
		for (i, pt) in shown.iter().enumerate() {
			let c = pos2(x_of(pt.tick), norm_to_y(rect, meta.to_norm(pt.value)));
			if !visible.expand(4.0).contains(c) {
				continue;
			}
			let sel = shown_sel.get(i).copied().unwrap_or(false);
			let r = if hovered_point == Some(i) { 4.5 } else { 3.5 };
			match pt.shape {
				PointShape::Hold => {
					let sq = Rect::from_center_size(c, vec2(r * 2.0, r * 2.0));
					p.rect_filled(sq, 1.0, if sel { Color32::WHITE } else { theme::BG_DEEP });
					p.rect_stroke(
						sq,
						1.0,
						Stroke::new(1.5, theme::AUTOMATION),
						StrokeKind::Middle,
					);
				}
				_ => {
					p.circle(
						c,
						r,
						if sel { Color32::WHITE } else { theme::BG_DEEP },
						Stroke::new(1.5, theme::AUTOMATION),
					);
				}
			}
		}

		// Tooltip: the value under the pointer (or of the dragged/hovered point).
		let tip = match (&self.drag, hovered_point) {
			(Some(d), _) if &d.address == address => shown
				.iter()
				.zip(&shown_sel)
				.find(|(pt, s)| **s && pt.tick == d.anchor + d.dt)
				.or_else(|| shown.iter().zip(&shown_sel).find(|(_, s)| **s))
				.map(|(pt, _)| (pt.tick, pt.value)),
			(_, Some(i)) => Some((shown[i].tick, shown[i].value)),
			_ => None,
		};
		if let Some((tick, v)) = tip
			&& (resp.hovered() || resp.dragged())
		{
			let ts = state.project().time_signature;
			let at = pos2(x_of(tick), norm_to_y(rect, meta.to_norm(v)));
			value_tag(
				ui,
				at,
				&format!("{}  @ {}", meta.format(v), format_bbt(tick, ts)),
			);
		} else if resp.hovered()
			&& let Some(pos) = hover
			&& self.band.is_none()
		{
			let v = meta.from_norm(y_to_norm(rect, pos.y));
			value_tag(ui, pos + vec2(0.0, -12.0), &meta.format(v));
		}
		if hovered_point.is_some() {
			ui.ctx().set_cursor_icon(CursorIcon::Grab);
		}

		let (shift, alt, ctrl) =
			ui.input(|i| (i.modifiers.shift, i.modifiers.alt, i.modifiers.command));

		// Press: grab a point, start a band or a ramp.
		if resp.drag_started_by(PointerButton::Primary)
			&& let Some(press) = ui
				.input(|i| i.pointer.press_origin())
				.or(resp.interact_pointer_pos())
		{
			match near(press) {
				Some(i) => {
					let tick = shown[i].tick;
					let sel = &mut state.view.automation.selected;
					if !selected.contains(&tick) {
						if !shift {
							sel.clear();
						}
						sel.push((address.clone(), tick));
					}
					self.drag = Some(PointDrag {
						address: address.clone(),
						ticks: state.view.automation.selected_in(address),
						anchor: tick,
						press,
						dt: Tick::ZERO,
						dn: 0.0,
					});
				}
				None if ctrl => {
					let t = snapped(state, tick_at(press.x), alt);
					let v = meta.from_norm(y_to_norm(rect, press.y));
					self.line = Some(LineDraw {
						address: address.clone(),
						start: (t, v),
						end: (t, v),
					});
				}
				None => {
					self.band = Some(Band {
						address: address.clone(),
						start: press,
						end: press,
						add: shift,
					});
				}
			}
		}

		// Running gestures.
		let pointer = ui.ctx().pointer_interact_pos();
		let released = ui.input(|i| i.pointer.any_released());
		if let Some(d) = &mut self.drag
			&& &d.address == address
		{
			if let Some(pos) = pointer {
				let raw = tick_at(pos.x) - tick_at(d.press.x);
				d.dt = if shift {
					Tick::ZERO
				} else {
					snapped(state, d.anchor + raw, alt) - d.anchor
				};
				d.dn = (d.press.y - pos.y) / (rect.height() - 2.0 * PAD).max(1.0);
				let preview: Vec<AutomationPoint> =
					auto::moved(&model, &meta, &d.ticks, d.dt, d.dn)
						.into_iter()
						.map(|(p, _)| p)
						.collect();
				state.preview_lane(address, preview);
			}
			if released {
				let d = self.drag.take().expect("point drag");
				if let Some((cmd, ticks)) =
					auto::move_points(state.project(), address, &meta, &d.ticks, d.dt, d.dn)
					&& state.apply(cmd)
				{
					let sel = &mut state.view.automation.selected;
					sel.retain(|(a, _)| a != address);
					sel.extend(ticks.into_iter().map(|t| (address.clone(), t)));
				}
				state.end_lane_preview();
			}
		}
		if let Some(b) = &mut self.band
			&& &b.address == address
		{
			if let Some(pos) = pointer {
				b.end = pos;
			}
			let r = Rect::from_two_pos(b.start, b.end);
			p.rect_filled(r, 0.0, theme::ACCENT.gamma_multiply(0.12));
			p.rect_stroke(r, 0.0, Stroke::new(1.0, theme::ACCENT), StrokeKind::Inside);
			if released {
				let b = self.band.take().expect("band");
				let ticks = auto::points_in(
					&model,
					&meta,
					(tick_at(r.left()), tick_at(r.right())),
					(y_to_norm(rect, r.bottom()), y_to_norm(rect, r.top())),
				);
				let sel = &mut state.view.automation.selected;
				if !b.add {
					sel.clear();
				}
				for t in ticks {
					if !sel.iter().any(|(a, x)| a == address && *x == t) {
						sel.push((address.clone(), t));
					}
				}
			}
		}
		if let Some(l) = &mut self.line
			&& &l.address == address
		{
			if let Some(pos) = pointer {
				l.end = (
					snapped(state, tick_at(pos.x), alt),
					meta.from_norm(y_to_norm(rect, pos.y)),
				);
			}
			let a = pos2(x_of(l.start.0), norm_to_y(rect, meta.to_norm(l.start.1)));
			let b = pos2(x_of(l.end.0), norm_to_y(rect, meta.to_norm(l.end.1)));
			p.line_segment([a, b], Stroke::new(1.5, Color32::WHITE));
			if released {
				let l = self.line.take().expect("line");
				state.apply(auto::draw_line(state.project(), address, l.start, l.end));
			}
		}

		// Clicks (no drag).
		if resp.clicked()
			&& let Some(pos) = resp.interact_pointer_pos()
		{
			match near(pos) {
				Some(i) => {
					let tick = shown[i].tick;
					let sel = &mut state.view.automation.selected;
					if shift {
						if let Some(k) = sel.iter().position(|(a, t)| a == address && *t == tick) {
							sel.remove(k);
						} else {
							sel.push((address.clone(), tick));
						}
					} else {
						sel.clear();
						sel.push((address.clone(), tick));
					}
				}
				None => {
					let tick = snapped(state, tick_at(pos.x), alt);
					let v = meta.from_norm(y_to_norm(rect, pos.y));
					if state.apply(auto::add_point(state.project(), address, tick, v)) {
						let sel = &mut state.view.automation.selected;
						sel.clear();
						sel.push((address.clone(), tick));
					}
				}
			}
		}
		if resp.double_clicked()
			&& let Some(i) = resp.interact_pointer_pos().and_then(near)
		{
			let next = match shown[i].shape {
				PointShape::Linear => PointShape::Hold,
				PointShape::Hold => PointShape::Smooth,
				PointShape::Smooth => PointShape::Linear,
			};
			if let Some(cmd) = auto::set_shape(state.project(), address, &[shown[i].tick], next) {
				state.apply(cmd);
			}
		}
		let right_on_point =
			resp.secondary_clicked() && resp.interact_pointer_pos().and_then(near).is_some();
		if right_on_point {
			let i = resp.interact_pointer_pos().and_then(near).expect("point");
			if let Some(cmd) = auto::delete_points(state.project(), address, &[shown[i].tick]) {
				state.apply(cmd);
			}
		} else {
			resp.context_menu(|ui| lane_menu(ui, state, address));
		}
	}

	/// The tempo lane (step curve of the tempo map) with its header.
	pub fn tempo_lane(&mut self, ui: &mut Ui, state: &mut AppState, header: Rect, rect: Rect) {
		let hp = ui.painter_at(header);
		hp.rect_filled(header, 0.0, theme::BG_PANEL);
		hp.rect_filled(
			Rect::from_min_size(header.min, vec2(5.0, header.height())),
			0.0,
			theme::TEMPO,
		);
		hp.vline(
			header.right() - 0.5,
			header.y_range(),
			Stroke::new(1.0, theme::BORDER_STRONG),
		);
		hp.hline(
			header.x_range(),
			header.bottom() - 0.5,
			Stroke::new(1.0, theme::BORDER),
		);
		let bpm_now = state.project().tempo.bpm_at(state.position());
		hp.text(
			pos2(header.left() + 12.0, header.top() + 14.0),
			Align2::LEFT_CENTER,
			"Tempo",
			egui::FontId::proportional(12.0),
			theme::TEXT,
		);
		hp.text(
			pos2(header.left() + 12.0, header.top() + 34.0),
			Align2::LEFT_CENTER,
			format!("{bpm_now:.1} BPM"),
			theme::mono(10.5),
			theme::TEMPO,
		);
		hp.text(
			pos2(header.right() - 10.0, header.top() + 34.0),
			Align2::RIGHT_CENTER,
			"click: add change",
			theme::small(),
			theme::TEXT_FAINT,
		);

		let p = ui.painter_at(rect);
		p.rect_filled(rect, 0.0, theme::AUTO_LANE);
		p.hline(
			rect.x_range(),
			rect.bottom() - 0.5,
			Stroke::new(1.0, theme::BORDER),
		);
		let tl = state.view.timeline;
		let x_of = |t: Tick| rect.left() + tl.tick_to_x(t);
		let tick_at = |x: f32| tl.x_to_tick(x - rect.left());
		let mut points: Vec<(Tick, f64)> = state
			.project()
			.tempo
			.points()
			.iter()
			.map(|p| (p.tick, p.bpm))
			.collect();
		if let Some(d) = &self.tempo {
			points.retain(|p| p.0 != d.from && p.0 != d.to);
			points.push((d.to, d.bpm));
			points.sort_by_key(|p| p.0);
		}
		// Step curve.
		let y_of = |bpm: f64| norm_to_y(rect, auto::tempo_norm(bpm));
		let mut line = Vec::new();
		for (i, &(t, bpm)) in points.iter().enumerate() {
			let x0 = x_of(t).max(rect.left() - 2.0);
			let x1 = points
				.get(i + 1)
				.map_or(rect.right() + 2.0, |n| x_of(n.0))
				.min(rect.right() + 2.0);
			if let Some(&last) = line.last() {
				let last: Pos2 = last;
				line.push(pos2(x0, last.y));
			}
			line.push(pos2(x0, y_of(bpm)));
			line.push(pos2(x1.max(x0), y_of(bpm)));
		}
		p.add(Shape::line(line, Stroke::new(1.5, theme::TEMPO)));
		let resp = ui.interact(rect, Id::new("tempo-lane"), Sense::click_and_drag());
		let near = |pos: Pos2| {
			points
				.iter()
				.map(|&(t, bpm)| (t, pos2(x_of(t), y_of(bpm)).distance(pos)))
				.filter(|(_, d)| *d <= GRAB)
				.min_by(|a, b| a.1.total_cmp(&b.1))
				.map(|(t, _)| t)
		};
		let hovered = resp.hover_pos().and_then(near);
		for &(t, bpm) in &points {
			let c = pos2(x_of(t), y_of(bpm));
			if rect.expand(4.0).contains(c) {
				p.rect_filled(
					Rect::from_center_size(c, vec2(8.0, 8.0)),
					1.0,
					if hovered == Some(t) {
						Color32::WHITE
					} else {
						theme::BG_DEEP
					},
				);
				p.rect_stroke(
					Rect::from_center_size(c, vec2(8.0, 8.0)),
					1.0,
					Stroke::new(1.5, theme::TEMPO),
					StrokeKind::Middle,
				);
				if rect.contains(c + vec2(30.0, 0.0)) {
					p.text(
						c + vec2(6.0, -7.0),
						Align2::LEFT_BOTTOM,
						format!("{bpm:.1}"),
						theme::small(),
						theme::TEXT_DIM,
					);
				}
			}
		}
		if let Some(pos) = resp.hover_pos()
			&& self.tempo.is_none()
		{
			let bpm = auto::tempo_from_norm(y_to_norm(rect, pos.y));
			value_tag(ui, pos + vec2(0.0, -12.0), &format!("{bpm:.1} BPM"));
		}
		let alt = ui.input(|i| i.modifiers.alt);
		if resp.drag_started_by(PointerButton::Primary)
			&& let Some(press) = ui
				.input(|i| i.pointer.press_origin())
				.or(resp.interact_pointer_pos())
			&& let Some(t) = near(press)
		{
			let bpm = state.project().tempo.bpm_at(t);
			self.tempo = Some(TempoDrag {
				from: t,
				press,
				start_bpm: bpm,
				to: t,
				bpm,
			});
		}
		if let Some(d) = &mut self.tempo {
			if let Some(pos) = ui.ctx().pointer_interact_pos() {
				let dn = (d.press.y - pos.y) / (rect.height() - 2.0 * PAD).max(1.0);
				d.bpm = auto::tempo_from_norm(auto::tempo_norm(d.start_bpm) + dn);
				d.to = if d.from == Tick::ZERO {
					Tick::ZERO
				} else {
					let raw = d.from + (tick_at(pos.x) - tick_at(d.press.x));
					Tick(snapped(state, raw, alt).0.max(1))
				};
				value_tag(
					ui,
					pos2(x_of(d.to), y_of(d.bpm)),
					&format!(
						"{:.1} BPM @ {}",
						d.bpm,
						format_bbt(d.to, state.project().time_signature)
					),
				);
			}
			if ui.input(|i| i.pointer.any_released()) {
				let d = self.tempo.take().expect("tempo drag");
				if let Some(cmd) = auto::move_tempo(state.project(), d.from, d.to, d.bpm) {
					state.apply(cmd);
				}
			}
		}
		if resp.clicked()
			&& let Some(pos) = resp.interact_pointer_pos()
			&& near(pos).is_none()
		{
			let tick = snapped(state, tick_at(pos.x), alt);
			let bpm = auto::tempo_from_norm(y_to_norm(rect, pos.y));
			state.apply(auto::add_tempo(tick, bpm));
		}
		if resp.secondary_clicked()
			&& let Some(t) = resp.interact_pointer_pos().and_then(near)
			&& let Some(cmd) = auto::delete_tempo(t)
		{
			state.apply(cmd);
		}
	}
}

/// A small value label next to the pointer.
fn value_tag(ui: &Ui, at: Pos2, text: &str) {
	let p = ui.ctx().layer_painter(egui::LayerId::new(
		egui::Order::Tooltip,
		Id::new("auto-value-tag"),
	));
	let galley = p.layout_no_wrap(text.to_owned(), theme::small(), theme::TEXT);
	let r = Rect::from_min_size(at + vec2(10.0, -galley.size().y - 6.0), galley.size())
		.expand2(vec2(5.0, 2.0));
	p.rect(
		r,
		CornerRadius::same(3),
		theme::BG_RAISED,
		Stroke::new(1.0, theme::BORDER_STRONG),
		StrokeKind::Inside,
	);
	p.galley(r.min + vec2(5.0, 2.0), galley, theme::TEXT);
}

/// Right-click menu of a lane: shape of the selected points, clear, hide.
fn lane_menu(ui: &mut Ui, state: &mut AppState, address: &ParamAddress) {
	let selected = state.view.automation.selected_in(address);
	ui.add_enabled_ui(!selected.is_empty(), |ui| {
		for (label, shape) in [
			("Linear", PointShape::Linear),
			("Hold (step)", PointShape::Hold),
			("Smooth", PointShape::Smooth),
		] {
			if ui.button(format!("Shape: {label}")).clicked() {
				if let Some(cmd) = auto::set_shape(state.project(), address, &selected, shape) {
					state.apply(cmd);
				}
				ui.close();
			}
		}
		if ui
			.add(egui::Button::new("Delete selected points").shortcut_text("Del"))
			.clicked()
		{
			if let Some(cmd) = auto::delete_points(state.project(), address, &selected) {
				state.apply(cmd);
			}
			ui.close();
		}
	});
	ui.separator();
	if ui
		.add_enabled(state.is_automated(address), egui::Button::new("Clear lane"))
		.clicked()
	{
		state.apply(Command::SetAutomation {
			address: address.clone(),
			points: Vec::new(),
		});
		ui.close();
	}
	if ui.button("Hide lane").clicked() {
		state.view.automation.hide(address);
		ui.close();
	}
}

/// "Automate" entry for a control's context menu: shows the lane for `address` in the
/// arrange view (creating it at the control's current value).
pub fn automate_item(ui: &mut Ui, state: &mut AppState, address: ParamAddress) {
	let has = state.is_automated(&address);
	let label = if has { "Show automation" } else { "Automate" };
	if ui.button(label).clicked() {
		state.automate(address);
		ui.close();
	}
}

/// Paints a small "A" badge on a control that follows automation.
pub fn automated_badge(ui: &Ui, rect: Rect) {
	ui.painter().circle_filled(
		pos2(rect.right() - 3.0, rect.top() + 3.0),
		2.5,
		theme::AUTOMATION,
	);
}
