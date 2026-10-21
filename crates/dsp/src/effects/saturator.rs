//! `saturator`: waveshaping distortion with first-order antiderivative
//! anti-aliasing (ADAA), a tone tilt and dry/wet mix.

use crate::processor::{DeviceInfo, DeviceRole, MidiEvent, ParamInfo, Processor, Transport};
use crate::util::biquad::{Biquad, BiquadCoeffs};
use crate::util::smoother::Smoother;
use crate::util::{ParamValues, db_to_gain, flush, one_pole_coeff};

const P_DRIVE: usize = 0;
const P_MODE: usize = 1;
const P_TONE: usize = 2;
const P_OUTPUT: usize = 3;
const P_MIX: usize = 4;
const N_PARAMS: usize = 5;

static PARAMS: [ParamInfo; N_PARAMS] = [
	ParamInfo::new(
		"drive",
		"Drive",
		0.0,
		36.0,
		6.0,
		"dB",
		"Input gain into the shaper. 2-6 subtle warmth, 8-15 obvious grit, 20+ heavy distortion. Output is roughly level-compensated.",
	),
	ParamInfo::new(
		"mode",
		"Mode",
		0.0,
		2.0,
		0.0,
		"",
		"0 = tape (smooth symmetric, odd harmonics), 1 = tube (asymmetric, warm even harmonics), 2 = hard clip (aggressive).",
	),
	ParamInfo::new(
		"tone",
		"Tone",
		-100.0,
		100.0,
		0.0,
		"%",
		"Post-saturation tilt: negative darker, positive brighter (±9 dB high shelf at 3 kHz).",
	),
	ParamInfo::new(
		"output",
		"Output",
		-24.0,
		12.0,
		0.0,
		"dB",
		"Output level trim.",
	),
	ParamInfo::new(
		"mix",
		"Mix",
		0.0,
		100.0,
		100.0,
		"%",
		"Dry/saturated blend; 20-50% adds harmonics while keeping transients (parallel saturation).",
	),
];

pub static INFO: DeviceInfo = DeviceInfo {
	id: "saturator",
	name: "Saturator",
	role: DeviceRole::Effect,
	description: "Analog-style saturation/distortion (tape, tube, hard clip) with anti-aliasing. \
		Adds harmonics, warmth, perceived loudness and helps bass be heard on small speakers. \
		Recipes — WARM BASS: mode 1, drive 8, tone -20. DRUM BUS GLUE: mode 0, drive 4, mix 50. \
		GRITTY LEAD: mode 2, drive 18, tone 20. LO-FI KEYS: mode 1, drive 10, tone -60. Use subtly \
		on the master (drive 1-3).",
	params: &PARAMS,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
	Tape,
	Tube,
	Hard,
}

const TUBE_BIAS: f64 = 0.35;

/// Partial level compensation: loudness grows with drive, but less than the
/// raw gain (used both by [`saturator_shape`] and the processor).
#[inline]
fn drive_compensation(drive_gain: f64) -> f64 {
	drive_gain.powf(-0.7)
}

/// The static waveshaping curve the processor applies (drive scaling, the
/// mode's shape and level compensation), before the DC blocker, tone filter,
/// dry/wet mix and output gain. For a slowly changing `x` this matches
/// [`Adaa::process`]'s output exactly; at audio rate `Adaa` additionally
/// anti-aliases fast transitions.
pub fn saturator_shape(mode: Mode, drive_db: f32, x: f32) -> f32 {
	let drive = db_to_gain(drive_db) as f64;
	(mode.shape(x as f64 * drive) * drive_compensation(drive)) as f32
}

#[inline]
fn ln_cosh(x: f64) -> f64 {
	let a = x.abs();
	a + (-2.0 * a).exp().ln_1p() - std::f64::consts::LN_2
}

impl Mode {
	#[inline]
	fn shape(self, x: f64) -> f64 {
		match self {
			Mode::Tape => x.tanh(),
			Mode::Tube => (x + TUBE_BIAS).tanh() - TUBE_BIAS.tanh(),
			Mode::Hard => x.clamp(-1.0, 1.0),
		}
	}

	/// Antiderivative of `shape`.
	#[inline]
	fn integral(self, x: f64) -> f64 {
		match self {
			Mode::Tape => ln_cosh(x),
			Mode::Tube => ln_cosh(x + TUBE_BIAS) - x * TUBE_BIAS.tanh(),
			Mode::Hard => {
				if x.abs() <= 1.0 {
					0.5 * x * x
				} else {
					x.abs() - 0.5
				}
			}
		}
	}
}

