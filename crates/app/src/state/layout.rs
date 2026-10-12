//! Which panels are visible (FL Studio-style function keys), the "maximize the focused
//! panel" toggle, the keyboard shortcut map shown by Help → Keyboard shortcuts (F1) and
//! the per-user preferences persisted between sessions.

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
			PanelKind::Arrange => "Arrange",
			PanelKind::Browser => "Browser",
			PanelKind::PianoRoll => "Piano Roll",
			PanelKind::Inspector => "Inspector",
			PanelKind::Mixer => "Mixer",
			PanelKind::Chat => "AI Producer",
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
			FollowMode::Off => "Off",
			FollowMode::Page => "Page",
			FollowMode::Continuous => "Continuous",
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
}

impl Default for PanelLayout {
	fn default() -> Self {
		Self {
			arrange: true,
			browser: true,
			piano_roll: false,
			inspector: true,
			mixer: true,
			chat: false,
			maximized: None,
			focus: None,
			mixer_yielded: false,
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
		*self.flag(panel) = true;
		self.maximized = Some(panel);
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
	sc("Windows", "F1", "Keyboard shortcuts (this overlay)"),
	sc("Windows", "F5", "Arrange (playlist)"),
	sc("Windows", "F6 / Alt+F8", "Browser"),
	sc("Windows", "F7", "Piano roll"),
	sc("Windows", "F8", "Inspector"),
	sc("Windows", "F9", "Mixer"),
	sc("Windows", "F10", "AI producer chat"),
	sc(
		"Windows",
		"F11 / Ctrl+Enter",
		"Maximize the focused panel / restore",
	),
	sc("Windows", "F12", "Close all tool panels"),
	sc(
		"Windows",
		"Double-click plugin",
		"Plugin editor window (mixer insert / instrument, track instrument name)",
	),
	sc("Transport", "Space", "Play / stop (in the current mode)"),
	sc("Transport", "L", "Pattern / Song mode"),
	sc("Transport", "Ctrl+L", "Loop region on / off"),
	sc(
		"Transport",
		"Drag ruler top",
		"Set the loop (Ctrl+drag: anywhere)",
	),
	sc("Transport", "Click loop region", "Loop on / off"),
	sc("Transport", "Home / End", "Playhead to song start / end"),
	sc("File", "Ctrl+N / Ctrl+O", "New / open project"),
	sc("File", "Ctrl+S / Ctrl+Shift+S", "Save / save as"),
	sc("File", "Ctrl+I / Ctrl+E", "Import audio / export render"),
	sc("Edit", "Ctrl+Z / Ctrl+Shift+Z", "Undo / redo"),
	sc("Edit", "Ctrl+D", "Duplicate selection"),
	sc("Edit", "Del", "Delete selection"),
	sc("Edit", "S", "Split selected clips at the playhead"),
	sc(
		"Navigate",
		"Wheel / Shift+Wheel",
		"Scroll vertically / horizontally",
	),
	sc("Navigate", "Ctrl+Wheel", "Zoom at the mouse"),
	sc(
		"Navigate",
		"Alt+Wheel",
		"Track height / piano roll key height",
	),
	sc("Navigate", "Middle-drag", "Pan the view"),
	sc("Navigate", "PageUp / PageDown", "Previous / next page"),
	sc(
		"Navigate",
		"Shift+PageUp / Shift+PageDown",
		"Scroll up / down a page",
	),
	sc("Navigate", "Shift+Z / Z", "Zoom to fit / zoom to selection"),
	sc("Clips", "Ctrl+drag", "Copy clips"),
	sc("Clips", "Alt+drag", "Move or resize without snapping"),
	sc("Clips", "Double-click lane", "New MIDI clip"),
	sc("Clips", "Double-click MIDI clip", "Open in the piano roll"),
	sc(
		"Clips",
		"Double-click track header",
		"Piano roll of an instrument track",
	),
	sc(
		"Clips",
		"Drag empty lane",
		"Rubber band select (Shift adds)",
	),
	sc(
		"Clips",
		"Drag left / right edge",
		"Trim the start (content stays) / the end",
	),
	sc(
		"Clips",
		"Double-click audio clip",
		"Open in the audio editor",
	),
	sc(
		"Audio editor",
		"Drag waveform",
		"Select a range (double-click: all)",
	),
	sc(
		"Audio editor",
		"Edges / squares / pill",
		"Region start-end / fades / clip gain",
	),
	sc(
		"Audio editor",
		"S / Del",
		"Split / delete the selected range",
	),
	sc("Audio editor", "Ctrl+A / Esc", "Select all / clear"),
	sc("Audio editor", "Ctrl+Wheel", "Zoom down to samples"),
	sc("Automation", "A", "Show / hide the selected track's lanes"),
	sc("Automation", "Click lane", "Add a point"),
	sc(
		"Automation",
		"Drag point",
		"Move (snapped; Alt: free; Shift: vertical only)",
	),
	sc("Automation", "Drag empty lane", "Rubber band (Shift adds)"),
	sc("Automation", "Ctrl+drag lane", "Draw a ramp"),
	sc("Automation", "Right-click / Del", "Delete points"),
	sc(
		"Automation",
		"Double-click point",
		"Shape: linear, hold, smooth",
	),
	sc(
		"Automation",
		"Right-click a control",
		"Automate (mixer, inspector)",
	),
	sc(
		"Automation",
		"♩ (transport)",
		"Tempo lane: click adds a change",
	),
	sc(
		"Piano roll",
		"F7 (instrument track)",
		"Its clip, or a new one",
	),
	sc("Piano roll", "Clip menu / +", "Switch clip / new clip"),
	sc("Piano roll", "P / E", "Draw tool / select tool"),
	sc(
		"Piano roll",
		"Click / drag (draw)",
		"Add a note / set its length",
	),
	sc(
		"Piano roll",
		"Drag (select)",
		"Rubber band (Shift adds to the selection)",
	),
	sc("Piano roll", "Right-click / Del", "Delete notes"),
	sc("Piano roll", "Up / Down", "Transpose a semitone"),
	sc("Piano roll", "Shift+Up / Shift+Down", "Transpose an octave"),
	sc("Piano roll", "Left / Right", "Move by the grid"),
	sc("Piano roll", "Q", "Quantize to the grid"),
	sc(
		"Piano roll",
		"Ctrl+A / Ctrl+C / Ctrl+V",
		"Select all / copy / paste at the playhead",
	),
	sc(
		"Piano roll",
		"Ctrl+D",
		"Duplicate the selection after itself",
	),
	sc("Piano roll", "Shift+drag note", "Move vertically only"),
	sc(
		"Drag & drop",
		"Device → track",
		"Set instrument / insert effect",
	),
	sc(
		"Drag & drop",
		"Device → below the tracks",
		"New track with that device",
	),
	sc(
		"Drag & drop",
		"Effect → mixer insert slot",
		"Insert at that slot",
	),
	sc(
		"Drag & drop",
		"Insert → another strip",
		"Move it (Ctrl: copy)",
	),
	sc("Drag & drop", "Preset → device / track", "Load the preset"),
	sc(
		"Drag & drop",
		"Audio / MIDI file → arrange",
		"Import at the drop position",
	),
	sc("Drag & drop", ".cadenza.json → window", "Open the project"),
	sc("AI producer", "F10", "Show / hide the chat"),
	sc("AI producer", "Enter / Shift+Enter", "Send / new line"),
	sc(
		"AI producer",
		"Stop",
		"Stop at the next step (edits so far stay, one undo step)",
	),
	sc(
		"AI producer",
		"Ctrl+Z after a request",
		"Undo the whole request",
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
	/// `"<root> <kind>"` (cadenza-theory syntax), empty = no scale.
	pub piano_scale: String,
	pub bookmarks: Vec<PathBuf>,
	/// AI chat settings (no secrets: keys live in the OS keyring).
	pub ai: super::ai::settings::AiSettings,
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
			piano_scale: String::new(),
			bookmarks: super::files::default_bookmarks(),
			ai: super::ai::settings::AiSettings::default(),
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

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
		p.piano_scale = "D dorian".into();
		let json = serde_json::to_string(&p).unwrap();
		let back: Prefs = serde_json::from_str(&json).unwrap();
		assert_eq!(back.px_per_beat, 55.0);
		assert!(!back.panels.inspector);
		assert_eq!(back.piano_scale, "D dorian");
		// Missing fields (older prefs) fall back to defaults.
		let partial: Prefs = serde_json::from_str("{\"snap\": false}").unwrap();
		assert!(!partial.snap && partial.panels.arrange);
	}
}
