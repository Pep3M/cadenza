//! Update check and self-update (Help → Check for updates…, the status-bar chip, the
//! update window). The work is `cadenza-update` on worker threads; this holds what the UI
//! shows and the per-user preferences. Never modal, never on the audio thread.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use cadenza_update::{
	CURRENT_VERSION, Fetcher, HttpFetcher, InstallKind, ManualReason, Prepared, Progress, Release,
	UpdateError, Version,
};
use serde::{Deserialize, Serialize};

use crate::audio::Waker;
use crate::i18n::{tr, trf};
use crate::jobs::Job;

use super::notices::Notices;

/// Automatic checks run at most this often (seconds).
pub const CHECK_INTERVAL_SECS: u64 = 24 * 60 * 60;

/// Saved in `Prefs.updates`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UpdatePrefs {
	/// Check at startup (at most once per [`CHECK_INTERVAL_SECS`]).
	pub auto_check: bool,
	/// Unix seconds of the last check (0 = never).
	pub last_check: u64,
	/// The newest release seen that is newer than the running build (empty = none), so
	/// the chip survives restarts between checks.
	pub latest: String,
	/// A version the user chose to skip: no chip or notice for it.
	pub skipped: String,
	/// Why the update staged for the last quit could not be put in place (shown once at
	/// the next start).
	pub install_error: String,
}

impl Default for UpdatePrefs {
	fn default() -> Self {
		Self {
			auto_check: true,
			last_check: 0,
			latest: String::new(),
			skipped: String::new(),
			install_error: String::new(),
		}
	}
}

impl UpdatePrefs {
	/// Whether the startup check runs (a clock set back counts as due).
	pub fn check_due(&self, now: u64) -> bool {
		self.auto_check
			&& (self.last_check == 0
				|| now < self.last_check
				|| now - self.last_check >= CHECK_INTERVAL_SECS)
	}

	/// The version the status-bar chip offers, if any.
	pub fn offered(&self, current: &str) -> Option<&str> {
		let latest = Version::parse(&self.latest)?;
		let newer = Version::parse(current).is_none_or(|c| latest > c);
		(newer && self.latest != self.skipped).then_some(self.latest.as_str())
	}
}

/// The user-facing advice for installs that cannot update themselves.
pub fn manual_advice(reason: ManualReason) -> &'static str {
	match reason {
		ManualReason::Development => {
			tr("This is a development build: update it with git and cargo.")
		}
		ManualReason::PackageManager => {
			tr("This copy is managed by a package manager: update it with that package manager.")
		}
		ManualReason::ReadOnly => tr(
			"The install folder cannot be written by this user: update Cadenza the way it was installed.",
		),
		ManualReason::Unmanaged => tr(
			"This copy was not installed from a release package: download the new version from the release page.",
		),
		ManualReason::Unsupported => tr(
			"There are no release builds for this system yet: build the new version from source.",
		),
	}
}

type CheckJob = Job<Result<Release, String>>;
type InstallJob = Job<Result<Prepared, UpdateError>>;

#[derive(Default)]
pub struct UpdateState {
	pub prefs: UpdatePrefs,
	/// The update window is open.
	pub window: bool,
	/// The latest release from the last check.
	pub release: Option<Release>,
	/// The last check or download failure (shown in the window).
	pub error: Option<String>,
	/// A verified update waiting for the restart (or the installer run).
	pub ready: Option<Prepared>,
	check: Option<(bool, CheckJob)>,
	download: Option<(Arc<Progress>, InstallJob)>,
	kind: Option<InstallKind>,
	/// Launch `ready` once the app has quit.
	relaunch: bool,
	/// Tests inject a fake; `None` = HTTPS to GitHub.
	fetcher: Option<Arc<dyn Fetcher>>,
}

impl UpdateState {
	#[cfg(test)]
	pub(crate) fn with_fetcher(fetcher: Arc<dyn Fetcher>, kind: InstallKind) -> Self {
		Self {
			fetcher: Some(fetcher),
			kind: Some(kind),
			..Self::default()
		}
	}

	/// How this copy was installed (detected once).
	pub fn kind(&mut self) -> &InstallKind {
		self.kind
			.get_or_insert_with(|| match cadenza_update::Context::current() {
				Ok(ctx) => cadenza_update::detect(&ctx, &cadenza_update::RealProbe),
				Err(_) => InstallKind::Manual(ManualReason::Unmanaged),
			})
	}

