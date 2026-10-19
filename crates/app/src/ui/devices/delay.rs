//! The Delay editor body (design `16 Effects`, 16c): echo taps on the beat (or free-time)
//! timeline, Sync | Free + the note-value grid, the ping-pong toggle, the feedback filter
//! strip and the readout. Compact: the taps at full width, then one row of S knobs. The
//! gesture -> parameter mapping lives in [`crate::state::delay_editor`].

use std::sync::{Arc, Mutex};

use egui::{
	Align, Align2, Color32, CornerRadius, CursorIcon, Id, Layout, Rect, Sense, Shape, Stroke, Ui,
	UiBuilder, pos2, vec2,
};

use crate::state::AppState;
use crate::state::delay_editor::{
	self as dl, DelValues, FEEDBACK, HIGH_CUT, LOW_CUT, PING_PONG, SYNC, TIME,
};
use crate::state::device_display::DbRange;
use crate::state::selection::DeviceRef;
use crate::theme;
use crate::ui::device_editor::BodyCx;
use crate::ui::device_kit::{self, FreqDisplay, FreqEvent, FreqNode, KnobSize, ParamKnob};

const CHIP_H: f32 = 22.0;
const FOOTER_H: f32 = 32.0;
const GAP: f32 = 10.0;
const CAPTION_H: f32 = 18.0;
/// The feedback filter's band (and its nodes).
const BAND: Color32 = theme::BAND_COLORS[5];
const FILTER_RANGE: DbRange = DbRange {
	min: -24.0,
	max: 12.0,
	step: 12.0,
};

/// A drag of the first echo in progress: its time and feedback when the drag started,
/// and the pixel offset accumulated on each axis since.
#[derive(Debug, Clone, Copy)]
struct EchoDrag {
	start_ms: f32,
	start_feedback: f32,
	px: egui::Vec2,
}

#[derive(Default)]
struct DelayView {
	drag: Option<EchoDrag>,
}

type Shared = Arc<Mutex<DelayView>>;

fn view_id(salt: &str, r: DeviceRef) -> Id {
	Id::new(("delay-editor", salt, r.device))
}

fn view(ctx: &egui::Context, id: Id) -> Shared {
	ctx.data_mut(|d| {
		d.get_temp_mut_or_insert_with(id, || Arc::new(Mutex::new(DelayView::default())))
			.clone()
	})
}

fn lock(view: &Shared) -> std::sync::MutexGuard<'_, DelayView> {
	view.lock().unwrap_or_else(|e| e.into_inner())
}

/// S toggles Sync/Free, P toggles ping-pong, while the dock editor has focus.
pub fn shortcuts(ctx: &egui::Context, state: &mut AppState, r: DeviceRef) {
	let shown = DelValues::from_fn(|p| {
		state
			.param_value(r.track, r.device, p.id)
			.unwrap_or(p.default)
	});
	if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::S)) {
		state.commit_param(r.track, r.device, SYNC, dl::toggle_sync(shown.sync));
	}
	if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::P)) {
		state.commit_param(
			r.track,
			r.device,
			PING_PONG,
			dl::toggle_ping_pong(shown.ping_pong),
		);
	}
}

fn shown_values(state: &AppState, r: DeviceRef) -> DelValues {
	DelValues::from_fn(|p| {
		state
			.param_value(r.track, r.device, p.id)
			.unwrap_or(p.default)
	})
}

fn model_value(state: &AppState, r: DeviceRef, id: &str) -> f32 {
	state
		.project()
		.device(r.track, r.device)
		.and_then(|d| d.params.get(id).copied())
		.or_else(|| dl::info().param(id).map(|p| p.default))
		.unwrap_or(0.0)
}

fn differs(a: f32, b: f32) -> bool {
	(a - b).abs() > 1e-4 * a.abs().max(1.0)
}

