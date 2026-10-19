//! The one place that defines how Cadenza looks: a small dark palette, metrics and the
//! egui style built from them. Views use these constants instead of literal colors.

use egui::{Color32, CornerRadius, FontFamily, FontId, Margin, Stroke, TextStyle, Vec2};

// Graphite design system (docs/design/redesign-brief). Surfaces, darkest to lightest:
// menus and wells, content canvas, docked panels, raised groups, controls, hover, pressed.
pub const BG_DEEP: Color32 = Color32::from_rgb(0x0e, 0x0f, 0x11);
pub const CANVAS: Color32 = Color32::from_rgb(0x11, 0x12, 0x15);
pub const BG_PANEL: Color32 = Color32::from_rgb(0x15, 0x16, 0x1a);
pub const BG_RAISED: Color32 = Color32::from_rgb(0x1c, 0x1d, 0x21);
pub const BG_WIDGET: Color32 = Color32::from_rgb(0x21, 0x23, 0x28);
pub const BG_HOVER: Color32 = Color32::from_rgb(0x2a, 0x2c, 0x32);
pub const BG_PRESSED: Color32 = Color32::from_rgb(0x36, 0x39, 0x40);
pub const BORDER_SUBTLE: Color32 = Color32::from_rgb(0x1d, 0x1f, 0x23);
pub const BORDER: Color32 = Color32::from_rgb(0x24, 0x26, 0x2b);
pub const BORDER_STRONG: Color32 = Color32::from_rgb(0x36, 0x39, 0x40);

// Lanes and grid: the canvas sits below the panels so clips and notes pop.
pub const LANE: Color32 = CANVAS;
pub const LANE_ALT: Color32 = Color32::from_rgb(0x13, 0x14, 0x17);
pub const LANE_SELECTED: Color32 = Color32::from_rgb(0x16, 0x19, 0x1f);
pub const GRID_BAR: Color32 = Color32::from_rgb(0x22, 0x24, 0x29);
pub const GRID_BEAT: Color32 = Color32::from_rgb(0x19, 0x1a, 0x1e);
pub const GRID_FINE: Color32 = Color32::from_rgb(0x15, 0x16, 0x19);
/// Piano roll grid: every other bar is shaded; lines by weight (bar > beat > step).
pub const PIANO_ZEBRA: Color32 = Color32::from_rgba_premultiplied(0, 0, 0, 40);
pub const PIANO_GRID_BAR: Color32 = Color32::from_rgb(0x30, 0x33, 0x3a);
pub const PIANO_GRID_BEAT: Color32 = Color32::from_rgb(0x20, 0x22, 0x27);
pub const PIANO_GRID_STEP: Color32 = Color32::from_rgba_premultiplied(0x15, 0x16, 0x1a, 0x90);
/// Time signature tags and the signature lane.
pub const SIGNATURE: Color32 = Color32::from_rgb(0x9a, 0xb4, 0xd8);

// Text (contrast on BG_PANEL: 14.8, 7.6 and 4.7 : 1).
pub const TEXT: Color32 = Color32::from_rgb(0xe8, 0xe9, 0xec);
pub const TEXT_DIM: Color32 = Color32::from_rgb(0xa3, 0xa6, 0xad);
pub const TEXT_FAINT: Color32 = Color32::from_rgb(0x80, 0x84, 0x8c);
/// Text on accent fills.
pub const TEXT_ON_ACCENT: Color32 = Color32::from_rgb(0x0b, 0x12, 0x20);
/// Dark ink on solid track colors and state fills.
pub const TEXT_ON_COLOR: Color32 = Color32::from_rgb(0x10, 0x11, 0x14);

// Accent (Cadenza blue): focus, selection, primary action, everything the AI touched.
pub const ACCENT: Color32 = Color32::from_rgb(0x4c, 0x9c, 0xf5);
pub const ACCENT_HOVER: Color32 = Color32::from_rgb(0x6a, 0xae, 0xf7);
pub const ACCENT_PRESS: Color32 = Color32::from_rgb(0x3a, 0x86, 0xdc);
pub const ACCENT_TEXT: Color32 = Color32::from_rgb(0x7f, 0xb4, 0xff);
/// Selected row / segment / chat bubble fill (`accent-soft`).
pub const ACCENT_DIM: Color32 = Color32::from_rgb(0x1d, 0x2a, 0x3d);
/// Outline of `ACCENT_DIM` fills.
pub const ACCENT_BORDER: Color32 = Color32::from_rgb(0x2c, 0x4a, 0x73);

