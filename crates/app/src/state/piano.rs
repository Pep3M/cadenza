//! Piano roll: view state (its own zoom/scroll, tool, note selection, scale, clipboard)
//! and the pure builders that turn note gestures into model [`Command`]s.
//!
//! Positions in the view are absolute song time (the ruler matches the arrange view);
//! notes are stored relative to their clip. Every gesture becomes ONE command (a
//! `Batch` when it needs several), executed through `AppState::apply`.
//!
//! Editing past the clip end grows the clip (an `UpdateClip` in the same batch, rounded
//! up to the next bar) instead of clamping: what you draw is what plays, and the clip on
//! the arrange view follows. Notes cannot go before the clip start (they are clamped):
//! growing the clip to the left would move its start and shift every other note.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use cadenza_core::{ClipId, Meter, NoteId, PPQ, Tick, TimeSignature, TrackId};
use cadenza_model::controller::splice;
use cadenza_model::{Clip, ClipPatch, Command, Note, NotePatch, Project, TrackKind};
use cadenza_model::{Controller, ControllerPoint};
use cadenza_theory::Scale;
use serde::{Deserialize, Serialize};

use super::timeline::{Timeline, snap_to};

pub const DEFAULT_ROW_HEIGHT: f32 = 14.0;
pub const MIN_ROW_HEIGHT: f32 = 6.0;
pub const MAX_ROW_HEIGHT: f32 = 40.0;
/// Shortest note a resize or a drawn note can produce.
pub const MIN_NOTE_LENGTH: Tick = Tick(PPQ / 32);

/// Piano roll tools (FL Studio-like: the draw tool does nearly everything).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tool {
	/// Click adds a note and keeps dragging it; notes move/resize, Ctrl+drag selects.
	#[default]
	Draw,
	/// Drag paints consecutive notes at the snap step (pitch follows the mouse).
	Paint,
	/// Click selects, drag on empty space draws a rubber band.
	Select,
	/// Click or drag vertically across notes to split them at the snapped position.
	Slice,
}

impl Tool {
	pub const ALL: [Tool; 4] = [Tool::Draw, Tool::Paint, Tool::Select, Tool::Slice];

	pub fn label(self) -> &'static str {
		match self {
			Tool::Draw => "✏ Draw",
			Tool::Paint => "🎨 Paint",
			Tool::Select => "⛶ Select",
			Tool::Slice => "✂ Slice",
		}
	}

	pub fn key(self) -> &'static str {
		match self {
			Tool::Draw => "P",
			Tool::Paint => "B",
			Tool::Select => "E",
			Tool::Slice => "C",
		}
	}

	pub fn tip(self) -> &'static str {
		match self {
			Tool::Draw => {
				"Click adds a note (drag to place it), drag notes to move, edges to resize, \
				 right-drag deletes, Ctrl+drag selects, Shift+drag duplicates"
			}
			Tool::Paint => "Drag to paint notes at every snap step",
			Tool::Select => "Click selects, drag a rubber band (Shift/Ctrl adds)",
			Tool::Slice => "Click or drag vertically across notes to split them",
		}
	}
}

/// The piano roll's snap ("magnet" in FL Studio). `Main` follows the global snap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum PianoSnap {
	/// The arrange view's snap (off when the global snap is off).
	Main,
	Off,
	Bar,
	Beat,
	HalfBeat,
	/// A sixteenth (a quarter of a beat), FL's "step".
	#[default]
	Step,
	HalfStep,
	/// Beat triplets (a third of a beat).
	ThirdBeat,
	/// Step triplets (a sixth of a beat).
	ThirdStep,
}

impl PianoSnap {
	pub const ALL: [PianoSnap; 9] = [
		PianoSnap::Main,
		PianoSnap::Off,
		PianoSnap::Bar,
		PianoSnap::Beat,
		PianoSnap::HalfBeat,
		PianoSnap::Step,
		PianoSnap::HalfStep,
		PianoSnap::ThirdBeat,
		PianoSnap::ThirdStep,
	];

	pub fn name(self) -> &'static str {
		match self {
			PianoSnap::Main => "Main snap",
			PianoSnap::Off => "No snap",
			PianoSnap::Bar => "Bar",
			PianoSnap::Beat => "Beat",
			PianoSnap::HalfBeat => "1/2 beat",
			PianoSnap::Step => "Step (1/16)",
			PianoSnap::HalfStep => "1/2 step",
			PianoSnap::ThirdBeat => "1/3 beat",
			PianoSnap::ThirdStep => "1/6 beat",
		}
	}

	/// The snap step; `main` is the global snap's step (`None` when it is off).
	pub fn step(self, ts: TimeSignature, main: Option<Tick>) -> Option<Tick> {
		let beat = PPQ * 4 / i64::from(ts.denominator.max(1));
		match self {
			PianoSnap::Main => main,
			PianoSnap::Off => None,
			PianoSnap::Bar => Some(ts.ticks_per_bar()),
			PianoSnap::Beat => Some(Tick(beat)),
			PianoSnap::HalfBeat => Some(Tick(beat / 2)),
			PianoSnap::Step => Some(Tick(PPQ / 4)),
			PianoSnap::HalfStep => Some(Tick(PPQ / 8)),
			PianoSnap::ThirdBeat => Some(Tick(beat / 3)),
			PianoSnap::ThirdStep => Some(Tick(beat / 6)),
		}
	}
}

/// Copied notes (starts relative to the earliest one) and where they came from.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct NoteClipboard {
	pub notes: Vec<Note>,
	/// Absolute song position of the earliest copied note.
	pub origin: Tick,
	/// The playhead when the notes were copied.
	pub cue: Tick,
}

impl NoteClipboard {
	/// Where Ctrl+V pastes (absolute): at the playhead when it was moved since the copy
	/// (and the transport is stopped), else at the original position, like FL Studio.
	pub fn paste_at(&self, playhead: Tick, playing: bool) -> Tick {
		if !playing && playhead != self.cue {
			playhead
		} else {
			self.origin
		}
	}
}

/// Notes of other clips shown dimmed behind the edited clip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Ghosts {
	Off,
	/// Other MIDI clips of the same track.
	#[default]
	SameTrack,
	/// Every other MIDI clip of the song.
	AllTracks,
}

impl Ghosts {
	pub fn name(self) -> &'static str {
		match self {
			Ghosts::Off => "No ghost notes",
			Ghosts::SameTrack => "Ghosts: this track",
			Ghosts::AllTracks => "Ghosts: all tracks",
		}
	}
}

#[derive(Debug, Clone, PartialEq)]
pub struct PianoView {
	/// The edited MIDI clip.
	pub clip: Option<ClipId>,
	pub tool: Tool,
	/// Horizontal zoom/scroll in song beats (independent of the arrange view).
	pub timeline: Timeline,
	/// Pixels per semitone row.
	pub row_height: f32,
	/// Pixels scrolled from the top row (pitch 127).
	pub scroll_y: f32,
	pub selected: BTreeSet<NoteId>,
	/// Length and velocity of the last drawn or edited note (used for new notes).
	pub last_length: Tick,
	pub last_velocity: f32,
	/// Rows outside the scale are dimmed.
	pub scale: Option<Scale>,
	pub ghosts: Ghosts,
	pub velocity_lane: bool,
	/// What the event lane under the notes edits (velocity or a controller).
	pub event_lane: EventLane,
	pub snap: PianoSnap,
	pub clipboard: NoteClipboard,
	/// Clip the view was last framed for (scroll/zoom to its notes once on open).
	pub framed: Option<ClipId>,
	/// Last clip opened per track (F7 on a track reopens it).
	pub recent: HashMap<TrackId, ClipId>,
}

impl Default for PianoView {
	fn default() -> Self {
		Self {
			clip: None,
			tool: Tool::Draw,
			timeline: Timeline {
				px_per_beat: 80.0,
				scroll_beats: 0.0,
			},
			row_height: DEFAULT_ROW_HEIGHT,
			scroll_y: (127 - 84) as f32 * DEFAULT_ROW_HEIGHT,
			selected: BTreeSet::new(),
			last_length: Tick(PPQ / 4),
			last_velocity: 0.8,
			scale: None,
			ghosts: Ghosts::SameTrack,
			velocity_lane: true,
			event_lane: EventLane::Velocity,
			snap: PianoSnap::default(),
			clipboard: NoteClipboard::default(),
			framed: None,
			recent: HashMap::new(),
		}
	}
}

