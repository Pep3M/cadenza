//! Pure logic of the Drum Sampler editor (`drum_sampler`, design `17 Instruments`, 17d):
//! pad colours, note labels and tags (choke group, extra output, layers / round-robin),
//! the pad -> pad swap / copy, velocity layers and round-robin, the kit-from-folder
//! mapping by file names, the A/D envelope the processor plays (`sample_voice::ZoneEnv`),
//! the waveform's zoom window and the pad flashes from the processor's trigger counters
//! ([`cadenza_dsp::readings::DRUM_PAD_TRIGGERS_START`]). Every builder returns one command
//! (one undo step) through [`super::samples::set_zones`].

use crate::i18n::{tr, trf, trn};
use std::path::{Path, PathBuf};

use cadenza_dsp::instruments::drum_sampler::{PADS, pad_layout};
use cadenza_dsp::readings;
use cadenza_model::{Command, Device, LoopMode, Project, SampleRef, SampleZone};

use super::drum_machine_editor::flash_level;
use super::files::{FileKind, classify};
use super::samples::{device, pad_label, pad_zones, set_zones};
use super::selection::DeviceRef;

/// Choke groups and round-robin groups the model accepts.
pub const GROUPS: u8 = 16;
/// The processor's extra stereo outputs.
pub const OUTPUTS: u8 = cadenza_dsp::instruments::drum_sampler::AUX_OUTPUTS as u8;
/// The decay node sits where the decay has done a third of its way to -60 dB (-20 dB).
pub const DECAY_NODE: f32 = 1.0 / 3.0;
/// `decay_coeff`: an exponential stage reaches -60 dB at its time.
const LN_1000: f32 = 6.907_755;
/// The shortest stretch of sample the waveform zooms in to (seconds).
const MIN_SPAN: f64 = 0.005;

// --- Pads ----------------------------------------------------------------------------

/// Index into the kit palette (`theme::BAND_COLORS`) of pad `note`: kicks, snares (with
/// clap and rim), hats, toms, cymbals, percussion, like the Drum Machine's voices.
pub fn pad_color(note: u8) -> usize {
	match note {
		35 | 36 => 0,
		37..=40 => 1,
		42 | 44 | 46 => 3,
		41 | 43 | 45 | 47 | 48 | 50 => 4,
		49 | 51..=53 | 55 | 57 | 59 => 6,
		60..=66 => 5,
		_ => 7,
	}
}

/// The note name the processor's description uses (C1 = 36), `#` for sharps.
pub fn note_label(note: u8) -> String {
	const NAMES: [&str; 12] = [
		"C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
	];
	format!(
		"{}{}",
		NAMES[usize::from(note % 12)],
		i32::from(note / 12) - 2
	)
}

/// A short name for small pads ("Cl Hat").
pub fn pad_short(note: u8) -> &'static str {
	match note {
		35 => tr("Kick 2"),
		36 => tr("Kick"),
		37 => tr("Rim"),
		38 => tr("Snare"),
		39 => tr("Clap"),
		40 => tr("Snr 2"),
		41 => tr("Lo Tom"),
		42 => tr("Cl Hat"),
		44 => tr("Pd Hat"),
		45 => tr("Md Tom"),
		46 => tr("Op Hat"),
		48 => tr("Hi Tom"),
		49 => tr("Crash"),
		51 => tr("Ride"),
		54 => tr("Tamb"),
		56 => tr("Cowb"),
		60 => tr("Hi Bgo"),
		61 => tr("Lo Bgo"),
		62 => tr("Mt Cga"),
		63 => tr("Op Cga"),
		64 => tr("Lo Cga"),
		65 => tr("Hi Tim"),
		66 => tr("Lo Tim"),
		67 => tr("Hi Ago"),
		68 => tr("Lo Ago"),
		69 => tr("Cabasa"),
		70 => tr("Shaker"),
		73 | 74 => tr("Guiro"),
		75 => tr("Claves"),
		76 => tr("Hi Blk"),
		77 => tr("Lo Blk"),
		78 | 79 => tr("Cuica"),
		80 | 81 => tr("Tri"),
		_ => tr("Pad"),
	}
}

/// The kit's 16 pads (note, GM name): the GM pads, with spare ones given to the other
/// drum notes its zones use (`drum_sampler::pad_layout`, the processor's own layout).
pub fn kit_pads(device: &Device) -> [(u8, &'static str); 16] {
	pad_layout(
		device
			.zones
			.iter()
			.filter(|z| z.is_single_key())
			.map(|z| z.key_low),
	)
}

/// Pad `i` (0..16, pad 1 bottom-left) of the 4x4 grid: its (row from the top, column).
pub fn grid_cell(i: usize) -> (usize, usize) {
	(3 - i / 4, i % 4)
}

/// The layers of pad `note` (zone indices) in chip order: by velocity, then round-robin.
pub fn pad_layers(device: &Device, note: u8) -> Vec<usize> {
	let mut layers = pad_zones(device, note);
	layers.sort_by(|&a, &b| {
		let (za, zb) = (&device.zones[a], &device.zones[b]);
		za.vel_low
			.total_cmp(&zb.vel_low)
			.then(za.round_robin.cmp(&zb.round_robin))
			.then(a.cmp(&b))
	});
	layers
}

/// What a pad's tags say.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PadTags {
	/// Choke group (0 = none).
	pub choke: u8,
	/// Extra output (0 = main).
	pub output: u8,
	/// Velocity layers (distinct velocity ranges).
	pub layers: usize,
	/// Samples taking turns within one velocity range (1 = no round-robin).
	pub round_robin: usize,
}

impl PadTags {
	/// "C1", "→2", "L3", "RR3" (only what applies).
	pub fn labels(&self) -> Vec<String> {
		let mut out = Vec::new();
		if self.choke > 0 {
			out.push(format!("C{}", self.choke));
		}
		if self.output > 0 {
			out.push(format!("\u{2192}{}", self.output));
		}
		if self.layers > 1 {
			out.push(format!("L{}", self.layers));
		}
		if self.round_robin > 1 {
			out.push(format!("RR{}", self.round_robin));
		}
		out
	}
}

