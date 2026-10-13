//! Validation + application of commands, producing their inverses.
//!
//! Every single command validates everything before its first mutation, so a failing
//! command leaves the project untouched. `Batch` applies on a clone and swaps it in on
//! success.
//!
//! Removal policy (cascades are recorded in the inverse, so undo restores them exactly):
//! - removing a track removes the automation lanes addressing it, removes every send that
//!   targets it and re-routes tracks that output to it back to the master;
//! - removing/replacing a device (insert or instrument) removes the automation lanes
//!   addressing that device;
//! - removing a track also disconnects sidechains keyed by it and drops instrument output
//!   routes targeting it.

use std::collections::HashSet;

use cadenza_core::{MeterChange, ParamAddress, ParamTarget, Tick, TrackId};

use crate::command::{
	AudioClipPatch, ChannelPatch, ClipPatch, Command, NotePatch, SendPatch, TrackPatch,
};
use crate::error::ModelError;
use crate::project::{AutomationLane, ClipContent, Note, Output, Project, sort_notes};
use crate::validate::{self, location};

/// Applies `cmd` and returns the commands that undo it, to be applied in order.
pub(crate) fn apply(project: &mut Project, cmd: &Command) -> Result<Vec<Command>, ModelError> {
	match cmd {
		Command::AddTrack { track, index } => {
			validate::new_track_ids(project, track)?;
			validate::track(project, track)?;
			validate::new_track_sidechains(project, track)?;
			let index = checked_index("track position", *index, project.tracks.len())?;
			let mut track = track.clone();
			track.sort_clips();
			for clip in &mut track.clips {
				if let ClipContent::Midi { notes } = &mut clip.content {
					sort_notes(notes);
				}
			}
			let id = track.id;
			project.tracks.insert(index, track);
			Ok(vec![Command::RemoveTrack { track: id }])
		}

		Command::MoveTrack { track, index } => {
			let from = project
				.track_index(*track)
				.ok_or(ModelError::TrackNotFound(*track))?;
			let max = project.tracks.len() - 1;
			if *index > max {
				return Err(ModelError::InvalidIndex {
					what: "track position",
					index: *index,
					max,
				});
			}
			let t = project.tracks.remove(from);
			project.tracks.insert(*index, t);
			Ok(vec![Command::MoveTrack {
				track: *track,
				index: from,
			}])
		}

		Command::RemoveTrack { track } => {
			let id = *track;
			let index = project
				.track_index(id)
				.ok_or(ModelError::TrackNotFound(id))?;
			let removed = project.tracks.remove(index);
			let mut inverse = vec![Command::AddTrack {
				track: removed,
				index: Some(index),
			}];
			let mut dropped_sends = Vec::new();
			for other in &mut project.tracks {
				if other.channel.output == Output::Bus(id) {
					other.channel.output = Output::Master;
					inverse.push(Command::SetChannel {
						track: Some(other.id),
						patch: ChannelPatch {
							output: Some(Output::Bus(id)),
							..Default::default()
						},
					});
				}
				let other_id = other.id;
				let mut index = 0;
				other.channel.sends.retain(|send| {
					let keep = send.target != id;
					if !keep {
						dropped_sends.push(send.id);
						inverse.push(Command::AddSend {
							track: other_id,
							send: send.clone(),
							index: Some(index),
						});
					}
					index += 1;
					keep
				});
				if let Some(inst) = &mut other.instrument
					&& inst.aux_outputs.iter().any(|r| r.bus == Some(id))
				{
					let old = inst.aux_outputs.clone();
					inst.aux_outputs.retain(|r| r.bus != Some(id));
					inverse.push(Command::SetOutputRoutes {
						track: other_id,
						routes: old,
					});
				}
				for d in other
					.instrument
					.iter_mut()
					.chain(other.channel.inserts.iter_mut())
				{
					unkey(d, id, Some(other_id), &mut inverse);
				}
			}
			for d in &mut project.master.inserts {
				unkey(d, id, None, &mut inverse);
			}
			inverse.extend(take_lanes(project, |a| {
				a.track == Some(id)
					|| matches!(a.target, ParamTarget::Send { send } if dropped_sends.contains(&send))
			}));
			Ok(inverse)
		}

		Command::UpdateTrack { track, patch } => {
			if patch.name.is_none() && patch.color.is_none() && patch.input.is_none() {
				return Err(empty_patch("update_track", "name, color and/or input"));
			}
			if let Some(name) = &patch.name {
				validate::name("track", name)?;
			}
			if let Some(Some(color)) = &patch.color {
				validate::color(color)?;
			}
			if let Some(input) = &patch.input {
				let t = project
					.track(*track)
					.ok_or(ModelError::TrackNotFound(*track))?;
				validate::track_input(t.id, t.kind, input.as_ref())?;
			}
			let t = project
				.track_mut(*track)
				.ok_or(ModelError::TrackNotFound(*track))?;
			let mut old = TrackPatch::default();
			if let Some(input) = &patch.input {
				old.input = Some(std::mem::replace(&mut t.input, *input));
			}
			if let Some(name) = &patch.name {
				old.name = Some(std::mem::replace(&mut t.name, name.clone()));
			}
			if let Some(color) = &patch.color {
				old.color = Some(std::mem::replace(&mut t.color, color.clone()));
			}
			Ok(vec![Command::UpdateTrack {
				track: *track,
				patch: old,
			}])
		}

		Command::SetChannel { track, patch } => {
			let p = patch;
			if p.volume_db.is_none()
				&& p.pan.is_none()
				&& p.mute.is_none()
				&& p.solo.is_none()
				&& p.output.is_none()
			{
				return Err(empty_patch(
					"set_channel",
					"volume_db, pan, mute, solo and/or output",
				));
			}
			if let Some(id) = track
				&& project.track(*id).is_none()
			{
				return Err(ModelError::TrackNotFound(*id));
			}
			if track.is_none() {
				if p.output.is_some() {
					return Err(ModelError::MasterRestriction(
						"has no routable output; it always feeds the hardware",
					));
				}
				if p.solo.is_some() {
					return Err(ModelError::MasterRestriction(
						"cannot be soloed; solo individual tracks instead",
					));
				}
			}
			if let Some(v) = p.volume_db {
				validate::volume(v)?;
			}
			if let Some(v) = p.pan {
				validate::pan(v)?;
			}
			if let Some(Output::Bus(bus)) = p.output {
				validate::route(project, *track, bus)?;
			}
			let channel = project
				.channel_mut(*track)
				.ok_or(ModelError::TrackNotFound(track.unwrap_or_default()))?;
			let old = ChannelPatch {
				volume_db: p
					.volume_db
					.map(|v| std::mem::replace(&mut channel.volume_db, v)),
				pan: p.pan.map(|v| std::mem::replace(&mut channel.pan, v)),
				mute: p.mute.map(|v| std::mem::replace(&mut channel.mute, v)),
				solo: p.solo.map(|v| std::mem::replace(&mut channel.solo, v)),
				output: p.output.map(|v| std::mem::replace(&mut channel.output, v)),
			};
			Ok(vec![Command::SetChannel {
				track: *track,
				patch: old,
			}])
		}

		Command::SetInstrument { track, device } => {
			let t = project
				.track(*track)
				.ok_or(ModelError::TrackNotFound(*track))?;
			let current = t.instrument.as_ref().map(|d| d.id);
			if let Some(d) = device {
				if t.kind != crate::TrackKind::Instrument {
					return Err(ModelError::InstrumentNotAllowed {
						track: *track,
						kind: t.kind,
					});
				}
				validate::device(d)?;
				if Some(d.id) != current && project.device_id_in_use(d.id) {
					return Err(ModelError::DuplicateDeviceId(d.id));
				}
				if d.sidechain.is_some() || !d.aux_outputs.is_empty() {
					// Judge the new links without the outgoing instrument's.
					let mut without = project.clone();
					if let Some(t) = without.track_mut(*track) {
						t.instrument = None;
					}
					validate::device_links(&without, Some(*track), d, true)?;
				}
			}
			let t = project
				.track_mut(*track)
				.ok_or(ModelError::TrackNotFound(*track))?;
			let old = std::mem::replace(&mut t.instrument, device.clone());
			let mut inverse = vec![Command::SetInstrument {
				track: *track,
				device: old,
			}];
			if let Some(old_id) = current
				&& device.as_ref().map(|d| d.id) != Some(old_id)
			{
				inverse.extend(take_lanes(project, |a| {
					a.track == Some(*track) && targets_device(a, old_id)
				}));
			}
			Ok(inverse)
		}

		Command::InsertDevice {
			track,
			device,
			index,
		} => {
			let chain_len = project
				.channel(*track)
				.ok_or_else(|| missing_track(*track))?
				.inserts
				.len();
			validate::device(device)?;
			if project.device_id_in_use(device.id) {
				return Err(ModelError::DuplicateDeviceId(device.id));
			}
			validate::device_links(project, *track, device, false)?;
			let index = checked_index("insert position", *index, chain_len)?;
			let channel = project
				.channel_mut(*track)
				.ok_or_else(|| missing_track(*track))?;
			channel.inserts.insert(index, device.clone());
			Ok(vec![Command::RemoveDevice {
				track: *track,
				device: device.id,
			}])
		}

		Command::RemoveDevice { track, device } => {
			let index = insert_index(project, *track, *device)?;
			let channel = project
				.channel_mut(*track)
				.ok_or_else(|| missing_track(*track))?;
			let removed = channel.inserts.remove(index);
			let mut inverse = vec![Command::InsertDevice {
				track: *track,
				device: removed,
				index: Some(index),
			}];
			inverse.extend(take_lanes(project, |a| {
				a.track == *track && targets_device(a, *device)
			}));
			Ok(inverse)
		}

		Command::MoveDevice {
			track,
			device,
			index,
		} => {
			let from = insert_index(project, *track, *device)?;
			let channel = project
				.channel_mut(*track)
				.ok_or_else(|| missing_track(*track))?;
			let len = channel.inserts.len();
			if *index >= len {
				return Err(ModelError::InvalidDeviceIndex {
					device: *device,
					index: *index,
					len,
				});
			}
			let d = channel.inserts.remove(from);
			channel.inserts.insert(*index, d);
			Ok(vec![Command::MoveDevice {
				track: *track,
				device: *device,
				index: from,
			}])
		}

		Command::SetDeviceBypass {
			track,
			device,
			bypass,
		} => {
			if let Some(id) = track
				&& project.track(*id).is_none()
			{
				return Err(ModelError::TrackNotFound(*id));
			}
			let d =
				project
					.device_mut(*track, *device)
					.ok_or_else(|| ModelError::DeviceNotFound {
						location: location(*track),
						device: *device,
					})?;
			let old = std::mem::replace(&mut d.bypass, *bypass);
			Ok(vec![Command::SetDeviceBypass {
				track: *track,
				device: *device,
				bypass: old,
			}])
		}

		Command::SetDeviceState {
			track,
			device,
			state,
			params,
		} => {
			if let Some(params) = params {
				validate::params(*device, params)?;
			}
			let d = existing_device_mut(project, *track, *device)?;
			let old_state = std::mem::replace(&mut d.state, state.clone());
			let old_params = params
				.as_ref()
				.map(|p| std::mem::replace(&mut d.params, p.clone()));
			Ok(vec![Command::SetDeviceState {
				track: *track,
				device: *device,
				state: old_state,
				params: old_params,
			}])
		}

		Command::SetSampleZones {
			track,
			device,
			zones,
		} => {
			crate::sample::validate_zones(zones)?;
			let d = existing_device_mut(project, *track, *device)?;
			if d.is_plugin() {
				return Err(ModelError::invalid(
					format!("zones of device {device}"),
					format!("{} zones", zones.len()),
					"only the built-in sampler and drum_sampler play sample zones; plugins load \
					 samples in their own editor",
				));
			}
			let old = std::mem::replace(&mut d.zones, zones.clone());
			Ok(vec![Command::SetSampleZones {
				track: *track,
				device: *device,
				zones: old,
			}])
		}

		Command::SetSidechain {
			track,
			device,
			source,
		} => {
			existing_device_mut(project, *track, *device)?;
			if let Some(source) = source {
				validate::sidechain(project, *track, *device, *source)?;
			}
			let d = existing_device_mut(project, *track, *device)?;
			let old = std::mem::replace(&mut d.sidechain, *source);
			Ok(vec![Command::SetSidechain {
				track: *track,
				device: *device,
				source: old,
			}])
		}

		Command::SetOutputRoutes { track, routes } => {
			let t = project
				.track(*track)
				.ok_or(ModelError::TrackNotFound(*track))?;
			if t.instrument.is_none() {
				return Err(ModelError::NoInstrument(*track));
			}
			validate::output_routes(project, *track, routes)?;
			let instrument = project
				.track_mut(*track)
				.and_then(|t| t.instrument.as_mut())
				.ok_or(ModelError::NoInstrument(*track))?;
			let old = std::mem::replace(&mut instrument.aux_outputs, routes.clone());
			Ok(vec![Command::SetOutputRoutes {
				track: *track,
				routes: old,
			}])
		}

		Command::SetParam { address, value } => {
			validate::address(project, address)?;
			validate::param_value(address, *value)?;
			let value = *value;
			let old = match &address.target {
				ParamTarget::Volume => {
					let channel = project
						.channel_mut(address.track)
						.ok_or_else(|| missing_track(address.track))?;
					Some(std::mem::replace(&mut channel.volume_db, value))
				}
				ParamTarget::Pan => {
					let channel = project
						.channel_mut(address.track)
						.ok_or_else(|| missing_track(address.track))?;
					Some(std::mem::replace(&mut channel.pan, value))
				}
				ParamTarget::Device { device, param } => {
					let d = project.device_mut(address.track, *device).ok_or_else(|| {
						ModelError::DeviceNotFound {
							location: location(address.track),
							device: *device,
						}
					})?;
					d.params.insert(param.clone(), value)
				}
				ParamTarget::Send { send } => {
					let track = address.track.ok_or_else(|| missing_track(None))?;
					let s = project
						.track_mut(track)
						.and_then(|t| t.channel.sends.iter_mut().find(|s| s.id == *send))
						.ok_or(ModelError::SendNotFound { track, send: *send })?;
					Some(std::mem::replace(&mut s.level_db, value))
				}
			};
			Ok(vec![match old {
				Some(value) => Command::SetParam {
					address: address.clone(),
					value,
				},
				None => Command::ResetParam {
					address: address.clone(),
				},
			}])
		}

		Command::ResetParam { address } => {
			let ParamTarget::Device { device, param } = &address.target else {
				return Err(ModelError::invalid(
					"reset_param address",
					address,
					"only device parameters can be reset; use set_param to change volume, pan or send levels",
				));
			};
			validate::address(project, address)?;
			let d = project.device_mut(address.track, *device).ok_or_else(|| {
				ModelError::DeviceNotFound {
					location: location(address.track),
					device: *device,
				}
			})?;
			Ok(match d.params.remove(param) {
				Some(value) => vec![Command::SetParam {
					address: address.clone(),
					value,
				}],
				None => Vec::new(),
			})
		}

		Command::AddSend { track, send, index } => {
			let t = project
				.track(*track)
				.ok_or(ModelError::TrackNotFound(*track))?;
			let len = t.channel.sends.len();
			validate::volume(send.level_db)?;
			if project.device_id_in_use(send.id) {
				return Err(ModelError::DuplicateDeviceId(send.id));
			}
			validate::route(project, Some(*track), send.target)?;
			let index = checked_index("send position", *index, len)?;
			let t = project
				.track_mut(*track)
				.ok_or(ModelError::TrackNotFound(*track))?;
			t.channel.sends.insert(index, send.clone());
			Ok(vec![Command::RemoveSend {
				track: *track,
				send: send.id,
			}])
		}

		Command::RemoveSend { track, send } => {
			let t = project
				.track_mut(*track)
				.ok_or(ModelError::TrackNotFound(*track))?;
			let index = t.channel.sends.iter().position(|s| s.id == *send).ok_or(
				ModelError::SendNotFound {
					track: *track,
					send: *send,
				},
			)?;
			let removed = t.channel.sends.remove(index);
			let mut inverse = vec![Command::AddSend {
				track: *track,
				send: removed,
				index: Some(index),
			}];
			let id = *send;
			inverse.extend(take_lanes(project, |a| {
				a.target == ParamTarget::Send { send: id }
			}));
			Ok(inverse)
		}

		Command::UpdateSend { track, send, patch } => {
			if patch.level_db.is_none() && patch.pre_fader.is_none() {
				return Err(empty_patch("update_send", "level_db and/or pre_fader"));
			}
			if let Some(v) = patch.level_db {
				validate::volume(v)?;
			}
			let t = project
				.track_mut(*track)
				.ok_or(ModelError::TrackNotFound(*track))?;
			let s = t.channel.sends.iter_mut().find(|s| s.id == *send).ok_or(
				ModelError::SendNotFound {
					track: *track,
					send: *send,
				},
			)?;
			let old = SendPatch {
				level_db: patch
					.level_db
					.map(|v| std::mem::replace(&mut s.level_db, v)),
				pre_fader: patch
					.pre_fader
					.map(|v| std::mem::replace(&mut s.pre_fader, v)),
			};
			Ok(vec![Command::UpdateSend {
				track: *track,
				send: *send,
				patch: old,
			}])
		}

		Command::AddClip { track, clip } => {
			let t = project
				.track(*track)
				.ok_or(ModelError::TrackNotFound(*track))?;
			validate::clip_on_track(t, clip)?;
			validate::clip(clip)?;
			validate::clip_fades(clip, &project.tempo)?;
			if project.clip_id_in_use(clip.id) {
				return Err(ModelError::DuplicateClipId(clip.id));
			}
			let mut clip = clip.clone();
			if let ClipContent::Midi { notes } = &mut clip.content {
				sort_notes(notes);
			}
			let id = clip.id;
			let t = project
				.track_mut(*track)
				.ok_or(ModelError::TrackNotFound(*track))?;
			t.clips.push(clip);
			t.sort_clips();
			Ok(vec![Command::RemoveClip { clip: id }])
		}

		Command::RemoveClip { clip } => {
			let track = project
				.find_clip(*clip)
				.ok_or(ModelError::ClipNotFound(*clip))?
				.0
				.id;
			let t = project
				.track_mut(track)
				.ok_or(ModelError::TrackNotFound(track))?;
			let index = t
				.clips
				.iter()
				.position(|c| c.id == *clip)
				.ok_or(ModelError::ClipNotFound(*clip))?;
			let removed = t.clips.remove(index);
			Ok(vec![Command::AddClip {
				track,
				clip: removed,
			}])
		}

		Command::UpdateClip { clip, patch } => update_clip(project, *clip, patch),

		Command::SetClipFades {
			clip,
			fade_in,
			fade_out,
		} => {
			let (_, c) = project
				.find_clip(*clip)
				.ok_or(ModelError::ClipNotFound(*clip))?;
			if c.is_midi() {
				return Err(ModelError::NotAnAudioClip(*clip));
			}
			let mut updated = c.clone();
			if let ClipContent::Audio {
				fade_in: fi,
				fade_out: fo,
				..
			} = &mut updated.content
			{
				*fi = *fade_in;
				*fo = *fade_out;
			}
			validate::clip(&updated)?;
			validate::clip_fades(&updated, &project.tempo)?;
			let (old_in, old_out) = c.fades();
			let target = project
				.find_clip_mut(*clip)
				.ok_or(ModelError::ClipNotFound(*clip))?;
			*target = updated;
			Ok(vec![Command::SetClipFades {
				clip: *clip,
				fade_in: old_in,
				fade_out: old_out,
			}])
		}

		Command::UpdateAudioClip { clip, patch } => {
			if patch.file.is_none() && patch.offset_seconds.is_none() && patch.gain_db.is_none() {
				return Err(empty_patch(
					"update_audio_clip",
					"file, offset_seconds and/or gain_db",
				));
			}
			let (_, c) = project
				.find_clip(*clip)
				.ok_or(ModelError::ClipNotFound(*clip))?;
			let mut updated = c.clone();
			let ClipContent::Audio {
				file,
				offset_seconds,
				gain_db,
				..
			} = &mut updated.content
			else {
				return Err(ModelError::NotAnAudioClip(*clip));
			};
			let old = AudioClipPatch {
				file: patch
					.file
					.as_ref()
					.map(|f| std::mem::replace(file, f.clone())),
				offset_seconds: patch
					.offset_seconds
					.map(|v| std::mem::replace(offset_seconds, v)),
				gain_db: patch.gain_db.map(|v| std::mem::replace(gain_db, v)),
			};
			validate::clip(&updated)?;
			let target = project
				.find_clip_mut(*clip)
				.ok_or(ModelError::ClipNotFound(*clip))?;
			*target = updated;
			Ok(vec![Command::UpdateAudioClip {
				clip: *clip,
				patch: old,
			}])
		}

		Command::AddNotes { clip, notes } => {
			if notes.is_empty() {
				return Err(ModelError::invalid(
					"add_notes notes",
					"[]",
					"provide at least one note",
				));
			}
			let (_, c) = project
				.find_clip(*clip)
				.ok_or(ModelError::ClipNotFound(*clip))?;
			let existing = midi_notes(c)?;
			let mut ids: HashSet<_> = existing.iter().map(|n| n.id).collect();
			for n in notes {
				if !ids.insert(n.id) {
					return Err(ModelError::DuplicateNoteId {
						clip: *clip,
						note: n.id,
					});
				}
				validate::note(c, n)?;
			}
			let target = midi_notes_mut(project, *clip)?;
			target.extend(notes.iter().cloned());
			sort_notes(target);
			Ok(vec![Command::RemoveNotes {
				clip: *clip,
				notes: notes.iter().map(|n| n.id).collect(),
			}])
		}

		Command::RemoveNotes { clip, notes } => {
			if notes.is_empty() {
				return Err(ModelError::invalid(
					"remove_notes notes",
					"[]",
					"provide at least one note id",
				));
			}
			let (_, c) = project
				.find_clip(*clip)
				.ok_or(ModelError::ClipNotFound(*clip))?;
			let existing = midi_notes(c)?;
			let mut ids = HashSet::with_capacity(notes.len());
			for id in notes {
				if !ids.insert(*id) {
					return Err(ModelError::DuplicateNoteId {
						clip: *clip,
						note: *id,
					});
				}
				if !existing.iter().any(|n| n.id == *id) {
					return Err(ModelError::NoteNotFound {
						clip: *clip,
						note: *id,
					});
				}
			}
			let target = midi_notes_mut(project, *clip)?;
			let mut removed = Vec::with_capacity(notes.len());
			target.retain(|n| {
				let keep = !ids.contains(&n.id);
				if !keep {
					removed.push(n.clone());
				}
				keep
			});
			Ok(vec![Command::AddNotes {
				clip: *clip,
				notes: removed,
			}])
		}

		Command::UpdateNotes {
			clip,
			notes: patches,
		} => {
			if patches.is_empty() {
				return Err(ModelError::invalid(
					"update_notes notes",
					"[]",
					"provide at least one note patch",
				));
			}
			let (_, c) = project
				.find_clip(*clip)
				.ok_or(ModelError::ClipNotFound(*clip))?;
			let existing = midi_notes(c)?;
			let mut ids = HashSet::with_capacity(patches.len());
			let mut updated = Vec::with_capacity(patches.len());
			let mut inverse = Vec::with_capacity(patches.len());
			for patch in patches {
				if !ids.insert(patch.id) {
					return Err(ModelError::DuplicateNoteId {
						clip: *clip,
						note: patch.id,
					});
				}
				let old =
					existing
						.iter()
						.find(|n| n.id == patch.id)
						.ok_or(ModelError::NoteNotFound {
							clip: *clip,
							note: patch.id,
						})?;
				let new = Note {
					id: old.id,
					pitch: patch.pitch.unwrap_or(old.pitch),
					velocity: patch.velocity.unwrap_or(old.velocity),
					start: patch.start.unwrap_or(old.start),
					length: patch.length.unwrap_or(old.length),
				};
				validate::note(c, &new)?;
				inverse.push(NotePatch {
					id: old.id,
					pitch: patch.pitch.map(|_| old.pitch),
					velocity: patch.velocity.map(|_| old.velocity),
					start: patch.start.map(|_| old.start),
					length: patch.length.map(|_| old.length),
				});
				updated.push(new);
			}
			let target = midi_notes_mut(project, *clip)?;
			for new in updated {
				if let Some(n) = target.iter_mut().find(|n| n.id == new.id) {
					*n = new;
				}
			}
			sort_notes(target);
			Ok(vec![Command::UpdateNotes {
				clip: *clip,
				notes: inverse,
			}])
		}

		Command::SetTempo { tick, bpm } => {
			validate::start("tempo point", *tick)?;
			validate::tempo(*bpm)?;
			let old = project
				.tempo
				.points()
				.iter()
				.find(|p| p.tick == *tick)
				.map(|p| p.bpm);
			project.tempo.set_point(*tick, *bpm);
			Ok(vec![match old {
				Some(bpm) => Command::SetTempo { tick: *tick, bpm },
				None => Command::RemoveTempoPoint { tick: *tick },
			}])
		}

		Command::RemoveTempoPoint { tick } => {
			if *tick == Tick::ZERO {
				return Err(ModelError::CannotRemoveFirstTempoPoint);
			}
			let removed = project
				.tempo
				.remove_point(*tick)
				.ok_or(ModelError::TempoPointNotFound(*tick))?;
			Ok(vec![Command::SetTempo {
				tick: *tick,
				bpm: removed.bpm,
			}])
		}

		Command::SetTimeSignature {
			time_signature,
			bar,
		} => {
			validate::time_signature(*time_signature)?;
			if *bar == 0 {
				let old = std::mem::replace(&mut project.time_signature, *time_signature);
				return Ok(vec![Command::SetTimeSignature {
					time_signature: old,
					bar: 0,
				}]);
			}
			validate::meter_bar(*bar)?;
			let changes = &mut project.time_signature_changes;
			let change = MeterChange::new(*bar, *time_signature);
			match changes.binary_search_by_key(bar, |c| c.bar) {
				Ok(i) => {
					let old = std::mem::replace(&mut changes[i], change);
					Ok(vec![Command::SetTimeSignature {
						time_signature: old.signature(),
						bar: *bar,
					}])
				}
				Err(i) => {
					if changes.len() >= validate::MAX_METER_CHANGES {
						return Err(ModelError::invalid(
							"time signature changes",
							changes.len().to_string(),
							"a project holds at most 1024 time signature changes",
						));
					}
					changes.insert(i, change);
					Ok(vec![Command::RemoveTimeSignature { bar: *bar }])
				}
			}
		}

		Command::RemoveTimeSignature { bar } => {
			let changes = &mut project.time_signature_changes;
			let i = changes
				.binary_search_by_key(bar, |c| c.bar)
				.map_err(|_| ModelError::TimeSignatureChangeNotFound(*bar))?;
			let old = changes.remove(i);
			Ok(vec![Command::SetTimeSignature {
				time_signature: old.signature(),
				bar: *bar,
			}])
		}

		Command::SetAutomation { address, points } => {
			validate::address(project, address)?;
			validate::automation_points(address, points)?;
			let mut points = points.clone();
			points.sort_by_key(|p| p.tick);
			let old = set_lane(project, address, points);
			Ok(vec![Command::SetAutomation {
				address: address.clone(),
				points: old,
			}])
		}

		Command::AddMarker { marker } => {
			validate::start("marker", marker.tick)?;
			validate::name("marker", &marker.name)?;
			match project
				.markers
				.binary_search_by_key(&marker.tick, |m| m.tick)
			{
				Ok(_) => Err(ModelError::DuplicateMarker(marker.tick)),
				Err(i) => {
					project.markers.insert(i, marker.clone());
					Ok(vec![Command::RemoveMarker { tick: marker.tick }])
				}
			}
		}

		Command::RemoveMarker { tick } => {
			let i = project
				.markers
				.binary_search_by_key(tick, |m| m.tick)
				.map_err(|_| ModelError::MarkerNotFound(*tick))?;
			let removed = project.markers.remove(i);
			Ok(vec![Command::AddMarker { marker: removed }])
		}

		Command::Batch { label, commands } => {
			if commands.is_empty() {
				return Err(ModelError::EmptyBatch {
					label: label.clone(),
				});
			}
			let mut scratch = project.clone();
			let mut inverses = Vec::with_capacity(commands.len());
			for (index, sub) in commands.iter().enumerate() {
				let inverse = apply(&mut scratch, sub).map_err(|e| ModelError::BatchFailed {
					label: label.clone(),
					index,
					op: sub.op().to_owned(),
					source: Box::new(e),
				})?;
				inverses.push(inverse);
			}
			*project = scratch;
			Ok(inverses.into_iter().rev().flatten().collect())
		}
	}
}

