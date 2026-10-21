//! FL Studio playlist editing in the arrange: clip clipboard (Ctrl+C / X / V), Ctrl+B
//! clone, the Paint tool, the knife (Split tool dragged across tracks), sweep delete /
//! mute and quantizing clip starts. Pure gesture -> command builders; every gesture is one
//! command (one undo step). The view only hit-tests.

use crate::i18n::mark;
use cadenza_core::{ClipId, LinkId, NoteId, Tick, TrackId};
use cadenza_model::{Clip, ClipContent, ClipPatch, Command, Project};

use super::edits::{self, batch};
use super::piano::{ceil_to, snap_floor};

/// Clips copied with Ctrl+C / Ctrl+X.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ClipClipboard {
	/// The copied clips with their track and its display index (relative tracks).
	pub clips: Vec<(TrackId, usize, Clip)>,
	/// Earliest start of the copied clips.
	pub origin: Tick,
	/// Earliest start to latest end.
	pub span: Tick,
	/// The playhead when the clips were copied (or last pasted).
	pub cue: Tick,
	/// Where Ctrl+V pastes while the playhead stays at `cue`: right after the copied span
	/// (where they were for a cut), then after each paste, so repeated pastes append.
	pub next: Tick,
}

impl ClipClipboard {
	pub fn is_empty(&self) -> bool {
		self.clips.is_empty()
	}

	/// Where Ctrl+V pastes: at the playhead when it was moved since the copy (and the
	/// transport is stopped), else at [`ClipClipboard::next`].
	pub fn paste_at(&self, playhead: Tick, playing: bool) -> Tick {
		if !playing && playhead != self.cue {
			playhead
		} else {
			self.next
		}
	}

	/// After a paste at `at`: the next one appends after it.
	pub fn pasted(&mut self, at: Tick, playhead: Tick) {
		self.next = at + self.span;
		self.cue = playhead;
	}
}

/// The clipboard for `ids` (takes that do not play are left out). `cut` pastes back
/// where the clips were.
pub fn clipboard(project: &Project, ids: &[ClipId], playhead: Tick, cut: bool) -> ClipClipboard {
	let clips: Vec<(TrackId, usize, Clip)> = project
		.tracks
		.iter()
		.enumerate()
		.flat_map(|(i, t)| {
			t.clips
				.iter()
				.filter(|c| ids.contains(&c.id) && c.plays())
				.map(move |c| {
					// MIDI clips get a link id on the board so every linked paste of a cut
					// clip (gone from the project) shares one group.
					let mut c = c.clone();
					if c.is_midi() && c.link.is_none() {
						c.link = Some(LinkId::new());
					}
					(t.id, i, c)
				})
		})
		.collect();
	let origin = clips
		.iter()
		.map(|(_, _, c)| c.start)
		.min()
		.unwrap_or(Tick::ZERO);
	let end = clips
		.iter()
		.map(|(_, _, c)| c.end())
		.max()
		.unwrap_or(origin);
	let span = end - origin;
	ClipClipboard {
		clips,
		origin,
		span,
		cue: playhead,
		next: if cut { origin } else { origin + span },
	}
}

/// A copy of `clip` at `start` with fresh clip and note ids, as an ordinary clip (no take,
/// not linked).
pub fn copy_of(clip: &Clip, start: Tick) -> Clip {
	let mut copy = clip.clone();
	copy.id = ClipId::new();
	copy.start = start;
	copy.take = None;
	copy.link = None;
	if let ClipContent::Midi { notes, .. } = &mut copy.content {
		for n in notes {
			n.id = NoteId::new();
		}
	}
	copy
}

/// Makes the copies of one gesture: linked copies of MIDI clips (FL pattern instances,
/// the default) or independent ones (Shift). A linked copy shares the source's content
/// and link; an unlinked source joins the new link in the same step. Audio clips, and
/// sources whose group no longer holds their content, are copied independently.
pub struct Copier<'a> {
	project: &'a Project,
	linked: bool,
	/// Source clip -> link id chosen in this gesture.
	links: Vec<(ClipId, LinkId)>,
	/// Commands that link sources (they run before the copies are added).
	joins: Vec<Command>,
}

impl<'a> Copier<'a> {
	pub fn new(project: &'a Project, linked: bool) -> Self {
		Self {
			project,
			linked,
			links: Vec::new(),
			joins: Vec::new(),
		}
	}

	/// A copy of `clip` (the project's current version when it still exists) at `start`.
	pub fn copy(&mut self, clip: &Clip, start: Tick) -> Clip {
		let current = self.project.find_clip(clip.id).map(|(_, c)| c);
		let src = current.filter(|c| c.is_midi()).unwrap_or(clip);
		if !self.linked || !src.is_midi() {
			return copy_of(src, start);
		}
		let link = match self.links.iter().find(|(c, _)| *c == clip.id) {
			Some((_, l)) => *l,
			None => {
				let l = src.link.or(clip.link).unwrap_or_default();
				// The group must still hold this content (a stale clipboard copy of an
				// edited clip is pasted independently).
				if self
					.project
					.linked_clips(l)
					.any(|o| !o.same_midi_content(src))
				{
					return copy_of(src, start);
				}
				if let Some(c) = current.filter(|c| c.link.is_none()) {
					self.joins.push(Command::UpdateClip {
						clip: c.id,
						patch: ClipPatch {
							link: Some(Some(l)),
							..ClipPatch::default()
						},
					});
				}
				self.links.push((clip.id, l));
				l
			}
		};
		Clip {
			id: ClipId::new(),
			start,
			take: None,
			link: Some(link),
			..src.clone()
		}
	}

