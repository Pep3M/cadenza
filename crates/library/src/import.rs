//! Importing performed MIDI as a library pattern (`cadenza library import`, never at
//! runtime): notes are quantized to the grid, the timing deviations and velocities become a
//! groove, the role comes from the GM drum channel / program / register, and pitched notes
//! are written relative to the chords (or to the key's scale degrees) so the pattern fits
//! any key and progression. Every lane is checked against [`crate::realize`]: realizing the
//! imported pattern over the same chords gives back the quantized notes.

use cadenza_theory::{Chord, Scale, ScaleKind};

use crate::format::{
	Groove, Harmony, Item, MAX_SUMMARY_CHARS, OrderedMap, Pattern, Role, Variant, bar_beats,
};
use crate::lanes::{drum_name, step_char};
use crate::realize::{chord_tone, heptatonic, place};

/// One performed note (beats from the file start).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ImportNote {
	pub pitch: u8,
	pub start_beats: f64,
	pub length_beats: f64,
	pub velocity: f32,
}

#[derive(Debug, Clone)]
pub struct ImportOptions {
	/// Item slug of the pattern (the groove gets `<slug>_groove`).
	pub slug: String,
	pub name: String,
	/// Pack id the ids refer to.
	pub pack: String,
	/// Forced role (default: detected).
	pub role: Option<Role>,
	/// The notes came from the GM drum channel.
	pub drums: bool,
	/// GM program of the source channel.
	pub program: Option<u8>,
	/// Key of the material (default: inferred from the notes).
	pub key: Option<Scale>,
	/// Chords under the material (cycling every `beats_per_chord` from the first bar).
	pub chords: Vec<Chord>,
	pub beats_per_chord: f64,
	pub steps_per_beat: u32,
	/// Bars to import from the first bar holding a note (default: all, at most 16).
	pub bars: Option<u32>,
	/// (numerator, denominator).
	pub meter: (u32, u32),
	pub tempo: Option<f64>,
	pub genres: Vec<String>,
	/// Where the notes came from, for the summary ("groove.mid").
	pub source: String,
}

impl Default for ImportOptions {
	fn default() -> Self {
		Self {
			slug: "imported".into(),
			name: "Imported".into(),
			pack: "user".into(),
			role: None,
			drums: false,
			program: None,
			key: None,
			chords: Vec::new(),
			beats_per_chord: 4.0,
			steps_per_beat: 4,
			bars: None,
			meter: (4, 4),
			tempo: None,
			genres: Vec::new(),
			source: String::new(),
		}
	}
}

#[derive(Debug, Clone)]
pub struct Imported {
	pub pattern: Pattern,
	/// Timing/velocity template extracted from the performance (`None` when it was
	/// already quantized).
	pub groove: Option<Groove>,
	/// The key the pitched lanes were read in (given or inferred).
	pub key: Option<Scale>,
	pub warnings: Vec<String>,
}

/// GM program families -> role.
fn role_of_program(program: u8) -> Role {
	match program {
		0..=7 | 16..=31 | 104..=111 => Role::Comping,
		8..=15 => Role::Arp,
		32..=39 => Role::Bass,
		40..=55 | 88..=103 => Role::Pad,
		56..=87 => Role::Lead,
		_ => Role::Perc,
	}
}

fn detect_role(notes: &[ImportNote], opts: &ImportOptions) -> Role {
	if let Some(r) = opts.role {
		return r;
	}
	if opts.drums {
		let kit = notes
			.iter()
			.any(|n| matches!(n.pitch, 35..=40 | 42 | 44 | 46));
		return if kit { Role::Drums } else { Role::Perc };
	}
	if let Some(p) = opts.program {
		return role_of_program(p);
	}
	let mean = notes.iter().map(|n| n.pitch as f64).sum::<f64>() / notes.len().max(1) as f64;
	let chords = notes
		.iter()
		.filter(|a| {
			notes
				.iter()
				.filter(|b| (b.start_beats - a.start_beats).abs() < 0.05)
				.count() >= 3
		})
		.count();
	if mean < 50.0 {
		Role::Bass
	} else if chords * 2 >= notes.len() {
		let long = notes.iter().filter(|n| n.length_beats >= 2.0).count();
		if long * 2 >= notes.len() {
			Role::Pad
		} else {
			Role::Comping
		}
	} else if mean >= 64.0 {
		Role::Lead
	} else {
		Role::Arp
	}
}

