//! NE-4 acceptance for the resynthesis: null render, corrected notes within ±5 cents on the
//! corpus (closed loop), levels, formants, time edits and no clicks.

use cadenza_notes::analysis::{Analysis, AnalysisOptions, analyze};
use cadenza_notes::edit::{self, Edits, NoteEdit, TimeEdit};
use cadenza_notes::eval::synth;
use cadenza_notes::marks::pitch_marks;
use cadenza_notes::psola::psola;
use cadenza_notes::resynth::{self, Residual, VerifyOptions};
use cadenza_notes::segment::{SegmentOptions, Segmentation, segment};
use cadenza_notes::timemap::TimeMap;

fn prepare(samples: &[f32], rate: u32) -> (Analysis, Segmentation) {
	let a = analyze(samples, rate, &AnalysisOptions::default(), &mut |_| true).unwrap();
	let seg = segment(&a, &SegmentOptions::default(), &[]);
	(a, seg)
}

/// Pitch moves the corpus test asks for, cycled over the notes (semitones from the nearest
/// semitone of the sung center).
const MOVES: [f64; 8] = [0.0, 2.0, -1.0, 3.0, 0.0, -3.0, 1.0, -2.0];

fn moved(seg: &Segmentation) -> Edits {
	Edits {
		notes: seg
			.notes
			.iter()
			.enumerate()
			.map(|(i, n)| NoteEdit {
				pitch: Some(n.center.round() + MOVES[i % MOVES.len()]),
				..NoteEdit::default()
			})
			.collect(),
		sibilant_gain_db: 0.0,
	}
}

fn rms_db(x: &[f32]) -> f64 {
	let e = x.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() / x.len().max(1) as f64;
	10.0 * e.max(1e-30).log10()
}

#[test]
fn an_unchanged_edit_state_renders_the_input() {
	let case = &synth::corpus(22_050)[4];
	let (a, seg) = prepare(&case.samples, case.sample_rate);
	// Through PSOLA itself with flat curves: grains laid back at their marks sum exactly.
	let marks = pitch_marks(&case.samples, &a);
	let out = psola(
		&[&case.samples],
		case.sample_rate,
		&marks,
		&edit::Curves::flat(&a),
		&TimeMap::identity(),
	);
	let diff: Vec<f32> = out[0]
		.iter()
		.zip(&case.samples)
		.map(|(y, x)| y - x)
		.collect();
	let residual = rms_db(&diff) - rms_db(&case.samples);
	assert!(residual < -80.0, "{residual} dB");
	// And an edit that changes nothing never resynthesizes.
	let r = resynth::render(&[&case.samples], &a, &seg, &Edits::default()).unwrap();
	assert_eq!(r.channels[0], case.samples);
}

fn report(name: &str, residuals: &[Residual]) -> (usize, f64) {
	let mut worst = 0.0f64;
	let mut missing = 0;
	for r in residuals {
		match r.cents() {
			Some(c) => worst = worst.max(c.abs()),
			None => missing += 1,
		}
	}
	eprintln!(
		"{name}: {} notes, worst {worst:.1} c, {missing} not found",
		residuals.len()
	);
	(missing, worst)
}

#[test]
fn corrected_notes_land_within_five_cents() {
	let opts = VerifyOptions {
		tolerance_cents: 2.0,
		..VerifyOptions::default()
	};
	let mut failures = Vec::new();
	for case in synth::corpus(22_050) {
		let (a, seg) = prepare(&case.samples, case.sample_rate);
		let edits = moved(&seg);
		let (_, residuals) =
			resynth::render_verified(&[&case.samples], &a, &seg, &edits, &opts).unwrap();
		let (missing, worst) = report(&case.name, &residuals);
		if missing > 0 || worst > 5.0 {
			failures.push(format!(
				"{}: worst {worst:.1} c, {missing} missing",
				case.name
			));
		}
	}
	assert!(failures.is_empty(), "{failures:#?}");
}

