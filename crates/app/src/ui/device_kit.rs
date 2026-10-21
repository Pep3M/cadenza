//! The shared component kit of the device editors (design: `redesign-brief/designs/14
//! Device Kit`, `18 Interaction Spec`): knobs S/M/L/XL with automation and AI rings and a
//! floating value, typed value readouts, toggles, segmented and grid selectors, picture
//! cards, level / gain-reduction meters and the control context menu.
//!
//! Low-level widgets take plain values and return a [`Gesture`] (one commit per gesture);
//! [`param_knob`] / [`ParamKnob`] and [`param_toggle`] bind them to a built-in device
//! parameter through `AppState::{param_value, preview_param, commit_param}`.

use crate::i18n::{tr, tr_str, trf};
use cadenza_core::ParamAddress;
use cadenza_dsp::ParamInfo;
use egui::{
	Align2, Color32, CornerRadius, CursorIcon, Id, LayerId, Order, Painter, Pos2, Rect, Response,
	Sense, Shape, Stroke, StrokeKind, Ui, Vec2, pos2, vec2,
};

use crate::state::AppState;
use crate::state::ai_changes;
use crate::state::device::{self, KnobSpec, ParseError, Parsed};
use crate::state::device_display::{
	self, AdsrRanges, AdsrStage, AdsrValues, DbRange, LevelHistory, VoicePos,
};
use crate::state::meters::FLOOR_DB;
use crate::state::selection::DeviceRef;
use crate::theme;
use crate::ui::widgets::Gesture;

/// A demo transfer curve of the gallery's mode cards.
type CardCurve = fn(f32) -> f32;

/// Pixels of vertical drag for the whole range.
const DRAG_RANGE: f32 = 200.0;
/// Shift: fine adjustment.
const FINE: f32 = 0.1;
/// One wheel notch is about this many points of scroll.
pub const NOTCH: f32 = 40.0;
/// Travel per wheel notch (Shift: [`WHEEL_STEP_FINE`]).
const WHEEL_STEP: f32 = 0.01;
const WHEEL_STEP_FINE: f32 = 0.001;
/// The wheel rests this long (seconds) before its change is committed.
const WHEEL_SETTLE: f64 = 0.4;
/// Hover this long before the floating value appears.
const HOVER_DELAY: f64 = 0.3;
/// Values changed from outside (preset, undo, AI) ease over this long.
const EASE: f32 = 0.08;
/// An out-of-range or unreadable typed value flashes red this long.
const FLASH: f64 = 0.6;
/// Peak-hold ticks stay this long.
const HOLD: f64 = 1.5;
/// Meter range (dBFS).
const METER_MAX_DB: f32 = 6.0;

// Knob surfaces (Knob.dc.html).
const KNOB_TRACK: Color32 = Color32::from_rgb(0x26, 0x28, 0x2d);
const KNOB_BODY: Color32 = theme::BG_RAISED;
const KNOB_BODY_HOVER: Color32 = Color32::from_rgb(0x24, 0x26, 0x2b);
const KNOB_RIM: Color32 = theme::BG_HOVER;
const KNOB_RIM_ACTIVE: Color32 = Color32::from_rgb(0x3c, 0x3f, 0x46);
const KNOB_POINTER: Color32 = Color32::from_rgb(0xee, 0xf0, 0xf3);
const KNOB_DIM: Color32 = Color32::from_rgb(0x4a, 0x4d, 0x55);

// --- Display surface ----------------------------------------------------------------

/// Paints a display surface (curves, meters, pictures sit on it).
pub fn paint_display(p: &Painter, rect: Rect) {
	p.rect(
		rect,
		CornerRadius::same(theme::RADIUS_MD),
		theme::DISPLAY_BG,
		Stroke::new(1.0, theme::BORDER_SUBTLE),
		StrokeKind::Inside,
	);
}

/// Allocates a display of `size` and returns its rect and a painter clipped to it.
pub fn display(ui: &mut Ui, size: Vec2) -> (Response, Painter) {
	let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
	paint_display(ui.painter(), rect);
	let painter = ui.painter().with_clip_rect(rect.shrink(1.0));
	(response, painter)
}

// --- Values ---------------------------------------------------------------------------

/// How a value is shown and typed: a `ParamInfo` unit, or a list of choice names (index
/// `i` = value `min + i`).
#[derive(Debug, Clone, Copy, Default)]
pub struct ValueFmt<'a> {
	pub unit: &'a str,
	pub choices: Option<&'a [&'a str]>,
	/// From this value on the readout is this word (a drum pad's "full" decay).
	pub from: Option<(f32, &'a str)>,
}

impl<'a> ValueFmt<'a> {
	pub fn unit(unit: &'a str) -> Self {
		Self {
			unit,
			choices: None,
			from: None,
		}
	}

	pub fn choices(choices: &'a [&'a str]) -> Self {
		Self {
			unit: "",
			choices: Some(choices),
			from: None,
		}
	}

	pub fn format(&self, value: f32, min: f32) -> String {
		if let Some((at, word)) = self.from
			&& value >= at
		{
			return word.to_owned();
		}
		match self.choices {
			Some(c) => device::format_choice(value, min, c),
			None => device::format_value(value, self.unit),
		}
	}

	pub fn parse(&self, text: &str, min: f32, max: f32) -> Result<Parsed, ParseError> {
		device::parse_value(text, self.unit, min, max, self.choices)
	}
}

// --- Value readout --------------------------------------------------------------------

/// Under a knob (plain text) or in a field (boxed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadoutLook {
	Plain,
	Boxed,
}

/// What a readout is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadoutState {
	Rest,
	Hover,
	/// The control is being dragged.
	Active,
	/// The control follows automation.
	Automated,
	Typing,
	/// Typed value out of range (or unreadable).
	Error,
	/// The last AI request changed this value (blue).
	Ai,
}

/// Paints a readout's frame and text (the live widget and the kit gallery share it).
pub fn paint_readout(p: &Painter, rect: Rect, text: &str, look: ReadoutLook, state: ReadoutState) {
	let r = CornerRadius::same(theme::RADIUS);
	let font = theme::mono(if rect.height() >= 20.0 { 12.0 } else { 11.5 });
	match look {
		ReadoutLook::Plain => {
			let (bg, fg) = match state {
				ReadoutState::Rest => (Color32::TRANSPARENT, theme::TEXT),
				ReadoutState::Hover => (theme::BG_WIDGET, Color32::WHITE),
				ReadoutState::Active => (theme::ACCENT_DIM, theme::ACCENT_TEXT),
				ReadoutState::Automated => (Color32::TRANSPARENT, theme::AUTOMATION_TEXT),
				ReadoutState::Typing => (theme::DISPLAY_BG, theme::TEXT),
				ReadoutState::Error => (theme::ERROR_SOFT, theme::ERROR),
				ReadoutState::Ai => (theme::ACCENT_DIM, theme::ACCENT_TEXT),
			};
			let galley = p.layout_no_wrap(text.to_owned(), font, fg);
			let pill = Rect::from_center_size(
				rect.center(),
				vec2(
					(galley.size().x + 8.0).min(rect.width()),
					galley.size().y + 2.0,
				),
			);
			p.rect_filled(pill, r, bg);
			if state == ReadoutState::Typing {
				p.rect_stroke(pill, r, Stroke::new(1.0, theme::ACCENT), StrokeKind::Inside);
			}
			p.galley(pill.center() - galley.size() * 0.5, galley, fg);
		}
		ReadoutLook::Boxed => {
			let (bg, border, fg) = match state {
				ReadoutState::Rest | ReadoutState::Automated => {
					(theme::DISPLAY_BG, theme::BG_HOVER, theme::TEXT)
				}
				ReadoutState::Hover => (theme::DISPLAY_BG, KNOB_DIM, Color32::WHITE),
				ReadoutState::Active => {
					(theme::ACCENT_DIM, theme::ACCENT_BORDER, theme::ACCENT_TEXT)
				}
				ReadoutState::Typing => (theme::DISPLAY_BG, theme::ACCENT, theme::TEXT),
				ReadoutState::Error => (theme::ERROR_SOFT, theme::ERROR, theme::ERROR),
				ReadoutState::Ai => (theme::DISPLAY_BG, theme::ACCENT, theme::ACCENT_TEXT),
			};
			let fg = if state == ReadoutState::Automated {
				theme::AUTOMATION_TEXT
			} else {
				fg
			};
			p.rect(rect, r, bg, Stroke::new(1.0, border), StrokeKind::Inside);
			let (pos, align) = if state == ReadoutState::Typing {
				(
					pos2(rect.left() + 6.0, rect.center().y),
					Align2::LEFT_CENTER,
				)
			} else {
				(
					pos2(rect.right() - 7.0, rect.center().y),
					Align2::RIGHT_CENTER,
				)
			};
			p.text(pos, align, text, font, fg);
		}
	}
}

/// A value readout that turns into a text field on click: Enter commits (units and
/// shorthand accepted, see [`device::parse_value`]), Esc or clicking away cancels. An
/// out-of-range value flashes red, then clamps; unreadable text flashes and stays.
#[allow(clippy::too_many_arguments)]
pub fn readout(
	ui: &mut Ui,
	id: Id,
	value: f32,
	min: f32,
	max: f32,
	fmt: ValueFmt,
	look: ReadoutLook,
	size: Vec2,
	shown: ReadoutState,
) -> (Option<f32>, Response) {
	let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
	let response = ui.interact(rect, id, Sense::click());
	let typing_id = id.with("typing");
	let flash_id = id.with("flash");
	let edit_id = id.with("edit");
	let ctx = ui.ctx().clone();
	let now = ui.input(|i| i.time);
	let mut text: Option<String> = ctx.data(|d| d.get_temp(typing_id));
	let mut committed = None;
	let mut start = false;
	if text.is_none() && response.clicked() && ui.is_enabled() {
		text = Some(fmt.format(value, min));
		start = true;
	}
	let text_shown = fmt.format(value, min);
	response.widget_info(|| {
		egui::WidgetInfo::labeled(egui::WidgetType::TextEdit, ui.is_enabled(), &text_shown)
	});
	let flashing = ctx
		.data(|d| d.get_temp::<f64>(flash_id))
		.filter(|t| now - t < FLASH);
	if let Some(t) = flashing {
		ctx.request_repaint_after(std::time::Duration::from_secs_f64(FLASH - (now - t)));
	}
	match text {
		Some(mut t) => {
			let state = if flashing.is_some() {
				ReadoutState::Error
			} else {
				ReadoutState::Typing
			};
			let frame = rect;
			paint_readout(ui.painter(), frame, "", ReadoutLook::Boxed, state);
			let font = theme::mono(if rect.height() >= 20.0 { 12.0 } else { 11.5 });
			let inner = frame.shrink2(vec2(5.0, 1.0));
			let mut child = ui.new_child(egui::UiBuilder::new().max_rect(inner));
			let mut output = egui::TextEdit::singleline(&mut t)
				.id(edit_id)
				.font(font)
				.frame(egui::Frame::NONE)
				.margin(egui::Margin::ZERO)
				.vertical_align(egui::Align::Center)
				.desired_width(inner.width())
				.text_color(if state == ReadoutState::Error {
					theme::ERROR
				} else {
					theme::TEXT
				})
				.show(&mut child);
			if start {
				output.response.request_focus();
				let end = egui::text::CCursor::new(t.chars().count());
				output
					.state
					.cursor
					.set_char_range(Some(egui::text::CCursorRange::two(
						egui::text::CCursor::new(0),
						end,
					)));
				output.state.store(&ctx, edit_id);
			}
			let (enter, escape) = ui.input(|i| {
				(
					i.key_pressed(egui::Key::Enter),
					i.key_pressed(egui::Key::Escape),
				)
			});
			if output.response.lost_focus() && enter {
				match fmt.parse(&t, min, max) {
					Ok(parsed) => {
						if parsed.out_of_range {
							ctx.data_mut(|d| d.insert_temp(flash_id, now));
						}
						committed = Some(parsed.value);
						ctx.data_mut(|d| d.remove_temp::<String>(typing_id));
					}
					Err(_) => {
						ctx.data_mut(|d| {
							d.insert_temp(flash_id, now);
							d.insert_temp(typing_id, t);
						});
						output.response.request_focus();
					}
				}
			} else if escape || (!start && !output.response.has_focus()) {
				ctx.data_mut(|d| d.remove_temp::<String>(typing_id));
			} else {
				ctx.data_mut(|d| d.insert_temp(typing_id, t));
			}
		}
		None => {
			// Values changed from outside (preset, A/B, undo, AI) ease; never while the
			// user drags, types or the value follows automation.
			let instant = matches!(shown, ReadoutState::Active | ReadoutState::Automated)
				|| fmt.choices.is_some()
				|| fmt.from.is_some();
			let eased = ctx.animate_value_with_time(
				id.with("ease"),
				value,
				if instant { 0.0 } else { EASE },
			);
			let text_shown = if (eased - value).abs() > f32::EPSILON {
				fmt.format(eased, min)
			} else {
				text_shown
			};
			if ui.is_rect_visible(rect) {
				let state = if flashing.is_some() {
					ReadoutState::Error
				} else if response.hovered() && shown == ReadoutState::Rest {
					ReadoutState::Hover
				} else {
					shown
				};
				paint_readout(ui.painter(), rect, &text_shown, look, state);
			}
		}
	}
	let response = response.on_hover_cursor(CursorIcon::Text);
	(committed, response)
}

// --- Knob -----------------------------------------------------------------------------

/// Knob sizes: S for compact views and secondary parameters, M and L for main ones, XL
/// for the one control that defines the device (Drive, Input gain).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KnobSize {
	/// 40 px, 2.5 px arc.
	S,
	/// 56 px, 3 px arc.
	M,
	/// 72 px, 3 px arc.
	L,
	/// 96 px, 4 px arc.
	XL,
}

impl KnobSize {
	pub fn diameter(self) -> f32 {
		match self {
			KnobSize::S => 40.0,
			KnobSize::M => 56.0,
			KnobSize::L => 72.0,
			KnobSize::XL => 96.0,
		}
	}

	pub fn stroke(self) -> f32 {
		match self {
			KnobSize::S => 2.5,
			KnobSize::M | KnobSize::L => 3.0,
			KnobSize::XL => 4.0,
		}
	}

	fn big(self) -> bool {
		self != KnobSize::S
	}
}

/// Wheel steps not committed yet.
#[derive(Debug, Clone, Copy, Default)]
struct Wheel {
	/// Travel position when the wheel started.
	start: f32,
	/// Notches turned since (fractional with smooth scrolling).
	notches: f32,
	/// Input time of the last movement.
	last: f64,
}

impl Wheel {
	fn value(&self, spec: &KnobSpec, step: Option<f32>) -> f32 {
		if let Some(step) = step.filter(|s| *s > 0.0) {
			// Plain-unit steps (0.5 dB): the notch count snaps to whole steps.
			let v = spec.from_pos(self.start) + self.notches * step;
			((v / step).round() * step).clamp(spec.min, spec.max)
		} else if spec.integer {
			// Whole-number knobs move one value per notch.
			(spec.from_pos(self.start) + self.notches.round()).clamp(spec.min, spec.max)
		} else {
			spec.from_pos(self.start + self.notches * WHEEL_STEP)
		}
	}
}

/// A rotary knob with its label and typed readout under it. Drag vertically (200 px =
/// the range, Shift ×0.1), wheel = 1/100 of the range (Shift 1/1000, committed once it
/// rests), double-click = default, click the readout to type. Hovering shows the value
/// above the knob after 300 ms.
#[must_use = "call `show`"]
pub struct Knob<'a> {
	id: Id,
	value: f32,
	spec: KnobSpec,
	size: KnobSize,
	label: &'a str,
	fmt: ValueFmt<'a>,
	automation: Option<f32>,
	ai: bool,
	enabled: bool,
	bipolar: Option<bool>,
	readout: bool,
	color: Color32,
	wheel_step: Option<f32>,
	description: &'a str,
	modulation: Option<ModRingPaint>,
}

/// A modulated knob's ring, in plain values: the range the modulators sweep and where
/// the value is now.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModRingPaint {
	pub lo: f32,
	pub hi: f32,
	/// The live modulated value (the moving dot), while the device plays.
	pub now: Option<f32>,
	/// A modulator is dragged over the knob, or the ring is being dragged.
	pub hot: bool,
}

impl<'a> Knob<'a> {
	pub fn new(id: Id, value: f32, spec: KnobSpec) -> Self {
		Self {
			id,
			value,
			spec,
			size: KnobSize::M,
			label: "",
			fmt: ValueFmt::default(),
			automation: None,
			ai: false,
			enabled: true,
			bipolar: None,
			readout: true,
			color: theme::TEXT,
			wheel_step: None,
			description: "",
			modulation: None,
		}
	}

	/// Draws the modulation ring (teal) and makes it draggable: see [`Knob::show_ring`].
	pub fn modulation(mut self, ring: Option<ModRingPaint>) -> Self {
		self.modulation = ring;
		self
	}

	pub fn size(mut self, size: KnobSize) -> Self {
		self.size = size;
		self
	}

	pub fn label(mut self, label: &'a str) -> Self {
		self.label = tr_str(label);
		self
	}

	/// What the control does, under the value in the hover readout.
	pub fn description(mut self, description: &'a str) -> Self {
		self.description = description;
		self
	}

	/// One wheel notch moves the value by `step` plain units (Shift: a tenth), instead
	/// of 1/100 of the travel (Shift: ten notches per step).
	pub fn wheel_step(mut self, step: Option<f32>) -> Self {
		self.wheel_step = step;
		self
	}

	pub fn unit(mut self, unit: &'a str) -> Self {
		self.fmt.unit = unit;
		self
	}

