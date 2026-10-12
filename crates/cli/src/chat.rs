//! `cadenza chat`: one request to the in-app AI producer, from the command line, on an
//! existing project (the same incremental session mode the desktop app's chat uses).

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use cadenza_agent::config::{
	DEFAULT_MODEL, DEFAULT_REASONING_EFFORT, OPENAI_KEY_ENV, OPENROUTER_KEY_ENV, ProviderConfig,
	resolve_secret,
};
use cadenza_agent::session::{DEFAULT_SESSION_MAX_COST_USD, actionable_error};
use cadenza_agent::{
	AppContext, CancelToken, OpenRouterConfig, RequestStatus, SessionAgent, SessionConfig,
};
use cadenza_mcp::Session;
use cadenza_model::{Document, Project};
use clap::{Args, ValueEnum};

use crate::compose::Printer;

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum ChatProvider {
	/// OpenRouter; key from OPENROUTER_API_KEY or a .env file
	Openrouter,
	/// Any OpenAI-compatible endpoint (--base-url); key from OPENAI_API_KEY if set
	OpenaiCompatible,
}

#[derive(Args, Debug)]
pub struct ChatArgs {
	/// Project to work on (.cadenza.json)
	pub project: PathBuf,
	/// What to do, e.g. "add a bridge after the second chorus"
	pub request: String,
	#[arg(long, value_enum, default_value_t = ChatProvider::Openrouter)]
	pub provider: ChatProvider,
	/// Model id
	#[arg(long, default_value = DEFAULT_MODEL)]
	pub model: String,
	/// Endpoint of the OpenAI-compatible provider (e.g. http://localhost:11434/v1)
	#[arg(long)]
	pub base_url: Option<String>,
	/// Reasoning effort: low, medium, high, or none
	#[arg(long, default_value = DEFAULT_REASONING_EFFORT)]
	pub reasoning_effort: String,
	/// Spending cap for the request in USD
	#[arg(long, default_value_t = DEFAULT_SESSION_MAX_COST_USD)]
	pub max_cost_usd: f64,
	/// Allow installed third-party plugins
	#[arg(long)]
	pub plugins: bool,
	/// Write the result to this file instead of saving the project in place
	#[arg(short, long)]
	pub out: Option<PathBuf>,
	/// Do not save the result
	#[arg(long, conflicts_with = "out")]
	pub no_save: bool,
}

pub fn run(args: ChatArgs) -> Result<()> {
	if args.max_cost_usd.is_nan() || args.max_cost_usd <= 0.0 {
		bail!("--max-cost-usd must be positive");
	}
	let project = Project::load_json(&args.project)
		.with_context(|| format!("opening {}", args.project.display()))?;
	let cwd = std::env::current_dir().unwrap_or_default();
	let effort = match args.reasoning_effort.to_ascii_lowercase().as_str() {
		"none" | "off" | "" => None,
		e @ ("minimal" | "low" | "medium" | "high" | "xhigh") => Some(e.to_owned()),
		other => bail!("unknown --reasoning-effort '{other}' (low, medium, high, none)"),
	};
	let provider = match args.provider {
		ChatProvider::Openrouter => {
			let mut c =
				OpenRouterConfig::new(&args.model, resolve_secret(OPENROUTER_KEY_ENV, &cwd));
			c.reasoning_effort = effort;
			ProviderConfig::OpenRouter(c)
		}
		ChatProvider::OpenaiCompatible => {
			let Some(url) = &args.base_url else {
				bail!("--base-url is required with --provider openai-compatible");
			};
			let mut c = OpenRouterConfig::openai_compatible(
				url,
				&args.model,
				resolve_secret(OPENAI_KEY_ENV, &cwd),
			);
			c.reasoning_effort = effort;
			ProviderConfig::OpenAiCompatible(c)
		}
	};
	let stamp = std::time::SystemTime::now()
		.duration_since(std::time::UNIX_EPOCH)
		.map(|d| d.as_secs())
		.unwrap_or_default();
	let state_dir = std::env::var_os("XDG_STATE_HOME")
		.map(PathBuf::from)
		.filter(|p| p.is_absolute())
		.or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))
		.unwrap_or_else(std::env::temp_dir);
	let transcript = state_dir
		.join("cadenza/sessions")
		.join(format!("cli-{stamp}-{}", std::process::id()))
		.join("transcript.jsonl");
	let mut config = SessionConfig::new(provider, &transcript);
	config.max_cost_usd = args.max_cost_usd;
	config.allow_plugins = args.plugins;

	let mut session = Session::new();
	session.reset(Document::new(project), Some(args.project.clone()));
	let shared = session.into_shared();
	let mut agent =
		SessionAgent::new(config, shared.clone()).context("setting up the AI session")?;
	let files_root = args
		.project
		.canonicalize()
		.ok()
		.and_then(|p| p.parent().map(|d| d.to_path_buf()))
		.unwrap_or(cwd);
	let context = AppContext {
		project_path: Some(args.project.display().to_string()),
		files_root: files_root.display().to_string(),
		..AppContext::default()
	};
	eprintln!(
		"asking {} via {} (budget ${:.2})",
		agent.provider().model(),
		agent.provider().name(),
		args.max_cost_usd
	);
	let runtime = tokio::runtime::Builder::new_multi_thread()
		.enable_all()
		.build()
		.context("building the async runtime")?;
	let cancel = CancelToken::new();
	let stopper = cancel.clone();
	let observer = Printer::default();
	let outcome = runtime.block_on(async {
		tokio::select! {
			r = agent.run_request(&args.request, &context, &observer, &cancel) => r,
			_ = async {
				let _ = tokio::signal::ctrl_c().await;
				stopper.cancel();
				std::future::pending::<()>().await
			} => unreachable!(),
		}
	})?;
	eprintln!();
	if let Some(err) = &outcome.error {
		eprintln!("{}", actionable_error(err));
	}
	let summary = {
		let session = shared.lock().unwrap_or_else(|p| p.into_inner());
		let p = session.document().project();
		format!(
			"{} tracks, {} clips, {} markers",
			p.tracks.len(),
			p.tracks.iter().map(|t| t.clips.len()).sum::<usize>(),
			p.markers.len()
		)
	};
	eprintln!(
		"{:?} after {} turns; ${:.4}{} ({} requests, {} in / {} out tokens, {} cache-read); {}",
		outcome.status,
		outcome.turns,
		outcome.budget.spent_usd,
		if outcome.budget.estimated_usd > 0.0 {
			" (partly estimated)"
		} else {
			""
		},
		outcome.budget.requests,
		outcome.budget.input_tokens,
		outcome.budget.output_tokens,
		outcome.budget.cache_read_tokens,
		summary
	);
	eprintln!("transcript: {}", agent.transcript_path().display());
	if outcome.edited && !args.no_save {
		let path = args.out.clone().unwrap_or_else(|| args.project.clone());
		shared
			.lock()
			.unwrap_or_else(|p| p.into_inner())
			.document()
			.project()
			.save_json(&path)
			.with_context(|| format!("saving {}", path.display()))?;
		eprintln!("saved:      {}", path.display());
	} else if !outcome.edited {
		eprintln!("the project was not changed");
	}
	if let Some(text) = &outcome.final_message {
		println!("{text}");
	}
	match outcome.status {
		RequestStatus::Completed | RequestStatus::Cancelled => Ok(()),
		RequestStatus::Failed(why) => bail!("the request failed: {why}"),
		other => {
			eprintln!("stopped: {other:?}");
			Ok(())
		}
	}
}
