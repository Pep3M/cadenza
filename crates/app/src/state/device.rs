//! Pure logic behind the device component kit (`ui::device_kit`): how a knob travels over
//! a parameter's range, and how parameter values are shown and typed back ("3.2k",
//! "-6 dB", "250ms", "C4", "1/8"); the device editor's preset status and how the
//! generic body groups and draws a built-in's params.

use crate::i18n::{mark, tr, tr_str};
use std::collections::BTreeMap;
use std::sync::OnceLock;

use cadenza_dsp::{DeviceInfo, FactoryPreset, ParamInfo};

/// How a knob maps its travel (0..1) to values.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct KnobSpec {
	pub min: f32,
	pub max: f32,
	/// Double-click value.
	pub default: f32,
	/// Times (ms, Hz) feel natural on a logarithmic travel.
	pub log: bool,
	/// Whole numbers only (choices, groups, notes).
	pub integer: bool,
}

impl KnobSpec {
	pub fn new(min: f32, max: f32, default: f32) -> Self {
		Self {
			min,
			max,
			default,
			log: false,
			integer: false,
		}
	}

	/// The travel a built-in parameter gets from its range and unit: frequencies and
	/// positive times are logarithmic, switches and voice counts are whole numbers.
	pub fn for_param(min: f32, max: f32, default: f32, unit: &str) -> Self {
		let mut spec = Self::new(min, max, default);
		if unit == "Hz" || unit == "ms" && min > 0.0 {
			spec = spec.log();
		}
		if is_stepped(min, max, unit) || unit == "voices" {
			spec = spec.integer();
		}
		spec
	}

	pub fn log(mut self) -> Self {
		self.log = self.min > 0.0;
		self
	}

	pub fn integer(mut self) -> Self {
		self.integer = true;
		self
	}

	/// Ranges around zero (pan, gain, tune) fill their arc from zero.
	pub fn bipolar(&self) -> bool {
		self.min < 0.0 && self.max > 0.0
	}

	/// Travel position 0..1 of `v`.
	pub fn to_pos(&self, v: f32) -> f32 {
		let v = v.clamp(self.min, self.max);
		if self.log {
			(v / self.min).ln() / (self.max / self.min).ln()
		} else {
			(v - self.min) / (self.max - self.min).max(f32::EPSILON)
		}
	}

	/// Value at travel position `pos` (clamped, rounded for integer knobs).
	pub fn from_pos(&self, pos: f32) -> f32 {
		let p = pos.clamp(0.0, 1.0);
		let v = if self.log {
			self.min * (self.max / self.min).powf(p)
		} else {
			self.min + p * (self.max - self.min)
		};
		if self.integer { v.round() } else { v }
	}
}

/// A unitless whole-number parameter with a handful of values (mode, on/off).
pub fn is_stepped(min: f32, max: f32, unit: &str) -> bool {
	unit.is_empty() && max - min <= 3.0 && min.fract() == 0.0 && max.fract() == 0.0
}

/// A unitless 0/1 parameter (drawn as a toggle).
pub fn is_switch(min: f32, max: f32, unit: &str) -> bool {
	unit.is_empty() && min == 0.0 && max == 1.0
}

/// Why typed text is not a value.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ParseError {
	#[error("type a value")]
	Empty,
	#[error("'{0}' is not a number")]
	NotANumber(String),
	#[error("'{unit}' does not fit a value in {expected}")]
	WrongUnit { unit: String, expected: String },
	#[error("'{0}' is not one of the choices")]
	NotAChoice(String),
}

/// A typed value, clamped into the range.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Parsed {
	pub value: f32,
	/// The text asked for a value outside the range (the UI flashes, then clamps).
	pub out_of_range: bool,
}

/// Parses what the user typed into a readout of a parameter in `unit` (the `ParamInfo`
/// units: "Hz", "ms", "s", "dB", "%", "cents", "semitones", "ratio", "voices", "").
///
/// Accepts the unit or none ("-6", "-6 dB"), `k` shorthand ("3.2k", "3.2 kHz"), times in
/// the other unit ("1.2s" for a ms param), note names for frequencies ("C4", "A#3",
/// "Bb2"; A4 = 440 Hz), ratios ("4:1") and fractions ("1/4"). With `choices`
/// (index `i` = value `min + i`), a choice name or its short form ("1/8T", "1/8.")
/// picks it.
pub fn parse_value(
	text: &str,
	unit: &str,
	min: f32,
	max: f32,
	choices: Option<&[&str]>,
) -> Result<Parsed, ParseError> {
	let text = crate::i18n::delocalize(text.trim()).replace('\u{2212}', "-");
	if text.is_empty() {
		return Err(ParseError::Empty);
	}
	if let Some(choices) = choices {
		if let Some(i) = choices
			.iter()
			.position(|c| choice_matches(&text, c) || choice_matches(&text, tr_str(c)))
		{
			return Ok(clamped(min + i as f32, min, max));
		}
		// A bare index also works ("6").
		return match text.parse::<f32>() {
			Ok(v) => Ok(clamped(min + v.round(), min, max)),
			Err(_) => Err(ParseError::NotAChoice(text)),
		};
	}
	let lower = text.to_lowercase();
	if matches!(lower.as_str(), "-inf" | "-∞" | "off") && unit == "dB" {
		return Ok(clamped(min, min, max));
	}
	if unit == "pan%" {
		if lower == "c" {
			return Ok(clamped(0.0, min, max));
		}
		if let Some(rest) = lower.strip_prefix('l')
			&& let Ok(v) = rest.trim().parse::<f32>()
		{
			return Ok(clamped(-v, min, max));
		}
		if let Some(rest) = lower.strip_prefix('r')
			&& let Ok(v) = rest.trim().parse::<f32>()
		{
			return Ok(clamped(v, min, max));
		}
	}
	if unit == "Hz"
		&& let Some(hz) = note_to_hz(&text)
	{
		return Ok(clamped(hz, min, max));
	}
	if let Some(v) = parse_fraction(&lower) {
		return Ok(clamped(v, min, max));
	}
	let (number, rest) =
		split_number(&lower).ok_or_else(|| ParseError::NotANumber(text.clone()))?;
	let mut v: f32 = number
		.parse()
		.map_err(|_| ParseError::NotANumber(text.clone()))?;
	let mut suffix = rest.trim();
	if let Some(s) = suffix.strip_prefix('k')
		&& (s.is_empty() || unit == "Hz" && s.trim() == "hz")
	{
		v *= 1000.0;
		suffix = "";
	}
	let scale = unit_scale(suffix, unit).ok_or_else(|| ParseError::WrongUnit {
		unit: suffix.to_owned(),
		expected: if unit.is_empty() {
			tr("plain numbers").to_owned()
		} else {
			unit.to_owned()
		},
	})?;
	Ok(clamped(v * scale, min, max))
}

