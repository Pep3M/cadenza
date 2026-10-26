//! MIDI clips and notes: creation, editing and the humanize/quantize helpers.

use cadenza_core::Tick;
use cadenza_core::{ClipId, NoteId, TrackId};
use cadenza_model::{
	Clip, ClipPatch, Command, ModelError, Note, NoteBendPoint, NoteExpression, NotePatch,
	NoteValuePoint,
};
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
	/// The note's own pitch curve (per-note bend: guitar bends, slides, vibrato on one note
	/// of a chord): points joined by straight lines, e.g. [{at_beats: 0, semitones: 0},
	/// {at_beats: 0.25, semitones: 2}] bends a whole tone up over a sixteenth. Played by the
	/// built-in synths and CLAP plugins with note expressions.
	#[serde(default)]
	pub bend: Option<Vec<BendInput>>,
	/// The note's own pressure, 0..1 (MPE pressure / polyphonic aftertouch: swells,
	/// accents inside a held note), points joined by straight lines; none = 0. The
	/// built-in synths open their filter with it.
	#[serde(default)]
	pub pressure: Option<Vec<ValueInput>>,
	/// The note's own timbre, 0..1 (MPE CC74 "slide": brightness), points joined by straight
	/// lines; none = 0.5 (neutral). The built-in synths move their filter with it.
	#[serde(default)]
	pub timbre: Option<Vec<ValueInput>>,
}

/// A point of a note's pressure or timbre curve.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, JsonSchema)]
pub struct ValueInput {
	/// Beats from the note's start.
	pub at_beats: f64,
	/// 0..1.
	pub value: f32,
}

/// A point of a note's bend.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, JsonSchema)]
pub struct BendInput {
	/// Beats from the note's start.
	pub at_beats: f64,
	/// -48..48.
	pub semitones: f32,
}

/// Steps a straight segment of a bend is written in, ticks (1/64 beat).
const BEND_STEP_TICKS: i64 = cadenza_core::PPQ / 64;

/// Bend points joined by straight lines, as the model's held points: one every
/// [`BEND_STEP_TICKS`] along each segment where the value moves at least a cent.
pub fn bend_points(points: &[BendInput]) -> Result<Vec<NoteBendPoint>, McpError> {
	let mut sorted: Vec<BendInput> = points.to_vec();
	if sorted
		.iter()
		.any(|p| !(p.at_beats.is_finite() && p.at_beats >= 0.0 && p.semitones.is_finite()))
	{
		return Err(McpError::Invalid(
			"bend points need at_beats >= 0 (from the note's start) and finite semitones".into(),
		));
	}
	sorted.sort_by(|a, b| a.at_beats.total_cmp(&b.at_beats));
	let mut out: Vec<NoteBendPoint> = Vec::new();
	let mut push = |tick: Tick, semitones: f32| match out.last_mut() {
		Some(last) if last.tick >= tick => last.semitones = semitones,
		Some(last) if (last.semitones - semitones).abs() < 0.01 => {}
		_ => out.push(NoteBendPoint { tick, semitones }),
	};
	for (i, p) in sorted.iter().enumerate() {
		let tick = beats_to_tick(p.at_beats);
		if let Some(prev) = i.checked_sub(1).map(|k| sorted[k]) {
			let from = beats_to_tick(prev.at_beats);
			let mut t = from.0 + BEND_STEP_TICKS;
			while t < tick.0 {
				let u = (t - from.0) as f32 / (tick.0 - from.0).max(1) as f32;
				push(Tick(t), prev.semitones + u * (p.semitones - prev.semitones));
				t += BEND_STEP_TICKS;
			}
		}
		push(tick, p.semitones);
	}
	// A curve that never leaves 0 is no bend.
	if out.iter().all(|p| p.semitones == 0.0) {
		out.clear();
	}
	Ok(out)
}

