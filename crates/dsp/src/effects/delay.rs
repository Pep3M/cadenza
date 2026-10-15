//! `delay`: stereo / ping-pong echo with tempo sync and filtered feedback.

use crate::processor::{DeviceInfo, DeviceRole, MidiEvent, ParamInfo, Processor, Transport};
use crate::util::biquad::{Biquad, BiquadCoeffs};
use crate::util::delay_line::DelayLine;
use crate::util::smoother::Smoother;
use crate::util::{ParamValues, flush};

const P_TIME: usize = 0;
const P_SYNC: usize = 1;
const P_FEEDBACK: usize = 2;
const P_PING_PONG: usize = 3;
const P_HIGH_CUT: usize = 4;
const P_LOW_CUT: usize = 5;
const P_MIX: usize = 6;
const N_PARAMS: usize = 7;

const MAX_DELAY_MS: f32 = 4_000.0;

/// Note values for `sync`, in quarter-note beats (index 0 = free time in ms).
const SYNC_BEATS: [f64; 13] = [
	0.0,
	0.125,
	1.0 / 6.0,
	0.25,
	0.375,
	1.0 / 3.0,
	0.5,
	0.75,
	2.0 / 3.0,
	1.0,
	1.5,
	2.0,
	4.0,
];

static PARAMS: [ParamInfo; N_PARAMS] = [
	ParamInfo::new(
		"time",
		"Time",
		1.0,
		MAX_DELAY_MS,
		375.0,
		"ms",
		"Echo time in milliseconds; used only when sync = 0.",
	),
	ParamInfo::new(
		"sync",
		"Tempo Sync",
		0.0,
		12.0,
		6.0,
		"",
		"0 = free (uses time), 1 = 1/32, 2 = 1/16 triplet, 3 = 1/16, 4 = dotted 1/16, 5 = 1/8 triplet, \
		 6 = 1/8, 7 = dotted 1/8, 8 = 1/4 triplet, 9 = 1/4, 10 = dotted 1/4, 11 = 1/2, 12 = 1 bar (4/4). \
		 Follows the song tempo.",
	),
	ParamInfo::new(
		"feedback",
		"Feedback",
		0.0,
		95.0,
		35.0,
		"%",
		"Amount fed back = number of repeats. 15-30 slapback/subtle, 35-55 rhythmic, 60+ long dub trails.",
	),
	ParamInfo::new(
		"ping_pong",
		"Ping-Pong",
		0.0,
		1.0,
		0.0,
		"",
		"1 = echoes bounce left/right (wide), 0 = straight stereo echoes.",
	),
	ParamInfo::new(
		"high_cut",
		"High Cut",
		500.0,
		20_000.0,
		7_000.0,
		"Hz",
		"Low-pass inside the feedback loop: each repeat gets darker. 2000-5000 for warm tape-like echoes.",
	),
	ParamInfo::new(
		"low_cut",
		"Low Cut",
		20.0,
		2_000.0,
		120.0,
		"Hz",
		"High-pass inside the feedback loop so repeats don't build up low-end mud.",
	),
	ParamInfo::new(
		"mix",
		"Mix",
		0.0,
		100.0,
		25.0,
		"%",
		"Dry/wet balance. 100% on a send/bus track; 10-30% as an insert.",
	),
];

pub static INFO: DeviceInfo = DeviceInfo {
	id: "delay",
	name: "Delay",
	role: DeviceRole::Effect,
	description: "Stereo echo with tempo sync (straight, dotted and triplet note values), \
		ping-pong mode and filtered feedback. Recipes — VOCAL THROW: sync 7 (dotted 1/8), feedback \
		35, ping_pong 1, high_cut 5000, mix 20. SLAPBACK: sync 0, time 110, feedback 10, mix 25. \
		DUB: sync 10, feedback 65, high_cut 2500. Synth plucks/leads love sync 6 or 7 with \
		ping_pong 1. Best on a send at mix 100 when shared.",
	params: &PARAMS,
};

