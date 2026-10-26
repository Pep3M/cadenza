//! Builds a small song skeleton end-to-end through the public tool functions: an
//! instrument track with a chord progression, a drum track, a bass track, a reverb bus
//! with a send, and a limiter on the master. Also checks that undo reverses a whole
//! helper call (not just part of it).

use std::collections::BTreeMap;

use cadenza_mcp::tools::{arrange, clips, compose, project, tracks};
use cadenza_mcp::{Session, convert::DeviceSpec};
use cadenza_model::TrackKind;

#[test]
fn builds_a_song_skeleton_and_undo_reverses_whole_steps() {
	let mut session = Session::new();

	project::new_project(
		&mut session,
		project::NewProjectArgs {
			name: "Skeleton".into(),
			bpm: Some(100.0),
			time_signature: None,
		},
	)
	.unwrap();

	let drums = tracks::create_track(
		&mut session,
		tracks::CreateTrackArgs {
			name: "Drums".into(),
			kind: TrackKind::Instrument,
			instrument: Some(DeviceSpec {
				device: "drum_machine".into(),
				params: BTreeMap::new(),
				preset: None,
			}),
			color: None,
			output: None,
		},
	)
	.unwrap()
	.track_id;

	let keys = tracks::create_track(
		&mut session,
		tracks::CreateTrackArgs {
			name: "Keys".into(),
			kind: TrackKind::Instrument,
			instrument: Some(DeviceSpec {
				device: "electric_piano".into(),
				params: BTreeMap::new(),
				preset: None,
			}),
			color: None,
			output: None,
		},
	)
	.unwrap()
	.track_id;

	let bass = tracks::create_track(
		&mut session,
		tracks::CreateTrackArgs {
			name: "Bass".into(),
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

	let reverb_bus = tracks::create_track(
		&mut session,
		tracks::CreateTrackArgs {
			name: "Reverb".into(),
			kind: TrackKind::Bus,
			instrument: None,
			color: None,
			output: None,
		},
	)
	.unwrap()
	.track_id;
	tracks::insert_device(
		&mut session,
		tracks::InsertDeviceArgs {
			track_id: Some(reverb_bus),
			device: DeviceSpec {
				device: "reverb".into(),
				params: BTreeMap::new(),
				preset: None,
			},
			index: None,
		},
	)
	.unwrap();
	let send_id = tracks::add_send(
		&mut session,
		tracks::AddSendArgs {
			track_id: keys,
			bus_track_id: reverb_bus,
			level_db: -12.0,
			pre_fader: false,
		},
	)
	.unwrap()
	.send_id;

	tracks::insert_device(
		&mut session,
		tracks::InsertDeviceArgs {
			track_id: None,
			device: DeviceSpec {
				device: "limiter".into(),
				params: BTreeMap::new(),
				preset: None,
			},
			index: None,
		},
	)
	.unwrap();

	let mut lanes = BTreeMap::new();
	lanes.insert("kick".to_owned(), "X...X...X...X...".to_owned());
	lanes.insert("closed_hat".to_owned(), "x.x.x.x.x.x.x.x.".to_owned());
	compose::write_drum_pattern(
		&mut session,
		compose::WriteDrumPatternArgs {
			track_id: drums,
			start_beat: 0.0,
			bars: 1,
			steps_per_beat: Some(4),
			swing: None,
			lanes,
		},
	)
	.unwrap();

	let chord_clip = compose::write_chord_progression(
		&mut session,
		compose::WriteChordProgressionArgs {
			track_id: keys,
			start_beat: 0.0,
			progression: "Cmaj7 Am7 Dm7 G7".into(),
			key: None,
			beats_per_chord: Some(1.0),
			durations: None,
			voicing: None,
			pattern: None,
			rate_beats: None,
			center_pitch: None,
			velocity: None,
			include_bass: Some(false),
			bass_octave: None,
			repeat: None,
		},
	)
	.unwrap()
	.clip_id;

	// Undo the whole chord-progression helper call: the clip must disappear entirely,
	// not just some of its notes.
	let before_undo = session.document().history_labels().len();
	let label = project::undo(&mut session).unwrap().label;
	assert!(label.to_lowercase().contains("clip"), "{label}");
	assert_eq!(session.document().history_labels().len(), before_undo - 1);
	assert!(session.document().project().find_clip(chord_clip).is_none());
	project::redo(&mut session).unwrap();
	assert!(session.document().project().find_clip(chord_clip).is_some());

	clips::add_midi_clip(
		&mut session,
		clips::AddMidiClipArgs {
			track_id: bass,
			start_beat: 0.0,
			length_beats: 4.0,
			name: Some("Bass".into()),
			notes: vec![clips::NoteInput {
				pitch: cadenza_mcp::convert::PitchInput(36),
				start: 0.0,
				length: 4.0,
				velocity: Some(0.9),
				bend: None,
				pressure: None,
				timbre: None,
			}],
		},
	)
	.unwrap();

	arrange::add_marker(
		&mut session,
		arrange::AddMarkerArgs {
			beat: 0.0,
			name: "Intro".into(),
		},
	)
	.unwrap();

	let summary = project::get_project(&session);
	assert_eq!(summary.tracks.len(), 4);
	assert_eq!(summary.markers.len(), 1);
	let keys_summary = summary.tracks.iter().find(|t| t.id == keys).unwrap();
	assert_eq!(keys_summary.sends.len(), 1);
	assert_eq!(keys_summary.sends[0].id, send_id);
	assert_eq!(summary.master.inserts.len(), 1);
}
