//! Second look at a segmentation (NE-7a), from an expert's session on a real take
//! (`docs/design/note-editing-expert-session.md`): the optimal partition prices cores as
//! nearly flat lines, so a note sung on the way somewhere — a passing tone in a stepwise
//! run, a melisma step, a re-articulated syllable, a landing note after a fall — can end up
//! inside a transition, where no core sees it. Those notes are found here over the whole
//! run and become forced boundaries for a second partition, the way an expert splits them
//! by hand. Monotonic scoops and falls (no hold anywhere) are never split.

use crate::analysis::Analysis;
use crate::segment::{NoteFlag, SegmentedNote};
use crate::vibrato::VibratoFrame;

#[derive(Debug, Clone)]
pub struct RefineOptions {
	/// A stretch moving slower than this (cents per second over 30 ms) holds a pitch.
	pub hold_speed: f64,
	/// Shortest hold, seconds.
	pub min_hold: f64,
	/// Holds closer than this are the same note, cents.
	pub hold_cents: f64,
	/// A hold this much quieter than the loudest hold within 300 ms is a pickup or a tail.
	pub hold_drop_db: f64,
	/// Shortest note a new boundary may leave on either side, seconds.
	pub min_note: f64,
	/// The first or last hold of a phrase must last this long (seconds) to be a note: a
	/// fall slows down at its bottom and a scoop at its start without being one.
	pub edge_hold: f64,
	/// A pitch jump of at least this (cents) within `jump_window` seconds...
	pub jump_cents: f64,
	pub jump_window: f64,
	/// ... at a level dip of at least this (dB below the louder sides) is a new syllable.
	pub dip_db: f64,
	/// An existing boundary this close (seconds) already does the job.
	pub tolerance: f64,
	/// A note shorter than this, further than `blip_semitones` from the take's median and
	/// almost a pure tone (no energy above 4 kHz, low aperiodicity) is not the voice (a
	/// metronome click leaking from headphones, a beep): dropped.
	pub blip_length: f64,
	pub blip_semitones: f64,
}

impl Default for RefineOptions {
	fn default() -> Self {
		Self {
			hold_speed: 1000.0,
			min_hold: 0.035,
			hold_cents: 60.0,
			hold_drop_db: 15.0,
			min_note: 0.08,
			edge_hold: 0.06,
			jump_cents: 100.0,
			jump_window: 0.02,
			dip_db: 5.0,
			tolerance: 0.03,
			blip_length: 0.12,
			blip_semitones: 24.0,
		}
	}
}

/// One run's curves (run-relative frames).
pub(crate) struct RunView<'a> {
	/// First frame of the run in the analysis.
	pub start: usize,
	pub hop: f64,
	/// Pitch in cents without vibrato.
	pub smooth: &'a [f64],
	/// Pitch in cents as tracked (vibrato included).
	pub raw: &'a [f64],
	pub vibrato: &'a [Option<VibratoFrame>],
	pub level: &'a [f64],
}

impl RunView<'_> {
	fn time(&self, frame: f64) -> f64 {
		(self.start as f64 + frame) * self.hop
	}

	fn frame(&self, time: f64) -> f64 {
		time / self.hop - self.start as f64
	}
}

/// A stretch where the pitch holds still.
#[derive(Debug, Clone, Copy)]
struct Hold {
	start: usize,
	end: usize,
	cents: f64,
	level: f64,
	/// Vibrato depth over it (0 without): its peaks and troughs hold too, so two holds
	/// are only different notes when further apart than the swing.
	depth: f64,
}

fn median(values: &mut [f64]) -> f64 {
	values.sort_by(f64::total_cmp);
	values.get(values.len() / 2).copied().unwrap_or(0.0)
}

