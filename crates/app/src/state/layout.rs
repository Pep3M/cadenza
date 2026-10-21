//! Which panels are visible (FL Studio-style function keys), the "maximize the focused
//! panel" toggle, the keyboard shortcut map shown by Help → Keyboard shortcuts (F1) and
//! the per-user preferences persisted between sessions.

use crate::i18n::{mark, tr, trf};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// A dockable part of the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PanelKind {
	/// The arrange view ("playlist").
	Arrange,
	Browser,
	PianoRoll,
	Inspector,
	Mixer,
	/// The AI producer chat (docked right).
	Chat,
}

impl PanelKind {
	pub const ALL: [PanelKind; 6] = [
		PanelKind::Arrange,
		PanelKind::Browser,
		PanelKind::PianoRoll,
		PanelKind::Inspector,
		PanelKind::Mixer,
		PanelKind::Chat,
	];

	pub fn name(self) -> &'static str {
		match self {
			PanelKind::Arrange => tr("Arrange"),
			PanelKind::Browser => tr("Browser"),
			PanelKind::PianoRoll => tr("Piano Roll"),
			PanelKind::Inspector => tr("Inspector"),
			PanelKind::Mixer => tr("Mixer"),
			PanelKind::Chat => tr("AI Producer"),
		}
	}

	/// The function key that toggles the panel (shown in menus and the help overlay).
	pub fn shortcut(self) -> &'static str {
		match self {
			PanelKind::Arrange => "F5",
			PanelKind::Browser => "F6",
			PanelKind::PianoRoll => "F7",
			PanelKind::Inspector => "F8",
			PanelKind::Mixer => "F9",
			PanelKind::Chat => "F10",
		}
	}
}

/// How the arrange view and the piano roll follow the playhead while playing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum FollowMode {
	Off,
	/// Flip to the next page when the playhead leaves the view.
	#[default]
	Page,
	/// Keep the playhead at a fixed spot and scroll the song under it.
	Continuous,
}

impl FollowMode {
	pub fn name(self) -> &'static str {
		match self {
			FollowMode::Off => tr("Off"),
			FollowMode::Page => tr("Page"),
			FollowMode::Continuous => tr("Continuous"),
		}
	}
}

/// Panel visibility. A maximized panel fills the window (except menu, transport and
/// status bar) until the toggle is pressed again; the visibility flags are kept.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PanelLayout {
	pub arrange: bool,
	pub browser: bool,
	pub piano_roll: bool,
	pub inspector: bool,
	pub mixer: bool,
	pub chat: bool,
	#[serde(skip)]
	pub maximized: Option<PanelKind>,
	/// Last panel the user interacted with: target of keyboard edits and of "maximize".
	#[serde(skip)]
	pub focus: Option<PanelKind>,
	/// The piano roll took the bottom dock from the mixer: give it back on close.
	#[serde(skip)]
	pub mixer_yielded: bool,
	/// The AI panel stays docked in windows narrower than `CHAT_DOCK_MIN` (the "Pin" of
	/// its overlay drawer).
	pub chat_pinned: bool,
	/// The browser overlay is open over the arrange (only in windows where the browser is
	/// an icon rail; `browser` then says whether the rail is shown).
	#[serde(skip)]
	pub browser_overlay: bool,
}

impl Default for PanelLayout {
	fn default() -> Self {
		Self {
			arrange: true,
			browser: true,
			piano_roll: false,
			// Floating windows show tracks, clips and devices (FL Studio-like); the
			// inspector is optional (F8).
			inspector: false,
			mixer: true,
			chat: false,
			maximized: None,
			focus: None,
			mixer_yielded: false,
			chat_pinned: false,
			browser_overlay: false,
		}
	}
}

impl PanelLayout {
	/// Whether the panel is switched on (ignoring maximize).
	pub fn shown(&self, panel: PanelKind) -> bool {
		match panel {
			PanelKind::Arrange => self.arrange,
			PanelKind::Browser => self.browser,
			PanelKind::PianoRoll => self.piano_roll,
			PanelKind::Inspector => self.inspector,
			PanelKind::Mixer => self.mixer,
			PanelKind::Chat => self.chat,
		}
	}

	fn flag(&mut self, panel: PanelKind) -> &mut bool {
		match panel {
			PanelKind::Arrange => &mut self.arrange,
			PanelKind::Browser => &mut self.browser,
			PanelKind::PianoRoll => &mut self.piano_roll,
			PanelKind::Inspector => &mut self.inspector,
			PanelKind::Mixer => &mut self.mixer,
			PanelKind::Chat => &mut self.chat,
		}
	}

	/// Whether the panel is drawn this frame.
	pub fn visible(&self, panel: PanelKind) -> bool {
		match self.maximized {
			Some(m) => m == panel,
			None => self.shown(panel),
		}
	}

	pub fn set(&mut self, panel: PanelKind, on: bool) {
		self.maximized = None;
		if panel == PanelKind::PianoRoll && !on && self.mixer_yielded {
			self.mixer = true;
			self.mixer_yielded = false;
		}
		if panel == PanelKind::Mixer {
			self.mixer_yielded = false;
		}
		*self.flag(panel) = on;
		if on {
			self.focus = Some(panel);
		} else if self.focus == Some(panel) {
			self.focus = None;
		}
	}

