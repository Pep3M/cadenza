//! Files a project refers to (audio clips, sample zones) and the project folder layout.
//!
//! A portable project lives in its own folder:
//!
//! ```text
//! <Name>/<Name>.cadenza.json
//! <Name>/Audio/      recordings, imported and processed audio
//! <Name>/Samples/    sampler / drum sampler content
//! <Name>/Renders/    exports
//! <Name>/.cadenza/   caches and autosaves (safe to delete)
//! ```
//!
//! In memory every path is absolute. On disk, paths inside the folder that holds the
//! project file are stored relative to it (with `/` separators) and resolved again on load,
//! so the folder can be moved or shared; other paths stay absolute.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

use cadenza_core::{ClipId, DeviceId, TrackId};
use serde::{Deserialize, Serialize};

use crate::command::{AudioClipPatch, Command};
use crate::project::{ClipContent, Device, Project};

/// Audio clip files (recordings, imports, processed copies).
pub const AUDIO_DIR: &str = "Audio";
/// Sample zone files.
pub const SAMPLES_DIR: &str = "Samples";
/// Exports.
pub const RENDERS_DIR: &str = "Renders";
/// Caches and autosaves; never needed to open the project.
pub const CACHE_DIR: &str = ".cadenza";
/// Autosaves inside [`CACHE_DIR`].
pub const AUTOSAVE_DIR: &str = "autosave";
/// Project file suffix.
pub const PROJECT_SUFFIX: &str = ".cadenza.json";

/// The folders of a project saved at `file`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectLayout {
	pub file: PathBuf,
	pub root: PathBuf,
}

impl ProjectLayout {
	pub fn new(file: impl Into<PathBuf>) -> Self {
		let file = file.into();
		let root = file
			.parent()
			.map(Path::to_path_buf)
			.unwrap_or_else(|| PathBuf::from("."));
		Self { file, root }
	}

	/// `<parent>/<name>/<name>.cadenza.json` for a new project folder.
	pub fn in_new_folder(parent: &Path, name: &str) -> Self {
		let name = sanitize_file_name(name);
		Self::new(parent.join(&name).join(format!("{name}{PROJECT_SUFFIX}")))
	}

	pub fn audio_dir(&self) -> PathBuf {
		self.root.join(AUDIO_DIR)
	}

	pub fn samples_dir(&self) -> PathBuf {
		self.root.join(SAMPLES_DIR)
	}

	pub fn renders_dir(&self) -> PathBuf {
		self.root.join(RENDERS_DIR)
	}

	pub fn cache_dir(&self) -> PathBuf {
		self.root.join(CACHE_DIR)
	}

	pub fn autosave_dir(&self) -> PathBuf {
		self.cache_dir().join(AUTOSAVE_DIR)
	}

	/// The file name without `.cadenza.json` ("Song" for "Song.cadenza.json").
	pub fn stem(&self) -> String {
		project_stem(&self.file)
	}

	/// Whether `path` is inside the project folder.
	pub fn contains(&self, path: &Path) -> bool {
		path.starts_with(&self.root)
	}
}

/// "Song" for ".../Song.cadenza.json" (or any other extension).
pub fn project_stem(file: &Path) -> String {
	let name = file
		.file_name()
		.map(|n| n.to_string_lossy().into_owned())
		.unwrap_or_default();
	name.strip_suffix(PROJECT_SUFFIX)
		.or_else(|| name.strip_suffix(".json"))
		.map(str::to_owned)
		.unwrap_or_else(|| {
			Path::new(&name)
				.file_stem()
				.map(|s| s.to_string_lossy().into_owned())
				.unwrap_or(name.clone())
		})
}

/// `name` made safe as a single file name (separators and control characters replaced).
pub fn sanitize_file_name(name: &str) -> String {
	let cleaned: String = name
		.trim()
		.chars()
		.map(|c| match c {
			'/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
			c if c.is_control() => '_',
			c => c,
		})
		.collect();
	let cleaned = cleaned.trim_matches('.').trim().to_owned();
	if cleaned.is_empty() {
		"Untitled".into()
	} else {
		cleaned
	}
}

