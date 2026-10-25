//! Pitched percussion (NE-9f): the hits of an 808, a tom or a plucked bass line, each with
//! the note it rings at. A voice detector misses these: their pitch sits below the 50 Hz
//! the speech-sized window reaches, the attack is a click or a fast pitch sweep, and each
//! note decays instead of holding. Here a hit starts where the slow level (a 50 ms window,
//! longer than any period) rises by a few dB, ends where it falls 30 dB below its peak or
//! the next hit starts, and its pitch comes from YIN with a window long enough for 30 Hz,
//! run on a copy decimated to about 4 kHz, after the attack has settled. The result is an
//! ordinary [`Analysis`] + [`Segmentation`] (one note per pitched hit, the pitch curve on its
//! body only), so [`crate::midi::to_midi`] writes it with every option of a melody: key snap,
//! quantize, register fold and pitch bends for 808 slides.

use crate::analysis::{ANALYSIS_VERSION, Analysis, AnalysisFrame};
use crate::segment::{Segmentation, SegmentedNote};

#[derive(Debug, Clone, PartialEq)]
pub struct HitOptions {
	/// Lowest and highest pitch looked for, Hz.
	pub min_hz: f64,
	pub max_hz: f64,
	/// The slow level must rise this much (dB over 50 ms) to start a hit.
	pub rise_db: f64,
	/// Hits closer than this (seconds) are one.
	pub merge: f64,
	/// Hits quieter than this below the loudest moment are ignored, dB.
	pub floor_db: f64,
	/// A hit ends this far below its peak, dB.
	pub release_db: f64,
	/// Attack (click, pitch sweep) left out of the pitch, seconds.
	pub settle: f64,
	/// The note's pitch is the median over this much of the body, seconds (a slide after it
	/// becomes bends).
	pub center_span: f64,
}

impl Default for HitOptions {
	fn default() -> Self {
		Self {
			min_hz: 28.0,
			max_hz: 500.0,
			rise_db: 3.0,
			merge: 0.06,
			floor_db: 40.0,
			release_db: 30.0,
			settle: 0.025,
			center_span: 0.15,
		}
	}
}

/// The pitched hits of a recording.
#[derive(Debug, Clone)]
pub struct HitTrack {
	/// 5 ms frames: the slow level everywhere, the pitch on hit bodies only.
	pub analysis: Analysis,
	/// One note per pitched hit, in time order.
	pub segmentation: Segmentation,
	/// Hits without a steady pitch (left out).
	pub unpitched: usize,
}

const HOP_SECONDS: f64 = 0.005;
const LEVEL_SECONDS: f64 = 0.05;
/// Rate the pitch is measured at (about: a whole fraction of the input rate).
const PITCH_RATE: f64 = 4_000.0;