fn clamped(v: f32, min: f32, max: f32) -> Parsed {
	Parsed {
		value: v.clamp(min, max),
		out_of_range: v < min || v > max,
	}
}

/// Factor from a typed unit suffix to the parameter's unit, `None` when it does not fit.
fn unit_scale(suffix: &str, unit: &str) -> Option<f32> {
	if suffix.is_empty() {
		return Some(1.0);
	}
	let s = suffix;
	match unit {
		"Hz" => match s {
			"hz" => Some(1.0),
			"khz" => Some(1000.0),
			_ => None,
		},
		"ms" => match s {
			"ms" | "msec" => Some(1.0),
			"s" | "sec" => Some(1000.0),
			_ => None,
		},
		"s" => match s {
			"s" | "sec" => Some(1.0),
			"ms" | "msec" => Some(0.001),
			_ => None,
		},
		"dB" => (s == "db").then_some(1.0),
		"%" | "pan%" => (s == "%" || s == "pct").then_some(1.0),
		"cents" => matches!(s, "c" | "ct" | "cent" | "cents").then_some(1.0),
		"semitones" => {
			matches!(s, "st" | "semi" | "semis" | "semitone" | "semitones").then_some(1.0)
		}
		"ratio" => matches!(s, ":1" | ": 1").then_some(1.0),
		"voices" => matches!(s, "v" | "voice" | "voices").then_some(1.0),
		_ => None,
	}
}

/// Leading number of `s` (sign, digits, one dot, exponent) and the rest.
fn split_number(s: &str) -> Option<(&str, &str)> {
	let b = s.as_bytes();
	let mut i = 0;
	if i < b.len() && (b[i] == b'-' || b[i] == b'+') {
		i += 1;
	}
	let digits = i;
	let mut dot = false;
	while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'.' && !dot) {
		dot |= b[i] == b'.';
		i += 1;
	}
	if i == digits || &s[digits..i] == "." {
		return None;
	}
	Some((&s[..i], &s[i..]))
}

/// "1/4" -> 0.25.
fn parse_fraction(s: &str) -> Option<f32> {
	let (a, b) = s.split_once('/')?;
	let a: f32 = a.trim().parse().ok()?;
	let b: f32 = b.trim().parse().ok()?;
	(b != 0.0).then_some(a / b)
}

/// Frequency of a note name ("C4" = middle C, "A4" = 440 Hz, "F#2", "Bb3"; the sharp and flat signs too).
pub fn note_to_hz(text: &str) -> Option<f32> {
	let mut chars = text.trim().chars().peekable();
	let letter = chars.next()?.to_ascii_uppercase();
	let base = match letter {
		'C' => 0,
		'D' => 2,
		'E' => 4,
		'F' => 5,
		'G' => 7,
		'A' => 9,
		'B' => 11,
		_ => return None,
	};
	let mut semis = base;
	while let Some(&c) = chars.peek() {
		match c {
			'#' | '\u{266F}' => semis += 1,
			'b' | '\u{266D}' => semis -= 1,
			_ => break,
		}
		chars.next();
	}
	let octave: i32 = chars.collect::<String>().trim().parse().ok()?;
	let midi = (octave + 1) * 12 + semis;
	Some(440.0 * 2f32.powf((midi - 69) as f32 / 12.0))
}

/// Text normalized for comparing choices: lower case, no spaces, "dotted x" = "x.",
/// "x triplet" = "xt".
fn normalize_choice(s: &str) -> String {
	let s = s.to_lowercase();
	let s = if let Some(rest) = s.strip_prefix("dotted ") {
		format!("{rest}.")
	} else {
		s
	};
	let s = s.replace(" triplet", "t").replace("triplet", "t");
	let s: String = s.chars().filter(|c| !c.is_whitespace()).collect();
	s.strip_suffix('d')
		.filter(|r| r.ends_with(|c: char| c.is_ascii_digit()))
		.map(|r| format!("{r}."))
		.unwrap_or(s)
}

fn choice_matches(text: &str, choice: &str) -> bool {
	let t = normalize_choice(text);
	t == normalize_choice(choice) || t == normalize_choice(&short_choice(choice))
}

/// Compact label of a choice for grids and readouts: "1/16 triplet" -> "1/16T",
/// "dotted 1/8" -> "1/8.", "1 bar (4/4)" -> "1 bar".
pub fn short_choice(name: &str) -> String {
	if let Some(rest) = name.strip_prefix("dotted ") {
		return format!("{rest}.");
	}
	if let Some(rest) = name.strip_suffix(" triplet") {
		return format!("{rest}T");
	}
	match name.split_once(" (") {
		Some((head, _)) => head.to_owned(),
		None => name.to_owned(),
	}
}

/// The delay's tempo-sync steps by name (index = the `sync` value).
pub fn delay_sync_choices() -> &'static [&'static str] {
	static NAMES: OnceLock<Vec<&'static str>> = OnceLock::new();
	NAMES.get_or_init(|| {
		cadenza_dsp::response::delay_sync_steps()
			.iter()
			.map(|(name, _)| *name)
			.collect()
	})
}

/// A parameter value with a readable unit: "2.80 kHz", "184 ms", "1.20 s", "-6.0 dB",
/// "40 %", "+7 st", "4.0:1". Pass `"pan%"` instead of `"%"` for a bipolar pan parameter
/// (e.g. Gain/Utility's `pan`, -100..100) to read "C" at 0 and "L25"/"R25" otherwise, like
/// the mixer's `widgets::format_pan`; [`parse_value`] accepts that notation back.
/// `-24.0` -> "−24.0" (typographic minus, for display texts).
pub fn minus(text: String) -> String {
	text.replace('-', "\u{2212}")
}

