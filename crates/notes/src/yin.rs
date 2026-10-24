//! Frame-wise YIN pitch tracker (de Cheveigné & Kawahara 2002). The difference function is
//! computed from an FFT autocorrelation plus prefix energies, so a frame costs
//! O(W log W) instead of O(W x lags). [`Cmnd`] is shared with [`crate::pyin`].

use std::sync::Arc;

use realfft::num_complex::Complex;
use realfft::{RealFftPlanner, RealToComplex};

use crate::{PitchFrame, PitchTracker, hz_to_midi};

#[derive(Debug, Clone)]
pub struct YinOptions {
	/// Seconds between frames.
	pub hop_seconds: f64,
	pub min_hz: f64,
	pub max_hz: f64,
	/// Absolute threshold on the cumulative mean normalized difference.
	pub threshold: f64,
	/// A frame whose best dip is above this is unvoiced.
	pub voicing_limit: f64,
	/// Frames quieter than the loudest frame by more than this are unvoiced.
	pub silence_db: f64,
}

impl Default for YinOptions {
	fn default() -> Self {
		Self {
			hop_seconds: 0.005,
			min_hz: 50.0,
			max_hz: 1_500.0,
			threshold: 0.15,
			voicing_limit: 0.35,
			silence_db: 40.0,
		}
	}
}

/// One analysis frame, centered on `time`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Frame {
	pub time: f64,
	/// Fractional MIDI pitch, `None` when unvoiced.
	pub pitch: Option<f64>,
	/// Best normalized difference (0 = perfectly periodic).
	pub aperiodicity: f64,
	pub rms: f64,
}

/// Cumulative mean normalized difference function of one frame after another.
pub(crate) struct Cmnd {
	pub sample_rate: f64,
	pub window: usize,
	pub hop: usize,
	pub min_lag: usize,
	pub max_lag: usize,
	fft: Autocorr,
	frame: Vec<f64>,
	energy: Vec<f64>,
	acf: Vec<f64>,
	/// d'(tau) for tau in 0..=max_lag, filled by [`Cmnd::compute`].
	pub values: Vec<f64>,
}

impl Cmnd {
	/// `None` when the rate cannot hold the pitch range.
	pub fn new(sample_rate: u32, hop_seconds: f64, min_hz: f64, max_hz: f64) -> Option<Self> {
		let sr = f64::from(sample_rate.max(1));
		let window = ((sr * 0.04) as usize).next_power_of_two().max(256);
		let max_lag = ((sr / min_hz) as usize).min(window / 2);
		let min_lag = ((sr / max_hz) as usize).max(2);
		let hop = ((sr * hop_seconds).round() as usize).max(1);
		(min_lag + 2 < max_lag).then(|| Self {
			sample_rate: sr,
			window,
			hop,
			min_lag,
			max_lag,
			fft: Autocorr::new(window),
			frame: vec![0.0; window],
			energy: vec![0.0; window + 1],
			acf: vec![0.0; window],
			values: vec![0.0; max_lag + 1],
		})
	}

	pub fn frame_count(&self, samples: usize) -> usize {
		samples.div_ceil(self.hop)
	}

	pub fn time(&self, index: usize) -> f64 {
		(index * self.hop) as f64 / self.sample_rate
	}

	/// Fills `values` for frame `index` (centered on `index * hop`) and returns its RMS.
	pub fn compute(&mut self, samples: &[f32], index: usize) -> f64 {
		let (window, center) = (self.window, index * self.hop);
		for (j, slot) in self.frame.iter_mut().enumerate() {
			*slot = (center + j)
				.checked_sub(window / 2)
				.and_then(|k| samples.get(k))
				.map_or(0.0, |&v| f64::from(v));
		}
		for j in 0..window {
			self.energy[j + 1] = self.energy[j] + self.frame[j] * self.frame[j];
		}
		self.fft.compute(&self.frame, &mut self.acf);
		// d(tau) = sum_{j < W - tau} (x_j - x_{j+tau})^2, normalized cumulatively.
		let total = self.energy[window];
		self.values[0] = 1.0;
		let mut running = 0.0;
		for tau in 1..=self.max_lag {
			let d = (self.energy[window - tau] + (total - self.energy[tau]) - 2.0 * self.acf[tau])
				.max(0.0);
			running += d;
			self.values[tau] = if running > 0.0 {
				d * tau as f64 / running
			} else {
				1.0
			};
		}
		(total / window as f64).sqrt()
	}

	/// Period in samples of a dip at `tau`, refined by parabolic interpolation.
	pub fn refine(&self, tau: usize) -> f64 {
		if tau <= self.min_lag || tau + 1 >= self.max_lag {
			return tau as f64;
		}
		let (a, b, c) = (self.values[tau - 1], self.values[tau], self.values[tau + 1]);
		let denom = a - 2.0 * b + c;
		if denom.abs() > 1e-12 {
			tau as f64 + (0.5 * (a - c) / denom).clamp(-0.5, 0.5)
		} else {
			tau as f64
		}
	}

	pub fn pitch_of(&self, tau: usize) -> f64 {
		hz_to_midi(self.sample_rate / self.refine(tau))
	}
}

