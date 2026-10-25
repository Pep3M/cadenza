//! `sampler`: multi-zone sample player (key/velocity zones, loops, amp + filter envelopes).

use super::sample_voice::{
	KillFade, Playhead, SlotInfo, ZoneEnv, ZoneRt, pick_slot, select_zones, velocity_gain,
};
use super::voice_readings::{self, HeldNotes, NewestVoices, VoiceReading};
use crate::processor::{DeviceInfo, DeviceRole, MidiEvent, ParamInfo, Processor, Transport};
use crate::sample::Zone;
use crate::util::envelope::Adsr;
use crate::util::expression::{Expression, ExpressionAction};
use crate::util::smoother::Smoother;
use crate::util::svf::{Svf, SvfCoeffs};
use crate::util::{ParamValues, db_to_gain, event_spans, one_pole_coeff};

pub const P_GAIN: usize = 0;
pub const P_POLYPHONY: usize = 1;
pub const P_MODE: usize = 2;
pub const P_AMP_ATTACK: usize = 3;
pub const P_AMP_DECAY: usize = 4;
pub const P_AMP_SUSTAIN: usize = 5;
pub const P_AMP_RELEASE: usize = 6;
pub const P_VELOCITY: usize = 7;
pub const P_VELOCITY_CURVE: usize = 8;
pub const P_FILTER_TYPE: usize = 9;
pub const P_FILTER_CUTOFF: usize = 10;
pub const P_FILTER_RESONANCE: usize = 11;
pub const P_FILTER_ENV_AMOUNT: usize = 12;
pub const P_FILTER_ATTACK: usize = 13;
pub const P_FILTER_DECAY: usize = 14;
pub const P_FILTER_SUSTAIN: usize = 15;
pub const P_FILTER_RELEASE: usize = 16;
pub const P_FILTER_VELOCITY: usize = 17;
pub const P_FILTER_KEY_TRACK: usize = 18;
pub const P_GLIDE: usize = 19;
pub const P_LEGATO: usize = 20;
pub const P_TRANSPOSE: usize = 21;
pub const N_PARAMS: usize = 22;

/// Most voices a sampler can play at once (the `polyphony` param's maximum).
pub const MAX_POLYPHONY: usize = 64;

