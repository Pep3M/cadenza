//! Recording controls: arm / input / monitor buttons on track headers and mixer strips,
//! the clip being recorded drawn live, take lanes (pick or swipe-comp takes) and the
//! audio settings dialog.

use cadenza_core::{ClipId, Tick, TrackId};
use cadenza_engine::record::PEAK_BLOCK;
use cadenza_model::{Clip, Command, MonitorMode, Track, TrackInput, TrackKind};
use egui::{
	Align2, Color32, CornerRadius, FontId, Frame, Id, Margin, Painter, PointerButton, Rect,
	RichText, Sense, Stroke, Ui, pos2, vec2,
};

use crate::state::AppState;
use crate::state::record::{AudioPrefs, InputQuantize};
use crate::state::timeline::Timeline;
use crate::theme;
use crate::ui::widgets;

/// Paints a round record-arm button.
pub fn paint_arm(p: &Painter, rect: Rect, armed: bool, hovered: bool) {
	let bg = if armed {
		theme::mix(theme::RECORD, theme::BG_DEEP, 0.35)
	} else if hovered {
		theme::BG_HOVER
	} else {
		theme::BG_WIDGET
	};
	p.rect_filled(rect, CornerRadius::same(theme::RADIUS), bg);
	let r = rect.height().min(rect.width()) * 0.24;
	if armed {
		p.circle_filled(rect.center(), r, theme::RECORD);
	} else {
		p.circle_stroke(rect.center(), r, Stroke::new(1.5, theme::RECORD));
	}
}

/// Header row-1 buttons left of `right_of` (arm, and T when the track has takes).
/// Returns the leftmost x they use.
pub fn header_buttons(
	ui: &mut Ui,
	state: &mut AppState,
	track: &Track,
	id: Id,
	right_of: Rect,
) -> f32 {
	let p = ui.painter().clone();
	let mut left = right_of.left();
	if crate::state::rows::take_lanes(track) > 0 {
		let r = right_of.translate(vec2(-21.0, 0.0));
		let open = state.takes_open(track.id);
		let resp = ui.interact(r, id.with("takes"), Sense::click());
		widgets::paint_toggle(&p, r, open, "T", theme::ACCENT, resp.hovered());
		if resp
			.on_hover_text("Show / hide the take lanes (comping)")
			.clicked()
		{
			state.toggle_takes(track.id);
		}
		left = r.left();
	}
	if matches!(track.kind, TrackKind::Audio | TrackKind::Instrument) {
		let r = Rect::from_min_size(pos2(left - 21.0, right_of.top()), right_of.size());
		let armed = state.is_armed(track.id);
		let resp = ui.interact(r, id.with("arm"), Sense::click());
		let label = format!("Arm {}", track.name);
		resp.widget_info(|| {
			egui::WidgetInfo::selected(egui::WidgetType::Checkbox, true, armed, &label)
		});
		paint_arm(&p, r, armed, resp.hovered());
		let tip = if track.kind == TrackKind::Audio {
			"Arm for recording (audio input)"
		} else {
			"Arm for recording (MIDI input; also plays live MIDI)"
		};
		if resp.on_hover_text(tip).clicked() {
			state.toggle_arm(track.id);
		}
		left = r.left();
	}
	left
}

/// The choices of an input selector for a device with `channels` inputs.
pub fn input_choices(channels: u16) -> Vec<TrackInput> {
	let mut out: Vec<TrackInput> = (0..channels).map(TrackInput::mono).collect();
	out.extend(
		(0..channels.saturating_sub(1))
			.step_by(2)
			.map(TrackInput::stereo),
	);
	out
}

