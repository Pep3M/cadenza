//! Time <-> pixel mapping, zoom, adaptive grid and snapping for the arrange view.
//!
//! Pixel coordinates here are relative to the left edge of the timeline area; the view
//! adds its screen offset. Positions are kept in beats (quarter notes, f64) like the
//! AI-facing API; edits convert to [`Tick`] at the edge.

use cadenza_core::{PPQ, TempoMap, Tick, TimeSignature};

/// Narrowest grid cell drawn or snapped to, in pixels.
pub const MIN_GRID_PX: f32 = 12.0;
pub const MIN_PX_PER_BEAT: f32 = 2.0;
pub const MAX_PX_PER_BEAT: f32 = 800.0;

/// Horizontal view of the song.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Timeline {
	/// Zoom: pixels per quarter note.
	pub px_per_beat: f32,
	/// Beat at the left edge of the view (>= 0).
	pub scroll_beats: f64,
}

impl Default for Timeline {
	fn default() -> Self {
		Self {
			px_per_beat: 24.0,
			scroll_beats: 0.0,
		}
	}
}

/// Grid of the current zoom level: `step` is the snap/minor line spacing, `bar` the
/// major one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Grid {
	pub step: Tick,
	pub bar: Tick,
}

impl Timeline {
	pub fn beat_to_x(&self, beat: f64) -> f32 {
		((beat - self.scroll_beats) * f64::from(self.px_per_beat)) as f32
	}

	pub fn x_to_beat(&self, x: f32) -> f64 {
		self.scroll_beats + f64::from(x) / f64::from(self.px_per_beat)
	}

	pub fn tick_to_x(&self, tick: Tick) -> f32 {
		self.beat_to_x(tick.beats())
	}

	pub fn x_to_tick(&self, x: f32) -> Tick {
		Tick::from_beats(self.x_to_beat(x))
	}

	/// Width in pixels of a duration.
	pub fn ticks_to_px(&self, ticks: Tick) -> f32 {
		(ticks.beats() * f64::from(self.px_per_beat)) as f32
	}

	/// Duration covered by `px` pixels.
	pub fn px_to_ticks(&self, px: f32) -> Tick {
		Tick::from_beats(f64::from(px) / f64::from(self.px_per_beat))
	}

	/// Multiplies the zoom by `factor`, keeping the beat under `anchor_x` in place.
	pub fn zoom_around(&mut self, factor: f32, anchor_x: f32) {
		let anchor = self.x_to_beat(anchor_x);
		self.px_per_beat = (self.px_per_beat * factor).clamp(MIN_PX_PER_BEAT, MAX_PX_PER_BEAT);
		self.scroll_beats = (anchor - f64::from(anchor_x) / f64::from(self.px_per_beat)).max(0.0);
	}

	/// Scrolls by `dx` pixels (positive = later in the song).
	pub fn scroll_px(&mut self, dx: f32) {
		self.scroll_beats = (self.scroll_beats + f64::from(dx / self.px_per_beat)).max(0.0);
	}

	/// Zooms so `beats` fill `width` pixels, starting at 0.
	pub fn fit(&mut self, beats: f64, width: f32) {
		if beats > 0.0 && width > 0.0 {
			self.px_per_beat =
				((f64::from(width) / beats) as f32).clamp(MIN_PX_PER_BEAT, MAX_PX_PER_BEAT);
		}
		self.scroll_beats = 0.0;
	}

	/// Pages the view so `beat` (the playhead) stays visible in a view `width` wide.
	/// Returns true when it scrolled.
	pub fn follow(&mut self, beat: f64, width: f32) -> bool {
		let x = self.beat_to_x(beat);
		if x < 0.0 || x > width * 0.92 {
			self.scroll_beats = (beat - f64::from(width * 0.05 / self.px_per_beat)).max(0.0);
			return true;
		}
		false
	}

	/// The finest musical grid whose cells are at least [`MIN_GRID_PX`] wide: a power-of-two
	/// division of the beat, a beat, or a power-of-two number of bars.
	pub fn grid(&self, ts: TimeSignature) -> Grid {
		let bar = ts.ticks_per_bar();
		let beat = Tick(PPQ * 4 / i64::from(ts.denominator.max(1)));
		let min = self.px_to_ticks(MIN_GRID_PX).0.max(1);
		let mut step = beat;
		if step.0 >= min {
			// Subdivide while the half still fits.
			while step.0 % 2 == 0 && step.0 / 2 >= min && step.0 / 2 >= PPQ / 16 {
				step = Tick(step.0 / 2);
			}
		} else if bar.0 < min {
			step = bar;
			while step.0 < min {
				step = Tick(step.0 * 2);
			}
		} else {
			step = bar;
		}
		Grid { step, bar }
	}

	/// Rounds `tick` to the nearest grid line (never below 0).
	pub fn snap(&self, tick: Tick, ts: TimeSignature) -> Tick {
		snap_to(tick, self.grid(ts).step)
	}

	/// Snaps down to the grid line at or before `tick`.
	pub fn snap_floor(&self, tick: Tick, ts: TimeSignature) -> Tick {
		let step = self.grid(ts).step.0.max(1);
		Tick(tick.0.max(0).div_euclid(step) * step)
	}
}

pub fn snap_to(tick: Tick, step: Tick) -> Tick {
	let step = step.0.max(1);
	Tick(((tick.0 as f64 / step as f64).round() as i64 * step).max(0))
}

/// Bars:beats:ticks display (1-based bars and beats; ticks within the beat), e.g. `5.2.240`.
pub fn format_bbt(tick: Tick, ts: TimeSignature) -> String {
	let (bar, beat, ticks) = bbt(tick, ts);
	format!("{bar}.{beat}.{ticks:03}")
}

