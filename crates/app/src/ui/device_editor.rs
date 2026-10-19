//! The device editor of the bottom editor dock (design `14 Device Kit`, "Device header ·
//! dock and compact"): the shared header (power, name, track chip, presets, Save, A/B
//! compare, undo hint, IN/OUT meters from the probe, Mix, pop-out, close) over a body
//! chosen by device id. Built-ins only; plugins keep their own windows. Popped out, the
//! same editor floats in a window over the workspace (design 15f: Dock, Keep on top, ×).
//!
//! Bodies are plain functions dispatched in [`DeviceEditorUi::body`]: the samplers' pads
//! / zones editor, else [`generic_body`] (every param as a kit control, grouped in rows).
//! A dedicated body (`eq_body(ui, state, cx)`) is one more match arm there.

use cadenza_core::ParamAddress;
use cadenza_dsp::{DeviceInfo, ParamInfo};
use cadenza_engine::ProbeFrame;
use cadenza_model::{Command, Device};
use egui::{
	Align, Align2, Color32, CornerRadius, CursorIcon, Id, Layout, Pos2, Rect, Sense, Stroke,
	StrokeKind, Ui, UiBuilder, pos2, vec2,
};

use crate::state::AppState;
use crate::state::device::{self, ParamControl, PresetEntry, PresetStatus};
use crate::state::device_ab::{self, Slot};
use crate::state::device_display::IoLevels;
use crate::state::dnd::DropTarget;
use crate::state::selection::DeviceRef;
use crate::theme;
use crate::ui::device_kit::{self, ControlInfo, KnobSize, ParamKnob, ValueFmt};
use crate::ui::instrument_editor::InstrumentEditorUi;

/// Below this dock width the header is the compact two-row one.
pub const COMPACT_BELOW: f32 = 800.0;
/// From this width on the header also shows the undo hint and the A→B copy label.
pub const WIDE_FROM: f32 = 1600.0;
const POPOUT_BAR_H: f32 = 28.0;
const POPOUT_DEFAULT: egui::Vec2 = vec2(1240.0, 460.0);
const HEADER_H: f32 = 36.0;
const COMPACT_HEADER_H: f32 = 62.0;
const MIX_BAR_FILL: Color32 = Color32::from_rgb(0xc4, 0xc7, 0xcd);

/// What a device body draws into.
pub struct BodyCx<'a> {
	pub r: DeviceRef,
	pub info: &'static DeviceInfo,
	pub rect: Rect,
	/// The probe's frames drained this frame (input and output of the device).
	pub frames: &'a [ProbeFrame],
	/// The compact header is shown: the Mix control belongs in the body.
	pub compact: bool,
	/// The track's colour (master: the accent).
	pub accent: Color32,
	/// Tells the same device's editors apart ([`DOCK`], the inspector's).
	pub salt: &'static str,
}

/// Whether the device's dock body has the on-screen keyboard (A–K play it).
fn plays_keys(id: &str) -> bool {
	matches!(id, "poly_synth" | "electric_piano")
}

/// [`BodyCx::salt`] of the dock editor.
pub const DOCK: &str = "dock";

/// A user preset name being typed.
struct SaveDialog {
	device: DeviceRef,
	name: String,
}

/// View-local state of the device editor.
#[derive(Default)]
pub struct DeviceEditorUi {
	pub instrument: InstrumentEditorUi,
	synth: crate::ui::devices::poly_synth::SynthKeys,
	frames: Vec<ProbeFrame>,
	levels: IoLevels,
	save: Option<SaveDialog>,
	/// Header and body of the last frame (headless tests).
	pub header_rect: Option<Rect>,
	pub body_rect: Option<Rect>,
	/// The floating window in the last frame, and whether the last click landed in it
	/// (its keys then go to the editor).
	pub popout_rect: Option<Rect>,
	popout_focused: bool,
	ask: crate::ui::device_ai::AskAi,
	/// The AI change card of the last frame (headless tests).
	pub ai_card_rect: Option<Rect>,
}

impl DeviceEditorUi {
	/// The drop target under `pos` in the last frame (sampler pads and zones).
	pub fn target_at(&self, pos: Pos2) -> Option<DropTarget> {
		self.instrument.target_at(pos)
	}

	/// Releases notes held by the body (dock hidden, device changed).
	pub fn release(&mut self, state: &mut AppState) {
		self.instrument.release(state);
		self.synth.release(state);
	}

	/// The editor in the bottom dock.
	pub fn show(&mut self, ui: &mut Ui, state: &mut AppState) {
		self.show_in(ui, state, false);
	}

	/// Whether the floating editor has the keyboard (the last click landed in it).
	pub fn popout_focused(&self, state: &AppState) -> bool {
		self.popout_focused && state.view.device_popout
	}

	/// The floating device editor (popped out of the dock): a window over the workspace
	/// with its own title bar, resizable from every edge, its size and position kept per
	/// device kind for the session (egui memory).
	pub fn popout(&mut self, ctx: &egui::Context, state: &mut AppState, locked: bool) {
		self.popout_rect = None;
		if !state.view.device_popout {
			self.popout_focused = false;
			return;
		}
		let Some(r) = state.view.instrument.device else {
			return;
		};
		let Some(device) = state.device(r).cloned() else {
			return;
		};
		let Some(info) = device.builtin_id().and_then(cadenza_dsp::device_info) else {
			return;
		};
		let screen = ctx.content_rect();
		let size = POPOUT_DEFAULT.min(screen.size() * 0.9);
		let default = Rect::from_center_size(screen.center() + vec2(0.0, -40.0), size);
		let order = if state.view.popout_on_top {
			egui::Order::Foreground
		} else {
			egui::Order::Middle
		};
		let frame = egui::Frame::new()
			.fill(theme::BG_PANEL)
			.stroke(Stroke::new(1.0, theme::BORDER_STRONG))
			.corner_radius(CornerRadius::same(theme::RADIUS_XL))
			.shadow(theme::FLOATING_SHADOW);
		let title = format!("{} · {}", device.name, channel_name(state, r));
		let shown = egui::Window::new(title.clone())
			.id(Id::new(("device-popout", info.id)))
			.title_bar(false)
			.collapsible(false)
			.resizable(true)
			.scroll(false)
			.default_rect(default)
			.min_size(vec2(360.0, 220.0))
			.order(order)
			.frame(frame)
			.show(ctx, |ui| {
				if locked {
					ui.disable();
				}
				let full = ui.available_rect_before_wrap();
				ui.allocate_rect(full, Sense::hover());
				let bar = Rect::from_min_size(full.min, vec2(full.width(), POPOUT_BAR_H));
				let body = Rect::from_min_max(pos2(full.left(), bar.bottom()), full.max);
				// The title bar (no widgets under it) moves the window; the body's own
				// background must not, or dragging an empty display would.
				ui.interact(body, Id::new("device-popout-body"), Sense::drag());
				self.popout_bar(ui, state, bar, &device.name, &channel_name(state, r));
				let mut inner = ui.new_child(UiBuilder::new().max_rect(body));
				self.show_in(&mut inner, state, true);
			});
		self.popout_rect = shown.map(|s| s.response.rect);
		if ctx.input(|i| i.pointer.any_pressed())
			&& let Some(pos) = ctx.pointer_interact_pos()
		{
			self.popout_focused = self.popout_rect.is_some_and(|r| r.contains(pos));
		}
	}

