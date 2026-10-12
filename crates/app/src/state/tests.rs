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
	s.session.reset(Document::new(p), None);
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
		cadenza_midi_io::export_project(&state_with_song().0.session.document().project().clone())
			.unwrap(),
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
