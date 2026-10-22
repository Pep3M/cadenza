//! The Files section of the browser: bookmarked folders, a filtered listing of the
//! current folder, and how a file (from there or from the OS file manager) is used.

use crate::i18n::tr;
use std::path::{Path, PathBuf};

use super::AUDIO_EXTENSIONS;

/// What a file is to Cadenza.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
	Folder,
	Audio,
	Midi,
	/// A `.cdz` project.
	Project,
}

impl FileKind {
	pub fn glyph(self) -> &'static str {
		match self {
			FileKind::Folder => "📁",
			FileKind::Audio => "~",
			FileKind::Midi => "♪",
			FileKind::Project => "◆",
		}
	}
}

/// The kind of a file by its name (folders are not looked up here).
pub fn classify(path: &Path) -> Option<FileKind> {
	let name = path.file_name()?.to_str()?.to_lowercase();
	if name.ends_with(cadenza_model::files::PROJECT_SUFFIX) {
		return Some(FileKind::Project);
	}
	let ext = path.extension()?.to_str()?.to_lowercase();
	if matches!(ext.as_str(), "mid" | "midi" | "smf") {
		Some(FileKind::Midi)
	} else if AUDIO_EXTENSIONS.contains(&ext.as_str()) {
		Some(FileKind::Audio)
	} else {
		None
	}
}

/// Which files the listing shows (folders are always shown).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FileFilter {
	/// Audio, MIDI and projects.
	#[default]
	All,
	Audio,
	Midi,
}

impl FileFilter {
	pub fn name(self) -> &'static str {
		match self {
			FileFilter::All => tr("All"),
			FileFilter::Audio => tr("Audio"),
			FileFilter::Midi => tr("MIDI"),
		}
	}

	fn accepts(self, kind: FileKind) -> bool {
		match self {
			FileFilter::All => true,
			FileFilter::Audio => kind == FileKind::Audio,
			FileFilter::Midi => kind == FileKind::Midi,
		}
	}
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEntry {
	pub path: PathBuf,
	pub name: String,
	pub kind: FileKind,
}

/// Folders first, then usable files, each sorted case-insensitively; hidden entries
/// and files Cadenza cannot use are left out.
pub fn list_dir(dir: &Path, filter: FileFilter) -> std::io::Result<Vec<FileEntry>> {
	let mut entries: Vec<FileEntry> = std::fs::read_dir(dir)?
		.filter_map(Result::ok)
		.filter_map(|e| {
			let path = e.path();
			let name = e.file_name().to_string_lossy().into_owned();
			if name.starts_with('.') {
				return None;
			}
			let kind = if path.is_dir() {
				FileKind::Folder
			} else {
				classify(&path).filter(|k| filter.accepts(*k))?
			};
			Some(FileEntry { path, name, kind })
		})
		.collect();
	entries.sort_by_key(|e| (e.kind != FileKind::Folder, e.name.to_lowercase()));
	Ok(entries)
}

fn home() -> Option<PathBuf> {
	cadenza_core::paths::home_dir()
}

/// `~/Music` (when it exists) and the home folder.
pub fn default_bookmarks() -> Vec<PathBuf> {
	let Some(home) = home() else {
		return Vec::new();
	};
	let music = home.join("Music");
	if music.is_dir() {
		vec![music, home]
	} else {
		vec![home]
	}
}

/// Short label for a bookmark (`~/Music`).
pub fn display_path(path: &Path) -> String {
	if let Some(home) = home()
		&& let Ok(rest) = path.strip_prefix(&home)
	{
		return if rest.as_os_str().is_empty() {
			"~".into()
		} else {
			format!("~/{}", rest.display())
		};
	}
	path.display().to_string()
}

/// State of the Files section: current folder, filter and a cached listing.
#[derive(Debug, Default)]
pub struct FileBrowser {
	pub dir: Option<PathBuf>,
	pub filter: FileFilter,
	listing: Option<(PathBuf, FileFilter, Result<Vec<FileEntry>, String>)>,
}

impl FileBrowser {
	pub fn open(&mut self, dir: PathBuf) {
		self.dir = Some(dir);
	}

	/// Goes to the parent folder.
	pub fn up(&mut self) {
		if let Some(parent) = self.dir.as_deref().and_then(Path::parent) {
			self.dir = Some(parent.to_path_buf());
		}
	}

	/// Forgets the cached listing (re-read on the next frame).
	pub fn refresh(&mut self) {
		self.listing = None;
	}

	/// Entries of the current folder (read once per folder/filter change).
	pub fn entries(&mut self) -> Result<&[FileEntry], &str> {
		let Some(dir) = self.dir.clone() else {
			return Ok(&[]);
		};
		let stale = !matches!(&self.listing, Some((d, f, _)) if *d == dir && *f == self.filter);
		if stale {
			let list = list_dir(&dir, self.filter).map_err(|e| format!("{}: {e}", dir.display()));
			self.listing = Some((dir, self.filter, list));
		}
		match &self.listing {
			Some((_, _, Ok(list))) => Ok(list),
			Some((_, _, Err(e))) => Err(e),
			None => Ok(&[]),
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn files_are_classified_by_name() {
		assert_eq!(classify(Path::new("a/Song.cdz")), Some(FileKind::Project));
		assert_eq!(classify(Path::new("x.JSON")), None);
		assert_eq!(classify(Path::new("beat.MID")), Some(FileKind::Midi));
		assert_eq!(classify(Path::new("vox.flac")), Some(FileKind::Audio));
		assert_eq!(classify(Path::new("notes.txt")), None);
		assert_eq!(classify(Path::new("noext")), None);
	}

	#[test]
	fn listing_filters_and_sorts() {
		let dir = tempfile::tempdir().unwrap();
		for f in ["b.wav", "A.mid", "c.txt", ".hidden.wav", "song.cdz"] {
			std::fs::write(dir.path().join(f), b"").unwrap();
		}
		std::fs::create_dir(dir.path().join("zeta")).unwrap();
		let names = |filter| -> Vec<String> {
			list_dir(dir.path(), filter)
				.unwrap()
				.into_iter()
				.map(|e| e.name)
				.collect()
		};
		assert_eq!(
			names(FileFilter::All),
			["zeta", "A.mid", "b.wav", "song.cdz"]
		);
		assert_eq!(names(FileFilter::Audio), ["zeta", "b.wav"]);
		assert_eq!(names(FileFilter::Midi), ["zeta", "A.mid"]);

		let mut fb = FileBrowser::default();
		fb.open(dir.path().join("zeta"));
		assert!(fb.entries().unwrap().is_empty());
		fb.up();
		assert_eq!(fb.entries().unwrap().len(), 4);
		fb.open(dir.path().join("missing"));
		assert!(fb.entries().is_err());
	}
}
