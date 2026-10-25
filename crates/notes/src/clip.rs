//! The notes of an audio clip as the project stores them ([`NoteEdits`], NE-5) <-> the
//! detection and resynthesis types: detecting (and re-detecting with the user's splits,
//! merges and edits kept), splitting and merging notes, and what the resynthesis renders.

use cadenza_core::AudioNoteId;
use cadenza_model::{AudioNote, AudioNoteFlag, NoteEdits, NoteSpan};

use crate::NotesError;
use crate::analysis::Analysis;
use crate::edit::{Edits, NoteEdit, TimeEdit};
use crate::resynth::center_over;
use crate::segment::{NoteFlag, SegmentOptions, Segmentation, SegmentedNote, Sibilant, segment};

/// A boundary closer than this to a user merge is the merged one, seconds.
const MERGE_TOLERANCE: f64 = 0.03;

/// The stored notes as a segmentation (drift and vibrato are not stored: the resynthesis
/// measures them on the frames).
pub fn to_segmentation(e: &NoteEdits) -> Segmentation {
	Segmentation {
		notes: e
			.notes
			.iter()
			.map(|n| SegmentedNote {
				onset: n.start,
				offset: n.end,
				core_start: n.core_start,
				core_end: n.core_end,
				center: n.center,
				drift: 0.0,
				vibrato: None,
				confidence: f64::from(n.confidence),
				flags: Vec::new(),
			})
			.collect(),
		sibilants: e
			.sibilants
			.iter()
			.map(|s| Sibilant {
				start: s.start,
				end: s.end,
			})
			.collect(),
	}
}

/// The stored edits as resynthesis edits (index-aligned with [`to_segmentation`]).
pub fn to_edits(e: &NoteEdits) -> Edits {
	Edits {
		notes: e
			.notes
			.iter()
			.map(|n| NoteEdit {
				pitch: n.pitch,
				drift: f64::from(n.drift),
				modulation: f64::from(n.modulation),
				formant_cents: f64::from(n.formant_cents),
				gain_db: f64::from(n.gain_db),
				time: n.time.map(|t| TimeEdit {
					onset: t.start,
					offset: t.end,
				}),
			})
			.collect(),
		sibilant_gain_db: f64::from(e.sibilant_gain_db),
	}
}

fn stored(n: &SegmentedNote, id: AudioNoteId) -> AudioNote {
	AudioNote {
		core_start: n.core_start,
		core_end: n.core_end,
		confidence: n.confidence.clamp(0.0, 1.0) as f32,
		flags: n
			.flags
			.iter()
			.map(|f| match *f {
				NoteFlag::MaybeTwoNotes { at, cents } => AudioNoteFlag::MaybeTwoNotes {
					at,
					cents: cents as f32,
				},
				NoteFlag::ShortCore => AudioNoteFlag::ShortCore,
				NoteFlag::WeakVoicing => AudioNoteFlag::WeakVoicing,
			})
			.collect(),
		..AudioNote::new(id, n.onset, n.offset, n.center)
	}
}

