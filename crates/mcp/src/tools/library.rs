//! The style library as AI tools: search it, read style guides/forms/patterns, and apply
//! patterns and forms to the project. Applying builds ordinary commands (one `AddClip`, or
//! one `Batch` of markers/tempo/style), so each call is one undo step and playback picks
//! the result up like any other edit.

use cadenza_core::{ClipId, Tick, TrackId};
use cadenza_library::{
	Harmony, Item, ItemKind, Library, Pattern, Query, RealizeOptions, Role, Variant, card,
	parse_chords, realize,
};
use cadenza_model::{Clip, Command, Marker, Note, Project, ProjectMeta};
use cadenza_theory::{Chord, Scale, ScaleKind};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::convert::{beats_to_tick, tick_to_beats};
use crate::error::McpError;
use crate::session::Session;
use crate::tools::compose::drum_lane_warnings;

/// Default and largest number of search results.
pub const DEFAULT_LIMIT: usize = 8;
pub const MAX_LIMIT: usize = 25;
/// Largest number of ids per `library_get`.
pub const MAX_GET: usize = 5;
/// Longest clip `apply_pattern` writes.
pub const MAX_BARS: u32 = 256;

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum KindArg {
	Style,
	Form,
	Pattern,
	Progression,
	Groove,
}

impl From<KindArg> for ItemKind {
	fn from(k: KindArg) -> Self {
		match k {
			KindArg::Style => ItemKind::Style,
			KindArg::Form => ItemKind::Form,
			KindArg::Pattern => ItemKind::Pattern,
			KindArg::Progression => ItemKind::Progression,
			KindArg::Groove => ItemKind::Groove,
		}
	}
}

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RoleArg {
	Drums,
	Perc,
	Bass,
	Comping,
	Arp,
	Pad,
	#[serde(alias = "hook")]
	Lead,
	Fill,
}

