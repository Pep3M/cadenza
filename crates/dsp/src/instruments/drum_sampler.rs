//! `drum_sampler`: FL-style pad sampler (FPC / drum rack) on General MIDI drum notes.

use super::sample_voice::{
	KillFade, Playhead, SlotInfo, ZoneEnv, ZoneRt, pick_slot, select_zones, velocity_gain,
};
use crate::processor::{
	DeviceInfo, DeviceRole, MidiEvent, ParamInfo, Processor, Transport, readings,
};
use crate::sample::Zone;
use crate::util::smoother::Smoother;
use crate::util::{ParamValues, db_to_gain, event_spans};

pub const P_GAIN: usize = 0;
pub const P_VELOCITY: usize = 1;
pub const P_POLYPHONY: usize = 2;
pub const P_TUNE: usize = 3;
pub const N_PARAMS: usize = 4;

/// Extra stereo outputs pads can be routed to (`Zone::output` 1..=AUX_OUTPUTS).
pub const AUX_OUTPUTS: usize = 8;

/// The 16 pads of the editor's 4x4 grid (pad 1 bottom-left, like FPC/MPC), each on its
/// General MIDI drum note. Zones may sit on any other note too.
pub const PADS: [(u8, &str); 16] = [
	(36, "Kick"),
	(38, "Snare"),
	(42, "Closed Hat"),
	(46, "Open Hat"),
	(39, "Clap"),
	(37, "Rim"),
	(44, "Pedal Hat"),
	(49, "Crash"),
	(41, "Low Tom"),
	(45, "Mid Tom"),
	(48, "High Tom"),
	(51, "Ride"),
	(35, "Kick 2"),
	(40, "Snare 2"),
	(54, "Tambourine"),
	(56, "Cowbell"),
];

/// GM name of a pad note, if it is one of [`PADS`].
pub fn pad_name(note: u8) -> Option<&'static str> {
	PADS.iter().find(|(n, _)| *n == note).map(|(_, name)| *name)
}

static PARAMS: [ParamInfo; N_PARAMS] = [
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
		"velocity_sensitivity",
		"Velocity Sensitivity",
		0.0,
		100.0,
		80.0,
		"%",
		"How much note velocity changes loudness (0 = every hit equally loud).",
	),
	ParamInfo::new(
		"polyphony",
		"Polyphony",
		1.0,
		64.0,
		32.0,
		"voices",
		"Hits that can ring at once across all pads; beyond it the oldest is faded out.",
	),
	ParamInfo::new(
		"tune",
		"Kit Tune",
		-24.0,
		24.0,
		0.0,
		"semitones",
		"Pitch offset of every pad.",
	),
];

pub static INFO: DeviceInfo = DeviceInfo {
	id: "drum_sampler",
	name: "Drum Sampler",
	role: DeviceRole::Instrument,
	description: "Pad-based drum sampler (like FL Studio's FPC): 16 pads on General MIDI \
		drum notes — 36 kick, 38 snare, 42 closed hat, 46 open hat, 39 clap, 37 rim, 44 pedal \
		hat, 49 crash, 41/45/48 low/mid/high tom, 51 ride, 35 kick 2, 40 snare 2, 54 \
		tambourine, 56 cowbell — so write_drum_pattern lanes play it directly. Each pad plays \
		one-shot samples (velocity layers optional) with its own gain, pan, tune, attack/decay, \
		start offset, reverse, choke group (closed hat chokes open hat) and output: main or \
		one of 8 extra stereo outputs to route to mixer buses with set_output_routes. Load \
		pads with load_sample (pad: note or name), shape them with set_pad. A new drum_sampler \
		starts with the factory 'Synth Kit' (synthesized samples) loaded.",
	params: &PARAMS,
};

