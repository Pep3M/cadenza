//! A/B compare of the device editor and the undo hint of its header (design `14 Device
//! Kit`, "Toggle, segmented selector, A/B, preset bar").
//!
//! A [`Compare`] keeps two full param states of one device as view state. The slot you
//! are on IS the device's live params (whatever you edit belongs to it), so only the other
//! slot is stored: switching swaps the live params with it. [`StepTouches`] remembers
//! which devices each undo step changed, so the header shows the undo label only when
//! the step it would undo touched this device.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use cadenza_core::DeviceId;
use cadenza_model::{Device, Project};

pub type Params = BTreeMap<String, f32>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
	A,
	B,
}

impl Slot {
	pub fn other(self) -> Self {
		match self {
			Self::A => Self::B,
			Self::B => Self::A,
		}
	}

	pub fn name(self) -> &'static str {
		match self {
			Self::A => "A",
			Self::B => "B",
		}
	}

	/// The undo label of switching to this slot.
	pub fn label(self) -> String {
		format!("{COMPARE_PREFIX}{}", self.name())
	}
}

const COMPARE_PREFIX: &str = "Compare ";

/// Whether an undo step is an A/B switch.
pub fn is_compare_label(label: &str) -> bool {
	label == Slot::A.label() || label == Slot::B.label()
}

/// The two states of one device.
#[derive(Debug, Clone, PartialEq)]
pub struct Compare {
	active: Slot,
	/// The params of the slot you are not on.
	other: Params,
}

impl Compare {
	/// Both slots start as the device's current params, on A.
	pub fn new(current: &Params) -> Self {
		Self {
			active: Slot::A,
			other: current.clone(),
		}
	}

	pub fn active(&self) -> Slot {
		self.active
	}

	/// The params `slot` holds, given the live ones.
	pub fn params<'a>(&'a self, slot: Slot, current: &'a Params) -> &'a Params {
		if slot == self.active {
			current
		} else {
			&self.other
		}
	}

	/// What to apply to switch to `slot`: `None` when already on it, `Some(None)` when
	/// the slots are equal (the switch needs no edit), else the params to set.
	pub fn switch_to(&self, slot: Slot, current: &Params) -> Option<Option<Params>> {
		if slot == self.active {
			return None;
		}
		Some((self.other != *current).then(|| self.other.clone()))
	}

	/// The switch happened (or an A/B step was undone / redone): `current` (the params
	/// before it) becomes the slot left, and the other slot is the active one.
	pub fn flip(&mut self, current: &Params) {
		self.other = current.clone();
		self.active = self.active.other();
	}

	/// "A→B": copies the slot you are on onto the other one.
	pub fn copy(&mut self, current: &Params) {
		self.other = current.clone();
	}

	/// Label of [`Compare::copy`], pointing from the active slot.
	pub fn copy_label(&self) -> String {
		format!("{}→{}", self.active.name(), self.active.other().name())
	}

	/// Whether both slots hold the same params.
	pub fn same(&self, current: &Params) -> bool {
		self.other == *current
	}
}

/// Every device of the project by id (instruments, inserts, master inserts).
pub fn devices(project: &Project) -> HashMap<DeviceId, &Device> {
	project
		.tracks
		.iter()
		.flat_map(|t| t.devices())
		.chain(project.master.inserts.iter())
		.map(|d| (d.id, d))
		.collect()
}

/// Devices that differ between `before` and `after` (changed, added or removed).
pub fn touched_devices(before: &Project, after: &Project) -> BTreeSet<DeviceId> {
	let (b, a) = (devices(before), devices(after));
	let mut out: BTreeSet<DeviceId> = b
		.iter()
		.filter(|(id, d)| a.get(id) != Some(d))
		.map(|(id, _)| *id)
		.collect();
	out.extend(a.keys().filter(|id| !b.contains_key(id)));
	out
}

/// How the undo history moved between two snapshots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
	/// A new step on top (the redo stack cleared).
	New,
	Undo,
	Redo,
	/// Several steps folded into one (an undo group closing).
	Fold,
	/// Nothing recognisable (history cleared, another document).
	Other,
}

/// Classifies a history change from the (undo, redo) stack sizes before and after.
pub fn step(before: (usize, usize), after: (usize, usize)) -> Step {
	let (bu, br) = before;
	let (au, ar) = after;
	if au + 1 == bu && ar == br + 1 {
		Step::Undo
	} else if au == bu + 1 && ar + 1 == br {
		Step::Redo
	} else if au == bu + 1 && ar == 0 {
		Step::New
	} else if au < bu && au > 0 && ar == br {
		Step::Fold
	} else {
		Step::Other
	}
}

/// The devices each undo / redo step changed, kept aligned with the history.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StepTouches {
	undo: Vec<BTreeSet<DeviceId>>,
	redo: Vec<BTreeSet<DeviceId>>,
}

