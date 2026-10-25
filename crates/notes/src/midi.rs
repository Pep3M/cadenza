//! Segmented notes -> MIDI notes (NE-3): one pitch per note from its core center, timing in
//! song beats through the tempo map, optional grid quantize, key snap (given or inferred),
//! register fold, velocity from the note's level and an optional pitch-bend lane that
//! replays the sung curve around each MIDI note.

use cadenza_core::TempoMap;
use cadenza_theory::Scale;

use crate::analysis::Analysis;
use crate::segment::{Segmentation, SegmentedNote};

/// What to do with pitches outside a key.
#[derive(Debug, Clone, PartialEq)]
pub enum KeySnap {
	/// Nearest semitone to the sung center.
	Off,
	/// Infer the key from the notes (Krumhansl-Kessler) and snap to it.
	Auto,
	Scale(Scale),
}

#[derive(Debug, Clone, PartialEq)]
pub struct MidiOptions {
	/// Grid in beats (0.25 = sixteenths); `None` keeps the sung timing.
	pub quantize: Option<f64>,
	/// 0..1: how far starts and ends move toward the grid.
	pub quantize_strength: f64,
	pub key: KeySnap,
	/// Write a pitch-bend lane that follows the sung curve around each note.
	pub bends: bool,
	/// Semitones of a full bend (the instrument's bend range).
	pub bend_range: f64,
	/// Notes shorter than this are dropped, seconds.
	pub min_length: f64,
	/// Same-pitch notes less than 50 ms apart become one (sloppy humming).
	pub merge_repeats: bool,
	pub octave_shift: i32,
	/// Fold pitches by octaves into this range (inclusive).
	pub range: Option<(u8, u8)>,
	/// Fixed velocity 0..1; `None` = from each note's level.
	pub velocity: Option<f32>,
	/// Only notes mostly inside this span of the analysis (seconds), clipped to it.
	pub region: Option<(f64, f64)>,
}

impl Default for MidiOptions {
	/// Faithful audio-to-MIDI: sung timing, nearest semitone, no bends.
	fn default() -> Self {
		Self {
			quantize: None,
			quantize_strength: 1.0,
			key: KeySnap::Off,
			bends: false,
			bend_range: 2.0,
			min_length: 0.03,
			merge_repeats: false,
			octave_shift: 0,
			range: None,
			velocity: None,
			region: None,
		}
	}
}

impl MidiOptions {
	/// Hum-to-MIDI: sixteenth grid, inferred key, repeats merged, very short notes dropped.
	pub fn hum() -> Self {
		Self {
			quantize: Some(0.25),
			key: KeySnap::Auto,
			min_length: 0.08,
			merge_repeats: true,
			..Self::default()
		}
	}
}

#[derive(Debug, Clone, PartialEq)]
pub struct MidiNote {
	/// Song beats.
	pub start: f64,
	pub length: f64,
	pub pitch: u8,
	/// 0..1.
	pub velocity: f32,
	/// Sung center minus `pitch`, cents (before any octave shift or fold).
	pub cents: f64,
}

/// One pitch-bend event (MIDI semantics: holds until the next).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BendPoint {
	pub beat: f64,
	/// -1..1 of `bend_range`.
	pub value: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MidiConversion {
	pub notes: Vec<MidiNote>,
	/// Empty unless `bends`.
	pub bends: Vec<BendPoint>,
	/// The key snapped to (given or inferred).
	pub key: Option<Scale>,
	/// Notes left out: shorter than `min_length`, or lost to an overlap after quantizing.
	pub dropped: usize,
}