static PARAMS: [ParamInfo; N_PARAMS] = [
	ParamInfo::new(
		"gain",
		"Gain",
		-60.0,
		12.0,
		0.0,
		"dB",
		"Output level of the sampler.",
	),
	ParamInfo::new(
		"polyphony",
		"Polyphony",
		1.0,
		64.0,
		16.0,
		"voices",
		"Notes that can sound at once; beyond it the oldest note is faded out (stolen).",
	),
	ParamInfo::new(
		"mode",
		"Trigger Mode",
		0.0,
		1.0,
		0.0,
		"",
		"0 = gate: the note length matters (release on note-off; loops sustain while held). \
		 1 = one-shot: every note plays the whole sample regardless of its length (hits, FX).",
	),
	ParamInfo::new(
		"amp_attack",
		"Amp Attack",
		0.0,
		10_000.0,
		1.0,
		"ms",
		"Fade-in time. 0-5 keeps the sample's own attack, 200-2000 for swelling pads.",
	),
	ParamInfo::new(
		"amp_decay",
		"Amp Decay",
		1.0,
		20_000.0,
		1_000.0,
		"ms",
		"Time to fall from peak to the sustain level.",
	),
	ParamInfo::new(
		"amp_sustain",
		"Amp Sustain",
		0.0,
		100.0,
		100.0,
		"%",
		"Level held while the key is down. 100 plays the sample as recorded.",
	),
	ParamInfo::new(
		"amp_release",
		"Amp Release",
		1.0,
		20_000.0,
		200.0,
		"ms",
		"Fade-out after note-off. 50-200 for tight parts, 800-3000 for pads.",
	),
	ParamInfo::new(
		"velocity_sensitivity",
		"Velocity Sensitivity",
		0.0,
		100.0,
		70.0,
		"%",
		"How much note velocity changes loudness (0 = every note equally loud).",
	),
	ParamInfo::new(
		"velocity_curve",
		"Velocity Curve",
		-100.0,
		100.0,
		0.0,
		"%",
		"Shape of the velocity response: negative = soft notes louder (easy), positive = \
		 soft notes quieter (more dynamic), 0 = natural.",
	),
	ParamInfo::new(
		"filter_type",
		"Filter Type",
		0.0,
		3.0,
		0.0,
		"",
		"0 = off, 1 = low-pass, 2 = high-pass, 3 = band-pass (state-variable, 12 dB/oct).",
	),
	ParamInfo::new(
		"filter_cutoff",
		"Filter Cutoff",
		20.0,
		20_000.0,
		12_000.0,
		"Hz",
		"Cutoff before envelope, velocity and key tracking. Darken pads at 800-3000.",
	),
	ParamInfo::new(
		"filter_resonance",
		"Filter Resonance",
		0.0,
		100.0,
		10.0,
		"%",
		"Emphasis at the cutoff. 0-30 smooth, 50-80 vocal/squelchy.",
	),
	ParamInfo::new(
		"filter_env_amount",
		"Filter Env Amount",
		-96.0,
		96.0,
		0.0,
		"semitones",
		"How far the filter envelope moves the cutoff (negative closes it).",
	),
	ParamInfo::new(
		"filter_attack",
		"Filter Attack",
		0.0,
		10_000.0,
		1.0,
		"ms",
		"Filter envelope attack.",
	),
	ParamInfo::new(
		"filter_decay",
		"Filter Decay",
		1.0,
		20_000.0,
		500.0,
		"ms",
		"Filter envelope decay.",
	),
	ParamInfo::new(
		"filter_sustain",
		"Filter Sustain",
		0.0,
		100.0,
		0.0,
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
		"filter_velocity",
		"Filter Velocity",
		0.0,
		100.0,
		0.0,
		"%",
		"How much velocity opens the cutoff (100% = up to 4 octaves at full velocity).",
	),
	ParamInfo::new(
		"filter_key_track",
		"Filter Key Tracking",
		0.0,
		100.0,
		0.0,
		"%",
		"How much the cutoff follows the played note relative to C4 (100% = exactly).",
	),
	ParamInfo::new(
		"glide",
		"Glide",
		0.0,
		2_000.0,
		0.0,
		"ms",
		"Portamento time from the previous note's pitch (0 = off).",
	),
	ParamInfo::new(
		"legato",
		"Legato",
		0.0,
		1.0,
		0.0,
		"",
		"1 = monophonic legato: overlapping notes glide without restarting the sample \
		 or envelopes (bass and lead lines).",
	),
	ParamInfo::new(
		"transpose",
		"Transpose",
		-48.0,
		48.0,
		0.0,
		"semitones",
		"Pitch offset of every zone.",
	),
];

pub static INFO: DeviceInfo = DeviceInfo {
	id: "sampler",
	name: "Sampler",
	role: DeviceRole::Instrument,
	description: "Multi-zone sample player: maps audio files onto key and velocity ranges \
		(zones with root key, fine tune, start/end, forward or ping-pong loops with \
		crossfade, round-robin groups), with amp ADSR, a resonant LP/HP/BP filter with its \
		own envelope, velocity response, glide and legato. Load samples with load_sample \
		(one file is mapped across the keyboard at its detected root key) and edit zones \
		with set_zone. Factory presets use synthesized samples. Good for melodic one-shots \
		turned into instruments, vocal chops, multisampled keys and pads.",
	params: &PARAMS,
};

/// Static magnitude response of the filter stage alone (no envelope, velocity
/// or key tracking, which modulate `cutoff_hz` live), in dB. `filter_type`:
/// 0 off (returns 0 dB), 1 low-pass, 2 high-pass, 3 band-pass. Uses the same
/// [`SvfCoeffs::new`] the voice applies so the picture matches the sound.
pub fn sampler_filter_response_db(
	filter_type: usize,
	cutoff_hz: f32,
	resonance: f32,
	sample_rate: f32,
	freq_hz: f32,
) -> f32 {
	let output = match filter_type {
		0 => return 0.0,
		1 => crate::util::svf::SvfOutput::Low,
		2 => crate::util::svf::SvfOutput::High,
		_ => crate::util::svf::SvfOutput::Band,
	};
	let coeffs = SvfCoeffs::new(cutoff_hz, (resonance * 0.01).clamp(0.0, 1.0), sample_rate);
	crate::util::svf::svf_magnitude_db(&coeffs, output, freq_hz, sample_rate)
}

