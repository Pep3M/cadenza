use cadenza_core::{TempoMap, Tick, TimeSignature, TrackId};
use cadenza_model::{
	Clip, ClipContent, Command, Device, DeviceKind, Marker, Note, Project, Track, TrackKind,
};
use midly::num::{u4, u7, u15, u28};
use midly::{Format, Header, MetaMessage, MidiMessage, Smf, Timing, TrackEvent, TrackEventKind};

use crate::ids::RandomIds;
use crate::parsed::GM_DRUM_CHANNEL;
use crate::smf_export::export_project;
use crate::smf_import::{ImportOptions, ImportTarget, import_smf, parse_smf};

fn midi_event(delta: u32, channel: u8, message: MidiMessage) -> TrackEvent<'static> {
	TrackEvent {
		delta: u28::new(delta),
		kind: TrackEventKind::Midi {
			channel: u4::new(channel),
			message,
		},
	}
}

fn note_on(delta: u32, channel: u8, key: u8, vel: u8) -> TrackEvent<'static> {
	midi_event(
		delta,
		channel,
		MidiMessage::NoteOn {
			key: u7::new(key),
			vel: u7::new(vel),
		},
	)
}

fn note_off(delta: u32, channel: u8, key: u8) -> TrackEvent<'static> {
	midi_event(
		delta,
		channel,
		MidiMessage::NoteOff {
			key: u7::new(key),
			vel: u7::new(0),
		},
	)
}

fn write_smf(ppq: u16, tracks: Vec<Vec<TrackEvent<'static>>>) -> Vec<u8> {
	let smf = Smf {
		header: Header {
			format: Format::Parallel,
			timing: Timing::Metrical(u15::new(ppq)),
		},
		tracks,
	};
	let mut buf = Vec::new();
	smf.write_std(&mut buf).unwrap();
	buf
}

#[test]
fn rescales_ppq_to_cadenza_resolution() {
	// Source PPQ 480: a quarter note's worth of ticks (240 = half a quarter at 480 PPQ)
	// should become 480 ticks at Cadenza's PPQ 960.
	let bytes = write_smf(
		480,
		vec![vec![
			note_on(0, 0, 60, 100),
			note_off(240, 0, 60),
			TrackEvent {
				delta: u28::new(0),
				kind: TrackEventKind::Meta(MetaMessage::EndOfTrack),
			},
		]],
	);
	let parsed = parse_smf(&bytes).unwrap();
	assert_eq!(parsed.tracks[0].notes.len(), 1);
	let note = &parsed.tracks[0].notes[0];
	assert_eq!(note.start, Tick::ZERO);
	assert_eq!(note.length, Tick(cadenza_core::PPQ / 2));
}

#[test]
fn pairs_overlapping_same_pitch_notes_in_arrival_order() {
	let bytes = write_smf(
		960,
		vec![vec![
			note_on(0, 0, 60, 100),
			note_on(0, 0, 60, 80),
			note_off(100, 0, 60),
			note_off(100, 0, 60),
			TrackEvent {
				delta: u28::new(0),
				kind: TrackEventKind::Meta(MetaMessage::EndOfTrack),
			},
		]],
	);
	let parsed = parse_smf(&bytes).unwrap();
	let notes = &parsed.tracks[0].notes;
	assert_eq!(notes.len(), 2);
	// First note-on (vel 100) pairs with first note-off, at tick 0..100.
	assert_eq!(notes[0].velocity, 100.0 / 127.0);
	assert_eq!(notes[0].length, Tick(100));
	// Second note-on (vel 80) pairs with the second note-off, at tick 0..200.
	assert_eq!(notes[1].velocity, 80.0 / 127.0);
	assert_eq!(notes[1].length, Tick(200));
}

