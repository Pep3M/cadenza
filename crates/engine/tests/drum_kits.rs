//! The `drum_sampler` factory kits: every bundled sample is credited, installed and used,
//! every kit plays a groove of its genre at a level close to the others (browsing kits
//! never jumps in loudness) without overs.
//!
//! Audition them: `CADENZA_KIT_AUDITION_DIR=dir cargo test -p cadenza-engine --release
//! --test drum_kits -- --ignored --nocapture` writes one WAV per kit (its groove, then
//! every pad once) plus a table.

mod common;

use std::path::PathBuf;
use std::sync::OnceLock;

use cadenza_core::Tick;
use cadenza_dsp::{FactoryPreset, KIT_CATEGORIES, KIT_SAMPLES};
use cadenza_engine::LevelStats;
use cadenza_engine::factory::{FACTORY_SAMPLES_ENV, bundled_sample_dir};
use cadenza_model::{Clip, Device, Note, Project, Track, TrackKind};
use common::*;

/// Window for the loudest 400 ms of a kit playing its groove (momentary LUFS).
const LOUDNESS: (f32, f32) = (-16.0, -10.0);
/// Kits whose coinciding hits reach this true peak (dBTP) first may sit below the window:
/// drums are peaky (crest factors of 15-25 dB), so most kits are as loud as their peaks
/// allow.
const PEAK_LIMITED: f32 = -2.5;
/// Even a peak-limited kit must not be quieter than this (momentary LUFS).
const QUIETEST: f32 = -23.0;

/// Steps of a 16-step bar: `(note, "x...x...")` with `X` accent, `x` normal, `o` ghost.
type Groove = &'static [(u8, &'static str)];

const HOUSE: Groove = &[
	(36, "x...x...x...x..."),
	(39, "....x.......x..."),
	(42, "x.x.x.x.x.x.x.x."),
	(46, "..x...x...x...x."),
	(54, "o.o.o.o.o.o.o.o."),
];
const TECHNO: Groove = &[
	(36, "X...x...x...x..."),
	(39, "....x.......x..."),
	(42, "..x...x...x...x."),
	(46, "..x...x...x...x."),
	(51, "x.x.x.x.x.x.x.x."),
	(37, "...x......x....."),
];
const DANCE: Groove = &[
	(36, "X...x...X...x..."),
	(38, "....x.......x..."),
	(39, "....x.......x..."),
	(46, "..x...x...x...x."),
	(42, "x.o.x.o.x.o.x.o."),
	(49, "x..............."),
];
const TRAP: Groove = &[
	(36, "x.........x....."),
	(35, "x.........x....."),
	(39, "........x......."),
	(38, "........x......."),
	(42, "x.x.x.xxx.x.xxxx"),
	(46, "..............x."),
	(37, "...........o...."),
];
const HIP_HOP: Groove = &[
	(36, "x......x..x....."),
	(38, "....x.......x..o"),
	(42, "x.x.x.x.x.x.x.x."),
	(46, ".............x.."),
	(37, ".......o........"),
];
const REGGAETON: Groove = &[
	(36, "x...x...x...x..."),
	(38, "...x..x....x..x."),
	(37, "...x..x....x..x."),
	(42, "x.x.x.x.x.x.x.x."),
	(65, "..........x.x..."),
	(66, "..............x."),
];
const LATIN: Groove = &[
	(75, "x..x...x..x.x..."),
	(62, "x.x.x.x.x.x.x.x."),
	(63, "......x.......x."),
	(64, ".......x.......x"),
	(60, "x.x.x.x.x.x.x.x."),
	(61, "...x.......x...."),
	(56, "x...x...x...x..."),
	(74, "x.xxx.xxx.xxx.xx"),
	(36, "x.......x......."),
];
const AFRO: Groove = &[
	(36, "x...x...x...x..."),
	(37, "...x..x...x...x."),
	(39, "....x.......x..."),
	(70, "xxxxxxxxxxxxxxxx"),
	(69, "x.x.x.x.x.x.x.x."),
	(63, "..x.....x.x....."),
	(64, ".......x.......x"),
	(42, "..x...x...x...x."),
];
const POP: Groove = &[
	(36, "x......xx......."),
	(38, "....x.......x..."),
	(39, "....x.......x..."),
	(42, "x.x.x.x.x.x.x.x."),
	(54, "....x.......x..."),
];
const FUNK: Groove = &[
	(36, "x.x.......x..x.."),
	(38, "....x..o.o..x..o"),
	(42, "xxxxxxxxxxxxxx.x"),
	(46, "..............x."),
	(54, "x.x.x.x.x.x.x.x."),
];
const ACOUSTIC: Groove = &[
	(36, "x.......x.x....."),
	(38, "....x.......x..."),
	(42, "x.x.x.x.x.x.x.x."),
	(51, "x.x.x.x.x.x.x.x."),
	(49, "x..............."),
];
const RETRO: Groove = &[
	(36, "x...x...x...x..."),
	(38, "....x.......x..."),
	(42, "x.x.x.x.x.x.x.x."),
	(48, "............x.x."),
	(45, ".............x.x"),
];