// Semantic states; each also carries an icon or a label. `*_SOFT` = toggle on fills.
pub const PLAYHEAD: Color32 = TEXT;
pub const PLAYING: Color32 = Color32::from_rgb(0x4f, 0xc3, 0x8a);
pub const PLAYING_SOFT: Color32 = Color32::from_rgb(0x1d, 0x3a, 0x2b);
/// Pattern mode (the transport switch, the looped clip span).
pub const PATTERN: Color32 = Color32::from_rgb(0x8f, 0x8c, 0xf5);
pub const PATTERN_SOFT: Color32 = Color32::from_rgb(0x24, 0x23, 0x4a);
/// Selected notes in the piano roll: the note color lifted towards white.
pub const NOTE_SELECTED: Color32 = Color32::from_rgb(0xf6, 0xd3, 0x8a);
/// The arrange loop region.
pub const LOOP: Color32 = Color32::from_rgb(0x5a, 0xc8, 0xd8);
pub const LOOP_SOFT: Color32 = Color32::from_rgb(0x15, 0x32, 0x3a);
pub const MUTE: Color32 = Color32::from_rgb(0xf0, 0x8a, 0x4b);
pub const MUTE_SOFT: Color32 = Color32::from_rgb(0x3a, 0x26, 0x17);
pub const SOLO: Color32 = Color32::from_rgb(0xec, 0xd0, 0x4f);
pub const SOLO_SOFT: Color32 = Color32::from_rgb(0x37, 0x32, 0x1a);
pub const BYPASS: Color32 = Color32::from_rgb(0x7a, 0x7e, 0x86);
pub const WARNING: Color32 = Color32::from_rgb(0xf2, 0xa9, 0x44);
pub const WARNING_SOFT: Color32 = Color32::from_rgb(0x3a, 0x2c, 0x17);
pub const ERROR: Color32 = Color32::from_rgb(0xf0, 0x52, 0x4f);
pub const ERROR_SOFT: Color32 = Color32::from_rgb(0x2a, 0x17, 0x18);
/// Record buttons, arm states and the clip being recorded.
pub const RECORD: Color32 = Color32::from_rgb(0xf0, 0x52, 0x4f);
pub const RECORD_SOFT: Color32 = Color32::from_rgb(0x3a, 0x1a, 0x1b);
pub const MARKER: Color32 = Color32::from_rgb(0xe8, 0xb8, 0x4a);
/// Automation curves and points.
pub const AUTOMATION: Color32 = Color32::from_rgb(0xc8, 0x84, 0xe8);
pub const AUTOMATION_SOFT: Color32 = Color32::from_rgb(0x2a, 0x23, 0x33);
/// Input monitoring (speaker button).
pub const MONITOR: Color32 = Color32::from_rgb(0xf2, 0xa9, 0x44);
/// Automation write mode (Touch / Latch).
pub const AUTOMATION_WRITE: Color32 = Color32::from_rgb(0xf0, 0x52, 0x4f);
/// Frozen track badge.
pub const FROZEN: Color32 = Color32::from_rgb(0x8c, 0xc8, 0xf0);
pub const FROZEN_SOFT: Color32 = Color32::from_rgb(0x17, 0x28, 0x3a);
/// Tempo lane curve and BPM change points.
pub const TEMPO: Color32 = Color32::from_rgb(0xee, 0x86, 0xb4);
/// Background of automation/tempo lanes (a step darker than clip lanes).
pub const AUTO_LANE: Color32 = Color32::from_rgb(0x0f, 0x10, 0x13);

// Meters: peak bar full width, RMS core brighter.
pub const METER_LOW: Color32 = Color32::from_rgb(0x4f, 0xc3, 0x8a);
pub const METER_MID: Color32 = Color32::from_rgb(0xe8, 0xc9, 0x4a);
pub const METER_HIGH: Color32 = Color32::from_rgb(0xf0, 0x52, 0x4f);
pub const METER_BG: Color32 = BG_DEEP;
pub const METER_PEAK_HOLD: Color32 = TEXT;