/// Audio header row 2: input selector, monitor mode and the input level.
pub fn input_controls(ui: &mut Ui, state: &mut AppState, track: &Track, id: Id, rect: Rect) {
	let p = ui.painter().clone();
	let input = track.input;
	let in_rect = Rect::from_min_size(rect.min, vec2(52.0, rect.height()));
	let label = input.map_or("No input".to_owned(), |i| i.label());
	let resp = ui.interact(in_rect, id.with("input"), Sense::click());
	p.rect_filled(
		in_rect,
		CornerRadius::same(2),
		if resp.hovered() {
			theme::BG_HOVER
		} else {
			theme::BG_WIDGET
		},
	);
	p.text(
		in_rect.center(),
		Align2::CENTER_CENTER,
		&label,
		theme::small(),
		if input.is_some() {
			theme::TEXT
		} else {
			theme::TEXT_FAINT
		},
	);
	let channels = state.input_channels();
	egui::Popup::menu(&resp).show(|ui| {
		ui.set_min_width(120.0);
		if ui.selectable_label(input.is_none(), "No input").clicked() {
			state.set_track_input(track.id, None);
		}
		for choice in input_choices(channels) {
			let current =
				input.is_some_and(|i| i.channel == choice.channel && i.stereo == choice.stereo);
			if ui.selectable_label(current, choice.label()).clicked() {
				let monitor = input.map_or(MonitorMode::Auto, |i| i.monitor);
				state.set_track_input(track.id, Some(TrackInput { monitor, ..choice }));
			}
		}
	});
	resp.on_hover_text("Audio input this track records (and monitors)");
	let Some(input) = input else {
		return;
	};
	let mon_rect = Rect::from_min_size(
		pos2(in_rect.right() + 4.0, rect.top()),
		vec2(30.0, rect.height()),
	);
	let resp = ui.interact(mon_rect, id.with("monitor"), Sense::click());
	let (text, on) = match input.monitor {
		MonitorMode::Off => ("off", false),
		MonitorMode::Auto => ("auto", true),
		MonitorMode::On => ("on", true),
	};
	widgets::paint_toggle(&p, mon_rect, on, text, theme::PLAYING, resp.hovered());
	if resp
		.on_hover_text(
			"Input monitoring: off / auto (armed, while stopped or recording) / on (always; \
			 the input replaces the clips)",
		)
		.clicked()
	{
		state.cycle_monitor(track.id);
	}
	// Input level, before the track's inserts and fader.
	let meter = Rect::from_min_max(
		pos2(mon_rect.right() + 4.0, rect.center().y - 2.0),
		pos2(rect.right(), rect.center().y + 2.0),
	);
	if meter.width() > 6.0 {
		p.rect_filled(meter, 1.0, theme::METER_BG);
		let level = state.input_level(&input);
		let db = cadenza_core::gain_to_db(level.max(1e-9));
		let w = widgets::db_to_meter(db) * meter.width();
		let color = if db > -3.0 {
			theme::METER_HIGH
		} else {
			theme::METER_LOW
		};
		p.rect_filled(
			Rect::from_min_size(meter.min, vec2(w, meter.height())),
			1.0,
			color,
		);
	}
}

/// The record-arm toggle of a mixer strip.
pub fn mixer_arm(ui: &mut Ui, state: &mut AppState, track: TrackId, width: f32) {
	let (rect, resp) = ui.allocate_exact_size(vec2(width, 18.0), Sense::click());
	paint_arm(ui.painter(), rect, state.is_armed(track), resp.hovered());
	if resp.on_hover_text("Arm for recording").clicked() {
		state.toggle_arm(track);
	}
}

