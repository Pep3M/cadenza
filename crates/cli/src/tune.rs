//! `cadenza tune`: a tuning pass by the embedded AI tuning engineer on one audio clip of a
//! project (the app's "Tune with AI"). The clip's notes are read before and after
//! (analyze_vocal) and the numbers printed; the pass is one undo step "AI tuning pass".

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use cadenza_agent::{RequestStatus, SessionAgent, TuningBrief};
use cadenza_core::ClipId;
use cadenza_mcp::tools::tuning::{self, AnalyzeVocalArgs, TargetArgs, VocalReport};
use cadenza_mcp::{Session, SharedSession};
use cadenza_model::{Clip, ClipContent, Document, Project};
use clap::Args;

use crate::chat::{ProviderArgs, cli_context, report_outcome, run_with_ctrl_c};
use crate::compose::Printer;

#[derive(Args, Debug)]
pub struct TuneArgs {
	/// Project holding the take (.cdz)
	pub project: PathBuf,
	/// Audio clip to tune: its id or name
	#[arg(long, required_unless_present = "track")]
	pub clip: Option<String>,
	/// Tune the audio clip of this track (id or name; it must hold exactly one)
	#[arg(long, conflicts_with = "clip")]
	pub track: Option<String>,
	/// Scale to tune to, e.g. "E phrygian" (default: fitted to the notes)
	#[arg(long)]
	pub key: Option<String>,
	/// MIDI clip holding the intended melody (id or name): its notes decide the targets
	#[arg(long)]
	pub reference: Option<String>,
	/// Extra wishes for the engineer, e.g. "natural, keep the slides" or "hard tune"
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

/// The clip named by `query` (id or case-insensitive name) among those `keep` accepts.
fn find_clip<'a>(
	project: &'a Project,
	query: &str,
	what: &str,
	keep: impl Fn(&Clip) -> bool,
) -> Result<&'a Clip> {
	let q = query.trim();
	let clips: Vec<&Clip> = project
		.tracks
		.iter()
		.flat_map(|t| &t.clips)
		.filter(|c| keep(c))
		.collect();
	if let Ok(id) = q.parse::<ClipId>()
		&& let Some(c) = clips.iter().find(|c| c.id == id)
	{
		return Ok(c);
	}
	let named: Vec<&&Clip> = clips
		.iter()
		.filter(|c| c.name.eq_ignore_ascii_case(q))
		.collect();
	match named.as_slice() {
		[c] => Ok(c),
		[] => bail!(
			"no {what} clip '{q}'; {what} clips: {}",
			clips
				.iter()
				.map(|c| format!("'{}' ({})", c.name, c.id))
				.collect::<Vec<_>>()
				.join(", ")
		),
		_ => bail!("several {what} clips are named '{q}'; pass the clip id"),
	}
}

fn is_audio(c: &Clip) -> bool {
	matches!(c.content, ClipContent::Audio { .. })
}

fn report(session: &SharedSession, clip: ClipId, targets: &TargetArgs) -> Option<VocalReport> {
	let session = session.lock().unwrap_or_else(|p| p.into_inner());
	tuning::analyze_vocal(
		&session,
		AnalyzeVocalArgs {
			clip_id: clip,
			targets: targets.clone(),
		},
	)
	.ok()
}

fn print_report(title: &str, r: &VocalReport) {
	eprintln!(
		"{title}: {} notes {}, {} {:+.0} c; error sung {:.1} c, now {:.1} c; {} on target, {} \
		 decision(s)",
		r.notes,
		r.range,
		r.scale.as_deref().unwrap_or("nearest semitone"),
		r.offset_cents,
		r.sung_error_cents,
		r.now_error_cents,
		r.on_target,
		r.decisions.len() + r.more_decisions
	);
}

pub fn run(args: TuneArgs) -> Result<()> {
	let config = args.ai.session_config()?;
	let project = Project::load_json(&args.project)
		.with_context(|| format!("opening {}", args.project.display()))?;
	let clip = match (&args.clip, &args.track) {
		(Some(q), _) => find_clip(&project, q, "audio", is_audio)?,
		(None, Some(t)) => {
			let track = crate::find_track(&project, t)?;
			let audio: Vec<&Clip> = track.clips.iter().filter(|c| is_audio(c)).collect();
			match audio.as_slice() {
				[c] => *c,
				[] => bail!("track '{}' has no audio clip", track.name),
				_ => bail!(
					"track '{}' has {} audio clips; pick one with --clip (id or name)",
					track.name,
					audio.len()
				),
			}
		}
		(None, None) => bail!("pass --clip or --track"),
	};
	let reference = args
		.reference
		.as_deref()
		.map(|q| find_clip(&project, q, "MIDI", |c| !is_audio(c)))
		.transpose()?;
	let track = project
		.find_clip(clip.id)
		.map(|(t, _)| t.name.clone())
		.unwrap_or_default();
	let brief = TuningBrief {
		clip: format!(
			"\"{}\" (clip {}, track \"{track}\", beats {:.2}-{:.2}, audio)",
			clip.name,
			clip.id,
			clip.start.beats(),
			(clip.start + clip.length).beats()
		),
		key: args.key.clone(),
		reference: reference.map(|c| format!("\"{}\" (clip {})", c.name, c.id)),
		notes: args.notes.clone(),
	};
	let targets = TargetArgs {
		key: args.key.clone(),
		offset_cents: None,
		reference_clip_id: reference.map(|c| c.id),
	};
	let clip_id = clip.id;

	let mut session = Session::new();
	session.reset(Document::new(project), Some(args.project.clone()));
	let shared = session.into_shared();
	match report(&shared, clip_id, &targets) {
		Some(r) => print_report("before", &r),
		None => eprintln!("before: the clip has no detected notes yet"),
	}

	let mut agent =
		SessionAgent::new(config, shared.clone()).context("setting up the AI session")?;
	let context = cli_context(&args.project);
	eprintln!(
		"tuning pass by {} via {} (budget ${:.2})",
		agent.provider().model(),
		agent.provider().name(),
		args.ai.max_cost_usd
	);
	let observer = Printer::default();
	let outcome = {
		let (agent, context, observer, brief) = (&mut agent, &context, &observer, &brief);
		run_with_ctrl_c(|cancel| async move {
			agent
				.run_tuning_pass(brief, context, observer, &cancel)
				.await
		})?
	};
	eprintln!();
	report_outcome(&outcome, &agent, &outcome.undo_label);

	if outcome.edited {
		if let Some(r) = report(&shared, clip_id, &targets) {
			print_report("after", &r);
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
		eprintln!("the clip was not changed");
	}
	if let Some(text) = &outcome.final_message {
		println!("{text}");
	}
	match outcome.status {
		RequestStatus::Completed | RequestStatus::Cancelled => Ok(()),
		RequestStatus::Failed(why) => bail!("the tuning pass failed: {why}"),
		other => {
			eprintln!("stopped: {other:?}");
			Ok(())
		}
	}
}
