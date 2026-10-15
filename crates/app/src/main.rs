//! `cadenza-app [project.cadenza.json]`: the Cadenza desktop application.

// Release builds on Windows start without a console window (`cadenza gui` from a
// terminal still shows the CLI's output).
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
	let mut args = std::env::args_os().skip(1);
	let project = args.next().map(PathBuf::from);
	if let Some(p) = &project
		&& (p == "-h" || p == "--help")
	{
		println!("usage: cadenza-app [PROJECT.cadenza.json]");
		return ExitCode::SUCCESS;
	}
	match cadenza_app::run(project) {
		Ok(()) => ExitCode::SUCCESS,
		Err(e) => {
			eprintln!("cadenza-app: {e}");
			ExitCode::FAILURE
		}
	}
}
