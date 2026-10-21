//! Pure logic of the Delay editor (`delay`, design `16 Effects`, 16c): echo tap
//! positions/levels/darkening on the beat (or free-time) timeline, the sync snapping of
//! the first echo's drag, the S/P key toggles and the readout text. The view turns the
//! changes into one command per gesture.

use crate::i18n::{tr, trf};
use cadenza_dsp::response::delay_sync_steps;
use cadenza_dsp::{DeviceInfo, ParamInfo};

pub use crate::state::device::{minus, short_choice};

pub const TIME: &str = "time";
pub const SYNC: &str = "sync";
pub const FEEDBACK: &str = "feedback";
pub const PING_PONG: &str = "ping_pong";
pub const HIGH_CUT: &str = "high_cut";
pub const LOW_CUT: &str = "low_cut";
pub const MIX: &str = "mix";
/// Multi-tap: how many read heads (1..4) and the gap between them (% of the echo time).
pub const TAPS: &str = "taps";
pub const TAP_SPREAD: &str = "tap_spread";
/// Level / pan of taps 2, 3, 4.
pub const TAP_LEVELS: [&str; 3] = ["tap2_level", "tap3_level", "tap4_level"];
pub const TAP_PANS: [&str; 3] = ["tap2_pan", "tap3_pan", "tap4_pan"];

pub const MAX_DELAY_MS: f32 = 4_000.0;
/// The `sync` index of "1/8", the double-click reset.
pub const SYNC_1_8: usize = 6;
pub const RESET_FEEDBACK: f32 = 40.0;
/// Echoes quieter than this (as feedback^n) are not drawn.
const TAP_FLOOR: f32 = 0.03;
pub const MAX_TAPS: usize = 12;

/// A parameter change: (param id, plain value).
pub type Change = (&'static str, f32);

pub fn info() -> &'static DeviceInfo {
	cadenza_dsp::device_info("delay").expect("the delay is a built-in")
}

fn param(id: &str) -> &'static ParamInfo {
	info().param(id).expect("a delay param")
}

fn clamp(id: &str, v: f32) -> f32 {
	let p = param(id);
	v.clamp(p.min, p.max)
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DelValues {
	pub time: f32,
	pub sync: f32,
	pub feedback: f32,
	pub ping_pong: f32,
	pub high_cut: f32,
	pub low_cut: f32,
	pub taps: f32,
	pub tap_spread: f32,
	pub tap_levels: [f32; 3],
	pub tap_pans: [f32; 3],
}

/// One of taps 2..4 as the picture draws it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ExtraTap {
	/// 2, 3 or 4.
	pub n: usize,
	pub ms: f32,
	/// 0..1.
	pub level: f32,
	/// -1..1.
	pub pan: f32,
}

impl Default for DelValues {
	fn default() -> Self {
		Self::from_fn(|p| p.default)
	}
}

impl DelValues {
	pub fn from_fn(f: impl Fn(&ParamInfo) -> f32) -> Self {
		Self {
			time: f(param(TIME)),
			sync: f(param(SYNC)),
			feedback: f(param(FEEDBACK)),
			ping_pong: f(param(PING_PONG)),
			high_cut: f(param(HIGH_CUT)),
			low_cut: f(param(LOW_CUT)),
			taps: f(param(TAPS)),
			tap_spread: f(param(TAP_SPREAD)),
			tap_levels: TAP_LEVELS.map(|id| f(param(id))),
			tap_pans: TAP_PANS.map(|id| f(param(id))),
		}
	}

	/// The plain values in the processor's param order (mix at its default).
	fn params(&self) -> Vec<f32> {
		info()
			.params
			.iter()
			.map(|p| match p.id {
				TIME => self.time,
				SYNC => self.sync,
				FEEDBACK => self.feedback,
				PING_PONG => self.ping_pong,
				HIGH_CUT => self.high_cut,
				LOW_CUT => self.low_cut,
				TAPS => self.taps,
				TAP_SPREAD => self.tap_spread,
				id => TAP_LEVELS
					.iter()
					.position(|&t| t == id)
					.map(|i| self.tap_levels[i])
					.or_else(|| {
						TAP_PANS
							.iter()
							.position(|&t| t == id)
							.map(|i| self.tap_pans[i])
					})
					.unwrap_or(p.default),
			})
			.collect()
	}

