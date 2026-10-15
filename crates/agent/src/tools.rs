//! Bridge from model tool calls to the in-process Cadenza tools, behind the session's
//! policies: every file path is confined to the output folder, third-party plugins can be
//! switched off, and headless-useless tools (audio playback) are not offered.

use std::path::{Component, Path, PathBuf};

use cadenza_mcp::CadenzaServer;
use serde_json::{Map, Value};

use crate::types::{ToolCall, ToolSpec};

/// Tools not offered to the embedded agent: it runs headless and cannot hear playback
/// (in the app, playback belongs to the user's transport).
const HIDDEN_TOOLS: &[&str] = &["play", "stop", "transport_status", "restart_plugins"];
/// Also hidden when working on the user's open project: replacing the document would
/// throw away the user's work and its undo history.
pub const SESSION_HIDDEN_TOOLS: &[&str] = &["new_project", "open_project"];
/// Tools that only exist to work with third-party plugins.
const PLUGIN_TOOLS: &[&str] = &["scan_plugins"];
/// Tools that never change the project (they don't invalidate an earlier save/render).
const READ_ONLY_TOOLS: &[&str] = &[
	"get_project",
	"get_clip_notes",
	"history",
	"list_devices",
	"get_device_info",
	"scan_plugins",
	"scale_info",
	"analyze_mix",
	"analyze_track",
	"compare_to_reference",
	"mix_targets",
	"render",
	"save_project",
	"get_audio_info",
	"export_midi",
	"list_factory_samples",
	"list_missing_files",
	"pack_project",
	"export_stems",
];

/// Formats the `render` tool may write.
const RENDER_EXTENSIONS: &[&str] = &["wav", "flac", "mp3"];
/// Project archive extensions for `pack_project` (`.cadenza.zip` ends in `.zip`).
const ARCHIVE_EXTENSIONS: &[&str] = &["zip", "cadenzapack"];
/// MIDI file extensions for `import_midi`/`export_midi`.
const MIDI_EXTENSIONS: &[&str] = &["mid", "midi"];

pub use cadenza_core::paths::real_path;

/// A drive-relative (`C:file`) or `..` component: never inside the session folder. An
/// absolute path's drive prefix is fine (it is checked against the root).
fn escapes(raw: &Path) -> bool {
	raw.components().any(|c| match c {
		Component::ParentDir => true,
		Component::Prefix(_) => !raw.is_absolute(),
		_ => false,
	})
}

/// Where the session may read and write files.
#[derive(Debug, Clone)]
pub struct PathPolicy {
	root: PathBuf,
	default_project: PathBuf,
	default_wav: PathBuf,
	/// Files outside the root the session may read (the user's reference track).
	extra_reads: Vec<PathBuf>,
}

impl PathPolicy {
	/// `root` must exist; it is canonicalized so symlinks can't be used to escape it.
	pub fn new(root: &Path, project_file: &str, wav_file: &str) -> std::io::Result<Self> {
		let root = real_path(root)?;
		Ok(Self {
			default_project: root.join(project_file),
			default_wav: root.join(wav_file),
			root,
			extra_reads: Vec::new(),
		})
	}

	pub fn root(&self) -> &Path {
		&self.root
	}

	pub fn default_project(&self) -> &Path {
		&self.default_project
	}

	pub fn default_wav(&self) -> &Path {
		&self.default_wav
	}

	/// Resolves a model-supplied path (relative to the session folder, or absolute inside
	/// it) and checks its extension. Creates missing parent folders inside the root.
	pub fn resolve(&self, raw: &str, extension: &str) -> Result<PathBuf, String> {
		self.resolve_any(raw, &[extension])
	}