pub fn format_value(value: f32, unit: &str) -> String {
	crate::i18n::decimal(format_value_en(value, unit))
}

fn format_value_en(value: f32, unit: &str) -> String {
	let v = value;
	match unit {
		"Hz" => {
			if v >= 10_000.0 {
				format!("{:.1} kHz", v / 1000.0)
			} else if v >= 1000.0 {
				format!("{:.2} kHz", v / 1000.0)
			} else if v >= 100.0 {
				format!("{v:.0} Hz")
			} else {
				format!("{v:.1} Hz")
			}
		}
		"ms" => {
			if v >= 1000.0 {
				format!("{:.2} s", v / 1000.0)
			} else if v >= 100.0 {
				format!("{v:.0} ms")
			} else if v >= 10.0 {
				format!("{v:.1} ms")
			} else {
				format!("{v:.2} ms")
			}
		}
		"s" => {
			if v < 1.0 {
				format!("{:.0} ms", v * 1000.0)
			} else if v < 10.0 {
				format!("{v:.2} s")
			} else {
				format!("{v:.1} s")
			}
		}
		"dB" => {
			if v.abs() < 0.05 {
				"0.0 dB".to_owned()
			} else {
				format!("{v:+.1} dB")
			}
		}
		"%" => {
			if v.abs() < 10.0 && v.fract().abs() >= 0.05 {
				format!("{v:.1} %")
			} else {
				format!("{v:.0} %")
			}
		}
		"pan%" => {
			let p = v.round() as i32;
			match p {
				0 => "C".to_owned(),
				p if p < 0 => format!("L{}", -p),
				p => format!("R{p}"),
			}
		}
		"cents" => signed_whole(v, "ct"),
		"semitones" => {
			if (v - v.round()).abs() < 0.005 {
				signed_whole(v, "st")
			} else {
				format!("{v:+.2} st")
			}
		}
		"ratio" => format!("{v:.1}:1"),
		"voices" => format!("{v:.0}"),
		// Stepped/choice params go through `format_choice` instead; a Knob with an empty
		// unit is always a continuous value (e.g. EQ Q), so it always gets 2 decimals.
		"" => format!("{v:.2}"),
		other => format!("{v:.2} {other}"),
	}
}

fn signed_whole(v: f32, unit: &str) -> String {
	let r = v.round();
	if r == 0.0 {
		format!("0 {unit}")
	} else {
		format!("{r:+.0} {unit}")
	}
}

/// The short name of the choice `value` selects (index `value - min`).
pub fn format_choice(value: f32, min: f32, choices: &[&str]) -> String {
	let i = (value - min).round().max(0.0) as usize;
	choices
		.get(i.min(choices.len().saturating_sub(1)))
		.map(|c| tr_str(&short_choice(c)).to_owned())
		.unwrap_or_default()
}

// --- Presets ------------------------------------------------------------------------

/// One entry of a device editor's preset list.
#[derive(Debug, Clone, PartialEq)]
pub struct PresetEntry {
	pub name: String,
	/// Saved by the user (else shipped with the device).
	pub user: bool,
	/// Factory category or plugin bank (empty: ungrouped).
	pub category: String,
	pub description: String,
}

/// `entries` grouped by category: built-in categories in their browsing order, then plugin
/// banks as they come, with the uncategorized ones first under an empty name.
pub fn preset_groups<'a>(entries: &[&'a PresetEntry]) -> Vec<(&'a str, Vec<&'a PresetEntry>)> {
	let mut groups: Vec<(&'a str, Vec<&'a PresetEntry>)> = Vec::new();
	for e in entries {
		match groups.iter_mut().find(|(c, _)| *c == e.category) {
			Some((_, g)) => g.push(e),
			None => groups.push((e.category.as_str(), vec![e])),
		}
	}
	let rank = |c: &str| {
		if c.is_empty() {
			0
		} else {
			cadenza_dsp::PRESET_CATEGORIES
				.iter()
				.position(|k| *k == c)
				.map_or(usize::MAX, |i| i + 1)
		}
	};
	groups.sort_by_key(|(c, _)| rank(c));
	groups
}

/// The preset last loaded into or saved from a device, with the params it set.
#[derive(Debug, Clone, PartialEq)]
pub struct LoadedPreset {
	pub name: String,
	pub user: bool,
	pub params: BTreeMap<String, f32>,
}

/// What the preset bar shows: the preset a device is on and whether it was edited since.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PresetStatus {
	pub name: Option<String>,
	pub user: bool,
	/// The params differ from the preset's (the amber dot).
	pub dirty: bool,
}

/// A param's value as the device plays it: the stored one, else the default.
fn effective(info: &ParamInfo, params: &BTreeMap<String, f32>) -> f32 {
	params.get(info.id).copied().unwrap_or(info.default)
}

/// Whether two param maps make `device` sound different (missing = default; ids the
/// device does not know are ignored; equal within a millionth of each range).
pub fn params_differ(
	device: &DeviceInfo,
	a: &BTreeMap<String, f32>,
	b: &BTreeMap<String, f32>,
) -> bool {
	device.params.iter().any(|p| {
		let tolerance = (p.max - p.min).abs().max(1.0) * 1e-6;
		(effective(p, a) - effective(p, b)).abs() > tolerance
	})
}

/// The params a factory preset sets, as a map.
pub fn factory_params(preset: &FactoryPreset) -> BTreeMap<String, f32> {
	preset
		.params
		.iter()
		.map(|(k, v)| ((*k).to_owned(), *v))
		.collect()
}

/// The preset bar of a device with `params`: the preset loaded or saved here (dirty
/// when the params moved away from it), else the first factory preset these params
/// match exactly, else none.
pub fn preset_status(
	device: &DeviceInfo,
	params: &BTreeMap<String, f32>,
	loaded: Option<&LoadedPreset>,
	factory: &[FactoryPreset],
) -> PresetStatus {
	if let Some(l) = loaded {
		return PresetStatus {
			name: Some(l.name.clone()),
			user: l.user,
			dirty: params_differ(device, params, &l.params),
		};
	}
	factory
		.iter()
		.find(|p| !params_differ(device, params, &factory_params(p)))
		.map(|p| PresetStatus {
			name: Some(p.name.to_owned()),
			user: false,
			dirty: false,
		})
		.unwrap_or_default()
}

