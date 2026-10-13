use super::*;
use cadenza_core::PPQ;
use cadenza_model::{Clip, Track};

fn state_with_song() -> (AppState, TrackId) {
	let mut s = AppState::headless();
	let mut p = Project::new("Song");
	let mut keys = Track::new("Keys", TrackKind::Instrument);
	keys.instrument = Some(cadenza_model::Device::builtin("poly_synth", "Synth"));
	keys.clips.push(
		Clip::midi("A", Tick::ZERO, Tick(PPQ * 4)).with_notes([cadenza_model::Note::new(
			60,
			Tick::ZERO,
			Tick(PPQ),
			0.8,
		)]),
	);
	let id = keys.id;
	p.tracks.push(keys);
	s.load_document(Document::new(p), None);
	s.dirty = false;
	(s, id)
}

#[test]
fn edits_mark_dirty_schedule_a_reload_and_undo() {
	let (mut s, keys) = state_with_song();
	let now = Instant::now();
	s.tick(now + RELOAD_DEBOUNCE);
	assert!(!s.reload_pending());
	assert!(!s.dirty);

	let cmd = edits::add_track(
		s.project(),
		edits::new_track(s.project(), TrackKind::Bus),
		None,
	);
	assert!(s.apply(cmd));
	assert!(s.dirty);
	assert!(s.reload_pending());
	assert_eq!(s.project().tracks.len(), 2);
	assert_eq!(s.undo_label(), Some("Add track 'Bus 1'"));

	// Debounced: nothing is sent before the window elapses.
	let t0 = Instant::now();
	s.tick(t0);
	assert!(s.reload_pending());
	s.tick(t0 + RELOAD_DEBOUNCE * 2);
	assert!(!s.reload_pending());

	s.undo();
	assert_eq!(s.project().tracks.len(), 1);
	assert_eq!(s.redo_label(), Some("Add track 'Bus 1'"));
	assert!(s.reload_pending());
	s.redo();
	assert_eq!(s.project().tracks.len(), 2);

	// A rejected command changes nothing and reports why.
	assert!(!s.apply(Command::RemoveTrack {
		track: TrackId::new()
	}));
	assert_eq!(s.notices.count(notices::Level::Error), 1);
	assert_eq!(s.project().tracks[0].id, keys);
}

#[test]
fn fader_gestures_are_one_undo_step() {
	let (mut s, keys) = state_with_song();
	for db in [-1.0, -2.0, -3.5] {
		s.preview_volume(Some(keys), db);
		assert_eq!(s.volume_db(Some(keys)), db, "preview shown");
	}
	assert_eq!(
		s.project().tracks[0].channel.volume_db,
		0.0,
		"not committed yet"
	);
	assert!(s.document().history_labels().is_empty());
	s.commit_volume(Some(keys), -3.5);
	assert_eq!(s.project().tracks[0].channel.volume_db, -3.5);
	assert_eq!(s.document().history_labels().len(), 1);
	assert!(s.previews.volume.is_empty());
	// Committing the same value again is not a new step.
	s.commit_volume(Some(keys), -3.5);
	s.commit_pan(None, 0.25);
	assert_eq!(s.project().master.pan, 0.25);
	assert_eq!(s.document().history_labels().len(), 2);
	s.undo();
	s.undo();
	assert_eq!(s.volume_db(Some(keys)), 0.0);
}

#[test]
fn param_and_send_edits_reach_the_player_live_without_reloading() {
	let (mut s, keys) = state_with_song();
	let synth = s.project().tracks[0].instrument.as_ref().unwrap().id;
	s.tick(Instant::now() + RELOAD_DEBOUNCE);
	assert!(!s.reload_pending());
	for v in [500.0, 700.0, 900.0] {
		s.preview_param(synth, "filter_cutoff", v);
		assert_eq!(s.param_value(Some(keys), synth, "filter_cutoff"), Some(v));
	}
	assert!(s.document().history_labels().is_empty());
	s.commit_param(Some(keys), synth, "filter_cutoff", 900.0);
	assert_eq!(s.document().history_labels().len(), 1);
	assert!(!s.reload_pending(), "a knob edit never reloads the player");
	s.commit_volume(Some(keys), -6.0);
	s.undo();
	assert!(!s.reload_pending(), "undoing a mix value is live too");
	// Volume automation drives the displayed value at the playhead.
	assert!(s.apply(Command::SetAutomation {
		address: volume_address(Some(keys)),
		points: vec![
			cadenza_model::AutomationPoint::new(Tick::ZERO, -20.0),
			cadenza_model::AutomationPoint::new(Tick(PPQ * 4), 0.0),
		],
	}));
	assert!(!s.reload_pending(), "new automation is swapped in live");
	s.seek(Tick(PPQ * 2));
	assert!((s.volume_db(Some(keys)) + 10.0).abs() < 1e-4);
	// Structural edits still reload.
	assert!(s.apply(Command::SetChannel {
		track: Some(keys),
		patch: ChannelPatch {
			mute: Some(true),
			..Default::default()
		},
	}));
	assert!(s.reload_pending());
}

#[test]
fn browser_devices_target_the_selection() {
	let (mut s, keys) = state_with_song();
	let items = s.browser_items().to_vec();
	let reverb = items.iter().find(|i| i.id == "reverb").unwrap();
	let piano = items.iter().find(|i| i.id == "electric_piano").unwrap();

	// No selection: effects go to the master...
	s.selection.select_master();
	assert!(s.use_device(reverb, None));
	assert_eq!(s.project().master.inserts.len(), 1);
	// ...instruments create a new instrument track.
	assert!(s.use_device(piano, None));
	assert_eq!(s.project().tracks.len(), 2);
	assert_eq!(s.project().tracks[1].name, piano.name);

	// On an instrument track: replace its instrument, append effects.
	s.selection.select_track(keys);
	assert!(s.use_device(piano, None));
	let t = s.project().track(keys).unwrap();
	assert_eq!(
		t.instrument.as_ref().map(|d| &d.kind),
		Some(&cadenza_model::DeviceKind::Builtin {
			id: "electric_piano".into()
		})
	);
	assert!(s.use_device(reverb, Some(keys)));
	let fx = s.project().track(keys).unwrap().channel.inserts[0].id;
	assert_eq!(
		s.selection.device,
		Some(DeviceRef {
			track: Some(keys),
			device: fx
		})
	);

	// Presets replace the params in one step; removing selects nothing.
	let preset = &cadenza_dsp::factory_presets("reverb")[0];
	let r = s.selection.device.unwrap();
	s.load_factory_preset(r, preset);
	assert_eq!(s.device(r).unwrap().params.len(), preset.params.len());
	s.remove_device(Some(keys), fx);
	assert!(s.selection.device.is_none());
	let instrument = s
		.project()
		.track(keys)
		.unwrap()
		.instrument
		.as_ref()
		.unwrap()
		.id;
	s.remove_device(Some(keys), instrument);
	assert!(s.project().track(keys).unwrap().instrument.is_none());
}

