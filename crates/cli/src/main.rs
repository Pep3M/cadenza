use std::path::PathBuf;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use cadenza_engine::{
	Player, PluginHosting, RenderOptions, analyze_mix, export_options_for_path, render_to_file,
};
use cadenza_model::Project;
use cadenza_plugin_host::{PluginCatalog, default_cache_path};
use clap::{Parser, Subcommand};

mod compose;

const PLUGIN_HOSTING_HELP: &str = "\
Plugin sandbox:
  Third-party plugins (CLAP/VST3) used by render, analyze, play, mcp and compose run each in
  its own `cadenza-plugin-sandbox` helper process, so a plugin that crashes or hangs is
  bypassed and reported (render/analyze: in the report's warnings; play: on stderr) instead
  of taking Cadenza down. The helper must sit next to the `cadenza` executable
  (`cargo build --release` builds both into target/release) or be named by the
  CADENZA_PLUGIN_SANDBOX variable; if it is missing, plugins run in-process with a warning.
  Use --in-process-plugins or CADENZA_PLUGIN_HOSTING=in-process to load plugins into the
  cadenza process itself (debugging, benchmarks).";

#[derive(Parser)]
#[command(
	name = "cadenza",
	version,
	about = "Cadenza DAW command line",
	after_long_help = PLUGIN_HOSTING_HELP
)]
struct Cli {
	/// Load third-party plugins into this process instead of sandbox processes (faster
	/// startup, but a crashing plugin aborts cadenza; same as CADENZA_PLUGIN_HOSTING=in-process)
	#[arg(long, global = true)]
	in_process_plugins: bool,
	#[command(subcommand)]
	command: Command,
}

#[derive(Subcommand)]
enum Command {
	/// Render a project to an audio file and print the mix report.
	///
	/// The format follows the output extension: .wav (32-bit float by default, or
	/// --bit-depth 16/24), .flac (24-bit by default, or 16) or .mp3 (only in builds with the
	/// `mp3` feature; --bitrate in kbps). 16/24-bit output gets TPDF dither.
	///
	/// Plugins run sandboxed (see `cadenza --help`): a crashing plugin is bypassed and named
	/// in the report's warnings, and the render still completes.
	Render {
		project: PathBuf,
		#[arg(short, long)]
		output: PathBuf,
		#[arg(long, default_value_t = 48_000)]
		sample_rate: u32,
		/// WAV: 16, 24 or 32 (float); FLAC: 16 or 24
		#[arg(long)]
		bit_depth: Option<u32>,
		/// MP3 bitrate in kbps (32..=320, default 320)
		#[arg(long)]
		bitrate: Option<u16>,
	},
	/// Analyze a project's mix (loudness, peaks, band energy) without writing audio
	Analyze {
		project: PathBuf,
		#[arg(long, default_value_t = 48_000)]
		sample_rate: u32,
	},
	/// Play a project on the default audio device until it ends
	Play { project: PathBuf },
	/// Scan installed plugins (CLAP and VST3) into the plugin catalog and print the report.
	///
	/// Each new or changed plugin file is loaded by the `cadenza-plugin-scanner` helper in a
	/// separate process, so a crashing plugin is blocklisted instead of taking Cadenza down.
	/// The helper must sit next to the `cadenza` executable (`cargo build --release` builds
	/// both into target/release) or be named by the CADENZA_PLUGIN_SCANNER variable.
	ScanPlugins {
		/// Directories or plugin files to scan (default: the standard locations of this OS)
		paths: Vec<PathBuf>,
		/// Print the raw scan report as JSON
		#[arg(long)]
		json: bool,
	},
	/// List plugins in the catalog (run scan-plugins first)
	ListPlugins {
		/// Case-insensitive filter on name, vendor, id and category tags
		#[arg(short, long)]
		query: Option<String>,
		/// Print full descriptors (including parameters) as JSON
		#[arg(long)]
		json: bool,
	},
	/// Run the MCP server over stdio, for an AI client to connect to
	Mcp,
	/// Let the embedded AI producer compose, arrange, mix and render a song from a brief
	#[command(after_long_help = compose::presets_help())]
	Compose(compose::ComposeArgs),
}

fn load(path: &PathBuf) -> Result<Project> {
	Project::load_json(path).with_context(|| format!("loading project {}", path.display()))
}

/// The plugin catalog cache, when the project uses plugin devices.
fn plugins_for(project: &Project) -> Option<Arc<PluginCatalog>> {
	cadenza_mcp::session::uses_plugins(project)
		.then(|| Arc::new(PluginCatalog::load_or_default(default_cache_path())))
}

fn print_json(value: &impl serde::Serialize) -> Result<()> {
	println!("{}", serde_json::to_string_pretty(value)?);
	Ok(())
}

