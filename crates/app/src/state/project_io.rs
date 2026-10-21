//! Project files in the app: open (projects and archives), recovery, autosave, the
//! session marker, recent projects, collect/relink/pack, and the export job.
//! The decisions live in [`super::safety`], [`super::export`] and the shared MCP
//! `portable` tools; this file runs them off the UI thread and turns results into edits.

use crate::i18n::{tr, trf, trn, trn_with};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime};

use cadenza_engine::{ExportProgress, RenderOptions};
use cadenza_mcp::tools::portable::{self, CollectPlan};
use cadenza_model::{Document, FileRef, Project, ProjectLayout};

use super::export::{RangeContext, RunningExport};
use super::safety::{self, AutosavePolicy, Recovery};
use super::{AppState, Pending};
use crate::jobs::{FileAction, Job};

/// The demo project offered on the start screen.
pub const DEMO_PROJECT: &str = include_str!("../../../../examples/first_light.cadenza.json");

/// What the "Locate missing files" dialog shows.
#[derive(Debug, Clone, Default)]
pub struct RelinkDialog {
	pub missing: Vec<FileRef>,
	/// The last search: folder and how many files it relinked.
	pub last: Option<(PathBuf, usize)>,
}

/// A relink search: the folder and every missing path -> the file found for it.
pub type RelinkFound = (PathBuf, std::collections::HashMap<PathBuf, PathBuf>);

/// Background work on project files.
#[derive(Default)]
pub struct FileJobs {
	pub autosave: Option<Job<Result<PathBuf, String>>>,
	pub collect: Option<Job<Result<(CollectPlan, u64), String>>>,
	pub relink: Option<Job<Result<RelinkFound, String>>>,
	pub pack: Option<Job<Result<portable::PackOutput, String>>>,
	pub unpack: Option<Job<Result<PathBuf, String>>>,
}

impl FileJobs {
	pub fn busy(&self) -> bool {
		self.collect.is_some()
			|| self.relink.is_some()
			|| self.pack.is_some()
			|| self.unpack.is_some()
	}

	/// Labels of the running jobs (busy overlay).
	pub fn labels(&self) -> Vec<String> {
		let secs = |started: Instant| started.elapsed().as_secs_f32();
		[
			self.collect.as_ref().map(|j| {
				trf(
					"Collecting files… {secs} s",
					&[(
						"secs",
						&crate::i18n::decimal(format!("{:.0}", secs(j.started))),
					)],
				)
			}),
			self.relink.as_ref().map(|j| {
				trf(
					"Searching for missing files… {secs} s",
					&[(
						"secs",
						&crate::i18n::decimal(format!("{:.0}", secs(j.started))),
					)],
				)
			}),
			self.pack.as_ref().map(|j| {
				trf(
					"Packing the project… {secs} s",
					&[(
						"secs",
						&crate::i18n::decimal(format!("{:.0}", secs(j.started))),
					)],
				)
			}),
			self.unpack.as_ref().map(|j| {
				trf(
					"Extracting the archive… {secs} s",
					&[(
						"secs",
						&crate::i18n::decimal(format!("{:.0}", secs(j.started))),
					)],
				)
			}),
		]
		.into_iter()
		.flatten()
		.collect()
	}
}

impl AppState {
	// --- Safety -------------------------------------------------------------------

	/// Turns autosaves, the session marker and recovery on (the real app; tests opt in
	/// with their own state folder).
	pub fn enable_safety(&mut self, state_dir: PathBuf) {
		self.safety.enabled = true;
		self.safety.state_dir = state_dir;
		self.take_lock();
	}

	pub fn autosave_policy(&self) -> AutosavePolicy {
		if self.safety.enabled {
			AutosavePolicy::minutes(self.safety.minutes)
		} else {
			AutosavePolicy::off()
		}
	}

	fn autosave_dir(&self) -> PathBuf {
		safety::autosave_dir(self.path(), &self.safety.state_dir, std::process::id())
	}

	/// Writes the marker for the open (saved) project.
	fn take_lock(&mut self) {
		if !self.safety.enabled {
			return;
		}
		self.release_lock();
		if let Some(path) = self.path().map(Path::to_path_buf) {
			let lock = safety::lock_path(Some(&path), &self.safety.state_dir, std::process::id());
			if safety::write_lock(&lock, std::process::id()).is_ok() {
				self.safety.lock = Some(lock);
			}
		}
	}

