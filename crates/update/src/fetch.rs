//! HTTP behind a trait, so tests run without the network.

use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use crate::{CURRENT_VERSION, UpdateError};

/// Shared between a download worker and the UI: bytes so far, total (0 = unknown) and a
/// cancel request.
#[derive(Debug, Default)]
pub struct Progress {
	pub done: AtomicU64,
	pub total: AtomicU64,
	pub cancel: AtomicBool,
}

impl Progress {
	pub fn cancelled(&self) -> bool {
		self.cancel.load(Ordering::Relaxed)
	}

	/// 0..1, or `None` while the size is unknown.
	pub fn fraction(&self) -> Option<f32> {
		let total = self.total.load(Ordering::Relaxed);
		(total > 0)
			.then(|| (self.done.load(Ordering::Relaxed) as f64 / total as f64).min(1.0) as f32)
	}
}

pub trait Fetcher: Send + Sync {
	/// A small text document (the release JSON, `SHA256SUMS`).
	fn text(&self, url: &str) -> Result<String, UpdateError>;
	/// Streams `url` into `dest`, updating `progress`; stops with
	/// [`UpdateError::Cancelled`] (and no file left) when asked to.
	fn download(&self, url: &str, dest: &Path, progress: &Progress) -> Result<(), UpdateError>;
}

/// The real fetcher: anonymous HTTPS with a `cadenza/<version>` user agent.
pub struct HttpFetcher {
	client: reqwest::blocking::Client,
}

impl HttpFetcher {
	pub fn new() -> Result<Self, UpdateError> {
		let client = reqwest::blocking::Client::builder()
			.user_agent(format!("cadenza/{CURRENT_VERSION}"))
			.connect_timeout(Duration::from_secs(15))
			.build()
			.map_err(|e| UpdateError::Network(e.to_string()))?;
		Ok(Self { client })
	}
}

fn network(e: reqwest::Error) -> UpdateError {
	UpdateError::Network(e.without_url().to_string())
}

impl Fetcher for HttpFetcher {
	fn text(&self, url: &str) -> Result<String, UpdateError> {
		let r = self
			.client
			.get(url)
			.header("Accept", "application/vnd.github+json")
			.timeout(Duration::from_secs(20))
			.send()
			.map_err(network)?;
		let status = r.status();
		if !status.is_success() {
			return Err(UpdateError::Network(format!("HTTP {status}")));
		}
		r.text().map_err(network)
	}

	fn download(&self, url: &str, dest: &Path, progress: &Progress) -> Result<(), UpdateError> {
		let mut r = self
			.client
			.get(url)
			.timeout(Duration::from_secs(60 * 60))
			.send()
			.map_err(network)?;
		let status = r.status();
		if !status.is_success() {
			return Err(UpdateError::Network(format!("HTTP {status}")));
		}
		progress
			.total
			.store(r.content_length().unwrap_or(0), Ordering::Relaxed);
		progress.done.store(0, Ordering::Relaxed);
		let result = (|| {
			let mut file = std::fs::File::create(dest)
				.map_err(UpdateError::io(format!("creating {}", dest.display())))?;
			let mut buf = vec![0u8; 1 << 16];
			loop {
				if progress.cancelled() {
					return Err(UpdateError::Cancelled);
				}
				let n = r
					.read(&mut buf)
					.map_err(|e| UpdateError::Network(e.to_string()))?;
				if n == 0 {
					break;
				}
				file.write_all(&buf[..n])
					.map_err(UpdateError::io(format!("writing {}", dest.display())))?;
				progress.done.fetch_add(n as u64, Ordering::Relaxed);
			}
			file.sync_all()
				.map_err(UpdateError::io(format!("writing {}", dest.display())))
		})();
		if result.is_err() {
			let _ = std::fs::remove_file(dest);
		}
		result
	}
}
