//! Encoding buffers to files: WAV (16/24-bit int, 32-bit float), FLAC (16/24-bit)
//! and, with the `mp3` feature, MP3 (LAME).

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::buffer::AudioBuffer;
use crate::error::{AudioIoError, Result};

/// PCM sample format for WAV.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WavSampleFormat {
	Int16,
	Int24,
	Float32,
}

/// Bit depth for FLAC.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FlacBitDepth {
	Bits16,
	Bits24,
}

/// Target file format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "format", rename_all = "snake_case")]
pub enum ExportFormat {
	Wav {
		sample_format: WavSampleFormat,
	},
	Flac {
		bit_depth: FlacBitDepth,
	},
	/// Constant bitrate MP3 (32..=320 kbps, snapped to the nearest standard rate).
	/// Only available with the `mp3` cargo feature.
	Mp3 {
		bitrate_kbps: u16,
	},
}

/// Word-length reduction strategy when writing integer formats.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Dither {
	/// Plain rounding (truncation distortion on quiet material).
	None,
	/// Triangular-PDF dither of +/-1 LSB: decorrelates quantisation error from the
	/// signal at the cost of a flat noise floor (about -98 dBFS RMS at 16 bit).
	#[default]
	Tpdf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportOptions {
	pub format: ExportFormat,
	/// Applied when the target is 16 or 24-bit integer; ignored for float and MP3.
	pub dither: Dither,
}

impl ExportOptions {
	pub fn wav(sample_format: WavSampleFormat) -> Self {
		Self {
			format: ExportFormat::Wav { sample_format },
			dither: Dither::Tpdf,
		}
	}

	pub fn flac(bit_depth: FlacBitDepth) -> Self {
		Self {
			format: ExportFormat::Flac { bit_depth },
			dither: Dither::Tpdf,
		}
	}

	pub fn mp3(bitrate_kbps: u16) -> Self {
		Self {
			format: ExportFormat::Mp3 { bitrate_kbps },
			dither: Dither::None,
		}
	}
}

/// Writes `buffer` to `path` in the requested format. Samples beyond full scale are
/// clipped for integer formats; float WAV keeps them.
pub fn export(buffer: &AudioBuffer, path: impl AsRef<Path>, options: &ExportOptions) -> Result<()> {
	let path = path.as_ref();
	match options.format {
		ExportFormat::Wav { sample_format } => {
			write_wav(buffer, path, sample_format, options.dither)
		}
		ExportFormat::Flac { bit_depth } => write_flac(buffer, path, bit_depth, options.dither),
		ExportFormat::Mp3 { bitrate_kbps } => write_mp3(buffer, path, bitrate_kbps),
	}
}

fn write_wav(
	buffer: &AudioBuffer,
	path: &Path,
	format: WavSampleFormat,
	dither: Dither,
) -> Result<()> {
	let (bits, sample_format) = match format {
		WavSampleFormat::Int16 => (16, hound::SampleFormat::Int),
		WavSampleFormat::Int24 => (24, hound::SampleFormat::Int),
		WavSampleFormat::Float32 => (32, hound::SampleFormat::Float),
	};
	let spec = hound::WavSpec {
		channels: channel_count_u16(buffer)?,
		sample_rate: buffer.sample_rate(),
		bits_per_sample: bits,
		sample_format,
	};
	sample_count_u32(buffer)?;
	let enc = |e: hound::Error| AudioIoError::Encode(format!("WAV '{}': {e}", path.display()));
	let mut writer = hound::WavWriter::create(path, spec).map_err(enc)?;
	match format {
		WavSampleFormat::Float32 => {
			for s in buffer.to_interleaved() {
				writer.write_sample(s).map_err(enc)?;
			}
		}
		WavSampleFormat::Int16 => {
			for s in quantize(buffer, 16, dither) {
				writer.write_sample(s as i16).map_err(enc)?;
			}
		}
		WavSampleFormat::Int24 => {
			for s in quantize(buffer, 24, dither) {
				writer.write_sample(s).map_err(enc)?;
			}
		}
	}
	writer.finalize().map_err(enc)
}