	/// Function-key behavior: shows a hidden panel, hides a visible one. A key pressed
	/// while another panel is maximized brings that panel up instead.
	pub fn toggle(&mut self, panel: PanelKind) {
		let visible = self.visible(panel);
		if self.maximized.is_some() && !visible {
			self.maximized = None;
			self.set(panel, true);
			return;
		}
		self.set(panel, !visible);
	}

	/// Opens the piano roll in the bottom dock; a visible mixer makes room for it and
	/// comes back when the piano roll is closed.
	pub fn open_piano_roll(&mut self) {
		if self.maximized == Some(PanelKind::PianoRoll) {
			self.focus = Some(PanelKind::PianoRoll);
			return;
		}
		if !self.piano_roll && self.mixer && self.maximized.is_none() {
			self.mixer = false;
			self.mixer_yielded = true;
		}
		self.set(PanelKind::PianoRoll, true);
	}

	/// F11 / Ctrl+Enter: maximizes the focused panel (the arrange view by default), or
	/// restores the layout.
	pub fn toggle_maximize(&mut self) {
		if self.maximized.is_some() {
			self.maximized = None;
			return;
		}
		let panel = self
			.focus
			.filter(|p| self.shown(*p))
			.unwrap_or(PanelKind::Arrange);
		self.maximize(panel);
	}

	/// Maximizes `panel` (showing it), or restores the layout when it already is.
	pub fn toggle_maximize_panel(&mut self, panel: PanelKind) {
		if self.maximized == Some(panel) {
			self.maximized = None;
		} else {
			self.maximize(panel);
		}
	}

	fn maximize(&mut self, panel: PanelKind) {
		*self.flag(panel) = true;
		self.maximized = Some(panel);
		self.focus = Some(panel);
	}

	/// F12: hides every tool panel, leaving the arrange view alone.
	pub fn close_tools(&mut self) {
		self.maximized = None;
		for p in [
			PanelKind::Browser,
			PanelKind::PianoRoll,
			PanelKind::Inspector,
			PanelKind::Mixer,
			PanelKind::Chat,
		] {
			*self.flag(p) = false;
		}
		self.mixer_yielded = false;
		self.browser_overlay = false;
		self.arrange = true;
		self.focus = Some(PanelKind::Arrange);
	}

	/// Which panel takes the central area: the arrange view, or (when it is hidden) the
	/// piano roll, then the mixer. Bottom-docked panels moved to the center are not
	/// drawn at the bottom.
	pub fn center(&self) -> Option<PanelKind> {
		if let Some(m) = self.maximized {
			return Some(m);
		}
		[PanelKind::Arrange, PanelKind::PianoRoll, PanelKind::Mixer]
			.into_iter()
			.find(|p| self.shown(*p))
	}
}

/// Window widths where the shell changes (design 12): below `SIDE_DOCK_MIN` the browser is
/// an icon rail with an overlay and the inspector an overlay; below `CHAT_DOCK_MIN` the AI
/// panel is an overlay drawer unless pinned.
pub const SIDE_DOCK_MIN: f32 = 1600.0;
pub const CHAT_DOCK_MIN: f32 = 1800.0;
/// Width of the browser's icon rail.
pub const RAIL_WIDTH: f32 = 40.0;
/// The arrange keeps at least this much width next to docked side panels.
pub const ARRANGE_MIN_WIDTH: f32 = 480.0;
/// Width ranges of the side panels (min, default, max).
pub const BROWSER_WIDTH: (f32, f32, f32) = (220.0, 250.0, 420.0);
pub const INSPECTOR_WIDTH: (f32, f32, f32) = (260.0, 300.0, 400.0);
pub const CHAT_WIDTH: (f32, f32, f32) = (360.0, 380.0, 640.0);
/// The Mix tab of the AI panel asks for this much.
pub const CHAT_MIX_WIDTH: f32 = 560.0;

/// How the transport bar fits its width: groups fold by priority (design 12: the mode
/// toggles go into a "Modes ▾" popover first, then labels shorten). The position display
/// and the play controls never fold.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TransportLayout {
	/// Click, Count-in, MIDI record, Punch, Automation and Follow live in "Modes ▾".
	pub modes_folded: bool,
	/// Captions above the fields ("Mode", "Tempo", ...).
	pub captions: bool,
	/// "Pattern | Song" (else "Pat | Song").
	pub long_mode: bool,
	/// "⟲ Loop" (else the icon alone).
	pub loop_label: bool,
	/// The clock time next to the bars.beats position.
	pub time: bool,
	/// The sample rate text next to the audio status dot.
	pub audio_text: bool,
	/// Width of the master meter.
	pub meter: f32,
}

/// Widths the transport view draws with (kept here so the fold decision and the view agree).
pub mod transport_widths {
	pub const MARGIN: f32 = 10.0;
	pub const SPACING: f32 = 6.0;
	pub const GROUP_GAP: f32 = 12.0;
	/// Four 34 px buttons, 2 px apart.
	pub const BUTTONS: f32 = 142.0;
	pub const MODE_LONG: f32 = 118.0;
	pub const MODE_SHORT: f32 = 86.0;
	pub const LOOP_LABEL: f32 = 72.0;
	pub const LOOP_ICON: f32 = 32.0;
	pub const CLICK: f32 = 76.0;
	pub const COUNT_IN: f32 = 84.0;
	pub const POSITION: f32 = 150.0;
	pub const TIME: f32 = 78.0;
	pub const TEMPO: f32 = 76.0;
	pub const SIGNATURE: f32 = 92.0;
	pub const MIDI_RECORD: f32 = 80.0;
	pub const PUNCH: f32 = 48.0;
	pub const AUTOMATION: f32 = 70.0;
	pub const FOLLOW: f32 = 90.0;
	pub const MODES: f32 = 84.0;
	pub const AUDIO_TEXT: f32 = 62.0;
	pub const AUDIO_DOT: f32 = 14.0;
	/// Room kept for the dropout counter ("⚠ 12").
	pub const XRUNS: f32 = 44.0;
}