	/// The floating window's title bar: "<Device> · <Track>", Dock, Keep on top, ×.
	fn popout_bar(
		&mut self,
		ui: &mut Ui,
		state: &mut AppState,
		bar: Rect,
		name: &str,
		track: &str,
	) {
		let radius = theme::RADIUS_XL.saturating_sub(1);
		ui.painter().rect_filled(
			bar,
			CornerRadius {
				nw: radius,
				ne: radius,
				..CornerRadius::ZERO
			},
			theme::BG_RAISED,
		);
		ui.painter().hline(
			bar.x_range(),
			bar.bottom() - 0.5,
			Stroke::new(1.0, theme::BORDER),
		);
		let p = ui.painter();
		let g = p.layout_no_wrap(name.to_owned(), theme::semibold(12.5), theme::TEXT);
		let w = g.size().x;
		let cy = bar.center().y;
		p.galley(
			pos2(bar.left() + 10.0, cy - g.size().y * 0.5),
			g,
			theme::TEXT,
		);
		p.text(
			pos2(bar.left() + 10.0 + w + 6.0, cy),
			Align2::LEFT_CENTER,
			format!("· {track}"),
			theme::medium(12.0),
			theme::TEXT_FAINT,
		);
		let mut row = row(ui, bar.shrink2(vec2(6.0, 0.0)));
		row.with_layout(Layout::right_to_left(Align::Center), |ui| {
			ui.spacing_mut().item_spacing.x = 4.0;
			if close_button(ui, 22.0).clicked() {
				self.release(state);
				state.close_device_editor();
			}
			let on_top = state.view.popout_on_top;
			let hint = "Keep this window above other windows";
			if small_button(ui, "Keep on top", Some(on_top), hint).clicked() {
				state.view.popout_on_top = !on_top;
			}
			if small_button(ui, "Dock", None, "Put the editor back into the bottom dock").clicked()
			{
				state.dock_device_editor();
			}
		});
	}

	fn show_in(&mut self, ui: &mut Ui, state: &mut AppState, popped: bool) {
		let full = ui.available_rect_before_wrap();
		ui.allocate_rect(full, Sense::hover());
		ui.painter().rect_filled(full, 0.0, theme::BG_DEEP);
		self.header_rect = None;
		self.body_rect = None;
		let target = state.view.instrument.device;
		let device = target.and_then(|r| state.device(r).cloned());
		let info = device
			.as_ref()
			.and_then(|d| d.builtin_id())
			.and_then(cadenza_dsp::device_info);
		self.ai_card_rect = None;
		let (Some(r), Some(device), Some(info)) = (target, device, info) else {
			state.hear_before(None);
			self.instrument.clear();
			ui.painter().text(
				full.center(),
				Align2::CENTER_CENTER,
				"Double-click a device in the mixer or its track header, or use \"Edit\" in the \
				 inspector, to open it here",
				theme::small(),
				theme::TEXT_FAINT,
			);
			return;
		};
		self.drain_probe(ui, state);
		let compact = full.width() < COMPACT_BELOW;
		let wide = full.width() >= WIDE_FROM;
		state.compare(r);
		let header_h = if compact { COMPACT_HEADER_H } else { HEADER_H };
		let header = Rect::from_min_size(full.min, vec2(full.width(), header_h));
		self.header_rect = Some(header);
		let accent = r
			.track
			.and_then(|t| state.project().track(t))
			.map_or(theme::ACCENT, |t| theme::track_color(t.color.as_deref()));
		let mode = HeaderMode {
			compact,
			wide,
			popped,
		};
		self.header(ui, state, r, &device, info, header, mode, accent);
		let mut top = header.bottom();
		if crate::ui::device_ai::has_card(state, r) {
			let card = Rect::from_min_size(
				pos2(full.left() + 8.0, top + 8.0),
				vec2(full.width() - 16.0, crate::ui::device_ai::CARD_H - 8.0),
			);
			crate::ui::device_ai::card(ui, state, r, info, card);
			self.ai_card_rect = Some(card);
			top = card.bottom();
		} else if state.hearing_before() == Some(r.device) {
			state.hear_before(None);
		}
		let body = Rect::from_min_max(pos2(full.left(), top), full.max).shrink(8.0);
		self.body_rect = Some(body);
		let frames = std::mem::take(&mut self.frames);
		let cx = BodyCx {
			r,
			info,
			rect: body,
			frames: &frames,
			compact,
			accent,
			salt: DOCK,
		};
		self.body(ui, state, &cx);
		self.frames = frames;
		self.save_dialog(ui.ctx(), state);
	}

	/// The body of each device id (later tasks add dedicated ones here).
	fn body(&mut self, ui: &mut Ui, state: &mut AppState, cx: &BodyCx) {
		if !plays_keys(cx.info.id) {
			self.synth.release(state);
		}
		match cx.info.id {
			"sampler" => self.instrument.sampler_body(ui, state, cx),
			"drum_sampler" => self.instrument.drum_body(ui, state, cx),
			"eq" => {
				self.instrument.clear();
				crate::ui::devices::eq::eq_body(ui, state, cx);
			}
			"compressor" => {
				self.instrument.clear();
				crate::ui::devices::compressor::compressor_body(ui, state, cx);
			}
			"limiter" => {
				self.instrument.clear();
				crate::ui::devices::limiter::limiter_body(ui, state, cx);
			}
			"reverb" => {
				self.instrument.clear();
				crate::ui::devices::reverb::reverb_body(ui, state, cx);
			}
			"delay" => {
				self.instrument.clear();
				crate::ui::devices::delay::delay_body(ui, state, cx);
			}
			"chorus" => {
				self.instrument.clear();
				crate::ui::devices::chorus::chorus_body(ui, state, cx);
			}
			"saturator" => {
				self.instrument.clear();
				crate::ui::devices::saturator::saturator_body(ui, state, cx);
			}
			"gain" => {
				self.instrument.clear();
				crate::ui::devices::gain::gain_body(ui, state, cx);
			}
			"poly_synth" => {
				self.instrument.clear();
				crate::ui::devices::poly_synth::poly_synth_body(
					ui,
					state,
					cx,
					Some(&mut self.synth),
				);
			}
			"electric_piano" => {
				self.instrument.clear();
				crate::ui::devices::electric_piano::electric_piano_body(
					ui,
					state,
					cx,
					Some(&mut self.synth),
				);
			}
			"drum_machine" => {
				self.instrument.clear();
				crate::ui::devices::drum_machine::drum_machine_body(ui, state, cx);
			}
			_ => {
				self.instrument.clear();
				generic_body(ui, state, cx);
			}
		}
	}

