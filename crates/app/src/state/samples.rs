//! The instrument editor of the built-in samplers (`sampler`, `drum_sampler`), without
//! egui: which device it shows, the selected zone or pad, and the pure gesture ->
//! `Command` builders (one command per gesture; pad outputs get their route in the same
//! batch so a pad sent to an extra output stays audible).

use std::path::Path;

use cadenza_core::TrackId;
use cadenza_dsp::instruments::drum_sampler::{AUX_OUTPUTS, PADS};
use cadenza_model::{Command, Device, OutputRoute, Project, SampleZone};

use super::selection::DeviceRef;

/// Which kind of sample instrument a device is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleKind {
	Sampler,
	Drums,
}

/// The sample instrument kind of `device`, if it is one.
pub fn kind(device: &Device) -> Option<SampleKind> {
	match device.builtin_id()? {
		"sampler" => Some(SampleKind::Sampler),
		"drum_sampler" => Some(SampleKind::Drums),
		_ => None,
	}
}

/// View state of the device editor (not part of the project): its device and the
/// sampler / drum sampler selection.
#[derive(Debug, Clone, PartialEq)]
pub struct InstrumentView {
	/// The device shown.
	pub device: Option<DeviceRef>,
	/// Selected sampler zone (index).
	pub zone: usize,
	/// Selected drum pad (note).
	pub pad: u8,
	/// Selected layer of the drum pad (chip order, `drum_sampler_editor::pad_layers`).
	pub layer: usize,
	/// The pad (note; 0 = the whole kit) a sample / folder dialog is open for.
	pub dialog: Option<(DeviceRef, u8)>,
	/// The browser should show its Files tab (a pad's "Locate file").
	pub files_tab: bool,
	/// The sample file loaded into a sampler last (a double-click on an empty spot of
	/// the zone map makes a zone of it).
	pub last_sample: Option<std::path::PathBuf>,
}

impl Default for InstrumentView {
	fn default() -> Self {
		Self {
			device: None,
			zone: 0,
			pad: PADS[0].0,
			layer: 0,
			dialog: None,
			files_tab: false,
			last_sample: None,
		}
	}
}

impl InstrumentView {
	/// Forgets a device that no longer exists (or is no longer a built-in).
	pub fn prune(&mut self, project: &Project) {
		if let Some(r) = self.device {
			let alive = device(project, r).is_some_and(|d| d.builtin_id().is_some());
			if !alive {
				self.device = None;
			}
		}
	}
}

/// The device at `r` in `project`.
pub fn device(project: &Project, r: DeviceRef) -> Option<&Device> {
	match r.track {
		Some(t) => project.track(t)?.device(r.device),
		None => project.master.inserts.iter().find(|d| d.id == r.device),
	}
}

/// Indices of the zones of pad `note` (its layers).
pub fn pad_zones(device: &Device, note: u8) -> Vec<usize> {
	device
		.zones
		.iter()
		.enumerate()
		.filter(|(_, z)| z.is_single_key() && z.key_low == note)
		.map(|(i, _)| i)
		.collect()
}

/// Display name of pad `note`: the GM name, else the note name.
pub fn pad_label(note: u8) -> String {
	cadenza_dsp::instruments::drum_sampler::pad_name(note)
		.map_or_else(|| cadenza_model::note_name(note), str::to_owned)
}

/// Replaces `r`'s zones with `zones` (one command), plus a route for each extra output
/// used without one (instrument only).
pub fn set_zones(
	project: &Project,
	r: DeviceRef,
	zones: Vec<SampleZone>,
	label: &str,
) -> Option<Command> {
	let d = device(project, r)?;
	if d.zones == zones {
		return None;
	}
	let set = Command::SetSampleZones {
		track: r.track,
		device: r.device,
		zones: zones.clone(),
	};
	let instrument_track: Option<TrackId> = r.track.filter(|t| {
		project
			.track(*t)
			.and_then(|t| t.instrument.as_ref())
			.is_some_and(|i| i.id == r.device)
	});
	let mut routes = d.aux_outputs.clone();
	if let Some(track) = instrument_track {
		for port in zones.iter().map(|z| u32::from(z.output)) {
			if port > 0 && port as usize <= AUX_OUTPUTS && !routes.iter().any(|x| x.port == port) {
				routes.push(OutputRoute { port, bus: None });
			}
		}
		if routes != d.aux_outputs {
			routes.sort_by_key(|x| x.port);
			return Some(Command::Batch {
				label: label.to_owned(),
				commands: vec![set, Command::SetOutputRoutes { track, routes }],
			});
		}
	}
	Some(set)
}

