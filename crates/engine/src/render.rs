//! Offline rendering and mix analysis.
//!
//! Renders are latency-compensated: the graph runs `latency_samples` ahead and the leading
//! latency is trimmed, so the first output sample is the timeline sample at `start`.

use std::path::Path;
use std::sync::Arc;

use cadenza_analysis::detail::{Detail, DetailAnalyzer};
use cadenza_audio_io::{AudioBuffer, ExportFormat, ExportOptions, FlacBitDepth, WavSampleFormat};
use cadenza_core::{Tick, TrackId};
use cadenza_model::{Project, TrackKind};
use cadenza_plugin_host::{PluginCatalog, ProcessMode};

use crate::QUANTUM;
use crate::analysis::{Analyzer, MixReport, TrackLevel};
use crate::compile::compile_for;
use crate::error::EngineError;
use crate::graph::{Graph, Source};
use crate::hosting::PluginHosting;

/// What and how to render.
#[derive(Debug, Clone)]
pub struct RenderOptions {
	pub sample_rate: u32,
	/// Host block size used to pull audio from the graph. The result does not depend on it.
	pub block_size: usize,
	pub start: Tick,
	/// `None` = song end (`Project::end_tick`) plus the longest device tail (max 10 s).
	pub end: Option<Tick>,
	/// Catalog used to load third-party plugin devices. `None` renders built-in devices
	/// only; plugin devices are then bypassed with a warning.
	pub plugins: Option<Arc<PluginCatalog>>,
	/// Where plugin devices run. Defaults to [`PluginHosting::default`]: each plugin in
	/// its own sandbox process (a crash or hang only bypasses that device and adds a
	/// warning to the [`MixReport`]), unless `CADENZA_PLUGIN_HOSTING=in-process`.
	pub hosting: PluginHosting,
}

impl PartialEq for RenderOptions {
	fn eq(&self, other: &Self) -> bool {
		self.sample_rate == other.sample_rate
			&& self.block_size == other.block_size
			&& self.start == other.start
			&& self.end == other.end
			&& match (&self.plugins, &other.plugins) {
				(None, None) => true,
				(Some(a), Some(b)) => Arc::ptr_eq(a, b),
				_ => false,
			} && self.hosting == other.hosting
	}
}

impl Default for RenderOptions {
	fn default() -> Self {
		Self {
			sample_rate: 48_000,
			block_size: 512,
			start: Tick::ZERO,
			end: None,
			plugins: None,
			hosting: PluginHosting::default(),
		}
	}
}

/// Rendered stereo audio.
#[derive(Debug, Clone, PartialEq)]
pub struct StereoBuffer {
	pub sample_rate: u32,
	pub left: Vec<f32>,
	pub right: Vec<f32>,
}

impl StereoBuffer {
	pub fn len(&self) -> usize {
		self.left.len()
	}

	pub fn is_empty(&self) -> bool {
		self.left.is_empty()
	}

	pub fn duration_seconds(&self) -> f64 {
		self.len() as f64 / self.sample_rate as f64
	}
}

/// Compiles and positions a graph; returns it with the number of samples to render.
fn prepare(project: &Project, opts: &RenderOptions) -> Result<(Graph, usize), EngineError> {
	prepare_range(project, opts, opts.end.is_none())
}

/// [`prepare`] from `opts.start` to `opts.end` (`None` = song end), plus the longest device
/// tail when `tail`.
pub(crate) fn prepare_range(
	project: &Project,
	opts: &RenderOptions,
	tail: bool,
) -> Result<(Graph, usize), EngineError> {
	if opts.start.0 < 0 {
		return Err(EngineError::InvalidOptions(format!(
			"start tick {} is negative; use 0 or later",
			opts.start.0
		)));
	}
	let mut graph = compile_for(
		project,
		opts.sample_rate,
		opts.block_size,
		opts.plugins.as_deref(),
		&opts.hosting,
		ProcessMode::Offline,
	)?;
	let sr = opts.sample_rate as f64;
	let start = project.tempo.tick_to_sample(opts.start, sr) as i64;
	let end = match opts.end {
		Some(end) if end <= opts.start => {
			return Err(EngineError::InvalidOptions(format!(
				"end tick {} must be after start tick {}",
				end.0, opts.start.0
			)));
		}
		Some(end) => project.tempo.tick_to_sample(end, sr) as i64,
		None => (project.tempo.tick_to_sample(project.end_tick(), sr) as i64).max(start),
	};
	let end = if tail {
		end + (graph.tail_seconds() as f64 * sr).ceil() as i64
	} else {
		end
	};
	graph.seek_sample(start);
	graph.play();
	Ok((graph, (end - start).max(0) as usize))
}

