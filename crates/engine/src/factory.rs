//! Factory content of the sample-based built-ins, gathered in one folder on first use:
//! a small kit and a multisample rendered procedurally (the built-in `drum_machine` and
//! additive synthesis) into WAV files, plus the bundled genre kits (`assets/samples` in
//! the repository, shipped next to the binaries; see `assets/samples/CREDITS.md` for
//! sources and licenses) copied in. The factory presets of `sampler` and `drum_sampler`
//! ([`cadenza_dsp::factory_presets`]) refer to these files by name; [`preset_zones`]
//! turns them into project zones with absolute paths.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use cadenza_core::ModulatorId;
use cadenza_dsp::factory_samples as names;
use cadenza_dsp::{
	FactoryPreset, MidiEvent, PresetLfoShape, PresetModSource, PresetRetrigger, Transport,
};
use cadenza_model::{
	Device, EnvelopeSource, LfoShape, LoopMode, ModTarget, Modulator, ModulatorKind, Retrigger,
	SampleRef, SampleZone,
};
use schemars::JsonSchema;
use serde::Serialize;

use crate::error::EngineError;

/// Overrides the factory sample folder (tests, portable installs).
pub const FACTORY_SAMPLES_ENV: &str = "CADENZA_FACTORY_SAMPLES";
/// Bumped whenever the synthesized content changes, so old renders are replaced.
const CONTENT_VERSION: &str = "1";
const VERSION_FILE: &str = ".cadenza-factory-version";
const RATE: u32 = 48_000;

/// Where the factory samples live: `$CADENZA_FACTORY_SAMPLES`, else the OS data dir +
/// `cadenza/samples/factory` (see [`cadenza_core::paths`]).
pub fn factory_sample_dir() -> PathBuf {
	let var = |k: &str| {
		std::env::var_os(k)
			.filter(|v| !v.is_empty())
			.map(PathBuf::from)
	};
	if let Some(dir) = var(FACTORY_SAMPLES_ENV) {
		return dir;
	}
	cadenza_core::paths::data_dir()
		.join("samples")
		.join("factory")
}

/// Overrides the folder of bundled samples (tests, unusual installs).
pub const BUNDLED_SAMPLES_ENV: &str = "CADENZA_BUNDLED_SAMPLES";

/// The audio extensions of bundled samples.
const BUNDLED_EXTENSIONS: [&str; 2] = ["flac", "wav"];

/// Where the bundled sample files ship: `$CADENZA_BUNDLED_SAMPLES`, else `samples` next
/// to the executable (tar.gz, zip, Windows installer), `../share/cadenza/samples`
/// (AppImage), `../Resources/samples` (macOS bundle), else the repository's
/// `assets/samples` (development builds and tests). `None` when none exists.
pub fn bundled_sample_dir() -> Option<PathBuf> {
	if let Some(dir) = std::env::var_os(BUNDLED_SAMPLES_ENV).filter(|v| !v.is_empty()) {
		return Some(PathBuf::from(dir));
	}
	let exe = std::env::current_exe()
		.ok()
		.and_then(|e| e.canonicalize().ok());
	let beside = exe.as_deref().and_then(Path::parent).map(|bin| {
		[
			bin.join("samples"),
			bin.join("../share/cadenza/samples"),
			bin.join("../Resources/samples"),
		]
	});
	let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/samples");
	beside
		.into_iter()
		.flatten()
		.chain(std::iter::once(repo))
		.find(|d| d.join("CREDITS.md").is_file())
}

/// The bundled audio files (name, path), sorted by name.
fn bundled_files(dir: &Path) -> Vec<(String, PathBuf)> {
	let mut files: Vec<(String, PathBuf)> = std::fs::read_dir(dir)
		.into_iter()
		.flatten()
		.flatten()
		.map(|e| e.path())
		.filter(|p| {
			p.extension()
				.and_then(|e| e.to_str())
				.is_some_and(|e| BUNDLED_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
		})
		.filter_map(|p| Some((p.file_name()?.to_str()?.to_owned(), p)))
		.collect();
	files.sort();
	files
}

/// Copies the bundled files missing from `dir` (or of another size) into it.
fn copy_bundled(bundled: &Path, dir: &Path) -> Result<(), EngineError> {
	for (name, from) in bundled_files(bundled) {
		let to = dir.join(&name);
		let size = |p: &Path| std::fs::metadata(p).map(|m| m.len()).ok();
		if size(&to).is_some() && size(&to) == size(&from) {
			continue;
		}
		let err = |e: std::io::Error| EngineError::WavWrite {
			path: to.clone(),
			message: format!("cannot copy the bundled sample {}: {e}", from.display()),
		};
		let tmp = to.with_extension("part");
		std::fs::copy(&from, &tmp).map_err(err)?;
		std::fs::rename(&tmp, &to).map_err(err)?;
		cadenza_audio_io::AudioCache::global().invalidate(&to);
	}
	Ok(())
}

/// One factory sample file.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct FactorySample {
	/// File name ("Kick.wav").
	pub name: String,
	/// Absolute path, ready for load_sample.
	pub path: PathBuf,
	/// "drum" (one-shot hit, with its General MIDI note) or "tonal" (pitched, looped).
	pub kind: &'static str,
	/// GM drum note (drums) or recorded pitch (tonal), MIDI number (60 = C4).
	pub note: u8,
}