impl From<RoleArg> for Role {
	fn from(r: RoleArg) -> Self {
		match r {
			RoleArg::Drums => Role::Drums,
			RoleArg::Perc => Role::Perc,
			RoleArg::Bass => Role::Bass,
			RoleArg::Comping => Role::Comping,
			RoleArg::Arp => Role::Arp,
			RoleArg::Pad => Role::Pad,
			RoleArg::Lead => Role::Lead,
			RoleArg::Fill => Role::Fill,
		}
	}
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct LibrarySearchArgs {
	/// Words matched against ids, names, aliases, genres, tags and summaries ("latin
	/// pop", "dembow", "tumbao bass").
	#[serde(default)]
	pub query: Option<String>,
	#[serde(default)]
	pub kind: Option<KindArg>,
	/// Genre slug ("reggaeton", "house", "lofi").
	#[serde(default)]
	pub genre: Option<String>,
	/// Pattern role.
	#[serde(default)]
	pub role: Option<RoleArg>,
	/// Song section a pattern suits ("verse", "chorus", "Pre-chorus", "drop").
	#[serde(default)]
	pub section: Option<String>,
	/// Pattern energy 1..=5 (matches +/-1, exact first).
	#[serde(default)]
	pub energy: Option<u8>,
	/// Pattern family ("dembow", "house"): its main/variation/fill members.
	#[serde(default)]
	pub family: Option<String>,
	/// Default 8, at most 25.
	#[serde(default)]
	pub limit: Option<usize>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct LibraryCard {
	pub id: String,
	pub kind: String,
	pub summary: String,
	pub facts: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct LibrarySearchOutput {
	pub cards: Vec<LibraryCard>,
	/// Matches in total (more than `cards` when the limit cut them).
	pub total: usize,
}

pub fn library_search(
	session: &mut Session,
	args: LibrarySearchArgs,
) -> Result<LibrarySearchOutput, McpError> {
	if let Some(e) = args.energy
		&& !(1..=5).contains(&e)
	{
		return Err(McpError::Invalid(format!("energy {e} must be 1..=5")));
	}
	let limit = args.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
	let library = session.library();
	let found = library.search(&Query {
		text: args.query.unwrap_or_default(),
		kind: args.kind.map(Into::into),
		genre: args.genre,
		role: args.role.map(Into::into),
		section: args.section,
		energy: args.energy,
		family: args.family,
	});
	Ok(LibrarySearchOutput {
		total: found.len(),
		cards: found
			.into_iter()
			.take(limit)
			.map(|it| {
				let c = card(it);
				LibraryCard {
					id: c.id,
					kind: c.kind.as_str().to_owned(),
					summary: c.summary,
					facts: c.facts,
				}
			})
			.collect(),
	})
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct LibraryGetArgs {
	/// 1 to 5 ids from library_search ("core/latin_pop", "core/dembow_basic").
	pub ids: Vec<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct LibraryGetOutput {
	/// Each item as stored (patterns in step-grid notation) plus its pack's `license`.
	pub items: Vec<serde_json::Value>,
}

/// "unknown id 'x'" plus the closest ids, for an LLM that guessed an id.
fn unknown(library: &Library, id: &str) -> McpError {
	let guesses: Vec<String> = library
		.search(&Query {
			text: id.replace(['/', '_', '-'], " "),
			..Default::default()
		})
		.into_iter()
		.take(3)
		.map(|i| i.id.clone())
		.collect();
	let hint = if guesses.is_empty() {
		"find ids with library_search".to_owned()
	} else {
		format!("closest: {}", guesses.join(", "))
	};
	McpError::Invalid(format!("unknown library id '{id}'; {hint}"))
}

pub fn library_get(
	session: &mut Session,
	args: LibraryGetArgs,
) -> Result<LibraryGetOutput, McpError> {
	if args.ids.is_empty() || args.ids.len() > MAX_GET {
		return Err(McpError::Invalid(format!(
			"pass 1 to {MAX_GET} ids (got {}); fetch more in another call",
			args.ids.len()
		)));
	}
	let library = session.library();
	let mut items = Vec::with_capacity(args.ids.len());
	for id in &args.ids {
		let it = library.item(id).ok_or_else(|| unknown(&library, id))?;
		let mut value = serde_json::to_value(&it.item)
			.map_err(|e| McpError::Invalid(format!("item '{id}' cannot be read: {e}")))?;
		if let (serde_json::Value::Object(map), Some(pack)) = (&mut value, library.pack(&it.pack)) {
			map.insert("license".into(), pack.manifest.license.clone().into());
		}
		items.push(value);
	}
	Ok(LibraryGetOutput { items })
}

#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
pub struct ApplyPatternArgs {
	/// Pattern id ("core/dembow_basic").
	pub pattern: String,
	/// Instrument track that gets the new clip.
	pub track_id: TrackId,
	pub start_beat: f64,
	/// Bars to write (default: the pattern's own length); the pattern repeats.
	#[serde(default)]
	pub bars: Option<u32>,
	/// Key, e.g. "A minor": needed by scale-degree patterns (leads, hooks), roman
	/// numerals and progression ids; improves extensions of chord patterns.
	#[serde(default)]
	pub key: Option<String>,
	/// Chords cycling from start_beat: symbols ("Am F C G"), roman numerals with `key`
	/// ("i VI III VII") or a progression id ("core/prog_i_VI_III_VII", with `key`).
	/// Needed by bass, comping, arp and pad patterns.
	#[serde(default)]
	pub chords: Option<String>,
	/// Beats each chord lasts (default: the progression's, else 4 = one 4/4 bar).
	#[serde(default)]
	pub beats_per_chord: Option<f64>,
	/// The last bar of every n bars plays the pattern family's fill (e.g. 4 or 8).
	#[serde(default)]
	pub fill_every: Option<u32>,
	/// Alternate the main pattern and its family variation every pattern cycle.
	#[serde(default)]
	pub vary: Option<bool>,
	/// Groove id ("core/groove_swing_16_57") replacing the pattern's swing feel, or
	/// "none" to drop the pattern's default groove.
	#[serde(default)]
	pub groove: Option<String>,
	/// Velocity multiplier, 0.1..=1.5 (default 1).
	#[serde(default)]
	pub velocity: Option<f32>,
	/// Octave shift, -3..=3 (default 0).
	#[serde(default)]
	pub octave: Option<i32>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ApplyPatternOutput {
	pub clip_id: ClipId,
	pub length_beats: f64,
	/// One line: what was written, where and over which chords.
	pub summary: String,
	#[serde(skip_serializing_if = "Vec::is_empty")]
	pub warnings: Vec<String>,
}

/// The clip a pattern becomes, before it is added.
#[derive(Debug, Clone)]
pub struct PatternClip {
	pub clip: Clip,
	pub summary: String,
	pub warnings: Vec<String>,
}

fn library_error(e: impl std::fmt::Display) -> McpError {
	McpError::Invalid(e.to_string())
}

/// A short chord name ("Am", "F", "G7", "Cmaj7") for summaries.
pub fn chord_name(chord: &Chord) -> String {
	let root = chord.root.name(matches!(chord.root.0, 1 | 3 | 6 | 8 | 10));
	let mut ivs: Vec<u8> = chord.intervals.iter().map(|i| i % 12).collect();
	ivs.sort_unstable();
	ivs.dedup();
	let quality = match ivs.as_slice() {
		[0, 4, 7] => "",
		[0, 3, 7] => "m",
		[0, 3, 6] => "dim",
		[0, 4, 8] => "aug",
		[0, 4, 7, 10] => "7",
		[0, 3, 7, 10] => "m7",
		[0, 4, 7, 11] => "maj7",
		[0, 3, 6, 10] => "m7b5",
		[0, 3, 6, 9] => "dim7",
		[0, 5, 7] => "sus4",
		[0, 2, 7] => "sus2",
		_ => return chord.symbol.clone(),
	};
	let bass = chord
		.bass
		.map(|b| format!("/{}", b.name(false)))
		.unwrap_or_default();
	format!("{root}{quality}{bass}")
}

/// Chords resolved from `chords`, with the beats per chord of a progression and a warning.
struct ResolvedChords {
	chords: Vec<Chord>,
	beats_per_chord: Option<f64>,
	warning: Option<String>,
}

/// Resolves `chords` (symbols, roman numerals or a progression id).
fn resolve_chords(
	library: &Library,
	chords: &str,
	key: Option<&Scale>,
) -> Result<ResolvedChords, McpError> {
	let trimmed = chords.trim();
	let looks_like_id = trimmed.contains('/') || trimmed.starts_with("prog_");
	if looks_like_id && !trimmed.contains(' ') {
		let it = library
			.item(trimmed)
			.ok_or_else(|| unknown(library, trimmed))?;
		let Item::Progression(p) = &it.item else {
			return Err(McpError::Invalid(format!(
				"'{trimmed}' is a {}, not a progression; pass chord symbols or a progression id",
				it.item.kind()
			)));
		};
		let key = key.ok_or_else(|| {
			McpError::Invalid(format!(
				"progression '{trimmed}' is in roman numerals ({}): pass `key` too (e.g. \"A {}\")",
				p.numerals, p.mode
			))
		})?;
		let kind = Scale::parse(&format!("C {}", p.mode))
			.map(|s| s.kind)
			.unwrap_or(key.kind);
		let mut warning = None;
		if kind != key.kind && !(is_minorish(kind) && is_minorish(key.kind)) {
			warning = Some(format!(
				"progression '{}' is written for {} and was read in {} {}",
				it.id,
				p.mode,
				key.root.name(false),
				p.mode
			));
		}
		let scale = Scale::new(key.root, kind);
		let chords = cadenza_theory::parse_roman_progression(&p.numerals, &scale)?;
		return Ok(ResolvedChords {
			chords,
			beats_per_chord: Some(p.beats_per_chord),
			warning,
		});
	}
	Ok(ResolvedChords {
		chords: parse_chords(trimmed, key).map_err(McpError::Invalid)?,
		beats_per_chord: None,
		warning: None,
	})
}

fn is_minorish(kind: ScaleKind) -> bool {
	!matches!(
		kind,
		ScaleKind::Major | ScaleKind::Lydian | ScaleKind::Mixolydian | ScaleKind::MajorPentatonic
	)
}

/// Builds the clip `args` describes (pure: the project is only read). Shared by
/// `apply_pattern` and the app's library drag and drop.
pub fn build_pattern_clip(
	project: &Project,
	library: &Library,
	args: &ApplyPatternArgs,
) -> Result<PatternClip, McpError> {
	let it = library
		.item(&args.pattern)
		.ok_or_else(|| unknown(library, &args.pattern))?;
	let Some(pattern) = it.item.as_pattern() else {
		return Err(McpError::Invalid(format!(
			"'{}' is a {}; apply_pattern needs a pattern id (library_search kind pattern)",
			it.id,
			it.item.kind()
		)));
	};
	if !(args.start_beat.is_finite() && args.start_beat >= 0.0) {
		return Err(McpError::Invalid("start_beat must be >= 0".into()));
	}
	if let Some(b) = args.bars
		&& !(1..=MAX_BARS).contains(&b)
	{
		return Err(McpError::Invalid(format!("bars must be 1..={MAX_BARS}")));
	}
	if let Some(n) = args.fill_every
		&& n == 0
	{
		return Err(McpError::Invalid(
			"fill_every must be >= 1 (e.g. 4 or 8)".into(),
		));
	}
	let velocity = args.velocity.unwrap_or(1.0);
	if !(0.1..=1.5).contains(&velocity) {
		return Err(McpError::Invalid("velocity must be 0.1..=1.5".into()));
	}
	let octave = args.octave.unwrap_or(0);
	if !(-3..=3).contains(&octave) {
		return Err(McpError::Invalid("octave must be -3..=3".into()));
	}
	let track = project
		.track(args.track_id)
		.ok_or(cadenza_model::ModelError::TrackNotFound(args.track_id))?;
	let key = args.key.as_deref().map(Scale::parse).transpose()?;
	let mut warnings = Vec::new();
	let (chords, progression_bpc) = match args.chords.as_deref() {
		Some(c) if !c.trim().is_empty() => {
			let resolved = resolve_chords(library, c, key.as_ref())?;
			warnings.extend(resolved.warning);
			(resolved.chords, resolved.beats_per_chord)
		}
		_ => (Vec::new(), None),
	};
	let beats_per_chord = args.beats_per_chord.or(progression_bpc).unwrap_or(4.0);
	if !(beats_per_chord > 0.0 && beats_per_chord <= 64.0) {
		return Err(McpError::Invalid(
			"beats_per_chord must be in (0, 64]".into(),
		));
	}
	let groove = match args.groove.as_deref().map(str::trim) {
		Some("none") | Some("") => None,
		Some(id) => Some(id.to_owned()),
		None => pattern.groove.clone(),
	};
	let groove = match &groove {
		Some(id) => match library.item(id).map(|i| &i.item) {
			Some(Item::Groove(g)) => Some(g),
			Some(other) => {
				return Err(McpError::Invalid(format!(
					"'{id}' is a {}, not a groove",
					other.kind()
				)));
			}
			None => return Err(unknown(library, id)),
		},
		None => None,
	};
	let opts = RealizeOptions {
		bars: args.bars,
		key,
		chords: chords.clone(),
		beats_per_chord,
		fill_every: args.fill_every,
		vary: args.vary.unwrap_or(false),
		fill: library.family_member(pattern, Variant::Fill),
		variation: library.family_member(pattern, Variant::Variation),
		groove,
		velocity,
		octave,
	};
	let realized = realize(pattern, &opts).map_err(library_error)?;
	warnings.extend(realized.warnings.iter().cloned());

	let start = beats_to_tick(args.start_beat);
	let meter = project.meter();
	let ts = meter.signature_at(start);
	let pattern_meter = format!("{}/{}", ts.numerator, ts.denominator);
	if pattern_meter != pattern.meter {
		warnings.push(format!(
			"the song is in {pattern_meter} at beat {} but '{}' is {}: its bars will drift from the bar lines",
			args.start_beat, pattern.id, pattern.meter
		));
	}
	let drums = matches!(
		track.instrument.as_ref().and_then(|d| d.builtin_id()),
		Some("drum_machine" | "drum_sampler")
	);
	if pattern.role.is_percussive() && pattern.harmony == Harmony::None {
		let mut lanes: Vec<(&str, u8)> = Vec::new();
		for (name, _) in pattern.lanes.iter() {
			if let Some(n) = cadenza_library::drum_note(name) {
				lanes.push((name, n));
			}
		}
		warnings.extend(drum_lane_warnings(project, args.track_id, lanes));
		if track.instrument.is_some() && !drums {
			warnings.push(format!(
				"'{}' is a drum pattern but track '{}' has no drum instrument (drum_machine or \
				 drum_sampler): its notes will play as pitches",
				pattern.id, track.name
			));
		}
	} else if drums {
		warnings.push(format!(
			"'{}' is a pitched {} pattern but track '{}' plays drums",
			pattern.id, pattern.role, track.name
		));
	}

	let clip = Clip::midi(
		format!("{} [{}]", pattern.name, pattern.id),
		start,
		beats_to_tick(realized.length_beats),
	)
	.with_notes(realized.notes.iter().map(|n| {
		Note::new(
			n.pitch,
			beats_to_tick(n.start_beats),
			beats_to_tick(n.length_beats).max(Tick(1)),
			n.velocity,
		)
	}));
	let summary = summary_line(pattern, &realized, &opts, &chords, args.start_beat);
	Ok(PatternClip {
		clip,
		summary,
		warnings,
	})
}

fn summary_line(
	pattern: &Pattern,
	realized: &cadenza_library::Realized,
	opts: &RealizeOptions,
	chords: &[Chord],
	start_beat: f64,
) -> String {
	let mut s = format!(
		"{} [{}]: {} bar{} ({} notes) at beat {start_beat}",
		pattern.name,
		pattern.id,
		realized.bars,
		if realized.bars == 1 { "" } else { "s" },
		realized.notes.len()
	);
	if let Some(key) = &opts.key {
		s.push_str(&format!(
			", key {} {}",
			key.root.name(false),
			format!("{:?}", key.kind).to_lowercase()
		));
	}
	let uses_chords = pattern.harmony == Harmony::Chord
		|| opts
			.fill
			.into_iter()
			.chain(opts.variation)
			.any(|p| p.harmony == Harmony::Chord);
	if uses_chords && !chords.is_empty() {
		let names: Vec<String> = chords.iter().map(chord_name).collect();
		s.push_str(&format!(
			", over {} ({} beats/chord)",
			names.join(" "),
			opts.beats_per_chord
		));
	}
	if let Some(n) = opts.fill_every
		&& opts.fill.is_some()
	{
		s.push_str(&format!(", fill every {n} bars"));
	}
	if opts.vary && opts.variation.is_some() {
		s.push_str(", varied");
	}
	if let Some(g) = opts.groove {
		s.push_str(&format!(", groove {}", g.id));
	}
	s
}

pub fn apply_pattern(
	session: &mut Session,
	args: ApplyPatternArgs,
) -> Result<ApplyPatternOutput, McpError> {
	let library = session.library();
	let built = build_pattern_clip(session.document().project(), &library, &args)?;
	let clip_id = built.clip.id;
	let length_beats = tick_to_beats(built.clip.length);
	session.document_mut().execute(Command::AddClip {
		track: args.track_id,
		clip: built.clip,
	})?;
	Ok(ApplyPatternOutput {
		clip_id,
		length_beats,
		summary: built.summary,
		warnings: built.warnings,
	})
}

#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
pub struct ApplyFormArgs {
	/// Form id ("core/form_latin_pop_radio").
	pub form: String,
	/// 1-based bar the first section starts at (default 1).
	#[serde(default)]
	pub start_bar: Option<u32>,
	/// Song tempo in bpm, set from the form's start.
	#[serde(default)]
	pub tempo: Option<f64>,
	/// Style guide the song follows ("core/latin_pop"), stored in the project so later
	/// sessions can re-read it (get_project shows it). Default: the one style that lists
	/// this form, if exactly one does.
	#[serde(default)]
	pub style: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct SectionOut {
	pub name: String,
	pub start_beat: f64,
	/// 1-based.
	pub start_bar: u32,
	pub bars: u32,
	pub energy: u8,
	pub roles: Vec<String>,
	/// What the last bar does: drop | fill | riser.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub ending: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ApplyFormOutput {
	pub sections: Vec<SectionOut>,
	pub end_beat: f64,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub style: Option<String>,
	#[serde(skip_serializing_if = "Vec::is_empty")]
	pub warnings: Vec<String>,
}

/// The commands and section table of a form (pure). Markers replace any marker already at
/// a section start.
pub fn build_form(
	project: &Project,
	library: &Library,
	args: &ApplyFormArgs,
) -> Result<(Command, ApplyFormOutput), McpError> {
	let it = library
		.item(&args.form)
		.ok_or_else(|| unknown(library, &args.form))?;
	let Item::Form(form) = &it.item else {
		return Err(McpError::Invalid(format!(
			"'{}' is a {}; apply_form needs a form id (library_search kind form)",
			it.id,
			it.item.kind()
		)));
	};
	let start_bar = args.start_bar.unwrap_or(1);
	if !(1..=10_000).contains(&start_bar) {
		return Err(McpError::Invalid("start_bar is 1-based: 1..=10000".into()));
	}
	let style = match &args.style {
		Some(id) => match library.item(id) {
			Some(s) if s.item.kind() == ItemKind::Style => Some(s.id.clone()),
			Some(other) => {
				return Err(McpError::Invalid(format!(
					"'{id}' is a {}, not a style",
					other.item.kind()
				)));
			}
			None => return Err(unknown(library, id)),
		},
		None => {
			let listing: Vec<&str> = library
				.items()
				.filter_map(|i| match &i.item {
					Item::Style(s) if s.forms.contains(&it.id) => Some(i.id.as_str()),
					_ => None,
				})
				.collect();
			(listing.len() == 1).then(|| listing[0].to_owned())
		}
	};
	let meter = project.meter();
	let mut commands = Vec::new();
	let mut warnings = Vec::new();
	if let Some(bpm) = args.tempo {
		commands.push(Command::SetTempo {
			tick: meter.tick_at_bar_position(f64::from(start_bar - 1)),
			bpm,
		});
	}
	let mut sections = Vec::with_capacity(form.sections.len());
	let mut bar = start_bar - 1;
	for s in &form.sections {
		let tick = meter.tick_at_bar_position(f64::from(bar));
		if let Some(old) = project.markers.iter().find(|m| m.tick == tick) {
			if old.name != s.name {
				warnings.push(format!(
					"marker '{}' at bar {} was replaced by '{}'",
					old.name,
					bar + 1,
					s.name
				));
			}
			commands.push(Command::RemoveMarker { tick });
		}
		commands.push(Command::AddMarker {
			marker: Marker::new(tick, s.name.clone()),
		});
		sections.push(SectionOut {
			name: s.name.clone(),
			start_beat: tick_to_beats(tick),
			start_bar: bar + 1,
			bars: s.bars,
			energy: s.energy,
			roles: s.roles.clone(),
			ending: s.ending.map(|e| {
				serde_json::to_value(e)
					.ok()
					.and_then(|v| v.as_str().map(str::to_owned))
					.unwrap_or_default()
			}),
		});
		bar += s.bars;
	}
	let end_beat = tick_to_beats(meter.tick_at_bar_position(f64::from(bar)));
	if style.is_some() && project.meta.style != style {
		commands.push(Command::SetProjectMeta {
			meta: ProjectMeta {
				style: style.clone(),
			},
		});
	}
	let command = Command::Batch {
		label: format!("Apply form '{}'", form.name),
		commands,
	};
	Ok((
		command,
		ApplyFormOutput {
			sections,
			end_beat,
			style,
			warnings,
		},
	))
}

pub fn apply_form(session: &mut Session, args: ApplyFormArgs) -> Result<ApplyFormOutput, McpError> {
	let library = session.library();
	let (command, out) = build_form(session.document().project(), &library, &args)?;
	session.document_mut().execute(command)?;
	Ok(out)
}

#[cfg(test)]
mod tests {
	use std::sync::Arc;

	use cadenza_model::{Device, Track, TrackKind};

	use super::*;

	fn session() -> Session {
		let mut s = Session::new();
		s.set_library(Arc::new(Library::core()));
		s
	}

	fn track(session: &mut Session, instrument: &str) -> TrackId {
		let mut track = Track::new("T", TrackKind::Instrument);
		track.instrument = Some(Device::builtin(instrument, instrument));
		let id = track.id;
		session
			.document_mut()
			.execute(Command::AddTrack { track, index: None })
			.unwrap();
		id
	}

	#[test]
	fn search_finds_the_style_first_and_respects_the_limit() {
		let mut s = session();
		let out = library_search(
			&mut s,
			LibrarySearchArgs {
				query: Some("pop latino".into()),
				..Default::default()
			},
		)
		.unwrap();
		assert_eq!(out.cards[0].id, "core/latin_pop");
		assert_eq!(out.cards[0].kind, "style");
		assert!(out.cards.len() <= DEFAULT_LIMIT);
		let all = library_search(
			&mut s,
			LibrarySearchArgs {
				kind: Some(KindArg::Pattern),
				limit: Some(100),
				..Default::default()
			},
		)
		.unwrap();
		assert_eq!(all.cards.len(), MAX_LIMIT);
		assert!(all.total >= 40);
	}

	#[test]
	fn get_returns_items_with_license_and_suggests_on_unknown_ids() {
		let mut s = session();
		let out = library_get(
			&mut s,
			LibraryGetArgs {
				ids: vec!["core/dembow_basic".into(), "latin_pop".into()],
			},
		)
		.unwrap();
		assert_eq!(out.items[0]["lanes"]["snare"], "...x ..x. ...x ..x.");
		assert_eq!(out.items[1]["license"], "CC0-1.0");
		let err = library_get(
			&mut s,
			LibraryGetArgs {
				ids: vec!["core/dembow_basik".into()],
			},
		)
		.unwrap_err()
		.to_string();
		assert!(err.contains("closest: core/dembow"), "{err}");
	}

	#[test]
	fn apply_pattern_adds_one_named_clip_in_one_undo_step() {
		let mut s = session();
		let drums = track(&mut s, "drum_machine");
		let out = apply_pattern(
			&mut s,
			ApplyPatternArgs {
				pattern: "core/dembow_basic".into(),
				track_id: drums,
				start_beat: 16.0,
				bars: Some(8),
				fill_every: Some(4),
				..Default::default()
			},
		)
		.unwrap();
		assert_eq!(out.length_beats, 32.0);
		assert!(out.summary.contains("fill every 4 bars"), "{}", out.summary);
		let project = s.document().project();
		let (_, clip) = project.find_clip(out.clip_id).unwrap();
		assert_eq!(clip.name, "Dembow basic [core/dembow_basic]");
		assert_eq!(clip.start.beats(), 16.0);
		assert_eq!(s.document().history_labels().len(), 2);
		s.document_mut().undo().unwrap();
		assert!(s.document().project().find_clip(out.clip_id).is_none());
	}

	#[test]
	fn apply_pattern_reads_progressions_and_needs_chords() {
		let mut s = session();
		let bass = track(&mut s, "poly_synth");
		let err = apply_pattern(
			&mut s,
			ApplyPatternArgs {
				pattern: "core/bass_tresillo_808".into(),
				track_id: bass,
				..Default::default()
			},
		)
		.unwrap_err()
		.to_string();
		assert!(err.contains("needs chords"), "{err}");
		let err = apply_pattern(
			&mut s,
			ApplyPatternArgs {
				pattern: "core/bass_tresillo_808".into(),
				track_id: bass,
				chords: Some("core/prog_i_VI_III_VII".into()),
				..Default::default()
			},
		)
		.unwrap_err()
		.to_string();
		assert!(err.contains("pass `key`"), "{err}");
		let out = apply_pattern(
			&mut s,
			ApplyPatternArgs {
				pattern: "core/bass_tresillo_808".into(),
				track_id: bass,
				key: Some("A minor".into()),
				chords: Some("core/prog_i_VI_III_VII".into()),
				bars: Some(4),
				vary: Some(true),
				..Default::default()
			},
		)
		.unwrap();
		assert!(out.summary.contains("over Am F C G"), "{}", out.summary);
		assert!(out.summary.contains("varied"), "{}", out.summary);
		let project = s.document().project();
		let (_, clip) = project.find_clip(out.clip_id).unwrap();
		// A1 (33) under Am, F1 (29) under F.
		assert_eq!(clip.notes()[0].pitch, 33);
		assert!(clip.notes().iter().any(|n| n.pitch == 29));
	}

	#[test]
	fn apply_pattern_warns_about_silent_drum_lanes_and_wrong_tracks() {
		let mut s = session();
		let drums = track(&mut s, "drum_machine");
		let out = apply_pattern(
			&mut s,
			ApplyPatternArgs {
				pattern: "core/perc_conga_tumbao".into(),
				track_id: drums,
				..Default::default()
			},
		)
		.unwrap();
		assert!(
			out.warnings
				.iter()
				.any(|w| w.contains("silent on drum_machine"))
		);
		let out = apply_pattern(
			&mut s,
			ApplyPatternArgs {
				pattern: "core/hook_pluck_minor".into(),
				track_id: drums,
				key: Some("A minor".into()),
				..Default::default()
			},
		)
		.unwrap();
		assert!(out.warnings.iter().any(|w| w.contains("plays drums")));
		let err = apply_pattern(
			&mut s,
			ApplyPatternArgs {
				pattern: "core/latin_pop".into(),
				track_id: drums,
				..Default::default()
			},
		)
		.unwrap_err()
		.to_string();
		assert!(err.contains("is a style"), "{err}");
	}

	#[test]
	fn apply_form_marks_sections_sets_tempo_and_style_in_one_step() {
		let mut s = session();
		s.document_mut()
			.execute(Command::AddMarker {
				marker: Marker::new(Tick::ZERO, "Start"),
			})
			.unwrap();
		let out = apply_form(
			&mut s,
			ApplyFormArgs {
				form: "core/form_latin_pop_short".into(),
				tempo: Some(94.0),
				..Default::default()
			},
		)
		.unwrap();
		assert_eq!(out.style.as_deref(), Some("core/latin_pop"));
		assert_eq!(out.sections[1].name, "Verse 1");
		assert_eq!(out.sections[1].start_bar, 5);
		assert_eq!(out.sections[1].start_beat, 16.0);
		assert_eq!(out.end_beat, 48.0 * 4.0);
		assert_eq!(out.warnings.len(), 1);
		let project = s.document().project();
		assert_eq!(project.markers.len(), 8);
		assert_eq!(project.meta.style.as_deref(), Some("core/latin_pop"));
		assert_eq!(project.summary().style.as_deref(), Some("core/latin_pop"));
		assert_eq!(project.tempo.points()[0].bpm, 94.0);
		assert_eq!(
			s.document().history_labels().last().copied(),
			Some("Apply form 'Latin pop short form'")
		);
		s.document_mut().undo().unwrap();
		let project = s.document().project();
		assert_eq!(project.markers.len(), 1);
		assert!(project.meta.style.is_none());
	}

	/// The outputs the model reads stay within the design's budgets for the core pack.
	#[test]
	fn search_and_get_outputs_stay_under_their_caps() {
		let mut s = session();
		let library = s.library();
		for it in library.items() {
			let out = library_get(
				&mut s,
				LibraryGetArgs {
					ids: vec![it.id.clone()],
				},
			)
			.unwrap();
			let size = serde_json::to_string(&out).unwrap().len();
			let cap = it.item.kind().max_detail_chars() + 64;
			assert!(
				size <= cap,
				"library_get {} is {size} chars (cap {cap})",
				it.id
			);
		}
		for query in [
			"latin pop",
			"dembow",
			"house",
			"bass",
			"",
			"trap 808",
			"lofi",
		] {
			let out = library_search(
				&mut s,
				LibrarySearchArgs {
					query: Some(query.into()),
					..Default::default()
				},
			)
			.unwrap();
			for c in &out.cards {
				let size = serde_json::to_string(c).unwrap().len();
				assert!(size <= 320, "card {} is {size} chars", c.id);
			}
			let size = serde_json::to_string(&out).unwrap().len();
			assert!(size <= 2600, "search '{query}' is {size} chars");
		}
		let out = library_search(
			&mut s,
			LibrarySearchArgs {
				limit: Some(MAX_LIMIT),
				..Default::default()
			},
		)
		.unwrap();
		assert!(serde_json::to_string(&out).unwrap().len() <= 8000);
	}
}
