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

/// Directories listed in `$VST3_PATH` (OS path-list syntax). Not part of the
/// VST3 spec, but a common convention for non-standard install locations.
fn vst3_path_env() -> Vec<PathBuf> {
	std::env::var_os("VST3_PATH")
		.map(|v| {
			std::env::split_paths(&v)
				.filter(|p| !p.as_os_str().is_empty())
				.collect()
		})
		.unwrap_or_default()
}

/// Standard VST3 search locations for this OS (per the VST3 "Plug-in
/// Locations" spec), preceded by any directories in `$VST3_PATH`.
pub fn default_vst3_paths() -> Vec<PathBuf> {
	let mut paths = vst3_path_env();
	#[cfg(target_os = "linux")]
	{
		if let Some(h) = home() {
			paths.push(h.join(".vst3"));
		}
		paths.push("/usr/lib/vst3".into());
		paths.push("/usr/local/lib/vst3".into());
	}
	#[cfg(target_os = "macos")]
	{
		if let Some(h) = home() {
			paths.push(h.join("Library/Audio/Plug-Ins/VST3"));
		}
		paths.push("/Library/Audio/Plug-Ins/VST3".into());
	}
	#[cfg(windows)]
	{
		if let Some(p) = env_path("LOCALAPPDATA") {
			paths.push(p.join("Programs").join("Common").join("VST3"));
		}
		if let Some(p) = env_path("COMMONPROGRAMFILES") {
			paths.push(p.join("VST3"));
		}
	}
	#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
	{
		if let Some(h) = home() {
			paths.push(h.join(".vst3"));
		}
	}
	paths
}

/// Default search paths for every supported format (CLAP first, then VST3).
pub fn default_search_paths() -> Vec<PathBuf> {
	let mut paths = default_clap_paths();
	paths.extend(default_vst3_paths());
	paths
}

/// Architecture folder inside a VST3 bundle's `Contents` for this build
/// (e.g. `x86_64-linux`, `x86_64-win`), per the VST3 bundle spec.
pub fn vst3_arch_dir() -> &'static str {
	#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
	{
		"x86_64-linux"
	}
	#[cfg(all(target_os = "linux", target_arch = "x86"))]
	{
		"i386-linux"
	}
	#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
	{
		"aarch64-linux"
	}
	#[cfg(all(target_os = "linux", target_arch = "arm"))]
	{
		"armv7l-linux"
	}
	#[cfg(all(windows, target_arch = "x86_64"))]
	{
		"x86_64-win"
	}
	#[cfg(all(windows, target_arch = "x86"))]
	{
		"x86-win"
	}
	#[cfg(all(windows, target_arch = "aarch64"))]
	{
		"arm64-win"
	}
	#[cfg(target_os = "macos")]
	{
		"MacOS"
	}
	#[cfg(not(any(
		all(
			target_os = "linux",
			any(
				target_arch = "x86_64",
				target_arch = "x86",
				target_arch = "aarch64",
				target_arch = "arm"
			)
		),
		all(
			windows,
			any(target_arch = "x86_64", target_arch = "x86", target_arch = "aarch64")
		),
		target_os = "macos"
	)))]
	{
		"unknown"
	}
}

/// The loadable binary of a VST3 plugin: the file itself for a legacy
/// single-file `.vst3`, else `X.vst3/Contents/<arch>/X(.so|.vst3)` inside the
/// bundle (falling back to the only binary in the architecture folder when
/// its name differs from the bundle's).
pub fn vst3_binary(path: &Path) -> Option<PathBuf> {
	let meta = std::fs::metadata(path).ok()?;
	if meta.is_file() {
		return Some(path.to_path_buf());
	}
	let stem = path.file_stem()?;
	let arch = path.join("Contents").join(vst3_arch_dir());
	let ext = if cfg!(windows) {
		Some("vst3")
	} else if cfg!(target_os = "macos") {
		None
	} else {
		Some("so")
	};
	let mut name = PathBuf::from(stem);
	if let Some(ext) = ext {
		name.set_extension(ext);
	}
	let direct = arch.join(&name);
	if direct.is_file() {
		return Some(direct);
	}
	let mut candidates: Vec<PathBuf> = std::fs::read_dir(&arch)
		.ok()?
		.flatten()
		.map(|e| e.path())
		.filter(|p| {
			p.is_file()
				&& match ext {
					Some(ext) => p
						.extension()
						.and_then(|e| e.to_str())
						.is_some_and(|e| e.eq_ignore_ascii_case(ext)),
					None => true,
				}
		})
		.collect();
	candidates.sort();
	(candidates.len() == 1).then(|| candidates.remove(0))
}

