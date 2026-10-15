//! Drag and drop, without egui: what is dragged ([`Payload`]), where it is over
//! ([`DropTarget`]) and what dropping it there does ([`resolve`] -> [`DropAction`]).
//! The views only find the target under the pointer, show [`DropAction::describe`] as
//! the drop hint and call [`super::AppState::drop_payload`] on release.

use std::path::PathBuf;

use cadenza_core::{DeviceId, Tick, TrackId};
use cadenza_dsp::{DeviceRole, FactoryPreset};
use cadenza_model::{DeviceKind, Project, TrackKind};

use super::browser::BrowserItem;
use super::files::{FileKind, classify};
use super::selection::DeviceRef;

/// Something being dragged inside the app (or a file from the OS file manager).
#[derive(Debug, Clone, PartialEq)]
pub enum Payload {
	Device(BrowserItem),
	/// A built-in factory preset of device `device` (built-in id).
	Preset {
		device: String,
		preset: &'static FactoryPreset,
	},
	File(PathBuf),
	/// An insert of a mixer channel (`track: None` = master).
	Insert {
		track: Option<TrackId>,
		device: DeviceId,
	},
	/// A style library pattern (`id` = "core/dembow_basic").
	Pattern {
		id: String,
		name: String,
	},
}

impl Payload {
	/// Short label for the drag ghost.
	pub fn label(&self) -> String {
		match self {
			Payload::Device(item) => item.name.clone(),
			Payload::Preset { preset, .. } => preset.name.to_owned(),
			Payload::File(path) => path.file_name().map_or_else(
				|| path.display().to_string(),
				|f| f.to_string_lossy().into(),
			),
			Payload::Insert { .. } => "Insert".into(),
			Payload::Pattern { name, .. } => name.clone(),
		}
	}
}

/// Where the pointer is.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DropTarget {
	/// A track header (no time position).
	Track(TrackId),
	/// A track's clip lane at a song position.
	Lane { track: TrackId, at: Tick },
	/// The empty area below the last track, at a song position.
	Below { at: Tick },
	/// The same area with Alt held: audio files become a new sampler track.
	BelowSampler { at: Tick },
	/// A pad of a drum_sampler's editor (its note).
	Pad { device: DeviceRef, note: u8 },
	/// A sampler editor's zone map: a zone (replace its sample) or the keyboard
	/// (`None`: map the file across it).
	Zone {
		device: DeviceRef,
		zone: Option<usize>,
	},
	/// A mixer strip (`None` = master).
	Strip(Option<TrackId>),
	/// Between two inserts of a mixer strip: `slot` 0..=len, before the insert there.
	InsertSlot { track: Option<TrackId>, slot: usize },
	/// A device (instrument or insert) shown somewhere.
	Device(DeviceRef),
	/// Anywhere else in the window (OS drops without a better target).
	Window,
}

impl DropTarget {
	pub fn track(&self) -> Option<TrackId> {
		match *self {
			DropTarget::Track(t) | DropTarget::Lane { track: t, .. } => Some(t),
			DropTarget::Strip(t) | DropTarget::InsertSlot { track: t, .. } => t,
			DropTarget::Device(d)
			| DropTarget::Pad { device: d, .. }
			| DropTarget::Zone { device: d, .. } => d.track,
			DropTarget::Below { .. } | DropTarget::BelowSampler { .. } | DropTarget::Window => None,
		}
	}

	pub fn at(&self) -> Option<Tick> {
		match *self {
			DropTarget::Lane { at, .. }
			| DropTarget::Below { at }
			| DropTarget::BelowSampler { at } => Some(at),
			_ => None,
		}
	}
}

