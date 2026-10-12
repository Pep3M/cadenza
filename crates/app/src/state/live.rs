//! Which edits the running player can take without recompiling.
//!
//! [`diff`] compares the project before and after an edit (a command, an undo, a shared
//! MCP tool): when the only differences are fader volumes/pans, send levels, device
//! parameter values and automation lanes, they are sent to the player live
//! (`Player::set_live_*`, `Player::set_automation`) and sounding notes keep playing.
//! Anything else (tracks, devices, routing, clips, tempo, a parameter reset to its
//! default...) is structural and reloads the player.

use cadenza_core::{DeviceId, TrackId};
use cadenza_model::{Channel, Device, Project};

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
}

impl LiveChanges {
	pub fn is_empty(&self) -> bool {
		*self == Self::default()
	}
}

/// The live changes turning `old` into `new`, or `None` when the edit is structural.
pub fn diff(old: &Project, new: &Project) -> Option<LiveChanges> {
	if old.tracks.len() != new.tracks.len() {
		return None;
	}
	let mut normalized = old.clone();
	let mut changes = LiveChanges::default();
	for (o, n) in normalized.tracks.iter_mut().zip(&new.tracks) {
		if o.id != n.id {
			return None;
		}
		channel(Some(o.id), &mut o.channel, &n.channel, &mut changes)?;
		if let (Some(od), Some(nd)) = (&mut o.instrument, &n.instrument) {
			device(od, nd, &mut changes)?;
		}
	}
	channel(None, &mut normalized.master, &new.master, &mut changes)?;
	if normalized.automation != new.automation {
		changes.automation = true;
		normalized.automation = new.automation.clone();
	}
	(normalized == *new).then_some(changes)
}

fn channel(
	track: Option<TrackId>,
	o: &mut Channel,
	n: &Channel,
	changes: &mut LiveChanges,
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
		device(od, nd, changes)?;
	}
	Some(())
}

/// Parameter values set or changed on the same device; a parameter that disappeared
/// (reset to the device default) is structural.
fn device(o: &mut Device, n: &Device, changes: &mut LiveChanges) -> Option<()> {
	if o.id != n.id {
		return Some(());
	}
	if o.params.keys().any(|k| !n.params.contains_key(k)) {
		return None;
	}
	for (k, &v) in &n.params {
		if o.params.get(k) != Some(&v) {
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
}
