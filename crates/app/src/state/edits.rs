//! Pure builders turning UI gestures into model [`Command`]s. Nothing here mutates the
//! project: the result goes through [`super::AppState::apply`] like every other edit (and
//! like the AI's edits), so it is validated, undoable and reloaded into the player.

use cadenza_core::{ClipId, DeviceId, NoteId, PPQ, Tick, TrackId};
use cadenza_model::{
	Clip, ClipContent, ClipPatch, Command, Device, NotePatch, Project, Track, TrackKind,
};

/// Default colors for new tracks (cycled).
pub const TRACK_PALETTE: [&str; 10] = [
	"#E0603A", "#E8A33A", "#D8CF4A", "#7CC655", "#3FB98E", "#3BB3D0", "#4F86E8", "#8A6CE0",
	"#C65CC9", "#E05C8C",
];

/// Shortest clip a resize can produce.
pub const MIN_CLIP_LENGTH: Tick = Tick(PPQ / 16);

pub fn kind_name(kind: TrackKind) -> &'static str {
	match kind {
		TrackKind::Instrument => "Instrument",
		TrackKind::Audio => "Audio",
		TrackKind::Bus => "Bus",
	}
}

/// A new track named after its kind and numbered, with the next palette color.
/// Instrument tracks get the built-in poly synth so they sound right away.
pub fn new_track(project: &Project, kind: TrackKind) -> Track {
	let same = project.tracks.iter().filter(|t| t.kind == kind).count();
	let color = TRACK_PALETTE[project.tracks.len() % TRACK_PALETTE.len()];
	let mut track = Track::new(format!("{} {}", kind_name(kind), same + 1), kind).with_color(color);
	if kind == TrackKind::Instrument {
		track.instrument = Some(Device::builtin("poly_synth", "Poly Synth"));
	}
	track
}

/// Inserts `track` below `after` (or at the end).
pub fn add_track(project: &Project, track: Track, after: Option<TrackId>) -> Command {
	let index = after.and_then(|id| project.track_index(id)).map(|i| i + 1);
	Command::AddTrack { track, index }
}

/// Moves a track by `delta` positions (clamped); `None` when it would not move.
pub fn move_track(project: &Project, track: TrackId, delta: i64) -> Option<Command> {
	let from = project.track_index(track)? as i64;
	let to = (from + delta).clamp(0, project.tracks.len() as i64 - 1);
	(to != from).then_some(Command::MoveTrack {
		track,
		index: to as usize,
	})
}

/// Whether clips of `from` can live on `to`.
pub fn compatible(from: TrackKind, to: TrackKind) -> bool {
	from == to && from != TrackKind::Bus
}

/// Moves `clips` by `delta` (the earliest start clamps at 0) and, when `to_track` is
/// given, onto that track (only clips whose kind fits it move there).
pub fn move_clips(
	project: &Project,
	clips: &[ClipId],
	delta: Tick,
	to_track: Option<TrackId>,
) -> Option<Command> {
	let found: Vec<(&Track, &Clip)> = clips.iter().filter_map(|c| project.find_clip(*c)).collect();
	let earliest = found.iter().map(|(_, c)| c.start).min()?;
	let delta = Tick(delta.0.max(-earliest.0));
	let target = to_track.and_then(|t| project.track(t));
	let commands: Vec<Command> = found
		.iter()
		.filter_map(|(track, clip)| {
			let track_change = target
				.filter(|t| t.id != track.id && compatible(track.kind, t.kind))
				.map(|t| t.id);
			(delta.0 != 0 || track_change.is_some()).then(|| Command::UpdateClip {
				clip: clip.id,
				patch: ClipPatch {
					start: Some(clip.start + delta),
					track: track_change,
					..ClipPatch::default()
				},
			})
		})
		.collect();
	batch(
		if commands.len() == 1 {
			"Move clip"
		} else {
			"Move clips"
		},
		commands,
	)
}

