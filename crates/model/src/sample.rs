//! Sample zones: the structured state of the sample-based built-in devices (`sampler`,
//! `drum_sampler`), stored in [`crate::Device::zones`] and replaced as a whole by
//! `Command::SetSampleZones`.
//!
//! Sample files are referenced by absolute path, like audio clips. Each reference also
//! keeps the file's original name, so a later "collect project files" can copy the file
//! into the project folder (renaming on conflicts) and still show what it was.

use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::ModelError;

/// Most zones one device may hold (a big multisample is a few hundred).
pub const MAX_ZONES: usize = 1024;
/// Highest extra output a zone can be routed to.
pub const MAX_ZONE_OUTPUT: u8 = 16;
/// Highest choke and round-robin group.
pub const MAX_GROUP: u8 = 16;

/// An audio file used by a sample-based device.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct SampleRef {
	/// Absolute path of the file.
	pub path: PathBuf,
	/// Original file name (kept when the file is relocated); empty = the path's file name.
	#[serde(default, skip_serializing_if = "String::is_empty")]
	pub name: String,
}

impl SampleRef {
	/// A reference to `path`, named after its file name.
	pub fn new(path: impl Into<PathBuf>) -> Self {
		let path = path.into();
		let name = path
			.file_name()
			.map(|n| n.to_string_lossy().into_owned())
			.unwrap_or_default();
		Self { path, name }
	}

	/// The original file name, falling back to the path's.
	pub fn display_name(&self) -> String {
		if self.name.is_empty() {
			self.path
				.file_name()
				.map(|n| n.to_string_lossy().into_owned())
				.unwrap_or_else(|| self.path.display().to_string())
		} else {
			self.name.clone()
		}
	}

	/// The file name without its extension ("Kick" for "Kick.wav").
	pub fn stem(&self) -> String {
		let name = self.display_name();
		Path::new(&name)
			.file_stem()
			.map(|s| s.to_string_lossy().into_owned())
			.unwrap_or(name)
	}
}

/// How a zone loops.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LoopMode {
	#[default]
	Off,
	/// From the loop end back to the loop start, while the note is held.
	Forward,
	/// Back and forth between the loop points.
	PingPong,
}

impl LoopMode {
	pub fn is_off(&self) -> bool {
		*self == LoopMode::Off
	}
}

fn is_zero(v: &f32) -> bool {
	*v == 0.0
}

fn is_zero_f64(v: &f64) -> bool {
	*v == 0.0
}

fn is_zero_u8(v: &u8) -> bool {
	*v == 0
}

fn is_false(v: &bool) -> bool {
	!*v
}

fn one() -> f32 {
	1.0
}

fn is_one(v: &f32) -> bool {
	*v == 1.0
}

/// One sample mapped onto a key and velocity range. A `sampler` plays the zones whose
/// ranges contain a note; a `drum_sampler` pad is the set of zones on its note (several
/// = velocity layers or round robin). Positions are in seconds of the sample file (they
/// do not depend on the tempo or the engine rate). Every field but `sample` and the keys
/// is optional in JSON.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SampleZone {
	pub sample: SampleRef,
	/// Lowest MIDI note of the zone (0..=127).
	pub key_low: u8,
	/// Highest MIDI note (>= key_low).
	pub key_high: u8,
	/// Note that plays the sample at its recorded pitch (60 = C4).
	pub root_key: u8,
	/// Velocity range 0..=1 (inclusive): velocity layers.
	#[serde(default, skip_serializing_if = "is_zero")]
	pub vel_low: f32,
	#[serde(default = "one", skip_serializing_if = "is_one")]
	pub vel_high: f32,
	/// Pitch offset in semitones (-48..=48; fractions are fine tune: 0.1 = 10 cents).
	#[serde(default, skip_serializing_if = "is_zero")]
	pub tune: f32,
	/// -60..=24 dB.
	#[serde(default, skip_serializing_if = "is_zero")]
	pub gain_db: f32,
	/// -1 (left) ..= 1 (right).
	#[serde(default, skip_serializing_if = "is_zero")]
	pub pan: f32,
	/// Playback start, seconds into the file (sample start offset).
	#[serde(default, skip_serializing_if = "is_zero_f64")]
	pub start: f64,
	/// Playback end in seconds (> start); missing = the end of the file.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub end: Option<f64>,
	#[serde(default, skip_serializing_if = "LoopMode::is_off")]
	pub loop_mode: LoopMode,
	/// Loop start in seconds (inside start..end).
	#[serde(default, skip_serializing_if = "is_zero_f64")]
	pub loop_start: f64,
	/// Loop end in seconds (> loop_start); missing = the playback end.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub loop_end: Option<f64>,
	/// Crossfade at the loop seam in seconds (removes clicks; 0 = none).
	#[serde(default, skip_serializing_if = "is_zero_f64")]
	pub loop_crossfade: f64,
	/// Play the region backwards (loops are ignored).
	#[serde(default, skip_serializing_if = "is_false")]
	pub reverse: bool,
	/// Fade-in of this zone in ms (0..=10000).
	#[serde(default, skip_serializing_if = "is_zero")]
	pub attack_ms: f32,
	/// Decay of this zone to silence in ms (1..=30000); missing = the sample rings out.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub decay_ms: Option<f32>,
	/// Choke group 1..=16: starting a zone of the group cuts the others (closed hat chokes
	/// open hat). 0 = none.
	#[serde(default, skip_serializing_if = "is_zero_u8")]
	pub choke: u8,
	/// 0 = main output; n = extra output n of the instrument (route it with
	/// `SetOutputRoutes`; an unrouted extra output is silent).
	#[serde(default, skip_serializing_if = "is_zero_u8")]
	pub output: u8,
	/// Round-robin group 1..=16: zones of a group matching the same note take turns.
	/// 0 = every matching zone plays.
	#[serde(default, skip_serializing_if = "is_zero_u8")]
	pub round_robin: u8,
}

