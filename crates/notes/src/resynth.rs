//! Resynthesis of an edited recording (NE-4): pitch marks + edit curves + time map ->
//! PSOLA, then a closed loop that measures every note of the render with the same
//! analysis and segmentation and re-renders once with the residuals taken out.

use serde::{Deserialize, Serialize};

use crate::NotesError;
use crate::analysis::{Analysis, AnalysisOptions, analyze};
use crate::edit::{self, Edits};
use crate::marks::{Marks, pitch_marks};
use crate::psola::{Grain, REACH_SECONDS, changed_span, output_len, plan, psola_range};
use crate::segment::{SegmentOptions, Segmentation, segment};
use crate::timemap::TimeMap;
use crate::vibrato;

#[derive(Debug, Clone, PartialEq)]
pub struct Render {
	/// Same channel count as the input.
	pub channels: Vec<Vec<f32>>,
	/// Source seconds <-> render seconds.
	pub map: TimeMap,
}

/// Mono mix of `channels` (what the analysis must have been made from).
pub fn mono(channels: &[&[f32]]) -> Vec<f32> {
	let len = channels.first().map_or(0, |c| c.len());
	if channels.len() == 1 {
		return channels[0].to_vec();
	}
	let scale = 1.0 / channels.len().max(1) as f32;
	(0..len)
		.map(|i| channels.iter().map(|c| c[i]).sum::<f32>() * scale)
		.collect()
}

/// Renders `edits` of `seg` (found on `analysis` of the mono mix of `channels`).
pub fn render(
	channels: &[&[f32]],
	analysis: &Analysis,
	seg: &Segmentation,
	edits: &Edits,
) -> Result<Render, NotesError> {
	check(channels, seg, edits)?;
	if edits.is_unchanged() {
		return Ok(Render {
			channels: channels.iter().map(|c| c.to_vec()).collect(),
			map: edit::time_map(seg, edits)?,
		});
	}
	let input = mono(channels);
	let marks = pitch_marks(&input, analysis);
	let src = Source {
		channels,
		analysis,
		seg,
		marks: &marks,
		input: &input,
	};
	let (part, _) = render_full(&src, edits)?;
	Ok(Render {
		channels: part.channels,
		map: part.map,
	})
}

fn check(channels: &[&[f32]], seg: &Segmentation, edits: &Edits) -> Result<(), NotesError> {
	if channels.iter().any(|c| c.len() != channels[0].len()) {
		return Err(NotesError::Edit(
			"every channel must have the same length".into(),
		));
	}
	edits.validate(seg)
}

/// The audio a render works from.
struct Source<'a> {
	channels: &'a [&'a [f32]],
	analysis: &'a Analysis,
	seg: &'a Segmentation,
	marks: &'a Marks,
	/// Mono mix of `channels`.
	input: &'a [f32],
}

impl Source<'_> {
	fn rate(&self) -> u32 {
		self.analysis.sample_rate
	}

	fn len(&self) -> usize {
		self.input.len()
	}
}

/// Output samples `from..from + len` of a render (the whole of it when `from` is 0 and it
/// is as long as the render).
struct Part {
	from: usize,
	channels: Vec<Vec<f32>>,
	map: TimeMap,
}

/// The whole render of `edits`, each reshaped note brought back to its own level, and the
/// edits it followed (those level fixes in).
fn render_full(src: &Source, edits: &Edits) -> Result<(Part, Edits), NotesError> {
	let map = edit::time_map(src.seg, edits)?;
	let mut effective = edits.clone();
	effective
		.notes
		.resize(src.seg.notes.len(), Default::default());
	if edits.is_unchanged() {
		let part = Part {
			from: 0,
			channels: src.channels.iter().map(|c| c.to_vec()).collect(),
			map,
		};
		return Ok((part, effective));
	}
	let rate = src.rate();
	let out_len = output_len(src.channels, rate, &map);
	let draw = |e: &Edits| {
		let curves = edit::curves(src.analysis, src.seg, e);
		let grains = plan(rate, src.marks, &curves, &map, out_len);
		psola_range(
			src.channels,
			rate,
			src.marks,
			&curves,
			&map,
			&grains,
			0..out_len,
		)
	};
	let mut channels = draw(&effective);
	if let Some(leveled) = level_fix(src, &effective, &channels, 0, &map, |_| true) {
		effective = leveled;
		channels = draw(&effective);
	}
	Ok((
		Part {
			from: 0,
			channels,
			map,
		},
		effective,
	))
}

