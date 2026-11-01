//! The latest GitHub release and whether it is newer than this build.

use serde::{Deserialize, Serialize};

use crate::{Fetcher, LATEST_URL, UpdateError, Version};

/// One downloadable file of a release.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Asset {
	pub name: String,
	#[serde(rename = "browser_download_url")]
	pub url: String,
	#[serde(default)]
	pub size: u64,
}

/// What the app needs of a GitHub release (field names as the REST API sends them).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Release {
	#[serde(rename = "tag_name")]
	pub tag: String,
	#[serde(default)]
	pub name: Option<String>,
	/// Release notes (Markdown).
	#[serde(default)]
	pub body: Option<String>,
	#[serde(default)]
	pub html_url: String,
	#[serde(default)]
	pub draft: bool,
	#[serde(default)]
	pub prerelease: bool,
	#[serde(default)]
	pub assets: Vec<Asset>,
}

impl Release {
	pub fn parse(json: &str) -> Result<Self, UpdateError> {
		let r: Self = serde_json::from_str(json).map_err(|e| UpdateError::Parse(e.to_string()))?;
		if r.version().is_none() {
			return Err(UpdateError::Parse(format!(
				"the release tag '{}' is not a version",
				r.tag
			)));
		}
		Ok(r)
	}

	pub fn version(&self) -> Option<Version> {
		Version::parse(&self.tag)
	}

	/// True for a published, final release newer than `current` (drafts and pre-releases
	/// are never offered).
	pub fn is_newer_than(&self, current: &str) -> bool {
		if self.draft || self.prerelease {
			return false;
		}
		match (self.version(), Version::parse(current)) {
			(Some(latest), Some(current)) => !latest.is_prerelease() && latest > current,
			_ => false,
		}
	}

	pub fn asset(&self, name: &str) -> Option<&Asset> {
		self.assets.iter().find(|a| a.name == name)
	}

	/// The release notes cut to about `max_chars` (at a line end when possible), and
	/// whether they were cut.
	pub fn notes_excerpt(&self, max_chars: usize) -> (String, bool) {
		let body = self.body.as_deref().unwrap_or("").trim();
		if body.chars().count() <= max_chars {
			return (body.to_string(), false);
		}
		let cut: String = body.chars().take(max_chars).collect();
		let cut = match cut.rfind('\n') {
			Some(i) if i > max_chars / 2 => cut[..i].to_string(),
			_ => cut,
		};
		(format!("{}…", cut.trim_end()), true)
	}
}

/// Asks GitHub for the latest release (whatever its version; see
/// [`Release::is_newer_than`]).
pub fn check(fetcher: &dyn Fetcher) -> Result<Release, UpdateError> {
	Release::parse(&fetcher.text(LATEST_URL)?)
}

#[cfg(test)]
pub(crate) mod tests {
	use super::*;

	pub(crate) fn release_json(tag: &str, prerelease: bool) -> String {
		let v = tag.trim_start_matches('v');
		let assets: Vec<String> = [
			format!("cadenza-{v}-linux-x86_64.tar.gz"),
			format!("cadenza-{v}-linux-x86_64.AppImage"),
			format!("cadenza-{v}-windows-x86_64.zip"),
			format!("cadenza-{v}-windows-x86_64-setup.exe"),
			format!("cadenza-{v}-macos-universal.dmg"),
			"SHA256SUMS".into(),
			"install.sh".into(),
		]
		.iter()
		.map(|n| {
			format!(
				r#"{{"name":"{n}","browser_download_url":"https://example.invalid/{tag}/{n}","size":10}}"#
			)
		})
		.collect();
		format!(
			r###"{{"tag_name":"{tag}","name":"Cadenza {v}","body":"## Added\n- things","html_url":"https://github.com/Pep3M/cadenza/releases/tag/{tag}","draft":false,"prerelease":{prerelease},"assets":[{}]}}"###,
			assets.join(",")
		)
	}

	#[test]
	fn parses_the_api_answer_and_compares() {
		let r = Release::parse(&release_json("v0.4.0", false)).unwrap();
		assert_eq!(r.version(), Version::parse("0.4.0"));
		assert_eq!(r.assets.len(), 7);
		assert!(r.asset("SHA256SUMS").is_some());
		assert!(r.is_newer_than("0.3.2"));
		assert!(!r.is_newer_than("0.4.0"));
		assert!(!r.is_newer_than("0.5.0-rc.1"));
		let pre = Release::parse(&release_json("v0.5.0-rc.1", true)).unwrap();
		assert!(!pre.is_newer_than("0.3.2"));
		assert!(Release::parse(r#"{"tag_name":"nightly"}"#).is_err());
		assert!(Release::parse("<html>").is_err());
	}

	#[test]
	fn notes_are_cut_at_a_line() {
		let mut r = Release::parse(&release_json("v1.0.0", false)).unwrap();
		r.body = Some("line one\nline two\nline three".into());
		assert_eq!(
			r.notes_excerpt(100),
			("line one\nline two\nline three".into(), false)
		);
		assert_eq!(r.notes_excerpt(20), ("line one\nline two…".into(), true));
		r.body = None;
		assert_eq!(r.notes_excerpt(10).0, "");
	}
}