fn write_flac(
	buffer: &AudioBuffer,
	path: &Path,
	depth: FlacBitDepth,
	dither: Dither,
) -> Result<()> {
	use flacenc::component::BitRepr;
	use flacenc::error::Verify;

	let bits = match depth {
		FlacBitDepth::Bits16 => 16,
		FlacBitDepth::Bits24 => 24,
	};
	let samples = quantize(buffer, bits, dither);
	let config = flacenc::config::Encoder::default()
		.into_verified()
		.map_err(|(_, e)| AudioIoError::Encode(format!("FLAC config: {e:?}")))?;
	let source = flacenc::source::MemSource::from_samples(
		&samples,
		buffer.num_channels(),
		bits as usize,
		buffer.sample_rate() as usize,
	);
	let stream = flacenc::encode_with_fixed_block_size(&config, source, config.block_size)
		.map_err(|e| AudioIoError::Encode(format!("FLAC '{}': {e:?}", path.display())))?;
	let mut sink = flacenc::bitsink::ByteSink::new();
	stream
		.write(&mut sink)
		.map_err(|e| AudioIoError::Encode(format!("FLAC '{}': {e:?}", path.display())))?;
	let mut bytes = sink.as_slice().to_vec();
	// flacenc reports the short final block as STREAMINFO's minimum block size. The
	// spec excludes the last block, and decoders (symphonia included) then treat the
	// fixed-blocksize stream as variable and fail; declare min = max.
	if bytes.len() >= 12 && &bytes[..4] == b"fLaC" && bytes[4] & 0x7f == 0 {
		let (min, max) = bytes[8..12].split_at_mut(2);
		min.copy_from_slice(max);
	}
	std::fs::write(path, bytes).map_err(|source| AudioIoError::Io {
		path: path.to_path_buf(),
		source,
	})
}

#[cfg(feature = "mp3")]
fn write_mp3(buffer: &AudioBuffer, path: &Path, bitrate_kbps: u16) -> Result<()> {
	use mp3lame_encoder::{Bitrate, Builder, DualPcm, FlushNoGap, Quality};

	const RATES: [(u16, Bitrate); 13] = [
		(32, Bitrate::Kbps32),
		(40, Bitrate::Kbps40),
		(48, Bitrate::Kbps48),
		(64, Bitrate::Kbps64),
		(80, Bitrate::Kbps80),
		(96, Bitrate::Kbps96),
		(112, Bitrate::Kbps112),
		(128, Bitrate::Kbps128),
		(160, Bitrate::Kbps160),
		(192, Bitrate::Kbps192),
		(224, Bitrate::Kbps224),
		(256, Bitrate::Kbps256),
		(320, Bitrate::Kbps320),
	];
	let enc = |what: &str, e: &dyn std::fmt::Debug| {
		AudioIoError::Encode(format!("MP3 '{}': {what}: {e:?}", path.display()))
	};
	if !matches!(
		buffer.sample_rate(),
		8000 | 11025 | 12000 | 16000 | 22050 | 24000 | 32000 | 44100 | 48000
	) {
		return Err(AudioIoError::InvalidArgument(format!(
			"MP3 supports 8-48 kHz standard rates, got {} Hz; resample to 44100 or 48000 first",
			buffer.sample_rate()
		)));
	}
	let bitrate = RATES
		.iter()
		.min_by_key(|(k, _)| k.abs_diff(bitrate_kbps))
		.map(|(_, b)| *b)
		.unwrap_or(Bitrate::Kbps192);
	let stereo = buffer.to_stereo();
	let mut encoder = Builder::new()
		.ok_or_else(|| AudioIoError::Encode("cannot create LAME encoder".into()))?
		.with_num_channels(2)
		.map_err(|e| enc("channels", &e))?
		.with_sample_rate(buffer.sample_rate())
		.map_err(|e| enc("sample rate", &e))?
		.with_brate(bitrate)
		.map_err(|e| enc("bitrate", &e))?
		.with_quality(Quality::Best)
		.map_err(|e| enc("quality", &e))?
		.build()
		.map_err(|e| enc("init", &e))?;
	let mut out = Vec::new();
	const CHUNK: usize = 8192;
	for (l, r) in stereo
		.channel(0)
		.chunks(CHUNK)
		.zip(stereo.channel(1).chunks(CHUNK))
	{
		out.reserve(mp3lame_encoder::max_required_buffer_size(l.len()));
		encoder
			.encode_to_vec(DualPcm { left: l, right: r }, &mut out)
			.map_err(|e| enc("encode", &e))?;
	}
	out.reserve(7200);
	encoder
		.flush_to_vec::<FlushNoGap>(&mut out)
		.map_err(|e| enc("flush", &e))?;
	std::fs::write(path, out).map_err(|source| AudioIoError::Io {
		path: path.to_path_buf(),
		source,
	})
}