/// 1 ms blocks whose second-difference energy is 4x that of every block 3..25 ms away: a
/// one-off burst (a glottal pulse repeats within the window, a click does not). Seconds.
fn clicks(x: &[f32], rate: u32) -> Vec<f64> {
	let block = (f64::from(rate) * 0.001) as usize;
	let e: Vec<f64> = x
		.chunks(block)
		.map(|c| {
			c.windows(3)
				.map(|w| f64::from(w[2] - 2.0 * w[1] + w[0]).powi(2))
				.sum()
		})
		.collect();
	let peak = e.iter().copied().fold(0.0, f64::max);
	(0..e.len())
		.filter(|&i| {
			let around = (i.saturating_sub(25)..(i + 26).min(e.len()))
				.filter(|j| i.abs_diff(*j) >= 3)
				.map(|j| e[j])
				.fold(0.0, f64::max);
			e[i] >= peak * 1e-4 && e[i] > 4.0 * around
		})
		.map(|i| i as f64 / 1000.0)
		.collect()
}

#[test]
fn moved_notes_add_no_clicks() {
	for case in synth::corpus(22_050) {
		let (a, seg) = prepare(&case.samples, case.sample_rate);
		let before = clicks(&case.samples, case.sample_rate);
		// Big steps both ways between neighbours, the hardest borders.
		let edits = Edits {
			notes: seg
				.notes
				.iter()
				.enumerate()
				.map(|(i, n)| NoteEdit {
					pitch: Some(n.center + if i % 2 == 0 { 7.0 } else { -3.5 }),
					..NoteEdit::default()
				})
				.collect(),
			sibilant_gain_db: -6.0,
		};
		let r = resynth::render(&[&case.samples], &a, &seg, &edits).unwrap();
		let new: Vec<f64> = clicks(&r.channels[0], case.sample_rate)
			.into_iter()
			.filter(|t| !before.iter().any(|b| (b - t).abs() < 0.004))
			.collect();
		assert!(new.is_empty(), "{}: clicks at {new:?}", case.name);
	}
	// The detector does see a click.
	let case = &synth::corpus(22_050)[1];
	let mut clicked = case.samples.clone();
	clicked[30_000] += 0.3;
	assert_eq!(clicks(&clicked, case.sample_rate).len(), 1);
}

#[test]
fn moved_notes_keep_their_level() {
	let case = &synth::corpus(22_050)[7];
	let (a, seg) = prepare(&case.samples, case.sample_rate);
	let edits = moved(&seg);
	let r = resynth::render(&[&case.samples], &a, &seg, &edits).unwrap();
	let rate = f64::from(case.sample_rate);
	for n in &seg.notes {
		let (s, e) = ((n.core_start * rate) as usize, (n.core_end * rate) as usize);
		let d = rms_db(&r.channels[0][s..e]) - rms_db(&case.samples[s..e]);
		assert!(d.abs() < 0.5, "{:.2} s: {d:+.2} dB", n.onset);
	}
}

/// LPC envelope (order 18, autocorrelation + Levinson) of a 40 ms Hann window at `at`
/// seconds, dB at `freqs`, mean removed.
fn envelope(x: &[f32], rate: u32, at: f64, freqs: &[f64]) -> Vec<f64> {
	let n = (0.04 * f64::from(rate)) as usize;
	let start = (at * f64::from(rate)) as usize - n / 2;
	let w: Vec<f64> = (0..n)
		.map(|i| {
			let h = 0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / n as f64).cos();
			f64::from(x[start + i]) * h
		})
		.collect();
	let order = 18;
	let r: Vec<f64> = (0..=order)
		.map(|k| (k..n).map(|i| w[i] * w[i - k]).sum::<f64>())
		.collect();
	let mut a = vec![0.0; order + 1];
	a[0] = 1.0;
	let mut err = r[0] * (1.0 + 1e-9);
	for i in 1..=order {
		let k = -(0..i).map(|j| a[j] * r[i - j]).sum::<f64>() / err;
		let prev = a.clone();
		for j in 1..i {
			a[j] = prev[j] + k * prev[i - j];
		}
		a[i] = k;
		err *= 1.0 - k * k;
	}
	let db: Vec<f64> = freqs
		.iter()
		.map(|f| {
			let om = std::f64::consts::TAU * f / f64::from(rate);
			let (re, im) = a.iter().enumerate().fold((0.0, 0.0), |(re, im), (k, c)| {
				(
					re + c * (om * k as f64).cos(),
					im - c * (om * k as f64).sin(),
				)
			});
			-10.0 * (re * re + im * im).log10()
		})
		.collect();
	let mean = db.iter().sum::<f64>() / db.len() as f64;
	db.iter().map(|d| d - mean).collect()
}

