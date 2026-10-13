//! Standard MIDI File export: a whole [`Project`] (one track per instrument track plus a
//! tempo/meta track) or a single [`Clip`].

use cadenza_core::{Meter, TempoMap, Tick, TimeSignature};
use cadenza_model::{Clip, ClipContent, Project, TrackKind};
use midly::num::{u4, u7, u15, u24, u28};
use midly::{
	Format, Header, MetaMessage, MidiMessage, Smf, Timing, Track, TrackEvent, TrackEventKind,
};

use crate::error::{MidiIoError, Result};

/// One absolute-tick note-on or note-off, before delta-encoding.
struct RawNoteEvent {
	tick: i64,
	is_on: bool,
	pitch: u8,
	velocity: u8,
}

fn note_events(notes: &[(Tick, cadenza_model::Note)]) -> Vec<RawNoteEvent> {
	let mut events = Vec::with_capacity(notes.len() * 2);
	for (clip_start, note) in notes {
		let start = (*clip_start + note.start).0;
		let end = start + note.length.0.max(1);
		let velocity = ((note.velocity * 127.0).round() as i32).clamp(1, 127) as u8;
		events.push(RawNoteEvent {
			tick: start,
			is_on: true,
			pitch: note.pitch,
			velocity,
		});
		events.push(RawNoteEvent {
			tick: end,
			is_on: false,
			pitch: note.pitch,
			velocity: 0,
		});
	}
	// Note-offs before note-ons at the same tick, so a note never appears to retrigger
	// on top of itself.
	events.sort_by_key(|e| (e.tick, e.is_on));
	events
}

/// `name` must outlive the returned track; callers keep the owning `Vec<u8>` alive
/// (an arena) until after `Smf::write_std` has run, so nothing needs to be leaked.
fn to_midi_track<'a>(events: &[RawNoteEvent], name: &'a [u8]) -> Track<'a> {
	let mut track: Track<'a> = Vec::with_capacity(events.len() + 2);
	track.push(TrackEvent {
		delta: u28::new(0),
		kind: TrackEventKind::Meta(MetaMessage::TrackName(name)),
	});
	let mut last_tick = 0i64;
	for event in events {
		let delta = (event.tick - last_tick).max(0) as u32;
		last_tick = event.tick;
		let message = if event.is_on {
			MidiMessage::NoteOn {
				key: u7::new(event.pitch),
				vel: u7::new(event.velocity),
			}
		} else {
			MidiMessage::NoteOff {
				key: u7::new(event.pitch),
				vel: u7::new(0),
			}
		};
		track.push(TrackEvent {
			delta: u28::new(delta),
			kind: TrackEventKind::Midi {
				channel: u4::new(0),
				message,
			},
		});
	}
	track.push(TrackEvent {
		delta: u28::new(0),
		kind: TrackEventKind::Meta(MetaMessage::EndOfTrack),
	});
	track
}

/// `marker_names[i]` must outlive the returned track (same arena convention as
/// [`to_midi_track`]); `markers[i].1` indexes into it.
fn meta_track<'a>(
	tempo: &TempoMap,
	meter: Meter<'_>,
	markers: &[(Tick, usize)],
	marker_names: &'a [Vec<u8>],
) -> Track<'a> {
	struct MetaEvent<'a> {
		tick: i64,
		kind: TrackEventKind<'a>,
	}
	let mut events = Vec::new();
	for point in tempo.points() {
		let micros_per_quarter = (60_000_000.0 / point.bpm).round() as u32;
		events.push(MetaEvent {
			tick: point.tick.0,
			kind: TrackEventKind::Meta(MetaMessage::Tempo(u24::new(micros_per_quarter))),
		});
	}
	let signatures = std::iter::once((Tick::ZERO, meter.first()))
		.chain(meter.change_ticks().map(|(_, tick, ts)| (tick, ts)));
	for (tick, ts) in signatures {
		events.push(MetaEvent {
			tick: tick.0,
			kind: TrackEventKind::Meta(MetaMessage::TimeSignature(
				ts.numerator as u8,
				ts.denominator.trailing_zeros() as u8,
				24,
				8,
			)),
		});
	}
	for (tick, name_index) in markers {
		events.push(MetaEvent {
			tick: tick.0,
			kind: TrackEventKind::Meta(MetaMessage::Marker(&marker_names[*name_index])),
		});
	}
	events.sort_by_key(|e| e.tick);

	let mut track: Track<'a> = Vec::with_capacity(events.len() + 1);
	let mut last_tick = 0i64;
	for event in events {
		let delta = (event.tick - last_tick).max(0) as u32;
		last_tick = event.tick;
		track.push(TrackEvent {
			delta: u28::new(delta),
			kind: event.kind,
		});
	}
	track.push(TrackEvent {
		delta: u28::new(0),
		kind: TrackEventKind::Meta(MetaMessage::EndOfTrack),
	});
	track
}

