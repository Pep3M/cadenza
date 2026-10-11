//! `reverb`: 8-line feedback delay network (FDN) reverb.
//!
//! Signal flow: mono input → pre-delay → 4 series all-pass diffusers → FDN
//! with a Householder mixing matrix, per-line damping and per-line gains
//! derived from the requested RT60 (so `decay` is the real -60 dB time and
//! `tail_seconds` is accurate) → slowly modulated line lengths to avoid
//! metallic ringing → wet low-cut → mid/side width → dry/wet mix.

use crate::processor::{DeviceInfo, DeviceRole, MidiEvent, ParamInfo, Processor, Transport};
use crate::util::biquad::{Biquad, BiquadCoeffs};
use crate::util::delay_line::{Allpass, DelayLine};
use crate::util::smoother::Smoother;
use crate::util::{PI, ParamValues, TAU, flush};

const P_SIZE: usize = 0;
const P_DECAY: usize = 1;
const P_DAMPING: usize = 2;
const P_PRE_DELAY: usize = 3;
const P_WIDTH: usize = 4;
const P_LOW_CUT: usize = 5;
const P_MIX: usize = 6;
const N_PARAMS: usize = 7;

static PARAMS: [ParamInfo; N_PARAMS] = [
	ParamInfo::new(
		"size",
		"Size",
		0.0,
		100.0,
		50.0,
		"%",
		"Room dimensions (echo density/spacing). 10-30 small room, 40-60 hall/plate, 70-100 large hall.",
	),
	ParamInfo::new(
		"decay",
		"Decay",
		0.2,
		20.0,
		2.0,
		"s",
		"RT60: time for the tail to fall 60 dB. 0.4-1 room, 1.5-3 plate/hall, 4-10 ambient.",
	),
	ParamInfo::new(
		"damping",
		"Damping",
		0.0,
		100.0,
		50.0,
		"%",
		"High-frequency absorption. Higher = darker, warmer tail that sits behind the mix.",
	),
	ParamInfo::new(
		"pre_delay",
		"Pre-Delay",
		0.0,
		250.0,
		20.0,
		"ms",
		"Gap before the reverb starts; 20-60 ms keeps vocals and snares clear and upfront.",
	),
	ParamInfo::new(
		"width",
		"Width",
		0.0,
		100.0,
		100.0,
		"%",
		"Stereo width of the reverb tail (0 = mono).",
	),
	ParamInfo::new(
		"low_cut",
		"Low Cut",
		20.0,
		1_000.0,
		150.0,
		"Hz",
		"High-pass on the wet signal so the reverb doesn't muddy the low end. 150-400 typical.",
	),
	ParamInfo::new(
		"mix",
		"Mix",
		0.0,
		100.0,
		25.0,
		"%",
		"Dry/wet balance. Use 100% when the reverb sits on a send/bus track, 10-30% as an insert.",
	),
];

pub static INFO: DeviceInfo = DeviceInfo {
	id: "reverb",
	name: "Reverb",
	role: DeviceRole::Effect,
	description: "Lush algorithmic stereo reverb (8-line FDN) for space and depth. Best on a send/bus \
		track at mix 100 shared by several instruments; or as an insert with mix 10-30. Recipes — \
		VOCAL PLATE: size 45, decay 1.8, damping 40, pre_delay 30. SNARE ROOM: size 25, decay 0.8, \
		damping 50. BIG HALL (pads/strings): size 80, decay 3.5, damping 55, pre_delay 20. AMBIENT \
		WASH: size 100, decay 9, damping 60. Keep low_cut 150-300 to avoid mud.",
	params: &PARAMS,
};

const LINES: usize = 8;
/// Base line lengths in ms at size factor 1 (mutually prime-ish to avoid
/// coinciding echoes).
const LINE_MS: [f32; LINES] = [31.7, 37.3, 41.9, 47.5, 53.3, 59.9, 67.1, 73.9];
const DIFFUSER_MS: [f32; 4] = [4.77, 3.59, 12.73, 9.31];
const MAX_SIZE: f32 = 1.6;
const MOD_MS: f32 = 0.35;
const CONTROL_BLOCK: usize = 32;
const OUT_SIGNS: [f32; LINES] = [1.0, 1.0, -1.0, 1.0, 1.0, -1.0, -1.0, -1.0];

fn size_factor(size_pct: f32) -> f32 {
	0.4 + 1.2 * size_pct * 0.01
}

pub struct Reverb {
	params: ParamValues<N_PARAMS>,
	sample_rate: f32,
	pre_delay: DelayLine,
	diffusers: [Allpass; 4],
	lines: [DelayLine; LINES],
	lowpass: [f32; LINES],
	gains: [f32; LINES],
	lengths: [f32; LINES],
	lfo_phase: [f32; 4],
	damp_coeff: f32,
	low_cut: [Biquad; 2],
	low_cut_coeffs: BiquadCoeffs,
	size: Smoother,
	pre: Smoother,
	width: Smoother,
	mix: Smoother,
	control_counter: usize,
	params_dirty: bool,
}

