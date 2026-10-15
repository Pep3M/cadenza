//! Portable projects: collect external files into the project folder, find missing files
//! again, and pack/unpack a project folder as a single archive.
//!
//! Folder layout and on-disk path rules live in [`cadenza_model::files`]. Every change to
//! file references is one undoable `Batch` through the document.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use cadenza_model::files::{AUDIO_DIR, CACHE_DIR, PROJECT_SUFFIX, SAMPLES_DIR};
use cadenza_model::{Command, Document, FileRef, Project, ProjectLayout};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::McpError;
use crate::session::Session;

/// Extensions of project archives (`.cadenza.zip` is written; both are read).
pub const ARCHIVE_EXTENSIONS: &[&str] = &["cadenza.zip", "cadenzapack"];

/// Label of the undo step written by [`collect_files`].
pub const COLLECT_LABEL: &str = "Collect project files";
/// Label of the undo step written by [`relink_files`].
pub const RELINK_LABEL: &str = "Relink missing files";

fn io(path: &Path, e: impl std::fmt::Display) -> McpError {
	McpError::Io {
		path: path.display().to_string(),
		message: e.to_string(),
	}
}

/// SHA-256 of a file's content.
pub fn hash_file(path: &Path) -> Result<[u8; 32], McpError> {
	let mut file = File::open(path).map_err(|e| io(path, e))?;
	let mut hasher = Sha256::new();
	let mut buf = vec![0u8; 1 << 16];
	loop {
		let n = file.read(&mut buf).map_err(|e| io(path, e))?;
		if n == 0 {
			break;
		}
		hasher.update(&buf[..n]);
	}
	Ok(hasher.finalize().into())
}

// --- collect ----------------------------------------------------------------------------

/// One file to copy into the project folder.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct PlannedCopy {
	pub from: PathBuf,
	pub to: PathBuf,
	pub bytes: u64,
}

/// What [`plan_collect`] decided; [`run_copies`] executes it.
#[derive(Debug, Clone, Default)]
pub struct CollectPlan {
	pub copies: Vec<PlannedCopy>,
	/// Every external path -> its place in the project folder (copied or deduplicated).
	pub mapping: HashMap<PathBuf, PathBuf>,
	/// External files whose content already exists in the project (or another source):
	/// referenced without a new copy.
	pub deduplicated: usize,
	/// References already inside the project folder.
	pub inside: usize,
	/// Factory samples left in the shared factory folder.
	pub factory_skipped: usize,
	/// Referenced files that do not exist.
	pub missing: Vec<PathBuf>,
}

impl CollectPlan {
	pub fn bytes(&self) -> u64 {
		self.copies.iter().map(|c| c.bytes).sum()
	}
}

/// `dir/name`, or `dir/stem (n).ext` for the first `n` whose name is free (neither on
/// disk nor already planned) or holds the same content (`hash`).
fn destination(
	dir: &Path,
	name: &str,
	hash: &[u8; 32],
	reserved: &HashMap<PathBuf, [u8; 32]>,
) -> Result<(PathBuf, bool), McpError> {
	let p = Path::new(name);
	let stem = p
		.file_stem()
		.map(|s| s.to_string_lossy().into_owned())
		.unwrap_or_else(|| "audio".into());
	let ext = p
		.extension()
		.map(|e| format!(".{}", e.to_string_lossy()))
		.unwrap_or_default();
	for n in 1.. {
		let candidate = if n == 1 {
			dir.join(format!("{stem}{ext}"))
		} else {
			dir.join(format!("{stem} ({n}){ext}"))
		};
		if let Some(h) = reserved.get(&candidate) {
			if h == hash {
				return Ok((candidate, true));
			}
			continue;
		}
		if candidate.exists() {
			if candidate.is_file() && hash_file(&candidate)? == *hash {
				return Ok((candidate, true));
			}
			continue;
		}
		return Ok((candidate, false));
	}
	unreachable!("an unbounded range always yields a free name")
}