/// The changes that differ from the document, committed as one undo step.
fn commit(state: &mut AppState, r: DeviceRef, changes: &[dl::Change], label: &str) {
	let real: Vec<(&str, f32)> = changes
		.iter()
		.filter(|(id, v)| differs(model_value(state, r, id), *v))
		.map(|&(id, v)| (id, v))
		.collect();
	for (id, _) in changes {
		if !real.iter().any(|(r, _)| r == id) {
			state.drop_param_preview(r.device, id);
		}
	}
	if !real.is_empty() {
		state.commit_params(r.track, r.device, &real, label);
	}
}

fn bpm_at_playhead(state: &AppState) -> f64 {
	state.project().tempo.bpm_at(state.position())
}

pub fn delay_body(ui: &mut Ui, state: &mut AppState, cx: &BodyCx) {
	let shared = view(ui.ctx(), view_id(cx.salt, cx.r));
	let mut v = lock(&shared);
	if cx.compact {
		compact(ui, state, cx, &mut v);
	} else {
		dock(ui, state, cx, &mut v);
	}
}

// --- Taps picture -----------------------------------------------------------------------

fn lane_color(lane: dl::Lane) -> Color32 {
	match lane {
		dl::Lane::Left => theme::STEREO_L,
		dl::Lane::Right => theme::STEREO_R,
		dl::Lane::Center => theme::TEXT,
	}
}

