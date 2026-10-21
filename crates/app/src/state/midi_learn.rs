//! MIDI learn (T36): hardware controllers assigned to device parameters, faders, pans and
//! send levels, stored in `Project::midi_map` and edited with one `SetMidiMap` step each.
//!
//! The engine applies mapped controllers on the audio thread and publishes what they did
//! in a [`MidiMapMonitor`]; [`AppState::tick`] reads it: a learn in progress takes the
//! next controller heard, and values the controllers set are mirrored into the UI like a
//! gesture (previews, so automation Touch/Latch writes them) and committed as one
//! command once the control rests ([`COMMIT_AFTER`]).

use std::sync::Arc;
use std::time::{Duration, Instant};

use cadenza_core::{ParamAddress, ParamTarget};
use cadenza_engine::{ControllerInput, MidiMapMonitor};
use cadenza_model::{Command, Controller, MapMode, MidiMapping, Project, Takeover};

use super::AppState;

/// A controller that stops moving for this long ends its gesture (one undo step).
pub const COMMIT_AFTER: Duration = Duration::from_millis(400);

/// A learn waiting for the next controller message.
#[derive(Debug, Clone, PartialEq)]
pub struct Learning {
	pub target: ParamAddress,
	/// The monitor's controller sequence when the learn started (older messages do not
	/// count).
	pub since: u32,
	pub started: Instant,
}

/// A mapped controller's value shown in the UI and not committed yet.
#[derive(Debug, Clone, PartialEq)]
struct Moving {
	target: ParamAddress,
	value: f32,
	last: Instant,
}

#[derive(Default)]
pub struct MidiLearnState {
	monitor: Option<Arc<MidiMapMonitor>>,
	pub learning: Option<Learning>,
	/// The "MIDI mappings" window is open.
	pub window_open: bool,
	/// The map the monitor slots belong to, and the last sequence seen per slot.
	known: Vec<MidiMapping>,
	seen: Vec<u32>,
	moving: Vec<Moving>,
}

impl std::fmt::Debug for MidiLearnState {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("MidiLearnState")
			.field("attached", &self.monitor.is_some())
			.field("learning", &self.learning)
			.field("window_open", &self.window_open)
			.finish()
	}
}

/// Default range of a new assignment: the parameter's own range, a useful fader range
/// (-60..+6 dB), full pan.
pub fn default_range(project: &Project, target: &ParamAddress) -> Option<(f32, f32)> {
	match &target.target {
		ParamTarget::Volume | ParamTarget::Send { .. } => Some((-60.0, 6.0)),
		ParamTarget::Pan => Some((-1.0, 1.0)),
		ParamTarget::Device { device, param } => {
			let d = project.device(target.track, *device)?;
			let info = cadenza_dsp::device_info(d.builtin_id()?)?;
			let p = info.params.iter().find(|p| p.id == param)?;
			Some((p.min, p.max))
		}
	}
}

/// Whether a control can be MIDI-mapped (built-in params, volume, pan, sends).
pub fn mappable(project: &Project, target: &ParamAddress) -> bool {
	default_range(project, target).is_some()
}

/// The assignment of `target`, if any.
pub fn mapping_for<'a>(project: &'a Project, target: &ParamAddress) -> Option<&'a MidiMapping> {
	project.midi_map.iter().find(|m| m.target == *target)
}

/// The map after learning `controller` for `target`: it replaces the target's old
/// assignment and takes the controller away from whatever it moved before.
pub fn learned_map(
	project: &Project,
	target: &ParamAddress,
	input: &ControllerInput,
) -> Option<Vec<MidiMapping>> {
	let (min, max) = default_range(project, target)?;
	let mut map: Vec<MidiMapping> = project
		.midi_map
		.iter()
		.filter(|m| m.target != *target && m.controller != input.controller)
		.cloned()
		.collect();
	map.push(MidiMapping {
		controller: input.controller,
		channel: None,
		target: target.clone(),
		min,
		max,
		mode: MapMode::Absolute,
		takeover: Takeover::Jump,
	});
	Some(map)
}

/// The map without `target`'s assignment (`None` when it had none).
pub fn forgotten_map(project: &Project, target: &ParamAddress) -> Option<Vec<MidiMapping>> {
	mapping_for(project, target)?;
	Some(
		project
			.midi_map
			.iter()
			.filter(|m| m.target != *target)
			.cloned()
			.collect(),
	)
}

/// Undo label of a learn ("MIDI learn CC 74").
pub fn learn_label(controller: Controller) -> String {
	let tag = MidiMapping {
		controller,
		channel: None,
		target: ParamAddress {
			track: None,
			target: ParamTarget::Volume,
		},
		min: 0.0,
		max: 1.0,
		mode: MapMode::Absolute,
		takeover: Takeover::Jump,
	}
	.tag();
	format!("MIDI learn {tag}")
}

impl AppState {
	/// Feeds MIDI learn from a player's monitor (the audio thread's, or a test player's).
	pub fn attach_midi_map(&mut self, monitor: Option<Arc<MidiMapMonitor>>) {
		self.midi.monitor = monitor;
		self.midi.known.clear();
		self.midi.seen.clear();
	}

	/// Waits for the next controller to assign it to `target`.
	pub fn start_midi_learn(&mut self, target: ParamAddress) {
		let since = self
			.midi
			.monitor
			.as_ref()
			.and_then(|m| m.last_controller())
			.map_or(0, |c| c.seq);
		self.midi.learning = Some(Learning {
			target,
			since,
			started: Instant::now(),
		});
	}

