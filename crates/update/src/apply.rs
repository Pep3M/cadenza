//! Downloading, verifying and putting a release in place. The current install is only
//! touched after the download is verified, and every swap keeps the old copy until the
//! new one is in place (rolled back on failure).

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{Fetcher, InstallKind, Progress, Release, UpdateError, verify_file};

/// Inno Setup switches for an unattended upgrade (progress shown, no questions).
pub const INSTALLER_ARGS: &[&str] = &[
	"/SILENT",
	"/SUPPRESSMSGBOXES",
	"/CLOSEAPPLICATIONS",
	"/NORESTART",
];
/// Makes the installer start `cadenza-app` again when it is done (a `[Run]` entry in
/// `packaging/windows/cadenza.iss`).
pub const RELAUNCH_ARG: &str = "/RELAUNCH=1";

/// What is left to do once [`install`] succeeded (always after the app quits).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Prepared {
	/// The new version is in place: start `app` (the new `cadenza-app`, or the bundle on
	/// macOS).
	Restart { app: PathBuf },
	/// Run this verified installer (Windows), which upgrades and relaunches.
	RunInstaller { setup: PathBuf },
	/// Open this verified disk image for the user to drag the app (macOS bundle that
	/// cannot be replaced).
	OpenFile { file: PathBuf },
}

impl Prepared {
	/// Whether the app must quit for the update to take effect (all of them: a restart or
	/// the installer replacing the running files).
	pub fn needs_quit(&self) -> bool {
		!matches!(self, Self::OpenFile { .. })
	}
}

/// Replaces the folder `current` with `new` (a sibling on the same file system): `current`
/// moves to `<name>.old`, `new` takes its place, then the backup is removed. If the second
/// rename fails the backup is moved back, so `current` is never left missing.
pub fn swap_dirs(current: &Path, new: &Path) -> std::io::Result<()> {
	let name = current
		.file_name()
		.ok_or_else(|| std::io::Error::other("the install folder has no name"))?;
	let mut backup_name = name.to_os_string();
	backup_name.push(".old");
	let backup = current.with_file_name(backup_name);
	if backup.exists() {
		std::fs::remove_dir_all(&backup)?;
	}
	std::fs::rename(current, &backup)?;
	if let Err(e) = std::fs::rename(new, current) {
		let _ = std::fs::rename(&backup, current);
		return Err(e);
	}
	let _ = std::fs::remove_dir_all(&backup);
	Ok(())
}

fn sibling(path: &Path, suffix: &str) -> PathBuf {
	let mut name = path.file_name().unwrap_or_default().to_os_string();
	name.push(suffix);
	path.with_file_name(name)
}

fn run(cmd: &mut Command, what: &str) -> Result<(), UpdateError> {
	let out = cmd
		.output()
		.map_err(UpdateError::io(format!("running {what}")))?;
	if out.status.success() {
		Ok(())
	} else {
		Err(UpdateError::Install(format!(
			"{what} failed: {}; nothing was changed",
			String::from_utf8_lossy(&out.stderr).trim()
		)))
	}
}

