//! Validation rules shared by command application and project loading.

use std::collections::HashSet;

use cadenza_core::{ClipId, ParamAddress, ParamTarget, Tick, TimeSignature, TrackId};

use crate::error::ModelError;
use crate::project::{
	AutomationPoint, Channel, Clip, ClipContent, Device, Note, Output, Project, Track, TrackKind,
};

pub const MIN_VOLUME_DB: f32 = -144.0;
pub const MAX_VOLUME_DB: f32 = 12.0;
pub const MIN_BPM: f64 = 20.0;
pub const MAX_BPM: f64 = 999.0;

pub(crate) fn volume(db: f32) -> Result<(), ModelError> {
	if (MIN_VOLUME_DB..=MAX_VOLUME_DB).contains(&db) {
		Ok(())
	} else {
		Err(ModelError::InvalidVolume(db))
	}
}

pub(crate) fn pan(pan: f32) -> Result<(), ModelError> {
	if (-1.0..=1.0).contains(&pan) {
		Ok(())
	} else {
		Err(ModelError::InvalidPan(pan))
	}
}

pub(crate) fn pitch(pitch: u8) -> Result<(), ModelError> {
	if pitch <= 127 {
		Ok(())
	} else {
		Err(ModelError::InvalidPitch(pitch))
	}
}

pub(crate) fn velocity(v: f32) -> Result<(), ModelError> {
	if (0.0..=1.0).contains(&v) {
		Ok(())
	} else {
		Err(ModelError::InvalidVelocity(v))
	}
}

pub(crate) fn length(what: &'static str, len: Tick) -> Result<(), ModelError> {
	if len.0 > 0 {
		Ok(())
	} else {
		Err(ModelError::InvalidLength { what, ticks: len.0 })
	}
}

pub(crate) fn start(what: &'static str, start: Tick) -> Result<(), ModelError> {
	if start.0 >= 0 {
		Ok(())
	} else {
		Err(ModelError::NegativeStart {
			what,
			ticks: start.0,
		})
	}
}

pub(crate) fn tempo(bpm: f64) -> Result<(), ModelError> {
	if (MIN_BPM..=MAX_BPM).contains(&bpm) {
		Ok(())
	} else {
		Err(ModelError::InvalidTempo(bpm))
	}
}

pub(crate) fn time_signature(ts: TimeSignature) -> Result<(), ModelError> {
	let ok = (1..=32).contains(&ts.numerator)
		&& (1..=32).contains(&ts.denominator)
		&& ts.denominator.is_power_of_two();
	if ok {
		Ok(())
	} else {
		Err(ModelError::InvalidTimeSignature {
			numerator: ts.numerator,
			denominator: ts.denominator,
		})
	}
}

pub(crate) fn name(what: &str, name: &str) -> Result<(), ModelError> {
	if name.trim().is_empty() {
		Err(ModelError::invalid(
			format!("{what} name"),
			format!("{name:?}"),
			"names must not be empty",
		))
	} else {
		Ok(())
	}
}

pub(crate) fn color(color: &str) -> Result<(), ModelError> {
	let ok = color.len() == 7
		&& color.starts_with('#')
		&& color[1..].chars().all(|c| c.is_ascii_hexdigit());
	if ok {
		Ok(())
	} else {
		Err(ModelError::invalid(
			"track color",
			format!("{color:?}"),
			"colors must be \"#RRGGBB\" hex, e.g. \"#3FA9F5\"",
		))
	}
}

pub(crate) fn finite(what: impl Into<String>, value: f32) -> Result<(), ModelError> {
	if value.is_finite() {
		Ok(())
	} else {
		Err(ModelError::invalid(what, value, "must be a finite number"))
	}
}

pub(crate) fn device(device: &Device) -> Result<(), ModelError> {
	name("device", &device.name)?;
	for (param, value) in &device.params {
		if param.is_empty() || param.contains('/') {
			return Err(ModelError::invalid(
				format!("parameter id on device {}", device.id),
				format!("{param:?}"),
				"param ids must be non-empty and must not contain '/'",
			));
		}
		finite(
			format!("parameter '{param}' of device {}", device.id),
			*value,
		)?;
	}
	Ok(())
}