/// Copies `clips` shifted by `delta` (the earliest copy clamps at 0) and, when
/// `to_track` is given, onto that track (Ctrl+drag). Returns the command and the ids of
/// the copies.
pub fn copy_clips(
	project: &Project,
	clips: &[ClipId],
	delta: Tick,
	to_track: Option<TrackId>,
) -> Option<(Command, Vec<ClipId>)> {
	let found: Vec<(&Track, &Clip)> = clips.iter().filter_map(|c| project.find_clip(*c)).collect();
	let earliest = found.iter().map(|(_, c)| c.start).min()?;
	let delta = Tick(delta.0.max(-earliest.0));
	let target = to_track.and_then(|t| project.track(t));
	let mut ids = Vec::new();
	let commands = found
		.iter()
		.map(|(track, clip)| {
			let mut copy = (*clip).clone();
			copy.id = ClipId::new();
			copy.start = clip.start + delta;
			if let ClipContent::Midi { notes, .. } = &mut copy.content {
				for n in notes {
					n.id = NoteId::new();
				}
			}
			ids.push(copy.id);
			let track = target
				.filter(|t| compatible(track.kind, t.kind))
				.map_or(track.id, |t| t.id);
			Command::AddClip { track, clip: copy }
		})
		.collect();
	Some((
		batch(
			if ids.len() == 1 {
				"Copy clip"
			} else {
				"Copy clips"
			},
			commands,
		)?,
		ids,
	))
}

/// Drops the insert `device` of channel `from` (`None` = master) into slot `slot`
/// (0..=len, before the insert currently there) of channel `to`: reorders within a
/// chain, moves to another chain, or with `copy` inserts a copy with a new id.
pub fn drop_insert(
	project: &Project,
	from: Option<TrackId>,
	device: DeviceId,
	to: Option<TrackId>,
	slot: usize,
	copy: bool,
) -> Option<Command> {
	let source = project.channel(from)?;
	let index = source.inserts.iter().position(|d| d.id == device)?;
	let d = source.inserts[index].clone();
	let target = project.channel(to)?;
	let slot = slot.min(target.inserts.len());
	if copy {
		let mut d = d;
		d.id = DeviceId::new();
		return Some(Command::InsertDevice {
			track: to,
			device: d,
			index: Some(slot),
		});
	}
	if from == to {
		let final_index = if slot > index { slot - 1 } else { slot };
		return (final_index != index).then_some(Command::MoveDevice {
			track: to,
			device,
			index: final_index,
		});
	}
	batch(
		"Move device",
		vec![
			Command::RemoveDevice {
				track: from,
				device,
			},
			Command::InsertDevice {
				track: to,
				device: d,
				index: Some(slot),
			},
		],
	)
}

/// Sets the end of `clip`: at least [`MIN_CLIP_LENGTH`] long and past its last note start.
pub fn resize_clip(project: &Project, clip: ClipId, end: Tick) -> Option<Command> {
	let (_, c) = project.find_clip(clip)?;
	// The model keeps every note start inside its clip.
	let last_note = c.notes().iter().map(|n| n.start.0).max().unwrap_or(0);
	let min = MIN_CLIP_LENGTH.0 + last_note;
	let length = Tick((end - c.start).0.max(min));
	(length != c.length).then_some(Command::UpdateClip {
		clip,
		patch: ClipPatch {
			length: Some(length),
			..ClipPatch::default()
		},
	})
}

pub fn delete_clips(clips: &[ClipId]) -> Option<Command> {
	batch(
		if clips.len() == 1 {
			"Delete clip"
		} else {
			"Delete clips"
		},
		clips
			.iter()
			.map(|&clip| Command::RemoveClip { clip })
			.collect(),
	)
}

/// Copies `clips` (fresh clip and note ids) right after the end of the selection, on
/// their own tracks. Returns the command and the new clip ids (to select them).
pub fn duplicate_clips(project: &Project, clips: &[ClipId]) -> Option<(Command, Vec<ClipId>)> {
	let found: Vec<(&Track, &Clip)> = clips.iter().filter_map(|c| project.find_clip(*c)).collect();
	let start = found.iter().map(|(_, c)| c.start).min()?;
	let end = found.iter().map(|(_, c)| c.end()).max()?;
	let offset = end - start;
	let mut ids = Vec::new();
	let commands = found
		.iter()
		.map(|(track, clip)| {
			let mut copy = (*clip).clone();
			copy.id = ClipId::new();
			copy.start = clip.start + offset;
			if let ClipContent::Midi { notes, .. } = &mut copy.content {
				for n in notes {
					n.id = NoteId::new();
				}
			}
			ids.push(copy.id);
			Command::AddClip {
				track: track.id,
				clip: copy,
			}
		})
		.collect();
	Some((batch("Duplicate", commands)?, ids))
}