/// The pitched hits of mono `samples` at `rate`.
pub fn detect(samples: &[f32], rate: u32, o: &HitOptions) -> HitTrack {
	let sr = f64::from(rate.max(1));
	let hop = ((sr * HOP_SECONDS).round() as usize).max(1);
	let hop_s = hop as f64 / sr;
	let count = samples.len() / hop + 1;
	let energy = slow_energy(samples, hop, ((sr * LEVEL_SECONDS) as usize).max(1), count);
	let db: Vec<f64> = energy.iter().map(|e| 10.0 * (e + 1e-12).log10()).collect();
	let onsets = onsets(&energy, &db, hop_s, o);
	let pitch = Pitch::new(samples, rate, o);
	let mut frames: Vec<AnalysisFrame> = (0..count)
		.map(|i| AnalysisFrame {
			time: i as f64 * hop_s,
			pitch: None,
			voicing: 0.0,
			aperiodicity: 1.0,
			level_db: db[i],
			flux: 0.0,
			hf_ratio: 0.0,
			candidates: Vec::new(),
		})
		.collect();
	let duration = samples.len() as f64 / sr;
	let mut notes = Vec::new();
	let mut unpitched = 0;
	for (k, &a) in onsets.iter().enumerate() {
		let next = onsets.get(k + 1).copied().unwrap_or(count);
		let attack = (a + (0.06 / hop_s) as usize).min(next).max(a + 1);
		let (top_at, top) = (a..attack.min(count))
			.map(|t| (t, db[t]))
			.fold((a, f64::NEG_INFINITY), |m, v| if v.1 > m.1 { v } else { m });
		let onset = a as f64 * hop_s;
		// The slow level falls half a window after the sound does.
		let offset = match (top_at..next).find(|&t| db[t] < top - o.release_db) {
			Some(t) => (t as f64 * hop_s - LEVEL_SECONDS / 2.0).max(onset + hop_s),
			None => next.min(count - 1) as f64 * hop_s,
		}
		.min(duration);
		// Body frames whose pitch window stays inside the hit, after the attack.
		let half = pitch.half_window;
		let first = ((onset + o.settle + half) / hop_s).ceil() as usize;
		let last = ((offset - half) / hop_s).floor() as usize;
		let body: Vec<usize> = if first <= last {
			(first..=last.min(count - 1)).collect()
		} else {
			let mid = ((onset + o.settle).max((onset + offset) * 0.5) / hop_s).round() as usize;
			vec![mid.min(count - 1)]
		};
		let mut voiced = Vec::new();
		for &t in &body {
			if let Some((p, aperiodicity)) = pitch.at(t as f64 * hop_s) {
				frames[t].pitch = Some(p);
				frames[t].voicing = 1.0;
				frames[t].aperiodicity = aperiodicity;
				voiced.push((t, p));
			}
		}
		// A note struck again while the last one still rings barely raises the level: a
		// step between two steady pitches starts a hit too.
		let steps = pitch_steps(&voiced, hop_s);
		let mut bounds = vec![onset];
		for &(a, b) in &steps {
			for f in &mut frames[a + 1..b] {
				f.pitch = None;
				f.voicing = 0.0;
			}
			// The old pitch held until the window's leading edge reached the new note.
			bounds.push((a as f64 * hop_s + half).min(b as f64 * hop_s));
		}
		bounds.push(offset);
		let mut found = false;
		for w in bounds.windows(2) {
			let (from, to) = (w[0], w[1]);
			let part: Vec<(usize, f64)> = voiced
				.iter()
				.copied()
				.filter(|&(t, _)| {
					let at = t as f64 * hop_s;
					at >= from && at < to && frames[t].pitch.is_some()
				})
				.collect();
			let early: Vec<(usize, f64)> = part
				.iter()
				.copied()
				.take_while(|&(t, _)| (t - part[0].0) as f64 * hop_s <= o.center_span)
				.collect();
			if early.len() < 2 && !(body.len() == 1 && early.len() == 1) {
				for &(t, _) in &part {
					frames[t].pitch = None;
					frames[t].voicing = 0.0;
				}
				continue;
			}
			let mut ps: Vec<f64> = early.iter().map(|e| e.1).collect();
			ps.sort_by(f64::total_cmp);
			found = true;
			notes.push(SegmentedNote {
				onset: from,
				offset: to,
				core_start: early[0].0 as f64 * hop_s,
				core_end: early[early.len() - 1].0 as f64 * hop_s,
				center: ps[ps.len() / 2],
				drift: 0.0,
				vibrato: None,
				confidence: part.len() as f64 / ((to - from) / hop_s).max(1.0),
				flags: Vec::new(),
			});
		}
		if !found {
			unpitched += 1;
		}
	}
	HitTrack {
		analysis: Analysis {
			version: ANALYSIS_VERSION,
			sample_rate: rate.max(1),
			hop: hop as u32,
			duration,
			frames,
		},
		segmentation: Segmentation {
			notes,
			sibilants: Vec::new(),
		},
		unpitched,
	}
}

/// Where the pitch steps from one steady note to another (frames: the end of the first
/// steady stretch, the start of the second), at least 0.7 semitones apart and at most
/// 80 ms between them. Slides take longer and stay one note (bends).
fn pitch_steps(voiced: &[(usize, f64)], hop_s: f64) -> Vec<(usize, usize)> {
	let n = ((0.03 / hop_s).round() as usize).max(2);
	let steady = |i: usize| -> Option<f64> {
		let run = voiced.get(i..i + n)?;
		if run.windows(2).any(|w| w[1].0 - w[0].0 > 2) {
			return None;
		}
		let (lo, hi) = run
			.iter()
			.fold((f64::MAX, f64::MIN), |m, v| (m.0.min(v.1), m.1.max(v.1)));
		(hi - lo < 0.3).then(|| {
			let mut ps: Vec<f64> = run.iter().map(|v| v.1).collect();
			ps.sort_by(f64::total_cmp);
			ps[n / 2]
		})
	};
	let mut out = Vec::new();
	// The last steady stretch: its last frame and pitch.
	let mut last: Option<(usize, f64)> = None;
	for i in 0..voiced.len() {
		let Some(p) = steady(i) else { continue };
		let start = voiced[i].0;
		if let Some((end, q)) = last
			&& (p - q).abs() >= 0.7
			&& start > end
			&& (start - end) as f64 * hop_s <= 0.08
		{
			out.push((end, start));
		}
		last = Some((voiced[i + n - 1].0, p));
	}
	out
}