fn same_range(a: &SampleZone, b: &SampleZone) -> bool {
	(a.vel_low - b.vel_low).abs() < 1e-4 && (a.vel_high - b.vel_high).abs() < 1e-4
}

/// The tags of pad `note`.
pub fn pad_tags(device: &Device, note: u8) -> PadTags {
	let zones: Vec<&SampleZone> = pad_zones(device, note)
		.into_iter()
		.map(|i| &device.zones[i])
		.collect();
	let Some(first) = zones.first() else {
		return PadTags::default();
	};
	let mut ranges: Vec<&SampleZone> = Vec::new();
	for z in &zones {
		if !ranges.iter().any(|r| same_range(r, z)) {
			ranges.push(z);
		}
	}
	let round_robin = zones
		.iter()
		.filter(|z| z.round_robin > 0)
		.map(|z| {
			zones
				.iter()
				.filter(|o| o.round_robin == z.round_robin && same_range(o, z))
				.count()
		})
		.max()
		.unwrap_or(1);
	PadTags {
		choke: first.choke,
		output: first.output,
		layers: ranges.len(),
		round_robin,
	}
}

/// Whether pad `note`'s layers take turns (round-robin) rather than split by velocity.
pub fn is_round_robin(device: &Device, note: u8) -> bool {
	pad_tags(device, note).round_robin > 1
}

/// `cmd` as one undo step named `label`.
pub fn labeled(cmd: Command, label: String) -> Command {
	match cmd {
		Command::Batch { commands, .. } => Command::Batch { label, commands },
		other => Command::Batch {
			label,
			commands: vec![other],
		},
	}
}

/// `z` moved onto pad `note`, keeping its tuning relative to the pad.
fn retarget(z: &SampleZone, note: u8) -> SampleZone {
	let mut z = z.clone();
	let offset = i16::from(z.root_key) - i16::from(z.key_low);
	z.key_low = note;
	z.key_high = note;
	z.root_key = (i16::from(note) + offset).clamp(0, 127) as u8;
	z
}

/// Drags pad `from` onto pad `to`: swaps them (`copy`: `to` becomes a copy of `from`).
pub fn move_pad(project: &Project, r: DeviceRef, from: u8, to: u8, copy: bool) -> Option<Command> {
	let d = device(project, r)?;
	if from == to || (copy && pad_zones(d, from).is_empty()) {
		return None;
	}
	let on = |z: &SampleZone, n: u8| z.is_single_key() && z.key_low == n;
	let mut zones = Vec::with_capacity(d.zones.len() + 4);
	let mut copies = Vec::new();
	for z in &d.zones {
		if on(z, from) {
			if copy {
				zones.push(z.clone());
				copies.push(retarget(z, to));
			} else {
				zones.push(retarget(z, to));
			}
		} else if on(z, to) {
			if !copy {
				zones.push(retarget(z, from));
			}
		} else {
			zones.push(z.clone());
		}
	}
	zones.extend(copies);
	zones.sort_by_key(|z| z.key_low);
	let label = if copy {
		format!("Copy pad {} to {}", pad_label(from), pad_label(to))
	} else {
		format!("Swap pads {} and {}", pad_label(from), pad_label(to))
	};
	set_zones(project, r, zones, &label).map(|c| labeled(c, label))
}

/// Pad layers spread over the velocity range (`round_robin`: all over the whole range,
/// taking turns in group `group`).
fn spread(zones: &mut [SampleZone], layers: &[usize], round_robin: Option<u8>) {
	let n = layers.len();
	for (k, &i) in layers.iter().enumerate() {
		let z = &mut zones[i];
		match round_robin {
			Some(group) if n > 1 => {
				z.vel_low = 0.0;
				z.vel_high = 1.0;
				z.round_robin = group;
			}
			_ => {
				z.round_robin = 0;
				// A hair between ranges: MIDI velocities (m / 127) never fall in it.
				z.vel_low = if k == 0 { 0.0 } else { k as f32 / n as f32 };
				z.vel_high = if k + 1 == n {
					1.0
				} else {
					(k + 1) as f32 / n as f32 - 0.001
				};
			}
		}
	}
}

/// The lowest round-robin group no other pad uses (the processor counts turns per group).
fn free_group(device: &Device, note: u8) -> u8 {
	(1..=GROUPS)
		.find(|g| {
			!device
				.zones
				.iter()
				.any(|z| z.round_robin == *g && !(z.is_single_key() && z.key_low == note))
		})
		.unwrap_or(1)
}

/// The round-robin group pad `note` uses now, else a free one.
fn group_of(device: &Device, note: u8) -> u8 {
	pad_zones(device, note)
		.into_iter()
		.map(|i| device.zones[i].round_robin)
		.find(|g| *g > 0)
		.unwrap_or_else(|| free_group(device, note))
}

/// Pad `note`'s layers take turns (`on`) or split the velocity range evenly.
pub fn set_round_robin(project: &Project, r: DeviceRef, note: u8, on: bool) -> Option<Command> {
	let d = device(project, r)?;
	let layers = pad_layers(d, note);
	if layers.len() < 2 {
		return None;
	}
	let mut zones = d.zones.clone();
	spread(&mut zones, &layers, on.then(|| group_of(d, note)));
	let label = if on {
		format!("Round-robin {}", pad_label(note))
	} else {
		format!("Velocity layers {}", pad_label(note))
	};
	set_zones(project, r, zones, &label).map(|c| labeled(c, label))
}

/// Adds `path` as one more layer of pad `note` (its settings from the first layer), the
/// layers spread again the way they are now (round-robin or by velocity).
pub fn add_layer(project: &Project, r: DeviceRef, note: u8, path: &Path) -> Option<Command> {
	let d = device(project, r)?;
	let layers = pad_layers(d, note);
	let mut zone = match layers.first() {
		Some(&i) => {
			let mut z = d.zones[i].clone();
			z.sample = SampleRef::new(path);
			z.start = 0.0;
			z.end = None;
			z
		}
		None => SampleZone::pad(path, note),
	};
	zone.loop_mode = LoopMode::Off;
	let rr = is_round_robin(d, note).then(|| group_of(d, note));
	let mut zones = d.zones.clone();
	zones.push(zone);
	let mut all = layers;
	all.push(zones.len() - 1);
	spread(&mut zones, &all, rr);
	let label = format!("Add layer to {}", pad_label(note));
	set_zones(project, r, zones, &label).map(|c| labeled(c, label))
}

