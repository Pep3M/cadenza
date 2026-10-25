//! The audio editor (opened by double-clicking an audio clip): its view state, the
//! clip-based (non-destructive) edits as [`Command`] builders, and the "render a
//! processed copy" operations (normalize, reverse, gain, stretch/pitch with the
//! `stretch` feature) that write a new file and swap it into the clip with
//! `UpdateAudioClip` (undoable: the original file is never touched).
//!
//! The editor's time axis is the clip's **source file** in seconds; the clip region is
//! `offset_seconds .. offset_seconds + duration`.

use crate::i18n::{mark, tr, trf};
use std::path::{Path, PathBuf};

use cadenza_audio_io::AudioBuffer;
use cadenza_core::{ClipId, Tick, TrackId};
use cadenza_model::{
	AudioClipPatch, Clip, ClipContent, ClipFade, ClipPatch, Command, FadeCurve, Project,
};

use super::edits::{self, MIN_CLIP_LENGTH};

/// Normalize target peak.
pub const NORMALIZE_DBFS: f32 = -0.1;
/// Length of the de-click ramps at the edges of a processed selection.
const EDGE_RAMP_SECONDS: f64 = 0.003;
pub const MIN_PX_PER_SECOND: f32 = 2.0;
pub const MAX_PX_PER_SECOND: f32 = 200_000.0;

/// View state of the audio editor.
#[derive(Debug, Clone, PartialEq)]
pub struct AudioEditorView {
	pub clip: Option<ClipId>,
	pub px_per_second: f32,
	/// File second at the left edge.
	pub scroll: f64,
	/// Selected range, file seconds (inside the clip region).
	pub selection: Option<(f64, f64)>,
	/// Amount for "Gain" processing (dB).
	pub gain_db: f32,
	/// Ratio for time-stretch (output / input length).
	pub stretch_ratio: f64,
	pub pitch_semitones: f32,
	/// Clip the view was last fitted to.
	pub framed: Option<ClipId>,
	/// Waveform or Notes (NE-6).
	pub mode: super::note_editor::EditorMode,
	pub notes: super::note_editor::NoteEditorView,
}

impl Default for AudioEditorView {
	fn default() -> Self {
		Self {
			clip: None,
			px_per_second: 100.0,
			scroll: 0.0,
			selection: None,
			gain_db: -3.0,
			stretch_ratio: 1.25,
			pitch_semitones: 2.0,
			framed: None,
			mode: Default::default(),
			notes: Default::default(),
		}
	}
}

impl AudioEditorView {
	pub fn x_of(&self, seconds: f64) -> f32 {
		((seconds - self.scroll) * f64::from(self.px_per_second)) as f32
	}

	pub fn seconds_at(&self, x: f32) -> f64 {
		self.scroll + f64::from(x) / f64::from(self.px_per_second)
	}

	/// Zooms by `factor` keeping the second under `anchor_x` in place.
	pub fn zoom_around(&mut self, factor: f32, anchor_x: f32) {
		let anchor = self.seconds_at(anchor_x);
		self.px_per_second =
			(self.px_per_second * factor).clamp(MIN_PX_PER_SECOND, MAX_PX_PER_SECOND);
		self.scroll = (anchor - f64::from(anchor_x) / f64::from(self.px_per_second)).max(0.0);
	}

	/// Fits `start..end` (file seconds) into `width` pixels with a small margin.
	pub fn frame(&mut self, start: f64, end: f64, width: f32) {
		let span = (end - start).max(0.001);
		let margin = span * 0.04;
		self.px_per_second = ((f64::from(width.max(1.0)) / (span + 2.0 * margin)) as f32)
			.clamp(MIN_PX_PER_SECOND, MAX_PX_PER_SECOND);
		self.scroll = (start - margin).max(0.0);
	}

