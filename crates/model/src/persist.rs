//! JSON persistence with format-version checking.

use std::path::Path;

use crate::error::ModelError;
use crate::files::{absolutize, relativize};
use crate::project::{ClipContent, FORMAT_VERSION, Project, sort_notes};
use crate::validate;

impl Project {
	/// Pretty JSON.
	pub fn to_json_string(&self) -> Result<String, ModelError> {
		serde_json::to_string_pretty(self).map_err(|e| ModelError::Json(e.to_string()))
	}

	/// Parses, version-checks, normalizes ordering and validates a project.
	pub fn from_json_str(json: &str) -> Result<Self, ModelError> {
		let value: serde_json::Value =
			serde_json::from_str(json).map_err(|e| ModelError::Json(e.to_string()))?;
		let version = value
			.get("format_version")
			.ok_or_else(|| {
				ModelError::Json(
					"missing 'format_version' field; is this a Cadenza project?".into(),
				)
			})?
			.as_u64()
			.ok_or_else(|| {
				ModelError::Json("'format_version' must be a positive integer".into())
			})?;
		if version == 0 || version > u64::from(FORMAT_VERSION) {
			return Err(ModelError::UnsupportedFormatVersion {
				found: version,
				supported: FORMAT_VERSION,
			});
		}
		let mut project: Project =
			serde_json::from_value(value).map_err(|e| ModelError::Json(e.to_string()))?;
		project.normalize();
		project.validate()?;
		Ok(project)
	}

	/// JSON as written to `file`: paths inside the folder holding `file` are stored
	/// relative to it (see [`crate::files`]), others stay absolute.
	pub fn to_json_string_at(&self, file: &Path) -> Result<String, ModelError> {
		let base = project_dir(file);
		let mut copy = self.clone();
		copy.map_file_paths(|p| relativize(p, &base));
		copy.to_json_string()
	}

	/// Parses a project read from `file`, resolving relative paths against its folder.
	pub fn from_json_str_at(json: &str, file: &Path) -> Result<Self, ModelError> {
		let mut project = Self::from_json_str(json)?;
		let base = project_dir(file);
		project.map_file_paths(|p| absolutize(p, &base));
		Ok(project)
	}

	/// Saves atomically (temp file + fsync + rename) with portable relative paths, keeping
	/// the previous version as `<file>.bak`.
	pub fn save_json(&self, path: impl AsRef<Path>) -> Result<(), ModelError> {
		let path = path.as_ref();
		let json = self.to_json_string_at(path)?;
		write_atomic(path, json.as_bytes(), true)
	}

	/// Saves atomically with every path absolute and no backup (autosaves and snapshots
	/// stored away from the project folder).
	pub fn save_json_absolute(&self, path: impl AsRef<Path>) -> Result<(), ModelError> {
		let path = path.as_ref();
		let json = self.to_json_string()?;
		write_atomic(path, json.as_bytes(), false)
	}

	/// Loads a project; relative paths are resolved against the file's folder, so a moved
	/// project folder still finds its audio. Old projects with absolute paths load as-is.
	pub fn load_json(path: impl AsRef<Path>) -> Result<Self, ModelError> {
		let path = path.as_ref();
		let json = std::fs::read_to_string(path).map_err(|e| io_error(path, e))?;
		Self::from_json_str_at(&json, path)
	}

	/// Checks every invariant the command API maintains (ids, ranges, routing, ordering keys).
	pub fn validate(&self) -> Result<(), ModelError> {
		validate::project(self)
	}

	/// Restores the canonical orderings commands maintain (clips, notes, lanes, markers).
	fn normalize(&mut self) {
		for track in &mut self.tracks {
			track.sort_clips();
			for clip in &mut track.clips {
				if let ClipContent::Midi { notes } = &mut clip.content {
					sort_notes(notes);
				}
			}
		}
		for lane in &mut self.automation {
			lane.points.sort_by_key(|p| p.tick);
		}
		self.automation
			.sort_by_cached_key(|l| l.address.to_string());
		self.markers.sort_by_key(|m| m.tick);
		self.time_signature_changes.sort_by_key(|c| c.bar);
		self.time_signature_changes.dedup_by_key(|c| c.bar);
	}
}

/// The folder relative paths of a project saved at `file` are resolved against.
fn project_dir(file: &Path) -> std::path::PathBuf {
	let dir = file.parent().unwrap_or(Path::new("."));
	if dir.as_os_str().is_empty() {
		std::env::current_dir().unwrap_or_default()
	} else if dir.is_relative() {
		std::env::current_dir().unwrap_or_default().join(dir)
	} else {
		dir.to_path_buf()
	}
}

/// The backup kept by [`Project::save_json`]: `<file>.bak`.
pub fn backup_path(file: &Path) -> std::path::PathBuf {
	let mut name = file.as_os_str().to_owned();
	name.push(".bak");
	name.into()
}

/// Writes `bytes` to `path` atomically: a temp file in the same folder is written and
/// fsynced, then renamed over `path` (and the folder fsynced), so a crash leaves either
/// the old or the new file, never a torn one. With `backup`, an existing `path` is first
/// copied to [`backup_path`]. Creates missing parent folders.
pub fn write_atomic(path: &Path, bytes: &[u8], backup: bool) -> Result<(), ModelError> {
	use std::io::Write;
	let dir = path.parent().filter(|d| !d.as_os_str().is_empty());
	if let Some(dir) = dir {
		std::fs::create_dir_all(dir).map_err(|e| io_error(dir, e))?;
	}
	let mut tmp_name = path.file_name().unwrap_or_default().to_owned();
	tmp_name.push(format!(".tmp-{}", std::process::id()));
	let tmp = path.with_file_name(tmp_name);
	let write = || -> std::io::Result<()> {
		let mut f = std::fs::File::create(&tmp)?;
		f.write_all(bytes)?;
		f.sync_all()
	};
	if let Err(e) = write() {
		let _ = std::fs::remove_file(&tmp);
		return Err(io_error(&tmp, e));
	}
	if backup && path.is_file() {
		let bak = backup_path(path);
		if let Err(e) = std::fs::copy(path, &bak) {
			let _ = std::fs::remove_file(&tmp);
			return Err(io_error(&bak, e));
		}
	}
	if let Err(e) = std::fs::rename(&tmp, path) {
		let _ = std::fs::remove_file(&tmp);
		return Err(io_error(path, e));
	}
	#[cfg(unix)]
	if let Some(dir) = dir
		&& let Ok(d) = std::fs::File::open(dir)
	{
		let _ = d.sync_all();
	}
	Ok(())
}

fn io_error(path: &Path, e: std::io::Error) -> ModelError {
	ModelError::Io {
		path: path.display().to_string(),
		message: e.to_string(),
	}
}
