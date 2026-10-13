//! MIDI controller lanes of MIDI clips: mod wheel, sustain pedal, expression, any CC#,
//! pitch bend and channel pressure, as events or drawn curves.

use cadenza_core::{ClipId, Tick};
use cadenza_model::controller::{ramp, splice};
use cadenza_model::{Command, Controller, ControllerPoint, ModelError};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::convert::beats_to_tick;
use crate::error::McpError;
use crate::session::Session;

fn parse(controller: &str) -> Result<Controller, McpError> {
	controller.parse().map_err(McpError::Invalid)
}

fn lane_points(
	session: &Session,
	clip: ClipId,
	controller: Controller,
) -> Result<(Tick, Vec<ControllerPoint>), McpError> {
	let (_, c) = session
		.document()
		.project()
		.find_clip(clip)
		.ok_or(ModelError::ClipNotFound(clip))?;
	if !c.is_midi() {
		return Err(ModelError::NotAMidiClip(clip).into());
	}
	Ok((
		c.length,
		c.controller(controller)
			.map(|l| l.points.clone())
			.unwrap_or_default(),
	))
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ControllerEventInput {
	/// Beats from the clip start.
	pub beat: f64,
	/// CC and pressure 0..=1 (MIDI 0..127; sustain: >= 0.5 = pedal down), pitch bend
	/// -1..=1 (0 = center, ±1 = ±2 semitones on the built-in instruments).
	pub value: f32,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SetControllerEventsArgs {
	pub clip_id: ClipId,
	/// "cc1"/"mod_wheel", "cc64"/"sustain", "cc11"/"expression", "cc74", any "cc0".."cc127",
	/// "pitch_bend" or "channel_pressure".
	pub controller: String,
	/// Events; each value holds until the next event.
	pub events: Vec<ControllerEventInput>,
	/// true (default): the events replace the whole lane (empty events clear it); false:
	/// they are added (replacing events at the same beats).
	#[serde(default)]
	pub replace: Option<bool>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ControllerLaneOutput {
	pub controller: String,
	pub event_count: usize,
}

pub fn set_controller_events(
	session: &mut Session,
	args: SetControllerEventsArgs,
) -> Result<ControllerLaneOutput, McpError> {
	let controller = parse(&args.controller)?;
	let (_, old) = lane_points(session, args.clip_id, controller)?;
	for e in &args.events {
		if !e.beat.is_finite() || e.beat < 0.0 {
			return Err(McpError::Invalid(format!(
				"event beat {} is invalid; beats count from the clip start (>= 0)",
				e.beat
			)));
		}
	}
	let new: Vec<ControllerPoint> = args
		.events
		.iter()
		.map(|e| ControllerPoint::new(beats_to_tick(e.beat), e.value))
		.collect();
	let points = if args.replace.unwrap_or(true) {
		new
	} else {
		let mut merged: Vec<ControllerPoint> = old
			.into_iter()
			.filter(|p| !new.iter().any(|n| n.tick == p.tick))
			.chain(new.iter().copied())
			.collect();
		merged.sort_by_key(|p| p.tick);
		merged
	};
	let event_count = points.len();
	session.document_mut().execute(Command::SetControllerLane {
		clip: args.clip_id,
		controller,
		points,
	})?;
	Ok(ControllerLaneOutput {
		controller: controller.to_string(),
		event_count,
	})
}

#[derive(Debug, Clone, Copy, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CurveShape {
	#[default]
	Linear,
	/// Slow start, fast end (swells, filter sweeps).
	Exponential,
	/// Fast start, slow end.
	Logarithmic,
	/// A sine wave between `from` and `to` (vibrato-like or tremolo-like wobble);
	/// `cycles` sets how many periods fit the range.
	Sine,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DrawControllerCurveArgs {
	pub clip_id: ClipId,
	/// Same names as set_controller_events ("cc1", "sustain", "pitch_bend", ...).
	pub controller: String,
	/// Range in beats from the clip start.
	pub start_beat: f64,
	pub end_beat: f64,
	/// Value at the start and at the end (see set_controller_events for ranges).
	pub from: f32,
	pub to: f32,
	#[serde(default)]
	pub shape: Option<CurveShape>,
	/// Sine shape: periods across the range (default 1).
	#[serde(default)]
	pub cycles: Option<f64>,
	/// Distance between events in beats (default 1/16 beat, min 1/64).
	#[serde(default)]
	pub resolution_beats: Option<f64>,
}

/// Draws a curve into a lane, replacing the events inside its range (the rest stay).
pub fn draw_controller_curve(
	session: &mut Session,
	args: DrawControllerCurveArgs,
) -> Result<ControllerLaneOutput, McpError> {
	let controller = parse(&args.controller)?;
	if !(args.start_beat.is_finite() && args.end_beat.is_finite())
		|| args.start_beat < 0.0
		|| args.end_beat <= args.start_beat
	{
		return Err(McpError::Invalid(format!(
			"curve range {}..{} beats is invalid; use 0 <= start_beat < end_beat (beats from the clip start)",
			args.start_beat, args.end_beat
		)));
	}
	let (lo, hi) = controller.range();
	for v in [args.from, args.to] {
		if !(lo..=hi).contains(&v) {
			return Err(McpError::Invalid(format!(
				"value {v} is out of range for {controller}; use {lo}..={hi}"
			)));
		}
	}
	let (_, old) = lane_points(session, args.clip_id, controller)?;
	let (from, to) = (beats_to_tick(args.start_beat), beats_to_tick(args.end_beat));
	let step = beats_to_tick(args.resolution_beats.unwrap_or(0.0625).max(1.0 / 64.0));
	let new = match args.shape.unwrap_or_default() {
		CurveShape::Linear => ramp(controller, from, args.from, to, args.to, step, 0.0),
		CurveShape::Exponential => ramp(controller, from, args.from, to, args.to, step, 1.0),
		CurveShape::Logarithmic => ramp(controller, from, args.from, to, args.to, step, -1.0),
		CurveShape::Sine => {
			let cycles = args.cycles.unwrap_or(1.0).clamp(0.25, 256.0);
			let span = (to - from).0;
			let mut out = Vec::new();
			let mut t = 0;
			while t <= span {
				let phase = t as f64 / span as f64 * cycles * std::f64::consts::TAU;
				let mid = (args.from + args.to) * 0.5;
				let amp = (args.to - args.from) * 0.5;
				let v = (mid - amp * phase.cos() as f32).clamp(lo, hi);
				out.push(ControllerPoint::new(Tick(from.0 + t), v));
				if t == span {
					break;
				}
				t = (t + step.0.max(1)).min(span);
			}
			out
		}
	};
	let points = splice(&old, from, to, &new);
	let event_count = points.len();
	session.document_mut().execute(Command::SetControllerLane {
		clip: args.clip_id,
		controller,
		points,
	})?;
	Ok(ControllerLaneOutput {
		controller: controller.to_string(),
		event_count,
	})
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetControllerEventsArgs {
	pub clip_id: ClipId,
	pub controller: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ControllerEventOutput {
	pub beat: f64,
	pub value: f32,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct GetControllerEventsOutput {
	pub controller: String,
	pub events: Vec<ControllerEventOutput>,
}

pub fn get_controller_events(
	session: &Session,
	args: GetControllerEventsArgs,
) -> Result<GetControllerEventsOutput, McpError> {
	let controller = parse(&args.controller)?;
	let (_, points) = lane_points(session, args.clip_id, controller)?;
	Ok(GetControllerEventsOutput {
		controller: controller.to_string(),
		events: points
			.iter()
			.map(|p| ControllerEventOutput {
				beat: p.tick.beats(),
				value: p.value,
			})
			.collect(),
	})
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::tools::clips::{AddMidiClipArgs, add_midi_clip};
	use crate::tools::tracks::{CreateTrackArgs, create_track};

	fn clip(session: &mut Session) -> ClipId {
		let track = create_track(
			session,
			serde_json::from_value::<CreateTrackArgs>(
				serde_json::json!({"name": "Lead", "kind": "instrument"}),
			)
			.unwrap(),
		)
		.unwrap()
		.track_id;
		add_midi_clip(
			session,
			AddMidiClipArgs {
				track_id: track,
				start_beat: 0.0,
				length_beats: 8.0,
				name: None,
				notes: Vec::new(),
			},
		)
		.unwrap()
		.clip_id
	}

	#[test]
	fn events_curves_and_errors() {
		let mut s = Session::new();
		let clip_id = clip(&mut s);
		let out = set_controller_events(
			&mut s,
			SetControllerEventsArgs {
				clip_id,
				controller: "sustain".into(),
				events: vec![
					ControllerEventInput {
						beat: 0.0,
						value: 1.0,
					},
					ControllerEventInput {
						beat: 3.75,
						value: 0.0,
					},
				],
				replace: None,
			},
		)
		.unwrap();
		assert_eq!((out.controller.as_str(), out.event_count), ("cc64", 2));
		let out = draw_controller_curve(
			&mut s,
			DrawControllerCurveArgs {
				clip_id,
				controller: "mod wheel".into(),
				start_beat: 0.0,
				end_beat: 4.0,
				from: 0.0,
				to: 1.0,
				shape: Some(CurveShape::Exponential),
				cycles: None,
				resolution_beats: Some(0.25),
			},
		)
		.unwrap();
		assert!(out.event_count > 8);
		let got = get_controller_events(
			&s,
			GetControllerEventsArgs {
				clip_id,
				controller: "cc1".into(),
			},
		)
		.unwrap();
		assert_eq!(got.events.last().unwrap().value, 1.0);
		assert!(got.events[4].value < 0.25, "exponential starts slowly");
		let err = draw_controller_curve(
			&mut s,
			DrawControllerCurveArgs {
				clip_id,
				controller: "pitch_bend".into(),
				start_beat: 1.0,
				end_beat: 2.0,
				from: 0.0,
				to: 3.0,
				shape: None,
				cycles: None,
				resolution_beats: None,
			},
		)
		.unwrap_err()
		.to_string();
		assert!(err.contains("out of range"), "{err}");
		let err = set_controller_events(
			&mut s,
			SetControllerEventsArgs {
				clip_id,
				controller: "cc200".into(),
				events: vec![],
				replace: None,
			},
		)
		.unwrap_err()
		.to_string();
		assert!(err.contains("cc0"), "{err}");
		// The summary shows the lanes.
		let summary = s.document().project().summary();
		let c = &summary.tracks[0].clips[0];
		assert_eq!(c.controllers.len(), 2);
	}
}
