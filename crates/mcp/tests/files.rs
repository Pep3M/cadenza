//! Audio and MIDI file tools: import_audio, get_audio_info, set_clip_fades, import_midi,
//! export_midi.

use std::path::Path;

use cadenza_mcp::Session;
use cadenza_mcp::tools::{audio, midi};
use cadenza_model::{ClipFade, FadeCurve, TrackKind};

fn write_wav(path: &Path, sample_rate: u32, channels: u16, seconds: f64) {
	let spec = hound::WavSpec {
		channels,
		sample_rate,
		bits_per_sample: 16,
		sample_format: hound::SampleFormat::Int,
	};
	let mut w = hound::WavWriter::create(path, spec).unwrap();
	for i in 0..(sample_rate as f64 * seconds) as usize {
		let v = ((i as f64 * 0.05).sin() * 8000.0) as i16;
		for _ in 0..channels {
			w.write_sample(v).unwrap();
		}
	}
	w.finalize().unwrap();
}

#[test]
fn import_audio_creates_a_track_and_reports_the_file() {
	let dir = tempfile::tempdir().unwrap();
	let wav = dir.path().join("Guitar Take.wav");
	write_wav(&wav, 44_100, 2, 3.0);
	let mut session = Session::new();
	let out = audio::import_audio(
		&mut session,
		audio::ImportAudioArgs {
			path: wav.display().to_string(),
			at_beats: Some(4.0),
			..Default::default()
		},
	)
	.unwrap();
	assert!(out.created_track);
	// 3 s at 120 bpm = 6 beats.
	assert_eq!(out.length_beats, 6.0);
	assert_eq!(out.start_beat, 4.0);
	assert_eq!(out.file.sample_rate, 44_100);
	assert_eq!(out.file.channels, 2);
	assert_eq!(out.file.duration_seconds, 3.0);
	assert_eq!(out.file.format, "wav");
	let project = session.document().project();
	let track = project.track(out.track_id).unwrap();
	assert_eq!(track.kind, TrackKind::Audio);
	assert_eq!(track.name, "Guitar Take");
	// One undo removes track and clip.
	session.document_mut().undo().unwrap();
	assert!(session.document().project().tracks.is_empty());
	session.document_mut().redo().unwrap();

	// A second import onto the same track.
	let again = audio::import_audio(
		&mut session,
		audio::ImportAudioArgs {
			path: wav.display().to_string(),
			track_id: Some(out.track_id),
			at_beats: Some(12.0),
			gain_db: Some(-3.0),
			..Default::default()
		},
	)
	.unwrap();
	assert!(!again.created_track);
	assert_eq!(
		session
			.document()
			.project()
			.track(out.track_id)
			.unwrap()
			.clips
			.len(),
		2
	);

	let info = audio::get_audio_info(
		&session,
		audio::GetAudioInfoArgs {
			clip_id: Some(again.clip_id),
			..Default::default()
		},
	)
	.unwrap();
	let clip = info.clip.unwrap();
	assert_eq!(clip.gain_db, -3.0);
	assert_eq!(clip.length_seconds, 3.0);
}

#[test]
fn import_audio_errors_are_actionable() {
	let dir = tempfile::tempdir().unwrap();
	let mut session = Session::new();
	let missing = audio::import_audio(
		&mut session,
		audio::ImportAudioArgs {
			path: dir.path().join("nope.wav").display().to_string(),
			..Default::default()
		},
	)
	.unwrap_err()
	.to_string();
	assert!(missing.contains("existing file"), "{missing}");
	let junk = dir.path().join("junk.wav");
	std::fs::write(&junk, b"not audio").unwrap();
	let unsupported = audio::import_audio(
		&mut session,
		audio::ImportAudioArgs {
			path: junk.display().to_string(),
			..Default::default()
		},
	)
	.unwrap_err()
	.to_string();
	assert!(unsupported.contains("junk.wav"), "{unsupported}");
	assert!(!session.document().can_undo());
}

