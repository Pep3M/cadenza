//! Hand percussion for the `drum_machine`: rim, cowbell, bongos, congas, timbales, agogo,
//! shakers, guiro, claves, woodblocks, tambourine and triangle, synthesized from a tuned
//! body (sine pair or band-passed square pair) plus filtered noise.

use crate::util::biquad::{Biquad, BiquadCoeffs};
use crate::util::envelope::DecayEnv;
use crate::util::{Rng, TAU};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Body {
	/// Two sines (`freq`, `freq * ratio`).
	Sine,
	/// Two squares through a band-pass: the analog cowbell.
	Square,
}

/// How one GM note is synthesized.
#[derive(Debug, Clone, Copy)]
struct Spec {
	note: u8,
	body: Body,
	freq: f32,
	/// Second partial frequency ratio and level (0 = none).
	ratio: f32,
	partial: f32,
	tone_ms: f32,
	/// Pitch drop at the attack (fraction of `freq`).
	bend: f32,
	tone: f32,
	noise_freq: f32,
	noise_q: f32,
	noise_ms: f32,
	noise: f32,
	attack_ms: f32,
	/// Scrape rate (Hz) and length (ms) of a gated noise train (guiro); 0 = none.
	scrape_hz: f32,
	scrape_ms: f32,
	pan: f32,
	/// Voices of the same non-zero group cut each other (mute/open conga, triangle).
	choke: u8,
}

const fn spec(note: u8, body: Body, freq: f32, tone_ms: f32, tone: f32) -> Spec {
	Spec {
		note,
		body,
		freq,
		ratio: 0.0,
		partial: 0.0,
		tone_ms,
		bend: 0.0,
		tone,
		noise_freq: 2_000.0,
		noise_q: 1.0,
		noise_ms: 10.0,
		noise: 0.0,
		attack_ms: 0.0,
		scrape_hz: 0.0,
		scrape_ms: 0.0,
		pan: 0.0,
		choke: 0,
	}
}

const fn partial(mut s: Spec, ratio: f32, level: f32) -> Spec {
	s.ratio = ratio;
	s.partial = level;
	s
}

const fn noise(mut s: Spec, freq: f32, q: f32, ms: f32, level: f32) -> Spec {
	s.noise_freq = freq;
	s.noise_q = q;
	s.noise_ms = ms;
	s.noise = level;
	s
}

const fn shape(mut s: Spec, bend: f32, attack_ms: f32, pan: f32, choke: u8) -> Spec {
	s.bend = bend;
	s.attack_ms = attack_ms;
	s.pan = pan;
	s.choke = choke;
	s
}

const fn scrape(mut s: Spec, hz: f32, ms: f32) -> Spec {
	s.scrape_hz = hz;
	s.scrape_ms = ms;
	s
}

use Body::{Sine, Square};

const NONE: f32 = 0.0;

