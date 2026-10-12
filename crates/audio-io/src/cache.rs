//! In-memory cache of decoded (and resampled) audio, shared across renders.
//!
//! Not for the audio thread: lookups take a mutex and misses decode from disk. The
//! engine resolves clips to `Arc<AudioBuffer>` while compiling a graph, then the
//! audio thread only reads the shared buffers.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::SystemTime;

use crate::buffer::AudioBuffer;
use crate::decode::decode_to_buffer;
use crate::error::{AudioIoError, Result};

/// Identity of a cached decode: the file (canonical path + modification time + size,
/// so edits on disk invalidate it) and the sample rate it was converted to.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CacheKey {
	pub path: PathBuf,
	pub modified: Option<SystemTime>,
	pub size: u64,
	pub sample_rate: u32,
}

impl CacheKey {
	/// Builds the key for `path` from its current metadata.
	pub fn for_file(path: &Path, sample_rate: u32) -> Result<Self> {
		let io = |source| AudioIoError::Io {
			path: path.to_path_buf(),
			source,
		};
		let canonical = path.canonicalize().map_err(io)?;
		let meta = std::fs::metadata(&canonical).map_err(io)?;
		Ok(Self {
			path: canonical,
			modified: meta.modified().ok(),
			size: meta.len(),
			sample_rate,
		})
	}
}

/// Counters for diagnostics.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CacheStats {
	pub entries: usize,
	pub bytes: usize,
	pub hits: u64,
	pub misses: u64,
}

type Slot = Arc<Mutex<Option<Arc<AudioBuffer>>>>;

struct Entry {
	slot: Slot,
	bytes: usize,
	last_used: u64,
}

struct Inner {
	entries: HashMap<CacheKey, Entry>,
	bytes: usize,
	clock: u64,
	hits: u64,
	misses: u64,
}

/// Thread-safe decoded-audio cache with a memory budget (least recently used
/// entries are evicted first). Concurrent requests for the same key decode once.
pub struct AudioCache {
	inner: Mutex<Inner>,
	max_bytes: usize,
}

impl Default for AudioCache {
	/// 2 GiB budget (about 1h40 of stereo audio at 48 kHz).
	fn default() -> Self {
		Self::new(2 << 30)
	}
}

impl AudioCache {
	/// Cache holding at most `max_bytes` of sample data. The most recently loaded
	/// buffer is always kept, even if it alone exceeds the budget.
	pub fn new(max_bytes: usize) -> Self {
		Self {
			inner: Mutex::new(Inner {
				entries: HashMap::new(),
				bytes: 0,
				clock: 0,
				hits: 0,
				misses: 0,
			}),
			max_bytes,
		}
	}

