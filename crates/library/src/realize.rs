//! Turning a pattern into notes for a key, a chord progression and a number of bars.
//!
//! Pure: everything (the pattern, its fill/variation from the same family, the groove)
//! is resolved by the caller. Chord-relative lanes read the chord sounding at each step;
//! scale-degree lanes read the key; drum lanes are GM notes. Bars of the clip cycle
//! through the pattern's bars; `fill_every` puts the family's fill in the last bar of
//! every n bars and `vary` alternates the main pattern and its variation per cycle.

use std::collections::HashMap;

use cadenza_theory::{Chord, NoteSpec, Scale, ScaleKind, VoicingStyle};

use crate::format::{Feel, Groove, Harmony, Pattern, parse_meter};
use crate::lanes::{LaneKey, Step, parse_steps};

/// Everything a pattern is realized against.
#[derive(Debug, Clone)]
pub struct RealizeOptions<'a> {
	/// Bars to write (default: the pattern's own length).
	pub bars: Option<u32>,
	/// Key for scale-degree lanes, diatonic chord-tone fallbacks and transposing
	/// absolute patterns.
	pub key: Option<Scale>,
	/// Chords cycling every `beats_per_chord` from the clip start.
	pub chords: Vec<Chord>,
	pub beats_per_chord: f64,
	/// Every n-th bar plays the family's fill (`fill`).
	pub fill_every: Option<u32>,
	/// Alternate the main pattern and `variation` per pattern cycle.
	pub vary: bool,
	pub fill: Option<&'a Pattern>,
	pub variation: Option<&'a Pattern>,
	/// Replaces the pattern's swing feel.
	pub groove: Option<&'a Groove>,
	/// Velocity multiplier.
	pub velocity: f32,
	/// Octave shift on top of the pattern's register.
	pub octave: i32,
}

impl Default for RealizeOptions<'_> {
	fn default() -> Self {
		Self {
			bars: None,
			key: None,
			chords: Vec::new(),
			beats_per_chord: 4.0,
			fill_every: None,
			vary: false,
			fill: None,
			variation: None,
			groove: None,
			velocity: 1.0,
			octave: 0,
		}
	}
}

/// The notes of a realized pattern (beats from the clip start).
#[derive(Debug, Clone, PartialEq)]
pub struct Realized {
	pub notes: Vec<NoteSpec>,
	pub bars: u32,
	pub length_beats: f64,
	pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum RealizeError {
	#[error(
		"pattern '{pattern}' needs chords: pass `chords` (e.g. \"Am F C G\"), roman numerals \
		 with a `key` (\"i VI III VII\"), or a progression id (\"core/prog_i_VI_III_VII\") \
		 with a `key`"
	)]
	NeedsChords { pattern: String },
	#[error(
		"pattern '{pattern}' uses scale degrees and needs a `key` (e.g. \"A minor\", \"C major\")"
	)]
	NeedsKey { pattern: String },
	#[error("pattern '{pattern}' is invalid: {message}")]
	Invalid { pattern: String, message: String },
}

struct Lane<'a> {
	name: &'a str,
	key: LaneKey,
	steps: Vec<Step>,
}

struct Prepared<'a> {
	pattern: &'a Pattern,
	steps_per_bar: usize,
	step_beats: f64,
	feel: Feel,
	lanes: Vec<Lane<'a>>,
	/// Semitones added to absolute lanes (written key -> target key).
	transpose: i32,
}

