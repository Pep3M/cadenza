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

/// An open undo group (see [`Document::begin_group`]).
#[derive(Debug, Clone, PartialEq)]
struct Group {
	label: String,
	/// Undo entries below this index predate the group.
	undo_len: usize,
	/// Redo stack size when the group began.
	redo_len: usize,
	/// Nested `begin_group` calls; only the outermost one folds.
	depth: u32,
}

/// Owns a [`Project`] and is the only way to mutate it: every change is a [`Command`]
/// recorded with its inverse.
///
/// Undo groups fold everything executed between [`Document::begin_group`] and
/// [`Document::end_group`] into ONE undo step (a `Batch` labelled with the group's label),
/// e.g. all the edits the AI makes for one request. Inside a group `undo` only reverts the
/// group's own steps.
#[derive(Debug, Clone, PartialEq)]
pub struct Document {
	project: Project,
	undo: Vec<Entry>,
	redo: Vec<Entry>,
	group: Option<Group>,
	/// Bumped by every change to the project or the history (observers poll it).
	revision: u64,
}

impl Document {
	pub fn new(project: Project) -> Self {
		Self {
			project,
			undo: Vec::new(),
			redo: Vec::new(),
			group: None,
			revision: 0,
		}
	}

	/// Counter bumped by every successful execute/undo/redo, group fold or abort, and
	/// history clear: cheap change detection for observers sharing the document.
	pub fn revision(&self) -> u64 {
		self.revision
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
		if let Some(g) = &mut self.group {
			g.redo_len = 0;
		}
		self.revision += 1;
		Ok(())
	}

	/// Reverts the last executed command and returns its label.
	pub fn undo(&mut self) -> Result<String, ModelError> {
		let floor = self.group.as_ref().map_or(0, |g| g.undo_len);
		if self.undo.len() <= floor {
			return Err(ModelError::NothingToUndo);
		}
		let entry = self.undo.pop().ok_or(ModelError::NothingToUndo)?;
		match apply_all(&mut self.project, &entry.inverse) {
			Ok(_) => {
				let label = entry.label.clone();
				self.redo.push(entry);
				self.revision += 1;
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
		if let Some(g) = &self.group
			&& self.redo.len() <= g.redo_len
		{
			// The steps undone before the group are not the group's to redo.
			return Err(ModelError::NothingToRedo);
		}
		let mut entry = self.redo.pop().ok_or(ModelError::NothingToRedo)?;
		match apply(&mut self.project, &entry.forward) {
			Ok(inverse) => {
				entry.inverse = inverse;
				let label = entry.label.clone();
				self.undo.push(entry);
				self.revision += 1;
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
		if let Some(g) = &mut self.group {
			g.undo_len = 0;
			g.redo_len = 0;
		}
		self.revision += 1;
	}

	/// Starts folding the following commands into one undo step labelled `label`. Groups
	/// nest: inner `begin_group`/`end_group` pairs only count depth, the outermost label
	/// wins.
	pub fn begin_group(&mut self, label: impl Into<String>) {
		match &mut self.group {
			Some(g) => g.depth += 1,
			None => {
				self.group = Some(Group {
					label: label.into(),
					undo_len: self.undo.len(),
					redo_len: self.redo.len(),
					depth: 1,
				})
			}
		}
	}

	/// Whether an undo group is open.
	pub fn in_group(&self) -> bool {
		self.group.is_some()
	}

	/// Closes the innermost group. Closing the outermost one folds every step executed
	/// in it into a single undo entry (nothing happens for an empty group). Returns true
	/// when a step was folded; calling it without an open group does nothing.
	pub fn end_group(&mut self) -> bool {
		let Some(g) = &mut self.group else {
			return false;
		};
		if g.depth > 1 {
			g.depth -= 1;
			return false;
		}
		let g = self.group.take().expect("checked above");
		if self.undo.len() < g.undo_len {
			return false;
		}
		let steps: Vec<Entry> = self.undo.drain(g.undo_len..).collect();
		// Steps the group undid and did not redo were its own scratch work.
		self.redo.truncate(g.redo_len);
		if steps.is_empty() {
			return false;
		}
		let inverse = steps
			.iter()
			.rev()
			.flat_map(|e| e.inverse.iter().cloned())
			.collect();
		let forward = Command::Batch {
			label: g.label.clone(),
			commands: steps.into_iter().map(|e| e.forward).collect(),
		};
		self.undo.push(Entry {
			label: g.label,
			forward,
			inverse,
		});
		self.revision += 1;
		true
	}

	/// Closes the group (whatever its depth) and reverts every step executed in it, as
	/// if it never happened. Returns how many steps were reverted.
	pub fn abort_group(&mut self) -> Result<usize, ModelError> {
		let Some(g) = self.group.take() else {
			return Ok(0);
		};
		let mut reverted = 0;
		while self.undo.len() > g.undo_len {
			let entry = self.undo.pop().expect("length checked");
			if let Err(e) = apply_all(&mut self.project, &entry.inverse) {
				self.undo.push(entry);
				self.revision += 1;
				return Err(e);
			}
			reverted += 1;
		}
		self.redo.truncate(g.redo_len);
		self.revision += 1;
		Ok(reverted)
	}
}

impl From<Project> for Document {
	fn from(project: Project) -> Self {
		Self::new(project)
	}
}