pub(crate) fn note(clip: &Clip, note: &Note) -> Result<(), ModelError> {
	pitch(note.pitch)?;
	velocity(note.velocity)?;
	length("note", note.length)?;
	start("note", note.start)?;
	if note.start >= clip.length {
		return Err(ModelError::NoteOutsideClip {
			clip: clip.id,
			note: note.id,
			start: note.start.0,
			clip_length: clip.length.0,
		});
	}
	Ok(())
}

/// Validates a clip in isolation (kind compatibility is checked by [`clip_on_track`]).
pub(crate) fn clip(clip: &Clip) -> Result<(), ModelError> {
	start("clip", clip.start)?;
	length("clip", clip.length)?;
	match &clip.content {
		ClipContent::Midi { notes } => {
			let mut ids = HashSet::with_capacity(notes.len());
			for n in notes {
				if !ids.insert(n.id) {
					return Err(ModelError::DuplicateNoteId {
						clip: clip.id,
						note: n.id,
					});
				}
				note(clip, n)?;
			}
		}
		ClipContent::Audio {
			file,
			offset_seconds,
			gain_db,
		} => {
			if file.as_os_str().is_empty() {
				return Err(ModelError::invalid(
					"audio clip file",
					"\"\"",
					"provide the path of an audio file",
				));
			}
			if !(offset_seconds.is_finite() && *offset_seconds >= 0.0) {
				return Err(ModelError::invalid(
					"audio clip offset_seconds",
					offset_seconds,
					"must be >= 0",
				));
			}
			volume(*gain_db)?;
		}
	}
	Ok(())
}

pub(crate) fn clip_on_track(track: &Track, clip: &Clip) -> Result<(), ModelError> {
	let (content, allowed, ok) = match clip.content {
		ClipContent::Midi { .. } => ("MIDI", "Instrument", track.kind == TrackKind::Instrument),
		ClipContent::Audio { .. } => ("audio", "Audio", track.kind == TrackKind::Audio),
	};
	if ok {
		Ok(())
	} else {
		Err(ModelError::ClipKindMismatch {
			track: track.id,
			kind: track.kind,
			content,
			allowed,
		})
	}
}

/// `target` must be an existing Bus track other than `from`, and routing `from -> target`
/// must not close a cycle.
pub(crate) fn route(
	project: &Project,
	from: Option<TrackId>,
	target: TrackId,
) -> Result<(), ModelError> {
	let Some(bus) = project.track(target) else {
		return Err(ModelError::TrackNotFound(target));
	};
	if bus.kind != TrackKind::Bus {
		return Err(ModelError::NotABus {
			track: target,
			kind: bus.kind,
		});
	}
	let Some(from) = from else {
		return Err(ModelError::MasterRestriction(
			"cannot route to a bus; it is the final output",
		));
	};
	if reaches(project, target, from) {
		return Err(ModelError::RoutingCycle { from, to: target });
	}
	Ok(())
}

fn targets(channel: &Channel) -> impl Iterator<Item = TrackId> + '_ {
	let output = match channel.output {
		Output::Bus(id) => Some(id),
		Output::Master => None,
	};
	output
		.into_iter()
		.chain(channel.sends.iter().map(|s| s.target))
}

/// Whether audio leaving `from` can arrive at `to` (including `from == to`).
fn reaches(project: &Project, from: TrackId, to: TrackId) -> bool {
	let mut stack = vec![from];
	let mut seen = HashSet::new();
	while let Some(id) = stack.pop() {
		if id == to {
			return true;
		}
		if seen.insert(id)
			&& let Some(track) = project.track(id)
		{
			stack.extend(targets(&track.channel));
		}
	}
	false
}

