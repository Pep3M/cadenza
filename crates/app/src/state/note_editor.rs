//! The note editor (NE-6): the "Notes" mode of the audio editor. View state (tool,
//! selection, pitch range, key, macro settings), the analyses it draws from (pitch track
//! per file, loaded on a worker and kept), and the pure gesture/macro -> [`Command`]
//! builders. Every gesture is one undo step; the edited sound reaches the player through
//! the background renders (`state::note_renders`).

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use cadenza_core::{AudioNoteId, ClipId};
use cadenza_model::{
	AudioNote, AudioNotePatch, ClipContent, Command, NoteEdits, NoteSpan, Project,
};
use cadenza_notes::analysis::Analysis;
use cadenza_notes::tuning::{self, ScaleFit, Target, TargetOptions, TuningNote};
use cadenza_theory::Scale;

use super::audio_edit::{self, Region};
use crate::audio::Waker;
use crate::i18n::mark;
use crate::jobs::Job;

/// What the audio editor shows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum EditorMode {
	#[default]
	Waveform,
	Notes,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum NoteTool {
	/// Pitch ⇕ (snapped unless Alt), time ⇔; double-click splits / merges.
	#[default]
	Main,
	/// Vibrato and other fast pitch movement: share kept.
	Modulation,
	/// Slow pitch wander: share kept.
	Drift,
	Formant,
	Amplitude,
	/// Click splits a note, double-click on a boundary merges.
	Separation,
}

impl NoteTool {
	pub const ALL: [NoteTool; 6] = [
		NoteTool::Main,
		NoteTool::Modulation,
		NoteTool::Drift,
		NoteTool::Formant,
		NoteTool::Amplitude,
		NoteTool::Separation,
	];

	pub fn label(self) -> &'static str {
		match self {
			NoteTool::Main => mark("Main"),
			NoteTool::Modulation => mark("Vibrato"),
			NoteTool::Drift => mark("Drift"),
			NoteTool::Formant => mark("Formant"),
			NoteTool::Amplitude => mark("Level"),
			NoteTool::Separation => mark("Split"),
		}
	}

	pub fn glyph(self) -> &'static str {
		match self {
			NoteTool::Main => "↕",
			NoteTool::Modulation => "~",
			NoteTool::Drift => "≈",
			NoteTool::Formant => "◐",
			NoteTool::Amplitude => "▮",
			NoteTool::Separation => "✂",
		}
	}

	/// Shortcut key (1..6).
	pub fn key(self) -> &'static str {
		match self {
			NoteTool::Main => "1",
			NoteTool::Modulation => "2",
			NoteTool::Drift => "3",
			NoteTool::Formant => "4",
			NoteTool::Amplitude => "5",
			NoteTool::Separation => "6",
		}
	}

	pub fn tip(self) -> &'static str {
		match self {
			NoteTool::Main => mark(
				"Drag a note up/down to retune it (snaps to the key; Alt = free), sideways to move it; double-click splits",
			),
			NoteTool::Modulation => {
				mark("Drag up/down to deepen or flatten the vibrato; double-click resets")
			}
			NoteTool::Drift => {
				mark("Drag up/down to keep or straighten the slow pitch drift; double-click resets")
			}
			NoteTool::Formant => mark(
				"Drag up/down to change the voice's colour without changing the pitch; double-click resets",
			),
			NoteTool::Amplitude => mark("Drag up/down to change the level; double-click resets"),
			NoteTool::Separation => {
				mark("Click a note to split it there; double-click a boundary to merge two notes")
			}
		}
	}
}

/// The scale notes snap to.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum KeyChoice {
	/// Nearest semitone.
	Chromatic,
	/// The scale and the singer's offset fitted to the clip's notes (NE-7a).
	#[default]
	Detected,
	Scale(Scale),
}

/// View state of the note editor.
#[derive(Debug, Clone, PartialEq)]
pub struct NoteEditorView {
	pub tool: NoteTool,
	pub selection: BTreeSet<AudioNoteId>,
	/// Visible pitch range, fractional MIDI (bottom..top).
	pub low: f64,
	pub high: f64,
	/// Clip the pitch range was last fitted to.
	pub framed: Option<ClipId>,
	pub key: KeyChoice,
	/// Snap pitch moves to the key (Alt inverts while dragging).
	pub snap: bool,
	/// Correct Pitch: share of the way to the tone (0..1), share of the drift straightened,
	/// notes closer than the threshold (cents) are left alone.
	pub correct_center: f32,
	pub correct_drift: f32,
	pub correct_threshold: f32,
	/// Quantize Time: grid (beats) and strength (0..1).
	pub quantize_grid: f64,
	pub quantize_strength: f32,
	/// Leveling: share of the level differences removed (0..1).
	pub leveling: f32,
	/// Detection sensitivity for "Detect again".
	pub sensitivity: f32,
	/// Show the macros column.
	pub macros: bool,
	/// Show the polyphonic detector's notes behind (read only).
	pub chords: bool,
}

impl Default for NoteEditorView {
	fn default() -> Self {
		Self {
			tool: NoteTool::Main,
			selection: BTreeSet::new(),
			low: 48.0,
			high: 72.0,
			framed: None,
			key: KeyChoice::Detected,
			snap: true,
			correct_center: 1.0,
			correct_drift: 0.0,
			correct_threshold: 0.0,
			quantize_grid: 0.25,
			quantize_strength: 1.0,
			leveling: 0.5,
			sensitivity: 1.0,
			macros: true,
			chords: false,
		}
	}
}

impl NoteEditorView {
	/// Fits the pitch range to the notes (at least an octave, a margin of 3 semitones).
	pub fn frame(&mut self, edits: &NoteEdits) {
		let (lo, hi) = edits
			.notes
			.iter()
			.flat_map(|n| [n.center, n.result_pitch()])
			.fold((f64::MAX, f64::MIN), |(a, b), p| (a.min(p), b.max(p)));
		let (lo, hi) = if lo > hi { (55.0, 67.0) } else { (lo, hi) };
		let mid = 0.5 * (lo + hi);
		let half = (0.5 * (hi - lo) + 3.0).max(6.0);
		self.low = (mid - half).max(0.0);
		self.high = (mid + half).min(127.0);
	}

