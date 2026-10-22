//! MIDI learn on the audio thread: hardware controllers mapped in `Project::midi_map`
//! move parameters, faders, pans and send levels as the MIDI arrives.
//!
//! The control side resolves the project's map into a [`MidiMapTable`] (device param
//! indices, current values) and hands it over boxed through a ring; the callback keeps
//! it across graph swaps (entries name devices and tracks by id, like live commands)
//! and returns the old box through a garbage ring. What the controllers did is
//! published wait-free in [`MidiMapMonitor`] so the UI can follow (and learn).

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use cadenza_core::{DeviceId, ParamTarget, TrackId};
use cadenza_midi_io::RawMessage;
use cadenza_model::{Controller, MapMode, Project, Takeover, midi_map::MAX_MIDI_MAPPINGS};

use crate::automation::Resolver;
use crate::graph::Graph;

/// What one mapping moves in the running graph.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum MapTarget {
	Param { device: DeviceId, index: usize },
	Volume(Option<TrackId>),
	Pan(Option<TrackId>),
	Send(DeviceId),
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct MapEntry {
	/// Index of the mapping in `Project::midi_map` (its monitor slot).
	pub slot: usize,
	pub controller: Controller,
	pub channel: Option<u8>,
	pub target: MapTarget,
	pub min: f32,
	pub max: f32,
	pub mode: MapMode,
	pub takeover: Takeover,
	/// The target's current plain value.
	pub value: f32,
	/// Pickup: the control has reached the value since it last changed elsewhere.
	pub picked: bool,
	/// Pickup: the control's previous position (0..1), NaN = none yet.
	pub last: f32,
}

impl MapEntry {
	fn position_of(&self, value: f32) -> f32 {
		let span = self.max - self.min;
		if span.abs() < f32::EPSILON {
			0.0
		} else {
			((value - self.min) / span).clamp(0.0, 1.0)
		}
	}

	/// The new target value for controller data `raw` (normalized `position` for
	/// absolute controls), or `None` when nothing moves (pickup not reached yet).
	fn next_value(&mut self, position: f32, data: u8) -> Option<f32> {
		if self.mode == MapMode::Relative {
			let step = if data < 64 {
				f32::from(data)
			} else {
				f32::from(data) - 128.0
			};
			let (lo, hi) = (self.min.min(self.max), self.min.max(self.max));
			let delta = step * (self.max - self.min) / 128.0;
			return Some((self.value + delta).clamp(lo, hi));
		}
		if self.takeover == Takeover::Pickup && !self.picked {
			let current = self.position_of(self.value);
			let near = (position - current).abs() <= 1.5 / 127.0;
			let crossed =
				!self.last.is_nan() && (self.last - current) * (position - current) <= 0.0;
			self.last = position;
			if !(near || crossed) {
				return None;
			}
			self.picked = true;
		}
		self.last = position;
		let span = self.max - self.min;
		Some(self.min + position.clamp(0.0, 1.0) * span)
	}
}

/// The resolved map of the loaded project (boxed to the audio thread).
#[derive(Debug, Default)]
pub(crate) struct MidiMapTable {
	pub entries: Vec<MapEntry>,
}

impl MidiMapTable {
	/// Resolves `project.midi_map` against the loaded graph's `resolver`; mappings the
	/// graph cannot play (bypassed device, unknown param) are left out.
	pub fn build(project: &Project, resolver: Option<&Resolver>) -> Self {
		let mut entries = Vec::with_capacity(project.midi_map.len());
		for (slot, m) in project.midi_map.iter().enumerate().take(MAX_MIDI_MAPPINGS) {
			let track = m.target.track;
			let (target, value) = match &m.target.target {
				ParamTarget::Volume => match project.channel(track) {
					Some(c) => (MapTarget::Volume(track), c.volume_db),
					None => continue,
				},
				ParamTarget::Pan => match project.channel(track) {
					Some(c) => (MapTarget::Pan(track), c.pan),
					None => continue,
				},
				ParamTarget::Send { send } => {
					let level = track.and_then(|t| project.track(t)).and_then(|t| {
						t.channel
							.sends
							.iter()
							.find(|s| s.id == *send)
							.map(|s| s.level_db)
					});
					match level {
						Some(db) => (MapTarget::Send(*send), db),
						None => continue,
					}
				}
				ParamTarget::Device { device, param } => {
					let Some(d) = project.device(track, *device) else {
						continue;
					};
					let info = d.builtin_id().and_then(cadenza_dsp::device_info);
					let index = resolver
						.and_then(|r| r.param_index(*device, param))
						.or_else(|| info.and_then(|i| i.param_index(param)));
					let Some(index) = index else {
						continue;
					};
					let default = info
						.and_then(|i| i.params.get(index))
						.map_or(0.0, |p| p.default);
					let value = d.params.get(param).copied().unwrap_or(default);
					(
						MapTarget::Param {
							device: *device,
							index,
						},
						value,
					)
				}
			};
			entries.push(MapEntry {
				slot,
				controller: m.controller,
				channel: m.channel,
				target,
				min: m.min,
				max: m.max,
				mode: m.mode,
				takeover: m.takeover,
				value,
				picked: false,
				last: f32::NAN,
			});
		}
		Self { entries }
	}