	/// [`PathPolicy::resolve`] accepting any of `extensions` (the first one is suggested
	/// in error messages).
	pub fn resolve_any(&self, raw: &str, extensions: &[&str]) -> Result<PathBuf, String> {
		let extension = extensions.first().copied().unwrap_or("");
		let outside = || {
			format!(
				"path '{raw}' is outside the session folder; use a plain file name such as \
				 'mix.{extension}' (paths are relative to the session folder)"
			)
		};
		let raw_path = Path::new(raw.trim());
		if raw.trim().is_empty() {
			return Err(format!(
				"empty path; use a file name ending in .{extension}"
			));
		}
		if escapes(raw_path) {
			return Err(outside());
		}
		let path = if raw_path.is_absolute() {
			raw_path.to_path_buf()
		} else {
			self.root.join(raw_path)
		};
		if !path.starts_with(&self.root) || path == self.root {
			return Err(outside());
		}
		let ext_ok = path
			.extension()
			.and_then(|e| e.to_str())
			.is_some_and(|e| extensions.iter().any(|x| e.eq_ignore_ascii_case(x)));
		if !ext_ok {
			let list = extensions
				.iter()
				.map(|x| format!(".{x}"))
				.collect::<Vec<_>>()
				.join(", ");
			return Err(format!("path '{raw}' must end in one of: {list}"));
		}
		let parent = path.parent().ok_or_else(outside)?;
		std::fs::create_dir_all(parent).map_err(|e| format!("cannot create folder: {e}"))?;
		let real_parent = real_path(parent).map_err(|e| format!("cannot resolve folder: {e}"))?;
		if !real_parent.starts_with(&self.root) {
			return Err(outside());
		}
		if path.is_symlink() {
			return Err(outside());
		}
		Ok(path)
	}
}

impl PathPolicy {
	/// A folder inside the session folder (relative or absolute; the root itself is
	/// allowed), e.g. where `relink_files` searches.
	pub fn resolve_dir(&self, raw: &str) -> Result<PathBuf, String> {
		let outside = || {
			format!("folder '{raw}' is outside the session folder; use a folder inside it (or '.')")
		};
		let raw_path = Path::new(raw.trim());
		if escapes(raw_path) {
			return Err(outside());
		}
		let path = if raw_path.is_absolute() {
			raw_path.to_path_buf()
		} else {
			self.root.join(raw_path)
		};
		let real = real_path(&path).map_err(|e| format!("cannot open folder '{raw}': {e}"))?;
		if !real.starts_with(&self.root) {
			return Err(outside());
		}
		Ok(real)
	}

	/// [`PathPolicy::resolve_dir`] creating the folder first (output folders).
	pub fn resolve_dir_create(&self, raw: &str) -> Result<PathBuf, String> {
		let raw_path = Path::new(raw.trim());
		let joined = if raw_path.is_absolute() {
			raw_path.to_path_buf()
		} else {
			self.root.join(raw_path)
		};
		let lexically_inside = joined.starts_with(&self.root) && !escapes(raw_path);
		if lexically_inside {
			std::fs::create_dir_all(&joined).map_err(|e| format!("cannot create folder: {e}"))?;
		}
		self.resolve_dir(raw)
	}

	/// Lets the session read `path` although it lies outside the root (a reference track
	/// the user chose). Ignored when it does not exist.
	pub fn allow_read(&mut self, path: &Path) {
		if let Ok(real) = real_path(path) {
			self.extra_reads.push(real);
		}
	}

	/// An existing file to read: one allowed with [`PathPolicy::allow_read`], else a file in
	/// the session folder with one of `extensions`.
	pub fn resolve_read(&self, raw: &str, extensions: &[&str]) -> Result<PathBuf, String> {
		if let Ok(real) = real_path(Path::new(raw.trim()))
			&& self.extra_reads.contains(&real)
		{
			return Ok(real);
		}
		self.resolve_any(raw, extensions)
	}

	/// A sample for `load_sample`: an audio file in the session folder, or one of the
	/// factory samples (read-only library).
	pub fn resolve_sample(&self, raw: &str) -> Result<PathBuf, String> {
		self.resolve_sample_in(raw, &cadenza_mcp::tools::samples::factory_folder())
	}

	fn resolve_sample_in(&self, raw: &str, factory: &Path) -> Result<PathBuf, String> {
		let extensions = cadenza_mcp::tools::audio::AUDIO_EXTENSIONS;
		let candidate = Path::new(raw.trim());
		if candidate.is_absolute()
			&& let (Ok(real), Ok(factory)) = (real_path(candidate), real_path(factory))
			&& real.starts_with(&factory)
		{
			return Ok(real);
		}
		self.resolve_any(raw, extensions)
			.map_err(|e| format!("{e}; factory samples (list_factory_samples) are allowed too"))
	}
}

/// Side effects the agent tracks to know when the session's deliverables exist.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
	Saved(PathBuf),
	Rendered {
		path: PathBuf,
		summary: String,
	},
	/// The project changed (an earlier save/render is stale).
	Mutated,
	None,
}