/// The major or natural minor key whose profile best matches the notes' pitch classes
/// (weighted by duration).
pub fn infer_key(notes: &[ImportNote]) -> Option<Scale> {
	let mut hist = [0.0f64; 12];
	for n in notes {
		hist[n.pitch as usize % 12] += n.length_beats.max(0.1);
	}
	Scale::infer(&hist)
}

fn octave_suffix(octave: i32) -> String {
	match octave {
		0 => String::new(),
		o if o > 0 => format!("+{o}"),
		o => o.to_string(),
	}
}

/// The chord-tone lane realizing to `pitch` over `chord`, if any.
fn chord_lane(pitch: u8, chord: &Chord, key: Option<&Scale>, center: i32) -> Option<String> {
	// Triad tones in any octave first; an extension that falls back to the octave is not one.
	for tones in [[1u8, 3, 5].as_slice(), &[7, 9, 11, 13]] {
		for octave in [0, 1, -1, 2, -2] {
			for &tone in tones {
				let iv = chord_tone(chord, tone, key);
				if tone != 1 && iv % 12 == 0 {
					continue;
				}
				let root = place(chord.root.0, center + 12 * octave);
				if root + iv != pitch as i32 {
					continue;
				}
				let base = if tone == 1 {
					"R".to_owned()
				} else {
					tone.to_string()
				};
				return Some(format!("{base}{}", octave_suffix(octave)));
			}
		}
	}
	None
}

/// The scale-degree lane realizing to `pitch` in `key`.
fn degree_lane(pitch: u8, key: &Scale, center: i32) -> Option<String> {
	let ivs = heptatonic(key.kind);
	for accidental in [0i32, -1, 1] {
		for octave in [0, 1, -1, 2, -2] {
			let tonic = place(key.root.0, center + 12 * octave);
			for (d, iv) in ivs.iter().enumerate() {
				if tonic + *iv as i32 + accidental == pitch as i32 {
					let acc = match accidental {
						-1 => "b",
						1 => "#",
						_ => "",
					};
					return Some(format!("^{acc}{}{}", d + 1, octave_suffix(octave)));
				}
			}
		}
	}
	None
}

fn energy_of(notes_per_bar: f64) -> u8 {
	match notes_per_bar {
		n if n < 4.0 => 1,
		n if n < 8.0 => 2,
		n if n < 14.0 => 3,
		n if n < 24.0 => 4,
		_ => 5,
	}
}

