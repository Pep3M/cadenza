//! The shared-memory block exchanged between the host and one sandbox child.
//!
//! Everything in the region is a 32-bit word accessed only through
//! [`AtomicU32`] (relaxed for payload, acquire/release on the sequence
//! numbers). The other process may write any word at any time, so treating the
//! memory as atomics keeps concurrent access well-defined in Rust's memory
//! model, and the host never derives an index, length or pointer from a word
//! the child wrote: it only reads the acknowledgement sequence number and the
//! audio samples of a frame range it chose itself.
//!
//! Layout (word offsets; one word = 4 bytes, the region is page aligned):
//!
//! | words | content |
//! |---|---|
//! | `0..64` | header: magic, version, capacities, request fields, transport; `REQ_SEQ` and `ACK_SEQ` sit on their own cache lines |
//! | `events` | `event_capacity` x 4 words: kind, offset, pitch, velocity bits |
//! | `params` | `param_capacity` x 2 words: index, value bits |
//! | audio | `capacity` samples (`f32` bits) per channel: main left/right (processed in place), sidechain left/right, then left/right of each extra output |

use std::sync::atomic::{AtomicU32, Ordering};

use cadenza_dsp::{MidiEvent, Transport};

pub(crate) const MAGIC: u32 = u32::from_le_bytes(*b"CDZS");
pub(crate) const VERSION: u32 = 2;

pub(crate) const W_MAGIC: usize = 0;
pub(crate) const W_VERSION: usize = 1;
pub(crate) const W_CAPACITY: usize = 2;
pub(crate) const W_EVENT_CAPACITY: usize = 3;
pub(crate) const W_PARAM_CAPACITY: usize = 4;
pub(crate) const W_AUX_OUTPUTS: usize = 5;
/// Written by the host to post a request; the child futex-waits on it.
pub(crate) const W_REQ_SEQ: usize = 16;
pub(crate) const W_CMD: usize = 17;
pub(crate) const W_FRAMES: usize = 18;
pub(crate) const W_EVENTS: usize = 19;
pub(crate) const W_PARAMS: usize = 20;
pub(crate) const W_PLAYING: usize = 21;
pub(crate) const W_BPM: usize = 22; // + 23
pub(crate) const W_BEAT: usize = 24; // + 25
/// Non-zero when the sidechain channels carry a signal for this request.
pub(crate) const W_SIDECHAIN: usize = 26;
/// Written by the child when a request is done; the host futex-waits on it.
pub(crate) const W_ACK_SEQ: usize = 32;
const HEADER_WORDS: usize = 64;

/// Request kinds in `W_CMD`.
pub(crate) const CMD_PROCESS: u32 = 1;
pub(crate) const CMD_RESET: u32 = 2;
/// Only deliver the queued parameter changes (before a state save).
pub(crate) const CMD_PARAMS: u32 = 3;

const EV_NOTE_ON: u32 = 1;
const EV_NOTE_OFF: u32 = 2;
const EV_ALL_OFF: u32 = 3;

/// Upper bounds a child accepts from a header (the host never exceeds them).
pub(crate) const MAX_CAPACITY: u32 = 1 << 16;
pub(crate) const MAX_EVENTS: u32 = 1 << 14;
pub(crate) const MAX_PARAMS: u32 = 1 << 16;
pub(crate) const MAX_AUX_OUTPUTS: u32 = 32;

/// Audio channel indices (see [`Layout::channel`]).
pub(crate) const CH_LEFT: usize = 0;
pub(crate) const CH_RIGHT: usize = 1;
pub(crate) const CH_SIDECHAIN: usize = 2;
/// Left channel of extra output `k` is `CH_AUX + 2 * k`, right is `+ 1`.
pub(crate) const CH_AUX: usize = 4;

/// Sizes of the variable sections; offsets derive from them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Layout {
	pub capacity: usize,
	pub event_capacity: usize,
	pub param_capacity: usize,
	/// Extra stereo outputs transferred after each block.
	pub aux_outputs: usize,
}

