//! Hardware MIDI controller assignments ("MIDI learn"): a knob, fader or encoder of a
//! controller moves a device parameter, a channel's volume/pan or a send level while the
//! song plays live. Stored in [`crate::Project::midi_map`] (saved with the song) and
//! replaced as a whole by `Command::SetMidiMap`; the engine applies them on the audio
//! thread as the MIDI arrives.

use cadenza_core::{ParamAddress, ParamTarget};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{Controller, ModelError, Project};

/// At most this many assignments per project.
pub const MAX_MIDI_MAPPINGS: usize = 256;

/// One controller -> parameter assignment.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MidiMapping {
	/// The hardware control: `"cc74"`, `"pitch_bend"`, `"channel_pressure"`.
	pub controller: Controller,
	/// MIDI channel 0..=15 it listens to (`None` = any channel).
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub channel: Option<u8>,
	/// What it moves (device parameter, volume, pan or send level).
	pub target: ParamAddress,
	/// Plain target value at the controller's minimum / maximum (`min > max` inverts).
	pub min: f32,
	pub max: f32,
	#[serde(default, skip_serializing_if = "MapMode::is_absolute")]
	pub mode: MapMode,
	#[serde(default, skip_serializing_if = "Takeover::is_jump")]
	pub takeover: Takeover,
}

/// How controller values become target values.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MapMode {
	/// The controller position is the value (knobs, faders).
	#[default]
	Absolute,
	/// Endless encoders sending steps (two's complement: 1..63 up, 127..65 down), each
	/// 1/128 of the range. CCs only.
	Relative,
}

impl MapMode {
	pub fn is_absolute(&self) -> bool {
		*self == MapMode::Absolute
	}
}

/// What happens when an absolute control's position differs from the value.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Takeover {
	/// The value jumps to the control at once.
	#[default]
	Jump,
	/// Nothing moves until the control passes the current value.
	Pickup,
}

impl Takeover {
	pub fn is_jump(&self) -> bool {
		*self == Takeover::Jump
	}
}

impl MidiMapping {
	/// The plain target value for a normalized controller position (0..1).
	pub fn value_at(&self, position: f32) -> f32 {
		self.min + position.clamp(0.0, 1.0) * (self.max - self.min)
	}

	/// Whether `controller` on `channel` drives this mapping.
	pub fn listens_to(&self, controller: Controller, channel: u8) -> bool {
		self.controller == controller && self.channel.is_none_or(|c| c == channel)
	}

	/// Short tag for a mapped control ("CC 74", "CC 1 ch 2", "Bend").
	pub fn tag(&self) -> String {
		let c = match self.controller {
			Controller::Cc(n) => format!("CC {n}"),
			Controller::PitchBend => "Bend".into(),
			Controller::ChannelPressure => "AT".into(),
		};
		match self.channel {
			Some(ch) => format!("{c} ch {}", ch + 1),
			None => c,
		}
	}
}

/// Validates a whole map against `project` (targets exist, values finite, no control
/// used twice, no target mapped twice).
pub(crate) fn validate(project: &Project, map: &[MidiMapping]) -> Result<(), ModelError> {
	if map.len() > MAX_MIDI_MAPPINGS {
		return Err(ModelError::invalid(
			"MIDI map",
			map.len(),
			format!("a project holds at most {MAX_MIDI_MAPPINGS} MIDI assignments"),
		));
	}
	for (i, m) in map.iter().enumerate() {
		shape(m)?;
		crate::validate::address(project, &m.target)?;
		if let ParamTarget::Device { device, .. } = &m.target.target
			&& project
				.device(m.target.track, *device)
				.is_some_and(|d| d.builtin_id().is_none())
		{
			return Err(ModelError::invalid(
				"MIDI assignment",
				m.target.to_string(),
				"only built-in device parameters, volume, pan and send levels can be MIDI-mapped (plugins have their own MIDI learn)",
			));
		}
		crate::validate::param_value(&m.target, m.min)?;
		crate::validate::param_value(&m.target, m.max)?;
		for other in &map[..i] {
			if other.target == m.target {
				return Err(ModelError::invalid(
					"MIDI assignment",
					m.target.to_string(),
					"is mapped twice; keep one controller per target (replace the old entry)",
				));
			}
			let channels_meet =
				other.channel.is_none() || m.channel.is_none() || other.channel == m.channel;
			if other.controller == m.controller && channels_meet {
				return Err(ModelError::invalid(
					"MIDI assignment",
					m.controller.to_string(),
					format!(
						"{} already moves {}; one control drives one target",
						m.tag(),
						other.target
					),
				));
			}
		}
	}
	Ok(())
}

/// Checks of one mapping that need no project.
fn shape(m: &MidiMapping) -> Result<(), ModelError> {
	if let Some(ch) = m.channel
		&& ch > 15
	{
		return Err(ModelError::invalid(
			"MIDI channel",
			ch,
			"channels are 0..=15 (0 = MIDI channel 1); omit it to listen on every channel",
		));
	}
	if !m.min.is_finite() || !m.max.is_finite() {
		return Err(ModelError::invalid(
			"MIDI assignment range",
			format!("{}..{}", m.min, m.max),
			"min and max must be finite numbers",
		));
	}
	if m.mode == MapMode::Relative && !matches!(m.controller, Controller::Cc(_)) {
		return Err(ModelError::invalid(
			"MIDI assignment mode",
			m.controller.to_string(),
			"relative mode is for endless encoders sending CCs; use absolute for pitch bend and pressure",
		));
	}
	Ok(())
}

/// Shape checks used when loading a file (targets are checked by the project load).
pub(crate) fn validate_loaded(project: &Project) -> Result<(), ModelError> {
	validate(project, &project.midi_map)
}
