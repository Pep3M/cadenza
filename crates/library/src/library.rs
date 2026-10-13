//! Packs on disk (and the embedded `core` pack), the loaded library, and search.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::format::{Item, ItemKind, PackManifest, Pattern, Role, RoleValue, Variant};
use crate::validate;

/// Overrides the user library folder.
pub const LIBRARY_DIR_ENV: &str = "CADENZA_LIBRARY_DIR";

/// Id of the embedded pack.
pub const CORE_PACK: &str = "core";

pub(crate) mod embedded {
	include!(concat!(env!("OUT_DIR"), "/core_pack.rs"));
}

/// Where user packs live: `$CADENZA_LIBRARY_DIR`, else the OS data dir +
/// `cadenza/library` (see [`cadenza_core::paths`]).
pub fn default_library_dir() -> PathBuf {
	let var = |k: &str| {
		std::env::var_os(k)
			.filter(|v| !v.is_empty())
			.map(PathBuf::from)
	};
	if let Some(dir) = var(LIBRARY_DIR_ENV) {
		return dir;
	}
	cadenza_core::paths::data_dir().join("library")
}

/// One item of a loaded pack.
#[derive(Debug, Clone, PartialEq)]
pub struct LoadedItem {
	/// `<pack>/<slug>`.
	pub id: String,
	pub pack: String,
	/// File inside the pack ("patterns/drums/dembow_basic.json").
	pub path: String,
	pub item: Item,
}

/// Where a pack came from.
#[derive(Debug, Clone, PartialEq)]
pub enum PackOrigin {
	/// Compiled into the binary.
	Embedded,
	Dir(PathBuf),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Pack {
	pub manifest: PackManifest,
	pub origin: PackOrigin,
	pub items: Vec<LoadedItem>,
}

/// A problem found while loading: the file (inside the pack) and what is wrong.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Issue {
	pub file: String,
	pub message: String,
}

/// What happened to one pack folder.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PackReport {
	/// Folder, or "(embedded)".
	pub location: String,
	pub pack: Option<String>,
	/// False: the pack was skipped (bad `pack.json`, duplicate id).
	pub loaded: bool,
	pub items: usize,
	/// Items with problems are skipped; the rest of the pack loads.
	pub issues: Vec<Issue>,
}

