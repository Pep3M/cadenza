//! `drum_machine`: synthesized, GM-mapped drum kit.

use super::percussion::{PercParams, Percussion};
use crate::processor::{DeviceInfo, DeviceRole, MidiEvent, ParamInfo, Processor, Transport};
use crate::util::biquad::{Biquad, BiquadCoeffs};
use crate::util::envelope::{DecayEnv, decay_coeff};
use crate::util::smoother::Smoother;
use crate::util::{ParamValues, Rng, TAU, db_to_gain, event_spans};

const P_KICK_TUNE: usize = 0;
const P_KICK_DECAY: usize = 1;
const P_KICK_PUNCH: usize = 2;
const P_KICK_LEVEL: usize = 3;
const P_SNARE_TUNE: usize = 4;
const P_SNARE_DECAY: usize = 5;
const P_SNARE_SNAPPY: usize = 6;
const P_SNARE_LEVEL: usize = 7;
const P_CLAP_LEVEL: usize = 8;
const P_HAT_TUNE: usize = 9;
const P_HAT_DECAY: usize = 10;
const P_OPEN_HAT_DECAY: usize = 11;
const P_HAT_LEVEL: usize = 12;
const P_TOM_TUNE: usize = 13;
const P_TOM_DECAY: usize = 14;
const P_TOM_LEVEL: usize = 15;
const P_CYMBAL_DECAY: usize = 16;
const P_CYMBAL_LEVEL: usize = 17;
const P_TONE: usize = 18;
const P_GAIN: usize = 19;
const P_PERC_TUNE: usize = 20;
const P_PERC_DECAY: usize = 21;
const P_PERC_LEVEL: usize = 22;
const N_PARAMS: usize = 23;

const TUNE_DESC: &str = "Pitch offset of the drum in semitones.";
const LEVEL_DESC: &str = "Level of this drum within the kit.";

static PARAMS: [ParamInfo; N_PARAMS] = [
	ParamInfo::new(
		"kick_tune",
		"Kick Tune",
		-12.0,
		12.0,
		0.0,
		"semitones",
		TUNE_DESC,
	),
	ParamInfo::new(
		"kick_decay",
		"Kick Decay",
		50.0,
		2_000.0,
		450.0,
		"ms",
		"Kick body length. 200-350 tight pop/house, 400-600 punchy, 800-2000 long 808 boom (hip-hop/trap).",
	),
	ParamInfo::new(
		"kick_punch",
		"Kick Punch",
		0.0,
		100.0,
		60.0,
		"%",
		"Pitch sweep depth and beater click. Higher = harder attack that cuts through dense mixes.",
	),
	ParamInfo::new(
		"kick_level",
		"Kick Level",
		-60.0,
		6.0,
		0.0,
		"dB",
		LEVEL_DESC,
	),
	ParamInfo::new(
		"snare_tune",
		"Snare Tune",
		-12.0,
		12.0,
		0.0,
		"semitones",
		TUNE_DESC,
	),
	ParamInfo::new(
		"snare_decay",
		"Snare Decay",
		50.0,
		1_000.0,
		220.0,
		"ms",
		"Length of the snare wires/noise. 120-200 tight, 250-400 roomy.",
	),
	ParamInfo::new(
		"snare_snappy",
		"Snare Snappy",
		0.0,
		100.0,
		60.0,
		"%",
		"Noise (wires) versus tonal body. Higher = brighter, crisper snare.",
	),
	ParamInfo::new(
		"snare_level",
		"Snare Level",
		-60.0,
		6.0,
		-2.0,
		"dB",
		LEVEL_DESC,
	),
	ParamInfo::new(
		"clap_level",
		"Clap Level",
		-60.0,
		6.0,
		-3.0,
		"dB",
		LEVEL_DESC,
	),
	ParamInfo::new(
		"hat_tune",
		"Hat Tune",
		-12.0,
		12.0,
		0.0,
		"semitones",
		TUNE_DESC,
	),
	ParamInfo::new(
		"hat_decay",
		"Closed Hat Decay",
		10.0,
		500.0,
		70.0,
		"ms",
		"Closed hi-hat length. 30-60 tight trap/house, 80-150 looser.",
	),
	ParamInfo::new(
		"open_hat_decay",
		"Open Hat Decay",
		50.0,
		2_000.0,
		450.0,
		"ms",
		"Open hi-hat length (a closed hat always chokes it).",
	),
	ParamInfo::new(
		"hat_level",
		"Hat Level",
		-60.0,
		6.0,
		-6.0,
		"dB",
		"Level of closed and open hats.",
	),
	ParamInfo::new(
		"tom_tune",
		"Tom Tune",
		-12.0,
		12.0,
		0.0,
		"semitones",
		TUNE_DESC,
	),
	ParamInfo::new(
		"tom_decay",
		"Tom Decay",
		50.0,
		2_000.0,
		400.0,
		"ms",
		"Tom ring length.",
	),
	ParamInfo::new("tom_level", "Tom Level", -60.0, 6.0, -4.0, "dB", LEVEL_DESC),
	ParamInfo::new(
		"cymbal_decay",
		"Cymbal Decay",
		200.0,
		6_000.0,
		1_800.0,
		"ms",
		"Crash length (ride is a bit shorter).",
	),
	ParamInfo::new(
		"cymbal_level",
		"Cymbal Level",
		-60.0,
		6.0,
		-9.0,
		"dB",
		"Level of crash and ride.",
	),
	ParamInfo::new(
		"tone",
		"Tone",
		0.0,
		100.0,
		50.0,
		"%",
		"Overall brightness of the kit: below 50 darker/lo-fi, above 50 brighter/crisper.",
	),
	ParamInfo::new(
		"gain",
		"Gain",
		-60.0,
		12.0,
		0.0,
		"dB",
		"Output level of the whole kit.",
	),
	ParamInfo::new(
		"perc_tune",
		"Perc Tune",
		-12.0,
		12.0,
		0.0,
		"semitones",
		"Pitch offset of the hand percussion (rim, cowbell, bongos, congas, timbales, agogo, \
		 guiro, claves, woodblocks, triangle).",
	),
	ParamInfo::new(
		"perc_decay",
		"Perc Decay",
		25.0,
		400.0,
		100.0,
		"%",
		"Length of the hand percussion relative to its natural decay.",
	),
	ParamInfo::new(
		"perc_level",
		"Perc Level",
		-60.0,
		6.0,
		-3.0,
		"dB",
		"Level of the hand percussion and shakers.",
	),
];