	/// Drops state of a clip that no longer exists.
	pub fn prune(&mut self, project: &Project) {
		if let Some(c) = self.clip
			&& region(project, c).is_none()
		{
			self.clip = None;
			self.selection = None;
		}
		let notes = self
			.clip
			.and_then(|c| super::note_editor::clip_notes(project, c));
		self.notes.prune(notes.as_ref().map(|(n, _)| n));
	}
}

/// The audio clip being edited, resolved.
#[derive(Debug, Clone, PartialEq)]
pub struct Region {
	pub track: TrackId,
	pub clip: Clip,
	pub file: PathBuf,
	/// File second where the clip starts.
	pub offset: f64,
	/// Clip duration in seconds at the project tempo.
	pub duration: f64,
	/// Song second of the clip start.
	pub song_start: f64,
	pub gain_db: f32,
	pub fade_in: Option<ClipFade>,
	pub fade_out: Option<ClipFade>,
}

impl Region {
	pub fn end(&self) -> f64 {
		self.offset + self.duration
	}

	/// Song position of file second `s`.
	pub fn tick_of(&self, project: &Project, s: f64) -> Tick {
		project
			.tempo
			.seconds_to_tick((self.song_start + s - self.offset).max(0.0))
	}

	/// File second of song position `tick`.
	pub fn seconds_of(&self, project: &Project, tick: Tick) -> f64 {
		project.tempo.tick_to_seconds(tick) - self.song_start + self.offset
	}

	/// `range` ordered and clamped to the clip region.
	pub fn clamp(&self, (a, b): (f64, f64)) -> (f64, f64) {
		let (a, b) = (a.min(b), a.max(b));
		(
			a.clamp(self.offset, self.end()),
			b.clamp(self.offset, self.end()),
		)
	}
}

/// The audio clip `clip`, if it exists and is audio.
pub fn region(project: &Project, clip: ClipId) -> Option<Region> {
	let (track, c) = project.find_clip(clip)?;
	let ClipContent::Audio {
		file,
		offset_seconds,
		gain_db,
		fade_in,
		fade_out,
		..
	} = &c.content
	else {
		return None;
	};
	Some(Region {
		track: track.id,
		clip: c.clone(),
		file: file.clone(),
		offset: *offset_seconds,
		duration: c.duration_seconds(&project.tempo),
		song_start: project.tempo.tick_to_seconds(c.start),
		gain_db: *gain_db,
		fade_in: *fade_in,
		fade_out: *fade_out,
	})
}

/// Splits the clip at file second `at`.
pub fn split_at(project: &Project, clip: ClipId, at: f64) -> Option<(Command, ClipId)> {
	let r = region(project, clip)?;
	edits::split_clip(project, clip, r.tick_of(project, at))
}

/// Moves the region start to file second `at` (left trim; content stays in place).
pub fn set_region_start(project: &Project, clip: ClipId, at: f64) -> Option<Command> {
	let r = region(project, clip)?;
	edits::trim_start(project, clip, r.tick_of(project, at.max(0.0)))
}

/// Moves the region end to file second `at`.
pub fn set_region_end(project: &Project, clip: ClipId, at: f64) -> Option<Command> {
	let r = region(project, clip)?;
	let end = r.tick_of(project, at).max(r.clip.start + MIN_CLIP_LENGTH);
	edits::resize_clip(project, clip, end)
}