impl TransportLayout {
	const WIDE: Self = Self {
		modes_folded: false,
		captions: true,
		long_mode: true,
		loop_label: true,
		time: true,
		audio_text: true,
		meter: 120.0,
	};

	/// The richest layout that fits `width`; every mode toggle is on the bar from
	/// `SIDE_DOCK_MIN` up (design 12).
	pub fn for_width(width: f32) -> Self {
		if width >= SIDE_DOCK_MIN {
			return Self::WIDE;
		}
		let tiers = [
			Self {
				modes_folded: true,
				..Self::WIDE
			},
			Self {
				modes_folded: true,
				long_mode: false,
				loop_label: false,
				audio_text: false,
				meter: 90.0,
				..Self::WIDE
			},
			Self {
				modes_folded: true,
				captions: false,
				long_mode: false,
				loop_label: false,
				time: false,
				audio_text: false,
				meter: 64.0,
			},
		];
		tiers
			.into_iter()
			.find(|t| t.width() <= width)
			.unwrap_or(tiers[2])
	}

	/// The width the bar needs in this layout (left groups, the gap and the right side).
	pub fn width(&self) -> f32 {
		use transport_widths::*;
		let mut items = vec![
			BUTTONS,
			GROUP_GAP,
			if self.long_mode {
				MODE_LONG
			} else {
				MODE_SHORT
			},
			GROUP_GAP,
			if self.loop_label {
				LOOP_LABEL
			} else {
				LOOP_ICON
			},
		];
		if !self.modes_folded {
			items.extend([CLICK, COUNT_IN]);
		}
		items.extend([GROUP_GAP, POSITION]);
		if self.time {
			items.push(TIME);
		}
		items.extend([TEMPO, SIGNATURE, GROUP_GAP]);
		if self.modes_folded {
			items.push(MODES);
		} else {
			items.extend([MIDI_RECORD, PUNCH, AUTOMATION, FOLLOW]);
		}
		let left: f32 = items.iter().sum::<f32>() + SPACING * items.len() as f32;
		let right = self.meter
			+ AUDIO_DOT
			+ XRUNS + if self.audio_text { AUDIO_TEXT } else { 0.0 }
			+ 3.0 * SPACING;
		MARGIN * 2.0 + left + right
	}
}

/// How the status bar's right side (counts, panel chips) fits next to the message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusLayout {
	/// "7 tracks · 24 clips · 12 undo steps" and "F6 Browser" chips.
	Full,
	/// "7 tr · 24 clips" and key-only chips.
	Short,
	/// Key-only chips, no counts.
	Keys,
}

impl StatusLayout {
	pub fn for_width(width: f32) -> Self {
		if width >= 1400.0 {
			StatusLayout::Full
		} else if width >= 1100.0 {
			StatusLayout::Short
		} else {
			StatusLayout::Keys
		}
	}

	pub fn chip_names(self) -> bool {
		self == StatusLayout::Full
	}

	/// The project counts, `None` when hidden.
	pub fn counts(self, tracks: usize, clips: usize, undo: usize) -> Option<String> {
		match self {
			StatusLayout::Full => Some(trf(
				"{tracks} tracks · {clips} clips · {undo} undo steps",
				&[("tracks", &tracks), ("clips", &clips), ("undo", &undo)],
			)),
			StatusLayout::Short => Some(trf(
				"{tracks} tr · {clips} clips",
				&[("tracks", &tracks), ("clips", &clips)],
			)),
			StatusLayout::Keys => None,
		}
	}
}

/// The shell for one window size (design 12), recomputed every frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Responsive {
	pub width: f32,
	pub height: f32,
	pub transport: TransportLayout,
	pub status: StatusLayout,
	/// The browser is a 40 px icon rail; F6 / an icon opens it as an overlay.
	pub browser_rail: bool,
	/// The inspector opens as an overlay from the right (closes on Esc).
	pub inspector_overlay: bool,
	/// The AI panel is an overlay drawer (unless pinned).
	pub chat_overlay: bool,
}

impl Default for Responsive {
	fn default() -> Self {
		Self::for_window(1600.0, 1000.0, false)
	}
}

impl Responsive {
	pub fn for_window(width: f32, height: f32, chat_pinned: bool) -> Self {
		Self {
			width,
			height,
			transport: TransportLayout::for_width(width),
			status: StatusLayout::for_width(width),
			browser_rail: width < SIDE_DOCK_MIN,
			inspector_overlay: width < SIDE_DOCK_MIN,
			chat_overlay: width < CHAT_DOCK_MIN && !chat_pinned,
		}
	}

	/// Whether `panel` is drawn as an overlay (or a rail) instead of a docked panel.
	pub fn overlay(&self, panel: PanelKind) -> bool {
		match panel {
			PanelKind::Browser => self.browser_rail,
			PanelKind::Inspector => self.inspector_overlay,
			PanelKind::Chat => self.chat_overlay,
			_ => false,
		}
	}

