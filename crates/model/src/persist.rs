//! JSON persistence with format-version checking.

use std::path::Path;

use crate::error::ModelError;
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

	pub fn save_json(&self, path: impl AsRef<Path>) -> Result<(), ModelError> {
		let path = path.as_ref();
		let json = self.to_json_string()?;
		std::fs::write(path, json).map_err(|e| io_error(path, e))
	}

	pub fn load_json(path: impl AsRef<Path>) -> Result<Self, ModelError> {
		let path = path.as_ref();
		let json = std::fs::read_to_string(path).map_err(|e| io_error(path, e))?;
		Self::from_json_str(&json)
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
	}
}

fn io_error(path: &Path, e: std::io::Error) -> ModelError {
	ModelError::Io {
		path: path.display().to_string(),
		message: e.to_string(),
	}
}