/// The preset `step` entries away from the one named `current` (wrapping); from no
/// preset, ‹ goes to the last and › to the first.
pub fn step_preset(list: &[PresetEntry], current: &PresetStatus, step: i32) -> Option<usize> {
	if list.is_empty() {
		return None;
	}
	let n = list.len() as i32;
	let at = current.name.as_ref().and_then(|name| {
		list.iter()
			.position(|e| &e.name == name && e.user == current.user)
	});
	Some(match at {
		Some(i) => (i as i32 + step).rem_euclid(n) as usize,
		None if step < 0 => (n - 1) as usize,
		None => 0,
	})
}

// --- Generic device body ----------------------------------------------------------

/// Named choices of a built-in's discrete params (index = value - min).
pub fn param_choices(device: &str, param: &str) -> Option<&'static [&'static str]> {
	Some(match (device, param) {
		("eq", "hp_slope" | "lp_slope") => &const { [mark("Off"), "12 dB", "24 dB", "48 dB"] },
		("delay", "sync") => delay_sync_choices(),
		("saturator", "mode") => &const { [mark("Tape"), mark("Tube"), mark("Hard clip")] },
		("poly_synth", "osc1_wave" | "osc2_wave") => &crate::state::poly_synth_editor::WAVE_NAMES,
		("poly_synth", "osc1_table" | "osc2_table") => {
			&crate::state::poly_synth_editor::TABLE_NAMES
		}
		("poly_synth", "filter_slope") => &["12 dB", "24 dB"],
		("sampler", "mode") => &const { [mark("Gate"), mark("One-shot")] },
		("sampler", "filter_type") => &const { [mark("Off"), "LP", "HP", "BP"] },
		("pitch_correct", "key") => &crate::state::pitch_correct_editor::KEY_NAMES,
		("pitch_correct", "scale") => &crate::state::pitch_correct_editor::SCALE_LABELS,
		_ => return None,
	})
}

/// How the generic body draws a param.
#[derive(Debug, Clone, PartialEq)]
pub enum ParamControl {
	Knob,
	/// A 0/1 switch: a toggle with this label.
	Toggle(String),
	/// Five choices or fewer.
	Segmented(Vec<String>),
	/// More choices: a grid of `columns`.
	Grid(Vec<String>, usize),
}

/// The control of `param` of built-in `device`: named choices as segments or a grid,
/// other small unitless whole-number ranges as numbered segments, 0/1 switches as
/// toggles, everything else a knob.
pub fn param_control(device: &str, param: &ParamInfo) -> ParamControl {
	let labels = |c: &[&str]| c.iter().map(|c| short_choice(c)).collect::<Vec<_>>();
	if let Some(c) = param_choices(device, param.id) {
		return if c.len() <= 5 {
			ParamControl::Segmented(labels(c))
		} else {
			ParamControl::Grid(labels(c), c.len().div_ceil(2).min(7))
		};
	}
	if is_switch(param.min, param.max, param.unit) {
		let label = match (device, param.id) {
			("delay", "ping_pong") => tr("⇄ Ping-pong").to_owned(),
			("gain", "invert_left") => tr("Ø Invert L").to_owned(),
			("gain", "invert_right") => tr("Ø Invert R").to_owned(),
			_ => tr_str(param.name).to_owned(),
		};
		return ParamControl::Toggle(label);
	}
	if is_stepped(param.min, param.max, param.unit) {
		let n = (param.max - param.min) as i32;
		return ParamControl::Segmented(
			(0..=n)
				.map(|i| format!("{}", param.min as i32 + i))
				.collect(),
		);
	}
	ParamControl::Knob
}

