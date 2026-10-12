//! Samples for the built-in `sampler` and `drum_sampler`: loading files into zones and
//! pads, editing them, and the factory sample library. Every edit is one undoable
//! `SetSampleZones` (plus the output route a pad needs, in the same batch).

use std::path::{Path, PathBuf};

use cadenza_core::{DeviceId, TrackId};
use cadenza_model::{Command, Device, LoopMode, OutputRoute, SampleRef, SampleZone, ZoneSummary};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::convert::PitchInput;
use crate::error::McpError;
use crate::session::Session;

/// Drum lane / pad names accepted wherever a pad is expected (besides note names and
/// numbers and the drum_sampler's own pad names).
pub const DRUM_LANES: &[(&str, u8)] = &[
	("kick", 36),
	("kick2", 35),
	("rim", 37),
	("snare", 38),
	("clap", 39),
	("snare2", 40),
	("low_tom", 41),
	("closed_hat", 42),
	("hat", 42),
	("pedal_hat", 44),
	("mid_tom", 45),
	("open_hat", 46),
	("high_tom", 48),
	("crash", 49),
	("ride", 51),
	("tambourine", 54),
	("cowbell", 56),
];

/// The note of a pad given as a lane name ("kick", "open_hat"), a drum_sampler pad name
/// ("Closed Hat"), a note name ("C2") or a MIDI number ("36").
pub fn pad_note(name: &str) -> Result<u8, McpError> {
	let key = name
		.trim()
		.to_lowercase()
		.replace([' ', '-'], "_")
		.replace("_2", "2");
	if let Some((_, n)) = DRUM_LANES.iter().find(|(l, _)| *l == key) {
		return Ok(*n);
	}
	if let Some((n, _)) = cadenza_dsp::instruments::drum_sampler::PADS
		.iter()
		.find(|(_, p)| p.to_lowercase().replace([' ', '-'], "_").replace("_2", "2") == key)
	{
		return Ok(*n);
	}
	cadenza_theory::parse_pitch(name.trim()).map_err(|_| {
		McpError::Invalid(format!(
			"pad '{name}' is not a drum name or note; use kick, snare, clap, rim, closed_hat, \
			 pedal_hat, open_hat, low_tom, mid_tom, high_tom, crash, ride, tambourine, cowbell, \
			 a note name (\"C2\") or a MIDI number (36)"
		))
	})
}

fn text_or_number<'de, D: serde::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
	match serde_json::Value::deserialize(d)? {
		serde_json::Value::String(s) => Ok(s),
		serde_json::Value::Number(n) => Ok(n.to_string()),
		other => Err(serde::de::Error::custom(format!(
			"pad must be a drum name, note name or MIDI number, got {other}"
		))),
	}
}

fn opt_text_or_number<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
	text_or_number(d).map(Some)
}

/// The sample device on `track` (`device` or the track's instrument) and its builtin id.
fn sample_device(
	session: &Session,
	track: TrackId,
	device: Option<DeviceId>,
) -> Result<(Device, &'static str), McpError> {
	let project = session.document().project();
	let t = project
		.track(track)
		.ok_or(cadenza_model::ModelError::TrackNotFound(track))?;
	let d = match device {
		Some(id) => t.device(id).ok_or_else(|| {
			McpError::Invalid(format!(
				"device {id} is not on track '{}'; omit device_id to use the track's instrument",
				t.name
			))
		})?,
		None => t.instrument.as_ref().ok_or_else(|| {
			McpError::Invalid(format!(
				"track '{}' has no instrument; create one with create_track instrument \
				 \"sampler\" or \"drum_sampler\"",
				t.name
			))
		})?,
	};
	let kind = match d.builtin_id() {
		Some("sampler") => "sampler",
		Some("drum_sampler") => "drum_sampler",
		_ => {
			return Err(McpError::Invalid(format!(
				"device '{}' does not play samples; samples load into the built-in \"sampler\" or \
				 \"drum_sampler\" (set_instrument / create_track)",
				d.name
			)));
		}
	};
	Ok((d.clone(), kind))
}

/// Checks that `path` is a readable audio file.
fn check_audio(path: &Path) -> Result<cadenza_audio_io::AudioInfo, McpError> {
	cadenza_audio_io::probe(path).map_err(|e| {
		McpError::Invalid(format!(
			"cannot use '{}' as a sample: {e}; give the absolute path of a WAV, AIFF, FLAC, MP3 \
			 or OGG file (list_factory_samples lists the built-in ones)",
			path.display()
		))
	})
}

