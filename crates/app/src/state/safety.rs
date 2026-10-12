//! Safety nets: periodic autosaves, a "session running" marker to detect crashes,
//! crash/autosave recovery, and the recent projects list. Pure logic over paths and
//! explicit times (tests drive it with a fake clock); [`super::AppState`] runs the I/O on
//! a worker thread.
//!
//! Autosaves of a saved project go to `<project folder>/.cadenza/autosave/`, those of an
//! unsaved one to `$XDG_STATE_HOME/cadenza/autosave/untitled-<pid>/`. They are rotating
//! (`keep` newest), written atomically with absolute paths. The marker
//! `.cadenza/session.lock` (or `session.lock` in the untitled folder) holds the pid of the
//! app editing the project; it is removed when the project is closed normally.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use cadenza_mcp::ChangeKey;
use cadenza_model::{Project, ProjectLayout};

/// Default autosave interval.
pub const DEFAULT_AUTOSAVE_MINUTES: u32 = 2;
/// Autosaves kept per project.
pub const AUTOSAVES_KEPT: usize = 5;
/// Entries in File → Open Recent.
pub const RECENT_MAX: usize = 10;
/// Marker file name.
pub const LOCK_FILE: &str = "session.lock";
const AUTOSAVE_PREFIX: &str = "autosave-";

/// When autosaves are written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AutosavePolicy {
	/// `None` = off.
	pub interval: Option<Duration>,
	pub keep: usize,
}

impl AutosavePolicy {
	pub fn minutes(minutes: u32) -> Self {
		Self {
			interval: (minutes > 0).then(|| Duration::from_secs(u64::from(minutes) * 60)),
			keep: AUTOSAVES_KEPT,
		}
	}

	pub fn off() -> Self {
		Self {
			interval: None,
			keep: AUTOSAVES_KEPT,
		}
	}
}

/// Decides when the next autosave is due: `interval` after the first unsaved change since
/// the last save/autosave, and only when something changed.
#[derive(Debug, Clone, Default)]
pub struct Autosaver {
	/// The state last written (autosave or save).
	saved: Option<ChangeKey>,
	/// When the current unsaved changes started to count.
	since: Option<Instant>,
}

impl Autosaver {
	/// Whether an autosave of state `key` is due at `now`.
	pub fn due(
		&mut self,
		now: Instant,
		key: ChangeKey,
		dirty: bool,
		policy: &AutosavePolicy,
	) -> bool {
		let Some(interval) = policy.interval else {
			return false;
		};
		if !dirty || self.saved == Some(key) {
			self.since = None;
			return false;
		}
		let since = *self.since.get_or_insert(now);
		now.saturating_duration_since(since) >= interval
	}

	/// An autosave (or a save) of `key` was written.
	pub fn written(&mut self, key: ChangeKey) {
		self.saved = Some(key);
		self.since = None;
	}

	/// A new document: forget everything.
	pub fn reset(&mut self) {
		*self = Self::default();
	}
}

/// `$XDG_STATE_HOME/cadenza` (or `~/.local/state/cadenza`).
pub fn state_dir() -> PathBuf {
	std::env::var_os("XDG_STATE_HOME")
		.map(PathBuf::from)
		.or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))
		.unwrap_or_else(std::env::temp_dir)
		.join("cadenza")
}

/// Where unsaved projects of this process autosave.
pub fn untitled_dir(state: &Path, pid: u32) -> PathBuf {
	state.join("autosave").join(format!("untitled-{pid}"))
}

/// The autosave folder of a project (`None` path = unsaved, under `state`).
pub fn autosave_dir(project: Option<&Path>, state: &Path, pid: u32) -> PathBuf {
	match project {
		Some(p) => ProjectLayout::new(p).autosave_dir(),
		None => untitled_dir(state, pid),
	}
}

/// The marker of a project (`None` = unsaved, in its untitled folder).
pub fn lock_path(project: Option<&Path>, state: &Path, pid: u32) -> PathBuf {
	match project {
		Some(p) => ProjectLayout::new(p).cache_dir().join(LOCK_FILE),
		None => untitled_dir(state, pid).join(LOCK_FILE),
	}
}

/// Writes the "session running" marker (pid inside).
pub fn write_lock(path: &Path, pid: u32) -> std::io::Result<()> {
	if let Some(dir) = path.parent() {
		std::fs::create_dir_all(dir)?;
	}
	std::fs::write(path, pid.to_string())
}

/// Removes the marker if it is ours.
pub fn remove_lock(path: &Path, pid: u32) {
	if read_lock(path) == Some(pid) {
		let _ = std::fs::remove_file(path);
	}
}

