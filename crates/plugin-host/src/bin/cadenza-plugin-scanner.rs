//! Out-of-process plugin scanner.
//!
//! Usage: `cadenza-plugin-scanner <plugin-file> [--output <json-file>]`
//!
//! Loads one plugin file, describes every plugin it contains and writes a JSON
//! array of `PluginDescriptor`s to `--output` (or stdout). Exits non-zero on
//! failure. Hosts run it as a subprocess so a crashing or hanging plugin cannot
//! take them down.

use std::path::PathBuf;
use std::process::ExitCode;

use cadenza_plugin_host::PluginFormat;
use cadenza_plugin_host::discovery::plugin_format;

fn main() -> ExitCode {
	let mut args = std::env::args_os().skip(1);
	let mut plugin = None;
	let mut output = None;
	while let Some(arg) = args.next() {
		if arg == "--output" {
			output = args.next().map(PathBuf::from);
		} else if plugin.is_none() {
			plugin = Some(PathBuf::from(arg));
		}
	}
	let Some(plugin) = plugin else {
		eprintln!("usage: cadenza-plugin-scanner <plugin-file> [--output <json-file>]");
		return ExitCode::from(2);
	};

	let result = match plugin_format(&plugin) {
		Some(PluginFormat::Clap) => cadenza_plugin_host::clap::scan_file(&plugin),
		Some(PluginFormat::Vst3) => {
			Err(cadenza_plugin_host::PluginError::UnsupportedFormat("VST3"))
		}
		None => cadenza_plugin_host::clap::scan_file(&plugin),
	};
	let descriptors = match result {
		Ok(d) => d,
		Err(e) => {
			eprintln!("{e}");
			return ExitCode::from(1);
		}
	};
	let json = match serde_json::to_string(&descriptors) {
		Ok(j) => j,
		Err(e) => {
			eprintln!("cannot serialize scan result: {e}");
			return ExitCode::from(1);
		}
	};
	let written = match output {
		Some(path) => std::fs::write(&path, json),
		None => {
			use std::io::Write;
			let mut out = std::io::stdout().lock();
			writeln!(out, "{json}").and_then(|()| out.flush())
		}
	};
	if let Err(e) = written {
		eprintln!("cannot write scan result: {e}");
		return ExitCode::from(1);
	}
	// Skip destructors of loaded plugins: some crash or hang while unloading,
	// which would turn a successful scan into a reported failure.
	std::process::exit(0)
}