	/// The gesture's commands: source links first, then `adds`.
	pub fn finish(self, adds: Vec<Command>) -> Vec<Command> {
		let mut commands = self.joins;
		commands.extend(adds);
		commands
	}
}

/// Ctrl+V: the clipboard's clips with their earliest start at `at`. With `anchor` (the
/// track under the pointer) the topmost copied track lands on it and the others keep
/// their distance below; a clip whose target track is missing or of another kind goes
/// back to its own track (or is skipped). Returns the command and the new clip ids.
pub fn paste_clips(
	project: &Project,
	board: &ClipClipboard,
	at: Tick,
	anchor: Option<TrackId>,
	linked: bool,
) -> Option<(Command, Vec<ClipId>)> {
	let top = board.clips.iter().map(|(_, i, _)| *i).min()?;
	let mut copier = Copier::new(project, linked);
	let anchor = anchor.and_then(|t| project.track_index(t));
	let at = Tick(at.0.max(0));
	let mut ids = Vec::new();
	let mut commands = Vec::new();
	for (track, index, clip) in &board.clips {
		let kind = |t: &cadenza_model::Track| clip_fits(t, clip);
		let target = anchor
			.and_then(|a| project.tracks.get(a + (index - top)))
			.filter(|t| kind(t))
			.or_else(|| project.track(*track).filter(|t| kind(t)));
		let Some(target) = target else {
			continue;
		};
		let copy = copier.copy(clip, at + (clip.start - board.origin));
		ids.push(copy.id);
		commands.push(Command::AddClip {
			track: target.id,
			clip: copy,
		});
	}
	Some((batch("Paste clips", copier.finish(commands))?, ids))
}

/// Whether `clip` can live on `track` (MIDI on instrument tracks, audio on audio tracks).
pub fn clip_fits(track: &cadenza_model::Track, clip: &Clip) -> bool {
	use cadenza_model::TrackKind;
	match track.kind {
		TrackKind::Instrument => clip.is_midi(),
		TrackKind::Audio => !clip.is_midi(),
		TrackKind::Bus | TrackKind::Folder => false,
	}
}

/// How far Ctrl+B shifts the copies: the selection's span (earliest start to latest end)
/// widened to the grid `step`; spans of a bar or more round up to whole bars.
pub fn clone_offset(project: &Project, ids: &[ClipId], step: Option<Tick>) -> Option<Tick> {
	let found: Vec<&Clip> = ids
		.iter()
		.filter_map(|c| project.find_clip(*c))
		.map(|(_, c)| c)
		.collect();
	let start = found.iter().map(|c| c.start).min()?;
	let end = found.iter().map(|c| c.end()).max()?;
	let span = match step.filter(|s| s.0 > 0) {
		Some(s) => ceil_to(end, s) - snap_floor(start, Some(s)),
		None => end - start,
	};
	let bar = project.meter().signature_at(start).ticks_per_bar();
	Some(if span >= bar {
		ceil_to(span, bar)
	} else {
		span
	})
}

/// Ctrl+B (FL "duplicate"): copies of the selection right after itself on their own
/// tracks (see [`clone_offset`]). Returns the command and the copies (select them so a
/// repeated Ctrl+B keeps appending).
pub fn clone_clips(
	project: &Project,
	ids: &[ClipId],
	step: Option<Tick>,
	linked: bool,
) -> Option<(Command, Vec<ClipId>)> {
	let offset = clone_offset(project, ids, step)?;
	let mut new_ids = Vec::new();
	let mut copier = Copier::new(project, linked);
	let commands = ids
		.iter()
		.filter_map(|c| project.find_clip(*c))
		.map(|(t, c)| {
			let copy = copier.copy(c, c.start + offset);
			new_ids.push(copy.id);
			Command::AddClip {
				track: t.id,
				clip: copy,
			}
		})
		.collect();
	Some((batch("Clone clips", copier.finish(commands))?, new_ids))
}

/// The clip the Paint tool repeats on `track`: the clip pressed, else the selected clip
/// on that track, else the last clip used; it must fit the track.
pub fn paint_source(
	project: &Project,
	track: TrackId,
	pressed: Option<ClipId>,
	selection: &[ClipId],
	last: Option<ClipId>,
) -> Option<ClipId> {
	let t = project.track(track)?;
	let fits = |id: &ClipId| project.find_clip(*id).is_some_and(|(_, c)| clip_fits(t, c));
	pressed
		.filter(fits)
		.or_else(|| {
			selection
				.iter()
				.rev()
				.find(|c| t.clips.iter().any(|x| x.id == **c))
				.copied()
		})
		.filter(fits)
		.or(last.filter(fits))
}

