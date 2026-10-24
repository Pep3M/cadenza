//! Note segmentation (NE-2): from an [`Analysis`] to notes whose regions never mix two
//! real notes and whose centers come from their stable cores.
//!
//! 1. Voiced runs: the take is cut at unvoiced stretches (rests, consonants). A gap of a few
//!    frames with the same pitch on both sides and no attack in it is bridged.
//! 2. Vibrato is detected and replaced by its mean over one cycle ([`crate::vibrato`]), so it
//!    can neither split a note nor bias its center.
//! 3. Each run is partitioned **optimally** (dynamic programming over every possible cut)
//!    into *cores* — a constant pitch plus a slow drift of bounded slope, costing their
//!    squared deviation in cents plus a fixed price per note — and *transitions* (glides,
//!    scoops, falls, glitches) costing a flat amount per frame, which are never used for
//!    a center. A new note is created only where it explains more deviation than its price.
//! 4. Neighbouring cores less than `merge_cents` apart are one note (a glitch between them
//!    was a transition); between two different notes the boundary is where the transition
//!    crosses halfway between their centers.
//! 5. Every note checks itself: the best split of its own core is computed, and a gain
//!    close to the price of a note lowers its confidence and flags where it might split.

use serde::{Deserialize, Serialize};

use crate::analysis::{Analysis, AnalysisOptions, analyze};
use crate::vibrato::{self, VibratoFrame, VibratoOptions};
use crate::{Note, NoteDetector};

#[derive(Debug, Clone)]
pub struct SegmentOptions {
	pub vibrato: VibratoOptions,
	/// Price of one note in squared cents: a split must remove at least this much squared
	/// deviation from the fits. Divided by `sensitivity`.
	pub note_cost: f64,
	/// Price of one transition frame where the pitch moves (squared cents).
	pub transition_cost: f64,
	/// Price of one transition frame where the pitch holds still: a plateau is a note, not
	/// a glide, unless it is further than this square root from every core.
	pub flat_transition_cost: f64,
	/// Pitch speed above which a frame counts as moving, cents per second (over 30 ms).
	pub moving_speed: f64,
	/// Largest drift slope inside a core, cents per second; steeper is a glide.
	pub max_drift: f64,
	/// Shortest core, seconds.
	pub min_core: f64,
	/// Longest transition, seconds.
	pub max_transition: f64,
	/// Neighbouring cores closer than this are one note, cents.
	pub merge_cents: f64,
	/// Longest unvoiced gap bridged inside a note, seconds.
	pub bridge_gap: f64,
	/// A core shorter than this at a run edge, gliding to its neighbour and quieter than it,
	/// belongs to that note: the bottom of a fall (below it and `fall_drop_db` quieter, or
	/// `loud_drop_db` quieter in any direction: it decays) or the faint start of a scoop
	/// (`scoop_drop_db`: a grace note is short too, but averages only a little lower because
	/// of its attack). Seconds.
	pub edge_glide_core: f64,
	pub fall_drop_db: f64,
	pub loud_drop_db: f64,
	pub scoop_drop_db: f64,
	/// A voice that stops in a reverberant room leaves its pitch ringing: a level fall of
	/// `tail_drop_db` within 100 ms that does not come back within `tail_hold_db` of the level
	/// before it for at least `min_tail` seconds is a tail, not part of the note.
	pub tail_drop_db: f64,
	pub tail_hold_db: f64,
	pub min_tail: f64,
	/// A core shorter than this (seconds), right next to a note an octave, a twelfth or two
	/// octaves away, is the tracker locking onto a harmonic for a moment (attacks, beating
	/// unison oscillators): it belongs to that note.
	pub harmonic_glitch: f64,
	/// Above 1 finds more notes, below 1 fewer.
	pub sensitivity: f64,
}

