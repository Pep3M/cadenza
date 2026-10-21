//! Mix assistant (the AI panel's Mix tab): analysis on a worker thread with progress, a
//! report the views draw (levels, spectrum, masking, issues), one-click fixes through the
//! same move builders the AI uses (`cadenza_mcp::tools::mix`), and the "AI mix pass": the
//! session agent in engineer mode, measured before and after, kept or reverted as one undo
//! step.

use crate::i18n::{mark, tr, trf};
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::time::Instant;

use cadenza_agent::{ENGINEER_UNDO_LABEL, EngineerBrief};
use cadenza_analysis::spectrum::OCTAVE_HZ;
use cadenza_engine::RenderOptions;
use cadenza_mcp::ChangeKey;
use cadenza_mcp::tools::mix::{
	AnalysisInput, MetricChange, MixAnalysis, TargetArgs, compare_passes, mix_pass_command,
	resolve_targets, run_analysis,
};

use super::AppState;
use super::ai::worker::Finished;

/// Why an analysis runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
	/// The Analyze button.
	Report,
	/// The "before" numbers of an AI mix pass (the pass starts when it is done).
	BeforePass,
	/// The "after" numbers of an AI mix pass.
	AfterPass,
}

/// Foldable sections of the report card, in display order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MixSection {
	Master,
	Issues,
	Tone,
	Channels,
	Masking,
}

impl MixSection {
	pub const ALL: [MixSection; 5] = [
		MixSection::Master,
		MixSection::Issues,
		MixSection::Tone,
		MixSection::Channels,
		MixSection::Masking,
	];

	/// Folded by default in a panel shorter than this (until the user folds anything):
	/// the master numbers and the issues stay in view.
	pub const COMPACT_HEIGHT: f32 = 560.0;

	fn auto_folded(self, panel_height: f32) -> bool {
		panel_height < Self::COMPACT_HEIGHT
			&& matches!(
				self,
				MixSection::Tone | MixSection::Channels | MixSection::Masking
			)
	}
}

enum JobEvent {
	Progress(f32),
	Done(Result<Box<MixAnalysis>, String>),
}

pub struct AnalysisJob {
	rx: Receiver<JobEvent>,
	cancel: Arc<AtomicBool>,
	pub progress: f32,
	pub purpose: Purpose,
	/// The document the analysis renders.
	key: ChangeKey,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PassPhase {
	/// Measuring the mix before the pass.
	Before,
	/// The engineer is working.
	Running,
	/// Measuring the result.
	After,
	/// Numbers shown with Accept / Revert.
	Review,
}

pub struct MixPass {
	pub phase: PassPhase,
	pub before: Option<Arc<MixAnalysis>>,
	pub after: Option<Arc<MixAnalysis>>,
	pub changes: Vec<MetricChange>,
	/// The pass changed the project (one undo step [`ENGINEER_UNDO_LABEL`]).
	pub edited: bool,
	pub error: Option<String>,
}

#[derive(Default)]
pub struct MixAssistant {
	/// The AI panel shows the Mix tab instead of the chat.
	pub open: bool,
	/// Loudness target override (None: the style's).
	pub target_lufs: Option<f32>,
	pub reference: Option<PathBuf>,
	pub notes: String,
	pub report: Option<Arc<MixAnalysis>>,
	report_key: Option<ChangeKey>,
	pub job: Option<AnalysisJob>,
	pub pass: Option<MixPass>,
	/// Issue ids fixed since the report was made.
	pub fixed: BTreeSet<String>,
	pub error: Option<String>,
	/// Report sections folded by the user (`None`: automatic, by the panel height).
	pub folded: Option<BTreeSet<MixSection>>,
	/// When the current report was measured.
	pub analyzed_at: Option<Instant>,
}

impl MixAssistant {
	pub fn busy(&self) -> bool {
		self.job.is_some()
			|| self
				.pass
				.as_ref()
				.is_some_and(|p| p.phase != PassPhase::Review)
	}

	pub fn analyzing(&self) -> Option<(Purpose, f32)> {
		self.job.as_ref().map(|j| (j.purpose, j.progress))
	}

