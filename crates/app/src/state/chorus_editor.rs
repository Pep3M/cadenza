//! Pure logic of the Chorus editor (`chorus`, design `16 Effects`, 16d): the two voices'
//! modulated delay over time (the same LFO formula as the DSP), the free-running
//! display phase, the wave and width-fan gestures and the label texts. The view turns
//! the changes into one command per gesture.

use cadenza_dsp::{DeviceInfo, ParamInfo};

pub use crate::state::device::minus;

pub const RATE: &str = "rate";
pub const DEPTH: &str = "depth";
pub const DELAY: &str = "delay";
pub const FEEDBACK: &str = "feedback";
pub const WIDTH: &str = "width";
pub const MIX: &str = "mix";

/// The DSP's largest excursion at 100 % depth (`effects::chorus::MAX_DEPTH_MS`).
pub const MAX_DEPTH_MS: f32 = 5.0;
/// The fan's voices sit this far from the centre at 100 % width (degrees).
pub const FAN_DEGREES: f32 = 80.0;
/// A horizontal drag of this many points doubles the rate.
pub const RATE_OCTAVE_PX: f32 = 160.0;
/// The traces' amplitude at 100 % depth, as a share of half the display height.
pub const TRACE_SCALE: f32 = 0.82;

/// A parameter change: (param id, plain value).
pub type Change = (&'static str, f32);

pub fn info() -> &'static DeviceInfo {
	cadenza_dsp::device_info("chorus").expect("the chorus is a built-in")
}

fn param(id: &str) -> &'static ParamInfo {
	info().param(id).expect("a chorus param")
}

