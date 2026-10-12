//! Builds the `cadenza-test-plugins` CLAP binary, the plugin scanner and the plugin
//! sandbox, then scans the test plugins into a temporary catalog shared by every test in
//! the process. The crash/hang test effect (`clap_crash` example) gets its own catalog.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

use cadenza_engine::{PluginCatalog, PluginHosting};

pub const GAIN_ID: &str = "dev.cadenza.test.gain";
pub const SINE_ID: &str = "dev.cadenza.test.sine";
pub const CRASH_ID: &str = "dev.cadenza.test.crash";

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

fn find_exe(files: Vec<PathBuf>, name: &str) -> PathBuf {
	files
		.into_iter()
		.find(|f| f.file_stem().is_some_and(|s| s == name))
		.unwrap_or_else(|| panic!("cargo reported no {name} executable"))
}

/// The `cadenza-plugin-sandbox` executable, built once per process.
pub fn sandbox_exe() -> PathBuf {
	static EXE: OnceLock<PathBuf> = OnceLock::new();
	EXE.get_or_init(|| {
		find_exe(
			cargo_build(&[
				"-p",
				"cadenza-plugin-host",
				"--bin",
				"cadenza-plugin-sandbox",
			]),
			"cadenza-plugin-sandbox",
		)
	})
	.clone()
}

/// Sandboxed hosting with this build's sandbox executable, no core dumps (the crash
/// tests abort on purpose) and a realtime deadline loose enough for loaded CI machines.
pub fn sandboxed() -> PluginHosting {
	let mut hosting = PluginHosting::sandboxed();
	hosting.sandbox.executable = Some(sandbox_exe());
	hosting.sandbox.core_dumps = false;
	hosting.sandbox.realtime_timeout = std::time::Duration::from_millis(500);
	hosting
}

/// A catalog holding only the plugin of `cadenza-test-plugins` example `example`,
/// installed as `file`.
fn example_catalog(example: &str, file: &str) -> Arc<PluginCatalog> {
	let library = cargo_build(&["-p", "cadenza-test-plugins", "--example", example])
		.into_iter()
		.find(|f| {
			let ext = f.extension().and_then(|e| e.to_str()).unwrap_or("");
			let stem = f.file_stem().map(|s| s.to_string_lossy().into_owned());
			matches!(ext, "so" | "dll" | "dylib") && stem.is_some_and(|s| s.contains(example))
		})
		.unwrap_or_else(|| panic!("cargo reported no cdylib for {example}"));
	let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
		.join(format!("{example}-plugin-{}", std::process::id()))
		.join("plugins");
	std::fs::create_dir_all(&dir).unwrap();
	std::fs::copy(&library, dir.join(file)).unwrap();
	let cache = dir.join("../catalog.json");
	let mut catalog = PluginCatalog::load_or_default(&cache);
	let report = catalog
		.scan(&[dir], &scanner())
		.unwrap_or_else(|e| panic!("scan {example}: {e}"));
	assert_eq!(report.added.len(), 1, "{report:?}");
	Arc::new(catalog)
}

/// A catalog holding only the crash/hang test effect (`dev.cadenza.test.crash`).
pub fn crash_catalog() -> Arc<PluginCatalog> {
	static CATALOG: OnceLock<Arc<PluginCatalog>> = OnceLock::new();
	CATALOG
		.get_or_init(|| example_catalog("clap_crash", "cadenza-crash.clap"))
		.clone()
}

pub const GUI_ID: &str = "dev.cadenza.test.gui";

/// A catalog holding only the scripted-editor test effect (`dev.cadenza.test.gui`).
pub fn gui_catalog() -> Arc<PluginCatalog> {
	static CATALOG: OnceLock<Arc<PluginCatalog>> = OnceLock::new();
	CATALOG
		.get_or_init(|| example_catalog("clap_gui", "cadenza-gui.clap"))
		.clone()
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

pub const DUCKER_ID: &str = "dev.cadenza.test.ducker";
pub const MULTI_ID: &str = "dev.cadenza.test.multi";

/// A catalog holding the sidechain/multi-output test plugins (`clap_routing` example)
/// plus the regular test plugins (gain, sine).
pub fn routing_catalog() -> Arc<PluginCatalog> {
	static CATALOG: OnceLock<Arc<PluginCatalog>> = OnceLock::new();
	CATALOG
		.get_or_init(|| {
			let library = cargo_build(&["-p", "cadenza-test-plugins", "--example", "clap_routing"])
				.into_iter()
				.find(|f| {
					let ext = f.extension().and_then(|e| e.to_str()).unwrap_or("");
					let stem = f.file_stem().map(|s| s.to_string_lossy().into_owned());
					matches!(ext, "so" | "dll" | "dylib")
						&& stem.is_some_and(|s| s.contains("clap_routing"))
				})
				.expect("cargo reported no cdylib for clap_routing");
			let dir = plugin_dir("routing-plugins");
			std::fs::copy(&library, dir.join("cadenza-routing.clap")).unwrap();
			let cache = dir.join("../catalog.json");
			let mut catalog = PluginCatalog::load_or_default(&cache);
			let report = catalog
				.scan(&[dir], &scanner())
				.expect("scan routing plugins");
			assert_eq!(report.added.len(), 4, "{report:?}");
			Arc::new(catalog)
		})
		.clone()
}

/// A CLAP device for `id` from [`routing_catalog`].
pub fn routing_device(id: &str, name: &str) -> cadenza_model::Device {
	let mut d = cadenza_model::Device::builtin("unused", name);
	d.kind = cadenza_model::DeviceKind::Clap {
		plugin_id: id.into(),
		path: routing_catalog().find(id).expect("scanned").path.clone(),
	};
	d
}
