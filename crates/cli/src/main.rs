use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use cadenza_engine::{Player, RenderOptions, analyze_mix, render_to_wav};
use cadenza_model::Project;
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
}

fn load(path: &PathBuf) -> Result<Project> {
	Project::load_json(path).with_context(|| format!("loading project {}", path.display()))
}

fn print_json(value: &impl serde::Serialize) -> Result<()> {
	println!("{}", serde_json::to_string_pretty(value)?);
	Ok(())
}

fn main() -> Result<()> {
	match Cli::parse().command {
		Command::Render { project, output, sample_rate } => {
			let opts = RenderOptions { sample_rate, ..RenderOptions::default() };
			let report = render_to_wav(&load(&project)?, &opts, &output)?;
			eprintln!("wrote {}", output.display());
			print_json(&report)
		}
		Command::Analyze { project, sample_rate } => {
			let opts = RenderOptions { sample_rate, ..RenderOptions::default() };
			print_json(&analyze_mix(&load(&project)?, &opts)?)
		}
		Command::Play { project } => {
			let project = load(&project)?;
			let mut player = Player::new()?;
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
	}
}