fn distance(a: &[f64], b: &[f64]) -> f64 {
	a.iter().zip(b).map(|(x, y)| (x - y).abs()).sum::<f64>() / a.len() as f64
}

#[test]
fn pitch_moves_keep_the_formants_and_formant_moves_shift_them() {
	let case = &synth::corpus(22_050)[1];
	let rate = case.sample_rate;
	let (a, seg) = prepare(&case.samples, rate);
	let freqs: Vec<f64> = (0..60).map(|i| 250.0 * 1.05f64.powi(i)).collect();
	let shifted = |pitch: f64, formant_cents: f64| {
		let edits = Edits {
			notes: seg
				.notes
				.iter()
				.map(|n| NoteEdit {
					pitch: Some(n.center + pitch),
					formant_cents,
					..NoteEdit::default()
				})
				.collect(),
			sibilant_gain_db: 0.0,
		};
		resynth::render(&[&case.samples], &a, &seg, &edits)
			.unwrap()
			.channels[0]
			.clone()
	};
	let up = shifted(4.0, 0.0);
	let formant = shifted(0.0, 300.0);
	let mut kept = Vec::new();
	let mut naive = Vec::new();
	let mut warps = Vec::new();
	for n in seg
		.notes
		.iter()
		.filter(|n| n.core_end - n.core_start > 0.15)
	{
		let at = 0.5 * (n.core_start + n.core_end);
		let original = envelope(&case.samples, rate, at, &freqs);
		kept.push(distance(&envelope(&up, rate, at, &freqs), &original));
		// What a plain resampling (formants moved with the pitch) would give.
		let moved: Vec<f64> = freqs.iter().map(|f| f / 2f64.powf(4.0 / 12.0)).collect();
		naive.push(distance(
			&envelope(&case.samples, rate, at, &moved),
			&original,
		));
		// The factor that best maps the original envelope onto the formant-shifted one.
		let got = envelope(&formant, rate, at, &freqs);
		let best = (0..=60)
			.map(|i| 0.85 + i as f64 * 0.01)
			.min_by(|x, y| {
				let warp = |f: f64| freqs.iter().map(|q| q / f).collect::<Vec<_>>();
				let dx = distance(&envelope(&case.samples, rate, at, &warp(*x)), &got);
				let dy = distance(&envelope(&case.samples, rate, at, &warp(*y)), &got);
				dx.total_cmp(&dy)
			})
			.unwrap();
		warps.push(best);
	}
	let mean = |v: &[f64]| v.iter().sum::<f64>() / v.len() as f64;
	eprintln!(
		"envelope distance kept {:.2} dB vs resampled {:.2} dB, formant warp {:?}",
		mean(&kept),
		mean(&naive),
		warps
	);
	assert!(mean(&kept) < 0.5 * mean(&naive), "{kept:?} vs {naive:?}");
	let wanted = 2f64.powf(300.0 / 1200.0);
	assert!(
		(mean(&warps) - wanted).abs() < 0.04,
		"{warps:?} vs {wanted}"
	);
}

#[test]
fn corrected_notes_land_within_five_cents_at_44k() {
	for case in synth::corpus(44_100).into_iter().step_by(3) {
		let (a, seg) = prepare(&case.samples, case.sample_rate);
		let (_, residuals) = resynth::render_verified(
			&[&case.samples],
			&a,
			&seg,
			&moved(&seg),
			&VerifyOptions::default(),
		)
		.unwrap();
		let (missing, worst) = report(&case.name, &residuals);
		assert!(missing == 0 && worst <= 5.0, "{}: {worst:.1} c", case.name);
	}
}

