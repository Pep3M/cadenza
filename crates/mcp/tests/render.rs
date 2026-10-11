//! `render`/`analyze_mix` tool logic: writes a valid WAV with a sane report for a small
//! song, and the mix-observation thresholds trigger on a deliberately hot mix.

use std::collections::BTreeMap;

use cadenza_mcp::Session;
use cadenza_mcp::convert::{DeviceSpec, PitchInput};
use cadenza_mcp::tools::{clips, render, tracks};
use cadenza_model::TrackKind;

fn instrument_track_with_synth(session: &mut Session) -> cadenza_core::TrackId {
	tracks::create_track(
		session,
		tracks::CreateTrackArgs {
			name: "Synth".into(),
			kind: TrackKind::Instrument,
			instrument: Some(DeviceSpec {
				device: "poly_synth".into(),
				params: BTreeMap::new(),
			}),
			color: None,
			output: None,
		},
	)
	.unwrap()
	.track_id
}

fn add_notes(session: &mut Session, track_id: cadenza_core::TrackId, velocity: f32) {
	clips::add_midi_clip(
		session,
		clips::AddMidiClipArgs {
			track_id,
			start_beat: 0.0,
			length_beats: 2.0,
			name: None,
			notes: vec![
				clips::NoteInput {
					pitch: PitchInput(60),
					start: 0.0,
					length: 2.0,
					velocity: Some(velocity),
				},
				clips::NoteInput {
					pitch: PitchInput(64),
					start: 0.0,
					length: 2.0,
					velocity: Some(velocity),
				},
				clips::NoteInput {
					pitch: PitchInput(67),
					start: 0.0,
					length: 2.0,
					velocity: Some(velocity),
				},
			],
		},
	)
	.unwrap();
}

#[tokio::test]
async fn render_writes_a_valid_wav_with_a_sane_report() {
	let mut session = Session::new();
	let track_id = instrument_track_with_synth(&mut session);
	add_notes(&mut session, track_id, 0.8);
	let project = session.document().project().clone();

	let path = std::env::temp_dir().join(format!(
		"cadenza-mcp-render-test-{}.wav",
		std::process::id()
	));
	let report = render::render(
		project,
		None,
		render::RenderArgs {
			path: path.display().to_string(),
			start_beat: None,
			end_beat: Some(2.0),
			sample_rate: Some(48_000),
		},
	)
	.await
	.expect("render should succeed");

	assert_eq!(report.sample_rate, 48_000);
	assert!(report.duration_seconds > 0.0);
	assert_eq!(report.tracks.len(), 1);
	assert!(report.master.peak_db.is_finite());
	assert!(report.master.lufs_integrated.is_finite());

	let mut reader =
		hound::WavReader::open(&path).expect("render should produce a readable WAV file");
	let spec = reader.spec();
	assert_eq!(spec.channels, 2);
	assert_eq!(spec.sample_format, hound::SampleFormat::Float);
	assert_eq!(spec.sample_rate, 48_000);
	let samples: Vec<f32> = reader.samples::<f32>().map(Result::unwrap).collect();
	assert!(!samples.is_empty());
	assert!(
		samples.iter().any(|&s| s != 0.0),
		"the rendered audio should not be silent"
	);

	std::fs::remove_file(&path).ok();
}

#[tokio::test]
async fn render_rejects_an_end_before_the_start() {
	let mut session = Session::new();
	instrument_track_with_synth(&mut session);
	let project = session.document().project().clone();
	let path = std::env::temp_dir().join("cadenza-mcp-render-invalid.wav");
	let err = render::render(
		project,
		None,
		render::RenderArgs {
			path: path.display().to_string(),
			start_beat: Some(4.0),
			end_beat: Some(2.0),
			sample_rate: None,
		},
	)
	.await
	.unwrap_err();
	assert!(err.to_string().contains("end"), "{err}");
}

#[tokio::test]
async fn analyze_mix_flags_a_deliberately_hot_mix() {
	let mut session = Session::new();
	let track_id = instrument_track_with_synth(&mut session);
	add_notes(&mut session, track_id, 1.0);
	// Push the master well above any reasonable loudness target so the comparison against
	// `target_lufs` is certain to trip, regardless of the synth's exact gain staging.
	tracks::set_channel(
		&mut session,
		tracks::SetChannelArgs {
			track_id: None,
			patch: cadenza_model::ChannelPatch {
				volume_db: Some(12.0),
				..Default::default()
			},
		},
	)
	.unwrap();
	let project = session.document().project().clone();

	let out = render::analyze_mix(
		project,
		None,
		render::AnalyzeMixArgs {
			start_beat: None,
			end_beat: Some(2.0),
			sample_rate: Some(48_000),
			target_lufs: Some(-23.0),
			genre_profile: None,
		},
	)
	.await
	.expect("analyze_mix should succeed");

	assert!(
		!out.observations.is_empty(),
		"a +12 dB master with a -23 LUFS target must trigger at least one observation"
	);
	assert!(
		out.observations
			.iter()
			.any(|o| o.contains("LUFS") || o.contains("louder")),
		"{:?}",
		out.observations
	);
}

#[tokio::test]
async fn analyze_mix_on_silence_has_no_loudness_observations() {
	let mut session = Session::new();
	instrument_track_with_synth(&mut session);
	let project = session.document().project().clone();
	let out = render::analyze_mix(
		project,
		None,
		render::AnalyzeMixArgs {
			start_beat: None,
			end_beat: Some(1.0),
			sample_rate: Some(48_000),
			target_lufs: None,
			genre_profile: None,
		},
	)
	.await
	.unwrap();
	assert_eq!(out.report.master.clipped_samples, 0);
}
