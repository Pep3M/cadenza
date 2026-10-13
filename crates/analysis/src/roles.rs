//! Arrangement roles of tracks and the mix defaults that follow from them: relative level,
//! high-pass frequency, pan spread and which frequency bands a role "owns" when two parts
//! fight for the same range.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Role {
	Kick,
	Drums,
	Perc,
	Bass,
	Keys,
	Guitar,
	Pad,
	Lead,
	Vocal,
	Strings,
	Brass,
	Fx,
	Other,
}

impl Role {
	pub const ALL: [Role; 13] = [
		Role::Kick,
		Role::Drums,
		Role::Perc,
		Role::Bass,
		Role::Keys,
		Role::Guitar,
		Role::Pad,
		Role::Lead,
		Role::Vocal,
		Role::Strings,
		Role::Brass,
		Role::Fx,
		Role::Other,
	];

	pub fn name(self) -> &'static str {
		match self {
			Role::Kick => "kick",
			Role::Drums => "drums",
			Role::Perc => "perc",
			Role::Bass => "bass",
			Role::Keys => "keys",
			Role::Guitar => "guitar",
			Role::Pad => "pad",
			Role::Lead => "lead",
			Role::Vocal => "vocal",
			Role::Strings => "strings",
			Role::Brass => "brass",
			Role::Fx => "fx",
			Role::Other => "other",
		}
	}

	/// Parses a role name or a style library role ("comping" -> keys, "hook" -> lead...).
	pub fn parse(text: &str) -> Option<Role> {
		let t = text.trim().to_lowercase();
		Some(match t.as_str() {
			"kick" => Role::Kick,
			"drums" | "drum" | "beat" | "kit" | "fill" => Role::Drums,
			"perc" | "percussion" | "clave" => Role::Perc,
			"bass" | "808" | "sub" => Role::Bass,
			"keys" | "piano" | "comping" | "chords" | "organ" | "rhodes" => Role::Keys,
			"guitar" | "gtr" => Role::Guitar,
			"pad" | "pads" => Role::Pad,
			"lead" | "hook" | "melody" | "arp" | "counter" | "pluck" => Role::Lead,
			"vocal" | "vocals" | "vox" | "voice" => Role::Vocal,
			"strings" => Role::Strings,
			"brass" | "horns" => Role::Brass,
			"fx" | "sfx" => Role::Fx,
			"other" => Role::Other,
			_ => return None,
		})
	}

	/// Typical level relative to the mix's loudest parts (dB of integrated loudness):
	/// a starting balance, refined by ear and by the style.
	pub fn balance_db(self) -> f32 {
		match self {
			Role::Kick | Role::Drums | Role::Vocal => 0.0,
			Role::Bass => -1.0,
			Role::Lead => -2.0,
			Role::Keys | Role::Guitar => -5.0,
			Role::Brass => -6.0,
			Role::Strings => -7.0,
			Role::Pad | Role::Perc => -9.0,
			Role::Fx => -12.0,
			Role::Other => -6.0,
		}
	}

	/// High-pass corner that keeps the role's useful range (None: it needs the lows).
	pub fn high_pass_hz(self) -> Option<f32> {
		match self {
			Role::Kick | Role::Bass | Role::Drums => None,
			Role::Perc => Some(250.0),
			Role::Keys => Some(100.0),
			Role::Guitar => Some(90.0),
			Role::Pad | Role::Lead | Role::Fx => Some(150.0),
			Role::Vocal => Some(90.0),
			Role::Strings => Some(70.0),
			Role::Brass => Some(110.0),
			Role::Other => Some(60.0),
		}
	}

	/// Whether the role usually sits off-center (spread across the stereo field).
	pub fn spreads(self) -> bool {
		matches!(
			self,
			Role::Perc | Role::Guitar | Role::Keys | Role::Pad | Role::Strings | Role::Brass
		)
	}

	/// How much a role owns an octave band (index into `spectrum::OCTAVE_HZ`), 0..1: when
	/// two parts mask each other there, the lower owner gets the EQ cut.
	pub fn ownership(self, octave: usize) -> f32 {
		let table: [f32; 10] = match self {
			//          31   63  125  250  500   1k   2k   4k   8k  16k
			Role::Kick => [0.8, 1.0, 0.6, 0.3, 0.2, 0.2, 0.5, 0.5, 0.2, 0.1],
			Role::Bass => [0.9, 0.9, 1.0, 0.6, 0.3, 0.2, 0.2, 0.1, 0.1, 0.1],
			Role::Drums => [0.6, 0.8, 0.6, 0.4, 0.3, 0.3, 0.6, 0.7, 0.9, 0.9],
			Role::Perc => [0.1, 0.1, 0.2, 0.3, 0.3, 0.4, 0.5, 0.6, 0.7, 0.6],
			Role::Vocal => [0.1, 0.1, 0.3, 0.5, 0.7, 0.9, 1.0, 1.0, 0.6, 0.4],
			Role::Lead => [0.1, 0.1, 0.2, 0.4, 0.6, 0.9, 0.9, 0.8, 0.5, 0.3],
			Role::Keys => [0.1, 0.1, 0.3, 0.5, 0.7, 0.6, 0.4, 0.3, 0.2, 0.2],
			Role::Guitar => [0.1, 0.1, 0.3, 0.5, 0.6, 0.7, 0.6, 0.4, 0.3, 0.2],
			Role::Pad => [0.1, 0.1, 0.2, 0.3, 0.4, 0.3, 0.3, 0.3, 0.3, 0.3],
			Role::Strings => [0.1, 0.2, 0.4, 0.5, 0.6, 0.6, 0.5, 0.4, 0.3, 0.3],
			Role::Brass => [0.1, 0.1, 0.3, 0.5, 0.6, 0.7, 0.6, 0.4, 0.2, 0.1],
			Role::Fx => [0.1, 0.1, 0.1, 0.2, 0.2, 0.2, 0.2, 0.2, 0.2, 0.2],
			Role::Other => [0.3; 10],
		};
		table[octave.min(9)]
	}
}

