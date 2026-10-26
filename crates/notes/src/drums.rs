//! Percussive detection (NE-8): the hits of a drum recording, each with a General MIDI drum
//! note and a velocity, so a loop becomes a drum clip `drum_machine` plays. Onsets are
//! found per band (lows: kick, mids: snare and toms, highs: hats and cymbals) as sudden
//! rises of the band's level, grouped into events, then labelled by which bands rose and
//! how they relate: a kick owns the lows, a snare's noisy mids are as loud as its lows, a
//! hat is a rise of the highs alone; how long the highs ring tells closed, open and crash
//! apart, the spectral peak of the lows places a tom.

use realfft::RealFftPlanner;
use realfft::num_complex::Complex;

/// What was hit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DrumKind {
	Kick,
	Snare,
	ClosedHat,
	OpenHat,
	Crash,
	LowTom,
	MidTom,
	HighTom,
}

impl DrumKind {
	/// The General MIDI drum note.
	pub fn note(self) -> u8 {
		match self {
			Self::Kick => 36,
			Self::Snare => 38,
			Self::ClosedHat => 42,
			Self::OpenHat => 46,
			Self::Crash => 49,
			Self::LowTom => 45,
			Self::MidTom => 47,
			Self::HighTom => 50,
		}
	}

	pub fn name(self) -> &'static str {
		match self {
			Self::Kick => "kick",
			Self::Snare => "snare",
			Self::ClosedHat => "closed hat",
			Self::OpenHat => "open hat",
			Self::Crash => "crash",
			Self::LowTom => "low tom",
			Self::MidTom => "mid tom",
			Self::HighTom => "high tom",
		}
	}
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DrumHit {
	/// Seconds.
	pub time: f64,
	pub kind: DrumKind,
	/// 0..1, from the hit's level relative to the loudest of its kind.
	pub velocity: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DrumOptions {
	/// A band must rise at least this much (dB over 10 ms) to start a hit.
	pub rise_db: f64,
	/// Rises closer than this (seconds) are one hit.
	pub merge: f64,
	/// Bands quieter than this below the recording's loudest frame of that band are ignored
	/// (bleed, room).
	pub floor_db: f64,
}

impl Default for DrumOptions {
	fn default() -> Self {
		Self {
			rise_db: 9.0,
			merge: 0.03,
			floor_db: 35.0,
		}
	}
}

const BANDS: [(f64, f64); 3] = [(30.0, 150.0), (150.0, 4_000.0), (6_000.0, 16_000.0)];
const LOW: usize = 0;
const MID: usize = 1;
const HIGH: usize = 2;

/// Inside the mids: a tom's body vs a snare's crack.
const BODY: (f64, f64) = (150.0, 1_000.0);
const CRACK: (f64, f64) = (1_000.0, 4_000.0);

/// Band levels (dB) per 5 ms frame and the low-band spectral peak (Hz) per frame.
struct Bands {
	hop: f64,
	level: Vec<[f64; 3]>,
	/// (body, crack) levels, dB.
	mids: Vec<(f64, f64)>,
	low_peak: Vec<f64>,
}

fn bands(samples: &[f32], rate: u32) -> Bands {
	let sr = f64::from(rate.max(1));
	let size = ((sr * 0.023) as usize).next_power_of_two().max(256);
	let hop = (sr * 0.005).round().max(1.0) as usize;
	let fft = RealFftPlanner::<f64>::new().plan_fft_forward(size);
	let hann: Vec<f64> = (0..size)
		.map(|i| 0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / size as f64).cos())
		.collect();
	let bin = |hz: f64| ((hz / sr * size as f64).round() as usize).min(size / 2);
	let ranges: Vec<(usize, usize)> = BANDS
		.iter()
		.map(|&(a, b)| (bin(a).max(1), bin(b).max(bin(a) + 1)))
		.collect();
	let count = samples.len() / hop + 1;
	let mut input = vec![0.0f64; size];
	let mut spectrum = vec![Complex::new(0.0, 0.0); size / 2 + 1];
	let mut out = Bands {
		hop: hop as f64 / sr,
		level: Vec::with_capacity(count),
		mids: Vec::with_capacity(count),
		low_peak: Vec::with_capacity(count),
	};
	let db_of = |power: &[f64], (a, b): (f64, f64)| {
		let (lo, hi) = (bin(a).max(1), bin(b).max(bin(a) + 1).min(power.len()));
		10.0 * (power[lo..hi].iter().sum::<f64>() + 1e-12).log10()
	};
	for i in 0..count {
		let center = i * hop;
		for (j, slot) in input.iter_mut().enumerate() {
			let x = (center + j)
				.checked_sub(size / 2)
				.and_then(|k| samples.get(k))
				.map_or(0.0, |&v| f64::from(v));
			*slot = x * hann[j];
		}
		fft.process(&mut input, &mut spectrum)
			.expect("fft sizes match the plan");
		let power: Vec<f64> = spectrum.iter().map(|c| c.norm_sqr()).collect();
		let mut level = [0.0; 3];
		for (b, &(lo, hi)) in ranges.iter().enumerate() {
			let e: f64 = power[lo..hi.min(power.len())].iter().sum();
			level[b] = 10.0 * (e + 1e-12).log10();
		}
		let (lo, hi) = ranges[LOW];
		let peak = (lo..hi.min(power.len()))
			.max_by(|a, b| power[*a].total_cmp(&power[*b]))
			.unwrap_or(lo);
		out.level.push(level);
		out.mids.push((db_of(&power, BODY), db_of(&power, CRACK)));
		out.low_peak.push(peak as f64 * sr / size as f64);
	}
	out
}

