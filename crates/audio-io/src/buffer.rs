//! Planar `f32` audio buffer shared between decoding, caching, rendering and export.

use std::f32::consts::FRAC_1_SQRT_2;

use crate::error::{AudioIoError, Result};

/// Decoded audio: one `Vec<f32>` per channel (planar), all of equal length,
/// nominal full scale is `[-1.0, 1.0]`.
///
/// Wrap in `Arc<AudioBuffer>` to share it between threads (the cache does so).
#[derive(Debug, Clone, PartialEq)]
pub struct AudioBuffer {
	sample_rate: u32,
	channels: Vec<Vec<f32>>,
}

impl AudioBuffer {
	/// Builds a buffer from planar channels. Fails when there are no channels, the
	/// channels differ in length, or the sample rate is zero.
	pub fn from_planar(sample_rate: u32, channels: Vec<Vec<f32>>) -> Result<Self> {
		if sample_rate == 0 {
			return Err(AudioIoError::InvalidArgument(
				"sample rate must be greater than 0".into(),
			));
		}
		let Some(first) = channels.first() else {
			return Err(AudioIoError::InvalidArgument(
				"an audio buffer needs at least one channel".into(),
			));
		};
		let frames = first.len();
		if channels.iter().any(|c| c.len() != frames) {
			return Err(AudioIoError::InvalidArgument(
				"all channels of an audio buffer must have the same length".into(),
			));
		}
		Ok(Self {
			sample_rate,
			channels,
		})
	}

	/// Builds a buffer from interleaved samples (`L R L R ...`). A trailing partial
	/// frame is dropped.
	pub fn from_interleaved(
		sample_rate: u32,
		num_channels: usize,
		samples: &[f32],
	) -> Result<Self> {
		if num_channels == 0 {
			return Err(AudioIoError::InvalidArgument(
				"an audio buffer needs at least one channel".into(),
			));
		}
		let frames = samples.len() / num_channels;
		let mut channels: Vec<Vec<f32>> = (0..num_channels)
			.map(|_| Vec::with_capacity(frames))
			.collect();
		for frame in samples.chunks_exact(num_channels) {
			for (ch, &s) in channels.iter_mut().zip(frame) {
				ch.push(s);
			}
		}
		Self::from_planar(sample_rate, channels)
	}

	/// A silent buffer.
	pub fn silence(sample_rate: u32, num_channels: usize, frames: usize) -> Result<Self> {
		Self::from_planar(sample_rate, vec![vec![0.0; frames]; num_channels.max(1)])
	}

	pub fn sample_rate(&self) -> u32 {
		self.sample_rate
	}

	pub fn num_channels(&self) -> usize {
		self.channels.len()
	}

	/// Number of frames (samples per channel).
	pub fn frames(&self) -> usize {
		self.channels[0].len()
	}

	pub fn is_empty(&self) -> bool {
		self.frames() == 0
	}

	pub fn duration_seconds(&self) -> f64 {
		self.frames() as f64 / self.sample_rate as f64
	}

	/// Samples of one channel; panics if `index >= num_channels()`.
	pub fn channel(&self, index: usize) -> &[f32] {
		&self.channels[index]
	}

	pub fn channel_mut(&mut self, index: usize) -> &mut [f32] {
		&mut self.channels[index]
	}

	pub fn channels(&self) -> &[Vec<f32>] {
		&self.channels
	}

	pub fn into_channels(self) -> Vec<Vec<f32>> {
		self.channels
	}

	/// Interleaved copy (`L R L R ...`).
	pub fn to_interleaved(&self) -> Vec<f32> {
		let n = self.num_channels();
		let mut out = vec![0.0; self.frames() * n];
		for (c, ch) in self.channels.iter().enumerate() {
			for (i, &s) in ch.iter().enumerate() {
				out[i * n + c] = s;
			}
		}
		out
	}

	/// Approximate heap size of the sample data, in bytes.
	pub fn byte_size(&self) -> usize {
		self.frames() * self.num_channels() * std::mem::size_of::<f32>()
	}

	/// Absolute peak over all channels.
	pub fn peak(&self) -> f32 {
		self.channels
			.iter()
			.flat_map(|c| c.iter())
			.fold(0.0f32, |m, s| m.max(s.abs()))
	}

	/// Multiplies every sample by `gain` (linear).
	pub fn apply_gain(&mut self, gain: f32) {
		for ch in &mut self.channels {
			crate::clip::apply_gain(ch, gain);
		}
	}

	/// Converts to exactly two channels using [`stereo_downmix_gains`].
	pub fn to_stereo(&self) -> AudioBuffer {
		let n = self.num_channels();
		if n == 2 {
			return self.clone();
		}
		let frames = self.frames();
		let mut left = vec![0.0f32; frames];
		let mut right = vec![0.0f32; frames];
		for (c, ch) in self.channels.iter().enumerate() {
			let (gl, gr) = stereo_downmix_gains(n, c);
			for ((l, r), &s) in left.iter_mut().zip(right.iter_mut()).zip(ch) {
				*l += s * gl;
				*r += s * gr;
			}
		}
		AudioBuffer {
			sample_rate: self.sample_rate,
			channels: vec![left, right],
		}
	}

