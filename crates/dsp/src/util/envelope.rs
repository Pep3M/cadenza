//! Envelope generators.

/// Level below which a releasing envelope is considered finished (-80 dB).
const SILENCE: f32 = 1e-4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
	Idle,
	Attack,
	Decay,
	Sustain,
	Release,
}

/// ADSR with a linear attack and exponential decay/release (times are the
/// time to fall ~60 dB toward the target, which matches how musicians hear
/// "decay time").
#[derive(Debug, Clone, Copy)]
pub struct Adsr {
	stage: Stage,
	level: f32,
	attack_step: f32,
	decay_coeff: f32,
	sustain: f32,
	release_coeff: f32,
	sample_rate: f32,
}

impl Default for Adsr {
	fn default() -> Self {
		Self {
			stage: Stage::Idle,
			level: 0.0,
			attack_step: 1.0,
			decay_coeff: 0.0,
			sustain: 1.0,
			release_coeff: 0.0,
			sample_rate: 48_000.0,
		}
	}
}

/// Per-sample multiplier that decays by 60 dB over `ms`.
#[inline]
pub fn decay_coeff(ms: f32, sample_rate: f32) -> f32 {
	let samples = (ms * 0.001 * sample_rate).max(1.0);
	(-6.907_755 / samples).exp()
}

impl Adsr {
	pub fn set_sample_rate(&mut self, sample_rate: f32) {
		self.sample_rate = sample_rate;
	}

	/// Times in milliseconds, sustain in `0..=1`.
	pub fn set_params(&mut self, attack_ms: f32, decay_ms: f32, sustain: f32, release_ms: f32) {
		self.attack_step = 1.0 / (attack_ms * 0.001 * self.sample_rate).max(1.0);
		self.decay_coeff = decay_coeff(decay_ms, self.sample_rate);
		self.sustain = sustain.clamp(0.0, 1.0);
		self.release_coeff = decay_coeff(release_ms, self.sample_rate);
	}

	/// Starts (or restarts) the attack from the current level, which keeps
	/// retriggers and voice steals click-free.
	pub fn gate_on(&mut self) {
		self.stage = Stage::Attack;
	}

	pub fn gate_off(&mut self) {
		if self.stage != Stage::Idle {
			self.stage = Stage::Release;
		}
	}

	pub fn reset(&mut self) {
		self.stage = Stage::Idle;
		self.level = 0.0;
	}

	#[inline]
	pub fn stage(&self) -> Stage {
		self.stage
	}

	#[inline]
	pub fn level(&self) -> f32 {
		self.level
	}

	#[inline]
	pub fn is_active(&self) -> bool {
		self.stage != Stage::Idle
	}

	#[inline]
	pub fn tick(&mut self) -> f32 {
		match self.stage {
			Stage::Idle => {}
			Stage::Attack => {
				self.level += self.attack_step;
				if self.level >= 1.0 {
					self.level = 1.0;
					self.stage = Stage::Decay;
				}
			}
			Stage::Decay => {
				self.level = self.sustain + (self.level - self.sustain) * self.decay_coeff;
				if (self.level - self.sustain).abs() < SILENCE {
					self.level = self.sustain;
					self.stage = Stage::Sustain;
				}
			}
			Stage::Sustain => {
				// Follow sustain changes smoothly instead of jumping.
				self.level = self.sustain + (self.level - self.sustain) * self.decay_coeff;
				if self.level < SILENCE {
					self.reset();
				}
			}
			Stage::Release => {
				self.level *= self.release_coeff;
				if self.level < SILENCE {
					self.reset();
				}
			}
		}
		self.level
	}
}

/// One-shot exponential decay envelope used by percussion.
#[derive(Debug, Clone, Copy, Default)]
pub struct DecayEnv {
	level: f32,
	coeff: f32,
}

impl DecayEnv {
	pub fn trigger(&mut self, level: f32, decay_ms: f32, sample_rate: f32) {
		self.level = level;
		self.coeff = decay_coeff(decay_ms, sample_rate);
	}

	/// Changes the decay rate of the running envelope (used for choking).
	pub fn set_decay(&mut self, decay_ms: f32, sample_rate: f32) {
		self.coeff = decay_coeff(decay_ms, sample_rate);
	}

	#[inline]
	pub fn tick(&mut self) -> f32 {
		let v = self.level;
		self.level *= self.coeff;
		if self.level < 1e-5 {
			self.level = 0.0;
		}
		v
	}

	#[inline]
	pub fn is_active(&self) -> bool {
		self.level > 0.0
	}

	pub fn reset(&mut self) {
		self.level = 0.0;
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn adsr_full_cycle() {
		let sr = 48_000.0;
		let mut env = Adsr::default();
		env.set_sample_rate(sr);
		env.set_params(10.0, 100.0, 0.5, 200.0);
		env.gate_on();
		let mut peak: f32 = 0.0;
		for _ in 0..480 {
			peak = peak.max(env.tick());
		}
		assert!(peak > 0.99);
		for _ in 0..48_000 {
			env.tick();
		}
		assert!((env.level() - 0.5).abs() < 1e-3);
		env.gate_off();
		let mut n = 0;
		while env.is_active() {
			env.tick();
			n += 1;
		}
		// 200 ms to -60 dB from 0.5, then a little further to -80 dB.
		assert!(n > 9_000 && n < 15_000, "release took {n} samples");
	}

	#[test]
	fn zero_sustain_finishes_without_release() {
		let mut env = Adsr::default();
		env.set_sample_rate(48_000.0);
		env.set_params(0.0, 50.0, 0.0, 50.0);
		env.gate_on();
		for _ in 0..48_000 {
			env.tick();
		}
		assert!(!env.is_active());
	}
}
