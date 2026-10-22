//! Match EQ (T31): fits the bands of a built-in Parametric EQ so the signal it receives
//! takes the tonal shape of a reference (an audio file or another track). The spectra come
//! from one offline render of the song with that EQ and every insert after it bypassed
//! (the device's input), the fit from `cadenza_analysis::match_eq`; applying is ONE undo
//! step "Match EQ to <reference>". Shared by the MCP tool and the app's EQ editor.

use std::path::PathBuf;
use std::sync::Arc;

use cadenza_analysis::detail::Detail;
use cadenza_analysis::match_eq::{MatchBand, MatchFit, MatchOptions, fit, match_target};
use cadenza_analysis::moves::EqKind;
use cadenza_analysis::spectrum::THIRD_OCTAVE_HZ;
use cadenza_core::{DeviceId, ParamAddress, TrackId};
use cadenza_dsp::response::EqBands;
use cadenza_engine::{PluginHosting, analyze_mix_detailed};
use cadenza_model::{Command, Device, DeviceKind, Document, Project};
use cadenza_plugin_host::PluginCatalog;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::McpError;
use crate::session::Session;
use crate::tools::mix::analyze_reference;
use crate::tools::render::build_options;

/// Default largest boost / cut of a fitted band (dB).
pub const DEFAULT_MAX_GAIN_DB: f32 = 6.0;
/// Where the device's high-pass / low-pass cut more than this, the reference's difference
/// is ignored (the cut is intentional; the bands must not fight it).
const CUT_MASK_DB: f32 = 3.0;

/// The undo label of a match to `reference`.
pub fn match_label(reference: &str) -> String {
	format!("Match EQ to {reference}")
}

/// What the EQ is matched to.
#[derive(Debug, Clone, PartialEq)]
pub enum MatchReference {
	/// An audio file, optionally a part of it (seconds).
	File {
		path: PathBuf,
		start_seconds: Option<f64>,
		seconds: Option<f64>,
	},
	/// Another channel of the song (its post-fader signal).
	Track(TrackId),
}

/// The two spectra a match compares (mid third-octave levels, dB per [`THIRD_OCTAVE_HZ`]).
#[derive(Debug, Clone, PartialEq)]
pub struct MatchSpectra {
	/// What the EQ receives.
	pub input: Vec<f32>,
	pub reference: Vec<f32>,
	/// Shown in the label: the file name or the track name.
	pub reference_name: String,
}

fn eq_device(
	project: &Project,
	track: Option<TrackId>,
	device: DeviceId,
) -> Result<&Device, String> {
	if let Some(id) = track
		&& project.track(id).is_none()
	{
		return Err(format!(
			"track {id} does not exist; use ids from get_project"
		));
	}
	let d = project.device(track, device).ok_or_else(|| {
		format!(
			"device {device} is not on {}; use the device ids get_project lists for that track",
			track.map_or_else(|| "the master".to_owned(), |t| format!("track {t}"))
		)
	})?;
	match &d.kind {
		DeviceKind::Builtin { id } if id == "eq" => Ok(d),
		_ => Err(format!(
			"device {device} ('{}') is not the built-in Parametric EQ; match_eq fits the \
			 bands of an `eq` insert (insert_device eq first)",
			d.name
		)),
	}
}

/// Checks the target and the reference before any rendering.
pub fn check_match(
	project: &Project,
	track: Option<TrackId>,
	device: DeviceId,
	reference: &MatchReference,
) -> Result<(), String> {
	eq_device(project, track, device)?;
	if let MatchReference::Track(id) = reference {
		if Some(*id) == track {
			return Err("reference_track_id is the EQ's own track; pick another track".into());
		}
		if project.track(*id).is_none() {
			return Err(format!(
				"reference track {id} does not exist; use ids from get_project"
			));
		}
	}
	Ok(())
}

