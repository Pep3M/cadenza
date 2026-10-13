//! Offline rendering and mix analysis, run on a blocking task so the session mutex is
//! never held while the engine is crunching audio.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use cadenza_core::{Tick, TrackId};
use cadenza_engine::{
	BandEnergy, ExportReport, ExportSpec, LevelStats, Metadata, MixReport, Normalize,
	PluginHosting, RenderOptions, StemSet, export_options_for_path, export_project,
};
use cadenza_model::{Command, Project, TrackKind};
use cadenza_plugin_host::PluginCatalog;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::convert::beats_to_tick;
use crate::error::McpError;

pub(crate) fn build_options(
	start_beat: Option<f64>,
	end_beat: Option<f64>,
	sample_rate: Option<u32>,
	plugins: Option<Arc<PluginCatalog>>,
	hosting: PluginHosting,
) -> RenderOptions {
	RenderOptions {
		sample_rate: sample_rate.unwrap_or(48_000),
		start: beats_to_tick(start_beat.unwrap_or(0.0)),
		end: end_beat.map(beats_to_tick),
		plugins,
		hosting,
		..RenderOptions::default()
	}
}

pub(crate) fn round1(x: f32) -> f32 {
	(x * 10.0).round() / 10.0
}

fn compact_bands(b: BandEnergy) -> BandEnergy {
	BandEnergy {
		low: round1(b.low),
		low_mid: round1(b.low_mid),
		mid: round1(b.mid),
		high_mid: round1(b.high_mid),
		high: round1(b.high),
	}
}

fn compact_level(mut l: LevelStats) -> LevelStats {
	l.peak_db = round1(l.peak_db);
	l.true_peak_db = round1(l.true_peak_db);
	l.rms_db = round1(l.rms_db);
	l.lufs_integrated = round1(l.lufs_integrated);
	l.lufs_short_term_max = round1(l.lufs_short_term_max);
	l.crest_factor_db = round1(l.crest_factor_db);
	l.band_energy_db = compact_bands(l.band_energy_db);
	l
}

/// Rounds every dB-valued field to 0.1 dB so reports stay compact; counts, ids and
/// warnings are kept as-is.
fn compact_report(mut report: MixReport) -> MixReport {
	report.master = compact_level(report.master);
	for t in &mut report.tracks {
		t.stats = compact_level(t.stats);
	}
	report
}

/// How exported audio is normalized.
#[derive(Debug, Clone, Copy, Default, PartialEq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum NormalizeMode {
	#[default]
	Off,
	/// Sample peak to `peak_db`.
	Peak,
	/// Integrated loudness to `lufs`, never above `true_peak_db`.
	Loudness,
}

#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
pub struct NormalizeArgs {
	pub mode: NormalizeMode,
	/// Peak target in dBFS (default -1).
	#[serde(default)]
	pub peak_db: Option<f32>,
	/// Loudness target in LUFS (default -14, streaming).
	#[serde(default)]
	pub lufs: Option<f32>,
	/// True-peak ceiling in dBTP for loudness mode (default -1).
	#[serde(default)]
	pub true_peak_db: Option<f32>,
}

impl NormalizeArgs {
	pub fn to_engine(&self) -> Normalize {
		match self.mode {
			NormalizeMode::Off => Normalize::Off,
			NormalizeMode::Peak => Normalize::Peak {
				peak_db: self.peak_db.unwrap_or(-1.0),
			},
			NormalizeMode::Loudness => Normalize::Loudness {
				lufs: self.lufs.unwrap_or(-14.0),
				true_peak_db: self.true_peak_db.unwrap_or(-1.0),
			},
		}
	}
}

/// Song information written into the files (WAV INFO, FLAC Vorbis comments, MP3 ID3).
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
pub struct MetadataArgs {
	#[serde(default)]
	pub title: Option<String>,
	#[serde(default)]
	pub artist: Option<String>,
	#[serde(default)]
	pub album: Option<String>,
	#[serde(default)]
	pub genre: Option<String>,
	/// Year or date, e.g. "2026".
	#[serde(default)]
	pub date: Option<String>,
	#[serde(default)]
	pub track_number: Option<String>,
	#[serde(default)]
	pub comment: Option<String>,
}

