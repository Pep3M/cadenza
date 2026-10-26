//! Which edits the running player can take without recompiling.
//!
//! [`diff`] compares the project before and after an edit (a command, an undo, a shared
//! MCP tool): when the only differences are fader volumes/pans, send levels, device
//! parameter values, automation lanes and the MIDI clips of instrument tracks (notes,
//! clip moves/resizes: the piano roll's edits), they are sent to the player live
//! (`Player::set_live_*`, `Player::set_automation`, `Player::set_track_clips`) and
//! sounding notes keep playing. Anything else (tracks, devices, routing, audio clips,
//! tempo, a parameter reset to its default...) is structural and reloads the player.
//! A device state is live only when the running instance already holds it (a state
//! captured from a plugin editor; see `state::editors`).

use cadenza_core::{DeviceId, TrackId};
use cadenza_model::{Channel, Device, DeviceKind, Project, TrackKind};

/// Edits the player can apply live.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LiveChanges {
	/// `None` track = master.
	pub volumes: Vec<(Option<TrackId>, f32)>,
	pub pans: Vec<(Option<TrackId>, f32)>,
	pub sends: Vec<(DeviceId, f32)>,
	/// Device, param id, plain value.
	pub params: Vec<(DeviceId, String, f32)>,
	/// The automation lanes changed.
	pub automation: bool,
	/// Instrument tracks whose MIDI clips changed (reschedule them).
	pub clips: Vec<TrackId>,
	/// The MIDI controller assignments changed (`Player::set_midi_map`).
	pub midi_map: bool,
}

impl LiveChanges {
	pub fn is_empty(&self) -> bool {
		*self == Self::default()
	}
}

/// Whether the running instance of a device holds this (base64) state.
pub type LiveState<'a> = &'a dyn Fn(DeviceId, Option<&str>) -> bool;

/// The live changes turning `old` into `new`, or `None` when the edit is structural.
pub fn diff(old: &Project, new: &Project, live_state: LiveState) -> Option<LiveChanges> {
	if old.tracks.len() != new.tracks.len() {
		return None;
	}
	let mut normalized = old.clone();
	let mut changes = LiveChanges::default();
	for (o, n) in normalized.tracks.iter_mut().zip(&new.tracks) {
		if o.id != n.id {
			return None;
		}
		channel(
			Some(o.id),
			&mut o.channel,
			&n.channel,
			&mut changes,
			live_state,
		)?;
		if let (Some(od), Some(nd)) = (&mut o.instrument, &n.instrument) {
			device(od, nd, &mut changes, live_state)?;
		}
		if o.clips != n.clips && o.kind == TrackKind::Instrument && n.kind == o.kind {
			// Effects fed this track's MIDI hold their own schedule: recompile for them.
			if feeds_midi(new, n.id) {
				return None;
			}
			o.clips = n.clips.clone();
			changes.clips.push(n.id);
		}
	}
	channel(
		None,
		&mut normalized.master,
		&new.master,
		&mut changes,
		live_state,
	)?;
	if normalized.automation != new.automation {
		changes.automation = true;
		normalized.automation = new.automation.clone();
	}
	// Song metadata does not sound; arm states reach the player on their own.
	normalized.meta = new.meta.clone();
	normalized.armed = new.armed.clone();
	if normalized.midi_map != new.midi_map {
		changes.midi_map = true;
		normalized.midi_map = new.midi_map.clone();
	}
	(normalized == *new).then_some(changes)
}

/// Whether a device of `project` takes the MIDI of `track`.
fn feeds_midi(project: &Project, track: TrackId) -> bool {
	project
		.tracks
		.iter()
		.flat_map(|t| t.devices())
		.chain(&project.master.inserts)
		.any(|d| d.midi_input == Some(track))
}

fn channel(
	track: Option<TrackId>,
	o: &mut Channel,
	n: &Channel,
	changes: &mut LiveChanges,
	live_state: LiveState,
) -> Option<()> {
	if o.volume_db != n.volume_db {
		o.volume_db = n.volume_db;
		changes.volumes.push((track, n.volume_db));
	}
	if o.pan != n.pan {
		o.pan = n.pan;
		changes.pans.push((track, n.pan));
	}
	if o.sends.len() != n.sends.len() || o.inserts.len() != n.inserts.len() {
		return None;
	}
	for (os, ns) in o.sends.iter_mut().zip(&n.sends) {
		if os.id == ns.id && os.level_db != ns.level_db {
			os.level_db = ns.level_db;
			changes.sends.push((ns.id, ns.level_db));
		}
	}
	for (od, nd) in o.inserts.iter_mut().zip(&n.inserts) {
		device(od, nd, changes, live_state)?;
	}
	Some(())
}

/// Parameter values set or changed on the same device; a parameter that disappeared
/// (reset to the device default) is structural, and so is a new state unless the
/// running instance already holds it.
fn device(
	o: &mut Device,
	n: &Device,
	changes: &mut LiveChanges,
	live_state: LiveState,
) -> Option<()> {
	if o.id != n.id {
		return Some(());
	}
	if o.state != n.state && live_state(n.id, n.state.as_deref()) {
		o.state = n.state.clone();
	}
	if o.params.keys().any(|k| !n.params.contains_key(k)) {
		return None;
	}
	for (k, &v) in &n.params {
		if o.params.get(k) != Some(&v) {
			if let DeviceKind::Builtin { id } = &n.kind
				&& cadenza_dsp::param_changes_latency(id, k)
			{
				return None;
			}
			o.params.insert(k.clone(), v);
			changes.params.push((n.id, k.clone(), v));
		}
	}
	Some(())
}