	pub fn choices(mut self, choices: Option<&'a [&'a str]>) -> Self {
		self.fmt.choices = choices;
		self
	}

	/// From `at` on the readout says `word` instead of a value.
	pub fn word_from(mut self, at: f32, word: &'a str) -> Self {
		self.fmt.from = Some((at, word));
		self
	}

	/// The control follows automation; `playing` is the value now playing (the ring's
	/// dot), the pointer shows the written value.
	pub fn automation(mut self, playing: Option<f32>) -> Self {
		self.automation = playing;
		self
	}

	/// The AI changed this value (blue ring).
	pub fn ai(mut self, changed: bool) -> Self {
		self.ai = changed;
		self
	}

	pub fn enabled(mut self, enabled: bool) -> Self {
		self.enabled = enabled;
		self
	}

	/// Fill the arc from zero (default: when the range spans zero).
	pub fn bipolar(mut self, bipolar: bool) -> Self {
		self.bipolar = Some(bipolar);
		self
	}

	/// Hide the value readout (the floating value still shows).
	pub fn no_readout(mut self) -> Self {
		self.readout = false;
		self
	}

	/// Arc colour (default: text white; device editors may use their accent).
	pub fn color(mut self, color: Color32) -> Self {
		self.color = color;
		self
	}

	/// Returns the gesture and the dial's response (attach the context menu to it).
	pub fn show(self, ui: &mut Ui) -> (Gesture, Response) {
		let (g, r, _) = self.show_ring(ui);
		(g, r)
	}

	/// [`Knob::show`] plus, for a knob with [`Knob::modulation`], the response of the
	/// ring around the dial (drag = depth, Alt+click = remove; the caller acts on it).
	pub fn show_ring(self, ui: &mut Ui) -> (Gesture, Response, Option<Response>) {
		let Knob {
			id,
			value,
			spec,
			size,
			label,
			fmt,
			automation,
			ai,
			enabled,
			bipolar,
			readout: with_readout,
			color,
			wheel_step,
			description,
			modulation,
		} = self;
		let d = size.diameter();
		let big = size.big();
		let label_h = if label.is_empty() {
			0.0
		} else if big {
			16.0
		} else {
			15.0
		};
		let readout_h = if with_readout { 18.0 } else { 0.0 };
		let width = (d + 8.0).max(56.0);
		let (rect, _) = ui.allocate_exact_size(
			vec2(width, 4.0 + d + 4.0 + label_h + readout_h),
			Sense::hover(),
		);
		let dial = Rect::from_center_size(
			pos2(rect.center().x, rect.top() + 4.0 + d * 0.5),
			Vec2::splat(d),
		);
		let sense = if enabled {
			Sense::click_and_drag()
		} else {
			Sense::hover()
		};
		// The ring band around the dial: registered first, so the dial (on top) keeps
		// the pointer over its own square.
		let ring_response = modulation.filter(|_| enabled).map(|_| {
			ui.interact(
				dial.expand(7.0),
				id.with("mod-ring"),
				Sense::click_and_drag(),
			)
			.on_hover_cursor(CursorIcon::ResizeVertical)
		});
		let response = ui.interact(dial, id, sense);
		response.widget_info(|| egui::WidgetInfo::slider(enabled, f64::from(value), label));
		let response = if enabled {
			response.on_hover_cursor(CursorIcon::ResizeVertical)
		} else {
			response
		};
		let ctx = ui.ctx().clone();
		let (now, shift, scroll) = ui.input(|i| {
			(
				i.time,
				i.modifiers.shift,
				i.smooth_scroll_delta.x + i.smooth_scroll_delta.y,
			)
		});

		// Drag: relative, on the travel, clamped so turning back responds at once.
		let drag_id = id.with("drag");
		let mut gesture = Gesture::Idle;
		if response.drag_started() {
			ctx.data_mut(|d| d.insert_temp(drag_id, spec.to_pos(value)));
		}
		if response.dragged() {
			let fine = if shift { FINE } else { 1.0 };
			let start = ctx
				.data(|d| d.get_temp::<f32>(drag_id))
				.unwrap_or(spec.to_pos(value));
			let pos = (start - response.drag_delta().y / DRAG_RANGE * fine).clamp(0.0, 1.0);
			ctx.data_mut(|d| d.insert_temp(drag_id, pos));
			gesture = Gesture::Preview(spec.from_pos(pos));
		}
		if response.drag_stopped() {
			let pos = ctx
				.data_mut(|d| d.remove_temp::<f32>(drag_id))
				.unwrap_or(spec.to_pos(value));
			gesture = Gesture::Commit(spec.from_pos(pos));
		}
		if response.double_clicked() {
			gesture = Gesture::Commit(spec.default);
		}

		// Wheel: steps preview at once and commit as one gesture once the wheel rests.
		let wheel_id = id.with("wheel");
		let mut wheel: Option<Wheel> = ctx.data(|d| d.get_temp(wheel_id));
		if enabled && response.hovered() && scroll != 0.0 && !response.dragged() {
			ui.input_mut(|i| i.smooth_scroll_delta = Vec2::ZERO);
			let mut w = wheel.unwrap_or(Wheel {
				start: spec.to_pos(value),
				notches: 0.0,
				last: now,
			});
			// Shift steps finer: folded into the notch count so a change of modifier
			// mid-turn keeps the position.
			w.notches += scroll / NOTCH
				* if shift {
					WHEEL_STEP_FINE / WHEEL_STEP
				} else {
					1.0
				};
			w.last = now;
			wheel = Some(w);
			ctx.data_mut(|d| d.insert_temp(wheel_id, w));
			gesture = Gesture::Preview(w.value(&spec, wheel_step));
			ctx.request_repaint_after(std::time::Duration::from_secs_f64(WHEEL_SETTLE + 0.05));
		} else if let Some(w) = wheel
			&& (now - w.last > WHEEL_SETTLE || response.drag_started())
		{
			ctx.data_mut(|d| d.remove_temp::<Wheel>(wheel_id));
			wheel = None;
			if gesture == Gesture::Idle {
				gesture = Gesture::Commit(w.value(&spec, wheel_step));
			}
		}

		// Typed value under the knob.
		let label_rect =
			Rect::from_min_size(pos2(rect.left(), dial.bottom() + 4.0), vec2(width, label_h));
		let readout_rect = Rect::from_min_size(
			pos2(rect.left(), label_rect.bottom()),
			vec2(width, readout_h),
		);
		let target = match gesture {
			Gesture::Preview(v) | Gesture::Commit(v) => v,
			Gesture::Idle => wheel.map_or(value, |w| w.value(&spec, wheel_step)),
		};
		let active = response.dragged() || wheel.is_some();
		if with_readout {
			let shown = if active {
				ReadoutState::Active
			} else if automation.is_some() {
				ReadoutState::Automated
			} else if response.hovered() {
				ReadoutState::Hover
			} else {
				ReadoutState::Rest
			};
			let mut child = ui.new_child(egui::UiBuilder::new().max_rect(readout_rect));
			child.add_enabled_ui(enabled, |ui| {
				let (typed, _) = readout(
					ui,
					id.with("readout"),
					target,
					spec.min,
					spec.max,
					fmt,
					ReadoutLook::Plain,
					readout_rect.size(),
					shown,
				);
				if let Some(v) = typed {
					gesture = Gesture::Commit(if spec.integer { v.round() } else { v });
				}
			});
		}

		// Floating value: after a hover delay, and always while the value moves.
		let tip = active || hover_waited(&ctx, id.with("hover"), response.hovered());

		if ui.is_rect_visible(rect) {
			// External changes ease; the user's own gestures and automation never lag.
			let instant = active || gesture != Gesture::Idle || automation.is_some();
			let pos = ctx.animate_value_with_time(
				id.with("ease"),
				spec.to_pos(target),
				if instant { 0.0 } else { EASE },
			);
			let mut p = ui.painter().clone();
			if !enabled {
				p.multiply_opacity(0.4);
			}
			let look = if active {
				DialLook::Drag
			} else if response.hovered() {
				DialLook::Hover
			} else {
				DialLook::Rest
			};
			paint_dial(
				&p,
				dial,
				size,
				DialPaint {
					pos,
					origin: if bipolar.unwrap_or(spec.bipolar()) {
						spec.to_pos(0.0)
					} else {
						0.0
					},
					color: if enabled { color } else { KNOB_DIM },
					automation: automation.map(|v| spec.to_pos(v)),
					ai,
					look,
					modulation: modulation.map(|m| ModArc {
						lo: spec.to_pos(m.lo),
						hi: spec.to_pos(m.hi),
						now: m.now.map(|v| spec.to_pos(v)),
						hot: m.hot
							|| ring_response
								.as_ref()
								.is_some_and(|r| r.hovered() || r.dragged()),
					}),
				},
			);
			if !label.is_empty() {
				p.text(
					label_rect.center(),
					Align2::CENTER_CENTER,
					label,
					egui::FontId::proportional(if big { 12.0 } else { 11.0 }),
					theme::TEXT_DIM,
				);
			}
			if tip && enabled {
				let about = if active { "" } else { description };
				floating_tip(&ctx, id, dial, &fmt.format(target, spec.min), about);
			}
		}
		(gesture, response, ring_response)
	}
}

/// The modulation ring of [`DialPaint`], as travel positions.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModArc {
	pub lo: f32,
	pub hi: f32,
	pub now: Option<f32>,
	pub hot: bool,
}

/// How the dial body is lit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialLook {
	Rest,
	Hover,
	Drag,
}

/// What [`paint_dial`] draws, as travel positions (0..1).
#[derive(Debug, Clone, Copy)]
pub struct DialPaint {
	pub pos: f32,
	/// Where the value arc starts (0.5 = 12 o'clock for a symmetric bipolar range).
	pub origin: f32,
	pub color: Color32,
	/// Automation ring with its dot at this position.
	pub automation: Option<f32>,
	/// AI-changed ring.
	pub ai: bool,
	pub look: DialLook,
	/// Modulation ring (outermost).
	pub modulation: Option<ModArc>,
}

/// Point at travel position `pos` (0..1 over the 270° sweep, 0 = 7:30, 1 = 4:30).
fn dial_point(center: Pos2, radius: f32, pos: f32) -> Pos2 {
	let a = (-135.0 + 270.0 * pos).to_radians();
	center + vec2(a.sin(), -a.cos()) * radius
}

fn arc(p: &Painter, center: Pos2, radius: f32, from: f32, to: f32, stroke: Stroke) {
	if to - from <= 1e-4 {
		return;
	}
	let steps = ((to - from) * 64.0).ceil().max(2.0) as usize;
	let pts: Vec<Pos2> = (0..=steps)
		.map(|i| dial_point(center, radius, from + (to - from) * i as f32 / steps as f32))
		.collect();
	// Round caps.
	p.circle_filled(pts[0], stroke.width * 0.5, stroke.color);
	p.circle_filled(pts[steps], stroke.width * 0.5, stroke.color);
	p.add(Shape::line(pts, stroke));
}

/// Paints a knob dial into the square `rect` (the [`Knob`] widget uses it; device
/// displays may too).
pub fn paint_dial(p: &Painter, rect: Rect, size: KnobSize, d: DialPaint) {
	let c = rect.center();
	let sw = size.stroke();
	let rr = rect.width() * 0.5 - sw * 0.5 - 1.0;
	let br = rr - sw - 1.5;
	let ring = rr + sw * 0.5 + 3.5;
	if d.automation.is_some() {
		p.circle_stroke(c, ring, Stroke::new(1.25, theme::AUTOMATION));
	}
	if d.ai {
		let r = if d.automation.is_some() {
			ring + 2.5
		} else {
			ring
		};
		p.circle_stroke(c, r, Stroke::new(2.0, theme::ACCENT));
	}
	if let Some(m) = d.modulation {
		let r =
			ring + 2.5 * (f32::from(u8::from(d.automation.is_some())) + f32::from(u8::from(d.ai)));
		let width = if m.hot { 3.0 } else { 2.0 };
		arc(p, c, r, 0.0, 1.0, Stroke::new(1.0, theme::MOD_SOFT));
		let (lo, hi) = (m.lo.clamp(0.0, 1.0), m.hi.clamp(0.0, 1.0));
		if hi - lo > 1e-3 {
			arc(p, c, r, lo, hi, Stroke::new(width, theme::MOD));
		} else {
			p.circle_filled(dial_point(c, r, lo), width, theme::MOD);
		}
		if let Some(now) = m.now {
			p.circle(
				dial_point(c, r, now.clamp(0.0, 1.0)),
				3.0,
				theme::MOD_TEXT,
				Stroke::new(1.5, theme::DISPLAY_BG),
			);
		}
	}
	arc(p, c, rr, 0.0, 1.0, Stroke::new(sw, KNOB_TRACK));
	let pos = d.pos.clamp(0.0, 1.0);
	arc(
		p,
		c,
		rr,
		d.origin.min(pos),
		d.origin.max(pos),
		Stroke::new(sw, d.color),
	);
	let (body, rim) = match d.look {
		DialLook::Rest => (KNOB_BODY, KNOB_RIM),
		DialLook::Hover => (KNOB_BODY_HOVER, KNOB_RIM_ACTIVE),
		DialLook::Drag => (theme::BG_HOVER, KNOB_RIM_ACTIVE),
	};
	p.circle(c, br, body, Stroke::new(1.0, rim));
	let pw = if size.big() { 2.5 } else { 2.0 };
	let a = dial_point(c, (rr - sw) * 0.25, pos);
	let b = dial_point(c, rr - sw - 2.0, pos);
	p.line_segment([a, b], Stroke::new(pw, KNOB_POINTER));
	p.circle_filled(a, pw * 0.5, KNOB_POINTER);
	p.circle_filled(b, pw * 0.5, KNOB_POINTER);
	if let Some(at) = d.automation {
		let dot = dial_point(c, rr, at.clamp(0.0, 1.0));
		p.circle(
			dot,
			3.2,
			theme::AUTOMATION,
			Stroke::new(1.5, theme::DISPLAY_BG),
		);
	}
}

/// Whether the pointer has rested on a control for [`HOVER_DELAY`] (asks for the repaint
/// that shows it); leaving resets the wait.
pub fn hover_waited(ctx: &egui::Context, id: Id, hovered: bool) -> bool {
	let now = ctx.input(|i| i.time);
	if !hovered || ctx.input(|i| i.pointer.any_down()) || egui::Popup::is_any_open(ctx) {
		ctx.data_mut(|d| d.remove_temp::<f64>(id));
		return false;
	}
	let since = ctx.data_mut(|d| *d.get_temp_mut_or_insert_with(id, || now));
	let waited = now - since;
	if waited < HOVER_DELAY {
		ctx.request_repaint_after(std::time::Duration::from_secs_f64(HOVER_DELAY - waited));
	}
	waited >= HOVER_DELAY
}

/// The value floating 2 px above `anchor`, never under the pointer.
pub fn floating_value(ctx: &egui::Context, id: Id, anchor: Rect, text: &str) {
	floating_tip(ctx, id, anchor, text, "");
}

/// Width the hover readout wraps a parameter description at.
const TIP_WRAP: f32 = 240.0;

/// [`floating_value`] with what the control does under the value (the hover readout:
/// value + unit, then the parameter's description).
pub fn floating_tip(ctx: &egui::Context, id: Id, anchor: Rect, text: &str, description: &str) {
	let p = ctx.layer_painter(LayerId::new(Order::Tooltip, id.with("floating-value")));
	let galley = p.layout_no_wrap(text.to_owned(), theme::mono(11.5), theme::TEXT);
	let about = (!description.is_empty()).then(|| {
		p.layout(
			description.to_owned(),
			theme::small(),
			theme::TEXT_DIM,
			TIP_WRAP,
		)
	});
	let about_h = about.as_ref().map_or(0.0, |g| g.size().y + 3.0);
	let inner_w = about
		.as_ref()
		.map_or(galley.size().x, |g| g.size().x.max(galley.size().x));
	let size = vec2(inner_w, galley.size().y + about_h) + vec2(14.0, 6.0);
	let mut r = Rect::from_center_size(
		pos2(anchor.center().x, anchor.top() - 2.0 - size.y * 0.5),
		size,
	);
	if let Some(pointer) = ctx.pointer_latest_pos()
		&& r.expand(2.0).contains(pointer)
	{
		r = r.translate(vec2(0.0, pointer.y - 6.0 - r.bottom()));
	}
	p.rect_filled(
		r.translate(vec2(0.0, 4.0)).expand(3.0),
		CornerRadius::same(theme::RADIUS_LG),
		Color32::from_black_alpha(70),
	);
	p.rect(
		r,
		CornerRadius::same(theme::RADIUS_MD),
		theme::BG_HOVER,
		Stroke::new(1.0, theme::BORDER_STRONG),
		StrokeKind::Inside,
	);
	match about {
		Some(about) => {
			let top = r.top() + 3.0;
			p.galley(
				pos2(r.center().x - galley.size().x * 0.5, top),
				galley.clone(),
				theme::TEXT,
			);
			p.galley(
				pos2(r.left() + 7.0, top + galley.size().y + 3.0),
				about,
				theme::TEXT_DIM,
			);
		}
		None => p.galley(r.center() - galley.size() * 0.5, galley, theme::TEXT),
	}
}

// --- Context menu ---------------------------------------------------------------------

fn clipboard_id() -> Id {
	Id::new("device-kit-clipboard")
}

/// Range, default and display of a control, for its context menu.
#[derive(Debug, Clone, Copy)]
pub struct ControlInfo<'a> {
	pub value: f32,
	pub min: f32,
	pub max: f32,
	pub default: f32,
	pub fmt: ValueFmt<'a>,
	/// Show the hover readout (value + this description) after [`HOVER_DELAY`]; `None`
	/// when the control draws its own (knobs).
	pub tip: Option<&'a str>,
}

/// The pointer is over `response` and no other layer covers it there (overlapping
/// widgets of the same layer, like a readout's drag area, do not count).
fn pointer_on(response: &Response) -> bool {
	response.contains_pointer()
		&& response
			.ctx
			.pointer_latest_pos()
			.is_some_and(|p| response.ctx.layer_id_at(p) == Some(response.layer_id))
}

/// The kit control of `device` the user touched last (the ↑ ↓ keys nudge it).
fn touched_id(device: cadenza_core::DeviceId) -> Id {
	Id::new(("device-kit-touched", device))
}

/// The param of `device` touched last through a kit control.
pub fn touched(ctx: &egui::Context, device: cadenza_core::DeviceId) -> Option<String> {
	ctx.data(|d| d.get_temp::<String>(touched_id(device)))
}

/// Right-click menu of a control: Automate (creates and shows the lane), Reset, Copy
/// value, Paste value (an app-wide one-value clipboard) and, on a device param, "Ask AI
/// about this" (the editor's Ask AI popover, prefilled with the param and its value).
/// Also shows the hover readout ([`ControlInfo::tip`]) and remembers the device param
/// the user touches for the editor's ↑ ↓ keys. Returns the value to commit (reset /
/// paste).
pub fn control_menu(
	response: &Response,
	state: &mut AppState,
	address: ParamAddress,
	info: ControlInfo,
) -> Option<f32> {
	let mut out = None;
	let ctx = response.ctx.clone();
	let device_param = match &address.target {
		cadenza_core::ParamTarget::Device { device, param } => Some((
			DeviceRef {
				track: address.track,
				device: *device,
			},
			param.clone(),
		)),
		_ => None,
	};
	if let Some((r, param)) = &device_param {
		// Readouts may sit under a drag area of their own: the pointer, not the hover.
		let touched = pointer_on(response)
			&& ctx.input(|i| i.pointer.any_pressed() || i.smooth_scroll_delta != Vec2::ZERO);
		if touched {
			ctx.data_mut(|d| d.insert_temp(touched_id(r.device), param.clone()));
		}
	}
	if let Some(about) = info.tip
		&& hover_waited(&ctx, response.id.with("tip"), pointer_on(response))
	{
		let text = info.fmt.format(info.value, info.min);
		floating_tip(&ctx, response.id, response.rect, &text, about);
	}
	crate::ui::midi_learn::decorate(response, state, &address);
	response.context_menu(|ui| {
		crate::ui::automation::automate_item(ui, state, address.clone());
		if let Some((r, param)) = &device_param
			&& state.device(*r).is_some_and(|d| d.builtin_id().is_some())
			&& ui.button(tr("Ask AI about this")).clicked()
		{
			let name = param_info(state, *r, param).map_or(param.as_str(), |p| p.name);
			let value = info.fmt.format(info.value, info.min);
			let text = trf(
				"{param} (now {value}): ",
				&[("param", &crate::i18n::tr_str(name)), ("value", &value)],
			);
			crate::ui::device_ai::ask_about(ui.ctx(), state, *r, text);
			ui.close();
		}
		if let Some((r, param)) = &device_param
			&& param_info(state, *r, param).is_some()
		{
			crate::ui::devices::modulation::modulate_menu(ui, state, *r, param);
		}
		if let Some((r, _)) = &device_param
			&& state.device(*r).is_some_and(|d| d.builtin_id().is_some())
			&& ui
				.add_enabled(
					!state.ai.busy(),
					egui::Button::new(tr("Explain this device")),
				)
				.clicked()
		{
			state.explain_device(*r);
			ui.close();
		}
		if ui.button(tr("Reset")).clicked() {
			out = Some(info.default);
			ui.close();
		}
		ui.separator();
		let text = info.fmt.format(info.value, info.min);
		if ui
			.button(trf("Copy value  {text}", &[("text", &text)]))
			.clicked()
		{
			ui.ctx()
				.data_mut(|d| d.insert_temp(clipboard_id(), info.value));
			ui.ctx().copy_text(text);
			ui.close();
		}
		let clip = ui.ctx().data(|d| d.get_temp::<f32>(clipboard_id()));
		let paste = match clip {
			Some(v) => trf(
				"Paste value  {format}",
				&[(
					"format",
					&info.fmt.format(v.clamp(info.min, info.max), info.min),
				)],
			),
			None => tr("Paste value").to_owned(),
		};
		if ui
			.add_enabled(clip.is_some(), egui::Button::new(paste))
			.clicked()
		{
			out = clip.map(|v| v.clamp(info.min, info.max));
			ui.close();
		}
	});
	out
}

// --- Parameter bindings ---------------------------------------------------------------

