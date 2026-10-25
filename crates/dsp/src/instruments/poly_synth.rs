//! `poly_synth`: 16-voice subtractive synthesizer.

use super::voice_readings::{self, HeldNotes, NewestVoices};
use super::{VoiceSlot, pick_voice};
use crate::processor::{DeviceInfo, DeviceRole, MidiEvent, ParamInfo, Processor, Transport};
use crate::util::envelope::Adsr;
use crate::util::expression::{Expression, ExpressionAction};
use crate::util::oscillator::{Oscillator, Waveform};
use crate::util::smoother::Smoother;
use crate::util::svf::{Svf, SvfCoeffs};
use crate::util::wavetable::{self, N_TABLES, Table, WavetableOsc, Wavetables};
use crate::util::{ParamValues, Rng, db_to_gain, event_spans, midi_to_hz, one_pole_coeff};

const VOICES: usize = 16;
const MAX_UNISON: usize = 4;
const CHUNK: usize = 64;
/// Filter coefficients are recomputed every this many samples per voice.
const CONTROL_RATE: usize = 8;
/// Per-voice headroom so a few simultaneous notes stay below 0 dBFS.
const VOICE_GAIN: f32 = 0.4;

const P_OSC1_WAVE: usize = 0;
const P_OSC2_WAVE: usize = 1;
const P_OSC2_SEMI: usize = 2;
const P_OSC2_CENTS: usize = 3;
const P_OSC_MIX: usize = 4;
const P_UNISON: usize = 5;
const P_UNISON_DETUNE: usize = 6;
const P_NOISE: usize = 7;
const P_CUTOFF: usize = 8;
const P_RESONANCE: usize = 9;
const P_SLOPE: usize = 10;
const P_ENV_AMOUNT: usize = 11;
const P_KEY_TRACK: usize = 12;
const P_AMP_A: usize = 13;
const P_AMP_D: usize = 14;
const P_AMP_S: usize = 15;
const P_AMP_R: usize = 16;
const P_FLT_A: usize = 17;
const P_FLT_D: usize = 18;
const P_FLT_S: usize = 19;
const P_FLT_R: usize = 20;
const P_VEL_SENS: usize = 21;
const P_GLIDE: usize = 22;
const P_GAIN: usize = 23;
const P_OSC1_TABLE: usize = 24;
const P_OSC1_POS: usize = 25;
const P_OSC2_TABLE: usize = 26;
const P_OSC2_POS: usize = 27;
const N_PARAMS: usize = 28;

/// `oscN_wave` value of the wavetable oscillator.
pub const WAVE_WAVETABLE: usize = 4;

const WAVE_DESC: &str = "0 = sine, 1 = saw, 2 = square, 3 = triangle, 4 = wavetable (morphs through \
	the built-in table picked by oscN_table at oscN_position).";
const TABLE_DESC: &str = "Built-in wavetable used when the wave is 4 (wavetable). 0 = Basic \
	(sine -> triangle -> saw -> square), 1 = PWM (square -> narrow pulse), 2 = Formant (vowels \
	a-e-i-o-u), 3 = Harmonics (a bright partial sweeping up), 4 = Digital (hard-synced, \
	bit-reduced saw).";
const POS_DESC: &str = "Morph position through the wavetable (0 = first frame, 1 = last). \
	Automate it or modulate it with an LFO/envelope for evolving pads and talking basses.";