#[test]
fn moved_and_stretched_notes_land_where_asked() {
	let case = &synth::corpus(22_050)[1];
	let (a, seg) = prepare(&case.samples, case.sample_rate);
	let i = 3;
	let (n, next) = (&seg.notes[i], &seg.notes[i + 1]);
	let room = next.onset - n.offset;
	let wanted = TimeEdit {
		onset: n.onset + 0.25 * room,
		offset: n.offset + 0.75 * room,
	};
	let mut edits = Edits::default();
	edits.notes.resize(i + 1, NoteEdit::default());
	edits.notes[i].time = Some(wanted);
	let r = resynth::render(&[&case.samples], &a, &seg, &edits).unwrap();
	let (_, found) = prepare(&r.channels[0], case.sample_rate);
	let moved = found
		.notes
		.iter()
		.find(|f| (f.center - n.center).abs() < 0.3 && (f.onset - wanted.onset).abs() < 0.05)
		.unwrap_or_else(|| panic!("{:?} not in {:#?}", wanted, found.notes));
	assert!(
		(moved.onset - wanted.onset).abs() < 0.02,
		"{moved:?} vs {wanted:?}"
	);
	assert!(
		(moved.offset - wanted.offset).abs() < 0.03,
		"{moved:?} vs {wanted:?}"
	);
	assert!(
		(moved.center - n.center).abs() * 100.0 < 5.0,
		"{moved:?} vs {n:?}"
	);
	// Its neighbours did not move.
	for k in [i - 1, i + 1] {
		let m = &seg.notes[k];
		assert!(
			found
				.notes
				.iter()
				.any(|f| (f.onset - m.onset).abs() < 0.01
					&& (f.center - m.center).abs() * 100.0 < 3.0),
			"{m:?}"
		);
	}
	// Edits that would cross a neighbour are refused with a reason.
	edits.notes[i].time = Some(TimeEdit {
		onset: n.onset,
		offset: next.offset,
	});
	let err = resynth::render(&[&case.samples], &a, &seg, &edits).unwrap_err();
	assert!(err.to_string().contains("neighbour"), "{err}");
}

#[test]
fn stereo_channels_get_the_same_grains() {
	let case = &synth::corpus(22_050)[2];
	let right: Vec<f32> = case.samples.iter().map(|x| 0.25 * x).collect();
	// The analysis is of the mono mix.
	let (a, seg) = prepare(&resynth::mono(&[&case.samples, &right]), case.sample_rate);
	let r = resynth::render(&[&case.samples, &right], &a, &seg, &moved(&seg)).unwrap();
	assert_eq!(r.channels.len(), 2);
	for (l, rr) in r.channels[0].iter().zip(&r.channels[1]) {
		assert!((rr - 0.25 * l).abs() < 1e-6);
	}
}

#[test]
fn sibilant_gain_touches_only_the_sibilants() {
	let case = synth::corpus(22_050)
		.into_iter()
		.find(|c| c.name == "repeated")
		.unwrap();
	let (a, seg) = prepare(&case.samples, case.sample_rate);
	assert!(!seg.sibilants.is_empty());
	let edits = Edits {
		notes: Vec::new(),
		sibilant_gain_db: -12.0,
	};
	let r = resynth::render(&[&case.samples], &a, &seg, &edits).unwrap();
	let rate = f64::from(case.sample_rate);
	let range = |x: &[f32], s: f64, e: f64| rms_db(&x[(s * rate) as usize..(e * rate) as usize]);
	for s in &seg.sibilants {
		// The middle of each sibilant (its edges ramp over one frame).
		let (from, to) = (s.start + 0.006, s.end - 0.006);
		if to - from < 0.01 {
			continue;
		}
		let d = range(&r.channels[0], from, to) - range(&case.samples, from, to);
		assert!((d + 12.0).abs() < 1.0, "{s:?}: {d:+.1} dB");
	}
	for n in &seg.notes {
		let d = range(&r.channels[0], n.core_start, n.core_end)
			- range(&case.samples, n.core_start, n.core_end);
		assert!(d.abs() < 0.1, "{n:?}: {d:+.2} dB");
	}
}

