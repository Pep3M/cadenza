//! The sound engineer's tools: detailed mix analysis (summary + issues with fixes), one
//! channel in depth, comparison with a reference track, the mix targets of the song's style,
//! and `apply_mix_moves`, which turns a list of [`MixMove`]s into ONE undo step.
//!
//! The analysis renders offline on a blocking thread (like `render`), never touching the
//! audio thread. [`run_analysis`] / [`assess`] are shared with the app's mix assistant and
//! `cadenza mix`; [`mix_move_commands`] with the app's "fix" buttons.

use std::path::Path;
use std::sync::Arc;

use cadenza_analysis::detail::{Detail, analyze as analyze_signal};
use cadenza_analysis::issues::{
	Issue, IssueKind, LimiterState, MasterInput, TrackInput, find_issues, track_masking,
};
use cadenza_analysis::masking::MaskingPair;
use cadenza_analysis::moves::{EqKind, MixMove};
use cadenza_analysis::reference::{ReferenceComparison, compare};
use cadenza_analysis::roles::{Role, infer_role};
use cadenza_analysis::spectrum::OCTAVE_HZ;
use cadenza_analysis::targets::{MixTargets, ToneProfile};
use cadenza_core::{ParamAddress, TrackId};
use cadenza_engine::{DetailedMix, MixReport, PluginHosting, RenderOptions, analyze_mix_detailed};
use cadenza_library::{Item, Library};
use cadenza_model::{
	Channel, ChannelPatch, Command, Device, Document, Project, Send as ModelSend, SendPatch,
	TrackKind,
};
use cadenza_plugin_host::PluginCatalog;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::McpError;
use crate::session::Session;
use crate::tools::render::{build_options, round1};

/// Rounded to 0.1 for output (an `f64`, so JSON shows `-9.1`, not `-9.100000381`).
fn r1(x: f32) -> f64 {
	(f64::from(x) * 10.0).round() / 10.0
}

fn r2(x: f32) -> f64 {
	(f64::from(x) * 100.0).round() / 100.0
}

/// Undo label of [`apply_mix_moves`] unless the caller names it.
pub const MIX_PASS_LABEL: &str = "AI mix pass";

// -- targets ---------------------------------------------------------------------------

/// Overrides of the style's targets (all optional).
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
pub struct TargetArgs {
	/// Integrated loudness target, LUFS (default: the style's mix target, else -14).
	#[serde(default)]
	pub target_lufs: Option<f32>,
	/// Tonal balance reference (default: the style's, else guessed from its genres, else pop).
	#[serde(default)]
	pub genre_profile: Option<ToneProfile>,
	/// True-peak ceiling, dBTP (default -1).
	#[serde(default)]
	pub true_peak_db: Option<f32>,
}

/// The mix targets of the project: its style guide's mix section (`Project.meta.style`),
/// else streaming defaults, then the explicit overrides.
pub fn resolve_targets(project: &Project, library: &Library, args: &TargetArgs) -> MixTargets {
	let mut targets = project
		.meta
		.style
		.as_deref()
		.and_then(|id| {
			let item = library.item(id)?;
			let Item::Style(style) = &item.item else {
				return None;
			};
			let words = format!("{} {}", style.name, style.genres.join(" "));
			let levels: Vec<(String, f64)> = style
				.mix
				.levels
				.iter()
				.map(|(role, db)| (role.to_owned(), *db))
				.collect();
			Some(MixTargets::from_style(
				&item.id,
				&words,
				style.mix.lufs,
				style.mix.true_peak,
				style.mix.profile.as_deref(),
				&levels,
				&style.mix.notes,
			))
		})
		.unwrap_or_default();
	let mut overridden = false;
	if let Some(lufs) = args.target_lufs {
		targets.lufs = lufs.clamp(-40.0, 0.0);
		overridden = true;
	}
	if let Some(profile) = args.genre_profile {
		targets.profile = profile;
		overridden = true;
	}
	if let Some(tp) = args.true_peak_db {
		targets.true_peak_db = tp.clamp(-12.0, 0.0);
		overridden = true;
	}
	if overridden {
		targets.source = if targets.source == "default" {
			"arguments".into()
		} else {
			format!("{} + arguments", targets.source)
		};
	}
	targets
}

// -- channels --------------------------------------------------------------------------

/// The role of a track from its name, instrument and the library patterns of its clips.
pub fn track_role(project: &Project, library: &Library, track: TrackId) -> Role {
	let Some(t) = project.track(track) else {
		return Role::Other;
	};
	let hints: Vec<&'static str> = t
		.clips
		.iter()
		.filter_map(|c| {
			let id = c.name.rsplit_once('[')?.1.strip_suffix(']')?;
			Some(library.pattern(id)?.role.as_str())
		})
		.collect();
	infer_role(
		&t.name,
		t.instrument.as_ref().and_then(|d| d.builtin_id()),
		&hints,
	)
}

/// The first active built-in device `id` on a channel.
fn builtin<'a>(channel: &'a Channel, id: &str) -> Option<&'a Device> {
	channel
		.inserts
		.iter()
		.find(|d| !d.bypass && d.builtin_id() == Some(id))
}

/// A device parameter's value (its default when never set).
fn param(device: &Device, id: &str) -> f32 {
	device.params.get(id).copied().unwrap_or_else(|| {
		device
			.builtin_id()
			.and_then(cadenza_dsp::device_info)
			.and_then(|i| i.param(id))
			.map_or(0.0, |p| p.default)
	})
}

fn high_pass(channel: &Channel) -> Option<f32> {
	let eq = builtin(channel, "eq")?;
	(param(eq, "hp_slope") >= 1.0).then(|| param(eq, "hp_freq"))
}

/// One analyzed channel.
#[derive(Debug, Clone)]
pub struct ChannelAnalysis {
	pub track_id: TrackId,
	pub name: String,
	pub kind: TrackKind,
	pub role: Role,
	pub volume_db: f32,
	pub pan: f32,
	pub high_pass_hz: Option<f32>,
	pub detail: Detail,
}

/// Everything the mix assistant knows after one analysis pass.
#[derive(Debug, Clone)]
pub struct MixAnalysis {
	pub targets: MixTargets,
	pub report: MixReport,
	pub master: Detail,
	pub channels: Vec<ChannelAnalysis>,
	/// Indices into `channels`.
	pub masking: Vec<MaskingPair>,
	pub issues: Vec<Issue>,
}