	/// A value set elsewhere (UI, AI): relative steps start from it and pickup waits
	/// for the control again. Realtime safe.
	pub fn external(&mut self, target: MapTarget, value: f32) {
		for e in self.entries.iter_mut().filter(|e| e.target == target) {
			if e.value != value {
				e.picked = false;
			}
			e.value = value;
		}
	}

	/// Applies `raw` (a controller message) to every entry listening to it; returns
	/// whether one did (the message is then consumed: it does not reach the instrument
	/// or a MIDI recording). Realtime safe.
	pub fn apply(
		&mut self,
		raw: &RawMessage,
		graph: Option<&mut Graph>,
		monitor: &MidiMapMonitor,
	) -> bool {
		let Some((controller, channel, position, data)) = controller_of(raw) else {
			return false;
		};
		let mut graph = graph;
		let mut hit = false;
		for e in &mut self.entries {
			if e.controller != controller || e.channel.is_some_and(|c| c != channel) {
				continue;
			}
			hit = true;
			let Some(value) = e.next_value(position, data) else {
				continue;
			};
			e.value = value;
			if let Some(g) = graph.as_deref_mut() {
				match e.target {
					MapTarget::Param { device, index } => {
						g.set_device_param(device, index, value);
					}
					MapTarget::Volume(track) => {
						g.set_channel_volume(track, value);
					}
					MapTarget::Pan(track) => {
						g.set_channel_pan(track, value);
					}
					MapTarget::Send(send) => {
						g.set_send_level(send, value);
					}
				}
			}
			monitor.publish(e.slot, value);
		}
		hit
	}
}

/// `(controller, channel, normalized position, data byte)` of a controller message.
fn controller_of(raw: &RawMessage) -> Option<(Controller, u8, f32, u8)> {
	let channel = raw.channel()?;
	let b = raw.bytes;
	match (raw.status() & 0xF0, raw.len) {
		(0xB0, 3) => {
			let d = b[2] & 0x7f;
			Some((
				Controller::Cc(b[1] & 0x7f),
				channel,
				f32::from(d) / 127.0,
				d,
			))
		}
		(0xE0, 3) => {
			let v = u16::from(b[1] & 0x7f) | (u16::from(b[2] & 0x7f) << 7);
			Some((
				Controller::PitchBend,
				channel,
				f32::from(v) / 16383.0,
				b[2] & 0x7f,
			))
		}
		(0xD0, 2..=3) => {
			let d = b[1] & 0x7f;
			Some((
				Controller::ChannelPressure,
				channel,
				f32::from(d) / 127.0,
				d,
			))
		}
		_ => None,
	}
}

/// The last controller the input sent (for MIDI learn).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ControllerInput {
	/// Bumped by every controller message (never 0).
	pub seq: u32,
	pub controller: Controller,
	/// 0..=15.
	pub channel: u8,
	/// Normalized position 0..1.
	pub position: f32,
}

/// What the MIDI controllers did, published by the audio thread (wait-free) for the UI:
/// the last controller message (MIDI learn) and, per mapping of `Project::midi_map`
/// (by index), the last value it set.
#[derive(Debug)]
pub struct MidiMapMonitor {
	/// seq (32) | status (8) | data1 (8) | data2 (8).
	last: AtomicU64,
	values: Box<[(AtomicU32, AtomicU32)]>,
}

