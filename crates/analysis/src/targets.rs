//! Mix targets: integrated loudness, true-peak ceiling, tonal balance profile and per-role
//! balance, from a style guide's mix section or genre defaults.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::roles::Role;

/// Genre a mix's tonal balance is compared against (see [`ToneProfile::curve`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ToneProfile {
	#[default]
	Pop,
	Electronic,
	Hiphop,
	Rock,
	Acoustic,
	Orchestral,
}

/// Typical band energy relative to the mid band (dB): a reference tonal balance rather than
/// a flat "mid is the only correct level" rule, since modern mixes routinely carry a hotter
/// low end and a rolled-off top compared with a literally flat spectrum.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ToneCurve {
	pub low: f32,
	pub low_mid: f32,
	pub high_mid: f32,
	pub high: f32,
}

impl ToneProfile {
	pub fn name(self) -> &'static str {
		match self {
			ToneProfile::Pop => "pop",
			ToneProfile::Electronic => "electronic",
			ToneProfile::Hiphop => "hiphop",
			ToneProfile::Rock => "rock",
			ToneProfile::Acoustic => "acoustic",
			ToneProfile::Orchestral => "orchestral",
		}
	}

	pub fn parse(text: &str) -> Option<Self> {
		Some(match text.trim().to_lowercase().as_str() {
			"pop" => Self::Pop,
			"electronic" => Self::Electronic,
			"hiphop" | "hip-hop" | "hip hop" => Self::Hiphop,
			"rock" => Self::Rock,
			"acoustic" => Self::Acoustic,
			"orchestral" => Self::Orchestral,
			_ => return None,
		})
	}

	/// The profile that best fits free genre words ("lo-fi hip hop", "house", "salsa").
	pub fn from_genre(text: &str) -> Self {
		let text = text.to_lowercase();
		let has = |words: &[&str]| words.iter().any(|w| text.contains(w));
		if has(&[
			"lo-fi",
			"lofi",
			"hip hop",
			"hip-hop",
			"hiphop",
			"trap",
			"rap",
			"boom bap",
			"reggaeton",
			"dembow",
		]) {
			Self::Hiphop
		} else if has(&[
			"edm",
			"house",
			"techno",
			"electronic",
			"synthwave",
			"trance",
			"dubstep",
			"drum and bass",
			"dnb",
			"electro",
		]) {
			Self::Electronic
		} else if has(&["rock", "metal", "punk", "grunge"]) {
			Self::Rock
		} else if has(&[
			"orchestral",
			"cinematic",
			"classical",
			"film score",
			"string quartet",
		]) {
			Self::Orchestral
		} else if has(&[
			"folk",
			"acoustic",
			"singer-songwriter",
			"unplugged",
			"salsa",
			"son ",
			"bolero",
			"jazz",
			"neo soul",
			"neo-soul",
		]) {
			Self::Acoustic
		} else {
			Self::Pop
		}
	}

	/// Reference curves, dB relative to the mid band. Starting points, not gospel.
	pub fn curve(self) -> ToneCurve {
		let c = |low, low_mid, high_mid, high| ToneCurve {
			low,
			low_mid,
			high_mid,
			high,
		};
		match self {
			ToneProfile::Pop => c(7.0, 3.0, -5.0, -8.0),
			ToneProfile::Electronic => c(9.0, 2.0, -4.0, -6.0),
			ToneProfile::Hiphop => c(10.0, 2.0, -6.0, -9.0),
			ToneProfile::Rock => c(4.0, 2.0, -2.0, -6.0),
			ToneProfile::Acoustic => c(2.0, 1.0, -3.0, -7.0),
			ToneProfile::Orchestral => c(1.0, 0.0, -4.0, -9.0),
		}
	}
}

/// What the mix aims for.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MixTargets {
	/// Integrated loudness of the master, LUFS.
	pub lufs: f32,
	/// True-peak ceiling of the master, dBTP.
	pub true_peak_db: f32,
	/// Tonal balance reference.
	pub profile: ToneProfile,
	/// Role balance overrides (dB relative to the loudest parts); other roles use
	/// [`Role::balance_db`].
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub roles: Vec<(Role, f32)>,
	/// The style's mix advice, verbatim.
	#[serde(default, skip_serializing_if = "String::is_empty")]
	pub notes: String,
	/// Where these came from: "style core/pop", "default" or "arguments".
	pub source: String,
}

/// Integrated loudness tolerance before the loudness issue fires, LU.
pub const LOUDNESS_TOLERANCE_LU: f32 = 1.5;

impl Default for MixTargets {
	/// A streaming-safe default: -14 LUFS, -1 dBTP, pop balance.
	fn default() -> Self {
		Self {
			lufs: -14.0,
			true_peak_db: -1.0,
			profile: ToneProfile::Pop,
			roles: Vec::new(),
			notes: String::new(),
			source: "default".into(),
		}
	}
}

impl MixTargets {
	pub fn balance_db(&self, role: Role) -> f32 {
		self.roles
			.iter()
			.find(|(r, _)| *r == role)
			.map_or(role.balance_db(), |(_, db)| *db)
	}

	/// Targets of a style guide's mix section: its loudness, optional true peak, profile
	/// (else guessed from the style's genre words) and role levels (by role name).
	pub fn from_style(
		style_id: &str,
		genre_words: &str,
		lufs: f64,
		true_peak_db: Option<f64>,
		profile: Option<&str>,
		roles: &[(String, f64)],
		notes: &str,
	) -> Self {
		Self {
			lufs: lufs as f32,
			true_peak_db: true_peak_db.unwrap_or(-1.0) as f32,
			profile: profile
				.and_then(ToneProfile::parse)
				.unwrap_or_else(|| ToneProfile::from_genre(genre_words)),
			roles: roles
				.iter()
				.filter_map(|(name, db)| Some((Role::parse(name)?, *db as f32)))
				.collect(),
			notes: notes.to_owned(),
			source: format!("style {style_id}"),
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn style_targets_and_defaults() {
		let t = MixTargets::from_style(
			"core/trap",
			"trap hip-hop",
			-8.0,
			None,
			None,
			&[
				("808".into(), 0.0),
				("melody".into(), -4.0),
				("x".into(), 1.0),
			],
			"808 and kick never fight",
		);
		assert_eq!(t.lufs, -8.0);
		assert_eq!(t.true_peak_db, -1.0);
		assert_eq!(t.profile, ToneProfile::Hiphop);
		assert_eq!(t.balance_db(Role::Bass), 0.0);
		assert_eq!(t.balance_db(Role::Lead), -4.0);
		assert_eq!(t.balance_db(Role::Pad), Role::Pad.balance_db());
		assert_eq!(t.source, "style core/trap");
		let d = MixTargets::default();
		assert_eq!((d.lufs, d.true_peak_db), (-14.0, -1.0));
		assert_eq!(
			ToneProfile::from_genre("Deep House"),
			ToneProfile::Electronic
		);
		assert_eq!(ToneProfile::from_genre("salsa dura"), ToneProfile::Acoustic);
		assert_eq!(ToneProfile::parse("hip-hop"), Some(ToneProfile::Hiphop));
	}
}