#[test]
fn unsaved_changes_are_confirmed_and_files_round_trip() {
	let (mut s, _) = state_with_song();
	let dir = tempfile::tempdir().unwrap();
	let path = dir.path().join("song.cadenza.json");

	// Clean: proceeds at once.
	s.request(Pending::New);
	assert!(s.confirm.is_none());
	assert!(s.project().tracks.is_empty());

	let cmd = edits::add_track(
		s.project(),
		edits::new_track(s.project(), TrackKind::Audio),
		None,
	);
	s.apply(cmd);
	s.request(Pending::Quit);
	assert_eq!(s.confirm, Some(Pending::Quit));
	assert!(!s.quit_confirmed);

	assert!(s.save_as(dir.path().join("song")));
	assert_eq!(s.path(), Some(path.as_path()), "extension added");
	assert!(!s.dirty);
	assert!(s.title().contains("song.cadenza.json"));

	s.request(Pending::New);
	assert!(s.open(&path));
	assert_eq!(s.project().tracks.len(), 1);
	assert!(!s.dirty);
	assert!(!s.open(&dir.path().join("missing.json")));
	assert_eq!(
		s.project().tracks.len(),
		1,
		"a failed open keeps the project"
	);

	s.apply(Command::SetTempo {
		tick: Tick::ZERO,
		bpm: 100.0,
	});
	s.save_then(Pending::Quit);
	assert!(s.quit_confirmed && !s.dirty);
	assert_eq!(
		Project::load_json(&path).unwrap().tempo.bpm_at(Tick::ZERO),
		100.0
	);
}

#[test]
fn imports_use_the_shared_tools() {
	let (mut s, _) = state_with_song();
	let dir = tempfile::tempdir().unwrap();
	let wav = dir.path().join("loop.wav");
	let samples = vec![0.25f32; 48_000];
	let buf =
		cadenza_audio_io::AudioBuffer::from_planar(48_000, vec![samples.clone(), samples]).unwrap();
	cadenza_audio_io::export(
		&buf,
		&wav,
		&cadenza_audio_io::ExportOptions::wav(cadenza_audio_io::WavSampleFormat::Int16),
	)
	.unwrap();
	s.seek(Tick(PPQ * 4));
	s.import_audio(&wav);
	let audio = s.project().tracks.last().unwrap();
	assert_eq!(audio.kind, TrackKind::Audio);
	assert_eq!(audio.clips[0].start, Tick(PPQ * 4));
	assert_eq!(s.selection.clips.len(), 1);
	assert_eq!(s.document().history_labels().len(), 1, "one undo step");

	let mid = dir.path().join("riff.mid");
	std::fs::write(
		&mid,
		cadenza_midi_io::export_project(&state_with_song().0.project().clone()).unwrap(),
	)
	.unwrap();
	let before = s.project().tracks.len();
	s.import_midi(&mid);
	assert!(s.project().tracks.len() > before);

	s.import_midi(&dir.path().join("nope.mid"));
	assert!(s.notices.count(notices::Level::Error) >= 1);
}

#[test]
fn transport_without_audio_keeps_a_position() {
	let (mut s, _) = state_with_song();
	s.seek(Tick(-5));
	assert_eq!(s.position(), Tick::ZERO);
	s.seek(Tick(PPQ));
	assert_eq!(s.position(), Tick(PPQ));
	s.play();
	assert!(!s.is_playing());
	assert_eq!(s.notices.count(notices::Level::Warning), 1);
}

#[test]
fn drops_create_tracks_and_import_midi_at_the_drop_beat() {
	let (mut s, keys) = state_with_song();
	let synth = s
		.browser_items()
		.iter()
		.find(|i| i.id == "electric_piano")
		.unwrap()
		.clone();
	let verb = s
		.browser_items()
		.iter()
		.find(|i| i.id == "reverb")
		.unwrap()
		.clone();
	let at = Tick(PPQ * 8);
	assert!(s.drop_payload(&Payload::Device(synth), DropTarget::Below { at }, false));
	let t = s.project().tracks.last().unwrap().clone();
	assert_eq!(t.kind, TrackKind::Instrument);
	assert_eq!(t.instrument.as_ref().unwrap().name, "Electric Piano");
	assert_eq!(
		s.undo_label().map(str::to_owned),
		Some(format!("Add track '{}'", t.name))
	);
	assert!(s.drop_payload(
		&Payload::Device(verb),
		DropTarget::InsertSlot {
			track: Some(keys),
			slot: 0
		},
		false
	));
	assert_eq!(
		s.project().track(keys).unwrap().channel.inserts[0].name,
		"Reverb"
	);

	// A MIDI file dropped below the tracks: new tracks starting at the drop beat, one
	// undo step.
	let dir = tempfile::tempdir().unwrap();
	let mid = dir.path().join("riff.mid");
	std::fs::write(&mid, cadenza_midi_io::export_project(s.project()).unwrap()).unwrap();
	let before = s.project().tracks.len();
	let history = s.document().history_labels().len();
	assert!(s.drop_payload(&Payload::File(mid), DropTarget::Below { at }, false));
	assert!(s.project().tracks.len() > before);
	assert_eq!(s.document().history_labels().len(), history + 1);
	let new = &s.project().tracks[before];
	assert!(
		new.clips.iter().all(|c| c.start >= at),
		"shifted to the drop beat"
	);
	// Rejected drops change nothing.
	assert!(!s.drop_payload(
		&Payload::File(dir.path().join("notes.txt")),
		DropTarget::Window,
		false
	));
}

#[test]
fn note_previews_are_tracked_and_released() {
	let (mut s, keys) = state_with_song();
	let clip = s.project().track(keys).unwrap().clips[0].id;
	assert!(s.open_piano_roll(clip));
	assert!(s.view.panels.visible(PanelKind::PianoRoll));
	assert_eq!(s.piano_track(), Some(keys));
	s.preview_note_on(keys, 60, 0.8);
	s.preview_note_on(keys, 64, 0.8);
	s.preview_note_on(keys, 64, 0.8);
	assert_eq!(s.previewing.len(), 2);
	s.preview_note_off(keys, 60);
	assert_eq!(s.previewing, vec![(keys, 64)]);
	// Another track: the held notes are released first.
	let other = TrackId::new();
	s.preview_note_on(other, 50, 0.8);
	assert_eq!(s.previewing, vec![(other, 50)]);
	s.close_piano_roll();
	assert!(s.previewing.is_empty());
	assert!(!s.view.panels.visible(PanelKind::PianoRoll));
	// Removing the clip closes the editor's selection state.
	s.view.piano.selected.insert(cadenza_core::NoteId::new());
	s.apply(Command::RemoveClip { clip });
	assert_eq!(s.view.piano.clip, None);
	assert!(s.view.piano.selected.is_empty());
}

