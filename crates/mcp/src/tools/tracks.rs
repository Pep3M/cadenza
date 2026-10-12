//! Tracks, the mixer (channels, sends) and devices inserted on a channel.

use std::collections::BTreeMap;

use cadenza_core::{DeviceId, ParamAddress, TrackId};
use cadenza_dsp::DeviceRole;
use cadenza_model::{ChannelPatch, Command, Output, Send as ModelSend, TrackKind, TrackPatch};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::convert::{
	DeviceSpec, build_device, device_ref_for, is_plugin_device, needs_catalog, resolve_param,
	resolve_param_key,
};
use crate::error::McpError;
use crate::session::Session;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CreateTrackArgs {
	pub name: String,
	pub kind: TrackKind,
	/// Instrument to load on an Instrument track (invalid on other kinds): a built-in id or
	/// a plugin id such as `"clap:<plugin id>"` from `list_devices`.
	#[serde(default)]
	pub instrument: Option<DeviceSpec>,
	/// `"#RRGGBB"`.
	#[serde(default)]
	pub color: Option<String>,
	/// Bus track to route this track's output to; omit to route to the master bus.
	#[serde(default)]
	pub output: Option<TrackId>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct CreateTrackOutput {
	pub track_id: TrackId,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub instrument_device_id: Option<DeviceId>,
}

/// Builds a device, loading the plugin catalog only when the id is not a built-in.
fn build_spec(
	session: &mut Session,
	spec: &DeviceSpec,
	role: DeviceRole,
) -> Result<cadenza_model::Device, McpError> {
	let catalog = needs_catalog(&spec.device).then(|| session.plugins());
	build_device(spec, role, catalog.as_deref())
}

/// Resolves a param key of an existing device to its stored id, validating `values`
/// against the device's range (built-ins, and plugins present in the catalog).
fn resolve_existing_param(
	session: &mut Session,
	track: Option<TrackId>,
	device: DeviceId,
	key: &str,
	values: &[f32],
) -> Result<String, McpError> {
	let project = session.document().project();
	let catalog = is_plugin_device(project, track, device).then(|| session.plugins());
	let device = device_ref_for(
		session.document().project(),
		track,
		device,
		catalog.as_deref(),
	);
	let id = resolve_param_key(device, key)?;
	for &value in values {
		resolve_param(device, key, value)?;
	}
	Ok(id)
}

pub fn create_track(
	session: &mut Session,
	args: CreateTrackArgs,
) -> Result<CreateTrackOutput, McpError> {
	let mut track = cadenza_model::Track::new(args.name, args.kind);
	if let Some(color) = args.color {
		track = track.with_color(color);
	}
	let instrument_device_id = if let Some(spec) = args.instrument {
		let device = build_spec(session, &spec, DeviceRole::Instrument)?;
		let id = device.id;
		track = track.with_instrument(device);
		Some(id)
	} else {
		None
	};
	if let Some(bus) = args.output {
		track.channel.output = Output::Bus(bus);
	}
	let track_id = track.id;
	session
		.document_mut()
		.execute(Command::AddTrack { track, index: None })?;
	Ok(CreateTrackOutput {
		track_id,
		instrument_device_id,
	})
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct UpdateTrackArgs {
	pub track_id: TrackId,
	#[serde(flatten)]
	pub patch: TrackPatch,
}

pub fn update_track(session: &mut Session, args: UpdateTrackArgs) -> Result<(), McpError> {
	session.document_mut().execute(Command::UpdateTrack {
		track: args.track_id,
		patch: args.patch,
	})?;
	Ok(())
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DeleteTrackArgs {
	pub track_id: TrackId,
}

pub fn delete_track(session: &mut Session, args: DeleteTrackArgs) -> Result<(), McpError> {
	session.document_mut().execute(Command::RemoveTrack {
		track: args.track_id,
	})?;
	Ok(())
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SetChannelArgs {
	/// Track to change; omit to change the master channel.
	#[serde(default)]
	pub track_id: Option<TrackId>,
	#[serde(flatten)]
	pub patch: ChannelPatch,
}

pub fn set_channel(session: &mut Session, args: SetChannelArgs) -> Result<(), McpError> {
	session.document_mut().execute(Command::SetChannel {
		track: args.track_id,
		patch: args.patch,
	})?;
	Ok(())
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AddSendArgs {
	pub track_id: TrackId,
	/// Must be an existing Bus track.
	pub bus_track_id: TrackId,
	pub level_db: f32,
	#[serde(default)]
	pub pre_fader: bool,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct AddSendOutput {
	pub send_id: DeviceId,
}

pub fn add_send(session: &mut Session, args: AddSendArgs) -> Result<AddSendOutput, McpError> {
	let mut send = ModelSend::new(args.bus_track_id, args.level_db);
	send.pre_fader = args.pre_fader;
	let send_id = send.id;
	session.document_mut().execute(Command::AddSend {
		track: args.track_id,
		send,
		index: None,
	})?;
	Ok(AddSendOutput { send_id })
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RemoveSendArgs {
	pub track_id: TrackId,
	pub send_id: DeviceId,
}

pub fn remove_send(session: &mut Session, args: RemoveSendArgs) -> Result<(), McpError> {
	session.document_mut().execute(Command::RemoveSend {
		track: args.track_id,
		send: args.send_id,
	})?;
	Ok(())
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct InsertDeviceArgs {
	/// Channel to insert on; omit to insert on the master bus.
	#[serde(default)]
	pub track_id: Option<TrackId>,
	#[serde(flatten)]
	pub device: DeviceSpec,
	/// Position in the effect chain; omit to append at the end.
	#[serde(default)]
	pub index: Option<usize>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct InsertDeviceOutput {
	pub device_id: DeviceId,
}

pub fn insert_device(
	session: &mut Session,
	args: InsertDeviceArgs,
) -> Result<InsertDeviceOutput, McpError> {
	let device = build_spec(session, &args.device, DeviceRole::Effect)?;
	let device_id = device.id;
	session.document_mut().execute(Command::InsertDevice {
		track: args.track_id,
		device,
		index: args.index,
	})?;
	Ok(InsertDeviceOutput { device_id })
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RemoveDeviceArgs {
	#[serde(default)]
	pub track_id: Option<TrackId>,
	pub device_id: DeviceId,
}

pub fn remove_device(session: &mut Session, args: RemoveDeviceArgs) -> Result<(), McpError> {
	session.document_mut().execute(Command::RemoveDevice {
		track: args.track_id,
		device: args.device_id,
	})?;
	Ok(())
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SetDeviceParamsArgs {
	#[serde(default)]
	pub track_id: Option<TrackId>,
	pub device_id: DeviceId,
	/// Plain (unnormalized) parameter values by param id; for plugins also by exact
	/// (case-insensitive) param name or `"module/name"`.
	pub params: BTreeMap<String, f32>,
}

pub fn set_device_params(session: &mut Session, args: SetDeviceParamsArgs) -> Result<(), McpError> {
	if args.params.is_empty() {
		return Err(McpError::Invalid("params must not be empty".into()));
	}
	let mut commands = Vec::with_capacity(args.params.len());
	for (param, value) in &args.params {
		let id = resolve_existing_param(session, args.track_id, args.device_id, param, &[*value])?;
		commands.push(Command::SetParam {
			address: ParamAddress::device(args.track_id, args.device_id, id),
			value: *value,
		});
	}
	session.document_mut().execute(Command::Batch {
		label: "Set device parameters".into(),
		commands,
	})?;
	Ok(())
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SetBypassArgs {
	#[serde(default)]
	pub track_id: Option<TrackId>,
	pub device_id: DeviceId,
	pub bypass: bool,
}

pub fn set_bypass(session: &mut Session, args: SetBypassArgs) -> Result<(), McpError> {
	session.document_mut().execute(Command::SetDeviceBypass {
		track: args.track_id,
		device: args.device_id,
		bypass: args.bypass,
	})?;
	Ok(())
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SetParamArgs {
	/// `track/{id}/volume` | `track/{id}/pan` | `track/{id}/device/{id}/{param}` |
	/// `master/volume` | `master/device/{id}/{param}`. For plugin devices `{param}` may be
	/// the param id or its exact name.
	pub address: ParamAddress,
	pub value: f32,
}

/// Rewrites a device param address so it names the param by its stored id.
fn resolve_address(
	session: &mut Session,
	address: ParamAddress,
	values: &[f32],
) -> Result<ParamAddress, McpError> {
	match &address.target {
		cadenza_core::ParamTarget::Device { device, param } => {
			let id = resolve_existing_param(session, address.track, *device, param, values)?;
			Ok(ParamAddress::device(address.track, *device, id))
		}
		_ => Ok(address),
	}
}

pub fn set_param(session: &mut Session, args: SetParamArgs) -> Result<(), McpError> {
	let address = resolve_address(session, args.address, &[args.value])?;
	session.document_mut().execute(Command::SetParam {
		address,
		value: args.value,
	})?;
	Ok(())
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AutomationPointArg {
	pub beat: f64,
	pub value: f32,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SetAutomationArgs {
	pub address: ParamAddress,
	/// Empty removes the automation lane.
	pub points: Vec<AutomationPointArg>,
}

pub fn set_automation(session: &mut Session, args: SetAutomationArgs) -> Result<(), McpError> {
	let values: Vec<f32> = args.points.iter().map(|p| p.value).collect();
	let address = resolve_address(session, args.address, &values)?;
	let points = args
		.points
		.into_iter()
		.map(|p| {
			cadenza_model::AutomationPoint::new(crate::convert::beats_to_tick(p.beat), p.value)
		})
		.collect();
	session
		.document_mut()
		.execute(Command::SetAutomation { address, points })?;
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::convert::DeviceSpec;

	fn synth_track(session: &mut Session) -> TrackId {
		create_track(
			session,
			CreateTrackArgs {
				name: "Synth".into(),
				kind: TrackKind::Instrument,
				instrument: Some(DeviceSpec {
					device: "poly_synth".into(),
					params: BTreeMap::new(),
				}),
				color: None,
				output: None,
			},
		)
		.unwrap()
		.track_id
	}

	#[test]
	fn create_track_with_instrument_round_trips_through_undo() {
		let mut session = Session::new();
		let track_id = synth_track(&mut session);
		assert_eq!(session.document().project().tracks.len(), 1);
		assert!(session.document().project().track(track_id).is_some());

		session.document_mut().undo().unwrap();
		assert!(session.document().project().tracks.is_empty());
		session.document_mut().redo().unwrap();
		assert_eq!(session.document().project().tracks.len(), 1);
	}

	#[test]
	fn create_track_rejects_unknown_device() {
		let mut session = Session::new();
		let err = create_track(
			&mut session,
			CreateTrackArgs {
				name: "Synth".into(),
				kind: TrackKind::Instrument,
				instrument: Some(DeviceSpec {
					device: "nope".into(),
					params: BTreeMap::new(),
				}),
				color: None,
				output: None,
			},
		)
		.unwrap_err();
		assert!(matches!(err, McpError::UnknownDevice { .. }), "{err}");
	}

	#[test]
	fn create_track_rejects_effect_as_instrument() {
		let mut session = Session::new();
		let err = create_track(
			&mut session,
			CreateTrackArgs {
				name: "Synth".into(),
				kind: TrackKind::Instrument,
				instrument: Some(DeviceSpec {
					device: "reverb".into(),
					params: BTreeMap::new(),
				}),
				color: None,
				output: None,
			},
		)
		.unwrap_err();
		assert!(matches!(err, McpError::WrongDeviceRole { .. }), "{err}");
	}

	#[test]
	fn insert_device_validates_param_range() {
		let mut session = Session::new();
		let track_id = synth_track(&mut session);
		let mut params = BTreeMap::new();
		params.insert("mix".to_owned(), 999.0);
		let err = insert_device(
			&mut session,
			InsertDeviceArgs {
				track_id: Some(track_id),
				device: DeviceSpec {
					device: "reverb".into(),
					params,
				},
				index: None,
			},
		)
		.unwrap_err();
		assert!(matches!(err, McpError::ParamOutOfRange { .. }), "{err}");
	}

	#[test]
	fn insert_device_rejects_unknown_param() {
		let mut session = Session::new();
		let track_id = synth_track(&mut session);
		let mut params = BTreeMap::new();
		params.insert("not_a_param".to_owned(), 0.5);
		let err = insert_device(
			&mut session,
			InsertDeviceArgs {
				track_id: Some(track_id),
				device: DeviceSpec {
					device: "reverb".into(),
					params,
				},
				index: None,
			},
		)
		.unwrap_err();
		assert!(matches!(err, McpError::UnknownParam { .. }), "{err}");
	}

	#[test]
	fn set_device_params_validates_range_and_is_one_undo_step() {
		let mut session = Session::new();
		let track_id = synth_track(&mut session);
		let device_id = insert_device(
			&mut session,
			InsertDeviceArgs {
				track_id: Some(track_id),
				device: DeviceSpec {
					device: "reverb".into(),
					params: BTreeMap::new(),
				},
				index: None,
			},
		)
		.unwrap()
		.device_id;

		let mut params = BTreeMap::new();
		params.insert("mix".to_owned(), 0.6);
		params.insert("size".to_owned(), 0.8);
		set_device_params(
			&mut session,
			SetDeviceParamsArgs {
				track_id: Some(track_id),
				device_id,
				params,
			},
		)
		.unwrap();

		let steps_before = session.document().history_labels().len();
		session.document_mut().undo().unwrap();
		assert_eq!(session.document().history_labels().len(), steps_before - 1);

		let mut bad = BTreeMap::new();
		bad.insert("mix".to_owned(), -5.0);
		let err = set_device_params(
			&mut session,
			SetDeviceParamsArgs {
				track_id: Some(track_id),
				device_id,
				params: bad,
			},
		)
		.unwrap_err();
		assert!(matches!(err, McpError::ParamOutOfRange { .. }), "{err}");
	}

	#[test]
	fn add_send_requires_existing_bus() {
		let mut session = Session::new();
		let track_id = synth_track(&mut session);
		let err = add_send(
			&mut session,
			AddSendArgs {
				track_id,
				bus_track_id: TrackId::new(),
				level_db: -6.0,
				pre_fader: false,
			},
		)
		.unwrap_err();
		assert!(matches!(err, McpError::Model(_)), "{err}");
	}
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SetSidechainArgs {
	/// Track holding the keyed device; omit for a master insert.
	#[serde(default)]
	pub track_id: Option<TrackId>,
	/// The device whose sidechain (key) input is fed, e.g. a compressor on the bass.
	pub device_id: DeviceId,
	/// Track whose signal drives it (e.g. the kick); null or omitted disconnects the
	/// sidechain.
	#[serde(default)]
	pub source_track_id: Option<TrackId>,
	/// Tap the source before its fader/pan/mute (a muted "ghost" kick still keys).
	/// Default false: the source as it is heard.
	#[serde(default)]
	pub pre_fader: bool,
}

/// Whether the device can take a sidechain: built-ins that implement it, plugins whose
/// scan found a sidechain input. `None` when unknown (plugin not in the catalog).
fn has_sidechain(session: &mut Session, device: &cadenza_model::Device) -> Option<bool> {
	match &device.kind {
		cadenza_model::DeviceKind::Builtin { id } => {
			Some(cadenza_dsp::create(id).is_some_and(|p| p.has_sidechain()))
		}
		_ => {
			let catalog = session.plugins();
			crate::convert::plugin_ref_of(device, &catalog).map(|d| d.sidechain_channels > 0)
		}
	}
}

pub fn set_sidechain(session: &mut Session, args: SetSidechainArgs) -> Result<(), McpError> {
	let device = session
		.document()
		.project()
		.device(args.track_id, args.device_id)
		.cloned();
	if let (Some(device), Some(_)) = (&device, args.source_track_id)
		&& has_sidechain(session, device) == Some(false)
	{
		return Err(McpError::Invalid(format!(
			"device '{}' has no sidechain input; key a built-in 'compressor' instead (insert \
			 one with insert_device) or a plugin whose get_device_info mentions a sidechain input",
			device.name
		)));
	}
	session.document_mut().execute(Command::SetSidechain {
		track: args.track_id,
		device: args.device_id,
		source: args
			.source_track_id
			.map(|track| cadenza_model::SidechainSource {
				track,
				pre_fader: args.pre_fader,
			}),
	})?;
	Ok(())
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct OutputRouteArg {
	/// Extra output of the instrument, 1-based (as numbered by get_device_info's
	/// aux_outputs); the main output is always mixed into the track.
	pub port: u32,
	/// Bus track receiving this output; omit to mix it into the instrument's own track.
	#[serde(default)]
	pub bus_track_id: Option<TrackId>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SetOutputRoutesArgs {
	/// Instrument track whose (multi-output) instrument is routed.
	pub track_id: TrackId,
	/// Every extra output to use; outputs not listed are not heard. Empty = main only.
	pub routes: Vec<OutputRouteArg>,
}

pub fn set_output_routes(session: &mut Session, args: SetOutputRoutesArgs) -> Result<(), McpError> {
	let instrument = session
		.document()
		.project()
		.track(args.track_id)
		.and_then(|t| t.instrument.clone());
	if let Some(instrument) = &instrument {
		let outputs = match &instrument.kind {
			cadenza_model::DeviceKind::Builtin { .. } => Some(Vec::new()),
			_ => {
				let catalog = session.plugins();
				crate::convert::plugin_ref_of(instrument, &catalog).map(|d| d.aux_outputs.clone())
			}
		};
		if let Some(outputs) = outputs
			&& let Some(bad) = args.routes.iter().find(|r| r.port as usize > outputs.len())
		{
			let available = if outputs.is_empty() {
				"it has no extra outputs (only multi-output plugins such as drum samplers do)"
					.to_string()
			} else {
				format!(
					"its extra outputs are {}",
					outputs
						.iter()
						.enumerate()
						.map(|(i, o)| format!("{} '{}' ({} ch)", i + 1, o.name, o.channels))
						.collect::<Vec<_>>()
						.join(", ")
				)
			};
			return Err(McpError::Invalid(format!(
				"instrument '{}' has no extra output {}; {available}",
				instrument.name, bad.port
			)));
		}
	}
	session.document_mut().execute(Command::SetOutputRoutes {
		track: args.track_id,
		routes: args
			.routes
			.iter()
			.map(|r| cadenza_model::OutputRoute {
				port: r.port,
				bus: r.bus_track_id,
			})
			.collect(),
	})?;
	Ok(())
}
