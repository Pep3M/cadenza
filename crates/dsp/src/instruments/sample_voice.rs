//! Playback machinery shared by `sampler` and `drum_sampler`: per-zone frame positions
//! precomputed in `set_zones`, a playhead that reads a zone's region (forward, reverse,
//! forward/ping-pong loops with a seam crossfade) at any rate, the per-zone AD envelope,
//! click-free kill fades and voice-slot selection. Everything here is realtime safe.

use crate::sample::{LoopMode, SampleData, Zone};
use crate::util::envelope::decay_coeff;
use crate::util::interp;

/// Shortest loop (source frames) that is honored; shorter loops play through.
const MIN_LOOP: f64 = 8.0;
/// Level at which a decaying zone envelope ends (-60 dB).
const ZONE_ENV_END: f32 = 1e-3;

/// Frame positions and gains of one zone, in source frames of its sample.
#[derive(Debug, Clone, Copy, Default)]
pub struct ZoneRt {
	pub start: f64,
	pub end: f64,
	/// `Some((start, end, crossfade))` when the zone loops.
	pub looping: Option<(f64, f64, f64)>,
	pub ping_pong: bool,
	/// Frames of fade at the region's far end (0 when the sample already ends quietly).
	pub end_fade: f64,
	pub gain_l: f32,
	pub gain_r: f32,
	pub sample_rate: f32,
}

impl ZoneRt {
	/// Precomputes `zone` (non-realtime). Zones without a sample get an empty region.
	pub fn new(zone: &Zone) -> Self {
		let gain = crate::util::db_to_gain(zone.gain_db.clamp(-144.0, 48.0));
		let pan = if zone.pan.is_finite() {
			zone.pan.clamp(-1.0, 1.0)
		} else {
			0.0
		};
		let angle = (pan + 1.0) * std::f32::consts::FRAC_PI_4;
		// Equal-power, normalized so the center is unity.
		let (gain_l, gain_r) = (
			gain * angle.cos() * std::f32::consts::SQRT_2,
			gain * angle.sin() * std::f32::consts::SQRT_2,
		);
		let Some(sample) = zone.sample.as_deref() else {
			return Self {
				gain_l,
				gain_r,
				sample_rate: 48_000.0,
				..Self::default()
			};
		};
		let sr = f64::from(sample.sample_rate().max(1.0));
		let frames = sample.frames() as f64;
		let secs = |s: f64| {
			if s.is_finite() {
				(s * sr).max(0.0)
			} else {
				0.0
			}
		};
		let start = secs(zone.start).min(frames);
		let end = zone.end.map_or(frames, secs).clamp(start, frames);
		let looping = (!zone.reverse && zone.loop_mode != LoopMode::Off)
			.then(|| {
				let ls = secs(zone.loop_start).clamp(start, end);
				let le = zone.loop_end.map_or(end, secs).clamp(ls, end);
				(le - ls >= MIN_LOOP).then(|| {
					let xf = if zone.loop_mode == LoopMode::Forward {
						secs(zone.loop_crossfade).min(ls).min(le - ls)
					} else {
						0.0
					};
					(ls, le, xf)
				})
			})
			.flatten();
		let edge = if zone.reverse { start } else { end - 1.0 };
		let loud_edge = edge >= 0.0
			&& edge < frames
			&& (0..sample.num_channels().min(2))
				.any(|c| sample.channel(c)[edge as usize].abs() > 1e-3);
		let end_fade = if loud_edge {
			(sr * 0.0015).min((end - start) * 0.5)
		} else {
			0.0
		};
		Self {
			start,
			end,
			looping,
			ping_pong: zone.loop_mode == LoopMode::PingPong,
			end_fade,
			gain_l,
			gain_r,
			sample_rate: sample.sample_rate().max(1.0),
		}
	}

	/// Length of the region in seconds at the sample's own rate.
	pub fn region_seconds(&self) -> f64 {
		(self.end - self.start) / f64::from(self.sample_rate)
	}
}