// Device editors (docs/design/redesign-brief/designs/14 Device Kit): displays sit one
// step below the panel; bands and voices take the 8-colour palette left to right.
pub const DISPLAY_BG: Color32 = Color32::from_rgb(0x0b, 0x0c, 0x0e);
/// Coral, orange, yellow, green, teal, sky, violet, pink: equal lightness, so no band
/// looks more important than another.
pub const BAND_COLORS: [Color32; 8] = [
	Color32::from_rgb(0xff, 0x7b, 0x6e),
	Color32::from_rgb(0xff, 0xa9, 0x4d),
	Color32::from_rgb(0xf2, 0xd4, 0x5c),
	Color32::from_rgb(0x7d, 0xdc, 0x7a),
	Color32::from_rgb(0x4f, 0xd6, 0xc8),
	Color32::from_rgb(0x5a, 0xa2, 0xf5),
	Color32::from_rgb(0x9d, 0x8c, 0xff),
	Color32::from_rgb(0xe8, 0x84, 0xd6),
];
/// Function colours, outside the palette (AI and selection use [`ACCENT`]).
pub const GAIN_REDUCTION: Color32 = Color32::from_rgb(0xff, 0x7a, 0x5c);
/// Gain reduction numbers (readouts, scales).
pub const GAIN_REDUCTION_TEXT: Color32 = Color32::from_rgb(0xff, 0x9a, 0x80);
/// A dynamics device's output level (history line, makeup).
pub const LEVEL_OUTPUT: Color32 = Color32::from_rgb(0x8c, 0xc8, 0xf0);
pub const STEREO_L: Color32 = BAND_COLORS[4];
pub const STEREO_R: Color32 = BAND_COLORS[1];
/// Readout of an automated control.
pub const AUTOMATION_TEXT: Color32 = Color32::from_rgb(0xd9, 0xa8, 0xf0);

/// Strokes a display curve with its glow: one wide faint stroke under the 1.6 px line.
pub fn glow_line(painter: &egui::Painter, points: Vec<egui::Pos2>, color: Color32) {
	painter.add(egui::Shape::line(
		points.clone(),
		Stroke::new(6.0, color.gamma_multiply(0.18)),
	));
	painter.add(egui::Shape::line(points, Stroke::new(1.6, color)));
}

/// Clip body = this share of the track color over the lane (selected: the larger one).
pub const CLIP_FILL_ALPHA: f32 = 0.15;
pub const CLIP_FILL_ALPHA_SELECTED: f32 = 0.32;

// Metrics.
pub const RADIUS: u8 = 3;
pub const RADIUS_MD: u8 = 4;
pub const RADIUS_LG: u8 = 6;
pub const RADIUS_XL: u8 = 8;

/// Floating windows over the workspace (a popped-out device editor).
pub const FLOATING_SHADOW: egui::Shadow = egui::Shadow {
	offset: [0, 20],
	blur: 56,
	spread: 0,
	color: Color32::from_black_alpha(165),
};
pub const HEADER_WIDTH: f32 = 232.0;
pub const RULER_HEIGHT: f32 = 26.0;
pub const MARKER_LANE_HEIGHT: f32 = 20.0;
pub const STRIP_WIDTH: f32 = 124.0;
/// Mixer strips in the narrow view.
pub const STRIP_NARROW: f32 = 76.0;
pub const TRANSPORT_HEIGHT: f32 = 48.0;
/// Panel title bars and toolbars.
pub const TAB_HEIGHT: f32 = 28.0;
/// Song overview strip above the arrange ruler.
pub const OVERVIEW_HEIGHT: f32 = 22.0;
/// Thickness of the custom scrollbars (arrange view, piano roll).
pub const SCROLLBAR: f32 = 10.0;
/// Width of the piano roll keyboard.
pub const KEYBOARD_WIDTH: f32 = 64.0;

// Piano roll.
pub const KEY_WHITE: Color32 = Color32::from_rgb(0xc9, 0xcc, 0xd2);
pub const KEY_BLACK: Color32 = Color32::from_rgb(0x1c, 0x1d, 0x21);
pub const ROW_WHITE: Color32 = Color32::from_rgb(0x15, 0x16, 0x1a);
pub const ROW_BLACK: Color32 = Color32::from_rgb(0x11, 0x12, 0x15);
/// Rows of pitches outside the selected scale.
pub const ROW_OUT_OF_SCALE: Color32 = Color32::from_rgb(0x0e, 0x0f, 0x11);
pub const ROW_ROOT: Color32 = Color32::from_rgb(0x1a, 0x21, 0x2c);