static SPECS: [Spec; 24] = [
	// 37 side stick / rim: a woody knock with a click.
	noise(
		partial(spec(37, Sine, 1_700.0, 28.0, 0.55), 0.27, 0.9),
		4_000.0,
		0.8,
		8.0,
		0.5,
	),
	// 54 tambourine: jingles = bright noise with a short attack.
	shape(
		noise(
			spec(54, Sine, 7_200.0, 40.0, 0.08),
			8_200.0,
			1.3,
			220.0,
			0.75,
		),
		NONE,
		1.5,
		0.25,
		0,
	),
	// 56 cowbell (808 style).
	shape(
		partial(spec(56, Square, 540.0, 260.0, 0.4), 1.48, 1.0),
		NONE,
		NONE,
		-0.15,
		0,
	),
	// 60/61 bongos.
	shape(
		noise(spec(60, Sine, 420.0, 110.0, 0.9), 2_400.0, 1.0, 12.0, 0.25),
		0.12,
		NONE,
		0.3,
		0,
	),
	shape(
		noise(spec(61, Sine, 300.0, 150.0, 0.9), 2_000.0, 1.0, 12.0, 0.25),
		0.12,
		NONE,
		0.2,
		0,
	),
	// 62 mute conga (slap), 63 open conga, 64 low conga.
	shape(
		noise(spec(62, Sine, 340.0, 45.0, 0.8), 1_800.0, 0.9, 30.0, 0.45),
		0.05,
		NONE,
		-0.25,
		1,
	),
	shape(
		noise(spec(63, Sine, 300.0, 280.0, 0.9), 1_500.0, 1.0, 14.0, 0.2),
		0.08,
		NONE,
		-0.25,
		1,
	),
	shape(
		noise(spec(64, Sine, 210.0, 320.0, 0.95), 1_200.0, 1.0, 14.0, 0.2),
		0.08,
		NONE,
		-0.4,
		0,
	),
	// 65/66 timbales: tuned shells with a metallic ring.
	shape(
		noise(spec(65, Sine, 480.0, 280.0, 0.7), 3_200.0, 0.9, 200.0, 0.35),
		0.15,
		NONE,
		0.35,
		0,
	),
	shape(
		noise(spec(66, Sine, 360.0, 300.0, 0.7), 2_800.0, 0.9, 220.0, 0.35),
		0.15,
		NONE,
		0.15,
		0,
	),
	// 67/68 agogo bells.
	shape(
		partial(spec(67, Sine, 920.0, 300.0, 0.55), 2.71, 0.5),
		NONE,
		NONE,
		0.3,
		0,
	),
	shape(
		partial(spec(68, Sine, 690.0, 320.0, 0.55), 2.71, 0.5),
		NONE,
		NONE,
		0.3,
		0,
	),
	// 69 cabasa, 70 maracas / shaker: noise with a soft attack.
	shape(
		noise(spec(69, Sine, NONE, 1.0, 0.0), 7_000.0, 1.0, 90.0, 0.8),
		NONE,
		4.0,
		0.2,
		0,
	),
	shape(
		noise(spec(70, Sine, NONE, 1.0, 0.0), 6_000.0, 0.8, 70.0, 1.2),
		NONE,
		10.0,
		0.3,
		0,
	),
	// 73/74 short and long guiro: a gated noise train.
	scrape(
		shape(
			noise(spec(73, Sine, NONE, 1.0, 0.0), 2_800.0, 3.0, 400.0, 1.8),
			NONE,
			2.0,
			-0.3,
			2,
		),
		70.0,
		110.0,
	),
	scrape(
		shape(
			noise(spec(74, Sine, NONE, 1.0, 0.0), 2_800.0, 3.0, 900.0, 1.8),
			NONE,
			2.0,
			-0.3,
			2,
		),
		55.0,
		380.0,
	),
	// 75 claves: a clean, hard wooden tone.
	shape(
		partial(spec(75, Sine, 2_500.0, 60.0, 0.7), 2.1, 0.1),
		NONE,
		NONE,
		0.1,
		0,
	),
	// 76/77 woodblocks.
	shape(
		noise(
			partial(spec(76, Sine, 1_100.0, 70.0, 0.75), 2.4, 0.3),
			3_000.0,
			1.0,
			6.0,
			0.2,
		),
		NONE,
		NONE,
		0.2,
		0,
	),
	shape(
		noise(
			partial(spec(77, Sine, 800.0, 80.0, 0.75), 2.4, 0.3),
			2_500.0,
			1.0,
			6.0,
			0.2,
		),
		NONE,
		NONE,
		0.2,
		0,
	),
	// 78/79 cuica (mute/open): a quick upward squeak.
	shape(
		partial(spec(78, Sine, 650.0, 90.0, 0.5), 2.0, 0.2),
		-0.3,
		NONE,
		-0.2,
		0,
	),
	shape(
		partial(spec(79, Sine, 450.0, 160.0, 0.5), 2.0, 0.2),
		-0.4,
		NONE,
		-0.2,
		0,
	),
	// 80 mute triangle, 81 open triangle.
	shape(
		partial(spec(80, Sine, 4_200.0, 140.0, 0.3), 1.48, 0.5),
		NONE,
		NONE,
		0.4,
		3,
	),
	shape(
		partial(spec(81, Sine, 4_200.0, 1_500.0, 0.3), 1.48, 0.5),
		NONE,
		NONE,
		0.4,
		3,
	),
	// 71/72 whistles: a short and a long breathy tone.
	shape(
		noise(
			spec(71, Sine, 2_400.0, 160.0, 0.35),
			2_400.0,
			4.0,
			160.0,
			0.2,
		),
		NONE,
		15.0,
		0.0,
		4,
	),
];