/// Voice slots: the polyphony maximum plus room for voices fading out after a steal.
const SLOTS: usize = MAX_POLYPHONY + 16;
/// Samples between pitch/filter coefficient updates.
const CONTROL: u32 = 16;
const STEAL_MS: f32 = 5.0;
const CHOKE_MS: f32 = 4.0;
const ALL_OFF_MS: f32 = 10.0;

#[derive(Clone, Copy, Default)]
struct Voice {
	slot: SlotInfo,
	note: u8,
	zone: usize,
	head: Playhead,
	zone_env: ZoneEnv,
	kill: KillFade,
	amp: Adsr,
	filter_env: Adsr,
	/// Current and target pitch (MIDI note units) for glide.
	pitch: f32,
	target: f32,
	velocity: f32,
	vel_gain: f32,
	ratio: f64,
	counter: u32,
	svf: [Svf; 2],
	coeffs: SvfCoeffs,
	/// Key up while the sustain pedal is down.
	pedal: bool,
	/// Samples since the note-on, or since the release once released (readings).
	elapsed: u32,
}

impl Voice {
	fn release(&mut self) {
		if !self.slot.released {
			self.slot.released = true;
			self.elapsed = 0;
		}
	}
}

/// Multi-zone sampler.
pub struct Sampler {
	params: ParamValues<N_PARAMS>,
	zones: Vec<Zone>,
	rt: Vec<ZoneRt>,
	voices: Box<[Voice; SLOTS]>,
	sample_rate: f32,
	gain: Smoother,
	clock: u64,
	last_pitch: Option<f32>,
	rr: [u32; 17],
	/// Longest playable region without a loop (seconds), for one-shot tails.
	longest: f32,
	expr: Expression,
	/// Bend + vibrato of the span being rendered (semitones).
	pitch_offset: f32,
	held: HeldNotes,
}

impl Default for Sampler {
	fn default() -> Self {
		Self::new()
	}
}

impl Sampler {
	pub fn new() -> Self {
		crate::util::interp::SincTable::global();
		let params = ParamValues::new(&INFO);
		Self {
			gain: Smoother::new(db_to_gain(params.get(P_GAIN))),
			params,
			zones: Vec::new(),
			rt: Vec::new(),
			voices: Box::new([Voice::default(); SLOTS]),
			sample_rate: 48_000.0,
			clock: 0,
			last_pitch: None,
			rr: [0; 17],
			longest: 0.0,
			expr: Expression::default(),
			pitch_offset: 0.0,
			held: HeldNotes::default(),
		}
	}

	/// The zone map last given to [`Processor::set_zones`].
	pub fn zones(&self) -> &[Zone] {
		&self.zones
	}

	/// Voices currently sounding (including ones fading out).
	pub fn active_voices(&self) -> usize {
		self.voices.iter().filter(|v| v.slot.active).count()
	}

	fn one_shot(&self) -> bool {
		self.params.flag(P_MODE)
	}

	fn legato(&self) -> bool {
		self.params.flag(P_LEGATO)
	}

	fn polyphony(&self) -> usize {
		if self.legato() {
			1
		} else {
			self.params
				.get(P_POLYPHONY)
				.round()
				.clamp(1.0, MAX_POLYPHONY as f32) as usize
		}
	}

	fn set_envelopes(&self, v: &mut Voice) {
		let p = &self.params;
		v.amp.set_sample_rate(self.sample_rate);
		v.amp.set_params(
			p.get(P_AMP_ATTACK),
			p.get(P_AMP_DECAY),
			p.get(P_AMP_SUSTAIN) * 0.01,
			p.get(P_AMP_RELEASE),
		);
		v.filter_env.set_sample_rate(self.sample_rate);
		v.filter_env.set_params(
			p.get(P_FILTER_ATTACK),
			p.get(P_FILTER_DECAY),
			p.get(P_FILTER_SUSTAIN) * 0.01,
			p.get(P_FILTER_RELEASE),
		);
	}