fn main() -> Result<()> {
	let cli = Cli::parse();
	let hosting = if cli.in_process_plugins {
		PluginHosting::in_process()
	} else {
		PluginHosting::default()
	};
	match cli.command {
		Command::Render {
			project,
			output,
			sample_rate,
			bit_depth,
			bitrate,
		} => {
			let export = export_options_for_path(&output, bit_depth, bitrate)?;
			let project = load(&project)?;
			let opts = RenderOptions {
				sample_rate,
				plugins: plugins_for(&project),
				hosting,
				..RenderOptions::default()
			};
			let report = render_to_file(&project, &opts, &output, &export)?;
			eprintln!("wrote {}", output.display());
			print_json(&report)
		}
		Command::Analyze {
			project,
			sample_rate,
		} => {
			let project = load(&project)?;
			let opts = RenderOptions {
				sample_rate,
				plugins: plugins_for(&project),
				hosting,
				..RenderOptions::default()
			};
			print_json(&analyze_mix(&project, &opts)?)
		}
		Command::Play { project } => {
			let project = load(&project)?;
			let mut player = Player::new()?;
			player.set_plugins(plugins_for(&project));
			player.set_hosting(hosting);
			for warning in player.load(&project)? {
				eprintln!("warning: {warning}");
			}
			let end = project.end_tick();
			player.play()?;
			while player.is_playing() && player.position() < end {
				eprint!("\r{:>8.2} beats", player.position().beats());
				for crash in player.take_plugin_crashes() {
					eprintln!("\nwarning: {}", crash.message);
				}
				thread::sleep(Duration::from_millis(100));
			}
			// let reverb and delay tails ring out
			thread::sleep(Duration::from_secs(2));
			player.stop()?;
			eprintln!();
			Ok(())
		}
		Command::Mcp => {
			set_in_process_env(cli.in_process_plugins);
			run_mcp()
		}
		Command::Compose(args) => {
			set_in_process_env(cli.in_process_plugins);
			compose::run(args)
		}
		Command::ScanPlugins { paths, json } => scan_plugins(paths, json),
		Command::ListPlugins { query, json } => list_plugins(query, json),
	}
}

/// The MCP server and the agent read their hosting default from the environment.
fn set_in_process_env(in_process: bool) {
	if in_process {
		// SAFETY: called on the main thread before any other thread is spawned.
		unsafe { std::env::set_var(cadenza_engine::HOSTING_ENV, "in-process") };
	}
}

fn scan_plugins(paths: Vec<PathBuf>, json: bool) -> Result<()> {
	let roots = if paths.is_empty() {
		cadenza_plugin_host::default_search_paths()
	} else {
		paths
	};
	let scanner = cadenza_plugin_host::scanner_path()?;
	let mut catalog = PluginCatalog::load_or_default(default_cache_path());
	eprintln!(
		"scanning {} (this loads each new plugin in a helper process)...",
		roots
			.iter()
			.map(|r| r.display().to_string())
			.collect::<Vec<_>>()
			.join(", ")
	);
	let report = catalog.scan(&roots, &scanner)?;
	catalog.save()?;
	if json {
		return print_json(&report);
	}
	println!(
		"{} plugin files found, {} scanned: {} added, {} updated, {} removed",
		report.files_found,
		report.files_scanned,
		report.added.len(),
		report.updated.len(),
		report.removed.len()
	);
	println!(
		"catalog: {} plugins ({})",
		catalog.descriptors().count(),
		catalog.cache_path().display()
	);
	for b in &report.blocked {
		println!("blocked: {} — {}", b.path.display(), b.reason);
	}
	let older = catalog.blocked().count() - report.blocked.len();
	if older > 0 {
		println!("{older} file(s) still blocked from earlier scans");
	}
	Ok(())
}

fn list_plugins(query: Option<String>, json: bool) -> Result<()> {
	let catalog = PluginCatalog::load_or_default(default_cache_path());
	let terms: Vec<String> = query
		.as_deref()
		.unwrap_or("")
		.split_whitespace()
		.map(str::to_lowercase)
		.collect();
	let mut found: Vec<_> = catalog
		.descriptors()
		.filter(|d| {
			let hay =
				format!("{} {} {} {}", d.id, d.name, d.vendor, d.features.join(" ")).to_lowercase();
			terms.iter().all(|t| hay.contains(t.as_str()))
		})
		.collect();
	found.sort_by_cached_key(|d| d.name.to_lowercase());
	if json {
		return print_json(&found);
	}
	if catalog.descriptors().next().is_none() {
		eprintln!("the plugin catalog is empty; run `cadenza scan-plugins` first");
	}
	for d in &found {
		let role = match d.role {
			cadenza_plugin_host::PluginRole::Instrument => "instrument",
			cadenza_plugin_host::PluginRole::Effect => "effect",
		};
		println!(
			"{}:{}  {} ({}, {} {}, {} params)",
			d.format.as_str().to_lowercase(),
			d.id,
			d.name,
			if d.vendor.is_empty() { "?" } else { &d.vendor },
			d.format.as_str(),
			role,
			d.params.len()
		);
	}
	eprintln!("{} plugin(s)", found.len());
	Ok(())
}

/// Runs the MCP server over stdio. stdout is the JSON-RPC channel, so every log line goes
/// to stderr instead.
fn run_mcp() -> Result<()> {
	tracing_subscriber::fmt()
		.with_writer(std::io::stderr)
		.with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
		.init();
	tokio::runtime::Builder::new_multi_thread()
		.enable_all()
		.build()
		.context("building the async runtime")?
		.block_on(cadenza_mcp::serve_stdio())
		.context("running the MCP server")
}
