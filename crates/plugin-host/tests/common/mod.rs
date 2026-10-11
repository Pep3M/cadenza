//! Shared helpers: builds `cadenza-test-plugins` once and exposes it as a
//! `.clap` file, plus the scanner path.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::Duration;

use cadenza_plugin_host::PluginDescriptor;
use cadenza_plugin_host::scanner::scan_in_subprocess;

pub const GAIN_ID: &str = "dev.cadenza.test.gain";
pub const SINE_ID: &str = "dev.cadenza.test.sine";

pub fn scanner() -> PathBuf {
	PathBuf::from(env!("CARGO_BIN_EXE_cadenza-plugin-scanner"))
}

fn build_test_plugins() -> PathBuf {
	let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.toml");
	let out = Command::new(env!("CARGO"))
		.args([
			"build",
			"-p",
			"cadenza-test-plugins",
			"--message-format=json",
		])
		.arg("--manifest-path")
		.arg(&manifest)
		.output()
		.expect("run cargo build for cadenza-test-plugins");
	assert!(
		out.status.success(),
		"building cadenza-test-plugins failed:\n{}",
		String::from_utf8_lossy(&out.stderr)
	);
	let library = String::from_utf8_lossy(&out.stdout)
		.lines()
		.filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
		.filter(|m| {
			m["reason"] == "compiler-artifact" && m["target"]["name"] == "cadenza_test_plugins"
		})
		.flat_map(|m| {
			m["filenames"]
				.as_array()
				.cloned()
				.unwrap_or_default()
				.into_iter()
				.filter_map(|f| f.as_str().map(PathBuf::from))
		})
		.find(|f| {
			let ext = f.extension().and_then(|e| e.to_str()).unwrap_or("");
			matches!(ext, "so" | "dll" | "dylib")
		})
		.expect("cargo reported no cdylib for cadenza-test-plugins");
	// One directory per test process: a loaded plugin binary is never
	// overwritten (Windows forbids it, and it is unsafe elsewhere).
	let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
		.join(format!("cadenza-test-plugins-{}", std::process::id()));
	std::fs::create_dir_all(&dir).unwrap();
	let dest = dir.join("cadenza-test.clap");
	std::fs::copy(&library, &dest).unwrap();
	dest
}

/// Path to the built test plugin bundle (`cadenza-test.clap`).
pub fn test_plugin() -> &'static Path {
	static PATH: OnceLock<PathBuf> = OnceLock::new();
	PATH.get_or_init(build_test_plugins)
}

/// Copies the test plugin into `dir` as `cadenza-test.clap`.
pub fn install_into(dir: &Path) -> PathBuf {
	let dest = dir.join("cadenza-test.clap");
	std::fs::copy(test_plugin(), &dest).unwrap();
	dest
}

/// Descriptors of both test plugins, scanned out-of-process once.
pub fn descriptors() -> &'static [PluginDescriptor] {
	static DESCS: OnceLock<Vec<PluginDescriptor>> = OnceLock::new();
	DESCS.get_or_init(|| {
		scan_in_subprocess(&scanner(), test_plugin(), Duration::from_secs(20))
			.expect("scan test plugins")
	})
}

pub fn descriptor(id: &str) -> &'static PluginDescriptor {
	descriptors()
		.iter()
		.find(|d| d.id == id)
		.unwrap_or_else(|| panic!("{id} not scanned"))
}