/// Tracks the pitch of mono `samples`.
pub fn track(samples: &[f32], sample_rate: u32, opts: &YinOptions) -> Vec<Frame> {
	let Some(mut cmnd) = Cmnd::new(sample_rate, opts.hop_seconds, opts.min_hz, opts.max_hz) else {
		return Vec::new();
	};
	if samples.is_empty() {
		return Vec::new();
	}
	let (min_lag, max_lag) = (cmnd.min_lag, cmnd.max_lag);
	let count = cmnd.frame_count(samples.len());
	let mut frames = Vec::with_capacity(count);
	for i in 0..count {
		let rms = cmnd.compute(samples, i);
		let v = &cmnd.values;
		let mut best = None;
		let mut tau = min_lag;
		while tau < max_lag {
			if v[tau] < opts.threshold {
				while tau + 1 < max_lag && v[tau + 1] < v[tau] {
					tau += 1;
				}
				best = Some(tau);
				break;
			}
			tau += 1;
		}
		let best = best.unwrap_or_else(|| {
			(min_lag..max_lag)
				.min_by(|&a, &b| v[a].total_cmp(&v[b]))
				.unwrap_or(min_lag)
		});
		let aperiodicity = v[best];
		let pitch = (aperiodicity <= opts.voicing_limit).then(|| cmnd.pitch_of(best));
		frames.push(Frame {
			time: cmnd.time(i),
			pitch,
			aperiodicity,
			rms,
		});
	}
	let loudest = frames.iter().map(|f| f.rms).fold(0.0, f64::max);
	let floor = loudest * 10f64.powf(-opts.silence_db / 20.0);
	for f in &mut frames {
		if f.rms <= floor || loudest <= 0.0 {
			f.pitch = None;
		}
	}
	frames
}

/// Plain YIN as a [`PitchTracker`] (the NE-0 reference).
#[derive(Debug, Clone, Default)]
pub struct Yin(pub YinOptions);

impl PitchTracker for Yin {
	fn name(&self) -> &'static str {
		"yin"
	}

	fn track(&self, samples: &[f32], sample_rate: u32) -> Vec<PitchFrame> {
		track(samples, sample_rate, &self.0)
			.into_iter()
			.map(|f| PitchFrame {
				time: f.time,
				pitch: f.pitch,
				voicing: if f.pitch.is_some() {
					1.0 - f.aperiodicity
				} else {
					0.0
				},
			})
			.collect()
	}
}

/// Linear autocorrelation of a frame through a zero-padded real FFT.
struct Autocorr {
	forward: Arc<dyn RealToComplex<f64>>,
	inverse: Arc<dyn realfft::ComplexToReal<f64>>,
	time: Vec<f64>,
	spectrum: Vec<Complex<f64>>,
}

impl Autocorr {
	fn new(window: usize) -> Self {
		let mut planner = RealFftPlanner::<f64>::new();
		let forward = planner.plan_fft_forward(window * 2);
		let inverse = planner.plan_fft_inverse(window * 2);
		let spectrum = forward.make_output_vec();
		Self {
			forward,
			inverse,
			time: vec![0.0; window * 2],
			spectrum,
		}
	}

	fn compute(&mut self, frame: &[f64], out: &mut [f64]) {
		let n = self.time.len();
		self.time[..frame.len()].copy_from_slice(frame);
		self.time[frame.len()..].fill(0.0);
		self.forward
			.process(&mut self.time, &mut self.spectrum)
			.expect("fft sizes match the plan");
		for c in &mut self.spectrum {
			*c = Complex::new(c.norm_sqr(), 0.0);
		}
		self.inverse
			.process(&mut self.spectrum, &mut self.time)
			.expect("fft sizes match the plan");
		let scale = 1.0 / n as f64;
		for (o, t) in out.iter_mut().zip(&self.time) {
			*o = t * scale;
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::midi_to_hz;

	pub(crate) fn tone(midi: f64, sr: u32, seconds: f64) -> Vec<f32> {
		let hz = midi_to_hz(midi);
		(0..(f64::from(sr) * seconds) as usize)
			.map(|i| {
				let t = i as f64 / f64::from(sr);
				let x = (2.0 * std::f64::consts::PI * hz * t).sin()
					+ 0.5 * (4.0 * std::f64::consts::PI * hz * t).sin();
				(0.4 * x) as f32
			})
			.collect()
	}

	#[test]
	fn tracks_a_steady_tone_to_within_a_cent() {
		for (midi, sr) in [(45.0, 22_050), (60.3, 44_100), (76.0, 48_000)] {
			let frames = track(&tone(midi, sr, 0.5), sr, &YinOptions::default());
			let voiced: Vec<f64> = frames[20..frames.len() - 20]
				.iter()
				.map(|f| f.pitch.expect("voiced"))
				.collect();
			for p in voiced {
				assert!((p - midi).abs() < 0.01, "{p} vs {midi} at {sr}");
			}
		}
	}

	#[test]
	fn silence_is_unvoiced() {
		let mut audio = tone(57.0, 22_050, 0.3);
		audio.extend(std::iter::repeat_n(0.0, 22_050 / 2));
		let frames = track(&audio, 22_050, &YinOptions::default());
		assert!(frames.last().unwrap().pitch.is_none());
		assert!(frames[10].pitch.is_some());
	}
}
