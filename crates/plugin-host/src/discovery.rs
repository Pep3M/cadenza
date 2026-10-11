//! Locating plugin binaries on disk, following each format's conventions.

use std::path::{Path, PathBuf};

use crate::descriptor::PluginFormat;

fn env_path(var: &str) -> Option<PathBuf> {
	std::env::var_os(var)
		.filter(|v| !v.is_empty())
		.map(PathBuf::from)
}

fn home() -> Option<PathBuf> {
	#[cfg(windows)]
	{
		env_path("USERPROFILE")
	}
	#[cfg(not(windows))]
	{
		env_path("HOME")
	}
}

/// Directories listed in `$CLAP_PATH` (OS path-list syntax: `:` on Unix, `;` on Windows).
fn clap_path_env() -> Vec<PathBuf> {
	std::env::var_os("CLAP_PATH")
		.map(|v| {
			std::env::split_paths(&v)
				.filter(|p| !p.as_os_str().is_empty())
				.collect()
		})
		.unwrap_or_default()
}

/// Standard CLAP search locations for this OS (per the CLAP `entry.h` spec),
/// preceded by any directories in `$CLAP_PATH`. Directories that do not exist
/// are included; scanning simply skips them.
pub fn default_clap_paths() -> Vec<PathBuf> {
	let mut paths = clap_path_env();
	#[cfg(target_os = "linux")]
	{
		if let Some(h) = home() {
			paths.push(h.join(".clap"));
		}
		paths.push("/usr/lib/clap".into());
		paths.push("/usr/local/lib/clap".into());
	}
	#[cfg(target_os = "macos")]
	{
		paths.push("/Library/Audio/Plug-Ins/CLAP".into());
		if let Some(h) = home() {
			paths.push(h.join("Library/Audio/Plug-Ins/CLAP"));
		}
	}
	#[cfg(windows)]
	{
		if let Some(p) = env_path("COMMONPROGRAMFILES") {
			paths.push(p.join("CLAP"));
		}
		if let Some(p) = env_path("LOCALAPPDATA") {
			paths.push(p.join("Programs").join("Common").join("CLAP"));
		}
	}
	#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
	{
		if let Some(h) = home() {
			paths.push(h.join(".clap"));
		}
	}
	paths
}

/// Default search paths for every supported format.
pub fn default_search_paths() -> Vec<PathBuf> {
	default_clap_paths()
}

/// The format a plugin file belongs to, judged by its extension.
pub fn plugin_format(path: &Path) -> Option<PluginFormat> {
	let ext = path.extension()?.to_str()?;
	if ext.eq_ignore_ascii_case("clap") {
		Some(PluginFormat::Clap)
	} else if ext.eq_ignore_ascii_case("vst3") {
		Some(PluginFormat::Vst3)
	} else {
		None
	}
}

/// Finds every `.clap` file (or macOS bundle directory) under `roots`,
/// recursing into sub-directories. A root may itself be a plugin file. Results
/// are sorted and de-duplicated; unreadable directories are skipped.
pub fn find_clap_files(roots: &[PathBuf]) -> Vec<PathBuf> {
	let mut found = Vec::new();
	for root in roots {
		walk(root, 0, &mut found);
	}
	found.sort();
	found.dedup();
	found
}

const MAX_DEPTH: usize = 16;

fn walk(path: &Path, depth: usize, found: &mut Vec<PathBuf>) {
	// Follow symlinks: distro packages often link plugins into the CLAP dirs.
	let Ok(meta) = std::fs::metadata(path) else {
		return;
	};
	if plugin_format(path) == Some(PluginFormat::Clap) {
		// A `.clap` is either a shared library or (macOS) a bundle directory.
		found.push(path.to_path_buf());
		return;
	}
	if !meta.is_dir() || depth >= MAX_DEPTH {
		return;
	}
	let Ok(entries) = std::fs::read_dir(path) else {
		return;
	};
	for entry in entries.flatten() {
		walk(&entry.path(), depth + 1, found);
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn finds_nested_clap_files_only() {
		let dir = tempfile::tempdir().unwrap();
		let nested = dir.path().join("vendor/sub");
		std::fs::create_dir_all(&nested).unwrap();
		std::fs::write(nested.join("a.clap"), b"x").unwrap();
		std::fs::write(dir.path().join("B.CLAP"), b"x").unwrap();
		std::fs::write(dir.path().join("readme.txt"), b"x").unwrap();
		std::fs::create_dir_all(dir.path().join("bundle.clap/Contents")).unwrap();
		let found = find_clap_files(&[dir.path().to_path_buf(), dir.path().join("missing")]);
		let names: Vec<_> = found
			.iter()
			.map(|p| p.file_name().unwrap().to_str().unwrap().to_string())
			.collect();
		assert_eq!(names.len(), 3, "{names:?}");
		assert!(names.contains(&"a.clap".to_string()));
		assert!(names.contains(&"B.CLAP".to_string()));
		assert!(names.contains(&"bundle.clap".to_string()));
	}

	#[test]
	fn default_paths_are_absolute() {
		for p in default_clap_paths() {
			assert!(
				p.is_absolute() || std::env::var_os("CLAP_PATH").is_some(),
				"{p:?}"
			);
		}
	}
}