	/// Process-wide shared cache.
	pub fn global() -> &'static AudioCache {
		static GLOBAL: std::sync::OnceLock<AudioCache> = std::sync::OnceLock::new();
		GLOBAL.get_or_init(AudioCache::default)
	}

	/// Returns `path` decoded at `sample_rate` (all channels), decoding on a miss.
	pub fn get_or_load(
		&self,
		path: impl AsRef<Path>,
		sample_rate: u32,
	) -> Result<Arc<AudioBuffer>> {
		let path = path.as_ref();
		let key = CacheKey::for_file(path, sample_rate)?;
		let slot = {
			let mut inner = self.lock();
			inner.clock += 1;
			let now = inner.clock;
			match inner.entries.get_mut(&key) {
				Some(e) => {
					e.last_used = now;
					e.slot.clone()
				}
				None => {
					let slot: Slot = Arc::new(Mutex::new(None));
					inner.entries.insert(
						key.clone(),
						Entry {
							slot: slot.clone(),
							bytes: 0,
							last_used: now,
						},
					);
					slot
				}
			}
		};
		// Per-key lock: a second caller for the same file waits for the first decode.
		let mut guard = slot.lock().unwrap_or_else(|p| p.into_inner());
		if let Some(buf) = guard.as_ref() {
			self.lock().hits += 1;
			return Ok(buf.clone());
		}
		match decode_to_buffer(&key.path, sample_rate) {
			Ok(buf) => {
				let buf = Arc::new(buf);
				*guard = Some(buf.clone());
				drop(guard);
				self.finish_insert(&key, buf.byte_size());
				Ok(buf)
			}
			Err(e) => {
				drop(guard);
				let mut inner = self.lock();
				inner.misses += 1;
				inner.entries.remove(&key);
				Err(e)
			}
		}
	}

	/// Returns the cached buffer if present, without decoding.
	pub fn get(&self, path: impl AsRef<Path>, sample_rate: u32) -> Option<Arc<AudioBuffer>> {
		let key = CacheKey::for_file(path.as_ref(), sample_rate).ok()?;
		let slot = self.lock().entries.get(&key)?.slot.clone();
		slot.lock().unwrap_or_else(|p| p.into_inner()).clone()
	}

	/// Drops every cached rate/version of `path`.
	pub fn invalidate(&self, path: impl AsRef<Path>) {
		let path = path.as_ref();
		let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
		let mut inner = self.lock();
		let removed: usize = inner
			.entries
			.extract_if(|k, _| k.path == canonical)
			.map(|(_, e)| e.bytes)
			.sum();
		inner.bytes -= removed;
	}

	pub fn clear(&self) {
		let mut inner = self.lock();
		inner.entries.clear();
		inner.bytes = 0;
	}

	pub fn stats(&self) -> CacheStats {
		let inner = self.lock();
		CacheStats {
			entries: inner.entries.len(),
			bytes: inner.bytes,
			hits: inner.hits,
			misses: inner.misses,
		}
	}

	fn finish_insert(&self, key: &CacheKey, bytes: usize) {
		let mut inner = self.lock();
		inner.misses += 1;
		// Older versions of the same file (different mtime/size) can never hit again.
		let stale: usize = inner
			.entries
			.extract_if(|k, _| {
				k.path == key.path
					&& k.sample_rate == key.sample_rate
					&& (k.modified != key.modified || k.size != key.size)
			})
			.map(|(_, e)| e.bytes)
			.sum();
		inner.bytes -= stale;
		if let Some(e) = inner.entries.get_mut(key) {
			e.bytes = bytes;
			inner.bytes += bytes;
		}
		while inner.bytes > self.max_bytes {
			let victim = inner
				.entries
				.iter()
				.filter(|(k, e)| *k != key && e.bytes > 0)
				.min_by_key(|(_, e)| e.last_used)
				.map(|(k, _)| k.clone());
			let Some(victim) = victim else { break };
			if let Some(e) = inner.entries.remove(&victim) {
				inner.bytes -= e.bytes;
			}
		}
	}

	fn lock(&self) -> MutexGuard<'_, Inner> {
		self.inner.lock().unwrap_or_else(|p| p.into_inner())
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn write_wav(path: &Path, frames: usize, value: f32) {
		let spec = hound::WavSpec {
			channels: 1,
			sample_rate: 44_100,
			bits_per_sample: 32,
			sample_format: hound::SampleFormat::Float,
		};
		let mut w = hound::WavWriter::create(path, spec).unwrap();
		for _ in 0..frames {
			w.write_sample(value).unwrap();
		}
		w.finalize().unwrap();
	}

	#[test]
	fn hits_after_first_load_and_shares_the_buffer() {
		let dir = tempfile::tempdir().unwrap();
		let p = dir.path().join("a.wav");
		write_wav(&p, 1000, 0.25);
		let cache = AudioCache::new(1 << 20);
		let a = cache.get_or_load(&p, 44_100).unwrap();
		let b = cache.get_or_load(&p, 44_100).unwrap();
		assert!(Arc::ptr_eq(&a, &b));
		let s = cache.stats();
		assert_eq!((s.hits, s.misses, s.entries), (1, 1, 1));
		assert_eq!(s.bytes, 4000);
		// Another rate is another entry.
		let c = cache.get_or_load(&p, 48_000).unwrap();
		assert_eq!(c.sample_rate(), 48_000);
		assert_eq!(cache.stats().entries, 2);
		cache.invalidate(&p);
		assert_eq!(cache.stats().entries, 0);
		assert_eq!(cache.stats().bytes, 0);
	}

	#[test]
	fn file_change_produces_a_fresh_decode() {
		let dir = tempfile::tempdir().unwrap();
		let p = dir.path().join("b.wav");
		write_wav(&p, 100, 0.25);
		let cache = AudioCache::new(1 << 20);
		let a = cache.get_or_load(&p, 44_100).unwrap();
		write_wav(&p, 200, 0.5);
		let b = cache.get_or_load(&p, 44_100).unwrap();
		assert_eq!(a.frames(), 100);
		assert_eq!(b.frames(), 200);
		assert_eq!(cache.stats().entries, 1, "stale version evicted");
	}

	#[test]
	fn evicts_least_recently_used_over_budget() {
		let dir = tempfile::tempdir().unwrap();
		let paths: Vec<PathBuf> = (0..3)
			.map(|i| {
				let p = dir.path().join(format!("{i}.wav"));
				write_wav(&p, 1000, 0.1);
				p
			})
			.collect();
		let cache = AudioCache::new(8000); // room for two 4000-byte buffers
		cache.get_or_load(&paths[0], 44_100).unwrap();
		cache.get_or_load(&paths[1], 44_100).unwrap();
		cache.get_or_load(&paths[0], 44_100).unwrap(); // 0 is now most recent
		cache.get_or_load(&paths[2], 44_100).unwrap();
		assert!(cache.get(&paths[0], 44_100).is_some());
		assert!(cache.get(&paths[1], 44_100).is_none());
		assert!(cache.get(&paths[2], 44_100).is_some());
		assert_eq!(cache.stats().bytes, 8000);
	}

	#[test]
	fn concurrent_loads_decode_once() {
		let dir = tempfile::tempdir().unwrap();
		let p = dir.path().join("c.wav");
		write_wav(&p, 50_000, 0.3);
		let cache = Arc::new(AudioCache::new(1 << 24));
		let handles: Vec<_> = (0..8)
			.map(|_| {
				let cache = cache.clone();
				let p = p.clone();
				std::thread::spawn(move || cache.get_or_load(&p, 48_000).unwrap())
			})
			.collect();
		let bufs: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
		assert!(bufs.iter().all(|b| Arc::ptr_eq(b, &bufs[0])));
		let s = cache.stats();
		assert_eq!((s.misses, s.hits), (1, 7));
	}

	#[test]
	fn missing_file_is_an_io_error() {
		let cache = AudioCache::new(1024);
		let err = cache
			.get_or_load("/definitely/not/here.wav", 48_000)
			.unwrap_err();
		assert!(matches!(err, AudioIoError::Io { .. }), "{err}");
	}
}