impl Pack {
	/// Builds a pack from `(path inside the pack, contents)` pairs; every problem is
	/// reported and only valid items are kept. `None` when `pack.json` is unusable.
	pub fn from_files<'a>(
		origin: PackOrigin,
		files: impl IntoIterator<Item = (String, std::borrow::Cow<'a, str>)>,
	) -> (Option<Pack>, PackReport) {
		let location = match &origin {
			PackOrigin::Embedded => "(embedded)".to_owned(),
			PackOrigin::Dir(d) => d.display().to_string(),
		};
		let mut report = PackReport {
			location,
			pack: None,
			loaded: false,
			items: 0,
			issues: Vec::new(),
		};
		let mut files: Vec<(String, std::borrow::Cow<'a, str>)> = files.into_iter().collect();
		files.sort_by(|a, b| a.0.cmp(&b.0));
		let Some(manifest_text) = files.iter().find(|(p, _)| p == "pack.json").map(|(_, t)| t)
		else {
			report.issues.push(Issue {
				file: "pack.json".into(),
				message: "missing: a pack folder needs a pack.json manifest".into(),
			});
			return (None, report);
		};
		let manifest: PackManifest = match serde_json::from_str(manifest_text) {
			Ok(m) => m,
			Err(e) => {
				report.issues.push(Issue {
					file: "pack.json".into(),
					message: e.to_string(),
				});
				return (None, report);
			}
		};
		let problems = validate::manifest(&manifest);
		if !problems.is_empty() {
			report
				.issues
				.extend(problems.into_iter().map(|message| Issue {
					file: "pack.json".into(),
					message,
				}));
			return (None, report);
		}
		report.pack = Some(manifest.id.clone());
		let mut items = Vec::new();
		let mut seen = HashSet::new();
		for (path, text) in &files {
			let Some(folder) = path.split('/').next() else {
				continue;
			};
			let Some(expected) = ItemKind::ALL.into_iter().find(|k| k.folder() == folder) else {
				// pack.json, index files, midi/, kits/...
				continue;
			};
			if !path.ends_with(".json") {
				continue;
			}
			let mut issue = |message: String| {
				report.issues.push(Issue {
					file: path.clone(),
					message,
				})
			};
			let mut item: Item = match serde_json::from_str(text) {
				Ok(i) => i,
				Err(e) => {
					issue(e.to_string());
					continue;
				}
			};
			if item.kind() != expected {
				issue(format!(
					"is a {} but lives in {}/ (put it in {}/)",
					item.kind(),
					folder,
					item.kind().folder()
				));
				continue;
			}
			let slug = item.common().id.to_owned();
			let id = format!("{}/{slug}", manifest.id);
			if !seen.insert(slug.clone()) {
				issue(format!("duplicate id '{slug}' in this pack"));
				continue;
			}
			item.set_id(id.clone());
			let problems = validate::item(&item);
			if !problems.is_empty() {
				for p in problems {
					issue(format!("{id}: {p}"));
				}
				continue;
			}
			items.push(LoadedItem {
				id,
				pack: manifest.id.clone(),
				path: path.clone(),
				item,
			});
		}
		report.loaded = true;
		report.items = items.len();
		(
			Some(Pack {
				manifest,
				origin,
				items,
			}),
			report,
		)
	}

	/// The pack compiled into the binary.
	pub fn core() -> (Option<Pack>, PackReport) {
		Self::from_files(
			PackOrigin::Embedded,
			embedded::CORE_FILES
				.iter()
				.map(|(p, t)| ((*p).to_owned(), std::borrow::Cow::Borrowed(*t))),
		)
	}

	/// Loads a pack folder (`pack.json` + item folders).
	pub fn load_dir(dir: &Path) -> (Option<Pack>, PackReport) {
		let mut files = Vec::new();
		let mut errors = Vec::new();
		read_tree(dir, dir, &mut files, &mut errors);
		let (pack, mut report) = Self::from_files(
			PackOrigin::Dir(dir.to_path_buf()),
			files
				.into_iter()
				.map(|(p, t)| (p, std::borrow::Cow::Owned(t))),
		);
		report.issues.extend(errors);
		(pack, report)
	}
}

fn read_tree(root: &Path, dir: &Path, out: &mut Vec<(String, String)>, errors: &mut Vec<Issue>) {
	let Ok(entries) = std::fs::read_dir(dir) else {
		return;
	};
	for entry in entries.flatten() {
		let path = entry.path();
		let hidden = path
			.file_name()
			.and_then(|n| n.to_str())
			.is_some_and(|n| n.starts_with('.'));
		if hidden {
			continue;
		}
		if path.is_dir() {
			read_tree(root, &path, out, errors);
		} else if path.extension().is_some_and(|e| e == "json") {
			let rel = path
				.strip_prefix(root)
				.unwrap_or(&path)
				.components()
				.map(|c| c.as_os_str().to_string_lossy().into_owned())
				.collect::<Vec<_>>()
				.join("/");
			match std::fs::read_to_string(&path) {
				Ok(text) => out.push((rel, text)),
				Err(e) => errors.push(Issue {
					file: rel,
					message: e.to_string(),
				}),
			}
		}
	}
}

/// Every installed pack, searchable.
#[derive(Debug, Clone, Default)]
pub struct Library {
	packs: Vec<Pack>,
	reports: Vec<PackReport>,
}

/// Search filters; empty fields match everything.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Query {
	pub text: String,
	pub kind: Option<ItemKind>,
	pub genre: Option<String>,
	pub role: Option<Role>,
	pub section: Option<String>,
	pub energy: Option<u8>,
	pub family: Option<String>,
}

/// A search result: enough to choose an item.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Card {
	pub id: String,
	pub kind: ItemKind,
	pub name: String,
	pub summary: String,
	/// Compact key facts ("drums · 1 bar · energy 3 · 88-102 bpm").
	pub facts: String,
}

impl Library {
	/// Only the embedded `core` pack.
	pub fn core() -> Self {
		let mut lib = Self::default();
		let (pack, report) = Pack::core();
		lib.add(pack, report);
		lib
	}

