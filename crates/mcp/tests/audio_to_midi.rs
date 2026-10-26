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
					bend: None,
					pressure: None,
					timbre: None,
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

/// C - F - G - Am - C, block chords of two beats at 120 bpm.
const CHORDS: [[u8; 3]; 5] = [
	[60, 64, 67],
	[60, 65, 69],
	[59, 62, 67],
	[57, 60, 64],
	[55, 60, 64],
];

#[tokio::test]
async fn rendered_chords_come_back_as_chords() {
	let dir = tempfile::tempdir().unwrap();
	unsafe { std::env::set_var("XDG_CACHE_HOME", dir.path().join("cache")) };
	let mut session = Session::new();
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
	clips::add_midi_clip(
		&mut session,
		clips::AddMidiClipArgs {
			track_id: keys,
			start_beat: 0.0,
			length_beats: 10.0,
			name: None,
			notes: CHORDS
				.iter()
				.enumerate()
				.flat_map(|(i, chord)| {
					chord.iter().map(move |&p| clips::NoteInput {
						pitch: PitchInput(p),
						start: 2.0 * i as f64,
						length: 1.9,
						velocity: Some(0.8),
						bend: None,
						pressure: None,
						timbre: None,
					})
				})
				.collect(),
		},
	)
	.unwrap();
	let wav = dir.path().join("chords.wav");
	render::render(
		session.document().project().clone(),
		None,
		cadenza_engine::PluginHosting::default(),
		render::RenderArgs {
			path: wav.display().to_string(),
			start_beat: Some(0.0),
			end_beat: Some(11.0),
			sample_rate: Some(44_100),
			..Default::default()
		},
	)
	.await
	.unwrap();
	tracks::delete_track(&mut session, tracks::DeleteTrackArgs { track_id: keys }).unwrap();
	let imported = audio::import_audio(
		&mut session,
		audio::ImportAudioArgs {
			path: wav.display().to_string(),
			at_beats: Some(0.0),
			..Default::default()
		},
	)
	.unwrap();
	let mpe = dir.path().join("chords mpe.mid");
	let out = notes::audio_to_midi(
		&mut session,
		notes::AudioToMidiArgs {
			clip_id: imported.clip_id,
			mpe_path: Some(mpe.display().to_string()),
			..Default::default()
		},
	)
	.unwrap();
	assert_eq!(out.algorithm, notes::Algorithm::Poly, "{out:?}");
	// The MPE file: the same notes (read back as MPE: one zone, notes on its master).
	assert_eq!(
		out.mpe_file.as_deref(),
		Some(mpe.display().to_string().as_str())
	);
	let parsed = cadenza_midi_io::parse_smf(&std::fs::read(&mpe).unwrap()).unwrap();
	let mpe_notes: Vec<_> = parsed.tracks.iter().flat_map(|t| t.notes.clone()).collect();
	assert_eq!(mpe_notes.len(), out.note_count);
	assert_eq!(parsed.mpe.as_ref().map(|m| m.zones.len()), Some(1));
	assert!(mpe_notes.iter().all(|n| n.channel == 0));
	let found = clip_notes(&session, out.clip_id);
	// Every chord's notes start at its beat (within 0.1 beat).
	let wanted: Vec<(u8, f64)> = CHORDS
		.iter()
		.enumerate()
		.flat_map(|(i, c)| c.iter().map(move |&p| (p, 2.0 * i as f64)))
		.collect();
	let matched = wanted
		.iter()
		.filter(|&&(p, s)| {
			found
				.iter()
				.any(|&(q, t, _)| q == p && (t - s).abs() <= 0.1)
		})
		.count();
	assert!(
		matched as f64 >= 0.95 * wanted.len() as f64,
		"{matched} of {}: {found:?}",
		wanted.len()
	);
	// Anything else is an overtone of a note of the chord sounding then (the synthetic
	// electric piano is bright; the overtone filter is off by default).
	for &(q, t, _) in &found {
		let chord = CHORDS[((t + 0.1) / 2.0).floor().clamp(0.0, 4.0) as usize];
		let overtone = chord
			.iter()
			.any(|&p| q > p && [12, 19, 24, 28, 31].contains(&(q - p)));
		assert!(chord.contains(&q) || overtone, "{q} at {t}: {found:?}");
	}
	// Forced mono gives one line.
	session.document_mut().undo().unwrap();
	let out = notes::audio_to_midi(
		&mut session,
		notes::AudioToMidiArgs {
			clip_id: imported.clip_id,
			algorithm: notes::Algorithm::Mono,
			..Default::default()
		},
	)
	.unwrap();
	assert_eq!(out.algorithm, notes::Algorithm::Mono);
}

