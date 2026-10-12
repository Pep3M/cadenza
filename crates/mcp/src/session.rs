//! The AI's editing session: one project document plus the path it was opened/saved from,
//! and the catalog of installed third-party plugins.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use cadenza_engine::PluginHosting;
use cadenza_model::{Document, Project};
use cadenza_plugin_host::{PluginCatalog, default_cache_path};

/// A session several owners edit together (the app's state and the embedded AI agent):
/// lock it for short critical sections only.
pub type SharedSession = Arc<Mutex<Session>>;

/// Identifies one state of a session: `(epoch, document revision)`. The epoch changes
/// whenever the document is replaced (new/open project).
pub type ChangeKey = (u64, u64);

pub struct Session {
	document: Document,
	/// Bumped by [`Session::reset`]: document revisions restart with a new document.
	epoch: u64,
	/// The state last saved to or loaded from disk.
	clean: ChangeKey,
	path: Option<PathBuf>,
	plugin_cache: PathBuf,
	/// Loaded from `plugin_cache` on first use.
	plugins: Option<Arc<PluginCatalog>>,
	/// Where plugin devices run in renders, analysis and playback.
	plugin_hosting: PluginHosting,
	/// User presets (`<dir>/<device id>/<name>.json`).
	preset_dir: PathBuf,
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
			epoch: 0,
			clean: (0, 0),
			path: None,
			plugin_cache: cache_path.into(),
			plugins: None,
			plugin_hosting: PluginHosting::default(),
			preset_dir: crate::tools::presets::default_preset_dir(),
		}
	}

	/// Where user presets are stored (see [`crate::tools::presets::default_preset_dir`]).
	pub fn preset_dir(&self) -> &Path {
		&self.preset_dir
	}

	pub fn set_preset_dir(&mut self, dir: impl Into<PathBuf>) {
		self.preset_dir = dir.into();
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

	/// Replaces the whole session with a freshly opened/created document (which counts
	/// as clean).
	pub fn reset(&mut self, document: Document, path: Option<PathBuf>) {
		self.document = document;
		self.path = path;
		self.epoch += 1;
		self.clean = self.change_key();
	}

	/// The current state's key: compare it with an earlier one to know whether anything
	/// changed (document edits, undo/redo, a new document).
	pub fn change_key(&self) -> ChangeKey {
		(self.epoch, self.document.revision())
	}

	/// Records that the current state is what is on disk (after a save).
	pub fn mark_clean(&mut self) {
		self.clean = self.change_key();
	}

	/// Whether nothing changed since the last save/open.
	pub fn is_clean(&self) -> bool {
		self.clean == self.change_key()
	}

	/// Wraps the session for shared use.
	pub fn into_shared(self) -> SharedSession {
		Arc::new(Mutex::new(self))
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