/// Starts of the paint cells from the one under `a` to the one under `b` (cells of
/// `length` from `first`), never before 0.
pub fn paint_starts(first: Tick, length: Tick, a: Tick, b: Tick) -> Vec<Tick> {
	let len = length.0.max(1);
	let cell = |t: Tick| (t - first).0.div_euclid(len);
	let (i0, i1) = (cell(a).min(cell(b)), cell(a).max(cell(b)));
	(i0..=i1)
		.map(|i| first + Tick(i * len))
		.filter(|s| s.0 >= 0)
		.collect()
}

/// The Paint tool's clips: copies of `template` at `starts` on `track`, skipping cells
/// that would overlap a clip already there (one step). Returns the command and the copies.
pub fn paint_clips(
	project: &Project,
	template: ClipId,
	track: TrackId,
	starts: &[Tick],
	linked: bool,
) -> Option<(Command, Vec<ClipId>)> {
	let (_, clip) = project.find_clip(template)?;
	let mut copier = Copier::new(project, linked);
	let t = project.track(track)?;
	if !clip_fits(t, clip) {
		return None;
	}
	let mut taken: Vec<(Tick, Tick)> = t
		.clips
		.iter()
		.filter(|c| c.plays())
		.map(|c| (c.start, c.end()))
		.collect();
	let mut ids = Vec::new();
	let mut commands = Vec::new();
	for &start in starts {
		let end = start + clip.length;
		if taken.iter().any(|&(s, e)| s < end && e > start) {
			continue;
		}
		taken.push((start, end));
		let copy = copier.copy(clip, start);
		ids.push(copy.id);
		commands.push(Command::AddClip { track, clip: copy });
	}
	Some((batch("Paint clips", copier.finish(commands))?, ids))
}

/// The knife: where a line from `a` to `b` (content x, y) crosses each track row
/// (`(track, top, bottom)`): the line's x at the row's vertical centre (clamped to the
/// line's ends), so a slanted line cuts each track at its own x.
pub fn knife_rows(
	rows: &[(TrackId, f32, f32)],
	a: (f32, f32),
	b: (f32, f32),
) -> Vec<(TrackId, f32)> {
	let (lo, hi) = (a.1.min(b.1), a.1.max(b.1));
	rows.iter()
		.filter(|(_, top, bottom)| *top <= hi && *bottom > lo)
		.map(|&(track, top, bottom)| {
			let y = ((top + bottom) * 0.5).clamp(lo, hi);
			let dy = b.1 - a.1;
			let x = if dy.abs() < 0.5 {
				(a.0 + b.0) * 0.5
			} else {
				a.0 + (b.0 - a.0) * (y - a.1) / dy
			};
			(track, x)
		})
		.collect()
}

/// One command cutting every playing clip of each track at its tick ("Slice clips").
pub fn slice(project: &Project, cuts: &[(TrackId, Tick)]) -> Option<Command> {
	let commands: Vec<Command> = cuts
		.iter()
		.filter_map(|&(track, at)| project.track(track).map(|t| (t, at)))
		.flat_map(|(t, at)| {
			t.clips
				.iter()
				.filter(move |c| c.plays() && c.start < at && c.end() > at)
				.filter_map(move |c| edits::split_clip(project, c.id, at))
				.map(|(cmd, _)| cmd)
		})
		.collect();
	batch("Slice clips", commands)
}

/// Clips whose rect (`[x0, y0, x1, y1]`) the pointer path from `a` to `b` crosses (sweep
/// delete / mute), in the order they were reached.
pub fn sweep_hits(rects: &[(ClipId, [f32; 4])], a: (f32, f32), b: (f32, f32)) -> Vec<ClipId> {
	let len = ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt();
	let steps = (len / 2.0).ceil().max(1.0) as usize;
	let mut hits = Vec::new();
	for i in 0..=steps {
		let f = i as f32 / steps as f32;
		let (x, y) = (a.0 + (b.0 - a.0) * f, a.1 + (b.1 - a.1) * f);
		for (id, r) in rects {
			if x >= r[0] && x < r[2] && y >= r[1] && y < r[3] && !hits.contains(id) {
				hits.push(*id);
			}
		}
	}
	hits
}

/// Mutes (or unmutes) `ids`; clips already in that state are left alone (one step).
pub fn set_muted(project: &Project, ids: &[ClipId], muted: bool) -> Option<Command> {
	let commands: Vec<Command> = ids
		.iter()
		.filter_map(|c| project.find_clip(*c))
		.filter(|(_, c)| c.muted != muted)
		.map(|(_, c)| Command::UpdateClip {
			clip: c.id,
			patch: ClipPatch {
				muted: Some(muted),
				..ClipPatch::default()
			},
		})
		.collect();
	batch(if muted { "Mute clips" } else { "Unmute clips" }, commands)
}

/// The Mute tool / "Mute clip": every clip of `ids` takes the opposite of the first
/// one's state (a sweep never leaves a mix of both).
pub fn toggle_mute(project: &Project, ids: &[ClipId]) -> Option<Command> {
	let first = ids.iter().find_map(|c| project.find_clip(*c))?.1.muted;
	set_muted(project, ids, !first)
}

