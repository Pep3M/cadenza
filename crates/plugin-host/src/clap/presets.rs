//! CLAP factory presets: discovery through the preset-discovery factory and
//! loading through the preset-load extension. Runs on the plugin main thread.
//!
//! A preset is addressed by an opaque key (see [`PresetKey`]) holding its
//! location (inside the plugin, or a file) and the provider's load key.

use std::ffi::{CStr, CString};
use std::path::{Path, PathBuf};

use cadenza_dsp::PresetInfo;
use clack_extensions::preset_discovery::indexer::IndexerImpl;
use clack_extensions::preset_discovery::metadata_receiver::MetadataReceiverImpl;
use clack_extensions::preset_discovery::prelude::{
	PluginPresetLoad, PresetDiscoveryFactory, Provider,
};
use clack_extensions::preset_discovery::preset_data::{
	FileType, Flags, Location, LocationInfo, Soundpack,
};
use clack_host::prelude::*;
use clack_host::utils::{Timestamp, UniversalPluginId};

use super::host::host_info;

/// Upper bounds that keep a huge preset library from stalling the caller.
const MAX_PRESETS: usize = 20_000;
const MAX_FILES: usize = 50_000;
const MAX_DEPTH: usize = 8;

/// Separates the fields of an encoded preset key.
const SEP: char = '\u{1f}';

/// Where a preset lives and how to load it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PresetKey {
	/// `None`: inside the plugin itself.
	pub file: Option<PathBuf>,
	pub load_key: Option<String>,
}

impl PresetKey {
	pub fn encode(&self) -> String {
		let load_key = self.load_key.as_deref().unwrap_or("");
		match &self.file {
			None => format!("plugin{SEP}{load_key}"),
			Some(path) => format!("file{SEP}{}{SEP}{load_key}", path.display()),
		}
	}

	pub fn decode(key: &str) -> Option<Self> {
		let mut parts = key.split(SEP);
		let kind = parts.next()?;
		let file = match kind {
			"plugin" => None,
			"file" => Some(PathBuf::from(parts.next()?)),
			_ => return None,
		};
		let load_key = parts.next()?;
		if parts.next().is_some() {
			return None;
		}
		Some(Self {
			file,
			load_key: (!load_key.is_empty()).then(|| load_key.to_string()),
		})
	}
}

#[derive(Default)]
struct Indexer {
	extensions: Vec<Option<String>>,
	locations: Vec<(String, Option<PathBuf>)>,
}

impl IndexerImpl for Indexer {
	fn declare_filetype(&mut self, file_type: FileType) -> Result<(), HostError> {
		let ext = file_type
			.file_extension
			.map(|e| e.to_string_lossy().trim_start_matches('.').to_string())
			.filter(|e| !e.is_empty());
		self.extensions.push(ext);
		Ok(())
	}

	fn declare_location(&mut self, location: LocationInfo) -> Result<(), HostError> {
		let name = location.name.to_string_lossy().into_owned();
		let path = location
			.location
			.file_path()
			.map(|p| PathBuf::from(p.to_string_lossy().into_owned()));
		self.locations.push((name, path));
		Ok(())
	}

	fn declare_soundpack(&mut self, _soundpack: Soundpack) -> Result<(), HostError> {
		Ok(())
	}
}

struct Pending {
	name: Option<String>,
	load_key: Option<String>,
	plugin_ids: Vec<(String, String)>,
}

/// Collects the presets one `get_metadata` call reports.
struct Receiver<'a> {
	plugin_id: &'a str,
	category: &'a str,
	file: Option<&'a Path>,
	current: Option<Pending>,
	out: &'a mut Vec<PresetInfo>,
}

impl Receiver<'_> {
	fn finish(&mut self) {
		let Some(p) = self.current.take() else { return };
		let ours = p.plugin_ids.is_empty()
			|| p.plugin_ids
				.iter()
				.any(|(abi, id)| abi.eq_ignore_ascii_case("clap") && id == self.plugin_id);
		if !ours || self.out.len() >= MAX_PRESETS {
			return;
		}
		let name = p
			.name
			.filter(|n| !n.trim().is_empty())
			.or_else(|| {
				self.file
					.and_then(Path::file_stem)
					.map(|s| s.to_string_lossy().into_owned())
			})
			.or_else(|| p.load_key.clone())
			.unwrap_or_default();
		let key = PresetKey {
			file: self.file.map(Path::to_path_buf),
			load_key: p.load_key,
		};
		self.out.push(PresetInfo {
			key: key.encode(),
			name,
			category: self.category.to_string(),
		});
	}
}

impl MetadataReceiverImpl for Receiver<'_> {
	fn on_error(&mut self, _error_code: i32, _error_message: Option<&CStr>) {}

	fn begin_preset(
		&mut self,
		name: Option<&CStr>,
		load_key: Option<&CStr>,
	) -> Result<(), HostError> {
		self.finish();
		self.current = Some(Pending {
			name: name.map(|n| n.to_string_lossy().into_owned()),
			load_key: load_key.map(|k| k.to_string_lossy().into_owned()),
			plugin_ids: Vec::new(),
		});
		Ok(())
	}

	fn add_plugin_id(&mut self, plugin_id: UniversalPluginId) {
		if let Some(p) = &mut self.current {
			p.plugin_ids.push((
				plugin_id.abi.to_string_lossy().into_owned(),
				plugin_id.id.to_string_lossy().into_owned(),
			));
		}
	}

	fn set_soundpack_id(&mut self, _soundpack_id: &CStr) {}
	fn set_flags(&mut self, _flags: Flags) {}
	fn add_creator(&mut self, _creator: &CStr) {}
	fn set_description(&mut self, _description: &CStr) {}
	fn set_timestamps(&mut self, _creation: Option<Timestamp>, _modification: Option<Timestamp>) {}
	fn add_feature(&mut self, _feature: &CStr) {}
	fn add_extra_info(&mut self, _key: &CStr, _value: &CStr) {}
}