static PARAMS: [ParamInfo; N_PARAMS] = [
	ParamInfo::new("osc1_wave", "Osc 1 Wave", 0.0, 4.0, 1.0, "", WAVE_DESC),
	ParamInfo::new("osc2_wave", "Osc 2 Wave", 0.0, 4.0, 1.0, "", WAVE_DESC),
	ParamInfo::new(
		"osc2_semitones",
		"Osc 2 Semitones",
		-24.0,
		24.0,
		0.0,
		"semitones",
		"Coarse tuning of oscillator 2. 12 or -12 thickens basses/leads; 7 gives a fifth.",
	),
	ParamInfo::new(
		"osc2_cents",
		"Osc 2 Detune",
		-100.0,
		100.0,
		7.0,
		"cents",
		"Fine detune of oscillator 2. 5-12 cents gives a classic fat analog beating.",
	),
	ParamInfo::new(
		"osc_mix",
		"Osc Mix",
		0.0,
		100.0,
		50.0,
		"%",
		"Balance between oscillator 1 (0%) and oscillator 2 (100%).",
	),
	ParamInfo::new(
		"unison",
		"Unison Voices",
		1.0,
		4.0,
		1.0,
		"",
		"Stacked, detuned and stereo-spread copies per note (1-4). 3-4 for supersaw pads/leads.",
	),
	ParamInfo::new(
		"unison_detune",
		"Unison Detune",
		0.0,
		100.0,
		20.0,
		"cents",
		"Detune spread between unison copies. 10-30 subtle, 40+ trance supersaw.",
	),
	ParamInfo::new(
		"noise",
		"Noise",
		0.0,
		100.0,
		0.0,
		"%",
		"White noise level before the filter. Small amounts add breath to pads and attack to plucks.",
	),
	ParamInfo::new(
		"filter_cutoff",
		"Filter Cutoff",
		20.0,
		20_000.0,
		2_500.0,
		"Hz",
		"Low-pass cutoff before envelope/key tracking. Bass 200-800, pads 800-3000, leads 2000-8000.",
	),
	ParamInfo::new(
		"filter_resonance",
		"Filter Resonance",
		0.0,
		100.0,
		15.0,
		"%",
		"Emphasis at the cutoff. 0-30 smooth, 50-80 squelchy/acid, near 100 whistles.",
	),
	ParamInfo::new(
		"filter_slope",
		"Filter Slope",
		0.0,
		1.0,
		1.0,
		"",
		"0 = 12 dB/oct (brighter, softer), 1 = 24 dB/oct (classic, darker).",
	),
	ParamInfo::new(
		"filter_env_amount",
		"Filter Env Amount",
		-60.0,
		60.0,
		24.0,
		"semitones",
		"How far the filter envelope opens the cutoff. Plucks 36-60, pads 0-12, basses 12-36.",
	),
	ParamInfo::new(
		"filter_key_track",
		"Filter Key Tracking",
		0.0,
		100.0,
		50.0,
		"%",
		"How much the cutoff follows the played note (100% = follows pitch exactly).",
	),
	ParamInfo::new(
		"amp_attack",
		"Amp Attack",
		0.0,
		10_000.0,
		5.0,
		"ms",
		"Fade-in time. 0-5 for plucks/bass, 200-2000 for pads.",
	),
	ParamInfo::new(
		"amp_decay",
		"Amp Decay",
		1.0,
		10_000.0,
		400.0,
		"ms",
		"Time to fall from peak to sustain level.",
	),
	ParamInfo::new(
		"amp_sustain",
		"Amp Sustain",
		0.0,
		100.0,
		70.0,
		"%",
		"Level held while the key is down. 0 for plucks, 70-100 for pads/leads/bass.",
	),
	ParamInfo::new(
		"amp_release",
		"Amp Release",
		1.0,
		20_000.0,
		250.0,
		"ms",
		"Fade-out after key release. 50-150 bass, 200-500 leads, 1000-4000 pads.",
	),
	ParamInfo::new(
		"filter_attack",
		"Filter Attack",
		0.0,
		10_000.0,
		2.0,
		"ms",
		"Filter envelope attack.",
	),
	ParamInfo::new(
		"filter_decay",
		"Filter Decay",
		1.0,
		10_000.0,
		500.0,
		"ms",
		"Filter envelope decay. 100-300 for plucks, 1000+ for slow sweeps.",
	),
	ParamInfo::new(
		"filter_sustain",
		"Filter Sustain",
		0.0,
		100.0,
		30.0,
		"%",
		"Filter envelope sustain level.",
	),
	ParamInfo::new(
		"filter_release",
		"Filter Release",
		1.0,
		20_000.0,
		300.0,
		"ms",
		"Filter envelope release.",
	),
	ParamInfo::new(
		"velocity_sensitivity",
		"Velocity Sensitivity",
		0.0,
		100.0,
		60.0,
		"%",
		"How much velocity affects loudness and filter envelope depth.",
	),
	ParamInfo::new(
		"glide",
		"Glide",
		0.0,
		2_000.0,
		0.0,
		"ms",
		"Portamento from the previous note's pitch. 30-150 for leads and 303-style basses.",
	),
	ParamInfo::new(
		"gain",
		"Gain",
		-60.0,
		12.0,
		0.0,
		"dB",
		"Output level of the instrument.",
	),
	ParamInfo::new("osc1_table", "Osc 1 Table", 0.0, 4.0, 0.0, "", TABLE_DESC),
	ParamInfo::new(
		"osc1_position",
		"Osc 1 Position",
		0.0,
		1.0,
		0.0,
		"",
		POS_DESC,
	),
	ParamInfo::new("osc2_table", "Osc 2 Table", 0.0, 4.0, 0.0, "", TABLE_DESC),
	ParamInfo::new(
		"osc2_position",
		"Osc 2 Position",
		0.0,
		1.0,
		0.0,
		"",
		POS_DESC,
	),
];

