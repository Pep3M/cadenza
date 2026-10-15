//! Tests over the embedded `core` pack and the pack loader.

use std::borrow::Cow;

use cadenza_theory::Scale;

use crate::*;

fn core() -> Library {
	let mut lib = Library::core();
	lib.check_references();
	lib
}

/// macOS and Windows file systems ignore case: two files differing only in case would
/// collide in a checkout there (ids may differ in case, file names may not).
#[test]
fn core_pack_file_names_differ_beyond_case() {
	let mut seen = std::collections::BTreeMap::new();
	for (path, _) in crate::library::embedded::CORE_FILES {
		if let Some(other) = seen.insert(path.to_lowercase(), *path) {
			panic!("'{path}' and '{other}' differ only in case");
		}
	}
}

#[test]
fn core_pack_loads_without_issues() {
	let lib = core();
	let report = &lib.reports()[0];
	assert!(report.loaded);
	assert!(report.issues.is_empty(), "{:#?}", report.issues);
	let count = |kind| lib.items().filter(|i| i.item.kind() == kind).count();
	assert!(
		count(ItemKind::Pattern) >= 40,
		"{} patterns",
		count(ItemKind::Pattern)
	);
	assert!(count(ItemKind::Style) >= 6);
	assert!(count(ItemKind::Form) >= 6);
	assert!(count(ItemKind::Progression) >= 10);
	assert!(count(ItemKind::Groove) >= 3);
	let manifest = &lib.pack(CORE_PACK).unwrap().manifest;
	assert!(manifest.redistributable);
}

#[test]
fn every_core_pattern_realizes_in_a_minor_and_c_major() {
	let lib = core();
	for (key, chords) in [("A minor", "Am F C G"), ("C major", "C G Am F")] {
		let scale = Scale::parse(key).unwrap();
		let chords = parse_chords(chords, None).unwrap();
		for it in lib.items() {
			let Some(p) = it.item.as_pattern() else {
				continue;
			};
			let opts = RealizeOptions {
				bars: Some(4),
				key: Some(scale),
				chords: chords.clone(),
				fill_every: Some(4),
				vary: true,
				fill: lib.family_member(p, Variant::Fill),
				variation: lib.family_member(p, Variant::Variation),
				..Default::default()
			};
			let r = realize(p, &opts).unwrap_or_else(|e| panic!("{}: {e}", p.id));
			assert!(!r.notes.is_empty(), "{} wrote no notes", p.id);
			assert_eq!(r.length_beats, 4.0 * p.bar_beats());
			for n in &r.notes {
				assert!(
					n.start_beats >= 0.0 && n.start_beats < r.length_beats,
					"{}",
					p.id
				);
				assert!(n.length_beats > 0.0);
				if !p.role.is_percussive() {
					assert!(
						(21..=96).contains(&n.pitch),
						"{} pitch {} out of its register",
						p.id,
						n.pitch
					);
				}
			}
		}
	}
}

#[test]
fn budgets_hold_for_every_core_item() {
	for it in core().items() {
		let size = serde_json::to_string(&it.item).unwrap().len();
		assert!(
			size <= it.item.kind().max_detail_chars(),
			"{} is {size} chars",
			it.id
		);
		assert!(it.item.common().summary.chars().count() <= MAX_SUMMARY_CHARS);
	}
}

fn starts(lib: &Library, id: &str, pitch: u8) -> Vec<f64> {
	let p = lib.pattern(id).unwrap();
	realize(p, &RealizeOptions::default())
		.unwrap()
		.notes
		.iter()
		.filter(|n| n.pitch == pitch)
		.map(|n| n.start_beats)
		.collect()
}

