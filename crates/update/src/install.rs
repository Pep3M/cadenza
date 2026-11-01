//! How this copy of Cadenza was installed, which decides how (or whether) it updates
//! itself and which release asset it needs.

use std::path::{Path, PathBuf};

/// The platforms releases are built for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
	Linux,
	Windows,
	MacOs,
	Other,
}

impl Os {
	pub fn current() -> Self {
		if cfg!(target_os = "linux") {
			Self::Linux
		} else if cfg!(windows) {
			Self::Windows
		} else if cfg!(target_os = "macos") {
			Self::MacOs
		} else {
			Self::Other
		}
	}
}

/// Why an install cannot update itself (the user updates it the way it was installed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManualReason {
	/// Running from a cargo `target/` folder.
	Development,
	/// Under `/usr` or `/opt` (a distribution or AUR package).
	PackageManager,
	/// The install folder cannot be written by this user.
	ReadOnly,
	/// Not laid out like an install from a release (unpacked by hand, `cargo install`...).
	Unmanaged,
	/// No release builds for this OS / CPU.
	Unsupported,
}

impl ManualReason {
	/// What the user does instead (English; the app has its own translated texts).
	pub fn advice(self) -> &'static str {
		match self {
			Self::Development => "this is a development build; update it with git and cargo",
			Self::PackageManager => {
				"this copy is managed by a package manager; update it with that package manager"
			}
			Self::ReadOnly => {
				"the install folder is not writable by this user; update it the way it was installed"
			}
			Self::Unmanaged => {
				"this copy was not installed from a release package; download the new release by hand"
			}
			Self::Unsupported => {
				"there are no release builds for this system; build the new version from source"
			}
		}
	}
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallKind {
	/// The Linux AppImage at this path (`$APPIMAGE`): replaced by the new one.
	AppImage {
		file: PathBuf,
	},
	/// The Linux tar.gz unpacked by `install.sh` in this folder: swapped for the new one.
	Folder {
		dir: PathBuf,
	},
	/// Windows, installed by the Inno Setup installer (or `install.ps1` in the same
	/// folder): the new installer upgrades it in place.
	WindowsInstaller,
	/// macOS, this writable `.app` bundle: replaced from the new disk image.
	MacApp {
		bundle: PathBuf,
	},
	/// macOS, a bundle that cannot be replaced: the new disk image is opened.
	MacDmg,
	Manual(ManualReason),
}

impl InstallKind {
	/// Whether "Download and install" does the whole job (else the user is pointed at the
	/// release page or their package manager).
	pub fn can_download(&self) -> bool {
		!matches!(self, Self::Manual(_))
	}

	/// The release asset this install needs, for release version `version` (no `v`).
	pub fn asset_name(&self, version: &str) -> Option<String> {
		let suffix = match self {
			Self::AppImage { .. } => "linux-x86_64.AppImage",
			Self::Folder { .. } => "linux-x86_64.tar.gz",
			Self::WindowsInstaller => "windows-x86_64-setup.exe",
			Self::MacApp { .. } | Self::MacDmg => "macos-universal.dmg",
			Self::Manual(_) => return None,
		};
		Some(format!("cadenza-{version}-{suffix}"))
	}
}

/// File system questions [`detect`] asks (faked in tests).
pub trait Probe {
	fn exists(&self, path: &Path) -> bool;
	/// Whether this user can create files in the folder `dir`.
	fn writable(&self, dir: &Path) -> bool;
}

pub struct RealProbe;

impl Probe for RealProbe {
	fn exists(&self, path: &Path) -> bool {
		path.exists()
	}

	fn writable(&self, dir: &Path) -> bool {
		let probe = dir.join(format!(".cadenza-write-test-{}", std::process::id()));
		match std::fs::File::create(&probe) {
			Ok(_) => {
				let _ = std::fs::remove_file(&probe);
				true
			}
			Err(_) => false,
		}
	}
}

/// What [`detect`] looks at.
#[derive(Debug, Clone)]
pub struct Context {
	pub os: Os,
	/// `std::env::consts::ARCH`.
	pub arch: String,
	/// The running executable, symlinks resolved.
	pub exe: PathBuf,
	/// `$APPIMAGE` (set by the AppImage runtime to the image file).
	pub appimage: Option<PathBuf>,
	/// `%LOCALAPPDATA%` on Windows.
	pub local_app_data: Option<PathBuf>,
}

impl Context {
	pub fn current() -> std::io::Result<Self> {
		let exe = std::env::current_exe()?;
		let exe = cadenza_core::paths::real_path(&exe).unwrap_or(exe);
		let var = |k: &str| {
			std::env::var_os(k)
				.filter(|v| !v.is_empty())
				.map(PathBuf::from)
		};
		Ok(Self {
			os: Os::current(),
			arch: std::env::consts::ARCH.to_string(),
			exe,
			appimage: var("APPIMAGE"),
			local_app_data: var("LOCALAPPDATA"),
		})
	}
}

