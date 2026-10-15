//! Shared DSP building blocks. Everything here is allocation-free after
//! construction/`prepare` and safe to call from the audio thread.

pub mod biquad;
pub mod delay_line;
pub mod envelope;
pub mod expression;
pub mod interp;
pub mod oscillator;
pub mod pitch;
pub mod smoother;
pub mod svf;

use crate::processor::{DeviceInfo, MidiEvent};

pub const TAU: f32 = std::f32::consts::TAU;
pub const PI: f32 = std::f32::consts::PI;

#[inline]
pub fn db_to_gain(db: f32) -> f32 {
	10.0_f32.powf(db * 0.05)
}

#[inline]
pub fn gain_to_db(gain: f32) -> f32 {
	20.0 * gain.max(1e-12).log10()
}

#[inline]
pub fn midi_to_hz(note: f32) -> f32 {
	440.0 * 2.0_f32.powf((note - 69.0) / 12.0)
}

/// Replaces values too small to matter with exact zero so recursive filters
/// never decay into denormal (subnormal) territory, which is very slow on
/// many CPUs.
#[inline(always)]
pub fn flush(x: f32) -> f32 {
	if x.abs() < 1e-18 { 0.0 } else { x }
}

/// Coefficient for a one-pole smoother/follower with time constant `ms`.
#[inline]
pub fn one_pole_coeff(ms: f32, sample_rate: f32) -> f32 {
	let samples = (ms * 0.001 * sample_rate).max(1e-3);
	1.0 - (-1.0 / samples).exp()
}

/// Small, fast, deterministic xorshift PRNG for noise sources.
#[derive(Debug, Clone, Copy)]
pub struct Rng(u32);

impl Rng {
	pub const fn new(seed: u32) -> Self {
		Self(if seed == 0 { 0x9E37_79B9 } else { seed })
	}

	#[inline]
	pub fn next_u32(&mut self) -> u32 {
		let mut x = self.0;
		x ^= x << 13;
		x ^= x >> 17;
		x ^= x << 5;
		self.0 = x;
		x
	}

	/// Uniform in `-1.0..1.0`.
	#[inline]
	pub fn bipolar(&mut self) -> f32 {
		(self.next_u32() >> 8) as f32 * (2.0 / 16_777_216.0) - 1.0
	}

	/// Uniform in `0.0..1.0`.
	#[inline]
	pub fn unipolar(&mut self) -> f32 {
		(self.next_u32() >> 8) as f32 * (1.0 / 16_777_216.0)
	}
}

/// Plain parameter values for a device, initialised from its defaults.
#[derive(Debug, Clone)]
pub struct ParamValues<const N: usize> {
	values: [f32; N],
}

impl<const N: usize> ParamValues<N> {
	pub fn new(info: &DeviceInfo) -> Self {
		debug_assert_eq!(info.params.len(), N);
		let mut values = [0.0; N];
		for (v, p) in values.iter_mut().zip(info.params) {
			*v = p.default;
		}
		Self { values }
	}

	/// Stores a clamped value; returns the stored value if `index` is valid
	/// and `value` is not NaN.
	pub fn set(&mut self, info: &DeviceInfo, index: usize, value: f32) -> Option<f32> {
		let v = info.params.get(index)?.clamp(value)?;
		let slot = self.values.get_mut(index)?;
		*slot = v;
		Some(v)
	}

	#[inline]
	pub fn get(&self, index: usize) -> f32 {
		self.values[index]
	}

	/// Integer-valued (switch/choice) parameter.
	#[inline]
	pub fn choice(&self, index: usize) -> usize {
		self.values[index].round().max(0.0) as usize
	}

	#[inline]
	pub fn flag(&self, index: usize) -> bool {
		self.values[index] >= 0.5
	}
}

/// Splits a block at event offsets so instruments can render sample-accurately.
///
/// Yields `(start, end, event)`: render `start..end`, then apply `event` (if
/// any). Offsets beyond the block are clamped to its end; the last item has no
/// event and renders the remainder.
pub fn event_spans(len: usize, events: &[MidiEvent]) -> EventSpans<'_> {
	EventSpans {
		events,
		len,
		pos: 0,
		idx: 0,
		done: false,
	}
}

pub struct EventSpans<'a> {
	events: &'a [MidiEvent],
	len: usize,
	pos: usize,
	idx: usize,
	done: bool,
}

impl<'a> Iterator for EventSpans<'a> {
	type Item = (usize, usize, Option<&'a MidiEvent>);

	fn next(&mut self) -> Option<Self::Item> {
		if let Some(ev) = self.events.get(self.idx) {
			self.idx += 1;
			let at = (ev.offset() as usize).min(self.len).max(self.pos);
			let start = self.pos;
			self.pos = at;
			Some((start, at, Some(ev)))
		} else if !self.done {
			self.done = true;
			Some((self.pos, self.len, None))
		} else {
			None
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn db_roundtrip() {
		for db in [-60.0, -6.0, 0.0, 6.0, 24.0] {
			assert!((gain_to_db(db_to_gain(db)) - db).abs() < 1e-3);
		}
	}

	#[test]
	fn rng_ranges() {
		let mut r = Rng::new(1);
		for _ in 0..10_000 {
			let b = r.bipolar();
			let u = r.unipolar();
			assert!((-1.0..1.0).contains(&b));
			assert!((0.0..1.0).contains(&u));
		}
	}

	#[test]
	fn spans_cover_block() {
		let events = [
			MidiEvent::NoteOn {
				offset: 3,
				pitch: 60,
				velocity: 1.0,
			},
			MidiEvent::NoteOff {
				offset: 3,
				pitch: 60,
			},
			MidiEvent::AllNotesOff { offset: 99 },
		];
		let spans: Vec<_> = event_spans(10, &events)
			.map(|(a, b, e)| (a, b, e.is_some()))
			.collect();
		assert_eq!(
			spans,
			vec![(0, 3, true), (3, 3, true), (3, 10, true), (10, 10, false)]
		);
	}
}
