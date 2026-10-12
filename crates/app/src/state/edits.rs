//! Pure builders turning UI gestures into model [`Command`]s. Nothing here mutates the
//! project: the result goes through [`super::AppState::apply`] like every other edit (and
//! like the AI's edits), so it is validated, undoable and reloaded into the player.

use cadenza_core::{ClipId, NoteId, PPQ, Tick, TrackId};
use cadenza_model::{
	Clip, ClipContent, ClipPatch, Command, Device, Note, NotePatch, Project, Track, TrackKind,
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
			if let ClipContent::Midi { notes } = &mut copy.content {
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
/// the right file offset and keeps its fade-out on the right part.
/// Returns the command and the id of the new (right) clip.
pub fn split_clip(project: &Project, clip: ClipId, at: Tick) -> Option<(Command, ClipId)> {
	let (track, c) = project.find_clip(clip)?;
	if at <= c.start + MIN_CLIP_LENGTH || at >= c.end() - MIN_CLIP_LENGTH {
		return None;
	}
	let left_len = at - c.start;
	let mut right = Clip {
		id: ClipId::new(),
		name: c.name.clone(),
		start: at,
		length: c.end() - at,
		content: c.content.clone(),
	};
	// Note and fade edits first, so the left clip never holds notes past its new end.
	let mut commands = Vec::new();
	match (&c.content, &mut right.content) {
		(ClipContent::Midi { notes }, ClipContent::Midi { notes: right_notes }) => {
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
	let id = right.id;
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
	Some((batch("Split clip", commands)?, id))
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
fn batch(label: &str, mut commands: Vec<Command>) -> Option<Command> {
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
	use cadenza_model::{ClipFade, Document, FadeCurve};

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
