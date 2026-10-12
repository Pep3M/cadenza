//! The AI's editing session: one project document plus the path it was opened/saved from,
//! and the catalog of installed third-party plugins.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use cadenza_engine::PluginHosting;
use cadenza_model::{Document, Project};
use cadenza_plugin_host::{PluginCatalog, default_cache_path};

pub struct Session {
	document: Document,
	path: Option<PathBuf>,
	plugin_cache: PathBuf,
	/// Loaded from `plugin_cache` on first use.
	plugins: Option<Arc<PluginCatalog>>,
	/// Where plugin devices run in renders, analysis and playback.
	plugin_hosting: PluginHosting,
}

impl Session {
	/// A session using the default plugin catalog cache (see [`default_cache_path`]).
	pub fn new() -> Self {
		Self::with_plugin_cache(default_cache_path())
	}

	/// A session whose plugin catalog lives at `cache_path` (used by tests and embedders).
	pub fn with_plugin_cache(cache_path: impl Into<PathBuf>) -> Self {
		Self {
			document: Document::new(Project::new("Untitled")),
			path: None,
			plugin_cache: cache_path.into(),
			plugins: None,
			plugin_hosting: PluginHosting::default(),
		}
	}

	pub fn document(&self) -> &Document {
		&self.document
	}

	pub fn document_mut(&mut self) -> &mut Document {
		&mut self.document
	}

	pub fn path(&self) -> Option<&Path> {
		self.path.as_deref()
	}

	/// Replaces the whole session with a freshly opened/created document.
	pub fn reset(&mut self, document: Document, path: Option<PathBuf>) {
		self.document = document;
		self.path = path;
	}

	pub fn set_path(&mut self, path: PathBuf) {
		self.path = Some(path);
	}

	/// Where the plugin catalog is cached on disk.
	pub fn plugin_cache_path(&self) -> &Path {
		&self.plugin_cache
	}

	/// The plugin catalog, loaded from its cache file on first use (empty until
	/// `scan_plugins` has run once on this machine).
	pub fn plugins(&mut self) -> Arc<PluginCatalog> {
		self.plugins
			.get_or_insert_with(|| Arc::new(PluginCatalog::load_or_default(&self.plugin_cache)))
			.clone()
	}

	/// Where plugin devices run (default: sandboxed unless
	/// `CADENZA_PLUGIN_HOSTING=in-process`; see [`PluginHosting`]).
	pub fn plugin_hosting(&self) -> &PluginHosting {
		&self.plugin_hosting
	}

	pub fn set_plugin_hosting(&mut self, hosting: PluginHosting) {
		self.plugin_hosting = hosting;
	}

	/// Replaces the plugin catalog (e.g. after a scan).
	pub fn set_plugins(&mut self, catalog: Arc<PluginCatalog>) {
		self.plugins = Some(catalog);
	}

	/// The catalog if the current project uses any plugin device, else `None` (so
	/// renders of plugin-free projects never touch the catalog cache).
	pub fn plugins_for_project(&mut self) -> Option<Arc<PluginCatalog>> {
		uses_plugins(self.document.project()).then(|| self.plugins())
	}
}

/// Whether any device in `project` is a third-party plugin.
pub fn uses_plugins(project: &Project) -> bool {
	let is_plugin =
		|d: &cadenza_model::Device| !matches!(d.kind, cadenza_model::DeviceKind::Builtin { .. });
	project.master.inserts.iter().any(is_plugin)
		|| project.tracks.iter().any(|t| {
			t.instrument.as_ref().is_some_and(is_plugin) || t.channel.inserts.iter().any(is_plugin)
		})
}

impl Default for Session {
	fn default() -> Self {
		Self::new()
	}
}
