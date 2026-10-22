//! Standard MIDI File export: a whole [`Project`] (one track per instrument track plus a
//! tempo/meta track) or a single [`Clip`].

use cadenza_core::{Meter, TempoMap, Tick, TimeSignature};
use cadenza_model::{Clip, Controller, Project, TrackKind};
use midly::num::{u4, u7, u15, u24, u28};
use midly::{
	Format, Header, MetaMessage, MidiMessage, Smf, Timing, Track, TrackEvent, TrackEventKind,
};

use crate::error::{MidiIoError, Result};

/// One absolute-tick MIDI event, before delta-encoding.
#[derive(Debug, Clone, Copy, PartialEq)]
enum RawEvent {
	NoteOff { pitch: u8 },
	Control { controller: Controller, value: f32 },
	NoteOn { pitch: u8, velocity: u8 },
}

impl RawEvent {
	/// Note-offs, then controllers (a sustain pedal or bend applies to the notes starting
	/// with it), then note-ons at the same tick.
	fn order(&self) -> u8 {
		match self {
			RawEvent::NoteOff { .. } => 0,
			RawEvent::Control { .. } => 1,
			RawEvent::NoteOn { .. } => 2,
		}
	}

	fn message(&self) -> MidiMessage {
		let u7v = |v: f32| u7::new((v.clamp(0.0, 1.0) * 127.0).round() as u8);
		match *self {
			RawEvent::NoteOn { pitch, velocity } => MidiMessage::NoteOn {
				key: u7::new(pitch.min(127)),
				vel: u7::new(velocity),
			},
			RawEvent::NoteOff { pitch } => MidiMessage::NoteOff {
				key: u7::new(pitch.min(127)),
				vel: u7::new(0),
			},
			RawEvent::Control {
				controller: Controller::Cc(n),
				value,
			} => MidiMessage::Controller {
				controller: u7::new(n.min(127)),
				value: u7v(value),
			},
			RawEvent::Control {
				controller: Controller::PitchBend,
				value,
			} => {
				let v = value.clamp(-1.0, 1.0);
				let bend = if v < 0.0 { v * 8192.0 } else { v * 8191.0 }.round() as i16;
				MidiMessage::PitchBend {
					bend: midly::PitchBend::from_int(bend),
				}
			}
			RawEvent::Control {
				controller: Controller::ChannelPressure,
				value,
			} => MidiMessage::ChannelAftertouch { vel: u7v(value) },
		}
	}
}

/// The MIDI events of clips (`(clip start, clip)`): notes and controller lanes cut at
/// the clip end, absolute ticks, sorted.
fn clip_events(clips: &[&Clip], origin: Tick) -> Vec<(i64, RawEvent)> {
	let mut events = Vec::new();
	for clip in clips {
		let base = clip.start - origin;
		// Linked clips share content that may reach past a shorter copy: export what plays.
		for note in clip.notes().iter().filter(|n| n.start < clip.length) {
			let start = (base + note.start).0;
			let end = start + note.length.0.max(1);
			let velocity = ((note.velocity * 127.0).round() as i32).clamp(1, 127) as u8;
			events.push((
				start,
				RawEvent::NoteOn {
					pitch: note.pitch,
					velocity,
				},
			));
			events.push((end, RawEvent::NoteOff { pitch: note.pitch }));
		}
		for lane in clip.controllers() {
			for p in lane.points.iter().filter(|p| p.tick < clip.length) {
				events.push((
					(base + p.tick).0,
					RawEvent::Control {
						controller: lane.controller,
						value: p.value,
					},
				));
			}
		}
	}
	events.sort_by_key(|(tick, e)| (*tick, e.order()));
	events
}

/// `name` must outlive the returned track; callers keep the owning `Vec<u8>` alive
/// (an arena) until after `Smf::write_std` has run, so nothing needs to be leaked.
fn to_midi_track<'a>(events: &[(i64, RawEvent)], name: &'a [u8]) -> Track<'a> {
	let mut track: Track<'a> = Vec::with_capacity(events.len() + 2);
	track.push(TrackEvent {
		delta: u28::new(0),
		kind: TrackEventKind::Meta(MetaMessage::TrackName(name)),
	});
	let mut last_tick = 0i64;
	for (tick, event) in events {
		let delta = (tick - last_tick).max(0) as u32;
		last_tick = *tick;
		track.push(TrackEvent {
			delta: u28::new(delta),
			kind: TrackEventKind::Midi {
				channel: u4::new(0),
				message: event.message(),
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
	let events_by_track: Vec<Vec<(i64, RawEvent)>> = project
		.tracks
		.iter()
		.map(|track| {
			if track.kind != TrackKind::Instrument {
				return Vec::new();
			}
			let clips: Vec<&Clip> = track
				.clips
				.iter()
				.filter(|c| c.is_midi() && c.sounds())
				.collect();
			clip_events(&clips, Tick::ZERO)
		})
		.collect();

	let mut tracks = vec![meta_track(
		&project.tempo,
		project.meter(),
		&markers,
		&marker_names,
	)];
	for (index, track) in project.tracks.iter().enumerate() {
		let events = &events_by_track[index];
		if track.kind != TrackKind::Instrument || events.is_empty() {
			continue;
		}
		tracks.push(to_midi_track(events, &track_names[index]));
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
	if !clip.is_midi() {
		return Err(MidiIoError::Write("clip has no MIDI content".to_string()));
	}
	let name_bytes = clip.name.clone().into_bytes();
	let events = clip_events(&[clip], clip.start);
	let tracks = vec![
		meta_track(tempo, Meter::new(*time_signature, &[]), &[], &[]),
		to_midi_track(&events, &name_bytes),
	];
	write_smf(tracks)
}
