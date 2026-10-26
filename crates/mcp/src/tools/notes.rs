//! Audio to MIDI (NE-3): the notes sung or played on an audio clip become a MIDI clip on an
//! instrument track, in one undo step. Shared with the app's "Convert to MIDI" dialog,
//! which analyzes once ([`analyze_clip`]) and previews conversions ([`plan_conversion`]).

use cadenza_core::{ClipId, Tick, TrackId};
use cadenza_model::{
	Clip, ClipContent, Command, Controller, ControllerLane, ControllerPoint, ModelError, Note,
	Project, Track, TrackKind,
};
use cadenza_notes::analysis::{Analysis, AnalysisOptions, Cache};
use cadenza_notes::drums::DrumHit;
use cadenza_notes::midi::{
	KeySnap, MidiConversion, MidiOptions, drums_to_midi, poly_to_midi, to_midi,
};
use cadenza_notes::poly::PolyNote;
use cadenza_notes::segment::{SegmentOptions, Segmentation, segment};
use cadenza_theory::{Scale, format_pitch, parse_pitch};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::convert::{DeviceSpec, PitchInput, build_device, tick_to_beats};
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

/// Which detector turns the audio into notes.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Algorithm {
	/// Poly when chords sound a good part of the time, else mono.
	#[default]
	Auto,
	/// One note at a time (voice, bass, lead): exact timing, intonation, pitch bends.
	Mono,
	/// Chords and several lines (piano, guitar, keys, ensembles): no pitch bends.
	Poly,
	/// Drums and percussion: each hit a General MIDI drum note (kick, snare, hats, toms,
	/// crash), for drum_machine.
	Percussive,
	/// 808s, toms, plucked or slapped bass: each hit one note at the pitch it rings at, down
	/// to B0 (slides become pitch bends when `bends` is on).
	PitchedPercussive,
}

/// Share of the sounding time with two or more notes above which `auto` picks poly.
pub const POLY_SHARE: f64 = 0.2;
/// `auto` picks percussive with at least this many hits per second and less than
/// `DRUM_VOICED` of the frames pitched.
pub const DRUM_RATE: f64 = 1.0;
pub const DRUM_VOICED: f64 = 0.3;

#[derive(Debug, Default, Clone, Deserialize, JsonSchema)]
pub struct AudioToMidiArgs {
	/// Audio clip whose notes to capture (a monophonic part: voice, bass, lead).
	pub clip_id: ClipId,
	/// audio (default) or hum; the other fields override its defaults.
	#[serde(default)]
	pub mode: ConvertMode,
	/// auto (default), mono (one line: voice, bass, lead), poly (chords: piano, guitar),
	/// percussive (drums) or pitched_percussive (808, toms: a note per hit).
	#[serde(default)]
	pub algorithm: Algorithm,
	/// Percussive: write every hit as this one note (e.g. "C1" for a sample pad) instead of
	/// the detected kit pieces.
	#[serde(default)]
	pub drum_note: Option<PitchInput>,
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
	/// Replay the sung pitch curve (slides, vibrato, intonation): a pitch-bend lane for a
	/// melody, each note's own bend for chords (poly).
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
	/// Also write the notes to this .mid as MPE: every note on its own channel with its own
	/// pitch curve (chord notes bend independently: guitar bends, slides, vibrato), for MPE
	/// synths in other programs. The project clip is written as usual.
	#[serde(default)]
	pub mpe_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct AudioToMidiOutput {
	/// The detector used: mono or poly.
	pub algorithm: Algorithm,
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
	/// The MPE file written (`mpe_path`).
	#[serde(skip_serializing_if = "Option::is_none")]
	pub mpe_file: Option<String>,
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
	/// Polyphonic notes of the region (file seconds), when detected ([`detect_poly`]).
	pub poly: Option<Vec<PolyNote>>,
	/// Drum hits of the region (file seconds), when detected ([`detect_drums`]).
	pub drums: Option<Vec<DrumHit>>,
	/// Pitched hits of the region, when detected ([`detect_hits`]).
	pub hits: Option<Box<RegionHits>>,
	/// The analysis cache folder (the polyphonic model's activations are kept there too).
	pub cache_dir: std::path::PathBuf,
}

impl ClipNotes {
	/// Share of the sounding time with two or more polyphonic notes at once (0 without
	/// them).
	pub fn polyphony(&self) -> f64 {
		self.poly
			.as_deref()
			.map_or(0.0, cadenza_notes::poly::polyphony)
	}