/// Mean square over `window` samples centered on each hop.
fn slow_energy(samples: &[f32], hop: usize, window: usize, count: usize) -> Vec<f64> {
	let mut sum = Vec::with_capacity(samples.len() + 1);
	sum.push(0.0f64);
	for &s in samples {
		let last = *sum.last().expect("starts with 0");
		sum.push(last + f64::from(s) * f64::from(s));
	}
	(0..count)
		.map(|i| {
			let c = i * hop;
			let a = c.saturating_sub(window / 2).min(samples.len());
			let b = (c + window / 2).min(samples.len());
			(sum[b] - sum[a]) / window as f64
		})
		.collect()
}

/// Frames where hits start. A rise of the slow level over its own window peaks half a
/// window after the attack. The window smears a step at time `a` into a ramp from half a
/// window before to half a window after (plus the attack's own length `A`): a quarter of
/// the way up is a quarter window early, so the start is that crossing plus a quarter
/// window (`a + A / 4`; the halfway crossing gives `a + A / 2`).
fn onsets(energy: &[f64], db: &[f64], hop_s: f64, o: &HitOptions) -> Vec<usize> {
	let n = db.len();
	let span = ((LEVEL_SECONDS / hop_s).round() as usize).max(1);
	let loudest = db.iter().copied().fold(f64::NEG_INFINITY, f64::max);
	let before = |t: usize| if t >= span { db[t - span] } else { -240.0 };
	let rise = |t: usize| db[t] - before(t);
	let gap = (o.merge / hop_s).round() as usize;
	let mut out: Vec<usize> = Vec::new();
	for t in 0..n {
		let r = rise(t);
		if r < o.rise_db
			|| db[t] < loudest - o.floor_db
			|| (t > 0 && rise(t - 1) > r)
			|| (t + 1 < n && rise(t + 1) >= r)
		{
			continue;
		}
		let from = t.saturating_sub(span);
		let e0 = if t >= span { energy[from] } else { 0.0 };
		let quarter = e0 + 0.25 * (energy[t] - e0);
		let at = (from..=t).find(|&u| energy[u] >= quarter).unwrap_or(t);
		let at = (at + span / 4).min(t);
		match out.last() {
			Some(&l) if at <= l + gap => {}
			_ => out.push(at),
		}
	}
	out
}

/// YIN on a low-passed, decimated copy, with a window as long as the lowest period.
struct Pitch {
	x: Vec<f64>,
	rate: f64,
	min_lag: usize,
	max_lag: usize,
	/// Seconds the measurement reaches either side of its time.
	half_window: f64,
}

impl Pitch {
	fn new(samples: &[f32], rate: u32, o: &HitOptions) -> Self {
		let sr = f64::from(rate.max(1));
		let factor = ((sr / PITCH_RATE).floor() as usize).max(1);
		let dr = sr / factor as f64;
		let mut x: Vec<f64> = samples.iter().map(|&s| f64::from(s)).collect();
		let cutoff = (0.3 * dr).min(2.5 * o.max_hz);
		for _ in 0..2 {
			low_pass(&mut x, sr, cutoff);
		}
		let x: Vec<f64> = x.into_iter().step_by(factor).collect();
		let max_lag = (dr / o.min_hz.max(1.0)).ceil() as usize;
		let min_lag = ((dr / o.max_hz.max(1.0)).floor() as usize).max(2);
		Self {
			x,
			rate: dr,
			min_lag,
			max_lag: max_lag.max(min_lag + 3),
			half_window: max_lag as f64 / dr,
		}
	}