impl PianoView {
	/// Y (relative to the top of the note grid) of the top of `pitch`'s row.
	pub fn pitch_to_y(&self, pitch: u8) -> f32 {
		f32::from(127 - pitch.min(127)) * self.row_height - self.scroll_y
	}

	/// Pitch of the row at `y` (relative to the grid top), clamped to 0..=127.
	pub fn y_to_pitch(&self, y: f32) -> u8 {
		let row = ((y + self.scroll_y) / self.row_height).floor();
		(127.0 - row).clamp(0.0, 127.0) as u8
	}

	pub fn content_height(&self) -> f32 {
		128.0 * self.row_height
	}

	/// Changes the row height keeping the pitch under `anchor_y` in place.
	pub fn zoom_rows(&mut self, factor: f32, anchor_y: f32, view_h: f32) {
		let anchor = (anchor_y + self.scroll_y) / self.row_height;
		self.row_height = (self.row_height * factor).clamp(MIN_ROW_HEIGHT, MAX_ROW_HEIGHT);
		self.scroll_y = super::timeline::clamp_scroll(
			anchor * self.row_height - anchor_y,
			self.content_height(),
			view_h,
		);
	}

	/// Scrolls/zooms to show `clip`: centered on its notes' pitch range (C4 when empty),
	/// its span horizontally.
	pub fn frame(&mut self, clip: &Clip, width: f32, view_h: f32) {
		self.framed = Some(clip.id);
		let notes = clip.notes();
		let (lo, hi) = notes.iter().fold((u8::MAX, 0u8), |(lo, hi), n| {
			(lo.min(n.pitch), hi.max(n.pitch))
		});
		let center = if notes.is_empty() {
			60.0
		} else {
			(f32::from(lo) + f32::from(hi)) * 0.5
		};
		self.scroll_y = super::timeline::clamp_scroll(
			(127.0 - center) * self.row_height - view_h * 0.5,
			self.content_height(),
			view_h,
		);
		if width > 0.0 {
			self.timeline
				.zoom_to_range(clip.start.beats(), clip.end().beats(), width);
		}
	}

	/// Selects `ids` (replacing the selection, or adding with `add`).
	pub fn select(&mut self, ids: impl IntoIterator<Item = NoteId>, add: bool) {
		if !add {
			self.selected.clear();
		}
		self.selected.extend(ids);
	}

	/// Drops selected ids that are not notes of the edited clip any more.
	pub fn prune(&mut self, project: &Project) {
		let clip = self.clip.and_then(|c| project.find_clip(c).map(|(_, c)| c));
		match clip {
			Some(c) if c.is_midi() => {
				self.selected
					.retain(|id| c.notes().iter().any(|n| n.id == *id));
			}
			_ => {
				self.clip = None;
				self.selected.clear();
			}
		}
		self.recent.retain(|t, c| {
			project
				.find_clip(*c)
				.is_some_and(|(track, _)| track.id == *t)
		});
	}
}

/// The MIDI clip of instrument track `track` the piano roll opens for it: a selected clip
/// of that track, else the clip under the playhead, else the last one opened there.
/// `None` when the track has none of those (create one with [`new_clip`]).
pub fn clip_for_track(
	project: &Project,
	track: TrackId,
	selected: &BTreeSet<ClipId>,
	playhead: Tick,
	recent: Option<ClipId>,
) -> Option<ClipId> {
	let t = project.track(track)?;
	if t.kind != TrackKind::Instrument {
		return None;
	}
	let midi = || t.clips.iter().filter(|c| c.is_midi());
	midi()
		.find(|c| selected.contains(&c.id))
		.or_else(|| midi().find(|c| c.start <= playhead && playhead < c.end()))
		.or_else(|| midi().find(|c| Some(c.id) == recent))
		.map(|c| c.id)
}

/// An empty one-bar MIDI clip named after instrument track `track`, at the first bar
/// from `at` (snapped down to the bar) that no clip of the track covers.
pub fn new_clip(project: &Project, track: TrackId, at: Tick) -> Option<(Command, ClipId)> {
	let t = project.track(track)?;
	if t.kind != TrackKind::Instrument {
		return None;
	}
	let meter = project.meter();
	let mut start = meter.bar_floor(Tick(at.0.max(0)));
	while let Some(end) = t
		.clips
		.iter()
		.filter(|c| c.start < meter.bar_at(start).end() && start < c.end())
		.map(|c| c.end())
		.max()
	{
		start = meter.bar_ceil(end);
	}
	let clip = Clip::midi(t.name.clone(), start, meter.bar_at(start).length);
	let id = clip.id;
	Some((Command::AddClip { track, clip }, id))
}

/// Snaps an absolute position to `step` (no snapping when `step` is `None`).
pub fn snap_abs(tick: Tick, step: Option<Tick>) -> Tick {
	match step {
		Some(s) => snap_to(tick, s),
		None => Tick(tick.0.max(0)),
	}
}

/// Snaps down to the grid line at or before `tick` (for the start of a drawn note).
pub fn snap_floor(tick: Tick, step: Option<Tick>) -> Tick {
	match step {
		Some(s) => {
			let s = s.0.max(1);
			Tick(tick.0.max(0).div_euclid(s) * s)
		}
		None => Tick(tick.0.max(0)),
	}
}

/// The clip length needed for notes ending at `end` (relative): unchanged when they fit,
/// else grown to the next bar line (absolute bars, so the clip end lands on the grid).
pub fn grown_length(clip: &Clip, end: Tick, meter: Meter<'_>) -> Option<Tick> {
	if end <= clip.length {
		return None;
	}
	Some(meter.bar_ceil(clip.start + end) - clip.start)
}

fn grow(clip: &Clip, end: Tick, meter: Meter<'_>) -> Option<Command> {
	grown_length(clip, end, meter).map(|length| Command::UpdateClip {
		clip: clip.id,
		patch: ClipPatch {
			length: Some(length),
			..ClipPatch::default()
		},
	})
}

/// One undo step with a readable label (even for a single command).
fn labelled(label: &str, commands: Vec<Command>) -> Option<Command> {
	(!commands.is_empty()).then(|| Command::Batch {
		label: label.into(),
		commands,
	})
}

fn midi_clip(project: &Project, clip: ClipId) -> Option<&Clip> {
	project
		.find_clip(clip)
		.map(|(_, c)| c)
		.filter(|c| c.is_midi())
}

fn selected<'a>(clip: &'a Clip, ids: &BTreeSet<NoteId>) -> Vec<&'a Note> {
	clip.notes()
		.iter()
		.filter(|n| ids.contains(&n.id))
		.collect()
}

/// Adds one note (`start` relative to the clip, clamped at 0). Returns the command and
/// the new note's id.
pub fn add_note(
	project: &Project,
	clip: ClipId,
	start: Tick,
	pitch: u8,
	length: Tick,
	velocity: f32,
) -> Option<(Command, NoteId)> {
	let c = midi_clip(project, clip)?;
	let start = Tick(start.0.max(0));
	let note = Note::new(
		pitch.min(127),
		start,
		Tick(length.0.max(MIN_NOTE_LENGTH.0)),
		velocity.clamp(0.0, 1.0),
	);
	let id = note.id;
	let mut commands: Vec<Command> = grow(c, note.end(), project.meter()).into_iter().collect();
	commands.push(Command::AddNotes {
		clip,
		notes: vec![note],
	});
	Some((labelled("Add note", commands)?, id))
}

/// Clamps a move so every note stays at or after the clip start and inside 0..=127.
pub fn clamp_move(clip: &Clip, ids: &BTreeSet<NoteId>, dt: Tick, dp: i32) -> (Tick, i32) {
	let notes = selected(clip, ids);
	let Some(earliest) = notes.iter().map(|n| n.start).min() else {
		return (Tick::ZERO, 0);
	};
	let lo = notes.iter().map(|n| i32::from(n.pitch)).min().unwrap_or(0);
	let hi = notes
		.iter()
		.map(|n| i32::from(n.pitch))
		.max()
		.unwrap_or(127);
	(Tick(dt.0.max(-earliest.0)), dp.clamp(-lo, 127 - hi))
}

