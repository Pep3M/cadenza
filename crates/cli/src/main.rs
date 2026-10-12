use std::path::PathBuf;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use cadenza_engine::{
	Player, PluginHosting, RenderOptions, analyze_mix, export_options_for_path, find_midi_input,
	midi_input_ports, render_to_file,
};
use cadenza_model::{Device, Project, Track, TrackKind};
use cadenza_plugin_host::{PluginCatalog, default_cache_path};
use clap::{Parser, Subcommand};

mod chat;
mod compose;
mod midi_files;
mod portable;

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
	/// Play a project on the default audio device until it ends.
	///
	/// With --midi-in and --monitor, a MIDI keyboard plays the instrument of a track along
	/// with the song (see `cadenza midi-ports` for port names).
	Play {
		project: PathBuf,
		/// MIDI input port: index or part of its name (see `cadenza midi-ports`)
		#[arg(long, requires = "monitor")]
		midi_in: Option<String>,
		/// Instrument track (name or id) that plays the MIDI input
		#[arg(long, requires = "midi_in")]
		monitor: Option<String>,
	},
	/// Play a built-in instrument live from a MIDI input, without a project (Ctrl-C to quit)
	Monitor {
		/// MIDI input port: index or part of its name (see `cadenza midi-ports`)
		#[arg(long)]
		midi_in: String,
		/// Built-in instrument id (e.g. poly_synth, electric_piano, drum_machine)
		#[arg(long, default_value = "poly_synth")]
		instrument: String,
	},
	/// List MIDI input ports
	MidiPorts,
	/// Import a standard MIDI file into a new project, or into an existing one with --into
	ImportMidi {
		/// The .mid file
		file: PathBuf,
		/// Project file to write (default with --into: that project, updated in place)
		#[arg(short, long, required_unless_present = "into")]
		output: Option<PathBuf>,
		/// One track per MIDI channel instead of per file track
		#[arg(long)]
		split_by_channel: bool,
		/// Existing project to import into (tempo map, markers and tracks are added to it)
		#[arg(long)]
		into: Option<PathBuf>,
		/// With --into: put all notes in one clip on this instrument track (name or id)
		#[arg(long, requires = "into")]
		track: Option<String>,
		/// With --track: clip start in beats
		#[arg(long, default_value_t = 0.0, requires = "track")]
		at_beats: f64,
	},
	/// Export a project's MIDI (all instrument tracks, or one) to a standard MIDI file
	ExportMidi {
		project: PathBuf,
		#[arg(short, long)]
		output: PathBuf,
		/// Only this instrument track (name or id)
		#[arg(long)]
		track: Option<String>,
	},
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
	/// Copy every external file a project uses (audio clips, samples) into its folder
	/// (Audio/, Samples/), once per content, rewrite the references and save the project
	Collect {
		project: PathBuf,
		/// Also copy factory samples (normally left in the shared factory library)
		#[arg(long)]
		include_factory: bool,
		/// Only print what would be copied
		#[arg(long)]
		dry_run: bool,
	},
	/// Find a project's missing files in a folder (recursively, by original name), relink
	/// them and save the project
	Relink {
		project: PathBuf,
		/// Folder to search
		#[arg(long)]
		search: PathBuf,
		#[arg(long)]
		dry_run: bool,
	},
	/// Pack a project folder into one .cadenza.zip archive for sharing
	Pack {
		project: PathBuf,
		/// Archive to write (default: <project>.cadenza.zip next to the project file)
		#[arg(short, long)]
		output: Option<PathBuf>,
		/// Include the Renders/ folder
		#[arg(long)]
		include_renders: bool,
	},
	/// Extract a project archive into a folder and print the project file's path
	Unpack { archive: PathBuf, dest: PathBuf },
	/// Run the MCP server over stdio, for an AI client to connect to
	Mcp,
	/// Open the desktop app, optionally with a project.
	///
	/// Starts the `cadenza-app` executable found next to this one (`cargo build --release`
	/// builds both into target/release), on the PATH, or named by CADENZA_APP. It is a
	/// separate binary so this CLI stays free of GPU and windowing dependencies.
	Gui {
		/// Project to open (.cadenza.json)
		project: Option<PathBuf>,
	},
	/// Let the embedded AI producer compose, arrange, mix and render a song from a brief
	#[command(after_long_help = compose::presets_help())]
	Compose(compose::ComposeArgs),
	/// Ask the AI producer for one change to an existing project (the app chat's mode):
	/// "add a bridge after the second chorus", "mix this"... Saves the result in place
	/// unless --out or --no-save
	#[command(after_long_help = compose::presets_help())]
	Chat(chat::ChatArgs),
}

/// Finds a track by id or (case-insensitive) name.
fn find_track<'a>(project: &'a Project, query: &str) -> Result<&'a Track> {
	let q = query.trim();
	project
		.tracks
		.iter()
		.find(|t| t.id.to_string() == q)
		.or_else(|| {
			project
				.tracks
				.iter()
				.find(|t| t.name.eq_ignore_ascii_case(q))
		})
		.with_context(|| {
			let names: Vec<&str> = project.tracks.iter().map(|t| t.name.as_str()).collect();
			format!("no track named '{q}'; tracks: {}", names.join(", "))
		})
}