fn prepare<'a>(pattern: &'a Pattern, key: Option<&Scale>) -> Result<Prepared<'a>, RealizeError> {
	let invalid = |message: String| RealizeError::Invalid {
		pattern: pattern.id.clone(),
		message,
	};
	let steps_per_bar = pattern
		.steps_per_bar()
		.ok_or_else(|| invalid(format!("meter '{}' does not fit the grid", pattern.meter)))?;
	let feel =
		Feel::parse(&pattern.feel).ok_or_else(|| invalid(format!("feel '{}'", pattern.feel)))?;
	let mut lanes = Vec::with_capacity(pattern.lanes.len());
	for (name, steps) in pattern.lanes.iter() {
		let key = LaneKey::parse(name).map_err(invalid)?;
		let steps = parse_steps(steps)
			.map_err(|c| invalid(format!("lane '{name}' has unrecognized step '{c}'")))?;
		if steps.is_empty() {
			return Err(invalid(format!("lane '{name}' is empty")));
		}
		lanes.push(Lane { name, key, steps });
	}
	let transpose = match (pattern.harmony, &pattern.key, key) {
		(Harmony::Absolute, Some(from), Some(to)) => {
			let from = Scale::parse(from).map_err(|e| invalid(e.to_string()))?;
			let d = (to.root.0 as i32 - from.root.0 as i32).rem_euclid(12);
			if d > 6 { d - 12 } else { d }
		}
		_ => 0,
	};
	Ok(Prepared {
		pattern,
		steps_per_bar,
		step_beats: 1.0 / pattern.steps_per_beat as f64,
		feel,
		lanes,
		transpose,
	})
}

struct Pending {
	pitch: u8,
	start: f64,
	length: f64,
	velocity: f32,
	offset: f64,
}

/// The seven-tone scale degrees are read in (pentatonic keys use their parent scale).
fn heptatonic(kind: ScaleKind) -> &'static [u8] {
	match kind {
		ScaleKind::MinorPentatonic | ScaleKind::Blues => ScaleKind::NaturalMinor.intervals(),
		ScaleKind::MajorPentatonic | ScaleKind::WholeTone | ScaleKind::Chromatic => {
			ScaleKind::Major.intervals()
		}
		other => other.intervals(),
	}
}

/// Semitones from `root` to the scale tone `steps` diatonic steps above it, when the root
/// belongs to the key.
fn diatonic_above(key: Option<&Scale>, root: u8, steps: usize) -> Option<i32> {
	let key = key?;
	let ivs = heptatonic(key.kind);
	let rel = (root as i32 - key.root.0 as i32).rem_euclid(12) as u8;
	let pos = ivs.iter().position(|&iv| iv == rel)?;
	let target = pos + steps;
	let iv = ivs[target % 7] as i32 + 12 * (target / 7) as i32;
	Some(iv - ivs[pos] as i32)
}

/// Semitones above the chord root of chord tone `tone` (1, 3, 5... 13): the chord's own
/// tone when it has one, else the key's diatonic tone, else a plain default (a seventh
/// falls back to the octave so a triad never gets a foreign seventh).
fn chord_tone(chord: &Chord, tone: u8, key: Option<&Scale>) -> i32 {
	let has = |set: &[u8]| {
		set.iter()
			.find(|iv| chord.intervals.contains(iv))
			.map(|&iv| iv as i32)
	};
	let diatonic = |steps| diatonic_above(key, chord.root.0, steps);
	match tone {
		1 => 0,
		3 => has(&[3, 4, 5, 2]).or_else(|| diatonic(2)).unwrap_or(4),
		5 => has(&[7, 6, 8]).unwrap_or(7),
		7 => has(&[10, 11])
			.or_else(|| (chord.intervals.contains(&6) && chord.intervals.contains(&9)).then_some(9))
			.or_else(|| diatonic(6))
			.unwrap_or(12),
		9 => has(&[14, 13, 15])
			.or_else(|| has(&[2]).map(|iv| iv + 12))
			.or_else(|| diatonic(8))
			.unwrap_or(14),
		11 => has(&[17, 18]).or_else(|| diatonic(10)).unwrap_or(17),
		_ => has(&[21, 20, 22]).or_else(|| diatonic(12)).unwrap_or(21),
	}
}

/// The pitch of pitch class `pc` in `[center - 6, center + 5]`.
fn place(pc: u8, center: i32) -> i32 {
	let low = center - 6;
	low + (pc as i32 - low).rem_euclid(12)
}

