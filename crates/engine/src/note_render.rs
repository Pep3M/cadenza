//! Rendered copies of audio clips with note edits (NE-5).
//!
//! A clip whose [`NoteEdits`] change the audio plays a render of its file through the
//! resynthesis (`cadenza-notes`). Renders are derived data, like the analysis cache: one
//! 32-bit float WAV per (file, file size and modification time, edits) in the user cache,
//! so undoing an edit finds its earlier render again, the project folder stays small and a
//! project opened on another machine renders them anew. Offline renders and exports make a
//! missing render on the spot; the realtime player plays the clip as recorded (with a
//! warning) until the app's background render lands and the project is reloaded.
//!
//! Each render keeps beside it (`<render>.json`) the edits it was asked and the ones it
//! followed, and every file lists its latest renders (`<file key>.renders`): a new render
//! starts from the closest earlier one with the same notes and re-renders only around the
//! notes that changed (`resynth::rerender_verified`), so a one-note edit on a long take
//! lands in a fraction of a second instead of re-rendering the whole take.

use std::path::{Path, PathBuf};

use cadenza_audio_io::{AudioBuffer, ExportOptions, WavSampleFormat, decode_file, export};
use cadenza_core::ClipId;
use cadenza_model::{ClipContent, NoteEdits, Project};
use cadenza_notes::analysis::{AnalysisOptions, Cache};
use cadenza_notes::clip;
use cadenza_notes::edit::Edits;
use cadenza_notes::marks::{Marks, pitch_marks};
use cadenza_notes::resynth::{self, Previous, Residual, VerifyOptions};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::EngineError;

/// Bumped when the resynthesis changes what it renders (old renders are then ignored).
pub const RENDER_VERSION: u32 = 2;

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
	let mut h = file_hasher(file);
	h.update(serde_json::to_vec(edits).unwrap_or_default());
	render_dir().join(format!("{}.wav", hex(h)))
}

/// The hash state of the render version and `file` (path, size, modification time).
fn file_hasher(file: &Path) -> Sha256 {
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
	h
}

fn hex(h: Sha256) -> String {
	h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// The list of `file`'s latest renders.
fn index_path(file: &Path) -> PathBuf {
	render_dir().join(format!("{}.renders", hex(file_hasher(file))))
}

/// Renders a file's index remembers.
const INDEX_LEN: usize = 8;

/// What a render was made of (`<render>.json`).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Sidecar {
	/// The clip's notes and edits it was asked for.
	asked: NoteEdits,
	/// What the closed loop followed (`resynth::Verified::effective`).
	effective: Edits,
}

fn sidecar_path(render: &Path) -> PathBuf {
	render.with_extension("json")
}

/// Earlier renders of `file` with the same notes as `edits`, fewest changed notes first,
/// with their sidecars.
fn earlier(file: &Path, edits: &NoteEdits) -> Vec<(PathBuf, Sidecar, usize)> {
	let edits = &normalized(edits);
	let seg = clip::to_segmentation(edits);
	let asked = clip::to_edits(edits);
	let list: Vec<String> = std::fs::read(index_path(file))
		.ok()
		.and_then(|b| serde_json::from_slice(&b).ok())
		.unwrap_or_default();
	let mut out: Vec<(PathBuf, Sidecar, usize)> = list
		.iter()
		.filter_map(|name| {
			let path = render_dir().join(name);
			let side: Sidecar =
				serde_json::from_slice(&std::fs::read(sidecar_path(&path)).ok()?).ok()?;
			if !path.is_file() || clip::to_segmentation(&side.asked) != seg {
				return None;
			}
			let theirs = clip::to_edits(&side.asked);
			let changed = (0..seg.notes.len())
				.filter(|&i| theirs.note(i) != asked.note(i))
				.count() + usize::from(theirs.sibilant_gain_db != asked.sibilant_gain_db)
				* seg.notes.len();
			Some((path, side, changed))
		})
		.collect();
	out.sort_by_key(|e| e.2);
	out
}

/// `edits` as a sidecar reads them back (JSON does not give every float back to the
/// last bit), so a render's notes compare equal to the ones it was made for.
fn normalized(edits: &NoteEdits) -> NoteEdits {
	serde_json::to_vec(edits)
		.ok()
		.and_then(|b| serde_json::from_slice(&b).ok())
		.unwrap_or_else(|| edits.clone())
}

