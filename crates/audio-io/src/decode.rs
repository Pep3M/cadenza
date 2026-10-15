//! Audio file probing and decoding (symphonia).

use std::fs::File;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use symphonia::core::codecs::audio::{AudioDecoder, AudioDecoderOptions};
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;

use crate::buffer::AudioBuffer;
use crate::error::{AudioIoError, Result};
use crate::resample::resample;

/// Container/codec family of a decoded file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioFormat {
	Wav,
	Aiff,
	Flac,
	Mp3,
	OggVorbis,
	/// MP4/M4A with AAC.
	Aac,
	/// MP4/M4A or CAF with Apple Lossless.
	Alac,
	Caf,
	Matroska,
	Other,
}

/// Cheap metadata about an audio file (no full decode unless the container lacks a
/// frame count).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AudioInfo {
	pub sample_rate: u32,
	pub channels: u16,
	/// Playable frames (encoder delay/padding excluded when the container reports it).
	pub frames: u64,
	pub duration_seconds: f64,
	pub format: AudioFormat,
	/// Container short name as reported by symphonia (`wave`, `flac`, `isomp4`...).
	pub container: String,
	/// Codec short name (`pcm_s24le`, `mp3`, `vorbis`, `aac`...).
	pub codec: String,
	/// Source bit depth for PCM/lossless formats.
	pub bits_per_sample: Option<u32>,
}

/// An opened audio file ready to be decoded.
pub struct AudioFile {
	path: PathBuf,
	info: AudioInfo,
	reader: Box<dyn FormatReader>,
	decoder: Box<dyn AudioDecoder>,
	track_id: u32,
}

impl std::fmt::Debug for AudioFile {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("AudioFile")
			.field("path", &self.path)
			.field("info", &self.info)
			.finish_non_exhaustive()
	}
}

impl AudioFile {
	/// Opens and probes `path`. When the container does not state its length
	/// (e.g. some MP3s), the file is scanned once to count frames.
	pub fn open(path: impl AsRef<Path>) -> Result<Self> {
		let path = path.as_ref();
		let (reader, decoder, track_id, mut info) = open_stream(path)?;
		if info.frames == 0 {
			let (scan_reader, mut scan_decoder, _, _) = open_stream(path)?;
			let counted = count_frames(path, scan_reader, &mut *scan_decoder, track_id)?;
			info.frames = counted;
			info.duration_seconds = counted as f64 / info.sample_rate as f64;
		}
		Ok(Self {
			path: path.to_path_buf(),
			info,
			reader,
			decoder,
			track_id,
		})
	}

	pub fn info(&self) -> &AudioInfo {
		&self.info
	}

	pub fn path(&self) -> &Path {
		&self.path
	}

	/// Decodes the whole file at its native sample rate, keeping every channel.
	/// Corrupt packets are skipped (replaced by nothing) rather than aborting, which
	/// matches how players treat damaged MP3/OGG files.
	pub fn decode(mut self) -> Result<AudioBuffer> {
		let n_ch = usize::from(self.info.channels);
		let mut channels: Vec<Vec<f32>> = (0..n_ch)
			.map(|_| Vec::with_capacity(self.info.frames as usize))
			.collect();
		let mut scratch: Vec<Vec<f32>> = Vec::new();
		let mut sample_rate = self.info.sample_rate;
		loop {
			let packet = match self.reader.next_packet() {
				Ok(Some(p)) => p,
				Ok(None) | Err(SymError::ResetRequired) => break,
				Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
				Err(e) => return Err(decode_err(&self.path, e)),
			};
			if packet.track_id != self.track_id {
				continue;
			}
			match self.decoder.decode(&packet) {
				Ok(buf) => {
					sample_rate = buf.spec().rate();
					buf.copy_to_vecs_planar::<f32>(&mut scratch);
					for (dst, src) in channels.iter_mut().zip(&scratch) {
						dst.extend_from_slice(src);
					}
					// A decoder may report fewer planes than the codec params promised.
					let produced = scratch.first().map_or(0, Vec::len);
					for dst in channels.iter_mut().skip(scratch.len()) {
						dst.resize(dst.len() + produced, 0.0);
					}
				}
				Err(SymError::DecodeError(_)) | Err(SymError::IoError(_)) => continue,
				Err(e) => return Err(decode_err(&self.path, e)),
			}
		}
		// Containers that state a length (e.g. MP4 without edit-list support in the
		// decoder) may decode trailing encoder padding; the declared length wins.
		let declared = self.info.frames as usize;
		if declared > 0 {
			for ch in &mut channels {
				ch.truncate(declared);
			}
		}
		AudioBuffer::from_planar(sample_rate, channels).map_err(|e| AudioIoError::Decode {
			path: self.path.clone(),
			reason: e.to_string(),
		})
	}
}

/// Probes a file without decoding it (see [`AudioFile::open`]).
pub fn probe(path: impl AsRef<Path>) -> Result<AudioInfo> {
	Ok(AudioFile::open(path)?.info)
}

