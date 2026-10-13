//! The browser's Library tab (style library): search filters and which item is open, the
//! pattern preview (auditioned through the stopped-transport note preview path), and the
//! pure builders that turn library items into commands — the same `apply_pattern` /
//! `apply_form` builders the MCP tools use, so a dropped pattern and an AI-applied one are
//! identical.

use std::time::Instant;

use cadenza_core::{ClipId, Tick, TrackId};
use cadenza_library::{
	Harmony, Item, ItemKind, Library, LoadedItem, Pattern, Query, RealizeOptions, Role,
	parse_chords, realize,
};
use cadenza_mcp::tools::library::{
	ApplyFormArgs, ApplyPatternArgs, build_form, build_pattern_clip,
};
use cadenza_model::{Command, Device, Project, Track, TrackKind};
use cadenza_theory::Scale;

use super::edits;

/// Search filters and navigation of the Library tab.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LibraryView {
	pub query: String,
	pub kind: Option<ItemKind>,
	pub genre: Option<String>,
	pub role: Option<Role>,
	pub section: Option<String>,
	pub energy: Option<u8>,
	/// A style or form shown as a read-only document.
	pub open: Option<String>,
	/// The credits (packs and their attribution) instead of the list.
	pub credits: bool,
	/// The card last clicked.
	pub selected: Option<String>,
}

impl LibraryView {
	pub fn query(&self) -> Query {
		Query {
			text: self.query.clone(),
			kind: self.kind,
			genre: self.genre.clone(),
			role: self.role,
			section: self.section.clone(),
			energy: self.energy,
			family: None,
		}
	}

	/// Any filter besides the text is set.
	pub fn filtered(&self) -> bool {
		self.kind.is_some()
			|| self.genre.is_some()
			|| self.role.is_some()
			|| self.section.is_some()
			|| self.energy.is_some()
	}

	pub fn clear_filters(&mut self) {
		self.kind = None;
		self.genre = None;
		self.role = None;
		self.section = None;
		self.energy = None;
	}
}

/// Section names the filter offers (as patterns list them).
pub const SECTIONS: &[&str] = &[
	"intro",
	"verse",
	"pre",
	"chorus",
	"post_chorus",
	"bridge",
	"drop",
	"break",
	"montuno",
	"outro",
];

/// Genres the installed items use, sorted.
pub fn genres(library: &Library) -> Vec<String> {
	let mut all: Vec<String> = library
		.items()
		.flat_map(|i| i.item.common().genres.iter().cloned())
		.collect();
	all.sort();
	all.dedup();
	all
}

/// Key and chords patterns are auditioned and dropped with: the first loop of the song's
/// style guide (`Project.meta.style`), else i-VI-III-VII, read in A minor for minor
/// progressions and C major otherwise.
pub fn default_harmony(project: &Project, library: &Library) -> (String, String) {
	let from_style = project
		.meta
		.style
		.as_deref()
		.and_then(|s| library.item(s))
		.and_then(|i| match &i.item {
			Item::Style(s) => s.harmony.loops.first().cloned(),
			_ => None,
		});
	let chords = from_style.unwrap_or_else(|| "core/prog_i_VI_III_VII".to_owned());
	let mode = match library.item(&chords).map(|i| &i.item) {
		Some(Item::Progression(p)) => p.mode.clone(),
		_ => "minor".to_owned(),
	};
	let root = match mode.as_str() {
		"major" | "lydian" | "mixolydian" => "C",
		_ => "A",
	};
	(format!("{root} {mode}"), chords)
}

/// Bars a dropped pattern fills: its own length repeated up to at least 4 bars.
pub fn drop_bars(pattern: &Pattern) -> u32 {
	let bars = pattern.bars.max(1);
	bars * 4u32.div_ceil(bars)
}

/// The `apply_pattern` arguments of a browser drop at `at` (snapped to its bar).
pub fn pattern_args(
	project: &Project,
	library: &Library,
	pattern: &Pattern,
	track: TrackId,
	at: Tick,
) -> ApplyPatternArgs {
	let start = project.meter().bar_floor(at.max(Tick::ZERO));
	let (key, chords) = default_harmony(project, library);
	let needs_chords = pattern.harmony == Harmony::Chord;
	ApplyPatternArgs {
		pattern: pattern.id.clone(),
		track_id: track,
		start_beat: start.beats(),
		bars: Some(drop_bars(pattern)),
		key: (pattern.harmony != Harmony::None).then_some(key),
		chords: needs_chords.then_some(chords),
		..Default::default()
	}
}

