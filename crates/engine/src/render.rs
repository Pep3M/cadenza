//! Offline rendering and mix analysis.
//!
//! Renders are latency-compensated: the graph runs `latency_samples` ahead and the leading
//! latency is trimmed, so the first output sample is the timeline sample at `start`.

use std::path::Path;

use cadenza_core::Tick;
use cadenza_model::Project;

use crate::analysis::{Analyzer, MixReport, TrackLevel};
use crate::error::EngineError;
use crate::graph::Graph;
use crate::{QUANTUM, compile};

/// What and how to render.
#[derive(Debug, Clone, PartialEq)]
pub struct RenderOptions {
	pub sample_rate: u32,
	/// Host block size used to pull audio from the graph. The result does not depend on it.
	pub block_size: usize,
	pub start: Tick,
	/// `None` = song end (`Project::end_tick`) plus the longest device tail (max 10 s).
	pub end: Option<Tick>,
}

impl Default for RenderOptions {
	fn default() -> Self {
		Self {
			sample_rate: 48_000,
			block_size: 512,
			start: Tick::ZERO,
			end: None,
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
	if opts.start.0 < 0 {
		return Err(EngineError::InvalidOptions(format!(
			"start tick {} is negative; use 0 or later",
			opts.start.0
		)));
	}
	let mut graph = compile(project, opts.sample_rate, opts.block_size)?;
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
		None => {
			let song_end = project.tempo.tick_to_sample(project.end_tick(), sr) as i64;
			song_end.max(start) + (graph.tail_seconds() as f64 * sr).ceil() as i64
		}
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

/// Renders quantum by quantum, measuring the master and every track's post-fader output,
/// and hands each master chunk to `sink`.
fn render_analyzed(
	mut graph: Graph,
	len: usize,
	mut sink: impl FnMut(&[f32], &[f32]) -> Result<(), EngineError>,
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
		warnings: graph.warnings().to_vec(),
		master: master.finish(),
		tracks: track_levels.into_iter().map(|(_, t)| t).collect(),
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
