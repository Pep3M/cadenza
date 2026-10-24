//! Audio to MIDI (NE-3): the notes sung or played on an audio clip become a MIDI clip on an
//! instrument track, in one undo step. Shared with the app's "Convert to MIDI" dialog,
//! which analyzes once ([`analyze_clip`]) and previews conversions ([`plan_conversion`]).

use cadenza_core::{ClipId, Tick, TrackId};
use cadenza_model::{
	Clip, ClipContent, Command, Controller, ControllerLane, ControllerPoint, ModelError, Note,
	Project, Track, TrackKind,
};
use cadenza_notes::analysis::{Analysis, AnalysisOptions, Cache};
use cadenza_notes::midi::{KeySnap, MidiConversion, MidiOptions, to_midi};
use cadenza_notes::segment::{SegmentOptions, Segmentation, segment};
use cadenza_theory::{Scale, format_pitch, parse_pitch};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::convert::{DeviceSpec, build_device, tick_to_beats};
use crate::error::McpError;
use crate::session::Session;

/// Conversion defaults.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConvertMode {
	/// Faithful: sung timing, nearest semitone, no key.
	#[default]
	Audio,
	/// A hummed or sung idea: sixteenth grid, key inferred and snapped, repeats merged.
	Hum,
}

#[derive(Debug, Default, Clone, Deserialize, JsonSchema)]
pub struct AudioToMidiArgs {
	/// Audio clip whose notes to capture (a monophonic part: voice, bass, lead).
	pub clip_id: ClipId,
	/// audio (default) or hum; the other fields override its defaults.
	#[serde(default)]
	pub mode: ConvertMode,
	/// Instrument track to write the clip on; omit to create one below the audio track
	/// (bass, keys or lead sound picked by register).
	#[serde(default)]
	pub track_id: Option<TrackId>,
	/// Grid in beats (0.25 = sixteenths, 0 = keep the sung timing).
	#[serde(default)]
	pub quantize_beats: Option<f64>,
	/// 0..1 (default 1): how far notes move toward the grid.
	#[serde(default)]
	pub quantize_strength: Option<f64>,
	/// "off", "auto" (infer) or a scale like "A minor" / "F# dorian": pitches snap into it.
	#[serde(default)]
	pub key: Option<String>,
	/// Write a pitch-bend lane replaying the sung pitch curve (slides, vibrato, intonation).
	#[serde(default)]
	pub bends: Option<bool>,
	/// Semitones of a full bend on the target instrument (default 2).
	#[serde(default)]
	pub bend_range: Option<f64>,
	/// Shorter notes are dropped (default 0.03 s, hum 0.08 s).
	#[serde(default)]
	pub min_length_seconds: Option<f64>,
	/// Same-pitch notes less than 50 ms apart become one.
	#[serde(default)]
	pub merge_repeats: Option<bool>,
	#[serde(default)]
	pub octave_shift: Option<i32>,
	/// Fold notes by octaves into lowest..highest (note names or MIDI numbers).
	#[serde(default)]
	pub lowest: Option<String>,
	#[serde(default)]
	pub highest: Option<String>,
	/// Fixed velocity 0..1; default follows each note's loudness.
	#[serde(default)]
	pub velocity: Option<f32>,
	/// Note detection sensitivity: above 1 finds more notes, below 1 fewer (default 1).
	#[serde(default)]
	pub sensitivity: Option<f64>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct AudioToMidiOutput {
	pub track_id: TrackId,
	pub created_track: bool,
	pub clip_id: ClipId,
	pub note_count: usize,
	pub start_beat: f64,
	pub length_beats: f64,
	/// Key the pitches were snapped to.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub key: Option<String>,
	/// Lowest and highest note, e.g. "C3-G4".
	#[serde(skip_serializing_if = "Option::is_none")]
	pub range: Option<String>,
	pub bend_points: usize,
	/// The first notes: "E4 @2.00 x0.50 (-12 c)" = pitch, beat in the song, length, sung
	/// intonation.
	pub notes: Vec<String>,
	pub warnings: Vec<String>,
}

/// An audio clip's notes, analyzed once.
#[derive(Debug, Clone)]
pub struct ClipNotes {
	pub clip_id: ClipId,
	pub track_id: TrackId,
	pub analysis: Analysis,
	pub segmentation: Segmentation,
	/// Song seconds at file time 0.
	pub origin: f64,
	/// File seconds the clip plays.
	pub region: (f64, f64),
}

/// The analysis cache folder in the user cache folder.
pub fn default_cache_dir() -> std::path::PathBuf {
	cadenza_core::paths::cache_dir().join("analysis")
}

/// Decodes and analyzes the file of an audio clip (cached in `cache_dir` by content) and
/// segments it. Slow on first use of a file (~0.1 s per minute of audio); allocates freely.
pub fn analyze_clip(
	project: &Project,
	clip_id: ClipId,
	sensitivity: f64,
	cache_dir: &std::path::Path,
) -> Result<ClipNotes, McpError> {
	let (track, clip) = project
		.find_clip(clip_id)
		.ok_or(ModelError::ClipNotFound(clip_id))?;
	let ClipContent::Audio {
		file,
		offset_seconds,
		..
	} = &clip.content
	else {
		return Err(McpError::Invalid(format!(
			"clip {clip_id} is a MIDI clip; audio_to_midi converts audio clips (get_project lists them)"
		)));
	};
	if !(sensitivity.is_finite() && sensitivity > 0.0) {
		return Err(McpError::Invalid(format!(
			"sensitivity {sensitivity} is invalid; use a value above 0 (1 = default, 2 = more notes, 0.5 = fewer)"
		)));
	}
	let buffer = cadenza_audio_io::decode_file(file).map_err(|e| McpError::Io {
		path: file.display().to_string(),
		message: format!("{e}; relink the clip's file (list_missing_files, relink_files)"),
	})?;
	let mono = buffer.to_mono();
	let cache = Cache::new(cache_dir.to_path_buf());
	let analysis = cache
		.get_or_analyze(
			mono.channel(0),
			mono.sample_rate(),
			&AnalysisOptions::default(),
			&mut |_| true,
		)
		.ok_or_else(|| McpError::Invalid("the analysis was cancelled".into()))?;
	let segmentation = segment(
		&analysis,
		&SegmentOptions {
			sensitivity,
			..SegmentOptions::default()
		},
		&[],
	);
	let start = project.tempo.tick_to_seconds(clip.start);
	let origin = start - offset_seconds;
	let region = (
		*offset_seconds,
		offset_seconds + clip.duration_seconds(&project.tempo),
	);
	Ok(ClipNotes {
		clip_id,
		track_id: track.id,
		analysis,
		segmentation,
		origin,
		region,
	})
}

/// The conversion options an `AudioToMidiArgs` asks for.
pub fn midi_options(args: &AudioToMidiArgs) -> Result<MidiOptions, McpError> {
	let mut o = match args.mode {
		ConvertMode::Audio => MidiOptions::default(),
		ConvertMode::Hum => MidiOptions::hum(),
	};
	if let Some(q) = args.quantize_beats {
		if !(q.is_finite() && q >= 0.0) {
			return Err(McpError::Invalid(format!(
				"quantize_beats {q} is invalid; use a grid in beats like 0.25 (sixteenths), or 0 for none"
			)));
		}
		o.quantize = (q > 0.0).then_some(q);
	}
	if let Some(s) = args.quantize_strength {
		if !(0.0..=1.0).contains(&s) {
			return Err(McpError::Invalid(format!(
				"quantize_strength {s} is invalid; use 0..1"
			)));
		}
		o.quantize_strength = s;
	}
	if let Some(k) = &args.key {
		o.key = match k.trim().to_ascii_lowercase().as_str() {
			"" | "off" | "none" => KeySnap::Off,
			"auto" => KeySnap::Auto,
			_ => KeySnap::Scale(Scale::parse(k).map_err(|_| {
				McpError::Invalid(format!(
					"key '{k}' is not understood; use \"off\", \"auto\" or a scale like \"A minor\", \"Eb major\", \"F# dorian\""
				))
			})?),
		};
	}
	if let Some(b) = args.bends {
		o.bends = b;
	}
	if let Some(r) = args.bend_range {
		if !(r > 0.0 && r <= 48.0) {
			return Err(McpError::Invalid(format!(
				"bend_range {r} is invalid; use the instrument's range in semitones (1..48, usually 2)"
			)));
		}
		o.bend_range = r;
	}
	if let Some(m) = args.min_length_seconds {
		if !(m.is_finite() && m >= 0.0) {
			return Err(McpError::Invalid(format!(
				"min_length_seconds {m} is invalid; use 0 or more"
			)));
		}
		o.min_length = m;
	}
	if let Some(m) = args.merge_repeats {
		o.merge_repeats = m;
	}
	if let Some(s) = args.octave_shift {
		if !(-4..=4).contains(&s) {
			return Err(McpError::Invalid(format!(
				"octave_shift {s} is invalid; use -4..4"
			)));
		}
		o.octave_shift = s;
	}
	match (&args.lowest, &args.highest) {
		(None, None) => {}
		(Some(l), Some(h)) => {
			let (l, h) = (parse_pitch(l)?, parse_pitch(h)?);
			if h < l + 11 {
				return Err(McpError::Invalid(format!(
					"lowest..highest {}..{} is narrower than an octave; folding by octaves needs at least 12 semitones",
					format_pitch(l, false),
					format_pitch(h, false)
				)));
			}
			o.range = Some((l, h));
		}
		_ => {
			return Err(McpError::Invalid(
				"pass both lowest and highest (e.g. \"E1\" and \"G3\" for a bass), or neither"
					.into(),
			));
		}
	}
	if let Some(v) = args.velocity {
		if !(0.0..=1.0).contains(&v) || v == 0.0 {
			return Err(McpError::Invalid(format!(
				"velocity {v} is invalid; use 0..1 (above 0)"
			)));
		}
		o.velocity = Some(v);
	}
	Ok(o)
}

/// Converts analyzed clip notes with `opts`, restricted to what the clip plays.
pub fn convert(project: &Project, notes: &ClipNotes, opts: &MidiOptions) -> MidiConversion {
	let opts = MidiOptions {
		region: Some(notes.region),
		..opts.clone()
	};
	to_midi(
		&notes.segmentation,
		&notes.analysis,
		&project.tempo,
		notes.origin,
		&opts,
	)
}

/// "C major", "F# dorian".
pub fn scale_name(scale: &Scale) -> String {
	let kind = serde_json::to_value(scale.kind)
		.ok()
		.and_then(|v| v.as_str().map(|s| s.replace('_', " ")))
		.unwrap_or_default();
	format!("{} {kind}", scale.root.name(false))
}

/// The command writing a conversion: one MIDI clip over the audio clip's span (grown to the
/// last note) on `target`, or on a new instrument track below the audio track. Pure.
pub fn conversion_command(
	project: &Project,
	notes: &ClipNotes,
	conversion: &MidiConversion,
	target: Option<TrackId>,
) -> Result<(Command, AudioToMidiOutput), McpError> {
	let (_, audio) = project
		.find_clip(notes.clip_id)
		.ok_or(ModelError::ClipNotFound(notes.clip_id))?;
	if conversion.notes.is_empty() {
		return Err(McpError::Invalid(format!(
			"no notes found in clip '{}'; it needs a monophonic pitched part (voice, bass, lead). Try a higher \
			 sensitivity or a lower min_length_seconds",
			audio.name
		)));
	}
	let start_beat = tick_to_beats(audio.start);
	let mut midi_notes = Vec::with_capacity(conversion.notes.len());
	let mut end = audio.length;
	for n in &conversion.notes {
		let a = Tick::from_beats(n.start - start_beat).max(Tick::ZERO);
		let b = Tick::from_beats(n.start + n.length - start_beat);
		if b <= a {
			continue;
		}
		end = end.max(b);
		midi_notes.push(Note::new(n.pitch, a, b - a, n.velocity));
	}
	let mut controllers = Vec::new();
	if !conversion.bends.is_empty() {
		let mut points: Vec<ControllerPoint> = Vec::with_capacity(conversion.bends.len());
		for b in &conversion.bends {
			let tick = Tick::from_beats(b.beat - start_beat).max(Tick::ZERO);
			match points.last_mut() {
				Some(last) if last.tick == tick => last.value = b.value,
				_ => points.push(ControllerPoint::new(tick, b.value)),
			}
		}
		controllers.push(ControllerLane {
			controller: Controller::PitchBend,
			points,
		});
	}
	let name = format!("{} (MIDI)", audio.name);
	let mut clip = Clip::midi(name.clone(), audio.start, end);
	clip.content = ClipContent::Midi {
		notes: midi_notes,
		controllers,
	};
	let clip_id = clip.id;
	let low = conversion.notes.iter().map(|n| n.pitch).min().unwrap_or(60);
	let high = conversion.notes.iter().map(|n| n.pitch).max().unwrap_or(60);
	let mut warnings = Vec::new();
	if conversion.dropped > 0 {
		warnings.push(format!(
			"{} very short or overlapping notes were left out (min_length_seconds)",
			conversion.dropped
		));
	}
	let (track_id, created, command) = match target {
		Some(id) => {
			let t = project.track(id).ok_or(ModelError::TrackNotFound(id))?;
			if t.kind != TrackKind::Instrument {
				return Err(McpError::Invalid(format!(
					"track {id} is a {:?} track; write the MIDI to an Instrument track or omit track_id",
					t.kind
				)));
			}
			if t.clips.iter().any(|c| {
				c.plays() && c.start < audio.start + end && audio.start < c.start + c.length
			}) {
				warnings.push(format!(
					"the new clip overlaps clips already on '{}'",
					t.name
				));
			}
			(id, false, Command::AddClip { track: id, clip })
		}
		None => {
			let mut sorted: Vec<u8> = conversion.notes.iter().map(|n| n.pitch).collect();
			sorted.sort_unstable();
			let (device, preset) = match sorted[sorted.len() / 2] {
				..48 => ("poly_synth", Some("Sub Bass")),
				48..60 => ("electric_piano", None),
				_ => ("poly_synth", Some("Night Drive Lead")),
			};
			let spec = DeviceSpec {
				preset: preset.map(str::to_owned),
				..DeviceSpec::new(device)
			};
			let instrument = build_device(&spec, cadenza_dsp::DeviceRole::Instrument, None)
				.or_else(|_| {
					build_device(
						&DeviceSpec::new(device),
						cadenza_dsp::DeviceRole::Instrument,
						None,
					)
				})?;
			let source = project
				.track(notes.track_id)
				.ok_or(ModelError::TrackNotFound(notes.track_id))?;
			let mut track = Track::new(format!("{} MIDI", source.name), TrackKind::Instrument);
			track.instrument = Some(instrument);
			track.color = source.color.clone();
			track.folder = source.folder;
			let id = track.id;
			let index = project
				.tracks
				.iter()
				.position(|t| t.id == source.id)
				.map(|i| i + 1);
			(
				id,
				true,
				Command::Batch {
					label: format!("Convert '{}' to MIDI", audio.name),
					commands: vec![
						Command::AddTrack { track, index },
						Command::AddClip { track: id, clip },
					],
				},
			)
		}
	};
	let command = match command {
		Command::AddClip { .. } => Command::Batch {
			label: format!("Convert '{}' to MIDI", audio.name),
			commands: vec![command],
		},
		c => c,
	};
	let flats = conversion.key.as_ref().is_some_and(prefers_flats);
	let preview = conversion
		.notes
		.iter()
		.take(16)
		.map(|n| {
			format!(
				"{} @{:.2} x{:.2} ({:+.0} c)",
				format_pitch(n.pitch, flats),
				n.start,
				n.length,
				n.cents
			)
		})
		.collect();
	Ok((
		command,
		AudioToMidiOutput {
			track_id,
			created_track: created,
			clip_id,
			note_count: conversion.notes.len(),
			start_beat,
			length_beats: tick_to_beats(end),
			key: conversion.key.as_ref().map(scale_name),
			range: Some(format!(
				"{}-{}",
				format_pitch(low, flats),
				format_pitch(high, flats)
			)),
			bend_points: conversion.bends.len(),
			notes: preview,
			warnings,
		},
	))
}

/// Keys written with flats (F major and the flat keys, their relative minors).
fn prefers_flats(scale: &Scale) -> bool {
	let major_root = match scale.kind {
		cadenza_theory::ScaleKind::NaturalMinor => (scale.root.0 + 3) % 12,
		_ => scale.root.0,
	};
	matches!(major_root, 1 | 3 | 5 | 8 | 10)
}

/// MCP `audio_to_midi`: analyze, convert and write in one undo step.
pub fn audio_to_midi(
	session: &mut Session,
	args: AudioToMidiArgs,
) -> Result<AudioToMidiOutput, McpError> {
	let opts = midi_options(&args)?;
	let project = session.document().project();
	let notes = analyze_clip(
		project,
		args.clip_id,
		args.sensitivity.unwrap_or(1.0),
		&default_cache_dir(),
	)?;
	let conversion = convert(project, &notes, &opts);
	let (command, out) = conversion_command(project, &notes, &conversion, args.track_id)?;
	session.document_mut().execute(command)?;
	Ok(out)
}
