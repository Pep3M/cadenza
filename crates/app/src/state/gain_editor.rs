//! Pure logic of the Gain / Utility editor (`gain`, design `16 Effects`, 16f): the
//! goniometer's point mapping (mid up, side across, L and R on the diagonals), its
//! auto scale, the correlation meter and its colours, and the goniometer drag (across =
//! pan, up = width). The view turns the changes into one command per gesture.

use cadenza_dsp::{DeviceInfo, ParamInfo};

pub use crate::state::compressor_editor::signed_db;
pub use crate::state::device::minus;

pub const GAIN: &str = "gain";
pub const PAN: &str = "pan";
pub const WIDTH: &str = "width";
pub const INVERT_L: &str = "invert_left";
pub const INVERT_R: &str = "invert_right";

/// Points kept for the goniometer (about 43 ms at 48 kHz).
pub const GONIO_POINTS: usize = 2048;
/// The goniometer's loudest point lands this far out (1 = the diamond's corner).
const GONIO_FILL: f32 = 0.85;
/// Quieter material is magnified at most this much.
const GONIO_MAX_ZOOM: f32 = 16.0;

/// A parameter change: (param id, plain value).
pub type Change = (&'static str, f32);

pub fn info() -> &'static DeviceInfo {
	cadenza_dsp::device_info("gain").expect("the gain utility is a built-in")
}

fn param(id: &str) -> &'static ParamInfo {
	info().param(id).expect("a gain param")
}

fn clamp(id: &str, v: f32) -> f32 {
	let p = param(id);
	v.clamp(p.min, p.max)
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GainValues {
	pub gain: f32,
	pub pan: f32,
	pub width: f32,
	pub invert_left: bool,
	pub invert_right: bool,
}

impl Default for GainValues {
	fn default() -> Self {
		Self::from_fn(|p| p.default)
	}
}

impl GainValues {
	pub fn from_fn(f: impl Fn(&ParamInfo) -> f32) -> Self {
		Self {
			gain: f(param(GAIN)),
			pan: f(param(PAN)),
			width: f(param(WIDTH)),
			invert_left: f(param(INVERT_L)) >= 0.5,
			invert_right: f(param(INVERT_R)) >= 0.5,
		}
	}
}

/// A stereo frame on the goniometer, in diamond units: `x` = side (R right of centre,
/// L left), `y` = mid (up). L alone lands on the upper-left edge, R alone on the
/// upper-right one, mono on the vertical, anti-phase on the horizontal. `|x| + |y|`
/// never exceeds the larger channel times `scale`.
pub fn gonio_point(l: f32, r: f32, scale: f32) -> (f32, f32) {
	((r - l) * 0.5 * scale, (l + r) * 0.5 * scale)
}

/// The zoom that puts the loudest of `frames` near the diamond's corners.
pub fn gonio_scale(frames: &[[f32; 2]]) -> f32 {
	let peak = frames
		.iter()
		.map(|f| f[0].abs().max(f[1].abs()))
		.fold(0.0f32, f32::max);
	if peak <= 1e-6 {
		1.0
	} else {
		(GONIO_FILL / peak).clamp(GONIO_FILL, GONIO_MAX_ZOOM)
	}
}

/// Correlation of L and R over `frames` (-1 anti-phase .. +1 mono); silence reads 0
/// and a one-sided signal (only L or only R) 0 as well.
pub fn correlation(frames: &[[f32; 2]]) -> f32 {
	let (mut lr, mut ll, mut rr) = (0.0f64, 0.0f64, 0.0f64);
	for f in frames {
		let (l, r) = (f64::from(f[0]), f64::from(f[1]));
		lr += l * r;
		ll += l * l;
		rr += r * r;
	}
	let d = (ll * rr).sqrt();
	if d <= 1e-12 {
		0.0
	} else {
		(lr / d).clamp(-1.0, 1.0) as f32
	}
}

/// How the correlation meter reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
	Fine,
	/// Below 0: amber.
	Wide,
	/// Below -0.5: coral, mono playback will cancel.
	Cancelling,
}

pub fn phase_of(correlation: f32) -> Phase {
	if correlation < -0.5 {
		Phase::Cancelling
	} else if correlation < 0.0 {
		Phase::Wide
	} else {
		Phase::Fine
	}
}

/// "+0.82" / "−0.40".
pub fn correlation_text(c: f32) -> String {
	minus(format!("{c:+.2}"))
}

/// Pan and width after a drag on the goniometer: `dx` across (`px_per_pct` points per
/// pan percent, right = right), `dy` down (`px_per_width_pct` per width percent, up =
/// wider); whole percents.
pub fn drag_gonio(
	start: (f32, f32),
	dx: f32,
	dy: f32,
	px_per_pct: f32,
	px_per_width_pct: f32,
) -> (f32, f32) {
	let pan = start.0 + dx / px_per_pct.max(0.01);
	let width = start.1 - dy / px_per_width_pct.max(0.01);
	(clamp(PAN, pan.round()), clamp(WIDTH, width.round()))
}

