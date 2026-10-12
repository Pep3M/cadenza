//! The undoable command API shared by the UI and the AI.

use std::collections::BTreeMap;
use std::path::PathBuf;

use cadenza_core::{ClipId, DeviceId, NoteId, ParamAddress, Tick, TimeSignature, TrackId};
use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};

use crate::project::{
	AutomationPoint, Clip, ClipFade, Device, Marker, Note, Output, OutputRoute, Send,
	SidechainSource, Take, Track, TrackInput,
};
use crate::sample::SampleZone;

/// Every mutation of a [`crate::Project`]. Commands carry every id they create, so
/// executing the same command twice (e.g. redo) is deterministic.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Command {
	/// Adds a fully described track. `index` is the display position (default: last).
	AddTrack {
		track: Track,
		#[serde(default, skip_serializing_if = "Option::is_none")]
		index: Option<usize>,
	},
	/// Removes a track together with the automation lanes addressing it and every send
	/// targeting it; tracks that output to it are re-routed to the master.
	RemoveTrack {
		track: TrackId,
	},
	/// Moves a track to display position `index` (0 = top); routing is unaffected.
	MoveTrack {
		track: TrackId,
		index: usize,
	},
	UpdateTrack {
		track: TrackId,
		#[serde(flatten)]
		patch: TrackPatch,
	},
	/// `track: null` edits the master channel.
	SetChannel {
		#[serde(default)]
		track: Option<TrackId>,
		#[serde(flatten)]
		patch: ChannelPatch,
	},
	/// Sets (or with `device: null` removes) the instrument of an Instrument track.
	SetInstrument {
		track: TrackId,
		#[serde(default)]
		device: Option<Device>,
	},
	/// Inserts an effect into a channel chain (`track: null` = master) at `index` (default: last).
	InsertDevice {
		#[serde(default)]
		track: Option<TrackId>,
		device: Device,
		#[serde(default, skip_serializing_if = "Option::is_none")]
		index: Option<usize>,
	},
	/// Removes an insert effect and the automation lanes addressing it.
	RemoveDevice {
		#[serde(default)]
		track: Option<TrackId>,
		device: DeviceId,
	},
	/// Moves an insert to `index` within the same chain.
	MoveDevice {
		#[serde(default)]
		track: Option<TrackId>,
		device: DeviceId,
		index: usize,
	},
	SetDeviceBypass {
		#[serde(default)]
		track: Option<TrackId>,
		device: DeviceId,
		bypass: bool,
	},
	/// Replaces a device's opaque plugin state (`null` clears it) and, when `params` is
	/// given, its whole explicit parameter map (e.g. after loading a preset).
	SetDeviceState {
		#[serde(default)]
		track: Option<TrackId>,
		device: DeviceId,
		#[serde(default)]
		state: Option<String>,
		#[serde(default, skip_serializing_if = "Option::is_none")]
		params: Option<BTreeMap<String, f32>>,
	},
	/// Replaces the sample zones of a sample-based built-in device (`sampler`,
	/// `drum_sampler`); an empty list unloads every sample.
	SetSampleZones {
		#[serde(default)]
		track: Option<TrackId>,
		device: DeviceId,
		zones: Vec<SampleZone>,
	},
	/// Feeds a device's sidechain input from another track (`source: null` disconnects it).
	SetSidechain {
		#[serde(default)]
		track: Option<TrackId>,
		device: DeviceId,
		#[serde(default)]
		source: Option<SidechainSource>,
	},
	/// Replaces which extra outputs of a track's (multi-output) instrument are used and
	/// where they go; an empty list means main output only.
	SetOutputRoutes {
		track: TrackId,
		routes: Vec<OutputRoute>,
	},
	/// Volume (dB), pan (-1..1), a send level (dB) or a device parameter (plain value).
	SetParam {
		address: ParamAddress,
		value: f32,
	},
	/// Clears an explicit device parameter value so the device default applies again.
	ResetParam {
		address: ParamAddress,
	},
	AddSend {
		track: TrackId,
		send: Send,
		#[serde(default, skip_serializing_if = "Option::is_none")]
		index: Option<usize>,
	},
	RemoveSend {
		track: TrackId,
		send: DeviceId,
	},
	UpdateSend {
		track: TrackId,
		send: DeviceId,
		#[serde(flatten)]
		patch: SendPatch,
	},
	AddClip {
		track: TrackId,
		clip: Clip,
	},
	RemoveClip {
		clip: ClipId,
	},
	UpdateClip {
		clip: ClipId,
		#[serde(flatten)]
		patch: ClipPatch,
	},
	/// Replaces both fades of an audio clip (`null` = no fade). Each fade, and their sum,
	/// must fit in the clip's duration at its current position and tempo.
	SetClipFades {
		clip: ClipId,
		#[serde(default)]
		fade_in: Option<ClipFade>,
		#[serde(default)]
		fade_out: Option<ClipFade>,
	},
	/// Changes the source file, the start offset inside it (seconds) and/or the gain of an
	/// audio clip (e.g. after rendering a processed copy of the file, or trimming its
	/// start). Fields left out are unchanged.
	UpdateAudioClip {
		clip: ClipId,
		#[serde(flatten)]
		patch: AudioClipPatch,
	},
	AddNotes {
		clip: ClipId,
		notes: Vec<Note>,
	},
	RemoveNotes {
		clip: ClipId,
		notes: Vec<NoteId>,
	},
	UpdateNotes {
		clip: ClipId,
		notes: Vec<NotePatch>,
	},
	/// Sets the tempo from `tick` on (adds or replaces a tempo point).
	SetTempo {
		tick: Tick,
		bpm: f64,
	},
	RemoveTempoPoint {
		tick: Tick,
	},
	SetTimeSignature {
		time_signature: TimeSignature,
	},
	/// Replaces the automation lane for `address`; empty `points` removes the lane.
	SetAutomation {
		address: ParamAddress,
		points: Vec<AutomationPoint>,
	},
	AddMarker {
		marker: Marker,
	},
	RemoveMarker {
		tick: Tick,
	},
	/// Applies all commands atomically: if one fails, nothing changes. Undone as one step.
	Batch {
		label: String,
		commands: Vec<Command>,
	},
}

