//! Persistent catalog of scanned plugins with a crash blocklist.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::descriptor::PluginDescriptor;
use crate::discovery::find_clap_files;
use crate::error::PluginError;
use crate::scanner::{DEFAULT_SCAN_TIMEOUT, ScanFailure, scan_in_subprocess};

const CACHE_VERSION: u32 = 1;

/// Identity of a plugin file on disk; a change means it must be rescanned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FileStamp {
	/// Modification time, nanoseconds since the Unix epoch.
	pub mtime_ns: u128,
	pub size: u64,
}

impl FileStamp {
	/// Stamp of a file or bundle directory (for bundles, the newest entry
	/// inside counts, so updating the binary inside a bundle is noticed).
	pub fn of(path: &Path) -> Option<FileStamp> {
		let meta = std::fs::metadata(path).ok()?;
		let mut stamp = FileStamp {
			mtime_ns: mtime(&meta),
			size: meta.len(),
		};
		if meta.is_dir() {
			stamp.size = 0;
			bundle_stamp(path, &mut stamp, 0);
		}
		Some(stamp)
	}
}

fn mtime(meta: &std::fs::Metadata) -> u128 {
	meta.modified()
		.ok()
		.and_then(|t| t.duration_since(UNIX_EPOCH).ok())
		.map_or(0, |d| d.as_nanos())
}

fn bundle_stamp(dir: &Path, stamp: &mut FileStamp, depth: usize) {
	let Ok(entries) = std::fs::read_dir(dir) else {
		return;
	};
	for entry in entries.flatten() {
		let Ok(meta) = std::fs::metadata(entry.path()) else {
			continue;
		};
		stamp.mtime_ns = stamp.mtime_ns.max(mtime(&meta));
		if meta.is_dir() {
			if depth < 8 {
				bundle_stamp(&entry.path(), stamp, depth + 1);
			}
		} else {
			stamp.size += meta.len();
		}
	}
}

/// A plugin file that failed scanning and will not be loaded again until it
/// changes on disk or is explicitly unblocked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct BlockedPlugin {
	pub path: PathBuf,
	/// What happened (crash signal, timeout, load error...).
	pub reason: String,
	pub stamp: FileStamp,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct ScannedFile {
	stamp: FileStamp,
	plugins: Vec<PluginDescriptor>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct CacheFile {
	version: u32,
	files: BTreeMap<PathBuf, ScannedFile>,
	blocked: BTreeMap<PathBuf, BlockedPlugin>,
}

/// Outcome of [`PluginCatalog::scan`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ScanReport {
	/// Ids of plugins that are new to the catalog.
	pub added: Vec<String>,
	/// Ids of plugins whose file changed and was rescanned.
	pub updated: Vec<String>,
	/// Ids of plugins whose file disappeared.
	pub removed: Vec<String>,
	/// Files that failed during this scan (crash, timeout, invalid binary).
	pub blocked: Vec<BlockedPlugin>,
	/// Plugin files found on disk.
	pub files_found: usize,
	/// Files actually loaded by the scanner (the rest were unchanged or blocked).
	pub files_scanned: usize,
}

/// All known third-party plugins, persisted as JSON so that rescans only load
/// new or changed files.
#[derive(Debug)]
pub struct PluginCatalog {
	cache_path: PathBuf,
	data: CacheFile,
	timeout: Duration,
}

/// Default cache location: `$XDG_CACHE_HOME/cadenza/plugins.json` (or
/// `~/.cache/...`) on Linux, `~/Library/Caches/cadenza/plugins.json` on macOS,
/// `%LOCALAPPDATA%\cadenza\plugins.json` on Windows.
pub fn default_cache_path() -> PathBuf {
	let var = |k: &str| {
		std::env::var_os(k)
			.filter(|v| !v.is_empty())
			.map(PathBuf::from)
	};
	let base = if cfg!(windows) {
		var("LOCALAPPDATA")
	} else if cfg!(target_os = "macos") {
		var("HOME").map(|h| h.join("Library/Caches"))
	} else {
		var("XDG_CACHE_HOME").or_else(|| var("HOME").map(|h| h.join(".cache")))
	};
	base.unwrap_or_else(std::env::temp_dir)
		.join("cadenza")
		.join("plugins.json")
}