/// A row of the generic body: params sharing an id prefix ("amp_attack", "amp_decay"
/// -> "Amp"), or untitled runs of the rest.
#[derive(Debug, Clone, PartialEq)]
pub struct ParamGroup {
	pub title: Option<String>,
	/// Params in registry order, each with its label inside the group.
	pub params: Vec<(&'static ParamInfo, String)>,
}

/// Groups `params` (registry order kept) for the generic body: a run of two or more
/// params with the same id prefix becomes a titled group (title = the leading words
/// their names share, else the prefix), single params gather into untitled groups.
/// Labels drop the group title ("Amp Attack" -> "Attack").
pub fn param_groups(params: &[&'static ParamInfo]) -> Vec<ParamGroup> {
	fn key(p: &ParamInfo) -> &str {
		p.id.split('_').next().unwrap_or(p.id)
	}
	let mut runs: Vec<Vec<&'static ParamInfo>> = Vec::new();
	for p in params {
		match runs.last_mut() {
			Some(run) if key(run[0]) == key(p) => run.push(p),
			_ => runs.push(vec![p]),
		}
	}
	let mut groups: Vec<ParamGroup> = Vec::new();
	for run in runs {
		if run.len() < 2 {
			let entry = (run[0], run[0].name.to_owned());
			match groups.last_mut() {
				Some(g) if g.title.is_none() => g.params.push(entry),
				_ => groups.push(ParamGroup {
					title: None,
					params: vec![entry],
				}),
			}
			continue;
		}
		let title = shared_words(&run).unwrap_or_else(|| capitalize(key(run[0])));
		let params = run
			.iter()
			.map(|p| {
				let label = p
					.name
					.strip_prefix(title.as_str())
					.map(str::trim)
					.filter(|l| !l.is_empty())
					.unwrap_or(p.name);
				(*p, label.to_owned())
			})
			.collect();
		groups.push(ParamGroup {
			title: Some(title),
			params,
		});
	}
	groups
}

/// What the full (non-compact) device-editor header shows at `width` (the dock or window
/// width, between [`crate::ui::device_editor::COMPACT_BELOW`] and 1600, where the undo hint
/// and A→B label already have their own threshold): the header's name/track/preset/Save/A-B
/// group on the left and close/pop-out/Ask AI/Mix/meters on the right are laid out
/// independently (left-to-right and right-to-left) over the same row, so without a shared
/// budget they can overlap in the middle before either side actually runs out of room.
/// `left_fixed` / `right_fixed` are the already-measured width of each side's items that are
/// never hidden (everything but the preset box, the sidechain chip and the meters);
/// `sidechain_width` is `Some` width of the sidechain chip when the device has a key. Items
/// give way in the order the Interaction Spec lists ("Responsive: what collapses first"):
/// the sidechain chip goes first, then the IN/OUT meters shrink to the compact pair, then
/// the preset box narrows down to [`MIN_PRESET_WIDTH`] — only then does the two-row compact
/// header take over.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HeaderItems {
	/// The preset combo box's width.
	pub preset_width: f32,
	/// The sidechain key chip (dropped first as the header narrows).
	pub show_sidechain: bool,
	/// IN/OUT meters as the compact 34 px pair instead of the full labelled bars.
	pub compact_meters: bool,
}

/// The preset box never narrows past this (still shows a short preset name).
pub const MIN_PRESET_WIDTH: f32 = 130.0;
/// The preset box's width when there is room for it.
pub const FULL_PRESET_WIDTH: f32 = 200.0;
/// Width of the full labelled IN/OUT meter pair (`device_kit::io_meters(.., compact: false)`).
const FULL_METERS_WIDTH: f32 = 248.0;
/// Width of the compact IN/OUT meter pair (`device_kit::io_meters(.., compact: true)`).
const COMPACT_METERS_WIDTH: f32 = 34.0;

pub fn header_items(
	width: f32,
	left_fixed: f32,
	right_fixed: f32,
	sidechain_width: Option<f32>,
) -> HeaderItems {
	let fixed = left_fixed + right_fixed;
	let budget = (width - fixed).max(0.0);
	let full_sidechain_w = sidechain_width.unwrap_or(0.0);
	let show_sidechain = sidechain_width.is_some()
		&& FULL_PRESET_WIDTH + full_sidechain_w + FULL_METERS_WIDTH <= budget;
	let sidechain_w = if show_sidechain {
		full_sidechain_w
	} else {
		0.0
	};
	let compact_meters = FULL_PRESET_WIDTH + sidechain_w + FULL_METERS_WIDTH > budget;
	let meters_w = if compact_meters {
		COMPACT_METERS_WIDTH
	} else {
		FULL_METERS_WIDTH
	};
	let preset_width = (budget - sidechain_w - meters_w).clamp(MIN_PRESET_WIDTH, FULL_PRESET_WIDTH);
	HeaderItems {
		preset_width,
		show_sidechain,
		compact_meters,
	}
}

/// Greedy rows of items of `widths` that fit `available` (each row keeps at least one).
pub fn pack_rows(widths: &[f32], available: f32, gap: f32) -> Vec<Vec<usize>> {
	let mut rows: Vec<Vec<usize>> = Vec::new();
	let mut used = 0.0;
	for (i, &w) in widths.iter().enumerate() {
		match rows.last_mut() {
			Some(row) if used + gap + w <= available => {
				row.push(i);
				used += gap + w;
			}
			_ => {
				rows.push(vec![i]);
				used = w;
			}
		}
	}
	rows
}

/// The leading words every name in `run` shares ("Peak 1 Freq", "Peak 1 Q" -> "Peak 1").
fn shared_words(run: &[&ParamInfo]) -> Option<String> {
	let first: Vec<&str> = run[0].name.split(' ').collect();
	let n = run.iter().skip(1).fold(first.len(), |n, p| {
		p.name
			.split(' ')
			.zip(&first)
			.take(n)
			.take_while(|(a, b)| a == *b)
			.count()
	});
	// Keep at least one word for the label.
	let n = run
		.iter()
		.map(|p| p.name.split(' ').count().saturating_sub(1))
		.fold(n, usize::min);
	(n > 0).then(|| first[..n].join(" "))
}

fn capitalize(word: &str) -> String {
	let mut c = word.chars();
	c.next()
		.map(|f| f.to_uppercase().chain(c).collect())
		.unwrap_or_default()
}

/// One ↑ / ↓ key press on a parameter (`dir` ±1): 0.5 dB (Shift: 0.1) on dB values, one
/// step on whole-number ones, else 1 % of the knob travel (Shift: 0.1 %).
pub fn nudge(p: &ParamInfo, value: f32, dir: f32, fine: bool) -> f32 {
	let spec = KnobSpec::for_param(p.min, p.max, p.default, p.unit);
	let v = if p.unit == "dB" {
		let step = if fine { 0.1 } else { 0.5 };
		((value + dir * step) / step).round() * step
	} else if spec.integer {
		value.round() + dir
	} else {
		spec.from_pos(spec.to_pos(value) + dir * if fine { 0.001 } else { 0.01 })
	};
	v.clamp(p.min, p.max)
}

/// The parameter ↑ / ↓ nudge in an editor until the user touches a control: the one
/// that defines the device.
pub fn main_param(device: &str) -> Option<&'static str> {
	Some(match device {
		"compressor" => "threshold",
		"limiter" => "input_gain",
		"reverb" => "decay",
		"delay" => "feedback",
		"chorus" => "depth",
		"saturator" => "drive",
		"gain" => "gain",
		"poly_synth" => "filter_cutoff",
		"electric_piano" => "brightness",
		_ => return None,
	})
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn nudges_step_in_units_and_main_params_exist() {
		let info = cadenza_dsp::device_info("compressor").unwrap();
		let threshold = info.params.iter().find(|p| p.id == "threshold").unwrap();
		assert!((nudge(threshold, -18.0, 1.0, false) + 17.5).abs() < 1e-4);
		assert!((nudge(threshold, -18.0, -1.0, true) + 18.1).abs() < 1e-4);
		assert_eq!(nudge(threshold, threshold.max, 1.0, false), threshold.max);
		for d in cadenza_dsp::registry() {
			if let Some(id) = main_param(d.id) {
				assert!(d.params.iter().any(|p| p.id == id), "{}: {id}", d.id);
			}
		}
	}

	fn parse(text: &str, unit: &str, min: f32, max: f32) -> Parsed {
		parse_value(text, unit, min, max, None).unwrap()
	}

	fn close(a: f32, b: f32) -> bool {
		(a - b).abs() < 1e-3 * b.abs().max(1.0)
	}