pub fn read_lock(path: &Path) -> Option<u32> {
	std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

/// Whether process `pid` is running (Linux: `/proc/<pid>`; elsewhere assumed not, so a
/// stale marker counts as a crash).
pub fn pid_alive(pid: u32) -> bool {
	cfg!(target_os = "linux") && Path::new("/proc").join(pid.to_string()).exists()
}

/// Autosaves in `dir`, newest first.
pub fn autosaves(dir: &Path) -> Vec<(PathBuf, SystemTime)> {
	let Ok(entries) = std::fs::read_dir(dir) else {
		return Vec::new();
	};
	let mut list: Vec<(PathBuf, SystemTime)> = entries
		.flatten()
		.filter(|e| {
			let n = e.file_name().to_string_lossy().into_owned();
			n.starts_with(AUTOSAVE_PREFIX) && n.ends_with(".json")
		})
		.filter_map(|e| {
			let modified = e.metadata().ok()?.modified().ok()?;
			Some((e.path(), modified))
		})
		.collect();
	list.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| b.0.cmp(&a.0)));
	list
}

/// Writes `project` as a new autosave in `dir` (named after `now`) and deletes all but the
/// `keep` newest. Runs on a worker thread.
pub fn write_autosave(
	dir: &Path,
	project: &Project,
	keep: usize,
	now: SystemTime,
) -> Result<PathBuf, String> {
	let millis = now
		.duration_since(SystemTime::UNIX_EPOCH)
		.map_or(0, |d| d.as_millis());
	let path = dir.join(format!("{AUTOSAVE_PREFIX}{millis:015}.cadenza.json"));
	project
		.save_json_absolute(&path)
		.map_err(|e| e.to_string())?;
	for (old, _) in autosaves(dir).into_iter().skip(keep.max(1)) {
		let _ = std::fs::remove_file(old);
	}
	Ok(path)
}

/// Deletes every autosave in `dir` (after a save, or when the user discards changes).
pub fn clear_autosaves(dir: &Path) {
	for (p, _) in autosaves(dir) {
		let _ = std::fs::remove_file(p);
	}
}

/// Why recovery is offered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryReason {
	/// The marker of a process that is gone: Cadenza crashed or was killed.
	Crashed,
	/// An autosave is newer than the saved project.
	Newer,
}

/// Unsaved work that can be restored.
#[derive(Debug, Clone, PartialEq)]
pub struct Recovery {
	pub autosave: PathBuf,
	pub modified: SystemTime,
	pub reason: RecoveryReason,
	/// The project the autosave belongs to (`None` = an unsaved project).
	pub project: Option<PathBuf>,
	/// Folder to delete once handled (untitled sessions).
	pub cleanup: Option<PathBuf>,
}

/// Recovery for opening `project`: its newest autosave when it is newer than the project
/// file. `alive` says whether a marker's pid still runs (another instance editing it).
pub fn recovery_for(project: &Path, own_pid: u32, alive: impl Fn(u32) -> bool) -> Option<Recovery> {
	let layout = ProjectLayout::new(project);
	let (autosave, modified) = autosaves(&layout.autosave_dir()).into_iter().next()?;
	let saved = std::fs::metadata(project).and_then(|m| m.modified()).ok();
	if saved.is_some_and(|s| s >= modified) {
		return None;
	}
	let lock = read_lock(&layout.cache_dir().join(LOCK_FILE));
	if lock.is_some_and(|pid| pid != own_pid && alive(pid)) {
		return None;
	}
	Some(Recovery {
		autosave,
		modified,
		reason: if lock.is_some() {
			RecoveryReason::Crashed
		} else {
			RecoveryReason::Newer
		},
		project: Some(project.to_path_buf()),
		cleanup: None,
	})
}

/// Unsaved projects of processes that are gone (crashed while untitled), newest first.
pub fn untitled_recoveries(
	state: &Path,
	own_pid: u32,
	alive: impl Fn(u32) -> bool,
) -> Vec<Recovery> {
	let Ok(entries) = std::fs::read_dir(state.join("autosave")) else {
		return Vec::new();
	};
	let mut out: Vec<Recovery> = entries
		.flatten()
		.filter_map(|e| {
			let name = e.file_name().to_string_lossy().into_owned();
			let pid: u32 = name.strip_prefix("untitled-")?.parse().ok()?;
			if pid == own_pid || alive(pid) {
				return None;
			}
			let dir = e.path();
			let (autosave, modified) = autosaves(&dir).into_iter().next()?;
			Some(Recovery {
				autosave,
				modified,
				reason: RecoveryReason::Crashed,
				project: None,
				cleanup: Some(dir),
			})
		})
		.collect();
	out.sort_by_key(|a| std::cmp::Reverse(a.modified));
	out
}

/// Moves `path` to the front of `recent` (deduplicated, at most [`RECENT_MAX`]).
pub fn push_recent(recent: &mut Vec<PathBuf>, path: &Path) {
	let path = path
		.canonicalize()
		.or_else(|_| std::path::absolute(path))
		.unwrap_or_else(|_| path.to_path_buf());
	recent.retain(|p| *p != path);
	recent.insert(0, path);
	recent.truncate(RECENT_MAX);
}