fn clamp(id: &str, v: f32) -> f32 {
	let p = param(id);
	v.clamp(p.min, p.max)
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChorusValues {
	pub rate: f32,
	pub depth: f32,
	pub delay: f32,
	pub feedback: f32,
	pub width: f32,
	pub mix: f32,
}

impl Default for ChorusValues {
	fn default() -> Self {
		Self::from_fn(|p| p.default)
	}
}

impl ChorusValues {
	pub fn from_fn(f: impl Fn(&ParamInfo) -> f32) -> Self {
		Self {
			rate: f(param(RATE)),
			depth: f(param(DEPTH)),
			delay: f(param(DELAY)),
			feedback: f(param(FEEDBACK)),
			width: f(param(WIDTH)),
			mix: f(param(MIX)),
		}
	}

	/// How far the read point swings each way (ms): depth × 5 ms, never closer than
	/// 0.5 ms to the write point (as the DSP clamps it).
	pub fn excursion_ms(&self) -> f32 {
		(self.depth * 0.01 * MAX_DEPTH_MS)
			.min(self.delay - 0.5)
			.max(0.0)
	}

	/// The right voice's LFO offset in cycles (the DSP's `0.25 × width`).
	pub fn offset_cycles(&self) -> f32 {
		0.25 * self.width * 0.01
	}

	/// The same offset in degrees ("63° apart").
	pub fn offset_degrees(&self) -> f32 {
		self.offset_cycles() * 360.0
	}

	/// The time one LFO cycle takes (s).
	pub fn cycle_seconds(&self) -> f32 {
		1.0 / self.rate.max(0.001)
	}

	/// The time span the traces show: two cycles, kept between 1 and 8 s so slow rates
	/// still move and fast ones stay readable.
	pub fn view_seconds(&self) -> f32 {
		(2.0 * self.cycle_seconds()).clamp(1.0, 8.0)
	}

	/// The traces' amplitude as a share of half the display height.
	pub fn trace_amplitude(&self) -> f32 {
		self.excursion_ms() / MAX_DEPTH_MS * TRACE_SCALE
	}
}

/// The display phase (cycles, 0..1) after `dt` seconds at `rate` Hz.
pub fn advance(phase: f64, rate: f32, dt: f32) -> f64 {
	(phase + f64::from(rate) * f64::from(dt.max(0.0))).rem_euclid(1.0)
}

/// One voice's LFO value (-1..1) at `phase` cycles: voice 0 is left, voice 1 right
/// (offset by the width), as in the DSP's first tap.
pub fn voice_value(phase: f64, voice: usize, offset_cycles: f32) -> f32 {
	let ph = phase + f64::from(offset_cycles) * voice as f64;
	(std::f64::consts::TAU * ph).sin() as f32
}

/// `n + 1` samples of a voice's trace, oldest first: `(t, value)` with `t` in seconds
/// relative to now, from `-past` to `future`, where `phase_now` is the LFO phase now.
pub fn trace(
	phase_now: f64,
	rate: f32,
	voice: usize,
	offset_cycles: f32,
	past: f32,
	future: f32,
	n: usize,
) -> Vec<(f32, f32)> {
	let n = n.max(1);
	(0..=n)
		.map(|i| {
			let t = -past + (past + future) * i as f32 / n as f32;
			let ph = phase_now + f64::from(rate) * f64::from(t);
			(t, voice_value(ph, voice, offset_cycles))
		})
		.collect()
}

/// The times (s, relative to now) inside `-past..future` where the left voice crosses
/// a quarter cycle: grid lines that scroll with the traces.
pub fn quarter_lines(phase_now: f64, rate: f32, past: f32, future: f32) -> Vec<f32> {
	let rate = f64::from(rate.max(0.001));
	let first = ((phase_now - f64::from(past) * rate) * 4.0).ceil() as i64;
	let last = ((phase_now + f64::from(future) * rate) * 4.0).floor() as i64;
	(first..=last)
		.take(256)
		.map(|q| ((q as f64 / 4.0 - phase_now) / rate) as f32)
		.collect()
}

fn round_to(v: f32, step: f32) -> f32 {
	(v / step).round() * step
}

/// Rate and depth after a drag on the wave of `dx` (right = faster, 160 px doubles) and
/// `dy` points (up = deeper, `px_per_pct` points per percent).
pub fn drag_wave(start: (f32, f32), dx: f32, dy: f32, px_per_pct: f32) -> (f32, f32) {
	let rate = start.0 * (dx / RATE_OCTAVE_PX).exp2();
	let rate = clamp(RATE, round_to(rate, 0.01));
	let depth = start.1 - dy / px_per_pct.max(0.01);
	(rate, clamp(DEPTH, depth.round()))
}

/// The width a fan voice dragged to `dx` points from the fan's centre (on a fan of
/// radius `radius`) stands for: its angle from the centre line over [`FAN_DEGREES`].
pub fn fan_width(dx: f32, radius: f32) -> f32 {
	let s = (dx.abs() / radius.max(1.0)).min(1.0);
	let degrees = s.asin().to_degrees();
	clamp(WIDTH, (degrees / FAN_DEGREES * 100.0).round())
}

/// A voice's angle on the fan (degrees from the centre line, right positive).
pub fn fan_angle(width: f32, voice: usize) -> f32 {
	let a = width * 0.01 * FAN_DEGREES;
	if voice == 0 { -a } else { a }
}

/// The double-click resets: the wave back to the default rate and depth, the fan to
/// full width.
pub fn reset_wave() -> [Change; 2] {
	[(RATE, param(RATE).default), (DEPTH, param(DEPTH).default)]
}

pub fn reset_width() -> Change {
	(WIDTH, param(WIDTH).default)
}

/// "Voice R · 63° apart (width)".
pub fn apart_label(values: &ChorusValues) -> String {
	format!(
		"Voice R \u{b7} {:.0}\u{b0} apart (width)",
		values.offset_degrees()
	)
}

/// "rate 0.60 Hz · one cycle = 1.67 s".
pub fn rate_label(values: &ChorusValues) -> String {
	format!(
		"rate {:.2} Hz \u{b7} one cycle = {:.2} s",
		values.rate,
		values.cycle_seconds()
	)
}

/// "modulated delay ±2.3 ms".
pub fn excursion_label(values: &ChorusValues) -> String {
	format!("modulated delay \u{b1}{:.1} ms", values.excursion_ms())
}

/// The undo label of a wave drag that changed `rate` and/or `depth`.
pub fn wave_label(rate: bool, depth: bool) -> &'static str {
	match (rate, depth) {
		(true, true) => "Chorus rate and depth",
		(true, false) => "Chorus rate",
		_ => "Chorus depth",
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn values(rate: f32, depth: f32, width: f32) -> ChorusValues {
		ChorusValues {
			rate,
			depth,
			width,
			..ChorusValues::default()
		}
	}

	#[test]
	fn excursion_follows_the_dsp_clamp() {
		assert!((values(0.8, 45.0, 100.0).excursion_ms() - 2.25).abs() < 1e-5);
		let shallow = ChorusValues {
			delay: 2.0,
			depth: 100.0,
			..ChorusValues::default()
		};
		assert!((shallow.excursion_ms() - 1.5).abs() < 1e-5, "base - 0.5 ms");
		assert_eq!(values(0.8, 0.0, 100.0).trace_amplitude(), 0.0);
	}

	#[test]
	fn the_right_voice_leads_by_a_quarter_cycle_at_full_width() {
		let v = values(1.0, 50.0, 100.0);
		assert!((v.offset_degrees() - 90.0).abs() < 1e-4);
		// At phase 0 the left voice is at 0, the right one at its peak.
		assert!(voice_value(0.0, 0, v.offset_cycles()).abs() < 1e-6);
		assert!((voice_value(0.0, 1, v.offset_cycles()) - 1.0).abs() < 1e-6);
		// Mono chorus: both voices move together.
		let mono = values(1.0, 50.0, 0.0);
		for ph in [0.1, 0.37, 0.8] {
			assert_eq!(
				voice_value(ph, 0, mono.offset_cycles()),
				voice_value(ph, 1, mono.offset_cycles())
			);
		}
	}

	#[test]
	fn the_phase_advances_at_the_rate_and_wraps() {
		let p = advance(0.9, 0.5, 0.4);
		assert!((p - 0.1).abs() < 1e-6, "{p}");
		assert_eq!(advance(0.25, 2.0, 0.0), 0.25, "frozen while dt is 0");
		assert_eq!(advance(0.25, 2.0, -1.0), 0.25);
	}

	#[test]
	fn the_trace_ends_at_the_current_phase_and_spans_the_window() {
		let t = trace(0.25, 0.5, 0, 0.0, 3.0, 1.0, 40);
		assert_eq!(t.len(), 41);
		assert!((t[0].0 + 3.0).abs() < 1e-5 && (t[40].0 - 1.0).abs() < 1e-5);
		let now = t.iter().find(|(x, _)| x.abs() < 1e-5).unwrap();
		assert!((now.1 - 1.0).abs() < 1e-5, "sin(2π·0.25) at now");
		// One second back at 0.5 Hz is half a cycle earlier.
		let back = t.iter().find(|(x, _)| (x + 1.0).abs() < 1e-5).unwrap();
		assert!((back.1 + 1.0).abs() < 1e-5);
	}

	#[test]
	fn quarter_lines_fall_on_quarter_phases() {
		let lines = quarter_lines(0.1, 1.0, 1.0, 0.5);
		assert!(!lines.is_empty());
		for t in &lines {
			let ph = 0.1 + *t as f64;
			assert!(((ph * 4.0) - (ph * 4.0).round()).abs() < 1e-4);
			assert!((-1.0..=0.5).contains(t));
		}
		assert_eq!(lines.len(), 6, "-0.85 .. 0.4 in quarter steps");
	}

	#[test]
	fn the_wave_drag_moves_rate_sideways_and_depth_up() {
		let (rate, depth) = drag_wave((0.8, 45.0), RATE_OCTAVE_PX, 0.0, 1.0);
		assert!((rate - 1.6).abs() < 1e-4 && depth == 45.0);
		let (rate, depth) = drag_wave((0.8, 45.0), 0.0, -20.0, 2.0);
		assert!((rate - 0.8).abs() < 1e-4);
		assert_eq!(depth, 55.0);
		let (rate, depth) = drag_wave((0.8, 45.0), -10_000.0, 10_000.0, 1.0);
		assert_eq!((rate, depth), (0.05, 0.0), "clamped to the ranges");
	}

	#[test]
	fn the_fan_maps_the_voice_angle_to_width() {
		let r = 100.0;
		let dx = r * FAN_DEGREES.to_radians().sin();
		assert_eq!(fan_width(dx, r), 100.0);
		assert_eq!(fan_width(-dx, r), 100.0, "either voice");
		assert_eq!(fan_width(0.0, r), 0.0);
		let half = r * (FAN_DEGREES * 0.5).to_radians().sin();
		assert_eq!(fan_width(half, r), 50.0);
		assert_eq!(fan_angle(50.0, 0), -40.0);
		assert_eq!(fan_angle(50.0, 1), 40.0);
	}

	#[test]
	fn labels_read_like_the_design() {
		let v = values(0.6, 46.0, 70.0);
		assert_eq!(rate_label(&v), "rate 0.60 Hz \u{b7} one cycle = 1.67 s");
		assert_eq!(apart_label(&v), "Voice R \u{b7} 63\u{b0} apart (width)");
		assert_eq!(excursion_label(&v), "modulated delay \u{b1}2.3 ms");
		assert_eq!(wave_label(true, true), "Chorus rate and depth");
	}
}
