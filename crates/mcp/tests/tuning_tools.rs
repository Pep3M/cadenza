//! NE-7b: the tuning engineer's tools on a synthetic take sung sharp in E Phrygian.

use std::path::Path;

use cadenza_core::ClipId;
use cadenza_mcp::Session;
use cadenza_mcp::convert::PitchInput;
use cadenza_mcp::tools::{audio, clips, tracks, tuning};
use cadenza_model::{ClipContent, NoteEdits, TrackKind};
use cadenza_notes::eval::synth::{self, Phrase, SynthNote, Vibrato, Vowel};

/// E F G A G F B E: E Phrygian, the singer 30 cents sharp, vibrato on the last note.
const MELODY: [(f64, f64); 8] = [
	(64.0, 0.5),
	(65.0, 0.4),
	(67.0, 0.5),
	(69.0, 0.6),
	(67.0, 0.4),
	(65.0, 0.4),
	(71.0, 0.6),
	(64.0, 1.0),
];
const SHARP: f64 = 30.0;

fn write_take(dir: &Path) -> std::path::PathBuf {
	let notes = MELODY
		.iter()
		.enumerate()
		.map(|(i, &(p, d))| SynthNote {
			detune_cents: SHARP + if i % 2 == 0 { 6.0 } else { -6.0 },
			vibrato: (i == MELODY.len() - 1).then_some(Vibrato {
				rate_hz: 5.5,
				depth_cents: 40.0,
				delay: 0.2,
			}),
			..SynthNote::new(p, d).rest(0.2)
		})
		.collect();
	let rendered = synth::render(&Phrase {
		notes,
		vowel: Vowel::A,
		sample_rate: 44_100,
		snr_db: None,
		reverb: None,
		seed: 7,
	});
	let path = dir.join("take.wav");
	let spec = hound::WavSpec {
		channels: 1,
		sample_rate: 44_100,
		bits_per_sample: 32,
		sample_format: hound::SampleFormat::Float,
	};
	let mut w = hound::WavWriter::create(&path, spec).unwrap();
	for s in &rendered.samples {
		w.write_sample(*s).unwrap();
	}
	w.finalize().unwrap();
	path
}

fn setup(dir: &Path) -> (Session, ClipId) {
	let mut session = Session::new();
	let path = write_take(dir);
	let clip = audio::import_audio(
		&mut session,
		audio::ImportAudioArgs {
			path: path.display().to_string(),
			track_id: None,
			at_beats: Some(4.0),
			name: Some("Vox".into()),
			gain_db: None,
		},
	)
	.unwrap()
	.clip_id;
	(session, clip)
}

fn notes_of(session: &Session, clip: ClipId) -> NoteEdits {
	let (_, c) = session.document().project().find_clip(clip).unwrap();
	match &c.content {
		ClipContent::Audio { notes: Some(n), .. } => (**n).clone(),
		_ => panic!("no notes"),
	}
}

fn detect(session: &mut Session, clip: ClipId) -> tuning::DetectAudioNotesOutput {
	tuning::detect_audio_notes(
		session,
		tuning::DetectAudioNotesArgs {
			clip_id: clip,
			sensitivity: None,
		},
	)
	.unwrap()
}

fn analyze(session: &Session, clip: ClipId, targets: tuning::TargetArgs) -> tuning::VocalReport {
	tuning::analyze_vocal(
		session,
		tuning::AnalyzeVocalArgs {
			clip_id: clip,
			targets,
		},
	)
	.unwrap()
}

fn tune(clip: ClipId) -> tuning::TuneAudioNotesArgs {
	tuning::TuneAudioNotesArgs {
		clip_id: clip,
		notes: None,
		targets: tuning::TargetArgs::default(),
		overrides: Vec::new(),
		center_pct: None,
		drift_pct: None,
		vibrato_pct: None,
		threshold_cents: None,
	}
}

