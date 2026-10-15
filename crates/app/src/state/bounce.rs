//! Freeze, bounce in place and consolidate: renders on a worker (engine
//! `render_track`), then ONE undoable command each. Pure builders here; `AppState` runs
//! the jobs.

use std::path::{Path, PathBuf};

use cadenza_core::{ClipId, Tick, TrackId};
use cadenza_model::{
	ChannelPatch, Clip, ClipContent, Command, ControllerLane, Freeze, Note, Project, Track,
	TrackKind,
};

/// What a track render is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BounceKind {
	/// The track plays the render instead of its devices (`.cadenza/freeze/`).
	Freeze,
	/// A new audio track with the render, the original muted (`Audio/`).
	InPlace,
	/// Selected audio clips of one track merged into one clip (`Audio/`).
	Consolidate,
}

/// A render in flight.
#[derive(Debug, Clone, PartialEq)]
pub struct BounceJob {
	pub kind: BounceKind,
	pub track: TrackId,
	pub clips: Vec<ClipId>,
	/// Song position of the render's first frame.
	pub start: Tick,
	pub file: PathBuf,
	/// The new track / clip, created by the caller so redo is deterministic.
	pub new_track: TrackId,
	pub new_clip: ClipId,
	/// Freeze fingerprint of the project the render was made from.
	pub source: Option<String>,
}

/// A free file name `<dir>/<stem> <n>.wav`.
pub fn render_path(dir: &Path, stem: &str) -> PathBuf {
	let stem = cadenza_model::files::sanitize_file_name(stem);
	(1..)
		.map(|n| dir.join(format!("{stem} {n}.wav")))
		.find(|p| !p.exists())
		.expect("a free file name")
}

/// Freezing makes sense for tracks that compute audio.
pub fn can_freeze(track: &Track) -> bool {
	matches!(track.kind, TrackKind::Instrument | TrackKind::Audio)
}

/// The commands that land a finished render (`seconds` long), or why not.
pub fn finish(project: &Project, job: &BounceJob, seconds: f64) -> Result<Command, String> {
	let track = project
		.track(job.track)
		.ok_or("the track was removed before the render finished")?;
	let tempo = &project.tempo;
	let end = tempo.seconds_to_tick(tempo.tick_to_seconds(job.start) + seconds.max(0.001));
	let length = Tick((end - job.start).0.max(1));
	match job.kind {
		BounceKind::Freeze => Ok(Command::SetTrackFreeze {
			track: job.track,
			freeze: Some(Freeze {
				file: job.file.clone(),
				source: job.source.clone(),
			}),
		}),
		BounceKind::InPlace => {
			let mut t = Track::new(format!("{} (bounce)", track.name), TrackKind::Audio);
			t.id = job.new_track;
			t.color = track.color.clone();
			t.folder = track.folder;
			t.channel.volume_db = track.channel.volume_db;
			t.channel.pan = track.channel.pan;
			t.channel.output = track.channel.output;
			let mut clip = Clip::audio(t.name.clone(), job.start, length, &job.file);
			clip.id = job.new_clip;
			t.clips.push(clip);
			let index = project.track_index(job.track).map(|i| i + 1);
			Ok(Command::Batch {
				label: format!("Bounce '{}' in place", track.name),
				commands: vec![
					Command::AddTrack { track: t, index },
					Command::SetChannel {
						track: Some(job.track),
						patch: ChannelPatch {
							mute: Some(true),
							..ChannelPatch::default()
						},
					},
				],
			})
		}
		BounceKind::Consolidate => {
			let gone: Vec<&Clip> = track
				.clips
				.iter()
				.filter(|c| job.clips.contains(&c.id))
				.collect();
			if gone.is_empty() {
				return Err("the clips were removed before the render finished".into());
			}
			let mut clip = Clip::audio(
				format!("{} (consolidated)", gone[0].name),
				job.start,
				length,
				&job.file,
			);
			clip.id = job.new_clip;
			let mut commands: Vec<Command> = gone
				.iter()
				.map(|c| Command::RemoveClip { clip: c.id })
				.collect();
			commands.push(Command::AddClip {
				track: job.track,
				clip,
			});
			Ok(Command::Batch {
				label: "Consolidate clips".into(),
				commands,
			})
		}
	}
}

/// The track and span of the clips to consolidate (they must share one track and be at
/// least two), or why not.
pub fn consolidation(project: &Project, clips: &[ClipId]) -> Result<(TrackId, Tick, Tick), String> {
	let found: Vec<(TrackId, &Clip)> = clips
		.iter()
		.filter_map(|id| project.find_clip(*id).map(|(t, c)| (t.id, c)))
		.collect();
	if found.len() < 2 {
		return Err("Select two or more clips on one track to consolidate".into());
	}
	let track = found[0].0;
	if found.iter().any(|(t, _)| *t != track) {
		return Err("Consolidate works on clips of one track".into());
	}
	let start = found.iter().map(|(_, c)| c.start).min().expect("non-empty");
	let end = found.iter().map(|(_, c)| c.end()).max().expect("non-empty");
	Ok((track, start, end))
}