/// Keeps only `range` (file seconds) of the clip, in place on the timeline.
pub fn trim_to(project: &Project, clip: ClipId, range: (f64, f64)) -> Option<Command> {
	let r = region(project, clip)?;
	let (a, b) = r.clamp(range);
	if b - a <= 1e-6 {
		return None;
	}
	let start = r.tick_of(project, a);
	let end = r.tick_of(project, b).max(start + MIN_CLIP_LENGTH);
	let duration = project.tempo.tick_to_seconds(end) - project.tempo.tick_to_seconds(start);
	let fade_in = r
		.fade_in
		.filter(|f| a <= r.offset + 1e-9 && f.seconds < duration);
	let fade_out = r.fade_out.filter(|f| {
		b >= r.end() - 1e-9 && f.seconds + fade_in.map_or(0.0, |f| f.seconds) < duration
	});
	let mut commands = Vec::new();
	if fade_in != r.fade_in || fade_out != r.fade_out {
		commands.push(Command::SetClipFades {
			clip,
			fade_in,
			fade_out,
		});
	}
	if (a - r.offset).abs() > 1e-9 {
		commands.push(Command::UpdateAudioClip {
			clip,
			patch: AudioClipPatch {
				offset_seconds: Some(a),
				..Default::default()
			},
		});
	}
	if start != r.clip.start || end != r.clip.end() {
		commands.push(Command::UpdateClip {
			clip,
			patch: ClipPatch {
				start: Some(start),
				length: Some(end - start),
				..ClipPatch::default()
			},
		});
	}
	edits::batch("Trim clip to selection", commands)
}

/// Removes `range` from the clip, leaving a gap: the part after it becomes a new clip
/// at its original timeline position. Returns the command and the new clip's id.
pub fn delete_range(
	project: &Project,
	clip: ClipId,
	range: (f64, f64),
) -> Option<(Command, Option<ClipId>)> {
	let r = region(project, clip)?;
	let (a, b) = r.clamp(range);
	if b - a <= 1e-6 {
		return None;
	}
	let at_start = a <= r.offset + 1e-6;
	let at_end = b >= r.end() - 1e-6;
	match (at_start, at_end) {
		(true, true) => Some((Command::RemoveClip { clip }, None)),
		(true, false) => trim_to(project, clip, (b, r.end())).map(|c| (c, None)),
		(false, true) => trim_to(project, clip, (r.offset, a)).map(|c| (c, None)),
		(false, false) => {
			let left_end = r.tick_of(project, a);
			let right_start = r.tick_of(project, b);
			let mut right = r.clip.clone();
			right.id = ClipId::new();
			right.start = right_start;
			right.length = r.clip.end() - right_start;
			if let ClipContent::Audio {
				offset_seconds,
				fade_in,
				..
			} = &mut right.content
			{
				*offset_seconds = b;
				*fade_in = None;
			}
			let id = right.id;
			let left_secs = a - r.offset;
			let mut commands = vec![Command::SetClipFades {
				clip,
				fade_in: r.fade_in.filter(|f| f.seconds < left_secs),
				fade_out: None,
			}];
			if right.length.0 <= 0 || left_end <= r.clip.start {
				return None;
			}
			commands.push(Command::UpdateClip {
				clip,
				patch: ClipPatch {
					length: Some(left_end - r.clip.start),
					..ClipPatch::default()
				},
			});
			if let ClipContent::Audio { fade_out, .. } = &mut right.content {
				let right_secs = r.end() - b;
				*fade_out = fade_out.filter(|f| f.seconds < right_secs);
			}
			commands.push(Command::AddClip {
				track: r.track,
				clip: right,
			});
			Some((edits::batch("Delete range", commands)?, Some(id)))
		}
	}
}

/// Sets both fades (seconds clamped so they fit in the clip together).
pub fn set_fades(
	project: &Project,
	clip: ClipId,
	fade_in: Option<ClipFade>,
	fade_out: Option<ClipFade>,
) -> Option<Command> {
	let r = region(project, clip)?;
	let fit = |f: Option<ClipFade>, room: f64| {
		f.map(|f| ClipFade {
			seconds: f.seconds.min(room),
			curve: f.curve,
		})
		.filter(|f| f.seconds > 1e-4)
	};
	let fade_in = fit(fade_in, r.duration);
	let fade_out = fit(fade_out, r.duration - fade_in.map_or(0.0, |f| f.seconds));
	(fade_in != r.fade_in || fade_out != r.fade_out).then_some(Command::SetClipFades {
		clip,
		fade_in,
		fade_out,
	})
}

