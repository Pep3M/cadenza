//! The Poly Synth factory library: every preset plays a phrase typical of its category and
//! must sound (not silent, no overs) at a level close to its neighbours, so browsing
//! presets never jumps in loudness.
//!
//! Audition them: `CADENZA_PRESET_AUDITION_DIR=dir cargo test -p cadenza-engine --release
//! --test preset_library -- --ignored --nocapture` writes one WAV per preset plus a table.

mod common;

use cadenza_core::Tick;
use cadenza_dsp::FactoryPreset;
use cadenza_engine::LevelStats;
use cadenza_model::{Clip, Device, Document, Note, Project, Track, TrackKind};
use common::*;

/// `(pitch, start beat, length beats, velocity)`.
type Phrase = &'static [(u8, f64, f64, f32)];

/// Window for the loudest 400 ms of a preset playing its phrase (momentary LUFS): how loud
/// it feels when played, fair to short stabs and dense pads alike.
const LOUDNESS: (f32, f32) = (-15.0, -10.0);
/// Presets whose transients reach this true peak (dBTP) first may sit below the window:
/// short stabs and deep subs are as loud as their peaks allow.
const PEAK_LIMITED: f32 = -2.5;

/// Root-octave line with an octave jump and an overlapping slide.
const BASS: Phrase = &[
	(36, 0.0, 0.45, 0.9),
	(36, 0.5, 0.45, 0.7),
	(48, 1.0, 0.45, 0.9),
	(36, 1.5, 0.45, 0.7),
	(39, 2.0, 0.95, 0.9),
	(43, 3.0, 0.6, 0.8),
	(46, 3.5, 0.6, 0.8),
	(36, 4.0, 0.45, 0.9),
	(36, 4.5, 0.45, 0.7),
	(48, 5.0, 0.45, 0.9),
	(36, 5.5, 0.45, 0.7),
	(41, 6.0, 1.0, 0.9),
	(43, 7.0, 0.9, 0.8),
];

/// Long low notes that overlap so a glide slides between them.
const EIGHT_O_EIGHT: Phrase = &[
	(36, 0.0, 1.75, 0.95),
	(43, 1.5, 1.0, 0.9),
	(36, 3.0, 1.0, 0.95),
	(36, 4.0, 1.75, 0.95),
	(48, 5.5, 1.0, 0.9),
	(34, 7.0, 1.0, 0.95),
];

const LEAD: Phrase = &[
	(72, 0.0, 0.75, 0.9),
	(75, 0.75, 0.75, 0.8),
	(79, 1.5, 0.5, 0.85),
	(77, 2.0, 1.0, 0.9),
	(75, 3.0, 0.5, 0.8),
	(74, 3.5, 0.6, 0.8),
	(72, 4.0, 1.5, 0.9),
	(70, 5.5, 0.5, 0.75),
	(72, 6.0, 2.0, 0.9),
];

/// Sixteenth-note arpeggio over Cm7 and Abmaj7.
const ARP: Phrase = &[
	(60, 0.0, 0.25, 0.9),
	(63, 0.25, 0.25, 0.7),
	(67, 0.5, 0.25, 0.8),
	(70, 0.75, 0.25, 0.7),
	(72, 1.0, 0.25, 0.9),
	(70, 1.25, 0.25, 0.7),
	(67, 1.5, 0.25, 0.8),
	(63, 1.75, 0.25, 0.7),
	(60, 2.0, 0.25, 0.9),
	(63, 2.25, 0.25, 0.7),
	(67, 2.5, 0.25, 0.8),
	(70, 2.75, 0.25, 0.7),
	(72, 3.0, 0.25, 0.9),
	(70, 3.25, 0.25, 0.7),
	(67, 3.5, 0.25, 0.8),
	(63, 3.75, 0.25, 0.7),
	(56, 4.0, 0.25, 0.9),
	(60, 4.25, 0.25, 0.7),
	(63, 4.5, 0.25, 0.8),
	(67, 4.75, 0.25, 0.7),
	(72, 5.0, 0.25, 0.9),
	(67, 5.25, 0.25, 0.7),
	(63, 5.5, 0.25, 0.8),
	(60, 5.75, 0.25, 0.7),
	(56, 6.0, 0.25, 0.9),
	(60, 6.25, 0.25, 0.7),
	(63, 6.5, 0.25, 0.8),
	(67, 6.75, 0.25, 0.7),
	(72, 7.0, 0.5, 0.9),
];

/// Short plucked melody with space between the notes.
const PLUCK: Phrase = &[
	(72, 0.0, 0.25, 0.9),
	(67, 0.5, 0.25, 0.7),
	(70, 1.0, 0.25, 0.8),
	(72, 1.5, 0.25, 0.8),
	(75, 2.0, 0.5, 0.9),
	(72, 3.0, 0.25, 0.7),
	(70, 3.5, 0.25, 0.8),
	(68, 4.0, 0.25, 0.9),
	(63, 4.5, 0.25, 0.7),
	(67, 5.0, 0.25, 0.8),
	(68, 5.5, 0.25, 0.8),
	(72, 6.0, 0.5, 0.9),
	(67, 7.0, 0.5, 0.8),
];