/// Whether a pattern is played by a drum kit.
pub fn is_drum_pattern(pattern: &Pattern) -> bool {
	pattern.role.is_percussive() && pattern.harmony == Harmony::None
}

fn is_kit(track: &Track) -> bool {
	matches!(
		track.instrument.as_ref().and_then(|d| d.builtin_id()),
		Some("drum_machine" | "drum_sampler")
	)
}

/// A new instrument track for a pattern: a drum machine for drum patterns, else a synth
/// with a factory preset that suits the role.
fn track_for(project: &Project, pattern: &Pattern) -> Track {
	let mut track = edits::new_track(project, TrackKind::Instrument);
	let (name, device, preset) = match pattern.role {
		_ if is_drum_pattern(pattern) => (
			if pattern.role == Role::Perc {
				"Percussion"
			} else {
				"Drums"
			},
			"drum_machine",
			None,
		),
		Role::Bass => ("Bass", "poly_synth", Some("Sub Bass")),
		Role::Pad => ("Pad", "poly_synth", Some("Warm Pad")),
		Role::Comping => ("Keys", "electric_piano", None),
		Role::Arp => ("Arp", "poly_synth", Some("Pluck Lead")),
		_ => ("Lead", "poly_synth", Some("Pluck Lead")),
	};
	let info = cadenza_dsp::registry().iter().find(|d| d.id == device);
	let mut instrument = Device::builtin(device, info.map_or(device, |i| i.name));
	if let Some(p) = preset.and_then(|n| {
		cadenza_dsp::factory_presets(device)
			.iter()
			.find(|p| p.name == n)
	}) {
		instrument.params = p
			.params
			.iter()
			.map(|(k, v)| ((*k).to_owned(), *v))
			.collect();
	}
	track.name = name.to_owned();
	track.instrument = Some(instrument);
	track
}

/// What dropping a pattern adds.
#[derive(Debug, Clone)]
pub struct PatternDrop {
	pub command: Command,
	pub clip: ClipId,
	/// The new track, when the pattern got one.
	pub new_track: Option<TrackId>,
	pub track: TrackId,
	pub summary: String,
	pub warnings: Vec<String>,
}

/// One `Batch` "Add pattern '<name>'": the clip on `track`, or a new track (below `after`
/// or at the end) plus the clip.
pub fn add_pattern(
	project: &Project,
	library: &Library,
	pattern_id: &str,
	track: Option<TrackId>,
	at: Tick,
) -> Result<PatternDrop, String> {
	let pattern = library
		.pattern(pattern_id)
		.ok_or_else(|| format!("'{pattern_id}' is not an installed pattern"))?;
	let mut commands = Vec::new();
	let mut scratch;
	let (target, project, new_track) = match track {
		Some(t) => (t, project, None),
		None => {
			let new = track_for(project, pattern);
			let id = new.id;
			commands.push(edits::add_track(
				project,
				new.clone(),
				project.tracks.last().map(|t| t.id),
			));
			scratch = project.clone();
			scratch.tracks.push(new);
			(id, &scratch, Some(id))
		}
	};
	let args = pattern_args(project, library, pattern, target, at);
	let built = build_pattern_clip(project, library, &args).map_err(|e| e.to_string())?;
	let clip = built.clip.id;
	commands.push(Command::AddClip {
		track: target,
		clip: built.clip,
	});
	Ok(PatternDrop {
		command: Command::Batch {
			label: format!("Add pattern '{}'", pattern.name),
			commands,
		},
		clip,
		new_track,
		track: target,
		summary: built.summary,
		warnings: built.warnings,
	})
}

/// `apply_form` from the browser: the form from bar 1, with the style guide (and its
/// typical tempo) when one is given.
pub fn apply_form(
	project: &Project,
	library: &Library,
	form: &str,
	style: Option<&str>,
) -> Result<Command, String> {
	let tempo = style
		.and_then(|s| library.item(s))
		.and_then(|i| match &i.item {
			Item::Style(s) => Some(s.tempo.typical),
			_ => None,
		});
	let args = ApplyFormArgs {
		form: form.to_owned(),
		start_bar: Some(1),
		tempo,
		style: style.map(str::to_owned),
	};
	build_form(project, library, &args)
		.map(|(cmd, _)| cmd)
		.map_err(|e| e.to_string())
}