	/// Keys of the open editor while it has focus (the dock or the floating window): A
	/// switches the A/B compare; EQ: 1-7 select a band, Del sleeps it; Limiter: R resets
	/// the integrated loudness; Saturator: 1/2/3 pick the mode; Gain: M = mono; Poly
	/// Synth and Electric Piano: A–K play notes (so A/B has no key there), Z / X octave;
	/// Drum Machine: ← → voices; Drum Sampler: 1–8 (Shift: 9–16) play the pads; Sampler:
	/// arrows nudge the selected zone.
	pub fn shortcuts(&mut self, ctx: &egui::Context, state: &mut AppState) {
		let Some(r) = state.view.instrument.device else {
			return;
		};
		let id = state
			.device(r)
			.and_then(|d| d.builtin_id())
			.map(str::to_owned);
		if !id.as_deref().is_some_and(plays_keys)
			&& ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::A))
		{
			state.toggle_compare(r);
		}
		match id.as_deref() {
			Some("poly_synth" | "electric_piano") => self.synth.shortcuts(ctx, state, r),
			Some("drum_machine") => crate::ui::devices::drum_machine::shortcuts(ctx, r),
			Some("drum_sampler") => crate::ui::devices::drum_sampler::shortcuts(
				ctx,
				state,
				r,
				&mut self.instrument.drum,
			),
			Some("sampler") => crate::ui::devices::sampler::shortcuts(ctx, state, r),
			Some("eq") => crate::ui::devices::eq::shortcuts(ctx, state, r),
			Some("limiter") => crate::ui::devices::limiter::shortcuts(ctx, r),
			Some("delay") => crate::ui::devices::delay::shortcuts(ctx, state, r),
			Some("saturator") => crate::ui::devices::saturator::shortcuts(ctx, state, r),
			Some("gain") => crate::ui::devices::gain::shortcuts(ctx, state, r),
			_ => {}
		}
	}

	/// Takes this frame's probe frames and moves the header meters.
	fn drain_probe(&mut self, ui: &Ui, state: &AppState) {
		self.frames.clear();
		if let Some(probe) = state.probe() {
			probe.drain(&mut self.frames);
		}
		let dt = ui.input(|i| i.stable_dt).min(0.25);
		self.levels.update(&self.frames, dt);
		if !self.levels.is_silent() || state.probe().is_some_and(|p| p.is_active()) {
			ui.ctx().request_repaint();
		}
	}

	#[allow(clippy::too_many_arguments)]
	fn header(
		&mut self,
		ui: &mut Ui,
		state: &mut AppState,
		r: DeviceRef,
		d: &Device,
		info: &'static DeviceInfo,
		rect: Rect,
		mode: HeaderMode,
		accent: Color32,
	) {
		ui.painter().rect_filled(rect, 0.0, theme::BG_PANEL);
		ui.painter().hline(
			rect.x_range(),
			rect.bottom() - 0.5,
			Stroke::new(1.0, theme::BORDER),
		);
		let track_name = channel_name(state, r);
		let mix = info.param("mix");
		let status = state.preset_status(r);
		let undo = state.device_undo_label(r).map(device_ab::undo_hint);
		if !mode.compact {
			let sidechain = d
				.sidechain
				.and_then(|s| state.project().track(s.track))
				.map(|key| (key.name.clone(), theme::track_color(key.color.as_deref())));
			let items = self.header_budget(
				ui,
				state,
				r,
				d,
				&track_name,
				&undo,
				mode,
				mix.is_some(),
				&sidechain,
				rect.width() - 18.0,
			);
			let mut bar = row(
				ui,
				rect.shrink2(vec2(8.0, 0.0)).with_max_x(rect.right() - 10.0),
			);
			let ui = &mut bar;
			ui.spacing_mut().item_spacing.x = 8.0;
			power_button(ui, state, r, d, 24.0);
			ui.label(
				egui::RichText::new(&d.name)
					.font(theme::semibold(13.0))
					.color(theme::TEXT),
			);
			track_chip(ui, &track_name, accent);
			divider(ui);
			ui.spacing_mut().item_spacing.x = 2.0;
			self.preset_bar(ui, state, r, &status, items.preset_width, true);
			ui.spacing_mut().item_spacing.x = 8.0;
			if header_button(ui, "Save", "Save these settings as a user preset").clicked() {
				self.open_save(r, &status);
			}
			ab_switch(ui, state, r);
			if mode.wide {
				ui.add_space(-4.0);
				copy_button(ui, state, r);
				if let Some(hint) = &undo {
					undo_chip(ui, state, hint);
				}
			}
			// The chip is the lowest-priority item: dropped first as the header narrows.
			if items.show_sidechain
				&& let Some((name, color)) = &sidechain
			{
				divider(ui);
				sidechain_chip(ui, name, *color);
			}
			ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
				ui.spacing_mut().item_spacing.x = 8.0;
				if !mode.popped {
					if close_button(ui, 24.0).clicked() {
						self.close(state);
					}
					ui.add_space(-4.0);
					if pop_out_button(ui, 24.0).clicked() {
						state.pop_out_device_editor();
					}
				}
				crate::ui::device_ai::ask_button(ui, state, r, &mut self.ask);
				if let Some(mix) = mix {
					mix_control(ui, state, r, mix);
				}
				device_kit::io_meters(
					ui,
					self.levels.input,
					self.levels.output,
					items.compact_meters,
				);
			});
			return;
		}
		let top = Rect::from_min_size(rect.min + vec2(8.0, 6.0), vec2(rect.width() - 16.0, 22.0));
		let mut first = row(ui, top);
		let ui1 = &mut first;
		ui1.spacing_mut().item_spacing.x = 7.0;
		power_button(ui1, state, r, d, 22.0);
		ui1.label(
			egui::RichText::new(&d.name)
				.font(theme::semibold(13.0))
				.color(theme::TEXT),
		);
		track_dot(ui1, &track_name, accent);
		ui1.with_layout(Layout::right_to_left(Align::Center), |ui| {
			ui.spacing_mut().item_spacing.x = 4.0;
			if !mode.popped {
				if close_button(ui, 22.0).clicked() {
					self.close(state);
				}
				if pop_out_button(ui, 22.0).clicked() {
					state.pop_out_device_editor();
				}
			}
			crate::ui::device_ai::ask_button(ui, state, r, &mut self.ask);
			let more = header_button(ui, "⋯", "More: save preset, A/B copy, undo");
			egui::Popup::menu(&more).show(|ui| {
				if ui.button("Save preset…").clicked() {
					self.open_save(r, &status);
					ui.close();
				}
				let copy = state.compare(r).copy_label();
				if ui
					.button(format!("Copy {copy}"))
					.on_hover_text("Copy the slot you are on onto the other one")
					.clicked()
				{
					state.copy_compare(r);
					ui.close();
				}
				if let Some(hint) = &undo
					&& ui.button(hint).clicked()
				{
					state.undo();
					ui.close();
				}
			});
		});
		let bottom = Rect::from_min_size(
			pos2(top.left(), top.bottom() + 6.0),
			vec2(top.width(), 22.0),
		);
		let mut second = row(ui, bottom);
		let ui2 = &mut second;
		ui2.spacing_mut().item_spacing.x = 2.0;
		let width = (bottom.width() - 44.0 - 44.0 - AB_W - 8.0).max(80.0);
		self.preset_bar(ui2, state, r, &status, width, false);
		ui2.add_space(6.0);
		ab_switch(ui2, state, r);
		ui2.add_space(6.0);
		device_kit::io_meters(ui2, self.levels.input, self.levels.output, true);
	}

	/// What the full header's preset box/sidechain chip/meters show at `rect`'s width, so
	/// they shrink or drop in order instead of the left (name/presets/A-B) and right
	/// (close/Ask AI/Mix/meters) groups overlapping in the middle (14 Device Kit;
	/// `device::header_items`). Measures the one piece of each side that varies with the
	/// device/track (name, track chip, sidechain chip) and feeds the rest as literal
	/// pixel constants mirroring the layout just below.
	#[allow(clippy::too_many_arguments)]
	fn header_budget(
		&self,
		ui: &Ui,
		state: &mut AppState,
		r: DeviceRef,
		d: &Device,
		track_name: &str,
		undo: &Option<String>,
		mode: HeaderMode,
		mix: bool,
		sidechain: &Option<(String, Color32)>,
		width: f32,
	) -> device::HeaderItems {
		let text_w = |s: &str, font: egui::FontId| {
			ui.painter()
				.layout_no_wrap(s.to_owned(), font, theme::TEXT)
				.size()
				.x
		};
		let name_w = text_w(&d.name, theme::semibold(13.0));
		let track_chip_w = text_w(track_name, theme::medium(12.0)) + 29.0;
		let save_w = (text_w("Save", theme::medium(12.0)) + 16.0).max(22.0);
		// power + gaps + name + chip + divider + preset steps (back/forward) + Save + A/B.
		let mut left_fixed = 173.0 + name_w + track_chip_w + save_w;
		if mode.wide {
			let copy_w = text_w(&state.compare(r).copy_label(), theme::medium(11.0)) + 14.0;
			left_fixed += copy_w;
			if let Some(hint) = undo {
				left_fixed += text_w(hint, theme::mono(10.5)) + 18.0;
			}
		}
		let ask_w = text_w("Ask AI", theme::medium(12.0)) + 16.0;
		let mut right_fixed = ask_w + 8.0;
		if !mode.popped {
			right_fixed += 60.0;
		}
		if mix {
			right_fixed += 8.0 + 138.0;
		}
		let sidechain_width = sidechain.as_ref().map(|(name, _)| {
			61.0 + text_w("Sidechain", theme::medium(11.0)) + text_w(name, theme::medium(12.0))
		});
		device::header_items(width, left_fixed, right_fixed, sidechain_width)
	}

	/// ‹ › and the preset box with its dropdown (factory, then user presets).
	fn preset_bar(
		&mut self,
		ui: &mut Ui,
		state: &mut AppState,
		r: DeviceRef,
		status: &PresetStatus,
		width: f32,
		label: bool,
	) {
		let list = state.device_presets(r).to_vec();
		for (glyph, step, hint, corner) in [
			(
				"‹",
				-1,
				"Previous preset",
				CornerRadius {
					nw: theme::RADIUS_MD,
					sw: theme::RADIUS_MD,
					..CornerRadius::ZERO
				},
			),
			("›", 1, "Next preset", CornerRadius::ZERO),
		] {
			let (rect, resp) = ui.allocate_exact_size(vec2(22.0, 22.0), Sense::click());
			resp.widget_info(|| {
				egui::WidgetInfo::labeled(egui::WidgetType::Button, !list.is_empty(), hint)
			});
			let fill = if resp.hovered() {
				theme::BG_HOVER
			} else {
				theme::BG_WIDGET
			};
			ui.painter().rect_filled(rect, corner, fill);
			ui.painter().text(
				rect.center(),
				Align2::CENTER_CENTER,
				glyph,
				theme::medium(14.0),
				theme::TEXT_DIM,
			);
			if resp.on_hover_text(hint).clicked()
				&& let Some(i) = device::step_preset(&list, status, step)
			{
				state.load_device_preset(r, &list[i]);
			}
		}
		let (rect, resp) = ui.allocate_exact_size(vec2(width, 22.0), Sense::click());
		resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::ComboBox, true, "Presets"));
		let p = ui.painter();
		p.rect(
			rect,
			CornerRadius {
				ne: theme::RADIUS_MD,
				se: theme::RADIUS_MD,
				..CornerRadius::ZERO
			},
			if resp.hovered() {
				theme::BG_RAISED
			} else {
				theme::BG_DEEP
			},
			Stroke::new(1.0, theme::BORDER),
			StrokeKind::Inside,
		);
		let mut x = rect.left() + 8.0;
		if label {
			let g = p.layout_no_wrap("Preset".into(), theme::medium(11.0), theme::TEXT_FAINT);
			let w = g.size().x;
			p.galley(
				pos2(x, rect.center().y - g.size().y * 0.5),
				g,
				theme::TEXT_FAINT,
			);
			x += w + 6.0;
		}
		let name = status
			.name
			.clone()
			.unwrap_or_else(|| "No preset".to_owned());
		let color = if status.name.is_some() {
			theme::TEXT
		} else {
			theme::TEXT_FAINT
		};
		let room = rect.right() - 22.0 - x - if status.dirty { 14.0 } else { 0.0 };
		let shown = crate::ui::mixer::elide(ui, &name, &theme::medium(12.0), room);
		let g = p.layout_no_wrap(shown, theme::medium(12.0), color);
		let w = g.size().x;
		p.galley(pos2(x, rect.center().y - g.size().y * 0.5), g, color);
		if status.dirty {
			p.circle_filled(pos2(x + w + 8.0, rect.center().y), 3.0, theme::WARNING);
		}
		chevron(
			p,
			pos2(rect.right() - 11.0, rect.center().y),
			theme::TEXT_FAINT,
		);
		let resp = resp
			.on_hover_cursor(CursorIcon::PointingHand)
			.on_hover_text(if status.dirty {
				"Presets · ● the settings differ from the loaded preset"
			} else {
				"Presets"
			});
		egui::Popup::menu(&resp).show(|ui| {
			ui.set_min_width(width.max(180.0));
			if list.is_empty() {
				ui.label(
					egui::RichText::new("No presets yet: Save stores the current settings")
						.color(theme::TEXT_FAINT),
				);
			}
			for (user, title) in [(false, "Factory"), (true, "User")] {
				let entries: Vec<&PresetEntry> = list.iter().filter(|e| e.user == user).collect();
				if entries.is_empty() {
					continue;
				}
				ui.label(
					egui::RichText::new(title)
						.font(theme::small())
						.color(theme::TEXT_FAINT),
				);
				for e in entries {
					let current = status.name.as_deref() == Some(&e.name) && status.user == user;
					let mut b = ui.selectable_label(current, &e.name);
					if !e.description.is_empty() {
						b = b.on_hover_text(&e.description);
					}
					if b.clicked() {
						state.load_device_preset(r, e);
						ui.close();
					}
				}
			}
		});
	}

	fn open_save(&mut self, r: DeviceRef, status: &PresetStatus) {
		self.save = Some(SaveDialog {
			device: r,
			name: status
				.name
				.clone()
				.filter(|_| status.user)
				.unwrap_or_default(),
		});
	}

	fn close(&mut self, state: &mut AppState) {
		self.release(state);
		state.close_device_editor();
		state.close_piano_roll();
	}

	/// The name prompt of Save (replacing a user preset of the same name asks first).
	fn save_dialog(&mut self, ctx: &egui::Context, state: &mut AppState) {
		let Some(dialog) = &mut self.save else {
			return;
		};
		let r = dialog.device;
		let exists = state
			.device_presets(r)
			.iter()
			.any(|e| e.user && e.name.eq_ignore_ascii_case(dialog.name.trim()));
		let mut done = false;
		let mut save = None;
		egui::Modal::new(Id::new("device-preset-save")).show(ctx, |ui| {
			ui.set_width(320.0);
			ui.label(
				egui::RichText::new("Save preset")
					.font(theme::heading())
					.color(theme::TEXT),
			);
			ui.add_space(6.0);
			let edit = ui.add(
				egui::TextEdit::singleline(&mut dialog.name)
					.hint_text("Preset name")
					.desired_width(f32::INFINITY),
			);
			edit.request_focus();
			let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
			if exists {
				ui.label(
					egui::RichText::new("A user preset with this name exists: it will be replaced")
						.font(theme::small())
						.color(theme::WARNING),
				);
			}
			ui.add_space(6.0);
			ui.horizontal(|ui| {
				let ok = !dialog.name.trim().is_empty();
				let label = if exists { "Replace" } else { "Save" };
				if ui.add_enabled(ok, egui::Button::new(label)).clicked() || (enter && ok) {
					save = Some(dialog.name.clone());
				}
				if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
					done = true;
				}
			});
		});
		if let Some(name) = save {
			done = state.save_device_preset(r, &name, exists);
		}
		if done {
			self.save = None;
		}
	}
}

