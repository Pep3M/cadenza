//! The one place that defines how Cadenza looks: a small dark palette, metrics and the
//! egui style built from them. Views use these constants instead of literal colors.

use egui::{Color32, CornerRadius, FontFamily, FontId, Margin, Stroke, TextStyle, Vec2};

// Surfaces, darkest to lightest.
pub const BG_DEEP: Color32 = Color32::from_rgb(0x0f, 0x11, 0x14);
pub const BG_PANEL: Color32 = Color32::from_rgb(0x17, 0x1a, 0x1e);
pub const BG_RAISED: Color32 = Color32::from_rgb(0x1f, 0x23, 0x28);
pub const BG_WIDGET: Color32 = Color32::from_rgb(0x2a, 0x2f, 0x36);
pub const BG_HOVER: Color32 = Color32::from_rgb(0x34, 0x3a, 0x42);
pub const BORDER: Color32 = Color32::from_rgb(0x2e, 0x33, 0x3a);
pub const BORDER_STRONG: Color32 = Color32::from_rgb(0x43, 0x4a, 0x54);

// Lanes and grid.
pub const LANE: Color32 = Color32::from_rgb(0x1a, 0x1d, 0x22);
pub const LANE_ALT: Color32 = Color32::from_rgb(0x1d, 0x21, 0x26);
pub const LANE_SELECTED: Color32 = Color32::from_rgb(0x22, 0x28, 0x30);
pub const GRID_BAR: Color32 = Color32::from_rgb(0x34, 0x3a, 0x43);
pub const GRID_BEAT: Color32 = Color32::from_rgb(0x26, 0x2b, 0x31);
pub const GRID_FINE: Color32 = Color32::from_rgb(0x21, 0x25, 0x2a);
/// Piano roll grid: every other bar is shaded; lines by weight (bar > beat > step).
pub const PIANO_ZEBRA: Color32 = Color32::from_rgba_premultiplied(0, 0, 0, 52);
pub const PIANO_GRID_BAR: Color32 = Color32::from_rgb(0x4a, 0x52, 0x5e);
pub const PIANO_GRID_BEAT: Color32 = Color32::from_rgb(0x30, 0x36, 0x3e);
pub const PIANO_GRID_STEP: Color32 = Color32::from_rgba_premultiplied(0x1c, 0x1f, 0x24, 0x90);
/// Time signature tags and the signature lane.
pub const SIGNATURE: Color32 = Color32::from_rgb(0x8f, 0xb8, 0xde);

// Text.
pub const TEXT: Color32 = Color32::from_rgb(0xd8, 0xdc, 0xe2);
pub const TEXT_DIM: Color32 = Color32::from_rgb(0x8c, 0x94, 0x9f);
pub const TEXT_FAINT: Color32 = Color32::from_rgb(0x5d, 0x65, 0x70);

// Accents.
pub const ACCENT: Color32 = Color32::from_rgb(0x3d, 0x9c, 0xf0);
pub const ACCENT_DIM: Color32 = Color32::from_rgb(0x24, 0x4f, 0x78);
pub const PLAYHEAD: Color32 = Color32::from_rgb(0xff, 0x5f, 0x4f);
pub const PLAYING: Color32 = Color32::from_rgb(0x46, 0xc8, 0x6c);
/// Pattern mode (the transport switch, the looped clip span).
pub const PATTERN: Color32 = Color32::from_rgb(0xf0, 0x8c, 0x3c);
/// Selected notes in the piano roll (FL Studio-like warm red).
pub const NOTE_SELECTED: Color32 = Color32::from_rgb(0xf2, 0x6b, 0x6b);
/// The arrange loop region.
pub const LOOP: Color32 = Color32::from_rgb(0x6f, 0xa8, 0xff);
pub const MUTE: Color32 = Color32::from_rgb(0xe3, 0x8b, 0x2c);
pub const SOLO: Color32 = Color32::from_rgb(0xe6, 0xcc, 0x3c);
pub const BYPASS: Color32 = Color32::from_rgb(0x7a, 0x82, 0x8c);
pub const WARNING: Color32 = Color32::from_rgb(0xe6, 0xb4, 0x3c);
pub const ERROR: Color32 = Color32::from_rgb(0xf0, 0x55, 0x55);
/// Record buttons, arm states and the clip being recorded.
pub const RECORD: Color32 = Color32::from_rgb(0xe8, 0x3b, 0x3b);
pub const MARKER: Color32 = Color32::from_rgb(0xc7, 0x8b, 0xf0);
/// Automation curves and points.
pub const AUTOMATION: Color32 = Color32::from_rgb(0x5c, 0xd6, 0xc4);
/// Tempo lane.
pub const TEMPO: Color32 = Color32::from_rgb(0xf0, 0x9a, 0x5a);
/// Background of automation/tempo lanes (a step darker than clip lanes).
pub const AUTO_LANE: Color32 = Color32::from_rgb(0x15, 0x18, 0x1c);