	/// Whether a report section is folded in a panel `panel_height` tall.
	pub fn is_folded(&self, section: MixSection, panel_height: f32) -> bool {
		match &self.folded {
			Some(set) => set.contains(&section),
			None => section.auto_folded(panel_height),
		}
	}

	/// Folds / unfolds a section; from then on the sections stay as the user leaves them.
	pub fn toggle_section(&mut self, section: MixSection, panel_height: f32) {
		let mut set = self.folded.take().unwrap_or_else(|| {
			MixSection::ALL
				.into_iter()
				.filter(|s| s.auto_folded(panel_height))
				.collect()
		});
		if !set.remove(&section) {
			set.insert(section);
		}
		self.folded = Some(set);
	}

	fn targets(&self) -> TargetArgs {
		TargetArgs {
			target_lufs: self.target_lufs,
			..TargetArgs::default()
		}
	}
}

impl AppState {
	/// Whether the report describes an older version of the project.
	pub fn mix_report_stale(&self) -> bool {
		self.mix.report.is_some() && self.mix.report_key != Some(self.session_key)
	}

	/// Starts an analysis of the current mix on a worker (the Analyze button).
	pub fn mix_analyze(&mut self) {
		self.start_mix_job(Purpose::Report);
	}

	pub fn mix_cancel(&mut self) {
		if let Some(job) = self.mix.job.take() {
			job.cancel.store(true, Ordering::Relaxed);
		}
		if self
			.mix
			.pass
			.as_ref()
			.is_some_and(|p| p.phase == PassPhase::Before)
		{
			self.mix.pass = None;
		}
	}

	fn start_mix_job(&mut self, purpose: Purpose) {
		if let Some(old) = self.mix.job.take() {
			old.cancel.store(true, Ordering::Relaxed);
		}
		self.mix.error = None;
		let library = self.library();
		let input = {
			let mut session = self.session();
			session.set_library(library);
			AnalysisInput::from_session(&mut session)
		};
		let targets = resolve_targets(&input.project, &input.library, &self.mix.targets());
		let opts = RenderOptions {
			plugins: input.plugins.clone(),
			hosting: input.hosting.clone(),
			..RenderOptions::default()
		};
		let (tx, rx) = mpsc::channel();
		let cancel = Arc::new(AtomicBool::new(false));
		let stop = cancel.clone();
		let waker = self.waker.clone();
		let wake = move || {
			if let Some(w) = &waker {
				w();
			}
		};
		let spawned = std::thread::Builder::new()
			.name("cadenza-mix-analysis".into())
			.spawn(move || {
				let mut last = 0.0;
				let result =
					run_analysis(&input.project, &input.library, &opts, targets, &mut |f| {
						if f - last >= 0.02 || f >= 1.0 {
							last = f;
							let _ = tx.send(JobEvent::Progress(f));
							wake();
						}
						!stop.load(Ordering::Relaxed)
					})
					.map(Box::new)
					.map_err(|e| e.to_string());
				let _ = tx.send(JobEvent::Done(result));
				wake();
			});
		if let Err(e) = spawned {
			self.mix.error = Some(trf("Cannot start the analysis: {e}", &[("e", &e)]));
			return;
		}
		self.mix.job = Some(AnalysisJob {
			rx,
			cancel,
			progress: 0.0,
			purpose,
			key: self.session_key,
		});
	}

	/// Applies one issue's fix (its moves) as one undo step.
	pub fn mix_fix(&mut self, issue_id: &str) {
		let Some(report) = self.mix.report.clone() else {
			return;
		};
		let Some(issue) = report.issues.iter().find(|i| i.id == issue_id) else {
			return;
		};
		if issue.moves.is_empty() || self.mix.fixed.contains(issue_id) {
			return;
		}
		let label = format!("Mix fix: {}", issue_label(&issue.message));
		match mix_pass_command(self.project(), &issue.moves, &label) {
			Ok(cmd) => {
				if self.apply(cmd) {
					self.mix.fixed.insert(issue_id.to_owned());
				}
			}
			Err(e) => self.notices.error(e.to_string()),
		}
	}