/// The result of one dispatched call, ready to feed back to the model.
#[derive(Debug, Clone)]
pub struct ToolOutcome {
	pub content: String,
	pub is_error: bool,
	pub effect: Effect,
	/// One-line summary for progress displays.
	pub summary: String,
}

pub struct ToolBridge {
	server: CadenzaServer,
	policy: PathPolicy,
	allow_plugins: bool,
	/// Hidden on top of [`HIDDEN_TOOLS`].
	hidden: &'static [&'static str],
	max_result_chars: usize,
	specs: Vec<ToolSpec>,
}

impl ToolBridge {
	pub fn new(
		server: CadenzaServer,
		policy: PathPolicy,
		allow_plugins: bool,
		max_result_chars: usize,
	) -> Self {
		Self::with_hidden(server, policy, allow_plugins, max_result_chars, &[])
	}

	/// A bridge that also hides `hidden` tools (e.g. [`SESSION_HIDDEN_TOOLS`]).
	pub fn with_hidden(
		server: CadenzaServer,
		policy: PathPolicy,
		allow_plugins: bool,
		max_result_chars: usize,
		hidden: &'static [&'static str],
	) -> Self {
		let specs = server
			.tool_definitions()
			.into_iter()
			.filter(|d| offered(&d.name, allow_plugins) && !hidden.contains(&d.name.as_str()))
			.map(|d| ToolSpec {
				description: describe(&d.name, d.description),
				parameters: sanitize_schema(d.input_schema),
				name: d.name,
			})
			.collect();
		Self {
			server,
			policy,
			allow_plugins,
			hidden,
			max_result_chars,
			specs,
		}
	}

	/// Changes where files may be read and written (the open project's folder moves
	/// with "Save As").
	pub fn set_policy(&mut self, policy: PathPolicy) {
		self.policy = policy;
	}

	/// Tools offered to the model, sorted by name (stable across sessions).
	pub fn specs(&self) -> &[ToolSpec] {
		&self.specs
	}

	pub fn policy(&self) -> &PathPolicy {
		&self.policy
	}

	pub fn server(&self) -> &CadenzaServer {
		&self.server
	}

	/// Validates, rewrites (paths) and dispatches one call.
	pub async fn call(&self, call: &ToolCall) -> ToolOutcome {
		let args = match parse_arguments(&call.arguments) {
			Ok(args) => args,
			Err(message) => return self.error(message),
		};
		let args = match self.apply_policy(&call.name, args) {
			Ok(args) => args,
			Err(message) => return self.error(message),
		};
		self.dispatch(&call.name, args).await
	}

	/// Dispatches a call the harness itself makes (already policy-conformant).
	pub async fn dispatch(&self, name: &str, args: Value) -> ToolOutcome {
		let path_arg = args.get("path").and_then(Value::as_str).map(PathBuf::from);
		match self.server.call_tool_json(name, args).await {
			Ok(value) => {
				let effect = match (name, path_arg) {
					("save_project", Some(path)) => Effect::Saved(path),
					("render", Some(path)) => Effect::Rendered {
						summary: loudness_summary(&value),
						path,
					},
					_ if READ_ONLY_TOOLS.contains(&name) => Effect::None,
					_ => Effect::Mutated,
				};
				let summary = match &effect {
					Effect::Rendered { summary, .. } => summary.clone(),
					_ if name == "analyze_mix" => loudness_summary(&value),
					_ => crate::observer::clip(&compact(&value), 140),
				};
				ToolOutcome {
					content: self.truncate(compact(&value)),
					is_error: false,
					effect,
					summary,
				}
			}
			Err(message) => self.error(message),
		}
	}

	fn error(&self, message: String) -> ToolOutcome {
		ToolOutcome {
			summary: crate::observer::clip(&message, 140),
			content: self.truncate(message),
			is_error: true,
			effect: Effect::None,
		}
	}

	fn truncate(&self, text: String) -> String {
		let total = text.chars().count();
		if total <= self.max_result_chars {
			return text;
		}
		let kept: String = text.chars().take(self.max_result_chars).collect();
		format!(
			"{kept}\n[truncated: the result had {total} characters; request less at once, e.g. \
			 filter list_devices/get_device_info with query/limit or read one clip at a time]"
		)
	}

	fn apply_policy(&self, name: &str, mut args: Value) -> Result<Value, String> {
		if !offered(name, self.allow_plugins) || self.hidden.contains(&name) {
			return Err(format!(
				"tool '{name}' is not available in this session; use the tools you were given"
			));
		}
		if !self.allow_plugins {
			if let Some(id) = find_plugin_id(&args) {
				return Err(format!(
					"third-party plugins are disabled for this session ('{id}'); use built-in \
					 devices from list_devices"
				));
			}
			if name == "list_devices" {
				args["source"] = Value::String("builtin".into());
			}
		}
		let obj = args
			.as_object_mut()
			.expect("parse_arguments returns objects");
		match name {
			"save_project" => {
				let path = match obj.get("path").and_then(Value::as_str) {
					Some(raw) => self.policy.resolve(raw, "json")?,
					None => self.policy.default_project.clone(),
				};
				obj.insert("path".into(), path_value(&path));
			}
			"open_project" => {
				let raw = obj
					.get("path")
					.and_then(Value::as_str)
					.ok_or("missing 'path'")?;
				let path = self.policy.resolve(raw, "json")?;
				obj.insert("path".into(), path_value(&path));
			}
			"render" => {
				let path = match obj.get("path").and_then(Value::as_str) {
					Some(raw) => self.policy.resolve_any(raw, RENDER_EXTENSIONS)?,
					None => self.policy.default_wav.clone(),
				};
				obj.insert("path".into(), path_value(&path));
			}
			// Audio/MIDI files are read from and written to the session folder only.
			"import_audio" | "import_midi" | "export_midi" | "get_audio_info" => {
				let extensions = match name {
					"import_midi" | "export_midi" => MIDI_EXTENSIONS,
					_ => cadenza_mcp::tools::audio::AUDIO_EXTENSIONS,
				};
				match obj.get("path").and_then(Value::as_str) {
					Some(raw) => {
						let path = self.policy.resolve_any(raw, extensions)?;
						obj.insert("path".into(), path_value(&path));
					}
					None if name == "get_audio_info" => {}
					None => return Err("missing 'path'".into()),
				}
			}
			"compare_to_reference" => {
				let raw = obj
					.get("file")
					.and_then(Value::as_str)
					.ok_or("missing 'file'")?;
				let path = self
					.policy
					.resolve_read(raw, cadenza_mcp::tools::audio::AUDIO_EXTENSIONS)?;
				obj.insert("file".into(), path_value(&path));
			}
			"relink_files" => {
				let raw = obj
					.get("search_folder")
					.and_then(Value::as_str)
					.ok_or("missing 'search_folder'")?;
				let path = self.policy.resolve_dir(raw)?;
				obj.insert("search_folder".into(), path_value(&path));
			}
			"export_stems" => {
				let raw = obj
					.get("folder")
					.and_then(Value::as_str)
					.unwrap_or("Renders")
					.to_owned();
				let path = self.policy.resolve_dir_create(&raw)?;
				obj.insert("folder".into(), path_value(&path));
			}
			"pack_project" => {
				let raw = obj
					.get("path")
					.and_then(Value::as_str)
					.ok_or("missing 'path'")?;
				let path = self.policy.resolve_any(raw, ARCHIVE_EXTENSIONS)?;
				obj.insert("path".into(), path_value(&path));
			}
			// Samples come from the session folder or the factory sample library.
			"load_sample" => {
				let raw = obj
					.get("path")
					.and_then(Value::as_str)
					.ok_or("missing 'path'")?;
				let path = self.policy.resolve_sample(raw)?;
				obj.insert("path".into(), path_value(&path));
			}
			"scan_plugins" => {
				let custom = obj
					.get("paths")
					.is_some_and(|p| p.as_array().is_some_and(|a| !a.is_empty()));
				if custom {
					return Err(
						"custom scan paths are disabled; omit 'paths' to scan the standard \
						 plugin locations"
							.into(),
					);
				}
			}
			_ => {}
		}
		Ok(args)
	}
}