pub static INFO: DeviceInfo = DeviceInfo {
	id: "drum_machine",
	name: "Drum Machine",
	role: DeviceRole::Instrument,
	description: "Synthesized drum kit on General MIDI notes: 36 (C1) kick, 38 snare, 39 clap, \
		42 closed hat, 44 pedal hat, 46 open hat (choked by 42/44), 41/43 low tom, 45/47 mid tom, \
		48/50 high tom, 49/57 crash, 51/59 ride, 52 china, 53 ride bell, 55 splash; 35 and 40 \
		alias kick and snare. Latin/hand percussion: 37 rim (side stick), 54 tambourine, 56 \
		cowbell, 60/61 hi/low bongo, 62 mute conga, 63 open conga, 64 low conga, 65/66 \
		timbales, 67/68 agogo, 69 cabasa, 70 shaker/maracas, 71/72 whistle, 73/74 short/long \
		guiro, 75 claves, 76/77 woodblocks, 78/79 cuica, 80/81 mute/open triangle. Note length does \
		not matter (one-shots); velocity controls loudness, so vary hat velocities (0.5-0.9) for \
		groove. Punchy defaults suit pop, hip-hop, house and electronic. Recipes — TRAP/808: \
		kick_decay 1200, kick_tune -3, hat_decay 35. HOUSE: kick_decay 350, kick_punch 70, \
		open_hat_decay 250. LO-FI: tone 25, snare_snappy 40. ROCK-ish: kick_decay 300, snare_decay \
		300, tom_decay 600.",
	params: &PARAMS,
};

/// Whether the drum machine has a sound for this GM note.
pub fn plays(note: u8) -> bool {
	matches!(note, 35..=36 | 38..=53 | 55 | 57 | 59) || Percussion::plays(note)
}

const METAL_RATIOS: [f32; 6] = [205.3, 304.4, 369.6, 522.7, 540.0, 800.0];

/// Six detuned square oscillators: the classic analog cymbal/hat source.
#[derive(Debug, Clone, Copy, Default)]
struct Metal {
	phases: [f32; 6],
}

impl Metal {
	#[inline]
	fn next(&mut self, freq_scale: f32) -> f32 {
		let mut s = 0.0;
		for (ph, ratio) in self.phases.iter_mut().zip(METAL_RATIOS) {
			s += if *ph < 0.5 { 1.0 } else { -1.0 };
			*ph += ratio * freq_scale;
			if *ph >= 1.0 {
				*ph -= 1.0;
			}
		}
		s * (1.0 / 6.0)
	}
}

