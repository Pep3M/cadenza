//! `cadenza import-midi` / `cadenza export-midi`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use cadenza_midi_io::{ImportOptions, ImportTarget};
use cadenza_model::{Document, Project, TrackKind};

use crate::{find_track, load};

pub fn import(
	file: &Path,
	output: Option<PathBuf>,
	split_by_channel: bool,
	into: Option<PathBuf>,
	track: Option<String>,
	at_beats: f64,
) -> Result<()> {
	let bytes = std::fs::read(file).with_context(|| format!("reading {}", file.display()))?;
	let project = match &into {
		Some(path) => load(path)?,
		None => Project::new(
			file.file_stem()
				.and_then(|s| s.to_str())
				.unwrap_or("Imported MIDI"),
		),
	};
	let target = match &track {
		Some(name) => {
			let t = find_track(&project, name)?;
			if t.kind != TrackKind::Instrument {
				bail!("track '{}' is not an instrument track", t.name);
			}
			if !(at_beats.is_finite() && at_beats >= 0.0) {
				bail!("--at-beats must be >= 0");
			}
			ImportTarget::ExistingTrack {
				track: t.id,
				position: cadenza_core::Tick::from_beats(at_beats),
			}
		}
		None => ImportTarget::NewTracks,
	};
	let result = cadenza_midi_io::import_smf(
		&bytes,
		&ImportOptions {
			split_by_channel,
			target,
		},
	)
	.with_context(|| format!("importing {}", file.display()))?;
	let mut doc = Document::new(project);
	let before = doc.project().tracks.len();
	doc.execute(result.command)
		.context("applying the MIDI import")?;
	for w in &result.warnings {
		eprintln!("warning: {w}");
	}
	let project = doc.project();
	for t in &project.tracks[before..] {
		let notes: usize = t.clips.iter().map(|c| c.notes().len()).sum();
		eprintln!("track '{}': {notes} notes", t.name);
	}
	let out = output.or(into).expect("clap requires -o or --into");
	project
		.save_json(&out)
		.with_context(|| format!("saving {}", out.display()))?;
	eprintln!("wrote {}", out.display());
	Ok(())
}

pub fn export(project: &Path, output: &Path, track: Option<String>) -> Result<()> {
	let mut project = load(&project.to_path_buf())?;
	if let Some(name) = track {
		let id = find_track(&project, &name)?.id;
		project.tracks.retain(|t| t.id == id);
	}
	let notes: usize = project
		.tracks
		.iter()
		.filter(|t| t.kind == TrackKind::Instrument)
		.flat_map(|t| &t.clips)
		.map(|c| c.notes().len())
		.sum();
	if notes == 0 {
		bail!("nothing to export: no instrument track has MIDI notes");
	}
	let bytes = cadenza_midi_io::export_project(&project).context("encoding the MIDI file")?;
	std::fs::write(output, bytes).with_context(|| format!("writing {}", output.display()))?;
	eprintln!("wrote {} ({notes} notes)", output.display());
	Ok(())
}
