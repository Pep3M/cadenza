//! `set_modulators`: LFOs and envelope followers on built-in device parameters.

use cadenza_core::{DeviceId, ModulatorId, TrackId};
use cadenza_model::{Command, ModTarget, Modulator, ModulatorKind, SidechainSource};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::McpError;
use crate::session::Session;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ModulatorArg {
	/// Keep the id of an existing modulator (from a previous call); omitted = the id of
	/// the modulator at the same position, or a new one.
	#[serde(default)]
	pub id: Option<ModulatorId>,
	/// `type: "lfo"` (shape sine|triangle|saw|square|random, rate_hz or sync_beats,
	/// phase 0..1, retrigger free|note|transport) or `type: "envelope"` (source
	/// own_input|sidechain, attack_ms, release_ms, gain_db).
	#[serde(flatten)]
	pub kind: ModulatorKind,
	/// Parameters it moves: `{param, depth}` with depth -1..1 of the param's range.
	#[serde(default)]
	pub targets: Vec<ModTarget>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SetModulatorsArgs {
	/// Track holding the device; omit for a master insert.
	#[serde(default)]
	pub track_id: Option<TrackId>,
	/// A built-in device (instrument or insert).
	pub device_id: DeviceId,
	/// Every modulator of the device (replaces the previous ones); [] removes them.
	pub modulators: Vec<ModulatorArg>,
	/// Connects this track as the device's sidechain key in the same step (for an
	/// envelope with source "sidechain").
	#[serde(default)]
	pub sidechain_track_id: Option<TrackId>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct SetModulatorsOutput {
	/// The device's modulators now, one line each.
	pub modulators: Vec<String>,
}

pub fn set_modulators(
	session: &mut Session,
	args: SetModulatorsArgs,
) -> Result<SetModulatorsOutput, McpError> {
	let device = session
		.document()
		.project()
		.device(args.track_id, args.device_id)
		.cloned()
		.ok_or_else(|| {
			McpError::Invalid(format!(
				"device {} is not on {}; use an instrument or insert id from get_project",
				args.device_id,
				args.track_id
					.map_or_else(|| "the master".to_string(), |t| format!("track {t}"))
			))
		})?;
	let Some(builtin) = device.builtin_id() else {
		return Err(McpError::Invalid(format!(
			"device '{}' is a plugin; only built-in devices can be modulated (automate plugin \
			 parameters with set_automation instead)",
			device.name
		)));
	};
	let info = cadenza_dsp::device_info(builtin).ok_or_else(|| McpError::UnknownDevice {
		id: builtin.to_string(),
		role: "device",
		available: cadenza_dsp::registry()
			.iter()
			.map(|d| d.id)
			.collect::<Vec<_>>()
			.join(", "),
	})?;
	let mut modulators = Vec::with_capacity(args.modulators.len());
	for (i, m) in args.modulators.into_iter().enumerate() {
		for t in &m.targets {
			if info.param_index(&t.param).is_none() {
				return Err(McpError::UnknownParam {
					device: format!("{} (modulator {i})", device.name),
					param: t.param.clone(),
					available: info
						.params
						.iter()
						.map(|p| p.id)
						.collect::<Vec<_>>()
						.join(", "),
				});
			}
		}
		let id =
			m.id.or_else(|| device.modulators.get(i).map(|old| old.id))
				.unwrap_or_default();
		modulators.push(Modulator {
			id,
			kind: m.kind,
			targets: m.targets,
		});
	}
	let set = Command::SetModulators {
		track: args.track_id,
		device: args.device_id,
		modulators,
	};
	let command = match args.sidechain_track_id {
		Some(track) => Command::Batch {
			label: "Edit modulation".into(),
			commands: vec![
				Command::SetSidechain {
					track: args.track_id,
					device: args.device_id,
					source: Some(SidechainSource {
						track,
						pre_fader: false,
					}),
				},
				set,
			],
		},
		None => set,
	};
	session.document_mut().execute(command)?;
	let modulators = session
		.document()
		.project()
		.device(args.track_id, args.device_id)
		.map(|d| d.modulators.iter().map(Modulator::describe).collect())
		.unwrap_or_default();
	Ok(SetModulatorsOutput { modulators })
}

#[cfg(test)]
mod tests {
	use super::*;
	use cadenza_model::{Device, Track, TrackKind};

	#[test]
	fn set_modulators_validates_params_and_reports_compactly() {
		let mut session = Session::new();
		let synth = Device::builtin("poly_synth", "Synth");
		let synth_id = synth.id;
		let kick = Track::new("Kick", TrackKind::Audio);
		let kick_id = kick.id;
		let mut pad = Track::new("Pad", TrackKind::Instrument).with_instrument(synth);
		let eq = Device::builtin("eq", "EQ");
		let eq_id = eq.id;
		pad.channel.inserts.push(eq);
		let pad_id = pad.id;
		for track in [pad, kick] {
			session
				.document_mut()
				.execute(Command::AddTrack { track, index: None })
				.unwrap();
		}
		let args: SetModulatorsArgs = serde_json::from_value(serde_json::json!({
			"track_id": pad_id, "device_id": synth_id,
			"modulators": [{"type": "lfo", "shape": "triangle", "sync_beats": 0.5,
				"targets": [{"param": "filter_cutoff", "depth": 0.3}]}]
		}))
		.unwrap();
		let out = set_modulators(&mut session, args).unwrap();
		assert_eq!(out.modulators, ["LFO triangle 1/8 -> filter_cutoff +0.30"]);
		let json = serde_json::to_string(&out).unwrap();
		assert!(json.len() < 200, "{json}");
		let first = session
			.document()
			.project()
			.device(Some(pad_id), synth_id)
			.unwrap()
			.modulators[0]
			.id;
		// Unknown params list the valid ones.
		let bad: SetModulatorsArgs = serde_json::from_value(serde_json::json!({
			"track_id": pad_id, "device_id": synth_id,
			"modulators": [{"type": "lfo", "targets": [{"param": "filter_cutof", "depth": 0.3}]}]
		}))
		.unwrap();
		let err = set_modulators(&mut session, bad).unwrap_err().to_string();
		assert!(
			err.contains("filter_cutof") && err.contains("filter_resonance"),
			"{err}"
		);
		// Keeps the id by position; an envelope keyed by the kick connects the sidechain
		// in the same undo step.
		let keyed: SetModulatorsArgs = serde_json::from_value(serde_json::json!({
			"track_id": pad_id, "device_id": eq_id, "sidechain_track_id": kick_id,
			"modulators": [{"type": "envelope", "source": "sidechain", "release_ms": 80,
				"targets": [{"param": "peak2_gain", "depth": -0.2}]}]
		}))
		.unwrap();
		let out = set_modulators(&mut session, keyed).unwrap();
		assert!(
			out.modulators[0].starts_with("Envelope sidechain"),
			"{:?}",
			out.modulators
		);
		let project = session.document().project();
		assert_eq!(
			project
				.device(Some(pad_id), eq_id)
				.unwrap()
				.sidechain
				.unwrap()
				.track,
			kick_id
		);
		let summary = serde_json::to_string(&project.summary()).unwrap();
		assert!(summary.contains("LFO triangle 1/8 -> filter_cutoff +0.30"));
		assert_eq!(
			session.document().history_labels().last(),
			Some(&"Edit modulation")
		);
		let again: SetModulatorsArgs = serde_json::from_value(serde_json::json!({
			"track_id": pad_id, "device_id": synth_id,
			"modulators": [{"type": "lfo", "rate_hz": 2.0}]
		}))
		.unwrap();
		set_modulators(&mut session, again).unwrap();
		let project = session.document().project();
		assert_eq!(
			project.device(Some(pad_id), synth_id).unwrap().modulators[0].id,
			first
		);
	}
}