#[derive(Debug, Clone, Copy, Default)]
struct Kick {
	phase: f32,
	base: f32,
	sweep: f32,
	amp: DecayEnv,
	pitch: DecayEnv,
	click: DecayEnv,
	click_filter: Biquad,
	click_coeffs: BiquadCoeffs,
	gain: f32,
}

#[derive(Debug, Clone, Copy, Default)]
struct Snare {
	phase1: f32,
	phase2: f32,
	f1: f32,
	f2: f32,
	tone: DecayEnv,
	pitch: DecayEnv,
	noise: DecayEnv,
	hp: Biquad,
	bp: Biquad,
	hp_c: BiquadCoeffs,
	bp_c: BiquadCoeffs,
	tone_gain: f32,
	noise_gain: f32,
}

#[derive(Debug, Clone, Copy, Default)]
struct Clap {
	t: u32,
	burst_len: u32,
	burst: f32,
	burst_coeff: f32,
	tail: DecayEnv,
	bp: Biquad,
	c: BiquadCoeffs,
	gain: f32,
	active: bool,
}

/// Hats and cymbals share this metallic voice with different filtering.
#[derive(Debug, Clone, Copy, Default)]
struct MetalVoice {
	metal: Metal,
	freq_scale: f32,
	env: DecayEnv,
	bp: Biquad,
	hp: Biquad,
	bp_c: BiquadCoeffs,
	hp_c: BiquadCoeffs,
	noise_mix: f32,
	gain: f32,
	pan_l: f32,
	pan_r: f32,
}

#[derive(Debug, Clone, Copy, Default)]
struct Tom {
	phase: f32,
	base: f32,
	amp: DecayEnv,
	pitch: DecayEnv,
	gain: f32,
	pan_l: f32,
	pan_r: f32,
}

pub struct DrumMachine {
	params: ParamValues<N_PARAMS>,
	sample_rate: f32,
	rng: Rng,
	kick: Kick,
	snare: Snare,
	clap: Clap,
	closed_hat: MetalVoice,
	open_hat: MetalVoice,
	crash: MetalVoice,
	ride: MetalVoice,
	toms: [Tom; 3],
	perc: Percussion,
	tone_db: Smoother,
	gain: Smoother,
	tone_filters: [Biquad; 2],
	tone_coeffs: BiquadCoeffs,
	tone_coeffs_db: f32,
	tone_counter: u32,
}

impl Default for DrumMachine {
	fn default() -> Self {
		Self::new()
	}
}

#[inline]
fn semis(s: f32) -> f32 {
	2.0_f32.powf(s / 12.0)
}

#[inline]
fn velocity_gain(velocity: f32) -> f32 {
	let v = velocity.clamp(0.0, 1.0);
	0.15 + 0.85 * v * v.sqrt()
}

impl DrumMachine {
	pub fn new() -> Self {
		let params = ParamValues::new(&INFO);
		Self {
			sample_rate: 48_000.0,
			rng: Rng::new(0xD2D2_0001),
			kick: Kick::default(),
			snare: Snare::default(),
			clap: Clap::default(),
			closed_hat: MetalVoice::default(),
			open_hat: MetalVoice::default(),
			crash: MetalVoice::default(),
			ride: MetalVoice::default(),
			toms: [Tom::default(); 3],
			perc: Percussion::default(),
			tone_db: Smoother::new(tone_to_db(params.get(P_TONE))),
			gain: Smoother::new(db_to_gain(params.get(P_GAIN))),
			tone_filters: [Biquad::default(); 2],
			tone_coeffs: BiquadCoeffs::IDENTITY,
			tone_coeffs_db: f32::NAN,
			tone_counter: 0,
			params,
		}
	}