	/// Taps 2..4 in use (empty with one tap), at the processor's exact times.
	pub fn extra_taps(&self, bpm: f64) -> Vec<ExtraTap> {
		let taps = cadenza_dsp::response::delay_taps(&self.params(), bpm);
		taps.as_slice()
			.iter()
			.enumerate()
			.skip(1)
			.map(|(i, t)| ExtraTap {
				n: i + 1,
				ms: (t.seconds * 1000.0) as f32,
				level: t.level,
				pan: t.pan,
			})
			.collect()
	}

	pub fn is_free(&self) -> bool {
		self.sync.round() <= 0.0
	}

	/// The echo time in milliseconds at `bpm` (matches the processor exactly).
	pub fn echo_ms(&self, bpm: f64) -> f32 {
		(cadenza_dsp::response::delay_time_seconds(&self.params(), bpm) * 1000.0) as f32
	}
}

/// Milliseconds of sync step `i` at `bpm` (index 0 is free time: not meaningful here).
fn step_ms(i: usize, bpm: f64) -> f32 {
	(delay_sync_steps()[i].1 * 60_000.0 / bpm) as f32
}

/// The sync step closest to `ms` at `bpm` (never index 0, the free step).
pub fn nearest_sync_step(ms: f32, bpm: f64) -> usize {
	(1..delay_sync_steps().len())
		.min_by(|&a, &b| {
			let da = (step_ms(a, bpm) - ms).abs();
			let db = (step_ms(b, bpm) - ms).abs();
			da.total_cmp(&db)
		})
		.unwrap_or(SYNC_1_8)
}

/// What dragging the first echo changes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TimeChange {
	/// Set `sync` to this step (synced mode: the drag snaps to the nearest note value).
	Sync(usize),
	/// Set `time` to these milliseconds (free mode).
	Free(f32),
}

/// The first echo dragged by `delta_ms` from `start_ms`: in free mode this is a plain
/// time edit, in sync mode it snaps to the nearest note value at `bpm`. The param model
/// only has a free `time` while `sync` is 0, so a synced drag always snaps (there is no
/// free-while-synced state to hold Shift's "free ms" in).
pub fn drag_first_echo(start_ms: f32, delta_ms: f32, bpm: f64, free_mode: bool) -> TimeChange {
	let ms = (start_ms + delta_ms).clamp(1.0, MAX_DELAY_MS);
	if free_mode {
		TimeChange::Free(ms.round())
	} else {
		TimeChange::Sync(nearest_sync_step(ms, bpm))
	}
}

/// The feedback after dragging an echo's height by `delta_pct` (up = more feedback).
pub fn drag_feedback(start_pct: f32, delta_pct: f32) -> f32 {
	clamp(FEEDBACK, ((start_pct + delta_pct) * 10.0).round() / 10.0)
}

/// The feedback after `notches` wheel notches over the echoes.
pub fn wheel_feedback(start_pct: f32, notches: f32) -> f32 {
	drag_feedback(start_pct, notches * 2.0)
}

/// Double-click: time → 1/8 (synced), feedback → 40 %.
pub fn reset() -> [Change; 2] {
	[(SYNC, SYNC_1_8 as f32), (FEEDBACK, RESET_FEEDBACK)]
}

/// S: toggles Sync/Free (free = `sync` 0; back on picks 1/8, the model keeps no memory
/// of the step last used).
pub fn toggle_sync(sync: f32) -> f32 {
	if sync.round() <= 0.0 {
		SYNC_1_8 as f32
	} else {
		0.0
	}
}