/// Moves notes by `dt` and `dp` semitones (clamped, see [`clamp_move`]); the clip grows
/// when notes go past its end.
pub fn move_notes(
	project: &Project,
	clip: ClipId,
	ids: &BTreeSet<NoteId>,
	dt: Tick,
	dp: i32,
) -> Option<Command> {
	let c = midi_clip(project, clip)?;
	let (dt, dp) = clamp_move(c, ids, dt, dp);
	if dt.0 == 0 && dp == 0 {
		return None;
	}
	let notes = selected(c, ids);
	let end = notes.iter().map(|n| n.end() + dt).max()?;
	let mut commands: Vec<Command> = grow(c, end, project.meter()).into_iter().collect();
	commands.push(Command::UpdateNotes {
		clip,
		notes: notes
			.iter()
			.map(|n| NotePatch {
				id: n.id,
				pitch: (dp != 0).then(|| (i32::from(n.pitch) + dp) as u8),
				velocity: None,
				start: (dt.0 != 0).then_some(n.start + dt),
				length: None,
			})
			.collect(),
	});
	labelled(
		if dt.0 == 0 {
			"Transpose notes"
		} else {
			"Move notes"
		},
		commands,
	)
}

/// Changes the length of notes by `delta` (each at least [`MIN_NOTE_LENGTH`]).
pub fn resize_notes(
	project: &Project,
	clip: ClipId,
	ids: &BTreeSet<NoteId>,
	delta: Tick,
) -> Option<Command> {
	let c = midi_clip(project, clip)?;
	let notes = selected(c, ids);
	let patches: Vec<(NotePatch, Tick)> = notes
		.iter()
		.filter_map(|n| {
			let length = Tick((n.length + delta).0.max(MIN_NOTE_LENGTH.0));
			(length != n.length).then(|| {
				(
					NotePatch {
						id: n.id,
						pitch: None,
						velocity: None,
						start: None,
						length: Some(length),
					},
					n.start + length,
				)
			})
		})
		.collect();
	let end = patches.iter().map(|(_, end)| *end).max()?;
	let mut commands: Vec<Command> = grow(c, end, project.meter()).into_iter().collect();
	commands.push(Command::UpdateNotes {
		clip,
		notes: patches.into_iter().map(|(p, _)| p).collect(),
	});
	labelled("Resize notes", commands)
}

pub fn delete_notes(project: &Project, clip: ClipId, ids: &BTreeSet<NoteId>) -> Option<Command> {
	let c = midi_clip(project, clip)?;
	let notes: Vec<NoteId> = selected(c, ids).iter().map(|n| n.id).collect();
	(!notes.is_empty()).then_some(Command::RemoveNotes { clip, notes })
}

/// Pastes clipboard notes (starts relative to the first one) at `at` (relative to the
/// clip, clamped at 0) with fresh ids.
pub fn paste_notes(
	project: &Project,
	clip: ClipId,
	clipboard: &[Note],
	at: Tick,
) -> Option<(Command, Vec<NoteId>)> {
	let c = midi_clip(project, clip)?;
	let at = Tick(at.0.max(0));
	let notes = clipboard
		.iter()
		.map(|n| Note {
			id: NoteId::new(),
			start: at + n.start,
			..n.clone()
		})
		.collect();
	insert_notes(project, c, notes, "Paste notes")
}

fn insert_notes(
	project: &Project,
	c: &Clip,
	notes: Vec<Note>,
	label: &str,
) -> Option<(Command, Vec<NoteId>)> {
	let end = notes.iter().map(Note::end).max()?;
	let ids = notes.iter().map(|n| n.id).collect();
	let mut commands: Vec<Command> = grow(c, end, project.meter()).into_iter().collect();
	commands.push(Command::AddNotes { clip: c.id, notes });
	Some((labelled(label, commands)?, ids))
}

/// The selected notes with starts relative to the earliest one (for the clipboard).
pub fn copy_notes(clip: &Clip, ids: &BTreeSet<NoteId>) -> Vec<Note> {
	let notes = selected(clip, ids);
	let Some(start) = notes.iter().map(|n| n.start).min() else {
		return Vec::new();
	};
	notes
		.into_iter()
		.map(|n| Note {
			start: n.start - start,
			..n.clone()
		})
		.collect()
}

/// Moves note starts onto the absolute grid `step` (`strength` 0..1 like the MCP
/// `quantize_notes` tool); lengths are kept. Empty `ids` quantizes the whole clip.
pub fn quantize_notes(
	project: &Project,
	clip: ClipId,
	ids: &BTreeSet<NoteId>,
	step: Tick,
	strength: f64,
) -> Option<Command> {
	let c = midi_clip(project, clip)?;
	let strength = strength.clamp(0.0, 1.0);
	let patches: Vec<NotePatch> = c
		.notes()
		.iter()
		.filter(|n| ids.is_empty() || ids.contains(&n.id))
		.filter_map(|n| {
			let abs = c.start + n.start;
			let target = snap_to(abs, step);
			let moved = abs.0 + ((target.0 - abs.0) as f64 * strength).round() as i64;
			let start = Tick((moved - c.start.0).max(0));
			(start != n.start).then_some(NotePatch {
				id: n.id,
				pitch: None,
				velocity: None,
				start: Some(start),
				length: None,
			})
		})
		.collect();
	if patches.is_empty() {
		return None;
	}
	let end = patches
		.iter()
		.filter_map(|p| {
			let n = c.notes().iter().find(|n| n.id == p.id)?;
			Some(p.start? + n.length)
		})
		.max()?;
	let mut commands: Vec<Command> = grow(c, end, project.meter()).into_iter().collect();
	commands.push(Command::UpdateNotes {
		clip,
		notes: patches,
	});
	labelled("Quantize notes", commands)
}

/// Sets velocities (0..1) of several notes in one command.
pub fn set_velocities(
	project: &Project,
	clip: ClipId,
	values: &HashMap<NoteId, f32>,
) -> Option<Command> {
	let c = midi_clip(project, clip)?;
	let notes: Vec<NotePatch> = c
		.notes()
		.iter()
		.filter_map(|n| {
			let v = values.get(&n.id)?.clamp(0.0, 1.0);
			((v - n.velocity).abs() > 1e-4).then_some(NotePatch {
				id: n.id,
				pitch: None,
				velocity: Some(v),
				start: None,
				length: None,
			})
		})
		.collect();
	(!notes.is_empty()).then_some(Command::UpdateNotes { clip, notes })
}

/// Notes an Alt+wheel over `hovered` changes (FL Studio): the selection when the note is
/// in it, else that note alone.
pub fn wheel_targets(selected: &BTreeSet<NoteId>, hovered: NoteId) -> BTreeSet<NoteId> {
	if selected.contains(&hovered) {
		selected.clone()
	} else {
		BTreeSet::from([hovered])
	}
}

/// Velocity change of one wheel notch (0..1 units).
pub const WHEEL_VELOCITY_STEP: f32 = 0.02;

/// The velocities of `ids` moved by `delta` from their values in `clip` (clamped 0..1;
/// a note never drops below 1 % so it keeps sounding).
pub fn nudge_velocities(clip: &Clip, ids: &BTreeSet<NoteId>, delta: f32) -> HashMap<NoteId, f32> {
	clip.notes()
		.iter()
		.filter(|n| ids.contains(&n.id))
		.map(|n| (n.id, (n.velocity + delta).clamp(0.01, 1.0)))
		.collect()
}

/// What the piano roll's event lane shows (FL Studio's event selector).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum EventLane {
	#[default]
	Velocity,
	Controller(Controller),
}

impl EventLane {
	/// The lanes offered first in the selector (any CC# can be typed too).
	pub const COMMON: [EventLane; 8] = [
		EventLane::Velocity,
		EventLane::Controller(Controller::Cc(1)),
		EventLane::Controller(Controller::Cc(11)),
		EventLane::Controller(Controller::Cc(64)),
		EventLane::Controller(Controller::PitchBend),
		EventLane::Controller(Controller::ChannelPressure),
		EventLane::Controller(Controller::Cc(74)),
		EventLane::Controller(Controller::Cc(7)),
	];

	pub fn label(self) -> String {
		match self {
			EventLane::Velocity => "Velocity".into(),
			EventLane::Controller(Controller::Cc(74)) => "Brightness (CC 74)".into(),
			EventLane::Controller(c) => c.label(),
		}
	}
}