/// Four-voice chords, two beats each, played on the beat.
const KEYS: Phrase = &[
	(48, 0.0, 1.9, 0.8),
	(55, 0.0, 1.9, 0.7),
	(63, 0.0, 1.9, 0.75),
	(70, 0.0, 1.9, 0.7),
	(44, 2.0, 1.9, 0.8),
	(55, 2.0, 1.9, 0.7),
	(60, 2.0, 1.9, 0.75),
	(67, 2.0, 1.9, 0.7),
	(51, 4.0, 1.9, 0.8),
	(58, 4.0, 1.9, 0.7),
	(62, 4.0, 1.9, 0.75),
	(67, 4.0, 1.9, 0.7),
	(46, 6.0, 1.9, 0.8),
	(53, 6.0, 1.9, 0.7),
	(62, 6.0, 1.9, 0.75),
	(65, 6.0, 1.9, 0.7),
];

/// Two sustained four-voice chords.
const PAD: Phrase = &[
	(48, 0.0, 3.9, 0.8),
	(55, 0.0, 3.9, 0.8),
	(63, 0.0, 3.9, 0.8),
	(70, 0.0, 3.9, 0.8),
	(44, 4.0, 3.9, 0.8),
	(55, 4.0, 3.9, 0.8),
	(60, 4.0, 3.9, 0.8),
	(67, 4.0, 3.9, 0.8),
];

/// Short chord stabs on the off-beats.
const STAB: Phrase = &[
	(60, 0.5, 0.3, 0.9),
	(63, 0.5, 0.3, 0.9),
	(67, 0.5, 0.3, 0.9),
	(70, 0.5, 0.3, 0.9),
	(60, 1.5, 0.3, 0.8),
	(63, 1.5, 0.3, 0.8),
	(67, 1.5, 0.3, 0.8),
	(70, 1.5, 0.3, 0.8),
	(60, 2.75, 0.3, 0.9),
	(63, 2.75, 0.3, 0.9),
	(67, 2.75, 0.3, 0.9),
	(70, 2.75, 0.3, 0.9),
	(56, 4.5, 0.3, 0.9),
	(60, 4.5, 0.3, 0.9),
	(63, 4.5, 0.3, 0.9),
	(67, 4.5, 0.3, 0.9),
	(56, 5.5, 0.3, 0.8),
	(60, 5.5, 0.3, 0.8),
	(63, 5.5, 0.3, 0.8),
	(67, 5.5, 0.3, 0.8),
	(56, 6.75, 0.3, 0.9),
	(60, 6.75, 0.3, 0.9),
	(63, 6.75, 0.3, 0.9),
	(67, 6.75, 0.3, 0.9),
];

const FX: Phrase = &[(48, 0.0, 6.0, 0.9), (55, 0.0, 6.0, 0.9)];

/// The phrase a preset is judged by: its category's, or a stab for "Stab"/"Chord" sounds.
fn phrase(p: &FactoryPreset) -> Phrase {
	let n = p.name.to_lowercase();
	if n.contains("stab") || n.contains("chord") {
		return STAB;
	}
	match p.category {
		"Bass" => BASS,
		"808" => EIGHT_O_EIGHT,
		"Lead" => LEAD,
		"Pluck" | "Bell" => PLUCK,
		"Arp" => ARP,
		"Keys" => KEYS,
		"Pad" | "Choir" | "Strings" | "Brass" => PAD,
		_ => FX,
	}
}

fn presets() -> &'static [FactoryPreset] {
	cadenza_dsp::factory_presets("poly_synth")
}

fn preset_project(p: &FactoryPreset) -> Project {
	let mut synth = Device::builtin("poly_synth", "Poly Synth");
	cadenza_engine::factory::apply_preset(&mut synth, p).unwrap();
	let mut track = Track::new(p.name, TrackKind::Instrument).with_instrument(synth);
	track.clips.push(
		Clip::midi(p.name, Tick::ZERO, beats(8.0)).with_notes(
			phrase(p)
				.iter()
				.map(|&(n, s, l, v)| Note::new(n, beats(s), beats(l), v)),
		),
	);
	project(vec![track])
}

/// Momentary loudness max and true peak of the phrase plus two beats of tail.
fn measure(p: &FactoryPreset) -> (f32, LevelStats) {
	let buf = cadenza_engine::render_offline(&preset_project(p), &opts(10.0)).unwrap();
	let mut meter = cadenza_analysis::loudness::LoudnessMeter::new(48_000);
	meter.push(&buf.left, &buf.right);
	let mut analyzer = cadenza_analysis::Analyzer::new(48_000);
	analyzer.push(&buf.left, &buf.right);
	(meter.momentary_max(), analyzer.finish())
}