impl StepTouches {
	/// Follows a history change; `touched` = the devices the change modified.
	pub fn update(
		&mut self,
		before: (usize, usize),
		after: (usize, usize),
		touched: BTreeSet<DeviceId>,
	) {
		match step(before, after) {
			Step::Undo => {
				let t = self.undo.pop().unwrap_or_default();
				self.redo.push(t);
			}
			// A redo with one step left looks like a new step: either way the step on
			// top changed what the diff says.
			Step::Redo => {
				self.redo.pop();
				self.undo.push(touched);
			}
			Step::New => {
				self.redo.clear();
				self.undo.push(touched);
			}
			Step::Fold => {
				let keep = after.0 - 1;
				let mut merged = touched;
				for t in self.undo.drain(keep.min(self.undo.len())..) {
					merged.extend(t);
				}
				self.undo.push(merged);
			}
			Step::Other => {
				self.undo.resize_with(after.0, BTreeSet::new);
				self.redo.resize_with(after.1, BTreeSet::new);
				if let Some(top) = self.undo.last_mut() {
					top.extend(touched);
				}
			}
		}
		// Keep the bookkeeping aligned whatever happened.
		self.undo.resize_with(after.0, BTreeSet::new);
		self.redo.resize_with(after.1, BTreeSet::new);
	}

	/// Whether the step undo would revert touched `device`.
	pub fn last_touches(&self, device: DeviceId) -> bool {
		self.undo.last().is_some_and(|t| t.contains(&device))
	}

	pub fn clear(&mut self) {
		self.undo.clear();
		self.redo.clear();
	}
}

/// The header's undo hint: "Ctrl+Z <label>" ("⌘Z" on macOS).
pub fn undo_hint(label: &str) -> String {
	let key = if cfg!(target_os = "macos") {
		"⌘Z"
	} else {
		"Ctrl+Z"
	};
	format!("{key} {label}")
}

#[cfg(test)]
mod tests {
	use super::*;

	fn p(pairs: &[(&str, f32)]) -> Params {
		pairs.iter().map(|(k, v)| ((*k).to_owned(), *v)).collect()
	}

	#[test]
	fn switching_swaps_the_live_params_with_the_other_slot() {
		let a = p(&[("gain", 0.0)]);
		let mut c = Compare::new(&a);
		assert_eq!(c.active(), Slot::A);
		assert!(c.same(&a), "both slots start as the current params");
		assert_eq!(c.switch_to(Slot::A, &a), None, "already on A");
		assert_eq!(c.switch_to(Slot::B, &a), Some(None), "equal slots: no edit");
		// Editing A (the live params) then switching restores B's params.
		let edited = p(&[("gain", 3.0)]);
		assert_eq!(c.switch_to(Slot::B, &edited), Some(Some(a.clone())));
		c.flip(&edited);
		assert_eq!(c.active(), Slot::B);
		assert_eq!(c.params(Slot::A, &a), &edited, "A kept the live edit");
		assert_eq!(c.params(Slot::B, &a), &a);
		// Back to A brings the edit back.
		assert_eq!(c.switch_to(Slot::A, &a), Some(Some(edited.clone())));
	}

	#[test]
	fn the_active_slot_tracks_live_edits_and_copies_onto_the_other() {
		let start = p(&[("ratio", 4.0)]);
		let mut c = Compare::new(&start);
		c.flip(&start);
		assert_eq!(c.active(), Slot::B);
		let tweaked = p(&[("ratio", 8.0)]);
		assert_eq!(c.params(Slot::B, &tweaked), &tweaked, "B is what plays");
		assert_eq!(c.copy_label(), "B→A");
		c.copy(&tweaked);
		assert!(c.same(&tweaked));
		assert_eq!(c.params(Slot::A, &tweaked), &tweaked);
		c.flip(&tweaked);
		assert_eq!(c.copy_label(), "A→B");
	}

	#[test]
	fn labels_name_the_slot() {
		assert_eq!(Slot::B.label(), "Compare B");
		assert!(is_compare_label("Compare A"));
		assert!(!is_compare_label("Compare C") && !is_compare_label("Set param"));
		assert!(undo_hint("Peak 2 gain").ends_with("Z Peak 2 gain"));
	}

	#[test]
	fn step_touches_follow_new_undo_redo_and_folds() {
		let (d1, d2) = (DeviceId::new(), DeviceId::new());
		let one = |d: DeviceId| BTreeSet::from([d]);
		let mut t = StepTouches::default();
		t.update((0, 0), (1, 0), one(d1));
		t.update((1, 0), (2, 0), one(d2));
		assert!(t.last_touches(d2) && !t.last_touches(d1));
		t.update((2, 0), (1, 1), one(d2));
		assert!(t.last_touches(d1), "undo uncovers the previous step");
		t.update((1, 1), (2, 0), one(d2));
		assert!(t.last_touches(d2), "redo puts it back");
		// A group of two steps folds into one touching both.
		t.update((2, 0), (3, 0), one(d1));
		t.update((3, 0), (4, 0), BTreeSet::new());
		t.update((4, 0), (3, 0), BTreeSet::new());
		assert!(t.last_touches(d1));
		assert_eq!(step((4, 0), (3, 0)), Step::Fold);
		t.update((3, 0), (0, 0), BTreeSet::new());
		assert!(!t.last_touches(d1), "history cleared");
	}
}