	pub fn cancel_midi_learn(&mut self) {
		self.midi.learning = None;
	}

	pub fn is_learning(&self, target: &ParamAddress) -> bool {
		self.midi
			.learning
			.as_ref()
			.is_some_and(|l| l.target == *target)
	}

	pub fn midi_learning(&self) -> Option<&Learning> {
		self.midi.learning.as_ref()
	}

	/// Whether the UI should keep polling the controllers (a learn waits, assignments
	/// exist, or a mapped control is moving).
	pub fn midi_listening(&self) -> bool {
		self.midi.monitor.is_some()
			&& (self.midi.learning.is_some()
				|| !self.project().midi_map.is_empty()
				|| !self.midi.moving.is_empty())
	}

	/// Learns `input` for `target` now (one undo step).
	pub fn learn_midi(&mut self, target: &ParamAddress, input: &ControllerInput) -> bool {
		let Some(map) = learned_map(self.project(), target, input) else {
			return false;
		};
		self.midi.learning = None;
		self.apply(Command::Batch {
			label: learn_label(input.controller),
			commands: vec![Command::SetMidiMap { map }],
		})
	}

	/// Removes `target`'s assignment (one undo step).
	pub fn forget_midi(&mut self, target: &ParamAddress) -> bool {
		let Some(map) = forgotten_map(self.project(), target) else {
			return false;
		};
		self.apply(Command::Batch {
			label: "Forget MIDI assignment".into(),
			commands: vec![Command::SetMidiMap { map }],
		})
	}

	/// Replaces the whole map (the mappings window's edits).
	pub fn set_midi_map(&mut self, map: Vec<MidiMapping>) -> bool {
		if map == self.project().midi_map {
			return false;
		}
		self.apply(Command::SetMidiMap { map })
	}

	/// Learn + mirror, once per frame (see the module docs).
	pub(super) fn tick_midi_learn(&mut self, now: Instant) {
		let Some(monitor) = self.midi.monitor.clone() else {
			return;
		};
		if let Some(learning) = self.midi.learning.clone()
			&& let Some(input) = monitor.last_controller()
			&& input.seq != learning.since
		{
			self.learn_midi(&learning.target, &input);
		}
		let map = self.project().midi_map.clone();
		if map != self.midi.known {
			// New slots: what they did before belongs to the old map.
			self.midi.seen = (0..map.len()).map(|i| monitor.value(i).0).collect();
			self.midi.known = map.clone();
		}
		for (i, m) in map.iter().enumerate() {
			let (seq, value) = monitor.value(i);
			if seq == self.midi.seen[i] {
				continue;
			}
			self.midi.seen[i] = seq;
			self.mirror_value(&m.target, value);
			match self.midi.moving.iter_mut().find(|v| v.target == m.target) {
				Some(v) => {
					v.value = value;
					v.last = now;
				}
				None => self.midi.moving.push(Moving {
					target: m.target.clone(),
					value,
					last: now,
				}),
			}
		}
		let (done, still): (Vec<Moving>, Vec<Moving>) = std::mem::take(&mut self.midi.moving)
			.into_iter()
			.partition(|v| now.saturating_duration_since(v.last) >= COMMIT_AFTER);
		self.midi.moving = still;
		for v in done {
			self.commit_address(&v.target, v.value);
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use cadenza_model::{Device, Track, TrackKind};

	fn input(cc: u8) -> ControllerInput {
		ControllerInput {
			seq: 1,
			controller: Controller::Cc(cc),
			channel: 0,
			position: 0.5,
		}
	}

	#[test]
	fn learning_replaces_the_target_and_takes_the_controller() {
		let mut p = Project::new("t");
		let synth = Device::builtin("poly_synth", "Synth");
		let id = synth.id;
		let t = Track::new("Keys", TrackKind::Instrument).with_instrument(synth);
		let track = t.id;
		p.tracks.push(t);
		let cutoff = ParamAddress::device(Some(track), id, "filter_cutoff");
		let volume = ParamAddress::track_volume(track);
		let map = learned_map(&p, &cutoff, &input(74)).unwrap();
		assert_eq!(map.len(), 1);
		assert_eq!((map[0].min, map[0].max), (20.0, 20000.0));
		p.midi_map = map;
		// The same CC on another target moves over; a new CC for cutoff replaces it.
		let moved = learned_map(&p, &volume, &input(74)).unwrap();
		assert_eq!(moved.len(), 1);
		assert_eq!(moved[0].target, volume);
		assert_eq!(moved[0].min, -60.0);
		let replaced = learned_map(&p, &cutoff, &input(71)).unwrap();
		assert_eq!(replaced.len(), 1);
		assert_eq!(replaced[0].controller, Controller::Cc(71));
		assert_eq!(mapping_for(&p, &cutoff).unwrap().tag(), "CC 74");
		assert!(forgotten_map(&p, &cutoff).unwrap().is_empty());
		assert!(forgotten_map(&p, &volume).is_none());
		assert_eq!(learn_label(Controller::Cc(74)), "MIDI learn CC 74");
		// Plugins cannot be mapped.
		let gone = ParamAddress::device(Some(track), cadenza_core::DeviceId::new(), "x");
		assert!(!mappable(&p, &gone));
	}
}