	/// Scrolls the pitch range by `semitones`, keeping it inside 0..127.
	pub fn scroll(&mut self, semitones: f64) {
		let span = self.high - self.low;
		self.low = (self.low + semitones).clamp(0.0, 127.0 - span);
		self.high = self.low + span;
	}

	/// Zooms the pitch range by `factor` (> 1 = more semitones) around `pitch`.
	pub fn zoom(&mut self, factor: f64, pitch: f64) {
		let span = ((self.high - self.low) * factor).clamp(6.0, 96.0);
		let t = ((pitch - self.low) / (self.high - self.low)).clamp(0.0, 1.0);
		self.low = (pitch - t * span).clamp(0.0, 127.0 - span);
		self.high = self.low + span;
	}

	/// Drops selected ids that are not notes of `edits` any more.
	pub fn prune(&mut self, edits: Option<&NoteEdits>) {
		match edits {
			Some(e) => self.selection.retain(|id| e.note(*id).is_some()),
			None => self.selection.clear(),
		}
	}

	/// The scale snapping uses (`None` = nearest semitone).
	pub fn scale(&self, edits: &NoteEdits) -> Option<Scale> {
		match &self.key {
			KeyChoice::Chromatic => None,
			KeyChoice::Detected => detected_fit(edits).map(|f| f.scale),
			KeyChoice::Scale(s) => Some(*s),
		}
	}
}

/// The notes of audio clip `clip` and its region.
pub fn clip_notes(project: &Project, clip: ClipId) -> Option<(NoteEdits, Region)> {
	let r = audio_edit::region(project, clip)?;
	let ClipContent::Audio { notes: Some(n), .. } = &r.clip.content else {
		return None;
	};
	Some(((**n).clone(), r))
}

fn tuning_notes(edits: &NoteEdits) -> Vec<TuningNote> {
	edits.notes.iter().map(TuningNote::from).collect()
}

/// The scale (any mode) and the singer's offset that explain the notes best.
pub fn detected_fit(edits: &NoteEdits) -> Option<ScaleFit> {
	tuning::fit_scale(&tuning_notes(edits), 1).first().copied()
}

/// The tone each note is tuned to under the detected scale, by note.
pub fn detected_targets(edits: &NoteEdits) -> Option<Vec<Target>> {
	let notes = tuning_notes(edits);
	let fit = tuning::fit_scale(&notes, 1).first().copied()?;
	Some(tuning::targets(&notes, &fit, &TargetOptions::default()))
}

/// The tone `pitch` snaps to.
pub fn snap(key: Option<&Scale>, pitch: f64) -> f64 {
	cadenza_notes::edit::snap_target(key, pitch.clamp(0.0, 127.0))
}

fn pitch_patch(n: &AudioNote, pitch: f64) -> AudioNotePatch {
	cadenza_mcp::tools::tuning::pitch_patch(n, pitch)
}

fn command(clip: ClipId, label: &str, patches: Vec<AudioNotePatch>) -> Option<Command> {
	(!patches.is_empty()).then(|| Command::Batch {
		label: label.into(),
		commands: vec![Command::UpdateAudioNotes {
			clip,
			patches,
			sibilant_gain_db: None,
		}],
	})
}

fn selected<'a>(
	edits: &'a NoteEdits,
	ids: &'a BTreeSet<AudioNoteId>,
) -> impl Iterator<Item = &'a AudioNote> {
	edits.notes.iter().filter(|n| ids.contains(&n.id))
}

/// Where each note lands when `ids` are dragged by `delta` semitones: snapped to the key
/// (`snap_to`) or moved freely.
pub fn moved_pitches(
	edits: &NoteEdits,
	ids: &BTreeSet<AudioNoteId>,
	delta: f64,
	snap_to: Option<Option<&Scale>>,
) -> Vec<(AudioNoteId, f64)> {
	selected(edits, ids)
		.map(|n| {
			let p = n.result_pitch() + delta;
			let p = match snap_to {
				Some(key) => snap(key, p),
				None => p,
			};
			(n.id, p)
		})
		.collect()
}

/// Retunes `ids` by `delta` semitones (see [`moved_pitches`]).
pub fn move_pitch(
	clip: ClipId,
	edits: &NoteEdits,
	ids: &BTreeSet<AudioNoteId>,
	delta: f64,
	snap_to: Option<Option<&Scale>>,
) -> Option<Command> {
	let patches = moved_pitches(edits, ids, delta, snap_to)
		.into_iter()
		.filter_map(|(id, p)| {
			let n = edits.note(id)?;
			((p - n.result_pitch()).abs() > 1e-9).then(|| pitch_patch(n, p))
		})
		.collect();
	command(clip, "Retune notes", patches)
}

/// The edit a vertical drag of `amount` (semitones of drag) makes with `tool`: the new
/// value of the tool's parameter for `n`.
pub fn tool_value(tool: NoteTool, n: &AudioNote, amount: f64) -> f32 {
	match tool {
		NoteTool::Modulation => (f64::from(n.modulation) + amount * 0.25).clamp(-1.0, 2.0) as f32,
		NoteTool::Drift => (f64::from(n.drift) + amount * 0.25).clamp(-1.0, 2.0) as f32,
		NoteTool::Formant => {
			(f64::from(n.formant_cents) + amount * 50.0).clamp(-1200.0, 1200.0) as f32
		}
		NoteTool::Amplitude => (f64::from(n.gain_db) + amount).clamp(-60.0, 24.0) as f32,
		NoteTool::Main | NoteTool::Separation => 0.0,
	}
}