/// `edits` with the reshaped notes `include` accepts brought back to their own level,
/// measured on `rendered` (output samples `from..`); `None` when none needs it.
fn level_fix(
	src: &Source,
	edits: &Edits,
	rendered: &[Vec<f32>],
	from: usize,
	map: &TimeMap,
	include: impl Fn(usize) -> bool,
) -> Option<Edits> {
	// Re-spaced grains sample the spectrum between the original harmonics, so a moved note
	// comes out up to a few dB louder or softer.
	let out: Vec<&[f32]> = rendered.iter().map(Vec::as_slice).collect();
	let output = mono(&out);
	let r = f64::from(src.rate());
	let span = |x: &[f32], offset: usize, a: f64, b: f64| {
		let a = ((a * r) as usize).saturating_sub(offset);
		let b = ((b * r) as usize).saturating_sub(offset).min(x.len());
		rms_db(&x[a.min(b)..b])
	};
	let mut leveled = edits.clone();
	let mut changed = false;
	for (i, (e, n)) in leveled.notes.iter_mut().zip(&src.seg.notes).enumerate() {
		let reshaped = e.pitch.is_some_and(|p| p != n.center)
			|| e.formant_cents != 0.0
			|| e.drift != 1.0
			|| e.modulation != 1.0;
		if !reshaped || !include(i) {
			continue;
		}
		let before = span(src.input, 0, n.core_start, n.core_end);
		let after = span(
			&output,
			from,
			map.to_output(n.core_start),
			map.to_output(n.core_end),
		);
		if before > -100.0 && after > -100.0 {
			let fix = (before - after).clamp(-LEVEL_FIX_DB, LEVEL_FIX_DB);
			if fix.abs() > 0.05 {
				e.gain_db = (e.gain_db + fix).clamp(-60.0, 24.0);
				changed = true;
			}
		}
	}
	changed.then_some(leveled)
}

/// Largest level correction of a reshaped note, dB.
const LEVEL_FIX_DB: f64 = 6.0;

fn rms_db(x: &[f32]) -> f64 {
	if x.is_empty() {
		return -144.0;
	}
	let e = x.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() / x.len() as f64;
	10.0 * e.max(1e-15).log10()
}

/// How far a note of the render landed from where it was asked to go.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Residual {
	/// Index in the segmentation.
	pub note: usize,
	/// Asked-for center (the sung one when the pitch was not edited).
	pub target: f64,
	/// Center measured on the render; `None` when no note was found there.
	pub measured: Option<f64>,
}

impl Residual {
	pub fn cents(&self) -> Option<f64> {
		self.measured.map(|m| (m - self.target) * 100.0)
	}
}

#[derive(Debug, Clone)]
pub struct VerifyOptions {
	pub analysis: AnalysisOptions,
	pub segment: SegmentOptions,
	/// Notes further than this from their target are rendered again, cents. 0 = one pass.
	pub tolerance_cents: f64,
	/// Most extra passes.
	pub passes: usize,
}

impl Default for VerifyOptions {
	fn default() -> Self {
		Self {
			analysis: AnalysisOptions::default(),
			segment: SegmentOptions::default(),
			tolerance_cents: 2.0,
			passes: 2,
		}
	}
}

/// Measures every note of `render` (its mono mix) against what `edits` asked.
pub fn verify(
	render: &Render,
	sample_rate: u32,
	seg: &Segmentation,
	edits: &Edits,
	opts: &VerifyOptions,
) -> Vec<Residual> {
	measure(
		&render.channels,
		0,
		&render.map,
		sample_rate,
		seg,
		edits,
		opts,
		|_| true,
	)
}

/// Measures the notes `include` accepts on `rendered` (output samples `from..`).
#[allow(clippy::too_many_arguments)]
fn measure(
	rendered: &[Vec<f32>],
	from: usize,
	map: &TimeMap,
	sample_rate: u32,
	seg: &Segmentation,
	edits: &Edits,
	opts: &VerifyOptions,
	include: impl Fn(usize) -> bool,
) -> Vec<Residual> {
	let channels: Vec<&[f32]> = rendered.iter().map(Vec::as_slice).collect();
	let Some(analysis) = analyze(&mono(&channels), sample_rate, &opts.analysis, &mut |_| true)
	else {
		return Vec::new();
	};
	let found = segment(&analysis, &opts.segment, &[]);
	let offset = from as f64 / f64::from(sample_rate.max(1));
	seg.notes
		.iter()
		.enumerate()
		.filter(|(i, _)| include(*i))
		.map(|(i, n)| {
			// Times of the part's own analysis.
			let (a, b) = (
				map.to_output(n.core_start) - offset,
				map.to_output(n.core_end) - offset,
			);
			// The note the render's segmentation found there, when its core covers most of
			// this one; else (a move can make a note look like its neighbour's fall) the
			// center measured straight over the core.
			let measured = found
				.notes
				.iter()
				.map(|f| (f.core_end.min(b) - f.core_start.max(a), f.center))
				.filter(|(overlap, _)| *overlap >= 0.5 * (b - a))
				.max_by(|x, y| x.0.total_cmp(&y.0))
				.map(|(_, c)| c)
				.or_else(|| center_over(&analysis, a, b, &opts.segment));
			Residual {
				note: i,
				target: edits.note(i).pitch.unwrap_or(n.center),
				measured,
			}
		})
		.collect()
}