impl SampleZone {
	/// `sample` across `key_low..=key_high`, at its recorded pitch on `root_key`, every
	/// other setting at its default.
	pub fn new(sample: SampleRef, key_low: u8, key_high: u8, root_key: u8) -> Self {
		Self {
			sample,
			key_low,
			key_high,
			root_key,
			vel_low: 0.0,
			vel_high: 1.0,
			tune: 0.0,
			gain_db: 0.0,
			pan: 0.0,
			start: 0.0,
			end: None,
			loop_mode: LoopMode::Off,
			loop_start: 0.0,
			loop_end: None,
			loop_crossfade: 0.0,
			reverse: false,
			attack_ms: 0.0,
			decay_ms: None,
			choke: 0,
			output: 0,
			round_robin: 0,
		}
	}

	/// The whole keyboard playing `path` at its pitch on `root_key` (sampler auto-map).
	pub fn full_keyboard(path: impl Into<PathBuf>, root_key: u8) -> Self {
		Self::new(SampleRef::new(path), 0, 127, root_key.min(127))
	}

	/// A drum pad: `path` on the single note `note`, at its recorded pitch.
	pub fn pad(path: impl Into<PathBuf>, note: u8) -> Self {
		let note = note.min(127);
		Self::new(SampleRef::new(path), note, note, note)
	}

	/// Whether a note-on of `pitch` at `velocity` plays this zone.
	pub fn matches(&self, pitch: u8, velocity: f32) -> bool {
		(self.key_low..=self.key_high).contains(&pitch)
			&& velocity >= self.vel_low
			&& velocity <= self.vel_high
	}

	/// Whether the zone sits on exactly one note (a drum pad layer).
	pub fn is_single_key(&self) -> bool {
		self.key_low == self.key_high
	}
}

