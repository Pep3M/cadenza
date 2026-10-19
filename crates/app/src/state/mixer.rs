//! Mixer view options (strip width, visible sections; per user, not in the project) and
//! the routing builders of its output menu.

use cadenza_core::TrackId;
use cadenza_model::{ChannelPatch, Command, Output, Project, TrackKind};
use serde::{Deserialize, Serialize};

use super::edits;

/// What the mixer shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MixerView {
	/// Narrow strips (more channels on screen; names and inserts elided).
	pub narrow: bool,
	pub input: bool,
	pub inserts: bool,
	pub sends: bool,
	/// The output selector.
	pub routing: bool,
}

impl Default for MixerView {
	fn default() -> Self {
		Self {
			narrow: false,
			input: true,
			inserts: true,
			sends: true,
			routing: true,
		}
	}
}

/// "New bus from this track": a bus named after the track, right below it, with the
/// track's output routed to it — one undo step. `None` for buses, folders and unknown ids.
pub fn new_bus_from(project: &Project, track: TrackId) -> Option<(Command, TrackId)> {
	let t = project.track(track)?;
	if matches!(t.kind, TrackKind::Bus | TrackKind::Folder) {
		return None;
	}
	let mut bus = edits::new_track(project, TrackKind::Bus);
	bus.name = format!("{} Bus", t.name);
	bus.color = t.color.clone();
	// The bus goes where the track went before.
	bus.channel.output = t.channel.output;
	let id = bus.id;
	let commands = vec![
		edits::add_track(project, bus, Some(track)),
		Command::SetChannel {
			track: Some(track),
			patch: ChannelPatch {
				output: Some(Output::Bus(id)),
				..ChannelPatch::default()
			},
		},
	];
	Some((
		Command::Batch {
			label: format!("New bus from '{}'", t.name),
			commands,
		},
		id,
	))
}

#[cfg(test)]
mod tests {
	use cadenza_model::{Document, Track};

	use super::*;

	#[test]
	fn a_new_bus_takes_over_the_track_output() {
		let mut p = Project::new("Mix");
		let mut drums = Track::new("Drums", TrackKind::Instrument).with_color("#E8705F");
		let id = drums.id;
		let group = Track::new("Group", TrackKind::Bus);
		drums.channel.output = Output::Bus(group.id);
		let group_id = group.id;
		p.tracks.push(drums);
		p.tracks.push(group);
		let (cmd, bus) = new_bus_from(&p, id).unwrap();
		let mut doc = Document::new(p);
		doc.execute(cmd).unwrap();
		let p = doc.project();
		let b = p.track(bus).unwrap();
		assert_eq!(b.name, "Drums Bus");
		assert_eq!(b.kind, TrackKind::Bus);
		assert_eq!(p.track_index(bus), Some(1), "right below the track");
		assert_eq!(
			b.channel.output,
			Output::Bus(group_id),
			"keeps the old route"
		);
		assert_eq!(p.track(id).unwrap().channel.output, Output::Bus(bus));
		assert_eq!(doc.history_labels().len(), 1);
		assert!(new_bus_from(p, bus).is_none(), "not from a bus");
	}
}
