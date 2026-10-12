//! `cadenza export`: mixdown and/or stems with the export dialog's options.

use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use cadenza_engine::{PluginHosting, RenderOptions, export_project};
use cadenza_mcp::tools::render::{
	ExportStemsArgs, MetadataArgs, NormalizeArgs, NormalizeMode, StemKind, export_output,
	stems_spec,
};
use clap::Args;

#[derive(Args)]
pub struct ExportArgs {
	project: PathBuf,
	/// Output folder (default: the project's Renders/ folder)
	#[arg(short, long)]
	output: Option<PathBuf>,
	/// Base file name (default: the project name)
	#[arg(long)]
	name: Option<String>,
	/// wav, flac or mp3 (mp3 only in builds with the `mp3` feature)
	#[arg(long, default_value = "wav")]
	format: String,
	/// WAV: 16, 24 or 32 (float); FLAC: 16 or 24
	#[arg(long)]
	bit_depth: Option<u32>,
	/// MP3 bitrate in kbps
	#[arg(long)]
	bitrate: Option<u16>,
	/// song (default), bars:A-B (1-based, B included) or beats:A-B
	#[arg(long, default_value = "song")]
	range: String,
	/// Cut at the range end instead of letting effects ring out (default for bars/beats)
	#[arg(long, conflicts_with = "tail")]
	no_tail: bool,
	/// Append the effect tail after a bars/beats range
	#[arg(long)]
	tail: bool,
	/// Stems: tracks, buses, or a comma-separated list of track names/ids
	#[arg(long)]
	stems: Option<String>,
	/// Skip the mixdown (stems only)
	#[arg(long, requires = "stems")]
	no_mixdown: bool,
	/// Run each stem through the master chain
	#[arg(long, requires = "stems")]
	through_master: bool,
	/// Tap stems before the channel fader
	#[arg(long, requires = "stems")]
	pre_fader: bool,
	/// Render sample rate
	#[arg(long, default_value_t = 48_000)]
	sample_rate: u32,
	/// Resample the files to this rate (e.g. 44100)
	#[arg(long)]
	output_rate: Option<u32>,
	/// peak:<dBFS> or lufs:<LUFS>[:<true-peak dBTP>] (e.g. peak:-1, lufs:-14:-1)
	#[arg(long)]
	normalize: Option<String>,
	/// Normalize every stem on its own (default: stems share the mix gain)
	#[arg(long)]
	normalize_stems_individually: bool,
	#[arg(long)]
	title: Option<String>,
	#[arg(long)]
	artist: Option<String>,
	#[arg(long)]
	album: Option<String>,
	#[arg(long)]
	genre: Option<String>,
	#[arg(long)]
	date: Option<String>,
	#[arg(long)]
	comment: Option<String>,
}

fn parse_pair(text: &str, what: &str) -> Result<(f64, f64)> {
	let (a, b) = text
		.split_once('-')
		.with_context(|| format!("{what} range must look like A-B"))?;
	Ok((
		a.trim()
			.parse()
			.with_context(|| format!("bad {what} '{a}'"))?,
		b.trim()
			.parse()
			.with_context(|| format!("bad {what} '{b}'"))?,
	))
}

fn parse_normalize(text: &str) -> Result<NormalizeArgs> {
	let mut parts = text.split(':');
	let mode = parts.next().unwrap_or_default().trim().to_ascii_lowercase();
	let num = |p: Option<&str>| -> Result<Option<f32>> {
		p.map(|v| {
			v.trim()
				.parse::<f32>()
				.with_context(|| format!("bad number '{v}'"))
		})
		.transpose()
	};
	match mode.as_str() {
		"peak" => Ok(NormalizeArgs {
			mode: NormalizeMode::Peak,
			peak_db: num(parts.next())?,
			..Default::default()
		}),
		"lufs" | "loudness" => Ok(NormalizeArgs {
			mode: NormalizeMode::Loudness,
			lufs: num(parts.next())?,
			true_peak_db: num(parts.next())?,
			..Default::default()
		}),
		_ => bail!("--normalize must be peak:<dBFS> or lufs:<LUFS>[:<dBTP>], got '{text}'"),
	}
}