/// Checks one zone; `index` names it in errors.
pub(crate) fn validate_zone(index: usize, z: &SampleZone) -> Result<(), ModelError> {
	let what = |field: &str| format!("zone {index} {field}");
	let invalid = |field: &str, value: String, reason: &str| {
		Err(ModelError::invalid(what(field), value, reason))
	};
	if z.sample.path.as_os_str().is_empty() {
		return invalid(
			"sample.path",
			"\"\"".into(),
			"must be the path of an audio file",
		);
	}
	for (field, key) in [
		("key_low", z.key_low),
		("key_high", z.key_high),
		("root_key", z.root_key),
	] {
		if key > 127 {
			return invalid(field, key.to_string(), "MIDI notes are 0..=127 (60 = C4)");
		}
	}
	if z.key_low > z.key_high {
		return invalid(
			"key_low",
			z.key_low.to_string(),
			"must not be above key_high",
		);
	}
	let finite = |field: &str, v: f64| -> Result<(), ModelError> {
		if v.is_finite() {
			Ok(())
		} else {
			invalid(field, v.to_string(), "must be a finite number")
		}
	};
	for (field, v) in [
		("vel_low", f64::from(z.vel_low)),
		("vel_high", f64::from(z.vel_high)),
		("tune", f64::from(z.tune)),
		("gain_db", f64::from(z.gain_db)),
		("pan", f64::from(z.pan)),
		("start", z.start),
		("loop_start", z.loop_start),
		("loop_crossfade", z.loop_crossfade),
		("attack_ms", f64::from(z.attack_ms)),
	] {
		finite(field, v)?;
	}
	if !(0.0..=1.0).contains(&z.vel_low) || !(0.0..=1.0).contains(&z.vel_high) {
		return invalid(
			"velocity range",
			format!("{}..{}", z.vel_low, z.vel_high),
			"velocities are 0..=1",
		);
	}
	if z.vel_low > z.vel_high {
		return invalid(
			"vel_low",
			z.vel_low.to_string(),
			"must not be above vel_high",
		);
	}
	if !(-48.0..=48.0).contains(&z.tune) {
		return invalid("tune", z.tune.to_string(), "must be -48..=48 semitones");
	}
	if !(-60.0..=24.0).contains(&z.gain_db) {
		return invalid("gain_db", z.gain_db.to_string(), "must be -60..=24 dB");
	}
	if !(-1.0..=1.0).contains(&z.pan) {
		return invalid("pan", z.pan.to_string(), "must be -1 (left)..=1 (right)");
	}
	if z.start < 0.0 {
		return invalid("start", z.start.to_string(), "must be >= 0 seconds");
	}
	if let Some(end) = z.end {
		finite("end", end)?;
		if end <= z.start {
			return invalid(
				"end",
				end.to_string(),
				"must be after start (seconds); omit it to play to the end of the file",
			);
		}
	}
	if z.loop_start < 0.0 {
		return invalid(
			"loop_start",
			z.loop_start.to_string(),
			"must be >= 0 seconds",
		);
	}
	if let Some(loop_end) = z.loop_end {
		finite("loop_end", loop_end)?;
		if loop_end <= z.loop_start {
			return invalid(
				"loop_end",
				loop_end.to_string(),
				"must be after loop_start (seconds); omit it to loop up to the playback end",
			);
		}
	}
	if !(0.0..=10.0).contains(&z.loop_crossfade) {
		return invalid(
			"loop_crossfade",
			z.loop_crossfade.to_string(),
			"must be 0..=10 seconds",
		);
	}
	if !(0.0..=10_000.0).contains(&z.attack_ms) {
		return invalid("attack_ms", z.attack_ms.to_string(), "must be 0..=10000 ms");
	}
	if let Some(decay) = z.decay_ms
		&& (!decay.is_finite() || !(1.0..=30_000.0).contains(&decay))
	{
		return invalid(
			"decay_ms",
			decay.to_string(),
			"must be 1..=30000 ms; omit it to let the sample ring out",
		);
	}
	if z.choke > MAX_GROUP {
		return invalid(
			"choke",
			z.choke.to_string(),
			"choke groups are 1..=16 (0 = none)",
		);
	}
	if z.round_robin > MAX_GROUP {
		return invalid(
			"round_robin",
			z.round_robin.to_string(),
			"round-robin groups are 1..=16 (0 = none)",
		);
	}
	if z.output > MAX_ZONE_OUTPUT {
		return invalid(
			"output",
			z.output.to_string(),
			"outputs are 0 (main) or an extra output 1..=16",
		);
	}
	Ok(())
}

/// Checks a whole zone map.
pub(crate) fn validate_zones(zones: &[SampleZone]) -> Result<(), ModelError> {
	if zones.len() > MAX_ZONES {
		return Err(ModelError::invalid(
			"zones",
			zones.len().to_string(),
			"a device holds at most 1024 zones",
		));
	}
	zones
		.iter()
		.enumerate()
		.try_for_each(|(i, z)| validate_zone(i, z))
}

/// The root key a sample's file name announces: a note name token ("Piano C3.wav",
/// "pad_F#2", "Bass-Eb1") or a MIDI number token ("Strings_60.wav"), the last one
/// winning. Uses this project's convention, 60 = C4. `None` when the name has neither.
pub fn root_from_file_name(name: &str) -> Option<u8> {
	let stem = Path::new(name)
		.file_stem()
		.map(|s| s.to_string_lossy().into_owned())
		.unwrap_or_else(|| name.to_owned());
	let tokens: Vec<&str> = stem
		.split(|c: char| c.is_whitespace() || "_-.()[]{},+".contains(c))
		.filter(|t| !t.is_empty())
		.collect();
	// Note names first ("C-1" splits into "C" and "1": rejoin through the raw stem).
	let note = tokens.iter().rev().find_map(|t| parse_note_token(t));
	if note.is_some() {
		return note;
	}
	if let Some(n) = negative_octave_note(&stem) {
		return Some(n);
	}
	tokens.iter().rev().find_map(|t| {
		let digits = t.len() >= 2 && t.len() <= 3 && t.chars().all(|c| c.is_ascii_digit());
		if !digits || t.starts_with('0') {
			return None;
		}
		t.parse::<u8>().ok().filter(|n| *n <= 127)
	})
}

