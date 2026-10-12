//! Export jobs: a mixdown and/or stems of a time range, in any export format, at the
//! render rate or resampled, normalized (peak, or loudness with a true-peak ceiling),
//! with metadata, progress and cancellation.
//!
//! One offline pass renders the mix and taps every stem (a track's post- or pre-fader
//! signal, latency aligned) into temporary 32-bit float files next to the outputs. Stems
//! "through the master chain" are then each played through a copy of the master channel
//! (inserts, fader, master automation). Finally every file is gained, resampled,
//! measured and encoded (dithered below 32-bit float).

use std::path::{Path, PathBuf};

use cadenza_audio_io::{AudioBuffer, ExportFormat, ExportOptions, Metadata};
use cadenza_core::{Tick, TrackId};
use cadenza_model::{Clip, Output, Project, Track, TrackKind};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::analysis::{Analyzer, LevelStats, MixReport};
use crate::error::EngineError;
use crate::render::{RenderOptions, prepare_range, render_analyzed_with, render_offline};
use crate::{FLOOR_DB, MP3_SUPPORTED, QUANTUM};

/// Which stems to write.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", content = "tracks", rename_all = "snake_case")]
pub enum StemSet {
	#[default]
	None,
	/// One stem per instrument/audio track (its own channel output).
	Tracks,
	/// One stem per bus and per track routed straight to the master: together they sum
	/// to the mix (before the master chain).
	Buses,
	/// One stem per listed track (any kind).
	Selected(Vec<TrackId>),
}

/// Level normalization of the exported files.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum Normalize {
	#[default]
	Off,
	/// Gain so the sample peak lands on `peak_db` dBFS.
	Peak { peak_db: f32 },
	/// Gain so the integrated loudness reaches `lufs`, reduced if the true peak would
	/// exceed `true_peak_db` dBTP (no limiter: the ceiling wins).
	Loudness { lufs: f32, true_peak_db: f32 },
}

/// A whole export job.
#[derive(Debug, Clone, PartialEq)]
pub struct ExportSpec {
	pub start: Tick,
	/// `None` = song end.
	pub end: Option<Tick>,
	/// Append the longest device tail (reverb/delay decay, at most 10 s) after the end.
	pub tail: bool,
	pub mixdown: bool,
	pub stems: StemSet,
	/// Play each stem through the master inserts and fader.
	pub stems_through_master: bool,
	/// Tap stems before their channel fader (volume/pan) instead of after.
	pub stems_pre_fader: bool,
	pub format: ExportOptions,
	/// Output sample rate (`None` = the render rate).
	pub sample_rate: Option<u32>,
	pub normalize: Normalize,
	/// Normalize each stem on its own; by default stems take the mixdown's gain so they
	/// still add up to the (normalized) mix.
	pub normalize_stems_individually: bool,
	pub metadata: Metadata,
	pub folder: PathBuf,
	/// Base file name: the mix is `<name>.<ext>`, stems `<name> - <track>.<ext>`.
	pub name: String,
}

impl Default for ExportSpec {
	fn default() -> Self {
		Self {
			start: Tick::ZERO,
			end: None,
			tail: true,
			mixdown: true,
			stems: StemSet::None,
			stems_through_master: false,
			stems_pre_fader: false,
			format: ExportOptions::wav(cadenza_audio_io::WavSampleFormat::Int24),
			sample_rate: None,
			normalize: Normalize::Off,
			normalize_stems_individually: false,
			metadata: Metadata::default(),
			folder: PathBuf::from("."),
			name: "Mix".into(),
		}
	}
}

/// Where an export job is.
#[derive(Debug, Clone, PartialEq)]
pub struct ExportProgress {
	/// 0..=1 over the whole job.
	pub fraction: f32,
	/// What is happening ("Rendering", "Master chain: Bass", "Writing Mix.wav").
	pub stage: String,
}

/// One written file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ExportedFile {
	pub path: PathBuf,
	/// The stem's track (`None` = the mixdown).
	pub track_id: Option<TrackId>,
	pub name: String,
	/// Normalization gain applied.
	pub gain_db: f32,
	/// Levels of the file as written (after gain and resampling, before dither).
	pub stats: LevelStats,
	pub sample_rate: u32,
	pub duration_seconds: f64,
	pub bytes: u64,
}

/// What an export wrote.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ExportReport {
	pub files: Vec<ExportedFile>,
	/// Analysis of the render (before normalization), per track.
	pub mix: MixReport,
	/// Normalization notes (e.g. the true-peak ceiling limited the gain).
	pub notes: Vec<String>,
}