// Meters.
pub const METER_LOW: Color32 = Color32::from_rgb(0x3b, 0xc9, 0x6a);
pub const METER_MID: Color32 = Color32::from_rgb(0xe2, 0xc8, 0x3e);
pub const METER_HIGH: Color32 = Color32::from_rgb(0xf0, 0x4e, 0x45);
pub const METER_BG: Color32 = Color32::from_rgb(0x0c, 0x0e, 0x10);

// Metrics.
pub const RADIUS: u8 = 3;
pub const HEADER_WIDTH: f32 = 216.0;
pub const RULER_HEIGHT: f32 = 26.0;
pub const MARKER_LANE_HEIGHT: f32 = 18.0;
pub const STRIP_WIDTH: f32 = 100.0;
pub const TRANSPORT_HEIGHT: f32 = 46.0;
/// Song overview strip above the arrange ruler.
pub const OVERVIEW_HEIGHT: f32 = 22.0;
/// Thickness of the custom scrollbars (arrange view, piano roll).
pub const SCROLLBAR: f32 = 10.0;
/// Width of the piano roll keyboard.
pub const KEYBOARD_WIDTH: f32 = 64.0;

// Piano roll.
pub const KEY_WHITE: Color32 = Color32::from_rgb(0xd9, 0xdd, 0xe3);
pub const KEY_BLACK: Color32 = Color32::from_rgb(0x1d, 0x20, 0x25);
pub const ROW_WHITE: Color32 = Color32::from_rgb(0x1c, 0x20, 0x25);
pub const ROW_BLACK: Color32 = Color32::from_rgb(0x16, 0x19, 0x1d);
/// Rows of pitches outside the selected scale.
pub const ROW_OUT_OF_SCALE: Color32 = Color32::from_rgb(0x12, 0x14, 0x17);
pub const ROW_ROOT: Color32 = Color32::from_rgb(0x21, 0x29, 0x33);

/// Fallback color for tracks without one.
pub const DEFAULT_TRACK: Color32 = Color32::from_rgb(0x6c, 0x7a, 0x8a);

/// Parses `#RRGGBB` (track colors in the model).
pub fn parse_hex(color: Option<&str>) -> Option<Color32> {
	let hex = color?.strip_prefix('#')?;
	if hex.len() != 6 {
		return None;
	}
	let v = u32::from_str_radix(hex, 16).ok()?;
	Some(Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, v as u8))
}

pub fn to_hex(c: Color32) -> String {
	format!("#{:02X}{:02X}{:02X}", c.r(), c.g(), c.b())
}

pub fn track_color(color: Option<&str>) -> Color32 {
	parse_hex(color).unwrap_or(DEFAULT_TRACK)
}

/// `c` mixed towards `other` by `t` (0..1).
pub fn mix(c: Color32, other: Color32, t: f32) -> Color32 {
	let l = |a: u8, b: u8| (f32::from(a) + (f32::from(b) - f32::from(a)) * t).round() as u8;
	Color32::from_rgb(
		l(c.r(), other.r()),
		l(c.g(), other.g()),
		l(c.b(), other.b()),
	)
}

/// Readable text color on top of `bg`.
pub fn text_on(bg: Color32) -> Color32 {
	let lum = 0.299 * f32::from(bg.r()) + 0.587 * f32::from(bg.g()) + 0.114 * f32::from(bg.b());
	if lum > 150.0 {
		Color32::from_rgb(0x12, 0x14, 0x17)
	} else {
		Color32::from_rgb(0xf2, 0xf4, 0xf7)
	}
}

pub fn small() -> FontId {
	FontId::new(10.5, FontFamily::Proportional)
}

pub fn mono(size: f32) -> FontId {
	FontId::new(size, FontFamily::Monospace)
}