/// How the header lays out.
#[derive(Debug, Clone, Copy)]
struct HeaderMode {
	/// Two rows (narrow dock or window).
	compact: bool,
	/// Room for the undo hint and the A→B label.
	wide: bool,
	/// In the floating window (its title bar has Dock and ×).
	popped: bool,
}

/// "Master" or the name of the track `r` sits on.
fn channel_name(state: &AppState, r: DeviceRef) -> String {
	match r.track {
		Some(t) => state
			.project()
			.track(t)
			.map_or(String::new(), |t| t.name.clone()),
		None => "Master".to_owned(),
	}
}

const AB_W: f32 = 50.0;

/// The A | B compare switch: the active slot is raised; a click switches (one undo step).
fn ab_switch(ui: &mut Ui, state: &mut AppState, r: DeviceRef) {
	let active = state.compare_slot(r);
	let (rect, _) = ui.allocate_exact_size(vec2(AB_W, 22.0), Sense::hover());
	ui.painter().rect(
		rect,
		CornerRadius::same(theme::RADIUS_MD),
		theme::BG_DEEP,
		Stroke::new(1.0, theme::BORDER),
		StrokeKind::Inside,
	);
	for (i, slot) in [Slot::A, Slot::B].into_iter().enumerate() {
		let cell = Rect::from_min_size(
			pos2(rect.left() + 2.0 + i as f32 * 24.0, rect.top() + 2.0),
			vec2(22.0, 18.0),
		);
		let resp = ui.interact(cell, Id::new(("device-ab", r.device, i)), Sense::click());
		let on = slot == active;
		let label = slot.label();
		resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, true, on, &label));
		let fill = if on {
			theme::BG_PRESSED
		} else if resp.hovered() {
			theme::BG_HOVER
		} else {
			Color32::TRANSPARENT
		};
		let p = ui.painter();
		p.rect_filled(cell, CornerRadius::same(theme::RADIUS), fill);
		p.text(
			cell.center(),
			Align2::CENTER_CENTER,
			slot.name(),
			theme::semibold(11.0),
			if on { theme::TEXT } else { theme::TEXT_FAINT },
		);
		let hint = if on {
			format!("Compare: {} is playing; your edits go to it", slot.name())
		} else {
			format!(
				"Compare: switch to {} (A key while the editor is focused)",
				slot.name()
			)
		};
		if resp
			.on_hover_cursor(CursorIcon::PointingHand)
			.on_hover_text(hint)
			.clicked()
		{
			state.switch_compare(r, slot);
		}
	}
}