/// Detects the notes of `analysis` with the sensitivity, splits and merges of `previous`
/// (or `sensitivity` and none). Notes that match a previous one (most of their core over
/// its core) keep its id and edits; the others get ids from `new_id`.
pub fn detect(
	analysis: &Analysis,
	sensitivity: f32,
	previous: Option<&NoteEdits>,
	new_id: &mut dyn FnMut() -> AudioNoteId,
) -> NoteEdits {
	let (splits, merges) = previous.map_or((Vec::new(), Vec::new()), |p| {
		(p.user_splits.clone(), p.user_merges.clone())
	});
	let opts = SegmentOptions {
		sensitivity: f64::from(sensitivity),
		..SegmentOptions::default()
	};
	let mut seg = segment(analysis, &opts, &splits);
	for &m in &merges {
		if let Some(i) = (1..seg.notes.len()).find(|&i| {
			(seg.notes[i - 1].offset - m).abs() < MERGE_TOLERANCE
				&& (seg.notes[i].onset - m).abs() < MERGE_TOLERANCE
		}) {
			let b = seg.notes.remove(i);
			let a = &mut seg.notes[i - 1];
			join(analysis, a, &b, &opts);
		}
	}
	let mut taken = vec![false; previous.map_or(0, |p| p.notes.len())];
	let notes = seg
		.notes
		.iter()
		.map(|n| {
			let matched = previous.and_then(|p| {
				p.notes
					.iter()
					.enumerate()
					.filter(|(k, _)| !taken[*k])
					.map(|(k, old)| (k, overlap(n, old)))
					.filter(|(_, o)| *o >= 0.5 * (n.core_end - n.core_start).max(1e-6))
					.max_by(|a, b| a.1.total_cmp(&b.1))
					.map(|(k, _)| k)
			});
			match matched {
				Some(k) => {
					taken[k] = true;
					let old = &previous.expect("matched a previous note").notes[k];
					AudioNote {
						pitch: old.pitch.filter(|p| {
							(p - n.center).abs() <= cadenza_model::audio_notes::MAX_SHIFT
						}),
						drift: old.drift,
						modulation: old.modulation,
						formant_cents: old.formant_cents,
						gain_db: old.gain_db,
						time: old.time,
						..stored(n, old.id)
					}
				}
				None => stored(n, new_id()),
			}
		})
		.collect();
	NoteEdits {
		notes,
		sibilants: seg
			.sibilants
			.iter()
			.map(|s| NoteSpan {
				start: s.start,
				end: s.end,
			})
			.collect(),
		sibilant_gain_db: previous.map_or(0.0, |p| p.sibilant_gain_db),
		sensitivity,
		user_splits: splits,
		user_merges: merges,
	}
}

fn overlap(n: &SegmentedNote, old: &AudioNote) -> f64 {
	(n.core_end.min(old.core_end) - n.core_start.max(old.core_start)).max(0.0)
}

/// `a` absorbs the following note `b`: one region, one core, the center measured over it.
fn join(analysis: &Analysis, a: &mut SegmentedNote, b: &SegmentedNote, opts: &SegmentOptions) {
	a.offset = b.offset;
	a.core_end = b.core_end;
	a.center = center_over(analysis, a.core_start, a.core_end, opts).unwrap_or(a.center);
	a.flags.clear();
	a.confidence = a.confidence.min(b.confidence);
}

/// Splits note `id` at `at` seconds (a user separation, kept by later re-detections).
pub fn split(
	analysis: &Analysis,
	edits: &NoteEdits,
	id: AudioNoteId,
	at: f64,
	new_id: &mut dyn FnMut() -> AudioNoteId,
) -> Result<NoteEdits, NotesError> {
	let n = edits
		.note(id)
		.ok_or_else(|| NotesError::Edit(format!("note {id} is not in the clip")))?;
	if !(at > n.start + 0.02 && at < n.end - 0.02) {
		return Err(NotesError::Edit(format!(
			"split at {at:.3} s is not inside the note ({:.3}..{:.3} s, at least 20 ms from \
			 its ends)",
			n.start, n.end
		)));
	}
	let mut previous = edits.clone();
	previous
		.user_merges
		.retain(|m| !(n.start..=n.end).contains(m));
	previous.user_splits.push(at);
	previous.user_splits.sort_by(f64::total_cmp);
	Ok(detect(analysis, edits.sensitivity, Some(&previous), new_id))
}