/// Reads one stereo frame of `sample` (mono doubles, >2 channels use the first two).
#[inline]
fn frame(sample: &dyn SampleData, pos: f64, ratio: f64) -> (f32, f32) {
	let l = interp::read(sample.channel(0), pos, ratio);
	let r = if sample.num_channels() > 1 {
		interp::read(sample.channel(1), pos, ratio)
	} else {
		l
	};
	(l, r)
}

/// Position of a voice inside its zone's region.
#[derive(Debug, Clone, Copy, Default)]
pub struct Playhead {
	pos: f64,
	/// +1 forward, -1 backward (reverse zones, ping-pong return).
	dir: f64,
	reverse: bool,
}

impl Playhead {
	pub fn start(rt: &ZoneRt, reverse: bool) -> Self {
		Self {
			pos: if reverse { rt.end - 1.0 } else { rt.start },
			dir: if reverse { -1.0 } else { 1.0 },
			reverse,
		}
	}

	/// The next stereo frame at playback `ratio` (source frames per output sample), or
	/// `None` once the region is over.
	#[inline]
	pub fn next(&mut self, sample: &dyn SampleData, rt: &ZoneRt, ratio: f64) -> Option<(f32, f32)> {
		let ratio = ratio.max(1e-6);
		let (mut l, mut r) = if self.reverse {
			if self.pos < rt.start {
				return None;
			}
			frame(sample, self.pos, ratio)
		} else if let Some((ls, le, xf)) = rt.looping {
			if rt.ping_pong {
				frame(sample, self.pos, ratio)
			} else {
				let (a, b) = frame(sample, self.pos, ratio);
				let seam = le - xf;
				if xf > 0.0 && self.pos >= seam {
					// Fade the loop end out while the material before the loop start
					// fades in; at the wrap the faded-in material simply continues.
					let t = ((self.pos - seam) / xf) as f32;
					let (c, d) = frame(sample, self.pos - (le - ls), ratio);
					(a + (c - a) * t, b + (d - b) * t)
				} else {
					(a, b)
				}
			}
		} else {
			if self.pos >= rt.end {
				return None;
			}
			frame(sample, self.pos, ratio)
		};
		if rt.looping.is_none() && rt.end_fade > 0.0 {
			let left = if self.reverse {
				self.pos - rt.start
			} else {
				rt.end - self.pos
			};
			if left < rt.end_fade {
				let g = (left / rt.end_fade).clamp(0.0, 1.0) as f32;
				l *= g;
				r *= g;
			}
		}
		self.advance(rt, ratio);
		Some((l, r))
	}

	#[inline]
	fn advance(&mut self, rt: &ZoneRt, ratio: f64) {
		self.pos += self.dir * ratio;
		let Some((ls, le, _)) = rt.looping else {
			return;
		};
		let len = le - ls;
		if rt.ping_pong {
			// Reflect at both ends (a huge step may need several reflections).
			for _ in 0..8 {
				if self.dir > 0.0 && self.pos >= le {
					self.pos = 2.0 * le - self.pos;
					self.dir = -1.0;
				} else if self.dir < 0.0 && self.pos < ls {
					self.pos = 2.0 * ls - self.pos;
					self.dir = 1.0;
				} else {
					break;
				}
			}
			// Before reaching the loop the playhead runs freely from the region start.
			if self.pos >= ls {
				self.pos = self.pos.min(le);
			}
		} else if self.pos >= le {
			self.pos = ls + (self.pos - le) % len;
		}
	}
}

/// The per-zone envelope: linear attack, then an optional exponential decay to -60 dB.
#[derive(Debug, Clone, Copy, Default)]
pub struct ZoneEnv {
	level: f32,
	attack_step: f32,
	decay: Option<f32>,
	attacking: bool,
}

impl ZoneEnv {
	pub fn trigger(zone: &Zone, sample_rate: f32) -> Self {
		let attack = zone.attack_ms.max(0.0) * 0.001 * sample_rate;
		Self {
			level: if attack < 1.0 { 1.0 } else { 0.0 },
			attack_step: 1.0 / attack.max(1.0),
			decay: zone
				.decay_ms
				.filter(|d| d.is_finite())
				.map(|d| decay_coeff(d.max(1.0), sample_rate)),
			attacking: attack >= 1.0,
		}
	}