/// The hits of a (mono) drum recording at `rate`, in time order.
pub fn detect(samples: &[f32], rate: u32, o: &DrumOptions) -> Vec<DrumHit> {
	let b = bands(samples, rate);
	let n = b.level.len();
	if n < 4 {
		return Vec::new();
	}
	let frames = |s: f64| (s / b.hop).round() as usize;
	let loudest: [f64; 3] = std::array::from_fn(|k| {
		b.level
			.iter()
			.map(|l| l[k])
			.fold(f64::NEG_INFINITY, f64::max)
	});
	// Rise of each band over 10 ms; peaks of it start a hit.
	// Before the recording is silence.
	let level = |t: isize, k: usize| {
		if t < 0 {
			-240.0
		} else {
			b.level[t as usize][k]
		}
	};
	let rise = |t: usize, k: usize| {
		let t = t as isize;
		level(t, k) - level(t - 1, k).max(level(t - 2, k))
	};
	let gap = frames(o.merge).max(1);
	let mut onsets: Vec<(usize, usize)> = Vec::new();
	for (k, &loud) in loudest.iter().enumerate() {
		let mut last: Option<usize> = None;
		for t in 0..n - 1 {
			let r = rise(t, k);
			// Where the band gets loud: the level a few frames on, not the first frame.
			let top = (t..(t + 4).min(n))
				.map(|u| b.level[u][k])
				.fold(f64::MIN, f64::max);
			if r >= o.rise_db
				&& (t == 0 || r >= rise(t - 1, k))
				&& r > rise(t + 1, k)
				&& top >= loud - o.floor_db
				&& last.is_none_or(|l| t - l > gap)
			{
				onsets.push((t, k));
				last = Some(t);
			}
		}
	}
	onsets.sort_unstable();
	// Group the band onsets into events.
	let mut events: Vec<(usize, [bool; 3])> = Vec::new();
	for (t, k) in onsets {
		match events.last_mut() {
			Some((start, rose)) if t - *start <= gap => rose[k] = true,
			_ => {
				let mut rose = [false; 3];
				rose[k] = true;
				events.push((t, rose));
			}
		}
	}
	let window = frames(0.06).max(1);
	let mut hits: Vec<(f64, DrumKind, f64)> = Vec::new();
	for (start, rose) in events {
		let end = (start + window).min(n);
		let peak: [f64; 3] = std::array::from_fn(|k| {
			b.level[start..end]
				.iter()
				.map(|l| l[k])
				.fold(f64::NEG_INFINITY, f64::max)
		});
		let time = start as f64 * b.hop;
		let (body, crack) = b.mids[start..end]
			.iter()
			.fold((f64::MIN, f64::MIN), |m, v| (m.0.max(v.0), m.1.max(v.1)));
		// Absolute levels decide who owns the hit: a kick's lows, a snare's or tom's mids
		// (louder than its highs), a hat's highs.
		let kick = rose[LOW] && peak[LOW] >= peak[MID] - 6.0;
		let drum = rose[MID] && peak[MID] >= peak[LOW] - 6.0 && peak[MID] >= peak[HIGH] - 3.0;
		if kick {
			hits.push((time, DrumKind::Kick, peak[LOW]));
		}
		if drum {
			let kind = if crack >= body - 18.0 {
				DrumKind::Snare
			} else {
				let hz = b.low_peak[start..end].iter().sum::<f64>() / (end - start) as f64;
				match hz {
					..100.0 => DrumKind::LowTom,
					..160.0 => DrumKind::MidTom,
					_ => DrumKind::HighTom,
				}
			};
			// A kick's swept attack passes through the mids: not a tom too.
			if !(kick && kind != DrumKind::Snare) {
				hits.push((time, kind, peak[MID]));
			}
		} else if rose[HIGH] && peak[HIGH] >= peak[MID] - 3.0 {
			// How long the highs ring: 15 dB down from the hit's peak.
			let top = peak[HIGH];
			let ring = (start..n)
				.skip(1)
				.find(|&u| b.level[u][HIGH] < top - 15.0)
				.map_or(n - start, |u| u - start) as f64
				* b.hop;
			let kind = match ring {
				..0.15 => DrumKind::ClosedHat,
				..0.6 => DrumKind::OpenHat,
				_ => DrumKind::Crash,
			};
			hits.push((time, kind, peak[HIGH]));
		}
	}
	// Velocity: level against the loudest hit of the same kind (30 dB range).
	let mut out: Vec<DrumHit> = hits
		.iter()
		.map(|&(time, kind, level)| {
			let top = hits
				.iter()
				.filter(|h| h.1 == kind)
				.map(|h| h.2)
				.fold(f64::NEG_INFINITY, f64::max);
			DrumHit {
				time,
				kind,
				velocity: (1.0 + (level - top) / 30.0).clamp(0.2, 1.0) as f32,
			}
		})
		.collect();
	out.sort_by(|a, b| a.time.total_cmp(&b.time));
	out
}