	/// The width range of a docked side panel (`range` = its (min, default, max)) when
	/// `others` points of other docked side panels share the row: the arrange keeps
	/// `ARRANGE_MIN_WIDTH`, the minimum always holds.
	pub fn side_range(&self, range: (f32, f32, f32), others: f32) -> (f32, f32, f32) {
		let (min, default, max) = range;
		let room = (self.width - ARRANGE_MIN_WIDTH - others).max(min);
		let max = max.min(room);
		(min, default.clamp(min, max), max)
	}

	/// Overlay width: `wanted`, at most 60 % of the window, never below `min` (unless the
	/// window itself is narrower).
	pub fn overlay_width(&self, wanted: f32, min: f32) -> f32 {
		wanted
			.min(self.width * 0.6)
			.max(min.min(self.width - RAIL_WIDTH))
	}

	/// Height range of the bottom mixer in `available` points of height (design: 180 px to
	/// 60 %) and its default.
	pub fn mixer_height(available: f32) -> (f32, f32, f32) {
		let max = (available * 0.6).max(180.0);
		(180.0, 470.0f32.min(available * 0.5).clamp(180.0, max), max)
	}

	/// Height range of the bottom editor dock (200 px to 70 %) and its default.
	pub fn editor_height(available: f32) -> (f32, f32, f32) {
		let max = (available * 0.7).max(200.0);
		(200.0, 360.0f32.min(available * 0.55).clamp(200.0, max), max)
	}
}

/// Mixer strips switch to Narrow by themselves when `count` Normal strips (plus the
/// master) do not fit in `width`.
pub fn mixer_auto_narrow(count: usize, width: f32, normal: f32, gap: f32, fixed: f32) -> bool {
	count as f32 * (normal + gap) + fixed > width
}

/// One row of the shortcut map.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shortcut {
	pub section: &'static str,
	pub keys: &'static str,
	pub action: &'static str,
}

const fn sc(section: &'static str, keys: &'static str, action: &'static str) -> Shortcut {
	Shortcut {
		section,
		keys,
		action,
	}
}

