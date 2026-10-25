//! The tuning engineer's tools (NE-7b): detect the notes of a sung (monophonic) audio
//! clip, read them as an engineer would (scale and the singer's offset, a target per note
//! with its reason, the doubtful items), retune them with musical restraint and fix the
//! segmentation. The deterministic proposals of `cadenza_notes::tuning` do the measuring;
//! the model decides only what they leave open (the mode, ornaments, F vs F#).
//!
//! Notes are addressed by their number in time order (1 = first): cheaper to read than ids,
//! and every listing gives them again. A split, merge or detection renumbers the notes.
//! The pure builders ([`correction_patches`], [`pitch_patch`], [`plan_targets`],
//! [`analyze_file`]) are shared with the app's note editor.

use std::collections::BTreeSet;
use std::path::Path;

use cadenza_core::{AudioNoteId, ClipId, Tick};
use cadenza_model::{
	AudioNote, AudioNoteFlag, AudioNotePatch, Clip, ClipContent, Command, ModelError, NoteEdits,
	Project,
};
use cadenza_notes::analysis::{Analysis, AnalysisOptions, Cache};
use cadenza_notes::tuning::{self, ScaleFit, Target, TargetOptions, TargetReason, TuningNote};
use cadenza_theory::{PitchClass, Scale, ScaleKind, format_pitch};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::convert::PitchInput;
use crate::error::McpError;
use crate::session::Session;
use crate::tools::notes::{default_cache_dir, scale_name};

/// Decodes `file` and analyzes its mono mix through the content cache in `cache` (the same
/// analysis the app's note editor draws from).
pub fn analyze_file(file: &Path, cache: &Path) -> Result<Analysis, McpError> {
	let buffer = cadenza_audio_io::decode_file(file).map_err(|e| McpError::Io {
		path: file.display().to_string(),
		message: format!("{e}; relink the clip's file (list_missing_files, relink_files)"),
	})?;
	let channels: Vec<&[f32]> = buffer.channels().iter().map(Vec::as_slice).collect();
	let mono = cadenza_notes::resynth::mono(&channels);
	Cache::new(cache)
		.get_or_analyze(
			&mono,
			buffer.sample_rate(),
			&AnalysisOptions::default(),
			&mut |_| true,
		)
		.ok_or_else(|| McpError::Invalid("the analysis was cancelled".into()))
}

/// An audio clip and where its file sits on the song timeline.
pub struct VocalClip<'a> {
	pub clip: &'a Clip,
	pub file: &'a Path,
	pub notes: Option<&'a NoteEdits>,
	/// Song seconds at file time 0.
	pub origin: f64,
	/// File seconds the clip plays.
	pub region: (f64, f64),
}

impl<'a> VocalClip<'a> {
	pub fn find(project: &'a Project, clip_id: ClipId) -> Result<Self, McpError> {
		let (_, clip) = project
			.find_clip(clip_id)
			.ok_or(ModelError::ClipNotFound(clip_id))?;
		let ClipContent::Audio {
			file,
			offset_seconds,
			notes,
			..
		} = &clip.content
		else {
			return Err(McpError::Invalid(format!(
				"clip {clip_id} is a MIDI clip; the tuning tools work on audio clips of a sung or \
				 played monophonic part (get_project lists the audio clips)"
			)));
		};
		let start = project.tempo.tick_to_seconds(clip.start);
		Ok(Self {
			clip,
			file,
			notes: notes.as_deref(),
			origin: start - offset_seconds,
			region: (
				*offset_seconds,
				offset_seconds + clip.duration_seconds(&project.tempo),
			),
		})
	}

	/// The detected notes, or an actionable error.
	pub fn edits(&self) -> Result<&'a NoteEdits, McpError> {
		self.notes.ok_or_else(|| {
			McpError::Invalid(format!(
				"clip '{}' has no detected notes yet; call detect_audio_notes first",
				self.clip.name
			))
		})
	}

	/// Song beat of a file second.
	pub fn beat(&self, project: &Project, file_seconds: f64) -> f64 {
		let song = (self.origin + file_seconds).max(0.0);
		round(project.tempo.seconds_to_tick(song).beats(), 100.0)
	}

	/// File second of a song beat.
	pub fn file_seconds(&self, project: &Project, beat: f64) -> f64 {
		project
			.tempo
			.tick_to_seconds(Tick::from_beats(beat.max(0.0)))
			- self.origin
	}

	/// Whether a note sounds inside the clip.
	fn plays(&self, n: &AudioNote) -> bool {
		n.end > self.region.0 && n.start < self.region.1
	}
}

fn round(x: f64, scale: f64) -> f64 {
	(x * scale).round() / scale
}

/// "E4 +31 c".
fn sung(p: f64) -> String {
	let midi = p.round().clamp(0.0, 127.0);
	let cents = ((p - midi) * 100.0).round();
	format!("{} {cents:+.0} c", format_pitch(midi as u8, false))
}

fn tone(p: f64) -> String {
	format_pitch(p.round().clamp(0.0, 127.0) as u8, false)
}

/// The scale targets come from.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum KeyPlan {
	/// Fitted to the notes (any mode, with the singer's offset).
	Detected,
	/// Nearest semitone.
	Nearest,
	Scale(Scale),
}

