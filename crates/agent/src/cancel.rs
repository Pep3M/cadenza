//! Cooperative cancellation: the session checks the token at tool boundaries and races
//! it against model requests in flight.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::Notify;

#[derive(Debug, Default)]
struct Inner {
	cancelled: AtomicBool,
	notify: Notify,
}

/// A cheap, clonable "stop" flag shared between the UI and a running request.
#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<Inner>);

impl CancelToken {
	pub fn new() -> Self {
		Self::default()
	}

	/// Asks the request to stop at the next safe point.
	pub fn cancel(&self) {
		self.0.cancelled.store(true, Ordering::SeqCst);
		self.0.notify.notify_waiters();
	}

	pub fn is_cancelled(&self) -> bool {
		self.0.cancelled.load(Ordering::SeqCst)
	}

	/// Clears the flag for the next request.
	pub fn reset(&self) {
		self.0.cancelled.store(false, Ordering::SeqCst);
	}

	/// Resolves once [`CancelToken::cancel`] was called (immediately if it already was).
	pub async fn cancelled(&self) {
		loop {
			let notified = self.0.notify.notified();
			if self.is_cancelled() {
				return;
			}
			notified.await;
			if self.is_cancelled() {
				return;
			}
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[tokio::test]
	async fn cancelled_resolves_after_cancel_from_another_task() {
		let token = CancelToken::new();
		assert!(!token.is_cancelled());
		let waiter = token.clone();
		let handle = tokio::spawn(async move { waiter.cancelled().await });
		tokio::task::yield_now().await;
		token.cancel();
		tokio::time::timeout(std::time::Duration::from_secs(2), handle)
			.await
			.expect("woken")
			.unwrap();
		// Already cancelled: returns at once.
		token.cancelled().await;
		token.reset();
		assert!(!token.is_cancelled());
	}
}
