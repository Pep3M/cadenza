//! `cadenza library list | validate | import`: the style library's packs on the command
//! line. Importing converts a standard MIDI file into a pattern (and its groove) inside a
//! user pack; it never downloads anything.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use cadenza_library::import::{ImportNote, ImportOptions, import_pattern};
use cadenza_library::{
	Item, Library, Pack, PackManifest, PackOrigin, PackReport, Role, default_library_dir,
};
use cadenza_midi_io::{GM_DRUM_CHANNEL, OtherEvent, ParsedMidiFile};
use cadenza_theory::Scale;
use clap::{Args, Subcommand};
use serde::Serialize;

#[derive(Subcommand)]
pub enum LibraryCommand {
	/// List the installed packs (the embedded `core` pack and the user packs of the library
	/// folder) with license and item counts, and any problems found loading them.
	List {
		/// Also list every item id.
		#[arg(long)]
		items: bool,
		#[arg(long)]
		json: bool,
	},
	/// Check a pack folder (with a pack.json) or a library folder of packs: schema, budgets,
	/// lane keys, license fields and references. Exits with 1 when anything is wrong.
	Validate { dir: PathBuf },
	/// Convert a standard MIDI file into a library pattern (plus its groove) in a user pack.
	Import(Box<ImportArgs>),
}

#[derive(Args)]
pub struct ImportArgs {
	/// The .mid file.
	file: PathBuf,
	/// Pack folder to write into (created with a pack.json when missing); default: the
	/// `user` pack of the library folder.
	#[arg(long)]
	pack: Option<PathBuf>,
	/// Item id (slug) of the pattern; default: from the file name.
	#[arg(long)]
	id: Option<String>,
	/// Display name; default: the file name.
	#[arg(long)]
	name: Option<String>,
	/// 1-based track of the file to import (among tracks with notes).
	#[arg(long)]
	track: Option<usize>,
	/// 1-based MIDI channel to import (10 = GM drums).
	#[arg(long)]
	channel: Option<u8>,
	/// drums, perc, bass, comping, arp, pad, lead or fill (default: detected from the GM
	/// drum channel, the program, the register).
	#[arg(long)]
	role: Option<String>,
	/// Key of the material ("A minor"); default: inferred from the notes.
	#[arg(long)]
	key: Option<String>,
	/// Chords under the material ("Am F C G", or roman numerals with --key): pitched parts
	/// are then written relative to the chords.
	#[arg(long)]
	chords: Option<String>,
	#[arg(long, default_value_t = 4.0)]
	beats_per_chord: f64,
	/// Grid resolution (4 = sixteenths, 3 = eighth-note triplets).
	#[arg(long, default_value_t = 4)]
	steps_per_beat: u32,
	/// Bars to import from the first bar with notes (at most 16).
	#[arg(long)]
	bars: Option<u32>,
	/// Genre tags ("reggaeton"); repeatable.
	#[arg(long)]
	genre: Vec<String>,
	/// Replace an existing item with the same id.
	#[arg(long)]
	force: bool,
}

pub fn run(command: LibraryCommand) -> Result<()> {
	match command {
		LibraryCommand::List { items, json } => list(items, json),
		LibraryCommand::Validate { dir } => validate(&dir),
		LibraryCommand::Import(args) => import(*args),
	}
}

#[derive(Serialize)]
struct PackRow {
	id: String,
	name: String,
	version: String,
	license: String,
	attribution: String,
	redistributable: bool,
	location: String,
	items: usize,
	#[serde(skip_serializing_if = "Vec::is_empty")]
	ids: Vec<String>,
}

fn location(pack: &Pack) -> String {
	match &pack.origin {
		PackOrigin::Embedded => "(embedded)".into(),
		PackOrigin::Dir(d) => d.display().to_string(),
	}
}