/// Dragging tap `n` (2..4): `dx_ms` along the time axis moves it, and with it every
/// extra tap (they share `tap_spread`, tap n sits at echo x (1 + (n-1) x spread));
/// `dy` (0..1 of the picture's half height, up = louder) is its level. Returns
/// `(spread %, level %)`.
pub fn drag_tap(
	n: usize,
	echo_ms: f32,
	start_spread: f32,
	start_level: f32,
	dx_ms: f32,
	dy: f32,
) -> (f32, f32) {
	let steps = (n.max(2) - 1) as f32;
	let spread = start_spread + dx_ms / (echo_ms.max(1.0) * steps) * 100.0;
	(
		clamp(TAP_SPREAD, spread.round()),
		clamp(TAP_LEVELS[0], (start_level + dy * 100.0).round()),
	)
}

/// A tap's pan after `notches` wheel notches (up = right), in 0.05 steps.
pub fn wheel_pan(pan: f32, notches: f32) -> f32 {
	(((pan + notches * 0.1) * 20.0).round() / 20.0).clamp(-1.0, 1.0)
}

/// "L50" / "C" / "R30".
pub fn pan_label(pan: f32) -> String {
	let pct = (pan * 100.0).round() as i32;
	match pct {
		0 => "C".to_owned(),
		p if p < 0 => format!("L{}", -p),
		p => format!("R{p}"),
	}
}

/// "Tap 2 · 70 % · L50".
pub fn tap_tag(tap: &ExtraTap) -> String {
	trf(
		"Tap {n} · {level} % · {pan}",
		&[
			("n", &tap.n.to_string()),
			(
				"level",
				&crate::i18n::decimal(format!("{:.0}", tap.level * 100.0)),
			),
			("pan", &pan_label(tap.pan)),
		],
	)
}

/// Double-click on tap `n` (2..4): level and pan back to their defaults.
pub fn reset_tap(n: usize) -> [Change; 2] {
	let i = n.clamp(2, 4) - 2;
	[
		(TAP_LEVELS[i], param(TAP_LEVELS[i]).default),
		(TAP_PANS[i], param(TAP_PANS[i]).default),
	]
}

/// P: toggles ping-pong.
pub fn toggle_ping_pong(ping_pong: f32) -> f32 {
	if ping_pong >= 0.5 { 0.0 } else { 1.0 }
}

/// How many repeats are still loud enough to draw (feedback^n >= [`TAP_FLOOR`]), at least
/// one so the first echo always shows.
pub fn tap_count(feedback_pct: f32) -> usize {
	let fb = (feedback_pct * 0.01).clamp(0.0, 0.95);
	if fb < 0.05 {
		return 1;
	}
	let n = (TAP_FLOOR.ln() / fb.ln()).ceil().max(1.0) as usize;
	n.clamp(1, MAX_TAPS)
}

/// The level (0..1) of repeat `n` (1 = the first echo).
pub fn tap_level(n: u32, feedback_pct: f32) -> f32 {
	(feedback_pct * 0.01).clamp(0.0, 0.95).powi(n as i32)
}

/// L/R lane of repeat `n` while ping-pong is on (the first echo bounces left).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lane {
	Left,
	Right,
	Center,
}

pub fn tap_lane(n: u32, ping_pong: bool) -> Lane {
	if !ping_pong {
		Lane::Center
	} else if n % 2 == 1 {
		Lane::Left
	} else {
		Lane::Right
	}
}

/// How darkened repeat `n` is (0 bright .. 1 fully dark): each repeat loses a fraction of
/// its remaining brightness through the band-pass, faster the narrower the feedback
/// filter is. Not a dB-accurate render of the filter, just a picture that tightens with it.
pub fn tap_darkness(n: u32, low_cut: f32, high_cut: f32) -> f32 {
	let full_octaves = (20_000.0_f32 / 20.0).log2();
	let band_octaves = (high_cut / low_cut.max(1.0)).max(1.0).log2();
	let narrow = (1.0 - band_octaves / full_octaves).clamp(0.0, 1.0);
	let loss = 0.06 + narrow * 0.30;
	1.0 - (1.0 - loss).powi(n as i32 - 1)
}

