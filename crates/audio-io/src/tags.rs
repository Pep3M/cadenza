//! Text metadata written into exported files: a RIFF `LIST/INFO` chunk for WAV, a
//! `VORBIS_COMMENT` block for FLAC and an ID3v2.4 tag for MP3.

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{AudioIoError, Result};

/// Song information for exported files. Empty fields are not written.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Metadata {
	#[serde(default)]
	pub title: String,
	#[serde(default)]
	pub artist: String,
	#[serde(default)]
	pub album: String,
	#[serde(default)]
	pub genre: String,
	/// Year or full date ("2026", "2026-10-12").
	#[serde(default)]
	pub date: String,
	#[serde(default)]
	pub track_number: String,
	#[serde(default)]
	pub comment: String,
}

impl Metadata {
	pub fn is_empty(&self) -> bool {
		self.fields().all(|(_, v)| v.is_empty())
	}

	/// `(field, value)` in a fixed order (field names are the Vorbis comment keys).
	fn fields(&self) -> impl Iterator<Item = (&'static str, &str)> {
		[
			("TITLE", self.title.as_str()),
			("ARTIST", self.artist.as_str()),
			("ALBUM", self.album.as_str()),
			("GENRE", self.genre.as_str()),
			("DATE", self.date.as_str()),
			("TRACKNUMBER", self.track_number.as_str()),
			("COMMENT", self.comment.as_str()),
		]
		.into_iter()
		.map(|(k, v)| (k, v.trim()))
	}
}

/// Software name written alongside the metadata.
const SOFTWARE: &str = "Cadenza";

fn io_err(path: &Path, source: std::io::Error) -> AudioIoError {
	AudioIoError::Io {
		path: path.to_path_buf(),
		source,
	}
}

/// The `LIST` chunk (type `INFO`) for `meta`, including the chunk header.
pub fn wav_info_chunk(meta: &Metadata) -> Vec<u8> {
	let ids = [
		("TITLE", *b"INAM"),
		("ARTIST", *b"IART"),
		("ALBUM", *b"IPRD"),
		("GENRE", *b"IGNR"),
		("DATE", *b"ICRD"),
		("TRACKNUMBER", *b"ITRK"),
		("COMMENT", *b"ICMT"),
	];
	let mut body = b"INFO".to_vec();
	let mut push = |id: [u8; 4], text: &str| {
		let mut data = text.as_bytes().to_vec();
		data.push(0);
		body.extend_from_slice(&id);
		body.extend_from_slice(&(data.len() as u32).to_le_bytes());
		body.extend_from_slice(&data);
		if data.len() % 2 == 1 {
			body.push(0);
		}
	};
	for (key, value) in meta.fields() {
		if let Some((_, id)) = ids.iter().find(|(k, _)| *k == key)
			&& !value.is_empty()
		{
			push(*id, value);
		}
	}
	push(*b"ISFT", SOFTWARE);
	let mut chunk = b"LIST".to_vec();
	chunk.extend_from_slice(&(body.len() as u32).to_le_bytes());
	chunk.extend_from_slice(&body);
	chunk
}

/// Appends a `LIST/INFO` chunk to the finished WAV at `path` and fixes the RIFF size.
pub fn append_wav_info(path: &Path, meta: &Metadata) -> Result<()> {
	let chunk = wav_info_chunk(meta);
	let mut f = std::fs::OpenOptions::new()
		.read(true)
		.write(true)
		.open(path)
		.map_err(|e| io_err(path, e))?;
	let mut header = [0u8; 12];
	f.read_exact(&mut header).map_err(|e| io_err(path, e))?;
	if &header[..4] != b"RIFF" || &header[8..12] != b"WAVE" {
		return Err(AudioIoError::Encode(format!(
			"'{}' is not a RIFF/WAVE file",
			path.display()
		)));
	}
	let mut len = f.seek(SeekFrom::End(0)).map_err(|e| io_err(path, e))?;
	if len % 2 == 1 {
		f.write_all(&[0]).map_err(|e| io_err(path, e))?;
		len += 1;
	}
	f.write_all(&chunk).map_err(|e| io_err(path, e))?;
	let riff = u32::try_from(len + chunk.len() as u64 - 8).map_err(|_| {
		AudioIoError::InvalidArgument("WAV file too large for metadata (4 GiB limit)".into())
	})?;
	f.seek(SeekFrom::Start(4)).map_err(|e| io_err(path, e))?;
	f.write_all(&riff.to_le_bytes())
		.map_err(|e| io_err(path, e))?;
	Ok(())
}