impl Layout {
	pub fn events(&self) -> usize {
		HEADER_WORDS
	}
	pub fn params(&self) -> usize {
		self.events() + self.event_capacity * 4
	}
	fn audio(&self) -> usize {
		self.params() + self.param_capacity * 2
	}
	pub fn channels(&self) -> usize {
		CH_AUX + 2 * self.aux_outputs
	}
	/// First word of audio channel `c` (`c < channels()`).
	pub fn channel(&self, c: usize) -> usize {
		self.audio() + c * self.capacity
	}
	pub fn words(&self) -> usize {
		self.channel(self.channels())
	}
	pub fn bytes(&self) -> usize {
		self.words() * 4
	}

	/// Reads and validates the layout a header declares, against the size of
	/// the mapping that holds it.
	pub fn from_header(words: &[AtomicU32]) -> Result<Layout, String> {
		let get = |i: usize| words.get(i).map_or(0, |w| w.load(Ordering::Acquire));
		if words.len() < HEADER_WORDS || get(W_MAGIC) != MAGIC {
			return Err("shared memory has no sandbox header".into());
		}
		if get(W_VERSION) != VERSION {
			return Err(format!(
				"sandbox protocol version {} (expected {VERSION})",
				get(W_VERSION)
			));
		}
		let (c, e, p, a) = (
			get(W_CAPACITY),
			get(W_EVENT_CAPACITY),
			get(W_PARAM_CAPACITY),
			get(W_AUX_OUTPUTS),
		);
		if c == 0 || c > MAX_CAPACITY || e > MAX_EVENTS || p > MAX_PARAMS || a > MAX_AUX_OUTPUTS {
			return Err(format!("invalid sandbox capacities {c}/{e}/{p}/{a}"));
		}
		let layout = Layout {
			capacity: c as usize,
			event_capacity: e as usize,
			param_capacity: p as usize,
			aux_outputs: a as usize,
		};
		if layout.words() > words.len() {
			return Err("shared memory is smaller than its header declares".into());
		}
		Ok(layout)
	}

	pub fn write_header(&self, words: &[AtomicU32]) {
		words[W_VERSION].store(VERSION, Ordering::Relaxed);
		words[W_CAPACITY].store(self.capacity as u32, Ordering::Relaxed);
		words[W_EVENT_CAPACITY].store(self.event_capacity as u32, Ordering::Relaxed);
		words[W_PARAM_CAPACITY].store(self.param_capacity as u32, Ordering::Relaxed);
		words[W_AUX_OUTPUTS].store(self.aux_outputs as u32, Ordering::Relaxed);
		words[W_MAGIC].store(MAGIC, Ordering::Release);
	}
}

/// A typed view of a mapped region with a validated [`Layout`]. All accessors
/// clamp counts to the layout's capacities, so no word outside the region is
/// ever touched whatever the other side wrote.
#[derive(Clone, Copy)]
pub(crate) struct Block<'a> {
	pub words: &'a [AtomicU32],
	pub layout: Layout,
}

impl<'a> Block<'a> {
	pub fn new(words: &'a [AtomicU32], layout: Layout) -> Option<Self> {
		(layout.words() <= words.len()).then_some(Self { words, layout })
	}

