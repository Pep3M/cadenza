//! Take groups and comping: pure builders producing ordinary [`Command`]s (so every
//! comping step is validated and undoable like any other edit).
//!
//! A take group is the set of clips sharing a [`Take::group`]: alternative recordings of
//! the same stretch of a track (loop recording passes, or a recording made over existing
//! clips). Only the active take of a group plays. Comping picks the active take of a
//! group ([`select_take`]); to comp per part, the group is split at a time into two
//! groups that each keep every take ([`split_group`]); [`comp_range`] does both for a
//! range swiped over a take lane.

use cadenza_core::{ClipId, NoteId, TakeGroupId, Tick, TrackId};

use crate::command::{ClipPatch, Command, NotePatch};
use crate::document::Document;
use crate::project::{Clip, ClipContent, Note, Project, Take};

/// The clips of `group`, with their track, sorted by (lane, start).
pub fn group_clips(project: &Project, group: TakeGroupId) -> Vec<(TrackId, &Clip)> {
	let mut out: Vec<(TrackId, &Clip)> = project
		.tracks
		.iter()
		.flat_map(|t| t.clips.iter().map(move |c| (t.id, c)))
		.filter(|(_, c)| c.take_group() == Some(group))
		.collect();
	out.sort_by_key(|(_, c)| (c.take.map_or(0, |t| t.lane), c.start));
	out
}

/// Number of take lanes of `group` (highest lane + 1).
pub fn lane_count(project: &Project, group: TakeGroupId) -> u32 {
	group_clips(project, group)
		.iter()
		.filter_map(|(_, c)| c.take.map(|t| t.lane + 1))
		.max()
		.unwrap_or(0)
}

/// Makes `clip` the take that plays in its group (the group's other clips stop
/// playing). `None` when the clip is not a take or already the only active one.
pub fn select_take(project: &Project, clip: ClipId) -> Option<Command> {
	let (_, picked) = project.find_clip(clip)?;
	let group = picked.take_group()?;
	let commands: Vec<Command> = group_clips(project, group)
		.into_iter()
		.filter_map(|(_, c)| {
			let take = c.take?;
			let active = c.id == clip;
			(take.active != active).then(|| Command::UpdateClip {
				clip: c.id,
				patch: ClipPatch {
					take: Some(Some(Take { active, ..take })),
					..ClipPatch::default()
				},
			})
		})
		.collect();
	(!commands.is_empty()).then(|| Command::Batch {
		label: "Select take".into(),
		commands,
	})
}

/// Splits `clip` at `at` (strictly inside it) into two clips: the commands shorten it
/// and add the right part as `right_id` with `right_take` as its take membership. MIDI
/// notes starting after the split move to the new clip, notes crossing it are
/// shortened; audio continues with the right file offset and keeps its fade-out on the
/// right part.
pub fn split_clip(
	project: &Project,
	clip: ClipId,
	at: Tick,
	right_id: ClipId,
	right_take: Option<Take>,
) -> Option<Vec<Command>> {
	let (track, c) = project.find_clip(clip)?;
	if at <= c.start || at >= c.end() {
		return None;
	}
	let left_len = at - c.start;
	// A linked copy leaves its group first (see `links::unique_commands`), losing the
	// shared notes past its end: split what it keeps.
	let trimmed;
	let c = if c.link.is_some() {
		let mut t = c.clone();
		if let ClipContent::Midi { notes, .. } = &mut t.content {
			notes.retain(|n| n.start < c.length);
		}
		trimmed = t;
		&trimmed
	} else {
		c
	};
	let mut right = Clip {
		id: right_id,
		name: c.name.clone(),
		start: at,
		length: c.end() - at,
		content: c.content.clone(),
		take: right_take,
		muted: c.muted,
		link: None,
	};
	// Note and fade edits first, so the left clip never holds notes past its new end.
	// A linked clip leaves its group first: cutting one copy must not cut the others.
	let mut commands = crate::links::unique_commands(project, clip);
	match (&c.content, &mut right.content) {
		(
			ClipContent::Midi {
				notes, controllers, ..
			},
			ClipContent::Midi {
				notes: right_notes,
				controllers: right_controllers,
			},
		) => {
			*right_controllers = crate::controller::window(controllers, left_len, c.length);
			for lane in controllers {
				if lane.points.iter().any(|p| p.tick >= left_len) {
					commands.push(Command::SetControllerLane {
						clip,
						controller: lane.controller,
						points: lane
							.points
							.iter()
							.filter(|p| p.tick < left_len)
							.copied()
							.collect(),
					});
				}
			}
			let moved: Vec<&Note> = notes.iter().filter(|n| n.start >= left_len).collect();
			*right_notes = moved
				.iter()
				.map(|n| Note {
					id: NoteId::new(),
					start: n.start - left_len,
					..(*n).clone()
				})
				.collect();
			if !moved.is_empty() {
				commands.push(Command::RemoveNotes {
					clip,
					notes: moved.iter().map(|n| n.id).collect(),
				});
			}
			let shortened: Vec<NotePatch> = notes
				.iter()
				.filter(|n| n.start < left_len && n.end() > left_len)
				.map(|n| NotePatch {
					id: n.id,
					pitch: None,
					velocity: None,
					start: None,
					length: Some(left_len - n.start),
					bend: None,
				})
				.collect();
			if !shortened.is_empty() {
				commands.push(Command::UpdateNotes {
					clip,
					notes: shortened,
				});
			}
		}
		(
			ClipContent::Audio {
				fade_in, fade_out, ..
			},
			ClipContent::Audio {
				offset_seconds,
				fade_in: right_in,
				fade_out: right_out,
				..
			},
		) => {
			let tempo = &project.tempo;
			let cut = tempo.tick_to_seconds(at) - tempo.tick_to_seconds(c.start);
			*offset_seconds += cut;
			*right_in = None;
			let right_secs = tempo.tick_to_seconds(c.end()) - tempo.tick_to_seconds(at);
			*right_out = fade_out.filter(|f| f.seconds < right_secs);
			if fade_out.is_some() {
				commands.push(Command::SetClipFades {
					clip,
					fade_in: fade_in.filter(|f| f.seconds < cut),
					fade_out: None,
				});
			}
		}
		_ => return None,
	}
	commands.push(Command::UpdateClip {
		clip,
		patch: ClipPatch {
			length: Some(left_len),
			..ClipPatch::default()
		},
	});
	commands.push(Command::AddClip {
		track: track.id,
		clip: right,
	});
	Some(commands)
}

