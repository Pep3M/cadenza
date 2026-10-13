//! Automation lanes resolved to sample positions, and the [`Resolver`] that maps
//! parameter addresses to the parts of a compiled graph they drive.
//!
//! Values follow each point's [`PointShape`] in time between points (interpolated in
//! samples, which equals seconds, like [`cadenza_model::automation_value`]) and are held
//! before the first and after the last point. The engine evaluates every lane once per
//! [`crate::QUANTUM`] samples.

use std::collections::{HashMap, HashSet};

use cadenza_core::{DeviceId, ParamAddress, ParamTarget, TempoMap, TrackId};
use cadenza_dsp::DeviceInfo;
use cadenza_model::{AutomationLane, PointShape, Project};

#[derive(Debug, Clone)]
pub(crate) struct Curve {
	/// `(sample, value, shape of the segment after it)`, strictly increasing in sample.
	points: Vec<(i64, f32, PointShape)>,
}

impl Curve {
	pub fn from_lane(lane: &AutomationLane, tempo: &TempoMap, sample_rate: f64) -> Option<Self> {
		let mut points: Vec<(i64, f32, PointShape)> = Vec::with_capacity(lane.points.len());
		let mut sorted = lane.points.clone();
		sorted.sort_by_key(|p| p.tick);
		for p in sorted {
			if !p.value.is_finite() {
				continue;
			}
			let sample =
				tempo.tick_to_sample(p.tick.max(cadenza_core::Tick::ZERO), sample_rate) as i64;
			match points.last_mut() {
				// Two ticks rounding to the same sample: the later point wins.
				Some(last) if last.0 == sample => {
					last.1 = p.value;
					last.2 = p.shape;
				}
				_ => points.push((sample, p.value, p.shape)),
			}
		}
		(!points.is_empty()).then_some(Self { points })
	}

	pub fn value_at(&self, sample: i64) -> f32 {
		let i = self.points.partition_point(|p| p.0 <= sample);
		if i == 0 {
			return self.points[0].1;
		}
		if i == self.points.len() {
			return self.points[i - 1].1;
		}
		let (s0, v0, shape) = self.points[i - 1];
		let (s1, v1, _) = self.points[i];
		let t = (sample - s0) as f64 / (s1 - s0) as f64;
		shape.interpolate(v0, v1, t)
	}
}

/// Which slot of a channel's device chain a parameter lane drives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum DeviceSlot {
	Instrument,
	Insert(usize),
}

/// `None` node = master channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum AutoTarget {
	Volume {
		node: Option<usize>,
	},
	/// A folder's volume: an offset for the tracks inside.
	FolderVolume {
		folder: usize,
	},
	Pan {
		node: Option<usize>,
	},
	Param {
		node: Option<usize>,
		slot: DeviceSlot,
		index: usize,
	},
	/// Level of the send edge `edge` of `node`.
	Send {
		node: usize,
		edge: usize,
	},
}

#[derive(Debug, Clone)]
pub(crate) struct Binding {
	pub curve: Curve,
	pub target: AutoTarget,
	/// Last value sent, to avoid redundant `set_param` calls; NaN forces a send.
	pub last: f32,
}

/// A device of a compiled graph, as the resolver knows it.
#[derive(Debug, Clone, Copy)]
pub(crate) struct DeviceEntry {
	pub node: Option<usize>,
	pub slot: DeviceSlot,
	pub info: &'static DeviceInfo,
}

/// Why an address cannot drive anything in the graph.
pub(crate) enum Unresolved {
	Track,
	/// Not in the graph (missing from the project, or bypassed/unsupported).
	Device,
	Param(&'static str),
	/// Missing, or dropped while compiling (invalid target, feedback loop).
	Send,
}

/// Maps parameter addresses to graph targets. Built while compiling and kept by the
/// player (off the audio thread) so parameter changes and automation edits can reach
/// the running graph without recompiling it.
#[derive(Debug, Clone, Default)]
pub(crate) struct Resolver {
	pub nodes: HashMap<TrackId, usize>,
	/// Folder tracks -> index in `Graph::folders` (their volume is a group offset).
	pub folders: HashMap<TrackId, usize>,
	pub devices: HashMap<DeviceId, DeviceEntry>,
	pub sends: HashMap<DeviceId, (usize, usize)>,
	/// Nodes with an instrument source (MIDI schedules).
	pub instruments: HashSet<usize>,
	/// Per node: the bus nodes it feeds directly (output, sends, extra outputs).
	pub feeds: Vec<Vec<usize>>,
	/// Tempo the graph's curves were computed with.
	pub tempo: TempoMap,
	pub sample_rate: f64,
}

impl Resolver {
	/// One flag per node: `tracks` and every bus they reach downstream.
	pub fn audition_mask(&self, tracks: &[TrackId]) -> Vec<bool> {
		let mut on = vec![false; self.feeds.len()];
		let mut stack: Vec<usize> = tracks
			.iter()
			.filter_map(|t| self.nodes.get(t).copied())
			.collect();
		while let Some(i) = stack.pop() {
			if on.get(i).copied().unwrap_or(true) {
				continue;
			}
			on[i] = true;
			stack.extend(&self.feeds[i]);
		}
		on
	}