/// The edit a drop performs.
#[derive(Debug, Clone, PartialEq)]
pub enum DropAction {
	SetInstrument {
		track: TrackId,
		item: BrowserItem,
	},
	InsertEffect {
		track: Option<TrackId>,
		item: BrowserItem,
		index: Option<usize>,
	},
	/// New track below `after` (or at the end): an instrument track with the instrument,
	/// or a bus with the effect.
	NewTrack {
		item: BrowserItem,
		after: Option<TrackId>,
	},
	LoadPreset {
		device: DeviceRef,
		preset: &'static FactoryPreset,
	},
	/// A device of that kind with the preset loaded, on `track`.
	DeviceWithPreset {
		track: TrackId,
		device: String,
		preset: &'static FactoryPreset,
	},
	MoveInsert {
		from: Option<TrackId>,
		device: DeviceId,
		to: Option<TrackId>,
		slot: usize,
		copy: bool,
	},
	/// `track: None` = a new audio track; `at: None` = at the playhead.
	ImportAudio {
		path: PathBuf,
		track: Option<TrackId>,
		at: Option<Tick>,
	},
	/// `track: None` = new instrument tracks; `at: None` = at the playhead.
	ImportMidi {
		path: PathBuf,
		track: Option<TrackId>,
		at: Option<Tick>,
	},
	OpenProject(PathBuf),
	/// An audio file into a sampler / drum_sampler.
	LoadSample {
		device: DeviceRef,
		path: PathBuf,
		slot: super::samples::SampleSlot,
	},
	/// A new instrument track with a sampler playing the file across the keyboard.
	NewSampler {
		path: PathBuf,
	},
	/// A library pattern written on `track` (`None`: a new track) from the bar of `at`
	/// (`None`: the playhead).
	AddPattern {
		pattern: String,
		name: String,
		track: Option<TrackId>,
		at: Option<Tick>,
	},
	/// Not possible here; the text says why (shown as the drop hint).
	Reject(String),
}

impl DropAction {
	pub fn accepted(&self) -> bool {
		!matches!(self, DropAction::Reject(_))
	}

	/// Hint shown next to the pointer while hovering.
	pub fn describe(&self, project: &Project) -> String {
		let track = |t: &TrackId| {
			project
				.track(*t)
				.map_or_else(|| "?".to_owned(), |t| format!("'{}'", t.name))
		};
		let channel = |t: &Option<TrackId>| t.as_ref().map_or_else(|| "Master".into(), track);
		match self {
			DropAction::SetInstrument { track: t, item } => {
				format!("Set {} as the instrument of {}", item.name, track(t))
			}
			DropAction::InsertEffect {
				track: t,
				item,
				index: Some(i),
			} => format!("Insert {} on {} at slot {}", item.name, channel(t), i + 1),
			DropAction::InsertEffect { track: t, item, .. } => {
				format!("Insert {} on {}", item.name, channel(t))
			}
			DropAction::NewTrack { item, .. } => match item.role {
				DeviceRole::Instrument => format!("New instrument track with {}", item.name),
				DeviceRole::Effect => format!("New bus with {}", item.name),
			},
			DropAction::LoadPreset { preset, .. } => format!("Load preset '{}'", preset.name),
			DropAction::DeviceWithPreset {
				track: t, preset, ..
			} => format!("Add '{}' to {}", preset.name, track(t)),
			DropAction::MoveInsert { to, copy, slot, .. } => format!(
				"{} to {} slot {}",
				if *copy { "Copy" } else { "Move" },
				channel(to),
				slot + 1
			),
			DropAction::ImportAudio { track: t, at, .. } => format!(
				"Import audio {}{}",
				t.as_ref().map_or_else(
					|| "on a new track".to_owned(),
					|t| format!("on {}", track(t))
				),
				at.map_or_else(
					|| " at the playhead".to_owned(),
					|a| format!(" at beat {:.2}", a.beats() + 1.0)
				)
			),
			DropAction::ImportMidi { track: t, at, .. } => format!(
				"Import MIDI {}{}",
				t.as_ref().map_or_else(
					|| "as new tracks".to_owned(),
					|t| format!("into {}", track(t))
				),
				at.map_or_else(String::new, |a| format!(" at beat {:.2}", a.beats() + 1.0))
			),
			DropAction::OpenProject(p) => format!(
				"Open {}",
				p.file_name().unwrap_or_default().to_string_lossy()
			),
			DropAction::LoadSample { path, slot, .. } => {
				let name = path.file_name().unwrap_or_default().to_string_lossy();
				match slot {
					super::samples::SampleSlot::Pad(n) => {
						format!("Load {name} into pad {}", super::samples::pad_label(*n))
					}
					super::samples::SampleSlot::Zone(i) => {
						format!("Replace zone {} with {name}", i + 1)
					}
					super::samples::SampleSlot::Keyboard => {
						format!("Map {name} across the keyboard")
					}
				}
			}
			DropAction::NewSampler { path } => format!(
				"New sampler track with {}",
				path.file_name().unwrap_or_default().to_string_lossy()
			),
			DropAction::AddPattern {
				name, track: t, at, ..
			} => format!(
				"Write '{name}' {}{}",
				t.as_ref().map_or_else(
					|| "on a new track".to_owned(),
					|t| format!("on {}", track(t))
				),
				at.map_or_else(
					|| " at the playhead's bar".to_owned(),
					|a| format!(
						" from bar {}",
						project.meter().bar_position(a).floor() + 1.0
					)
				)
			),
			DropAction::Reject(why) => why.clone(),
		}
	}
}