/// Snaps a lane value to what the controller can hold (switches are on/off; MIDI
/// resolution otherwise).
pub fn controller_value(controller: Controller, v: f32) -> f32 {
	let (lo, hi) = controller.range();
	let v = v.clamp(lo, hi);
	if controller.is_switch() {
		if v >= 0.5 { 1.0 } else { 0.0 }
	} else if controller == Controller::PitchBend {
		(v * 8191.0).round() / 8191.0
	} else {
		(v * 127.0).round() / 127.0
	}
}

/// Pencil stroke in a controller lane: `cells` (clip-relative tick -> value, one per
/// grid cell the pointer crossed) replace the events in their span.
pub fn controller_paint(
	old: &[ControllerPoint],
	cells: &BTreeMap<Tick, f32>,
) -> Option<Vec<ControllerPoint>> {
	let (&first, _) = cells.first_key_value()?;
	let (&last, _) = cells.last_key_value()?;
	let new: Vec<ControllerPoint> = cells
		.iter()
		.map(|(&t, &v)| ControllerPoint::new(t, v))
		.collect();
	Some(splice(old, first, last, &new))
}

/// The SetControllerLane for `points` on the lane, `None` when nothing changes. Points
/// outside the clip are dropped.
pub fn set_controller(
	project: &Project,
	clip: ClipId,
	controller: Controller,
	points: Vec<ControllerPoint>,
) -> Option<Command> {
	let c = midi_clip(project, clip)?;
	let points: Vec<ControllerPoint> = points
		.into_iter()
		.filter(|p| p.tick >= Tick::ZERO && p.tick < c.length)
		.map(|p| ControllerPoint::new(p.tick, controller_value(controller, p.value)))
		.collect();
	let old = c.controller(controller).map_or(&[][..], |l| &l.points[..]);
	(old != points.as_slice()).then_some(Command::SetControllerLane {
		clip,
		controller,
		points,
	})
}

/// Removes the event at `tick` (clip-relative) from the lane.
pub fn controller_delete(old: &[ControllerPoint], tick: Tick) -> Vec<ControllerPoint> {
	old.iter().filter(|p| p.tick != tick).copied().collect()
}

/// Notes overlapping the relative time range `t0..t1` and the pitch range `lo..=hi`.
pub fn notes_in_rect(clip: &Clip, t0: Tick, t1: Tick, lo: u8, hi: u8) -> Vec<NoteId> {
	let (t0, t1) = (t0.min(t1), t0.max(t1));
	let (lo, hi) = (lo.min(hi), lo.max(hi));
	clip.notes()
		.iter()
		.filter(|n| n.start < t1 && n.end() > t0 && (lo..=hi).contains(&n.pitch))
		.map(|n| n.id)
		.collect()
}

/// Topmost note at relative time `t` and `pitch` (latest start wins).
pub fn note_at(clip: &Clip, t: Tick, pitch: u8) -> Option<&Note> {
	clip.notes()
		.iter()
		.filter(|n| n.pitch == pitch && n.start <= t && t < n.end())
		.max_by_key(|n| n.start)
}

/// The notes a keyboard edit applies to: the selection, or every note of the clip when
/// nothing is selected (FL Studio).
pub fn targets(clip: &Clip, selected: &BTreeSet<NoteId>) -> BTreeSet<NoteId> {
	if selected.is_empty() {
		clip.notes().iter().map(|n| n.id).collect()
	} else {
		selected.clone()
	}
}

fn ceil_to(tick: Tick, step: Tick) -> Tick {
	let s = step.0.max(1);
	Tick((tick.0 + s - 1).div_euclid(s) * s)
}

/// How far Ctrl+B shifts the copies: the selection's span (earliest start to latest end,
/// absolute) widened to the snap grid; spans of a bar or more round up to whole bars.
pub fn clone_offset(
	clip: &Clip,
	ids: &BTreeSet<NoteId>,
	step: Option<Tick>,
	ts: TimeSignature,
) -> Option<Tick> {
	let notes = selected(clip, ids);
	let start = clip.start + notes.iter().map(|n| n.start).min()?;
	let end = clip.start + notes.iter().map(|n| n.end()).max()?;
	let span = match step {
		Some(s) => ceil_to(end, s) - snap_floor(start, Some(s)),
		None => end - start,
	};
	let bar = ts.ticks_per_bar();
	Some(if span >= bar {
		ceil_to(span, bar)
	} else {
		span
	})
}

/// Ctrl+B (FL Studio "duplicate"): copies the notes right after the selection (see
/// [`clone_offset`]). Returns the command and the ids of the copies (select them so a
/// repeated Ctrl+B keeps appending).
pub fn clone_notes(
	project: &Project,
	clip: ClipId,
	ids: &BTreeSet<NoteId>,
	step: Option<Tick>,
) -> Option<(Command, Vec<NoteId>)> {
	let c = midi_clip(project, clip)?;
	let offset = clone_offset(c, ids, step, project.meter().signature_at(c.start))?;
	let copies: Vec<Note> = selected(c, ids)
		.iter()
		.map(|n| Note {
			id: NoteId::new(),
			start: n.start + offset,
			..(*n).clone()
		})
		.collect();
	insert_notes(project, c, copies, "Clone notes")
}

/// Copies of `ids` moved by `dt`/`dp` (clamped like a move), for Shift+drag.
pub fn copy_moved(
	project: &Project,
	clip: ClipId,
	ids: &BTreeSet<NoteId>,
	dt: Tick,
	dp: i32,
) -> Option<(Command, Vec<NoteId>)> {
	let c = midi_clip(project, clip)?;
	let (dt, dp) = clamp_move(c, ids, dt, dp);
	let copies: Vec<Note> = selected(c, ids)
		.iter()
		.map(|n| Note {
			id: NoteId::new(),
			start: n.start + dt,
			pitch: (i32::from(n.pitch) + dp) as u8,
			..(*n).clone()
		})
		.collect();
	insert_notes(project, c, copies, "Duplicate notes")
}

/// Ctrl+X: deletes the notes (copy them first).
pub fn cut_notes(project: &Project, clip: ClipId, ids: &BTreeSet<NoteId>) -> Option<Command> {
	labelled("Cut notes", vec![delete_notes(project, clip, ids)?])
}

/// The clipboard for `ids`, remembering their absolute position and the playhead.
pub fn clipboard(clip: &Clip, ids: &BTreeSet<NoteId>, playhead: Tick) -> NoteClipboard {
	let origin = selected(clip, ids)
		.iter()
		.map(|n| clip.start + n.start)
		.min()
		.unwrap_or(clip.start);
	NoteClipboard {
		notes: copy_notes(clip, ids),
		origin,
		cue: playhead,
	}
}

/// Moves the starts of notes by `delta`, keeping their ends (left-edge resize); each
/// stays inside the clip and at least [`MIN_NOTE_LENGTH`] long.
pub fn resize_start_notes(
	project: &Project,
	clip: ClipId,
	ids: &BTreeSet<NoteId>,
	delta: Tick,
) -> Option<Command> {
	let c = midi_clip(project, clip)?;
	let notes: Vec<NotePatch> = selected(c, ids)
		.iter()
		.filter_map(|n| {
			let end = n.end();
			let start = Tick(
				(n.start + delta)
					.0
					.clamp(0, (end - MIN_NOTE_LENGTH).0.max(0)),
			);
			(start != n.start).then_some(NotePatch {
				id: n.id,
				pitch: None,
				velocity: None,
				start: Some(start),
				length: Some(end - start),
			})
		})
		.collect();
	(!notes.is_empty()).then(|| Command::Batch {
		label: "Resize notes".into(),
		commands: vec![Command::UpdateNotes { clip, notes }],
	})
}

