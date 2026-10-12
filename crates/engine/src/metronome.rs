//! The built-in metronome: a synthesized click on every beat (accented on the bar),
//! sample-accurate on the song timeline, and the count-in before a recording.
//!
//! Clicks are scheduled from the same tempo segments the graph renders with, for the
//! timeline position each quantum renders, then delayed by the graph's latency so they
//! line up with the (latency-compensated) output. During a count-in the transport is
//! frozen and the clicks follow the count-in counter instead.

use serde::{Deserialize, Serialize};

use crate::QUANTUM;

/// Click length.
const CLICK_SECONDS: f32 = 0.035;

/// When and how loud the metronome plays (kept by the player across loads).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MetronomeSettings {
	/// Click while playing (not recording).
	pub on_play: bool,
	/// Click while recording (and during the count-in).
	pub on_record: bool,
	/// Click level, dB (0 = full scale peak).
	pub level_db: f32,
}

impl Default for MetronomeSettings {
	fn default() -> Self {
		Self {
			on_play: false,
			on_record: true,
			level_db: -6.0,
		}
	}
}

/// Click voices, rendered into the master output (audio thread; allocation free after
/// [`Metronome::new`]).
pub(crate) struct Metronome {
	accent: Vec<f32>,
	normal: Vec<f32>,
	/// Playing click: (accented, position in its sample).
	voice: Option<(bool, usize)>,
	/// Latency compensation of the clicks (one sample per graph latency sample).
	delay: Vec<f32>,
	delay_pos: usize,
	/// Samples until the last click has left the delay line (0 = silent).
	tail: usize,
	buf: [f32; QUANTUM],
	pub settings: MetronomeSettings,
}

/// A click starting `offset` samples into a quantum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Click {
	pub offset: usize,
	pub accent: bool,
}

/// Up to this many clicks start in one quantum (tempo at most ~1000 bpm in 1/32 units).
pub(crate) const MAX_CLICKS: usize = 4;

fn click_sample(sample_rate: f32, freq: f32, gain: f32) -> Vec<f32> {
	let len = (CLICK_SECONDS * sample_rate) as usize;
	(0..len)
		.map(|i| {
			let t = i as f32 / sample_rate;
			// Fast attack, exponential decay: a woodblock-like tick.
			let env = (1.0 - (-t * 4000.0).exp()) * (-t * 90.0).exp();
			gain * env * (std::f32::consts::TAU * freq * t).sin()
		})
		.collect()
}

impl Metronome {
	pub fn new(sample_rate: u32, latency: u32) -> Self {
		let sr = sample_rate as f32;
		Self {
			accent: click_sample(sr, 1760.0, 1.0),
			normal: click_sample(sr, 1320.0, 0.7),
			voice: None,
			delay: vec![0.0; latency as usize],
			delay_pos: 0,
			tail: 0,
			buf: [0.0; QUANTUM],
			settings: MetronomeSettings::default(),
		}
	}

	/// Whether clicks play now.
	pub fn enabled(&self, recording: bool) -> bool {
		if recording {
			self.settings.on_record
		} else {
			self.settings.on_play
		}
	}

	pub fn reset(&mut self) {
		self.voice = None;
		if self.tail > 0 {
			self.delay.fill(0.0);
		}
		self.delay_pos = 0;
		self.tail = 0;
	}

	/// Renders one quantum of clicks starting at `clicks` into the scratch buffer.
	fn render(&mut self, clicks: &[Click]) {
		self.buf.fill(0.0);
		let gain = cadenza_core::db_to_gain(self.settings.level_db);
		let mut next = 0;
		if !clicks.is_empty() || self.voice.is_some() {
			self.tail = self.delay.len() + QUANTUM;
		}
		for i in 0..QUANTUM {
			if next < clicks.len() && clicks[next].offset == i {
				self.voice = Some((clicks[next].accent, 0));
				next += 1;
			}
			if let Some((accent, pos)) = &mut self.voice {
				let sample = if *accent { &self.accent } else { &self.normal };
				match sample.get(*pos) {
					Some(v) => {
						self.buf[i] = v * gain;
						*pos += 1;
					}
					None => self.voice = None,
				}
			}
		}
	}

