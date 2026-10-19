//! Pure logic of the Reverb editor (`reverb`, design `16 Effects`, 16b): the tail
//! envelope (early reflections, lows/highs decay), the time axis, the pre-delay / decay /
//! damping node gestures, the space box (size/width) and the readout texts. Not
//! sample-exact with the processor's FDN — it only has to move the way the params do. The
//! view turns the changes into one command per gesture.

use cadenza_dsp::{DeviceInfo, ParamInfo};

pub use crate::state::device::minus;

pub const SIZE: &str = "size";
pub const DECAY: &str = "decay";
pub const DAMPING: &str = "damping";
pub const PRE_DELAY: &str = "pre_delay";
pub const WIDTH: &str = "width";
pub const LOW_CUT: &str = "low_cut";
pub const MIX: &str = "mix";

/// Below this level the tail is drawn as silence.
pub const FLOOR_DB: f32 = -60.0;
/// The bottom of the tail picture: a little below the RT60 line so the decay node sits
/// clear of the edge.
pub const PICTURE_FLOOR_DB: f32 = -66.0;
/// Early reflection ticks drawn after the pre-delay.
pub const EARLY_REFLECTIONS: usize = 10;
/// The tail builds up over this long after the pre-delay (the early reflections sit on
/// top of the build-up).
pub const ONSET_S: f32 = 0.06;
/// How far below its peak the tail starts its build-up.
const ONSET_DB: f32 = 18.0;
/// The decay is drawn slightly convex in dB (faster early fall, like a real room), still
/// reaching -60 dB exactly at the RT60 point.
const CURVE: f32 = 0.85;

/// A parameter change: (param id, plain value).
pub type Change = (&'static str, f32);

pub fn info() -> &'static DeviceInfo {
	cadenza_dsp::device_info("reverb").expect("the reverb is a built-in")
}

fn param(id: &str) -> &'static ParamInfo {
	info().param(id).expect("a reverb param")
}

fn clamp(id: &str, v: f32) -> f32 {
	let p = param(id);
	v.clamp(p.min, p.max)
}