fn summaries(zones: &[SampleZone]) -> Vec<ZoneSummary> {
	cadenza_model::zone_summaries(zones)
}

/// Executes the zone change, plus a route to the track for pad outputs that have none
/// (an unrouted extra output would be silent).
fn commit(
	session: &mut Session,
	track: TrackId,
	device: &Device,
	zones: Vec<SampleZone>,
	label: &str,
) -> Result<Vec<String>, McpError> {
	let mut notes = Vec::new();
	let is_instrument = session
		.document()
		.project()
		.track(track)
		.and_then(|t| t.instrument.as_ref())
		.is_some_and(|i| i.id == device.id);
	let mut routes = device.aux_outputs.clone();
	for port in zones.iter().map(|z| u32::from(z.output)).filter(|p| *p > 0) {
		if port as usize > cadenza_dsp::instruments::drum_sampler::AUX_OUTPUTS {
			return Err(McpError::Invalid(format!(
				"output {port} does not exist; the drum_sampler has extra outputs 1..=8 (0 = main)"
			)));
		}
		if is_instrument && !routes.iter().any(|r| r.port == port) {
			routes.push(OutputRoute { port, bus: None });
			notes.push(format!(
				"extra output {port} now plays on the track itself; send it to a bus with \
				 set_output_routes"
			));
		}
	}
	let mut commands = vec![Command::SetSampleZones {
		track: Some(track),
		device: device.id,
		zones,
	}];
	if routes != device.aux_outputs {
		routes.sort_by_key(|r| r.port);
		commands.push(Command::SetOutputRoutes { track, routes });
	}
	let cmd = if commands.len() == 1 {
		commands.pop().expect("one command")
	} else {
		Command::Batch {
			label: label.to_owned(),
			commands,
		}
	};
	session.document_mut().execute(cmd)?;
	Ok(notes)
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct LoadSampleArgs {
	/// Track whose sampler / drum_sampler receives the sample.
	pub track_id: TrackId,
	/// The device on that track; omit for the track's instrument.
	#[serde(default)]
	pub device_id: Option<DeviceId>,
	/// Absolute path of the audio file (see list_factory_samples for built-in sounds).
	pub path: String,
	/// drum_sampler: the pad — a drum name (kick, snare, clap, rim, closed_hat, pedal_hat,
	/// open_hat, low_tom, mid_tom, high_tom, crash, ride, tambourine, cowbell), a note name
	/// ("C2") or a MIDI number (36). Replaces the pad's sample, keeping its settings.
	#[serde(default, deserialize_with = "opt_text_or_number")]
	pub pad: Option<String>,
	/// drum_sampler: add the file as another layer of the pad instead of replacing it
	/// (then split the layers with set_zone vel_low/vel_high or round_robin).
	#[serde(default)]
	pub layer: bool,
	/// sampler: replace the sample of this zone index (keeps its mapping).
	#[serde(default)]
	pub zone: Option<usize>,
	/// sampler: add a new zone on this key range ("C2-B3", "60-72" or one note) instead of
	/// mapping the file across the whole keyboard.
	#[serde(default)]
	pub keys: Option<String>,
	/// Note that plays the file at its recorded pitch. Default: from the file name
	/// ("Piano C3.wav"), else detected pitch, else C4.
	#[serde(default)]
	pub root: Option<PitchInput>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct LoadSampleOutput {
	/// What happened, in one line.
	pub loaded: String,
	pub root: String,
	pub duration_seconds: f64,
	/// The device's zones after the change.
	pub zones: Vec<ZoneSummary>,
	#[serde(skip_serializing_if = "Vec::is_empty")]
	pub notes: Vec<String>,
}

fn parse_keys(keys: &str) -> Result<(u8, u8), McpError> {
	let bad = || {
		McpError::Invalid(format!(
			"keys '{keys}' is not a key range; use \"C2-B3\", \"48-59\" or a single note \"C3\""
		))
	};
	// "C-1" contains a dash: split on the dash that follows a digit.
	let split = keys
		.char_indices()
		.find(|&(i, c)| {
			c == '-'
				&& i > 0 && keys[..i].chars().last().is_some_and(|p| p.is_ascii_digit())
				&& i + 1 < keys.len()
		})
		.map(|(i, _)| (&keys[..i], &keys[i + 1..]));
	let (a, b) = split.unwrap_or((keys, keys));
	let lo = cadenza_theory::parse_pitch(a.trim()).map_err(|_| bad())?;
	let hi = cadenza_theory::parse_pitch(b.trim()).map_err(|_| bad())?;
	Ok((lo.min(hi), lo.max(hi)))
}

pub fn load_sample(
	session: &mut Session,
	args: LoadSampleArgs,
) -> Result<LoadSampleOutput, McpError> {
	let (device, kind) = sample_device(session, args.track_id, args.device_id)?;
	let path = PathBuf::from(args.path.trim());
	if !path.is_absolute() {
		return Err(McpError::Invalid(format!(
			"path '{}' must be absolute (e.g. from list_factory_samples or the project folder)",
			args.path
		)));
	}
	let info = check_audio(&path)?;
	let detected =
		|| cadenza_engine::detect_root(&path, cadenza_audio_io::decode_file(&path).ok().as_ref());
	let mut zones = device.zones.clone();
	let sample = SampleRef::new(&path);
	let (loaded, root) = if kind == "drum_sampler" {
		if args.zone.is_some() || args.keys.is_some() {
			return Err(McpError::Invalid(
				"the drum_sampler is played by pads: pass pad (e.g. \"snare\") instead of zone/keys"
					.into(),
			));
		}
		let pad = args.pad.as_deref().ok_or_else(|| {
			McpError::Invalid(
				"pass pad: which drum this sample is (\"kick\", \"snare\", \"closed_hat\", a note \
				 like \"C2\" or 36)"
					.into(),
			)
		})?;
		let note = pad_note(pad)?;
		let root = args.root.map_or(note, |r| r.0);
		let existing = zones
			.iter()
			.position(|z| z.is_single_key() && z.key_low == note);
		let mut zone = match existing {
			Some(i) if !args.layer => {
				let mut z = zones[i].clone();
				z.sample = sample;
				z.start = 0.0;
				z.end = None;
				z.loop_mode = LoopMode::Off;
				z.loop_start = 0.0;
				z.loop_end = None;
				z
			}
			_ => SampleZone::new(sample, note, note, root),
		};
		zone.root_key = root;
		if args.layer {
			zones.push(zone);
		} else {
			zones.retain(|z| !(z.is_single_key() && z.key_low == note));
			zones.push(zone);
			zones.sort_by_key(|z| z.key_low);
		}
		let what = if args.layer {
			"added a layer to"
		} else {
			"loaded into"
		};
		(
			format!(
				"{} {what} pad {} ({note})",
				path.display(),
				cadenza_model::note_name(note)
			),
			root,
		)
	} else {
		if args.pad.is_some() {
			return Err(McpError::Invalid(
				"pad is for the drum_sampler; on the sampler pass zone (index) or keys (range), or \
				 nothing to map the file across the keyboard"
					.into(),
			));
		}
		match (args.zone, &args.keys) {
			(Some(i), _) => {
				let count = zones.len();
				let z = zones.get_mut(i).ok_or_else(|| {
					McpError::Invalid(format!(
						"zone {i} does not exist; this sampler has {count} zones (0..{count})"
					))
				})?;
				z.sample = sample;
				z.root_key = args.root.map_or_else(
					|| {
						let name = path.file_name().map(|n| n.to_string_lossy().into_owned());
						if name
							.as_deref()
							.and_then(cadenza_model::root_from_file_name)
							.is_some()
						{
							detected()
						} else {
							z.root_key
						}
					},
					|r| r.0,
				);
				z.start = 0.0;
				z.end = None;
				z.loop_end = None;
				z.loop_start = 0.0;
				(format!("{} replaces zone {i}", path.display()), z.root_key)
			}
			(None, Some(keys)) => {
				let (lo, hi) = parse_keys(keys)?;
				let root = args.root.map_or_else(detected, |r| r.0);
				zones.push(SampleZone::new(sample, lo, hi, root));
				(
					format!(
						"{} mapped to {}..{}",
						path.display(),
						cadenza_model::note_name(lo),
						cadenza_model::note_name(hi)
					),
					root,
				)
			}
			(None, None) => {
				let root = args.root.map_or_else(detected, |r| r.0);
				zones = vec![SampleZone::full_keyboard(&path, root)];
				(
					format!(
						"{} mapped across the keyboard (previous zones replaced)",
						path.display()
					),
					root,
				)
			}
		}
	};
	let notes = commit(
		session,
		args.track_id,
		&device,
		zones.clone(),
		"Load sample",
	)?;
	Ok(LoadSampleOutput {
		loaded,
		root: format!("{} ({root})", cadenza_model::note_name(root)),
		duration_seconds: info.duration_seconds,
		zones: summaries(&zones),
		notes,
	})
}

/// Settings shared by set_zone and set_pad; omitted fields keep their value.
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ZonePatch {
	/// Level in dB (-60..=24).
	#[serde(default)]
	pub gain_db: Option<f32>,
	/// -1 (left) ..= 1 (right).
	#[serde(default)]
	pub pan: Option<f32>,
	/// Pitch offset in semitones (-48..=48; 0.1 = 10 cents).
	#[serde(default)]
	pub tune: Option<f32>,
	/// Fade-in in ms (0..=10000).
	#[serde(default)]
	pub attack_ms: Option<f32>,
	/// Decay to silence in ms (1..=30000); 0 lets the sample ring out.
	#[serde(default)]
	pub decay_ms: Option<f32>,
	/// Sample start offset in seconds.
	#[serde(default)]
	pub start: Option<f64>,
	/// Playback end in seconds; 0 = the end of the file.
	#[serde(default)]
	pub end: Option<f64>,
	/// Play backwards.
	#[serde(default)]
	pub reverse: Option<bool>,
	/// Choke group 1..=16 (0 = none): hats in one group cut each other.
	#[serde(default)]
	pub choke: Option<u8>,
	/// 0 = main output, 1..=8 = extra output (drum_sampler) for its own mixer bus.
	#[serde(default)]
	pub output: Option<u8>,
	/// Round-robin group 1..=16 (0 = none): matching zones of a group take turns.
	#[serde(default)]
	pub round_robin: Option<u8>,
	/// Velocity range 0..=1 of this zone (velocity layers).
	#[serde(default)]
	pub vel_low: Option<f32>,
	#[serde(default)]
	pub vel_high: Option<f32>,
}

impl ZonePatch {
	fn apply(&self, z: &mut SampleZone) {
		if let Some(v) = self.gain_db {
			z.gain_db = v;
		}
		if let Some(v) = self.pan {
			z.pan = v;
		}
		if let Some(v) = self.tune {
			z.tune = v;
		}
		if let Some(v) = self.attack_ms {
			z.attack_ms = v;
		}
		if let Some(v) = self.decay_ms {
			z.decay_ms = (v > 0.0).then_some(v);
		}
		if let Some(v) = self.start {
			z.start = v;
		}
		if let Some(v) = self.end {
			z.end = (v > 0.0).then_some(v);
		}
		if let Some(v) = self.reverse {
			z.reverse = v;
		}
		if let Some(v) = self.choke {
			z.choke = v;
		}
		if let Some(v) = self.output {
			z.output = v;
		}
		if let Some(v) = self.round_robin {
			z.round_robin = v;
		}
		if let Some(v) = self.vel_low {
			z.vel_low = v;
		}
		if let Some(v) = self.vel_high {
			z.vel_high = v;
		}
	}
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SetZoneArgs {
	pub track_id: TrackId,
	/// Omit for the track's instrument.
	#[serde(default)]
	pub device_id: Option<DeviceId>,
	/// Zone index (from get_project's instrument zones).
	pub zone: usize,
	/// Delete the zone instead of editing it.
	#[serde(default)]
	pub remove: bool,
	/// Lowest note of the zone.
	#[serde(default)]
	pub key_low: Option<PitchInput>,
	/// Highest note of the zone.
	#[serde(default)]
	pub key_high: Option<PitchInput>,
	/// Note that plays the sample at its recorded pitch.
	#[serde(default)]
	pub root: Option<PitchInput>,
	/// off | forward | ping_pong.
	#[serde(default)]
	pub loop_mode: Option<LoopMode>,
	/// Loop start in seconds.
	#[serde(default)]
	pub loop_start: Option<f64>,
	/// Loop end in seconds; 0 = the playback end.
	#[serde(default)]
	pub loop_end: Option<f64>,
	/// Crossfade at the loop seam in seconds (0.01-0.1 removes clicks).
	#[serde(default)]
	pub loop_crossfade: Option<f64>,
	#[serde(flatten)]
	pub patch: ZonePatch,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ZonesOutput {
	pub zones: Vec<ZoneSummary>,
	#[serde(skip_serializing_if = "Vec::is_empty")]
	pub notes: Vec<String>,
}

pub fn set_zone(session: &mut Session, args: SetZoneArgs) -> Result<ZonesOutput, McpError> {
	let (device, _) = sample_device(session, args.track_id, args.device_id)?;
	let mut zones = device.zones.clone();
	let count = zones.len();
	if args.zone >= count {
		return Err(McpError::Invalid(format!(
			"zone {} does not exist; this device has {count} zones (0..{count}); add one with \
			 load_sample",
			args.zone
		)));
	}
	if args.remove {
		zones.remove(args.zone);
	} else {
		let z = &mut zones[args.zone];
		if let Some(k) = args.key_low {
			z.key_low = k.0;
		}
		if let Some(k) = args.key_high {
			z.key_high = k.0;
		}
		if let Some(k) = args.root {
			z.root_key = k.0;
		}
		if let Some(m) = args.loop_mode {
			z.loop_mode = m;
		}
		if let Some(v) = args.loop_start {
			z.loop_start = v;
		}
		if let Some(v) = args.loop_end {
			z.loop_end = (v > 0.0).then_some(v);
		}
		if let Some(v) = args.loop_crossfade {
			z.loop_crossfade = v;
		}
		args.patch.apply(z);
	}
	let notes = commit(
		session,
		args.track_id,
		&device,
		zones.clone(),
		"Edit sample zone",
	)?;
	Ok(ZonesOutput {
		zones: summaries(&zones),
		notes,
	})
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SetPadArgs {
	pub track_id: TrackId,
	/// Omit for the track's instrument.
	#[serde(default)]
	pub device_id: Option<DeviceId>,
	/// Drum name ("snare", "open_hat"...), note name ("D2") or MIDI number (38).
	#[serde(deserialize_with = "text_or_number")]
	pub pad: String,
	/// Unload the pad (every layer).
	#[serde(default)]
	pub clear: bool,
	#[serde(flatten)]
	pub patch: ZonePatch,
}

pub fn set_pad(session: &mut Session, args: SetPadArgs) -> Result<ZonesOutput, McpError> {
	let (device, kind) = sample_device(session, args.track_id, args.device_id)?;
	if kind != "drum_sampler" {
		return Err(McpError::Invalid(
			"set_pad edits drum_sampler pads; edit sampler zones with set_zone".into(),
		));
	}
	let note = pad_note(&args.pad)?;
	let mut zones = device.zones.clone();
	let on_pad = |z: &SampleZone| z.is_single_key() && z.key_low == note;
	if !zones.iter().any(on_pad) {
		return Err(McpError::Invalid(format!(
			"pad '{}' ({note}) has no sample; load one first with load_sample pad \"{}\"",
			args.pad, args.pad
		)));
	}
	if args.clear {
		zones.retain(|z| !on_pad(z));
	} else {
		zones
			.iter_mut()
			.filter(|z| on_pad(z))
			.for_each(|z| args.patch.apply(z));
	}
	let notes = commit(
		session,
		args.track_id,
		&device,
		zones.clone(),
		"Edit drum pad",
	)?;
	Ok(ZonesOutput {
		zones: summaries(&zones),
		notes,
	})
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ListFactorySamplesOutput {
	/// Folder holding them (synthesized on first use).
	pub folder: String,
	pub samples: Vec<cadenza_engine::factory::FactorySample>,
	/// Ready-made kits / instruments built from them (load_preset on the device).
	pub presets: Vec<String>,
}

pub fn list_factory_samples() -> Result<ListFactorySamplesOutput, McpError> {
	let samples = cadenza_engine::factory::list_factory_samples()?;
	let presets = ["drum_sampler", "sampler"]
		.iter()
		.flat_map(|id| {
			cadenza_dsp::factory_presets(id)
				.iter()
				.map(move |p| format!("{id}: {}", p.name))
		})
		.collect();
	Ok(ListFactorySamplesOutput {
		folder: cadenza_engine::factory::factory_sample_dir()
			.display()
			.to_string(),
		samples,
		presets,
	})
}

/// The factory sample folder (samples there are always allowed to load).
pub fn factory_folder() -> PathBuf {
	cadenza_engine::factory::factory_sample_dir()
}