/// Ctrl+U (glue): joins the selected notes of each pitch into one note from the first
/// start to the last end. `None` when no pitch has two selected notes.
pub fn glue_notes(project: &Project, clip: ClipId, ids: &BTreeSet<NoteId>) -> Option<Command> {
	let c = midi_clip(project, clip)?;
	let mut by_pitch: std::collections::BTreeMap<u8, Vec<&Note>> = Default::default();
	for n in selected(c, ids) {
		by_pitch.entry(n.pitch).or_default().push(n);
	}
	let mut patches = Vec::new();
	let mut removed = Vec::new();
	for notes in by_pitch.values_mut().filter(|v| v.len() > 1) {
		notes.sort_by_key(|n| (n.start, n.id));
		let first = notes[0];
		let end = notes.iter().map(|n| n.end()).max()?;
		patches.push(NotePatch {
			id: first.id,
			pitch: None,
			velocity: None,
			start: None,
			length: Some(end - first.start),
		});
		removed.extend(notes[1..].iter().map(|n| n.id));
	}
	if patches.is_empty() {
		return None;
	}
	labelled(
		"Glue notes",
		vec![
			Command::RemoveNotes {
				clip,
				notes: removed,
			},
			Command::UpdateNotes {
				clip,
				notes: patches,
			},
		],
	)
}

/// Ctrl+L (quick legato): every note lasts until the next later start among the
/// targeted notes; the last ones keep their length.
pub fn legato_notes(project: &Project, clip: ClipId, ids: &BTreeSet<NoteId>) -> Option<Command> {
	let c = midi_clip(project, clip)?;
	let notes = selected(c, ids);
	let starts: BTreeSet<Tick> = notes.iter().map(|n| n.start).collect();
	let patches: Vec<NotePatch> = notes
		.iter()
		.filter_map(|n| {
			let next = *starts.range(n.start + Tick(1)..).next()?;
			let length = next - n.start;
			(length != n.length).then_some(NotePatch {
				id: n.id,
				pitch: None,
				velocity: None,
				start: None,
				length: Some(length),
			})
		})
		.collect();
	labelled(
		"Legato",
		(!patches.is_empty())
			.then_some(Command::UpdateNotes {
				clip,
				notes: patches,
			})
			.into_iter()
			.collect(),
	)
}

/// Splits the notes with a pitch in `lo..=hi` that sound across `at` (relative): the
/// note ends at `at` and a copy starts there (slice tool).
pub fn slice_notes(project: &Project, clip: ClipId, at: Tick, lo: u8, hi: u8) -> Option<Command> {
	let c = midi_clip(project, clip)?;
	let (lo, hi) = (lo.min(hi), lo.max(hi));
	let cut: Vec<&Note> = c
		.notes()
		.iter()
		.filter(|n| (lo..=hi).contains(&n.pitch))
		.filter(|n| n.start + MIN_NOTE_LENGTH <= at && at + MIN_NOTE_LENGTH <= n.end())
		.collect();
	if cut.is_empty() {
		return None;
	}
	labelled(
		"Slice notes",
		vec![
			Command::UpdateNotes {
				clip,
				notes: cut
					.iter()
					.map(|n| NotePatch {
						id: n.id,
						pitch: None,
						velocity: None,
						start: None,
						length: Some(at - n.start),
					})
					.collect(),
			},
			Command::AddNotes {
				clip,
				notes: cut
					.iter()
					.map(|n| Note {
						id: NoteId::new(),
						start: at,
						length: n.end() - at,
						..(*n).clone()
					})
					.collect(),
			},
		],
	)
}

/// Paint tool: the cell index of `tick` on a grid of `spacing` starting at `first`.
pub fn paint_cell(first: Tick, spacing: Tick, tick: Tick) -> i64 {
	(tick - first).0.div_euclid(spacing.0.max(1))
}

/// Adds painted notes (`(start relative, pitch)`) of one length and velocity as one
/// command; starts before the clip are dropped.
pub fn paint_notes(
	project: &Project,
	clip: ClipId,
	cells: &[(Tick, u8)],
	length: Tick,
	velocity: f32,
) -> Option<(Command, Vec<NoteId>)> {
	let c = midi_clip(project, clip)?;
	let notes: Vec<Note> = cells
		.iter()
		.filter(|(start, _)| start.0 >= 0)
		.map(|(start, pitch)| {
			Note::new(
				(*pitch).min(127),
				*start,
				Tick(length.0.max(MIN_NOTE_LENGTH.0)),
				velocity.clamp(0.0, 1.0),
			)
		})
		.collect();
	insert_notes(project, c, notes, "Paint notes")
}

/// Notes of `clip` under the segment `a`..`b` (grid pixels: x from the grid's left edge,
/// y from its top), sampled every 2 px: what a right-button sweep erases.
pub fn sweep_hits(clip: &Clip, view: &PianoView, a: (f32, f32), b: (f32, f32)) -> BTreeSet<NoteId> {
	let len = ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt();
	let steps = (len / 2.0).ceil().max(1.0) as usize;
	(0..=steps)
		.filter_map(|i| {
			let f = i as f32 / steps as f32;
			let (x, y) = (a.0 + (b.0 - a.0) * f, a.1 + (b.1 - a.1) * f);
			let t = view.timeline.x_to_tick(x) - clip.start;
			note_at(clip, t, view.y_to_pitch(y)).map(|n| n.id)
		})
		.collect()
}

/// Velocities on the line from `(t0, v0)` to `(t1, v1)` (absolute ticks) for the notes
/// starting in between (only `among` when it is not empty): the velocity lane's line.
pub fn velocity_line(
	clip: &Clip,
	among: &BTreeSet<NoteId>,
	(t0, v0): (Tick, f32),
	(t1, v1): (Tick, f32),
) -> HashMap<NoteId, f32> {
	let ((t0, v0), (t1, v1)) = if t0 <= t1 {
		((t0, v0), (t1, v1))
	} else {
		((t1, v1), (t0, v0))
	};
	clip.notes()
		.iter()
		.filter(|n| among.is_empty() || among.contains(&n.id))
		.filter_map(|n| {
			let at = clip.start + n.start;
			if at < t0 || at > t1 {
				return None;
			}
			let f = if t1 > t0 {
				(at - t0).0 as f32 / (t1 - t0).0 as f32
			} else {
				0.0
			};
			Some((n.id, (v0 + (v1 - v0) * f).clamp(0.0, 1.0)))
		})
		.collect()
}

/// The properties popover: sets one note's pitch, start (relative), length and
/// velocity (the clip grows when needed).
pub fn set_note(project: &Project, clip: ClipId, note: &Note) -> Option<Command> {
	let c = midi_clip(project, clip)?;
	let old = c.notes().iter().find(|n| n.id == note.id)?;
	let start = Tick(note.start.0.max(0));
	let length = Tick(note.length.0.max(MIN_NOTE_LENGTH.0));
	let velocity = note.velocity.clamp(0.0, 1.0);
	let pitch = note.pitch.min(127);
	let patch = NotePatch {
		id: note.id,
		pitch: (pitch != old.pitch).then_some(pitch),
		velocity: ((velocity - old.velocity).abs() > 1e-4).then_some(velocity),
		start: (start != old.start).then_some(start),
		length: (length != old.length).then_some(length),
	};
	if patch.pitch.is_none()
		&& patch.velocity.is_none()
		&& patch.start.is_none()
		&& patch.length.is_none()
	{
		return None;
	}
	let mut commands: Vec<Command> = grow(c, start + length, project.meter())
		.into_iter()
		.collect();
	commands.push(Command::UpdateNotes {
		clip,
		notes: vec![patch],
	});
	labelled("Edit note", commands)
}

/// Black keys of the piano.
pub fn is_black(pitch: u8) -> bool {
	matches!(pitch % 12, 1 | 3 | 6 | 8 | 10)
}

#[cfg(test)]
mod tests {
	use super::*;
	use cadenza_model::{Document, Track, TrackKind};

	fn b(beats: f64) -> Tick {
		Tick::from_beats(beats)
	}

	struct Fx {
		doc: Document,
		clip: ClipId,
		ids: Vec<NoteId>,
	}

	/// One 4-beat clip at bar 2 (beat 4) with C4, E4, G4 at beats 0, 1, 2 (relative).
	fn fixture() -> Fx {
		let mut p = Project::new("p");
		let mut t = Track::new("keys", TrackKind::Instrument);
		let clip = Clip::midi("c", b(4.0), b(4.0)).with_notes([
			Note::new(60, b(0.0), b(1.0), 0.8),
			Note::new(64, b(1.0), b(1.0), 0.6),
			Note::new(67, b(2.0), b(1.0), 0.4),
		]);
		let ids = clip.notes().iter().map(|n| n.id).collect();
		let id = clip.id;
		t.clips.push(clip);
		p.tracks.push(t);
		Fx {
			doc: Document::new(p),
			clip: id,
			ids,
		}
	}