/// Applies a sequence of commands atomically (used for undo).
pub(crate) fn apply_all(
	project: &mut Project,
	commands: &[Command],
) -> Result<Vec<Command>, ModelError> {
	match commands {
		[] => Ok(Vec::new()),
		[single] => apply(project, single),
		_ => {
			let mut scratch = project.clone();
			let mut inverses = Vec::with_capacity(commands.len());
			for cmd in commands {
				inverses.push(apply(&mut scratch, cmd)?);
			}
			*project = scratch;
			Ok(inverses.into_iter().rev().flatten().collect())
		}
	}
}

fn update_clip(
	project: &mut Project,
	clip: cadenza_core::ClipId,
	patch: &ClipPatch,
) -> Result<Vec<Command>, ModelError> {
	if patch.name.is_none()
		&& patch.start.is_none()
		&& patch.length.is_none()
		&& patch.track.is_none()
		&& patch.take.is_none()
	{
		return Err(empty_patch(
			"update_clip",
			"name, start, length, track and/or take",
		));
	}
	if let Some(Some(take)) = &patch.take {
		validate::take(take)?;
	}
	let (track, current) = project
		.find_clip(clip)
		.ok_or(ModelError::ClipNotFound(clip))?;
	let from = track.id;
	let to = patch.track.unwrap_or(from);
	if to != from {
		let dest = project.track(to).ok_or(ModelError::TrackNotFound(to))?;
		validate::clip_on_track(dest, current)?;
	}
	if let Some(name) = &patch.name {
		validate::name("clip", name)?;
	}
	if let Some(start) = patch.start {
		validate::start("clip", start)?;
	}
	if let Some(length) = patch.length {
		validate::length("clip", length)?;
		let outside = current.notes().iter().filter(|n| n.start >= length).count();
		if outside > 0 {
			return Err(ModelError::NotesWouldStartOutsideClip {
				clip,
				length: length.0,
				count: outside,
			});
		}
	}
	let inverse = ClipPatch {
		name: patch.name.as_ref().map(|_| current.name.clone()),
		start: patch.start.map(|_| current.start),
		length: patch.length.map(|_| current.length),
		track: patch.track.map(|_| from),
		take: patch.take.map(|_| current.take),
	};

	let source = project
		.track_mut(from)
		.ok_or(ModelError::TrackNotFound(from))?;
	let index = source
		.clips
		.iter()
		.position(|c| c.id == clip)
		.ok_or(ModelError::ClipNotFound(clip))?;
	let mut updated = source.clips.remove(index);
	if let Some(name) = &patch.name {
		updated.name = name.clone();
	}
	if let Some(start) = patch.start {
		updated.start = start;
	}
	if let Some(length) = patch.length {
		updated.length = length;
	}
	if let Some(take) = patch.take {
		updated.take = take;
	}
	let dest = project.track_mut(to).ok_or(ModelError::TrackNotFound(to))?;
	dest.clips.push(updated);
	dest.sort_clips();
	Ok(vec![Command::UpdateClip {
		clip,
		patch: inverse,
	}])
}

