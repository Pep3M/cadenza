//! Running the out-of-process scanner (`cadenza-plugin-scanner`).
//!
//! Loading an unknown plugin can crash, hang or corrupt the loading process,
//! so the host never does it directly: a short-lived child process loads one
//! file, writes the descriptors as JSON and exits. Crashes, non-zero exits and
//! timeouts become blocklist entries in the catalog.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crate::descriptor::PluginDescriptor;
use crate::error::PluginError;

/// Environment variable overriding the scanner executable location.
pub const SCANNER_ENV: &str = "CADENZA_PLUGIN_SCANNER";
/// Default per-file scan timeout.
pub const DEFAULT_SCAN_TIMEOUT: Duration = Duration::from_secs(20);
/// File name of the scanner executable for this OS.
pub const SCANNER_FILE_NAME: &str = if cfg!(windows) {
	"cadenza-plugin-scanner.exe"
} else {
	"cadenza-plugin-scanner"
};

/// Where the scanner executable is expected: `$CADENZA_PLUGIN_SCANNER` if set,
/// else next to the running executable (also checking the parent directory,
/// which is where Cargo puts binaries relative to test executables in `deps/`).
pub fn scanner_path() -> Result<PathBuf, PluginError> {
	if let Some(p) = std::env::var_os(SCANNER_ENV).filter(|p| !p.is_empty()) {
		let p = PathBuf::from(p);
		return if p.is_file() {
			Ok(p)
		} else {
			Err(PluginError::ScannerMissing(p))
		};
	}
	let exe = std::env::current_exe()
		.map_err(|_| PluginError::ScannerMissing(SCANNER_FILE_NAME.into()))?;
	let dir = exe.parent().unwrap_or(Path::new("."));
	let candidates = [
		Some(dir.join(SCANNER_FILE_NAME)),
		dir.parent().map(|d| d.join(SCANNER_FILE_NAME)),
	];
	candidates
		.iter()
		.flatten()
		.find(|p| p.is_file())
		.cloned()
		.ok_or_else(|| PluginError::ScannerMissing(dir.join(SCANNER_FILE_NAME)))
}

/// Why a scan subprocess did not yield descriptors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScanFailure {
	/// The scanner could not be started at all (not the plugin's fault).
	Spawn(String),
	/// The plugin crashed, failed, timed out or produced unusable output.
	Plugin(String),
}

fn unique_output_path() -> PathBuf {
	static COUNTER: AtomicU64 = AtomicU64::new(0);
	std::env::temp_dir().join(format!(
		"cadenza-scan-{}-{}.json",
		std::process::id(),
		COUNTER.fetch_add(1, Ordering::Relaxed)
	))
}

fn tail_text(bytes: &[u8], max: usize) -> String {
	let text = String::from_utf8_lossy(bytes);
	let text = text.trim();
	let start = text.len().saturating_sub(max);
	let start = (start..text.len())
		.find(|&i| text.is_char_boundary(i))
		.unwrap_or(text.len());
	text[start..].replace('\n', " | ")
}

#[cfg(unix)]
fn describe_exit(status: std::process::ExitStatus) -> String {
	use std::os::unix::process::ExitStatusExt;
	match (status.code(), status.signal()) {
		(_, Some(sig)) => format!("crashed (signal {sig})"),
		(Some(code), _) => format!("exited with code {code}"),
		_ => "exited abnormally".into(),
	}
}

#[cfg(not(unix))]
fn describe_exit(status: std::process::ExitStatus) -> String {
	match status.code() {
		// NTSTATUS crash codes (access violation etc.) have the high bit set.
		Some(code) if code < 0 => format!("crashed (exception 0x{:08X})", code as u32),
		Some(code) => format!("exited with code {code}"),
		None => "exited abnormally".into(),
	}
}

/// Scans one plugin file in a child process. The child writes its JSON result
/// to a temporary file (not stdout, which plugins may pollute with logging).
pub fn scan_in_subprocess(
	scanner: &Path,
	plugin: &Path,
	timeout: Duration,
) -> Result<Vec<PluginDescriptor>, ScanFailure> {
	let output = unique_output_path();
	let mut child = Command::new(scanner)
		.arg(plugin)
		.arg("--output")
		.arg(&output)
		.stdin(Stdio::null())
		.stdout(Stdio::null())
		.stderr(Stdio::piped())
		.spawn()
		.map_err(|e| ScanFailure::Spawn(format!("cannot run '{}': {e}", scanner.display())))?;

	// Drain stderr concurrently so a chatty plugin cannot block on a full pipe.
	let stderr = child.stderr.take();
	let reader = std::thread::spawn(move || {
		let mut buf = Vec::new();
		if let Some(mut s) = stderr {
			let _ = s.read_to_end(&mut buf);
		}
		buf
	});

	let deadline = Instant::now() + timeout;
	let status = loop {
		match child.try_wait() {
			Ok(Some(status)) => break Some(status),
			Ok(None) if Instant::now() >= deadline => {
				let _ = child.kill();
				let _ = child.wait();
				break None;
			}
			Ok(None) => std::thread::sleep(Duration::from_millis(10)),
			Err(e) => {
				let _ = child.kill();
				let _ = std::fs::remove_file(&output);
				return Err(ScanFailure::Spawn(format!(
					"waiting for scanner failed: {e}"
				)));
			}
		}
	};
	// A hung plugin may have spawned grandchildren holding stderr open, so do
	// not wait for the reader after a timeout.
	let stderr = if status.is_some() {
		reader.join().unwrap_or_default()
	} else {
		Vec::new()
	};
	let result = std::fs::read(&output);
	let _ = std::fs::remove_file(&output);

	let Some(status) = status else {
		return Err(ScanFailure::Plugin(format!(
			"timed out after {:.0} s while loading (the plugin hung)",
			timeout.as_secs_f64()
		)));
	};
	let detail = tail_text(&stderr, 400);
	let with_detail = |msg: String| {
		if detail.is_empty() {
			msg
		} else {
			format!("{msg}: {detail}")
		}
	};
	if !status.success() {
		return Err(ScanFailure::Plugin(with_detail(describe_exit(status))));
	}
	let bytes = result
		.map_err(|_| ScanFailure::Plugin(with_detail("scanner produced no result".into())))?;
	serde_json::from_slice(&bytes)
		.map_err(|e| ScanFailure::Plugin(format!("scanner output was not valid JSON: {e}")))
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn tail_text_keeps_the_end() {
		assert_eq!(tail_text(b"abc\ndef", 3), "def");
		assert_eq!(tail_text("ééé".as_bytes(), 3), "é");
		assert_eq!(tail_text(b"  \n", 10), "");
	}

	#[test]
	fn missing_scanner_is_a_spawn_failure() {
		let r = scan_in_subprocess(
			Path::new("/definitely/not/here/scanner"),
			Path::new("x.clap"),
			Duration::from_secs(1),
		);
		assert!(matches!(r, Err(ScanFailure::Spawn(_))));
	}
}
