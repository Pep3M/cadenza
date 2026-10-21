//! UI translations: gettext-style lookups keyed by the English source text.
//!
//! - [`tr`] translates a string literal (`tr("Save As…")`); [`trf`] fills named
//!   placeholders (`trf("{n} undo steps", &[("n", &n)])`); [`trn`] / [`trn_with`] pick the
//!   plural form (`trn("{n} track", "{n} tracks", n)`); [`mark`] tags a literal in a const
//!   table so the key checker finds it, and the table is translated with `tr` where drawn.
//! - English needs no file. Every other language is `locales/<code>.toml` (English key =
//!   translation, or an array of plural forms in CLDR order), compiled by `build.rs` into a
//!   sorted static table: a lookup is a binary search, no allocation and no parsing at run
//!   time. A missing entry falls back to English.
//! - The active language is a process-wide atomic (the app) with a per-thread override
//!   (headless states and tests, so parallel tests never see each other's language).
//! - Tests scan the sources for every `tr`/`trf`/`trn`/`trn_with`/`mark` literal: each must
//!   be in every language file, no file may hold unused keys, and placeholders must match.

use std::cell::Cell;
use std::fmt::Display;
use std::sync::atomic::{AtomicU8, Ordering};

pub mod external;

mod tables {
	include!(concat!(env!("OUT_DIR"), "/locales.rs"));
}

/// A UI language.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Language {
	English,
	Spanish,
	PortugueseBr,
	French,
	German,
	Italian,
	Russian,
	Japanese,
	ChineseSimplified,
	Korean,
}

impl Language {
	pub const ALL: [Language; 10] = [
		Language::English,
		Language::Spanish,
		Language::PortugueseBr,
		Language::French,
		Language::German,
		Language::Italian,
		Language::Russian,
		Language::Japanese,
		Language::ChineseSimplified,
		Language::Korean,
	];

