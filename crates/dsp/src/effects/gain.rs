//! `gain`: utility gain, pan, stereo width and polarity.

use crate::processor::{DeviceInfo, DeviceRole, MidiEvent, ParamInfo, Processor, Transport};
use crate::util::smoother::Smoother;
use crate::util::{PI, ParamValues, db_to_gain};

const P_GAIN: usize = 0;
const P_PAN: usize = 1;
const P_WIDTH: usize = 2;
const P_INVERT_L: usize = 3;
const P_INVERT_R: usize = 4;
const N_PARAMS: usize = 5;

static PARAMS: [ParamInfo; N_PARAMS] = [
	ParamInfo::new("gain", "Gain", -60.0, 24.0, 0.0, "dB", "Level change."),
	ParamInfo::new(
		"pan",
		"Pan",
		-100.0,
		100.0,
		0.0,
		"%",
		"Balance: -100 hard left, 0 center (unity), +100 hard right.",
	),
	ParamInfo::new(
		"width",
		"Width",
		0.0,
		200.0,
		100.0,
		"%",
		"Stereo width: 0 = mono, 100 = unchanged, 200 = extra wide (use sparingly).",
	),
	ParamInfo::new(
		"invert_left",
		"Invert Left",
		0.0,
		1.0,
		0.0,
		"",
		"1 flips the polarity of the left channel.",
	),
	ParamInfo::new(
		"invert_right",
		"Invert Right",
		0.0,
		1.0,
		0.0,
		"",
		"1 flips the polarity of the right channel.",
	),
];

pub static INFO: DeviceInfo = DeviceInfo {
	id: "gain",
	name: "Gain / Utility",
	role: DeviceRole::Effect,
	description: "Clean utility: gain in dB, balance pan, stereo width (0 = mono, e.g. for bass \
		and kick to keep low end centered) and per-channel polarity invert. Use for gain staging \
		before compressors/saturators, narrowing or widening, and fixing phase issues.",
	params: &PARAMS,
};

pub struct Gain {
	params: ParamValues<N_PARAMS>,
	gain: Smoother,
	pan: Smoother,
	width: Smoother,
	sign_l: Smoother,
	sign_r: Smoother,
}

impl Default for Gain {
	fn default() -> Self {
		Self::new()
	}
}

impl Gain {
	pub fn new() -> Self {
		let params = ParamValues::new(&INFO);
		Self {
			gain: Smoother::new(db_to_gain(params.get(P_GAIN))),
			pan: Smoother::new(params.get(P_PAN) * 0.01),
			width: Smoother::new(params.get(P_WIDTH) * 0.01),
			sign_l: Smoother::new(1.0),
			sign_r: Smoother::new(1.0),
			params,
		}
	}
}

impl Processor for Gain {
	fn info(&self) -> &'static DeviceInfo {
		&INFO
	}

	fn prepare(&mut self, sample_rate: f32, _max_block: usize) {
		for s in [
			&mut self.gain,
			&mut self.pan,
			&mut self.width,
			&mut self.sign_l,
			&mut self.sign_r,
		] {
			s.prepare(sample_rate, 10.0);
		}
	}

	fn set_param(&mut self, index: usize, value: f32) {
		let Some(v) = self.params.set(&INFO, index, value) else {
			return;
		};
		match index {
			P_GAIN => self.gain.set_target(db_to_gain(v)),
			P_PAN => self.pan.set_target(v * 0.01),
			P_WIDTH => self.width.set_target(v * 0.01),
			P_INVERT_L => self.sign_l.set_target(if v >= 0.5 { -1.0 } else { 1.0 }),
			P_INVERT_R => self.sign_r.set_target(if v >= 0.5 { -1.0 } else { 1.0 }),
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
		for (l, r) in left.iter_mut().zip(right.iter_mut()) {
			let g = self.gain.tick();
			let pan = self.pan.tick();
			let width = self.width.tick();
			let mid = (*l + *r) * 0.5;
			let side = (*l - *r) * 0.5 * width;
			let (sl, sr) = (mid + side, mid - side);
			// Balance law: the near side stays at unity, the far side follows a
			// quarter-cosine so the sweep sounds even.
			let gl = if pan > 0.0 {
				(pan * PI * 0.5).cos()
			} else {
				1.0
			};
			let gr = if pan < 0.0 {
				(-pan * PI * 0.5).cos()
			} else {
				1.0
			};
			*l = sl * gl * g * self.sign_l.tick();
			*r = sr * gr * g * self.sign_r.tick();
		}
	}

	fn reset(&mut self) {
		for s in [
			&mut self.gain,
			&mut self.pan,
			&mut self.width,
			&mut self.sign_l,
			&mut self.sign_r,
		] {
			s.snap();
		}
	}
}