/// Every keyboard and mouse shortcut, as shown by the F1 overlay.
pub const SHORTCUTS: &[Shortcut] = &[
	sc(
		mark("Windows"),
		"F1",
		mark("Keyboard shortcuts (this overlay)"),
	),
	sc(mark("Windows"), "F5", mark("Arrange (playlist)")),
	sc(mark("Windows"), "F6 / Alt+F8", mark("Browser")),
	sc(mark("Windows"), "F7", mark("Piano roll")),
	sc(mark("Windows"), "F8", mark("Inspector")),
	sc(mark("Windows"), "F9", mark("Mixer")),
	sc(mark("Windows"), "F10", mark("AI producer chat")),
	sc(
		mark("Windows"),
		"F11 / Ctrl+Enter",
		mark("Maximize the focused panel / restore"),
	),
	sc(mark("Windows"), "F12", mark("Close all tool panels")),
	sc(
		mark("Windows"),
		mark("Double-click plugin"),
		mark("Plugin editor window (mixer insert / instrument, track instrument name)"),
	),
	sc(
		mark("Windows"),
		mark("Click mixer insert"),
		mark("Its floating device window (several stay open; Dock puts one below)"),
	),
	sc(
		mark("Windows"),
		mark("Double-click mixer strip"),
		mark("Track properties window (also: track menu)"),
	),
	sc(
		mark("Windows"),
		mark("Alt+double-click clip"),
		mark("Clip properties window (also: clip menu)"),
	),
	sc(
		mark("Windows"),
		"Esc",
		mark("Close the focused floating window"),
	),
	sc(
		mark("Transport"),
		"Space",
		mark("Play / stop (pattern mode: from the pattern start)"),
	),
	sc(mark("Transport"), "L", mark("Pattern / Song mode")),
	sc(
		mark("Transport"),
		"Ctrl+R",
		mark("Record the armed tracks (again: stop)"),
	),
	sc(mark("Transport"), "R", mark("Record mode: Play records")),
	sc(
		mark("Recording"),
		mark("● on a track"),
		mark("Arm for recording (audio: In + monitor off/auto/on)"),
	),
	sc(
		mark("Recording"),
		mark("T on a track / take lane"),
		mark("Show takes; click a take plays it, drag comps a range"),
	),
	sc(
		mark("Transport"),
		"Ctrl+L",
		mark("Loop region on / off (piano roll: legato)"),
	),
	sc(
		mark("Transport"),
		mark("Drag ruler top"),
		mark("Set the loop (Ctrl+drag: anywhere)"),
	),
	sc(
		mark("Transport"),
		mark("Click loop region"),
		mark("Loop on / off"),
	),
	sc(
		mark("Transport"),
		"Home / End",
		mark("Playhead to song start / end"),
	),
	sc(
		mark("File"),
		mark("Ctrl+N / Ctrl+O"),
		mark("New / open project"),
	),
	sc(
		mark("File"),
		mark("Ctrl+S / Ctrl+Shift+S"),
		mark("Save / save as"),
	),
	sc(mark("File"), "Ctrl+I", mark("Import audio")),
	sc(
		mark("File"),
		"Ctrl+E",
		mark("Export: mixdown, stems, range, format, loudness"),
	),
	sc(
		mark("Edit"),
		mark("Ctrl+Z / Ctrl+Shift+Z"),
		mark("Undo / redo"),
	),
	sc(mark("Edit"), "Ctrl+D", mark("Duplicate selection")),
	sc(mark("Edit"), "Del", mark("Delete selection")),
	sc(
		mark("Edit"),
		"S",
		mark("Split selected clips at the playhead"),
	),
	sc(
		mark("Arrange tools"),
		"V",
		mark("Select tool: select, move, trim clips"),
	),
	sc(
		mark("Arrange tools"),
		"D",
		mark("Draw tool: click / drag an instrument lane draws a clip"),
	),
	sc(
		mark("Arrange tools"),
		"B",
		mark("Paint tool: drag along a lane repeats the clip on the grid"),
	),
	sc(
		mark("Arrange tools"),
		"C",
		mark("Split tool: click splits a clip, drag a line cuts across tracks"),
	),
	sc(
		mark("Arrange tools"),
		"T",
		mark("Mute tool: click / sweep mutes or unmutes clips"),
	),
	sc(
		mark("Arrange tools"),
		mark("Right-drag (Draw, Paint)"),
		mark("Delete every clip swept over"),
	),
	sc(
		mark("Arrange tools"),
		mark("Ctrl+C / Ctrl+X / Ctrl+V"),
		mark("Copy / cut / paste clips"),
	),
	sc(
		mark("Arrange tools"),
		"Ctrl+B",
		mark("Clone the selection right after itself"),
	),
	sc(
		mark("Arrange tools"),
		mark("Shift+Ctrl+V / B / D, Shift+Ctrl+drag"),
		mark("Independent copies (default: MIDI copies are linked)"),
	),
	sc(
		mark("Arrange tools"),
		"Ctrl+G",
		mark("Glue (consolidate) the selected clips"),
	),
	sc(
		mark("Arrange tools"),
		"Q",
		mark("Quantize selected clip starts to the grid"),
	),
	sc(
		mark("Arrange tools"),
		mark("Ctrl+click header"),
		mark("Select every clip on the track"),
	),
	sc(
		mark("Arrange tools"),
		mark("Drag header bottom edge"),
		mark("Resize that track (double-click resets)"),
	),
	sc(
		mark("Navigate"),
		mark("Wheel / Shift+Wheel"),
		mark("Scroll vertically / horizontally"),
	),
	sc(mark("Navigate"), "Ctrl+Wheel", mark("Zoom at the mouse")),
	sc(
		mark("Navigate"),
		"Alt+Wheel",
		mark("Track height / piano roll key height"),
	),
	sc(mark("Navigate"), mark("Middle-drag"), mark("Pan the view")),
	sc(
		mark("Navigate"),
		mark("PageUp / PageDown"),
		mark("Previous / next page"),
	),
	sc(
		mark("Navigate"),
		mark("Shift+PageUp / Shift+PageDown"),
		mark("Scroll up / down a page"),
	),
	sc(
		mark("Navigate"),
		"Shift+Z / Z",
		mark("Zoom to fit / zoom to selection"),
	),
	sc(
		mark("Clips"),
		"Ctrl+drag",
		mark("Copy clips (MIDI: linked copies)"),
	),
	sc(
		mark("Clips"),
		mark("Drag right edge"),
		mark("Resize (MIDI content repeats past its end)"),
	),
	sc(
		mark("Clips"),
		mark("Shift+drag right edge"),
		mark("Stretch a MIDI clip's notes with it"),
	),
	sc(
		mark("Clips"),
		"Alt+drag",
		mark("Move or resize without snapping"),
	),
	sc(
		mark("Clips"),
		mark("Double-click lane"),
		mark("New MIDI clip"),
	),
	sc(
		mark("Clips"),
		mark("Double-click MIDI clip"),
		mark("Open in the piano roll"),
	),
	sc(
		mark("Clips"),
		mark("Double-click track header"),
		mark("Piano roll of an instrument track"),
	),
	sc(
		mark("Clips"),
		mark("Drag empty lane"),
		mark("Rubber band select (Shift adds)"),
	),
	sc(
		mark("Clips"),
		mark("Drag left / right edge"),
		mark("Trim the start (content stays) / the end"),
	),
	sc(
		mark("Clips"),
		mark("Double-click audio clip"),
		mark("Open in the audio editor"),
	),
	sc(
		mark("Audio editor"),
		mark("Drag waveform"),
		mark("Select a range (double-click: all)"),
	),
	sc(
		mark("Audio editor"),
		mark("Edges / squares / pill"),
		mark("Region start-end / fades / clip gain"),
	),
	sc(
		mark("Audio editor"),
		"S / Del",
		mark("Split / delete the selected range"),
	),
	sc(
		mark("Audio editor"),
		"Ctrl+A / Esc",
		mark("Select all / clear"),
	),
	sc(
		mark("Device editor"),
		"A / Shift+A",
		mark("Switch the A/B compare (Shift+A where A–K play notes)"),
	),
	sc(mark("Device editor"), "B", mark("Bypass the device")),
	sc(
		mark("Device editor"),
		"← →",
		mark("Select the band / pad / zone / voice"),
	),
	sc(
		mark("Device editor"),
		mark("↑ ↓ (Shift: fine)"),
		mark("Nudge its value (else the control touched last)"),
	),
	sc(
		mark("Device editor"),
		mark("Alt+arrows"),
		mark("Sampler: move the selected zone"),
	),
	sc(
		mark("Device editor"),
		mark("Right-click a control"),
		mark("Automate, reset, copy / paste, Ask AI about this"),
	),
	sc(
		mark("Device editor"),
		mark("Rest the pointer on a control"),
		mark("Its value and what it does"),
	),
	sc(
		mark("Audio editor"),
		"Ctrl+Wheel",
		mark("Zoom down to samples"),
	),
	sc(
		mark("Automation"),
		"A",
		mark("Show / hide the selected track's lanes"),
	),
	sc(mark("Automation"), mark("Click lane"), mark("Add a point")),
	sc(
		mark("Automation"),
		mark("Drag point"),
		mark("Move (snapped; Alt: free; Shift: vertical only)"),
	),
	sc(
		mark("Automation"),
		mark("Drag empty lane"),
		mark("Rubber band (Shift adds)"),
	),
	sc(
		mark("Automation"),
		mark("Ctrl+drag lane"),
		mark("Draw a ramp"),
	),
	sc(
		mark("Automation"),
		mark("Right-click / Del"),
		mark("Delete points"),
	),
	sc(
		mark("Automation"),
		mark("Double-click point"),
		mark("Shape: linear, hold, smooth"),
	),
	sc(
		mark("Automation"),
		mark("Right-click a control"),
		mark("Automate (mixer, inspector)"),
	),
	sc(
		mark("Automation"),
		mark("♩ (transport)"),
		mark("Tempo lane: click adds a change"),
	),
	sc(
		mark("Piano roll"),
		mark("F7 (instrument track)"),
		mark("Its clip, or a new one"),
	),
	sc(
		mark("Piano roll"),
		mark("Clip menu / +"),
		mark("Switch clip / new clip"),
	),
	sc(
		mark("Piano roll"),
		mark("Alt+wheel over a note"),
		mark("Velocity (the selection when the note is in it)"),
	),
	sc(
		mark("Piano roll"),
		"P / B / E / C",
		mark("Draw / paint / select / slice tool"),
	),
	sc(
		mark("Piano roll"),
		mark("Click empty grid (draw)"),
		mark("Add a note (last length); keep dragging to place it"),
	),
	sc(
		mark("Piano roll"),
		mark("Drag note / its edges"),
		mark("Move (selection) / resize end or start"),
	),
	sc(
		mark("Piano roll"),
		mark("Shift+drag note"),
		mark("Duplicate (drag a copy)"),
	),
	sc(
		mark("Piano roll"),
		mark("Right-click / right-drag"),
		mark("Delete the note / every note swept over"),
	),
	sc(
		mark("Piano roll"),
		mark("Ctrl+drag / Ctrl+click"),
		mark("Rubber band select / toggle a note"),
	),
	sc(mark("Piano roll"), "Alt+drag", mark("No snap")),
	sc(
		mark("Piano roll"),
		mark("Double-click note"),
		mark("Note properties"),
	),
	sc(
		mark("Piano roll"),
		mark("Drag (paint tool)"),
		mark("Paint notes at every step"),
	),
	sc(
		mark("Piano roll"),
		mark("Click / drag (slice tool)"),
		mark("Split notes at the snapped line"),
	),
	sc(
		mark("Piano roll"),
		mark("Up / Down"),
		mark("Transpose a semitone (no selection: all notes)"),
	),
	sc(
		mark("Piano roll"),
		mark("Shift / Ctrl+Up / Down"),
		mark("Transpose an octave"),
	),
	sc(
		mark("Piano roll"),
		mark("Left / Right (Shift)"),
		mark("Move by the snap step (a bar)"),
	),
	sc(
		mark("Piano roll"),
		"Ctrl+B",
		mark("Clone the selection after itself"),
	),
	sc(
		mark("Piano roll"),
		mark("Ctrl+C / Ctrl+X / Ctrl+V"),
		mark("Copy / cut / paste (in place, or at a moved playhead)"),
	),
	sc(
		mark("Piano roll"),
		mark("Ctrl+A / Ctrl+D, Esc"),
		mark("Select all / deselect"),
	),
	sc(mark("Piano roll"), "Del", mark("Delete the selection")),
	sc(
		mark("Piano roll"),
		"Ctrl+Q / Q",
		mark("Quantize to the snap"),
	),
	sc(
		mark("Piano roll"),
		mark("Ctrl+U / Ctrl+L"),
		mark("Glue same-pitch notes / quick legato"),
	),
	sc(
		mark("Piano roll"),
		mark("Velocity lane: drag / Alt or right-drag"),
		mark("Paint velocities / draw a line"),
	),
	sc(
		mark("Piano roll"),
		mark("Keyboard: click / drag"),
		mark("Play / glide notes"),
	),
	sc(
		mark("Drag & drop"),
		mark("Device → track"),
		mark("Set instrument / insert effect"),
	),
	sc(
		mark("Drag & drop"),
		mark("Device → below the tracks"),
		mark("New track with that device"),
	),
	sc(
		mark("Drag & drop"),
		mark("Effect → mixer insert slot"),
		mark("Insert at that slot"),
	),
	sc(
		mark("Drag & drop"),
		mark("Insert → another strip"),
		mark("Move it (Ctrl: copy)"),
	),
	sc(
		mark("Drag & drop"),
		mark("Preset → device / track"),
		mark("Load the preset"),
	),
	sc(
		mark("Drag & drop"),
		mark("Audio / MIDI file → arrange"),
		mark("Import at the drop position"),
	),
	sc(
		mark("Drag & drop"),
		mark(".cdz → window"),
		mark("Open the project"),
	),
	sc(mark("AI producer"), "F10", mark("Show / hide the chat")),
	sc(
		mark("AI producer"),
		mark("Enter / Shift+Enter"),
		mark("Send / new line"),
	),
	sc(
		mark("AI producer"),
		mark("Stop"),
		mark("Stop at the next step (edits so far stay, one undo step)"),
	),
	sc(
		mark("AI producer"),
		mark("Ctrl+Z after a request"),
		mark("Undo the whole request"),
	),
];