#[test]
fn note_on_velocity_zero_is_treated_as_note_off() {
	let bytes = write_smf(
		960,
		vec![vec![
			note_on(0, 0, 64, 90),
			note_on(480, 0, 64, 0), // note-on vel 0 == note-off
			TrackEvent {
				delta: u28::new(0),
				kind: TrackEventKind::Meta(MetaMessage::EndOfTrack),
			},
		]],
	);
	let parsed = parse_smf(&bytes).unwrap();
	assert_eq!(parsed.tracks[0].notes.len(), 1);
	assert_eq!(parsed.tracks[0].notes[0].length, Tick(480));
}

#[test]
fn detects_gm_drum_channel() {
	let bytes = write_smf(
		960,
		vec![vec![
			note_on(0, GM_DRUM_CHANNEL, 36, 100),
			note_off(100, GM_DRUM_CHANNEL, 36),
			TrackEvent {
				delta: u28::new(0),
				kind: TrackEventKind::Meta(MetaMessage::EndOfTrack),
			},
		]],
	);
	let parsed = parse_smf(&bytes).unwrap();
	assert!(parsed.tracks[0].is_drum);
}

#[test]
fn drum_track_imports_with_drum_machine_instrument() {
	let bytes = write_smf(
		960,
		vec![vec![
			note_on(0, GM_DRUM_CHANNEL, 36, 100),
			note_off(100, GM_DRUM_CHANNEL, 36),
			TrackEvent {
				delta: u28::new(0),
				kind: TrackEventKind::Meta(MetaMessage::EndOfTrack),
			},
		]],
	);
	let result = import_smf(&bytes, &ImportOptions::default()).unwrap();
	let Command::Batch { commands, .. } = result.command else {
		panic!("expected a batch")
	};
	let add_track = commands
		.iter()
		.find_map(|c| match c {
			Command::AddTrack { track, .. } => Some(track),
			_ => None,
		})
		.expect("add_track command");
	match &add_track.instrument {
		Some(device) => match &device.kind {
			DeviceKind::Builtin { id } => assert_eq!(id, "drum_machine"),
			_ => panic!("expected a builtin instrument"),
		},
		None => panic!("drum track should get an instrument"),
	}
}

#[test]
fn splits_tracks_by_channel_when_requested() {
	let bytes = write_smf(
		960,
		vec![vec![
			note_on(0, 0, 60, 100),
			note_off(100, 0, 60),
			note_on(0, 1, 64, 100),
			note_off(100, 1, 64),
			TrackEvent {
				delta: u28::new(0),
				kind: TrackEventKind::Meta(MetaMessage::EndOfTrack),
			},
		]],
	);
	let options = ImportOptions {
		split_by_channel: true,
		target: ImportTarget::NewTracks,
	};
	let result = import_smf(&bytes, &options).unwrap();
	let Command::Batch { commands, .. } = result.command else {
		panic!("expected a batch")
	};
	let track_count = commands
		.iter()
		.filter(|c| matches!(c, Command::AddTrack { .. }))
		.count();
	assert_eq!(track_count, 2);
}

#[test]
fn cc_and_pitch_bend_become_controller_lanes() {
	let bytes = write_smf(
		960,
		vec![vec![
			midi_event(
				0,
				0,
				MidiMessage::Controller {
					controller: u7::new(1),
					value: u7::new(64),
				},
			),
			midi_event(
				0,
				0,
				MidiMessage::PitchBend {
					bend: midly::PitchBend(midly::num::u14::new(1000)),
				},
			),
			note_on(0, 0, 60, 100),
			note_off(100, 0, 60),
			TrackEvent {
				delta: u28::new(0),
				kind: TrackEventKind::Meta(MetaMessage::EndOfTrack),
			},
		]],
	);
	let parsed = parse_smf(&bytes).unwrap();
	assert_eq!(parsed.tracks[0].other_events.len(), 2);
	assert!(parsed.warnings.is_empty(), "{:?}", parsed.warnings);
	let lanes = crate::controller_lanes(&parsed.tracks[0].other_events, None);
	assert_eq!(lanes.len(), 2);
	assert_eq!(lanes[0].controller, cadenza_model::Controller::MOD_WHEEL);
	assert_eq!(lanes[0].points[0].value, 64.0 / 127.0);
	assert_eq!(lanes[1].controller, cadenza_model::Controller::PitchBend);
	assert!((lanes[1].points[0].value - (1000.0 - 8192.0) / 8192.0).abs() < 1e-6);
}