/// Fields left out (or `None`) are unchanged.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TrackPatch {
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub name: Option<String>,
	/// `"#RRGGBB"` sets the color, `null` clears it, absent leaves it unchanged.
	#[serde(
		default,
		deserialize_with = "double_option",
		skip_serializing_if = "Option::is_none"
	)]
	pub color: Option<Option<String>>,
	/// Audio tracks: the audio input (`null` removes it), absent leaves it unchanged.
	#[serde(
		default,
		deserialize_with = "double_option",
		skip_serializing_if = "Option::is_none"
	)]
	pub input: Option<Option<TrackInput>>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ChannelPatch {
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub volume_db: Option<f32>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub pan: Option<f32>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub mute: Option<bool>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub solo: Option<bool>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub output: Option<Output>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SendPatch {
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub level_db: Option<f32>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub pre_fader: Option<bool>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ClipPatch {
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub name: Option<String>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub start: Option<Tick>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub length: Option<Tick>,
	/// Moves the clip to another track of a compatible kind.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub track: Option<TrackId>,
	/// Take membership (`null` makes it an ordinary clip), absent leaves it unchanged.
	#[serde(
		default,
		deserialize_with = "double_option",
		skip_serializing_if = "Option::is_none"
	)]
	pub take: Option<Option<Take>>,
}

/// Fields left out (or `None`) are unchanged.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AudioClipPatch {
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub file: Option<PathBuf>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub offset_seconds: Option<f64>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub gain_db: Option<f32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct NotePatch {
	pub id: NoteId,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub pitch: Option<u8>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub velocity: Option<f32>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub start: Option<Tick>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub length: Option<Tick>,
}

/// Distinguishes an explicit `null` (`Some(None)`) from an absent field (`None`).
fn double_option<'de, T, D>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
	T: Deserialize<'de>,
	D: Deserializer<'de>,
{
	Option::<T>::deserialize(deserializer).map(Some)
}