fn in_range(p: i32) -> Option<u8> {
	(0..=127).contains(&p).then_some(p as u8)
}

struct Harmonizer<'a> {
	opts: &'a RealizeOptions<'a>,
	/// Last voicing of each `chord` lane (voice leading).
	voiced: HashMap<String, Vec<u8>>,
}

impl Harmonizer<'_> {
	fn chord_index(&self, beat: f64) -> usize {
		let bpc = if self.opts.beats_per_chord > 0.0 {
			self.opts.beats_per_chord
		} else {
			4.0
		};
		((beat + 1e-6) / bpc).floor().max(0.0) as usize
	}

	fn chord(&self, i: usize) -> &Chord {
		&self.opts.chords[i % self.opts.chords.len()]
	}

	fn pitches(&mut self, prep: &Prepared, lane: &Lane, beat: f64) -> Vec<u8> {
		let pattern = prep.pattern;
		let shift = |octave: i32| 12 * (pattern.octave.unwrap_or(0) + self.opts.octave + octave);
		let center = |octave: i32| pattern.role.center() + shift(octave);
		let key = self.opts.key.as_ref();
		match lane.key {
			LaneKey::Drum(n) => vec![n],
			LaneKey::Absolute(n) => in_range(n as i32 + prep.transpose + shift(0))
				.into_iter()
				.collect(),
			LaneKey::ChordTone { tone, octave } => {
				let chord = self.chord(self.chord_index(beat));
				let root = place(chord.root.0, center(octave));
				in_range(root + chord_tone(chord, tone, key))
					.into_iter()
					.collect()
			}
			LaneKey::Bass { octave } => {
				let chord = self.chord(self.chord_index(beat));
				let pc = chord.bass.unwrap_or(chord.root).0;
				in_range(place(pc, center(octave))).into_iter().collect()
			}
			LaneKey::Approach { octave } | LaneKey::Anticipation { octave } => {
				let next = self.chord(self.chord_index(beat) + 1);
				let below = matches!(lane.key, LaneKey::Approach { .. }) as i32;
				in_range(place(next.root.0, center(octave)) - below)
					.into_iter()
					.collect()
			}
			LaneKey::Chord { octave } => {
				let chord = self.chord(self.chord_index(beat)).clone();
				let voicing = pattern.voicing.unwrap_or(VoicingStyle::Close);
				let c = center(octave).clamp(24, 108) as u8;
				let previous = self.voiced.get(lane.name).cloned();
				let voiced = cadenza_theory::voice(&chord, voicing, c, previous.as_deref());
				self.voiced.insert(lane.name.to_owned(), voiced.clone());
				voiced
			}
			LaneKey::Degree {
				degree,
				accidental,
				octave,
			} => {
				let Some(key) = key else {
					return Vec::new();
				};
				let tonic = place(key.root.0, center(octave));
				let iv = heptatonic(key.kind)[degree as usize - 1] as i32 + accidental as i32;
				in_range(tonic + iv).into_iter().collect()
			}
		}
	}
}

/// A fill/variation prepared for use, or `None` (with a warning) when its meter differs.
fn alternate<'a>(
	pattern: &Pattern,
	other: Option<&'a Pattern>,
	what: &str,
	key: Option<&Scale>,
	warnings: &mut Vec<String>,
) -> Result<Option<Prepared<'a>>, RealizeError> {
	match other {
		Some(p) if parse_meter(&p.meter) == parse_meter(&pattern.meter) => {
			prepare(p, key).map(Some)
		}
		Some(p) => {
			warnings.push(format!(
				"{what} '{}' is in {}, not {}: not used",
				p.id, p.meter, pattern.meter
			));
			Ok(None)
		}
		None => Ok(None),
	}
}