	/// BCP 47 primary subtag (matches the OS locale and round-trips through
	/// [`LanguageChoice`]); the `locales/<code>.toml` file stem carries a region suffix
	/// when a variant needs one to stay unambiguous (`pt-BR.toml`).
	pub fn code(self) -> &'static str {
		match self {
			Language::English => "en",
			Language::Spanish => "es",
			Language::PortugueseBr => "pt",
			Language::French => "fr",
			Language::German => "de",
			Language::Italian => "it",
			Language::Russian => "ru",
			Language::Japanese => "ja",
			Language::ChineseSimplified => "zh",
			Language::Korean => "ko",
		}
	}

	/// The language's own name (shown untranslated in the picker).
	pub fn native_name(self) -> &'static str {
		match self {
			Language::English => "English",
			Language::Spanish => "Español",
			Language::PortugueseBr => "Português (Brasil)",
			Language::French => "Français",
			Language::German => "Deutsch",
			Language::Italian => "Italiano",
			Language::Russian => "Русский",
			Language::Japanese => "日本語",
			Language::ChineseSimplified => "简体中文",
			Language::Korean => "한국어",
		}
	}

	/// Whether the UI needs a system CJK font loaded as a fallback (see
	/// `theme::find_cjk_fonts`): Japanese, Simplified Chinese and Korean do (Cyrillic, in
	/// `Spanish`'s Latin family and `ru`, is already covered by IBM Plex Sans, but CJK
	/// ideographs are not).
	pub fn needs_cjk_fallback(self) -> bool {
		matches!(
			self,
			Language::Japanese | Language::ChineseSimplified | Language::Korean
		)
	}

	pub fn from_code(code: &str) -> Option<Language> {
		let primary = code
			.split(['-', '_', '.', '@'])
			.next()
			.unwrap_or("")
			.to_ascii_lowercase();
		Language::ALL.into_iter().find(|l| l.code() == primary)
	}

	fn table(self) -> &'static [(&'static str, &'static [&'static str])] {
		match self {
			Language::English => &[],
			Language::Spanish => tables::ES,
			Language::PortugueseBr => tables::PT_BR,
			Language::French => tables::FR,
			Language::German => tables::DE,
			Language::Italian => tables::IT,
			Language::Russian => tables::RU,
			Language::Japanese => tables::JA,
			Language::ChineseSimplified => tables::ZH_CN,
			Language::Korean => tables::KO,
		}
	}

	/// Index of the plural form for `n` (CLDR cardinal rules: Spanish, English and German
	/// have only "one" at exactly 1; French and Portuguese also use "one" at 0; Italian
	/// matches English/German (one at 1). Russian has three forms — one/few/many — on the
	/// last two digits: "one" when it ends in 1 but not 11 (1, 21, 101…), "few" when it ends
	/// in 2-4 but not 12-14 (2, 3, 4, 22…), "many" otherwise (0, 5-20, 25...). Japanese,
	/// Chinese and Korean have a single CLDR cardinal form ("other") for every `n`.
	pub fn plural_index(self, n: u64) -> usize {
		match self {
			Language::English | Language::Spanish | Language::German | Language::Italian => {
				usize::from(n != 1)
			}
			Language::PortugueseBr | Language::French => usize::from(n > 1),
			Language::Russian => {
				let last_two = n % 100;
				let last_one = n % 10;
				if last_one == 1 && last_two != 11 {
					0
				} else if (2..=4).contains(&last_one) && !(12..=14).contains(&last_two) {
					1
				} else {
					2
				}
			}
			Language::Japanese | Language::ChineseSimplified | Language::Korean => 0,
		}
	}

	/// How many plural forms the language's files carry.
	pub fn plural_forms(self) -> usize {
		match self {
			Language::English
			| Language::Spanish
			| Language::PortugueseBr
			| Language::French
			| Language::German
			| Language::Italian => 2,
			Language::Russian => 3,
			Language::Japanese | Language::ChineseSimplified | Language::Korean => 1,
		}
	}

	/// Whether numbers use a decimal comma ("1,5 dB"). Japanese, Chinese and Korean use a
	/// decimal point like English.
	pub fn decimal_comma(self) -> bool {
		matches!(
			self,
			Language::Spanish
				| Language::PortugueseBr
				| Language::French
				| Language::German
				| Language::Italian
				| Language::Russian
		)
	}

	fn index(self) -> u8 {
		match self {
			Language::English => 0,
			Language::Spanish => 1,
			Language::PortugueseBr => 2,
			Language::French => 3,
			Language::German => 4,
			Language::Italian => 5,
			Language::Russian => 6,
			Language::Japanese => 7,
			Language::ChineseSimplified => 8,
			Language::Korean => 9,
		}
	}

	fn from_index(i: u8) -> Language {
		match i {
			1 => Language::Spanish,
			2 => Language::PortugueseBr,
			3 => Language::French,
			4 => Language::German,
			5 => Language::Italian,
			6 => Language::Russian,
			7 => Language::Japanese,
			8 => Language::ChineseSimplified,
			9 => Language::Korean,
			_ => Language::English,
		}
	}

	fn lookup(self, key: &str) -> Option<&'static [&'static str]> {
		let table = self.table();
		table
			.binary_search_by(|(k, _)| (*k).cmp(key))
			.ok()
			.map(|i| table[i].1)
	}
}

/// The language preference saved in `Prefs.language`: the OS language or a fixed one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(from = "String", into = "String")]
pub enum LanguageChoice {
	#[default]
	System,
	Fixed(Language),
}

impl LanguageChoice {
	/// The language this choice resolves to on this machine.
	pub fn resolve(self) -> Language {
		match self {
			LanguageChoice::System => system_language(),
			LanguageChoice::Fixed(l) => l,
		}
	}

	/// Every choice, in picker order.
	pub fn all() -> impl Iterator<Item = LanguageChoice> {
		std::iter::once(LanguageChoice::System).chain(Language::ALL.map(LanguageChoice::Fixed))
	}

