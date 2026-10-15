//! `render`/`analyze_mix` tool logic: writes a valid WAV with a sane report for a small
//! song, and the mix-observation thresholds trigger on a deliberately hot mix.

use std::collections::BTreeMap;

use cadenza_mcp::Session;
use cadenza_mcp::convert::{DeviceSpec, PitchInput};
use cadenza_mcp::tools::{clips, mix, render, tracks};
use cadenza_model::TrackKind;

fn analysis_input(project: cadenza_model::Project) -> mix::AnalysisInput {
	mix::AnalysisInput {
		project,
		library: std::sync::Arc::new(cadenza_library::Library::core()),
		plugins: None,
		hosting: cadenza_engine::PluginHosting::default(),
	}
}

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
		cadenza_engine::PluginHosting::default(),
		render::RenderArgs {
			path: path.display().to_string(),
			start_beat: None,
			end_beat: Some(2.0),
			sample_rate: Some(48_000),
			..Default::default()
		},
	)
	.await
	.expect("render should succeed")
	.report;

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
		cadenza_engine::PluginHosting::default(),
		render::RenderArgs {
			path: path.display().to_string(),
			start_beat: Some(4.0),
			end_beat: Some(2.0),
			sample_rate: None,
			..Default::default()
		},
	)
	.await
	.unwrap_err();
	assert!(err.to_string().contains("end"), "{err}");
}

#[tokio::test]
async fn render_picks_the_format_from_the_extension() {
	let mut session = Session::new();
	let track_id = instrument_track_with_synth(&mut session);
	add_notes(&mut session, track_id, 0.8);
	let dir = tempfile::tempdir().unwrap();
	for (name, depth) in [("mix.flac", Some(16)), ("mix16.wav", Some(16))] {
		let path = dir.path().join(name);
		render::render(
			session.document().project().clone(),
			None,
			cadenza_engine::PluginHosting::default(),
			render::RenderArgs {
				path: path.display().to_string(),
				end_beat: Some(1.0),
				bit_depth: depth,
				..Default::default()
			},
		)
		.await
		.unwrap();
		let info = cadenza_audio_io::probe(&path).unwrap();
		assert_eq!(info.bits_per_sample, Some(16), "{name}");
		assert_eq!(info.channels, 2);
	}
	let err = render::render(
		session.document().project().clone(),
		None,
		cadenza_engine::PluginHosting::default(),
		render::RenderArgs {
			path: dir.path().join("mix.aiff").display().to_string(),
			..Default::default()
		},
	)
	.await
	.unwrap_err();
	assert!(err.to_string().contains(".wav, .flac or .mp3"), "{err}");
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

	let out = mix::analyze_mix(
		analysis_input(project),
		serde_json::from_value(serde_json::json!({
			"end_beat": 2.0,
			"sample_rate": 48_000,
			"target_lufs": -23.0,
		}))
		.unwrap(),
	)
	.await
	.expect("analyze_mix should succeed");

	assert_eq!(out.targets.lufs, -23.0);
	assert!(
		out.issues
			.iter()
			.any(|i| i.message.contains("LUFS") || i.message.contains("louder")),
		"a +12 dB master with a -23 LUFS target must be flagged: {:?}",
		out.issues
	);
}

#[tokio::test]
async fn analyze_mix_on_silence_has_no_loudness_observations() {
	let mut session = Session::new();
	instrument_track_with_synth(&mut session);
	let project = session.document().project().clone();
	let out = mix::analyze_mix(
		analysis_input(project),
		serde_json::from_value(serde_json::json!({ "end_beat": 1.0 })).unwrap(),
	)
	.await
	.unwrap();
	assert_eq!(out.master.clipped_samples, 0);
	assert!(
		!out.issues.iter().any(|i| i.message.contains("LUFS")),
		"silence is not judged: {:?}",
		out.issues
	);
}