/// Changes `tool`'s parameter of `ids` by a vertical drag of `amount` semitones, or resets
/// it (`amount = None`).
pub fn tool_edit(
	clip: ClipId,
	edits: &NoteEdits,
	ids: &BTreeSet<AudioNoteId>,
	tool: NoteTool,
	amount: Option<f64>,
) -> Option<Command> {
	let patches = selected(edits, ids)
		.filter_map(|n| {
			let v = amount.map_or(
				match tool {
					NoteTool::Modulation | NoteTool::Drift => 1.0,
					_ => 0.0,
				},
				|a| tool_value(tool, n, a),
			);
			let mut p = AudioNotePatch {
				id: n.id,
				..Default::default()
			};
			let current = match tool {
				NoteTool::Modulation => {
					p.modulation = Some(v);
					n.modulation
				}
				NoteTool::Drift => {
					p.drift = Some(v);
					n.drift
				}
				NoteTool::Formant => {
					p.formant_cents = Some(v);
					n.formant_cents
				}
				NoteTool::Amplitude => {
					p.gain_db = Some(v);
					n.gain_db
				}
				NoteTool::Main | NoteTool::Separation => return None,
			};
			((current - v).abs() > 1e-6).then_some(p)
		})
		.collect();
	let label = match (tool, amount) {
		(NoteTool::Modulation, _) => "Edit vibrato",
		(NoteTool::Drift, _) => "Edit pitch drift",
		(NoteTool::Formant, _) => "Edit formant",
		(NoteTool::Amplitude, _) => "Edit note level",
		_ => "Edit notes",
	};
	command(clip, label, patches)
}

/// Where a note sits in the output: its time edit, else where it was sung.
pub fn note_span(n: &AudioNote) -> (f64, f64) {
	n.time.map_or((n.start, n.end), |t| (t.start, t.end))
}

/// Moves `ids` by `delta` seconds; refuses moves that would cross a neighbour.
pub fn move_time(
	clip: ClipId,
	edits: &NoteEdits,
	ids: &BTreeSet<AudioNoteId>,
	delta: f64,
) -> Result<Option<Command>, String> {
	if delta.abs() < 1e-6 {
		return Ok(None);
	}
	let mut moved = edits.clone();
	let mut patches = Vec::new();
	for n in moved.notes.iter_mut().filter(|n| ids.contains(&n.id)) {
		let (a, b) = note_span(n);
		let span = NoteSpan {
			start: (a + delta).max(0.0),
			end: (b + delta).max(0.01),
		};
		let back = (span.start - n.start).abs() < 1e-6 && (span.end - n.end).abs() < 1e-6;
		let time = (!back).then_some(span);
		n.time = time;
		patches.push(AudioNotePatch {
			id: n.id,
			time: Some(time),
			..Default::default()
		});
	}
	check_times(&moved)?;
	Ok(command(clip, "Move notes", patches))
}

/// Errors when the time edits of `edits` cross (the resynthesis would refuse them).
fn check_times(edits: &NoteEdits) -> Result<(), String> {
	cadenza_notes::edit::time_map(
		&cadenza_notes::clip::to_segmentation(edits),
		&cadenza_notes::clip::to_edits(edits),
	)
	.map(|_| ())
	.map_err(|e| e.to_string())
}

/// Correct Pitch macro over `ids` (all notes when empty); the AI's tune_audio_notes runs
/// the same correction.
pub fn correct_pitch(
	clip: ClipId,
	edits: &NoteEdits,
	ids: &BTreeSet<AudioNoteId>,
	view: &NoteEditorView,
) -> Option<Command> {
	let key = view.scale(edits);
	let targets = (view.key == KeyChoice::Detected)
		.then(|| detected_targets(edits))
		.flatten();
	let tones: Vec<f64> = edits
		.notes
		.iter()
		.enumerate()
		.map(|(i, n)| {
			targets
				.as_ref()
				.and_then(|t| t.get(i))
				.map_or_else(|| snap(key.as_ref(), n.center), |t| t.pitch)
		})
		.collect();
	let correction = cadenza_mcp::tools::tuning::Correction {
		center: f64::from(view.correct_center),
		drift: f64::from(view.correct_drift),
		vibrato: None,
		threshold_cents: f64::from(view.correct_threshold),
	};
	let patches = cadenza_mcp::tools::tuning::correction_patches(edits, &tones, ids, &correction);
	command(clip, "Correct pitch", patches)
}

/// Quantize Time macro: note starts move `strength` of the way to the nearest grid line
/// (beats of the song) keeping their length; notes that would cross a neighbour stay.
pub fn quantize_time(
	project: &Project,
	clip: ClipId,
	edits: &NoteEdits,
	ids: &BTreeSet<AudioNoteId>,
	view: &NoteEditorView,
) -> Result<Option<Command>, String> {
	let Some(r) = audio_edit::region(project, clip) else {
		return Ok(None);
	};
	let grid = view.quantize_grid.max(1.0 / 64.0);
	let strength = f64::from(view.quantize_strength.clamp(0.0, 1.0));
	let tempo = &project.tempo;
	let mut moved = edits.clone();
	let mut patches = Vec::new();
	for i in 0..moved.notes.len() {
		let n = moved.notes[i].clone();
		if !(ids.is_empty() || ids.contains(&n.id)) {
			continue;
		}
		let (a, b) = note_span(&n);
		let song = r.song_start + a - r.offset;
		let beat = tempo.seconds_to_tick(song.max(0.0)).beats();
		let snapped =
			tempo.tick_to_seconds(cadenza_core::Tick::from_beats((beat / grid).round() * grid));
		let delta = strength * (snapped - song);
		if delta.abs() < 1e-4 {
			continue;
		}
		let span = NoteSpan {
			start: (a + delta).max(0.0),
			end: (b + delta).max(0.01),
		};
		let previous = moved.notes[i].time;
		moved.notes[i].time = Some(span);
		if check_times(&moved).is_err() {
			moved.notes[i].time = previous;
			continue;
		}
		patches.push(AudioNotePatch {
			id: n.id,
			time: Some(Some(span)),
			..Default::default()
		});
	}
	Ok(command(clip, "Quantize note timing", patches))
}

/// Mean level of each note's core, dB (the analysis' frame levels).
pub fn note_levels(edits: &NoteEdits, analysis: &Analysis) -> Vec<Option<f64>> {
	edits
		.notes
		.iter()
		.map(|n| {
			let v: Vec<f64> = analysis
				.frames
				.iter()
				.filter(|f| (n.core_start..=n.core_end).contains(&f.time))
				.map(|f| f.level_db)
				.collect();
			(!v.is_empty()).then(|| v.iter().sum::<f64>() / v.len() as f64)
		})
		.collect()
}