/// Splits take group `group` at `at` into two groups so each part can play another
/// take: every take crossing `at` is split (its right part joins `new_group` with the
/// same lane and active state) and takes starting at or after `at` move to `new_group`.
/// `None` when no take reaches past `at` or none starts before it (nothing to split).
pub fn split_group(
	project: &Project,
	group: TakeGroupId,
	at: Tick,
	new_group: TakeGroupId,
) -> Option<Vec<Command>> {
	let clips = group_clips(project, group);
	if !clips.iter().any(|(_, c)| c.start < at) || !clips.iter().any(|(_, c)| c.end() > at) {
		return None;
	}
	let mut commands = Vec::new();
	for (_, c) in clips {
		let take = c.take?;
		let moved = Take {
			group: new_group,
			..take
		};
		if c.start >= at {
			commands.push(Command::UpdateClip {
				clip: c.id,
				patch: ClipPatch {
					take: Some(Some(moved)),
					..ClipPatch::default()
				},
			});
		} else if c.end() > at {
			commands.extend(split_clip(project, c.id, at, ClipId::new(), Some(moved))?);
		}
	}
	Some(commands)
}

/// Swipe comping: plays the take of `clip` from `from` to `to` (clamped to its group),
/// splitting the group at both ends as needed. One `Batch` ("Comp take"); `None` when
/// nothing changes.
pub fn comp_range(project: &Project, clip: ClipId, from: Tick, to: Tick) -> Option<Command> {
	let (_, picked) = project.find_clip(clip)?;
	let take = picked.take?;
	let (from, to) = (from.min(to), from.max(to));
	let mut doc = Document::new(project.clone());
	let mut commands = Vec::new();
	let run = |doc: &mut Document, cmds: Vec<Command>, out: &mut Vec<Command>| -> bool {
		for cmd in cmds {
			if doc.execute(cmd.clone()).is_err() {
				return false;
			}
			out.push(cmd);
		}
		true
	};
	// The part from `from` on lives in `middle` after the first split.
	let mut middle = take.group;
	let first = TakeGroupId::new();
	if let Some(cmds) = split_group(doc.project(), middle, from, first) {
		if !run(&mut doc, cmds, &mut commands) {
			return None;
		}
		middle = first;
	}
	if let Some(cmds) = split_group(doc.project(), middle, to, TakeGroupId::new())
		&& !run(&mut doc, cmds, &mut commands)
	{
		return None;
	}
	// The picked lane's clip in the middle part (the one overlapping the range most).
	let target = group_clips(doc.project(), middle)
		.into_iter()
		.filter(|(_, c)| c.take.is_some_and(|t| t.lane == take.lane))
		.max_by_key(|(_, c)| (c.end().min(to) - c.start.max(from)).0)
		.map(|(_, c)| c.id)?;
	if let Some(Command::Batch { commands: cmds, .. }) = select_take(doc.project(), target)
		&& !run(&mut doc, cmds, &mut commands)
	{
		return None;
	}
	(!commands.is_empty()).then(|| Command::Batch {
		label: "Comp take".into(),
		commands,
	})
}