/// The registry entry of a built-in device's parameter.
pub fn param_info(state: &AppState, r: DeviceRef, param: &str) -> Option<&'static ParamInfo> {
	let device = state.project().device(r.track, r.device)?;
	cadenza_dsp::device_info(device.builtin_id()?)?
		.params
		.iter()
		.find(|p| p.id == param)
}

/// A knob bound to a built-in device parameter: shows the value at the playhead
/// (automation, live editor, preview), previews while dragging, commits one command
/// per gesture, and carries the control context menu.
#[must_use = "call `show`"]
pub struct ParamKnob<'a> {
	r: DeviceRef,
	param: &'a str,
	size: KnobSize,
	label: Option<&'a str>,
	choices: Option<&'a [&'a str]>,
	ai: Option<bool>,
	color: Color32,
	salt: Option<Id>,
	wheel_step: Option<f32>,
}

impl<'a> ParamKnob<'a> {
	pub fn new(r: DeviceRef, param: &'a str) -> Self {
		Self {
			r,
			param,
			size: KnobSize::M,
			label: None,
			choices: None,
			ai: None,
			color: theme::TEXT,
			salt: None,
			wheel_step: None,
		}
	}

	/// See [`Knob::wheel_step`].
	pub fn wheel_step(mut self, step: f32) -> Self {
		self.wheel_step = Some(step);
		self
	}

	/// Tells this knob apart from another one bound to the same parameter (the same
	/// device shown in the dock and the inspector).
	pub fn id_salt(mut self, salt: Id) -> Self {
		self.salt = Some(salt);
		self
	}

	pub fn size(mut self, size: KnobSize) -> Self {
		self.size = size;
		self
	}

	/// Label instead of the parameter name.
	pub fn label(mut self, label: &'a str) -> Self {
		self.label = Some(label);
		self
	}

	pub fn choices(mut self, choices: &'a [&'a str]) -> Self {
		self.choices = Some(choices);
		self
	}

	/// Overrides the AI ring (default: whether the last AI request moved this param,
	/// with its delta tag).
	pub fn ai(mut self, changed: bool) -> Self {
		self.ai = Some(changed);
		self
	}

	pub fn color(mut self, color: Color32) -> Self {
		self.color = color;
		self
	}

	pub fn show(self, ui: &mut Ui, state: &mut AppState) -> Response {
		let r = self.r;
		let Some(info) = param_info(state, r, self.param) else {
			return ui.allocate_response(Vec2::ZERO, Sense::hover());
		};
		let spec = KnobSpec::for_param(info.min, info.max, info.default, info.unit);
		let address = ParamAddress::device(r.track, r.device, info.id);
		let value = state
			.param_value(r.track, r.device, info.id)
			.unwrap_or(info.default);
		let playing = state.automated(&address);
		// Bipolar pan reads "C"/"L25"/"R25" like the mixer, not "0 %"/"-25 %".
		let unit = if info.id == "pan" { "pan%" } else { info.unit };
		let fmt = ValueFmt {
			unit,
			choices: self.choices,
			from: None,
		};
		let id = Id::new(("device-kit-knob", r.device, info.id));
		let id = self.salt.map_or(id, |s| id.with(s));
		let change = state.ai_change(r.device, info.id).cloned();
		let ai = self.ai.unwrap_or(change.is_some());
		let rect_id = id.with("last-rect");
		let last_rect = ui.ctx().data(|d| d.get_temp::<Rect>(rect_id));
		let ring = crate::ui::devices::modulation::knob_ring(
			ui.ctx(),
			state,
			r,
			info,
			playing.unwrap_or(value),
			last_rect,
		);
		let (gesture, response, ring_response) = Knob::new(id, value, spec)
			.size(self.size)
			.label(self.label.unwrap_or(info.name))
			.unit(unit)
			.choices(self.choices)
			.automation(playing)
			.ai(ai)
			.color(self.color)
			.wheel_step(self.wheel_step)
			.description(info.description)
			.modulation(ring)
			.show_ring(ui);
		ui.ctx()
			.data_mut(|d| d.insert_temp(rect_id, response.rect.expand(7.0)));
		crate::ui::devices::modulation::knob_gestures(
			state,
			r,
			info.id,
			&response,
			ring_response.as_ref(),
		);
		if ai && let Some(c) = &change {
			let text = match self.choices {
				Some(choices) => format!("→ {}", device::format_choice(c.after, info.min, choices)),
				None => ai_changes::delta_text(c.before, c.after, info.unit),
			};
			ai_delta_tag(ui, response.rect, &text);
		}
		let menu = control_menu(
			&response,
			state,
			address,
			ControlInfo {
				value,
				min: info.min,
				max: info.max,
				default: info.default,
				fmt,
				tip: None,
			},
		);
		match (menu, gesture) {
			(Some(v), _) | (None, Gesture::Commit(v)) => {
				state.commit_param(r.track, r.device, info.id, v)
			}
			(None, Gesture::Preview(v)) => state.preview_param(r.device, info.id, v),
			(None, Gesture::Idle) => {}
		}
		response
	}
}

/// The small blue "+3.0 dB" tag over the top-right of an AI-changed control.
pub fn ai_delta_tag(ui: &Ui, anchor: Rect, text: &str) {
	let p = ui.painter();
	let galley = p.layout_no_wrap(text.to_owned(), theme::mono_medium(9.5), theme::ACCENT_TEXT);
	let size = galley.size() + vec2(8.0, 2.0);
	let r = Rect::from_min_size(
		pos2(
			anchor.right() - size.x * 0.5,
			anchor.top() - size.y * 0.5 - 2.0,
		),
		size,
	);
	p.rect(
		r,
		CornerRadius::same(theme::RADIUS),
		theme::ACCENT_DIM,
		Stroke::new(1.0, theme::ACCENT_BORDER),
		StrokeKind::Inside,
	);
	p.galley(r.center() - galley.size() * 0.5, galley, theme::ACCENT_TEXT);
}

/// [`ParamKnob`] with the parameter's own name.
pub fn param_knob(
	ui: &mut Ui,
	state: &mut AppState,
	r: DeviceRef,
	param: &str,
	size: KnobSize,
) -> Response {
	ParamKnob::new(r, param).size(size).show(ui, state)
}

/// A toggle bound to a built-in 0/1 parameter (one command per click).
pub fn param_toggle(
	ui: &mut Ui,
	state: &mut AppState,
	r: DeviceRef,
	param: &str,
	label: &str,
	color: Color32,
) -> Response {
	param_toggle_with(ui, state, r, param, label, color, None)
}

/// [`param_toggle`] with a [`Glyph`] before the label (e.g. the reverb's Freeze).
pub fn param_glyph_toggle(
	ui: &mut Ui,
	state: &mut AppState,
	r: DeviceRef,
	param: &str,
	glyph: Glyph,
	label: &str,
	color: Color32,
) -> Response {
	param_toggle_with(ui, state, r, param, label, color, Some(glyph))
}

fn param_toggle_with(
	ui: &mut Ui,
	state: &mut AppState,
	r: DeviceRef,
	param: &str,
	label: &str,
	color: Color32,
	glyph: Option<Glyph>,
) -> Response {
	let Some(info) = param_info(state, r, param) else {
		return ui.allocate_response(Vec2::ZERO, Sense::hover());
	};
	let value = state
		.param_value(r.track, r.device, info.id)
		.unwrap_or(info.default);
	let on = value >= 0.5;
	let response = match glyph {
		Some(g) => glyph_toggle(ui, on, g, label, color),
		None => toggle(ui, on, label, color),
	};
	if state.ai_change(r.device, info.id).is_some() {
		ui.painter().rect_stroke(
			response.rect.expand(2.5),
			CornerRadius::same(theme::RADIUS_MD + 2),
			Stroke::new(1.5, theme::ACCENT),
			StrokeKind::Outside,
		);
	}
	let address = ParamAddress::device(r.track, r.device, info.id);
	let menu = control_menu(
		&response,
		state,
		address,
		ControlInfo {
			value,
			min: info.min,
			max: info.max,
			default: info.default,
			fmt: ValueFmt::unit(info.unit),
			tip: Some(info.description),
		},
	);
	if let Some(v) = menu {
		state.commit_param(r.track, r.device, info.id, v);
	} else if response.clicked() {
		state.commit_param(
			r.track,
			r.device,
			info.id,
			if on { info.min } else { info.max },
		);
	}
	response
}

// --- Toggle and selectors -------------------------------------------------------------

/// A toggle sized to its label: on = a tinted fill and a 1 px ring in its function
/// colour (teal for stereo, amber for phase and warnings).
pub fn toggle(ui: &mut Ui, on: bool, label: &str, color: Color32) -> Response {
	let font = theme::medium(12.0);
	let galley = ui
		.painter()
		.layout_no_wrap(label.to_owned(), font.clone(), theme::TEXT);
	let size = vec2(galley.size().x + 18.0, 24.0);
	let (rect, response) = ui.allocate_exact_size(size, Sense::click());
	response.widget_info(|| {
		egui::WidgetInfo::selected(egui::WidgetType::Checkbox, ui.is_enabled(), on, label)
	});
	if ui.is_rect_visible(rect) {
		let p = ui.painter();
		let r = CornerRadius::same(theme::RADIUS_MD);
		let (fill, fg) = if on {
			(theme::mix(theme::BG_PANEL, color, 0.2), color)
		} else if response.hovered() {
			(theme::BG_HOVER, theme::TEXT)
		} else {
			(theme::BG_WIDGET, theme::TEXT_DIM)
		};
		p.rect_filled(rect, r, fill);
		if on {
			p.rect_stroke(
				rect,
				r,
				Stroke::new(1.0, color.gamma_multiply(0.8)),
				StrokeKind::Inside,
			);
		}
		p.text(rect.center(), Align2::CENTER_CENTER, label, font, fg);
	}
	response.on_hover_cursor(CursorIcon::PointingHand)
}

/// Small line icons drawn with the painter (the app fonts have no such symbols).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Glyph {
	/// Listen / solo.
	Headphones,
	/// Freeze.
	Snowflake,
}

/// Paints `glyph` centred on `c`, `size` px tall.
pub fn paint_glyph(p: &egui::Painter, glyph: Glyph, c: Pos2, size: f32, color: Color32) {
	let stroke = Stroke::new((size * 0.11).max(1.2), color);
	match glyph {
		Glyph::Headphones => {
			let r = size * 0.42;
			let center = pos2(c.x, c.y + size * 0.05);
			let band: Vec<Pos2> = (0..=16)
				.map(|i| {
					let a = std::f32::consts::PI * (1.0 + i as f32 / 16.0);
					pos2(center.x + r * a.cos(), center.y + r * a.sin())
				})
				.collect();
			p.add(egui::Shape::line(band, stroke));
			let cup = vec2(size * 0.22, size * 0.38);
			for side in [-1.0, 1.0] {
				let x = center.x + side * (r - cup.x * 0.5 + stroke.width * 0.5);
				p.rect_filled(
					Rect::from_center_size(pos2(x, center.y + cup.y * 0.35), cup),
					CornerRadius::same(2),
					color,
				);
			}
		}
		Glyph::Snowflake => {
			let r = size * 0.48;
			for k in 0..3 {
				let a = std::f32::consts::PI * (0.5 + k as f32 / 3.0);
				let d = vec2(a.cos(), a.sin());
				p.line_segment([c - d * r, c + d * r], stroke);
				for end in [-1.0, 1.0] {
					let tip = c + d * r * end * 0.62;
					for turn in [-0.6f32, 0.6] {
						let b = a + turn + if end < 0.0 { std::f32::consts::PI } else { 0.0 };
						let arm = vec2(b.cos(), b.sin()) * r * 0.32;
						p.line_segment([tip, tip + arm], stroke);
					}
				}
			}
		}
	}
}

/// A [`toggle`] with a [`Glyph`] before its label (Listen, Freeze).
pub fn glyph_toggle(ui: &mut Ui, on: bool, glyph: Glyph, label: &str, color: Color32) -> Response {
	let font = theme::medium(12.0);
	let galley = ui
		.painter()
		.layout_no_wrap(label.to_owned(), font, theme::TEXT);
	let icon = 13.0;
	let size = vec2(galley.size().x + icon + 24.0, 24.0);
	let (rect, response) = ui.allocate_exact_size(size, Sense::click());
	response.widget_info(|| {
		egui::WidgetInfo::selected(egui::WidgetType::Checkbox, ui.is_enabled(), on, label)
	});
	if ui.is_rect_visible(rect) {
		let p = ui.painter();
		let r = CornerRadius::same(theme::RADIUS_MD);
		let (fill, fg) = if on {
			(theme::mix(theme::BG_PANEL, color, 0.2), color)
		} else if response.hovered() {
			(theme::BG_HOVER, theme::TEXT)
		} else {
			(theme::BG_WIDGET, theme::TEXT_DIM)
		};
		p.rect_filled(rect, r, fill);
		if on {
			p.rect_stroke(
				rect,
				r,
				Stroke::new(1.0, color.gamma_multiply(0.8)),
				StrokeKind::Inside,
			);
		}
		let x = rect.left() + 9.0;
		paint_glyph(p, glyph, pos2(x + icon * 0.5, rect.center().y), icon, fg);
		p.galley(
			pos2(x + icon + 6.0, rect.center().y - galley.size().y * 0.5),
			galley,
			fg,
		);
	}
	response.on_hover_cursor(CursorIcon::PointingHand)
}

fn selector_cell(ui: &mut Ui, id: Id, rect: Rect, label: &str, on: bool) -> bool {
	selector_cell_with(ui, id, rect, label, on, true, &theme::mono(11.5))
}

fn selector_cell_with(
	ui: &mut Ui,
	id: Id,
	rect: Rect,
	label: &str,
	on: bool,
	enabled: bool,
	font: &egui::FontId,
) -> bool {
	let enabled = enabled && ui.is_enabled();
	let sense = if enabled {
		Sense::click()
	} else {
		Sense::hover()
	};
	let response = ui.interact(rect, id, sense);
	response.widget_info(|| {
		egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, enabled, on, label)
	});
	let (bg, fg) = if on {
		(theme::BG_PRESSED, theme::TEXT)
	} else if enabled && response.hovered() {
		(theme::BG_WIDGET, theme::TEXT)
	} else if enabled {
		(Color32::TRANSPARENT, theme::TEXT_FAINT)
	} else {
		(Color32::TRANSPARENT, theme::TEXT_FAINT.gamma_multiply(0.45))
	};
	let p = ui.painter();
	p.rect_filled(rect, CornerRadius::same(theme::RADIUS), bg);
	p.text(
		rect.center(),
		Align2::CENTER_CENTER,
		label,
		font.clone(),
		fg,
	);
	enabled && response.clicked()
}

/// Segmented selector for 5 choices or fewer (above that, [`grid_selector`]); returns
/// the clicked segment.
pub fn segmented(ui: &mut Ui, id: Id, labels: &[&str], selected: usize) -> Option<usize> {
	segmented_with(ui, id, labels, selected, &[], &theme::mono(11.5))
}

/// [`segmented`] in `font`, with segments `enabled[i] == false` drawn dim and not
/// clickable (missing entries are enabled): the dialogs' choices (export range).
pub fn segmented_with(
	ui: &mut Ui,
	id: Id,
	labels: &[&str],
	selected: usize,
	enabled: &[bool],
	font: &egui::FontId,
) -> Option<usize> {
	let widths: Vec<f32> = labels
		.iter()
		.map(|l| {
			(ui.painter()
				.layout_no_wrap(tr_str(l).to_owned(), font.clone(), theme::TEXT)
				.size()
				.x + 16.0)
				.max(30.0)
		})
		.collect();
	let size = vec2(widths.iter().sum::<f32>() + 4.0, 24.0);
	let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
	ui.painter().rect(
		rect,
		CornerRadius::same(theme::RADIUS_MD),
		theme::BG_DEEP,
		Stroke::new(1.0, theme::BORDER),
		StrokeKind::Inside,
	);
	let mut x = rect.left() + 2.0;
	let mut clicked = None;
	for (i, (label, w)) in labels.iter().zip(widths).enumerate() {
		let cell = Rect::from_min_size(pos2(x, rect.top() + 2.0), vec2(w, rect.height() - 4.0));
		let on = enabled.get(i).copied().unwrap_or(true);
		if selector_cell_with(ui, id.with(i), cell, tr_str(label), i == selected, on, font) {
			clicked = Some(i);
		}
		x += w;
	}
	clicked
}

/// A grid of choices (delay note values) in `columns` columns; returns the clicked one.
pub fn grid_selector(
	ui: &mut Ui,
	id: Id,
	labels: &[&str],
	selected: usize,
	columns: usize,
	cell: Vec2,
) -> Option<usize> {
	let columns = columns.max(1);
	let rows = labels.len().div_ceil(columns);
	let size = vec2(cell.x * columns as f32 + 4.0, cell.y * rows as f32 + 4.0);
	let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
	ui.painter().rect(
		rect,
		CornerRadius::same(theme::RADIUS_MD),
		theme::BG_DEEP,
		Stroke::new(1.0, theme::BORDER),
		StrokeKind::Inside,
	);
	let mut clicked = None;
	for (i, label) in labels.iter().enumerate() {
		let (row, col) = (i / columns, i % columns);
		let r = Rect::from_min_size(
			rect.min + vec2(2.0 + col as f32 * cell.x, 2.0 + row as f32 * cell.y),
			cell,
		)
		.shrink(1.0);
		if selector_cell(ui, id.with(i), r, tr_str(label), i == selected) {
			clicked = Some(i);
		}
	}
	clicked
}

// --- Picture cards --------------------------------------------------------------------

/// Card layouts: picture above a caption (waves), or a picture square beside the name
/// (modes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CardShape {
	Tall,
	Wide,
}

/// A wave / mode picker card. The picture is the label: `picture` paints into its rect
/// with the stroke colour it is given (the device accent when selected).
pub fn mode_card(
	ui: &mut Ui,
	selected: bool,
	label: &str,
	accent: Color32,
	shape: CardShape,
	picture: impl FnOnce(&Painter, Rect, Color32),
) -> Response {
	let label = tr_str(label);
	let size = match shape {
		CardShape::Tall => vec2(64.0, 54.0),
		CardShape::Wide => vec2(140.0, 44.0),
	};
	let (rect, response) = ui.allocate_exact_size(size, Sense::click());
	response.widget_info(|| {
		egui::WidgetInfo::selected(
			egui::WidgetType::RadioButton,
			ui.is_enabled(),
			selected,
			label,
		)
	});
	if ui.is_rect_visible(rect) {
		let p = ui.painter();
		let r = CornerRadius::same(theme::RADIUS_LG);
		let (fill, border, width) = if selected {
			(theme::BG_HOVER, accent, 1.5)
		} else if response.hovered() {
			(theme::BG_WIDGET, theme::BORDER_STRONG, 1.0)
		} else {
			(theme::BG_RAISED, KNOB_TRACK, 1.0)
		};
		p.rect(
			rect,
			r,
			fill,
			Stroke::new(width, border),
			StrokeKind::Inside,
		);
		let ink = if selected { accent } else { theme::TEXT_DIM };
		match shape {
			CardShape::Tall => {
				let pic = Rect::from_min_max(
					rect.min + vec2(10.0, 8.0),
					pos2(rect.right() - 10.0, rect.top() + 32.0),
				);
				picture(&p.with_clip_rect(pic.expand(2.0)), pic, ink);
				p.text(
					pos2(rect.center().x, rect.bottom() - 11.0),
					Align2::CENTER_CENTER,
					label,
					egui::FontId::proportional(11.0),
					if selected {
						theme::TEXT
					} else {
						theme::TEXT_FAINT
					},
				);
			}
			CardShape::Wide => {
				let pic = Rect::from_min_size(
					rect.min + vec2(7.0, 7.0),
					Vec2::splat(rect.height() - 14.0),
				);
				p.rect_filled(pic, CornerRadius::same(theme::RADIUS), theme::DISPLAY_BG);
				let inner = pic.shrink(5.0);
				picture(&p.with_clip_rect(pic), inner, ink);
				p.text(
					pos2(pic.right() + 10.0, rect.center().y),
					Align2::LEFT_CENTER,
					label,
					theme::semibold(12.0),
					if selected { accent } else { theme::TEXT },
				);
			}
		}
	}
	response.on_hover_cursor(CursorIcon::PointingHand)
}