/// Pressure or timbre points joined by straight lines, as the model's held points (like
/// [`bend_points`]); a curve that never leaves `kind`'s resting value is none.
pub fn value_points(
	points: &[ValueInput],
	kind: NoteExpression,
) -> Result<Vec<NoteValuePoint>, McpError> {
	let mut sorted: Vec<ValueInput> = points.to_vec();
	if sorted
		.iter()
		.any(|p| !(p.at_beats.is_finite() && p.at_beats >= 0.0 && (0.0..=1.0).contains(&p.value)))
	{
		return Err(McpError::Invalid(format!(
			"{} points need at_beats >= 0 (from the note's start) and a value in 0..1",
			kind.name()
		)));
	}
	sorted.sort_by(|a, b| a.at_beats.total_cmp(&b.at_beats));
	let mut out: Vec<NoteValuePoint> = Vec::new();
	let mut push = |tick: Tick, value: f32| match out.last_mut() {
		Some(last) if last.tick >= tick => last.value = value,
		Some(last) if (last.value - value).abs() < 0.005 => {}
		_ => out.push(NoteValuePoint { tick, value }),
	};
	for (i, p) in sorted.iter().enumerate() {
		let tick = beats_to_tick(p.at_beats);
		if let Some(prev) = i.checked_sub(1).map(|k| sorted[k]) {
			let from = beats_to_tick(prev.at_beats);
			let mut t = from.0 + BEND_STEP_TICKS;
			while t < tick.0 {
				let u = (t - from.0) as f32 / (tick.0 - from.0).max(1) as f32;
				push(Tick(t), prev.value + u * (p.value - prev.value));
				t += BEND_STEP_TICKS;
			}
		}
		push(tick, p.value);
	}
	if out.iter().all(|p| p.value == kind.resting()) {
		out.clear();
	}
	Ok(out)
}

/// A pressure or timbre curve's shape for reading: first and last points and every turn
/// (at most 24).
pub fn value_shape(points: &[NoteValuePoint]) -> Vec<ValueInput> {
	let bend: Vec<NoteBendPoint> = points
		.iter()
		.map(|p| NoteBendPoint {
			tick: p.tick,
			semitones: p.value,
		})
		.collect();
	bend_shape(&bend)
		.into_iter()
		.map(|b| ValueInput {
			at_beats: b.at_beats,
			value: b.semitones,
		})
		.collect()
}

/// A bend's shape for reading: its first and last points and every turn (at most 24).
pub fn bend_shape(bend: &[NoteBendPoint]) -> Vec<BendInput> {
	let mut keep: Vec<&NoteBendPoint> = Vec::new();
	for (i, p) in bend.iter().enumerate() {
		let turn = match (i.checked_sub(1).map(|k| &bend[k]), bend.get(i + 1)) {
			(Some(a), Some(b)) => (p.semitones - a.semitones) * (b.semitones - p.semitones) <= 0.0,
			_ => true,
		};
		if turn {
			keep.push(p);
		}
	}
	if keep.len() > 24 {
		let step = keep.len() as f64 / 23.0;
		let last = keep[keep.len() - 1];
		keep = (0..23).map(|i| keep[(i as f64 * step) as usize]).collect();
		keep.push(last);
	}
	keep.iter()
		.map(|p| BendInput {
			at_beats: (tick_to_beats(p.tick) * 1000.0).round() / 1000.0,
			semitones: (p.semitones * 100.0).round() / 100.0,
		})
		.collect()
}