pub struct Delay {
	params: ParamValues<N_PARAMS>,
	sample_rate: f32,
	lines: [DelayLine; 2],
	filters: [[Biquad; 2]; 2],
	lp: BiquadCoeffs,
	hp: BiquadCoeffs,
	filters_dirty: bool,
	time: Smoother,
	feedback: Smoother,
	mix: Smoother,
	prepared: bool,
}

impl Default for Delay {
	fn default() -> Self {
		Self::new()
	}
}

impl Delay {
	pub fn new() -> Self {
		let params = ParamValues::new(&INFO);
		Self {
			sample_rate: 48_000.0,
			lines: Default::default(),
			filters: [[Biquad::default(); 2]; 2],
			lp: BiquadCoeffs::IDENTITY,
			hp: BiquadCoeffs::IDENTITY,
			filters_dirty: true,
			time: Smoother::new(1.0),
			feedback: Smoother::new(params.get(P_FEEDBACK) * 0.01),
			mix: Smoother::new(params.get(P_MIX) * 0.01),
			prepared: false,
			params,
		}
	}

	/// Delay time in samples for the current settings and tempo.
	fn time_samples(&self, bpm: f64) -> f32 {
		let sync = self.params.choice(P_SYNC).min(SYNC_BEATS.len() - 1);
		let ms = if sync == 0 || !(bpm.is_finite() && bpm > 0.0) {
			self.params.get(P_TIME)
		} else {
			(SYNC_BEATS[sync] * 60_000.0 / bpm) as f32
		};
		ms.clamp(1.0, MAX_DELAY_MS) * 0.001 * self.sample_rate
	}

	fn update_filters(&mut self) {
		let sr = self.sample_rate;
		self.lp = BiquadCoeffs::lowpass(self.params.get(P_HIGH_CUT), 0.707, sr);
		self.hp = BiquadCoeffs::highpass(self.params.get(P_LOW_CUT), 0.707, sr);
		self.filters_dirty = false;
	}
}