	fn note_on(&mut self, pitch: u8, velocity: f32) {
		let target = f32::from(pitch);
		if self.legato() {
			let instant = self.params.get(P_GLIDE) <= 0.0;
			if let Some(v) = self
				.voices
				.iter_mut()
				.find(|v| v.slot.active && !v.slot.fading && !v.slot.released)
			{
				v.note = pitch;
				v.target = target;
				if instant {
					v.pitch = target;
				}
				self.last_pitch = Some(target);
				return;
			}
		}
		if !self.one_shot() {
			let sr = self.sample_rate;
			for v in self.voices.iter_mut() {
				if v.slot.active && v.note == pitch {
					v.kill.start(STEAL_MS, sr);
					v.slot.fading = true;
				}
			}
		}
		let start_pitch = match self.last_pitch {
			Some(last) if self.params.get(P_GLIDE) > 0.0 => last,
			_ => target,
		};
		self.last_pitch = Some(target);
		let mut chosen = [usize::MAX; 32];
		let mut n = 0;
		select_zones(&self.zones, pitch, velocity, &mut self.rr, |i| {
			if n < chosen.len() {
				chosen[n] = i;
				n += 1;
			}
		});
		for &zone in &chosen[..n] {
			if self.zones[zone].sample.is_some() {
				self.start_voice(zone, pitch, velocity, start_pitch);
			}
		}
	}

	fn start_voice(&mut self, zone: usize, pitch: u8, velocity: f32, start_pitch: f32) {
		let sr = self.sample_rate;
		let choke = self.zones[zone].choke;
		if choke > 0 {
			for v in self.voices.iter_mut() {
				if v.slot.active && self.zones.get(v.zone).is_some_and(|z| z.choke == choke) {
					v.kill.start(CHOKE_MS, sr);
					v.slot.fading = true;
				}
			}
		}
		let mut infos = [SlotInfo::default(); SLOTS];
		for (i, v) in infos.iter_mut().zip(self.voices.iter()) {
			*i = v.slot;
		}
		let (slot, steal) = pick_slot(&infos, self.polyphony());
		if let Some(s) = steal {
			self.voices[s].kill.start(STEAL_MS, sr);
			self.voices[s].slot.fading = true;
		}
		self.clock += 1;
		let p = &self.params;
		let mut v = Voice {
			slot: SlotInfo {
				active: true,
				fading: false,
				released: false,
				started: self.clock,
			},
			note: pitch,
			zone,
			head: Playhead::start(&self.rt[zone], self.zones[zone].reverse),
			zone_env: ZoneEnv::trigger(&self.zones[zone], sr),
			kill: KillFade::default(),
			amp: Adsr::default(),
			filter_env: Adsr::default(),
			pitch: start_pitch,
			target: f32::from(pitch),
			velocity,
			vel_gain: velocity_gain(
				velocity,
				p.get(P_VELOCITY) * 0.01,
				p.get(P_VELOCITY_CURVE) * 0.01,
			),
			ratio: 1.0,
			counter: 0,
			svf: [Svf::default(); 2],
			coeffs: SvfCoeffs::default(),
			pedal: false,
			elapsed: 0,
		};
		self.set_envelopes(&mut v);
		v.amp.gate_on();
		v.filter_env.gate_on();
		self.voices[slot] = v;
	}

	fn note_off(&mut self, pitch: u8) {
		if self.one_shot() {
			return;
		}
		let sustain = self.expr.sustain;
		for v in self.voices.iter_mut() {
			if v.slot.active && !v.slot.released && v.note == pitch {
				if sustain {
					v.pedal = true;
					continue;
				}
				v.release();
				v.amp.gate_off();
				v.filter_env.gate_off();
			}
		}
	}

	fn pedal_up(&mut self) {
		for v in self.voices.iter_mut() {
			if v.slot.active && !v.slot.released && v.pedal {
				v.release();
				v.amp.gate_off();
				v.filter_env.gate_off();
			}
			v.pedal = false;
		}
	}

	fn all_notes_off(&mut self) {
		let sr = self.sample_rate;
		for v in self.voices.iter_mut().filter(|v| v.slot.active) {
			v.kill.start(ALL_OFF_MS, sr);
			v.slot.fading = true;
			v.release();
		}
	}