/// Center of `start..end` seconds like the segmentation computes it: the mean of the
/// voiced frames with vibrato removed.
pub(crate) fn center_over(
	analysis: &Analysis,
	start: f64,
	end: f64,
	opts: &SegmentOptions,
) -> Option<f64> {
	let cents: Vec<f64> = analysis
		.frames
		.iter()
		.filter(|f| (start..=end).contains(&f.time))
		.filter_map(|f| f.pitch.map(|p| p * 100.0))
		.collect();
	if cents.is_empty() {
		return None;
	}
	let hop = analysis.hop_seconds();
	let smooth = vibrato::remove(&cents, &vibrato::detect(&cents, hop, &opts.vibrato), hop);
	Some(smooth.iter().sum::<f64>() / smooth.len() as f64 / 100.0)
}

/// [`render`] + [`verify`], re-rendering up to `opts.passes` more times with the measured
/// error taken out of the pitch-edited notes further than `opts.tolerance_cents` from
/// their target. Returns the render with the smallest worst error and its residuals
/// against the original targets.
pub fn render_verified(
	channels: &[&[f32]],
	analysis: &Analysis,
	seg: &Segmentation,
	edits: &Edits,
	opts: &VerifyOptions,
) -> Result<(Render, Vec<Residual>), NotesError> {
	check(channels, seg, edits)?;
	let input = mono(channels);
	let marks = pitch_marks(&input, analysis);
	let v = verified_with(channels, analysis, seg, &marks, edits, opts)?;
	Ok((v.render, v.residuals))
}

/// A verified render and the edits it followed, kept to re-render quickly after a local
/// change ([`rerender_verified`]).
#[derive(Debug, Clone, PartialEq)]
pub struct Verified {
	pub render: Render,
	/// Residuals of the notes measured (every note on a whole render, the changed ones on
	/// a re-render).
	pub residuals: Vec<Residual>,
	/// What the render followed: the asked edits with the closed loop's pitch corrections
	/// and the level fixes in (one entry per note).
	pub effective: Edits,
}

/// [`render_verified`] with the pitch marks of the mono mix already made, also returning
/// the edits it followed.
pub fn verified_with(
	channels: &[&[f32]],
	analysis: &Analysis,
	seg: &Segmentation,
	marks: &Marks,
	edits: &Edits,
	opts: &VerifyOptions,
) -> Result<Verified, NotesError> {
	check(channels, seg, edits)?;
	let input = mono(channels);
	let src = Source {
		channels,
		analysis,
		seg,
		marks,
		input: &input,
	};
	let rate = src.rate();
	let (part, residuals, effective) = closed_loop(
		seg,
		edits,
		opts,
		|e| render_full(&src, e),
		|part, e| measure(&part.channels, 0, &part.map, rate, seg, e, opts, |_| true),
	)?;
	Ok(Verified {
		render: Render {
			channels: part.channels,
			map: part.map,
		},
		residuals,
		effective,
	})
}

/// A render made earlier from the same audio and segmentation.
#[derive(Debug, Clone, Copy)]
pub struct Previous<'a> {
	pub channels: &'a [Vec<f32>],
	/// The edits it was asked for.
	pub asked: &'a Edits,
	/// The edits it followed ([`Verified::effective`]).
	pub effective: &'a Edits,
}