impl ExportReport {
	pub fn bytes(&self) -> u64 {
		self.files.iter().map(|f| f.bytes).sum()
	}
}

/// File extension of `format`.
pub fn format_extension(format: &ExportFormat) -> &'static str {
	match format {
		ExportFormat::Wav { .. } => "wav",
		ExportFormat::Flac { .. } => "flac",
		ExportFormat::Mp3 { .. } => "mp3",
	}
}

/// A stem to tap: its track and the node rendering it.
struct Stem {
	track: TrackId,
	name: String,
	node: usize,
	temp: PathBuf,
	writer: Option<hound::WavWriter<std::io::BufWriter<std::fs::File>>>,
	analyzer: Analyzer,
	frames: usize,
}

/// The stems of `set` in display order, as `(track id, track name)`.
pub fn stem_tracks(
	project: &Project,
	set: &StemSet,
) -> Result<Vec<(TrackId, String)>, EngineError> {
	let pick = |t: &Track| (t.id, t.name.clone());
	Ok(match set {
		StemSet::None => Vec::new(),
		StemSet::Tracks => project
			.tracks
			.iter()
			.filter(|t| t.kind != TrackKind::Bus)
			.map(pick)
			.collect(),
		StemSet::Buses => project
			.tracks
			.iter()
			.filter(|t| t.kind == TrackKind::Bus || t.channel.output == Output::Master)
			.map(pick)
			.collect(),
		StemSet::Selected(ids) => {
			if ids.is_empty() {
				return Err(EngineError::InvalidOptions(
					"no tracks selected for stems; list track ids or use tracks/buses".into(),
				));
			}
			for id in ids {
				if project.track(*id).is_none() {
					return Err(EngineError::InvalidOptions(format!(
						"stem track {id} does not exist; use the ids from get_project"
					)));
				}
			}
			project
				.tracks
				.iter()
				.filter(|t| ids.contains(&t.id))
				.map(pick)
				.collect()
		}
	})
}

/// Removes the temp folder when the job ends, however it ends.
struct TempDir(PathBuf);

impl Drop for TempDir {
	fn drop(&mut self) {
		let _ = std::fs::remove_dir_all(&self.0);
	}
}

/// Deletes the outputs of a failed or cancelled job.
struct Outputs {
	paths: Vec<PathBuf>,
	keep: bool,
}

impl Drop for Outputs {
	fn drop(&mut self) {
		if !self.keep {
			for p in &self.paths {
				let _ = std::fs::remove_file(p);
			}
		}
	}
}

fn wav_err(path: &Path) -> impl Fn(hound::Error) -> EngineError + '_ {
	move |e| EngineError::WavWrite {
		path: path.to_path_buf(),
		message: e.to_string(),
	}
}

fn float_writer(
	path: &Path,
	sample_rate: u32,
) -> Result<hound::WavWriter<std::io::BufWriter<std::fs::File>>, EngineError> {
	let spec = hound::WavSpec {
		channels: 2,
		sample_rate,
		bits_per_sample: 32,
		sample_format: hound::SampleFormat::Float,
	};
	hound::WavWriter::create(path, spec).map_err(wav_err(path))
}

fn write_frames(
	writer: &mut hound::WavWriter<std::io::BufWriter<std::fs::File>>,
	l: &[f32],
	r: &[f32],
	path: &Path,
) -> Result<(), EngineError> {
	for (&a, &b) in l.iter().zip(r) {
		writer.write_sample(a).map_err(wav_err(path))?;
		writer.write_sample(b).map_err(wav_err(path))?;
	}
	Ok(())
}

/// The normalization gain for audio measured as `stats`, plus a note when the
/// true-peak ceiling limited it.
pub fn normalize_gain(normalize: Normalize, stats: &LevelStats) -> (f32, Option<String>) {
	let silent = stats.peak_db <= FLOOR_DB + 1.0;
	match normalize {
		Normalize::Off => (0.0, None),
		_ if silent => (0.0, None),
		Normalize::Peak { peak_db } => (peak_db - stats.peak_db, None),
		Normalize::Loudness { lufs, true_peak_db } => {
			if stats.lufs_integrated <= FLOOR_DB + 1.0 {
				return (0.0, None);
			}
			let wanted = lufs - stats.lufs_integrated;
			let ceiling = true_peak_db - stats.true_peak_db;
			if wanted > ceiling {
				(
					ceiling,
					Some(format!(
						"loudness target {lufs:.1} LUFS needs {wanted:+.1} dB but the {true_peak_db:.1} dBTP \
						 ceiling allows {ceiling:+.1} dB; add a limiter to get louder"
					)),
				)
			} else {
				(wanted, None)
			}
		}
	}
}