fn kind_of(project: &Project, track: TrackId) -> Option<TrackKind> {
	project.track(track).map(|t| t.kind)
}

/// Built-in device id of a device.
fn builtin_id(device: &cadenza_model::Device) -> Option<&str> {
	match &device.kind {
		DeviceKind::Builtin { id } => Some(id),
		_ => None,
	}
}

/// What dropping `payload` on `target` does. `copy`: Ctrl held (inserts are copied).
pub fn resolve(project: &Project, payload: &Payload, target: DropTarget, copy: bool) -> DropAction {
	match payload {
		Payload::Device(item) => resolve_device(project, item, target),
		Payload::Preset { device, preset } => resolve_preset(project, device, preset, target),
		Payload::Insert { track, device } => match target {
			DropTarget::InsertSlot { track: to, slot } => DropAction::MoveInsert {
				from: *track,
				device: *device,
				to,
				slot,
				copy,
			},
			DropTarget::Strip(to) => DropAction::MoveInsert {
				from: *track,
				device: *device,
				to,
				slot: project.channel(to).map_or(0, |c| c.inserts.len()),
				copy,
			},
			_ => DropAction::Reject("Drop inserts on a mixer strip".into()),
		},
		Payload::File(path) => resolve_file(project, path, target),
		Payload::Pattern { id, name } => {
			let add = |track, at| DropAction::AddPattern {
				pattern: id.clone(),
				name: name.clone(),
				track,
				at,
			};
			match target {
				DropTarget::Below { at } | DropTarget::BelowSampler { at } => add(None, Some(at)),
				DropTarget::Track(t) | DropTarget::Lane { track: t, .. }
					if kind_of(project, t) == Some(TrackKind::Instrument) =>
				{
					add(Some(t), target.at())
				}
				_ => DropAction::Reject(
					"Drop patterns on an instrument track, or below the tracks for a new one"
						.into(),
				),
			}
		}
	}
}

fn resolve_device(project: &Project, item: &BrowserItem, target: DropTarget) -> DropAction {
	let new_track = |after| DropAction::NewTrack {
		item: item.clone(),
		after,
	};
	match item.role {
		DeviceRole::Instrument => match target {
			DropTarget::Below { .. } | DropTarget::BelowSampler { .. } => new_track(None),
			DropTarget::Track(t)
			| DropTarget::Lane { track: t, .. }
			| DropTarget::Strip(Some(t))
			| DropTarget::Device(DeviceRef { track: Some(t), .. }) => {
				if kind_of(project, t) == Some(TrackKind::Instrument) {
					DropAction::SetInstrument {
						track: t,
						item: item.clone(),
					}
				} else {
					new_track(Some(t))
				}
			}
			_ => DropAction::Reject(
				"Instruments go on an instrument track, or below the tracks for a new one".into(),
			),
		},
		DeviceRole::Effect => match target {
			DropTarget::Below { .. } | DropTarget::BelowSampler { .. } => new_track(None),
			DropTarget::InsertSlot { track, slot } => DropAction::InsertEffect {
				track,
				item: item.clone(),
				index: Some(slot),
			},
			DropTarget::Track(t) | DropTarget::Lane { track: t, .. } => DropAction::InsertEffect {
				track: Some(t),
				item: item.clone(),
				index: None,
			},
			DropTarget::Strip(track) | DropTarget::Device(DeviceRef { track, .. }) => {
				DropAction::InsertEffect {
					track,
					item: item.clone(),
					index: None,
				}
			}
			DropTarget::Window | DropTarget::Pad { .. } | DropTarget::Zone { .. } => {
				DropAction::Reject("Drop effects on a track or a mixer strip".into())
			}
		},
	}
}

