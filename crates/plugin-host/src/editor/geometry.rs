//! Pure geometry for the host-drawn title strip, independent of X11 so it is
//! unit-testable without a display.
//!
//! Layout: our top-level window is `content_width x (content_height +
//! STRIP_HEIGHT)`; the strip occupies `y in 0..STRIP_HEIGHT` at the top, the
//! plugin's embedded window (CLAP `set_parent` / VST3 `X11EmbedWindowID`)
//! sits below it at `(0, STRIP_HEIGHT)`.

/// Height in pixels of the host-drawn title strip.
pub const STRIP_HEIGHT: u32 = 24;

/// Width/height of the square close button, flush with the strip's right edge.
pub const CLOSE_BUTTON_SIZE: u32 = 24;

/// A rectangle in window-local coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
	pub x: i32,
	pub y: i32,
	pub width: u32,
	pub height: u32,
}

impl Rect {
	pub fn contains(&self, x: i32, y: i32) -> bool {
		x >= self.x
			&& x < self.x + self.width as i32
			&& y >= self.y
			&& y < self.y + self.height as i32
	}
}

/// Outer (top-level) window size that fits a `content_width x content_height`
/// plugin view plus the strip.
pub fn outer_size(content_width: u32, content_height: u32) -> (u32, u32) {
	(content_width, content_height.saturating_add(STRIP_HEIGHT))
}

/// The plugin content size that fits inside an outer window of this size
/// (never below 1 px: a plugin view cannot be zero-sized).
pub fn content_size(outer_width: u32, outer_height: u32) -> (u32, u32) {
	(
		outer_width.max(1),
		outer_height.saturating_sub(STRIP_HEIGHT).max(1),
	)
}

/// Where the embedded content window goes inside the outer one.
pub fn content_offset() -> (i32, i32) {
	(0, STRIP_HEIGHT as i32)
}

/// The strip's close button, in outer-window-local coordinates.
pub fn close_button_rect(outer_width: u32) -> Rect {
	let width = CLOSE_BUTTON_SIZE.min(outer_width);
	Rect {
		x: (outer_width - width) as i32,
		y: 0,
		width,
		height: STRIP_HEIGHT,
	}
}

/// Whether `(x, y)` (outer-window-local) falls inside the strip at all.
pub fn in_strip(y: i32) -> bool {
	(0..STRIP_HEIGHT as i32).contains(&y)
}

/// Whether `(x, y)` hits the close button.
pub fn hit_close_button(outer_width: u32, x: i32, y: i32) -> bool {
	close_button_rect(outer_width).contains(x, y)
}

/// Top-left corner that centers a `window_w x window_h` window on `monitor`.
pub fn centered_on(monitor: Rect, window_w: u32, window_h: u32) -> (i32, i32) {
	let x = monitor.x + (monitor.width as i32 - window_w as i32) / 2;
	let y = monitor.y + (monitor.height as i32 - window_h as i32) / 2;
	(x, y)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn outer_size_adds_the_strip_height() {
		assert_eq!(outer_size(800, 600), (800, 624));
	}

	#[test]
	fn content_size_subtracts_the_strip_height() {
		assert_eq!(content_size(800, 624), (800, 600));
	}

	#[test]
	fn content_size_never_goes_to_zero() {
		// A user resize can briefly shrink below the strip height.
		assert_eq!(content_size(10, 10), (10, 1));
		assert_eq!(content_size(0, 0), (1, 1));
	}

	#[test]
	fn content_offset_is_below_the_strip() {
		assert_eq!(content_offset(), (0, STRIP_HEIGHT as i32));
	}

	#[test]
	fn close_button_sits_at_the_strip_right_edge() {
		let r = close_button_rect(800);
		assert_eq!(
			r,
			Rect {
				x: 800 - 24,
				y: 0,
				width: 24,
				height: 24
			}
		);
	}

	#[test]
	fn close_button_shrinks_on_a_narrow_strip() {
		let r = close_button_rect(10);
		assert_eq!(
			r,
			Rect {
				x: 0,
				y: 0,
				width: 10,
				height: 24
			}
		);
	}

	#[test]
	fn hit_test_close_button() {
		assert!(hit_close_button(800, 790, 10));
		assert!(!hit_close_button(800, 700, 10));
		// Just outside the strip (below it, in plugin territory).
		assert!(!hit_close_button(800, 790, 24));
	}

	#[test]
	fn strip_band_excludes_the_plugin_area() {
		assert!(in_strip(0));
		assert!(in_strip(23));
		assert!(!in_strip(24));
		assert!(!in_strip(-1));
	}

	#[test]
	fn centers_a_window_on_a_monitor_rect() {
		let monitor = Rect {
			x: 0,
			y: 0,
			width: 1920,
			height: 1080,
		};
		assert_eq!(centered_on(monitor, 800, 624), (560, 228));
	}

	#[test]
	fn centers_on_a_monitor_with_an_offset_origin() {
		// A second monitor to the right of a 1920-wide primary.
		let monitor = Rect {
			x: 1920,
			y: 0,
			width: 1080,
			height: 1920,
		};
		assert_eq!(centered_on(monitor, 800, 624), (1920 + 140, 648));
	}
}