/// "A→B": copies the slot you are on onto the other one.
fn copy_button(ui: &mut Ui, state: &mut AppState, r: DeviceRef) {
	let label = state.compare(r).copy_label();
	let resp = text_button(ui, &label, theme::medium(11.0));
	if resp
		.on_hover_text("Copy the slot you are on onto the other one")
		.clicked()
	{
		state.copy_compare(r);
	}
}

/// The dim "Ctrl+Z <step>" hint: the step undo would revert touched this device.
fn undo_chip(ui: &mut Ui, state: &mut AppState, hint: &str) {
	let font = theme::mono(10.5);
	let shown = crate::ui::mixer::elide(ui, hint, &font, 280.0);
	let resp = text_button(ui, &shown, font);
	if resp.on_hover_text(format!("Undo: {hint}")).clicked() {
		state.undo();
	}
}

/// Dim text that lights up on hover (no fill at rest).
fn text_button(ui: &mut Ui, label: &str, font: egui::FontId) -> egui::Response {
	let g = ui
		.painter()
		.layout_no_wrap(label.to_owned(), font, theme::TEXT_FAINT);
	let (rect, resp) = ui.allocate_exact_size(vec2(g.size().x + 10.0, 22.0), Sense::click());
	resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label));
	let p = ui.painter();
	let color = if resp.hovered() {
		p.rect_filled(rect, CornerRadius::same(theme::RADIUS_MD), theme::BG_HOVER);
		theme::TEXT_DIM
	} else {
		theme::TEXT_FAINT
	};
	p.galley(rect.center() - g.size() * 0.5, g, color);
	resp.on_hover_cursor(CursorIcon::PointingHand)
}

