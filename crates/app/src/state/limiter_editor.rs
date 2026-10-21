//! Pure logic of the Limiter editor (`limiter`, design `16 Effects`, 16a): the gestures
//! of the level history (ceiling line, vertical drag = input gain, wheel = release,
//! resets), the loudness target chips (view only, never the sound) and the readout
//! texts. The view turns the changes into one command per gesture.

use crate::i18n::tr;
use cadenza_dsp::{DeviceInfo, ParamInfo};

pub use crate::state::compressor_editor::{gr_text, signed_db};
pub use crate::state::device::minus;
use crate::state::meters::FLOOR_DB;

pub const INPUT_GAIN: &str = "input_gain";
pub const CEILING: &str = "ceiling";
pub const RELEASE: &str = "release";
/// 0 = 1x, 1 = 2x, 2 = 4x.
pub const OVERSAMPLE: &str = "oversample";
pub const OVERSAMPLE_LABELS: [&str; 3] = ["1x", "2x", "4x"];

/// The segment of an `oversample` value.
pub fn oversample_index(v: f32) -> usize {
	(v.round().max(0.0) as usize).min(2)
}

/// "5.7 ms latency": what the limiter delays its output by at `oversample` (the
/// processor's own figure: lookahead + the oversampling filters).
pub fn latency_text(oversample: f32, sample_rate: f32) -> String {
	let samples = cadenza_dsp::response::limiter_latency_samples(oversample, sample_rate);
	crate::i18n::trf(
		"{ms} ms latency",
		&[(
			"ms",
			&crate::i18n::decimal(format!(
				"{:.1}",
				samples as f32 * 1000.0 / sample_rate.max(1.0)
			)),
		)],
	)
}

/// Double-click resets.
pub const RESET_CEILING: f32 = -1.0;
pub const RESET_GAIN: f32 = 0.0;
/// A vertical drag inside the history moves the input gain this much per point (the
/// knobs' 200 px for the whole range).
pub const GAIN_DB_PER_PX: f32 = 24.0 / 200.0;
/// One wheel notch over the history multiplies the release by this.
const RELEASE_STEP: f32 = 1.15;
/// The waveform's top: +4 dBFS of input (after the gain) fits.
pub const WAVE_MAX_DB: f32 = 4.0;

/// A parameter change: (param id, plain value).
pub type Change = (&'static str, f32);

pub fn info() -> &'static DeviceInfo {
	cadenza_dsp::device_info("limiter").expect("the limiter is a built-in")
}

fn param(id: &str) -> &'static ParamInfo {
	info().param(id).expect("a limiter param")
}

fn clamp(id: &str, v: f32) -> f32 {
	let p = param(id);
	v.clamp(p.min, p.max)
}

fn round1(v: f32) -> f32 {
	(v * 10.0).round() / 10.0
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LimValues {
	pub input_gain: f32,
	pub ceiling: f32,
	pub release: f32,
}

impl Default for LimValues {
	fn default() -> Self {
		Self::from_fn(|p| p.default)
	}
}

impl LimValues {
	pub fn from_fn(f: impl Fn(&ParamInfo) -> f32) -> Self {
		Self {
			input_gain: f(param(INPUT_GAIN)),
			ceiling: f(param(CEILING)),
			release: f(param(RELEASE)),
		}
	}
}

/// Input gain after a vertical drag of `dy` points (down = less) from `start`.
pub fn drag_gain(start: f32, dy: f32) -> f32 {
	clamp(INPUT_GAIN, round1(start - dy * GAIN_DB_PER_PX))
}

/// The ceiling a dragged line lands on (0.1 dB steps).
pub fn drag_ceiling(db: f32) -> f32 {
	clamp(CEILING, round1(db))
}

/// Release after `notches` wheel notches (up = longer) from `start`: geometric steps,
/// whole milliseconds from 10 ms up.
pub fn wheel_release(start: f32, notches: f32) -> f32 {
	let v = start.max(1.0) * RELEASE_STEP.powf(notches);
	let v = if v >= 10.0 { v.round() } else { round1(v) };
	clamp(RELEASE, v)
}

/// The double-click reset of the history: the gain back to 0 dB.
pub fn reset_gain() -> Change {
	(INPUT_GAIN, RESET_GAIN)
}

/// The double-click reset of the ceiling line.
pub fn reset_ceiling() -> Change {
	(CEILING, RESET_CEILING)
}

/// The loudness target chips: they only move the green band of the loudness panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Target {
	#[default]
	Streaming,
	Loud,
	Broadcast,
}

impl Target {
	pub const ALL: [Target; 3] = [Target::Streaming, Target::Loud, Target::Broadcast];

	pub fn lufs(self) -> f32 {
		match self {
			Target::Streaming => -14.0,
			Target::Loud => -9.0,
			Target::Broadcast => -23.0,
		}
	}