	fn fetcher(&self) -> Result<Arc<dyn Fetcher>, UpdateError> {
		match &self.fetcher {
			Some(f) => Ok(f.clone()),
			None => Ok(Arc::new(HttpFetcher::new()?)),
		}
	}

	pub fn checking(&self) -> bool {
		self.check.is_some()
	}

	pub fn downloading(&self) -> Option<&Progress> {
		self.download.as_ref().map(|(p, _)| p.as_ref())
	}

	pub fn busy(&self) -> bool {
		self.check.is_some() || self.download.is_some()
	}

	/// The release offered for download: newer than this build (skipped or not).
	pub fn available(&self) -> Option<&Release> {
		self.release
			.as_ref()
			.filter(|r| r.is_newer_than(CURRENT_VERSION))
	}

	/// The startup check, when enabled and due.
	pub fn auto_check(&mut self, now: u64, waker: Option<Waker>) {
		if self.prefs.check_due(now) {
			self.start_check(false, now, waker);
		}
	}

	/// Asks GitHub for the latest release on a worker. `manual` (the menu) opens the
	/// window with the answer and reports "up to date" and failures; automatic checks
	/// stay quiet unless there is something new.
	pub fn start_check(&mut self, manual: bool, now: u64, waker: Option<Waker>) {
		if manual {
			self.window = true;
			self.error = None;
		}
		if let Some((m, _)) = &mut self.check {
			*m |= manual;
			return;
		}
		self.prefs.last_check = now;
		let fetcher = self.fetcher();
		let job = Job::spawn("update-check", waker, move || {
			let fetcher = fetcher.map_err(|e| e.to_string())?;
			cadenza_update::check(fetcher.as_ref()).map_err(|e| e.to_string())
		});
		self.check = Some((manual, job));
	}

	/// Downloads, verifies and installs the available release on a worker.
	pub fn start_download(&mut self, waker: Option<Waker>) {
		let Some(release) = self.available().cloned() else {
			return;
		};
		if self.download.is_some() {
			return;
		}
		let kind = self.kind().clone();
		let fetcher = match self.fetcher() {
			Ok(f) => f,
			Err(e) => {
				self.error = Some(e.to_string());
				return;
			}
		};
		self.error = None;
		let progress = Arc::new(Progress::default());
		let p = progress.clone();
		let work = std::env::temp_dir().join(format!("cadenza-update-{}", std::process::id()));
		let job = Job::spawn("update-download", waker, move || {
			cadenza_update::install(&kind, &release, fetcher.as_ref(), &work, &p)
		});
		self.download = Some((progress, job));
	}

	pub fn cancel_download(&self) {
		if let Some((p, _)) = &self.download {
			p.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
		}
	}

	/// "Skip this version": no chip or notice until a newer one.
	pub fn skip(&mut self) {
		if let Some(v) = self.available().and_then(Release::version) {
			self.prefs.skipped = v.to_string();
		} else {
			self.prefs.skipped = self.prefs.latest.clone();
		}
		self.window = false;
	}

	/// The chip / menu: shows the window, checking first when nothing is known yet.
	pub fn open_window(&mut self, now: u64, waker: Option<Waker>) {
		self.window = true;
		if self.release.is_none() && !self.busy() {
			self.start_check(true, now, waker);
		}
	}

	/// Collects finished checks and downloads (once per frame).
	pub fn poll(&mut self, notices: &mut Notices) {
		if let Some(r) = self.check.as_ref().and_then(|(_, j)| j.poll()) {
			let manual = self.check.take().is_some_and(|(m, _)| m);
			let r = r.unwrap_or_else(|| Err(tr("the worker thread stopped").to_string()));
			match r {
				Ok(release) => self.checked(release, manual, notices),
				Err(e) => {
					if manual {
						notices.error(trf("Update check failed: {e}", &[("e", &e)]));
						self.error = Some(e);
					}
				}
			}
		}
		if let Some(r) = self.download.as_ref().and_then(|(_, j)| j.poll()) {
			self.download = None;
			match r {
				Some(Ok(Prepared::OpenFile { file })) => self.open_disk_image(&file, notices),
				Some(Ok(prepared)) => self.ready = Some(prepared),
				Some(Err(UpdateError::Cancelled)) => notices.info(tr("Update cancelled")),
				Some(Err(e)) => {
					let e = e.to_string();
					notices.error(trf("Update failed: {e}", &[("e", &e)]));
					self.error = Some(e);
				}
				None => self.error = Some(tr("the worker thread stopped").to_string()),
			}
		}
	}