#[derive(Debug, Clone, Copy, Default)]
struct Adaa {
	prev_x: f64,
	prev_f: f64,
}

impl Adaa {
	#[inline]
	fn process(&mut self, mode: Mode, x: f64) -> f64 {
		let f = mode.integral(x);
		let dx = x - self.prev_x;
		let y = if dx.abs() > 1e-5 {
			(f - self.prev_f) / dx
		} else {
			mode.shape(0.5 * (x + self.prev_x))
		};
		self.prev_x = x;
		self.prev_f = f;
		y
	}
}

pub struct Saturator {
	params: ParamValues<N_PARAMS>,
	sample_rate: f32,
	mode: Mode,
	adaa: [Adaa; 2],
	dc: [(f32, f32); 2],
	dc_coeff: f32,
	tone: [Biquad; 2],
	tone_coeffs: BiquadCoeffs,
	tone_db: Smoother,
	tone_coeffs_db: f32,
	drive: Smoother,
	output: Smoother,
	mix: Smoother,
}

impl Default for Saturator {
	fn default() -> Self {
		Self::new()
	}
}

fn mode_from(v: usize) -> Mode {
	match v {
		0 => Mode::Tape,
		1 => Mode::Tube,
		_ => Mode::Hard,
	}
}

impl Saturator {
	pub fn new() -> Self {
		let params = ParamValues::new(&INFO);
		Self {
			sample_rate: 48_000.0,
			mode: mode_from(params.choice(P_MODE)),
			adaa: [Adaa::default(); 2],
			dc: [(0.0, 0.0); 2],
			dc_coeff: 0.999,
			tone: [Biquad::default(); 2],
			tone_coeffs: BiquadCoeffs::IDENTITY,
			tone_db: Smoother::new(params.get(P_TONE) * 0.09),
			tone_coeffs_db: f32::NAN,
			drive: Smoother::new(db_to_gain(params.get(P_DRIVE))),
			output: Smoother::new(db_to_gain(params.get(P_OUTPUT))),
			mix: Smoother::new(params.get(P_MIX) * 0.01),
			params,
		}
	}
}