fn round1(v: f32) -> f32 {
	(v * 10.0).round() / 10.0
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ReverbValues {
	pub size: f32,
	pub decay: f32,
	pub damping: f32,
	pub pre_delay: f32,
	pub width: f32,
	pub low_cut: f32,
}

impl Default for ReverbValues {
	fn default() -> Self {
		Self::from_fn(|p| p.default)
	}
}

impl ReverbValues {
	pub fn from_fn(f: impl Fn(&ParamInfo) -> f32) -> Self {
		Self {
			size: f(param(SIZE)),
			decay: f(param(DECAY)),
			damping: f(param(DAMPING)),
			pre_delay: f(param(PRE_DELAY)),
			width: f(param(WIDTH)),
			low_cut: f(param(LOW_CUT)),
		}
	}

	pub fn pre_delay_seconds(&self) -> f32 {
		self.pre_delay * 0.001
	}

	/// Same shape as the processor's own `size_factor` (echo density/spacing), reused here
	/// only to place the early-reflection ticks; the picture does not need to be exact.
	pub fn size_factor(&self) -> f32 {
		0.4 + 1.2 * self.size * 0.01
	}

	/// The whole tail's time span, including the pre-delay and a little breathing room
	/// after the RT60 point, clamped to a sane viewing window.
	pub fn view_span(&self) -> f32 {
		(self.pre_delay_seconds() + self.decay * 1.3).clamp(0.6, 26.0)
	}

	/// How much faster the damped highs fall than the lows.
	fn highs_rate(&self) -> f32 {
		1.0 + self.damping * 0.01
	}

	/// The envelope `rel` seconds after the pre-delay at `rate` times the RT60 speed,
	/// unclamped (0 dB at the start, -60 dB at the RT60 point).
	fn envelope_db(&self, rel: f32, rate: f32) -> f32 {
		-60.0 * (rel.max(0.0) * rate / self.decay.max(0.05)).powf(CURVE)
	}

	/// The low (undamped) tail level at `t` seconds from the start of the picture; -60 dB
	/// at the RT60 point, silent before the pre-delay ends.
	pub fn lows_db(&self, t: f32) -> f32 {
		let rel = t - self.pre_delay_seconds();
		if rel < 0.0 {
			return FLOOR_DB;
		}
		self.envelope_db(rel, 1.0).max(FLOOR_DB)
	}

	/// The damped (high-frequency) tail level: falls faster the more `damping`.
	pub fn highs_db(&self, t: f32) -> f32 {
		let rel = t - self.pre_delay_seconds();
		if rel < 0.0 {
			return FLOOR_DB;
		}
		self.envelope_db(rel, self.highs_rate()).max(FLOOR_DB)
	}

	/// The tail as drawn: the envelope with its build-up after the pre-delay and a small
	/// fixed texture (so it reads as a decaying sound, not a ruler), from 0 to `span`
	/// seconds in `n` points of (seconds, dB), floored at [`PICTURE_FLOOR_DB`].
	pub fn tail_points(&self, highs: bool, span: f32, n: usize) -> Vec<(f32, f32)> {
		let pd = self.pre_delay_seconds();
		let rate = if highs { self.highs_rate() } else { 1.0 };
		let n = n.max(2);
		let mut out = Vec::with_capacity(n + 2);
		// A vertical rise at the pre-delay instead of a slope from the previous sample.
		if pd > 0.0 {
			out.push((0.0, PICTURE_FLOOR_DB));
			out.push((pd, PICTURE_FLOOR_DB));
		}
		for i in 0..n {
			let t = pd + (span - pd).max(0.0) * i as f32 / (n - 1) as f32;
			let rel = t - pd;
			let build = (rel / ONSET_S).min(1.0);
			let fade = (1.0 - rel / (self.decay / rate).max(0.05)).clamp(0.0, 1.0);
			let db = self.envelope_db(rel, rate) - (1.0 - build) * ONSET_DB
				+ texture(i as u32 + u32::from(highs) * 977) * 1.5 * fade;
			out.push((t, db.clamp(PICTURE_FLOOR_DB, 0.0)));
		}
		out
	}

	/// Times (seconds from the picture's start) of the early-reflection ticks, closer
	/// together for a small room and more spread out for a large one, a little irregular
	/// like real wall bounces.
	pub fn early_reflection_times(&self) -> [f32; EARLY_REFLECTIONS] {
		let step = 0.011 * self.size_factor();
		let start = self.pre_delay_seconds();
		std::array::from_fn(|i| start + step * (i as f32 + 0.6 + 0.35 * texture(i as u32 + 31)))
	}

	/// The level of early reflection `i`: each later bounce a little quieter.
	pub fn early_reflection_db(i: usize) -> f32 {
		-1.5 - i as f32 * 1.6 - (texture(i as u32 + 7) + 0.5) * 2.0
	}
}

/// A fixed pseudo-random value in -0.5..0.5 for `i` (same picture every frame).
fn texture(i: u32) -> f32 {
	let h = i
		.wrapping_mul(0x9e37_79b9)
		.rotate_left(13)
		.wrapping_mul(0x85eb_ca6b);
	(h >> 8) as f32 / (1u32 << 24) as f32 - 0.5
}

/// Fraction of the RT60 at which the damping node sits on the highs, where the lows/highs
/// gap it controls is already wide. The decay node sits at the RT60 point itself, on the
/// -60 dB line (above the picture's floor, so it is easy to grab).
const DAMPING_NODE_FRACTION: f32 = 0.4;

/// Where the decay node sits on the time axis: the RT60 point, so a horizontal drag moves
/// the decay by exactly the time dragged.
pub fn decay_node_time(v: &ReverbValues) -> f32 {
	v.pre_delay_seconds() + v.decay
}

/// Where the damping node sits on the time axis.
pub fn damping_node_time(v: &ReverbValues) -> f32 {
	v.pre_delay_seconds() + v.decay * DAMPING_NODE_FRACTION
}

/// The damping that puts the highs at `db` under the damping node (the inverse of
/// [`ReverbValues::highs_db`] there), so the node follows the pointer: lower = more
/// damping.
pub fn damping_for_node_db(db: f32) -> f32 {
	let u = (-db / 60.0).max(0.0).powf(CURVE.recip());
	clamp(DAMPING, round1((u / DAMPING_NODE_FRACTION - 1.0) * 100.0))
}

/// The wet low cut's magnitude at `freq` (the processor's own high-pass, Q 0.7).
pub fn low_cut_response_db(low_cut: f32, freq: f32) -> f32 {
	use cadenza_dsp::util::biquad::BiquadCoeffs;
	const SR: f32 = 96_000.0;
	BiquadCoeffs::highpass(low_cut, 0.7, SR).magnitude_db(freq, SR)
}

/// Room picture proportions (0..1 of the space box) for `size` and `width` (%): the room
/// grows with the size, and the width stretches it sideways.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RoomShape {
	/// Front wall width and height as fractions of the box.
	pub w: f32,
	pub h: f32,
	/// The back wall's scale of the front one (bigger rooms look deeper).
	pub depth: f32,
	/// Speaker spread, as a fraction of the front width.
	pub spread: f32,
}