/// Draws, over a track's clip lane, the clip being recorded (live waveform or notes) and
/// a badge on take clips.
pub fn paint_track_overlay(
	p: &Painter,
	state: &AppState,
	track: &Track,
	lane: Rect,
	tl: &Timeline,
) {
	let x = |t: Tick| lane.left() + tl.tick_to_x(t);
	// Takes badge: "n takes" on the playing clip of each group.
	for c in track.clips.iter().filter(|c| c.plays() && c.take.is_some()) {
		let group = c.take.map(|t| t.group);
		let n = track
			.clips
			.iter()
			.filter(|o| o.take.map(|t| t.group) == group)
			.count();
		if n < 2 {
			continue;
		}
		let (x0, x1) = (x(c.start), x(c.end()));
		if x1 < lane.left() || x0 > lane.right() || x1 - x0 < 30.0 {
			continue;
		}
		let badge = Rect::from_min_size(pos2(x1 - 22.0, lane.top() + 3.0), vec2(19.0, 12.0));
		p.rect_filled(badge, CornerRadius::same(2), Color32::from_black_alpha(140));
		p.text(
			badge.center(),
			Align2::CENTER_CENTER,
			format!("{n}"),
			FontId::monospace(9.0),
			theme::TEXT,
		);
	}
	let (Some(monitor), Some(live)) = (state.recording_monitor(), &state.record.live) else {
		return;
	};
	let Some(rec) = monitor.tracks().iter().find(|t| t.track == track.id) else {
		return;
	};
	let sr = f64::from(monitor.sample_rate().max(1));
	let tempo = &state.project().tempo;
	let tick = |s: i64| tempo.seconds_to_tick(s.max(0) as f64 / sr);
	let now = state.position();
	let (start, file_start) = match live.passes.last() {
		Some(pass) => (tick(pass.tag), Some((pass.file_start, pass.frames))),
		None => (tick(monitor.start_sample()), None),
	};
	if now <= start && rec.input.is_some() && file_start.is_none() {
		return;
	}
	let end = if now > start { now } else { start + Tick(1) };
	let body = Rect::from_x_y_ranges(
		x(start).max(lane.left())..=x(end).min(lane.right()),
		lane.top() + 2.0..=lane.bottom() - 2.0,
	);
	if body.width() <= 0.0 {
		return;
	}
	p.rect_filled(
		body,
		CornerRadius::same(3),
		theme::RECORD.gamma_multiply(0.22),
	);
	p.rect_stroke(
		body,
		CornerRadius::same(3),
		Stroke::new(1.0, theme::RECORD),
		egui::StrokeKind::Inside,
	);
	if rec.input.is_some() {
		let Some((file_start, frames)) = file_start else {
			return;
		};
		let Some((_, peaks)) = live.peaks.iter().find(|(t, _)| *t == track.id) else {
			return;
		};
		let first = file_start as usize / PEAK_BLOCK;
		let count = (frames as usize / PEAK_BLOCK).min(peaks.len().saturating_sub(first));
		if count == 0 {
			return;
		}
		let x0 = x(start);
		let px_per_peak = (x(tick(
			live.passes.last().map_or(0, |p| p.tag) + (count * PEAK_BLOCK) as i64,
		)) - x0) / count as f32;
		let mid = body.center().y;
		let half = body.height() * 0.45;
		let mut col = x0.max(lane.left());
		while col < body.right() {
			let i0 = ((col - x0) / px_per_peak.max(1e-3)) as usize;
			let i1 = (((col + 1.0 - x0) / px_per_peak.max(1e-3)) as usize).max(i0 + 1);
			let peak = peaks[first + i0.min(count - 1)..first + i1.min(count)]
				.iter()
				.fold(0.0f32, |m, v| m.max(*v));
			let h = peak.min(1.0) * half;
			if h > 0.3 {
				p.vline(col, (mid - h)..=(mid + h), Stroke::new(1.0, theme::RECORD));
			}
			col += 1.0;
		}
	} else {
		let notes = monitor.midi_notes(tempo, now);
		if notes.is_empty() {
			return;
		}
		let lo = notes.iter().map(|n| n.pitch).min().unwrap_or(60);
		let hi = notes
			.iter()
			.map(|n| n.pitch)
			.max()
			.unwrap_or(60)
			.max(lo + 1);
		let span = f32::from(hi - lo);
		for n in notes.iter().filter(|n| n.start >= start) {
			let y = body.bottom() - 4.0 - f32::from(n.pitch - lo) / span * (body.height() - 10.0);
			let r = Rect::from_x_y_ranges(
				x(n.start)..=x(n.start + n.length).max(x(n.start) + 2.0),
				y - 1.5..=y + 1.5,
			);
			p.rect_filled(r.intersect(body), 0.0, theme::RECORD);
		}
	}
}

/// The header of take lane `lane` under `track`: name, playing marker, menu.
pub fn take_header(ui: &mut Ui, state: &mut AppState, track: TrackId, lane: u32, rect: Rect) {
	let p = ui.painter().clone();
	p.rect_filled(rect, 0.0, theme::AUTO_LANE);
	p.hline(
		rect.x_range(),
		rect.bottom() - 0.5,
		Stroke::new(1.0, theme::GRID_BEAT),
	);
	let playing = state.project().track(track).is_some_and(|t| {
		t.clips
			.iter()
			.any(|c| c.take.is_some_and(|k| k.lane == lane && k.active))
	});
	p.text(
		pos2(rect.left() + 24.0, rect.center().y),
		Align2::LEFT_CENTER,
		format!("Take {}", lane + 1),
		theme::small(),
		if playing {
			theme::TEXT
		} else {
			theme::TEXT_DIM
		},
	);
	if playing {
		p.circle_filled(
			pos2(rect.left() + 14.0, rect.center().y),
			3.0,
			theme::PLAYING,
		);
	}
	let resp = ui.interact(rect, Id::new(("take-header", track, lane)), Sense::click());
	if resp.double_clicked() {
		state.select_lane(track, lane);
	}
	resp.on_hover_text(
		"Double-click: play this take everywhere; click or drag a take in its lane to comp",
	)
	.context_menu(|ui| {
		if ui.button("Play this take everywhere").clicked() {
			state.select_lane(track, lane);
			ui.close();
		}
	});
}

