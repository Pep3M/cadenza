//! Pack and item validation: schema rules, character budgets, lane keys and license
//! fields. Every message names the offending field and what is accepted, so a pack
//! author (or an LLM writing a variation) can fix it from the text alone.

use cadenza_theory::Scale;

use crate::format::{
	Feel, Form, Groove, Harmony, Item, MAX_SUMMARY_CHARS, PACK_FORMAT_VERSION, PackManifest,
	Pattern, Progression, Role, RoleValue, Style, Variant, parse_meter,
};
use crate::lanes::{LaneKey, Step, parse_steps};

/// `[a-z0-9_]+`, at most `max` chars.
pub fn is_slug(s: &str, max: usize) -> bool {
	!s.is_empty()
		&& s.len() <= max
		&& s.bytes()
			.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// An item slug: `[A-Za-z0-9_]+` (roman numerals keep their case: `prog_i_VI_III_VII`).
pub fn is_item_slug(s: &str) -> bool {
	!s.is_empty() && s.len() <= 64 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// `<pack>/<slug>`.
pub fn is_item_id(s: &str) -> bool {
	s.split_once('/')
		.is_some_and(|(p, i)| is_slug(p, 32) && is_item_slug(i))
}

fn is_spdx(s: &str) -> bool {
	!s.is_empty()
		&& s.len() <= 64
		&& s.bytes()
			.all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'+'))
}

/// Problems of a `pack.json` (empty = valid).
pub fn manifest(m: &PackManifest) -> Vec<String> {
	let mut out = Vec::new();
	if m.format_version != PACK_FORMAT_VERSION {
		out.push(format!(
			"format_version {} is not supported (expected {PACK_FORMAT_VERSION})",
			m.format_version
		));
	}
	if !is_slug(&m.id, 32) {
		out.push(format!(
			"id '{}' must be 1-32 chars of a-z, 0-9 and '_'",
			m.id
		));
	}
	if m.name.trim().is_empty() {
		out.push("name is empty".into());
	}
	if m.version.trim().is_empty() {
		out.push("version is empty (e.g. \"1.0.0\")".into());
	}
	if !is_spdx(&m.license) {
		out.push(format!(
			"license '{}' must be an SPDX id (\"CC0-1.0\", \"CC-BY-4.0\", \"MIT\") or \
			 \"LicenseRef-<name>\"",
			m.license
		));
	}
	if m.license.starts_with("CC-BY") && m.attribution.trim().is_empty() {
		out.push(format!(
			"license {} requires an attribution line (\"attribution\")",
			m.license
		));
	}
	out
}

fn check_common(item: &Item, out: &mut Vec<String>) {
	let c = item.common();
	let slug = c.id.rsplit('/').next().unwrap_or(c.id);
	if !is_item_slug(slug) {
		out.push(format!(
			"id '{}' must be 1-64 chars of letters, digits and '_'",
			c.id
		));
	}
	if c.name.trim().is_empty() || c.name.chars().count() > 60 {
		out.push("name must be 1-60 chars".into());
	}
	let summary = c.summary.chars().count();
	if summary == 0 || summary > MAX_SUMMARY_CHARS {
		out.push(format!(
			"summary is {summary} chars; it must be 1-{MAX_SUMMARY_CHARS} (the search card line)"
		));
	}
	for g in c.genres.iter().chain(c.tags) {
		if !is_slug(g, 32) {
			out.push(format!(
				"genre/tag '{g}' must be lower-case a-z, 0-9 and '_' (\"latin_pop\")"
			));
		}
	}
	let size = serde_json::to_string(item).map(|s| s.len()).unwrap_or(0);
	let max = item.kind().max_detail_chars();
	if size > max {
		out.push(format!(
			"is {size} chars as compact JSON; a {} must stay under {max} (library_get budget)",
			item.kind()
		));
	}
}

fn check_ids(field: &str, ids: &[String], out: &mut Vec<String>) {
	for id in ids {
		if !is_item_id(id) {
			out.push(format!(
				"{field}: '{id}' is not an item id like \"core/dembow_basic\""
			));
		}
	}
}