fn matches_extension(path: &Path, extensions: &[Option<String>]) -> bool {
	if extensions.is_empty() || extensions.iter().any(Option::is_none) {
		return true;
	}
	let ext = path
		.extension()
		.map(|e| e.to_string_lossy().to_lowercase())
		.unwrap_or_default();
	extensions.iter().flatten().any(|e| e.to_lowercase() == ext)
}

/// Preset files under `root` (or `root` itself), depth-first and sorted.
fn preset_files(root: &Path, extensions: &[Option<String>]) -> Vec<PathBuf> {
	let mut out = Vec::new();
	if root.is_file() {
		out.push(root.to_path_buf());
		return out;
	}
	let mut stack = vec![(root.to_path_buf(), 0)];
	while let Some((dir, depth)) = stack.pop() {
		let Ok(entries) = std::fs::read_dir(&dir) else {
			continue;
		};
		let mut entries: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
		entries.sort();
		for path in entries.into_iter().rev() {
			if out.len() >= MAX_FILES {
				return out;
			}
			if path.is_dir() {
				if depth < MAX_DEPTH {
					stack.push((path, depth + 1));
				}
			} else if matches_extension(&path, extensions) {
				out.push(path);
			}
		}
	}
	out.sort();
	out
}

/// Every factory preset the entry's preset providers report for `plugin_id`.
/// Plugins without a preset-discovery factory have none.
pub(crate) fn discover(entry: &PluginEntry, plugin_id: &str) -> Vec<PresetInfo> {
	let Some(factory) = entry.get_factory::<PresetDiscoveryFactory>() else {
		return Vec::new();
	};
	let provider_ids: Vec<CString> = factory
		.provider_descriptors()
		.filter_map(|d| d.id().map(CStr::to_owned))
		.collect();
	let host = host_info();
	let mut out = Vec::new();
	for id in provider_ids {
		let Ok(mut provider) = Provider::instantiate(Indexer::default(), entry, &id, &host) else {
			continue;
		};
		let locations = provider.indexer().locations.clone();
		let extensions = provider.indexer().extensions.clone();
		for (category, path) in locations {
			let files = match &path {
				None => vec![None],
				Some(root) => preset_files(root, &extensions)
					.into_iter()
					.map(Some)
					.collect(),
			};
			for file in files {
				if out.len() >= MAX_PRESETS {
					return out;
				}
				let c_path = match &file {
					None => None,
					Some(f) => match CString::new(f.to_string_lossy().into_owned()) {
						Ok(c) => Some(c),
						Err(_) => continue,
					},
				};
				let location = match &c_path {
					None => Location::Plugin,
					Some(path) => Location::File { path },
				};
				let mut receiver = Receiver {
					plugin_id,
					category: &category,
					file: file.as_deref(),
					current: None,
					out: &mut out,
				};
				provider.get_metadata(location, &mut receiver);
				receiver.finish();
			}
		}
	}
	out
}

/// Loads the preset `key` into `instance` through the preset-load extension.
pub(crate) fn load(instance: &mut PluginInstance<super::host::CadenzaHost>, key: &str) -> bool {
	let Some(key) = PresetKey::decode(key) else {
		return false;
	};
	let handle = instance.plugin_handle();
	let Some(ext) = handle.get_extension::<PluginPresetLoad>() else {
		return false;
	};
	let path = match &key.file {
		None => None,
		Some(f) => match CString::new(f.to_string_lossy().into_owned()) {
			Ok(c) => Some(c),
			Err(_) => return false,
		},
	};
	let location = match &path {
		None => Location::Plugin,
		Some(path) => Location::File { path },
	};
	let load_key = match key.load_key.map(CString::new) {
		None => None,
		Some(Ok(k)) => Some(k),
		Some(Err(_)) => return false,
	};
	ext.load_from_location(&handle, location, load_key.as_deref())
		.is_ok()
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn keys_round_trip() {
		for key in [
			PresetKey {
				file: None,
				load_key: Some("bank/1".into()),
			},
			PresetKey {
				file: None,
				load_key: None,
			},
			PresetKey {
				file: Some("/presets/My Pad.fxp".into()),
				load_key: None,
			},
		] {
			assert_eq!(PresetKey::decode(&key.encode()), Some(key));
		}
		assert_eq!(PresetKey::decode("nonsense"), None);
	}

	#[test]
	fn extension_filter() {
		let exts = vec![Some("fxp".to_string())];
		assert!(matches_extension(Path::new("/a/b.FXP"), &exts));
		assert!(!matches_extension(Path::new("/a/b.txt"), &exts));
		assert!(matches_extension(Path::new("/a/b.txt"), &[None]));
	}
}