/// Moves each clip's start to the grid (`snap`), one step ("Quantize clips").
pub fn quantize_starts(
	project: &Project,
	ids: &[ClipId],
	snap: impl Fn(Tick) -> Tick,
) -> Option<Command> {
	let commands: Vec<Command> = ids
		.iter()
		.filter_map(|c| project.find_clip(*c))
		.filter_map(|(_, c)| {
			let start = Tick(snap(c.start).0.max(0));
			(start != c.start).then(|| Command::UpdateClip {
				clip: c.id,
				patch: ClipPatch {
					start: Some(start),
					..ClipPatch::default()
				},
			})
		})
		.collect();
	batch("Quantize clips", commands)
}

/// Every clip on `track` that plays (Ctrl+click on the header, "Select all clips").
pub fn track_clips(project: &Project, track: TrackId) -> Vec<ClipId> {
	project.track(track).map_or_else(Vec::new, |t| {
		t.clips.iter().filter(|c| c.plays()).map(|c| c.id).collect()
	})
}

/// "Make unique" (clip menu): unlinks the clips of `ids` from their copies, one step.
pub fn make_unique(project: &Project, ids: &[ClipId]) -> Option<Command> {
	let mut scratch = cadenza_model::Document::new(project.clone());
	let mut commands = Vec::new();
	for &id in ids {
		let unlink = cadenza_model::links::unique_commands(scratch.project(), id);
		for cmd in unlink {
			scratch.execute(cmd.clone()).ok()?;
			commands.push(cmd);
		}
	}
	batch(
		if ids.len() == 1 {
			"Make clip unique"
		} else {
			"Make clips unique"
		},
		commands,
	)
}

/// How "Chop into…" divides a clip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Chop {
	/// Equal parts.
	Parts(u32),
	/// At every beat of the meter (from the clip start).
	Beat,
	/// At every bar line inside the clip.
	Bar,
}