pub static INFO: DeviceInfo = DeviceInfo {
	id: "poly_synth",
	name: "Poly Synth",
	role: DeviceRole::Instrument,
	description: "16-voice subtractive (virtual analog) synthesizer: 2 band-limited oscillators \
		(sine/saw/square/triangle) or morphing wavetables (wave 4: Basic, PWM, Formant, Harmonics, \
		Digital tables; oscN_position sweeps the table, great to automate or modulate) with unison, noise, 12/24 dB resonant low-pass filter with its \
		own ADSR, amp ADSR and glide. The general-purpose melodic instrument for pads, plucks, \
		basses, leads and chords in pop, EDM, synthwave, hip-hop and ambient. Recipes — \
		WARM PAD: saw+saw, osc2_cents 8, unison 3, cutoff 1200, env 6, amp attack 600 / release 2500, \
		sustain 90. PLUCK: saw, cutoff 400, env amount 48, filter decay 180, filter sustain 0, \
		amp decay 350, amp sustain 0, release 200. SUB/DEEP BASS: osc1 sine, osc2 square -12 semitones, \
		mix 30, cutoff 300, env 12, attack 2, release 80. ACID/EDM BASS: saw, slope 1, cutoff 300, \
		resonance 70, env 40, filter decay 200, glide 60. LEAD: square+saw, osc2 12 semitones, cutoff \
		4000, resonance 20, glide 40, release 300. SUPERSAW: saw+saw, unison 4, unison_detune 35, \
		cutoff 6000. WAVETABLE PAD: osc1_wave 4, osc1_table 2 (Formant), LFO on osc1_position.",
	params: &PARAMS,
};

/// Static magnitude response of the filter stage alone (no envelope, key
/// tracking or pressure, which modulate `cutoff_hz` live), in dB. Uses the
/// same [`SvfCoeffs::new`] and cascade/level-compensation the voice applies
/// (`res_comp`, `coeffs_flat` in the 24 dB/oct mode) so the picture matches
/// the sound exactly.
pub fn poly_synth_filter_response_db(
	cutoff_hz: f32,
	resonance: f32,
	slope_24: bool,
	sample_rate: f32,
	freq_hz: f32,
) -> f32 {
	let res = (resonance * 0.01).clamp(0.0, 1.0);
	let coeffs = SvfCoeffs::new(cutoff_hz, res, sample_rate);
	let mut db = crate::util::svf::svf_magnitude_db(
		&coeffs,
		crate::util::svf::SvfOutput::Low,
		freq_hz,
		sample_rate,
	);
	if slope_24 {
		let flat = SvfCoeffs::new(cutoff_hz, 0.3, sample_rate);
		db += crate::util::svf::svf_magnitude_db(
			&flat,
			crate::util::svf::SvfOutput::Low,
			freq_hz,
			sample_rate,
		);
	}
	// Resonance thins the low end; the voice trims its input to keep loudness
	// even (`res_comp` in `process`).
	db + crate::util::gain_to_db(1.0 - 0.45 * res)
}