	#[test]
	fn knob_travel_round_trips() {
		let lin = KnobSpec::new(-1.0, 1.0, 0.0);
		assert!((lin.to_pos(0.0) - 0.5).abs() < 1e-6);
		assert!((lin.from_pos(0.75) - 0.5).abs() < 1e-6);
		assert!(lin.bipolar());
		let log = KnobSpec::new(1.0, 10_000.0, 100.0).log();
		assert!((log.to_pos(100.0) - 0.5).abs() < 1e-4);
		assert!((log.from_pos(0.25) - 10.0).abs() < 1e-3);
		assert!(!log.bipolar());
		let int = KnobSpec::new(0.0, 16.0, 0.0).integer();
		assert_eq!(int.from_pos(0.49), 8.0);
		assert_eq!(int.from_pos(2.0), 16.0);
	}

	#[test]
	fn param_travel_follows_the_unit() {
		assert!(KnobSpec::for_param(20.0, 20_000.0, 1000.0, "Hz").log);
		assert!(KnobSpec::for_param(0.1, 2000.0, 10.0, "ms").log);
		assert!(!KnobSpec::for_param(0.0, 500.0, 0.0, "ms").log);
		assert!(KnobSpec::for_param(0.0, 2.0, 0.0, "").integer);
		assert!(KnobSpec::for_param(1.0, 64.0, 8.0, "voices").integer);
		assert!(!KnobSpec::for_param(0.0, 100.0, 50.0, "%").integer);
		assert!(is_switch(0.0, 1.0, ""));
		assert!(!is_switch(0.0, 1.0, "%"));
	}

	#[test]
	fn parses_numbers_units_and_shorthand() {
		assert!(close(parse("3.2k", "Hz", 20.0, 20_000.0).value, 3200.0));
		assert!(close(parse("3.2 kHz", "Hz", 20.0, 20_000.0).value, 3200.0));
		assert!(close(parse("440hz", "Hz", 20.0, 20_000.0).value, 440.0));
		assert!(close(parse("-6", "dB", -60.0, 24.0).value, -6.0));
		assert!(close(parse("-6 dB", "dB", -60.0, 24.0).value, -6.0));
		assert!(close(parse("\u{2212}6dB", "dB", -60.0, 24.0).value, -6.0));
		assert!(close(parse("+3", "dB", -60.0, 24.0).value, 3.0));
		assert!(close(parse("40%", "%", 0.0, 100.0).value, 40.0));
		assert!(close(parse("250ms", "ms", 1.0, 5000.0).value, 250.0));
		assert!(close(parse("1.2s", "ms", 1.0, 5000.0).value, 1200.0));
		assert!(close(parse("250 ms", "s", 0.1, 20.0).value, 0.25));
		assert!(close(parse(".5", "s", 0.1, 20.0).value, 0.5));
		assert!(close(parse("4:1", "ratio", 1.0, 20.0).value, 4.0));
		assert!(close(parse("-7 st", "semitones", -24.0, 24.0).value, -7.0));
		assert!(close(parse("12ct", "cents", -100.0, 100.0).value, 12.0));
		assert!(close(parse("1/4", "", 0.0, 1.0).value, 0.25));
		assert!(close(parse("-inf", "dB", -60.0, 24.0).value, -60.0));
	}

	#[test]
	fn note_names_are_frequencies() {
		assert!(close(parse("A4", "Hz", 20.0, 20_000.0).value, 440.0));
		assert!(close(parse("C4", "Hz", 20.0, 20_000.0).value, 261.626));
		assert!(close(parse("a#3", "Hz", 20.0, 20_000.0).value, 233.082));
		assert!(close(parse("Bb3", "Hz", 20.0, 20_000.0).value, 233.082));
		assert!(close(
			parse("C\u{266F}4", "Hz", 20.0, 20_000.0).value,
			277.183
		));
		assert_eq!(note_to_hz("H4"), None);
		// Note names are only frequencies.
		assert!(parse_value("C4", "dB", -60.0, 24.0, None).is_err());
	}

	#[test]
	fn out_of_range_clamps_and_says_so() {
		let p = parse("40k", "Hz", 20.0, 20_000.0);
		assert_eq!(p.value, 20_000.0);
		assert!(p.out_of_range);
		let p = parse("-90", "dB", -60.0, 24.0);
		assert_eq!(p.value, -60.0);
		assert!(p.out_of_range);
		assert!(!parse("12", "dB", -60.0, 24.0).out_of_range);
	}

	#[test]
	fn bad_text_is_an_error() {
		assert_eq!(
			parse_value("  ", "dB", 0.0, 1.0, None),
			Err(ParseError::Empty)
		);
		assert!(matches!(
			parse_value("loud", "dB", 0.0, 1.0, None),
			Err(ParseError::NotANumber(_))
		));
		assert!(matches!(
			parse_value("3 dB", "Hz", 20.0, 20_000.0, None),
			Err(ParseError::WrongUnit { .. })
		));
		assert!(matches!(
			parse_value("3k ms", "%", 0.0, 100.0, None),
			Err(ParseError::WrongUnit { .. })
		));
	}

	#[test]
	fn choices_match_names_and_short_forms() {
		let sync = delay_sync_choices();
		assert_eq!(sync.len(), 13);
		let pick = |t: &str| parse_value(t, "", 0.0, 12.0, Some(sync)).map(|p| p.value);
		assert_eq!(pick("1/8"), Ok(6.0));
		assert_eq!(pick("1/8T"), Ok(5.0));
		assert_eq!(pick("1/8 triplet"), Ok(5.0));
		assert_eq!(pick("dotted 1/8"), Ok(7.0));
		assert_eq!(pick("1/8."), Ok(7.0));
		assert_eq!(pick("1/8d"), Ok(7.0));
		assert_eq!(pick("free"), Ok(0.0));
		assert_eq!(pick("1 bar"), Ok(12.0));
		assert_eq!(pick("9"), Ok(9.0));
		assert!(matches!(pick("1/7"), Err(ParseError::NotAChoice(_))));
		assert_eq!(format_choice(5.0, 0.0, sync), "1/8T");
		assert_eq!(format_choice(4.0, 0.0, sync), "1/16.");
		assert_eq!(format_choice(12.0, 0.0, sync), "1 bar");
		assert_eq!(short_choice("1/4"), "1/4");
	}

