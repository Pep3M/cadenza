//! Export jobs: ranges, stems (sum, pre/post fader, master chain), normalization,
//! resampling, metadata, progress and cancellation.

mod common;

use std::path::Path;

use cadenza_audio_io::{ExportOptions, FlacBitDepth, Metadata, WavSampleFormat, decode_file};
use cadenza_engine::{
	EngineError, ExportSpec, Normalize, RenderOptions, StemSet, export_project, render_offline,
};
use cadenza_model::{Device, Output, Project};
use common::*;

/// Two synths straight to the master, one into a bus (with a gain insert) that also gets
/// a post-fader send from the first track. Linear everywhere.
fn song() -> Project {
	let mut a = synth_track("Keys", &[(60, 0.0, 2.0), (64, 2.0, 2.0)], 4.0);
	let b = synth_track("Bass", &[(36, 0.0, 4.0)], 4.0);
	let mut c = synth_track("Lead", &[(72, 1.0, 2.0)], 4.0);
	let mut fx = bus("FX");
	fx.channel
		.inserts
		.push(Device::builtin("gain", "Gain").with_param("gain", -3.0));
	fx.channel.volume_db = -2.0;
	c.channel.output = Output::Bus(fx.id);
	a.channel.sends.push(send(&fx, -6.0, false));
	a.channel.volume_db = -4.0;
	a.channel.pan = -0.4;
	project(vec![a, b, c, fx])
}

fn spec(dir: &Path) -> ExportSpec {
	ExportSpec {
		end: Some(beats(4.0)),
		tail: false,
		format: ExportOptions::wav(WavSampleFormat::Float32),
		folder: dir.to_path_buf(),
		name: "Song".into(),
		..ExportSpec::default()
	}
}

fn run(p: &Project, s: &ExportSpec) -> Result<cadenza_engine::ExportReport, EngineError> {
	export_project(p, &RenderOptions::default(), s, &mut |_| true)
}

fn channels(path: &Path) -> (Vec<f32>, Vec<f32>) {
	let b = decode_file(path).unwrap();
	(b.channel(0).to_vec(), b.channel(1).to_vec())
}

#[test]
fn bus_stems_sum_to_the_mixdown() {
	let dir = tempfile::tempdir().unwrap();
	let p = song();
	let report = run(
		&p,
		&ExportSpec {
			stems: StemSet::Buses,
			..spec(dir.path())
		},
	)
	.unwrap();
	let names: Vec<&str> = report.files.iter().map(|f| f.name.as_str()).collect();
	assert_eq!(
		names,
		["Song", "Keys", "Bass", "FX"],
		"Lead feeds the FX bus"
	);
	let (ml, mr) = channels(&report.files[0].path);
	assert_eq!(ml.len(), 4 * 24_000, "exactly 4 beats at 120 bpm");
	let mut sl = vec![0.0f32; ml.len()];
	let mut sr = vec![0.0f32; ml.len()];
	for f in &report.files[1..] {
		let (l, r) = channels(&f.path);
		assert_eq!(l.len(), ml.len(), "{}", f.name);
		for i in 0..l.len() {
			sl[i] += l[i];
			sr[i] += r[i];
		}
	}
	let diff = ml
		.iter()
		.zip(&sl)
		.chain(mr.iter().zip(&sr))
		.fold(0.0f32, |m, (a, b)| m.max((a - b).abs()));
	assert!(diff < 1e-5, "stems differ from the mix by {diff}");
	assert!(peak(&ml) > 0.05);
	assert_eq!(
		report.files[0].path,
		dir.path().join("Song.wav"),
		"{:?}",
		report.files
	);
	assert_eq!(report.files[1].path, dir.path().join("Song - Keys.wav"));
	let leftovers: Vec<_> = std::fs::read_dir(dir.path())
		.unwrap()
		.map(|e| e.unwrap().file_name())
		.filter(|n| n.to_string_lossy().starts_with('.'))
		.collect();
	assert!(leftovers.is_empty(), "temp files removed: {leftovers:?}");
}