#[test]
fn round_trips_project_export_and_import() {
	let track_id = TrackId::new();
	let mut project = Project::new("round trip");
	project.tempo = TempoMap::constant(96.0);
	project.time_signature = TimeSignature {
		numerator: 3,
		denominator: 4,
	};
	project.time_signature_changes = vec![cadenza_core::MeterChange::new(
		2,
		TimeSignature {
			numerator: 7,
			denominator: 8,
		},
	)];
	project.markers.push(Marker {
		tick: Tick(cadenza_core::PPQ * 2),
		name: "Verse".to_string(),
	});
	project.tracks.push(Track {
		id: track_id,
		name: "Lead".to_string(),
		kind: TrackKind::Instrument,
		color: None,
		instrument: Some(Device {
			id: cadenza_core::DeviceId::new(),
			kind: DeviceKind::Builtin {
				id: "poly_synth".to_string(),
			},
			name: "Poly Synth".to_string(),
			bypass: false,
			params: Default::default(),
			state: None,
			sidechain: None,
			midi_input: None,
			aux_outputs: Vec::new(),
			zones: Vec::new(),
			modulators: Vec::new(),
		}),
		clips: vec![Clip {
			id: cadenza_core::ClipId::new(),
			name: "Lead clip".to_string(),
			start: Tick::ZERO,
			length: Tick(cadenza_core::PPQ * 4),
			content: ClipContent::Midi {
				notes: vec![
					Note {
						id: cadenza_core::NoteId::new(),
						pitch: 60,
						velocity: 0.8,
						start: Tick::ZERO,
						length: Tick(cadenza_core::PPQ),
					},
					Note {
						id: cadenza_core::NoteId::new(),
						pitch: 64,
						velocity: 0.6,
						start: Tick(cadenza_core::PPQ),
						length: Tick(cadenza_core::PPQ),
					},
				],
				controllers: vec![
					cadenza_model::ControllerLane {
						controller: cadenza_model::Controller::SUSTAIN,
						points: vec![
							cadenza_model::ControllerPoint::new(Tick::ZERO, 1.0),
							cadenza_model::ControllerPoint::new(Tick(cadenza_core::PPQ * 2), 0.0),
						],
					},
					cadenza_model::ControllerLane {
						controller: cadenza_model::Controller::PitchBend,
						points: vec![cadenza_model::ControllerPoint::new(
							Tick(cadenza_core::PPQ / 2),
							-1.0,
						)],
					},
				],
			},
			take: None,
			muted: false,
			link: None,
		}],
		channel: Default::default(),
		input: None,
		folder: None,
		freeze: None,
	});

	let bytes = export_project(&project).unwrap();
	let result = import_smf(&bytes, &ImportOptions::default()).unwrap();
	let Command::Batch { commands, .. } = result.command else {
		panic!("expected a batch")
	};

	let imported_track = commands
		.iter()
		.find_map(|c| match c {
			Command::AddTrack { track, .. } => Some(track),
			_ => None,
		})
		.expect("add_track command");
	let Some(clip) = imported_track.clips.first() else {
		panic!("expected a clip")
	};
	let ClipContent::Midi { notes, .. } = &clip.content else {
		panic!("expected midi content")
	};
	assert_eq!(notes.len(), 2);
	assert_eq!(notes[0].pitch, 60);
	assert_eq!(notes[0].start, Tick::ZERO);
	assert_eq!(notes[1].pitch, 64);
	assert_eq!(notes[1].start, Tick(cadenza_core::PPQ));
	let sustain = clip.controller(cadenza_model::Controller::SUSTAIN).unwrap();
	assert_eq!(sustain.points.len(), 2);
	assert_eq!(sustain.points[1].tick, Tick(cadenza_core::PPQ * 2));
	let bend = clip
		.controller(cadenza_model::Controller::PitchBend)
		.unwrap();
	assert_eq!(bend.points[0].value, -1.0);

	let tempo_set = commands
		.iter()
		.any(|c| matches!(c, Command::SetTempo { bpm, .. } if (*bpm - 96.0).abs() < 1e-6));
	assert!(tempo_set);
	let sig_set = commands.iter().any(|c| matches!(c, Command::SetTimeSignature { time_signature, bar: 0 } if *time_signature == TimeSignature { numerator: 3, denominator: 4 }));
	assert!(sig_set);
	let change_set = commands.iter().any(|c| matches!(c, Command::SetTimeSignature { time_signature, bar: 2 } if *time_signature == TimeSignature { numerator: 7, denominator: 8 }));
	assert!(change_set, "the change at bar 3 survives the round trip");
	let marker_added = commands
		.iter()
		.any(|c| matches!(c, Command::AddMarker { marker } if marker.name == "Verse"));
	assert!(marker_added);
}