fn sanitize(name: &str) -> String {
	cadenza_model::files::sanitize_file_name(name)
}

fn validate(spec: &ExportSpec, render_rate: u32) -> Result<(), EngineError> {
	if !spec.mixdown && spec.stems == StemSet::None {
		return Err(EngineError::InvalidOptions(
			"nothing to export: enable the mixdown and/or choose stems".into(),
		));
	}
	if let Some(end) = spec.end
		&& end <= spec.start
	{
		return Err(EngineError::InvalidOptions(format!(
			"the export range is empty (start {} >= end {}); pick an end after the start",
			spec.start.beats(),
			end.beats()
		)));
	}
	let out_rate = spec.sample_rate.unwrap_or(render_rate);
	if !(8_000..=384_000).contains(&out_rate) {
		return Err(EngineError::InvalidOptions(format!(
			"sample rate {out_rate} Hz is out of range; use 44100, 48000, 88200 or 96000"
		)));
	}
	if let ExportFormat::Mp3 { .. } = spec.format.format {
		if !MP3_SUPPORTED {
			return Err(EngineError::InvalidOptions(
				"MP3 export is not available in this build (compile with the `mp3` feature); \
				 export WAV or FLAC instead"
					.into(),
			));
		}
		if !matches!(
			out_rate,
			8000 | 11025 | 12000 | 16000 | 22050 | 24000 | 32000 | 44100 | 48000
		) {
			return Err(EngineError::InvalidOptions(format!(
				"MP3 needs a standard sample rate up to 48000 Hz (got {out_rate}); use 44100 or 48000"
			)));
		}
	}
	match spec.normalize {
		Normalize::Peak { peak_db } if !(-60.0..=0.0).contains(&peak_db) => {
			Err(EngineError::InvalidOptions(format!(
				"peak target {peak_db} dBFS is out of range; use -60..=0 (e.g. -1)"
			)))
		}
		Normalize::Loudness { lufs, true_peak_db }
			if !(-60.0..=0.0).contains(&lufs) || !(-20.0..=0.0).contains(&true_peak_db) =>
		{
			Err(EngineError::InvalidOptions(format!(
				"loudness target {lufs} LUFS / ceiling {true_peak_db} dBTP out of range; use \
				 -60..=0 LUFS (e.g. -14) and -20..=0 dBTP (e.g. -1)"
			)))
		}
		_ => Ok(()),
	}
}

/// Plays a stem file (starting at `start`) through `project`'s master channel.
fn through_master(
	project: &Project,
	opts: &RenderOptions,
	start: Tick,
	file: &Path,
	frames: usize,
) -> Result<AudioBuffer, EngineError> {
	let sr = f64::from(opts.sample_rate);
	let mut p = Project::new("Stem");
	p.tempo = project.tempo.clone();
	p.time_signature = project.time_signature;
	p.master = project.master.clone();
	p.automation = project
		.automation
		.iter()
		.filter(|l| l.address.track.is_none())
		.cloned()
		.collect();
	let start_s = project.tempo.tick_to_seconds(start);
	let end = project
		.tempo
		.seconds_to_tick(start_s + frames as f64 / sr)
		.max(Tick(start.0 + 1));
	let length = Tick(end.0 - start.0 + 1);
	let mut track = Track::new("Stem", TrackKind::Audio);
	track.clips.push(Clip::audio("stem", start, length, file));
	p.tracks.push(track);
	let render = RenderOptions {
		start,
		end: Some(Tick(start.0 + length.0)),
		..opts.clone()
	};
	let out = render_offline(&p, &render);
	cadenza_audio_io::AudioCache::global().invalidate(file);
	let mut out = out?;
	out.left.resize(frames, 0.0);
	out.right.resize(frames, 0.0);
	AudioBuffer::from_planar(opts.sample_rate, vec![out.left, out.right]).map_err(|e| {
		EngineError::Export {
			path: file.to_path_buf(),
			message: e.to_string(),
		}
	})
}

fn measure(buffer: &AudioBuffer) -> LevelStats {
	let mut a = Analyzer::new(buffer.sample_rate());
	let (l, r) = if buffer.num_channels() >= 2 {
		(buffer.channel(0), buffer.channel(1))
	} else {
		(buffer.channel(0), buffer.channel(0))
	};
	for (cl, cr) in l.chunks(4096).zip(r.chunks(4096)) {
		a.push(cl, cr);
	}
	a.finish()
}