/// Converts a segmentation. `origin` = song seconds at analysis time 0 (an audio clip's
/// start minus its file offset).
pub fn to_midi(
	seg: &Segmentation,
	analysis: &Analysis,
	tempo: &TempoMap,
	origin: f64,
	opts: &MidiOptions,
) -> MidiConversion {
	let mut dropped = 0;
	// Notes in the region, clipped to it.
	let mut picked: Vec<SegmentedNote> = Vec::new();
	for n in &seg.notes {
		let mut n = n.clone();
		if let Some((a, b)) = opts.region {
			let inside = n.offset.min(b) - n.onset.max(a);
			if inside * 2.0 < n.offset - n.onset {
				continue;
			}
			n.onset = n.onset.max(a);
			n.offset = n.offset.min(b);
		}
		if n.offset - n.onset < opts.min_length {
			dropped += 1;
			continue;
		}
		picked.push(n);
	}
	if opts.merge_repeats {
		let mut merged: Vec<SegmentedNote> = Vec::with_capacity(picked.len());
		for n in picked {
			if let Some(last) = merged.last_mut()
				&& n.onset - last.offset < 0.05
				&& (n.center - last.center).abs() < 0.5
			{
				let (a, b) = (last.offset - last.onset, n.offset - n.onset);
				last.center = (last.center * a + n.center * b) / (a + b);
				last.offset = n.offset;
				last.core_end = n.core_end;
				continue;
			}
			merged.push(n);
		}
		picked = merged;
	}
	let key = match &opts.key {
		KeySnap::Off => None,
		KeySnap::Scale(s) => Some(*s),
		KeySnap::Auto => {
			let mut hist = [0.0f64; 12];
			for n in &picked {
				hist[(n.center.round() as i64).rem_euclid(12) as usize] += n.offset - n.onset;
			}
			Scale::infer(&hist)
		}
	};
	let levels: Vec<f64> = picked.iter().map(|n| level_of(analysis, n)).collect();
	let loudest = levels.iter().copied().fold(f64::NEG_INFINITY, f64::max);
	let beat = |seconds: f64| tempo.seconds_to_tick((origin + seconds).max(0.0)).beats();
	let snap = |b: f64| match opts.quantize {
		Some(grid) if grid > 0.0 => {
			b + opts.quantize_strength.clamp(0.0, 1.0) * ((b / grid).round() * grid - b)
		}
		_ => b,
	};
	struct Placed {
		note: MidiNote,
		/// The semitone the bends are measured from (before octave shift and fold).
		base: f64,
		/// Analysis seconds of the original span, for the bends.
		onset: f64,
		offset: f64,
	}
	let mut placed: Vec<Placed> = Vec::with_capacity(picked.len());
	for (n, level) in picked.iter().zip(&levels) {
		let sung = n.center;
		let base = match &key {
			Some(k) => nearest_in_key(k, sung),
			None => sung.round().clamp(0.0, 127.0) as u8,
		};
		let mut pitch = i32::from(base) + 12 * opts.octave_shift;
		if let Some((low, high)) = opts.range {
			let (low, high) = (i32::from(low.min(high)), i32::from(low.max(high)));
			while pitch < low && pitch + 12 <= 127 {
				pitch += 12;
			}
			while pitch > high && pitch - 12 >= 0 && pitch - 12 >= low.min(high - 11) {
				pitch -= 12;
			}
		}
		let (start, mut end) = (snap(beat(n.onset)), snap(beat(n.offset)));
		if end <= start {
			end = start
				+ opts
					.quantize
					.filter(|g| *g > 0.0)
					.unwrap_or(beat(n.offset) - beat(n.onset))
					.max(1e-3);
		}
		let velocity = opts
			.velocity
			.unwrap_or_else(|| (0.95 + (level - loudest) / 40.0).clamp(0.25, 1.0) as f32);
		placed.push(Placed {
			note: MidiNote {
				start,
				length: end - start,
				pitch: pitch.clamp(0, 127) as u8,
				velocity,
				cents: (sung - f64::from(base)) * 100.0,
			},
			base: f64::from(base),
			onset: n.onset,
			offset: n.offset,
		});
	}
	// Mono line: a note ends where the next starts; one squeezed to nothing loses to the
	// longer of the two.
	placed.sort_by(|a, b| a.note.start.total_cmp(&b.note.start));
	let mut kept: Vec<Placed> = Vec::with_capacity(placed.len());
	for p in placed {
		if let Some(last) = kept.last_mut()
			&& p.note.start < last.note.start + last.note.length
		{
			if p.note.start - last.note.start < 1e-6 {
				dropped += 1;
				if p.note.length > last.note.length {
					*last = p;
				}
				continue;
			}
			last.note.length = p.note.start - last.note.start;
		}
		kept.push(p);
	}
	let mut bends = Vec::new();
	if opts.bends && opts.bend_range > 0.0 {
		let mut last: Option<f32> = None;
		let step = (3.0 / 100.0 / opts.bend_range) as f32;
		for (i, p) in kept.iter().enumerate() {
			let first = analysis.frame_at(p.onset).unwrap_or(0);
			let end = analysis.frame_at(p.offset).unwrap_or(0);
			let to_beat = |t: f64| {
				let u = ((t - p.onset) / (p.offset - p.onset).max(1e-9)).clamp(0.0, 1.0);
				p.note.start + u * p.note.length
			};
			let mut wrote = false;
			for f in &analysis.frames[first..end.max(first)] {
				let Some(sung) = f.pitch else { continue };
				let value = ((sung - p.base) / opts.bend_range).clamp(-1.0, 1.0) as f32;
				if !wrote || last.is_none_or(|l| (l - value).abs() >= step) {
					bends.push(BendPoint {
						beat: if wrote { to_beat(f.time) } else { p.note.start },
						value,
					});
					last = Some(value);
					wrote = true;
				}
			}
			let end_beat = p.note.start + p.note.length;
			let next_starts = kept
				.get(i + 1)
				.is_some_and(|q| q.note.start <= end_beat + 1e-6);
			if wrote && !next_starts && last != Some(0.0) {
				bends.push(BendPoint {
					beat: end_beat,
					value: 0.0,
				});
				last = Some(0.0);
			}
		}
	}
	MidiConversion {
		notes: kept.into_iter().map(|p| p.note).collect(),
		bends,
		key,
		dropped,
	}
}

