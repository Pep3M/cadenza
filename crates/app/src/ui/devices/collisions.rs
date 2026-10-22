//! The EQ's collision view (T32; logic in [`crate::state::collisions`]): red columns and
//! a heat strip where other tracks collide with this one, their spectra as thin lines in
//! their track colours, the strongest pair labelled, a tooltip with both levels on the
//! strip, and a click on a cell (or the label) proposing a cut, shown dashed with Apply /
//! Cancel.

use crate::i18n::{tr, trf};
use std::sync::Arc;

use cadenza_analysis::masking::Collision;
use cadenza_mcp::tools::mix::MixAnalysis;
use egui::{
	Align, Color32, CornerRadius, CursorIcon, Id, Layout, Rect, Sense, Stroke, StrokeKind, Ui,
	UiBuilder, pos2, vec2,
};

use crate::state::AppState;
use crate::state::collisions::{self as col, CollisionBand, CollisionStatus, MAX_SPECTRA, hz_text};
use crate::state::device_display;
use crate::state::selection::DeviceRef;
use crate::theme;
use crate::ui::device_kit::{self, FreqResponse, HEAT_STRIP_H, HeatBand};

/// Everything the display draws for the collision view of one EQ.
pub struct Layer {
	report: Arc<MixAnalysis>,
	collisions: Vec<Collision>,
	pub bands: Vec<CollisionBand>,
	/// This track's spectrum (drawn as the dim "pre" fill).
	pub own: Vec<f32>,
	/// Other tracks: bins, colour, name.
	spectra: Vec<(Vec<f32>, Color32, String)>,
	pub stale: bool,
}

impl Layer {
	pub fn spectra(&self) -> Vec<(&[f32], Color32)> {
		self.spectra
			.iter()
			.map(|(b, c, _)| (b.as_slice(), *c))
			.collect()
	}

	/// Heat cells; `hot` = the octave band hovered or holding the suggestion.
	pub fn heat(&self, hot: Option<usize>) -> Vec<HeatBand> {
		// Relative to the strongest, so the worst band always reads at full heat.
		let max = self.bands.iter().map(|b| b.heat).fold(0.0f32, f32::max);
		self.bands
			.iter()
			.map(|b| HeatBand {
				low: b.low_hz,
				high: b.high_hz,
				heat: if max > 0.0 { b.heat / max } else { 0.0 },
				hot: hot == Some(b.collision.band),
			})
			.collect()
	}

	fn name(&self, channel: usize) -> String {
		self.report
			.channels
			.get(channel)
			.map_or_else(String::new, |c| c.name.clone())
	}
}

/// The view's layer when it is shown and a report exists.
pub fn layer(state: &AppState, r: DeviceRef) -> Option<Layer> {
	if !state.collisions_shown(r) {
		return None;
	}
	let CollisionStatus::Ready { report, stale } = state.collision_status(r) else {
		return None;
	};
	let track = r.track?;
	let collisions = col::collisions_of(&report.channels, track);
	let bands = col::bands_of(&collisions);
	let others = col::top_others(&collisions, MAX_SPECTRA);
	let (own, other_bins) = col::spectra(&report.channels, track, &others);
	let spectra = others
		.iter()
		.zip(other_bins)
		.map(|(&i, bins)| {
			let c = &report.channels[i];
			let color = state
				.project()
				.track(c.track_id)
				.map_or(theme::DEFAULT_TRACK, |t| {
					theme::track_color(t.color.as_deref())
				});
			(bins, color, c.name.clone())
		})
		.collect();
	Some(Layer {
		report,
		collisions,
		bands,
		own,
		spectra,
		stale,
	})
}

/// The octave band to draw hot: the suggestion's, else the hovered one.
pub fn hot_band(state: &AppState, r: DeviceRef, hovered: Option<usize>) -> Option<usize> {
	state
		.collision_suggestion(r)
		.map(|s| s.collision.band)
		.or(hovered)
}

fn chip(ui: &mut Ui, at: Rect, text: &str, color: Color32, id: Id) -> egui::Response {
	let galley = ui
		.painter()
		.layout_no_wrap(text.to_owned(), theme::semibold(11.0), color);
	let size = galley.size() + vec2(12.0, 6.0);
	let rect = Rect::from_min_size(at.min, size);
	let resp = ui.interact(rect, id, Sense::click());
	let p = ui.painter();
	p.rect_filled(
		rect,
		CornerRadius::same(theme::RADIUS),
		theme::BG_RAISED.gamma_multiply(0.92),
	);
	p.rect_stroke(
		rect,
		CornerRadius::same(theme::RADIUS),
		Stroke::new(1.0, color.gamma_multiply(0.6)),
		StrokeKind::Inside,
	);
	p.galley(rect.min + vec2(6.0, 3.0), galley, color);
	resp
}