/// Splits `clip` at `at` (strictly inside it) into two clips. MIDI notes starting after
/// the split move to the new clip, notes crossing it are shortened; audio continues with
/// the right file offset and keeps its fade-out on the right part. A take is split with
/// its whole take group (split comping: each part then picks its own take).
/// Returns the command and the id of the new (right) clip.
pub fn split_clip(project: &Project, clip: ClipId, at: Tick) -> Option<(Command, ClipId)> {
	let (_, c) = project.find_clip(clip)?;
	if at <= c.start + MIN_CLIP_LENGTH || at >= c.end() - MIN_CLIP_LENGTH {
		return None;
	}
	if let Some(take) = c.take {
		let group = cadenza_core::TakeGroupId::new();
		let commands = cadenza_model::takes::split_group(project, take.group, at, group)?;
		let right = commands.iter().find_map(|cmd| match cmd {
			Command::AddClip { clip: added, .. }
				if added.take.is_some_and(|t| t.lane == take.lane) && added.start == at =>
			{
				Some(added.id)
			}
			_ => None,
		})?;
		return Some((batch("Split takes", commands)?, right));
	}
	let id = ClipId::new();
	let commands = cadenza_model::takes::split_clip(project, clip, at, id, None)?;
	Some((batch("Split clip", commands)?, id))
}

/// Moves a clip's start edge to `start` (left trim/extend) keeping its content where it
/// is on the timeline: MIDI notes shift inside the clip (notes cut off at the left are
/// removed or shortened), an audio clip's `offset_seconds` follows (never below 0, which
/// limits how far it extends). The end stays put.
pub fn trim_start(project: &Project, clip: ClipId, start: Tick) -> Option<Command> {
	let (_, c) = project.find_clip(clip)?;
	let end = c.end();
	let mut start = Tick(start.0.clamp(0, end.0 - MIN_CLIP_LENGTH.0));
	let mut commands = Vec::new();
	match &c.content {
		ClipContent::Midi { notes, .. } => {
			let delta = start - c.start;
			if delta.0 == 0 {
				return None;
			}
			let resize = Command::UpdateClip {
				clip,
				patch: ClipPatch {
					start: Some(start),
					length: Some(end - start),
					..ClipPatch::default()
				},
			};
			let gone: Vec<NoteId> = notes
				.iter()
				.filter(|n| n.end() <= delta)
				.map(|n| n.id)
				.collect();
			let moved: Vec<NotePatch> = notes
				.iter()
				.filter(|n| !gone.contains(&n.id))
				.map(|n| {
					let new_start = n.start - delta;
					let (s, len) = if new_start.0 < 0 {
						(Tick::ZERO, n.length + new_start)
					} else {
						(new_start, n.length)
					};
					NotePatch {
						id: n.id,
						pitch: None,
						velocity: None,
						start: Some(s),
						length: (len != n.length).then_some(len),
					}
				})
				.collect();
			if delta.0 < 0 {
				// Growing: make room first, then move the notes later.
				commands.push(resize.clone());
			}
			if !gone.is_empty() {
				commands.push(Command::RemoveNotes { clip, notes: gone });
			}
			if !moved.is_empty() {
				commands.push(Command::UpdateNotes { clip, notes: moved });
			}
			if delta.0 > 0 {
				commands.push(resize);
			}
		}
		ClipContent::Audio {
			offset_seconds,
			fade_in,
			fade_out,
			..
		} => {
			let tempo = &project.tempo;
			let old_s = tempo.tick_to_seconds(c.start);
			let mut offset = offset_seconds + tempo.tick_to_seconds(start) - old_s;
			if offset < 0.0 {
				// Cannot reveal audio before the file start.
				start = tempo
					.seconds_to_tick(old_s - offset_seconds)
					.max(Tick::ZERO);
				offset = (offset_seconds + tempo.tick_to_seconds(start) - old_s).max(0.0);
			}
			if start == c.start {
				return None;
			}
			let duration = tempo.tick_to_seconds(end) - tempo.tick_to_seconds(start);
			let fin = fade_in.filter(|f| f.seconds < duration);
			let fout = fade_out.filter(|f| f.seconds + fin.map_or(0.0, |f| f.seconds) < duration);
			if fin != *fade_in || fout != *fade_out {
				commands.push(Command::SetClipFades {
					clip,
					fade_in: fin,
					fade_out: fout,
				});
			}
			commands.push(Command::UpdateAudioClip {
				clip,
				patch: cadenza_model::AudioClipPatch {
					offset_seconds: Some(offset),
					..Default::default()
				},
			});
			commands.push(Command::UpdateClip {
				clip,
				patch: ClipPatch {
					start: Some(start),
					length: Some(end - start),
					..ClipPatch::default()
				},
			});
		}
	}
	batch("Trim clip start", commands)
}

/// Clips of `tracks` overlapping the time range `t0..t1` (rubber band selection).
pub fn clips_in(project: &Project, tracks: &[TrackId], t0: Tick, t1: Tick) -> Vec<ClipId> {
	let (t0, t1) = (t0.min(t1), t0.max(t1));
	project
		.tracks
		.iter()
		.filter(|t| tracks.contains(&t.id))
		.flat_map(|t| t.clips.iter())
		// Takes that do not play are not on screen (they live in the take lanes).
		.filter(|c| c.plays() && c.start < t1 && c.end() > t0)
		.map(|c| c.id)
		.collect()
}

