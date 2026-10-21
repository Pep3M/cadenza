//! Arrange tools (Select / Draw / Split): what a press on a lane or a clip does. Pure
//! gesture -> command builders; the view only hit-tests.

use crate::i18n::tr;
use cadenza_core::{ClipId, Tick, TrackId};
use cadenza_model::{Command, Project};
use serde::{Deserialize, Serialize};

use super::edits;

/// The arrange view's pointer tool (view state, persisted in the prefs).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArrangeTool {
	/// Today's behaviour: click selects, drag moves / resizes / trims, drag on an empty
	/// lane draws a rubber band.
	#[default]
	Select,
	/// Click or drag on an empty instrument lane creates a MIDI clip on the grid; clips
	/// still move.
	Draw,
	/// Click on a clip splits it at the snapped pointer; a drag is a knife cutting every
	/// clip the line crosses (several tracks).
	Split,
	/// Drag along a lane repeats the pressed / selected / last clip at every grid cell of
	/// its length (FL Paint).
	Paint,
	/// Click or sweep over clips to mute / unmute them.
	Mute,
}

impl ArrangeTool {
	pub const ALL: [ArrangeTool; 5] = [
		ArrangeTool::Select,
		ArrangeTool::Draw,
		ArrangeTool::Paint,
		ArrangeTool::Split,
		ArrangeTool::Mute,
	];

	pub fn name(self) -> &'static str {
		match self {
			ArrangeTool::Select => tr("Select"),
			ArrangeTool::Draw => tr("Draw"),
			ArrangeTool::Split => tr("Split"),
			ArrangeTool::Paint => tr("Paint"),
			ArrangeTool::Mute => tr("Mute"),
		}
	}

	pub fn glyph(self) -> &'static str {
		match self {
			ArrangeTool::Select => "↖",
			ArrangeTool::Draw => "✏",
			ArrangeTool::Split => "✂",
			ArrangeTool::Paint => "▤",
			ArrangeTool::Mute => "⊘",
		}
	}

	/// Its key (arrange focused). S stays "split at the playhead", so Split is C, like the
	/// piano roll's Slice; Paint B and Mute T as in FL Studio.
	pub fn key(self) -> &'static str {
		match self {
			ArrangeTool::Select => "V",
			ArrangeTool::Draw => "D",
			ArrangeTool::Split => "C",
			ArrangeTool::Paint => "B",
			ArrangeTool::Mute => "T",
		}
	}

	pub fn hint(self) -> &'static str {
		match self {
			ArrangeTool::Select => tr("Select, move and trim clips; drag empty space to select"),
			ArrangeTool::Draw => tr("Click or drag on an instrument lane to draw a MIDI clip"),
			ArrangeTool::Split => tr(
				"Click a clip to split it; drag a line across tracks to cut them all (Alt: no snap)",
			),
			ArrangeTool::Paint => {
				tr("Drag along a lane to repeat the clip at every grid cell; right-drag deletes")
			}
			ArrangeTool::Mute => tr("Click or sweep over clips to mute / unmute them"),
		}
	}
}

/// The span a Draw gesture creates from the press to the pointer. With a grid `step` the
/// start snaps down and the end up to it (at least one step); a click (`now == press`)
/// gives `click_length` (a bar) from the snapped press. Never before 0.
pub fn draw_span(press: Tick, now: Tick, step: Option<Tick>, click_length: Tick) -> (Tick, Tick) {
	let step = step.filter(|s| s.0 > 0);
	let floor = |t: Tick| step.map_or(t, |s| Tick(t.0.div_euclid(s.0) * s.0));
	let ceil = |t: Tick| step.map_or(t, |s| Tick((t.0 + s.0 - 1).div_euclid(s.0) * s.0));
	if now == press {
		let start = Tick(floor(press).0.max(0));
		return (start, start + click_length);
	}
	let (a, b) = if now < press {
		(now, press)
	} else {
		(press, now)
	};
	let start = Tick(floor(a).0.max(0));
	let end = ceil(b).max(start + step.unwrap_or(Tick(1)));
	(start, end)
}

/// The Draw tool's clip: a new empty MIDI clip on an instrument track (one undo step).
pub fn draw_clip(
	project: &Project,
	track: TrackId,
	(start, end): (Tick, Tick),
) -> Option<(Command, ClipId)> {
	(end > start)
		.then(|| edits::new_midi_clip(project, track, start, end - start))
		.flatten()
}