	pub fn word(&self, i: usize) -> &'a AtomicU32 {
		&self.words[i]
	}

	fn store(&self, i: usize, v: u32) {
		self.words[i].store(v, Ordering::Relaxed);
	}

	fn load(&self, i: usize) -> u32 {
		self.words[i].load(Ordering::Relaxed)
	}

	pub fn set_sidechain(&self, active: bool) {
		self.store(W_SIDECHAIN, u32::from(active));
	}

	pub fn sidechain(&self) -> bool {
		self.load(W_SIDECHAIN) != 0
	}

	pub fn set_request(&self, cmd: u32, frames: usize, transport: &Transport) {
		self.store(W_CMD, cmd);
		self.store(W_FRAMES, frames.min(self.layout.capacity) as u32);
		self.store(W_PLAYING, u32::from(transport.playing));
		let bpm = transport.bpm.to_bits();
		self.store(W_BPM, bpm as u32);
		self.store(W_BPM + 1, (bpm >> 32) as u32);
		let beat = transport.beat_position.to_bits();
		self.store(W_BEAT, beat as u32);
		self.store(W_BEAT + 1, (beat >> 32) as u32);
	}

	pub fn cmd(&self) -> u32 {
		self.load(W_CMD)
	}

	pub fn frames(&self) -> usize {
		(self.load(W_FRAMES) as usize).min(self.layout.capacity)
	}

	pub fn transport(&self) -> Transport {
		let f64_at = |i: usize| {
			f64::from_bits(u64::from(self.load(i)) | (u64::from(self.load(i + 1)) << 32))
		};
		Transport {
			bpm: f64_at(W_BPM),
			playing: self.load(W_PLAYING) != 0,
			beat_position: f64_at(W_BEAT),
		}
	}

	/// Writes up to `event_capacity` events with their offsets moved back by
	/// `shift` frames (the rest are dropped, as an in-process plugin's fixed
	/// event list would) and returns how many fit.
	pub fn write_events_shifted(&self, events: &[MidiEvent], shift: u32) -> usize {
		let n = events.len().min(self.layout.event_capacity);
		let base = self.layout.events();
		for (i, e) in events[..n].iter().enumerate() {
			let w = base + i * 4;
			let (kind, offset, pitch, velocity) = match *e {
				MidiEvent::NoteOn {
					offset,
					pitch,
					velocity,
				} => (EV_NOTE_ON, offset, pitch, velocity),
				MidiEvent::NoteOff { offset, pitch } => (EV_NOTE_OFF, offset, pitch, 0.0),
				MidiEvent::AllNotesOff { offset } => (EV_ALL_OFF, offset, 0, 0.0),
			};
			self.store(w, kind);
			self.store(w + 1, offset.saturating_sub(shift));
			self.store(w + 2, u32::from(pitch));
			self.store(w + 3, velocity.to_bits());
		}
		self.store(W_EVENTS, n as u32);
		n
	}

	/// Decodes the events of the current request into `out` (without growing
	/// it); malformed entries are skipped.
	pub fn read_events(&self, out: &mut Vec<MidiEvent>) {
		out.clear();
		let n = (self.load(W_EVENTS) as usize).min(self.layout.event_capacity);
		let base = self.layout.events();
		for i in 0..n {
			if out.len() == out.capacity() {
				break;
			}
			let w = base + i * 4;
			let offset = self.load(w + 1);
			let pitch = self.load(w + 2).min(127) as u8;
			let velocity = f32::from_bits(self.load(w + 3));
			let event = match self.load(w) {
				EV_NOTE_ON => MidiEvent::NoteOn {
					offset,
					pitch,
					velocity: if velocity.is_finite() {
						velocity.clamp(0.0, 1.0)
					} else {
						0.0
					},
				},
				EV_NOTE_OFF => MidiEvent::NoteOff { offset, pitch },
				EV_ALL_OFF => MidiEvent::AllNotesOff { offset },
				_ => continue,
			};
			out.push(event);
		}
	}

	/// Stores parameter change `slot` of the current request.
	pub fn write_param(&self, slot: usize, index: u32, value: f32) -> bool {
		if slot >= self.layout.param_capacity {
			return false;
		}
		let w = self.layout.params() + slot * 2;
		self.store(w, index);
		self.store(w + 1, value.to_bits());
		true
	}

	pub fn set_param_count(&self, n: usize) {
		self.store(W_PARAMS, n.min(self.layout.param_capacity) as u32);
	}

	pub fn for_each_param(&self, mut f: impl FnMut(usize, f32)) {
		let n = (self.load(W_PARAMS) as usize).min(self.layout.param_capacity);
		let base = self.layout.params();
		for i in 0..n {
			let w = base + i * 2;
			f(self.load(w) as usize, f32::from_bits(self.load(w + 1)));
		}
	}

	/// Copies `src` into audio channel `c` starting at frame 0 (ignored for
	/// channels the layout does not have).
	pub fn write_audio(&self, c: usize, src: &[f32]) {
		self.write_audio_at(c, 0, src);
	}

	/// Like [`Self::write_audio`], starting at frame `offset`.
	pub fn write_audio_at(&self, c: usize, offset: usize, src: &[f32]) {
		if c >= self.layout.channels() || offset >= self.layout.capacity {
			return;
		}
		let base = self.layout.channel(c) + offset;
		let n = src.len().min(self.layout.capacity - offset);
		for (w, &x) in self.words[base..base + n].iter().zip(src) {
			w.store(x.to_bits(), Ordering::Relaxed);
		}
	}

	/// Reads audio channel `c` into `dst` (silence for missing channels).
	pub fn read_audio(&self, c: usize, dst: &mut [f32]) {
		if c >= self.layout.channels() {
			dst.fill(0.0);
			return;
		}
		let base = self.layout.channel(c);
		let n = dst.len().min(self.layout.capacity);
		for (x, w) in dst[..n].iter_mut().zip(&self.words[base..base + n]) {
			*x = f32::from_bits(w.load(Ordering::Relaxed));
		}
	}
}