/// What uses a referenced file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FileUse {
	AudioClip {
		clip: ClipId,
	},
	SampleZone {
		/// `None` = a master insert.
		track: Option<TrackId>,
		device: DeviceId,
		zone: usize,
	},
}

/// One reference to a file from the project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRef {
	pub path: PathBuf,
	/// The file's original name (sample zones keep it across relocations).
	pub name: String,
	pub used_by: FileUse,
}

impl FileRef {
	pub fn is_sample(&self) -> bool {
		matches!(self.used_by, FileUse::SampleZone { .. })
	}
}

fn device_zone_refs(track: Option<TrackId>, device: &Device, out: &mut Vec<FileRef>) {
	for (i, z) in device.zones.iter().enumerate() {
		out.push(FileRef {
			path: z.sample.path.clone(),
			name: z.sample.display_name(),
			used_by: FileUse::SampleZone {
				track,
				device: device.id,
				zone: i,
			},
		});
	}
}

impl Project {
	/// Every file reference: audio clips, then sample zones (track order).
	pub fn file_refs(&self) -> Vec<FileRef> {
		let mut out = Vec::new();
		for track in &self.tracks {
			for clip in &track.clips {
				if let ClipContent::Audio { file, .. } = &clip.content {
					out.push(FileRef {
						path: file.clone(),
						name: file
							.file_name()
							.map(|n| n.to_string_lossy().into_owned())
							.unwrap_or_default(),
						used_by: FileUse::AudioClip { clip: clip.id },
					});
				}
			}
		}
		for track in &self.tracks {
			for device in track.instrument.iter().chain(&track.channel.inserts) {
				device_zone_refs(Some(track.id), device, &mut out);
			}
		}
		for device in &self.master.inserts {
			device_zone_refs(None, device, &mut out);
		}
		out
	}

	/// The commands that point every reference whose path `relink` maps somewhere else
	/// to the new path (one `UpdateAudioClip` per clip, one `SetSampleZones` per device;
	/// zones keep their original names). Empty when nothing changes.
	pub fn relink_commands(&self, relink: impl Fn(&Path) -> Option<PathBuf>) -> Vec<Command> {
		let mut commands = Vec::new();
		for track in &self.tracks {
			for clip in &track.clips {
				if let ClipContent::Audio { file, .. } = &clip.content
					&& let Some(new) = relink(file).filter(|n| n != file)
				{
					commands.push(Command::UpdateAudioClip {
						clip: clip.id,
						patch: AudioClipPatch {
							file: Some(new),
							..AudioClipPatch::default()
						},
					});
				}
			}
		}
		let mut zone_command = |track: Option<TrackId>, device: &Device| {
			let mut changed = false;
			let zones = device
				.zones
				.iter()
				.map(|z| {
					let mut z = z.clone();
					if let Some(new) = relink(&z.sample.path).filter(|n| *n != z.sample.path) {
						if z.sample.name.is_empty() {
							z.sample.name = z.sample.display_name();
						}
						z.sample.path = new;
						changed = true;
					}
					z
				})
				.collect();
			if changed {
				commands.push(Command::SetSampleZones {
					track,
					device: device.id,
					zones,
				});
			}
		};
		for track in &self.tracks {
			for device in track.instrument.iter().chain(&track.channel.inserts) {
				zone_command(Some(track.id), device);
			}
		}
		for device in &self.master.inserts {
			zone_command(None, device);
		}
		commands
	}