/// (file, kind, note, renderer)
type Spec = (&'static str, &'static str, u8, fn() -> Vec<f32>);

fn specs() -> Vec<Spec> {
	vec![
		(names::KICK, "drum", 36, || drum(36, 0.9)),
		(names::SNARE, "drum", 38, || drum(38, 0.6)),
		(names::CLAP, "drum", 39, || drum(39, 0.6)),
		(names::RIM, "drum", 37, rim),
		(names::CLOSED_HAT, "drum", 42, || drum(42, 0.3)),
		(names::OPEN_HAT, "drum", 46, || drum(46, 1.0)),
		(names::LOW_TOM, "drum", 41, || drum(41, 0.9)),
		(names::MID_TOM, "drum", 45, || drum(45, 0.9)),
		(names::HIGH_TOM, "drum", 48, || drum(48, 0.9)),
		(names::CRASH, "drum", 49, || drum(49, 3.0)),
		(names::RIDE, "drum", 51, || drum(51, 2.5)),
		(names::SINE_PAD[0], "tonal", 36, || sine_pad(36)),
		(names::SINE_PAD[1], "tonal", 48, || sine_pad(48)),
		(names::SINE_PAD[2], "tonal", 60, || sine_pad(60)),
		(names::SINE_PAD[3], "tonal", 72, || sine_pad(72)),
	]
}

/// Renders one hit of the built-in drum machine (mono, peak-normalized to -1 dBFS,
/// trailing silence trimmed).
fn drum(note: u8, seconds: f32) -> Vec<f32> {
	let mut dm = cadenza_dsp::create("drum_machine").expect("drum_machine is built in");
	dm.prepare(RATE as f32, 512);
	let frames = (seconds * RATE as f32) as usize;
	let mut out = Vec::with_capacity(frames);
	let mut first = true;
	while out.len() < frames {
		let n = (frames - out.len()).min(512);
		let mut l = vec![0.0f32; n];
		let mut r = vec![0.0f32; n];
		let events = [MidiEvent::NoteOn {
			offset: 0,
			pitch: note,
			velocity: 1.0,
		}];
		let ev: &[MidiEvent] = if first { &events } else { &[] };
		first = false;
		dm.process(&mut l, &mut r, ev, &Transport::default());
		out.extend(l.iter().zip(&r).map(|(a, b)| 0.5 * (a + b)));
	}
	finish(out)
}

/// A short rim click: a damped 1.7 kHz partial over a noise tick.
fn rim() -> Vec<f32> {
	let mut rng = cadenza_dsp::util::Rng::new(0x51_7a);
	let frames = (0.12 * RATE as f32) as usize;
	finish(
		(0..frames)
			.map(|n| {
				let t = n as f32 / RATE as f32;
				let tone = (std::f32::consts::TAU * 1_700.0 * t).sin() * (-t * 60.0).exp();
				let tick = rng.bipolar() * (-t * 400.0).exp();
				0.8 * tone + 0.5 * tick
			})
			.collect(),
	)
}

/// A steady organ-like sine pad (partials 1-4) on `note`, 3 s, looped by the presets
/// between 0.5 s and 2.5 s.
fn sine_pad(note: u8) -> Vec<f32> {
	let freq = cadenza_dsp::util::midi_to_hz(f32::from(note)) as f64;
	let frames = 3 * RATE as usize;
	let partials = [(1.0, 1.0), (2.0, 0.35), (3.0, 0.15), (4.0, 0.07)];
	finish(
		(0..frames)
			.map(|n| {
				let t = n as f64 / f64::from(RATE);
				let fade = (t / 0.01).min(1.0);
				let s: f64 = partials
					.iter()
					.map(|(k, a)| a * (std::f64::consts::TAU * freq * k * t).sin())
					.sum();
				(s * fade) as f32
			})
			.collect(),
	)
}

fn finish(mut x: Vec<f32>) -> Vec<f32> {
	let last = x.iter().rposition(|v| v.abs() > 1e-4).map_or(0, |i| i + 1);
	x.truncate(last.max(1));
	let peak = x.iter().fold(0.0f32, |m, v| m.max(v.abs()));
	if peak > 0.0 {
		let g = 0.89 / peak;
		for v in &mut x {
			*v *= g;
		}
	}
	x
}

