//! One-pole parameter smoothing to avoid zipper noise.

/// Exponential (one-pole) glide from the current value to a target.
///
/// Until `prepare` is called the smoother is instantaneous, so devices that
/// receive parameters before `prepare` start at the right value.
#[derive(Debug, Clone, Copy)]
pub struct Smoother {
	current: f32,
	target: f32,
	coeff: f32,
}

impl Smoother {
	pub const fn new(value: f32) -> Self {
		Self {
			current: value,
			target: value,
			coeff: 1.0,
		}
	}

	/// Sets the time constant and snaps to the target.
	pub fn prepare(&mut self, sample_rate: f32, time_ms: f32) {
		self.coeff = super::one_pole_coeff(time_ms, sample_rate);
		self.current = self.target;
	}

	#[inline]
	pub fn set_target(&mut self, value: f32) {
		self.target = value;
	}

	/// Jumps to the target immediately.
	#[inline]
	pub fn snap(&mut self) {
		self.current = self.target;
	}

	#[inline]
	pub fn set_immediate(&mut self, value: f32) {
		self.target = value;
		self.current = value;
	}

	#[inline]
	pub fn target(&self) -> f32 {
		self.target
	}

	#[inline]
	pub fn value(&self) -> f32 {
		self.current
	}

	#[inline]
	pub fn is_settled(&self) -> bool {
		self.current == self.target
	}

	#[inline]
	fn epsilon(&self) -> f32 {
		1e-5 * self.target.abs().max(1e-2)
	}

	#[inline]
	pub fn tick(&mut self) -> f32 {
		if self.current != self.target {
			let next = self.current + (self.target - self.current) * self.coeff;
			// Snap once close enough, or when the step underflows f32 precision.
			if next == self.current || (self.target - next).abs() <= self.epsilon() {
				self.current = self.target;
			} else {
				self.current = next;
			}
		}
		self.current
	}

	/// Advances `n` samples at once (control-rate smoothing).
	#[inline]
	pub fn advance(&mut self, n: usize) -> f32 {
		if self.current != self.target {
			let keep = (1.0 - self.coeff).powi(n.min(i32::MAX as usize) as i32);
			self.current = self.target + (self.current - self.target) * keep;
			if (self.target - self.current).abs() <= self.epsilon() {
				self.current = self.target;
			}
		}
		self.current
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn converges_monotonically() {
		let mut s = Smoother::new(0.0);
		s.prepare(48_000.0, 10.0);
		s.set_target(1.0);
		let mut prev = 0.0;
		for _ in 0..48_000 {
			let v = s.tick();
			assert!(v >= prev && v <= 1.0);
			prev = v;
		}
		assert!(s.is_settled());
	}

	#[test]
	fn advance_matches_next() {
		let mut a = Smoother::new(0.0);
		let mut b = Smoother::new(0.0);
		a.prepare(48_000.0, 5.0);
		b.prepare(48_000.0, 5.0);
		a.set_target(2.0);
		b.set_target(2.0);
		for _ in 0..100 {
			a.tick();
		}
		b.advance(100);
		assert!((a.value() - b.value()).abs() < 1e-4);
	}
}