/// Downloads `release`'s asset for `kind` into `work` (a scratch folder), verifies it
/// against `SHA256SUMS` and installs it. Blocking; run it on a worker thread.
pub fn install(
	kind: &InstallKind,
	release: &Release,
	fetcher: &dyn Fetcher,
	work: &Path,
	progress: &Progress,
) -> Result<Prepared, UpdateError> {
	let version = release
		.version()
		.ok_or_else(|| UpdateError::Parse(format!("bad tag {}", release.tag)))?
		.to_string();
	let name = kind.asset_name(&version).ok_or_else(|| {
		UpdateError::Install(
			"this install cannot update itself; update it the way it was installed".into(),
		)
	})?;
	let asset = release
		.asset(&name)
		.ok_or_else(|| UpdateError::NoAsset(name.clone()))?;
	let sums_asset = release
		.asset("SHA256SUMS")
		.ok_or_else(|| UpdateError::NoAsset("SHA256SUMS".into()))?;
	let sums = fetcher.text(&sums_asset.url)?;
	std::fs::create_dir_all(work)
		.map_err(UpdateError::io(format!("creating {}", work.display())))?;
	// The AppImage is downloaded next to itself so the final rename stays on one file system.
	let dest = match kind {
		InstallKind::AppImage { file } => sibling(file, ".download"),
		_ => work.join(&name),
	};
	fetcher.download(&asset.url, &dest, progress)?;
	if progress.cancelled() {
		let _ = std::fs::remove_file(&dest);
		return Err(UpdateError::Cancelled);
	}
	if let Err(e) = verify_file(&dest, &sums, &name) {
		let _ = std::fs::remove_file(&dest);
		return Err(e);
	}
	match kind {
		InstallKind::AppImage { file } => {
			#[cfg(unix)]
			{
				use std::os::unix::fs::PermissionsExt;
				std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755))
					.map_err(UpdateError::io("making the AppImage executable"))?;
			}
			std::fs::rename(&dest, file).map_err(|e| {
				let _ = std::fs::remove_file(&dest);
				UpdateError::io(format!("replacing {}", file.display()))(e)
			})?;
			Ok(Prepared::Restart { app: file.clone() })
		}
		InstallKind::Folder { dir } => {
			let staging = sibling(dir, ".update");
			if staging.exists() {
				std::fs::remove_dir_all(&staging)
					.map_err(UpdateError::io(format!("clearing {}", staging.display())))?;
			}
			std::fs::create_dir_all(&staging)
				.map_err(UpdateError::io(format!("creating {}", staging.display())))?;
			let result = (|| {
				run(
					Command::new("tar")
						.arg("-xzf")
						.arg(&dest)
						.arg("-C")
						.arg(&staging),
					"unpacking the update (tar)",
				)?;
				let unpacked = staging.join(format!("cadenza-{version}-linux-x86_64"));
				if !unpacked.join("cadenza-app").is_file() {
					return Err(UpdateError::Install(
						"the downloaded archive has an unexpected layout; nothing was changed"
							.into(),
					));
				}
				swap_dirs(dir, &unpacked)
					.map_err(UpdateError::io(format!("replacing {}", dir.display())))
			})();
			let _ = std::fs::remove_dir_all(&staging);
			let _ = std::fs::remove_file(&dest);
			result?;
			Ok(Prepared::Restart {
				app: dir.join("cadenza-app"),
			})
		}
		InstallKind::WindowsInstaller => Ok(Prepared::RunInstaller { setup: dest }),
		InstallKind::MacApp { bundle } => {
			let mount = work.join("mnt");
			std::fs::create_dir_all(&mount)
				.map_err(UpdateError::io(format!("creating {}", mount.display())))?;
			run(
				Command::new("hdiutil")
					.args(["attach", "-quiet", "-nobrowse", "-readonly", "-mountpoint"])
					.arg(&mount)
					.arg(&dest),
				"opening the disk image (hdiutil attach)",
			)?;
			let staging = sibling(bundle, ".update");
			let result = (|| {
				let src = mount.join("Cadenza.app");
				if !src.is_dir() {
					return Err(UpdateError::Install(
						"the disk image has an unexpected layout; nothing was changed".into(),
					));
				}
				if staging.exists() {
					std::fs::remove_dir_all(&staging)
						.map_err(UpdateError::io(format!("clearing {}", staging.display())))?;
				}
				run(
					Command::new("ditto").arg(&src).arg(&staging),
					"copying the new app (ditto)",
				)?;
				// Ad-hoc signed, not notarized: without this Gatekeeper blocks the relaunch.
				let _ = Command::new("xattr")
					.args(["-dr", "com.apple.quarantine"])
					.arg(&staging)
					.output();
				swap_dirs(bundle, &staging)
					.map_err(UpdateError::io(format!("replacing {}", bundle.display())))
			})();
			let _ = Command::new("hdiutil")
				.args(["detach", "-quiet"])
				.arg(&mount)
				.output();
			if result.is_err() {
				let _ = std::fs::remove_dir_all(&staging);
			}
			let _ = std::fs::remove_file(&dest);
			result?;
			Ok(Prepared::Restart {
				app: bundle.clone(),
			})
		}
		InstallKind::MacDmg => Ok(Prepared::OpenFile { file: dest }),
		InstallKind::Manual(_) => unreachable!("no asset name for manual installs"),
	}
}