/// Replaces (or with empty `points` removes) the lane for `address`, keeping lanes sorted
/// by address. Returns the previous points (empty if there was no lane).
fn set_lane(
	project: &mut Project,
	address: &ParamAddress,
	points: Vec<crate::AutomationPoint>,
) -> Vec<crate::AutomationPoint> {
	let key = address.to_string();
	let lanes = &mut project.automation;
	match lanes.binary_search_by(|l| l.address.to_string().cmp(&key)) {
		Ok(i) if points.is_empty() => lanes.remove(i).points,
		Ok(i) => std::mem::replace(&mut lanes[i].points, points),
		Err(_) if points.is_empty() => Vec::new(),
		Err(i) => {
			lanes.insert(
				i,
				AutomationLane {
					address: address.clone(),
					points,
				},
			);
			Vec::new()
		}
	}
}

/// Removes the lanes matching `pred` and returns the commands that restore them.
fn take_lanes(project: &mut Project, pred: impl Fn(&ParamAddress) -> bool) -> Vec<Command> {
	let mut restore = Vec::new();
	project.automation.retain(|lane| {
		let keep = !pred(&lane.address);
		if !keep {
			restore.push(Command::SetAutomation {
				address: lane.address.clone(),
				points: lane.points.clone(),
			});
		}
		keep
	});
	restore
}