	/// Share of the analysis frames in the clip's region with a pitch.
	pub fn voiced_share(&self) -> f64 {
		let (a, b) = self.region;
		let frames: Vec<bool> = self
			.analysis
			.frames
			.iter()
			.filter(|f| (a..b).contains(&f.time))
			.map(|f| f.pitch.is_some())
			.collect();
		frames.iter().filter(|v| **v).count() as f64 / frames.len().max(1) as f64
	}

	/// The detector `algorithm` resolves to for these notes: percussive when hits come
	/// often and little is pitched, poly when chords sound often, else mono.
	pub fn resolve(&self, algorithm: Algorithm) -> Algorithm {
		let seconds = (self.region.1 - self.region.0).max(1e-3);
		let percussive = self.drums.as_ref().is_some_and(|d| {
			d.len() as f64 / seconds >= DRUM_RATE && self.voiced_share() < DRUM_VOICED
		});
		match algorithm {
			Algorithm::Auto if percussive => Algorithm::Percussive,
			Algorithm::Auto if self.poly.is_some() && self.polyphony() > POLY_SHARE => {
				Algorithm::Poly
			}
			Algorithm::Auto => Algorithm::Mono,
			a => a,
		}
	}
}

/// Pitched hits ([`cadenza_notes::hits`]) of a clip's region.
#[derive(Debug, Clone)]
pub struct RegionHits {
	/// File seconds at the hit track's time 0.
	pub start: f64,
	pub track: cadenza_notes::hits::HitTrack,
}

/// The mono mix of the part of the file the clip plays, its rate and the file second it
/// starts at.
fn region_samples(project: &Project, notes: &ClipNotes) -> Result<(Vec<f32>, u32, f64), McpError> {
	let (_, clip) = project
		.find_clip(notes.clip_id)
		.ok_or(ModelError::ClipNotFound(notes.clip_id))?;
	let ClipContent::Audio { file, .. } = &clip.content else {
		return Err(McpError::Invalid(format!(
			"clip {} is a MIDI clip",
			notes.clip_id
		)));
	};
	let buffer = cadenza_audio_io::decode_file(file).map_err(|e| McpError::Io {
		path: file.display().to_string(),
		message: format!("{e}; relink the clip's file (list_missing_files, relink_files)"),
	})?;
	let channels: Vec<&[f32]> = buffer.channels().iter().map(Vec::as_slice).collect();
	let mono = cadenza_notes::resynth::mono(&channels);
	let rate = buffer.sample_rate();
	let from = ((notes.region.0.max(0.0)) * f64::from(rate)) as usize;
	let to = ((notes.region.1 * f64::from(rate)) as usize).min(mono.len());
	let from = from.min(to);
	Ok((mono[from..to].to_vec(), rate, from as f64 / f64::from(rate)))
}

/// Runs the polyphonic detector (Basic Pitch) over the part of the file the clip plays and
/// keeps its notes in `notes.poly`. A few seconds per minute of audio the first time, then
/// read from the cache.
pub fn detect_poly(project: &Project, notes: &mut ClipNotes) -> Result<(), McpError> {
	let (part, rate, shift) = region_samples(project, notes)?;
	let (_, found) = cadenza_notes::poly::transcribe_cached(
		&part,
		rate,
		&cadenza_notes::poly::PolyOptions::default(),
		&notes.cache_dir,
		&mut |_| true,
	)
	.ok_or_else(|| McpError::Invalid("the analysis was cancelled".into()))?;
	notes.poly = Some(
		found
			.into_iter()
			.map(|n| PolyNote {
				onset: n.onset + shift,
				offset: n.offset + shift,
				bend: n.bend.iter().map(|&(t, v)| (t + shift, v)).collect(),
				..n
			})
			.collect(),
	);
	Ok(())
}

/// Finds the drum hits of the part of the file the clip plays (`notes.drums`). Fast.
pub fn detect_drums(project: &Project, notes: &mut ClipNotes) -> Result<(), McpError> {
	let (part, rate, shift) = region_samples(project, notes)?;
	let hits =
		cadenza_notes::drums::detect(&part, rate, &cadenza_notes::drums::DrumOptions::default());
	notes.drums = Some(
		hits.into_iter()
			.map(|h| DrumHit {
				time: h.time + shift,
				..h
			})
			.collect(),
	);
	Ok(())
}

/// Finds the pitched hits (808, toms) of the part of the file the clip plays
/// (`notes.hits`). Fast.
pub fn detect_hits(project: &Project, notes: &mut ClipNotes) -> Result<(), McpError> {
	let (part, rate, start) = region_samples(project, notes)?;
	let track =
		cadenza_notes::hits::detect(&part, rate, &cadenza_notes::hits::HitOptions::default());
	notes.hits = Some(Box::new(RegionHits { start, track }));
	Ok(())
}

/// Every detector `algorithm` may need (mono is always there).
pub fn detect_for(
	project: &Project,
	notes: &mut ClipNotes,
	algorithm: Algorithm,
) -> Result<(), McpError> {
	if matches!(algorithm, Algorithm::Auto | Algorithm::Percussive) && notes.drums.is_none() {
		detect_drums(project, notes)?;
	}
	if matches!(algorithm, Algorithm::Auto | Algorithm::Poly) && notes.poly.is_none() {
		detect_poly(project, notes)?;
	}
	if algorithm == Algorithm::PitchedPercussive && notes.hits.is_none() {
		detect_hits(project, notes)?;
	}
	Ok(())
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
		poly: None,
		drums: None,
		hits: None,
		cache_dir: cache_dir.to_path_buf(),
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

/// Converts analyzed clip notes with `opts`, restricted to what the clip plays, with the
/// detector `algorithm` resolves to (poly needs [`detect_poly`] first, else mono is used).
pub fn convert(
	project: &Project,
	notes: &ClipNotes,
	opts: &MidiOptions,
	algorithm: Algorithm,
	drum_note: Option<u8>,
) -> MidiConversion {
	let opts = MidiOptions {
		region: Some(notes.region),
		..opts.clone()
	};
	match notes.resolve(algorithm) {
		Algorithm::Poly if let Some(poly) = &notes.poly => MidiConversion {
			// Chords bend per note (a pitch-bend lane would bend the whole chord).
			note_bends: opts.bends,
			..poly_to_midi(poly, &project.tempo, notes.origin, &opts)
		},
		Algorithm::Percussive if let Some(hits) = &notes.drums => {
			drums_to_midi(hits, &project.tempo, notes.origin, &opts, drum_note)
		}
		Algorithm::PitchedPercussive if let Some(h) = &notes.hits => {
			let opts = MidiOptions {
				region: Some((notes.region.0 - h.start, notes.region.1 - h.start)),
				..opts
			};
			to_midi(
				&h.track.segmentation,
				&h.track.analysis,
				&project.tempo,
				notes.origin + h.start,
				&opts,
			)
		}
		_ => to_midi(
			&notes.segmentation,
			&notes.analysis,
			&project.tempo,
			notes.origin,
			&opts,
		),
	}
}

/// A converted note's curve as its own bend (ticks from its start); none when it stays at 0.
fn note_bend(n: &cadenza_notes::midi::MidiNote) -> Vec<cadenza_model::NoteBendPoint> {
	let start = Tick::from_beats(n.start);
	let mut out: Vec<cadenza_model::NoteBendPoint> = Vec::with_capacity(n.curve.len());
	for &(beat, semitones) in &n.curve {
		let tick = (Tick::from_beats(beat) - start).max(Tick::ZERO);
		let semitones =
			semitones.clamp(-cadenza_model::MAX_NOTE_BEND, cadenza_model::MAX_NOTE_BEND);
		match out.last_mut() {
			Some(last) if last.tick >= tick => last.semitones = semitones,
			_ => out.push(cadenza_model::NoteBendPoint { tick, semitones }),
		}
	}
	out.truncate(cadenza_model::MAX_NOTE_BEND_POINTS);
	if out.iter().all(|p| p.semitones == 0.0) {
		out.clear();
	}
	out
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
	algorithm: Algorithm,
) -> Result<(Command, AudioToMidiOutput), McpError> {
	let algorithm = notes.resolve(algorithm);
	let drums = algorithm == Algorithm::Percussive;
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
	// Chords: a note starting while another (of another pitch) still sounds, often.
	let overlapping = conversion
		.notes
		.windows(2)
		.filter(|w| w[1].start + 1e-6 < w[0].start + w[0].length)
		.count();
	let chords = overlapping * 4 >= conversion.notes.len();
	let mut midi_notes = Vec::with_capacity(conversion.notes.len());
	let mut end = audio.length;
	for n in &conversion.notes {
		let a = Tick::from_beats(n.start - start_beat).max(Tick::ZERO);
		let b = Tick::from_beats(n.start + n.length - start_beat);
		if b <= a {
			continue;
		}
		end = end.max(b);
		let mut note = Note::new(n.pitch, a, b - a, n.velocity);
		if conversion.note_bends {
			note.bend = note_bend(n);
		}
		midi_notes.push(note);
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
	if algorithm == Algorithm::PitchedPercussive
		&& let Some(h) = &notes.hits
		&& h.track.unpitched > 0
	{
		warnings.push(format!(
			"{} hits without a steady pitch were left out (algorithm percussive writes them as drum notes)",
			h.track.unpitched
		));
	}
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
				_ if drums => ("drum_machine", None),
				..48 if algorithm == Algorithm::PitchedPercussive => {
					("poly_synth", Some("808 Clean"))
				}
				_ if chords => ("electric_piano", None),
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
			let name = if drums {
				format!("{} Drums", source.name)
			} else {
				format!("{} MIDI", source.name)
			};
			let mut track = Track::new(name, TrackKind::Instrument);
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
			algorithm,
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
			mpe_file: None,
			warnings,
		},
	))
}

