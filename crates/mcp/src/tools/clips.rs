//! MIDI clips and notes: creation, editing and the humanize/quantize helpers.

use cadenza_core::{ClipId, NoteId, TrackId};
use cadenza_model::{Clip, ClipPatch, Command, ModelError, Note, NotePatch};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::convert::{PitchInput, beats_to_tick, tick_to_beats};
use crate::error::McpError;
use crate::session::Session;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct NoteInput {
	pub pitch: PitchInput,
	/// Beats from the clip start.
	pub start: f64,
	pub length: f64,
	/// 0..=1; defaults to 0.8.
	#[serde(default)]
	pub velocity: Option<f32>,
}

impl NoteInput {
	fn into_note(self) -> Note {
		Note::new(
			self.pitch.0,
			beats_to_tick(self.start),
			beats_to_tick(self.length),
			self.velocity.unwrap_or(0.8),
		)
	}
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AddMidiClipArgs {
	pub track_id: TrackId,
	pub start_beat: f64,
	pub length_beats: f64,
	#[serde(default)]
	pub name: Option<String>,
	#[serde(default)]
	pub notes: Vec<NoteInput>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct AddMidiClipOutput {
	pub clip_id: ClipId,
}

pub fn add_midi_clip(
	session: &mut Session,
	args: AddMidiClipArgs,
) -> Result<AddMidiClipOutput, McpError> {
	let clip = Clip::midi(
		args.name.unwrap_or_else(|| "Clip".to_owned()),
		beats_to_tick(args.start_beat),
		beats_to_tick(args.length_beats),
	)
	.with_notes(args.notes.into_iter().map(NoteInput::into_note));
	let clip_id = clip.id;
	session.document_mut().execute(Command::AddClip {
		track: args.track_id,
		clip,
	})?;
	Ok(AddMidiClipOutput { clip_id })
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AddNotesArgs {
	pub clip_id: ClipId,
	pub notes: Vec<NoteInput>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct AddNotesOutput {
	pub note_ids: Vec<NoteId>,
}

pub fn add_notes(session: &mut Session, args: AddNotesArgs) -> Result<AddNotesOutput, McpError> {
	let notes: Vec<Note> = args.notes.into_iter().map(NoteInput::into_note).collect();
	let note_ids = notes.iter().map(|n| n.id).collect();
	session.document_mut().execute(Command::AddNotes {
		clip: args.clip_id,
		notes,
	})?;
	Ok(AddNotesOutput { note_ids })
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RemoveNotesArgs {
	pub clip_id: ClipId,
	/// Note ids to remove.
	#[serde(default)]
	pub note_ids: Vec<NoteId>,
	/// Remove every note in the clip instead of listing `note_ids`.
	#[serde(default)]
	pub all: bool,
}

pub fn remove_notes(session: &mut Session, args: RemoveNotesArgs) -> Result<(), McpError> {
	let note_ids = if args.all {
		let (_, clip) = session
			.document()
			.project()
			.find_clip(args.clip_id)
			.ok_or(ModelError::ClipNotFound(args.clip_id))?;
		clip.notes().iter().map(|n| n.id).collect()
	} else if !args.note_ids.is_empty() {
		args.note_ids
	} else {
		return Err(McpError::Invalid(
			"provide note_ids or set all: true".into(),
		));
	};
	session.document_mut().execute(Command::RemoveNotes {
		clip: args.clip_id,
		notes: note_ids,
	})?;
	Ok(())
}

fn clip_note_ids(
	session: &Session,
	clip_id: ClipId,
	note_ids: &[NoteId],
) -> Result<Vec<NoteId>, McpError> {
	let (_, clip) = session
		.document()
		.project()
		.find_clip(clip_id)
		.ok_or(ModelError::ClipNotFound(clip_id))?;
	if note_ids.is_empty() {
		Ok(clip.notes().iter().map(|n| n.id).collect())
	} else {
		Ok(note_ids.to_vec())
	}
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TransposeNotesArgs {
	pub clip_id: ClipId,
	/// Notes to transpose; omit to transpose every note in the clip.
	#[serde(default)]
	pub note_ids: Vec<NoteId>,
	pub semitones: i32,
}

pub fn transpose_notes(session: &mut Session, args: TransposeNotesArgs) -> Result<(), McpError> {
	let note_ids = clip_note_ids(session, args.clip_id, &args.note_ids)?;
	let project = session.document().project();
	let (_, clip) = project
		.find_clip(args.clip_id)
		.ok_or(ModelError::ClipNotFound(args.clip_id))?;
	let mut patches = Vec::with_capacity(note_ids.len());
	for id in note_ids {
		let note = clip
			.notes()
			.iter()
			.find(|n| n.id == id)
			.ok_or(ModelError::NoteNotFound {
				clip: args.clip_id,
				note: id,
			})?;
		let moved = note.pitch as i32 + args.semitones;
		if !(0..=127).contains(&moved) {
			return Err(McpError::Invalid(format!(
				"transposing note {id} by {} semitones would put its pitch at {moved}, outside 0..=127",
				args.semitones
			)));
		}
		patches.push(NotePatch {
			id,
			pitch: Some(moved as u8),
			velocity: None,
			start: None,
			length: None,
		});
	}
	session.document_mut().execute(Command::UpdateNotes {
		clip: args.clip_id,
		notes: patches,
	})?;
	Ok(())
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct QuantizeNotesArgs {
	pub clip_id: ClipId,
	/// Grid size in beats, e.g. 0.25 for sixteenth notes.
	pub grid_beats: f64,
	/// 0 leaves notes untouched, 1 snaps them fully onto the grid.
	pub strength: f64,
}

pub fn quantize_notes(session: &mut Session, args: QuantizeNotesArgs) -> Result<(), McpError> {
	if args.grid_beats <= 0.0 {
		return Err(McpError::Invalid("grid_beats must be > 0".into()));
	}
	let strength = args.strength.clamp(0.0, 1.0);
	let project = session.document().project();
	let (_, clip) = project
		.find_clip(args.clip_id)
		.ok_or(ModelError::ClipNotFound(args.clip_id))?;
	let mut patches = Vec::with_capacity(clip.notes().len());
	for note in clip.notes() {
		let start = tick_to_beats(note.start);
		let target = (start / args.grid_beats).round() * args.grid_beats;
		let new_start = start + (target - start) * strength;
		patches.push(NotePatch {
			id: note.id,
			pitch: None,
			velocity: None,
			start: Some(beats_to_tick(new_start.max(0.0))),
			length: None,
		});
	}
	session.document_mut().execute(Command::UpdateNotes {
		clip: args.clip_id,
		notes: patches,
	})?;
	Ok(())
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct HumanizeNotesArgs {
	pub clip_id: ClipId,
	/// Maximum random timing offset, in beats.
	pub timing_beats: f64,
	/// Maximum random velocity offset (0..=1).
	pub velocity: f32,
	/// Seed for the deterministic pseudo-random generator: the same seed always
	/// produces the same result.
	pub seed: u64,
}

/// Deterministic splitmix64 PRNG; no external dependency needed for this.
struct Rng(u64);

impl Rng {
	fn new(seed: u64) -> Self {
		Self(seed)
	}

	fn next_u64(&mut self) -> u64 {
		self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
		let mut z = self.0;
		z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
		z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
		z ^ (z >> 31)
	}

	/// `-1.0..=1.0`.
	fn bipolar(&mut self) -> f64 {
		let unit = (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64;
		unit * 2.0 - 1.0
	}
}

pub fn humanize_notes(session: &mut Session, args: HumanizeNotesArgs) -> Result<(), McpError> {
	let project = session.document().project();
	let (_, clip) = project
		.find_clip(args.clip_id)
		.ok_or(ModelError::ClipNotFound(args.clip_id))?;
	let mut rng = Rng::new(args.seed);
	let mut patches = Vec::with_capacity(clip.notes().len());
	for note in clip.notes() {
		let start = tick_to_beats(note.start);
		let new_start = (start + rng.bipolar() * args.timing_beats).max(0.0);
		let new_velocity =
			(note.velocity as f64 + rng.bipolar() * args.velocity as f64).clamp(0.0, 1.0);
		patches.push(NotePatch {
			id: note.id,
			pitch: None,
			velocity: Some(new_velocity as f32),
			start: Some(beats_to_tick(new_start)),
			length: None,
		});
	}
	session.document_mut().execute(Command::UpdateNotes {
		clip: args.clip_id,
		notes: patches,
	})?;
	Ok(())
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct MoveClipArgs {
	pub clip_id: ClipId,
	#[serde(default)]
	pub start_beat: Option<f64>,
	#[serde(default)]
	pub track_id: Option<TrackId>,
}

pub fn move_clip(session: &mut Session, args: MoveClipArgs) -> Result<(), McpError> {
	if args.start_beat.is_none() && args.track_id.is_none() {
		return Err(McpError::Invalid(
			"provide start_beat and/or track_id".into(),
		));
	}
	let patch = ClipPatch {
		name: None,
		start: args.start_beat.map(beats_to_tick),
		length: None,
		track: args.track_id,
		take: None,
	};
	session.document_mut().execute(Command::UpdateClip {
		clip: args.clip_id,
		patch,
	})?;
	Ok(())
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ResizeClipArgs {
	pub clip_id: ClipId,
	pub length_beats: f64,
}

pub fn resize_clip(session: &mut Session, args: ResizeClipArgs) -> Result<(), McpError> {
	let patch = ClipPatch {
		name: None,
		start: None,
		length: Some(beats_to_tick(args.length_beats)),
		track: None,
		take: None,
	};
	session.document_mut().execute(Command::UpdateClip {
		clip: args.clip_id,
		patch,
	})?;
	Ok(())
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DeleteClipArgs {
	pub clip_id: ClipId,
}

pub fn delete_clip(session: &mut Session, args: DeleteClipArgs) -> Result<(), McpError> {
	session
		.document_mut()
		.execute(Command::RemoveClip { clip: args.clip_id })?;
	Ok(())
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DuplicateClipArgs {
	pub clip_id: ClipId,
	pub to_beat: f64,
	/// Defaults to the source clip's track.
	#[serde(default)]
	pub track_id: Option<TrackId>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct DuplicateClipOutput {
	pub clip_id: ClipId,
}

pub fn duplicate_clip(
	session: &mut Session,
	args: DuplicateClipArgs,
) -> Result<DuplicateClipOutput, McpError> {
	let project = session.document().project();
	let (source_track, source_clip) = project
		.find_clip(args.clip_id)
		.ok_or(ModelError::ClipNotFound(args.clip_id))?;
	let target_track = args.track_id.unwrap_or(source_track.id);
	let mut clip = Clip::midi(
		source_clip.name.clone(),
		beats_to_tick(args.to_beat),
		source_clip.length,
	);
	match &source_clip.content {
		cadenza_model::ClipContent::Midi { notes, .. } => {
			clip = clip.with_notes(
				notes
					.iter()
					.map(|n| Note::new(n.pitch, n.start, n.length, n.velocity)),
			);
		}
		audio @ cadenza_model::ClipContent::Audio { .. } => {
			clip.content = audio.clone();
		}
	}
	let clip_id = clip.id;
	session.document_mut().execute(Command::AddClip {
		track: target_track,
		clip,
	})?;
	Ok(DuplicateClipOutput { clip_id })
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

	fn note_input(pitch: &str, start: f64, length: f64) -> NoteInput {
		NoteInput {
			pitch: PitchInput(cadenza_theory::parse_pitch(pitch).unwrap()),
			start,
			length,
			velocity: Some(0.8),
		}
	}

	#[test]
	fn duplicate_clip_gets_fresh_clip_and_note_ids() {
		let mut session = Session::new();
		let track_id = instrument_track(&mut session);
		let clip_id = add_midi_clip(
			&mut session,
			AddMidiClipArgs {
				track_id,
				start_beat: 0.0,
				length_beats: 4.0,
				name: None,
				notes: vec![note_input("C4", 0.0, 1.0), note_input("E4", 1.0, 1.0)],
			},
		)
		.unwrap()
		.clip_id;

		let dup = duplicate_clip(
			&mut session,
			DuplicateClipArgs {
				clip_id,
				to_beat: 8.0,
				track_id: None,
			},
		)
		.unwrap()
		.clip_id;
		assert_ne!(dup, clip_id);

		let original_notes = get_notes(&session, clip_id);
		let dup_notes = get_notes(&session, dup);
		assert_eq!(original_notes.len(), dup_notes.len());
		for (o, d) in original_notes.iter().zip(&dup_notes) {
			assert_ne!(o.id, d.id, "duplicated notes must have fresh ids");
			assert_eq!(o.pitch, d.pitch);
		}
	}

	fn get_notes(session: &Session, clip_id: ClipId) -> Vec<Note> {
		session
			.document()
			.project()
			.find_clip(clip_id)
			.unwrap()
			.1
			.notes()
			.to_vec()
	}

	#[test]
	fn humanize_is_deterministic_for_a_given_seed() {
		let mut session_a = Session::new();
		let track_a = instrument_track(&mut session_a);
		let clip_a = add_midi_clip(
			&mut session_a,
			AddMidiClipArgs {
				track_id: track_a,
				start_beat: 0.0,
				length_beats: 4.0,
				name: None,
				notes: vec![note_input("C4", 0.0, 1.0), note_input("D4", 1.0, 1.0)],
			},
		)
		.unwrap()
		.clip_id;
		humanize_notes(
			&mut session_a,
			HumanizeNotesArgs {
				clip_id: clip_a,
				timing_beats: 0.05,
				velocity: 0.1,
				seed: 42,
			},
		)
		.unwrap();
		let notes_a = get_notes(&session_a, clip_a);

		let mut session_b = Session::new();
		let track_b = instrument_track(&mut session_b);
		let clip_b = add_midi_clip(
			&mut session_b,
			AddMidiClipArgs {
				track_id: track_b,
				start_beat: 0.0,
				length_beats: 4.0,
				name: None,
				notes: vec![note_input("C4", 0.0, 1.0), note_input("D4", 1.0, 1.0)],
			},
		)
		.unwrap()
		.clip_id;
		humanize_notes(
			&mut session_b,
			HumanizeNotesArgs {
				clip_id: clip_b,
				timing_beats: 0.05,
				velocity: 0.1,
				seed: 42,
			},
		)
		.unwrap();
		let notes_b = get_notes(&session_b, clip_b);

		assert_eq!(notes_a.len(), notes_b.len());
		for (a, b) in notes_a.iter().zip(&notes_b) {
			assert_eq!(a.start, b.start);
			assert_eq!(a.velocity, b.velocity);
		}
		// Humanize actually perturbed something (seed 42 is not a degenerate case).
		assert!(
			notes_a
				.iter()
				.any(|n| n.start != cadenza_core::Tick::ZERO && n.start.beats() != 1.0)
				|| notes_a[0].velocity != 0.8
		);
	}

	#[test]
	fn quantize_strength_one_snaps_exactly_onto_the_grid() {
		let mut session = Session::new();
		let track_id = instrument_track(&mut session);
		let clip_id = add_midi_clip(
			&mut session,
			AddMidiClipArgs {
				track_id,
				start_beat: 0.0,
				length_beats: 4.0,
				name: None,
				notes: vec![note_input("C4", 0.1, 1.0)],
			},
		)
		.unwrap()
		.clip_id;
		quantize_notes(
			&mut session,
			QuantizeNotesArgs {
				clip_id,
				grid_beats: 0.25,
				strength: 1.0,
			},
		)
		.unwrap();
		let notes = get_notes(&session, clip_id);
		assert_eq!(notes[0].start, cadenza_core::Tick::ZERO);
	}

	#[test]
	fn transpose_rejects_out_of_range_pitch() {
		let mut session = Session::new();
		let track_id = instrument_track(&mut session);
		let clip_id = add_midi_clip(
			&mut session,
			AddMidiClipArgs {
				track_id,
				start_beat: 0.0,
				length_beats: 4.0,
				name: None,
				notes: vec![note_input("G9", 0.0, 1.0)],
			},
		)
		.unwrap()
		.clip_id;
		let err = transpose_notes(
			&mut session,
			TransposeNotesArgs {
				clip_id,
				note_ids: Vec::new(),
				semitones: 10,
			},
		)
		.unwrap_err();
		assert!(matches!(err, McpError::Invalid(_)), "{err}");
	}
}
