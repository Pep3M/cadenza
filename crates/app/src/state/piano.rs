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

use std::collections::{BTreeSet, HashMap};

use cadenza_core::{ClipId, NoteId, PPQ, Tick, TimeSignature, TrackId};
use cadenza_model::{Clip, ClipPatch, Command, Note, NotePatch, Project, TrackKind};
use cadenza_theory::Scale;
use serde::{Deserialize, Serialize};

use super::timeline::{Timeline, snap_to};

pub const DEFAULT_ROW_HEIGHT: f32 = 14.0;
pub const MIN_ROW_HEIGHT: f32 = 6.0;
pub const MAX_ROW_HEIGHT: f32 = 40.0;
/// Shortest note a resize or a drawn note can produce.
pub const MIN_NOTE_LENGTH: Tick = Tick(PPQ / 32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tool {
	/// Click adds a note, drag sets its length; existing notes still move and resize.
	#[default]
	Draw,
	/// Click selects, drag on empty space draws a rubber band.
	Select,
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
	/// Copied notes, starts relative to the earliest one.
	pub clipboard: Vec<Note>,
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
			clipboard: Vec::new(),
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

	/// Scrolls/zooms to show `clip` (its notes vertically, its span horizontally).
	pub fn frame(&mut self, clip: &Clip, width: f32, view_h: f32) {
		self.framed = Some(clip.id);
		let notes = clip.notes();
		let (lo, hi) = notes.iter().fold((u8::MAX, 0u8), |(lo, hi), n| {
			(lo.min(n.pitch), hi.max(n.pitch))
		});
		let center = if notes.is_empty() {
			66.0
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
	let bar = project.time_signature.ticks_per_bar();
	let mut start = Tick(at.0.max(0) / bar.0 * bar.0);
	while let Some(end) = t
		.clips
		.iter()
		.filter(|c| c.start < start + bar && start < c.end())
		.map(|c| c.end())
		.max()
	{
		start = Tick((end.0 + bar.0 - 1) / bar.0 * bar.0);
	}
	let clip = Clip::midi(t.name.clone(), start, bar);
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
pub fn grown_length(clip: &Clip, end: Tick, ts: TimeSignature) -> Option<Tick> {
	if end <= clip.length {
		return None;
	}
	let bar = ts.ticks_per_bar().0.max(1);
	let abs_end = clip.start.0 + end.0;
	let rounded = (abs_end + bar - 1).div_euclid(bar) * bar;
	Some(Tick(rounded - clip.start.0))
}

fn grow(clip: &Clip, end: Tick, ts: TimeSignature) -> Option<Command> {
	grown_length(clip, end, ts).map(|length| Command::UpdateClip {
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
	let mut commands: Vec<Command> = grow(c, note.end(), project.time_signature)
		.into_iter()
		.collect();
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
	let mut commands: Vec<Command> = grow(c, end, project.time_signature).into_iter().collect();
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
	let mut commands: Vec<Command> = grow(c, end, project.time_signature).into_iter().collect();
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

/// Copies the notes right after the selection (shifted by its length: earliest start to
/// latest end). Returns the command and the ids of the copies.
pub fn duplicate_notes(
	project: &Project,
	clip: ClipId,
	ids: &BTreeSet<NoteId>,
) -> Option<(Command, Vec<NoteId>)> {
	let c = midi_clip(project, clip)?;
	let notes = selected(c, ids);
	let start = notes.iter().map(|n| n.start).min()?;
	let end = notes.iter().map(|n| n.end()).max()?;
	let copies: Vec<Note> = notes
		.iter()
		.map(|n| Note {
			id: NoteId::new(),
			start: n.start + (end - start),
			..(*n).clone()
		})
		.collect();
	insert_notes(project, c, copies, "Duplicate notes")
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
	let mut commands: Vec<Command> = grow(c, end, project.time_signature).into_iter().collect();
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
	let mut commands: Vec<Command> = grow(c, end, project.time_signature).into_iter().collect();
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
	fn duplicate_shifts_by_the_selection_length() {
		let mut fx = fixture();
		let two = set(&fx.ids[..2]);
		let (cmd, ids) = duplicate_notes(fx.doc.project(), fx.clip, &two).unwrap();
		fx.doc.execute(cmd).unwrap();
		let c = clip(&fx);
		let starts: Vec<Tick> = ids
			.iter()
			.map(|id| c.notes().iter().find(|n| n.id == *id).unwrap().start)
			.collect();
		assert_eq!(starts, vec![b(2.0), b(3.0)], "selection spans 2 beats");
		assert_eq!(c.notes().len(), 5);
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
}
