//! Duplicating a track (FL/Ableton-style "Duplicate track"): a pure builder that clones a
//! track with fresh ids for everything it creates (the track, its instrument/inserts,
//! sends, clips and notes) and re-addresses the automation lanes that followed the
//! original onto the copy. Always one undo step (`Command::Batch`), even when there is
//! nothing to re-address, so the history always reads "Duplicate track '<name>'".
//!
//! Decisions (documented here, not just in code): the copy is independent of the
//! original's take groups and links — not dropped, but given fresh group/link ids, so
//! takes and FL pattern instances stay linked *among the copies* without also pulling in
//! (or being pulled into) the originals. Keeping `take` matters: `Clip::plays()` is "no
//! take, or the active one", so dropping it would make every inactive take lane of a
//! comped track audible on the copy. Keeping `link` (remapped) matters too: `AddClip`
//! only bounds a clip's notes by its own length when `link` is `None` — a trimmed linked
//! clip (notes reaching past its own, shorter length; the group's longest copy covers
//! them) would fail that check as an independent copy. Sidechain and MIDI-input sources on
//! copied devices keep pointing at their original tracks (only this track's own ids
//! change). Solo is turned off on the copy so duplicating a soloed track does not silence
//! everything else twice over. Freeze is dropped (the copy has no render to reuse). Folder
//! tracks are not duplicated yet (`can_duplicate` says so); duplicating one along with its
//! nested tracks is a larger feature.

use std::collections::HashMap;

use cadenza_core::{
	AudioNoteId, ClipId, DeviceId, LinkId, ModulatorId, NoteId, ParamAddress, ParamTarget,
	TakeGroupId, TrackId,
};
use cadenza_model::{
	Channel, Clip, ClipContent, Command, Device, NoteEdits, Project, Send, Track, TrackKind,
};

/// Whether `track` can be duplicated as a whole. Folders would also need their nested
/// tracks cloned and re-parented onto the copy, which this does not do; the menu disables
/// itself for them with a tooltip explaining why.
pub fn can_duplicate(track: &Track) -> bool {
	track.kind != TrackKind::Folder
}

/// A copy of `device` with a fresh id (and fresh modulator ids); records the id change so
/// callers can re-address automation and sends. Sidechain/MIDI-input sources are left as
/// they are: they point at other tracks, untouched by this duplicate.
fn copy_device(device: &Device, map: &mut HashMap<DeviceId, DeviceId>) -> Device {
	let mut copy = device.clone();
	let fresh = DeviceId::new();
	map.insert(device.id, fresh);
	copy.id = fresh;
	for m in &mut copy.modulators {
		m.id = ModulatorId::new();
	}
	copy
}

fn copy_send(send: &Send, map: &mut HashMap<DeviceId, DeviceId>) -> Send {
	let fresh = DeviceId::new();
	map.insert(send.id, fresh);
	Send {
		id: fresh,
		target: send.target,
		level_db: send.level_db,
		pre_fader: send.pre_fader,
	}
}

fn copy_note_edits(edits: &NoteEdits) -> NoteEdits {
	let mut copy = edits.clone();
	for note in &mut copy.notes {
		note.id = AudioNoteId::new();
	}
	copy
}

/// A copy of `clip`: fresh clip (and note) ids, same position and content otherwise. Its
/// take group and link (if any) are given fresh ids (shared with any other clip of this
/// duplicate carrying the same originals, via `groups`/`links`), so the copies stay
/// comped/linked among themselves, independent of the source track's groups.
fn copy_clip(
	clip: &Clip,
	groups: &mut HashMap<TakeGroupId, TakeGroupId>,
	links: &mut HashMap<LinkId, LinkId>,
) -> Clip {
	let mut copy = clip.clone();
	copy.id = ClipId::new();
	if let Some(take) = &mut copy.take {
		take.group = *groups.entry(take.group).or_default();
	}
	copy.link = clip.link.map(|l| *links.entry(l).or_default());
	match &mut copy.content {
		ClipContent::Midi { notes, .. } => {
			for n in notes {
				n.id = NoteId::new();
			}
		}
		ClipContent::Audio { notes, .. } => {
			if let Some(edits) = notes {
				**edits = copy_note_edits(edits);
			}
		}
	}
	copy
}

