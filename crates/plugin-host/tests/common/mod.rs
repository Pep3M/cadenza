//! Shared helpers: builds `cadenza-test-plugins` once and exposes it as a
//! `.clap` file (and its `vst3_gain` example as a `.vst3` bundle), plus the
//! scanner path.

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

/// Builds a `cadenza-test-plugins` target and returns its shared library.
fn build_cdylib(extra: &[&str], target_name: &str) -> PathBuf {
	let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.toml");
	let out = Command::new(env!("CARGO"))
		.args([
			"build",
			"-p",
			"cadenza-test-plugins",
			"--message-format=json",
		])
		.args(extra)
		.arg("--manifest-path")
		.arg(&manifest)
		.output()
		.expect("run cargo build for cadenza-test-plugins");
	assert!(
		out.status.success(),
		"building cadenza-test-plugins failed:\n{}",
		String::from_utf8_lossy(&out.stderr)
	);
	String::from_utf8_lossy(&out.stdout)
		.lines()
		.filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
		.filter(|m| m["reason"] == "compiler-artifact" && m["target"]["name"] == target_name)
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
		.expect("cargo reported no cdylib for cadenza-test-plugins")
}

fn build_test_plugins() -> PathBuf {
	let library = build_cdylib(&[], "cadenza_test_plugins");
	// One directory per test process: a loaded plugin binary is never
	// overwritten (Windows forbids it, and it is unsafe elsewhere).
	let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
		.join(format!("cadenza-test-plugins-{}", std::process::id()));
	std::fs::create_dir_all(&dir).unwrap();
	let dest = dir.join("cadenza-test.clap");
	std::fs::copy(&library, &dest).unwrap();
	dest
}

/// Class id of the VST3 test gain's audio component (as in descriptors).
pub const VST3_GAIN_ID: &str = "43444E5A56334741494E50524F430001";

fn build_vst3_bundle() -> PathBuf {
	let library = build_cdylib(&["--example", "vst3_gain"], "vst3_gain");
	let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
		.join(format!("cadenza-test-vst3-{}", std::process::id()));
	let bundle = dir.join("CadenzaTestGain.vst3");
	let file = if cfg!(windows) {
		"CadenzaTestGain.vst3"
	} else if cfg!(target_os = "macos") {
		"CadenzaTestGain"
	} else {
		"CadenzaTestGain.so"
	};
	let arch_dir = bundle
		.join("Contents")
		.join(cadenza_plugin_host::discovery::vst3_arch_dir());
	std::fs::create_dir_all(&arch_dir).unwrap();
	std::fs::copy(&library, arch_dir.join(file)).unwrap();
	bundle
}

/// Path to the VST3 test gain bundle (`CadenzaTestGain.vst3`).
pub fn vst3_bundle() -> &'static Path {
	static PATH: OnceLock<PathBuf> = OnceLock::new();
	PATH.get_or_init(build_vst3_bundle)
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

pub const CRASH_ID: &str = "dev.cadenza.test.crash";

pub fn sandbox_exe() -> PathBuf {
	PathBuf::from(env!("CARGO_BIN_EXE_cadenza-plugin-sandbox"))
}

/// Sandbox options pointing at this build's sandbox executable, without core
/// dumps (the crash tests abort on purpose).
pub fn sandbox_options() -> cadenza_plugin_host::SandboxOptions {
	cadenza_plugin_host::SandboxOptions {
		executable: Some(sandbox_exe()),
		core_dumps: false,
		..Default::default()
	}
}

fn build_crash_plugin() -> PathBuf {
	let library = build_cdylib(&["--example", "clap_crash"], "clap_crash");
	let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
		.join(format!("cadenza-test-crash-{}", std::process::id()));
	std::fs::create_dir_all(&dir).unwrap();
	let dest = dir.join("cadenza-crash.clap");
	std::fs::copy(&library, &dest).unwrap();
	dest
}

/// Descriptor of the crash/hang test effect (`cadenza-crash.clap`).
pub fn crash_descriptor() -> &'static PluginDescriptor {
	static DESC: OnceLock<PluginDescriptor> = OnceLock::new();
	DESC.get_or_init(|| {
		let file = build_crash_plugin();
		let mut descs = scan_in_subprocess(&scanner(), &file, Duration::from_secs(20))
			.expect("scan crash plugin");
		assert_eq!(descs.len(), 1);
		descs.remove(0)
	})
}

pub const DUCKER_ID: &str = "dev.cadenza.test.ducker";
pub const MULTI_ID: &str = "dev.cadenza.test.multi";

/// Descriptors of the sidechain/multi-output test plugins
/// (`cadenza-routing.clap`), scanned out-of-process once.
pub fn routing_descriptors() -> &'static [PluginDescriptor] {
	static DESCS: OnceLock<Vec<PluginDescriptor>> = OnceLock::new();
	DESCS.get_or_init(|| {
		let library = build_cdylib(&["--example", "clap_routing"], "clap_routing");
		let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
			.join(format!("cadenza-test-routing-{}", std::process::id()));
		std::fs::create_dir_all(&dir).unwrap();
		let dest = dir.join("cadenza-routing.clap");
		std::fs::copy(&library, &dest).unwrap();
		scan_in_subprocess(&scanner(), &dest, Duration::from_secs(20))
			.expect("scan routing test plugins")
	})
}

pub fn routing_descriptor(id: &str) -> &'static PluginDescriptor {
	routing_descriptors()
		.iter()
		.find(|d| d.id == id)
		.unwrap_or_else(|| panic!("{id} not scanned"))
}
