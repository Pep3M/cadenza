//! Sample data and zone maps for the sample-based built-ins (`sampler`, `drum_sampler`).
//!
//! The host decodes files off the audio thread (the engine uses its shared audio cache,
//! already resampled to the engine rate) and hands each zone an `Arc<dyn SampleData>`
//! through [`crate::Processor::set_zones`]. Processors only read the shared frames.

use std::sync::Arc;

/// Decoded audio shared with a sample-based device. Planar, every channel of equal
/// length, nominal full scale `[-1, 1]`.
pub trait SampleData: Send + Sync {
	/// Rate the frames are stored at (playback resamples when it differs from the
	/// device's rate).
	fn sample_rate(&self) -> f32;
	/// At least one.
	fn num_channels(&self) -> usize;
	/// Frames of channel `index` (`index < num_channels()`).
	fn channel(&self, index: usize) -> &[f32];
	fn frames(&self) -> usize {
		self.channel(0).len()
	}
	fn duration_seconds(&self) -> f64 {
		self.frames() as f64 / f64::from(self.sample_rate().max(1.0))
	}
}

/// In-memory [`SampleData`] (tests, synthesized content).
#[derive(Debug, Clone, PartialEq)]
pub struct SampleBuffer {
	sample_rate: f32,
	channels: Vec<Vec<f32>>,
}

impl SampleBuffer {
	/// Planar channels; missing channels become one silent channel and ragged ones are
	/// cut to the shortest.
	pub fn new(sample_rate: f32, mut channels: Vec<Vec<f32>>) -> Self {
		if channels.is_empty() {
			channels.push(Vec::new());
		}
		let frames = channels.iter().map(Vec::len).min().unwrap_or(0);
		for c in &mut channels {
			c.truncate(frames);
		}
		Self {
			sample_rate: sample_rate.max(1.0),
			channels,
		}
	}

	pub fn mono(sample_rate: f32, frames: Vec<f32>) -> Self {
		Self::new(sample_rate, vec![frames])
	}

	/// Shared, ready for [`Zone::sample`].
	pub fn shared(self) -> Arc<dyn SampleData> {
		Arc::new(self)
	}
}

impl SampleData for SampleBuffer {
	fn sample_rate(&self) -> f32 {
		self.sample_rate
	}

	fn num_channels(&self) -> usize {
		self.channels.len()
	}

	fn channel(&self, index: usize) -> &[f32] {
		&self.channels[index]
	}
}

/// How a zone loops while its note is held (and, for one-shots, until the envelope ends).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum LoopMode {
	#[default]
	Off,
	/// Jumps from the loop end back to the loop start.
	Forward,
	/// Plays the loop forward and backward alternately.
	PingPong,
}

/// One sample mapped onto a key and velocity range. Mirrors `cadenza_model::SampleZone`
/// (the engine converts it); positions are in seconds of the sample.
#[derive(Clone)]
pub struct Zone {
	/// `None`: the file could not be loaded; the zone stays mapped but silent.
	pub sample: Option<Arc<dyn SampleData>>,
	pub key_low: u8,
	pub key_high: u8,
	/// Key that plays the sample at its original pitch.
	pub root_key: u8,
	/// Velocity range `0..=1`, inclusive (velocity layers).
	pub vel_low: f32,
	pub vel_high: f32,
	/// Pitch offset in semitones (fractional = fine tune).
	pub tune: f32,
	pub gain_db: f32,
	/// -1 (left) ..= 1 (right).
	pub pan: f32,
	/// Playback start, seconds into the sample.
	pub start: f64,
	/// Playback end; `None` = the end of the sample.
	pub end: Option<f64>,
	pub loop_mode: LoopMode,
	pub loop_start: f64,
	/// `None` = the playback end.
	pub loop_end: Option<f64>,
	/// Crossfade at the loop seam, seconds (0 = hard loop).
	pub loop_crossfade: f64,
	/// Plays the start..end region backwards (loops are ignored).
	pub reverse: bool,
	/// Zone envelope: linear fade-in.
	pub attack_ms: f32,
	/// Zone envelope: exponential decay to -60 dB after the attack; `None` = play on
	/// (until the region ends, the key is released in gate mode, or a choke).
	pub decay_ms: Option<f32>,
	/// Choke group 1..=16; starting any zone of a group quickly fades every sounding
	/// voice of that group (closed hat chokes open hat). 0 = none.
	pub choke: u8,
	/// 0 = main output; 1..=n = extra (aux) stereo output n of devices that have them.
	pub output: u8,
	/// Round-robin group 1..=16: zones matching the same note in one group take turns
	/// (one per note-on). 0 = every matching zone plays (layers).
	pub round_robin: u8,
}

impl Default for Zone {
	fn default() -> Self {
		Self {
			sample: None,
			key_low: 0,
			key_high: 127,
			root_key: 60,
			vel_low: 0.0,
			vel_high: 1.0,
			tune: 0.0,
			gain_db: 0.0,
			pan: 0.0,
			start: 0.0,
			end: None,
			loop_mode: LoopMode::Off,
			loop_start: 0.0,
			loop_end: None,
			loop_crossfade: 0.0,
			reverse: false,
			attack_ms: 0.0,
			decay_ms: None,
			choke: 0,
			output: 0,
			round_robin: 0,
		}
	}
}

impl std::fmt::Debug for Zone {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Zone")
			.field(
				"sample",
				&self
					.sample
					.as_ref()
					.map(|s| (s.frames(), s.num_channels(), s.sample_rate())),
			)
			.field("keys", &(self.key_low..=self.key_high))
			.field("root_key", &self.root_key)
			.field("vel", &(self.vel_low..=self.vel_high))
			.field("loop_mode", &self.loop_mode)
			.field("choke", &self.choke)
			.field("output", &self.output)
			.finish_non_exhaustive()
	}
}

impl Zone {
	/// A zone playing `sample` across `key_low..=key_high`, at its pitch on `root_key`.
	pub fn new(sample: Arc<dyn SampleData>, key_low: u8, key_high: u8, root_key: u8) -> Self {
		Self {
			sample: Some(sample),
			key_low,
			key_high,
			root_key,
			..Self::default()
		}
	}

	/// Whether a note-on of `pitch` at `velocity` selects this zone.
	pub fn matches(&self, pitch: u8, velocity: f32) -> bool {
		(self.key_low..=self.key_high).contains(&pitch)
			&& velocity >= self.vel_low
			&& velocity <= self.vel_high
	}
}

/// Built-in devices that play sample zones (they accept [`crate::Processor::set_zones`]).
pub fn uses_samples(device_id: &str) -> bool {
	matches!(device_id, "sampler" | "drum_sampler")
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn buffers_are_rectangular_and_never_empty() {
		let b = SampleBuffer::new(48_000.0, vec![vec![0.0; 10], vec![0.0; 7]]);
		assert_eq!((b.num_channels(), b.frames()), (2, 7));
		let e = SampleBuffer::new(44_100.0, Vec::new());
		assert_eq!((e.num_channels(), e.frames()), (1, 0));
		assert!(
			(SampleBuffer::mono(1_000.0, vec![0.0; 500]).duration_seconds() - 0.5).abs() < 1e-9
		);
	}

	#[test]
	fn zones_match_keys_and_velocities_inclusively() {
		let z = Zone {
			key_low: 36,
			key_high: 38,
			vel_low: 0.5,
			vel_high: 1.0,
			..Zone::default()
		};
		assert!(z.matches(36, 0.5) && z.matches(38, 1.0));
		assert!(!z.matches(39, 0.8) && !z.matches(37, 0.49));
	}
}
