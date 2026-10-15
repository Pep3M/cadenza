//! Freeze bookkeeping: a fingerprint of what a track's freeze render was made from, so a
//! frozen track edited afterwards can be flagged as stale (and refrozen).

use cadenza_core::{ParamTarget, TrackId};
use serde::Serialize;

use crate::project::{AutomationLane, ClipContent, Device, Project, Track, TrackKind};

/// What the render of a track depends on: its clips, devices (instrument + inserts), the
/// device-parameter lanes it keeps and the tempo. The fader, pan, sends and mute apply after
/// the render, so they are left out. File paths count by file name only, so moving the
/// project folder does not make a render stale.
#[derive(Serialize)]
struct FreezeSource<'a> {
	kind: TrackKind,
	instrument: Option<Device>,
	inserts: Vec<Device>,
	clips: Vec<crate::project::Clip>,
	lanes: Vec<&'a AutomationLane>,
	tempo: &'a cadenza_core::TempoMap,
}

fn file_name_only(path: &mut std::path::PathBuf) {
	if let Some(name) = path.file_name() {
		*path = name.into();
	}
}

fn portable(device: &Device) -> Device {
	let mut d = device.clone();
	for z in &mut d.zones {
		file_name_only(&mut z.sample.path);
	}
	d
}

/// FNV-1a, 64 bits: stable across builds and platforms (unlike `std`'s hasher).
fn fnv1a(bytes: &[u8]) -> u64 {
	bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, b| {
		(h ^ u64::from(*b)).wrapping_mul(0x0000_0100_0000_01b3)
	})
}

impl Project {
	/// Fingerprint of what a freeze render of `track` would be made from (`None`: no such
	/// track). Stored in [`crate::Freeze::source`] when freezing.
	pub fn freeze_source(&self, track: TrackId) -> Option<String> {
		let t = self.track(track)?;
		Some(format!("{:016x}", fnv1a(&source_bytes(self, t))))
	}

	/// A frozen track whose clips, devices, device lanes or tempo changed since it was
	/// frozen. Renders made before fingerprints existed are never stale.
	pub fn freeze_is_stale(&self, track: TrackId) -> bool {
		let Some(t) = self.track(track) else {
			return false;
		};
		match t.freeze.as_ref().and_then(|f| f.source.as_deref()) {
			Some(source) => self.freeze_source(track).as_deref() != Some(source),
			None => false,
		}
	}
}

fn source_bytes(project: &Project, t: &Track) -> Vec<u8> {
	let devices: Vec<_> = t
		.instrument
		.iter()
		.chain(&t.channel.inserts)
		.map(|d| d.id)
		.collect();
	let lanes = project
		.automation
		.iter()
		.filter(|l| {
			l.address.track == Some(t.id)
				&& matches!(&l.address.target, ParamTarget::Device { device, .. } if devices.contains(device))
		})
		.collect();
	let clips = t
		.clips
		.iter()
		.map(|c| {
			let mut c = c.clone();
			if let ClipContent::Audio { file, .. } = &mut c.content {
				file_name_only(file);
			}
			c
		})
		.collect();
	let source = FreezeSource {
		kind: t.kind,
		instrument: t.instrument.as_ref().map(portable),
		inserts: t.channel.inserts.iter().map(portable).collect(),
		clips,
		lanes,
		tempo: &project.tempo,
	};
	serde_json::to_vec(&source).unwrap_or_default()
}

#[cfg(test)]
mod tests {
	use cadenza_core::{ParamAddress, Tick};

	use crate::project::{Clip, Freeze, Note};
	use crate::{ChannelPatch, Command, Document};

	use super::*;

	fn frozen() -> (Document, TrackId) {
		let mut p = Project::new("x");
		let mut t = Track::new("Keys", TrackKind::Instrument);
		t.instrument = Some(Device::builtin("poly_synth", "Poly Synth"));
		let mut clip = Clip::midi("c", Tick(0), Tick(3840));
		if let ClipContent::Midi { notes, .. } = &mut clip.content {
			notes.push(Note::new(60, Tick(0), Tick(960), 0.8));
		}
		t.clips.push(clip);
		let id = t.id;
		p.tracks.push(t);
		let source = p.freeze_source(id);
		let mut doc = Document::new(p);
		doc.execute(Command::SetTrackFreeze {
			track: id,
			freeze: Some(Freeze {
				file: "/p/.cadenza/freeze/Keys 1.wav".into(),
				source,
			}),
		})
		.unwrap();
		(doc, id)
	}

	#[test]
	fn edits_after_freezing_make_the_render_stale_but_mixing_does_not() {
		let (mut doc, id) = frozen();
		assert!(!doc.project().freeze_is_stale(id));
		doc.execute(Command::SetChannel {
			track: Some(id),
			patch: ChannelPatch {
				volume_db: Some(-6.0),
				pan: Some(0.3),
				..ChannelPatch::default()
			},
		})
		.unwrap();
		assert!(
			!doc.project().freeze_is_stale(id),
			"the fader is after the render"
		);
		let clip = doc.project().tracks[0].clips[0].id;
		doc.execute(Command::AddNotes {
			clip,
			notes: vec![Note::new(64, Tick(960), Tick(960), 0.8)],
		})
		.unwrap();
		assert!(doc.project().freeze_is_stale(id));
		doc.undo().unwrap();
		assert!(
			!doc.project().freeze_is_stale(id),
			"undo makes it current again"
		);
		let synth = doc.project().tracks[0].instrument.as_ref().unwrap().id;
		doc.execute(Command::SetParam {
			address: ParamAddress::device(Some(id), synth, "cutoff"),
			value: 1200.0,
		})
		.unwrap();
		assert!(doc.project().freeze_is_stale(id));
	}

	#[test]
	fn renders_without_a_fingerprint_are_never_stale_and_moving_the_folder_keeps_it() {
		let (doc, id) = frozen();
		let mut p = doc.project().clone();
		p.tracks[0].freeze.as_mut().unwrap().source = None;
		p.tracks[0].clips[0].name = "edited".into();
		assert!(!p.freeze_is_stale(id));
		// Same audio file name in another folder: same fingerprint.
		let mut a = Track::new("Vox", TrackKind::Audio);
		a.clips
			.push(Clip::audio("v", Tick(0), Tick(960), "/old/Audio/v.wav"));
		let aid = a.id;
		p.tracks.push(a);
		let before = p.freeze_source(aid);
		let ClipContent::Audio { file, .. } = &mut p.tracks[1].clips[0].content else {
			panic!()
		};
		*file = "/new/Audio/v.wav".into();
		assert_eq!(p.freeze_source(aid), before);
	}
}
