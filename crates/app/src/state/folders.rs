//! Folder tracks in the app (pure builders): what is hidden by collapsed folders, nesting
//! depth, moving tracks into and out of folders (folders move with their contents) and
//! grouping tracks into a new folder.

use std::collections::HashSet;

use cadenza_core::TrackId;
use cadenza_model::{Command, Project, Track, TrackKind};

/// Whether `track` is inside a collapsed folder.
pub fn is_hidden(project: &Project, collapsed: &HashSet<TrackId>, track: TrackId) -> bool {
	!collapsed.is_empty()
		&& project
			.folders_of(track)
			.iter()
			.any(|f| collapsed.contains(f))
}

/// Nesting depth of `track` (0 = top level).
pub fn depth(project: &Project, track: TrackId) -> usize {
	project.folders_of(track).len()
}

/// The tracks that move with `track`: itself, plus its contents when it is a folder (in
/// display order).
fn block(project: &Project, track: TrackId) -> Vec<TrackId> {
	let mut out = vec![track];
	if project
		.track(track)
		.is_some_and(|t| t.kind == TrackKind::Folder)
	{
		out.extend(project.folder_contents(track));
	}
	out
}

/// The commands that turn the display order into `target` (a permutation of it).
fn reorder(project: &Project, target: &[TrackId]) -> Vec<Command> {
	let mut current: Vec<TrackId> = project.tracks.iter().map(|t| t.id).collect();
	let mut out = Vec::new();
	for (i, &id) in target.iter().enumerate() {
		if current[i] == id {
			continue;
		}
		let from = current.iter().position(|t| *t == id).expect("permutation");
		let t = current.remove(from);
		current.insert(i, t);
		out.push(Command::MoveTrack {
			track: id,
			index: i,
		});
	}
	out
}

/// Moves `track` (with its contents when it is a folder) to where the arrange's reorder
/// line shows it: below row `to` when moving down, above it when moving up (a collapsed
/// folder counts as one row). It is filed like DAWs do: right under an open folder = into
/// that folder, else into the folder of the row above (top level when none). One undo
/// step; `None` when nothing changes.
pub fn move_track(
	project: &Project,
	collapsed: &HashSet<TrackId>,
	track: TrackId,
	to: usize,
) -> Option<Command> {
	let from = project.track_index(track)?;
	let moving = block(project, track);
	let mut rest: Vec<TrackId> = project
		.tracks
		.iter()
		.map(|t| t.id)
		.filter(|t| !moving.contains(t))
		.collect();
	let target = project
		.tracks
		.get(to.min(project.tracks.len().saturating_sub(1)))?
		.id;
	let at = if moving.contains(&target) {
		// Dropped on itself or its own contents: stays.
		rest.iter()
			.position(|t| project.track_index(*t).is_some_and(|i| i > from))
			.unwrap_or(rest.len())
	} else if to > from {
		let i = rest.iter().position(|t| *t == target)?;
		let mut end = i + 1;
		if collapsed.contains(&target) {
			let inside = project.folder_contents(target);
			while end < rest.len() && inside.contains(&rest[end]) {
				end += 1;
			}
		}
		end
	} else {
		rest.iter().position(|t| *t == target)?
	};
	for (k, id) in moving.iter().enumerate() {
		rest.insert(at + k, *id);
	}
	let folder = at
		.checked_sub(1)
		.and_then(|i| project.track(rest[i]))
		.and_then(|above| {
			// The visible row above: the outermost collapsed folder holding it, else itself.
			let visible = project
				.folders_of(above.id)
				.into_iter()
				.rfind(|f| collapsed.contains(f))
				.and_then(|f| project.track(f))
				.unwrap_or(above);
			if visible.kind == TrackKind::Folder && !collapsed.contains(&visible.id) {
				Some(visible.id)
			} else {
				visible.folder
			}
		})
		.filter(|f| !moving.contains(f));
	let mut commands = reorder(project, &rest);
	if project.track(track)?.folder != folder {
		commands.push(Command::SetTrackFolder { track, folder });
	}
	match commands.len() {
		0 => None,
		_ => Some(Command::Batch {
			label: "Move track".into(),
			commands,
		}),
	}
}