/// Per-user preferences saved in the eframe storage (not part of the project).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
	pub panels: PanelLayout,
	pub px_per_beat: f32,
	pub track_height: f32,
	pub snap: bool,
	pub follow: FollowMode,
	pub piano_px_per_beat: f32,
	pub piano_row_height: f32,
	pub piano_velocity_lane: bool,
	pub piano_ghosts: super::piano::Ghosts,
	pub piano_snap: super::piano::PianoSnap,
	/// `"<root> <kind>"` (cadenza-theory syntax), empty = no scale.
	pub piano_scale: String,
	pub bookmarks: Vec<PathBuf>,
	/// AI chat settings (no secrets: keys live in the OS keyring).
	pub ai: super::ai::settings::AiSettings,
	/// File → Open Recent, most recent first.
	pub recent: Vec<PathBuf>,
	/// Autosave interval in minutes (0 = off).
	pub autosave_minutes: u32,
	/// The export dialog's last settings.
	pub export: super::export::ExportSettings,
	/// Audio devices, MIDI input and latency offset.
	pub audio: super::record::AudioPrefs,
	/// Metronome, count-in, punch and MIDI recording options.
	pub record: super::record::RecordSettings,
	/// Mixer strip width and visible sections.
	pub mixer: super::mixer::MixerView,
	/// The arrange pointer tool.
	pub arrange_tool: super::arrange_tool::ArrangeTool,
	/// Per-track heights (header bottom edge drags), by track id.
	pub track_heights: std::collections::HashMap<cadenza_core::TrackId, f32>,
	/// Floating window rects per kind and per item.
	pub windows: super::windows::SavedWindows,
	/// UI language: the OS language ("system") or a language code.
	pub language: crate::i18n::LanguageChoice,
}

