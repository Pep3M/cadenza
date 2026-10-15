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
//! | `0..64` | header: magic, version, capacities, request fields; `REQ_SEQ` and `ACK_SEQ` sit on their own cache lines |
//! | `blocks` | `block_capacity` x 8 words: the sub-blocks of a request (frames, flags, tempo, beat, where their events and parameter changes end) |
//! | `events` | `event_capacity` x 4 words: kind, offset, pitch, velocity bits |
//! | `params` | `param_capacity` x 2 words: index, value bits |
//! | audio | `capacity` samples (`f32` bits) per channel: main left/right (processed in place), sidechain left/right, then left/right of each extra output |
//!
//! A process request carries one or more **sub-blocks**, laid out back to
//! back in the audio channels. The child makes one `Processor::process` call
//! per sub-block, applying that sub-block's parameter changes first and
//! handing it its events with offsets relative to the sub-block, so the host
//! can batch several engine quanta into one round trip without changing what
//! the plugin sees.

use std::sync::atomic::{AtomicU32, Ordering};

use cadenza_dsp::{MidiEvent, Transport};

pub(crate) const MAGIC: u32 = u32::from_le_bytes(*b"CDZS");
pub(crate) const VERSION: u32 = 3;

pub(crate) const W_MAGIC: usize = 0;
pub(crate) const W_VERSION: usize = 1;
pub(crate) const W_CAPACITY: usize = 2;
pub(crate) const W_EVENT_CAPACITY: usize = 3;
pub(crate) const W_PARAM_CAPACITY: usize = 4;
pub(crate) const W_AUX_OUTPUTS: usize = 5;
pub(crate) const W_BLOCK_CAPACITY: usize = 6;
/// Written by the host to post a request; the child futex-waits on it.
pub(crate) const W_REQ_SEQ: usize = 16;
pub(crate) const W_CMD: usize = 17;
/// `FLAG_*` bits of the request.
pub(crate) const W_FLAGS: usize = 18;
pub(crate) const W_EVENTS: usize = 19;
pub(crate) const W_PARAMS: usize = 20;
/// Sub-blocks of a `CMD_PROCESS` request.
pub(crate) const W_BLOCKS: usize = 21;
/// Written by the child when a request is done; the host futex-waits on it.
pub(crate) const W_ACK_SEQ: usize = 32;
const HEADER_WORDS: usize = 64;
const BLOCK_WORDS: usize = 12;

/// Request kinds in `W_CMD`.
pub(crate) const CMD_PROCESS: u32 = 1;
/// Only deliver the queued parameter changes (and a pending reset).
pub(crate) const CMD_PARAMS: u32 = 3;
/// Deliver queued parameter changes and call `Processor::idle` (transport
/// stopped while an editor is open).
pub(crate) const CMD_IDLE: u32 = 4;

/// `W_FLAGS`: call `Processor::reset` before anything else in the request.
pub(crate) const FLAG_RESET: u32 = 1;

const SUB_PLAYING: u32 = 1;
const SUB_KEYED: u32 = 2;

const EV_NOTE_ON: u32 = 1;
const EV_NOTE_OFF: u32 = 2;
const EV_ALL_OFF: u32 = 3;
/// Control change: the pitch word holds the controller number.
const EV_CC: u32 = 4;
const EV_BEND: u32 = 5;
const EV_PRESSURE: u32 = 6;

/// Upper bounds a child accepts from a header (the host never exceeds them).
pub(crate) const MAX_CAPACITY: u32 = 1 << 16;
pub(crate) const MAX_EVENTS: u32 = 1 << 14;
pub(crate) const MAX_PARAMS: u32 = 1 << 18;
pub(crate) const MAX_AUX_OUTPUTS: u32 = 32;
pub(crate) const MAX_BLOCKS: u32 = 64;

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
	/// Sub-blocks per request.
	pub block_capacity: usize,
}

