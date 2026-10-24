//! `cadenza notes eval | corpus`: the note-detection evaluation harness (NE-0). `eval`
//! scores a detector on the synthetic corpus and on hand-labeled recordings; `corpus`
//! writes the synthetic corpus as WAV + label files to listen to or to label tools with.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use cadenza_audio_io::{AudioBuffer, ExportOptions, WavSampleFormat, decode_file, export};
use cadenza_notes::eval::{self, Case, Report, Score, labels, synth};
use clap::{Args, Subcommand};

const AUDIO_EXTENSIONS: &[&str] = &["wav", "flac", "mp3", "ogg", "m4a", "aif", "aiff"];

#[derive(Subcommand)]
pub enum NotesCommand {
	/// Score a note detector: COnPOff/COnP/COn F-measures, pitch-center error in cents,
	/// mixed regions (one region over two real notes), splits, merges, octave errors.
	Eval(EvalArgs),
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
	let mut cases = Vec::new();
	if !args.no_synthetic {
		cases.extend(synth::corpus(args.sample_rate));
	}
	let corpus = args
		.corpus
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
	let report = eval::evaluate(detector.as_ref(), &cases);
	if args.json {
		println!("{}", serde_json::to_string_pretty(&report)?);
	} else {
		print_table(&report);
	}
	Ok(())
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
		cases.push(Case {
			name,
			tags: vec!["real".into()],
			sample_rate: mono.sample_rate(),
			samples: mono.channel(0).to_vec(),
			truth,
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