	/// Control-rate update: glide, playback ratio and filter coefficients.
	fn control(&self, v: &mut Voice) {
		let p = &self.params;
		let glide = p.get(P_GLIDE);
		if glide > 0.0 {
			let k = one_pole_coeff(glide, self.sample_rate / CONTROL as f32);
			v.pitch += (v.target - v.pitch) * k;
			if (v.target - v.pitch).abs() < 1e-3 {
				v.pitch = v.target;
			}
		} else {
			v.pitch = v.target;
		}
		let zone = &self.zones[v.zone];
		let semis = v.pitch - f32::from(zone.root_key)
			+ zone.tune
			+ p.get(P_TRANSPOSE)
			+ self.pitch_offset
			+ self.expr.note_bend(v.note);
		v.ratio = 2.0_f64.powf(f64::from(semis) / 12.0) * f64::from(self.rt[v.zone].sample_rate)
			/ f64::from(self.sample_rate);
		if p.choice(P_FILTER_TYPE) != 0 {
			let offset = v.filter_env.level() * p.get(P_FILTER_ENV_AMOUNT)
				+ v.velocity * 48.0 * p.get(P_FILTER_VELOCITY) * 0.01
				+ (v.pitch - 60.0) * p.get(P_FILTER_KEY_TRACK) * 0.01
				+ self.expr.pressure * 24.0;
			let cutoff = p.get(P_FILTER_CUTOFF) * 2.0_f32.powf(offset / 12.0);
			v.coeffs = SvfCoeffs::new(cutoff, p.get(P_FILTER_RESONANCE) * 0.01, self.sample_rate);
		}
	}

	fn render(&mut self, left: &mut [f32], right: &mut [f32]) {
		self.pitch_offset = self.expr.pitch_offset(left.len(), self.sample_rate);
		let filter = self.params.choice(P_FILTER_TYPE);
		for i in 0..self.voices.len() {
			if !self.voices[i].slot.active {
				continue;
			}
			let mut v = self.voices[i];
			let zone = v.zone;
			let Some(sample) = self.zones[zone].sample.as_deref() else {
				self.voices[i].slot.active = false;
				continue;
			};
			let rt = self.rt[zone];
			for (l, r) in left.iter_mut().zip(right.iter_mut()) {
				if v.counter == 0 {
					self.control(&mut v);
				}
				v.counter = (v.counter + 1) % CONTROL;
				let amp = v.amp.tick();
				v.filter_env.tick();
				let (Some(kill), Some(zenv), true) =
					(v.kill.tick(), v.zone_env.tick(), v.amp.is_active())
				else {
					v.slot.active = false;
					break;
				};
				let Some((mut a, mut b)) = v.head.next(sample, &rt, v.ratio) else {
					v.slot.active = false;
					break;
				};
				if filter != 0 {
					let [fl, fr] = &mut v.svf;
					let (ol, or) = (fl.process(&v.coeffs, a), fr.process(&v.coeffs, b));
					(a, b) = match filter {
						1 => (ol.low, or.low),
						2 => (ol.high, or.high),
						_ => (ol.band, or.band),
					};
				}
				let g = amp * zenv * kill * v.vel_gain;
				*l += a * rt.gain_l * g;
				*r += b * rt.gain_r * g;
			}
			v.elapsed = v.elapsed.saturating_add(left.len() as u32);
			self.voices[i] = v;
		}
		for (l, r) in left.iter_mut().zip(right.iter_mut()) {
			let g = self.gain.tick();
			*l *= g;
			*r *= g;
		}
	}
}