// --------------------------------------------------------------- regions

/// A named shared-memory mapping. The host creates it (and unlinks the name as
/// soon as the child has mapped it, so nothing is left behind even if both
/// processes crash afterwards); the child opens it by name.
pub(crate) struct Region {
	inner: imp::Region,
}

impl Region {
	pub fn create(bytes: usize) -> Result<Region, String> {
		imp::Region::create(bytes).map(|inner| Region { inner })
	}

	pub fn open(name: &str) -> Result<Region, String> {
		imp::Region::open(name).map(|inner| Region { inner })
	}

	pub fn name(&self) -> &str {
		self.inner.name()
	}

	/// Removes the name; existing mappings stay valid. Idempotent.
	pub fn unlink(&mut self) {
		self.inner.unlink();
	}

	pub fn words(&self) -> &[AtomicU32] {
		let (ptr, len) = self.inner.raw();
		// SAFETY: the mapping is page aligned, `len` bytes long and lives as
		// long as `self`; it is only ever accessed through atomics.
		unsafe { std::slice::from_raw_parts(ptr.cast::<AtomicU32>(), len / 4) }
	}
}

#[cfg(unix)]
mod imp {
	use std::ffi::CString;
	use std::sync::atomic::{AtomicU64, Ordering};

	pub struct Region {
		ptr: *mut libc::c_void,
		len: usize,
		name: String,
		owner: bool,
		unlinked: bool,
	}

	// SAFETY: the mapping is plain shared memory accessed through atomics.
	unsafe impl Send for Region {}
	unsafe impl Sync for Region {}

	/// Linux accepts (and always applies) close-on-exec for shm descriptors;
	/// other systems may reject the flag, and the descriptor is closed right
	/// after mapping anyway.
	#[cfg(target_os = "linux")]
	const CLOEXEC: libc::c_int = libc::O_CLOEXEC;
	#[cfg(not(target_os = "linux"))]
	const CLOEXEC: libc::c_int = 0;

	fn errno() -> String {
		std::io::Error::last_os_error().to_string()
	}

	fn unique_name() -> String {
		static COUNTER: AtomicU64 = AtomicU64::new(0);
		let nanos = std::time::SystemTime::now()
			.duration_since(std::time::UNIX_EPOCH)
			.map_or(0, |d| d.subsec_nanos());
		// macOS limits shm names to 31 bytes.
		format!(
			"/cdz{:x}.{:x}.{:x}",
			std::process::id(),
			COUNTER.fetch_add(1, Ordering::Relaxed),
			nanos & 0xfffff
		)
	}