/// After the display: hover tooltip + click on the strip, the label of the strongest
/// pair, the legend, status chips. Returns the octave band hovered on the strip.
pub fn interact(
	ui: &mut Ui,
	state: &mut AppState,
	r: DeviceRef,
	layer: Option<&Layer>,
	out: &FreqResponse,
	salt: &str,
) -> Option<usize> {
	if !state.collisions_shown(r) {
		return None;
	}
	let plot = out.plot;
	let status = state.collision_status(r);
	let id = |s: &str| Id::new(("eq-collisions", salt, r.device, s.to_owned()));
	let corner = Rect::from_min_size(plot.min + vec2(8.0, 8.0), vec2(0.0, 0.0));
	match &status {
		CollisionStatus::Running(f) => {
			ui.ctx().request_repaint();
			chip(
				ui,
				corner,
				&trf(
					"Measuring collisions… {pct} %",
					&[("pct", &format!("{:.0}", f * 100.0))],
				),
				theme::TEXT_DIM,
				id("running"),
			);
		}
		CollisionStatus::Failed(e) => {
			if chip(ui, corner, e, theme::WARNING, id("failed"))
				.on_hover_cursor(CursorIcon::PointingHand)
				.on_hover_text(tr("Click to analyze again"))
				.clicked()
			{
				state.refresh_collisions();
			}
		}
		CollisionStatus::Idle => {
			if chip(
				ui,
				corner,
				tr("Analyze collisions"),
				theme::ACCENT,
				id("idle"),
			)
			.on_hover_cursor(CursorIcon::PointingHand)
			.clicked()
			{
				state.refresh_collisions();
			}
		}
		CollisionStatus::Unavailable(_) | CollisionStatus::Ready { .. } => {}
	}
	let layer = layer?;
	let mut top = corner;
	if layer.stale {
		let resp = chip(
			ui,
			top,
			tr("Changed since the analysis · Analyze again"),
			theme::WARNING,
			id("stale"),
		)
		.on_hover_cursor(CursorIcon::PointingHand);
		top = top.translate(vec2(0.0, resp.rect.height() + 4.0));
		if resp.clicked() {
			state.refresh_collisions();
		}
	}
	if layer.bands.is_empty() {
		chip(
			ui,
			top,
			tr("No collisions with other tracks"),
			theme::TEXT_DIM,
			id("none"),
		);
	}
	// The strongest pair, over its band.
	if let Some(c) = layer.collisions.first()
		&& state.collision_suggestion(r).is_none()
	{
		let own = layer.report.name_of(r.track);
		let text = col::pair_label(&own, &layer.name(c.other), c.band_hz);
		let center = plot.left() + device_display::freq_to_x(c.band_hz, plot.width());
		let w = ui
			.painter()
			.layout_no_wrap(text.clone(), theme::semibold(11.0), theme::ERROR)
			.size()
			.x + 12.0;
		let x = (center - w * 0.5).clamp(plot.left() + 4.0, plot.right() - w - 4.0);
		let y = plot.bottom() - HEAT_STRIP_H - 28.0;
		let at = Rect::from_min_size(pos2(x, y), vec2(0.0, 0.0));
		if chip(ui, at, &text, theme::ERROR, id("top"))
			.on_hover_cursor(CursorIcon::PointingHand)
			.on_hover_text(tr("Click to suggest a cut"))
			.clicked()
		{
			let c = c.clone();
			state.suggest_collision_cut(r, &c);
		}
	}
	// Legend of the other tracks' lines (top right).
	let mut x = plot.right() - 8.0;
	for (_, color, name) in layer.spectra.iter().rev() {
		let galley = ui
			.painter()
			.layout_no_wrap(name.clone(), theme::medium(11.0), *color);
		x -= galley.size().x;
		ui.painter()
			.galley(pos2(x, plot.top() + 8.0), galley, *color);
		x -= 12.0;
		ui.painter().line_segment(
			[pos2(x, plot.top() + 15.0), pos2(x + 8.0, plot.top() + 15.0)],
			Stroke::new(2.0, *color),
		);
		x -= 10.0;
	}
	// The strip: tooltip + click.
	let strip = Rect::from_min_max(
		pos2(plot.left(), plot.bottom() - HEAT_STRIP_H - 2.0),
		plot.right_bottom(),
	);
	let pointer = out.response.hover_pos().filter(|p| strip.contains(*p))?;
	let freq = device_display::x_to_freq(pointer.x - plot.left(), plot.width());
	let band = col::band_at(&layer.bands, freq)?;
	let rows: Vec<&Collision> = layer
		.collisions
		.iter()
		.filter(|c| c.band == band.collision.band)
		.collect();
	let own = layer.report.name_of(r.track);
	let resp = out.response.clone().on_hover_ui_at_pointer(|ui| {
		ui.label(
			egui::RichText::new(crate::state::mix::octave_range(band.collision.band_hz))
				.font(theme::semibold(12.0)),
		);
		for c in &rows {
			ui.label(
				egui::RichText::new(trf(
					"{own} {a} dB · {other} {b} dB · {pct} % of the time",
					&[
						("own", &own),
						("a", &crate::i18n::decimal(format!("{:.1}", c.level_db))),
						("other", &layer.name(c.other)),
						(
							"b",
							&crate::i18n::decimal(format!("{:.1}", c.other_level_db)),
						),
						("pct", &format!("{:.0}", c.overlap * 100.0)),
					],
				))
				.font(theme::mono(11.0)),
			);
		}
		ui.label(
			egui::RichText::new(tr("Click to suggest a cut"))
				.font(theme::medium(11.0))
				.color(theme::TEXT_FAINT),
		);
	});
	if resp.clicked() {
		let c = band.collision.clone();
		state.suggest_collision_cut(r, &c);
	}
	Some(band.collision.band)
}