impl KeyPlan {
	pub fn parse(key: Option<&str>) -> Result<Self, McpError> {
		let Some(k) = key.map(str::trim).filter(|k| !k.is_empty()) else {
			return Ok(Self::Detected);
		};
		match k.to_lowercase().as_str() {
			"auto" | "detected" | "detect" => Ok(Self::Detected),
			"chromatic" | "nearest" | "off" | "none" => Ok(Self::Nearest),
			_ => Scale::parse(k).map(Self::Scale).map_err(|_| {
				McpError::Invalid(format!(
					"key '{k}' is not a scale; use \"auto\" (fitted to the notes), \"nearest\" \
					 (nearest semitone) or a scale like \"E phrygian\", \"A minor\", \"D dorian\""
				))
			}),
		}
	}
}

/// What the targets of a clip's notes are and where they come from.
#[derive(Debug, Clone)]
pub struct TargetPlan {
	/// The scale and the singer's offset (`None`: nearest semitone, offset 0 or given).
	pub fit: Option<ScaleFit>,
	/// One per note of the clip, in order.
	pub targets: Vec<Target>,
	/// "E phrygian (fitted)", "nearest semitone", "reference 'Melody' + E phrygian".
	pub source: String,
}

fn check_offset(offset: Option<f64>) -> Result<Option<f64>, McpError> {
	match offset {
		Some(o) if !(o.is_finite() && (-50.0..=50.0).contains(&o)) => Err(McpError::Invalid(
			format!("offset_cents {o} is outside -50..50 (the singer's reference: + = sharp)"),
		)),
		o => Ok(o),
	}
}

/// The targets of `edits` (the notes of `vocal`) under `key`, the singer's offset fitted
/// unless `offset` is given, and the notes of the MIDI clip `reference` where it plays.
pub fn plan_targets(
	project: &Project,
	vocal: &VocalClip,
	edits: &NoteEdits,
	key: KeyPlan,
	offset: Option<f64>,
	reference: Option<ClipId>,
) -> Result<TargetPlan, McpError> {
	let offset = check_offset(offset)?;
	let notes: Vec<TuningNote> = edits.notes.iter().map(TuningNote::from).collect();
	let chromatic = Scale::new(PitchClass(0), ScaleKind::Chromatic);
	let (fit, mut source) = match key {
		KeyPlan::Detected => match tuning::fit_scale(&notes, 1).first().copied() {
			Some(f) => (Some(f), format!("{} (fitted)", scale_name(&f.scale))),
			None => (
				None,
				format!(
					"nearest semitone (fewer than {} notes to fit a scale)",
					tuning::MIN_NOTES
				),
			),
		},
		KeyPlan::Nearest => (None, "nearest semitone".into()),
		KeyPlan::Scale(s) => (
			tuning::fit_for_scale(&notes, s),
			format!("{} (given)", scale_name(&s)),
		),
	};
	let fit = fit.map(|f| ScaleFit {
		offset_cents: offset.unwrap_or(f.offset_cents),
		..f
	});
	let used = fit.unwrap_or(ScaleFit {
		scale: chromatic,
		offset_cents: offset.unwrap_or(0.0),
		rms_cents: 0.0,
	});
	let mut targets = tuning::targets(&notes, &used, &TargetOptions::default());
	if let Some(id) = reference {
		let (track, clip) = project.find_clip(id).ok_or(ModelError::ClipNotFound(id))?;
		if !matches!(clip.content, ClipContent::Midi { .. }) {
			return Err(McpError::Invalid(format!(
				"reference clip {id} is an audio clip; pass a MIDI clip holding the melody"
			)));
		}
		let mut matched = 0;
		for (n, t) in edits.notes.iter().zip(targets.iter_mut()) {
			let mid = vocal.origin + 0.5 * (n.core_start + n.core_end);
			let tick = project.tempo.seconds_to_tick(mid.max(0.0));
			if tick < clip.start || tick >= clip.start + clip.length {
				continue;
			}
			let rel = tick - clip.start;
			let sung = n.center - used.offset_cents / 100.0;
			let heard = clip
				.notes()
				.iter()
				.filter(|m| m.start <= rel && rel < m.start + m.length)
				.map(|m| {
					let p = f64::from(m.pitch);
					p + 12.0 * ((sung - p) / 12.0).round()
				})
				.min_by(|a, b| (a - sung).abs().total_cmp(&(b - sung).abs()));
			if let Some(p) = heard {
				matched += 1;
				*t = Target {
					pitch: p,
					reason: TargetReason::Reference,
					runner_up: t.pitch,
					ambiguous: false,
				};
			}
		}
		source = format!(
			"reference '{}' on '{}' ({matched} of {} notes), else {source}",
			clip.name,
			track.name,
			edits.notes.len()
		);
	}
	Ok(TargetPlan {
		fit,
		targets,
		source,
	})
}

/// A pitch change of `n` to `pitch`, clamped to the shift the model allows (`None` pitch
/// when it is the sung one).
pub fn pitch_patch(n: &AudioNote, pitch: f64) -> AudioNotePatch {
	let pitch = pitch.clamp(
		n.center - cadenza_model::audio_notes::MAX_SHIFT,
		n.center + cadenza_model::audio_notes::MAX_SHIFT,
	);
	AudioNotePatch {
		id: n.id,
		pitch: Some(((pitch - n.center).abs() > 1e-9).then_some(pitch)),
		..Default::default()
	}
}