const ROOM_W: (f32, f32) = (0.34, 0.86);
const ROOM_H: (f32, f32) = (0.30, 0.70);

pub fn room_shape(size_pct: f32, width_pct: f32) -> RoomShape {
	let s = (size_pct * 0.01).clamp(0.0, 1.0);
	let w = (width_pct * 0.01).clamp(0.0, 1.0);
	let full = ROOM_W.0 + (ROOM_W.1 - ROOM_W.0) * s;
	RoomShape {
		w: full * (0.6 + 0.4 * w),
		h: ROOM_H.0 + (ROOM_H.1 - ROOM_H.0) * s,
		depth: 0.62 - 0.17 * s,
		spread: 0.1 + 0.36 * w,
	}
}

/// Pixels the room's corner moves per % of size in a box `box_w` x `box_h` (diagonal
/// average), so a corner drag follows the pointer.
pub fn size_px_per_pct(box_w: f32, box_h: f32, width_pct: f32) -> f32 {
	let w = (width_pct * 0.01).clamp(0.0, 1.0);
	let dx = (ROOM_W.1 - ROOM_W.0) * (0.6 + 0.4 * w) * box_w * 0.5;
	let dy = (ROOM_H.1 - ROOM_H.0) * box_h * 0.5;
	((dx + dy) * 0.5 * 0.01).max(0.05)
}

/// Pixels the room's side edge moves per % of width (half the front wall's growth).
pub fn width_px_per_pct(box_w: f32, size_pct: f32) -> f32 {
	let s = (size_pct * 0.01).clamp(0.0, 1.0);
	let full = ROOM_W.0 + (ROOM_W.1 - ROOM_W.0) * s;
	(full * 0.4 * box_w * 0.5 * 0.01).max(0.05)
}

/// 0..1 position of `time_s` on the picture's time axis of `span` seconds.
pub fn time_to_t(time_s: f32, span: f32) -> f32 {
	(time_s / span.max(0.01)).clamp(0.0, 1.0)
}

/// Time in seconds at axis position `t` (0..1) of `span` seconds.
pub fn t_to_time(t: f32, span: f32) -> f32 {
	t.clamp(0.0, 1.0) * span
}

/// The pre-delay after dragging its node by `delta_s` seconds (ms, rounded to 1 ms).
pub fn drag_pre_delay(start_ms: f32, delta_s: f32) -> f32 {
	clamp(PRE_DELAY, (start_ms + delta_s * 1000.0).round())
}

/// The decay (RT60) after dragging its node by `delta_s` seconds.
pub fn drag_decay(start_s: f32, delta_s: f32) -> f32 {
	clamp(DECAY, round1(start_s + delta_s))
}

