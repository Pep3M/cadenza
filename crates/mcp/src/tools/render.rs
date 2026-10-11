//! Offline rendering and mix analysis, run on a blocking task so the session mutex is
//! never held while the engine is crunching audio.

use std::path::PathBuf;

use cadenza_engine::{
	BandEnergy, LevelStats, MixReport, RenderOptions, analyze_mix as engine_analyze_mix,
	render_to_wav,
};
use cadenza_model::Project;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::convert::beats_to_tick;
use crate::error::McpError;

fn build_options(
	start_beat: Option<f64>,
	end_beat: Option<f64>,
	sample_rate: Option<u32>,
) -> RenderOptions {
	RenderOptions {
		sample_rate: sample_rate.unwrap_or(48_000),
		start: beats_to_tick(start_beat.unwrap_or(0.0)),
		end: end_beat.map(beats_to_tick),
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

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RenderArgs {
	/// Output WAV path (32-bit float, stereo).
	pub path: String,
	#[serde(default)]
	pub start_beat: Option<f64>,
	/// Omit to render to the song's end plus the longest device tail.
	#[serde(default)]
	pub end_beat: Option<f64>,
	/// Defaults to 48000.
	#[serde(default)]
	pub sample_rate: Option<u32>,
}

/// Renders `project` to a WAV file on a blocking thread pool thread.
pub async fn render(project: Project, args: RenderArgs) -> Result<MixReport, McpError> {
	let opts = build_options(args.start_beat, args.end_beat, args.sample_rate);
	let path = PathBuf::from(args.path);
	let report = tokio::task::spawn_blocking(move || render_to_wav(&project, &opts, &path))
		.await
		.map_err(|e| McpError::Invalid(format!("render task panicked: {e}")))??;
	Ok(compact_report(report))
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
/// How many dB above the average track RMS before a track is "much louder than the rest".
const LOUD_TRACK_MARGIN_DB: f32 = 6.0;
/// L/R correlation above this is treated as effectively mono.
const NEAR_MONO_CORRELATION: f32 = 0.95;
/// How many dB the low band can sit above the mid band before flagging low-end build-up.
const LOW_BUILDUP_MARGIN_DB: f32 = 6.0;
/// How many dB the high-mid band can sit above the mid band before flagging harshness.
const HARSH_HIGH_MID_MARGIN_DB: f32 = 3.0;

fn observations(report: &MixReport, target_lufs: f32) -> Vec<String> {
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
	let bands = &m.band_energy_db;
	if bands.low - bands.mid > LOW_BUILDUP_MARGIN_DB {
		notes.push(format!(
			"low band is {:.1} dB above the mid band; the low end may be building up/muddy.",
			bands.low - bands.mid
		));
	}
	if bands.high_mid - bands.mid > HARSH_HIGH_MID_MARGIN_DB {
		notes.push(format!(
			"high-mid band is {:.1} dB above the mid band; the mix may sound harsh.",
			bands.high_mid - bands.mid
		));
	}

	if report.tracks.len() > 1 {
		let avg_rms: f32 =
			report.tracks.iter().map(|t| t.stats.rms_db).sum::<f32>() / report.tracks.len() as f32;
		for t in &report.tracks {
			if t.stats.rms_db - avg_rms > LOUD_TRACK_MARGIN_DB {
				notes.push(format!(
					"track '{}' is {:.1} dB louder than the average track; consider turning it down or automating a dip.",
					t.name,
					t.stats.rms_db - avg_rms
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
	args: AnalyzeMixArgs,
) -> Result<AnalyzeMixOutput, McpError> {
	let opts = build_options(args.start_beat, args.end_beat, args.sample_rate);
	let target_lufs = args.target_lufs.unwrap_or(-14.0);
	let report = tokio::task::spawn_blocking(move || engine_analyze_mix(&project, &opts))
		.await
		.map_err(|e| McpError::Invalid(format!("analyze task panicked: {e}")))??;
	let observations = observations(&report, target_lufs);
	Ok(AnalyzeMixOutput {
		report: compact_report(report),
		observations,
	})
}
