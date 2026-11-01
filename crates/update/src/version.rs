//! Semantic versions as release tags spell them (`v0.3.2`, `0.4.0-rc.1`).

use std::cmp::Ordering;
use std::fmt;

/// `major.minor.patch[-pre]`; build metadata (`+...`) is ignored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
	pub major: u64,
	pub minor: u64,
	pub patch: u64,
	/// Pre-release identifiers (`rc.1` → `["rc", "1"]`), empty for a release.
	pub pre: Vec<String>,
}

impl Version {
	/// Parses `v1.2.3`, `1.2.3`, `1.2.3-beta.2`, `1.2` (patch 0). `None` if malformed.
	pub fn parse(text: &str) -> Option<Self> {
		let text = text.trim();
		let text = text
			.strip_prefix('v')
			.or_else(|| text.strip_prefix('V'))
			.unwrap_or(text);
		let text = text.split('+').next().unwrap_or(text);
		let (core, pre) = match text.split_once('-') {
			Some((c, p)) => (c, Some(p)),
			None => (text, None),
		};
		let mut parts = core.split('.');
		let mut number = |required: bool| -> Option<u64> {
			match parts.next() {
				Some(p) if !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) => p.parse().ok(),
				None if !required => Some(0),
				_ => None,
			}
		};
		let (major, minor, patch) = (number(true)?, number(true)?, number(false)?);
		if parts.next().is_some() {
			return None;
		}
		let pre = match pre {
			Some(p) => {
				let ids: Vec<String> = p.split('.').map(str::to_string).collect();
				if ids.iter().any(|i| i.is_empty()) {
					return None;
				}
				ids
			}
			None => Vec::new(),
		};
		Some(Self {
			major,
			minor,
			patch,
			pre,
		})
	}

	pub fn is_prerelease(&self) -> bool {
		!self.pre.is_empty()
	}
}

impl Ord for Version {
	fn cmp(&self, other: &Self) -> Ordering {
		(self.major, self.minor, self.patch)
			.cmp(&(other.major, other.minor, other.patch))
			.then_with(|| match (self.pre.is_empty(), other.pre.is_empty()) {
				(true, true) => Ordering::Equal,
				// A release sorts after its own pre-releases.
				(true, false) => Ordering::Greater,
				(false, true) => Ordering::Less,
				(false, false) => {
					for (a, b) in self.pre.iter().zip(&other.pre) {
						let o = match (a.parse::<u64>(), b.parse::<u64>()) {
							(Ok(x), Ok(y)) => x.cmp(&y),
							(Ok(_), Err(_)) => Ordering::Less,
							(Err(_), Ok(_)) => Ordering::Greater,
							(Err(_), Err(_)) => a.cmp(b),
						};
						if o != Ordering::Equal {
							return o;
						}
					}
					self.pre.len().cmp(&other.pre.len())
				}
			})
	}
}

impl PartialOrd for Version {
	fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
		Some(self.cmp(other))
	}
}

impl fmt::Display for Version {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
		if !self.pre.is_empty() {
			write!(f, "-{}", self.pre.join("."))?;
		}
		Ok(())
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn v(s: &str) -> Version {
		Version::parse(s).unwrap()
	}

	#[test]
	fn parses_tags() {
		assert_eq!(v("v0.3.2"), v("0.3.2"));
		assert_eq!(v("1.2").patch, 0);
		assert_eq!(v("1.2.3+build.5"), v("1.2.3"));
		assert_eq!(v("0.4.0-rc.1").pre, ["rc", "1"]);
		assert_eq!(v("v0.4.0-rc.1").to_string(), "0.4.0-rc.1");
		for bad in [
			"",
			"v",
			"1",
			"1.x.3",
			"1.2.3.4",
			"1.2.3-",
			"1.2.3-a..b",
			"latest",
		] {
			assert!(Version::parse(bad).is_none(), "{bad}");
		}
	}

	#[test]
	fn orders_like_semver() {
		let sorted = [
			"0.3.2",
			"0.3.10",
			"0.4.0-alpha",
			"0.4.0-alpha.1",
			"0.4.0-alpha.beta",
			"0.4.0-beta.2",
			"0.4.0-beta.11",
			"0.4.0-rc.1",
			"0.4.0",
			"1.0.0",
		];
		for w in sorted.windows(2) {
			assert!(v(w[0]) < v(w[1]), "{} < {}", w[0], w[1]);
		}
		assert!(v("0.4.0-rc.1").is_prerelease() && !v("0.4.0").is_prerelease());
	}
}