/// Edits zone `index` with `f`.
pub fn edit_zone(
	project: &Project,
	r: DeviceRef,
	index: usize,
	f: impl FnOnce(&mut SampleZone),
) -> Option<Command> {
	let mut zones = device(project, r)?.zones.clone();
	f(zones.get_mut(index)?);
	set_zones(project, r, zones, "Edit sample zone")
}

/// Edits every layer of pad `note` with `f`.
pub fn edit_pad(
	project: &Project,
	r: DeviceRef,
	note: u8,
	f: impl Fn(&mut SampleZone),
) -> Option<Command> {
	let d = device(project, r)?;
	let layers = pad_zones(d, note);
	if layers.is_empty() {
		return None;
	}
	let mut zones = d.zones.clone();
	for i in layers {
		f(&mut zones[i]);
	}
	set_zones(project, r, zones, "Edit drum pad")
}

/// Removes zone `index`.
pub fn remove_zone(project: &Project, r: DeviceRef, index: usize) -> Option<Command> {
	let mut zones = device(project, r)?.zones.clone();
	(index < zones.len()).then(|| zones.remove(index))?;
	set_zones(project, r, zones, "Remove sample zone")
}

/// Unloads pad `note` (every layer).
pub fn clear_pad(project: &Project, r: DeviceRef, note: u8) -> Option<Command> {
	let mut zones = device(project, r)?.zones.clone();
	let before = zones.len();
	zones.retain(|z| !(z.is_single_key() && z.key_low == note));
	(zones.len() != before).then_some(())?;
	set_zones(project, r, zones, "Clear drum pad")
}

/// Where a dropped sample goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleSlot {
	/// A drum pad (replaces its sample, keeping its settings).
	Pad(u8),
	/// A sampler zone (replaces its sample, keeping its mapping).
	Zone(usize),
	/// The whole keyboard at the sample's root (replaces every zone).
	Keyboard,
	/// A new sampler zone filling the empty spot of the zone map at this key and
	/// velocity (0..=127).
	Area { key: u8, vel: u8 },
}

/// The arguments of the shared MCP `load_sample` tool for a drop.
pub fn load_args(
	track: TrackId,
	device: DeviceRef,
	path: &Path,
	slot: SampleSlot,
) -> cadenza_mcp::tools::samples::LoadSampleArgs {
	let (pad, zone) = match slot {
		SampleSlot::Pad(n) => (Some(n.to_string()), None),
		SampleSlot::Zone(i) => (None, Some(i)),
		SampleSlot::Keyboard | SampleSlot::Area { .. } => (None, None),
	};
	let keys = match slot {
		SampleSlot::Area { key, .. } => Some(key.to_string()),
		_ => None,
	};
	cadenza_mcp::tools::samples::LoadSampleArgs {
		track_id: track,
		device_id: Some(device.device),
		path: path.display().to_string(),
		pad,
		layer: false,
		zone,
		keys,
		root: None,
	}
}

/// Seconds of a marker drag clamped into the sample (`length` seconds) and kept in
/// order with the other markers of the zone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Marker {
	Start,
	End,
	LoopStart,
	LoopEnd,
}

