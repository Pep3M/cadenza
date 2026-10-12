//! What the user has selected. Ids only, so selections survive edits and undo; stale ids
//! are pruned after every change.

use std::collections::BTreeSet;

use cadenza_core::{ClipId, DeviceId, TrackId};
use cadenza_model::Project;

/// A device in a channel chain (`track: None` = master) or a track's instrument.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceRef {
	pub track: Option<TrackId>,
	pub device: DeviceId,
}

/// What the inspector shows, most specific first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
	Device(DeviceRef),
	Clip(ClipId),
	Track(TrackId),
	Master,
	Nothing,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Selection {
	/// The current track (target of browser actions, inspector).
	pub track: Option<TrackId>,
	/// Selected clips (any tracks).
	pub clips: BTreeSet<ClipId>,
	pub device: Option<DeviceRef>,
	/// The master strip is selected (no track).
	pub master: bool,
}

impl Selection {
	pub fn select_track(&mut self, track: TrackId) {
		self.track = Some(track);
		self.master = false;
		if self.device.is_some_and(|d| d.track != Some(track)) {
			self.device = None;
		}
	}

	pub fn select_master(&mut self) {
		self.track = None;
		self.master = true;
		self.clips.clear();
		if self.device.is_some_and(|d| d.track.is_some()) {
			self.device = None;
		}
	}

	/// Selects one clip (and its track); with `add`, toggles it in the selection instead.
	pub fn click_clip(&mut self, track: TrackId, clip: ClipId, add: bool) {
		if add {
			if !self.clips.remove(&clip) {
				self.clips.insert(clip);
			}
		} else if !self.clips.contains(&clip) {
			self.clips.clear();
			self.clips.insert(clip);
		}
		self.select_track(track);
		self.device = None;
	}

	pub fn select_device(&mut self, device: DeviceRef) {
		match device.track {
			Some(t) => self.select_track(t),
			None => self.select_master(),
		}
		self.device = Some(device);
	}

	pub fn clear_clips(&mut self) {
		self.clips.clear();
	}

	pub fn focus(&self) -> Focus {
		if let Some(d) = self.device {
			return Focus::Device(d);
		}
		if self.clips.len() == 1 {
			return Focus::Clip(*self.clips.first().expect("one clip"));
		}
		match (self.track, self.master) {
			(Some(t), _) => Focus::Track(t),
			(None, true) => Focus::Master,
			(None, false) => Focus::Nothing,
		}
	}

	/// Drops ids that no longer exist in `project`.
	pub fn prune(&mut self, project: &Project) {
		if self.track.is_some_and(|t| project.track(t).is_none()) {
			self.track = None;
		}
		self.clips.retain(|c| project.find_clip(*c).is_some());
		if let Some(d) = self.device {
			let exists = match d.track {
				Some(t) => project
					.track(t)
					.is_some_and(|t| t.device(d.device).is_some()),
				None => project.master.inserts.iter().any(|x| x.id == d.device),
			};
			if !exists {
				self.device = None;
			}
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use cadenza_core::Tick;
	use cadenza_model::{Clip, Command, Device, Document, Track, TrackKind};

	#[test]
	fn clicks_select_and_toggle_clips() {
		let (t, a, b) = (TrackId::new(), ClipId::new(), ClipId::new());
		let mut s = Selection::default();
		s.click_clip(t, a, false);
		assert_eq!(s.focus(), Focus::Clip(a));
		s.click_clip(t, b, true);
		assert_eq!(s.clips.len(), 2);
		assert_eq!(s.focus(), Focus::Track(t), "several clips: the track");
		// Clicking an already selected clip keeps the group (start of a group drag).
		s.click_clip(t, a, false);
		assert_eq!(s.clips.len(), 2);
		s.click_clip(t, b, true);
		assert_eq!(s.focus(), Focus::Clip(a));
		s.select_master();
		assert_eq!(s.focus(), Focus::Master);
	}

	#[test]
	fn prune_drops_deleted_items() {
		let mut p = Project::new("p");
		let fx = Device::builtin("eq", "EQ");
		let mut track = Track::new("t", TrackKind::Instrument);
		track.channel.inserts.push(fx.clone());
		let clip = Clip::midi("c", Tick::ZERO, Tick(960));
		track.clips.push(clip.clone());
		let id = track.id;
		p.tracks.push(track);
		let mut doc = Document::new(p);
		let mut s = Selection::default();
		s.click_clip(id, clip.id, false);
		s.select_device(DeviceRef {
			track: Some(id),
			device: fx.id,
		});
		s.prune(doc.project());
		assert!(s.device.is_some() && s.clips.len() == 1);
		doc.execute(Command::RemoveTrack { track: id }).unwrap();
		s.prune(doc.project());
		assert_eq!(s, Selection::default());
	}
}