#[test]
fn prefs_restore_layout_and_zoom() {
	let (mut s, _) = state_with_song();
	s.view.panels.toggle(PanelKind::Inspector);
	s.view.timeline.px_per_beat = 40.0;
	s.view.piano.scale = cadenza_theory::Scale::parse("D dorian").ok();
	s.view.follow = FollowMode::Continuous;
	s.view.panels.open_piano_roll();
	let prefs = s.prefs();
	assert!(
		prefs.panels.mixer,
		"the mixer only yielded to the piano roll"
	);
	let mut t = AppState::headless();
	t.apply_prefs(prefs);
	assert!(!t.view.panels.inspector);
	assert!(!t.view.panels.piano_roll, "no clip to show yet");
	assert_eq!(t.view.timeline.px_per_beat, 40.0);
	assert_eq!(t.view.follow, FollowMode::Continuous);
	assert_eq!(t.view.piano.scale, s.view.piano.scale);
	// Out-of-range values are clamped.
	let mut bad = t.prefs();
	bad.px_per_beat = 1e9;
	bad.track_height = -3.0;
	t.apply_prefs(bad);
	assert_eq!(t.view.timeline.px_per_beat, timeline::MAX_PX_PER_BEAT);
	assert_eq!(t.view.track_height, MIN_TRACK_HEIGHT);
}

#[test]
fn f7_opens_the_track_clip_or_creates_one() {
	let (mut s, keys) = state_with_song();
	let clip_a = s.project().tracks[0].clips[0].id;
	// Under the playhead.
	s.selection.select_track(keys);
	s.seek(Tick(PPQ));
	s.toggle_piano_roll();
	assert_eq!(s.view.piano.clip, Some(clip_a));
	assert!(s.view.panels.visible(PanelKind::PianoRoll));
	// F7 again hides it.
	s.toggle_piano_roll();
	assert!(!s.view.panels.visible(PanelKind::PianoRoll));
	// Nothing under the playhead: the last clip opened on the track.
	s.selection.select_track(keys);
	s.seek(Tick(PPQ * 9));
	s.toggle_piano_roll();
	assert_eq!(s.view.piano.clip, Some(clip_a));
	s.close_piano_roll();

	// A track with no clip: a one-bar clip at the playhead's bar, named after it.
	let mut t = edits::new_track(s.project(), TrackKind::Instrument);
	t.name = "Electric Piano".into();
	let ep = t.id;
	assert!(s.apply(edits::add_track(s.project(), t, None)));
	let steps = s.document().history_labels().len();
	s.selection.select_track(ep);
	s.seek(Tick(PPQ * 9));
	s.toggle_piano_roll();
	let created = s.view.piano.clip.expect("a clip is open");
	let (track, clip) = s.project().find_clip(created).unwrap();
	assert_eq!(track.id, ep);
	assert_eq!(
		(clip.start, clip.length, clip.name.as_str()),
		(Tick(PPQ * 8), Tick(PPQ * 4), "Electric Piano")
	);
	assert_eq!(s.document().history_labels().len(), steps + 1, "one step");
	assert!(s.view.panels.visible(PanelKind::PianoRoll));
	s.undo();
	assert!(s.project().find_clip(created).is_none());
	assert_eq!(s.view.piano.clip, None, "pruned with the clip");

	// Not an instrument track: a hint, nothing created.
	let bus = edits::new_track(s.project(), TrackKind::Bus);
	let bus_id = bus.id;
	assert!(s.apply(edits::add_track(s.project(), bus, None)));
	assert!(!s.open_track_in_piano_roll(bus_id));
	assert!(s.notices.last().unwrap().text.contains("not an instrument"));

	// F7 with the piano roll open on another track's clip switches to the selected track.
	s.selection.select_track(keys);
	s.toggle_piano_roll();
	let open = s.view.piano.clip.expect("a clip is open");
	assert_eq!(s.project().find_clip(open).unwrap().0.id, keys);
	s.selection.select_track(ep);
	s.toggle_piano_roll();
	let open = s.view.piano.clip.expect("a clip is open");
	assert_eq!(s.project().find_clip(open).unwrap().0.id, ep);
	assert!(s.view.panels.visible(PanelKind::PianoRoll));
	// Same track: F7 closes it.
	s.toggle_piano_roll();
	assert!(!s.view.panels.visible(PanelKind::PianoRoll));
}

#[test]
fn adding_an_instrument_selects_its_track_and_hints_at_f7() {
	let (mut s, _) = state_with_song();
	let items = s.browser_items().to_vec();
	let piano = items.iter().find(|i| i.id == "electric_piano").unwrap();
	s.selection.select_master();
	assert!(s.use_device(piano, None));
	let new = s.project().tracks.last().unwrap().id;
	assert_eq!(s.selection.track, Some(new));
	assert!(s.notices.last().unwrap().text.contains("F7"));
	s.toggle_piano_roll();
	let clip = s.view.piano.clip.unwrap();
	assert_eq!(s.project().find_clip(clip).unwrap().0.id, new);
	// Dropped below the tracks: same.
	assert!(s.new_track_with(piano, None));
	assert_eq!(s.selection.track, s.project().tracks.last().map(|t| t.id));
}

#[test]
fn pattern_mode_loops_the_piano_roll_clip_alone_and_falls_back() {
	use transport::PlayMode;
	let (mut s, keys) = state_with_song();
	let clip_a = s.project().tracks[0].clips[0].id;
	s.set_play_mode(PlayMode::Pattern);
	// No clip open: plays the song, with a hint.
	s.play();
	assert!(
		s.notices
			.log()
			.iter()
			.any(|n| n.text.contains("Pattern mode plays the clip"))
	);
	assert_eq!(s.play_config().unwrap().audition, None);
	s.stop();

	assert!(s.open_piano_roll(clip_a));
	s.seek(Tick(PPQ * 12));
	s.play();
	let c = s.play_config().unwrap().clone();
	assert_eq!(c.loop_range, Some((Tick::ZERO, Tick(PPQ * 4))));
	assert_eq!(c.audition, Some(vec![keys]));
	assert_eq!(s.position(), Tick::ZERO, "starts at the clip");
	// The clip grows while playing: the loop follows.
	assert!(s.apply(Command::UpdateClip {
		clip: clip_a,
		patch: cadenza_model::ClipPatch {
			length: Some(Tick(PPQ * 8)),
			..Default::default()
		},
	}));
	s.tick(Instant::now());
	assert_eq!(
		s.play_config().unwrap().loop_range,
		Some((Tick::ZERO, Tick(PPQ * 8)))
	);
	// Switching to song mode restarts without the audition or the loop.
	s.toggle_play_mode();
	assert_eq!(s.view.transport.mode, PlayMode::Song);
	assert!(s.play_intent());
	assert_eq!(s.play_config(), Some(&transport::PlayConfig::default()));
	s.toggle_play_mode();
	assert_eq!(s.play_config().unwrap().audition, Some(vec![keys]));
	s.stop();
	assert_eq!(
		s.play_config().unwrap().audition,
		None,
		"stopped: no audition"
	);
	assert!(s.play_config().unwrap().loop_range.is_some());
}