	/// Mono sum (average of the stereo downmix's left and right).
	pub fn to_mono(&self) -> AudioBuffer {
		let stereo = self.to_stereo();
		let mono = stereo.channels[0]
			.iter()
			.zip(&stereo.channels[1])
			.map(|(l, r)| 0.5 * (l + r))
			.collect();
		AudioBuffer {
			sample_rate: self.sample_rate,
			channels: vec![mono],
		}
	}
}

/// Left/right gains for channel `index` of a `num_channels` file when folding it to
/// stereo. Realtime safe (pure arithmetic), shared by [`AudioBuffer::to_stereo`] and
/// [`crate::clip::render_clip`].
///
/// Channel order follows the WAV/FLAC/SMPTE convention. Coefficients follow ITU-R
/// BS.775 (centre and surrounds at -3 dB, LFE discarded), without normalisation, so
/// loud multichannel material can exceed full scale after the fold-down:
///
/// | channels | layout | fold-down |
/// |---|---|---|
/// | 1 | M | M to both sides at unity (a centred mono clip) |
/// | 2 | L R | unchanged |
/// | 3 | L R C | C at -3 dB to both |
/// | 4 | L R SL SR | surrounds to their side at unity |
/// | 5 | L R C SL SR | C and surrounds at -3 dB |
/// | 6 | L R C LFE SL SR (5.1) | C, surrounds -3 dB; LFE dropped |
/// | 7 | L R C LFE BC SL SR (6.1) | BC -3 dB to both at half power each |
/// | 8 | L R C LFE BL BR SL SR (7.1) | backs and sides -3 dB to their side |
/// | other | unknown | even indices left, odd right, scaled by 1/(channels/2) |
pub fn stereo_downmix_gains(num_channels: usize, index: usize) -> (f32, f32) {
	const H: f32 = FRAC_1_SQRT_2;
	match (num_channels, index) {
		(1, 0) => (1.0, 1.0),
		(2, 0) => (1.0, 0.0),
		(2, 1) => (0.0, 1.0),
		(3, 0) | (4, 0) => (1.0, 0.0),
		(3, 1) | (4, 1) => (0.0, 1.0),
		(3, 2) => (H, H),
		(4, 2) => (1.0, 0.0),
		(4, 3) => (0.0, 1.0),
		(5..=8, 0) => (1.0, 0.0),
		(5..=8, 1) => (0.0, 1.0),
		(5..=8, 2) => (H, H),
		(5, 3) => (H, 0.0),
		(5, 4) => (0.0, H),
		(6..=8, 3) => (0.0, 0.0),
		(6, 4) => (H, 0.0),
		(6, 5) => (0.0, H),
		(7, 4) => (0.5, 0.5),
		(7, 5) => (H, 0.0),
		(7, 6) => (0.0, H),
		(8, 4) | (8, 6) => (H, 0.0),
		(8, 5) | (8, 7) => (0.0, H),
		(n, i) if i < n => {
			let scale = 1.0 / (n as f32 / 2.0).ceil();
			if i % 2 == 0 {
				(scale, 0.0)
			} else {
				(0.0, scale)
			}
		}
		_ => (0.0, 0.0),
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn interleave_roundtrip() {
		let b = AudioBuffer::from_interleaved(48_000, 2, &[1.0, -1.0, 0.5, -0.5]).unwrap();
		assert_eq!(b.frames(), 2);
		assert_eq!(b.channel(0), &[1.0, 0.5]);
		assert_eq!(b.to_interleaved(), vec![1.0, -1.0, 0.5, -0.5]);
	}

	#[test]
	fn rejects_ragged_channels() {
		assert!(AudioBuffer::from_planar(44_100, vec![vec![0.0; 3], vec![0.0; 2]]).is_err());
		assert!(AudioBuffer::from_planar(44_100, vec![]).is_err());
		assert!(AudioBuffer::from_planar(0, vec![vec![0.0]]).is_err());
	}

	#[test]
	fn mono_duplicates_to_stereo() {
		let b = AudioBuffer::from_planar(44_100, vec![vec![0.25, -0.5]]).unwrap();
		let s = b.to_stereo();
		assert_eq!(s.channel(0), &[0.25, -0.5]);
		assert_eq!(s.channel(1), &[0.25, -0.5]);
	}

	#[test]
	fn surround_51_folds_down_without_lfe() {
		// L R C LFE SL SR with only centre and LFE active.
		let mut chans = vec![vec![0.0f32; 1]; 6];
		chans[2][0] = 1.0;
		chans[3][0] = 1.0;
		let s = AudioBuffer::from_planar(48_000, chans).unwrap().to_stereo();
		assert!((s.channel(0)[0] - FRAC_1_SQRT_2).abs() < 1e-6);
		assert!((s.channel(1)[0] - FRAC_1_SQRT_2).abs() < 1e-6);
	}

	#[test]
	fn every_channel_reaches_some_side_except_lfe() {
		for n in 1..=12usize {
			for i in 0..n {
				let (l, r) = stereo_downmix_gains(n, i);
				let is_lfe = (6..=8).contains(&n) && i == 3;
				assert_eq!(l + r > 0.0, !is_lfe, "n={n} i={i}");
			}
		}
	}
}