	impl Region {
		pub fn create(bytes: usize) -> Result<Region, String> {
			let len = bytes.max(1).next_multiple_of(4096);
			for _ in 0..8 {
				let name = unique_name();
				let cname = CString::new(name.clone()).map_err(|e| e.to_string())?;
				// SAFETY: plain libc calls on a valid C string.
				let fd = unsafe {
					libc::shm_open(
						cname.as_ptr(),
						libc::O_CREAT | libc::O_EXCL | libc::O_RDWR | CLOEXEC,
						0o600 as libc::mode_t,
					)
				};
				if fd < 0 {
					if std::io::Error::last_os_error().kind() == std::io::ErrorKind::AlreadyExists {
						continue;
					}
					return Err(format!("shm_open failed: {}", errno()));
				}
				let mut region = Region {
					ptr: std::ptr::null_mut(),
					len,
					name,
					owner: true,
					unlinked: false,
				};
				// SAFETY: `fd` is a fresh shm descriptor we own.
				let result = unsafe {
					if libc::ftruncate(fd, len as libc::off_t) != 0 {
						Err(format!("cannot size shared memory: {}", errno()))
					} else {
						map(fd, len)
					}
				};
				// SAFETY: closing our own descriptor; the mapping keeps the memory.
				unsafe { libc::close(fd) };
				region.ptr = result?;
				return Ok(region);
			}
			Err("could not find a free shared memory name".into())
		}

		pub fn open(name: &str) -> Result<Region, String> {
			let cname = CString::new(name).map_err(|e| e.to_string())?;
			// SAFETY: plain libc calls on a valid C string / our own fd.
			unsafe {
				let fd = libc::shm_open(cname.as_ptr(), libc::O_RDWR | CLOEXEC, 0);
				if fd < 0 {
					return Err(format!("cannot open shared memory '{name}': {}", errno()));
				}
				let mut st: libc::stat = std::mem::zeroed();
				let result = if libc::fstat(fd, &mut st) != 0 {
					Err(format!("cannot stat shared memory: {}", errno()))
				} else {
					let len = st.st_size as usize;
					map(fd, len).map(|ptr| (ptr, len))
				};
				libc::close(fd);
				let (ptr, len) = result?;
				Ok(Region {
					ptr,
					len,
					name: name.to_string(),
					owner: false,
					unlinked: true,
				})
			}
		}

		pub fn name(&self) -> &str {
			&self.name
		}

		pub fn raw(&self) -> (*const u8, usize) {
			(self.ptr.cast_const().cast(), self.len)
		}

		pub fn unlink(&mut self) {
			if self.owner && !self.unlinked {
				self.unlinked = true;
				if let Ok(c) = CString::new(self.name.clone()) {
					// SAFETY: valid C string.
					unsafe { libc::shm_unlink(c.as_ptr()) };
				}
			}
		}
	}

	/// # Safety
	/// `fd` must be a valid shared memory descriptor of at least `len` bytes.
	unsafe fn map(fd: libc::c_int, len: usize) -> Result<*mut libc::c_void, String> {
		if len < 4096 {
			return Err("shared memory region is too small".into());
		}
		// SAFETY: guaranteed by the caller.
		let ptr = unsafe {
			libc::mmap(
				std::ptr::null_mut(),
				len,
				libc::PROT_READ | libc::PROT_WRITE,
				libc::MAP_SHARED,
				fd,
				0,
			)
		};
		if ptr == libc::MAP_FAILED {
			Err(format!("mmap failed: {}", errno()))
		} else {
			Ok(ptr)
		}
	}

	impl Drop for Region {
		fn drop(&mut self) {
			self.unlink();
			if !self.ptr.is_null() {
				// SAFETY: unmapping our own mapping; no references outlive `self`.
				unsafe { libc::munmap(self.ptr, self.len) };
			}
		}
	}
}

/// Windows would use a named `CreateFileMappingW`/`MapViewOfFile` section
/// (closed rather than unlinked) behind the same interface. Not implemented
/// yet: sandboxed hosting reports itself unsupported on non-Unix systems.
#[cfg(not(unix))]
mod imp {
	pub struct Region;

	const UNSUPPORTED: &str = "sandboxed plugin hosting is not implemented on this OS yet";