#[test]
fn pattern_mode_starts_at_the_pattern_and_follows_drawn_notes() {
	use transport::PlayMode;
	let (mut s, _) = state_with_song();
	let clip = s.project().tracks[0].clips[0].id;
	assert!(s.open_piano_roll(clip));
	s.set_play_mode(PlayMode::Pattern);
	// FL Studio: Play starts at the pattern start, Stop goes back there.
	s.seek(Tick(PPQ * 2));
	s.stop();
	assert_eq!(
		s.position(),
		Tick::ZERO,
		"stop rewinds to the pattern start"
	);
	s.play();
	assert_eq!(s.position(), Tick::ZERO);
	s.stop();
	// ... unless the playhead was placed inside the pattern since the last stop.
	s.seek(Tick(PPQ * 2));
	s.play();
	assert_eq!(s.position(), Tick(PPQ * 2), "cued inside the pattern");
	assert_eq!(
		s.play_config().unwrap().loop_range,
		Some((Tick::ZERO, Tick(PPQ * 4)))
	);
	// A note drawn past the end grows the clip to the next bar and the loop follows on
	// the next tick, without moving the playhead (no restart).
	let (cmd, _) = piano::add_note(s.project(), clip, Tick(PPQ * 5), 64, Tick(PPQ), 0.8).unwrap();
	assert!(s.apply(cmd));
	s.tick(Instant::now());
	assert_eq!(
		s.play_config().unwrap().loop_range,
		Some((Tick::ZERO, Tick(PPQ * 8)))
	);
	assert_eq!(s.position(), Tick(PPQ * 2), "still playing where it was");
	assert!(s.play_intent());
	// Undo shrinks it back and the loop follows again.
	s.undo();
	s.tick(Instant::now());
	assert_eq!(
		s.play_config().unwrap().loop_range,
		Some((Tick::ZERO, Tick(PPQ * 4)))
	);
	s.stop();
	assert_eq!(s.position(), Tick::ZERO);
}

#[test]
fn loop_region_gestures_drive_song_playback() {
	let (mut s, _) = state_with_song();
	assert_eq!(s.view.transport.loop_range, None);
	// Ctrl+L without a region loops the bar at the playhead.
	s.seek(Tick(PPQ * 5));
	s.toggle_loop();
	assert!(s.view.transport.loop_on);
	assert_eq!(
		s.view.transport.loop_range,
		Some((Tick(PPQ * 4), Tick(PPQ * 8)))
	);
	assert_eq!(
		s.play_config().unwrap().loop_range,
		Some((Tick(PPQ * 4), Tick(PPQ * 8)))
	);
	s.toggle_loop();
	assert_eq!(s.play_config().unwrap().loop_range, None);
	// A ruler drag sets a region and turns the loop on.
	let range = transport::loop_from_drag(Tick(PPQ * 9), Tick(PPQ * 2), Some(Tick(PPQ)));
	s.set_loop_range(range);
	assert!(s.view.transport.loop_on);
	assert_eq!(
		s.play_config().unwrap().loop_range,
		Some((Tick(PPQ * 2), Tick(PPQ * 9)))
	);
	// Playing from past the end starts at the loop start.
	s.seek(Tick(PPQ * 12));
	s.play();
	assert_eq!(s.position(), Tick(PPQ * 2));
	s.stop();
	// New project: the region is gone.
	s.new_project();
	assert_eq!(s.view.transport.loop_range, None);
	assert!(!s.view.transport.loop_on);
}

#[test]
fn note_edits_reschedule_live_without_reloading() {
	let (mut s, _) = state_with_song();
	let clip = s.project().tracks[0].clips[0].id;
	s.tick(Instant::now() + RELOAD_DEBOUNCE);
	assert!(!s.reload_pending());
	assert!(s.apply(Command::AddNotes {
		clip,
		notes: vec![cadenza_model::Note::new(64, Tick(PPQ), Tick(PPQ), 0.7)],
	}));
	assert!(!s.reload_pending(), "piano roll edits do not recompile");
	assert!(s.apply(Command::UpdateClip {
		clip,
		patch: cadenza_model::ClipPatch {
			start: Some(Tick(PPQ * 4)),
			..Default::default()
		},
	}));
	assert!(!s.reload_pending(), "nor do MIDI clip moves");
}

#[test]
fn plugin_editor_gestures_are_undoable_and_live_states_need_no_reload() {
	use cadenza_engine::{EditorEvent, EditorState, EditorUpdate};
	let (mut s, keys) = state_with_song();
	let mut fx = cadenza_model::Device::builtin("unused", "Surge XT Effects");
	fx.kind = cadenza_model::DeviceKind::Clap {
		plugin_id: "org.surge-synth-team.surge-xt-fx".into(),
		path: "/usr/lib/clap/Surge XT Effects.clap".into(),
	};
	let fx_id = fx.id;
	assert!(s.apply(Command::InsertDevice {
		track: Some(keys),
		device: fx,
		index: None,
	}));
	let t0 = Instant::now();
	s.tick(t0 + RELOAD_DEBOUNCE * 2);
	assert!(!s.reload_pending());
	assert!(s.is_plugin(fx_id));
	let steps = s.document().history_labels().len();

	// Without an audio engine there is no editor to open: say so.
	s.open_plugin_editor(fx_id);
	assert!(!s.plugin_editor_open(fx_id));

	// One gesture in the plugin's GUI = one undo step, applied live.
	let up = |event| EditorUpdate {
		device: fx_id,
		event,
	};
	s.editor_updates(
		vec![
			up(EditorEvent::GestureBegin { param: "12".into() }),
			up(EditorEvent::Value {
				param: "12".into(),
				value: 0.25,
			}),
		],
		t0,
	);
	assert_eq!(s.param_value(Some(keys), fx_id, "12"), Some(0.25));
	assert_eq!(s.document().history_labels().len(), steps);
	s.editor_updates(
		vec![
			up(EditorEvent::Value {
				param: "12".into(),
				value: 0.75,
			}),
			up(EditorEvent::GestureEnd { param: "12".into() }),
		],
		t0,
	);
	assert_eq!(s.document().history_labels().len(), steps + 1);
	assert_eq!(
		s.project()
			.device(Some(keys), fx_id)
			.unwrap()
			.params
			.get("12"),
		Some(&0.75)
	);
	assert!(!s.reload_pending(), "parameter edits are live");
	s.undo();
	assert_eq!(s.document().history_labels().len(), steps);
	// (The parameter had no explicit value before: back to the plugin's own, reload.)
	assert!(s.reload_pending());
	s.tick(Instant::now() + RELOAD_DEBOUNCE * 2);

	// A state captured from the running instance is stored without a reload; undoing
	// it reloads (the instance must go back to the old state).
	s.store_editor_states(vec![EditorState {
		track: Some(keys),
		device: fx_id,
		state: "U1RBVEU=".into(),
	}]);
	assert_eq!(
		s.project()
			.device(Some(keys), fx_id)
			.unwrap()
			.state
			.as_deref(),
		Some("U1RBVEU=")
	);
	assert!(!s.reload_pending());
	s.undo();
	assert!(s.reload_pending());
	// An identical state is not stored twice.
	s.tick(Instant::now() + RELOAD_DEBOUNCE * 2);
	s.redo();
	let steps = s.document().history_labels().len();
	s.store_editor_states(vec![EditorState {
		track: Some(keys),
		device: fx_id,
		state: "U1RBVEU=".into(),
	}]);
	assert_eq!(s.document().history_labels().len(), steps);
}