/// Converts performed notes into a pattern (and its groove). Errors are actionable.
pub fn import_pattern(notes: &[ImportNote], opts: &ImportOptions) -> Result<Imported, String> {
	if notes.is_empty() {
		return Err("the source has no notes".into());
	}
	if !(1..=12).contains(&opts.steps_per_beat) {
		return Err("steps per beat must be 1..=12".into());
	}
	let mut warnings = Vec::new();
	let role = detect_role(notes, opts);
	let spb = opts.steps_per_beat as f64;
	let bar = bar_beats(opts.meter);
	let steps_per_bar = (bar * spb).round() as usize;
	if (bar * spb).fract() != 0.0 {
		return Err(format!(
			"a {}/{} bar is not a whole number of steps at {} steps per beat",
			opts.meter.0, opts.meter.1, opts.steps_per_beat
		));
	}
	let first = notes
		.iter()
		.map(|n| n.start_beats)
		.fold(f64::INFINITY, f64::min);
	let origin = (first / bar).floor().max(0.0) * bar;
	let last_end = notes
		.iter()
		.map(|n| n.start_beats + n.length_beats.min(bar))
		.fold(0.0f64, f64::max);
	let found_bars = (((last_end - origin) / bar) - 1e-6).ceil().max(1.0) as u32;
	let bars = opts.bars.unwrap_or(found_bars.min(16)).clamp(1, 16);
	if opts.bars.is_none() && found_bars > 16 {
		warnings.push(format!(
			"the source has {found_bars} bars; imported the first 16 (pass --bars to choose)"
		));
	}
	let total = steps_per_bar * bars as usize;
	let span = bars as f64 * bar;

	let pitched = !role.is_percussive();
	let key = if pitched {
		opts.key.or_else(|| {
			let k = infer_key(notes);
			if let Some(k) = &k {
				warnings.push(format!(
					"key inferred from the notes: {} {} (pass --key to override)",
					k.root.name(false),
					if k.kind == ScaleKind::Major {
						"major"
					} else {
						"minor"
					}
				));
			}
			k
		})
	} else {
		None
	};
	let center = role.center();
	let bpc = if opts.beats_per_chord > 0.0 {
		opts.beats_per_chord
	} else {
		4.0
	};

	let chord_at =
		|beat: f64| &opts.chords[((beat + 1e-6) / bpc).floor() as usize % opts.chords.len()];
	let grid_beat = |n: &ImportNote| ((n.start_beats - origin) * spb).round() / spb;
	// Chord-relative when the chords explain most notes, else scale degrees of the key.
	let use_chords = pitched && !opts.chords.is_empty() && {
		let hits = notes
			.iter()
			.filter(|n| chord_lane(n.pitch, chord_at(grid_beat(n)), key.as_ref(), center).is_some())
			.count();
		if hits * 2 < notes.len() {
			warnings.push(format!(
				"the chords explain only {hits} of {} notes: wrote scale degrees instead",
				notes.len()
			));
		}
		hits * 2 >= notes.len()
	};
	// Lane name -> step chars; per-step timing deviations and velocity ratios.
	let mut lanes: Vec<(String, Vec<char>)> = Vec::new();
	let mut deviation = vec![(0.0f64, 0u32); steps_per_bar];
	let mut vel_ratio = vec![(0.0f64, 0u32); steps_per_bar];
	let (mut merged, mut dropped) = (0usize, 0usize);
	let mut sorted: Vec<&ImportNote> = notes.iter().collect();
	sorted.sort_by(|a, b| a.start_beats.total_cmp(&b.start_beats));
	for n in sorted {
		let rel = n.start_beats - origin;
		let exact = rel * spb;
		let idx = exact.round();
		if idx < 0.0 || idx as usize >= total || rel >= span {
			dropped += 1;
			continue;
		}
		let idx = idx as usize;
		let lane = if !pitched {
			drum_name(n.pitch)
				.map(str::to_owned)
				.unwrap_or_else(|| n.pitch.to_string())
		} else {
			use_chords
				.then(|| chord_lane(n.pitch, chord_at(idx as f64 / spb), key.as_ref(), center))
				.flatten()
				.or_else(|| key.as_ref().and_then(|k| degree_lane(n.pitch, k, center)))
				.unwrap_or_else(|| cadenza_theory::format_pitch(n.pitch, false))
		};
		let pos = match lanes.iter().position(|(l, _)| *l == lane) {
			Some(p) => p,
			None => {
				lanes.push((lane, vec!['.'; total]));
				lanes.len() - 1
			}
		};
		let steps = &mut lanes[pos].1;
		let c = step_char(n.velocity);
		if matches!(steps[idx], 'X' | 'x' | 'o') {
			merged += 1;
			if c == 'X' || (c == 'x' && steps[idx] == 'o') {
				steps[idx] = c;
			}
		} else {
			steps[idx] = c;
		}
		if pitched {
			let len = ((n.length_beats * spb).round() as usize).max(1);
			for s in steps.iter_mut().take((idx + len).min(total)).skip(idx + 1) {
				if *s != '.' {
					break;
				}
				*s = '=';
			}
		}
		let in_bar = idx % steps_per_bar;
		let d = &mut deviation[in_bar];
		d.0 += (exact - idx as f64).clamp(-0.5, 0.5);
		d.1 += 1;
		let grid = match c {
			'X' => crate::lanes::ACCENT,
			'x' => crate::lanes::NORMAL,
			_ => crate::lanes::GHOST,
		};
		let v = &mut vel_ratio[in_bar];
		v.0 += (n.velocity / grid) as f64;
		v.1 += 1;
	}
	if dropped > 0 {
		warnings.push(format!(
			"{dropped} notes past the imported bars were left out"
		));
	}
	if merged > 0 {
		warnings.push(format!(
			"{merged} notes landed on an occupied step and were merged (try a finer grid)"
		));
	}
	if lanes.is_empty() {
		return Err("no notes fall inside the imported bars".into());
	}
	let harmony = if !pitched {
		Harmony::None
	} else if use_chords {
		Harmony::Chord
	} else if key.is_some() {
		Harmony::Scale
	} else {
		Harmony::Absolute
	};

	// Readable lanes: one group per beat.
	let group = opts.steps_per_beat as usize;
	let format_lane = |steps: &[char], spaced: bool| -> String {
		if !spaced {
			return steps.iter().collect();
		}
		steps
			.chunks(group)
			.map(|c| c.iter().collect::<String>())
			.collect::<Vec<_>>()
			.join(" ")
	};
	let mut summary = format!(
		"Imported {} {} ({} bar{}{}).",
		if opts.source.is_empty() {
			"MIDI"
		} else {
			&opts.source
		},
		role,
		bars,
		if bars == 1 { "" } else { "s" },
		key.as_ref()
			.map(|k| format!(", {}", k.root.name(false)))
			.unwrap_or_default()
	);
	if summary.chars().count() > MAX_SUMMARY_CHARS {
		summary = summary
			.chars()
			.take(MAX_SUMMARY_CHARS - 1)
			.collect::<String>()
			+ ".";
	}
	let groove_id = format!("{}_groove", opts.slug);
	let has_groove = deviation
		.iter()
		.any(|(sum, n)| *n > 0 && (sum / *n as f64).abs() >= 0.02)
		|| vel_ratio
			.iter()
			.any(|(sum, n)| *n > 0 && ((sum / *n as f64) - 1.0).abs() >= 0.05);
	let groove = has_groove.then(|| Groove {
		id: groove_id.clone(),
		name: format!("{} groove", opts.name).chars().take(60).collect(),
		summary: format!("Timing and velocity of {}", opts.name)
			.chars()
			.take(MAX_SUMMARY_CHARS)
			.collect(),
		steps_per_beat: opts.steps_per_beat,
		timing: deviation
			.iter()
			.map(|(s, n)| {
				if *n == 0 {
					0.0
				} else {
					((s / *n as f64) * 100.0).round() / 100.0
				}
			})
			.collect(),
		velocity: vel_ratio
			.iter()
			.map(|(s, n)| {
				if *n == 0 {
					1.0
				} else {
					((s / *n as f64).clamp(0.0, 2.0) * 100.0).round() / 100.0
				}
			})
			.collect(),
		genres: opts.genres.clone(),
		tags: vec!["imported".into()],
	});
	let notes_per_bar = (notes.len() - dropped) as f64 / bars as f64;
	let mut pattern = Pattern {
		id: opts.slug.clone(),
		name: opts.name.chars().take(60).collect(),
		summary,
		role,
		genres: opts.genres.clone(),
		sections: vec!["any".into()],
		energy: energy_of(notes_per_bar),
		feel: "straight".into(),
		meter: format!("{}/{}", opts.meter.0, opts.meter.1),
		bars,
		tempo: opts
			.tempo
			.map(|t| [(t - 8.0).clamp(40.0, 240.0), (t + 8.0).clamp(40.0, 240.0)]),
		family: None,
		variant: Variant::Main,
		harmony,
		key: None,
		voicing: None,
		octave: None,
		steps_per_beat: opts.steps_per_beat,
		lanes: OrderedMap(
			lanes
				.iter()
				.map(|(l, s)| (l.clone(), format_lane(s, true)))
				.collect(),
		),
		pairs_with: Vec::new(),
		groove: groove
			.as_ref()
			.map(|_| format!("{}/{groove_id}", opts.pack)),
		tags: vec!["imported".into()],
	};
	let budget = crate::format::ItemKind::Pattern.max_detail_chars();
	let size = |p: &Pattern| {
		serde_json::to_string(&Item::Pattern(p.clone()))
			.map(|s| s.len() + opts.pack.len() + 1)
			.unwrap_or(usize::MAX)
	};
	if size(&pattern) > budget {
		pattern.lanes = OrderedMap(
			lanes
				.iter()
				.map(|(l, s)| (l.clone(), format_lane(s, false)))
				.collect(),
		);
	}
	if size(&pattern) > budget {
		return Err(format!(
			"the pattern would be {} chars ({} lanes x {total} steps), over the {budget}-char \
			 budget: import fewer bars (--bars), a coarser grid (--steps-per-beat) or one part",
			size(&pattern),
			lanes.len()
		));
	}
	Ok(Imported {
		pattern,
		groove,
		key,
		warnings,
	})
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::realize::{RealizeOptions, parse_chords, realize};

	fn note(pitch: u8, start: f64, length: f64, velocity: f32) -> ImportNote {
		ImportNote {
			pitch,
			start_beats: start,
			length_beats: length,
			velocity,
		}
	}

	#[test]
	fn drums_are_quantized_and_keep_their_feel_as_a_groove() {
		// A dembow played slightly late on the snares.
		let mut notes = Vec::new();
		for beat in 0..4 {
			notes.push(note(36, beat as f64, 0.1, 0.8));
		}
		for (start, late) in [(0.75, 0.02), (1.5, 0.03), (2.75, 0.02), (3.5, 0.03)] {
			notes.push(note(38, start + late, 0.1, 1.0));
		}
		let imported = import_pattern(
			&notes,
			&ImportOptions {
				drums: true,
				..Default::default()
			},
		)
		.unwrap();
		let p = &imported.pattern;
		assert_eq!(p.role, Role::Drums);
		assert_eq!(p.lanes.get("snare").unwrap(), "...X ..X. ...X ..X.");
		assert_eq!(p.lanes.get("kick").unwrap(), "x... x... x... x...");
		let groove = imported.groove.unwrap();
		assert_eq!(groove.timing.len(), 16);
		assert!(groove.timing[3] > 0.05);
		assert_eq!(p.groove.as_deref(), Some("user/imported_groove"));
	}

	#[test]
	fn bass_over_chords_becomes_chord_relative_and_realizes_back() {
		let chords = parse_chords("Am F", None).unwrap();
		// A2 root tresillo, E3 fifth; then F2 roots.
		let notes = vec![
			note(45, 0.0, 1.5, 0.8),
			note(45, 1.5, 1.5, 0.8),
			note(52, 3.0, 0.5, 0.8),
			note(41, 4.0, 1.5, 0.8),
			note(41, 5.5, 1.5, 0.8),
			note(48, 7.0, 0.5, 0.8),
		];
		let imported = import_pattern(
			&notes,
			&ImportOptions {
				program: Some(33),
				chords: chords.clone(),
				key: Some(Scale::parse("A minor").unwrap()),
				..Default::default()
			},
		)
		.unwrap();
		let p = &imported.pattern;
		assert_eq!(p.role, Role::Bass);
		assert_eq!(p.harmony, Harmony::Chord);
		assert_eq!(p.bars, 2);
		assert!(imported.groove.is_none());
		let back = realize(
			p,
			&RealizeOptions {
				chords,
				key: imported.key,
				..Default::default()
			},
		)
		.unwrap();
		let got: Vec<(u8, f64, f64)> = back
			.notes
			.iter()
			.map(|n| (n.pitch, n.start_beats, n.length_beats))
			.collect();
		let want: Vec<(u8, f64, f64)> = notes
			.iter()
			.map(|n| (n.pitch, n.start_beats, n.length_beats))
			.collect();
		assert_eq!(got, want);
	}

	#[test]
	fn melodies_become_scale_degrees_in_the_inferred_key() {
		// C major scale run.
		let notes: Vec<ImportNote> = [72u8, 74, 76, 77, 79, 81, 83, 84]
			.iter()
			.enumerate()
			.map(|(i, &p)| note(p, i as f64 * 0.5, 0.5, 0.8))
			.collect();
		let imported = import_pattern(&notes, &ImportOptions::default()).unwrap();
		let p = &imported.pattern;
		assert_eq!(p.role, Role::Lead);
		assert_eq!(p.harmony, Harmony::Scale);
		let key = imported.key.unwrap();
		assert_eq!(key.root.0, 0);
		assert!(p.lanes.get("^1").is_some() && p.lanes.get("^1+1").is_some());
		let back = realize(
			p,
			&RealizeOptions {
				key: Some(key),
				..Default::default()
			},
		)
		.unwrap();
		let pitches: Vec<u8> = back.notes.iter().map(|n| n.pitch).collect();
		assert_eq!(pitches, vec![72, 74, 76, 77, 79, 81, 83, 84]);
	}

	#[test]
	fn infers_minor_keys() {
		let notes: Vec<ImportNote> = [57u8, 60, 64, 57, 62, 65, 64, 59, 57]
			.iter()
			.enumerate()
			.map(|(i, &p)| note(p, i as f64, 1.0, 0.8))
			.collect();
		let key = infer_key(&notes).unwrap();
		assert_eq!((key.root.0, key.kind), (9, ScaleKind::NaturalMinor));
	}
}