/// Time ranges where audio clips of one track overlap (they sum; drawn as crossfades).
pub fn overlaps(track: &Track) -> Vec<(Tick, Tick)> {
	// Takes that do not play never sum.
	let audio: Vec<&Clip> = track
		.clips
		.iter()
		.filter(|c| !c.is_midi() && c.plays())
		.collect();
	let mut out = Vec::new();
	for (i, a) in audio.iter().enumerate() {
		for b in &audio[i + 1..] {
			let (s, e) = (a.start.max(b.start), a.end().min(b.end()));
			if s < e {
				out.push((s, e));
			}
		}
	}
	out
}

/// An automatic crossfade between two overlapping audio clips (the engine's rule: the
/// later clip starts inside the earlier one and ends after it).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Crossfade {
	pub earlier: ClipId,
	pub later: ClipId,
	/// Where the crossfade starts (the later clip's start) and ends.
	pub start: Tick,
	pub end: Tick,
	/// End of the overlap (the longest the crossfade can be).
	pub overlap_end: Tick,
	pub curve: cadenza_model::FadeCurve,
	/// The length comes from the later clip's fade-in (else the whole overlap).
	pub custom: bool,
}

/// The crossfades of a track's playing audio clips.
pub fn crossfades(project: &Project, track: &Track) -> Vec<Crossfade> {
	let mut audio: Vec<&Clip> = track
		.clips
		.iter()
		.filter(|c| !c.is_midi() && c.plays())
		.collect();
	audio.sort_by_key(|c| c.start);
	let tempo = &project.tempo;
	let mut out = Vec::new();
	for (i, b) in audio.iter().enumerate() {
		let Some(a) = audio[..i]
			.iter()
			.filter(|a| a.start < b.start && b.start < a.end() && a.end() < b.end())
			.max_by_key(|a| a.end())
		else {
			continue;
		};
		let overlap_end = a.end();
		let (fade_in, _) = b.fades();
		let (end, curve, custom) = match fade_in {
			Some(f) => {
				let secs = tempo.tick_to_seconds(b.start) + f.seconds;
				(tempo.seconds_to_tick(secs).min(overlap_end), f.curve, true)
			}
			None => (overlap_end, cadenza_model::FadeCurve::EqualPower, false),
		};
		out.push(Crossfade {
			earlier: a.id,
			later: b.id,
			start: b.start,
			end,
			overlap_end,
			curve,
			custom,
		});
	}
	out
}

/// Sets a crossfade to end at `end` (clamped to the overlap) through the later clip's
/// fade-in; `None` resets it to the whole overlap.
pub fn set_crossfade(project: &Project, xf: &Crossfade, end: Option<Tick>) -> Option<Command> {
	let (_, later) = project.find_clip(xf.later)?;
	let (fade_in, fade_out) = later.fades();
	let tempo = &project.tempo;
	let fade_in_new = end.map(|end| {
		let end = end.clamp(xf.start + Tick(1), xf.overlap_end);
		let secs = tempo.tick_to_seconds(end) - tempo.tick_to_seconds(xf.start);
		cadenza_model::ClipFade::new(secs.max(0.0005), xf.curve)
	});
	(fade_in_new != fade_in).then_some(Command::SetClipFades {
		clip: xf.later,
		fade_in: fade_in_new,
		fade_out,
	})
}

/// An empty one-bar (at least) MIDI clip on an instrument track.
pub fn new_midi_clip(
	project: &Project,
	track: TrackId,
	start: Tick,
	length: Tick,
) -> Option<(Command, ClipId)> {
	let t = project.track(track)?;
	if t.kind != TrackKind::Instrument {
		return None;
	}
	let clip = Clip::midi(format!("{} clip", t.name), start, length);
	let id = clip.id;
	Some((Command::AddClip { track, clip }, id))
}