fn resolve_preset(
	project: &Project,
	device: &str,
	preset: &'static FactoryPreset,
	target: DropTarget,
) -> DropAction {
	if let DropTarget::Device(r) = target {
		let d = match r.track {
			Some(t) => project.track(t).and_then(|t| t.device(r.device)),
			None => project.master.inserts.iter().find(|d| d.id == r.device),
		};
		return match d.and_then(builtin_id) {
			Some(id) if id == device => DropAction::LoadPreset { device: r, preset },
			_ => DropAction::Reject(format!("'{}' is a preset of another device", preset.name)),
		};
	}
	let Some(track) = target.track().and_then(|t| project.track(t)) else {
		return DropAction::Reject("Drop presets on a device or a track".into());
	};
	let existing = track
		.instrument
		.iter()
		.chain(track.channel.inserts.iter())
		.find(|d| builtin_id(d) == Some(device));
	if let Some(d) = existing {
		return DropAction::LoadPreset {
			device: DeviceRef {
				track: Some(track.id),
				device: d.id,
			},
			preset,
		};
	}
	let role = cadenza_dsp::registry()
		.iter()
		.find(|i| i.id == device)
		.map(|i| i.role);
	match role {
		Some(DeviceRole::Instrument) if track.kind != TrackKind::Instrument => {
			DropAction::Reject(format!("'{}' needs an instrument track", preset.name))
		}
		Some(_) => DropAction::DeviceWithPreset {
			track: track.id,
			device: device.to_owned(),
			preset,
		},
		None => DropAction::Reject(format!("Unknown device '{device}'")),
	}
}