impl MixAnalysis {
	pub fn channel(&self, track: TrackId) -> Option<&ChannelAnalysis> {
		self.channels.iter().find(|c| c.track_id == track)
	}

	pub fn name_of(&self, track: Option<TrackId>) -> String {
		match track {
			None => "Master".into(),
			Some(id) => self
				.channel(id)
				.map_or_else(|| id.to_string(), |c| c.name.clone()),
		}
	}
}

/// Judges a detailed render against the targets: roles, masking and issues.
pub fn assess(
	project: &Project,
	library: &Library,
	mix: DetailedMix,
	targets: MixTargets,
) -> MixAnalysis {
	let channels: Vec<ChannelAnalysis> = mix
		.tracks
		.into_iter()
		.map(|t| {
			let channel = project.track(t.track_id).map(|tr| &tr.channel);
			ChannelAnalysis {
				role: track_role(project, library, t.track_id),
				volume_db: channel.map_or(0.0, |c| c.volume_db),
				pan: channel.map_or(0.0, |c| c.pan),
				high_pass_hz: channel.and_then(high_pass),
				track_id: t.track_id,
				name: t.name,
				kind: t.kind,
				detail: t.detail,
			}
		})
		.collect();
	let inputs: Vec<TrackInput<'_>> = channels
		.iter()
		.map(|c| TrackInput {
			id: c.track_id,
			name: &c.name,
			role: c.role,
			is_bus: c.kind == TrackKind::Bus,
			detail: &c.detail,
			pan: c.pan,
			high_pass_hz: c.high_pass_hz,
		})
		.collect();
	let masking = track_masking(&inputs);
	let limiter = builtin(&project.master, "limiter").map(|l| LimiterState {
		input_gain_db: param(l, "input_gain"),
		ceiling_db: param(l, "ceiling"),
	});
	let issues = find_issues(
		&MasterInput {
			detail: &mix.master,
			limiter,
		},
		&inputs,
		&targets,
		&masking,
		&mix.report.warnings,
	);
	MixAnalysis {
		targets,
		report: mix.report,
		master: mix.master,
		channels,
		masking,
		issues,
	}
}

/// Renders and assesses the mix (blocking; call it from a worker or `spawn_blocking`).
pub fn run_analysis(
	project: &Project,
	library: &Library,
	opts: &RenderOptions,
	targets: MixTargets,
	progress: &mut dyn FnMut(f32) -> bool,
) -> Result<MixAnalysis, McpError> {
	let mix = analyze_mix_detailed(project, opts, progress)?;
	Ok(assess(project, library, mix, targets))
}

// -- outputs ---------------------------------------------------------------------------

fn pct(x: f32) -> u8 {
	(x * 100.0).round().clamp(0.0, 100.0) as u8
}

