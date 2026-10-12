//! Portable projects: collect_files, relink_files, list_missing_files, archives and
//! opening projects whose folder moved.

use std::path::{Path, PathBuf};

use cadenza_core::Tick;
use cadenza_mcp::Session;
use cadenza_mcp::tools::{portable, project};
use cadenza_model::{Clip, Command, Device, Document, Project, SampleZone, Track, TrackKind};

fn write_wav(path: &Path, seed: f64) {
	std::fs::create_dir_all(path.parent().unwrap()).unwrap();
	let spec = hound::WavSpec {
		channels: 1,
		sample_rate: 48_000,
		bits_per_sample: 16,
		sample_format: hound::SampleFormat::Int,
	};
	let mut w = hound::WavWriter::create(path, spec).unwrap();
	for i in 0..4800 {
		w.write_sample(((i as f64 * seed).sin() * 8000.0) as i16)
			.unwrap();
	}
	w.finalize().unwrap();
}

/// Two clips on external files with identical content, one on a different file, and a
/// sampler zone on an external sample.
fn session_with_external_files(root: &Path) -> (Session, PathBuf, Vec<PathBuf>) {
	let ext = root.join("external");
	let a = ext.join("takes").join("vox.wav");
	let a_copy = ext.join("backup").join("vox copy.wav");
	let b = ext.join("guitar.wav");
	let s = ext.join("lib").join("Piano C4.wav");
	write_wav(&a, 0.05);
	std::fs::create_dir_all(a_copy.parent().unwrap()).unwrap();
	std::fs::copy(&a, &a_copy).unwrap();
	write_wav(&b, 0.07);
	write_wav(&s, 0.03);

	let mut p = Project::new("Song");
	let mut audio = Track::new("Audio", TrackKind::Audio);
	for (i, f) in [&a, &a_copy, &b].into_iter().enumerate() {
		audio.clips.push(Clip::audio(
			format!("c{i}"),
			Tick(960 * 4 * i as i64),
			Tick(960 * 4),
			f,
		));
	}
	p.tracks.push(audio);
	let mut sampler = Device::builtin("sampler", "Sampler");
	sampler.zones = vec![SampleZone::full_keyboard(&s, 60)];
	p.tracks
		.push(Track::new("Keys", TrackKind::Instrument).with_instrument(sampler));
	let file = root.join("Song").join("Song.cadenza.json");
	p.save_json(&file).unwrap();
	let mut session = Session::with_plugin_cache(root.join("plugins.json"));
	session.reset(Document::new(p), Some(file.clone()));
	(session, file, vec![a, a_copy, b, s])
}

fn paths(p: &Project) -> Vec<PathBuf> {
	p.file_refs().into_iter().map(|r| r.path).collect()
}

#[test]
fn collect_copies_once_per_content_rewrites_in_one_undo_step_and_survives_a_move() {
	let tmp = tempfile::tempdir().unwrap();
	let (mut session, file, _) = session_with_external_files(tmp.path());
	let before = session.document().project().clone();
	let root = file.parent().unwrap().to_path_buf();

	let dry = portable::collect_files(
		&mut session,
		portable::CollectFilesArgs {
			dry_run: Some(true),
			..Default::default()
		},
	)
	.unwrap();
	assert_eq!(dry.copied.len(), 3);
	assert!(!root.join("Audio").exists(), "dry run copies nothing");
	assert_eq!(session.document().project(), &before);

	let out = portable::collect_files(&mut session, Default::default()).unwrap();
	assert_eq!(out.copied.len(), 3, "{out:?}");
	assert_eq!(out.deduplicated, 1, "identical content copied once");
	assert_eq!(out.references_rewritten, 4);
	assert_eq!(
		out.bytes_copied,
		out.copied.iter().map(|c| c.bytes).sum::<u64>()
	);
	let project = session.document().project().clone();
	assert_eq!(
		paths(&project),
		vec![
			root.join("Audio/vox.wav"),
			root.join("Audio/vox.wav"),
			root.join("Audio/guitar.wav"),
			root.join("Samples/Piano C4.wav"),
		]
	);
	assert_eq!(
		session.document().history_labels().last().copied(),
		Some(portable::COLLECT_LABEL)
	);

	// Collecting again finds nothing to do.
	let again = portable::collect_files(&mut session, Default::default()).unwrap();
	assert!(again.copied.is_empty());
	assert_eq!(again.already_inside, 3);

	// Saved and moved elsewhere, the project still finds every file.
	project::save_project(
		&mut session,
		project::SaveProjectArgs {
			path: None,
			capture_plugin_states: Some(false),
		},
	)
	.unwrap();
	std::fs::remove_dir_all(tmp.path().join("external")).unwrap();
	let moved = tmp.path().join("Moved Song");
	std::fs::rename(&root, &moved).unwrap();
	let (doc, missing) = portable::open_document(&moved.join("Song.cadenza.json")).unwrap();
	assert!(missing.is_empty(), "{missing:?}");
	assert!(paths(doc.project()).iter().all(|p| p.starts_with(&moved)));

	// Undo restores the external references.
	session.document_mut().undo().unwrap();
	assert_eq!(session.document().project(), &before);
}