/// Runs an export job. `opts` gives the render rate, plugins and hosting (its start/end
/// are replaced by the spec's range). `progress` is called often with the job's state;
/// returning `false` cancels it ([`EngineError::Cancelled`], nothing is left on disk).
pub fn export_project(
	project: &Project,
	opts: &RenderOptions,
	spec: &ExportSpec,
	progress: &mut dyn FnMut(&ExportProgress) -> bool,
) -> Result<ExportReport, EngineError> {
	validate(spec, opts.sample_rate)?;
	let stems_wanted = stem_tracks(project, &spec.stems)?;
	let render = RenderOptions {
		start: spec.start,
		end: spec.end,
		..opts.clone()
	};
	let (mut graph, len) = prepare_range(project, &render, spec.tail)?;
	std::fs::create_dir_all(&spec.folder).map_err(|e| EngineError::Export {
		path: spec.folder.clone(),
		message: e.to_string(),
	})?;
	let stamp = std::time::SystemTime::now()
		.duration_since(std::time::UNIX_EPOCH)
		.map_or(0, |d| d.as_nanos());
	let temp = TempDir(
		spec.folder
			.join(format!(".cadenza-export-{}-{stamp}", std::process::id())),
	);
	std::fs::create_dir_all(&temp.0).map_err(|e| EngineError::Export {
		path: temp.0.clone(),
		message: e.to_string(),
	})?;

	let mut stems = Vec::with_capacity(stems_wanted.len());
	for (i, (track, name)) in stems_wanted.into_iter().enumerate() {
		let Some(node) = graph.nodes.iter().position(|n| n.track_id == track) else {
			continue;
		};
		if spec.stems_pre_fader {
			graph.nodes[node].tap_pre = true;
		}
		let path = temp.0.join(format!("stem-{i}.wav"));
		stems.push(Stem {
			track,
			name,
			node,
			writer: Some(float_writer(&path, opts.sample_rate)?),
			temp: path,
			analyzer: Analyzer::new(opts.sample_rate),
			frames: 0,
		});
	}
	let mix_temp = temp.0.join("mix.wav");
	let mut mix_writer = if spec.mixdown {
		Some(float_writer(&mix_temp, opts.sample_rate)?)
	} else {
		None
	};

	// Progress weights: the main pass, one pass per stem through the master, and the
	// final gain/resample/encode of every file (lighter).
	let through = if spec.stems_through_master {
		stems.len()
	} else {
		0
	};
	let files = stems.len() + usize::from(spec.mixdown);
	let total_units = 1.0 + through as f32 + files as f32 * 0.25;
	let mut cancelled = false;
	let mut report_progress = |done: f32, stage: String| -> Result<(), EngineError> {
		let p = ExportProgress {
			fraction: (done / total_units).clamp(0.0, 1.0),
			stage,
		};
		if progress(&p) {
			Ok(())
		} else {
			Err(EngineError::Cancelled)
		}
	};
	report_progress(0.0, "Rendering".into())?;

	let start_sample = graph.position_sample();
	let mut last_report = 0usize;
	let mix_path = mix_temp.clone();
	let mut tap = |graph: &crate::graph::Graph,
	               pos: i64,
	               rendered: usize,
	               total: usize|
	 -> Result<(), EngineError> {
		for stem in &mut stems {
			let node = &graph.nodes[stem.node];
			let first = start_sample + node.out_latency as i64 - pos;
			let last = start_sample + len as i64 + node.out_latency as i64 - pos;
			let from = first.clamp(0, QUANTUM as i64) as usize;
			let to = last.clamp(0, QUANTUM as i64) as usize;
			if from >= to {
				continue;
			}
			let silent = [0.0f32; QUANTUM];
			let (l, r) = if spec.stems_pre_fader {
				if node.audible {
					(&node.pre.l[from..to], &node.pre.r[from..to])
				} else {
					(&silent[from..to], &silent[from..to])
				}
			} else {
				(&node.buf.l[from..to], &node.buf.r[from..to])
			};
			stem.analyzer.push(l, r);
			stem.frames += to - from;
			if let Some(w) = &mut stem.writer {
				write_frames(w, l, r, &stem.temp)?;
			}
		}
		if rendered - last_report >= 16_384 || rendered == total {
			last_report = rendered;
			if cancelled {
				return Err(EngineError::Cancelled);
			}
			if let Err(e) =
				report_progress(rendered as f32 / total.max(1) as f32, "Rendering".into())
			{
				cancelled = true;
				return Err(e);
			}
		}
		Ok(())
	};
	let mix = render_analyzed_with(
		graph,
		len,
		|l, r| match &mut mix_writer {
			Some(w) => write_frames(w, l, r, &mix_path),
			None => Ok(()),
		},
		&mut tap,
	)?;
	if let Some(w) = mix_writer.take() {
		w.finalize().map_err(wav_err(&mix_temp))?;
	}
	for stem in &mut stems {
		if let Some(w) = stem.writer.take() {
			w.finalize().map_err(wav_err(&stem.temp))?;
		}
	}

	let ext = format_extension(&spec.format.format);
	let base = sanitize(&spec.name);
	let mut notes = Vec::new();
	let (mix_gain, note) = normalize_gain(spec.normalize, &mix.master);
	notes.extend(note);
	let out_rate = spec.sample_rate.unwrap_or(opts.sample_rate);
	let mut outputs = Outputs {
		paths: Vec::new(),
		keep: false,
	};
	let mut written = Vec::new();
	let mut done = 1.0f32;
	let mut taken: Vec<String> = Vec::new();

	let finish = |buffer: AudioBuffer,
	              gain_db: f32,
	              path: PathBuf,
	              track_id: Option<TrackId>,
	              name: String,
	              outputs: &mut Outputs|
	 -> Result<ExportedFile, EngineError> {
		let export_err = |message: String| EngineError::Export {
			path: path.clone(),
			message,
		};
		let mut buffer = buffer;
		if gain_db != 0.0 {
			buffer.apply_gain(cadenza_core::db_to_gain(gain_db));
		}
		if buffer.sample_rate() != out_rate && buffer.frames() > 0 {
			buffer = cadenza_audio_io::resample(&buffer, out_rate)
				.map_err(|e| export_err(e.to_string()))?;
		}
		let stats = measure(&buffer);
		let mut meta = spec.metadata.clone();
		if track_id.is_some() && !meta.title.trim().is_empty() {
			meta.title = format!("{} - {name}", meta.title.trim());
		}
		outputs.paths.push(path.clone());
		cadenza_audio_io::export_with_metadata(&buffer, &path, &spec.format, &meta)
			.map_err(|e| export_err(e.to_string()))?;
		Ok(ExportedFile {
			bytes: std::fs::metadata(&path).map_or(0, |m| m.len()),
			sample_rate: buffer.sample_rate(),
			duration_seconds: buffer.duration_seconds(),
			path,
			track_id,
			name,
			gain_db,
			stats,
		})
	};
	let read = |path: &Path| {
		cadenza_audio_io::decode_file(path).map_err(|e| EngineError::Export {
			path: path.to_path_buf(),
			message: e.to_string(),
		})
	};

	if spec.mixdown {
		let path = spec.folder.join(format!("{base}.{ext}"));
		taken.push(path.display().to_string());
		report_progress(done, format!("Writing {}", file_label(&path)))?;
		let file = finish(
			read(&mix_temp)?,
			mix_gain,
			path,
			None,
			base.clone(),
			&mut outputs,
		)?;
		written.push(file);
		done += 0.25;
	}
	for stem in &stems {
		let mut file_name = format!("{base} - {}", sanitize(&stem.name));
		let mut n = 2;
		while taken.contains(
			&spec
				.folder
				.join(format!("{file_name}.{ext}"))
				.display()
				.to_string(),
		) {
			file_name = format!("{base} - {} ({n})", sanitize(&stem.name));
			n += 1;
		}
		let path = spec.folder.join(format!("{file_name}.{ext}"));
		taken.push(path.display().to_string());
		let (buffer, stats) = if spec.stems_through_master {
			report_progress(done, format!("Master chain: {}", stem.name))?;
			let b = through_master(project, &render, spec.start, &stem.temp, stem.frames)?;
			done += 1.0;
			let stats = measure(&b);
			(b, stats)
		} else {
			(read(&stem.temp)?, stem.analyzer.finish())
		};
		let gain = if spec.normalize_stems_individually {
			let (g, note) = normalize_gain(spec.normalize, &stats);
			notes.extend(note.map(|n| format!("{}: {n}", stem.name)));
			g
		} else {
			mix_gain
		};
		report_progress(done, format!("Writing {}", file_label(&path)))?;
		let file = finish(
			buffer,
			gain,
			path,
			Some(stem.track),
			stem.name.clone(),
			&mut outputs,
		)?;
		written.push(file);
		done += 0.25;
	}
	report_progress(total_units, "Done".into())?;
	outputs.keep = true;
	Ok(ExportReport {
		files: written,
		mix,
		notes,
	})
}

fn file_label(path: &Path) -> String {
	path.file_name()
		.map(|n| n.to_string_lossy().into_owned())
		.unwrap_or_default()
}