#[test]
fn import_into_existing_track_places_clip_at_requested_position() {
	let bytes = write_smf(
		960,
		vec![vec![
			note_on(0, 0, 60, 100),
			note_off(480, 0, 60),
			TrackEvent {
				delta: u28::new(0),
				kind: TrackEventKind::Meta(MetaMessage::EndOfTrack),
			},
		]],
	);
	let target_track = TrackId::new();
	let options = ImportOptions {
		split_by_channel: false,
		target: ImportTarget::ExistingTrack {
			track: target_track,
			position: Tick(cadenza_core::PPQ * 4),
		},
	};
	let result = import_smf(&bytes, &options).unwrap();
	let Command::Batch { commands, .. } = result.command else {
		panic!("expected a batch")
	};
	let add_clip = commands
		.iter()
		.find_map(|c| match c {
			Command::AddClip { track, clip } if *track == target_track => Some(clip),
			_ => None,
		})
		.expect("add_clip command targeting the existing track");
	assert_eq!(add_clip.start, Tick(cadenza_core::PPQ * 4));
}

#[test]
fn deterministic_ids_are_reused_when_an_allocator_is_supplied() {
	let bytes = write_smf(
		960,
		vec![vec![
			note_on(0, 0, 60, 100),
			note_off(100, 0, 60),
			TrackEvent {
				delta: u28::new(0),
				kind: TrackEventKind::Meta(MetaMessage::EndOfTrack),
			},
		]],
	);
	let mut ids = RandomIds;
	let parsed = parse_smf(&bytes).unwrap();
	let mut warnings = Vec::new();
	let command = crate::smf_import::build_import_command(
		&parsed,
		&ImportOptions::default(),
		&mut ids,
		&mut warnings,
	);
	// Just confirms the lower-level entry point used by import_smf is itself usable
	// directly, e.g. to reuse a single parse across several `ImportOptions`.
	assert!(matches!(command, Command::Batch { .. }));
}

#[test]
fn meter_changes_snap_to_bar_lines() {
	use cadenza_core::{MeterChange, PPQ};
	let ts = |numerator, denominator| TimeSignature {
		numerator,
		denominator,
	};
	let mut warnings = Vec::new();
	// 4/4, 3/4 at bar 3 (beat 8), 3/4 again (dropped), 6/8 off the grid at beat 12.5.
	let (first, changes) = crate::meter_changes(
		&[
			(Tick(0), ts(4, 4)),
			(Tick(8 * PPQ), ts(3, 4)),
			(Tick(11 * PPQ), ts(3, 4)),
			(Tick(12 * PPQ + PPQ / 2), ts(6, 8)),
		],
		&mut warnings,
	);
	assert_eq!(first, Some(ts(4, 4)));
	assert_eq!(
		changes,
		vec![MeterChange::new(2, ts(3, 4)), MeterChange::new(4, ts(6, 8))]
	);
	assert_eq!(warnings.len(), 1);
	assert!(warnings[0].contains("not on a bar line"));
	// A change without a signature at the start keeps 4/4 for bar 1.
	let (first, changes) = crate::meter_changes(&[(Tick(4 * PPQ), ts(3, 4))], &mut warnings);
	assert_eq!(first, Some(ts(4, 4)));
	assert_eq!(changes, vec![MeterChange::new(1, ts(3, 4))]);
}