	/// Rewrites every referenced path in place (persistence only: relative <-> absolute).
	pub(crate) fn map_file_paths(&mut self, mut f: impl FnMut(&Path) -> Option<PathBuf>) {
		let mut zones = |device: &mut Device| {
			for z in &mut device.zones {
				if let Some(p) = f(&z.sample.path) {
					z.sample.path = p;
				}
			}
		};
		for track in &mut self.tracks {
			if let Some(d) = &mut track.instrument {
				zones(d);
			}
			for d in &mut track.channel.inserts {
				zones(d);
			}
		}
		for d in &mut self.master.inserts {
			zones(d);
		}
		for track in &mut self.tracks {
			for clip in &mut track.clips {
				if let ClipContent::Audio { file, .. } = &mut clip.content
					&& let Some(p) = f(file)
				{
					*file = p;
				}
			}
		}
	}

	/// The referenced files that do not exist on disk, deduplicated, in reference order.
	pub fn missing_files(&self) -> Vec<FileRef> {
		let mut seen = HashMap::new();
		self.file_refs()
			.into_iter()
			.filter(|r| !r.path.as_os_str().is_empty() && !r.path.exists())
			.filter(|r| seen.insert(r.path.clone(), ()).is_none())
			.collect()
	}
}

/// `path` relative to `base` with `/` separators when it lies inside `base`.
pub fn relativize(path: &Path, base: &Path) -> Option<PathBuf> {
	if !path.is_absolute() {
		return None;
	}
	let rel = path.strip_prefix(base).ok()?;
	if rel.as_os_str().is_empty() || rel.components().any(|c| !matches!(c, Component::Normal(_))) {
		return None;
	}
	let parts: Vec<String> = rel
		.components()
		.map(|c| c.as_os_str().to_string_lossy().into_owned())
		.collect();
	Some(PathBuf::from(parts.join("/")))
}

/// A stored path made absolute against `base` (relative ones only; `/` or the platform
/// separator both work).
pub fn absolutize(path: &Path, base: &Path) -> Option<PathBuf> {
	if path.is_absolute() || path.as_os_str().is_empty() {
		return None;
	}
	let text = path.to_string_lossy();
	let mut out = base.to_path_buf();
	for part in text
		.split(['/', '\\'])
		.filter(|p| !p.is_empty() && *p != ".")
	{
		out.push(part);
	}
	Some(out)
}

#[cfg(test)]
mod tests {
	use cadenza_core::Tick;

	use super::*;
	use crate::project::{Clip, Track, TrackKind};
	use crate::sample::{SampleRef, SampleZone};
	use crate::{Document, backup_path};

	/// A project with an audio clip inside the folder, one outside and a sampler zone.
	fn project(inside: &Path, outside: &Path, sample: &Path) -> Project {
		let mut p = Project::new("Song");
		let mut audio = Track::new("Vox", TrackKind::Audio);
		audio
			.clips
			.push(Clip::audio("take", Tick::ZERO, Tick(960 * 4), inside));
		audio
			.clips
			.push(Clip::audio("ext", Tick(960 * 4), Tick(960 * 4), outside));
		p.tracks.push(audio);
		let mut device = Device::builtin("sampler", "Sampler");
		device.zones = vec![SampleZone::full_keyboard(sample, 60)];
		p.tracks
			.push(Track::new("Keys", TrackKind::Instrument).with_instrument(device));
		p
	}

	fn audio_files(p: &Project) -> Vec<PathBuf> {
		p.file_refs().into_iter().map(|r| r.path).collect()
	}

	#[test]
	fn paths_inside_the_folder_are_stored_relative_and_survive_a_move() {
		let tmp = tempfile::tempdir().unwrap();
		let root = tmp.path().join("Song");
		let file = root.join("Song.cadenza.json");
		let inside = root.join("Audio").join("take 1.wav");
		let outside = tmp.path().join("elsewhere").join("loop.wav");
		let sample = root.join("Samples").join("piano C4.wav");
		let p = project(&inside, &outside, &sample);
		p.save_json(&file).unwrap();

		let json = std::fs::read_to_string(&file).unwrap();
		assert!(json.contains("\"Audio/take 1.wav\""), "{json}");
		assert!(json.contains("\"Samples/piano C4.wav\""));
		assert!(json.contains(&outside.display().to_string()));
		assert_eq!(Project::load_json(&file).unwrap(), p, "round trip");

		let moved = tmp.path().join("Moved");
		std::fs::rename(&root, &moved).unwrap();
		let loaded = Project::load_json(moved.join("Song.cadenza.json")).unwrap();
		assert_eq!(
			audio_files(&loaded),
			vec![
				moved.join("Audio").join("take 1.wav"),
				outside.clone(),
				moved.join("Samples").join("piano C4.wav"),
			]
		);
	}