/// The automation target re-addressed to the copies in `map` (device and send ids);
/// `None` when a device/send target does not belong to this track (should not happen for
/// a lane already filtered by `address.track`).
fn remap_target(target: &ParamTarget, map: &HashMap<DeviceId, DeviceId>) -> Option<ParamTarget> {
	match target {
		ParamTarget::Volume => Some(ParamTarget::Volume),
		ParamTarget::Pan => Some(ParamTarget::Pan),
		ParamTarget::Device { device, param } => {
			map.get(device).map(|&device| ParamTarget::Device {
				device,
				param: param.clone(),
			})
		}
		ParamTarget::Send { send } => map.get(send).map(|&send| ParamTarget::Send { send }),
	}
}

/// Builds the "Duplicate track" command: the track itself (with fresh-id devices, sends
/// and clips) right below `track` in the same folder, plus the automation lanes that
/// addressed it or its devices/sends, re-addressed to the copies. `None` when `track`
/// does not exist or cannot be duplicated (see [`can_duplicate`]).
pub fn duplicate_track(project: &Project, track: TrackId) -> Option<(Command, TrackId)> {
	let source = project.track(track)?;
	if !can_duplicate(source) {
		return None;
	}
	let new_id = TrackId::new();
	let mut devices = HashMap::new();
	let instrument = source
		.instrument
		.as_ref()
		.map(|d| copy_device(d, &mut devices));
	let inserts: Vec<Device> = source
		.channel
		.inserts
		.iter()
		.map(|d| copy_device(d, &mut devices))
		.collect();
	let sends: Vec<Send> = source
		.channel
		.sends
		.iter()
		.map(|s| copy_send(s, &mut devices))
		.collect();
	let mut groups = HashMap::new();
	let mut links = HashMap::new();
	let clips: Vec<Clip> = source
		.clips
		.iter()
		.map(|c| copy_clip(c, &mut groups, &mut links))
		.collect();
	let copy = Track {
		id: new_id,
		name: format!("{} (copy)", source.name),
		kind: source.kind,
		color: source.color.clone(),
		instrument,
		clips,
		channel: Channel {
			volume_db: source.channel.volume_db,
			pan: source.channel.pan,
			mute: source.channel.mute,
			solo: false,
			inserts,
			sends,
			output: source.channel.output,
		},
		input: source.input,
		folder: source.folder,
		freeze: None,
	};
	let index = project.track_index(track).map(|i| i + 1);
	let mut commands = vec![Command::AddTrack { track: copy, index }];
	for lane in &project.automation {
		if lane.address.track != Some(track) {
			continue;
		}
		let Some(target) = remap_target(&lane.address.target, &devices) else {
			continue;
		};
		commands.push(Command::SetAutomation {
			address: ParamAddress {
				track: Some(new_id),
				target,
			},
			points: lane.points.clone(),
		});
	}
	Some((
		Command::Batch {
			label: format!("Duplicate track '{}'", source.name),
			commands,
		},
		new_id,
	))
}

#[cfg(test)]
mod tests {
	use super::*;
	use cadenza_core::{ParamAddress, Tick};
	use cadenza_model::{AutomationPoint, Document, Note, Output};

	struct Fx {
		doc: Document,
		track: TrackId,
		synth: DeviceId,
		eq: DeviceId,
		send: DeviceId,
	}

	fn fixture() -> Fx {
		let mut p = Project::new("t");
		let mut bus = Track::new("Bus", TrackKind::Bus);
		bus.channel.output = Output::Master;
		let bus_id = bus.id;
		p.tracks.push(bus);

		let mut track = Track::new("Keys", TrackKind::Instrument);
		track.color = Some("#ff0000".into());
		let mut synth = Device::builtin("poly_synth", "Poly Synth");
		synth.params.insert("cutoff".into(), 0.5);
		let synth_id = synth.id;
		track.instrument = Some(synth);
		let eq = Device::builtin("eq", "EQ");
		let eq_id = eq.id;
		track.channel.inserts.push(eq);
		let send_id = DeviceId::new();
		track.channel.sends.push(Send {
			id: send_id,
			target: bus_id,
			level_db: -6.0,
			pre_fader: false,
		});
		track.clips.push(
			Clip::midi("riff", Tick::ZERO, Tick(960)).with_notes([Note::new(
				60,
				Tick::ZERO,
				Tick(480),
				0.8,
			)]),
		);
		let track_id = track.id;
		p.tracks.push(track);

		let mut doc = Document::new(p);
		doc.execute(Command::SetAutomation {
			address: ParamAddress::track_volume(track_id),
			points: vec![AutomationPoint::new(Tick::ZERO, 0.0)],
		})
		.unwrap();
		doc.execute(Command::SetAutomation {
			address: ParamAddress::device(Some(track_id), synth_id, "cutoff"),
			points: vec![AutomationPoint::new(Tick::ZERO, 0.5)],
		})
		.unwrap();
		doc.execute(Command::SetAutomation {
			address: ParamAddress::send(track_id, send_id),
			points: vec![AutomationPoint::new(Tick::ZERO, -6.0)],
		})
		.unwrap();
		Fx {
			doc,
			track: track_id,
			synth: synth_id,
			eq: eq_id,
			send: send_id,
		}
	}

