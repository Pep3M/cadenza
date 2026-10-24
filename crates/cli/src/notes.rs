//! `cadenza notes eval | corpus`: the note-detection evaluation harness (NE-0). `eval`
//! scores a detector on the synthetic corpus and on hand-labeled recordings; `corpus`
//! writes the synthetic corpus as WAV + label files to listen to or to label tools with.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use cadenza_audio_io::{AudioBuffer, ExportOptions, WavSampleFormat, decode_file, export};
use cadenza_notes::analysis::{self, AnalysisOptions, Cache};
use cadenza_notes::eval::{self, Case, FrameScore, Report, Score, labels, synth};
use cadenza_notes::segment::{self, NoteFlag, SegmentOptions};
use clap::{Args, Subcommand};

const AUDIO_EXTENSIONS: &[&str] = &["wav", "flac", "mp3", "ogg", "m4a", "aif", "aiff"];

#[derive(Subcommand)]
pub enum NotesCommand {
	/// Score a note detector: COnPOff/COnP/COn F-measures, pitch-center error in cents,
	/// mixed regions (one region over two real notes), splits, merges, octave errors.
	Eval(EvalArgs),
	/// Score a pitch tracker frame by frame: raw pitch/chroma accuracy, voicing recall and
	/// false alarms, octave and gross errors, speed.
	EvalPitch(EvalPitchArgs),
	/// Analyze an audio file (pitch track + level/onset/sibilance features per 5 ms frame)
	/// and print a summary, or every frame with --json. Results are cached per content in
	/// the user cache folder.
	Analyze {
		file: PathBuf,
		/// Print the whole analysis as JSON.
		#[arg(long)]
		json: bool,
		/// Analyze again even when a cached result exists.
		#[arg(long)]
		no_cache: bool,
	},
	/// Detect the notes of an audio file: onset, offset, stable core, center, drift,
	/// vibrato, confidence and flags (doubtful regions), plus sibilants.
	Segment {
		file: PathBuf,
		/// Above 1 finds more notes, below 1 fewer.
		#[arg(long, default_value_t = 1.0)]
		sensitivity: f64,
		/// Forced note boundaries, seconds (repeatable).
		#[arg(long)]
		split: Vec<f64>,
		#[arg(long)]
		json: bool,
	},
	/// Write the synthetic corpus as `<case>.wav` + `<case>.json` labels into a folder.
	Corpus {
		dir: PathBuf,
		#[arg(long, default_value_t = 22_050)]
		sample_rate: u32,
	},
}

#[derive(Args)]
pub struct EvalArgs {
	/// Detector id (baseline).
	#[arg(long, default_value = "baseline")]
	detector: String,
	#[command(flatten)]
	cases: CaseArgs,
}

#[derive(Args)]
pub struct EvalPitchArgs {
	/// Tracker id (pyin, yin).
	#[arg(long, default_value = "pyin")]
	tracker: String,
	#[command(flatten)]
	cases: CaseArgs,
}

#[derive(Args)]
pub struct CaseArgs {
	/// Folder of labeled recordings (`<name>.json` next to `<name>.wav|flac|...`), searched
	/// recursively; default: $CADENZA_NOTES_CORPUS when set.
	#[arg(long)]
	corpus: Option<PathBuf>,
	/// Skip the synthetic corpus.
	#[arg(long)]
	no_synthetic: bool,
	/// Only cases whose name contains this text.
	#[arg(long)]
	case: Option<String>,
	/// Sample rate of the synthetic corpus.
	#[arg(long, default_value_t = 22_050)]
	sample_rate: u32,
	#[arg(long)]
	json: bool,
}

pub fn run(command: NotesCommand) -> Result<()> {
	match command {
		NotesCommand::Eval(args) => run_eval(args),
		NotesCommand::EvalPitch(args) => run_eval_pitch(args),
		NotesCommand::Analyze {
			file,
			json,
			no_cache,
		} => analyze(&file, json, no_cache),
		NotesCommand::Segment {
			file,
			sensitivity,
			split,
			json,
		} => segment_file(&file, sensitivity, &split, json),
		NotesCommand::Corpus { dir, sample_rate } => write_corpus(&dir, sample_rate),
	}
}