	#[test]
	fn formats_with_nice_units() {
		assert_eq!(format_value(2800.0, "Hz"), "2.80 kHz");
		assert_eq!(format_value(12_500.0, "Hz"), "12.5 kHz");
		assert_eq!(format_value(440.0, "Hz"), "440 Hz");
		assert_eq!(format_value(40.0, "Hz"), "40.0 Hz");
		assert_eq!(format_value(184.0, "ms"), "184 ms");
		assert_eq!(format_value(12.34, "ms"), "12.3 ms");
		assert_eq!(format_value(1200.0, "ms"), "1.20 s");
		assert_eq!(format_value(0.25, "s"), "250 ms");
		assert_eq!(format_value(2.5, "s"), "2.50 s");
		assert_eq!(format_value(-9.6, "dB"), "-9.6 dB");
		assert_eq!(format_value(3.0, "dB"), "+3.0 dB");
		assert_eq!(format_value(0.01, "dB"), "0.0 dB");
		assert_eq!(format_value(40.0, "%"), "40 %");
		assert_eq!(format_value(2.5, "%"), "2.5 %");
		assert_eq!(format_value(7.0, "semitones"), "+7 st");
		assert_eq!(format_value(-0.5, "semitones"), "-0.50 st");
		assert_eq!(format_value(0.0, "cents"), "0 ct");
		assert_eq!(format_value(4.0, "ratio"), "4.0:1");
		assert_eq!(format_value(2.0, ""), "2.00");
		assert_eq!(format_value(0.25, ""), "0.25");
	}

	#[test]
	fn pan_reads_and_parses_like_the_mixer() {
		assert_eq!(format_value(0.0, "pan%"), "C");
		assert_eq!(format_value(-25.3, "pan%"), "L25");
		assert_eq!(format_value(25.3, "pan%"), "R25");
		assert_eq!(format_value(-100.0, "pan%"), "L100");
		let pick = |t: &str| parse_value(t, "pan%", -100.0, 100.0, None).map(|p| p.value);
		assert_eq!(pick("C"), Ok(0.0));
		assert_eq!(pick("c"), Ok(0.0));
		assert_eq!(pick("L25"), Ok(-25.0));
		assert_eq!(pick("l 25"), Ok(-25.0));
		assert_eq!(pick("R25"), Ok(25.0));
		assert_eq!(pick("r 25"), Ok(25.0));
		// Plain numbers (and a "%" suffix) still work, unaffected by the pan notation.
		assert_eq!(pick("-25"), Ok(-25.0));
		assert_eq!(pick("25%"), Ok(25.0));
	}

	#[test]
	fn unitless_readouts_always_show_two_decimals() {
		// EQ's Q is the only Knob control with an empty unit; whole values must not
		// collapse to "4" while others read "0.80".
		assert_eq!(format_value(4.0, ""), "4.00");
		assert_eq!(format_value(0.8, ""), "0.80");
	}

	#[test]
	fn formatted_values_parse_back() {
		for (v, unit, min, max) in [
			(2800.0, "Hz", 20.0, 20_000.0),
			(184.0, "ms", 1.0, 5000.0),
			(1200.0, "ms", 1.0, 5000.0),
			(-9.6, "dB", -60.0, 24.0),
			(40.0, "%", 0.0, 100.0),
			(7.0, "semitones", -24.0, 24.0),
			(4.0, "ratio", 1.0, 20.0),
			(0.25, "s", 0.1, 20.0),
		] {
			let text = format_value(v, unit);
			let back = parse(&text, unit, min, max).value;
			assert!(close(back, v), "{text} -> {back}");
		}
	}

	fn params(pairs: &[(&str, f32)]) -> BTreeMap<String, f32> {
		pairs.iter().map(|(k, v)| ((*k).to_owned(), *v)).collect()
	}

	#[test]
	fn preset_dirty_compares_effective_params() {
		let reverb = cadenza_dsp::device_info("reverb").unwrap();
		// Missing = default, so an explicit default is not a change.
		assert!(!params_differ(
			reverb,
			&params(&[]),
			&params(&[("size", 50.0)])
		));
		assert!(params_differ(
			reverb,
			&params(&[("size", 60.0)]),
			&params(&[])
		));
		// Unknown ids are ignored, tiny float noise too.
		assert!(!params_differ(
			reverb,
			&params(&[("nope", 1.0)]),
			&params(&[])
		));
		assert!(!params_differ(
			reverb,
			&params(&[("decay", 2.000_000_1)]),
			&params(&[])
		));
		let loaded = LoadedPreset {
			name: "Room".into(),
			user: true,
			params: params(&[("size", 20.0)]),
		};
		let status = preset_status(reverb, &params(&[("size", 20.0)]), Some(&loaded), &[]);
		assert_eq!(status.name.as_deref(), Some("Room"));
		assert!(status.user && !status.dirty);
		let edited = preset_status(reverb, &params(&[("size", 25.0)]), Some(&loaded), &[]);
		assert!(edited.dirty);
	}

	#[test]
	fn preset_status_recognizes_factory_presets() {
		let reverb = cadenza_dsp::device_info("reverb").unwrap();
		let factory = cadenza_dsp::factory_presets("reverb");
		let first = &factory[0];
		let status = preset_status(reverb, &factory_params(first), None, factory);
		assert_eq!(status.name.as_deref(), Some(first.name));
		assert!(!status.dirty);
		let mut moved = factory_params(first);
		moved.insert("pre_delay".into(), 249.0);
		assert_eq!(
			preset_status(reverb, &moved, None, factory),
			PresetStatus::default()
		);
	}

