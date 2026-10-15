//! Lock-free level metering for the UI.
//!
//! The graph measures every track's post-fader signal and the master output once per
//! quantum (plain floats owned by the graph), and publishes them into a shared
//! [`MeterBank`] of atomics at the end of each [`crate::Graph::process`] call. Publishing
//! is wait-free and allocation-free: peaks use `fetch_max` on the float bit pattern
//! (non-negative IEEE floats order like their bits), RMS is a plain store.
//!
//! Readers ([`MeterBank::track`], [`MeterBank::master`]) take the peak accumulated since
//! their previous read (so no transient is lost between two UI frames) and the current
//! RMS, already smoothed with a 300 ms integration time. Peak hold and fall-off are the
//! reader's job.

use std::sync::atomic::{AtomicU32, Ordering};

use cadenza_core::TrackId;

use crate::QUANTUM;

/// RMS integration time (VU-like ballistics).
const RMS_SECONDS: f32 = 0.3;

/// Levels of one stereo channel strip, linear amplitude (1.0 = 0 dBFS).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct MeterReading {
	/// Highest absolute sample per channel since the previous read.
	pub peak: [f32; 2],
	/// Smoothed RMS per channel.
	pub rms: [f32; 2],
}

impl MeterReading {
	pub fn max_peak(&self) -> f32 {
		self.peak[0].max(self.peak[1])
	}
}

/// Atomic levels of one channel strip.
#[derive(Debug, Default)]
struct ChannelMeter {
	peak: [AtomicU32; 2],
	rms: [AtomicU32; 2],
}

impl ChannelMeter {
	fn publish(&self, acc: &mut MeterAccum) {
		for c in 0..2 {
			self.peak[c].fetch_max(acc.peak[c].to_bits(), Ordering::Relaxed);
			self.rms[c].store(acc.mean_square[c].sqrt().to_bits(), Ordering::Relaxed);
		}
		acc.peak = [0.0; 2];
	}

	fn read(&self) -> MeterReading {
		let take = |a: &AtomicU32| f32::from_bits(a.swap(0, Ordering::Relaxed));
		let load = |a: &AtomicU32| f32::from_bits(a.load(Ordering::Relaxed));
		MeterReading {
			peak: [take(&self.peak[0]), take(&self.peak[1])],
			rms: [load(&self.rms[0]), load(&self.rms[1])],
		}
	}
}

/// Shared meters of one compiled graph: one per track (in processing order) plus the
/// master. Cheap to clone through its `Arc`; read from any thread.
#[derive(Debug)]
pub struct MeterBank {
	tracks: Vec<(TrackId, ChannelMeter)>,
	master: ChannelMeter,
}

impl MeterBank {
	/// Levels of `track` (`None` if the graph has no such track). Resets its peak.
	pub fn track(&self, track: TrackId) -> Option<MeterReading> {
		self.tracks
			.iter()
			.find(|(id, _)| *id == track)
			.map(|(_, m)| m.read())
	}

	/// Levels of the master output. Resets its peak.
	pub fn master(&self) -> MeterReading {
		self.master.read()
	}

	/// Track ids in the bank (processing order).
	pub fn track_ids(&self) -> impl Iterator<Item = TrackId> + '_ {
		self.tracks.iter().map(|(id, _)| *id)
	}
}

/// Audio-thread accumulator for one strip.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct MeterAccum {
	peak: [f32; 2],
	mean_square: [f32; 2],
}

/// The graph's metering state: accumulators (index = node, last = master) and the bank.
#[derive(Debug)]
pub(crate) struct Meters {
	pub bank: std::sync::Arc<MeterBank>,
	accum: Vec<MeterAccum>,
	/// One-pole coefficient applied once per quantum.
	rms_coeff: f32,
}

