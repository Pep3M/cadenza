//! Resynthesis of an edited recording (NE-4): pitch marks + edit curves + time map ->
//! PSOLA, then a closed loop that measures every note of the render with the same
//! analysis and segmentation and re-renders once with the residuals taken out.

use serde::{Deserialize, Serialize};

use crate::NotesError;
use crate::analysis::{Analysis, AnalysisOptions, analyze};
use crate::edit::{self, Edits};
use crate::marks::pitch_marks;
use crate::psola::psola;
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
	if channels.iter().any(|c| c.len() != channels[0].len()) {
		return Err(NotesError::Edit(
			"every channel must have the same length".into(),
		));
	}
	edits.validate(seg)?;
	let map = edit::time_map(seg, edits)?;
	if edits.is_unchanged() {
		return Ok(Render {
			channels: channels.iter().map(|c| c.to_vec()).collect(),
			map,
		});
	}
	let input = mono(channels);
	let marks = pitch_marks(&input, analysis);
	let rate = analysis.sample_rate;
	let rendered = psola(
		channels,
		rate,
		&marks,
		&edit::curves(analysis, seg, edits),
		&map,
	);
	// Re-spaced grains sample the spectrum between the original harmonics, so a moved note
	// comes out up to a few dB louder or softer: bring every reshaped note back to its own
	// level (plus its gain edit).
	let out: Vec<&[f32]> = rendered.iter().map(Vec::as_slice).collect();
	let output = mono(&out);
	let mut leveled = edits.clone();
	leveled.notes.resize(edits.notes.len(), Default::default());
	let mut changed = false;
	for (i, (e, n)) in leveled.notes.iter_mut().zip(&seg.notes).enumerate() {
		let reshaped = e.pitch.is_some_and(|p| p != n.center)
			|| e.formant_cents != 0.0
			|| e.drift != 1.0
			|| e.modulation != 1.0;
		if !reshaped {
			continue;
		}
		let span = |x: &[f32], a: f64, b: f64| {
			let r = f64::from(rate);
			let (a, b) = ((a * r) as usize, ((b * r) as usize).min(x.len()));
			rms_db(&x[a.min(b)..b])
		};
		let before = span(&input, n.core_start, n.core_end);
		let after = span(
			&output,
			map.to_output(n.core_start),
			map.to_output(n.core_end),
		);
		if before > -100.0 && after > -100.0 {
			let fix = (before - after).clamp(-LEVEL_FIX_DB, LEVEL_FIX_DB);
			if fix.abs() > 0.05 {
				e.gain_db = (edits.note(i).gain_db + fix).clamp(-60.0, 24.0);
				changed = true;
			}
		}
	}
	let channels = if changed {
		psola(
			channels,
			rate,
			&marks,
			&edit::curves(analysis, seg, &leveled),
			&map,
		)
	} else {
		rendered
	};
	Ok(Render { channels, map })
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
	let channels: Vec<&[f32]> = render.channels.iter().map(Vec::as_slice).collect();
	let Some(analysis) = analyze(&mono(&channels), sample_rate, &opts.analysis, &mut |_| true)
	else {
		return Vec::new();
	};
	let found = segment(&analysis, &opts.segment, &[]);
	seg.notes
		.iter()
		.enumerate()
		.map(|(i, n)| {
			let (a, b) = (
				render.map.to_output(n.core_start),
				render.map.to_output(n.core_end),
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
	let rate = analysis.sample_rate;
	let worst = |rs: &[Residual]| {
		rs.iter()
			.filter_map(Residual::cents)
			.fold(0.0f64, |m, c| m.max(c.abs()))
	};
	let first = render(channels, analysis, seg, edits)?;
	let residuals = verify(&first, rate, seg, edits, opts);
	let mut best = (first, residuals.clone());
	let mut current = edits.clone();
	current.notes.resize(seg.notes.len(), Default::default());
	let mut last = residuals;
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
		let render = render(channels, analysis, seg, &current)?;
		let mut after = verify(&render, rate, seg, &current, opts);
		for (r, original) in after.iter_mut().zip(&best.1) {
			r.target = original.target;
		}
		// Each pass takes the error against the original target out of what it asked
		// (an integrating loop), so the next one starts from `after`.
		last = after.clone();
		if worst(&after) < worst(&best.1) {
			best = (render, after);
		}
	}
	Ok(best)
}
