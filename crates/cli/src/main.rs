use std::path::PathBuf;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use cadenza_engine::{Player, RenderOptions, analyze_mix, render_to_wav};
use cadenza_model::Project;
use cadenza_plugin_host::{PluginCatalog, default_cache_path};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "cadenza", version, about = "Cadenza DAW command line")]
struct Cli {
	#[command(subcommand)]
	command: Command,
}

#[derive(Subcommand)]
enum Command {
	/// Render a project to a 32-bit float WAV file and print the mix report
	Render {
		project: PathBuf,
		#[arg(short, long)]
		output: PathBuf,
		#[arg(long, default_value_t = 48_000)]
		sample_rate: u32,
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
	match Cli::parse().command {
		Command::Render {
			project,
			output,
			sample_rate,
		} => {
			let project = load(&project)?;
			let opts = RenderOptions {
				sample_rate,
				plugins: plugins_for(&project),
				..RenderOptions::default()
			};
			let report = render_to_wav(&project, &opts, &output)?;
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
				..RenderOptions::default()
			};
			print_json(&analyze_mix(&project, &opts)?)
		}
		Command::Play { project } => {
			let project = load(&project)?;
			let mut player = Player::new()?;
			player.set_plugins(plugins_for(&project));
			for warning in player.load(&project)? {
				eprintln!("warning: {warning}");
			}
			let end = project.end_tick();
			player.play()?;
			while player.is_playing() && player.position() < end {
				eprint!("\r{:>8.2} beats", player.position().beats());
				thread::sleep(Duration::from_millis(100));
			}
			// let reverb and delay tails ring out
			thread::sleep(Duration::from_secs(2));
			player.stop()?;
			eprintln!();
			Ok(())
		}
		Command::Mcp => run_mcp(),
		Command::ScanPlugins { paths, json } => scan_plugins(paths, json),
		Command::ListPlugins { query, json } => list_plugins(query, json),
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