/// Renders `project` to memory, time-aligned with the timeline from `opts.start`.
pub fn render_offline(
	project: &Project,
	opts: &RenderOptions,
) -> Result<StereoBuffer, EngineError> {
	let (mut graph, len) = prepare(project, opts)?;
	let mut left = vec![0.0; len];
	let mut right = vec![0.0; len];
	for (l, r) in left
		.chunks_mut(opts.block_size)
		.zip(right.chunks_mut(opts.block_size))
	{
		graph.process(l, r);
	}
	Ok(StereoBuffer {
		sample_rate: opts.sample_rate,
		left,
		right,
	})
}

/// Called after every rendered quantum with the graph, the engine position the quantum
/// started at, and the engine samples rendered so far / in total; an error aborts the render.
pub(crate) type QuantumHook<'a> =
	&'a mut dyn FnMut(&Graph, i64, usize, usize) -> Result<(), EngineError>;

/// Renders quantum by quantum, measuring the master and every track's post-fader output,
/// and hands each master chunk to `sink`.
fn render_analyzed(
	graph: Graph,
	len: usize,
	sink: impl FnMut(&[f32], &[f32]) -> Result<(), EngineError>,
) -> Result<MixReport, EngineError> {
	render_analyzed_with(graph, len, sink, &mut |_, _, _, _| Ok(()))
}

/// [`render_analyzed`] calling `hook` after each quantum (stems taps, progress, cancel).
pub(crate) fn render_analyzed_with(
	mut graph: Graph,
	len: usize,
	mut sink: impl FnMut(&[f32], &[f32]) -> Result<(), EngineError>,
	hook: QuantumHook<'_>,
) -> Result<MixReport, EngineError> {
	let sr = graph.sample_rate();
	let start = graph.position_sample();
	let mut master = Analyzer::new(sr);
	let mut tracks: Vec<Analyzer> = graph.nodes.iter().map(|_| Analyzer::new(sr)).collect();
	// The master output lags the engine by `latency`: drop that many leading samples.
	let latency = graph.latency_samples() as usize;
	let total = len + latency;
	let mut rendered = 0;
	while rendered < total {
		let pos = graph.engine_pos;
		graph.render_quantum();
		graph.note_plugin_crashes();
		let from = latency.saturating_sub(rendered).min(QUANTUM);
		let to = (total - rendered).min(QUANTUM);
		if from < to {
			let out = &graph.master.buf;
			master.push(&out.l[from..to], &out.r[from..to]);
			sink(&out.l[from..to], &out.r[from..to])?;
		}
		// A track's tap at engine sample p carries timeline sample p - out_latency; only
		// measure what falls inside the rendered range.
		for (node, analyzer) in graph.nodes.iter().zip(&mut tracks) {
			let first = start + node.out_latency as i64 - pos;
			let last = start + len as i64 + node.out_latency as i64 - pos;
			let from = first.clamp(0, QUANTUM as i64) as usize;
			let to = last.clamp(0, QUANTUM as i64) as usize;
			if from < to {
				analyzer.push(&node.buf.l[from..to], &node.buf.r[from..to]);
			}
		}
		rendered += QUANTUM;
		hook(&graph, pos, rendered.min(total), total)?;
	}
	let mut track_levels: Vec<(usize, TrackLevel)> = graph
		.nodes
		.iter()
		.zip(&tracks)
		.map(|(node, a)| {
			(
				node.display_index,
				TrackLevel {
					track_id: node.track_id,
					name: node.name.clone(),
					kind: node_kind(&node.source),
					stats: a.finish(),
				},
			)
		})
		.collect();
	track_levels.sort_by_key(|(i, _)| *i);
	Ok(MixReport {
		sample_rate: sr,
		duration_seconds: len as f64 / sr as f64,
		latency_samples: graph.latency_samples(),
		warnings: graph
			.warnings()
			.iter()
			.cloned()
			.chain(graph.crash_warnings())
			.collect(),
		master: master.finish(),
		tracks: track_levels.into_iter().map(|(_, t)| t).collect(),
	})
}

fn node_kind(source: &Source) -> TrackKind {
	match source {
		Source::Instrument { .. } => TrackKind::Instrument,
		Source::Audio { .. } => TrackKind::Audio,
		Source::Bus { .. } => TrackKind::Bus,
	}
}

/// One channel of a [`DetailedMix`]: its post-fader output.
#[derive(Debug, Clone)]
pub struct TrackDetail {
	pub track_id: TrackId,
	pub name: String,
	pub kind: TrackKind,
	pub detail: Detail,
}