/// Clip gain (dB, clamped to the model's range).
pub fn set_gain(project: &Project, clip: ClipId, db: f32) -> Option<Command> {
	let r = region(project, clip)?;
	let db = (db * 10.0).round() / 10.0;
	let db = db.clamp(-60.0, 12.0);
	((db - r.gain_db).abs() > 1e-6).then_some(Command::UpdateAudioClip {
		clip,
		patch: AudioClipPatch {
			gain_db: Some(db),
			..Default::default()
		},
	})
}

/// All fade curves, for the curve pickers.
/// The DSP curve of a model fade curve (for drawing).
pub fn fade_curve(c: FadeCurve) -> cadenza_audio_io::FadeCurve {
	match c {
		FadeCurve::Linear => cadenza_audio_io::FadeCurve::Linear,
		FadeCurve::EqualPower => cadenza_audio_io::FadeCurve::EqualPower,
		FadeCurve::Exponential => cadenza_audio_io::FadeCurve::Exponential,
		FadeCurve::SCurve => cadenza_audio_io::FadeCurve::SCurve,
	}
}

pub const FADE_CURVES: [(FadeCurve, &str); 4] = [
	(FadeCurve::Linear, mark("Linear")),
	(FadeCurve::EqualPower, mark("Equal power")),
	(FadeCurve::Exponential, mark("Exponential")),
	(FadeCurve::SCurve, mark("S-curve")),
];

// --- Processed copies ------------------------------------------------------------

/// A destructive operation rendered into a new file.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Process {
	Normalize,
	Reverse,
	Gain(f32),
	/// Time-stretch by a ratio (output / input), `stretch` feature only.
	Stretch(f64),
	/// Transpose by semitones, `stretch` feature only.
	Pitch(f32),
}

impl Process {
	pub fn name(self) -> &'static str {
		match self {
			Process::Normalize => "normalized",
			Process::Reverse => "reversed",
			Process::Gain(_) => "gain",
			Process::Stretch(_) => "stretched",
			Process::Pitch(_) => "pitched",
		}
	}

	pub fn label(self) -> String {
		match self {
			Process::Normalize => tr("Normalize").into(),
			Process::Reverse => tr("Reverse").into(),
			Process::Gain(db) => trf(
				"Gain {db} dB",
				&[("db", &crate::i18n::decimal(format!("{:+.1}", db)))],
			),
			Process::Stretch(r) => trf(
				"Time-stretch ×{r}",
				&[("r", &crate::i18n::decimal(format!("{:.2}", r)))],
			),
			Process::Pitch(s) => trf(
				"Pitch {s} st",
				&[("s", &crate::i18n::decimal(format!("{:+.1}", s)))],
			),
		}
	}

	/// Whether this build can run it.
	pub fn available(self) -> bool {
		cfg!(feature = "stretch") || !matches!(self, Process::Stretch(_) | Process::Pitch(_))
	}
}

/// Explains how to get time-stretch/pitch in a build without the `stretch` feature.
pub const STRETCH_HINT: &str = mark(
	"Time-stretch and pitch-shift need the optional `stretch` \
	feature (Signalsmith Stretch; needs a C++ compiler and libclang): build with \
	`cargo build --release -p cadenza-app --features stretch`.",
);

/// What a processing job produced.
#[derive(Debug, Clone, PartialEq)]
pub struct Rendered {
	pub clip: ClipId,
	pub op: Process,
	pub file: PathBuf,
	/// Seconds the region grew (stretch), to lengthen the clip by.
	pub extra_seconds: f64,
}

