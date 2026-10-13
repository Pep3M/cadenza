//! Validation rules shared by command application and project loading.

use std::collections::HashSet;

use cadenza_core::{
	ClipId, DeviceId, ParamAddress, ParamTarget, TempoMap, Tick, TimeSignature, TrackId,
};

use crate::error::ModelError;
use crate::project::{
	AutomationPoint, Channel, Clip, ClipContent, Device, Note, Output, OutputRoute, Project,
	ProjectMeta, SidechainSource, Track, TrackKind,
};

/// Highest extra output port an instrument route may use.
pub const MAX_AUX_PORT: u32 = 64;

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

/// Most events one controller lane holds.
pub(crate) const MAX_CONTROLLER_POINTS: usize = 100_000;

/// Controller lane points: ticks >= 0, values in the controller's range.
pub(crate) fn controller_points(
	controller: crate::controller::Controller,
	points: &[crate::controller::ControllerPoint],
) -> Result<(), ModelError> {
	if points.len() > MAX_CONTROLLER_POINTS {
		return Err(ModelError::invalid(
			format!("{controller} points"),
			points.len().to_string(),
			"a controller lane holds at most 100000 events",
		));
	}
	let (lo, hi) = controller.range();
	for p in points {
		if p.tick.0 < 0 {
			return Err(ModelError::invalid(
				format!("{controller} point tick"),
				p.tick.0.to_string(),
				"controller events are clip-relative and start at tick >= 0",
			));
		}
		if !(lo..=hi).contains(&p.value) {
			return Err(ModelError::invalid(
				format!("{controller} value"),
				p.value.to_string(),
				if lo < 0.0 {
					"pitch bend values are -1.0 (down) ..= 1.0 (up), 0 = center"
				} else {
					"controller values are 0.0..=1.0 (MIDI 0..127; sustain on >= 0.5)"
				},
			));
		}
	}
	Ok(())
}

/// Most time signature changes a project holds.
pub(crate) const MAX_METER_CHANGES: usize = 1024;
/// Highest bar index a time signature change may start at.
const MAX_METER_BAR: u32 = 100_000;

