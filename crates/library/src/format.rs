//! File formats of a pack: `pack.json` and one JSON file per item (style, form, pattern,
//! progression, groove). Items are tagged by `kind`; their `id` in the file is the slug,
//! the library id is `<pack>/<slug>`.

use std::fmt;
use std::marker::PhantomData;

use cadenza_theory::VoicingStyle;
use serde::de::{MapAccess, Visitor};
use serde::ser::SerializeMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Format version of `pack.json`.
pub const PACK_FORMAT_VERSION: u32 = 1;

/// `pack.json`: who made the pack and under which terms it may be shared.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackManifest {
	pub format_version: u32,
	/// Lower-case slug (`[a-z0-9_]`), the prefix of every item id.
	pub id: String,
	pub name: String,
	pub version: String,
	/// SPDX license id ("CC0-1.0", "CC-BY-4.0", "MIT") or "LicenseRef-<name>".
	pub license: String,
	/// Credit line shown in the app (required by CC-BY licenses).
	#[serde(default, skip_serializing_if = "String::is_empty")]
	pub attribution: String,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub source: Option<String>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub homepage: Option<String>,
	#[serde(default, skip_serializing_if = "String::is_empty")]
	pub description: String,
	/// False: non-commercial or unclear terms; the user may install it, it is never bundled.
	pub redistributable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemKind {
	Style,
	Form,
	Pattern,
	Progression,
	Groove,
}

impl ItemKind {
	pub const ALL: [ItemKind; 5] = [
		ItemKind::Style,
		ItemKind::Form,
		ItemKind::Pattern,
		ItemKind::Progression,
		ItemKind::Groove,
	];

	pub fn as_str(self) -> &'static str {
		match self {
			ItemKind::Style => "style",
			ItemKind::Form => "form",
			ItemKind::Pattern => "pattern",
			ItemKind::Progression => "progression",
			ItemKind::Groove => "groove",
		}
	}

	pub fn parse(s: &str) -> Option<Self> {
		Self::ALL
			.into_iter()
			.find(|k| k.as_str() == s.trim().to_lowercase())
	}

	/// Largest compact JSON size of an item of this kind (`library_get` budget).
	pub fn max_detail_chars(self) -> usize {
		match self {
			ItemKind::Style => 3500,
			ItemKind::Form => 2000,
			ItemKind::Pattern => 1200,
			ItemKind::Progression => 600,
			ItemKind::Groove => 1200,
		}
	}

	/// The folder of a pack holding items of this kind.
	pub fn folder(self) -> &'static str {
		match self {
			ItemKind::Style => "styles",
			ItemKind::Form => "forms",
			ItemKind::Pattern => "patterns",
			ItemKind::Progression => "progressions",
			ItemKind::Groove => "grooves",
		}
	}
}

impl fmt::Display for ItemKind {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.write_str(self.as_str())
	}
}

/// Card summaries (search results) are at most this long.
pub const MAX_SUMMARY_CHARS: usize = 120;

/// One item file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Item {
	Style(Style),
	Form(Form),
	Pattern(Pattern),
	Progression(Progression),
	Groove(Groove),
}

impl Item {
	pub fn kind(&self) -> ItemKind {
		match self {
			Item::Style(_) => ItemKind::Style,
			Item::Form(_) => ItemKind::Form,
			Item::Pattern(_) => ItemKind::Pattern,
			Item::Progression(_) => ItemKind::Progression,
			Item::Groove(_) => ItemKind::Groove,
		}
	}

	pub fn common(&self) -> Common<'_> {
		match self {
			Item::Style(s) => Common {
				id: &s.id,
				name: &s.name,
				summary: &s.summary,
				genres: &s.genres,
				tags: &s.tags,
				aliases: &s.aliases,
			},
			Item::Form(s) => Common {
				id: &s.id,
				name: &s.name,
				summary: &s.summary,
				genres: &s.genres,
				tags: &s.tags,
				aliases: &[],
			},
			Item::Pattern(s) => Common {
				id: &s.id,
				name: &s.name,
				summary: &s.summary,
				genres: &s.genres,
				tags: &s.tags,
				aliases: &[],
			},
			Item::Progression(s) => Common {
				id: &s.id,
				name: &s.name,
				summary: &s.summary,
				genres: &s.genres,
				tags: &s.tags,
				aliases: &[],
			},
			Item::Groove(s) => Common {
				id: &s.id,
				name: &s.name,
				summary: &s.summary,
				genres: &s.genres,
				tags: &s.tags,
				aliases: &[],
			},
		}
	}

	pub(crate) fn set_id(&mut self, id: String) {
		match self {
			Item::Style(s) => s.id = id,
			Item::Form(s) => s.id = id,
			Item::Pattern(s) => s.id = id,
			Item::Progression(s) => s.id = id,
			Item::Groove(s) => s.id = id,
		}
	}

	pub fn as_pattern(&self) -> Option<&Pattern> {
		match self {
			Item::Pattern(p) => Some(p),
			_ => None,
		}
	}
}