impl Default for Reverb {
	fn default() -> Self {
		Self::new()
	}
}

impl Reverb {
	pub fn new() -> Self {
		let params = ParamValues::new(&INFO);
		Self {
			sample_rate: 48_000.0,
			pre_delay: DelayLine::default(),
			diffusers: Default::default(),
			lines: Default::default(),
			lowpass: [0.0; LINES],
			gains: [0.0; LINES],
			lengths: [1.0; LINES],
			lfo_phase: [0.0, 0.25, 0.5, 0.75],
			damp_coeff: 0.0,
			low_cut: [Biquad::default(); 2],
			low_cut_coeffs: BiquadCoeffs::IDENTITY,
			size: Smoother::new(size_factor(params.get(P_SIZE))),
			pre: Smoother::new(params.get(P_PRE_DELAY)),
			width: Smoother::new(params.get(P_WIDTH) * 0.01),
			mix: Smoother::new(params.get(P_MIX) * 0.01),
			control_counter: 0,
			params_dirty: true,
			params,
		}
	}

	fn update_control(&mut self) {
		let sr = self.sample_rate;
		let size = self.size.value();
		let rt60 = self.params.get(P_DECAY);
		for ((len, gain), ms) in self.lengths.iter_mut().zip(&mut self.gains).zip(LINE_MS) {
			*len = ms * size * 0.001 * sr;
			// Gain per pass so that the loop loses 60 dB in `rt60` seconds.
			*gain = 10.0_f32.powf(-3.0 * *len / (rt60 * sr));
		}
		if self.params_dirty {
			let damping = self.params.get(P_DAMPING) * 0.01;
			let cutoff = 20_000.0 * 0.06_f32.powf(damping);
			self.damp_coeff = (-TAU * cutoff.min(sr * 0.45) / sr).exp();
			self.low_cut_coeffs = BiquadCoeffs::highpass(self.params.get(P_LOW_CUT), 0.7, sr);
			self.params_dirty = false;
		}
	}
}

