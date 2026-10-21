//! Modulators of a built-in device (LFOs, envelope followers; `cadenza_model::modulation`)
//! running on the audio thread.
//!
//! Every quantum ([`QUANTUM`] samples) each modulator produces one value (LFO: -1..1 at
//! the quantum start; envelope: 0..1 after following the quantum's signal) and every
//! modulated parameter gets `base + sum(depth * value) * (max - min)`, clamped to its
//! range, through `Processor::set_param`. `base` is the parameter's value as set or
//! automated: the graph's parameter writes for a modulated parameter only move `base`.
//!
//! Zipper / smoothing: no extra smoothing here. A quantum is 32 samples (0.7 ms at
//! 48 kHz, a 1.5 kHz control rate), far above LFO rates, and the built-ins already
//! smooth their parameters toward each new value, so the per-quantum steps are inaudible.
//! Square and random shapes step on purpose and get softened by that same smoothing.
//!
//! Everything is allocated at compile time; [`Modulation::apply`] and
//! [`Modulation::follow`] are allocation and lock free. Rate / depth edits recompile
//! (they are structural in `live::diff`): modulator settings are rarely dragged
//! continuously and the recompile keeps the phase deterministic.

use cadenza_dsp::{DeviceInfo, MidiEvent, Processor, Transport};
use cadenza_model::modulation::MAX_MODULATORS;
use cadenza_model::{EnvelopeSource, LfoShape, Modulator, ModulatorKind, Retrigger};

use crate::QUANTUM;

/// The value of an LFO of `shape` at `cycles` (cycles since its start, any real; the
/// fractional part is the phase). -1..1. `seed` picks the random sequence.
pub fn lfo_value(shape: LfoShape, cycles: f64, seed: u64) -> f32 {
	let p = cycles - cycles.floor();
	match shape {
		LfoShape::Sine => (p * std::f64::consts::TAU).sin() as f32,
		LfoShape::Triangle => {
			let v = if p < 0.25 {
				4.0 * p
			} else if p < 0.75 {
				2.0 - 4.0 * p
			} else {
				4.0 * p - 4.0
			};
			v as f32
		}
		LfoShape::Saw => (2.0 * p - 1.0) as f32,
		LfoShape::Square => {
			if p < 0.5 {
				1.0
			} else {
				-1.0
			}
		}
		LfoShape::Random => {
			let cycle = cycles.floor() as i64 as u64;
			let h = splitmix(seed ^ cycle.wrapping_mul(0x9E37_79B9_7F4A_7C15));
			((h >> 40) as f32 / (1u64 << 24) as f32) * 2.0 - 1.0
		}
	}
}

fn splitmix(mut z: u64) -> u64 {
	z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
	z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
	z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
	z ^ (z >> 31)
}

/// Envelope level (linear peak) -> 0..1 over a -60..0 dBFS window after `gain_db`.
pub fn envelope_value(level: f32, gain_db: f32) -> f32 {
	if level <= 1e-6 {
		return 0.0;
	}
	((20.0 * level.log10() + gain_db + 60.0) / 60.0).clamp(0.0, 1.0)
}

enum Kind {
	Lfo {
		shape: LfoShape,
		rate_hz: f64,
		sync_beats: Option<f64>,
		start: f64,
		retrigger: Retrigger,
		/// Cycles since the start (free / note modes).
		cycles: f64,
		seed: u64,
	},
	Envelope {
		sidechain: bool,
		attack: f32,
		release: f32,
		gain_db: f32,
		level: f32,
	},
}

struct State {
	kind: Kind,
	value: f32,
}

struct Target {
	index: usize,
	min: f32,
	max: f32,
	base: f32,
	last: f32,
	/// (modulator index, depth).
	links: Vec<(usize, f32)>,
}

/// The modulators of one device and the parameters they drive.
pub(crate) struct Modulation {
	states: Vec<State>,
	targets: Vec<Target>,
	sample_rate: f64,
	/// An instrument: "own input" follows its output (the input is silence).
	instrument: bool,
	needs_key: bool,
}

impl Modulation {
	/// Off the audio thread. `params` are the device's explicit values (bases); unknown
	/// target params are skipped and reported through `warn`.
	pub fn new(
		modulators: &[Modulator],
		info: &'static DeviceInfo,
		params: &std::collections::BTreeMap<String, f32>,
		sample_rate: f64,
		instrument: bool,
		mut warn: impl FnMut(&str),
	) -> Option<Self> {
		let mut states = Vec::new();
		let mut targets: Vec<Target> = Vec::new();
		let mut needs_key = false;
		for m in modulators.iter().take(MAX_MODULATORS) {
			let mi = states.len();
			let kind = match &m.kind {
				ModulatorKind::Lfo {
					shape,
					rate_hz,
					sync_beats,
					phase,
					retrigger,
				} => Kind::Lfo {
					shape: *shape,
					rate_hz: f64::from(*rate_hz),
					sync_beats: sync_beats.map(f64::from),
					start: f64::from(*phase),
					retrigger: *retrigger,
					cycles: f64::from(*phase),
					seed: m.id.0.as_u64_pair().0 ^ m.id.0.as_u64_pair().1,
				},
				ModulatorKind::Envelope {
					source,
					attack_ms,
					release_ms,
					gain_db,
				} => {
					let sidechain = *source == EnvelopeSource::Sidechain;
					needs_key |= sidechain;
					Kind::Envelope {
						sidechain,
						attack: coef(*attack_ms, sample_rate),
						release: coef(*release_ms, sample_rate),
						gain_db: *gain_db,
						level: 0.0,
					}
				}
			};
			states.push(State { kind, value: 0.0 });
			for t in &m.targets {
				let Some(index) = info.param_index(&t.param) else {
					warn(&t.param);
					continue;
				};
				match targets.iter_mut().find(|x| x.index == index) {
					Some(x) => x.links.push((mi, t.depth)),
					None => {
						let p = &info.params[index];
						targets.push(Target {
							index,
							min: p.min,
							max: p.max,
							base: params.get(&t.param).copied().unwrap_or(p.default),
							last: f32::NAN,
							links: {
								let mut v = Vec::with_capacity(MAX_MODULATORS);
								v.push((mi, t.depth));
								v
							},
						});
					}
				}
			}
		}
		(!states.is_empty()).then_some(Self {
			states,
			targets,
			sample_rate,
			instrument,
			needs_key,
		})
	}