fn taps(
	ui: &mut Ui,
	state: &mut AppState,
	cx: &BodyCx,
	v: &mut DelayView,
	rect: Rect,
	compact: bool,
) {
	let r = cx.r;
	let values = shown_values(state, r);
	let bpm = bpm_at_playhead(state);
	let ms = values.echo_ms(bpm);
	let count = dl::tap_count(values.feedback);
	let ping_pong = values.ping_pong >= 0.5;
	let view_ms = dl::view_ms(&values, bpm);
	let (left, top, bottom) = if compact {
		(16.0, 18.0, 8.0)
	} else {
		(22.0, 26.0, 12.0)
	};
	let plot = Rect::from_min_max(
		pos2(rect.left() + left, rect.top() + top),
		pos2(rect.right() - 12.0, rect.bottom() - bottom),
	);
	// Time 0 (the dry hit) sits a little inside the plot so its bar is not cut.
	let x0 = plot.left() + 6.0;
	let px_per_ms = (plot.right() - x0) / view_ms.max(1.0);
	let x_of = |t_ms: f32| x0 + t_ms.clamp(0.0, view_ms) * px_per_ms;
	let mid = plot.center().y;
	let half = plot.height() * 0.5 - 4.0;
	let bar_w = (ms * px_per_ms * 0.1).clamp(5.0, if compact { 8.0 } else { 12.0 });
	// Outer end of a tap: up for the left lane (and centre), down for the right.
	let tip = |level: f32, lane: dl::Lane| match lane {
		dl::Lane::Right => mid + level * half,
		_ => mid - level * half,
	};
	let first_lane = dl::tap_lane(1, ping_pong);
	let first_at = pos2(x_of(ms), tip(dl::tap_level(1, values.feedback), first_lane));

	if ui.is_rect_visible(rect) {
		let p = ui.painter().with_clip_rect(rect);
		device_kit::paint_display(&p, rect);
		let label_font = theme::mono(if compact { 9.0 } else { 10.5 });

		// A true time grid, independent of the taps: bars, beats and sixteenths at the
		// project tempo when synced, round milliseconds when free.
		for line in dl::grid_lines(view_ms, bpm, values.is_free(), px_per_ms, 6.0) {
			let x = x_of(line.ms);
			let color = match line.kind {
				dl::GridKind::Bar => theme::BG_HOVER,
				dl::GridKind::Beat => device_kit::GRID_MAJOR,
				dl::GridKind::Minor => device_kit::GRID_MINOR,
			};
			p.vline(x, plot.y_range(), Stroke::new(1.0, color));
			if let Some(label) = line.label
				&& x + 28.0 < plot.right()
			{
				let strong = line.kind == dl::GridKind::Bar;
				p.text(
					pos2(x + 3.0, rect.top() + top * 0.5),
					Align2::LEFT_CENTER,
					label,
					label_font.clone(),
					if strong {
						theme::TEXT_DIM
					} else {
						theme::TEXT_FAINT
					},
				);
			}
		}
		if !values.is_free() {
			p.text(
				pos2(x0 + 3.0, rect.top() + top * 0.5),
				Align2::LEFT_CENTER,
				"1",
				label_font.clone(),
				theme::TEXT_DIM,
			);
		}
		p.hline(plot.x_range(), mid, Stroke::new(1.0, theme::BG_HOVER));
		if ping_pong {
			p.text(
				pos2(rect.left() + left * 0.5, plot.top() + 6.0),
				Align2::CENTER_TOP,
				"L",
				theme::medium(10.5),
				theme::STEREO_L,
			);
			p.text(
				pos2(rect.left() + left * 0.5, plot.bottom() - 6.0),
				Align2::CENTER_BOTTOM,
				"R",
				theme::medium(10.5),
				theme::STEREO_R,
			);
		}

		let plain = p.with_clip_rect(plot.expand2(vec2(4.0, 2.0)));
		// The dry hit at 0, then each repeat: a rounded bar from the centre line whose
		// length is its level, bouncing L / R with ping-pong, darker as the feedback
		// filter eats into it.
		let bar = |x: f32, from: f32, to: f32| {
			Rect::from_x_y_ranges(
				x - bar_w * 0.5..=x + bar_w * 0.5,
				from.min(to)..=from.max(to),
			)
		};
		// The dry hit.
		plain.rect_filled(
			bar(x0, mid - half * 0.95, mid + half * 0.95),
			CornerRadius::same(2),
			theme::TEXT.gamma_multiply(0.75),
		);
		let mut envelope = vec![pos2(x0, mid - half * 0.95)];
		for n in 1..=count as u32 {
			let x = x_of(n as f32 * ms);
			let level = dl::tap_level(n, values.feedback).max(3.0 / half.max(1.0));
			let lane = dl::tap_lane(n, ping_pong);
			let dark = dl::tap_darkness(n, values.low_cut, values.high_cut);
			let color = theme::mix(lane_color(lane), theme::DISPLAY_BG, dark * 0.7)
				.gamma_multiply((1.0 - n as f32 * 0.04).max(0.45));
			let end = tip(level, lane);
			let from = if lane == dl::Lane::Center {
				mid + level * half
			} else {
				mid
			};
			let body = bar(x, from, end);
			if n == 1 {
				plain.rect_filled(
					body.expand(4.0),
					CornerRadius::same(5),
					color.gamma_multiply(0.18),
				);
				plain.rect_filled(
					body.expand(8.0),
					CornerRadius::same(8),
					color.gamma_multiply(0.06),
				);
			}
			if n > 1 {
				plain.rect_filled(
					body.expand(3.0),
					CornerRadius::same(4),
					color.gamma_multiply(0.1),
				);
			}
			plain.rect_filled(body, CornerRadius::same(2), color.gamma_multiply(0.8));
			// The head: a brighter rounded cap at the tap's level.
			let color = theme::mix(color, Color32::WHITE, 0.2);
			let head_h = (bar_w * 0.75).min(body.height());
			let head = if lane == dl::Lane::Right {
				Rect::from_x_y_ranges(body.x_range(), end - head_h..=end)
			} else {
				Rect::from_x_y_ranges(body.x_range(), end..=end + head_h)
			};
			plain.rect_filled(head, CornerRadius::same(2), color);
			envelope.push(pos2(x, mid - level * half));
		}
		plain.extend(Shape::dashed_line(
			&envelope,
			Stroke::new(1.0, theme::TEXT_DIM.gamma_multiply(0.5)),
			3.0,
			3.0,
		));

		// The first echo's node.
		let lit = v.drag.is_some();
		let node_color = lane_color(first_lane);
		if lit {
			plain.circle_filled(first_at, 13.0, node_color.gamma_multiply(0.2));
		}
		plain.circle_filled(
			first_at,
			6.5,
			if lit {
				theme::mix(node_color, Color32::WHITE, 0.3)
			} else {
				node_color
			},
		);
		plain.circle_stroke(first_at, 6.5, Stroke::new(1.5, theme::DISPLAY_BG));

		if !compact {
			let level = dl::tag_level(values.feedback, ping_pong);
			device_kit::paint_value_tag(
				&p,
				pos2(first_at.x + 14.0, mid - level * half),
				Align2::LEFT_CENTER,
				&dl::echo_tag(&values, bpm),
				theme::TEXT,
				plot,
			);
			p.text(
				pos2(plot.right() - 4.0, plot.bottom() - 4.0),
				Align2::RIGHT_BOTTOM,
				dl::feedback_tag(&values, bpm),
				label_font.clone(),
				theme::TEXT_FAINT,
			);
		}
	}

	// The first echo: horizontal drag changes the time (snapping to the sync grid unless
	// free), vertical drag the feedback.
	let node_id = Id::new(("delay-first-echo", cx.salt, r.device));
	let node_rect = Rect::from_center_size(first_at, egui::Vec2::splat(22.0));
	let node = ui
		.interact(node_rect, node_id, Sense::click_and_drag())
		.on_hover_cursor(CursorIcon::Grab)
		.on_hover_text(format!(
			"{} · drag: \u{2194} time, \u{2195} feedback · double-click resets",
			dl::readout(&values, bpm)
		));
	node.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Other, true, "First echo"));
	let ms_per_px = 1.0 / px_per_ms.max(1e-4);
	let pct_per_px = 100.0 / half.max(1.0);
	let flip = if first_lane == dl::Lane::Right {
		-1.0
	} else {
		1.0
	};
	let change_of = |d: &EchoDrag| {
		let time = dl::drag_first_echo(d.start_ms, d.px.x * ms_per_px, bpm, values.is_free());
		let feedback = dl::drag_feedback(d.start_feedback, -d.px.y * flip * pct_per_px);
		let time: dl::Change = match time {
			dl::TimeChange::Sync(i) => (SYNC, i as f32),
			dl::TimeChange::Free(ms) => (TIME, ms),
		};
		(time, feedback)
	};
	if node.drag_started() {
		v.drag = Some(EchoDrag {
			start_ms: ms,
			start_feedback: values.feedback,
			px: egui::Vec2::ZERO,
		});
	}
	if node.dragged()
		&& let Some(d) = &mut v.drag
	{
		d.px += node.drag_delta();
		let ((param, time_value), feedback) = change_of(d);
		state.preview_param(r.device, param, time_value);
		state.preview_param(r.device, FEEDBACK, feedback);
	}
	if node.drag_stopped()
		&& let Some(d) = v.drag.take()
	{
		let (time, feedback) = change_of(&d);
		commit(state, r, &[time, (FEEDBACK, feedback)], "Delay echo");
	}
	if node.double_clicked() {
		commit(state, r, &dl::reset(), "Reset delay echo");
	}

	ui.interact(
		rect,
		Id::new(("delay-taps", cx.salt, r.device)),
		Sense::hover(),
	)
	.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Other, true, "Delay taps"));
}