	fn release_lock(&mut self) {
		if let Some(lock) = self.safety.lock.take() {
			safety::remove_lock(&lock, std::process::id());
		}
	}

	/// The project is going away normally (new/open/quit): marker removed, an unsaved
	/// project's autosaves dropped.
	fn close_files(&mut self) {
		self.release_lock();
		if self.safety.enabled && self.path().is_none() {
			let _ = std::fs::remove_dir_all(safety::untitled_dir(
				&self.safety.state_dir,
				std::process::id(),
			));
		}
		self.safety.autosaver.reset();
	}

	/// The app is quitting.
	pub fn shutdown(&mut self) {
		self.close_files();
	}

	/// "Don't save": the autosaves of the changes being thrown away go too.
	pub fn discard_changes(&mut self) {
		if self.safety.enabled {
			safety::clear_autosaves(&self.autosave_dir());
		}
		self.dirty = false;
	}

	/// Writes an autosave on a worker when one is due (never blocks the UI or audio).
	pub(super) fn tick_autosave(&mut self, now: Instant) {
		if let Some(r) = self.files_jobs.autosave.as_ref().and_then(Job::poll) {
			self.files_jobs.autosave = None;
			match r {
				Some(Ok(_)) => {}
				Some(Err(e)) if !self.safety.warned => {
					self.safety.warned = true;
					self.notices.warn(trf("Autosave failed: {e}", &[("e", &e)]));
				}
				_ => {}
			}
		}
		if self.files_jobs.autosave.is_some() {
			return;
		}
		let policy = self.autosave_policy();
		if !self
			.safety
			.autosaver
			.due(now, self.session_key, self.dirty, &policy)
		{
			return;
		}
		self.safety.autosaver.written(self.session_key);
		let project = self.project().clone();
		let dir = self.autosave_dir();
		self.files_jobs.autosave = Some(Job::spawn("autosave", None, move || {
			safety::write_autosave(&dir, &project, policy.keep, SystemTime::now())
		}));
	}

	/// Waits for a running autosave (tests).
	pub fn wait_autosave(&mut self) -> Option<Result<PathBuf, String>> {
		let job = self.files_jobs.autosave.take()?;
		let deadline = Instant::now() + std::time::Duration::from_secs(10);
		loop {
			if let Some(r) = job.poll() {
				return r;
			}
			if Instant::now() > deadline {
				return None;
			}
			std::thread::sleep(std::time::Duration::from_millis(2));
		}
	}

	/// Unsaved work of a crashed untitled session, offered at startup without a project.
	pub fn check_untitled_recovery(&mut self) {
		if !self.safety.enabled {
			return;
		}
		self.recovery = safety::untitled_recoveries(
			&self.safety.state_dir,
			std::process::id(),
			safety::pid_alive,
		)
		.into_iter()
		.next();
	}

	/// Restores the offered autosave as the (unsaved) document.
	pub fn recover(&mut self) -> bool {
		let Some(r) = self.recovery.take() else {
			return false;
		};
		match Project::load_json(&r.autosave) {
			Ok(project) => {
				self.stop();
				self.close_files();
				self.load_document(Document::new(project), r.project.clone());
				self.reset_view();
				self.session().mark_unsaved();
				self.sync_session(true);
				self.dirty = true;
				self.view.start_screen = false;
				self.take_lock();
				if let Some(dir) = &r.cleanup {
					let _ = std::fs::remove_dir_all(dir);
				}
				self.notices.info(tr(
					"Recovered the autosaved work — save to keep it (Ctrl+S)",
				));
				true
			}
			Err(e) => {
				self.notices.error(trf(
					"Cannot read the autosave {r}: {e}",
					&[("r", &r.autosave.display()), ("e", &e)],
				));
				false
			}
		}
	}

	/// Declines the recovery (the autosaves are deleted).
	pub fn dismiss_recovery(&mut self) {
		if let Some(r) = self.recovery.take() {
			match (&r.cleanup, &r.project) {
				(Some(dir), _) => {
					let _ = std::fs::remove_dir_all(dir);
				}
				(None, Some(p)) => safety::clear_autosaves(&ProjectLayout::new(p).autosave_dir()),
				_ => {}
			}
		}
	}

	// --- Open / new / save hooks ----------------------------------------------------