/// Converts polyphonic notes ([`crate::poly`]): chords stay chords. Same options as
/// [`to_midi`] except `bends` and `merge_repeats` (per-note bends need MPE); a key snap
/// moves out-of-key notes to the nearest scale tone, and notes of one pitch that end up
/// overlapping are cut where the later starts. `origin` = song seconds at time 0.
pub fn poly_to_midi(
	notes: &[crate::poly::PolyNote],
	tempo: &TempoMap,
	origin: f64,
	opts: &MidiOptions,
) -> MidiConversion {
	let mut dropped = 0;
	let mut picked = Vec::with_capacity(notes.len());
	for n in notes {
		let (mut a, mut b) = (n.onset, n.offset);
		if let Some((lo, hi)) = opts.region {
			if (b.min(hi) - a.max(lo)) * 2.0 < b - a {
				continue;
			}
			a = a.max(lo);
			b = b.min(hi);
		}
		if b - a < opts.min_length {
			dropped += 1;
			continue;
		}
		picked.push((a, b, n.pitch, n.amplitude));
	}
	let key = match &opts.key {
		KeySnap::Off => None,
		KeySnap::Scale(s) => Some(*s),
		KeySnap::Auto => {
			let mut hist = [0.0f64; 12];
			for &(a, b, p, _) in &picked {
				hist[usize::from(p % 12)] += b - a;
			}
			Scale::infer(&hist)
		}
	};
	let beat = |seconds: f64| tempo.seconds_to_tick((origin + seconds).max(0.0)).beats();
	let snap = |b: f64| match opts.quantize {
		Some(grid) if grid > 0.0 => {
			b + opts.quantize_strength.clamp(0.0, 1.0) * ((b / grid).round() * grid - b)
		}
		_ => b,
	};
	let loudest = picked.iter().map(|n| n.3).fold(0.0f32, f32::max).max(1e-6);
	let mut out: Vec<MidiNote> = Vec::with_capacity(picked.len());
	for &(a, b, p, amp) in &picked {
		let base = match &key {
			Some(k) => nearest_in_key(k, f64::from(p)),
			None => p,
		};
		let mut pitch = i32::from(base) + 12 * opts.octave_shift;
		if let Some((low, high)) = opts.range {
			let (low, high) = (i32::from(low.min(high)), i32::from(low.max(high)));
			while pitch < low && pitch + 12 <= 127 {
				pitch += 12;
			}
			while pitch > high && pitch - 12 >= 0 && pitch - 12 >= low.min(high - 11) {
				pitch -= 12;
			}
		}
		let (start, mut end) = (snap(beat(a)), snap(beat(b)));
		if end <= start {
			end = start
				+ opts
					.quantize
					.filter(|g| *g > 0.0)
					.unwrap_or(beat(b) - beat(a))
					.max(1e-3);
		}
		out.push(MidiNote {
			start,
			length: end - start,
			pitch: pitch.clamp(0, 127) as u8,
			velocity: opts
				.velocity
				.unwrap_or_else(|| (0.35 + 0.65 * amp / loudest).clamp(0.25, 1.0)),
			cents: 0.0,
		});
	}
	out.sort_by(|x, y| x.start.total_cmp(&y.start).then(x.pitch.cmp(&y.pitch)));
	// One pitch at a time: a repeat cuts the note before it; a duplicate start is dropped.
	let mut kept: Vec<MidiNote> = Vec::with_capacity(out.len());
	for n in out {
		if let Some(prev) = kept.iter_mut().rev().find(|m| m.pitch == n.pitch)
			&& n.start < prev.start + prev.length
		{
			if n.start - prev.start < 1e-6 {
				dropped += 1;
				prev.length = prev.length.max(n.length);
				continue;
			}
			prev.length = n.start - prev.start;
		}
		kept.push(n);
	}
	MidiConversion {
		notes: kept,
		bends: Vec::new(),
		key,
		dropped,
	}
}

