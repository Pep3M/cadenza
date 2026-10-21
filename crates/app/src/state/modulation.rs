//! Device modulation (LFOs, envelope followers) for the editor's MOD section and the
//! kit knobs' modulation rings: pure builders returning one `SetModulators` command per
//! gesture, and the ring math (what range a modulated knob sweeps, where it is now).

use cadenza_core::ModulatorId;
use cadenza_model::modulation::{MAX_MODULATORS, MAX_TARGETS, SYNC_STEPS};
use cadenza_model::{Command, Device, ModTarget, Modulator, ModulatorKind};

use super::DeviceRef;

/// Depth of a target created by dropping a modulator on a knob.
pub const NEW_DEPTH: f32 = 0.25;
/// Pixels of vertical drag on a ring for the full depth range (-1..1 is twice this).
pub const DEPTH_DRAG_PX: f32 = 200.0;

/// "LFO 2" / "Env 1": numbered per kind, in device order.
pub fn label(device: &Device, index: usize) -> String {
	let Some(m) = device.modulators.get(index) else {
		return String::new();
	};
	let n = device.modulators[..index]
		.iter()
		.filter(|o| o.is_lfo() == m.is_lfo())
		.count()
		+ 1;
	if m.is_lfo() {
		format!("LFO {n}")
	} else {
		format!("Env {n}")
	}
}

/// One modulator acting on a parameter.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Ring {
	pub modulator: usize,
	pub depth: f32,
	/// LFO (swings both ways) vs envelope (moves one way).
	pub bipolar: bool,
}

/// The modulators targeting `param`, in device order.
pub fn rings(device: &Device, param: &str) -> Vec<Ring> {
	device
		.modulators
		.iter()
		.enumerate()
		.filter_map(|(i, m)| {
			m.targets.iter().find(|t| t.param == param).map(|t| Ring {
				modulator: i,
				depth: t.depth,
				bipolar: m.is_lfo(),
			})
		})
		.collect()
}

/// The plain range `base` can be modulated over (clamped to min..max).
pub fn extent(base: f32, rings: &[Ring], min: f32, max: f32) -> (f32, f32) {
	let range = max - min;
	let (mut lo, mut hi) = (0.0f32, 0.0f32);
	for r in rings {
		let d = r.depth * range;
		if r.bipolar {
			lo -= d.abs();
			hi += d.abs();
		} else if d < 0.0 {
			lo += d;
		} else {
			hi += d;
		}
	}
	((base + lo).clamp(min, max), (base + hi).clamp(min, max))
}

/// The modulated value with the modulators at `values` (engine order), as the engine
/// computes it.
pub fn live_value(base: f32, rings: &[Ring], values: &[f32], min: f32, max: f32) -> f32 {
	let m: f32 = rings
		.iter()
		.map(|r| r.depth * values.get(r.modulator).copied().unwrap_or(0.0))
		.sum();
	(base + m * (max - min)).clamp(min, max)
}

/// Depth after dragging a ring `dy` pixels (up = deeper), on the 0.01 grid.
pub fn drag_depth(start: f32, dy: f32) -> f32 {
	((start - dy / DEPTH_DRAG_PX) * 100.0)
		.round()
		.clamp(-100.0, 100.0)
		/ 100.0
}

/// The [`SYNC_STEPS`] index of a synced cycle length.
pub fn sync_index(beats: f32) -> Option<usize> {
	SYNC_STEPS
		.iter()
		.position(|(_, b)| (b - beats).abs() < 1e-4)
}

fn set(r: DeviceRef, modulators: Vec<Modulator>) -> Command {
	Command::SetModulators {
		track: r.track,
		device: r.device,
		modulators,
	}
}

/// Adds an LFO (`lfo`) or an envelope follower; `None` when the device is full.
pub fn add(device: &Device, r: DeviceRef, lfo: bool, id: ModulatorId) -> Option<Command> {
	if device.modulators.len() >= MAX_MODULATORS {
		return None;
	}
	let mut mods = device.modulators.clone();
	mods.push(if lfo {
		Modulator::lfo(id)
	} else {
		Modulator::envelope(id)
	});
	Some(set(r, mods))
}

pub fn remove(device: &Device, r: DeviceRef, index: usize) -> Option<Command> {
	(index < device.modulators.len()).then(|| {
		let mut mods = device.modulators.clone();
		mods.remove(index);
		set(r, mods)
	})
}

/// Replaces the settings of modulator `index` (its targets stay).
pub fn set_kind(
	device: &Device,
	r: DeviceRef,
	index: usize,
	kind: ModulatorKind,
) -> Option<Command> {
	let mut mods = device.modulators.clone();
	let m = mods.get_mut(index)?;
	if m.kind == kind {
		return None;
	}
	m.kind = kind;
	Some(set(r, mods))
}