fn write_smf(tracks: Vec<Track<'_>>) -> Result<Vec<u8>> {
	let header = Header {
		format: Format::Parallel,
		timing: Timing::Metrical(u15::new(cadenza_core::PPQ as u16)),
	};
	let smf = Smf { header, tracks };
	let mut buf = Vec::new();
	smf.write_std(&mut buf)
		.map_err(|e| MidiIoError::Write(e.to_string()))?;
	Ok(buf)
}

/// Exports the whole project: one meta track (tempo map, time signature, markers) plus
/// one track per instrument track that has at least one MIDI clip.
pub fn export_project(project: &Project) -> Result<Vec<u8>> {
	let marker_names: Vec<Vec<u8>> = project
		.markers
		.iter()
		.map(|m| m.name.clone().into_bytes())
		.collect();
	let markers: Vec<(Tick, usize)> = project
		.markers
		.iter()
		.enumerate()
		.map(|(i, m)| (m.tick, i))
		.collect();

	let track_names: Vec<Vec<u8>> = project
		.tracks
		.iter()
		.map(|t| t.name.clone().into_bytes())
		.collect();
	let mut notes_by_track: Vec<Vec<(Tick, cadenza_model::Note)>> =
		Vec::with_capacity(project.tracks.len());
	for track in &project.tracks {
		let notes = if track.kind == TrackKind::Instrument {
			track
				.clips
				.iter()
				.filter_map(|clip| match &clip.content {
					ClipContent::Midi { notes } => {
						Some(notes.iter().map(|n| (clip.start, n.clone())))
					}
					ClipContent::Audio { .. } => None,
				})
				.flatten()
				.collect()
		} else {
			Vec::new()
		};
		notes_by_track.push(notes);
	}

	let mut tracks = vec![meta_track(
		&project.tempo,
		project.meter(),
		&markers,
		&marker_names,
	)];
	for (index, track) in project.tracks.iter().enumerate() {
		let notes = &notes_by_track[index];
		if track.kind != TrackKind::Instrument || notes.is_empty() {
			continue;
		}
		tracks.push(to_midi_track(&note_events(notes), &track_names[index]));
	}

	write_smf(tracks)
}

/// Exports a single clip as a two-track SMF (tempo/time signature, then the notes).
/// `time_signature` is the one in effect at the clip start.
pub fn export_clip(
	clip: &Clip,
	tempo: &TempoMap,
	time_signature: &TimeSignature,
) -> Result<Vec<u8>> {
	let ClipContent::Midi { notes } = &clip.content else {
		return Err(MidiIoError::Write("clip has no MIDI content".to_string()));
	};
	let notes: Vec<(Tick, cadenza_model::Note)> =
		notes.iter().map(|n| (Tick::ZERO, n.clone())).collect();
	let name_bytes = clip.name.clone().into_bytes();
	let tracks = vec![
		meta_track(tempo, Meter::new(*time_signature, &[]), &[], &[]),
		to_midi_track(&note_events(&notes), &name_bytes),
	];
	write_smf(tracks)
}
