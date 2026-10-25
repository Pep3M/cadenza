//! A short stereo snippet played once over the output (the note editor's audition of a
//! note at the pitch it is dragged to): monitoring only, never in renders or exports.
//! Built off the audio thread ([`Snippet::new`] fades its ends), played by the output
//! callback with Hermite interpolation from its own rate, and faded out over
//! [`FADE_SECONDS`] when a newer one replaces it or it is stopped.

use cadenza_dsp::util::interp::hermite;

/// Fade at both ends of a snippet and when one is cut short, seconds.
pub const FADE_SECONDS: f64 = 0.005;

pub struct Snippet {
	left: Vec<f32>,
	right: Vec<f32>,
	/// Source frames per output frame.
	step: f64,
	pos: f64,
	gain: f32,
	/// Output frames of a cut-short fade still to play (`None` = playing on).
	fading: Option<usize>,
	fade_len: usize,
}

impl Snippet {
	/// `channels` (one or two, at `rate`) played at `output_rate`, `gain_db` louder. Not
	/// realtime safe: call it on the control side.
	pub fn new(mut channels: Vec<Vec<f32>>, rate: u32, output_rate: f64, gain_db: f32) -> Self {
		let mut right = if channels.len() > 1 {
			channels.swap_remove(1)
		} else {
			Vec::new()
		};
		let mut left = channels.into_iter().next().unwrap_or_default();
		if right.len() != left.len() {
			right = left.clone();
		}
		let fade = ((FADE_SECONDS * f64::from(rate)) as usize).min(left.len() / 2);
		for ch in [&mut left, &mut right] {
			let len = ch.len();
			for i in 0..fade {
				let g = i as f32 / fade as f32;
				ch[i] *= g;
				ch[len - 1 - i] *= g;
			}
		}
		Self {
			left,
			right,
			step: f64::from(rate.max(1)) / output_rate.max(1.0),
			pos: 0.0,
			gain: cadenza_core::db_to_gain(gain_db),
			fading: None,
			fade_len: ((FADE_SECONDS * output_rate) as usize).max(1),
		}
	}

	/// Starts the fade-out of a snippet cut short.
	pub fn fade_out(&mut self) {
		if self.fading.is_none() {
			self.fading = Some(self.fade_len);
		}
	}

	/// Whether it has nothing more to play.
	pub fn done(&self) -> bool {
		self.fading == Some(0) || self.pos >= self.left.len() as f64
	}

	/// Adds the next `left.len()` output frames. Realtime safe.
	pub fn add(&mut self, left: &mut [f32], right: &mut [f32]) {
		let len = self.left.len();
		let at = |ch: &[f32], i: i64| -> f32 {
			if i < 0 || i as usize >= len {
				0.0
			} else {
				ch[i as usize]
			}
		};
		for (l, r) in left.iter_mut().zip(right.iter_mut()) {
			if self.done() {
				return;
			}
			let mut gain = self.gain;
			if let Some(n) = &mut self.fading {
				gain *= *n as f32 / self.fade_len as f32;
				*n -= 1;
			}
			let i = self.pos.floor();
			let t = (self.pos - i) as f32;
			let i = i as i64;
			let read =
				|ch: &[f32]| hermite(at(ch, i - 1), at(ch, i), at(ch, i + 1), at(ch, i + 2), t);
			*l += gain * read(&self.left);
			*r += gain * read(&self.right);
			self.pos += self.step;
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn plays_once_at_the_output_rate_with_soft_ends() {
		let rate = 24_000;
		let tone: Vec<f32> = (0..rate).map(|n| (n as f32 * 0.05).sin() * 0.5).collect();
		let mut s = Snippet::new(vec![tone], rate, 48_000.0, -6.0);
		let mut out = Vec::new();
		let (mut l, mut r) = (vec![0.0; 256], vec![0.0; 256]);
		while !s.done() {
			l.fill(0.0);
			r.fill(0.0);
			s.add(&mut l, &mut r);
			out.extend_from_slice(&l);
			assert_eq!(l, r, "mono plays on both sides");
		}
		// One second of source, played at the output rate.
		assert!((out.len() as i64 - 48_000).abs() <= 256, "{}", out.len());
		assert_eq!(out[0], 0.0);
		let peak = out.iter().fold(0.0f32, |m, v| m.max(v.abs()));
		assert!((peak - 0.25).abs() < 0.01, "-6 dB: {peak}");
		let steepest = out
			.windows(2)
			.map(|w| (w[1] - w[0]).abs())
			.fold(0.0, f32::max);
		assert!(steepest < 0.02, "{steepest}");
	}

	#[test]
	fn a_cut_fades_out_quickly() {
		let mut s = Snippet::new(
			vec![vec![0.5; 48_000], vec![0.5; 48_000]],
			48_000,
			48_000.0,
			0.0,
		);
		let (mut l, mut r) = (vec![0.0; 1024], vec![0.0; 1024]);
		s.add(&mut l, &mut r);
		s.fade_out();
		let (mut l, mut r) = (vec![0.0; 1024], vec![0.0; 1024]);
		s.add(&mut l, &mut r);
		assert!(s.done());
		let fade = (FADE_SECONDS * 48_000.0) as usize;
		assert!(l[1] < l[0] && l[fade + 10] == 0.0);
	}
}