/// Reads the `LIST/INFO` fields of a WAV file as `(id, text)` pairs.
pub fn read_wav_info(path: &Path) -> Result<Vec<(String, String)>> {
	let bytes = std::fs::read(path).map_err(|e| io_err(path, e))?;
	let mut out = Vec::new();
	let mut pos = 12;
	while pos + 8 <= bytes.len() {
		let id = &bytes[pos..pos + 4];
		let size =
			u32::from_le_bytes(bytes[pos + 4..pos + 8].try_into().unwrap_or([0; 4])) as usize;
		let body = pos + 8;
		let end = (body + size).min(bytes.len());
		if id == b"LIST" && end >= body + 4 && &bytes[body..body + 4] == b"INFO" {
			let mut p = body + 4;
			while p + 8 <= end {
				let sub = String::from_utf8_lossy(&bytes[p..p + 4]).into_owned();
				let n =
					u32::from_le_bytes(bytes[p + 4..p + 8].try_into().unwrap_or([0; 4])) as usize;
				let text = &bytes[p + 8..(p + 8 + n).min(end)];
				let text = String::from_utf8_lossy(text)
					.trim_end_matches('\0')
					.to_owned();
				out.push((sub, text));
				p += 8 + n + n % 2;
			}
		}
		pos = body + size + size % 2;
	}
	Ok(out)
}

/// Inserts a `VORBIS_COMMENT` metadata block right after STREAMINFO in an encoded FLAC
/// stream (keeping the "last metadata block" flag on the right block).
pub fn flac_insert_comments(bytes: &mut Vec<u8>, meta: &Metadata) -> Result<()> {
	if bytes.len() < 8 + 34 || &bytes[..4] != b"fLaC" || bytes[4] & 0x7f != 0 {
		return Err(AudioIoError::Encode(
			"not a FLAC stream starting with STREAMINFO".into(),
		));
	}
	let mut body = Vec::new();
	let vendor = SOFTWARE.as_bytes();
	body.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
	body.extend_from_slice(vendor);
	let comments: Vec<String> = meta
		.fields()
		.filter(|(_, v)| !v.is_empty())
		.map(|(k, v)| format!("{k}={v}"))
		.collect();
	body.extend_from_slice(&(comments.len() as u32).to_le_bytes());
	for c in &comments {
		body.extend_from_slice(&(c.len() as u32).to_le_bytes());
		body.extend_from_slice(c.as_bytes());
	}
	let streaminfo_len = u32::from_be_bytes([0, bytes[5], bytes[6], bytes[7]]) as usize;
	let insert_at = 8 + streaminfo_len;
	let was_last = bytes[4] & 0x80 != 0;
	bytes[4] &= 0x7f;
	let len = body.len() as u32;
	let mut block = vec![
		if was_last { 0x84 } else { 0x04 },
		(len >> 16) as u8,
		(len >> 8) as u8,
		len as u8,
	];
	block.extend_from_slice(&body);
	bytes.splice(insert_at..insert_at, block);
	Ok(())
}

/// Reads the Vorbis comments of a FLAC stream (`KEY=value` strings).
pub fn read_flac_comments(bytes: &[u8]) -> Vec<String> {
	let mut out = Vec::new();
	if bytes.len() < 4 || &bytes[..4] != b"fLaC" {
		return out;
	}
	let mut pos = 4;
	while pos + 4 <= bytes.len() {
		let header = bytes[pos];
		let len = u32::from_be_bytes([0, bytes[pos + 1], bytes[pos + 2], bytes[pos + 3]]) as usize;
		let body = &bytes[(pos + 4).min(bytes.len())..(pos + 4 + len).min(bytes.len())];
		if header & 0x7f == 4 && body.len() >= 8 {
			let vlen = u32::from_le_bytes(body[..4].try_into().unwrap_or([0; 4])) as usize;
			let mut p = 4 + vlen;
			if p + 4 <= body.len() {
				let n = u32::from_le_bytes(body[p..p + 4].try_into().unwrap_or([0; 4]));
				p += 4;
				for _ in 0..n {
					if p + 4 > body.len() {
						break;
					}
					let l =
						u32::from_le_bytes(body[p..p + 4].try_into().unwrap_or([0; 4])) as usize;
					let text = &body[(p + 4).min(body.len())..(p + 4 + l).min(body.len())];
					out.push(String::from_utf8_lossy(text).into_owned());
					p += 4 + l;
				}
			}
		}
		if header & 0x80 != 0 {
			break;
		}
		pos += 4 + len;
	}
	out
}