/// Fields every item has.
#[derive(Debug, Clone, Copy)]
pub struct Common<'a> {
	pub id: &'a str,
	pub name: &'a str,
	pub summary: &'a str,
	pub genres: &'a [String],
	pub tags: &'a [String],
	pub aliases: &'a [String],
}

/// A map that keeps the order of its JSON object (lanes and roles read top to bottom).
#[derive(Debug, Clone, PartialEq)]
pub struct OrderedMap<V>(pub Vec<(String, V)>);

impl<V> Default for OrderedMap<V> {
	fn default() -> Self {
		Self(Vec::new())
	}
}

impl<V> OrderedMap<V> {
	pub fn get(&self, key: &str) -> Option<&V> {
		self.0.iter().find(|(k, _)| k == key).map(|(_, v)| v)
	}

	pub fn iter(&self) -> impl Iterator<Item = (&str, &V)> {
		self.0.iter().map(|(k, v)| (k.as_str(), v))
	}

	pub fn keys(&self) -> impl Iterator<Item = &str> {
		self.0.iter().map(|(k, _)| k.as_str())
	}

	pub fn len(&self) -> usize {
		self.0.len()
	}

	pub fn is_empty(&self) -> bool {
		self.0.is_empty()
	}
}

impl<V: Serialize> Serialize for OrderedMap<V> {
	fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
		let mut map = serializer.serialize_map(Some(self.0.len()))?;
		for (k, v) in &self.0 {
			map.serialize_entry(k, v)?;
		}
		map.end()
	}
}

impl<'de, V: Deserialize<'de>> Deserialize<'de> for OrderedMap<V> {
	fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
		struct MapVisitor<V>(PhantomData<V>);
		impl<'de, V: Deserialize<'de>> Visitor<'de> for MapVisitor<V> {
			type Value = OrderedMap<V>;
			fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
				f.write_str("a JSON object")
			}
			fn visit_map<A: MapAccess<'de>>(self, mut access: A) -> Result<Self::Value, A::Error> {
				let mut out: Vec<(String, V)> = Vec::new();
				while let Some((k, v)) = access.next_entry::<String, V>()? {
					if out.iter().any(|(e, _)| *e == k) {
						return Err(serde::de::Error::custom(format!("duplicate key '{k}'")));
					}
					out.push((k, v));
				}
				Ok(OrderedMap(out))
			}
		}
		deserializer.deserialize_map(MapVisitor(PhantomData))
	}
}

/// What a pattern plays.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
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

impl Role {
	pub const ALL: [Role; 8] = [
		Role::Drums,
		Role::Perc,
		Role::Bass,
		Role::Comping,
		Role::Arp,
		Role::Pad,
		Role::Lead,
		Role::Fill,
	];

	pub fn as_str(self) -> &'static str {
		match self {
			Role::Drums => "drums",
			Role::Perc => "perc",
			Role::Bass => "bass",
			Role::Comping => "comping",
			Role::Arp => "arp",
			Role::Pad => "pad",
			Role::Lead => "lead",
			Role::Fill => "fill",
		}
	}

	pub fn parse(s: &str) -> Option<Self> {
		let s = s.trim().to_lowercase();
		if s == "hook" {
			return Some(Role::Lead);
		}
		Self::ALL.into_iter().find(|r| r.as_str() == s)
	}

	/// Center of the register chord- and scale-relative lanes are placed around: the
	/// chord root (or key tonic) lands in `[center - 6, center + 5]` (bass: C2..B2).
	pub fn center(self) -> i32 {
		match self {
			Role::Bass => 42,
			Role::Arp => 64,
			Role::Lead => 72,
			_ => 60,
		}
	}

	/// Unpitched roles: lanes are drum names.
	pub fn is_percussive(self) -> bool {
		matches!(self, Role::Drums | Role::Perc | Role::Fill)
	}
}