/// One command, or a labelled batch of several; `None` for nothing to do.
pub(crate) fn batch(label: &str, mut commands: Vec<Command>) -> Option<Command> {
	match commands.len() {
		0 => None,
		1 => commands.pop(),
		_ => Some(Command::Batch {
			label: label.into(),
			commands,
		}),
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use cadenza_model::{ClipFade, Document, FadeCurve, Note};

	fn beats(b: f64) -> Tick {
		Tick::from_beats(b)
	}

	struct Fx {
		doc: Document,
		keys: TrackId,
		keys2: TrackId,
		audio: TrackId,
		riff: ClipId,
		vox: ClipId,
	}

	fn fixture() -> Fx {
		let mut p = Project::new("t");
		let mut keys = new_track(&p, TrackKind::Instrument);
		let riff = Clip::midi("riff", beats(4.0), beats(4.0)).with_notes([
			Note::new(60, beats(0.0), beats(1.0), 0.8),
			Note::new(64, beats(1.5), beats(1.0), 0.8),
			Note::new(67, beats(3.0), beats(1.0), 0.8),
		]);
		keys.clips.push(riff.clone());
		p.tracks.push(keys.clone());
		let keys2 = new_track(&p, TrackKind::Instrument);
		p.tracks.push(keys2.clone());
		let mut audio = new_track(&p, TrackKind::Audio);
		let mut vox = Clip::audio("vox", beats(0.0), beats(8.0), "/tmp/vox.wav");
		if let ClipContent::Audio {
			fade_in, fade_out, ..
		} = &mut vox.content
		{
			*fade_in = Some(ClipFade::new(0.1, FadeCurve::Linear));
			*fade_out = Some(ClipFade::new(0.5, FadeCurve::Linear));
		}
		audio.clips.push(vox.clone());
		p.tracks.push(audio.clone());
		Fx {
			doc: Document::new(p),
			keys: keys.id,
			keys2: keys2.id,
			audio: audio.id,
			riff: riff.id,
			vox: vox.id,
		}
	}

	fn clip(doc: &Document, id: ClipId) -> (TrackId, Clip) {
		let (t, c) = doc.project().find_clip(id).unwrap();
		(t.id, c.clone())
	}

	#[test]
	fn new_tracks_are_named_colored_and_playable() {
		let fx = fixture();
		let p = fx.doc.project();
		let t = new_track(p, TrackKind::Instrument);
		assert_eq!(t.name, "Instrument 3");
		assert!(t.instrument.is_some());
		assert_eq!(t.color.as_deref(), Some(TRACK_PALETTE[3]));
		let bus = new_track(p, TrackKind::Bus);
		assert_eq!(bus.name, "Bus 1");
		assert!(bus.instrument.is_none());
		match add_track(p, bus, Some(fx.keys)) {
			Command::AddTrack { index, .. } => assert_eq!(index, Some(1)),
			other => panic!("{other:?}"),
		}
	}

	#[test]
	fn move_track_clamps() {
		let mut fx = fixture();
		let p = fx.doc.project();
		assert!(move_track(p, fx.keys, -1).is_none());
		let cmd = move_track(p, fx.keys, 10).unwrap();
		assert_eq!(
			cmd,
			Command::MoveTrack {
				track: fx.keys,
				index: 2
			}
		);
		fx.doc.execute(cmd).unwrap();
		assert_eq!(fx.doc.project().tracks[2].id, fx.keys);
	}

	#[test]
	fn moving_clips_clamps_at_zero_and_respects_track_kinds() {
		let mut fx = fixture();
		let p = fx.doc.project();
		// Earliest clip (vox at 0) cannot go earlier, so nothing moves.
		assert!(move_clips(p, &[fx.riff, fx.vox], beats(-2.0), None).is_none());
		let cmd = move_clips(p, &[fx.riff], beats(-8.0), Some(fx.keys2)).unwrap();
		fx.doc.execute(cmd).unwrap();
		let (track, c) = clip(&fx.doc, fx.riff);
		assert_eq!((track, c.start), (fx.keys2, Tick::ZERO));
		// MIDI cannot go to an audio track: only the time changes.
		let cmd = move_clips(fx.doc.project(), &[fx.riff], beats(2.0), Some(fx.audio)).unwrap();
		fx.doc.execute(cmd).unwrap();
		assert_eq!(clip(&fx.doc, fx.riff), (fx.keys2, clip(&fx.doc, fx.riff).1));
		assert_eq!(clip(&fx.doc, fx.riff).1.start, beats(2.0));
		// A group move is one undo step.
		let cmd = move_clips(fx.doc.project(), &[fx.riff, fx.vox], beats(1.0), None).unwrap();
		assert!(matches!(&cmd, Command::Batch { label, .. } if label == "Move clips"));
		fx.doc.execute(cmd).unwrap();
		fx.doc.undo().unwrap();
		assert_eq!(clip(&fx.doc, fx.vox).1.start, Tick::ZERO);
	}

	#[test]
	fn resize_has_a_minimum_length() {
		let mut fx = fixture();
		let p = fx.doc.project();
		assert!(resize_clip(p, fx.riff, beats(8.0)).is_none(), "unchanged");
		fx.doc
			.execute(resize_clip(p, fx.riff, beats(2.0)).unwrap())
			.unwrap();
		assert_eq!(
			clip(&fx.doc, fx.riff).1.length,
			beats(3.0) + MIN_CLIP_LENGTH,
			"stops after the last note start"
		);
		let (cmd, id) = new_midi_clip(fx.doc.project(), fx.keys2, Tick::ZERO, beats(4.0)).unwrap();
		fx.doc.execute(cmd).unwrap();
		fx.doc
			.execute(resize_clip(fx.doc.project(), id, Tick(-50)).unwrap())
			.unwrap();
		assert_eq!(clip(&fx.doc, id).1.length, MIN_CLIP_LENGTH);
	}

	#[test]
	fn delete_and_duplicate() {
		let mut fx = fixture();
		let (cmd, ids) = duplicate_clips(fx.doc.project(), &[fx.riff]).unwrap();
		fx.doc.execute(cmd).unwrap();
		let (track, copy) = clip(&fx.doc, ids[0]);
		let orig = clip(&fx.doc, fx.riff).1;
		assert_eq!(track, fx.keys);
		assert_eq!(copy.start, orig.end());
		assert_eq!(copy.notes().len(), 3);
		assert!(
			copy.notes()
				.iter()
				.all(|n| orig.notes().iter().all(|o| o.id != n.id))
		);
		fx.doc
			.execute(delete_clips(&[fx.riff, ids[0]]).unwrap())
			.unwrap();
		assert!(fx.doc.project().tracks[0].clips.is_empty());
		fx.doc.undo().unwrap();
		assert_eq!(fx.doc.project().tracks[0].clips.len(), 2);
	}

	#[test]
	fn ctrl_drag_copies_clips() {
		let mut fx = fixture();
		let (cmd, ids) =
			copy_clips(fx.doc.project(), &[fx.riff], beats(4.0), Some(fx.keys2)).unwrap();
		fx.doc.execute(cmd).unwrap();
		let (track, copy) = clip(&fx.doc, ids[0]);
		assert_eq!((track, copy.start), (fx.keys2, beats(8.0)));
		assert_eq!(clip(&fx.doc, fx.riff).0, fx.keys, "original stays");
		// Incompatible target: copied on its own track.
		let (cmd, ids) =
			copy_clips(fx.doc.project(), &[fx.riff], beats(-40.0), Some(fx.audio)).unwrap();
		fx.doc.execute(cmd).unwrap();
		assert_eq!(clip(&fx.doc, ids[0]), (fx.keys, clip(&fx.doc, ids[0]).1));
		assert_eq!(clip(&fx.doc, ids[0]).1.start, Tick::ZERO);
	}

	#[test]
	fn inserts_reorder_move_and_copy_between_strips() {
		let mut fx = fixture();
		let (a, b, c) = (
			Device::builtin("eq", "EQ"),
			Device::builtin("compressor", "Comp"),
			Device::builtin("reverb", "Verb"),
		);
		for d in [&a, &b, &c] {
			fx.doc
				.execute(Command::InsertDevice {
					track: Some(fx.keys),
					device: d.clone(),
					index: None,
				})
				.unwrap();
		}
		let names = |doc: &Document, t: Option<TrackId>| -> Vec<String> {
			doc.project()
				.channel(t)
				.unwrap()
				.inserts
				.iter()
				.map(|d| d.name.clone())
				.collect()
		};
		let k = Some(fx.keys);
		// Dropping into its own slot (or the one after) does nothing.
		assert!(drop_insert(fx.doc.project(), k, a.id, k, 0, false).is_none());
		assert!(drop_insert(fx.doc.project(), k, a.id, k, 1, false).is_none());
		let cmd = drop_insert(fx.doc.project(), k, a.id, k, 3, false).unwrap();
		fx.doc.execute(cmd).unwrap();
		assert_eq!(names(&fx.doc, k), ["Comp", "Verb", "EQ"]);
		let cmd = drop_insert(fx.doc.project(), k, a.id, k, 0, false).unwrap();
		fx.doc.execute(cmd).unwrap();
		assert_eq!(names(&fx.doc, k), ["EQ", "Comp", "Verb"]);
		// Move to the master (one undo step), copy to another track.
		let cmd = drop_insert(fx.doc.project(), k, c.id, None, 0, false).unwrap();
		fx.doc.execute(cmd).unwrap();
		assert_eq!(names(&fx.doc, k), ["EQ", "Comp"]);
		assert_eq!(names(&fx.doc, None), ["Verb"]);
		let cmd = drop_insert(fx.doc.project(), k, b.id, Some(fx.keys2), 5, true).unwrap();
		fx.doc.execute(cmd).unwrap();
		assert_eq!(names(&fx.doc, k), ["EQ", "Comp"]);
		assert_eq!(names(&fx.doc, Some(fx.keys2)), ["Comp"]);
		let copied = fx.doc.project().channel(Some(fx.keys2)).unwrap().inserts[0].id;
		assert_ne!(copied, b.id, "a copy gets a new id");
		fx.doc.undo().unwrap();
		fx.doc.undo().unwrap();
		assert_eq!(names(&fx.doc, k), ["EQ", "Comp", "Verb"]);
	}

	#[test]
	fn split_midi_moves_and_shortens_notes() {
		let mut fx = fixture();
		let p = fx.doc.project();
		assert!(split_clip(p, fx.riff, beats(4.0)).is_none(), "at the edge");
		let (cmd, right) = split_clip(p, fx.riff, beats(6.0)).unwrap();
		fx.doc.execute(cmd).unwrap();
		let left = clip(&fx.doc, fx.riff).1;
		let right = clip(&fx.doc, right).1;
		assert_eq!((left.start, left.length), (beats(4.0), beats(2.0)));
		assert_eq!((right.start, right.length), (beats(6.0), beats(2.0)));
		let lengths: Vec<_> = left.notes().iter().map(|n| n.length).collect();
		assert_eq!(lengths, vec![beats(1.0), beats(0.5)], "crossing note cut");
		assert_eq!(right.notes().len(), 1);
		assert_eq!(right.notes()[0].start, beats(1.0));
		fx.doc.undo().unwrap();
		assert_eq!(clip(&fx.doc, fx.riff).1.notes().len(), 3);
	}

	#[test]
	fn split_audio_offsets_the_right_part() {
		let mut fx = fixture();
		// 120 bpm: beat 2 = 1 s.
		let (cmd, right) = split_clip(fx.doc.project(), fx.vox, beats(2.0)).unwrap();
		fx.doc.execute(cmd).unwrap();
		let left = clip(&fx.doc, fx.vox).1;
		let right = clip(&fx.doc, right).1;
		assert_eq!(left.fades().1, None, "fade-out moved right");
		assert!(left.fades().0.is_some(), "fade-in kept");
		match right.content {
			ClipContent::Audio {
				offset_seconds,
				fade_in,
				fade_out,
				..
			} => {
				assert!((offset_seconds - 1.0).abs() < 1e-9);
				assert!(fade_in.is_none() && fade_out.is_some());
			}
			_ => panic!("audio expected"),
		}
	}

	#[test]
	fn new_clips_only_on_instrument_tracks() {
		let mut fx = fixture();
		let p = fx.doc.project();
		assert!(new_midi_clip(p, fx.audio, Tick::ZERO, beats(4.0)).is_none());
		let (cmd, id) = new_midi_clip(p, fx.keys2, beats(8.0), beats(4.0)).unwrap();
		fx.doc.execute(cmd).unwrap();
		assert_eq!(clip(&fx.doc, id).0, fx.keys2);
	}
}

#[cfg(test)]
mod trim_tests {
	use super::*;
	use cadenza_model::{Document, Note};

	fn beats(b: f64) -> Tick {
		Tick::from_beats(b)
	}

	fn exec(p: &Project, cmd: Command) -> Project {
		let mut doc = Document::new(p.clone());
		doc.execute(cmd).unwrap();
		doc.into_project()
	}

	#[test]
	fn trimming_the_start_keeps_midi_notes_in_place() {
		let mut p = Project::new("t");
		let mut t = Track::new("Keys", TrackKind::Instrument);
		let clip = Clip::midi("A", beats(4.0), beats(8.0)).with_notes([
			Note::new(60, beats(0.0), beats(1.0), 0.8),
			Note::new(62, beats(1.5), beats(1.0), 0.8),
			Note::new(64, beats(4.0), beats(1.0), 0.8),
		]);
		let id = clip.id;
		t.clips.push(clip);
		p.tracks.push(t);
		// Trim 2 beats: note 1 is gone, note 2 is cut to its tail, note 3 moves.
		let trimmed = exec(&p, trim_start(&p, id, beats(6.0)).unwrap());
		let (_, c) = trimmed.find_clip(id).unwrap();
		assert_eq!((c.start, c.length), (beats(6.0), beats(6.0)));
		let notes = c.notes();
		assert_eq!(notes.len(), 2);
		assert_eq!((notes[0].start, notes[0].length), (Tick::ZERO, beats(0.5)));
		assert_eq!(
			notes[1].start,
			beats(2.0),
			"absolute position kept (beat 8)"
		);
		// Extending back left moves them later again.
		let grown = exec(&trimmed, trim_start(&trimmed, id, beats(2.0)).unwrap());
		let (_, c) = grown.find_clip(id).unwrap();
		assert_eq!((c.start, c.length), (beats(2.0), beats(10.0)));
		assert_eq!(c.notes()[1].start, beats(6.0));
		assert!(trim_start(&grown, id, beats(2.0)).is_none());
	}

	#[test]
	fn rubber_band_and_overlaps() {
		let mut p = Project::new("t");
		let mut a = Track::new("A", TrackKind::Audio);
		let c1 = Clip::audio("1", beats(0.0), beats(4.0), "a.wav");
		let c2 = Clip::audio("2", beats(3.0), beats(4.0), "b.wav");
		let c3 = Clip::audio("3", beats(10.0), beats(2.0), "c.wav");
		let ids = [c1.id, c2.id, c3.id];
		a.clips = vec![c1, c2, c3];
		let b = Track::new("B", TrackKind::Audio);
		let (ta, tb) = (a.id, b.id);
		p.tracks = vec![a, b];
		assert_eq!(
			clips_in(&p, &[ta, tb], beats(5.0), beats(2.0)),
			vec![ids[0], ids[1]]
		);
		assert!(clips_in(&p, &[tb], beats(0.0), beats(20.0)).is_empty());
		assert_eq!(overlaps(&p.tracks[0]), vec![(beats(3.0), beats(4.0))]);
	}

	#[test]
	fn trimming_an_audio_clip_moves_its_offset() {
		let mut p = Project::new("t");
		let mut t = Track::new("Vox", TrackKind::Audio);
		let clip = Clip::audio("V", beats(4.0), beats(8.0), "v.wav");
		let id = clip.id;
		t.clips.push(clip);
		p.tracks.push(t);
		// 120 bpm: 2 beats = 1 s.
		let trimmed = exec(&p, trim_start(&p, id, beats(6.0)).unwrap());
		let (_, c) = trimmed.find_clip(id).unwrap();
		assert_eq!((c.start, c.end()), (beats(6.0), beats(12.0)));
		let ClipContent::Audio { offset_seconds, .. } = c.content else {
			panic!()
		};
		assert!((offset_seconds - 1.0).abs() < 1e-9);
		// Extending further left than the file start stops at offset 0.
		let back = exec(&trimmed, trim_start(&trimmed, id, beats(0.0)).unwrap());
		let (_, c) = back.find_clip(id).unwrap();
		assert_eq!(c.start, beats(4.0));
		let ClipContent::Audio { offset_seconds, .. } = c.content else {
			panic!()
		};
		assert!(offset_seconds.abs() < 1e-9);
	}

	#[test]
	fn crossfades_follow_overlaps_and_the_later_fade_in() {
		let mut p = Project::new("x");
		let mut t = Track::new("Vox", TrackKind::Audio);
		let a = Clip::audio("a", beats(0.0), beats(4.0), "/a.wav");
		let b = Clip::audio("b", beats(3.0), beats(4.0), "/b.wav");
		let inner = Clip::audio("i", beats(4.5), beats(1.0), "/i.wav");
		let (a_id, b_id) = (a.id, b.id);
		t.clips = vec![a, b, inner];
		p.tracks.push(t);
		let xfs = crossfades(&p, &p.tracks[0]);
		assert_eq!(xfs.len(), 1, "the nested clip sums");
		let xf = xfs[0];
		assert_eq!((xf.earlier, xf.later), (a_id, b_id));
		assert_eq!((xf.start, xf.end), (beats(3.0), beats(4.0)));
		assert!(!xf.custom);
		// Shorten to half a beat through the later clip's fade-in (one command).
		let cmd = set_crossfade(&p, &xf, Some(beats(3.5))).unwrap();
		let p2 = exec(&p, cmd);
		let xf2 = crossfades(&p2, &p2.tracks[0])[0];
		assert_eq!(xf2.end, beats(3.5));
		assert!(xf2.custom);
		// Past the overlap is clamped; None resets.
		let cmd = set_crossfade(&p2, &xf2, Some(beats(9.0))).unwrap();
		let p3 = exec(&p2, cmd);
		assert_eq!(crossfades(&p3, &p3.tracks[0])[0].end, beats(4.0));
		let reset = set_crossfade(&p2, &xf2, None).unwrap();
		let p4 = exec(&p2, reset);
		assert!(!crossfades(&p4, &p4.tracks[0])[0].custom);
	}
}