/// Holds on the tracked curve: frames slower than `hold_speed`, a stretch spanning more
/// than one note's worth of pitch cut where two constants fit it best, same-pitch
/// neighbours joined, quiet pickups and tails dropped.
fn holds(v: &RunView, bounds: &[f64], o: &RefineOptions) -> Vec<Hold> {
	let n = v.raw.len();
	let reach = 3usize;
	let min = ((o.min_hold / v.hop).round() as usize).max(2);
	// Slow on the tracked curve, or on the curve without vibrato (a note sung with
	// vibrato holds although its pitch swings).
	let speed =
		|c: &[f64], t: usize| (c[t + reach] - c[t - reach]).abs() / (2 * reach) as f64 / v.hop;
	let slow = |t: usize| {
		t >= reach
			&& t + reach < n
			&& speed(v.raw, t).min(if v.vibrato[t].is_some() {
				speed(v.smooth, t)
			} else {
				f64::INFINITY
			}) < o.hold_speed
	};
	let depth = |a: usize, b: usize| {
		v.vibrato[a..b]
			.iter()
			.map(|f| f.map_or(0.0, |f| f.depth_cents))
			.sum::<f64>()
			/ (b - a) as f64
	};
	let apart = |d: f64| o.hold_cents.max(2.5 * d);
	let frames: Vec<f64> = bounds.iter().map(|b| v.frame(*b).round()).collect();
	// Across a boundary that exists, two holds are one note only at nearly one pitch.
	let same = |a: &Hold, b: &Hold| {
		let across = frames
			.iter()
			.any(|&f| f >= a.end as f64 - 1.0 && f <= b.start as f64 + 1.0);
		let limit = if across {
			o.hold_cents
		} else {
			apart(a.depth.min(b.depth))
		};
		(a.cents - b.cents).abs() < limit
	};
	let make = |a: usize, b: usize| Hold {
		start: a,
		end: b,
		cents: median(&mut v.raw[a..b].to_vec()),
		level: v.level[a..b].iter().sum::<f64>() / (b - a) as f64,
		depth: depth(a, b),
	};
	let mut stretches = Vec::new();
	let mut t = 0;
	while t < n {
		if !slow(t) {
			t += 1;
			continue;
		}
		let a = t;
		while t < n && slow(t) {
			t += 1;
		}
		// Never across a boundary that exists: each note's holds are its own.
		let mut from = a;
		for &f in &frames {
			if f > from as f64 && f < t as f64 {
				let f = f as usize;
				if f - from >= min {
					stretches.push((from, f));
				}
				from = f;
			}
		}
		if t - from >= min {
			stretches.push((from, t));
		}
	}
	// Slow glides between two notes: split while the stretch spans two pitches.
	let mut parts: Vec<(usize, usize)> = Vec::new();
	while let Some((a, b)) = stretches.pop() {
		let x = &v.raw[a..b];
		let (lo, hi) = x
			.iter()
			.fold((f64::INFINITY, f64::NEG_INFINITY), |(l, h), &c| {
				(l.min(c), h.max(c))
			});
		if hi - lo <= apart(depth(a, b)) || b - a < 2 * min {
			parts.push((a, b));
			continue;
		}
		let total: f64 = x.iter().sum();
		let (mut left, mut best) = (0.0, (f64::NEG_INFINITY, 0usize));
		for (k, c) in x.iter().enumerate().take(x.len() - min).skip(1) {
			left += c;
			let m = k + 1;
			if m < min {
				continue;
			}
			// Between-group sum of squares: the larger, the better two constants fit.
			let (ml, mr) = (left / m as f64, (total - left) / (x.len() - m) as f64);
			let score = m as f64 * (x.len() - m) as f64 * (ml - mr).powi(2);
			if score > best.0 {
				best = (score, m);
			}
		}
		if best.1 == 0 {
			parts.push((a, b));
		} else {
			stretches.push((a, a + best.1));
			stretches.push((a + best.1, b));
		}
	}
	parts.sort_unstable();
	let mut out: Vec<Hold> = Vec::new();
	for (a, b) in parts {
		let h = make(a, b);
		match out.last_mut() {
			Some(last) if same(last, &h) => {
				let level = last.level.max(h.level);
				*last = Hold {
					level,
					..make(last.start, h.end)
				};
			}
			_ => out.push(h),
		}
	}
	let window = (0.3 / v.hop).round() as usize;
	let loud: Vec<f64> = out
		.iter()
		.map(|h| {
			out.iter()
				.filter(|g| g.end + window >= h.start && g.start <= h.end + window)
				.map(|g| g.level)
				.fold(f64::NEG_INFINITY, f64::max)
		})
		.collect();
	let kept: Vec<Hold> = out
		.into_iter()
		.zip(loud)
		.filter(|(h, loud)| h.level >= loud - o.hold_drop_db)
		.map(|(h, _)| h)
		.collect();
	// What a dropped hold kept apart may be one note.
	let mut joined: Vec<Hold> = Vec::new();
	for h in kept {
		match joined.last_mut() {
			Some(last) if same(last, &h) => {
				let level = last.level.max(h.level);
				*last = Hold {
					level,
					..make(last.start, h.end)
				};
			}
			_ => joined.push(h),
		}
	}
	joined
}