impl Default for SegmentOptions {
	fn default() -> Self {
		Self {
			vibrato: VibratoOptions::default(),
			note_cost: 10_000.0,
			transition_cost: 300.0,
			flat_transition_cost: 4_000.0,
			moving_speed: 300.0,
			max_drift: 150.0,
			min_core: 0.03,
			max_transition: 0.4,
			merge_cents: 35.0,
			bridge_gap: 0.025,
			edge_glide_core: 0.1,
			fall_drop_db: 3.0,
			loud_drop_db: 10.0,
			scoop_drop_db: 12.0,
			tail_drop_db: 12.0,
			tail_hold_db: 8.0,
			min_tail: 0.06,
			harmonic_glitch: 0.08,
			sensitivity: 1.0,
		}
	}
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "flag", rename_all = "snake_case")]
pub enum NoteFlag {
	/// Splitting the core at `at` (seconds) would gain almost the price of a note; the two
	/// parts would sit `cents` apart.
	MaybeTwoNotes { at: f64, cents: f64 },
	/// Stable core shorter than 50 ms.
	ShortCore,
	/// Mostly weak voicing (breathy, noisy, reverberant).
	WeakVoicing,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SegmentedNote {
	/// Seconds; between legato notes, where the glide crosses halfway.
	pub onset: f64,
	pub offset: f64,
	/// The stable part the center comes from.
	pub core_start: f64,
	pub core_end: f64,
	/// Fractional MIDI pitch: mean of the core without vibrato.
	pub center: f64,
	/// Drift slope over the core, cents per second.
	pub drift: f64,
	/// Mean vibrato over the core, when it has one.
	pub vibrato: Option<NoteVibrato>,
	/// 0..1: how sure the analysis is that this region is exactly one note.
	pub confidence: f64,
	pub flags: Vec<NoteFlag>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct NoteVibrato {
	pub rate_hz: f64,
	pub depth_cents: f64,
}

/// Unvoiced, bright stretch: s, sh, t, breaths. Never pitch-shifted.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Sibilant {
	pub start: f64,
	pub end: f64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Segmentation {
	pub notes: Vec<SegmentedNote>,
	pub sibilants: Vec<Sibilant>,
}

/// Segments an analysis. `splits` are times (seconds) where a note boundary is forced (the
/// user's separations).
pub fn segment(analysis: &Analysis, opts: &SegmentOptions, splits: &[f64]) -> Segmentation {
	let hop = analysis.hop_seconds();
	let frames = &analysis.frames;
	let mut notes = Vec::new();
	for run in voiced_runs(analysis, opts, splits) {
		let cents: Vec<f64> = run.pitch.iter().map(|p| p * 100.0).collect();
		let vib = vibrato::detect(&cents, hop, &opts.vibrato);
		let smooth = vibrato::remove(&cents, &vib, hop);
		let parts = partition(&smooth, hop, opts);
		let voicing: Vec<f64> = (run.start..run.end).map(|i| frames[i].voicing).collect();
		let level: Vec<f64> = (run.start..run.end).map(|i| frames[i].level_db).collect();
		let curves = RunCurves {
			smooth: &smooth,
			vibrato: &vib,
			voicing: &voicing,
			level: &level,
		};
		notes.extend(notes_of_run(&run, &parts, &curves, hop, opts));
	}
	Segmentation {
		notes,
		sibilants: sibilants(analysis),
	}
}

/// A stretch of consecutive frames treated as continuously voiced.
struct Run {
	/// Frame range in the analysis.
	start: usize,
	end: usize,
	/// Pitch per frame (MIDI), bridged gaps interpolated.
	pitch: Vec<f64>,
}

fn voiced_runs(analysis: &Analysis, opts: &SegmentOptions, splits: &[f64]) -> Vec<Run> {
	let frames = &analysis.frames;
	let hop = analysis.hop_seconds();
	let bridge = (opts.bridge_gap / hop).round() as usize;
	let split_frames: Vec<usize> = splits
		.iter()
		.filter_map(|&t| analysis.frame_at(t))
		.collect();
	let mut ranges: Vec<(usize, usize)> = Vec::new();
	let mut i = 0;
	while i < frames.len() {
		if frames[i].pitch.is_none() {
			i += 1;
			continue;
		}
		let start = i;
		while i < frames.len() && frames[i].pitch.is_some() {
			i += 1;
		}
		ranges.push((start, i));
	}
	// Bridge short gaps with continuous pitch and no attack.
	let mut merged: Vec<(usize, usize)> = Vec::new();
	for r in ranges {
		if let Some(last) = merged.last_mut() {
			let gap = r.0 - last.1;
			let before = frames[last.1 - 1].pitch.unwrap_or(0.0);
			let after = frames[r.0].pitch.unwrap_or(0.0);
			let forced = split_frames.iter().any(|&s| s >= last.1 && s <= r.0);
			if gap <= bridge
				&& (before - after).abs() < 0.5
				&& !forced && !onset_between(analysis, last.1, r.0)
			{
				last.1 = r.1;
				continue;
			}
		}
		merged.push(r);
	}
	// Forced splits cut runs.
	let mut cut: Vec<(usize, usize)> = Vec::new();
	for (a, b) in merged {
		let mut start = a;
		let mut inside: Vec<usize> = split_frames
			.iter()
			.copied()
			.filter(|&s| s > a && s < b)
			.collect();
		inside.sort_unstable();
		for s in inside {
			if s > start {
				cut.push((start, s));
				start = s;
			}
		}
		cut.push((start, b));
	}
	cut.into_iter()
		.flat_map(|(a, b)| without_tails(analysis, a, b, opts))
		.map(|(start, end)| {
			let mut pitch: Vec<Option<f64>> = frames[start..end].iter().map(|f| f.pitch).collect();
			fill_gaps(&mut pitch);
			Run {
				start,
				end,
				pitch: pitch.into_iter().map(|p| p.unwrap_or(0.0)).collect(),
			}
		})
		.collect()
}

/// Frames `a..b` with reverb tails taken out (see [`SegmentOptions::tail_drop_db`]).
fn without_tails(
	analysis: &Analysis,
	a: usize,
	b: usize,
	opts: &SegmentOptions,
) -> Vec<(usize, usize)> {
	let level: Vec<f64> = analysis.frames[a..b].iter().map(|f| f.level_db).collect();
	let hop = analysis.hop_seconds();
	let look = ((0.1 / hop).round() as usize).max(1);
	let min_tail = (opts.min_tail / hop).round() as usize;
	let mut out = Vec::new();
	let (mut start, mut t) = (0, look);
	while t < level.len() {
		let before = level[t - look..t]
			.iter()
			.copied()
			.fold(f64::NEG_INFINITY, f64::max);
		if level[t] > before - opts.tail_drop_db {
			t += 1;
			continue;
		}
		// The fall starts where the level first leaves the top of the window.
		let fall = (t - look..t)
			.find(|&k| level[k] < before - 3.0)
			.unwrap_or(t);
		let end = (t..level.len())
			.find(|&k| level[k] >= before - opts.tail_hold_db)
			.unwrap_or(level.len());
		if end - fall >= min_tail && fall > start {
			out.push((a + start, a + fall));
			start = end;
		}
		t = end.max(t + 1);
	}
	if start < level.len() {
		out.push((a + start, b));
	}
	out
}

/// Linear interpolation over `None`s between voiced frames.
fn fill_gaps(pitch: &mut [Option<f64>]) {
	let mut last: Option<(usize, f64)> = None;
	let mut i = 0;
	while i < pitch.len() {
		if let Some(p) = pitch[i] {
			if let Some((j, q)) = last
				&& i > j + 1
			{
				for (k, slot) in pitch.iter_mut().enumerate().take(i).skip(j + 1) {
					let u = (k - j) as f64 / (i - j) as f64;
					*slot = Some(q + (p - q) * u);
				}
			}
			last = Some((i, p));
		}
		i += 1;
	}
}

/// A clear attack inside frames `a..b` (spectral flux well above its local median).
fn onset_between(analysis: &Analysis, a: usize, b: usize) -> bool {
	let frames = &analysis.frames;
	let lo = a.saturating_sub(40);
	let hi = (b + 40).min(frames.len());
	let mut around: Vec<f64> = frames[lo..hi].iter().map(|f| f.flux).collect();
	if around.is_empty() {
		return false;
	}
	around.sort_by(f64::total_cmp);
	let median = around[around.len() / 2].max(1e-6);
	frames[a.saturating_sub(1)..(b + 1).min(frames.len())]
		.iter()
		.any(|f| f.flux > 4.0 * median)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
	Core,
	Transition,
}

#[derive(Debug, Clone, Copy)]
struct Part {
	kind: Kind,
	/// Run-relative frame range.
	start: usize,
	end: usize,
}

/// Prefix sums for O(1) least-squares fits of a line with bounded slope.
struct Fits {
	s1: Vec<f64>,
	s2: Vec<f64>,
	q1: Vec<f64>,
	q2: Vec<f64>,
	tq: Vec<f64>,
	/// Subtracted from every value before summing (precision); added back to means.
	offset: f64,
	max_slope: f64,
}

#[derive(Debug, Clone, Copy)]
struct Fit {
	/// Mean of the segment (the line's value at its middle).
	mean: f64,
	slope: f64,
	sse: f64,
}

impl Fits {
	fn new(values: &[f64], max_slope_per_frame: f64) -> Self {
		let n = values.len();
		let offset = values.iter().sum::<f64>() / n.max(1) as f64;
		let mut f = Fits {
			s1: vec![0.0; n + 1],
			s2: vec![0.0; n + 1],
			q1: vec![0.0; n + 1],
			q2: vec![0.0; n + 1],
			tq: vec![0.0; n + 1],
			offset,
			max_slope: max_slope_per_frame,
		};
		for (i, v) in values.iter().enumerate() {
			let (t, q) = (i as f64, v - offset);
			f.s1[i + 1] = f.s1[i] + t;
			f.s2[i + 1] = f.s2[i] + t * t;
			f.q1[i + 1] = f.q1[i] + q;
			f.q2[i + 1] = f.q2[i] + q * q;
			f.tq[i + 1] = f.tq[i] + t * q;
		}
		f
	}

	fn fit(&self, a: usize, b: usize) -> Fit {
		let n = (b - a) as f64;
		let s1 = self.s1[b] - self.s1[a];
		let s2 = self.s2[b] - self.s2[a];
		let q1 = self.q1[b] - self.q1[a];
		let q2 = self.q2[b] - self.q2[a];
		let tq = self.tq[b] - self.tq[a];
		let sxx = s2 - s1 * s1 / n;
		let sxq = tq - s1 * q1 / n;
		let sqq = q2 - q1 * q1 / n;
		let slope = if sxx > 1e-9 {
			(sxq / sxx).clamp(-self.max_slope, self.max_slope)
		} else {
			0.0
		};
		Fit {
			mean: q1 / n + self.offset,
			slope,
			sse: (sqq - 2.0 * slope * sxq + slope * slope * sxx).max(0.0),
		}
	}
}

/// Optimal partition of a run into cores and transitions (see the module docs).
fn partition(values: &[f64], hop: f64, opts: &SegmentOptions) -> Vec<Part> {
	let n = values.len();
	let min_core = ((opts.min_core / hop).round() as usize).max(2);
	if n < min_core {
		return vec![Part {
			kind: Kind::Core,
			start: 0,
			end: n,
		}];
	}
	let fits = Fits::new(values, opts.max_drift * hop);
	let max_trans = ((opts.max_transition / hop).round() as usize).max(1);
	let price = opts.note_cost / opts.sensitivity.max(1e-3);
	// Prefix sums of the per-frame transition price.
	let reach = 3usize;
	let speed_limit = opts.moving_speed * hop * (2 * reach) as f64;
	let mut kappa = vec![0.0f64; n + 1];
	for t in 0..n {
		let (a, b) = (t.saturating_sub(reach), (t + reach).min(n - 1));
		let moving = (values[b] - values[a]).abs() * (2 * reach) as f64 / (b - a).max(1) as f64
			>= speed_limit;
		kappa[t + 1] = kappa[t]
			+ if moving {
				opts.transition_cost
			} else {
				opts.flat_transition_cost
			};
	}
	const INF: f64 = f64::INFINITY;
	// Previous state of a back pointer: 0 = start, 1 = core, 2 = transition.
	let mut core = vec![INF; n + 1];
	let mut trans = vec![INF; n + 1];
	let mut core_back = vec![(0usize, 0u8); n + 1];
	let mut trans_back = vec![(0usize, 0u8); n + 1];
	for j in 1..=n {
		if j >= min_core {
			for i in 0..=j - min_core {
				let (before, from) = if i == 0 {
					(0.0, 0)
				} else if core[i] <= trans[i] {
					(core[i], 1)
				} else {
					(trans[i], 2)
				};
				if before == INF {
					continue;
				}
				let c = before + fits.fit(i, j).sse + price;
				if c < core[j] {
					core[j] = c;
					core_back[j] = (i, from);
				}
			}
		}
		for i in j.saturating_sub(max_trans)..j {
			let (before, from) = if i == 0 { (0.0, 0) } else { (core[i], 1) };
			if before == INF {
				continue;
			}
			let c = before + kappa[j] - kappa[i];
			if c < trans[j] {
				trans[j] = c;
				trans_back[j] = (i, from);
			}
		}
	}
	if core[n] == INF {
		return vec![Part {
			kind: Kind::Core,
			start: 0,
			end: n,
		}];
	}
	// End on a core, or on a transition (a fall) that follows one.
	let mut state = if core[n] <= trans[n] || trans_back[n].1 == 0 {
		1u8
	} else {
		2u8
	};
	let mut parts = Vec::new();
	let mut j = n;
	while j > 0 {
		let (i, from) = if state == 1 {
			core_back[j]
		} else {
			trans_back[j]
		};
		parts.push(Part {
			kind: if state == 1 {
				Kind::Core
			} else {
				Kind::Transition
			},
			start: i,
			end: j,
		});
		j = i;
		state = from;
	}
	parts.reverse();
	parts
}

/// Per-frame curves of one run.
struct RunCurves<'a> {
	/// Pitch in cents without vibrato.
	smooth: &'a [f64],
	vibrato: &'a [Option<VibratoFrame>],
	voicing: &'a [f64],
	level: &'a [f64],
}

/// Turns a run's parts into notes: merges same-pitch neighbours, folds scoop starts and fall
/// ends into their notes, places boundaries, scores.
fn notes_of_run(
	run: &Run,
	parts: &[Part],
	curves: &RunCurves,
	hop: f64,
	opts: &SegmentOptions,
) -> Vec<SegmentedNote> {
	let (smooth, vib, voicing) = (curves.smooth, curves.vibrato, curves.voicing);
	let fits = Fits::new(smooth, opts.max_drift * hop);
	// Cores as lists of run-relative frame ranges (merging keeps the ranges, not the glitch).
	struct CoreGroup {
		ranges: Vec<(usize, usize)>,
		/// Transition before it (inside the run), if any.
		lead: Option<(usize, usize)>,
	}
	let mut groups: Vec<CoreGroup> = Vec::new();
	let mut pending_lead: Option<(usize, usize)> = None;
	let mean_of = |ranges: &[(usize, usize)]| {
		let (sum, count) = ranges.iter().fold((0.0, 0usize), |(s, c), &(a, b)| {
			(s + fits.fit(a, b).mean * (b - a) as f64, c + b - a)
		});
		sum / count.max(1) as f64
	};
	let max_glitch = (0.06 / hop).round() as usize;
	for p in parts {
		match p.kind {
			Kind::Transition => {
				pending_lead = Some((p.start, p.end));
			}
			Kind::Core => {
				let lead = pending_lead.take();
				if let Some(last) = groups.last_mut() {
					let gap = lead.map_or(0, |(a, b)| b - a);
					let close = (mean_of(&last.ranges) - fits.fit(p.start, p.end).mean).abs()
						< opts.merge_cents;
					if close && gap <= max_glitch {
						last.ranges.push((p.start, p.end));
						continue;
					}
				}
				groups.push(CoreGroup {
					ranges: vec![(p.start, p.end)],
					lead,
				});
			}
		}
	}
	// Harmonic glitches: drop them, joining the notes on both sides when they are one.
	let adjacent = (0.04 / hop).round() as usize;
	let harmonic = |a: f64, b: f64| {
		let d = (a - b).abs();
		[1200.0, 1901.955, 2400.0]
			.iter()
			.any(|h| (d - h).abs() < 40.0)
	};
	let mut k = 0;
	while k < groups.len() {
		let g = &groups[k];
		let short = (g.ranges.last().expect("ranges").1 - g.ranges[0].0) as f64 * hop
			< opts.harmonic_glitch;
		let center = mean_of(&g.ranges);
		let gap = |lead: Option<(usize, usize)>| lead.map_or(0, |(a, b)| b - a);
		let prev = k.checked_sub(1).filter(|_| {
			gap(g.lead) <= adjacent && harmonic(center, mean_of(&groups[k - 1].ranges))
		});
		let next = groups
			.get(k + 1)
			.filter(|n| gap(n.lead) <= adjacent && harmonic(center, mean_of(&n.ranges)))
			.map(|_| k + 1);
		if !short || (prev.is_none() && next.is_none()) {
			k += 1;
			continue;
		}
		let glitch = groups.remove(k);
		if k > 0 && k < groups.len() {
			let joined = (mean_of(&groups[k - 1].ranges) - mean_of(&groups[k].ranges)).abs()
				< opts.merge_cents;
			if joined {
				let after = groups.remove(k);
				groups[k - 1].ranges.extend(after.ranges);
			} else if next.is_some() {
				// It is the start of the next note: the boundary moves to where it began.
				let start = glitch.ranges[0].0;
				groups[k].lead = Some(glitch.lead.unwrap_or((start, start)));
			}
		} else if k == 0 && !groups.is_empty() {
			// The note it led into starts with the run.
			groups[0].lead = None;
		}
	}
	// A short core at either end of the run, joined to its neighbour by a glide and clearly
	// quieter than it, is the start of a scoop or the bottom of a fall, not a note (a grace
	// note sounds at full level).
	let level_of = |g: &CoreGroup| {
		let (sum, count) = g.ranges.iter().fold((0.0, 0usize), |(s, c), &(a, b)| {
			(s + curves.level[a..b].iter().sum::<f64>(), c + b - a)
		});
		sum / count.max(1) as f64
	};
	// Median, so the attack ramp of a short grace note does not pass for a quiet scoop.
	let median_level = |g: &CoreGroup| {
		let mut levels: Vec<f64> = g
			.ranges
			.iter()
			.flat_map(|&(a, b)| curves.level[a..b].iter().copied())
			.collect();
		levels.sort_by(f64::total_cmp);
		levels
			.get(levels.len() / 2)
			.copied()
			.unwrap_or(f64::NEG_INFINITY)
	};
	let span = |g: &CoreGroup| (g.ranges.last().expect("ranges").1 - g.ranges[0].0) as f64 * hop;
	// A core sloped near the drift limit is a glide the partition kept as a core.
	let glides = |g: &CoreGroup, lead: Option<(usize, usize)>| {
		let (a, b) = (g.ranges[0].0, g.ranges.last().expect("ranges").1);
		lead.is_some() || fits.fit(a, b).slope.abs() >= 0.8 * opts.max_drift * hop
	};
	// The analysis cannot judge vibrato in the last half window of a run, so the swing that
	// ends (or starts) a note held with vibrato can come out as a short sloped core.
	let in_swing = |edge: &CoreGroup, note: &CoreGroup| {
		let (a, b) = (note.ranges[0].0, note.ranges.last().expect("ranges").1);
		let frames: Vec<f64> = vib[a..b]
			.iter()
			.filter_map(|v| v.map(|v| v.depth_cents))
			.collect();
		frames.len() * 2 >= b - a && {
			let depth = frames.iter().sum::<f64>() / frames.len() as f64;
			(mean_of(&edge.ranges) - mean_of(&note.ranges)).abs() <= 1.5 * depth
		}
	};
	if groups.len() >= 2 {
		let (first, second) = (&groups[0], &groups[1]);
		if span(first) < opts.edge_glide_core
			&& (second.lead.is_some() || glides(first, None))
			&& (median_level(first) < median_level(second) - opts.scoop_drop_db
				|| in_swing(first, second))
		{
			groups.remove(0);
		}
	}
	if groups.len() >= 2 {
		let k = groups.len() - 1;
		let (before, last) = (&groups[k - 1], &groups[k]);
		let drop = level_of(before) - level_of(last);
		let (a, b) = (last.ranges[0].0, last.ranges.last().expect("ranges").1);
		// A fall leaving a vibrato peak can sit above the note's mean while it goes down.
		let down = mean_of(&last.ranges) < mean_of(&before.ranges) || fits.fit(a, b).slope < 0.0;
		if span(last) < opts.edge_glide_core
			&& glides(last, last.lead)
			&& (drop >= opts.loud_drop_db
				|| (down && drop >= opts.fall_drop_db)
				|| in_swing(last, before))
		{
			groups.pop();
		}
	}
	let time = |frame: usize| (run.start + frame) as f64 * hop;
	let n = smooth.len();
	let centers: Vec<f64> = groups.iter().map(|g| mean_of(&g.ranges)).collect();
	let mut notes = Vec::with_capacity(groups.len());
	for (k, g) in groups.iter().enumerate() {
		let core_a = g.ranges[0].0;
		let core_b = g.ranges.last().expect("a core group has a range").1;
		let onset = if k == 0 {
			time(0) - hop / 2.0
		} else {
			match g.lead {
				Some((a, b)) => crossing(smooth, a, b, centers[k - 1], centers[k], hop) + time(0),
				None => time(core_a) - hop / 2.0,
			}
		};
		let offset = if k + 1 == groups.len() {
			time(n - 1) + hop / 2.0
		} else {
			match groups[k + 1].lead {
				Some((a, b)) => crossing(smooth, a, b, centers[k], centers[k + 1], hop) + time(0),
				None => time(core_b) - hop / 2.0,
			}
		};
		// Over the core ranges only (a merged glitch between them is not the note).
		let frames: usize = g.ranges.iter().map(|&(a, b)| b - a).sum();
		let slope = g
			.ranges
			.iter()
			.map(|&(a, b)| fits.fit(a, b).slope * (b - a) as f64)
			.sum::<f64>()
			/ frames.max(1) as f64;
		let (confidence, split) = self_check(&fits, &g.ranges, opts);
		let mut flags = Vec::new();
		if let Some(f) = split {
			flags.push(NoteFlag::MaybeTwoNotes {
				at: time(f.0),
				cents: f.1,
			});
		}
		if (core_b - core_a) as f64 * hop < 0.05 {
			flags.push(NoteFlag::ShortCore);
		}
		let weak = voicing[core_a..core_b].iter().sum::<f64>() / (core_b - core_a).max(1) as f64;
		if weak < 0.5 {
			flags.push(NoteFlag::WeakVoicing);
		}
		let vib_frames: Vec<VibratoFrame> = vib[core_a..core_b].iter().filter_map(|v| *v).collect();
		let vibrato = (vib_frames.len() * 2 >= core_b - core_a).then(|| NoteVibrato {
			rate_hz: vib_frames.iter().map(|v| v.rate_hz).sum::<f64>() / vib_frames.len() as f64,
			depth_cents: vib_frames.iter().map(|v| v.depth_cents).sum::<f64>()
				/ vib_frames.len() as f64,
		});
		notes.push(SegmentedNote {
			onset,
			offset,
			core_start: time(core_a) - hop / 2.0,
			core_end: time(core_b - 1) + hop / 2.0,
			center: centers[k] / 100.0,
			drift: slope / hop,
			vibrato,
			confidence,
			flags,
		});
	}
	notes
}

/// Seconds from the run start where `smooth` crosses halfway from `from` to `to` inside the
/// transition `a..b` (its middle when it never does).
fn crossing(smooth: &[f64], a: usize, b: usize, from: f64, to: f64, hop: f64) -> f64 {
	let mid = (from + to) / 2.0;
	let up = to > from;
	let lo = a.saturating_sub(1);
	for t in (lo + 1)..b.min(smooth.len()) {
		let (x, y) = (smooth[t - 1], smooth[t]);
		let crossed = if up {
			x < mid && y >= mid
		} else {
			x > mid && y <= mid
		};
		if crossed {
			let u = if (y - x).abs() > 1e-9 {
				(mid - x) / (y - x)
			} else {
				0.5
			};
			return (t as f64 - 1.0 + u) * hop;
		}
	}
	(a + b) as f64 / 2.0 * hop - hop / 2.0
}

/// Confidence of a core and the best split it resists: (confidence, Some((frame, cents))).
fn self_check(
	fits: &Fits,
	ranges: &[(usize, usize)],
	opts: &SegmentOptions,
) -> (f64, Option<(usize, f64)>) {
	let (a, b) = (ranges[0].0, ranges.last().expect("ranges").1);
	let min = 6;
	if b - a < 2 * min {
		return (1.0, None);
	}
	let whole = fits.fit(a, b).sse;
	let mut best: Option<(f64, usize, f64)> = None;
	for m in (a + min)..=(b - min) {
		let (l, r) = (fits.fit(a, m), fits.fit(m, b));
		let cents = (l.mean - r.mean).abs();
		if cents < 50.0 {
			continue;
		}
		let gain = whole - l.sse - r.sse;
		if best.is_none_or(|x| gain > x.0) {
			best = Some((gain, m, cents));
		}
	}
	let price = opts.note_cost / opts.sensitivity.max(1e-3);
	match best {
		Some((gain, m, cents)) => {
			let ratio = (gain / price).max(0.0);
			let confidence = (1.0 - ratio).clamp(0.0, 1.0);
			(confidence, (ratio > 0.5).then_some((m, cents)))
		}
		None => (1.0, None),
	}
}

fn sibilants(analysis: &Analysis) -> Vec<Sibilant> {
	let loudest = analysis
		.frames
		.iter()
		.map(|f| f.level_db)
		.fold(-120.0, f64::max);
	let hop = analysis.hop_seconds();
	let mut out = Vec::new();
	let mut start: Option<usize> = None;
	for (i, f) in analysis.frames.iter().enumerate() {
		let bright = f.pitch.is_none() && f.hf_ratio > 0.35 && f.level_db > loudest - 45.0;
		match (bright, start) {
			(true, None) => start = Some(i),
			(false, Some(s)) => {
				if (i - s) as f64 * hop >= 0.02 {
					out.push(Sibilant {
						start: s as f64 * hop - hop / 2.0,
						end: i as f64 * hop - hop / 2.0,
					});
				}
				start = None;
			}
			_ => {}
		}
	}
	out
}

/// Analysis + segmentation as a [`NoteDetector`].
#[derive(Debug, Clone, Default)]
pub struct Segmenter {
	pub analysis: AnalysisOptions,
	pub segment: SegmentOptions,
}

impl NoteDetector for Segmenter {
	fn name(&self) -> &'static str {
		"segment"
	}