/// "C3", "c#4", "Db2", "A0" -> MIDI (60 = C4).
fn parse_note_token(token: &str) -> Option<u8> {
	let mut chars = token.chars();
	let letter = chars.next()?.to_ascii_uppercase();
	let base: i32 = match letter {
		'C' => 0,
		'D' => 2,
		'E' => 4,
		'F' => 5,
		'G' => 7,
		'A' => 9,
		'B' => 11,
		_ => return None,
	};
	let rest: String = chars.collect();
	let (accidental, octave) = if let Some(r) = rest.strip_prefix('#') {
		(1, r)
	} else if let Some(r) = rest.strip_prefix("♯") {
		(1, r)
	} else if let Some(r) = rest.strip_prefix('b') {
		(-1, r)
	} else if let Some(r) = rest.strip_prefix("♭") {
		(-1, r)
	} else {
		(0, rest.as_str())
	};
	if octave.is_empty() || octave.len() > 2 || !octave.chars().all(|c| c.is_ascii_digit()) {
		return None;
	}
	let octave: i32 = octave.parse().ok()?;
	let midi = (octave + 1) * 12 + base + accidental;
	u8::try_from(midi).ok().filter(|m| *m <= 127)
}

/// "C-1" style names (octave -1), which the tokenizer splits.
fn negative_octave_note(stem: &str) -> Option<u8> {
	let bytes: Vec<char> = stem.chars().collect();
	for i in 0..bytes.len() {
		let c = bytes[i].to_ascii_uppercase();
		if !('A'..='G').contains(&c) {
			continue;
		}
		if i > 0 && bytes[i - 1].is_ascii_alphanumeric() {
			continue;
		}
		let mut j = i + 1;
		let mut accidental = String::new();
		if j < bytes.len() && (bytes[j] == '#' || bytes[j] == 'b') {
			accidental.push(bytes[j]);
			j += 1;
		}
		if j + 1 < bytes.len()
			&& bytes[j] == '-'
			&& bytes[j + 1] == '1'
			&& bytes.get(j + 2).is_none_or(|c| !c.is_ascii_alphanumeric())
		{
			let token = format!("{c}{accidental}0");
			return parse_note_token(&token).and_then(|m| m.checked_sub(12));
		}
	}
	None
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn roots_come_from_note_names_or_midi_numbers() {
		assert_eq!(root_from_file_name("Piano C4.wav"), Some(60));
		assert_eq!(root_from_file_name("pad_F#2.flac"), Some(42));
		assert_eq!(root_from_file_name("Bass-Eb1.wav"), Some(27));
		assert_eq!(root_from_file_name("Strings_60.wav"), Some(60));
		assert_eq!(root_from_file_name("Sine Pad A3 loop.wav"), Some(57));
		assert_eq!(root_from_file_name("lead c-1.wav"), Some(0));
		assert_eq!(root_from_file_name("Kick 01.wav"), None);
		assert_eq!(root_from_file_name("Kick.wav"), None);
		assert_eq!(root_from_file_name("Break 174bpm.wav"), None);
		assert_eq!(root_from_file_name("Vox_200.wav"), None);
		// Note names beat numbers, the last token wins.
		assert_eq!(root_from_file_name("Take 72 G3.wav"), Some(55));
	}

	#[test]
	fn zones_validate_ranges_with_actionable_messages() {
		let ok = SampleZone::full_keyboard("/s/a.wav", 60);
		assert!(validate_zone(0, &ok).is_ok());
		let mut z = ok.clone();
		z.key_low = 70;
		z.key_high = 60;
		assert!(
			validate_zone(3, &z)
				.unwrap_err()
				.to_string()
				.contains("zone 3 key_low")
		);
		let mut z = ok.clone();
		z.end = Some(0.0);
		assert!(validate_zone(0, &z).is_err());
		let mut z = ok.clone();
		z.vel_low = 0.8;
		z.vel_high = 0.2;
		assert!(validate_zone(0, &z).is_err());
		let mut z = ok.clone();
		z.choke = 17;
		assert!(validate_zone(0, &z).is_err());
		let mut z = ok;
		z.decay_ms = Some(f32::NAN);
		assert!(validate_zone(0, &z).is_err());
	}

	#[test]
	fn defaults_are_omitted_in_json() {
		let z = SampleZone::pad("/kit/Kick.wav", 36);
		let json = serde_json::to_value(&z).unwrap();
		let obj = json.as_object().unwrap();
		assert_eq!(obj.len(), 4, "{json}");
		assert_eq!(json["sample"]["name"], "Kick.wav");
		let back: SampleZone = serde_json::from_value(json).unwrap();
		assert_eq!(back, z);
		assert_eq!(back.sample.stem(), "Kick");
	}
}