/// Disconnects `device`'s sidechain if it is keyed by `source`, recording the undo.
fn unkey(
	device: &mut crate::Device,
	source: TrackId,
	owner: Option<TrackId>,
	inverse: &mut Vec<Command>,
) {
	if device.sidechain.is_some_and(|s| s.track == source) {
		inverse.push(Command::SetSidechain {
			track: owner,
			device: device.id,
			source: device.sidechain.take(),
		});
	}
}

fn existing_device_mut(
	project: &mut Project,
	track: Option<TrackId>,
	device: cadenza_core::DeviceId,
) -> Result<&mut crate::Device, ModelError> {
	if let Some(id) = track
		&& project.track(id).is_none()
	{
		return Err(ModelError::TrackNotFound(id));
	}
	project
		.device_mut(track, device)
		.ok_or_else(|| ModelError::DeviceNotFound {
			location: location(track),
			device,
		})
}

fn targets_device(address: &ParamAddress, id: cadenza_core::DeviceId) -> bool {
	matches!(&address.target, ParamTarget::Device { device, .. } if *device == id)
}

fn insert_index(
	project: &Project,
	track: Option<TrackId>,
	device: cadenza_core::DeviceId,
) -> Result<usize, ModelError> {
	if let Some(id) = track {
		let t = project.track(id).ok_or(ModelError::TrackNotFound(id))?;
		if t.instrument.as_ref().is_some_and(|d| d.id == device) {
			return Err(ModelError::DeviceIsInstrument(device));
		}
	}
	let channel = project.channel(track).ok_or_else(|| missing_track(track))?;
	channel
		.inserts
		.iter()
		.position(|d| d.id == device)
		.ok_or_else(|| ModelError::DeviceNotFound {
			location: location(track),
			device,
		})
}