/// MIDI consolidation needs no render: one clip over the whole span with every note and
/// controller event (later clips win where lanes overlap). One undo step.
pub fn consolidate_midi(project: &Project, clips: &[ClipId], new_clip: ClipId) -> Option<Command> {
	let (track, start, end) = consolidation(project, clips).ok()?;
	let mut parts: Vec<&Clip> = clips
		.iter()
		.filter_map(|id| project.find_clip(*id).map(|(_, c)| c))
		.collect();
	if parts.iter().any(|c| !c.is_midi()) {
		return None;
	}
	parts.sort_by_key(|c| c.start);
	let mut notes: Vec<Note> = Vec::new();
	let mut lanes: Vec<ControllerLane> = Vec::new();
	for c in &parts {
		let shift = c.start - start;
		for n in c.notes().iter().filter(|n| n.start < c.length) {
			notes.push(Note {
				id: cadenza_core::NoteId::new(),
				start: n.start + shift,
				length: n.length.min(c.length - n.start),
				..n.clone()
			});
		}
		for l in c.controllers() {
			let points = l
				.points
				.iter()
				.filter(|p| p.tick < c.length)
				.map(|p| cadenza_model::ControllerPoint::new(p.tick + shift, p.value));
			match lanes.iter_mut().find(|x| x.controller == l.controller) {
				Some(x) => {
					let from = shift;
					let to = shift + c.length;
					let new: Vec<_> = points.collect();
					x.points = cadenza_model::controller::splice(&x.points, from, to, &new);
				}
				None => lanes.push(ControllerLane {
					controller: l.controller,
					points: points.collect(),
				}),
			}
		}
	}
	let mut clip = Clip::midi(parts[0].name.clone(), start, end - start)
		.with_notes(notes)
		.with_controllers(lanes);
	clip.id = new_clip;
	if let ClipContent::Midi { .. } = &clip.content {
		let mut commands: Vec<Command> = parts
			.iter()
			.map(|c| Command::RemoveClip { clip: c.id })
			.collect();
		commands.push(Command::AddClip { track, clip });
		return Some(Command::Batch {
			label: "Consolidate clips".into(),
			commands,
		});
	}
	None
}

#[cfg(test)]
mod tests {
	use super::*;
	use cadenza_core::PPQ;
	use cadenza_model::Document;

	#[test]
	fn midi_consolidation_merges_notes_in_one_step() {
		let mut p = Project::new("c");
		let mut t = Track::new("Keys", TrackKind::Instrument);
		let a = Clip::midi("A", Tick(0), Tick(PPQ * 4)).with_notes([Note::new(
			60,
			Tick(0),
			Tick(PPQ),
			0.8,
		)]);
		let b = Clip::midi("B", Tick(PPQ * 8), Tick(PPQ * 4)).with_notes([Note::new(
			64,
			Tick(PPQ),
			Tick(PPQ),
			0.8,
		)]);
		let ids = vec![a.id, b.id];
		t.clips = vec![a, b];
		let track = t.id;
		p.tracks.push(t);
		let mut doc = Document::new(p);
		let new = ClipId::new();
		let cmd = consolidate_midi(doc.project(), &ids, new).unwrap();
		doc.execute(cmd).unwrap();
		let t = doc.project().track(track).unwrap();
		assert_eq!(t.clips.len(), 1);
		let c = &t.clips[0];
		assert_eq!((c.id, c.start, c.length), (new, Tick(0), Tick(PPQ * 12)));
		let starts: Vec<Tick> = c.notes().iter().map(|n| n.start).collect();
		assert_eq!(starts, vec![Tick(0), Tick(PPQ * 9)]);
		assert_eq!(doc.history_labels(), vec!["Consolidate clips"]);
		assert!(consolidation(doc.project(), &[new]).is_err());
	}

	#[test]
	fn renders_land_as_one_command() {
		let mut p = Project::new("b");
		let t = Track::new("Lead", TrackKind::Instrument);
		let track = t.id;
		p.tracks.push(t);
		let job = BounceJob {
			kind: BounceKind::InPlace,
			track,
			clips: vec![],
			start: Tick(PPQ * 4),
			file: PathBuf::from("/tmp/x.wav"),
			new_track: TrackId::new(),
			new_clip: ClipId::new(),
			source: None,
		};
		let mut doc = Document::new(p);
		doc.execute(finish(doc.project(), &job, 2.0).unwrap())
			.unwrap();
		let p = doc.project();
		assert_eq!(p.tracks.len(), 2);
		assert!(p.tracks[0].channel.mute, "the original is muted");
		let clip = &p.tracks[1].clips[0];
		// 2 s at 120 bpm = 4 beats.
		assert_eq!((clip.start, clip.length), (Tick(PPQ * 4), Tick(PPQ * 4)));
		let freeze = BounceJob {
			kind: BounceKind::Freeze,
			..job
		};
		doc.execute(finish(doc.project(), &freeze, 2.0).unwrap())
			.unwrap();
		assert!(doc.project().track(track).unwrap().freeze.is_some());
	}
}
