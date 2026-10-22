//! Linked clips (FL pattern instances): pure builders producing ordinary [`Command`]s.
//!
//! Clips sharing a [`crate::Clip::link`] hold identical MIDI content; the `Document`
//! keeps it so (every note/controller command on one clip edits all of them, in the same
//! undo step). Copies are made linked by giving them the source's link (or a new one
//! that the source joins in the same batch); "make unique" clears the link.

use cadenza_core::{ClipId, LinkId, Tick, TrackId};

use crate::command::{ClipPatch, Command};
use crate::project::{Clip, Project};

/// Commands adding a copy of `clip` as `new_id` at `start` on `track` (default: the
/// source's track). `linked` copies of a MIDI clip share its content: an unlinked
/// source joins `new_link` first. Audio clips are always copied independently.
pub fn copy_clip(
	project: &Project,
	clip: ClipId,
	new_id: ClipId,
	track: Option<TrackId>,
	start: Tick,
	linked: bool,
	new_link: LinkId,
) -> Option<Vec<Command>> {
	let (t, c) = project.find_clip(clip)?;
	let mut copy = Clip {
		id: new_id,
		start,
		take: None,
		link: None,
		..c.clone()
	};
	let mut commands = Vec::new();
	if linked && c.is_midi() {
		let link = match c.link {
			Some(link) => link,
			None => {
				commands.push(set_link(clip, Some(new_link)));
				new_link
			}
		};
		copy.link = Some(link);
	}
	commands.push(Command::AddClip {
		track: track.unwrap_or(t.id),
		clip: copy,
	});
	Some(commands)
}

/// Unlinks `clip` from its copies (its content stays as it is). `None` when it is not
/// linked. When only one other copy is left, that one becomes independent too.
pub fn make_unique(project: &Project, clip: ClipId) -> Option<Command> {
	let commands = unique_commands(project, clip);
	(!commands.is_empty()).then(|| Command::Batch {
		label: "Make clip unique".into(),
		commands,
	})
}

/// The commands of [`make_unique`] (empty when `clip` is not linked), for builders that
/// change one copy's content on its own (split, trim, stretch).
pub fn unique_commands(project: &Project, clip: ClipId) -> Vec<Command> {
	let Some(link) = project.find_clip(clip).and_then(|(_, c)| c.link) else {
		return Vec::new();
	};
	let others: Vec<ClipId> = project
		.linked_clips(link)
		.filter(|o| o.id != clip)
		.map(|o| o.id)
		.collect();
	let mut freed = vec![clip];
	if let [last] = others.as_slice() {
		freed.push(*last);
	}
	// Shared notes past a copy's own end would be notes outside an independent clip
	// (never heard there): such a copy is replaced by a trimmed one with the same id.
	// Replacements go first so undo re-adds the original while the group still exists.
	let mut replace = Vec::new();
	let mut unlink = Vec::new();
	for id in freed {
		let Some((t, c)) = project.find_clip(id) else {
			continue;
		};
		if c.notes().iter().any(|n| n.start >= c.length) {
			let mut trimmed = c.clone();
			trimmed.link = None;
			if let crate::ClipContent::Midi { notes, .. } = &mut trimmed.content {
				notes.retain(|n| n.start < c.length);
			}
			replace.push(Command::RemoveClip { clip: id });
			replace.push(Command::AddClip {
				track: t.id,
				clip: trimmed,
			});
		} else {
			unlink.push(set_link(id, None));
		}
	}
	replace.extend(unlink);
	replace
}

/// Number of clips sharing `clip`'s content, itself included (1 = not linked).
pub fn link_count(project: &Project, clip: ClipId) -> usize {
	project
		.find_clip(clip)
		.and_then(|(_, c)| c.link)
		.map_or(1, |l| project.linked_clips(l).count())
}

fn set_link(clip: ClipId, link: Option<LinkId>) -> Command {
	Command::UpdateClip {
		clip,
		patch: ClipPatch {
			link: Some(link),
			..ClipPatch::default()
		},
	}
}