	impl Region {
		pub fn create(_bytes: usize) -> Result<Region, String> {
			Err(UNSUPPORTED.into())
		}
		pub fn open(_name: &str) -> Result<Region, String> {
			Err(UNSUPPORTED.into())
		}
		pub fn name(&self) -> &str {
			""
		}
		pub fn raw(&self) -> (*const u8, usize) {
			(std::ptr::NonNull::<u32>::dangling().as_ptr().cast(), 0)
		}
		pub fn unlink(&mut self) {}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn layout() -> Layout {
		Layout {
			capacity: 64,
			event_capacity: 3,
			param_capacity: 2,
			aux_outputs: 1,
		}
	}

	#[cfg(unix)]
	#[test]
	fn region_round_trips_a_block_and_unlinks() {
		let mut a = Region::create(layout().bytes()).unwrap();
		layout().write_header(a.words());
		let b = Region::open(a.name()).unwrap();
		a.unlink();
		assert!(Region::open(a.name()).is_err(), "name is gone after unlink");
		let la = Layout::from_header(a.words()).unwrap();
		let lb = Layout::from_header(b.words()).unwrap();
		assert_eq!(la, lb);
		let (ba, bb) = (
			Block::new(a.words(), la).unwrap(),
			Block::new(b.words(), lb).unwrap(),
		);
		let t = Transport {
			bpm: 97.5,
			playing: true,
			beat_position: 3.25,
		};
		ba.set_request(CMD_PROCESS, 1000, &t);
		assert_eq!(bb.frames(), 64, "frames are clamped to the capacity");
		assert_eq!(bb.transport(), t);
		let events = [
			MidiEvent::NoteOn {
				offset: 1,
				pitch: 60,
				velocity: 0.5,
			},
			MidiEvent::NoteOff {
				offset: 2,
				pitch: 61,
			},
			MidiEvent::AllNotesOff { offset: 3 },
			MidiEvent::AllNotesOff { offset: 4 },
		];
		assert_eq!(ba.write_events_shifted(&events, 0), 3);
		let mut out = Vec::with_capacity(8);
		bb.read_events(&mut out);
		assert_eq!(out, events[..3]);
		ba.write_audio(CH_RIGHT, &[0.5, -0.25]);
		let mut r = [0.0; 2];
		bb.read_audio(CH_RIGHT, &mut r);
		assert_eq!(r, [0.5, -0.25]);
		ba.write_audio(CH_AUX + 1, &[0.125]);
		bb.read_audio(CH_AUX + 1, &mut r[..1]);
		assert_eq!(r[0], 0.125);
		ba.write_audio(CH_AUX + 2, &[1.0]);
		bb.read_audio(CH_AUX + 2, &mut r);
		assert_eq!(r, [0.0, 0.0], "missing channels read as silence");
		ba.set_sidechain(true);
		assert!(bb.sidechain());
		assert!(ba.write_param(1, 7, 0.75) && !ba.write_param(2, 0, 0.0));
		ba.set_param_count(9);
		let mut seen = Vec::new();
		bb.for_each_param(|i, v| seen.push((i, v)));
		assert_eq!(seen.len(), 2);
		assert_eq!(seen[1], (7, 0.75));
	}

	#[test]
	fn corrupt_headers_are_rejected() {
		let words: Vec<AtomicU32> = (0..layout().words()).map(|_| AtomicU32::new(0)).collect();
		assert!(Layout::from_header(&words).is_err());
		layout().write_header(&words);
		assert!(Layout::from_header(&words).is_ok());
		words[W_CAPACITY].store(1 << 20, Ordering::Relaxed);
		assert!(Layout::from_header(&words).is_err());
		words[W_CAPACITY].store(4096, Ordering::Relaxed);
		assert!(
			Layout::from_header(&words).is_err(),
			"larger than the mapping"
		);
		// Counts written by the other side are clamped.
		words[W_CAPACITY].store(64, Ordering::Relaxed);
		let block = Block::new(&words, layout()).unwrap();
		words[W_EVENTS].store(u32::MAX, Ordering::Relaxed);
		words[W_PARAMS].store(u32::MAX, Ordering::Relaxed);
		let mut out = Vec::with_capacity(16);
		block.read_events(&mut out);
		assert!(out.len() <= 3);
		let mut n = 0;
		block.for_each_param(|_, _| n += 1);
		assert_eq!(n, 2);
	}
}