/// How far a correction goes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Correction {
	/// Share of the way from the sung center to the tone (0..1).
	pub center: f64,
	/// Share of the slow drift straightened (0..1); 0 leaves the drift alone.
	pub drift: f64,
	/// Share of the vibrato kept (0..2); `None` leaves it alone.
	pub vibrato: Option<f32>,
	/// Notes closer than this to their tone keep their pitch.
	pub threshold_cents: f64,
}

impl Default for Correction {
	fn default() -> Self {
		Self {
			center: 1.0,
			drift: 0.0,
			vibrato: None,
			threshold_cents: 0.0,
		}
	}
}

/// Correct Pitch over the notes of `ids` (all when empty): each moves toward its tone in
/// `tones` (one per note). The app's macro and the AI's tune_audio_notes.
pub fn correction_patches(
	edits: &NoteEdits,
	tones: &[f64],
	ids: &BTreeSet<AudioNoteId>,
	c: &Correction,
) -> Vec<AudioNotePatch> {
	let center = c.center.clamp(0.0, 1.0);
	let drift = (1.0 - c.drift.clamp(0.0, 1.0)) as f32;
	edits
		.notes
		.iter()
		.zip(tones)
		.filter(|(n, _)| ids.is_empty() || ids.contains(&n.id))
		.filter_map(|(n, &tone)| {
			let off = tone - n.center;
			let mut p = AudioNotePatch {
				id: n.id,
				..Default::default()
			};
			if off.abs() * 100.0 > c.threshold_cents && center > 0.0 {
				let target = n.center + center * off;
				if (target - n.result_pitch()).abs() > 1e-9 {
					p.pitch = pitch_patch(n, target).pitch;
				}
			}
			if c.drift > 0.0 && (n.drift - drift).abs() > 1e-6 {
				p.drift = Some(drift);
			}
			if let Some(v) = c.vibrato
				&& (n.modulation - v).abs() > 1e-6
			{
				p.modulation = Some(v);
			}
			(!p.is_empty()).then_some(p)
		})
		.collect()
}

fn notes_command(clip: ClipId, label: &str, patches: Vec<AudioNotePatch>) -> Command {
	Command::Batch {
		label: label.into(),
		commands: vec![Command::UpdateAudioNotes {
			clip,
			patches,
			sibilant_gain_db: None,
		}],
	}
}

fn replace_command(clip: ClipId, label: &str, notes: NoteEdits) -> Command {
	Command::Batch {
		label: label.into(),
		commands: vec![Command::SetAudioNotes {
			clip,
			notes: Some(Box::new(notes)),
		}],
	}
}

/// The note numbered `number` (1-based, time order).
fn numbered(edits: &NoteEdits, number: usize) -> Result<&AudioNote, McpError> {
	number
		.checked_sub(1)
		.and_then(|i| edits.notes.get(i))
		.ok_or_else(|| {
			McpError::Invalid(format!(
				"note {number} does not exist: the clip has {} notes, numbered 1..{} in time \
				 order (get_audio_notes lists them; numbers shift after a split or merge)",
				edits.notes.len(),
				edits.notes.len()
			))
		})
}

fn percent(name: &str, v: f64, lo: f64, hi: f64) -> Result<f64, McpError> {
	if v.is_finite() && (lo..=hi).contains(&v) {
		Ok(v / 100.0)
	} else {
		Err(McpError::Invalid(format!(
			"{name} {v} is outside {lo}..{hi} %"
		)))
	}
}

fn sensitivity(s: Option<f32>, previous: Option<&NoteEdits>) -> Result<f32, McpError> {
	match s {
		Some(s) if !(s.is_finite() && (0.1..=10.0).contains(&s)) => {
			Err(McpError::Invalid(format!(
				"sensitivity {s} is outside 0.1..10 (1 = default, 2 = more notes, 0.5 = fewer)"
			)))
		}
		Some(s) => Ok(s),
		None => Ok(previous.map_or(1.0, |p| p.sensitivity)),
	}
}