#[test]
fn flattened_modulation_removes_the_vibrato() {
	let case = synth::corpus(22_050)
		.into_iter()
		.find(|c| c.name == "vibrato")
		.unwrap();
	let (a, seg) = prepare(&case.samples, case.sample_rate);
	let edits = Edits {
		notes: seg
			.notes
			.iter()
			.map(|_| NoteEdit {
				modulation: 0.0,
				..NoteEdit::default()
			})
			.collect(),
		sibilant_gain_db: 0.0,
	};
	let r = resynth::render(&[&case.samples], &a, &seg, &edits).unwrap();
	let (_, after) = prepare(&r.channels[0], case.sample_rate);
	let depth = |s: &Segmentation| {
		s.notes
			.iter()
			.map(|n| n.vibrato.map_or(0.0, |v| v.depth_cents))
			.sum::<f64>()
	};
	let (before, flat) = (depth(&seg), depth(&after));
	eprintln!("vibrato depth sum {before:.0} c -> {flat:.0} c");
	assert!(before > 100.0 && flat < 0.15 * before, "{before} -> {flat}");
	// And the notes are still where they were.
	for n in &seg.notes {
		assert!(
			after
				.notes
				.iter()
				.any(|m| (m.center - n.center).abs() * 100.0 < 5.0
					&& m.core_end > n.core_start
					&& m.core_start < n.core_end),
			"{n:?}"
		);
	}
}

#[test]
fn an_octave_either_way_stays_in_tune() {
	let mut all = Vec::new();
	for case in synth::corpus(22_050).into_iter().filter(|c| {
		[
			"steady_mid",
			"legato_glide_long",
			"vibrato",
			"expressive_mid",
		]
		.contains(&c.name.as_str())
	}) {
		let (a, seg) = prepare(&case.samples, case.sample_rate);
		for shift in [-12.0, -7.0, 7.0, 12.0] {
			let edits = Edits {
				notes: seg
					.notes
					.iter()
					.map(|n| NoteEdit {
						pitch: Some(n.center.round() + shift),
						..NoteEdit::default()
					})
					.collect(),
				sibilant_gain_db: 0.0,
			};
			let (_, residuals) = resynth::render_verified(
				&[&case.samples],
				&a,
				&seg,
				&edits,
				&VerifyOptions::default(),
			)
			.unwrap();
			for r in residuals {
				let c = r
					.cents()
					.unwrap_or_else(|| panic!("{} {shift:+}: note {} lost", case.name, r.note));
				all.push(c.abs());
			}
		}
	}
	all.sort_by(f64::total_cmp);
	let p95 = all[(all.len() - 1) * 95 / 100];
	let max = all[all.len() - 1];
	assert!(p95 <= 3.0 && max <= 8.0, "p95 {p95:.1} c, max {max:.1} c");
}

#[test]
fn a_noisy_take_stays_voiced_when_moved() {
	let case = synth::corpus(22_050)
		.into_iter()
		.find(|c| c.name == "noisy_snr10")
		.unwrap();
	let (a, seg) = prepare(&case.samples, case.sample_rate);
	let voiced = |a: &Analysis| a.frames.iter().filter(|f| f.pitch.is_some()).count();
	let edits = Edits {
		notes: seg
			.notes
			.iter()
			.map(|n| NoteEdit {
				pitch: Some(n.center + 7.0),
				..NoteEdit::default()
			})
			.collect(),
		sibilant_gain_db: 0.0,
	};
	let r = resynth::render(&[&case.samples], &a, &seg, &edits).unwrap();
	let (b, _) = prepare(&r.channels[0], case.sample_rate);
	let kept = voiced(&b) as f64 / voiced(&a) as f64;
	assert!(kept >= 0.75, "{kept:.2} of the voiced frames kept");
}