impl PluginCatalog {
	/// Loads the catalog cached at `cache_path`, or starts empty if the file is
	/// missing, unreadable or from an incompatible version.
	pub fn load_or_default(cache_path: impl Into<PathBuf>) -> Self {
		let cache_path = cache_path.into();
		let data = std::fs::read(&cache_path)
			.ok()
			.and_then(|b| serde_json::from_slice::<CacheFile>(&b).ok())
			.filter(|c| c.version == CACHE_VERSION)
			.unwrap_or_else(|| CacheFile {
				version: CACHE_VERSION,
				..Default::default()
			});
		Self {
			cache_path,
			data,
			timeout: DEFAULT_SCAN_TIMEOUT,
		}
	}

	/// Per-file timeout for the scanner subprocess (default 20 s).
	pub fn set_timeout(&mut self, timeout: Duration) {
		self.timeout = timeout;
	}

	pub fn cache_path(&self) -> &Path {
		&self.cache_path
	}

	/// Scans every `.clap` file under `roots` (directories or files) using the
	/// scanner executable, one subprocess per new or changed file. Unchanged
	/// files and blocked files are skipped. Plugins previously found under one
	/// of `roots` whose file is gone are removed.
	///
	/// Fails only if the scanner itself cannot be run; plugin failures are
	/// recorded in the blocklist and the report.
	pub fn scan(
		&mut self,
		roots: &[PathBuf],
		scanner_exe: &Path,
	) -> Result<ScanReport, PluginError> {
		if !scanner_exe.is_file() {
			return Err(PluginError::ScannerMissing(scanner_exe.to_path_buf()));
		}
		let files = find_clap_files(roots);
		let mut report = ScanReport {
			files_found: files.len(),
			..Default::default()
		};
		let known_ids: std::collections::HashSet<String> =
			self.descriptors().map(|d| d.id.clone()).collect();

		for file in &files {
			let Some(stamp) = FileStamp::of(file) else {
				continue;
			};
			if self.data.files.get(file).is_some_and(|f| f.stamp == stamp) {
				continue;
			}
			if let Some(b) = self.data.blocked.get(file) {
				if b.stamp == stamp {
					continue;
				}
				// The file changed since it failed: give it another chance.
				self.data.blocked.remove(file);
			}
			report.files_scanned += 1;
			match scan_in_subprocess(scanner_exe, file, self.timeout) {
				Ok(plugins) => {
					let old = self.data.files.remove(file);
					let old_ids: Vec<String> = old
						.map(|o| o.plugins.into_iter().map(|p| p.id).collect())
						.unwrap_or_default();
					for p in &plugins {
						if old_ids.contains(&p.id) || known_ids.contains(&p.id) {
							report.updated.push(p.id.clone());
						} else {
							report.added.push(p.id.clone());
						}
					}
					for id in old_ids {
						if !plugins.iter().any(|p| p.id == id) {
							report.removed.push(id);
						}
					}
					self.data
						.files
						.insert(file.clone(), ScannedFile { stamp, plugins });
				}
				Err(ScanFailure::Spawn(reason)) => {
					return Err(PluginError::Scan {
						path: file.clone(),
						reason,
					});
				}
				Err(ScanFailure::Plugin(reason)) => {
					if let Some(old) = self.data.files.remove(file) {
						report.removed.extend(old.plugins.into_iter().map(|p| p.id));
					}
					let blocked = BlockedPlugin {
						path: file.clone(),
						reason,
						stamp,
					};
					self.data.blocked.insert(file.clone(), blocked.clone());
					report.blocked.push(blocked);
				}
			}
		}

		// Forget files that vanished from the scanned roots (or from disk).
		let under_roots = |p: &Path| roots.iter().any(|r| p.starts_with(r));
		let gone: Vec<PathBuf> = self
			.data
			.files
			.keys()
			.filter(|p| !p.exists() || (under_roots(p) && !files.contains(p)))
			.cloned()
			.collect();
		for p in gone {
			if let Some(f) = self.data.files.remove(&p) {
				report.removed.extend(f.plugins.into_iter().map(|d| d.id));
			}
		}
		self.data
			.blocked
			.retain(|p, _| p.exists() && (!under_roots(p) || files.contains(p)));
		Ok(report)
	}

