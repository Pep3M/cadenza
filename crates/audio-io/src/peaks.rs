//! Multi-resolution min/max waveform summaries for drawing clips.

use serde::{Deserialize, Serialize};

use crate::buffer::AudioBuffer;
use crate::error::{AudioIoError, Result};

/// Default bucket sizes (frames per peak): close zoom, mid zoom, whole-song overview.
pub const DEFAULT_RESOLUTIONS: [u32; 3] = [256, 4096, 65536];

/// Minimum and maximum sample value inside a bucket.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MinMax {
	pub min: f32,
	pub max: f32,
}

impl MinMax {
	/// Identity for [`MinMax::merge`].
	pub const EMPTY: MinMax = MinMax {
		min: f32::INFINITY,
		max: f32::NEG_INFINITY,
	};

	#[inline]
	pub fn merge(self, other: MinMax) -> MinMax {
		MinMax {
			min: self.min.min(other.min),
			max: self.max.max(other.max),
		}
	}

	pub fn is_empty(&self) -> bool {
		self.min > self.max
	}
}

/// Peaks at one resolution: `channels[c][b]` covers frames
/// `b * frames_per_bucket .. (b + 1) * frames_per_bucket` (the last bucket may be partial).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PeakLevel {
	pub frames_per_bucket: u32,
	pub channels: Vec<Vec<MinMax>>,
}

impl PeakLevel {
	pub fn buckets(&self) -> usize {
		self.channels.first().map_or(0, Vec::len)
	}

	/// Min/max of `channel` over frames `start..end`, at this level's granularity
	/// (buckets touching the range are included whole).
	pub fn range(&self, channel: usize, start: u64, end: u64) -> MinMax {
		let Some(ch) = self.channels.get(channel) else {
			return MinMax::EMPTY;
		};
		let fpb = u64::from(self.frames_per_bucket);
		let first = (start / fpb) as usize;
		let last = end.div_ceil(fpb).min(ch.len() as u64) as usize;
		ch.get(first..last.max(first))
			.unwrap_or(&[])
			.iter()
			.fold(MinMax::EMPTY, |a, &b| a.merge(b))
	}
}

/// Waveform overview of a buffer at several resolutions (finest first). Serializable
/// so it can be cached on disk next to the audio file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WaveformPeaks {
	pub sample_rate: u32,
	pub num_channels: u16,
	pub frames: u64,
	pub levels: Vec<PeakLevel>,
}

impl WaveformPeaks {
	/// Computes peaks with [`DEFAULT_RESOLUTIONS`].
	pub fn compute_default(buffer: &AudioBuffer) -> Self {
		Self::compute(buffer, &DEFAULT_RESOLUTIONS).expect("default resolutions are valid")
	}

	/// Computes peaks for each requested bucket size. Coarser levels are derived from
	/// the finest one when their size is a multiple of it, so the cost is one pass over
	/// the samples.
	pub fn compute(buffer: &AudioBuffer, resolutions: &[u32]) -> Result<Self> {
		let mut sizes = resolutions.to_vec();
		sizes.sort_unstable();
		sizes.dedup();
		if sizes.is_empty() || sizes[0] == 0 {
			return Err(AudioIoError::InvalidArgument(
				"peak resolutions must be a non-empty list of bucket sizes > 0".into(),
			));
		}
		let base = sizes[0];
		let finest: Vec<Vec<MinMax>> = buffer
			.channels()
			.iter()
			.map(|ch| bucketize(ch, base as usize))
			.collect();
		let mut levels = Vec::with_capacity(sizes.len());
		for &size in &sizes[1..] {
			let channels = if size % base == 0 {
				let k = (size / base) as usize;
				finest
					.iter()
					.map(|ch| {
						ch.chunks(k)
							.map(|c| c.iter().fold(MinMax::EMPTY, |a, &b| a.merge(b)))
							.collect()
					})
					.collect()
			} else {
				buffer
					.channels()
					.iter()
					.map(|ch| bucketize(ch, size as usize))
					.collect()
			};
			levels.push(PeakLevel {
				frames_per_bucket: size,
				channels,
			});
		}
		levels.insert(
			0,
			PeakLevel {
				frames_per_bucket: base,
				channels: finest,
			},
		);
		Ok(Self {
			sample_rate: buffer.sample_rate(),
			num_channels: u16::try_from(buffer.num_channels()).unwrap_or(u16::MAX),
			frames: buffer.frames() as u64,
			levels,
		})
	}