impl NoteInput {
	fn into_note(self) -> Result<Note, McpError> {
		let mut note = Note::new(
			self.pitch.0,
			beats_to_tick(self.start),
			beats_to_tick(self.length),
			self.velocity.unwrap_or(0.8),
		);
		if let Some(bend) = &self.bend {
			note.bend = bend_points(bend)?;
		}
		if let Some(points) = &self.pressure {
			note.pressure = value_points(points, NoteExpression::Pressure)?;
		}
		if let Some(points) = &self.timbre {
			note.timbre = value_points(points, NoteExpression::Timbre)?;
		}
		Ok(note)
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
	let notes: Vec<Note> = args
		.notes
		.into_iter()
		.map(NoteInput::into_note)
		.collect::<Result<_, _>>()?;
	let clip = Clip::midi(
		args.name.unwrap_or_else(|| "Clip".to_owned()),
		beats_to_tick(args.start_beat),
		beats_to_tick(args.length_beats),
	)
	.with_notes(notes);
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
	let notes: Vec<Note> = args
		.notes
		.into_iter()
		.map(NoteInput::into_note)
		.collect::<Result<_, _>>()?;
	let note_ids = notes.iter().map(|n| n.id).collect();
	session.document_mut().execute(Command::AddNotes {
		clip: args.clip_id,
		notes,
	})?;
	Ok(AddNotesOutput { note_ids })
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SetNoteBendArgs {
	pub clip_id: ClipId,
	pub note_id: NoteId,
	/// The note's new pitch curve (replaces the old one): points joined by straight lines,
	/// beats from the note's start. Empty = straight.
	pub points: Vec<BendInput>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct SetNoteBendOutput {
	/// The curve as stored, by its turns.
	pub bend: Vec<BendInput>,
}

/// MCP `set_note_bend`: one note's own pitch curve, one undo step.
pub fn set_note_bend(
	session: &mut Session,
	args: SetNoteBendArgs,
) -> Result<SetNoteBendOutput, McpError> {
	let bend = bend_points(&args.points)?;
	let shape = bend_shape(&bend);
	session.document_mut().execute(Command::UpdateNotes {
		clip: args.clip_id,
		notes: vec![NotePatch {
			id: args.note_id,
			pitch: None,
			velocity: None,
			start: None,
			length: None,
			bend: Some(bend),
			pressure: None,
			timbre: None,
		}],
	})?;
	Ok(SetNoteBendOutput { bend: shape })
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SetNoteExpressionArgs {
	pub clip_id: ClipId,
	pub note_id: NoteId,
	/// pressure (0..1, none = 0) or timbre (0..1, none = 0.5).
	pub kind: NoteExpression,
	/// The new curve (replaces the old one): points joined by straight lines, beats from the
	/// note's start. Empty = none.
	pub points: Vec<ValueInput>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct SetNoteExpressionOutput {
	/// The curve as stored, by its turns.
	pub points: Vec<ValueInput>,
}

/// MCP `set_note_expression`: one note's own pressure or timbre curve, one undo step.
pub fn set_note_expression(
	session: &mut Session,
	args: SetNoteExpressionArgs,
) -> Result<SetNoteExpressionOutput, McpError> {
	let points = value_points(&args.points, args.kind)?;
	let shape = value_shape(&points);
	let (pressure, timbre) = match args.kind {
		NoteExpression::Pressure => (Some(points), None),
		NoteExpression::Timbre => (None, Some(points)),
	};
	session.document_mut().execute(Command::UpdateNotes {
		clip: args.clip_id,
		notes: vec![NotePatch {
			id: args.note_id,
			pitch: None,
			velocity: None,
			start: None,
			length: None,
			bend: None,
			pressure,
			timbre,
		}],
	})?;
	Ok(SetNoteExpressionOutput { points: shape })
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
			bend: None,
			pressure: None,
			timbre: None,
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
			bend: None,
			pressure: None,
			timbre: None,
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
			bend: None,
			pressure: None,
			timbre: None,
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
		muted: None,
		link: None,
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
		muted: None,
		link: None,
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
	/// MIDI only: make a linked copy (like an FL pattern instance) that shares its notes
	/// and controller lanes with the source, so editing either edits both. Default false:
	/// an independent copy. Use linked copies for repeated sections that must stay equal.
	#[serde(default)]
	pub linked: bool,
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
	if args.linked {
		if !source_clip.is_midi() {
			return Err(McpError::Invalid(
				"linked: true works only for MIDI clips; duplicate audio clips with linked: false"
					.into(),
			));
		}
		let clip_id = ClipId::new();
		let commands = cadenza_model::links::copy_clip(
			project,
			args.clip_id,
			clip_id,
			Some(target_track),
			beats_to_tick(args.to_beat),
			true,
			cadenza_core::LinkId::new(),
		)
		.ok_or(ModelError::ClipNotFound(args.clip_id))?;
		session.document_mut().execute(Command::Batch {
			label: "Duplicate linked clip".into(),
			commands,
		})?;
		return Ok(DuplicateClipOutput { clip_id });
	}
	let mut clip = Clip::midi(
		source_clip.name.clone(),
		beats_to_tick(args.to_beat),
		source_clip.length,
	);
	match &source_clip.content {
		cadenza_model::ClipContent::Midi { notes, .. } => {
			clip = clip.with_notes(notes.iter().map(|n| Note {
				id: NoteId::new(),
				..n.clone()
			}));
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
			bend: None,
			pressure: None,
			timbre: None,
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
				linked: false,
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

	#[test]
	fn linked_duplicate_shares_note_edits() {
		let mut session = Session::new();
		let track_id = instrument_track(&mut session);
		let clip_id = add_midi_clip(
			&mut session,
			AddMidiClipArgs {
				track_id,
				start_beat: 0.0,
				length_beats: 4.0,
				name: None,
				notes: vec![note_input("C4", 0.0, 1.0)],
			},
		)
		.unwrap()
		.clip_id;
		let dup = duplicate_clip(
			&mut session,
			DuplicateClipArgs {
				clip_id,
				to_beat: 4.0,
				track_id: None,
				linked: true,
			},
		)
		.unwrap()
		.clip_id;
		session
			.document_mut()
			.execute(Command::AddNotes {
				clip: dup,
				notes: vec![Note::new(
					64,
					cadenza_core::Tick(960),
					cadenza_core::Tick(960),
					0.7,
				)],
			})
			.unwrap();
		assert_eq!(get_notes(&session, clip_id), get_notes(&session, dup));
		let summary = session.document().project().summary();
		assert_eq!(summary.tracks[0].clips[0].link, Some(vec![dup]));
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