/// Double-click on the goniometer: centred and unchanged width.
pub fn reset_image() -> [Change; 2] {
	[(PAN, 0.0), (WIDTH, 100.0)]
}

/// M: mono (width 0), or back to unchanged when already mono.
pub fn toggle_mono(width: f32) -> Change {
	(WIDTH, if width <= 0.0 { 100.0 } else { 0.0 })
}

/// The undo label of a goniometer drag that changed `pan` and/or `width`.
pub fn image_label(pan: bool, width: bool) -> &'static str {
	match (pan, width) {
		(true, true) => "Gain pan and width",
		(true, false) => "Gain pan",
		_ => "Gain width",
	}
}

/// "C", "L 30", "R 100".
pub fn pan_text(pan: f32) -> String {
	let p = pan.round();
	if p == 0.0 {
		"C".to_owned()
	} else if p < 0.0 {
		format!("L {:.0}", -p)
	} else {
		format!("R {p:.0}")
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn channels_land_on_the_diamonds_axes() {
		let (x, y) = gonio_point(1.0, 0.0, 1.0);
		assert!(x < 0.0 && y > 0.0 && (x + y).abs() < 1e-6, "L: up-left");
		let (x, y) = gonio_point(0.0, 1.0, 1.0);
		assert!(x > 0.0 && y > 0.0 && (x - y).abs() < 1e-6, "R: up-right");
		assert_eq!(gonio_point(0.5, 0.5, 1.0), (0.0, 0.5), "mono: vertical");
		assert_eq!(
			gonio_point(-0.5, 0.5, 1.0),
			(0.5, 0.0),
			"anti-phase: across"
		);
		for (l, r) in [(1.0, -1.0), (1.0, 1.0), (-0.7, 0.2)] {
			let (x, y) = gonio_point(l, r, 1.0);
			assert!(x.abs() + y.abs() <= 1.0 + 1e-6, "inside the diamond");
		}
		assert_eq!(gonio_point(0.5, 0.5, 2.0), (0.0, 1.0));
	}

	#[test]
	fn the_scale_zooms_quiet_signals_up_to_a_limit() {
		assert_eq!(gonio_scale(&[]), 1.0);
		assert!((gonio_scale(&[[0.5, -0.25]]) - 1.7).abs() < 1e-5);
		assert_eq!(gonio_scale(&[[1e-4, 0.0]]), 16.0);
		assert_eq!(
			gonio_scale(&[[2.0, 0.0]]),
			GONIO_FILL,
			"loud is never shrunk"
		);
	}

	#[test]
	fn correlation_reads_mono_wide_and_anti_phase() {
		let sine = |phase: f32| -> Vec<[f32; 2]> {
			(0..480)
				.map(|i| {
					let w = std::f32::consts::TAU * i as f32 / 48.0;
					[w.sin(), (w + phase).sin()]
				})
				.collect()
		};
		assert!((correlation(&sine(0.0)) - 1.0).abs() < 1e-4);
		assert!((correlation(&sine(std::f32::consts::PI)) + 1.0).abs() < 1e-4);
		assert!(correlation(&sine(std::f32::consts::FRAC_PI_2)).abs() < 1e-3);
		assert_eq!(correlation(&[[0.0, 0.0]; 10]), 0.0, "silence");
		assert_eq!(correlation(&[[0.5, 0.0]; 10]), 0.0, "one side only");
		assert_eq!(phase_of(0.82), Phase::Fine);
		assert_eq!(phase_of(-0.2), Phase::Wide);
		assert_eq!(phase_of(-0.6), Phase::Cancelling);
		assert_eq!(correlation_text(0.82), "+0.82");
		assert_eq!(correlation_text(-0.4), "\u{2212}0.40");
	}

	#[test]
	fn the_gonio_drag_pans_across_and_widens_up() {
		assert_eq!(drag_gonio((0.0, 100.0), 30.0, 0.0, 1.5, 1.0), (20.0, 100.0));
		assert_eq!(drag_gonio((0.0, 100.0), 0.0, -40.0, 1.0, 2.0), (0.0, 120.0));
		assert_eq!(
			drag_gonio((0.0, 100.0), -1e4, 1e4, 1.0, 1.0),
			(-100.0, 0.0),
			"clamped"
		);
		assert_eq!(toggle_mono(120.0), (WIDTH, 0.0));
		assert_eq!(toggle_mono(0.0), (WIDTH, 100.0));
		assert_eq!(image_label(true, false), "Gain pan");
		assert_eq!(pan_text(0.2), "C");
		assert_eq!(pan_text(-30.0), "L 30");
		assert_eq!(pan_text(100.0), "R 100");
	}
}