/// Removes layer `layer` (chip order) of pad `note`, spreading the others again.
pub fn remove_layer(project: &Project, r: DeviceRef, note: u8, layer: usize) -> Option<Command> {
	let d = device(project, r)?;
	let layers = pad_layers(d, note);
	let gone = *layers.get(layer)?;
	let rr = is_round_robin(d, note).then(|| group_of(d, note));
	let mut zones = d.zones.clone();
	let rest: Vec<usize> = layers
		.iter()
		.filter(|&&i| i != gone)
		.map(|&i| if i > gone { i - 1 } else { i })
		.collect();
	zones.remove(gone);
	spread(&mut zones, &rest, rr);
	let label = format!("Remove layer of {}", pad_label(note));
	set_zones(project, r, zones, &label).map(|c| labeled(c, label))
}

/// Edits layer `layer` (chip order) of pad `note` only.
pub fn edit_layer(
	project: &Project,
	r: DeviceRef,
	note: u8,
	layer: usize,
	f: impl FnOnce(&mut SampleZone),
) -> Option<Command> {
	let d = device(project, r)?;
	let index = *pad_layers(d, note).get(layer)?;
	super::samples::edit_zone(project, r, index, f)
}

// --- Kit from a folder ---------------------------------------------------------------

/// Which drum a file name names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KitSlot {
	Kick,
	Snare,
	Clap,
	Rim,
	ClosedHat,
	OpenHat,
	PedalHat,
	Crash,
	Ride,
	LowTom,
	MidTom,
	HighTom,
	/// A tom without a position: the first free of high, mid, low.
	Tom,
	Tambourine,
	Cowbell,
}

impl KitSlot {
	/// The pads it may go to, in order of preference.
	pub fn pads(self) -> &'static [u8] {
		match self {
			KitSlot::Kick => &[36, 35],
			KitSlot::Snare => &[38, 40],
			KitSlot::Clap => &[39],
			KitSlot::Rim => &[37],
			KitSlot::ClosedHat => &[42],
			KitSlot::OpenHat => &[46],
			KitSlot::PedalHat => &[44],
			KitSlot::Crash => &[49],
			KitSlot::Ride => &[51],
			KitSlot::LowTom => &[41, 45, 48],
			KitSlot::MidTom => &[45, 48, 41],
			KitSlot::HighTom | KitSlot::Tom => &[48, 45, 41],
			KitSlot::Tambourine => &[54],
			KitSlot::Cowbell => &[56],
		}
	}
}

/// Lower-case words of a name: split at anything not alphanumeric, at camel-case humps
/// and between letters and digits ("OpenHat02" -> open, hat, 02).
fn words(name: &str) -> Vec<String> {
	let mut out: Vec<String> = Vec::new();
	let mut cur = String::new();
	let mut prev: Option<char> = None;
	for c in name.chars() {
		if !c.is_alphanumeric() {
			if !cur.is_empty() {
				out.push(std::mem::take(&mut cur));
			}
			prev = None;
			continue;
		}
		let split = prev.is_some_and(|p| {
			(p.is_lowercase() && c.is_uppercase()) || (p.is_alphabetic() != c.is_alphabetic())
		});
		if split && !cur.is_empty() {
			out.push(std::mem::take(&mut cur));
		}
		cur.extend(c.to_lowercase());
		prev = Some(c);
	}
	if !cur.is_empty() {
		out.push(cur);
	}
	out
}

/// The drum a file (or folder) name names, by keywords: kick / bd, snare / sd, clap,
/// rim / side stick, closed hat / chh / hh, open hat / ohh, pedal, low / mid / hi /
/// floor tom (or tom 1-3), crash, ride, tambourine, cowbell.
pub fn kit_slot(name: &str) -> Option<KitSlot> {
	let w = words(name);
	let joined: String = w.concat();
	let has = |k: &str| w.iter().any(|x| x == k);
	let sub = |k: &str| joined.contains(k);
	if sub("cowbell") || has("cow") || has("cb") {
		return Some(KitSlot::Cowbell);
	}
	if sub("tamb") {
		return Some(KitSlot::Tambourine);
	}
	if sub("ride") || has("rd") {
		return Some(KitSlot::Ride);
	}
	if sub("crash") || sub("splash") || sub("cymbal") || has("cym") || has("cr") {
		return Some(KitSlot::Crash);
	}
	let hat = sub("hat") || has("hh") || sub("hihat");
	if (sub("open") && hat) || has("oh") || has("ohh") || has("ophat") {
		return Some(KitSlot::OpenHat);
	}
	if sub("pedal") || has("phh") || sub("foot") {
		return Some(KitSlot::PedalHat);
	}
	if hat || has("chh") || has("ch") || has("closed") {
		return Some(KitSlot::ClosedHat);
	}
	if sub("clap") || has("clp") || has("cp") {
		return Some(KitSlot::Clap);
	}
	if sub("rim") || sub("sidestick") || has("stick") {
		return Some(KitSlot::Rim);
	}
	if sub("snare") || has("sd") || has("snr") || has("sn") {
		return Some(KitSlot::Snare);
	}
	if sub("kick") || sub("bassdrum") || has("bd") || has("kik") || has("kck") {
		return Some(KitSlot::Kick);
	}
	tom_slot(&w)
}

