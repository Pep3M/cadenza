//! `reverb`: 8-line feedback delay network (FDN) reverb.
//!
//! Signal flow: mono input → pre-delay → 4 series all-pass diffusers → FDN
//! with a Householder mixing matrix, per-line damping and per-line gains
//! derived from the requested RT60 (so `decay` is the real -60 dB time and
//! `tail_seconds` is accurate) → slowly modulated line lengths to avoid
//! metallic ringing → wet low-cut → mid/side width → ducking → dry/wet mix.
//!
//! Ducking (`duck`, `duck_release`): a peak follower (5 ms attack) on the dry
//! input, or on the sidechain key when one is set, turns the wet signal down
//! by up to `duck` dB: nothing below -40 dBFS, the full amount from -10 dBFS,
//! linear in dB between. So the tail stays out of the way while the source
//! plays and blooms in the gaps. `duck` 0 skips the path (bit-identical).

use crate::processor::{DeviceInfo, DeviceRole, MidiEvent, ParamInfo, Processor, Transport};
use crate::util::biquad::{Biquad, BiquadCoeffs};
use crate::util::delay_line::{Allpass, DelayLine};
use crate::util::smoother::Smoother;
use crate::util::{PI, ParamValues, TAU, db_to_gain, flush, gain_to_db, one_pole_coeff};

const P_SIZE: usize = 0;
const P_DECAY: usize = 1;
const P_DAMPING: usize = 2;
const P_PRE_DELAY: usize = 3;
const P_WIDTH: usize = 4;
const P_LOW_CUT: usize = 5;
const P_MIX: usize = 6;
const P_FREEZE: usize = 7;
const P_DUCK: usize = 8;
const P_DUCK_RELEASE: usize = 9;
const N_PARAMS: usize = 10;

/// Key level (dBFS) below which the wet signal is not ducked.
pub const DUCK_FLOOR_DB: f32 = -40.0;
/// Key level (dBFS) from which the wet signal is ducked by the whole `duck` amount.
pub const DUCK_FULL_DB: f32 = -10.0;
const DUCK_ATTACK_MS: f32 = 5.0;