	/// The picker label ("System default (Español)", "English").
	pub fn label(self) -> String {
		match self {
			LanguageChoice::System => trf(
				"System default ({language})",
				&[("language", &system_language().native_name())],
			),
			LanguageChoice::Fixed(l) => l.native_name().to_string(),
		}
	}
}

impl From<String> for LanguageChoice {
	fn from(s: String) -> Self {
		Language::from_code(&s).map_or(LanguageChoice::System, LanguageChoice::Fixed)
	}
}

impl From<LanguageChoice> for String {
	fn from(c: LanguageChoice) -> Self {
		match c {
			LanguageChoice::System => "system".into(),
			LanguageChoice::Fixed(l) => l.code().into(),
		}
	}
}

/// The OS UI language when it is one we have, else English.
pub fn system_language() -> Language {
	let env = ["LC_ALL", "LC_MESSAGES", "LANG"]
		.into_iter()
		.filter_map(|v| std::env::var(v).ok())
		.find(|v| !v.is_empty() && v != "C" && v != "POSIX");
	env.or_else(sys_locale::get_locale)
		.and_then(|l| Language::from_code(&l))
		.unwrap_or(Language::English)
}

static GLOBAL: AtomicU8 = AtomicU8::new(0);

thread_local! {
	static LOCAL: Cell<Option<Language>> = const { Cell::new(None) };
}

/// The language UI text is drawn in on this thread.
pub fn current() -> Language {
	LOCAL
		.with(Cell::get)
		.unwrap_or_else(|| Language::from_index(GLOBAL.load(Ordering::Relaxed)))
}

/// Sets the language of the whole process (the app).
pub fn set_global(lang: Language) {
	GLOBAL.store(lang.index(), Ordering::Relaxed);
}

/// Overrides the language on this thread only (headless states and tests).
pub fn set_thread(lang: Option<Language>) {
	LOCAL.with(|l| l.set(lang));
}