fn tom_slot(w: &[String]) -> Option<KitSlot> {
	const LOW: [&str; 5] = ["low", "lo", "floor", "fl", "ft"];
	const MID: [&str; 3] = ["mid", "md", "mt"];
	const HIGH: [&str; 4] = ["high", "hi", "rack", "ht"];
	let tom =
		w.iter().position(|x| {
			x.starts_with("tom")
				|| x.strip_suffix("tom")
					.is_some_and(|p| LOW.contains(&p) || MID.contains(&p) || HIGH.contains(&p))
				|| x == "lt" || x == "mt"
				|| x == "ht" || x == "ft"
		})?;
	let is = |set: &[&str]| {
		w.iter().any(|x| {
			set.contains(&x.as_str()) || x.strip_suffix("tom").is_some_and(|p| set.contains(&p))
		})
	};
	if is(&LOW) || w[tom] == "lt" {
		return Some(KitSlot::LowTom);
	}
	if is(&MID) {
		return Some(KitSlot::MidTom);
	}
	if is(&HIGH) {
		return Some(KitSlot::HighTom);
	}
	// "Tom 1" is the highest, like a drummer counts them.
	match w.get(tom + 1).and_then(|n| n.parse::<u32>().ok()) {
		Some(1) => Some(KitSlot::HighTom),
		Some(2) => Some(KitSlot::MidTom),
		Some(_) => Some(KitSlot::LowTom),
		None => Some(KitSlot::Tom),
	}
}

/// What mapping a folder onto the pads does.
#[derive(Debug, Clone, PartialEq)]
pub struct KitMapping {
	/// One undo step "Map kit from folder" (None: nothing placed).
	pub command: Option<Command>,
	/// Files and the pads they went to.
	pub placed: Vec<(PathBuf, u8)>,
	/// Files whose name names no drum, or whose pads were all taken.
	pub unplaced: Vec<PathBuf>,
}

/// Maps `files` onto the pads of drum sampler `r` by their names (else the name of the
/// folder holding them, "Kicks/01.wav"); files are taken in name order and each pad gets
/// the first file naming it (a second kick goes to Kick 2). A loaded pad keeps its
/// settings with the new sample; pads no file names stay as they are.
pub fn map_kit(project: &Project, r: DeviceRef, files: &[PathBuf]) -> KitMapping {
	let mut files: Vec<&PathBuf> = files.iter().collect();
	files.sort_by_key(|p| p.to_string_lossy().to_lowercase());
	let mut placed: Vec<(PathBuf, u8)> = Vec::new();
	let mut unplaced = Vec::new();
	for path in files {
		let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned());
		let parent = path
			.parent()
			.and_then(Path::file_name)
			.map(|s| s.to_string_lossy().into_owned());
		let slot = stem
			.as_deref()
			.and_then(kit_slot)
			.or_else(|| parent.as_deref().and_then(kit_slot));
		let pad = slot.and_then(|s| {
			s.pads()
				.iter()
				.copied()
				.find(|n| !placed.iter().any(|(_, p)| p == n))
		});
		match pad {
			Some(n) => placed.push((path.clone(), n)),
			None => unplaced.push(path.clone()),
		}
	}
	let command = device(project, r)
		.filter(|_| !placed.is_empty())
		.and_then(|d| {
			let mut zones = d.zones.clone();
			for (path, note) in &placed {
				let template = pad_layers(d, *note).first().map(|&i| d.zones[i].clone());
				let zone = match template {
					Some(mut z) => {
						z.sample = SampleRef::new(path);
						z.start = 0.0;
						z.end = None;
						z.loop_mode = LoopMode::Off;
						z.vel_low = 0.0;
						z.vel_high = 1.0;
						z.round_robin = 0;
						z
					}
					None => {
						let mut z = SampleZone::pad(path, *note);
						// Hats cut each other off like on a real kit.
						if matches!(note, 42 | 44 | 46) {
							z.choke = 1;
						}
						z
					}
				};
				zones.retain(|z| !(z.is_single_key() && z.key_low == *note));
				zones.push(zone);
			}
			zones.sort_by_key(|z| z.key_low);
			let label = "Map kit from folder".to_owned();
			set_zones(project, r, zones, &label).map(|c| labeled(c, label))
		});
	KitMapping {
		command,
		placed,
		unplaced,
	}
}

/// The audio files of `dir` and of its folders up to two levels down (kits often keep
/// "Kicks/", "Snares/"...), hidden entries skipped.
pub fn kit_files(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
	fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
		for e in std::fs::read_dir(dir)?.flatten() {
			let path = e.path();
			if e.file_name().to_string_lossy().starts_with('.') {
				continue;
			}
			if path.is_dir() {
				if depth < 2 {
					walk(&path, depth + 1, out)?;
				}
			} else if classify(&path) == Some(FileKind::Audio) {
				out.push(path);
			}
		}
		Ok(())
	}
	let mut out = Vec::new();
	walk(dir, 0, &mut out)?;
	out.sort();
	Ok(out)
}

/// The notice after a mapping: what went where, and the files left out.
pub fn mapping_notice(m: &KitMapping) -> String {
	let mut text = trn(
		"Mapped {n} file onto pads",
		"Mapped {n} files onto pads",
		m.placed.len(),
	);
	if !m.unplaced.is_empty() {
		let names: Vec<String> = m
			.unplaced
			.iter()
			.take(6)
			.map(|p| {
				p.file_name()
					.unwrap_or_default()
					.to_string_lossy()
					.into_owned()
			})
			.collect();
		let more = m.unplaced.len().saturating_sub(names.len());
		let mut names = names.join(", ");
		if more > 0 {
			names.push_str(&trf(" and {n} more", &[("n", &more)]));
		}
		text.push_str(&trf(
			"; could not place {files} (rename them after the drum, e.g. \"snare_02\", or drop \
			 them on a pad)",
			&[("files", &names)],
		));
	}
	text
}

// --- Envelope ------------------------------------------------------------------------

/// The pad's amplitude `t_ms` after the hit, as the processor plays it: a linear attack,
/// then an exponential decay reaching -60 dB `decay_ms` later (where the voice ends);
/// no decay = full level to the end of the sample.
pub fn envelope(attack_ms: f32, decay_ms: Option<f32>, t_ms: f32) -> f32 {
	if t_ms < 0.0 {
		return 0.0;
	}
	if attack_ms >= 0.5 && t_ms < attack_ms {
		return t_ms / attack_ms;
	}
	match decay_ms {
		Some(d) => {
			let d = d.max(1.0);
			let x = t_ms - attack_ms.max(0.0);
			if x >= d {
				0.0
			} else {
				(-LN_1000 * x / d).exp()
			}
		}
		None => 1.0,
	}
}

