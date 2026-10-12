//! Offline rendering and mix analysis, run on a blocking task so the session mutex is
//! never held while the engine is crunching audio.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use cadenza_core::{Tick, TrackId};
use cadenza_engine::{
	BandEnergy, ExportReport, ExportSpec, LevelStats, Metadata, MixReport, Normalize,
	PluginHosting, RenderOptions, StemSet, analyze_mix as engine_analyze_mix,
	export_options_for_path, export_project,
};
use cadenza_model::{Project, TrackKind};
use cadenza_plugin_host::PluginCatalog;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::convert::beats_to_tick;
use crate::error::McpError;

fn build_options(
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

fn round1(x: f32) -> f32 {
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
	let bar = project.time_signature.ticks_per_bar();
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
		(_, Some(n)) => Tick(bar.0 * i64::from(n - 1)),
		_ => Tick::ZERO,
	};
	let end = match (end_beat, end_bar) {
		(Some(b), _) => Some(beats_to_tick(b)),
		(_, Some(n)) => Some(Tick(bar.0 * i64::from(n))),
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
pub struct AnalyzeMixArgs {
	#[serde(default)]
	pub start_beat: Option<f64>,
	#[serde(default)]
	pub end_beat: Option<f64>,
	/// Defaults to 48000.
	#[serde(default)]
	pub sample_rate: Option<u32>,
	/// Integrated-loudness target in LUFS used to flag the mix as too loud/quiet.
	/// Defaults to -14 (a typical streaming-platform target).
	#[serde(default)]
	pub target_lufs: Option<f32>,
	/// Tonal-balance reference the low/low-mid/high-mid/high band observations are compared
	/// against. Defaults to `pop`.
	#[serde(default)]
	pub genre_profile: Option<GenreProfile>,
}

/// Genre a mix's tonal balance is compared against (see [`reference_curve`]).
#[derive(Debug, Clone, Copy, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum GenreProfile {
	#[default]
	Pop,
	Electronic,
	Hiphop,
	Rock,
	Acoustic,
	Orchestral,
}

/// Typical band energy relative to the mid band (dB), used as a reference tonal balance
/// rather than a flat "mid is the only correct level" rule: modern mixes routinely carry a
/// hotter low end and a rolled-off high-mid/high than a literally flat spectrum would.
struct ToneBalance {
	low: f32,
	low_mid: f32,
	high_mid: f32,
	high: f32,
}

/// Reference curves, dB relative to the mid band. Starting points, not gospel — tune freely.
fn reference_curve(genre: GenreProfile) -> ToneBalance {
	match genre {
		// Modern pop/electronic masters commonly carry a bass-forward, slightly
		// treble-rolled-off balance.
		GenreProfile::Pop => ToneBalance {
			low: 7.0,
			low_mid: 3.0,
			high_mid: -5.0,
			high: -8.0,
		},
		GenreProfile::Electronic => ToneBalance {
			low: 9.0,
			low_mid: 2.0,
			high_mid: -4.0,
			high: -6.0,
		},
		GenreProfile::Hiphop => ToneBalance {
			low: 10.0,
			low_mid: 2.0,
			high_mid: -6.0,
			high: -9.0,
		},
		GenreProfile::Rock => ToneBalance {
			low: 4.0,
			low_mid: 2.0,
			high_mid: -2.0,
			high: -6.0,
		},
		GenreProfile::Acoustic => ToneBalance {
			low: 2.0,
			low_mid: 1.0,
			high_mid: -3.0,
			high: -7.0,
		},
		GenreProfile::Orchestral => ToneBalance {
			low: 1.0,
			low_mid: 0.0,
			high_mid: -4.0,
			high: -9.0,
		},
	}
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct AnalyzeMixOutput {
	#[serde(flatten)]
	pub report: MixReport,
	/// Plain-language hints computed from the report, meant to guide the next mix move.
	pub observations: Vec<String>,
}

// Thresholds for `observations`, documented here so they are easy to retune.
/// Headroom an export/broadcast limiter typically wants before the true-peak ceiling.
const TRUE_PEAK_CEILING_DBTP: f32 = -1.0;
/// How many LU away from `target_lufs` before flagging the integrated loudness.
const LOUDNESS_TOLERANCE_LU: f32 = 1.5;
/// How many dB above the median source-track RMS before a track is "much louder than the
/// rest".
const LOUD_TRACK_MARGIN_DB: f32 = 6.0;
/// L/R correlation above this is treated as effectively mono.
const NEAR_MONO_CORRELATION: f32 = 0.95;
/// Loudness below which a track is treated as effectively silent and excluded from the
/// "loud track" comparison (bus returns can legitimately sit far below this).
const SILENT_TRACK_LUFS: f32 = -70.0;
/// How many dB a band may drift from the genre reference curve ([`reference_curve`]) before
/// it is flagged as off-balance.
const TONE_BALANCE_TOLERANCE_DB: f32 = 4.0;
/// Crest factor below which the master is considered over-compressed (little difference
/// between peak and average level).
const OVER_COMPRESSED_CREST_DB: f32 = 8.0;
/// Crest factor above which the master is considered very dynamic.
const VERY_DYNAMIC_CREST_DB: f32 = 20.0;

fn observations(report: &MixReport, target_lufs: f32, genre: GenreProfile) -> Vec<String> {
	let mut notes = Vec::new();
	let m = &report.master;

	if m.true_peak_db > TRUE_PEAK_CEILING_DBTP {
		notes.push(format!(
			"master true peak is {:.1} dBTP, above the {TRUE_PEAK_CEILING_DBTP:.1} dBTP ceiling; leave more headroom before the final limiter/export.",
			m.true_peak_db
		));
	}
	let loudness_gap = m.lufs_integrated - target_lufs;
	if loudness_gap.abs() > LOUDNESS_TOLERANCE_LU {
		let direction = if loudness_gap > 0.0 {
			"louder"
		} else {
			"quieter"
		};
		notes.push(format!(
			"integrated loudness is {:.1} LUFS, {:.1} LU {direction} than the {target_lufs:.1} LUFS target; adjust the master gain.",
			m.lufs_integrated,
			loudness_gap.abs()
		));
	}
	if m.clipped_samples > 0 {
		notes.push(format!(
			"master is clipping ({} sample(s) at or above full scale); reduce gain upstream of the limiter.",
			m.clipped_samples
		));
	}
	if m.stereo_correlation > NEAR_MONO_CORRELATION {
		notes.push(format!(
			"master is near-mono (L/R correlation {:.2}); consider widening the mix.",
			m.stereo_correlation
		));
	} else if m.stereo_correlation < 0.0 {
		notes.push(format!(
			"master L/R correlation is negative ({:.2}); check for phase cancellation.",
			m.stereo_correlation
		));
	}
	// Compare tonal balance against a genre reference curve rather than a flat "mid is the
	// only right level" rule: e.g. 7-9 dB of low-band lift over mid is normal, not build-up,
	// in modern pop/electronic masters.
	let bands = &m.band_energy_db;
	let curve = reference_curve(genre);
	let genre_name = match genre {
		GenreProfile::Pop => "pop",
		GenreProfile::Electronic => "electronic",
		GenreProfile::Hiphop => "hip-hop",
		GenreProfile::Rock => "rock",
		GenreProfile::Acoustic => "acoustic",
		GenreProfile::Orchestral => "orchestral",
	};
	for (band_name, actual_offset, reference_offset) in [
		("low end", bands.low - bands.mid, curve.low),
		("low-mid", bands.low_mid - bands.mid, curve.low_mid),
		("high-mid", bands.high_mid - bands.mid, curve.high_mid),
		("high end", bands.high - bands.mid, curve.high),
	] {
		let delta = actual_offset - reference_offset;
		if delta.abs() > TONE_BALANCE_TOLERANCE_DB {
			let direction = if delta > 0.0 { "above" } else { "below" };
			notes.push(format!(
				"{band_name} is {:.1} dB {direction} a typical {genre_name} balance (vs. mid band); re-check EQ in that range.",
				delta.abs()
			));
		}
	}

	if m.crest_factor_db < OVER_COMPRESSED_CREST_DB {
		notes.push(format!(
			"master crest factor is {:.1} dB (< {OVER_COMPRESSED_CREST_DB:.0} dB); the mix may be over-compressed/limited, losing punch.",
			m.crest_factor_db
		));
	} else if m.crest_factor_db > VERY_DYNAMIC_CREST_DB && m.lufs_integrated < target_lufs {
		notes.push(format!(
			"master crest factor is {:.1} dB (> {VERY_DYNAMIC_CREST_DB:.0} dB) and loudness is below target; the mix is very dynamic and could take more bus compression or limiting.",
			m.crest_factor_db
		));
	}

	// Only compare source tracks (not bus returns, which legitimately sit far quieter, e.g.
	// a reverb/delay send) and ignore effectively silent ones, using the median so a handful
	// of outliers (like those same buses, if included) can't skew the reference.
	let mut source_rms: Vec<f32> = report
		.tracks
		.iter()
		.filter(|t| {
			matches!(t.kind, TrackKind::Instrument | TrackKind::Audio)
				&& t.stats.lufs_integrated > SILENT_TRACK_LUFS
		})
		.map(|t| t.stats.rms_db)
		.collect();
	if source_rms.len() > 1 {
		source_rms.sort_by(|a, b| a.total_cmp(b));
		let mid = source_rms.len() / 2;
		let median_rms = if source_rms.len().is_multiple_of(2) {
			(source_rms[mid - 1] + source_rms[mid]) * 0.5
		} else {
			source_rms[mid]
		};
		for t in &report.tracks {
			if !matches!(t.kind, TrackKind::Instrument | TrackKind::Audio)
				|| t.stats.lufs_integrated <= SILENT_TRACK_LUFS
			{
				continue;
			}
			if t.stats.rms_db - median_rms > LOUD_TRACK_MARGIN_DB {
				notes.push(format!(
					"track '{}' is {:.1} dB louder than the median source track; consider turning it down or automating a dip.",
					t.name,
					t.stats.rms_db - median_rms
				));
			}
		}
	}
	notes.extend(
		report
			.warnings
			.iter()
			.map(|w| format!("compile warning: {w}")),
	);
	notes
}

/// Renders and measures `project` without writing audio anywhere, on a blocking thread
/// pool thread.
pub async fn analyze_mix(
	project: Project,
	plugins: Option<Arc<PluginCatalog>>,
	hosting: PluginHosting,
	args: AnalyzeMixArgs,
) -> Result<AnalyzeMixOutput, McpError> {
	let opts = build_options(
		args.start_beat,
		args.end_beat,
		args.sample_rate,
		plugins,
		hosting,
	);
	let target_lufs = args.target_lufs.unwrap_or(-14.0);
	let genre = args.genre_profile.unwrap_or_default();
	let report = tokio::task::spawn_blocking(move || engine_analyze_mix(&project, &opts))
		.await
		.map_err(|e| McpError::Invalid(format!("analyze task panicked: {e}")))??;
	let observations = observations(&report, target_lufs, genre);
	Ok(AnalyzeMixOutput {
		report: compact_report(report),
		observations,
	})
}

#[cfg(test)]
mod tests {
	use cadenza_core::TrackId;

	use super::*;

	fn flat_stats(rms_db: f32, crest_db: f32, lufs: f32) -> LevelStats {
		LevelStats {
			peak_db: rms_db + crest_db,
			true_peak_db: rms_db + crest_db,
			rms_db,
			lufs_integrated: lufs,
			lufs_short_term_max: lufs,
			crest_factor_db: crest_db,
			clipped_samples: 0,
			band_energy_db: BandEnergy {
				low: -10.0,
				low_mid: -10.0,
				mid: -10.0,
				high_mid: -10.0,
				high: -10.0,
			},
			stereo_correlation: 0.5,
		}
	}

	fn track(name: &str, kind: TrackKind, stats: LevelStats) -> cadenza_engine::TrackLevel {
		cadenza_engine::TrackLevel {
			track_id: TrackId::new(),
			name: name.into(),
			kind,
			stats,
		}
	}

	fn report_with(master: LevelStats, tracks: Vec<cadenza_engine::TrackLevel>) -> MixReport {
		MixReport {
			sample_rate: 48_000,
			duration_seconds: 1.0,
			latency_samples: 0,
			warnings: Vec::new(),
			master,
			tracks,
		}
	}

	#[test]
	fn loud_track_comparison_ignores_bus_returns_and_uses_the_median() {
		let report = report_with(
			flat_stats(-14.0, 12.0, -14.0),
			vec![
				track(
					"Drums",
					TrackKind::Instrument,
					flat_stats(-16.0, 10.0, -16.0),
				),
				track(
					"Bass",
					TrackKind::Instrument,
					flat_stats(-15.0, 10.0, -15.0),
				),
				track("Lead", TrackKind::Instrument, flat_stats(-6.0, 10.0, -6.0)),
				// A quiet reverb return: without filtering, this drags the average down and
				// makes the loud lead look even louder than it already is; with the median
				// over source tracks only, it must not participate at all.
				track("Reverb Bus", TrackKind::Bus, flat_stats(-40.0, 10.0, -40.0)),
			],
		);
		let notes = observations(&report, -14.0, GenreProfile::Pop);
		assert!(
			notes.iter().any(|n| n.contains("Lead")),
			"the clearly loud source track should be flagged: {notes:?}"
		);
		assert!(
			!notes.iter().any(|n| n.contains("Reverb Bus")),
			"a quiet bus return must never be flagged as loud: {notes:?}"
		);
		assert!(
			!notes
				.iter()
				.any(|n| n.contains("Drums") || n.contains("Bass")),
			"tracks close to the median must not be flagged: {notes:?}"
		);
	}

	#[test]
	fn silent_source_tracks_are_excluded_from_the_comparison() {
		let report = report_with(
			flat_stats(-14.0, 12.0, -14.0),
			vec![
				track(
					"Drums",
					TrackKind::Instrument,
					flat_stats(-14.0, 10.0, -14.0),
				),
				// An empty/silent clip track: must not be treated as "the median" or flagged.
				track(
					"Unused",
					TrackKind::Instrument,
					flat_stats(-90.0, 0.0, -144.0),
				),
			],
		);
		let notes = observations(&report, -14.0, GenreProfile::Pop);
		assert!(!notes.iter().any(|n| n.contains("Unused")));
	}

	#[test]
	fn pop_reference_curve_tolerates_a_typical_bass_forward_balance() {
		// low +7, low_mid +3, high_mid -5, high -8 over mid: right on the pop reference
		// curve, so no tonal-balance observation should fire.
		let mut m = flat_stats(-14.0, 12.0, -14.0);
		m.band_energy_db = BandEnergy {
			low: -3.0,
			low_mid: -7.0,
			mid: -10.0,
			high_mid: -15.0,
			high: -18.0,
		};
		let report = report_with(m, Vec::new());
		let notes = observations(&report, -14.0, GenreProfile::Pop);
		assert!(
			!notes.iter().any(|n| n.contains("balance")),
			"a textbook pop balance must not be flagged: {notes:?}"
		);
	}

	#[test]
	fn low_end_far_outside_the_reference_curve_is_flagged() {
		let mut m = flat_stats(-14.0, 12.0, -14.0);
		m.band_energy_db = BandEnergy {
			low: 2.0, // +12 dB over mid, well above pop's +7 reference and tolerance
			low_mid: -10.0,
			mid: -10.0,
			high_mid: -10.0,
			high: -10.0,
		};
		let report = report_with(m, Vec::new());
		let notes = observations(&report, -14.0, GenreProfile::Pop);
		assert!(
			notes
				.iter()
				.any(|n| n.contains("low end") && n.contains("pop")),
			"{notes:?}"
		);
	}

	#[test]
	fn over_compressed_master_is_flagged() {
		let report = report_with(flat_stats(-10.0, 4.0, -10.0), Vec::new());
		let notes = observations(&report, -14.0, GenreProfile::Pop);
		assert!(
			notes.iter().any(|n| n.contains("over-compressed")),
			"{notes:?}"
		);
	}

	#[test]
	fn very_dynamic_and_quiet_master_is_flagged() {
		let report = report_with(flat_stats(-30.0, 24.0, -30.0), Vec::new());
		let notes = observations(&report, -14.0, GenreProfile::Pop);
		assert!(
			notes.iter().any(|n| n.contains("very dynamic")),
			"{notes:?}"
		);
	}
}