/// "2 min ago", "3 h ago", "yesterday"... for recovery prompts.
pub fn age(modified: SystemTime, now: SystemTime) -> String {
	let secs = now.duration_since(modified).map_or(0, |d| d.as_secs());
	match secs {
		0..60 => "just now".into(),
		60..3600 => format!("{} min ago", secs / 60),
		3600..86_400 => format!("{} h ago", secs / 3600),
		86_400..172_800 => "yesterday".into(),
		_ => format!("{} days ago", secs / 86_400),
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn set_mtime(path: &Path, t: SystemTime) {
		let f = std::fs::File::options().write(true).open(path).unwrap();
		f.set_modified(t).unwrap();
	}

	#[test]
	fn autosaves_are_due_after_the_interval_of_unsaved_changes_only() {
		let policy = AutosavePolicy::minutes(2);
		let mut a = Autosaver::default();
		let t0 = Instant::now();
		let min = Duration::from_secs(60);
		assert!(!a.due(t0, (1, 1), false, &policy), "clean: never");
		assert!(
			!a.due(t0, (1, 2), true, &policy),
			"clock starts at the first change"
		);
		assert!(!a.due(t0 + min, (1, 3), true, &policy));
		assert!(a.due(t0 + 2 * min, (1, 4), true, &policy));
		a.written((1, 4));
		assert!(!a.due(t0 + 10 * min, (1, 4), true, &policy), "nothing new");
		assert!(!a.due(t0 + 10 * min, (1, 5), true, &policy));
		assert!(a.due(t0 + 12 * min, (1, 5), true, &policy));
		assert!(!a.due(t0 + 60 * min, (1, 6), true, &AutosavePolicy::off()));
		a.written((1, 6));
		assert!(!a.due(t0, (1, 6), false, &policy));
	}

	#[test]
	fn autosaves_rotate_and_recovery_needs_a_newer_autosave() {
		let tmp = tempfile::tempdir().unwrap();
		let file = tmp.path().join("Song").join("Song.cadenza.json");
		let mut p = Project::new("Song");
		p.save_json(&file).unwrap();
		let base = SystemTime::now() - Duration::from_secs(3600);
		set_mtime(&file, base);
		let dir = autosave_dir(Some(&file), tmp.path(), 1);
		assert_eq!(dir, file.parent().unwrap().join(".cadenza/autosave"));
		assert_eq!(recovery_for(&file, 1, |_| false), None, "no autosave");

		for i in 0..7u64 {
			p.name = format!("v{i}");
			let at = base + Duration::from_secs(60 * (i + 1));
			let path = write_autosave(&dir, &p, AUTOSAVES_KEPT, at).unwrap();
			set_mtime(&path, at);
		}
		let list = autosaves(&dir);
		assert_eq!(list.len(), AUTOSAVES_KEPT, "rotating");
		assert_eq!(Project::load_json(&list[0].0).unwrap().name, "v6");

		let r = recovery_for(&file, 1, |_| false).unwrap();
		assert_eq!(r.reason, RecoveryReason::Newer);
		assert_eq!(r.autosave, list[0].0);

		// A marker of a dead process: crashed. Of a live other process: still editing.
		let lock = lock_path(Some(&file), tmp.path(), 1);
		write_lock(&lock, 4242).unwrap();
		assert_eq!(
			recovery_for(&file, 1, |_| false).unwrap().reason,
			RecoveryReason::Crashed
		);
		assert_eq!(recovery_for(&file, 1, |pid| pid == 4242), None);
		remove_lock(&lock, 1);
		assert!(lock.exists(), "only our own marker is removed");
		remove_lock(&lock, 4242);
		assert!(!lock.exists());

		// Saving after the autosaves: nothing to recover.
		p.save_json(&file).unwrap();
		assert_eq!(recovery_for(&file, 1, |_| false), None);
		clear_autosaves(&dir);
		assert!(autosaves(&dir).is_empty());
	}

	#[test]
	fn untitled_sessions_of_dead_processes_are_recoverable() {
		let tmp = tempfile::tempdir().unwrap();
		let state = tmp.path();
		let p = Project::new("Sketch");
		for pid in [10, 11, 12] {
			let dir = untitled_dir(state, pid);
			write_autosave(&dir, &p, 5, SystemTime::now()).unwrap();
			write_lock(&dir.join(LOCK_FILE), pid).unwrap();
		}
		let found = untitled_recoveries(state, 12, |pid| pid == 11);
		assert_eq!(found.len(), 1, "own and live sessions are skipped");
		assert_eq!(found[0].cleanup, Some(untitled_dir(state, 10)));
		assert_eq!(found[0].project, None);
	}

	#[test]
	fn recent_projects_are_deduplicated_and_bounded() {
		let mut recent = Vec::new();
		for i in 0..12 {
			push_recent(&mut recent, Path::new(&format!("/p/{i}.cadenza.json")));
		}
		push_recent(&mut recent, Path::new("/p/5.cadenza.json"));
		assert_eq!(recent.len(), RECENT_MAX);
		assert_eq!(recent[0], PathBuf::from("/p/5.cadenza.json"));
		assert_eq!(
			recent
				.iter()
				.filter(|p| p.ends_with("5.cadenza.json"))
				.count(),
			1
		);
		let now = SystemTime::now();
		assert_eq!(age(now - Duration::from_secs(150), now), "2 min ago");
	}
}
