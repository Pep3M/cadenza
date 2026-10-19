//! The drop plumbing of the samplers' device editor bodies (`ui::device_editor`, bottom
//! editor dock): the drum sampler ([`crate::ui::devices::drum_sampler`]) and the sampler
//! ([`crate::ui::devices::sampler`]) draw themselves and leave their hit areas here, so
//! in-app and OS file drops land on a pad, the kit, a zone, an empty spot of the zone map
//! or its keys. Every drop is one command through `AppState`.

use egui::{CornerRadius, Pos2, Rect, Stroke, StrokeKind, Ui};

use crate::state::AppState;
use crate::state::dnd::{DropTarget, Payload};
use crate::state::selection::DeviceRef;
use crate::theme;
use crate::ui::device_editor::BodyCx;
use crate::ui::devices::drum_sampler::PadHits;
use crate::ui::devices::sampler::ZoneHits;
use crate::ui::dnd;

/// View-local state of the samplers' editor bodies.
#[derive(Default)]
pub struct InstrumentEditorUi {
	device: Option<DeviceRef>,
	/// Waveform area in the last frame (headless tests).
	pub wave_rect: Option<Rect>,
	/// Pad grid area in the last frame (headless tests).
	pub pad_rect: Option<Rect>,
	/// The drum sampler body's pads, kit area and held keys.
	pub drum: PadHits,
	/// The sampler body's zones, map, keys and held note.
	pub zones: ZoneHits,
}

impl InstrumentEditorUi {
	/// The drop target under `pos` in the last frame (pads, kit, zones, map, keys).
	pub fn target_at(&self, pos: Pos2) -> Option<DropTarget> {
		let device = self.device?;
		if let Some((_, note)) = self.drum.pads.iter().find(|(r, _)| r.contains(pos)) {
			return Some(DropTarget::Pad {
				device,
				note: *note,
			});
		}
		if self.drum.kit.is_some_and(|r| r.contains(pos)) {
			return Some(DropTarget::Kit { device });
		}
		self.zones.target_at(device, pos)
	}

	/// Releases notes still held by the editor (dock hidden, device changed).
	pub fn release(&mut self, state: &mut AppState) {
		self.drum.release(state);
		self.zones.release(state);
	}

	fn switch_to(&mut self, state: &mut AppState, r: DeviceRef) {
		if self.device != Some(r) {
			self.release(state);
		}
		self.device = Some(r);
	}

	/// The drum sampler body of the device editor.
	pub fn drum_body(&mut self, ui: &mut Ui, state: &mut AppState, cx: &BodyCx) {
		self.switch_to(state, cx.r);
		self.zones.clear();
		crate::ui::devices::drum_sampler::drum_sampler_body(ui, state, cx, &mut self.drum);
		self.wave_rect = self.drum.wave;
		self.pad_rect = self.drum.grid;
		self.drop_zone(ui, state);
	}

	/// The sampler body of the device editor.
	pub fn sampler_body(&mut self, ui: &mut Ui, state: &mut AppState, cx: &BodyCx) {
		self.switch_to(state, cx.r);
		self.drum.pads.clear();
		self.drum.kit = None;
		self.pad_rect = None;
		crate::ui::devices::sampler::sampler_body(ui, state, cx, &mut self.zones);
		self.wave_rect = self.zones.wave;
		self.drop_zone(ui, state);
	}

	/// The editor shows no sample device (hit areas forgotten).
	pub fn clear(&mut self) {
		self.device = None;
		self.wave_rect = None;
		self.pad_rect = None;
		self.drum.pads.clear();
		self.drum.kit = None;
		self.zones.clear();
	}

	/// Highlights the target under an in-app drag or an OS file hover, shows the hint and
	/// performs in-app drops on release (OS drops are handled by the window).
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
			// The drum sampler draws its own dashed targets.
			DropTarget::Pad { .. } | DropTarget::Kit { .. } if action.accepted() => None,
			DropTarget::Pad { note, .. } => self
				.drum
				.pads
				.iter()
				.find(|(_, n)| *n == note)
				.map(|(r, _)| *r),
			DropTarget::Kit { .. } => self.drum.kit,
			other => self.zones.target_rect(other),
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