#[derive(Debug, Clone, Copy, Default)]
struct Voice {
	slot: VoiceSlot,
	osc1: [Oscillator; MAX_UNISON],
	osc2: [Oscillator; MAX_UNISON],
	wt1: [WavetableOsc; MAX_UNISON],
	wt2: [WavetableOsc; MAX_UNISON],
	/// Mipmap levels of the wavetable oscillators, updated at control rate.
	lvl1: [usize; MAX_UNISON],
	lvl2: [usize; MAX_UNISON],
	filters: [[Svf; 2]; 2],
	coeffs: SvfCoeffs,
	/// Non-resonant first stage of the 24 dB mode, so resonance peaks only once.
	coeffs_flat: SvfCoeffs,
	res_comp: f32,
	amp_env: Adsr,
	filter_env: Adsr,
	pitch: f32,
	target_pitch: f32,
	velocity: f32,
	control_counter: usize,
}

pub struct PolySynth {
	params: ParamValues<N_PARAMS>,
	sample_rate: f32,
	voices: [Voice; VOICES],
	note_counter: u64,
	last_pitch: Option<f32>,
	expr: Expression,
	rng: Rng,
	envelopes_dirty: bool,
	held: HeldNotes,
	cutoff_oct: Smoother,
	resonance: Smoother,
	mix: Smoother,
	noise: Smoother,
	gain: Smoother,
	pos1: Smoother,
	pos2: Smoother,
	tables: &'static Wavetables,
	// Per-chunk scratch for smoothed parameters shared by all voices.
	s_cutoff: [f32; CHUNK],
	s_res: [f32; CHUNK],
	s_mix: [f32; CHUNK],
	s_noise: [f32; CHUNK],
	s_pos1: [f32; CHUNK],
	s_pos2: [f32; CHUNK],
}

impl Default for PolySynth {
	fn default() -> Self {
		Self::new()
	}
}

impl PolySynth {
	pub fn new() -> Self {
		let params = ParamValues::new(&INFO);
		let mut s = Self {
			sample_rate: 48_000.0,
			voices: [Voice::default(); VOICES],
			note_counter: 0,
			last_pitch: None,
			expr: Expression::default(),
			rng: Rng::new(0x5EED_1234),
			envelopes_dirty: true,
			held: HeldNotes::default(),
			cutoff_oct: Smoother::new(params.get(P_CUTOFF).log2()),
			resonance: Smoother::new(params.get(P_RESONANCE) * 0.01),
			mix: Smoother::new(params.get(P_OSC_MIX) * 0.01),
			noise: Smoother::new(params.get(P_NOISE) * 0.01),
			gain: Smoother::new(db_to_gain(params.get(P_GAIN))),
			pos1: Smoother::new(params.get(P_OSC1_POS)),
			pos2: Smoother::new(params.get(P_OSC2_POS)),
			// Generates the tables here, never on the audio thread.
			tables: wavetable::tables(),
			s_cutoff: [0.0; CHUNK],
			s_res: [0.0; CHUNK],
			s_mix: [0.0; CHUNK],
			s_noise: [0.0; CHUNK],
			s_pos1: [0.0; CHUNK],
			s_pos2: [0.0; CHUNK],
			params,
		};
		s.apply_envelopes();
		s
	}

	fn apply_envelopes(&mut self) {
		let p = &self.params;
		for v in &mut self.voices {
			v.amp_env.set_sample_rate(self.sample_rate);
			v.filter_env.set_sample_rate(self.sample_rate);
			v.amp_env.set_params(
				p.get(P_AMP_A),
				p.get(P_AMP_D),
				p.get(P_AMP_S) * 0.01,
				p.get(P_AMP_R),
			);
			v.filter_env.set_params(
				p.get(P_FLT_A),
				p.get(P_FLT_D),
				p.get(P_FLT_S) * 0.01,
				p.get(P_FLT_R),
			);
		}
		self.envelopes_dirty = false;
	}

