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
	/// The clip's fade-in is a crossfade from the clip before it (micro-fades skip it).
	pub crossfaded_in: bool,
	/// The clip is cut and faded out under the next one.
	pub crossfaded_out: bool,
}

/// Longest automatic micro-fade at clip edges and loop wraps, seconds (de-click).
pub(crate) const MICRO_FADE_SECONDS: f64 = 0.001;

/// Automatic crossfades: where a clip starts inside the previous one and ends after it,
/// the earlier clip fades out and the later one fades in over the crossfade, then the
/// earlier one stops. The crossfade is the later clip's fade-in when it has one (its
/// length, clamped to the overlap, and its curve: the editable handle), else the whole
/// overlap, equal power (uncorrelated material keeps its loudness). Clips nested inside
/// another still sum. `clips` must be in timeline order.
pub(crate) fn crossfade_overlaps(clips: &mut [ClipPlayback]) {
	clips.sort_by_key(|c| c.start);
	for i in 1..clips.len() {
		let (before, rest) = clips.split_at_mut(i);
		let b = &mut rest[0];
		// The latest-ending earlier clip that b overlaps partially.
		let Some(a) = before
			.iter_mut()
			.filter(|a| {
				let a_end = a.start + a.region.length as i64;
				a.start < b.start && b.start < a_end && a_end < b.start + b.region.length as i64
			})
			.max_by_key(|a| a.start + a.region.length as i64)
		else {
			continue;
		};
		let overlap = (a.start + a.region.length as i64 - b.start).max(0) as u64;
		if overlap == 0 {
			continue;
		}
		let (frames, curve) = if b.region.fade_in.frames > 0 {
			(b.region.fade_in.frames.min(overlap), b.region.fade_in.curve)
		} else {
			(overlap, cadenza_audio_io::FadeCurve::EqualPower)
		};
		b.region.fade_in = Fade::new(frames, curve);
		b.crossfaded_in = true;
		// a stops where the crossfade ends.
		a.region.length = (b.start - a.start) as u64 + frames;
		a.region.fade_out = Fade::new(frames, curve);
		a.crossfaded_out = true;
	}
}

/// Short fades at clip edges that have no fade (de-click where a clip starts or ends in
/// the middle of a waveform).
pub(crate) fn micro_fades(clips: &mut [ClipPlayback], sample_rate: f64) {
	let micro = (MICRO_FADE_SECONDS * sample_rate).round() as u64;
	for c in clips {
		let len = c.region.length;
		let edge = micro.min(len / 4);
		if edge == 0 {
			continue;
		}
		if !c.crossfaded_in && c.region.fade_in.frames < edge {
			c.region.fade_in = Fade::new(edge, cadenza_audio_io::FadeCurve::Linear);
		}
		if !c.crossfaded_out && c.region.fade_out.frames < edge {
			c.region.fade_out = Fade::new(edge, cadenza_audio_io::FadeCurve::Linear);
		}
	}
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
			crossfaded_in: false,
			crossfaded_out: false,
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

/// Frames of the micro-fade at a loop wrap / a start in the middle of the song.
pub(crate) fn micro_frames(sample_rate: u32) -> u32 {
	((MICRO_FADE_SECONDS * f64::from(sample_rate)).round() as u32).clamp(1, crate::QUANTUM as u32)
}

/// Fades `l`/`r` out over their last `frames` samples (ending at silence).
pub(crate) fn fade_out_tail(l: &mut [f32], r: &mut [f32], frames: usize) {
	let n = l.len();
	let k = frames.min(n);
	for j in 0..k {
		let g = (k - j) as f32 / (k + 1) as f32;
		l[n - k + j] *= g;
		r[n - k + j] *= g;
	}
}

/// Continues a micro fade-in (`left` samples still to fade out of `total`) over `l`/`r`.
pub(crate) fn fade_in_head(l: &mut [f32], r: &mut [f32], left: &mut u32, total: u32) {
	for (a, b) in l.iter_mut().zip(r.iter_mut()) {
		if *left == 0 {
			return;
		}
		let g = 1.0 - *left as f32 / (total + 1) as f32;
		*a *= g;
		*b *= g;
		*left -= 1;
	}
}

#[cfg(test)]
mod crossfade_tests {
	use super::*;

	fn clip(frames: usize, start: i64) -> ClipPlayback {
		let source = Arc::new(AudioBuffer::from_planar(48_000, vec![vec![1.0; frames]]).unwrap());
		ClipPlayback {
			region: ClipRegion::whole(&source),
			source,
			start,
			crossfaded_in: false,
			crossfaded_out: false,
		}
	}

	#[test]
	fn overlaps_crossfade_with_equal_power_and_the_earlier_clip_stops() {
		let mut clips = vec![clip(1000, 0), clip(1000, 600)];
		crossfade_overlaps(&mut clips);
		assert_eq!(
			clips[0].region.length, 1000,
			"a crossfade over the whole overlap"
		);
		assert_eq!(clips[0].region.fade_out.frames, 400);
		assert_eq!(clips[1].region.fade_in.frames, 400);
		let mut l = vec![0.0f32; 1700];
		let mut r = vec![0.0f32; 1700];
		for c in &clips {
			c.mix(0, &mut l, &mut r);
		}
		// Equal power: amplitudes sum above 1 mid-fade (power is constant), ends at 1.
		assert!(
			(l[800] - std::f32::consts::SQRT_2).abs() < 0.01,
			"{}",
			l[800]
		);
		assert!((l[500] - 1.0).abs() < 1e-6);
		assert!((l[1100] - 1.0).abs() < 1e-6);
		// A shorter fade-in on the later clip is the crossfade length (the handle).
		let mut clips = vec![clip(1000, 0), clip(1000, 600)];
		clips[1].region.fade_in = Fade::new(100, cadenza_audio_io::FadeCurve::Linear);
		crossfade_overlaps(&mut clips);
		assert_eq!(
			clips[0].region.length, 700,
			"the earlier clip stops after it"
		);
		assert_eq!(clips[0].region.fade_out.frames, 100);
		// Nested clips still sum.
		let mut clips = vec![clip(1000, 0), clip(100, 300)];
		crossfade_overlaps(&mut clips);
		assert_eq!(clips[0].region.length, 1000);
		assert_eq!(clips[1].region.fade_in, Fade::NONE);
	}

	#[test]
	fn micro_fades_soften_bare_edges_only() {
		let mut clips = vec![clip(1000, 0), clip(1000, 600)];
		crossfade_overlaps(&mut clips);
		micro_fades(&mut clips, 48_000.0);
		assert_eq!(clips[0].region.fade_in.frames, 48);
		assert_eq!(clips[0].region.fade_out.frames, 400, "crossfade kept");
		assert_eq!(clips[1].region.fade_out.frames, 48);
		let (mut l, mut r) = ([1.0f32; 32], [1.0f32; 32]);
		fade_out_tail(&mut l, &mut r, 8);
		assert!(l[31] < 0.2 && l[23] == 1.0);
		let mut left = 48;
		let (mut l, mut r) = ([1.0f32; 32], [1.0f32; 32]);
		fade_in_head(&mut l, &mut r, &mut left, 48);
		assert!(l[0] < 0.05 && l[31] < 1.0);
		assert_eq!(left, 16);
	}
}