impl Processor for Sampler {
	fn info(&self) -> &'static DeviceInfo {
		&INFO
	}

	fn prepare(&mut self, sample_rate: f32, _max_block: usize) {
		self.sample_rate = sample_rate.max(1.0);
		self.gain.prepare(self.sample_rate, 15.0);
		self.reset();
	}

	fn set_param(&mut self, index: usize, value: f32) {
		let Some(v) = self.params.set(&INFO, index, value) else {
			return;
		};
		match index {
			P_GAIN => self.gain.set_target(db_to_gain(v)),
			P_AMP_ATTACK..=P_AMP_RELEASE | P_FILTER_ATTACK..=P_FILTER_RELEASE => {
				for i in 0..self.voices.len() {
					if self.voices[i].slot.active {
						let mut voice = self.voices[i];
						self.set_envelopes(&mut voice);
						self.voices[i] = voice;
					}
				}
			}
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
		left[..len].fill(0.0);
		right[..len].fill(0.0);
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
				}) if velocity > 0.0 => self.note_on(pitch, velocity),
				Some(&MidiEvent::NoteOn { pitch, .. })
				| Some(&MidiEvent::NoteOff { pitch, .. }) => self.note_off(pitch),
				Some(ev @ MidiEvent::AllNotesOff { .. }) => {
					self.expr.apply(ev);
					self.all_notes_off();
				}
				Some(ev) => match self.expr.apply(ev) {
					ExpressionAction::PedalUp => self.pedal_up(),
					ExpressionAction::AllNotesOff => self.all_notes_off(),
					ExpressionAction::None => {}
				},
				None => {}
			}
		}
	}

	fn reset(&mut self) {
		for v in self.voices.iter_mut() {
			*v = Voice::default();
		}
		self.gain.snap();
		self.last_pitch = None;
		self.rr = [0; 17];
		self.held.clear();
		self.expr.reset_controllers();
	}

	fn tail_seconds(&self) -> f32 {
		let release = self.params.get(P_AMP_RELEASE) * 0.0014 + 0.05;
		let one_shot = if self.one_shot() { self.longest } else { 0.0 };
		(release + one_shot).min(30.0)
	}

	fn readings(&self, out: &mut [f32]) -> usize {
		let mut voices = NewestVoices::default();
		for v in self
			.voices
			.iter()
			.filter(|v| v.slot.active && !v.slot.fading)
		{
			let Some(rt) = self.rt.get(v.zone) else {
				continue;
			};
			voices.offer(
				v.slot.started,
				VoiceReading {
					note: v.note,
					zone: Some(v.zone),
					position: Some(v.head.seconds(rt)),
					released: v.slot.released,
					elapsed_ms: voice_readings::samples_to_ms(v.elapsed, self.sample_rate),
					level: v.amp.level(),
				},
			);
		}
		voice_readings::write(out, &self.held, &voices)
	}

	fn set_zones(&mut self, zones: Vec<Zone>) -> bool {
		for v in self.voices.iter_mut() {
			*v = Voice::default();
		}
		self.rt = zones.iter().map(ZoneRt::new).collect();
		self.longest = self
			.rt
			.iter()
			.zip(&zones)
			.filter(|(rt, _)| rt.looping.is_none())
			// Room for notes played up to an octave below the root.
			.map(|(rt, z)| {
				let down = 2.0_f64.powf(f64::from((-z.tune).max(0.0)) / 12.0);
				(rt.region_seconds() * 2.0 * down) as f32
			})
			.fold(0.0, f32::max);
		self.zones = zones;
		self.rr = [0; 17];
		true
	}
}

#[cfg(test)]
mod tests {
	use std::sync::Arc;

	use super::*;
	use crate::sample::{LoopMode, SampleBuffer, SampleData};

	const SR: f32 = 48_000.0;

	fn sine(freq: f32, sr: f32, seconds: f32) -> Arc<dyn SampleData> {
		let frames = (sr * seconds) as usize;
		SampleBuffer::mono(
			sr,
			(0..frames)
				.map(|n| (std::f32::consts::TAU * freq * n as f32 / sr).sin() * 0.5)
				.collect(),
		)
		.shared()
	}

	fn sampler(zones: Vec<Zone>) -> Sampler {
		let mut s = Sampler::new();
		s.prepare(SR, 256);
		s.set_param(P_VELOCITY, 0.0);
		assert!(s.set_zones(zones));
		s
	}

	fn on(pitch: u8, offset: u32) -> MidiEvent {
		MidiEvent::NoteOn {
			offset,
			pitch,
			velocity: 1.0,
		}
	}

	/// Renders `frames` (256-frame blocks), `events` given as (frame, event).
	fn run(s: &mut Sampler, events: &[(usize, MidiEvent)], frames: usize) -> Vec<f32> {
		let mut out = Vec::new();
		while out.len() < frames {
			let at = out.len();
			let evs: Vec<MidiEvent> = events
				.iter()
				.filter(|(f, _)| (at..at + 256).contains(f))
				.map(|(f, e)| e.with_offset((f - at) as u32))
				.collect();
			let mut l = [0.0f32; 256];
			let mut r = [0.0f32; 256];
			s.process(&mut l, &mut r, &evs, &Transport::default());
			out.extend_from_slice(&l);
		}
		out
	}

	/// Frequency from rising zero crossings (linear interpolation between samples).
	fn frequency(x: &[f32]) -> f32 {
		let mut first = None;
		let mut last = 0.0;
		let mut count = 0;
		for i in 1..x.len() {
			if x[i - 1] < 0.0 && x[i] >= 0.0 {
				let t = (i - 1) as f32 + x[i - 1] / (x[i - 1] - x[i]);
				if first.is_none() {
					first = Some(t);
				} else {
					count += 1;
				}
				last = t;
			}
		}
		count as f32 * SR / (last - first.unwrap_or(0.0))
	}