impl Layout {
	fn blocks(&self) -> usize {
		HEADER_WORDS
	}
	pub fn events(&self) -> usize {
		self.blocks() + self.block_capacity * BLOCK_WORDS
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
		let (c, e, p, a, b) = (
			get(W_CAPACITY),
			get(W_EVENT_CAPACITY),
			get(W_PARAM_CAPACITY),
			get(W_AUX_OUTPUTS),
			get(W_BLOCK_CAPACITY),
		);
		if c == 0
			|| c > MAX_CAPACITY
			|| e > MAX_EVENTS
			|| p > MAX_PARAMS
			|| a > MAX_AUX_OUTPUTS
			|| b == 0 || b > MAX_BLOCKS
		{
			return Err(format!("invalid sandbox capacities {c}/{e}/{p}/{a}/{b}"));
		}
		let layout = Layout {
			capacity: c as usize,
			event_capacity: e as usize,
			param_capacity: p as usize,
			aux_outputs: a as usize,
			block_capacity: b as usize,
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
		words[W_BLOCK_CAPACITY].store(self.block_capacity as u32, Ordering::Relaxed);
		words[W_MAGIC].store(MAGIC, Ordering::Release);
	}
}

/// One `Processor::process` call of a request.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct SubBlock {
	pub frames: u32,
	pub transport: Transport,
	/// The sidechain channels carry a key for this sub-block.
	pub keyed: bool,
	/// One past this sub-block's last event in the event area.
	pub events_end: u32,
	/// One past the last parameter change to apply before this sub-block
	/// (the changes from the previous sub-block's end up to here).
	pub params_end: u32,
}

/// `e` with its offset replaced.
pub(crate) fn with_offset(e: MidiEvent, offset: u32) -> MidiEvent {
	e.with_offset(offset)
}

