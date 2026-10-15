//! Mix assistant (the AI panel's Mix tab): analysis on a worker thread with progress, a
//! report the views draw (levels, spectrum, masking, issues), one-click fixes through the
//! same move builders the AI uses (`cadenza_mcp::tools::mix`), and the "AI mix pass": the
//! session agent in engineer mode, measured before and after, kept or reverted as one undo
//! step.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};

use cadenza_agent::{ENGINEER_UNDO_LABEL, EngineerBrief};
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
			self.mix.error = Some(format!("Cannot start the analysis: {e}"));
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
			self.notices
				.info("The AI is busy — wait for it or press Stop in the chat (F10)");
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
		let sent = self
			.ai
			.send_mix_pass(ENGINEER_UNDO_LABEL, brief, context, &session);
		match (&mut self.mix.pass, sent) {
			(Some(pass), true) => pass.phase = PassPhase::Running,
			(_, false) => {
				self.mix.pass = None;
				self.notices.error("The AI could not start the mix pass");
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
			self.notices
				.warn("The mix pass is no longer the last step: undo it from the history instead");
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
					done = Some(Err("the analysis thread stopped".into()));
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
				self.mix.error = Some(format!("Analysis failed: {e}"));
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
				self.mix.fixed.clear();
			}
			Purpose::BeforePass => {
				self.mix.report = Some(analysis.clone());
				self.mix.report_key = Some(job.key);
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
	fn issue_labels_are_short() {
		assert_eq!(
			issue_label("master is -20.0 LUFS: too quiet"),
			"master is -20.0 LUFS"
		);
		assert!(issue_label(&"x".repeat(80)).ends_with('…'));
	}
}
