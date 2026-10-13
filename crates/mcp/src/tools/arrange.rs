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
	/// 1-based bar the signature starts at; omit (or 1) for the song's first signature.
	/// Later bars add a change (or replace the change at that bar). Notes keep their
	/// beat positions; only bar lines move.
	#[serde(default)]
	pub at_bar: Option<u32>,
}

pub fn set_time_signature(
	session: &mut Session,
	args: SetTimeSignatureArgs,
) -> Result<(), McpError> {
	let bar = bar_index(args.at_bar.unwrap_or(1))?;
	session.document_mut().execute(Command::SetTimeSignature {
		time_signature: args.time_signature,
		bar,
	})?;
	Ok(())
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RemoveTimeSignatureArgs {
	/// 1-based bar of the change to remove (>= 2; bar 1 is changed, never removed).
	pub bar: u32,
}

pub fn remove_time_signature(
	session: &mut Session,
	args: RemoveTimeSignatureArgs,
) -> Result<(), McpError> {
	let bar = bar_index(args.bar)?;
	if bar == 0 {
		return Err(McpError::Invalid(
			"bar 1 holds the song's first time signature; change it with set_time_signature instead of removing it".into(),
		));
	}
	session
		.document_mut()
		.execute(Command::RemoveTimeSignature { bar })?;
	Ok(())
}

/// 1-based bar number -> 0-based bar index.
fn bar_index(bar: u32) -> Result<u32, McpError> {
	bar.checked_sub(1).ok_or_else(|| {
		McpError::Invalid("bars are numbered from 1 (bar 1 = the song start)".into())
	})
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

#[cfg(test)]
mod tests {
	use super::*;

	fn ts(numerator: u32, denominator: u32) -> TimeSignature {
		TimeSignature {
			numerator,
			denominator,
		}
	}

	#[test]
	fn time_signature_changes_by_bar() {
		let mut session = Session::new();
		set_time_signature(
			&mut session,
			SetTimeSignatureArgs {
				time_signature: ts(3, 4),
				at_bar: Some(5),
			},
		)
		.unwrap();
		let s = session.document().project().summary();
		assert_eq!(s.time_signature, "4/4");
		assert_eq!(s.time_signature_changes[0].bar, 5);
		assert_eq!(s.time_signature_changes[0].beat, 16.0);
		assert!(
			set_time_signature(
				&mut session,
				SetTimeSignatureArgs {
					time_signature: ts(3, 4),
					at_bar: Some(0),
				},
			)
			.is_err()
		);
		assert!(remove_time_signature(&mut session, RemoveTimeSignatureArgs { bar: 1 }).is_err());
		let err = remove_time_signature(&mut session, RemoveTimeSignatureArgs { bar: 4 })
			.unwrap_err()
			.to_string();
		assert!(err.contains("bar 4"), "{err}");
		remove_time_signature(&mut session, RemoveTimeSignatureArgs { bar: 5 }).unwrap();
		assert!(
			session
				.document()
				.project()
				.time_signature_changes
				.is_empty()
		);
	}
}