fn row(ui: &mut Ui, rect: Rect) -> Ui {
	ui.new_child(
		UiBuilder::new()
			.max_rect(rect)
			.layout(Layout::left_to_right(Align::Center)),
	)
}

// --- Sync / ping-pong / filter ---------------------------------------------------------

fn sync_mode(ui: &mut Ui, state: &mut AppState, cx: &BodyCx) {
	let r = cx.r;
	let values = shown_values(state, r);
	let selected = usize::from(values.is_free());
	if let Some(i) = device_kit::segmented(
		ui,
		Id::new(("delay-sync-mode", cx.salt, r.device)),
		&["Sync", "Free"],
		selected,
	) {
		let sync = if i == 1 { 0.0 } else { dl::SYNC_1_8 as f32 };
		if differs(sync, values.sync) {
			commit(state, r, &[(SYNC, sync)], "Delay sync");
		}
	}
}

/// The 12 note-value steps (the delay's `sync` choices minus "Free" at index 0), disabled
/// in free mode.
fn note_grid(ui: &mut Ui, state: &mut AppState, cx: &BodyCx, columns: usize, cell: egui::Vec2) {
	let r = cx.r;
	let values = shown_values(state, r);
	let names: Vec<String> = crate::state::device::delay_sync_choices()[1..]
		.iter()
		.map(|c| crate::state::device::short_choice(c))
		.collect();
	let names: Vec<&str> = names.iter().map(String::as_str).collect();
	let step = values.sync.round().clamp(0.0, 12.0) as usize;
	let grid_selected = step.saturating_sub(1).min(names.len() - 1);
	ui.scope(|ui| {
		if values.is_free() {
			ui.disable();
		}
		if let Some(i) = device_kit::grid_selector(
			ui,
			Id::new(("delay-sync-grid", cx.salt, r.device)),
			&names,
			grid_selected,
			columns,
			cell,
		) {
			commit(state, r, &[(SYNC, (i + 1) as f32)], "Delay sync");
		}
	});
}