/// The decay after `notches` wheel notches over the tail (geometric, Shift fine).
pub fn wheel_decay(start_s: f32, notches: f32, fine: bool) -> f32 {
	let step: f32 = if fine { 1.03 } else { 1.08 };
	clamp(DECAY, round1(start_s.max(0.05) * step.powf(notches)))
}

/// The damping after dragging its node vertically by `delta_pct` (up = more damping).
pub fn drag_damping(start_pct: f32, delta_pct: f32) -> f32 {
	clamp(DAMPING, round1(start_pct + delta_pct))
}

/// The room size after `notches` wheel notches over the space box.
pub fn wheel_size(start_pct: f32, notches: f32) -> f32 {
	clamp(SIZE, round1(start_pct + notches * 2.0))
}

/// The size after dragging a corner of the space box by `delta_pct`.
pub fn drag_size(start_pct: f32, delta_pct: f32) -> f32 {
	clamp(SIZE, round1(start_pct + delta_pct))
}

/// The width after dragging the space box's side edge by `delta_pct`.
pub fn drag_width(start_pct: f32, delta_pct: f32) -> f32 {
	clamp(WIDTH, round1(start_pct + delta_pct))
}

pub fn reset_pre_delay() -> Change {
	(PRE_DELAY, param(PRE_DELAY).default)
}

pub fn reset_decay() -> Change {
	(DECAY, param(DECAY).default)
}

pub fn reset_damping() -> Change {
	(DAMPING, param(DAMPING).default)
}

pub fn reset_size() -> Change {
	(SIZE, param(SIZE).default)
}

pub fn reset_width() -> Change {
	(WIDTH, param(WIDTH).default)
}

/// "Pre-delay 40 ms".
pub fn pre_delay_label(ms: f32) -> String {
	format!("Pre-delay {ms:.0} ms")
}

/// "Decay 2.4 s".
pub fn decay_label(s: f32) -> String {
	format!("Decay {s:.1} s")
}

/// "Damping 40 %".
pub fn damping_label(pct: f32) -> String {
	format!("Damping {pct:.0} %")
}