/// Where processed copies of `source` go: the project's `Audio/` folder, or the user
/// cache when the project is not saved yet (File → Collect Project Files brings those in
/// later). Never overwrites.
pub fn processed_path(project_path: Option<&Path>, source: &Path, op: Process) -> PathBuf {
	let dir = match project_path {
		Some(p) => cadenza_model::ProjectLayout::new(p).audio_dir(),
		None => cache_dir().join("processed"),
	};
	let stem = source
		.file_stem()
		.and_then(|s| s.to_str())
		.unwrap_or("audio");
	(1..)
		.map(|n| dir.join(format!("{stem}-{}-{n}.wav", op.name())))
		.find(|p| !p.exists())
		.expect("a free file name")
}

fn cache_dir() -> PathBuf {
	cadenza_core::paths::cache_dir()
}

/// Applies `op` to frames `a..b` of `buffer`. Returns the new buffer and how many
/// frames the range grew by (stretch).
pub fn process(
	buffer: &AudioBuffer,
	(a, b): (usize, usize),
	op: Process,
) -> Result<(AudioBuffer, i64), String> {
	let frames = buffer.frames();
	let (a, b) = (a.min(frames), b.min(frames));
	if b <= a {
		return Err(tr("the selection is empty").into());
	}
	let sr = buffer.sample_rate();
	let ramp = ((EDGE_RAMP_SECONDS * f64::from(sr)) as usize).max(1);
	let mut channels: Vec<Vec<f32>> = buffer.channels().to_vec();
	match op {
		Process::Normalize | Process::Gain(_) => {
			let gain = match op {
				Process::Gain(db) => cadenza_core::db_to_gain(db),
				_ => {
					let peak = channels
						.iter()
						.flat_map(|c| c[a..b].iter())
						.fold(0.0f32, |m, x| m.max(x.abs()));
					if peak <= 0.0 {
						return Err(tr("the selection is silent: nothing to normalize").into());
					}
					cadenza_core::db_to_gain(NORMALIZE_DBFS) / peak
				}
			};
			let whole = a == 0 && b == frames;
			for c in &mut channels {
				for (i, x) in c[a..b].iter_mut().enumerate() {
					// Short ramps at inner edges avoid clicks against the untouched audio.
					let edge = if whole {
						1.0
					} else {
						let from_start = if a > 0 { i } else { usize::MAX };
						let to_end = if b < frames {
							b - a - 1 - i
						} else {
							usize::MAX
						};
						(from_start.min(to_end) as f32 / ramp as f32).min(1.0)
					};
					*x *= 1.0 + (gain - 1.0) * edge;
				}
			}
		}
		Process::Reverse => {
			for c in &mut channels {
				c[a..b].reverse();
			}
		}
		Process::Stretch(_) | Process::Pitch(_) => return stretch(buffer, (a, b), op),
	}
	let out = AudioBuffer::from_planar(sr, channels).map_err(|e| e.to_string())?;
	Ok((out, 0))
}

#[cfg(feature = "stretch")]
fn stretch(
	buffer: &AudioBuffer,
	(a, b): (usize, usize),
	op: Process,
) -> Result<(AudioBuffer, i64), String> {
	let sr = buffer.sample_rate();
	let part: Vec<Vec<f32>> = buffer.channels().iter().map(|c| c[a..b].to_vec()).collect();
	let part = AudioBuffer::from_planar(sr, part).map_err(|e| e.to_string())?;
	let done = match op {
		Process::Stretch(r) => cadenza_audio_io::time_stretch(&part, r),
		Process::Pitch(s) => cadenza_audio_io::pitch_shift(&part, s),
		_ => unreachable!("only stretch/pitch get here"),
	}
	.map_err(|e| e.to_string())?;
	let grown = done.frames() as i64 - (b - a) as i64;
	let channels: Vec<Vec<f32>> = buffer
		.channels()
		.iter()
		.zip(done.channels())
		.map(|(orig, mid)| {
			let mut c = orig[..a].to_vec();
			c.extend_from_slice(mid);
			c.extend_from_slice(&orig[b..]);
			c
		})
		.collect();
	let out = AudioBuffer::from_planar(sr, channels).map_err(|e| e.to_string())?;
	Ok((out, grown))
}