impl fmt::Display for Role {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.write_str(self.as_str())
	}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Variant {
	#[default]
	Main,
	Variation,
	Fill,
	Intro,
	Ending,
}

impl Variant {
	pub fn as_str(self) -> &'static str {
		match self {
			Variant::Main => "main",
			Variant::Variation => "variation",
			Variant::Fill => "fill",
			Variant::Intro => "intro",
			Variant::Ending => "ending",
		}
	}
}

/// How a pattern's pitched lanes are interpreted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Harmony {
	/// Drums and percussion: lanes are drum names or absolute notes.
	#[default]
	None,
	/// Chord tones (R 3 5 7..., chord, bass, app, ant): needs chords.
	Chord,
	/// Scale degrees (^1..^7): needs a key.
	Scale,
	/// Absolute notes, written in `key` and transposed to the target key.
	Absolute,
}

fn is_default<T: Default + PartialEq>(v: &T) -> bool {
	*v == T::default()
}

fn default_meter() -> String {
	"4/4".into()
}

fn is_default_meter(m: &str) -> bool {
	m == "4/4"
}

fn default_feel() -> String {
	"straight".into()
}

fn is_straight(f: &str) -> bool {
	f == "straight"
}

fn default_steps() -> u32 {
	4
}

fn is_four(v: &u32) -> bool {
	*v == 4
}

/// A MIDI pattern for one role, in step-grid notation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pattern {
	pub id: String,
	pub name: String,
	pub summary: String,
	pub role: Role,
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub genres: Vec<String>,
	/// Song sections it suits ("intro", "verse", "pre", "chorus", "bridge", "outro", "any").
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub sections: Vec<String>,
	/// 1 (sparse) ..= 5 (full).
	pub energy: u8,
	/// "straight" | "swing:<0..1>" | "triplet".
	#[serde(default = "default_feel", skip_serializing_if = "is_straight")]
	pub feel: String,
	#[serde(default = "default_meter", skip_serializing_if = "is_default_meter")]
	pub meter: String,
	pub bars: u32,
	/// Suggested tempo range in bpm.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub tempo: Option<[f64; 2]>,
	/// Patterns of one family share a groove; `variant` says what this one is for.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub family: Option<String>,
	#[serde(default, skip_serializing_if = "is_default")]
	pub variant: Variant,
	#[serde(default, skip_serializing_if = "is_default")]
	pub harmony: Harmony,
	/// Key an `absolute` pattern is written in ("A minor").
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub key: Option<String>,
	/// Voicing of `chord` lanes (default close).
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub voicing: Option<VoicingStyle>,
	/// Octave shift of the role's register (an 808 bass: -1).
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub octave: Option<i32>,
	/// Grid steps per quarter note.
	#[serde(default = "default_steps", skip_serializing_if = "is_four")]
	pub steps_per_beat: u32,
	/// Lane key -> step string (`X` accent, `x` normal, `o` ghost, `=` tie, `.`/`-` rest).
	pub lanes: OrderedMap<String>,
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub pairs_with: Vec<String>,
	/// Default groove id.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub groove: Option<String>,
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub tags: Vec<String>,
}

