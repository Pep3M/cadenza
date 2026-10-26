//! `electric_piano`: FM tine piano in the spirit of the classic DX-style
//! "E.Piano" patches (clean-room: two FM pairs plus a bell partial).

use super::voice_readings::{self, HeldNotes, NewestVoices};
use super::{VoiceSlot, pick_voice};
use crate::processor::{DeviceInfo, DeviceRole, MidiEvent, ParamInfo, Processor, Transport};
use crate::util::envelope::{Adsr, DecayEnv};
use crate::util::expression::{Expression, ExpressionAction};
use crate::util::smoother::Smoother;
use crate::util::{ParamValues, TAU, db_to_gain, event_spans, midi_to_hz};

const VOICES: usize = 16;
const VOICE_GAIN: f32 = 0.28;

const P_BRIGHTNESS: usize = 0;
const P_BELL: usize = 1;
const P_DECAY: usize = 2;
const P_RELEASE: usize = 3;
const P_VEL_SENS: usize = 4;
const P_TREM_RATE: usize = 5;
const P_TREM_DEPTH: usize = 6;
const P_GAIN: usize = 7;
const N_PARAMS: usize = 8;

static PARAMS: [ParamInfo; N_PARAMS] = [
	ParamInfo::new(
		"brightness",
		"Brightness",
		0.0,
		100.0,
		50.0,
		"%",
		"FM depth: low = mellow, round neo-soul keys; high = barky, bright 80s ballad keys. Velocity adds to it.",
	),
	ParamInfo::new(
		"bell",
		"Bell",
		0.0,
		100.0,
		35.0,
		"%",
		"Level of the metallic tine 'ding' at the start of each note.",
	),
	ParamInfo::new(
		"decay",
		"Decay",
		500.0,
		15_000.0,
		4_000.0,
		"ms",
		"How long a held middle-C note takes to fade out (lower notes ring longer).",
	),
	ParamInfo::new(
		"release",
		"Release",
		10.0,
		3_000.0,
		250.0,
		"ms",
		"Damper time after key release. 100-300 natural, 1000+ sustain-pedal feel.",
	),
	ParamInfo::new(
		"velocity_sensitivity",
		"Velocity Sensitivity",
		0.0,
		100.0,
		70.0,
		"%",
		"How much velocity changes loudness (brightness always follows velocity).",
	),
	ParamInfo::new(
		"tremolo_rate",
		"Tremolo Rate",
		0.1,
		10.0,
		4.5,
		"Hz",
		"Speed of the stereo auto-pan tremolo.",
	),
	ParamInfo::new(
		"tremolo_depth",
		"Tremolo Depth",
		0.0,
		100.0,
		0.0,
		"%",
		"Amount of stereo auto-pan tremolo. 20-50 for the classic suitcase sound.",
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
];

pub static INFO: DeviceInfo = DeviceInfo {
	id: "electric_piano",
	name: "Electric Piano",
	role: DeviceRole::Instrument,
	description: "16-voice FM electric piano (Rhodes/tine style). Velocity drives brightness and \
		bark, so humanize velocities (0.5-0.95). Use for chords and comping in neo-soul, R&B, lo-fi \
		hip-hop, jazz, pop ballads and funk. Recipes — NEO-SOUL/LO-FI: brightness 30, bell 25, \
		tremolo_depth 30, tremolo_rate 3.5. 80s BALLAD: brightness 75, bell 60, decay 6000. FUNK \
		COMPING: brightness 65, release 120, decay 2500. Pairs well with chorus and reverb.",
	params: &PARAMS,
};

#[derive(Debug, Clone, Copy, Default)]
struct Voice {
	slot: VoiceSlot,
	freq: f32,
	phase_c: f32,
	phase_m: f32,
	phase_b: f32,
	amp: Adsr,
	mod_env: DecayEnv,
	bell_env: DecayEnv,
	index: f32,
	bell: f32,
	gain: f32,
}

pub struct ElectricPiano {
	expr: Expression,
	params: ParamValues<N_PARAMS>,
	sample_rate: f32,
	voices: [Voice; VOICES],
	note_counter: u64,
	lfo_phase: f32,
	trem_depth: Smoother,
	gain: Smoother,
	held: HeldNotes,
}

impl Default for ElectricPiano {
	fn default() -> Self {
		Self::new()
	}
}

impl ElectricPiano {
	pub fn new() -> Self {
		let params = ParamValues::new(&INFO);
		Self {
			sample_rate: 48_000.0,
			voices: [Voice::default(); VOICES],
			note_counter: 0,
			lfo_phase: 0.0,
			trem_depth: Smoother::new(params.get(P_TREM_DEPTH) * 0.01),
			gain: Smoother::new(db_to_gain(params.get(P_GAIN))),
			expr: Expression::default(),
			held: HeldNotes::default(),
			params,
		}
	}

	fn note_on(&mut self, pitch: u8, velocity: f32) {
		let sr = self.sample_rate;
		let p = &self.params;
		let idx = pick_voice(self.voices.iter().map(|v| &v.slot), pitch);
		self.note_counter += 1;
		let vel = velocity.clamp(0.0, 1.0);
		let note = pitch as f32;
		// Lower notes ring longer and are naturally darker; high notes get less FM.
		let key_scale = 2.0_f32.powf(-(note - 60.0) / 24.0);
		let decay = p.get(P_DECAY) * key_scale.clamp(0.25, 3.0);
		let bright = p.get(P_BRIGHTNESS) * 0.01;
		let high_damp = (1.0 - (note - 72.0).max(0.0) / 48.0).max(0.2);

		let v = &mut self.voices[idx];
		if !v.slot.active {
			v.phase_c = 0.0;
			v.phase_m = 0.0;
			v.phase_b = 0.0;
		}
		v.freq = midi_to_hz(note);
		v.amp.set_sample_rate(sr);
		v.amp.set_params(2.0, decay, 0.0, p.get(P_RELEASE));
		v.amp.gate_on();
		v.index = (0.15 + 2.6 * bright * (0.35 + 0.65 * vel * vel)) * high_damp;
		v.mod_env.trigger(1.0, (decay * 0.35).max(200.0), sr);
		v.bell = p.get(P_BELL) * 0.01 * (0.3 + 0.7 * vel) * high_damp;
		v.bell_env.trigger(1.0, 250.0 + 300.0 * key_scale, sr);
		let sens = p.get(P_VEL_SENS) * 0.01;
		let loud = 1.0 - sens * (1.0 - vel);
		v.gain = loud * loud * VOICE_GAIN;
		v.slot = VoiceSlot {
			note: pitch,
			active: true,
			released: false,
			started: self.note_counter,
			pedal: false,
			elapsed: 0,
		};
	}

	fn note_off(&mut self, pitch: u8) {
		let sustain = self.expr.sustain;
		for v in &mut self.voices {
			if v.slot.active && !v.slot.released && v.slot.note == pitch && v.slot.key_up(sustain) {
				v.slot.release();
				v.amp.gate_off();
			}
		}
	}

	fn release_all(&mut self) {
		for v in &mut self.voices {
			v.slot.release();
			v.slot.pedal = false;
			v.amp.gate_off();
		}
	}

	fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
		let inv_sr = 1.0 / self.sample_rate;
		// Bend and mod wheel vibrato for this span.
		let ratio = 2.0_f32.powf(self.expr.pitch_offset(left.len(), self.sample_rate) / 12.0);
		let lfo_inc = self.params.get(P_TREM_RATE) * inv_sr;
		// Each key's own bend, per span.
		let mut key = [1.0f32; VOICES];
		for (k, v) in key.iter_mut().zip(&self.voices) {
			let bend = self.expr.note_bend(v.slot.note);
			if v.slot.active && bend != 0.0 {
				*k = 2.0_f32.powf(bend / 12.0);
			}
		}
		for (l, r) in left.iter_mut().zip(right.iter_mut()) {
			let mut mono = 0.0;
			for (v, key) in self
				.voices
				.iter_mut()
				.zip(&key)
				.filter(|(v, _)| v.slot.active)
			{
				let a = v.amp.tick();
				let m = (TAU * v.phase_m).sin() * v.index * v.mod_env.tick();
				let body = (TAU * v.phase_c + m).sin();
				let bell = (TAU * v.phase_b + 0.5 * m).sin() * v.bell * v.bell_env.tick();
				mono += (body + bell) * a * v.gain;
				let inc = v.freq * ratio * key * inv_sr;
				v.phase_c = (v.phase_c + inc).fract();
				// Slight modulator detune gives the tine its slow beating.
				v.phase_m = (v.phase_m + inc * 1.0007).fract();
				v.phase_b = (v.phase_b + inc * 4.0).fract();
				if !v.amp.is_active() {
					v.slot.active = false;
				}
			}
			let depth = self.trem_depth.tick();
			let s = (TAU * self.lfo_phase).sin();
			self.lfo_phase = (self.lfo_phase + lfo_inc).fract();
			let g = self.gain.tick();
			*l = mono * (1.0 - depth * (0.5 + 0.5 * s)) * g;
			*r = mono * (1.0 - depth * (0.5 - 0.5 * s)) * g;
		}
		for v in self.voices.iter_mut().filter(|v| v.slot.active) {
			v.slot.advance(left.len());
		}
	}
}