	/// Fractional MIDI pitch and the dip's depth at `seconds`, `None` when not periodic.
	fn at(&self, seconds: f64) -> Option<(f64, f64)> {
		let w = self.max_lag;
		let c = (seconds * self.rate).round() as isize;
		let s = c - w as isize;
		let get = |i: isize| {
			if i < 0 {
				0.0
			} else {
				self.x.get(i as usize).copied().unwrap_or(0.0)
			}
		};
		let power: f64 = (0..w as isize).map(|j| get(s + j) * get(s + j)).sum();
		if power <= 1e-12 {
			return None;
		}
		let mut d = vec![0.0f64; w + 1];
		for (tau, slot) in d.iter_mut().enumerate().skip(1) {
			*slot = (0..w as isize)
				.map(|j| {
					let v = get(s + j) - get(s + j + tau as isize);
					v * v
				})
				.sum();
		}
		// Cumulative mean normalized difference.
		let mut cmnd = vec![1.0f64; w + 1];
		let mut running = 0.0;
		for tau in 1..=w {
			running += d[tau];
			cmnd[tau] = if running > 0.0 {
				d[tau] * tau as f64 / running
			} else {
				1.0
			};
		}
		let range = self.min_lag..=w;
		let mut pick = None;
		let mut tau = self.min_lag;
		while tau <= w {
			if cmnd[tau] < 0.15 {
				while tau < w && cmnd[tau + 1] < cmnd[tau] {
					tau += 1;
				}
				pick = Some(tau);
				break;
			}
			tau += 1;
		}
		let tau = match pick {
			Some(t) => t,
			None => {
				let best = range
					.clone()
					.min_by(|a, b| cmnd[*a].total_cmp(&cmnd[*b]))
					.unwrap_or(w);
				if cmnd[best] > 0.3 {
					return None;
				}
				best
			}
		};
		if tau <= self.min_lag || tau >= w {
			return None;
		}
		let (a, b, c) = (cmnd[tau - 1], cmnd[tau], cmnd[tau + 1]);
		let den = a - 2.0 * b + c;
		let shift = if den.abs() > 1e-12 {
			(0.5 * (a - c) / den).clamp(-0.5, 0.5)
		} else {
			0.0
		};
		let hz = self.rate / (tau as f64 + shift);
		Some((crate::hz_to_midi(hz), b))
	}
}

