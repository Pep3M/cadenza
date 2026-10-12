//! Audio clip playback: WAV formats, resampling, offset, length, gain, missing files.

mod common;

use cadenza_engine::{analyze_mix, render_offline};
use cadenza_model::{Clip, ClipContent, ClipFade, FadeCurve, Track, TrackKind};
use common::*;
use hound::SampleFormat;

fn audio_track(clip: Clip) -> Track {
	let mut t = Track::new("audio", TrackKind::Audio);
	t.clips.push(clip);
	t
}

/// Counts upward zero crossings to estimate frequency.
fn frequency(x: &[f32], sr: f64) -> f64 {
	let crossings = x.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count();
	crossings as f64 * sr / x.len() as f64
}

#[test]
fn mono_44k_clip_is_resampled_positioned_and_panned() {
	let path = write_sine_wav(
		"mono44.wav",
		44_100,
		1,
		(SampleFormat::Int, 16),
		441.0,
		0.5,
		2.0,
	);
	// Clip at beat 1 (0.5 s), 1 beat long (0.5 s), starting 0.25 s into the file, -6 dB.
	let mut clip = Clip::audio("c", beats(1.0), beats(1.0), &path);
	clip.content = ClipContent::Audio {
		file: path,
		offset_seconds: 0.25,
		gain_db: -6.0,
		fade_in: None,
		fade_out: None,
	};
	let out = render_offline(&project(vec![audio_track(clip)]), &opts(4.0)).unwrap();
	assert!(
		out.left[..24_000].iter().all(|x| *x == 0.0),
		"silent before the clip"
	);
	assert!(
		out.left[48_000..].iter().all(|x| *x == 0.0),
		"silent after the clip end"
	);
	let body = &out.left[24_100..47_900];
	assert!((frequency(body, 48_000.0) - 441.0).abs() < 3.0);
	// 0.5 amplitude * -6 dB * -3 dB center pan law (mono source).
	let expected = 0.5 * cadenza_core::db_to_gain(-6.0) * std::f32::consts::FRAC_1_SQRT_2;
	assert!((peak(body) - expected).abs() < 0.01, "peak {}", peak(body));
	assert_eq!(out.left, out.right);
	// offset 0.25 s = 11025 source samples: the sine phase at the clip start is sin(2*pi*441*0.25).
	let phase = (2.0 * std::f64::consts::PI * 441.0 * 0.25).sin() as f32 * expected;
	assert!((out.left[24_000] - phase).abs() < 0.01);
}

#[test]
fn stereo_24bit_and_float_files_load() {
	for (name, format, bits) in [
		("s24.wav", SampleFormat::Int, 24),
		("s32.wav", SampleFormat::Int, 32),
		("f32.wav", SampleFormat::Float, 32),
	] {
		let path = write_sine_wav(name, 96_000, 2, (format, bits), 1_000.0, 0.25, 1.0);
		let clip = Clip::audio("c", beats(0.0), beats(2.0), path);
		let out = render_offline(&project(vec![audio_track(clip)]), &opts(2.0)).unwrap();
		let body = &out.right[1_000..47_000];
		// Stereo source: center pan is unity (balance law).
		assert!((peak(body) - 0.25).abs() < 0.005, "{name}: {}", peak(body));
		assert!((frequency(body, 48_000.0) - 1_000.0).abs() < 3.0, "{name}");
	}
}

#[test]
fn missing_file_warns_and_renders_silence() {
	let clip = Clip::audio("c", beats(0.0), beats(2.0), tmp_path("does_not_exist.wav"));
	let p = project(vec![audio_track(clip)]);
	let out = render_offline(&p, &opts(2.0)).unwrap();
	assert!(out.left.iter().all(|x| *x == 0.0));
	let report = analyze_mix(&p, &opts(2.0)).unwrap();
	assert_eq!(report.warnings.len(), 1);
	assert!(
		report.warnings[0].contains("does_not_exist.wav"),
		"{}",
		report.warnings[0]
	);
}

fn with_fades(mut clip: Clip, fade_in: Option<ClipFade>, fade_out: Option<ClipFade>) -> Clip {
	if let ClipContent::Audio {
		fade_in: fi,
		fade_out: fo,
		..
	} = &mut clip.content
	{
		*fi = fade_in;
		*fo = fade_out;
	}
	clip
}

