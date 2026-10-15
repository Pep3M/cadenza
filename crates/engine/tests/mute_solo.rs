//! Mute and solo-in-place.

mod common;

use cadenza_engine::{FLOOR_DB, analyze_mix, render_offline};
use common::*;

fn two_tracks() -> (cadenza_model::Track, cadenza_model::Track) {
	(
		synth_track("a", &[(60, 0.0, 4.0)], 4.0),
		synth_track("b", &[(67, 0.0, 4.0)], 4.0),
	)
}

#[test]
fn mute_silences_track_and_its_contribution() {
	let (a, mut b) = two_tracks();
	let only_a = render_offline(&project(vec![a.clone()]), &opts(4.0)).unwrap();
	b.channel.mute = true;
	let p = project(vec![a, b]);
	let both = render_offline(&p, &opts(4.0)).unwrap();
	assert_eq!(max_diff(&only_a, &both), 0.0);
	let report = analyze_mix(&p, &opts(4.0)).unwrap();
	assert!(report.tracks[0].stats.peak_db > -40.0);
	assert_eq!(report.tracks[1].stats.peak_db, FLOOR_DB);
}

#[test]
fn solo_in_place_silences_other_sources_but_keeps_buses() {
	let (mut a, b) = two_tracks();
	let mut fx = bus("fx");
	fx.channel
		.inserts
		.push(cadenza_model::Device::builtin("reverb", "Verb"));
	a.channel.solo = true;
	a.channel.sends.push(send(&fx, -6.0, false));
	let p = project(vec![a.clone(), b.clone(), fx.clone()]);
	let report = analyze_mix(&p, &opts(4.0)).unwrap();
	assert!(report.tracks[0].stats.peak_db > -40.0, "soloed track plays");
	assert_eq!(
		report.tracks[1].stats.peak_db, FLOOR_DB,
		"other source is silent"
	);
	assert!(
		report.tracks[2].stats.peak_db > -80.0,
		"bus fed by the solo stays active"
	);

	// Soloing a bus makes the sources routed into it audible.
	let (mut a, b) = two_tracks();
	let mut group = bus("group");
	group.channel.solo = true;
	a.channel.output = cadenza_model::Output::Bus(group.id);
	let report = analyze_mix(&project(vec![a, b, group]), &opts(4.0)).unwrap();
	assert!(report.tracks[0].stats.peak_db > -40.0);
	assert_eq!(report.tracks[1].stats.peak_db, FLOOR_DB);
	assert!(report.tracks[2].stats.peak_db > -40.0);
}

#[test]
fn folders_offset_volume_and_carry_mute_and_solo() {
	use cadenza_model::{Track, TrackKind};
	let (mut a, b) = two_tracks();
	let mut folder = Track::new("Group", TrackKind::Folder);
	a.folder = Some(folder.id);
	let plain = analyze_mix(&project(vec![a.clone(), b.clone()]), &opts(4.0)).unwrap();
	folder.channel.volume_db = -12.0;
	let p = project(vec![folder.clone(), a.clone(), b.clone()]);
	let report = analyze_mix(&p, &opts(4.0)).unwrap();
	assert_eq!(report.tracks.len(), 2, "the folder has no audio node");
	let drop = plain.tracks[0].stats.peak_db - report.tracks[0].stats.peak_db;
	assert!((drop - 12.0).abs() < 0.3, "VCA offset: {drop} dB");
	assert!((plain.tracks[1].stats.peak_db - report.tracks[1].stats.peak_db).abs() < 0.01);

	folder.channel.volume_db = 0.0;
	folder.channel.mute = true;
	let report = analyze_mix(
		&project(vec![folder.clone(), a.clone(), b.clone()]),
		&opts(4.0),
	)
	.unwrap();
	assert_eq!(report.tracks[0].stats.peak_db, FLOOR_DB, "folder mute");
	assert!(report.tracks[1].stats.peak_db > -40.0);

	folder.channel.mute = false;
	folder.channel.solo = true;
	let report = analyze_mix(&project(vec![folder, a, b]), &opts(4.0)).unwrap();
	assert!(
		report.tracks[0].stats.peak_db > -40.0,
		"inside the soloed folder"
	);
	assert_eq!(report.tracks[1].stats.peak_db, FLOOR_DB, "outside it");
}
