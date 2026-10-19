//! Pure logic of the Saturator editor (`saturator`, design `16 Effects`, 16e): the mode
//! cards, the shaping curve (the DSP's own `saturator_shape`, scaled to fit), the 150 ms
//! morph between modes, the drive drag, the test-sine scope and the harmonics a test
//! sine gets from the curve (a DFT of one period), so they show while stopped too.

use cadenza_dsp::response::{SaturatorMode as Mode, saturator_shape};
use cadenza_dsp::{DeviceInfo, ParamInfo};

pub use crate::state::compressor_editor::signed_db;

pub const DRIVE: &str = "drive";
pub const MODE: &str = "mode";
pub const TONE: &str = "tone";
pub const OUTPUT: &str = "output";
pub const MIX: &str = "mix";

/// A vertical drag moves the drive this much per point (the knobs' 200 px range).
pub const DRIVE_DB_PER_PX: f32 = 36.0 / 200.0;
/// Drive moves in these steps on the curve and the wheel.
pub const DRIVE_STEP: f32 = 0.5;
/// The dotted reference curve's drive.
pub const REFERENCE_DRIVE: f32 = 6.0;
/// Mode changes morph the curve over this long (s).
pub const MORPH_SECONDS: f64 = 0.15;
/// The test sine's peak (the design's 0.85).
pub const TEST_LEVEL: f32 = 0.85;
/// Harmonics shown, the fundamental included.
pub const HARMONICS: usize = 12;
/// Bars span this many dB below the fundamental.
pub const HARMONIC_RANGE_DB: f32 = 80.0;
const DFT_POINTS: usize = 256;

/// A parameter change: (param id, plain value).
pub type Change = (&'static str, f32);

pub fn info() -> &'static DeviceInfo {
	cadenza_dsp::device_info("saturator").expect("the saturator is a built-in")
}

fn param(id: &str) -> &'static ParamInfo {
	info().param(id).expect("a saturator param")
}

fn clamp(id: &str, v: f32) -> f32 {
	let p = param(id);
	v.clamp(p.min, p.max)
}

pub const MODES: [Mode; 3] = [Mode::Tape, Mode::Tube, Mode::Hard];

/// The mode of a `mode` parameter value (the DSP rounds the same way).
pub fn mode_of(value: f32) -> Mode {
	match value.round() as i32 {
		i32::MIN..=0 => Mode::Tape,
		1 => Mode::Tube,
		_ => Mode::Hard,
	}
}

pub fn mode_value(mode: Mode) -> f32 {
	match mode {
		Mode::Tape => 0.0,
		Mode::Tube => 1.0,
		Mode::Hard => 2.0,
	}
}

pub fn mode_index(mode: Mode) -> usize {
	mode_value(mode) as usize
}

pub fn mode_name(mode: Mode) -> &'static str {
	match mode {
		Mode::Tape => "Tape",
		Mode::Tube => "Tube",
		Mode::Hard => "Hard clip",
	}
}

pub fn mode_description(mode: Mode) -> &'static str {
	match mode {
		Mode::Tape => "Soft, smooth top",
		Mode::Tube => "Warm, uneven, 2nd harmonic",
		Mode::Hard => "Flat top, odd harmonics",
	}
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SatValues {
	pub drive: f32,
	pub mode: Mode,
	pub tone: f32,
	pub output: f32,
	pub mix: f32,
}

impl Default for SatValues {
	fn default() -> Self {
		Self::from_fn(|p| p.default)
	}
}

impl SatValues {
	pub fn from_fn(f: impl Fn(&ParamInfo) -> f32) -> Self {
		Self {
			drive: f(param(DRIVE)),
			mode: mode_of(f(param(MODE))),
			tone: f(param(TONE)),
			output: f(param(OUTPUT)),
			mix: f(param(MIX)),
		}
	}
}

/// The level the curve reaches at full scale input (the larger side: tube is
/// asymmetric).
fn curve_peak(mode: Mode, drive_db: f32) -> f32 {
	saturator_shape(mode, drive_db, 1.0)
		.abs()
		.max(saturator_shape(mode, drive_db, -1.0).abs())
		.max(1e-6)
}

/// The shaping curve at `x` (-1..1) scaled so its larger end reaches ±1: the DSP's level
/// compensation shrinks the curve as drive grows, the picture shows its shape.
pub fn shown_curve(mode: Mode, drive_db: f32, x: f32) -> f32 {
	saturator_shape(mode, drive_db, x) / curve_peak(mode, drive_db)
}

/// Smoothstep progress of a morph `elapsed` seconds in (0..1).
pub fn morph_progress(elapsed: f64) -> f32 {
	let t = (elapsed / MORPH_SECONDS).clamp(0.0, 1.0) as f32;
	t * t * (3.0 - 2.0 * t)
}