/// Renders the song once with the EQ (and every insert after it) bypassed and returns the
/// EQ's input spectrum and the reference's. Blocking; `progress` (0..1) returning false
/// cancels.
#[allow(clippy::too_many_arguments)]
pub fn analyze_match_sources(
	project: &Project,
	plugins: Option<Arc<PluginCatalog>>,
	hosting: PluginHosting,
	track: Option<TrackId>,
	device: DeviceId,
	reference: &MatchReference,
	range: (Option<f64>, Option<f64>),
	progress: &mut dyn FnMut(f32) -> bool,
) -> Result<MatchSpectra, McpError> {
	check_match(project, track, device, reference).map_err(McpError::Invalid)?;
	let channel = project
		.channel(track)
		.ok_or_else(|| McpError::Invalid("the EQ's channel does not exist".into()))?;
	let at = channel
		.inserts
		.iter()
		.position(|d| d.id == device)
		.ok_or_else(|| {
			McpError::Invalid(format!(
				"device {device} is not an insert of that channel; match_eq works on inserts"
			))
		})?;
	let mut scratch = Document::new(project.clone());
	for d in &channel.inserts[at..] {
		if !d.bypass {
			scratch.execute(Command::SetDeviceBypass {
				track,
				device: d.id,
				bypass: true,
			})?;
		}
	}
	let (file_ref, reference_name) = match reference {
		MatchReference::File {
			path,
			start_seconds,
			seconds,
		} => {
			let detail = analyze_reference(path, *start_seconds, *seconds)?;
			let name = path.file_name().map_or_else(
				|| path.display().to_string(),
				|n| n.to_string_lossy().into_owned(),
			);
			(Some(detail), name)
		}
		MatchReference::Track(id) => (
			None,
			project
				.track(*id)
				.map_or_else(|| id.to_string(), |t| t.name.clone()),
		),
	};
	let opts = build_options(range.0, range.1, None, plugins, hosting);
	let mix = analyze_mix_detailed(scratch.project(), &opts, progress)?;
	let detail_of = |id: Option<TrackId>| -> Option<&Detail> {
		match id {
			None => Some(&mix.master),
			Some(id) => mix
				.tracks
				.iter()
				.find(|t| t.track_id == id)
				.map(|t| &t.detail),
		}
	};
	let input = detail_of(track).ok_or_else(|| {
		McpError::Invalid("the EQ's track renders nothing (no node in the mix)".into())
	})?;
	if input.is_silent() {
		return Err(McpError::Invalid(
			"the EQ's input is silent in that range; give the track some audio or pick a \
			 range where it plays (start_beat / end_beat)"
				.into(),
		));
	}
	let reference = match (&file_ref, reference) {
		(Some(d), _) => d,
		(None, MatchReference::Track(id)) => detail_of(Some(*id))
			.ok_or_else(|| McpError::Invalid(format!("reference track {id} renders nothing")))?,
		(None, MatchReference::File { .. }) => unreachable!("files are analyzed above"),
	};
	if reference.is_silent() {
		return Err(McpError::Invalid(format!(
			"the reference '{reference_name}' is silent in that range; pick a part where it plays"
		)));
	}
	Ok(MatchSpectra {
		input: input.spectrum.third_octave_db.clone(),
		reference: reference.spectrum.third_octave_db.clone(),
		reference_name,
	})
}

/// The EQ params a fit sets: the low shelf, the three peaks (by frequency) and the high
/// shelf, stereo and static; unused bands go to 0 dB. High-pass, low-pass and the output
/// stay.
pub fn fit_changes(fit: &MatchFit) -> Vec<(&'static str, f32)> {
	let mut out: Vec<(&'static str, f32)> = Vec::new();
	let low = fit.bands.iter().find(|b| b.kind == EqKind::LowShelf);
	let high = fit.bands.iter().find(|b| b.kind == EqKind::HighShelf);
	let mut bells: Vec<&MatchBand> = fit
		.bands
		.iter()
		.filter(|b| b.kind == EqKind::Bell)
		.collect();
	bells.sort_by(|a, b| a.freq_hz.total_cmp(&b.freq_hz));
	let clamp = |id: &str, v: f32| {
		cadenza_dsp::device_info("eq")
			.and_then(|i| i.param(id))
			.map_or(v, |p| v.clamp(p.min, p.max))
	};
	let mut shelf = |prefix: &'static [&'static str; 4], band: Option<&MatchBand>| {
		let [freq, gain, dyn_, ms] = *prefix;
		match band {
			Some(b) => {
				out.push((freq, clamp(freq, b.freq_hz.round())));
				out.push((gain, clamp(gain, round1(b.gain_db))));
			}
			None => out.push((gain, 0.0)),
		}
		out.push((dyn_, 0.0));
		out.push((ms, 0.0));
	};
	shelf(
		&[
			"low_shelf_freq",
			"low_shelf_gain",
			"low_shelf_dyn",
			"low_shelf_ms",
		],
		low,
	);
	shelf(
		&[
			"high_shelf_freq",
			"high_shelf_gain",
			"high_shelf_dyn",
			"high_shelf_ms",
		],
		high,
	);
	const PEAKS: [[&str; 5]; 3] = [
		[
			"peak1_freq",
			"peak1_gain",
			"peak1_q",
			"peak1_dyn",
			"peak1_ms",
		],
		[
			"peak2_freq",
			"peak2_gain",
			"peak2_q",
			"peak2_dyn",
			"peak2_ms",
		],
		[
			"peak3_freq",
			"peak3_gain",
			"peak3_q",
			"peak3_dyn",
			"peak3_ms",
		],
	];
	for (k, [freq, gain, q, dyn_, ms]) in PEAKS.into_iter().enumerate() {
		match bells.get(k) {
			Some(b) => {
				out.push((freq, clamp(freq, b.freq_hz.round())));
				out.push((gain, clamp(gain, round1(b.gain_db))));
				out.push((q, clamp(q, (b.q * 100.0).round() / 100.0)));
			}
			None => out.push((gain, 0.0)),
		}
		out.push((dyn_, 0.0));
		out.push((ms, 0.0));
	}
	out
}