	fn note_on(&mut self, pitch: u8, velocity: f32) {
		let idx = pick_voice(self.voices.iter().map(|v| &v.slot), pitch);
		self.note_counter += 1;
		let glide = self.params.get(P_GLIDE) > 0.0;
		let start_pitch = match self.last_pitch {
			Some(p) if glide => p,
			_ => pitch as f32,
		};
		let v = &mut self.voices[idx];
		if !v.slot.active {
			for (j, (o1, o2)) in v.osc1.iter_mut().zip(&mut v.osc2).enumerate() {
				o1.set_phase(j as f32 * 0.37);
				o2.set_phase(j as f32 * 0.61 + 0.25);
			}
			for (j, (o1, o2)) in v.wt1.iter_mut().zip(&mut v.wt2).enumerate() {
				o1.set_phase(j as f32 * 0.37);
				o2.set_phase(j as f32 * 0.61 + 0.25);
			}
			for f in v.filters.iter_mut().flatten() {
				f.reset();
			}
			v.pitch = start_pitch;
			v.control_counter = 0;
		} else if glide {
			v.pitch = start_pitch;
		}
		v.target_pitch = pitch as f32;
		if !glide {
			v.pitch = pitch as f32;
		}
		v.velocity = velocity.clamp(0.0, 1.0);
		v.slot = VoiceSlot {
			note: pitch,
			active: true,
			released: false,
			started: self.note_counter,
			pedal: false,
			elapsed: 0,
		};
		v.amp_env.gate_on();
		v.filter_env.gate_on();
		self.last_pitch = Some(pitch as f32);
	}

	fn note_off(&mut self, pitch: u8) {
		let sustain = self.expr.sustain;
		for v in &mut self.voices {
			if v.slot.active && !v.slot.released && v.slot.note == pitch && v.slot.key_up(sustain) {
				v.slot.release();
				v.amp_env.gate_off();
				v.filter_env.gate_off();
			}
		}
	}

	fn pedal_up(&mut self) {
		for v in &mut self.voices {
			if v.slot.pedal_up() {
				v.slot.release();
				v.amp_env.gate_off();
				v.filter_env.gate_off();
			}
		}
	}

	fn all_notes_off(&mut self) {
		for v in &mut self.voices {
			if v.slot.active {
				v.slot.release();
				v.amp_env.gate_off();
				v.filter_env.gate_off();
			}
		}
	}

	fn handle(&mut self, ev: &MidiEvent) {
		self.held.apply(ev);
		if matches!(ev, MidiEvent::NoteOn { .. } | MidiEvent::NoteOff { .. }) {
			self.expr.apply(ev);
		}
		match *ev {
			MidiEvent::NoteOn {
				pitch, velocity, ..
			} => {
				if velocity <= 0.0 {
					self.note_off(pitch);
				} else {
					self.note_on(pitch.min(127), velocity);
				}
			}
			MidiEvent::NoteOff { pitch, .. } => self.note_off(pitch),
			MidiEvent::AllNotesOff { .. } => {
				self.expr.apply(ev);
				self.all_notes_off();
			}
			_ => match self.expr.apply(ev) {
				ExpressionAction::PedalUp => self.pedal_up(),
				ExpressionAction::AllNotesOff => self.all_notes_off(),
				ExpressionAction::None => {}
			},
		}
	}

	fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
		let n = left.len();
		for i in 0..n {
			self.s_cutoff[i] = self.cutoff_oct.tick();
			self.s_res[i] = self.resonance.tick();
			self.s_mix[i] = self.mix.tick();
			self.s_noise[i] = self.noise.tick();
			self.s_pos1[i] = self.pos1.tick();
			self.s_pos2[i] = self.pos2.tick();
		}
		left.fill(0.0);
		right.fill(0.0);