/// Applies the Cadenza style to `ctx`.
pub fn apply(ctx: &egui::Context) {
	ctx.set_theme(egui::Theme::Dark);
	ctx.all_styles_mut(|style| {
		style.text_styles = [
			(
				TextStyle::Small,
				FontId::new(10.5, FontFamily::Proportional),
			),
			(TextStyle::Body, FontId::new(12.5, FontFamily::Proportional)),
			(
				TextStyle::Button,
				FontId::new(12.5, FontFamily::Proportional),
			),
			(
				TextStyle::Heading,
				FontId::new(14.5, FontFamily::Proportional),
			),
			(
				TextStyle::Monospace,
				FontId::new(12.0, FontFamily::Monospace),
			),
		]
		.into();

		let s = &mut style.spacing;
		s.item_spacing = Vec2::new(6.0, 4.0);
		s.button_padding = Vec2::new(7.0, 2.0);
		s.interact_size = Vec2::new(28.0, 20.0);
		s.window_margin = Margin::same(10);
		s.menu_margin = Margin::same(4);
		s.slider_width = 120.0;
		s.combo_width = 120.0;
		s.icon_width = 13.0;
		s.indent = 14.0;

		let v = &mut style.visuals;
		v.dark_mode = true;
		v.override_text_color = None;
		v.panel_fill = BG_PANEL;
		v.window_fill = BG_RAISED;
		v.window_stroke = Stroke::new(1.0, BORDER_STRONG);
		v.window_corner_radius = CornerRadius::same(6);
		v.menu_corner_radius = CornerRadius::same(4);
		v.extreme_bg_color = BG_DEEP;
		v.faint_bg_color = BG_RAISED;
		v.code_bg_color = BG_DEEP;
		v.text_edit_bg_color = Some(BG_DEEP);
		v.hyperlink_color = ACCENT;
		v.warn_fg_color = WARNING;
		v.error_fg_color = ERROR;
		v.selection.bg_fill = ACCENT_DIM;
		v.selection.stroke = Stroke::new(1.0, ACCENT);
		v.striped = true;
		v.slider_trailing_fill = true;
		v.indent_has_left_vline = false;

		let r = CornerRadius::same(RADIUS);
		let w = &mut v.widgets;
		w.noninteractive.bg_fill = BG_PANEL;
		w.noninteractive.weak_bg_fill = BG_PANEL;
		w.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
		w.noninteractive.fg_stroke = Stroke::new(1.0, TEXT_DIM);
		w.noninteractive.corner_radius = r;
		w.inactive.bg_fill = BG_WIDGET;
		w.inactive.weak_bg_fill = BG_WIDGET;
		w.inactive.bg_stroke = Stroke::NONE;
		w.inactive.fg_stroke = Stroke::new(1.0, TEXT);
		w.inactive.corner_radius = r;
		w.hovered.bg_fill = BG_HOVER;
		w.hovered.weak_bg_fill = BG_HOVER;
		w.hovered.bg_stroke = Stroke::new(1.0, BORDER_STRONG);
		w.hovered.fg_stroke = Stroke::new(1.0, Color32::WHITE);
		w.hovered.corner_radius = r;
		w.hovered.expansion = 0.0;
		w.active.bg_fill = ACCENT_DIM;
		w.active.weak_bg_fill = ACCENT_DIM;
		w.active.bg_stroke = Stroke::new(1.0, ACCENT);
		w.active.fg_stroke = Stroke::new(1.0, Color32::WHITE);
		w.active.corner_radius = r;
		w.active.expansion = 0.0;
		w.open.bg_fill = BG_HOVER;
		w.open.weak_bg_fill = BG_HOVER;
		w.open.corner_radius = r;
	});
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn hex_colors_round_trip() {
		let c = parse_hex(Some("#E8A33A")).unwrap();
		assert_eq!(c, Color32::from_rgb(0xe8, 0xa3, 0x3a));
		assert_eq!(to_hex(c), "#E8A33A");
		assert_eq!(parse_hex(Some("E8A33A")), None);
		assert_eq!(parse_hex(Some("#E8A3")), None);
		assert_eq!(track_color(None), DEFAULT_TRACK);
		assert_eq!(
			mix(Color32::BLACK, Color32::WHITE, 0.5),
			Color32::from_rgb(128, 128, 128)
		);
		assert_eq!(text_on(Color32::WHITE), Color32::from_rgb(0x12, 0x14, 0x17));
	}
}