#[test]
fn fades_shape_the_clip_edges() {
	let path = write_sine_wav(
		"fades.wav",
		48_000,
		2,
		(SampleFormat::Float, 32),
		1_000.0,
		0.5,
		2.0,
	);
	// 2 beats at 120 bpm = 1 s; 0.25 s linear fade-in, 0.5 s equal-power fade-out.
	let clip = with_fades(
		Clip::audio("c", beats(0.0), beats(2.0), &path),
		Some(ClipFade::new(0.25, FadeCurve::Linear)),
		Some(ClipFade::new(0.5, FadeCurve::EqualPower)),
	);
	let out = render_offline(&project(vec![audio_track(clip)]), &opts(2.0)).unwrap();
	let start = peak(&out.left[0..48]);
	let mid_in = peak(&out.left[5_760..6_240]); // around 0.125 s: half the fade-in
	let body = peak(&out.left[12_500..23_500]);
	let tail = peak(&out.left[47_000..48_000]);
	assert!(start < 0.003, "fade-in starts silent: {start}");
	assert!(
		(mid_in - 0.25).abs() < 0.02,
		"linear fade-in is halfway: {mid_in}"
	);
	assert!((body - 0.5).abs() < 0.01, "body at full level: {body}");
	assert!(tail < 0.05, "fade-out ends near silence: {tail}");
}

#[test]
fn overlapping_clips_on_one_track_sum() {
	let a = write_sine_wav(
		"ovl_a.wav",
		48_000,
		1,
		(SampleFormat::Float, 32),
		500.0,
		0.25,
		1.0,
	);
	let mut t = Track::new("audio", TrackKind::Audio);
	t.clips.push(Clip::audio("a", beats(0.0), beats(2.0), &a));
	t.clips.push(Clip::audio("b", beats(0.0), beats(2.0), &a));
	let both = render_offline(&project(vec![t]), &opts(2.0)).unwrap();
	let single = render_offline(
		&project(vec![audio_track(Clip::audio(
			"a",
			beats(0.0),
			beats(2.0),
			&a,
		))]),
		&opts(2.0),
	)
	.unwrap();
	for i in (0..48_000).step_by(997) {
		assert!((both.left[i] - 2.0 * single.left[i]).abs() < 1e-6);
	}
}

#[test]
fn flac_files_play() {
	let src = write_sine_wav(
		"to_flac.wav",
		48_000,
		2,
		(SampleFormat::Int, 16),
		1_000.0,
		0.25,
		1.0,
	);
	let buf = cadenza_audio_io::decode_file(&src).unwrap();
	let flac = tmp_path("clip.flac");
	cadenza_audio_io::export(
		&buf,
		&flac,
		&cadenza_audio_io::ExportOptions::flac(cadenza_audio_io::FlacBitDepth::Bits16),
	)
	.unwrap();
	let clip = Clip::audio("c", beats(0.0), beats(2.0), &flac);
	let out = render_offline(&project(vec![audio_track(clip)]), &opts(2.0)).unwrap();
	assert!((peak(&out.left[1_000..47_000]) - 0.25).abs() < 0.005);
}

#[test]
fn undecodable_file_warns_with_a_fix() {
	let path = tmp_path("garbage.wav");
	std::fs::write(&path, b"this is not audio at all").unwrap();
	let clip = Clip::audio("c", beats(0.0), beats(2.0), &path);
	let report = analyze_mix(&project(vec![audio_track(clip)]), &opts(2.0)).unwrap();
	assert_eq!(report.warnings.len(), 1, "{:?}", report.warnings);
	assert!(report.warnings[0].contains("garbage.wav"));
	assert!(report.warnings[0].contains("silent"));
}

#[test]
fn only_the_active_take_of_a_group_plays() {
	let quiet = write_sine_wav(
		"take-quiet.wav",
		48_000,
		1,
		(SampleFormat::Float, 32),
		220.0,
		0.1,
		1.0,
	);
	let loud = write_sine_wav(
		"take-loud.wav",
		48_000,
		1,
		(SampleFormat::Float, 32),
		220.0,
		0.8,
		1.0,
	);
	let group = cadenza_core::TakeGroupId::new();
	let mut t = Track::new("audio", TrackKind::Audio);
	for (lane, (path, active)) in [(&quiet, false), (&loud, true)].into_iter().enumerate() {
		let mut c = Clip::audio("take", beats(0.0), beats(1.0), path);
		c.take = Some(cadenza_model::Take {
			group,
			lane: lane as u32,
			active,
		});
		t.clips.push(c);
	}
	// A MIDI take that does not play stays silent too.
	let mut synth = synth_track("keys", &[(60, 0.0, 1.0)], 1.0);
	synth.clips[0].take = Some(cadenza_model::Take {
		group: cadenza_core::TakeGroupId::new(),
		lane: 0,
		active: false,
	});
	let out = render_offline(&project(vec![t, synth]), &opts(1.0)).unwrap();
	let expected = 0.8 * std::f32::consts::FRAC_1_SQRT_2;
	assert!(
		(peak(&out.left[..24_000]) - expected).abs() < 0.01,
		"only the loud take sums: {}",
		peak(&out.left[..24_000])
	);
}
