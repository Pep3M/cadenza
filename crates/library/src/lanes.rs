//! Lane keys and step strings of the pattern notation.

/// Drum lane names (General MIDI percussion notes). The first entries are the lanes
/// `write_drum_pattern` has always accepted; the rest add Latin percussion and cymbals.
pub const DRUM_LANES: &[(&str, u8)] = &[
	("kick", 36),
	("kick2", 35),
	("rim", 37),
	("snare", 38),
	("clap", 39),
	("snare2", 40),
	("low_tom", 41),
	("closed_hat", 42),
	("hat", 42),
	("pedal_hat", 44),
	("mid_tom", 45),
	("open_hat", 46),
	("high_tom", 48),
	("crash", 49),
	("ride", 51),
	("tambourine", 54),
	("cowbell", 56),
	("floor_tom", 43),
	("china", 52),
	("ride_bell", 53),
	("splash", 55),
	("crash2", 57),
	("hi_bongo", 60),
	("low_bongo", 61),
	("mute_conga", 62),
	("open_conga", 63),
	("low_conga", 64),
	("high_timbale", 65),
	("low_timbale", 66),
	("high_agogo", 67),
	("low_agogo", 68),
	("cabasa", 69),
	("shaker", 70),
	("maracas", 70),
	("short_guiro", 73),
	("guiro", 74),
	("claves", 75),
	("clave", 75),
	("hi_woodblock", 76),
	("low_woodblock", 77),
	("triangle", 81),
];

/// The GM note of a drum lane name.
pub fn drum_note(name: &str) -> Option<u8> {
	let key = name.trim().to_lowercase();
	DRUM_LANES.iter().find(|(l, _)| *l == key).map(|(_, n)| *n)
}

/// The first lane name of a GM note (for importers and previews).
pub fn drum_name(note: u8) -> Option<&'static str> {
	DRUM_LANES.iter().find(|(_, n)| *n == note).map(|(l, _)| *l)
}

/// What a lane plays. Octave suffixes (`R-1`, `5+1`, `^3+1`) are in `octave`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaneKey {
	/// A drum lane (GM note).
	Drum(u8),
	/// A chord tone of the chord at that step: 1 (root, written `R`), 3, 5, 7, 9, 11, 13.
	ChordTone { tone: u8, octave: i32 },
	/// The whole chord, voiced with the pattern's voicing.
	Chord { octave: i32 },
	/// The chord's bass note (slash bass aware).
	Bass { octave: i32 },
	/// Chromatic approach: a semitone below the next chord's root.
	Approach { octave: i32 },
	/// Anticipation: the next chord's root, played early (tumbao, montuno pushes).
	Anticipation { octave: i32 },
	/// Scale degree of the key (1..=7) with an accidental (-1, 0, +1).
	Degree {
		degree: u8,
		accidental: i8,
		octave: i32,
	},
	/// An absolute note.
	Absolute(u8),
}

impl LaneKey {
	/// Parses a lane key; the error text says what is accepted.
	pub fn parse(key: &str) -> Result<Self, String> {
		let k = key.trim();
		if let Some(n) = drum_note(k) {
			return Ok(LaneKey::Drum(n));
		}
		if let Some(rest) = k.strip_prefix('^') {
			let (base, octave) = split_octave(rest);
			let (accidental, digits) = match base.as_bytes().first() {
				Some(b'b') => (-1, &base[1..]),
				Some(b'#') => (1, &base[1..]),
				_ => (0, base),
			};
			let degree: u8 = digits
				.parse()
				.ok()
				.filter(|d| (1..=7).contains(d))
				.ok_or_else(|| {
					format!(
						"lane '{key}': scale degrees are ^1..^7 with an optional b/# and octave \
						 (^b3, ^#4, ^5+1)"
					)
				})?;
			return Ok(LaneKey::Degree {
				degree,
				accidental,
				octave,
			});
		}
		let (base, octave) = split_octave(k);
		match base {
			"R" => return Ok(LaneKey::ChordTone { tone: 1, octave }),
			"3" | "5" | "7" | "9" | "11" | "13" => {
				return Ok(LaneKey::ChordTone {
					tone: base.parse().expect("digits"),
					octave,
				});
			}
			"chord" => return Ok(LaneKey::Chord { octave }),
			"bass" => return Ok(LaneKey::Bass { octave }),
			"app" => return Ok(LaneKey::Approach { octave }),
			"ant" => return Ok(LaneKey::Anticipation { octave }),
			_ => {}
		}
		cadenza_theory::parse_pitch(k)
			.map(LaneKey::Absolute)
			.map_err(|_| {
				format!(
					"lane '{key}' is not a drum name (kick, snare, closed_hat, shaker, claves, \
					 hi_bongo, open_conga...), a chord tone (R 3 5 7 9 11 13, chord, bass, app, \
					 ant), a scale degree (^1..^7) or a note (C4, 60)"
				)
			})
	}