/// Decodes `path` at its native rate, all channels.
pub fn decode_file(path: impl AsRef<Path>) -> Result<AudioBuffer> {
	AudioFile::open(path)?.decode()
}

/// Decodes `path` and converts it to `target_sample_rate` with the high-quality sinc
/// resampler. All channels are kept; call [`AudioBuffer::to_stereo`] to fold down.
pub fn decode_to_buffer(path: impl AsRef<Path>, target_sample_rate: u32) -> Result<AudioBuffer> {
	let buffer = decode_file(path)?;
	if buffer.sample_rate() == target_sample_rate {
		return Ok(buffer);
	}
	resample(&buffer, target_sample_rate)
}

type Stream = (Box<dyn FormatReader>, Box<dyn AudioDecoder>, u32, AudioInfo);

fn open_stream(path: &Path) -> Result<Stream> {
	let file = File::open(path).map_err(|source| AudioIoError::Io {
		path: path.to_path_buf(),
		source,
	})?;
	let mss = MediaSourceStream::new(Box::new(file), Default::default());
	let mut hint = Hint::new();
	if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
		hint.with_extension(ext);
	}
	let reader = symphonia::default::get_probe()
		.probe(
			&hint,
			mss,
			FormatOptions::default(),
			MetadataOptions::default(),
		)
		.map_err(|e| match e {
			SymError::IoError(source) => AudioIoError::Io {
				path: path.to_path_buf(),
				source,
			},
			other => AudioIoError::Unsupported {
				path: path.to_path_buf(),
				reason: other.to_string(),
			},
		})?;
	let container = reader.format_info().short_name.to_string();
	let track =
		reader
			.default_track(TrackType::Audio)
			.ok_or_else(|| AudioIoError::NoAudioTrack {
				path: path.to_path_buf(),
			})?;
	let params = track
		.codec_params
		.as_ref()
		.and_then(|p| p.audio())
		.ok_or_else(|| AudioIoError::NoAudioTrack {
			path: path.to_path_buf(),
		})?
		.clone();
	let track_id = track.id;
	let num_frames = track.num_frames;
	let codec = symphonia::default::get_codecs()
		.get_audio_decoder(params.codec)
		.map(|d| d.codec.info.short_name.to_string())
		.ok_or_else(|| AudioIoError::Unsupported {
			path: path.to_path_buf(),
			reason: format!("no decoder for codec {:?}", params.codec),
		})?;
	let decoder = symphonia::default::get_codecs()
		.make_audio_decoder(&params, &AudioDecoderOptions::default())
		.map_err(|e| AudioIoError::Unsupported {
			path: path.to_path_buf(),
			reason: e.to_string(),
		})?;
	let sample_rate = params.sample_rate.ok_or_else(|| AudioIoError::Decode {
		path: path.to_path_buf(),
		reason: "the file does not declare a sample rate".into(),
	})?;
	let channels = params.channels.as_ref().map_or(0, |c| c.count());
	if channels == 0 {
		return Err(AudioIoError::Decode {
			path: path.to_path_buf(),
			reason: "the file does not declare its channel count".into(),
		});
	}
	let frames = num_frames.unwrap_or(0);
	let info = AudioInfo {
		sample_rate,
		channels: u16::try_from(channels).unwrap_or(u16::MAX),
		frames,
		duration_seconds: frames as f64 / sample_rate as f64,
		format: classify(&container, &codec),
		container,
		codec,
		bits_per_sample: params.bits_per_sample,
	};
	Ok((reader, decoder, track_id, info))
}

fn count_frames(
	path: &Path,
	mut reader: Box<dyn FormatReader>,
	decoder: &mut dyn AudioDecoder,
	track_id: u32,
) -> Result<u64> {
	let mut frames = 0u64;
	loop {
		let packet = match reader.next_packet() {
			Ok(Some(p)) => p,
			Ok(None) | Err(SymError::ResetRequired) => break,
			Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
			Err(e) => return Err(decode_err(path, e)),
		};
		if packet.track_id != track_id {
			continue;
		}
		match decoder.decode(&packet) {
			Ok(buf) => frames += buf.frames() as u64,
			Err(SymError::DecodeError(_)) | Err(SymError::IoError(_)) => continue,
			Err(e) => return Err(decode_err(path, e)),
		}
	}
	Ok(frames)
}

fn classify(container: &str, codec: &str) -> AudioFormat {
	match (container, codec) {
		(_, "alac") => AudioFormat::Alac,
		("wave", _) => AudioFormat::Wav,
		("aiff", _) => AudioFormat::Aiff,
		(_, "flac") => AudioFormat::Flac,
		(_, "mp3") => AudioFormat::Mp3,
		(_, "vorbis") => AudioFormat::OggVorbis,
		(_, "aac") => AudioFormat::Aac,
		("caf", _) => AudioFormat::Caf,
		("matroska", _) => AudioFormat::Matroska,
		_ => AudioFormat::Other,
	}
}

fn decode_err(path: &Path, e: SymError) -> AudioIoError {
	AudioIoError::Decode {
		path: path.to_path_buf(),
		reason: e.to_string(),
	}
}