/// A title-bar button of the floating window (20 px high, raised); `check` = a toggle
/// that shows a check mark while on.
fn small_button(ui: &mut Ui, label: &str, check: Option<bool>, hint: &str) -> egui::Response {
	let g = ui
		.painter()
		.layout_no_wrap(label.to_owned(), theme::medium(11.0), theme::TEXT_DIM);
	let mark = if check == Some(true) { 13.0 } else { 0.0 };
	let (rect, resp) = ui.allocate_exact_size(vec2(g.size().x + 14.0 + mark, 20.0), Sense::click());
	resp.widget_info(|| match check {
		Some(on) => egui::WidgetInfo::selected(egui::WidgetType::Checkbox, true, on, label),
		None => egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label),
	});
	let p = ui.painter();
	p.rect_filled(
		rect,
		CornerRadius::same(theme::RADIUS),
		if resp.hovered() {
			theme::BG_HOVER
		} else {
			theme::BG_WIDGET
		},
	);
	let color = if resp.hovered() {
		theme::TEXT
	} else {
		theme::TEXT_DIM
	};
	let text = pos2(rect.left() + 7.0, rect.center().y - g.size().y * 0.5);
	let right = text.x + g.size().x;
	p.galley(text, g, color);
	if mark > 0.0 {
		let c = pos2(right + 8.0, rect.center().y);
		p.add(egui::Shape::line(
			vec![
				c + vec2(-3.5, 0.0),
				c + vec2(-1.0, 2.5),
				c + vec2(3.5, -2.5),
			],
			Stroke::new(1.4, theme::TEXT),
		));
	}
	resp.on_hover_cursor(CursorIcon::PointingHand)
		.on_hover_text(hint)
}

/// Pops the editor out into a floating window: a window frame with an arrow leaving it.
fn pop_out_button(ui: &mut Ui, size: f32) -> egui::Response {
	let (rect, resp) = ui.allocate_exact_size(vec2(size, size), Sense::click());
	resp.widget_info(|| {
		egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Pop out device editor")
	});
	let p = ui.painter();
	if resp.hovered() {
		p.rect_filled(rect, CornerRadius::same(theme::RADIUS_MD), theme::BG_HOVER);
	}
	let color = if resp.hovered() {
		theme::TEXT
	} else {
		theme::TEXT_DIM
	};
	let stroke = Stroke::new(1.3, color);
	let c = rect.center();
	let frame = Rect::from_min_size(c + vec2(-5.5, -3.5), vec2(9.0, 9.0));
	// The frame is open at the top-right corner, where the arrow leaves it.
	p.line_segment(
		[frame.left_top(), pos2(frame.left() + 4.0, frame.top())],
		stroke,
	);
	p.line_segment([frame.left_top(), frame.left_bottom()], stroke);
	p.line_segment([frame.left_bottom(), frame.right_bottom()], stroke);
	p.line_segment(
		[
			frame.right_bottom(),
			pos2(frame.right(), frame.bottom() - 4.0),
		],
		stroke,
	);
	let tip = c + vec2(5.5, -5.5);
	p.line_segment([c + vec2(-0.5, 0.5), tip], stroke);
	p.line_segment([tip, tip + vec2(-4.0, 0.0)], stroke);
	p.line_segment([tip, tip + vec2(0.0, 4.0)], stroke);
	resp.on_hover_cursor(CursorIcon::PointingHand)
		.on_hover_text("Pop out into a floating window")
}

/// A left-to-right row in `rect`.
fn row(ui: &mut Ui, rect: Rect) -> Ui {
	ui.new_child(
		UiBuilder::new()
			.max_rect(rect)
			.layout(Layout::left_to_right(Align::Center)),
	)
}

fn divider(ui: &mut Ui) {
	let (rect, _) = ui.allocate_exact_size(vec2(9.0, 18.0), Sense::hover());
	ui.painter().vline(
		rect.center().x,
		rect.y_range(),
		Stroke::new(1.0, theme::BORDER),
	);
}

fn chevron(p: &egui::Painter, center: Pos2, color: Color32) {
	p.add(egui::Shape::convex_polygon(
		vec![
			center + vec2(-3.5, -1.5),
			center + vec2(3.5, -1.5),
			center + vec2(0.0, 2.5),
		],
		color,
		Stroke::NONE,
	));
}

/// The power (bypass) button: green when the device runs.
fn power_button(ui: &mut Ui, state: &mut AppState, r: DeviceRef, d: &Device, size: f32) {
	let on = !d.bypass;
	let (rect, resp) = ui.allocate_exact_size(vec2(size, size), Sense::click());
	resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Checkbox, true, on, "Power"));
	let (bg, ring, fg) = if on {
		(
			theme::PLAYING_SOFT,
			Color32::from_rgb(0x2d, 0x5a, 0x40),
			theme::PLAYING,
		)
	} else {
		(theme::BG_WIDGET, theme::BG_HOVER, theme::BYPASS)
	};
	let p = ui.painter();
	p.rect(
		rect,
		CornerRadius::same(theme::RADIUS_MD),
		bg,
		Stroke::new(1.0, ring),
		StrokeKind::Inside,
	);
	// The power symbol: an open ring (gap at the top) and a stem.
	let c = rect.center() + vec2(0.0, 0.6);
	let radius = size * 0.2;
	let stroke = Stroke::new(1.6, fg);
	let points: Vec<Pos2> = (0..=24)
		.map(|i| {
			let a = -std::f32::consts::FRAC_PI_2
				+ 0.7 + i as f32 / 24.0 * (std::f32::consts::TAU - 1.4);
			c + vec2(a.cos(), a.sin()) * radius
		})
		.collect();
	p.add(egui::Shape::line(points, stroke));
	p.line_segment(
		[c - vec2(0.0, radius + 1.2), c - vec2(0.0, radius * 0.25)],
		stroke,
	);
	let resp = resp
		.on_hover_cursor(CursorIcon::PointingHand)
		.on_hover_text(if on {
			"Bypass the device"
		} else {
			"Enable the device"
		});
	if resp.clicked() {
		state.apply(Command::SetDeviceBypass {
			track: r.track,
			device: r.device,
			bypass: on,
		});
	}
}

/// The track the device sits on, as a chip with its colour.
fn track_chip(ui: &mut Ui, name: &str, color: Color32) {
	let font = theme::medium(12.0);
	let g = ui
		.painter()
		.layout_no_wrap(name.to_owned(), font, theme::TEXT_DIM);
	let (rect, _) = ui.allocate_exact_size(vec2(g.size().x + 29.0, 20.0), Sense::hover());
	let p = ui.painter();
	p.rect_filled(rect, CornerRadius::same(theme::RADIUS), theme::BG_RAISED);
	p.rect_filled(
		Rect::from_center_size(pos2(rect.left() + 11.0, rect.center().y), vec2(8.0, 8.0)),
		CornerRadius::same(2),
		color,
	);
	p.galley(
		pos2(rect.left() + 21.0, rect.center().y - g.size().y * 0.5),
		g,
		theme::TEXT_DIM,
	);
}