fn run_eval(args: EvalArgs) -> Result<()> {
	let Some(detector) = cadenza_notes::detector(&args.detector) else {
		bail!(
			"unknown detector '{}': use one of {}",
			args.detector,
			cadenza_notes::DETECTORS.join(", ")
		);
	};
	let cases = load_cases(&args.cases)?;
	let report = eval::evaluate(detector.as_ref(), &cases);
	if args.cases.json {
		println!("{}", serde_json::to_string_pretty(&report)?);
	} else {
		print_table(&report);
	}
	Ok(())
}

fn run_eval_pitch(args: EvalPitchArgs) -> Result<()> {
	let Some(tracker) = cadenza_notes::tracker(&args.tracker) else {
		bail!(
			"unknown tracker '{}': use one of {}",
			args.tracker,
			cadenza_notes::TRACKERS.join(", ")
		);
	};
	let cases = load_cases(&args.cases)?;
	let report = eval::evaluate_pitch(tracker.as_ref(), &cases);
	if args.cases.json {
		println!("{}", serde_json::to_string_pretty(&report)?);
		return Ok(());
	}
	println!(
		"tracker: {} ({:.0}x realtime)",
		report.tracker, report.speed
	);
	println!(
		"{:<20} {:>7} {:>7} {:>7} {:>7} {:>7} {:>6} {:>6} {:>7} {:>7}",
		"case", "frames", "pitch", "chroma", "v.rec", "v.fa", "oct", "gross", "med", "p95"
	);
	let row = |name: &str, s: &FrameScore| {
		println!(
			"{:<20} {:>7} {:>7} {:>7} {:>7} {:>7} {:>6} {:>6} {:>7} {:>7}",
			name,
			s.voiced_frames,
			pct(s.raw_pitch),
			pct(s.raw_chroma),
			pct(s.voicing_recall),
			pct(s.voicing_false_alarm),
			pct(s.octave_rate),
			pct(s.gross_rate),
			format!("{:.1}c", s.median_cents),
			format!("{:.0}c", s.p95_cents),
		);
	};
	for case in &report.cases {
		row(&case.name, &case.score);
	}
	row("TOTAL", &report.total);
	Ok(())
}

fn load_cases(args: &CaseArgs) -> Result<Vec<Case>> {
	let mut cases = Vec::new();
	if !args.no_synthetic {
		cases.extend(synth::corpus(args.sample_rate));
	}
	let corpus = args
		.corpus
		.clone()
		.or_else(|| std::env::var_os("CADENZA_NOTES_CORPUS").map(PathBuf::from));
	if let Some(dir) = corpus {
		cases.extend(load_labeled(&dir)?);
	}
	if let Some(filter) = &args.case {
		cases.retain(|c| c.name.contains(filter.as_str()));
	}
	if cases.is_empty() {
		bail!("no cases to evaluate (check --case, --no-synthetic and --corpus)");
	}
	Ok(cases)
}

fn print_table(report: &Report) {
	println!("detector: {}", report.detector);
	println!(
		"{:<20} {:>5} {:>5} {:>7} {:>6} {:>6} {:>8} {:>7} {:>6} {:>6} {:>6} {:>6} {:>5}",
		"case",
		"notes",
		"found",
		"COnPOff",
		"COnP",
		"COn",
		"ctr med",
		"ctr p95",
		">10c",
		"mixed",
		"split",
		"merge",
		"oct"
	);
	let row = |name: &str, s: &Score| {
		println!(
			"{:<20} {:>5} {:>5} {:>7} {:>6} {:>6} {:>8} {:>7} {:>6} {:>6} {:>6} {:>6} {:>5}",
			name,
			s.truth,
			s.detected,
			pct(s.conpoff.f),
			pct(s.conp.f),
			pct(s.con.f),
			format!("{:.1}c", s.center.median_cents),
			format!("{:.0}c", s.center.p95_cents),
			pct(s.center.over_10),
			pct(s.mixed_rate),
			pct(s.split_rate),
			pct(s.merge_rate),
			pct(s.octave_rate),
		);
	};
	for case in &report.cases {
		row(&case.name, &case.score);
	}
	row("TOTAL", &report.total);
}