	fn checked(&mut self, release: Release, manual: bool, notices: &mut Notices) {
		let newer = release
			.is_newer_than(CURRENT_VERSION)
			.then(|| release.version())
			.flatten();
		match newer {
			Some(v) => {
				let v = v.to_string();
				let first_time = self.prefs.latest != v;
				self.prefs.latest = v.clone();
				if !manual && first_time && self.prefs.skipped != v {
					notices.info(trf(
						"Cadenza {version} is available: click “Update” in the status bar",
						&[("version", &v)],
					));
				}
			}
			None => {
				self.prefs.latest.clear();
				if manual {
					notices.info(trf(
						"Cadenza {version} is up to date",
						&[("version", &CURRENT_VERSION)],
					));
				}
			}
		}
		self.release = Some(release);
	}

	fn open_disk_image(&mut self, file: &Path, notices: &mut Notices) {
		let opened = Prepared::OpenFile {
			file: file.to_path_buf(),
		};
		match cadenza_update::launch(&opened, None, false) {
			Ok(()) => notices.info(tr(
				"Opened the new disk image: drag Cadenza onto Applications to finish",
			)),
			Err(e) => {
				let e = e.to_string();
				notices.error(trf("Update failed: {e}", &[("e", &e)]));
				self.error = Some(e);
			}
		}
	}

	/// Called when the user confirmed the restart (after the unsaved-changes prompt).
	pub fn arm_relaunch(&mut self) -> bool {
		self.relaunch = self.ready.is_some();
		self.relaunch
	}

	/// After the app shut down (any quit). A staged folder / bundle update is swapped in
	/// now, also on a plain quit after "Later" (it was verified; the next start runs it);
	/// a failed swap leaves the old install and is reported at the next start
	/// (`UpdatePrefs.install_error`). With a confirmed restart the app (new, or the old
	/// one after a failure) starts again, or the installer runs; a Windows installer is
	/// never run on a plain quit (it is downloaded again next time).
	pub fn on_quit(&mut self, reopen: Option<PathBuf>) {
		let restart = std::mem::take(&mut self.relaunch);
		let Some(p) = self.ready.take() else {
			return;
		};
		if let Err(e) = cadenza_update::finish(&p) {
			eprintln!("cadenza-app: could not install the update: {e}");
			self.prefs.install_error = e.to_string();
		}
		if restart && let Err(e) = cadenza_update::launch(&p, reopen.as_deref(), true) {
			eprintln!("cadenza-app: could not start the updated app: {e}");
		}
	}

	/// The failure of the last on-quit install, once (startup notice).
	pub fn take_install_error(&mut self) -> Option<String> {
		Some(std::mem::take(&mut self.prefs.install_error)).filter(|e| !e.is_empty())
	}
}