/// The curve partway from `from` to `to` (`t` = [`morph_progress`]).
pub fn morph_curve(from: Mode, to: Mode, t: f32, drive_db: f32, x: f32) -> f32 {
	let a = shown_curve(from, drive_db, x);
	let b = shown_curve(to, drive_db, x);
	a + (b - a) * t.clamp(0.0, 1.0)
}

/// Drive after a vertical drag of `dy` points (up = more) from `start`, in 0.5 dB steps.
pub fn drag_drive(start: f32, dy: f32) -> f32 {
	clamp(
		DRIVE,
		((start - dy * DRIVE_DB_PER_PX) / DRIVE_STEP).round() * DRIVE_STEP,
	)
}

pub fn reset_drive() -> Change {
	(DRIVE, 0.0)
}

/// "Drive +12.0 dB".
pub fn drive_label(drive: f32) -> String {
	format!("Drive {} dB", signed_db(drive))
}

/// `n + 1` points of the test sine (`cycles` periods) before and after the curve,
/// both scaled to fit: `(phase 0..1, before, after)`.
pub fn test_scope(mode: Mode, drive_db: f32, cycles: f32, n: usize) -> Vec<(f32, f32, f32)> {
	let n = n.max(1);
	(0..=n)
		.map(|i| {
			let t = i as f32 / n as f32;
			let x = TEST_LEVEL * (std::f32::consts::TAU * cycles * t).sin();
			(t, x, shown_curve(mode, drive_db, x) * TEST_LEVEL)
		})
		.collect()
}

/// Harmonic amplitudes 1..=[`HARMONICS`] of one period of the test sine through the
/// shaping curve (a plain DFT; index 0 is the fundamental).
pub fn harmonics(mode: Mode, drive_db: f32) -> [f32; HARMONICS] {
	let signal: Vec<f64> = (0..DFT_POINTS)
		.map(|i| {
			let x = TEST_LEVEL * (std::f32::consts::TAU * i as f32 / DFT_POINTS as f32).sin();
			f64::from(saturator_shape(mode, drive_db, x))
		})
		.collect();
	harmonics_of(&signal)
}

/// Harmonic amplitudes of one period of `signal` (a DFT at bins 1..=[`HARMONICS`]).
pub fn harmonics_of(signal: &[f64]) -> [f32; HARMONICS] {
	let n = signal.len().max(1) as f64;
	let mut out = [0.0f32; HARMONICS];
	for (k, a) in out.iter_mut().enumerate() {
		let h = (k + 1) as f64;
		let (mut re, mut im) = (0.0f64, 0.0f64);
		for (i, v) in signal.iter().enumerate() {
			let w = std::f64::consts::TAU * h * i as f64 / n;
			re += v * w.cos();
			im += v * w.sin();
		}
		*a = ((re * re + im * im).sqrt() / (n / 2.0)) as f32;
	}
	out
}

/// Each harmonic in dB below the fundamental (the fundamental reads 0).
pub fn harmonics_db(amps: &[f32; HARMONICS]) -> [f32; HARMONICS] {
	let h1 = amps[0].max(1e-9);
	amps.map(|a| 20.0 * (a.max(1e-9) / h1).log10())
}

/// A bar's height (0..1) for a level in dB below the fundamental.
pub fn bar_height(db: f32) -> f32 {
	(1.0 + db / HARMONIC_RANGE_DB).clamp(0.02, 1.0)
}

/// Even harmonics (2nd, 4th...) are coloured apart from odd ones.
pub fn is_even(index: usize) -> bool {
	(index + 1).is_multiple_of(2)
}