#[cfg(test)]
mod tests {
	use super::*;
	use cadenza_core::{ParamAddress, Tick};
	use cadenza_model::{AutomationPoint, ChannelPatch, Command, Document, Send, Track, TrackKind};

	fn project() -> (Project, TrackId, DeviceId, DeviceId) {
		let mut p = Project::new("live");
		let bus = Track::new("Bus", TrackKind::Bus);
		let mut t = Track::new("Synth", TrackKind::Instrument)
			.with_instrument(Device::builtin("poly_synth", "Synth"));
		let send = Send::new(bus.id, -12.0);
		let (sid, did) = (send.id, t.instrument.as_ref().unwrap().id);
		t.channel.sends.push(send);
		let id = t.id;
		p.tracks = vec![t, bus];
		(p, id, did, sid)
	}

	fn diff(old: &Project, new: &Project) -> Option<LiveChanges> {
		super::diff(old, new, &|_, _| false)
	}

	fn after(p: &Project, cmd: Command) -> Project {
		let mut doc = Document::new(p.clone());
		doc.execute(cmd).unwrap();
		doc.into_project()
	}

	#[test]
	fn mix_values_and_automation_are_live() {
		let (p, track, synth, send) = project();
		let n = after(
			&p,
			Command::SetParam {
				address: ParamAddress::device(Some(track), synth, "filter_cutoff"),
				value: 800.0,
			},
		);
		let c = diff(&p, &n).unwrap();
		assert_eq!(c.params, vec![(synth, "filter_cutoff".to_owned(), 800.0)]);
		let n2 = after(
			&n,
			Command::SetParam {
				address: ParamAddress::send(track, send),
				value: -3.0,
			},
		);
		assert_eq!(diff(&n, &n2).unwrap().sends, vec![(send, -3.0)]);
		let n3 = after(
			&n2,
			Command::SetChannel {
				track: None,
				patch: ChannelPatch {
					volume_db: Some(-2.0),
					pan: Some(0.5),
					..Default::default()
				},
			},
		);
		let c = diff(&n2, &n3).unwrap();
		assert_eq!((c.volumes, c.pans), (vec![(None, -2.0)], vec![(None, 0.5)]));
		let n4 = after(
			&n3,
			Command::SetAutomation {
				address: ParamAddress::track_volume(track),
				points: vec![AutomationPoint::new(Tick(0), -6.0)],
			},
		);
		assert!(diff(&n3, &n4).unwrap().automation);
		assert!(diff(&p, &p).unwrap().is_empty());
	}

	#[test]
	fn midi_clip_edits_reschedule_their_track() {
		let (mut p, track, _, _) = project();
		let clip = cadenza_model::Clip::midi("c", Tick(0), Tick(960 * 4));
		let clip_id = clip.id;
		p.tracks[0].clips.push(clip);
		let n = after(
			&p,
			Command::AddNotes {
				clip: clip_id,
				notes: vec![cadenza_model::Note::new(60, Tick(0), Tick(480), 0.8)],
			},
		);
		let c = diff(&p, &n).unwrap();
		assert_eq!(c.clips, vec![track]);
		assert!(c.params.is_empty() && !c.automation);
		let moved = after(
			&n,
			Command::UpdateClip {
				clip: clip_id,
				patch: cadenza_model::ClipPatch {
					start: Some(Tick(960)),
					..Default::default()
				},
			},
		);
		assert_eq!(diff(&n, &moved).unwrap().clips, vec![track]);
		// A new instrument track is structural.
		let added = after(
			&p,
			Command::AddTrack {
				track: Track::new("x", TrackKind::Instrument),
				index: None,
			},
		);
		assert!(diff(&p, &added).is_none());
		// Once an effect takes this track's MIDI, its edits recompile.
		let mut vox = Track::new("Vox", TrackKind::Audio);
		vox.channel
			.inserts
			.push(Device::builtin("pitch_correct", "Tune"));
		vox.channel.inserts[0].midi_input = Some(track);
		let mut fed = n.clone();
		fed.tracks.push(vox.clone());
		let mut edited = moved.clone();
		edited.tracks.push(vox);
		assert!(diff(&fed, &edited).is_none());
	}

	#[test]
	fn structural_edits_reload() {
		let (p, track, synth, _) = project();
		let with = after(
			&p,
			Command::SetParam {
				address: ParamAddress::device(Some(track), synth, "filter_cutoff"),
				value: 800.0,
			},
		);
		// Reset to the default: the engine needs the device's default, so reload.
		assert!(diff(&with, &p).is_none());
		let muted = after(
			&p,
			Command::SetChannel {
				track: Some(track),
				patch: ChannelPatch {
					mute: Some(true),
					..Default::default()
				},
			},
		);
		assert!(diff(&p, &muted).is_none());
		let bypassed = after(
			&p,
			Command::SetDeviceBypass {
				track: Some(track),
				device: synth,
				bypass: true,
			},
		);
		assert!(diff(&p, &bypassed).is_none());
		let tempo = after(
			&p,
			Command::SetTempo {
				tick: Tick(0),
				bpm: 90.0,
			},
		);
		assert!(diff(&p, &tempo).is_none());
	}

	#[test]
	fn states_the_running_instance_holds_are_live() {
		let (p, track, synth, _) = project();
		let n = after(
			&p,
			Command::SetDeviceState {
				track: Some(track),
				device: synth,
				state: Some("TElWRQ==".into()),
				params: None,
			},
		);
		assert!(diff(&p, &n).is_none());
		let live = |d: DeviceId, s: Option<&str>| d == synth && s == Some("TElWRQ==");
		assert!(super::diff(&p, &n, &live).unwrap().is_empty());
		// Undoing it restores a state the instance does not hold: reload.
		assert!(super::diff(&n, &p, &live).is_none());
	}
}