impl Processor for ElectricPiano {
	fn info(&self) -> &'static DeviceInfo {
		&INFO
	}

	fn prepare(&mut self, sample_rate: f32, _max_block: usize) {
		self.sample_rate = sample_rate.max(1.0);
		self.trem_depth.prepare(self.sample_rate, 20.0);
		self.gain.prepare(self.sample_rate, 15.0);
		self.reset();
	}

	fn set_param(&mut self, index: usize, value: f32) {
		let Some(v) = self.params.set(&INFO, index, value) else {
			return;
		};
		match index {
			P_TREM_DEPTH => self.trem_depth.set_target(v * 0.01),
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
			if let Some(ev) = ev {
				self.held.apply(ev);
				if matches!(ev, MidiEvent::NoteOn { .. } | MidiEvent::NoteOff { .. }) {
					self.expr.apply(ev);
				}
			}
			match ev {
				Some(&MidiEvent::NoteOn {
					pitch, velocity, ..
				}) => {
					if velocity > 0.0 {
						self.note_on(pitch.min(127), velocity);
					} else {
						self.note_off(pitch);
					}
				}
				Some(&MidiEvent::NoteOff { pitch, .. }) => self.note_off(pitch),
				Some(ev @ MidiEvent::AllNotesOff { .. }) => {
					self.expr.apply(ev);
					self.release_all();
				}
				Some(ev) => match self.expr.apply(ev) {
					ExpressionAction::PedalUp => {
						for v in &mut self.voices {
							if v.slot.pedal_up() {
								v.slot.release();
								v.amp.gate_off();
							}
						}
					}
					ExpressionAction::AllNotesOff => self.release_all(),
					ExpressionAction::None => {}
				},
				None => {}
			}
		}
	}

	fn reset(&mut self) {
		for v in &mut self.voices {
			*v = Voice::default();
		}
		self.lfo_phase = 0.0;
		self.held.clear();
		self.expr.reset_controllers();
		self.trem_depth.snap();
		self.gain.snap();
	}

	fn tail_seconds(&self) -> f32 {
		self.params.get(P_RELEASE) * 0.001 * 1.4
	}

	fn readings(&self, out: &mut [f32]) -> usize {
		let mut voices = NewestVoices::default();
		for v in self.voices.iter().filter(|v| v.slot.active) {
			voices.offer(
				v.slot.started,
				v.slot.reading(v.amp.level(), self.sample_rate),
			);
		}
		voice_readings::write(out, &self.held, &voices)
	}
}