/// Where a scope of `len` samples starts so it begins at a rising zero crossing of
/// `samples` (the newest one that leaves room), else at the newest `len`.
pub fn trigger(samples: &[f32], len: usize) -> usize {
	let Some(last) = samples.len().checked_sub(len) else {
		return 0;
	};
	(1..=last)
		.rev()
		.find(|&i| samples[i - 1] < 0.0 && samples[i] >= 0.0)
		.unwrap_or(last)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn modes_round_trip_through_the_param() {
		for m in MODES {
			assert_eq!(mode_of(mode_value(m)), m);
		}
		assert_eq!(mode_of(1.4), Mode::Tube);
		assert_eq!(mode_of(-3.0), Mode::Tape);
		assert_eq!(mode_name(Mode::Hard), "Hard clip");
	}

	#[test]
	fn the_shown_curve_is_the_dsp_curve_scaled_to_fit() {
		for m in MODES {
			for drive in [0.0, 6.0, 12.0, 36.0] {
				let peak = shown_curve(m, drive, 1.0)
					.abs()
					.max(shown_curve(m, drive, -1.0).abs());
				assert!((peak - 1.0).abs() < 1e-5, "{m:?} {drive}");
				let ratio = shown_curve(m, drive, 0.3) / saturator_shape(m, drive, 0.3);
				let ratio2 = shown_curve(m, drive, -0.6) / saturator_shape(m, drive, -0.6);
				assert!((ratio - ratio2).abs() < 1e-3 * ratio.abs(), "one scale");
			}
		}
		// More drive bends the tape curve more: a small input reaches higher.
		assert!(shown_curve(Mode::Tape, 12.0, 0.2) > shown_curve(Mode::Tape, 6.0, 0.2));
	}

	#[test]
	fn the_morph_eases_from_one_mode_to_the_other() {
		assert_eq!(morph_progress(0.0), 0.0);
		assert_eq!(morph_progress(MORPH_SECONDS), 1.0);
		assert_eq!(morph_progress(1.0), 1.0);
		assert!((morph_progress(MORPH_SECONDS / 2.0) - 0.5).abs() < 1e-6);
		let x = 0.4;
		assert_eq!(
			morph_curve(Mode::Tape, Mode::Hard, 0.0, 12.0, x),
			shown_curve(Mode::Tape, 12.0, x)
		);
		assert_eq!(
			morph_curve(Mode::Tape, Mode::Hard, 1.0, 12.0, x),
			shown_curve(Mode::Hard, 12.0, x)
		);
	}

	#[test]
	fn drive_drags_in_half_db_steps() {
		assert_eq!(drag_drive(6.0, -10.0), 8.0, "10 px up = +1.8 -> 8.0");
		assert_eq!(drag_drive(6.0, 3.0), 5.5);
		assert_eq!(drag_drive(6.0, 1_000.0), 0.0);
		assert_eq!(drag_drive(6.0, -1_000.0), 36.0);
		assert_eq!(drive_label(12.0), "Drive +12.0 dB");
		assert_eq!(drive_label(0.0), "Drive 0.0 dB");
	}

	#[test]
	fn a_pure_sine_has_only_its_fundamental() {
		let sine: Vec<f64> = (0..256)
			.map(|i| 0.5 * (std::f64::consts::TAU * i as f64 / 256.0).sin())
			.collect();
		let h = harmonics_of(&sine);
		assert!((h[0] - 0.5).abs() < 1e-6);
		assert!(h[1..].iter().all(|a| *a < 1e-9));
		let mixed: Vec<f64> = (0..256)
			.map(|i| {
				let w = std::f64::consts::TAU * i as f64 / 256.0;
				w.sin() + 0.25 * (3.0 * w).cos()
			})
			.collect();
		let h = harmonics_of(&mixed);
		assert!((h[2] - 0.25).abs() < 1e-6, "{h:?}");
		let db = harmonics_db(&h);
		assert_eq!(db[0], 0.0);
		assert!((db[2] + 12.04).abs() < 0.01);
	}

	#[test]
	fn harmonics_follow_each_modes_symmetry() {
		let db = |m: Mode, drive: f32| harmonics_db(&harmonics(m, drive));
		let tape = db(Mode::Tape, 12.0);
		assert!(tape[2] > -30.0, "tape adds odd harmonics: {tape:?}");
		assert!(tape[1] < -100.0 && tape[3] < -100.0, "but no even ones");
		let tube = db(Mode::Tube, 12.0);
		assert!(tube[1] > -30.0, "tube adds a strong 2nd: {tube:?}");
		let hard = db(Mode::Hard, 12.0);
		assert!(hard[2] > -20.0 && hard[1] < -100.0, "{hard:?}");
		let clean = db(Mode::Hard, 0.0);
		assert!(
			clean[1..].iter().all(|d| *d < -100.0),
			"0.85 never clips at 0 dB"
		);
		assert_eq!(bar_height(0.0), 1.0);
		assert_eq!(bar_height(-40.0), 0.5);
		assert_eq!(bar_height(-200.0), 0.02);
		assert!(is_even(1) && !is_even(2));
	}

	#[test]
	fn the_test_scope_runs_the_sine_through_the_curve() {
		let s = test_scope(Mode::Hard, 12.0, 2.5, 100);
		assert_eq!(s.len(), 101);
		let peak_in = s.iter().map(|p| p.1.abs()).fold(0.0, f32::max);
		let peak_out = s.iter().map(|p| p.2.abs()).fold(0.0, f32::max);
		assert!((peak_in - TEST_LEVEL).abs() < 0.01);
		assert!(peak_out <= TEST_LEVEL + 1e-5);
		// Hard clipped at 12 dB: most of the cycle sits on the ceiling.
		let flat = s.iter().filter(|p| p.2.abs() > TEST_LEVEL - 1e-3).count();
		assert!(flat > 40, "{flat}");
	}

	#[test]
	fn the_scope_triggers_on_a_rising_zero_crossing() {
		let samples: Vec<f32> = (0..400)
			.map(|i| (std::f32::consts::TAU * i as f32 / 100.0 + 1.0).sin())
			.collect();
		let start = trigger(&samples, 150);
		assert!(start <= 250);
		assert!(samples[start - 1] < 0.0 && samples[start] >= 0.0);
		assert_eq!(trigger(&samples[..10], 150), 0, "too short");
		assert_eq!(trigger(&[0.5; 300], 100), 200, "no crossing: the newest");
	}
}