	/// Next gain; `None` once a decay has run out.
	#[inline]
	pub fn tick(&mut self) -> Option<f32> {
		if self.attacking {
			self.level += self.attack_step;
			if self.level >= 1.0 {
				self.level = 1.0;
				self.attacking = false;
			}
			return Some(self.level);
		}
		if let Some(c) = self.decay {
			self.level *= c;
			if self.level < ZONE_ENV_END {
				return None;
			}
		}
		Some(self.level)
	}
}

/// A linear fade to silence (voice steals, chokes, all-notes-off).
#[derive(Debug, Clone, Copy)]
pub struct KillFade {
	gain: f32,
	step: f32,
}

impl Default for KillFade {
	fn default() -> Self {
		Self {
			gain: 1.0,
			step: 0.0,
		}
	}
}

impl KillFade {
	/// Starts fading over `ms` (keeps the faster of two overlapping fades).
	pub fn start(&mut self, ms: f32, sample_rate: f32) {
		let step = 1.0 / (ms * 0.001 * sample_rate).max(1.0);
		self.step = self.step.max(step);
	}

	pub fn is_fading(&self) -> bool {
		self.step > 0.0
	}

	/// Next gain; `None` when silent.
	#[inline]
	pub fn tick(&mut self) -> Option<f32> {
		if self.step == 0.0 {
			return Some(1.0);
		}
		self.gain -= self.step;
		(self.gain > 0.0).then_some(self.gain)
	}
}

/// Velocity -> gain: `sensitivity` 0..1, `curve` -1..1 (negative = soft notes louder).
#[inline]
pub fn velocity_gain(velocity: f32, sensitivity: f32, curve: f32) -> f32 {
	let v = velocity.clamp(0.0, 1.0);
	let shaped = v.powf(2.0_f32.powf(curve.clamp(-1.0, 1.0) * 2.0));
	1.0 - sensitivity.clamp(0.0, 1.0) * (1.0 - shaped)
}

/// What [`pick_slot`] needs to know about a voice slot.
#[derive(Debug, Clone, Copy, Default)]
pub struct SlotInfo {
	pub active: bool,
	pub fading: bool,
	/// Key released (in its release tail).
	pub released: bool,
	pub started: u64,
}

/// Chooses voice slots for a new voice. Returns `(slot, steal)`: `steal` is a sounding
/// voice the caller must fade out because the polyphony limit is reached. `slot` is idle
/// when possible; with every slot busy it is the oldest fading (or oldest) voice, which
/// the caller restarts hard.
pub fn pick_slot(slots: &[SlotInfo], polyphony: usize) -> (usize, Option<usize>) {
	let sounding = slots.iter().filter(|s| s.active && !s.fading).count();
	let steal = (sounding >= polyphony.max(1))
		.then(|| {
			slots
				.iter()
				.enumerate()
				.filter(|(_, s)| s.active && !s.fading)
				.min_by_key(|(_, s)| (!s.released, s.started))
				.map(|(i, _)| i)
		})
		.flatten();
	let idle = slots.iter().position(|s| !s.active);
	let slot = idle.unwrap_or_else(|| {
		slots
			.iter()
			.enumerate()
			.filter(|(i, _)| Some(*i) != steal)
			.min_by_key(|(_, s)| (!s.fading, s.started))
			.map_or(0, |(i, _)| i)
	});
	(slot, steal)
}