/// The role of a track from its name, instrument and the style library roles of its clips
/// (e.g. "bass" from a clip named "Tumbao [core/bass_tumbao]"). Name words win, then the
/// instrument, then the clip hints.
pub fn infer_role(name: &str, instrument: Option<&str>, hints: &[&str]) -> Role {
	let lower = name.to_lowercase();
	let words: Vec<&str> = lower
		.split(|c: char| !c.is_alphanumeric())
		.filter(|w| !w.is_empty())
		.collect();
	let has = |keys: &[&str]| {
		words.iter().any(|w| {
			keys.iter()
				.any(|k| w == k || (k.len() > 3 && w.starts_with(k)))
		})
	};
	if has(&["kick", "bd"]) {
		return Role::Kick;
	}
	if has(&["808", "bass", "sub", "tumbao"]) {
		return Role::Bass;
	}
	if has(&[
		"drum", "drums", "snare", "hat", "hats", "beat", "kit", "breaks",
	]) {
		return Role::Drums;
	}
	if has(&[
		"perc", "conga", "congas", "bongo", "bongos", "shaker", "clave", "claves", "tamb",
		"timbale", "guiro", "cowbell",
	]) {
		return Role::Perc;
	}
	if has(&["vox", "vocal", "vocals", "voice", "choir", "singer"]) {
		return Role::Vocal;
	}
	if has(&["gtr", "guitar", "guitars", "nylon", "strum"]) {
		return Role::Guitar;
	}
	if has(&["pad", "pads", "atmos", "drone", "texture"]) {
		return Role::Pad;
	}
	if has(&["string", "strings", "violin", "viola", "cello", "orchestra"]) {
		return Role::Strings;
	}
	if has(&["brass", "horn", "horns", "trumpet", "trombone", "sax"]) {
		return Role::Brass;
	}
	if has(&[
		"lead", "hook", "melody", "pluck", "arp", "bell", "bells", "flute", "whistle",
	]) {
		return Role::Lead;
	}
	if has(&[
		"keys", "piano", "rhodes", "organ", "ep", "chords", "montuno", "comp",
	]) {
		return Role::Keys;
	}
	if has(&["fx", "sfx", "riser", "sweep", "noise", "impact"]) {
		return Role::Fx;
	}
	match instrument {
		Some("drum_machine" | "drum_sampler") => return Role::Drums,
		Some("electric_piano") => return Role::Keys,
		_ => {}
	}
	hints
		.iter()
		.find_map(|h| Role::parse(h))
		.unwrap_or(Role::Other)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn roles_from_names_instruments_and_hints() {
		assert_eq!(infer_role("Kick", None, &[]), Role::Kick);
		assert_eq!(infer_role("808 Bass", None, &[]), Role::Bass);
		assert_eq!(infer_role("Drums", Some("drum_machine"), &[]), Role::Drums);
		assert_eq!(
			infer_role("Track 3", Some("drum_sampler"), &[]),
			Role::Drums
		);
		assert_eq!(infer_role("Congas L", None, &[]), Role::Perc);
		assert_eq!(infer_role("Lead Vox", None, &[]), Role::Vocal);
		assert_eq!(
			infer_role("Rhodes", Some("electric_piano"), &[]),
			Role::Keys
		);
		assert_eq!(
			infer_role("Synth", Some("poly_synth"), &["comping"]),
			Role::Keys
		);
		assert_eq!(
			infer_role("Synth", Some("poly_synth"), &["hook"]),
			Role::Lead
		);
		assert_eq!(infer_role("Synth", Some("poly_synth"), &[]), Role::Other);
		assert_eq!(Role::parse("808"), Some(Role::Bass));
		for r in Role::ALL {
			assert_eq!(Role::parse(r.name()), Some(r));
		}
	}

	#[test]
	fn lows_belong_to_kick_and_bass() {
		assert!(Role::Bass.ownership(2) > Role::Keys.ownership(2));
		assert!(Role::Kick.ownership(1) > Role::Bass.ownership(1) - 0.2);
		assert!(Role::Vocal.ownership(6) > Role::Guitar.ownership(6));
		assert_eq!(Role::Bass.high_pass_hz(), None);
		assert!(Role::Pad.high_pass_hz().unwrap() > 100.0);
	}
}