#[test]
fn latin_idioms_are_where_they_belong() {
	let lib = core();
	// Dembow snare: the a of 1, the & of 2, the a of 3, the & of 4.
	assert_eq!(
		starts(&lib, "core/dembow_basic", 38),
		vec![0.75, 1.5, 2.75, 3.5]
	);
	// Son clave 3-2: 1, the & of 2, 4 | 2, 3.
	assert_eq!(
		starts(&lib, "core/clave_son_32", 75),
		vec![0.0, 1.5, 3.0, 5.0, 6.0]
	);
	assert_eq!(
		starts(&lib, "core/clave_son_23", 75),
		vec![1.0, 2.0, 4.0, 5.5, 7.0]
	);
	// Rumba clave: third note of the three side on the a of 2.
	assert_eq!(
		starts(&lib, "core/clave_rumba_32", 75),
		vec![0.0, 1.75, 3.0, 5.0, 6.0]
	);
	// Tumbao conga: open tones on 4 and the & of 4.
	assert_eq!(starts(&lib, "core/perc_conga_tumbao", 63), vec![3.0, 3.5]);
	// Martillo: open tone on the low bongo on 4.
	assert_eq!(starts(&lib, "core/perc_bongo_martillo", 61), vec![3.0]);
}

#[test]
fn tumbao_bass_anticipates_the_next_chord_over_the_bar_line() {
	let lib = core();
	let p = lib.pattern("core/bass_tumbao").unwrap();
	let r = realize(
		p,
		&RealizeOptions {
			bars: Some(2),
			chords: parse_chords("C G", None).unwrap(),
			..Default::default()
		},
	)
	.unwrap();
	let notes: Vec<(u8, f64, f64)> = r
		.notes
		.iter()
		.map(|n| (n.pitch, n.start_beats, n.length_beats))
		.collect();
	// Bar 1: G2 (fifth of C) on the & of 2, G anticipated on 4 and held to the & of 2
	// of bar 2; then D (fifth of G) and C anticipated again.
	assert_eq!(
		notes,
		vec![
			(43, 1.5, 1.5),
			(43, 3.0, 2.5),
			(50, 5.5, 1.5),
			(36, 7.0, 1.0)
		]
	);
}

#[test]
fn search_ranks_styles_and_filters() {
	let lib = core();
	let first = |q: Query| lib.search(&q).first().map(|i| i.id.clone());
	assert_eq!(
		first(Query {
			text: "latin pop".into(),
			kind: Some(ItemKind::Style),
			..Default::default()
		}),
		Some("core/latin_pop".into())
	);
	assert_eq!(
		first(Query {
			text: "reggaetón".into(),
			kind: Some(ItemKind::Style),
			..Default::default()
		}),
		Some("core/latin_pop".into())
	);
	let bass = lib.search(&Query {
		genre: Some("reggaeton".into()),
		role: Some(Role::Bass),
		..Default::default()
	});
	assert!(bass.iter().any(|i| i.id == "core/bass_tresillo_808"));
	assert!(
		bass.iter()
			.all(|i| i.item.as_pattern().unwrap().role == Role::Bass)
	);
	let chorus = lib.search(&Query {
		text: "dembow".into(),
		section: Some("Chorus 2".into()),
		energy: Some(4),
		..Default::default()
	});
	assert_eq!(chorus[0].id, "core/dembow_variation");
	assert!(
		lib.search(&Query {
			text: "zzzz".into(),
			..Default::default()
		})
		.is_empty()
	);
	let c = card(lib.item("dembow_basic").unwrap());
	assert_eq!(c.facts, "drums · 1 bar · energy 3 · 88-102 bpm");
}

#[test]
fn family_members_resolve() {
	let lib = core();
	let p = lib.pattern("core/dembow_basic").unwrap();
	assert_eq!(
		lib.family_member(p, Variant::Fill).map(|p| p.id.as_str()),
		Some("core/dembow_fill")
	);
	assert_eq!(
		lib.family_member(p, Variant::Variation)
			.map(|p| p.id.as_str()),
		Some("core/dembow_variation")
	);
}

fn files(list: &[(&str, &str)]) -> Vec<(String, Cow<'static, str>)> {
	list.iter()
		.map(|(p, t)| (p.to_string(), Cow::Owned(t.to_string())))
		.collect()
}