	fn trigger(&mut self, pitch: u8, velocity: f32) {
		let sr = self.sample_rate;
		let vel = velocity_gain(velocity);
		let p = &self.params;
		match pitch {
			35 | 36 => {
				let k = &mut self.kick;
				if !k.amp.is_active() {
					k.phase = 0.0;
				}
				let punch = p.get(P_KICK_PUNCH) * 0.01;
				k.base = 50.0 * semis(p.get(P_KICK_TUNE));
				k.sweep = 1.0 + 5.0 * punch;
				k.amp.trigger(1.0, p.get(P_KICK_DECAY), sr);
				k.pitch.trigger(1.0, 35.0 + 25.0 * (1.0 - punch), sr);
				k.click.trigger(0.6 * punch, 6.0, sr);
				k.click_coeffs = BiquadCoeffs::bandpass(3_500.0, 0.8, sr);
				k.gain = vel * db_to_gain(p.get(P_KICK_LEVEL)) * 0.7;
			}
			38 | 40 => {
				let s = &mut self.snare;
				let tune = semis(p.get(P_SNARE_TUNE));
				let snappy = p.get(P_SNARE_SNAPPY) * 0.01;
				let decay = p.get(P_SNARE_DECAY);
				s.f1 = 185.0 * tune;
				s.f2 = 330.0 * tune;
				s.tone.trigger(1.0, decay * 0.55, sr);
				s.pitch.trigger(1.0, 30.0, sr);
				s.noise.trigger(1.0, decay, sr);
				s.hp_c = BiquadCoeffs::highpass(1_200.0 * tune, 0.7, sr);
				s.bp_c = BiquadCoeffs::peaking(5_000.0, 0.8, 6.0, sr);
				let g = vel * db_to_gain(p.get(P_SNARE_LEVEL)) * 0.5;
				s.tone_gain = g * (1.0 - 0.6 * snappy);
				s.noise_gain = g * (0.3 + 0.9 * snappy);
			}
			39 => {
				let c = &mut self.clap;
				c.t = 0;
				c.burst_len = (0.011 * sr) as u32;
				c.burst = 1.0;
				c.burst_coeff = decay_coeff(9.0, sr);
				c.tail.reset();
				c.c = BiquadCoeffs::bandpass(1_150.0, 1.4, sr);
				c.gain = vel * db_to_gain(p.get(P_CLAP_LEVEL)) * 2.0;
				c.active = true;
			}
			42 | 44 => {
				let tune = semis(p.get(P_HAT_TUNE));
				let level = db_to_gain(p.get(P_HAT_LEVEL));
				let decay = p.get(P_HAT_DECAY);
				Self::trigger_metal(
					&mut self.closed_hat,
					sr,
					tune * 1.6,
					decay,
					[9_500.0, 7_000.0],
					0.35,
					vel * level * 1.1,
					0.2,
				);
				self.open_hat.env.set_decay(6.0, sr);
			}
			46 => {
				let tune = semis(p.get(P_HAT_TUNE));
				let level = db_to_gain(p.get(P_HAT_LEVEL));
				let decay = p.get(P_OPEN_HAT_DECAY);
				Self::trigger_metal(
					&mut self.open_hat,
					sr,
					tune * 1.6,
					decay,
					[9_000.0, 6_500.0],
					0.35,
					vel * level * 0.8,
					0.2,
				);
			}
			49 | 57 => {
				let level = db_to_gain(p.get(P_CYMBAL_LEVEL));
				let decay = p.get(P_CYMBAL_DECAY);
				Self::trigger_metal(
					&mut self.crash,
					sr,
					1.25,
					decay,
					[6_500.0, 3_500.0],
					0.55,
					vel * level * 1.0,
					-0.35,
				);
			}
			52 => {
				let level = db_to_gain(p.get(P_CYMBAL_LEVEL));
				let decay = p.get(P_CYMBAL_DECAY) * 0.7;
				Self::trigger_metal(
					&mut self.crash,
					sr,
					1.05,
					decay,
					[4_500.0, 2_500.0],
					0.7,
					vel * level * 1.1,
					-0.45,
				);
			}
			55 => {
				let level = db_to_gain(p.get(P_CYMBAL_LEVEL));
				let decay = p.get(P_CYMBAL_DECAY) * 0.35;
				Self::trigger_metal(
					&mut self.crash,
					sr,
					1.6,
					decay,
					[8_000.0, 4_500.0],
					0.5,
					vel * level * 0.9,
					-0.2,
				);
			}
			53 => {
				let level = db_to_gain(p.get(P_CYMBAL_LEVEL));
				let decay = p.get(P_CYMBAL_DECAY) * 0.6;
				Self::trigger_metal(
					&mut self.ride,
					sr,
					2.4,
					decay,
					[3_500.0, 1_800.0],
					0.05,
					vel * level * 1.4,
					0.35,
				);
			}
			51 | 59 => {
				let level = db_to_gain(p.get(P_CYMBAL_LEVEL));
				let decay = p.get(P_CYMBAL_DECAY) * 0.8;
				Self::trigger_metal(
					&mut self.ride,
					sr,
					1.9,
					decay,
					[5_200.0, 2_800.0],
					0.15,
					vel * level * 1.3,
					0.35,
				);
			}
			41 | 43 | 45 | 47 | 48 | 50 => {
				let (idx, base, pan): (usize, f32, f32) = match pitch {
					41 | 43 => (0, 92.0, -0.3),
					45 | 47 => (1, 125.0, 0.0),
					_ => (2, 165.0, 0.3),
				};
				let t = &mut self.toms[idx];
				t.base = base * semis(p.get(P_TOM_TUNE));
				t.amp.trigger(1.0, p.get(P_TOM_DECAY), sr);
				t.pitch.trigger(1.0, 120.0, sr);
				t.gain = vel * db_to_gain(p.get(P_TOM_LEVEL)) * 0.8;
				t.pan_l = (1.0 - pan).min(1.0);
				t.pan_r = (1.0 + pan).min(1.0);
			}
			_ => {
				let perc = PercParams {
					tune: p.get(P_PERC_TUNE),
					decay: p.get(P_PERC_DECAY) * 0.01,
					level: db_to_gain(p.get(P_PERC_LEVEL)),
				};
				self.perc.trigger(pitch, vel, perc, sr);
			}
		}
	}

