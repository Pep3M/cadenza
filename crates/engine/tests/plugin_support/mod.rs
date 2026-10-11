//! Builds the `cadenza-test-plugins` CLAP binary and the plugin scanner, then scans the
//! test plugins into a temporary catalog shared by every test in the process.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

use cadenza_engine::PluginCatalog;

pub const GAIN_ID: &str = "dev.cadenza.test.gain";
pub const SINE_ID: &str = "dev.cadenza.test.sine";

fn workspace_manifest() -> PathBuf {
	Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.toml")
}

/// Runs `cargo build` with `args` and returns the artifact file names it reported.
fn cargo_build(args: &[&str]) -> Vec<PathBuf> {
	let out = Command::new(env!("CARGO"))
		.arg("build")
		.args(args)
		.arg("--message-format=json")
		.arg("--manifest-path")
		.arg(workspace_manifest())
		.output()
		.expect("run cargo build");
	assert!(
		out.status.success(),
		"cargo build {args:?} failed:\n{}",
		String::from_utf8_lossy(&out.stderr)
	);
	String::from_utf8_lossy(&out.stdout)
		.lines()
		.filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
		.filter(|m| m["reason"] == "compiler-artifact")
		.flat_map(|m| {
			let mut files: Vec<PathBuf> = m["filenames"]
				.as_array()
				.cloned()
				.unwrap_or_default()
				.into_iter()
				.filter_map(|f| f.as_str().map(PathBuf::from))
				.collect();
			if let Some(exe) = m["executable"].as_str() {
				files.push(PathBuf::from(exe));
			}
			files
		})
		.collect()
}

fn build() -> (PathBuf, PathBuf) {
	let library = cargo_build(&["-p", "cadenza-test-plugins"])
		.into_iter()
		.find(|f| {
			let ext = f.extension().and_then(|e| e.to_str()).unwrap_or("");
			matches!(ext, "so" | "dll" | "dylib")
		})
		.expect("cargo reported no cdylib for cadenza-test-plugins");
	let scanner = cargo_build(&[
		"-p",
		"cadenza-plugin-host",
		"--bin",
		"cadenza-plugin-scanner",
	])
	.into_iter()
	.find(|f| f.file_stem().is_some_and(|s| s == "cadenza-plugin-scanner"))
	.expect("cargo reported no cadenza-plugin-scanner executable");
	(library, scanner)
}

/// Built artifacts: (test plugin library, scanner executable), built once per process.
fn artifacts() -> &'static (PathBuf, PathBuf) {
	static BUILT: OnceLock<(PathBuf, PathBuf)> = OnceLock::new();
	BUILT.get_or_init(build)
}

/// The scanner executable.
pub fn scanner() -> PathBuf {
	artifacts().1.clone()
}

/// A fresh directory (under `CARGO_TARGET_TMPDIR`) holding `cadenza-test.clap`.
pub fn plugin_dir(name: &str) -> PathBuf {
	static COUNTER: AtomicUsize = AtomicUsize::new(0);
	// Unique per call: a loaded plugin binary is never overwritten.
	let n = COUNTER.fetch_add(1, Ordering::Relaxed);
	let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
		.join(format!("{name}-{}-{n}", std::process::id()))
		.join("plugins");
	std::fs::create_dir_all(&dir).unwrap();
	std::fs::copy(&artifacts().0, dir.join("cadenza-test.clap")).unwrap();
	dir
}

/// Scans a fresh copy of the test plugins into a catalog cached at `cache`.
pub fn scan_into(cache: &Path) -> PluginCatalog {
	let plugins = plugin_dir("test-plugins");
	let mut catalog = PluginCatalog::load_or_default(cache);
	let report = catalog
		.scan(&[plugins], &scanner())
		.expect("scan test plugins");
	assert!(report.blocked.is_empty(), "{report:?}");
	assert_eq!(report.added.len(), 2, "{report:?}");
	catalog.save().unwrap();
	catalog
}

/// A catalog with both test plugins, scanned out-of-process once per test binary.
pub fn catalog() -> Arc<PluginCatalog> {
	static CATALOG: OnceLock<Arc<PluginCatalog>> = OnceLock::new();
	CATALOG
		.get_or_init(|| {
			let cache = Path::new(env!("CARGO_TARGET_TMPDIR"))
				.join(format!("plugin-catalog-{}.json", std::process::id()));
			Arc::new(scan_into(&cache))
		})
		.clone()
}

/// Path of the test plugin binary inside the catalog.
pub fn plugin_path(id: &str) -> PathBuf {
	catalog().find(id).expect("scanned").path.clone()
}