	/// Applies every unfixed issue's moves as one undo step "Mix fixes".
	pub fn mix_fix_all(&mut self) {
		let Some(report) = self.mix.report.clone() else {
			return;
		};
		let (ids, moves): (Vec<String>, Vec<_>) = report
			.issues
			.iter()
			.filter(|i| !i.moves.is_empty() && !self.mix.fixed.contains(&i.id))
			.map(|i| (i.id.clone(), i.moves.clone()))
			.unzip();
		if ids.is_empty() {
			return;
		}
		let moves: Vec<_> = moves.into_iter().flatten().collect();
		match mix_pass_command(self.project(), &moves, "Mix fixes") {
			Ok(cmd) => {
				if self.apply(cmd) {
					self.mix.fixed.extend(ids);
				}
			}
			Err(e) => self.notices.error(e.to_string()),
		}
	}

	/// Runs the AI sound engineer on the project: measure, mix, measure again, then review.
	pub fn mix_ai_pass(&mut self) {
		if self.ai.busy() || self.mix.busy() {
			self.notices.info(tr(
				"The AI is busy — wait for it or press Stop in the chat (F10)",
			));
			return;
		}
		let before = (!self.mix_report_stale())
			.then(|| self.mix.report.clone())
			.flatten();
		let fresh = before.is_some();
		self.mix.pass = Some(MixPass {
			phase: PassPhase::Before,
			before,
			after: None,
			changes: Vec::new(),
			edited: false,
			error: None,
		});
		if fresh {
			self.start_engineer();
		} else {
			self.start_mix_job(Purpose::BeforePass);
		}
	}

	fn start_engineer(&mut self) {
		let mut context = self.ai_context();
		context.reference_file = self.mix.reference.as_ref().map(|p| p.display().to_string());
		let brief = EngineerBrief {
			target_lufs: self.mix.target_lufs,
			reference: context.reference_file.clone(),
			notes: Some(self.mix.notes.clone()).filter(|n| !n.trim().is_empty()),
		};
		let session = self.session.clone();
		if !self.ai.busy() {
			self.begin_ai_changes(ENGINEER_UNDO_LABEL);
		}
		let sent = self
			.ai
			.send_mix_pass(ENGINEER_UNDO_LABEL, brief, context, &session);
		match (&mut self.mix.pass, sent) {
			(Some(pass), true) => pass.phase = PassPhase::Running,
			(_, false) => {
				self.mix.pass = None;
				self.notices
					.error(tr("The AI could not start the mix pass"));
			}
			(None, true) => {}
		}
	}

	/// The engineer finished (called from the chat's events).
	pub(super) fn mix_pass_finished(&mut self, finished: &Finished) {
		let Some(pass) = &mut self.mix.pass else {
			return;
		};
		if pass.phase != PassPhase::Running {
			return;
		}
		pass.edited = finished.edited;
		pass.error = finished.error.clone();
		if finished.edited {
			pass.phase = PassPhase::After;
			self.start_mix_job(Purpose::AfterPass);
		} else {
			pass.phase = PassPhase::Review;
		}
	}

	/// Keeps the pass (its undo step stays in the history).
	pub fn mix_accept(&mut self) {
		if let Some(pass) = self.mix.pass.take()
			&& let Some(after) = pass.after
		{
			self.mix.report = Some(after);
			self.mix.report_key = Some(self.session_key);
			self.mix.fixed.clear();
		}
	}

	/// Undoes the pass (only while it is still the last step).
	pub fn mix_revert(&mut self) {
		let Some(pass) = self.mix.pass.take() else {
			return;
		};
		if !pass.edited {
			return;
		}
		if self.undo_label() != Some(ENGINEER_UNDO_LABEL) {
			self.notices.warn(tr(
				"The mix pass is no longer the last step: undo it from the history instead",
			));
			return;
		}
		self.undo();
		if let Some(before) = pass.before {
			self.mix.report = Some(before);
			self.mix.report_key = Some(self.session_key);
			self.mix.fixed.clear();
		}
	}