/// Clips a Split click on `clip` cuts: the whole selection when the clip is part of it,
/// else that clip alone.
pub fn split_targets(clip: ClipId, selection: &[ClipId]) -> Vec<ClipId> {
	if selection.contains(&clip) && selection.len() > 1 {
		selection.to_vec()
	} else {
		vec![clip]
	}
}

/// One command splitting every clip in `clips` that spans `at` ("Split clip", or a
/// "Split clips" batch); `None` when none can be split there.
pub fn split_at(project: &Project, clips: &[ClipId], at: Tick) -> Option<Command> {
	let commands: Vec<Command> = clips
		.iter()
		.filter_map(|&c| edits::split_clip(project, c, at))
		.map(|(cmd, _)| cmd)
		.collect();
	edits::batch("Split clips", commands)
}

#[cfg(test)]
mod tests {
	use super::*;
	use cadenza_core::PPQ;
	use cadenza_model::{Clip, Document, Track, TrackKind};

	const BAR: Tick = Tick(PPQ * 4);
	const STEP: Tick = Tick(PPQ / 4);

	#[test]
	fn draw_span_snaps_out_to_the_grid_and_a_click_is_one_bar() {
		// A click (no movement) = one bar from the step under the pointer.
		let at = Tick(PPQ * 5 + 30);
		assert_eq!(
			draw_span(at, at, Some(STEP), BAR),
			(Tick(PPQ * 5), Tick(PPQ * 5) + BAR)
		);
		// A drag covers every cell it touched, either direction.
		let (a, b) = (Tick(PPQ + 10), Tick(PPQ * 3 + 10));
		let expected = (Tick(PPQ), Tick(PPQ * 3) + STEP);
		assert_eq!(draw_span(a, b, Some(STEP), BAR), expected);
		assert_eq!(draw_span(b, a, Some(STEP), BAR), expected);
		// No snap: exact; never before 0.
		assert_eq!(draw_span(a, b, None, BAR), (a, b));
		assert_eq!(
			draw_span(Tick(-50), Tick(-50), Some(STEP), BAR).0,
			Tick::ZERO
		);
	}

	#[test]
	fn draw_clip_only_on_instrument_tracks() {
		let mut doc = Document::new(cadenza_model::Project::new("t"));
		let keys = Track::new("Keys", TrackKind::Instrument);
		let vox = Track::new("Vox", TrackKind::Audio);
		let (k, v) = (keys.id, vox.id);
		for track in [keys, vox] {
			doc.execute(Command::AddTrack { track, index: None })
				.unwrap();
		}
		assert!(draw_clip(doc.project(), v, (Tick::ZERO, BAR)).is_none());
		assert!(draw_clip(doc.project(), k, (BAR, BAR)).is_none());
		let (cmd, id) = draw_clip(doc.project(), k, (BAR, Tick(PPQ * 12))).unwrap();
		let depth = doc.history_labels().len();
		doc.execute(cmd).unwrap();
		let (_, c) = doc.project().find_clip(id).unwrap();
		assert_eq!((c.start, c.length), (BAR, Tick(PPQ * 8)));
		assert_eq!(doc.history_labels().len(), depth + 1);
	}

	#[test]
	fn split_cuts_the_selection_or_the_clicked_clip_in_one_step() {
		let mut doc = Document::new(cadenza_model::Project::new("t"));
		let mut t = Track::new("Keys", TrackKind::Instrument);
		let a = Clip::midi("A", Tick::ZERO, Tick(PPQ * 8));
		let b = Clip::midi("B", Tick::ZERO, Tick(PPQ * 16));
		let (ia, ib) = (a.id, b.id);
		t.clips = vec![a, b];
		doc.execute(Command::AddTrack {
			track: t,
			index: None,
		})
		.unwrap();
		assert_eq!(split_targets(ia, &[ib]), vec![ia]);
		assert_eq!(split_targets(ia, &[ia, ib]), vec![ia, ib]);
		assert!(
			split_at(doc.project(), &[ia], Tick(PPQ * 8)).is_none(),
			"at the end"
		);
		let cmd = split_at(doc.project(), &[ia, ib], BAR).unwrap();
		assert!(matches!(&cmd, Command::Batch { label, .. } if label == "Split clips"));
		doc.execute(cmd).unwrap();
		assert_eq!(doc.project().tracks[0].clips.len(), 4);
		assert_eq!(doc.history_labels().len(), 2);
	}
}