#[cfg(not(feature = "mp3"))]
fn write_mp3(_: &AudioBuffer, _: &Path, _: u16) -> Result<()> {
	Err(AudioIoError::Encode(
		"MP3 export is not available in this build (enable the `mp3` feature of cadenza-audio-io); export WAV or FLAC instead".into(),
	))
}

fn channel_count_u16(buffer: &AudioBuffer) -> Result<u16> {
	u16::try_from(buffer.num_channels())
		.map_err(|_| AudioIoError::InvalidArgument("too many channels to export".into()))
}

fn sample_count_u32(buffer: &AudioBuffer) -> Result<u32> {
	u32::try_from(buffer.frames() * buffer.num_channels()).map_err(|_| {
		AudioIoError::InvalidArgument("buffer too long for a WAV file (4 GiB limit)".into())
	})
}

/// Interleaved integer samples at `bits` depth, rounded with optional TPDF dither and
/// clipped to the representable range.
pub fn quantize(buffer: &AudioBuffer, bits: u32, dither: Dither) -> Vec<i32> {
	let scale = (1i64 << (bits - 1)) as f64;
	let max = scale - 1.0;
	let min = -scale;
	let mut rng = Rng::new(0x9E37_79B9_7F4A_7C15);
	buffer
		.to_interleaved()
		.into_iter()
		.map(|s| {
			let d = match dither {
				Dither::None => 0.0,
				Dither::Tpdf => rng.uniform() - rng.uniform(),
			};
			(f64::from(s) * scale + d).round().clamp(min, max) as i32
		})
		.collect()
}

/// xorshift64*: deterministic so repeated exports are bit-identical.
struct Rng(u64);

impl Rng {
	fn new(seed: u64) -> Self {
		Self(seed | 1)
	}

	/// Uniform in `[0, 1)`.
	fn uniform(&mut self) -> f64 {
		self.0 ^= self.0 >> 12;
		self.0 ^= self.0 << 25;
		self.0 ^= self.0 >> 27;
		(self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11) as f64 / (1u64 << 53) as f64
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn quantize_without_dither_rounds_and_clips() {
		let b = AudioBuffer::from_planar(44_100, vec![vec![0.0, 0.5, 1.5, -1.5, 1.0 / 32768.0]])
			.unwrap();
		let q = quantize(&b, 16, Dither::None);
		assert_eq!(q, vec![0, 16384, 32767, -32768, 1]);
	}

	#[test]
	fn tpdf_dither_is_bounded_zero_mean_and_triangular() {
		let n = 200_000;
		let b = AudioBuffer::from_planar(44_100, vec![vec![0.0; n]]).unwrap();
		let q = quantize(&b, 16, Dither::Tpdf);
		assert!(q.iter().all(|&s| (-1..=1).contains(&s)));
		let mean = q.iter().map(|&s| s as f64).sum::<f64>() / n as f64;
		assert!(mean.abs() < 0.01, "mean {mean}");
		// TPDF over [-1, 1] rounded: P(0) = 3/4, P(+-1) = 1/8 each.
		let zeros = q.iter().filter(|&&s| s == 0).count() as f64 / n as f64;
		assert!((zeros - 0.75).abs() < 0.01, "P(0) = {zeros}");
	}

	#[test]
	fn dither_linearises_a_sub_lsb_tone() {
		// A 0.3 LSB DC offset vanishes without dither but survives on average with it.
		let lsb = 1.0 / 32768.0;
		let b = AudioBuffer::from_planar(44_100, vec![vec![0.3 * lsb; 100_000]]).unwrap();
		let plain = quantize(&b, 16, Dither::None);
		assert!(plain.iter().all(|&s| s == 0));
		let d = quantize(&b, 16, Dither::Tpdf);
		let mean = d.iter().map(|&s| s as f64).sum::<f64>() / d.len() as f64;
		assert!((mean - 0.3).abs() < 0.02, "mean {mean}");
	}
}