/// Realizes `pattern` (see the module docs).
pub fn realize(pattern: &Pattern, opts: &RealizeOptions) -> Result<Realized, RealizeError> {
	let key = opts.key.as_ref();
	let main = prepare(pattern, key)?;
	let mut warnings = Vec::new();
	let fill = if opts.fill_every.is_some_and(|n| n > 0) {
		let fill = alternate(pattern, opts.fill, "fill", key, &mut warnings)?;
		if fill.is_none() && opts.fill.is_none() {
			warnings.push(format!(
				"pattern '{}' has no fill in its family: fill_every ignored",
				pattern.id
			));
		}
		fill
	} else {
		None
	};
	let variation = if opts.vary {
		let variation = alternate(pattern, opts.variation, "variation", key, &mut warnings)?;
		if variation.is_none() && opts.variation.is_none() {
			warnings.push(format!(
				"pattern '{}' has no variation in its family: vary ignored",
				pattern.id
			));
		}
		variation
	} else {
		None
	};
	let sources: Vec<&Prepared> = [Some(&main), fill.as_ref(), variation.as_ref()]
		.into_iter()
		.flatten()
		.collect();
	let lanes = || sources.iter().flat_map(|s| s.lanes.iter());
	if opts.chords.is_empty() && lanes().any(|l| l.key.needs_chords()) {
		return Err(RealizeError::NeedsChords {
			pattern: pattern.id.clone(),
		});
	}
	if key.is_none() && lanes().any(|l| l.key.needs_key()) {
		return Err(RealizeError::NeedsKey {
			pattern: pattern.id.clone(),
		});
	}
	if pattern.harmony == Harmony::Absolute && pattern.key.is_some() && key.is_none() {
		warnings.push(format!(
			"pattern '{}' is written in {}; pass `key` to transpose it",
			pattern.id,
			pattern.key.as_deref().unwrap_or_default()
		));
	}

	let bars = opts.bars.unwrap_or(pattern.bars).max(1);
	let bar_beats = pattern.bar_beats();
	let mut harmonizer = Harmonizer {
		opts,
		voiced: HashMap::new(),
	};
	let mut notes: Vec<Pending> = Vec::new();
	// Per lane name: the notes its last hit started and where they end (ties extend them).
	let mut open: HashMap<&str, (Vec<usize>, f64)> = HashMap::new();
	for bar in 0..bars {
		let (src, src_bar) = match (&fill, opts.fill_every) {
			(Some(f), Some(n)) if n > 0 && (bar + 1) % n == 0 => (f, f.pattern.bars.max(1) - 1),
			_ => {
				let cycle = bar / pattern.bars.max(1);
				match &variation {
					Some(v) if cycle % 2 == 1 => (v, bar % v.pattern.bars.max(1)),
					_ => (&main, bar % pattern.bars.max(1)),
				}
			}
		};
		let bar_start = bar as f64 * bar_beats;
		for lane in &src.lanes {
			for s in 0..src.steps_per_bar {
				let idx = (src_bar as usize * src.steps_per_bar + s) % lane.steps.len();
				let beat = bar_start + s as f64 * src.step_beats;
				match lane.steps[idx] {
					Step::Rest => {
						open.remove(lane.name);
					}
					Step::Tie => {
						if let Some((ids, end)) = open.get_mut(lane.name)
							&& (*end - beat).abs() < 1e-6
						{
							for &i in ids.iter() {
								notes[i].length += src.step_beats;
							}
							*end += src.step_beats;
						}
					}
					Step::Hit(velocity) => {
						let mut offset = match (opts.groove, src.feel) {
							(None, Feel::Swing(amount)) if s % 2 == 1 => {
								amount * src.step_beats * 0.5
							}
							_ => 0.0,
						};
						let mut velocity = velocity * opts.velocity;
						if let Some(groove) = opts.groove
							&& !groove.timing.is_empty()
						{
							let g = beat * groove.steps_per_beat.max(1) as f64;
							if (g - g.round()).abs() < 1e-6 {
								let i = g.round() as usize;
								offset += groove.timing[i % groove.timing.len()]
									/ groove.steps_per_beat.max(1) as f64;
								if !groove.velocity.is_empty() {
									velocity *= groove.velocity[i % groove.velocity.len()] as f32;
								}
							}
						}
						let velocity = velocity.clamp(0.05, 1.0);
						let mut ids = Vec::new();
						for pitch in harmonizer.pitches(src, lane, beat) {
							ids.push(notes.len());
							notes.push(Pending {
								pitch,
								start: beat,
								length: src.step_beats,
								velocity,
								offset,
							});
						}
						open.insert(lane.name, (ids, beat + src.step_beats));
					}
				}
			}
		}
	}
	let mut out: Vec<NoteSpec> = notes
		.into_iter()
		.map(|n| NoteSpec {
			pitch: n.pitch,
			start_beats: (n.start + n.offset).max(0.0),
			length_beats: n.length,
			velocity: n.velocity,
		})
		.collect();
	out.sort_by(|a, b| {
		a.start_beats
			.total_cmp(&b.start_beats)
			.then(a.pitch.cmp(&b.pitch))
	});
	Ok(Realized {
		notes: out,
		bars,
		length_beats: bars as f64 * bar_beats,
		warnings,
	})
}