fn check_energy(energy: u8, out: &mut Vec<String>) {
	if !(1..=5).contains(&energy) {
		out.push(format!("energy {energy} must be 1..=5"));
	}
}

/// Problems of one item (empty = valid). References to other items are checked by
/// [`crate::Library::check_references`].
pub fn item(item: &Item) -> Vec<String> {
	let mut out = Vec::new();
	check_common(item, &mut out);
	match item {
		Item::Pattern(p) => pattern(p, &mut out),
		Item::Style(s) => style(s, &mut out),
		Item::Form(f) => form(f, &mut out),
		Item::Progression(p) => progression(p, &mut out),
		Item::Groove(g) => groove(g, &mut out),
	}
	out
}

fn pattern(p: &Pattern, out: &mut Vec<String>) {
	check_energy(p.energy, out);
	if !(1..=16).contains(&p.bars) {
		out.push(format!("bars {} must be 1..=16", p.bars));
	}
	if !(1..=12).contains(&p.steps_per_beat) {
		out.push(format!(
			"steps_per_beat {} must be 1..=12 (4 = sixteenths)",
			p.steps_per_beat
		));
	}
	if parse_meter(&p.meter).is_none() {
		out.push(format!(
			"meter '{}' must look like \"4/4\" or \"6/8\"",
			p.meter
		));
	}
	if Feel::parse(&p.feel).is_none() {
		out.push(format!(
			"feel '{}' must be straight, triplet or swing:<0..1>",
			p.feel
		));
	}
	if let Some([lo, hi]) = p.tempo
		&& !(40.0 <= lo && lo <= hi && hi <= 240.0)
	{
		out.push(format!(
			"tempo [{lo}, {hi}] must be a range within 40..=240 bpm"
		));
	}
	if p.variant != Variant::Main && p.family.is_none() {
		out.push(format!(
			"variant {} needs a family (the pattern it varies)",
			p.variant.as_str()
		));
	}
	if let Some(f) = &p.family
		&& !is_slug(f, 32)
	{
		out.push(format!("family '{f}' must be a slug"));
	}
	for s in &p.sections {
		if !is_slug(s, 32) {
			out.push(format!(
				"section '{s}' must be a slug (\"verse\", \"chorus\")"
			));
		}
	}
	match (p.harmony, &p.key) {
		(Harmony::Absolute, None) => {
			out.push("harmony absolute needs the key it is written in (\"key\")".into())
		}
		(_, Some(k)) if Scale::parse(k).is_err() => {
			out.push(format!("key '{k}' must look like \"A minor\""))
		}
		_ => {}
	}
	if matches!(p.role, Role::Drums | Role::Perc) && p.harmony != Harmony::None {
		out.push(format!("role {} must use harmony none", p.role));
	}
	if p.octave.is_some_and(|o| !(-3..=3).contains(&o)) {
		out.push("octave must be -3..=3".into());
	}
	check_ids("pairs_with", &p.pairs_with, out);
	if let Some(g) = &p.groove {
		check_ids("groove", std::slice::from_ref(g), out);
	}
	if p.lanes.is_empty() || p.lanes.len() > 24 {
		out.push(format!("has {} lanes; it needs 1..=24", p.lanes.len()));
	}
	let total = p.steps_per_bar().map(|s| s * p.bars as usize);
	let mut hits = 0;
	for (name, steps) in p.lanes.iter() {
		let key = match LaneKey::parse(name) {
			Ok(k) => k,
			Err(e) => {
				out.push(e);
				continue;
			}
		};
		let allowed = match p.harmony {
			Harmony::None => matches!(key, LaneKey::Drum(_) | LaneKey::Absolute(_)),
			Harmony::Chord => !key.is_drum(),
			Harmony::Scale => matches!(key, LaneKey::Degree { .. } | LaneKey::Absolute(_)),
			Harmony::Absolute => matches!(key, LaneKey::Absolute(_)),
		};
		if !allowed {
			let what = match p.harmony {
				Harmony::None => "drum names or notes",
				Harmony::Chord => "chord tones, scale degrees or notes",
				Harmony::Scale => "scale degrees or notes",
				Harmony::Absolute => "notes",
			};
			out.push(format!(
				"lane '{name}' does not fit harmony {:?}: use {what}",
				p.harmony
			));
		}
		match parse_steps(steps) {
			Err(c) => out.push(format!(
				"lane '{name}' has unrecognized step '{c}'; use X x o = . - (spaces and | are ignored)"
			)),
			Ok(steps) => {
				hits += steps.iter().filter(|s| matches!(s, Step::Hit(_))).count();
				if let Some(total) = total
					&& (steps.is_empty() || total % steps.len() != 0)
				{
					out.push(format!(
						"lane '{name}' has {} steps; it must divide the pattern's {total} \
						 ({} bars x {} steps)",
						steps.len(),
						p.bars,
						total / p.bars.max(1) as usize
					));
				}
			}
		}
	}
	if hits == 0 {
		out.push("has no notes".into());
	}
}