/// Joins note `id` with the note right after it (a user merge, kept by re-detections).
pub fn merge_with_next(
	analysis: &Analysis,
	edits: &NoteEdits,
	id: AudioNoteId,
	new_id: &mut dyn FnMut() -> AudioNoteId,
) -> Result<NoteEdits, NotesError> {
	let i = edits
		.notes
		.iter()
		.position(|n| n.id == id)
		.ok_or_else(|| NotesError::Edit(format!("note {id} is not in the clip")))?;
	let (Some(a), Some(b)) = (edits.notes.get(i), edits.notes.get(i + 1)) else {
		return Err(NotesError::Edit(
			"the note is the last one: there is no next note to merge with".into(),
		));
	};
	if b.start - a.end > MERGE_TOLERANCE {
		return Err(NotesError::Edit(format!(
			"the notes are {:.0} ms apart: only touching notes can be merged",
			(b.start - a.end) * 1000.0
		)));
	}
	let boundary = 0.5 * (a.end + b.start);
	let mut previous = edits.clone();
	previous
		.user_splits
		.retain(|s| (s - boundary).abs() >= MERGE_TOLERANCE);
	previous.user_merges.push(boundary);
	previous.user_merges.sort_by(f64::total_cmp);
	Ok(detect(analysis, edits.sensitivity, Some(&previous), new_id))
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::analysis::{AnalysisOptions, analyze};
	use crate::eval::synth;

	fn ids() -> impl FnMut() -> AudioNoteId {
		AudioNoteId::new
	}

	#[test]
	fn re_detection_keeps_ids_edits_splits_and_merges() {
		let case = &synth::corpus(22_050)[1];
		let a = analyze(
			&case.samples,
			case.sample_rate,
			&AnalysisOptions::default(),
			&mut |_| true,
		)
		.unwrap();
		let mut new_id = ids();
		let first = detect(&a, 1.0, None, &mut new_id);
		assert_eq!(first.notes.len(), 10);
		cadenza_model::audio_notes::validate(&first).unwrap();

		let mut edited = first.clone();
		edited.notes[2].pitch = Some(edited.notes[2].center.round());
		edited.notes[2].gain_db = -2.0;
		let again = detect(&a, 1.0, Some(&edited), &mut new_id);
		assert_eq!(again, edited);

		// Split note 4 in its middle: two notes, the larger part keeps the id.
		let n = &edited.notes[4];
		let at = 0.5 * (n.core_start + n.core_end);
		let split = split(&a, &edited, n.id, at, &mut new_id).unwrap();
		assert_eq!(split.notes.len(), 11);
		assert!(split.notes.iter().any(|m| m.id == n.id));
		assert_eq!(split.user_splits, vec![at]);
		// The split survives a re-detection, and the edited note keeps its edit.
		let kept = detect(&a, 1.0, Some(&split), &mut new_id);
		assert_eq!(kept.notes.len(), 11);
		assert_eq!(kept.notes[2].gain_db, -2.0);

		// Merging the halves back removes the split.
		let i = split
			.notes
			.iter()
			.position(|m| m.start >= at - 0.03)
			.unwrap() - 1;
		let merged = merge_with_next(&a, &split, split.notes[i].id, &mut new_id).unwrap();
		assert_eq!(merged.notes.len(), 10);
		assert!(merged.user_splits.is_empty());
		let err = merge_with_next(&a, &merged, merged.notes[0].id, &mut new_id).unwrap_err();
		assert!(err.to_string().contains("apart"), "{err}");
	}

	#[test]
	fn stored_notes_round_trip_to_the_resynthesis() {
		let e = NoteEdits {
			notes: vec![AudioNote {
				pitch: Some(61.0),
				time: Some(NoteSpan {
					start: 0.2,
					end: 0.6,
				}),
				..AudioNote::new(AudioNoteId::new(), 0.1, 0.5, 60.7)
			}],
			sibilants: vec![NoteSpan {
				start: 0.6,
				end: 0.7,
			}],
			sibilant_gain_db: -3.0,
			sensitivity: 1.0,
			user_splits: Vec::new(),
			user_merges: Vec::new(),
		};
		let seg = to_segmentation(&e);
		let edits = to_edits(&e);
		assert_eq!(seg.notes[0].center, 60.7);
		assert_eq!(seg.sibilants.len(), 1);
		assert_eq!(edits.notes[0].pitch, Some(61.0));
		assert_eq!(edits.notes[0].time.unwrap().onset, 0.2);
		assert_eq!(edits.sibilant_gain_db, -3.0);
		edits.validate(&seg).unwrap();
	}
}