	/// The `core` pack plus every pack folder inside `user_root` (each a folder with a
	/// `pack.json`); invalid packs and items are reported in [`Library::reports`].
	pub fn load(user_root: Option<&Path>) -> Self {
		let mut lib = Self::core();
		if let Some(root) = user_root
			&& let Ok(entries) = std::fs::read_dir(root)
		{
			let mut dirs: Vec<PathBuf> = entries
				.flatten()
				.map(|e| e.path())
				.filter(|p| p.join("pack.json").is_file())
				.collect();
			dirs.sort();
			for dir in dirs {
				let (pack, report) = Pack::load_dir(&dir);
				lib.add(pack, report);
			}
		}
		lib.check_references();
		lib
	}

	/// [`Library::load`] on [`default_library_dir`].
	pub fn load_default() -> Self {
		Self::load(Some(&default_library_dir()))
	}

	/// Adds a loaded pack (a pack whose id is taken is skipped and reported).
	pub fn add(&mut self, pack: Option<Pack>, mut report: PackReport) {
		if let Some(pack) = pack {
			let id = &pack.manifest.id;
			let reserved = id == CORE_PACK && pack.origin != PackOrigin::Embedded;
			if reserved || self.packs.iter().any(|p| p.manifest.id == *id) {
				report.loaded = false;
				report.items = 0;
				report.issues.push(Issue {
					file: "pack.json".into(),
					message: format!(
						"pack id '{id}' is already installed{}: rename it",
						if reserved { " (reserved)" } else { "" }
					),
				});
			} else {
				self.packs.push(pack);
			}
		}
		self.reports.push(report);
	}

	/// Reports references (style patterns/forms/loops, pairs_with, grooves) that do not
	/// resolve; the items stay loaded.
	pub fn check_references(&mut self) {
		let mut found: Vec<(usize, Issue)> = Vec::new();
		for (pi, pack) in self.packs.iter().enumerate() {
			for it in &pack.items {
				let mut want = |id: &str, kind: ItemKind, field: &str| match self.item(id) {
					Some(other) if other.item.kind() == kind => {}
					Some(other) => found.push((
						pi,
						Issue {
							file: it.path.clone(),
							message: format!(
								"{}: {field} '{id}' is a {}, expected a {kind}",
								it.id,
								other.item.kind()
							),
						},
					)),
					None => found.push((
						pi,
						Issue {
							file: it.path.clone(),
							message: format!("{}: {field} '{id}' is not installed", it.id),
						},
					)),
				};
				match &it.item {
					Item::Pattern(p) => {
						for id in &p.pairs_with {
							want(id, ItemKind::Pattern, "pairs_with");
						}
						if let Some(g) = &p.groove {
							want(g, ItemKind::Groove, "groove");
						}
					}
					Item::Style(s) => {
						for id in &s.forms {
							want(id, ItemKind::Form, "forms");
						}
						for id in &s.harmony.loops {
							want(id, ItemKind::Progression, "harmony.loops");
						}
						for (role, sections) in s.roles.iter() {
							for (section, value) in sections.iter() {
								if let RoleValue::Ids(ids) = value {
									for id in ids {
										want(
											id,
											ItemKind::Pattern,
											&format!("roles.{role}.{section}"),
										);
									}
								}
							}
						}
					}
					_ => {}
				}
			}
		}
		for (pi, issue) in found {
			let pack = &self.packs[pi].manifest.id;
			if let Some(r) = self
				.reports
				.iter_mut()
				.find(|r| r.pack.as_deref() == Some(pack) && r.loaded)
			{
				r.issues.push(issue);
			}
		}
	}

	pub fn packs(&self) -> &[Pack] {
		&self.packs
	}

	pub fn reports(&self) -> &[PackReport] {
		&self.reports
	}

	pub fn pack(&self, id: &str) -> Option<&Pack> {
		self.packs.iter().find(|p| p.manifest.id == id)
	}

	pub fn items(&self) -> impl Iterator<Item = &LoadedItem> {
		self.packs.iter().flat_map(|p| p.items.iter())
	}

	/// An item by id (`core/dembow_basic`); a bare slug finds the first pack having it.
	pub fn item(&self, id: &str) -> Option<&LoadedItem> {
		let id = id.trim();
		if id.contains('/') {
			self.items().find(|i| i.id == id)
		} else {
			self.items().find(|i| i.id.rsplit('/').next() == Some(id))
		}
	}

	pub fn pattern(&self, id: &str) -> Option<&Pattern> {
		self.item(id).and_then(|i| i.item.as_pattern())
	}