impl Processor for Saturator {
	fn info(&self) -> &'static DeviceInfo {
		&INFO
	}

	fn prepare(&mut self, sample_rate: f32, _max_block: usize) {
		self.sample_rate = sample_rate.max(1.0);
		let sr = self.sample_rate;
		for s in [
			&mut self.drive,
			&mut self.output,
			&mut self.mix,
			&mut self.tone_db,
		] {
			s.prepare(sr, 20.0);
		}
		// DC blocker around 8 Hz: tube mode is asymmetric and creates offset.
		self.dc_coeff = 1.0 - one_pole_coeff(1_000.0 / (std::f32::consts::TAU * 8.0), sr);
		self.tone_coeffs_db = f32::NAN;
		self.reset();
	}

	fn set_param(&mut self, index: usize, value: f32) {
		let Some(v) = self.params.set(&INFO, index, value) else {
			return;
		};
		match index {
			P_DRIVE => self.drive.set_target(db_to_gain(v)),
			P_MODE => self.mode = mode_from(v.round() as usize),
			P_TONE => self.tone_db.set_target(v * 0.09),
			P_OUTPUT => self.output.set_target(db_to_gain(v)),
			P_MIX => self.mix.set_target(v * 0.01),
			_ => {}
		}
	}

	fn process(
		&mut self,
		left: &mut [f32],
		right: &mut [f32],
		_events: &[MidiEvent],
		_transport: &Transport,
	) {
		let mode = self.mode;
		let sr = self.sample_rate;
		for (i, (l, r)) in left.iter_mut().zip(right.iter_mut()).enumerate() {
			let tone_db = self.tone_db.tick();
			if i % 16 == 0 && tone_db != self.tone_coeffs_db {
				self.tone_coeffs = BiquadCoeffs::high_shelf(3_000.0, tone_db, sr);
				self.tone_coeffs_db = tone_db;
			}
			let drive = self.drive.tick();
			let comp = drive_compensation(drive as f64) as f32;
			let out = self.output.tick();
			let mix = self.mix.tick();
			for (ch, x) in [l, r].into_iter().enumerate() {
				let dry = *x;
				let shaped = self.adaa[ch].process(mode, (dry * drive) as f64) as f32;
				let (px, py) = self.dc[ch];
				let blocked = flush(shaped - px + self.dc_coeff * py);
				self.dc[ch] = (shaped, blocked);
				let wet = self.tone[ch].process(&self.tone_coeffs, blocked) * comp;
				*x = (dry * (1.0 - mix) + wet * mix) * out;
			}
		}
	}

	fn reset(&mut self) {
		self.adaa = [Adaa::default(); 2];
		self.dc = [(0.0, 0.0); 2];
		self.tone = [Biquad::default(); 2];
		for s in [
			&mut self.drive,
			&mut self.output,
			&mut self.mix,
			&mut self.tone_db,
		] {
			s.snap();
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn antiderivatives_match_shapes() {
		for mode in [Mode::Tape, Mode::Tube, Mode::Hard] {
			for i in -40..40 {
				let x = i as f64 * 0.1;
				let h = 1e-5;
				let d = (mode.integral(x + h) - mode.integral(x - h)) / (2.0 * h);
				assert!((d - mode.shape(x)).abs() < 1e-4, "{mode:?} at {x}");
			}
		}
	}

	/// A slow triangle ramp (well above the DC blocker's ~8 Hz cutoff so it
	/// passes through essentially unattenuated, but slow enough that ADAA's
	/// anti-aliasing correction converges to the plain shape) processed at
	/// mix 100%, neutral tone, 0 dB output should match `saturator_shape`.
	#[test]
	fn processed_ramp_matches_shape() {
		for mode in [Mode::Tape, Mode::Tube, Mode::Hard] {
			let mut s = Saturator::new();
			s.prepare(48_000.0, 512);
			s.set_param(P_MODE, mode as usize as f32);
			s.set_param(P_DRIVE, 12.0);
			s.set_param(P_TONE, 0.0);
			s.set_param(P_OUTPUT, 0.0);
			s.set_param(P_MIX, 100.0);
			s.reset();
			let sr = 48_000.0;
			let freq = 200.0; // well above the ~8 Hz DC-blocker cutoff
			// The DC blocker's time constant is ~955 samples (~20 ms); give
			// it ~10 time constants to settle before measuring.
			let settle = 9_600;
			let n = settle + 9_600;
			let x: Vec<f32> = (0..n)
				.map(|i| {
					let t = (i as f32 * freq / sr).fract();
					// Triangle wave in -1..1.
					if t < 0.5 {
						4.0 * t - 1.0
					} else {
						3.0 - 4.0 * t
					}
				})
				.collect();
			let mut l = x.clone();
			let mut r = x.clone();
			for (cl, cr) in l.chunks_mut(256).zip(r.chunks_mut(256)) {
				s.process(cl, cr, &[], &Transport::default());
			}
			// Skip the first few cycles so the tone filter settles.
			let want: Vec<f32> = x
				.iter()
				.map(|&xi| saturator_shape(mode, 12.0, xi))
				.collect();
			// Tube mode is asymmetric, so its shaped output carries a DC
			// offset; the real processor's DC blocker removes it (an
			// explicit, separate stage after shaping), but the static curve
			// in `saturator_shape` does not. Compare with any constant
			// offset removed so this still checks the curve's *shape*.
			let got_mean: f32 = l[settle..].iter().sum::<f32>() / (n - settle) as f32;
			let want_mean: f32 = want[settle..].iter().sum::<f32>() / (n - settle) as f32;
			for i in settle..n {
				let got = l[i] - got_mean;
				let w = want[i] - want_mean;
				assert!(
					(got - w).abs() < 0.02,
					"{mode:?} at sample {i} (x={}): got {got} (raw {}), want {w} (raw {})",
					x[i],
					l[i],
					want[i]
				);
			}
		}
	}

	#[test]
	fn output_bounded_under_heavy_drive() {
		let mut s = Saturator::new();
		s.prepare(48_000.0, 512);
		s.set_param(P_DRIVE, 36.0);
		s.reset();
		let mut l: Vec<f32> = (0..4_800).map(|n| (n as f32 * 0.03).sin()).collect();
		let mut r = l.clone();
		s.process(&mut l, &mut r, &[], &Transport::default());
		assert!(l.iter().all(|x| x.is_finite() && x.abs() < 1.5));
	}
}