fn ping_pong_toggle(ui: &mut Ui, state: &mut AppState, r: DeviceRef) {
	device_kit::param_toggle(
		ui,
		state,
		r,
		PING_PONG,
		"\u{21c4} Ping-pong",
		theme::STEREO_L,
	);
}

fn filter_strip(ui: &mut Ui, state: &mut AppState, cx: &BodyCx, rect: Rect) {
	let r = cx.r;
	let values = shown_values(state, r);
	let cap = Rect::from_min_size(rect.min, vec2(rect.width(), CAPTION_H));
	let p = ui.painter();
	p.text(
		cap.left_center(),
		Align2::LEFT_CENTER,
		"Feedback filter",
		theme::medium(10.5),
		theme::TEXT_FAINT,
	);
	p.text(
		cap.right_center(),
		Align2::RIGHT_CENTER,
		format!(
			"{} \u{2013} {}",
			crate::state::device::format_value(values.low_cut, "Hz"),
			crate::state::device::format_value(values.high_cut, "Hz")
		),
		theme::mono(10.5),
		theme::TEXT_DIM,
	);
	let display_rect = Rect::from_min_max(pos2(rect.left(), cap.bottom() + 2.0), rect.max);
	let mut child = ui.new_child(UiBuilder::new().max_rect(display_rect));
	let node = |label, freq, id| FreqNode {
		label,
		freq,
		db: 0.0,
		color: BAND,
		dormant: false,
		x_only: true,
		ai: state.ai_change(r.device, id).is_some(),
	};
	let nodes = [
		node("LC", values.low_cut, LOW_CUT),
		node("HC", values.high_cut, HIGH_CUT),
	];
	let (low, high) = (values.low_cut, values.high_cut);
	let response = move |f: f32| dl::filter_response_db(low, high, f);
	let out = FreqDisplay::new(
		Id::new(("delay-filter", cx.salt, r.device)),
		display_rect.size(),
	)
	.range(FILTER_RANGE)
	.sum(&response, BAND)
	.sum_fill(0.16)
	.small_nodes()
	.nodes(&nodes)
	.show(&mut child);
	for e in out.events {
		match e {
			FreqEvent::Move {
				node,
				freq,
				commit: done,
				..
			} => {
				let id = if node == 0 { LOW_CUT } else { HIGH_CUT };
				if done {
					commit(state, r, &[(id, freq)], "Delay feedback filter");
				} else {
					state.preview_param(r.device, id, freq);
				}
			}
			FreqEvent::Reset { node } => {
				let id = if node == 0 { LOW_CUT } else { HIGH_CUT };
				let default = dl::info().param(id).unwrap().default;
				commit(state, r, &[(id, default)], "Reset delay filter");
			}
			_ => {}
		}
	}
	out.response.on_hover_text(format!(
		"{}: what each repeat keeps. Drag a node, double-click resets",
		dl::filter_label(values.low_cut, values.high_cut)
	));
}