/// Wet reduction (positive dB) for a key level, the processor's own law: 0 below
/// [`DUCK_FLOOR_DB`], `duck_db` from [`DUCK_FULL_DB`], linear in dB between.
pub fn reverb_duck_db(duck_db: f32, key_db: f32) -> f32 {
	let t = ((key_db - DUCK_FLOOR_DB) / (DUCK_FULL_DB - DUCK_FLOOR_DB)).clamp(0.0, 1.0);
	duck_db.max(0.0) * t
}

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
	ParamInfo::new(
		"freeze",
		"Freeze",
		0.0,
		1.0,
		0.0,
		"",
		"1 = hold the current tail forever (infinite sustain, new input muted, no damping); 0 = normal. Automate it for pad-like drones and ambient swells.",
	),
	ParamInfo::new(
		"duck",
		"Duck",
		0.0,
		24.0,
		0.0,
		"dB",
		"Turns the reverb's wet signal down by up to this much while the dry input plays (or the \
		 sidechain key, when set with set_sidechain), so the tail fills the gaps instead of washing \
		 over the vocal. 0 = off, 3-6 subtle, 8-12 clear vocal reverb, 15+ pumping effect.",
	),
	ParamInfo::new(
		"duck_release",
		"Duck Release",
		20.0,
		2_000.0,
		300.0,
		"ms",
		"How fast the wet signal comes back after the input stops. 150-400 natural, 600+ slow bloom.",
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
		WASH: size 100, decay 9, damping 60. Keep low_cut 150-300 to avoid mud. freeze 1 holds the \
		tail as a drone. DUCKED VOCAL: duck 8-10, duck_release 300 (the tail rises in the gaps); \
		key it from another track with set_sidechain to duck by that track instead.",
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
	/// 0 = normal, 1 = frozen (gains, damping and input glide between both).
	freeze: Smoother,
	control_counter: usize,
	params_dirty: bool,
	/// Ducking: smoothed amount (dB), follower (linear peak) and its coefficients.
	duck: Smoother,
	duck_env: f32,
	duck_attack: f32,
	duck_release: f32,
	/// Largest wet reduction of the last block, positive dB.
	duck_meter: f32,
	/// Sidechain key for the next block (allocated in `prepare`).
	key_l: Vec<f32>,
	key_r: Vec<f32>,
	key_len: usize,
	key_active: bool,
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
			freeze: Smoother::new(params.get(P_FREEZE).round()),
			control_counter: 0,
			params_dirty: true,
			duck: Smoother::new(params.get(P_DUCK)),
			duck_env: 0.0,
			duck_attack: 1.0,
			duck_release: 1.0,
			duck_meter: 0.0,
			key_l: Vec::new(),
			key_r: Vec::new(),
			key_len: 0,
			key_active: false,
			params,
		}
	}

	/// Wet reduction during the last block, positive dB (0 = not ducking).
	pub fn duck_db(&self) -> f32 {
		self.duck_meter
	}

	fn update_duck_times(&mut self) {
		self.duck_attack = one_pole_coeff(DUCK_ATTACK_MS, self.sample_rate);
		self.duck_release = one_pole_coeff(self.params.get(P_DUCK_RELEASE), self.sample_rate);
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

	fn prepare(&mut self, sample_rate: f32, max_block: usize) {
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
		self.freeze.prepare(sr, 50.0);
		self.duck.prepare(sr, 20.0);
		self.key_l = vec![0.0; max_block.max(1)];
		self.key_r = vec![0.0; max_block.max(1)];
		self.update_duck_times();
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
			P_FREEZE => self.freeze.set_target(v.round()),
			P_DUCK => self.duck.set_target(v),
			P_DUCK_RELEASE => self.update_duck_times(),
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
		let mut input_gain = 0.35;
		let mut gains = self.gains;
		let mut damp = self.damp_coeff;
		let keyed = std::mem::take(&mut self.key_active);
		let ducking = self.duck.value() > 0.0 || self.duck.target() > 0.0;
		let mut duck_max: f32 = 0.0;
		for (i, (l, r)) in left.iter_mut().zip(right.iter_mut()).enumerate() {
			if self.control_counter == 0 {
				self.size.advance(CONTROL_BLOCK);
				self.freeze.advance(CONTROL_BLOCK);
				self.update_control();
				// Frozen: lossless loop (Householder matrix, unit gains, no damping) and
				// no new input, so the tail rings on unchanged.
				let f = self.freeze.value();
				input_gain = 0.35 * (1.0 - f);
				for (g, &own) in gains.iter_mut().zip(&self.gains) {
					*g = own + (1.0 - own) * f;
				}
				damp = self.damp_coeff * (1.0 - f);
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
				self.lowpass[k] = flush(y + (self.lowpass[k] - y) * damp);
				outs[k] = self.lowpass[k] * gains[k];
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
			let mut mid = (wl + wr) * 0.5;
			let mut side = (wl - wr) * 0.5 * width;
			if ducking {
				let (kl, kr) = if !keyed {
					(dry_l, dry_r)
				} else if i < self.key_len {
					(self.key_l[i], self.key_r[i])
				} else {
					(0.0, 0.0)
				};
				let peak = kl.abs().max(kr.abs());
				let coeff = if peak > self.duck_env {
					self.duck_attack
				} else {
					self.duck_release
				};
				self.duck_env = flush(self.duck_env + (peak - self.duck_env) * coeff);
				let gr = reverb_duck_db(self.duck.tick(), gain_to_db(self.duck_env));
				duck_max = duck_max.max(gr);
				let g = db_to_gain(-gr);
				mid *= g;
				side *= g;
			}
			let mix = self.mix.tick();
			// Equal-power-ish crossfade keeps perceived level steady across the mix range.
			let dry_gain = (mix * PI * 0.5).cos();
			let wet_gain = (mix * PI * 0.5).sin();
			*l = dry_l * dry_gain + (mid + side) * wet_gain;
			*r = dry_r * dry_gain + (mid - side) * wet_gain;
		}
		self.duck_meter = duck_max;
	}

	fn has_sidechain(&self) -> bool {
		true
	}

	fn set_sidechain(&mut self, left: &[f32], right: &[f32]) {
		let n = left.len().min(right.len()).min(self.key_l.len());
		self.key_l[..n].copy_from_slice(&left[..n]);
		self.key_r[..n].copy_from_slice(&right[..n]);
		self.key_len = n;
		self.key_active = true;
	}

	fn readings(&self, out: &mut [f32]) -> usize {
		match out.get_mut(crate::readings::GAIN_REDUCTION_DB) {
			Some(gr) => {
				*gr = self.duck_meter;
				1
			}
			None => 0,
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
			&mut self.freeze,
			&mut self.duck,
		] {
			s.snap();
		}
		self.duck_env = 0.0;
		self.duck_meter = 0.0;
		self.control_counter = 0;
		self.params_dirty = true;
		self.update_control();
	}

	/// A frozen tail rings forever; renders still stop after the normal decay.
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
	fn freeze_sustains_the_tail_and_mutes_new_input() {
		let sr = 48_000.0;
		let mut rv = Reverb::new();
		rv.prepare(sr, 256);
		for (id, v) in [("mix", 100.0), ("decay", 0.5), ("low_cut", 20.0)] {
			rv.set_param(INFO.param_index(id).unwrap(), v);
		}
		rv.reset();
		let mut rng = crate::util::Rng::new(5);
		let mut l: Vec<f32> = (0..4_800).map(|_| rng.bipolar() * 0.5).collect();
		let mut r = l.clone();
		run(&mut rv, &mut l, &mut r);
		rv.set_param(P_FREEZE, 1.0);
		// Two seconds frozen (4 x RT60): a normal tail would be 240 dB down.
		let n = (sr * 2.0) as usize;
		let mut l = vec![0.0_f32; n];
		let mut r = vec![0.0_f32; n];
		run(&mut rv, &mut l, &mut r);
		let early = rms(&l[4_800..9_600]);
		let late = rms(&l[n - 4_800..]);
		assert!(early > 1e-3, "tail held: {early}");
		let loss = gain_to_db(early) - gain_to_db(late);
		assert!(loss.abs() < 3.0, "frozen tail lost {loss} dB");
		// New input is not added while frozen.
		let mut loud = vec![0.9_f32; 4_800];
		let mut loud_r = loud.clone();
		run(&mut rv, &mut loud, &mut loud_r);
		let with_input = rms(&loud[2_400..]);
		assert!(gain_to_db(with_input) - gain_to_db(late) < 3.0);
		// Unfrozen, the tail decays again.
		rv.set_param(P_FREEZE, 0.0);
		let mut l = vec![0.0_f32; n];
		let mut r = vec![0.0_f32; n];
		run(&mut rv, &mut l, &mut r);
		assert!(gain_to_db(rms(&l[n - 4_800..])) - gain_to_db(late) < -60.0);
	}

	/// Wet-only output (mix 100) of a 0.5 s noise burst followed by 1.5 s of silence.
	fn ducked_tail(duck: f32, key: Option<f32>) -> (Vec<f32>, f32) {
		let sr = 48_000.0;
		let mut rv = Reverb::new();
		rv.prepare(sr, 256);
		for (id, v) in [
			("mix", 100.0),
			("decay", 2.0),
			("low_cut", 20.0),
			("duck", duck),
			("duck_release", 100.0),
		] {
			rv.set_param(INFO.param_index(id).unwrap(), v);
		}
		rv.reset();
		let n = (sr * 2.0) as usize;
		let mut rng = crate::util::Rng::new(9);
		let mut l: Vec<f32> = (0..n)
			.map(|i| if i < 24_000 { rng.bipolar() * 0.5 } else { 0.0 })
			.collect();
		let mut r = l.clone();
		let mut max_reading: f32 = 0.0;
		for (a, b) in l.chunks_mut(256).zip(r.chunks_mut(256)) {
			if let Some(k) = key {
				let kl = vec![k; a.len()];
				rv.set_sidechain(&kl, &kl);
			}
			rv.process(a, b, &[], &Transport::default());
			let mut out = [0.0; crate::readings::MAX];
			assert_eq!(rv.readings(&mut out), 1);
			max_reading = max_reading.max(out[crate::readings::GAIN_REDUCTION_DB]);
		}
		(l, max_reading)
	}

	#[test]
	fn ducking_lowers_the_wet_only_while_the_input_plays() {
		let (plain, plain_gr) = ducked_tail(0.0, None);
		let (ducked, gr) = ducked_tail(12.0, None);
		assert_eq!(plain_gr, 0.0);
		assert!((gr - 12.0).abs() < 0.5, "full duck reading: {gr}");
		let db = |a: &[f32], b: &[f32]| gain_to_db(rms(a)) - gain_to_db(rms(b));
		// During the burst (-6 dBFS noise peaks: full duck) the tail is 12 dB down.
		let during = db(&ducked[12_000..23_000], &plain[12_000..23_000]);
		assert!((during + 12.0).abs() < 1.5, "during the burst: {during} dB");
		// 0.5 s after it stops (5 x release) the tail is back.
		let after = db(&ducked[48_000..60_000], &plain[48_000..60_000]);
		assert!(after.abs() < 1.0, "after the burst: {after} dB");
		// The dry path is not touched: at mix 0 ducking changes nothing.
		let mut rv = Reverb::new();
		rv.prepare(48_000.0, 64);
		rv.set_param(P_MIX, 0.0);
		rv.set_param(P_DUCK, 24.0);
		rv.reset();
		let mut l = vec![0.5_f32; 64];
		let mut r = vec![0.5_f32; 64];
		rv.process(&mut l, &mut r, &[], &Transport::default());
		assert!(l.iter().all(|&x| (x - 0.5).abs() < 1e-6));
	}

	#[test]
	fn a_sidechain_key_ducks_instead_of_the_input() {
		let (plain, _) = ducked_tail(0.0, None);
		// A loud key ducks the tail even after the input stopped.
		let (keyed, gr) = ducked_tail(12.0, Some(0.8));
		let late =
			gain_to_db(rms(&keyed[48_000..60_000])) - gain_to_db(rms(&plain[48_000..60_000]));
		assert!((late + 12.0).abs() < 1.5, "keyed tail: {late} dB");
		assert!(gr > 11.0);
		// A silent key never ducks, however loud the input.
		let (quiet_key, gr) = ducked_tail(12.0, Some(0.0));
		assert_eq!(gr, 0.0);
		assert_eq!(quiet_key, plain);
	}

	#[test]
	fn duck_law_is_flat_below_the_floor_and_full_above() {
		assert_eq!(reverb_duck_db(10.0, -60.0), 0.0);
		assert_eq!(reverb_duck_db(10.0, -5.0), 10.0);
		assert!((reverb_duck_db(10.0, -25.0) - 5.0).abs() < 1e-5);
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