fn groove(category: &str) -> Groove {
	match category {
		"House" => HOUSE,
		"Techno" => TECHNO,
		"Dance" => DANCE,
		"Trap" => TRAP,
		"Hip-Hop" => HIP_HOP,
		"Reggaeton" => REGGAETON,
		"Latin" => LATIN,
		"Afro" => AFRO,
		"Pop" => POP,
		"Funk" => FUNK,
		"Acoustic" => ACOUSTIC,
		_ => RETRO,
	}
}

/// Points the factory folder at a fresh temporary folder (once per test binary) so the
/// tests install the bundled samples there, never into the user's data folder.
fn factory_dir() -> &'static PathBuf {
	static DIR: OnceLock<PathBuf> = OnceLock::new();
	DIR.get_or_init(|| {
		let dir = std::env::temp_dir().join(format!("cadenza-kit-tests-{}", std::process::id()));
		// SAFETY: set once, before any test reads it (every test goes through here first).
		unsafe { std::env::set_var(FACTORY_SAMPLES_ENV, &dir) };
		cadenza_engine::factory::ensure_factory_samples().unwrap();
		dir
	})
}

fn kits() -> Vec<&'static FactoryPreset> {
	factory_dir();
	cadenza_dsp::factory_presets("drum_sampler")
		.iter()
		.collect()
}

/// Two bars of the kit's groove, then every pad once (a beat apart).
fn kit_notes(p: &FactoryPreset) -> Vec<Note> {
	let mut notes = Vec::new();
	for bar in 0..2 {
		for (note, steps) in groove(p.category) {
			for (i, c) in steps.chars().enumerate() {
				let velocity = match c {
					'X' => 1.0,
					'x' => 0.8,
					'o' => 0.45,
					_ => continue,
				};
				let at = f64::from(bar) * 4.0 + i as f64 * 0.25;
				notes.push(Note::new(*note, beats(at), beats(0.2), velocity));
			}
		}
	}
	let mut pads: Vec<u8> = p.zones.iter().map(|z| z.key_low).collect();
	pads.sort_unstable();
	pads.dedup();
	for (i, note) in pads.into_iter().enumerate() {
		notes.push(Note::new(note, beats(8.0 + i as f64), beats(0.2), 0.8));
	}
	notes
}

fn tour_beats(p: &FactoryPreset) -> f64 {
	let mut pads: Vec<u8> = p.zones.iter().map(|z| z.key_low).collect();
	pads.sort_unstable();
	pads.dedup();
	8.0 + pads.len() as f64 + 3.0
}

fn kit_project(p: &FactoryPreset, tour: bool) -> Project {
	let mut kit = Device::builtin("drum_sampler", "Drum Sampler");
	cadenza_engine::factory::apply_preset(&mut kit, p).unwrap();
	let mut notes = kit_notes(p);
	if !tour {
		notes.retain(|n| n.start < beats(8.0));
	}
	let mut track = Track::new(p.name, TrackKind::Instrument).with_instrument(kit);
	track
		.clips
		.push(Clip::midi(p.name, Tick::ZERO, beats(tour_beats(p))).with_notes(notes));
	let mut project = project(vec![track]);
	project.tempo = cadenza_core::TempoMap::constant(tempo(p.category));
	project
}

fn tempo(category: &str) -> f64 {
	match category {
		"Trap" => 140.0,
		"Hip-Hop" => 90.0,
		"Reggaeton" => 95.0,
		"Latin" => 100.0,
		"Afro" => 108.0,
		"Pop" | "Funk" | "Acoustic" => 104.0,
		"Techno" => 132.0,
		"Dance" => 128.0,
		_ => 124.0,
	}
}

/// Momentary loudness max and level stats of the groove plus a beat of tail.
fn measure(p: &FactoryPreset) -> (f32, LevelStats) {
	let buf = cadenza_engine::render_offline(&kit_project(p, false), &opts(9.0)).unwrap();
	let mut meter = cadenza_analysis::loudness::LoudnessMeter::new(48_000);
	meter.push(&buf.left, &buf.right);
	let mut analyzer = cadenza_analysis::Analyzer::new(48_000);
	analyzer.push(&buf.left, &buf.right);
	(meter.momentary_max(), analyzer.finish())
}

