//! `delay`: stereo / ping-pong echo with tempo sync and filtered feedback.
//!
//! Multi-tap (`taps` 2..4): extra read heads on the same delay lines. Tap 1 is
//! the echo at `time`; tap n sits at `time x (1 + (n - 1) x tap_spread)`, so
//! spread 50 % with 4 taps plays echoes at 1, 1.5, 2 and 2.5 x the time (spread
//! 100 % = whole multiples). Each extra tap has its own level and a balance pan
//! of the stereo pair it reads. Only the lines feed back, so every tap repeats
//! with the feedback at the line's rhythm (tap time + k x time). With ping-pong
//! the lines bounce L/R as usual and each tap reads both lines at its own time:
//! it carries the same bouncing repeats, offset, and its pan balances them.
//! Taps past the 4 s line are clamped to it. `taps` 1 = the plain delay
//! (bit-identical).

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
const P_TAPS: usize = 7;
const P_TAP_SPREAD: usize = 8;
/// `tap2_level`, `tap3_level`, `tap4_level`.
const P_TAP_LEVEL: usize = 9;
/// `tap2_pan`, `tap3_pan`, `tap4_pan`.
const P_TAP_PAN: usize = 12;
const N_PARAMS: usize = 15;

/// Most read heads (tap 1 = the plain echo).
pub const MAX_TAPS: usize = 4;

/// One read head of the delay as the processor plays it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DelayTap {
	/// Time after the dry signal, seconds.
	pub seconds: f64,
	/// Linear level, 0..1 (tap 1 = 1).
	pub level: f32,
	/// Balance pan of the tap's stereo pair, -1 (left) ..= 1 (right).
	pub pan: f32,
}

/// The read heads for the given plain parameter values and tempo: the first
/// `count` entries of `taps` are used (tap 1 at [`delay_time_seconds`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DelayTaps {
	pub taps: [DelayTap; MAX_TAPS],
	pub count: usize,
}

impl DelayTaps {
	pub fn as_slice(&self) -> &[DelayTap] {
		&self.taps[..self.count]
	}
}

/// The delay's taps (see the module docs for the model), shared by the processor
/// and the editor's echo picture.
pub fn delay_taps(params: &[f32], bpm: f64) -> DelayTaps {
	let p = |i: usize| params.get(i).copied().unwrap_or(PARAMS[i].default);
	let base = delay_time_seconds(params, bpm);
	let count = (p(P_TAPS).round() as usize).clamp(1, MAX_TAPS);
	let spread = p(P_TAP_SPREAD) as f64 * 0.01;
	let mut taps = [DelayTap {
		seconds: base,
		level: 1.0,
		pan: 0.0,
	}; MAX_TAPS];
	for (n, tap) in taps.iter_mut().enumerate().skip(1) {
		tap.seconds = (base * (1.0 + n as f64 * spread)).min(MAX_DELAY_MS as f64 * 0.001);
		tap.level = p(P_TAP_LEVEL + n - 1) * 0.01;
		tap.pan = p(P_TAP_PAN + n - 1);
	}
	DelayTaps { taps, count }
}

/// Balance gains `(left, right)` for a tap pan: 0 keeps both sides.
fn balance(pan: f32) -> (f32, f32) {
	((1.0 - pan).min(1.0), (1.0 + pan).min(1.0))
}

const MAX_DELAY_MS: f32 = 4_000.0;

/// Tempo-sync steps for `sync`: `(display name, quarter notes)`, index 0 =
/// free time (uses `time` instead). The single source for the step list and
/// [`delay_time_seconds`], so the UI's picture of the steps and the
/// processor's timing cannot drift apart.
const SYNC_STEPS: [(&str, f64); 13] = [
	("Free", 0.0),
	("1/32", 0.125),
	("1/16 triplet", 1.0 / 6.0),
	("1/16", 0.25),
	("dotted 1/16", 0.375),
	("1/8 triplet", 1.0 / 3.0),
	("1/8", 0.5),
	("dotted 1/8", 0.75),
	("1/4 triplet", 2.0 / 3.0),
	("1/4", 1.0),
	("dotted 1/4", 1.5),
	("1/2", 2.0),
	("1 bar (4/4)", 4.0),
];

/// The tempo-sync steps for `sync`, as `(name, quarter notes)`; index 0 is
/// free time (uses `time` instead of a tempo-synced step).
pub fn delay_sync_steps() -> &'static [(&'static str, f64)] {
	&SYNC_STEPS
}