#[cfg(not(feature = "stretch"))]
fn stretch(_: &AudioBuffer, _: (usize, usize), _: Process) -> Result<(AudioBuffer, i64), String> {
	Err(crate::i18n::tr_str(STRETCH_HINT).into())
}

/// Decodes `source`, processes `range` (file seconds) and writes a 32-bit float WAV to
/// `out`. Runs on a worker thread. Returns the seconds the range grew by.
pub fn render_processed(
	source: &Path,
	out: &Path,
	range: (f64, f64),
	op: Process,
) -> Result<f64, String> {
	let buffer = cadenza_audio_io::decode_file(source).map_err(|e| e.to_string())?;
	let sr = f64::from(buffer.sample_rate());
	let a = (range.0.max(0.0) * sr).round() as usize;
	let b = (range.1.max(0.0) * sr).round() as usize;
	let (processed, grown) = process(&buffer, (a, b), op)?;
	if let Some(dir) = out.parent() {
		std::fs::create_dir_all(dir).map_err(|e| {
			trf(
				"cannot create {dir}: {e}",
				&[("dir", &dir.display()), ("e", &e)],
			)
		})?;
	}
	cadenza_audio_io::export(
		&processed,
		out,
		&cadenza_audio_io::ExportOptions::wav(cadenza_audio_io::WavSampleFormat::Float32),
	)
	.map_err(|e| e.to_string())?;
	Ok(grown as f64 / sr)
}

/// The command that swaps a rendered copy into its clip (and lengthens it after a
/// stretch), one undo step.
pub fn swap_in(project: &Project, done: &Rendered) -> Option<Command> {
	let r = region(project, done.clip)?;
	let mut commands = vec![Command::UpdateAudioClip {
		clip: done.clip,
		patch: AudioClipPatch {
			file: Some(done.file.clone()),
			..Default::default()
		},
	}];
	// Detected notes are times and pitches of the old file: after a reverse, stretch or
	// transpose they no longer describe it (normalize and gain keep them).
	let reshaped = matches!(
		done.op,
		Process::Reverse | Process::Stretch(_) | Process::Pitch(_)
	);
	if reshaped && let ClipContent::Audio { notes: Some(_), .. } = &r.clip.content {
		commands.push(Command::SetAudioNotes {
			clip: done.clip,
			notes: None,
		});
	}
	if done.extra_seconds.abs() > 1e-6 {
		let end = project
			.tempo
			.seconds_to_tick(r.song_start + (r.duration + done.extra_seconds).max(0.01));
		commands.push(Command::UpdateClip {
			clip: done.clip,
			patch: ClipPatch {
				length: Some((end - r.clip.start).max(MIN_CLIP_LENGTH)),
				..ClipPatch::default()
			},
		});
	}
	// Always a labelled batch, so the undo history names the operation.
	Some(Command::Batch {
		label: done.op.label(),
		commands,
	})
}

#[cfg(test)]
mod tests {
	use super::*;
	use cadenza_model::{Document, Track, TrackKind};

	fn beats(b: f64) -> Tick {
		Tick::from_beats(b)
	}

	/// 120 bpm: an audio clip at beat 4 (2 s), 8 beats (4 s), file offset 1 s.
	fn project() -> (Project, ClipId) {
		let mut p = Project::new("a");
		let mut t = Track::new("Vox", TrackKind::Audio);
		let mut c = Clip::audio("V", beats(4.0), beats(8.0), "/tmp/v.wav");
		if let ClipContent::Audio {
			offset_seconds,
			fade_in,
			fade_out,
			..
		} = &mut c.content
		{
			*offset_seconds = 1.0;
			*fade_in = Some(ClipFade::new(0.5, FadeCurve::Linear));
			*fade_out = Some(ClipFade::new(0.5, FadeCurve::SCurve));
		}
		let id = c.id;
		t.clips.push(c);
		p.tracks.push(t);
		(p, id)
	}