pub fn run(args: ExportArgs, hosting: PluginHosting) -> Result<()> {
	let (document, missing) = cadenza_mcp::tools::portable::open_document(&args.project)
		.with_context(|| format!("loading project {}", args.project.display()))?;
	for m in &missing {
		eprintln!("warning: missing file {}", m.path.display());
	}
	let project = document.project().clone();
	let mut a = ExportStemsArgs {
		folder: args.output.as_ref().map(|p| p.display().to_string()),
		name: args.name.clone(),
		format: Some(args.format.clone()),
		bit_depth: args.bit_depth,
		mp3_bitrate_kbps: args.bitrate,
		include_mixdown: Some(!args.no_mixdown),
		through_master: Some(args.through_master),
		pre_fader: Some(args.pre_fader),
		sample_rate: Some(args.sample_rate),
		output_sample_rate: args.output_rate,
		normalize: args.normalize.as_deref().map(parse_normalize).transpose()?,
		normalize_individually: Some(args.normalize_stems_individually),
		metadata: Some(MetadataArgs {
			title: args.title.clone().or_else(|| Some(project.name.clone())),
			artist: args.artist.clone(),
			album: args.album.clone(),
			genre: args.genre.clone(),
			date: args.date.clone(),
			comment: args.comment.clone(),
			track_number: None,
		}),
		..Default::default()
	};
	let range = args.range.trim().to_ascii_lowercase();
	if let Some(r) = range.strip_prefix("bars:") {
		let (s, e) = parse_pair(r, "bar")?;
		a.start_bar = Some(s as u32);
		a.end_bar = Some(e as u32);
	} else if let Some(r) = range.strip_prefix("beats:") {
		let (s, e) = parse_pair(r, "beat")?;
		a.start_beat = Some(s);
		a.end_beat = Some(e);
	} else if range != "song" {
		bail!(
			"--range must be song, bars:A-B or beats:A-B, got '{}'",
			args.range
		);
	}
	if args.tail {
		a.tail = Some(true);
	} else if args.no_tail {
		a.tail = Some(false);
	}
	match args.stems.as_deref().map(str::trim) {
		None => {}
		Some("tracks") => a.stems = StemKind::Tracks,
		Some("buses") => a.stems = StemKind::Buses,
		Some(list) => {
			a.stems = StemKind::Selected;
			for q in list.split(',').filter(|q| !q.trim().is_empty()) {
				a.track_ids.push(crate::find_track(&project, q)?.id);
			}
		}
	}
	let renders = cadenza_model::ProjectLayout::new(&args.project).renders_dir();
	let mut spec = stems_spec(&project, &a, Some(renders))?;
	if args.stems.is_none() {
		spec.stems = cadenza_engine::StemSet::None;
	}
	let opts = RenderOptions {
		sample_rate: args.sample_rate,
		plugins: crate::plugins_for(&project),
		hosting,
		..RenderOptions::default()
	};
	let mut last = -1i32;
	let report = export_project(&project, &opts, &spec, &mut |p| {
		let pct = (p.fraction * 100.0) as i32;
		if pct != last {
			last = pct;
			eprint!("\r{pct:>3}% {:<40}", p.stage);
			let _ = std::io::stderr().flush();
		}
		true
	})?;
	eprintln!();
	for f in &report.files {
		eprintln!(
			"wrote {} ({:.1} LUFS, {:.1} dBTP)",
			f.path.display(),
			f.stats.lufs_integrated,
			f.stats.true_peak_db
		);
	}
	for n in &report.notes {
		eprintln!("note: {n}");
	}
	crate::print_json(&export_output(report))
}