#[test]
fn edits_by_another_owner_of_the_session_go_through_the_same_bookkeeping() {
	let (mut s, keys) = state_with_song();
	let now = Instant::now();
	s.tick(now + RELOAD_DEBOUNCE);
	s.selection.select_track(keys);
	let shared = s.session.clone();

	// The lock is held elsewhere (an agent tool running): the frame does not wait.
	{
		let mut session = shared.lock().unwrap();
		session
			.document_mut()
			.execute(Command::RemoveTrack { track: keys })
			.unwrap();
		s.tick(now + RELOAD_DEBOUNCE * 2);
		assert_eq!(s.project().tracks.len(), 1, "snapshot not refreshed yet");
	}
	s.tick(now + RELOAD_DEBOUNCE * 3);
	assert!(s.project().tracks.is_empty());
	assert!(s.dirty);
	assert_eq!(s.selection.track, None, "pruned");
	assert_eq!(s.undo_label(), Some(s.document().history_labels()[0]));

	// A save by the other owner clears the dirty flag and updates the path.
	let dir = tempfile::tempdir().unwrap();
	let path = dir.path().join("x.cadenza.json");
	cadenza_mcp::tools::project::save_project(
		&mut shared.lock().unwrap(),
		cadenza_mcp::tools::project::SaveProjectArgs {
			path: Some(path.display().to_string()),
			capture_plugin_states: None,
		},
	)
	.unwrap();
	s.tick(now + RELOAD_DEBOUNCE * 4);
	assert!(!s.dirty);
	assert_eq!(s.path(), Some(path.as_path()));
}

pub(crate) mod ai_chat {
	use super::*;
	use crate::state::ai::secrets::{MemorySecretStore, SecretStore};
	use crate::state::ai::settings::ProviderKind;
	use crate::state::ai::{AiChat, ChatItem};
	use cadenza_agent::scripted::{ScriptedProvider, ScriptedTurn};

	pub struct Chat {
		pub state: AppState,
		pub _dir: tempfile::TempDir,
	}

	/// A state whose chat answers from `turns` (no network); the key is in memory.
	pub fn chat_state(turns: Vec<ScriptedTurn>, delay_ms: u64, with_key: bool) -> Chat {
		let (mut state, _) = state_with_song();
		let dir = tempfile::tempdir().unwrap();
		let provider =
			ScriptedProvider::new(turns).with_delay(std::time::Duration::from_millis(delay_ms));
		let store = MemorySecretStore::default();
		if with_key {
			store
				.set(ProviderKind::OpenRouter.account(), "test-key")
				.unwrap();
		}
		state.ai = AiChat::with_parts(
			Arc::new(move |_, _| Ok(Box::new(provider.clone()))),
			Arc::new(store),
			dir.path().join("session"),
			dir.path().to_path_buf(),
			None,
		);
		Chat { state, _dir: dir }
	}

	/// Ticks until the running request ends.
	pub fn wait(s: &mut AppState) {
		let start = Instant::now();
		loop {
			s.tick(Instant::now());
			if !s.ai.busy() {
				return;
			}
			assert!(start.elapsed().as_secs() < 20, "request did not finish");
			std::thread::sleep(std::time::Duration::from_millis(5));
		}
	}