/// Picks which zones a note-on starts: every matching zone with `round_robin == 0`, and
/// one matching zone per round-robin group, in turn (`counters` per group 1..=16).
/// Calls `start` with each chosen zone index. Realtime safe.
pub fn select_zones(
	zones: &[Zone],
	pitch: u8,
	velocity: f32,
	counters: &mut [u32; 17],
	mut start: impl FnMut(usize),
) {
	let mut seen = [false; 17];
	for (i, z) in zones.iter().enumerate() {
		if !z.matches(pitch, velocity) {
			continue;
		}
		let g = usize::from(z.round_robin.min(16));
		if g == 0 {
			start(i);
			continue;
		}
		if seen[g] {
			continue;
		}
		seen[g] = true;
		let members = zones
			.iter()
			.filter(|o| o.round_robin.min(16) as usize == g && o.matches(pitch, velocity))
			.count() as u32;
		let pick = counters[g] % members;
		counters[g] = counters[g].wrapping_add(1);
		if let Some((j, _)) = zones
			.iter()
			.enumerate()
			.filter(|(_, o)| o.round_robin.min(16) as usize == g && o.matches(pitch, velocity))
			.nth(pick as usize)
		{
			start(j);
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::sample::SampleBuffer;
	use std::sync::Arc;

	fn ramp_zone(frames: usize) -> Zone {
		let data: Vec<f32> = (0..frames).map(|i| i as f32).collect();
		Zone {
			sample: Some(Arc::new(SampleBuffer::mono(1_000.0, data))),
			..Zone::default()
		}
	}

	fn collect(zone: &Zone, ratio: f64, max: usize) -> Vec<f32> {
		let rt = ZoneRt::new(zone);
		let mut ph = Playhead::start(&rt, zone.reverse);
		let s = zone.sample.as_deref().unwrap();
		let mut out = Vec::new();
		while out.len() < max {
			match ph.next(s, &rt, ratio) {
				Some((l, _)) => out.push(l),
				None => break,
			}
		}
		out
	}

	#[test]
	fn region_and_reverse() {
		let mut z = ramp_zone(100);
		z.start = 0.010;
		z.end = Some(0.020);
		let fwd = collect(&z, 1.0, 1000);
		assert_eq!(fwd.len(), 10);
		assert!((fwd[0] - 10.0).abs() < 1e-4);
		z.reverse = true;
		let rev = collect(&z, 1.0, 1000);
		assert_eq!(rev.len(), 10);
		assert!(rev[0] > rev[9]);
	}

	#[test]
	fn forward_loop_wraps_and_ping_pong_reflects() {
		let mut z = ramp_zone(100);
		z.loop_mode = LoopMode::Forward;
		z.loop_start = 0.040;
		z.loop_end = Some(0.060);
		let out = collect(&z, 1.0, 200);
		assert_eq!(out.len(), 200);
		assert!(out[100..].iter().all(|&v| (40.0..60.0).contains(&v)));
		z.loop_mode = LoopMode::PingPong;
		let out = collect(&z, 1.0, 200);
		let jumps = out
			.windows(2)
			.map(|w| (w[1] - w[0]).abs())
			.fold(0.0, f32::max);
		assert!(jumps <= 1.01, "ping-pong jumped {jumps}");
	}

	#[test]
	fn round_robin_takes_turns_and_layers_all_play() {
		let mut zones = vec![Zone::default(); 3];
		zones[0].round_robin = 1;
		zones[1].round_robin = 1;
		let mut counters = [0; 17];
		let mut picks = Vec::new();
		for _ in 0..4 {
			let mut now = Vec::new();
			select_zones(&zones, 60, 0.5, &mut counters, |i| now.push(i));
			picks.push(now);
		}
		assert_eq!(picks, vec![vec![0, 2], vec![1, 2], vec![0, 2], vec![1, 2]]);
	}

	#[test]
	fn slots_steal_oldest_released_first() {
		let s = |active, released, started| SlotInfo {
			active,
			fading: false,
			released,
			started,
		};
		let slots = [s(true, false, 1), s(true, true, 5), s(false, false, 0)];
		assert_eq!(pick_slot(&slots, 2), (2, Some(1)));
		assert_eq!(pick_slot(&slots, 8), (2, None));
	}

	#[test]
	fn velocity_curve_shapes() {
		assert_eq!(velocity_gain(0.5, 0.0, 0.0), 1.0);
		assert!((velocity_gain(1.0, 1.0, 0.7) - 1.0).abs() < 1e-6);
		assert!(velocity_gain(0.5, 1.0, -1.0) > velocity_gain(0.5, 1.0, 0.0));
		assert!(velocity_gain(0.5, 1.0, 1.0) < velocity_gain(0.5, 1.0, 0.0));
	}
}
