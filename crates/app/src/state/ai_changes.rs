//! What the last AI request changed on the built-in devices (design 15e, `18 Interaction
//! Spec` rule "AI change"): the params of every built-in are snapshotted when a request
//! starts and diffed when it ends. View state only: the rings and the summary card read
//! it; Keep, undoing the AI step or a new request clear it, and touching a changed param
//! keeps that change and drops it from the set.

use std::collections::{BTreeMap, HashMap};

use cadenza_core::DeviceId;
use cadenza_model::{Device, Project};

use super::device;
use super::device_ab;

/// Effective params (defaults filled in) of every built-in device, by device.
pub type Snapshot = HashMap<DeviceId, BTreeMap<String, f32>>;

/// One param the AI moved.
#[derive(Debug, Clone, PartialEq)]
pub struct Change {
	pub param: String,
	pub before: f32,
	pub after: f32,
}

/// The changes of one finished AI request that edited the project.
#[derive(Debug, Clone, PartialEq)]
pub struct AiChanges {
	/// The undo step ("AI: make it brighter").
	pub label: String,
	/// What the user asked, for the card.
	pub request: String,
	/// Undo history length right after the AI step: shorter = the step was undone.
	pub depth: usize,
	pub devices: BTreeMap<DeviceId, Vec<Change>>,
}

fn differs(a: f32, b: f32) -> bool {
	(a - b).abs() > 1e-4 * a.abs().max(1.0)
}

/// A built-in's params with the registry defaults for the ones not stored.
pub fn effective_params(d: &Device) -> Option<BTreeMap<String, f32>> {
	let info = cadenza_dsp::device_info(d.builtin_id()?)?;
	Some(
		info.params
			.iter()
			.map(|p| {
				(
					p.id.to_owned(),
					d.params.get(p.id).copied().unwrap_or(p.default),
				)
			})
			.collect(),
	)
}

pub fn snapshot(project: &Project) -> Snapshot {
	device_ab::devices(project)
		.into_iter()
		.filter_map(|(id, d)| Some((id, effective_params(d)?)))
		.collect()
}

/// The params that differ between `before` and `project`, per device (registry order).
/// A built-in that did not exist before is compared with its defaults.
pub fn diff(before: &Snapshot, project: &Project) -> BTreeMap<DeviceId, Vec<Change>> {
	let mut out = BTreeMap::new();
	for (id, d) in device_ab::devices(project) {
		let Some(info) = d.builtin_id().and_then(cadenza_dsp::device_info) else {
			continue;
		};
		let Some(now) = effective_params(d) else {
			continue;
		};
		let old = before.get(&id);
		let changes: Vec<Change> = info
			.params
			.iter()
			.filter_map(|p| {
				let after = now[p.id];
				let before = old.and_then(|o| o.get(p.id).copied()).unwrap_or(p.default);
				differs(before, after).then(|| Change {
					param: p.id.to_owned(),
					before,
					after,
				})
			})
			.collect();
		if !changes.is_empty() {
			out.insert(id, changes);
		}
	}
	out
}

impl AiChanges {
	/// The changes of a request that ended with `depth` undo steps, or None when it moved
	/// no built-in param.
	pub fn new(
		label: &str,
		request: &str,
		depth: usize,
		before: &Snapshot,
		project: &Project,
	) -> Option<Self> {
		let devices = diff(before, project);
		(!devices.is_empty()).then(|| Self {
			label: label.to_owned(),
			request: request.to_owned(),
			depth,
			devices,
		})
	}

	pub fn is_empty(&self) -> bool {
		self.devices.is_empty()
	}

	pub fn of(&self, device: DeviceId) -> &[Change] {
		self.devices.get(&device).map_or(&[], Vec::as_slice)
	}

	pub fn change(&self, device: DeviceId, param: &str) -> Option<&Change> {
		self.of(device).iter().find(|c| c.param == param)
	}

	/// The user touched `param`: that change is kept and leaves the set.
	pub fn touch(&mut self, device: DeviceId, param: &str) -> bool {
		let Some(list) = self.devices.get_mut(&device) else {
			return false;
		};
		let n = list.len();
		list.retain(|c| c.param != param);
		let touched = list.len() != n;
		if list.is_empty() {
			self.devices.remove(&device);
		}
		touched
	}