fn midi_notes(clip: &crate::Clip) -> Result<&[Note], ModelError> {
	match &clip.content {
		ClipContent::Midi { notes } => Ok(notes),
		ClipContent::Audio { .. } => Err(ModelError::NotAMidiClip(clip.id)),
	}
}

fn midi_notes_mut(
	project: &mut Project,
	clip: cadenza_core::ClipId,
) -> Result<&mut Vec<Note>, ModelError> {
	match &mut project
		.find_clip_mut(clip)
		.ok_or(ModelError::ClipNotFound(clip))?
		.content
	{
		ClipContent::Midi { notes } => Ok(notes),
		ClipContent::Audio { .. } => Err(ModelError::NotAMidiClip(clip)),
	}
}

fn checked_index(
	what: &'static str,
	index: Option<usize>,
	len: usize,
) -> Result<usize, ModelError> {
	match index {
		None => Ok(len),
		Some(i) if i <= len => Ok(i),
		Some(i) => Err(ModelError::InvalidIndex {
			what,
			index: i,
			max: len,
		}),
	}
}

fn missing_track(track: Option<TrackId>) -> ModelError {
	ModelError::TrackNotFound(track.unwrap_or_default())
}

fn empty_patch(op: &str, fields: &str) -> ModelError {
	ModelError::invalid(
		format!("{op} patch"),
		"{}",
		format!("set at least one of {fields}"),
	)
}