/// Gesture state of a take lane (press position in ticks).
#[derive(Clone, Copy)]
struct Swipe {
	from: Tick,
	clip: ClipId,
}

/// Take lane `lane` of `track`: its takes drawn (bright = playing), click plays a take
/// for its region, dragging over a take plays it for the swiped range (comping).
pub fn take_lane(
	ui: &mut Ui,
	state: &mut AppState,
	track: &Track,
	lane: u32,
	rect: Rect,
	area: Rect,
	tl: &Timeline,
) {
	let r = rect.intersect(area);
	if r.height() <= 0.0 {
		return;
	}
	let p = ui.painter_at(area);
	let x = |t: Tick| area.left() + tl.tick_to_x(t);
	let color = theme::track_color(track.color.as_deref());
	let clips: Vec<&Clip> = track
		.clips
		.iter()
		.filter(|c| c.take.is_some_and(|t| t.lane == lane))
		.collect();
	for c in &clips {
		let body = Rect::from_x_y_ranges(
			x(c.start)..=x(c.end()),
			rect.top() + 3.0..=rect.bottom() - 3.0,
		);
		if !body.intersects(area) {
			continue;
		}
		let active = c.plays();
		let fill = if active {
			theme::mix(color, theme::BG_DEEP, 0.25)
		} else {
			theme::mix(color, theme::BG_DEEP, 0.75)
		};
		p.rect_filled(body, CornerRadius::same(2), fill);
		if active {
			p.rect_stroke(
				body,
				CornerRadius::same(2),
				Stroke::new(1.5, Color32::WHITE),
				egui::StrokeKind::Inside,
			);
		}
		if body.width() > 40.0 {
			p.text(
				pos2(body.left() + 5.0, body.center().y),
				Align2::LEFT_CENTER,
				&c.name,
				theme::small(),
				if active {
					Color32::WHITE
				} else {
					theme::TEXT_DIM
				},
			);
		}
	}
	let id = Id::new(("take-lane", track.id, lane));
	let resp = ui.interact(r, id, Sense::click_and_drag());
	let ts = state.project().time_signature;
	let snap = |state: &AppState, t: Tick| {
		if state.view.snap { tl.snap(t, ts) } else { t }
	};
	let clip_at = |px: f32| {
		let t = tl.x_to_tick(px - area.left());
		clips
			.iter()
			.find(|c| c.start <= t && t < c.end())
			.map(|c| c.id)
	};
	if resp.clicked()
		&& let Some(pos) = resp.interact_pointer_pos()
		&& let Some(clip) = clip_at(pos.x)
	{
		state.select_take(clip);
	}
	if resp.drag_started_by(PointerButton::Primary)
		&& let Some(pos) = ui.input(|i| i.pointer.press_origin())
		&& let Some(clip) = clip_at(pos.x)
	{
		let from = snap(state, tl.x_to_tick(pos.x - area.left()));
		ui.data_mut(|d| d.insert_temp(id, Swipe { from, clip }));
	}
	if let Some(swipe) = ui.data(|d| d.get_temp::<Swipe>(id))
		&& let Some(pos) = ui.ctx().pointer_interact_pos()
	{
		let to = snap(state, tl.x_to_tick(pos.x - area.left()));
		let band = Rect::from_x_y_ranges(
			x(swipe.from.min(to))..=x(swipe.from.max(to)),
			rect.top() + 1.0..=rect.bottom() - 1.0,
		);
		p.rect_filled(band, 0.0, theme::ACCENT.gamma_multiply(0.25));
		p.rect_stroke(
			band,
			0.0,
			Stroke::new(1.0, theme::ACCENT),
			egui::StrokeKind::Inside,
		);
		if resp.drag_stopped() || ui.input(|i| i.pointer.any_released()) {
			ui.data_mut(|d| d.remove::<Swipe>(id));
			if to != swipe.from {
				state.comp_take(swipe.clip, swipe.from, to);
			}
		}
	}
	let menu_clip = resp.interact_pointer_pos().and_then(|pos| clip_at(pos.x));
	resp.on_hover_text("Click: play this take here; drag: play it for the swiped range")
		.context_menu(|ui| {
			let Some(clip) = menu_clip else {
				ui.label("No take here");
				return;
			};
			if ui.button("Play this take").clicked() {
				state.select_take(clip);
				ui.close();
			}
			let group = state
				.project()
				.find_clip(clip)
				.and_then(|(_, c)| c.take_group());
			if let Some(group) = group
				&& ui.button("Flatten takes (keep only what plays)").clicked()
			{
				state.flatten_takes(group);
				ui.close();
			}
			if ui.button("Delete this take").clicked() {
				state.apply(Command::RemoveClip { clip });
				ui.close();
			}
		});
}