/// Removes the takes of `group` that do not play and makes the active one an ordinary
/// clip ("flatten"). `None` for an unknown group.
pub fn flatten_group(project: &Project, group: TakeGroupId) -> Option<Command> {
	let clips = group_clips(project, group);
	if clips.is_empty() {
		return None;
	}
	let commands = clips
		.into_iter()
		.map(|(_, c)| {
			if c.plays() {
				Command::UpdateClip {
					clip: c.id,
					patch: ClipPatch {
						take: Some(None),
						..ClipPatch::default()
					},
				}
			} else {
				Command::RemoveClip { clip: c.id }
			}
		})
		.collect();
	Some(Command::Batch {
		label: "Flatten takes".into(),
		commands,
	})
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::project::{Track, TrackKind};
	use cadenza_core::PPQ;

	fn bar(n: i64) -> Tick {
		Tick(PPQ * 4 * n)
	}

	/// An audio track with three 2-bar takes from bar 1 (the last one active).
	fn takes() -> (Project, TakeGroupId, Vec<ClipId>) {
		let mut p = Project::new("t");
		let mut t = Track::new("Vox", TrackKind::Audio);
		let group = TakeGroupId::new();
		let mut ids = Vec::new();
		for lane in 0..3u32 {
			let mut c = Clip::audio(format!("Take {}", lane + 1), bar(1), bar(2), "/rec.wav");
			if let ClipContent::Audio { offset_seconds, .. } = &mut c.content {
				*offset_seconds = f64::from(lane) * 4.0;
			}
			c.take = Some(Take {
				group,
				lane,
				active: lane == 2,
			});
			ids.push(c.id);
			t.clips.push(c);
		}
		p.tracks.push(t);
		(p, group, ids)
	}

	fn active(p: &Project) -> Vec<(u32, Tick, Tick)> {
		let mut v: Vec<(u32, Tick, Tick)> = p.tracks[0]
			.clips
			.iter()
			.filter(|c| c.plays())
			.map(|c| (c.take.unwrap().lane, c.start, c.end()))
			.collect();
		v.sort_by_key(|x| x.1);
		v
	}

	#[test]
	fn selecting_a_take_switches_the_playing_clip() {
		let (p, group, ids) = takes();
		assert_eq!(lane_count(&p, group), 3);
		let mut doc = Document::new(p);
		let cmd = select_take(doc.project(), ids[0]).unwrap();
		assert_eq!(cmd.label(), "Select take");
		doc.execute(cmd).unwrap();
		assert_eq!(active(doc.project()), vec![(0, bar(1), bar(3))]);
		assert!(
			select_take(doc.project(), ids[0]).is_none(),
			"already active"
		);
		doc.undo().unwrap();
		assert_eq!(active(doc.project()), vec![(2, bar(1), bar(3))]);
	}

	#[test]
	fn comping_a_range_splits_the_group_and_plays_that_take_there() {
		let (p, group, ids) = takes();
		let mut doc = Document::new(p);
		let from = bar(1) + Tick(PPQ * 2);
		let to = bar(2);
		let cmd = comp_range(doc.project(), ids[0], from, to).unwrap();
		doc.execute(cmd).unwrap();
		assert_eq!(
			active(doc.project()),
			vec![(2, bar(1), from), (0, from, to), (2, to, bar(3))]
		);
		// Every take was split into three parts (9 clips, three groups).
		assert_eq!(doc.project().tracks[0].clips.len(), 9);
		assert_eq!(group_clips(doc.project(), group).len(), 3);
		// The middle part of take 1 continues the file where it was cut.
		let mid = doc.project().tracks[0]
			.clips
			.iter()
			.find(|c| c.start == from && c.take.unwrap().lane == 0)
			.unwrap();
		let ClipContent::Audio { offset_seconds, .. } = mid.content else {
			unreachable!()
		};
		assert!((offset_seconds - 1.0).abs() < 1e-9, "{offset_seconds}");
		// One undo step restores the three whole takes.
		doc.undo().unwrap();
		assert_eq!(doc.project().tracks[0].clips.len(), 3);
		assert_eq!(active(doc.project()), vec![(2, bar(1), bar(3))]);
	}

	#[test]
	fn flattening_keeps_only_the_playing_take() {
		let (p, group, _) = takes();
		let mut doc = Document::new(p);
		doc.execute(flatten_group(doc.project(), group).unwrap())
			.unwrap();
		let clips = &doc.project().tracks[0].clips;
		assert_eq!(clips.len(), 1);
		assert!(clips[0].take.is_none() && clips[0].name == "Take 3");
	}

	#[test]
	fn takes_and_inputs_survive_json() {
		let (mut p, _, _) = takes();
		p.tracks[0].input = Some(crate::TrackInput::stereo(2));
		let json = serde_json::to_string(&p).unwrap();
		let back: Project = serde_json::from_str(&json).unwrap();
		assert_eq!(back, p);
		// Older files (no take, no input) still load.
		let plain = Project::new("old");
		let json = serde_json::to_string(&plain).unwrap();
		assert!(!json.contains("take") && !json.contains("input"));
	}
}