/// Re-renders `previous` for `edits` only where they change it: the notes whose asked
/// edits differ are rendered between the nearest points where the grains fall back on the
/// source grid (a run of unpitched marks), closed loop on those notes alone, and spliced
/// into the previous render; every other sample stays as it was (the same samples a whole
/// render gives). `None` when this does not apply (a time edit on either side, another
/// sibilant gain, a different length) or the change spans most of the take: render it
/// whole then.
pub fn rerender_verified(
	channels: &[&[f32]],
	analysis: &Analysis,
	seg: &Segmentation,
	marks: &Marks,
	previous: Previous,
	edits: &Edits,
	opts: &VerifyOptions,
) -> Result<Option<Verified>, NotesError> {
	check(channels, seg, edits)?;
	let len = channels.first().map_or(0, |c| c.len());
	if previous.channels.len() != channels.len()
		|| previous.channels.iter().any(|c| c.len() != len)
		|| previous.effective.notes.len() != seg.notes.len()
		|| previous.asked.sibilant_gain_db != edits.sibilant_gain_db
		|| !edit::time_map(seg, edits)?.is_identity()
		|| !edit::time_map(seg, previous.asked)?.is_identity()
	{
		return Ok(None);
	}
	// Unchanged notes keep what the previous render followed for them.
	let mut candidate = previous.effective.clone();
	candidate.sibilant_gain_db = edits.sibilant_gain_db;
	let mut changed = vec![false; seg.notes.len()];
	for (i, c) in changed.iter_mut().enumerate() {
		if edits.note(i) != previous.asked.note(i) {
			*c = true;
			candidate.notes[i] = edits.note(i);
		}
	}
	if !changed.contains(&true) {
		return Ok(Some(Verified {
			render: Render {
				channels: previous.channels.to_vec(),
				map: TimeMap::identity(),
			},
			residuals: Vec::new(),
			effective: previous.effective.clone(),
		}));
	}
	let input = mono(channels);
	let src = Source {
		channels,
		analysis,
		seg,
		marks,
		input: &input,
	};
	let rate = src.rate();
	let map = TimeMap::identity();
	let base_curves = edit::curves(analysis, seg, previous.effective);
	let base_plan = plan(rate, marks, &base_curves, &map, len);
	let around = |e: &Edits| -> Result<(Part, Edits), NotesError> {
		let window = |e: &Edits| {
			let curves = edit::curves(analysis, seg, e);
			let grains = plan(rate, marks, &curves, &map, len);
			let span = changed_window(&src, &base_curves, &base_plan, &curves, &grains, &changed);
			(curves, grains, span)
		};
		let draw = |curves: &edit::Curves, grains: &[Grain], (from, to): (usize, usize)| {
			psola_range(channels, rate, marks, curves, &map, grains, from..to)
		};
		let mut effective = e.clone();
		let (curves, grains, span) = window(&effective);
		let mut part = draw(&curves, &grains, span);
		let mut span = span;
		if let Some(leveled) = level_fix(&src, &effective, &part, span.0, &map, |i| changed[i]) {
			effective = leveled;
			let (curves, grains, wider) = window(&effective);
			span = (span.0.min(wider.0), span.1.max(wider.1));
			part = draw(&curves, &grains, span);
		}
		Ok((
			Part {
				from: span.0,
				channels: part,
				map: map.clone(),
			},
			effective,
		))
	};
	// A first look: a change over most of the take is a whole render.
	let first = edit::curves(analysis, seg, &candidate);
	let first_plan = plan(rate, marks, &first, &map, len);
	let (from, to) = changed_window(
		&src,
		&base_curves,
		&base_plan,
		&first,
		&first_plan,
		&changed,
	);
	if (to - from) as f64 > WHOLE_RENDER_SHARE * len as f64 {
		return Ok(None);
	}
	let (part, residuals, effective) = closed_loop(seg, &candidate, opts, around, |part, e| {
		measure(
			&part.channels,
			part.from,
			&part.map,
			rate,
			seg,
			e,
			opts,
			|i| changed[i],
		)
	})?;
	let mut out = previous.channels.to_vec();
	for (o, p) in out.iter_mut().zip(&part.channels) {
		o[part.from..part.from + p.len()].copy_from_slice(p);
	}
	Ok(Some(Verified {
		render: Render { channels: out, map },
		residuals,
		effective,
	}))
}

/// Above this share of the take a change is rendered whole.
const WHOLE_RENDER_SHARE: f64 = 0.6;