	/// Collects the worker's progress and results (each frame).
	pub(super) fn poll_mix(&mut self) {
		let Some(job) = &mut self.mix.job else {
			return;
		};
		let mut done = None;
		loop {
			match job.rx.try_recv() {
				Ok(JobEvent::Progress(f)) => job.progress = f,
				Ok(JobEvent::Done(r)) => {
					done = Some(r);
					break;
				}
				Err(mpsc::TryRecvError::Empty) => break,
				Err(mpsc::TryRecvError::Disconnected) => {
					done = Some(Err(tr("the analysis thread stopped").into()));
					break;
				}
			}
		}
		let Some(result) = done else {
			return;
		};
		let job = self.mix.job.take().expect("polled above");
		let analysis = match result {
			Ok(a) => Arc::new(*a),
			Err(e) => {
				if e.contains("cancelled") || e.contains("Cancelled") {
					return;
				}
				self.mix.error = Some(trf("Analysis failed: {e}", &[("e", &e)]));
				if let Some(pass) = &mut self.mix.pass {
					match pass.phase {
						PassPhase::Before => self.mix.pass = None,
						_ => pass.phase = PassPhase::Review,
					}
				}
				return;
			}
		};
		match job.purpose {
			Purpose::Report => {
				self.mix.report = Some(analysis);
				self.mix.report_key = Some(job.key);
				self.mix.analyzed_at = Some(Instant::now());
				self.mix.fixed.clear();
			}
			Purpose::BeforePass => {
				self.mix.report = Some(analysis.clone());
				self.mix.report_key = Some(job.key);
				self.mix.analyzed_at = Some(Instant::now());
				self.mix.fixed.clear();
				if let Some(pass) = &mut self.mix.pass {
					pass.before = Some(analysis);
				}
				self.start_engineer();
			}
			Purpose::AfterPass => {
				if let Some(pass) = &mut self.mix.pass {
					if let Some(before) = &pass.before {
						pass.changes = compare_passes(before, &analysis);
					}
					pass.after = Some(analysis);
					pass.phase = PassPhase::Review;
				}
			}
		}
	}
}

/// A short undo label from an issue message ("'Bass' and 'Keys' collide...").
fn issue_label(message: &str) -> String {
	let first = message.split(':').next().unwrap_or(message).trim();
	let mut s: String = first.chars().take(48).collect();
	if first.chars().count() > 48 {
		s.push('…');
	}
	s
}

/// Loudness targets offered by the Mix tab's target menu (name, LUFS).
pub const TARGET_PRESETS: [(&str, f32); 4] = [
	(mark("Streaming"), -14.0),
	(mark("Apple Music"), -16.0),
	(mark("Club / loud"), -9.0),
	(mark("Broadcast"), -23.0),
];

/// The target menu's label: the preset a value matches, else the number.
pub fn target_label(target: Option<f32>, style_lufs: f32) -> String {
	match target {
		None => trf("Style {lufs}", &[("lufs", &signed(style_lufs, 0))]),
		Some(v) => TARGET_PRESETS
			.iter()
			.find(|(_, l)| (l - v).abs() < 0.05)
			.map_or_else(
				|| trf("Custom {lufs}", &[("lufs", &signed(v, 1))]),
				|(n, l)| format!("{} {}", crate::i18n::tr_str(n), signed(*l, 0)),
			),
	}
}

/// A number with a real minus sign ("−14", "+2.8").
pub fn signed(v: f32, decimals: usize) -> String {
	let s = format!("{:.*}", decimals, v.abs());
	if s.trim_start_matches(['0', '.']).is_empty() {
		s
	} else if v < 0.0 {
		format!("−{s}")
	} else {
		s
	}
}

/// The same with an explicit plus for positive values.
pub fn delta(v: f32, decimals: usize) -> String {
	let s = signed(v, decimals);
	if v > 0.0 && !s.trim_start_matches(['0', '.']).is_empty() {
		format!("+{s}")
	} else {
		s
	}
}

/// An issue message split into its headline and the detail after the first colon
/// ("'Bass' and 'Keys' collide around 125 Hz: cut 'Keys' 4 dB there").
pub fn issue_parts(message: &str) -> (String, Option<String>) {
	let capital = |s: &str| {
		let mut c = s.chars();
		c.next().map_or_else(String::new, |f| {
			f.to_uppercase().collect::<String>() + c.as_str()
		})
	};
	match message.split_once(": ") {
		Some((head, rest)) if !rest.trim().is_empty() => {
			(capital(head.trim()), Some(capital(rest.trim())))
		}
		_ => (capital(message.trim()), None),
	}
}

/// Suggested loudness band half width around a channel's balance level, dB.
pub const BAND_HALF_DB: f32 = 3.0;

/// One row of the Channels card.
#[derive(Debug, Clone, PartialEq)]
pub struct ChannelRow {
	/// Index into `MixAnalysis::channels`.
	pub index: usize,
	pub silent: bool,
	pub lufs: f32,
	pub peak_db: f32,
	/// The level its role usually sits at ± [`BAND_HALF_DB`] (sources only, not buses).
	pub band: Option<(f32, f32)>,
	/// How far outside the band (+ = too loud), 0 inside.
	pub out_db: f32,
}

/// The Channels card: every channel with the band its role suggests, anchored on the
/// median like the analysis's balance check.
pub fn channel_rows(r: &MixAnalysis) -> Vec<ChannelRow> {
	let source = |c: &cadenza_mcp::tools::mix::ChannelAnalysis| {
		c.kind != cadenza_model::TrackKind::Bus && !c.detail.is_silent()
	};
	let mut anchors: Vec<f32> = r
		.channels
		.iter()
		.filter(|c| source(c))
		.map(|c| c.detail.stats.lufs_integrated - r.targets.balance_db(c.role))
		.collect();
	anchors.sort_by(f32::total_cmp);
	let anchor = match anchors.len() {
		0 | 1 => None,
		n if n.is_multiple_of(2) => Some((anchors[n / 2 - 1] + anchors[n / 2]) * 0.5),
		n => Some(anchors[n / 2]),
	};
	r.channels
		.iter()
		.enumerate()
		.map(|(index, c)| {
			let lufs = c.detail.stats.lufs_integrated;
			let band = anchor.filter(|_| source(c)).map(|a| {
				let mid = a + r.targets.balance_db(c.role);
				(mid - BAND_HALF_DB, mid + BAND_HALF_DB)
			});
			let out_db = band.map_or(0.0, |(lo, hi)| {
				if lufs > hi {
					lufs - hi
				} else if lufs < lo {
					lufs - lo
				} else {
					0.0
				}
			});
			ChannelRow {
				index,
				silent: c.detail.is_silent(),
				lufs,
				peak_db: c.detail.stats.peak_db,
				band,
				out_db,
			}
		})
		.collect()
}

/// Band balance minus the target profile's curve (+ = too much): lows, low-mids,
/// high-mids, highs.
pub fn tone_deviation(r: &MixAnalysis) -> [(&'static str, f32); 4] {
	let b = &r.master.stats.band_energy_db;
	let c = r.targets.profile.curve();
	[
		("lows", b.low - b.mid - c.low),
		("low-mids", b.low_mid - b.mid - c.low_mid),
		("high-mids", b.high_mid - b.mid - c.high_mid),
		("highs", b.high - b.mid - c.high),
	]
}

/// Which tone band (0 lows, 1 low-mids, 2 mids, 3 high-mids, 4 highs; crossovers 150,
/// 500, 2k, 6k Hz) an octave of [`OCTAVE_HZ`] belongs to.
pub fn tone_band_of(octave: usize) -> usize {
	match OCTAVE_HZ[octave] {
		f if f < 150.0 => 0,
		f if f < 500.0 => 1,
		f if f < 2_000.0 => 2,
		f if f < 6_000.0 => 3,
		_ => 4,
	}
}

/// The dashed target line of the Tone card per octave (same scale as `shape`): each tone
/// band's mean mix level minus its deviation from the profile, so the gap between the
/// two lines in a band is exactly the deviation the report states.
pub fn tone_target(shape: &[f32], deviation: &[(&str, f32); 4]) -> Vec<f32> {
	let dev = |band: usize| match band {
		0 => deviation[0].1,
		1 => deviation[1].1,
		2 => 0.0,
		3 => deviation[2].1,
		_ => deviation[3].1,
	};
	let mut sum = [0.0f32; 5];
	let mut n = [0usize; 5];
	for (i, v) in shape.iter().enumerate().take(OCTAVE_HZ.len()) {
		let b = tone_band_of(i);
		sum[b] += v;
		n[b] += 1;
	}
	(0..shape.len().min(OCTAVE_HZ.len()))
		.map(|i| {
			let b = tone_band_of(i);
			sum[b] / n[b].max(1) as f32 - dev(b)
		})
		.collect()
}

/// A masking pair's severity: lit steps of a 5-step meter and its word.
pub fn masking_level(overlap: f32) -> (usize, &'static str) {
	let steps = ((overlap * 5.0).ceil() as usize).clamp(1, 5);
	let word = match steps {
		4.. => mark("High"),
		3 => mark("Medium"),
		_ => mark("Low"),
	};
	(steps, word)
}

/// The frequency range of an octave band around `center` ("88–177 Hz", "1.4–2.8 kHz").
pub fn octave_range(center: f32) -> String {
	let (lo, hi) = (
		center / std::f32::consts::SQRT_2,
		center * std::f32::consts::SQRT_2,
	);
	let f = |v: f32| {
		if v >= 10_000.0 {
			format!("{:.0}", v / 1_000.0)
		} else {
			format!("{:.1}", v / 1_000.0)
		}
	};
	if hi < 1_000.0 {
		format!("{lo:.0}–{hi:.0} Hz")
	} else if lo < 1_000.0 {
		trf(
			"{lo} Hz–{f} kHz",
			&[
				("lo", &crate::i18n::decimal(format!("{:.0}", lo))),
				("f", &f(hi)),
			],
		)
	} else {
		trf("{f}–{f2} kHz", &[("f", &f(lo)), ("f2", &f(hi))])
	}
}

/// One row of the before → after table of an AI mix pass.
#[derive(Debug, Clone, PartialEq)]
pub struct ReviewRow {
	pub label: String,
	pub before: String,
	pub after: String,
	pub target: String,
	/// The before value misses its target (drawn amber).
	pub before_bad: bool,
	/// The after value meets it (drawn green).
	pub after_good: bool,
	/// A channel's loudness row (the label is the track name).
	pub channel: bool,
}

/// The pass's changes against the report's targets; unchanged rows without a target
/// are left out, the loudness row is always first.
pub fn review_rows(changes: &[MetricChange], lufs: f32, ceiling: f32) -> Vec<ReviewRow> {
	let num = |v: f64, unit: &str| {
		let s = if v.fract().abs() < 1e-9 && v.abs() < 1000.0 {
			signed(v as f32, 0)
		} else {
			signed(v as f32, 1)
		};
		match unit {
			"" => s,
			"%" => format!("{s} %"),
			u => format!("{s} {u}"),
		}
	};
	changes
		.iter()
		.filter_map(|c| {
			let lufs = f64::from(lufs);
			let ceiling = f64::from(ceiling);
			let tolerance = f64::from(cadenza_analysis::targets::LOUDNESS_TOLERANCE_LU);
			// (target text, the range of values that meet it)
			let (target, meets) = match c.label.as_str() {
				"Loudness" => (
					format!("{} LUFS", signed(lufs as f32, 0)),
					Some((lufs - tolerance, lufs + tolerance)),
				),
				"True peak" => (
					format!("≤ {}", signed(ceiling as f32, 1)),
					Some((f64::NEG_INFINITY, ceiling + 0.05)),
				),
				"Issues" | "Severe issues" | "Masking pairs" => {
					("0".into(), Some((f64::NEG_INFINITY, 0.5)))
				}
				_ => (String::new(), None),
			};
			let ok = |v: f64| meets.map(|(lo, hi)| (lo..=hi).contains(&v));
			let changed = c.delta().abs() >= 0.05;
			if !changed && meets.is_none() {
				return None;
			}
			let channel = !matches!(
				c.label.as_str(),
				"Loudness"
					| "True peak" | "Crest factor"
					| "Width" | "Issues"
					| "Severe issues"
					| "Masking pairs"
			);
			Some(ReviewRow {
				label: c.label.clone(),
				before: num(c.before, &c.unit),
				after: num(c.after, &c.unit),
				target,
				before_bad: ok(c.before) == Some(false),
				after_good: ok(c.after) == Some(true) && changed,
				channel,
			})
		})
		.collect()
}

#[cfg(test)]
mod tests {

	use std::time::{Duration, Instant};

	use cadenza_agent::scripted::ScriptedTurn;
	use cadenza_analysis::issues::IssueKind;

	use super::*;
	use crate::state::tests::ai_chat::{chat_state, wait};

	#[test]
	fn short_panels_fold_the_detail_sections_until_the_user_chooses() {
		let mut m = MixAssistant::default();
		let short = MixSection::COMPACT_HEIGHT - 100.0;
		let tall = MixSection::COMPACT_HEIGHT + 100.0;
		assert!(!m.is_folded(MixSection::Master, short));
		assert!(!m.is_folded(MixSection::Issues, short));
		assert!(m.is_folded(MixSection::Channels, short));
		assert!(!m.is_folded(MixSection::Channels, tall));
		// Opening one section keeps the others as they were shown.
		m.toggle_section(MixSection::Channels, short);
		assert!(!m.is_folded(MixSection::Channels, short));
		assert!(m.is_folded(MixSection::Tone, short));
		assert!(m.is_folded(MixSection::Tone, tall), "the choice sticks");
		m.toggle_section(MixSection::Issues, tall);
		assert!(m.is_folded(MixSection::Issues, tall));
	}

	/// Ticks until the mix assistant is idle.
	fn settle(s: &mut AppState) {
		let start = Instant::now();
		while s.mix.busy() || s.ai.busy() {
			s.tick(Instant::now());
			assert!(
				start.elapsed().as_secs() < 60,
				"mix assistant did not settle"
			);
			std::thread::sleep(Duration::from_millis(5));
		}
		s.tick(Instant::now());
	}

	#[test]
	fn analyze_then_fix_an_issue_as_one_undo_step() {
		let mut c = chat_state(Vec::new(), 0, true);
		let s = &mut c.state;
		s.mix.target_lufs = Some(-30.0);
		s.mix_analyze();
		assert!(s.mix.busy());
		settle(s);
		let report = s.mix.report.clone().expect("a report");
		assert_eq!(report.targets.lufs, -30.0);
		assert!(!s.mix_report_stale());
		let loud = report
			.issues
			.iter()
			.find(|i| i.kind == IssueKind::Loudness)
			.expect("far above -30 LUFS");
		let undo_before = s.document().history_labels().len();
		s.mix_fix(&loud.id);
		assert!(s.mix.fixed.contains(&loud.id));
		assert_eq!(s.document().history_labels().len(), undo_before + 1);
		assert!(s.undo_label().unwrap().starts_with("Mix fix: master is"));
		assert!(s.mix_report_stale(), "the project changed since");
		// Fixing twice is a no-op.
		s.mix_fix(&loud.id);
		assert_eq!(s.document().history_labels().len(), undo_before + 1);
	}

	#[test]
	fn an_ai_mix_pass_is_measured_and_can_be_reverted() {
		let mut c = chat_state(
			vec![
				ScriptedTurn::tools(
					Some("Limiting the master."),
					&[(
						"apply_mix_moves",
						r#"{"moves":[{"move":"limiter","ceiling_db":-2,"input_gain_db":4}]}"#,
					)],
				),
				ScriptedTurn::text("Done: -14 -> -11 LUFS."),
			],
			0,
			true,
		);
		let s = &mut c.state;
		s.mix_ai_pass();
		settle(s);
		wait(s);
		let pass = s.mix.pass.as_ref().expect("a pass to review");
		assert_eq!(pass.phase, PassPhase::Review);
		assert!(pass.edited);
		assert!(pass.before.is_some() && pass.after.is_some());
		let loudness = pass.changes.iter().find(|c| c.label == "Loudness").unwrap();
		assert!(loudness.after > loudness.before, "{loudness:?}");
		assert_eq!(s.undo_label(), Some(ENGINEER_UNDO_LABEL));
		assert_eq!(s.project().master.inserts.len(), 1);

		s.mix_revert();
		assert!(s.mix.pass.is_none());
		assert!(s.project().master.inserts.is_empty());
		assert_ne!(s.undo_label(), Some(ENGINEER_UNDO_LABEL));
	}

	#[test]
	fn report_card_helpers() {
		assert_eq!(signed(-14.0, 0), "−14");
		assert_eq!(signed(-0.04, 1), "0.0");
		assert_eq!(delta(2.84, 1), "+2.8");
		assert_eq!(target_label(None, -14.0), "Style −14");
		assert_eq!(target_label(Some(-16.0), -14.0), "Apple Music −16");
		assert_eq!(target_label(Some(-12.5), -14.0), "Custom −12.5");
		let (head, detail) =
			issue_parts("'Bass' and 'Keys' collide around 125 Hz: cut 'Keys' 4 dB");
		assert_eq!(head, "'Bass' and 'Keys' collide around 125 Hz");
		assert_eq!(detail.as_deref(), Some("Cut 'Keys' 4 dB"));
		assert_eq!(issue_parts("master is too loud").0, "Master is too loud");
		assert_eq!(masking_level(0.7), (4, "High"));
		assert_eq!(masking_level(0.5), (3, "Medium"));
		assert_eq!(masking_level(0.0), (1, "Low"));
		assert_eq!(octave_range(125.0), "88–177 Hz");
		assert_eq!(octave_range(1_000.0), "707 Hz–1.4 kHz");
		assert_eq!(octave_range(16_000.0), "11–23 kHz");
		assert_eq!(tone_band_of(0), 0);
		assert_eq!(tone_band_of(3), 1);
		assert_eq!(tone_band_of(5), 2);
		assert_eq!(tone_band_of(7), 3);
		assert_eq!(tone_band_of(9), 4);
		// The target line sits each band's deviation away from the band's mean.
		let shape = [-10.0, -8.0, -6.0, -4.0, 0.0, -2.0, -6.0, -8.0, -12.0, -20.0];
		let dev = [
			("lows", 3.0),
			("low-mids", -1.0),
			("high-mids", 0.0),
			("highs", 2.0),
		];
		let t = tone_target(&shape, &dev);
		assert_eq!(t[0], -8.0 - 3.0);
		assert_eq!(t[3], -4.0 + 1.0);
		assert_eq!(t[4], -1.0);
		assert_eq!(t[9], -16.0 - 2.0);
		// Review rows: targets judge before / after; unchanged rows without one go.
		let m = |label: &str, before, after, unit: &str| MetricChange {
			label: label.into(),
			before,
			after,
			unit: unit.into(),
		};
		let rows = review_rows(
			&[
				m("Loudness", -11.8, -14.1, "LUFS"),
				m("True peak", -0.4, -1.2, "dBTP"),
				m("Width", 30.0, 30.0, "%"),
				m("Issues", 3.0, 0.0, ""),
				m("Bass", -13.0, -15.0, "LUFS"),
			],
			-14.0,
			-1.0,
		);
		assert_eq!(rows.len(), 4);
		assert_eq!(rows[0].target, "−14 LUFS");
		assert!(rows[0].before_bad && rows[0].after_good);
		assert_eq!(rows[1].before, "−0.4 dBTP");
		assert!(rows[1].before_bad && rows[1].after_good);
		assert!(rows[3].channel && rows[3].target.is_empty());
	}

	#[test]
	fn channel_rows_band_around_the_role_level() {
		let mut c = chat_state(Vec::new(), 0, true);
		let s = &mut c.state;
		s.mix_analyze();
		settle(s);
		let r = s.mix.report.clone().unwrap();
		let rows = channel_rows(&r);
		assert_eq!(rows.len(), r.channels.len());
		let sources = rows
			.iter()
			.filter(|row| {
				!row.silent && r.channels[row.index].kind != cadenza_model::TrackKind::Bus
			})
			.count();
		for row in &rows {
			let ch = &r.channels[row.index];
			if ch.kind == cadenza_model::TrackKind::Bus || row.silent || sources < 2 {
				assert!(row.band.is_none());
			} else {
				let (lo, hi) = row.band.expect("sources get a band");
				assert!((hi - lo - 2.0 * BAND_HALF_DB).abs() < 1e-4);
				let inside = (lo..=hi).contains(&row.lufs);
				assert_eq!(inside, row.out_db == 0.0, "{row:?}");
			}
		}
	}

	#[test]
	fn issue_labels_are_short() {
		assert_eq!(
			issue_label("master is -20.0 LUFS: too quiet"),
			"master is -20.0 LUFS"
		);
		assert!(issue_label(&"x".repeat(80)).ends_with('…'));
	}
}
