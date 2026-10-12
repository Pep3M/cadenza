//! Non-realtime control channel between the host and a sandbox child: one
//! JSON message per frame, prefixed by its length as a little-endian `u32`.
//! The host writes requests to the child's stdin and reads replies from its
//! stdout (the child moves its own stdout to stderr first, so plugin logging
//! cannot corrupt the stream). Binary payloads (plugin state) are base64.

use std::io::{Read, Write};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::descriptor::PluginDescriptor;

/// Largest frame either side accepts (plugin states can be a few MB).
pub(crate) const MAX_FRAME: usize = 256 << 20;

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub(crate) enum Request {
	Init {
		descriptor: Box<PluginDescriptor>,
		sample_rate: f32,
		max_block: usize,
		offline: bool,
		state: Option<String>,
	},
	Prepare {
		sample_rate: f32,
		max_block: usize,
	},
	SaveState,
	LoadState {
		data: String,
	},
	ListPresets,
	LoadPreset {
		key: String,
	},
	Shutdown,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub(crate) struct Reply {
	/// `None` on success.
	pub error: Option<String>,
	pub latency: u32,
	pub tail_seconds: f32,
	/// `SaveState`: the state (base64); `LoadState`: unused.
	pub state: Option<String>,
	/// `LoadState`: whether the plugin accepted the data.
	pub accepted: bool,
	/// `Init` failed because the plugin refused the saved state (the host
	/// reports it as `PluginError::State` so callers can retry without it).
	pub state_rejected: bool,
	/// `ListPresets`: the plugin's factory presets.
	#[serde(default)]
	pub presets: Vec<WirePreset>,
}

/// A factory preset as sent over the control channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct WirePreset {
	pub key: String,
	pub name: String,
	pub category: String,
}

pub(crate) fn write_frame(out: &mut impl Write, msg: &impl Serialize) -> std::io::Result<()> {
	let body = serde_json::to_vec(msg).map_err(std::io::Error::other)?;
	if body.len() > MAX_FRAME {
		return Err(std::io::Error::other("control message too large"));
	}
	let mut frame = Vec::with_capacity(body.len() + 4);
	frame.extend_from_slice(&(body.len() as u32).to_le_bytes());
	frame.extend_from_slice(&body);
	out.write_all(&frame)?;
	out.flush()
}

fn parse<T: for<'de> Deserialize<'de>>(body: &[u8]) -> std::io::Result<T> {
	serde_json::from_slice(body).map_err(|e| {
		std::io::Error::new(
			std::io::ErrorKind::InvalidData,
			format!("bad control message: {e}"),
		)
	})
}

fn frame_len(header: [u8; 4]) -> std::io::Result<usize> {
	let len = u32::from_le_bytes(header) as usize;
	if len > MAX_FRAME {
		return Err(std::io::Error::new(
			std::io::ErrorKind::InvalidData,
			format!("control frame of {len} bytes exceeds the limit"),
		));
	}
	Ok(len)
}

/// Blocking read of one frame (child side).
pub(crate) fn read_frame<T: for<'de> Deserialize<'de>>(
	input: &mut impl Read,
) -> std::io::Result<T> {
	let mut header = [0u8; 4];
	input.read_exact(&mut header)?;
	let mut body = vec![0u8; frame_len(header)?];
	input.read_exact(&mut body)?;
	parse(&body)
}

/// Reads one frame from a pipe, giving up at `deadline` (host side: a hung
/// child must not hang the caller).
#[cfg(unix)]
pub(crate) fn read_frame_until<T: for<'de> Deserialize<'de>>(
	input: &mut std::process::ChildStdout,
	deadline: Instant,
) -> std::io::Result<T> {
	let mut header = [0u8; 4];
	read_exact_until(input, &mut header, deadline)?;
	let mut body = vec![0u8; frame_len(header)?];
	read_exact_until(input, &mut body, deadline)?;
	parse(&body)
}

#[cfg(unix)]
fn read_exact_until(
	input: &mut std::process::ChildStdout,
	mut buf: &mut [u8],
	deadline: Instant,
) -> std::io::Result<()> {
	use std::os::fd::AsRawFd;
	let fd = input.as_raw_fd();
	while !buf.is_empty() {
		let left = deadline.saturating_duration_since(Instant::now());
		if left.is_zero() {
			return Err(std::io::Error::new(
				std::io::ErrorKind::TimedOut,
				"the sandboxed plugin did not answer in time",
			));
		}
		let mut pfd = libc::pollfd {
			fd,
			events: libc::POLLIN,
			revents: 0,
		};
		let ms = left.as_millis().clamp(1, i32::MAX as u128) as libc::c_int;
		// SAFETY: one valid pollfd.
		let r = unsafe { libc::poll(&mut pfd, 1, ms) };
		if r < 0 {
			let e = std::io::Error::last_os_error();
			if e.kind() == std::io::ErrorKind::Interrupted {
				continue;
			}
			return Err(e);
		}
		if r == 0 {
			continue;
		}
		match input.read(buf) {
			Ok(0) => {
				return Err(std::io::Error::new(
					std::io::ErrorKind::UnexpectedEof,
					"the sandbox process closed its control channel",
				));
			}
			Ok(n) => buf = &mut buf[n..],
			Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
			Err(e) => return Err(e),
		}
	}
	Ok(())
}

/// Default deadline for control operations (plugin load, state, activation).
pub(crate) const CONTROL_TIMEOUT: Duration = Duration::from_secs(30);

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn frames_round_trip_and_reject_oversized_lengths() {
		let mut buf = Vec::new();
		write_frame(
			&mut buf,
			&Request::Prepare {
				sample_rate: 44_100.0,
				max_block: 64,
			},
		)
		.unwrap();
		match read_frame::<Request>(&mut buf.as_slice()).unwrap() {
			Request::Prepare {
				sample_rate,
				max_block,
			} => assert_eq!((sample_rate, max_block), (44_100.0, 64)),
			other => panic!("{other:?}"),
		}
		let huge = (u32::MAX).to_le_bytes();
		assert!(read_frame::<Reply>(&mut huge.as_slice()).is_err());
		let garbage = [3u8, 0, 0, 0, b'x', b'y', b'z'];
		assert!(read_frame::<Reply>(&mut garbage.as_slice()).is_err());
	}
}