	fn clip(fx: &Fx) -> Clip {
		fx.doc
			.project()
			.find_clip(fx.clip)
			.map(|(_, c)| c)
			.unwrap()
			.clone()
	}

	fn set(ids: &[NoteId]) -> BTreeSet<NoteId> {
		ids.iter().copied().collect()
	}

	#[test]
	fn alt_wheel_targets_the_selection_or_the_note_and_clamps_velocities() {
		let fx = fixture();
		let c = clip(&fx);
		let sel: BTreeSet<NoteId> = [fx.ids[0], fx.ids[1]].into();
		assert_eq!(wheel_targets(&sel, fx.ids[1]), sel);
		assert_eq!(wheel_targets(&sel, fx.ids[2]), BTreeSet::from([fx.ids[2]]));
		let up = nudge_velocities(&c, &sel, 0.3);
		assert_eq!(up.len(), 2);
		assert_eq!(up[&fx.ids[0]], 1.0, "clamped");
		assert!((up[&fx.ids[1]] - 0.9).abs() < 1e-6);
		let down = nudge_velocities(&c, &BTreeSet::from([fx.ids[2]]), -1.0);
		assert_eq!(down[&fx.ids[2]], 0.01, "still sounds");
	}

	#[test]
	fn track_targets_prefer_selection_then_playhead_then_recent() {
		let mut fx = fixture();
		let track = fx.doc.project().tracks[0].id;
		let later = Clip::midi("later", b(16.0), b(4.0));
		let later_id = later.id;
		fx.doc
			.execute(Command::AddClip { track, clip: later })
			.unwrap();
		let p = fx.doc.project();
		let none = BTreeSet::new();
		// Under the playhead (bar 2 = beats 4..8).
		assert_eq!(clip_for_track(p, track, &none, b(5.0), None), Some(fx.clip));
		// Selection wins over the playhead.
		let sel: BTreeSet<ClipId> = [later_id].into();
		assert_eq!(clip_for_track(p, track, &sel, b(5.0), None), Some(later_id));
		// Nothing under the playhead: the last opened one, else none.
		assert_eq!(
			clip_for_track(p, track, &none, b(12.0), Some(later_id)),
			Some(later_id)
		);
		assert_eq!(clip_for_track(p, track, &none, b(12.0), None), None);
		// A recent clip of another track does not count.
		assert_eq!(
			clip_for_track(p, track, &none, b(12.0), Some(ClipId::new())),
			None
		);
	}

	#[test]
	fn new_clips_take_the_first_free_bar_and_the_track_name() {
		let mut fx = fixture();
		let track = fx.doc.project().tracks[0].id;
		// Playhead in bar 1 (free): the clip starts at 0.
		let (cmd, id) = new_clip(fx.doc.project(), track, b(2.5)).unwrap();
		fx.doc.execute(cmd).unwrap();
		let (_, c) = fx.doc.project().find_clip(id).unwrap();
		assert_eq!(
			(c.start, c.length, c.name.as_str()),
			(b(0.0), b(4.0), "keys")
		);
		// Playhead inside bars 1-2 (both taken): the next free bar.
		let (cmd, id) = new_clip(fx.doc.project(), track, b(1.0)).unwrap();
		fx.doc.execute(cmd).unwrap();
		assert_eq!(fx.doc.project().find_clip(id).unwrap().1.start, b(8.0));
		fx.doc.undo().unwrap();
		assert!(fx.doc.project().find_clip(id).is_none(), "undoable");
		let bus = Track::new("bus", TrackKind::Bus);
		let bus_id = bus.id;
		fx.doc
			.execute(Command::AddTrack {
				track: bus,
				index: None,
			})
			.unwrap();
		assert!(new_clip(fx.doc.project(), bus_id, b(0.0)).is_none());
	}

	#[test]
	fn drawing_adds_one_undoable_note_and_grows_the_clip() {
		let mut fx = fixture();
		let (cmd, id) = add_note(fx.doc.project(), fx.clip, b(1.5), 72, b(0.5), 0.9).unwrap();
		assert!(
			matches!(&cmd, Command::Batch { commands, .. } if commands.len() == 1),
			"fits: no resize"
		);
		fx.doc.execute(cmd).unwrap();
		assert!(
			clip(&fx)
				.notes()
				.iter()
				.any(|n| n.id == id && n.pitch == 72)
		);

		// Past the end: one batch that grows the clip to the next bar line.
		let (cmd, _) = add_note(fx.doc.project(), fx.clip, b(5.0), 60, b(1.0), 0.8).unwrap();
		assert!(matches!(&cmd, Command::Batch { commands, .. } if commands.len() == 2));
		fx.doc.execute(cmd).unwrap();
		assert_eq!(clip(&fx).length, b(8.0));
		fx.doc.undo().unwrap();
		assert_eq!(clip(&fx).length, b(4.0));
		assert_eq!(clip(&fx).notes().len(), 4);
		// Before the clip start: clamped to 0.
		let (cmd, id) = add_note(fx.doc.project(), fx.clip, b(-3.0), 50, b(0.25), 0.8).unwrap();
		fx.doc.execute(cmd).unwrap();
		let n = clip(&fx)
			.notes()
			.iter()
			.find(|n| n.id == id)
			.unwrap()
			.clone();
		assert_eq!(n.start, Tick::ZERO);
	}

	#[test]
	fn moving_clamps_and_grows() {
		let mut fx = fixture();
		let all = set(&fx.ids);
		let c = clip(&fx);
		assert_eq!(clamp_move(&c, &all, b(-2.0), 100), (Tick::ZERO, 60));
		assert!(move_notes(fx.doc.project(), fx.clip, &all, b(-1.0), 0).is_none());
		let cmd = move_notes(fx.doc.project(), fx.clip, &set(&fx.ids[2..]), b(2.0), -2).unwrap();
		fx.doc.execute(cmd).unwrap();
		let c = clip(&fx);
		let g = c.notes().iter().find(|n| n.id == fx.ids[2]).unwrap();
		assert_eq!((g.start, g.pitch), (b(4.0), 65));
		assert_eq!(c.length, b(8.0), "grown to bar 3's end");
		fx.doc.undo().unwrap();
		assert_eq!(clip(&fx).length, b(4.0));
		// Transpose only: labelled as such.
		let cmd = move_notes(fx.doc.project(), fx.clip, &all, Tick::ZERO, 12).unwrap();
		assert_eq!(cmd.label(), "Transpose notes");
	}

	#[test]
	fn resize_has_a_minimum() {
		let mut fx = fixture();
		let first = set(&fx.ids[..1]);
		let cmd = resize_notes(fx.doc.project(), fx.clip, &first, b(-5.0)).unwrap();
		fx.doc.execute(cmd).unwrap();
		assert_eq!(clip(&fx).notes()[0].length, MIN_NOTE_LENGTH);
		assert!(resize_notes(fx.doc.project(), fx.clip, &first, Tick(-1)).is_none());
	}

	#[test]
	fn quantize_uses_the_absolute_grid() {
		let mut fx = fixture();
		let cmd = move_notes(fx.doc.project(), fx.clip, &set(&fx.ids), Tick(70), 0).unwrap();
		fx.doc.execute(cmd).unwrap();
		let cmd = quantize_notes(fx.doc.project(), fx.clip, &BTreeSet::new(), b(0.5), 1.0).unwrap();
		fx.doc.execute(cmd).unwrap();
		let starts: Vec<Tick> = clip(&fx).notes().iter().map(|n| n.start).collect();
		assert_eq!(starts, vec![b(0.0), b(1.0), b(2.0)]);
		assert!(
			quantize_notes(fx.doc.project(), fx.clip, &BTreeSet::new(), b(0.5), 1.0).is_none(),
			"already on the grid"
		);
		// Half strength moves half way.
		let cmd = move_notes(fx.doc.project(), fx.clip, &set(&fx.ids[..1]), Tick(100), 0).unwrap();
		fx.doc.execute(cmd).unwrap();
		let cmd =
			quantize_notes(fx.doc.project(), fx.clip, &set(&fx.ids[..1]), b(1.0), 0.5).unwrap();
		fx.doc.execute(cmd).unwrap();
		assert_eq!(clip(&fx).notes()[0].start, Tick(50));
	}