/// Checks a track's own content and the routes it declares. Does not check id uniqueness
/// against the project (see [`new_track_ids`]).
pub(crate) fn track(project: &Project, track: &Track) -> Result<(), ModelError> {
	name("track", &track.name)?;
	if let Some(c) = &track.color {
		color(c)?;
	}
	if let Some(instrument) = &track.instrument {
		if track.kind != TrackKind::Instrument {
			return Err(ModelError::InstrumentNotAllowed {
				track: track.id,
				kind: track.kind,
			});
		}
		device(instrument)?;
	}
	for c in &track.clips {
		clip_on_track(track, c)?;
		self::clip(c)?;
	}
	channel_values(&track.channel)?;
	for target in targets(&track.channel) {
		if target == track.id {
			return Err(ModelError::RoutingCycle {
				from: track.id,
				to: target,
			});
		}
		let Some(bus) = project.track(target) else {
			return Err(ModelError::TrackNotFound(target));
		};
		if bus.kind != TrackKind::Bus {
			return Err(ModelError::NotABus {
				track: target,
				kind: bus.kind,
			});
		}
	}
	Ok(())
}

pub(crate) fn channel_values(channel: &Channel) -> Result<(), ModelError> {
	volume(channel.volume_db)?;
	pan(channel.pan)?;
	for d in &channel.inserts {
		device(d)?;
	}
	for s in &channel.sends {
		volume(s.level_db)?;
	}
	Ok(())
}

/// Ensures none of the ids carried by `track` already exist in `project` or repeat inside it.
pub(crate) fn new_track_ids(project: &Project, track: &Track) -> Result<(), ModelError> {
	if project.track(track.id).is_some() {
		return Err(ModelError::DuplicateTrackId(track.id));
	}
	let mut devices = HashSet::new();
	let device_ids = track
		.instrument
		.iter()
		.chain(track.channel.inserts.iter())
		.map(|d| d.id)
		.chain(track.channel.sends.iter().map(|s| s.id));
	for id in device_ids {
		if !devices.insert(id) || project.device_id_in_use(id) {
			return Err(ModelError::DuplicateDeviceId(id));
		}
	}
	let mut clips: HashSet<ClipId> = HashSet::new();
	for c in &track.clips {
		if !clips.insert(c.id) || project.clip_id_in_use(c.id) {
			return Err(ModelError::DuplicateClipId(c.id));
		}
	}
	Ok(())
}

/// Checks that `address` resolves to something automatable in `project`.
pub(crate) fn address(project: &Project, address: &ParamAddress) -> Result<(), ModelError> {
	if let Some(id) = address.track
		&& project.track(id).is_none()
	{
		return Err(ModelError::TrackNotFound(id));
	}
	if let ParamTarget::Device { device, .. } = &address.target
		&& project.device(address.track, *device).is_none()
	{
		return Err(ModelError::DeviceNotFound {
			location: location(address.track),
			device: *device,
		});
	}
	Ok(())
}

/// Validates a value for `address` (dB range for volume, -1..1 for pan, finite otherwise).
pub(crate) fn param_value(address: &ParamAddress, value: f32) -> Result<(), ModelError> {
	match &address.target {
		ParamTarget::Volume => volume(value),
		ParamTarget::Pan => pan(value),
		ParamTarget::Device { param, .. } => finite(format!("parameter '{param}'"), value),
	}
}

pub(crate) fn automation_points(
	address: &ParamAddress,
	points: &[AutomationPoint],
) -> Result<(), ModelError> {
	let mut ticks = HashSet::with_capacity(points.len());
	for p in points {
		start("automation point", p.tick)?;
		if !ticks.insert(p.tick) {
			return Err(ModelError::InvalidAutomation {
				address: address.clone(),
				reason: format!(
					"two points share tick {} (beat {}); keep one point per tick",
					p.tick.0,
					p.tick.beats()
				),
			});
		}
		param_value(address, p.value)?;
	}
	Ok(())
}