#[test]
fn track_stems_post_fader_pre_fader_and_through_the_master() {
	let dir = tempfile::tempdir().unwrap();
	let mut p = song();
	p.tracks[1].channel.volume_db = -6.0;
	let base = ExportSpec {
		mixdown: false,
		stems: StemSet::Selected(vec![p.tracks[1].id]),
		..spec(dir.path())
	};
	let post = run(&p, &base).unwrap();
	assert_eq!(post.files.len(), 1);
	let post_peak = post.files[0].stats.peak_db;

	let pre = run(
		&p,
		&ExportSpec {
			stems_pre_fader: true,
			name: "Pre".into(),
			..base.clone()
		},
	)
	.unwrap();
	let pre_peak = pre.files[0].stats.peak_db;
	assert!(
		(pre_peak - post_peak - 6.0).abs() < 0.05,
		"{pre_peak} vs {post_peak}"
	);

	p.master
		.inserts
		.push(Device::builtin("gain", "Gain").with_param("gain", 6.0));
	let through = run(
		&p,
		&ExportSpec {
			stems_through_master: true,
			name: "Master".into(),
			..base.clone()
		},
	)
	.unwrap();
	let t_peak = through.files[0].stats.peak_db;
	assert!(
		(t_peak - post_peak - 6.0).abs() < 0.1,
		"{t_peak} vs {post_peak}"
	);
	let (tl, _) = channels(&through.files[0].path);
	let (pl, _) = channels(&post.files[0].path);
	assert_eq!(tl.len(), pl.len(), "same length through the master");

	let tracks = run(
		&p,
		&ExportSpec {
			stems: StemSet::Tracks,
			name: "All".into(),
			..base
		},
	)
	.unwrap();
	let names: Vec<&str> = tracks.files.iter().map(|f| f.name.as_str()).collect();
	assert_eq!(names, ["Keys", "Bass", "Lead"], "buses are not track stems");
}

#[test]
fn ranges_and_tails() {
	let dir = tempfile::tempdir().unwrap();
	let p = song();
	let r = run(
		&p,
		&ExportSpec {
			start: beats(1.0),
			end: Some(beats(3.0)),
			..spec(dir.path())
		},
	)
	.unwrap();
	assert_eq!(r.files[0].duration_seconds, 1.0);
	let (l, _) = channels(&r.files[0].path);
	let same = render_offline(
		&p,
		&RenderOptions {
			start: beats(1.0),
			..opts(3.0)
		},
	)
	.unwrap();
	assert_eq!(same.len(), l.len());
	let d = l
		.iter()
		.zip(&same.left)
		.fold(0.0f32, |m, (a, b)| m.max((a - b).abs()));
	assert!(
		d < 1e-6,
		"the range renders like an offline render of that range: {d}"
	);

	let mut wet = song();
	wet.master.inserts.push(Device::builtin("reverb", "Reverb"));
	let with_tail = run(
		&wet,
		&ExportSpec {
			end: None,
			tail: true,
			name: "Tail".into(),
			..spec(dir.path())
		},
	)
	.unwrap();
	let without = run(
		&wet,
		&ExportSpec {
			end: None,
			tail: false,
			name: "NoTail".into(),
			..spec(dir.path())
		},
	)
	.unwrap();
	assert_eq!(without.files[0].duration_seconds, 2.0);
	assert!(with_tail.files[0].duration_seconds > 2.5);
	let err = run(
		&p,
		&ExportSpec {
			start: beats(3.0),
			end: Some(beats(3.0)),
			..spec(dir.path())
		},
	)
	.unwrap_err();
	assert!(err.to_string().contains("empty"), "{err}");
}