fn offered(name: &str, allow_plugins: bool) -> bool {
	!HIDDEN_TOOLS.contains(&name) && (allow_plugins || !PLUGIN_TOOLS.contains(&name))
}

/// File-path tools get a note that paths are relative to the session folder.
fn describe(name: &str, description: String) -> String {
	match name {
		"save_project" | "open_project" | "render" | "import_audio" | "import_midi"
		| "export_midi" | "get_audio_info" | "relink_files" | "pack_project" | "export_stems" => format!(
			"{description} Paths are file names relative to the session folder; locations \
			 outside it are rejected."
		),
		"load_sample" => format!(
			"{description} Paths are file names relative to the session folder, or factory \
			 sample paths from list_factory_samples; other locations are rejected."
		),
		_ => description,
	}
}

fn path_value(path: &Path) -> Value {
	Value::String(path.display().to_string())
}

/// Parses the model's argument text into a JSON object (empty text means no arguments).
fn parse_arguments(raw: &str) -> Result<Value, String> {
	let trimmed = raw.trim();
	if trimmed.is_empty() {
		return Ok(Value::Object(Map::new()));
	}
	match serde_json::from_str::<Value>(trimmed) {
		Ok(Value::Object(map)) => Ok(Value::Object(map)),
		Ok(Value::Null) => Ok(Value::Object(Map::new())),
		Ok(other) => Err(format!(
			"tool arguments must be a JSON object, got {}; re-send the call with an object",
			crate::observer::clip(&other.to_string(), 120)
		)),
		Err(err) => Err(format!(
			"tool arguments are not valid JSON ({err}); nothing was executed. Received: {}. \
			 Re-send the call with a valid JSON object.",
			crate::observer::clip(trimmed, 200)
		)),
	}
}