fn pct(x: f64) -> String {
	format!("{:.1}%", 100.0 * x)
}

/// Every `<name>.json` with an audio file of the same stem beside it.
fn load_labeled(dir: &Path) -> Result<Vec<Case>> {
	let mut labels_files = Vec::new();
	collect_json(dir, &mut labels_files).with_context(|| format!("reading {}", dir.display()))?;
	labels_files.sort();
	let mut cases = Vec::new();
	for json in labels_files {
		let Some(audio) = AUDIO_EXTENSIONS
			.iter()
			.map(|ext| json.with_extension(ext))
			.find(|p| p.is_file())
		else {
			continue;
		};
		let text = std::fs::read_to_string(&json)
			.with_context(|| format!("reading {}", json.display()))?;
		let truth = labels::parse(&text).with_context(|| json.display().to_string())?;
		let buffer =
			decode_file(&audio).with_context(|| format!("decoding {}", audio.display()))?;
		let mono = buffer.to_mono();
		let name = json
			.strip_prefix(dir)
			.unwrap_or(&json)
			.with_extension("")
			.to_string_lossy()
			.replace('\\', "/");
		let frames = labels::frames(&truth, mono.duration_seconds());
		cases.push(Case {
			name,
			tags: vec!["real".into()],
			sample_rate: mono.sample_rate(),
			samples: mono.channel(0).to_vec(),
			truth,
			frames,
		});
	}
	if cases.is_empty() {
		bail!(
			"no labeled recordings in {}: put <name>.json labels next to <name>.wav (or flac, mp3...)",
			dir.display()
		);
	}
	Ok(cases)
}

fn collect_json(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
	for entry in std::fs::read_dir(dir)? {
		let path = entry?.path();
		if path.is_dir() {
			collect_json(&path, out)?;
		} else if path
			.extension()
			.is_some_and(|e| e.eq_ignore_ascii_case("json"))
		{
			out.push(path);
		}
	}
	Ok(())
}

fn write_corpus(dir: &Path, sample_rate: u32) -> Result<()> {
	std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
	let cases = synth::corpus(sample_rate);
	for case in &cases {
		let buffer = AudioBuffer::from_planar(case.sample_rate, vec![case.samples.clone()])?;
		let wav = dir.join(format!("{}.wav", case.name));
		export(&buffer, &wav, &ExportOptions::wav(WavSampleFormat::Float32))?;
		std::fs::write(
			dir.join(format!("{}.json", case.name)),
			labels::to_json(&case.truth),
		)?;
	}
	println!("wrote {} cases to {}", cases.len(), dir.display());
	Ok(())
}