	pub fn target(&self, address: &ParamAddress) -> Result<AutoTarget, Unresolved> {
		if let (Some(id), ParamTarget::Volume) = (address.track, &address.target)
			&& let Some(&folder) = self.folders.get(&id)
		{
			return Ok(AutoTarget::FolderVolume { folder });
		}
		let node = match address.track {
			None => None,
			Some(id) => Some(*self.nodes.get(&id).ok_or(Unresolved::Track)?),
		};
		Ok(match &address.target {
			ParamTarget::Volume => AutoTarget::Volume { node },
			ParamTarget::Pan => AutoTarget::Pan { node },
			ParamTarget::Device { device, param } => {
				let entry = self
					.devices
					.get(device)
					.filter(|e| e.node == node)
					.ok_or(Unresolved::Device)?;
				let index = entry
					.info
					.param_index(param)
					.ok_or(Unresolved::Param(entry.info.id))?;
				AutoTarget::Param {
					node,
					slot: entry.slot,
					index,
				}
			}
			ParamTarget::Send { send } => {
				let &(n, edge) = self.sends.get(send).ok_or(Unresolved::Send)?;
				if node != Some(n) {
					return Err(Unresolved::Send);
				}
				AutoTarget::Send { node: n, edge }
			}
		})
	}

	/// Index of `param` on `device` (built-in id or plugin param id).
	pub fn param_index(&self, device: DeviceId, param: &str) -> Option<usize> {
		self.devices.get(&device)?.info.param_index(param)
	}

	/// Bindings for `lanes`. Unusable lanes are skipped; when `project` is given, each
	/// one gets an actionable warning (devices present in the project but left out of the
	/// graph, e.g. bypassed, are skipped silently).
	pub fn bind(
		&self,
		lanes: &[AutomationLane],
		tempo: &TempoMap,
		project: Option<&Project>,
		warnings: &mut Vec<String>,
	) -> Vec<Binding> {
		let mut bindings = Vec::with_capacity(lanes.len());
		for lane in lanes {
			let Some(curve) = Curve::from_lane(lane, tempo, self.sample_rate) else {
				continue;
			};
			let address = &lane.address;
			let target = match self.target(address) {
				Ok(t) => t,
				Err(why) => {
					if let Some(project) = project {
						warn(project, address, why, warnings);
					}
					continue;
				}
			};
			bindings.push(Binding {
				curve,
				target,
				last: f32::NAN,
			});
		}
		bindings
	}
}

fn warn(project: &Project, address: &ParamAddress, why: Unresolved, warnings: &mut Vec<String>) {
	match why {
		Unresolved::Track => warnings.push(format!(
			"automation lane '{address}' targets a track that does not exist; ignored"
		)),
		Unresolved::Device => {
			let ParamTarget::Device { device, .. } = &address.target else {
				return;
			};
			if project.device(address.track, *device).is_none() {
				warnings.push(format!(
					"automation lane '{address}' targets a device that does not exist; ignored"
				));
			}
		}
		Unresolved::Param(id) => {
			let param = match &address.target {
				ParamTarget::Device { param, .. } => param.as_str(),
				_ => "?",
			};
			warnings.push(format!(
				"automation lane '{address}': device '{id}' has no parameter '{param}'; ignored"
			));
		}
		Unresolved::Send => warnings.push(format!(
			"automation lane '{address}' targets a send that does not exist or is not connected; ignored"
		)),
	}
}

/// Automation lanes resolved against one compiled graph (see
/// [`crate::Graph::prepare_automation`]).
#[derive(Debug, Clone, Default)]
pub struct AutomationBindings(pub(crate) Vec<Binding>);

/// A new set of automation bindings for the graph loaded as `load_id` (see
/// [`crate::Player::set_automation`]); travels to the audio thread and back in a box.
pub(crate) struct AutomationSet {
	pub load_id: u64,
	pub bindings: AutomationBindings,
}

/// Targets driven by `bindings` (to tell whether a lane disappeared).
pub(crate) fn targets(bindings: &[Binding]) -> HashSet<AutoTarget> {
	bindings.iter().map(|b| b.target).collect()
}

#[cfg(test)]
mod tests {
	use super::*;
	use cadenza_core::{PPQ, ParamAddress, Tick, TrackId};
	use cadenza_model::AutomationPoint;

	#[test]
	fn interpolates_and_holds() {
		let lane = AutomationLane::new(
			ParamAddress::track_volume(TrackId::new()),
			vec![
				AutomationPoint::new(Tick(PPQ), -10.0),
				AutomationPoint::new(Tick(PPQ * 2), 0.0),
			],
		);
		let c = Curve::from_lane(&lane, &TempoMap::constant(120.0), 48_000.0).unwrap();
		assert_eq!(c.value_at(0), -10.0);
		assert_eq!(c.value_at(24_000), -10.0);
		assert!((c.value_at(36_000) + 5.0).abs() < 1e-6);
		assert_eq!(c.value_at(48_000), 0.0);
		assert_eq!(c.value_at(1_000_000), 0.0);
	}

	#[test]
	fn shapes_match_the_model() {
		let tempo = TempoMap::constant(100.0);
		let points = vec![
			AutomationPoint::new(Tick(0), 0.0).with_shape(PointShape::Smooth),
			AutomationPoint::new(Tick(PPQ * 2), 1.0).with_shape(PointShape::Hold),
			AutomationPoint::new(Tick(PPQ * 4), 0.0),
		];
		let lane = AutomationLane::new(ParamAddress::track_pan(TrackId::new()), points.clone());
		let c = Curve::from_lane(&lane, &tempo, 48_000.0).unwrap();
		for beat in [0.3, 0.9, 1.5, 2.5, 3.9, 5.0] {
			let tick = Tick::from_beats(beat);
			let sample = tempo.tick_to_sample(tick, 48_000.0) as i64;
			let model = cadenza_model::automation_value(&points, tick, &tempo).unwrap();
			assert!((c.value_at(sample) - model).abs() < 1e-3, "beat {beat}");
		}
	}

	#[test]
	fn empty_lane_has_no_curve() {
		let lane = AutomationLane::new(ParamAddress::track_pan(TrackId::new()), vec![]);
		assert!(Curve::from_lane(&lane, &TempoMap::constant(120.0), 48_000.0).is_none());
	}
}