	/// Opens a project file or a project archive (asks where to extract it first).
	pub fn open(&mut self, path: &Path) -> bool {
		if portable::is_archive(path) {
			self.pending_archive = Some(path.to_path_buf());
			self.file_dialog(FileAction::UnpackFolder);
			return true;
		}
		match portable::open_document(path) {
			Ok((document, missing)) => {
				let recovery = self
					.safety
					.enabled
					.then(|| safety::recovery_for(path, std::process::id(), safety::pid_alive))
					.flatten();
				self.stop();
				self.close_files();
				self.load_document(document, Some(path.to_path_buf()));
				self.reset_view();
				self.notices
					.info(trf("Opened {path}", &[("path", &path.display())]));
				safety::push_recent(&mut self.recent, path);
				self.take_lock();
				self.view.start_screen = false;
				self.relink = (!missing.is_empty()).then_some(RelinkDialog {
					missing,
					last: None,
				});
				self.recovery = recovery;
				true
			}
			Err(e) => {
				self.notices.error(trf(
					"Cannot open {path}: {e}",
					&[("path", &path.display()), ("e", &e)],
				));
				self.recent.retain(|p| p != path);
				false
			}
		}
	}

	pub fn new_project(&mut self) {
		self.stop();
		self.close_files();
		self.load_document(Document::new(Project::new("Untitled")), None);
		self.reset_view();
		self.view.start_screen = false;
		self.notices.info(tr("New project"));
	}

	/// Opens the bundled demo song (written to the user's state folder first).
	pub fn open_demo(&mut self) -> bool {
		let file = self
			.safety
			.state_dir
			.join("demo")
			.join("First Light")
			.join("First Light.cadenza.json");
		let written = Project::from_json_str(DEMO_PROJECT)
			.and_then(|p| p.save_json(&file))
			.map_err(|e| e.to_string());
		match written {
			Ok(()) => self.open(&file),
			Err(e) => {
				self.notices
					.error(trf("Cannot write the demo project: {e}", &[("e", &e)]));
				false
			}
		}
	}

	/// After a successful save to `path` (`was` = the path before).
	pub(super) fn after_save(&mut self, was: Option<PathBuf>) {
		let Some(path) = self.path().map(Path::to_path_buf) else {
			return;
		};
		safety::push_recent(&mut self.recent, &path);
		self.start.invalidate(&path);
		self.safety.autosaver.written(self.session_key);
		if self.safety.enabled {
			safety::clear_autosaves(&self.autosave_dir());
			if was.is_none() {
				let _ = std::fs::remove_dir_all(safety::untitled_dir(
					&self.safety.state_dir,
					std::process::id(),
				));
			}
		}
		if was.as_deref() != Some(path.as_path()) {
			self.take_lock();
			self.export.reset_project();
			let layout = ProjectLayout::new(&path);
			let outside = self
				.project()
				.file_refs()
				.iter()
				.any(|r| !layout.contains(&r.path));
			// "Save As" into a new folder: offer to bring the files along.
			self.collect_prompt = outside;
		}
	}

	// --- Collect / relink / pack ----------------------------------------------------

	/// Copies external files into the project folder on a worker; the references are
	/// rewritten in one undo step when it finishes.
	pub fn collect_files(&mut self) {
		self.collect_prompt = false;
		let Some(path) = self.path().map(Path::to_path_buf) else {
			self.notices.warn(tr(
				"Save the project first: collecting copies files into its folder",
			));
			self.after_save = Some(Pending::Collect);
			self.file_dialog(FileAction::SaveAs);
			return;
		};
		if self.files_jobs.collect.is_some() {
			return;
		}
		let project = self.project().clone();
		let factory = cadenza_mcp::tools::samples::factory_folder();
		self.files_jobs.collect = Some(Job::spawn("collect", self.waker.clone(), move || {
			let plan = portable::plan_collect(&project, &path, false, &factory)
				.map_err(|e| e.to_string())?;
			let bytes = portable::run_copies(&plan, |_, _| {}).map_err(|e| e.to_string())?;
			Ok((plan, bytes))
		}));
	}

	/// Searches `folder` for the missing files on a worker; matches are relinked at once.
	pub fn search_missing(&mut self, folder: PathBuf) {
		if self.files_jobs.relink.is_some() {
			return;
		}
		let missing = self.project().missing_files();
		self.files_jobs.relink = Some(Job::spawn("relink", self.waker.clone(), move || {
			let (index, _) = portable::index_folder(&folder).map_err(|e| e.to_string())?;
			Ok((folder, portable::find_relinks(&missing, &index)))
		}));
	}