/// Parses chords given as symbols ("Am F C G") or, with a key, roman numerals
/// ("i VI III VII"); separators: spaces, '-', '|', ','. Tokens may mix both.
pub fn parse_chords(input: &str, key: Option<&Scale>) -> Result<Vec<Chord>, String> {
	let tokens: Vec<String> = input
		.chars()
		.map(|c| if matches!(c, '-' | '|' | ',') { ' ' } else { c })
		.collect::<String>()
		.split_whitespace()
		.map(str::to_owned)
		.collect();
	if tokens.is_empty() {
		return Err("chords are empty: pass symbols like \"Am F C G\"".into());
	}
	tokens
		.iter()
		.map(|t| {
			if is_roman(t) {
				let key = key.ok_or_else(|| {
					format!("'{t}' is a roman numeral: pass `key` (e.g. \"A minor\") with it")
				})?;
				cadenza_theory::parse_roman(t, key).map_err(|e| e.to_string())
			} else {
				Chord::parse(t).map_err(|e| e.to_string())
			}
		})
		.collect()
}

/// Roman numerals start (after an optional lower-case `b`/`#`) with I or V in any case;
/// chord symbols start with an upper-case letter A-G.
pub fn is_roman(token: &str) -> bool {
	let t = token.trim_start_matches(['b', '#', '♭', '♯']);
	matches!(t.chars().next(), Some('I' | 'V' | 'i' | 'v'))
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::format::{OrderedMap, Role, Variant};

	fn pattern(role: Role, harmony: Harmony, lanes: &[(&str, &str)]) -> Pattern {
		Pattern {
			id: "test/p".into(),
			name: "P".into(),
			summary: "s".into(),
			role,
			genres: vec![],
			sections: vec![],
			energy: 3,
			feel: "straight".into(),
			meter: "4/4".into(),
			bars: 1,
			tempo: None,
			family: None,
			variant: Variant::Main,
			harmony,
			key: None,
			voicing: None,
			octave: None,
			steps_per_beat: 4,
			lanes: OrderedMap(
				lanes
					.iter()
					.map(|(k, v)| (k.to_string(), v.to_string()))
					.collect(),
			),
			pairs_with: vec![],
			groove: None,
			tags: vec![],
		}
	}

	fn key(s: &str) -> Option<Scale> {
		Some(Scale::parse(s).unwrap())
	}

	#[test]
	fn drum_lanes_become_gm_notes_with_step_velocities() {
		let p = pattern(
			Role::Drums,
			Harmony::None,
			&[("kick", "X...x...x...x..."), ("snare", "...x..o....x..x.")],
		);
		let r = realize(&p, &RealizeOptions::default()).unwrap();
		assert_eq!(r.length_beats, 4.0);
		let kicks: Vec<_> = r.notes.iter().filter(|n| n.pitch == 36).collect();
		assert_eq!(kicks.len(), 4);
		assert_eq!(kicks[0].velocity, 1.0);
		assert_eq!(kicks[1].start_beats, 1.0);
		let snares: Vec<f64> = r
			.notes
			.iter()
			.filter(|n| n.pitch == 38)
			.map(|n| n.start_beats)
			.collect();
		assert_eq!(snares, vec![0.75, 1.5, 2.75, 3.5]);
		assert_eq!(
			r.notes
				.iter()
				.find(|n| n.start_beats == 1.5)
				.unwrap()
				.velocity,
			0.45
		);
	}

	#[test]
	fn chord_tones_follow_the_progression_and_ties_extend() {
		let p = pattern(
			Role::Bass,
			Harmony::Chord,
			&[("R", "x=====x=====x=.."), ("5", "..............x.")],
		);
		let opts = RealizeOptions {
			bars: Some(2),
			chords: parse_chords("Am F", None).unwrap(),
			..Default::default()
		};
		let r = realize(&p, &opts).unwrap();
		// Bar 1 on A (A2 = 45), bar 2 on F (F2 = 41); the fifth of A is E3 (52).
		let first: Vec<(u8, f64, f64)> = r
			.notes
			.iter()
			.take(4)
			.map(|n| (n.pitch, n.start_beats, n.length_beats))
			.collect();
		assert_eq!(
			first,
			vec![
				(45, 0.0, 1.5),
				(45, 1.5, 1.5),
				(45, 3.0, 0.5),
				(52, 3.5, 0.25)
			]
		);
		assert_eq!(r.notes[4].pitch, 41);
		assert_eq!(r.notes[4].start_beats, 4.0);
	}

	#[test]
	fn chord_relative_needs_chords_and_degrees_need_a_key() {
		let p = pattern(Role::Bass, Harmony::Chord, &[("R", "x...")]);
		let err = realize(&p, &RealizeOptions::default()).unwrap_err();
		assert!(err.to_string().contains("needs chords"), "{err}");
		let p = pattern(Role::Lead, Harmony::Scale, &[("^1", "x...")]);
		let err = realize(&p, &RealizeOptions::default()).unwrap_err();
		assert!(err.to_string().contains("needs a `key`"), "{err}");
	}

	#[test]
	fn scale_degrees_read_the_key() {
		let p = pattern(
			Role::Lead,
			Harmony::Scale,
			&[
				("^1", "x..............."),
				("^3", "....x..........."),
				("^5+1", "........x......."),
			],
		);
		let r = realize(
			&p,
			&RealizeOptions {
				key: key("A minor"),
				..Default::default()
			},
		)
		.unwrap();
		let pitches: Vec<u8> = r.notes.iter().map(|n| n.pitch).collect();
		// A4, C5, E6.
		assert_eq!(pitches, vec![69, 72, 88]);
	}

	#[test]
	fn sevenths_fall_back_to_the_key_or_the_octave() {
		let p = pattern(Role::Comping, Harmony::Chord, &[("7", "x...............")]);
		let chords = parse_chords("C", None).unwrap();
		let with_key = realize(
			&p,
			&RealizeOptions {
				chords: chords.clone(),
				key: key("C major"),
				..Default::default()
			},
		)
		.unwrap();
		assert_eq!(with_key.notes[0].pitch, 71); // B4 over C4
		let without = realize(
			&p,
			&RealizeOptions {
				chords,
				..Default::default()
			},
		)
		.unwrap();
		assert_eq!(without.notes[0].pitch, 72);
	}

	#[test]
	fn approach_and_anticipation_read_the_next_chord() {
		let p = pattern(
			Role::Bass,
			Harmony::Chord,
			&[
				("R", "x..............."),
				("app", "............x..."),
				("ant", "..............x="),
			],
		);
		let opts = RealizeOptions {
			chords: parse_chords("C G", None).unwrap(),
			..Default::default()
		};
		let r = realize(&p, &opts).unwrap();
		let pitches: Vec<u8> = r.notes.iter().map(|n| n.pitch).collect();
		// C2 (36), F#2 (42) below G2 (43), then G2 anticipated.
		assert_eq!(pitches, vec![36, 42, 43]);
	}

	#[test]
	fn chord_lane_voices_and_voice_leads() {
		let p = pattern(Role::Comping, Harmony::Chord, &[("chord", "x...")]);
		let opts = RealizeOptions {
			bars: Some(1),
			chords: parse_chords("C F G C", None).unwrap(),
			beats_per_chord: 1.0,
			..Default::default()
		};
		let r = realize(&p, &opts).unwrap();
		assert_eq!(r.notes.len(), 12);
		assert!(r.notes.iter().all(|n| (48..=76).contains(&n.pitch)));
	}

	#[test]
	fn fill_every_and_vary_swap_bars() {
		let main = pattern(Role::Drums, Harmony::None, &[("kick", "x...............")]);
		let mut variation = pattern(Role::Drums, Harmony::None, &[("snare", "x...............")]);
		variation.variant = Variant::Variation;
		let mut fill = pattern(Role::Drums, Harmony::None, &[("crash", "x...............")]);
		fill.variant = Variant::Fill;
		let opts = RealizeOptions {
			bars: Some(4),
			fill_every: Some(4),
			vary: true,
			fill: Some(&fill),
			variation: Some(&variation),
			..Default::default()
		};
		let r = realize(&main, &opts).unwrap();
		let pitches: Vec<u8> = r.notes.iter().map(|n| n.pitch).collect();
		assert_eq!(pitches, vec![36, 38, 36, 49]);
		assert!(r.warnings.is_empty());
		let r = realize(
			&main,
			&RealizeOptions {
				bars: Some(2),
				fill_every: Some(2),
				..Default::default()
			},
		)
		.unwrap();
		assert_eq!(r.warnings.len(), 1);
	}

	#[test]
	fn swing_and_groove_shift_offbeats() {
		let mut p = pattern(Role::Drums, Harmony::None, &[("closed_hat", "xxxx")]);
		p.feel = "swing:1".into();
		let r = realize(&p, &RealizeOptions::default()).unwrap();
		assert_eq!(r.notes[1].start_beats, 0.25 + 0.125);
		let groove = Groove {
			id: "g".into(),
			name: "g".into(),
			summary: "s".into(),
			steps_per_beat: 4,
			timing: vec![0.0, 0.2],
			velocity: vec![1.0, 0.5],
			genres: vec![],
			tags: vec![],
		};
		let r = realize(
			&p,
			&RealizeOptions {
				groove: Some(&groove),
				..Default::default()
			},
		)
		.unwrap();
		assert!((r.notes[1].start_beats - 0.3).abs() < 1e-9);
		assert_eq!(r.notes[1].velocity, 0.4);
	}

	#[test]
	fn absolute_patterns_transpose_to_the_key() {
		let mut p = pattern(Role::Lead, Harmony::Absolute, &[("A4", "x...")]);
		p.key = Some("A minor".into());
		let r = realize(
			&p,
			&RealizeOptions {
				key: key("C minor"),
				..Default::default()
			},
		)
		.unwrap();
		assert_eq!(r.notes[0].pitch, 72);
	}

	#[test]
	fn roman_numerals_need_a_key() {
		assert!(parse_chords("i VI", None).is_err());
		let chords = parse_chords("i VI III VII", key("A minor").as_ref()).unwrap();
		assert_eq!(chords[1].root.0, 5); // F
		assert!(!is_roman("Bb"));
		assert!(is_roman("bVII"));
	}
}
