//! Per-user folders for Cadenza's data, configuration, caches and state, following each
//! OS's conventions. Every function returns the `cadenza` folder inside the base folder.
//!
//! | kind | Linux / BSD | macOS | Windows |
//! |---|---|---|---|
//! | data | `$XDG_DATA_HOME` or `~/.local/share` | `~/Library/Application Support` | `%APPDATA%` |
//! | config | `$XDG_CONFIG_HOME` or `~/.config` | `~/Library/Application Support` | `%APPDATA%` |
//! | cache | `$XDG_CACHE_HOME` or `~/.cache` | `~/Library/Caches` | `%LOCALAPPDATA%` |
//! | state | `$XDG_STATE_HOME` or `~/.local/state` | `~/Library/Application Support` | `%LOCALAPPDATA%` |
//!
//! An absolute `XDG_*_HOME` wins on every OS (handy for tests and portable setups);
//! without a usable base folder the system temp folder is used.

use std::path::{Path, PathBuf};

const APP: &str = "cadenza";

#[derive(Clone, Copy)]
enum Kind {
	Data,
	Config,
	Cache,
	State,
}

fn var(key: &str) -> Option<PathBuf> {
	std::env::var_os(key)
		.filter(|v| !v.is_empty())
		.map(PathBuf::from)
		.filter(|p| p.is_absolute())
}

/// The user's home folder: `$HOME`, else `%USERPROFILE%` (Windows).
pub fn home_dir() -> Option<PathBuf> {
	var("HOME").or_else(|| {
		if cfg!(windows) {
			var("USERPROFILE")
		} else {
			None
		}
	})
}

fn base(kind: Kind) -> Option<PathBuf> {
	let xdg = match kind {
		Kind::Data => "XDG_DATA_HOME",
		Kind::Config => "XDG_CONFIG_HOME",
		Kind::Cache => "XDG_CACHE_HOME",
		Kind::State => "XDG_STATE_HOME",
	};
	if let Some(dir) = var(xdg) {
		return Some(dir);
	}
	if cfg!(windows) {
		match kind {
			Kind::Data | Kind::Config => var("APPDATA"),
			Kind::Cache | Kind::State => var("LOCALAPPDATA"),
		}
	} else if cfg!(target_os = "macos") {
		let library = home_dir()?.join("Library");
		Some(match kind {
			Kind::Cache => library.join("Caches"),
			_ => library.join("Application Support"),
		})
	} else {
		let home = home_dir()?;
		Some(match kind {
			Kind::Data => home.join(".local/share"),
			Kind::Config => home.join(".config"),
			Kind::Cache => home.join(".cache"),
			Kind::State => home.join(".local/state"),
		})
	}
}

fn app_dir(kind: Kind) -> PathBuf {
	base(kind).unwrap_or_else(std::env::temp_dir).join(APP)
}

/// Data the user would back up: libraries, presets, factory samples.
pub fn data_dir() -> PathBuf {
	app_dir(Kind::Data)
}

/// Settings and credentials.
pub fn config_dir() -> PathBuf {
	app_dir(Kind::Config)
}

/// Re-creatable files (waveform peaks, rendered edits).
pub fn cache_dir() -> PathBuf {
	app_dir(Kind::Cache)
}

/// Logs, autosaves and AI session transcripts.
pub fn state_dir() -> PathBuf {
	app_dir(Kind::State)
}

/// `path` canonicalized (absolute, symlinks resolved), without the `\\?\` prefix Windows
/// puts on drive paths, so it compares equal to the paths users, file dialogs and models
/// write (macOS temp folders, for one, resolve from `/var` to `/private/var`).
pub fn real_path(path: impl AsRef<Path>) -> std::io::Result<PathBuf> {
	let real = path.as_ref().canonicalize()?;
	#[cfg(windows)]
	if let Some(rest) = real.to_str().and_then(|s| s.strip_prefix(r"\\?\"))
		&& rest.as_bytes().get(1) == Some(&b':')
	{
		return Ok(PathBuf::from(rest));
	}
	Ok(real)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn every_kind_ends_in_the_app_folder() {
		for dir in [data_dir(), config_dir(), cache_dir(), state_dir()] {
			assert!(dir.ends_with(APP), "{}", dir.display());
			assert!(dir.is_absolute(), "{}", dir.display());
		}
	}
}