fn print_issues(reports: &[PackReport]) -> usize {
	let mut count = 0;
	for r in reports {
		for issue in &r.issues {
			count += 1;
			eprintln!(
				"{} {}: {}",
				if r.loaded { "warning" } else { "error" },
				Path::new(&r.location).join(&issue.file).display(),
				issue.message
			);
		}
		if !r.loaded {
			eprintln!("error: pack at {} was not loaded", r.location);
		}
	}
	count
}

fn list(with_items: bool, json: bool) -> Result<()> {
	let dir = default_library_dir();
	let library = Library::load(Some(&dir));
	let rows: Vec<PackRow> = library
		.packs()
		.iter()
		.map(|p| PackRow {
			id: p.manifest.id.clone(),
			name: p.manifest.name.clone(),
			version: p.manifest.version.clone(),
			license: p.manifest.license.clone(),
			attribution: p.manifest.attribution.clone(),
			redistributable: p.manifest.redistributable,
			location: location(p),
			items: p.items.len(),
			ids: if with_items {
				p.items.iter().map(|i| i.id.clone()).collect()
			} else {
				Vec::new()
			},
		})
		.collect();
	if json {
		crate::print_json(&rows)?;
	} else {
		println!("library folder: {}", dir.display());
		for r in &rows {
			println!(
				"{:<12} {:<28} v{:<8} {:<14} {:>4} items  {}",
				r.id, r.name, r.version, r.license, r.items, r.location
			);
			if !r.attribution.is_empty() {
				println!("             by {}", r.attribution);
			}
			for id in &r.ids {
				println!("             {id}");
			}
		}
	}
	print_issues(library.reports());
	Ok(())
}

fn validate(dir: &Path) -> Result<()> {
	if !dir.is_dir() {
		bail!("{} is not a folder", dir.display());
	}
	let mut library = Library::core();
	let is_pack = dir.join("pack.json").is_file();
	if is_pack {
		let (pack, report) = Pack::load_dir(dir);
		library.add(pack, report);
	} else {
		let mut found = false;
		let mut dirs: Vec<PathBuf> = std::fs::read_dir(dir)?
			.flatten()
			.map(|e| e.path())
			.filter(|p| p.join("pack.json").is_file())
			.collect();
		dirs.sort();
		for d in dirs {
			found = true;
			let (pack, report) = Pack::load_dir(&d);
			library.add(pack, report);
		}
		if !found {
			bail!(
				"{} holds no pack (a folder with a pack.json) and is not one",
				dir.display()
			);
		}
	}
	library.check_references();
	// The embedded pack is checked by its own tests; report the folder's packs.
	let reports: Vec<PackReport> = library.reports()[1..].to_vec();
	let problems = print_issues(&reports);
	for r in &reports {
		println!(
			"{}: {} ({} items{})",
			r.location,
			if r.loaded && r.issues.is_empty() {
				"ok"
			} else if r.loaded {
				"loaded with problems"
			} else {
				"invalid"
			},
			r.items,
			if r.issues.is_empty() {
				String::new()
			} else {
				format!(", {} problems", r.issues.len())
			}
		);
	}
	if problems > 0 || reports.iter().any(|r| !r.loaded) {
		std::process::exit(1);
	}
	Ok(())
}

fn slug(s: &str) -> String {
	let mut out = String::new();
	for c in cadenza_library::normalize(s).chars() {
		if out.len() < 64 {
			out.push(c);
		}
	}
	if out.is_empty() {
		"imported".into()
	} else {
		out
	}
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
	if let Some(parent) = path.parent() {
		std::fs::create_dir_all(parent)?;
	}
	let mut buf = Vec::new();
	let mut ser = serde_json::Serializer::with_formatter(
		&mut buf,
		serde_json::ser::PrettyFormatter::with_indent(b"\t"),
	);
	value.serialize(&mut ser)?;
	buf.push(b'\n');
	std::fs::write(path, buf).with_context(|| format!("writing {}", path.display()))
}

