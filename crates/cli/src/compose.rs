//! `cadenza compose`: the embedded AI producer turns a brief into a mixed, rendered song.

use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result, bail};
use cadenza_agent::config::{
	DEFAULT_MAX_COST_USD, DEFAULT_MAX_TURNS, DEFAULT_MODEL, DEFAULT_REASONING_EFFORT,
	MODEL_PRESETS, OPENROUTER_KEY_ENV, slugify,
};
use cadenza_agent::observer::clip;
use cadenza_agent::{Agent, AgentConfig, AgentEvent, AgentObserver, ProviderConfig, StopStatus};
use clap::{Args, ValueEnum};

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum Provider {
	/// OpenRouter (OpenAI-compatible); key from OPENROUTER_API_KEY or a .env file
	Openrouter,
}

pub fn presets_help() -> String {
	let mut help = String::from("Model presets (OpenRouter ids):\n");
	for (id, note) in MODEL_PRESETS {
		help.push_str(&format!("  {id:<32} {note}\n"));
	}
	help.push_str(
		"\nThe API key is read from the environment variable named by --api-key-env, or from \
		 the nearest .env file in the current directory or up to five parents.",
	);
	help
}

#[derive(Args, Debug)]
pub struct ComposeArgs {
	/// Creative brief, e.g. "lo-fi ballad, 3 minutes, sad, piano and strings"
	pub brief: String,
	/// Output folder: receives <slug>.cadenza.json, <slug>.wav and session.jsonl
	#[arg(short, long, default_value = ".")]
	pub out: PathBuf,
	/// Base file name (default: derived from the brief)
	#[arg(long)]
	pub slug: Option<String>,
	#[arg(long, value_enum, default_value_t = Provider::Openrouter)]
	pub provider: Provider,
	/// Model id (see the presets below)
	#[arg(long, default_value = DEFAULT_MODEL)]
	pub model: String,
	/// Reasoning effort passed to the model: low, medium, high, or none to omit it
	#[arg(long, default_value = DEFAULT_REASONING_EFFORT)]
	pub reasoning_effort: String,
	/// Spending cap in USD; the session wraps up near it and stops when it is reached
	#[arg(long, default_value_t = DEFAULT_MAX_COST_USD)]
	pub max_cost_usd: f64,
	/// Maximum number of model responses
	#[arg(long, default_value_t = DEFAULT_MAX_TURNS)]
	pub max_turns: u32,
	/// Allow installed third-party plugins (CLAP/VST3); default is built-in devices only
	#[arg(long)]
	pub plugins: bool,
	/// Genre/style hint for the arranger and the mix reference
	#[arg(long)]
	pub genre: Option<String>,
	/// Integrated loudness target in LUFS
	#[arg(long, default_value_t = -14.0, allow_hyphen_values = true)]
	pub target_lufs: f32,
	/// Name of the environment variable (or .env entry) holding the API key
	#[arg(long, default_value = OPENROUTER_KEY_ENV)]
	pub api_key_env: String,
	/// Output token cap per model response
	#[arg(long, default_value_t = 16_000)]
	pub max_output_tokens: u32,
	/// Use plain JSON responses instead of streaming
	#[arg(long)]
	pub no_stream: bool,
	/// Print the first request instead of sending it (no API key needed, no cost)
	#[arg(long)]
	pub dry_run: bool,
	/// With --dry-run: print the full request body as JSON
	#[arg(long, requires = "dry_run")]
	pub request_json: bool,
}

pub fn run(args: ComposeArgs) -> Result<()> {
	if args.max_cost_usd.is_nan() || args.max_cost_usd <= 0.0 {
		bail!("--max-cost-usd must be positive");
	}
	let slug = args.slug.clone().unwrap_or_else(|| slugify(&args.brief));
	let provider = match args.provider {
		Provider::Openrouter => {
			let mut config = ProviderConfig::openrouter_from_env(&args.model, &args.api_key_env);
			let ProviderConfig::OpenRouter(c) = &mut config;
			c.reasoning_effort = match args.reasoning_effort.to_ascii_lowercase().as_str() {
				"none" | "off" | "" => None,
				effort @ ("minimal" | "low" | "medium" | "high" | "xhigh") => {
					Some(effort.to_owned())
				}
				other => bail!("unknown --reasoning-effort '{other}' (low, medium, high, none)"),
			};
			c.max_output_tokens = args.max_output_tokens;
			c.stream = !args.no_stream;
			config
		}
	};
	let mut config = AgentConfig::new(provider, &args.out, slug);
	config.max_cost_usd = args.max_cost_usd;
	config.max_turns = args.max_turns;
	config.allow_plugins = args.plugins;
	config.genre = args.genre.clone();
	config.target_lufs = args.target_lufs;

	if args.dry_run {
		return dry_run(config, &args);
	}

	let runtime = tokio::runtime::Builder::new_multi_thread()
		.enable_all()
		.build()
		.context("building the async runtime")?;
	let mut agent = Agent::new(config).context("setting up the composition session")?;
	eprintln!(
		"composing with {} via {} (budget ${:.2}, up to {} turns) into {}",
		agent.provider().model(),
		agent.provider().name(),
		args.max_cost_usd,
		args.max_turns,
		args.out.display()
	);
	let observer = Printer::default();
	let outcome = runtime
		.block_on(agent.run(&args.brief, &observer))
		.context("composition session failed")?;

	eprintln!();
	let budget = &outcome.budget;
	eprintln!(
		"{} after {} turns; ${:.4}{} ({} requests, {} in / {} out tokens, {} cache-read)",
		match &outcome.status {
			StopStatus::Completed => "finished".to_owned(),
			StopStatus::BudgetExhausted => "stopped: budget reached".to_owned(),
			StopStatus::MaxTurns => "stopped: turn limit reached".to_owned(),
			StopStatus::Refused => "stopped: the model declined to continue".to_owned(),
			StopStatus::Failed(why) => format!("stopped: {why}"),
		},
		outcome.turns,
		budget.spent_usd,
		if budget.estimated_usd > 0.0 {
			" (partly estimated)"
		} else {
			""
		},
		budget.requests,
		budget.input_tokens,
		budget.output_tokens,
		budget.cache_read_tokens,
	);
	if outcome.finalized_by_harness {
		eprintln!("the session was saved and rendered by the harness");
	}
	match &outcome.project_path {
		Some(p) => eprintln!("project:    {}", p.display()),
		None => eprintln!("project:    not saved"),
	}
	match (&outcome.wav_path, &outcome.loudness) {
		(Some(p), Some(l)) => eprintln!("render:     {} ({l})", p.display()),
		(Some(p), None) => eprintln!("render:     {}", p.display()),
		_ => eprintln!("render:     not rendered"),
	}
	eprintln!("transcript: {}", outcome.transcript_path.display());
	if let Some(summary) = &outcome.final_message {
		println!("{summary}");
	}
	match outcome.status {
		StopStatus::Completed => Ok(()),
		_ if outcome.wav_path.is_some() => Ok(()),
		_ => bail!("the session ended without a rendered song"),
	}
}