/// Oscillator waves the cards draw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wave {
	Sine,
	Saw,
	Square,
	Triangle,
}

/// Two cycles of `wave` across `rect`.
pub fn paint_wave(p: &Painter, rect: Rect, wave: Wave, color: Color32) {
	let n = 96;
	let cycles = 2.0;
	let f = |t: f32| {
		let ph = (t * cycles).fract();
		match wave {
			Wave::Sine => (ph * std::f32::consts::TAU).sin(),
			Wave::Saw => 1.0 - 2.0 * ph,
			Wave::Square => {
				if ph < 0.5 {
					1.0
				} else {
					-1.0
				}
			}
			Wave::Triangle => {
				if ph < 0.5 {
					4.0 * ph - 1.0
				} else {
					3.0 - 4.0 * ph
				}
			}
		}
	};
	let y = |v: f32| rect.center().y - v * rect.height() * 0.42;
	let mut pts = Vec::with_capacity(n + 8);
	let mut prev = f(0.0);
	for i in 0..=n {
		let t = i as f32 / n as f32;
		let v = f(t);
		let x = rect.left() + t * rect.width();
		// Vertical edges for the jumps (saw, square).
		if i > 0 && (v - prev).abs() > 1.0 {
			pts.push(pos2(x, y(prev)));
		}
		pts.push(pos2(x, y(v)));
		prev = v;
	}
	p.add(Shape::line(pts, Stroke::new(1.5, color)));
}

/// A transfer curve `shape(-1..1) -> -1..1` across `rect` (saturator modes).
pub fn paint_curve(p: &Painter, rect: Rect, color: Color32, shape: impl Fn(f32) -> f32) {
	let n = 48;
	let pts: Vec<Pos2> = (0..=n)
		.map(|i| {
			let x = -1.0 + 2.0 * i as f32 / n as f32;
			pos2(
				rect.center().x + x * rect.width() * 0.5,
				rect.center().y - shape(x).clamp(-1.0, 1.0) * rect.height() * 0.5,
			)
		})
		.collect();
	p.add(Shape::line(pts, Stroke::new(1.5, color)));
}

// --- Meters ---------------------------------------------------------------------------

fn meter_pos(db: f32) -> f32 {
	((db - FLOOR_DB) / (METER_MAX_DB - FLOOR_DB)).clamp(0.0, 1.0)
}

/// Green up to −9 dBFS, yellow up to −3, red above.
const ZONES: [(f32, f32, Color32); 3] = [
	(FLOOR_DB, -9.0, theme::METER_LOW),
	(-9.0, -3.0, theme::METER_MID),
	(-3.0, METER_MAX_DB, theme::METER_HIGH),
];

/// The held peak of `db` under `id`: it holds for 1.5 s, then follows the level.
pub fn peak_hold(ctx: &egui::Context, id: Id, db: f32) -> f32 {
	let now = ctx.input(|i| i.time);
	let (held, at) = ctx
		.data(|d| d.get_temp::<(f32, f64)>(id))
		.unwrap_or((FLOOR_DB, now));
	let next = if db >= held || now - at > HOLD {
		(db, now)
	} else {
		(held, at)
	};
	ctx.data_mut(|d| d.insert_temp(id, next));
	next.0
}

/// A vertical level meter, one bar per channel (dBFS), coloured by zone, with a white
/// 1.5 s peak-hold tick.
pub fn level_meter(ui: &mut Ui, id: Id, levels_db: &[f32], size: Vec2) -> Response {
	let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
	let holds: Vec<f32> = levels_db
		.iter()
		.enumerate()
		.map(|(i, db)| peak_hold(ui.ctx(), id.with(i), *db))
		.collect();
	if ui.is_rect_visible(rect) {
		paint_level_meter(ui.painter(), rect, levels_db, &holds);
	}
	response
}

/// Paints [`level_meter`] bars (holds per channel; `FLOOR_DB` hides one).
pub fn paint_level_meter(p: &Painter, rect: Rect, levels_db: &[f32], holds_db: &[f32]) {
	let n = levels_db.len().max(1);
	let gap = 2.0;
	let w = (rect.width() - gap * (n - 1) as f32) / n as f32;
	for (i, db) in levels_db.iter().enumerate() {
		let col = Rect::from_min_size(
			pos2(rect.left() + i as f32 * (w + gap), rect.top()),
			vec2(w, rect.height()),
		);
		p.rect_filled(col, CornerRadius::same(1), theme::DISPLAY_BG);
		let y = |db: f32| col.bottom() - meter_pos(db) * col.height();
		for (lo, hi, color) in ZONES {
			if *db <= lo {
				break;
			}
			let r = Rect::from_x_y_ranges(col.x_range(), y(db.min(hi))..=y(lo));
			p.rect_filled(r, 0.0, color);
		}
		if let Some(&hold) = holds_db.get(i)
			&& hold > FLOOR_DB
		{
			p.hline(
				col.x_range(),
				y(hold),
				Stroke::new(1.5, theme::METER_PEAK_HOLD),
			);
		}
	}
}

/// Gain reduction (positive dB) as a coral bar hanging from the top (or from the left in
/// a wide `size`) over `range_db`, with a held-maximum tick.
pub fn gr_meter(ui: &mut Ui, id: Id, gr_db: f32, range_db: f32, size: Vec2) -> Response {
	let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
	let held = -peak_hold(ui.ctx(), id, -gr_db.abs());
	if ui.is_rect_visible(rect) {
		paint_gr_meter(ui.painter(), rect, gr_db.abs(), held, range_db);
	}
	response
}

/// Paints [`gr_meter`]: hanging from the top, or growing from the left when `rect` is
/// wider than tall.
pub fn paint_gr_meter(p: &Painter, rect: Rect, gr_db: f32, held_db: f32, range_db: f32) {
	p.rect_filled(rect, CornerRadius::same(1), theme::DISPLAY_BG);
	let across = rect.width() > rect.height();
	let len = if across { rect.width() } else { rect.height() };
	let at = |db: f32| (db / range_db.max(1.0)).clamp(0.0, 1.0) * len;
	if gr_db > 0.0 {
		let size = if across {
			vec2(at(gr_db), rect.height())
		} else {
			vec2(rect.width(), at(gr_db))
		};
		p.rect_filled(
			Rect::from_min_size(rect.min, size),
			0.0,
			theme::GAIN_REDUCTION,
		);
	}
	if held_db > 0.05 {
		let stroke = Stroke::new(1.5, theme::METER_PEAK_HOLD);
		if across {
			p.vline(rect.left() + at(held_db), rect.y_range(), stroke);
		} else {
			p.hline(rect.x_range(), rect.top() + at(held_db), stroke);
		}
	}
}

/// A horizontal stereo bar pair (two thin lines) for device headers.
pub fn paint_bar_pair(p: &Painter, rect: Rect, levels_db: [f32; 2]) {
	let line_h = ((rect.height() - 1.0) * 0.5).max(1.0);
	for (i, db) in levels_db.iter().enumerate() {
		let row = Rect::from_min_size(
			pos2(rect.left(), rect.top() + i as f32 * (line_h + 1.0)),
			vec2(rect.width(), line_h),
		);
		p.rect_filled(row, 0.0, theme::BG_WIDGET);
		let x = |db: f32| row.left() + meter_pos(db) * row.width();
		for (lo, hi, color) in ZONES {
			if *db <= lo {
				break;
			}
			p.rect_filled(
				Rect::from_x_y_ranges(x(lo)..=x(db.min(hi)), row.y_range()),
				0.0,
				color,
			);
		}
	}
}

/// The IN / OUT meter pair of a device header (levels in dBFS per channel). `compact`
/// = the 34 px pair of the compact header (no labels or numbers).
pub fn io_meters(ui: &mut Ui, input: [f32; 2], output: [f32; 2], compact: bool) -> Response {
	if compact {
		let (rect, response) = ui.allocate_exact_size(vec2(34.0, 12.0), Sense::hover());
		if ui.is_rect_visible(rect) {
			let p = ui.painter();
			let w = (rect.width() - 2.0) * 0.5;
			let bar = |x: f32| Rect::from_min_size(pos2(x, rect.center().y - 2.5), vec2(w, 5.0));
			paint_bar_pair(p, bar(rect.left()), input);
			paint_bar_pair(p, bar(rect.left() + w + 2.0), output);
		}
		return response.on_hover_text(trf(
			"In {level_text} · Out {out}",
			&[
				("level_text", &level_text(input)),
				("out", &level_text(output)),
			],
		));
	}
	let label_w = 24.0;
	let bar_w = 56.0;
	let value_w = 40.0;
	let one = label_w + bar_w + value_w;
	let (rect, response) = ui.allocate_exact_size(vec2(one * 2.0 + 8.0, 18.0), Sense::hover());
	if ui.is_rect_visible(rect) {
		let p = ui.painter();
		for (i, (name, levels)) in [(tr("IN"), input), (tr("OUT"), output)]
			.into_iter()
			.enumerate()
		{
			let x0 = rect.left() + i as f32 * (one + 8.0);
			p.text(
				pos2(x0, rect.center().y),
				Align2::LEFT_CENTER,
				name,
				theme::mono(10.5),
				theme::TEXT_FAINT,
			);
			let bar =
				Rect::from_min_size(pos2(x0 + label_w, rect.center().y - 2.5), vec2(bar_w, 5.0));
			paint_bar_pair(p, bar, levels);
			p.text(
				pos2(bar.right() + value_w - 2.0, rect.center().y),
				Align2::RIGHT_CENTER,
				level_text(levels),
				theme::mono(10.5),
				theme::TEXT_DIM,
			);
		}
	}
	response
}

fn level_text(levels: [f32; 2]) -> String {
	let db = levels[0].max(levels[1]);
	if db <= FLOOR_DB {
		"-inf".to_owned()
	} else {
		format!("{db:.1}")
	}
}

// --- Display helpers ------------------------------------------------------------------

/// A filled area between a polyline and the horizontal line `base_y`, as one mesh (egui
/// polygons must be convex; curves are not).
pub fn fill_to(points: &[Pos2], base_y: f32, color: Color32) -> Shape {
	let mut mesh = egui::Mesh::default();
	if points.len() < 2 {
		return Shape::Noop;
	}
	for (i, p) in points.iter().enumerate() {
		mesh.colored_vertex(*p, color);
		mesh.colored_vertex(pos2(p.x, base_y), color);
		if i > 0 {
			let k = (i as u32) * 2;
			mesh.add_triangle(k - 2, k - 1, k);
			mesh.add_triangle(k - 1, k, k + 1);
		}
	}
	Shape::mesh(mesh)
}

/// A value tag on a display ("Decay 2.4 s"): a raised chip with a 1 px ring and mono
/// text in `color`, placed by `align` at `anchor` and kept inside `bounds`. Returns its
/// rect.
pub fn paint_value_tag(
	p: &Painter,
	anchor: Pos2,
	align: Align2,
	text: &str,
	color: Color32,
	bounds: Rect,
) -> Rect {
	let galley = p.layout_no_wrap(text.to_owned(), theme::mono(11.0), color);
	let size = galley.size() + vec2(12.0, 4.0);
	let mut rect = align.anchor_size(anchor, size);
	let shift = vec2(
		(bounds.left() + 2.0 - rect.left()).max(0.0)
			+ (bounds.right() - 2.0 - rect.right()).min(0.0),
		(bounds.top() + 2.0 - rect.top()).max(0.0)
			+ (bounds.bottom() - 2.0 - rect.bottom()).min(0.0),
	);
	rect = rect.translate(shift);
	p.rect(
		rect,
		CornerRadius::same(theme::RADIUS),
		theme::BG_WIDGET,
		Stroke::new(1.0, theme::BORDER_STRONG),
		StrokeKind::Inside,
	);
	p.galley(rect.center() - galley.size() * 0.5, galley, color);
	rect
}

/// A display curve with the full three-stroke glow (sum curves).
fn glow_line_wide(p: &Painter, points: Vec<Pos2>, color: Color32) {
	p.add(Shape::line(
		points.clone(),
		Stroke::new(11.0, color.gamma_multiply(0.06)),
	));
	theme::glow_line(p, points, color);
}

fn add_rect(mesh: &mut egui::Mesh, rect: Rect, color: Color32) {
	mesh.add_colored_rect(rect, color);
}

// --- ADSR editor ----------------------------------------------------------------------

/// What [`adsr`] reports: one [`Gesture`] per stage (indexed by [`AdsrStage::index`]);
/// dragging D moves decay and sustain together.
#[derive(Debug)]
pub struct AdsrResponse {
	pub gestures: [Gesture; 4],
	/// The node being dragged.
	pub active: Option<AdsrStage>,
	pub response: Response,
}

impl AdsrResponse {
	pub fn gesture(&self, stage: AdsrStage) -> Gesture {
		self.gestures[stage.index()]
	}
}

#[derive(Debug, Clone, Copy)]
struct AdsrDrag {
	stage: AdsrStage,
	start: AdsrValues,
	/// Accumulated (fine-scaled) drag in unit coordinates.
	dx: f32,
	dy: f32,
}

fn adsr_tag(stage: AdsrStage, v: &AdsrValues, r: &AdsrRanges) -> String {
	let time = |ms: f32| device::format_value(ms, "ms");
	let sustain = || device::format_value(v.sustain, r.sustain_unit);
	match stage {
		AdsrStage::Attack => format!("A {}", time(v.attack)),
		AdsrStage::Decay => format!("D {} · S {}", time(v.decay), sustain()),
		AdsrStage::Sustain => format!("S {}", sustain()),
		AdsrStage::Release => format!("R {}", time(v.release)),
	}
}

/// The ADSR editor: A drags horizontally, D both ways (decay and sustain), S vertically,
/// R horizontally, on a cube-root time axis; Shift = fine, double-click resets that
/// stage. The dragged node turns white with a floating "D 400 ms · S 62 %" tag. `voices`
/// draws up to 4 playhead dots.
pub fn adsr(
	ui: &mut Ui,
	id: Id,
	values: AdsrValues,
	ranges: &AdsrRanges,
	color: Color32,
	size: Vec2,
	voices: &[VoicePos],
) -> AdsrResponse {
	let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
	let plot = rect.shrink2(vec2(12.0, 12.0));
	let to_screen = |(x, y): (f32, f32)| {
		pos2(
			plot.left() + x * plot.width(),
			plot.bottom() - y * plot.height(),
		)
	};
	let ctx = ui.ctx().clone();
	let shift = ui.input(|i| i.modifiers.shift);
	let drag_id = id.with("drag");
	let mut drag: Option<AdsrDrag> = ctx.data(|d| d.get_temp(drag_id)).flatten();
	let mut gestures = [Gesture::Idle; 4];
	let mut shown = drag.map_or(values, |d| {
		device_display::adsr_drag(&d.start, ranges, d.stage, d.dx, d.dy)
	});
	let points = device_display::adsr_points(&shown, ranges);
	let mut hovered = None;
	// Sustain before decay: D is on top where they meet.
	for stage in [
		AdsrStage::Attack,
		AdsrStage::Sustain,
		AdsrStage::Decay,
		AdsrStage::Release,
	] {
		let at = to_screen(points.node(stage));
		let r = ui.interact(
			Rect::from_center_size(at, Vec2::splat(16.0)),
			id.with(stage.index()),
			Sense::click_and_drag(),
		);
		let name = match stage {
			AdsrStage::Attack => tr("Envelope attack"),
			AdsrStage::Decay => tr("Envelope decay"),
			AdsrStage::Sustain => tr("Envelope sustain"),
			AdsrStage::Release => tr("Envelope release"),
		};
		r.widget_info(|| {
			egui::WidgetInfo::slider(ui.is_enabled(), f64::from(shown.get(stage)), name)
		});
		let r = r.on_hover_cursor(match stage {
			AdsrStage::Sustain => CursorIcon::ResizeVertical,
			AdsrStage::Decay => CursorIcon::Move,
			_ => CursorIcon::ResizeHorizontal,
		});
		if r.hovered() {
			hovered = Some(stage);
		}
		if r.drag_started() {
			drag = Some(AdsrDrag {
				stage,
				start: values,
				dx: 0.0,
				dy: 0.0,
			});
		}
		if r.dragged()
			&& let Some(d) = drag.as_mut().filter(|d| d.stage == stage)
		{
			let fine = if shift { FINE } else { 1.0 };
			let delta = r.drag_delta();
			d.dx += delta.x / plot.width().max(1.0) * fine;
			d.dy -= delta.y / plot.height().max(1.0) * fine;
			shown = device_display::adsr_drag(&d.start, ranges, stage, d.dx, d.dy);
			for &s in moved(stage) {
				gestures[s.index()] = Gesture::Preview(shown.get(s));
			}
		}
		if r.drag_stopped()
			&& let Some(d) = drag.take().filter(|d| d.stage == stage)
		{
			shown = device_display::adsr_drag(&d.start, ranges, stage, d.dx, d.dy);
			for &s in moved(stage) {
				gestures[s.index()] = Gesture::Commit(shown.get(s));
			}
		}
		if r.double_clicked() {
			drag = None;
			let default = ranges.get(stage).default;
			shown.set(stage, default);
			gestures[stage.index()] = Gesture::Commit(default);
		}
	}
	ctx.data_mut(|m| m.insert_temp(drag_id, drag));
	let active = drag.map(|d| d.stage);

	if ui.is_rect_visible(rect) {
		let p = ui.painter().with_clip_rect(rect);
		paint_display(&p, rect);
		let points = device_display::adsr_points(&shown, ranges);
		for (x, _) in [points.attack, points.decay, points.sustain] {
			let x = to_screen((x, 0.0)).x;
			p.vline(x, plot.y_range(), Stroke::new(1.0, GRID_MINOR));
		}
		p.hline(plot.x_range(), plot.bottom(), Stroke::new(1.0, GRID_MAJOR));
		let curve: Vec<Pos2> = device_display::adsr_curve(&points, 16)
			.into_iter()
			.map(to_screen)
			.collect();
		p.add(fill_to(&curve, plot.bottom(), color.gamma_multiply(0.12)));
		theme::glow_line(&p, curve, color);
		for stage in AdsrStage::ALL {
			let at = to_screen(points.node(stage));
			if active == Some(stage) {
				p.circle_filled(at, 5.5, Color32::WHITE);
			} else if hovered == Some(stage) {
				p.circle(at, 5.0, color, Stroke::new(1.5, Color32::WHITE));
			} else {
				p.circle(at, 4.5, theme::DISPLAY_BG, Stroke::new(1.5, color));
			}
		}
		// Voice dots above the nodes (a held voice sits on the sustain node), ringed in
		// the display colour so they read on a node too.
		for &v in voices.iter().take(4) {
			let at = to_screen(device_display::voice_point(&shown, ranges, v));
			p.circle(at, 3.2, theme::TEXT, Stroke::new(1.5, theme::DISPLAY_BG));
		}
		if let Some(stage) = active.or(hovered) {
			let at = to_screen(points.node(stage));
			floating_value(
				&ctx,
				id,
				Rect::from_center_size(at, Vec2::splat(12.0)),
				&adsr_tag(stage, &shown, ranges),
			);
		}
	}
	AdsrResponse {
		gestures,
		active,
		response,
	}
}

/// The stages a node's drag changes.
fn moved(stage: AdsrStage) -> &'static [AdsrStage] {
	match stage {
		AdsrStage::Attack => &[AdsrStage::Attack],
		AdsrStage::Decay => &[AdsrStage::Decay, AdsrStage::Sustain],
		AdsrStage::Sustain => &[AdsrStage::Sustain],
		AdsrStage::Release => &[AdsrStage::Release],
	}
}

// --- Frequency display ----------------------------------------------------------------