	/// Drops every change whose param differs between `before` and `after` (a later
	/// user step moved it).
	pub fn touch_changed(&mut self, before: &Project, after: &Project) {
		let (b, a) = (snapshot(before), snapshot(after));
		let moved: Vec<(DeviceId, String)> = self
			.devices
			.iter()
			.flat_map(|(id, list)| list.iter().map(move |c| (*id, c.param.clone())))
			.filter(|(id, p)| {
				let get = |s: &Snapshot| s.get(id).and_then(|m| m.get(p)).copied();
				match (get(&b), get(&a)) {
					(Some(x), Some(y)) => differs(x, y),
					(x, y) => x != y,
				}
			})
			.collect();
		for (id, p) in moved {
			self.touch(id, &p);
		}
	}

	/// Whether the AI step is still the last undo step (Undo may take it back).
	pub fn is_last_step(&self, undo: &[String]) -> bool {
		undo.len() == self.depth && undo.last() == Some(&self.label)
	}

	/// Whether the AI step was undone (the history is shorter than right after it).
	pub fn undone(&self, undo: &[String]) -> bool {
		undo.len() < self.depth || undo.get(self.depth.wrapping_sub(1)) != Some(&self.label)
	}

	/// Total changed params.
	pub fn count(&self) -> usize {
		self.devices.values().map(Vec::len).sum()
	}
}

/// The delta tag of a knob ("+3.0 dB", "+0.3 oct", "→ 4.0:1").
pub fn delta_text(before: f32, after: f32, unit: &str) -> String {
	match unit {
		"dB" | "%" | "cents" | "semitones" | "ms" | "s" => {
			let d = after - before;
			let text = device::format_value(d, unit);
			if d > 0.0 && !text.starts_with('+') {
				format!("+{text}")
			} else {
				text
			}
		}
		"Hz" if before > 0.0 && after > 0.0 => format!("{:+.1} oct", (after / before).log2()),
		_ => format!("→ {}", device::format_value(after, unit)),
	}
}

/// Suggestions of the device editor's Ask AI popover for a built-in id.
pub fn suggestions(device_id: &str) -> &'static [&'static str] {
	match device_id {
		"eq" => &[
			"Make it brighter",
			"Clean up the low end",
			"Remove boxiness",
			"Tame harshness",
		],
		"compressor" => &[
			"Glue it more",
			"Less pumping",
			"More punch",
			"Gentler, more transparent",
		],
		"limiter" => &["Louder, keep it clean", "Hit -14 LUFS", "Less squashed"],
		"reverb" => &["Smaller room", "Longer, darker tail", "Less muddy"],
		"delay" => &["Sync to 1/8 dotted", "Fewer repeats", "Wider ping-pong"],
		"chorus" => &["Subtler", "Wider", "Slower, lusher"],
		"saturator" => &["Warmer", "More grit", "Softer"],
		"gain" => &["Make it mono", "Narrower", "Lower by 3 dB"],
		"poly_synth" => &["Brighter pad", "Shorter pluck", "Fatter, more detune"],
		"electric_piano" => &["Mellower", "More bell", "Add tremolo"],
		"drum_machine" => &["Punchier kick", "Tighter hats", "Lower the snare"],
		"sampler" | "drum_sampler" => &["Shorter release", "Brighter", "Less velocity range"],
		_ => GENERIC,
	}
}

/// Suggestions that fit any device.
pub const GENERIC: &[&str] = &[
	"Make it subtler",
	"Make it stronger",
	"Explain these settings",
];

#[cfg(test)]
mod tests {
	use super::*;
	use cadenza_core::{DeviceId, ParamAddress, TrackId};
	use cadenza_model::{Command, Document, Track, TrackKind};

	fn eq_project() -> (Document, TrackId, DeviceId) {
		let mut doc = Document::new(Project::new("t"));
		let t = Track::new("Vox", TrackKind::Audio);
		let track = t.id;
		let d = Device::builtin("eq", "EQ");
		let eq = d.id;
		doc.execute(Command::AddTrack {
			track: t,
			index: None,
		})
		.unwrap();
		doc.execute(Command::InsertDevice {
			track: Some(track),
			device: d,
			index: None,
		})
		.unwrap();
		(doc, track, eq)
	}

	fn set(doc: &mut Document, track: TrackId, eq: DeviceId, param: &str, value: f32) {
		doc.execute(Command::SetParam {
			address: ParamAddress::device(Some(track), eq, param),
			value,
		})
		.unwrap();
	}