	/// Opens the "Locate missing files" dialog with the current missing files.
	pub fn open_relink(&mut self) {
		self.relink = Some(RelinkDialog {
			missing: self.project().missing_files(),
			last: None,
		});
	}

	/// Writes the project archive on a worker.
	pub fn pack_to(&mut self, out: PathBuf) {
		let Some(path) = self.path().map(Path::to_path_buf) else {
			self.notices
				.warn(tr("Save the project first: the archive packs its folder"));
			return;
		};
		let out = if portable::is_archive(&out) {
			out
		} else {
			let mut s = out.into_os_string();
			s.push(".cadenza.zip");
			PathBuf::from(s)
		};
		let project = self.project().clone();
		self.files_jobs.pack = Some(Job::spawn("pack", self.waker.clone(), move || {
			portable::pack_project(&project, &path, &out, false).map_err(|e| e.to_string())
		}));
	}

	fn unpack_into(&mut self, folder: PathBuf) {
		let Some(archive) = self.pending_archive.take() else {
			return;
		};
		self.files_jobs.unpack = Some(Job::spawn("unpack", self.waker.clone(), move || {
			portable::unpack_project(&archive, &folder).map_err(|e| e.to_string())
		}));
	}

	/// Dialog results for the file actions of this module.
	pub(super) fn finish_file_action(&mut self, action: FileAction, path: PathBuf) {
		match action {
			FileAction::ExportFolder => self.export.folder = Some(path),
			FileAction::UnpackFolder => self.unpack_into(path),
			FileAction::RelinkFolder => self.search_missing(path),
			FileAction::PackArchive => self.pack_to(path),
			_ => {}
		}
	}

	pub(super) fn poll_file_jobs(&mut self) {
		if let Some(r) = self.files_jobs.collect.as_ref().and_then(Job::poll) {
			self.files_jobs.collect = None;
			match r {
				Some(Ok((plan, bytes))) => {
					let cmd = portable::relink_command(
						self.project(),
						&plan.mapping,
						portable::COLLECT_LABEL,
					);
					let n = plan.copies.len();
					if let Some(cmd) = cmd {
						self.apply_or_queue(cmd);
					}
					let mut text = trn_with(
						"Collected {n} file ({size})",
						"Collected {n} files ({size})",
						n,
						&[("size", &super::export::human_bytes(bytes))],
					);
					if plan.deduplicated > 0 {
						text.push_str(&trn(
							", {n} duplicate shared",
							", {n} duplicates shared",
							plan.deduplicated,
						));
					}
					if !plan.missing.is_empty() {
						text.push_str(&trf(" — {n} missing", &[("n", &plan.missing.len())]));
					}
					if n > 0 || plan.deduplicated > 0 {
						text.push_str(tr("; save to keep the new references"));
					}
					self.notices.info(text);
				}
				Some(Err(e)) => self.notices.error(trf("Collect failed: {e}", &[("e", &e)])),
				None => self.notices.error(tr("Collect failed: the worker stopped")),
			}
		}
		if let Some(r) = self.files_jobs.relink.as_ref().and_then(Job::poll) {
			self.files_jobs.relink = None;
			match r {
				Some(Ok((folder, mapping))) => {
					let n = mapping.len();
					if let Some(cmd) =
						portable::relink_command(self.project(), &mapping, portable::RELINK_LABEL)
					{
						self.apply_or_queue(cmd);
					}
					let missing = self.project().missing_files();
					let dialog = self.relink.get_or_insert_with(RelinkDialog::default);
					dialog.missing = missing;
					dialog.last = Some((folder, n));
					self.notices
						.info(trn("Relinked {n} file", "Relinked {n} files", n));
				}
				Some(Err(e)) => self.notices.error(trf("Search failed: {e}", &[("e", &e)])),
				None => self.notices.error(tr("Search failed: the worker stopped")),
			}
		}
		if let Some(r) = self.files_jobs.pack.as_ref().and_then(Job::poll) {
			self.files_jobs.pack = None;
			match r {
				Some(Ok(out)) => {
					self.notices.info(trn_with(
						"Wrote {path} ({n} file, {size})",
						"Wrote {path} ({n} files, {size})",
						out.files,
						&[
							("path", &out.path),
							("size", &super::export::human_bytes(out.bytes)),
						],
					));
					if !out.outside.is_empty() {
						self.notices.warn(trn(
							"{n} file outside the project folder is not in the archive — use File → \
							 Collect Project Files first",
							"{n} files outside the project folder are not in the archive — use File → \
							 Collect Project Files first",
							out.outside.len(),
						));
					}
				}
				Some(Err(e)) => self.notices.error(trf("Archive failed: {e}", &[("e", &e)])),
				None => self.notices.error(tr("Archive failed: the worker stopped")),
			}
		}
		if let Some(r) = self.files_jobs.unpack.as_ref().and_then(Job::poll) {
			self.files_jobs.unpack = None;
			match r {
				Some(Ok(project)) => {
					self.open(&project);
				}
				Some(Err(e)) => self
					.notices
					.error(trf("Cannot extract the archive: {e}", &[("e", &e)])),
				None => self
					.notices
					.error(tr("Extracting failed: the worker stopped")),
			}
		}
		self.poll_export();
	}