pub const GRID_MAJOR: Color32 = Color32::from_rgb(0x1c, 0x1e, 0x22);
pub const GRID_MINOR: Color32 = Color32::from_rgb(0x15, 0x17, 0x1a);
const GRID_ZERO: Color32 = Color32::from_rgb(0x2a, 0x2c, 0x32);
const PIANO_HEIGHT: f32 = 14.0;
const NODE_RADIUS: f32 = 10.0;
/// Node radius used by the compact (inspector) form, where the dock's 20 px nodes
/// collide at 7 bands.
const NODE_RADIUS_SMALL: f32 = 8.0;
/// Pixels of movement before a drag picks the axis Alt locks to.
const AXIS_PICK: f32 = 3.0;

/// A band's response curve, filled translucent between it and 0 dB (stronger when
/// `selected`).
pub struct BandCurve<'a> {
	pub response: &'a dyn Fn(f32) -> f32,
	pub color: Color32,
	pub selected: bool,
}

/// A bandwidth bracket under a node: `low`..`high` Hz at `db`.
#[derive(Debug, Clone, Copy)]
pub struct Bracket {
	pub low: f32,
	pub high: f32,
	pub db: f32,
	pub color: Color32,
}

/// A draggable node (20 px, labelled with the band id; a dormant band is a hollow ring).
#[derive(Debug, Clone, Copy)]
pub struct FreqNode<'a> {
	pub label: &'a str,
	pub freq: f32,
	pub db: f32,
	pub color: Color32,
	pub dormant: bool,
	/// Moves in frequency only (high-pass / low-pass on the 0 dB line).
	pub x_only: bool,
	/// The last AI request moved this band (blue ring).
	pub ai: bool,
	/// A dynamic band: (the dB it plays now, activity 0..=1 of its depth). The node
	/// stays at its static gain; a dashed bar spans 0 dB → gain (how far it can move),
	/// a marker sits at the live gain and a halo grows with the activity.
	pub dynamic: Option<(f32, f32)>,
}

/// What the user did on a [`FreqDisplay`]; callers map it to parameters.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FreqEvent {
	/// Node dragged to `freq`/`db` (`commit` on release: one command per gesture).
	Move {
		node: usize,
		freq: f32,
		db: f32,
		commit: bool,
	},
	/// Wheel over a node: its third dimension (Q, slope, width), in notches (Shift: ×0.1).
	Wheel {
		node: usize,
		notches: f32,
	},
	Click {
		node: usize,
	},
	/// Double-click on a node.
	Reset {
		node: usize,
	},
	/// Double-click on empty space, at the pointer.
	Create {
		freq: f32,
		db: f32,
	},
}

#[derive(Debug)]
pub struct FreqResponse {
	pub events: Vec<FreqEvent>,
	/// Frequency and dB under the pointer.
	pub hover: Option<(f32, f32)>,
	pub hovered_node: Option<usize>,
	pub dragging: Option<usize>,
	pub response: Response,
	/// Each node's own response (context menus).
	pub node_responses: Vec<Response>,
	/// The plot area (above the piano strip).
	pub plot: Rect,
}

/// Height of the collision heat strip along the bottom of the plot.
pub const HEAT_STRIP_H: f32 = 14.0;

/// A frequency range where this track collides with others (collision view): a red
/// column over the plot and a cell of the heat strip, stronger with `heat` (0..1).
#[derive(Debug, Clone, Copy)]
pub struct HeatBand {
	pub low: f32,
	pub high: f32,
	pub heat: f32,
	/// Hovered or selected: drawn brighter and outlined.
	pub hot: bool,
}

#[derive(Debug, Clone, Copy)]
struct NodeDrag {
	node: usize,
	start: (f32, f32),
	/// Accumulated (fine-scaled) pixels.
	delta: Vec2,
	/// Axis the drag started on: true = horizontal.
	horizontal: Option<bool>,
	last: (f32, f32),
	/// Grabbed on the curve between nodes (the display's own response drags).
	curve: bool,
}

/// Picks the node a drag on the curve at a frequency moves.
pub type CurveGrab<'a> = &'a dyn Fn(f32) -> Option<usize>;

/// The log 20 Hz–20 kHz display of EQs and filters: labelled grid, optional pre/post
/// analyzer (1/6-octave dB bins at [`device_display::analyzer_freq`]), per-band fills,
/// a glowing sum curve, draggable nodes, a pointer hairline and a piano strip.
#[must_use = "call `show`"]
pub struct FreqDisplay<'a> {
	id: Id,
	size: Vec2,
	range: DbRange,
	sum: Option<(&'a dyn Fn(f32) -> f32, Color32)>,
	bands: &'a [BandCurve<'a>],
	pre: Option<&'a [f32]>,
	post: Option<&'a [f32]>,
	nodes: &'a [FreqNode<'a>],
	selected: Option<usize>,
	piano: bool,
	grab: Option<CurveGrab<'a>>,
	bracket: Option<Bracket>,
	node_radius: f32,
	before: Option<&'a dyn Fn(f32) -> f32>,
	ghost: Option<&'a dyn Fn(f32) -> f32>,
	sum_fill: f32,
	/// A second glowing curve (an EQ's side response next to the mid one).
	sum2: Option<(&'a dyn Fn(f32) -> f32, Color32)>,
	/// Tags written at the right end of the sum and the second curve ("M", "S").
	sum_tags: Option<[&'a str; 2]>,
	/// Small letter badges on the nodes ("M" / "S"), by node index.
	badges: &'a [Option<&'a str>],
	/// A proposed response (match EQ preview), drawn dashed in the accent.
	proposal: Option<&'a dyn Fn(f32) -> f32>,
	/// A reference spectrum (dB per analyzer bin, levelled to the curve): a thin line.
	reference: Option<&'a [f32]>,
	/// Other tracks' spectra (analyzer bins) as thin lines in their colours.
	spectra: &'a [(&'a [f32], Color32)],
	/// Collision heat: red columns + the strip along the bottom.
	heat: &'a [HeatBand],
}

impl<'a> FreqDisplay<'a> {
	pub fn new(id: Id, size: Vec2) -> Self {
		Self {
			id,
			size,
			range: DbRange::PM12,
			sum: None,
			bands: &[],
			pre: None,
			post: None,
			nodes: &[],
			selected: None,
			piano: false,
			grab: None,
			bracket: None,
			node_radius: NODE_RADIUS,
			before: None,
			ghost: None,
			sum_fill: 0.0,
			sum2: None,
			sum_tags: None,
			badges: &[],
			proposal: None,
			reference: None,
			spectra: &[],
			heat: &[],
		}
	}

	/// Other tracks' spectra as thin lines (analyzer bins at
	/// [`device_display::analyzer_freq`], each in its colour).
	pub fn spectra(mut self, spectra: &'a [(&'a [f32], Color32)]) -> Self {
		self.spectra = spectra;
		self
	}

	/// Collision heat bands (columns + a strip of [`HEAT_STRIP_H`] along the bottom).
	pub fn heat(mut self, bands: &'a [HeatBand]) -> Self {
		self.heat = bands;
		self
	}

	/// A second sum curve in `color` (the side next to the mid).
	pub fn sum2(mut self, response: Option<&'a dyn Fn(f32) -> f32>, color: Color32) -> Self {
		self.sum2 = response.map(|f| (f, color));
		self
	}

	/// Tags at the right end of the sum and the second sum.
	pub fn sum_tags(mut self, tags: Option<[&'a str; 2]>) -> Self {
		self.sum_tags = tags;
		self
	}

	/// Letter badges on the nodes, by index.
	pub fn badges(mut self, badges: &'a [Option<&'a str>]) -> Self {
		self.badges = badges;
		self
	}

	/// A proposed response, dashed in the accent (match EQ before Apply).
	pub fn proposal(mut self, response: Option<&'a dyn Fn(f32) -> f32>) -> Self {
		self.proposal = response;
		self
	}

	/// A reference spectrum shown as a thin line (dB per analyzer bin at
	/// [`device_display::analyzer_freq`]).
	pub fn reference(mut self, bins: Option<&'a [f32]>) -> Self {
		self.reference = bins;
		self
	}

	/// A thin dim solid curve under the sum (a dynamic EQ's static curve while the sum
	/// shows the live one).
	pub fn ghost(mut self, response: Option<&'a dyn Fn(f32) -> f32>) -> Self {
		self.ghost = response;
		self
	}

	/// The response before the last AI request, drawn dashed and dim.
	pub fn before(mut self, response: Option<&'a dyn Fn(f32) -> f32>) -> Self {
		self.before = response;
		self
	}

	/// Shrinks the draggable nodes (and their label font) from the default 20 px to
	/// 16 px, for forms too narrow for the full size (the compact EQ).
	pub fn small_nodes(mut self) -> Self {
		self.node_radius = NODE_RADIUS_SMALL;
		self
	}

	/// The selected peak's bandwidth.
	pub fn bracket(mut self, bracket: Option<Bracket>) -> Self {
		self.bracket = bracket;
		self
	}

	/// Dragging the curve (not a node) moves the node `grab` picks for the pointer's
	/// frequency.
	pub fn curve_grab(mut self, grab: CurveGrab<'a>) -> Self {
		self.grab = Some(grab);
		self
	}

	/// ±12, ±24 or the filter range (`DbRange::FILTER`, −48..+24).
	pub fn range(mut self, range: DbRange) -> Self {
		self.range = range;
		self
	}

	pub fn sum(mut self, response: &'a dyn Fn(f32) -> f32, color: Color32) -> Self {
		self.sum = Some((response, color));
		self
	}

	/// Fills the area under the sum curve down to the bottom at `alpha` of its colour (the
	/// pass band of a filter: what gets through).
	pub fn sum_fill(mut self, alpha: f32) -> Self {
		self.sum_fill = alpha;
		self
	}

	pub fn bands(mut self, bands: &'a [BandCurve<'a>]) -> Self {
		self.bands = bands;
		self
	}

	/// Analyzer before the device (solid dim fill).
	pub fn pre(mut self, bins: Option<&'a [f32]>) -> Self {
		self.pre = bins;
		self
	}

	/// Analyzer after the device (translucent blue fill with a line).
	pub fn post(mut self, bins: Option<&'a [f32]>) -> Self {
		self.post = bins;
		self
	}

	pub fn nodes(mut self, nodes: &'a [FreqNode<'a>]) -> Self {
		self.nodes = nodes;
		self
	}

	/// The selected node (ring; lights the piano when nothing is hovered).
	pub fn selected(mut self, node: Option<usize>) -> Self {
		self.selected = node;
		self
	}

	pub fn piano(mut self, on: bool) -> Self {
		self.piano = on;
		self
	}

	pub fn show(self, ui: &mut Ui) -> FreqResponse {
		let (rect, _) = ui.allocate_exact_size(self.size, Sense::hover());
		let strip = if self.piano {
			Some(Rect::from_min_max(
				pos2(rect.left() + 1.0, rect.bottom() - 1.0 - PIANO_HEIGHT),
				pos2(rect.right() - 1.0, rect.bottom() - 1.0),
			))
		} else {
			None
		};
		let plot = Rect::from_min_max(
			rect.min + vec2(1.0, 1.0),
			pos2(
				rect.right() - 1.0,
				strip.map_or(rect.bottom() - 1.0, |s| s.top()),
			),
		);
		let range = self.range;
		let w = plot.width();
		let h = plot.height();
		let x_of = |f: f32| plot.left() + device_display::freq_to_x(f, w);
		let y_of = |db: f32| plot.top() + device_display::db_to_y(db, range, h);
		let node_radius = self.node_radius;
		let node_at = |n: &FreqNode| {
			pos2(
				x_of(n.freq),
				y_of(n.db.clamp(range.min, range.max))
					.clamp(plot.top() + node_radius, plot.bottom() - node_radius),
			)
		};

		let response = ui.interact(rect, self.id, Sense::click_and_drag());
		response.widget_info(|| {
			egui::WidgetInfo::labeled(egui::WidgetType::Other, true, tr("Frequency display"))
		});
		let ctx = ui.ctx().clone();
		let (shift, alt, command, scroll) = ui.input(|i| {
			(
				i.modifiers.shift,
				i.modifiers.alt,
				i.modifiers.command,
				i.smooth_scroll_delta.x + i.smooth_scroll_delta.y,
			)
		});
		let drag_id = self.id.with("drag");
		let mut drag: Option<NodeDrag> = ctx.data(|d| d.get_temp(drag_id)).flatten();
		let nodes = self.nodes;
		// Shift fine, Alt locks to the first axis moved, Cmd/Ctrl moves the gain only.
		let advance = |d: &mut NodeDrag, delta: Vec2| -> FreqEvent {
			d.delta += delta * if shift { FINE } else { 1.0 };
			if d.horizontal.is_none() && d.delta.length() > AXIS_PICK {
				d.horizontal = Some(d.delta.x.abs() >= d.delta.y.abs());
			}
			let mut delta = d.delta;
			if alt {
				match d.horizontal {
					Some(true) => delta.y = 0.0,
					Some(false) => delta.x = 0.0,
					None => {}
				}
			}
			if command {
				delta.x = 0.0;
			}
			if nodes.get(d.node).is_some_and(|n| n.x_only) {
				delta.y = 0.0;
			}
			let x = device_display::freq_to_x(d.start.0, w) + delta.x;
			let y = device_display::db_to_y(d.start.1, range, h) + delta.y;
			d.last = (
				device_display::x_to_freq(x, w)
					.clamp(device_display::FREQ_MIN, device_display::FREQ_MAX),
				device_display::y_to_db(y, range, h).clamp(range.min, range.max),
			);
			FreqEvent::Move {
				node: d.node,
				freq: d.last.0,
				db: d.last.1,
				commit: false,
			}
		};
		let start_drag = |i: usize, node: &FreqNode, curve: bool| NodeDrag {
			node: i,
			start: (node.freq, node.db),
			delta: Vec2::ZERO,
			horizontal: None,
			last: (node.freq, node.db),
			curve,
		};
		let mut events = Vec::new();
		let mut hovered_node = None;
		let mut node_responses = Vec::with_capacity(self.nodes.len());
		for (i, node) in self.nodes.iter().enumerate() {
			let at = node_at(node);
			let r = ui.interact(
				Rect::from_center_size(at, Vec2::splat(node_radius * 2.0 + 2.0)),
				self.id.with(("node", i)),
				Sense::click_and_drag(),
			);
			r.widget_info(|| {
				egui::WidgetInfo::labeled(
					egui::WidgetType::Other,
					true,
					trf("Band {label}", &[("label", &node.label)]),
				)
			});
			let r = r.on_hover_cursor(if node.x_only {
				CursorIcon::ResizeHorizontal
			} else {
				CursorIcon::Grab
			});
			if r.hovered() {
				hovered_node = Some(i);
			}
			if r.drag_started() {
				drag = Some(start_drag(i, node, false));
			}
			if r.dragged()
				&& let Some(d) = drag.as_mut().filter(|d| d.node == i && !d.curve)
			{
				events.push(advance(d, r.drag_delta()));
			}
			if r.drag_stopped()
				&& let Some(d) = drag.take().filter(|d| d.node == i)
			{
				events.push(FreqEvent::Move {
					node: i,
					freq: d.last.0,
					db: d.last.1,
					commit: true,
				});
			}
			if r.clicked() {
				events.push(FreqEvent::Click { node: i });
			}
			if r.double_clicked() {
				events.push(FreqEvent::Reset { node: i });
			}
			if r.hovered() && scroll != 0.0 && !r.dragged() {
				ui.input_mut(|inp| inp.smooth_scroll_delta = Vec2::ZERO);
				events.push(FreqEvent::Wheel {
					node: i,
					notches: scroll / NOTCH * if shift { FINE } else { 1.0 },
				});
			}
			node_responses.push(r);
		}
		if let Some(grab) = self.grab
			&& response.drag_started()
			&& let Some(p) = response.interact_pointer_pos()
			&& let Some(i) = grab(device_display::x_to_freq(p.x - plot.left(), w))
			&& let Some(node) = self.nodes.get(i)
		{
			drag = Some(start_drag(i, node, true));
		}
		if response.dragged()
			&& let Some(d) = drag.as_mut().filter(|d| d.curve)
		{
			events.push(advance(d, response.drag_delta()));
		}
		if response.drag_stopped()
			&& let Some(d) = drag.take_if(|d| d.curve)
		{
			events.push(FreqEvent::Move {
				node: d.node,
				freq: d.last.0,
				db: d.last.1,
				commit: true,
			});
		}
		ctx.data_mut(|m| m.insert_temp(drag_id, drag));
		let dragging = drag.map(|d| d.node);
		let pointer = response
			.hover_pos()
			.filter(|p| plot.contains(*p) || strip.is_some_and(|s| s.contains(*p)));
		let hover = pointer.map(|p| {
			(
				device_display::x_to_freq(p.x - plot.left(), w),
				device_display::y_to_db((p.y - plot.top()).min(h), range, h),
			)
		});
		if response.double_clicked()
			&& hovered_node.is_none()
			&& let Some((freq, db)) = hover
		{
			events.push(FreqEvent::Create { freq, db });
		}

		if ui.is_rect_visible(rect) {
			let p = ui.painter().with_clip_rect(rect);
			paint_display(&p, rect);
			let p = p.with_clip_rect(plot);
			// Grid: unlabelled 1-2-3 lines, labelled 50/100/200/500 per decade, dB lines.
			for &f in &device_display::FREQ_GRID {
				let label = device_display::freq_label(f);
				let color = if label.is_some() {
					GRID_MAJOR
				} else {
					GRID_MINOR
				};
				p.vline(x_of(f), plot.y_range(), Stroke::new(1.0, color));
			}
			for db in range.lines() {
				let color = if db == 0.0 { GRID_ZERO } else { GRID_MAJOR };
				p.hline(plot.x_range(), y_of(db), Stroke::new(1.0, color));
			}
			// Analyzers sit under the curves; their own dBFS scale spans the plot.
			let analyzer = |bins: &[f32]| -> Vec<Pos2> {
				bins.iter()
					.enumerate()
					.map(|(i, &db)| {
						let t = (db / device_display::ANALYZER_FLOOR_DB).clamp(0.0, 1.0);
						pos2(x_of(device_display::analyzer_freq(i)), plot.top() + t * h)
					})
					.collect()
			};
			if let Some(bins) = self.pre {
				p.add(fill_to(&analyzer(bins), plot.bottom(), ANALYZER_PRE));
			}
			if let Some(bins) = self.reference {
				p.add(Shape::line(
					analyzer(bins),
					Stroke::new(1.5, theme::SOLO.gamma_multiply(0.75)),
				));
			}
			if let Some(bins) = self.post {
				let pts = analyzer(bins);
				p.add(fill_to(
					&pts,
					plot.bottom(),
					theme::ACCENT.gamma_multiply(0.10),
				));
				p.add(Shape::line(
					pts,
					Stroke::new(1.0, theme::ACCENT.gamma_multiply(0.6)),
				));
			}
			for (bins, color) in self.spectra {
				p.add(Shape::line(
					analyzer(bins),
					Stroke::new(1.25, color.gamma_multiply(0.9)),
				));
			}
			for band in self.heat {
				let (x0, x1) = (
					x_of(band.low).max(plot.left()),
					x_of(band.high).min(plot.right()),
				);
				if x1 <= x0 {
					continue;
				}
				let heat = band.heat.clamp(0.0, 1.0);
				let column = Rect::from_x_y_ranges(x0..=x1, plot.y_range());
				let boost = if band.hot { 1.6 } else { 1.0 };
				p.rect_filled(
					column,
					0.0,
					theme::ERROR.gamma_multiply((0.02 + 0.12 * heat * heat) * boost),
				);
				let cell = Rect::from_x_y_ranges(
					x0 + 1.0..=x1 - 1.0,
					plot.bottom() - HEAT_STRIP_H..=plot.bottom() - 1.0,
				);
				p.rect_filled(
					cell,
					CornerRadius::same(2),
					theme::ERROR.gamma_multiply((0.15 + 0.75 * heat * heat).min(1.0)),
				);
				if band.hot {
					p.rect_stroke(
						column,
						0.0,
						Stroke::new(1.0, theme::ERROR.gamma_multiply(0.7)),
						StrokeKind::Inside,
					);
				}
			}

			let zero = y_of(0.0);
			let curve = |f: &dyn Fn(f32) -> f32, n: usize| -> Vec<Pos2> {
				(0..n)
					.map(|i| {
						let x = plot.left() + w * i as f32 / (n - 1) as f32;
						let db = f(device_display::x_to_freq(x - plot.left(), w));
						pos2(x, y_of(db).clamp(plot.top() - 40.0, plot.bottom() + 40.0))
					})
					.collect()
			};
			for band in self.bands {
				let pts = curve(band.response, 120);
				let fill = if band.selected { 0.2 } else { 0.07 };
				p.add(fill_to(&pts, zero, band.color.gamma_multiply(fill)));
				if band.selected {
					// Outlined only where the band acts, not along the 0 dB line.
					let stroke = Stroke::new(1.0, band.color.gamma_multiply(0.85));
					for run in pts
						.split(|q| (q.y - zero).abs() < 2.0)
						.filter(|r| r.len() > 1)
					{
						p.add(Shape::line(run.to_vec(), stroke));
					}
				}
			}
			if let Some(b) = self.bracket {
				let y = y_of(b.db);
				let (x0, x1) = (x_of(b.low), x_of(b.high));
				let stroke = Stroke::new(1.5, b.color.gamma_multiply(0.9));
				p.line_segment([pos2(x0, y), pos2(x1, y)], stroke);
				for x in [x0, x1] {
					p.line_segment([pos2(x, y - 4.0), pos2(x, y + 4.0)], stroke);
				}
			}
			if let Some(f) = self.before {
				p.extend(Shape::dashed_line(
					&curve(f, 240),
					Stroke::new(1.25, theme::TEXT_DIM.gamma_multiply(0.8)),
					5.0,
					4.0,
				));
			}
			if let Some(f) = self.ghost {
				p.add(Shape::line(
					curve(f, 240),
					Stroke::new(1.0, theme::TEXT_DIM.gamma_multiply(0.55)),
				));
			}
			if let Some((f, color)) = self.sum {
				let pts = curve(f, 240);
				if self.sum_fill > 0.0 {
					p.add(fill_to(
						&pts,
						plot.bottom(),
						color.gamma_multiply(self.sum_fill),
					));
				}
				let tag_at = pts.last().copied();
				glow_line_wide(&p, pts, color);
				let tag = |at: Option<Pos2>, text: &str, color: Color32| {
					if let Some(at) = at {
						p.text(
							pos2(
								plot.right() - 6.0,
								at.y.clamp(plot.top() + 8.0, plot.bottom() - 8.0) - 3.0,
							),
							Align2::RIGHT_BOTTOM,
							text,
							theme::semibold(10.0),
							color,
						);
					}
				};
				if let Some((f2, color2)) = self.sum2 {
					let pts2 = curve(f2, 240);
					let tag2 = pts2.last().copied();
					glow_line_wide(&p, pts2, color2);
					if let Some([a, b]) = self.sum_tags {
						tag(tag_at, a, color);
						tag(tag2.map(|t| t + vec2(0.0, 12.0)), b, color2);
					}
				} else if let Some([a, _]) = self.sum_tags {
					tag(tag_at, a, color);
				}
			}
			if let Some(f) = self.proposal {
				let pts = curve(f, 240);
				p.extend(Shape::dashed_line(
					&pts,
					Stroke::new(1.75, theme::ACCENT),
					6.0,
					4.0,
				));
			}

			let label_font = theme::mono(9.5);
			for &f in &device_display::FREQ_GRID {
				if let Some(label) = device_display::freq_label(f) {
					p.text(
						pos2(x_of(f) + 3.0, plot.bottom() - 3.0),
						Align2::LEFT_BOTTOM,
						label,
						label_font.clone(),
						theme::TEXT_FAINT,
					);
				}
			}
			for db in range.lines() {
				if db == range.max || db == range.min {
					continue;
				}
				let text = if db > 0.0 {
					format!("+{db:.0}")
				} else {
					format!("{db:.0}")
				};
				p.text(
					pos2(plot.left() + 4.0, y_of(db) - 1.0),
					Align2::LEFT_BOTTOM,
					text,
					label_font.clone(),
					theme::TEXT_FAINT,
				);
			}

			if let Some(at) = pointer {
				p.vline(
					at.x,
					plot.y_range(),
					Stroke::new(1.0, Color32::from_white_alpha(28)),
				);
			}

			// Nodes: filled with a dark label, hollow when dormant; ring when selected.
			// The label font scales down with the node so it still fits inside it.
			let node_font = theme::semibold(11.0 * node_radius / NODE_RADIUS);
			for node in self.nodes {
				let Some((live, activity)) = node.dynamic else {
					continue;
				};
				let at = node_at(node);
				let clamp_y = |db: f32| y_of(db.clamp(range.min, range.max));
				p.extend(Shape::dashed_line(
					&[pos2(at.x, zero), pos2(at.x, clamp_y(node.db))],
					Stroke::new(1.5, node.color.gamma_multiply(0.55)),
					3.0,
					3.0,
				));
				let live_y = clamp_y(live);
				p.line_segment(
					[pos2(at.x, zero), pos2(at.x, live_y)],
					Stroke::new(3.0, node.color.gamma_multiply(0.9)),
				);
				p.line_segment(
					[pos2(at.x - 7.0, live_y), pos2(at.x + 7.0, live_y)],
					Stroke::new(2.0, theme::mix(node.color, Color32::WHITE, 0.4)),
				);
				if activity > 0.01 {
					p.circle_filled(
						at,
						node_radius + 4.0 + 8.0 * activity,
						theme::GAIN_REDUCTION.gamma_multiply(0.10 + 0.22 * activity),
					);
				}
			}
			for (i, node) in self.nodes.iter().enumerate() {
				let at = node_at(node);
				let active = dragging == Some(i);
				let lit = active || hovered_node == Some(i);
				if (active || self.selected == Some(i)) && !node.dormant {
					p.circle_filled(at, node_radius + 7.0, node.color.gamma_multiply(0.2));
				}
				if node.dormant && !active {
					p.circle(
						at,
						node_radius - 0.75,
						theme::DISPLAY_BG.gamma_multiply(0.8),
						Stroke::new(1.5, node.color),
					);
					p.text(
						at,
						Align2::CENTER_CENTER,
						node.label,
						node_font.clone(),
						node.color,
					);
				} else {
					let fill = if lit {
						theme::mix(node.color, Color32::WHITE, 0.25)
					} else {
						node.color
					};
					p.circle_filled(at, node_radius, fill);
					p.text(
						at,
						Align2::CENTER_CENTER,
						node.label,
						node_font.clone(),
						theme::TEXT_ON_COLOR,
					);
				}
				if active || self.selected == Some(i) {
					p.circle_stroke(at, node_radius + 2.5, Stroke::new(1.5, Color32::WHITE));
				}
				if let Some((_, activity)) = node.dynamic {
					// Dashed ring: a dynamic band; it lights up with its activity.
					let ring = node_radius + 5.0;
					let color = theme::mix(node.color, theme::GAIN_REDUCTION, activity)
						.gamma_multiply(0.45 + 0.55 * activity);
					for k in 0..12 {
						let a0 = k as f32 / 12.0 * std::f32::consts::TAU;
						let a1 = a0 + std::f32::consts::TAU / 24.0;
						let pts: Vec<Pos2> = (0..=4)
							.map(|j| {
								let a = a0 + (a1 - a0) * j as f32 / 4.0;
								at + vec2(a.cos(), a.sin()) * ring
							})
							.collect();
						p.add(Shape::line(pts, Stroke::new(1.75, color)));
					}
				}
				if node.ai {
					p.circle_stroke(at, node_radius + 5.0, Stroke::new(2.0, theme::ACCENT));
				}
				if let Some(Some(badge)) = self.badges.get(i) {
					let r = (node_radius * 0.32).max(5.5);
					let c = at + vec2(node_radius * 0.78, -node_radius * 0.78);
					p.circle(c, r, theme::DISPLAY_BG, Stroke::new(1.25, node.color));
					p.text(
						c,
						Align2::CENTER_CENTER,
						*badge,
						theme::semibold(r * 1.45),
						node.color,
					);
				}
			}

			// The note under the pointer (or the dragged node).
			let key_color = dragging
				.or(hovered_node)
				.or(self.selected)
				.and_then(|i| self.nodes.get(i))
				.map_or(theme::ACCENT, |n| n.color);
			let lit_freq = drag.map(|d| d.last.0).or(hover.map(|h| h.0));
			if let Some(strip) = strip {
				paint_piano(ui.painter(), strip, lit_freq, key_color);
			}
			if let Some(f) = lit_freq {
				let x = x_of(f);
				let text = device_display::note_hint(f);
				let galley = p.layout_no_wrap(text, theme::mono(10.5), theme::TEXT);
				let size = galley.size() + vec2(10.0, 4.0);
				let left = if x + 8.0 + size.x > plot.right() {
					x - 8.0 - size.x
				} else {
					x + 8.0
				};
				let r = Rect::from_min_size(pos2(left, plot.top() + 6.0), size);
				p.rect_filled(r, CornerRadius::same(theme::RADIUS), theme::BG_HOVER);
				p.galley(r.min + vec2(5.0, 2.0), galley, theme::TEXT);
			}
		}
		if let Some(d) = drag
			&& let Some(node) = self.nodes.get(d.node)
		{
			let at = node_at(node);
			floating_value(
				&ctx,
				self.id.with("node-value"),
				Rect::from_center_size(at, Vec2::splat(node_radius * 2.0)),
				&if node.x_only {
					device::format_value(d.last.0, tr("Hz"))
				} else {
					format!(
						"{} · {}",
						device::format_value(d.last.0, "Hz"),
						device::format_value(d.last.1, "dB")
					)
				},
			);
		}
		FreqResponse {
			events,
			hover,
			hovered_node,
			dragging,
			response,
			node_responses,
			plot,
		}
	}
}