const MANIFEST: &str = r#"{"format_version": 1, "id": "mine", "name": "Mine", "version": "1",
	"license": "CC-BY-4.0", "attribution": "Me", "redistributable": false}"#;

#[test]
fn invalid_items_are_reported_and_skipped() {
	let (pack, report) = Pack::from_files(
		PackOrigin::Embedded,
		files(&[
			("pack.json", MANIFEST),
			(
				"patterns/drums/good.json",
				r#"{"id": "good", "kind": "pattern", "name": "Good", "summary": "ok", "role": "drums",
				"energy": 3, "bars": 1, "lanes": {"kick": "x... x... x... x..."}}"#,
			),
			(
				"patterns/drums/bad_lane.json",
				r#"{"id": "bad_lane", "kind": "pattern", "name": "Bad", "summary": "bad", "role": "drums",
				"energy": 3, "bars": 1, "lanes": {"bongo": "x...", "kick": "x.?."}}"#,
			),
			(
				"patterns/drums/bad_length.json",
				r#"{"id": "bad_length", "kind": "pattern", "name": "Bad", "summary": "bad", "role": "drums",
				"energy": 3, "bars": 1, "lanes": {"kick": "x.. x.. x.."}}"#,
			),
			("styles/broken.json", "{ not json"),
			(
				"forms/typo.json",
				r#"{"id": "typo", "kind": "form", "name": "T", "summary": "t", "sectoins": []}"#,
			),
		]),
	);
	let pack = pack.unwrap();
	assert_eq!(pack.items.len(), 1);
	assert_eq!(pack.items[0].id, "mine/good");
	let text = format!("{:?}", report.issues);
	assert!(text.contains("lane 'bongo'"), "{text}");
	assert!(text.contains("unrecognized step '?'"), "{text}");
	assert!(text.contains("must divide"), "{text}");
	assert!(text.contains("styles/broken.json"), "{text}");
	assert!(text.contains("sectoins"), "{text}");
}

#[test]
fn manifests_need_license_fields() {
	let (pack, report) = Pack::from_files(
		PackOrigin::Embedded,
		files(&[(
			"pack.json",
			r#"{"format_version": 1, "id": "x", "name": "X", "version": "1", "license": "CC-BY-4.0",
			"redistributable": true}"#,
		)]),
	);
	assert!(pack.is_none());
	assert!(report.issues[0].message.contains("attribution"));
	let (pack, report) = Pack::from_files(PackOrigin::Embedded, files(&[]));
	assert!(pack.is_none());
	assert!(report.issues[0].message.contains("missing"));
}

#[test]
fn user_packs_load_from_folders_and_bad_references_are_reported() {
	let dir = tempfile::tempdir().unwrap();
	let pack = dir.path().join("mine");
	std::fs::create_dir_all(pack.join("patterns/bass")).unwrap();
	std::fs::write(pack.join("pack.json"), MANIFEST).unwrap();
	std::fs::write(
		pack.join("patterns/bass/b.json"),
		r#"{"id": "b", "kind": "pattern", "name": "B", "summary": "b", "role": "bass", "energy": 2,
		"bars": 1, "harmony": "chord", "lanes": {"R": "x..."}, "pairs_with": ["core/nope"]}"#,
	)
	.unwrap();
	// A second pack claiming the reserved id is skipped.
	let fake = dir.path().join("fake");
	std::fs::create_dir_all(&fake).unwrap();
	std::fs::write(
		fake.join("pack.json"),
		MANIFEST.replace("\"mine\"", "\"core\""),
	)
	.unwrap();
	let lib = Library::load(Some(dir.path()));
	assert!(lib.item("mine/b").is_some());
	let mine = lib
		.reports()
		.iter()
		.find(|r| r.pack.as_deref() == Some("mine"))
		.unwrap();
	assert!(format!("{:?}", mine.issues).contains("core/nope"));
	let fake = lib
		.reports()
		.iter()
		.find(|r| r.location.ends_with("fake"))
		.unwrap();
	assert!(!fake.loaded);
	assert_eq!(lib.packs().len(), 2);
}