impl Chop {
	pub const MENU: [(Chop, &'static str); 6] = [
		(Chop::Parts(2), mark("2 parts")),
		(Chop::Parts(4), mark("4 parts")),
		(Chop::Parts(8), mark("8 parts")),
		(Chop::Parts(16), mark("16 parts")),
		(Chop::Beat, mark("Every beat")),
		(Chop::Bar, mark("Every bar")),
	];
}

/// Where `chop` cuts `clip` (song ticks strictly inside it).
pub fn chop_points(project: &Project, clip: &Clip, chop: Chop) -> Vec<Tick> {
	let (s, e) = (clip.start, clip.end());
	let mut cuts: Vec<Tick> = match chop {
		Chop::Parts(n) => {
			let n = i64::from(n.max(1));
			(1..n).map(|i| s + Tick(clip.length.0 * i / n)).collect()
		}
		Chop::Beat => {
			let meter = project.meter();
			let mut out = Vec::new();
			let mut t = s;
			loop {
				let sig = meter.signature_at(t);
				let beat = Tick::from_beats(4.0 / f64::from(sig.denominator.max(1)));
				t = t + Tick(beat.0.max(1));
				if t >= e {
					break;
				}
				out.push(t);
			}
			out
		}
		Chop::Bar => {
			let meter = project.meter();
			let mut out = Vec::new();
			let mut t = meter.bar_ceil(s + Tick(1));
			while t < e {
				out.push(t);
				t = meter.bar_ceil(t + Tick(1));
			}
			out
		}
	};
	cuts.retain(|t| *t > s && *t < e);
	cuts.dedup();
	cuts
}

/// "Chop into…": splits `clip` at every [`chop_points`] (MIDI and audio; a linked clip
/// becomes unique), one step. Returns the command and the parts (the clip first).
pub fn chop(project: &Project, clip: ClipId, chop: Chop) -> Option<(Command, Vec<ClipId>)> {
	let (_, c) = project.find_clip(clip)?;
	let cuts = chop_points(project, c, chop);
	let mut scratch = cadenza_model::Document::new(project.clone());
	let mut commands = Vec::new();
	let mut parts = vec![clip];
	let mut current = clip;
	for at in cuts {
		let Some((cmd, right)) = edits::split_clip(scratch.project(), current, at) else {
			continue;
		};
		scratch.execute(cmd.clone()).ok()?;
		commands.push(cmd);
		parts.push(right);
		current = right;
	}
	Some((batch("Chop clip", commands)?, parts))
}

/// Shift+drag of a MIDI clip's right edge (FL stretch): sets its length to `length` and
/// scales its notes (starts and lengths) and controller events by the same factor, one
/// step. A linked clip becomes unique first (only this copy is stretched).
pub fn stretch(project: &Project, clip: ClipId, length: Tick) -> Option<Command> {
	let (_, c) = project.find_clip(clip)?;
	if !c.is_midi() || length.0 <= 0 || length == c.length {
		return None;
	}
	let mut commands = cadenza_model::links::unique_commands(project, clip);
	let old = c.length.0 as f64;
	let ratio = length.0 as f64 / old;
	let scale = |t: Tick| Tick((t.0 as f64 * ratio).round() as i64);
	// What the copy keeps once unique: its notes start inside it.
	let notes: Vec<&cadenza_model::Note> =
		c.notes().iter().filter(|n| n.start < c.length).collect();
	let patches: Vec<cadenza_model::NotePatch> = notes
		.iter()
		.map(|n| {
			let start = Tick(scale(n.start).0.min(length.0 - 1).max(0));
			cadenza_model::NotePatch {
				id: n.id,
				pitch: None,
				velocity: None,
				start: Some(start),
				length: Some(Tick(scale(n.length).0.max(1))),
			}
		})
		.collect();
	let resize = Command::UpdateClip {
		clip,
		patch: ClipPatch {
			length: Some(length),
			..ClipPatch::default()
		},
	};
	let lanes: Vec<Command> = c
		.controllers()
		.iter()
		.map(|lane| {
			let mut points: Vec<_> = lane
				.points
				.iter()
				.map(|p| cadenza_model::ControllerPoint {
					tick: scale(p.tick),
					value: p.value,
				})
				.collect();
			points.dedup_by_key(|p| p.tick);
			Command::SetControllerLane {
				clip,
				controller: lane.controller,
				points,
			}
		})
		.collect();
	let growing = length > c.length;
	if growing {
		commands.push(resize.clone());
	}
	if !patches.is_empty() {
		commands.push(Command::UpdateNotes {
			clip,
			notes: patches,
		});
	}
	commands.extend(lanes);
	if !growing {
		commands.push(resize);
	}
	batch("Stretch clip", commands)
}

/// Resizing a MIDI clip's right edge (FL pattern repeat): growing a clip whose content
/// ends inside it repeats that content into the new part, on a period of the content
/// rounded up to whole bars (at most the old length), as notes with fresh ids and
/// controller events, in the same step as the resize. Otherwise a plain resize
/// ([`edits::resize_clip`]).
pub fn resize_repeating(project: &Project, clip: ClipId, end: Tick) -> Option<Command> {
	let resize = edits::resize_clip(project, clip, end)?;
	let (_, c) = project.find_clip(clip)?;
	let new_len = match &resize {
		Command::UpdateClip {
			patch: ClipPatch {
				length: Some(l), ..
			},
			..
		} => *l,
		_ => return Some(resize),
	};
	let old = c.length;
	let notes = c.notes();
	if new_len <= old || notes.is_empty() || notes.iter().any(|n| n.start >= old) {
		return Some(resize);
	}
	let content_end = notes.iter().map(|n| n.end()).max().unwrap_or(old);
	let bar = project.meter().signature_at(c.start).ticks_per_bar();
	let period = ceil_to(content_end.max(Tick(1)), bar).min(old);
	let period = if period.0 <= 0 { old } else { period };
	let first = (old.0 + period.0 - 1) / period.0;
	let mut added = Vec::new();
	let mut k = first.max(1);
	while k * period.0 < new_len.0 {
		let base = Tick(k * period.0);
		for n in notes.iter().filter(|n| n.start < period) {
			let start = base + n.start;
			if start >= old && start < new_len {
				added.push(cadenza_model::Note {
					id: NoteId::new(),
					start,
					..n.clone()
				});
			}
		}
		k += 1;
	}
	let mut lanes = Vec::new();
	for lane in c.controllers() {
		let mut points = lane.points.clone();
		let mut k = first.max(1);
		while k * period.0 < new_len.0 {
			let base = Tick(k * period.0);
			points.extend(
				lane.points
					.iter()
					.filter(|p| p.tick < period && base + p.tick >= old)
					.map(|p| cadenza_model::ControllerPoint {
						tick: base + p.tick,
						value: p.value,
					}),
			);
			k += 1;
		}
		if points.len() != lane.points.len() {
			lanes.push(Command::SetControllerLane {
				clip,
				controller: lane.controller,
				points,
			});
		}
	}
	if added.is_empty() && lanes.is_empty() {
		return Some(resize);
	}
	let mut commands = vec![resize];
	if !added.is_empty() {
		commands.push(Command::AddNotes { clip, notes: added });
	}
	commands.extend(lanes);
	batch("Resize clip (repeat)", commands)
}

#[cfg(test)]
mod tests {
	use super::*;
	use cadenza_core::PPQ;
	use cadenza_model::{Document, Note, Track, TrackKind};

	const BAR: Tick = Tick(PPQ * 4);

	fn bars(n: i64) -> Tick {
		Tick(BAR.0 * n)
	}

	/// Keys (two clips), Bass (one clip), Vox (audio, one clip).
	fn doc() -> (Document, Vec<TrackId>, Vec<ClipId>) {
		let mut doc = Document::new(Project::new("p"));
		let mut keys = Track::new("Keys", TrackKind::Instrument);
		let mut a = Clip::midi("A", Tick::ZERO, BAR);
		if let ClipContent::Midi { notes, .. } = &mut a.content {
			notes.push(Note::new(60, Tick::ZERO, Tick(PPQ), 0.8));
		}
		let b = Clip::midi("B", bars(2), BAR);
		let mut bass = Track::new("Bass", TrackKind::Instrument);
		let c = Clip::midi("C", BAR, BAR);
		let mut vox = Track::new("Vox", TrackKind::Audio);
		let d = Clip::audio("D", Tick::ZERO, BAR, "/tmp/x.wav");
		let clips = vec![a.id, b.id, c.id, d.id];
		keys.clips = vec![a, b];
		bass.clips = vec![c];
		vox.clips = vec![d];
		let tracks = vec![keys.id, bass.id, vox.id];
		for track in [keys, bass, vox] {
			doc.execute(Command::AddTrack { track, index: None })
				.unwrap();
		}
		(doc, tracks, clips)
	}