/// In-place RBJ low-pass (Q 0.707).
fn low_pass(x: &mut [f64], rate: f64, cutoff: f64) {
	let w = 2.0 * std::f64::consts::PI * (cutoff / rate).min(0.49);
	let alpha = w.sin() / (2.0 * std::f64::consts::FRAC_1_SQRT_2);
	let cos = w.cos();
	let a0 = 1.0 + alpha;
	let b0 = (1.0 - cos) / 2.0 / a0;
	let b1 = (1.0 - cos) / a0;
	let a1 = -2.0 * cos / a0;
	let a2 = (1.0 - alpha) / a0;
	let (mut x1, mut x2, mut y1, mut y2) = (0.0, 0.0, 0.0, 0.0);
	for v in x.iter_mut() {
		let y = b0 * *v + b1 * x1 + b0 * x2 - a1 * y1 - a2 * y2;
		x2 = x1;
		x1 = *v;
		y2 = y1;
		y1 = y;
		*v = y;
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::midi::{MidiOptions, to_midi};
	use cadenza_core::TempoMap;

	/// One 808 hit: a sine that starts an octave up and falls to its note in ~15 ms, a short
	/// click, an exponential decay; `slide` semitones reached between 40 and 80 % of it.
	fn hit(x: &mut [f64], rate: f64, at: f64, len: f64, midi: f64, slide: f64) {
		let start = (at * rate) as usize;
		let mut phase = 0.0f64;
		for i in 0..(len * rate) as usize {
			let Some(slot) = x.get_mut(start + i) else {
				break;
			};
			let t = i as f64 / rate;
			let glide = slide * ((t / len - 0.4) / 0.4).clamp(0.0, 1.0);
			let sweep = 12.0 * (-t / 0.005).exp();
			phase += 2.0 * std::f64::consts::PI * crate::midi_to_hz(midi + glide + sweep) / rate;
			let click = if t < 0.002 {
				0.3 * (1.0 - t / 0.002)
			} else {
				0.0
			};
			// The previous hit is cut by this one (a mono 808).
			*slot = 0.8 * phase.sin() * (-t / 0.5).exp() * (1.0 - (-t / 0.001).exp()) + click;
		}
		let end = start + (len * rate) as usize;
		let fade = (0.004 * rate) as usize;
		for i in 0..fade {
			if let Some(s) = x.get_mut(end.saturating_sub(fade) + i) {
				*s *= 1.0 - i as f64 / fade as f64;
			}
		}
	}

	#[test]
	fn an_808_line_comes_out_note_for_note_down_to_b0() {
		let rate = 44_100;
		let sr = f64::from(rate);
		// (time, length, MIDI, slide): E1, G1, A#1, D2, B0, C2 with a slide down to A1, F1.
		let line = [
			(0.0, 0.5, 28.0, 0.0),
			(0.5, 0.25, 31.0, 0.0),
			(0.75, 0.25, 34.0, 0.0),
			(1.0, 0.5, 38.0, 0.0),
			(1.5, 0.375, 23.0, 0.0),
			(1.875, 0.625, 36.0, -3.0),
			(2.5, 0.12, 29.0, 0.0),
			(2.75, 0.75, 29.0, 0.0),
		];
		let mut x = vec![0.0f64; (3.6 * sr) as usize];
		for &(at, len, midi, slide) in &line {
			hit(&mut x, sr, at, len, midi, slide);
		}
		let x: Vec<f32> = x.into_iter().map(|v| v as f32).collect();
		let track = detect(&x, rate, &HitOptions::default());
		let notes = &track.segmentation.notes;
		assert_eq!(notes.len(), line.len(), "{notes:#?}");
		assert_eq!(track.unpitched, 0);
		for (n, &(at, len, midi, _)) in notes.iter().zip(&line) {
			assert!((n.onset - at).abs() <= 0.012, "{} vs {at}", n.onset);
			assert!((n.center - midi).abs() < 0.15, "{} vs {midi}", n.center);
			assert!(n.offset <= at + len + 0.012, "{} vs {}", n.offset, at + len);
		}
		// The slide: bends from the note down three semitones.
		let opts = MidiOptions {
			bends: true,
			bend_range: 12.0,
			..MidiOptions::default()
		};
		let tempo = TempoMap::constant(120.0);
		let midi = to_midi(&track.segmentation, &track.analysis, &tempo, 0.0, &opts);
		let pitches: Vec<u8> = midi.notes.iter().map(|n| n.pitch).collect();
		assert_eq!(pitches, [28, 31, 34, 38, 23, 36, 29, 29]);
		let slide = &midi.notes[5];
		let lowest = midi
			.bends
			.iter()
			.filter(|b| b.beat > slide.start && b.beat < slide.start + slide.length)
			.map(|b| b.value)
			.fold(f32::MAX, f32::min);
		assert!((f64::from(lowest) * 12.0 + 3.0).abs() < 0.3, "{lowest}");
		// Steady notes get no bend worth hearing.
		assert!(
			midi.bends
				.iter()
				.filter(|b| b.beat < midi.notes[5].start)
				.all(|b| (b.value * 12.0).abs() < 0.2),
			"{:?}",
			midi.bends
		);
	}

	#[test]
	fn noise_hits_have_no_pitch_and_silence_has_no_hits() {
		let rate = 44_100;
		let mut x = vec![0.0f32; rate as usize * 2];
		assert!(
			detect(&x, rate, &HitOptions::default())
				.segmentation
				.notes
				.is_empty()
		);
		let mut rng = 0x9e37_79b9_7f4a_7c15_u64;
		for at in [0.25, 1.0] {
			let start = (at * f64::from(rate)) as usize;
			for i in 0..(0.2 * f64::from(rate)) as usize {
				rng ^= rng << 13;
				rng ^= rng >> 7;
				rng ^= rng << 17;
				let v = (rng >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0;
				x[start + i] = (v * (-(i as f64) / 4_000.0).exp() * 0.5) as f32;
			}
		}
		let track = detect(&x, rate, &HitOptions::default());
		assert!(track.segmentation.notes.is_empty(), "{track:?}");
		assert_eq!(track.unpitched, 2);
	}
}