fn genre_kits() -> Vec<&'static FactoryPreset> {
	kits()
		.into_iter()
		.filter(|p| p.zones.iter().any(|z| z.sample.ends_with(".flac")))
		.collect()
}

#[test]
fn every_bundled_sample_is_credited_installed_and_used() {
	let dir = factory_dir();
	let bundled = bundled_sample_dir().expect("assets/samples ships with the repository");
	let credits = std::fs::read_to_string(bundled.join("CREDITS.md")).unwrap();
	let used: Vec<&str> = kits()
		.iter()
		.flat_map(|p| p.zones.iter().map(|z| z.sample))
		.collect();
	let mut on_disk: Vec<String> = std::fs::read_dir(&bundled)
		.unwrap()
		.flatten()
		.map(|e| e.file_name().to_string_lossy().into_owned())
		.filter(|n| n.ends_with(".flac"))
		.collect();
	on_disk.sort();
	let mut listed: Vec<String> = KIT_SAMPLES.iter().map(|(n, _)| (*n).to_owned()).collect();
	listed.sort();
	assert_eq!(on_disk, listed, "assets/samples and KIT_SAMPLES differ");
	for (name, note) in KIT_SAMPLES {
		let stem = name.trim_end_matches(".flac");
		assert!(credits.contains(stem), "{name} is not in CREDITS.md");
		assert!(used.contains(&name), "{name} is in no kit");
		assert!((35..=81).contains(&note), "{name}: note {note}");
		let buf = cadenza_audio_io::decode_file(dir.join(name)).unwrap();
		assert!(buf.peak() > 0.5, "{name}: peak {}", buf.peak());
		assert!(
			buf.duration_seconds() < 4.0,
			"{name}: {} s",
			buf.duration_seconds()
		);
	}
	for p in kits() {
		for z in p.zones {
			assert!(
				dir.join(z.sample).is_file(),
				"{}: {} missing",
				p.name,
				z.sample
			);
		}
	}
}

#[test]
fn every_kit_is_categorized_described_and_valid() {
	let all = genre_kits();
	assert!(all.len() >= 30, "the library has {} genre kits", all.len());
	for p in kits() {
		assert!(
			KIT_CATEGORIES.contains(&p.category),
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
		let project = kit_project(p, true);
		let mut doc = cadenza_model::Document::new(Project::new("check"));
		let mut track = project.tracks[0].clone();
		track.clips.clear();
		doc.execute(cadenza_model::Command::AddTrack { track, index: None })
			.unwrap_or_else(|e| panic!("{}: {e}", p.name));
	}
	for c in KIT_CATEGORIES {
		assert!(
			all.iter().filter(|p| p.category == c).count() >= 2 || c == "Retro",
			"category {c} needs at least two kits"
		);
	}
}

#[test]
fn every_kit_sounds_at_a_matched_level() {
	let mut problems = Vec::new();
	for p in kits() {
		let (momentary, s) = measure(p);
		if s.true_peak_db > -1.0 {
			problems.push(format!("{}: true peak {:.1} dBTP", p.name, s.true_peak_db));
		}
		let peak_limited =
			s.true_peak_db >= PEAK_LIMITED && (QUIETEST..LOUDNESS.0).contains(&momentary);
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
#[ignore = "writes WAVs for listening; set CADENZA_KIT_AUDITION_DIR"]
fn audition_kits() {
	let dir = std::env::var_os("CADENZA_KIT_AUDITION_DIR")
		.map(PathBuf::from)
		.unwrap_or_else(|| std::env::temp_dir().join("cadenza-kit-audition"));
	std::fs::create_dir_all(&dir).unwrap();
	println!(
		"{:<24} {:<9} {:>6} {:>6} {:>6} {:>6} | {:>6} {:>6} {:>6} {:>6} {:>6}",
		"kit", "category", "M", "LUFS", "TP", "crest", "low", "lowmid", "mid", "himid", "high"
	);
	for (i, p) in kits().iter().enumerate() {
		let file = dir.join(format!(
			"{:02} {} - {}.wav",
			i + 1,
			p.category,
			p.name.replace('/', "-")
		));
		cadenza_engine::render_to_wav(&kit_project(p, true), &opts(tour_beats(p)), &file).unwrap();
		let (momentary, s) = measure(p);
		let b = s.band_energy_db;
		println!(
			"{:<24} {:<9} {:>6.1} {:>6.1} {:>6.1} {:>6.1} | {:>6.1} {:>6.1} {:>6.1} {:>6.1} {:>6.1}",
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