	/// Needs the chord at each step.
	pub fn needs_chords(self) -> bool {
		matches!(
			self,
			LaneKey::ChordTone { .. }
				| LaneKey::Chord { .. }
				| LaneKey::Bass { .. }
				| LaneKey::Approach { .. }
				| LaneKey::Anticipation { .. }
		)
	}

	pub fn needs_key(self) -> bool {
		matches!(self, LaneKey::Degree { .. })
	}

	pub fn is_drum(self) -> bool {
		matches!(self, LaneKey::Drum(_))
	}
}

/// Splits a trailing `+n` / `-n` octave suffix.
fn split_octave(s: &str) -> (&str, i32) {
	if let Some(i) = s.rfind(['+', '-'])
		&& i > 0
		&& let Ok(n) = s[i + 1..].parse::<u8>()
		&& n <= 4
	{
		let n = n as i32;
		return (&s[..i], if s.as_bytes()[i] == b'-' { -n } else { n });
	}
	(s, 0)
}

/// One grid step.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Step {
	/// A note with this velocity (X 1.0, x 0.8, o 0.45).
	Hit(f32),
	/// Extends the previous note of the lane by one step.
	Tie,
	Rest,
}

pub const ACCENT: f32 = 1.0;
pub const NORMAL: f32 = 0.8;
pub const GHOST: f32 = 0.45;

/// Parses a step string; spaces and `|` are ignored. Errors name the bad character.
pub fn parse_steps(s: &str) -> Result<Vec<Step>, char> {
	s.chars()
		.filter(|c| !matches!(c, ' ' | '|'))
		.map(|c| match c {
			'X' => Ok(Step::Hit(ACCENT)),
			'x' => Ok(Step::Hit(NORMAL)),
			'o' => Ok(Step::Hit(GHOST)),
			'=' => Ok(Step::Tie),
			'.' | '-' => Ok(Step::Rest),
			other => Err(other),
		})
		.collect()
}

/// The step character closest to a velocity (importers).
pub fn step_char(velocity: f32) -> char {
	if velocity >= 0.9 {
		'X'
	} else if velocity >= 0.6 {
		'x'
	} else {
		'o'
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn lane_keys_parse() {
		assert_eq!(LaneKey::parse("kick"), Ok(LaneKey::Drum(36)));
		assert_eq!(LaneKey::parse("claves"), Ok(LaneKey::Drum(75)));
		assert_eq!(
			LaneKey::parse("R-1"),
			Ok(LaneKey::ChordTone {
				tone: 1,
				octave: -1
			})
		);
		assert_eq!(
			LaneKey::parse("5+1"),
			Ok(LaneKey::ChordTone { tone: 5, octave: 1 })
		);
		assert_eq!(
			LaneKey::parse("^b3+1"),
			Ok(LaneKey::Degree {
				degree: 3,
				accidental: -1,
				octave: 1
			})
		);
		assert_eq!(LaneKey::parse("chord"), Ok(LaneKey::Chord { octave: 0 }));
		assert_eq!(
			LaneKey::parse("ant-1"),
			Ok(LaneKey::Anticipation { octave: -1 })
		);
		assert_eq!(LaneKey::parse("C-1"), Ok(LaneKey::Absolute(0)));
		assert_eq!(LaneKey::parse("60"), Ok(LaneKey::Absolute(60)));
		assert!(LaneKey::parse("^8").is_err());
		assert!(LaneKey::parse("bongo").is_err());
	}

	#[test]
	fn steps_parse_and_reject_unknown_characters() {
		assert_eq!(
			parse_steps("X=.o | x-"),
			Ok(vec![
				Step::Hit(1.0),
				Step::Tie,
				Step::Rest,
				Step::Hit(0.45),
				Step::Hit(0.8),
				Step::Rest
			])
		);
		assert_eq!(parse_steps("x?"), Err('?'));
	}
}