/// (bar, beat, tick-in-beat), 1-based bar and beat. Assumes a constant time signature.
pub fn bbt(tick: Tick, ts: TimeSignature) -> (i64, i64, i64) {
	let t = tick.0.max(0);
	let bar_len = ts.ticks_per_bar().0.max(1);
	let beat_len = (PPQ * 4 / i64::from(ts.denominator.max(1))).max(1);
	let bar = t / bar_len;
	let in_bar = t % bar_len;
	(bar + 1, in_bar / beat_len + 1, in_bar % beat_len)
}

/// Clock time `m:ss.mmm` of `tick`.
pub fn format_time(tick: Tick, tempo: &TempoMap) -> String {
	let secs = tempo.tick_to_seconds(Tick(tick.0.max(0)));
	let ms = (secs * 1000.0).round() as u64;
	format!("{}:{:02}.{:03}", ms / 60_000, (ms / 1000) % 60, ms % 1000)
}

#[cfg(test)]
mod tests {
	use super::*;

	const FOUR_FOUR: TimeSignature = TimeSignature {
		numerator: 4,
		denominator: 4,
	};

	#[test]
	fn time_pixel_mapping_round_trips() {
		let t = Timeline {
			px_per_beat: 40.0,
			scroll_beats: 8.0,
		};
		assert_eq!(t.beat_to_x(8.0), 0.0);
		assert_eq!(t.beat_to_x(10.0), 80.0);
		assert_eq!(t.x_to_beat(80.0), 10.0);
		assert_eq!(t.x_to_tick(t.tick_to_x(Tick(PPQ * 13))), Tick(PPQ * 13));
		assert_eq!(t.ticks_to_px(Tick(PPQ)), 40.0);
		assert_eq!(t.px_to_ticks(20.0), Tick(PPQ / 2));
	}

	#[test]
	fn zoom_keeps_the_anchor_beat_in_place() {
		let mut t = Timeline {
			px_per_beat: 20.0,
			scroll_beats: 4.0,
		};
		let anchor = 200.0;
		let before = t.x_to_beat(anchor);
		t.zoom_around(2.0, anchor);
		assert_eq!(t.px_per_beat, 40.0);
		assert!((t.x_to_beat(anchor) - before).abs() < 1e-9);
		t.zoom_around(1e6, 0.0);
		assert_eq!(t.px_per_beat, MAX_PX_PER_BEAT);
		t.zoom_around(1e-9, 0.0);
		assert_eq!(t.px_per_beat, MIN_PX_PER_BEAT);
		t.scroll_px(-1e9);
		assert_eq!(t.scroll_beats, 0.0, "never scrolls before the song start");
	}

	#[test]
	fn grid_adapts_to_zoom() {
		let at = |px| Timeline {
			px_per_beat: px,
			scroll_beats: 0.0,
		};
		// 200 px per beat: subdivides down to 1/64 notes (12.5 px), the finest grid.
		assert_eq!(at(200.0).grid(FOUR_FOUR).step, Tick(PPQ / 16));
		assert_eq!(at(24.0).grid(FOUR_FOUR).step, Tick(PPQ / 2));
		assert_eq!(at(12.0).grid(FOUR_FOUR).step, Tick(PPQ));
		// 4 px per beat: a beat is too small, a bar (16 px) fits.
		assert_eq!(at(4.0).grid(FOUR_FOUR).step, Tick(PPQ * 4));
		// 2 px per beat: bars are 8 px, so two bars.
		assert_eq!(at(2.0).grid(FOUR_FOUR).step, Tick(PPQ * 8));
		let six_eight = TimeSignature {
			numerator: 6,
			denominator: 8,
		};
		let g = at(24.0).grid(six_eight);
		assert_eq!(g.bar, Tick(PPQ * 3));
		assert_eq!(g.step, Tick(PPQ / 2), "eighth-note beats");
	}

	#[test]
	fn snapping() {
		let t = Timeline {
			px_per_beat: 12.0,
			scroll_beats: 0.0,
		};
		assert_eq!(t.snap(Tick(PPQ * 3 + 400), FOUR_FOUR), Tick(PPQ * 3));
		assert_eq!(t.snap(Tick(PPQ * 3 + 500), FOUR_FOUR), Tick(PPQ * 4));
		assert_eq!(t.snap(Tick(-500), FOUR_FOUR), Tick::ZERO);
		assert_eq!(t.snap_floor(Tick(PPQ * 3 + 900), FOUR_FOUR), Tick(PPQ * 3));
	}

	#[test]
	fn follow_pages_the_playhead_into_view() {
		let mut t = Timeline {
			px_per_beat: 10.0,
			scroll_beats: 0.0,
		};
		assert!(!t.follow(50.0, 1000.0));
		assert!(t.follow(95.0, 1000.0));
		assert!(t.beat_to_x(95.0) >= 0.0 && t.beat_to_x(95.0) < 100.0);
	}

	#[test]
	fn position_formats() {
		assert_eq!(format_bbt(Tick::ZERO, FOUR_FOUR), "1.1.000");
		assert_eq!(
			format_bbt(Tick(PPQ * 4 * 4 + PPQ + 240), FOUR_FOUR),
			"5.2.240"
		);
		let tempo = TempoMap::constant(120.0);
		assert_eq!(format_time(Tick(PPQ * 2 * 61), &tempo), "1:01.000");
		assert_eq!(format_time(Tick(PPQ / 4), &tempo), "0:00.125");
	}
}
