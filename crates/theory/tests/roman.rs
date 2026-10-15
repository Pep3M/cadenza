//! Roman-numeral analysis tests, in major and minor keys.

use cadenza_theory::scale::Scale;
use cadenza_theory::{parse_roman, parse_roman_progression};

fn pcs_of(chord: &cadenza_theory::Chord) -> Vec<u8> {
	let mut v: Vec<u8> = chord.pitch_classes().into_iter().map(|p| p.0).collect();
	v.sort_unstable();
	v
}

fn c_major() -> Scale {
	Scale::parse("C major").unwrap()
}

fn a_minor() -> Scale {
	Scale::parse("A minor").unwrap()
}

#[test]
fn major_key_diatonic_triads() {
	let key = c_major();
	let cases: &[(&str, &[u8])] = &[
		("I", &[0, 4, 7]),
		("ii", &[2, 5, 9]),
		("iii", &[4, 7, 11]),
		("IV", &[0, 5, 9]),
		("V", &[2, 7, 11]),
		("vi", &[0, 4, 9]),
		("vii°", &[2, 5, 11]),
	];
	for (roman, expected) in cases {
		let chord = parse_roman(roman, &key).unwrap();
		assert_eq!(pcs_of(&chord), *expected, "roman: {roman}");
	}
}

#[test]
fn major_key_seventh_chords() {
	let key = c_major();
	let ii7 = parse_roman("ii7", &key).unwrap();
	assert_eq!(pcs_of(&ii7), vec![0, 2, 5, 9]);

	let v7 = parse_roman("V7", &key).unwrap();
	assert_eq!(pcs_of(&v7), vec![2, 5, 7, 11]);

	let i_maj7 = parse_roman("Imaj7", &key).unwrap();
	assert_eq!(pcs_of(&i_maj7), vec![0, 4, 7, 11]);
}

#[test]
fn minor_key_default_vs_raised_leading_tone() {
	let key = a_minor();

	// Lowercase "v" stays diatonic to natural minor (minor triad on E).
	let v_lower = parse_roman("v", &key).unwrap();
	assert_eq!(pcs_of(&v_lower), vec![4, 7, 11]);

	// Uppercase "V"/"V7" raises the leading tone (major triad / dominant7
	// on E), matching harmonic-minor dominant function.
	let v_upper = parse_roman("V", &key).unwrap();
	assert_eq!(pcs_of(&v_upper), vec![4, 8, 11]);

	let v7_upper = parse_roman("V7", &key).unwrap();
	assert_eq!(pcs_of(&v7_upper), vec![2, 4, 8, 11]);
}

#[test]
fn secondary_dominants() {
	let key = c_major();

	// V7/V resolves to G, so it is a D7 (raised F#/leading tone of G).
	let v7_of_v = parse_roman("V7/V", &key).unwrap();
	assert_eq!(pcs_of(&v7_of_v), vec![0, 2, 6, 9]);

	let v_of_v = parse_roman("V/V", &key).unwrap();
	assert_eq!(pcs_of(&v_of_v), vec![2, 6, 9]);
}

#[test]
fn progression_vi_ii_v_i_in_c_major() {
	let key = c_major();
	let chords = parse_roman_progression("vi-ii-V-I", &key).unwrap();
	let pcs: Vec<Vec<u8>> = chords.iter().map(pcs_of).collect();
	assert_eq!(
		pcs,
		vec![vec![0, 4, 9], vec![2, 5, 9], vec![2, 7, 11], vec![0, 4, 7]]
	);
}

#[test]
fn progression_accepts_mixed_separators() {
	let key = c_major();
	let a = parse_roman_progression("I-V-vi-IV", &key).unwrap();
	let b = parse_roman_progression("I V vi IV", &key).unwrap();
	let c = parse_roman_progression("I | V | vi | IV", &key).unwrap();
	let d = parse_roman_progression("I, V, vi, IV", &key).unwrap();
	assert_eq!(a, b);
	assert_eq!(b, c);
	assert_eq!(c, d);
	assert_eq!(a.len(), 4);
}

#[test]
fn progression_ii7_v7_imaj7() {
	let key = c_major();
	let chords = parse_roman_progression("ii7 V7 Imaj7", &key).unwrap();
	let pcs: Vec<Vec<u8>> = chords.iter().map(pcs_of).collect();
	assert_eq!(
		pcs,
		vec![vec![0, 2, 5, 9], vec![2, 5, 7, 11], vec![0, 4, 7, 11]]
	);
}

#[test]
fn flat_and_sharp_accidentals_on_degrees() {
	let key = c_major();
	let b_flat_seven = parse_roman("bVII", &key).unwrap();
	// bVII in C major: root Bb (borrowed major triad).
	assert_eq!(pcs_of(&b_flat_seven), vec![2, 5, 10]);

	let sharp_iv = parse_roman("#iv", &key).unwrap();
	// #iv: root F# minor triad (default lowercase = minor).
	assert_eq!(pcs_of(&sharp_iv), vec![1, 6, 9]);
}

#[test]
fn lowercase_diminished_diatonic_degree_overrides_case_default() {
	let c_major = c_major();
	let a_minor = a_minor();

	// vii in C major is diatonically diminished (B D F), not the
	// case-based minor triad.
	let vii_bare = parse_roman("vii", &c_major).unwrap();
	assert_eq!(pcs_of(&vii_bare), vec![2, 5, 11]);
	let vii_explicit_dim = parse_roman("vii°", &c_major).unwrap();
	assert_eq!(pcs_of(&vii_bare), pcs_of(&vii_explicit_dim));

	// A bare "7" on that degree is half-diminished (m7b5), not minor7.
	let vii7 = parse_roman("vii7", &c_major).unwrap();
	assert_eq!(pcs_of(&vii7), vec![2, 5, 9, 11]);

	// ii in natural A minor is diatonically diminished (B D F) as well.
	let ii_bare = parse_roman("ii", &a_minor).unwrap();
	assert_eq!(pcs_of(&ii_bare), vec![2, 5, 11]);
	let ii7 = parse_roman("ii7", &a_minor).unwrap();
	assert_eq!(pcs_of(&ii7), vec![2, 5, 9, 11]);

	// Degrees that are NOT diatonically diminished keep the case-based
	// default: lowercase "iii" in A minor is diatonically major (C E G),
	// but the case-based convention (minor) still applies since it is not
	// diminished.
	let iii_in_a_minor = parse_roman("iii", &a_minor).unwrap();
	assert_eq!(pcs_of(&iii_in_a_minor), vec![0, 3, 7]);
}

#[test]
fn empty_progression_is_rejected() {
	let key = c_major();
	assert!(parse_roman_progression("", &key).is_err());
	assert!(parse_roman_progression("   ", &key).is_err());
}