fn modal_frame() -> Frame {
	Frame::new()
		.fill(theme::BG_RAISED)
		.stroke(Stroke::new(1.0, theme::BORDER_STRONG))
		.corner_radius(6)
		.inner_margin(Margin::same(16))
}

fn section(ui: &mut Ui, title: &str) {
	ui.add_space(6.0);
	ui.label(
		RichText::new(title.to_uppercase())
			.font(theme::small())
			.color(theme::ACCENT),
	);
	ui.add_space(2.0);
}

fn device_combo(
	ui: &mut Ui,
	id: &str,
	value: &mut Option<String>,
	default_label: &str,
	names: &[String],
) {
	egui::ComboBox::from_id_salt(id)
		.width(300.0)
		.selected_text(value.clone().unwrap_or_else(|| default_label.to_owned()))
		.show_ui(ui, |ui| {
			ui.selectable_value(value, None, default_label);
			for n in names {
				ui.selectable_value(value, Some(n.clone()), n);
			}
		});
}

/// File → Audio Settings: devices, sample rate, buffer, latency offset, MIDI input, test
/// tone. Applied (and persisted) with Apply.
pub fn audio_settings(ctx: &egui::Context, state: &mut AppState) {
	let Some(mut dialog) = state.record.dialog.take() else {
		return;
	};
	let mut close = false;
	let mut apply = false;
	let (outputs, inputs, midi) = match &state.record.devices {
		Some((audio, midi)) => (
			audio
				.outputs
				.iter()
				.map(|d| d.name.clone())
				.collect::<Vec<_>>(),
			audio
				.inputs
				.iter()
				.map(|d| d.name.clone())
				.collect::<Vec<_>>(),
			midi.clone(),
		),
		None => (Vec::new(), Vec::new(), Vec::new()),
	};
	let status = state.record.status.clone();
	let mut test_tone = state.record.test_tone;
	egui::Modal::new(Id::new("audio-settings"))
		.frame(modal_frame())
		.show(ctx, |ui| {
			ui.set_width(520.0);
			ui.heading("Audio settings");
			let d: &mut AudioPrefs = &mut dialog.draft;
			if state.record.devices.is_none() {
				ui.label(
					RichText::new("Looking for devices…")
						.font(theme::small())
						.color(theme::TEXT_FAINT),
				);
			}
			section(ui, "Output");
			egui::Grid::new("audio-out")
				.num_columns(2)
				.spacing([12.0, 6.0])
				.show(ui, |ui| {
					ui.label("Device");
					device_combo(
						ui,
						"audio-output",
						&mut d.config.output,
						"System default",
						&outputs,
					);
					ui.end_row();
					ui.label("Sample rate");
					egui::ComboBox::from_id_salt("audio-rate")
						.selected_text(
							d.config
								.sample_rate
								.map_or("Device default".into(), |r| format!("{r} Hz")),
						)
						.show_ui(ui, |ui| {
							ui.selectable_value(&mut d.config.sample_rate, None, "Device default");
							for r in [44_100u32, 48_000, 88_200, 96_000] {
								ui.selectable_value(
									&mut d.config.sample_rate,
									Some(r),
									format!("{r} Hz"),
								);
							}
						});
					ui.end_row();
					ui.label("Buffer size");
					egui::ComboBox::from_id_salt("audio-buffer")
						.selected_text(
							d.config
								.buffer_frames
								.map_or("Device default".into(), |b| format!("{b} samples")),
						)
						.show_ui(ui, |ui| {
							ui.selectable_value(
								&mut d.config.buffer_frames,
								None,
								"Device default",
							);
							for b in [64u32, 128, 256, 512, 1024, 2048] {
								ui.selectable_value(
									&mut d.config.buffer_frames,
									Some(b),
									format!("{b} samples"),
								);
							}
						});
					ui.end_row();
				});
			section(ui, "Input");
			egui::Grid::new("audio-in")
				.num_columns(2)
				.spacing([12.0, 6.0])
				.show(ui, |ui| {
					ui.label("Audio input");
					ui.checkbox(
						&mut d.config.input_enabled,
						"Record and monitor audio input",
					);
					ui.end_row();
					ui.label("Device");
					ui.add_enabled_ui(d.config.input_enabled, |ui| {
						device_combo(
							ui,
							"audio-input",
							&mut d.config.input,
							"System default",
							&inputs,
						);
					});
					ui.end_row();
					ui.label("MIDI input");
					egui::ComboBox::from_id_salt("midi-input")
						.width(300.0)
						.selected_text(d.midi_input.clone().unwrap_or_else(|| "None".into()))
						.show_ui(ui, |ui| {
							ui.selectable_value(&mut d.midi_input, None, "None");
							for m in &midi {
								ui.selectable_value(&mut d.midi_input, Some(m.clone()), m);
							}
						});
					ui.end_row();
					ui.label("Latency offset");
					ui.horizontal(|ui| {
						ui.add(
							egui::DragValue::new(&mut d.latency_offset_ms)
								.range(-200.0..=200.0)
								.speed(0.1)
								.suffix(" ms"),
						);
						ui.label(
							RichText::new(
								"added to the measured latency; positive moves recordings earlier",
							)
							.font(theme::small())
							.color(theme::TEXT_FAINT),
						);
					});
					ui.end_row();
				});
			section(ui, "Status");
			match &status {
				Some(s) => {
					ui.label(format!(
						"Output: {} at {} Hz (latency {:.1} ms)",
						s.output,
						s.sample_rate,
						s.output_latency as f64 * 1000.0 / f64::from(s.sample_rate.max(1))
					));
					match (&s.input, &s.input_error) {
						(Some(i), _) => ui.label(format!(
							"Input: {} — {} channel(s) at {} Hz{}; recordings shift by {:.1} ms",
							i.device,
							i.channels,
							i.sample_rate,
							if matches!(i.mode, cadenza_engine::InputMode::Exact) {
								", same clock as the output"
							} else {
								", resampled with drift correction"
							},
							s.recording_latency as f64 * 1000.0 / f64::from(s.sample_rate.max(1))
						)),
						(None, Some(e)) => ui.colored_label(theme::ERROR, format!("Input: {e}")),
						(None, None) => ui.label("Input: off"),
					};
					if let Some(e) = &s.midi_error {
						ui.colored_label(theme::ERROR, e);
					}
				}
				None => {
					ui.label("No audio device is open.");
				}
			}
			ui.add_space(6.0);
			if ui
				.checkbox(&mut test_tone, "Test tone (440 Hz, -20 dBFS)")
				.on_hover_text("Plays through the output to check the device; off by default")
				.changed()
			{
				// Applied below, outside the dialog borrow.
			}
			ui.add_space(10.0);
			ui.horizontal(|ui| {
				if ui.button("Apply").clicked() {
					apply = true;
				}
				if ui.button("OK").clicked() {
					apply = true;
					close = true;
				}
				if ui.button("Close").clicked() {
					close = true;
				}
			});
		});
	if test_tone != state.record.test_tone {
		state.set_test_tone(test_tone);
	}
	if apply {
		state.apply_audio_settings(dialog.draft.clone());
	}
	if close {
		if state.record.test_tone {
			state.set_test_tone(false);
		}
	} else {
		state.record.dialog = Some(dialog);
	}
}

/// Labels of the MIDI input quantize choices.
pub fn quantize_label(q: InputQuantize) -> &'static str {
	q.name()
}