/// The 16 track colors: readable on dark lanes and as clip fills with dark ink.
pub const TRACK_COLORS: [&str; 16] = [
	"#E8705F", "#EC8D4F", "#E3B24F", "#B5C94F", "#6CC46A", "#4FC3A1", "#4FBFD1", "#5AA2F5",
	"#7489F2", "#9A82EC", "#B97BE0", "#D878C8", "#E07FB0", "#E87F8A", "#C2A27A", "#8A93A3",
];

/// Fallback color for tracks without one.
pub const DEFAULT_TRACK: Color32 = Color32::from_rgb(0x8a, 0x93, 0xa3);

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

/// Caption / secondary text (labels over fields, hints, badges).
pub fn small() -> FontId {
	FontId::new(11.0, FontFamily::Proportional)
}

pub fn mono(size: f32) -> FontId {
	FontId::new(size, FontFamily::Monospace)
}

/// IBM Plex Sans Medium: buttons, tabs, segmented controls.
pub fn medium(size: f32) -> FontId {
	FontId::new(size, FontFamily::Name(MEDIUM.into()))
}

/// IBM Plex Sans SemiBold: panel titles, track and strip names.
pub fn semibold(size: f32) -> FontId {
	FontId::new(size, FontFamily::Name(SEMIBOLD.into()))
}

/// Panel titles and track names (13 / 600).
pub fn heading() -> FontId {
	semibold(13.0)
}

/// IBM Plex Mono Medium: the position counter.
pub fn mono_medium(size: f32) -> FontId {
	FontId::new(size, FontFamily::Name(MONO_MEDIUM.into()))
}

const MEDIUM: &str = "medium";
const SEMIBOLD: &str = "semibold";
const MONO_MEDIUM: &str = "mono-medium";

