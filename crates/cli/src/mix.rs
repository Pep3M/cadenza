//! `cadenza mix`: a mix pass by the embedded AI sound engineer on an existing project. The
//! mix is analyzed before and after (offline renders) and the numbers that changed are
//! printed; the pass is one undo step "AI mix pass" in the saved project's history.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use cadenza_agent::{EngineerBrief, RequestStatus, SessionAgent};
use cadenza_engine::RenderOptions;
use cadenza_library::Library;
use cadenza_mcp::tools::mix::{
	AnalysisInput, MixAnalysis, TargetArgs, compare_passes, resolve_targets, run_analysis,
};
use cadenza_mcp::{Session, SharedSession};
use cadenza_model::{Document, Project};
use clap::Args;

use crate::chat::{ProviderArgs, cli_context, report_outcome, run_with_ctrl_c};
use crate::compose::Printer;

#[derive(Args, Debug)]
pub struct MixArgs {
	/// Project to mix (.cadenza.json)
	pub project: PathBuf,
	/// Integrated loudness target in LUFS (default: the song style's, else -14)
	#[arg(long, allow_hyphen_values = true)]
	pub target_lufs: Option<f32>,
	/// Reference track to steer the tone and loudness toward (any audio file)
	#[arg(long)]
	pub reference: Option<PathBuf>,
	/// Extra wishes for the engineer, e.g. "vocal forward, more punch"
	#[arg(long)]
	pub notes: Option<String>,
	/// Run the pass and print the changes, but do not save the project
	#[arg(long, conflicts_with = "out")]
	pub dry_run: bool,
	/// Write the result to this file instead of saving the project in place
	#[arg(short, long)]
	pub out: Option<PathBuf>,
	#[command(flatten)]
	pub ai: ProviderArgs,
}

/// Renders and assesses the session's current mix (with its plugins, like the agent's
/// analyze_mix).
fn analyze(session: &SharedSession, targets: &TargetArgs) -> Result<MixAnalysis> {
	let input = AnalysisInput::from_session(&mut session.lock().unwrap_or_else(|p| p.into_inner()));
	let opts = RenderOptions {
		plugins: input.plugins.clone(),
		hosting: input.hosting.clone(),
		..RenderOptions::default()
	};
	let targets = resolve_targets(&input.project, &input.library, targets);
	run_analysis(&input.project, &input.library, &opts, targets, &mut |_| {
		true
	})
	.context("analyzing the mix")
}

fn print_summary(title: &str, a: &MixAnalysis) {
	let s = &a.master.stats;
	eprintln!(
		"{title}: {:.1} LUFS (target {:.1}, {}), true peak {:.1} dBTP, crest {:.1} dB, {} \
		 issue(s)",
		s.lufs_integrated,
		a.targets.lufs,
		a.targets.source,
		s.true_peak_db,
		s.crest_factor_db,
		a.issues.len()
	);
	for issue in a.issues.iter().take(5) {
		eprintln!("  [{:?}] {}", issue.severity, issue.message);
	}
}

pub fn run(args: MixArgs) -> Result<()> {
	let config = args.ai.session_config()?;
	let project = Project::load_json(&args.project)
		.with_context(|| format!("opening {}", args.project.display()))?;
	let reference = match &args.reference {
		Some(path) => Some(
			cadenza_core::paths::real_path(path)
				.with_context(|| format!("reference track {}", path.display()))?,
		),
		None => None,
	};
	let targets = TargetArgs {
		target_lufs: args.target_lufs,
		..TargetArgs::default()
	};
	let mut session = Session::new();
	session.set_library(std::sync::Arc::new(Library::load_default()));
	session.reset(Document::new(project), Some(args.project.clone()));
	let shared = session.into_shared();
	let before = analyze(&shared, &targets)?;
	print_summary("before", &before);

	let mut agent =
		SessionAgent::new(config, shared.clone()).context("setting up the AI session")?;
	let mut context = cli_context(&args.project);
	context.reference_file = reference.as_ref().map(|p| p.display().to_string());
	let brief = EngineerBrief {
		target_lufs: args.target_lufs,
		reference: context.reference_file.clone(),
		notes: args.notes.clone(),
	};
	eprintln!(
		"mix pass by {} via {} (budget ${:.2})",
		agent.provider().model(),
		agent.provider().name(),
		args.ai.max_cost_usd
	);
	let observer = Printer::default();
	let outcome = {
		let (agent, context, observer, brief) = (&mut agent, &context, &observer, &brief);
		run_with_ctrl_c(|cancel| async move {
			agent
				.run_engineer_pass(brief, context, observer, &cancel)
				.await
		})?
	};
	eprintln!();
	report_outcome(&outcome, &agent, &outcome.undo_label);

	if outcome.edited {
		let after = analyze(&shared, &targets)?;
		print_summary("after", &after);
		eprintln!("changes:");
		for c in compare_passes(&before, &after) {
			if c.delta().abs() >= 0.05 {
				eprintln!(
					"  {:<16} {:>7.1} -> {:>7.1} {}",
					c.label, c.before, c.after, c.unit
				);
			}
		}
		if args.dry_run {
			eprintln!("dry run: the project was not saved");
		} else {
			let path = args.out.clone().unwrap_or_else(|| args.project.clone());
			shared
				.lock()
				.unwrap_or_else(|p| p.into_inner())
				.document()
				.project()
				.save_json(&path)
				.with_context(|| format!("saving {}", path.display()))?;
			eprintln!("saved:      {}", path.display());
		}
	} else {
		eprintln!("the mix was not changed");
	}
	if let Some(text) = &outcome.final_message {
		println!("{text}");
	}
	match outcome.status {
		RequestStatus::Completed | RequestStatus::Cancelled => Ok(()),
		RequestStatus::Failed(why) => bail!("the mix pass failed: {why}"),
		other => {
			eprintln!("stopped: {other:?}");
			Ok(())
		}
	}
}