fn find(note: u8) -> Option<Spec> {
	// The long whistle is the short one held longer.
	if note == 72 {
		let mut s = *SPECS.iter().find(|s| s.note == 71)?;
		s.note = 72;
		s.tone_ms = 450.0;
		s.noise_ms = 450.0;
		s.freq = 2_250.0;
		return Some(s);
	}
	SPECS.iter().find(|s| s.note == note).copied()
}

#[derive(Debug, Clone, Copy, Default)]
struct Voice {
	note: u8,
	age: u32,
	choke: u8,
	square: bool,
	phase1: f32,
	phase2: f32,
	f1: f32,
	f2: f32,
	partial: f32,
	bend: f32,
	tone: DecayEnv,
	bend_env: DecayEnv,
	noise: DecayEnv,
	tone_gain: f32,
	noise_gain: f32,
	attack: f32,
	attack_step: f32,
	scrape_phase: f32,
	scrape_inc: f32,
	scrape_left: u32,
	body_bp: Biquad,
	body_c: BiquadCoeffs,
	noise_bp: Biquad,
	noise_c: BiquadCoeffs,
	pan_l: f32,
	pan_r: f32,
}

impl Voice {
	fn active(&self) -> bool {
		self.tone.is_active() || self.noise.is_active()
	}
}

/// A small voice pool for the percussion notes (one voice per sounding note).
#[derive(Debug, Clone, Default)]
pub struct Percussion {
	voices: [Voice; 8],
	counter: u32,
}

/// Global percussion controls (tune in semitones, decay as a factor, linear level).
#[derive(Debug, Clone, Copy)]
pub struct PercParams {
	pub tune: f32,
	pub decay: f32,
	pub level: f32,
}

impl Percussion {
	pub fn plays(note: u8) -> bool {
		find(note).is_some()
	}