/// The track a pattern is previewed on: the selected instrument track when its sound fits
/// (a kit for drum patterns, a melodic instrument otherwise), else the first one that fits,
/// else any instrument track.
pub fn preview_track(
	project: &Project,
	selected: Option<TrackId>,
	pattern: &Pattern,
) -> Option<TrackId> {
	let drums = is_drum_pattern(pattern);
	let fits = |t: &&Track| {
		t.kind == TrackKind::Instrument && t.instrument.is_some() && is_kit(t) == drums
	};
	selected
		.and_then(|id| project.track(id))
		.filter(fits)
		.or_else(|| project.tracks.iter().find(fits))
		.or_else(|| {
			project
				.tracks
				.iter()
				.find(|t| t.kind == TrackKind::Instrument && t.instrument.is_some())
		})
		.map(|t| t.id)
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct PreviewEvent {
	/// Seconds from the preview start.
	at: f64,
	pitch: u8,
	/// `Some(velocity)` = note on, `None` = note off.
	velocity: Option<f32>,
}

/// A pattern sounding through the preview path once, at the song tempo.
#[derive(Debug, Clone)]
pub struct PatternPreview {
	pub track: TrackId,
	pub pattern: String,
	events: Vec<PreviewEvent>,
	next: usize,
	started: Instant,
}

impl PatternPreview {
	/// The pattern (with its default harmony) as timed note events on `track`.
	pub fn new(
		project: &Project,
		library: &Library,
		pattern_id: &str,
		track: TrackId,
		at: Tick,
		now: Instant,
	) -> Result<Self, String> {
		let pattern = library
			.pattern(pattern_id)
			.ok_or_else(|| format!("'{pattern_id}' is not an installed pattern"))?;
		let (key, chords) = default_harmony(project, library);
		let scale = Scale::parse(&key).map_err(|e| e.to_string())?;
		let (chords, beats_per_chord) = match library.item(&chords).map(|i| &i.item) {
			Some(Item::Progression(p)) => (
				cadenza_theory::parse_roman_progression(&p.numerals, &scale)
					.map_err(|e| e.to_string())?,
				p.beats_per_chord,
			),
			_ => (parse_chords(&chords, Some(&scale))?, 4.0),
		};
		let realized = realize(
			pattern,
			&RealizeOptions {
				key: Some(scale),
				chords,
				beats_per_chord,
				..Default::default()
			},
		)
		.map_err(|e| e.to_string())?;
		let seconds = 60.0 / project.tempo.bpm_at(at).max(1.0);
		let mut events = Vec::with_capacity(realized.notes.len() * 2);
		for n in &realized.notes {
			let on = n.start_beats * seconds;
			// Leave a hair between repeated notes so the off lands before the next on.
			let off = on + (n.length_beats * seconds - 0.005).max(0.02);
			events.push(PreviewEvent {
				at: on,
				pitch: n.pitch,
				velocity: Some(n.velocity),
			});
			events.push(PreviewEvent {
				at: off,
				pitch: n.pitch,
				velocity: None,
			});
		}
		events.sort_by(|a, b| {
			a.at.total_cmp(&b.at)
				.then(a.velocity.is_some().cmp(&b.velocity.is_some()))
		});
		Ok(Self {
			track,
			pattern: pattern.id.clone(),
			events,
			next: 0,
			started: now,
		})
	}

	/// Events due at `now`, in order: `(pitch, Some(velocity))` = on, `None` = off.
	pub fn due(&mut self, now: Instant) -> Vec<(u8, Option<f32>)> {
		let t = now.saturating_duration_since(self.started).as_secs_f64();
		let mut out = Vec::new();
		while let Some(e) = self.events.get(self.next)
			&& e.at <= t
		{
			out.push((e.pitch, e.velocity));
			self.next += 1;
		}
		out
	}

	pub fn finished(&self) -> bool {
		self.next >= self.events.len()
	}

	/// Length in seconds.
	pub fn duration(&self) -> f64 {
		self.events.last().map_or(0.0, |e| e.at)
	}
}

/// Kind glyph and a short role/kind label for a card.
pub fn badge(item: &LoadedItem) -> (&'static str, String) {
	match &item.item {
		Item::Style(_) => ("★", "style".into()),
		Item::Form(_) => ("☰", "form".into()),
		Item::Progression(_) => ("♫", "progression".into()),
		Item::Groove(_) => ("~", "groove".into()),
		Item::Pattern(p) if is_drum_pattern(p) => ("⏺", p.role.as_str().into()),
		Item::Pattern(p) => ("♪", p.role.as_str().into()),
	}
}

#[cfg(test)]
mod tests {
	use std::time::Duration;

	use super::*;

	fn project() -> (Project, TrackId, TrackId) {
		let mut p = Project::new("p");
		let mut keys = Track::new("Keys", TrackKind::Instrument);
		keys.instrument = Some(Device::builtin("poly_synth", "Synth"));
		let mut drums = Track::new("Drums", TrackKind::Instrument);
		drums.instrument = Some(Device::builtin("drum_machine", "Drums"));
		let ids = (keys.id, drums.id);
		p.tracks.push(keys);
		p.tracks.push(drums);
		(p, ids.0, ids.1)
	}

	#[test]
	fn dropping_a_pattern_snaps_to_the_bar_and_fills_four_bars() {
		let lib = Library::core();
		let (p, _, drums) = project();
		let drop = add_pattern(
			&p,
			&lib,
			"core/dembow_basic",
			Some(drums),
			Tick::from_beats(5.5),
		)
		.unwrap();
		let Command::Batch { label, commands } = &drop.command else {
			panic!()
		};
		assert_eq!(label, "Add pattern 'Dembow basic'");
		let Command::AddClip { clip, .. } = &commands[0] else {
			panic!()
		};
		assert_eq!(clip.start.beats(), 4.0);
		assert_eq!(clip.length.beats(), 16.0);
		assert_eq!(clip.name, "Dembow basic [core/dembow_basic]");
		assert!(drop.new_track.is_none());
	}

	#[test]
	fn dropping_below_the_tracks_adds_a_fitting_track() {
		let lib = Library::core();
		let (p, _, _) = project();
		let drop = add_pattern(&p, &lib, "core/bass_tresillo_808", None, Tick::ZERO).unwrap();
		let Command::Batch { commands, .. } = &drop.command else {
			panic!()
		};
		let Command::AddTrack { track, .. } = &commands[0] else {
			panic!()
		};
		assert_eq!(track.name, "Bass");
		assert_eq!(
			track.instrument.as_ref().unwrap().builtin_id(),
			Some("poly_synth")
		);
		assert!(!track.instrument.as_ref().unwrap().params.is_empty());
		assert!(drop.summary.contains("over Am F C G"), "{}", drop.summary);
		let mut doc = cadenza_model::Document::new(p);
		doc.execute(drop.command).unwrap();
		assert_eq!(doc.project().tracks.len(), 3);
	}

	#[test]
	fn the_song_style_picks_the_default_progression() {
		let lib = Library::core();
		let (mut p, _, _) = project();
		assert_eq!(
			default_harmony(&p, &lib),
			("A minor".into(), "core/prog_i_VI_III_VII".into())
		);
		p.meta.style = Some("core/pop".into());
		assert_eq!(
			default_harmony(&p, &lib),
			("C major".into(), "core/prog_I_V_vi_IV".into())
		);
	}

	#[test]
	fn previews_pick_a_fitting_track_and_play_in_time() {
		let lib = Library::core();
		let (p, keys, drums) = project();
		let dembow = lib.pattern("core/dembow_basic").unwrap();
		let hook = lib.pattern("core/hook_pluck_minor").unwrap();
		assert_eq!(preview_track(&p, Some(keys), dembow), Some(drums));
		assert_eq!(preview_track(&p, Some(drums), hook), Some(keys));
		let start = Instant::now();
		let mut preview =
			PatternPreview::new(&p, &lib, "core/dembow_basic", drums, Tick::ZERO, start).unwrap();
		// 120 bpm: one bar is 2 s; the last hat ends a 16th before the bar line.
		assert!((1.8..=2.0).contains(&preview.duration()));
		let first = preview.due(start);
		assert!(first.contains(&(36, Some(0.8))));
		let rest = preview.due(start + Duration::from_secs(3));
		assert!(preview.finished());
		let ons = first
			.iter()
			.chain(&rest)
			.filter(|(_, v)| v.is_some())
			.count();
		assert_eq!(ons, 4 + 4 + 8);
	}

	#[test]
	fn forms_apply_with_the_style_tempo() {
		let lib = Library::core();
		let (p, _, _) = project();
		let cmd = apply_form(&p, &lib, "core/form_house_club", Some("core/house")).unwrap();
		let mut doc = cadenza_model::Document::new(p);
		doc.execute(cmd).unwrap();
		assert_eq!(doc.project().tempo.points()[0].bpm, 124.0);
		assert_eq!(doc.project().meta.style.as_deref(), Some("core/house"));
		assert_eq!(doc.project().markers.len(), 7);
	}
}