const ANALYZER_PRE: Color32 = Color32::from_rgb(0x26, 0x29, 0x30);

/// The piano strip of a frequency display: one semitone column per key on the log axis,
/// the key of `lit` in `color` (one mesh plus the lit key).
pub fn paint_piano(p: &Painter, strip: Rect, lit: Option<f32>, color: Color32) {
	let w = strip.width();
	let white = Color32::from_rgb(0x3a, 0x3d, 0x44);
	let black = Color32::from_rgb(0x12, 0x13, 0x16);
	let gap = Color32::from_rgb(0x1a, 0x1b, 0x1f);
	let mut mesh = egui::Mesh::default();
	add_rect(&mut mesh, strip, white);
	let key = |n: i32| {
		let x0 = strip.left()
			+ device_display::freq_to_x(device_display::midi_to_freq(n as f32 - 0.5), w);
		let x1 = strip.left()
			+ device_display::freq_to_x(device_display::midi_to_freq(n as f32 + 0.5), w);
		(x0.max(strip.left()), x1.min(strip.right()))
	};
	let black_bottom = strip.top() + strip.height() * 0.62;
	let first = device_display::freq_to_midi(device_display::FREQ_MIN).floor() as i32;
	let last = device_display::freq_to_midi(device_display::FREQ_MAX).ceil() as i32;
	for n in first..=last {
		let (x0, x1) = key(n);
		if device_display::is_black_key(n) {
			add_rect(
				&mut mesh,
				Rect::from_min_max(pos2(x0, strip.top()), pos2(x1, black_bottom)),
				black,
			);
		} else if matches!(n.rem_euclid(12), 0 | 5) {
			// White keys meet only at E-F and B-C.
			add_rect(
				&mut mesh,
				Rect::from_min_max(pos2(x0, strip.top()), pos2(x0 + 1.0, strip.bottom())),
				gap,
			);
		}
	}
	p.add(Shape::mesh(mesh));
	if let Some(f) = lit {
		let n = device_display::freq_to_midi(f).round() as i32;
		let (x0, x1) = key(n);
		let bottom = if device_display::is_black_key(n) {
			black_bottom
		} else {
			strip.bottom()
		};
		p.rect_filled(
			Rect::from_min_max(pos2(x0, strip.top()), pos2(x1.max(x0 + 2.0), bottom)),
			0.0,
			color,
		);
	}
	p.hline(
		strip.x_range(),
		strip.top(),
		Stroke::new(1.0, theme::BORDER_SUBTLE),
	);
}

// --- Level history --------------------------------------------------------------------

/// A draggable horizontal line on a level history (threshold, ceiling): dashed, with a
/// "⇕ Threshold −24.0 dB" tag near the right edge that drags it too.
#[derive(Debug, Clone, Copy)]
pub struct HistoryLine<'a> {
	pub db: f32,
	pub default: f32,
	pub label: &'a str,
	pub color: Color32,
}

/// The sidechain key trace: a thin line in the key track's colour.
#[derive(Debug, Clone, Copy)]
pub struct HistoryKey<'a> {
	pub color: Color32,
	/// Legend name ("Kick").
	pub name: &'a str,
}

/// The scrolling level display of dynamics devices: input level as one filled polygon,
/// output as a line, gain reduction in coral hanging from the top, an optional key
/// trace, newest column at the right edge. `stopped` freezes and dims it to 50 %.
#[must_use = "call `show`"]
pub struct LevelHistoryView<'a> {
	id: Id,
	size: Vec2,
	min_db: f32,
	max_db: f32,
	gr_range_db: f32,
	input: bool,
	output: bool,
	gr: bool,
	key: Option<HistoryKey<'a>>,
	line: Option<HistoryLine<'a>>,
	stopped: bool,
	legend: bool,
	gr_labels: bool,
	labels: bool,
	time_label: Option<&'a str>,
	wave_max_db: Option<f32>,
	input_label: &'a str,
	body_drag: bool,
	wheel: bool,
}

/// What [`LevelHistoryView::show_full`] reports.
pub struct HistoryResponse {
	/// The line's drag (dB; Shift fine, double-click resets).
	pub line: Gesture,
	/// A vertical drag on the history outside the line: points moved since the press
	/// (down positive, Shift fine), committed on release.
	pub body: Gesture,
	/// The history was double-clicked outside the line.
	pub body_reset: bool,
	/// Wheel notches over the history (with [`LevelHistoryView::wheel`]).
	pub wheel: f32,
	pub response: Response,
}

impl<'a> LevelHistoryView<'a> {
	pub fn new(id: Id, size: Vec2) -> Self {
		Self {
			id,
			size,
			min_db: -48.0,
			max_db: 0.0,
			gr_range_db: 24.0,
			input: true,
			output: true,
			gr: true,
			key: None,
			line: None,
			stopped: false,
			legend: false,
			gr_labels: false,
			labels: true,
			time_label: None,
			wave_max_db: None,
			input_label: tr("Input"),
			body_drag: false,
			wheel: false,
		}
	}

	/// A waveform instead of levels: linear amplitude mirrored around the centre line,
	/// `max_db` at the edges, each column's min/max filled. The line is drawn on both
	/// halves; `range` only bounds its drag.
	pub fn waveform(mut self, max_db: f32) -> Self {
		self.wave_max_db = Some(max_db);
		self
	}

	/// The input trace's legend name ("Input + gain").
	pub fn input_label(mut self, label: &'a str) -> Self {
		self.input_label = label;
		self
	}

	/// Vertical drags on the history itself ([`HistoryResponse::body`]).
	pub fn body_drag(mut self, on: bool) -> Self {
		self.body_drag = on;
		self
	}

	/// Reports (and consumes) wheel notches over the history.
	pub fn wheel(mut self, on: bool) -> Self {
		self.wheel = on;
		self
	}

	/// The level scale (dBFS, bottom to top).
	pub fn range(mut self, min_db: f32, max_db: f32) -> Self {
		self.min_db = min_db;
		self.max_db = max_db.max(min_db + 1.0);
		self
	}

	/// Gain reduction that reaches the bottom.
	pub fn gr_range(mut self, db: f32) -> Self {
		self.gr_range_db = db.max(1.0);
		self
	}

	/// Which traces to draw: input fill, output line, gain reduction.
	pub fn traces(mut self, input: bool, output: bool, gr: bool) -> Self {
		self.input = input;
		self.output = output;
		self.gr = gr;
		self
	}

	/// Draws the columns' key levels.
	pub fn key(mut self, key: Option<HistoryKey<'a>>) -> Self {
		self.key = key;
		self
	}

	pub fn line(mut self, line: Option<HistoryLine<'a>>) -> Self {
		self.line = line;
		self
	}

	pub fn stopped(mut self, stopped: bool) -> Self {
		self.stopped = stopped;
		self
	}

	/// The trace legend (top left) and the gain reduction scale (left, coral).
	pub fn legend(mut self, legend: bool, gr_labels: bool) -> Self {
		self.legend = legend;
		self.gr_labels = gr_labels;
		self
	}

	/// Small strips: no dB scale, and the line without its tag.
	pub fn bare(mut self) -> Self {
		self.labels = false;
		self
	}

	/// A caption in the bottom right corner ("4 s · now →").
	pub fn time_label(mut self, label: Option<&'a str>) -> Self {
		self.time_label = label;
		self
	}

	/// The dB labels of the scale: every 6 dB down to −24, then every 12.
	fn scale(&self) -> Vec<f32> {
		let mut out = Vec::new();
		let mut db = self.max_db;
		loop {
			db -= if db > -24.0 { 6.0 } else { 12.0 };
			if db <= self.min_db + 1.0 {
				break;
			}
			out.push(db);
		}
		out
	}

	/// Returns the line's drag (dB; Shift fine, double-click resets) and the response.
	pub fn show(self, ui: &mut Ui, history: &LevelHistory) -> (Gesture, Response) {
		let r = self.show_full(ui, history);
		(r.line, r.response)
	}