/// The time span of the taps picture (ms): every drawn repeat plus a little headroom; in
/// sync mode rounded up to whole beats (two at least), so the beat grid ends on a line.
pub fn view_ms(v: &DelValues, bpm: f64) -> f32 {
	let last_tap = v.extra_taps(bpm).last().map_or(0.0, |t| t.ms);
	let raw = (v.echo_ms(bpm) * (tap_count(v.feedback) as f32 + 0.6))
		.max(last_tap * 1.15)
		.clamp(80.0, 16_000.0);
	if v.is_free() {
		return raw;
	}
	let beat = beat_ms(bpm);
	(raw / beat).ceil().max(2.0) * beat
}

fn beat_ms(bpm: f64) -> f32 {
	(60_000.0 / bpm.max(1.0)) as f32
}

/// How strong a grid line is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GridKind {
	Bar,
	Beat,
	/// Sixteenths (sync) or the unlabelled half steps (free).
	Minor,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GridLine {
	pub ms: f32,
	pub kind: GridKind,
	/// "2" on a bar, "·3" on a beat, "250 ms" on a free-time step.
	pub label: Option<String>,
}

/// The time grid of the taps picture over `view_ms`: bars/beats/sixteenths at `bpm` when
/// synced (4/4 counting, bar 1 at 0), a round millisecond step when free. Lines denser
/// than `min_px` apart at `px_per_ms` are left out (labels need twice that).
pub fn grid_lines(
	view_ms: f32,
	bpm: f64,
	free: bool,
	px_per_ms: f32,
	min_px: f32,
) -> Vec<GridLine> {
	let mut out = Vec::new();
	if free {
		// Wide enough for "1000 ms" labels.
		let step = nice_ms_step((min_px * 12.0 / px_per_ms.max(1e-4)).max(1.0));
		let half = step * 0.5;
		let mut i = 1;
		while i as f32 * half <= view_ms + 0.01 {
			let ms = i as f32 * half;
			let major = i % 2 == 0;
			out.push(GridLine {
				ms,
				kind: if major {
					GridKind::Beat
				} else {
					GridKind::Minor
				},
				label: major.then(|| format!("{ms:.0} ms")),
			});
			i += 1;
		}
		return out;
	}
	let beat = beat_ms(bpm);
	let sixteenth = beat * 0.25;
	let show_minor = sixteenth * px_per_ms >= min_px;
	let label_beats = beat * px_per_ms >= min_px * 4.0;
	let mut i = 1;
	while i as f32 * sixteenth <= view_ms + 0.01 {
		let ms = i as f32 * sixteenth;
		let (kind, label) = if i % 16 == 0 {
			(GridKind::Bar, Some(format!("{}", i / 16 + 1)))
		} else if i % 4 == 0 {
			let label = label_beats.then(|| format!("\u{b7}{}", (i / 4) % 4 + 1));
			(GridKind::Beat, label)
		} else {
			(GridKind::Minor, None)
		};
		if kind != GridKind::Minor || show_minor {
			out.push(GridLine { ms, kind, label });
		}
		i += 1;
	}
	out
}

/// A readable millisecond step at or above `target_ms` (1-2-5 per decade).
pub fn nice_ms_step(target_ms: f32) -> f32 {
	let exp = target_ms.max(1.0).log10().floor();
	let base = 10f32.powf(exp);
	for m in [1.0, 2.0, 5.0] {
		if target_ms <= base * m {
			return base * m;
		}
	}
	base * 10.0
}

/// The feedback filter's magnitude at `freq`: the processor's own high-pass + low-pass
/// (Q 0.707) in series.
pub fn filter_response_db(low_cut: f32, high_cut: f32, freq: f32) -> f32 {
	use cadenza_dsp::util::biquad::BiquadCoeffs;
	const SR: f32 = 96_000.0;
	BiquadCoeffs::highpass(low_cut, 0.707, SR).magnitude_db(freq, SR)
		+ BiquadCoeffs::lowpass(high_cut, 0.707, SR).magnitude_db(freq, SR)
}

/// The level (0..1 of the picture's half height) the first echo's tag sits at: between
/// the first echo's head and the next repeat on the same side, so it covers neither.
pub fn tag_level(feedback_pct: f32, ping_pong: bool) -> f32 {
	let next = if ping_pong { 3 } else { 2 };
	(tap_level(1, feedback_pct) + tap_level(next, feedback_pct)) * 0.5
}

