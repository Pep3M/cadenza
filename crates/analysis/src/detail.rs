//! Everything measured about one signal (a track, a bus, the master or a reference file).

use crate::spectrum::{Spectrum, SpectrumAnalyzer, Stereo, Timeline};
use crate::{Analyzer, LevelStats};

/// Levels + spectrum + stereo image + octave timeline of one signal.
#[derive(Debug, Clone, PartialEq)]
pub struct Detail {
	pub stats: LevelStats,
	pub spectrum: Spectrum,
	pub stereo: Stereo,
	pub timeline: Timeline,
}

impl Detail {
	/// Effectively silent (nothing to judge).
	pub fn is_silent(&self) -> bool {
		self.stats.lufs_integrated <= -70.0 && self.stats.peak_db <= -60.0
	}
}

/// Streaming analyzer producing a [`Detail`] (offline: allocates).
#[derive(Debug)]
pub struct DetailAnalyzer {
	levels: Analyzer,
	spectrum: SpectrumAnalyzer,
}

impl DetailAnalyzer {
	pub fn new(sample_rate: u32) -> Self {
		Self {
			levels: Analyzer::new(sample_rate),
			spectrum: SpectrumAnalyzer::new(sample_rate),
		}
	}

	pub fn push(&mut self, left: &[f32], right: &[f32]) {
		self.levels.push(left, right);
		self.spectrum.push(left, right);
	}

	pub fn finish(self) -> Detail {
		let stats = self.levels.finish();
		let (spectrum, stereo, timeline) = self.spectrum.finish();
		Detail {
			stats,
			spectrum,
			stereo,
			timeline,
		}
	}
}

/// Analyzes a whole buffer at once.
pub fn analyze(sample_rate: u32, left: &[f32], right: &[f32]) -> Detail {
	let mut a = DetailAnalyzer::new(sample_rate);
	for (l, r) in left.chunks(4096).zip(right.chunks(4096)) {
		a.push(l, r);
	}
	a.finish()
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn levels_and_spectrum_come_from_the_same_signal() {
		let sr = 48_000;
		let x: Vec<f32> = (0..sr as usize * 3)
			.map(|n| (std::f32::consts::TAU * 100.0 * n as f32 / sr as f32).sin() * 0.5)
			.collect();
		let d = analyze(sr, &x, &x);
		assert!((d.stats.peak_db + 6.02).abs() < 0.05);
		assert_eq!(
			d.spectrum.peak_octave(),
			2,
			"100 Hz is in the 125 Hz octave"
		);
		assert!(!d.is_silent());
		let z = vec![0.0; 4800];
		assert!(analyze(sr, &z, &z).is_silent());
	}
}