	pub fn show_full(self, ui: &mut Ui, history: &LevelHistory) -> HistoryResponse {
		let sense = if self.body_drag {
			Sense::click_and_drag()
		} else {
			Sense::hover()
		};
		let (rect, response) = ui.allocate_exact_size(self.size, sense);
		let plot = rect.shrink(1.0);
		let (lo, hi) = (self.min_db, self.max_db);
		let wave = self.wave_max_db;
		let center = plot.center().y;
		let half = plot.height() * 0.5 * 0.96;
		let y_of = |db: f32| match wave {
			Some(max) => center - device_display::wave_fraction(db, max) * half,
			None => plot.top() + ((hi - db) / (hi - lo)).clamp(0.0, 1.0) * plot.height(),
		};
		let db_of = |y: f32| match wave {
			Some(max) => device_display::wave_db((center - y) / half.max(1.0), max),
			None => hi - (y - plot.top()) / plot.height().max(1.0) * (hi - lo),
		};
		// A signed sample on the waveform.
		let amp_y = |a: f32| match wave {
			Some(max) => center - a / 10f32.powf(max / 20.0) * half,
			None => center,
		};
		let tag_rect = |ui: &Ui, line: &HistoryLine, db: f32| {
			let text = history_tag(line.label, db);
			let w = ui
				.painter()
				.layout_no_wrap(text, theme::mono(11.0), theme::TEXT)
				.size()
				.x + 34.0;
			let left = (plot.right() - w - 150.0).max(plot.left() + 40.0);
			Rect::from_min_size(pos2(left, y_of(db) - 10.0), vec2(w, 20.0))
		};
		let ctx = ui.ctx().clone();
		let fine = if ui.input(|i| i.modifiers.shift) {
			FINE
		} else {
			1.0
		};
		let mut body = Gesture::Idle;
		let mut body_reset = false;
		let mut wheel = 0.0;
		if self.body_drag {
			let drag_id = self.id.with("body-drag");
			if response.drag_started() {
				ctx.data_mut(|d| d.insert_temp(drag_id, 0.0f32));
			}
			if response.dragged() {
				let dy = ctx.data(|d| d.get_temp::<f32>(drag_id)).unwrap_or(0.0)
					+ response.drag_delta().y * fine;
				ctx.data_mut(|d| d.insert_temp(drag_id, dy));
				body = Gesture::Preview(dy);
			}
			if response.drag_stopped() {
				let dy = ctx
					.data_mut(|d| d.remove_temp::<f32>(drag_id))
					.unwrap_or(0.0);
				body = Gesture::Commit(dy);
			}
			body_reset = response.double_clicked();
		}
		if self.wheel && response.hovered() && !response.dragged() {
			let scroll = ui.input(|i| i.smooth_scroll_delta.x + i.smooth_scroll_delta.y);
			if scroll != 0.0 {
				ui.input_mut(|i| i.smooth_scroll_delta = Vec2::ZERO);
				wheel = scroll / NOTCH * fine;
			}
		}
		let response = if self.body_drag {
			response.on_hover_cursor(CursorIcon::ResizeVertical)
		} else {
			response
		};
		let mut gesture = Gesture::Idle;
		let mut line_db = self.line.map(|l| l.db);
		if let Some(line) = self.line {
			let y = y_of(line.db);
			let grab = Rect::from_x_y_ranges(plot.x_range(), y - 4.0..=y + 4.0)
				.union(tag_rect(ui, &line, line.db));
			let r = ui.interact(grab, self.id.with("line"), Sense::click_and_drag());
			r.widget_info(|| egui::WidgetInfo::slider(true, f64::from(line.db), line.label));
			let mut r = r.on_hover_cursor(CursorIcon::ResizeVertical);
			// The lower copy of a waveform line drags mirrored.
			let mut below = false;
			if wave.is_some() {
				let mirror = 2.0 * center - y;
				let m = ui.interact(
					Rect::from_x_y_ranges(plot.x_range(), mirror - 4.0..=mirror + 4.0),
					self.id.with("line-mirror"),
					Sense::click_and_drag(),
				);
				let m = m.on_hover_cursor(CursorIcon::ResizeVertical);
				if m.dragged() || m.drag_stopped() || m.double_clicked() {
					below = true;
					r = m;
				}
			}
			let drag_id = self.id.with("line-drag");
			if r.drag_started() {
				ctx.data_mut(|d| d.insert_temp(drag_id, line.db));
			}
			if r.dragged() {
				let start = ctx.data(|d| d.get_temp::<f32>(drag_id)).unwrap_or(line.db);
				let dy = r.drag_delta().y * fine * if below { -1.0 } else { 1.0 };
				let db = db_of(y_of(start) + dy).clamp(lo, hi);
				ctx.data_mut(|d| d.insert_temp(drag_id, db));
				gesture = Gesture::Preview(db);
			}
			if r.drag_stopped() {
				let db = ctx
					.data_mut(|d| d.remove_temp::<f32>(drag_id))
					.unwrap_or(line.db);
				gesture = Gesture::Commit(db);
			}
			if r.double_clicked() {
				gesture = Gesture::Commit(line.default);
			}
			if let Gesture::Preview(v) | Gesture::Commit(v) = gesture {
				line_db = Some(v);
			}
		}

		if ui.is_rect_visible(rect) {
			let base = ui.painter().with_clip_rect(rect);
			paint_display(&base, rect);
			let mut p = base.with_clip_rect(plot);
			let scale = if wave.is_some() {
				Vec::new()
			} else {
				self.scale()
			};
			if wave.is_some() {
				p.hline(plot.x_range(), center, Stroke::new(1.0, GRID_MAJOR));
				for a in [-1.0, 1.0] {
					p.hline(plot.x_range(), amp_y(a), Stroke::new(1.0, GRID_MINOR));
				}
			} else {
				let mut db = hi - 6.0;
				while db > lo {
					p.hline(plot.x_range(), y_of(db), Stroke::new(1.0, GRID_MINOR));
					db -= 6.0;
				}
			}
			for &db in &scale {
				p.hline(plot.x_range(), y_of(db), Stroke::new(1.0, GRID_MAJOR));
			}
			let range = self.gr_range_db;
			let gr_y = |gr: f32| plot.top() + (gr / range).clamp(0.0, 1.0) * plot.height();
			if self.stopped {
				p.multiply_opacity(0.5);
			}
			let cw = plot.width() / history.capacity() as f32;
			let x_of = |age: usize| plot.right() - (age as f32 + 0.5) * cw;
			// Newest last in the ring: age 0 at the right edge.
			let n = history.len();
			let points = |f: &dyn Fn(&device_display::HistoryColumn) -> f32| -> Vec<Pos2> {
				history
					.columns()
					.enumerate()
					.map(|(i, c)| pos2(x_of(n - 1 - i), f(c)))
					.collect()
			};
			if n >= 2 && wave.is_some() {
				let band = |side: fn(&device_display::HistoryColumn) -> (f32, f32)| {
					let top = points(&|c| amp_y(side(c).1));
					let bottom = points(&|c| amp_y(side(c).0));
					(top, bottom)
				};
				if self.input {
					let (top, bottom) = band(|c| c.input);
					p.add(fill_between(&top, &bottom, HISTORY_WAVE_INPUT));
				}
				if self.output {
					let (top, bottom) = band(|c| c.output);
					p.add(fill_between(
						&top,
						&bottom,
						theme::LEVEL_OUTPUT.gamma_multiply(0.32),
					));
					let stroke = Stroke::new(1.0, theme::LEVEL_OUTPUT.gamma_multiply(0.8));
					p.add(Shape::line(top, stroke));
					p.add(Shape::line(bottom, stroke));
				}
			}
			if n >= 2 {
				if self.input && wave.is_none() {
					let pts = points(&|c| y_of(device_display::HistoryColumn::peak_db(c.input)));
					p.add(fill_to(&pts, plot.bottom(), HISTORY_INPUT));
				}
				if self.output && wave.is_none() {
					let pts = points(&|c| y_of(device_display::HistoryColumn::peak_db(c.output)));
					p.add(fill_to(
						&pts,
						plot.bottom(),
						theme::LEVEL_OUTPUT.gamma_multiply(0.13),
					));
					p.add(Shape::line(
						pts,
						Stroke::new(1.25, theme::LEVEL_OUTPUT.gamma_multiply(0.85)),
					));
				}
				if let Some(key) = self.key {
					let pts = points(&|c| y_of(c.key_db));
					p.add(Shape::line(
						pts,
						Stroke::new(1.0, key.color.gamma_multiply(0.6)),
					));
				}
				if self.gr {
					let pts = points(&|c| gr_y(c.gr_db));
					p.add(fill_to(
						&pts,
						plot.top(),
						theme::GAIN_REDUCTION.gamma_multiply(0.16),
					));
					p.add(Shape::line(
						pts.clone(),
						Stroke::new(5.0, theme::GAIN_REDUCTION.gamma_multiply(0.18)),
					));
					p.add(Shape::line(pts, Stroke::new(1.6, theme::GAIN_REDUCTION)));
				}
			}
			// Scales over the traces, at full strength.
			let labels = base.with_clip_rect(plot);
			for &db in scale.iter().filter(|_| self.labels) {
				labels.text(
					pos2(plot.right() - 6.0, y_of(db)),
					Align2::RIGHT_CENTER,
					device::minus(format!("{db:.0}")),
					theme::mono(10.5),
					theme::TEXT_FAINT,
				);
			}
			if self.gr && self.gr_labels {
				let steps = if range < 40.0 {
					[3.0, 6.0, 9.0]
				} else {
					[6.0, 12.0, 18.0]
				};
				for gr in steps {
					if gr < range {
						labels.text(
							pos2(plot.left() + 6.0, gr_y(gr)),
							Align2::LEFT_CENTER,
							device::minus(format!("-{gr:.0}")),
							theme::mono(10.5),
							theme::GAIN_REDUCTION_TEXT,
						);
					}
				}
			}
			if self.legend {
				legend(
					&base,
					plot,
					(self.input, self.input_label),
					self.output,
					self.gr,
					self.key,
				);
			}
			if let Some(text) = self.time_label {
				base.text(
					plot.right_bottom() + vec2(-8.0, -6.0),
					Align2::RIGHT_BOTTOM,
					text,
					theme::medium(10.5),
					theme::TEXT_FAINT,
				);
			}
			if let (Some(line), Some(db)) = (self.line, line_db) {
				let y = y_of(db);
				let lp = base.with_clip_rect(plot);
				let ys: &[f32] = if wave.is_some() {
					&[y, 2.0 * center - y]
				} else {
					&[y]
				};
				for &y in ys {
					lp.add(Shape::dashed_line(
						&[pos2(plot.left(), y), pos2(plot.right(), y)],
						Stroke::new(1.0, line.color.gamma_multiply(0.7)),
						6.0,
						4.0,
					));
				}
				if !self.labels {
					return HistoryResponse {
						line: gesture,
						body,
						body_reset,
						wheel,
						response,
					};
				}
				let tag = tag_rect(ui, &line, db);
				let tag = tag.translate(vec2(
					0.0,
					(plot.top() + 1.0 - tag.top()).max(0.0)
						+ (plot.bottom() - 1.0 - tag.bottom()).min(0.0),
				));
				lp.rect(
					tag,
					CornerRadius::same(theme::RADIUS),
					theme::BG_WIDGET,
					Stroke::new(1.0, theme::BG_PRESSED),
					StrokeKind::Inside,
				);
				lp.text(
					pos2(tag.left() + 8.0, tag.center().y),
					Align2::LEFT_CENTER,
					"⇕",
					theme::medium(12.0),
					theme::TEXT_FAINT,
				);
				lp.text(
					pos2(tag.left() + 22.0, tag.center().y),
					Align2::LEFT_CENTER,
					history_tag(line.label, db),
					theme::mono(11.0),
					theme::TEXT,
				);
			}
		}
		HistoryResponse {
			line: gesture,
			body,
			body_reset,
			wheel,
			response,
		}
	}
}

/// "Threshold −24.0 dB".
fn history_tag(label: &str, db: f32) -> String {
	device::minus(format!("{label} {db:.1} dB"))
}

/// Input · Output · Gain reduction · Key · <name>, top left.
fn legend(
	p: &Painter,
	plot: Rect,
	(input, input_label): (bool, &str),
	output: bool,
	gr: bool,
	key: Option<HistoryKey>,
) {
	let font = theme::medium(10.5);
	let mut x = plot.left() + 44.0;
	let y = plot.top() + 14.0;
	let key_text = key.map(|k| trf("Key · {name}", &[("name", &k.name)]));
	let items = [
		(input, input_label, HISTORY_INPUT_SWATCH, 6.0),
		(output, tr("Output"), theme::LEVEL_OUTPUT, 2.0),
		(gr, tr("Gain reduction"), theme::GAIN_REDUCTION, 2.0),
	];
	let mut entries: Vec<(&str, Color32, f32)> = items
		.iter()
		.filter(|i| i.0)
		.map(|i| (i.1, i.2, i.3))
		.collect();
	if let (Some(k), Some(t)) = (key, key_text.as_deref()) {
		entries.push((t, k.color, 1.0));
	}
	for (text, color, h) in entries {
		p.rect_filled(
			Rect::from_center_size(pos2(x + 5.0, y), vec2(10.0, h)),
			0.0,
			color,
		);
		let g = p.layout_no_wrap(text.to_owned(), font.clone(), theme::TEXT_DIM);
		let w = g.size().x;
		p.galley(pos2(x + 15.0, y - g.size().y * 0.5), g, theme::TEXT_DIM);
		x += 15.0 + w + 14.0;
	}
}

const HISTORY_INPUT_SWATCH: Color32 = Color32::from_rgb(0x3a, 0x3e, 0x46);
const HISTORY_INPUT: Color32 = Color32::from_rgb(0x2c, 0x2f, 0x36);
/// Design 16a: #4a4f59 at 55 % over the display.
const HISTORY_WAVE_INPUT: Color32 = Color32::from_rgba_premultiplied(0x29, 0x2b, 0x31, 0x8c);

/// A filled band between two polylines of the same length (one mesh).
pub fn fill_between(top: &[Pos2], bottom: &[Pos2], color: Color32) -> Shape {
	let mut mesh = egui::Mesh::default();
	for (i, (a, b)) in top.iter().zip(bottom).enumerate() {
		mesh.colored_vertex(*a, color);
		mesh.colored_vertex(*b, color);
		if i > 0 {
			let k = (i * 2) as u32;
			mesh.add_triangle(k - 2, k - 1, k);
			mesh.add_triangle(k - 1, k + 1, k);
		}
	}
	Shape::mesh(mesh)
}

// --- Gallery --------------------------------------------------------------------------

/// A section of the gallery: a raised card with a title.
fn section(ui: &mut Ui, title: &str, width: f32, add: impl FnOnce(&mut Ui)) {
	egui::Frame::new()
		.fill(theme::BG_PANEL)
		.stroke(Stroke::new(1.0, theme::BORDER))
		.corner_radius(CornerRadius::same(theme::RADIUS_XL))
		.inner_margin(egui::Margin::same(16))
		.show(ui, |ui| {
			ui.vertical(|ui| {
				ui.set_width(width);
				ui.label(
					egui::RichText::new(title)
						.font(theme::heading())
						.color(theme::TEXT),
				);
				ui.add_space(4.0);
				add(ui);
			});
		});
}

/// A gallery value kept in egui memory, edited by the demo controls.
fn demo_value(ui: &Ui, key: &str, initial: f32) -> (Id, f32) {
	let id = Id::new(("device-kit-demo", key));
	(id, ui.data(|d| d.get_temp::<f32>(id)).unwrap_or(initial))
}

fn demo_knob(ui: &mut Ui, key: &str, initial: f32, knob: impl FnOnce(Id, f32) -> Knob<'static>) {
	let (id, value) = demo_value(ui, key, initial);
	let (gesture, _) = knob(id.with("knob"), value).show(ui);
	if let Gesture::Preview(v) | Gesture::Commit(v) = gesture {
		ui.data_mut(|d| d.insert_temp(id, v));
	}
}

fn caption(ui: &mut Ui, text: &str) {
	ui.add(
		egui::Label::new(
			egui::RichText::new(text)
				.font(theme::small())
				.color(theme::TEXT_FAINT),
		)
		.extend(),
	);
}

/// Deterministic noise for the gallery's synthetic signals.
fn demo_noise(seed: &mut u32) -> f32 {
	*seed ^= *seed << 13;
	*seed ^= *seed >> 17;
	*seed ^= *seed << 5;
	(*seed as f32 / u32::MAX as f32) * 2.0 - 1.0
}

const EQ_DEMO_RATE: f32 = 48_000.0;
const BAND_LABELS: [&str; 7] = ["1", "2", "3", "4", "5", "6", "7"];
/// Frequency / gain parameter of each EQ band (HP and LP have no gain).
const EQ_BAND_PARAMS: [(usize, Option<usize>); 7] = [
	(0, None),
	(2, Some(3)),
	(4, Some(5)),
	(7, Some(8)),
	(10, Some(11)),
	(13, Some(14)),
	(15, None),
];

fn eq_demo_params() -> Vec<f32> {
	let mut p: Vec<f32> = cadenza_dsp::device_info("eq")
		.map(|i| i.params.iter().map(|p| p.default).collect())
		.unwrap_or_default();
	for (i, v) in [
		(0, 40.0),
		(1, 2.0),
		(4, 280.0),
		(5, -4.5),
		(6, 1.2),
		(7, 2800.0),
		(8, 3.5),
		(9, 1.4),
		(14, 2.0),
	] {
		if let Some(slot) = p.get_mut(i) {
			*slot = v;
		}
	}
	p
}

/// Pink-ish noise with a few partials, as the analyzer would see a mix.
fn demo_spectrum() -> Vec<f32> {
	let mut seed = 0x9e37_79b9;
	let (mut b0, mut b1, mut b2) = (0.0f32, 0.0f32, 0.0f32);
	let samples: Vec<f32> = (0..device_display::FFT_MAX)
		.map(|i| {
			let white = demo_noise(&mut seed);
			b0 = 0.997 * b0 + 0.029 * white;
			b1 = 0.985 * b1 + 0.032 * white;
			b2 = 0.950 * b2 + 0.048 * white;
			let t = i as f32 / EQ_DEMO_RATE;
			let tone = [(55.0, 0.25), (110.0, 0.12), (440.0, 0.05), (2600.0, 0.02)]
				.iter()
				.map(|(f, a)| a * (std::f32::consts::TAU * f * t).sin())
				.sum::<f32>();
			((b0 + b1 + b2 + 0.02 * white) * 0.6 + tone) * 0.08
		})
		.collect();
	device_display::spectrum_bins(&samples, EQ_DEMO_RATE)
}

/// The EQ demo: the real `EqBands` response, band fills, nodes, analyzer, piano strip.
fn eq_demo(ui: &mut Ui, size: Vec2) {
	let id = Id::new("device-kit-demo-eq");
	let mut params: Vec<f32> = ui.data(|d| d.get_temp(id)).unwrap_or_else(eq_demo_params);
	let pre: Vec<f32> = ui.data_mut(|d| {
		d.get_temp_mut_or_insert_with(id.with("pre"), demo_spectrum)
			.clone()
	});
	let eq = cadenza_dsp::response::EqBands::new(&params, EQ_DEMO_RATE);
	let eq = &eq;
	let post: Vec<f32> = pre
		.iter()
		.enumerate()
		.map(|(i, db)| db + eq.response_db(device_display::analyzer_freq(i)))
		.collect();
	let defaults = eq_demo_params();
	let dormant = |b: usize| match EQ_BAND_PARAMS[b] {
		(_, Some(g)) => params[g].abs() < 0.05,
		(0, None) => params[1] < 0.5,
		_ => params[16] < 0.5,
	};
	let responses: Vec<Box<dyn Fn(f32) -> f32 + '_>> = (0..7)
		.map(|b| Box::new(move |f| eq.band_response_db(b, f)) as Box<dyn Fn(f32) -> f32>)
		.collect();
	let bands: Vec<BandCurve> = (0..7)
		.filter(|&b| !dormant(b))
		.map(|b| BandCurve {
			response: &*responses[b],
			color: theme::BAND_COLORS[b],
			selected: false,
		})
		.collect();
	let nodes: Vec<FreqNode> = (0..7)
		.map(|b| {
			let (f, g) = EQ_BAND_PARAMS[b];
			FreqNode {
				label: BAND_LABELS[b],
				freq: params[f],
				db: g.map_or(0.0, |g| params[g]),
				color: theme::BAND_COLORS[b],
				dormant: dormant(b),
				x_only: g.is_none(),
				ai: false,
				dynamic: None,
			}
		})
		.collect();
	let sum = |f: f32| eq.response_db(f);
	let (sel_id, selected) = demo_value(ui, "eq-selected", 3.0);
	let out = FreqDisplay::new(id.with("display"), size)
		.range(DbRange::PM12)
		.pre(Some(&pre))
		.post(Some(&post))
		.bands(&bands)
		.sum(&sum, theme::TEXT)
		.nodes(&nodes)
		.selected(Some(selected as usize))
		.piano(true)
		.show(ui);
	for e in out.events {
		match e {
			FreqEvent::Move { node, freq, db, .. } => {
				let (f, g) = EQ_BAND_PARAMS[node];
				params[f] = freq;
				if let Some(g) = g {
					params[g] = db;
				}
				ui.data_mut(|d| d.insert_temp(sel_id, node as f32));
			}
			FreqEvent::Click { node } => {
				ui.data_mut(|d| d.insert_temp(sel_id, node as f32));
			}
			FreqEvent::Reset { node } => match EQ_BAND_PARAMS[node] {
				(_, Some(g)) => params[g] = defaults[g],
				(0, None) => params[1] = if params[1] > 0.5 { 0.0 } else { 2.0 },
				_ => params[16] = if params[16] > 0.5 { 0.0 } else { 2.0 },
			},
			FreqEvent::Wheel { node, notches } => {
				let q = match node {
					2 => Some(6),
					3 => Some(9),
					4 => Some(12),
					_ => None,
				};
				if let Some(q) = q {
					params[q] = (params[q] * 1.1f32.powf(notches)).clamp(0.1, 18.0);
				}
			}
			FreqEvent::Create { .. } => {}
		}
	}
	ui.data_mut(|d| d.insert_temp(id, params));
}