	fn exec(p: &Project, cmd: Command) -> Project {
		let mut doc = Document::new(p.clone());
		doc.execute(cmd).unwrap();
		doc.into_project()
	}

	#[test]
	fn region_maps_file_seconds_to_song_time() {
		let (p, id) = project();
		let r = region(&p, id).unwrap();
		assert_eq!((r.offset, r.duration, r.end()), (1.0, 4.0, 5.0));
		assert_eq!(r.tick_of(&p, 1.0), beats(4.0));
		assert_eq!(r.tick_of(&p, 3.0), beats(8.0));
		assert!((r.seconds_of(&p, beats(6.0)) - 2.0).abs() < 1e-9);
		assert_eq!(r.clamp((7.0, 0.0)), (1.0, 5.0));
	}

	#[test]
	fn clip_edits_become_commands() {
		let (p, id) = project();
		// Trim to 2..4 s of the file: starts at beat 6, lasts 4 beats, offset 2 s.
		let t = exec(&p, trim_to(&p, id, (4.0, 2.0)).unwrap());
		let r = region(&t, id).unwrap();
		assert_eq!((r.clip.start, r.clip.length), (beats(6.0), beats(4.0)));
		assert!((r.offset - 2.0).abs() < 1e-9);
		assert_eq!(
			(r.fade_in, r.fade_out),
			(None, None),
			"trimmed-off fades go"
		);

		// Delete 2..3 s: the left part ends at beat 6, the right one starts at beat 8.
		let (cmd, right) = delete_range(&p, id, (2.0, 3.0)).unwrap();
		let d = exec(&p, cmd);
		let left = region(&d, id).unwrap();
		assert_eq!(left.clip.end(), beats(6.0));
		assert!(left.fade_out.is_none() && left.fade_in.is_some());
		let right = region(&d, right.unwrap()).unwrap();
		assert_eq!(
			(right.clip.start, right.clip.end()),
			(beats(8.0), beats(12.0))
		);
		assert!((right.offset - 3.0).abs() < 1e-9);
		assert!(right.fade_in.is_none() && right.fade_out.is_some());
		// Deleting from the start is a trim; everything removes the clip.
		let (cmd, none) = delete_range(&p, id, (0.0, 2.0)).unwrap();
		assert!(none.is_none());
		let s = exec(&p, cmd);
		assert_eq!(region(&s, id).unwrap().clip.start, beats(6.0));
		let (cmd, _) = delete_range(&p, id, (0.0, 9.0)).unwrap();
		assert!(matches!(cmd, Command::RemoveClip { .. }));

		let (cmd, _) = split_at(&p, id, 3.0).unwrap();
		let s = exec(&p, cmd);
		assert_eq!(region(&s, id).unwrap().clip.end(), beats(8.0));

		let e = exec(&p, set_region_end(&p, id, 4.0).unwrap());
		assert_eq!(region(&e, id).unwrap().clip.end(), beats(10.0));
		let b = exec(&p, set_region_start(&p, id, 2.0).unwrap());
		assert_eq!(region(&b, id).unwrap().clip.start, beats(6.0));

		let f = exec(
			&p,
			set_fades(
				&p,
				id,
				Some(ClipFade::new(10.0, FadeCurve::EqualPower)),
				Some(ClipFade::new(1.0, FadeCurve::Linear)),
			)
			.unwrap(),
		);
		let r = region(&f, id).unwrap();
		assert_eq!(r.fade_in.unwrap().seconds, 4.0, "clamped to the clip");
		assert!(r.fade_out.is_none(), "no room left");
		let g = exec(&p, set_gain(&p, id, -3.04).unwrap());
		assert_eq!(region(&g, id).unwrap().gain_db, -3.0);
		assert!(set_gain(&g, id, -3.0).is_none());
	}