pub(crate) fn location(track: Option<TrackId>) -> String {
	match track {
		Some(id) => format!("track {id}"),
		None => "the master channel".into(),
	}
}

/// Full consistency check of a whole project (used when loading files).
pub(crate) fn project(project: &Project) -> Result<(), ModelError> {
	name("project", &project.name)?;
	time_signature(project.time_signature)?;
	let tempo_points = project.tempo.points();
	match tempo_points.first() {
		Some(first) if first.tick == Tick::ZERO => {}
		_ => {
			return Err(ModelError::invalid(
				"tempo map",
				"",
				"the first tempo point must be at tick 0",
			));
		}
	}
	for w in tempo_points.windows(2) {
		if w[0].tick >= w[1].tick {
			return Err(ModelError::invalid(
				"tempo map",
				w[1].tick.0,
				"tempo points must be strictly increasing",
			));
		}
	}
	for p in tempo_points {
		tempo(p.bpm)?;
	}

	if project.master.output != Output::Master {
		return Err(ModelError::MasterRestriction(
			"must output to the hardware (output: master)",
		));
	}
	if !project.master.sends.is_empty() {
		return Err(ModelError::MasterRestriction("cannot have sends"));
	}
	channel_values(&project.master)?;

	// Rebuild the project track by track so id uniqueness is checked with the same rules
	// commands use.
	let mut partial = Project {
		tracks: Vec::new(),
		..project.clone()
	};
	partial.master.inserts.clear();
	for d in &project.master.inserts {
		if partial.device_id_in_use(d.id) {
			return Err(ModelError::DuplicateDeviceId(d.id));
		}
		partial.master.inserts.push(d.clone());
	}
	for t in &project.tracks {
		new_track_ids(&partial, t)?;
		partial.tracks.push(t.clone());
	}
	for t in &project.tracks {
		track(project, t)?;
	}
	for t in &project.tracks {
		for target in targets(&t.channel) {
			if reaches(project, target, t.id) {
				return Err(ModelError::RoutingCycle {
					from: t.id,
					to: target,
				});
			}
		}
	}

	let mut addresses = HashSet::new();
	for lane in &project.automation {
		address(project, &lane.address)?;
		automation_points(&lane.address, &lane.points)?;
		if !addresses.insert(lane.address.to_string()) {
			return Err(ModelError::InvalidAutomation {
				address: lane.address.clone(),
				reason: "the project has two lanes for this address".into(),
			});
		}
	}
	let mut marker_ticks = HashSet::new();
	for m in &project.markers {
		start("marker", m.tick)?;
		if !marker_ticks.insert(m.tick) {
			return Err(ModelError::DuplicateMarker(m.tick));
		}
	}
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn time_signatures() {
		let ts = |numerator, denominator| TimeSignature {
			numerator,
			denominator,
		};
		assert!(time_signature(ts(4, 4)).is_ok());
		assert!(time_signature(ts(7, 8)).is_ok());
		assert!(time_signature(ts(32, 32)).is_ok());
		assert!(time_signature(ts(1, 1)).is_ok());
		assert!(time_signature(ts(3, 6)).is_err());
		assert!(time_signature(ts(0, 4)).is_err());
		assert!(time_signature(ts(33, 4)).is_err());
		assert!(time_signature(ts(4, 64)).is_err());
	}

	#[test]
	fn ranges() {
		assert!(volume(12.0).is_ok());
		assert!(volume(-144.0).is_ok());
		assert!(volume(12.1).is_err());
		assert!(volume(f32::NAN).is_err());
		assert!(pan(-1.0).is_ok());
		assert!(pan(1.01).is_err());
		assert!(velocity(0.0).is_ok());
		assert!(velocity(1.5).is_err());
		assert!(pitch(127).is_ok());
		assert!(pitch(128).is_err());
		assert!(tempo(20.0).is_ok());
		assert!(tempo(1000.0).is_err());
		assert!(color("#a1B2c3").is_ok());
		assert!(color("red").is_err());
		assert!(color("#12345g").is_err());
	}
}