/// The Poly Synth filter on the same component (filter range, one node).
fn filter_demo(ui: &mut Ui, size: Vec2) {
	let (id, cutoff) = demo_value(ui, "filter-cutoff", 1200.0);
	let (res_id, resonance) = demo_value(ui, "filter-res", 62.0);
	let response = |f: f32| {
		cadenza_dsp::response::poly_synth_filter_response_db(
			cutoff,
			resonance,
			true,
			EQ_DEMO_RATE,
			f,
		)
	};
	let nodes = [FreqNode {
		label: "F",
		freq: cutoff,
		db: response(cutoff),
		color: theme::BAND_COLORS[6],
		dormant: false,
		x_only: false,
		ai: false,
		dynamic: None,
	}];
	let out = FreqDisplay::new(id.with("display"), size)
		.range(DbRange::FILTER)
		.bands(&[])
		.sum(&response, theme::BAND_COLORS[6])
		.nodes(&nodes)
		.piano(true)
		.show(ui);
	for e in out.events {
		match e {
			FreqEvent::Move { freq, .. } => {
				ui.data_mut(|d| d.insert_temp(id, freq));
			}
			FreqEvent::Wheel { notches, .. } => {
				ui.data_mut(|d| {
					d.insert_temp(res_id, (resonance + notches * 2.0).clamp(0.0, 100.0));
				});
			}
			FreqEvent::Reset { .. } => ui.data_mut(|d| {
				d.insert_temp(id, 1200.0f32);
				d.insert_temp(res_id, 15.0f32);
			}),
			_ => {}
		}
	}
}

fn poly_synth_amp_ranges() -> AdsrRanges {
	let spec = |id: &str| {
		cadenza_dsp::device_info("poly_synth")
			.and_then(|i| i.params.iter().find(|p| p.id == id))
			.map_or(KnobSpec::new(0.0, 1000.0, 100.0), |p| {
				KnobSpec::new(p.min, p.max, p.default)
			})
	};
	AdsrRanges {
		attack: spec("amp_attack"),
		decay: spec("amp_decay"),
		sustain: spec("amp_sustain"),
		release: spec("amp_release"),
		sustain_unit: "%",
	}
}

fn adsr_demo(ui: &mut Ui, size: Vec2) {
	let id = Id::new("device-kit-demo-adsr");
	let ranges = poly_synth_amp_ranges();
	let mut values: AdsrValues = ui.data(|d| d.get_temp(id)).unwrap_or(AdsrValues {
		attack: 12.0,
		decay: 400.0,
		sustain: 62.0,
		release: 900.0,
	});
	let out = adsr(
		ui,
		id.with("editor"),
		values,
		&ranges,
		theme::BAND_COLORS[6],
		size,
		&[VoicePos::Held(150.0), VoicePos::Released(260.0)],
	);
	for stage in AdsrStage::ALL {
		if let Gesture::Preview(v) | Gesture::Commit(v) = out.gesture(stage) {
			values.set(stage, v);
		}
	}
	ui.data_mut(|d| d.insert_temp(id, values));
}

/// A drum loop through a 4:1 compressor at `threshold_db`, as a probe would feed it.
fn demo_history(threshold_db: f32) -> LevelHistory {
	let rate = 8000;
	let mut h = LevelHistory::new(240, 4.0, rate);
	let mut seed = 0x1234_5678;
	let mut env = 0.0f32;
	let total = rate as usize * 4;
	for i in 0..total {
		let t = i as f32 / rate as f32;
		let beat = (t * 4.0).fract();
		let accent = if ((t * 4.0) as usize).is_multiple_of(2) {
			0.5
		} else {
			0.25
		};
		let amp = accent * (-beat * 4.0).exp() + 0.04 + 0.03 * (t * 1.3).sin().abs();
		let s = amp * demo_noise(&mut seed);
		env = env.max(s.abs()) * 0.996;
		let level = 20.0 * env.max(1e-6).log10();
		let gr = ((level - threshold_db) * 0.75).max(0.0);
		let g = 10f32.powf(-gr / 20.0);
		h.push([s, s * 0.9], [s * g, s * g * 0.9], gr);
	}
	h
}

fn history_demo(ui: &mut Ui, width: f32) {
	let (id, threshold) = demo_value(ui, "threshold", -18.0);
	let key = id.with(("history", (threshold * 4.0).round() as i32));
	let history: LevelHistory = ui.data_mut(|d| {
		d.get_temp_mut_or_insert_with(key, || demo_history(threshold))
			.clone()
	});
	ui.horizontal_top(|ui| {
		let (gesture, _) = LevelHistoryView::new(id.with("live"), vec2(width * 0.62, 150.0))
			.line(Some(HistoryLine {
				db: threshold,
				default: -24.0,
				label: tr("Threshold"),
				color: theme::BAND_COLORS[2],
			}))
			.show(ui, &history);
		if let Gesture::Preview(v) | Gesture::Commit(v) = gesture {
			ui.data_mut(|d| d.insert_temp(id, v));
		}
		ui.vertical(|ui| {
			let _ = LevelHistoryView::new(id.with("stopped"), vec2(width * 0.38 - 16.0, 116.0))
				.stopped(true)
				.show(ui, &history);
			caption(ui, tr("Stopped: frozen, dimmed to 50 %"));
		});
	});
}

/// Every kit component on one page (screenshots; no device editor needed). The last
/// row binds knobs to the first built-in effect or instrument of the open project.
pub fn gallery(ui: &mut Ui, state: &mut AppState) {
	let col = 450.0;
	let pct = KnobSpec::new(0.0, 100.0, 50.0);
	ui.spacing_mut().item_spacing = vec2(16.0, 12.0);
	ui.horizontal_top(|ui| {
		section(ui, tr("Knob · sizes"), col, |ui| {
			ui.horizontal_top(|ui| {
				ui.spacing_mut().item_spacing.x = 10.0;
				for (key, size, label) in [
					("s", KnobSize::S, "S 40"),
					("m", KnobSize::M, "M 56"),
					("l", KnobSize::L, "L 72"),
					("xl", KnobSize::XL, "XL 96"),
				] {
					ui.vertical(|ui| {
						ui.add_space(KnobSize::XL.diameter() - size.diameter());
						demo_knob(ui, key, 60.0, |id, v| {
							Knob::new(id, v, pct).size(size).label(label).unit("%")
						});
					});
				}
			});
		});
		section(ui, tr("Knob · polarity, automation, AI"), col, |ui| {
			ui.horizontal_top(|ui| {
				ui.spacing_mut().item_spacing.x = 10.0;
				demo_knob(ui, "uni", 25.0, |id, v| {
					Knob::new(id, v, pct).label(tr("Unipolar")).unit("%")
				});
				demo_knob(ui, "bi", -9.6, |id, v| {
					Knob::new(id, v, KnobSpec::new(-24.0, 24.0, 0.0))
						.label(tr("Bipolar"))
						.unit("dB")
				});
				demo_knob(ui, "auto", 58.0, |id, v| {
					Knob::new(id, v, pct)
						.label(tr("Automated"))
						.unit("%")
						.automation(Some(40.0))
				});
				demo_knob(ui, "ai", 2800.0, |id, v| {
					Knob::new(id, v, KnobSpec::for_param(20.0, 20_000.0, 1000.0, tr("Hz")))
						.label(tr("AI changed"))
						.unit(tr("Hz"))
						.ai(true)
				});
				demo_knob(ui, "off", 30.0, |id, v| {
					Knob::new(id, v, pct)
						.label(tr("Disabled"))
						.unit("%")
						.enabled(false)
				});
			});
		});
		section(ui, tr("Knob · states and floating value"), col, |ui| {
			ui.horizontal_top(|ui| {
				ui.spacing_mut().item_spacing.x = 10.0;
				let ms = KnobSpec::for_param(0.1, 2000.0, 10.0, "ms");
				demo_knob(ui, "rest", 120.0, |id, v| {
					Knob::new(id, v, ms).label(tr("Rest")).unit("ms")
				});
				demo_knob(ui, "hover", 184.0, |id, v| {
					Knob::new(id, v, ms).label(tr("Hover")).unit("ms")
				});
				demo_knob(ui, "accent", 120.0, |id, v| {
					Knob::new(id, v, ms)
						.label(tr("Accent"))
						.unit("ms")
						.color(theme::BAND_COLORS[6])
				});
				demo_knob(ui, "plain", 4.0, |id, v| {
					Knob::new(id, v, KnobSpec::for_param(1.0, 4.0, 1.0, "voices"))
						.label(tr("Whole"))
						.unit("voices")
				});
			});
			caption(
				ui,
				tr("Drag: 200 px = range, Shift ×0.1 · wheel 1/100 · double-click resets"),
			);
		});
	});
	ui.horizontal_top(|ui| {
		section(ui, tr("Value readout"), col, |ui| {
			ui.horizontal(|ui| {
				let size = vec2(84.0, 22.0);
				for (text, state) in [
					("2.80 kHz", ReadoutState::Rest),
					("2.80 kHz", ReadoutState::Hover),
					("3.2k", ReadoutState::Typing),
					("40 kHz", ReadoutState::Error),
				] {
					let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
					paint_readout(ui.painter(), rect, text, ReadoutLook::Boxed, state);
				}
			});
			ui.horizontal(|ui| {
				for name in [tr("Rest"), tr("Hover"), tr("Typing"), tr("Out of range")] {
					ui.add_sized(
						[84.0, 14.0],
						egui::Label::new(
							egui::RichText::new(name)
								.font(theme::small())
								.color(theme::TEXT_FAINT),
						),
					);
				}
			});
			ui.add_space(4.0);
			ui.horizontal(|ui| {
				let (id, v) = demo_value(ui, "readout", 2800.0);
				let (typed, _) = readout(
					ui,
					id.with("live"),
					v,
					20.0,
					20_000.0,
					ValueFmt::unit(tr("Hz")),
					ReadoutLook::Boxed,
					vec2(84.0, 22.0),
					ReadoutState::Rest,
				);
				if let Some(v) = typed {
					ui.data_mut(|d| d.insert_temp(id, v));
				}
				caption(ui, tr("Click to type: 3.2k, -6 dB, 250ms, C4, 1/8"));
			});
		});
		section(ui, tr("Toggle, segmented, grid"), col, |ui| {
			ui.horizontal(|ui| {
				ui.spacing_mut().item_spacing.x = 8.0;
				for (key, label, color, initial) in [
					("pp-off", "⇄ Ping-pong", theme::STEREO_L, 0.0),
					("pp-on", "⇄ Ping-pong", theme::STEREO_L, 1.0),
					("inv", tr("Ø Invert R"), theme::WARNING, 1.0),
				] {
					let (id, v) = demo_value(ui, key, initial);
					if toggle(ui, v >= 0.5, label, color).clicked() {
						ui.data_mut(|d| d.insert_temp(id, 1.0 - v));
					}
				}
				let (id, v) = demo_value(ui, "slope", 2.0);
				if let Some(i) = segmented(
					ui,
					id.with("seg"),
					&[tr("Off"), "12", "24", "48"],
					v as usize,
				) {
					ui.data_mut(|d| d.insert_temp(id, i as f32));
				}
			});
			let names: Vec<String> = device::delay_sync_choices()
				.iter()
				.map(|n| device::short_choice(n))
				.collect();
			let labels: Vec<&str> = names.iter().map(String::as_str).collect();
			let (id, v) = demo_value(ui, "sync", 6.0);
			if let Some(i) = grid_selector(
				ui,
				id.with("grid"),
				&labels,
				v as usize,
				7,
				vec2(52.0, 22.0),
			) {
				ui.data_mut(|d| d.insert_temp(id, i as f32));
			}
		});
		section(ui, tr("Wave and mode picker cards"), col, |ui| {
			let (id, v) = demo_value(ui, "wave", 1.0);
			ui.horizontal(|ui| {
				ui.spacing_mut().item_spacing.x = 6.0;
				for (i, (label, wave)) in [
					(tr("Sine"), Wave::Sine),
					(tr("Saw"), Wave::Saw),
					(tr("Square"), Wave::Square),
					(tr("Triangle"), Wave::Triangle),
				]
				.into_iter()
				.enumerate()
				{
					let r = mode_card(
						ui,
						v as usize == i,
						label,
						theme::BAND_COLORS[6],
						CardShape::Tall,
						|p, rect, ink| paint_wave(p, rect, wave, ink),
					);
					if r.clicked() {
						ui.data_mut(|d| d.insert_temp(id, i as f32));
					}
				}
			});
			let (id, v) = demo_value(ui, "mode", 1.0);
			ui.horizontal(|ui| {
				ui.spacing_mut().item_spacing.x = 6.0;
				let shapes: [(&str, CardCurve); 3] = [
					(tr("Tape"), |x| (2.0 * x).tanh()),
					(tr("Tube"), |x| {
						((2.5 * x + 0.4).tanh() - 0.4f32.tanh()) * 0.9
					}),
					(tr("Hard clip"), |x| (1.8 * x).clamp(-0.8, 0.8)),
				];
				for (i, (label, f)) in shapes.into_iter().enumerate() {
					let r = mode_card(
						ui,
						v as usize == i,
						label,
						theme::BAND_COLORS[0],
						CardShape::Wide,
						|p, rect, ink| paint_curve(p, rect, ink, f),
					);
					if r.clicked() {
						ui.data_mut(|d| d.insert_temp(id, i as f32));
					}
				}
			});
		});
	});
	ui.horizontal_top(|ui| {
		section(ui, tr("Meters"), col, |ui| {
			ui.horizontal_top(|ui| {
				ui.spacing_mut().item_spacing.x = 18.0;
				let meter = |ui: &mut Ui,
				             name: &str,
				             add: &dyn Fn(&mut Ui),
				             value: &str,
				             color: Color32| {
					ui.allocate_ui_with_layout(
						vec2(40.0, 190.0),
						egui::Layout::top_down(egui::Align::Center),
						|ui| {
							ui.spacing_mut().item_spacing.y = 4.0;
							ui.label(
								egui::RichText::new(name)
									.font(theme::mono(10.5))
									.color(theme::TEXT_FAINT),
							);
							add(ui);
							ui.label(
								egui::RichText::new(value)
									.font(theme::mono(11.5))
									.color(color),
							);
						},
					);
				};
				meter(
					ui,
					"L / R",
					&|ui| {
						let (rect, _) = ui.allocate_exact_size(vec2(26.0, 140.0), Sense::hover());
						paint_level_meter(ui.painter(), rect, &[-6.2, -7.5], &[-2.4, -4.0]);
					},
					"-6.2",
					theme::TEXT,
				);
				meter(
					ui,
					"GR",
					&|ui| {
						let (rect, _) = ui.allocate_exact_size(vec2(10.0, 140.0), Sense::hover());
						paint_gr_meter(ui.painter(), rect, 6.2, 8.5, 24.0);
					},
					"-6.2",
					theme::GAIN_REDUCTION,
				);
				meter(
					ui,
					tr("CLIP"),
					&|ui| {
						let (rect, _) = ui.allocate_exact_size(vec2(26.0, 140.0), Sense::hover());
						paint_level_meter(ui.painter(), rect, &[0.4, -0.8], &[0.4, 0.1]);
					},
					"+0.4",
					theme::METER_HIGH,
				);
				ui.vertical(|ui| {
					ui.add_space(40.0);
					caption(ui, tr("HEADER PAIR (IN / OUT)"));
					io_meters(ui, [-12.6, -13.4], [-14.1, -14.8], false);
					ui.add_space(6.0);
					caption(ui, "COMPACT (34 px)");
					io_meters(ui, [-12.6, -13.4], [-4.1, -2.0], true);
					ui.add_space(6.0);
					caption(ui, tr("LIVE (peak hold 1.5 s)"));
					ui.horizontal(|ui| {
						level_meter(
							ui,
							Id::new("device-kit-live-meter"),
							&[-18.0, -20.0],
							vec2(16.0, 40.0),
						);
						gr_meter(
							ui,
							Id::new("device-kit-live-gr"),
							3.0,
							24.0,
							vec2(6.0, 40.0),
						);
					});
				});
			});
		});
		section(ui, tr("Band / voice palette · glow"), col, |ui| {
			ui.horizontal_wrapped(|ui| {
				ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
				for (name, color) in [
					tr("Coral"),
					tr("Orange"),
					tr("Yellow"),
					tr("Green"),
					tr("Teal"),
					tr("Sky"),
					tr("Violet"),
					tr("Pink"),
				]
				.iter()
				.zip(theme::BAND_COLORS)
				{
					let (rect, _) = ui.allocate_exact_size(vec2(104.0, 44.0), Sense::hover());
					let p = ui.painter();
					p.rect_filled(
						rect,
						CornerRadius::same(theme::RADIUS_MD),
						theme::DISPLAY_BG,
					);
					p.rect_filled(
						Rect::from_min_size(
							rect.min + vec2(6.0, 6.0),
							vec2(rect.width() - 12.0, 12.0),
						),
						CornerRadius::same(2),
						color,
					);
					p.text(
						rect.min + vec2(7.0, 24.0),
						Align2::LEFT_TOP,
						format!("{name} {}", theme::to_hex(color)),
						theme::mono(10.0),
						theme::TEXT_DIM,
					);
				}
			});
			let (_, p) = display(ui, vec2(440.0, 80.0));
			let r = p.clip_rect();
			for (k, color) in [(0usize, theme::BAND_COLORS[5]), (1, theme::BAND_COLORS[3])] {
				let pts: Vec<Pos2> = (0..=120)
					.map(|i| {
						let t = i as f32 / 120.0;
						let v =
							(t * std::f32::consts::TAU * (1.5 + k as f32)).sin() * (1.0 - t * 0.6);
						pos2(
							r.left() + t * r.width(),
							r.center().y - v * r.height() * 0.35,
						)
					})
					.collect();
				theme::glow_line(&p, pts, color);
			}
		});
		section(ui, tr("Bound to the project"), col, |ui| {
			let device = state.project().tracks.iter().find_map(|t| {
				t.channel
					.inserts
					.iter()
					.chain(t.instrument.iter())
					.find(|d| d.builtin_id().and_then(cadenza_dsp::device_info).is_some())
					.map(|d| {
						(
							DeviceRef {
								track: Some(t.id),
								device: d.id,
							},
							d.name.clone(),
						)
					})
			});
			let Some((r, name)) = device else {
				caption(ui, tr("No built-in device in this project"));
				return;
			};
			caption(
				ui,
				&format!("{name}: param_knob reads, previews and commits"),
			);
			let params: Vec<&'static str> = state
				.project()
				.device(r.track, r.device)
				.and_then(|d| d.builtin_id())
				.and_then(cadenza_dsp::device_info)
				.map(|i| i.params.iter().take(5).map(|p| p.id).collect())
				.unwrap_or_default();
			ui.horizontal_top(|ui| {
				ui.spacing_mut().item_spacing.x = 6.0;
				for param in params {
					let _ = param_knob(ui, state, r, param, KnobSize::S);
				}
			});
		});
	});
	ui.horizontal_top(|ui| {
		section(ui, tr("Frequency display · grid, analyzer, nodes, piano strip"), 2.0 * col + 48.0, |ui| {
			eq_demo(ui, vec2(2.0 * col + 48.0, 260.0));
			caption(
				ui,
				tr("Real EqBands response · pre (solid) and post (blue) analyzer · drag nodes (Shift fine, Alt axis lock), wheel = Q, double-click resets"),
			);
		});
		section(ui, tr("ADSR editor"), col, |ui| {
			adsr_demo(ui, vec2(col, 160.0));
			caption(ui, tr("Cube-root time axis · D moves decay and sustain · Shift fine"));
			ui.add_space(4.0);
			filter_demo(ui, vec2(col, 104.0));
			caption(ui, tr("Poly Synth filter, -48..+24 dB"));
		});
	});
	ui.horizontal_top(|ui| {
		section(ui, tr("Level history"), 2.0 * col + 48.0, |ui| {
			history_demo(ui, 2.0 * col + 48.0);
			caption(
				ui,
				tr("Input fill, output line, gain reduction from the top · drag the threshold"),
			);
		});
	});
}