#[test]
fn the_engineer_reads_the_scale_and_tunes_the_offset_out_keeping_the_vibrato() {
	let dir = tempfile::tempdir().unwrap();
	let (mut session, clip) = setup(dir.path());
	let err = tuning::analyze_vocal(
		&session,
		tuning::AnalyzeVocalArgs {
			clip_id: clip,
			targets: tuning::TargetArgs::default(),
		},
	)
	.unwrap_err()
	.to_string();
	assert!(err.contains("detect_audio_notes"), "{err}");

	let found = detect(&mut session, clip);
	assert_eq!(found.notes, MELODY.len());
	assert!(!found.again);
	assert_eq!(
		*session.document().history_labels().last().unwrap(),
		"Detect notes"
	);

	let report = analyze(&session, clip, tuning::TargetArgs::default());
	assert_eq!(report.scale.as_deref(), Some("E phrygian"), "{report:?}");
	assert!((20.0..=40.0).contains(&report.offset_cents), "{report:?}");
	assert!(report.sung_error_cents > 20.0, "{report:?}");
	assert_eq!(report.on_target, 0);
	assert_eq!(report.range, "E4-B4");

	let listed = tuning::get_audio_notes(
		&session,
		tuning::GetAudioNotesArgs {
			clip_id: clip,
			start_beat: None,
			end_beat: None,
			limit: Some(3),
			targets: tuning::TargetArgs::default(),
		},
	)
	.unwrap();
	assert_eq!(listed.total, MELODY.len());
	assert_eq!(listed.notes.len(), 3);
	assert_eq!(listed.more, MELODY.len() - 3);
	assert_eq!(listed.notes[1].target, "F4");
	assert!(
		listed.notes[0].beat >= 4.0,
		"song beats: {:?}",
		listed.notes[0]
	);

	let tuned = tuning::tune_audio_notes(&mut session, tune(clip)).unwrap();
	assert_eq!(tuned.changed, MELODY.len());
	assert!(tuned.error_after_cents < 1.0, "{tuned:?}");
	assert_eq!(tuned.undo_label.as_deref(), Some("Tune notes"));
	let edits = notes_of(&session, clip);
	for (n, &(p, _)) in edits.notes.iter().zip(&MELODY) {
		assert!((n.result_pitch() - p).abs() < 1e-6, "{n:?}");
		assert_eq!(n.modulation, 1.0, "the vibrato stays");
	}
	let after = analyze(&session, clip, tuning::TargetArgs::default());
	assert_eq!(after.on_target, MELODY.len());
	assert!(after.now_error_cents < 1.0);
	assert_eq!(after.edited, MELODY.len());

	// Nothing left to do: no undo step.
	let again = tuning::tune_audio_notes(&mut session, tune(clip)).unwrap();
	assert_eq!(again.changed, 0);
	assert!(again.undo_label.is_none());
}