/// The sidechain key of the device: "Sidechain ▪ Kick".
fn sidechain_chip(ui: &mut Ui, name: &str, color: Color32) {
	let p = ui.painter();
	let lead = p.layout_no_wrap("Sidechain".into(), theme::medium(11.0), theme::TEXT_FAINT);
	let g = p.layout_no_wrap(name.to_owned(), theme::medium(12.0), theme::TEXT_DIM);
	let (lw, gw) = (lead.size().x, g.size().x);
	let (rect, resp) = ui.allocate_exact_size(vec2(lw + gw + 36.0, 20.0), Sense::hover());
	let p = ui.painter();
	p.rect_filled(rect, CornerRadius::same(theme::RADIUS), theme::BG_RAISED);
	let cy = rect.center().y;
	p.galley(
		pos2(rect.left() + 8.0, cy - lead.size().y * 0.5),
		lead,
		theme::TEXT_FAINT,
	);
	let x = rect.left() + 8.0 + lw + 6.0;
	p.rect_filled(
		Rect::from_center_size(pos2(x + 4.0, cy), vec2(8.0, 8.0)),
		CornerRadius::same(2),
		color,
	);
	p.galley(pos2(x + 13.0, cy - g.size().y * 0.5), g, theme::TEXT_DIM);
	resp.on_hover_text("The track keying this device's sidechain");
}

/// The compact header's track: a colour dot and the name.
fn track_dot(ui: &mut Ui, name: &str, color: Color32) {
	let (dot, _) = ui.allocate_exact_size(vec2(7.0, 7.0), Sense::hover());
	ui.painter().rect_filled(dot, CornerRadius::same(2), color);
	ui.add_space(-2.0);
	ui.label(
		egui::RichText::new(name)
			.font(theme::medium(11.0))
			.color(theme::TEXT_DIM),
	);
}

/// A flat 22 px header button.
fn header_button(ui: &mut Ui, label: &str, hint: &str) -> egui::Response {
	let font = theme::medium(12.0);
	let g = ui
		.painter()
		.layout_no_wrap(label.to_owned(), font, theme::TEXT_DIM);
	let w = (g.size().x + 16.0).max(22.0);
	let (rect, resp) = ui.allocate_exact_size(vec2(w, 22.0), Sense::click());
	resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label));
	let fill = if resp.hovered() {
		theme::BG_HOVER
	} else {
		theme::BG_WIDGET
	};
	let p = ui.painter();
	p.rect_filled(rect, CornerRadius::same(theme::RADIUS_MD), fill);
	let color = if resp.hovered() {
		theme::TEXT
	} else {
		theme::TEXT_DIM
	};
	p.galley(rect.center() - g.size() * 0.5, g, color);
	resp.on_hover_cursor(CursorIcon::PointingHand)
		.on_hover_text(hint)
}

fn close_button(ui: &mut Ui, size: f32) -> egui::Response {
	let (rect, resp) = ui.allocate_exact_size(vec2(size, size), Sense::click());
	resp.widget_info(|| {
		egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Close device editor")
	});
	let p = ui.painter();
	if resp.hovered() {
		p.rect_filled(rect, CornerRadius::same(theme::RADIUS_MD), theme::BG_HOVER);
	}
	let color = if resp.hovered() {
		theme::TEXT
	} else {
		theme::TEXT_DIM
	};
	let c = rect.center();
	let s = 4.0;
	let stroke = Stroke::new(1.4, color);
	p.line_segment([c + vec2(-s, -s), c + vec2(s, s)], stroke);
	p.line_segment([c + vec2(-s, s), c + vec2(s, -s)], stroke);
	resp.on_hover_cursor(CursorIcon::PointingHand)
		.on_hover_text("Close the device editor")
}

/// The header's dry/wet control: a mini slider bound to the `mix` param (drag previews,
/// release commits one command, double-click resets, right-click = control menu).
fn mix_control(ui: &mut Ui, state: &mut AppState, r: DeviceRef, info: &ParamInfo) {
	let (rect, resp) = ui.allocate_exact_size(vec2(138.0, 22.0), Sense::click_and_drag());
	let address = ParamAddress::device(r.track, r.device, info.id);
	let value = state
		.param_value(r.track, r.device, info.id)
		.unwrap_or(info.default);
	resp.widget_info(|| egui::WidgetInfo::slider(true, f64::from(value), "Mix"));
	let bar = Rect::from_min_size(
		pos2(rect.left() + 34.0, rect.center().y - 2.0),
		vec2(56.0, 4.0),
	);
	let at = |x: f32| {
		let t = ((x - bar.left()) / bar.width()).clamp(0.0, 1.0);
		info.min + t * (info.max - info.min)
	};
	let pointer = ui.ctx().pointer_interact_pos();
	if resp.double_clicked() {
		state.commit_param(r.track, r.device, info.id, info.default);
	} else if resp.drag_stopped()
		&& let Some(p) = pointer
	{
		state.commit_param(r.track, r.device, info.id, at(p.x));
	} else if resp.dragged()
		&& let Some(p) = pointer
	{
		state.preview_param(r.device, info.id, at(p.x));
	}
	let shown = state
		.param_value(r.track, r.device, info.id)
		.unwrap_or(info.default);
	let automated = state.automated(&address).is_some();
	let p = ui.painter();
	p.rect(
		rect,
		CornerRadius::same(theme::RADIUS),
		theme::BG_DEEP,
		Stroke::new(1.0, theme::BORDER),
		StrokeKind::Inside,
	);
	p.text(
		pos2(rect.left() + 7.0, rect.center().y),
		Align2::LEFT_CENTER,
		"Mix",
		theme::medium(11.0),
		theme::TEXT_DIM,
	);
	p.rect_filled(bar, CornerRadius::same(2), theme::BG_WIDGET);
	let t = ((shown - info.min) / (info.max - info.min).max(f32::EPSILON)).clamp(0.0, 1.0);
	p.rect_filled(
		Rect::from_min_size(bar.min, vec2(bar.width() * t, bar.height())),
		CornerRadius::same(2),
		if automated {
			theme::AUTOMATION
		} else {
			MIX_BAR_FILL
		},
	);
	p.text(
		pos2(rect.right() - 7.0, rect.center().y),
		Align2::RIGHT_CENTER,
		device::format_value(shown, info.unit),
		theme::mono(11.0),
		if automated {
			theme::AUTOMATION_TEXT
		} else {
			theme::TEXT
		},
	);
	let resp = resp
		.on_hover_cursor(CursorIcon::ResizeHorizontal)
		.on_hover_text("Dry/wet: drag, double-click resets");
	if let Some(v) = device_kit::control_menu(
		&resp,
		state,
		address,
		ControlInfo {
			value: shown,
			min: info.min,
			max: info.max,
			default: info.default,
			fmt: ValueFmt::unit(info.unit),
		},
	) {
		state.commit_param(r.track, r.device, info.id, v);
	}
}

