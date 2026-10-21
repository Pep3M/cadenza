//! Modulation sources of a built-in device: LFOs and envelope followers that move
//! parameters around their set (or automated) value. Stored on [`crate::Device`] and
//! replaced as a whole by `Command::SetModulators`; the engine runs them per quantum.

use cadenza_core::{DeviceId, ModulatorId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::ModelError;

/// At most this many modulators per device.
pub const MAX_MODULATORS: usize = 8;
/// At most this many targets per modulator.
pub const MAX_TARGETS: usize = 16;
pub const RATE_HZ_RANGE: (f32, f32) = (0.01, 40.0);
/// Cycle length range in quarter notes for a tempo-synced LFO.
pub const SYNC_BEATS_RANGE: (f32, f32) = (0.0625, 64.0);
pub const ATTACK_MS_RANGE: (f32, f32) = (0.1, 1000.0);
pub const RELEASE_MS_RANGE: (f32, f32) = (1.0, 5000.0);
pub const GAIN_DB_RANGE: (f32, f32) = (-24.0, 24.0);

/// Common tempo-synced cycle lengths: `(name, quarter notes)`.
pub const SYNC_STEPS: [(&str, f32); 13] = [
	("1/32", 0.125),
	("1/16T", 1.0 / 6.0),
	("1/16", 0.25),
	("1/8T", 1.0 / 3.0),
	("1/8", 0.5),
	("1/8.", 0.75),
	("1/4T", 2.0 / 3.0),
	("1/4", 1.0),
	("1/4.", 1.5),
	("1/2", 2.0),
	("1 bar", 4.0),
	("2 bars", 8.0),
	("4 bars", 16.0),
];

/// The name of a synced cycle length ("1/8", "1/4.", or "3 beats").
pub fn sync_name(beats: f32) -> String {
	SYNC_STEPS
		.iter()
		.find(|(_, b)| (b - beats).abs() < 1e-4)
		.map_or_else(
			|| format!("{} beats", round3(beats)),
			|(n, _)| (*n).to_string(),
		)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Modulator {
	pub id: ModulatorId,
	#[serde(flatten)]
	pub kind: ModulatorKind,
	/// Parameters it moves (each by its own depth).
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub targets: Vec<ModTarget>,
}

/// What produces the modulation signal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ModulatorKind {
	/// Low-frequency oscillator, bipolar (-1..1): the parameter swings both ways around
	/// its value.
	Lfo {
		#[serde(default)]
		shape: LfoShape,
		/// Free rate in Hz (used when `sync_beats` is not set).
		#[serde(default = "default_rate")]
		rate_hz: f32,
		/// Tempo-synced cycle length in quarter notes (0.5 = 1/8, 4 = one 4/4 bar);
		/// overrides `rate_hz`.
		#[serde(default, skip_serializing_if = "Option::is_none")]
		sync_beats: Option<f32>,
		/// Start phase, 0..1 of a cycle.
		#[serde(default, skip_serializing_if = "is_zero")]
		phase: f32,
		#[serde(default)]
		retrigger: Retrigger,
	},
	/// Envelope follower, unipolar (0..1 over a -60..0 dBFS window after `gain_db`): the
	/// parameter moves from its value toward `depth` as the signal gets louder.
	Envelope {
		#[serde(default)]
		source: EnvelopeSource,
		#[serde(default = "default_attack")]
		attack_ms: f32,
		#[serde(default = "default_release")]
		release_ms: f32,
		#[serde(default, skip_serializing_if = "is_zero")]
		gain_db: f32,
	},
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LfoShape {
	#[default]
	Sine,
	Triangle,
	/// Rising ramp.
	Saw,
	Square,
	/// A new random value each cycle (sample and hold).
	Random,
}

impl LfoShape {
	pub const ALL: [LfoShape; 5] = [
		LfoShape::Sine,
		LfoShape::Triangle,
		LfoShape::Saw,
		LfoShape::Square,
		LfoShape::Random,
	];

	pub fn name(self) -> &'static str {
		match self {
			LfoShape::Sine => "sine",
			LfoShape::Triangle => "triangle",
			LfoShape::Saw => "saw",
			LfoShape::Square => "square",
			LfoShape::Random => "random",
		}
	}
}

/// When an LFO restarts its cycle.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Retrigger {
	/// Runs on from where playback started.
	#[default]
	Free,
	/// Restarts at every note-on (instruments; effects run free).
	Note,
	/// Locked to the song position (same phase at the same bar every time).
	Transport,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EnvelopeSource {
	/// What the device receives (for an instrument: its own output).
	#[default]
	OwnInput,
	/// The device's sidechain key (needs a sidechain source, see `set_sidechain`).
	Sidechain,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ModTarget {
	/// Parameter id of the device.
	pub param: String,
	/// -1..1 of the parameter's range (0.25 = a quarter of min..max at full modulation).
	pub depth: f32,
}

fn default_rate() -> f32 {
	1.0
}
fn default_attack() -> f32 {
	10.0
}
fn default_release() -> f32 {
	150.0
}
fn is_zero(v: &f32) -> bool {
	*v == 0.0
}

impl Modulator {
	/// A 1 Hz sine LFO with no targets.
	pub fn lfo(id: ModulatorId) -> Self {
		Self {
			id,
			kind: ModulatorKind::Lfo {
				shape: LfoShape::Sine,
				rate_hz: default_rate(),
				sync_beats: None,
				phase: 0.0,
				retrigger: Retrigger::Free,
			},
			targets: Vec::new(),
		}
	}

	/// An envelope follower on the device input with no targets.
	pub fn envelope(id: ModulatorId) -> Self {
		Self {
			id,
			kind: ModulatorKind::Envelope {
				source: EnvelopeSource::OwnInput,
				attack_ms: default_attack(),
				release_ms: default_release(),
				gain_db: 0.0,
			},
			targets: Vec::new(),
		}
	}

	pub fn is_lfo(&self) -> bool {
		matches!(self.kind, ModulatorKind::Lfo { .. })
	}

	/// Compact one-line description ("LFO sine 1/8 -> cutoff +0.30, reso -0.10").
	pub fn describe(&self) -> String {
		let head = match &self.kind {
			ModulatorKind::Lfo {
				shape,
				rate_hz,
				sync_beats,
				phase,
				retrigger,
			} => {
				let rate = match sync_beats {
					Some(b) => sync_name(*b),
					None => format!("{} Hz", round3(*rate_hz)),
				};
				let mut s = format!("LFO {} {rate}", shape.name());
				if *phase != 0.0 {
					s.push_str(&format!(" phase {}", round3(*phase)));
				}
				match retrigger {
					Retrigger::Free => {}
					Retrigger::Note => s.push_str(" retrig note"),
					Retrigger::Transport => s.push_str(" retrig transport"),
				}
				s
			}
			ModulatorKind::Envelope {
				source,
				attack_ms,
				release_ms,
				gain_db,
			} => {
				let src = match source {
					EnvelopeSource::OwnInput => "input",
					EnvelopeSource::Sidechain => "sidechain",
				};
				let mut s = format!(
					"Envelope {src} A {} ms R {} ms",
					round3(*attack_ms),
					round3(*release_ms)
				);
				if *gain_db != 0.0 {
					s.push_str(&format!(" gain {:+} dB", round3(*gain_db)));
				}
				s
			}
		};
		if self.targets.is_empty() {
			return format!("{head} (no targets)");
		}
		let targets: Vec<String> = self
			.targets
			.iter()
			.map(|t| format!("{} {:+.2}", t.param, t.depth))
			.collect();
		format!("{head} -> {}", targets.join(", "))
	}
}

fn range(what: String, value: f32, (lo, hi): (f32, f32), unit: &str) -> Result<(), ModelError> {
	if value.is_finite() && (lo..=hi).contains(&value) {
		Ok(())
	} else {
		Err(ModelError::invalid(
			what,
			value,
			format!("must be {lo}..={hi}{unit}"),
		))
	}
}

/// Structural checks of `device`'s modulators (param existence is checked by callers
/// that know the device, like the MCP tools).
pub(crate) fn validate_modulators(
	device: DeviceId,
	modulators: &[Modulator],
	has_sidechain: bool,
) -> Result<(), ModelError> {
	if modulators.len() > MAX_MODULATORS {
		return Err(ModelError::invalid(
			format!("modulators of device {device}"),
			format!("{} modulators", modulators.len()),
			format!(
				"a device holds at most {MAX_MODULATORS} modulators; remove some or reuse one with several targets"
			),
		));
	}
	for (i, m) in modulators.iter().enumerate() {
		let at = format!("modulator {i} ({}) of device {device}", m.id);
		if modulators[..i].iter().any(|o| o.id == m.id) {
			return Err(ModelError::invalid(
				at,
				m.id,
				"modulator ids must be unique on a device; generate a new id",
			));
		}
		match &m.kind {
			ModulatorKind::Lfo {
				rate_hz,
				sync_beats,
				phase,
				..
			} => {
				range(format!("rate_hz of {at}"), *rate_hz, RATE_HZ_RANGE, " Hz")?;
				if let Some(b) = sync_beats {
					range(
						format!("sync_beats of {at}"),
						*b,
						SYNC_BEATS_RANGE,
						" quarter notes (0.5 = 1/8, 4 = one 4/4 bar)",
					)?;
				}
				range(format!("phase of {at}"), *phase, (0.0, 1.0), " of a cycle")?;
			}
			ModulatorKind::Envelope {
				source,
				attack_ms,
				release_ms,
				gain_db,
			} => {
				range(
					format!("attack_ms of {at}"),
					*attack_ms,
					ATTACK_MS_RANGE,
					" ms",
				)?;
				range(
					format!("release_ms of {at}"),
					*release_ms,
					RELEASE_MS_RANGE,
					" ms",
				)?;
				range(format!("gain_db of {at}"), *gain_db, GAIN_DB_RANGE, " dB")?;
				if *source == EnvelopeSource::Sidechain && !has_sidechain {
					return Err(ModelError::invalid(
						format!("source of {at}"),
						"sidechain",
						"the device has no sidechain source; connect one with set_sidechain first, or use source \"own_input\"",
					));
				}
			}
		}
		if m.targets.len() > MAX_TARGETS {
			return Err(ModelError::invalid(
				format!("targets of {at}"),
				format!("{} targets", m.targets.len()),
				format!("a modulator has at most {MAX_TARGETS} targets"),
			));
		}
		for (k, t) in m.targets.iter().enumerate() {
			if t.param.is_empty() || t.param.contains('/') {
				return Err(ModelError::invalid(
					format!("target {k} of {at}"),
					format!("{:?}", t.param),
					"param ids must be non-empty and must not contain '/'",
				));
			}
			if m.targets[..k].iter().any(|o| o.param == t.param) {
				return Err(ModelError::invalid(
					format!("target {k} of {at}"),
					&t.param,
					"each parameter appears once per modulator; change the existing target's depth instead",
				));
			}
			range(
				format!("depth of target '{}' of {at}", t.param),
				t.depth,
				(-1.0, 1.0),
				" (fraction of the parameter's range)",
			)?;
		}
	}
	Ok(())
}

/// `v` rounded to 3 decimals, printed without float noise.
fn round3(v: f32) -> f64 {
	(f64::from(v) * 1000.0).round() / 1000.0
}
