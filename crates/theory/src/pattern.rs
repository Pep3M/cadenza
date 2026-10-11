//! Turning a chord progression into note events (comping patterns).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::chord::Chord;
use crate::voicing::{self, VoicingStyle};

/// One note event. Time is expressed in beats (quarter notes), matching the
/// rest of Cadenza's AI-facing API.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct NoteSpec {
	pub pitch: u8,
	pub start_beats: f64,
	pub length_beats: f64,
	pub velocity: f32,
}

/// Arpeggio direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ArpDirection {
	Up,
	Down,
	UpDown,
}

/// A comping/accompaniment pattern.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CompPattern {
	/// One block chord held for the full duration.
	Sustain,
	/// Tones played one after another, cycling at `rate_beats`.
	Arpeggio {
		direction: ArpDirection,
		rate_beats: f64,
	},
	/// Block chord with a short, ascending stagger between tones.
	Strum,
	/// Block chord repeated every `rate_beats`.
	Pulse { rate_beats: f64 },
}

/// Options for [`progression_to_notes`].
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PatternOptions {
	/// Center pitch voicings are built around.
	pub center: u8,
	/// Note velocity (0..=1).
	pub velocity: f32,
	/// Whether to emit a separate sustained bass line (root/slash bass, one
	/// octave below `center` by default).
	pub include_bass: bool,
	/// MIDI octave number for the bass line.
	pub bass_octave: i32,
}

impl Default for PatternOptions {
	fn default() -> Self {
		Self {
			center: 60,
			velocity: 0.8,
			include_bass: true,
			bass_octave: 2,
		}
	}
}

const STRUM_STAGGER_BEATS: f64 = 0.02;

fn order_for_arpeggio(mut pitches: Vec<u8>, direction: ArpDirection) -> Vec<u8> {
	pitches.sort_unstable();
	match direction {
		ArpDirection::Up => pitches,
		ArpDirection::Down => {
			pitches.reverse();
			pitches
		}
		ArpDirection::UpDown => {
			let mut up = pitches.clone();
			let mut down: Vec<u8> = pitches.into_iter().rev().skip(1).collect();
			if down.len() > 1 {
				down.pop();
			}
			up.extend(down);
			up
		}
	}
}

/// Renders a chord progression (`(chord, duration_beats)` pairs) into note
/// events using a voicing `style` and a comping `pattern`.
pub fn progression_to_notes(
	chords: &[(Chord, f64)],
	style: VoicingStyle,
	pattern: CompPattern,
	options: &PatternOptions,
) -> Vec<NoteSpec> {
	let mut notes = Vec::new();
	let mut previous_voicing: Option<Vec<u8>> = None;
	let mut cursor = 0.0_f64;

	for (chord, duration) in chords {
		let duration = duration.max(0.0);
		let voiced = voicing::voice(chord, style, options.center, previous_voicing.as_deref());
		previous_voicing = Some(voiced.clone());

		match pattern {
			CompPattern::Sustain => {
				for &pitch in &voiced {
					notes.push(NoteSpec {
						pitch,
						start_beats: cursor,
						length_beats: duration,
						velocity: options.velocity,
					});
				}
			}
			CompPattern::Strum => {
				for (i, &pitch) in voiced.iter().enumerate() {
					let offset = i as f64 * STRUM_STAGGER_BEATS;
					notes.push(NoteSpec {
						pitch,
						start_beats: cursor + offset,
						length_beats: (duration - offset).max(0.0),
						velocity: options.velocity,
					});
				}
			}
			CompPattern::Pulse { rate_beats } => {
				let rate = rate_beats.max(0.01);
				let mut t = 0.0;
				while t < duration {
					let len = rate.min(duration - t);
					for &pitch in &voiced {
						notes.push(NoteSpec {
							pitch,
							start_beats: cursor + t,
							length_beats: len,
							velocity: options.velocity,
						});
					}
					t += rate;
				}
			}
			CompPattern::Arpeggio {
				direction,
				rate_beats,
			} => {
				let rate = rate_beats.max(0.01);
				let order = order_for_arpeggio(voiced.clone(), direction);
				if !order.is_empty() {
					let mut t = 0.0;
					let mut i = 0;
					while t < duration {
						let len = rate.min(duration - t);
						notes.push(NoteSpec {
							pitch: order[i % order.len()],
							start_beats: cursor + t,
							length_beats: len,
							velocity: options.velocity,
						});
						t += rate;
						i += 1;
					}
				}
			}
		}

		if options.include_bass {
			let bass = voicing::bass_note(chord, options.bass_octave);
			notes.push(NoteSpec {
				pitch: bass,
				start_beats: cursor,
				length_beats: duration,
				velocity: options.velocity,
			});
		}

		cursor += duration;
	}

	notes
}
