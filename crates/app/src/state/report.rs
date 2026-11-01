//! Help → Report a problem…: a prefilled GitHub issue (the `bug_report.yml` form) opened in
//! the browser. Only the version, OS, audio setup, plugin hosting mode and UI language go
//! with it: never project content, file paths or API keys.

/// The new-issue page; issue forms take field values by id in the query.
pub const NEW_ISSUE_URL: &str = "https://github.com/Pep3M/cadenza/issues/new";
/// GitHub refuses very long URLs (and browsers cut them); the description is shortened
/// to keep the whole link under this.
pub const MAX_URL_LEN: usize = 6000;

/// The report window's draft.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct ReportDraft {
	pub open: bool,
	pub title: String,
	pub description: String,
}

/// What a bug report needs to know about this setup (English: it goes to the issue).
#[derive(Debug, Clone, PartialEq)]
pub struct SystemInfo {
	pub version: String,
	/// `linux x86_64`.
	pub os: String,
	/// Backend, device, sample rate and buffer, one line.
	pub audio: String,
	pub plugin_hosting: String,
	pub language: String,
}

impl SystemInfo {
	/// The "Copy system info" text.
	pub fn text(&self) -> String {
		format!(
			"Cadenza {}\nOS: {}\nAudio: {}\nPlugin hosting: {}\nLanguage: {}",
			self.version, self.os, self.audio, self.plugin_hosting, self.language
		)
	}

	/// Audio and the rest that has no field of its own in the form.
	fn environment(&self) -> String {
		format!(
			"Audio: {}\nPlugin hosting: {}\nLanguage: {}",
			self.audio, self.plugin_hosting, self.language
		)
	}
}

/// Percent-encodes `s` for a query value (RFC 3986 unreserved characters stay).
pub fn encode(s: &str) -> String {
	let mut out = String::with_capacity(s.len() * 3);
	for b in s.bytes() {
		if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
			out.push(b as char);
		} else {
			out.push_str(&format!("%{b:02X}"));
		}
	}
	out
}

/// The prefilled new-issue URL, at most [`MAX_URL_LEN`] long (the description is cut
/// first, then the title).
pub fn issue_url(title: &str, description: &str, info: &SystemInfo) -> String {
	let build = |title: &str, description: &str| {
		let mut url = format!("{NEW_ISSUE_URL}?template=bug_report.yml");
		for (key, value) in [
			("title", title),
			("what-happened", description),
			("version", info.version.as_str()),
			("os", info.os.as_str()),
			("environment", &info.environment()),
		] {
			if !value.trim().is_empty() {
				url.push('&');
				url.push_str(key);
				url.push('=');
				url.push_str(&encode(value.trim()));
			}
		}
		url
	};
	let title: String = title.trim().chars().take(200).collect();
	let mut description: Vec<char> = description.trim().chars().collect();
	loop {
		let text: String = description.iter().collect();
		let url = build(&title, &text);
		if url.len() <= MAX_URL_LEN || description.is_empty() {
			return url;
		}
		// Each char is at most 12 encoded bytes: cut proportionally, then refine.
		let over = (url.len() - MAX_URL_LEN).div_ceil(12).max(1);
		let keep = description.len().saturating_sub(over + 1);
		description.truncate(keep);
		description.push('…');
		if keep == 0 {
			description.clear();
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn info() -> SystemInfo {
		SystemInfo {
			version: "0.3.2".into(),
			os: "linux x86_64".into(),
			audio: "ALSA, default device, 48000 Hz".into(),
			plugin_hosting: "sandboxed".into(),
			language: "es".into(),
		}
	}

	#[test]
	fn encodes_query_values() {
		assert_eq!(encode("a b&c=d/é"), "a%20b%26c%3Dd%2F%C3%A9");
		assert_eq!(encode("v0.3.2-rc_1~"), "v0.3.2-rc_1~");
	}

	#[test]
	fn builds_a_prefilled_issue_url() {
		let url = issue_url(" Crash on export ", "It crashed\nwhen exporting", &info());
		assert!(url.starts_with(
			"https://github.com/Pep3M/cadenza/issues/new?template=bug_report.yml&title=Crash%20on%20export&"
		));
		assert!(url.contains("&what-happened=It%20crashed%0Awhen%20exporting"));
		assert!(url.contains("&version=0.3.2&os=linux%20x86_64&environment=Audio%3A%20ALSA"));
		// Empty fields are left out.
		assert!(!issue_url("", "", &info()).contains("title="));
		assert!(info().text().contains("Plugin hosting: sandboxed"));
	}

	#[test]
	fn long_descriptions_are_cut_to_fit() {
		let long = "ñ".repeat(20_000);
		let url = issue_url("t", &long, &info());
		assert!(url.len() <= MAX_URL_LEN, "{}", url.len());
		assert!(url.contains("%E2%80%A6"), "marked as cut");
		assert!(url.contains("&version=0.3.2"), "system info kept");
	}
}