/// Leveling macro: each note's gain removes `view.leveling` of its distance from the mean
/// level (up to ±12 dB).
pub fn leveling(
	clip: ClipId,
	edits: &NoteEdits,
	analysis: &Analysis,
	ids: &BTreeSet<AudioNoteId>,
	view: &NoteEditorView,
) -> Option<Command> {
	let levels = note_levels(edits, analysis);
	let known: Vec<f64> = levels.iter().flatten().copied().collect();
	if known.is_empty() {
		return None;
	}
	let mean = known.iter().sum::<f64>() / known.len() as f64;
	let amount = f64::from(view.leveling.clamp(0.0, 1.0));
	let patches = edits
		.notes
		.iter()
		.zip(&levels)
		.filter(|(n, _)| ids.is_empty() || ids.contains(&n.id))
		.filter_map(|(n, l)| {
			let gain = (amount * (mean - (*l)?)).clamp(-12.0, 12.0) as f32;
			let gain = (gain * 10.0).round() / 10.0;
			((gain - n.gain_db).abs() > 0.05).then(|| AudioNotePatch {
				id: n.id,
				gain_db: Some(gain),
				..Default::default()
			})
		})
		.collect();
	command(clip, "Level notes", patches)
}

/// Every edit of `ids` back to as sung.
pub fn reset(clip: ClipId, edits: &NoteEdits, ids: &BTreeSet<AudioNoteId>) -> Option<Command> {
	let patches = selected(edits, ids)
		.filter(|n| n.is_edited() || n.pitch.is_some() || n.time.is_some())
		.map(|n| AudioNotePatch {
			id: n.id,
			pitch: n.pitch.is_some().then_some(None),
			drift: (n.drift != 1.0).then_some(1.0),
			modulation: (n.modulation != 1.0).then_some(1.0),
			formant_cents: (n.formant_cents != 0.0).then_some(0.0),
			gain_db: (n.gain_db != 0.0).then_some(0.0),
			time: n.time.is_some().then_some(None),
		})
		.filter(|p| !p.is_empty())
		.collect();
	command(clip, "Reset notes", patches)
}

/// Sibilant level (dB).
pub fn sibilant_gain(clip: ClipId, edits: &NoteEdits, db: f32) -> Option<Command> {
	let db = db.clamp(-60.0, 24.0);
	((db - edits.sibilant_gain_db).abs() > 0.01).then_some(Command::UpdateAudioNotes {
		clip,
		patches: Vec::new(),
		sibilant_gain_db: Some(db),
	})
}

/// The command replacing a clip's notes (detection, split, merge), labelled.
pub fn replace(clip: ClipId, label: &str, notes: NoteEdits) -> Command {
	Command::Batch {
		label: label.into(),
		commands: vec![Command::SetAudioNotes {
			clip,
			notes: Some(Box::new(notes)),
		}],
	}
}

/// The note under file second `s` at pitch `pitch` (within `tolerance` semitones of its
/// resulting pitch).
pub fn note_at(edits: &NoteEdits, s: f64, pitch: f64, tolerance: f64) -> Option<AudioNoteId> {
	edits
		.notes
		.iter()
		.filter(|n| {
			let (a, b) = note_span(n);
			(a..=b).contains(&s) && (n.result_pitch() - pitch).abs() <= tolerance
		})
		.min_by(|a, b| {
			(a.result_pitch() - pitch)
				.abs()
				.total_cmp(&(b.result_pitch() - pitch).abs())
		})
		.map(|n| n.id)
}

/// The note ending where the next one starts closest to `s` (within `tolerance` seconds):
/// the first of the pair a merge joins.
pub fn boundary_at(edits: &NoteEdits, s: f64, tolerance: f64) -> Option<AudioNoteId> {
	edits
		.notes
		.windows(2)
		.filter(|w| w[1].start - w[0].end <= 0.03)
		.map(|w| (w[0].id, (0.5 * (w[0].end + w[1].start) - s).abs()))
		.filter(|(_, d)| *d <= tolerance)
		.min_by(|a, b| a.1.total_cmp(&b.1))
		.map(|(id, _)| id)
}

/// Notes whose (output) span overlaps `a..b` seconds and whose pitch is inside `lo..hi`.
pub fn notes_in(
	edits: &NoteEdits,
	(a, b): (f64, f64),
	(lo, hi): (f64, f64),
) -> BTreeSet<AudioNoteId> {
	edits
		.notes
		.iter()
		.filter(|n| {
			let (s, e) = note_span(n);
			e >= a.min(b) && s <= a.max(b) && (lo.min(hi)..=lo.max(hi)).contains(&n.result_pitch())
		})
		.map(|n| n.id)
		.collect()
}

/// Analyses of audio files (pitch track + levels) for drawing and the detection, loaded on
/// a worker once per file.
#[derive(Default)]
pub struct NoteAnalyses {
	ready: HashMap<PathBuf, Arc<Analysis>>,
	failed: HashMap<PathBuf, String>,
	job: Option<(PathBuf, Job<Result<Analysis, String>>)>,
	/// Clips whose notes are detected as soon as their file's analysis is ready.
	pub detect: Vec<ClipId>,
}

impl NoteAnalyses {
	pub fn get(&self, file: &Path) -> Option<Arc<Analysis>> {
		self.ready.get(file).cloned()
	}

	pub fn failure(&self, file: &Path) -> Option<&str> {
		self.failed.get(file).map(String::as_str)
	}

	pub fn busy(&self) -> bool {
		self.job.is_some()
	}

	pub fn loading(&self, file: &Path) -> bool {
		self.job.as_ref().is_some_and(|(f, _)| f == file)
	}