fn readout_chip(ui: &mut Ui, text: &str) {
	let g = ui
		.painter()
		.layout_no_wrap(text.to_owned(), theme::mono(12.0), theme::TEXT);
	let (rect, _) = ui.allocate_exact_size(vec2(g.size().x + 16.0, CHIP_H), Sense::hover());
	ui.painter()
		.rect_filled(rect, CornerRadius::same(theme::RADIUS_MD), theme::BG_WIDGET);
	ui.painter()
		.galley(rect.center() - g.size() * 0.5, g, theme::TEXT);
}

/// Time (the note value when synced, ms when free), feedback and the filter's two cuts.
fn knob_grid(
	ui: &mut Ui,
	state: &mut AppState,
	cx: &BodyCx,
	rect: Rect,
	size: KnobSize,
	cols: usize,
) {
	let values = shown_values(state, cx.r);
	let names: Vec<String> = crate::state::device::delay_sync_choices()
		.iter()
		.map(|c| crate::state::device::short_choice(c))
		.collect();
	let names: Vec<&str> = names.iter().map(String::as_str).collect();
	let knobs: [(&str, &str); 4] = [
		(if values.is_free() { TIME } else { SYNC }, "Time"),
		(FEEDBACK, "Feedback"),
		(LOW_CUT, "Low cut"),
		(HIGH_CUT, "High cut"),
	];
	let rows = knobs.len().div_ceil(cols);
	let cell = vec2(rect.width() / cols as f32, rect.height() / rows as f32);
	for (i, (id, label)) in knobs.into_iter().enumerate() {
		let at = rect.min + vec2((i % cols) as f32 * cell.x, (i / cols) as f32 * cell.y);
		let mut cell_ui = ui.new_child(
			UiBuilder::new()
				.max_rect(Rect::from_min_size(at, cell))
				.layout(Layout::top_down(Align::Center)),
		);
		let mut knob = ParamKnob::new(cx.r, id)
			.size(size)
			.label(label)
			.color(if i >= 2 { BAND } else { theme::TEXT })
			.id_salt(Id::new(cx.salt));
		if id == SYNC {
			knob = knob.choices(&names);
		}
		knob.show(&mut cell_ui, state);
	}
}

// --- Dock (16c) -------------------------------------------------------------------------

const MODE_ROW_H: f32 = 26.0;
const NOTE_CELL_H: f32 = 28.0;

fn dock(ui: &mut Ui, state: &mut AppState, cx: &BodyCx, v: &mut DelayView) {
	let rect = cx.rect;
	let top = Rect::from_min_max(rect.min, pos2(rect.right(), rect.bottom() - FOOTER_H - 4.0));
	let col_w = if rect.width() >= 1250.0 { 320.0 } else { 284.0 };
	let knobs_w = if rect.width() >= 1450.0 { 220.0 } else { 0.0 };
	let knob_gap = if knobs_w > 0.0 { GAP } else { 0.0 };
	let taps_w = (top.width() - col_w - knobs_w - GAP - knob_gap).max(300.0);
	let taps_rect = top.with_max_x(top.left() + taps_w);
	taps(ui, state, cx, v, taps_rect, false);

	let col = Rect::from_min_size(
		pos2(taps_rect.right() + GAP, top.top()),
		vec2(col_w, top.height()),
	);
	let mut mode = row(
		ui,
		Rect::from_min_size(col.min, vec2(col.width(), MODE_ROW_H)),
	);
	sync_mode(&mut mode, state, cx);
	mode.with_layout(Layout::right_to_left(Align::Center), |ui| {
		ping_pong_toggle(ui, state, cx.r);
	});
	let grid_top = col.top() + MODE_ROW_H + 8.0;
	let grid_h = NOTE_CELL_H * 3.0 + 4.0;
	let mut grid = ui.new_child(
		UiBuilder::new()
			.max_rect(Rect::from_min_size(
				pos2(col.left(), grid_top),
				vec2(col.width(), grid_h),
			))
			.layout(Layout::top_down(Align::Min)),
	);
	note_grid(
		&mut grid,
		state,
		cx,
		4,
		vec2((col.width() - 4.0) / 4.0, NOTE_CELL_H),
	);
	let strip = Rect::from_min_max(pos2(col.left(), grid_top + grid_h + 10.0), col.max);
	filter_strip(ui, state, cx, strip);

	if knobs_w > 0.0 {
		let knobs = Rect::from_min_max(pos2(col.right() + GAP, top.top()), top.max);
		knob_grid(ui, state, cx, knobs.shrink2(vec2(0.0, 6.0)), KnobSize::M, 2);
	}

	let foot = Rect::from_min_size(
		pos2(rect.left() - 8.0, rect.bottom() - FOOTER_H),
		vec2(rect.width() + 16.0, FOOTER_H),
	);
	ui.painter().hline(
		foot.x_range(),
		foot.top(),
		Stroke::new(1.0, theme::BORDER_SUBTLE),
	);
	let mut bar = row(ui, foot.shrink2(vec2(10.0, 0.0)));
	let readout = dl::readout(&shown_values(state, cx.r), bpm_at_playhead(state));
	bar.with_layout(Layout::right_to_left(Align::Center), |ui| {
		readout_chip(ui, &readout);
		ui.add_space(12.0);
		ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
			ui.add(
				egui::Label::new(
					egui::RichText::new(
						"Drag the first echo: \u{2194} time, \u{2195} feedback. Each repeat gets \
					 darker as the feedback filter removes highs and lows.",
					)
					.font(theme::medium(12.0))
					.color(theme::TEXT_DIM),
				)
				.truncate(),
			);
		});
	});
}