#[test]
fn set_clip_fades_accepts_seconds_and_beats() {
	let dir = tempfile::tempdir().unwrap();
	let wav = dir.path().join("pad.wav");
	write_wav(&wav, 48_000, 1, 4.0);
	let mut session = Session::new();
	let clip = audio::import_audio(
		&mut session,
		audio::ImportAudioArgs {
			path: wav.display().to_string(),
			..Default::default()
		},
	)
	.unwrap()
	.clip_id;
	let out = audio::set_clip_fades(
		&mut session,
		audio::SetClipFadesArgs {
			clip_id: clip,
			fade_in: Some(audio::FadeInput {
				seconds: Some(0.005),
				..Default::default()
			}),
			// 2 beats at 120 bpm = 1 s.
			fade_out: Some(audio::FadeInput {
				beats: Some(2.0),
				curve: Some(FadeCurve::Exponential),
				..Default::default()
			}),
		},
	)
	.unwrap();
	assert_eq!(out.fade_in, Some(ClipFade::new(0.005, FadeCurve::Linear)));
	assert_eq!(
		out.fade_out,
		Some(ClipFade::new(1.0, FadeCurve::Exponential))
	);
	assert_eq!(out.clip_length_seconds, 4.0);

	// Only change the curve of the fade-in; the fade-out is kept.
	let out = audio::set_clip_fades(
		&mut session,
		audio::SetClipFadesArgs {
			clip_id: clip,
			fade_in: Some(audio::FadeInput {
				curve: Some(FadeCurve::SCurve),
				..Default::default()
			}),
			fade_out: None,
		},
	)
	.unwrap();
	assert_eq!(out.fade_in, Some(ClipFade::new(0.005, FadeCurve::SCurve)));
	assert_eq!(out.fade_out.unwrap().seconds, 1.0);

	let err = audio::set_clip_fades(
		&mut session,
		audio::SetClipFadesArgs {
			clip_id: clip,
			fade_in: Some(audio::FadeInput {
				seconds: Some(3.5),
				..Default::default()
			}),
			fade_out: None,
		},
	)
	.unwrap_err()
	.to_string();
	assert!(err.contains("must not exceed"), "{err}");

	// 0 removes.
	let out = audio::set_clip_fades(
		&mut session,
		audio::SetClipFadesArgs {
			clip_id: clip,
			fade_in: Some(audio::FadeInput {
				seconds: Some(0.0),
				..Default::default()
			}),
			fade_out: Some(audio::FadeInput {
				beats: Some(0.0),
				..Default::default()
			}),
		},
	)
	.unwrap();
	assert_eq!((out.fade_in, out.fade_out), (None, None));
}

#[test]
fn midi_export_then_import_round_trips_and_undoes_in_one_step() {
	let dir = tempfile::tempdir().unwrap();
	let mut session = Session::new();
	let track = cadenza_mcp::tools::tracks::create_track(
		&mut session,
		cadenza_mcp::tools::tracks::CreateTrackArgs {
			name: "Keys".into(),
			kind: TrackKind::Instrument,
			instrument: None,
			color: None,
			output: None,
		},
	)
	.unwrap()
	.track_id;
	cadenza_mcp::tools::clips::add_midi_clip(
		&mut session,
		cadenza_mcp::tools::clips::AddMidiClipArgs {
			track_id: track,
			start_beat: 0.0,
			length_beats: 4.0,
			name: None,
			notes: (0..4)
				.map(|i| cadenza_mcp::tools::clips::NoteInput {
					pitch: cadenza_mcp::convert::PitchInput(60 + i),
					start: f64::from(i),
					length: 1.0,
					velocity: Some(0.8),
				})
				.collect(),
		},
	)
	.unwrap();
	let mid = dir.path().join("keys.mid");
	let exported = midi::export_midi(
		&session,
		midi::ExportMidiArgs {
			path: mid.display().to_string(),
			..Default::default()
		},
	)
	.unwrap();
	assert_eq!((exported.tracks, exported.notes), (1, 4));
	let bad = midi::export_midi(
		&session,
		midi::ExportMidiArgs {
			path: dir.path().join("keys.txt").display().to_string(),
			..Default::default()
		},
	)
	.unwrap_err()
	.to_string();
	assert!(bad.contains(".mid"), "{bad}");

	let mut fresh = Session::new();
	let imported = midi::import_midi(
		&mut fresh,
		midi::ImportMidiArgs {
			path: mid.display().to_string(),
			..Default::default()
		},
	)
	.unwrap();
	assert_eq!(imported.created_tracks.len(), 1);
	assert_eq!(imported.created_tracks[0].note_count, 4);
	assert_eq!(fresh.document().project().tracks.len(), 1);
	fresh.document_mut().undo().unwrap();
	assert!(fresh.document().project().tracks.is_empty());

	// Into an existing track at beat 8.
	let into = midi::import_midi(
		&mut session,
		midi::ImportMidiArgs {
			path: mid.display().to_string(),
			track_id: Some(track),
			at_beats: Some(8.0),
			..Default::default()
		},
	)
	.unwrap();
	assert!(into.created_tracks.is_empty());
	let clip = into.clip.unwrap();
	assert_eq!(clip.note_count, 4);
	let project = session.document().project();
	let (_, c) = project.find_clip(clip.clip_id).unwrap();
	assert_eq!(c.start.beats(), 8.0);
}
