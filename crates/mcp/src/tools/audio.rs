//! Audio files: importing them as clips, inspecting them, and clip fades.

use std::path::{Path, PathBuf};

use cadenza_core::{ClipId, TempoMap, Tick, TrackId};
use cadenza_model::{
	Clip, ClipContent, ClipFade, Command, FadeCurve, ModelError, Track, TrackKind,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::convert::{beats_to_tick, tick_to_beats};
use crate::error::McpError;
use crate::session::Session;

/// Audio file extensions the decoder understands.
pub const AUDIO_EXTENSIONS: &[&str] = &[
	"wav", "wave", "aif", "aiff", "flac", "mp3", "ogg", "oga", "m4a", "mp4", "caf", "mka",
];

fn round3(x: f64) -> f64 {
	(x * 1000.0).round() / 1000.0
}

fn probe(path: &Path) -> Result<cadenza_audio_io::AudioInfo, McpError> {
	cadenza_audio_io::probe(path).map_err(|e| McpError::Invalid(e.to_string()))
}

/// Beats spanned by `seconds` of audio starting at `start` under `tempo`.
fn seconds_to_beats_at(tempo: &TempoMap, start: Tick, seconds: f64) -> f64 {
	let end = tempo.seconds_to_tick(tempo.tick_to_seconds(start) + seconds);
	(end - start).beats()
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct AudioFileInfo {
	pub path: String,
	pub duration_seconds: f64,
	/// Duration in beats at the project tempo, from `at_beat`.
	pub duration_beats: f64,
	/// Beat the beat duration was measured from (tempo changes make it position dependent).
	pub at_beat: f64,
	pub sample_rate: u32,
	pub channels: u16,
	/// Container/codec family (wav, flac, mp3, ogg_vorbis, aac, ...).
	pub format: String,
	pub codec: String,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub bits_per_sample: Option<u32>,
}

fn file_info(path: &Path, tempo: &TempoMap, at: Tick) -> Result<AudioFileInfo, McpError> {
	let info = probe(path)?;
	Ok(AudioFileInfo {
		path: path.display().to_string(),
		duration_seconds: round3(info.duration_seconds),
		duration_beats: round3(seconds_to_beats_at(tempo, at, info.duration_seconds)),
		at_beat: tick_to_beats(at),
		sample_rate: info.sample_rate,
		channels: info.channels,
		format: serde_json::to_value(info.format)
			.ok()
			.and_then(|v| v.as_str().map(str::to_owned))
			.unwrap_or_default(),
		codec: info.codec,
		bits_per_sample: info.bits_per_sample,
	})
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ImportAudioArgs {
	/// Audio file to import (WAV, AIFF, FLAC, MP3, OGG/Vorbis, M4A, CAF). Any sample rate:
	/// it is converted to the engine rate when played.
	pub path: String,
	/// Audio track to put the clip on; omit to create a new Audio track named after the file.
	#[serde(default)]
	pub track_id: Option<TrackId>,
	/// Clip start in beats (default 0).
	#[serde(default)]
	pub at_beats: Option<f64>,
	/// Clip name (default: the file name).
	#[serde(default)]
	pub name: Option<String>,
	/// Clip gain in dB (default 0).
	#[serde(default)]
	pub gain_db: Option<f32>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ImportAudioOutput {
	pub track_id: TrackId,
	/// True when a new Audio track was created for the clip.
	pub created_track: bool,
	pub clip_id: ClipId,
	pub start_beat: f64,
	/// Clip length in beats (the whole file at the project tempo).
	pub length_beats: f64,
	pub file: AudioFileInfo,
}

/// Imports an audio file as a clip covering the whole file, in one undoable step (with
/// the new track, when one is created). The stored path is absolute so the project still
/// finds the file when opened from another folder.
pub fn import_audio(
	session: &mut Session,
	args: ImportAudioArgs,
) -> Result<ImportAudioOutput, McpError> {
	let raw = PathBuf::from(args.path.trim());
	let path = cadenza_core::paths::real_path(&raw).map_err(|e| {
		McpError::Invalid(format!(
			"cannot access audio file '{}': {e}; pass an existing file path",
			raw.display()
		))
	})?;
	let at_beats = args.at_beats.unwrap_or(0.0);
	if !(at_beats.is_finite() && at_beats >= 0.0) {
		return Err(McpError::Invalid(format!(
			"at_beats {at_beats} is invalid; use a beat position >= 0"
		)));
	}
	let project = session.document().project();
	let start = beats_to_tick(at_beats);
	let file = file_info(&path, &project.tempo, start)?;
	if file.duration_seconds <= 0.0 {
		return Err(McpError::Invalid(format!(
			"audio file '{}' contains no audio frames; pick another file",
			path.display()
		)));
	}
	let stem = path
		.file_stem()
		.and_then(|s| s.to_str())
		.unwrap_or("Audio")
		.to_owned();
	let end = project
		.tempo
		.seconds_to_tick(project.tempo.tick_to_seconds(start) + file.duration_seconds);
	let length = Tick((end - start).0.max(1));
	let mut clip = Clip::audio(
		args.name.unwrap_or_else(|| stem.clone()),
		start,
		length,
		&path,
	);
	if let ClipContent::Audio { gain_db, .. } = &mut clip.content {
		*gain_db = args.gain_db.unwrap_or(0.0);
	}
	let clip_id = clip.id;
	let (track_id, created_track, command) = match args.track_id {
		Some(track) => (track, false, Command::AddClip { track, clip }),
		None => {
			let track = Track::new(stem.clone(), TrackKind::Audio);
			let id = track.id;
			(
				id,
				true,
				Command::Batch {
					label: format!("Import audio '{stem}'"),
					commands: vec![
						Command::AddTrack { track, index: None },
						Command::AddClip { track: id, clip },
					],
				},
			)
		}
	};
	session.document_mut().execute(command)?;
	Ok(ImportAudioOutput {
		track_id,
		created_track,
		clip_id,
		start_beat: tick_to_beats(start),
		length_beats: tick_to_beats(length),
		file,
	})
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct GetAudioInfoArgs {
	/// An audio file to inspect; or pass `clip_id` instead.
	#[serde(default)]
	pub path: Option<String>,
	/// An audio clip of the project: inspects its file, measuring beats from the clip start.
	#[serde(default)]
	pub clip_id: Option<ClipId>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct AudioClipInfo {
	pub clip_id: ClipId,
	pub start_beat: f64,
	pub length_beats: f64,
	pub length_seconds: f64,
	/// Seconds into the file heard at the clip start.
	pub offset_seconds: f64,
	pub gain_db: f32,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub fade_in: Option<ClipFade>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub fade_out: Option<ClipFade>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct GetAudioInfoOutput {
	pub file: AudioFileInfo,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub clip: Option<AudioClipInfo>,
}

pub fn get_audio_info(
	session: &Session,
	args: GetAudioInfoArgs,
) -> Result<GetAudioInfoOutput, McpError> {
	let project = session.document().project();
	match (args.path, args.clip_id) {
		(Some(path), None) => Ok(GetAudioInfoOutput {
			file: file_info(Path::new(path.trim()), &project.tempo, Tick::ZERO)?,
			clip: None,
		}),
		(None, Some(id)) => {
			let (_, clip) = project.find_clip(id).ok_or(ModelError::ClipNotFound(id))?;
			let ClipContent::Audio {
				file,
				offset_seconds,
				gain_db,
				fade_in,
				fade_out,
			} = &clip.content
			else {
				return Err(ModelError::NotAnAudioClip(id).into());
			};
			Ok(GetAudioInfoOutput {
				file: file_info(file, &project.tempo, clip.start)?,
				clip: Some(AudioClipInfo {
					clip_id: id,
					start_beat: tick_to_beats(clip.start),
					length_beats: tick_to_beats(clip.length),
					length_seconds: round3(clip.duration_seconds(&project.tempo)),
					offset_seconds: *offset_seconds,
					gain_db: *gain_db,
					fade_in: *fade_in,
					fade_out: *fade_out,
				}),
			})
		}
		_ => Err(McpError::Invalid(
			"pass exactly one of 'path' (an audio file) or 'clip_id' (an audio clip)".into(),
		)),
	}
}

/// One side of a fade: a length in seconds or beats (not both); 0 removes the fade.
#[derive(Debug, Default, Clone, Copy, Deserialize, JsonSchema)]
pub struct FadeInput {
	#[serde(default)]
	pub seconds: Option<f64>,
	/// Beats are converted to seconds at the clip's edge with the project tempo.
	#[serde(default)]
	pub beats: Option<f64>,
	/// linear (default), equal_power (crossfades of unrelated material), exponential (long
	/// fade-outs), s_curve.
	#[serde(default)]
	pub curve: Option<FadeCurve>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SetClipFadesArgs {
	pub clip_id: ClipId,
	/// Omit to keep the current fade-in.
	#[serde(default)]
	pub fade_in: Option<FadeInput>,
	/// Omit to keep the current fade-out.
	#[serde(default)]
	pub fade_out: Option<FadeInput>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct SetClipFadesOutput {
	#[serde(skip_serializing_if = "Option::is_none")]
	pub fade_in: Option<ClipFade>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub fade_out: Option<ClipFade>,
	pub clip_length_seconds: f64,
}

/// Seconds of a fade given in beats, measured inward from the clip edge.
fn fade_seconds(
	input: FadeInput,
	current: Option<ClipFade>,
	tempo: &TempoMap,
	edge: Tick,
	inward: bool,
	what: &str,
) -> Result<Option<ClipFade>, McpError> {
	let seconds = match (input.seconds, input.beats) {
		(Some(_), Some(_)) => {
			return Err(McpError::Invalid(format!(
				"{what}: give either seconds or beats, not both"
			)));
		}
		(Some(s), None) => s,
		(None, Some(b)) => {
			if !(b.is_finite() && b >= 0.0) {
				return Err(McpError::Invalid(format!(
					"{what} beats {b} is invalid; use a length >= 0 (0 removes the fade)"
				)));
			}
			let other = if inward {
				edge + beats_to_tick(b)
			} else {
				Tick((edge - beats_to_tick(b)).0.max(0))
			};
			(tempo.tick_to_seconds(other) - tempo.tick_to_seconds(edge)).abs()
		}
		(None, None) => match (current, input.curve) {
			(Some(c), Some(curve)) => return Ok(Some(ClipFade::new(c.seconds, curve))),
			(c, None) => return Ok(c),
			(None, Some(_)) => {
				return Err(McpError::Invalid(format!(
					"{what}: the clip has no {what} yet; give its length in seconds or beats"
				)));
			}
		},
	};
	if !(seconds.is_finite() && seconds >= 0.0) {
		return Err(McpError::Invalid(format!(
			"{what} seconds {seconds} is invalid; use a length >= 0 (0 removes the fade)"
		)));
	}
	if seconds == 0.0 {
		return Ok(None);
	}
	let curve = input.curve.or(current.map(|c| c.curve)).unwrap_or_default();
	Ok(Some(ClipFade::new(seconds, curve)))
}

pub fn set_clip_fades(
	session: &mut Session,
	args: SetClipFadesArgs,
) -> Result<SetClipFadesOutput, McpError> {
	if args.fade_in.is_none() && args.fade_out.is_none() {
		return Err(McpError::Invalid(
			"set_clip_fades: pass fade_in and/or fade_out (a length of 0 removes a fade)".into(),
		));
	}
	let project = session.document().project();
	let (_, clip) = project
		.find_clip(args.clip_id)
		.ok_or(ModelError::ClipNotFound(args.clip_id))?;
	if clip.is_midi() {
		return Err(ModelError::NotAnAudioClip(args.clip_id).into());
	}
	let (cur_in, cur_out) = clip.fades();
	let tempo = &project.tempo;
	let fade_in = match args.fade_in {
		Some(f) => fade_seconds(f, cur_in, tempo, clip.start, true, "fade_in")?,
		None => cur_in,
	};
	let fade_out = match args.fade_out {
		Some(f) => fade_seconds(f, cur_out, tempo, clip.end(), false, "fade_out")?,
		None => cur_out,
	};
	let clip_length_seconds = round3(clip.duration_seconds(tempo));
	session.document_mut().execute(Command::SetClipFades {
		clip: args.clip_id,
		fade_in,
		fade_out,
	})?;
	Ok(SetClipFadesOutput {
		fade_in,
		fade_out,
		clip_length_seconds,
	})
}