/// Run-relative frame where `curve` crosses halfway from `from` to `to` in `a..b`.
fn halfway(curve: &[f64], a: usize, b: usize, from: f64, to: f64) -> f64 {
	let mid = (from + to) / 2.0;
	for t in (a + 1)..=b.min(curve.len() - 1) {
		let (x, y) = (curve[t - 1], curve[t]);
		if (x - mid) * (y - mid) <= 0.0 && (x - y).abs() > 1e-9 {
			return t as f64 - 1.0 + (mid - x) / (y - x);
		}
	}
	(a + b) as f64 / 2.0
}

/// New boundaries (seconds) for one run whose notes are `notes`: between two holds of
/// different pitch with no boundary in between, and at re-articulations. `fixed` are
/// boundaries that already exist (the user's splits).
pub(crate) fn cuts(
	v: &RunView,
	notes: &[SegmentedNote],
	fixed: &[f64],
	o: &RefineOptions,
) -> Vec<f64> {
	let n = v.raw.len();
	if n < 4 {
		return Vec::new();
	}
	let (first, last) = (
		v.time(0.0) - v.hop / 2.0,
		v.time(n as f64 - 1.0) + v.hop / 2.0,
	);
	let mut bounds: Vec<f64> = notes
		.iter()
		.flat_map(|m| [m.onset, m.offset])
		.chain(fixed.iter().copied().filter(|t| (first..=last).contains(t)))
		.chain([first, last])
		.collect();
	bounds.sort_by(f64::total_cmp);
	let near = |bounds: &[f64], t: f64| bounds.iter().any(|b| (b - t).abs() <= o.tolerance);
	let mut found: Vec<f64> = Vec::new();
	let hs = holds(v, &bounds, o);
	// A boundary inside a hold is misplaced (the next partition moves it): it neither
	// separates two holds nor shortens a new note.
	let inside = |t: f64| {
		hs.iter()
			.any(|h| t > v.time(h.start as f64 + 1.0) && t < v.time(h.end as f64 - 2.0))
	};
	let fixed_or_run =
		|t: f64| fixed.iter().any(|f| (f - t).abs() < 1e-9) || t == first || t == last;
	bounds.retain(|&t| fixed_or_run(t) || !inside(t));
	let edge = (o.edge_hold / v.hop).round() as usize;
	for (k, w) in hs.windows(2).enumerate() {
		let (a, b) = (w[0], w[1]);
		if (k == 0 && a.end - a.start < edge) || (k + 2 == hs.len() && b.end - b.start < edge) {
			continue;
		}
		let (from, to) = (v.time(a.end as f64 - 1.0), v.time(b.start as f64));
		if bounds.iter().any(|&t| t >= from - v.hop && t <= to + v.hop) {
			continue;
		}
		let at = v.time(halfway(v.raw, a.end - 1, b.start, a.cents, b.cents));
		found.push(at);
	}
	// Re-articulations: one per note, its biggest jump.
	let gap = ((o.jump_window / 2.0 / v.hop).round() as usize).max(1);
	let side = (0.12 / v.hop).round() as usize;
	let inner = (0.02 / v.hop).round() as usize;
	for m in notes {
		let lo = (v.frame(m.onset).ceil().max(0.0) as usize) + 4;
		let hi = (v.frame(m.offset).floor() as usize)
			.min(n)
			.saturating_sub(4);
		let mut best: Option<(f64, usize)> = None;
		for i in lo.max(gap).max(inner)..hi.min(n - gap) {
			let jump = (v.raw[i + gap] - v.raw[i - gap]).abs();
			if jump < o.jump_cents {
				continue;
			}
			let peak = |r: std::ops::Range<usize>| {
				v.level[r].iter().copied().fold(f64::NEG_INFINITY, f64::max)
			};
			let before = peak(i.saturating_sub(side)..i - inner);
			let after = peak((i + inner).min(n - 1)..(i + side).min(n));
			let dip = 0.5 * (before + after)
				- v.level[i.saturating_sub(3)..(i + 4).min(n)]
					.iter()
					.copied()
					.fold(f64::INFINITY, f64::min);
			if dip >= o.dip_db && best.is_none_or(|b| jump > b.0) {
				best = Some((jump, i));
			}
		}
		if let Some((_, i)) = best {
			found.push(v.time(i as f64));
		}
	}
	found.sort_by(f64::total_cmp);
	let mut out: Vec<f64> = Vec::new();
	for t in found {
		if near(&bounds, t) || out.last().is_some_and(|l| t - l <= o.tolerance) {
			continue;
		}
		let prev = bounds
			.iter()
			.chain(&out)
			.copied()
			.filter(|&b| b < t)
			.fold(first, f64::max);
		let next = bounds
			.iter()
			.copied()
			.filter(|&b| b > t)
			.fold(last, f64::min);
		if t - prev >= o.min_note && next - t >= o.min_note {
			out.push(t);
		}
	}
	out
}

