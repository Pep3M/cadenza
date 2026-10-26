//! Composition helpers built on `cadenza-theory`: chord progressions, drum grids and
//! scale-aware editing.

use std::collections::BTreeMap;

use cadenza_core::{ClipId, TrackId};
use cadenza_model::{Clip, Command, ModelError, Note, NotePatch};
use cadenza_theory::{ArpDirection, Chord, CompPattern, PatternOptions, Scale, VoicingStyle};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::convert::{PitchInput, beats_to_tick};
use crate::error::McpError;
use crate::session::Session;

fn tokenize(input: &str) -> Vec<String> {
	input
		.chars()
		.map(|c| if matches!(c, '-' | '|' | ',') { ' ' } else { c })
		.collect::<String>()
		.split_whitespace()
		.map(str::to_owned)
		.collect()
}

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PatternKind {
	/// One block chord held for the full duration.
	Sustain,
	ArpeggioUp,
	ArpeggioDown,
	ArpeggioUpdown,
	/// Block chord with a short, ascending stagger between tones.
	Strum,
	/// Block chord repeated every `rate_beats`.
	Pulse,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WriteChordProgressionArgs {
	pub track_id: TrackId,
	pub start_beat: f64,
	/// Chord symbols ("Am7 Dm9 G13 Cmaj7") or, when `key` is given, roman numerals
	/// ("vi ii V I"). Separators: spaces, '-', '|' or ','.
	pub progression: String,
	/// Key the roman numerals are analyzed in, e.g. "C major"; required when
	/// `progression` uses roman numerals, ignored for chord symbols.
	#[serde(default)]
	pub key: Option<String>,
	/// Duration of every chord in beats; ignored if `durations` is given.
	#[serde(default)]
	pub beats_per_chord: Option<f64>,
	/// Per-chord durations in beats; must match the number of chords in `progression`.
	#[serde(default)]
	pub durations: Option<Vec<f64>>,
	#[serde(default)]
	pub voicing: Option<VoicingStyle>,
	#[serde(default)]
	pub pattern: Option<PatternKind>,
	/// Rate of the arpeggio/pulse pattern in beats; defaults to a sixteenth (0.25) for
	/// arpeggios and a half note (0.5) for pulse.
	#[serde(default)]
	pub rate_beats: Option<f64>,
	/// Pitch voicings are built around; defaults to C4 (60).
	#[serde(default)]
	pub center_pitch: Option<PitchInput>,
	#[serde(default)]
	pub velocity: Option<f32>,
	/// Emit a sustained bass line (root/slash bass); defaults to true.
	#[serde(default)]
	pub include_bass: Option<bool>,
	/// MIDI octave for the bass line; defaults to 2.
	#[serde(default)]
	pub bass_octave: Option<i32>,
	/// Repeats the whole progression this many times; defaults to 1.
	#[serde(default)]
	pub repeat: Option<u32>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ChordOut {
	pub symbol: String,
	/// The concrete MIDI note names used for this chord's voicing.
	pub pitches: Vec<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct WriteChordProgressionOutput {
	pub clip_id: ClipId,
	pub chords: Vec<ChordOut>,
}

pub fn write_chord_progression(
	session: &mut Session,
	args: WriteChordProgressionArgs,
) -> Result<WriteChordProgressionOutput, McpError> {
	let chords: Vec<Chord> = if let Some(key) = &args.key {
		let scale = Scale::parse(key)?;
		cadenza_theory::parse_roman_progression(&args.progression, &scale)?
	} else {
		tokenize(&args.progression)
			.iter()
			.map(|t| Chord::parse(t))
			.collect::<cadenza_theory::Result<_>>()?
	};
	if chords.is_empty() {
		return Err(McpError::Invalid(
			"progression must contain at least one chord".into(),
		));
	}

	let durations: Vec<f64> = match (&args.durations, args.beats_per_chord) {
		(Some(d), _) => {
			if d.len() != chords.len() {
				return Err(McpError::Invalid(format!(
					"durations has {} entries but progression has {} chords",
					d.len(),
					chords.len()
				)));
			}
			d.clone()
		}
		(None, Some(b)) => vec![b; chords.len()],
		(None, None) => vec![4.0; chords.len()],
	};

	let style = args.voicing.unwrap_or(VoicingStyle::Close);
	let center = args.center_pitch.map(|p| p.0).unwrap_or(60);
	let pattern = match args.pattern.unwrap_or(PatternKind::Sustain) {
		PatternKind::Sustain => CompPattern::Sustain,
		PatternKind::ArpeggioUp => CompPattern::Arpeggio {
			direction: ArpDirection::Up,
			rate_beats: args.rate_beats.unwrap_or(0.25),
		},
		PatternKind::ArpeggioDown => CompPattern::Arpeggio {
			direction: ArpDirection::Down,
			rate_beats: args.rate_beats.unwrap_or(0.25),
		},
		PatternKind::ArpeggioUpdown => CompPattern::Arpeggio {
			direction: ArpDirection::UpDown,
			rate_beats: args.rate_beats.unwrap_or(0.25),
		},
		PatternKind::Strum => CompPattern::Strum,
		PatternKind::Pulse => CompPattern::Pulse {
			rate_beats: args.rate_beats.unwrap_or(0.5),
		},
	};
	let options = PatternOptions {
		center,
		velocity: args.velocity.unwrap_or(0.8),
		include_bass: args.include_bass.unwrap_or(true),
		bass_octave: args.bass_octave.unwrap_or(2),
	};

	let repeat = args.repeat.unwrap_or(1).max(1);
	let mut pairs = Vec::with_capacity(chords.len() * repeat as usize);
	for _ in 0..repeat {
		pairs.extend(chords.iter().cloned().zip(durations.iter().copied()));
	}
	let total_beats: f64 = pairs.iter().map(|(_, d)| d).sum();

	let note_specs = cadenza_theory::progression_to_notes(&pairs, style, pattern, &options);
	let clip = Clip::midi(
		"Chords",
		beats_to_tick(args.start_beat),
		beats_to_tick(total_beats),
	)
	.with_notes(note_specs.into_iter().map(|n| {
		Note::new(
			n.pitch,
			beats_to_tick(n.start_beats),
			beats_to_tick(n.length_beats),
			n.velocity,
		)
	}));
	let clip_id = clip.id;
	session.document_mut().execute(Command::AddClip {
		track: args.track_id,
		clip,
	})?;

	let mut previous: Option<Vec<u8>> = None;
	let chords_out = chords
		.iter()
		.map(|chord| {
			let voiced = cadenza_theory::voice(chord, style, center, previous.as_deref());
			previous = Some(voiced.clone());
			ChordOut {
				symbol: chord.symbol.clone(),
				pitches: voiced
					.iter()
					.map(|&p| cadenza_theory::format_pitch(p, false))
					.collect(),
			}
		})
		.collect();
	Ok(WriteChordProgressionOutput {
		clip_id,
		chords: chords_out,
	})
}

fn lane_pitch(name: &str) -> Result<u8, McpError> {
	crate::tools::samples::pad_note(name)
}

/// Lanes the track's drum instrument would leave silent: a drum_sampler pad with no sample,
/// or a note the drum_machine has no sound for.
pub(crate) fn drum_lane_warnings<'a>(
	project: &cadenza_model::Project,
	track: TrackId,
	lanes: impl IntoIterator<Item = (&'a str, u8)>,
) -> Vec<String> {
	let Some(device) = project.track(track).and_then(|t| t.instrument.as_ref()) else {
		return Vec::new();
	};
	let mut warnings = Vec::new();
	for (lane, pitch) in lanes {
		match device.builtin_id() {
			Some("drum_sampler")
				if !device
					.zones
					.iter()
					.any(|z| z.matches(pitch, 1.0) || z.matches(pitch, 0.5)) =>
			{
				warnings.push(format!(
					"lane '{lane}' ({pitch}) has no sample on '{}': load one with load_sample pad \
					 \"{lane}\" or use another lane",
					device.name
				));
			}
			Some("drum_machine") if !cadenza_dsp::instruments::drum_machine::plays(pitch) => {
				warnings.push(format!(
					"lane '{lane}' ({pitch}) is silent on drum_machine (it plays GM notes 35-81: \
					 kit, cymbals and Latin percussion): use a drum_sampler with a sample on that pad"
				));
			}
			_ => {}
		}
	}
	warnings
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WriteDrumPatternArgs {
	pub track_id: TrackId,
	pub start_beat: f64,
	pub bars: u32,
	/// Grid resolution per beat; defaults to 4 (sixteenth notes).
	#[serde(default)]
	pub steps_per_beat: Option<u32>,
	/// 0..=1: delays every second step for a swung feel.
	#[serde(default)]
	pub swing: Option<f64>,
	/// Lane name (a GM drum name such as "kick", "snare", "clap", "rim", "closed_hat",
	/// "pedal_hat", "open_hat", "low_tom", "mid_tom", "high_tom", "crash", "ride",
	/// "tambourine", "cowbell", a drum_sampler pad name, or any note name/MIDI number) to a
	/// step string. Steps: `X` accent, `x` normal, `o` ghost, `.`/`-` rest.
	/// Spaces and `|` are ignored (use them for readability); the pattern repeats to
	/// fill the requested bars.
	pub lanes: BTreeMap<String, String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct WriteDrumPatternOutput {
	pub clip_id: ClipId,
	pub length_beats: f64,
	/// Lanes the track's drum_sampler has no sample for (they would be silent).
	#[serde(skip_serializing_if = "Vec::is_empty")]
	pub warnings: Vec<String>,
}

pub fn write_drum_pattern(
	session: &mut Session,
	args: WriteDrumPatternArgs,
) -> Result<WriteDrumPatternOutput, McpError> {
	if args.bars == 0 {
		return Err(McpError::Invalid("bars must be >= 1".into()));
	}
	if args.lanes.is_empty() {
		return Err(McpError::Invalid("lanes must not be empty".into()));
	}
	let steps_per_beat = args.steps_per_beat.unwrap_or(4).max(1);
	let swing = args.swing.unwrap_or(0.0).clamp(0.0, 1.0);
	// Bars follow the time signatures from the start (a 3/4 bar is three beats long).
	let total_beats = {
		let meter = session.document().project().meter();
		let start = beats_to_tick(args.start_beat);
		let end = meter.tick_at_bar_position(meter.bar_position(start) + f64::from(args.bars));
		(end - start).beats()
	};
	let total_steps = (total_beats * steps_per_beat as f64).round() as usize;
	let step_beats = 1.0 / steps_per_beat as f64;

	let mut lanes = Vec::with_capacity(args.lanes.len());
	for (lane, pattern) in &args.lanes {
		lanes.push((lane.clone(), lane_pitch(lane)?, pattern));
	}
	let warnings = drum_lane_warnings(
		session.document().project(),
		args.track_id,
		lanes.iter().map(|(l, p, _)| (l.as_str(), *p)),
	);
	let mut notes = Vec::new();
	for (lane, pitch, pattern) in lanes {
		let cleaned: Vec<char> = pattern
			.chars()
			.filter(|c| !matches!(c, ' ' | '|'))
			.collect();
		if cleaned.is_empty() {
			return Err(McpError::Invalid(format!(
				"lane '{lane}' has an empty step pattern"
			)));
		}
		for i in 0..total_steps {
			let velocity = match cleaned[i % cleaned.len()] {
				'X' => 1.0,
				'x' => 0.8,
				'o' => 0.45,
				'.' | '-' => continue,
				other => {
					return Err(McpError::Invalid(format!(
						"lane '{lane}' has unrecognized step '{other}'; use X (accent), x (normal), o (ghost), '.' or '-' (rest)"
					)));
				}
			};
			let mut beat = i as f64 * step_beats;
			if i % 2 == 1 {
				beat += swing * step_beats * 0.5;
			}
			notes.push(Note::new(
				pitch,
				beats_to_tick(beat),
				beats_to_tick(step_beats),
				velocity,
			));
		}
	}

	let length_beats = total_steps as f64 * step_beats;
	let clip = Clip::midi(
		"Drums",
		beats_to_tick(args.start_beat),
		beats_to_tick(length_beats),
	)
	.with_notes(notes);
	let clip_id = clip.id;
	session.document_mut().execute(Command::AddClip {
		track: args.track_id,
		clip,
	})?;
	Ok(WriteDrumPatternOutput {
		clip_id,
		length_beats,
		warnings,
	})
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ScaleInfoArgs {
	/// `"<root> <kind>"`, e.g. "C major", "A minor", "F# dorian".
	pub scale: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ScaleInfoOutput {
	pub root: String,
	pub kind: cadenza_theory::ScaleKind,
	/// Ascending note names for one octave starting at the root (octave 4).
	pub notes: Vec<String>,
	pub pitches: Vec<u8>,
}

pub fn scale_info(args: ScaleInfoArgs) -> Result<ScaleInfoOutput, McpError> {
	let scale = Scale::parse(&args.scale)?;
	let low = (5 * 12) + scale.root.0 as i32;
	let pitches = scale.pitches_in_range(low as u8, (low + 11) as u8);
	let notes = pitches
		.iter()
		.map(|&p| cadenza_model::note_name(p))
		.collect();
	Ok(ScaleInfoOutput {
		root: scale.root.sharp_name().to_owned(),
		kind: scale.kind,
		notes,
		pitches,
	})
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SnapToScaleArgs {
	pub clip_id: ClipId,
	pub scale: String,
}

pub fn snap_to_scale(session: &mut Session, args: SnapToScaleArgs) -> Result<(), McpError> {
	let scale = Scale::parse(&args.scale)?;
	let project = session.document().project();
	let (_, clip) = project
		.find_clip(args.clip_id)
		.ok_or(ModelError::ClipNotFound(args.clip_id))?;
	let patches: Vec<NotePatch> = clip
		.notes()
		.iter()
		.filter_map(|n| {
			let snapped = scale.snap(n.pitch);
			(snapped != n.pitch).then_some(NotePatch {
				id: n.id,
				pitch: Some(snapped),
				velocity: None,
				start: None,
				length: None,
				bend: None,
				pressure: None,
				timbre: None,
			})
		})
		.collect();
	if patches.is_empty() {
		return Ok(());
	}
	session.document_mut().execute(Command::UpdateNotes {
		clip: args.clip_id,
		notes: patches,
	})?;
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;
	use cadenza_model::TrackKind;

	fn instrument_track(session: &mut Session) -> TrackId {
		let track = cadenza_model::Track::new("Lead", TrackKind::Instrument);
		let track_id = track.id;
		session
			.document_mut()
			.execute(Command::AddTrack { track, index: None })
			.unwrap();
		track_id
	}

	#[test]
	fn write_chord_progression_from_symbols_creates_one_clip() {
		let mut session = Session::new();
		let track_id = instrument_track(&mut session);
		let out = write_chord_progression(
			&mut session,
			WriteChordProgressionArgs {
				track_id,
				start_beat: 0.0,
				progression: "Am7 Dm9 G13 Cmaj7".into(),
				key: None,
				beats_per_chord: Some(2.0),
				durations: None,
				voicing: Some(VoicingStyle::Close),
				pattern: Some(PatternKind::Sustain),
				rate_beats: None,
				center_pitch: None,
				velocity: None,
				include_bass: Some(true),
				bass_octave: None,
				repeat: None,
			},
		)
		.unwrap();
		assert_eq!(out.chords.len(), 4);
		assert_eq!(out.chords[0].symbol, "Am7");
		let (_, clip) = session.document().project().find_clip(out.clip_id).unwrap();
		assert_eq!(clip.length.beats(), 8.0);
		assert!(!clip.notes().is_empty());
	}

	#[test]
	fn write_chord_progression_from_roman_numerals_needs_a_key() {
		let mut session = Session::new();
		let track_id = instrument_track(&mut session);
		let out = write_chord_progression(
			&mut session,
			WriteChordProgressionArgs {
				track_id,
				start_beat: 0.0,
				progression: "vi ii V I".into(),
				key: Some("C major".into()),
				beats_per_chord: Some(1.0),
				durations: None,
				voicing: None,
				pattern: None,
				rate_beats: None,
				center_pitch: None,
				velocity: None,
				include_bass: Some(false),
				bass_octave: None,
				repeat: None,
			},
		)
		.unwrap();
		assert_eq!(out.chords[0].symbol, "vi");
	}

	#[test]
	fn write_drum_pattern_parses_grid_with_accents_and_rests() {
		let mut session = Session::new();
		let track_id = instrument_track(&mut session);
		let mut lanes = BTreeMap::new();
		lanes.insert("kick".to_owned(), "X...x...".to_owned());
		lanes.insert("closed_hat".to_owned(), "o.o.".to_owned());
		let out = write_drum_pattern(
			&mut session,
			WriteDrumPatternArgs {
				track_id,
				start_beat: 0.0,
				bars: 1,
				steps_per_beat: Some(4),
				swing: None,
				lanes,
			},
		)
		.unwrap();
		assert_eq!(out.length_beats, 4.0);
		let (_, clip) = session.document().project().find_clip(out.clip_id).unwrap();
		let kicks: Vec<_> = clip.notes().iter().filter(|n| n.pitch == 36).collect();
		// "X...x..." (8 steps) tiles twice across 16 steps: X at 0/8, x at 4/12.
		assert_eq!(kicks.len(), 4);
		assert_eq!(kicks[0].velocity, 1.0); // 'X'
		let hats: Vec<_> = clip.notes().iter().filter(|n| n.pitch == 42).collect();
		assert_eq!(hats.len(), 8); // "o.o." tiled x4 across 16 steps, half are rests -> 8 hits
		assert!(hats.iter().all(|n| n.velocity == 0.45));
	}

	#[test]
	fn write_drum_pattern_swing_delays_odd_steps() {
		let mut session = Session::new();
		let track_id = instrument_track(&mut session);
		let mut lanes = BTreeMap::new();
		lanes.insert("closed_hat".to_owned(), "xxxx".to_owned());
		let out = write_drum_pattern(
			&mut session,
			WriteDrumPatternArgs {
				track_id,
				start_beat: 0.0,
				bars: 1,
				steps_per_beat: Some(4),
				swing: Some(1.0),
				lanes,
			},
		)
		.unwrap();
		let (_, clip) = session.document().project().find_clip(out.clip_id).unwrap();
		let mut starts: Vec<f64> = clip.notes().iter().map(|n| n.start.beats()).collect();
		starts.sort_by(|a, b| a.partial_cmp(b).unwrap());
		// Step 1 (index 1) is swung later than an unswung sixteenth grid position.
		assert!(starts[1] > 0.25);
	}

	#[test]
	fn write_drum_pattern_rejects_unknown_step_character() {
		let mut session = Session::new();
		let track_id = instrument_track(&mut session);
		let mut lanes = BTreeMap::new();
		lanes.insert("kick".to_owned(), "X?..".to_owned());
		let err = write_drum_pattern(
			&mut session,
			WriteDrumPatternArgs {
				track_id,
				start_beat: 0.0,
				bars: 1,
				steps_per_beat: Some(4),
				swing: None,
				lanes,
			},
		)
		.unwrap_err();
		assert!(matches!(err, McpError::Invalid(_)), "{err}");
	}

	#[test]
	fn snap_to_scale_moves_out_of_key_notes() {
		let mut session = Session::new();
		let track_id = instrument_track(&mut session);
		let clip_id = crate::tools::clips::add_midi_clip(
			&mut session,
			crate::tools::clips::AddMidiClipArgs {
				track_id,
				start_beat: 0.0,
				length_beats: 4.0,
				name: None,
				notes: vec![crate::tools::clips::NoteInput {
					pitch: PitchInput(63), // D#4, not in C major
					start: 0.0,
					length: 1.0,
					velocity: Some(0.8),
					bend: None,
					pressure: None,
					timbre: None,
				}],
			},
		)
		.unwrap()
		.clip_id;
		snap_to_scale(
			&mut session,
			SnapToScaleArgs {
				clip_id,
				scale: "C major".into(),
			},
		)
		.unwrap();
		let (_, clip) = session.document().project().find_clip(clip_id).unwrap();
		assert_ne!(clip.notes()[0].pitch, 63);
		assert!(
			Scale::parse("C major")
				.unwrap()
				.contains(clip.notes()[0].pitch)
		);
	}
}