/// The notes to import and whether they are GM drums, plus the channel's program.
fn pick_notes(
	parsed: &ParsedMidiFile,
	track: Option<usize>,
	channel: Option<u8>,
) -> Result<(Vec<ImportNote>, bool, Option<u8>)> {
	let with_notes: Vec<_> = parsed
		.tracks
		.iter()
		.filter(|t| !t.notes.is_empty())
		.collect();
	if with_notes.is_empty() {
		bail!("the file has no notes");
	}
	let channel = match channel {
		Some(c) if (1..=16).contains(&c) => Some(c - 1),
		Some(_) => bail!("--channel is 1..=16 (10 = GM drums)"),
		None => None,
	};
	let tracks: Vec<_> =
		match track {
			Some(i) => vec![*with_notes.get(i.wrapping_sub(1)).with_context(|| {
				format!("--track is 1..={} (tracks with notes)", with_notes.len())
			})?],
			None if channel.is_some() => with_notes.clone(),
			None if with_notes.len() == 1 => with_notes.clone(),
			None => {
				let list: Vec<String> = with_notes
					.iter()
					.enumerate()
					.map(|(i, t)| {
						format!(
							"  {}: {} ({} notes{})",
							i + 1,
							t.name.as_deref().unwrap_or("unnamed"),
							t.notes.len(),
							if t.is_drum { ", drums" } else { "" }
						)
					})
					.collect();
				bail!(
					"the file has {} tracks with notes; pick one with --track (or --channel):\n{}",
					with_notes.len(),
					list.join("\n")
				);
			}
		};
	let mut channels: Vec<u8> = tracks
		.iter()
		.flat_map(|t| t.notes.iter().map(|n| n.channel))
		.collect();
	channels.sort_unstable();
	channels.dedup();
	let channel = match channel {
		Some(c) => c,
		None if channels.len() == 1 => channels[0],
		None => bail!(
			"the notes use channels {}; pick one with --channel",
			channels
				.iter()
				.map(|c| (c + 1).to_string())
				.collect::<Vec<_>>()
				.join(", ")
		),
	};
	let notes: Vec<ImportNote> = tracks
		.iter()
		.flat_map(|t| t.notes.iter())
		.filter(|n| n.channel == channel)
		.map(|n| ImportNote {
			pitch: n.pitch,
			start_beats: n.start.beats(),
			length_beats: n.length.beats(),
			velocity: n.velocity,
		})
		.collect();
	if notes.is_empty() {
		bail!("no notes on channel {}", channel + 1);
	}
	let program = tracks
		.iter()
		.flat_map(|t| t.other_events.iter())
		.find_map(|e| match e.event {
			OtherEvent::ProgramChange {
				channel: c,
				program,
			} if c == channel => Some(program),
			_ => None,
		});
	Ok((notes, channel == GM_DRUM_CHANNEL, program))
}