	#[allow(clippy::too_many_arguments)]
	fn trigger_metal(
		v: &mut MetalVoice,
		sr: f32,
		tune: f32,
		decay: f32,
		[bp, hp]: [f32; 2],
		noise_mix: f32,
		gain: f32,
		pan: f32,
	) {
		v.freq_scale = tune / sr;
		v.env.trigger(1.0, decay, sr);
		v.bp_c = BiquadCoeffs::bandpass(bp, 0.9, sr);
		v.hp_c = BiquadCoeffs::highpass(hp, 0.7, sr);
		v.noise_mix = noise_mix;
		v.gain = gain * 2.2;
		v.pan_l = (1.0 - pan).min(1.0);
		v.pan_r = (1.0 + pan).min(1.0);
	}

	fn stop_all(&mut self) {
		let sr = self.sample_rate;
		for env in [
			&mut self.kick.amp,
			&mut self.snare.tone,
			&mut self.snare.noise,
			&mut self.clap.tail,
			&mut self.closed_hat.env,
			&mut self.open_hat.env,
			&mut self.crash.env,
			&mut self.ride.env,
		] {
			env.set_decay(30.0, sr);
		}
		for t in &mut self.toms {
			t.amp.set_decay(30.0, sr);
		}
		self.clap.burst = 0.0;
		self.perc.stop_all(sr);
	}

	#[inline]
	fn metal_sample(v: &mut MetalVoice, rng: &mut Rng) -> f32 {
		if !v.env.is_active() {
			return 0.0;
		}
		let m = v.metal.next(v.freq_scale);
		let src = m + (rng.bipolar() - m) * v.noise_mix;
		let y = v.hp.process(&v.hp_c, v.bp.process(&v.bp_c, src));
		y * v.env.tick() * v.gain
	}

	fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
		let sr = self.sample_rate;
		let inv_sr = 1.0 / sr;
		for (l, r) in left.iter_mut().zip(right.iter_mut()) {
			let mut mono = 0.0;
			let mut sl = 0.0;
			let mut sr_ = 0.0;

			let k = &mut self.kick;
			if k.amp.is_active() {
				let pe = k.pitch.tick();
				let freq = k.base * (1.0 + (k.sweep - 1.0) * pe * pe);
				let body = (TAU * k.phase).sin();
				k.phase = (k.phase + freq * inv_sr).fract();
				let click =
					k.click_filter.process(&k.click_coeffs, self.rng.bipolar()) * k.click.tick();
				let x = (body * k.amp.tick() + click) * 1.4;
				mono += x.tanh() * k.gain;
			}

			let s = &mut self.snare;
			if s.noise.is_active() || s.tone.is_active() {
				let pe = 1.0 + 0.5 * s.pitch.tick();
				let tone = ((TAU * s.phase1).sin() + 0.6 * (TAU * s.phase2).sin())
					* s.tone.tick() * s.tone_gain;
				s.phase1 = (s.phase1 + s.f1 * pe * inv_sr).fract();
				s.phase2 = (s.phase2 + s.f2 * pe * inv_sr).fract();
				let n =
					s.bp.process(&s.bp_c, s.hp.process(&s.hp_c, self.rng.bipolar()));
				mono += tone + n * s.noise.tick() * s.noise_gain;
			}

			let c = &mut self.clap;
			if c.active {
				let mut env = c.tail.tick() * 0.8;
				if c.t < c.burst_len * 3 {
					if c.t > 0 && c.t.is_multiple_of(c.burst_len) {
						c.burst = 1.0;
					}
					env += c.burst;
					c.burst *= c.burst_coeff;
				}
				if c.t == c.burst_len * 2 {
					c.tail.trigger(1.0, 220.0, sr);
				}
				c.t = c.t.saturating_add(1);
				let n = c.bp.process(&c.c, self.rng.bipolar());
				mono += n * env * c.gain;
				if c.t > c.burst_len * 3 && !c.tail.is_active() {
					c.active = false;
				}
			}

			for v in [
				&mut self.closed_hat,
				&mut self.open_hat,
				&mut self.crash,
				&mut self.ride,
			] {
				let x = Self::metal_sample(v, &mut self.rng);
				sl += x * v.pan_l;
				sr_ += x * v.pan_r;
			}

			for t in &mut self.toms {
				if t.amp.is_active() {
					let pe = t.pitch.tick();
					let x = (TAU * t.phase).sin() * t.amp.tick() * t.gain;
					t.phase = (t.phase + t.base * (1.0 + 0.6 * pe) * inv_sr).fract();
					sl += x * t.pan_l;
					sr_ += x * t.pan_r;
				}
			}

			let (pl, pr) = self.perc.tick(&mut self.rng, sr);
			sl += pl;
			sr_ += pr;

			let tone_db = self.tone_db.tick();
			if self.tone_counter == 0 && tone_db != self.tone_coeffs_db {
				self.tone_coeffs = BiquadCoeffs::high_shelf(3_000.0, tone_db, sr);
				self.tone_coeffs_db = tone_db;
			}
			self.tone_counter = (self.tone_counter + 1) % 16;
			let g = self.gain.tick();
			let [tl, tr] = &mut self.tone_filters;
			*l = tl.process(&self.tone_coeffs, mono + sl) * g;
			*r = tr.process(&self.tone_coeffs, mono + sr_) * g;
		}
	}
}

fn tone_to_db(tone: f32) -> f32 {
	(tone - 50.0) / 50.0 * 9.0
}

impl Processor for DrumMachine {
	fn info(&self) -> &'static DeviceInfo {
		&INFO
	}

	fn prepare(&mut self, sample_rate: f32, _max_block: usize) {
		self.sample_rate = sample_rate.max(1.0);
		self.tone_db.prepare(self.sample_rate, 20.0);
		self.gain.prepare(self.sample_rate, 15.0);
		self.tone_coeffs_db = f32::NAN;
		self.reset();
	}

	fn set_param(&mut self, index: usize, value: f32) {
		let Some(v) = self.params.set(&INFO, index, value) else {
			return;
		};
		match index {
			P_TONE => self.tone_db.set_target(tone_to_db(v)),
			P_GAIN => self.gain.set_target(db_to_gain(v)),
			_ => {}
		}
	}

	fn process(
		&mut self,
		left: &mut [f32],
		right: &mut [f32],
		events: &[MidiEvent],
		_transport: &Transport,
	) {
		let len = left.len().min(right.len());
		for (start, end, ev) in event_spans(len, events) {
			self.render(&mut left[start..end], &mut right[start..end]);
			match ev {
				Some(&MidiEvent::NoteOn {
					pitch, velocity, ..
				}) if velocity > 0.0 => self.trigger(pitch, velocity),
				Some(MidiEvent::AllNotesOff { .. }) => self.stop_all(),
				_ => {}
			}
		}
	}

	fn reset(&mut self) {
		self.kick = Kick::default();
		self.snare = Snare::default();
		self.clap = Clap::default();
		self.closed_hat = MetalVoice::default();
		self.open_hat = MetalVoice::default();
		self.crash = MetalVoice::default();
		self.ride = MetalVoice::default();
		self.toms = [Tom::default(); 3];
		self.perc.reset();
		self.tone_filters = [Biquad::default(); 2];
		self.tone_db.snap();
		self.gain.snap();
		self.tone_counter = 0;
	}

	fn tail_seconds(&self) -> f32 {
		let p = &self.params;
		let longest = p
			.get(P_KICK_DECAY)
			.max(p.get(P_OPEN_HAT_DECAY))
			.max(p.get(P_TOM_DECAY))
			.max(p.get(P_CYMBAL_DECAY))
			.max(p.get(P_SNARE_DECAY))
			.max(1_500.0 * p.get(P_PERC_DECAY) * 0.01)
			.max(250.0);
		longest * 0.001 * 1.3
	}
}