/// Converts drum hits ([`crate::drums`]) to General MIDI drum notes (a sixteenth long, or
/// the grid when quantizing), or every hit to `one_note`. Uses `quantize`,
/// `quantize_strength`, `velocity` and `region` of `opts`; hits of one note landing on the
/// same spot become one.
pub fn drums_to_midi(
	hits: &[crate::drums::DrumHit],
	tempo: &TempoMap,
	origin: f64,
	opts: &MidiOptions,
	one_note: Option<u8>,
) -> MidiConversion {
	let beat = |seconds: f64| tempo.seconds_to_tick((origin + seconds).max(0.0)).beats();
	let grid = opts.quantize.filter(|g| *g > 0.0);
	let snap = |b: f64| match grid {
		Some(g) => b + opts.quantize_strength.clamp(0.0, 1.0) * ((b / g).round() * g - b),
		None => b,
	};
	let length = grid.unwrap_or(0.25).min(0.25);
	let mut notes: Vec<MidiNote> = Vec::with_capacity(hits.len());
	let mut dropped = 0;
	for h in hits {
		if let Some((a, b)) = opts.region
			&& !(a..b).contains(&h.time)
		{
			continue;
		}
		let start = snap(beat(h.time));
		let pitch = one_note.unwrap_or_else(|| h.kind.note());
		if let Some(same) = notes
			.iter_mut()
			.rev()
			.take(16)
			.find(|n| n.pitch == pitch && (n.start - start).abs() < 1e-6)
		{
			same.velocity = same.velocity.max(h.velocity);
			dropped += 1;
			continue;
		}
		notes.push(MidiNote {
			start,
			length,
			pitch,
			velocity: opts.velocity.unwrap_or(h.velocity),
			cents: 0.0,
		});
	}
	notes.sort_by(|a, b| a.start.total_cmp(&b.start).then(a.pitch.cmp(&b.pitch)));
	MidiConversion {
		notes,
		bends: Vec::new(),
		key: None,
		dropped,
	}
}

