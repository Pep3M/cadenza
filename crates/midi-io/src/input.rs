//! Hardware MIDI input via `midir` (ALSA on Linux), delivering timestamped messages
//! into a lock-free SPSC queue (`rtrb`) that a realtime consumer can drain without
//! allocation or locks.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use cadenza_dsp::MidiEvent;
use midir::{Ignore, MidiInput};
use rtrb::{Consumer, Producer, RingBuffer};

use crate::error::{MidiIoError, Result};
use crate::raw::RawMessage;

/// One MIDI message tagged with the host (monotonic, microsecond) time it arrived at,
/// as handed to us by the backend callback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimedMessage {
	pub host_micros: u64,
	pub message: RawMessage,
}

/// Creates the SPSC queue shared between the MIDI input thread (producer) and the
/// realtime consumer (e.g. the engine's audio callback).
pub fn new_queue(capacity: usize) -> (Producer<TimedMessage>, Consumer<TimedMessage>) {
	RingBuffer::new(capacity)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortInfo {
	pub index: usize,
	pub name: String,
}

/// Lists the available hardware/virtual MIDI input ports.
pub fn list_input_ports() -> Result<Vec<PortInfo>> {
	let midi_in =
		MidiInput::new("cadenza-midi-io").map_err(|e| MidiIoError::Input(e.to_string()))?;
	midi_in
		.ports()
		.iter()
		.enumerate()
		.map(|(index, port)| {
			midi_in
				.port_name(port)
				.map(|name| PortInfo { index, name })
				.map_err(|e| MidiIoError::Input(e.to_string()))
		})
		.collect()
}

/// An open hardware MIDI input connection. Dropping it (or calling [`Self::close`])
/// disconnects.
pub struct MidiInputConnection {
	inner: Option<midir::MidiInputConnection<()>>,
	port_name: String,
	dropped: Arc<AtomicU64>,
}

impl MidiInputConnection {
	pub fn port_name(&self) -> &str {
		&self.port_name
	}

	/// Number of events dropped because the queue was full (the realtime consumer
	/// wasn't draining fast enough).
	pub fn dropped_events(&self) -> u64 {
		self.dropped.load(Ordering::Relaxed)
	}

	/// Closes the connection, consuming it. Gracefully handles a port that has
	/// already disappeared (hot-unplugged hardware): `midir` simply stops delivering
	/// messages in that case, and closing an already-dead connection still succeeds.
	pub fn close(mut self) {
		if let Some(conn) = self.inner.take() {
			let _ = conn.close();
		}
	}
}

/// Opens `port` for input, pushing every message it receives (tagged with its arrival
/// time) into `producer`. `client_name` is the name this app registers with the MIDI
/// subsystem.
pub fn open_input_port(
	port: &PortInfo,
	producer: Producer<TimedMessage>,
	client_name: &str,
) -> Result<MidiInputConnection> {
	let mut midi_in = MidiInput::new(client_name).map_err(|e| MidiIoError::Input(e.to_string()))?;
	// Timing clock and active-sensing bytes are high-frequency and useless to us.
	midi_in.ignore(Ignore::ActiveSense);
	let ports = midi_in.ports();
	let target = ports
		.get(port.index)
		.ok_or_else(|| MidiIoError::PortNotFound(port.name.clone()))?;

	let dropped = Arc::new(AtomicU64::new(0));
	let dropped_in_callback = Arc::clone(&dropped);
	let mut producer = producer;

	let connection = midi_in
		.connect(
			target,
			client_name,
			move |host_micros, bytes, _: &mut ()| {
				if let Some(message) = RawMessage::from_bytes(bytes) {
					let timed = TimedMessage {
						host_micros,
						message,
					};
					if producer.push(timed).is_err() {
						dropped_in_callback.fetch_add(1, Ordering::Relaxed);
					}
				}
			},
			(),
		)
		.map_err(|e| MidiIoError::Input(e.to_string()))?;

	Ok(MidiInputConnection {
		inner: Some(connection),
		port_name: port.name.clone(),
		dropped,
	})
}

/// Maps a queued hardware message to the realtime [`MidiEvent`] vocabulary when
/// possible, keeping the raw message too (sustain pedal, pitch bend and the rest have
/// no `MidiEvent` representation yet).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MappedEvent {
	pub offset: u32,
	pub raw: RawMessage,
	/// `None` for messages `cadenza_dsp::MidiEvent` cannot represent (CC other than
	/// all-notes-off, pitch bend, program change, ...).
	pub dsp: Option<MidiEvent>,
}

/// All-notes-off is MIDI CC 123.
const CC_ALL_NOTES_OFF: u8 = 123;

fn to_dsp_event(message: &RawMessage, offset: u32) -> Option<MidiEvent> {
	if message.is_note_off_equivalent() {
		return Some(MidiEvent::NoteOff {
			offset,
			pitch: message.bytes[1],
		});
	}
	if message.is_note_on() {
		return Some(MidiEvent::NoteOn {
			offset,
			pitch: message.bytes[1],
			velocity: message.bytes[2] as f32 / 127.0,
		});
	}
	if message.controller_number() == Some(CC_ALL_NOTES_OFF) {
		return Some(MidiEvent::AllNotesOff { offset });
	}
	None
}