fn dry_run(config: AgentConfig, args: &ComposeArgs) -> Result<()> {
	let scratch = std::env::temp_dir().join(format!("cadenza-dry-run-{}", std::process::id()));
	let mut preview_config = config.clone();
	preview_config.out_dir = scratch.clone();
	let agent = Agent::new(preview_config).context("setting up the dry run")?;
	let preview = agent.preview_first_request(&args.brief);
	let _ = std::fs::remove_dir_all(&scratch);
	if args.request_json {
		println!("{}", serde_json::to_string_pretty(&preview.body)?);
		return Ok(());
	}
	let body = &preview.body;
	let body_text = body.to_string();
	let tools_text = body["tools"].to_string();
	let system = agent.system_prompt();
	let user = body["messages"][1]["content"]
		.as_array()
		.and_then(|parts| parts[0]["text"].as_str())
		.or_else(|| body["messages"][1]["content"].as_str())
		.unwrap_or_default();
	println!("{} {}", preview.method, preview.url);
	for (name, value) in &preview.headers {
		println!("{name}: {value}");
	}
	let mut params = body.clone();
	if let Some(obj) = params.as_object_mut() {
		obj.remove("messages");
		obj.remove("tools");
	}
	println!("\nparameters: {params}");
	let caps = agent.provider().capabilities();
	println!(
		"prompt caching: {}",
		if caps.supports_prompt_cache {
			"explicit breakpoints on the system prompt and the latest message"
		} else {
			"left to the provider (no explicit breakpoints for this model)"
		}
	);
	println!("\n=== system prompt ({} chars) ===\n{system}", system.len());
	println!(
		"\n=== first user message ({} chars) ===\n{user}",
		user.len()
	);
	let tools = agent.tools();
	println!("\n=== tools ({}) ===", tools.len());
	for tool in tools {
		let first = tool.description.split(". ").next().unwrap_or_default();
		println!("  {:<24} {}", tool.name, clip(first, 90));
	}
	println!(
		"\nsize: request body {} bytes (tool schemas {} bytes); roughly {} input tokens at \
		 ~4 bytes/token. Output files would go to {} as {} and {}.",
		body_text.len(),
		tools_text.len(),
		body_text.len() / 4,
		config.out_dir.display(),
		config.project_file_name(),
		config.wav_file_name(),
	);
	Ok(())
}

/// Live progress on stderr.
#[derive(Default)]
struct Printer {
	lock: Mutex<()>,
	mid_line: AtomicBool,
}

impl AgentObserver for Printer {
	fn on_event(&self, event: &AgentEvent<'_>) {
		let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
		let mut err = std::io::stderr().lock();
		if self.mid_line.swap(false, Ordering::Relaxed) {
			let _ = writeln!(err);
		}
		let _ = match event {
			AgentEvent::TurnStarted { turn } => writeln!(err, "-- turn {turn}"),
			AgentEvent::Thinking(text) => writeln!(err, "   (thinking) {}", clip(text, 240)),
			AgentEvent::Text(text) => writeln!(err, "{}", text.trim()),
			AgentEvent::ToolCall { name, args } => writeln!(err, "   > {name} {}", clip(args, 140)),
			AgentEvent::ToolResult { name, ok, summary } => writeln!(
				err,
				"     {} {name}: {}",
				if *ok { "ok" } else { "error" },
				clip(summary, 160)
			),
			AgentEvent::Usage {
				usage,
				turn_cost_usd,
				total_cost_usd,
				estimated,
				..
			} => writeln!(
				err,
				"   [in {} (cached {}), out {}; ${turn_cost_usd:.4}{}, total ${total_cost_usd:.4}]",
				usage.input_tokens,
				usage.cache_read_tokens,
				usage.output_tokens,
				if *estimated { " est." } else { "" }
			),
			AgentEvent::Retry {
				attempt,
				wait_ms,
				reason,
			} => writeln!(
				err,
				"   retry {attempt} in {:.1}s: {}",
				*wait_ms as f64 / 1000.0,
				clip(reason, 160)
			),
			AgentEvent::Notice(text) => writeln!(err, "!! {text}"),
			AgentEvent::Final(_) => Ok(()),
		};
	}
}