impl MetadataArgs {
	pub fn to_metadata(&self) -> Metadata {
		let f = |v: &Option<String>| v.clone().unwrap_or_default();
		Metadata {
			title: f(&self.title),
			artist: f(&self.artist),
			album: f(&self.album),
			genre: f(&self.genre),
			date: f(&self.date),
			track_number: f(&self.track_number),
			comment: f(&self.comment),
		}
	}
}

/// The export range from beats or (1-based, inclusive) bars; `None` end = song end.
pub fn resolve_range(
	project: &Project,
	start_beat: Option<f64>,
	end_beat: Option<f64>,
	start_bar: Option<u32>,
	end_bar: Option<u32>,
) -> Result<(Tick, Option<Tick>), McpError> {
	if (start_beat.is_some() || end_beat.is_some()) && (start_bar.is_some() || end_bar.is_some()) {
		return Err(McpError::Invalid(
			"give the range in beats (start_beat/end_beat) or in bars (start_bar/end_bar), not both"
				.into(),
		));
	}
	let meter = project.meter();
	if start_bar == Some(0) || end_bar == Some(0) {
		return Err(McpError::Invalid(
			"bars are numbered from 1 (start_bar 1 = the song start)".into(),
		));
	}
	if let (Some(a), Some(b)) = (start_bar, end_bar)
		&& b < a
	{
		return Err(McpError::Invalid(format!(
			"end_bar {b} is before start_bar {a}; the end bar is included, so use end_bar >= start_bar"
		)));
	}
	let start = match (start_beat, start_bar) {
		(Some(b), _) => beats_to_tick(b),
		(_, Some(n)) => meter.bar_start(i64::from(n) - 1),
		_ => Tick::ZERO,
	};
	let end = match (end_beat, end_bar) {
		(Some(b), _) => Some(beats_to_tick(b)),
		(_, Some(n)) => Some(meter.bar_start(i64::from(n))),
		_ => None,
	};
	Ok((start, end))
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct RenderArgs {
	/// Output path; the extension picks the format: .wav, .flac or .mp3 (MP3 only when
	/// this build has it). Stereo.
	pub path: String,
	#[serde(default)]
	pub start_beat: Option<f64>,
	/// Omit to render to the song's end plus the longest device tail.
	#[serde(default)]
	pub end_beat: Option<f64>,
	/// Range in bars instead of beats: first bar (1 = song start).
	#[serde(default)]
	pub start_bar: Option<u32>,
	/// Last bar, included.
	#[serde(default)]
	pub end_bar: Option<u32>,
	/// Append the effect tail after the end (default: true for the whole song, false for a
	/// range with an end).
	#[serde(default)]
	pub tail: Option<bool>,
	/// Render sample rate; defaults to 48000.
	#[serde(default)]
	pub sample_rate: Option<u32>,
	/// Resample the file to this rate (e.g. 44100 for CD); default = the render rate.
	#[serde(default)]
	pub output_sample_rate: Option<u32>,
	/// WAV: 16, 24 or 32 (float, default); FLAC: 16 or 24 (default). 16/24-bit output is
	/// TPDF-dithered. Not used for MP3.
	#[serde(default)]
	pub bit_depth: Option<u32>,
	/// MP3 bitrate in kbps, 32..=320 (default 320).
	#[serde(default)]
	pub mp3_bitrate_kbps: Option<u16>,
	/// Normalize the file (peak, or loudness with a true-peak ceiling).
	#[serde(default)]
	pub normalize: Option<NormalizeArgs>,
	#[serde(default)]
	pub metadata: Option<MetadataArgs>,
}

/// The `render` result: the mix analysis plus what was written.
#[derive(Debug, Serialize, JsonSchema)]
pub struct RenderOutput {
	#[serde(flatten)]
	pub report: MixReport,
	pub path: String,
	/// Normalization gain applied (dB).
	pub gain_db: f32,
	/// Levels of the written file (after normalization/resampling).
	pub file: LevelStats,
	#[serde(skip_serializing_if = "Vec::is_empty")]
	pub notes: Vec<String>,
}

/// Renders `project` to an audio file on a blocking thread pool thread. `plugins` is needed
/// for projects with plugin devices (see `Session::plugins_for_project`); `hosting` says
/// where they run (see `Session::plugin_hosting`). Plugin crashes during the render are
/// reported in the report's warnings.
pub async fn render(
	project: Project,
	plugins: Option<Arc<PluginCatalog>>,
	hosting: PluginHosting,
	args: RenderArgs,
) -> Result<RenderOutput, McpError> {
	let path = PathBuf::from(args.path.trim());
	let format = export_options_for_path(&path, args.bit_depth, args.mp3_bitrate_kbps)?;
	let (start, end) = resolve_range(
		&project,
		args.start_beat,
		args.end_beat,
		args.start_bar,
		args.end_bar,
	)?;
	let opts = build_options(None, None, args.sample_rate, plugins, hosting);
	let folder = path
		.parent()
		.filter(|p| !p.as_os_str().is_empty())
		.map(PathBuf::from)
		.unwrap_or_else(|| PathBuf::from("."));
	let ext = cadenza_engine::format_extension(&format.format);
	let name = path
		.file_name()
		.map(|n| n.to_string_lossy().into_owned())
		.unwrap_or_default();
	let name = name
		.strip_suffix(&format!(".{ext}"))
		.or_else(|| name.rsplit_once('.').map(|(a, _)| a))
		.unwrap_or(&name)
		.to_owned();
	let spec = ExportSpec {
		start,
		end,
		tail: args.tail.unwrap_or(end.is_none()),
		mixdown: true,
		format,
		sample_rate: args.output_sample_rate,
		normalize: args
			.normalize
			.as_ref()
			.map_or(Normalize::Off, NormalizeArgs::to_engine),
		metadata: args
			.metadata
			.as_ref()
			.map(MetadataArgs::to_metadata)
			.unwrap_or_default(),
		folder,
		name,
		..ExportSpec::default()
	};
	let report =
		tokio::task::spawn_blocking(move || export_project(&project, &opts, &spec, &mut |_| true))
			.await
			.map_err(|e| McpError::Invalid(format!("render task panicked: {e}")))??;
	let file = report
		.files
		.into_iter()
		.next()
		.ok_or_else(|| McpError::Invalid("the render wrote no file".into()))?;
	Ok(RenderOutput {
		report: compact_report(report.mix),
		path: file.path.display().to_string(),
		gain_db: round1(file.gain_db),
		file: compact_level(file.stats),
		notes: report.notes,
	})
}

/// Which stems `export_stems` writes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StemKind {
	/// One per instrument/audio track.
	#[default]
	Tracks,
	/// One per bus and per track going straight to the master (they sum to the mix).
	Buses,
	/// One per track in track_ids.
	Selected,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ExportStemsArgs {
	/// Folder for the files (default: the project's Renders/ folder).
	#[serde(default)]
	pub folder: Option<String>,
	/// Base file name (default: the project name): stems are "<name> - <track>.<ext>",
	/// the mixdown "<name>.<ext>".
	#[serde(default)]
	pub name: Option<String>,
	#[serde(default)]
	pub stems: StemKind,
	/// Tracks for stems: selected.
	#[serde(default)]
	pub track_ids: Vec<TrackId>,
	/// Also write the full mixdown (default false).
	#[serde(default)]
	pub include_mixdown: Option<bool>,
	/// Play every stem through the master inserts and fader (default false: stems are
	/// the raw channel outputs, which sum to the mix before the master chain).
	#[serde(default)]
	pub through_master: Option<bool>,
	/// Tap stems before the channel fader (default false = post-fader, as mixed).
	#[serde(default)]
	pub pre_fader: Option<bool>,
	/// "wav" (default), "flac" or "mp3".
	#[serde(default)]
	pub format: Option<String>,
	#[serde(default)]
	pub bit_depth: Option<u32>,
	#[serde(default)]
	pub mp3_bitrate_kbps: Option<u16>,
	#[serde(default)]
	pub start_beat: Option<f64>,
	#[serde(default)]
	pub end_beat: Option<f64>,
	#[serde(default)]
	pub start_bar: Option<u32>,
	#[serde(default)]
	pub end_bar: Option<u32>,
	/// Append the effect tail (default: true for the whole song, false with an end).
	#[serde(default)]
	pub tail: Option<bool>,
	#[serde(default)]
	pub sample_rate: Option<u32>,
	#[serde(default)]
	pub output_sample_rate: Option<u32>,
	/// Normalization; stems share the mix's gain unless normalize_individually.
	#[serde(default)]
	pub normalize: Option<NormalizeArgs>,
	#[serde(default)]
	pub normalize_individually: Option<bool>,
	#[serde(default)]
	pub metadata: Option<MetadataArgs>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ExportedFileOut {
	pub path: String,
	pub name: String,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub track_id: Option<TrackId>,
	pub gain_db: f32,
	pub peak_db: f32,
	pub true_peak_db: f32,
	pub lufs_integrated: f32,
	pub duration_seconds: f64,
	pub sample_rate: u32,
	pub bytes: u64,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ExportStemsOutput {
	pub files: Vec<ExportedFileOut>,
	pub total_bytes: u64,
	/// Master levels of the render before normalization.
	pub master: LevelStats,
	#[serde(skip_serializing_if = "Vec::is_empty")]
	pub warnings: Vec<String>,
	#[serde(skip_serializing_if = "Vec::is_empty")]
	pub notes: Vec<String>,
}

/// Builds the engine job for `export_stems` (`default_folder`: the project's Renders/).
pub fn stems_spec(
	project: &Project,
	args: &ExportStemsArgs,
	default_folder: Option<PathBuf>,
) -> Result<ExportSpec, McpError> {
	let folder = match &args.folder {
		Some(f) if !f.trim().is_empty() => PathBuf::from(f.trim()),
		_ => default_folder.ok_or_else(|| {
			McpError::Invalid(
				"pass 'folder' (the project is not saved, so it has no Renders/ folder)".into(),
			)
		})?,
	};
	let ext = args
		.format
		.as_deref()
		.unwrap_or("wav")
		.trim()
		.to_ascii_lowercase();
	let format = export_options_for_path(
		Path::new(&format!("x.{ext}")),
		args.bit_depth,
		args.mp3_bitrate_kbps,
	)?;
	let stems = match args.stems {
		StemKind::Tracks => StemSet::Tracks,
		StemKind::Buses => StemSet::Buses,
		StemKind::Selected => StemSet::Selected(args.track_ids.clone()),
	};
	let (start, end) = resolve_range(
		project,
		args.start_beat,
		args.end_beat,
		args.start_bar,
		args.end_bar,
	)?;
	Ok(ExportSpec {
		start,
		end,
		tail: args.tail.unwrap_or(end.is_none()),
		mixdown: args.include_mixdown.unwrap_or(false),
		stems,
		stems_through_master: args.through_master.unwrap_or(false),
		stems_pre_fader: args.pre_fader.unwrap_or(false),
		format,
		sample_rate: args.output_sample_rate,
		normalize: args
			.normalize
			.as_ref()
			.map_or(Normalize::Off, NormalizeArgs::to_engine),
		normalize_stems_individually: args.normalize_individually.unwrap_or(false),
		metadata: args
			.metadata
			.as_ref()
			.map(MetadataArgs::to_metadata)
			.unwrap_or_default(),
		folder,
		name: args
			.name
			.clone()
			.filter(|n| !n.trim().is_empty())
			.unwrap_or_else(|| project.name.clone()),
	})
}

/// The compact AI-facing view of an export.
pub fn export_output(report: ExportReport) -> ExportStemsOutput {
	ExportStemsOutput {
		total_bytes: report.bytes(),
		files: report
			.files
			.into_iter()
			.map(|f| ExportedFileOut {
				path: f.path.display().to_string(),
				name: f.name,
				track_id: f.track_id,
				gain_db: round1(f.gain_db),
				peak_db: round1(f.stats.peak_db),
				true_peak_db: round1(f.stats.true_peak_db),
				lufs_integrated: round1(f.stats.lufs_integrated),
				duration_seconds: f.duration_seconds,
				sample_rate: f.sample_rate,
				bytes: f.bytes,
			})
			.collect(),
		master: compact_level(report.mix.master),
		warnings: report.mix.warnings,
		notes: report.notes,
	}
}

/// Renders stems (and optionally the mixdown) on a blocking thread pool thread.
pub async fn export_stems(
	project: Project,
	plugins: Option<Arc<PluginCatalog>>,
	hosting: PluginHosting,
	args: ExportStemsArgs,
	default_folder: Option<PathBuf>,
) -> Result<ExportStemsOutput, McpError> {
	let spec = stems_spec(&project, &args, default_folder)?;
	let opts = build_options(None, None, args.sample_rate, plugins, hosting);
	let report =
		tokio::task::spawn_blocking(move || export_project(&project, &opts, &spec, &mut |_| true))
			.await
			.map_err(|e| McpError::Invalid(format!("export task panicked: {e}")))??;
	Ok(export_output(report))
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct FreezeTrackArgs {
	pub track_id: TrackId,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct FreezeTrackOutput {
	/// The render the frozen track now plays.
	pub file: String,
	pub duration_seconds: f64,
}

/// Where `freeze_track` writes: the project's `.cadenza/freeze/`, or a temp folder for an
/// unsaved project.
fn freeze_dir(project_path: Option<&std::path::Path>) -> PathBuf {
	match project_path {
		Some(p) => cadenza_model::ProjectLayout::new(p)
			.cache_dir()
			.join("freeze"),
		None => std::env::temp_dir().join(format!("cadenza-freeze-{}", std::process::id())),
	}
}

/// Renders the track (instrument, clips, inserts, before its fader) and returns the
/// command that freezes it.
pub async fn freeze_render(
	project: Project,
	project_path: Option<PathBuf>,
	plugins: Option<Arc<PluginCatalog>>,
	hosting: PluginHosting,
	track: TrackId,
) -> Result<(Command, FreezeTrackOutput), McpError> {
	let t = project
		.track(track)
		.ok_or(cadenza_model::ModelError::TrackNotFound(track))?;
	if !matches!(t.kind, TrackKind::Instrument | TrackKind::Audio) {
		return Err(McpError::Invalid(
			"only instrument and audio tracks can be frozen".into(),
		));
	}
	let dir = freeze_dir(project_path.as_deref());
	let stem = cadenza_model::files::sanitize_file_name(&format!("{} freeze", t.name));
	let file = (1..)
		.map(|n| dir.join(format!("{stem} {n}.wav")))
		.find(|p| !p.exists())
		.expect("a free file name");
	let mut source = project.clone();
	if let Some(t) = source.tracks.iter_mut().find(|t| t.id == track) {
		t.freeze = None;
	}
	let opts = build_options(None, None, None, plugins, hosting);
	let out = file.clone();
	let report = tokio::task::spawn_blocking(move || {
		cadenza_engine::render_track(
			&source,
			&opts,
			track,
			&cadenza_engine::BounceScope::Track,
			cadenza_core::Tick::ZERO,
			&out,
		)
	})
	.await
	.map_err(|e| McpError::Invalid(format!("the render task failed: {e}")))??;
	Ok((
		Command::SetTrackFreeze {
			track,
			freeze: Some(cadenza_model::Freeze { file: file.clone() }),
		},
		FreezeTrackOutput {
			file: file.display().to_string(),
			duration_seconds: report.duration_seconds,
		},
	))
}

#[cfg(test)]
mod freeze_tests {
	use super::*;
	use cadenza_model::{Clip, Device, Document, Note, Track};

	#[tokio::test]
	async fn freezing_renders_and_points_the_track_at_the_render() {
		let mut p = Project::new("f");
		let mut t = Track::new("Lead", TrackKind::Instrument)
			.with_instrument(Device::builtin("poly_synth", "Synth"));
		t.clips.push(
			Clip::midi("A", Tick(0), Tick(960 * 4)).with_notes([Note::new(
				60,
				Tick(0),
				Tick(960),
				0.8,
			)]),
		);
		let id = t.id;
		p.tracks.push(t);
		let (cmd, out) = freeze_render(p.clone(), None, None, PluginHosting::in_process(), id)
			.await
			.unwrap();
		assert!(out.duration_seconds >= 2.0);
		assert!(std::path::Path::new(&out.file).exists());
		let mut doc = Document::new(p);
		doc.execute(cmd).unwrap();
		assert!(doc.project().summary().tracks[0].frozen);
		let _ = std::fs::remove_file(&out.file);
	}
}