/// "Size 62 % · Width 80 %".
pub fn space_label(size_pct: f32, width_pct: f32) -> String {
	format!("Size {size_pct:.0} % · Width {width_pct:.0} %")
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn defaults_come_from_the_processor() {
		let v = ReverbValues::default();
		assert_eq!(v.size, 50.0);
		assert_eq!(v.decay, 2.0);
		assert_eq!(v.damping, 50.0);
		assert_eq!(v.pre_delay, 20.0);
		assert_eq!(v.width, 100.0);
		assert_eq!(v.low_cut, 150.0);
	}

	#[test]
	fn node_drags_clamp_into_their_ranges() {
		assert_eq!(drag_pre_delay(20.0, 0.05), 70.0);
		assert_eq!(drag_pre_delay(20.0, -1.0), 0.0);
		assert_eq!(drag_pre_delay(20.0, 10.0), 250.0);
		assert_eq!(drag_decay(2.0, 0.5), 2.5);
		assert_eq!(drag_decay(2.0, -5.0), 0.2);
		assert_eq!(drag_decay(2.0, 30.0), 20.0);
		assert_eq!(drag_damping(50.0, 10.0), 60.0);
		assert_eq!(drag_damping(50.0, -200.0), 0.0);
		assert_eq!(drag_size(50.0, 60.0), 100.0);
		assert_eq!(drag_width(80.0, -200.0), 0.0);
	}

	#[test]
	fn wheel_steps_decay_and_size() {
		assert!(wheel_decay(2.0, 1.0, false) > 2.0);
		assert!(wheel_decay(2.0, -1.0, false) < 2.0);
		let coarse = wheel_decay(2.0, 1.0, false) - 2.0;
		let fine = wheel_decay(2.0, 1.0, true) - 2.0;
		assert!(fine < coarse, "Shift steps more finely");
		assert_eq!(wheel_size(50.0, 30.0), 100.0, "clamped");
		assert_eq!(wheel_decay(0.0, -5.0, false), 0.2, "clamped at the floor");
	}

	#[test]
	fn resets_are_the_processor_defaults() {
		assert_eq!(reset_pre_delay(), (PRE_DELAY, 20.0));
		assert_eq!(reset_decay(), (DECAY, 2.0));
		assert_eq!(reset_damping(), (DAMPING, 50.0));
		assert_eq!(reset_size(), (SIZE, 50.0));
		assert_eq!(reset_width(), (WIDTH, 100.0));
	}

	#[test]
	fn tail_envelope_decays_and_damps() {
		let v = ReverbValues {
			size: 50.0,
			decay: 2.0,
			damping: 0.0,
			pre_delay: 20.0,
			width: 100.0,
			low_cut: 150.0,
		};
		// Silent before the pre-delay, 0 dB right after it, -60 at the RT60 point.
		assert_eq!(v.lows_db(0.0), FLOOR_DB);
		assert!((v.lows_db(v.pre_delay_seconds()) - 0.0).abs() < 1e-3);
		assert!((v.lows_db(v.pre_delay_seconds() + 2.0) - -60.0).abs() < 1e-3);
		// No damping: highs track the lows exactly.
		assert_eq!(v.highs_db(1.0), v.lows_db(1.0));
		let damped = ReverbValues { damping: 80.0, ..v };
		let t = v.pre_delay_seconds() + 0.5;
		assert!(
			damped.highs_db(t) < damped.lows_db(t),
			"more damping falls the highs faster"
		);
	}

	#[test]
	fn early_reflections_spread_with_the_room_size() {
		let small = ReverbValues {
			size: 10.0,
			..ReverbValues::default()
		};
		let big = ReverbValues {
			size: 90.0,
			..ReverbValues::default()
		};
		let s = small.early_reflection_times();
		let b = big.early_reflection_times();
		assert!(s.is_sorted());
		assert!(b[5] - b[0] > s[5] - s[0], "a bigger room spreads them out");
		assert!(s[0] > small.pre_delay_seconds(), "after the pre-delay");
	}

	#[test]
	fn view_span_grows_with_decay() {
		let short = ReverbValues {
			decay: 0.5,
			..ReverbValues::default()
		};
		let long = ReverbValues {
			decay: 10.0,
			..ReverbValues::default()
		};
		assert!(long.view_span() > short.view_span());
	}

	#[test]
	fn time_axis_round_trips() {
		for t in [0.0_f32, 0.3, 0.75, 1.0] {
			let time = t_to_time(t, 6.0);
			assert!((time_to_t(time, 6.0) - t).abs() < 1e-5);
		}
		assert_eq!(time_to_t(-1.0, 6.0), 0.0);
		assert_eq!(time_to_t(100.0, 6.0), 1.0);
	}

	#[test]
	fn node_placements_move_with_their_params() {
		let v = ReverbValues::default();
		let longer = ReverbValues {
			decay: v.decay * 2.0,
			..v
		};
		assert!(decay_node_time(&longer) > decay_node_time(&v));
		assert!(damping_node_time(&longer) > damping_node_time(&v));
		assert!(
			decay_node_time(&v) > damping_node_time(&v),
			"decay sits further out than damping"
		);
	}

	#[test]
	fn the_drawn_tail_builds_up_then_decays_inside_the_picture() {
		let v = ReverbValues {
			pre_delay: 40.0,
			decay: 2.4,
			damping: 40.0,
			..ReverbValues::default()
		};
		let span = v.view_span();
		let lows = v.tail_points(false, span, 200);
		let highs = v.tail_points(true, span, 200);
		assert_eq!(
			lows[0],
			(0.0, PICTURE_FLOOR_DB),
			"silent before the pre-delay"
		);
		assert!(
			lows.iter()
				.all(|&(_, db)| (PICTURE_FLOOR_DB..=0.0).contains(&db))
		);
		assert!(
			lows.iter().map(|p| p.0).is_sorted(),
			"time runs left to right"
		);
		// Builds up over the onset, then follows the envelope within the texture.
		let at = |pts: &[(f32, f32)], t: f32| {
			pts.iter()
				.min_by(|a, b| (a.0 - t).abs().total_cmp(&(b.0 - t).abs()))
				.unwrap()
				.1
		};
		let pd = v.pre_delay_seconds();
		assert!(at(&lows, pd) < at(&lows, pd + ONSET_S) - 10.0);
		let mid = pd + 1.0;
		assert!((at(&lows, mid) - v.lows_db(mid)).abs() < 1.5);
		assert!(
			at(&highs, mid) < at(&lows, mid) - 5.0,
			"damped highs fall faster"
		);
		assert_eq!(lows.last().unwrap().1, PICTURE_FLOOR_DB, "gone by the end");
	}

	#[test]
	fn the_decay_is_gently_curved_in_db() {
		let v = ReverbValues::default();
		let pd = v.pre_delay_seconds();
		let half = v.lows_db(pd + v.decay * 0.5);
		assert!(
			half < -30.0,
			"falls faster early than a straight line: {half}"
		);
		assert!(half > -40.0, "only slightly: {half}");
	}

	#[test]
	fn the_damping_node_follows_the_pointer() {
		for damping in [0.0_f32, 25.0, 40.0, 80.0, 100.0] {
			let v = ReverbValues {
				damping,
				..ReverbValues::default()
			};
			let db = v.highs_db(damping_node_time(&v));
			assert!((damping_for_node_db(db) - damping).abs() < 0.2, "{damping}");
		}
		assert_eq!(damping_for_node_db(0.0), 0.0, "clamped");
		assert_eq!(damping_for_node_db(-200.0), 100.0, "clamped");
		assert!(
			damping_for_node_db(-45.0) > damping_for_node_db(-35.0),
			"lower = more"
		);
	}

	#[test]
	fn early_reflections_get_quieter() {
		let levels: Vec<f32> = (0..EARLY_REFLECTIONS)
			.map(ReverbValues::early_reflection_db)
			.collect();
		assert!(levels.windows(2).all(|w| w[1] < w[0] + 0.5));
		assert!(levels[EARLY_REFLECTIONS - 1] < levels[0] - 10.0);
		assert!(levels.iter().all(|&db| db < 0.0 && db > -30.0));
	}

	#[test]
	fn low_cut_response_is_a_high_pass() {
		assert!(low_cut_response_db(180.0, 20.0) < -20.0);
		assert!((low_cut_response_db(180.0, 180.0) + 3.0).abs() < 1.0);
		assert!(low_cut_response_db(180.0, 5_000.0).abs() < 0.2);
	}

	#[test]
	fn the_room_grows_with_size_and_stretches_with_width() {
		let small = room_shape(10.0, 80.0);
		let big = room_shape(90.0, 80.0);
		assert!(big.w > small.w && big.h > small.h);
		assert!(big.depth < small.depth, "a bigger room looks deeper");
		let narrow = room_shape(60.0, 0.0);
		let wide = room_shape(60.0, 100.0);
		assert!(wide.w > narrow.w && wide.spread > narrow.spread);
		assert_eq!(wide.h, narrow.h, "width does not change the height");
		assert!(big.w <= 1.0 && big.h <= 1.0);
		// The drag scales match the shape's own growth.
		let px = width_px_per_pct(200.0, 60.0);
		let edge = |w: f32| room_shape(60.0, w).w * 200.0 * 0.5;
		assert!((edge(51.0) - edge(50.0) - px).abs() < 1e-3);
		assert!(size_px_per_pct(200.0, 200.0, 80.0) > 0.0);
	}

	#[test]
	fn readable_labels() {
		assert_eq!(pre_delay_label(40.0), "Pre-delay 40 ms");
		assert_eq!(decay_label(2.4), "Decay 2.4 s");
		assert_eq!(damping_label(40.0), "Damping 40 %");
		assert_eq!(space_label(62.0, 80.0), "Size 62 % · Width 80 %");
	}
}
