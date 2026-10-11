//! The AI's editing session: one project document plus the path it was opened/saved from.

use std::path::{Path, PathBuf};

use cadenza_model::{Document, Project};

pub struct Session {
	document: Document,
	path: Option<PathBuf>,
}

impl Session {
	pub fn new() -> Self {
		Self {
			document: Document::new(Project::new("Untitled")),
			path: None,
		}
	}

	pub fn document(&self) -> &Document {
		&self.document
	}

	pub fn document_mut(&mut self) -> &mut Document {
		&mut self.document
	}

	pub fn path(&self) -> Option<&Path> {
		self.path.as_deref()
	}

	/// Replaces the whole session with a freshly opened/created document.
	pub fn reset(&mut self, document: Document, path: Option<PathBuf>) {
		self.document = document;
		self.path = path;
	}

	pub fn set_path(&mut self, path: PathBuf) {
		self.path = Some(path);
	}
}

impl Default for Session {
	fn default() -> Self {
		Self::new()
	}
}