	// --- Export ----------------------------------------------------------------------

	/// File → Export (Ctrl+E): opens the export dialog.
	pub fn open_export_dialog(&mut self) {
		let project = self.project().clone();
		let path = self.path().map(Path::to_path_buf);
		self.export.open_for(&project, path.as_deref());
	}

	/// The loop region and the selected clips' span, for the range choices.
	pub fn export_ranges(&self) -> RangeContext {
		let t = &self.view.transport;
		let selection = self
			.selection
			.clips
			.iter()
			.filter_map(|c| {
				self.project()
					.find_clip(*c)
					.map(|(_, c)| (c.start, c.end()))
			})
			.reduce(|a, b| (a.0.min(b.0), a.1.max(b.1)));
		RangeContext {
			loop_range: t.loop_range,
			selection,
		}
	}

	/// Starts the export the dialog describes on a worker thread.
	pub fn start_export(&mut self) -> bool {
		if self.export.running.is_some() {
			return false;
		}
		let spec = match self.export.spec(self.project(), self.export_ranges()) {
			Ok(s) => s,
			Err(e) => {
				self.export.result = Some(Err(e));
				return false;
			}
		};
		let project = self.project().clone();
		let (plugins, hosting) = {
			let mut session = self.session();
			(
				session.plugins_for_project(),
				session.plugin_hosting().clone(),
			)
		};
		let opts = RenderOptions {
			sample_rate: match self.audio_status {
				super::AudioStatus::Ready { sample_rate } => sample_rate,
				_ => RenderOptions::default().sample_rate,
			},
			plugins,
			hosting,
			..RenderOptions::default()
		};
		let progress = Arc::new(Mutex::new(ExportProgress {
			fraction: 0.0,
			stage: "Starting".into(),
		}));
		let cancel = Arc::new(AtomicBool::new(false));
		let (p, c, waker) = (progress.clone(), cancel.clone(), self.waker.clone());
		let job = Job::spawn("export", self.waker.clone(), move || {
			let mut last = Instant::now();
			cadenza_engine::export_project(&project, &opts, &spec, &mut |pr| {
				if let Ok(mut g) = p.lock() {
					*g = pr.clone();
				}
				if last.elapsed().as_millis() > 50 {
					last = Instant::now();
					if let Some(w) = &waker {
						w();
					}
				}
				!c.load(std::sync::atomic::Ordering::Relaxed)
			})
			.map_err(|e| e.to_string())
		});
		self.export.result = None;
		self.export.running = Some(RunningExport {
			job,
			progress,
			cancel,
		});
		true
	}

	fn poll_export(&mut self) {
		let Some(r) = self.export.running.as_ref().and_then(|r| r.job.poll()) else {
			return;
		};
		self.export.running = None;
		let result = r.unwrap_or_else(|| Err(tr("the export thread stopped").into()));
		match &result {
			Ok(report) => {
				for w in &report.mix.warnings {
					self.notices.warn(w.clone());
				}
				self.notices.info(trn_with(
					"Exported {n} file ({size})",
					"Exported {n} files ({size})",
					report.files.len(),
					&[("size", &super::export::human_bytes(report.bytes()))],
				));
			}
			Err(e) if e.contains("cancelled") => self.notices.info(tr("Export cancelled")),
			Err(e) => self.notices.error(trf("Export failed: {e}", &[("e", &e)])),
		}
		self.export.result = Some(result);
		self.export.open = true;
	}

	/// The recovery being offered, if any.
	pub fn recovery(&self) -> Option<&Recovery> {
		self.recovery.as_ref()
	}
}