	#[test]
	fn fresh_ids_everywhere_and_devices_params_copied() {
		let fx = fixture();
		let (cmd, new_id) = duplicate_track(fx.doc.project(), fx.track).unwrap();
		assert_ne!(new_id, fx.track);
		let Command::Batch { commands, .. } = &cmd else {
			panic!("expected a batch, got {cmd:?}");
		};
		let Command::AddTrack { track, index } = &commands[0] else {
			panic!("expected AddTrack first, got {:?}", commands[0]);
		};
		assert_eq!(track.id, new_id);
		assert_eq!(track.name, "Keys (copy)");
		assert_eq!(track.color.as_deref(), Some("#ff0000"));
		// Placed right below the source (index 1: after the bus at 0).
		assert_eq!(*index, Some(2));

		let new_synth = track.instrument.as_ref().expect("instrument copied");
		assert_ne!(new_synth.id, fx.synth);
		assert_eq!(new_synth.params.get("cutoff"), Some(&0.5));

		assert_eq!(track.channel.inserts.len(), 1);
		let new_eq = &track.channel.inserts[0];
		assert_ne!(new_eq.id, fx.eq);

		assert_eq!(track.channel.sends.len(), 1);
		let new_send = &track.channel.sends[0];
		assert_ne!(new_send.id, fx.send);
		assert_eq!(new_send.level_db, -6.0);

		assert_eq!(track.clips.len(), 1);
		let source_clip = &fx.doc.project().track(fx.track).unwrap().clips[0];
		let new_clip = &track.clips[0];
		assert_ne!(new_clip.id, source_clip.id);
		assert_eq!(new_clip.start, source_clip.start);
		if let (ClipContent::Midi { notes: old, .. }, ClipContent::Midi { notes: new, .. }) =
			(&source_clip.content, &new_clip.content)
		{
			assert_eq!(old.len(), new.len());
			assert_ne!(old[0].id, new[0].id);
			assert_eq!(old[0].pitch, new[0].pitch);
		} else {
			panic!("expected MIDI content");
		}
	}

	#[test]
	fn automation_is_re_addressed_to_the_copies() {
		let fx = fixture();
		let (cmd, new_id) = duplicate_track(fx.doc.project(), fx.track).unwrap();
		let Command::Batch { commands, .. } = &cmd else {
			panic!("expected a batch");
		};
		let Command::AddTrack {
			track: new_track, ..
		} = &commands[0]
		else {
			panic!("expected AddTrack first");
		};
		let new_synth = new_track.instrument.as_ref().unwrap().id;
		let new_send = new_track.channel.sends[0].id;

		let automations: Vec<&ParamAddress> = commands[1..]
			.iter()
			.map(|c| match c {
				Command::SetAutomation { address, .. } => address,
				other => panic!("expected SetAutomation, got {other:?}"),
			})
			.collect();
		assert_eq!(automations.len(), 3, "volume, device param and send lanes");
		assert!(automations.iter().all(|a| a.track == Some(new_id)));
		assert!(automations.iter().any(|a| a.target == ParamTarget::Volume));
		assert!(automations.iter().any(|a| matches!(
			a.target,
			ParamTarget::Device { device, .. } if device == new_synth
		)));
		assert!(
			automations
				.iter()
				.any(|a| matches!(a.target, ParamTarget::Send { send } if send == new_send))
		);
	}