/// Samples (from, to) where a render following `curves` / `grains` can differ from one
/// following `base_curves` / `base_grains`: where the grains differ, where the curves
/// differ (gain is applied per sample) and over the `changed` notes, each widened by the
/// grains' reach.
fn changed_window(
	src: &Source,
	base_curves: &edit::Curves,
	base_grains: &[Grain],
	curves: &edit::Curves,
	grains: &[Grain],
	changed: &[bool],
) -> (usize, usize) {
	let rate = f64::from(src.rate());
	let reach = REACH_SECONDS * rate;
	let hop = src.analysis.hop_seconds() * rate;
	let mut lo = f64::INFINITY;
	let mut hi = f64::NEG_INFINITY;
	let mut take = |a: f64, b: f64| {
		lo = lo.min(a);
		hi = hi.max(b);
	};
	if let Some((a, b)) = changed_span(src.rate(), base_grains, grains) {
		take(a, b);
	}
	for (k, f) in src.analysis.frames.iter().enumerate() {
		let differs = |x: &[f64], y: &[f64]| (x[k] - y[k]).abs() > 1e-12;
		if differs(&base_curves.shift, &curves.shift)
			|| differs(&base_curves.formant, &curves.formant)
			|| differs(&base_curves.gain, &curves.gain)
		{
			let at = f.time * rate;
			take(at - hop - reach, at + hop + reach);
		}
	}
	for (n, _) in src.seg.notes.iter().zip(changed).filter(|(_, c)| **c) {
		take(n.onset * rate - reach, n.offset * rate + reach);
	}
	let len = src.len();
	if lo > hi {
		return (0, 0);
	}
	(
		(lo.floor().max(0.0) as usize).min(len),
		(hi.ceil().max(0.0) as usize).min(len),
	)
}

/// The closed loop shared by whole renders and re-renders: `render` the edits, `measure`
/// the notes, take each pitch-edited note's error out of what is asked and render again,
/// up to `opts.passes` more times; the pass with the smallest worst error wins. Returns
/// its part, its residuals against the original targets and the edits it followed.
fn closed_loop(
	seg: &Segmentation,
	edits: &Edits,
	opts: &VerifyOptions,
	mut render: impl FnMut(&Edits) -> Result<(Part, Edits), NotesError>,
	mut measure: impl FnMut(&Part, &Edits) -> Vec<Residual>,
) -> Result<(Part, Vec<Residual>, Edits), NotesError> {
	let worst = |rs: &[Residual]| {
		rs.iter()
			.filter_map(Residual::cents)
			.fold(0.0f64, |m, c| m.max(c.abs()))
	};
	let mut current = edits.clone();
	current.notes.resize(seg.notes.len(), Default::default());
	let (part, effective) = render(&current)?;
	let residuals = measure(&part, &current);
	let mut last = residuals.clone();
	let mut best = (part, residuals, effective);
	for _ in 0..opts.passes {
		if opts.tolerance_cents <= 0.0 {
			break;
		}
		let mut again = false;
		for r in &last {
			let (Some(cents), Some(pitch)) = (r.cents(), current.notes[r.note].pitch) else {
				continue;
			};
			if cents.abs() > opts.tolerance_cents {
				let n = &seg.notes[r.note];
				current.notes[r.note].pitch = Some(
					(pitch - cents / 100.0)
						.clamp(n.center - edit::MAX_SHIFT, n.center + edit::MAX_SHIFT),
				);
				again = true;
			}
		}
		if !again {
			break;
		}
		let (part, effective) = render(&current)?;
		let mut after = measure(&part, &current);
		for (r, original) in after.iter_mut().zip(&best.1) {
			r.target = original.target;
		}
		// Each pass takes the error against the original target out of what it asked
		// (an integrating loop), so the next one starts from `after`.
		last = after.clone();
		if worst(&after) < worst(&best.1) {
			best = (part, after, effective);
		}
	}
	Ok(best)
}

/// File seconds `from..to` of `edits` rendered quickly to audition a note while it is
/// dragged: open loop, no level fix, time edits left out (pitch, formant, gain and
/// modulation only). Milliseconds for a note on a long take.
pub fn audition(
	channels: &[&[f32]],
	analysis: &Analysis,
	seg: &Segmentation,
	marks: &Marks,
	edits: &Edits,
	from: f64,
	to: f64,
) -> Result<Vec<Vec<f32>>, NotesError> {
	let mut edits = edits.clone();
	for n in &mut edits.notes {
		n.time = None;
	}
	check(channels, seg, &edits)?;
	let len = channels.first().map_or(0, |c| c.len());
	let rate = analysis.sample_rate;
	let r = f64::from(rate);
	let a = ((from.max(0.0) * r) as usize).min(len);
	let b = ((to.max(0.0) * r) as usize).clamp(a, len);
	let curves = edit::curves(analysis, seg, &edits);
	let map = TimeMap::identity();
	let grains = plan(rate, marks, &curves, &map, len);
	Ok(psola_range(
		channels,
		rate,
		marks,
		&curves,
		&map,
		&grains,
		a..b,
	))
}