#[test]
fn every_preset_is_categorized_named_once_and_valid() {
	let all = presets();
	assert!(all.len() >= 60, "the library has {} presets", all.len());
	for p in all {
		assert!(
			cadenza_dsp::SYNTH_CATEGORIES.contains(&p.category),
			"{}: category '{}'",
			p.name,
			p.category
		);
		assert!(
			(60..=400).contains(&p.description.len()),
			"{}: describe it in 60-400 characters ({})",
			p.name,
			p.description.len()
		);
		// The model accepts the modulators (ranges, unique targets, known params).
		let project = preset_project(p);
		let device = project.tracks[0].instrument.clone().unwrap();
		let mut doc = Document::new(Project::new("check"));
		let mut track = project.tracks[0].clone();
		track.clips.clear();
		doc.execute(cadenza_model::Command::AddTrack { track, index: None })
			.unwrap_or_else(|e| panic!("{}: {e}", p.name));
		let info = cadenza_dsp::device_info("poly_synth").unwrap();
		for m in &device.modulators {
			for t in &m.targets {
				assert!(info.param(&t.param).is_some(), "{}: {}", p.name, t.param);
			}
		}
	}
	for c in cadenza_dsp::SYNTH_CATEGORIES {
		assert!(
			all.iter().filter(|p| p.category == c).count() >= 2,
			"category {c} needs at least two presets"
		);
	}
}

#[test]
fn every_preset_sounds_at_a_matched_level() {
	let mut problems = Vec::new();
	for p in presets() {
		let (momentary, s) = measure(p);
		if s.true_peak_db > -1.0 {
			problems.push(format!("{}: true peak {:.1} dBTP", p.name, s.true_peak_db));
		}
		let peak_limited = s.true_peak_db >= PEAK_LIMITED && momentary < LOUDNESS.0;
		if !(LOUDNESS.0..=LOUDNESS.1).contains(&momentary) && !peak_limited {
			problems.push(format!(
				"{}: {momentary:.1} LUFS momentary (window {LOUDNESS:?})",
				p.name
			));
		}
	}
	assert!(problems.is_empty(), "{}", problems.join("\n"));
}

#[test]
fn every_preset_modulator_is_audible() {
	let mut weak = Vec::new();
	for p in presets().iter().filter(|p| !p.modulators.is_empty()) {
		let with = preset_project(p);
		let mut without = with.clone();
		without.tracks[0]
			.instrument
			.as_mut()
			.unwrap()
			.modulators
			.clear();
		let a = cadenza_engine::render_offline(&with, &opts(10.0)).unwrap();
		let b = cadenza_engine::render_offline(&without, &opts(10.0)).unwrap();
		let diff: Vec<f32> = a.left.iter().zip(&b.left).map(|(x, y)| x - y).collect();
		let db = 20.0 * (rms(&diff) / rms(&b.left)).log10();
		// Slow pad drifts land around -22 dB; a forgotten depth sits far below.
		if db < -26.0 {
			weak.push(format!(
				"{}: modulation changes the sound by only {db:.1} dB",
				p.name
			));
		}
	}
	assert!(weak.is_empty(), "{}", weak.join("\n"));
}

#[test]
#[ignore = "writes WAVs for listening; set CADENZA_PRESET_AUDITION_DIR"]
fn audition_presets() {
	let dir = std::env::var_os("CADENZA_PRESET_AUDITION_DIR")
		.map(std::path::PathBuf::from)
		.unwrap_or_else(|| std::env::temp_dir().join("cadenza-preset-audition"));
	std::fs::create_dir_all(&dir).unwrap();
	println!(
		"{:<26} {:<8} {:>6} {:>6} {:>6} {:>6} | {:>6} {:>6} {:>6} {:>6} {:>6}",
		"preset", "category", "M", "LUFS", "TP", "crest", "low", "lowmid", "mid", "himid", "high"
	);
	for (i, p) in presets().iter().enumerate() {
		let file = dir.join(format!(
			"{:02} {} - {}.wav",
			i + 1,
			p.category,
			p.name.replace('/', "-")
		));
		cadenza_engine::render_to_wav(&preset_project(p), &opts(10.0), &file).unwrap();
		let (momentary, s) = measure(p);
		let b = s.band_energy_db;
		println!(
			"{:<26} {:<8} {:>6.1} {:>6.1} {:>6.1} {:>6.1} | {:>6.1} {:>6.1} {:>6.1} {:>6.1} {:>6.1}",
			p.name,
			p.category,
			momentary,
			s.lufs_integrated,
			s.true_peak_db,
			s.crest_factor_db,
			b.low,
			b.low_mid,
			b.mid,
			b.high_mid,
			b.high
		);
	}
	println!("WAVs in {}", dir.display());
}