	#[test]
	fn copy_paste_and_velocities() {
		let mut fx = fixture();
		let c = clip(&fx);
		let board = copy_notes(&c, &set(&fx.ids[1..]));
		assert_eq!(board[0].start, Tick::ZERO);
		assert_eq!(board[1].start, b(1.0));
		let (cmd, ids) = paste_notes(fx.doc.project(), fx.clip, &board, b(3.0)).unwrap();
		fx.doc.execute(cmd).unwrap();
		assert_eq!(ids.len(), 2);
		assert_eq!(
			clip(&fx).length,
			b(8.0),
			"paste past the end grows the clip"
		);

		let mut v = HashMap::new();
		v.insert(fx.ids[0], 0.25);
		v.insert(fx.ids[1], 0.6); // unchanged: skipped
		match set_velocities(fx.doc.project(), fx.clip, &v).unwrap() {
			Command::UpdateNotes { notes, .. } => {
				assert_eq!(notes.len(), 1);
				assert_eq!(notes[0].velocity, Some(0.25));
			}
			other => panic!("{other:?}"),
		}
	}

	#[test]
	fn hit_testing_and_rubber_band() {
		let fx = fixture();
		let c = clip(&fx);
		assert_eq!(note_at(&c, b(1.5), 64).map(|n| n.id), Some(fx.ids[1]));
		assert!(note_at(&c, b(1.5), 65).is_none());
		let mut hits = notes_in_rect(&c, b(0.5), b(2.5), 62, 70);
		hits.sort();
		let mut want = vec![fx.ids[1], fx.ids[2]];
		want.sort();
		assert_eq!(hits, want);
		assert!(is_black(61) && !is_black(60));
	}

	#[test]
	fn view_geometry() {
		let mut v = PianoView {
			scroll_y: 0.0,
			row_height: 10.0,
			..PianoView::default()
		};
		assert_eq!(v.pitch_to_y(127), 0.0);
		assert_eq!(v.y_to_pitch(5.0), 127);
		assert_eq!(v.y_to_pitch(15.0), 126);
		assert_eq!(v.y_to_pitch(1e6), 0);
		v.scroll_y = 100.0;
		let anchor = v.y_to_pitch(200.0);
		v.zoom_rows(2.0, 200.0, 400.0);
		assert_eq!(v.row_height, 20.0);
		assert_eq!(v.y_to_pitch(200.0), anchor, "pitch under the mouse stays");
		assert_eq!(snap_floor(b(1.7), Some(b(0.5))), b(1.5));
		assert_eq!(snap_abs(b(1.8), Some(b(0.5))), b(2.0));
		assert_eq!(snap_abs(Tick(-5), None), Tick::ZERO);
	}

	fn note(fx: &Fx, id: NoteId) -> Note {
		clip(fx)
			.notes()
			.iter()
			.find(|n| n.id == id)
			.unwrap()
			.clone()
	}

	#[test]
	fn snap_modes_and_main() {
		let ts = TimeSignature::default();
		assert_eq!(PianoSnap::default(), PianoSnap::Step);
		assert_eq!(PianoSnap::Step.step(ts, None), Some(b(0.25)));
		assert_eq!(PianoSnap::Bar.step(ts, None), Some(b(4.0)));
		assert_eq!(PianoSnap::ThirdBeat.step(ts, None), Some(Tick(PPQ / 3)));
		assert_eq!(PianoSnap::Off.step(ts, Some(b(1.0))), None);
		assert_eq!(PianoSnap::Main.step(ts, Some(b(0.5))), Some(b(0.5)));
		assert_eq!(PianoSnap::Main.step(ts, None), None, "global snap off");
	}

	#[test]
	fn ctrl_b_clones_after_the_selection_and_keeps_appending() {
		let mut fx = fixture();
		let step = Some(b(0.25));
		// One 1-beat note: the copy lands right after it.
		let (cmd, ids) = clone_notes(fx.doc.project(), fx.clip, &set(&fx.ids[..1]), step).unwrap();
		assert_eq!(cmd.label(), "Clone notes");
		fx.doc.execute(cmd).unwrap();
		assert_eq!(note(&fx, ids[0]).start, b(1.0));
		// Repeating on the copy keeps appending.
		let (cmd, ids) = clone_notes(fx.doc.project(), fx.clip, &set(&ids), step).unwrap();
		fx.doc.execute(cmd).unwrap();
		assert_eq!(note(&fx, ids[0]).start, b(2.0));
		// A ragged span widens to the snap grid.
		let c = clip(&fx);
		let mut short = c.clone();
		if let cadenza_model::ClipContent::Midi { notes, .. } = &mut short.content {
			notes[0].start = b(0.1);
			notes[0].length = b(0.5);
		}
		assert_eq!(
			clone_offset(&short, &set(&fx.ids[..1]), step, TimeSignature::default()),
			Some(b(0.75)),
			"0.1..0.6 widens to 0..0.75"
		);
		assert_eq!(
			clone_offset(&short, &set(&fx.ids[..1]), None, TimeSignature::default()),
			Some(b(0.5)),
			"no snap: the exact span"
		);
		// The 3-note phrase (beats 0..3) is shorter than a bar: shifted by 3 beats; a
		// longer span rounds up to whole bars.
		assert_eq!(
			clone_offset(&c, &set(&fx.ids), step, TimeSignature::default()),
			Some(b(3.0))
		);
		let cmd = move_notes(fx.doc.project(), fx.clip, &set(&fx.ids[2..]), b(2.5), 0).unwrap();
		fx.doc.execute(cmd).unwrap();
		assert_eq!(
			clone_offset(&clip(&fx), &set(&fx.ids), step, TimeSignature::default()),
			Some(b(8.0)),
			"0..5.5 beats -> two bars"
		);
		assert!(clone_notes(fx.doc.project(), fx.clip, &BTreeSet::new(), step).is_none());
	}

	#[test]
	fn copy_cut_and_paste_position_rules() {
		let mut fx = fixture();
		let c = clip(&fx);
		let board = clipboard(&c, &set(&fx.ids[1..]), b(0.0));
		assert_eq!(board.origin, b(5.0), "absolute: clip at beat 4 + 1");
		assert_eq!(board.notes.len(), 2);
		// Playhead untouched: paste at the original position; moved: at the playhead;
		// while playing: the original position.
		assert_eq!(board.paste_at(b(0.0), false), b(5.0));
		assert_eq!(board.paste_at(b(6.0), false), b(6.0));
		assert_eq!(board.paste_at(b(6.0), true), b(5.0));
		let cmd = cut_notes(fx.doc.project(), fx.clip, &set(&fx.ids[1..])).unwrap();
		assert_eq!(cmd.label(), "Cut notes");
		fx.doc.execute(cmd).unwrap();
		assert_eq!(clip(&fx).notes().len(), 1);
		let at = board.paste_at(b(0.0), false) - c.start;
		let (cmd, ids) = paste_notes(fx.doc.project(), fx.clip, &board.notes, at).unwrap();
		fx.doc.execute(cmd).unwrap();
		let starts: Vec<(u8, Tick)> = ids
			.iter()
			.map(|id| {
				let n = note(&fx, *id);
				(n.pitch, n.start)
			})
			.collect();
		assert_eq!(
			starts,
			vec![(64, b(1.0)), (67, b(2.0))],
			"back where they were"
		);
	}

	#[test]
	fn arrows_move_the_selection_or_every_note() {
		let mut fx = fixture();
		let c = clip(&fx);
		assert_eq!(
			targets(&c, &BTreeSet::new()).len(),
			3,
			"nothing selected: all"
		);
		assert_eq!(targets(&c, &set(&fx.ids[..1])).len(), 1);
		let all = targets(&c, &BTreeSet::new());
		let cmd = move_notes(fx.doc.project(), fx.clip, &all, Tick::ZERO, 12).unwrap();
		fx.doc.execute(cmd).unwrap();
		let pitches: Vec<u8> = clip(&fx).notes().iter().map(|n| n.pitch).collect();
		assert_eq!(pitches, vec![72, 76, 79]);
		let cmd = move_notes(fx.doc.project(), fx.clip, &all, b(0.25), 0).unwrap();
		fx.doc.execute(cmd).unwrap();
		assert_eq!(clip(&fx).notes()[0].start, b(0.25));
		// Left past the clip start is clamped (the whole group keeps its shape).
		let cmd = move_notes(fx.doc.project(), fx.clip, &all, b(-4.0), 0).unwrap();
		fx.doc.execute(cmd).unwrap();
		let starts: Vec<Tick> = clip(&fx).notes().iter().map(|n| n.start).collect();
		assert_eq!(starts, vec![b(0.0), b(1.0), b(2.0)]);
	}