	pub fn needs_key(&self) -> bool {
		self.needs_key
	}

	/// A write to parameter `index`: moves its base when modulated. Returns false when
	/// the parameter is not modulated (the caller sets it directly).
	pub fn set_base(&mut self, index: usize, value: f32) -> bool {
		match self.targets.iter_mut().find(|t| t.index == index) {
			Some(t) => {
				t.base = value;
				true
			}
			None => false,
		}
	}

	pub fn reset(&mut self) {
		for s in &mut self.states {
			s.value = 0.0;
			match &mut s.kind {
				Kind::Lfo { cycles, start, .. } => *cycles = *start,
				Kind::Envelope { level, .. } => *level = 0.0,
			}
		}
		for t in &mut self.targets {
			t.last = f32::NAN;
		}
	}

	/// Feeds envelope followers that listen to `l`/`r` (`key`: the sidechain key, else
	/// the device's own signal). Realtime safe.
	pub fn follow(&mut self, l: &[f32], r: &[f32], key: bool) {
		for s in &mut self.states {
			if let Kind::Envelope {
				sidechain,
				attack,
				release,
				gain_db,
				level,
			} = &mut s.kind
				&& *sidechain == key
			{
				let mut env = *level;
				for (a, b) in l.iter().zip(r) {
					let x = a.abs().max(b.abs());
					let c = if x > env { *attack } else { *release };
					env = x + (env - x) * c;
				}
				// Denormal guard.
				*level = if env < 1e-9 { 0.0 } else { env };
				s.value = envelope_value(*level, *gain_db);
			}
		}
	}

	/// Whether own-signal followers listen to the output (instruments) instead of input.
	pub fn follows_output(&self) -> bool {
		self.instrument
	}

	/// Advances the LFOs by one quantum and writes every modulated parameter. Realtime
	/// safe.
	pub fn apply(&mut self, processor: &mut dyn Processor, events: &[MidiEvent], t: &Transport) {
		let note_on = events.iter().any(|e| matches!(e, MidiEvent::NoteOn { .. }));
		let step = QUANTUM as f64 / self.sample_rate;
		for s in &mut self.states {
			if let Kind::Lfo {
				shape,
				rate_hz,
				sync_beats,
				start,
				retrigger,
				cycles,
				seed,
			} = &mut s.kind
			{
				let hz = match sync_beats {
					Some(b) => t.bpm / 60.0 / *b,
					None => *rate_hz,
				};
				if *retrigger == Retrigger::Note && note_on {
					*cycles = *start;
				}
				let now = if *retrigger == Retrigger::Transport && t.playing {
					let pos = match sync_beats {
						Some(b) => t.beat_position / *b,
						None => t.beat_position * 60.0 / t.bpm.max(1.0) * *rate_hz,
					};
					*cycles = *start + pos;
					*cycles
				} else {
					let c = *cycles;
					*cycles += hz * step;
					c
				};
				s.value = lfo_value(*shape, now, *seed);
			}
		}
		for tg in &mut self.targets {
			let mut m = 0.0;
			for &(i, depth) in &tg.links {
				m += depth * self.states[i].value;
			}
			let v = (tg.base + m * (tg.max - tg.min)).clamp(tg.min, tg.max);
			if v != tg.last {
				tg.last = v;
				processor.set_param(tg.index, v);
			}
		}
	}

	/// The current value of every modulator, in order (for the editor's rings).
	pub fn values(&self, out: &mut [f32]) -> usize {
		let n = self.states.len().min(out.len());
		for (o, s) in out.iter_mut().zip(&self.states) {
			*o = s.value;
		}
		n
	}
}

/// One-pole coefficient for a time constant of `ms`.
fn coef(ms: f32, sample_rate: f64) -> f32 {
	let samples = f64::from(ms.max(0.01)) * 0.001 * sample_rate;
	(-1.0 / samples).exp() as f32
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn shapes_span_minus_one_to_one() {
		for shape in LfoShape::ALL {
			let mut lo = f32::MAX;
			let mut hi = f32::MIN;
			for i in 0..1000 {
				let v = lfo_value(shape, i as f64 / 100.0, 7);
				lo = lo.min(v);
				hi = hi.max(v);
			}
			assert!(lo >= -1.0 && hi <= 1.0, "{shape:?}");
			assert!(hi - lo > 1.5, "{shape:?} spans {lo}..{hi}");
		}
		assert!(lfo_value(LfoShape::Sine, 0.25, 0) > 0.99);
		assert_eq!(lfo_value(LfoShape::Square, 0.75, 0), -1.0);
		// Random holds within a cycle, and repeats for the same seed.
		assert_eq!(
			lfo_value(LfoShape::Random, 3.1, 9),
			lfo_value(LfoShape::Random, 3.9, 9)
		);
		assert_eq!(envelope_value(1.0, 0.0), 1.0);
		assert!((envelope_value(0.001, 0.0) - 0.0).abs() < 1e-6);
		assert!((envelope_value(0.0316, 0.0) - 0.5).abs() < 0.01);
	}
}