/// Flags notes holding another pitch outside their core that no rule cut (too short, too
/// quiet, or a boundary already near): the cases an expert looks at.
pub(crate) fn flag(v: &RunView, notes: &mut [SegmentedNote], o: &RefineOptions) {
	let bounds: Vec<f64> = notes.iter().flat_map(|m| [m.onset, m.offset]).collect();
	let hs = holds(v, &bounds, o);
	for m in notes.iter_mut() {
		if m.flags
			.iter()
			.any(|f| matches!(f, NoteFlag::MaybeTwoNotes { .. }))
		{
			continue;
		}
		let center = m.center * 100.0;
		let other = hs.iter().find(|h| {
			let (s, e) = (v.time(h.start as f64), v.time(h.end as f64 - 1.0));
			s >= m.onset
				&& e <= m.offset
				&& (e < m.core_start || s > m.core_end)
				&& (h.cents - center).abs() >= o.hold_cents.max(2.5 * h.depth)
		});
		if let Some(h) = other {
			let e = v.time(h.end as f64 - 1.0);
			let at = if e < m.core_start {
				v.time(halfway(
					v.raw,
					h.end - 1,
					v.frame(m.core_start).round().max(0.0) as usize,
					h.cents,
					center,
				))
			} else {
				v.time(halfway(
					v.raw,
					v.frame(m.core_end).round().max(0.0) as usize,
					h.start,
					center,
					h.cents,
				))
			};
			m.flags.push(NoteFlag::MaybeTwoNotes {
				at,
				cents: (h.cents - center).abs(),
			});
			m.confidence = m.confidence.min(0.5);
		}
	}
}

/// Drops blips that are not the voice (see [`RefineOptions::blip_length`]).
pub(crate) fn drop_blips(analysis: &Analysis, notes: &mut Vec<SegmentedNote>, o: &RefineOptions) {
	let mut weighted: Vec<(f64, f64)> = notes
		.iter()
		.map(|m| (m.center, m.offset - m.onset))
		.collect();
	weighted.sort_by(|a, b| a.0.total_cmp(&b.0));
	let total: f64 = weighted.iter().map(|w| w.1).sum();
	let mut acc = 0.0;
	let Some(take) = weighted.iter().find_map(|&(c, w)| {
		acc += w;
		(acc >= total / 2.0).then_some(c)
	}) else {
		return;
	};
	notes.retain(|m| {
		if m.offset - m.onset >= o.blip_length || (m.center - take).abs() <= o.blip_semitones {
			return true;
		}
		let frames: Vec<_> = analysis
			.frames
			.iter()
			.filter(|f| f.time >= m.onset && f.time <= m.offset && f.pitch.is_some())
			.collect();
		if frames.is_empty() {
			return true;
		}
		let k = frames.len() as f64;
		let hf = frames.iter().map(|f| f.hf_ratio).sum::<f64>() / k;
		let aperiodic = frames.iter().map(|f| f.aperiodicity).sum::<f64>() / k;
		!(hf < 0.05 && aperiodic < 0.2)
	});
}