/// [`analyze_mix`] plus spectrum, stereo image and octave timeline of the master and of every
/// channel (tracks and buses, display order): the input of the mix assistant's issues,
/// masking and reference comparison.
#[derive(Debug, Clone)]
pub struct DetailedMix {
	pub report: MixReport,
	pub master: Detail,
	pub tracks: Vec<TrackDetail>,
}

/// Renders and analyzes the mix in detail (offline, like an export). `progress` gets the
/// rendered fraction now and then; returning `false` cancels ([`EngineError::Cancelled`]).
pub fn analyze_mix_detailed(
	project: &Project,
	opts: &RenderOptions,
	progress: &mut dyn FnMut(f32) -> bool,
) -> Result<DetailedMix, EngineError> {
	let (graph, len) = prepare(project, opts)?;
	let sr = graph.sample_rate();
	let start = graph.position_sample();
	let meta: Vec<(usize, TrackId, String, TrackKind)> = graph
		.nodes
		.iter()
		.map(|n| {
			(
				n.display_index,
				n.track_id,
				n.name.clone(),
				node_kind(&n.source),
			)
		})
		.collect();
	let mut master = DetailAnalyzer::new(sr);
	let mut channels: Vec<DetailAnalyzer> = meta.iter().map(|_| DetailAnalyzer::new(sr)).collect();
	let mut last_report = 0usize;
	let mut hook = |graph: &Graph, pos: i64, rendered: usize, total: usize| {
		// Same alignment as the per-track level analyzers.
		for (node, analyzer) in graph.nodes.iter().zip(&mut channels) {
			let first = start + node.out_latency as i64 - pos;
			let last = start + len as i64 + node.out_latency as i64 - pos;
			let from = first.clamp(0, QUANTUM as i64) as usize;
			let to = last.clamp(0, QUANTUM as i64) as usize;
			if from < to {
				analyzer.push(&node.buf.l[from..to], &node.buf.r[from..to]);
			}
		}
		if rendered - last_report >= 16_384 || rendered == total {
			last_report = rendered;
			if !progress(rendered as f32 / total.max(1) as f32) {
				return Err(EngineError::Cancelled);
			}
		}
		Ok(())
	};
	let report = render_analyzed_with(
		graph,
		len,
		|l, r| {
			master.push(l, r);
			Ok(())
		},
		&mut hook,
	)?;
	let mut tracks: Vec<(usize, TrackDetail)> = meta
		.into_iter()
		.zip(channels)
		.map(|((index, track_id, name, kind), a)| {
			(
				index,
				TrackDetail {
					track_id,
					name,
					kind,
					detail: a.finish(),
				},
			)
		})
		.collect();
	tracks.sort_by_key(|(i, _)| *i);
	Ok(DetailedMix {
		report,
		master: master.finish(),
		tracks: tracks.into_iter().map(|(_, t)| t).collect(),
	})
}

/// Renders and measures the mix without writing audio anywhere.
pub fn analyze_mix(project: &Project, opts: &RenderOptions) -> Result<MixReport, EngineError> {
	let (graph, len) = prepare(project, opts)?;
	render_analyzed(graph, len, |_, _| Ok(()))
}

/// Renders to a 32-bit float stereo WAV at `path` and returns the analysis of that audio.
pub fn render_to_wav(
	project: &Project,
	opts: &RenderOptions,
	path: &Path,
) -> Result<MixReport, EngineError> {
	let wav_err = |e: hound::Error| EngineError::WavWrite {
		path: path.to_path_buf(),
		message: e.to_string(),
	};
	let spec = hound::WavSpec {
		channels: 2,
		sample_rate: opts.sample_rate,
		bits_per_sample: 32,
		sample_format: hound::SampleFormat::Float,
	};
	let (graph, len) = prepare(project, opts)?;
	let mut writer = hound::WavWriter::create(path, spec).map_err(wav_err)?;
	let report = render_analyzed(graph, len, |l, r| {
		for (&a, &b) in l.iter().zip(r) {
			writer.write_sample(a).map_err(wav_err)?;
			writer.write_sample(b).map_err(wav_err)?;
		}
		Ok(())
	})?;
	writer.finalize().map_err(wav_err)?;
	Ok(report)
}

/// File formats [`render_to_file`] can write, chosen from the path's extension.
pub const EXPORT_EXTENSIONS: &[&str] = &["wav", "flac", "mp3"];

/// Whether this build can write MP3 (the `mp3` cargo feature).
pub const MP3_SUPPORTED: bool = cfg!(feature = "mp3");