/// How fast pad samples play (tune and kit tune in semitones): sample seconds per second.
/// How much a pad glows from what the processor plays: half while its key is held,
/// else the loudest of its sounding hits at half strength (the trigger flash adds the
/// full-strength blink).
pub fn live_glow(live: &super::live_voices::LiveVoices, note: u8) -> f32 {
	let sounding = live
		.voices
		.iter()
		.filter(|v| v.note == note)
		.map(|v| v.level * 0.5)
		.fold(0.0, f32::max);
	if live.held.is_held(note) {
		sounding.max(0.5)
	} else {
		sounding
	}
}

pub fn playback_rate(tune: f32, kit_tune: f32) -> f64 {
	2.0_f64.powf(f64::from(tune + kit_tune) / 12.0)
}

/// Milliseconds of decay for the decay node dragged `node_ms` after the hit.
pub fn decay_from_node(attack_ms: f32, node_ms: f32) -> f32 {
	((node_ms - attack_ms) / DECAY_NODE).clamp(1.0, 4000.0)
}

/// "4 ms", "412 ms", "1.20 s".
pub fn ms_label(seconds: f64) -> String {
	let ms = seconds * 1000.0;
	if ms >= 1000.0 {
		format!("{:.2} s", seconds)
	} else if ms >= 10.0 || ms.fract().abs() < 0.05 {
		format!("{ms:.0} ms")
	} else {
		format!("{ms:.1} ms")
	}
}

// --- Waveform window -----------------------------------------------------------------

/// The stretch of the sample the waveform shows (seconds).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WaveWindow {
	pub from: f64,
	pub to: f64,
}

impl WaveWindow {
	pub fn full(length: f64) -> Self {
		Self {
			from: 0.0,
			to: length.max(MIN_SPAN),
		}
	}

	pub fn span(&self) -> f64 {
		self.to - self.from
	}

	/// Kept inside a sample of `length` seconds.
	pub fn clamped(self, length: f64) -> Self {
		let length = length.max(MIN_SPAN);
		let span = self.span().clamp(MIN_SPAN.min(length), length);
		let from = self.from.clamp(0.0, length - span);
		Self {
			from,
			to: from + span,
		}
	}

	/// Zoomed by `factor` (< 1 = in) around `at` seconds, which stays under the pointer.
	pub fn zoom(self, at: f64, factor: f64, length: f64) -> Self {
		let span = (self.span() * factor).clamp(MIN_SPAN, length.max(MIN_SPAN));
		let k = ((at - self.from) / self.span()).clamp(0.0, 1.0);
		let from = at - k * span;
		Self {
			from,
			to: from + span,
		}
		.clamped(length)
	}

	/// Moved by `seconds`.
	pub fn scroll(self, seconds: f64, length: f64) -> Self {
		Self {
			from: self.from + seconds,
			to: self.to + seconds,
		}
		.clamped(length)
	}

	pub fn is_full(&self, length: f64) -> bool {
		self.from <= 1e-9 && self.to >= length - 1e-9
	}
}

// --- Flashes -------------------------------------------------------------------------

/// The pads' flashes from the processor's trigger counters (one per pad of [`kit_pads`]):
/// a counter that grew since the last frame flashes its pad; one that went down (a
/// fresh instance after a reload) rebases without flashing.
#[derive(Debug, Clone, Default)]
pub struct PadFlashes {
	last: Option<[f32; readings::DRUM_PADS]>,
	at: [Option<f64>; readings::DRUM_PADS],
}

impl PadFlashes {
	pub fn update(&mut self, counters: &[f32], now: f64) {
		let start = readings::DRUM_PAD_TRIGGERS_START;
		let Some(values) = counters.get(start..start + readings::DRUM_PADS) else {
			return;
		};
		let mut current = [0.0; readings::DRUM_PADS];
		current.copy_from_slice(values);
		if let Some(last) = self.last {
			for i in 0..readings::DRUM_PADS {
				if current[i] > last[i] {
					self.at[i] = Some(now);
				}
			}
		}
		self.last = Some(current);
	}

	/// How lit pad `i` (index into [`kit_pads`]) is at `now`.
	pub fn level(&self, i: usize, now: f64) -> f32 {
		self.at
			.get(i)
			.copied()
			.flatten()
			.map_or(0.0, |t| flash_level(now - t))
	}

	pub fn any_lit(&self, now: f64) -> bool {
		(0..PADS.len()).any(|i| self.level(i, now) > 0.0)
	}
}

/// The pad (index into [`kit_pads`]) a computer key auditions: 1-8 the lower two rows, Shift
/// + 1-8 the upper two.
pub fn key_pad(digit: u8, shift: bool) -> Option<usize> {
	(1..=8)
		.contains(&digit)
		.then(|| usize::from(digit - 1) + if shift { 8 } else { 0 })
}

// --- App glue ------------------------------------------------------------------------

impl super::AppState {
	/// Maps the audio files of `dir` onto drum sampler `r`'s pads by name (one undo step
	/// "Map kit from folder"); the notice names files it could not place.
	pub fn map_kit_folder(&mut self, r: DeviceRef, dir: &Path) -> bool {
		let files = match kit_files(dir) {
			Ok(f) => f,
			Err(e) => {
				self.notices.error(trf(
					"Cannot read the folder {dir}: {e}",
					&[("dir", &dir.display()), ("e", &e)],
				));
				return false;
			}
		};
		if files.is_empty() {
			self.notices.warn(trf(
				"{dir} holds no audio files to map onto the pads",
				&[("dir", &dir.display())],
			));
			return false;
		}
		let m = map_kit(self.project(), r, &files);
		let Some(cmd) = m.command.clone() else {
			self.notices.warn(mapping_notice(&m));
			return false;
		};
		if !self.apply(cmd) {
			return false;
		}
		if let Some((_, first)) = m.placed.first() {
			self.view.instrument.pad = *first;
			self.view.instrument.layer = 0;
		}
		if m.unplaced.is_empty() {
			self.notices.info(mapping_notice(&m));
		} else {
			self.notices.warn(mapping_notice(&m));
		}
		true
	}