#[tokio::test]
async fn render_takes_bars_normalization_resampling_and_metadata() {
	let mut session = Session::new();
	let track_id = instrument_track_with_synth(&mut session);
	add_notes(&mut session, track_id, 0.8);
	let dir = tempfile::tempdir().unwrap();
	let path = dir.path().join("master.flac");
	let out = render::render(
		session.document().project().clone(),
		None,
		cadenza_engine::PluginHosting::default(),
		render::RenderArgs {
			path: path.display().to_string(),
			start_bar: Some(1),
			end_bar: Some(1),
			output_sample_rate: Some(44_100),
			normalize: Some(render::NormalizeArgs {
				mode: render::NormalizeMode::Peak,
				peak_db: Some(-3.0),
				..Default::default()
			}),
			metadata: Some(render::MetadataArgs {
				title: Some("Bar One".into()),
				..Default::default()
			}),
			..Default::default()
		},
	)
	.await
	.unwrap();
	assert_eq!(out.path, path.display().to_string());
	assert!((out.file.peak_db + 3.0).abs() < 0.15, "{:?}", out.file);
	let info = cadenza_audio_io::probe(&path).unwrap();
	assert_eq!(info.sample_rate, 44_100);
	assert!(
		(info.frames as f64 / 44_100.0 - 2.0).abs() < 1e-3,
		"one 4/4 bar at 120 bpm"
	);
	let comments = cadenza_audio_io::tags::read_flac_comments(&std::fs::read(&path).unwrap());
	assert!(comments.contains(&"TITLE=Bar One".to_string()));

	let err = render::render(
		session.document().project().clone(),
		None,
		cadenza_engine::PluginHosting::default(),
		render::RenderArgs {
			path: path.display().to_string(),
			start_beat: Some(0.0),
			end_bar: Some(2),
			..Default::default()
		},
	)
	.await
	.unwrap_err();
	assert!(err.to_string().contains("not both"), "{err}");
}

#[tokio::test]
async fn export_stems_writes_one_file_per_track_into_renders() {
	let mut session = Session::new();
	let a = instrument_track_with_synth(&mut session);
	add_notes(&mut session, a, 0.8);
	let b = instrument_track_with_synth(&mut session);
	add_notes(&mut session, b, 0.5);
	let dir = tempfile::tempdir().unwrap();
	let renders = dir.path().join("Renders");
	let out = render::export_stems(
		session.document().project().clone(),
		None,
		cadenza_engine::PluginHosting::default(),
		render::ExportStemsArgs {
			include_mixdown: Some(true),
			end_beat: Some(2.0),
			format: Some("wav".into()),
			bit_depth: Some(24),
			..Default::default()
		},
		Some(renders.clone()),
	)
	.await
	.unwrap();
	assert_eq!(out.files.len(), 3);
	assert_eq!(out.files[0].name, "Untitled");
	assert!(
		out.files[1].path.ends_with("Untitled - Synth.wav"),
		"{}",
		out.files[1].path
	);
	assert!(
		out.files[2].path.ends_with("Untitled - Synth (2).wav"),
		"{}",
		out.files[2].path
	);
	assert!(
		out.files
			.iter()
			.all(|f| std::path::Path::new(&f.path).starts_with(&renders))
	);
	assert_eq!(
		out.total_bytes,
		out.files.iter().map(|f| f.bytes).sum::<u64>()
	);

	let err = render::export_stems(
		session.document().project().clone(),
		None,
		cadenza_engine::PluginHosting::default(),
		render::ExportStemsArgs {
			stems: render::StemKind::Selected,
			..Default::default()
		},
		Some(renders),
	)
	.await
	.unwrap_err();
	assert!(err.to_string().contains("no tracks selected"), "{err}");
	let err = render::export_stems(
		session.document().project().clone(),
		None,
		cadenza_engine::PluginHosting::default(),
		Default::default(),
		None,
	)
	.await
	.unwrap_err();
	assert!(err.to_string().contains("folder"), "{err}");
}
