//! Audio clips: files are decoded and resampled to the engine rate at compile time
//! through the shared [`AudioCache`] (so recompiles and repeated renders don't decode
//! again), then played sample-accurately with [`render_clip`], which is realtime safe.

use std::path::Path;
use std::sync::Arc;

use cadenza_audio_io::{AudioBuffer, AudioCache, ClipRegion, Fade, render_clip};
use cadenza_model::{ClipFade, FadeCurve};

/// Loads `path` at `sample_rate` through the process-wide cache. Errors are
/// human-readable (they become warnings).
pub(crate) fn load(path: &Path, sample_rate: u32) -> Result<Arc<AudioBuffer>, String> {
	AudioCache::global()
		.get_or_load(path, sample_rate)
		.map_err(|e| e.to_string())
}

pub(crate) fn curve(curve: FadeCurve) -> cadenza_audio_io::FadeCurve {
	match curve {
		FadeCurve::Linear => cadenza_audio_io::FadeCurve::Linear,
		FadeCurve::EqualPower => cadenza_audio_io::FadeCurve::EqualPower,
		FadeCurve::Exponential => cadenza_audio_io::FadeCurve::Exponential,
		FadeCurve::SCurve => cadenza_audio_io::FadeCurve::SCurve,
	}
}

/// Converts model fades to frames for a clip of `length` frames. Fades that no longer
/// fit (the clip was shortened or the tempo changed after they were set) are scaled down
/// proportionally so they never overlap.
pub(crate) fn fades(
	fade_in: Option<ClipFade>,
	fade_out: Option<ClipFade>,
	length: u64,
	sample_rate: f64,
) -> (Fade, Fade) {
	let frames =
		|f: Option<ClipFade>| f.map_or(0, |f| (f.seconds.max(0.0) * sample_rate).round() as u64);
	let (mut fin, mut fout) = (frames(fade_in), frames(fade_out));
	let total = fin + fout;
	if total > length {
		fin = (fin as f64 * length as f64 / total as f64).floor() as u64;
		fout = (length - fin).min(fout);
	}
	let make = |frames: u64, f: Option<ClipFade>| match f {
		Some(f) if frames > 0 => Fade::new(frames, curve(f.curve)),
		_ => Fade::NONE,
	};
	(make(fin, fade_in), make(fout, fade_out))
}

/// One audio clip placed on the timeline (positions in engine samples).
#[derive(Debug, Clone)]
pub(crate) struct ClipPlayback {
	pub source: Arc<AudioBuffer>,
	/// Timeline sample of the clip's first frame.
	pub start: i64,
	pub region: ClipRegion,
}

impl ClipPlayback {
	/// Adds the clip's contribution for the block starting at timeline sample `pos`.
	/// Overlapping clips simply sum. Realtime safe.
	#[inline]
	pub fn mix(&self, pos: i64, left: &mut [f32], right: &mut [f32]) {
		let n = left.len() as i64;
		if pos + n <= self.start || pos >= self.start + self.region.length as i64 {
			return;
		}
		render_clip(&self.source, &self.region, pos - self.start, left, right);
	}

	pub fn is_mono(&self) -> bool {
		self.source.num_channels() == 1
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn clip(
		frames: usize,
		value: f32,
		start: i64,
		region: impl FnOnce(&AudioBuffer) -> ClipRegion,
	) -> ClipPlayback {
		let source = Arc::new(AudioBuffer::from_planar(48_000, vec![vec![value; frames]]).unwrap());
		ClipPlayback {
			region: region(&source),
			source,
			start,
		}
	}

	#[test]
	fn fades_are_scaled_to_fit_the_clip() {
		let f = |s| Some(ClipFade::new(s, FadeCurve::Linear));
		let (fin, fout) = fades(f(1.0), f(1.0), 1000, 1000.0);
		assert_eq!((fin.frames, fout.frames), (500, 500));
		let (fin, fout) = fades(f(0.1), None, 1000, 1000.0);
		assert_eq!((fin.frames, fout.frames), (100, 0));
		assert_eq!(fout, Fade::NONE);
	}

	#[test]
	fn overlapping_clips_sum() {
		let a = clip(100, 0.25, 0, ClipRegion::whole);
		let b = clip(100, 0.5, 50, ClipRegion::whole);
		let mut l = [0.0f32; 160];
		let mut r = [0.0f32; 160];
		a.mix(0, &mut l, &mut r);
		b.mix(0, &mut l, &mut r);
		assert_eq!(l[10], 0.25);
		assert_eq!(l[60], 0.75);
		assert_eq!(l[120], 0.5);
		assert_eq!(l[155], 0.0);
		assert_eq!(l, r);
	}

	#[test]
	fn block_split_matches_whole_block() {
		let c = clip(200, 1.0, 7, |_| ClipRegion {
			source_offset: 20,
			length: 150,
			gain: 0.5,
			fade_in: Fade::new(30, cadenza_audio_io::FadeCurve::SCurve),
			fade_out: Fade::new(40, cadenza_audio_io::FadeCurve::EqualPower),
		});
		let mut whole_l = vec![0.0f32; 192];
		let mut whole_r = vec![0.0f32; 192];
		c.mix(0, &mut whole_l, &mut whole_r);
		let mut l = vec![0.0f32; 192];
		let mut r = vec![0.0f32; 192];
		for (i, (bl, br)) in l.chunks_mut(32).zip(r.chunks_mut(32)).enumerate() {
			c.mix(i as i64 * 32, bl, br);
		}
		assert_eq!(whole_l, l);
		assert_eq!(l[7], 0.0);
		assert!(l[100] > 0.49);
		assert_eq!(l[160], 0.0);
	}
}