// detect_audio_notes

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct DetectAudioNotesArgs {
	/// Audio clip of a sung or played monophonic part.
	pub clip_id: ClipId,
	/// Above 1 finds more notes, below 1 fewer (default: the clip's last, else 1).
	#[serde(default)]
	pub sensitivity: Option<f32>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct DetectAudioNotesOutput {
	pub clip_id: ClipId,
	pub notes: usize,
	/// Notes the detection is unsure about (analyze_vocal lists them).
	pub flagged: usize,
	/// A detection again keeps the edits, splits and merges of the notes it finds again.
	pub again: bool,
	pub edited_kept: usize,
	pub undo_label: String,
}

/// MCP `detect_audio_notes`: (re)detect the notes of a clip, one undo step.
pub fn detect_audio_notes(
	session: &mut Session,
	args: DetectAudioNotesArgs,
) -> Result<DetectAudioNotesOutput, McpError> {
	let project = session.document().project();
	let vocal = VocalClip::find(project, args.clip_id)?;
	let previous = vocal.notes.cloned();
	let sensitivity = sensitivity(args.sensitivity, previous.as_ref())?;
	let analysis = analyze_file(vocal.file, &default_cache_dir())?;
	let found = cadenza_notes::clip::detect(
		&analysis,
		sensitivity,
		previous.as_ref(),
		&mut AudioNoteId::new,
	);
	let in_clip: Vec<&AudioNote> = found.notes.iter().filter(|n| vocal.plays(n)).collect();
	if in_clip.is_empty() {
		return Err(McpError::Invalid(format!(
			"no notes found in clip '{}': it needs a pitched monophonic part (voice, bass, \
			 lead). Try a higher sensitivity",
			vocal.clip.name
		)));
	}
	let label = if previous.is_some() {
		"Detect notes again"
	} else {
		"Detect notes"
	};
	let out = DetectAudioNotesOutput {
		clip_id: args.clip_id,
		notes: in_clip.len(),
		flagged: in_clip.iter().filter(|n| !n.flags.is_empty()).count(),
		again: previous.is_some(),
		edited_kept: in_clip.iter().filter(|n| n.is_edited()).count(),
		undo_label: label.into(),
	};
	session
		.document_mut()
		.execute(replace_command(args.clip_id, label, found))?;
	Ok(out)
}

// analyze_vocal / get_audio_notes

#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
pub struct TargetArgs {
	/// "auto" (default: scale and the singer's offset fitted to the notes, every mode),
	/// "nearest" (nearest semitone) or a scale like "E phrygian".
	#[serde(default)]
	pub key: Option<String>,
	/// The singer's reference in cents (+ = sharp, -50..50); default fitted.
	#[serde(default)]
	pub offset_cents: Option<f64>,
	/// A MIDI clip with the intended melody: notes it plays take its pitch (octave of the
	/// singer), the others follow the key.
	#[serde(default)]
	pub reference_clip_id: Option<ClipId>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct AnalyzeVocalArgs {
	pub clip_id: ClipId,
	#[serde(flatten)]
	pub targets: TargetArgs,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct VocalReport {
	pub clip_id: ClipId,
	pub notes: usize,
	/// "B3-G4".
	pub range: String,
	/// Where the targets come from.
	pub targets_from: String,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub scale: Option<String>,
	/// The singer sings this far from concert pitch overall (+ = sharp); tuning to the
	/// targets takes it out.
	pub offset_cents: f64,
	/// How well the scale explains the notes after the offset (lower = clearer).
	#[serde(skip_serializing_if = "Option::is_none")]
	pub fit_rms_cents: Option<f64>,
	/// Other scales that fit nearly as well (key auto only).
	#[serde(skip_serializing_if = "Vec::is_empty")]
	pub alternatives: Vec<String>,
	/// Mean distance of the sung centers from their targets.
	pub sung_error_cents: f64,
	/// The same for the pitches the notes have now (with the edits).
	pub now_error_cents: f64,
	/// Notes within 10 cents of their target now.
	pub on_target: usize,
	pub edited: usize,
	/// Items for an engineer's ear: doubtful targets, kept chromatic notes, places that
	/// may hold two notes, unsure notes. Note numbers in time order.
	pub decisions: Vec<String>,
	#[serde(skip_serializing_if = "is_zero")]
	pub more_decisions: usize,
}

fn is_zero(n: &usize) -> bool {
	*n == 0
}

const MAX_DECISIONS: usize = 20;

fn reason_name(r: TargetReason) -> &'static str {
	match r {
		TargetReason::InScale => "in_scale",
		TargetReason::Chromatic => "chromatic",
		TargetReason::JoinsNext => "joins_next",
		TargetReason::ScoopLanding => "scoop_landing",
		TargetReason::Reference => "reference",
	}
}

fn decision(
	project: &Project,
	vocal: &VocalClip,
	number: usize,
	n: &AudioNote,
	t: &Target,
	offset: f64,
) -> Vec<String> {
	let at = vocal.beat(project, n.start);
	let mut out = Vec::new();
	if t.ambiguous {
		out.push(format!(
			"note {number} (beat {at:.2}): sung {}, between {} and {} after the {offset:+.0} c \
			 offset: listen and pick one",
			sung(n.center),
			tone(t.pitch),
			tone(t.runner_up)
		));
	}
	if t.reason == TargetReason::Chromatic {
		out.push(format!(
			"note {number} (beat {at:.2}): sung clearly on {} outside the scale, kept; the scale \
			 tone would be {}",
			tone(t.pitch),
			tone(t.runner_up)
		));
	}
	for f in &n.flags {
		match f {
			AudioNoteFlag::MaybeTwoNotes { at: s, cents } => out.push(format!(
				"note {number} (beat {at:.2}): may hold two notes {cents:.0} c apart; \
				 split_audio_note at beat {:.2} if it does",
				vocal.beat(project, *s)
			)),
			AudioNoteFlag::ShortCore | AudioNoteFlag::WeakVoicing if n.confidence < 0.6 => out
				.push(format!(
					"note {number} (beat {at:.2}): short or breathy, its pitch is unsure: tune \
					 it gently or leave it"
				)),
			_ => {}
		}
	}
	out
}

/// MCP `analyze_vocal`: the engineer's first read of a clip.
pub fn analyze_vocal(session: &Session, args: AnalyzeVocalArgs) -> Result<VocalReport, McpError> {
	let project = session.document().project();
	let vocal = VocalClip::find(project, args.clip_id)?;
	let edits = vocal.edits()?;
	let key = KeyPlan::parse(args.targets.key.as_deref())?;
	let plan = plan_targets(
		project,
		&vocal,
		edits,
		key,
		args.targets.offset_cents,
		args.targets.reference_clip_id,
	)?;
	let offset = plan
		.fit
		.map_or(args.targets.offset_cents.unwrap_or(0.0), |f| f.offset_cents);
	let alternatives = if key == KeyPlan::Detected {
		let notes: Vec<TuningNote> = edits.notes.iter().map(TuningNote::from).collect();
		tuning::fit_scale(&notes, 4)
			.iter()
			.skip(1)
			.map(|f| {
				format!(
					"{} {:+.0} c (rms {:.0} c)",
					scale_name(&f.scale),
					f.offset_cents,
					f.rms_cents
				)
			})
			.collect()
	} else {
		Vec::new()
	};
	let mut decisions = Vec::new();
	let (mut sung_err, mut now_err, mut on_target, mut count) = (0.0, 0.0, 0, 0);
	let (mut low, mut high) = (f64::MAX, f64::MIN);
	for (i, (n, t)) in edits.notes.iter().zip(&plan.targets).enumerate() {
		if !vocal.plays(n) {
			continue;
		}
		count += 1;
		low = low.min(n.center);
		high = high.max(n.center);
		sung_err += (n.center - t.pitch).abs() * 100.0;
		let now = (n.result_pitch() - t.pitch).abs() * 100.0;
		now_err += now;
		if now <= 10.0 {
			on_target += 1;
		}
		decisions.extend(decision(project, &vocal, i + 1, n, t, offset));
	}
	if count == 0 {
		return Err(McpError::Invalid(format!(
			"clip '{}' plays none of its detected notes (its region was trimmed); call \
			 detect_audio_notes again",
			vocal.clip.name
		)));
	}
	let more = decisions.len().saturating_sub(MAX_DECISIONS);
	decisions.truncate(MAX_DECISIONS);
	let mean = |s: f64| round(s / count as f64, 10.0);
	Ok(VocalReport {
		clip_id: args.clip_id,
		notes: count,
		range: format!("{}-{}", tone(low), tone(high)),
		targets_from: plan.source,
		scale: plan.fit.map(|f| scale_name(&f.scale)),
		offset_cents: offset,
		fit_rms_cents: plan.fit.map(|f| round(f.rms_cents, 10.0)),
		alternatives,
		sung_error_cents: mean(sung_err),
		now_error_cents: mean(now_err),
		on_target,
		edited: edits
			.notes
			.iter()
			.filter(|n| vocal.plays(n) && n.is_edited())
			.count(),
		decisions,
		more_decisions: more,
	})
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct GetAudioNotesArgs {
	pub clip_id: ClipId,
	/// Song beats; default the whole clip.
	#[serde(default)]
	pub start_beat: Option<f64>,
	#[serde(default)]
	pub end_beat: Option<f64>,
	/// At most this many notes (default 40, max 200).
	#[serde(default)]
	pub limit: Option<usize>,
	#[serde(flatten)]
	pub targets: TargetArgs,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct AudioNoteRow {
	/// Number in time order (what the other tuning tools take).
	pub note: usize,
	/// Song beat of the onset and length in beats.
	pub beat: f64,
	pub beats: f64,
	/// Sung center: "E4 +31 c".
	pub sung: String,
	/// Tone it should be: "E4".
	pub target: String,
	/// in_scale, chromatic, joins_next, scoop_landing or reference.
	pub reason: &'static str,
	/// The other tone it could be, when that is a real question.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub or: Option<String>,
	/// Pitch now with the edits, when retuned.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub now: Option<String>,
	/// Edits other than pitch: "vibrato 50 %, drift 0 %, formant +100 c, gain -2 dB, moved".
	#[serde(skip_serializing_if = "Option::is_none")]
	pub edits: Option<String>,
	/// 0..1 when below 1: how sure the detection is that this is exactly one note.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub confidence: Option<f32>,
	#[serde(skip_serializing_if = "Vec::is_empty")]
	pub flags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct GetAudioNotesOutput {
	pub targets_from: String,
	pub offset_cents: f64,
	pub total: usize,
	pub notes: Vec<AudioNoteRow>,
	#[serde(skip_serializing_if = "is_zero")]
	pub more: usize,
}

fn edit_text(n: &AudioNote) -> Option<String> {
	let mut parts = Vec::new();
	if n.modulation != 1.0 {
		parts.push(format!("vibrato {:.0} %", n.modulation * 100.0));
	}
	if n.drift != 1.0 {
		parts.push(format!("drift {:.0} %", n.drift * 100.0));
	}
	if n.formant_cents != 0.0 {
		parts.push(format!("formant {:+.0} c", n.formant_cents));
	}
	if n.gain_db != 0.0 {
		parts.push(format!("gain {:+.1} dB", n.gain_db));
	}
	if n.time.is_some() {
		parts.push("moved".into());
	}
	(!parts.is_empty()).then(|| parts.join(", "))
}

/// MCP `get_audio_notes`: the notes of a clip with their targets, budgeted.
pub fn get_audio_notes(
	session: &Session,
	args: GetAudioNotesArgs,
) -> Result<GetAudioNotesOutput, McpError> {
	let project = session.document().project();
	let vocal = VocalClip::find(project, args.clip_id)?;
	let edits = vocal.edits()?;
	let plan = plan_targets(
		project,
		&vocal,
		edits,
		KeyPlan::parse(args.targets.key.as_deref())?,
		args.targets.offset_cents,
		args.targets.reference_clip_id,
	)?;
	let limit = args.limit.unwrap_or(40).clamp(1, 200);
	let (from, to) = (
		args.start_beat.unwrap_or(f64::MIN),
		args.end_beat.unwrap_or(f64::MAX),
	);
	let mut rows = Vec::new();
	let mut total = 0;
	for (i, (n, t)) in edits.notes.iter().zip(&plan.targets).enumerate() {
		let beat = vocal.beat(project, n.start);
		let end = vocal.beat(project, n.end);
		if !vocal.plays(n) || end <= from || beat >= to {
			continue;
		}
		total += 1;
		if rows.len() >= limit {
			continue;
		}
		let or = (t.ambiguous || t.reason != TargetReason::InScale).then(|| tone(t.runner_up));
		rows.push(AudioNoteRow {
			note: i + 1,
			beat,
			beats: round(end - beat, 100.0),
			sung: sung(n.center),
			target: tone(t.pitch),
			reason: reason_name(t.reason),
			or,
			now: n.pitch.map(sung),
			edits: edit_text(n),
			confidence: (n.confidence < 1.0).then(|| round(f64::from(n.confidence), 100.0) as f32),
			flags: n
				.flags
				.iter()
				.map(|f| match f {
					AudioNoteFlag::MaybeTwoNotes { at, cents } => format!(
						"maybe two notes {cents:.0} c apart, split at beat {:.2}",
						vocal.beat(project, *at)
					),
					AudioNoteFlag::ShortCore => "short core".into(),
					AudioNoteFlag::WeakVoicing => "weak voicing".into(),
				})
				.collect(),
		});
	}
	Ok(GetAudioNotesOutput {
		targets_from: plan.source,
		offset_cents: plan
			.fit
			.map_or(args.targets.offset_cents.unwrap_or(0.0), |f| f.offset_cents),
		total,
		more: total - rows.len(),
		notes: rows,
	})
}

// tune_audio_notes

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct ToneOverride {
	/// Note number (time order).
	pub note: usize,
	/// The tone it should be: "F#4" or a MIDI number.
	pub pitch: PitchInput,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct TuneAudioNotesArgs {
	pub clip_id: ClipId,
	/// Note numbers to tune; default every note of the clip.
	#[serde(default)]
	pub notes: Option<Vec<usize>>,
	#[serde(flatten)]
	pub targets: TargetArgs,
	/// Your decisions on doubtful notes: these notes take this tone instead.
	#[serde(default)]
	pub overrides: Vec<ToneOverride>,
	/// Share of the way to the tone, 0..100 % (default 100; 70-90 keeps some life).
	#[serde(default)]
	pub center_pct: Option<f64>,
	/// Share of the slow pitch drift straightened, 0..100 % (default 0 = leave it).
	#[serde(default)]
	pub drift_pct: Option<f64>,
	/// Share of the vibrato kept, 0..200 % (default: leave it as it is).
	#[serde(default)]
	pub vibrato_pct: Option<f64>,
	/// Notes closer than this to their tone are left alone (cents, default 0).
	#[serde(default)]
	pub threshold_cents: Option<f64>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct TuneAudioNotesOutput {
	pub targets_from: String,
	pub offset_cents: f64,
	pub notes_considered: usize,
	pub changed: usize,
	/// Mean distance from the targets of the notes considered, before -> after.
	pub error_before_cents: f64,
	pub error_after_cents: f64,
	/// "note 3 (beat 2.50): F4 +38 c -> F4 +0 c".
	pub changes: Vec<String>,
	#[serde(skip_serializing_if = "is_zero")]
	pub more: usize,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub undo_label: Option<String>,
}

/// MCP `tune_audio_notes`: Correct Pitch toward the targets, one undo step.
pub fn tune_audio_notes(
	session: &mut Session,
	args: TuneAudioNotesArgs,
) -> Result<TuneAudioNotesOutput, McpError> {
	let project = session.document().project();
	let vocal = VocalClip::find(project, args.clip_id)?;
	let edits = vocal.edits()?;
	let plan = plan_targets(
		project,
		&vocal,
		edits,
		KeyPlan::parse(args.targets.key.as_deref())?,
		args.targets.offset_cents,
		args.targets.reference_clip_id,
	)?;
	let mut tones: Vec<f64> = plan.targets.iter().map(|t| t.pitch).collect();
	for o in &args.overrides {
		numbered(edits, o.note)?;
		tones[o.note - 1] = f64::from(o.pitch.0);
	}
	let ids: BTreeSet<AudioNoteId> = match &args.notes {
		Some(list) if !list.is_empty() => list
			.iter()
			.map(|&k| numbered(edits, k).map(|n| n.id))
			.collect::<Result<_, _>>()?,
		_ => edits
			.notes
			.iter()
			.filter(|n| vocal.plays(n))
			.map(|n| n.id)
			.collect(),
	};
	let c = Correction {
		center: percent("center_pct", args.center_pct.unwrap_or(100.0), 0.0, 100.0)?,
		drift: percent("drift_pct", args.drift_pct.unwrap_or(0.0), 0.0, 100.0)?,
		vibrato: args
			.vibrato_pct
			.map(|v| percent("vibrato_pct", v, 0.0, 200.0).map(|v| v as f32))
			.transpose()?,
		threshold_cents: match args.threshold_cents.unwrap_or(0.0) {
			t if t.is_finite() && (0.0..=100.0).contains(&t) => t,
			t => {
				return Err(McpError::Invalid(format!(
					"threshold_cents {t} is outside 0..100"
				)));
			}
		},
	};
	let patches = correction_patches(edits, &tones, &ids, &c);
	let mut changes = Vec::new();
	let (mut before, mut after) = (0.0, 0.0);
	for (i, (n, &t)) in edits.notes.iter().zip(&tones).enumerate() {
		if !ids.contains(&n.id) {
			continue;
		}
		let now = patches
			.iter()
			.find(|p| p.id == n.id)
			.and_then(|p| p.pitch)
			.map_or(n.result_pitch(), |p| p.unwrap_or(n.center));
		before += (n.result_pitch() - t).abs() * 100.0;
		after += (now - t).abs() * 100.0;
		if (now - n.result_pitch()).abs() > 1e-9 {
			changes.push(format!(
				"note {} (beat {:.2}): {} -> {}",
				i + 1,
				vocal.beat(project, n.start),
				sung(n.result_pitch()),
				sung(now)
			));
		}
	}
	let count = ids.len().max(1) as f64;
	let changed = patches.len();
	let more = changes.len().saturating_sub(24);
	changes.truncate(24);
	let out = TuneAudioNotesOutput {
		targets_from: plan.source,
		offset_cents: plan
			.fit
			.map_or(args.targets.offset_cents.unwrap_or(0.0), |f| f.offset_cents),
		notes_considered: ids.len(),
		changed,
		error_before_cents: round(before / count, 10.0),
		error_after_cents: round(after / count, 10.0),
		changes,
		more,
		undo_label: (changed > 0).then(|| "Tune notes".to_owned()),
	};
	if changed > 0 {
		session
			.document_mut()
			.execute(notes_command(args.clip_id, "Tune notes", patches))?;
	}
	Ok(out)
}

// edit_audio_notes

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct NoteEditArgs {
	/// Note number (time order).
	pub note: usize,
	/// New tone: "F#4" or a MIDI number (with `cents` for a fine offset).
	#[serde(default)]
	pub pitch: Option<PitchInput>,
	/// Added to `pitch` (-100..100); alone it moves the note from where it is now.
	#[serde(default)]
	pub cents: Option<f64>,
	/// Share of the slow drift kept, -100..200 % (100 = as sung, 0 = straight).
	#[serde(default)]
	pub drift_pct: Option<f64>,
	/// Share of the vibrato kept, -100..200 % (100 = as sung, 0 = flat).
	#[serde(default)]
	pub vibrato_pct: Option<f64>,
	/// Formant shift in cents (-1200..1200; 0 = natural).
	#[serde(default)]
	pub formant_cents: Option<f32>,
	/// Level change in dB (-60..24).
	#[serde(default)]
	pub gain_db: Option<f32>,
	/// Back to as sung (pitch, drift, vibrato, formant, gain, timing) before the rest.
	#[serde(default)]
	pub as_sung: bool,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct EditAudioNotesArgs {
	pub clip_id: ClipId,
	pub edits: Vec<NoteEditArgs>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct EditAudioNotesOutput {
	pub changed: usize,
	pub undo_label: String,
}

/// MCP `edit_audio_notes`: hand edits of single notes, one undo step.
pub fn edit_audio_notes(
	session: &mut Session,
	args: EditAudioNotesArgs,
) -> Result<EditAudioNotesOutput, McpError> {
	if args.edits.is_empty() {
		return Err(McpError::Invalid(
			"edits is empty; give at least one { note, ... }".into(),
		));
	}
	let project = session.document().project();
	let vocal = VocalClip::find(project, args.clip_id)?;
	let edits = vocal.edits()?;
	let mut patches = Vec::new();
	for e in &args.edits {
		let n = numbered(edits, e.note)?;
		let mut p = AudioNotePatch {
			id: n.id,
			..Default::default()
		};
		if e.as_sung {
			p.pitch = Some(None);
			p.drift = Some(1.0);
			p.modulation = Some(1.0);
			p.formant_cents = Some(0.0);
			p.gain_db = Some(0.0);
			p.time = Some(None);
		}
		if let Some(c) = e.cents
			&& !(c.is_finite() && (-100.0..=100.0).contains(&c))
		{
			return Err(McpError::Invalid(format!(
				"note {}: cents {c} is outside -100..100",
				e.note
			)));
		}
		let base = match e.pitch {
			Some(p) => Some(f64::from(p.0)),
			None if e.as_sung => e.cents.map(|_| n.center),
			None => e.cents.map(|_| n.result_pitch()),
		};
		if let Some(b) = base {
			let target = b + e.cents.unwrap_or(0.0) / 100.0;
			if (target - n.center).abs() > cadenza_model::audio_notes::MAX_SHIFT {
				return Err(McpError::Invalid(format!(
					"note {}: {} is more than {} semitones from the sung {}",
					e.note,
					sung(target),
					cadenza_model::audio_notes::MAX_SHIFT,
					sung(n.center)
				)));
			}
			p.pitch = pitch_patch(n, target).pitch;
		}
		if let Some(d) = e.drift_pct {
			p.drift = Some(percent("drift_pct", d, -100.0, 200.0)? as f32);
		}
		if let Some(v) = e.vibrato_pct {
			p.modulation = Some(percent("vibrato_pct", v, -100.0, 200.0)? as f32);
		}
		p.formant_cents = e.formant_cents.or(p.formant_cents);
		p.gain_db = e.gain_db.or(p.gain_db);
		if !p.is_empty() {
			patches.push(p);
		}
	}
	let changed = patches.len();
	if changed > 0 {
		session
			.document_mut()
			.execute(notes_command(args.clip_id, "Edit notes", patches))?;
	}
	Ok(EditAudioNotesOutput {
		changed,
		undo_label: "Edit notes".into(),
	})
}

// split_audio_note / merge_audio_notes

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct SplitAudioNoteArgs {
	pub clip_id: ClipId,
	/// Note number (time order).
	pub note: usize,
	/// Where to cut, song beats (inside the note).
	pub at_beat: f64,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct MergeAudioNotesArgs {
	pub clip_id: ClipId,
	/// Note number: it is joined with the note right after it.
	pub note: usize,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ReshapeOutput {
	/// Notes in the clip now (numbers shifted).
	pub notes: usize,
	/// The notes around the change now: "note 7 (beat 6.20): E4 +12 c".
	pub around: Vec<String>,
	pub undo_label: String,
}

fn reshape(
	session: &mut Session,
	clip_id: ClipId,
	label: &str,
	at: f64,
	f: impl FnOnce(&Analysis, &NoteEdits) -> Result<NoteEdits, cadenza_notes::NotesError>,
) -> Result<ReshapeOutput, McpError> {
	let project = session.document().project();
	let vocal = VocalClip::find(project, clip_id)?;
	let edits = vocal.edits()?;
	let analysis = analyze_file(vocal.file, &default_cache_dir())?;
	let reshaped = f(&analysis, edits).map_err(|e| McpError::Invalid(e.to_string()))?;
	let around = reshaped
		.notes
		.iter()
		.enumerate()
		.filter(|(_, n)| n.end > at - 0.6 && n.start < at + 0.6)
		.map(|(i, n)| {
			format!(
				"note {} (beat {:.2}): {}",
				i + 1,
				vocal.beat(project, n.start),
				sung(n.center)
			)
		})
		.collect();
	let out = ReshapeOutput {
		notes: reshaped.notes.len(),
		around,
		undo_label: label.into(),
	};
	session
		.document_mut()
		.execute(replace_command(clip_id, label, reshaped))?;
	Ok(out)
}

/// MCP `split_audio_note`.
pub fn split_audio_note(
	session: &mut Session,
	args: SplitAudioNoteArgs,
) -> Result<ReshapeOutput, McpError> {
	let project = session.document().project();
	let vocal = VocalClip::find(project, args.clip_id)?;
	let n = numbered(vocal.edits()?, args.note)?;
	let id = n.id;
	let at = vocal.file_seconds(project, args.at_beat);
	if !(at > n.start && at < n.end) {
		return Err(McpError::Invalid(format!(
			"beat {:.2} is not inside note {} (beats {:.2}..{:.2})",
			args.at_beat,
			args.note,
			vocal.beat(project, n.start),
			vocal.beat(project, n.end)
		)));
	}
	reshape(session, args.clip_id, "Split note", at, |a, e| {
		cadenza_notes::clip::split(a, e, id, at, &mut AudioNoteId::new)
	})
}

/// MCP `merge_audio_notes`.
pub fn merge_audio_notes(
	session: &mut Session,
	args: MergeAudioNotesArgs,
) -> Result<ReshapeOutput, McpError> {
	let project = session.document().project();
	let vocal = VocalClip::find(project, args.clip_id)?;
	let edits = vocal.edits()?;
	let n = numbered(edits, args.note)?;
	if args.note >= edits.notes.len() {
		return Err(McpError::Invalid(format!(
			"note {} is the last note; merge joins a note with the one after it",
			args.note
		)));
	}
	let (id, at) = (n.id, n.end);
	reshape(session, args.clip_id, "Merge notes", at, |a, e| {
		cadenza_notes::clip::merge_with_next(a, e, id, &mut AudioNoteId::new)
	})
}