/// `Contents/Resources/moduleinfo.json` of a VST3 bundle, if present.
pub fn vst3_moduleinfo(path: &Path) -> Option<PathBuf> {
	let p = path
		.join("Contents")
		.join("Resources")
		.join("moduleinfo.json");
	p.is_file().then_some(p)
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
	find_files(roots, &[PluginFormat::Clap])
}

/// Finds every `.vst3` bundle directory (or legacy single-file `.vst3`) under
/// `roots`. Bundles are not searched inside.
pub fn find_vst3_files(roots: &[PathBuf]) -> Vec<PathBuf> {
	find_files(roots, &[PluginFormat::Vst3])
}

/// Every plugin file of any supported format under `roots`.
pub fn find_plugin_files(roots: &[PathBuf]) -> Vec<PathBuf> {
	find_files(roots, &[PluginFormat::Clap, PluginFormat::Vst3])
}

fn find_files(roots: &[PathBuf], formats: &[PluginFormat]) -> Vec<PathBuf> {
	let mut found = Vec::new();
	for root in roots {
		walk(root, 0, formats, &mut found);
	}
	found.sort();
	found.dedup();
	found
}

const MAX_DEPTH: usize = 16;

fn walk(path: &Path, depth: usize, formats: &[PluginFormat], found: &mut Vec<PathBuf>) {
	// Follow symlinks: distro packages often link plugins into the plugin dirs.
	let Ok(meta) = std::fs::metadata(path) else {
		return;
	};
	if let Some(format) = plugin_format(path) {
		// A plugin is either a single file or a bundle directory; never look
		// inside bundles (a VST3 bundle's Windows binary is itself `*.vst3`).
		if formats.contains(&format) {
			found.push(path.to_path_buf());
		}
		return;
	}
	if !meta.is_dir() || depth >= MAX_DEPTH {
		return;
	}
	let Ok(entries) = std::fs::read_dir(path) else {
		return;
	};
	for entry in entries.flatten() {
		walk(&entry.path(), depth + 1, formats, found);
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
	fn finds_vst3_bundles_without_descending() {
		let dir = tempfile::tempdir().unwrap();
		let arch = dir.path().join("Synth.vst3/Contents").join(vst3_arch_dir());
		std::fs::create_dir_all(&arch).unwrap();
		let bin_name = if cfg!(windows) {
			"Synth.vst3"
		} else if cfg!(target_os = "macos") {
			"Synth"
		} else {
			"Synth.so"
		};
		std::fs::write(arch.join(bin_name), b"x").unwrap();
		std::fs::write(dir.path().join("Legacy.vst3"), b"x").unwrap();
		std::fs::write(dir.path().join("x.clap"), b"x").unwrap();
		let found = find_vst3_files(&[dir.path().to_path_buf()]);
		assert_eq!(
			found,
			vec![
				dir.path().join("Legacy.vst3"),
				dir.path().join("Synth.vst3")
			]
		);
		assert_eq!(find_plugin_files(&[dir.path().to_path_buf()]).len(), 3);
		assert_eq!(
			vst3_binary(&dir.path().join("Synth.vst3")),
			Some(arch.join(bin_name))
		);
		assert_eq!(
			vst3_binary(&dir.path().join("Legacy.vst3")),
			Some(dir.path().join("Legacy.vst3"))
		);
		assert_eq!(vst3_moduleinfo(&dir.path().join("Synth.vst3")), None);
	}

	#[test]
	fn vst3_binary_falls_back_to_single_candidate() {
		let dir = tempfile::tempdir().unwrap();
		let arch = dir.path().join("A.vst3/Contents").join(vst3_arch_dir());
		std::fs::create_dir_all(&arch).unwrap();
		let other = if cfg!(windows) {
			"Other.vst3"
		} else if cfg!(target_os = "macos") {
			"Other"
		} else {
			"Other.so"
		};
		std::fs::write(arch.join(other), b"x").unwrap();
		assert_eq!(
			vst3_binary(&dir.path().join("A.vst3")),
			Some(arch.join(other))
		);
		assert_eq!(vst3_binary(&dir.path().join("missing.vst3")), None);
	}

	#[test]
	fn default_paths_are_absolute() {
		for p in default_vst3_paths() {
			assert!(
				p.is_absolute() || std::env::var_os("VST3_PATH").is_some(),
				"{p:?}"
			);
		}
		for p in default_clap_paths() {
			assert!(
				p.is_absolute() || std::env::var_os("CLAP_PATH").is_some(),
				"{p:?}"
			);
		}
	}
}