fn analyze(file: &Path, json: bool, no_cache: bool) -> Result<()> {
	let buffer = decode_file(file).with_context(|| format!("decoding {}", file.display()))?;
	let mono = buffer.to_mono();
	let (samples, rate) = (mono.channel(0), mono.sample_rate());
	let opts = AnalysisOptions::default();
	let started = std::time::Instant::now();
	let cache = Cache::new(cadenza_core::paths::cache_dir().join("analysis"));
	let analysis = if no_cache {
		analysis::analyze(samples, rate, &opts, &mut |_| true)
	} else {
		cache.get_or_analyze(samples, rate, &opts, &mut |_| true)
	}
	.context("analysis cancelled")?;
	let elapsed = started.elapsed().as_secs_f64();
	if json {
		println!("{}", serde_json::to_string(&analysis)?);
		return Ok(());
	}
	let voiced: Vec<f64> = analysis.frames.iter().filter_map(|f| f.pitch).collect();
	let mut sorted = voiced.clone();
	sorted.sort_by(f64::total_cmp);
	let name = |p: f64| {
		let midi = p.round().clamp(0.0, 127.0) as u8;
		let cents = ((p - p.round()) * 100.0).round();
		format!("{}{:+}c", cadenza_theory::format_pitch(midi, false), cents)
	};
	println!("file: {}", file.display());
	println!(
		"duration: {:.2} s at {} Hz, {} frames of {:.1} ms",
		analysis.duration,
		analysis.sample_rate,
		analysis.frames.len(),
		1000.0 * analysis.hop_seconds()
	);
	println!(
		"voiced: {:.1} % of frames",
		100.0 * voiced.len() as f64 / analysis.frames.len().max(1) as f64
	);
	if let (Some(low), Some(high)) = (sorted.first(), sorted.last()) {
		let pick = |q: f64| sorted[((sorted.len() - 1) as f64 * q).round() as usize];
		println!(
			"pitch: {} .. {} (5-95 %: {} .. {}, median {})",
			name(*low),
			name(*high),
			name(pick(0.05)),
			name(pick(0.95)),
			name(pick(0.5))
		);
	}
	println!(
		"time: {:.2} s ({:.0}x realtime)",
		elapsed,
		analysis.duration / elapsed.max(1e-9)
	);
	Ok(())
}

fn segment_file(file: &Path, sensitivity: f64, splits: &[f64], json: bool) -> Result<()> {
	if sensitivity.is_nan() || sensitivity <= 0.0 {
		bail!("--sensitivity must be above 0 (1 = default, 2 = more notes, 0.5 = fewer)");
	}
	let buffer = decode_file(file).with_context(|| format!("decoding {}", file.display()))?;
	let mono = buffer.to_mono();
	let cache = Cache::new(cadenza_core::paths::cache_dir().join("analysis"));
	let analysis = cache
		.get_or_analyze(
			mono.channel(0),
			mono.sample_rate(),
			&AnalysisOptions::default(),
			&mut |_| true,
		)
		.context("analysis cancelled")?;
	let opts = SegmentOptions {
		sensitivity,
		..SegmentOptions::default()
	};
	let result = segment::segment(&analysis, &opts, splits);
	if json {
		println!("{}", serde_json::to_string_pretty(&result)?);
		return Ok(());
	}
	println!(
		"{:>3} {:>8} {:>8} {:>17} {:>9} {:>7} {:>5} {:>13} {:>5}  flags",
		"#", "onset", "offset", "core", "center", "cents", "drift", "vibrato", "conf"
	);
	for (i, n) in result.notes.iter().enumerate() {
		let midi = n.center.round().clamp(0.0, 127.0) as u8;
		let flags: Vec<String> = n
			.flags
			.iter()
			.map(|f| match f {
				NoteFlag::MaybeTwoNotes { at, cents } => {
					format!("two notes? split at {at:.3} s ({cents:.0} c apart)")
				}
				NoteFlag::ShortCore => "short core".into(),
				NoteFlag::WeakVoicing => "weak voicing".into(),
			})
			.collect();
		println!(
			"{:>3} {:>8.3} {:>8.3} {:>8.3}-{:<8.3} {:>9} {:>+7.1} {:>5.0} {:>13} {:>5.2}  {}",
			i + 1,
			n.onset,
			n.offset,
			n.core_start,
			n.core_end,
			cadenza_theory::format_pitch(midi, false),
			(n.center - n.center.round()) * 100.0,
			n.drift,
			n.vibrato
				.map(|v| format!("{:.1} Hz {:.0} c", v.rate_hz, v.depth_cents))
				.unwrap_or_default(),
			n.confidence,
			flags.join("; ")
		);
	}
	if !result.sibilants.is_empty() {
		let list: Vec<String> = result
			.sibilants
			.iter()
			.map(|s| format!("{:.3}-{:.3}", s.start, s.end))
			.collect();
		println!("sibilants: {}", list.join(", "));
	}
	Ok(())
}