	/// Best level to draw at `frames_per_pixel`: the coarsest level that still has at
	/// least one bucket per pixel (falls back to the finest).
	pub fn level_for(&self, frames_per_pixel: f64) -> &PeakLevel {
		self.levels
			.iter()
			.rev()
			.find(|l| f64::from(l.frames_per_bucket) <= frames_per_pixel)
			.unwrap_or(&self.levels[0])
	}
}

fn bucketize(samples: &[f32], frames_per_bucket: usize) -> Vec<MinMax> {
	samples
		.chunks(frames_per_bucket)
		.map(|chunk| {
			chunk.iter().fold(MinMax::EMPTY, |m, &s| MinMax {
				min: m.min.min(s),
				max: m.max.max(s),
			})
		})
		.collect()
}

#[cfg(test)]
mod tests {
	use super::*;

	fn ramp_buffer(frames: usize) -> AudioBuffer {
		let ch: Vec<f32> = (0..frames)
			.map(|i| (i as f32 / frames as f32) * 2.0 - 1.0)
			.collect();
		let neg: Vec<f32> = ch.iter().map(|s| -s).collect();
		AudioBuffer::from_planar(44_100, vec![ch, neg]).unwrap()
	}

	#[test]
	fn levels_cover_the_buffer_and_agree() {
		let buf = ramp_buffer(200_000);
		let peaks = WaveformPeaks::compute_default(&buf);
		assert_eq!(peaks.levels.len(), 3);
		assert_eq!(peaks.levels[0].buckets(), 200_000usize.div_ceil(256));
		assert_eq!(peaks.levels[2].buckets(), 200_000usize.div_ceil(65536));
		for level in &peaks.levels {
			let all = level.range(0, 0, peaks.frames);
			assert_eq!(all.min, -1.0);
			assert!((all.max - buf.channel(0)[199_999]).abs() < 1e-6);
		}
		// Coarse buckets equal the merge of the fine ones they cover.
		let coarse = peaks.levels[1].channels[1][3];
		let fine = peaks.levels[0].range(1, 3 * 4096, 4 * 4096);
		assert_eq!(coarse, fine);
	}

	#[test]
	fn non_multiple_resolution_is_computed_directly() {
		let buf = ramp_buffer(10_000);
		let peaks = WaveformPeaks::compute(&buf, &[100, 333]).unwrap();
		assert_eq!(peaks.levels[1].frames_per_bucket, 333);
		let b = peaks.levels[1].channels[0][1];
		assert_eq!(b.min, buf.channel(0)[333]);
		assert_eq!(b.max, buf.channel(0)[665]);
	}

	#[test]
	fn picks_level_by_zoom() {
		let peaks = WaveformPeaks::compute_default(&ramp_buffer(1000));
		assert_eq!(peaks.level_for(10.0).frames_per_bucket, 256);
		assert_eq!(peaks.level_for(5000.0).frames_per_bucket, 4096);
		assert_eq!(peaks.level_for(1e9).frames_per_bucket, 65536);
	}

	#[test]
	fn serde_roundtrip() {
		let peaks = WaveformPeaks::compute(&ramp_buffer(3000), &[64, 1024]).unwrap();
		let json = serde_json::to_string(&peaks).unwrap();
		let back: WaveformPeaks = serde_json::from_str(&json).unwrap();
		assert_eq!(back, peaks);
	}

	#[test]
	fn rejects_zero_resolution() {
		assert!(WaveformPeaks::compute(&ramp_buffer(10), &[0]).is_err());
		assert!(WaveformPeaks::compute(&ramp_buffer(10), &[]).is_err());
	}
}
