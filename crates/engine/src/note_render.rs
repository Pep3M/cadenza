//! Rendered copies of audio clips with note edits (NE-5).
//!
//! A clip whose [`NoteEdits`] change the audio plays a render of its file through the
//! resynthesis (`cadenza-notes`). Renders are derived data, like the analysis cache: one
//! 32-bit float WAV per (file, file size and modification time, edits) in the user cache,
//! so undoing an edit finds its earlier render again, the project folder stays small and a
//! project opened on another machine renders them anew. Offline renders and exports make a
//! missing render on the spot; the realtime player plays the clip as recorded (with a
//! warning) until the app's background render lands and the project is reloaded.

use std::path::{Path, PathBuf};

use cadenza_audio_io::{AudioBuffer, ExportOptions, WavSampleFormat, decode_file, export};
use cadenza_core::ClipId;
use cadenza_model::{ClipContent, NoteEdits, Project};
use cadenza_notes::analysis::{AnalysisOptions, Cache};
use cadenza_notes::clip;
use cadenza_notes::resynth::{self, Residual, VerifyOptions};
use sha2::{Digest, Sha256};

use crate::EngineError;

/// Bumped when the resynthesis changes what it renders (old renders are then ignored).
pub const RENDER_VERSION: u32 = 1;

/// Part of the warning of a clip whose render is not ready (the app, rendering it in the
/// background, leaves it out of its notices).
pub const PENDING_WARNING: &str = "its note edits are still rendering";

/// Environment variable naming another folder for the renders (tests).
pub const NOTE_RENDERS_ENV: &str = "CADENZA_NOTE_RENDERS";

/// Folder of the renders: `$CADENZA_NOTE_RENDERS` (then its analyses go to its `analysis/`
/// too), else `note-renders` in the user cache.
pub fn render_dir() -> PathBuf {
	match std::env::var_os(NOTE_RENDERS_ENV) {
		Some(dir) if !dir.is_empty() => PathBuf::from(dir),
		_ => cadenza_core::paths::cache_dir().join("note-renders"),
	}
}

/// Where the render of `file` with `edits` lives (whether or not it exists yet).
pub fn render_path(file: &Path, edits: &NoteEdits) -> PathBuf {
	let mut h = Sha256::new();
	h.update(RENDER_VERSION.to_le_bytes());
	h.update(file.to_string_lossy().as_bytes());
	if let Ok(meta) = std::fs::metadata(file) {
		h.update(meta.len().to_le_bytes());
		if let Ok(modified) = meta.modified()
			&& let Ok(since) = modified.duration_since(std::time::UNIX_EPOCH)
		{
			h.update(since.as_nanos().to_le_bytes());
		}
	}
	h.update(serde_json::to_vec(edits).unwrap_or_default());
	let hex: String = h
		.finalize()
		.iter()
		.take(16)
		.map(|b| format!("{b:02x}"))
		.collect();
	render_dir().join(format!("{hex}.wav"))
}

/// The render of `file` with `edits`, when it exists.
pub fn ready(file: &Path, edits: &NoteEdits) -> Option<PathBuf> {
	let path = render_path(file, edits);
	path.is_file().then_some(path)
}

/// What a render measured: each note's distance from its target.
#[derive(Debug, Clone)]
pub struct NoteRender {
	pub path: PathBuf,
	pub residuals: Vec<Residual>,
}

/// Renders `file` with `edits` (closed loop: every note measured and corrected once more
/// when still off) into its [`render_path`]. Blocking: seconds for a long take.
pub fn render(file: &Path, edits: &NoteEdits) -> Result<NoteRender, EngineError> {
	let path = render_path(file, edits);
	let fail = |message: String| EngineError::NoteRender {
		file: file.to_path_buf(),
		message,
	};
	let buffer = decode_file(file).map_err(|e| fail(e.to_string()))?;
	let rate = buffer.sample_rate();
	let channels: Vec<&[f32]> = buffer.channels().iter().map(Vec::as_slice).collect();
	let mono = resynth::mono(&channels);
	// The analysis cache the CLI and the AI tools share (inside the renders folder when it
	// was moved).
	let analysis_dir = match std::env::var_os(NOTE_RENDERS_ENV) {
		Some(dir) if !dir.is_empty() => PathBuf::from(dir).join("analysis"),
		_ => cadenza_core::paths::cache_dir().join("analysis"),
	};
	let cache = Cache::new(analysis_dir);
	let analysis = cache
		.get_or_analyze(&mono, rate, &AnalysisOptions::default(), &mut |_| true)
		.ok_or_else(|| fail("the analysis was cancelled".into()))?;
	let seg = clip::to_segmentation(edits);
	let (rendered, residuals) = resynth::render_verified(
		&channels,
		&analysis,
		&seg,
		&clip::to_edits(edits),
		&VerifyOptions::default(),
	)
	.map_err(|e| fail(e.to_string()))?;
	let out = AudioBuffer::from_planar(rate, rendered.channels).map_err(|e| fail(e.to_string()))?;
	std::fs::create_dir_all(render_dir()).map_err(|e| fail(e.to_string()))?;
	let temp = path.with_extension(format!("tmp-{}.wav", std::process::id()));
	export(&out, &temp, &ExportOptions::wav(WavSampleFormat::Float32))
		.map_err(|e| fail(e.to_string()))?;
	std::fs::rename(&temp, &path).map_err(|e| fail(e.to_string()))?;
	Ok(NoteRender { path, residuals })
}

/// A clip whose note edits have no render yet.
#[derive(Debug, Clone, PartialEq)]
pub struct PendingRender {
	pub clip: ClipId,
	pub file: PathBuf,
	pub edits: NoteEdits,
}

/// Clips of `project` whose edits change the audio and have no render yet (each distinct
/// file + edits once).
pub fn pending(project: &Project) -> Vec<PendingRender> {
	let mut out: Vec<PendingRender> = Vec::new();
	for track in &project.tracks {
		for c in &track.clips {
			if let ClipContent::Audio {
				file,
				notes: Some(edits),
				..
			} = &c.content
				&& edits.is_edited()
				&& ready(file, edits).is_none()
				&& !out.iter().any(|p| &p.file == file && p.edits == **edits)
			{
				out.push(PendingRender {
					clip: c.id,
					file: file.clone(),
					edits: (**edits).clone(),
				});
			}
		}
	}
	out
}

/// The file an audio clip plays: its render when its edits change the audio (made now
/// when `render_missing`), else the file itself. A missing render is a warning.
pub(crate) fn playable(
	file: &Path,
	edits: Option<&NoteEdits>,
	render_missing: bool,
	owner: &str,
	warnings: &mut Vec<String>,
) -> PathBuf {
	let Some(edits) = edits.filter(|e| e.is_edited()) else {
		return file.to_path_buf();
	};
	if let Some(path) = ready(file, edits) {
		return path;
	}
	if !render_missing {
		warnings.push(format!(
			"{owner}: {PENDING_WARNING}; it plays as recorded until then"
		));
		return file.to_path_buf();
	}
	match render(file, edits) {
		Ok(r) => r.path,
		Err(e) => {
			warnings.push(format!("{owner}: {e}; it plays as recorded"));
			file.to_path_buf()
		}
	}
}