impl Meters {
	pub fn new(tracks: impl IntoIterator<Item = TrackId>, sample_rate: u32) -> Self {
		let tracks: Vec<_> = tracks
			.into_iter()
			.map(|id| (id, ChannelMeter::default()))
			.collect();
		let accum = vec![MeterAccum::default(); tracks.len() + 1];
		let quanta_per_second = sample_rate.max(1) as f32 / QUANTUM as f32;
		Self {
			bank: std::sync::Arc::new(MeterBank {
				tracks,
				master: ChannelMeter::default(),
			}),
			accum,
			rms_coeff: (-1.0 / (RMS_SECONDS * quanta_per_second)).exp(),
		}
	}

	/// Accumulates one quantum of node `index` (`None` = master).
	pub fn measure(&mut self, index: Option<usize>, l: &[f32], r: &[f32]) {
		let i = index.unwrap_or(self.accum.len() - 1);
		let Some(acc) = self.accum.get_mut(i) else {
			return;
		};
		for (c, buf) in [l, r].into_iter().enumerate() {
			let mut peak = acc.peak[c];
			let mut sum = 0.0f32;
			for &x in buf {
				let a = x.abs();
				// `a > peak` is false for NaN, so a broken device cannot poison the meter.
				if a > peak {
					peak = a;
				}
				sum += x * x;
			}
			acc.peak[c] = peak;
			let ms = if sum.is_finite() {
				sum / buf.len().max(1) as f32
			} else {
				0.0
			};
			acc.mean_square[c] = ms + (acc.mean_square[c] - ms) * self.rms_coeff;
		}
	}

	/// Accumulates one silent quantum for node `index` (muted tracks).
	pub fn measure_silence(&mut self, index: usize) {
		if let Some(acc) = self.accum.get_mut(index) {
			for ms in &mut acc.mean_square {
				*ms *= self.rms_coeff;
			}
		}
	}

	/// Clears every level (transport stopped).
	pub fn silence(&mut self) {
		for acc in &mut self.accum {
			*acc = MeterAccum::default();
		}
	}

	/// Publishes the accumulated levels to the bank. Wait-free, no allocation.
	pub fn publish(&mut self) {
		let (master, nodes) = self.accum.split_last_mut().expect("master accumulator");
		for (acc, (_, meter)) in nodes.iter_mut().zip(&self.bank.tracks) {
			meter.publish(acc);
		}
		self.bank.master.publish(master);
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn peak_is_held_until_read_and_rms_is_smoothed() {
		let id = TrackId::new();
		let mut m = Meters::new([id], 48_000);
		let loud = [0.5f32; QUANTUM];
		let quiet = [0.1f32; QUANTUM];
		m.measure(Some(0), &loud, &quiet);
		m.publish();
		m.measure(Some(0), &quiet, &quiet);
		m.publish();
		let r = m.bank.track(id).unwrap();
		assert_eq!(r.peak, [0.5, 0.1], "the loudest block since the last read");
		assert!(r.rms[0] > 0.0 && r.rms[0] < 0.5, "smoothed: {}", r.rms[0]);
		assert_eq!(
			m.bank.track(id).unwrap().peak,
			[0.0, 0.0],
			"reading resets peaks"
		);

		// A steady signal converges to its RMS.
		for _ in 0..48_000 * 2 / QUANTUM {
			m.measure(Some(0), &loud, &loud);
		}
		m.publish();
		let r = m.bank.track(id).unwrap();
		assert!((r.rms[0] - 0.5).abs() < 1e-3, "{}", r.rms[0]);
	}

	#[test]
	fn master_and_unknown_tracks() {
		let mut m = Meters::new([], 48_000);
		m.measure(None, &[-0.25; 4], &[f32::NAN; 4]);
		m.publish();
		let r = m.bank.master();
		assert_eq!(r.peak, [0.25, 0.0]);
		assert_eq!(r.rms[1], 0.0, "NaN never reaches the meter");
		assert!(m.bank.track(TrackId::new()).is_none());
		m.silence();
		m.publish();
		assert_eq!(m.bank.master(), MeterReading::default());
	}
}
