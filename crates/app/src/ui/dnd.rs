//! Drag-and-drop helpers shared by the views: the hint shown next to the pointer (the
//! "ghost" label describing what a drop would do) and OS file hovers.

use std::path::PathBuf;

use egui::{Align2, Context, Frame, Id, Margin, RichText};

use crate::state::dnd::Payload;
use crate::theme;

fn hint_id() -> Id {
	Id::new("cadenza-drop-hint")
}

/// Sets this frame's drop hint (the view under the pointer calls it).
pub fn set_hint(ctx: &Context, text: String, accepted: bool) {
	ctx.data_mut(|d| d.insert_temp(hint_id(), (text, accepted)));
}

/// First file the OS file manager is dragging over the window, if any.
pub fn hovered_file(ctx: &Context) -> Option<PathBuf> {
	ctx.input(|i| i.raw.hovered_files.iter().find_map(|f| f.path.clone()))
}

/// Whether anything (in-app payload or OS file) is being dragged.
pub fn dragging(ctx: &Context) -> bool {
	egui::DragAndDrop::has_payload_of_type::<Payload>(ctx) || hovered_file(ctx).is_some()
}

/// Draws the hint near the pointer and clears it for the next frame. With nothing under
/// the pointer, shows what is being dragged.
pub fn paint_hint(ctx: &Context) {
	let hint = ctx.data_mut(|d| d.remove_temp::<(String, bool)>(hint_id()));
	if !dragging(ctx) {
		return;
	}
	let Some(pos) = ctx.pointer_latest_pos() else {
		return;
	};
	let (text, accepted) = match hint {
		Some(h) => h,
		None => {
			let label = egui::DragAndDrop::payload::<Payload>(ctx)
				.map(|p| p.label())
				.or_else(|| hovered_file(ctx).map(|p| Payload::File(p).label()))
				.unwrap_or_default();
			(
				format!("{label} — drop on a track, the mixer or the arrange view"),
				true,
			)
		}
	};
	let (icon, color) = if accepted {
		("⤵", theme::ACCENT)
	} else {
		("⊘", theme::ERROR)
	};
	egui::Area::new(Id::new("cadenza-drop-hint-area"))
		.fixed_pos(pos + egui::vec2(16.0, 18.0))
		.pivot(Align2::LEFT_TOP)
		.order(egui::Order::Tooltip)
		.interactable(false)
		.show(ctx, |ui| {
			Frame::new()
				.fill(theme::BG_RAISED)
				.stroke(egui::Stroke::new(1.0, color.gamma_multiply(0.8)))
				.corner_radius(4)
				.inner_margin(Margin::symmetric(8, 4))
				.show(ui, |ui| {
					ui.horizontal(|ui| {
						ui.label(RichText::new(icon).color(color));
						ui.label(RichText::new(text).font(theme::small()).color(theme::TEXT));
					});
				});
		});
}