	/// Adds `path` as another layer of pad `note` (one undo step).
	pub fn add_pad_layer(&mut self, r: DeviceRef, note: u8, path: &Path) -> bool {
		let cmd = add_layer(self.project(), r, note, path);
		let ok = self.apply_sample_edit(cmd);
		if ok && let Some(n) = self.device(r).map(|d| pad_layers(d, note).len()) {
			self.view.instrument.pad = note;
			self.view.instrument.layer = n.saturating_sub(1);
		}
		ok
	}

	/// A file dialog for pad `note`: its sample (`layer`: one more layer).
	pub fn pick_pad_sample(&mut self, r: DeviceRef, note: u8, layer: bool) {
		self.view.instrument.dialog = Some((r, note));
		self.file_dialog(if layer {
			crate::jobs::FileAction::PadLayer
		} else {
			crate::jobs::FileAction::PadSample
		});
	}

	/// A folder dialog: the kit to map onto `r`'s pads.
	pub fn pick_kit_folder(&mut self, r: DeviceRef) {
		self.view.instrument.dialog = Some((r, 0));
		self.file_dialog(crate::jobs::FileAction::KitFolder);
	}

	/// What a pad / kit dialog picked.
	pub(crate) fn finish_sample_dialog(&mut self, action: crate::jobs::FileAction, path: &Path) {
		use crate::jobs::FileAction;
		let Some((r, note)) = self.view.instrument.dialog.take() else {
			return;
		};
		match action {
			FileAction::PadSample => {
				self.load_sample(r, path, super::samples::SampleSlot::Pad(note));
			}
			FileAction::PadLayer => {
				self.add_pad_layer(r, note, path);
			}
			FileAction::KitFolder => {
				self.map_kit_folder(r, path);
			}
			_ => {}
		}
	}