impl Command {
	/// Snake-case operation name, as it appears in the `op` JSON field.
	pub fn op(&self) -> &'static str {
		match self {
			Self::AddTrack { .. } => "add_track",
			Self::RemoveTrack { .. } => "remove_track",
			Self::MoveTrack { .. } => "move_track",
			Self::UpdateTrack { .. } => "update_track",
			Self::SetChannel { .. } => "set_channel",
			Self::SetInstrument { .. } => "set_instrument",
			Self::InsertDevice { .. } => "insert_device",
			Self::RemoveDevice { .. } => "remove_device",
			Self::MoveDevice { .. } => "move_device",
			Self::SetDeviceBypass { .. } => "set_device_bypass",
			Self::SetDeviceState { .. } => "set_device_state",
			Self::SetSampleZones { .. } => "set_sample_zones",
			Self::SetSidechain { .. } => "set_sidechain",
			Self::SetOutputRoutes { .. } => "set_output_routes",
			Self::SetParam { .. } => "set_param",
			Self::ResetParam { .. } => "reset_param",
			Self::AddSend { .. } => "add_send",
			Self::RemoveSend { .. } => "remove_send",
			Self::UpdateSend { .. } => "update_send",
			Self::AddClip { .. } => "add_clip",
			Self::RemoveClip { .. } => "remove_clip",
			Self::UpdateClip { .. } => "update_clip",
			Self::SetClipFades { .. } => "set_clip_fades",
			Self::UpdateAudioClip { .. } => "update_audio_clip",
			Self::AddNotes { .. } => "add_notes",
			Self::RemoveNotes { .. } => "remove_notes",
			Self::UpdateNotes { .. } => "update_notes",
			Self::SetTempo { .. } => "set_tempo",
			Self::RemoveTempoPoint { .. } => "remove_tempo_point",
			Self::SetTimeSignature { .. } => "set_time_signature",
			Self::SetAutomation { .. } => "set_automation",
			Self::AddMarker { .. } => "add_marker",
			Self::RemoveMarker { .. } => "remove_marker",
			Self::Batch { .. } => "batch",
		}
	}

	/// Human-readable description for the undo history.
	pub fn label(&self) -> String {
		match self {
			Self::AddTrack { track, .. } => format!("Add track '{}'", track.name),
			Self::RemoveTrack { .. } => "Remove track".into(),
			Self::MoveTrack { .. } => "Move track".into(),
			Self::UpdateTrack { patch, .. } => match (&patch.name, &patch.color, &patch.input) {
				(Some(name), _, _) => format!("Rename track to '{name}'"),
				(None, None, Some(Some(input))) => format!("Set track input to {}", input.label()),
				(None, None, Some(None)) => "Remove track input".into(),
				_ => "Change track color".into(),
			},
			Self::SetChannel { track, patch } => {
				let what = match (
					patch.volume_db,
					patch.pan,
					patch.mute,
					patch.solo,
					patch.output,
				) {
					(Some(db), None, None, None, None) => format!("volume to {db:.1} dB"),
					(None, Some(pan), None, None, None) => format!("pan to {pan:.2}"),
					(None, None, Some(true), None, None) => "mute".into(),
					(None, None, Some(false), None, None) => "unmute".into(),
					(None, None, None, Some(true), None) => "solo".into(),
					(None, None, None, Some(false), None) => "unsolo".into(),
					(None, None, None, None, Some(_)) => "output".into(),
					_ => "channel".into(),
				};
				format!("Set {} {what}", channel_name(*track))
			}
			Self::SetInstrument {
				device: Some(d), ..
			} => format!("Set instrument '{}'", d.name),
			Self::SetInstrument { device: None, .. } => "Remove instrument".into(),
			Self::InsertDevice { track, device, .. } => {
				format!("Insert '{}' on {}", device.name, channel_name(*track))
			}
			Self::RemoveDevice { track, .. } => {
				format!("Remove device from {}", channel_name(*track))
			}
			Self::MoveDevice { .. } => "Move device".into(),
			Self::SetDeviceBypass { bypass: true, .. } => "Bypass device".into(),
			Self::SetDeviceBypass { bypass: false, .. } => "Enable device".into(),
			Self::SetDeviceState {
				params: Some(_), ..
			} => "Load device preset".into(),
			Self::SetDeviceState { .. } => "Store plugin state".into(),
			Self::SetSampleZones { zones, .. } if zones.is_empty() => "Unload samples".into(),
			Self::SetSampleZones { .. } => "Edit sample zones".into(),
			Self::SetSidechain {
				source: Some(_), ..
			} => "Set sidechain".into(),
			Self::SetSidechain { source: None, .. } => "Remove sidechain".into(),
			Self::SetOutputRoutes { routes, .. } if routes.is_empty() => {
				"Use main instrument output only".into()
			}
			Self::SetOutputRoutes { .. } => "Route instrument outputs".into(),
			Self::SetParam { address, value } => match &address.target {
				cadenza_core::ParamTarget::Volume => format!("Set volume to {value:.1} dB"),
				cadenza_core::ParamTarget::Pan => format!("Set pan to {value:.2}"),
				cadenza_core::ParamTarget::Device { param, .. } => {
					format!("Set '{param}' to {value}")
				}
				cadenza_core::ParamTarget::Send { .. } => format!("Set send to {value:.1} dB"),
			},
			Self::ResetParam { address } => match &address.target {
				cadenza_core::ParamTarget::Device { param, .. } => format!("Reset '{param}'"),
				_ => "Reset parameter".into(),
			},
			Self::AddSend { send, .. } => format!("Add send at {:.1} dB", send.level_db),
			Self::RemoveSend { .. } => "Remove send".into(),
			Self::UpdateSend { .. } => "Change send".into(),
			Self::AddClip { clip, .. } => format!("Add clip '{}'", clip.name),
			Self::RemoveClip { .. } => "Remove clip".into(),
			Self::UpdateClip { patch, .. } if patch.take.is_some() => "Edit take".into(),
			Self::UpdateClip { patch, .. } => {
				match (&patch.name, patch.start, patch.length, patch.track) {
					(Some(name), None, None, None) => format!("Rename clip to '{name}'"),
					(None, _, None, _) => "Move clip".into(),
					(None, None, Some(_), None) => "Resize clip".into(),
					_ => "Edit clip".into(),
				}
			}
			Self::SetClipFades {
				fade_in: None,
				fade_out: None,
				..
			} => "Remove clip fades".into(),
			Self::SetClipFades { .. } => "Set clip fades".into(),
			Self::UpdateAudioClip { patch, .. } => {
				match (&patch.file, patch.offset_seconds, patch.gain_db) {
					(Some(_), _, _) => "Replace clip audio".into(),
					(None, None, Some(db)) => format!("Set clip gain to {db:.1} dB"),
					_ => "Edit audio clip".into(),
				}
			}
			Self::AddNotes { notes, .. } => plural("Add", notes.len(), "note"),
			Self::RemoveNotes { notes, .. } => plural("Remove", notes.len(), "note"),
			Self::UpdateNotes { notes, .. } => plural("Edit", notes.len(), "note"),
			Self::SetTempo { bpm, tick } if *tick == Tick::ZERO => {
				format!("Set tempo to {bpm} bpm")
			}
			Self::SetTempo { bpm, tick } => {
				format!("Set tempo to {bpm} bpm at beat {}", tick.beats())
			}
			Self::RemoveTempoPoint { .. } => "Remove tempo change".into(),
			Self::SetTimeSignature { time_signature: ts } => {
				format!("Set time signature to {}/{}", ts.numerator, ts.denominator)
			}
			Self::SetAutomation { points, .. } if points.is_empty() => "Clear automation".into(),
			Self::SetAutomation { .. } => "Write automation".into(),
			Self::AddMarker { marker } => format!("Add marker '{}'", marker.name),
			Self::RemoveMarker { .. } => "Remove marker".into(),
			Self::Batch { label, .. } => label.clone(),
		}
	}
}

fn channel_name(track: Option<TrackId>) -> &'static str {
	if track.is_some() { "track" } else { "master" }
}

fn plural(verb: &str, n: usize, noun: &str) -> String {
	if n == 1 {
		format!("{verb} 1 {noun}")
	} else {
		format!("{verb} {n} {noun}s")
	}
}