	/// Every scanned plugin, ordered by file path.
	pub fn descriptors(&self) -> impl Iterator<Item = &PluginDescriptor> {
		self.data.files.values().flat_map(|f| f.plugins.iter())
	}

	/// The plugin with the given id (first match if several files provide it).
	pub fn find(&self, id: &str) -> Option<&PluginDescriptor> {
		self.descriptors().find(|d| d.id == id)
	}

	/// The plugin with the given id, preferring the copy at `path` (as stored
	/// in a project) and falling back to any file that provides the id.
	pub fn find_at(&self, id: &str, path: &Path) -> Option<&PluginDescriptor> {
		self.data
			.files
			.get(path)
			.and_then(|f| f.plugins.iter().find(|d| d.id == id))
			.or_else(|| self.find(id))
	}

	/// Like [`PluginCatalog::find`], with an LLM-actionable error.
	pub fn get(&self, id: &str) -> Result<&PluginDescriptor, PluginError> {
		self.find(id)
			.ok_or_else(|| PluginError::NotFound(id.to_string()))
	}

	/// Files that failed scanning.
	pub fn blocked(&self) -> impl Iterator<Item = &BlockedPlugin> {
		self.data.blocked.values()
	}

	/// Removes a file from the blocklist so the next scan retries it. Returns
	/// whether it was blocked.
	pub fn unblock(&mut self, path: &Path) -> bool {
		self.data.blocked.remove(path).is_some()
	}

	/// Writes the catalog to its cache file (atomically, via a temp file).
	pub fn save(&self) -> Result<(), PluginError> {
		let err = |reason: String| PluginError::Cache {
			path: self.cache_path.clone(),
			reason,
		};
		if let Some(dir) = self.cache_path.parent() {
			std::fs::create_dir_all(dir).map_err(|e| err(e.to_string()))?;
		}
		let json = serde_json::to_vec_pretty(&self.data).map_err(|e| err(e.to_string()))?;
		let tmp = self.cache_path.with_extension("json.tmp");
		std::fs::write(&tmp, json).map_err(|e| err(e.to_string()))?;
		std::fs::rename(&tmp, &self.cache_path).map_err(|e| err(e.to_string()))
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn missing_cache_starts_empty_and_saves() {
		let dir = tempfile::tempdir().unwrap();
		let path = dir.path().join("sub/plugins.json");
		let cat = PluginCatalog::load_or_default(&path);
		assert_eq!(cat.descriptors().count(), 0);
		cat.save().unwrap();
		assert!(path.is_file());
		let again = PluginCatalog::load_or_default(&path);
		assert_eq!(again.blocked().count(), 0);
	}

	#[test]
	fn corrupt_cache_is_ignored() {
		let dir = tempfile::tempdir().unwrap();
		let path = dir.path().join("plugins.json");
		std::fs::write(&path, b"{not json").unwrap();
		let cat = PluginCatalog::load_or_default(&path);
		assert_eq!(cat.descriptors().count(), 0);
		assert!(matches!(cat.get("x"), Err(PluginError::NotFound(_))));
	}

	#[test]
	fn stamp_changes_with_content() {
		let dir = tempfile::tempdir().unwrap();
		let f = dir.path().join("a.clap");
		std::fs::write(&f, b"1").unwrap();
		let a = FileStamp::of(&f).unwrap();
		std::fs::write(&f, b"22").unwrap();
		assert_ne!(a, FileStamp::of(&f).unwrap());
	}

	#[test]
	fn default_cache_path_ends_in_cadenza() {
		assert!(default_cache_path().ends_with("cadenza/plugins.json"));
	}
}