	/// Starts analyzing `file` unless it is known or being analyzed.
	pub fn request(&mut self, file: &Path, cache: PathBuf, waker: Option<Waker>) {
		if self.ready.contains_key(file) || self.failed.contains_key(file) || self.job.is_some() {
			return;
		}
		let path = file.to_path_buf();
		let job = Job::spawn("note-analysis", waker, move || analyze_file(&path, &cache));
		self.job = Some((file.to_path_buf(), job));
	}

	/// The file whose analysis just finished, if any.
	pub fn poll(&mut self) -> Option<PathBuf> {
		let r = self.job.as_ref().and_then(|(_, j)| j.poll())?;
		let (file, _) = self.job.take().expect("a running analysis");
		match r {
			Some(Ok(a)) => {
				self.ready.insert(file.clone(), Arc::new(a));
			}
			Some(Err(e)) => {
				self.failed.insert(file.clone(), e);
			}
			None => {
				self.failed
					.insert(file.clone(), "the analysis stopped unexpectedly".into());
			}
		}
		Some(file)
	}

	/// For tests: an analysis made elsewhere.
	pub fn insert(&mut self, file: PathBuf, analysis: Analysis) {
		self.ready.insert(file, Arc::new(analysis));
	}
}

type PolyJob = Job<Result<Vec<cadenza_notes::poly::PolyNote>, String>>;

/// The notes the polyphonic detector hears in a file (file seconds), per file, made on a
/// worker once: the note editor's read-only "Chords" layer.
#[derive(Default)]
pub struct PolyLayers {
	ready: HashMap<PathBuf, Arc<Vec<cadenza_notes::poly::PolyNote>>>,
	failed: HashMap<PathBuf, String>,
	job: Option<(PathBuf, PolyJob)>,
}

impl PolyLayers {
	/// Whether `file` sounds chords a good part of the time (the polyphonic notes are shown
	/// instead of the single-voice ones); `None` until detected.
	pub fn polyphonic(&self, file: &Path) -> Option<bool> {
		self.get(file)
			.map(|p| cadenza_notes::poly::polyphony(&p) > cadenza_mcp::tools::notes::POLY_SHARE)
	}

	pub fn get(&self, file: &Path) -> Option<Arc<Vec<cadenza_notes::poly::PolyNote>>> {
		self.ready.get(file).cloned()
	}

	pub fn loading(&self, file: &Path) -> bool {
		self.job.as_ref().is_some_and(|(f, _)| f == file)
	}

	pub fn failure(&self, file: &Path) -> Option<&str> {
		self.failed.get(file).map(String::as_str)
	}

	pub fn busy(&self) -> bool {
		self.job.is_some()
	}

	/// Starts detecting `file` unless it is known or something runs.
	pub fn request(&mut self, file: &Path, waker: Option<Waker>) {
		if self.ready.contains_key(file) || self.failed.contains_key(file) || self.job.is_some() {
			return;
		}
		let path = file.to_path_buf();
		let job = Job::spawn("poly-notes", waker, move || poly_notes(&path));
		self.job = Some((file.to_path_buf(), job));
	}

	pub fn poll(&mut self) {
		let Some(r) = self.job.as_ref().and_then(|(_, j)| j.poll()) else {
			return;
		};
		let (file, _) = self.job.take().expect("a running detection");
		match r {
			Some(Ok(n)) => {
				self.ready.insert(file, Arc::new(n));
			}
			Some(Err(e)) => {
				self.failed.insert(file, e);
			}
			None => {
				self.failed
					.insert(file, "the detection stopped unexpectedly".into());
			}
		}
	}
}

/// The polyphonic notes of `file`'s mono mix.
pub fn poly_notes(file: &Path) -> Result<Vec<cadenza_notes::poly::PolyNote>, String> {
	let buffer = cadenza_audio_io::decode_file(file).map_err(|e| e.to_string())?;
	let channels: Vec<&[f32]> = buffer.channels().iter().map(Vec::as_slice).collect();
	let mono = cadenza_notes::resynth::mono(&channels);
	cadenza_notes::poly::transcribe(
		&mono,
		buffer.sample_rate(),
		&cadenza_notes::poly::PolyOptions::default(),
		&mut |_| true,
	)
	.map(|(_, n)| n)
	.ok_or_else(|| "the detection was cancelled".into())
}

/// Decodes `file` and analyzes its mono mix through the content cache in `cache`.
pub fn analyze_file(file: &Path, cache: &Path) -> Result<Analysis, String> {
	cadenza_mcp::tools::tuning::analyze_file(file, cache).map_err(|e| e.to_string())
}

/// The pitch each analysis frame ends up at with the edits (`None` = unvoiced), for the
/// curve drawn over the notes.
pub fn edited_curve(analysis: &Analysis, edits: &NoteEdits) -> Vec<Option<f64>> {
	let seg = cadenza_notes::clip::to_segmentation(edits);
	let e = cadenza_notes::clip::to_edits(edits);
	if e.validate(&seg).is_err() {
		return analysis.frames.iter().map(|f| f.pitch).collect();
	}
	let curves = cadenza_notes::edit::curves(analysis, &seg, &e);
	analysis
		.frames
		.iter()
		.zip(&curves.shift)
		.map(|(f, s)| f.pitch.map(|p| p + s))
		.collect()
}

impl super::AppState {
	fn note_cache(&self) -> PathBuf {
		self.analysis_cache
			.clone()
			.unwrap_or_else(cadenza_mcp::tools::notes::default_cache_dir)
	}

	/// Opens the audio editor on `clip` in Notes mode; detects the notes first when the
	/// clip has none (one undo step once the analysis is ready).
	pub fn open_note_editor(&mut self, clip: ClipId) -> bool {
		if !self.open_audio_editor(clip) {
			return false;
		}
		self.view.audio.mode = EditorMode::Notes;
		self.ensure_note_analysis(clip);
		true
	}

	/// Starts the analysis of `clip`'s file if it is not there yet, and queues the
	/// detection when the clip has no notes.
	pub fn ensure_note_analysis(&mut self, clip: ClipId) {
		let Some(r) = audio_edit::region(self.project(), clip) else {
			return;
		};
		let has_notes = matches!(&r.clip.content, ClipContent::Audio { notes: Some(_), .. });
		if !has_notes && !self.note_analyses.detect.contains(&clip) {
			self.note_analyses.detect.push(clip);
		}
		let cache = self.note_cache();
		let waker = self.waker.clone();
		self.note_analyses.request(&r.file, cache, waker);
	}