fn find_plugin_id(value: &Value) -> Option<String> {
	match value {
		Value::String(s) if s.starts_with("clap:") || s.starts_with("vst3:") => Some(s.clone()),
		Value::Array(items) => items.iter().find_map(find_plugin_id),
		Value::Object(map) => map.values().find_map(find_plugin_id),
		_ => None,
	}
}

pub fn compact(value: &Value) -> String {
	match value {
		Value::String(s) => s.clone(),
		other => other.to_string(),
	}
}

/// "-14.2 LUFS, true peak -1.1 dBTP" from a render report or analyze_mix output.
fn loudness_summary(value: &Value) -> String {
	let report = value.get("report").unwrap_or(value);
	let master = &report["master"];
	match (
		master["lufs_integrated"].as_f64(),
		master["true_peak_db"].as_f64(),
	) {
		(Some(lufs), Some(tp)) => {
			let mut s = format!("{lufs:.1} LUFS, true peak {tp:.1} dBTP");
			if let Some(d) = report["duration_seconds"].as_f64() {
				s.push_str(&format!(", {d:.0} s"));
			}
			s
		}
		_ => crate::observer::clip(&compact(value), 140),
	}
}

/// Makes a schemars-generated schema portable across providers: drops `$schema`,
/// inlines `$ref`s into `$defs` (some providers reject references), and removes
/// non-standard numeric `format`s ("double", "uint32"...).
pub fn sanitize_schema(schema: Value) -> Value {
	let mut original = schema;
	if let Some(obj) = original.as_object_mut() {
		obj.remove("$schema");
	}
	let mut without_defs = original.clone();
	let defs = without_defs
		.as_object_mut()
		.and_then(|o| o.remove("$defs"))
		.unwrap_or(Value::Null);
	// a cyclic or unresolvable schema keeps its references
	inline_refs(without_defs, &defs, 0).unwrap_or(original)
}

fn inline_refs(value: Value, defs: &Value, depth: u32) -> Option<Value> {
	if depth > 32 {
		return None;
	}
	Some(match value {
		Value::Object(mut map) => {
			if let Some(Value::String(reference)) = map.get("$ref") {
				let name = reference.strip_prefix("#/$defs/")?;
				let mut target = defs.get(name)?.clone();
				map.remove("$ref");
				// sibling keywords (description, default) override the definition's
				if let Value::Object(target_map) = &mut target {
					for (k, v) in map {
						target_map.insert(k, v);
					}
				}
				return inline_refs(target, defs, depth + 1);
			}
			let mut out = Map::new();
			for (key, val) in map {
				if key == "format"
					&& matches!(&val, Value::String(f) if !STANDARD_FORMATS.contains(&f.as_str()))
				{
					continue;
				}
				out.insert(key, inline_refs(val, defs, depth + 1)?);
			}
			Value::Object(out)
		}
		Value::Array(items) => Value::Array(
			items
				.into_iter()
				.map(|v| inline_refs(v, defs, depth + 1))
				.collect::<Option<Vec<_>>>()?,
		),
		other => other,
	})
}