#[test]
fn normalization_resampling_formats_and_metadata() {
	let dir = tempfile::tempdir().unwrap();
	let p = song();
	let peak = run(
		&p,
		&ExportSpec {
			normalize: Normalize::Peak { peak_db: -1.0 },
			name: "Peak".into(),
			..spec(dir.path())
		},
	)
	.unwrap();
	assert!(
		(peak.files[0].stats.peak_db + 1.0).abs() < 0.01,
		"{:?}",
		peak.files[0].stats
	);

	let loud = run(
		&p,
		&ExportSpec {
			normalize: Normalize::Loudness {
				lufs: -20.0,
				true_peak_db: -1.0,
			},
			sample_rate: Some(44_100),
			format: ExportOptions::flac(FlacBitDepth::Bits24),
			metadata: Metadata {
				title: "Song".into(),
				artist: "Tests".into(),
				..Metadata::default()
			},
			stems: StemSet::Buses,
			name: "Loud".into(),
			..spec(dir.path())
		},
	)
	.unwrap();
	let mix = &loud.files[0];
	assert_eq!(mix.sample_rate, 44_100);
	assert!((mix.duration_seconds - 2.0).abs() < 1e-3);
	assert!(
		(mix.stats.lufs_integrated + 20.0).abs() < 0.3,
		"{:?}",
		mix.stats
	);
	assert!(mix.stats.true_peak_db <= -0.9);
	let decoded = decode_file(&mix.path).unwrap();
	assert_eq!(decoded.sample_rate(), 44_100);
	let bytes = std::fs::read(&loud.files[1].path).unwrap();
	assert!(
		cadenza_audio_io::tags::read_flac_comments(&bytes)
			.contains(&"TITLE=Song - Keys".to_string())
	);
	// Linked normalization: every stem gets the mix gain.
	assert!(loud.files.iter().all(|f| f.gain_db == mix.gain_db));

	// A ceiling below the target's needs limits the gain and says so.
	let capped = run(
		&p,
		&ExportSpec {
			normalize: Normalize::Loudness {
				lufs: -3.0,
				true_peak_db: -1.0,
			},
			name: "Capped".into(),
			..spec(dir.path())
		},
	)
	.unwrap();
	assert!((capped.files[0].stats.true_peak_db + 1.0).abs() < 0.1);
	assert!(capped.notes[0].contains("ceiling"), "{:?}", capped.notes);
}

#[test]
fn progress_is_monotonic_and_cancel_leaves_nothing_behind() {
	let dir = tempfile::tempdir().unwrap();
	let p = song();
	let s = ExportSpec {
		stems: StemSet::Tracks,
		..spec(dir.path())
	};
	let mut seen = Vec::new();
	export_project(&p, &RenderOptions::default(), &s, &mut |pr| {
		seen.push(pr.fraction);
		true
	})
	.unwrap();
	assert!(seen.len() > 5);
	assert!(seen.windows(2).all(|w| w[1] >= w[0]), "{seen:?}");
	assert_eq!(*seen.last().unwrap(), 1.0);

	let out = tempfile::tempdir().unwrap();
	for stop_at in [2, seen.len() - 2] {
		let mut calls = 0;
		let err = export_project(
			&p,
			&RenderOptions::default(),
			&ExportSpec {
				folder: out.path().to_path_buf(),
				..s.clone()
			},
			&mut |_| {
				calls += 1;
				calls < stop_at
			},
		)
		.unwrap_err();
		assert!(matches!(err, EngineError::Cancelled), "{err}");
		let left: Vec<_> = std::fs::read_dir(out.path()).unwrap().collect();
		assert!(left.is_empty(), "cancel at {stop_at} left {left:?}");
	}

	let err = run(
		&p,
		&ExportSpec {
			mixdown: false,
			..spec(dir.path())
		},
	)
	.unwrap_err();
	assert!(err.to_string().contains("nothing to export"));
	let err = run(
		&p,
		&ExportSpec {
			stems: StemSet::Selected(vec![cadenza_core::TrackId::new()]),
			..spec(dir.path())
		},
	)
	.unwrap_err();
	assert!(err.to_string().contains("does not exist"));
}