	#[test]
	fn old_absolute_projects_still_load() {
		let tmp = tempfile::tempdir().unwrap();
		let file = tmp.path().join("old.cadenza.json");
		let a = tmp.path().join("a.wav");
		let p = project(&a, &a, &a);
		std::fs::write(&file, p.to_json_string().unwrap()).unwrap();
		assert_eq!(Project::load_json(&file).unwrap(), p);
	}

	#[test]
	fn saves_are_atomic_and_keep_a_backup() {
		let tmp = tempfile::tempdir().unwrap();
		let file = tmp.path().join("nested").join("Song.cadenza.json");
		let mut p = Project::new("First");
		p.save_json(&file).unwrap();
		assert!(!backup_path(&file).exists(), "nothing to back up yet");
		p.name = "Second".into();
		p.save_json(&file).unwrap();
		assert_eq!(Project::load_json(&file).unwrap().name, "Second");
		assert_eq!(
			Project::load_json(backup_path(&file)).unwrap().name,
			"First"
		);
		let leftovers: Vec<_> = std::fs::read_dir(file.parent().unwrap())
			.unwrap()
			.map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
			.filter(|n| n.contains(".tmp-"))
			.collect();
		assert!(leftovers.is_empty(), "{leftovers:?}");
	}

	#[test]
	fn relink_commands_rewrite_clips_and_zones_in_one_undoable_batch() {
		let a = PathBuf::from("/old/a.wav");
		let s = PathBuf::from("/old/s.wav");
		let p = project(&a, Path::new("/keep/b.wav"), &s);
		let commands = p.relink_commands(|path| {
			path.starts_with("/old")
				.then(|| Path::new("/new").join(path.file_name().unwrap()))
		});
		assert_eq!(commands.len(), 2);
		let mut doc = Document::new(p.clone());
		doc.execute(Command::Batch {
			label: "Relink".into(),
			commands,
		})
		.unwrap();
		assert_eq!(
			audio_files(doc.project()),
			vec![
				PathBuf::from("/new/a.wav"),
				PathBuf::from("/keep/b.wav"),
				PathBuf::from("/new/s.wav")
			]
		);
		let zone = &doc.project().tracks[1].instrument.as_ref().unwrap().zones[0];
		assert_eq!(zone.sample.name, "s.wav", "original name kept");
		doc.undo().unwrap();
		assert_eq!(doc.project(), &p);
	}

	#[test]
	fn layout_and_names() {
		let l = ProjectLayout::in_new_folder(Path::new("/music"), "My: Song");
		assert_eq!(
			l.file,
			PathBuf::from("/music/My_ Song/My_ Song.cadenza.json")
		);
		assert_eq!(l.audio_dir(), PathBuf::from("/music/My_ Song/Audio"));
		assert_eq!(l.stem(), "My_ Song");
		assert_eq!(
			relativize(Path::new("/a/b/c.wav"), Path::new("/a")),
			Some(PathBuf::from("b/c.wav"))
		);
		assert_eq!(relativize(Path::new("/x/c.wav"), Path::new("/a")), None);
		assert_eq!(
			absolutize(Path::new("b/c.wav"), Path::new("/a")),
			Some(PathBuf::from("/a/b/c.wav"))
		);
		let _ = SampleRef::new("/x.wav");
	}
}