#[test]
fn overrides_hand_edits_splits_merges_and_a_reference_melody() {
	let dir = tempfile::tempdir().unwrap();
	let (mut session, clip) = setup(dir.path());
	detect(&mut session, clip);

	// The engineer decides note 2 is an F#, keeps 60 % of the vibrato, tunes at 80 %.
	let tuned = tuning::tune_audio_notes(
		&mut session,
		tuning::TuneAudioNotesArgs {
			overrides: vec![tuning::ToneOverride {
				note: 2,
				pitch: PitchInput(66),
			}],
			center_pct: Some(80.0),
			vibrato_pct: Some(60.0),
			notes: Some(vec![2, 8]),
			..tune(clip)
		},
	)
	.unwrap();
	assert_eq!(tuned.notes_considered, 2);
	let edits = notes_of(&session, clip);
	let two = &edits.notes[1];
	let expected = two.center + 0.8 * (66.0 - two.center);
	assert!((two.result_pitch() - expected).abs() < 1e-6, "{two:?}");
	assert!((edits.notes[7].modulation - 0.6).abs() < 1e-6);
	assert!(edits.notes[0].pitch.is_none(), "not in `notes`");

	let edited = tuning::edit_audio_notes(
		&mut session,
		tuning::EditAudioNotesArgs {
			clip_id: clip,
			edits: vec![
				tuning::NoteEditArgs {
					note: 1,
					pitch: Some(PitchInput(64)),
					cents: Some(-10.0),
					drift_pct: None,
					vibrato_pct: None,
					formant_cents: None,
					gain_db: Some(-3.0),
					as_sung: false,
				},
				tuning::NoteEditArgs {
					note: 2,
					pitch: None,
					cents: None,
					drift_pct: None,
					vibrato_pct: None,
					formant_cents: None,
					gain_db: None,
					as_sung: true,
				},
			],
		},
	)
	.unwrap();
	assert_eq!(edited.changed, 2);
	let edits = notes_of(&session, clip);
	assert!((edits.notes[0].result_pitch() - 63.9).abs() < 1e-6);
	assert_eq!(edits.notes[0].gain_db, -3.0);
	assert!(edits.notes[1].pitch.is_none());
	let bad = tuning::edit_audio_notes(
		&mut session,
		tuning::EditAudioNotesArgs {
			clip_id: clip,
			edits: vec![tuning::NoteEditArgs {
				note: 42,
				pitch: None,
				cents: Some(5.0),
				drift_pct: None,
				vibrato_pct: None,
				formant_cents: None,
				gain_db: None,
				as_sung: false,
			}],
		},
	)
	.unwrap_err()
	.to_string();
	assert!(bad.contains("numbered 1..8"), "{bad}");

	// Split the long last note in two, then join it back.
	let last = &edits.notes[7];
	let project = session.document().project();
	let vocal = tuning::VocalClip::find(project, clip).unwrap();
	let mid = vocal.beat(project, 0.5 * (last.start + last.end));
	let split = tuning::split_audio_note(
		&mut session,
		tuning::SplitAudioNoteArgs {
			clip_id: clip,
			note: 8,
			at_beat: mid,
		},
	)
	.unwrap();
	assert_eq!(split.notes, 9);
	assert!(!split.around.is_empty());
	let merged = tuning::merge_audio_notes(
		&mut session,
		tuning::MergeAudioNotesArgs {
			clip_id: clip,
			note: 8,
		},
	)
	.unwrap();
	assert_eq!(merged.notes, 8);
	let edits = notes_of(&session, clip);
	assert_eq!(edits.notes[0].gain_db, -3.0, "reshaping keeps the edits");

	// A MIDI melody says the 4th note is a G#, not the A sung.
	let track = tracks::create_track(
		&mut session,
		tracks::CreateTrackArgs {
			name: "Melody".into(),
			kind: TrackKind::Instrument,
			instrument: None,
			color: None,
			output: None,
		},
	)
	.unwrap()
	.track_id;
	let project = session.document().project();
	let vocal = tuning::VocalClip::find(project, clip).unwrap();
	let notes: Vec<clips::NoteInput> = edits
		.notes
		.iter()
		.zip(&MELODY)
		.enumerate()
		.map(|(i, (n, &(p, _)))| clips::NoteInput {
			pitch: PitchInput(if i == 3 { 56 } else { p as u8 - 12 }),
			start: vocal.beat(project, n.start) - 4.0,
			length: vocal.beat(project, n.end) - vocal.beat(project, n.start),
			velocity: None,
			bend: None,
			pressure: None,
			timbre: None,
		})
		.collect();
	let reference = clips::add_midi_clip(
		&mut session,
		clips::AddMidiClipArgs {
			track_id: track,
			start_beat: 4.0,
			length_beats: 16.0,
			name: Some("Melody".into()),
			notes,
		},
	)
	.unwrap()
	.clip_id;
	let report = analyze(
		&session,
		clip,
		tuning::TargetArgs {
			reference_clip_id: Some(reference),
			..tuning::TargetArgs::default()
		},
	);
	assert!(
		report.targets_from.starts_with("reference 'Melody'"),
		"{report:?}"
	);
	let listed = tuning::get_audio_notes(
		&session,
		tuning::GetAudioNotesArgs {
			clip_id: clip,
			start_beat: None,
			end_beat: None,
			limit: None,
			targets: tuning::TargetArgs {
				reference_clip_id: Some(reference),
				..tuning::TargetArgs::default()
			},
		},
	)
	.unwrap();
	assert_eq!(listed.notes[3].target, "G#4", "octave of the singer");
	assert_eq!(listed.notes[3].reason, "reference");
	assert_eq!(listed.notes[0].target, "E4");

	// A detection again keeps the edits.
	let again = detect(&mut session, clip);
	assert!(again.again);
	assert!(again.edited_kept >= 1);
	assert_eq!(notes_of(&session, clip).notes[0].gain_db, -3.0);
}
