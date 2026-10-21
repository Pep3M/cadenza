//! Live signal of one device for its editor (analyzers, gain-reduction history, scopes).
//!
//! The control side picks at most one device ([`crate::Player::set_probe`]). While the
//! running graph contains it, every quantum the graph copies the device's stereo input
//! (before `process`) and output (after it) into a [`DeviceProbe`]: a preallocated
//! single-producer ring of atomics (the audio thread writes, readers drain), plus the
//! device's [`cadenza_dsp::Processor::readings`]. Writing is wait-free and allocation
//! free; a full ring drops the new frames and counts them.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use cadenza_dsp::readings;
use cadenza_model::modulation::MAX_MODULATORS;

use crate::QUANTUM;

/// Ring length in seconds of audio.
const PROBE_SECONDS: f64 = 0.5;
/// Floats per frame: input L/R, output L/R.
const FRAME: usize = 4;

/// One sample frame of the probed device.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ProbeFrame {
	/// What the device received (silence for instruments).
	pub input: [f32; 2],
	/// What it produced.
	pub output: [f32; 2],
}

/// Shared ring + readings of the probed device. Cheap to share through its `Arc`;
/// drain it from any thread (drains are serialized, never blocking the audio thread).
#[derive(Debug)]
pub struct DeviceProbe {
	sample_rate: u32,
	capacity: u64,
	slots: Box<[AtomicU32]>,
	/// Frames written / read so far (monotonic).
	write: AtomicU64,
	read: AtomicU64,
	/// First frame of the current target: earlier frames belong to the previous device.
	start: AtomicU64,
	dropped: AtomicU64,
	active: AtomicBool,
	readings: [AtomicU32; readings::MAX],
	reading_count: AtomicU32,
	modulation: [AtomicU32; MAX_MODULATORS],
	modulation_count: AtomicU32,
	/// Serializes readers only; the writer never touches it.
	reader: Mutex<()>,
}

impl DeviceProbe {
	/// A ring holding about half a second at `sample_rate`.
	pub fn new(sample_rate: u32) -> Self {
		let capacity = ((f64::from(sample_rate) * PROBE_SECONDS) as usize).max(QUANTUM * 4);
		let probe = Self {
			sample_rate,
			capacity: capacity as u64,
			slots: (0..capacity * FRAME).map(|_| AtomicU32::new(0)).collect(),
			write: AtomicU64::new(0),
			read: AtomicU64::new(0),
			start: AtomicU64::new(0),
			dropped: AtomicU64::new(0),
			active: AtomicBool::new(false),
			readings: std::array::from_fn(|_| AtomicU32::new(0)),
			reading_count: AtomicU32::new(0),
			modulation: std::array::from_fn(|_| AtomicU32::new(0)),
			modulation_count: AtomicU32::new(0),
			reader: Mutex::new(()),
		};
		// Where std's mutex is a pthread one (macOS), its first lock allocates it: do that
		// here, not in the first `drain` of a running meter.
		drop(probe.reader.lock());
		probe
	}

	pub fn sample_rate(&self) -> u32 {
		self.sample_rate
	}

	/// Ring length in frames.
	pub fn capacity(&self) -> usize {
		self.capacity as usize
	}

	/// Whether the probed device was processed in the last rendered quantum (false when
	/// it is not in the graph).
	pub fn is_active(&self) -> bool {
		self.active.load(Ordering::Relaxed)
	}

	/// Frames lost because the ring was full (readers too slow).
	pub fn dropped_frames(&self) -> u64 {
		self.dropped.load(Ordering::Relaxed)
	}

	/// The device's latest readings (see [`cadenza_dsp::readings`] for the indices);
	/// empty for devices that publish none.
	pub fn readings(&self) -> ([f32; readings::MAX], usize) {
		let n = (self.reading_count.load(Ordering::Acquire) as usize).min(readings::MAX);
		let mut out = [0.0; readings::MAX];
		for (o, a) in out.iter_mut().zip(&self.readings).take(n) {
			*o = f32::from_bits(a.load(Ordering::Relaxed));
		}
		(out, n)
	}

	/// Reading `index`, if the device published it.
	pub fn reading(&self, index: usize) -> Option<f32> {
		let (values, n) = self.readings();
		(index < n).then(|| values[index])
	}

	/// The current output of each of the device's modulators, in order (LFO -1..1,
	/// envelope 0..1); empty when it has none.
	pub fn modulation(&self) -> ([f32; MAX_MODULATORS], usize) {
		let n = (self.modulation_count.load(Ordering::Acquire) as usize).min(MAX_MODULATORS);
		let mut out = [0.0; MAX_MODULATORS];
		for (o, a) in out.iter_mut().zip(&self.modulation).take(n) {
			*o = f32::from_bits(a.load(Ordering::Relaxed));
		}
		(out, n)
	}