	/// The member of `pattern`'s family playing `variant` (same pack first).
	pub fn family_member(&self, pattern: &Pattern, variant: Variant) -> Option<&Pattern> {
		let family = pattern.family.as_deref()?;
		let pack = pattern.id.split('/').next().unwrap_or_default();
		let candidates = || {
			self.items()
				.filter_map(|i| i.item.as_pattern())
				.filter(|p| p.family.as_deref() == Some(family) && p.variant == variant)
				.filter(|p| p.id != pattern.id)
		};
		candidates()
			.find(|p| p.id.starts_with(&format!("{pack}/")))
			.or_else(|| candidates().next())
	}

	/// Items matching `query`, best first.
	pub fn search(&self, query: &Query) -> Vec<&LoadedItem> {
		let tokens: Vec<String> = words(&query.text)
			.into_iter()
			.filter(|w| !STOP_WORDS.contains(&w.as_str()))
			.collect();
		let joined = tokens.join("_");
		let genre = query.genre.as_deref().map(normalize);
		let section = query.section.as_deref().map(section_name);
		let family = query.family.as_deref().map(normalize);
		let mut scored: Vec<(f64, &LoadedItem)> = Vec::new();
		for it in self.items() {
			let common = it.item.common();
			let pattern = it.item.as_pattern();
			if query.kind.is_some_and(|k| k != it.item.kind()) {
				continue;
			}
			let slug = it.id.rsplit('/').next().unwrap_or_default();
			if let Some(g) = &genre {
				let matches = common.genres.iter().any(|x| x == g)
					|| (it.item.kind() == ItemKind::Style
						&& (slug == g || common.aliases.iter().any(|a| normalize(a) == *g)));
				if !matches {
					continue;
				}
			}
			if let Some(r) = query.role
				&& pattern.is_none_or(|p| p.role != r)
			{
				continue;
			}
			if let Some(f) = &family
				&& pattern.is_none_or(|p| p.family.as_deref() != Some(f.as_str()))
			{
				continue;
			}
			let mut score = 0.0;
			if let Some(s) = &section {
				let sections: &[String] = match &it.item {
					Item::Pattern(p) => &p.sections,
					Item::Progression(p) => &p.sections,
					_ => &[],
				};
				if sections.iter().any(|x| x == s) {
					score += 1.5;
				} else if sections.iter().any(|x| x == "any") {
					score += 0.5;
				} else {
					continue;
				}
			}
			if let Some(e) = query.energy {
				let Some(p) = pattern else {
					continue;
				};
				let d = (p.energy as i32 - e as i32).abs();
				if d > 1 {
					continue;
				}
				score += if d == 0 { 2.0 } else { 0.5 };
			}
			if !tokens.is_empty() {
				let mut text_score = 0.0;
				let name = words(common.name);
				let summary = words(common.summary);
				let aliases: Vec<String> = common.aliases.iter().flat_map(|a| words(a)).collect();
				for t in &tokens {
					let mut s = 0.0;
					if slug == t {
						s += 6.0;
					} else if slug.split('_').any(|w| w == t) {
						s += 3.0;
					} else if slug.contains(t.as_str()) {
						s += 1.5;
					}
					if name.contains(t) {
						s += 3.0;
					}
					if aliases.contains(t) {
						s += 3.0;
					}
					if common
						.genres
						.iter()
						.any(|g| g == t || g.split('_').any(|w| w == t))
					{
						s += 2.5;
					}
					if common.tags.iter().any(|g| g == t) {
						s += 2.0;
					}
					if pattern.is_some_and(|p| {
						p.family.as_deref() == Some(t.as_str()) || p.role.as_str() == t
					}) {
						s += 2.5;
					}
					if summary
						.iter()
						.any(|w| w == t || (t.len() >= 4 && w.starts_with(t.as_str())))
					{
						s += 1.0;
					}
					text_score += s;
				}
				if tokens.len() > 1
					&& (slug == joined
						|| common.aliases.iter().any(|a| normalize(a) == joined)
						|| common.genres.contains(&joined))
				{
					text_score += 6.0;
				}
				if text_score == 0.0 {
					continue;
				}
				score += text_score;
			}
			// Fills only when asked for: they are swapped in by fill_every.
			if pattern.is_some_and(|p| p.variant == Variant::Fill)
				&& !tokens.iter().any(|t| t == "fill" || t == "fills")
			{
				score -= 1.0;
			}
			scored.push((score, it));
		}
		scored.sort_by(|a, b| {
			b.0.total_cmp(&a.0)
				.then(a.1.item.kind().cmp(&b.1.item.kind()))
				.then(a.1.id.cmp(&b.1.id))
		});
		scored.into_iter().map(|(_, it)| it).collect()
	}
}