/// Records `render` (made for `edits`) as `file`'s latest.
fn remember(file: &Path, render: &Path, side: &Sidecar) -> std::io::Result<()> {
	let write = |path: &Path, bytes: &[u8]| -> std::io::Result<()> {
		let temp = path.with_extension(format!("tmp-{}", std::process::id()));
		std::fs::write(&temp, bytes)?;
		std::fs::rename(&temp, path)
	};
	write(
		&sidecar_path(render),
		&serde_json::to_vec(side).unwrap_or_default(),
	)?;
	let name = render
		.file_name()
		.map(|n| n.to_string_lossy().into_owned())
		.unwrap_or_default();
	let index = index_path(file);
	let mut list: Vec<String> = std::fs::read(&index)
		.ok()
		.and_then(|b| serde_json::from_slice(&b).ok())
		.unwrap_or_default();
	list.retain(|n| n != &name);
	list.insert(0, name);
	list.truncate(INDEX_LEN);
	write(&index, &serde_json::to_vec(&list).unwrap_or_default())
}

/// Pitch marks of the last file rendered (a burst of edits renders the same take again
/// and again).
static MARKS: std::sync::Mutex<Option<(String, std::sync::Arc<Marks>)>> =
	std::sync::Mutex::new(None);

fn marks_of(
	file: &Path,
	mono: &[f32],
	analysis: &cadenza_notes::analysis::Analysis,
) -> std::sync::Arc<Marks> {
	let key = hex(file_hasher(file));
	let mut slot = MARKS.lock().unwrap_or_else(|e| e.into_inner());
	if let Some((k, m)) = slot.as_ref()
		&& *k == key
	{
		return m.clone();
	}
	let marks = std::sync::Arc::new(pitch_marks(mono, analysis));
	*slot = Some((key, marks.clone()));
	marks
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
	/// Residuals of the notes measured: all of them, or only the changed ones when
	/// `local`.
	pub residuals: Vec<Residual>,
	/// Made from an earlier render by re-rendering only around the changed notes.
	pub local: bool,
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
	// The notes as every sidecar holds them.
	let edits = &normalized(edits);
	let seg = clip::to_segmentation(edits);
	let asked = clip::to_edits(edits);
	let marks = marks_of(file, &mono, &analysis);
	let opts = VerifyOptions::default();
	let mut quick = None;
	if let Some((base, side, _)) = earlier(file, edits).into_iter().next()
		&& let Ok(previous) = decode_file(&base)
		&& previous.sample_rate() == rate
	{
		let theirs = clip::to_edits(&side.asked);
		quick = resynth::rerender_verified(
			&channels,
			&analysis,
			&seg,
			&marks,
			Previous {
				channels: previous.channels(),
				asked: &theirs,
				effective: &side.effective,
			},
			&asked,
			&opts,
		)
		.map_err(|e| fail(e.to_string()))?;
	}
	let local = quick.is_some();
	let verified = match quick {
		Some(v) => v,
		None => resynth::verified_with(&channels, &analysis, &seg, &marks, &asked, &opts)
			.map_err(|e| fail(e.to_string()))?,
	};
	let (rendered, residuals) = (verified.render, verified.residuals);
	let out = AudioBuffer::from_planar(rate, rendered.channels).map_err(|e| fail(e.to_string()))?;
	std::fs::create_dir_all(render_dir()).map_err(|e| fail(e.to_string()))?;
	let temp = path.with_extension(format!("tmp-{}.wav", std::process::id()));
	export(&out, &temp, &ExportOptions::wav(WavSampleFormat::Float32))
		.map_err(|e| fail(e.to_string()))?;
	std::fs::rename(&temp, &path).map_err(|e| fail(e.to_string()))?;
	let side = Sidecar {
		asked: edits.clone(),
		effective: verified.effective,
	};
	// Only a speed-up for the next render: not worth failing this one.
	let _ = remember(file, &path, &side);
	Ok(NoteRender {
		path,
		residuals,
		local,
	})
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
		// Meanwhile the closest earlier render (the clip one edit ago) is nearer to what
		// was asked than the recording.
		let most = (edits.notes.len() / 4).max(1);
		if let Some((path, _, _)) = earlier(file, edits)
			.into_iter()
			.next()
			.filter(|e| e.2 <= most)
		{
			warnings.push(format!(
				"{owner}: {PENDING_WARNING}; it plays the previous render until then"
			));
			return path;
		}
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