	/// Starts `note`; returns false when it is not a percussion note.
	pub fn trigger(&mut self, note: u8, gain: f32, p: PercParams, sr: f32) -> bool {
		let Some(s) = find(note) else {
			return false;
		};
		self.counter = self.counter.wrapping_add(1);
		if s.choke != 0 {
			for v in self
				.voices
				.iter_mut()
				.filter(|v| v.choke == s.choke && v.active())
			{
				v.tone.set_decay(6.0, sr);
				v.noise.set_decay(6.0, sr);
				v.scrape_left = 0;
			}
		}
		let idx = self
			.voices
			.iter()
			.position(|v| v.note == note && v.active())
			.or_else(|| self.voices.iter().position(|v| !v.active()))
			.unwrap_or_else(|| {
				let mut oldest = 0;
				for (i, v) in self.voices.iter().enumerate() {
					if self.counter.wrapping_sub(v.age)
						> self.counter.wrapping_sub(self.voices[oldest].age)
					{
						oldest = i;
					}
				}
				oldest
			});
		let v = &mut self.voices[idx];
		let tune = 2.0_f32.powf(p.tune / 12.0);
		let decay = p.decay.max(0.05);
		let fresh = !v.active();
		v.note = note;
		v.age = self.counter;
		v.choke = s.choke;
		v.square = s.body == Body::Square;
		if fresh {
			v.phase1 = 0.0;
			v.phase2 = 0.0;
			v.body_bp.reset();
			v.noise_bp.reset();
		}
		v.f1 = s.freq * tune / sr;
		v.f2 = s.freq * s.ratio * tune / sr;
		v.partial = s.partial;
		v.bend = s.bend;
		v.tone.trigger(1.0, s.tone_ms * decay, sr);
		if s.freq <= 0.0 || s.tone <= 0.0 {
			v.tone.reset();
		}
		v.bend_env.trigger(1.0, 40.0, sr);
		v.noise.trigger(1.0, s.noise_ms * decay, sr);
		if s.noise <= 0.0 {
			v.noise.reset();
		}
		let g = gain * p.level;
		v.tone_gain = g * s.tone;
		v.noise_gain = g * s.noise;
		v.attack_step = if s.attack_ms > 0.0 {
			1.0 / (s.attack_ms * 0.001 * sr).max(1.0)
		} else {
			1.0
		};
		v.attack = if s.attack_ms > 0.0 { 0.0 } else { 1.0 };
		v.scrape_inc = s.scrape_hz * tune / sr;
		v.scrape_phase = 0.0;
		v.scrape_left = (s.scrape_ms * decay * 0.001 * sr) as u32;
		let nyquist = sr * 0.45;
		v.body_c = BiquadCoeffs::bandpass((s.freq * tune * 1.6).min(nyquist), 1.2, sr);
		v.noise_c = BiquadCoeffs::bandpass((s.noise_freq * tune).min(nyquist), s.noise_q, sr);
		v.pan_l = (1.0 - s.pan).min(1.0);
		v.pan_r = (1.0 + s.pan).min(1.0);
		true
	}

	/// Fades every voice out quickly (all notes off).
	pub fn stop_all(&mut self, sr: f32) {
		for v in &mut self.voices {
			v.tone.set_decay(30.0, sr);
			v.noise.set_decay(30.0, sr);
			v.scrape_left = 0;
		}
	}

	pub fn reset(&mut self) {
		*self = Self::default();
	}

	/// One stereo sample of every sounding voice.
	#[inline]
	pub fn tick(&mut self, rng: &mut Rng, sr: f32) -> (f32, f32) {
		let mut l = 0.0;
		let mut r = 0.0;
		for v in &mut self.voices {
			if !v.active() {
				continue;
			}
			if v.attack < 1.0 {
				v.attack = (v.attack + v.attack_step).min(1.0);
			}
			let mut x = 0.0;
			if v.tone.is_active() {
				let pitch = 1.0 - v.bend * (1.0 - v.bend_env.tick());
				let body = if v.square {
					let sq = |ph: f32| if ph < 0.5 { 1.0 } else { -1.0 };
					let raw = sq(v.phase1) + v.partial * sq(v.phase2);
					v.body_bp.process(&v.body_c, raw) * 0.8
				} else {
					(TAU * v.phase1).sin() + v.partial * (TAU * v.phase2).sin()
				};
				v.phase1 = (v.phase1 + v.f1 * pitch).fract();
				v.phase2 = (v.phase2 + v.f2 * pitch).fract();
				x += body * v.tone.tick() * v.tone_gain;
			}
			if v.noise.is_active() {
				let mut gate = 1.0;
				if v.scrape_inc > 0.0 {
					if v.scrape_left > 0 {
						v.scrape_left -= 1;
						if v.scrape_left == 0 {
							v.noise.set_decay(8.0, sr);
						}
					}
					// Each tooth of the scrape: a quick swell then a dip.
					gate = 0.15 + 0.85 * (1.0 - v.scrape_phase) * (1.0 - v.scrape_phase);
					v.scrape_phase = (v.scrape_phase + v.scrape_inc).fract();
				}
				let n = v.noise_bp.process(&v.noise_c, rng.bipolar());
				x += n * v.noise.tick() * v.noise_gain * gate;
			}
			x *= v.attack;
			l += x * v.pan_l;
			r += x * v.pan_r;
		}
		(l, r)
	}
}