/// The scale tone nearest to a fractional pitch (the lower one on a tie).
pub(crate) fn nearest_in_key(key: &Scale, sung: f64) -> u8 {
	let around = sung.round().clamp(0.0, 127.0) as u8;
	key.pitches_in_range(around.saturating_sub(2), around.saturating_add(2).min(127))
		.into_iter()
		.min_by(|a, b| {
			let (da, db) = ((f64::from(*a) - sung).abs(), (f64::from(*b) - sung).abs());
			da.total_cmp(&db).then(a.cmp(b))
		})
		.unwrap_or(around)
}

/// Loud level of a note: the 90th percentile of its core frames, dB.
fn level_of(analysis: &Analysis, n: &SegmentedNote) -> f64 {
	let (Some(a), Some(b)) = (
		analysis.frame_at(n.core_start),
		analysis.frame_at(n.core_end),
	) else {
		return -120.0;
	};
	let mut levels: Vec<f64> = analysis.frames[a..=b.max(a)]
		.iter()
		.map(|f| f.level_db)
		.collect();
	levels.sort_by(f64::total_cmp);
	levels[((levels.len() - 1) as f64 * 0.9).round() as usize]
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::analysis::{AnalysisOptions, analyze};
	use crate::eval::synth::{Phrase, Rendered, SynthNote, Vowel, render};
	use crate::segment::{SegmentOptions, segment};

	const RATE: u32 = 22_050;

	fn convert(notes: Vec<SynthNote>, opts: &MidiOptions) -> (Rendered, MidiConversion) {
		let r = render(&Phrase {
			notes,
			vowel: Vowel::A,
			sample_rate: RATE,
			snr_db: None,
			reverb: None,
			seed: 3,
		});
		let analysis =
			analyze(&r.samples, RATE, &AnalysisOptions::default(), &mut |_| true).unwrap();
		let seg = segment(&analysis, &SegmentOptions::default(), &[]);
		// 120 bpm: one beat = 0.5 s.
		let midi = to_midi(&seg, &analysis, &TempoMap::constant(120.0), 0.0, opts);
		(r, midi)
	}

	fn detuned(pitch: f64, cents: f64, duration: f64) -> SynthNote {
		let mut n = SynthNote::new(pitch, duration);
		n.detune_cents = cents;
		n
	}

	#[test]
	fn notes_land_at_their_pitch_and_time_in_beats() {
		let (r, m) = convert(
			vec![
				SynthNote::new(60.0, 0.5),
				SynthNote::new(64.0, 0.5).legato(0.08),
				detuned(67.0, 30.0, 0.6),
			],
			&MidiOptions::default(),
		);
		assert_eq!(
			m.notes.iter().map(|n| n.pitch).collect::<Vec<_>>(),
			[60, 64, 67]
		);
		for (n, t) in m.notes.iter().zip(&r.truth) {
			assert!((n.start - t.onset * 2.0).abs() < 0.04, "{n:?} vs {t:?}");
			assert!(
				(n.start + n.length - t.offset * 2.0).abs() < 0.08,
				"{n:?} vs {t:?}"
			);
		}
		assert!((m.notes[2].cents - 30.0).abs() < 5.0, "{:?}", m.notes[2]);
		assert!(m.bends.is_empty() && m.key.is_none());
	}

	#[test]
	fn quantize_key_and_range() {
		// D#4 sung 40 cents flat in a C major line snaps to D; everything lands on the grid.
		let line = vec![
			SynthNote::new(60.0, 0.4),
			SynthNote::new(64.0, 0.4),
			detuned(63.0, -40.0, 0.4),
			SynthNote::new(65.0, 0.4),
			SynthNote::new(67.0, 0.4),
			SynthNote::new(72.0, 0.6),
		];
		let opts = MidiOptions {
			key: KeySnap::Scale(Scale::parse("C major").unwrap()),
			range: Some((36, 59)),
			..MidiOptions::hum()
		};
		let (_, m) = convert(line, &opts);
		assert_eq!(
			m.notes.iter().map(|n| n.pitch).collect::<Vec<_>>(),
			[48, 52, 50, 53, 55, 48]
		);
		for n in &m.notes {
			for b in [n.start, n.start + n.length] {
				assert!((b * 4.0 - (b * 4.0).round()).abs() < 1e-6, "{n:?}");
			}
		}
	}

	#[test]
	fn the_key_is_inferred() {
		let line: Vec<SynthNote> = [57.0, 60.0, 64.0, 62.0, 60.0, 59.0, 57.0, 52.0, 57.0]
			.into_iter()
			.map(|p| SynthNote::new(p, 0.3))
			.collect();
		let (_, m) = convert(line, &MidiOptions::hum());
		assert_eq!(m.key, Some(Scale::parse("A minor").unwrap()));
	}

	#[test]
	fn bends_follow_the_sung_curve() {
		let (_, m) = convert(
			vec![
				detuned(62.0, 40.0, 0.6),
				SynthNote::new(65.0, 0.6).legato(0.2),
			],
			&MidiOptions {
				bends: true,
				..MidiOptions::default()
			},
		);
		assert_eq!(m.notes.len(), 2);
		// Inside the first note the bend holds +40 cents (0.2 of a 2-semitone range).
		let held: Vec<&BendPoint> = m
			.bends
			.iter()
			.filter(|b| b.beat > m.notes[0].start + 0.3 && b.beat < m.notes[0].start + 0.8)
			.collect();
		let at = m
			.bends
			.iter()
			.rev()
			.find(|b| b.beat <= m.notes[0].start + 0.6)
			.unwrap();
		assert!((at.value - 0.2).abs() < 0.03, "{at:?} {held:?}");
		// The glide into the second note is a bend from below.
		assert!(
			m.bends
				.iter()
				.any(|b| b.beat >= m.notes[1].start && b.value < -0.3),
			"{:?}",
			m.bends
		);
		assert_eq!(m.bends.last().unwrap().value, 0.0);
	}

	#[test]
	fn polyphonic_notes_keep_their_chords_snap_and_cut_repeats() {
		use crate::poly::PolyNote;
		let n = |onset, offset, pitch, amplitude| PolyNote {
			onset,
			offset,
			pitch,
			amplitude,
		};
		let notes = [
			n(0.0, 1.0, 60, 0.8),
			n(0.0, 1.0, 64, 0.8),
			n(0.0, 1.0, 67, 0.4),
			n(0.5, 1.5, 60, 0.8),
			n(1.0, 1.02, 72, 0.9),
			n(1.0, 2.0, 66, 0.8),
		];
		let tempo = TempoMap::constant(120.0);
		let c = poly_to_midi(&notes, &tempo, 1.0, &MidiOptions::default());
		assert_eq!(c.dropped, 1, "the 20 ms note");
		let starts: Vec<(f64, u8)> = c.notes.iter().map(|m| (m.start, m.pitch)).collect();
		assert_eq!(
			starts,
			[(2.0, 60), (2.0, 64), (2.0, 67), (3.0, 60), (4.0, 66)],
			"song beats: origin 1 s = beat 2"
		);
		assert!(
			(c.notes[0].length - 1.0).abs() < 1e-9,
			"cut where the repeat starts"
		);
		assert!(c.notes[2].velocity < c.notes[1].velocity);
		let snapped = poly_to_midi(
			&notes,
			&tempo,
			0.0,
			&MidiOptions {
				key: KeySnap::Scale(Scale::parse("C major").unwrap()),
				..MidiOptions::default()
			},
		);
		assert!(
			snapped.notes.iter().all(|m| m.pitch != 66),
			"F# snapped into C major"
		);
	}
}
