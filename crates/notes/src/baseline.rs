//! The naive detector every later phase has to beat: YIN pitch, a new note whenever the
//! pitch jumps by more than half a semitone between two frames or voicing stops, and the
//! center = plain mean pitch of the region. Glides move in small steps, so it merges the
//! notes they join and puts the center between them: the failure NE-2 exists to fix.

use crate::yin::{self, YinOptions};
use crate::{Note, NoteDetector};

#[derive(Debug, Clone)]
pub struct Baseline {
	pub yin: YinOptions,
	/// Semitones between consecutive frames that start a new note.
	pub jump: f64,
	/// Shorter regions are dropped (seconds).
	pub min_length: f64,
}

impl Default for Baseline {
	fn default() -> Self {
		Self {
			yin: YinOptions::default(),
			jump: 0.5,
			min_length: 0.05,
		}
	}
}

impl NoteDetector for Baseline {
	fn name(&self) -> &'static str {
		"baseline"
	}

	fn detect(&self, samples: &[f32], sample_rate: u32) -> Vec<Note> {
		let frames = yin::track(samples, sample_rate, &self.yin);
		let hop = self.yin.hop_seconds;
		let mut notes = Vec::new();
		let mut current: Option<(f64, f64, f64, usize)> = None; // onset, last time, sum, count
		let mut previous: Option<f64> = None;
		let close = |region: Option<(f64, f64, f64, usize)>, notes: &mut Vec<Note>| {
			if let Some((onset, last, sum, count)) = region {
				let offset = last + hop;
				if offset - onset >= self.min_length {
					notes.push(Note {
						onset,
						offset,
						center: sum / count as f64,
					});
				}
			}
		};
		for frame in &frames {
			match frame.pitch {
				None => {
					close(current.take(), &mut notes);
					previous = None;
				}
				Some(p) => {
					if previous.is_some_and(|q| (p - q).abs() > self.jump) {
						close(current.take(), &mut notes);
					}
					let region = current.get_or_insert((frame.time, frame.time, 0.0, 0));
					region.1 = frame.time;
					region.2 += p;
					region.3 += 1;
					previous = Some(p);
				}
			}
		}
		close(current.take(), &mut notes);
		notes
	}
}