/// "1/8 · 326 ms · drag to change" (free: "326 ms · drag to change").
pub fn echo_tag(v: &DelValues, bpm: f64) -> String {
	let ms = v.echo_ms(bpm);
	if v.is_free() {
		trf(
			"{ms} ms · drag to change",
			&[("ms", &crate::i18n::decimal(format!("{:.0}", ms)))],
		)
	} else {
		trf(
			"{short_choice} · {ms} ms · drag to change",
			&[
				(
					"short_choice",
					&short_choice(delay_sync_steps()[v.sync.round().clamp(0.0, 12.0) as usize].0),
				),
				("ms", &crate::i18n::decimal(format!("{:.0}", ms))),
			],
		)
	}
}

/// "feedback 60 % · 92 BPM".
pub fn feedback_tag(v: &DelValues, bpm: f64) -> String {
	trf(
		"feedback {feedback} % · {bpm} BPM",
		&[
			(
				"feedback",
				&crate::i18n::decimal(format!("{:.0}", v.feedback)),
			),
			("bpm", &crate::i18n::decimal(format!("{:.0}", bpm))),
		],
	)
}

/// "1/8 · 326 ms · feedback 60 % · 92 BPM" (free: "Free · 326 ms · ...").
pub fn readout(v: &DelValues, bpm: f64) -> String {
	let name = if v.is_free() {
		tr("Free").to_owned()
	} else {
		short_choice(delay_sync_steps()[v.sync.round().clamp(0.0, 12.0) as usize].0)
	};
	minus(trf(
		"{name} · {echo_ms} ms · feedback {feedback} % · {bpm} BPM",
		&[
			("name", &name),
			(
				"echo_ms",
				&crate::i18n::decimal(format!("{:.0}", v.echo_ms(bpm))),
			),
			(
				"feedback",
				&crate::i18n::decimal(format!("{:.0}", v.feedback)),
			),
			("bpm", &crate::i18n::decimal(format!("{:.0}", bpm))),
		],
	))
}