/// Drains every queued message whose timestamp falls within
/// `[block_start_host_micros, block_start_host_micros + block_len_samples / sample_rate)`
/// into `out`, converting host time to a sample-accurate offset within the block.
/// Messages from the future (not yet due) are left in the queue for the next block.
/// Never allocates: `out` must already have spare capacity.
pub fn drain_into_block(
	consumer: &mut Consumer<TimedMessage>,
	block_start_host_micros: u64,
	block_len_samples: u32,
	sample_rate: f64,
	out: &mut Vec<MappedEvent>,
) {
	let block_micros = block_len_samples as f64 / sample_rate * 1_000_000.0;
	let block_end = block_start_host_micros as f64 + block_micros;

	loop {
		match consumer.peek() {
			Ok(timed) if (timed.host_micros as f64) < block_end => {}
			_ => break,
		}
		let timed = match consumer.pop() {
			Ok(timed) => timed,
			Err(_) => break,
		};
		let elapsed_micros = timed.host_micros.saturating_sub(block_start_host_micros) as f64;
		let offset = ((elapsed_micros / 1_000_000.0) * sample_rate)
			.round()
			.clamp(0.0, (block_len_samples.saturating_sub(1)) as f64) as u32;
		out.push(MappedEvent {
			offset,
			raw: timed.message,
			dsp: to_dsp_event(&timed.message, offset),
		});
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn push_timed(producer: &mut Producer<TimedMessage>, host_micros: u64, bytes: &[u8]) {
		producer
			.push(TimedMessage {
				host_micros,
				message: RawMessage::from_bytes(bytes).unwrap(),
			})
			.unwrap();
	}

	#[test]
	fn maps_note_on_and_off_within_block() {
		let (mut producer, mut consumer) = new_queue(16);
		push_timed(&mut producer, 1_000, &[0x90, 60, 100]);
		push_timed(&mut producer, 1_500, &[0x80, 60, 0]);

		let mut out = Vec::with_capacity(4);
		drain_into_block(&mut consumer, 0, 48_000, 48_000.0, &mut out);

		assert_eq!(out.len(), 2);
		assert_eq!(out[0].offset, 48); // 1000us * 48000/1e6
		assert!(matches!(
			out[0].dsp,
			Some(MidiEvent::NoteOn { pitch: 60, .. })
		));
		assert!(matches!(
			out[1].dsp,
			Some(MidiEvent::NoteOff { pitch: 60, .. })
		));
	}

	#[test]
	fn note_on_velocity_zero_maps_to_note_off() {
		let (mut producer, mut consumer) = new_queue(16);
		push_timed(&mut producer, 0, &[0x91, 72, 0]);

		let mut out = Vec::with_capacity(4);
		drain_into_block(&mut consumer, 0, 48_000, 48_000.0, &mut out);

		assert!(matches!(
			out[0].dsp,
			Some(MidiEvent::NoteOff { pitch: 72, .. })
		));
	}

	#[test]
	fn all_notes_off_cc_maps_to_dsp_event() {
		let (mut producer, mut consumer) = new_queue(16);
		push_timed(&mut producer, 0, &[0xB0, 123, 0]);

		let mut out = Vec::with_capacity(4);
		drain_into_block(&mut consumer, 0, 48_000, 48_000.0, &mut out);

		assert!(matches!(out[0].dsp, Some(MidiEvent::AllNotesOff { .. })));
	}

	#[test]
	fn sustain_pedal_has_no_dsp_mapping_but_is_kept_raw() {
		let (mut producer, mut consumer) = new_queue(16);
		push_timed(&mut producer, 0, &[0xB0, 64, 127]);

		let mut out = Vec::with_capacity(4);
		drain_into_block(&mut consumer, 0, 48_000, 48_000.0, &mut out);

		assert_eq!(out[0].dsp, None);
		assert_eq!(out[0].raw.controller_number(), Some(64));
	}

	#[test]
	fn future_messages_stay_queued_for_the_next_block() {
		let (mut producer, mut consumer) = new_queue(16);
		push_timed(&mut producer, 0, &[0x90, 60, 100]);
		push_timed(&mut producer, 2_000_000, &[0x80, 60, 0]); // 2s later, out of this block

		let mut out = Vec::with_capacity(4);
		drain_into_block(&mut consumer, 0, 48_000, 48_000.0, &mut out); // 1s block
		assert_eq!(out.len(), 1);

		out.clear();
		drain_into_block(&mut consumer, 1_000_000, 48_000, 48_000.0, &mut out);
		assert_eq!(out.len(), 0); // still in the future
		drain_into_block(&mut consumer, 2_000_000, 48_000, 48_000.0, &mut out);
		assert_eq!(out.len(), 1);
	}
}
