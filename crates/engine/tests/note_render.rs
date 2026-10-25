//! Audio clips with note edits play their rendered copy (NE-5): made on the spot by
//! offline renders, picked up by the realtime player once it exists, identical in both.

mod common;

use cadenza_core::AudioNoteId;
use cadenza_engine::Player;
use cadenza_engine::note_render::{self, PENDING_WARNING};
use cadenza_model::{Clip, ClipContent, Track, TrackKind};
use cadenza_notes::analysis::{AnalysisOptions, analyze};
use cadenza_notes::eval::synth;
use cadenza_notes::segment::{SegmentOptions, segment};
use common::*;

const SR: u32 = 48_000;

#[test]
fn edited_clips_play_their_render_offline_and_live() {
	let dir = tempfile::tempdir().unwrap();
	// Renders and analyses go to the user cache: keep them inside the test.
	unsafe { std::env::set_var("XDG_CACHE_HOME", dir.path().join("cache")) };
	let case = synth::corpus(SR).remove(1);
	let wav = dir.path().join("voice.wav");
	let spec = hound::WavSpec {
		channels: 1,
		sample_rate: SR,
		bits_per_sample: 32,
		sample_format: hound::SampleFormat::Float,
	};
	let mut w = hound::WavWriter::create(&wav, spec).unwrap();
	for s in &case.samples {
		w.write_sample(*s).unwrap();
	}
	w.finalize().unwrap();

	let analysis = analyze(&case.samples, SR, &AnalysisOptions::default(), &mut |_| {
		true
	})
	.unwrap();
	let mut notes = cadenza_notes::clip::detect(&analysis, 1.0, None, &mut AudioNoteId::new);
	for n in &mut notes.notes {
		n.pitch = Some(n.center + 2.0);
	}
	let seconds = case.samples.len() as f64 / f64::from(SR);
	let mut clip = Clip::audio("voice", beats(0.0), beats(seconds * 2.0), &wav);
	if let ClipContent::Audio { notes: slot, .. } = &mut clip.content {
		*slot = Some(Box::new(notes.clone()));
	}
	let mut track = Track::new("Vox", TrackKind::Audio);
	track.clips.push(clip.clone());
	let p = project(vec![track]);
	assert_eq!(note_render::pending(&p).len(), 1);

	// Live before the render exists: the clip plays as recorded, with a warning.
	let (mut player, _) = Player::virtual_device(SR, 0);
	let warnings = player.load(&p).unwrap();
	assert!(
		warnings.iter().any(|w| w.contains(PENDING_WARNING)),
		"{warnings:?}"
	);
	drop(player);

	// An offline render makes it.
	let end = seconds * 2.0;
	let offline = cadenza_engine::render_offline(&p, &opts(end)).unwrap();
	assert!(note_render::pending(&p).is_empty());
	let path = note_render::ready(&wav, &notes).unwrap();

	// It is exactly a clip of the rendered file...
	let mut direct = clip.clone();
	direct.content = ClipContent::Audio {
		file: path,
		offset_seconds: 0.0,
		gain_db: 0.0,
		fade_in: None,
		fade_out: None,
		notes: None,
	};
	let mut t = Track::new("Vox", TrackKind::Audio);
	t.clips.push(direct);
	let plain = cadenza_engine::render_offline(&project(vec![t]), &opts(end)).unwrap();
	assert_eq!(max_diff(&offline, &plain), 0.0);

	// ...with every note two semitones up.
	let rendered = analyze(&offline.left, SR, &AnalysisOptions::default(), &mut |_| {
		true
	})
	.unwrap();
	let found = segment(&rendered, &SegmentOptions::default(), &[]);
	for n in &notes.notes {
		let target = n.center + 2.0;
		assert!(
			found
				.notes
				.iter()
				.any(|f| (f.center - target).abs() * 100.0 < 5.0 && f.core_end > n.core_start),
			"{n:?} not at {target}"
		);
	}

	// Live once it exists: no warning, and the same samples.
	let (mut player, mut dev) = Player::virtual_device(SR, 0);
	let warnings = player.load(&p).unwrap();
	assert!(
		!warnings.iter().any(|w| w.contains(PENDING_WARNING)),
		"{warnings:?}"
	);
	player.play().unwrap();
	let mut live = Vec::new();
	let mut out = vec![0.0f32; 2 * 256];
	while live.len() < SR as usize {
		dev.process(&mut out, &[]);
		live.extend(out.chunks(2).map(|f| f[0]));
	}
	let n = SR as usize;
	let diff = offline.left[..n]
		.iter()
		.zip(&live[..n])
		.fold(0.0f32, |m, (a, b)| m.max((a - b).abs()));
	assert!(diff < 1e-5, "offline and live differ by {diff}");

	// Undoing the edit (no edits) plays the file itself again.
	let mut undone = p.clone();
	if let ClipContent::Audio { notes: Some(e), .. } = &mut undone.tracks[0].clips[0].content {
		for n in &mut e.notes {
			n.pitch = None;
		}
	}
	assert!(note_render::pending(&undone).is_empty());
	let original = cadenza_engine::render_offline(&undone, &opts(end)).unwrap();
	let mut t = Track::new("Vox", TrackKind::Audio);
	t.clips
		.push(Clip::audio("voice", beats(0.0), beats(end), &wav));
	let as_recorded = cadenza_engine::render_offline(&project(vec![t]), &opts(end)).unwrap();
	assert_eq!(max_diff(&original, &as_recorded), 0.0);
}