impl Default for MidiMapMonitor {
	fn default() -> Self {
		Self {
			last: AtomicU64::new(0),
			values: (0..MAX_MIDI_MAPPINGS)
				.map(|_| (AtomicU32::new(0), AtomicU32::new(0)))
				.collect(),
		}
	}
}

impl MidiMapMonitor {
	/// Records a controller message for learning. Realtime safe.
	pub(crate) fn heard(&self, raw: &RawMessage) {
		if controller_of(raw).is_none() {
			return;
		}
		let seq = (self.last.load(Ordering::Relaxed) >> 32) as u32;
		let seq = seq.wrapping_add(1).max(1);
		let packed = (u64::from(seq) << 32)
			| (u64::from(raw.bytes[0]) << 16)
			| (u64::from(raw.bytes[1]) << 8)
			| u64::from(if raw.len == 3 { raw.bytes[2] } else { 0 });
		self.last.store(packed, Ordering::Release);
	}

	fn publish(&self, slot: usize, value: f32) {
		if let Some((seq, bits)) = self.values.get(slot) {
			bits.store(value.to_bits(), Ordering::Relaxed);
			let next = seq.load(Ordering::Relaxed).wrapping_add(1).max(1);
			seq.store(next, Ordering::Release);
		}
	}

	/// The last controller message heard (`None` before the first).
	pub fn last_controller(&self) -> Option<ControllerInput> {
		let packed = self.last.load(Ordering::Acquire);
		let seq = (packed >> 32) as u32;
		if seq == 0 {
			return None;
		}
		let status = (packed >> 16) as u8;
		let len = if status & 0xF0 == 0xD0 { 2 } else { 3 };
		let raw = RawMessage::from_bytes(&[status, (packed >> 8) as u8, packed as u8][..len])?;
		let (controller, channel, position, _) = controller_of(&raw)?;
		Some(ControllerInput {
			seq,
			controller,
			channel,
			position,
		})
	}

	/// `(seq, value)` last set by mapping `index` (seq 0 = never).
	pub fn value(&self, index: usize) -> (u32, f32) {
		self.values.get(index).map_or((0, 0.0), |(seq, bits)| {
			let s = seq.load(Ordering::Acquire);
			(s, f32::from_bits(bits.load(Ordering::Relaxed)))
		})
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn entry(mode: MapMode, takeover: Takeover) -> MapEntry {
		MapEntry {
			slot: 0,
			controller: Controller::Cc(74),
			channel: None,
			target: MapTarget::Volume(None),
			min: 0.0,
			max: 100.0,
			mode,
			takeover,
			value: 50.0,
			picked: false,
			last: f32::NAN,
		}
	}

	#[test]
	fn relative_steps_and_pickup() {
		let mut e = entry(MapMode::Relative, Takeover::Jump);
		assert_eq!(e.next_value(0.0, 2), Some(50.0 + 200.0 / 128.0));
		e.value = 0.5;
		assert_eq!(e.next_value(0.0, 127), Some(0.0), "clamped to the range");

		let mut p = entry(MapMode::Absolute, Takeover::Pickup);
		assert_eq!(p.next_value(0.1, 13), None, "far below: waits");
		assert_eq!(p.next_value(0.3, 38), None);
		// Passing over the value picks it up.
		assert!((p.next_value(0.6, 76).unwrap() - 60.0).abs() < 1e-3);
		assert!((p.next_value(0.2, 25).unwrap() - 20.0).abs() < 1e-3);

		let mut j = entry(MapMode::Absolute, Takeover::Jump);
		assert_eq!(j.next_value(1.0, 127), Some(100.0));
	}

	#[test]
	fn the_monitor_packs_the_last_controller() {
		let m = MidiMapMonitor::default();
		assert!(m.last_controller().is_none());
		m.heard(&RawMessage::from_bytes(&[0xB2, 74, 127]).unwrap());
		let c = m.last_controller().unwrap();
		assert_eq!(
			(c.controller, c.channel, c.position),
			(Controller::Cc(74), 2, 1.0)
		);
		m.heard(&RawMessage::from_bytes(&[0xD0, 64]).unwrap());
		assert_eq!(
			m.last_controller().unwrap().controller,
			Controller::ChannelPressure
		);
		m.heard(&RawMessage::from_bytes(&[0x90, 60, 100]).unwrap());
		assert_eq!(
			m.last_controller().unwrap().seq,
			2,
			"notes are not controllers"
		);
	}
}