#[cfg(test)]
mod tests {
	use super::*;

	fn noise(seed: &mut u64) -> f64 {
		*seed ^= *seed << 13;
		*seed ^= *seed >> 7;
		*seed ^= *seed << 17;
		(*seed >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0
	}

	/// Synthetic hits: a kick (pitch-swept sine), a snare (200 Hz tone + band noise), a
	/// closed hat (high noise, 50 ms).
	fn kit(hits: &[(f64, DrumKind)], seconds: f64, rate: u32) -> Vec<f32> {
		let sr = f64::from(rate);
		let mut x = vec![0.0f64; (seconds * sr) as usize];
		let mut seed = 0x2545_f491_4f6c_dd1d_u64;
		for &(at, kind) in hits {
			let start = (at * sr) as usize;
			let mut phase = 0.0;
			let mut lp = 0.0;
			let mut hp = [0.0f64; 3];
			let mut prev = [0.0f64; 3];
			for i in 0..(0.4 * sr) as usize {
				let t = i as f64 / sr;
				let Some(slot) = x.get_mut(start + i) else {
					break;
				};
				let v = match kind {
					DrumKind::Kick => {
						phase +=
							2.0 * std::f64::consts::PI * (50.0 + 100.0 * (-t * 30.0).exp()) / sr;
						phase.sin() * (-t * 8.0).exp()
					}
					DrumKind::Snare => {
						// Wires: noise low-passed around 5 kHz.
						lp += 0.5 * (noise(&mut seed) - lp);
						let tone =
							(2.0 * std::f64::consts::PI * 200.0 * t).sin() * (-t * 25.0).exp();
						tone * 0.5 + lp * (-t * 18.0).exp() * 0.8
					}
					_ => {
						// Noise high-passed three times (above ~3 kHz).
						let mut w = noise(&mut seed);
						for (h, p) in hp.iter_mut().zip(prev.iter_mut()) {
							let y = 0.6 * (*h + w - *p);
							*p = w;
							*h = y;
							w = y;
						}
						w * (-t * 60.0).exp() * 0.8
					}
				};
				*slot += v;
			}
		}
		x.into_iter().map(|v| v as f32).collect()
	}

	#[test]
	fn a_back_beat_comes_out_as_kick_snare_and_hats() {
		let rate = 44_100;
		let mut pattern = Vec::new();
		for bar in 0..2 {
			let b = bar as f64 * 2.0;
			pattern.push((b, DrumKind::Kick));
			pattern.push((b + 0.5, DrumKind::Snare));
			pattern.push((b + 1.0, DrumKind::Kick));
			pattern.push((b + 1.5, DrumKind::Snare));
			for e in 0..4 {
				pattern.push((b + 0.25 + 0.5 * e as f64, DrumKind::ClosedHat));
			}
		}
		let x = kit(&pattern, 4.5, rate);
		let found = detect(&x, rate, &DrumOptions::default());
		let got: Vec<(f64, DrumKind)> = found
			.iter()
			.map(|h| ((h.time * 100.0).round() / 100.0, h.kind))
			.collect();
		let mut want = pattern.clone();
		want.sort_by(|a, b| a.0.total_cmp(&b.0));
		assert_eq!(got.len(), want.len(), "{got:?}");
		for ((t, k), (wt, wk)) in got.iter().zip(&want) {
			assert_eq!(k, wk, "{got:?}");
			assert!((t - wt).abs() <= 0.015, "{t} vs {wt}");
		}
	}
}