#[test]
fn collect_renames_on_conflicting_content_and_needs_a_saved_project() {
	let tmp = tempfile::tempdir().unwrap();
	let (mut session, file, files) = session_with_external_files(tmp.path());
	// A different file already sits at Audio/guitar.wav.
	let taken = file.parent().unwrap().join("Audio/guitar.wav");
	write_wav(&taken, 0.11);
	let out = portable::collect_files(&mut session, Default::default()).unwrap();
	let guitar = out.copied.iter().find(|c| c.from == files[2]).unwrap();
	assert_eq!(
		guitar.to,
		file.parent().unwrap().join("Audio/guitar (2).wav")
	);

	let mut unsaved = Session::with_plugin_cache(tmp.path().join("p.json"));
	let err = portable::collect_files(&mut unsaved, Default::default()).unwrap_err();
	assert!(err.to_string().contains("save it first"), "{err}");
}

#[test]
fn relink_finds_moved_files_by_name_in_one_undo_step() {
	let tmp = tempfile::tempdir().unwrap();
	let (mut session, _, files) = session_with_external_files(tmp.path());
	let new_home = tmp.path().join("new home");
	std::fs::rename(tmp.path().join("external"), &new_home).unwrap();
	// A decoy with the same name in a folder that matches less of the old path.
	write_wav(&new_home.join("other").join("vox.wav"), 0.2);

	let missing = portable::list_missing_files(&session);
	assert_eq!(missing.len(), 4);
	assert!(
		missing
			.iter()
			.any(|m| m.used_by == "sample_zone" && m.name == "Piano C4.wav")
	);

	let out = portable::relink_files(
		&mut session,
		portable::RelinkFilesArgs {
			search_folder: new_home.display().to_string(),
			dry_run: None,
		},
	)
	.unwrap();
	assert_eq!(out.relinked.len(), 4);
	assert!(out.still_missing.is_empty());
	assert_eq!(
		paths(session.document().project())[0],
		new_home.join("takes/vox.wav"),
		"the candidate whose folders match the old path wins"
	);
	assert!(portable::list_missing_files(&session).is_empty());
	assert_eq!(
		session.document().history_labels().last().copied(),
		Some(portable::RELINK_LABEL)
	);
	session.document_mut().undo().unwrap();
	assert_eq!(paths(session.document().project())[0], files[0]);

	let err = portable::relink_files(
		&mut session,
		portable::RelinkFilesArgs {
			search_folder: tmp.path().join("nope").display().to_string(),
			dry_run: None,
		},
	)
	.unwrap_err();
	assert!(err.to_string().contains("not a folder"));
}

#[test]
fn archives_round_trip_with_unsaved_state_and_refuse_overwrites() {
	let tmp = tempfile::tempdir().unwrap();
	let (mut session, file, _) = session_with_external_files(tmp.path());
	portable::collect_files(&mut session, Default::default()).unwrap();
	let root = file.parent().unwrap();
	std::fs::create_dir_all(root.join(".cadenza/autosave")).unwrap();
	std::fs::write(root.join(".cadenza/autosave/x.json"), "{}").unwrap();
	session
		.document_mut()
		.execute(Command::AddMarker {
			marker: cadenza_model::Marker {
				tick: Tick::ZERO,
				name: "Unsaved".into(),
			},
		})
		.unwrap();

	let archive = tmp.path().join("share").join("Song.cadenza.zip");
	let packed = portable::pack_current(
		&session,
		portable::PackProjectArgs {
			path: archive.display().to_string(),
			include_renders: None,
		},
	)
	.unwrap();
	assert_eq!(packed.files, 4, "project + 2 audio + 1 sample: {packed:?}");
	assert!(packed.outside.is_empty());

	let dest = tmp.path().join("unpacked");
	let opened = portable::unpack_project(&archive, &dest).unwrap();
	assert_eq!(opened, dest.join("Song").join("Song.cadenza.json"));
	assert!(
		!dest.join("Song/.cadenza").exists(),
		"caches are not packed"
	);
	let (doc, missing) = portable::open_document(&opened).unwrap();
	assert!(missing.is_empty(), "{missing:?}");
	assert_eq!(doc.project().markers[0].name, "Unsaved");
	assert!(paths(doc.project()).iter().all(|p| p.starts_with(&dest)));

	let err = portable::unpack_project(&archive, &dest).unwrap_err();
	assert!(err.to_string().contains("already exists"), "{err}");
	let err = portable::pack_current(
		&session,
		portable::PackProjectArgs {
			path: tmp.path().join("x.wav").display().to_string(),
			include_renders: None,
		},
	)
	.unwrap_err();
	assert!(err.to_string().contains(".cadenza.zip"));
}

#[test]
fn factory_samples_from_another_machine_are_relinked_on_open() {
	let tmp = tempfile::tempdir().unwrap();
	let local = tmp.path().join("local").join("samples").join("factory");
	write_wav(&local.join("Kick.wav"), 0.01);
	let mut p = Project::new("F");
	let mut drums = Device::builtin("drum_sampler", "Drums");
	drums.zones = vec![SampleZone::pad(
		"/home/someone/.local/share/cadenza/samples/factory/Kick.wav",
		36,
	)];
	p.tracks
		.push(Track::new("Drums", TrackKind::Instrument).with_instrument(drums));
	let mapping = portable::factory_relinks(&p, &local);
	assert_eq!(mapping.values().next(), Some(&local.join("Kick.wav")));
}