fn import(args: ImportArgs) -> Result<()> {
	let bytes =
		std::fs::read(&args.file).with_context(|| format!("reading {}", args.file.display()))?;
	let parsed = cadenza_midi_io::parse_smf(&bytes)
		.with_context(|| format!("{} is not a standard MIDI file", args.file.display()))?;
	let (notes, drums, program) = pick_notes(&parsed, args.track, args.channel)?;
	let stem = args
		.file
		.file_stem()
		.and_then(|s| s.to_str())
		.unwrap_or("imported")
		.to_owned();
	let pack_dir = args
		.pack
		.clone()
		.unwrap_or_else(|| default_library_dir().join("user"));
	let manifest_path = pack_dir.join("pack.json");
	let manifest: PackManifest = if manifest_path.is_file() {
		serde_json::from_str(&std::fs::read_to_string(&manifest_path)?)
			.with_context(|| format!("reading {}", manifest_path.display()))?
	} else {
		let id = slug(
			pack_dir
				.file_name()
				.and_then(|n| n.to_str())
				.unwrap_or("user"),
		);
		let manifest = PackManifest {
			format_version: 1,
			name: format!("{} (imports)", id),
			id,
			version: "0.1.0".into(),
			license: "LicenseRef-user-imports".into(),
			attribution: String::new(),
			source: None,
			homepage: None,
			description: "Patterns imported from MIDI files with `cadenza library import`.".into(),
			redistributable: false,
		};
		write_json(&manifest_path, &manifest)?;
		println!("created pack '{}' at {}", manifest.id, pack_dir.display());
		manifest
	};
	let role = match args.role.as_deref() {
		Some(r) => Some(Role::parse(r).with_context(|| {
			format!("--role '{r}': use drums, perc, bass, comping, arp, pad, lead or fill")
		})?),
		None => None,
	};
	let key = args.key.as_deref().map(Scale::parse).transpose()?;
	let chords = match args.chords.as_deref() {
		Some(c) => cadenza_library::parse_chords(c, key.as_ref()).map_err(anyhow::Error::msg)?,
		None => Vec::new(),
	};
	let meter = parsed
		.time_signatures
		.first()
		.map(|(_, ts)| (ts.numerator, ts.denominator))
		.unwrap_or((4, 4));
	let slug = slug(args.id.as_deref().unwrap_or(&stem));
	let options = ImportOptions {
		slug: slug.clone(),
		name: args.name.clone().unwrap_or_else(|| stem.clone()),
		pack: manifest.id.clone(),
		role,
		drums,
		program,
		key,
		chords,
		beats_per_chord: args.beats_per_chord,
		steps_per_beat: args.steps_per_beat,
		bars: args.bars,
		meter,
		tempo: parsed.tempo_map.first().map(|t| t.bpm),
		genres: args.genre.iter().map(|g| slug_genre(g)).collect(),
		source: args
			.file
			.file_name()
			.and_then(|n| n.to_str())
			.unwrap_or_default()
			.to_owned(),
	};
	let imported = import_pattern(&notes, &options).map_err(anyhow::Error::msg)?;
	let role_dir = imported.pattern.role.as_str();
	let pattern_path = pack_dir
		.join("patterns")
		.join(role_dir)
		.join(format!("{slug}.json"));
	let existing = Pack::load_dir(&pack_dir).0.is_some_and(|p| {
		p.items
			.iter()
			.any(|i| i.id == format!("{}/{slug}", manifest.id))
	});
	if (existing || pattern_path.exists()) && !args.force {
		bail!(
			"'{}/{slug}' already exists in {}; pass --force to replace it or --id to rename",
			manifest.id,
			pack_dir.display()
		);
	}
	for w in &imported.warnings {
		eprintln!("warning: {w}");
	}
	write_json(&pattern_path, &Item::Pattern(imported.pattern.clone()))?;
	println!("wrote {}", pattern_path.display());
	if let Some(groove) = &imported.groove {
		let path = pack_dir.join("grooves").join(format!("{}.json", groove.id));
		write_json(&path, &Item::Groove(groove.clone()))?;
		println!("wrote {}", path.display());
	}
	let midi = pack_dir.join("midi").join(format!("{slug}.mid"));
	std::fs::create_dir_all(midi.parent().expect("has a parent"))?;
	std::fs::write(&midi, &bytes)?;
	let (_, report) = Pack::load_dir(&pack_dir);
	let ours: Vec<_> = report
		.issues
		.iter()
		.filter(|i| i.file.contains(&slug))
		.collect();
	for i in &ours {
		eprintln!("error {}: {}", i.file, i.message);
	}
	if !ours.is_empty() {
		bail!("the imported pattern does not validate; see the errors above");
	}
	println!(
		"imported {}/{slug}: {} {}, {} bar(s), {} lane(s){}",
		manifest.id,
		imported.pattern.role,
		match imported.pattern.harmony {
			cadenza_library::Harmony::Chord => "relative to the chords",
			cadenza_library::Harmony::Scale => "in scale degrees",
			cadenza_library::Harmony::Absolute => "in absolute notes",
			cadenza_library::Harmony::None => "on drum lanes",
		},
		imported.pattern.bars,
		imported.pattern.lanes.len(),
		if imported.groove.is_some() {
			", with its groove"
		} else {
			""
		}
	);
	Ok(())
}

fn slug_genre(g: &str) -> String {
	cadenza_library::normalize(g)
}