/// A conversion as an MPE file ([`cadenza_midi_io::export_mpe`]): song ticks, each note's
/// curve as its own channel's pitch bend. Pure.
pub fn mpe_export(
	project: &Project,
	conversion: &MidiConversion,
	name: &str,
) -> Result<cadenza_midi_io::MpeExport, McpError> {
	let notes: Vec<cadenza_midi_io::MpeNote> = conversion
		.notes
		.iter()
		.map(|n| {
			let start = Tick::from_beats(n.start).max(Tick::ZERO);
			cadenza_midi_io::MpeNote {
				start,
				length: Tick::from_beats(n.start + n.length) - start,
				pitch: n.pitch,
				velocity: n.velocity,
				curve: n
					.curve
					.iter()
					.map(|&(beat, v)| (Tick::from_beats(beat).max(start), v))
					.collect(),
				pressure: Vec::new(),
				timbre: Vec::new(),
			}
		})
		.collect();
	let first = notes.iter().map(|n| n.start).min().unwrap_or(Tick::ZERO);
	cadenza_midi_io::export_mpe(
		&notes,
		&project.tempo,
		&project.meter().signature_at(first),
		name,
	)
	.map_err(|e| McpError::Invalid(e.to_string()))
}

/// Writes [`mpe_export`] to `path` (.mid); returns the path and a warning when notes had to
/// share channels.
pub fn write_mpe(
	project: &Project,
	conversion: &MidiConversion,
	name: &str,
	path: &str,
) -> Result<(String, Option<String>), McpError> {
	let path = std::path::PathBuf::from(path.trim());
	let ext_ok = path
		.extension()
		.and_then(|e| e.to_str())
		.is_some_and(|e| e.eq_ignore_ascii_case("mid") || e.eq_ignore_ascii_case("midi"));
	if !ext_ok {
		return Err(McpError::Invalid(format!(
			"mpe_path '{}' must end in .mid",
			path.display()
		)));
	}
	let out = mpe_export(project, conversion, name)?;
	std::fs::write(&path, &out.bytes).map_err(|e| McpError::Io {
		path: path.display().to_string(),
		message: e.to_string(),
	})?;
	let warning = (out.shared > 0).then(|| {
		format!(
			"{} notes sounded with 15 others and share an MPE channel: their bends move each other",
			out.shared
		)
	});
	Ok((path.display().to_string(), warning))
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
	let mut notes = notes;
	detect_for(project, &mut notes, args.algorithm)?;
	let conversion = convert(
		project,
		&notes,
		&opts,
		args.algorithm,
		args.drum_note.map(|p| p.0),
	);
	let (command, mut out) =
		conversion_command(project, &notes, &conversion, args.track_id, args.algorithm)?;
	if let Some(path) = &args.mpe_path {
		let name = project
			.find_clip(args.clip_id)
			.map_or_else(|| "MPE".to_owned(), |(_, c)| c.name.clone());
		let (file, warning) = write_mpe(project, &conversion, &name, path)?;
		out.mpe_file = Some(file);
		out.warnings.extend(warning);
	}
	session.document_mut().execute(command)?;
	Ok(out)
}