	#[test]
	fn processing_renders_a_new_file_and_swaps_it_in() {
		let dir = tempfile::tempdir().unwrap();
		let source = dir.path().join("tone.wav");
		let sr = 8000;
		let samples: Vec<f32> = (0..sr).map(|i| 0.25 * (i as f32 * 0.05).sin()).collect();
		let buf = AudioBuffer::from_planar(sr as u32, vec![samples.clone(), samples]).unwrap();
		cadenza_audio_io::export(
			&buf,
			&source,
			&cadenza_audio_io::ExportOptions::wav(cadenza_audio_io::WavSampleFormat::Float32),
		)
		.unwrap();

		let project_file = dir.path().join("Song.cdz");
		let out = processed_path(Some(&project_file), &source, Process::Normalize);
		assert_eq!(out, dir.path().join("Audio").join("tone-normalized-1.wav"));
		let grown = render_processed(&source, &out, (0.25, 0.75), Process::Normalize).unwrap();
		assert_eq!(grown, 0.0);
		let done = cadenza_audio_io::decode_file(&out).unwrap();
		let peak = |s: &[f32]| s.iter().fold(0.0f32, |m, x| m.max(x.abs()));
		let ch = done.channel(0);
		assert!((peak(&ch[2500..5500]) - cadenza_core::db_to_gain(NORMALIZE_DBFS)).abs() < 0.01);
		assert!(peak(&ch[..1500]) < 0.26, "outside the selection untouched");
		assert_eq!(
			processed_path(Some(&project_file), &source, Process::Normalize),
			dir.path().join("Audio").join("tone-normalized-2.wav"),
			"never overwrites"
		);

		let (rev, _) = process(&buf, (0, 4), Process::Reverse).unwrap();
		assert_eq!(rev.channel(0)[0], buf.channel(0)[3]);
		let (quiet, _) = process(&buf, (0, buf.frames()), Process::Gain(-6.0)).unwrap();
		assert!((quiet.channel(1)[100] - buf.channel(1)[100] * 0.501).abs() < 1e-3);
		assert!(process(&buf, (10, 10), Process::Reverse).is_err());
		if !cfg!(feature = "stretch") {
			assert_eq!(
				process(&buf, (0, 100), Process::Pitch(2.0)).unwrap_err(),
				STRETCH_HINT
			);
		}

		// Swapping the file in is one undoable command.
		let (p, id) = project();
		let rendered = Rendered {
			clip: id,
			op: Process::Normalize,
			file: out.clone(),
			extra_seconds: 0.0,
		};
		let cmd = swap_in(&p, &rendered).unwrap();
		let s = exec(&p, cmd);
		assert_eq!(region(&s, id).unwrap().file, out);
		let stretched = Rendered {
			extra_seconds: 2.0,
			op: Process::Stretch(1.5),
			..rendered
		};
		let s = exec(&p, swap_in(&p, &stretched).unwrap());
		assert_eq!(region(&s, id).unwrap().clip.end(), beats(16.0));
	}
}

#[cfg(all(test, feature = "stretch"))]
mod stretch_tests {
	use super::*;

	#[test]
	fn stretch_grows_the_selection_and_pitch_keeps_the_length() {
		let sr = 8000u32;
		let tone: Vec<f32> = (0..sr).map(|i| 0.3 * (i as f32 * 0.07).sin()).collect();
		let buf = AudioBuffer::from_planar(sr, vec![tone]).unwrap();
		let (out, grown) = process(&buf, (2000, 6000), Process::Stretch(1.5)).unwrap();
		assert_eq!(grown, 2000);
		assert_eq!(out.frames(), 10_000);
		let (out, grown) = process(&buf, (0, 8000), Process::Pitch(3.0)).unwrap();
		assert_eq!((grown, out.frames()), (0, 8000));
	}
}