fn syncsafe(n: usize) -> [u8; 4] {
	[
		((n >> 21) & 0x7f) as u8,
		((n >> 14) & 0x7f) as u8,
		((n >> 7) & 0x7f) as u8,
		(n & 0x7f) as u8,
	]
}

/// An ID3v2.4 tag (UTF-8 text frames) to put in front of MP3 data; empty when `meta` is.
pub fn id3v2_tag(meta: &Metadata) -> Vec<u8> {
	if meta.is_empty() {
		return Vec::new();
	}
	let frame_ids = [
		("TITLE", *b"TIT2"),
		("ARTIST", *b"TPE1"),
		("ALBUM", *b"TALB"),
		("GENRE", *b"TCON"),
		("DATE", *b"TDRC"),
		("TRACKNUMBER", *b"TRCK"),
	];
	let mut frames = Vec::new();
	let mut frame = |id: [u8; 4], data: Vec<u8>| {
		frames.extend_from_slice(&id);
		frames.extend_from_slice(&syncsafe(data.len()));
		frames.extend_from_slice(&[0, 0]);
		frames.extend_from_slice(&data);
	};
	for (key, value) in meta.fields() {
		if value.is_empty() {
			continue;
		}
		if key == "COMMENT" {
			let mut data = vec![3];
			data.extend_from_slice(b"eng");
			data.push(0);
			data.extend_from_slice(value.as_bytes());
			frame(*b"COMM", data);
		} else if let Some((_, id)) = frame_ids.iter().find(|(k, _)| *k == key) {
			let mut data = vec![3];
			data.extend_from_slice(value.as_bytes());
			frame(*id, data);
		}
	}
	let mut data = vec![3];
	data.extend_from_slice(SOFTWARE.as_bytes());
	frame(*b"TSSE", data);
	let mut tag = b"ID3".to_vec();
	tag.extend_from_slice(&[4, 0, 0]);
	tag.extend_from_slice(&syncsafe(frames.len()));
	tag.extend_from_slice(&frames);
	tag
}

#[cfg(test)]
mod tests {
	use super::*;

	fn meta() -> Metadata {
		Metadata {
			title: "Night Drive".into(),
			artist: "Cadenza Ñ".into(),
			album: String::new(),
			genre: "Synthwave".into(),
			date: "2026".into(),
			track_number: "3".into(),
			comment: "made with tests".into(),
		}
	}

	#[test]
	fn info_chunk_is_well_formed() {
		let c = wav_info_chunk(&meta());
		assert_eq!(&c[..4], b"LIST");
		let size = u32::from_le_bytes(c[4..8].try_into().unwrap()) as usize;
		assert_eq!(size, c.len() - 8);
		assert_eq!(size % 2, 0);
		assert!(c.windows(4).any(|w| w == b"INAM"));
		assert!(!c.windows(4).any(|w| w == b"IPRD"), "empty album skipped");
	}

	#[test]
	fn id3_tag_has_frames_and_syncsafe_size() {
		let tag = id3v2_tag(&meta());
		assert_eq!(&tag[..5], b"ID3\x04\x00");
		let size = tag[6..10]
			.iter()
			.fold(0usize, |acc, b| (acc << 7) | usize::from(*b));
		assert_eq!(size, tag.len() - 10);
		for id in [
			b"TIT2", b"TPE1", b"TCON", b"TDRC", b"TRCK", b"COMM", b"TSSE",
		] {
			assert!(tag.windows(4).any(|w| w == id), "{id:?}");
		}
		assert!(id3v2_tag(&Metadata::default()).is_empty());
	}
}