const STANDARD_FORMATS: &[&str] = &["uuid", "date-time", "date", "time", "email", "uri"];

#[cfg(test)]
mod tests {
	use super::*;
	use serde_json::json;

	fn policy() -> (PathPolicy, tempfile::TempDir) {
		let dir = tempfile::tempdir().unwrap();
		(
			PathPolicy::new(dir.path(), "song.cadenza.json", "song.wav").unwrap(),
			dir,
		)
	}

	#[test]
	fn samples_load_from_the_session_folder_or_the_factory_library() {
		let (policy, dir) = policy();
		let factory = tempfile::tempdir().unwrap();
		let kick = factory.path().join("Kick.wav");
		std::fs::write(&kick, b"x").unwrap();
		let ok = policy
			.resolve_sample_in(kick.to_str().unwrap(), factory.path())
			.unwrap();
		assert_eq!(ok, real_path(kick).unwrap());
		let root = real_path(dir.path()).unwrap();
		assert_eq!(
			policy
				.resolve_sample_in("drums/snare.wav", factory.path())
				.unwrap(),
			root.join("drums/snare.wav")
		);
		let err = policy
			.resolve_sample_in("/etc/passwd.wav", factory.path())
			.unwrap_err();
		assert!(err.contains("factory samples"), "{err}");
		assert!(
			policy
				.resolve_sample_in("notes.txt", factory.path())
				.is_err()
		);
	}

	#[test]
	fn a_reference_track_outside_the_folder_is_readable_once_allowed() {
		let (mut policy, _dir) = policy();
		let outside = tempfile::tempdir().unwrap();
		let reference = outside.path().join("hit.wav");
		let other = outside.path().join("other.wav");
		std::fs::write(&reference, b"x").unwrap();
		std::fs::write(&other, b"x").unwrap();
		let exts = cadenza_mcp::tools::audio::AUDIO_EXTENSIONS;
		assert!(
			policy
				.resolve_read(reference.to_str().unwrap(), exts)
				.is_err()
		);
		policy.allow_read(&reference);
		assert_eq!(
			policy
				.resolve_read(reference.to_str().unwrap(), exts)
				.unwrap(),
			real_path(reference).unwrap()
		);
		assert!(policy.resolve_read(other.to_str().unwrap(), exts).is_err());
	}

	#[test]
	fn paths_are_confined_to_the_session_folder() {
		let (policy, dir) = policy();
		let root = real_path(dir.path()).unwrap();
		assert_eq!(
			policy.resolve("mix.wav", "wav").unwrap(),
			root.join("mix.wav")
		);
		assert_eq!(
			policy.resolve("takes/v2.wav", "wav").unwrap(),
			root.join("takes/v2.wav")
		);
		assert!(root.join("takes").is_dir());
		let inside = root.join("abs.wav");
		assert_eq!(
			policy.resolve(inside.to_str().unwrap(), "wav").unwrap(),
			inside
		);
		for bad in [
			"/etc/passwd.wav",
			"../escape.wav",
			"a/../../b.wav",
			"",
			"mix.mp3",
		] {
			assert!(policy.resolve(bad, "wav").is_err(), "{bad} accepted");
		}
	}

	#[cfg(unix)]
	#[test]
	fn symlinked_folders_cannot_escape() {
		let (policy, dir) = policy();
		let outside = tempfile::tempdir().unwrap();
		std::os::unix::fs::symlink(outside.path(), dir.path().join("link")).unwrap();
		assert!(policy.resolve("link/x.wav", "wav").is_err());
	}