	pub fn name(self) -> &'static str {
		match self {
			Target::Streaming => tr("Streaming"),
			Target::Loud => tr("Loud"),
			Target::Broadcast => tr("Broadcast"),
		}
	}

	/// "Streaming −14".
	pub fn label(self) -> String {
		minus(format!("{} {:.0}", self.name(), self.lufs()))
	}

	/// The green band: ±1 LU around the target.
	pub fn band(self) -> (f32, f32) {
		(self.lufs() - 1.0, self.lufs() + 1.0)
	}

	/// The LUFS span of the "last 30 s" trace: 4 LU below the target to 2 above, widened
	/// (up to 24 LU) to show the readings in `trace`.
	pub fn trace_range(self, trace: impl IntoIterator<Item = f32>) -> (f32, f32) {
		let (mut lo, mut hi) = (self.lufs() - 4.0, self.lufs() + 2.0);
		for v in trace.into_iter().filter(|&v| v > FLOOR_DB + 0.5) {
			lo = lo.min(v - 1.0);
			hi = hi.max(v + 1.0);
		}
		(lo.max(hi - 24.0), hi)
	}
}

/// Height of a LUFS value in a trace of `range` (0 bottom, 1 top, clamped).
pub fn trace_t(lufs: f32, range: (f32, f32)) -> f32 {
	((lufs - range.0) / (range.1 - range.0).max(0.1)).clamp(0.0, 1.0)
}

/// "−13.8", or "—" before anything was measured.
pub fn lufs_text(lufs: f32) -> String {
	if lufs <= FLOOR_DB + 0.5 {
		"\u{2014}".to_owned()
	} else {
		minus(format!("{lufs:.1}"))
	}
}

/// The true peak stays under the ceiling (what the ✓ says): within the readout's 0.1 dB.
pub fn true_peak_ok(true_peak_db: f32, ceiling: f32) -> bool {
	true_peak_db > FLOOR_DB + 0.5 && true_peak_db <= ceiling + 0.05
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn defaults_come_from_the_processor() {
		let v = LimValues::default();
		assert_eq!((v.input_gain, v.ceiling, v.release), (0.0, -1.0, 100.0));
	}

	#[test]
	fn dragging_up_raises_the_gain_within_its_range() {
		assert_eq!(drag_gain(6.0, -50.0), 12.0);
		assert_eq!(drag_gain(6.0, 25.0), 3.0);
		assert_eq!(drag_gain(6.0, 1_000.0), 0.0);
		assert_eq!(drag_gain(6.0, -1_000.0), 24.0);
		assert_eq!(drag_ceiling(-1.04), -1.0);
		assert_eq!(drag_ceiling(3.0), 0.0);
		assert_eq!(drag_ceiling(-40.0), -24.0);
	}

	#[test]
	fn wheel_steps_the_release_geometrically() {
		let up = wheel_release(100.0, 1.0);
		assert_eq!(up, 115.0);
		assert_eq!(wheel_release(100.0, -1.0), 87.0);
		assert_eq!(wheel_release(2.0, -30.0), 1.0);
		assert_eq!(wheel_release(900.0, 5.0), 1000.0);
		assert_eq!(reset_gain(), (INPUT_GAIN, 0.0));
		assert_eq!(reset_ceiling(), (CEILING, -1.0));
	}

	#[test]
	fn targets_set_the_band_and_the_trace_range() {
		assert_eq!(Target::default(), Target::Streaming);
		assert_eq!(Target::Streaming.label(), "Streaming \u{2212}14");
		assert_eq!(Target::Broadcast.band(), (-24.0, -22.0));
		let range = Target::Streaming.trace_range([-14.5, FLOOR_DB]);
		assert_eq!(range, (-18.0, -12.0));
		assert_eq!(Target::Loud.trace_range([-14.0, -12.0]), (-15.0, -7.0));
		assert_eq!(
			Target::Loud.trace_range([-50.0]),
			(-31.0, -7.0),
			"24 LU at most"
		);
		assert_eq!(trace_t(-14.0, range), 4.0 / 6.0);
		assert_eq!(trace_t(-30.0, range), 0.0);
		assert_eq!(trace_t(0.0, range), 1.0);
	}

	#[test]
	fn readouts() {
		assert_eq!(lufs_text(-13.84), "\u{2212}13.8");
		assert_eq!(lufs_text(FLOOR_DB), "\u{2014}");
		assert!(true_peak_ok(-1.02, -1.0));
		assert!(!true_peak_ok(-0.8, -1.0));
		assert!(!true_peak_ok(FLOOR_DB, -1.0), "nothing measured: no tick");
	}

	#[test]
	fn oversampling_latency_text_follows_the_processor() {
		assert_eq!(oversample_index(0.0), 0);
		assert_eq!(oversample_index(2.4), 2);
		assert_eq!(latency_text(0.0, 48_000.0), "5.0 ms latency");
		assert_eq!(latency_text(2.0, 48_000.0), "5.7 ms latency");
	}
}