fn style(s: &Style, out: &mut Vec<String>) {
	let t = &s.tempo;
	if !(40.0 <= t.min && t.min <= t.typical && t.typical <= t.max && t.max <= 240.0) {
		out.push("tempo must satisfy 40 <= min <= typical <= max <= 240".into());
	}
	if Feel::parse(&s.feel).is_none() {
		out.push(format!(
			"feel '{}' must be straight, triplet or swing:<0..1>",
			s.feel
		));
	}
	if parse_meter(&s.meter).is_none() {
		out.push(format!("meter '{}' must look like \"4/4\"", s.meter));
	}
	if !(-30.0..=0.0).contains(&s.mix.lufs) {
		out.push(format!("mix.lufs {} must be -30..=0", s.mix.lufs));
	}
	if s.harmony.harmonic_rhythm_beats <= 0.0 {
		out.push("harmony.harmonic_rhythm_beats must be > 0".into());
	}
	check_ids("harmony.loops", &s.harmony.loops, out);
	check_ids("forms", &s.forms, out);
	if s.roles.is_empty() {
		out.push("roles is empty".into());
	}
	for (role, sections) in s.roles.iter() {
		for (section, value) in sections.iter() {
			if let RoleValue::Ids(ids) = value {
				check_ids(&format!("roles.{role}.{section}"), ids, out);
			}
		}
	}
}

fn form(f: &Form, out: &mut Vec<String>) {
	if f.sections.is_empty() || f.sections.len() > 32 {
		out.push(format!(
			"has {} sections; it needs 1..=32",
			f.sections.len()
		));
	}
	for s in &f.sections {
		if s.name.trim().is_empty() {
			out.push("a section has an empty name".into());
		}
		if !(1..=64).contains(&s.bars) {
			out.push(format!("section '{}' has {} bars; 1..=64", s.name, s.bars));
		}
		check_energy(s.energy, out);
	}
}

fn progression(p: &Progression, out: &mut Vec<String>) {
	match Scale::parse(&format!("C {}", p.mode)) {
		Err(_) => out.push(format!(
			"mode '{}' must be a scale kind (major, minor, dorian, mixolydian...)",
			p.mode
		)),
		Ok(key) => {
			if let Err(e) = cadenza_theory::parse_roman_progression(&p.numerals, &key) {
				out.push(format!("numerals: {e}"));
			}
		}
	}
	if !(p.beats_per_chord > 0.0 && p.beats_per_chord <= 16.0) {
		out.push("beats_per_chord must be in (0, 16]".into());
	}
}

fn groove(g: &Groove, out: &mut Vec<String>) {
	if !(1..=12).contains(&g.steps_per_beat) {
		out.push(format!(
			"steps_per_beat {} must be 1..=12",
			g.steps_per_beat
		));
	}
	if g.timing.is_empty() || g.timing.len() > 64 {
		out.push(format!("timing has {} steps; 1..=64", g.timing.len()));
	}
	if g.timing.iter().any(|t| !(-0.5..=0.5).contains(t)) {
		out.push("timing offsets must be -0.5..=0.5 (fractions of a step)".into());
	}
	if !g.velocity.is_empty() && g.velocity.len() != g.timing.len() {
		out.push("velocity must be empty or as long as timing".into());
	}
	if g.velocity.iter().any(|v| !(0.0..=2.0).contains(v)) {
		out.push("velocity multipliers must be 0..=2".into());
	}
}