fn resolve_file(project: &Project, path: &std::path::Path, target: DropTarget) -> DropAction {
	let Some(kind) = classify(path) else {
		return DropAction::Reject(format!(
			"Cadenza cannot use {}",
			path.file_name().unwrap_or_default().to_string_lossy()
		));
	};
	let path = path.to_path_buf();
	let track_kind = target.track().and_then(|t| kind_of(project, t));
	let sample_target = match target {
		DropTarget::Pad { device, note } => Some((device, super::samples::SampleSlot::Pad(note))),
		DropTarget::Zone { device, zone } => Some((
			device,
			zone.map_or(
				super::samples::SampleSlot::Keyboard,
				super::samples::SampleSlot::Zone,
			),
		)),
		_ => None,
	};
	if let Some((device, slot)) = sample_target {
		return match kind {
			FileKind::Audio => DropAction::LoadSample { device, path, slot },
			_ => DropAction::Reject("Drop audio files on pads and zones".into()),
		};
	}
	if let (FileKind::Audio, DropTarget::BelowSampler { .. }) = (kind, target) {
		return DropAction::NewSampler { path };
	}
	if let (FileKind::Audio, DropTarget::Device(r)) = (kind, target) {
		let d = match r.track {
			Some(t) => project.track(t).and_then(|t| t.device(r.device)),
			None => None,
		};
		match d.and_then(super::samples::kind) {
			Some(super::samples::SampleKind::Sampler) => {
				return DropAction::LoadSample {
					device: r,
					path,
					slot: super::samples::SampleSlot::Keyboard,
				};
			}
			Some(super::samples::SampleKind::Drums) => {
				return DropAction::Reject(
					"Drop samples on a pad of the drum sampler editor".into(),
				);
			}
			None => {}
		}
	}
	match kind {
		FileKind::Project => DropAction::OpenProject(path),
		FileKind::Audio => DropAction::ImportAudio {
			path,
			track: target
				.track()
				.filter(|_| track_kind == Some(TrackKind::Audio)),
			at: target.at(),
		},
		FileKind::Midi => DropAction::ImportMidi {
			path,
			track: target
				.track()
				.filter(|_| track_kind == Some(TrackKind::Instrument)),
			at: target.at(),
		},
		FileKind::Folder => DropAction::Reject("Folders cannot be dropped".into()),
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use cadenza_model::{Device, Track};

	struct Fx {
		p: Project,
		keys: TrackId,
		audio: TrackId,
		eq: DeviceId,
	}

	fn fixture() -> Fx {
		let mut p = Project::new("p");
		let mut keys = Track::new("Keys", TrackKind::Instrument);
		keys.instrument = Some(Device::builtin("poly_synth", "Synth"));
		let eq = Device::builtin("compressor", "Comp");
		keys.channel.inserts.push(eq.clone());
		let audio = Track::new("Vox", TrackKind::Audio);
		let ids = (keys.id, audio.id);
		p.tracks.push(keys);
		p.tracks.push(audio);
		Fx {
			p,
			keys: ids.0,
			audio: ids.1,
			eq: eq.id,
		}
	}

	fn item(id: &str) -> BrowserItem {
		super::super::browser::items(None)
			.into_iter()
			.find(|i| i.id == id)
			.unwrap()
	}

	#[test]
	fn devices_resolve_by_role_and_track_kind() {
		let fx = fixture();
		let synth = Payload::Device(item("poly_synth"));
		let verb = Payload::Device(item("reverb"));
		let at = Tick(960);
		assert!(matches!(
			resolve(&fx.p, &synth, DropTarget::Track(fx.keys), false),
			DropAction::SetInstrument { track, .. } if track == fx.keys
		));
		assert!(matches!(
			resolve(&fx.p, &synth, DropTarget::Lane { track: fx.audio, at }, false),
			DropAction::NewTrack { after: Some(t), .. } if t == fx.audio
		));
		assert!(matches!(
			resolve(&fx.p, &synth, DropTarget::Below { at }, false),
			DropAction::NewTrack { after: None, .. }
		));
		assert!(!resolve(&fx.p, &synth, DropTarget::Strip(None), false).accepted());
		assert!(matches!(
			resolve(
				&fx.p,
				&verb,
				DropTarget::InsertSlot {
					track: None,
					slot: 0
				},
				false
			),
			DropAction::InsertEffect {
				track: None,
				index: Some(0),
				..
			}
		));
		assert!(matches!(
			resolve(&fx.p, &verb, DropTarget::Lane { track: fx.audio, at }, false),
			DropAction::InsertEffect { track: Some(t), index: None, .. } if t == fx.audio
		));
		let below = resolve(&fx.p, &verb, DropTarget::Below { at }, false);
		assert_eq!(below.describe(&fx.p), "New bus with Reverb");
	}

	#[test]
	fn inserts_move_or_copy_onto_strips() {
		let fx = fixture();
		let ins = Payload::Insert {
			track: Some(fx.keys),
			device: fx.eq,
		};
		assert_eq!(
			resolve(&fx.p, &ins, DropTarget::Strip(Some(fx.keys)), true),
			DropAction::MoveInsert {
				from: Some(fx.keys),
				device: fx.eq,
				to: Some(fx.keys),
				slot: 1,
				copy: true
			}
		);
		assert!(!resolve(&fx.p, &ins, DropTarget::Track(fx.keys), false).accepted());
	}

	#[test]
	fn presets_load_into_matching_devices_or_add_one() {
		let fx = fixture();
		let eq_preset = &cadenza_dsp::factory_presets("compressor")[0];
		let p = Payload::Preset {
			device: "compressor".into(),
			preset: eq_preset,
		};
		let r = DeviceRef {
			track: Some(fx.keys),
			device: fx.eq,
		};
		assert_eq!(
			resolve(&fx.p, &p, DropTarget::Device(r), false),
			DropAction::LoadPreset {
				device: r,
				preset: eq_preset
			}
		);
		assert_eq!(
			resolve(&fx.p, &p, DropTarget::Track(fx.keys), false),
			DropAction::LoadPreset {
				device: r,
				preset: eq_preset
			},
			"the track's compressor"
		);
		assert!(matches!(
			resolve(&fx.p, &p, DropTarget::Track(fx.audio), false),
			DropAction::DeviceWithPreset { track, .. } if track == fx.audio
		));
		let synth = Payload::Preset {
			device: "poly_synth".into(),
			preset: &cadenza_dsp::factory_presets("poly_synth")[0],
		};
		assert!(!resolve(&fx.p, &synth, DropTarget::Track(fx.audio), false).accepted());
		assert!(
			!resolve(&fx.p, &synth, DropTarget::Device(r), false).accepted(),
			"a synth preset on a compressor"
		);
	}

	#[test]
	fn patterns_go_on_instrument_tracks_or_a_new_one() {
		let fx = fixture();
		let at = Tick(1920);
		let p = Payload::Pattern {
			id: "core/dembow_basic".into(),
			name: "Dembow basic".into(),
		};
		let on_keys = resolve(&fx.p, &p, DropTarget::Lane { track: fx.keys, at }, false);
		assert_eq!(
			on_keys,
			DropAction::AddPattern {
				pattern: "core/dembow_basic".into(),
				name: "Dembow basic".into(),
				track: Some(fx.keys),
				at: Some(at)
			}
		);
		assert_eq!(
			on_keys.describe(&fx.p),
			"Write 'Dembow basic' on 'Keys' from bar 1"
		);
		assert!(matches!(
			resolve(&fx.p, &p, DropTarget::Below { at }, false),
			DropAction::AddPattern { track: None, .. }
		));
		assert!(!resolve(&fx.p, &p, DropTarget::Track(fx.audio), false).accepted());
		assert!(!resolve(&fx.p, &p, DropTarget::Strip(None), false).accepted());
	}

	#[test]
	fn files_import_where_they_fit() {
		let fx = fixture();
		let at = Tick(1920);
		let wav = Payload::File("/x/kick.wav".into());
		let mid = Payload::File("/x/riff.mid".into());
		let song = Payload::File("/x/song.cadenza.json".into());
		assert_eq!(
			resolve(
				&fx.p,
				&wav,
				DropTarget::Lane {
					track: fx.audio,
					at
				},
				false
			),
			DropAction::ImportAudio {
				path: "/x/kick.wav".into(),
				track: Some(fx.audio),
				at: Some(at)
			}
		);
		assert_eq!(
			resolve(&fx.p, &wav, DropTarget::Lane { track: fx.keys, at }, false),
			DropAction::ImportAudio {
				path: "/x/kick.wav".into(),
				track: None,
				at: Some(at)
			},
			"audio on a MIDI lane: a new audio track"
		);
		assert_eq!(
			resolve(&fx.p, &mid, DropTarget::Lane { track: fx.keys, at }, false),
			DropAction::ImportMidi {
				path: "/x/riff.mid".into(),
				track: Some(fx.keys),
				at: Some(at)
			}
		);
		assert_eq!(
			resolve(&fx.p, &mid, DropTarget::Below { at }, false),
			DropAction::ImportMidi {
				path: "/x/riff.mid".into(),
				track: None,
				at: Some(at)
			}
		);
		assert_eq!(
			resolve(&fx.p, &song, DropTarget::Window, false),
			DropAction::OpenProject("/x/song.cadenza.json".into())
		);
		assert!(
			!resolve(
				&fx.p,
				&Payload::File("/x/a.txt".into()),
				DropTarget::Window,
				false
			)
			.accepted()
		);
	}
}