/// A role entry of a style: pattern ids per section, or a short note ("sound", "register").
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RoleValue {
	Ids(Vec<String>),
	Text(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TempoRange {
	pub min: f64,
	pub max: f64,
	pub typical: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StyleHarmony {
	/// Progression ids.
	pub loops: Vec<String>,
	#[serde(default = "four")]
	pub harmonic_rhythm_beats: f64,
	#[serde(default, skip_serializing_if = "String::is_empty")]
	pub notes: String,
}

fn four() -> f64 {
	4.0
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StyleMix {
	pub lufs: f64,
	#[serde(default, skip_serializing_if = "String::is_empty")]
	pub notes: String,
}

/// A genre style guide: everything needed to plan a song in one document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Style {
	pub id: String,
	pub name: String,
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub aliases: Vec<String>,
	pub summary: String,
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub genres: Vec<String>,
	pub tempo: TempoRange,
	#[serde(default = "default_feel")]
	pub feel: String,
	#[serde(default = "default_meter")]
	pub meter: String,
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub keys: Vec<String>,
	pub harmony: StyleHarmony,
	/// Form ids.
	pub forms: Vec<String>,
	/// Arrangement role ("drums", "guitar", "keys", "hook"...) -> section -> pattern ids
	/// (or a note: "sound", "register", "notes", "fill").
	pub roles: OrderedMap<OrderedMap<RoleValue>>,
	#[serde(default, skip_serializing_if = "OrderedMap::is_empty")]
	pub sounds: OrderedMap<RoleValue>,
	pub mix: StyleMix,
	#[serde(rename = "do", default, skip_serializing_if = "Vec::is_empty")]
	pub dos: Vec<String>,
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub dont: Vec<String>,
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub tags: Vec<String>,
}

/// What happens in the last bar of a section.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Ending {
	Drop,
	Fill,
	Riser,
	None,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Section {
	pub name: String,
	pub bars: u32,
	pub energy: u8,
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub roles: Vec<String>,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub ending: Option<Ending>,
	/// Harmony label ("loop", "IV-led", a progression id).
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub harmony: Option<String>,
}

/// A song structure.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Form {
	pub id: String,
	pub name: String,
	pub summary: String,
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub genres: Vec<String>,
	pub sections: Vec<Section>,
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub tags: Vec<String>,
}

impl Form {
	pub fn total_bars(&self) -> u32 {
		self.sections.iter().map(|s| s.bars).sum()
	}
}

/// A named chord progression in roman numerals.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Progression {
	pub id: String,
	pub name: String,
	pub summary: String,
	/// Scale kind the numerals are read in ("major", "minor", "dorian"...).
	pub mode: String,
	/// "i VI III VII".
	pub numerals: String,
	#[serde(default = "four")]
	pub beats_per_chord: f64,
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub genres: Vec<String>,
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub sections: Vec<String>,
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub tags: Vec<String>,
}

/// A timing/velocity template applied per grid step (cycled).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Groove {
	pub id: String,
	pub name: String,
	pub summary: String,
	#[serde(default = "default_steps")]
	pub steps_per_beat: u32,
	/// Offset of each step in fractions of a step (-0.5..=0.5; positive = late).
	pub timing: Vec<f64>,
	/// Velocity multiplier of each step (0..=2); empty = unchanged.
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub velocity: Vec<f64>,
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub genres: Vec<String>,
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub tags: Vec<String>,
}

/// The feel of a pattern.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Feel {
	Straight,
	/// 0..=1: delays every second step by up to half a step.
	Swing(f64),
	/// Descriptive: the grid itself is triplet (steps_per_beat 3 or 6).
	Triplet,
}

impl Feel {
	pub fn parse(s: &str) -> Option<Self> {
		let s = s.trim();
		match s {
			"straight" => Some(Feel::Straight),
			"triplet" => Some(Feel::Triplet),
			_ => {
				let amount: f64 = s.strip_prefix("swing:")?.trim().parse().ok()?;
				(0.0..=1.0).contains(&amount).then_some(Feel::Swing(amount))
			}
		}
	}
}

/// Parses "4/4", "6/8", "3/4" into (numerator, denominator).
pub fn parse_meter(s: &str) -> Option<(u32, u32)> {
	let (n, d) = s.trim().split_once('/')?;
	let n: u32 = n.trim().parse().ok()?;
	let d: u32 = d.trim().parse().ok()?;
	((1..=32).contains(&n) && matches!(d, 1 | 2 | 4 | 8 | 16)).then_some((n, d))
}

/// Quarter notes in one bar of `meter`.
pub fn bar_beats(meter: (u32, u32)) -> f64 {
	meter.0 as f64 * 4.0 / meter.1 as f64
}

impl Pattern {
	/// Grid steps in one bar (`None` when the meter is invalid or the bar is not a whole
	/// number of steps).
	pub fn steps_per_bar(&self) -> Option<usize> {
		let beats = bar_beats(parse_meter(&self.meter)?);
		let steps = beats * self.steps_per_beat as f64;
		(steps.fract() == 0.0 && steps >= 1.0).then_some(steps as usize)
	}

	/// Quarter notes in one bar.
	pub fn bar_beats(&self) -> f64 {
		parse_meter(&self.meter).map(bar_beats).unwrap_or(4.0)
	}
}