const STOP_WORDS: &[&str] = &[
	"a", "an", "the", "for", "with", "and", "of", "in", "on", "to", "de", "y", "el", "la", "con",
	"un", "una", "para", "song", "track", "pattern", "patterns",
];

/// Lower-case, accents folded, words joined by '_' ("Pop Latino" -> "pop_latino").
pub fn normalize(s: &str) -> String {
	words(s).join("_")
}

/// Lower-case words with accents folded.
pub fn words(s: &str) -> Vec<String> {
	let folded: String = s
		.chars()
		.map(|c| match c {
			'á' | 'à' | 'â' | 'ä' | 'Á' => 'a',
			'é' | 'è' | 'ê' | 'ë' | 'É' => 'e',
			'í' | 'ì' | 'î' | 'ï' | 'Í' => 'i',
			'ó' | 'ò' | 'ô' | 'ö' | 'Ó' => 'o',
			'ú' | 'ù' | 'û' | 'ü' | 'Ú' => 'u',
			'ñ' | 'Ñ' => 'n',
			c => c.to_ascii_lowercase(),
		})
		.collect();
	folded
		.split(|c: char| !c.is_ascii_alphanumeric())
		.filter(|w| !w.is_empty())
		.map(str::to_owned)
		.collect()
}

/// A section name as patterns list it: "Chorus 2" -> "chorus", "Pre-chorus" -> "pre".
pub fn section_name(s: &str) -> String {
	let w: Vec<String> = words(s)
		.into_iter()
		.filter(|w| !w.bytes().all(|b| b.is_ascii_digit()))
		.collect();
	let joined = w.join("_");
	match joined.as_str() {
		"pre_chorus" | "prechorus" | "pre" | "build" | "buildup" | "build_up" => "pre".into(),
		"post_chorus" | "postchorus" | "post" => "post_chorus".into(),
		"refrain" | "hook" | "estribillo" | "coro" => "chorus".into(),
		"verso" | "estrofa" => "verse".into(),
		"breakdown" => "break".into(),
		_ => joined,
	}
}

/// The card of an item.
pub fn card(it: &LoadedItem) -> Card {
	let c = it.item.common();
	Card {
		id: it.id.clone(),
		kind: it.item.kind(),
		name: c.name.to_owned(),
		summary: c.summary.to_owned(),
		facts: facts(&it.item),
	}
}

fn facts(item: &Item) -> String {
	let bars = |n: u32| {
		if n == 1 {
			"1 bar".to_owned()
		} else {
			format!("{n} bars")
		}
	};
	let tempo = |lo: f64, hi: f64| format!("{lo:.0}-{hi:.0} bpm");
	match item {
		Item::Pattern(p) => {
			let mut parts = vec![p.role.as_str().to_owned(), bars(p.bars)];
			parts.push(format!("energy {}", p.energy));
			if p.variant != Variant::Main {
				parts.push(p.variant.as_str().to_owned());
			}
			if let Some([lo, hi]) = p.tempo {
				parts.push(tempo(lo, hi));
			}
			if p.meter != "4/4" {
				parts.push(p.meter.clone());
			}
			parts.join(" · ")
		}
		Item::Style(s) => format!(
			"{} · {} · {}",
			tempo(s.tempo.min, s.tempo.max),
			s.feel,
			s.meter
		),
		Item::Form(f) => format!("{} sections · {}", f.sections.len(), bars(f.total_bars())),
		Item::Progression(p) => format!(
			"{} ({}) · {} beats/chord",
			p.numerals, p.mode, p.beats_per_chord
		),
		Item::Groove(g) => format!("{} steps/beat · {} steps", g.steps_per_beat, g.timing.len()),
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn normalizes_words_and_sections() {
		assert_eq!(normalize("Pop Latino"), "pop_latino");
		assert_eq!(normalize("Reggaetón"), "reggaeton");
		assert_eq!(section_name("Chorus 2"), "chorus");
		assert_eq!(section_name("Pre-Chorus"), "pre");
		assert_eq!(section_name("Post-chorus"), "post_chorus");
	}
}