	#[test]
	fn file_tools_are_confined_and_typed() {
		let (policy, dir) = policy();
		let root = real_path(dir.path()).unwrap();
		let bridge = ToolBridge::new(CadenzaServer::new(), policy, false, 10_000);
		let ok = |name: &str, path: &str| {
			bridge
				.apply_policy(name, json!({ "path": path }))
				.map(|v| v["path"].as_str().unwrap().to_owned())
		};
		assert_eq!(
			ok("render", "mix.flac").unwrap(),
			root.join("mix.flac").display().to_string()
		);
		assert!(ok("render", "mix.aiff").is_err());
		assert!(ok("import_audio", "takes/vox.wav").is_ok());
		assert!(ok("import_audio", "/etc/passwd.wav").is_err());
		assert!(ok("import_audio", "song.mid").is_err());
		assert!(ok("import_midi", "song.mid").is_ok());
		assert!(ok("pack_project", "share/Song.cadenza.zip").is_ok());
		assert!(ok("pack_project", "/tmp/Song.cadenza.zip").is_err());
		let relink =
			|folder: &str| bridge.apply_policy("relink_files", json!({ "search_folder": folder }));
		std::fs::create_dir_all(root.join("takes")).unwrap();
		assert_eq!(
			relink("takes").unwrap()["search_folder"],
			json!(root.join("takes").display().to_string())
		);
		assert!(relink(".").is_ok());
		assert!(relink("/etc").is_err());
		assert!(relink("../").is_err());
		let stems = bridge.apply_policy("export_stems", json!({})).unwrap();
		assert_eq!(
			stems["folder"],
			json!(root.join("Renders").display().to_string())
		);
		assert!(
			bridge
				.apply_policy("export_stems", json!({ "folder": "/tmp" }))
				.is_err()
		);
		assert!(ok("export_midi", "../out.mid").is_err());
		assert!(ok("export_midi", "out.wav").unwrap_err().contains(".mid"));
		assert!(ok("get_audio_info", "/tmp/x.wav").is_err());
		assert!(bridge.apply_policy("get_audio_info", json!({})).is_ok());
		assert!(bridge.apply_policy("import_midi", json!({})).is_err());
		assert!(
			bridge
				.specs()
				.iter()
				.any(|s| s.name == "import_audio" && s.description.contains("session folder"))
		);
	}

	#[test]
	fn session_bridges_hide_document_replacing_tools() {
		let (policy, _dir) = policy();
		let bridge = ToolBridge::with_hidden(
			CadenzaServer::new(),
			policy,
			false,
			10_000,
			SESSION_HIDDEN_TOOLS,
		);
		let names: Vec<&str> = bridge.specs().iter().map(|s| s.name.as_str()).collect();
		assert!(!names.contains(&"new_project") && !names.contains(&"open_project"));
		assert!(!names.contains(&"play"));
		assert!(names.contains(&"get_project") && names.contains(&"undo"));
		assert!(
			bridge
				.apply_policy("new_project", json!({"name": "x"}))
				.unwrap_err()
				.contains("not available")
		);
	}

	#[test]
	fn schemas_lose_refs_and_nonstandard_formats() {
		let schema = json!({
			"$schema": "https://json-schema.org/draft/2020-12/schema",
			"$defs": {
				"Pitch": {"anyOf": [{"type": "string"}, {"type": "integer"}], "description": "note"},
				"Note": {"type": "object", "properties": {"pitch": {"$ref": "#/$defs/Pitch"},
					"start": {"type": "number", "format": "double"}}}
			},
			"type": "object",
			"properties": {
				"clip_id": {"type": "string", "format": "uuid"},
				"notes": {"type": "array", "items": {"$ref": "#/$defs/Note"}},
				"first": {"$ref": "#/$defs/Pitch", "description": "override"}
			}
		});
		let clean = sanitize_schema(schema);
		let text = clean.to_string();
		assert!(!text.contains("$ref") && !text.contains("$defs") && !text.contains("$schema"));
		assert!(!text.contains("double"));
		assert_eq!(clean["properties"]["clip_id"]["format"], "uuid");
		assert_eq!(
			clean["properties"]["notes"]["items"]["properties"]["pitch"]["anyOf"][0]["type"],
			"string"
		);
		assert_eq!(clean["properties"]["first"]["description"], "override");
	}

	#[test]
	fn arguments_parse_robustly() {
		assert_eq!(parse_arguments("").unwrap(), json!({}));
		assert_eq!(parse_arguments("null").unwrap(), json!({}));
		assert_eq!(parse_arguments(" {\"a\":1} ").unwrap(), json!({"a": 1}));
		assert!(parse_arguments("[1]").unwrap_err().contains("object"));
		assert!(
			parse_arguments("{\"a\":")
				.unwrap_err()
				.contains("not valid JSON")
		);
	}

	#[test]
	fn plugin_ids_are_found_anywhere_in_arguments() {
		assert_eq!(
			find_plugin_id(&json!({"instrument": {"device": "clap:org.surge"}})).as_deref(),
			Some("clap:org.surge")
		);
		assert_eq!(find_plugin_id(&json!({"device": "poly_synth"})), None);
	}
}