	/// Per frame: lands analyses, runs queued detections, keeps the open clip analyzed.
	pub(super) fn poll_note_analyses(&mut self) {
		self.poly_layers.poll();
		// Always for the open clip: a polyphonic recording is shown as its chords.
		if self.view.audio_editor
			&& self.view.audio.mode == EditorMode::Notes
			&& let Some(clip) = self.view.audio.clip
			&& let Some(r) = audio_edit::region(self.project(), clip)
		{
			let waker = self.waker.clone();
			self.poly_layers.request(&r.file, waker);
		}
		if self.note_analyses.poll().is_some() {
			let queued = std::mem::take(&mut self.note_analyses.detect);
			for clip in queued {
				match audio_edit::region(self.project(), clip) {
					Some(r) if self.note_analyses.get(&r.file).is_some() => {
						if !matches!(&r.clip.content, ClipContent::Audio { notes: Some(_), .. }) {
							self.detect_notes(clip, None);
						}
					}
					Some(r) if self.note_analyses.failure(&r.file).is_none() => {
						self.note_analyses.detect.push(clip);
					}
					Some(r) => {
						let e = self
							.note_analyses
							.failure(&r.file)
							.unwrap_or_default()
							.to_owned();
						self.notices.error(crate::i18n::trf(
							"Could not analyze the clip: {e}",
							&[("e", &e)],
						));
					}
					None => {}
				}
			}
		}
		if self.view.audio_editor
			&& self.view.audio.mode == EditorMode::Notes
			&& let Some(clip) = self.view.audio.clip
			&& let Some(r) = audio_edit::region(self.project(), clip)
			&& self.note_analyses.get(&r.file).is_none()
			&& self.note_analyses.failure(&r.file).is_none()
		{
			let cache = self.note_cache();
			let waker = self.waker.clone();
			self.note_analyses.request(&r.file, cache, waker);
		}
	}

	/// Detects the notes of `clip` (again: keeping the edits, splits and merges of what was
	/// there) with `sensitivity` (default: the clip's or the view's). One undo step.
	pub fn detect_notes(&mut self, clip: ClipId, sensitivity: Option<f32>) -> bool {
		let Some(r) = audio_edit::region(self.project(), clip) else {
			return false;
		};
		let Some(analysis) = self.note_analyses.get(&r.file) else {
			self.ensure_note_analysis(clip);
			return false;
		};
		let previous = match &r.clip.content {
			ClipContent::Audio { notes: Some(n), .. } => Some((**n).clone()),
			_ => None,
		};
		let sensitivity = sensitivity
			.or(previous.as_ref().map(|p| p.sensitivity))
			.unwrap_or(self.view.audio.notes.sensitivity)
			.clamp(0.1, 10.0);
		let found = cadenza_notes::clip::detect(
			&analysis,
			sensitivity,
			previous.as_ref(),
			&mut AudioNoteId::new,
		);
		let label = if previous.is_some() {
			"Detect notes again"
		} else {
			"Detect notes"
		};
		let ok = self.apply(replace(clip, label, found.clone()));
		if ok && previous.is_none() {
			self.view.audio.notes.frame(&found);
			self.view.audio.notes.framed = Some(clip);
		}
		ok
	}

	/// Splits note `id` of `clip` at file second `at`.
	pub fn split_audio_note(&mut self, clip: ClipId, id: AudioNoteId, at: f64) -> bool {
		self.reshape_notes(clip, "Split note", |a, e| {
			cadenza_notes::clip::split(a, e, id, at, &mut AudioNoteId::new)
		})
	}

	/// Merges note `id` of `clip` with the next one.
	pub fn merge_audio_notes(&mut self, clip: ClipId, id: AudioNoteId) -> bool {
		self.reshape_notes(clip, "Merge notes", |a, e| {
			cadenza_notes::clip::merge_with_next(a, e, id, &mut AudioNoteId::new)
		})
	}

	fn reshape_notes(
		&mut self,
		clip: ClipId,
		label: &str,
		f: impl FnOnce(&Analysis, &NoteEdits) -> Result<NoteEdits, cadenza_notes::NotesError>,
	) -> bool {
		let Some((edits, r)) = clip_notes(self.project(), clip) else {
			return false;
		};
		let Some(analysis) = self.note_analyses.get(&r.file) else {
			self.ensure_note_analysis(clip);
			return false;
		};
		match f(&analysis, &edits) {
			Ok(n) => self.apply(replace(clip, label, n)),
			Err(e) => {
				self.notices.warn(e.to_string());
				false
			}
		}
	}

	/// Removes the clip's notes and edits (it plays as recorded). One undo step.
	pub fn remove_audio_notes(&mut self, clip: ClipId) -> bool {
		self.view.audio.notes.selection.clear();
		self.apply(Command::SetAudioNotes { clip, notes: None })
	}

	/// "Tune with AI": the tuning engineer works on the notes of audio clip `clip` (detecting
	/// them first if needed), as one undo step "AI tuning pass"; the key chosen in the note
	/// editor is passed on. Opens the chat so the report shows. Returns whether it was sent.
	pub fn tune_with_ai(&mut self, clip: ClipId) -> bool {
		let Some(r) = audio_edit::region(self.project(), clip) else {
			return false;
		};
		if self.ai.busy() {
			return false;
		}
		let track = self
			.project()
			.find_clip(clip)
			.map(|(t, _)| t.name.clone())
			.unwrap_or_default();
		let described = format!(
			"\"{}\" (clip {}, track \"{track}\", beats {:.2}-{:.2}, audio)",
			r.clip.name,
			r.clip.id,
			r.clip.start.beats(),
			(r.clip.start + r.clip.length).beats()
		);
		let key = match &self.view.audio.notes.key {
			KeyChoice::Detected => None,
			KeyChoice::Chromatic => Some("nearest".to_owned()),
			KeyChoice::Scale(s) => Some(cadenza_mcp::tools::notes::scale_name(s)),
		};
		let brief = cadenza_agent::TuningBrief {
			clip: described,
			key,
			reference: None,
			notes: None,
		};
		let label = crate::i18n::trf("Tune '{clip}' with AI", &[("clip", &r.clip.name)]);
		let context = self.ai_context();
		let session = self.session.clone();
		self.begin_ai_changes(cadenza_agent::TUNING_UNDO_LABEL);
		let sent = self.ai.send_tuning_pass(&label, brief, context, &session);
		if sent {
			self.view.panels.set(super::PanelKind::Chat, true);
			self.mix.open = false;
		} else {
			self.ai_snapshot = None;
		}
		sent
	}