/// The suggestion card over the display: what the cut does and Apply / Cancel.
pub fn card(ui: &mut Ui, state: &mut AppState, r: DeviceRef, display: Rect) {
	let Some(s) = state.collision_suggestion(r).cloned() else {
		return;
	};
	let other = state
		.mix
		.report
		.as_ref()
		.and_then(|rep| rep.channels.get(s.collision.other))
		.map_or_else(String::new, |c| c.name.clone());
	let gain = s
		.changes
		.iter()
		.find(|(id, _)| id.ends_with("_gain"))
		.map_or(0.0, |c| c.1);
	let q = s
		.changes
		.iter()
		.find(|(id, _)| id.ends_with("_q"))
		.map_or(0.0, |c| c.1);
	let width = (display.width() - 16.0).min(520.0);
	let card = Rect::from_min_size(
		pos2(display.right() - width - 8.0, display.top() + 30.0),
		vec2(width, 62.0),
	);
	let p = ui.painter();
	p.rect_filled(card, CornerRadius::same(theme::RADIUS_MD), theme::BG_RAISED);
	p.rect_stroke(
		card,
		CornerRadius::same(theme::RADIUS_MD),
		Stroke::new(1.0, theme::ERROR.gamma_multiply(0.6)),
		StrokeKind::Inside,
	);
	let inner = card.shrink2(vec2(10.0, 5.0));
	let half = inner.height() * 0.5;
	let row = |ui: &mut Ui, rect: Rect| {
		let mut c = ui.new_child(
			UiBuilder::new()
				.max_rect(rect)
				.layout(Layout::left_to_right(Align::Center)),
		);
		c.spacing_mut().item_spacing.x = 8.0;
		c
	};
	let mut top = row(ui, inner.with_max_y(inner.top() + half));
	let mut bottom = row(ui, inner.with_min_y(inner.top() + half));
	top.label(
		egui::RichText::new(trf("Make room for {other}", &[("other", &other)]))
			.font(theme::semibold(12.0))
			.color(theme::ERROR),
	);
	top.label(
		egui::RichText::new(trf(
			"Bell {gain} dB at {hz} · Q {q}",
			&[
				("gain", &crate::i18n::decimal(format!("{gain:.1}"))),
				("hz", &hz_text(s.collision.center_hz)),
				("q", &crate::i18n::decimal(format!("{q:.1}"))),
			],
		))
		.font(theme::mono(11.0))
		.color(theme::TEXT_DIM),
	);
	let note = if s.collision.cut_here {
		tr("The masking rule cuts this track here")
	} else {
		tr("The masking rule would cut the other track: half the cut here")
	};
	bottom.add(
		egui::Label::new(
			egui::RichText::new(note)
				.font(theme::medium(11.0))
				.color(theme::TEXT_FAINT),
		)
		.truncate(),
	);
	let (mut apply, mut cancel) = (false, false);
	bottom.with_layout(Layout::right_to_left(Align::Center), |ui| {
		cancel = ui.button(tr("Cancel")).clicked();
		apply = theme::primary_button(ui, tr("Apply")).clicked();
	});
	if apply {
		state.apply_collision_cut();
	} else if cancel {
		state.cancel_collision_cut();
	}
}

/// The footer's "Collisions" toggle (disabled with the reason on a master / bus EQ).
pub fn toggle(ui: &mut Ui, state: &mut AppState, r: DeviceRef) {
	let unavailable = match state.collision_status(r) {
		CollisionStatus::Unavailable(why) => Some(why),
		_ => None,
	};
	let on = state.collisions_shown(r) && unavailable.is_none();
	let resp = ui
		.add_enabled_ui(unavailable.is_none(), |ui| {
			device_kit::toggle(ui, on, tr("Collisions"), theme::ERROR)
		})
		.inner;
	let resp = match unavailable {
		Some(why) => resp.on_disabled_hover_text(why),
		None => resp.on_hover_text(tr(
			"Where other tracks collide with this one (offline analysis of the song, like the Mix assistant's masking)",
		)),
	};
	if resp.clicked() {
		state.toggle_collisions(r);
	}
}
