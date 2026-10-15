//! Raw MIDI messages and a running-status decoder, independent of any backend so they
//! can be unit-tested without hardware.

/// A single complete MIDI channel or system message, at most 3 bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawMessage {
	pub bytes: [u8; 3],
	pub len: u8,
}

impl RawMessage {
	/// Builds a message from a complete, already-assembled byte slice (as delivered by
	/// `midir` backends, which expand running status themselves). Returns `None` for
	/// empty input or messages longer than 3 bytes (e.g. sysex), which this crate's
	/// realtime path does not carry.
	pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
		if bytes.is_empty() || bytes.len() > 3 {
			return None;
		}
		let mut buf = [0u8; 3];
		buf[..bytes.len()].copy_from_slice(bytes);
		Some(Self {
			bytes: buf,
			len: bytes.len() as u8,
		})
	}

	pub fn as_slice(&self) -> &[u8] {
		&self.bytes[..self.len as usize]
	}

	pub fn status(&self) -> u8 {
		self.bytes[0]
	}

	/// Channel 0..=15 for channel-voice messages, `None` for system messages.
	pub fn channel(&self) -> Option<u8> {
		(self.status() & 0xF0 != 0xF0).then(|| self.status() & 0x0F)
	}

	pub fn is_note_on(&self) -> bool {
		self.status() & 0xF0 == 0x90 && self.len == 3
	}

	pub fn is_note_off(&self) -> bool {
		self.status() & 0xF0 == 0x80 && self.len == 3
	}

	/// A note-on with velocity 0 is semantically a note-off.
	pub fn is_note_off_equivalent(&self) -> bool {
		self.is_note_off() || (self.is_note_on() && self.bytes[2] == 0)
	}

	pub fn is_controller(&self) -> bool {
		self.status() & 0xF0 == 0xB0 && self.len == 3
	}

	pub fn controller_number(&self) -> Option<u8> {
		self.is_controller().then_some(self.bytes[1])
	}
}

/// Number of data bytes that follow a given status byte, honouring running status.
fn data_len(status: u8) -> Option<u8> {
	match status & 0xF0 {
		0x80 | 0x90 | 0xA0 | 0xB0 | 0xE0 => Some(2),
		0xC0 | 0xD0 => Some(1),
		0xF0 => match status {
			0xF1 | 0xF3 => Some(1),
			0xF2 => Some(2),
			0xF6 | 0xF8..=0xFF => Some(0),
			_ => None, // sysex and escapes: not reconstructed here
		},
		_ => None,
	}
}

/// Reconstructs complete messages from a byte stream that may use running status
/// (status byte omitted when it is the same as the previous message).
#[derive(Debug, Default)]
pub struct RunningStatusDecoder {
	running_status: Option<u8>,
	pending: [u8; 3],
	pending_len: u8,
	expected_len: u8,
}

impl RunningStatusDecoder {
	pub fn new() -> Self {
		Self::default()
	}

	/// Feeds one byte, returning a complete message once enough data bytes have arrived.
	pub fn feed(&mut self, byte: u8) -> Option<RawMessage> {
		if byte & 0x80 != 0 && byte < 0xF8 {
			// New status byte (real-time messages 0xF8..=0xFF don't reset running status).
			if byte == 0xF7 || byte == 0xF0 {
				self.running_status = None;
				return None; // sysex framing is not reconstructed here
			}
			if let Some(len) = data_len(byte) {
				self.running_status = if byte < 0xF0 { Some(byte) } else { None };
				self.pending[0] = byte;
				self.pending_len = 1;
				self.expected_len = len + 1;
				if self.expected_len == 1 {
					self.pending_len = 0;
					return Some(RawMessage {
						bytes: [byte, 0, 0],
						len: 1,
					});
				}
			}
			return None;
		}

		if self.pending_len == 0 {
			// Data byte with no status pending: apply running status if we have one.
			let status = self.running_status?;
			self.pending[0] = status;
			self.pending_len = 1;
			self.expected_len = data_len(status).unwrap_or(0) + 1;
		}

		if self.pending_len < 3 {
			self.pending[self.pending_len as usize] = byte;
			self.pending_len += 1;
		}

		if self.pending_len == self.expected_len {
			let message = RawMessage {
				bytes: self.pending,
				len: self.pending_len,
			};
			self.pending_len = 0;
			Some(message)
		} else {
			None
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn feed_all(decoder: &mut RunningStatusDecoder, bytes: &[u8]) -> Vec<RawMessage> {
		bytes.iter().filter_map(|&b| decoder.feed(b)).collect()
	}

	#[test]
	fn decodes_explicit_status_messages() {
		let mut decoder = RunningStatusDecoder::new();
		let messages = feed_all(&mut decoder, &[0x90, 60, 100, 0x80, 60, 0]);
		assert_eq!(messages.len(), 2);
		assert!(messages[0].is_note_on());
		assert!(messages[1].is_note_off());
	}

	#[test]
	fn running_status_reuses_previous_status_byte() {
		let mut decoder = RunningStatusDecoder::new();
		// Note on 60, then (running status) note on 64, without repeating 0x90.
		let messages = feed_all(&mut decoder, &[0x90, 60, 100, 64, 90]);
		assert_eq!(messages.len(), 2);
		assert_eq!(messages[1].bytes[1], 64);
		assert_eq!(messages[1].bytes[2], 90);
	}

	#[test]
	fn note_on_velocity_zero_is_note_off_equivalent() {
		let mut decoder = RunningStatusDecoder::new();
		let messages = feed_all(&mut decoder, &[0x91, 60, 0]);
		assert!(messages[0].is_note_off_equivalent());
		assert!(!messages[0].is_note_off());
	}

	#[test]
	fn controller_number_is_extracted() {
		let mut decoder = RunningStatusDecoder::new();
		let messages = feed_all(&mut decoder, &[0xB0, 64, 127]);
		assert_eq!(messages[0].controller_number(), Some(64));
	}
}