/// Voice slots: the polyphony maximum plus room for voices fading out.
const SLOTS: usize = 64 + 16;
const STEAL_MS: f32 = 5.0;
const CHOKE_MS: f32 = 4.0;
const ALL_OFF_MS: f32 = 10.0;
/// Release of a pad hit by a new note-on of the same zone (no click on fast rolls).
const RETRIGGER_MS: f32 = 3.0;

#[derive(Clone, Copy, Default)]
struct Voice {
	slot: SlotInfo,
	zone: usize,
	head: Playhead,
	env: ZoneEnv,
	kill: KillFade,
	gain: f32,
	ratio: f64,
}

/// Pad sampler: one-shot voices, choke groups, 8 extra stereo outputs.
pub struct DrumSampler {
	params: ParamValues<N_PARAMS>,
	zones: Vec<Zone>,
	rt: Vec<ZoneRt>,
	voices: Box<[Voice; SLOTS]>,
	sample_rate: f32,
	gain: Smoother,
	clock: u64,
	rr: [u32; 17],
	/// Longest zone (seconds), for the tail.
	longest: f32,
	aux: Vec<[Vec<f32>; 2]>,
	aux_len: usize,
	/// Note-ons per pad of [`PADS`] (editor flashes, [`readings::DRUM_PAD_TRIGGERS_START`]).
	triggers: [f32; readings::DRUM_PADS],
}

impl Default for DrumSampler {
	fn default() -> Self {
		Self::new()
	}
}

impl DrumSampler {
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
			rr: [0; 17],
			longest: 0.0,
			aux: (0..AUX_OUTPUTS).map(|_| [Vec::new(), Vec::new()]).collect(),
			aux_len: 0,
			triggers: [0.0; readings::DRUM_PADS],
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

	fn polyphony(&self) -> usize {
		self.params.get(P_POLYPHONY).round().clamp(1.0, 64.0) as usize
	}

	fn note_on(&mut self, pitch: u8, velocity: f32) {
		let mut chosen = [usize::MAX; 32];
		let mut n = 0;
		select_zones(&self.zones, pitch, velocity, &mut self.rr, |i| {
			if n < chosen.len() {
				chosen[n] = i;
				n += 1;
			}
		});
		let sr = self.sample_rate;
		// A new hit of a pad fades its previous hit (like a real drum, no pile-up).
		for v in self.voices.iter_mut() {
			if v.slot.active && chosen[..n].contains(&v.zone) {
				v.kill.start(RETRIGGER_MS, sr);
				v.slot.fading = true;
			}
		}
		let mut sounded = false;
		for &zone in &chosen[..n] {
			if self.zones[zone].sample.is_some() {
				self.start_voice(zone, pitch, velocity);
				sounded = true;
			}
		}
		if sounded && let Some(pad) = PADS.iter().position(|(note, _)| *note == pitch) {
			self.triggers[pad] += 1.0;
		}
	}

	fn start_voice(&mut self, zone: usize, pitch: u8, velocity: f32) {
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
		let z = &self.zones[zone];
		let rt = &self.rt[zone];
		let semis = z.tune + self.params.get(P_TUNE) + f32::from(pitch) - f32::from(z.root_key);
		let ratio =
			2.0_f64.powf(f64::from(semis) / 12.0) * f64::from(rt.sample_rate) / f64::from(sr);
		self.voices[slot] = Voice {
			slot: SlotInfo {
				active: true,
				fading: false,
				released: true,
				started: self.clock,
			},
			zone,
			head: Playhead::start(rt, z.reverse),
			env: ZoneEnv::trigger(z, sr),
			kill: KillFade::default(),
			gain: velocity_gain(velocity, self.params.get(P_VELOCITY) * 0.01, 0.0),
			ratio,
		};
	}

	fn all_notes_off(&mut self) {
		let sr = self.sample_rate;
		for v in self.voices.iter_mut().filter(|v| v.slot.active) {
			v.kill.start(ALL_OFF_MS, sr);
			v.slot.fading = true;
		}
	}