	fn max_jump(x: &[f32]) -> f32 {
		x.windows(2)
			.map(|w| (w[1] - w[0]).abs())
			.fold(0.0, f32::max)
	}

	#[test]
	fn transposition_is_pitch_accurate() {
		// Root A4 recorded at 44.1 kHz, played on a 48 kHz device.
		for (note, want) in [
			(69u8, 440.0f32),
			(81, 880.0),
			(76, 659.255),
			(57, 220.0),
			(93, 1760.0),
		] {
			let mut s = sampler(vec![Zone::new(sine(440.0, 44_100.0, 2.0), 0, 127, 69)]);
			let out = run(&mut s, &[(0, on(note, 0))], 24_000);
			let got = frequency(&out[2_000..]);
			assert!(
				(got / want - 1.0).abs() < 0.002,
				"note {note}: {got} Hz, want {want}"
			);
		}
		// Fine tune: +0.5 semitone.
		let mut z = Zone::new(sine(440.0, SR, 2.0), 0, 127, 69);
		z.tune = 0.5;
		let mut s = sampler(vec![z]);
		let got = frequency(&run(&mut s, &[(0, on(69, 0))], 24_000)[2_000..]);
		assert!((got / 452.893 - 1.0).abs() < 0.002, "{got}");
	}

	#[test]
	fn loops_sustain_without_clicks() {
		// 100.3 Hz never fits the loop exactly: without a crossfade the seam would jump.
		for (mode, xf) in [(LoopMode::Forward, 0.02), (LoopMode::PingPong, 0.0)] {
			let mut z = Zone::new(sine(100.3, SR, 0.5), 0, 127, 60);
			z.loop_mode = mode;
			z.loop_start = 0.1;
			z.loop_end = Some(0.4);
			z.loop_crossfade = xf;
			let mut s = sampler(vec![z]);
			let out = run(&mut s, &[(0, on(60, 0))], (SR * 2.0) as usize);
			let tail = &out[(SR * 1.5) as usize..];
			assert!(tail.iter().any(|v| v.abs() > 0.3), "{mode:?} stopped");
			// A 100 Hz sine at 0.5 moves at most ~0.0066 per sample.
			assert!(
				max_jump(&out[1_000..]) < 0.012,
				"{mode:?} click {}",
				max_jump(&out)
			);
		}
	}

	#[test]
	fn amp_envelope_timing() {
		let dc = SampleBuffer::mono(SR, vec![0.5; SR as usize * 3]).shared();
		let mut s = sampler(vec![Zone::new(dc, 0, 127, 60)]);
		s.set_param(P_AMP_ATTACK, 100.0);
		s.set_param(P_AMP_RELEASE, 200.0);
		let off = MidiEvent::NoteOff {
			offset: 0,
			pitch: 60,
		};
		let out = run(&mut s, &[(0, on(60, 0)), (24_000, off)], 48_000);
		// Linear attack: half way at 50 ms, full by 100 ms.
		assert!((out[2_400] - 0.25).abs() < 0.02, "{}", out[2_400]);
		assert!((out[4_900] - 0.5).abs() < 0.01);
		// Release: -60 dB about 200 ms after note-off.
		let at = |ms: f32| out[24_000 + (ms * 48.0) as usize];
		assert!(
			at(100.0) < 0.5 * 0.05 && at(100.0) > 0.5 * 0.01,
			"{}",
			at(100.0)
		);
		assert!(at(230.0) < 0.5 * 0.001);
	}

	#[test]
	fn stealing_fades_the_oldest_voice() {
		let dc = SampleBuffer::mono(SR, vec![0.25; SR as usize]).shared();
		let mut s = sampler(vec![Zone::new(dc, 0, 127, 60)]);
		s.set_param(P_POLYPHONY, 2.0);
		let out = run(
			&mut s,
			&[(0, on(60, 0)), (1_000, on(62, 0)), (2_000, on(64, 0))],
			6_000,
		);
		assert!((out[1_500] - 0.5).abs() < 0.01);
		// Third note: one voice fades out while the new one starts — still two voices.
		assert!((out[3_000] - 0.5).abs() < 0.01, "{}", out[3_000]);
		assert!(max_jump(&out[1_001..]) <= 0.25 + 1e-3, "{}", max_jump(&out));
		assert_eq!(s.active_voices(), 2);
	}

