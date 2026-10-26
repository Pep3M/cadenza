//! Synthetic monophonic phrases with exact ground truth.
//!
//! A voice-like source (harmonics with a glottal tilt through vowel formants, fixed in Hz
//! like a real vocal tract) follows a pitch curve built from the notes: legato glides
//! (raised cosine centered on the boundary), scoops and falls, delayed vibrato, slow drift.
//! Breaths and consonant bursts are noise; background noise and a Schroeder reverb are
//! optional. Everything is deterministic (seeded).
//!
//! Truth conventions (also used for real labels):
//! - a legato boundary sits at the middle of the glide, where the pitch crosses halfway;
//! - the core excludes the glides, scoops, falls, the 20 ms attack and the 30 ms release;
//! - the center is the mean of the sung pitch over the core without vibrato (nominal +
//!   detune + drift): what a listener calls "the note" and what tuning moves.

use std::f64::consts::PI;

use super::frames::{FrameState, TRUTH_HOP, TruthFrame};
use super::{Case, TruthNote};

const LEAD_IN: f64 = 0.25;
const TAIL: f64 = 0.5;
const ATTACK: f64 = 0.02;
const RELEASE: f64 = 0.03;
/// Shortest amplitude crossfade between legato notes.
const MIN_CROSSFADE: f64 = 0.02;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bend {
	/// Offset at the far end of the bend (negative = from/to below).
	pub cents: f64,
	pub seconds: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Vibrato {
	pub rate_hz: f64,
	/// Peak deviation.
	pub depth_cents: f64,
	/// Seconds after the onset before it fades in (over 150 ms).
	pub delay: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SynthNote {
	/// Intended note (MIDI number).
	pub pitch: f64,
	pub detune_cents: f64,
	/// Onset to offset, seconds.
	pub duration: f64,
	/// Silence before the note; 0 = legato from the previous note.
	pub rest_before: f64,
	/// Legato only: portamento length, centered on the boundary.
	pub glide: f64,
	/// Detached notes only: pitch approach at the start.
	pub scoop: Option<Bend>,
	/// Before a rest only: pitch drop at the end.
	pub fall: Option<Bend>,
	pub vibrato: Option<Vibrato>,
	/// Linear drift over the note, from 0 to this.
	pub drift_cents: f64,
	/// 0..1.
	pub level: f64,
	/// Breath noise in the rest before the note.
	pub breath_before: bool,
	/// Short noise burst (t, k, s) right before the onset.
	pub consonant: bool,
}

impl SynthNote {
	pub fn new(pitch: f64, duration: f64) -> Self {
		Self {
			pitch,
			detune_cents: 0.0,
			duration,
			rest_before: 0.15,
			glide: 0.0,
			scoop: None,
			fall: None,
			vibrato: None,
			drift_cents: 0.0,
			level: 0.8,
			breath_before: false,
			consonant: false,
		}
	}

	pub fn legato(mut self, glide: f64) -> Self {
		self.rest_before = 0.0;
		self.glide = glide;
		self
	}

	pub fn rest(mut self, seconds: f64) -> Self {
		self.rest_before = seconds;
		self
	}

	fn is_legato(&self) -> bool {
		self.rest_before <= 0.0
	}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Vowel {
	A,
	E,
	I,
	O,
	U,
}

impl Vowel {
	/// (frequency Hz, bandwidth Hz, gain) of the first four formants.
	fn formants(self) -> [(f64, f64, f64); 4] {
		match self {
			Vowel::A => [
				(730.0, 90.0, 1.0),
				(1090.0, 110.0, 0.5),
				(2440.0, 160.0, 0.25),
				(3400.0, 250.0, 0.1),
			],
			Vowel::E => [
				(530.0, 80.0, 1.0),
				(1840.0, 120.0, 0.45),
				(2480.0, 160.0, 0.3),
				(3500.0, 250.0, 0.1),
			],
			Vowel::I => [
				(290.0, 60.0, 1.0),
				(2250.0, 120.0, 0.35),
				(2900.0, 170.0, 0.3),
				(3700.0, 250.0, 0.1),
			],
			Vowel::O => [
				(570.0, 80.0, 1.0),
				(840.0, 100.0, 0.6),
				(2410.0, 160.0, 0.15),
				(3300.0, 250.0, 0.08),
			],
			Vowel::U => [
				(300.0, 60.0, 1.0),
				(870.0, 100.0, 0.4),
				(2240.0, 160.0, 0.1),
				(3300.0, 250.0, 0.05),
			],
		}
	}

	fn gain(self, hz: f64) -> f64 {
		0.03 + self
			.formants()
			.iter()
			.map(|&(f, b, g)| g / (1.0 + ((hz - f) / b).powi(2)))
			.sum::<f64>()
	}
}

#[derive(Debug, Clone, PartialEq)]
pub struct Phrase {
	pub notes: Vec<SynthNote>,
	pub vowel: Vowel,
	pub sample_rate: u32,
	/// Background white noise relative to the voice.
	pub snr_db: Option<f64>,
	/// Reverb wet level 0..1.
	pub reverb: Option<f64>,
	pub seed: u64,
}

/// Where a note sits on the timeline (seconds).
#[derive(Debug, Clone, Copy)]
struct Span {
	onset: f64,
	offset: f64,
	legato_in: bool,
	legato_out: bool,
	glide_in: f64,
	glide_out: f64,
}

fn layout(notes: &[SynthNote]) -> Vec<Span> {
	let mut spans: Vec<Span> = Vec::with_capacity(notes.len());
	let mut cursor = LEAD_IN;
	for (i, n) in notes.iter().enumerate() {
		let legato_in = i > 0 && n.is_legato();
		let onset = if legato_in {
			cursor
		} else {
			cursor + n.rest_before.max(0.0)
		};
		let glide_in = if legato_in { n.glide } else { 0.0 };
		if let Some(prev) = spans.last_mut() {
			prev.legato_out = legato_in;
			prev.glide_out = glide_in;
		}
		spans.push(Span {
			onset,
			offset: onset + n.duration,
			legato_in,
			legato_out: false,
			glide_in,
			glide_out: 0.0,
		});
		cursor = onset + n.duration;
	}
	spans
}

fn smoothstep(x: f64) -> f64 {
	let x = x.clamp(0.0, 1.0);
	0.5 - 0.5 * (PI * x).cos()
}

/// Sung pitch of one note at `t` (fractional MIDI), ignoring its neighbours.
fn note_pitch(n: &SynthNote, s: &Span, t: f64, with_vibrato: bool) -> f64 {
	let u = ((t - s.onset) / n.duration).clamp(0.0, 1.0);
	let mut cents = n.detune_cents + n.drift_cents * u;
	if with_vibrato && let Some(v) = n.vibrato {
		let since = t - s.onset - v.delay;
		if since > 0.0 {
			cents +=
				v.depth_cents * smoothstep(since / 0.15) * (2.0 * PI * v.rate_hz * since).sin();
		}
	}
	if !s.legato_in
		&& let Some(b) = n.scoop
	{
		cents += b.cents * (1.0 - smoothstep((t - s.onset) / b.seconds));
	}
	if !s.legato_out
		&& let Some(b) = n.fall
	{
		let start = s.offset - RELEASE - b.seconds;
		if t > start {
			cents += b.cents * smoothstep((t - start) / b.seconds);
		}
	}
	n.pitch + cents / 100.0
}

/// Ground truth of a phrase.
pub fn truth(phrase: &Phrase) -> Vec<TruthNote> {
	let spans = layout(&phrase.notes);
	phrase
		.notes
		.iter()
		.zip(&spans)
		.map(|(n, s)| {
			let mut core_start = s.onset;
			core_start += if s.legato_in {
				s.glide_in / 2.0
			} else {
				ATTACK
			};
			if !s.legato_in
				&& let Some(b) = n.scoop
			{
				core_start = core_start.max(s.onset + b.seconds);
			}
			let mut core_end = s.offset;
			core_end -= if s.legato_out {
				s.glide_out / 2.0
			} else {
				RELEASE
			};
			if !s.legato_out
				&& let Some(b) = n.fall
			{
				core_end = s.offset - RELEASE - b.seconds;
			}
			assert!(
				core_end > core_start + 0.01,
				"note {n:?} has no stable core"
			);
			let steps = ((core_end - core_start) / 0.001).ceil().max(1.0) as usize;
			let center = (0..steps)
				.map(|k| {
					let u = ((core_start
						+ (k as f64 + 0.5) * (core_end - core_start) / steps as f64
						- s.onset) / n.duration)
						.clamp(0.0, 1.0);
					n.detune_cents + n.drift_cents * u
				})
				.sum::<f64>()
				/ steps as f64;
			TruthNote {
				onset: s.onset,
				offset: s.offset,
				nominal: n.pitch,
				center: n.pitch + center / 100.0,
				core_start,
				core_end,
			}
		})
		.collect()
}

/// A rendered phrase: mono samples (peak 0.7), note truth and frame truth.
#[derive(Debug, Clone)]
pub struct Rendered {
	pub samples: Vec<f32>,
	pub truth: Vec<TruthNote>,
	pub frames: Vec<TruthFrame>,
}

/// Renders a phrase.
pub fn render(phrase: &Phrase) -> Rendered {
	let notes = &phrase.notes;
	let truth = truth(phrase);
	let spans = layout(notes);
	let sr = f64::from(phrase.sample_rate.max(8_000));
	let end = spans.last().map_or(LEAD_IN, |s| s.offset)
		+ TAIL + if phrase.reverb.is_some() { 1.0 } else { 0.0 };
	let len = (end * sr) as usize;
	let mut rng = Rng::new(phrase.seed);

	let mut voice = vec![0.0f64; len];
	let mut amp = vec![0.0f64; len];
	let mut sung = vec![0.0f64; len];
	// Level relative to the sounding note's own level: 1 = fully sounding.
	let mut relative = vec![0.0f64; len];
	let mut gains = [0.0f64; 64];
	let mut gains_for = 0usize;
	let mut phase = 0.0f64;
	let mut current = 0usize;
	for (i, out) in voice.iter_mut().enumerate() {
		let t = i as f64 / sr;
		while current + 1 < notes.len()
			&& t >= spans[current + 1].onset - spans[current + 1].glide_in / 2.0
		{
			current += 1;
		}
		let (n, s) = (&notes[current], &spans[current]);
		let mut pitch = note_pitch(n, s, t, true);
		let mut level = n.level * envelope(s, t);
		if s.legato_in && current > 0 {
			let (pn, ps) = (&notes[current - 1], &spans[current - 1]);
			let w = if s.glide_in > 0.0 {
				smoothstep((t - (s.onset - s.glide_in / 2.0)) / s.glide_in)
			} else if t >= s.onset {
				1.0
			} else {
				0.0
			};
			pitch = (1.0 - w) * note_pitch(pn, ps, t, true) + w * pitch;
			level += pn.level * envelope(ps, t);
		}
		if current + 1 < notes.len() && spans[current + 1].legato_in {
			level += notes[current + 1].level * envelope(&spans[current + 1], t);
		}
		amp[i] = level;
		sung[i] = pitch;
		relative[i] = level / n.level.max(1e-9);
		let hz = crate::midi_to_hz(pitch);
		phase = (phase + 2.0 * PI * hz / sr) % (2.0 * PI);
		if level <= 0.0 {
			continue;
		}
		let harmonics = ((0.45 * sr / hz) as usize).clamp(1, 30);
		if i.is_multiple_of(32) || harmonics != gains_for {
			gains_for = harmonics;
			for (k, g) in gains.iter_mut().enumerate().take(harmonics) {
				let k1 = (k + 1) as f64;
				*g = phrase.vowel.gain(k1 * hz) / k1.sqrt();
			}
		}
		// sin(k phase) by the Chebyshev recurrence.
		let (s1, c1) = phase.sin_cos();
		let (mut prev, mut cur) = (0.0, s1);
		let mut sum = 0.0;
		for g in gains.iter().take(harmonics) {
			sum += g * cur;
			let next = 2.0 * c1 * cur - prev;
			prev = cur;
			cur = next;
		}
		*out = level * sum;
	}
	let peak_voice = voice.iter().fold(0.0f64, |m, v| m.max(v.abs())).max(1e-9);
	for v in &mut voice {
		*v /= peak_voice;
	}

	let mut noise_track = vec![0.0f64; len];
	let mut hp = 0.0f64;
	for (n, s) in notes.iter().zip(&spans) {
		if n.breath_before && !s.legato_in && n.rest_before >= 0.15 {
			add_noise(
				&mut noise_track,
				&mut rng,
				&mut hp,
				sr,
				s.onset - n.rest_before + 0.03,
				s.onset - 0.05,
				0.06,
			);
		}
		if n.consonant {
			add_noise(
				&mut noise_track,
				&mut rng,
				&mut hp,
				sr,
				s.onset - 0.04,
				s.onset + 0.005,
				0.2,
			);
		}
	}
	let mut mix: Vec<f64> = voice.iter().zip(&noise_track).map(|(v, n)| v + n).collect();

	if let Some(snr) = phrase.snr_db {
		let (sum, count) = voice
			.iter()
			.zip(&amp)
			.filter(|(_, a)| **a > 0.1)
			.fold((0.0, 0usize), |(s, c), (v, _)| (s + v * v, c + 1));
		let rms = (sum / count.max(1) as f64).sqrt();
		let noise_rms = rms / 10f64.powf(snr / 20.0);
		// Uniform white noise has rms = range / sqrt(3).
		let scale = noise_rms * 3f64.sqrt();
		for m in &mut mix {
			*m += scale * rng.bipolar();
		}
	}
	if let Some(wet) = phrase.reverb {
		mix = reverb(&mix, sr, wet);
	}
	let peak = mix.iter().fold(0.0f64, |m, v| m.max(v.abs())).max(1e-9);
	let samples = mix.iter().map(|v| (0.7 * v / peak) as f32).collect();
	let frames = (0..)
		.map(|k| k as f64 * TRUTH_HOP)
		.take_while(|t| *t < len as f64 / sr)
		.map(|time| {
			let i = ((time * sr).round() as usize).min(len - 1);
			let state = if relative[i] >= 0.5 {
				FrameState::Voiced {
					pitch: sung[i],
					tolerance_cents: 50.0,
				}
			} else if relative[i] < 0.01 && phrase.reverb.is_none() {
				FrameState::Unvoiced
			} else {
				FrameState::Ignore
			};
			TruthFrame { time, state }
		})
		.collect();
	Rendered {
		samples,
		truth,
		frames,
	}
}

/// Amplitude of one note at `t` (0..1): attack/release when detached, complementary
/// crossfades with legato neighbours.
fn envelope(s: &Span, t: f64) -> f64 {
	let rise = if s.legato_in {
		let c = s.glide_in.max(MIN_CROSSFADE);
		smoothstep((t - (s.onset - c / 2.0)) / c)
	} else {
		smoothstep((t - s.onset) / ATTACK)
	};
	let fall = if s.legato_out {
		let c = s.glide_out.max(MIN_CROSSFADE);
		1.0 - smoothstep((t - (s.offset - c / 2.0)) / c)
	} else {
		1.0 - smoothstep((t - (s.offset - RELEASE)) / RELEASE)
	};
	rise.min(fall)
}

#[allow(clippy::too_many_arguments)]
fn add_noise(
	out: &mut [f64],
	rng: &mut Rng,
	hp: &mut f64,
	sr: f64,
	from: f64,
	to: f64,
	level: f64,
) {
	if to <= from {
		return;
	}
	let (a, b) = (
		(from.max(0.0) * sr) as usize,
		((to * sr) as usize).min(out.len()),
	);
	let Some(span) = out.get_mut(a..b) else {
		return;
	};
	let n = span.len().max(1) as f64;
	for (k, sample) in span.iter_mut().enumerate() {
		let white = rng.bipolar();
		// First difference = a gentle high-pass: breathy, not rumbly.
		let bright = white - *hp;
		*hp = white;
		*sample += level * (PI * k as f64 / n).sin() * bright;
	}
}

/// Schroeder reverb: four combs into two all-passes.
fn reverb(input: &[f64], sr: f64, wet: f64) -> Vec<f64> {
	let scale = sr / 44_100.0;
	let mut out = vec![0.0f64; input.len()];
	for (delay, feedback) in [(1557, 0.8), (1617, 0.79), (1491, 0.78), (1422, 0.77)] {
		let d = ((delay as f64 * scale) as usize).max(1);
		let mut line = vec![0.0f64; d];
		let mut damp = 0.0;
		for (i, x) in input.iter().enumerate() {
			let y = line[i % d];
			damp = 0.7 * y + 0.3 * damp;
			line[i % d] = x + feedback * damp;
			out[i] += 0.25 * y;
		}
	}
	for delay in [225, 556] {
		let d = ((delay as f64 * scale) as usize).max(1);
		let mut line = vec![0.0f64; d];
		for (i, v) in out.iter_mut().enumerate() {
			let y = -0.5 * *v + line[i % d];
			line[i % d] = *v + 0.5 * y;
			*v = y;
		}
	}
	input
		.iter()
		.zip(&out)
		.map(|(x, r)| (1.0 - wet) * x + wet * r)
		.collect()
}

/// xorshift64*: tiny, deterministic, good enough for test material.
#[derive(Debug, Clone)]
pub struct Rng(u64);

impl Rng {
	pub fn new(seed: u64) -> Self {
		Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
	}

	pub fn next_u64(&mut self) -> u64 {
		self.0 ^= self.0 >> 12;
		self.0 ^= self.0 << 25;
		self.0 ^= self.0 >> 27;
		self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
	}

	/// Uniform in [0, 1).
	pub fn unit(&mut self) -> f64 {
		(self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
	}

	pub fn range(&mut self, lo: f64, hi: f64) -> f64 {
		lo + (hi - lo) * self.unit()
	}

	pub fn bipolar(&mut self) -> f64 {
		2.0 * self.unit() - 1.0
	}

	pub fn chance(&mut self, p: f64) -> bool {
		self.unit() < p
	}
}

const MAJOR: [i32; 7] = [0, 2, 4, 5, 7, 9, 11];

/// A random walk on C major between `low` and `high` (MIDI), steps of up to a fourth, never
/// the same note twice in a row (bouncing off the range edges): two legato notes of one
/// pitch with no articulation are one note, not a test of segmentation.
fn melody(rng: &mut Rng, low: i32, high: i32, count: usize) -> Vec<f64> {
	let degrees: Vec<i32> = (low..=high)
		.filter(|p| MAJOR.contains(&p.rem_euclid(12)))
		.collect();
	let mut at = degrees.len() / 2;
	let mut out = Vec::with_capacity(count);
	for _ in 0..count {
		out.push(f64::from(degrees[at]));
		let mut step = (rng.range(-3.0, 3.99)).floor() as i32;
		if step == 0 {
			step = 1;
		}
		let last = degrees.len() as i32 - 1;
		let mut next = at as i32 + step;
		if next < 0 || next > last {
			next = at as i32 - step;
		}
		at = next.clamp(0, last) as usize;
	}
	out
}

fn detune(rng: &mut Rng, max: f64) -> f64 {
	rng.range(-max, max)
}

/// The synthetic corpus: every situation the segmentation has to survive, at
/// `sample_rate` (22.05 kHz is plenty for analysis; detectors must accept any rate).
pub fn corpus(sample_rate: u32) -> Vec<Case> {
	let mut cases = Vec::new();
	let mut add = |name: &str, tags: &[&str], phrase: Phrase| {
		let r = render(&phrase);
		cases.push(Case {
			name: name.to_string(),
			tags: tags.iter().map(|t| t.to_string()).collect(),
			sample_rate,
			samples: r.samples,
			truth: r.truth,
			frames: r.frames,
		});
	};
	let phrase = |notes: Vec<SynthNote>, vowel: Vowel, seed: u64| Phrase {
		notes,
		vowel,
		sample_rate,
		snr_db: None,
		reverb: None,
		seed,
	};

	for (name, low, high, vowel, seed) in [
		("steady_low", 40, 52, Vowel::O, 1),
		("steady_mid", 50, 64, Vowel::A, 2),
		("steady_high", 67, 79, Vowel::E, 3),
	] {
		let mut rng = Rng::new(seed);
		let notes = melody(&mut rng, low, high, 10)
			.into_iter()
			.map(|p| {
				let mut n = SynthNote::new(p, rng.range(0.3, 0.7)).rest(rng.range(0.1, 0.25));
				n.detune_cents = detune(&mut rng, 25.0);
				n
			})
			.collect();
		add(name, &["steady"], phrase(notes, vowel, seed));
	}

	for (name, glide, low, high, seed) in [
		("legato_glide_short", (0.0, 0.04), 52, 67, 10),
		("legato_glide_long", (0.08, 0.25), 52, 67, 11),
		("legato_low", (0.06, 0.15), 40, 52, 12),
	] {
		let mut rng = Rng::new(seed);
		let notes = melody(&mut rng, low, high, 12)
			.into_iter()
			.enumerate()
			.map(|(i, p)| {
				let mut n = SynthNote::new(p, rng.range(0.35, 0.6));
				if i > 0 && !rng.chance(0.15) {
					n = n.legato(rng.range(glide.0, glide.1));
				}
				n.detune_cents = detune(&mut rng, 20.0);
				n
			})
			.collect();
		add(name, &["glide", "legato"], phrase(notes, Vowel::A, seed));
	}

	let mut rng = Rng::new(20);
	let notes = melody(&mut rng, 55, 69, 10)
		.into_iter()
		.map(|p| {
			let mut n = SynthNote::new(p, rng.range(0.4, 0.7)).rest(rng.range(0.12, 0.25));
			n.scoop = Some(Bend {
				cents: rng.range(-250.0, -80.0),
				seconds: rng.range(0.06, 0.14),
			});
			if rng.chance(0.5) {
				n.fall = Some(Bend {
					cents: rng.range(-300.0, -100.0),
					seconds: rng.range(0.08, 0.15),
				});
			}
			n.detune_cents = detune(&mut rng, 15.0);
			n
		})
		.collect();
	add(
		"scoops_falls",
		&["scoop", "fall"],
		phrase(notes, Vowel::O, 20),
	);

	let mut rng = Rng::new(30);
	let notes = melody(&mut rng, 55, 72, 8)
		.into_iter()
		.enumerate()
		.map(|(i, p)| {
			let mut n = SynthNote::new(p, rng.range(0.8, 1.4));
			if i > 0 && rng.chance(0.5) {
				n = n.legato(0.06);
			}
			n.vibrato = Some(Vibrato {
				rate_hz: rng.range(5.0, 6.5),
				depth_cents: rng.range(30.0, 90.0),
				delay: rng.range(0.1, 0.3),
			});
			n.detune_cents = detune(&mut rng, 20.0);
			n
		})
		.collect();
	add("vibrato", &["vibrato"], phrase(notes, Vowel::A, 30));

	let mut rng = Rng::new(40);
	let notes = melody(&mut rng, 52, 67, 10)
		.into_iter()
		.map(|p| {
			let mut n = SynthNote::new(p, rng.range(0.6, 1.0)).rest(rng.range(0.1, 0.2));
			n.drift_cents = rng.range(20.0, 60.0) * if rng.chance(0.5) { -1.0 } else { 1.0 };
			n
		})
		.collect();
	add("drift", &["drift"], phrase(notes, Vowel::E, 40));

	let mut rng = Rng::new(50);
	let mut notes = Vec::new();
	for p in melody(&mut rng, 52, 67, 4) {
		for k in 0..rng.range(3.0, 4.99) as usize {
			let mut n = SynthNote::new(p, rng.range(0.18, 0.3)).rest(if k == 0 {
				0.2
			} else {
				rng.range(0.03, 0.06)
			});
			n.consonant = k > 0;
			n.detune_cents = detune(&mut rng, 10.0);
			notes.push(n);
		}
	}
	add(
		"repeated",
		&["repeated", "consonant"],
		phrase(notes, Vowel::A, 50),
	);

	let mut rng = Rng::new(60);
	let mut notes = Vec::new();
	for p in melody(&mut rng, 55, 69, 8) {
		let above = if rng.chance(0.5) { 2.0 } else { -1.0 };
		notes.push(SynthNote::new(p + above, rng.range(0.07, 0.09)).rest(0.2));
		let mut main = SynthNote::new(p, rng.range(0.4, 0.6)).legato(0.02);
		main.detune_cents = detune(&mut rng, 15.0);
		notes.push(main);
	}
	add(
		"grace_notes",
		&["grace", "short"],
		phrase(notes, Vowel::O, 60),
	);

	let mut rng = Rng::new(70);
	let mut notes = Vec::new();
	for run in 0..4 {
		for (k, p) in melody(&mut rng, 57, 72, 7).into_iter().enumerate() {
			let mut n = SynthNote::new(p, rng.range(0.08, 0.14));
			n = if k == 0 {
				n.rest(if run == 0 { 0.1 } else { 0.25 })
			} else {
				n.legato(rng.range(0.015, 0.03))
			};
			n.detune_cents = detune(&mut rng, 15.0);
			notes.push(n);
		}
	}
	add(
		"melisma",
		&["melisma", "short", "legato"],
		phrase(notes, Vowel::A, 70),
	);

	let mut rng = Rng::new(80);
	let base = melody(&mut rng, 48, 60, 6);
	let mut notes = Vec::new();
	for (i, p) in base.into_iter().enumerate() {
		for (k, q) in [p, p + 12.0].into_iter().enumerate() {
			let mut n = SynthNote::new(q, rng.range(0.3, 0.5));
			n = if k == 1 && i.is_multiple_of(2) {
				n.legato(0.05)
			} else {
				n.rest(0.12)
			};
			notes.push(n);
		}
	}
	add("octave_leaps", &["octave"], phrase(notes, Vowel::O, 80));

	let mut rng = Rng::new(90);
	let notes = melody(&mut rng, 52, 67, 8)
		.into_iter()
		.map(|p| {
			let mut n = SynthNote::new(p, rng.range(0.4, 0.8)).rest(rng.range(0.25, 0.4));
			n.breath_before = true;
			n.detune_cents = detune(&mut rng, 15.0);
			n
		})
		.collect();
	add("breaths", &["breath"], phrase(notes, Vowel::A, 90));

	for (name, tags, low, high, vowel, snr, wet, seed) in [
		(
			"expressive_mid",
			&["expressive"][..],
			52,
			67,
			Vowel::A,
			None,
			None,
			100,
		),
		(
			"expressive_low",
			&["expressive"][..],
			40,
			55,
			Vowel::O,
			None,
			None,
			101,
		),
		(
			"expressive_high",
			&["expressive"][..],
			64,
			79,
			Vowel::I,
			None,
			None,
			102,
		),
		(
			"noisy_snr20",
			&["expressive", "noise"][..],
			52,
			67,
			Vowel::E,
			Some(20.0),
			None,
			103,
		),
		(
			"noisy_snr10",
			&["expressive", "noise"][..],
			52,
			67,
			Vowel::A,
			Some(10.0),
			None,
			104,
		),
		(
			"reverb",
			&["expressive", "reverb"][..],
			52,
			67,
			Vowel::O,
			None,
			Some(0.3),
			105,
		),
	] {
		let mut rng = Rng::new(seed);
		let notes = expressive(&mut rng, low, high, 12);
		let mut p = phrase(notes, vowel, seed);
		p.snr_db = snr;
		p.reverb = wet;
		add(name, tags, p);
	}
	cases
}

/// A mix of everything: legato glides, scoops, falls, vibrato on long notes, drift.
fn expressive(rng: &mut Rng, low: i32, high: i32, count: usize) -> Vec<SynthNote> {
	melody(rng, low, high, count)
		.into_iter()
		.enumerate()
		.map(|(i, p)| {
			let long = rng.chance(0.4);
			let mut n = SynthNote::new(
				p,
				if long {
					rng.range(0.8, 1.2)
				} else {
					rng.range(0.25, 0.5)
				},
			);
			if i > 0 && rng.chance(0.55) {
				n = n.legato(rng.range(0.02, 0.2));
			} else {
				n = n.rest(rng.range(0.1, 0.3));
				n.breath_before = rng.chance(0.3);
				if rng.chance(0.5) {
					n.scoop = Some(Bend {
						cents: rng.range(-200.0, -60.0),
						seconds: rng.range(0.05, 0.12),
					});
				}
			}
			if rng.chance(0.25) {
				n.fall = Some(Bend {
					cents: rng.range(-250.0, -80.0),
					seconds: rng.range(0.06, 0.12),
				});
			}
			if long {
				n.vibrato = Some(Vibrato {
					rate_hz: rng.range(4.8, 6.5),
					depth_cents: rng.range(25.0, 80.0),
					delay: rng.range(0.15, 0.35),
				});
			}
			n.drift_cents = rng.range(-30.0, 30.0);
			n.detune_cents = detune(rng, 25.0);
			n.level = rng.range(0.55, 1.0);
			n
		})
		.collect()
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::yin::{self, YinOptions};

	fn one(notes: Vec<SynthNote>) -> Phrase {
		Phrase {
			notes,
			vowel: Vowel::A,
			sample_rate: 22_050,
			snr_db: None,
			reverb: None,
			seed: 7,
		}
	}

	#[test]
	fn legato_boundary_is_the_middle_of_the_glide() {
		let mut b = SynthNote::new(64.0, 0.5).legato(0.2);
		b.detune_cents = 10.0;
		let t = truth(&one(vec![SynthNote::new(60.0, 0.5), b]));
		assert_eq!(t[0].offset, t[1].onset);
		assert!((t[1].core_start - (t[1].onset + 0.1)).abs() < 1e-12);
		assert!((t[0].core_end - (t[0].offset - 0.1)).abs() < 1e-12);
		assert!((t[1].center - 64.1).abs() < 1e-9);
	}

	#[test]
	fn drift_moves_the_center_to_its_mean_over_the_core() {
		let mut n = SynthNote::new(60.0, 1.0);
		n.drift_cents = 40.0;
		let t = truth(&one(vec![n]));
		// Core 0.02..0.97 of the note: mean ramp = 40 * 0.495.
		assert!(
			(t[0].center - (60.0 + 0.4 * 0.495)).abs() < 1e-3,
			"{}",
			t[0].center
		);
	}

	#[test]
	fn rendered_pitch_matches_the_truth_inside_cores() {
		let mut b = SynthNote::new(67.0, 0.6).legato(0.12);
		b.detune_cents = -20.0;
		let mut c = SynthNote::new(62.0, 0.6).rest(0.2);
		c.scoop = Some(Bend {
			cents: -150.0,
			seconds: 0.1,
		});
		let phrase = one(vec![SynthNote::new(60.0, 0.6), b, c]);
		let r = render(&phrase);
		let frames = yin::track(&r.samples, phrase.sample_rate, &YinOptions::default());
		for t in &r.truth {
			let inside: Vec<f64> = frames
				.iter()
				.filter(|f| f.time > t.core_start + 0.03 && f.time < t.core_end - 0.03)
				.map(|f| f.pitch.expect("voiced inside the core"))
				.collect();
			assert!(!inside.is_empty());
			for p in inside {
				assert!((p - t.center).abs() < 0.05, "{p} vs {}", t.center);
			}
		}
	}

	#[test]
	fn corpus_is_deterministic_and_well_formed() {
		let a = corpus(16_000);
		let b = corpus(16_000);
		assert!(a.len() >= 15);
		for (x, y) in a.iter().zip(&b) {
			assert_eq!(x.samples, y.samples);
			assert!(x.samples.iter().all(|s| s.is_finite() && s.abs() <= 0.71));
			for w in x.truth.windows(2) {
				assert!(
					w[0].offset <= w[1].onset + 1e-9,
					"{}: overlapping truth",
					x.name
				);
			}
			let length = x.samples.len() as f64 / 16_000.0;
			assert!(x.truth.last().unwrap().offset < length);
		}
	}
}