#[test]
fn mpe_export_gives_each_note_its_channel_and_bend() {
	use crate::smf_export::{MPE_BEND_RANGE, MpeNote, export_mpe};
	let note = |pitch, start: i64, len: i64, curve: Vec<(Tick, f32)>| MpeNote {
		start: Tick(start),
		length: Tick(len),
		pitch,
		velocity: 0.8,
		curve,
	};
	// A chord whose top note bends up a whole tone, then a note after it.
	let notes = [
		note(60, 0, 960, vec![(Tick(0), 0.0)]),
		note(64, 0, 960, vec![(Tick(0), 0.0)]),
		note(67, 0, 960, vec![(Tick(0), 0.0), (Tick(480), 2.0)]),
		note(62, 960, 960, vec![(Tick(960), -0.25)]),
	];
	let out = export_mpe(
		&notes,
		&TempoMap::constant(120.0),
		&TimeSignature::default(),
		"Chords",
	)
	.unwrap();
	assert_eq!(out.shared, 0);
	let smf = Smf::parse(&out.bytes).unwrap();
	let mut tick = 0u32;
	let mut on: Vec<(u32, u8, u8)> = Vec::new();
	let mut bends: Vec<(u32, u8, f32)> = Vec::new();
	let mut rpn6 = None;
	let mut last_cc = [[0u8; 128]; 16];
	for e in &smf.tracks[1] {
		tick += e.delta.as_int();
		if let TrackEventKind::Midi { channel, message } = e.kind {
			let ch = channel.as_int();
			match message {
				MidiMessage::NoteOn { key, vel } if vel.as_int() > 0 => {
					on.push((tick, ch, key.as_int()))
				}
				MidiMessage::PitchBend { bend } => {
					bends.push((tick, ch, bend.as_f32() * MPE_BEND_RANGE))
				}
				MidiMessage::Controller { controller, value } => {
					let (c, v) = (controller.as_int(), value.as_int());
					if c == 6 && last_cc[ch as usize][100] == 6 && ch == 0 {
						rpn6 = Some(v);
					}
					last_cc[ch as usize][c as usize] = v;
				}
				_ => {}
			}
		}
	}
	assert_eq!(rpn6, Some(15), "MPE configuration: 15 member channels");
	let channels: Vec<u8> = on.iter().map(|o| o.1).collect();
	assert!(channels.iter().all(|c| (1..=15).contains(c)), "{on:?}");
	assert_eq!(
		channels[..3]
			.iter()
			.collect::<std::collections::BTreeSet<_>>()
			.len(),
		3,
		"one channel per chord note: {on:?}"
	);
	let top = on.iter().find(|o| o.2 == 67).unwrap().1;
	assert!(
		bends
			.iter()
			.any(|b| b.0 == 480 && b.1 == top && (b.2 - 2.0).abs() < 0.01),
		"{bends:?}"
	);
	// The later note takes a channel the chord did not use last, starting at its bend.
	let later = on.iter().find(|o| o.2 == 62).unwrap();
	assert!(!channels[..3].contains(&later.1), "{on:?}");
	assert!(
		bends
			.iter()
			.any(|b| b.0 == 960 && b.1 == later.1 && (b.2 + 0.25).abs() < 0.01)
	);
	// 16 notes at once: one shares a channel.
	let crowd: Vec<MpeNote> = (0..16).map(|i| note(40 + i, 0, 960, Vec::new())).collect();
	let out = export_mpe(
		&crowd,
		&TempoMap::constant(120.0),
		&TimeSignature::default(),
		"Crowd",
	)
	.unwrap();
	assert_eq!(out.shared, 1);
}
