//! Toasts and the status-bar log.

use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
	Info,
	Warning,
	Error,
}

impl Level {
	fn ttl(self) -> Duration {
		Duration::from_secs(match self {
			Level::Info => 3,
			Level::Warning => 7,
			Level::Error => 12,
		})
	}
}

#[derive(Debug, Clone)]
pub struct Notice {
	pub level: Level,
	pub text: String,
	pub at: Instant,
}

const LOG_CAPACITY: usize = 200;

/// Recent messages: transient toasts plus a capped log for the status bar.
#[derive(Debug, Default)]
pub struct Notices {
	log: Vec<Notice>,
}

impl Notices {
	pub fn push(&mut self, level: Level, text: impl Into<String>) {
		if self.log.len() == LOG_CAPACITY {
			self.log.remove(0);
		}
		self.log.push(Notice {
			level,
			text: text.into(),
			at: Instant::now(),
		});
	}

	pub fn info(&mut self, text: impl Into<String>) {
		self.push(Level::Info, text);
	}

	pub fn warn(&mut self, text: impl Into<String>) {
		self.push(Level::Warning, text);
	}

	pub fn error(&mut self, text: impl Into<String>) {
		self.push(Level::Error, text);
	}

	/// Toasts still on screen at `now`, oldest first (at most `max`).
	pub fn toasts(&self, now: Instant, max: usize) -> Vec<&Notice> {
		let live: Vec<&Notice> = self
			.log
			.iter()
			.filter(|n| now.saturating_duration_since(n.at) < n.level.ttl())
			.collect();
		live[live.len().saturating_sub(max)..].to_vec()
	}

	pub fn last(&self) -> Option<&Notice> {
		self.log.last()
	}

	pub fn log(&self) -> &[Notice] {
		&self.log
	}

	pub fn count(&self, level: Level) -> usize {
		self.log.iter().filter(|n| n.level == level).count()
	}
}

/// `text` with every absolute path shortened to its file name: notices name files, the
/// full path stays available on hover.
pub fn short_paths(text: &str) -> String {
	let sep = |c: char| c == '/' || c == '\\';
	let opening = |c: char| matches!(c, '"' | '\'' | '(');
	let starts_path = |w: &str| {
		let w = w.trim_start_matches(opening);
		(w.starts_with('/') || w.get(1..3) == Some(":\\")) && w[1..].contains(sep)
	};
	let words: Vec<&str> = text.split(' ').collect();
	let mut out: Vec<String> = Vec::new();
	let mut i = 0;
	while i < words.len() {
		let w = words[i];
		if !starts_path(w) {
			out.push(w.to_owned());
			i += 1;
			continue;
		}
		// A path with spaces goes on while the next words still hold separators.
		let mut last = w;
		while i + 1 < words.len() && words[i + 1].contains(sep) && !starts_path(words[i + 1]) {
			i += 1;
			last = words[i];
		}
		let body = w.trim_start_matches(opening);
		let head = &w[..w.len() - body.len()];
		let name = last.rsplit(sep).next().unwrap_or(last);
		out.push(format!("{head}{name}"));
		i += 1;
	}
	out.join(" ")
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn short_paths_keep_file_names() {
		assert_eq!(
			short_paths("Opened /home/me/Songs/first_light.cdz"),
			"Opened first_light.cdz"
		);
		assert_eq!(
			short_paths("Saved \"/tmp/a b/x.wav\". Done"),
			"Saved \"x.wav\". Done"
		);
		assert_eq!(
			short_paths("Wrote (/tmp/out/mix.flac)."),
			"Wrote (mix.flac)."
		);
		assert_eq!(short_paths(r"Saved C:\Music\song.wav"), "Saved song.wav");
		assert_eq!(
			short_paths("Moved clip 1/2 to bar 5"),
			"Moved clip 1/2 to bar 5"
		);
	}

	#[test]
	fn toasts_expire_by_level() {
		let mut n = Notices::default();
		n.info("saved");
		n.error("boom");
		let now = Instant::now();
		assert_eq!(n.toasts(now, 5).len(), 2);
		assert_eq!(n.toasts(now, 1)[0].text, "boom", "newest kept");
		let later = now + Duration::from_secs(5);
		let left: Vec<_> = n.toasts(later, 5).iter().map(|t| t.text.clone()).collect();
		assert_eq!(left, vec!["boom"]);
		assert_eq!(n.count(Level::Error), 1);
		for i in 0..300 {
			n.info(format!("{i}"));
		}
		assert_eq!(n.log().len(), LOG_CAPACITY);
	}
}