	/// Renders `start..end` of the block into the main and aux buffers.
	fn render(&mut self, left: &mut [f32], right: &mut [f32], start: usize, end: usize) {
		for i in 0..self.voices.len() {
			if !self.voices[i].slot.active {
				continue;
			}
			let mut v = self.voices[i];
			let Some(sample) = self.zones[v.zone].sample.as_deref() else {
				self.voices[i].slot.active = false;
				continue;
			};
			let rt = self.rt[v.zone];
			let output = usize::from(self.zones[v.zone].output);
			let (out_l, out_r): (&mut [f32], &mut [f32]) = match output {
				0 => (&mut left[start..end], &mut right[start..end]),
				n if n <= AUX_OUTPUTS => {
					let [l, r] = &mut self.aux[n - 1];
					(&mut l[start..end], &mut r[start..end])
				}
				// Outputs this device does not have are silent.
				_ => {
					self.voices[i].slot.active = false;
					continue;
				}
			};
			for (l, r) in out_l.iter_mut().zip(out_r.iter_mut()) {
				let (Some(kill), Some(env)) = (v.kill.tick(), v.env.tick()) else {
					v.slot.active = false;
					break;
				};
				let Some((a, b)) = v.head.next(sample, &rt, v.ratio) else {
					v.slot.active = false;
					break;
				};
				let g = env * kill * v.gain;
				*l += a * rt.gain_l * g;
				*r += b * rt.gain_r * g;
			}
			self.voices[i] = v;
		}
	}
}

