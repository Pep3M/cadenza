//! NE-3 acceptance: MIDI -> engine render -> `audio_to_midi` gives the notes back.

use std::collections::BTreeMap;

use cadenza_core::{ClipId, TrackId};
use cadenza_mcp::Session;
use cadenza_mcp::convert::{DeviceSpec, PitchInput};
use cadenza_mcp::tools::{audio, clips, notes, render, tracks};
use cadenza_model::{Controller, TrackKind};

/// (pitch, start beat, length beats): a mono line on the beat grid, 120 bpm.
const LINE: [(u8, f64, f64); 16] = [
	(57, 0.0, 1.0),
	(60, 1.0, 0.5),
	(64, 1.5, 0.5),
	(62, 2.0, 1.5),
	(60, 4.0, 0.75),
	(59, 4.75, 0.25),
	(57, 5.0, 1.0),
	(52, 6.0, 2.0),
	(48, 8.5, 0.5),
	(55, 9.0, 1.0),
	(67, 10.0, 0.5),
	(65, 10.5, 0.5),
	(64, 11.0, 1.0),
	(72, 12.0, 0.75),
	(69, 13.0, 1.0),
	(57, 14.0, 2.0),
];

fn synth_line(session: &mut Session) -> TrackId {
	let track_id = tracks::create_track(
		session,
		tracks::CreateTrackArgs {
			name: "Line".into(),
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
	clips::add_midi_clip(
		session,
		clips::AddMidiClipArgs {
			track_id,
			start_beat: 0.0,
			length_beats: 16.0,
			name: None,
			notes: LINE
				.iter()
				.map(|&(p, start, length)| clips::NoteInput {
					pitch: PitchInput(p),
					start,
					length,
					velocity: Some(0.8),
				})
				.collect(),
		},
	)
	.unwrap();
	track_id
}

/// (pitch, start, length) in song beats of a MIDI clip.
fn clip_notes(session: &Session, clip: ClipId) -> Vec<(u8, f64, f64)> {
	let project = session.document().project();
	let (_, clip) = project.find_clip(clip).unwrap();
	let mut out: Vec<(u8, f64, f64)> = clip
		.notes()
		.iter()
		.map(|n| (n.pitch, (clip.start + n.start).beats(), n.length.beats()))
		.collect();
	out.sort_by(|a, b| a.1.total_cmp(&b.1));
	out
}

#[tokio::test]
async fn rendered_midi_comes_back_as_the_same_notes() {
	let dir = tempfile::tempdir().unwrap();
	// The analysis cache goes to the user cache folder: keep it inside the test.
	unsafe { std::env::set_var("XDG_CACHE_HOME", dir.path().join("cache")) };
	let mut session = Session::new();
	let synth = synth_line(&mut session);
	let wav = dir.path().join("line.wav");
	render::render(
		session.document().project().clone(),
		None,
		cadenza_engine::PluginHosting::default(),
		render::RenderArgs {
			path: wav.display().to_string(),
			start_beat: Some(0.0),
			end_beat: Some(17.0),
			sample_rate: Some(44_100),
			..Default::default()
		},
	)
	.await
	.unwrap();
	// Only the audio from here on.
	tracks::delete_track(&mut session, tracks::DeleteTrackArgs { track_id: synth }).unwrap();
	let imported = audio::import_audio(
		&mut session,
		audio::ImportAudioArgs {
			path: wav.display().to_string(),
			at_beats: Some(0.0),
			..Default::default()
		},
	)
	.unwrap();

	let out = notes::audio_to_midi(
		&mut session,
		notes::AudioToMidiArgs {
			clip_id: imported.clip_id,
			..Default::default()
		},
	)
	.unwrap();
	assert!(out.created_track);
	let project = session.document().project();
	let new = project.track(out.track_id).unwrap();
	assert_eq!(new.kind, TrackKind::Instrument);
	assert!(new.instrument.is_some());
	// Right below the audio track.
	let index = |id: TrackId| project.tracks.iter().position(|t| t.id == id).unwrap();
	assert_eq!(index(out.track_id), index(imported.track_id) + 1);

	// COnP: same pitch, onset within 50 ms (0.1 beat).
	let found = clip_notes(&session, out.clip_id);
	let matched = LINE
		.iter()
		.filter(|&&(p, s, _)| {
			found
				.iter()
				.any(|&(q, t, _)| q == p && (t - s).abs() <= 0.1)
		})
		.count();
	let precision = matched as f64 / found.len() as f64;
	let recall = matched as f64 / LINE.len() as f64;
	let f = 2.0 * precision * recall / (precision + recall);
	assert!(f >= 0.95, "F {f:.3}: {found:?}");

	// Hum mode lands on the grid, so the notes come back exactly (ends of detached notes
	// may differ by the synth's release).
	session.document_mut().undo().unwrap();
	let out = notes::audio_to_midi(
		&mut session,
		notes::AudioToMidiArgs {
			clip_id: imported.clip_id,
			mode: notes::ConvertMode::Hum,
			..Default::default()
		},
	)
	.unwrap();
	assert_eq!(out.key.as_deref(), Some("A natural minor"), "{out:?}");
	let found = clip_notes(&session, out.clip_id);
	let starts: Vec<(u8, f64)> = found.iter().map(|&(p, s, _)| (p, s)).collect();
	let wanted: Vec<(u8, f64)> = LINE.iter().map(|&(p, s, _)| (p, s)).collect();
	assert_eq!(starts, wanted);

	// Into an existing instrument track, with bends, one octave up.
	session.document_mut().undo().unwrap();
	let keys = tracks::create_track(
		&mut session,
		tracks::CreateTrackArgs {
			name: "Keys".into(),
			kind: TrackKind::Instrument,
			instrument: None,
			color: None,
			output: None,
		},
	)
	.unwrap()
	.track_id;
	let before = session.document().project().tracks.len();
	let out = notes::audio_to_midi(
		&mut session,
		notes::AudioToMidiArgs {
			clip_id: imported.clip_id,
			track_id: Some(keys),
			bends: Some(true),
			octave_shift: Some(1),
			..Default::default()
		},
	)
	.unwrap();
	assert!(!out.created_track && out.bend_points > 0);
	let project = session.document().project();
	assert_eq!(project.tracks.len(), before);
	let (_, clip) = project.find_clip(out.clip_id).unwrap();
	let cadenza_model::ClipContent::Midi { controllers, .. } = &clip.content else {
		panic!("a MIDI clip")
	};
	assert_eq!(controllers[0].controller, Controller::PitchBend);
	assert_eq!(clip_notes(&session, out.clip_id)[0].0, 69);

	// Actionable errors.
	let err = notes::audio_to_midi(
		&mut session,
		notes::AudioToMidiArgs {
			clip_id: out.clip_id,
			..Default::default()
		},
	)
	.unwrap_err();
	assert!(err.to_string().contains("audio clips"), "{err}");
	let err = notes::audio_to_midi(
		&mut session,
		notes::AudioToMidiArgs {
			clip_id: imported.clip_id,
			key: Some("H major".into()),
			..Default::default()
		},
	)
	.unwrap_err();
	assert!(err.to_string().contains("A minor"), "{err}");
}