fn db_int(x: f32) -> i16 {
	x.round().max(-120.0) as i16
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct TargetsOut {
	pub lufs: f64,
	pub true_peak_db: f64,
	pub profile: ToneProfile,
	/// "style core/pop", "default", "arguments"...
	pub source: String,
}

impl From<&MixTargets> for TargetsOut {
	fn from(t: &MixTargets) -> Self {
		Self {
			lufs: r1(t.lufs),
			true_peak_db: r1(t.true_peak_db),
			profile: t.profile,
			source: t.source.clone(),
		}
	}
}

/// Band balance of the master relative to the target profile (dB, + = too much).
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ToneOut {
	pub low: f64,
	pub low_mid: f64,
	pub high_mid: f64,
	pub high: f64,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct MasterOut {
	pub lufs_integrated: f64,
	pub lufs_short_term_max: f64,
	pub peak_db: f64,
	pub true_peak_db: f64,
	pub rms_db: f64,
	pub crest_factor_db: f64,
	#[serde(skip_serializing_if = "is_zero")]
	pub clipped_samples: u64,
	/// Side energy share, % (10-30 is a typical wide mix).
	pub width_pct: u8,
	pub correlation: f64,
	/// L/R correlation below 120 Hz (keep near 1).
	pub low_end_correlation: f64,
	pub tone_vs_target: ToneOut,
	/// Octave shape (dB under the loudest band) at 31.5, 63, 125, 250, 500, 1k, 2k, 4k, 8k,
	/// 16k Hz.
	pub octaves_db: Vec<i16>,
}

fn is_zero(n: &u64) -> bool {
	*n == 0
}

fn master_out(a: &MixAnalysis) -> MasterOut {
	let s = &a.master.stats;
	let b = &s.band_energy_db;
	let c = a.targets.profile.curve();
	MasterOut {
		lufs_integrated: r1(s.lufs_integrated),
		lufs_short_term_max: r1(s.lufs_short_term_max),
		peak_db: r1(s.peak_db),
		true_peak_db: r1(s.true_peak_db),
		rms_db: r1(s.rms_db),
		crest_factor_db: r1(s.crest_factor_db),
		clipped_samples: s.clipped_samples,
		width_pct: pct(a.master.stereo.width),
		correlation: r2(s.stereo_correlation),
		low_end_correlation: r2(a.master.stereo.low_correlation),
		tone_vs_target: ToneOut {
			low: r1(b.low - b.mid - c.low),
			low_mid: r1(b.low_mid - b.mid - c.low_mid),
			high_mid: r1(b.high_mid - b.mid - c.high_mid),
			high: r1(b.high - b.mid - c.high),
		},
		octaves_db: a
			.master
			.spectrum
			.octave_shape()
			.into_iter()
			.map(db_int)
			.collect(),
	}
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct TrackOut {
	pub id: TrackId,
	pub name: String,
	pub role: Role,
	#[serde(skip_serializing_if = "std::ops::Not::not")]
	pub bus: bool,
	/// Nothing rendered from this channel in the range.
	#[serde(skip_serializing_if = "std::ops::Not::not")]
	pub silent: bool,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub lufs: Option<f64>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub peak_db: Option<f64>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub crest_db: Option<f64>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub width_pct: Option<u8>,
}

fn track_out(c: &ChannelAnalysis) -> TrackOut {
	let silent = c.detail.is_silent();
	let s = &c.detail.stats;
	let some = |v: f32| (!silent).then_some(r1(v));
	TrackOut {
		id: c.track_id,
		name: c.name.clone(),
		role: c.role,
		bus: c.kind == TrackKind::Bus,
		silent,
		lufs: some(s.lufs_integrated),
		peak_db: some(s.peak_db),
		crest_db: some(s.crest_factor_db),
		width_pct: (!silent).then(|| pct(c.detail.stereo.width)),
	}
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct MaskOut {
	pub a: String,
	pub b: String,
	/// Octave band center.
	pub hz: f64,
	/// Share of the time both play there in which they collide.
	pub overlap_pct: u8,
	/// The one that should make room.
	pub cut: String,
}

fn mask_out(a: &MixAnalysis, p: &MaskingPair) -> MaskOut {
	MaskOut {
		a: a.channels[p.a].name.clone(),
		b: a.channels[p.b].name.clone(),
		hz: f64::from(p.band_hz),
		overlap_pct: pct(p.overlap),
		cut: a.channels[p.cut].name.clone(),
	}
}

fn compact_moves(issue: &Issue) -> Issue {
	let round = |v: f32| (v * 10.0).round() / 10.0;
	let mut i = issue.clone();
	for m in &mut i.moves {
		match m {
			MixMove::Gain { delta_db, db, .. } => {
				*delta_db = delta_db.map(round);
				*db = db.map(round);
			}
			MixMove::Eq {
				freq_hz, gain_db, ..
			} => {
				*freq_hz = freq_hz.round();
				*gain_db = round(*gain_db);
			}
			_ => {}
		}
	}
	i
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
	#[serde(flatten)]
	pub targets: TargetArgs,
	/// Issues listed (most severe first), default 8, max 20.
	#[serde(default)]
	pub max_issues: Option<usize>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct AnalyzeMixOutput {
	pub targets: TargetsOut,
	pub duration_seconds: f64,
	pub master: MasterOut,
	/// Every channel (display order); buses are flagged.
	pub tracks: Vec<TrackOut>,
	/// Parts colliding in the same band at the same time, worst first (top 5).
	#[serde(skip_serializing_if = "Vec::is_empty")]
	pub masking: Vec<MaskOut>,
	/// Most severe first; pass an issue's `moves` to apply_mix_moves to fix it.
	pub issues: Vec<Issue>,
	/// Issues not listed (raise max_issues to see them).
	#[serde(skip_serializing_if = "is_zero_usize")]
	pub more_issues: usize,
	/// Render problems (missing files, crashed or missing plugins).
	#[serde(skip_serializing_if = "Vec::is_empty")]
	pub warnings: Vec<String>,
}

fn is_zero_usize(n: &usize) -> bool {
	*n == 0
}

/// The issues about the mix itself (render warnings are listed apart).
fn mix_issues(a: &MixAnalysis) -> impl Iterator<Item = &Issue> {
	a.issues
		.iter()
		.filter(|i| i.kind != IssueKind::EngineWarning)
}

/// The compact summary the AI reads after each pass.
pub fn summary_output(a: &MixAnalysis, max_issues: usize) -> AnalyzeMixOutput {
	let max_issues = max_issues.clamp(1, 20);
	AnalyzeMixOutput {
		targets: (&a.targets).into(),
		duration_seconds: (a.report.duration_seconds * 10.0).round() / 10.0,
		master: master_out(a),
		tracks: a.channels.iter().map(track_out).collect(),
		masking: a.masking.iter().take(5).map(|p| mask_out(a, p)).collect(),
		issues: mix_issues(a).take(max_issues).map(compact_moves).collect(),
		more_issues: mix_issues(a).count().saturating_sub(max_issues),
		warnings: a.report.warnings.clone(),
	}
}

/// What analyze_mix needs from the session (the session lock is released before rendering).
pub struct AnalysisInput {
	pub project: Project,
	pub library: Arc<Library>,
	pub plugins: Option<Arc<PluginCatalog>>,
	pub hosting: PluginHosting,
}

impl AnalysisInput {
	pub fn from_session(session: &mut Session) -> Self {
		Self {
			plugins: session.plugins_for_project(),
			hosting: session.plugin_hosting().clone(),
			library: session.library(),
			project: session.document().project().clone(),
		}
	}
}

async fn analyze_blocking(
	input: AnalysisInput,
	start_beat: Option<f64>,
	end_beat: Option<f64>,
	sample_rate: Option<u32>,
	targets: &TargetArgs,
) -> Result<MixAnalysis, McpError> {
	let targets = resolve_targets(&input.project, &input.library, targets);
	let opts = build_options(
		start_beat,
		end_beat,
		sample_rate,
		input.plugins.clone(),
		input.hosting.clone(),
	);
	tokio::task::spawn_blocking(move || {
		run_analysis(&input.project, &input.library, &opts, targets, &mut |_| {
			true
		})
	})
	.await
	.map_err(|e| McpError::Invalid(format!("analysis task panicked: {e}")))?
}

pub async fn analyze_mix(
	input: AnalysisInput,
	args: AnalyzeMixArgs,
) -> Result<AnalyzeMixOutput, McpError> {
	let a = analyze_blocking(
		input,
		args.start_beat,
		args.end_beat,
		args.sample_rate,
		&args.targets,
	)
	.await?;
	Ok(summary_output(&a, args.max_issues.unwrap_or(8)))
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AnalyzeTrackArgs {
	/// A track or bus id from get_project.
	pub track_id: TrackId,
	#[serde(default)]
	pub start_beat: Option<f64>,
	#[serde(default)]
	pub end_beat: Option<f64>,
	#[serde(flatten)]
	pub targets: TargetArgs,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct RoleDefaults {
	/// Typical level relative to the loudest parts (dB of loudness).
	pub balance_db: f64,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub high_pass_hz: Option<f64>,
	/// Usually panned off-center.
	pub spreads: bool,
}

fn role_defaults(targets: &MixTargets, role: Role) -> RoleDefaults {
	RoleDefaults {
		balance_db: r1(targets.balance_db(role)),
		high_pass_hz: role.high_pass_hz().map(r1),
		spreads: role.spreads(),
	}
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct AnalyzeTrackOutput {
	pub id: TrackId,
	pub name: String,
	pub role: Role,
	pub fader_db: f64,
	pub pan: f64,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub high_pass_hz: Option<f64>,
	pub lufs_integrated: f64,
	pub lufs_short_term_max: f64,
	pub peak_db: f64,
	pub true_peak_db: f64,
	pub rms_db: f64,
	pub crest_factor_db: f64,
	pub width_pct: u8,
	pub correlation: f64,
	pub low_end_correlation: f64,
	/// Side minus mid below 120 Hz, dB (above -15 = wide lows).
	pub low_side_db: f64,
	/// Mean level per third-octave band, dBFS: 20, 25, 31.5, 40, 50, 63, 80, 100, 125, 160,
	/// 200, 250, 315, 400, 500, 630, 800, 1k, 1.25k, 1.6k, 2k, 2.5k, 3.15k, 4k, 5k, 6.3k, 8k,
	/// 10k, 12.5k, 16k, 20k Hz.
	pub third_octave_db: Vec<i16>,
	/// Loudest octave band center, Hz.
	pub peak_band_hz: f64,
	#[serde(skip_serializing_if = "Vec::is_empty")]
	pub masking: Vec<MaskOut>,
	#[serde(skip_serializing_if = "Vec::is_empty")]
	pub issues: Vec<Issue>,
	pub role_defaults: RoleDefaults,
}

pub async fn analyze_track(
	input: AnalysisInput,
	args: AnalyzeTrackArgs,
) -> Result<AnalyzeTrackOutput, McpError> {
	if input.project.track(args.track_id).is_none() {
		return Err(McpError::Invalid(format!(
			"track {} does not exist; use a track or bus id from get_project",
			args.track_id
		)));
	}
	let a = analyze_blocking(input, args.start_beat, args.end_beat, None, &args.targets).await?;
	track_output(&a, args.track_id).ok_or_else(|| {
		McpError::Invalid(format!(
			"track {} has no audio channel (folder tracks carry no audio); analyze the \
			 tracks inside it",
			args.track_id
		))
	})
}

/// The in-depth view of one channel.
pub fn track_output(a: &MixAnalysis, track: TrackId) -> Option<AnalyzeTrackOutput> {
	let index = a.channels.iter().position(|c| c.track_id == track)?;
	let c = &a.channels[index];
	let s = &c.detail.stats;
	Some(AnalyzeTrackOutput {
		id: c.track_id,
		name: c.name.clone(),
		role: c.role,
		fader_db: r1(c.volume_db),
		pan: r2(c.pan),
		high_pass_hz: c.high_pass_hz.map(|f| f64::from(f.round())),
		lufs_integrated: r1(s.lufs_integrated),
		lufs_short_term_max: r1(s.lufs_short_term_max),
		peak_db: r1(s.peak_db),
		true_peak_db: r1(s.true_peak_db),
		rms_db: r1(s.rms_db),
		crest_factor_db: r1(s.crest_factor_db),
		width_pct: pct(c.detail.stereo.width),
		correlation: r2(s.stereo_correlation),
		low_end_correlation: r2(c.detail.stereo.low_correlation),
		low_side_db: r1(c.detail.stereo.low_side_db),
		third_octave_db: c
			.detail
			.spectrum
			.third_octave_db
			.iter()
			.map(|v| db_int(*v))
			.collect(),
		peak_band_hz: f64::from(OCTAVE_HZ[c.detail.spectrum.peak_octave()]),
		masking: a
			.masking
			.iter()
			.filter(|p| p.a == index || p.b == index)
			.map(|p| mask_out(a, p))
			.collect(),
		issues: a
			.issues
			.iter()
			.filter(|i| {
				i.track_id == Some(track) || i.moves.iter().any(|m| m.track() == Some(track))
			})
			.map(compact_moves)
			.collect(),
		role_defaults: role_defaults(&a.targets, c.role),
	})
}

/// One number before and after a mix pass.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct MetricChange {
	pub label: String,
	pub before: f64,
	pub after: f64,
	pub unit: String,
}

impl MetricChange {
	pub fn delta(&self) -> f64 {
		self.after - self.before
	}
}

/// The numbers a mix pass changed: master loudness, true peak, crest, width, issue and
/// masking counts, then every channel whose loudness moved by 0.5 dB or more.
pub fn compare_passes(before: &MixAnalysis, after: &MixAnalysis) -> Vec<MetricChange> {
	let m = |label: &str, b: f64, a: f64, unit: &str| MetricChange {
		label: label.to_owned(),
		before: (b * 10.0).round() / 10.0,
		after: (a * 10.0).round() / 10.0,
		unit: unit.to_owned(),
	};
	let (b, a) = (&before.master.stats, &after.master.stats);
	let high = |x: &MixAnalysis| {
		mix_issues(x)
			.filter(|i| i.severity == cadenza_analysis::issues::Severity::High)
			.count() as f64
	};
	let mut out = vec![
		m(
			"Loudness",
			r1(b.lufs_integrated),
			r1(a.lufs_integrated),
			"LUFS",
		),
		m("True peak", r1(b.true_peak_db), r1(a.true_peak_db), "dBTP"),
		m(
			"Crest factor",
			r1(b.crest_factor_db),
			r1(a.crest_factor_db),
			"dB",
		),
		m(
			"Width",
			f64::from(pct(before.master.stereo.width)),
			f64::from(pct(after.master.stereo.width)),
			"%",
		),
		m(
			"Issues",
			mix_issues(before).count() as f64,
			mix_issues(after).count() as f64,
			"",
		),
		m("Severe issues", high(before), high(after), ""),
		m(
			"Masking pairs",
			before.masking.len() as f64,
			after.masking.len() as f64,
			"",
		),
	];
	for c in &after.channels {
		let Some(old) = before.channel(c.track_id) else {
			continue;
		};
		if c.detail.is_silent() && old.detail.is_silent() {
			continue;
		}
		let (x, y) = (
			old.detail.stats.lufs_integrated,
			c.detail.stats.lufs_integrated,
		);
		if (y - x).abs() >= 0.5 {
			out.push(m(&c.name, r1(x), r1(y), "LUFS"));
		}
	}
	out
}

// -- reference ---------------------------------------------------------------------------

/// Decodes and analyzes a reference file (any format the decoder reads), optionally only
/// `seconds` from `start_seconds`.
pub fn analyze_reference(
	path: &Path,
	start_seconds: Option<f64>,
	seconds: Option<f64>,
) -> Result<Detail, McpError> {
	let buffer = cadenza_audio_io::decode_file(path).map_err(|e| McpError::Io {
		path: path.display().to_string(),
		message: format!("{e}; the reference must be an audio file (wav, flac, mp3, ogg...)"),
	})?;
	let stereo = buffer.to_stereo();
	let sr = stereo.sample_rate();
	let frames = stereo.frames();
	let from = ((start_seconds.unwrap_or(0.0).max(0.0) * f64::from(sr)) as usize).min(frames);
	let to = match seconds {
		Some(s) if s > 0.0 => (from + (s * f64::from(sr)) as usize).min(frames),
		_ => frames,
	};
	if to <= from {
		return Err(McpError::Invalid(format!(
			"the reference range is empty: the file is {:.1} s long",
			buffer.duration_seconds()
		)));
	}
	Ok(analyze_signal(
		sr,
		&stereo.channel(0)[from..to],
		&stereo.channel(1)[from..to],
	))
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CompareToReferenceArgs {
	/// Reference audio file (wav, flac, mp3, ogg...).
	pub file: String,
	/// Part of the reference to use (e.g. its chorus), seconds.
	#[serde(default)]
	pub reference_start_seconds: Option<f64>,
	#[serde(default)]
	pub reference_seconds: Option<f64>,
	/// Part of the song to compare (default: all).
	#[serde(default)]
	pub start_beat: Option<f64>,
	#[serde(default)]
	pub end_beat: Option<f64>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct SignalOut {
	pub lufs_integrated: f64,
	pub true_peak_db: f64,
	pub crest_factor_db: f64,
	pub width_pct: u8,
}

fn signal_out(d: &Detail) -> SignalOut {
	SignalOut {
		lufs_integrated: r1(d.stats.lufs_integrated),
		true_peak_db: r1(d.stats.true_peak_db),
		crest_factor_db: r1(d.stats.crest_factor_db),
		width_pct: pct(d.stereo.width),
	}
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct CompareToReferenceOutput {
	pub mix: SignalOut,
	pub reference: SignalOut,
	/// Mix minus reference; `octave_delta_db` at 31.5, 63, 125, 250, 500, 1k, 2k, 4k, 8k,
	/// 16k Hz after matching levels. `moves` are master EQ moves toward the reference.
	pub comparison: ReferenceComparison,
}

/// Compares the master with an analyzed reference.
pub fn reference_output(master: &Detail, reference: &Detail) -> CompareToReferenceOutput {
	let mut comparison = compare(master, reference);
	comparison.lufs_delta = round1(comparison.lufs_delta);
	comparison.true_peak_delta_db = round1(comparison.true_peak_delta_db);
	comparison.crest_delta_db = round1(comparison.crest_delta_db);
	comparison.width_delta_pct = comparison.width_delta_pct.round();
	CompareToReferenceOutput {
		mix: signal_out(master),
		reference: signal_out(reference),
		comparison,
	}
}

pub async fn compare_to_reference(
	input: AnalysisInput,
	args: CompareToReferenceArgs,
	file: std::path::PathBuf,
) -> Result<CompareToReferenceOutput, McpError> {
	let opts = build_options(
		args.start_beat,
		args.end_beat,
		None,
		input.plugins.clone(),
		input.hosting.clone(),
	);
	tokio::task::spawn_blocking(move || {
		let reference =
			analyze_reference(&file, args.reference_start_seconds, args.reference_seconds)?;
		let mix = analyze_mix_detailed(&input.project, &opts, &mut |_| true)?;
		Ok(reference_output(&mix.master, &reference))
	})
	.await
	.map_err(|e| McpError::Invalid(format!("reference task panicked: {e}")))?
}

// -- targets tool ------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
pub struct MixTargetsArgs {
	#[serde(flatten)]
	pub targets: TargetArgs,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct TrackTarget {
	pub id: TrackId,
	pub name: String,
	pub role: Role,
	#[serde(flatten)]
	pub defaults: RoleDefaults,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct MixTargetsOutput {
	#[serde(flatten)]
	pub targets: TargetsOut,
	/// The style's mix advice.
	#[serde(skip_serializing_if = "String::is_empty")]
	pub notes: String,
	/// Per track: its role (from name, instrument and library clips) and that role's defaults.
	pub tracks: Vec<TrackTarget>,
}

pub fn mix_targets(session: &mut Session, args: MixTargetsArgs) -> MixTargetsOutput {
	let library = session.library();
	let project = session.document().project();
	let targets = resolve_targets(project, &library, &args.targets);
	MixTargetsOutput {
		tracks: project
			.tracks
			.iter()
			.filter(|t| t.kind != TrackKind::Folder)
			.map(|t| {
				let role = track_role(project, &library, t.id);
				TrackTarget {
					id: t.id,
					name: t.name.clone(),
					role,
					defaults: role_defaults(&targets, role),
				}
			})
			.collect(),
		notes: targets.notes.clone(),
		targets: (&targets).into(),
	}
}

// -- moves -------------------------------------------------------------------------------

fn clamp_param(device: &str, param: &str, value: f32) -> Result<f32, String> {
	if !value.is_finite() {
		return Err(format!("{param} must be a finite number"));
	}
	let info = cadenza_dsp::device_info(device)
		.and_then(|i| i.param(param))
		.ok_or_else(|| format!("built-in {device} has no parameter {param}"))?;
	Ok(value.clamp(info.min, info.max))
}

fn channel_of(project: &Project, track: Option<TrackId>) -> Result<&Channel, String> {
	let channel = project.channel(track).ok_or_else(|| {
		format!(
			"track {} does not exist; use ids from get_project",
			track.map(|t| t.to_string()).unwrap_or_default()
		)
	})?;
	if let Some(t) = track.and_then(|id| project.track(id))
		&& t.kind == TrackKind::Folder
	{
		return Err(format!(
			"'{}' is a folder (no audio): only gain applies to it; EQ/dynamics/sends go on \
			 the tracks inside",
			t.name
		));
	}
	Ok(channel)
}

/// Sets `params` on the channel's built-in `id` device, inserting one (at `index`) when the
/// channel has none.
fn device_params(
	track: Option<TrackId>,
	channel: &Channel,
	id: &str,
	index: impl FnOnce(&Channel) -> Option<usize>,
	params: Vec<(&str, f32)>,
) -> Result<Vec<Command>, String> {
	let mut checked = Vec::new();
	for (p, v) in params {
		checked.push((p, clamp_param(id, p, v)?));
	}
	match builtin(channel, id) {
		Some(device) => Ok(checked
			.into_iter()
			.filter(|(p, v)| (param(device, p) - v).abs() > 1e-4)
			.map(|(p, v)| Command::SetParam {
				address: ParamAddress::device(track, device.id, p),
				value: v,
			})
			.collect()),
		None => {
			let name = cadenza_dsp::device_info(id).map_or(id, |i| i.name);
			let mut device = Device::builtin(id, name);
			for (p, v) in checked {
				device = device.with_param(p, v);
			}
			Ok(vec![Command::InsertDevice {
				track,
				device,
				index: index(channel),
			}])
		}
	}
}

fn position(channel: &Channel, id: &str) -> Option<usize> {
	channel
		.inserts
		.iter()
		.position(|d| d.builtin_id() == Some(id))
}

/// The EQ params of a band, choosing the peak slot for bells.
fn eq_band(
	channel: &Channel,
	kind: EqKind,
	freq: f32,
	gain: f32,
	q: Option<f32>,
) -> Vec<(&'static str, f32)> {
	match kind {
		EqKind::HighPass => {
			let slope = builtin(channel, "eq").map_or(0.0, |d| param(d, "hp_slope"));
			vec![("hp_freq", freq), ("hp_slope", slope.max(1.0))]
		}
		EqKind::LowPass => {
			let slope = builtin(channel, "eq").map_or(0.0, |d| param(d, "lp_slope"));
			vec![("lp_freq", freq), ("lp_slope", slope.max(1.0))]
		}
		EqKind::LowShelf => vec![("low_shelf_freq", freq), ("low_shelf_gain", gain)],
		EqKind::HighShelf => vec![("high_shelf_freq", freq), ("high_shelf_gain", gain)],
		EqKind::Bell => {
			const SLOTS: [(&str, &str, &str); 3] = [
				("peak1_freq", "peak1_gain", "peak1_q"),
				("peak2_freq", "peak2_gain", "peak2_q"),
				("peak3_freq", "peak3_gain", "peak3_q"),
			];
			let slot = match builtin(channel, "eq") {
				None => 0,
				Some(eq) => {
					let near = SLOTS.iter().position(|(f, g, _)| {
						param(eq, g).abs() > 0.05 && (param(eq, f) / freq).log2().abs() < 1.0 / 3.0
					});
					let free = SLOTS
						.iter()
						.position(|(_, g, _)| param(eq, g).abs() <= 0.05);
					near.or(free).unwrap_or_else(|| {
						(0..3)
							.min_by(|&a, &b| {
								param(eq, SLOTS[a].1)
									.abs()
									.total_cmp(&param(eq, SLOTS[b].1).abs())
							})
							.unwrap_or(0)
					})
				}
			};
			let (f, g, qq) = SLOTS[slot];
			vec![(f, freq), (g, gain), (qq, q.unwrap_or(1.0))]
		}
	}
}

fn one_move(project: &Project, mv: &MixMove) -> Result<Vec<Command>, String> {
	match mv {
		MixMove::Gain {
			track_id,
			delta_db,
			db,
		} => {
			let channel = project.channel(*track_id).ok_or_else(|| {
				"track does not exist; use ids from get_project (omit track_id for the master)"
					.to_owned()
			})?;
			let volume = match (db, delta_db) {
				(Some(v), _) => *v,
				(None, Some(d)) => channel.volume_db + d,
				(None, None) => return Err("gain needs delta_db or db".into()),
			};
			if !volume.is_finite() {
				return Err("gain must be a finite number of dB".into());
			}
			Ok(vec![Command::SetChannel {
				track: *track_id,
				patch: ChannelPatch {
					volume_db: Some(volume.clamp(-96.0, 12.0)),
					..ChannelPatch::default()
				},
			}])
		}
		MixMove::Pan { track_id, pan } => {
			channel_of(project, Some(*track_id))?;
			if !pan.is_finite() {
				return Err("pan must be -1..1".into());
			}
			Ok(vec![Command::SetChannel {
				track: Some(*track_id),
				patch: ChannelPatch {
					pan: Some(pan.clamp(-1.0, 1.0)),
					..ChannelPatch::default()
				},
			}])
		}
		MixMove::Eq {
			track_id,
			kind,
			freq_hz,
			gain_db,
			q,
		} => {
			let channel = channel_of(project, *track_id)?;
			if !(20.0..=20_000.0).contains(freq_hz) {
				return Err(format!("freq_hz {freq_hz} must be 20..=20000"));
			}
			let params = eq_band(channel, *kind, *freq_hz, *gain_db, *q);
			// A new EQ goes first in the chain (before dynamics).
			device_params(*track_id, channel, "eq", |_| Some(0), params)
		}
		MixMove::Compressor {
			track_id,
			threshold_db,
			ratio,
			attack_ms,
			release_ms,
			makeup_db,
		} => {
			let channel = channel_of(project, *track_id)?;
			let params: Vec<(&str, f32)> = [
				("threshold", *threshold_db),
				("ratio", *ratio),
				("attack", *attack_ms),
				("release", *release_ms),
				("makeup", *makeup_db),
			]
			.into_iter()
			.filter_map(|(p, v)| Some((p, v?)))
			.collect();
			// After an EQ, before a limiter.
			device_params(
				*track_id,
				channel,
				"compressor",
				|c| {
					position(c, "limiter")
						.or_else(|| position(c, "eq").map(|i| i + 1))
						.or(Some(c.inserts.len()))
				},
				params,
			)
		}
		MixMove::Limiter {
			track_id,
			ceiling_db,
			input_gain_db,
		} => {
			let channel = channel_of(project, *track_id)?;
			let params: Vec<(&str, f32)> =
				[("ceiling", *ceiling_db), ("input_gain", *input_gain_db)]
					.into_iter()
					.filter_map(|(p, v)| Some((p, v?)))
					.collect();
			device_params(*track_id, channel, "limiter", |_| None, params)
		}
		MixMove::Send {
			track_id,
			bus_id,
			level_db,
		} => {
			let channel = channel_of(project, Some(*track_id))?;
			let bus = project.track(*bus_id).ok_or_else(|| {
				format!("bus {bus_id} does not exist; create one with create_track kind bus")
			})?;
			if bus.kind != TrackKind::Bus {
				return Err(format!("'{}' is not a bus track", bus.name));
			}
			if !level_db.is_finite() {
				return Err("level_db must be a finite number".into());
			}
			let level = level_db.clamp(-96.0, 12.0);
			Ok(vec![
				match channel.sends.iter().find(|s| s.target == *bus_id) {
					Some(send) => Command::UpdateSend {
						track: *track_id,
						send: send.id,
						patch: SendPatch {
							level_db: Some(level),
							..SendPatch::default()
						},
					},
					None => Command::AddSend {
						track: *track_id,
						send: ModelSend::new(*bus_id, level),
						index: None,
					},
				},
			])
		}
	}
}

fn move_name(mv: &MixMove) -> &'static str {
	match mv {
		MixMove::Gain { .. } => "gain",
		MixMove::Pan { .. } => "pan",
		MixMove::Eq { .. } => "eq",
		MixMove::Compressor { .. } => "compressor",
		MixMove::Limiter { .. } => "limiter",
		MixMove::Send { .. } => "send",
	}
}

/// The commands of a list of moves, in order (later moves see earlier ones: two bells on
/// one track land in two EQ bands). All or nothing: the first invalid move is an error
/// naming it.
pub fn mix_move_commands(project: &Project, moves: &[MixMove]) -> Result<Vec<Command>, McpError> {
	let mut scratch = Document::new(project.clone());
	let mut all = Vec::new();
	for (i, mv) in moves.iter().enumerate() {
		let fail =
			|e: String| McpError::Invalid(format!("move {} ({}): {e}", i + 1, move_name(mv)));
		for cmd in one_move(scratch.project(), mv).map_err(fail)? {
			scratch
				.execute(cmd.clone())
				.map_err(|e| fail(e.to_string()))?;
			all.push(cmd);
		}
	}
	Ok(all)
}

/// The batch command of a mix pass (one undo step named `label`).
pub fn mix_pass_command(
	project: &Project,
	moves: &[MixMove],
	label: &str,
) -> Result<Command, McpError> {
	Ok(Command::Batch {
		label: label.to_owned(),
		commands: mix_move_commands(project, moves)?,
	})
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ApplyMixMovesArgs {
	/// Moves in order, e.g. the `moves` of analyze_mix issues, or your own.
	pub moves: Vec<MixMove>,
	/// Undo step name (default "AI mix pass").
	#[serde(default)]
	pub label: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ApplyMixMovesOutput {
	/// What each move did.
	pub applied: Vec<String>,
	pub undo_label: String,
}

pub fn apply_mix_moves(
	session: &mut Session,
	args: ApplyMixMovesArgs,
) -> Result<ApplyMixMovesOutput, McpError> {
	if args.moves.is_empty() {
		return Err(McpError::Invalid(
			"moves is empty; pass the moves of analyze_mix issues or your own".into(),
		));
	}
	if args.moves.len() > 64 {
		return Err(McpError::Invalid(
			"at most 64 moves per call; split the pass".into(),
		));
	}
	let label = args
		.label
		.filter(|l| !l.trim().is_empty())
		.unwrap_or_else(|| MIX_PASS_LABEL.to_owned());
	let project = session.document().project();
	let names = |t: Option<TrackId>| match t {
		None => "Master".to_owned(),
		Some(id) => project
			.track(id)
			.map_or_else(|| id.to_string(), |t| t.name.clone()),
	};
	let applied: Vec<String> = args.moves.iter().map(|m| m.describe(&names)).collect();
	let cmd = mix_pass_command(project, &args.moves, &label)?;
	session.document_mut().execute(cmd)?;
	Ok(ApplyMixMovesOutput {
		applied,
		undo_label: label,
	})
}

#[cfg(test)]
mod tests {
	use cadenza_model::{Clip, ProjectMeta, Track};

	use super::*;

	fn session() -> Session {
		let mut s = Session::new();
		s.set_library(Arc::new(Library::core()));
		s
	}

	fn add(s: &mut Session, track: Track) -> TrackId {
		let id = track.id;
		s.document_mut()
			.execute(Command::AddTrack { track, index: None })
			.unwrap();
		id
	}

	fn eq_of(project: &Project, track: Option<TrackId>) -> Device {
		builtin(project.channel(track).unwrap(), "eq")
			.unwrap()
			.clone()
	}

	#[test]
	fn targets_come_from_the_style_then_the_arguments() {
		let mut s = session();
		let library = s.library();
		let defaults = resolve_targets(s.document().project(), &library, &TargetArgs::default());
		assert_eq!(defaults.source, "default");
		assert_eq!(defaults.lufs, -14.0);
		s.document_mut()
			.execute(Command::SetProjectMeta {
				meta: ProjectMeta {
					style: Some("core/trap".into()),
				},
			})
			.unwrap();
		let style = resolve_targets(s.document().project(), &library, &TargetArgs::default());
		assert_eq!(style.source, "style core/trap");
		assert_eq!(style.lufs, -8.0);
		assert_eq!(style.profile, ToneProfile::Hiphop);
		assert_eq!(style.balance_db(Role::Pad), -10.0);
		assert!(style.notes.contains("808"));
		let args = TargetArgs {
			target_lufs: Some(-11.0),
			..TargetArgs::default()
		};
		let mixed = resolve_targets(s.document().project(), &library, &args);
		assert_eq!(mixed.lufs, -11.0);
		assert_eq!(mixed.source, "style core/trap + arguments");

		let mut bass = Track::new("Synth", TrackKind::Instrument)
			.with_instrument(Device::builtin("poly_synth", "Poly"));
		bass.clips.push(Clip::midi(
			"Tumbao [core/bass_tumbao]",
			cadenza_core::Tick::ZERO,
			cadenza_core::Tick::from_beats(4.0),
		));
		let bass = add(&mut s, bass);
		let out = mix_targets(&mut s, MixTargetsArgs { targets: args });
		assert_eq!(out.tracks.len(), 1);
		assert_eq!(out.tracks[0].id, bass);
		assert_eq!(out.tracks[0].role, Role::Bass, "role from the library clip");
		assert_eq!(out.targets.lufs, -11.0);
	}

	#[test]
	fn moves_become_one_undo_step_on_the_built_in_devices() {
		let mut s = session();
		let keys = add(&mut s, Track::new("Keys", TrackKind::Instrument));
		let bus = add(&mut s, Track::new("Verb", TrackKind::Bus));
		let bell = |gain: f32, freq: f32| MixMove::Eq {
			track_id: Some(keys),
			kind: EqKind::Bell,
			freq_hz: freq,
			gain_db: gain,
			q: Some(1.4),
		};
		let moves = vec![
			MixMove::Gain {
				track_id: Some(keys),
				delta_db: Some(-3.0),
				db: None,
			},
			bell(-3.0, 250.0),
			bell(-2.0, 2_500.0),
			MixMove::Eq {
				track_id: Some(keys),
				kind: EqKind::HighPass,
				freq_hz: 100.0,
				gain_db: 0.0,
				q: None,
			},
			MixMove::Compressor {
				track_id: None,
				threshold_db: Some(-12.0),
				ratio: Some(2.0),
				attack_ms: None,
				release_ms: None,
				makeup_db: None,
			},
			MixMove::Limiter {
				track_id: None,
				ceiling_db: Some(-1.5),
				input_gain_db: Some(40.0),
			},
			MixMove::Send {
				track_id: keys,
				bus_id: bus,
				level_db: -18.0,
			},
			MixMove::Pan {
				track_id: keys,
				pan: 0.3,
			},
		];
		let undo_before = s.document().history_labels().len();
		let out = apply_mix_moves(&mut s, ApplyMixMovesArgs { moves, label: None }).unwrap();
		assert_eq!(out.undo_label, "AI mix pass");
		assert_eq!(out.applied[1], "Keys: EQ -3.0 dB at 250 Hz");
		assert_eq!(out.applied[4], "Master: compressor -12 dB 2.0:1");
		let p = s.document().project();
		assert_eq!(s.document().history_labels().len(), undo_before + 1);
		let keys_ch = &p.track(keys).unwrap().channel;
		assert_eq!(keys_ch.volume_db, -3.0);
		assert_eq!(keys_ch.pan, 0.3);
		assert_eq!(
			keys_ch.inserts.len(),
			1,
			"both bells and the high-pass share one EQ"
		);
		let eq = eq_of(p, Some(keys));
		assert_eq!(param(&eq, "peak1_freq"), 250.0);
		assert_eq!(param(&eq, "peak2_freq"), 2_500.0);
		assert_eq!(param(&eq, "peak2_gain"), -2.0);
		assert_eq!(param(&eq, "hp_slope"), 1.0);
		assert_eq!(keys_ch.sends.len(), 1);
		assert_eq!(keys_ch.sends[0].level_db, -18.0);
		let master: Vec<Option<&str>> = p.master.inserts.iter().map(|d| d.builtin_id()).collect();
		assert_eq!(master, vec![Some("compressor"), Some("limiter")]);
		let limiter = builtin(&p.master, "limiter").unwrap();
		assert_eq!(
			param(limiter, "input_gain"),
			24.0,
			"clamped to the device range"
		);

		// A second pass updates instead of inserting: same bell slot, same send.
		apply_mix_moves(
			&mut s,
			ApplyMixMovesArgs {
				moves: vec![
					bell(-4.0, 260.0),
					MixMove::Send {
						track_id: keys,
						bus_id: bus,
						level_db: -12.0,
					},
					MixMove::Compressor {
						track_id: Some(keys),
						threshold_db: Some(-20.0),
						ratio: None,
						attack_ms: None,
						release_ms: None,
						makeup_db: None,
					},
				],
				label: Some("Tighten keys".into()),
			},
		)
		.unwrap();
		let p = s.document().project();
		let ch = &p.track(keys).unwrap().channel;
		let eq = eq_of(p, Some(keys));
		assert_eq!(param(&eq, "peak1_freq"), 260.0);
		assert_eq!(param(&eq, "peak1_gain"), -4.0);
		assert_eq!(ch.sends.len(), 1);
		assert_eq!(ch.sends[0].level_db, -12.0);
		let chain: Vec<Option<&str>> = ch.inserts.iter().map(|d| d.builtin_id()).collect();
		assert_eq!(chain, vec![Some("eq"), Some("compressor")]);
		assert_eq!(s.document().history_labels().last(), Some(&"Tighten keys"));
		s.document_mut().undo().unwrap();
		s.document_mut().undo().unwrap();
		assert!(s.document().project().master.inserts.is_empty());
	}

	#[test]
	fn invalid_moves_name_their_index_and_change_nothing() {
		let mut s = session();
		let keys = add(&mut s, Track::new("Keys", TrackKind::Instrument));
		let before = s.document().project().clone();
		let err = apply_mix_moves(
			&mut s,
			ApplyMixMovesArgs {
				moves: vec![
					MixMove::Gain {
						track_id: Some(keys),
						delta_db: Some(-2.0),
						db: None,
					},
					MixMove::Send {
						track_id: keys,
						bus_id: keys,
						level_db: -10.0,
					},
				],
				label: None,
			},
		)
		.unwrap_err()
		.to_string();
		assert!(
			err.starts_with("move 2 (send): 'Keys' is not a bus track"),
			"{err}"
		);
		assert_eq!(s.document().project(), &before);
		let err = apply_mix_moves(
			&mut s,
			ApplyMixMovesArgs {
				moves: vec![MixMove::Gain {
					track_id: None,
					delta_db: None,
					db: None,
				}],
				label: None,
			},
		)
		.unwrap_err()
		.to_string();
		assert!(err.contains("gain needs delta_db or db"), "{err}");
	}
}