	/// Appends every frame written since the previous drain to `out`; returns how many.
	pub fn drain(&self, out: &mut Vec<ProbeFrame>) -> usize {
		let _guard = self.reader.lock().unwrap_or_else(|e| e.into_inner());
		let w = self.write.load(Ordering::Acquire);
		let r = self
			.read
			.load(Ordering::Relaxed)
			.max(self.start.load(Ordering::Acquire));
		let n = w.saturating_sub(r);
		out.reserve(n as usize);
		for f in r..w {
			let i = (f % self.capacity) as usize * FRAME;
			let s = |k: usize| f32::from_bits(self.slots[i + k].load(Ordering::Relaxed));
			out.push(ProbeFrame {
				input: [s(0), s(1)],
				output: [s(2), s(3)],
			});
		}
		self.read.store(w, Ordering::Release);
		n as usize
	}

	/// Audio thread: a new target starts; frames of the previous one are skipped.
	pub(crate) fn restart(&self) {
		self.start
			.store(self.write.load(Ordering::Relaxed), Ordering::Release);
		self.reading_count.store(0, Ordering::Release);
		self.modulation_count.store(0, Ordering::Release);
		self.active.store(false, Ordering::Relaxed);
	}

	/// Audio thread: one quantum of the device's input and output.
	pub(crate) fn push(&self, in_l: &[f32], in_r: &[f32], out_l: &[f32], out_r: &[f32]) {
		let w = self.write.load(Ordering::Relaxed);
		let r = self
			.read
			.load(Ordering::Acquire)
			.max(self.start.load(Ordering::Relaxed));
		let free = self.capacity - (w - r).min(self.capacity);
		let n = in_l.len().min(in_r.len()).min(out_l.len()).min(out_r.len());
		let k = (n as u64).min(free) as usize;
		for j in 0..k {
			let i = ((w + j as u64) % self.capacity) as usize * FRAME;
			self.slots[i].store(in_l[j].to_bits(), Ordering::Relaxed);
			self.slots[i + 1].store(in_r[j].to_bits(), Ordering::Relaxed);
			self.slots[i + 2].store(out_l[j].to_bits(), Ordering::Relaxed);
			self.slots[i + 3].store(out_r[j].to_bits(), Ordering::Relaxed);
		}
		self.write.store(w + k as u64, Ordering::Release);
		if k < n {
			self.dropped.fetch_add((n - k) as u64, Ordering::Relaxed);
		}
	}

	/// Audio thread: the readings of the quantum just pushed.
	pub(crate) fn publish_readings(&self, values: &[f32]) {
		let n = values.len().min(readings::MAX);
		for (a, v) in self.readings.iter().zip(&values[..n]) {
			a.store(v.to_bits(), Ordering::Relaxed);
		}
		self.reading_count.store(n as u32, Ordering::Release);
	}

	/// Audio thread: the modulator values of the quantum just pushed.
	pub(crate) fn publish_modulation(&self, values: &[f32]) {
		let n = values.len().min(MAX_MODULATORS);
		for (a, v) in self.modulation.iter().zip(&values[..n]) {
			a.store(v.to_bits(), Ordering::Relaxed);
		}
		self.modulation_count.store(n as u32, Ordering::Release);
	}

	/// Audio thread: whether the device ran in the last quantum.
	pub(crate) fn set_active(&self, active: bool) {
		self.active.store(active, Ordering::Relaxed);
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn ring_drops_when_full_and_skips_previous_targets() {
		let p = DeviceProbe::new(1000);
		let cap = p.capacity();
		let a = [1.0_f32; QUANTUM];
		let b = [2.0_f32; QUANTUM];
		let pushes = cap / QUANTUM + 2;
		for _ in 0..pushes {
			p.push(&a, &a, &b, &b);
		}
		assert_eq!(p.dropped_frames() as usize, pushes * QUANTUM - cap);
		let mut out = Vec::new();
		assert_eq!(p.drain(&mut out), cap);
		assert!(
			out.iter()
				.all(|f| f.input == [1.0; 2] && f.output == [2.0; 2])
		);
		p.push(&a, &a, &a, &a);
		p.restart();
		p.push(&b, &b, &b, &b);
		out.clear();
		assert_eq!(p.drain(&mut out), QUANTUM);
		assert!(out.iter().all(|f| f.input == [2.0; 2]));
		p.publish_readings(&[3.5]);
		assert_eq!(p.reading(0), Some(3.5));
		assert_eq!(p.reading(1), None);
	}
}