	fn detect(&self, samples: &[f32], sample_rate: u32) -> Vec<Note> {
		let Some(analysis) = analyze(samples, sample_rate, &self.analysis, &mut |_| true) else {
			return Vec::new();
		};
		segment(&analysis, &self.segment, &[])
			.notes
			.into_iter()
			.map(|n| Note {
				onset: n.onset,
				offset: n.offset,
				center: n.center,
			})
			.collect()
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::eval::synth::{Phrase, Rendered, SynthNote, Vibrato, Vowel, render};

	const RATE: u32 = 22_050;

	fn phrase(notes: Vec<SynthNote>, reverb: Option<f64>) -> Rendered {
		render(&Phrase {
			notes,
			vowel: Vowel::A,
			sample_rate: RATE,
			snr_db: None,
			reverb,
			seed: 7,
		})
	}

	fn run(r: &Rendered, splits: &[f64]) -> Segmentation {
		let analysis =
			analyze(&r.samples, RATE, &AnalysisOptions::default(), &mut |_| true).unwrap();
		segment(&analysis, &SegmentOptions::default(), splits)
	}

	#[test]
	fn a_long_glide_splits_at_its_middle_and_centers_ignore_it() {
		// The case Melodyne gets wrong: a slow portamento between two notes.
		let r = phrase(
			vec![
				SynthNote::new(57.0, 0.6),
				SynthNote::new(62.0, 0.6).legato(0.25),
			],
			None,
		);
		let s = run(&r, &[]);
		assert_eq!(s.notes.len(), 2, "{s:?}");
		let boundary = r.truth[1].onset;
		assert!(
			(s.notes[0].offset - boundary).abs() < 0.015,
			"{} vs {boundary}",
			s.notes[0].offset
		);
		assert_eq!(s.notes[0].offset, s.notes[1].onset);
		// The centers come from the cores alone, which stay out of the glide.
		assert!(
			s.notes[0].core_end < boundary - 0.05 && s.notes[1].core_start > boundary + 0.05,
			"{s:?}"
		);
		for (n, t) in s.notes.iter().zip(&r.truth) {
			assert!((n.center - t.center).abs() * 100.0 < 3.0, "{n:?} vs {t:?}");
			assert_eq!(n.confidence, 1.0);
		}
	}

	#[test]
	fn vibrato_is_one_note_centered_on_its_mean() {
		let mut note = SynthNote::new(64.0, 1.4);
		note.vibrato = Some(Vibrato {
			rate_hz: 5.5,
			depth_cents: 60.0,
			delay: 0.2,
		});
		let r = phrase(vec![note], None);
		let s = run(&r, &[]);
		assert_eq!(s.notes.len(), 1, "{s:?}");
		let n = &s.notes[0];
		assert!((n.center - r.truth[0].center).abs() * 100.0 < 3.0, "{n:?}");
		let v = n.vibrato.expect("vibrato");
		assert!(
			(v.rate_hz - 5.5).abs() < 0.5 && (v.depth_cents - 60.0).abs() < 15.0,
			"{v:?}"
		);
	}

	#[test]
	fn a_forced_split_cuts_a_steady_note() {
		let r = phrase(vec![SynthNote::new(60.0, 1.0)], None);
		let at = r.truth[0].onset + 0.5;
		assert_eq!(run(&r, &[]).notes.len(), 1);
		let s = run(&r, &[at]);
		assert_eq!(s.notes.len(), 2, "{s:?}");
		assert!(
			(s.notes[0].offset - at).abs() < 0.01 && (s.notes[1].onset - at).abs() < 0.01,
			"{s:?}"
		);
	}

	#[test]
	fn drift_stays_one_note_with_its_slope() {
		let mut note = SynthNote::new(55.0, 1.0);
		note.drift_cents = 40.0;
		let s = run(&phrase(vec![note], None), &[]);
		assert_eq!(s.notes.len(), 1, "{s:?}");
		assert!((s.notes[0].drift - 40.0).abs() < 15.0, "{:?}", s.notes[0]);
	}

	#[test]
	fn a_reverb_tail_is_not_part_of_the_note() {
		let r = phrase(
			vec![
				SynthNote::new(60.0, 0.5),
				SynthNote::new(64.0, 0.5).rest(0.4),
			],
			Some(0.3),
		);
		let s = run(&r, &[]);
		assert_eq!(s.notes.len(), 2, "{s:?}");
		for (n, t) in s.notes.iter().zip(&r.truth) {
			assert!((n.offset - t.offset).abs() < 0.05, "{n:?} vs {t:?}");
		}
	}
}
