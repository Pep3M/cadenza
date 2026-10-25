//! Notes with their own bend (NE-9h) through the AI tools: written with add_midi_clip,
//! read with get_clip_notes, replaced with set_note_bend, exported as MPE.

use std::collections::BTreeMap;

use cadenza_mcp::Session;
use cadenza_mcp::convert::{DeviceSpec, PitchInput};
use cadenza_mcp::tools::{clips, midi, project, tracks};
use cadenza_model::TrackKind;

#[test]
fn notes_carry_their_own_bend_through_the_tools() {
	let dir = tempfile::tempdir().unwrap();
	let mut session = Session::new();
	let track_id = tracks::create_track(
		&mut session,
		tracks::CreateTrackArgs {
			name: "Guitar".into(),
			kind: TrackKind::Instrument,
			instrument: Some(DeviceSpec {
				device: "poly_synth".into(),
				params: BTreeMap::new(),
				preset: None,
			}),
			color: None,
			output: None,
		},
	)
	.unwrap()
	.track_id;
	let note = |pitch: u8, bend: Option<Vec<clips::BendInput>>| clips::NoteInput {
		pitch: PitchInput(pitch),
		start: 0.0,
		length: 2.0,
		velocity: Some(0.8),
		bend,
	};
	// A chord whose top note bends up a whole tone over its first beat.
	let up = vec![
		clips::BendInput {
			at_beats: 0.0,
			semitones: 0.0,
		},
		clips::BendInput {
			at_beats: 1.0,
			semitones: 2.0,
		},
	];
	let clip_id = clips::add_midi_clip(
		&mut session,
		clips::AddMidiClipArgs {
			track_id,
			start_beat: 0.0,
			length_beats: 4.0,
			name: None,
			notes: vec![note(55, None), note(59, None), note(62, Some(up))],
		},
	)
	.unwrap()
	.clip_id;
	let read = project::get_clip_notes(&session, project::GetClipNotesArgs { clip_id }).unwrap();
	let top = read.iter().find(|n| n.pitch == 62).unwrap();
	assert!(
		read.iter()
			.filter(|n| n.pitch != 62)
			.all(|n| n.bend.is_empty())
	);
	let first = top.bend.first().unwrap();
	let last = top.bend.last().unwrap();
	assert_eq!(
		(first.at_beats, first.semitones),
		(0.0, 0.0),
		"{:?}",
		top.bend
	);
	assert_eq!(
		(last.at_beats, last.semitones),
		(1.0, 2.0),
		"{:?}",
		top.bend
	);
	// The ramp is written as held steps every 1/64 beat.
	let stored = session
		.document()
		.project()
		.find_clip(clip_id)
		.unwrap()
		.1
		.notes()
		.iter()
		.find(|n| n.pitch == 62)
		.unwrap()
		.bend
		.len();
	assert!(stored >= 60, "{stored}");
	// Plain MIDI cannot hold it: a warning; MPE keeps it.
	let plain = midi::export_midi(
		&session,
		midi::ExportMidiArgs {
			path: dir.path().join("plain.mid").display().to_string(),
			clip_id: Some(clip_id),
			..Default::default()
		},
	)
	.unwrap();
	assert!(
		plain.warnings.iter().any(|w| w.contains("mpe")),
		"{plain:?}"
	);
	let mpe_path = dir.path().join("mpe.mid");
	let out = midi::export_midi(
		&session,
		midi::ExportMidiArgs {
			path: mpe_path.display().to_string(),
			clip_id: Some(clip_id),
			mpe: true,
			..Default::default()
		},
	)
	.unwrap();
	assert_eq!(out.notes, 3);
	let parsed = cadenza_midi_io::parse_smf(&std::fs::read(&mpe_path).unwrap()).unwrap();
	let notes: Vec<_> = parsed.tracks.iter().flat_map(|t| t.notes.clone()).collect();
	// Read back as MPE: the member channel's bend is the note's own curve again, +2
	// semitones (of a 48-semitone range) at the top of the ramp.
	assert!(parsed.mpe.is_some());
	let top_curve = &notes.iter().find(|n| n.pitch == 62).unwrap().bend;
	let peak = top_curve
		.iter()
		.map(|p| p.semitones)
		.fold(f32::MIN, f32::max);
	assert!((peak - 2.0).abs() < 0.05, "{top_curve:?}");
	// Importing the MPE file gives the note its curve back, on one instrument track.
	let imported = midi::import_midi(
		&mut session,
		midi::ImportMidiArgs {
			path: mpe_path.display().to_string(),
			split_by_channel: true,
			track_id: None,
			at_beats: None,
		},
	)
	.unwrap();
	assert_eq!(imported.created_tracks.len(), 1, "{imported:?}");
	let model_note = |clip: cadenza_core::ClipId| {
		session
			.document()
			.project()
			.find_clip(clip)
			.unwrap()
			.1
			.notes()
			.iter()
			.find(|n| n.pitch == 62)
			.unwrap()
			.clone()
	};
	let (before, after) = (
		model_note(clip_id),
		model_note(imported.created_tracks[0].clip_id),
	);
	assert!(!after.bend.is_empty());
	for k in 0..before.length.0 / 30 {
		let at = cadenza_core::Tick(k * 30);
		assert!(
			(before.bend_at(at) - after.bend_at(at)).abs() < 0.02,
			"at {at:?}: {} vs {}",
			before.bend_at(at),
			after.bend_at(at)
		);
	}
	// set_note_bend straightens it again.
	let id = top.id;
	let straight = clips::set_note_bend(
		&mut session,
		clips::SetNoteBendArgs {
			clip_id,
			note_id: id,
			points: Vec::new(),
		},
	)
	.unwrap();
	assert!(straight.bend.is_empty());
	let read = project::get_clip_notes(&session, project::GetClipNotesArgs { clip_id }).unwrap();
	assert!(read.iter().all(|n| n.bend.is_empty()));
	assert_eq!(
		session.document().history_labels().last().copied(),
		Some("Edit 1 note")
	);
}