// --- Generic body -----------------------------------------------------------------------

/// Every param of a built-in as a kit control (knobs, toggles, segmented / grid
/// selectors), grouped in labelled rows on a display panel; scrolls when it does not fit.
/// Mix sits in the header unless the header is compact.
pub fn generic_body(ui: &mut Ui, state: &mut AppState, cx: &BodyCx) {
	device_kit::paint_display(ui.painter(), cx.rect);
	let params: Vec<&'static ParamInfo> = cx
		.info
		.params
		.iter()
		.filter(|p| cx.compact || p.id != "mix")
		.collect();
	let mut inner = ui.new_child(
		UiBuilder::new()
			.max_rect(cx.rect.shrink2(vec2(14.0, 10.0)))
			.layout(Layout::top_down(Align::Min)),
	);
	if params.is_empty() {
		inner.label(egui::RichText::new("No parameters").color(theme::TEXT_FAINT));
		return;
	}
	let groups = device::param_groups(&params);
	let controls: Vec<Vec<(ParamControl, f32)>> = groups
		.iter()
		.map(|g| {
			g.params
				.iter()
				.map(|(p, label)| {
					let control = device::param_control(cx.info.id, p);
					let width = control_width(&inner, &control, label);
					(control, width)
				})
				.collect()
		})
		.collect();
	let widths: Vec<f32> = controls
		.iter()
		.map(|c| {
			c.iter().map(|(_, w)| w).sum::<f32>() + CONTROL_GAP * (c.len() as f32 - 1.0).max(0.0)
		})
		.collect();
	let rows = device::pack_rows(&widths, inner.available_width() - 12.0, GROUP_GAP);
	egui::ScrollArea::vertical()
		.id_salt(("device-body", cx.r.device))
		.auto_shrink([false, false])
		.show(&mut inner, |ui| {
			ui.spacing_mut().item_spacing.y = 16.0;
			for row in rows {
				ui.horizontal_top(|ui| {
					ui.spacing_mut().item_spacing.x = 0.0;
					for (n, &i) in row.iter().enumerate() {
						if n > 0 {
							let (rect, _) =
								ui.allocate_exact_size(vec2(GROUP_GAP, 112.0), Sense::hover());
							ui.painter().vline(
								rect.center().x,
								rect.y_range().shrink(6.0),
								Stroke::new(1.0, theme::BORDER),
							);
						}
						let group = &groups[i];
						ui.vertical(|ui| {
							ui.spacing_mut().item_spacing = vec2(CONTROL_GAP, 4.0);
							ui.label(
								egui::RichText::new(
									group.title.as_deref().unwrap_or("").to_uppercase(),
								)
								.font(theme::mono_medium(10.0))
								.color(theme::TEXT_FAINT),
							);
							ui.horizontal_top(|ui| {
								for ((param, label), (control, width)) in
									group.params.iter().zip(&controls[i])
								{
									param_control(ui, state, cx, param, label, control, *width);
								}
							});
						});
					}
				});
			}
		});
}

/// Space between the controls of a group, and between groups (the divider sits in it).
const CONTROL_GAP: f32 = 6.0;
const GROUP_GAP: f32 = 29.0;
const GRID_CELL: egui::Vec2 = vec2(46.0, 22.0);

fn text_width(ui: &Ui, text: &str, font: egui::FontId) -> f32 {
	ui.painter()
		.layout_no_wrap(text.to_owned(), font, theme::TEXT)
		.size()
		.x
}

/// How wide a control of the generic body is (knobs widen for long labels).
fn control_width(ui: &Ui, control: &ParamControl, label: &str) -> f32 {
	let label_w = text_width(ui, label, theme::medium(11.0));
	match control {
		ParamControl::Knob => {
			let d = KnobSize::M.diameter();
			let text = text_width(ui, label, egui::FontId::proportional(12.0));
			(d + 8.0).max(56.0).max(text + 8.0)
		}
		ParamControl::Toggle(text) => label_w.max(text_width(ui, text, theme::medium(12.0)) + 18.0),
		ParamControl::Segmented(labels) => {
			let cells: f32 = labels
				.iter()
				.map(|l| (text_width(ui, l, theme::mono(11.5)) + 16.0).max(30.0))
				.sum();
			label_w.max(cells + 4.0)
		}
		ParamControl::Grid(_, columns) => label_w.max(GRID_CELL.x * *columns as f32 + 4.0),
	}
}

/// One param of the generic body, `width` wide.
fn param_control(
	ui: &mut Ui,
	state: &mut AppState,
	cx: &BodyCx,
	param: &ParamInfo,
	label: &str,
	control: &ParamControl,
	width: f32,
) {
	let r = cx.r;
	let ai = state.ai_change(r.device, param.id).is_some();
	match control {
		ParamControl::Knob => {
			// Long labels: the knob centred in its widened slot.
			let pad = (width - (KnobSize::M.diameter() + 8.0).max(56.0)) * 0.5;
			ui.add_space(pad);
			ParamKnob::new(r, param.id)
				.size(KnobSize::M)
				.label(label)
				.show(ui, state);
			ui.add_space(pad);
		}
		ParamControl::Toggle(text) => {
			let color = match param.id {
				"ping_pong" => theme::STEREO_L,
				id if id.starts_with("invert") => theme::WARNING,
				_ => cx.accent,
			};
			selector_column(ui, label, width, ai, |ui| {
				device_kit::param_toggle(ui, state, r, param.id, text, color);
			});
		}
		ParamControl::Segmented(labels) | ParamControl::Grid(labels, _) => {
			let value = state
				.param_value(r.track, r.device, param.id)
				.unwrap_or(param.default);
			let selected = (value - param.min).round().max(0.0) as usize;
			let names: Vec<&str> = labels.iter().map(String::as_str).collect();
			let id = Id::new(("device-body-choice", r.device, param.id));
			let mut picked = None;
			selector_column(ui, label, width, ai, |ui| {
				picked = match control {
					ParamControl::Grid(_, columns) => {
						device_kit::grid_selector(ui, id, &names, selected, *columns, GRID_CELL)
					}
					_ => device_kit::segmented(ui, id, &names, selected),
				};
			});
			if let Some(i) = picked
				&& i != selected
			{
				state.commit_param(r.track, r.device, param.id, param.min + i as f32);
			}
		}
	}
}

/// A selector or toggle under its label, lined up with the knobs' dials; `ai` = the last
/// AI request changed it (blue label).
fn selector_column(ui: &mut Ui, label: &str, width: f32, ai: bool, add: impl FnOnce(&mut Ui)) {
	ui.vertical(|ui| {
		ui.set_width(width);
		ui.spacing_mut().item_spacing.y = 6.0;
		ui.add_space(8.0);
		ui.label(
			egui::RichText::new(label)
				.font(theme::medium(11.0))
				.color(if ai {
					theme::ACCENT_TEXT
				} else {
					theme::TEXT_DIM
				}),
		);
		add(ui);
	});
}