/// Plans copying every referenced file outside the folder of `project_file` into
/// `Audio/` (clips) or `Samples/` (zones, under their original names). Identical content
/// is copied once and reuses a copy already in the folder. Factory samples (`factory`
/// folder) stay shared unless `include_factory`.
pub fn plan_collect(
	project: &Project,
	project_file: &Path,
	include_factory: bool,
	factory: &Path,
) -> Result<CollectPlan, McpError> {
	let layout = ProjectLayout::new(project_file);
	let mut plan = CollectPlan::default();
	let mut by_hash: HashMap<[u8; 32], PathBuf> = HashMap::new();
	let mut reserved: HashMap<PathBuf, [u8; 32]> = HashMap::new();
	let mut seen: HashSet<PathBuf> = HashSet::new();
	for r in project.file_refs() {
		if !seen.insert(r.path.clone()) {
			continue;
		}
		if layout.contains(&r.path) {
			plan.inside += 1;
			continue;
		}
		if !include_factory && r.path.starts_with(factory) {
			plan.factory_skipped += 1;
			continue;
		}
		let Ok(meta) = std::fs::metadata(&r.path) else {
			plan.missing.push(r.path.clone());
			continue;
		};
		if !meta.is_file() {
			plan.missing.push(r.path.clone());
			continue;
		}
		let hash = hash_file(&r.path)?;
		if let Some(dest) = by_hash.get(&hash) {
			plan.mapping.insert(r.path.clone(), dest.clone());
			plan.deduplicated += 1;
			continue;
		}
		let dir = if r.is_sample() {
			layout.samples_dir()
		} else {
			layout.audio_dir()
		};
		let name = if r.name.is_empty() {
			file_name(&r.path)
		} else {
			r.name.clone()
		};
		let (dest, exists) = destination(&dir, &name, &hash, &reserved)?;
		if exists {
			plan.deduplicated += 1;
		} else {
			reserved.insert(dest.clone(), hash);
			plan.copies.push(PlannedCopy {
				from: r.path.clone(),
				to: dest.clone(),
				bytes: meta.len(),
			});
		}
		by_hash.insert(hash, dest.clone());
		plan.mapping.insert(r.path.clone(), dest);
	}
	Ok(plan)
}

fn file_name(path: &Path) -> String {
	path.file_name()
		.map(|n| n.to_string_lossy().into_owned())
		.unwrap_or_else(|| "file".into())
}

/// Copies the planned files (creating `Audio/`/`Samples/`); returns the bytes copied.
/// `progress(done, total)` is called after each file.
pub fn run_copies(
	plan: &CollectPlan,
	mut progress: impl FnMut(usize, usize),
) -> Result<u64, McpError> {
	let mut bytes = 0;
	for (i, c) in plan.copies.iter().enumerate() {
		if let Some(dir) = c.to.parent() {
			std::fs::create_dir_all(dir).map_err(|e| io(dir, e))?;
		}
		// Copy to a temp name first so an interrupted copy never looks like a good file.
		let tmp = c.to.with_extension("collecting");
		bytes += std::fs::copy(&c.from, &tmp).map_err(|e| io(&c.from, e))?;
		std::fs::rename(&tmp, &c.to).map_err(|e| io(&c.to, e))?;
		progress(i + 1, plan.copies.len());
	}
	Ok(bytes)
}