/// Unix seconds now (0 if the clock is before 1970).
pub fn unix_now() -> u64 {
	std::time::SystemTime::now()
		.duration_since(std::time::UNIX_EPOCH)
		.map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::sync::atomic::{AtomicUsize, Ordering};
	use std::time::{Duration, Instant};

	struct FakeFetcher {
		json: String,
		calls: AtomicUsize,
	}

	impl Fetcher for FakeFetcher {
		fn text(&self, _: &str) -> Result<String, UpdateError> {
			self.calls.fetch_add(1, Ordering::Relaxed);
			Ok(self.json.clone())
		}
		fn download(&self, _: &str, _: &Path, _: &Progress) -> Result<(), UpdateError> {
			Err(UpdateError::Network("offline".into()))
		}
	}

	fn release(tag: &str) -> String {
		format!(
			r#"{{"tag_name":"{tag}","html_url":"https://github.com/Pep3M/cadenza/releases/tag/{tag}","assets":[]}}"#
		)
	}

	fn wait(u: &mut UpdateState, notices: &mut Notices) {
		let t = Instant::now();
		while u.busy() && t.elapsed() < Duration::from_secs(5) {
			std::thread::sleep(Duration::from_millis(5));
			u.poll(notices);
		}
		assert!(!u.busy());
	}

	#[test]
	fn prefs_decide_when_to_check_and_what_to_offer() {
		let mut p = UpdatePrefs::default();
		assert!(p.check_due(1_000));
		p.last_check = 1_000;
		assert!(!p.check_due(1_000 + CHECK_INTERVAL_SECS - 1));
		assert!(p.check_due(1_000 + CHECK_INTERVAL_SECS));
		assert!(p.check_due(10), "clock set back");
		p.auto_check = false;
		assert!(!p.check_due(u64::MAX));

		assert_eq!(p.offered("0.3.2"), None);
		p.latest = "0.4.0".into();
		assert_eq!(p.offered("0.3.2"), Some("0.4.0"));
		assert_eq!(p.offered("0.4.0"), None, "already running it");
		p.skipped = "0.4.0".into();
		assert_eq!(p.offered("0.3.2"), None);
	}

	#[test]
	fn a_newer_release_is_offered_once_and_can_be_skipped() {
		let fake = Arc::new(FakeFetcher {
			json: release("v999.0.0"),
			calls: AtomicUsize::new(0),
		});
		let mut u = UpdateState::with_fetcher(fake.clone(), InstallKind::WindowsInstaller);
		let mut notices = Notices::default();
		u.auto_check(5, None);
		wait(&mut u, &mut notices);
		assert_eq!(u.prefs.last_check, 5);
		assert_eq!(u.prefs.offered(CURRENT_VERSION), Some("999.0.0"));
		assert!(!u.window, "automatic checks never open a window");
		assert_eq!(notices.log().len(), 1);
		// Not due again the same day; a manual check opens the window, no new notice.
		u.auto_check(6, None);
		assert!(!u.busy());
		u.start_check(true, 7, None);
		wait(&mut u, &mut notices);
		assert!(u.window && u.available().is_some());
		assert_eq!(notices.log().len(), 1);
		assert_eq!(fake.calls.load(Ordering::Relaxed), 2);
		u.skip();
		assert_eq!(u.prefs.offered(CURRENT_VERSION), None);
		assert!(!u.window);
	}

	#[test]
	fn up_to_date_and_failed_downloads_are_reported() {
		let fake = Arc::new(FakeFetcher {
			json: release(&format!("v{CURRENT_VERSION}")),
			calls: AtomicUsize::new(0),
		});
		let mut u = UpdateState::with_fetcher(fake, InstallKind::WindowsInstaller);
		u.prefs.latest = "0.0.1".into();
		let mut notices = Notices::default();
		u.start_check(true, 1, None);
		wait(&mut u, &mut notices);
		assert!(u.available().is_none() && u.prefs.latest.is_empty());
		assert!(notices.last().unwrap().text.contains("up to date"));

		let fake = Arc::new(FakeFetcher {
			json: release("v999.0.0"),
			calls: AtomicUsize::new(0),
		});
		let mut u = UpdateState::with_fetcher(fake, InstallKind::WindowsInstaller);
		u.start_check(true, 1, None);
		wait(&mut u, &mut notices);
		u.start_download(None);
		wait(&mut u, &mut notices);
		assert!(u.ready.is_none());
		assert!(
			u.error.as_deref().unwrap().contains("no cadenza-999.0.0"),
			"{:?}",
			u.error
		);
		assert!(!u.arm_relaunch());
	}

	#[test]
	fn a_staged_update_is_swapped_in_on_any_quit() {
		let t = tempfile::tempdir().unwrap();
		let cur = t.path().join("cadenza");
		let staging = t.path().join("cadenza.update");
		let staged = staging.join("cadenza-9.0.0-linux-x86_64");
		std::fs::create_dir_all(&cur).unwrap();
		std::fs::create_dir_all(&staged).unwrap();
		std::fs::write(cur.join("cadenza-app"), "old").unwrap();
		std::fs::write(staged.join("cadenza-app"), "new").unwrap();
		let swap = Prepared::Swap {
			current: cur.clone(),
			staged,
			cleanup: staging.clone(),
			app: cur.join("cadenza-app"),
		};
		let mut u = UpdateState {
			ready: Some(swap.clone()),
			..UpdateState::default()
		};
		// "Later", then a plain quit: installed, nothing started.
		u.on_quit(None);
		assert_eq!(
			std::fs::read_to_string(cur.join("cadenza-app")).unwrap(),
			"new"
		);
		assert!(!staging.exists() && u.ready.is_none());
		assert_eq!(u.take_install_error(), None);
		// The staged copy vanished: the install stays and the error waits for next start.
		u.ready = Some(swap);
		u.on_quit(None);
		assert_eq!(
			std::fs::read_to_string(cur.join("cadenza-app")).unwrap(),
			"new"
		);
		assert!(
			u.take_install_error()
				.unwrap()
				.contains("download it again")
		);
		assert_eq!(u.take_install_error(), None);
	}
}