	/// A note command from the editor; errors become notices.
	pub fn note_edit(&mut self, cmd: Result<Option<Command>, String>) -> bool {
		match cmd {
			Ok(Some(cmd)) => self.apply(cmd),
			Ok(None) => false,
			Err(e) => {
				self.notices.warn(e);
				false
			}
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use cadenza_model::{Clip, Track, TrackKind};

	fn note(start: f64, end: f64, center: f64) -> AudioNote {
		AudioNote {
			core_start: start + 0.02,
			core_end: end - 0.02,
			..AudioNote::new(AudioNoteId::new(), start, end, center)
		}
	}

	fn edits() -> NoteEdits {
		NoteEdits {
			notes: vec![
				note(0.0, 0.5, 60.3),
				note(0.5, 1.0, 63.8),
				note(1.2, 1.6, 66.6),
			],
			sibilants: Vec::new(),
			sibilant_gain_db: 0.0,
			sensitivity: 1.0,
			user_splits: Vec::new(),
			user_merges: Vec::new(),
		}
	}

	fn project(e: &NoteEdits) -> (Project, ClipId) {
		let mut p = Project::new("t");
		let mut clip = Clip::audio(
			"vox",
			cadenza_core::Tick::ZERO,
			cadenza_core::Tick::from_beats(8.0),
			"/tmp/v.wav",
		);
		if let ClipContent::Audio { notes, .. } = &mut clip.content {
			*notes = Some(Box::new(e.clone()));
		}
		let id = clip.id;
		let mut t = Track::new("Vox", TrackKind::Audio);
		t.clips.push(clip);
		p.tracks.push(t);
		(p, id)
	}

	fn run(p: &mut Project, cmd: Command) -> NoteEdits {
		let mut doc = cadenza_model::Document::new(p.clone());
		doc.execute(cmd).unwrap();
		*p = doc.project().clone();
		let clip = p.tracks[0].clips[0].id;
		clip_notes(p, clip).unwrap().0
	}

	fn ids(e: &NoteEdits, which: &[usize]) -> BTreeSet<AudioNoteId> {
		which.iter().map(|i| e.notes[*i].id).collect()
	}

	#[test]
	fn dragging_retunes_to_the_key_or_freely() {
		let e = edits();
		let (mut p, clip) = project(&e);
		let c_major = Scale::parse("C major").unwrap();
		// Half a semitone up, snapped: 60.3 -> 61 is not in C major, 60.8 rounds to C...
		let moved = moved_pitches(&e, &ids(&e, &[0]), 0.4, Some(Some(&c_major)));
		assert_eq!(moved[0].1, 60.0);
		let moved = moved_pitches(&e, &ids(&e, &[0]), 1.4, Some(Some(&c_major)));
		assert_eq!(moved[0].1, 62.0);
		let free = moved_pitches(&e, &ids(&e, &[0]), 0.4, None);
		assert!((free[0].1 - 60.7).abs() < 1e-9);
		let after = run(
			&mut p,
			move_pitch(clip, &e, &ids(&e, &[0, 1]), 1.0, Some(None)).unwrap(),
		);
		assert_eq!(after.notes[0].pitch, Some(61.0));
		assert_eq!(after.notes[1].pitch, Some(65.0));
		// Back onto the sung center = no pitch edit.
		let back = moved_pitches(&after, &ids(&after, &[0]), -0.7, None);
		assert!((back[0].1 - 60.3).abs() < 1e-9);
		let cmd = move_pitch(clip, &after, &ids(&after, &[0]), -0.7, None).unwrap();
		let back = run(&mut p, cmd);
		assert_eq!(back.notes[0].pitch, None);
	}

	#[test]
	fn detected_correction_takes_the_singers_offset_out() {
		// E Phrygian sung 35 c sharp: the F at 65.5 rounds to F# chromatically.
		let sung = [64.35, 65.5, 67.3, 65.45, 64.4, 62.3, 60.4, 64.35];
		let e = NoteEdits {
			notes: sung
				.iter()
				.enumerate()
				.map(|(i, &c)| note(i as f64 * 0.5, i as f64 * 0.5 + 0.45, c))
				.collect(),
			..edits()
		};
		let fit = detected_fit(&e).expect("a fit");
		assert!((fit.offset_cents - 35.0).abs() <= 6.0, "{fit:?}");
		let (mut p, clip) = project(&e);
		let all = BTreeSet::new();
		let after = run(
			&mut p,
			correct_pitch(clip, &e, &all, &NoteEditorView::default()).unwrap(),
		);
		let pitches: Vec<f64> = after.notes.iter().map(|n| n.pitch.unwrap()).collect();
		assert_eq!(pitches, [64.0, 65.0, 67.0, 65.0, 64.0, 62.0, 60.0, 64.0]);
		let chromatic = NoteEditorView {
			key: KeyChoice::Chromatic,
			..NoteEditorView::default()
		};
		let (mut p, clip) = project(&e);
		let after = run(&mut p, correct_pitch(clip, &e, &all, &chromatic).unwrap());
		assert_eq!(after.notes[1].pitch, Some(66.0));
	}

	#[test]
	fn macros_correct_quantize_level_and_reset() {
		let e = edits();
		let (mut p, clip) = project(&e);
		let view = NoteEditorView {
			correct_threshold: 25.0,
			correct_drift: 0.5,
			..NoteEditorView::default()
		};
		let all = BTreeSet::new();
		let after = run(&mut p, correct_pitch(clip, &e, &all, &view).unwrap());
		assert_eq!(after.notes[0].pitch, Some(60.0));
		assert_eq!(after.notes[1].pitch, None, "20 c off: under the threshold");
		assert_eq!(after.notes[2].pitch, Some(67.0));
		assert!(after.notes.iter().all(|n| n.drift == 0.5));

		// At 120 bpm a sixteenth is 0.125 s: 1.2 s -> 1.25 s.
		let q = quantize_time(&p, clip, &after, &all, &view)
			.unwrap()
			.unwrap();
		let after = run(&mut p, q);
		let t = after.notes[2].time.unwrap();
		assert!(
			(t.start - 1.25).abs() < 1e-6 && (t.end - 1.65).abs() < 1e-6,
			"{t:?}"
		);

		let reset_cmd = reset(clip, &after, &ids(&after, &[0, 1, 2])).unwrap();
		let after = run(&mut p, reset_cmd);
		assert!(!after.is_edited());
		assert!(
			after
				.notes
				.iter()
				.all(|n| n.pitch.is_none() && n.time.is_none())
		);
	}

	#[test]
	fn time_moves_never_cross_a_neighbour() {
		let e = edits();
		let (mut p, clip) = project(&e);
		let gap = ids(&e, &[2]);
		let after = run(&mut p, move_time(clip, &e, &gap, -0.1).unwrap().unwrap());
		let t = after.notes[2].time.unwrap();
		assert!(
			(t.start - 1.1).abs() < 1e-9 && (t.end - 1.5).abs() < 1e-9,
			"{t:?}"
		);
		let err = move_time(clip, &after, &gap, -0.3).unwrap_err();
		assert!(err.contains("neighbour"), "{err}");
		// Moving it back where it was sung clears the edit.
		let back = run(&mut p, move_time(clip, &after, &gap, 0.1).unwrap().unwrap());
		assert_eq!(back.notes[2].time, None);
	}

	#[test]
	fn tools_edit_their_parameter_and_double_click_resets() {
		let e = edits();
		let (mut p, clip) = project(&e);
		let one = ids(&e, &[1]);
		let after = run(
			&mut p,
			tool_edit(clip, &e, &one, NoteTool::Modulation, Some(-4.0)).unwrap(),
		);
		assert_eq!(after.notes[1].modulation, 0.0);
		let after = run(
			&mut p,
			tool_edit(clip, &after, &one, NoteTool::Amplitude, Some(3.0)).unwrap(),
		);
		assert_eq!(after.notes[1].gain_db, 3.0);
		let after = run(
			&mut p,
			tool_edit(clip, &after, &one, NoteTool::Modulation, None).unwrap(),
		);
		assert_eq!(after.notes[1].modulation, 1.0);
		assert!(tool_edit(clip, &after, &one, NoteTool::Drift, None).is_none());
	}

	#[test]
	fn hit_tests_and_view_framing() {
		let e = edits();
		assert_eq!(note_at(&e, 0.7, 64.2, 0.6), Some(e.notes[1].id));
		assert_eq!(note_at(&e, 0.7, 62.0, 0.6), None);
		assert_eq!(boundary_at(&e, 0.51, 0.02), Some(e.notes[0].id));
		assert_eq!(boundary_at(&e, 1.1, 0.02), None);
		assert_eq!(notes_in(&e, (0.4, 0.6), (59.0, 70.0)).len(), 2);
		let mut v = NoteEditorView::default();
		v.frame(&e);
		assert!(v.low <= 57.3 && v.high >= 69.6, "{v:?}");
		// Three notes say nothing about a scale: Detected tunes to the nearest semitone.
		assert_eq!(detected_fit(&e), None);
		assert_eq!(NoteEditorView::default().scale(&e), None);
	}

	#[test]
	fn detect_split_merge_and_remove_are_undo_steps() {
		use cadenza_notes::eval::synth::{Phrase, SynthNote, Vowel, render};
		let dir = tempfile::tempdir().unwrap();
		let wav = dir.path().join("vox.wav");
		let r = render(&Phrase {
			notes: vec![
				SynthNote::new(57.0, 0.6),
				SynthNote::new(62.0, 0.6).rest(0.2),
			],
			vowel: Vowel::O,
			sample_rate: 22_050,
			snr_db: None,
			reverb: None,
			seed: 9,
		});
		let buf = cadenza_audio_io::AudioBuffer::from_planar(22_050, vec![r.samples]).unwrap();
		cadenza_audio_io::export(
			&buf,
			&wav,
			&cadenza_audio_io::ExportOptions::wav(cadenza_audio_io::WavSampleFormat::Float32),
		)
		.unwrap();
		let mut s = crate::state::AppState::headless();
		s.import_audio(&wav);
		let clip = s.project().tracks.last().unwrap().clips[0].id;
		let file = audio_edit::region(s.project(), clip).unwrap().file;
		let analysis = analyze_file(&file, &dir.path().join("cache")).unwrap();
		s.note_analyses.insert(file, analysis);
		assert!(s.detect_notes(clip, None));
		let notes = |s: &crate::state::AppState| clip_notes(s.project(), clip).unwrap().0;
		assert_eq!(notes(&s).notes.len(), 2);
		let first = notes(&s).notes[0].clone();
		assert!(s.split_audio_note(clip, first.id, 0.5 * (first.core_start + first.core_end)));
		assert_eq!(notes(&s).notes.len(), 3);
		assert_eq!(
			s.document().history_labels().last().copied(),
			Some("Split note")
		);
		let left = notes(&s).notes[0].id;
		assert!(s.merge_audio_notes(clip, left));
		assert_eq!(notes(&s).notes.len(), 2);
		// The two sung notes are 200 ms apart: they cannot be merged.
		let n0 = notes(&s).notes[0].id;
		assert!(!s.merge_audio_notes(clip, n0));
		assert!(s.remove_audio_notes(clip));
		assert!(clip_notes(s.project(), clip).is_none());
		s.undo();
		assert_eq!(notes(&s).notes.len(), 2);
	}
}
