//! Table-driven chord-symbol parser tests.

use cadenza_theory::Chord;

fn pcs(symbol: &str) -> Vec<u8> {
	let chord = Chord::parse(symbol).unwrap_or_else(|e| panic!("failed to parse {symbol}: {e}"));
	let mut v: Vec<u8> = chord.pitch_classes().into_iter().map(|p| p.0).collect();
	v.sort_unstable();
	v
}

#[test]
fn chord_symbols_produce_expected_pitch_classes() {
	let cases: &[(&str, &[u8])] = &[
		("C", &[0, 4, 7]),
		("Cm", &[0, 3, 7]),
		("Cmin", &[0, 3, 7]),
		("C-", &[0, 3, 7]),
		("Cdim", &[0, 3, 6]),
		("C°", &[0, 3, 6]),
		("Caug", &[0, 4, 8]),
		("C+", &[0, 4, 8]),
		("Csus2", &[0, 2, 7]),
		("Csus4", &[0, 5, 7]),
		("C5", &[0, 7]),
		("C6", &[0, 4, 7, 9]),
		("Cm6", &[0, 3, 7, 9]),
		("C6/9", &[0, 2, 4, 7, 9]),
		("C69", &[0, 2, 4, 7, 9]),
		("C7", &[0, 4, 7, 10]),
		("Cmaj7", &[0, 4, 7, 11]),
		("CM7", &[0, 4, 7, 11]),
		("CΔ7", &[0, 4, 7, 11]),
		("Cm7", &[0, 3, 7, 10]),
		("Cmin7", &[0, 3, 7, 10]),
		("C-7", &[0, 3, 7, 10]),
		("Cm(maj7)", &[0, 3, 7, 11]),
		("Cdim7", &[0, 3, 6, 9]),
		("C°7", &[0, 3, 6, 9]),
		("Cm7b5", &[0, 3, 6, 10]),
		("Cø7", &[0, 3, 6, 10]),
		("Cø", &[0, 3, 6, 10]),
		("C7sus4", &[0, 5, 7, 10]),
		("Caug7", &[0, 4, 8, 10]),
		("C+7", &[0, 4, 8, 10]),
		("C9", &[0, 2, 4, 7, 10]),
		("Cmaj9", &[0, 2, 4, 7, 11]),
		("Cm9", &[0, 2, 3, 7, 10]),
		("C11", &[0, 2, 4, 5, 7, 10]),
		("Cmaj11", &[0, 2, 4, 5, 7, 11]),
		("Cm11", &[0, 2, 3, 5, 7, 10]),
		("C13", &[0, 2, 4, 5, 7, 9, 10]),
		("Cmaj13", &[0, 2, 4, 5, 7, 9, 11]),
		("Cm13", &[0, 2, 3, 5, 7, 9, 10]),
		("Cadd9", &[0, 2, 4, 7]),
		("Cadd11", &[0, 4, 5, 7]),
		("Cadd2", &[0, 2, 4, 7]),
		("Cadd6", &[0, 4, 7, 9]),
		("Cadd13", &[0, 4, 7, 9]),
		("Cno3", &[0, 7]),
		("C7no5", &[0, 4, 10]),
		("C7b5", &[0, 4, 6, 10]),
		("C7#5", &[0, 4, 8, 10]),
		("C7b9", &[0, 1, 4, 7, 10]),
		("C7#9", &[0, 3, 4, 7, 10]),
		("C7#11", &[0, 4, 6, 7, 10]),
		("C7b13", &[0, 4, 7, 8, 10]),
		("C9#11", &[0, 2, 4, 6, 7, 10]),
		("Cmaj7#11", &[0, 4, 6, 7, 11]),
		("C(no5)", &[0, 4]),
		("C7♭9", &[0, 1, 4, 7, 10]),
		("Cmin6", &[0, 3, 7, 9]),
		("D7sus4", &[0, 2, 7, 9]),
		("Bb13", &[0, 2, 3, 5, 7, 8, 10]),
		("C#m7b5", &[1, 4, 7, 11]),
		("F#dim7", &[0, 3, 6, 9]),
		("Bbmaj7", &[2, 5, 9, 10]),
		("Dm9", &[0, 2, 4, 5, 9]),
		("G13", &[0, 2, 4, 5, 7, 9, 11]),
		("Esus4", &[4, 9, 11]),
	];

	for (symbol, expected) in cases {
		assert_eq!(pcs(symbol), *expected, "symbol: {symbol}");
	}
}

#[test]
fn slash_bass_is_parsed_separately_from_quality() {
	let c_over_e = Chord::parse("C/E").unwrap();
	assert_eq!(
		c_over_e
			.pitch_classes()
			.iter()
			.map(|p| p.0)
			.collect::<Vec<_>>(),
		vec![0, 4, 7]
	);
	assert_eq!(c_over_e.bass.unwrap().0, 4);

	let am7_over_g = Chord::parse("Am7/G").unwrap();
	let mut pcs: Vec<u8> = am7_over_g.pitch_classes().iter().map(|p| p.0).collect();
	pcs.sort_unstable();
	assert_eq!(pcs, vec![0, 4, 7, 9]);
	assert_eq!(am7_over_g.bass.unwrap().0, 7);
}

#[test]
fn invalid_chord_symbol_names_the_bad_token() {
	let err = Chord::parse("Cxyz").unwrap_err();
	let message = err.to_string();
	assert!(message.contains("xyz"), "message was: {message}");
}

#[test]
fn empty_chord_symbol_is_rejected() {
	assert!(Chord::parse("").is_err());
	assert!(Chord::parse("   ").is_err());
}