	/// Shows `path` in the browser's Files tab (its folder opened).
	pub fn locate_file(&mut self, path: &Path) {
		if !path.exists() {
			self.notices.warn(trf(
				"{path} is missing; File → Locate Missing Files can find it",
				&[("path", &path.display())],
			));
		}
		if let Some(dir) = path.parent() {
			self.files.open(dir.to_path_buf());
		}
		self.view.instrument.files_tab = true;
		self.view
			.panels
			.set(super::layout::PanelKind::Browser, true);
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn pads_glow_while_held_or_sounding() {
		use cadenza_dsp::instruments::voice_readings::VoiceReading;
		let mut live = super::super::live_voices::LiveVoices::default();
		assert_eq!(live_glow(&live, 38), 0.0);
		live.voices.push(VoiceReading {
			note: 38,
			level: 0.8,
			..Default::default()
		});
		assert!((live_glow(&live, 38) - 0.4).abs() < 1e-6);
		assert_eq!(live_glow(&live, 36), 0.0);
		live.held.press(36);
		assert_eq!(live_glow(&live, 36), 0.5);
	}
	use cadenza_model::{Document, OutputRoute, Track, TrackKind};

	fn fixture() -> (Project, DeviceRef) {
		let mut p = Project::new("s");
		let mut snare = SampleZone::pad("/k/Snare.wav", 38);
		snare.gain_db = -3.0;
		snare.choke = 2;
		let mut kick = SampleZone::pad("/k/Kick.wav", 36);
		kick.tune = 2.0;
		let d = Device::builtin("drum_sampler", "Drums").with_zones(vec![
			kick,
			snare,
			SampleZone::pad("/k/Snare soft.wav", 38),
		]);
		let t = Track::new("Drums", TrackKind::Instrument).with_instrument(d);
		let r = DeviceRef {
			track: Some(t.id),
			device: t.instrument.as_ref().unwrap().id,
		};
		p.tracks.push(t);
		(p, r)
	}

	fn run(p: &Project, cmd: Command) -> (Project, String) {
		let mut doc = Document::new(p.clone());
		doc.execute(cmd).unwrap();
		assert_eq!(doc.history_labels().len(), 1, "one undo step");
		let label = doc.history_labels()[0].to_string();
		(doc.project().clone(), label)
	}

	fn zones_of(p: &Project, r: DeviceRef, note: u8) -> Vec<SampleZone> {
		let d = device(p, r).unwrap();
		pad_layers(d, note)
			.into_iter()
			.map(|i| d.zones[i].clone())
			.collect()
	}

	#[test]
	fn pads_have_colours_labels_and_tags() {
		assert_eq!(note_label(36), "C1");
		assert_eq!(note_label(39), "D#1");
		assert_eq!(note_label(49), "C#2");
		assert_eq!(pad_color(36), 0);
		assert_eq!(pad_color(39), 1);
		assert_eq!(pad_color(46), 3);
		assert_eq!(pad_color(45), 4);
		assert_eq!(pad_color(51), 6);
		assert_eq!(pad_color(56), 7);
		assert!(
			PADS.iter()
				.all(|(n, _)| pad_short(*n) != "Pad" && pad_short(*n).len() <= 6)
		);
		// Pad 1 bottom-left, pad 16 top-right.
		assert_eq!(grid_cell(0), (3, 0));
		assert_eq!(grid_cell(5), (2, 1));
		assert_eq!(grid_cell(15), (0, 3));
		let (p, r) = fixture();
		let d = device(&p, r).unwrap();
		let tags = pad_tags(d, 38);
		assert_eq!(tags.layers, 1, "both layers cover every velocity");
		assert_eq!(tags.labels(), vec!["C2".to_owned()]);
		assert!(pad_tags(d, 40).labels().is_empty());
		// Split by velocity: L2; round-robin: RR2; an extra output: →3.
		let (p, _) = run(&p, set_round_robin(&p, r, 38, false).unwrap());
		let d = device(&p, r).unwrap();
		assert_eq!(pad_tags(d, 38).labels(), vec!["C2", "L2"]);
		let (p, _) = run(&p, set_round_robin(&p, r, 38, true).unwrap());
		let d = device(&p, r).unwrap();
		assert_eq!(pad_tags(d, 38).labels(), vec!["C2", "RR2"]);
		let cmd = super::super::samples::edit_pad(&p, r, 38, |z| z.output = 3).unwrap();
		let (p, _) = run(&p, cmd);
		assert_eq!(
			pad_tags(device(&p, r).unwrap(), 38).labels()[1],
			"\u{2192}3"
		);
	}

	#[test]
	fn dragging_a_pad_swaps_or_copies_it_in_one_step() {
		let (p, r) = fixture();
		let (after, label) = run(&p, move_pad(&p, r, 38, 36, false).unwrap());
		assert_eq!(label, "Swap pads Snare and Kick");
		let kick = zones_of(&after, r, 36);
		let snare = zones_of(&after, r, 38);
		assert_eq!(kick.len(), 2, "the snare's layers moved");
		assert!(
			kick.iter()
				.all(|z| z.sample.stem().starts_with("Snare") && z.root_key == 36)
		);
		assert_eq!(kick[0].choke, 2, "settings travel with the sample");
		assert_eq!(snare.len(), 1);
		assert_eq!(snare[0].sample.stem(), "Kick");
		assert_eq!((snare[0].tune, snare[0].root_key), (2.0, 38));
		// Onto an empty pad: a move; Alt: a copy.
		let (after, _) = run(&p, move_pad(&p, r, 36, 41, false).unwrap());
		assert!(zones_of(&after, r, 36).is_empty());
		assert_eq!(zones_of(&after, r, 41)[0].sample.stem(), "Kick");
		let (after, label) = run(&p, move_pad(&p, r, 38, 36, true).unwrap());
		assert_eq!(label, "Copy pad Snare to Kick");
		assert_eq!(zones_of(&after, r, 38).len(), 2);
		assert_eq!(zones_of(&after, r, 36).len(), 2);
		assert!(
			zones_of(&after, r, 36)
				.iter()
				.all(|z| z.sample.stem().starts_with("Snare"))
		);
		// Nothing to do.
		assert!(move_pad(&p, r, 38, 38, false).is_none());
		assert!(
			move_pad(&p, r, 40, 36, true).is_none(),
			"copying an empty pad"
		);
		// A pad on an extra output keeps its route when it moves.
		let cmd = super::super::samples::edit_pad(&p, r, 38, |z| z.output = 2).unwrap();
		let (p, _) = run(&p, cmd);
		let (after, _) = run(&p, move_pad(&p, r, 38, 45, false).unwrap());
		assert_eq!(
			device(&after, r).unwrap().aux_outputs,
			vec![OutputRoute { port: 2, bus: None }]
		);
	}

	#[test]
	fn layers_split_velocity_or_take_turns() {
		let (p, r) = fixture();
		let (p, label) = run(&p, set_round_robin(&p, r, 38, false).unwrap());
		assert_eq!(label, "Velocity layers Snare");
		let z = zones_of(&p, r, 38);
		assert_eq!((z[0].vel_low, z[1].vel_high), (0.0, 1.0));
		assert!(z[0].vel_high < z[1].vel_low && z[1].vel_low - z[0].vel_high < 0.002);
		let (p, _) = run(
			&p,
			add_layer(&p, r, 38, Path::new("/k/Snare hard.wav")).unwrap(),
		);
		let z = zones_of(&p, r, 38);
		assert_eq!(z.len(), 3);
		assert_eq!(z[2].sample.stem(), "Snare hard");
		assert_eq!(z[2].choke, 2, "a new layer takes the pad's settings");
		assert!((z[1].vel_low - 1.0 / 3.0).abs() < 1e-6);
		let (p, _) = run(&p, set_round_robin(&p, r, 38, true).unwrap());
		let z = zones_of(&p, r, 38);
		assert!(z.iter().all(|z| z.round_robin == 1 && z.vel_low == 0.0));
		let (p, _) = run(&p, remove_layer(&p, r, 38, 0).unwrap());
		let z = zones_of(&p, r, 38);
		assert_eq!(z.len(), 2);
		assert!(z.iter().all(|z| z.round_robin == 1), "still round-robin");
		// Only the chosen layer changes.
		let (p, _) = run(&p, edit_layer(&p, r, 38, 1, |z| z.start = 0.01).unwrap());
		let z = zones_of(&p, r, 38);
		assert_eq!((z[0].start, z[1].start), (0.0, 0.01));
		// A single sample has nothing to split.
		assert!(set_round_robin(&p, r, 36, true).is_none());
		// Another pad's round-robin picks its own group.
		let (p, _) = run(
			&p,
			add_layer(&p, r, 36, Path::new("/k/Kick 2.wav")).unwrap(),
		);
		let (p, _) = run(&p, set_round_robin(&p, r, 36, true).unwrap());
		assert!(zones_of(&p, r, 36).iter().all(|z| z.round_robin == 2));
	}

	#[test]
	fn file_names_name_their_drums() {
		let cases = [
			("Kick 01", Some(KitSlot::Kick)),
			("BD_909", Some(KitSlot::Kick)),
			("snare_brush_04", Some(KitSlot::Snare)),
			("SD-tight", Some(KitSlot::Snare)),
			("Clap Room", Some(KitSlot::Clap)),
			("side stick", Some(KitSlot::Rim)),
			("Rimshot", Some(KitSlot::Rim)),
			("hat_cl_03", Some(KitSlot::ClosedHat)),
			("CHH", Some(KitSlot::ClosedHat)),
			("HH 2", Some(KitSlot::ClosedHat)),
			("OpenHat02", Some(KitSlot::OpenHat)),
			("hat_open_01", Some(KitSlot::OpenHat)),
			("OHH", Some(KitSlot::OpenHat)),
			("hat_pedal", Some(KitSlot::PedalHat)),
			("Floor Tom", Some(KitSlot::LowTom)),
			("tom_lo", Some(KitSlot::LowTom)),
			("Mid Tom", Some(KitSlot::MidTom)),
			("HiTom", Some(KitSlot::HighTom)),
			("Tom 2", Some(KitSlot::MidTom)),
			("tom", Some(KitSlot::Tom)),
			("crash_16", Some(KitSlot::Crash)),
			("Ride Bell", Some(KitSlot::Ride)),
			("Tambourine", Some(KitSlot::Tambourine)),
			("Cowbell", Some(KitSlot::Cowbell)),
			("custom_fx", None),
			("bottom", None),
			("vocal chop", None),
		];
		for (name, slot) in cases {
			assert_eq!(kit_slot(name), slot, "{name}");
		}
	}

	#[test]
	fn a_folder_maps_onto_pads_by_name_in_one_step() {
		let (p, r) = fixture();
		let files: Vec<PathBuf> = [
			"/kit/snare_brush_04.wav",
			"/kit/Kick B.wav",
			"/kit/Kick A.wav",
			"/kit/hat_cl_03.wav",
			"/kit/hat_open_01.wav",
			"/kit/Toms/01.wav",
			"/kit/Kick C.wav",
			"/kit/vocal chop.wav",
		]
		.iter()
		.map(PathBuf::from)
		.collect();
		let m = map_kit(&p, r, &files);
		let pad = |name: &str| {
			m.placed
				.iter()
				.find(|(f, _)| f.file_name().unwrap().to_string_lossy() == name)
				.map(|(_, n)| *n)
		};
		assert_eq!(pad("Kick A.wav"), Some(36), "name order: A first");
		assert_eq!(pad("Kick B.wav"), Some(35), "the second kick: Kick 2");
		assert_eq!(pad("snare_brush_04.wav"), Some(38));
		assert_eq!(pad("hat_cl_03.wav"), Some(42));
		assert_eq!(pad("hat_open_01.wav"), Some(46));
		assert_eq!(pad("01.wav"), Some(48), "by its folder's name");
		let unplaced: Vec<String> = m
			.unplaced
			.iter()
			.map(|f| f.file_name().unwrap().to_string_lossy().into_owned())
			.collect();
		assert_eq!(unplaced, vec!["Kick C.wav", "vocal chop.wav"]);
		let notice = mapping_notice(&m);
		assert!(notice.starts_with("Mapped 6 files onto pads; could not place Kick C.wav"));
		let (after, label) = run(&p, m.command.unwrap());
		assert_eq!(label, "Map kit from folder");
		let snare = zones_of(&after, r, 38);
		assert_eq!(snare.len(), 1, "one sample replaces the layers");
		assert_eq!(snare[0].sample.stem(), "snare_brush_04");
		assert_eq!(
			(snare[0].gain_db, snare[0].choke),
			(-3.0, 2),
			"keeps the pad's settings"
		);
		assert_eq!(
			zones_of(&after, r, 42)[0].choke,
			1,
			"new hats choke each other"
		);
		assert_eq!(zones_of(&after, r, 35)[0].root_key, 35);
		// Nothing placeable: no command.
		let m = map_kit(&p, r, &[PathBuf::from("/x/pad.wav")]);
		assert!(m.command.is_none() && m.unplaced.len() == 1);
	}

	#[test]
	fn kit_files_lists_audio_two_levels_down() {
		let dir = tempfile::tempdir().unwrap();
		let root = dir.path();
		for f in [
			"Kick.wav",
			"notes.txt",
			".hidden.wav",
			"Snares/Snare 1.flac",
			"a/b/c/deep.wav",
		] {
			let p = root.join(f);
			std::fs::create_dir_all(p.parent().unwrap()).unwrap();
			std::fs::write(p, b"").unwrap();
		}
		let names: Vec<String> = kit_files(root)
			.unwrap()
			.iter()
			.map(|p| {
				p.strip_prefix(root)
					.unwrap()
					.to_string_lossy()
					.replace('\\', "/")
			})
			.collect();
		assert_eq!(names, vec!["Kick.wav", "Snares/Snare 1.flac"]);
	}

	#[test]
	fn the_envelope_matches_the_processor() {
		assert_eq!(envelope(0.0, None, 100.0), 1.0);
		assert_eq!(envelope(10.0, None, 5.0), 0.5);
		assert!(
			(envelope(0.0, Some(300.0), 100.0) - 0.1).abs() < 1e-3,
			"-20 dB at a third"
		);
		assert_eq!(
			envelope(0.0, Some(300.0), 300.0),
			0.0,
			"the voice ends at -60 dB"
		);
		assert!((envelope(20.0, Some(300.0), 120.0) - 0.1).abs() < 1e-3);
		assert!((decay_from_node(20.0, 120.0) - 300.0).abs() < 1e-3);
		assert!((playback_rate(12.0, 0.0) - 2.0).abs() < 1e-9);
		assert_eq!(ms_label(0.004), "4 ms");
		assert_eq!(ms_label(0.412), "412 ms");
		assert_eq!(ms_label(0.0025), "2.5 ms");
		assert_eq!(ms_label(1.2), "1.20 s");
	}

	#[test]
	fn the_waveform_zooms_around_the_pointer_and_scrolls_inside_the_sample() {
		let w = WaveWindow::full(2.0);
		assert!(w.is_full(2.0));
		let z = w.zoom(0.5, 0.5, 2.0);
		assert!((z.span() - 1.0).abs() < 1e-9);
		assert!((z.from - 0.25).abs() < 1e-9, "0.5 s stays a quarter in");
		let s = z.scroll(5.0, 2.0);
		assert_eq!((s.from, s.to), (1.0, 2.0));
		let s = z.scroll(-5.0, 2.0);
		assert_eq!(s.from, 0.0);
		assert!(w.zoom(1.0, 1e-6, 2.0).span() >= 0.005 - 1e-12);
		assert!(z.zoom(1.0, 100.0, 2.0).is_full(2.0));
	}

	#[test]
	fn pads_flash_when_their_counter_grows() {
		let mut f = PadFlashes::default();
		let mut c = [0.0f32; readings::MAX];
		c[0] = 3.0;
		f.update(&c, 1.0);
		assert!(!f.any_lit(1.0), "the first reading is the baseline");
		c[1] = 1.0;
		f.update(&c, 2.0);
		assert_eq!(f.level(1, 2.0), 1.0);
		assert_eq!(f.level(0, 2.0), 0.0);
		assert_eq!(f.level(1, 2.5), 0.0);
		c[1] = 0.0;
		f.update(&c, 3.0);
		assert!(!f.any_lit(3.0), "a fresh instance rebases");
		f.update(&c[..4], 3.0);
		assert_eq!(key_pad(1, false), Some(0));
		assert_eq!(key_pad(8, true), Some(15));
		assert_eq!(key_pad(9, false), None);
	}
}