impl Default for Prefs {
	fn default() -> Self {
		let view = super::ViewState::default();
		let piano = super::piano::PianoView::default();
		Self {
			panels: view.panels,
			px_per_beat: view.timeline.px_per_beat,
			track_height: view.track_height,
			snap: view.snap,
			follow: view.follow,
			piano_px_per_beat: piano.timeline.px_per_beat,
			piano_row_height: piano.row_height,
			piano_velocity_lane: piano.velocity_lane,
			piano_ghosts: piano.ghosts,
			piano_snap: piano.snap,
			piano_scale: String::new(),
			bookmarks: super::files::default_bookmarks(),
			ai: super::ai::settings::AiSettings::default(),
			recent: Vec::new(),
			autosave_minutes: super::safety::DEFAULT_AUTOSAVE_MINUTES,
			export: super::export::ExportSettings::default(),
			audio: super::record::AudioPrefs::default(),
			record: super::record::RecordSettings::default(),
			mixer: super::mixer::MixerView::default(),
			arrange_tool: view.arrange_tool,
			track_heights: view.track_heights,
			windows: super::windows::SavedWindows::default(),
			language: crate::i18n::LanguageChoice::System,
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn transport_folds_by_priority_and_always_fits() {
		let wide = TransportLayout::for_width(1600.0);
		assert!(!wide.modes_folded && wide.captions && wide.time);
		let mut last = wide;
		for w in [1599.0, 1440.0, 1366.0, 1280.0, 1100.0, 1024.0] {
			let t = TransportLayout::for_width(w);
			assert!(t.modes_folded, "{w}: modes fold under 1600");
			assert!(t.width() <= w, "{w}: needs {}", t.width());
			// Narrower never shows more than wider.
			assert!(!(t.captions && !last.captions) && !(t.time && !last.time));
			last = t;
		}
		let narrow = TransportLayout::for_width(1024.0);
		assert!(!narrow.loop_label && !narrow.long_mode && !narrow.audio_text);
		assert!(TransportLayout::for_width(1366.0).captions);
	}

	#[test]
	fn panels_dock_or_float_by_width() {
		let r = Responsive::for_window(1366.0, 768.0, false);
		assert!(r.browser_rail && r.inspector_overlay && r.chat_overlay);
		assert!(r.overlay(PanelKind::Chat) && !r.overlay(PanelKind::Mixer));
		assert!(!Responsive::for_window(1366.0, 768.0, true).chat_overlay);
		let r = Responsive::for_window(1600.0, 1000.0, false);
		assert!(!r.browser_rail && !r.inspector_overlay && r.chat_overlay);
		let r = Responsive::for_window(2560.0, 1440.0, false);
		assert!(!r.overlay(PanelKind::Browser) && !r.overlay(PanelKind::Chat));
		// Docked side panels leave the arrange its minimum, minimums always hold.
		let r = Responsive::for_window(1600.0, 1000.0, true);
		let (min, default, max) = r.side_range(CHAT_WIDTH, BROWSER_WIDTH.0 + INSPECTOR_WIDTH.0);
		assert_eq!(min, 360.0);
		assert!(max <= 1600.0 - ARRANGE_MIN_WIDTH - 480.0 && default <= max);
		let tight = Responsive::for_window(900.0, 700.0, true);
		assert_eq!(tight.side_range(CHAT_WIDTH, 300.0).2, 360.0);
		assert!(r.overlay_width(900.0, 220.0) <= 960.0);
		assert_eq!(tight.overlay_width(260.0, 220.0), 260.0);
	}

	#[test]
	fn status_bar_and_bottom_docks_shrink() {
		assert_eq!(StatusLayout::for_width(1600.0), StatusLayout::Full);
		assert_eq!(StatusLayout::for_width(1280.0), StatusLayout::Short);
		assert_eq!(StatusLayout::for_width(1024.0), StatusLayout::Keys);
		assert_eq!(StatusLayout::Keys.counts(3, 4, 5), None);
		assert_eq!(
			StatusLayout::Short.counts(3, 4, 5).unwrap(),
			"3 tr · 4 clips"
		);
		assert!(!StatusLayout::Short.chip_names());
		// A short window: the mixer takes at most 60 %, never less than 180 px.
		let (min, default, max) = Responsive::mixer_height(600.0);
		assert_eq!((min, max), (180.0, 360.0));
		assert!(default <= 300.0);
		let (_, default, _) = Responsive::mixer_height(900.0);
		assert_eq!(default, 450.0);
		let (min, _, max) = Responsive::editor_height(600.0);
		assert_eq!((min, max), (200.0, 420.0));
		assert!(mixer_auto_narrow(10, 1000.0, 124.0, 4.0, 300.0));
		assert!(!mixer_auto_narrow(4, 1600.0, 124.0, 4.0, 300.0));
	}

	#[test]
	fn function_keys_toggle_and_maximize() {
		let mut l = PanelLayout::default();
		assert!(l.visible(PanelKind::Mixer));
		l.toggle(PanelKind::Mixer);
		assert!(!l.visible(PanelKind::Mixer));
		l.toggle(PanelKind::Mixer);
		assert!(l.visible(PanelKind::Mixer));
		assert_eq!(l.focus, Some(PanelKind::Mixer));

		// Maximize the focused panel: only it is visible, flags are kept.
		l.toggle_maximize();
		assert_eq!(l.maximized, Some(PanelKind::Mixer));
		assert!(!l.visible(PanelKind::Arrange) && !l.visible(PanelKind::Browser));
		assert_eq!(l.center(), Some(PanelKind::Mixer));
		l.toggle_maximize();
		assert!(l.visible(PanelKind::Arrange) && l.visible(PanelKind::Browser));

		// A function key while maximized brings that panel up and restores the layout.
		l.toggle_maximize();
		l.toggle(PanelKind::Browser);
		assert_eq!(l.maximized, None);
		assert!(l.visible(PanelKind::Browser) && l.visible(PanelKind::Mixer));

		l.close_tools();
		for p in PanelKind::ALL {
			assert_eq!(l.visible(p), p == PanelKind::Arrange, "{p:?}");
		}
	}

	#[test]
	fn piano_roll_borrows_the_mixer_dock() {
		let mut l = PanelLayout::default();
		l.open_piano_roll();
		assert!(l.visible(PanelKind::PianoRoll) && !l.visible(PanelKind::Mixer));
		l.toggle(PanelKind::PianoRoll);
		assert!(l.visible(PanelKind::Mixer), "mixer restored");
		// Hidden mixer: stays hidden.
		l.toggle(PanelKind::Mixer);
		l.open_piano_roll();
		l.set(PanelKind::PianoRoll, false);
		assert!(!l.visible(PanelKind::Mixer));
		// Without the arrange view the piano roll takes the center.
		l.open_piano_roll();
		l.toggle(PanelKind::Arrange);
		assert_eq!(l.center(), Some(PanelKind::PianoRoll));
	}

	#[test]
	fn maximized_piano_roll_stays_maximized_when_switching_clips() {
		let mut l = PanelLayout::default();
		l.open_piano_roll();
		l.toggle_maximize_panel(PanelKind::PianoRoll);
		assert_eq!(l.center(), Some(PanelKind::PianoRoll));
		assert!(!l.visible(PanelKind::Arrange));
		l.open_piano_roll();
		assert_eq!(l.maximized, Some(PanelKind::PianoRoll));
		l.toggle_maximize_panel(PanelKind::PianoRoll);
		assert_eq!(l.maximized, None);
		assert!(l.visible(PanelKind::Arrange) && l.visible(PanelKind::PianoRoll));
	}

	#[test]
	fn every_panel_has_a_documented_key() {
		for p in PanelKind::ALL {
			assert!(
				SHORTCUTS.iter().any(|s| s.keys.contains(p.shortcut())),
				"{p:?}"
			);
		}
	}

	#[test]
	fn prefs_round_trip_through_serde() {
		let mut p = Prefs::default();
		p.panels.toggle(PanelKind::Inspector);
		p.px_per_beat = 55.0;
		p.windows.remember(
			"device/eq".into(),
			"device:x".into(),
			crate::state::windows::WinRect::new(1.0, 2.0, 300.0, 200.0),
		);
		p.piano_scale = "D dorian".into();
		let json = serde_json::to_string(&p).unwrap();
		let back: Prefs = serde_json::from_str(&json).unwrap();
		assert_eq!(back.px_per_beat, 55.0);
		assert!(back.panels.inspector);
		assert_eq!(back.windows, p.windows);
		assert_eq!(back.piano_scale, "D dorian");
		// Missing fields (older prefs) fall back to defaults.
		let partial: Prefs = serde_json::from_str("{\"snap\": false}").unwrap();
		assert!(!partial.snap && partial.panels.arrange);
	}
}
