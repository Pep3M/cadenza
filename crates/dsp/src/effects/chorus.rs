//! `chorus`: two-voice-per-side modulated delay chorus.

use crate::processor::{DeviceInfo, DeviceRole, MidiEvent, ParamInfo, Processor, Transport};
use crate::util::delay_line::DelayLine;
use crate::util::smoother::Smoother;
use crate::util::{ParamValues, TAU, flush};

const P_RATE: usize = 0;
const P_DEPTH: usize = 1;
const P_DELAY: usize = 2;
const P_FEEDBACK: usize = 3;
const P_WIDTH: usize = 4;
const P_MIX: usize = 5;
const N_PARAMS: usize = 6;

const MAX_DEPTH_MS: f32 = 5.0;
const MAX_BASE_MS: f32 = 30.0;

static PARAMS: [ParamInfo; N_PARAMS] = [
	ParamInfo::new(
		"rate",
		"Rate",
		0.05,
		5.0,
		0.8,
		"Hz",
		"Modulation speed. 0.2-1 lush, 2-5 vibrato-like/Leslie-ish.",
	),
	ParamInfo::new(
		"depth",
		"Depth",
		0.0,
		100.0,
		45.0,
		"%",
		"Modulation amount (pitch wobble).",
	),
	ParamInfo::new(
		"delay",
		"Delay",
		1.0,
		MAX_BASE_MS,
		12.0,
		"ms",
		"Base delay. 1-5 ms flanger-like, 7-20 ms classic chorus, 20-30 ms doubling.",
	),
	ParamInfo::new(
		"feedback",
		"Feedback",
		0.0,
		90.0,
		0.0,
		"%",
		"Feedback for a more resonant, flanger-like sound.",
	),
	ParamInfo::new(
		"width",
		"Width",
		0.0,
		100.0,
		100.0,
		"%",
		"Stereo spread of the modulation (0 = mono chorus).",
	),
	ParamInfo::new(
		"mix",
		"Mix",
		0.0,
		100.0,
		50.0,
		"%",
		"Dry/wet balance; 50% gives the classic chorus.",
	),
];

pub static INFO: DeviceInfo = DeviceInfo {
	id: "chorus",
	name: "Chorus",
	role: DeviceRole::Effect,
	description: "Stereo chorus that thickens and widens. Great on electric piano, clean guitars, \
		pads, string synths and backing vocals; use little or none on bass and kick (keep low end \
		mono). Recipes — CLASSIC: rate 0.8, depth 45, delay 12, mix 50. SUBTLE WIDENER: rate 0.3, \
		depth 20, mix 30. FLANGER: delay 2, feedback 60, rate 0.2.",
	params: &PARAMS,
};

pub struct Chorus {
	params: ParamValues<N_PARAMS>,
	sample_rate: f32,
	lines: [DelayLine; 2],
	phase: f32,
	fb_state: [f32; 2],
	rate: Smoother,
	depth: Smoother,
	delay: Smoother,
	feedback: Smoother,
	width: Smoother,
	mix: Smoother,
}

impl Default for Chorus {
	fn default() -> Self {
		Self::new()
	}
}

impl Chorus {
	pub fn new() -> Self {
		let params = ParamValues::new(&INFO);
		Self {
			sample_rate: 48_000.0,
			lines: Default::default(),
			phase: 0.0,
			fb_state: [0.0; 2],
			rate: Smoother::new(params.get(P_RATE)),
			depth: Smoother::new(params.get(P_DEPTH) * 0.01),
			delay: Smoother::new(params.get(P_DELAY)),
			feedback: Smoother::new(params.get(P_FEEDBACK) * 0.01),
			width: Smoother::new(params.get(P_WIDTH) * 0.01),
			mix: Smoother::new(params.get(P_MIX) * 0.01),
			params,
		}
	}

	fn smoothers(&mut self) -> [&mut Smoother; 6] {
		[
			&mut self.rate,
			&mut self.depth,
			&mut self.delay,
			&mut self.feedback,
			&mut self.width,
			&mut self.mix,
		]
	}
}

impl Processor for Chorus {
	fn info(&self) -> &'static DeviceInfo {
		&INFO
	}

	fn prepare(&mut self, sample_rate: f32, _max_block: usize) {
		self.sample_rate = sample_rate.max(1.0);
		let max = ((MAX_BASE_MS + MAX_DEPTH_MS + 1.0) * 0.001 * self.sample_rate).ceil() as usize;
		for line in &mut self.lines {
			line.allocate(max);
		}
		let sr = self.sample_rate;
		for s in self.smoothers() {
			s.prepare(sr, 30.0);
		}
		self.reset();
	}

	fn set_param(&mut self, index: usize, value: f32) {
		let Some(v) = self.params.set(&INFO, index, value) else {
			return;
		};
		match index {
			P_RATE => self.rate.set_target(v),
			P_DEPTH => self.depth.set_target(v * 0.01),
			P_DELAY => self.delay.set_target(v),
			P_FEEDBACK => self.feedback.set_target(v * 0.01),
			P_WIDTH => self.width.set_target(v * 0.01),
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
		let ms = 0.001 * self.sample_rate;
		let inv_sr = 1.0 / self.sample_rate;
		for (l, r) in left.iter_mut().zip(right.iter_mut()) {
			let rate = self.rate.tick();
			let depth = self.depth.tick();
			let base = self.delay.tick();
			let fb = self.feedback.tick();
			let width = self.width.tick();
			let mix = self.mix.tick();
			// Excursion never pulls the read point below 0.5 ms.
			let excursion = (depth * MAX_DEPTH_MS).min(base - 0.5).max(0.0) * ms;
			let base_s = base * ms;
			let offset = 0.25 * width;
			let mut wet = [0.0_f32; 2];
			for (ch, w) in wet.iter_mut().enumerate() {
				let ph = self.phase + offset * ch as f32;
				let a = (TAU * ph).sin();
				let b = (TAU * (ph + 0.5)).sin();
				let line = &self.lines[ch];
				*w = 0.5
					* (line.read(base_s + excursion * a) + line.read(base_s * 1.3 + excursion * b));
			}
			self.phase = (self.phase + rate * inv_sr).fract();
			let inputs = [*l, *r];
			for ch in 0..2 {
				self.fb_state[ch] = flush(wet[ch] * fb);
				self.lines[ch].push(inputs[ch] + self.fb_state[ch]);
			}
			*l = inputs[0] * (1.0 - mix) + wet[0] * mix;
			*r = inputs[1] * (1.0 - mix) + wet[1] * mix;
		}
	}

	fn reset(&mut self) {
		for line in &mut self.lines {
			line.clear();
		}
		self.phase = 0.0;
		self.fb_state = [0.0; 2];
		for s in self.smoothers() {
			s.snap();
		}
	}
}