	#[test]
	fn copy_paste_keeps_relative_tracks_and_appends() {
		let (mut doc, t, c) = doc();
		let mut board = clipboard(doc.project(), &[c[0], c[2]], Tick::ZERO, false);
		assert_eq!((board.origin, board.span), (Tick::ZERO, bars(2)));
		// Playhead not moved: right after the copied span.
		let at = board.paste_at(Tick::ZERO, false);
		assert_eq!(at, bars(2));
		let (cmd, ids) = paste_clips(doc.project(), &board, at, None, false).unwrap();
		let depth = doc.history_labels().len();
		doc.execute(cmd).unwrap();
		assert_eq!(doc.history_labels().len(), depth + 1, "one step");
		let p = doc.project();
		let (tk, k) = p.find_clip(ids[0]).unwrap();
		assert_eq!((tk.id, k.start), (t[0], bars(2)));
		assert_ne!(k.notes()[0].id, p.find_clip(c[0]).unwrap().1.notes()[0].id);
		let (tb, b) = p.find_clip(ids[1]).unwrap();
		assert_eq!((tb.id, b.start), (t[1], bars(3)));
		board.pasted(at, Tick::ZERO);
		assert_eq!(board.paste_at(Tick::ZERO, false), bars(4), "appends");
		// Moved playhead wins (stopped); onto the pointer's track: Keys clip -> Bass,
		// Bass clip -> the next track down (Vox, audio) does not fit: back to Bass.
		assert_eq!(board.paste_at(bars(8), false), bars(8));
		assert_eq!(board.paste_at(bars(8), true), bars(4), "not while playing");
		let (cmd, ids) = paste_clips(doc.project(), &board, bars(8), Some(t[1]), false).unwrap();
		doc.execute(cmd).unwrap();
		for id in ids {
			assert_eq!(doc.project().find_clip(id).unwrap().0.id, t[1]);
		}
		// A cut pastes back where it was.
		let cut = clipboard(doc.project(), &[c[3]], Tick::ZERO, true);
		assert_eq!(cut.paste_at(Tick::ZERO, false), Tick::ZERO);
	}

	#[test]
	fn clone_appends_after_the_snapped_span() {
		let (mut doc, t, c) = doc();
		// One bar clip: one bar on.
		assert_eq!(
			clone_offset(doc.project(), &[c[0]], Some(Tick(PPQ))),
			Some(BAR)
		);
		// A..B = 3 bars.
		assert_eq!(
			clone_offset(doc.project(), &[c[0], c[1]], Some(Tick(PPQ))),
			Some(bars(3))
		);
		let (cmd, ids) = clone_clips(doc.project(), &[c[0], c[1]], Some(Tick(PPQ)), false).unwrap();
		doc.execute(cmd).unwrap();
		let starts: Vec<Tick> = ids
			.iter()
			.map(|i| doc.project().find_clip(*i).unwrap().1.start)
			.collect();
		assert_eq!(starts, vec![bars(3), bars(5)]);
		assert!(
			ids.iter()
				.all(|i| doc.project().find_clip(*i).unwrap().0.id == t[0])
		);
		assert_eq!(doc.history_labels().last().copied(), Some("Clone clips"));
	}

	#[test]
	fn paint_repeats_the_clip_on_free_cells() {
		let (mut doc, t, c) = doc();
		assert_eq!(
			paint_starts(Tick::ZERO, BAR, Tick(10), bars(3) + Tick(5)),
			vec![Tick::ZERO, BAR, bars(2), bars(3)]
		);
		assert_eq!(
			paint_starts(BAR, BAR, bars(2), Tick::ZERO),
			vec![Tick::ZERO, BAR, bars(2)]
		);
		// Source: pressed, else selection on the track, else last; must fit.
		let p = doc.project();
		assert_eq!(paint_source(p, t[0], None, &[c[1]], None), Some(c[1]));
		assert_eq!(paint_source(p, t[1], None, &[c[1]], Some(c[0])), Some(c[0]));
		assert_eq!(paint_source(p, t[2], Some(c[0]), &[], None), None);
		// Keys cells 0..4 bars: A (0) and B (2) are taken.
		let starts = paint_starts(Tick::ZERO, BAR, Tick::ZERO, bars(4));
		let (cmd, ids) = paint_clips(p, c[0], t[0], &starts, false).unwrap();
		doc.execute(cmd).unwrap();
		assert_eq!(ids.len(), 3, "bars 1, 3, 4");
		assert_eq!(doc.project().tracks[0].clips.len(), 5);
		assert_eq!(doc.history_labels().len(), 4, "one step");
		assert!(
			paint_clips(doc.project(), c[3], t[0], &starts, false).is_none(),
			"audio on keys"
		);
	}