fn write_wav(path: &Path, samples: &[f32]) -> Result<(), EngineError> {
	let err = |e: &dyn std::fmt::Display| EngineError::WavWrite {
		path: path.to_path_buf(),
		message: e.to_string(),
	};
	let spec = hound::WavSpec {
		channels: 1,
		sample_rate: RATE,
		bits_per_sample: 32,
		sample_format: hound::SampleFormat::Float,
	};
	let tmp = path.with_extension("wav.tmp");
	let mut w = hound::WavWriter::create(&tmp, spec).map_err(|e| err(&e))?;
	for &s in samples {
		w.write_sample(s).map_err(|e| err(&e))?;
	}
	w.finalize().map_err(|e| err(&e))?;
	std::fs::rename(&tmp, path).map_err(|e| err(&e))
}

/// Renders the factory samples into [`factory_sample_dir`] unless they are already there
/// (same content version, every file present); returns the folder.
pub fn ensure_factory_samples() -> Result<PathBuf, EngineError> {
	let dir = factory_sample_dir();
	let bundled = bundled_sample_dir();
	// Copying is checked once per process and folder (sizes of a few hundred files).
	static SYNCED: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
	let synced = SYNCED
		.lock()
		.unwrap_or_else(|p| p.into_inner())
		.contains(&dir);
	ensure_in(&dir, if synced { None } else { bundled.as_deref() })?;
	SYNCED
		.lock()
		.unwrap_or_else(|p| p.into_inner())
		.push(dir.clone());
	Ok(dir)
}

static GENERATING: Mutex<()> = Mutex::new(());

fn ensure_in(dir: &Path, bundled: Option<&Path>) -> Result<PathBuf, EngineError> {
	let _guard = GENERATING.lock().unwrap_or_else(|p| p.into_inner());
	if let Some(bundled) = bundled {
		std::fs::create_dir_all(dir).map_err(|e| EngineError::WavWrite {
			path: dir.to_path_buf(),
			message: format!("cannot create the factory sample folder: {e}"),
		})?;
		copy_bundled(bundled, dir)?;
	}
	let specs = specs();
	let version = std::fs::read_to_string(dir.join(VERSION_FILE)).unwrap_or_default();
	let complete = version.trim() == CONTENT_VERSION
		&& specs.iter().all(|(name, ..)| dir.join(name).is_file());
	if complete {
		return Ok(dir.to_path_buf());
	}
	std::fs::create_dir_all(dir).map_err(|e| EngineError::WavWrite {
		path: dir.to_path_buf(),
		message: format!("cannot create the factory sample folder: {e}"),
	})?;
	for (name, _, _, render) in &specs {
		let path = dir.join(name);
		write_wav(&path, &render())?;
		cadenza_audio_io::AudioCache::global().invalidate(&path);
	}
	std::fs::write(dir.join(VERSION_FILE), CONTENT_VERSION).map_err(|e| EngineError::WavWrite {
		path: dir.join(VERSION_FILE),
		message: e.to_string(),
	})?;
	Ok(dir.to_path_buf())
}

/// Every factory sample: the synthesized ones (rendered on first use), then the bundled
/// kit samples that are installed.
pub fn list_factory_samples() -> Result<Vec<FactorySample>, EngineError> {
	let dir = ensure_factory_samples()?;
	let synthesized = specs()
		.into_iter()
		.map(|(name, kind, note, _)| (name, kind, note));
	let bundled = cadenza_dsp::KIT_SAMPLES
		.iter()
		.map(|&(name, note)| (name, "drum", note))
		.filter(|(name, ..)| dir.join(name).is_file());
	Ok(synthesized
		.chain(bundled)
		.map(|(name, kind, note)| FactorySample {
			name: name.to_owned(),
			path: dir.join(name),
			kind,
			note,
		})
		.collect())
}

/// The project zones of a factory preset, pointing at the factory samples (rendered on
/// first use). Empty for presets without zones.
pub fn preset_zones(preset: &FactoryPreset) -> Result<Vec<SampleZone>, EngineError> {
	if preset.zones.is_empty() {
		return Ok(Vec::new());
	}
	let dir = ensure_factory_samples()?;
	Ok(preset
		.zones
		.iter()
		.map(|z| {
			let mut zone = SampleZone::new(
				SampleRef::new(dir.join(z.sample)),
				z.key_low,
				z.key_high,
				z.root_key,
			);
			zone.gain_db = z.gain_db;
			zone.pan = z.pan;
			zone.choke = z.choke;
			zone.decay_ms = z.decay_ms;
			zone.vel_low = z.vel_low;
			zone.vel_high = z.vel_high;
			zone.round_robin = z.round_robin;
			zone.tune = z.tune;
			zone.attack_ms = z.attack_ms;
			zone.reverse = z.reverse;
			if let Some((start, end, xf)) = z.forward_loop {
				zone.loop_mode = LoopMode::Forward;
				zone.loop_start = start;
				zone.loop_end = Some(end);
				zone.loop_crossfade = xf;
			}
			zone
		})
		.collect())
}