impl Processor for Reverb {
	fn info(&self) -> &'static DeviceInfo {
		&INFO
	}

	fn prepare(&mut self, sample_rate: f32, _max_block: usize) {
		let sr = sample_rate.max(1.0);
		self.sample_rate = sr;
		let ms = |m: f32| (m * 0.001 * sr).ceil() as usize;
		self.pre_delay.allocate(ms(250.0) + 2);
		for (d, &m) in self.diffusers.iter_mut().zip(&DIFFUSER_MS) {
			d.allocate(ms(m));
		}
		for (line, &m) in self.lines.iter_mut().zip(&LINE_MS) {
			line.allocate(ms(m * MAX_SIZE + MOD_MS * 2.0) + 4);
		}
		for s in [&mut self.pre, &mut self.width, &mut self.mix] {
			s.prepare(sr, 20.0);
		}
		// Size glides slowly: line-length changes are heard as pitch bends.
		self.size.prepare(sr, 150.0);
		self.params_dirty = true;
		self.reset();
	}

	fn set_param(&mut self, index: usize, value: f32) {
		let Some(v) = self.params.set(&INFO, index, value) else {
			return;
		};
		match index {
			P_SIZE => self.size.set_target(size_factor(v)),
			P_PRE_DELAY => self.pre.set_target(v),
			P_WIDTH => self.width.set_target(v * 0.01),
			P_MIX => self.mix.set_target(v * 0.01),
			_ => self.params_dirty = true,
		}
	}

	fn process(
		&mut self,
		left: &mut [f32],
		right: &mut [f32],
		_events: &[MidiEvent],
		_transport: &Transport,
	) {
		let sr = self.sample_rate;
		let ms_to_samples = 0.001 * sr;
		let mod_depth = MOD_MS * ms_to_samples;
		let lfo_incs = [0.53 / sr, 0.71 / sr, 0.89 / sr, 0.37 / sr];
		let input_gain = 0.35;
		for (l, r) in left.iter_mut().zip(right.iter_mut()) {
			if self.control_counter == 0 {
				self.size.advance(CONTROL_BLOCK);
				self.update_control();
			}
			self.control_counter = (self.control_counter + 1) % CONTROL_BLOCK;

			let dry_l = *l;
			let dry_r = *r;
			self.pre_delay.push((dry_l + dry_r) * 0.5);
			let pre = self.pre.tick() * ms_to_samples;
			let mut x = if pre < 1.0 {
				(dry_l + dry_r) * 0.5
			} else {
				self.pre_delay.read(pre)
			};
			for d in &mut self.diffusers {
				x = d.process(x, 0.6);
			}

			let mut outs = [0.0_f32; LINES];
			for k in 0..LINES {
				let mut len = self.lengths[k];
				if k < 4 {
					len += mod_depth * (1.0 + (TAU * self.lfo_phase[k]).sin());
					self.lfo_phase[k] = (self.lfo_phase[k] + lfo_incs[k]).fract();
				}
				let y = self.lines[k].read(len);
				self.lowpass[k] = flush(y + (self.lowpass[k] - y) * self.damp_coeff);
				outs[k] = self.lowpass[k] * self.gains[k];
			}
			// Householder reflection: lossless, maximally mixing, O(N).
			let sum: f32 = outs.iter().sum::<f32>() * (2.0 / LINES as f32);
			for (k, (line, out)) in self.lines.iter_mut().zip(outs).enumerate() {
				let sign = if k % 2 == 0 { 1.0 } else { -1.0 };
				line.push(flush(out - sum + x * input_gain * sign));
			}

			let mut wl = 0.0;
			let mut wr = 0.0;
			for k in 0..LINES / 2 {
				wl += outs[2 * k] * OUT_SIGNS[2 * k];
				wr += outs[2 * k + 1] * OUT_SIGNS[2 * k + 1];
			}
			wl = self.low_cut[0].process(&self.low_cut_coeffs, wl * 0.6);
			wr = self.low_cut[1].process(&self.low_cut_coeffs, wr * 0.6);
			let width = self.width.tick();
			let mid = (wl + wr) * 0.5;
			let side = (wl - wr) * 0.5 * width;
			let mix = self.mix.tick();
			// Equal-power-ish crossfade keeps perceived level steady across the mix range.
			let dry_gain = (mix * PI * 0.5).cos();
			let wet_gain = (mix * PI * 0.5).sin();
			*l = dry_l * dry_gain + (mid + side) * wet_gain;
			*r = dry_r * dry_gain + (mid - side) * wet_gain;
		}
	}

	fn reset(&mut self) {
		self.pre_delay.clear();
		for d in &mut self.diffusers {
			d.clear();
		}
		for line in &mut self.lines {
			line.clear();
		}
		self.lowpass = [0.0; LINES];
		self.low_cut = [Biquad::default(); 2];
		for s in [
			&mut self.size,
			&mut self.pre,
			&mut self.width,
			&mut self.mix,
		] {
			s.snap();
		}
		self.control_counter = 0;
		self.params_dirty = true;
		self.update_control();
	}

	fn tail_seconds(&self) -> f32 {
		self.params.get(P_DECAY) + self.params.get(P_PRE_DELAY) * 0.001 + 0.1
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::util::gain_to_db;

	fn run(rv: &mut Reverb, l: &mut [f32], r: &mut [f32]) {
		for (a, b) in l.chunks_mut(256).zip(r.chunks_mut(256)) {
			rv.process(a, b, &[], &Transport::default());
		}
	}

	fn rms(x: &[f32]) -> f32 {
		(x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt()
	}

	#[test]
	fn decay_matches_rt60_and_tail() {
		let sr = 48_000.0;
		let mut rv = Reverb::new();
		rv.prepare(sr, 256);
		for (id, v) in [
			("mix", 100.0),
			("decay", 1.5),
			("damping", 0.0),
			("low_cut", 20.0),
		] {
			rv.set_param(INFO.param_index(id).unwrap(), v);
		}
		rv.reset();
		let n = (sr * 4.0) as usize;
		let mut l = vec![0.0_f32; n];
		let mut r = vec![0.0_f32; n];
		let mut rng = crate::util::Rng::new(3);
		for i in 0..4_800 {
			l[i] = rng.bipolar() * 0.5;
			r[i] = rng.bipolar() * 0.5;
		}
		run(&mut rv, &mut l, &mut r);
		assert!(l.iter().chain(&r).all(|x| x.is_finite()));
		let win = 4_800;
		let at = |t: f32| rms(&l[(t * sr) as usize..(t * sr) as usize + win]);
		let drop = gain_to_db(at(0.3)) - gain_to_db(at(1.05));
		// 0.75 s at RT60 1.5 s should be about 30 dB.
		assert!((drop - 30.0).abs() < 6.0, "drop {drop} dB");
		let tail_end = (rv.tail_seconds() * sr) as usize;
		let peak_start = l[..10_000].iter().fold(0.0_f32, |m, x| m.max(x.abs()));
		let peak_end = l[tail_end..].iter().fold(0.0_f32, |m, x| m.max(x.abs()));
		assert!(gain_to_db(peak_end) - gain_to_db(peak_start) < -50.0);
	}

	#[test]
	fn dry_passes_at_zero_mix() {
		let mut rv = Reverb::new();
		rv.prepare(48_000.0, 64);
		rv.set_param(P_MIX, 0.0);
		rv.reset();
		let mut l = vec![0.3_f32; 64];
		let mut r = vec![-0.3_f32; 64];
		rv.process(&mut l, &mut r, &[], &Transport::default());
		assert!(l.iter().all(|&x| (x - 0.3).abs() < 1e-6));
		assert!(r.iter().all(|&x| (x + 0.3).abs() < 1e-6));
	}
}