/// Plays a built-in instrument from a MIDI input until interrupted.
fn monitor(port: &str, instrument: &str) -> Result<()> {
	let known = cadenza_dsp::registry()
		.iter()
		.any(|d| d.id == instrument && d.role == cadenza_dsp::DeviceRole::Instrument);
	if !known {
		let ids: Vec<&str> = cadenza_dsp::registry()
			.iter()
			.filter(|d| d.role == cadenza_dsp::DeviceRole::Instrument)
			.map(|d| d.id)
			.collect();
		bail!(
			"'{instrument}' is not a built-in instrument; use one of: {}",
			ids.join(", ")
		);
	}
	let port = find_midi_input(port)?;
	let mut project = Project::new("Monitor");
	let track = Track::new(instrument, TrackKind::Instrument)
		.with_instrument(Device::builtin(instrument, instrument));
	let id = track.id;
	project.tracks.push(track);
	let mut player = Player::new()?;
	player.load(&project)?;
	player.set_midi_input(Some(&port))?;
	player.set_monitor_track(Some(id))?;
	player.play()?;
	eprintln!("playing {instrument} from '{}' (Ctrl-C to quit)", port.name);
	let mut dropped = 0;
	loop {
		thread::sleep(Duration::from_millis(250));
		let now = player.dropped_midi_events();
		if now > dropped {
			eprintln!(
				"warning: {} MIDI events dropped (audio thread too slow)",
				now - dropped
			);
			dropped = now;
		}
	}
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
		Command::Play {
			project,
			midi_in,
			monitor,
		} => {
			let project = load(&project)?;
			let mut player = Player::new()?;
			player.set_plugins(plugins_for(&project));
			player.set_hosting(hosting);
			for warning in player.load(&project)? {
				eprintln!("warning: {warning}");
			}
			if let (Some(port), Some(track)) = (midi_in, monitor) {
				let track = find_track(&project, &track)?;
				if track.kind != TrackKind::Instrument {
					bail!(
						"track '{}' is not an instrument track; --monitor needs one",
						track.name
					);
				}
				let port = find_midi_input(&port)?;
				player.set_midi_input(Some(&port))?;
				player.set_monitor_track(Some(track.id))?;
				eprintln!("monitoring '{}' on track '{}'", port.name, track.name);
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
		Command::Gui { project } => run_gui(project, cli.in_process_plugins),
		Command::Compose(args) => {
			set_in_process_env(cli.in_process_plugins);
			compose::run(args)
		}
		Command::Chat(args) => {
			set_in_process_env(cli.in_process_plugins);
			chat::run(args)
		}
		Command::ScanPlugins { paths, json } => scan_plugins(paths, json),
		Command::Monitor {
			midi_in,
			instrument,
		} => monitor(&midi_in, &instrument),
		Command::MidiPorts => {
			let ports = midi_input_ports()?;
			if ports.is_empty() {
				eprintln!("no MIDI input ports found; connect a device or start a virtual port");
			}
			for p in ports {
				println!("{}: {}", p.index, p.name);
			}
			Ok(())
		}
		Command::ImportMidi {
			file,
			output,
			split_by_channel,
			into,
			track,
			at_beats,
		} => midi_files::import(&file, output, split_by_channel, into, track, at_beats),
		Command::ExportMidi {
			project,
			output,
			track,
		} => midi_files::export(&project, &output, track),
		Command::ListPlugins { query, json } => list_plugins(query, json),
		Command::Collect {
			project,
			include_factory,
			dry_run,
		} => portable::collect(&project, include_factory, dry_run),
		Command::Relink {
			project,
			search,
			dry_run,
		} => portable::relink(&project, &search, dry_run),
		Command::Pack {
			project,
			output,
			include_renders,
		} => portable::pack(&project, output, include_renders),
		Command::Unpack { archive, dest } => portable::unpack(&archive, &dest),
	}
}

/// The MCP server and the agent read their hosting default from the environment.
fn set_in_process_env(in_process: bool) {
	if in_process {
		// SAFETY: called on the main thread before any other thread is spawned.
		unsafe { std::env::set_var(cadenza_engine::HOSTING_ENV, "in-process") };
	}
}

/// Runs the `cadenza-app` desktop executable and waits for it to close.
fn run_gui(project: Option<PathBuf>, in_process: bool) -> Result<()> {
	let name = format!("cadenza-app{}", std::env::consts::EXE_SUFFIX);
	let exe = std::env::var_os("CADENZA_APP")
		.map(PathBuf::from)
		.or_else(|| {
			std::env::current_exe()
				.ok()
				.and_then(|e| e.parent().map(|d| d.join(&name)))
				.filter(|p| p.is_file())
		})
		.unwrap_or_else(|| PathBuf::from(&name));
	let mut cmd = std::process::Command::new(&exe);
	if let Some(p) = project {
		cmd.arg(p);
	}
	if in_process {
		cmd.env(cadenza_engine::HOSTING_ENV, "in-process");
	}
	let status = cmd.status().with_context(|| {
		format!(
			"cannot start {}; build it with `cargo build --release -p cadenza-app` or set CADENZA_APP",
			exe.display()
		)
	})?;
	if !status.success() {
		bail!("cadenza-app exited with {status}");
	}
	Ok(())
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