/// The one undoable step pointing `project`'s references at their collected copies.
pub fn relink_command(
	project: &Project,
	mapping: &HashMap<PathBuf, PathBuf>,
	label: &str,
) -> Option<Command> {
	let commands = project.relink_commands(|p| mapping.get(p).cloned());
	(!commands.is_empty()).then(|| Command::Batch {
		label: label.into(),
		commands,
	})
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct CollectFilesArgs {
	/// Also copy the factory samples the project uses (default false: they live in the
	/// shared factory library, which every Cadenza install recreates).
	#[serde(default)]
	pub include_factory: Option<bool>,
	/// Only report what would be copied (default false).
	#[serde(default)]
	pub dry_run: Option<bool>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct CollectFilesOutput {
	pub project_folder: String,
	pub copied: Vec<PlannedCopy>,
	pub bytes_copied: u64,
	/// External files whose content was already in the project (no new copy).
	pub deduplicated: usize,
	pub already_inside: usize,
	pub factory_skipped: usize,
	/// References rewritten (one undo step).
	pub references_rewritten: usize,
	#[serde(skip_serializing_if = "Vec::is_empty")]
	pub missing: Vec<String>,
	pub dry_run: bool,
}

/// Copies every external file the project uses into its folder and rewrites the
/// references in one undo step. The project must have been saved (it needs a folder).
pub fn collect_files(
	session: &mut Session,
	args: CollectFilesArgs,
) -> Result<CollectFilesOutput, McpError> {
	let file = session.path().map(Path::to_path_buf).ok_or_else(|| {
		McpError::Invalid(
			"the project has no folder yet; save it first (save_project with a path), then \
			 collect its files"
				.into(),
		)
	})?;
	let factory = crate::tools::samples::factory_folder();
	let project = session.document().project().clone();
	let plan = plan_collect(
		&project,
		&file,
		args.include_factory.unwrap_or(false),
		&factory,
	)?;
	let dry_run = args.dry_run.unwrap_or(false);
	let mut rewritten = 0;
	let mut bytes = 0;
	if !dry_run {
		bytes = run_copies(&plan, |_, _| {})?;
		if let Some(cmd) = relink_command(&project, &plan.mapping, COLLECT_LABEL) {
			if let Command::Batch { commands, .. } = &cmd {
				rewritten = commands.len();
			}
			session.document_mut().execute(cmd)?;
		}
	}
	Ok(CollectFilesOutput {
		project_folder: ProjectLayout::new(&file).root.display().to_string(),
		bytes_copied: bytes,
		copied: plan.copies,
		deduplicated: plan.deduplicated,
		already_inside: plan.inside,
		factory_skipped: plan.factory_skipped,
		references_rewritten: rewritten,
		missing: plan
			.missing
			.iter()
			.map(|p| p.display().to_string())
			.collect(),
		dry_run,
	})
}

// --- missing files / relink --------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct MissingFile {
	pub path: String,
	/// Original file name (what a search looks for).
	pub name: String,
	/// "audio_clip" or "sample_zone".
	pub used_by: String,
}

fn missing_out(r: &FileRef) -> MissingFile {
	MissingFile {
		path: r.path.display().to_string(),
		name: r.name.clone(),
		used_by: match r.used_by {
			cadenza_model::FileUse::AudioClip { .. } => "audio_clip".into(),
			cadenza_model::FileUse::SampleZone { .. } => "sample_zone".into(),
		},
	}
}

/// Referenced files that are not on disk.
pub fn list_missing_files(session: &Session) -> Vec<MissingFile> {
	session
		.document()
		.project()
		.missing_files()
		.iter()
		.map(missing_out)
		.collect()
}

/// Most files a relink search looks at.
pub const MAX_SEARCH_FILES: usize = 200_000;

/// Files under `root` (recursive, symlinked folders not followed, hidden folders skipped)
/// indexed by lower-case file name.
pub fn index_folder(root: &Path) -> Result<(HashMap<String, Vec<PathBuf>>, usize), McpError> {
	if !root.is_dir() {
		return Err(McpError::Invalid(format!(
			"'{}' is not a folder; pass the folder to search for the missing files",
			root.display()
		)));
	}
	let mut index: HashMap<String, Vec<PathBuf>> = HashMap::new();
	let mut stack = vec![root.to_path_buf()];
	let mut count = 0;
	while let Some(dir) = stack.pop() {
		let Ok(entries) = std::fs::read_dir(&dir) else {
			continue;
		};
		for entry in entries.flatten() {
			let Ok(kind) = entry.file_type() else {
				continue;
			};
			let path = entry.path();
			let name = entry.file_name().to_string_lossy().into_owned();
			if kind.is_dir() {
				if !name.starts_with('.') {
					stack.push(path);
				}
			} else if kind.is_file() || (kind.is_symlink() && path.is_file()) {
				count += 1;
				if count > MAX_SEARCH_FILES {
					return Err(McpError::Invalid(format!(
						"'{}' holds more than {MAX_SEARCH_FILES} files; search a smaller folder",
						root.display()
					)));
				}
				index.entry(name.to_lowercase()).or_default().push(path);
			}
		}
	}
	for paths in index.values_mut() {
		paths.sort();
	}
	Ok((index, count))
}

/// How many trailing folder names `a` and `b` share (ranks same-named candidates).
fn shared_tail(a: &Path, b: &Path) -> usize {
	a.components()
		.rev()
		.zip(b.components().rev())
		.take_while(|(x, y)| x.as_os_str().eq_ignore_ascii_case(y.as_os_str()))
		.count()
}

/// For every missing file, the best match under the indexed folder: same original name
/// (or same file name), preferring candidates whose folders match the old path's.
pub fn find_relinks(
	missing: &[FileRef],
	index: &HashMap<String, Vec<PathBuf>>,
) -> HashMap<PathBuf, PathBuf> {
	let mut out = HashMap::new();
	for r in missing {
		let names = [r.name.to_lowercase(), file_name(&r.path).to_lowercase()];
		let best = names
			.iter()
			.filter_map(|n| index.get(n))
			.flatten()
			.max_by(|a, b| {
				shared_tail(a, &r.path)
					.cmp(&shared_tail(b, &r.path))
					.then_with(|| b.cmp(a))
			});
		if let Some(found) = best {
			out.insert(r.path.clone(), found.clone());
		}
	}
	out
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RelinkFilesArgs {
	/// Folder searched recursively for the missing files (matched by original file name;
	/// when several files share it, the one whose folders match the old path best wins).
	pub search_folder: String,
	/// Only report the matches (default false).
	#[serde(default)]
	pub dry_run: Option<bool>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Relinked {
	pub missing: String,
	pub found: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct RelinkFilesOutput {
	pub relinked: Vec<Relinked>,
	pub still_missing: Vec<MissingFile>,
	pub files_searched: usize,
	pub dry_run: bool,
}

/// Searches a folder for every missing file and relinks all matches in one undo step.
pub fn relink_files(
	session: &mut Session,
	args: RelinkFilesArgs,
) -> Result<RelinkFilesOutput, McpError> {
	let project = session.document().project().clone();
	let missing = project.missing_files();
	let (index, files_searched) = index_folder(Path::new(args.search_folder.trim()))?;
	let mapping = find_relinks(&missing, &index);
	let dry_run = args.dry_run.unwrap_or(false);
	if !dry_run && let Some(cmd) = relink_command(&project, &mapping, RELINK_LABEL) {
		session.document_mut().execute(cmd)?;
	}
	let mut relinked: Vec<Relinked> = mapping
		.iter()
		.map(|(m, f)| Relinked {
			missing: m.display().to_string(),
			found: f.display().to_string(),
		})
		.collect();
	relinked.sort_by(|a, b| a.missing.cmp(&b.missing));
	Ok(RelinkFilesOutput {
		relinked,
		still_missing: missing
			.iter()
			.filter(|r| !mapping.contains_key(&r.path))
			.map(missing_out)
			.collect(),
		files_searched,
		dry_run,
	})
}

/// Missing factory samples (a project made on another machine, or an older factory
/// folder) pointed at this machine's factory folder, when it has a file of that name.
pub fn factory_relinks(project: &Project, factory: &Path) -> HashMap<PathBuf, PathBuf> {
	let mut out = HashMap::new();
	for r in project.missing_files() {
		let parent = r.path.parent().unwrap_or(Path::new(""));
		let from_factory = parent.ends_with("samples/factory") || parent.starts_with(factory);
		if !from_factory {
			continue;
		}
		let local = factory.join(file_name(&r.path));
		if local != r.path && local.is_file() {
			out.insert(r.path.clone(), local);
		}
	}
	out
}

/// Loads a project file for editing: relative paths resolved, factory samples moved to
/// this machine's factory folder (outside the undo history). Returns the document and
/// the files still missing.
pub fn open_document(path: &Path) -> Result<(Document, Vec<FileRef>), McpError> {
	let project = Project::load_json(path)?;
	let mut document = Document::new(project);
	if !document.project().missing_files().is_empty() {
		let factory = cadenza_engine::factory::ensure_factory_samples()
			.unwrap_or_else(|_| crate::tools::samples::factory_folder());
		let mapping = factory_relinks(document.project(), &factory);
		if let Some(cmd) = relink_command(document.project(), &mapping, RELINK_LABEL) {
			document.execute(cmd)?;
			document.clear_history();
		}
	}
	let missing = document.project().missing_files();
	Ok((document, missing))
}

// --- archives ----------------------------------------------------------------------------

/// Whether `path` names a project archive (`.cadenza.zip`, `.cadenzapack`, or `.zip`).
pub fn is_archive(path: &Path) -> bool {
	let name = path
		.file_name()
		.map(|n| n.to_string_lossy().to_lowercase())
		.unwrap_or_default();
	name.ends_with(".zip") || name.ends_with(".cadenzapack")
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct PackOutput {
	pub path: String,
	pub files: usize,
	/// Uncompressed size of the packed files.
	pub bytes: u64,
	/// Referenced files outside the project folder, not in the archive (run collect_files
	/// first to include them).
	#[serde(skip_serializing_if = "Vec::is_empty")]
	pub outside: Vec<String>,
}

/// Zips the folder of `project_file` as `<folder name>/...`, with `project` written fresh
/// (so unsaved edits are included), skipping caches/autosaves (`.cadenza/`), backups and,
/// unless `include_renders`, `Renders/`.
pub fn pack_project(
	project: &Project,
	project_file: &Path,
	out: &Path,
	include_renders: bool,
) -> Result<PackOutput, McpError> {
	let layout = ProjectLayout::new(project_file);
	let top = layout
		.root
		.file_name()
		.map(|n| n.to_string_lossy().into_owned())
		.unwrap_or_else(|| layout.stem());
	let json = project.to_json_string_at(project_file)?;
	if let Some(dir) = out.parent().filter(|d| !d.as_os_str().is_empty()) {
		std::fs::create_dir_all(dir).map_err(|e| io(dir, e))?;
	}
	let tmp = out.with_extension("packing");
	let file = File::create(&tmp).map_err(|e| io(&tmp, e))?;
	let mut zip = zip::ZipWriter::new(file);
	let options = zip::write::SimpleFileOptions::default()
		.compression_method(zip::CompressionMethod::Deflated)
		.large_file(true);
	let zerr = |e: zip::result::ZipError| io(out, e);
	let project_name = file_name(project_file);
	zip.start_file(format!("{top}/{project_name}"), options)
		.map_err(zerr)?;
	zip.write_all(json.as_bytes()).map_err(|e| io(out, e))?;
	let mut files = 1;
	let mut bytes = json.len() as u64;
	let out_abs = std::path::absolute(out).unwrap_or_else(|_| out.to_path_buf());
	let mut stack = vec![layout.root.clone()];
	let mut entries: Vec<PathBuf> = Vec::new();
	while let Some(dir) = stack.pop() {
		let Ok(list) = std::fs::read_dir(&dir) else {
			continue;
		};
		for entry in list.flatten() {
			let path = entry.path();
			let Ok(kind) = entry.file_type() else {
				continue;
			};
			let rel = path.strip_prefix(&layout.root).unwrap_or(&path);
			let first = rel
				.components()
				.next()
				.map(|c| c.as_os_str().to_string_lossy().into_owned())
				.unwrap_or_default();
			if first == CACHE_DIR
				|| (!include_renders && first == cadenza_model::files::RENDERS_DIR)
			{
				continue;
			}
			if kind.is_dir() {
				stack.push(path);
				continue;
			}
			let name = file_name(&path);
			let skip = path == project_file
				|| path == out_abs
				|| path == tmp
				|| name.ends_with(".bak")
				|| name.contains(".tmp-")
				|| name.ends_with(".collecting")
				|| (path.parent() == Some(layout.root.as_path()) && is_archive(&path));
			if kind.is_file() && !skip {
				entries.push(path);
			}
		}
	}
	entries.sort();
	for path in entries {
		let rel = path.strip_prefix(&layout.root).unwrap_or(&path);
		let rel: Vec<String> = rel
			.components()
			.map(|c| c.as_os_str().to_string_lossy().into_owned())
			.collect();
		zip.start_file(format!("{top}/{}", rel.join("/")), options)
			.map_err(zerr)?;
		let mut f = File::open(&path).map_err(|e| io(&path, e))?;
		bytes += std::io::copy(&mut f, &mut zip).map_err(|e| io(&path, e))?;
		files += 1;
	}
	zip.finish().map_err(zerr)?;
	std::fs::rename(&tmp, out).map_err(|e| io(out, e))?;
	let outside = project
		.file_refs()
		.into_iter()
		.filter(|r| !layout.contains(&r.path))
		.map(|r| r.path.display().to_string())
		.collect::<HashSet<_>>()
		.into_iter()
		.collect::<Vec<_>>();
	let mut outside = outside;
	outside.sort();
	Ok(PackOutput {
		path: out.display().to_string(),
		files,
		bytes,
		outside,
	})
}

/// Extracts an archive made by [`pack_project`] (or any zip holding a `.cadenza.json`)
/// into `dest_dir` and returns the project file. Entries that would land outside
/// `dest_dir` are rejected; existing files are never overwritten.
pub fn unpack_project(archive: &Path, dest_dir: &Path) -> Result<PathBuf, McpError> {
	let file = File::open(archive).map_err(|e| io(archive, e))?;
	let zerr = |e: zip::result::ZipError| io(archive, e);
	let mut zip = zip::ZipArchive::new(file).map_err(zerr)?;
	let mut targets = Vec::with_capacity(zip.len());
	for i in 0..zip.len() {
		let entry = zip.by_index(i).map_err(zerr)?;
		let name = entry.enclosed_name().ok_or_else(|| {
			McpError::Invalid(format!(
				"'{}' contains an unsafe path ('{}'); refusing to extract it",
				archive.display(),
				entry.name().map(|n| n.into_owned()).unwrap_or_default()
			))
		})?;
		let dest = dest_dir.join(name);
		if !entry.is_dir() && dest.exists() {
			return Err(McpError::Invalid(format!(
				"'{}' already exists; extract the archive into an empty folder",
				dest.display()
			)));
		}
		targets.push((i, dest, entry.is_dir()));
	}
	let project = targets
		.iter()
		.filter(|(_, p, dir)| {
			!dir && p
				.file_name()
				.is_some_and(|n| n.to_string_lossy().ends_with(PROJECT_SUFFIX))
		})
		.min_by_key(|(_, p, _)| p.components().count())
		.map(|(_, p, _)| p.clone())
		.ok_or_else(|| {
			McpError::Invalid(format!(
				"'{}' holds no {PROJECT_SUFFIX} project file",
				archive.display()
			))
		})?;
	std::fs::create_dir_all(dest_dir).map_err(|e| io(dest_dir, e))?;
	for (i, dest, dir) in targets {
		if dir {
			std::fs::create_dir_all(&dest).map_err(|e| io(&dest, e))?;
			continue;
		}
		if let Some(parent) = dest.parent() {
			std::fs::create_dir_all(parent).map_err(|e| io(parent, e))?;
		}
		let mut entry = zip.by_index(i).map_err(zerr)?;
		let mut out = File::create(&dest).map_err(|e| io(&dest, e))?;
		std::io::copy(&mut entry, &mut out).map_err(|e| io(&dest, e))?;
	}
	Ok(project)
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct PackProjectArgs {
	/// Archive to write; ends in .cadenza.zip (or .cadenzapack).
	pub path: String,
	/// Also pack the Renders/ folder (default false).
	#[serde(default)]
	pub include_renders: Option<bool>,
}

/// Packs the open (saved) project's folder into one archive for sharing.
pub fn pack_current(session: &Session, args: PackProjectArgs) -> Result<PackOutput, McpError> {
	let file = session.path().map(Path::to_path_buf).ok_or_else(|| {
		McpError::Invalid(
			"the project has no folder yet; save it first (save_project), then pack it".into(),
		)
	})?;
	let out = PathBuf::from(args.path.trim());
	if !is_archive(&out) {
		return Err(McpError::Invalid(format!(
			"archive path '{}' must end in .cadenza.zip or .cadenzapack",
			out.display()
		)));
	}
	pack_project(
		session.document().project(),
		&file,
		&out,
		args.include_renders.unwrap_or(false),
	)
}

/// Where new audio for a project goes (`Audio/` of its folder) — `None` when unsaved.
pub fn audio_dir(session_path: Option<&Path>) -> Option<PathBuf> {
	session_path.map(|p| ProjectLayout::new(p).root.join(AUDIO_DIR))
}

/// Where collected samples go (`Samples/` of its folder) — `None` when unsaved.
pub fn samples_dir(session_path: Option<&Path>) -> Option<PathBuf> {
	session_path.map(|p| ProjectLayout::new(p).root.join(SAMPLES_DIR))
}