/// Sets (or creates) modulator `index`'s target on `param`; depth 0 removes it.
/// `None` when nothing changes or the modulator is full.
pub fn set_depth(
	device: &Device,
	r: DeviceRef,
	index: usize,
	param: &str,
	depth: f32,
) -> Option<Command> {
	let depth = depth.clamp(-1.0, 1.0);
	let mut mods = device.modulators.clone();
	let m = mods.get_mut(index)?;
	match m.targets.iter().position(|t| t.param == param) {
		Some(i) if depth == 0.0 => {
			m.targets.remove(i);
		}
		Some(i) if m.targets[i].depth == depth => return None,
		Some(i) => m.targets[i].depth = depth,
		None if depth == 0.0 || m.targets.len() >= MAX_TARGETS => return None,
		None => m.targets.push(ModTarget {
			param: param.to_owned(),
			depth,
		}),
	}
	Some(set(r, mods))
}

/// A modulator dropped on a knob: a new target at [`NEW_DEPTH`] (an existing one keeps
/// its depth: `None`).
pub fn assign(device: &Device, r: DeviceRef, index: usize, param: &str) -> Option<Command> {
	let m = device.modulators.get(index)?;
	if m.targets.iter().any(|t| t.param == param) {
		return None;
	}
	set_depth(device, r, index, param, NEW_DEPTH)
}

#[cfg(test)]
mod tests {
	use super::*;
	use cadenza_core::DeviceId;

	fn device() -> (Device, DeviceRef) {
		let d = Device::builtin("poly_synth", "Synth");
		let r = DeviceRef {
			track: None,
			device: d.id,
		};
		(d, r)
	}

	macro_rules! run {
		($d:ident, $e:expr) => {{
			let c = $e;
			apply(&mut $d, c);
		}};
	}

	fn apply(d: &mut Device, cmd: Option<Command>) {
		match cmd {
			Some(Command::SetModulators { modulators, .. }) => d.modulators = modulators,
			other => panic!("unexpected {other:?}"),
		}
	}

	#[test]
	fn builders_add_assign_drag_and_remove() {
		let (mut d, r) = device();
		run!(d, add(&d, r, true, ModulatorId::new()));
		run!(d, add(&d, r, false, ModulatorId::new()));
		run!(d, add(&d, r, true, ModulatorId::new()));
		assert_eq!(label(&d, 0), "LFO 1");
		assert_eq!(label(&d, 1), "Env 1");
		assert_eq!(label(&d, 2), "LFO 2");
		run!(d, assign(&d, r, 0, "filter_cutoff"));
		assert!(assign(&d, r, 0, "filter_cutoff").is_none());
		run!(d, set_depth(&d, r, 1, "filter_cutoff", -0.1));
		let rs = rings(&d, "filter_cutoff");
		assert_eq!(rs.len(), 2);
		assert_eq!(
			rs[0],
			Ring {
				modulator: 0,
				depth: NEW_DEPTH,
				bipolar: true
			}
		);
		// LFO +-0.25, envelope -0.1 of a 0..100 range around 50.
		assert_eq!(extent(50.0, &rs, 0.0, 100.0), (15.0, 75.0));
		assert_eq!(live_value(50.0, &rs, &[1.0, 0.5], 0.0, 100.0), 70.0);
		assert_eq!(live_value(90.0, &rs, &[1.0, 0.0], 0.0, 100.0), 100.0);
		assert_eq!(drag_depth(0.25, -50.0), 0.5);
		assert_eq!(drag_depth(0.9, -100.0), 1.0);
		// Depth 0 removes the target; removing a modulator drops its rings.
		run!(d, set_depth(&d, r, 1, "filter_cutoff", 0.0));
		assert_eq!(rings(&d, "filter_cutoff").len(), 1);
		run!(d, remove(&d, r, 0));
		assert!(rings(&d, "filter_cutoff").is_empty());
		assert!(set_depth(&d, r, 5, "x", 0.2).is_none());
		let full = Device {
			modulators: (0..MAX_MODULATORS)
				.map(|_| Modulator::lfo(ModulatorId::new()))
				.collect(),
			..Device::builtin("eq", "EQ")
		};
		let fr = DeviceRef {
			track: None,
			device: DeviceId::new(),
		};
		assert!(add(&full, fr, true, ModulatorId::new()).is_none());
		assert_eq!(sync_index(0.5).map(|i| SYNC_STEPS[i].0), Some("1/8"));
	}
}