/// Marks a literal for the key checker without translating it (const tables); translate
/// it with [`tr`] where it is drawn.
pub const fn mark(s: &'static str) -> &'static str {
	s
}

/// The translation of an English UI string (itself when untranslated).
pub fn tr(s: &'static str) -> &'static str {
	tr_in(current(), s)
}

/// [`tr`] for a string that is not a literal here (const tables of [`mark`]ed names,
/// names from other crates): the translation when there is one, else `s` itself.
pub fn tr_str(s: &str) -> &str {
	match current() {
		Language::English => s,
		lang => lang.lookup(s).map_or(s, |forms| forms[0]),
	}
}

/// [`tr`] for a given language.
pub fn tr_in(lang: Language, s: &'static str) -> &'static str {
	match lang {
		Language::English => s,
		_ => lang.lookup(s).map_or(s, |forms| forms[0]),
	}
}

/// Translates `s` and fills its `{name}` placeholders.
pub fn trf(s: &'static str, args: &[(&str, &dyn Display)]) -> String {
	fill(tr(s), args)
}

/// The plural form of `one`/`other` for `n`, with `{n}` filled.
pub fn trn(
	one: &'static str,
	other: &'static str,
	n: impl TryInto<u64> + Display + Copy,
) -> String {
	trn_with(one, other, n, &[])
}

/// [`trn`] with more placeholders.
pub fn trn_with(
	one: &'static str,
	other: &'static str,
	n: impl TryInto<u64> + Display + Copy,
	args: &[(&str, &dyn Display)],
) -> String {
	let lang = current();
	let count = n.try_into().unwrap_or(u64::MAX);
	let template = match lang.lookup(one).filter(|_| lang != Language::English) {
		Some(forms) => forms[lang.plural_index(count).min(forms.len() - 1)],
		None if count == 1 => one,
		None => other,
	};
	let mut all: Vec<(&str, &dyn Display)> = Vec::with_capacity(args.len() + 1);
	all.push(("n", &n));
	all.extend_from_slice(args);
	fill(template, &all)
}

/// Replaces each `{name}` in `template` with its argument (unknown names stay).
pub fn fill(template: &str, args: &[(&str, &dyn Display)]) -> String {
	use std::fmt::Write as _;
	let mut out = String::with_capacity(template.len() + 16);
	let mut rest = template;
	while let Some(open) = rest.find('{') {
		out.push_str(&rest[..open]);
		let after = &rest[open + 1..];
		match after.find('}') {
			Some(close) => {
				let name = &after[..close];
				match args.iter().find(|(k, _)| *k == name) {
					Some((_, v)) => {
						let _ = write!(out, "{v}");
					}
					None => {
						out.push('{');
						out.push_str(name);
						out.push('}');
					}
				}
				rest = &after[close + 1..];
			}
			None => {
				out.push_str(&rest[open..]);
				rest = "";
			}
		}
	}
	out.push_str(rest);
	out
}

/// Localizes the decimal points of an already formatted number ("-3.5 dB" -> "-3,5 dB"
/// in Spanish). Only a '.' between two digits changes, so "1.2.3" positions are left to
/// callers that never pass them here.
pub fn decimal(s: String) -> String {
	if !current().decimal_comma() || !s.contains('.') {
		return s;
	}
	let b = s.as_bytes();
	let mut out = String::with_capacity(s.len());
	for (i, c) in s.char_indices() {
		let between = c == '.'
			&& i > 0 && b[i - 1].is_ascii_digit()
			&& b.get(i + 1).is_some_and(u8::is_ascii_digit);
		out.push(if between { ',' } else { c });
	}
	out
}

/// `value` with `decimals` places and the locale's decimal separator.
pub fn num(value: f64, decimals: usize) -> String {
	decimal(format!("{value:.decimals$}"))
}

/// Typed numbers accept a decimal comma as well as a point ("2,5" = 2.5): a ',' between
/// digits becomes '.' when the text has no '.' of its own.
pub fn delocalize(text: &str) -> std::borrow::Cow<'_, str> {
	if text.contains('.') || !text.contains(',') {
		return std::borrow::Cow::Borrowed(text);
	}
	let b = text.as_bytes();
	let mut out = String::with_capacity(text.len());
	for (i, c) in text.char_indices() {
		let between = c == ','
			&& b.get(i + 1).is_some_and(u8::is_ascii_digit)
			&& (i == 0 || b[i - 1].is_ascii_digit() || b[i - 1] == b'-' || b[i - 1] == b' ');
		out.push(if between { '.' } else { c });
	}
	std::borrow::Cow::Owned(out)
}

/// Every translated string of a language, for coverage checks (fonts, layout).
pub fn all_translations(lang: Language) -> impl Iterator<Item = &'static str> {
	lang.table()
		.iter()
		.flat_map(|(_, forms)| forms.iter().copied())
}

#[cfg(test)]
pub(crate) mod keys {
	//! Source scanner for the translation keys (`tr`, `trf`, `trn`, `trn_with`, `mark`).

	use std::collections::BTreeMap;
	use std::path::Path;

	/// A key found in the sources: its English text and, for plurals, the English
	/// `other` form.
	#[derive(Debug, Clone)]
	pub struct Key {
		pub other: Option<String>,
		pub file: String,
	}

	pub fn source_keys() -> BTreeMap<String, Key> {
		let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
		let mut keys = BTreeMap::new();
		let mut stack = vec![dir];
		while let Some(d) = stack.pop() {
			for e in std::fs::read_dir(&d).unwrap().flatten() {
				let p = e.path();
				if p.is_dir() {
					stack.push(p);
				} else if p.extension().is_some_and(|x| x == "rs")
					&& p.file_name().is_some_and(|n| n != "i18n.rs")
				{
					let text = std::fs::read_to_string(&p).unwrap();
					scan(&text, &p.display().to_string(), &mut keys);
				}
			}
		}
		keys
	}

	fn scan(text: &str, file: &str, keys: &mut BTreeMap<String, Key>) {
		const CALLS: [&str; 5] = ["trn_with(", "trn(", "trf(", "tr(", "mark("];
		let bytes = text.as_bytes();
		let mut i = 0;
		while i < bytes.len() {
			let Some(call) = CALLS.iter().find(|c| bytes[i..].starts_with(c.as_bytes())) else {
				i += 1;
				continue;
			};
			let ident_before = i > 0 && {
				let b = bytes[i - 1];
				b.is_ascii_alphanumeric() || b == b'_'
			};
			let start = i + call.len();
			i = start;
			if ident_before {
				continue;
			}
			let Some((first, end)) = literal(text, start) else {
				continue;
			};
			let mut other = None;
			if call.starts_with("trn") {
				let after = text[end..].trim_start();
				if let Some(rest) = after.strip_prefix(',') {
					let at = text.len() - rest.len();
					other = literal(text, at).map(|(s, _)| s);
				}
			}
			keys.entry(first).or_insert(Key {
				other,
				file: file.to_string(),
			});
			i = end;
		}
	}

	/// The string literal starting at `at` (after whitespace), unescaped, and the index
	/// after its closing quote.
	fn literal(text: &str, at: usize) -> Option<(String, usize)> {
		let rest = &text[at..];
		let skipped = rest.len() - rest.trim_start().len();
		let mut chars = rest[skipped..].char_indices();
		if chars.next()?.1 != '"' {
			return None;
		}
		let mut out = String::new();
		while let Some((j, c)) = chars.next() {
			match c {
				'"' => return Some((out, at + skipped + j + 1)),
				'\\' => match chars.next()?.1 {
					'n' => out.push('\n'),
					't' => out.push('\t'),
					'"' => out.push('"'),
					'\'' => out.push('\''),
					'\\' => out.push('\\'),
					'0' => out.push('\0'),
					'u' => {
						let mut hex = String::new();
						for (_, h) in chars.by_ref() {
							match h {
								'{' => {}
								'}' => break,
								h => hex.push(h),
							}
						}
						out.push(char::from_u32(u32::from_str_radix(&hex, 16).ok()?)?);
					}
					'\n' => {
						// Line continuation: skip the next line's leading whitespace.
						while chars.clone().next().is_some_and(|(_, w)| w.is_whitespace()) {
							chars.next();
						}
					}
					other => out.push(other),
				},
				c => out.push(c),
			}
		}
		None
	}

	pub fn placeholders(s: &str) -> std::collections::BTreeSet<&str> {
		let mut set = std::collections::BTreeSet::new();
		let mut rest = s;
		while let Some(open) = rest.find('{') {
			let after = &rest[open + 1..];
			let Some(close) = after.find('}') else { break };
			set.insert(&after[..close]);
			rest = &after[close + 1..];
		}
		set
	}

	#[test]
	fn scanner_reads_calls_and_escapes() {
		let mut keys = BTreeMap::new();
		scan(
			"a(tr(\"Save\")); trn(\n\t\"{n} clip\",\n\t\"{n} clips\", n); attr(\"no\"); mark(\"Say \\\"hi\\\"\\u{2026}\")",
			"x",
			&mut keys,
		);
		assert_eq!(
			keys.keys().map(String::as_str).collect::<Vec<_>>(),
			["Save", "Say \"hi\"…", "{n} clip"]
		);
		assert_eq!(keys["{n} clip"].other.as_deref(), Some("{n} clips"));
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn lookups_fall_back_to_english_and_follow_the_thread_language() {
		set_thread(Some(Language::English));
		assert_eq!(tr("Save"), "Save");
		assert_eq!(trn("{n} track", "{n} tracks", 1), "1 track");
		assert_eq!(trn("{n} track", "{n} tracks", 3), "3 tracks");
		set_thread(Some(Language::Spanish));
		assert_eq!(tr("Save"), "Guardar");
		assert_eq!(tr("not a key, stays as is"), "not a key, stays as is");
		assert_eq!(trn("{n} track", "{n} tracks", 1), "1 pista");
		assert_eq!(trn("{n} track", "{n} tracks", 0), "0 pistas");
		assert_eq!(decimal(format!("{:.1} dB", -3.5)), "-3,5 dB");
		assert_eq!(decimal("1.2.3".into()), "1,2,3");
		set_thread(None);
	}

	#[test]
	fn placeholders_fill_by_name() {
		assert_eq!(
			fill("{a} of {b} {c}", &[("a", &1), ("b", &"two")]),
			"1 of two {c}"
		);
		assert_eq!(fill("open { brace", &[]), "open { brace");
	}

	#[test]
	fn typed_numbers_accept_a_decimal_comma() {
		assert_eq!(delocalize("2,5"), "2.5");
		assert_eq!(delocalize("-0,25 dB"), "-0.25 dB");
		assert_eq!(delocalize("1.5"), "1.5");
		assert_eq!(delocalize("a, b"), "a, b");
	}

	#[test]
	fn choices_round_trip_and_detect_codes() {
		for c in LanguageChoice::all() {
			let s: String = c.into();
			assert_eq!(LanguageChoice::from(s), c);
		}
		assert_eq!(
			LanguageChoice::from("klingon".to_string()),
			LanguageChoice::System
		);
		assert_eq!(Language::from_code("es_ES.UTF-8"), Some(Language::Spanish));
		assert_eq!(Language::from_code("es-419"), Some(Language::Spanish));
		assert_eq!(Language::from_code("en_US"), Some(Language::English));
		assert_eq!(Language::from_code("pt_BR"), Some(Language::PortugueseBr));
		assert_eq!(Language::from_code("fr_FR.UTF-8"), Some(Language::French));
		assert_eq!(Language::from_code("de_DE.UTF-8"), Some(Language::German));
		assert_eq!(Language::from_code("it_IT"), Some(Language::Italian));
		assert_eq!(Language::from_code("ru_RU.UTF-8"), Some(Language::Russian));
		assert_eq!(Language::from_code("ja_JP.UTF-8"), Some(Language::Japanese));
		assert_eq!(
			Language::from_code("zh_CN"),
			Some(Language::ChineseSimplified)
		);
		assert_eq!(
			Language::from_code("zh_Hans"),
			Some(Language::ChineseSimplified)
		);
		assert_eq!(
			Language::from_code("zh_Hans_CN"),
			Some(Language::ChineseSimplified)
		);
		assert_eq!(Language::from_code("ko_KR.UTF-8"), Some(Language::Korean));
		assert_eq!(Language::from_code("C"), None);
	}

	#[test]
	fn russian_uses_the_one_few_many_plural_rule() {
		set_thread(Some(Language::Russian));
		assert_eq!(trn("{n} track", "{n} tracks", 1), "1 дорожка");
		assert_eq!(trn("{n} track", "{n} tracks", 21), "21 дорожка");
		assert_eq!(trn("{n} track", "{n} tracks", 2), "2 дорожки");
		assert_eq!(trn("{n} track", "{n} tracks", 3), "3 дорожки");
		assert_eq!(trn("{n} track", "{n} tracks", 22), "22 дорожки");
		assert_eq!(trn("{n} track", "{n} tracks", 5), "5 дорожек");
		assert_eq!(trn("{n} track", "{n} tracks", 11), "11 дорожек");
		assert_eq!(trn("{n} track", "{n} tracks", 0), "0 дорожек");
		assert_eq!(decimal(format!("{:.1} dB", -3.5)), "-3,5 dB");
		set_thread(None);
	}

	#[test]
	fn german_and_italian_use_the_one_at_one_plural_rule() {
		set_thread(Some(Language::German));
		assert_eq!(trn("{n} track", "{n} tracks", 1), "1 Spur");
		assert_eq!(trn("{n} track", "{n} tracks", 0), "0 Spuren");
		set_thread(Some(Language::Italian));
		assert_eq!(trn("{n} track", "{n} tracks", 1), "1 traccia");
		assert_eq!(trn("{n} track", "{n} tracks", 0), "0 tracce");
		set_thread(None);
	}

	#[test]
	fn french_and_portuguese_use_the_pt_fr_plural_rule() {
		set_thread(Some(Language::French));
		assert_eq!(trn("{n} track", "{n} tracks", 0), "0 piste");
		assert_eq!(trn("{n} track", "{n} tracks", 1), "1 piste");
		assert_eq!(trn("{n} track", "{n} tracks", 2), "2 pistes");
		set_thread(Some(Language::PortugueseBr));
		assert_eq!(trn("{n} track", "{n} tracks", 0), "0 faixa");
		assert_eq!(trn("{n} track", "{n} tracks", 1), "1 faixa");
		assert_eq!(trn("{n} track", "{n} tracks", 2), "2 faixas");
		set_thread(None);
	}

	#[test]
	fn japanese_chinese_and_korean_use_a_single_cardinal_form() {
		for lang in [
			Language::Japanese,
			Language::ChineseSimplified,
			Language::Korean,
		] {
			assert_eq!(lang.plural_forms(), 1);
			set_thread(Some(lang));
			// The same translation for every count: no "one"/"other" split.
			let zero = trn("{n} track", "{n} tracks", 0);
			let one = trn("{n} track", "{n} tracks", 1);
			let many = trn("{n} track", "{n} tracks", 5);
			assert_eq!(zero.replace('0', "N"), one.replace('1', "N"));
			assert_eq!(one.replace('1', "N"), many.replace('5', "N"));
			assert!(!lang.decimal_comma(), "{lang:?} uses a decimal point");
		}
		set_thread(None);
	}

	/// Every key used in the sources is in every language file, no file keeps unused
	/// keys, plural keys carry the language's forms, and placeholders match.
	#[test]
	fn every_language_file_matches_the_sources() {
		let keys = keys::source_keys();
		assert!(keys.len() > 100, "scanner found {} keys", keys.len());
		let mut problems = Vec::new();
		for lang in Language::ALL
			.into_iter()
			.filter(|l| *l != Language::English)
		{
			let code = lang.code();
			for (key, k) in &keys {
				let Some(forms) = lang.lookup(key) else {
					problems.push(format!("{code}: missing {key:?} ({})", k.file));
					continue;
				};
				match &k.other {
					Some(other) => {
						if forms.len() != lang.plural_forms() {
							problems.push(format!(
								"{code}: {key:?} needs {} plural forms",
								lang.plural_forms()
							));
						}
						let english = keys::placeholders(other);
						for f in forms.iter() {
							if !keys::placeholders(f).is_subset(&english) {
								problems.push(format!(
									"{code}: {f:?} placeholders differ from {other:?}"
								));
							}
						}
						if keys::placeholders(forms[forms.len() - 1]) != english {
							problems.push(format!("{code}: {key:?} other form placeholders"));
						}
					}
					None => {
						if forms.len() != 1 {
							problems.push(format!("{code}: {key:?} is not a plural key"));
						}
						if keys::placeholders(forms[0]) != keys::placeholders(key) {
							problems.push(format!("{code}: {key:?} placeholders differ"));
						}
					}
				}
			}
			for (key, _) in lang.table() {
				if !keys.contains_key(*key) {
					problems.push(format!("{code}: unused key {key:?}"));
				}
			}
		}
		assert!(
			problems.is_empty(),
			"{} problems:\n{}",
			problems.len(),
			problems.join("\n")
		);
	}
}