	#[test]
	fn a_request_streams_into_the_chat_and_is_one_undo_step() {
		let mut c = chat_state(
			vec![
				ScriptedTurn::tools(
					Some("Adding a bass."),
					&[
						("create_track", r#"{"name":"Bass","kind":"instrument"}"#),
						("add_marker", r#"{"beat":16,"name":"Bridge"}"#),
						("add_notes", r#"{"clip_id":"nope","notes":[]}"#),
					],
				)
				.with_cost(0.002),
				ScriptedTurn::text("Added **Bass** and a Bridge marker.").with_cost(0.001),
			],
			0,
			true,
		);
		let s = &mut c.state;
		let steps = s.document().history_labels().len();
		assert!(s.send_ai("add a bass"));
		assert!(s.ai.busy());
		wait(s);
		assert_eq!(s.project().tracks.len(), 2);
		assert!(s.dirty);
		assert_eq!(s.document().history_labels().len(), steps + 1);
		assert_eq!(s.undo_label(), Some("AI: add a bass"));
		let items = &s.ai.items;
		assert_eq!(items[0], ChatItem::User("add a bass".into()));
		assert!(items.contains(&ChatItem::Assistant {
			text: "Adding a bass.".into(),
			streaming: false
		}));
		let tools: Vec<(&str, Option<bool>)> = items
			.iter()
			.filter_map(|i| match i {
				ChatItem::Tool { name, ok, .. } => Some((name.as_str(), *ok)),
				_ => None,
			})
			.collect();
		assert_eq!(
			tools,
			vec![
				("create_track", Some(true)),
				("add_marker", Some(true)),
				("add_notes", Some(false))
			]
		);
		let Some(ChatItem::Footer {
			status,
			cost_usd,
			undo_label,
			..
		}) = items.last()
		else {
			panic!("no footer: {items:?}")
		};
		assert_eq!(status, "done");
		assert!((cost_usd - 0.003).abs() < 1e-9);
		assert_eq!(undo_label.as_deref(), Some("AI: add a bass"));
		assert!((s.ai.total_usd - 0.003).abs() < 1e-9);

		// One Ctrl+Z takes the whole request back.
		s.undo();
		assert_eq!(s.project().tracks.len(), 1);
		assert!(s.project().markers.is_empty());
		assert!(s.ai.transcript_path().is_file());
	}

	#[test]
	fn user_edits_wait_while_the_ai_works_and_stop_keeps_one_step() {
		let mut c = chat_state(
			vec![
				ScriptedTurn::tools(None, &[("add_marker", r#"{"beat":0,"name":"A"}"#)]),
				ScriptedTurn::tools(None, &[("add_marker", r#"{"beat":4,"name":"B"}"#)]),
				ScriptedTurn::tools(None, &[("add_marker", r#"{"beat":8,"name":"C"}"#)]),
				ScriptedTurn::text("never reached"),
			],
			150,
			true,
		);
		let s = &mut c.state;
		let keys = s.project().tracks[0].id;
		assert!(s.send_ai("mark sections"));
		assert!(!s.send_ai("second request"), "one at a time");
		// User edits are refused while the AI works (with a notice), undo too.
		assert!(!s.apply(Command::RemoveTrack { track: keys }));
		s.undo();
		assert_eq!(s.project().tracks.len(), 1);
		assert!(s.notices.last().unwrap().text.contains("AI is editing"));
		// A plugin editor value is queued instead.
		assert!(s.apply_or_queue(Command::UpdateTrack {
			track: keys,
			patch: cadenza_model::TrackPatch {
				name: Some("Rhodes".into()),
				..Default::default()
			},
		}));
		// Stop after the first edit lands.
		let start = Instant::now();
		while s.project().markers.is_empty() {
			s.tick(Instant::now());
			assert!(start.elapsed().as_secs() < 10);
			std::thread::sleep(std::time::Duration::from_millis(5));
		}
		s.ai.stop();
		wait(s);
		assert!(s.project().markers.len() < 3, "stopped early");
		assert!(matches!(
			s.ai.items.last(),
			Some(ChatItem::Footer { status, .. }) if status == "stopped"
		));
		// The AI's edits are one step, the queued user edit came after it.
		let labels = s.document().history_labels();
		assert_eq!(labels[labels.len() - 2], "AI: mark sections");
		assert_eq!(s.project().tracks[0].name, "Rhodes");
		s.undo();
		s.undo();
		assert!(s.project().markers.is_empty());
		// Editing works again.
		assert!(s.apply(Command::RemoveTrack { track: keys }));
	}

	#[test]
	fn a_missing_key_is_an_actionable_error_that_opens_the_settings() {
		if std::env::var_os("OPENROUTER_API_KEY").is_some() {
			return;
		}
		let mut c = chat_state(vec![], 0, false);
		let s = &mut c.state;
		assert!(s.send_ai("hello"));
		wait(s);
		assert!(s.ai.items.iter().any(|i| matches!(
			i,
			ChatItem::Error { text, settings: true } if text.contains("No API key")
		)));
		assert!(s.ai.settings_open);
		assert_eq!(s.document().history_labels().len(), 0);
	}

	#[test]
	fn the_context_names_selection_playhead_and_files_root() {
		let (mut s, keys) = state_with_song();
		s.selection.select_track(keys);
		let ctx = s.ai_context();
		assert!(ctx.selected_tracks[0].contains("\"Keys\""));
		assert!(ctx.selected_tracks[0].contains("Synth"));
		assert_eq!(ctx.project_path, None);
		assert_eq!(ctx.files_root, s.ai.session_dir().display().to_string());
		let actions = s.ai_quick_actions();
		assert_eq!(actions[0].0, "Bassline for Keys");
		assert!(actions.iter().any(|(l, _)| l == "Mix the project"));
	}
}

/// Factory samples rendered once into a temp folder for this test binary.
pub(crate) fn factory_samples() -> std::path::PathBuf {
	static DIR: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
	DIR.get_or_init(|| {
		let dir = std::env::temp_dir().join(format!("cadenza-app-factory-{}", std::process::id()));
		// SAFETY: set once (OnceLock) before the samples are first needed.
		unsafe { std::env::set_var(cadenza_engine::factory::FACTORY_SAMPLES_ENV, &dir) };
		cadenza_engine::factory::ensure_factory_samples().unwrap()
	})
	.clone()
}

fn drum_sampler_track(s: &mut AppState) -> DeviceRef {
	factory_samples();
	let items = s.browser_items().to_vec();
	let drums = items.iter().find(|i| i.id == "drum_sampler").unwrap();
	s.selection.select_master();
	assert!(s.use_device(drums, None));
	let t = s.project().tracks.last().unwrap();
	DeviceRef {
		track: Some(t.id),
		device: t.instrument.as_ref().unwrap().id,
	}
}

#[test]
fn drum_pad_gestures_and_drops_are_single_undo_steps() {
	let (mut s, _) = state_with_song();
	let r = drum_sampler_track(&mut s);
	let kit = s.device(r).unwrap().zones.clone();
	assert!(
		kit.len() >= 12,
		"a new drum sampler starts with the Synth Kit"
	);
	assert!(s.open_instrument_editor(r));
	assert!(s.view.instrument_editor && s.view.panels.visible(PanelKind::PianoRoll));

	// A knob gesture on the snare pad: one undoable command.
	let cmd = samples::edit_pad(s.project(), r, 38, |z| z.tune = -3.0);
	assert!(s.apply_sample_edit(cmd));
	assert_eq!(s.undo_label(), Some("Edit sample zones"));
	let snare = &s.device(r).unwrap().zones;
	assert!(
		snare
			.iter()
			.filter(|z| z.key_low == 38)
			.all(|z| z.tune == -3.0)
	);
	// Sending the pad to an extra output routes it in the same step.
	let cmd = samples::edit_pad(s.project(), r, 38, |z| z.output = 4);
	assert!(s.apply_sample_edit(cmd));
	assert_eq!(s.undo_label(), Some("Edit drum pad"));
	assert_eq!(s.device(r).unwrap().aux_outputs[0].port, 4);
	s.undo();
	assert!(s.device(r).unwrap().aux_outputs.is_empty());

	// Dropping a file on the cowbell pad loads it there (the shared load_sample tool).
	let clap = factory_samples().join("Clap.wav");
	let target = DropTarget::Pad {
		device: r,
		note: 56,
	};
	let action = s.drop_action(&dnd::Payload::File(clap.clone()), target, false);
	assert_eq!(
		action.describe(s.project()),
		"Load Clap.wav into pad Cowbell"
	);
	assert!(s.drop_payload(&dnd::Payload::File(clap.clone()), target, false));
	let zones = &s.device(r).unwrap().zones;
	let cowbell: Vec<_> = zones.iter().filter(|z| z.key_low == 56).collect();
	assert_eq!(cowbell.len(), 1);
	assert_eq!(cowbell[0].sample.path, clap);
	assert_eq!(s.view.instrument.pad, 56);
	s.undo();
	assert!(!s.device(r).unwrap().zones.iter().any(|z| z.key_low == 56));
	// MIDI files are not samples.
	assert!(
		!s.drop_action(&dnd::Payload::File("/x/beat.mid".into()), target, false)
			.accepted()
	);
	// Removing the device closes the editor.
	s.remove_device(r.track, r.device);
	assert!(!s.view.instrument_editor && s.view.instrument.device.is_none());
}

#[test]
fn audio_dropped_with_alt_below_the_tracks_becomes_a_sampler() {
	let (mut s, _) = state_with_song();
	let pad = factory_samples().join("Sine Pad C3.wav");
	let payload = dnd::Payload::File(pad.clone());
	let below = DropTarget::BelowSampler { at: Tick::ZERO };
	assert_eq!(
		s.drop_action(&payload, below, false).describe(s.project()),
		"New sampler track with Sine Pad C3.wav"
	);
	assert!(s.drop_payload(&payload, below, false));
	assert_eq!(s.undo_label(), Some("Add track 'Sine Pad C3'"));
	let t = s.project().tracks.last().unwrap().clone();
	let d = t.instrument.as_ref().unwrap();
	assert_eq!(d.builtin_id(), Some("sampler"));
	assert_eq!(d.zones.len(), 1);
	assert_eq!(
		(d.zones[0].key_low, d.zones[0].key_high, d.zones[0].root_key),
		(0, 127, 48)
	);
	let r = DeviceRef {
		track: Some(t.id),
		device: d.id,
	};
	assert_eq!(s.view.instrument.device, Some(r));
	assert!(s.view.instrument_editor);

	// Zone gestures: markers, then replacing the zone's sample by a drop.
	let cmd = samples::edit_zone(s.project(), r, 0, |z| {
		samples::Marker::LoopEnd.set(z, 2.0, 3.0);
		z.loop_mode = cadenza_model::LoopMode::Forward;
	});
	assert!(s.apply_sample_edit(cmd));
	assert_eq!(s.device(r).unwrap().zones[0].loop_end, Some(2.0));
	let kick = factory_samples().join("Kick.wav");
	assert!(s.drop_payload(
		&dnd::Payload::File(kick.clone()),
		DropTarget::Zone {
			device: r,
			zone: Some(0)
		},
		false
	));
	let z = &s.device(r).unwrap().zones[0];
	assert_eq!(z.sample.path, kick);
	assert_eq!(
		z.loop_mode,
		cadenza_model::LoopMode::Forward,
		"mapping kept"
	);
	// Opening a clip in the piano roll takes the dock back.
	s.close_instrument_editor();
	assert!(!s.view.instrument_editor);
	// Plain drops below the tracks still import audio.
	assert!(matches!(
		s.drop_action(&payload, DropTarget::Below { at: Tick::ZERO }, false),
		dnd::DropAction::ImportAudio { .. }
	));
}

/// Polls `done` with frame ticks until it holds (background jobs).
fn wait_until(s: &mut AppState, done: impl Fn(&AppState) -> bool) {
	let deadline = Instant::now() + Duration::from_secs(30);
	while !done(s) {
		assert!(Instant::now() < deadline, "timed out");
		s.tick(Instant::now());
		std::thread::sleep(Duration::from_millis(5));
	}
}

fn saved_song(dir: &Path) -> (AppState, PathBuf) {
	let (mut s, _) = state_with_song();
	s.enable_safety(dir.join("state"));
	let file = dir.join("Song").join("Song.cadenza.json");
	assert!(s.save_as(file.clone()));
	(s, file)
}

#[test]
fn autosave_runs_off_thread_after_the_interval_and_a_crash_offers_recovery() {
	let tmp = tempfile::tempdir().unwrap();
	let (mut s, file) = saved_song(tmp.path());
	assert_eq!(
		s.recent.first(),
		Some(&file),
		"saved projects become recent"
	);
	let lock = file.parent().unwrap().join(".cadenza/session.lock");
	assert!(lock.exists(), "session marker");

	let cmd = edits::add_track(
		s.project(),
		edits::new_track(s.project(), TrackKind::Bus),
		None,
	);
	assert!(s.apply(cmd));
	// File times can be coarse (a few ms): keep the autosave strictly newer than the save.
	std::thread::sleep(Duration::from_millis(30));
	let t0 = Instant::now();
	s.tick_autosave(t0);
	assert!(s.files_jobs.autosave.is_none(), "not due yet");
	s.tick_autosave(t0 + Duration::from_secs(119));
	assert!(s.files_jobs.autosave.is_none());
	s.tick_autosave(t0 + Duration::from_secs(121));
	let written = s.wait_autosave().unwrap().unwrap();
	assert!(written.starts_with(file.parent().unwrap().join(".cadenza/autosave")));
	s.tick_autosave(t0 + Duration::from_secs(600));
	assert!(s.files_jobs.autosave.is_none(), "nothing changed since");

	// "Crash": the state goes away without closing the project.
	drop(s);
	let mut again = AppState::headless();
	again.enable_safety(tmp.path().join("state"));
	assert!(again.open(&file));
	let r = again.recovery().cloned().expect("recovery offered");
	assert_eq!(r.reason, safety::RecoveryReason::Crashed);
	assert_eq!(again.project().tracks.len(), 1, "the saved version is open");
	assert!(again.recover());
	assert_eq!(
		again.project().tracks.len(),
		2,
		"the autosaved edit is back"
	);
	assert_eq!(again.path(), Some(file.as_path()));
	again.tick(Instant::now());
	assert!(again.dirty, "recovered work is unsaved");
	assert!(again.save());
	assert!(safety::autosaves(&file.parent().unwrap().join(".cadenza/autosave")).is_empty());
	again.shutdown();
	assert!(!lock.exists(), "a normal close removes the marker");

	// Nothing newer than the save: no prompt.
	let mut third = AppState::headless();
	third.enable_safety(tmp.path().join("state"));
	assert!(third.open(&file));
	assert!(third.recovery().is_none());
}

#[test]
fn discarding_changes_drops_their_autosaves() {
	let tmp = tempfile::tempdir().unwrap();
	let (mut s, file) = saved_song(tmp.path());
	let cmd = edits::add_track(
		s.project(),
		edits::new_track(s.project(), TrackKind::Bus),
		None,
	);
	assert!(s.apply(cmd));
	let t0 = Instant::now();
	s.tick_autosave(t0);
	s.tick_autosave(t0 + Duration::from_secs(300));
	s.wait_autosave().unwrap().unwrap();
	let dir = file.parent().unwrap().join(".cadenza/autosave");
	assert_eq!(safety::autosaves(&dir).len(), 1);
	s.discard_changes();
	assert!(safety::autosaves(&dir).is_empty());
	assert!(!s.dirty);
}

#[test]
fn export_dialog_job_reports_progress_and_cancels() {
	let tmp = tempfile::tempdir().unwrap();
	let (mut s, file) = saved_song(tmp.path());
	s.open_export_dialog();
	assert!(s.export.open);
	assert_eq!(
		s.export.folder.as_deref(),
		Some(file.parent().unwrap().join("Renders").as_path())
	);
	s.export.settings.stems = export::StemMode::Tracks;
	s.export.settings.format = export::FormatChoice::Flac24;
	assert!(s.start_export());
	assert!(s.export.running.is_some());
	wait_until(&mut s, |s| s.export.running.is_none());
	let report = s.export.result.clone().unwrap().unwrap();
	assert_eq!(report.files.len(), 2, "mix + one stem");
	assert!(report.files.iter().all(|f| f.path.exists()));
	assert!(report.files[0].path.ends_with("Renders/Song.flac"));

	// Cancel: nothing is left behind.
	let out = tmp.path().join("cancelled");
	s.export.folder = Some(out.clone());
	assert!(s.start_export());
	s.export.running.as_ref().unwrap().cancel();
	wait_until(&mut s, |s| s.export.running.is_none());
	let err = s.export.result.clone().unwrap().unwrap_err();
	assert!(err.contains("cancelled"), "{err}");
	assert!(std::fs::read_dir(&out).map_or(0, |d| d.count()) == 0);

	// Invalid settings never start a job.
	s.export.settings.range = export::RangeKind::Loop;
	assert!(!s.start_export());
	assert!(
		s.export
			.result
			.clone()
			.unwrap()
			.unwrap_err()
			.contains("loop")
	);
}

#[test]
fn collect_and_relink_from_the_app_are_single_undo_steps() {
	let tmp = tempfile::tempdir().unwrap();
	let (mut s, file) = saved_song(tmp.path());
	let ext = tmp.path().join("ext").join("take.wav");
	std::fs::create_dir_all(ext.parent().unwrap()).unwrap();
	let buf = cadenza_audio_io::AudioBuffer::from_planar(48_000, vec![vec![0.1; 4800]]).unwrap();
	cadenza_audio_io::export(
		&buf,
		&ext,
		&cadenza_audio_io::ExportOptions::wav(cadenza_audio_io::WavSampleFormat::Int16),
	)
	.unwrap();
	s.import_audio(&ext);
	assert_eq!(s.project().file_refs()[0].path, ext);

	s.collect_files();
	wait_until(&mut s, |s| s.files_jobs.collect.is_none());
	let collected = file.parent().unwrap().join("Audio/take.wav");
	assert_eq!(s.project().file_refs()[0].path, collected);
	assert_eq!(
		s.undo_label(),
		Some(cadenza_mcp::tools::portable::COLLECT_LABEL)
	);

	// Missing after a move: the dialog lists it, a folder search relinks it.
	s.undo();
	std::fs::rename(tmp.path().join("ext"), tmp.path().join("moved")).unwrap();
	s.open_relink();
	assert_eq!(s.relink.as_ref().unwrap().missing.len(), 1);
	s.search_missing(tmp.path().join("moved"));
	wait_until(&mut s, |s| s.files_jobs.relink.is_none());
	assert_eq!(
		s.project().file_refs()[0].path,
		tmp.path().join("moved/take.wav")
	);
	let dialog = s.relink.as_ref().unwrap();
	assert!(dialog.missing.is_empty());
	assert_eq!(dialog.last.as_ref().map(|l| l.1), Some(1));
}

#[test]
fn save_as_elsewhere_offers_to_collect_outside_files() {
	let tmp = tempfile::tempdir().unwrap();
	let (mut s, _) = saved_song(tmp.path());
	assert!(!s.collect_prompt, "no external files");
	let wav = tmp.path().join("loose.wav");
	let buf = cadenza_audio_io::AudioBuffer::from_planar(48_000, vec![vec![0.1; 480]]).unwrap();
	cadenza_audio_io::export(
		&buf,
		&wav,
		&cadenza_audio_io::ExportOptions::wav(cadenza_audio_io::WavSampleFormat::Int16),
	)
	.unwrap();
	s.import_audio(&wav);
	assert!(s.save_as(tmp.path().join("Copy").join("Copy.cadenza.json")));
	assert!(s.collect_prompt);
}

#[test]
fn moving_controls_while_playing_writes_automation_in_one_step() {
	use autowrite::WriteMode;
	let (mut s, keys) = state_with_song();
	let synth = s
		.project()
		.track(keys)
		.unwrap()
		.instrument
		.clone()
		.unwrap()
		.id;
	// A virtual (silent) player drives the clock.
	let (mut player, mut dev) = cadenza_engine::Player::virtual_device(48_000, 0);
	player.load(s.project()).unwrap();
	s.clock = Some(player.clock());
	let mut out = vec![0.0f32; 512];
	let mut run = |dev: &mut cadenza_engine::VirtualDevice, blocks: usize| {
		for _ in 0..blocks {
			dev.process(&mut out, &[]);
		}
	};
	// Not playing: a fader move is an ordinary edit.
	s.set_write_mode(WriteMode::Touch);
	s.preview_volume(Some(keys), -2.0);
	s.commit_volume(Some(keys), -2.0);
	assert_eq!(s.undo_label(), Some("Set track volume to -2.0 dB"));
	let depth = s.document().history_labels().len();

	player.play().unwrap();
	run(&mut dev, 20);
	assert!(s.is_playing());
	for i in 0..10 {
		s.preview_volume(Some(keys), -2.0 - i as f32);
		s.preview_param(synth, "filter_cutoff", 500.0 + i as f32 * 100.0);
		run(&mut dev, 10);
		let (playing, pos) = (s.is_playing(), s.position());
		s.tick_autowrite(playing, pos);
	}
	s.commit_volume(Some(keys), -11.0);
	s.commit_param(Some(keys), synth, "filter_cutoff", 1400.0);
	assert_eq!(
		s.document().history_labels().len(),
		depth,
		"nothing is committed while the pass runs"
	);
	player.stop().unwrap();
	run(&mut dev, 1);
	let (playing, pos) = (s.is_playing(), s.position());
	s.tick_autowrite(playing, pos);
	assert_eq!(s.undo_label(), Some("Write automation"));
	assert_eq!(s.document().history_labels().len(), depth + 1, "one step");
	let lanes = &s.project().automation;
	assert_eq!(lanes.len(), 2, "volume and cutoff lanes");
	let vol = lanes
		.iter()
		.find(|l| l.address == ParamAddress::track_volume(keys))
		.unwrap();
	assert_eq!(vol.points.last().unwrap().value, -11.0);
	assert!(vol.points.len() >= 2);
	assert_eq!(
		s.project().track(keys).unwrap().channel.volume_db,
		-2.0,
		"the static value is untouched"
	);
	s.undo();
	assert!(s.project().automation.is_empty());
}

fn wait_bounce(s: &mut AppState) {
	let deadline = Instant::now() + Duration::from_secs(30);
	while s.bounce_busy() {
		assert!(Instant::now() < deadline, "render timed out");
		std::thread::sleep(Duration::from_millis(10));
		s.tick(Instant::now());
	}
}

#[test]
fn freeze_bounce_and_consolidate_are_single_undo_steps() {
	let tmp = tempfile::tempdir().unwrap();
	let (mut s, keys) = state_with_song();
	s.safety.state_dir = tmp.path().to_path_buf();
	let depth = s.document().history_labels().len();

	s.freeze_track(keys);
	wait_bounce(&mut s);
	assert_eq!(s.undo_label(), Some("Freeze track"));
	let file = s
		.project()
		.track(keys)
		.unwrap()
		.freeze
		.clone()
		.unwrap()
		.file;
	assert!(file.exists() && file.starts_with(tmp.path()));
	// Editing the frozen track's notes makes the render stale; refreezing makes it current.
	assert!(!s.document().freeze_stale(keys));
	let clip = s.project().track(keys).unwrap().clips[0].id;
	assert!(s.apply(Command::AddNotes {
		clip,
		notes: vec![cadenza_model::Note::new(72, Tick(0), Tick(PPQ), 0.7)],
	}));
	assert!(s.document().freeze_stale(keys));
	assert!(s.project().summary().tracks[0].freeze_stale);
	s.freeze_track(keys);
	wait_bounce(&mut s);
	assert!(!s.document().freeze_stale(keys), "refrozen");
	assert_ne!(
		s.project()
			.track(keys)
			.unwrap()
			.freeze
			.as_ref()
			.unwrap()
			.file,
		file
	);
	s.undo();
	s.undo();
	s.unfreeze_track(keys);
	assert!(s.project().track(keys).unwrap().freeze.is_none());

	s.bounce_in_place(keys);
	wait_bounce(&mut s);
	assert_eq!(s.undo_label(), Some("Bounce 'Keys' in place"));
	let p = s.project();
	assert_eq!(p.tracks.len(), 2);
	assert!(p.tracks[0].channel.mute);
	assert_eq!(p.tracks[1].kind, TrackKind::Audio);
	assert_eq!(p.tracks[1].clips.len(), 1);
	assert_eq!(s.document().history_labels().len(), depth + 3);

	// Two audio clips consolidate into one rendered clip.
	let bounced = s.project().tracks[1].id;
	let first = s.project().tracks[1].clips[0].clone();
	let (cmd, _) = edits::split_clip(s.project(), first.id, first.start + Tick(PPQ)).unwrap();
	assert!(s.apply(cmd));
	s.selection.clear_clips();
	for c in s.project().track(bounced).unwrap().clips.clone() {
		s.selection.click_clip(bounced, c.id, true);
	}
	s.consolidate_selection();
	wait_bounce(&mut s);
	assert_eq!(s.undo_label(), Some("Consolidate clips"));
	assert_eq!(s.project().track(bounced).unwrap().clips.len(), 1);
}