/// Puts `tracks` into a new folder `folder` inserted where the first of them is (they
/// gather under it in their display order). One undo step.
pub fn group_into_folder(project: &Project, tracks: &[TrackId], folder: Track) -> Option<Command> {
	let mut members: Vec<TrackId> = project
		.tracks
		.iter()
		.map(|t| t.id)
		.filter(|t| tracks.contains(t))
		.collect();
	let first = *members.first()?;
	let index = project.track_index(first)?;
	let parent = project.track(first)?.folder;
	let folder_id = folder.id;
	// The scratch order after the folder is inserted: members gather under it.
	let mut scratch = project.clone();
	scratch.tracks.insert(index, folder.clone());
	let all = members.clone();
	members.retain(|m| !scratch.folders_of(*m).iter().any(|f| all.contains(f)));
	let mut commands = vec![Command::AddTrack {
		track: Track {
			folder: parent,
			..folder
		},
		index: Some(index),
	}];
	let mut order: Vec<TrackId> = scratch
		.tracks
		.iter()
		.map(|t| t.id)
		.filter(|t| !members.contains(t))
		.collect();
	let at = order
		.iter()
		.position(|t| *t == folder_id)
		.expect("inserted")
		+ 1;
	let mut moved = Vec::new();
	for m in &members {
		moved.extend(block(&scratch, *m));
	}
	order.retain(|t| !moved.contains(t));
	for (k, id) in moved.iter().enumerate() {
		order.insert(at + k, *id);
	}
	commands.extend(reorder(&scratch, &order));
	for m in &members {
		commands.push(Command::SetTrackFolder {
			track: *m,
			folder: Some(folder_id),
		});
	}
	Some(Command::Batch {
		label: "Group into folder".into(),
		commands,
	})
}

/// Takes `track` one level out of its folder (it stays where it is shown).
pub fn take_out(project: &Project, track: TrackId) -> Option<Command> {
	let folder = project.track(track)?.folder?;
	let parent = project.track(folder)?.folder;
	// It moves below the folder's last track so it is visibly outside.
	let contents = project.folder_contents(folder);
	let last = contents
		.iter()
		.filter_map(|t| project.track_index(*t))
		.max()?;
	let mut commands = Vec::new();
	let mut order: Vec<TrackId> = project.tracks.iter().map(|t| t.id).collect();
	let moving = block(project, track);
	let last_id = order[last];
	order.retain(|t| !moving.contains(t));
	let at = order
		.iter()
		.position(|t| *t == last_id)
		.map_or(order.len(), |i| i + 1);
	for (k, id) in moving.iter().enumerate() {
		order.insert(at + k, *id);
	}
	commands.extend(reorder(project, &order));
	commands.push(Command::SetTrackFolder {
		track,
		folder: parent,
	});
	Some(Command::Batch {
		label: "Take track out of folder".into(),
		commands,
	})
}

#[cfg(test)]
mod tests {
	use super::*;
	use cadenza_model::Document;

	fn song() -> (Project, Vec<TrackId>) {
		let mut p = Project::new("f");
		for (name, kind) in [
			("A", TrackKind::Instrument),
			("B", TrackKind::Audio),
			("C", TrackKind::Instrument),
			("D", TrackKind::Audio),
		] {
			p.tracks.push(Track::new(name, kind));
		}
		let ids = p.tracks.iter().map(|t| t.id).collect();
		(p, ids)
	}

	fn names(p: &Project) -> Vec<String> {
		p.tracks
			.iter()
			.map(|t| {
				let indent = "  ".repeat(depth(p, t.id));
				format!("{indent}{}", t.name)
			})
			.collect()
	}

	#[test]
	fn grouping_dragging_and_taking_out() {
		let (p, ids) = song();
		let mut doc = Document::new(p);
		let folder = Track::new("Group", TrackKind::Folder);
		let fid = folder.id;
		let cmd = group_into_folder(doc.project(), &[ids[1], ids[3]], folder).unwrap();
		doc.execute(cmd).unwrap();
		assert_eq!(names(doc.project()), ["A", "Group", "  B", "  D", "C"]);
		assert_eq!(doc.history_labels(), vec!["Group into folder"]);

		// Drag A right under the folder: it goes in.
		let none = HashSet::new();
		let cmd = move_track(doc.project(), &none, ids[0], 1).unwrap();
		doc.execute(cmd).unwrap();
		assert_eq!(names(doc.project()), ["Group", "  A", "  B", "  D", "C"]);

		// Moving the folder carries its contents.
		let cmd = move_track(doc.project(), &none, fid, 4).unwrap();
		doc.execute(cmd).unwrap();
		assert_eq!(names(doc.project()), ["C", "Group", "  A", "  B", "  D"]);

		// Taking B out puts it under the folder's last track, at the top level.
		let cmd = take_out(doc.project(), ids[1]).unwrap();
		doc.execute(cmd).unwrap();
		assert_eq!(names(doc.project()), ["C", "Group", "  A", "  D", "B"]);

		// Collapsing hides the contents.
		let collapsed: HashSet<TrackId> = [fid].into();
		assert!(is_hidden(doc.project(), &collapsed, ids[0]));
		assert!(!is_hidden(doc.project(), &collapsed, ids[1]));
		doc.undo().unwrap();
		doc.undo().unwrap();
		doc.undo().unwrap();
		doc.undo().unwrap();
		assert_eq!(names(doc.project()), ["A", "B", "C", "D"]);
	}
}