fn finite(v: f32) -> f32 {
	if v.is_finite() { v } else { 0.0 }
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

	fn store_f64(&self, i: usize, v: f64) {
		let bits = v.to_bits();
		self.store(i, bits as u32);
		self.store(i + 1, (bits >> 32) as u32);
	}

	fn load_f64(&self, i: usize) -> f64 {
		f64::from_bits(u64::from(self.load(i)) | (u64::from(self.load(i + 1)) << 32))
	}

	/// Sets the request kind, its `FLAG_*` bits and its sub-block count.
	pub fn set_request(&self, cmd: u32, flags: u32, blocks: usize) {
		self.store(W_CMD, cmd);
		self.store(W_FLAGS, flags);
		self.store(W_BLOCKS, blocks.min(self.layout.block_capacity) as u32);
	}

	pub fn cmd(&self) -> u32 {
		self.load(W_CMD)
	}

	pub fn flags(&self) -> u32 {
		self.load(W_FLAGS)
	}

	pub fn blocks(&self) -> usize {
		(self.load(W_BLOCKS) as usize).min(self.layout.block_capacity)
	}

	/// Stores sub-block `i` (ignored past the capacity).
	pub fn write_sub_block(&self, i: usize, b: &SubBlock) {
		if i >= self.layout.block_capacity {
			return;
		}
		let w = self.layout.blocks() + i * BLOCK_WORDS;
		self.store(w, b.frames.min(self.layout.capacity as u32));
		let mut flags = 0;
		if b.transport.playing {
			flags |= SUB_PLAYING;
		}
		if b.keyed {
			flags |= SUB_KEYED;
		}
		self.store(w + 1, flags);
		self.store_f64(w + 2, b.transport.bpm);
		self.store_f64(w + 4, b.transport.beat_position);
		self.store(w + 6, b.events_end);
		self.store(w + 7, b.params_end);
		self.store(
			w + 8,
			u32::from(b.transport.time_sig_numerator)
				| (u32::from(b.transport.time_sig_denominator) << 16),
		);
		self.store(w + 9, b.transport.bar_number as u32);
		self.store_f64(w + 10, b.transport.bar_start);
	}

	/// Sub-block `i` as the other side wrote it (frames clamped to the
	/// capacity; readers clamp the event and parameter ends).
	pub fn sub_block(&self, i: usize) -> SubBlock {
		let i = i.min(self.layout.block_capacity - 1);
		let w = self.layout.blocks() + i * BLOCK_WORDS;
		let flags = self.load(w + 1);
		SubBlock {
			frames: self.load(w).min(self.layout.capacity as u32),
			transport: Transport {
				bpm: self.load_f64(w + 2),
				playing: flags & SUB_PLAYING != 0,
				beat_position: self.load_f64(w + 4),
				time_sig_numerator: (self.load(w + 8) & 0xffff) as u16,
				time_sig_denominator: (self.load(w + 8) >> 16) as u16,
				bar_number: self.load(w + 9) as i32,
				bar_start: self.load_f64(w + 10),
			},
			keyed: flags & SUB_KEYED != 0,
			events_end: self.load(w + 6),
			params_end: self.load(w + 7),
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
				MidiEvent::ControlChange {
					offset,
					controller,
					value,
				} => (EV_CC, offset, controller, value),
				MidiEvent::PitchBend { offset, value } => (EV_BEND, offset, 0, value),
				MidiEvent::ChannelPressure { offset, value } => (EV_PRESSURE, offset, 0, value),
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
	/// it). Entries of an unknown kind decode as a note-off for key 0, so
	/// indices in `out` keep matching the sub-block table.
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
				EV_ALL_OFF => MidiEvent::AllNotesOff { offset },
				EV_NOTE_OFF => MidiEvent::NoteOff { offset, pitch },
				EV_CC => MidiEvent::ControlChange {
					offset,
					controller: pitch,
					value: finite(velocity).clamp(0.0, 1.0),
				},
				EV_BEND => MidiEvent::PitchBend {
					offset,
					value: finite(velocity).clamp(-1.0, 1.0),
				},
				EV_PRESSURE => MidiEvent::ChannelPressure {
					offset,
					value: finite(velocity).clamp(0.0, 1.0),
				},
				_ => MidiEvent::NoteOff { offset, pitch: 0 },
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

	pub fn param_count(&self) -> usize {
		(self.load(W_PARAMS) as usize).min(self.layout.param_capacity)
	}

	/// Calls `f` for parameter changes `from..to` (clamped to the count).
	pub fn for_each_param(&self, from: usize, to: usize, mut f: impl FnMut(usize, f32)) {
		let to = to.min(self.param_count());
		let base = self.layout.params();
		for i in from.min(to)..to {
			let w = base + i * 2;
			f(self.load(w) as usize, f32::from_bits(self.load(w + 1)));
		}
	}

	/// Copies `src` into audio channel `c` starting at frame `offset`
	/// (ignored for channels the layout does not have, truncated at the
	/// capacity).
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

	/// Reads audio channel `c` from frame `offset` into `dst` (silence for
	/// missing channels and past the capacity).
	pub fn read_audio_at(&self, c: usize, offset: usize, dst: &mut [f32]) {
		if c >= self.layout.channels() || offset >= self.layout.capacity {
			dst.fill(0.0);
			return;
		}
		let base = self.layout.channel(c) + offset;
		let n = dst.len().min(self.layout.capacity - offset);
		for (x, w) in dst[..n].iter_mut().zip(&self.words[base..base + n]) {
			*x = f32::from_bits(w.load(Ordering::Relaxed));
		}
		dst[n..].fill(0.0);
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
						0o600 as libc::c_uint,
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
			return Err(format!("mmap failed: {}", errno()));
		}
		// Both audio threads touch every page of the block on each request: keep
		// it resident so a page fault can never stall a round trip. Best effort
		// (RLIMIT_MEMLOCK may be small); unlocked memory still works.
		// SAFETY: locking our own fresh mapping of `len` bytes.
		unsafe { libc::mlock(ptr, len) };
		Ok(ptr)
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
			block_capacity: 2,
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
			time_sig_numerator: 7,
			time_sig_denominator: 8,
			bar_start: 3.0,
			bar_number: 2,
		};
		ba.set_request(CMD_PROCESS, FLAG_RESET, 5);
		assert_eq!(bb.cmd(), CMD_PROCESS);
		assert_eq!(bb.flags(), FLAG_RESET);
		assert_eq!(bb.blocks(), 2, "sub-blocks are clamped to the capacity");
		let sub = SubBlock {
			frames: 1000,
			transport: t,
			keyed: true,
			events_end: 2,
			params_end: 1,
		};
		ba.write_sub_block(1, &sub);
		ba.write_sub_block(2, &sub);
		assert_eq!(
			bb.sub_block(1),
			SubBlock { frames: 64, ..sub },
			"frames are clamped to the capacity"
		);
		assert_eq!(bb.sub_block(9), bb.sub_block(1), "indices are clamped");
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
		let controllers = [
			MidiEvent::ControlChange {
				offset: 1,
				controller: 64,
				value: 1.0,
			},
			MidiEvent::PitchBend {
				offset: 2,
				value: -0.5,
			},
			MidiEvent::ChannelPressure {
				offset: 3,
				value: 0.25,
			},
		];
		assert_eq!(ba.write_events_shifted(&controllers, 0), 3);
		bb.read_events(&mut out);
		assert_eq!(out, controllers);
		assert_eq!(
			with_offset(events[0], 9),
			MidiEvent::NoteOn {
				offset: 9,
				pitch: 60,
				velocity: 0.5
			}
		);
		ba.write_audio_at(CH_RIGHT, 0, &[0.5, -0.25]);
		let mut r = [0.0; 2];
		bb.read_audio_at(CH_RIGHT, 0, &mut r);
		assert_eq!(r, [0.5, -0.25]);
		ba.write_audio_at(CH_AUX + 1, 63, &[0.125, 7.0]);
		bb.read_audio_at(CH_AUX + 1, 63, &mut r);
		assert_eq!(r, [0.125, 0.0], "truncated at the capacity");
		ba.write_audio_at(CH_AUX + 2, 0, &[1.0]);
		bb.read_audio_at(CH_AUX + 2, 0, &mut r);
		assert_eq!(r, [0.0, 0.0], "missing channels read as silence");
		assert!(ba.write_param(1, 7, 0.75) && !ba.write_param(2, 0, 0.0));
		ba.set_param_count(9);
		let mut seen = Vec::new();
		bb.for_each_param(0, usize::MAX, |i, v| seen.push((i, v)));
		assert_eq!(seen.len(), 2);
		assert_eq!(seen[1], (7, 0.75));
		seen.clear();
		bb.for_each_param(1, 1, |i, v| seen.push((i, v)));
		assert!(seen.is_empty());
	}

	#[test]
	fn corrupt_headers_are_rejected() {
		let words: Vec<AtomicU32> = (0..layout().words()).map(|_| AtomicU32::new(0)).collect();
		assert!(Layout::from_header(&words).is_err());
		layout().write_header(&words);
		assert!(Layout::from_header(&words).is_ok());
		words[W_BLOCK_CAPACITY].store(0, Ordering::Relaxed);
		assert!(Layout::from_header(&words).is_err());
		words[W_BLOCK_CAPACITY].store(2, Ordering::Relaxed);
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
		words[W_BLOCKS].store(u32::MAX, Ordering::Relaxed);
		let mut out = Vec::with_capacity(16);
		block.read_events(&mut out);
		assert!(out.len() <= 3);
		let mut n = 0;
		block.for_each_param(0, usize::MAX, |_, _| n += 1);
		assert_eq!(n, 2);
		assert_eq!(block.blocks(), 2);
	}
}