	#[test]
	fn undo_removes_the_copy_and_its_automation() {
		let mut fx = fixture();
		let before = fx.doc.project().tracks.len();
		let before_automation = fx.doc.project().automation.len();
		let (cmd, new_id) = duplicate_track(fx.doc.project(), fx.track).unwrap();
		fx.doc.execute(cmd).unwrap();
		assert_eq!(fx.doc.project().tracks.len(), before + 1);
		assert!(fx.doc.project().track(new_id).is_some());
		assert!(fx.doc.can_undo());
		fx.doc.undo().unwrap();
		assert_eq!(fx.doc.project().tracks.len(), before);
		assert!(fx.doc.project().track(new_id).is_none());
		assert_eq!(fx.doc.project().automation.len(), before_automation);
	}

	#[test]
	fn folders_cannot_be_duplicated() {
		let mut p = Project::new("t");
		let folder = Track::new("Folder", TrackKind::Folder);
		assert!(!can_duplicate(&folder));
		let id = folder.id;
		p.tracks.push(folder);
		let doc = Document::new(p);
		assert!(duplicate_track(doc.project(), id).is_none());
	}

	/// A comped track (two takes stacked over the same stretch, only one active): the
	/// copy must keep the same shape (one group, two lanes, the same one active) so only
	/// the active take plays on it too, not both.
	#[test]
	fn comped_track_keeps_only_the_active_take_playing() {
		use cadenza_model::Take;

		let mut p = Project::new("t");
		let mut track = Track::new("Vox", TrackKind::Instrument);
		let group = TakeGroupId::new();
		let mut take1 = Clip::midi("Take 1", Tick::ZERO, Tick(960));
		take1.take = Some(Take {
			group,
			lane: 0,
			active: false,
		});
		let mut take2 = Clip::midi("Take 2", Tick::ZERO, Tick(960));
		take2.take = Some(Take {
			group,
			lane: 1,
			active: true,
		});
		assert!(!take1.plays());
		assert!(take2.plays());
		let (take1_id, take2_id) = (take1.id, take2.id);
		track.clips.push(take1);
		track.clips.push(take2);
		let track_id = track.id;
		p.tracks.push(track);
		let mut doc = Document::new(p);

		let (cmd, new_id) = duplicate_track(doc.project(), track_id).unwrap();
		doc.execute(cmd).unwrap();
		let copy = doc.project().track(new_id).unwrap();
		assert_eq!(copy.clips.len(), 2);
		let new_take1 = copy.clips.iter().find(|c| c.name == "Take 1").unwrap();
		let new_take2 = copy.clips.iter().find(|c| c.name == "Take 2").unwrap();
		assert_ne!(new_take1.id, take1_id);
		assert_ne!(new_take2.id, take2_id);
		// Still one group, lanes kept, same one active: only the take that played before
		// plays on the copy.
		let (g1, g2) = (new_take1.take.unwrap().group, new_take2.take.unwrap().group);
		assert_eq!(g1, g2, "both copies stay in one (fresh) group");
		assert_ne!(g1, group, "but not the source's group");
		assert!(!new_take1.plays(), "inactive take stays inactive");
		assert!(new_take2.plays(), "active take stays active");
	}

	/// A trimmed linked clip (notes reach past its own, shorter length; valid because the
	/// group's longest copy covers them) must still validate as a copy: dropping `link`
	/// would bound it by its own length and reject the note past the end.
	#[test]
	fn trimmed_linked_clip_duplicates_without_a_validation_error() {
		let mut p = Project::new("t");
		let mut track = Track::new("Lead", TrackKind::Instrument);
		let link = LinkId::new();
		// Trimmed to 1 bar, but (like a real linked copy) still holds a note reaching
		// into bar 2: only valid because `link` makes `AddClip` skip the length bound.
		let mut clip = Clip::midi("Pattern", Tick::ZERO, Tick(960)).with_notes([Note::new(
			60,
			Tick(1200),
			Tick(100),
			0.8,
		)]);
		clip.link = Some(link);
		let clip_id = clip.id;
		track.clips.push(clip);
		let track_id = track.id;
		p.tracks.push(track);
		let mut doc = Document::new(p);

		let (cmd, new_id) = duplicate_track(doc.project(), track_id).unwrap();
		doc.execute(cmd)
			.expect("the copy must pass AddClip validation");
		let copy = doc.project().track(new_id).unwrap();
		assert_eq!(copy.clips.len(), 1);
		let new_clip = &copy.clips[0];
		assert_ne!(new_clip.id, clip_id);
		assert!(
			matches!(new_clip.link, Some(l) if l != link),
			"linked, but to a fresh id, not the source's"
		);
	}
}
