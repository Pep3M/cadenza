//! Standard MIDI files: import into the project (one undoable step) and export.

use std::path::PathBuf;

use cadenza_core::{ClipId, Tick, TrackId};
use cadenza_midi_io::{ImportOptions, ImportTarget};
use cadenza_model::{Command, ModelError, TrackKind};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::convert::{beats_to_tick, tick_to_beats};
use crate::error::McpError;
use crate::session::Session;

fn read(path: &str) -> Result<(PathBuf, Vec<u8>), McpError> {
	let path = PathBuf::from(path.trim());
	let bytes = std::fs::read(&path).map_err(|e| McpError::Io {
		path: path.display().to_string(),
		message: format!("{e}; pass the path of an existing .mid file"),
	})?;
	Ok((path, bytes))
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ImportMidiArgs {
	/// Standard MIDI file (.mid, type 0 or 1).
	pub path: String,
	/// One new track per MIDI channel used (e.g. a type-0 file with drums on channel 10
	/// becomes separate drum and instrument tracks). Ignored with `track_id`.
	#[serde(default)]
	pub split_by_channel: bool,
	/// Instrument track to drop all the notes into as one clip; omit to create new
	/// instrument tracks (General MIDI drums get drum_machine, others poly_synth).
	#[serde(default)]
	pub track_id: Option<TrackId>,
	/// Clip start in beats when importing into `track_id` (default 0).
	#[serde(default)]
	pub at_beats: Option<f64>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ImportedTrack {
	pub track_id: TrackId,
	pub name: String,
	pub clip_id: ClipId,
	pub note_count: usize,
	pub length_beats: f64,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ImportMidiOutput {
	/// New tracks (empty when importing into an existing track).
	pub created_tracks: Vec<ImportedTrack>,
	/// The clip added to `track_id`, when given.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub clip: Option<ImportedTrack>,
	pub tempo_points: usize,
	pub markers: usize,
	/// Data the project can't hold yet (CCs, pitch bend, program changes) and other notes.
	pub warnings: Vec<String>,
}

/// Imports a MIDI file through one `Batch` command, so a single undo removes it all
/// (including the tempo map, time signature and markers it sets).
pub fn import_midi(
	session: &mut Session,
	args: ImportMidiArgs,
) -> Result<ImportMidiOutput, McpError> {
	let (_, bytes) = read(&args.path)?;
	let target = match args.track_id {
		Some(track) => {
			let project = session.document().project();
			let t = project
				.track(track)
				.ok_or(ModelError::TrackNotFound(track))?;
			if t.kind != TrackKind::Instrument {
				return Err(McpError::Invalid(format!(
					"track {track} is a {:?} track; MIDI imports need an Instrument track (or omit track_id to create new tracks)",
					t.kind
				)));
			}
			let at = args.at_beats.unwrap_or(0.0);
			if !(at.is_finite() && at >= 0.0) {
				return Err(McpError::Invalid(format!(
					"at_beats {at} is invalid; use a beat position >= 0"
				)));
			}
			ImportTarget::ExistingTrack {
				track,
				position: beats_to_tick(at),
			}
		}
		None => ImportTarget::NewTracks,
	};
	let result = cadenza_midi_io::import_smf(
		&bytes,
		&ImportOptions {
			split_by_channel: args.split_by_channel,
			target,
		},
	)
	.map_err(|e| {
		McpError::Invalid(format!(
			"{e}; check that '{}' is a standard MIDI file",
			args.path
		))
	})?;
	let mut out = ImportMidiOutput {
		created_tracks: Vec::new(),
		clip: None,
		tempo_points: 0,
		markers: 0,
		warnings: result.warnings,
	};
	if let Command::Batch { commands, .. } = &result.command {
		for cmd in commands {
			match cmd {
				Command::SetTempo { .. } => out.tempo_points += 1,
				Command::AddMarker { .. } => out.markers += 1,
				Command::AddTrack { track, .. } => {
					if let Some(clip) = track.clips.first() {
						out.created_tracks.push(ImportedTrack {
							track_id: track.id,
							name: track.name.clone(),
							clip_id: clip.id,
							note_count: clip.notes().len(),
							length_beats: tick_to_beats(clip.length),
						});
					}
				}
				Command::AddClip { track, clip } => {
					out.clip = Some(ImportedTrack {
						track_id: *track,
						name: clip.name.clone(),
						clip_id: clip.id,
						note_count: clip.notes().len(),
						length_beats: tick_to_beats(clip.length),
					});
				}
				_ => {}
			}
		}
	}
	session.document_mut().execute(result.command)?;
	Ok(out)
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ExportMidiArgs {
	/// Output path ending in .mid.
	pub path: String,
	/// Export only this instrument track (default: every instrument track with notes).
	#[serde(default)]
	pub track_id: Option<TrackId>,
	/// Export only this MIDI clip (positioned at beat 0).
	#[serde(default)]
	pub clip_id: Option<ClipId>,
	/// Write MPE: every note on its own channel with its own bend, pressure and timbre
	/// (channel pressure, CC74), for MPE synths in other programs. Needs track_id or clip_id.
	#[serde(default)]
	pub mpe: bool,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ExportMidiOutput {
	pub path: String,
	/// Note tracks written (plus one tempo/meta track).
	pub tracks: usize,
	pub notes: usize,
	#[serde(skip_serializing_if = "Vec::is_empty")]
	pub warnings: Vec<String>,
}

pub fn export_midi(session: &Session, args: ExportMidiArgs) -> Result<ExportMidiOutput, McpError> {
	let path = PathBuf::from(args.path.trim());
	let ext_ok = path
		.extension()
		.and_then(|e| e.to_str())
		.is_some_and(|e| e.eq_ignore_ascii_case("mid") || e.eq_ignore_ascii_case("midi"));
	if !ext_ok {
		return Err(McpError::Invalid(format!(
			"path '{}' must end in .mid",
			path.display()
		)));
	}
	let project = session.document().project();
	if args.mpe {
		return export_mpe_file(project, &args, &path);
	}
	let bent = |clips: &mut dyn Iterator<Item = &cadenza_model::Clip>| -> usize {
		clips
			.flat_map(|c| c.notes().iter())
			.filter(|n| !(n.bend.is_empty() && n.pressure.is_empty() && n.timbre.is_empty()))
			.count()
	};
	let bent = match (args.track_id, args.clip_id) {
		(_, Some(id)) => bent(&mut project.find_clip(id).map(|(_, c)| c).into_iter()),
		(Some(id), None) => bent(&mut project.track(id).into_iter().flat_map(|t| t.clips.iter())),
		(None, None) => bent(&mut project.tracks.iter().flat_map(|t| t.clips.iter())),
	};
	let warnings: Vec<String> = (bent > 0)
		.then(|| {
			format!(
				"{bent} notes have their own bend, pressure or timbre, which plain MIDI cannot hold; \
				 export with mpe true (and a track_id or clip_id) to keep it"
			)
		})
		.into_iter()
		.collect();
	let (bytes, tracks, notes) = match (args.track_id, args.clip_id) {
		(Some(_), Some(_)) => {
			return Err(McpError::Invalid(
				"pass track_id or clip_id, not both (omit both to export the whole project)".into(),
			));
		}
		(None, Some(id)) => {
			let (_, clip) = project.find_clip(id).ok_or(ModelError::ClipNotFound(id))?;
			if !clip.is_midi() {
				return Err(ModelError::NotAMidiClip(id).into());
			}
			let signature = project.meter().signature_at(clip.start);
			let bytes = cadenza_midi_io::export_clip(clip, &project.tempo, &signature)
				.map_err(|e| McpError::Invalid(e.to_string()))?;
			(bytes, 1, clip.notes().len())
		}
		(track, None) => {
			let mut subset = project.clone();
			if let Some(id) = track {
				let t = project.track(id).ok_or(ModelError::TrackNotFound(id))?;
				if t.kind != TrackKind::Instrument {
					return Err(McpError::Invalid(format!(
						"track {id} is a {:?} track; only Instrument tracks hold MIDI",
						t.kind
					)));
				}
				subset.tracks.retain(|t| t.id == id);
			}
			let with_notes: Vec<usize> = subset
				.tracks
				.iter()
				.filter(|t| t.kind == TrackKind::Instrument)
				.map(|t| t.clips.iter().map(|c| c.notes().len()).sum())
				.filter(|&n| n > 0)
				.collect();
			if with_notes.is_empty() {
				return Err(McpError::Invalid(
					"nothing to export: no instrument track has MIDI notes; add notes first".into(),
				));
			}
			let bytes = cadenza_midi_io::export_project(&subset)
				.map_err(|e| McpError::Invalid(e.to_string()))?;
			(bytes, with_notes.len(), with_notes.iter().sum())
		}
	};
	std::fs::write(&path, bytes).map_err(|e| McpError::Io {
		path: path.display().to_string(),
		message: e.to_string(),
	})?;
	Ok(ExportMidiOutput {
		path: path.display().to_string(),
		tracks,
		notes,
		warnings,
	})
}

/// `export_midi { mpe: true }`: the clip's notes (from beat 0) or the track's sounding clips
/// as one MPE file ([`cadenza_midi_io::export_mpe`]).
fn export_mpe_file(
	project: &cadenza_model::Project,
	args: &ExportMidiArgs,
	path: &std::path::Path,
) -> Result<ExportMidiOutput, McpError> {
	let (clips, origin, name): (Vec<&cadenza_model::Clip>, Tick, String) =
		match (args.track_id, args.clip_id) {
			(_, Some(id)) => {
				let (_, clip) = project.find_clip(id).ok_or(ModelError::ClipNotFound(id))?;
				if !clip.is_midi() {
					return Err(ModelError::NotAMidiClip(id).into());
				}
				(vec![clip], clip.start, clip.name.clone())
			}
			(Some(id), None) => {
				let t = project.track(id).ok_or(ModelError::TrackNotFound(id))?;
				if t.kind != TrackKind::Instrument {
					return Err(McpError::Invalid(format!(
						"track {id} is a {:?} track; only Instrument tracks hold MIDI",
						t.kind
					)));
				}
				let clips = t
					.clips
					.iter()
					.filter(|c| c.is_midi() && c.sounds())
					.collect();
				(clips, Tick::ZERO, t.name.clone())
			}
			(None, None) => {
				return Err(McpError::Invalid(
					"mpe needs track_id or clip_id: an MPE file holds one instrument's notes"
						.into(),
				));
			}
		};
	let mut notes = Vec::new();
	for c in &clips {
		for n in c.notes().iter().filter(|n| n.start < c.length) {
			let start = c.start + n.start - origin;
			let length = n.length.min(c.length - n.start);
			notes.push(cadenza_midi_io::MpeNote {
				start,
				length,
				pitch: n.pitch,
				velocity: n.velocity,
				curve: n
					.bend
					.iter()
					.filter(|p| p.tick < length)
					.map(|p| (start + p.tick, p.semitones))
					.collect(),
				pressure: n
					.pressure
					.iter()
					.filter(|p| p.tick < length)
					.map(|p| (start + p.tick, p.value))
					.collect(),
				timbre: n
					.timbre
					.iter()
					.filter(|p| p.tick < length)
					.map(|p| (start + p.tick, p.value))
					.collect(),
			});
		}
	}
	if notes.is_empty() {
		return Err(McpError::Invalid(
			"nothing to export: no MIDI notes there; add notes first".into(),
		));
	}
	let first = clips.first().map_or(Tick::ZERO, |c| c.start);
	let out = cadenza_midi_io::export_mpe(
		&notes,
		&project.tempo,
		&project.meter().signature_at(first),
		&name,
	)
	.map_err(|e| McpError::Invalid(e.to_string()))?;
	std::fs::write(path, &out.bytes).map_err(|e| McpError::Io {
		path: path.display().to_string(),
		message: e.to_string(),
	})?;
	Ok(ExportMidiOutput {
		path: path.display().to_string(),
		tracks: 1,
		notes: notes.len(),
		warnings: (out.shared > 0)
			.then(|| {
				format!(
					"{} notes sounded with 15 others and share an MPE channel: their bends move each other",
					out.shared
				)
			})
			.into_iter()
			.collect(),
	})
}