impl Marker {
	pub fn label(self) -> &'static str {
		match self {
			Marker::Start => "S",
			Marker::End => "E",
			Marker::LoopStart => "L",
			Marker::LoopEnd => "L",
		}
	}

	/// Current position of the marker in seconds (`length` = sample duration).
	pub fn get(self, z: &SampleZone, length: f64) -> f64 {
		match self {
			Marker::Start => z.start,
			Marker::End => z.end.unwrap_or(length),
			Marker::LoopStart => z.loop_start.max(z.start),
			Marker::LoopEnd => z.loop_end.unwrap_or_else(|| z.end.unwrap_or(length)),
		}
	}

	/// Moves the marker to `seconds`, clamped between its neighbours (at least 1 ms
	/// apart). Markers at the sample end are stored as "to the end" (`None`).
	pub fn set(self, z: &mut SampleZone, seconds: f64, length: f64) {
		const GAP: f64 = 0.001;
		let end = z.end.unwrap_or(length);
		let s = seconds.clamp(0.0, length);
		match self {
			Marker::Start => z.start = s.min(end - GAP).max(0.0),
			Marker::End => {
				let e = s.max(z.start + GAP);
				z.end = (e < length - 1e-6).then_some(e);
			}
			Marker::LoopStart => {
				let le = z.loop_end.unwrap_or(end);
				z.loop_start = s.clamp(z.start, (le - GAP).max(z.start));
			}
			Marker::LoopEnd => {
				let e = s.clamp(z.loop_start + GAP, end);
				z.loop_end = (e < end - 1e-6).then_some(e);
			}
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use cadenza_model::{Document, LoopMode, Track, TrackKind};

	fn fixture() -> (Project, DeviceRef) {
		let mut p = Project::new("s");
		let d = Device::builtin("drum_sampler", "Drums").with_zones(vec![
			SampleZone::pad("/k/Kick.wav", 36),
			SampleZone::pad("/k/Snare.wav", 38),
			SampleZone::pad("/k/Snare soft.wav", 38),
		]);
		let r = DeviceRef {
			track: None,
			device: d.id,
		};
		let t = Track::new("Drums", TrackKind::Instrument).with_instrument(d);
		let r = DeviceRef {
			track: Some(t.id),
			..r
		};
		p.tracks.push(t);
		(p, r)
	}

	fn run(p: &Project, cmd: Command) -> Project {
		let mut doc = Document::new(p.clone());
		doc.execute(cmd).unwrap();
		assert_eq!(doc.history_labels().len(), 1, "one undo step");
		doc.project().clone()
	}

	#[test]
	fn pad_edits_reach_every_layer_and_outputs_get_routes() {
		let (p, r) = fixture();
		let d = device(&p, r).unwrap();
		assert_eq!(pad_zones(d, 38), vec![1, 2]);
		assert_eq!(pad_label(38), "Snare");
		assert_eq!(pad_label(60), "Hi Bongo");
		assert_eq!(pad_label(84), "C6");
		let cmd = edit_pad(&p, r, 38, |z| {
			z.gain_db = -6.0;
			z.output = 2;
		})
		.unwrap();
		assert!(matches!(&cmd, Command::Batch { label, .. } if label == "Edit drum pad"));
		let after = run(&p, cmd);
		let d = device(&after, r).unwrap();
		assert!(
			d.zones[1..]
				.iter()
				.all(|z| z.gain_db == -6.0 && z.output == 2)
		);
		assert_eq!(d.zones[0].gain_db, 0.0);
		assert_eq!(d.aux_outputs, vec![OutputRoute { port: 2, bus: None }]);
		// A second edit on the routed output needs no new route.
		let cmd = edit_pad(&after, r, 38, |z| z.pan = 0.5).unwrap();
		assert!(matches!(cmd, Command::SetSampleZones { .. }));
		// No change, no command; unknown pads neither.
		assert!(edit_pad(&after, r, 38, |z| z.gain_db = -6.0).is_none());
		assert!(edit_pad(&after, r, 50, |z| z.pan = 0.5).is_none());
		let after = run(&after, clear_pad(&after, r, 38).unwrap());
		assert_eq!(device(&after, r).unwrap().zones.len(), 1);
		assert!(clear_pad(&after, r, 38).is_none());
	}

	#[test]
	fn markers_stay_ordered_inside_the_sample() {
		let mut z = SampleZone::full_keyboard("/a.wav", 60);
		z.loop_mode = LoopMode::Forward;
		Marker::Start.set(&mut z, 0.5, 2.0);
		Marker::End.set(&mut z, 0.2, 2.0);
		assert!((z.end.unwrap() - 0.501).abs() < 1e-9);
		Marker::End.set(&mut z, 5.0, 2.0);
		assert_eq!(z.end, None, "the file end is stored as 'to the end'");
		Marker::LoopStart.set(&mut z, 0.1, 2.0);
		assert_eq!(z.loop_start, 0.5, "the loop starts after the start marker");
		Marker::LoopEnd.set(&mut z, 1.5, 2.0);
		assert_eq!(z.loop_end, Some(1.5));
		Marker::LoopStart.set(&mut z, 1.9, 2.0);
		assert!((z.loop_start - 1.499).abs() < 1e-9);
		assert_eq!(Marker::LoopEnd.get(&z, 2.0), 1.5);
		assert_eq!(Marker::End.get(&z, 2.0), 2.0);
	}
}