// --- Compact ------------------------------------------------------------------------------

const COMPACT_KNOBS_H: f32 = 80.0;
const COMPACT_ROW_H: f32 = 32.0;
const COMPACT_CELL_H: f32 = 24.0;
const COMPACT_GRID_H: f32 = COMPACT_CELL_H * 2.0 + 4.0;

/// How tall the compact form is at `width`.
pub fn compact_height(width: f32) -> f32 {
	compact_taps_h(width) + COMPACT_ROW_H + COMPACT_GRID_H + COMPACT_KNOBS_H + 8.0
}

fn compact_taps_h(width: f32) -> f32 {
	(width * 0.42).clamp(110.0, 170.0)
}

fn compact(ui: &mut Ui, state: &mut AppState, cx: &BodyCx, v: &mut DelayView) {
	let rect = cx.rect;
	let mut inner = ui.new_child(
		UiBuilder::new()
			.max_rect(rect)
			.layout(Layout::top_down(Align::Min)),
	);
	egui::ScrollArea::vertical()
		.id_salt(("delay-compact", cx.salt, cx.r.device))
		.auto_shrink([false, false])
		.show(&mut inner, |ui| {
			ui.spacing_mut().item_spacing.y = 0.0;
			let width = ui.available_width();
			let (t, _) = ui.allocate_exact_size(vec2(width, compact_taps_h(width)), Sense::hover());
			taps(ui, state, cx, v, t, true);
			let (row_rect, _) = ui.allocate_exact_size(vec2(width, COMPACT_ROW_H), Sense::hover());
			let mut r = row(ui, row_rect);
			sync_mode(&mut r, state, cx);
			r.with_layout(Layout::right_to_left(Align::Center), |ui| {
				ping_pong_toggle(ui, state, cx.r);
			});
			let (g, _) = ui.allocate_exact_size(vec2(width, COMPACT_GRID_H), Sense::hover());
			let mut grid = ui.new_child(
				UiBuilder::new()
					.max_rect(g)
					.layout(Layout::top_down(Align::Min)),
			);
			note_grid(
				&mut grid,
				state,
				cx,
				6,
				vec2((width - 4.0) / 6.0, COMPACT_CELL_H),
			);
			ui.add_space(8.0);
			let (k, _) = ui.allocate_exact_size(vec2(width, COMPACT_KNOBS_H), Sense::hover());
			knob_grid(ui, state, cx, k, KnobSize::S, 4);
		});
}