fn r1(x: f32) -> f64 {
	(f64::from(x) * 10.0).round() / 10.0
}

fn round1(x: f32) -> f32 {
	(x * 10.0).round() / 10.0
}

/// A fit and the EQ param changes that apply it.
pub type MatchPlan = (MatchFit, Vec<(&'static str, f32)>);

/// The plan of a match: the fit and the param changes, from two spectra (pure).
pub fn plan_match(
	project: &Project,
	track: Option<TrackId>,
	device: DeviceId,
	spectra: &MatchSpectra,
	opts: &MatchOptions,
) -> Result<MatchPlan, String> {
	let d = eq_device(project, track, device)?;
	let mut target = match_target(&spectra.reference, &spectra.input);
	let info = cadenza_dsp::device_info("eq").expect("the EQ is a built-in");
	let params: Vec<f32> = info
		.params
		.iter()
		.map(|p| d.params.get(p.id).copied().unwrap_or(p.default))
		.collect();
	let bands = EqBands::new(&params, 48_000.0);
	for (t, &hz) in target.iter_mut().zip(&THIRD_OCTAVE_HZ) {
		let cut = bands.band_response_db(0, hz) + bands.band_response_db(6, hz);
		if cut < -CUT_MASK_DB {
			*t = 0.0;
		}
	}
	let fit = fit(&target, opts);
	let changes = fit_changes(&fit);
	Ok((fit, changes))
}

/// ONE undo step setting `changes` on the device.
pub fn match_command(
	track: Option<TrackId>,
	device: DeviceId,
	changes: &[(&str, f32)],
	label: &str,
) -> Command {
	Command::Batch {
		label: label.to_owned(),
		commands: changes
			.iter()
			.map(|&(param, value)| Command::SetParam {
				address: ParamAddress::device(track, device, param),
				value,
			})
			.collect(),
	}
}

// -- MCP tool ----------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
pub struct MatchEqArgs {
	/// Track holding the EQ (omit for an EQ on the master).
	#[serde(default)]
	pub track_id: Option<TrackId>,
	/// The built-in `eq` insert to fit.
	pub device_id: DeviceId,
	/// Reference audio file (wav, flac, mp3, ogg...). Give this or reference_track_id.
	#[serde(default)]
	pub reference_file: Option<String>,
	/// Or another track of the song as the reference.
	#[serde(default)]
	pub reference_track_id: Option<TrackId>,
	/// How far to go toward the reference, 0..100 % (default 100).
	#[serde(default)]
	pub amount: Option<f32>,
	/// Largest boost / cut per band, dB (default 6, at most 12).
	#[serde(default)]
	pub max_gain_db: Option<f32>,
	/// At most this many bands (1..5, default 5).
	#[serde(default)]
	pub max_bands: Option<u8>,
	/// Part of the reference file to use, seconds.
	#[serde(default)]
	pub reference_start_seconds: Option<f64>,
	#[serde(default)]
	pub reference_seconds: Option<f64>,
	/// Part of the song to analyze (default: all).
	#[serde(default)]
	pub start_beat: Option<f64>,
	#[serde(default)]
	pub end_beat: Option<f64>,
	/// true = only report the bands, change nothing.
	#[serde(default)]
	pub dry_run: Option<bool>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct MatchEqOutput {
	/// The fitted bands (shelves and bells) as set on the EQ.
	pub bands: Vec<MatchBand>,
	/// "Peak 2: bell 2400 Hz +2.5 dB Q 1.4", one per band.
	pub applied: Vec<String>,
	/// Weighted RMS of the tonal difference before and after the fit (dB).
	pub difference_db: f64,
	pub residual_db: f64,
	/// The undo label (absent on a dry run).
	#[serde(skip_serializing_if = "Option::is_none")]
	pub undo_label: Option<String>,
	pub summary: String,
}

/// Validated arguments: the reference and the fit options.
pub fn parse_args(
	args: &MatchEqArgs,
	file: Option<PathBuf>,
) -> Result<(MatchReference, MatchOptions), McpError> {
	let reference = match (file, args.reference_track_id) {
		(Some(path), None) => MatchReference::File {
			path,
			start_seconds: args.reference_start_seconds,
			seconds: args.reference_seconds,
		},
		(None, Some(id)) => MatchReference::Track(id),
		(Some(_), Some(_)) => {
			return Err(McpError::Invalid(
				"give reference_file or reference_track_id, not both".into(),
			));
		}
		(None, None) => {
			return Err(McpError::Invalid(
				"missing reference: pass reference_file (an audio file) or reference_track_id"
					.into(),
			));
		}
	};
	let amount = args.amount.unwrap_or(100.0);
	if !(0.0..=100.0).contains(&amount) {
		return Err(McpError::Invalid(format!(
			"amount {amount} is outside 0..100 (percent of the difference to apply)"
		)));
	}
	let max_gain = args.max_gain_db.unwrap_or(DEFAULT_MAX_GAIN_DB);
	if !(0.5..=12.0).contains(&max_gain) {
		return Err(McpError::Invalid(format!(
			"max_gain_db {max_gain} is outside 0.5..12 dB"
		)));
	}
	let max_bands = args.max_bands.unwrap_or(5);
	if !(1..=5).contains(&max_bands) {
		return Err(McpError::Invalid(format!(
			"max_bands {max_bands} is outside 1..5 (the EQ has a low shelf, three peaks and a \
			 high shelf)"
		)));
	}
	Ok((
		reference,
		MatchOptions {
			amount: amount / 100.0,
			max_gain_db: max_gain,
			max_bands: max_bands.into(),
		},
	))
}

/// "Peak 2: bell 2400 Hz +2.5 dB Q 1.4" for each band, in the slots [`fit_changes`] uses.
pub fn describe(fit: &MatchFit) -> Vec<String> {
	let rank = |k: EqKind| match k {
		EqKind::LowShelf => 0,
		EqKind::HighShelf => 2,
		_ => 1,
	};
	let mut bands = fit.bands.clone();
	bands.sort_by(|a, b| {
		rank(a.kind)
			.cmp(&rank(b.kind))
			.then(a.freq_hz.total_cmp(&b.freq_hz))
	});
	let mut peak = 0;
	bands
		.iter()
		.map(|b| {
			let slot = match b.kind {
				EqKind::LowShelf => "Low shelf".to_owned(),
				EqKind::HighShelf => "High shelf".to_owned(),
				_ => {
					peak += 1;
					format!("Peak {peak}")
				}
			};
			let q = if b.kind == EqKind::Bell {
				format!(" Q {:.1}", b.q)
			} else {
				String::new()
			};
			format!(
				"{slot}: {:.0} Hz {:+.1} dB{q}",
				b.freq_hz.round(),
				round1(b.gain_db)
			)
		})
		.collect()
}

/// The MCP tool: analyze (async, blocking work on a worker), fit, apply as one step.
pub async fn match_eq(
	shared: crate::SharedSession,
	args: MatchEqArgs,
	file: Option<PathBuf>,
) -> Result<MatchEqOutput, McpError> {
	let (reference, opts) = parse_args(&args, file)?;
	let (project, plugins, hosting) = {
		let mut s = shared.lock().unwrap_or_else(|e| e.into_inner());
		(
			s.document().project().clone(),
			s.plugins_for_project(),
			s.plugin_hosting().clone(),
		)
	};
	check_match(&project, args.track_id, args.device_id, &reference).map_err(McpError::Invalid)?;
	let (track, device, range) = (
		args.track_id,
		args.device_id,
		(args.start_beat, args.end_beat),
	);
	let spectra = tokio::task::spawn_blocking(move || {
		analyze_match_sources(
			&project,
			plugins,
			hosting,
			track,
			device,
			&reference,
			range,
			&mut |_| true,
		)
	})
	.await
	.map_err(|e| McpError::Invalid(format!("match task panicked: {e}")))??;
	let mut session = shared.lock().unwrap_or_else(|e| e.into_inner());
	apply_match(&mut session, &args, &spectra, &opts)
}

/// Fits and (unless `dry_run`) applies on the session's current document.
pub fn apply_match(
	session: &mut Session,
	args: &MatchEqArgs,
	spectra: &MatchSpectra,
	opts: &MatchOptions,
) -> Result<MatchEqOutput, McpError> {
	let project = session.document().project();
	let (fit, changes) = plan_match(project, args.track_id, args.device_id, spectra, opts)
		.map_err(McpError::Invalid)?;
	let applied = describe(&fit);
	let dry = args.dry_run.unwrap_or(false);
	let label = match_label(&spectra.reference_name);
	let summary = if fit.bands.is_empty() {
		format!(
			"the EQ input already matches '{}' within {:.1} dB: nothing to change",
			spectra.reference_name, fit.before_db
		)
	} else {
		format!(
			"{} band(s) toward '{}': tonal difference {:.1} dB -> {:.1} dB{}",
			fit.bands.len(),
			spectra.reference_name,
			fit.before_db,
			fit.residual_db,
			if dry {
				" (dry run, nothing changed)"
			} else {
				""
			}
		)
	};
	let undo_label = if dry || fit.bands.is_empty() {
		None
	} else {
		session.document_mut().execute(match_command(
			args.track_id,
			args.device_id,
			&changes,
			&label,
		))?;
		Some(label)
	};
	Ok(MatchEqOutput {
		bands: fit
			.bands
			.iter()
			.map(|b| MatchBand {
				freq_hz: b.freq_hz.round(),
				gain_db: round1(b.gain_db),
				q: (b.q * 100.0).round() / 100.0,
				..*b
			})
			.collect(),
		applied,
		difference_db: r1(fit.before_db),
		residual_db: r1(fit.residual_db),
		undo_label,
		summary,
	})
}

#[cfg(test)]
mod tests {
	use super::*;
	use cadenza_analysis::match_eq::MatchOptions;

	#[test]
	fn a_fit_fills_every_tonal_band_and_leaves_the_filters() {
		let fit = MatchFit {
			bands: vec![
				MatchBand {
					kind: EqKind::Bell,
					freq_hz: 3_000.4,
					gain_db: 2.04,
					q: 1.4,
				},
				MatchBand {
					kind: EqKind::LowShelf,
					freq_hz: 110.0,
					gain_db: -2.5,
					q: 0.7,
				},
				MatchBand {
					kind: EqKind::Bell,
					freq_hz: 400.0,
					gain_db: -1.5,
					q: 0.8,
				},
			],
			target_db: vec![],
			fitted_db: vec![],
			before_db: 2.0,
			residual_db: 0.4,
		};
		let changes = fit_changes(&fit);
		let get = |id: &str| changes.iter().find(|(p, _)| *p == id).map(|c| c.1);
		assert_eq!(get("peak1_freq"), Some(400.0));
		assert_eq!(get("peak2_freq"), Some(3_000.0));
		assert_eq!(get("peak2_gain"), Some(2.0));
		assert_eq!(get("peak3_gain"), Some(0.0));
		assert_eq!(get("low_shelf_gain"), Some(-2.5));
		assert_eq!(get("high_shelf_gain"), Some(0.0));
		assert_eq!(get("peak2_ms"), Some(0.0));
		assert_eq!(get("hp_freq"), None);
		assert_eq!(
			describe(&fit),
			vec![
				"Low shelf: 110 Hz -2.5 dB",
				"Peak 1: 400 Hz -1.5 dB Q 0.8",
				"Peak 2: 3000 Hz +2.0 dB Q 1.4"
			]
		);
		let _ = MatchOptions::default();
	}
}