	#[test]
	fn knife_cuts_every_track_it_crosses() {
		let (mut doc, t, c) = doc();
		let rows = [(t[0], 0.0, 60.0), (t[1], 60.0, 120.0), (t[2], 120.0, 180.0)];
		// Slanted from Keys down to Bass: x at each centre.
		let cuts = knife_rows(&rows, (100.0, 30.0), (200.0, 90.0));
		assert_eq!(cuts, vec![(t[0], 100.0), (t[1], 200.0)]);
		// A short vertical line inside one row.
		assert_eq!(
			knife_rows(&rows, (50.0, 130.0), (50.0, 131.0)),
			vec![(t[2], 50.0)]
		);
		let cmd = slice(
			doc.project(),
			&[
				(t[0], Tick(PPQ * 2)),
				(t[1], BAR + Tick(PPQ)),
				(t[2], Tick(PPQ)),
			],
		)
		.unwrap();
		assert!(matches!(&cmd, Command::Batch { label, .. } if label == "Slice clips"));
		doc.execute(cmd).unwrap();
		let p = doc.project();
		assert_eq!(
			p.tracks.iter().map(|t| t.clips.len()).collect::<Vec<_>>(),
			vec![3, 2, 2]
		);
		assert_eq!(p.find_clip(c[0]).unwrap().1.length, Tick(PPQ * 2));
		assert!(slice(p, &[(t[0], bars(10))]).is_none());
	}

	#[test]
	fn sweep_hits_mute_and_quantize() {
		let (mut doc, _, c) = doc();
		let rects = [
			(c[0], [0.0, 0.0, 50.0, 20.0]),
			(c[1], [100.0, 0.0, 150.0, 20.0]),
		];
		assert_eq!(
			sweep_hits(&rects, (10.0, 10.0), (120.0, 10.0)),
			vec![c[0], c[1]]
		);
		assert!(sweep_hits(&rects, (60.0, 10.0), (90.0, 10.0)).is_empty());
		let cmd = toggle_mute(doc.project(), &[c[0], c[1]]).unwrap();
		doc.execute(cmd).unwrap();
		assert!(doc.project().find_clip(c[1]).unwrap().1.muted);
		assert_eq!(doc.history_labels().last().copied(), Some("Mute clips"));
		// The first decides: unmute both, even if one were unmuted.
		let cmd = toggle_mute(doc.project(), &[c[0], c[2]]).unwrap();
		doc.execute(cmd).unwrap();
		assert!(!doc.project().find_clip(c[0]).unwrap().1.muted);
		assert!(!doc.project().find_clip(c[2]).unwrap().1.muted);
		// Quantize starts to the bar.
		doc.execute(Command::UpdateClip {
			clip: c[2],
			patch: ClipPatch {
				start: Some(BAR + Tick(100)),
				..ClipPatch::default()
			},
		})
		.unwrap();
		let snap = |t: Tick| Tick((t.0 + BAR.0 / 2).div_euclid(BAR.0) * BAR.0);
		let cmd = quantize_starts(doc.project(), &[c[0], c[2]], snap).unwrap();
		doc.execute(cmd).unwrap();
		assert_eq!(doc.project().find_clip(c[2]).unwrap().1.start, BAR);
		assert_eq!(
			track_clips(doc.project(), doc.project().tracks[0].id).len(),
			2
		);
	}

	fn notes_of(doc: &Document, clip: ClipId) -> Vec<(u8, Tick, Tick)> {
		doc.project()
			.find_clip(clip)
			.unwrap()
			.1
			.notes()
			.iter()
			.map(|n| (n.pitch, n.start, n.length))
			.collect()
	}