	#[test]
	fn paint_cells_and_one_command() {
		let first = b(1.0);
		let spacing = b(0.5);
		assert_eq!(paint_cell(first, spacing, b(1.0)), 0);
		assert_eq!(paint_cell(first, spacing, b(1.49)), 0);
		assert_eq!(paint_cell(first, spacing, b(2.1)), 2);
		assert_eq!(paint_cell(first, spacing, b(0.9)), -1);
		let mut fx = fixture();
		let cells = [(b(0.0), 70), (b(0.5), 71), (b(1.0), 72), (b(-0.5), 60)];
		let (cmd, ids) = paint_notes(fx.doc.project(), fx.clip, &cells, b(0.5), 0.7).unwrap();
		assert_eq!(cmd.label(), "Paint notes");
		assert_eq!(ids.len(), 3, "the cell before the clip is dropped");
		fx.doc.execute(cmd).unwrap();
		assert_eq!(clip(&fx).notes().len(), 6);
		fx.doc.undo().unwrap();
		assert_eq!(clip(&fx).notes().len(), 3, "one undo step");
	}

	#[test]
	fn right_button_sweep_deletes_in_one_undo_step() {
		let mut fx = fixture();
		let mut v = PianoView {
			row_height: 10.0,
			scroll_y: 0.0,
			..PianoView::default()
		};
		v.timeline.px_per_beat = 40.0;
		v.timeline.scroll_beats = 0.0;
		let c = clip(&fx);
		// Notes at beats 4, 5, 6 (absolute) on C4/E4/G4 rows.
		let row = |p: u8| v.pitch_to_y(p) + 5.0;
		let x = |beat: f64| v.timeline.beat_to_x(beat);
		// A diagonal sweep from C4 at beat 4.5 to G4 at beat 6.5 crosses all three.
		let hits = sweep_hits(&c, &v, (x(4.5), row(60)), (x(6.5), row(67)));
		assert_eq!(hits, set(&fx.ids));
		// A sweep along the C4 row only hits C4.
		assert_eq!(
			sweep_hits(&c, &v, (x(3.0), row(60)), (x(7.0), row(60))),
			set(&fx.ids[..1])
		);
		// A sweep over empty rows hits nothing.
		assert!(sweep_hits(&c, &v, (x(4.0), row(80)), (x(7.0), row(80))).is_empty());
		let cmd = delete_notes(fx.doc.project(), fx.clip, &hits).unwrap();
		fx.doc.execute(cmd).unwrap();
		assert!(clip(&fx).notes().is_empty());
		fx.doc.undo().unwrap();
		assert_eq!(
			clip(&fx).notes().len(),
			3,
			"one undo restores the whole sweep"
		);
	}

	#[test]
	fn shift_drag_duplicates_and_left_edge_resizes() {
		let mut fx = fixture();
		let (cmd, ids) =
			copy_moved(fx.doc.project(), fx.clip, &set(&fx.ids[..1]), b(1.0), 2).unwrap();
		assert_eq!(cmd.label(), "Duplicate notes");
		fx.doc.execute(cmd).unwrap();
		let copy = note(&fx, ids[0]);
		assert_eq!((copy.start, copy.pitch), (b(1.0), 62));
		assert_eq!(note(&fx, fx.ids[0]).start, b(0.0), "the original stays");
		// Left edge: the start moves, the end stays; never past the end or the clip.
		let cmd =
			resize_start_notes(fx.doc.project(), fx.clip, &set(&fx.ids[1..2]), b(0.5)).unwrap();
		fx.doc.execute(cmd).unwrap();
		let n = note(&fx, fx.ids[1]);
		assert_eq!((n.start, n.end()), (b(1.5), b(2.0)));
		let cmd =
			resize_start_notes(fx.doc.project(), fx.clip, &set(&fx.ids[1..2]), b(5.0)).unwrap();
		fx.doc.execute(cmd).unwrap();
		assert_eq!(note(&fx, fx.ids[1]).length, MIN_NOTE_LENGTH);
		let cmd = resize_start_notes(fx.doc.project(), fx.clip, &set(&fx.ids[..1]), b(-3.0));
		assert!(cmd.is_none(), "already at the clip start");
	}

	#[test]
	fn glue_legato_and_slice() {
		let mut fx = fixture();
		// Legato: each note lasts until the next start.
		let cmd = resize_notes(fx.doc.project(), fx.clip, &set(&fx.ids), b(-0.5)).unwrap();
		fx.doc.execute(cmd).unwrap();
		let cmd = legato_notes(fx.doc.project(), fx.clip, &set(&fx.ids)).unwrap();
		assert_eq!(cmd.label(), "Legato");
		fx.doc.execute(cmd).unwrap();
		let lengths: Vec<Tick> = clip(&fx).notes().iter().map(|n| n.length).collect();
		assert_eq!(
			lengths,
			vec![b(1.0), b(1.0), b(0.5)],
			"the last keeps its length"
		);
		assert!(legato_notes(fx.doc.project(), fx.clip, &set(&fx.ids)).is_none());

		// Slice C4 (0..1) at 0.5: two halves.
		let cmd = slice_notes(fx.doc.project(), fx.clip, b(0.5), 60, 60).unwrap();
		fx.doc.execute(cmd).unwrap();
		let mut c4: Vec<(Tick, Tick)> = clip(&fx)
			.notes()
			.iter()
			.filter(|n| n.pitch == 60)
			.map(|n| (n.start, n.length))
			.collect();
		c4.sort();
		assert_eq!(c4, vec![(b(0.0), b(0.5)), (b(0.5), b(0.5))]);
		assert!(slice_notes(fx.doc.project(), fx.clip, b(0.5), 70, 80).is_none());

		// Glue joins them back into one note (one undo step).
		let ids: BTreeSet<NoteId> = clip(&fx)
			.notes()
			.iter()
			.filter(|n| n.pitch == 60)
			.map(|n| n.id)
			.collect();
		let cmd = glue_notes(fx.doc.project(), fx.clip, &ids).unwrap();
		fx.doc.execute(cmd).unwrap();
		let c = clip(&fx);
		let c4: Vec<&Note> = c.notes().iter().filter(|n| n.pitch == 60).collect();
		assert_eq!(c4.len(), 1);
		assert_eq!((c4[0].start, c4[0].length), (b(0.0), b(1.0)));
		assert!(glue_notes(fx.doc.project(), fx.clip, &set(&fx.ids[..1])).is_none());
	}

	#[test]
	fn velocity_lines_and_note_properties() {
		let mut fx = fixture();
		let c = clip(&fx);
		// Line from beat 4 (0.0) to beat 6 (1.0): 0, 0.5, 1.
		let v = velocity_line(&c, &BTreeSet::new(), (b(6.0), 1.0), (b(4.0), 0.0));
		assert_eq!(v[&fx.ids[0]], 0.0);
		assert!((v[&fx.ids[1]] - 0.5).abs() < 1e-6);
		assert_eq!(v[&fx.ids[2]], 1.0);
		let only = velocity_line(&c, &set(&fx.ids[..1]), (b(4.0), 0.3), (b(6.0), 0.3));
		assert_eq!(only.len(), 1, "restricted to the selection");

		let mut n = note(&fx, fx.ids[0]);
		n.pitch = 61;
		n.length = b(6.0);
		n.velocity = 0.5;
		let cmd = set_note(fx.doc.project(), fx.clip, &n).unwrap();
		fx.doc.execute(cmd).unwrap();
		let got = note(&fx, fx.ids[0]);
		assert_eq!((got.pitch, got.length, got.velocity), (61, b(6.0), 0.5));
		assert_eq!(clip(&fx).length, b(8.0), "grown to fit");
		assert!(
			set_note(fx.doc.project(), fx.clip, &got).is_none(),
			"unchanged"
		);
	}
}