/// The modulators of a factory preset as project modulators, with fresh ids.
pub fn preset_modulators(preset: &FactoryPreset) -> Vec<Modulator> {
	preset
		.modulators
		.iter()
		.map(|m| Modulator {
			id: ModulatorId::new(),
			kind: match m.source {
				PresetModSource::Lfo {
					shape,
					rate_hz,
					sync_beats,
					phase,
					retrigger,
				} => ModulatorKind::Lfo {
					shape: match shape {
						PresetLfoShape::Sine => LfoShape::Sine,
						PresetLfoShape::Triangle => LfoShape::Triangle,
						PresetLfoShape::Saw => LfoShape::Saw,
						PresetLfoShape::Square => LfoShape::Square,
						PresetLfoShape::Random => LfoShape::Random,
					},
					rate_hz,
					sync_beats,
					phase,
					retrigger: match retrigger {
						PresetRetrigger::Free => Retrigger::Free,
						PresetRetrigger::Note => Retrigger::Note,
						PresetRetrigger::Transport => Retrigger::Transport,
					},
				},
				PresetModSource::Envelope {
					attack_ms,
					release_ms,
					gain_db,
				} => ModulatorKind::Envelope {
					source: EnvelopeSource::OwnInput,
					attack_ms,
					release_ms,
					gain_db,
				},
			},
			targets: m
				.targets
				.iter()
				.map(|(param, depth)| ModTarget {
					param: (*param).to_owned(),
					depth: *depth,
				})
				.collect(),
		})
		.collect()
}

/// Loads factory `preset` into a new `device`: its params, zones and modulators.
pub fn apply_preset(device: &mut Device, preset: &FactoryPreset) -> Result<(), EngineError> {
	device.params = preset
		.params
		.iter()
		.map(|(k, v)| ((*k).to_owned(), *v))
		.collect();
	device.zones = preset_zones(preset)?;
	device.modulators = preset_modulators(preset);
	Ok(())
}

/// What a freshly created built-in device starts with: the params and zones of its first
/// sampled factory preset (`sampler`: "Sine Pad", `drum_sampler`: "Synth Kit"), so it
/// makes sound out of the box. `None` for devices that do not play samples.
pub fn default_content(device_id: &str) -> Result<Option<DeviceContent>, EngineError> {
	if !cadenza_dsp::uses_samples(device_id) {
		return Ok(None);
	}
	let Some(preset) = cadenza_dsp::factory_presets(device_id).first() else {
		return Ok(None);
	};
	let params = preset
		.params
		.iter()
		.map(|(k, v)| ((*k).to_owned(), *v))
		.collect();
	Ok(Some(DeviceContent {
		params,
		zones: preset_zones(preset)?,
	}))
}

/// Explicit params and sample zones for a new device ([`default_content`]).
#[derive(Debug, Clone, PartialEq)]
pub struct DeviceContent {
	pub params: BTreeMap<String, f32>,
	pub zones: Vec<SampleZone>,
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn renders_once_and_regenerates_stale_content() {
		let dir = tempfile::tempdir().unwrap();
		let root = dir.path().join("factory");
		ensure_in(&root, None).unwrap();
		for (name, kind, ..) in specs() {
			let buf = cadenza_audio_io::decode_file(root.join(name)).unwrap();
			assert!(buf.peak() > 0.5 && buf.peak() < 0.95, "{name}");
			let secs = buf.duration_seconds();
			if kind == "tonal" {
				assert!((secs - 3.0).abs() < 0.01);
			} else {
				assert!(secs > 0.05 && secs < 3.1, "{name}: {secs}");
			}
		}
		let kick = root.join(names::KICK);
		let modified = std::fs::metadata(&kick).unwrap().modified().unwrap();
		ensure_in(&root, None).unwrap();
		assert_eq!(
			std::fs::metadata(&kick).unwrap().modified().unwrap(),
			modified,
			"complete content is reused"
		);
		std::fs::remove_file(&kick).unwrap();
		ensure_in(&root, None).unwrap();
		assert!(kick.is_file());
	}

	#[test]
	fn sine_pad_loop_is_seamless_enough_and_pitched() {
		let x = sine_pad(60);
		let hz = cadenza_dsp::util::pitch::detect_pitch(&x, RATE as f32).unwrap();
		assert_eq!(cadenza_dsp::util::pitch::hz_to_midi(hz), Some(60));
	}
}
