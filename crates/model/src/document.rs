//! Undoable editing session over a [`Project`].

use crate::apply::{apply, apply_all};
use crate::command::Command;
use crate::error::ModelError;
use crate::project::Project;

#[derive(Debug, Clone, PartialEq)]
struct Entry {
	label: String,
	/// Re-applied on redo.
	forward: Command,
	/// Applied in order on undo.
	inverse: Vec<Command>,
}

/// Owns a [`Project`] and is the only way to mutate it: every change is a [`Command`]
/// recorded with its inverse.
#[derive(Debug, Clone, PartialEq)]
pub struct Document {
	project: Project,
	undo: Vec<Entry>,
	redo: Vec<Entry>,
}

impl Document {
	pub fn new(project: Project) -> Self {
		Self {
			project,
			undo: Vec::new(),
			redo: Vec::new(),
		}
	}

	pub fn project(&self) -> &Project {
		&self.project
	}

	pub fn into_project(self) -> Project {
		self.project
	}

	/// Validates and applies `cmd` atomically. On success the command becomes undoable and
	/// the redo stack is cleared; on error nothing changes.
	pub fn execute(&mut self, cmd: Command) -> Result<(), ModelError> {
		let inverse = apply(&mut self.project, &cmd)?;
		self.undo.push(Entry {
			label: cmd.label(),
			forward: cmd,
			inverse,
		});
		self.redo.clear();
		Ok(())
	}

	/// Reverts the last executed command and returns its label.
	pub fn undo(&mut self) -> Result<String, ModelError> {
		let entry = self.undo.pop().ok_or(ModelError::NothingToUndo)?;
		match apply_all(&mut self.project, &entry.inverse) {
			Ok(_) => {
				let label = entry.label.clone();
				self.redo.push(entry);
				Ok(label)
			}
			Err(e) => {
				self.undo.push(entry);
				Err(e)
			}
		}
	}

	/// Re-applies the last undone command and returns its label.
	pub fn redo(&mut self) -> Result<String, ModelError> {
		let mut entry = self.redo.pop().ok_or(ModelError::NothingToRedo)?;
		match apply(&mut self.project, &entry.forward) {
			Ok(inverse) => {
				entry.inverse = inverse;
				let label = entry.label.clone();
				self.undo.push(entry);
				Ok(label)
			}
			Err(e) => {
				self.redo.push(entry);
				Err(e)
			}
		}
	}

	pub fn can_undo(&self) -> bool {
		!self.undo.is_empty()
	}

	pub fn can_redo(&self) -> bool {
		!self.redo.is_empty()
	}

	/// Labels of undoable steps, oldest first.
	pub fn history_labels(&self) -> Vec<&str> {
		self.undo.iter().map(|e| e.label.as_str()).collect()
	}

	/// Labels of redoable steps, next redo first.
	pub fn redo_labels(&self) -> Vec<&str> {
		self.redo.iter().rev().map(|e| e.label.as_str()).collect()
	}

	pub fn clear_history(&mut self) {
		self.undo.clear();
		self.redo.clear();
	}
}

impl From<Project> for Document {
	fn from(project: Project) -> Self {
		Self::new(project)
	}
}