/// Running from `target/{debug,release}` (or `target/<triple>/...`).
fn is_dev_build(exe: &Path) -> bool {
	let Some(dir) = exe.parent() else {
		return false;
	};
	let profile = dir.file_name().and_then(|n| n.to_str());
	matches!(profile, Some("debug" | "release"))
		&& dir
			.ancestors()
			.skip(1)
			.take(2)
			.any(|a| a.file_name().is_some_and(|n| n == "target"))
}

pub fn detect(ctx: &Context, probe: &dyn Probe) -> InstallKind {
	use InstallKind::*;
	use ManualReason::*;
	let x86_64 = ctx.arch == "x86_64";
	if ctx.os == Os::Linux
		&& let Some(file) = &ctx.appimage
	{
		if !x86_64 {
			return Manual(Unsupported);
		}
		return match file.parent() {
			Some(dir) if probe.writable(dir) => AppImage { file: file.clone() },
			_ => Manual(ReadOnly),
		};
	}
	if is_dev_build(&ctx.exe) {
		return Manual(Development);
	}
	let Some(dir) = ctx.exe.parent() else {
		return Manual(Unmanaged);
	};
	match ctx.os {
		Os::Linux => {
			if dir.starts_with("/usr") {
				return Manual(PackageManager);
			}
			let installed = ["cadenza", "cadenza-app", "LICENSE"]
				.iter()
				.all(|f| probe.exists(&dir.join(f)));
			if !installed {
				return Manual(Unmanaged);
			}
			if !x86_64 {
				return Manual(Unsupported);
			}
			let swappable = probe.writable(dir) && dir.parent().is_some_and(|p| probe.writable(p));
			if swappable {
				Folder {
					dir: dir.to_path_buf(),
				}
			} else if dir.starts_with("/opt") {
				Manual(PackageManager)
			} else {
				Manual(ReadOnly)
			}
		}
		Os::Windows => {
			if !x86_64 {
				return Manual(Unsupported);
			}
			let per_user = ctx
				.local_app_data
				.as_ref()
				.is_some_and(|l| dir == l.join("Programs").join("Cadenza"));
			if probe.exists(&dir.join("unins000.exe")) || per_user {
				WindowsInstaller
			} else {
				Manual(Unmanaged)
			}
		}
		Os::MacOs => {
			// .../Cadenza.app/Contents/MacOS/<exe>
			let bundle = dir
				.parent()
				.filter(|c| c.file_name().is_some_and(|n| n == "Contents"))
				.and_then(Path::parent)
				.filter(|b| b.extension().is_some_and(|e| e == "app"));
			match bundle {
				Some(b) if probe.writable(b) && b.parent().is_some_and(|p| probe.writable(p)) => {
					MacApp {
						bundle: b.to_path_buf(),
					}
				}
				Some(_) => MacDmg,
				None => Manual(Unmanaged),
			}
		}
		Os::Other => Manual(Unsupported),
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::collections::HashSet;

	#[derive(Default)]
	struct Fake {
		files: HashSet<PathBuf>,
		writable: HashSet<PathBuf>,
	}

	impl Probe for Fake {
		fn exists(&self, p: &Path) -> bool {
			self.files.contains(p)
		}
		fn writable(&self, d: &Path) -> bool {
			self.writable.contains(d)
		}
	}

	fn ctx(os: Os, exe: &str) -> Context {
		Context {
			os,
			arch: "x86_64".into(),
			exe: exe.into(),
			appimage: None,
			local_app_data: None,
		}
	}

	fn fake(files: &[&str], writable: &[&str]) -> Fake {
		Fake {
			files: files.iter().map(PathBuf::from).collect(),
			writable: writable.iter().map(PathBuf::from).collect(),
		}
	}

	#[test]
	fn linux_kinds() {
		let home = "/home/u/.local/opt/cadenza";
		let shipped = [
			"/home/u/.local/opt/cadenza/cadenza",
			"/home/u/.local/opt/cadenza/cadenza-app",
			"/home/u/.local/opt/cadenza/LICENSE",
		];
		let exe = format!("{home}/cadenza-app");
		let c = ctx(Os::Linux, &exe);
		assert_eq!(
			detect(&c, &fake(&shipped, &[home, "/home/u/.local/opt"])),
			InstallKind::Folder { dir: home.into() }
		);
		assert_eq!(
			detect(&c, &fake(&shipped, &[home])),
			InstallKind::Manual(ManualReason::ReadOnly)
		);
		assert_eq!(
			detect(&c, &fake(&shipped[..2], &[home, "/home/u/.local/opt"])),
			InstallKind::Manual(ManualReason::Unmanaged)
		);
		let mut arm = c.clone();
		arm.arch = "aarch64".into();
		assert_eq!(
			detect(&arm, &fake(&shipped, &[home, "/home/u/.local/opt"])),
			InstallKind::Manual(ManualReason::Unsupported)
		);
		let aur = [
			"/opt/cadenza/cadenza",
			"/opt/cadenza/cadenza-app",
			"/opt/cadenza/LICENSE",
		];
		assert_eq!(
			detect(
				&ctx(Os::Linux, "/opt/cadenza/cadenza-app"),
				&fake(&aur, &[])
			),
			InstallKind::Manual(ManualReason::PackageManager)
		);
		assert_eq!(
			detect(
				&ctx(Os::Linux, "/usr/bin/cadenza-app"),
				&fake(&[], &["/usr/bin"])
			),
			InstallKind::Manual(ManualReason::PackageManager)
		);
		assert_eq!(
			detect(
				&ctx(Os::Linux, "/home/u/src/cadenza/target/release/cadenza-app"),
				&fake(&[], &["/home/u/src/cadenza/target/release"])
			),
			InstallKind::Manual(ManualReason::Development)
		);
		assert_eq!(
			detect(
				&ctx(
					Os::Linux,
					"/w/target/x86_64-unknown-linux-gnu/debug/cadenza"
				),
				&fake(&[], &[])
			),
			InstallKind::Manual(ManualReason::Development)
		);
	}

	#[test]
	fn appimage_wins_on_linux() {
		let mut c = ctx(Os::Linux, "/tmp/.mount_cadXYZ/usr/bin/cadenza-app");
		c.appimage = Some("/home/u/Apps/cadenza.AppImage".into());
		assert_eq!(
			detect(&c, &fake(&[], &["/home/u/Apps"])),
			InstallKind::AppImage {
				file: "/home/u/Apps/cadenza.AppImage".into()
			}
		);
		assert_eq!(
			detect(&c, &fake(&[], &[])),
			InstallKind::Manual(ManualReason::ReadOnly)
		);
	}

	#[test]
	fn windows_kinds() {
		let dir = PathBuf::from("C:/Users/u/AppData/Local/Programs/Cadenza");
		let exe = dir.join("cadenza-app.exe");
		let mut c = ctx(Os::Windows, exe.to_str().unwrap());
		let none = fake(&[], &[]);
		assert_eq!(
			detect(&c, &none),
			InstallKind::Manual(ManualReason::Unmanaged)
		);
		c.local_app_data = Some("C:/Users/u/AppData/Local".into());
		assert_eq!(detect(&c, &none), InstallKind::WindowsInstaller);
		let all_users = ctx(Os::Windows, "C:/Program Files/Cadenza/cadenza-app.exe");
		assert_eq!(
			detect(
				&all_users,
				&fake(&["C:/Program Files/Cadenza/unins000.exe"], &[])
			),
			InstallKind::WindowsInstaller
		);
		assert_eq!(
			detect(&ctx(Os::Windows, "D:/tools/cadenza/cadenza-app.exe"), &none),
			InstallKind::Manual(ManualReason::Unmanaged)
		);
	}

	#[test]
	fn mac_kinds() {
		let c = ctx(
			Os::MacOs,
			"/Applications/Cadenza.app/Contents/MacOS/cadenza-app",
		);
		assert_eq!(
			detect(
				&c,
				&fake(&[], &["/Applications", "/Applications/Cadenza.app"])
			),
			InstallKind::MacApp {
				bundle: "/Applications/Cadenza.app".into()
			}
		);
		assert_eq!(detect(&c, &fake(&[], &[])), InstallKind::MacDmg);
		assert_eq!(
			detect(
				&ctx(Os::MacOs, "/opt/homebrew/bin/cadenza"),
				&fake(&[], &[])
			),
			InstallKind::Manual(ManualReason::Unmanaged)
		);
	}

	#[test]
	fn asset_per_kind() {
		let v = "0.4.0";
		let name = |k: InstallKind| k.asset_name(v);
		assert_eq!(
			name(InstallKind::AppImage { file: "/a".into() }).unwrap(),
			"cadenza-0.4.0-linux-x86_64.AppImage"
		);
		assert_eq!(
			name(InstallKind::Folder { dir: "/a".into() }).unwrap(),
			"cadenza-0.4.0-linux-x86_64.tar.gz"
		);
		assert_eq!(
			name(InstallKind::WindowsInstaller).unwrap(),
			"cadenza-0.4.0-windows-x86_64-setup.exe"
		);
		assert_eq!(
			name(InstallKind::MacDmg).unwrap(),
			"cadenza-0.4.0-macos-universal.dmg"
		);
		assert_eq!(name(InstallKind::Manual(ManualReason::Development)), None);
	}
}
