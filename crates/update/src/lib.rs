//! Update check and self-update from the GitHub releases of Cadenza, shared by the app
//! (`Help → Check for updates…`) and the CLI (`cadenza update`). No UI and no async: the
//! callers run these blocking functions on a worker thread.
//!
//! - [`check`] reads `releases/latest` (one anonymous request, nothing else is sent).
//! - [`detect`] tells how this copy was installed ([`InstallKind`]), which decides
//!   whether it can update itself and which release asset it needs.
//! - [`install`] downloads that asset, verifies it against the release's `SHA256SUMS`
//!   and puts it in place; [`launch`] then restarts the app or runs the installer.

mod apply;
mod fetch;
mod install;
mod release;
mod sums;
mod version;

pub use apply::{INSTALLER_ARGS, Prepared, RELAUNCH_ARG, install, launch, swap_dirs};
pub use fetch::{Fetcher, HttpFetcher, Progress};
pub use install::{Context, InstallKind, ManualReason, Os, Probe, RealProbe, detect};
pub use release::{Asset, Release, check};
pub use sums::{expected_hash, parse_sums, sha256_file, verify_file};
pub use version::Version;

/// The GitHub repository releases come from.
pub const REPO: &str = "Pep3M/cadenza";
/// The latest published (non-draft, non-prerelease) release, GitHub REST API.
pub const LATEST_URL: &str = "https://api.github.com/repos/Pep3M/cadenza/releases/latest";
/// The release list, for people who update by hand.
pub const RELEASES_URL: &str = "https://github.com/Pep3M/cadenza/releases";
/// This build's version.
pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Why a check or an update failed. The text is shown to the user as is.
#[derive(Debug, thiserror::Error)]
pub enum UpdateError {
	#[error("could not reach GitHub ({0}); check the internet connection and try again later")]
	Network(String),
	#[error(
		"GitHub answered something unexpected ({0}); try again later or update by hand from {RELEASES_URL}"
	)]
	Parse(String),
	#[error("the release has no {0} download for this system; update by hand from {RELEASES_URL}")]
	NoAsset(String),
	#[error(
		"the download of {0} is damaged (its SHA-256 does not match SHA256SUMS); nothing was changed, try again"
	)]
	Checksum(String),
	#[error("the update was cancelled; nothing was changed")]
	Cancelled,
	#[error("{0}")]
	Install(String),
	#[error("{context}: {source}")]
	Io {
		context: String,
		#[source]
		source: std::io::Error,
	},
}

impl UpdateError {
	pub(crate) fn io(context: impl Into<String>) -> impl FnOnce(std::io::Error) -> Self {
		let context = context.into();
		move |source| Self::Io { context, source }
	}
}

/// Opens `url` (or a file) with the system's default handler, detached.
pub fn open_in_browser(url: &str) -> std::io::Result<()> {
	open::that_detached(url)
}
