//! Factory presets of the built-in devices outside the Poly Synth and drum kit libraries
//! (`drum_machine`, `electric_piano`, `sampler` and every effect): each one sounds when
//! played and stays under -1 dBTP, so loading a preset never mutes a track or clips it.

mod common;

use std::path::PathBuf;
use std::sync::OnceLock;

use cadenza_dsp::FactoryPreset;
use cadenza_engine::LevelStats;
use cadenza_engine::factory::{FACTORY_SAMPLES_ENV, apply_preset};
use cadenza_model::{Clip, Device, Note, Project, Track, TrackKind};
use common::*;

/// `(pitch, start beat, length beats, velocity)`.
type Phrase = &'static [(u8, f64, f64, f32)];

const DRUMS: Phrase = &[
	(36, 0.0, 0.25, 1.0),
	(42, 0.0, 0.25, 0.8),
	(42, 0.5, 0.25, 0.6),
	(38, 1.0, 0.25, 0.95),
	(39, 1.0, 0.25, 0.8),
	(46, 1.5, 0.5, 0.7),
	(36, 2.0, 0.25, 1.0),
	(36, 2.5, 0.25, 0.8),
	(63, 2.5, 0.25, 0.8),
	(56, 3.0, 0.25, 0.7),
	(38, 3.0, 0.25, 0.95),
	(45, 3.5, 0.25, 0.9),
	(49, 4.0, 2.0, 0.9),
	(36, 4.0, 0.25, 1.0),
	(51, 5.0, 0.25, 0.8),
];

const KEYS: Phrase = &[
	(48, 0.0, 1.9, 0.9),
	(55, 0.0, 1.9, 0.8),
	(64, 0.0, 1.9, 0.8),
	(67, 0.0, 1.9, 0.8),
	(45, 2.0, 1.9, 0.9),
	(57, 2.0, 1.9, 0.8),
	(60, 2.0, 1.9, 0.8),
	(64, 2.0, 1.9, 0.8),
	(72, 4.0, 0.5, 1.0),
	(76, 4.5, 0.5, 1.0),
	(79, 5.0, 1.0, 1.0),
];

const LOW: Phrase = &[
	(29, 0.0, 1.75, 0.95),
	(36, 1.5, 1.0, 0.9),
	(29, 3.0, 1.0, 0.95),
	(41, 4.0, 1.75, 1.0),
	(34, 5.5, 1.0, 0.9),
];

const LEAD: Phrase = &[
	(72, 0.0, 0.75, 0.9),
	(75, 0.6, 0.75, 0.8),
	(79, 1.5, 0.5, 1.0),
	(77, 2.0, 1.0, 0.9),
	(84, 3.0, 1.5, 1.0),
];

const HIT: Phrase = &[(60, 0.0, 4.0, 1.0)];

fn factory_dir() -> &'static PathBuf {
	static DIR: OnceLock<PathBuf> = OnceLock::new();
	DIR.get_or_init(|| {
		let dir = tmp_path("builtin-preset-samples");
		// SAFETY: set once, before any test reads it (every test goes through here first).
		unsafe { std::env::set_var(FACTORY_SAMPLES_ENV, &dir) };
		cadenza_engine::factory::ensure_factory_samples().unwrap()
	})
}

fn sampler_phrase(p: &FactoryPreset) -> Phrase {
	match p.category {
		"808" | "Bass" => LOW,
		"Lead" => LEAD,
		"FX" => HIT,
		_ => KEYS,
	}
}

fn instrument_track(device: Device, phrase: Phrase) -> Track {
	let mut track = Track::new("t", TrackKind::Instrument).with_instrument(device);
	track.clips.push(
		Clip::midi("c", cadenza_core::Tick::ZERO, beats(8.0)).with_notes(
			phrase
				.iter()
				.map(|&(n, s, l, v)| Note::new(n, beats(s), beats(l), v)),
		),
	);
	track
}

fn measure(project: &Project) -> LevelStats {
	let buf = cadenza_engine::render_offline(project, &opts(10.0)).unwrap();
	let mut analyzer = cadenza_analysis::Analyzer::new(48_000);
	analyzer.push(&buf.left, &buf.right);
	analyzer.finish()
}

fn check(problems: &mut Vec<String>, label: String, s: &LevelStats) {
	if std::env::var_os("CADENZA_PRESET_LEVELS").is_some() {
		println!(
			"{label:<40} {:>6.1} dBTP {:>6.1} LUFS",
			s.true_peak_db, s.lufs_integrated
		);
	}
	if s.true_peak_db.is_nan() || s.true_peak_db <= -40.0 {
		problems.push(format!("{label}: silent ({:.1} dBTP)", s.true_peak_db));
	}
	if s.true_peak_db > -1.0 {
		problems.push(format!("{label}: true peak {:.1} dBTP", s.true_peak_db));
	}
}

fn instrument_presets(id: &str, phrase: fn(&FactoryPreset) -> Phrase) -> Vec<String> {
	factory_dir();
	let mut problems = Vec::new();
	for p in cadenza_dsp::factory_presets(id) {
		let mut device = Device::builtin(id, id);
		apply_preset(&mut device, p).unwrap();
		let s = measure(&project(vec![instrument_track(device, phrase(p))]));
		check(&mut problems, format!("{id}/{}", p.name), &s);
	}
	problems
}

#[test]
fn drum_machine_presets_sound_without_overs() {
	let problems = instrument_presets("drum_machine", |_| DRUMS);
	assert!(problems.is_empty(), "{}", problems.join("\n"));
}

#[test]
fn electric_piano_presets_sound_without_overs() {
	let problems = instrument_presets("electric_piano", |_| KEYS);
	assert!(problems.is_empty(), "{}", problems.join("\n"));
}

#[test]
fn sampler_presets_sound_without_overs() {
	let problems = instrument_presets("sampler", sampler_phrase);
	assert!(problems.is_empty(), "{}", problems.join("\n"));
}

/// Every effect preset on a keys part peaking around -9 dBTP: sounds and leaves headroom.
#[test]
fn effect_presets_sound_without_overs() {
	let mut problems = Vec::new();
	let effects = cadenza_dsp::registry()
		.iter()
		.filter(|info| info.role == cadenza_dsp::DeviceRole::Effect);
	for info in effects {
		for p in cadenza_dsp::factory_presets(info.id) {
			let mut effect = Device::builtin(info.id, info.id);
			apply_preset(&mut effect, p).unwrap();
			let keys = Device::builtin("electric_piano", "Keys").with_param("gain", -6.0);
			let mut track = instrument_track(keys, KEYS);
			track.channel.inserts.push(effect);
			let s = measure(&project(vec![track]));
			check(&mut problems, format!("{}/{}", info.id, p.name), &s);
		}
	}
	assert!(problems.is_empty(), "{}", problems.join("\n"));
}
