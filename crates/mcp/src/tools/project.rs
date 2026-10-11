//! Project lifecycle: create/open/save, the AI-oriented summary, undo/redo.

use std::path::PathBuf;

use cadenza_core::{ClipId, Tick, TimeSignature};
use cadenza_model::{Command, Document, ModelError, Project, ProjectSummary};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::convert::tick_to_beats;
use crate::error::McpError;
use crate::session::Session;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct NewProjectArgs {
	/// Project name.
	pub name: String,
	/// Starting tempo in beats per minute (default 120).
	#[serde(default)]
	pub bpm: Option<f64>,
	/// Starting time signature (default 4/4).
	#[serde(default)]
	pub time_signature: Option<TimeSignature>,
}

pub fn new_project(
	session: &mut Session,
	args: NewProjectArgs,
) -> Result<ProjectSummary, McpError> {
	let mut document = Document::new(Project::new(args.name));
	if let Some(bpm) = args.bpm {
		document.execute(Command::SetTempo {
			tick: Tick::ZERO,
			bpm,
		})?;
	}
	if let Some(time_signature) = args.time_signature {
		document.execute(Command::SetTimeSignature { time_signature })?;
	}
	document.clear_history();
	session.reset(document, None);
	Ok(session.document().project().summary())
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct OpenProjectArgs {
	/// Path of the project JSON file to open.
	pub path: String,
}

pub fn open_project(
	session: &mut Session,
	args: OpenProjectArgs,
) -> Result<ProjectSummary, McpError> {
	let path = PathBuf::from(&args.path);
	let project = Project::load_json(&path)?;
	let summary = project.summary();
	session.reset(Document::new(project), Some(path));
	Ok(summary)
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SaveProjectArgs {
	/// Path to save to; defaults to the path the project was opened/last saved from.
	#[serde(default)]
	pub path: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct SaveProjectOutput {
	pub path: String,
}

pub fn save_project(
	session: &mut Session,
	args: SaveProjectArgs,
) -> Result<SaveProjectOutput, McpError> {
	let path = match args.path {
		Some(p) => PathBuf::from(p),
		None => session.path().ok_or(McpError::NoPath)?.to_path_buf(),
	};
	session.document().project().save_json(&path)?;
	session.set_path(path.clone());
	Ok(SaveProjectOutput {
		path: path.display().to_string(),
	})
}

pub fn get_project(session: &Session) -> ProjectSummary {
	session.document().project().summary()
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetClipNotesArgs {
	/// Clip id, as returned by `add_midi_clip` or found in `get_project`.
	pub clip_id: ClipId,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct NoteOut {
	pub id: cadenza_core::NoteId,
	/// MIDI pitch (60 = C4).
	pub pitch: u8,
	/// Note name, e.g. "C4".
	pub name: String,
	pub velocity: f32,
	/// Relative to the clip start.
	pub start_beats: f64,
	pub length_beats: f64,
}

pub fn get_clip_notes(session: &Session, args: GetClipNotesArgs) -> Result<Vec<NoteOut>, McpError> {
	let (_, clip) = session
		.document()
		.project()
		.find_clip(args.clip_id)
		.ok_or(ModelError::ClipNotFound(args.clip_id))?;
	Ok(clip
		.notes()
		.iter()
		.map(|n| NoteOut {
			id: n.id,
			pitch: n.pitch,
			name: cadenza_model::note_name(n.pitch),
			velocity: n.velocity,
			start_beats: tick_to_beats(n.start),
			length_beats: tick_to_beats(n.length),
		})
		.collect())
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct HistoryStep {
	pub label: String,
}

pub fn undo(session: &mut Session) -> Result<HistoryStep, McpError> {
	Ok(HistoryStep {
		label: session.document_mut().undo()?,
	})
}

pub fn redo(session: &mut Session) -> Result<HistoryStep, McpError> {
	Ok(HistoryStep {
		label: session.document_mut().redo()?,
	})
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct HistoryOutput {
	/// Undoable steps, oldest first.
	pub undo: Vec<String>,
	/// Redoable steps, next redo first.
	pub redo: Vec<String>,
}

pub fn history(session: &Session) -> HistoryOutput {
	let document = session.document();
	HistoryOutput {
		undo: document
			.history_labels()
			.into_iter()
			.map(str::to_owned)
			.collect(),
		redo: document
			.redo_labels()
			.into_iter()
			.map(str::to_owned)
			.collect(),
	}
}
