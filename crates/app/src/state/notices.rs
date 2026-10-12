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

#[cfg(test)]
mod tests {
	use super::*;

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