#[cfg(test)]
mod tests {
	use super::*;

	const HOP: f64 = 0.005;

	/// A run from piecewise-linear (seconds, cents) points at a constant level.
	fn curve(points: &[(f64, f64)]) -> Vec<f64> {
		let n = (points.last().unwrap().0 / HOP).round() as usize;
		(0..n)
			.map(|i| {
				let t = i as f64 * HOP;
				let k = points
					.windows(2)
					.position(|w| t < w[1].0)
					.unwrap_or(points.len() - 2);
				let ((t0, c0), (t1, c1)) = (points[k], points[k + 1]);
				c0 + (c1 - c0) * ((t - t0) / (t1 - t0)).clamp(0.0, 1.0)
			})
			.collect()
	}

	fn note(onset: f64, offset: f64, center: f64) -> SegmentedNote {
		SegmentedNote {
			onset,
			offset,
			core_start: onset + 0.02,
			core_end: offset - 0.02,
			center,
			drift: 0.0,
			vibrato: None,
			confidence: 1.0,
			flags: Vec::new(),
		}
	}

	fn run_cuts(cents: &[f64], level: &[f64], notes: &[SegmentedNote]) -> Vec<f64> {
		let vibrato = vec![None; cents.len()];
		let v = RunView {
			start: 0,
			hop: HOP,
			smooth: cents,
			raw: cents,
			vibrato: &vibrato,
			level,
		};
		cuts(&v, notes, &[], &RefineOptions::default())
	}

	#[test]
	fn a_passing_tone_sung_as_a_slow_down_becomes_a_note() {
		// C, then D held 100 ms while still moving at 500 c/s (a transition to the
		// partition), then E: the partition saw C and E only.
		let c = curve(&[
			(0.0, 6000.0),
			(0.3, 6000.0),
			(0.35, 6175.0),
			(0.45, 6225.0),
			(0.5, 6400.0),
			(0.9, 6400.0),
		]);
		let level = vec![-12.0; c.len()];
		let notes = [note(0.0, 0.4, 60.0), note(0.4, 0.9, 64.0)];
		let cuts = run_cuts(&c, &level, &notes);
		assert_eq!(cuts.len(), 2, "{cuts:?}");
		assert!(
			(cuts[0] - 0.325).abs() < 0.02 && (cuts[1] - 0.47).abs() < 0.02,
			"{cuts:?}"
		);
	}

	#[test]
	fn a_re_articulated_syllable_is_cut_and_a_fall_bottom_is_not() {
		// F, a new syllable a semitone up (fast jump + 8 dB dip), held.
		let mut c = curve(&[(0.0, 5300.0), (0.3, 5300.0), (0.31, 5420.0), (0.6, 5420.0)]);
		let mut level = vec![-12.0; c.len()];
		for l in &mut level[58..66] {
			*l = -20.0;
		}
		assert_eq!(
			run_cuts(&c, &level, &[note(0.0, 0.6, 53.6)])
				.iter()
				.map(|t| (t * 100.0).round() / 100.0)
				.collect::<Vec<_>>(),
			[0.3]
		);
		// A note that falls two semitones and slows down for 40 ms at the bottom: a fall.
		c = curve(&[(0.0, 6400.0), (0.4, 6400.0), (0.48, 6200.0), (0.52, 6190.0)]);
		level = vec![-12.0; c.len()];
		assert!(run_cuts(&c, &level, &[note(0.0, 0.52, 64.0)]).is_empty());
	}
}