impl Processor for Delay {
	fn info(&self) -> &'static DeviceInfo {
		&INFO
	}

	fn prepare(&mut self, sample_rate: f32, _max_block: usize) {
		self.sample_rate = sample_rate.max(1.0);
		let max = (MAX_DELAY_MS * 0.001 * self.sample_rate).ceil() as usize + 4;
		for line in &mut self.lines {
			line.allocate(max);
		}
		// Slow glide on time changes gives a tape-like pitch bend instead of clicks.
		self.time.prepare(self.sample_rate, 60.0);
		self.feedback.prepare(self.sample_rate, 20.0);
		self.mix.prepare(self.sample_rate, 20.0);
		self.update_filters();
		self.prepared = true;
		self.reset();
	}

	fn set_param(&mut self, index: usize, value: f32) {
		let Some(v) = self.params.set(&INFO, index, value) else {
			return;
		};
		match index {
			P_FEEDBACK => self.feedback.set_target(v * 0.01),
			P_MIX => self.mix.set_target(v * 0.01),
			P_HIGH_CUT | P_LOW_CUT => self.filters_dirty = true,
			_ => {}
		}
	}

	fn process(
		&mut self,
		left: &mut [f32],
		right: &mut [f32],
		_events: &[MidiEvent],
		transport: &Transport,
	) {
		if !self.prepared {
			return;
		}
		if self.filters_dirty {
			self.update_filters();
		}
		self.time.set_target(self.time_samples(transport.bpm));
		let ping_pong = self.params.flag(P_PING_PONG);
		for (l, r) in left.iter_mut().zip(right.iter_mut()) {
			let d = self.time.tick();
			let fb = self.feedback.tick();
			let mix = self.mix.tick();
			let wet_l = self.lines[0].read(d);
			let wet_r = self.lines[1].read(d);
			let [fl, fr] = &mut self.filters;
			let lp_l = fl[0].process(&self.lp, wet_l);
			let lp_r = fr[0].process(&self.lp, wet_r);
			let fb_l = fl[1].process(&self.hp, lp_l) * fb;
			let fb_r = fr[1].process(&self.hp, lp_r) * fb;
			let (in_l, in_r) = if ping_pong {
				((*l + *r) * 0.5 + fb_r, fb_l)
			} else {
				(*l + fb_l, *r + fb_r)
			};
			self.lines[0].push(flush(in_l.clamp(-4.0, 4.0)));
			self.lines[1].push(flush(in_r.clamp(-4.0, 4.0)));
			*l = *l * (1.0 - mix) + wet_l * mix;
			*r = *r * (1.0 - mix) + wet_r * mix;
		}
	}

	fn reset(&mut self) {
		for line in &mut self.lines {
			line.clear();
		}
		self.filters = [[Biquad::default(); 2]; 2];
		self.time.set_immediate(self.time_samples(120.0));
		self.feedback.snap();
		self.mix.snap();
	}

	fn tail_seconds(&self) -> f32 {
		let time = self.time.target() / self.sample_rate;
		let fb = self.params.get(P_FEEDBACK) * 0.01;
		// Repeats needed to fall 60 dB at this feedback, plus the first echo.
		let repeats = if fb > 0.001 { 3.0 / -fb.log10() } else { 0.0 };
		(time * (repeats + 1.0)).min(60.0)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn delay_with(params: &[(&str, f32)]) -> Delay {
		let mut d = Delay::new();
		for &(id, v) in params {
			d.set_param(INFO.param_index(id).unwrap(), v);
		}
		d.prepare(48_000.0, 1024);
		d
	}

	#[test]
	fn echo_at_exact_offset() {
		let mut d = delay_with(&[
			("sync", 0.0),
			("time", 100.0),
			("feedback", 0.0),
			("mix", 100.0),
		]);
		let mut l = vec![0.0_f32; 10_000];
		let mut r = vec![0.0_f32; 10_000];
		l[0] = 1.0;
		r[0] = 1.0;
		for (a, b) in l.chunks_mut(333).zip(r.chunks_mut(333)) {
			d.process(a, b, &[], &Transport::default());
		}
		let peak = l
			.iter()
			.enumerate()
			.max_by(|a, b| a.1.abs().total_cmp(&b.1.abs()))
			.unwrap();
		assert_eq!(peak.0, 4_800);
		assert!((peak.1 - 1.0).abs() < 1e-4);
	}

	#[test]
	fn tempo_sync_quarter_note() {
		let mut d = delay_with(&[("sync", 9.0), ("feedback", 0.0), ("mix", 100.0)]);
		let t = Transport {
			bpm: 100.0,
			..Transport::default()
		};
		// Let the time smoother land on the new tempo before measuring.
		let mut l = vec![0.0_f32; 48_000];
		let mut r = vec![0.0_f32; 48_000];
		d.process(&mut l, &mut r, &[], &t);
		let mut l = vec![0.0_f32; 48_000];
		let mut r = vec![0.0_f32; 48_000];
		l[0] = 1.0;
		d.process(&mut l, &mut r, &[], &t);
		let idx = l
			.iter()
			.enumerate()
			.max_by(|a, b| a.1.abs().total_cmp(&b.1.abs()))
			.unwrap()
			.0;
		assert_eq!(idx, 28_800); // 0.6 s at 100 bpm
	}

	#[test]
	fn ping_pong_alternates() {
		let mut d = delay_with(&[
			("sync", 0.0),
			("time", 10.0),
			("feedback", 50.0),
			("mix", 100.0),
			("ping_pong", 1.0),
			("high_cut", 20_000.0),
			("low_cut", 20.0),
		]);
		let mut l = vec![0.0_f32; 2_000];
		let mut r = vec![0.0_f32; 2_000];
		l[0] = 1.0;
		r[0] = 1.0;
		d.process(&mut l, &mut r, &[], &Transport::default());
		assert!(l[480].abs() > 0.5 && r[480].abs() < 0.05);
		assert!(
			r[960..980].iter().any(|x| x.abs() > 0.3) && l[960..980].iter().all(|x| x.abs() < 0.05)
		);
	}
}