/// IBM Plex (OFL-1.1, `assets/fonts`) for UI and numbers. egui has no font weights, so
/// each weight is its own family. egui's bundled faces stay as fallbacks: Ubuntu + emoji,
/// and Hack for arrows, geometric shapes and block elements (`←`, `→`, `◆`, `●`, `⊘`, `▍`).
pub fn fonts() -> egui::FontDefinitions {
	let mut fonts = egui::FontDefinitions::default();
	let faces: [(&str, &'static [u8]); 5] = [
		(
			"plex-sans",
			include_bytes!("../assets/fonts/IBMPlexSans-Regular.ttf"),
		),
		(
			"plex-sans-medium",
			include_bytes!("../assets/fonts/IBMPlexSans-Medium.ttf"),
		),
		(
			"plex-sans-semibold",
			include_bytes!("../assets/fonts/IBMPlexSans-SemiBold.ttf"),
		),
		(
			"plex-mono",
			include_bytes!("../assets/fonts/IBMPlexMono-Regular.ttf"),
		),
		(
			"plex-mono-medium",
			include_bytes!("../assets/fonts/IBMPlexMono-Medium.ttf"),
		),
	];
	for (name, data) in faces {
		fonts
			.font_data
			.insert(name.to_owned(), egui::FontData::from_static(data).into());
	}
	let proportional = fonts.families.entry(FontFamily::Proportional).or_default();
	if !proportional.iter().any(|f| f == "Hack") {
		proportional.push("Hack".to_owned());
	}
	let fallback = proportional.clone();
	proportional.insert(0, "plex-sans".to_owned());
	let monospace = fonts.families.entry(FontFamily::Monospace).or_default();
	monospace.insert(0, "plex-mono".to_owned());
	let mono_fallback = monospace.clone();
	for (family, face, rest) in [
		(MEDIUM, "plex-sans-medium", &fallback),
		(SEMIBOLD, "plex-sans-semibold", &fallback),
		(MONO_MEDIUM, "plex-mono-medium", &mono_fallback),
	] {
		let mut list = vec![face.to_owned()];
		list.extend(rest.iter().cloned());
		fonts.families.insert(FontFamily::Name(family.into()), list);
	}
	fonts
}

fn applied_id() -> egui::Id {
	egui::Id::new("cadenza-theme-applied")
}

/// Whether [`apply`] already ran on `ctx` (a fresh context needs the fonts again).
pub fn applied(ctx: &egui::Context) -> bool {
	ctx.data(|d| d.get_temp::<bool>(applied_id()).unwrap_or(false))
}

/// Applies the Cadenza style to `ctx`.
pub fn apply(ctx: &egui::Context) {
	ctx.data_mut(|d| d.insert_temp(applied_id(), true));
	ctx.set_fonts(fonts());
	ctx.set_theme(egui::Theme::Dark);
	ctx.all_styles_mut(|style| {
		style.text_styles = [
			(TextStyle::Small, small()),
			(TextStyle::Body, FontId::new(12.0, FontFamily::Proportional)),
			(TextStyle::Button, medium(12.0)),
			(TextStyle::Heading, semibold(15.0)),
			(TextStyle::Monospace, mono(11.5)),
		]
		.into();

		let s = &mut style.spacing;
		s.item_spacing = Vec2::new(6.0, 4.0);
		s.button_padding = Vec2::new(8.0, 3.0);
		s.interact_size = Vec2::new(28.0, 22.0);
		s.window_margin = Margin::same(12);
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
		v.window_corner_radius = CornerRadius::same(RADIUS_XL);
		v.menu_corner_radius = CornerRadius::same(RADIUS_LG);
		v.window_shadow = egui::Shadow {
			offset: [0, 8],
			blur: 24,
			spread: 0,
			color: Color32::from_black_alpha(140),
		};
		v.popup_shadow = egui::Shadow {
			offset: [0, 4],
			blur: 12,
			spread: 0,
			color: Color32::from_black_alpha(120),
		};
		v.extreme_bg_color = BG_DEEP;
		v.faint_bg_color = BG_RAISED;
		v.code_bg_color = BG_DEEP;
		v.text_edit_bg_color = Some(BG_DEEP);
		v.hyperlink_color = ACCENT_TEXT;
		v.warn_fg_color = WARNING;
		v.error_fg_color = ERROR;
		v.selection.bg_fill = ACCENT_DIM;
		v.selection.stroke = Stroke::new(1.0, ACCENT);
		v.striped = true;
		v.slider_trailing_fill = true;
		v.indent_has_left_vline = false;

		let r = CornerRadius::same(RADIUS_MD);
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
		w.hovered.fg_stroke = Stroke::new(1.0, TEXT);
		w.hovered.corner_radius = r;
		w.hovered.expansion = 0.0;
		w.active.bg_fill = BG_PRESSED;
		w.active.weak_bg_fill = BG_PRESSED;
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

	/// Every non-ASCII character the app draws in proportional text has a glyph in one of
	/// the proportional faces (no empty boxes): the symbols are scanned from the sources.
	#[test]
	fn proportional_fonts_cover_the_app_symbols() {
		use skrifa::MetadataProvider;
		let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
		let mut symbols = std::collections::BTreeSet::new();
		let mut stack = vec![dir];
		while let Some(d) = stack.pop() {
			for e in std::fs::read_dir(&d).unwrap().flatten() {
				let p = e.path();
				if p.is_dir() {
					stack.push(p);
				} else if p.extension().is_some_and(|x| x == "rs") {
					let text = std::fs::read_to_string(&p).unwrap();
					for line in text.lines().filter(|l| !l.trim_start().starts_with("//")) {
						symbols.extend(line.chars().filter(|c| !c.is_ascii()));
					}
				}
			}
		}
		assert!(symbols.contains(&'◆') && symbols.contains(&'●'));
		let defs = fonts();
		let faces: Vec<_> = defs.families[&FontFamily::Proportional]
			.iter()
			.map(|name| &defs.font_data[name])
			.collect();
		let covered = |c: char| {
			faces.iter().any(|data| {
				skrifa::FontRef::from_index(&data.font, data.index)
					.is_ok_and(|f| f.charmap().map(c).is_some())
			})
		};
		let missing: String = symbols.iter().filter(|&&c| !covered(c)).collect();
		assert!(missing.is_empty(), "no glyph for {missing:?}");
		assert!(covered('←'), "arrows for library/back links");
	}
}
