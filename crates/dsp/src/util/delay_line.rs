//! Circular delay buffer with fractional (cubic Hermite) reads.

#[derive(Debug, Clone, Default)]
pub struct DelayLine {
	buf: Vec<f32>,
	mask: usize,
	write: usize,
}

impl DelayLine {
	/// Allocates room for at least `max_delay` samples of delay. Call from
	/// `prepare` only.
	pub fn allocate(&mut self, max_delay: usize) {
		let size = (max_delay + 4).next_power_of_two();
		self.buf.clear();
		self.buf.resize(size, 0.0);
		self.mask = size - 1;
		self.write = 0;
	}

	/// Longest delay that can be read safely.
	pub fn max_delay(&self) -> usize {
		self.buf.len().saturating_sub(4)
	}

	pub fn clear(&mut self) {
		self.buf.fill(0.0);
		self.write = 0;
	}

	#[inline]
	pub fn push(&mut self, x: f32) {
		if self.buf.is_empty() {
			return;
		}
		self.buf[self.write] = x;
		self.write = (self.write + 1) & self.mask;
	}

	/// Sample written `delay` pushes ago (`delay >= 1`; 1 = most recent).
	#[inline]
	pub fn read_int(&self, delay: usize) -> f32 {
		if self.buf.is_empty() {
			return 0.0;
		}
		self.buf[self.write.wrapping_sub(delay.clamp(1, self.mask)) & self.mask]
	}

	/// Interpolated read; `delay` is clamped to `1.0..=max_delay`.
	#[inline]
	pub fn read(&self, delay: f32) -> f32 {
		if self.buf.is_empty() {
			return 0.0;
		}
		let d = delay.clamp(1.0, self.max_delay() as f32);
		let di = d as usize;
		let t = d - di as f32;
		let xm1 = self.read_int(di.saturating_sub(1).max(1));
		let x0 = self.read_int(di);
		let x1 = self.read_int(di + 1);
		let x2 = self.read_int(di + 2);
		let c1 = 0.5 * (x1 - xm1);
		let c2 = xm1 - 2.5 * x0 + 2.0 * x1 - 0.5 * x2;
		let c3 = 0.5 * (x2 - xm1) + 1.5 * (x0 - x1);
		((c3 * t + c2) * t + c1) * t + x0
	}
}

/// Schroeder all-pass diffuser built on a fixed-length delay.
#[derive(Debug, Clone, Default)]
pub struct Allpass {
	line: DelayLine,
	len: usize,
}

impl Allpass {
	pub fn allocate(&mut self, len: usize) {
		self.line.allocate(len.max(1));
		self.len = len.max(1);
	}

	pub fn clear(&mut self) {
		self.line.clear();
	}

	#[inline]
	pub fn process(&mut self, x: f32, g: f32) -> f32 {
		let d = self.line.read_int(self.len);
		let v = super::flush(x + g * d);
		self.line.push(v);
		d - g * v
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn integer_and_fractional_reads() {
		let mut d = DelayLine::default();
		d.allocate(64);
		for i in 0..100 {
			d.push(i as f32);
		}
		assert_eq!(d.read_int(1), 99.0);
		assert_eq!(d.read_int(10), 90.0);
		assert!((d.read(10.0) - 90.0).abs() < 1e-4);
		assert!((d.read(10.5) - 89.5).abs() < 1e-3);
	}

	#[test]
	fn unallocated_is_silent() {
		let mut d = DelayLine::default();
		d.push(1.0);
		assert_eq!(d.read(3.0), 0.0);
	}
}