	#[test]
	fn linked_paste_clone_and_paint_share_content_until_made_unique() {
		let (mut doc, t, c) = doc();
		// Ctrl+B linked: A and its clone share a link; a note edit reaches both.
		let (cmd, ids) = clone_clips(doc.project(), &[c[0]], None, true).unwrap();
		doc.execute(cmd).unwrap();
		let copy = ids[0];
		assert_eq!(cadenza_model::links::link_count(doc.project(), c[0]), 2);
		doc.execute(Command::AddNotes {
			clip: copy,
			notes: vec![Note::new(64, Tick(PPQ), Tick(PPQ), 0.7)],
		})
		.unwrap();
		assert_eq!(notes_of(&doc, c[0]), notes_of(&doc, copy));
		// Paste linked joins the same group; paint too; Shift (independent) does not.
		let board = clipboard(doc.project(), &[c[0]], Tick::ZERO, false);
		let (cmd, pasted) = paste_clips(doc.project(), &board, bars(8), None, true).unwrap();
		doc.execute(cmd).unwrap();
		let (cmd, painted) = paint_clips(doc.project(), c[0], t[0], &[bars(12)], true).unwrap();
		doc.execute(cmd).unwrap();
		let (cmd, plain) = paste_clips(doc.project(), &board, bars(16), None, false).unwrap();
		doc.execute(cmd).unwrap();
		assert_eq!(cadenza_model::links::link_count(doc.project(), c[0]), 4);
		assert_eq!(cadenza_model::links::link_count(doc.project(), plain[0]), 1);
		doc.execute(Command::RemoveNotes {
			clip: painted[0],
			notes: vec![doc.project().find_clip(c[0]).unwrap().1.notes()[0].id],
		})
		.unwrap();
		assert_eq!(notes_of(&doc, pasted[0]).len(), 1);
		assert_eq!(notes_of(&doc, plain[0]).len(), 2);
		// A cut clip pasted twice: both pastes are linked to each other.
		let board = clipboard(doc.project(), &[plain[0]], Tick::ZERO, true);
		doc.execute(edits::delete_clips(&[plain[0]]).unwrap())
			.unwrap();
		let (cmd, p1) = paste_clips(doc.project(), &board, bars(20), None, true).unwrap();
		doc.execute(cmd).unwrap();
		let (cmd, p2) = paste_clips(doc.project(), &board, bars(24), None, true).unwrap();
		doc.execute(cmd).unwrap();
		assert_eq!(
			doc.project().find_clip(p1[0]).unwrap().1.link,
			doc.project().find_clip(p2[0]).unwrap().1.link
		);
		// Make unique: one step, the clip keeps its notes, the others stay linked.
		let depth = doc.history_labels().len();
		doc.execute(make_unique(doc.project(), &[copy]).unwrap())
			.unwrap();
		assert_eq!(doc.history_labels().len(), depth + 1);
		assert_eq!(cadenza_model::links::link_count(doc.project(), copy), 1);
		assert_eq!(cadenza_model::links::link_count(doc.project(), c[0]), 3);
		assert!(make_unique(doc.project(), &[copy]).is_none());
	}

	#[test]
	fn chop_splits_into_equal_parts_and_per_beat() {
		let (mut doc, _, c) = doc();
		let (cmd, parts) = chop(doc.project(), c[0], Chop::Parts(4)).unwrap();
		let depth = doc.history_labels().len();
		doc.execute(cmd).unwrap();
		assert_eq!(doc.history_labels().len(), depth + 1);
		assert_eq!(parts.len(), 4);
		let starts: Vec<Tick> = parts
			.iter()
			.map(|id| doc.project().find_clip(*id).unwrap().1.start)
			.collect();
		assert_eq!(
			starts,
			vec![Tick(0), Tick(PPQ), Tick(PPQ * 2), Tick(PPQ * 3)]
		);
		assert!(
			parts
				.iter()
				.all(|id| doc.project().find_clip(*id).unwrap().1.length == Tick(PPQ))
		);
		// The note stays in the first part.
		assert_eq!(notes_of(&doc, parts[0]).len(), 1);
		doc.undo().unwrap();
		let points = chop_points(
			doc.project(),
			doc.project().find_clip(c[0]).unwrap().1,
			Chop::Beat,
		);
		assert_eq!(points.len(), 3);
		let bars_clip = Clip::midi("Long", bars(4), bars(3));
		assert_eq!(
			chop_points(doc.project(), &bars_clip, Chop::Bar),
			vec![bars(5), bars(6)]
		);
	}

	#[test]
	fn stretch_scales_notes_and_controllers() {
		let (mut doc, _, c) = doc();
		doc.execute(Command::SetControllerLane {
			clip: c[0],
			controller: cadenza_model::Controller::Cc(1),
			points: vec![cadenza_model::ControllerPoint {
				tick: Tick(PPQ),
				value: 0.5,
			}],
		})
		.unwrap();
		doc.execute(stretch(doc.project(), c[0], bars(2)).unwrap())
			.unwrap();
		let (_, clip) = doc.project().find_clip(c[0]).unwrap();
		assert_eq!(clip.length, bars(2));
		assert_eq!(notes_of(&doc, c[0]), vec![(60, Tick(0), Tick(PPQ * 2))]);
		assert_eq!(clip.controllers()[0].points[0].tick, Tick(PPQ * 2));
		assert_eq!(doc.history_labels().last().copied(), Some("Stretch clip"));
		// Shrinking back.
		doc.execute(stretch(doc.project(), c[0], BAR).unwrap())
			.unwrap();
		assert_eq!(notes_of(&doc, c[0]), vec![(60, Tick(0), Tick(PPQ))]);
	}

	#[test]
	fn resizing_past_the_content_repeats_it() {
		let (mut doc, _, c) = doc();
		// A: one bar, a note at 0; growing to 3.5 bars repeats it at bars 1, 2 and 3.
		let cmd = resize_repeating(doc.project(), c[0], bars(3) + Tick(PPQ * 2)).unwrap();
		doc.execute(cmd).unwrap();
		let starts: Vec<Tick> = notes_of(&doc, c[0]).iter().map(|n| n.1).collect();
		assert_eq!(starts, vec![Tick(0), bars(1), bars(2), bars(3)]);
		assert_eq!(
			doc.history_labels().last().copied(),
			Some("Resize clip (repeat)")
		);
		doc.undo().unwrap();
		assert_eq!(notes_of(&doc, c[0]).len(), 1);
		// An empty clip just resizes.
		let cmd = resize_repeating(doc.project(), c[1], bars(5)).unwrap();
		assert!(matches!(cmd, Command::UpdateClip { .. }));
	}
}