	#[test]
	fn preset_groups_follow_the_category_order_with_banks_after() {
		let entry = |name: &str, category: &str| PresetEntry {
			name: name.into(),
			user: false,
			category: category.into(),
			description: String::new(),
		};
		let list = [
			entry("Pad 1", "Pad"),
			entry("Bank A 1", "Bank A"),
			entry("Bass 1", "Bass"),
			entry("Loose", ""),
			entry("Pad 2", "Pad"),
		];
		let refs: Vec<&PresetEntry> = list.iter().collect();
		let groups: Vec<(&str, Vec<&str>)> = preset_groups(&refs)
			.into_iter()
			.map(|(c, g)| (c, g.iter().map(|e| e.name.as_str()).collect()))
			.collect();
		assert_eq!(
			groups,
			[
				("", vec!["Loose"]),
				("Bass", vec!["Bass 1"]),
				("Pad", vec!["Pad 1", "Pad 2"]),
				("Bank A", vec!["Bank A 1"]),
			]
		);
	}

	#[test]
	fn presets_step_and_wrap() {
		let list: Vec<PresetEntry> = ["A", "B", "C"]
			.iter()
			.map(|n| PresetEntry {
				name: (*n).into(),
				user: false,
				category: String::new(),
				description: String::new(),
			})
			.collect();
		let on = |n: &str| PresetStatus {
			name: Some(n.into()),
			user: false,
			dirty: true,
		};
		assert_eq!(step_preset(&list, &on("A"), 1), Some(1));
		assert_eq!(step_preset(&list, &on("A"), -1), Some(2));
		assert_eq!(step_preset(&list, &on("C"), 1), Some(0));
		assert_eq!(step_preset(&list, &PresetStatus::default(), 1), Some(0));
		assert_eq!(step_preset(&list, &PresetStatus::default(), -1), Some(2));
		assert_eq!(step_preset(&[], &on("A"), 1), None);
	}

	#[test]
	fn generic_body_groups_and_controls() {
		let synth = cadenza_dsp::device_info("poly_synth").unwrap();
		let all: Vec<&'static ParamInfo> = synth.params.iter().collect();
		let groups = param_groups(&all);
		let amp = groups
			.iter()
			.find(|g| g.title.as_deref() == Some("Amp"))
			.unwrap();
		let labels: Vec<&str> = amp.params.iter().map(|(_, l)| l.as_str()).collect();
		assert_eq!(labels, ["Attack", "Decay", "Sustain", "Release"]);
		let count: usize = groups.iter().map(|g| g.params.len()).sum();
		assert_eq!(count, synth.params.len());
		let eq = cadenza_dsp::device_info("eq").unwrap();
		let all: Vec<&'static ParamInfo> = eq.params.iter().collect();
		let titles: Vec<Option<String>> = param_groups(&all).into_iter().map(|g| g.title).collect();
		assert!(titles.contains(&Some("Peak 1".into())));
		assert!(titles.contains(&Some("Low Shelf".into())));

		let p = |d: &str, id: &str| *cadenza_dsp::device_info(d).unwrap().param(id).unwrap();
		assert_eq!(
			param_control("eq", &p("eq", "hp_slope")),
			ParamControl::Segmented(vec![
				"Off".into(),
				"12 dB".into(),
				"24 dB".into(),
				"48 dB".into()
			])
		);
		assert!(matches!(
			param_control("delay", &p("delay", "sync")),
			ParamControl::Grid(c, 7) if c.len() == 13 && c[2] == "1/16T"
		));
		assert_eq!(
			param_control("delay", &p("delay", "ping_pong")),
			ParamControl::Toggle("⇄ Ping-pong".into())
		);
		assert!(matches!(
			param_control("poly_synth", &p("poly_synth", "unison")),
			ParamControl::Segmented(c) if c == ["1", "2", "3", "4"]
		));
		assert_eq!(param_control("eq", &p("eq", "peak1_q")), ParamControl::Knob);
	}

	#[test]
	fn rows_pack_greedily() {
		assert_eq!(
			pack_rows(&[100.0, 100.0, 100.0, 250.0, 50.0], 250.0, 20.0),
			vec![vec![0, 1], vec![2], vec![3], vec![4]]
		);
		// Wider than the room: still one per row.
		assert_eq!(
			pack_rows(&[400.0, 10.0], 250.0, 20.0),
			vec![vec![0], vec![1]]
		);
		assert!(pack_rows(&[], 250.0, 20.0).is_empty());
	}

	#[test]
	fn header_items_drop_in_priority_order_as_the_width_shrinks() {
		// Plenty of room: everything full size.
		let items = header_items(1600.0, 300.0, 300.0, Some(190.0));
		assert_eq!(items.preset_width, FULL_PRESET_WIDTH);
		assert!(items.show_sidechain);
		assert!(!items.compact_meters);

		// Tight enough that the sidechain chip alone no longer fits: it goes first.
		let items = header_items(1100.0, 300.0, 300.0, Some(190.0));
		assert!(!items.show_sidechain);
		assert!(!items.compact_meters);
		assert_eq!(items.preset_width, FULL_PRESET_WIDTH);

		// Tighter still: the meters shrink to the compact pair before the preset box gives
		// up any width (matches the task's "shrink the meters ... before the compact
		// header kicks in").
		let items = header_items(900.0, 300.0, 300.0, Some(190.0));
		assert!(!items.show_sidechain);
		assert!(items.compact_meters);
		assert_eq!(items.preset_width, FULL_PRESET_WIDTH);

		// Very tight (just above the compact two-row header's own threshold): the preset
		// box narrows too, but never past its minimum, and nothing is asked to overlap.
		let items = header_items(820.0, 300.0, 300.0, Some(190.0));
		assert!(!items.show_sidechain);
		assert!(items.compact_meters);
		assert!(items.preset_width >= MIN_PRESET_WIDTH);
		assert!(items.preset_width < FULL_PRESET_WIDTH);
	}

	#[test]
	fn header_items_never_exceeds_the_available_width() {
		// Below `COMPACT_BELOW` (800) the two-row compact header takes over instead.
		for width in (800..=1700).step_by(10) {
			for sidechain in [None, Some(190.0)] {
				let items = header_items(width as f32, 300.0, 300.0, sidechain);
				let meters = if items.compact_meters { 34.0 } else { 248.0 };
				let sidechain_w = if items.show_sidechain {
					sidechain.unwrap_or(0.0)
				} else {
					0.0
				};
				let used = 600.0 + items.preset_width + sidechain_w + meters;
				assert!(
					used <= width as f32 + 0.01,
					"width {width}: used {used} > available"
				);
			}
		}
	}
}