	#[test]
	fn velocity_layers_pick_their_sample_and_one_shot_ignores_note_off() {
		let low = SampleBuffer::mono(SR, vec![0.1; 9_600]).shared();
		let high = SampleBuffer::mono(SR, vec![0.4; 9_600]).shared();
		let mut soft = Zone::new(low, 0, 127, 60);
		soft.vel_high = 0.5;
		let mut hard = Zone::new(high, 0, 127, 60);
		hard.vel_low = 0.51;
		let mut s = sampler(vec![soft, hard]);
		s.set_param(P_MODE, 1.0);
		let quiet = MidiEvent::NoteOn {
			offset: 0,
			pitch: 60,
			velocity: 0.3,
		};
		let off = MidiEvent::NoteOff {
			offset: 0,
			pitch: 60,
		};
		let out = run(&mut s, &[(0, quiet), (100, off)], 1_024);
		assert!((out[800] - 0.1).abs() < 1e-3, "{}", out[800]);
		s.reset();
		let out = run(&mut s, &[(0, on(60, 0))], 1_024);
		assert!((out[800] - 0.4).abs() < 1e-3);
	}

	#[test]
	fn legato_glides_without_retriggering() {
		let mut s = sampler(vec![Zone::new(sine(440.0, SR, 3.0), 0, 127, 69)]);
		s.set_param(P_LEGATO, 1.0);
		s.set_param(P_GLIDE, 50.0);
		let out = run(&mut s, &[(0, on(69, 0)), (24_000, on(81, 0))], 72_000);
		assert_eq!(s.active_voices(), 1);
		let got = frequency(&out[48_000..]);
		assert!((got / 880.0 - 1.0).abs() < 0.003, "{got}");
		// No restart: the level never drops to zero at the second note.
		assert!(out[23_000..26_000].iter().any(|v| v.abs() > 0.4));
	}

	#[test]
	fn low_pass_filter_darkens() {
		let bright = |filter: f32| {
			let mut s = sampler(vec![Zone::new(sine(5_000.0, SR, 1.0), 0, 127, 60)]);
			s.set_param(P_FILTER_TYPE, filter);
			s.set_param(P_FILTER_CUTOFF, 500.0);
			let out = run(&mut s, &[(0, on(60, 0))], 12_000);
			out[4_000..].iter().fold(0.0f32, |m, v| m.max(v.abs()))
		};
		assert!(bright(0.0) > 0.45);
		assert!(bright(1.0) < 0.05);
		assert!(bright(2.0) > 0.4);
	}

	#[test]
	fn filter_response_matches_voice_output() {
		let pitch = 69u8;
		let freq = 440.0;
		for &(filter_type, cutoff, resonance) in
			&[(1.0, 1_000.0, 20.0), (2.0, 300.0, 50.0), (3.0, 800.0, 70.0)]
		{
			let mut s = sampler(vec![Zone::new(sine(freq, SR, 2.0), 0, 127, pitch)]);
			s.set_param(P_FILTER_TYPE, filter_type);
			s.set_param(P_FILTER_CUTOFF, cutoff);
			s.set_param(P_FILTER_RESONANCE, resonance);
			s.set_param(P_AMP_ATTACK, 0.0);
			s.set_param(P_AMP_SUSTAIN, 100.0);
			let out = run(&mut s, &[(0, on(pitch, 0))], 96_000);
			let settled = &out[48_000..];
			let sum_sq: f64 = settled.iter().map(|&x| (x as f64) * (x as f64)).sum();
			let rms = (sum_sq / settled.len() as f64).sqrt() as f32;
			// The source sine has amplitude 0.5 (RMS 0.5/sqrt(2)); with
			// velocity sensitivity and zone gain/pan all neutral the voice
			// otherwise passes it through unchanged.
			let input_rms = 0.5 * std::f32::consts::FRAC_1_SQRT_2;
			let measured = crate::util::gain_to_db(rms / input_rms);
			let formula =
				sampler_filter_response_db(filter_type as usize, cutoff, resonance, SR, freq);
			assert!(
				(formula - measured).abs() < 0.5,
				"filter {filter_type} cutoff {cutoff} res {resonance}: formula {formula}, measured {measured}"
			);
		}
	}
}