/// Starts what [`install`] prepared, detached; `reopen` is a project the restarted app
/// opens. Call it when the app is quitting (the installer replaces the running files).
pub fn launch(prepared: &Prepared, reopen: Option<&Path>) -> std::io::Result<()> {
	match prepared {
		Prepared::Restart { app } => {
			let mut cmd = if app.extension().is_some_and(|e| e == "app") {
				let mut c = Command::new("open");
				c.arg("-n").arg(app);
				if reopen.is_some() {
					c.arg("--args");
				}
				c
			} else {
				Command::new(app)
			};
			if let Some(p) = reopen {
				cmd.arg(p);
			}
			cmd.spawn().map(drop)
		}
		Prepared::RunInstaller { setup } => {
			Command::new(setup).args(INSTALLER_ARGS).spawn().map(drop)
		}
		Prepared::OpenFile { file } => open::that_detached(file),
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::release::tests::release_json;
	use std::collections::HashMap;
	use std::sync::Mutex;

	#[test]
	fn swap_replaces_and_drops_the_backup() {
		let t = tempfile::tempdir().unwrap();
		let cur = t.path().join("cadenza");
		let new = t.path().join("cadenza.update").join("cadenza-1.0.0");
		std::fs::create_dir_all(&cur).unwrap();
		std::fs::create_dir_all(&new).unwrap();
		std::fs::write(cur.join("cadenza-app"), "old").unwrap();
		std::fs::write(new.join("cadenza-app"), "new").unwrap();
		// A stale backup from an interrupted update is cleared first.
		std::fs::create_dir_all(t.path().join("cadenza.old")).unwrap();
		swap_dirs(&cur, &new).unwrap();
		assert_eq!(
			std::fs::read_to_string(cur.join("cadenza-app")).unwrap(),
			"new"
		);
		assert!(!new.exists());
		assert!(!t.path().join("cadenza.old").exists());
	}

	#[test]
	fn failed_swap_keeps_the_current_install() {
		let t = tempfile::tempdir().unwrap();
		let cur = t.path().join("cadenza");
		std::fs::create_dir_all(&cur).unwrap();
		std::fs::write(cur.join("cadenza-app"), "old").unwrap();
		assert!(swap_dirs(&cur, &t.path().join("missing")).is_err());
		assert_eq!(
			std::fs::read_to_string(cur.join("cadenza-app")).unwrap(),
			"old"
		);
		assert!(!t.path().join("cadenza.old").exists());
	}

	/// Serves fixed bodies by URL suffix.
	#[derive(Default)]
	struct FakeFetcher {
		files: HashMap<String, Vec<u8>>,
		cancel_downloads: bool,
		requests: Mutex<Vec<String>>,
	}

	impl FakeFetcher {
		fn body(&self, url: &str) -> Result<&Vec<u8>, UpdateError> {
			self.requests.lock().unwrap().push(url.to_string());
			self.files
				.iter()
				.find(|(k, _)| url.ends_with(&format!("/{k}")))
				.map(|(_, v)| v)
				.ok_or_else(|| UpdateError::Network(format!("404 {url}")))
		}
	}

	impl Fetcher for FakeFetcher {
		fn text(&self, url: &str) -> Result<String, UpdateError> {
			Ok(String::from_utf8(self.body(url)?.clone()).unwrap())
		}
		fn download(&self, url: &str, dest: &Path, p: &Progress) -> Result<(), UpdateError> {
			if self.cancel_downloads {
				p.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
				return Err(UpdateError::Cancelled);
			}
			std::fs::write(dest, self.body(url)?).unwrap();
			Ok(())
		}
	}

	fn sha(bytes: &[u8]) -> String {
		use sha2::{Digest, Sha256};
		Sha256::digest(bytes)
			.iter()
			.map(|b| format!("{b:02x}"))
			.collect()
	}

	fn fetcher_with(name: &str, bytes: &[u8], listed: &[u8]) -> FakeFetcher {
		let mut f = FakeFetcher::default();
		f.files.insert(name.into(), bytes.to_vec());
		f.files.insert(
			"SHA256SUMS".into(),
			format!("{}  {name}\n", sha(listed)).into_bytes(),
		);
		f
	}

	#[test]
	fn appimage_is_replaced_only_when_verified() {
		let t = tempfile::tempdir().unwrap();
		let file = t.path().join("Cadenza.AppImage");
		std::fs::write(&file, "old").unwrap();
		let release = Release::parse(&release_json("v1.0.0", false)).unwrap();
		let kind = InstallKind::AppImage { file: file.clone() };
		let name = "cadenza-1.0.0-linux-x86_64.AppImage";
		let work = t.path().join("work");

		let bad = fetcher_with(name, b"new", b"something else");
		let e = install(&kind, &release, &bad, &work, &Progress::default()).unwrap_err();
		assert!(matches!(e, UpdateError::Checksum(_)), "{e}");
		assert_eq!(std::fs::read_to_string(&file).unwrap(), "old");

		let mut cancelled = fetcher_with(name, b"new", b"new");
		cancelled.cancel_downloads = true;
		let e = install(&kind, &release, &cancelled, &work, &Progress::default()).unwrap_err();
		assert!(matches!(e, UpdateError::Cancelled));
		assert_eq!(std::fs::read_to_string(&file).unwrap(), "old");

		let good = fetcher_with(name, b"new", b"new");
		let done = install(&kind, &release, &good, &work, &Progress::default()).unwrap();
		assert_eq!(done, Prepared::Restart { app: file.clone() });
		assert_eq!(std::fs::read_to_string(&file).unwrap(), "new");
		assert_eq!(
			std::fs::read_dir(t.path()).unwrap().count(),
			2,
			"no leftovers"
		);
		#[cfg(unix)]
		{
			use std::os::unix::fs::PermissionsExt;
			let mode = std::fs::metadata(&file).unwrap().permissions().mode();
			assert_eq!(mode & 0o111, 0o111);
		}
	}

	#[test]
	fn missing_assets_and_manual_installs_are_refused() {
		let t = tempfile::tempdir().unwrap();
		let mut release = Release::parse(&release_json("v1.0.0", false)).unwrap();
		let f = FakeFetcher::default();
		let manual = InstallKind::Manual(crate::ManualReason::PackageManager);
		assert!(install(&manual, &release, &f, t.path(), &Progress::default()).is_err());
		release.assets.retain(|a| a.name != "SHA256SUMS");
		let e = install(
			&InstallKind::WindowsInstaller,
			&release,
			&f,
			t.path(),
			&Progress::default(),
		)
		.unwrap_err();
		assert!(matches!(e, UpdateError::NoAsset(n) if n == "SHA256SUMS"));
		assert!(f.requests.lock().unwrap().is_empty(), "nothing downloaded");
	}

	#[test]
	fn windows_gets_a_verified_installer() {
		let t = tempfile::tempdir().unwrap();
		let release = Release::parse(&release_json("v1.0.0", false)).unwrap();
		let name = "cadenza-1.0.0-windows-x86_64-setup.exe";
		let f = fetcher_with(name, b"MZ", b"MZ");
		let done = install(
			&InstallKind::WindowsInstaller,
			&release,
			&f,
			t.path(),
			&Progress::default(),
		)
		.unwrap();
		assert_eq!(
			done,
			Prepared::RunInstaller {
				setup: t.path().join(name)
			}
		);
		assert!(done.needs_quit());
	}

	#[cfg(target_os = "linux")]
	#[test]
	fn folder_install_swaps_in_the_unpacked_archive() {
		let t = tempfile::tempdir().unwrap();
		let src = t.path().join("src").join("cadenza-1.0.0-linux-x86_64");
		std::fs::create_dir_all(&src).unwrap();
		std::fs::write(src.join("cadenza-app"), "new").unwrap();
		let archive = t.path().join("a.tar.gz");
		let ok = Command::new("tar")
			.arg("-czf")
			.arg(&archive)
			.arg("-C")
			.arg(t.path().join("src"))
			.arg("cadenza-1.0.0-linux-x86_64")
			.status()
			.unwrap();
		assert!(ok.success());
		let bytes = std::fs::read(&archive).unwrap();
		let dir = t.path().join("opt").join("cadenza");
		std::fs::create_dir_all(&dir).unwrap();
		std::fs::write(dir.join("cadenza-app"), "old").unwrap();
		let release = Release::parse(&release_json("v1.0.0", false)).unwrap();
		let f = fetcher_with("cadenza-1.0.0-linux-x86_64.tar.gz", &bytes, &bytes);
		let kind = InstallKind::Folder { dir: dir.clone() };
		let done = install(
			&kind,
			&release,
			&f,
			&t.path().join("work"),
			&Progress::default(),
		)
		.unwrap();
		assert_eq!(
			done,
			Prepared::Restart {
				app: dir.join("cadenza-app")
			}
		);
		assert_eq!(
			std::fs::read_to_string(dir.join("cadenza-app")).unwrap(),
			"new"
		);
		let left: Vec<_> = std::fs::read_dir(t.path().join("opt")).unwrap().collect();
		assert_eq!(left.len(), 1, "no backup or staging folder left");
	}
}