#[tokio::test]
async fn a_rendered_drum_loop_comes_back_as_a_drum_clip() {
	let dir = tempfile::tempdir().unwrap();
	unsafe { std::env::set_var("XDG_CACHE_HOME", dir.path().join("cache")) };
	let mut session = Session::new();
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
	let lanes: BTreeMap<String, String> = [
		("kick", "x... .... x... ...."),
		("snare", ".... x... .... x..."),
		("closed_hat", "..x. ..x. ..x. ..x."),
	]
	.into_iter()
	.map(|(k, v)| (k.to_owned(), v.to_owned()))
	.collect();
	cadenza_mcp::tools::compose::write_drum_pattern(
		&mut session,
		cadenza_mcp::tools::compose::WriteDrumPatternArgs {
			track_id: drums,
			start_beat: 0.0,
			bars: 2,
			steps_per_beat: None,
			swing: None,
			lanes,
		},
	)
	.unwrap();
	let wav = dir.path().join("drums.wav");
	render::render(
		session.document().project().clone(),
		None,
		cadenza_engine::PluginHosting::default(),
		render::RenderArgs {
			path: wav.display().to_string(),
			start_beat: Some(0.0),
			end_beat: Some(9.0),
			sample_rate: Some(44_100),
			..Default::default()
		},
	)
	.await
	.unwrap();
	tracks::delete_track(&mut session, tracks::DeleteTrackArgs { track_id: drums }).unwrap();
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
	assert_eq!(out.algorithm, notes::Algorithm::Percussive, "{out:?}");
	let project = session.document().project();
	let track = project.track(out.track_id).unwrap();
	assert_eq!(
		track.instrument.as_ref().and_then(|d| d.builtin_id()),
		Some("drum_machine")
	);
	let found = clip_notes(&session, out.clip_id);
	let mut wanted = Vec::new();
	for bar in 0..2 {
		let b = 4.0 * bar as f64;
		wanted.extend([(36, b), (36, b + 2.0), (38, b + 1.0), (38, b + 3.0)]);
		wanted.extend((0..4).map(|i| (42, b + 0.5 + i as f64)));
	}
	let matched = wanted
		.iter()
		.filter(|&&(p, s)| {
			found
				.iter()
				.any(|&(q, t, _)| q == p && (t - s).abs() <= 0.06)
		})
		.count();
	let precision = matched as f64 / found.len() as f64;
	let recall = matched as f64 / wanted.len() as f64;
	let f = 2.0 * precision * recall / (precision + recall);
	assert!(f >= 0.9, "F {f:.3}: {found:?}");
}

#[tokio::test]
async fn a_rendered_808_line_comes_back_note_for_note() {
	let dir = tempfile::tempdir().unwrap();
	unsafe { std::env::set_var("XDG_CACHE_HOME", dir.path().join("cache")) };
	let mut session = Session::new();
	let bass = tracks::create_track(
		&mut session,
		tracks::CreateTrackArgs {
			name: "808".into(),
			kind: TrackKind::Instrument,
			instrument: Some(DeviceSpec {
				device: "poly_synth".into(),
				params: BTreeMap::new(),
				preset: Some("808 Clean".into()),
			}),
			color: None,
			output: None,
		},
	)
	.unwrap()
	.track_id;
	// (pitch, start beat, length): E1 .. D2 and a B0, 120 bpm.
	let line: [(u8, f64, f64); 8] = [
		(28, 0.0, 1.0),
		(31, 1.0, 0.5),
		(34, 1.5, 0.5),
		(38, 2.0, 1.5),
		(23, 4.0, 1.0),
		(33, 5.0, 0.5),
		(36, 5.5, 0.5),
		(28, 6.0, 1.5),
	];
	clips::add_midi_clip(
		&mut session,
		clips::AddMidiClipArgs {
			track_id: bass,
			start_beat: 0.0,
			length_beats: 8.0,
			name: None,
			notes: line
				.iter()
				.map(|&(p, start, length)| clips::NoteInput {
					pitch: PitchInput(p),
					start,
					length,
					velocity: Some(0.9),
					bend: None,
					pressure: None,
					timbre: None,
				})
				.collect(),
		},
	)
	.unwrap();
	let wav = dir.path().join("808.wav");
	render::render(
		session.document().project().clone(),
		None,
		cadenza_engine::PluginHosting::default(),
		render::RenderArgs {
			path: wav.display().to_string(),
			start_beat: Some(0.0),
			end_beat: Some(9.0),
			sample_rate: Some(44_100),
			..Default::default()
		},
	)
	.await
	.unwrap();
	tracks::delete_track(&mut session, tracks::DeleteTrackArgs { track_id: bass }).unwrap();
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
			algorithm: notes::Algorithm::PitchedPercussive,
			..Default::default()
		},
	)
	.unwrap();
	assert_eq!(out.algorithm, notes::Algorithm::PitchedPercussive);
	let project = session.document().project();
	let track = project.track(out.track_id).unwrap();
	assert_eq!(
		track.instrument.as_ref().and_then(|d| d.builtin_id()),
		Some("poly_synth")
	);
	let found = clip_notes(&session, out.clip_id);
	let got: Vec<u8> = found.iter().map(|n| n.0).collect();
	let want: Vec<u8> = line.iter().map(|n| n.0).collect();
	assert_eq!(got, want, "{found:?} {out:?}");
	// Within 30 ms: a note struck over the last one's release swells in over its attack.
	for (f, w) in found.iter().zip(&line) {
		assert!((f.1 - w.1).abs() <= 0.06, "{found:?}");
	}
}