	fn labels(doc: &Document) -> Vec<String> {
		doc.history_labels()
			.into_iter()
			.map(str::to_owned)
			.collect()
	}

	#[test]
	fn the_diff_lists_moved_params_with_defaults_filled_in() {
		let (mut doc, track, eq) = eq_project();
		set(&mut doc, track, eq, "peak1_gain", -3.5);
		let before = snapshot(doc.project());
		assert_eq!(before[&eq]["high_shelf_gain"], 0.0, "default filled in");
		set(&mut doc, track, eq, "high_shelf_gain", 3.0);
		set(&mut doc, track, eq, "peak1_gain", -5.0);
		set(&mut doc, track, eq, "peak2_gain", 0.0);
		let d = diff(&before, doc.project());
		let list = &d[&eq];
		assert_eq!(list.len(), 2, "a value set to what it was is no change");
		assert!(list.contains(&Change {
			param: "peak1_gain".into(),
			before: -3.5,
			after: -5.0,
		}));
		assert!(
			list.iter()
				.any(|c| c.param == "high_shelf_gain" && c.before == 0.0)
		);
		assert!(diff(&snapshot(doc.project()), doc.project()).is_empty());
	}

	#[test]
	fn a_new_device_is_compared_with_its_defaults() {
		let (mut doc, track, _) = eq_project();
		let before = snapshot(doc.project());
		let mut d = Device::builtin("compressor", "Compressor");
		let comp = d.id;
		d.params.insert("ratio".into(), 8.0);
		doc.execute(Command::InsertDevice {
			track: Some(track),
			device: d,
			index: None,
		})
		.unwrap();
		let changes = diff(&before, doc.project());
		assert_eq!(changes.len(), 1);
		assert_eq!(changes[&comp][0].param, "ratio");
	}

	#[test]
	fn clearing_rules_follow_the_history_and_touches() {
		let (mut doc, track, eq) = eq_project();
		let before = snapshot(doc.project());
		doc.begin_group("AI: brighter");
		set(&mut doc, track, eq, "high_shelf_gain", 3.0);
		set(&mut doc, track, eq, "peak1_gain", -2.0);
		doc.end_group();
		let mut c = AiChanges::new(
			"AI: brighter",
			"brighter",
			labels(&doc).len(),
			&before,
			doc.project(),
		)
		.unwrap();
		assert_eq!(c.count(), 2);
		assert!(c.is_last_step(&labels(&doc)));
		assert!(!c.undone(&labels(&doc)));

		// A later user step: no longer the last step, not undone; it touched peak1_gain.
		let p0 = doc.project().clone();
		set(&mut doc, track, eq, "peak1_gain", -1.0);
		c.touch_changed(&p0, doc.project());
		assert!(
			c.change(eq, "peak1_gain").is_none(),
			"touch keeps and clears"
		);
		assert!(c.change(eq, "high_shelf_gain").is_some());
		assert!(!c.is_last_step(&labels(&doc)));
		assert!(!c.undone(&labels(&doc)));
		doc.undo().unwrap();
		assert!(c.is_last_step(&labels(&doc)));
		doc.undo().unwrap();
		assert!(c.undone(&labels(&doc)), "the AI step was undone");

		assert!(c.touch(eq, "high_shelf_gain"));
		assert!(c.is_empty());
		assert!(!c.touch(eq, "high_shelf_gain"));
	}

	#[test]
	fn no_change_means_no_card() {
		let (doc, ..) = eq_project();
		let before = snapshot(doc.project());
		assert!(AiChanges::new("AI: x", "x", 2, &before, doc.project()).is_none());
	}

	#[test]
	fn delta_tags_read_in_the_param_units() {
		assert_eq!(delta_text(3.0, 6.0, "dB"), "+3.0 dB");
		assert_eq!(delta_text(-3.5, -5.0, "dB"), "-1.5 dB");
		assert_eq!(delta_text(10_000.0, 8000.0, "Hz"), "-0.3 oct");
		assert_eq!(delta_text(2.0, 4.0, "ratio"), "→ 4.0:1");
		assert_eq!(delta_text(10.0, 25.0, "%"), "+15 %");
		assert!(!suggestions("eq").is_empty());
		assert_eq!(suggestions("nope"), GENERIC);
	}
}