/// Picks the export format from `path`'s extension: `.wav` (`bit_depth` 16, 24 or 32
/// float; default 32 float), `.flac` (16 or 24; default 24) or `.mp3` (`mp3_bitrate_kbps`,
/// default 320; needs the `mp3` feature). Integer formats get TPDF dither.
pub fn export_options_for_path(
	path: &Path,
	bit_depth: Option<u32>,
	mp3_bitrate_kbps: Option<u16>,
) -> Result<ExportOptions, EngineError> {
	let ext = path
		.extension()
		.and_then(|e| e.to_str())
		.map(str::to_ascii_lowercase)
		.unwrap_or_default();
	let bad_depth = |fmt: &str, allowed: &str| {
		EngineError::InvalidOptions(format!(
			"bit depth {} is not available for {fmt}; use {allowed}",
			bit_depth.unwrap_or_default()
		))
	};
	match ext.as_str() {
		"wav" => {
			let format = match bit_depth {
				Some(16) => WavSampleFormat::Int16,
				Some(24) => WavSampleFormat::Int24,
				None | Some(32) => WavSampleFormat::Float32,
				Some(_) => return Err(bad_depth("WAV", "16, 24 or 32 (float)")),
			};
			Ok(ExportOptions::wav(format))
		}
		"flac" => {
			let depth = match bit_depth {
				Some(16) => FlacBitDepth::Bits16,
				None | Some(24) => FlacBitDepth::Bits24,
				Some(_) => return Err(bad_depth("FLAC", "16 or 24")),
			};
			Ok(ExportOptions::flac(depth))
		}
		"mp3" => {
			if !MP3_SUPPORTED {
				return Err(EngineError::InvalidOptions(
					"MP3 export is not available in this build (compile with the `mp3` feature); \
					 render to .wav or .flac instead"
						.into(),
				));
			}
			if bit_depth.is_some() {
				return Err(EngineError::InvalidOptions(
					"bit depth does not apply to MP3; omit it and set the bitrate (kbps) instead"
						.into(),
				));
			}
			let kbps = mp3_bitrate_kbps.unwrap_or(320);
			if !(32..=320).contains(&kbps) {
				return Err(EngineError::InvalidOptions(format!(
					"MP3 bitrate {kbps} kbps is out of range; use 32..=320 (e.g. 192 or 320)"
				)));
			}
			Ok(ExportOptions::mp3(kbps))
		}
		_ => Err(EngineError::InvalidOptions(format!(
			"cannot tell the audio format of '{}'; end the file name in .wav, .flac or .mp3",
			path.display()
		))),
	}
}

/// Renders to `path` in the given format and returns the analysis of the rendered
/// (pre-quantization) audio. 32-bit float WAV streams to disk like [`render_to_wav`];
/// other formats are rendered to memory, then encoded (with TPDF dither for 16/24-bit).
pub fn render_to_file(
	project: &Project,
	opts: &RenderOptions,
	path: &Path,
	export: &ExportOptions,
) -> Result<MixReport, EngineError> {
	if export.format
		== (ExportFormat::Wav {
			sample_format: WavSampleFormat::Float32,
		}) {
		return render_to_wav(project, opts, path);
	}
	if matches!(export.format, ExportFormat::Mp3 { .. }) {
		if !MP3_SUPPORTED {
			return Err(EngineError::InvalidOptions(
				"MP3 export is not available in this build (compile with the `mp3` feature); \
				 render to .wav or .flac instead"
					.into(),
			));
		}
		if !matches!(
			opts.sample_rate,
			8000 | 11025 | 12000 | 16000 | 22050 | 24000 | 32000 | 44100 | 48000
		) {
			return Err(EngineError::InvalidOptions(format!(
				"MP3 needs a standard sample rate up to 48000 Hz (got {}); render at 44100 or 48000",
				opts.sample_rate
			)));
		}
	}
	let (graph, len) = prepare(project, opts)?;
	let mut left = Vec::with_capacity(len);
	let mut right = Vec::with_capacity(len);
	let report = render_analyzed(graph, len, |l, r| {
		left.extend_from_slice(l);
		right.extend_from_slice(r);
		Ok(())
	})?;
	let export_err = |message: String| EngineError::Export {
		path: path.to_path_buf(),
		message,
	};
	let buffer = AudioBuffer::from_planar(opts.sample_rate, vec![left, right])
		.map_err(|e| export_err(e.to_string()))?;
	cadenza_audio_io::export(&buffer, path, export).map_err(|e| export_err(e.to_string()))?;
	Ok(report)
}
