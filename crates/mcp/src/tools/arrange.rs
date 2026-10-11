//! Song-level arrangement: tempo, time signature and markers.

use cadenza_core::TimeSignature;
use cadenza_model::{Command, Marker};
use schemars::JsonSchema;
use serde::Deserialize;

use crate::convert::beats_to_tick;
use crate::error::McpError;
use crate::session::Session;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SetTempoArgs {
	pub bpm: f64,
	/// Beat the new tempo starts at; defaults to 0 (the whole song).
	#[serde(default)]
	pub at_beat: Option<f64>,
}

pub fn set_tempo(session: &mut Session, args: SetTempoArgs) -> Result<(), McpError> {
	let tick = beats_to_tick(args.at_beat.unwrap_or(0.0));
	session.document_mut().execute(Command::SetTempo {
		tick,
		bpm: args.bpm,
	})?;
	Ok(())
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SetTimeSignatureArgs {
	#[serde(flatten)]
	pub time_signature: TimeSignature,
}

pub fn set_time_signature(
	session: &mut Session,
	args: SetTimeSignatureArgs,
) -> Result<(), McpError> {
	session.document_mut().execute(Command::SetTimeSignature {
		time_signature: args.time_signature,
	})?;
	Ok(())
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AddMarkerArgs {
	pub beat: f64,
	pub name: String,
}

pub fn add_marker(session: &mut Session, args: AddMarkerArgs) -> Result<(), McpError> {
	let marker = Marker::new(beats_to_tick(args.beat), args.name);
	session
		.document_mut()
		.execute(Command::AddMarker { marker })?;
	Ok(())
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RemoveMarkerArgs {
	pub beat: f64,
}

pub fn remove_marker(session: &mut Session, args: RemoveMarkerArgs) -> Result<(), McpError> {
	session.document_mut().execute(Command::RemoveMarker {
		tick: beats_to_tick(args.beat),
	})?;
	Ok(())
}