		let p = &self.params;
		let sr = self.sample_rate;
		let w1 = Waveform::from_index(p.choice(P_OSC1_WAVE));
		let w2 = Waveform::from_index(p.choice(P_OSC2_WAVE));
		let table1: Option<&Table> = (p.choice(P_OSC1_WAVE) == WAVE_WAVETABLE)
			.then(|| self.tables.table(p.choice(P_OSC1_TABLE).min(N_TABLES - 1)));
		let table2: Option<&Table> = (p.choice(P_OSC2_WAVE) == WAVE_WAVETABLE)
			.then(|| self.tables.table(p.choice(P_OSC2_TABLE).min(N_TABLES - 1)));
		let osc2_ratio = 2.0_f32.powf((p.get(P_OSC2_SEMI) + p.get(P_OSC2_CENTS) * 0.01) / 12.0);
		let unison = p.choice(P_UNISON).clamp(1, MAX_UNISON);
		let spread = p.get(P_UNISON_DETUNE);
		let mut detune = [1.0_f32; MAX_UNISON];
		let mut pan_l = [1.0_f32; MAX_UNISON];
		let mut pan_r = [1.0_f32; MAX_UNISON];
		if unison > 1 {
			let norm = 1.0 / (unison as f32).sqrt();
			for j in 0..unison {
				let pos = j as f32 / (unison - 1) as f32 * 2.0 - 1.0;
				detune[j] = 2.0_f32.powf(pos * spread * 0.5 / 1200.0);
				let pan = pos * 0.8;
				pan_l[j] = ((1.0 - pan) * 0.5).sqrt() * std::f32::consts::SQRT_2 * norm;
				pan_r[j] = ((1.0 + pan) * 0.5).sqrt() * std::f32::consts::SQRT_2 * norm;
			}
		}
		let slope_24 = p.flag(P_SLOPE);
		let env_amount = p.get(P_ENV_AMOUNT);
		let key_track = p.get(P_KEY_TRACK) * 0.01;
		let vel_sens = p.get(P_VEL_SENS) * 0.01;
		let glide_coeff = one_pole_coeff(p.get(P_GLIDE) * 0.4, sr);
		// Bend + mod wheel vibrato for this chunk; pressure opens the filter.
		let pitch_offset = self.expr.pitch_offset(n, sr);
		let pressure_semis = self.expr.pressure * 24.0;

		for v in self.voices.iter_mut().filter(|v| v.slot.active) {
			let vel_scale = 1.0 - vel_sens * (1.0 - v.velocity);
			let amp_scale = vel_scale * vel_scale * VOICE_GAIN;
			let key_bend = self.expr.note_bend(v.slot.note);
			let mut inc = midi_to_hz(v.pitch + pitch_offset + key_bend) / sr;
			for i in 0..n {
				if v.pitch != v.target_pitch {
					v.pitch += (v.target_pitch - v.pitch) * glide_coeff;
					if (v.target_pitch - v.pitch).abs() < 1e-3 {
						v.pitch = v.target_pitch;
					}
				}
				let fenv = v.filter_env.tick();
				let aenv = v.amp_env.tick();
				if v.control_counter == 0 {
					inc = midi_to_hz(v.pitch + pitch_offset + key_bend) / sr;
					let lanes = v.lvl1.iter_mut().zip(&mut v.lvl2).zip(&detune);
					for ((l1, l2), d) in lanes.take(unison) {
						if table1.is_some() {
							*l1 = Table::level_for(inc * d);
						}
						if table2.is_some() {
							*l2 = Table::level_for(inc * d * osc2_ratio);
						}
					}
					let semis = fenv * env_amount * vel_scale
						+ key_track * (v.pitch - 60.0)
						+ pressure_semis;
					let cutoff = (self.s_cutoff[i] + semis / 12.0).exp2();
					let res = self.s_res[i];
					v.coeffs = SvfCoeffs::new(cutoff, res, sr);
					if slope_24 {
						v.coeffs_flat = SvfCoeffs::new(cutoff, 0.3, sr);
					}
					// Resonance thins the low end; trim input so loudness stays even.
					v.res_comp = 1.0 - 0.45 * res;
				}
				v.control_counter = (v.control_counter + 1) % CONTROL_RATE;

				let mix = self.s_mix[i];
				let mut l = 0.0;
				let mut r = 0.0;
				for j in 0..unison {
					let i1 = inc * detune[j];
					let a = match table1 {
						Some(t) => v.wt1[j].next(t, v.lvl1[j], self.s_pos1[i], i1),
						None => v.osc1[j].next(w1, i1),
					};
					let b = match table2 {
						Some(t) => v.wt2[j].next(t, v.lvl2[j], self.s_pos2[i], i1 * osc2_ratio),
						None => v.osc2[j].next(w2, i1 * osc2_ratio),
					};
					let s = a + (b - a) * mix;
					l += s * pan_l[j];
					r += s * pan_r[j];
				}
				let noise = self.s_noise[i];
				if noise > 0.0 {
					let nz = self.rng.bipolar() * noise;
					l += nz;
					r += nz;
				}
				let [fl, fr] = &mut v.filters;
				let (mut l, mut r) = (l * v.res_comp, r * v.res_comp);
				if slope_24 {
					l = fl[1].lowpass(&v.coeffs_flat, l);
					r = fr[1].lowpass(&v.coeffs_flat, r);
				}
				let yl = fl[0].lowpass(&v.coeffs, l);
				let yr = fr[0].lowpass(&v.coeffs, r);
				let g = aenv * amp_scale;
				left[i] += yl * g;
				right[i] += yr * g;
			}
			v.slot.advance(n);
			if !v.amp_env.is_active() {
				v.slot.active = false;
				v.filter_env.reset();
			}
		}