/// Echo time in seconds for the given plain parameter values and tempo
/// (quarter notes per minute), matching what the processor feeds its delay
/// line. `params` are the plain values in [`INFO`]'s param order.
pub fn delay_time_seconds(params: &[f32], bpm: f64) -> f64 {
	let p = |i: usize| params.get(i).copied().unwrap_or(PARAMS[i].default);
	let sync = (p(P_SYNC).round().max(0.0) as usize).min(SYNC_STEPS.len() - 1);
	let ms = if sync == 0 || !(bpm.is_finite() && bpm > 0.0) {
		p(P_TIME) as f64
	} else {
		SYNC_STEPS[sync].1 * 60_000.0 / bpm
	};
	ms.clamp(1.0, MAX_DELAY_MS as f64) * 0.001
}

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
	ParamInfo::new(
		"taps",
		"Taps",
		1.0,
		4.0,
		1.0,
		"",
		"Number of echo taps (read heads). 1 = plain delay; 2-4 add echoes after the first at \
		 time x (1 + (n-1) x tap_spread), each with its own level and pan, for rhythmic multi-tap \
		 patterns. All taps repeat with the feedback.",
	),
	ParamInfo::new(
		"tap_spread",
		"Tap Spread",
		10.0,
		100.0,
		50.0,
		"%",
		"Gap between taps as a share of the echo time: 50 = taps at 1, 1.5, 2, 2.5 x time (dotted \
		 feels), 33 = triplet-like, 100 = whole multiples.",
	),
	ParamInfo::new(
		"tap2_level",
		"Tap 2 Level",
		0.0,
		100.0,
		70.0,
		"%",
		"Level of tap 2 (taps >= 2).",
	),
	ParamInfo::new(
		"tap3_level",
		"Tap 3 Level",
		0.0,
		100.0,
		50.0,
		"%",
		"Level of tap 3 (taps >= 3).",
	),
	ParamInfo::new(
		"tap4_level",
		"Tap 4 Level",
		0.0,
		100.0,
		35.0,
		"%",
		"Level of tap 4 (taps = 4).",
	),
	ParamInfo::new(
		"tap2_pan",
		"Tap 2 Pan",
		-1.0,
		1.0,
		-0.5,
		"",
		"Balance of tap 2, -1 left .. 1 right; alternate signs for a wide bouncing pattern.",
	),
	ParamInfo::new(
		"tap3_pan",
		"Tap 3 Pan",
		-1.0,
		1.0,
		0.5,
		"",
		"Balance of tap 3, -1 left .. 1 right.",
	),
	ParamInfo::new(
		"tap4_pan",
		"Tap 4 Pan",
		-1.0,
		1.0,
		-0.5,
		"",
		"Balance of tap 4, -1 left .. 1 right.",
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
		ping_pong 1. Best on a send at mix 100 when shared. MULTI-TAP: taps 3-4 with tap_spread \
		50 and alternating tap pans for galloping, wide rhythmic echoes (lower feedback, 15-30).",
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
	/// Tap 2..4 read positions as multiples of the tap 1 time, and their gains.
	tap_factor: [f32; MAX_TAPS],
	tap_gain: [(f32, f32); MAX_TAPS],
	tap_count: usize,
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
			tap_factor: [1.0; MAX_TAPS],
			tap_gain: [(1.0, 1.0); MAX_TAPS],
			tap_count: 1,
			params,
		}
	}

	fn update_taps(&mut self, bpm: f64) {
		let taps = delay_taps(self.params.values(), bpm);
		let base = taps.taps[0].seconds;
		self.tap_count = taps.count;
		for ((factor, gain), tap) in self
			.tap_factor
			.iter_mut()
			.zip(&mut self.tap_gain)
			.zip(&taps.taps)
		{
			*factor = (tap.seconds / base) as f32;
			let (l, r) = balance(tap.pan);
			*gain = (l * tap.level, r * tap.level);
		}
	}

	/// Delay time in samples for the current settings and tempo.
	fn time_samples(&self, bpm: f64) -> f32 {
		(delay_time_seconds(self.params.values(), bpm) * self.sample_rate as f64) as f32
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
		let multi = self.params.get(P_TAPS).round() >= 2.0;
		if multi {
			self.update_taps(transport.bpm);
		}
		for (l, r) in left.iter_mut().zip(right.iter_mut()) {
			let d = self.time.tick();
			let fb = self.feedback.tick();
			let mix = self.mix.tick();
			let wet_l = self.lines[0].read(d);
			let wet_r = self.lines[1].read(d);
			let (mut out_l, mut out_r) = (wet_l, wet_r);
			if multi {
				for n in 1..self.tap_count {
					let at = d * self.tap_factor[n];
					let (gl, gr) = self.tap_gain[n];
					out_l += self.lines[0].read(at) * gl;
					out_r += self.lines[1].read(at) * gr;
				}
			}
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
			*l = *l * (1.0 - mix) + out_l * mix;
			*r = *r * (1.0 - mix) + out_r * mix;
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
		// The last tap starts its repeats that much later.
		let last_tap = if self.params.get(P_TAPS).round() >= 2.0 {
			let taps = delay_taps(self.params.values(), 120.0);
			(taps
				.as_slice()
				.last()
				.map_or(0.0, |t| t.seconds / taps.taps[0].seconds) as f32
				- 1.0) * time
		} else {
			0.0
		};
		(time * (repeats + 1.0) + last_tap).min(60.0)
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

	#[test]
	fn taps_play_at_their_times_levels_and_pans() {
		let mut d = delay_with(&[
			("sync", 0.0),
			("time", 100.0),
			("feedback", 0.0),
			("mix", 100.0),
			("taps", 4.0),
			("tap_spread", 50.0),
		]);
		let n = 14_000;
		let mut l = vec![0.0_f32; n];
		let mut r = vec![0.0_f32; n];
		l[0] = 1.0;
		r[0] = 1.0;
		for (a, b) in l.chunks_mut(300).zip(r.chunks_mut(300)) {
			d.process(a, b, &[], &Transport::default());
		}
		let mut params: Vec<f32> = PARAMS.iter().map(|p| p.default).collect();
		for (id, v) in [("sync", 0.0), ("time", 100.0), ("taps", 4.0)] {
			params[INFO.param_index(id).unwrap()] = v;
		}
		let taps = delay_taps(&params, 120.0);
		assert_eq!(taps.count, 4);
		// Default taps: 1, 1.5, 2, 2.5 x 100 ms; levels 100/70/50/35 %, pans 0/-0.5/0.5/-0.5.
		let want = [
			(4_800, 1.0, 1.0),
			(7_200, 0.7, 0.35),
			(9_600, 0.25, 0.5),
			(12_000, 0.35, 0.175),
		];
		for ((at, wl, wr), tap) in want.into_iter().zip(taps.as_slice()) {
			assert_eq!((tap.seconds * 48_000.0).round() as usize, at);
			assert!((l[at] - wl).abs() < 1e-3, "L at {at}: {}", l[at]);
			assert!((r[at] - wr).abs() < 1e-3, "R at {at}: {}", r[at]);
		}
		// Nothing else sounds.
		let others = l
			.iter()
			.chain(&r)
			.enumerate()
			.filter(|(i, _)| !want.iter().any(|w| (i % n).abs_diff(w.0) < 3))
			.fold(0.0_f32, |m, (_, x)| m.max(x.abs()));
		assert!(others < 1e-3, "stray output {others}");
	}

	#[test]
	fn extra_taps_repeat_with_the_feedback() {
		let mut d = delay_with(&[
			("sync", 0.0),
			("time", 100.0),
			("feedback", 50.0),
			("high_cut", 20_000.0),
			("low_cut", 20.0),
			("mix", 100.0),
			("taps", 2.0),
			("tap2_pan", 0.0),
		]);
		let mut l = vec![0.0_f32; 16_000];
		let mut r = vec![0.0_f32; 16_000];
		l[0] = 1.0;
		r[0] = 1.0;
		d.process(&mut l, &mut r, &[], &Transport::default());
		// Tap 2 at 150 ms; its first repeat 100 ms later is the line's (filtered) repeat
		// that tap 1 plays at 200 ms, at tap 2's level.
		assert!((l[7_200] - 0.7).abs() < 0.02, "{}", l[7_200]);
		assert!(l[9_600] > 0.2, "tap 1 repeat {}", l[9_600]);
		assert!(
			(l[12_000] - 0.7 * l[9_600]).abs() < 0.01,
			"{} vs {}",
			l[12_000],
			l[9_600]
		);
	}

	#[test]
	fn sync_steps_round_trip_the_processor() {
		let mut params = [0.0_f32; N_PARAMS];
		for (i, p) in params.iter_mut().enumerate() {
			*p = PARAMS[i].default;
		}
		let bpm = 120.0;
		for (step, &(_name, quarters)) in delay_sync_steps().iter().enumerate() {
			params[P_SYNC] = step as f32;
			params[P_TIME] = 375.0;
			let got = delay_time_seconds(&params, bpm);
			let want = if step == 0 {
				0.375
			} else {
				(quarters * 60.0 / bpm).clamp(0.001, MAX_DELAY_MS as f64 * 0.001)
			};
			assert!((got - want).abs() < 1e-9, "step {step}: {got} != {want}");

			// Matches what `Delay::time_samples` actually uses.
			let d = delay_with(&[("sync", step as f32), ("time", 375.0)]);
			let sr = 48_000.0_f64;
			let samples = d.time_samples(bpm);
			assert!(
				(samples as f64 - got * sr).abs() < 1.0,
				"step {step}: {samples} samples != {got} s"
			);
		}
	}
}
