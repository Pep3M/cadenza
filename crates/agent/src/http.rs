//! Transport helpers shared by providers: retry policy/backoff and a Server-Sent Events
//! line parser.

use std::time::Duration;

/// How transient failures (429, 5xx, dropped connections) are retried.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
	pub max_retries: u32,
	/// First backoff; doubles per attempt.
	pub base_delay: Duration,
	/// Upper bound for both computed backoff and server-requested waits.
	pub max_delay: Duration,
}

impl Default for RetryPolicy {
	fn default() -> Self {
		Self {
			max_retries: 5,
			base_delay: Duration::from_secs(1),
			max_delay: Duration::from_secs(60),
		}
	}
}

impl RetryPolicy {
	/// The wait before retry number `attempt` (0-based): the server's `retry-after` when
	/// given, else exponential backoff with a little deterministic jitter.
	pub fn delay(&self, attempt: u32, retry_after: Option<Duration>) -> Duration {
		if let Some(wait) = retry_after {
			return wait.min(self.max_delay);
		}
		let factor = 2u32.saturating_pow(attempt.min(16));
		let base = self.base_delay.saturating_mul(factor);
		// spread concurrent clients without a RNG dependency
		let jitter = base / 10 * (attempt % 3);
		(base + jitter).min(self.max_delay)
	}
}

/// Parses `retry-after-ms` (milliseconds) or `retry-after` (seconds; HTTP dates are
/// ignored) response headers.
pub fn parse_retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
	let value = |name: &str| headers.get(name)?.to_str().ok().map(str::trim);
	if let Some(ms) = value("retry-after-ms").and_then(|v| v.parse::<f64>().ok())
		&& ms >= 0.0
	{
		return Some(Duration::from_millis(ms as u64));
	}
	value("retry-after")
		.and_then(|v| v.parse::<f64>().ok())
		.filter(|s| *s >= 0.0)
		.map(Duration::from_secs_f64)
}

/// Incremental SSE parser: feed raw bytes as they arrive, get back the `data` payload of
/// each complete event. Comment lines (`: keep-alive`) and other fields are skipped.
#[derive(Debug, Default)]
pub struct SseParser {
	buffer: Vec<u8>,
	data: Vec<String>,
}

impl SseParser {
	pub fn new() -> Self {
		Self::default()
	}

	pub fn push(&mut self, bytes: &[u8]) -> Vec<String> {
		self.buffer.extend_from_slice(bytes);
		let mut events = Vec::new();
		while let Some(pos) = self.buffer.iter().position(|b| *b == b'\n') {
			let line: Vec<u8> = self.buffer.drain(..=pos).collect();
			let line = String::from_utf8_lossy(&line);
			let line = line.trim_end_matches(['\n', '\r']);
			self.line(line, &mut events);
		}
		events
	}

	/// Flushes a final event not terminated by a blank line.
	pub fn finish(&mut self) -> Vec<String> {
		let mut events = Vec::new();
		if !self.buffer.is_empty() {
			let rest = String::from_utf8_lossy(&std::mem::take(&mut self.buffer)).into_owned();
			self.line(rest.trim_end_matches('\r'), &mut events);
		}
		if !self.data.is_empty() {
			events.push(self.data.join("\n"));
			self.data.clear();
		}
		events
	}

	fn line(&mut self, line: &str, events: &mut Vec<String>) {
		if line.is_empty() {
			if !self.data.is_empty() {
				events.push(self.data.join("\n"));
				self.data.clear();
			}
		} else if let Some(data) = line.strip_prefix("data:") {
			self.data
				.push(data.strip_prefix(' ').unwrap_or(data).to_owned());
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn sse_events_survive_arbitrary_chunking() {
		let stream = ": OPENROUTER PROCESSING\n\ndata: {\"a\":1}\r\n\r\nevent: x\ndata: {\"b\":\ndata: 2}\n\ndata: [DONE]\n\n";
		for split in 1..stream.len() {
			let mut parser = SseParser::new();
			let mut events = parser.push(&stream.as_bytes()[..split]);
			events.extend(parser.push(&stream.as_bytes()[split..]));
			events.extend(parser.finish());
			assert_eq!(
				events,
				vec!["{\"a\":1}", "{\"b\":\n2}", "[DONE]"],
				"split at {split}"
			);
		}
	}

	#[test]
	fn sse_multibyte_characters_split_across_chunks() {
		let stream = "data: {\"t\":\"señal ♪\"}\n\n".as_bytes();
		let mut parser = SseParser::new();
		let mut events = Vec::new();
		for byte in stream {
			events.extend(parser.push(std::slice::from_ref(byte)));
		}
		assert_eq!(events, vec!["{\"t\":\"señal ♪\"}"]);
	}

	#[test]
	fn retry_delays_respect_server_hints_and_caps() {
		let policy = RetryPolicy {
			max_retries: 5,
			base_delay: Duration::from_millis(100),
			max_delay: Duration::from_secs(2),
		};
		assert_eq!(policy.delay(0, None), Duration::from_millis(100));
		assert!(policy.delay(1, None) >= Duration::from_millis(200));
		assert_eq!(policy.delay(10, None), Duration::from_secs(2));
		assert_eq!(
			policy.delay(0, Some(Duration::from_secs(1))),
			Duration::from_secs(1)
		);
		assert_eq!(
			policy.delay(0, Some(Duration::from_secs(90))),
			Duration::from_secs(2)
		);

		let mut headers = reqwest::header::HeaderMap::new();
		headers.insert("retry-after", "3".parse().unwrap());
		assert_eq!(parse_retry_after(&headers), Some(Duration::from_secs(3)));
		headers.insert("retry-after-ms", "250".parse().unwrap());
		assert_eq!(
			parse_retry_after(&headers),
			Some(Duration::from_millis(250))
		);
	}
}