/// A bar index where a time signature change may start (bar 1 is the song's first
/// signature, not a change).
pub(crate) fn meter_bar(bar: u32) -> Result<(), ModelError> {
	if (1..=MAX_METER_BAR).contains(&bar) {
		Ok(())
	} else {
		Err(ModelError::invalid(
			"time signature change bar index",
			bar.to_string(),
			"changes start at bar index 1..=100000 (0-based; index 0 is bar 1, the song's first signature)",
		))
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

pub(crate) fn meta(meta: &ProjectMeta) -> Result<(), ModelError> {
	if let Some(style) = &meta.style {
		let ok =
			!style.trim().is_empty() && style.len() <= 200 && !style.chars().any(char::is_control);
		if !ok {
			return Err(ModelError::invalid(
				"song style",
				format!("{style:?}"),
				"a library style id like \"core/latin_pop\" (1-200 chars)",
			));
		}
	}
	Ok(())
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
	params(device.id, &device.params)?;
	if !device.zones.is_empty() && device.is_plugin() {
		return Err(ModelError::invalid(
			format!("zones of device {}", device.id),
			format!("{} zones", device.zones.len()),
			"only the built-in sampler and drum_sampler play sample zones",
		));
	}
	crate::sample::validate_zones(&device.zones)
}

/// Explicit parameter values of device `id`.
pub(crate) fn params(
	id: DeviceId,
	params: &std::collections::BTreeMap<String, f32>,
) -> Result<(), ModelError> {
	let device = Device {
		id,
		..Device::builtin("", "")
	};
	for (param, value) in params {
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
/// Highest take lane (keeps the takes view bounded).
pub const MAX_TAKE_LANE: u32 = 999;

/// Highest audio input channel index a track can use (0-based).
pub const MAX_INPUT_CHANNEL: u16 = 63;

pub(crate) fn take(take: &crate::project::Take) -> Result<(), ModelError> {
	if take.lane > MAX_TAKE_LANE {
		return Err(ModelError::invalid(
			"take lane",
			take.lane,
			format!("use 0..={MAX_TAKE_LANE}"),
		));
	}
	Ok(())
}

/// Inputs belong to audio tracks and must name existing-looking channels.
pub(crate) fn track_input(
	track: TrackId,
	kind: TrackKind,
	input: Option<&crate::project::TrackInput>,
) -> Result<(), ModelError> {
	let Some(input) = input else {
		return Ok(());
	};
	if kind != TrackKind::Audio {
		return Err(ModelError::invalid(
			format!("input of track {track}"),
			input.label(),
			format!("only Audio tracks record audio input; this is a {kind:?} track"),
		));
	}
	let last = input.channel.saturating_add(u16::from(input.stereo));
	if last > MAX_INPUT_CHANNEL {
		return Err(ModelError::invalid(
			format!("input of track {track}"),
			input.label(),
			format!(
				"input channels are 0..={MAX_INPUT_CHANNEL} (a stereo pair uses channel and channel + 1)"
			),
		));
	}
	Ok(())
}

pub(crate) fn clip(clip: &Clip) -> Result<(), ModelError> {
	start("clip", clip.start)?;
	length("clip", clip.length)?;
	if let Some(t) = &clip.take {
		take(t)?;
	}
	match &clip.content {
		ClipContent::Midi { notes, controllers } => {
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
			for (i, lane) in controllers.iter().enumerate() {
				controller_points(lane.controller, &lane.points)?;
				if controllers[..i]
					.iter()
					.any(|l| l.controller == lane.controller)
				{
					return Err(ModelError::invalid(
						"controller lanes",
						lane.controller.to_string(),
						"a clip holds one lane per controller",
					));
				}
			}
		}
		ClipContent::Audio {
			file,
			offset_seconds,
			gain_db,
			fade_in,
			fade_out,
		} => {
			for (what, fade) in [("fade_in", fade_in), ("fade_out", fade_out)] {
				if let Some(f) = fade
					&& !(f.seconds.is_finite() && f.seconds > 0.0)
				{
					return Err(ModelError::invalid(
						format!("audio clip {what} seconds"),
						f.seconds,
						"must be > 0 (use null for no fade)",
					));
				}
			}
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

/// Checks that an audio clip's fades fit in its duration under `tempo`.
pub(crate) fn clip_fades(clip: &Clip, tempo: &TempoMap) -> Result<(), ModelError> {
	let (fade_in, fade_out) = clip.fades();
	let duration = clip.duration_seconds(tempo);
	let fin = fade_in.map_or(0.0, |f| f.seconds);
	let fout = fade_out.map_or(0.0, |f| f.seconds);
	// A microsecond of slack absorbs rounding when a fade is meant to span the clip.
	const EPS: f64 = 1e-6;
	for (what, len) in [("fade_in", fin), ("fade_out", fout)] {
		if len > duration + EPS {
			return Err(ModelError::invalid(
				format!("clip {} {what} seconds", clip.id),
				len,
				format!("the clip lasts {duration:.3} s; use a shorter fade or lengthen the clip"),
			));
		}
	}
	if fin + fout > duration + EPS {
		return Err(ModelError::invalid(
			format!("clip {} fades", clip.id),
			format!("{fin} s + {fout} s"),
			format!(
				"fade_in + fade_out must not exceed the clip duration ({duration:.3} s); shorten one of them"
			),
		));
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

/// Buses fed by the extra outputs of `track`'s instrument.
fn aux_targets(track: &Track) -> impl Iterator<Item = TrackId> + '_ {
	track
		.instrument
		.iter()
		.flat_map(|d| d.aux_outputs.iter().filter_map(|r| r.bus))
}

/// Tracks that need `track`'s signal before they can be processed: its output bus, send
/// targets, buses fed by its instrument's extra outputs, and tracks with a device keyed
/// by it.
fn consumers(project: &Project, track: &Track) -> Vec<TrackId> {
	let mut out: Vec<TrackId> = targets(&track.channel).chain(aux_targets(track)).collect();
	out.extend(
		project
			.tracks
			.iter()
			.filter(|t| {
				t.devices()
					.any(|d| d.sidechain.is_some_and(|s| s.track == track.id))
			})
			.map(|t| t.id),
	);
	out
}

/// Whether the signal leaving `from` can arrive at `to` (including `from == to`), through
/// outputs, sends, instrument output routes and sidechains.
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
			stack.extend(consumers(project, track));
		}
	}
	false
}

/// A sidechain on `device` sitting on `owner` (`None` = master inserts) keyed by `source`.
pub(crate) fn sidechain(
	project: &Project,
	owner: Option<TrackId>,
	device: DeviceId,
	source: SidechainSource,
) -> Result<(), ModelError> {
	if project.track(source.track).is_none() {
		return Err(ModelError::TrackNotFound(source.track));
	}
	if let Some(owner) = owner {
		if owner == source.track {
			return Err(ModelError::InvalidSidechain {
				device,
				reason:
					"a device cannot be keyed by its own track; pick another track (e.g. the kick)"
						.into(),
			});
		}
		if reaches(project, owner, source.track) {
			return Err(ModelError::InvalidSidechain {
				device,
				reason: format!(
					"track {owner} feeds track {} (through its output, sends, buses or other \
					 sidechains), so keying from it would create a feedback loop; key from a track \
					 that does not depend on this one",
					source.track
				),
			});
		}
	}
	Ok(())
}

/// Structural checks of instrument output routes (ports, duplicates, own track).
fn output_route_shape(track: TrackId, routes: &[OutputRoute]) -> Result<(), ModelError> {
	let fail = |reason: String| ModelError::InvalidOutputRoute { track, reason };
	let mut ports = HashSet::new();
	for r in routes {
		if r.port == 0 {
			return Err(fail(
				"port 0 is the main output and is always mixed into the track; extra outputs are \
				 numbered from 1"
					.into(),
			));
		}
		if r.port > MAX_AUX_PORT {
			return Err(fail(format!(
				"port {} is out of range; extra outputs are numbered 1..={MAX_AUX_PORT}",
				r.port
			)));
		}
		if !ports.insert(r.port) {
			return Err(fail(format!(
				"port {} is routed twice; give each extra output one destination",
				r.port
			)));
		}
		if r.bus == Some(track) {
			return Err(fail(
				"an output cannot be routed to its own track as a bus; use bus: null to mix it \
				 into the track"
					.into(),
			));
		}
	}
	Ok(())
}

/// Instrument output routes of `track` against the project (bus targets, cycles).
pub(crate) fn output_routes(
	project: &Project,
	track: TrackId,
	routes: &[OutputRoute],
) -> Result<(), ModelError> {
	output_route_shape(track, routes)?;
	for bus in routes.iter().filter_map(|r| r.bus) {
		let Some(b) = project.track(bus) else {
			return Err(ModelError::TrackNotFound(bus));
		};
		if b.kind != TrackKind::Bus {
			return Err(ModelError::NotABus {
				track: bus,
				kind: b.kind,
			});
		}
		if reaches(project, bus, track) {
			return Err(ModelError::RoutingCycle {
				from: track,
				to: bus,
			});
		}
	}
	Ok(())
}

/// Links from a device placed on `owner` (`None` = master inserts) to other tracks: its
/// sidechain and, for instruments, extra output routes.
pub(crate) fn device_links(
	project: &Project,
	owner: Option<TrackId>,
	device: &Device,
	instrument: bool,
) -> Result<(), ModelError> {
	if let Some(source) = device.sidechain {
		sidechain(project, owner, device.id, source)?;
	}
	if device.aux_outputs.is_empty() {
		return Ok(());
	}
	match owner {
		Some(track) if instrument => output_routes(project, track, &device.aux_outputs),
		_ => Err(ModelError::invalid(
			format!("aux_outputs of device {}", device.id),
			format!("{} route(s)", device.aux_outputs.len()),
			"only a track's instrument has extra outputs; remove aux_outputs from effects",
		)),
	}
}

/// Feedback loops a not-yet-added `track` would close through its devices' sidechains.
pub(crate) fn new_track_sidechains(project: &Project, track: &Track) -> Result<(), ModelError> {
	let feeds: Vec<TrackId> = targets(&track.channel).chain(aux_targets(track)).collect();
	for d in track.devices() {
		let Some(source) = d.sidechain else { continue };
		if feeds
			.iter()
			.any(|&c| c == source.track || reaches(project, c, source.track))
		{
			return Err(ModelError::InvalidSidechain {
				device: d.id,
				reason: format!(
					"track {} feeds track {} (through its output, sends or buses), so keying \
					 from it would create a feedback loop",
					track.id, source.track
				),
			});
		}
	}
	Ok(())
}

/// Checks a track's own content and the routes it declares. Does not check id uniqueness
/// against the project (see [`new_track_ids`]).
/// Folder tracks carry no audio: no devices or sends.
pub(crate) fn not_folder(
	project: &Project,
	track: Option<TrackId>,
	what: &str,
) -> Result<(), ModelError> {
	match track.and_then(|t| project.track(t)) {
		Some(t) if t.kind == TrackKind::Folder => Err(ModelError::invalid(
			format!("{what} on track {}", t.id),
			t.name.clone(),
			"folder tracks carry no audio (no devices or sends); put them on the tracks inside, or on a bus the tracks feed",
		)),
		_ => Ok(()),
	}
}

/// `folder` must be a folder track that is not `track` nor inside it.
pub(crate) fn track_folder(
	project: &Project,
	track: TrackId,
	folder: Option<TrackId>,
) -> Result<(), ModelError> {
	if project.track(track).is_none() {
		return Err(ModelError::TrackNotFound(track));
	}
	let Some(folder) = folder else {
		return Ok(());
	};
	let Some(f) = project.track(folder) else {
		return Err(ModelError::TrackNotFound(folder));
	};
	if f.kind != TrackKind::Folder {
		return Err(ModelError::invalid(
			"folder",
			folder.to_string(),
			"tracks can only be put into Folder tracks (create one with kind folder)",
		));
	}
	if folder == track || project.folders_of(folder).contains(&track) {
		return Err(ModelError::invalid(
			"folder",
			folder.to_string(),
			"a folder cannot be put inside itself or inside one of its own sub-folders",
		));
	}
	Ok(())
}

pub(crate) fn track(project: &Project, track: &Track) -> Result<(), ModelError> {
	name("track", &track.name)?;
	if let Some(folder) = track.folder {
		match project.track(folder) {
			Some(f) if f.kind == TrackKind::Folder => {}
			Some(_) => {
				return Err(ModelError::invalid(
					"folder",
					folder.to_string(),
					"tracks can only be put into Folder tracks",
				));
			}
			None => return Err(ModelError::TrackNotFound(folder)),
		}
	}
	if track.kind == TrackKind::Folder
		&& (!track.channel.inserts.is_empty() || !track.channel.sends.is_empty())
	{
		return Err(ModelError::invalid(
			format!("folder track {}", track.id),
			track.name.clone(),
			"folder tracks carry no audio: no devices or sends",
		));
	}
	track_input(track.id, track.kind, track.input.as_ref())?;
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
		output_route_shape(track.id, &instrument.aux_outputs)?;
		for bus in aux_targets(track) {
			match project.track(bus) {
				None => return Err(ModelError::TrackNotFound(bus)),
				Some(b) if b.kind != TrackKind::Bus => {
					return Err(ModelError::NotABus {
						track: bus,
						kind: b.kind,
					});
				}
				Some(_) => {}
			}
		}
	}
	for d in &track.channel.inserts {
		if !d.aux_outputs.is_empty() {
			return Err(ModelError::invalid(
				format!("aux_outputs of device {}", d.id),
				format!("{} route(s)", d.aux_outputs.len()),
				"only a track's instrument has extra outputs; remove aux_outputs from effects",
			));
		}
	}
	for d in track.devices() {
		if let Some(source) = d.sidechain {
			if source.track == track.id {
				return Err(ModelError::InvalidSidechain {
					device: d.id,
					reason: "a device cannot be keyed by its own track; pick another track (e.g. \
						 the kick)"
						.into(),
				});
			}
			if project.track(source.track).is_none() {
				return Err(ModelError::TrackNotFound(source.track));
			}
		}
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
	if let ParamTarget::Send { send } = &address.target {
		let Some(track) = address.track else {
			return Err(ModelError::MasterRestriction("cannot have sends"));
		};
		let exists = project
			.track(track)
			.is_some_and(|t| t.channel.sends.iter().any(|s| s.id == *send));
		if !exists {
			return Err(ModelError::SendNotFound { track, send: *send });
		}
	}
	Ok(())
}

/// Validates a value for `address` (dB range for volume and sends, -1..1 for pan, finite
/// otherwise).
pub(crate) fn param_value(address: &ParamAddress, value: f32) -> Result<(), ModelError> {
	match &address.target {
		ParamTarget::Volume => volume(value),
		ParamTarget::Pan => pan(value),
		ParamTarget::Device { param, .. } => finite(format!("parameter '{param}'"), value),
		ParamTarget::Send { .. } => volume(value),
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
	meta(&project.meta)?;
	time_signature(project.time_signature)?;
	for t in &project.tracks {
		if project.folders_of(t.id).contains(&t.id) {
			return Err(ModelError::invalid(
				"folder",
				t.id.to_string(),
				"folder tracks form a cycle; a folder cannot be inside itself",
			));
		}
	}
	if project.time_signature_changes.len() > MAX_METER_CHANGES {
		return Err(ModelError::invalid(
			"time signature changes",
			project.time_signature_changes.len().to_string(),
			"a project holds at most 1024 time signature changes",
		));
	}
	for c in &project.time_signature_changes {
		meter_bar(c.bar)?;
		time_signature(c.signature())?;
	}
	for w in project.time_signature_changes.windows(2) {
		if w[0].bar >= w[1].bar {
			return Err(ModelError::invalid(
				"time signature changes",
				format!("bar index {}", w[1].bar),
				"changes must be sorted by bar with at most one per bar",
			));
		}
	}
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
	for d in &project.master.inserts {
		device_links(project, None, d, false)?;
	}
	for t in &project.tracks {
		for target in consumers(project, t) {
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