/// "Feedback filter 200 Hz – 6 kHz".
pub fn filter_label(low_cut: f32, high_cut: f32) -> String {
	trf(
		"Feedback filter {low} – {high}",
		&[
			("low", &crate::state::device::format_value(low_cut, "Hz")),
			("high", &crate::state::device::format_value(high_cut, "Hz")),
		],
	)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn defaults_come_from_the_processor() {
		let v = DelValues::default();
		assert_eq!(v.time, 375.0);
		assert_eq!(v.sync, 6.0);
		assert_eq!(v.feedback, 35.0);
		assert_eq!(v.ping_pong, 0.0);
		assert!(!v.is_free());
	}

	#[test]
	fn echo_ms_matches_the_processor() {
		let v = DelValues {
			sync: 9.0,
			..DelValues::default()
		};
		assert!((v.echo_ms(120.0) - 500.0).abs() < 0.5, "1/4 at 120 bpm");
		let free = DelValues {
			sync: 0.0,
			time: 300.0,
			..DelValues::default()
		};
		assert!(free.is_free());
		assert!((free.echo_ms(120.0) - 300.0).abs() < 0.5);
	}

	#[test]
	fn drag_snaps_in_sync_mode_and_is_free_otherwise() {
		match drag_first_echo(500.0, 20.0, 120.0, false) {
			TimeChange::Sync(i) => assert_eq!(i, 9, "closest to 1/4 (500 ms at 120 bpm)"),
			TimeChange::Free(_) => panic!("expected a snap"),
		}
		match drag_first_echo(500.0, 20.0, 120.0, true) {
			TimeChange::Free(ms) => assert_eq!(ms, 520.0),
			TimeChange::Sync(_) => panic!("expected free time"),
		}
		match drag_first_echo(1.0, -1000.0, 120.0, true) {
			TimeChange::Free(ms) => assert_eq!(ms, 1.0, "clamped"),
			_ => panic!(),
		}
	}

	#[test]
	fn feedback_drag_wheel_and_reset() {
		assert_eq!(drag_feedback(35.0, 10.0), 45.0);
		assert_eq!(drag_feedback(35.0, 1000.0), 95.0, "clamped");
		assert_eq!(drag_feedback(35.0, -1000.0), 0.0, "clamped");
		assert_eq!(wheel_feedback(35.0, 2.0), 39.0);
		assert_eq!(reset(), [(SYNC, 6.0), (FEEDBACK, 40.0)]);
	}

	#[test]
	fn s_and_p_toggle() {
		assert_eq!(toggle_sync(6.0), 0.0);
		assert_eq!(toggle_sync(0.0), 6.0);
		assert_eq!(toggle_ping_pong(0.0), 1.0);
		assert_eq!(toggle_ping_pong(1.0), 0.0);
	}

	#[test]
	fn taps_fall_off_with_feedback() {
		assert_eq!(tap_count(0.0), 1);
		assert!(tap_count(80.0) > tap_count(20.0));
		assert_eq!(tap_level(1, 50.0), 0.5);
		assert!((tap_level(2, 50.0) - 0.25).abs() < 1e-5);
		assert!(tap_level(3, 50.0) < tap_level(2, 50.0));
	}

	#[test]
	fn lanes_alternate_with_ping_pong() {
		assert_eq!(tap_lane(1, false), Lane::Center);
		assert_eq!(tap_lane(1, true), Lane::Left);
		assert_eq!(tap_lane(2, true), Lane::Right);
		assert_eq!(tap_lane(3, true), Lane::Left);
	}

	#[test]
	fn darkness_grows_with_repeats_and_a_narrower_filter() {
		let wide = (0..1)
			.map(|_| tap_darkness(1, 20.0, 20_000.0))
			.next()
			.unwrap();
		assert!(wide < 1e-4, "the first echo is still bright");
		assert!(tap_darkness(4, 20.0, 20_000.0) > tap_darkness(1, 20.0, 20_000.0));
		assert!(
			tap_darkness(4, 200.0, 2_000.0) > tap_darkness(4, 20.0, 20_000.0),
			"a narrower filter darkens faster"
		);
	}

	#[test]
	fn readout_text() {
		let v = DelValues {
			sync: 6.0,
			feedback: 60.0,
			..DelValues::default()
		};
		assert_eq!(
			readout(&v, 92.0),
			"1/8 \u{b7} 326 ms \u{b7} feedback 60 % \u{b7} 92 BPM"
		);
		let free = DelValues {
			sync: 0.0,
			time: 300.0,
			feedback: 60.0,
			..DelValues::default()
		};
		assert_eq!(
			readout(&free, 92.0),
			"Free \u{b7} 300 ms \u{b7} feedback 60 % \u{b7} 92 BPM"
		);
	}

	#[test]
	fn the_view_ends_on_a_beat_or_bar_line() {
		let v = DelValues {
			sync: 6.0,
			feedback: 60.0,
			..DelValues::default()
		};
		let beat = 60_000.0 / 92.0;
		let span = view_ms(&v, 92.0);
		assert!(
			span >= v.echo_ms(92.0) * tap_count(60.0) as f32,
			"every tap fits"
		);
		assert!(
			(span / beat - (span / beat).round()).abs() < 1e-3,
			"whole beats: {span}"
		);
		assert!(
			span < v.echo_ms(92.0) * (tap_count(60.0) as f32 + 0.6) + beat,
			"tight"
		);
		let short = DelValues {
			sync: 1.0,
			feedback: 0.0,
			..DelValues::default()
		};
		assert!(
			(view_ms(&short, 92.0) - beat * 2.0).abs() < 0.5,
			"two beats at least"
		);
		let free = DelValues {
			sync: 0.0,
			time: 300.0,
			feedback: 0.0,
			..DelValues::default()
		};
		assert!(
			(view_ms(&free, 92.0) - 480.0).abs() < 0.5,
			"free: no rounding"
		);
	}

	#[test]
	fn grid_lines_label_bars_and_beats() {
		let beat = 500.0;
		let lines = grid_lines(beat * 8.0, 120.0, false, 0.2, 6.0);
		let bar = lines.iter().find(|l| l.kind == GridKind::Bar).unwrap();
		assert!((bar.ms - 2_000.0).abs() < 0.01);
		assert_eq!(bar.label.as_deref(), Some("2"));
		let beats: Vec<_> = lines
			.iter()
			.filter(|l| l.kind == GridKind::Beat)
			.map(|l| l.label.clone().unwrap())
			.collect();
		assert_eq!(beats[..3], ["\u{b7}2", "\u{b7}3", "\u{b7}4"]);
		assert!(
			lines.iter().any(|l| l.kind == GridKind::Minor),
			"sixteenths at 25 px"
		);
		// Zoomed out: no sixteenths, no beat labels.
		let dense = grid_lines(beat * 8.0, 120.0, false, 0.02, 6.0);
		assert!(dense.iter().all(|l| l.kind != GridKind::Minor));
		assert!(
			dense
				.iter()
				.filter(|l| l.kind == GridKind::Beat)
				.all(|l| l.label.is_none())
		);
		// Free time: round steps in ms.
		let free = grid_lines(1_000.0, 120.0, true, 0.9, 6.0);
		let labelled: Vec<_> = free.iter().filter_map(|l| l.label.clone()).collect();
		assert_eq!(labelled[0], "100 ms");
		assert_eq!(nice_ms_step(130.0), 200.0);
		assert_eq!(nice_ms_step(40.0), 50.0);
	}

	#[test]
	fn filter_response_passes_the_band() {
		assert!(filter_response_db(200.0, 6_000.0, 1_000.0).abs() < 0.5);
		assert!(filter_response_db(200.0, 6_000.0, 30.0) < -20.0);
		assert!(filter_response_db(200.0, 6_000.0, 18_000.0) < -15.0);
	}

	#[test]
	fn tags_name_the_echo_and_sit_between_taps() {
		let v = DelValues {
			sync: 6.0,
			feedback: 60.0,
			..DelValues::default()
		};
		assert_eq!(
			echo_tag(&v, 92.0),
			"1/8 \u{b7} 326 ms \u{b7} drag to change"
		);
		assert_eq!(feedback_tag(&v, 92.0), "feedback 60 % \u{b7} 92 BPM");
		let l = tag_level(60.0, true);
		assert!(l < tap_level(1, 60.0) && l > tap_level(3, 60.0));
		assert!(tag_level(60.0, false) > tap_level(2, 60.0));
	}

	#[test]
	fn filter_label_text() {
		assert_eq!(
			filter_label(200.0, 6_000.0),
			"Feedback filter 200 Hz \u{2013} 6.00 kHz"
		);
	}

	#[test]
	fn extra_taps_follow_the_processor() {
		let v = DelValues {
			sync: 0.0,
			time: 200.0,
			taps: 3.0,
			..DelValues::default()
		};
		let taps = v.extra_taps(120.0);
		assert_eq!(taps.len(), 2);
		assert!((taps[0].ms - 300.0).abs() < 0.01 && (taps[1].ms - 400.0).abs() < 0.01);
		assert_eq!((taps[0].level, taps[0].pan), (0.7, -0.5));
		assert!(DelValues::default().extra_taps(120.0).is_empty());
		assert!(view_ms(&v, 120.0) >= 400.0);
	}

	#[test]
	fn tap_drags_move_the_spread_and_the_level() {
		// Tap 3 dragged 100 ms later at a 200 ms echo: spread +25 %; 0.1 up: level +10.
		assert_eq!(drag_tap(3, 200.0, 50.0, 50.0, 100.0, 0.1), (75.0, 60.0));
		assert_eq!(drag_tap(2, 200.0, 50.0, 50.0, -1000.0, -2.0), (10.0, 0.0));
		assert_eq!(wheel_pan(-0.5, 2.0), -0.3);
		assert_eq!(wheel_pan(0.95, 3.0), 1.0);
		assert_eq!(pan_label(-0.5), "L50");
		assert_eq!(pan_label(0.0), "C");
		assert_eq!(pan_label(0.3), "R30");
		assert_eq!(reset_tap(3), [("tap3_level", 50.0), ("tap3_pan", 0.5)]);
	}
}