		for i in 0..n {
			let g = self.gain.tick();
			left[i] *= g;
			right[i] *= g;
		}
	}
}

impl Processor for PolySynth {
	fn info(&self) -> &'static DeviceInfo {
		&INFO
	}

	fn prepare(&mut self, sample_rate: f32, _max_block: usize) {
		self.sample_rate = sample_rate.max(1.0);
		for s in [
			&mut self.cutoff_oct,
			&mut self.resonance,
			&mut self.mix,
			&mut self.noise,
			&mut self.gain,
			&mut self.pos1,
			&mut self.pos2,
		] {
			s.prepare(self.sample_rate, 15.0);
		}
		self.apply_envelopes();
		self.reset();
	}

	fn set_param(&mut self, index: usize, value: f32) {
		let Some(v) = self.params.set(&INFO, index, value) else {
			return;
		};
		match index {
			P_CUTOFF => self.cutoff_oct.set_target(v.log2()),
			P_RESONANCE => self.resonance.set_target(v * 0.01),
			P_OSC_MIX => self.mix.set_target(v * 0.01),
			P_NOISE => self.noise.set_target(v * 0.01),
			P_GAIN => self.gain.set_target(db_to_gain(v)),
			P_OSC1_POS => self.pos1.set_target(v),
			P_OSC2_POS => self.pos2.set_target(v),
			P_AMP_A..=P_FLT_R => self.envelopes_dirty = true,
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
		if self.envelopes_dirty {
			self.apply_envelopes();
		}
		let len = left.len().min(right.len());
		for (start, end, ev) in event_spans(len, events) {
			let mut pos = start;
			while pos < end {
				let stop = (pos + CHUNK).min(end);
				self.render(&mut left[pos..stop], &mut right[pos..stop]);
				pos = stop;
			}
			if let Some(ev) = ev {
				self.handle(ev);
			}
		}
	}

	fn reset(&mut self) {
		for v in &mut self.voices {
			v.slot = VoiceSlot::default();
			v.amp_env.reset();
			v.filter_env.reset();
			for f in v.filters.iter_mut().flatten() {
				f.reset();
			}
		}
		self.last_pitch = None;
		self.held.clear();
		self.expr.reset_controllers();
		for s in [
			&mut self.cutoff_oct,
			&mut self.resonance,
			&mut self.mix,
			&mut self.noise,
			&mut self.gain,
			&mut self.pos1,
			&mut self.pos2,
		] {
			s.snap();
		}
	}

	fn tail_seconds(&self) -> f32 {
		self.params.get(P_AMP_R) * 0.001 * 1.4
	}

	fn readings(&self, out: &mut [f32]) -> usize {
		let mut voices = NewestVoices::default();
		for v in self.voices.iter().filter(|v| v.slot.active) {
			voices.offer(
				v.slot.started,
				v.slot.reading(v.amp_env.level(), self.sample_rate),
			);
		}
		voice_readings::write(out, &self.held, &voices)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn synth_with(params: &[(&str, f32)]) -> PolySynth {
		let mut s = PolySynth::new();
		s.prepare(48_000.0, 512);
		for &(id, v) in params {
			s.set_param(INFO.param_index(id).unwrap(), v);
		}
		s.reset();
		s
	}

	/// Plays a sine-wave note through one voice (no unison/noise/envelope
	/// modulation) and measures the filter's steady-state RMS gain, for
	/// comparison against `poly_synth_filter_response_db`'s closed form.
	fn voice_filter_gain_db(cutoff: f32, resonance: f32, slope_24: bool, pitch: u8) -> f32 {
		let mut s = synth_with(&[
			("osc1_wave", 0.0), // sine
			("osc_mix", 0.0),
			("unison", 1.0),
			("noise", 0.0),
			("filter_cutoff", cutoff),
			("filter_resonance", resonance),
			("filter_slope", if slope_24 { 1.0 } else { 0.0 }),
			("filter_env_amount", 0.0),
			("filter_key_track", 0.0),
			("velocity_sensitivity", 0.0),
			("glide", 0.0),
			("amp_attack", 0.0),
			("amp_sustain", 100.0),
		]);
		s.handle(&MidiEvent::NoteOn {
			offset: 0,
			pitch,
			velocity: 1.0,
		});
		let n_samples = 96_000;
		let mut left = vec![0.0_f32; n_samples];
		let mut right = vec![0.0_f32; n_samples];
		for (l, r) in left.chunks_mut(256).zip(right.chunks_mut(256)) {
			s.process(l, r, &[], &Transport::default());
		}
		let settled = &left[n_samples / 2..];
		let sum_sq: f64 = settled.iter().map(|&x| (x as f64) * (x as f64)).sum();
		let rms = (sum_sq / settled.len() as f64).sqrt() as f32;
		// Input sine amplitude 1.0 (RMS 1/sqrt(2)) times the fixed voice gain
		// (velocity_sensitivity 0, amp envelope settled at sustain 100%).
		let input_rms = std::f32::consts::FRAC_1_SQRT_2 * VOICE_GAIN;
		crate::util::gain_to_db(rms / input_rms)
	}

	fn render_note(s: &mut PolySynth, pitch: u8, frames: usize) -> Vec<f32> {
		s.handle(&MidiEvent::NoteOn {
			offset: 0,
			pitch,
			velocity: 1.0,
		});
		let mut left = vec![0.0_f32; frames];
		let mut right = vec![0.0_f32; frames];
		for (l, r) in left.chunks_mut(256).zip(right.chunks_mut(256)) {
			s.process(l, r, &[], &Transport::default());
		}
		left
	}

	#[test]
	fn wavetable_voice_plays_and_position_morphs() {
		let open = [
			("osc1_wave", 4.0),
			("osc_mix", 0.0),
			("filter_cutoff", 20_000.0),
			("filter_env_amount", 0.0),
			("amp_attack", 0.0),
			("amp_sustain", 100.0),
		];
		let mut a = synth_with(&open);
		let mut b = synth_with(&open);
		b.set_param(INFO.param_index("osc1_position").unwrap(), 1.0);
		b.reset();
		let sine = render_note(&mut a, 57, 9_600);
		let square = render_note(&mut b, 57, 9_600);
		let rms = |x: &[f32]| (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt();
		assert!(rms(&sine[4_800..]) > 0.1, "wavetable voice is silent");
		// Position 0 is a sine, 1 a square: louder RMS at the same peak.
		assert!(
			rms(&square[4_800..]) > rms(&sine[4_800..]) * 1.1,
			"{} {}",
			rms(&square[4_800..]),
			rms(&sine[4_800..])
		);
		// A position change glides (smoothed) instead of clicking.
		let idx = INFO.param_index("osc1_position").unwrap();
		a.set_param(idx, 1.0);
		let mut l = vec![0.0_f32; 256];
		let mut r = vec![0.0_f32; 256];
		a.process(&mut l, &mut r, &[], &Transport::default());
		let jump = l
			.windows(2)
			.fold(0.0_f32, |m, w| m.max((w[1] - w[0]).abs()));
		assert!(jump < 0.2, "position step clicked: {jump}");
	}

	#[test]
	fn filter_response_matches_voice_output() {
		let pitch = 69; // A4, 440 Hz
		let freq = crate::util::midi_to_hz(pitch as f32);
		for &(cutoff, resonance, slope_24) in &[
			(1_000.0, 20.0, false),
			(300.0, 70.0, true),
			(2_000.0, 0.0, false),
		] {
			let formula =
				poly_synth_filter_response_db(cutoff, resonance, slope_24, 48_000.0, freq);
			let measured = voice_filter_gain_db(cutoff, resonance, slope_24, pitch);
			assert!(
				(formula - measured).abs() < 0.5,
				"cutoff {cutoff} res {resonance} slope_24 {slope_24}: formula {formula}, measured {measured}"
			);
		}
	}
}