impl Processor for DrumSampler {
	fn info(&self) -> &'static DeviceInfo {
		&INFO
	}

	fn prepare(&mut self, sample_rate: f32, max_block: usize) {
		self.sample_rate = sample_rate.max(1.0);
		self.gain.prepare(self.sample_rate, 15.0);
		for [l, r] in &mut self.aux {
			*l = vec![0.0; max_block.max(1)];
			*r = vec![0.0; max_block.max(1)];
		}
		self.aux_len = 0;
		self.reset();
	}

	fn set_param(&mut self, index: usize, value: f32) {
		if let Some(v) = self.params.set(&INFO, index, value)
			&& index == P_GAIN
		{
			self.gain.set_target(db_to_gain(v));
		}
	}

	fn process(
		&mut self,
		left: &mut [f32],
		right: &mut [f32],
		events: &[MidiEvent],
		_transport: &Transport,
	) {
		let len = left
			.len()
			.min(right.len())
			.min(self.aux.first().map_or(0, |a| a[0].len()));
		left.fill(0.0);
		right.fill(0.0);
		for [l, r] in &mut self.aux {
			l[..len].fill(0.0);
			r[..len].fill(0.0);
		}
		self.aux_len = len;
		for (start, end, ev) in event_spans(len, events) {
			self.render(left, right, start, end);
			match ev {
				Some(&MidiEvent::NoteOn {
					pitch, velocity, ..
				}) if velocity > 0.0 => self.note_on(pitch, velocity),
				Some(MidiEvent::AllNotesOff { .. }) => self.all_notes_off(),
				_ => {}
			}
		}
		// The kit gain applies to every output alike.
		for n in 0..len {
			let g = self.gain.tick();
			left[n] *= g;
			right[n] *= g;
			for [l, r] in &mut self.aux {
				l[n] *= g;
				r[n] *= g;
			}
		}
	}

	fn reset(&mut self) {
		for v in self.voices.iter_mut() {
			*v = Voice::default();
		}
		self.gain.snap();
		self.rr = [0; 17];
	}

	fn tail_seconds(&self) -> f32 {
		self.longest.min(30.0)
	}

	fn readings(&self, out: &mut [f32]) -> usize {
		let start = readings::DRUM_PAD_TRIGGERS_START;
		let n = out.len().saturating_sub(start).min(readings::DRUM_PADS);
		out[start..start + n].copy_from_slice(&self.triggers[..n]);
		if n == 0 { 0 } else { start + n }
	}

	fn aux_output_count(&self) -> usize {
		AUX_OUTPUTS
	}

	fn aux_output(&self, index: usize) -> Option<(&[f32], &[f32])> {
		let [l, r] = self.aux.get(index)?;
		Some((&l[..self.aux_len], &r[..self.aux_len]))
	}

	fn set_zones(&mut self, zones: Vec<Zone>) -> bool {
		for v in self.voices.iter_mut() {
			*v = Voice::default();
		}
		self.rt = zones.iter().map(ZoneRt::new).collect();
		let kit_tune = self.params.get(P_TUNE).min(0.0);
		self.longest = self
			.rt
			.iter()
			.zip(&zones)
			.map(|(rt, z)| {
				let lowest = f32::from(z.key_low) - f32::from(z.root_key);
				let semis = f64::from(z.tune + kit_tune + lowest).min(0.0);
				let region = rt.region_seconds() / 2.0_f64.powf(semis / 12.0);
				let decay = z.decay_ms.map_or(f64::MAX, |d| f64::from(d) * 0.0015);
				(region.min(decay) + f64::from(z.attack_ms) * 0.001) as f32
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
	use crate::sample::SampleBuffer;

	const SR: f32 = 48_000.0;

	fn noise(seconds: f32) -> Arc<dyn crate::sample::SampleData> {
		let mut rng = crate::util::Rng::new(7);
		let frames = (seconds * SR) as usize;
		SampleBuffer::mono(SR, (0..frames).map(|_| rng.bipolar() * 0.5).collect()).shared()
	}

	fn pad(note: u8, sample: Arc<dyn crate::sample::SampleData>) -> Zone {
		Zone::new(sample, note, note, note)
	}

	fn kit(zones: Vec<Zone>) -> DrumSampler {
		let mut d = DrumSampler::new();
		d.prepare(SR, 256);
		assert!(d.set_zones(zones));
		d
	}

	fn hit(pitch: u8, offset: u32) -> MidiEvent {
		MidiEvent::NoteOn {
			offset,
			pitch,
			velocity: 1.0,
		}
	}

	/// Renders `blocks` x 256 frames; returns (main L, aux outputs L).
	fn run(
		d: &mut DrumSampler,
		events: &[(usize, MidiEvent)],
		blocks: usize,
	) -> (Vec<f32>, Vec<Vec<f32>>) {
		let mut main = Vec::new();
		let mut aux = vec![Vec::new(); AUX_OUTPUTS];
		for b in 0..blocks {
			let evs: Vec<MidiEvent> = events
				.iter()
				.filter(|(i, _)| *i == b)
				.map(|(_, e)| *e)
				.collect();
			let mut l = [0.0f32; 256];
			let mut r = [0.0f32; 256];
			d.process(&mut l, &mut r, &evs, &Transport::default());
			main.extend_from_slice(&l);
			for (i, a) in aux.iter_mut().enumerate() {
				a.extend_from_slice(d.aux_output(i).unwrap().0);
			}
		}
		(main, aux)
	}

	fn rms(x: &[f32]) -> f32 {
		(x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt()
	}

	#[test]
	fn closed_hat_chokes_open_hat() {
		// A steady open hat makes any click visible as a jump.
		let dc = SampleBuffer::mono(SR, vec![0.5; SR as usize]).shared();
		let mut open = pad(46, dc);
		open.choke = 1;
		let mut closed = pad(42, SampleBuffer::mono(SR, vec![0.0; 480]).shared());
		closed.choke = 1;
		let mut d = kit(vec![open.clone()]);
		let (alone, _) = run(&mut d, &[(0, hit(46, 0))], 40);
		let mut d = kit(vec![open, closed]);
		let (choked, _) = run(&mut d, &[(0, hit(46, 0)), (10, hit(42, 0))], 40);
		assert!(rms(&alone[30 * 256..]) > 0.3);
		assert!(rms(&choked[30 * 256..]) < 1e-6);
		let jump = choked[256..]
			.windows(2)
			.map(|w| (w[1] - w[0]).abs())
			.fold(0.0, f32::max);
		assert!(jump < 0.01, "click {jump}");
	}

	#[test]
	fn pads_route_to_their_outputs_and_decay() {
		let mut snare = pad(38, noise(1.0));
		snare.output = 3;
		snare.decay_ms = Some(50.0);
		let mut d = kit(vec![pad(36, noise(0.5)), snare]);
		let (main, aux) = run(&mut d, &[(0, hit(38, 0))], 20);
		assert_eq!(rms(&main), 0.0, "snare is not on the main output");
		assert!(rms(&aux[2][..2048]) > 0.05);
		assert!(aux.iter().enumerate().all(|(i, a)| i == 2 || rms(a) == 0.0));
		// 50 ms decay: silent well before the sample ends.
		assert!(rms(&aux[2][15 * 256..]) < 1e-3);
		let (main, aux) = run(&mut d, &[(0, hit(36, 0))], 4);
		assert!(rms(&main) > 0.05 && rms(&aux[2]) == 0.0);
		assert!(d.tail_seconds() > 0.4 && d.tail_seconds() < 1.1);
	}

	#[test]
	fn note_off_does_not_stop_one_shots_and_missing_samples_are_silent() {
		let missing = Zone {
			key_low: 40,
			key_high: 40,
			..Zone::default()
		};
		let mut d = kit(vec![pad(36, noise(0.5)), missing]);
		let events = [
			(0, hit(36, 0)),
			(
				0,
				MidiEvent::NoteOff {
					offset: 10,
					pitch: 36,
				},
			),
			(0, hit(40, 20)),
		];
		let (main, _) = run(&mut d, &events, 12);
		assert!(rms(&main[2048..]) > 0.1, "kept playing after note-off");
	}

	#[test]
	fn note_ons_count_per_pad() {
		let mut d = kit(vec![
			pad(36, noise(0.1)),
			pad(38, noise(0.1)),
			pad(60, noise(0.1)),
		]);
		let counters = |d: &DrumSampler| {
			let mut out = [0.0; readings::MAX];
			assert_eq!(d.readings(&mut out), readings::DRUM_PADS);
			out
		};
		assert!(counters(&d).iter().all(|c| *c == 0.0));
		// Two kicks, a snare, a note on no pad, a pad without a sample (closed hat).
		run(
			&mut d,
			&[
				(0, hit(36, 0)),
				(0, hit(38, 10)),
				(1, hit(36, 0)),
				(1, hit(60, 5)),
				(1, hit(42, 9)),
			],
			2,
		);
		let c = counters(&d);
		let at = |note: u8| PADS.iter().position(|(n, _)| *n == note).unwrap();
		assert_eq!(c[at(36)], 2.0);
		assert_eq!(c[at(38)], 1.0);
		assert_eq!(c.iter().sum::<f32>(), 3.0, "{c:?}");
		// Reset and new zones keep counting up (readers compare with their last value).
		d.reset();
		assert_eq!(counters(&d)[at(36)], 2.0);
	}

	#[test]
	fn reverse_and_start_offset() {
		let ramp: Vec<f32> = (0..4800).map(|i| i as f32 / 4800.0).collect();
		let s = SampleBuffer::mono(SR, ramp).shared();
		let mut z = pad(36, s);
		z.reverse = true;
		let mut d = kit(vec![z.clone()]);
		d.set_param(P_VELOCITY, 0.0);
		let (main, _) = run(&mut d, &[(0, hit(36, 0))], 2);
		assert!(main[1] > main[100], "reversed ramp falls");
		z.reverse = false;
		z.start = 0.05;
		let mut d = kit(vec![z]);
		d.set_param(P_VELOCITY, 0.0);
		let (main, _) = run(&mut d, &[(0, hit(36, 0))], 1);
		assert!((main[0] - 0.5).abs() < 0.01, "starts half-way: {}", main[0]);
	}
}