	/// Adds a quantum of clicks (on the timeline the graph rendered) to `l`/`r`, delayed
	/// by the graph latency like the rest of the output. Untouched while disabled and
	/// silent, so renders without clicks stay bit-identical.
	pub fn process(&mut self, enabled: bool, clicks: &[Click], l: &mut [f32], r: &mut [f32]) {
		if !enabled && self.tail == 0 {
			return;
		}
		self.mix_delayed(if enabled { clicks } else { &[] }, l, r);
		self.tail = self.tail.saturating_sub(QUANTUM);
	}

	fn mix_delayed(&mut self, clicks: &[Click], l: &mut [f32], r: &mut [f32]) {
		self.render(clicks);
		let len = self.delay.len();
		for i in 0..QUANTUM {
			let mut v = self.buf[i];
			if len > 0 {
				std::mem::swap(&mut v, &mut self.delay[self.delay_pos]);
				self.delay_pos = (self.delay_pos + 1) % len;
			}
			l[i] += v;
			r[i] += v;
		}
	}

	/// Adds a quantum of clicks without delay (count-in: nothing else plays).
	pub fn mix_direct(&mut self, clicks: &[Click], l: &mut [f32], r: &mut [f32]) {
		self.render(clicks);
		for i in 0..QUANTUM {
			l[i] += self.buf[i];
			r[i] += self.buf[i];
		}
	}
}

/// Clicks of beats (multiples of `unit` quarter notes, accented every `per_bar`) whose
/// position falls in `[b0, b1)` quarter notes, where `sample_of(beat)` maps a beat to
/// its offset from the quantum start. Returns how many were written to `out`.
pub(crate) fn clicks_between(
	b0: f64,
	b1: f64,
	unit: f64,
	per_bar: u32,
	sample_of: impl Fn(f64) -> i64,
	out: &mut [Click; MAX_CLICKS],
	count: &mut usize,
) {
	if unit.is_nan() || unit <= 0.0 || b1 <= b0 {
		return;
	}
	let mut k = (b0 / unit - 1e-9).ceil();
	while k * unit < b1 && *count < MAX_CLICKS {
		let offset = sample_of(k * unit);
		if (0..QUANTUM as i64).contains(&offset) {
			let index = k as i64;
			out[*count] = Click {
				offset: offset as usize,
				accent: index.rem_euclid(i64::from(per_bar.max(1))) == 0,
			};
			*count += 1;
		}
		k += 1.0;
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn clicks_land_on_beats_and_accent_bars() {
		// 120 bpm at 48 kHz: one beat = 24_000 samples; a quantum from sample 23_990.
		let start = 23_990i64;
		let b0 = start as f64 / 24_000.0;
		let b1 = (start + QUANTUM as i64) as f64 / 24_000.0;
		let mut out = [Click {
			offset: 0,
			accent: false,
		}; MAX_CLICKS];
		let mut n = 0;
		clicks_between(
			b0,
			b1,
			1.0,
			4,
			|b| (b * 24_000.0).round() as i64 - start,
			&mut out,
			&mut n,
		);
		assert_eq!(n, 1);
		assert_eq!(out[0].offset, 10);
		assert!(!out[0].accent, "beat 2 of the bar");
		n = 0;
		clicks_between(0.0, 0.001, 1.0, 4, |b| b as i64, &mut out, &mut n);
		assert_eq!((n, out[0].accent), (1, true), "the downbeat is accented");
	}

	#[test]
	fn the_click_sounds_after_the_delay() {
		let mut m = Metronome::new(48_000, 40);
		m.settings.level_db = 0.0;
		let (mut l, mut r) = ([0.0f32; QUANTUM], [0.0f32; QUANTUM]);
		let click = [Click {
			offset: 0,
			accent: true,
		}];
		m.mix_delayed(&click, &mut l, &mut r);
		assert!(l.iter().all(|v| *v == 0.0), "still in the delay line");
		let (mut l2, mut r2) = ([0.0f32; QUANTUM], [0.0f32; QUANTUM]);
		m.mix_delayed(&[], &mut l2, &mut r2);
		assert!(l2[8..].iter().any(|v| *v != 0.0));
		assert_eq!(l2, r2);
	}
}
