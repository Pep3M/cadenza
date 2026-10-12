//! Headless UI tests: the whole window driven through egui_kittest (AccessKit), without
//! an audio device.

use std::path::PathBuf;

use cadenza_app::CadenzaApp;
use cadenza_app::state::AppState;
use cadenza_app::state::layout::PanelKind;
use cadenza_app::state::notices::Level;
use cadenza_app::state::selection::DeviceRef;
use cadenza_core::{PPQ, Tick};
use egui::{Key, Modifiers, pos2, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;

fn example() -> PathBuf {
	PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/first_light.cadenza.json")
}

fn app() -> CadenzaApp {
	let mut state = AppState::headless();
	assert!(state.open(&example()));
	CadenzaApp::with_state(state)
}

fn harness(app: CadenzaApp) -> Harness<'static, CadenzaApp> {
	Harness::builder()
		.with_size(vec2(1600.0, 1000.0))
		.build_ui_state(
			|ui, app: &mut CadenzaApp| {
				app.logic(ui.ctx());
				app.ui(ui);
			},
			app,
		)
}

#[test]
fn menus_edit_through_commands_and_undo() {
	let mut h = harness(app());
	h.run_steps(2);
	let tracks = h.state().state.project().tracks.len();
	h.get_by_label("Track").click();
	h.run_steps(2);
	h.get_by_label("Add Bus").click();
	h.run_steps(2);
	let s = &h.state().state;
	assert_eq!(s.project().tracks.len(), tracks + 1);
	assert_eq!(s.undo_label(), Some("Add track 'Bus 3'"));
	assert!(s.dirty);

	h.key_press_modifiers(Modifiers::COMMAND, Key::Z);
	h.run_steps(2);
	assert_eq!(h.state().state.project().tracks.len(), tracks);
	h.key_press_modifiers(Modifiers::COMMAND | Modifiers::SHIFT, Key::Z);
	h.run_steps(2);
	assert_eq!(h.state().state.project().tracks.len(), tracks + 1);
}

#[test]
fn function_keys_toggle_panels_help_and_maximize() {
	let mut h = harness(app());
	h.run_steps(2);
	let visible =
		|h: &Harness<'_, CadenzaApp>, p: PanelKind| h.state().state.view.panels.visible(p);
	assert!(visible(&h, PanelKind::Mixer));
	h.key_press(Key::F9);
	h.run_steps(2);
	assert!(!visible(&h, PanelKind::Mixer));
	h.key_press(Key::F6);
	h.key_press(Key::F8);
	h.run_steps(2);
	assert!(!visible(&h, PanelKind::Browser) && !visible(&h, PanelKind::Inspector));
	h.key_press_modifiers(Modifiers::ALT, Key::F8);
	h.run_steps(2);
	assert!(
		visible(&h, PanelKind::Browser),
		"Alt+F8 (FL) also toggles the browser"
	);

	// F1: the shortcut overlay lists the panel keys.
	h.key_press(Key::F1);
	h.run_steps(2);
	assert!(h.state().state.view.help);
	assert!(h.query_by_label("Keyboard shortcuts").is_some());
	assert!(h.query_by_label("Piano roll").is_some());
	h.key_press(Key::Escape);
	h.run_steps(2);
	assert!(!h.state().state.view.help);

	// F11 maximizes the focused panel (the last one shown or clicked); again restores.
	h.key_press(Key::F11);
	h.run_steps(2);
	assert_eq!(
		h.state().state.view.panels.maximized,
		Some(PanelKind::Browser)
	);
	h.key_press(Key::F11);
	h.state_mut().state.view.panels.focus = Some(PanelKind::Arrange);
	h.key_press(Key::F11);
	h.run_steps(2);
	assert_eq!(
		h.state().state.view.panels.maximized,
		Some(PanelKind::Arrange)
	);
	assert!(!visible(&h, PanelKind::Browser));
	h.key_press(Key::F11);
	h.run_steps(2);
	assert!(visible(&h, PanelKind::Browser));
	// F12 closes every tool panel.
	h.key_press(Key::F12);
	h.run_steps(2);
	assert!(!visible(&h, PanelKind::Browser) && visible(&h, PanelKind::Arrange));

	// No audio device in tests: Space reports it instead of failing silently.
	h.key_press(Key::Space);
	h.run_steps(2);
	assert_eq!(h.state().state.notices.count(Level::Warning), 1);
	assert!(h.get_by_label("Snap").rect().width() > 0.0);
}

#[test]
fn piano_roll_draws_transposes_and_undoes_notes() {
	let mut a = app();
	let clip = {
		let p = a.state.project();
		let t = p.tracks.iter().find(|t| t.name == "Keys").unwrap();
		t.clips.iter().find(|c| c.is_midi()).unwrap().id
	};
	assert!(a.state.open_piano_roll(clip));
	let mut h = harness(a);
	h.run_steps(3);
	assert!(h.state().state.view.panels.visible(PanelKind::PianoRoll));
	assert!(
		!h.state().state.view.panels.visible(PanelKind::Mixer),
		"the piano roll takes the mixer's dock"
	);
	let grid = h.state().piano_grid().expect("grid drawn");
	let notes = |h: &Harness<'_, CadenzaApp>| -> Vec<cadenza_model::Note> {
		h.state()
			.state
			.project()
			.find_clip(clip)
			.unwrap()
			.1
			.notes()
			.to_vec()
	};
	let before = notes(&h);
	// An empty row near the top of the visible grid, a beat into the clip.
	let (x, y, pitch) = {
		let s = &h.state().state;
		let v = &s.view.piano;
		let c = s.project().find_clip(clip).unwrap().1;
		let pitch = (1..20)
			.map(|i| v.y_to_pitch(grid.height() * i as f32 / 20.0))
			.find(|p| c.notes().iter().all(|n| n.pitch != *p))
			.expect("an empty visible row");
		let tick = c.start + Tick(PPQ);
		(
			grid.left() + v.timeline.tick_to_x(tick) + 2.0,
			grid.top() + v.pitch_to_y(pitch) + v.row_height * 0.5,
			pitch,
		)
	};
	h.hover_at(pos2(x, y));
	h.run_steps(1);
	h.drag_at(pos2(x, y));
	h.run_steps(1);
	assert_eq!(
		h.state().state.previewing.len(),
		1,
		"the drawn note is auditioned"
	);
	h.hover_at(pos2(x + 120.0, y));
	h.run_steps(1);
	h.drop_at(pos2(x + 120.0, y));
	h.run_steps(2);
	let after = notes(&h);
	assert_eq!(after.len(), before.len() + 1);
	let added = after
		.iter()
		.find(|n| !before.iter().any(|b| b.id == n.id))
		.unwrap()
		.clone();
	assert_eq!(added.pitch, pitch);
	assert!(added.length > Tick(PPQ / 4), "dragging set the length");
	assert!(
		h.state().state.previewing.is_empty(),
		"released on mouse up"
	);
	assert_eq!(h.state().state.undo_label(), Some("Add note"));

	// The click focused the piano roll: arrows transpose the selected (new) note.
	h.key_press(Key::ArrowUp);
	h.run_steps(2);
	let moved = notes(&h).into_iter().find(|n| n.id == added.id).unwrap();
	assert_eq!(moved.pitch, pitch + 1);
	h.key_press_modifiers(Modifiers::SHIFT, Key::ArrowDown);
	h.run_steps(2);
	let moved = notes(&h).into_iter().find(|n| n.id == added.id).unwrap();
	assert_eq!(moved.pitch, pitch - 11);
	// Del removes it (the arrange clip stays), undo brings it back.
	h.key_press(Key::Delete);
	h.run_steps(2);
	assert_eq!(notes(&h).len(), before.len());
	h.key_press_modifiers(Modifiers::COMMAND, Key::Z);
	h.run_steps(2);
	assert_eq!(notes(&h).len(), before.len() + 1);

	// F7 hides it again and gives the dock back to the mixer.
	h.key_press(Key::F7);
	h.run_steps(2);
	assert!(!h.state().state.view.panels.visible(PanelKind::PianoRoll));
	assert!(h.state().state.view.panels.visible(PanelKind::Mixer));
}

#[test]
fn clip_shortcuts_duplicate_and_delete_the_selection() {
	let mut h = harness(app());
	h.run_steps(2);
	let (track, clip) = {
		let p = h.state().state.project();
		let t = p.tracks.iter().find(|t| !t.clips.is_empty()).unwrap();
		(t.id, t.clips[0].id)
	};
	let count = |h: &Harness<'_, CadenzaApp>| -> usize {
		h.state()
			.state
			.project()
			.tracks
			.iter()
			.map(|t| t.clips.len())
			.sum()
	};
	let before = count(&h);
	h.state_mut().state.selection.click_clip(track, clip, false);
	h.key_press_modifiers(Modifiers::COMMAND, Key::D);
	h.run_steps(2);
	assert_eq!(count(&h), before + 1);
	assert_eq!(
		h.state().state.selection.clips.len(),
		1,
		"the copy is selected"
	);
	h.key_press(Key::Delete);
	h.run_steps(2);
	assert_eq!(count(&h), before);
}

#[test]
fn inspector_shows_the_selected_device() {
	let mut a = app();
	let (track, device) = {
		let p = a.state.project();
		let t = p.tracks.iter().find(|t| t.name == "Keys").unwrap();
		(t.id, t.instrument.as_ref().unwrap().id)
	};
	a.state.selection.select_device(DeviceRef {
		track: Some(track),
		device,
	});
	let mut h = harness(a);
	h.run_steps(2);
	assert!(
		h.query_by_label("Bypass").is_none(),
		"instruments are not bypassed"
	);
	h.get_by_label("Remove").click();
	h.run_steps(2);
	let t = h.state().state.project().track(track).unwrap().clone();
	assert!(t.instrument.is_none());
	assert_eq!(h.state().state.undo_label(), Some("Remove instrument"));

	let insert = t.channel.inserts[0].id;
	h.state_mut().state.selection.select_device(DeviceRef {
		track: Some(track),
		device: insert,
	});
	h.run_steps(2);
	h.get_by_label("Bypass").click();
	h.run_steps(2);
	let t = h.state().state.project().track(track).unwrap();
	assert!(t.channel.inserts[0].bypass);
	assert_eq!(h.state().state.undo_label(), Some("Bypass device"));
}

/// A 2 s decaying stereo tone written to a temporary WAV.
fn tone_wav(dir: &std::path::Path) -> PathBuf {
	let sr = 44_100usize;
	let l: Vec<f32> = (0..sr * 2)
		.map(|i| {
			let t = i as f32 / sr as f32;
			0.8 * (-1.5 * t).exp() * (t * 220.0 * std::f32::consts::TAU).sin()
		})
		.collect();
	let r: Vec<f32> = l
		.iter()
		.enumerate()
		.map(|(i, x)| x * (0.5 + 0.5 * ((i / 4000) % 2) as f32))
		.collect();
	let buf = cadenza_audio_io::AudioBuffer::from_planar(sr as u32, vec![l, r]).unwrap();
	let path = dir.join("tone.wav");
	cadenza_audio_io::export(
		&buf,
		&path,
		&cadenza_audio_io::ExportOptions::wav(cadenza_audio_io::WavSampleFormat::Float32),
	)
	.unwrap();
	path
}

#[test]
fn audio_editor_trims_fades_and_processes_through_commands() {
	let dir = tempfile::tempdir().unwrap();
	let wav = tone_wav(dir.path());
	let mut a = app();
	// Saved in the temp dir, so processed copies land in "song audio" there.
	assert!(a.state.save_as(dir.path().join("song.cadenza.json")));
	a.state.import_audio_at(&wav, None, Some(Tick::ZERO));
	let clip = *a
		.state
		.selection
		.clips
		.iter()
		.next()
		.expect("imported clip selected");
	assert!(a.state.open_editor(clip));
	assert!(a.state.view.audio_editor);
	let mut h = harness(a);
	h.run_steps(4);
	// Wait for the peaks worker.
	for _ in 0..200 {
		h.run_steps(1);
		if matches!(
			h.state_mut().state.peaks.get(&wav),
			cadenza_app::jobs::Peaks::Ready(_)
		) {
			break;
		}
		std::thread::sleep(std::time::Duration::from_millis(10));
	}
	let wave = h.state().audio_wave().expect("audio editor drawn");
	let region = |h: &Harness<'_, CadenzaApp>| {
		cadenza_app::state::audio_edit::region(h.state().state.project(), clip).unwrap()
	};
	let x_of =
		|h: &Harness<'_, CadenzaApp>, s: f64| wave.left() + h.state().state.view.audio.x_of(s);
	assert!((region(&h).duration - 2.0).abs() < 0.01);

	// Drag a selection over 0.5..1.0 s, then Trim: one undo step, content stays in place.
	let y = wave.center().y + 20.0;
	h.drag_at(pos2(x_of(&h, 0.5), y));
	h.run_steps(1);
	h.hover_at(pos2(x_of(&h, 0.75), y));
	h.run_steps(1);
	h.drop_at(pos2(x_of(&h, 1.0), y));
	h.run_steps(2);
	let (a, b) = h.state().state.view.audio.selection.unwrap();
	assert!((a - 0.5).abs() < 0.02 && (b - 1.0).abs() < 0.02, "{a}..{b}");
	h.get_by_label("Trim").click();
	h.run_steps(3);
	let r = region(&h);
	assert!((r.offset - 0.5).abs() < 0.02 && (r.duration - 0.5).abs() < 0.03);
	assert_eq!(h.state().state.undo_label(), Some("Trim clip to selection"));

	// Fade-in handle drag sets the fade (one SetClipFades).
	let start = pos2(x_of(&h, r.offset), wave.top() + 6.0);
	h.drag_at(start);
	h.run_steps(1);
	h.hover_at(start + vec2(20.0, 0.0));
	h.run_steps(1);
	h.drop_at(pos2(x_of(&h, r.offset + 0.1), wave.top() + 6.0));
	h.run_steps(2);
	let fade = region(&h).fade_in.expect("fade in set").seconds;
	assert!((fade - 0.1).abs() < 0.02, "{fade}");
	assert_eq!(h.state().state.undo_label(), Some("Set clip fades"));

	// Reverse renders a processed copy next to the (unsaved: cache) project and swaps it.
	h.state_mut().state.view.audio.selection = None;
	h.get_by_label("Reverse").click();
	for _ in 0..300 {
		h.run_steps(1);
		if h.state().state.jobs.process.is_none() {
			break;
		}
		std::thread::sleep(std::time::Duration::from_millis(10));
	}
	let swapped = region(&h).file;
	assert_ne!(swapped, wav);
	assert!(swapped.exists());
	assert!(swapped.starts_with(dir.path().join("song audio")));
	assert_eq!(h.state().state.undo_label(), Some("Reverse"));
	h.key_press_modifiers(Modifiers::COMMAND, Key::Z);
	h.run_steps(2);
	assert_eq!(region(&h).file, wav, "undo swaps the original back");
}

#[test]
fn automation_lanes_add_move_and_delete_points() {
	let mut a = app();
	let keys = a
		.state
		.project()
		.tracks
		.iter()
		.find(|t| t.name == "Keys")
		.unwrap()
		.id;
	a.state.view.panels.set(PanelKind::Mixer, false);
	a.state.selection.select_track(keys);
	let mut h = harness(a);
	h.run_steps(2);
	h.state_mut().state.view.panels.focus = Some(PanelKind::Arrange);
	h.key_press(Key::A);
	h.run_steps(3);
	let vol = cadenza_core::ParamAddress::track_volume(keys);
	let lane = h.state().lane_rect(&vol).expect("A shows the volume lane");
	assert!(h.state().state.project().automation_lane(&vol).is_none());

	// Click: a point at the snapped beat, one undo step.
	let x_of = |h: &Harness<'_, CadenzaApp>, beat: f64| {
		lane.left() + h.state().state.view.timeline.beat_to_x(beat)
	};
	let at = pos2(x_of(&h, 4.0) + 1.0, lane.center().y);
	h.hover_at(at);
	h.run_steps(1);
	h.drag_at(at);
	h.run_steps(1);
	h.drop_at(at);
	h.run_steps(3);
	let points = |h: &Harness<'_, CadenzaApp>| {
		h.state()
			.state
			.project()
			.automation_lane(&vol)
			.map(|l| l.points.clone())
			.unwrap_or_default()
	};
	assert_eq!(points(&h).len(), 1);
	assert_eq!(points(&h)[0].tick, Tick(PPQ * 4));
	assert_eq!(h.state().state.undo_label(), Some("Write automation"));

	// Drag it two beats later and up: one more undo step.
	let from = pos2(
		x_of(&h, 4.0),
		lane.bottom()
			- 5.0 - h
			.state()
			.state
			.view
			.automation
			.selected
			.first()
			.map(|_| (lane.height() - 10.0) * 0.5)
			.unwrap(),
	);
	let steps = h.state().state.document().history_labels().len();
	h.drag_at(from);
	h.run_steps(1);
	h.hover_at(pos2(x_of(&h, 6.0) + 1.0, lane.top() + 8.0));
	h.run_steps(2);
	h.drop_at(pos2(x_of(&h, 6.0) + 1.0, lane.top() + 8.0));
	h.run_steps(3);
	let p = points(&h);
	assert_eq!(p.len(), 1);
	assert_eq!(p[0].tick, Tick(PPQ * 6));
	assert!(p[0].value > -1.0, "moved up: {}", p[0].value);
	assert_eq!(h.state().state.document().history_labels().len(), steps + 1);

	// Del removes the selected point (and the lane with it); undo brings it back.
	h.key_press(Key::Delete);
	h.run_steps(2);
	assert!(points(&h).is_empty());
	h.key_press_modifiers(Modifiers::COMMAND, Key::Z);
	h.run_steps(2);
	assert_eq!(points(&h).len(), 1);
	// A again hides the lanes.
	h.key_press(Key::A);
	h.run_steps(2);
	assert!(h.state().lane_rect(&vol).is_none());
}

#[test]
fn rubber_band_selects_clips_and_left_trim_keeps_notes_in_place() {
	let mut a = app();
	a.state.view.panels.set(PanelKind::Mixer, false);
	let p = a.state.project();
	let index = |name: &str| p.tracks.iter().position(|t| t.name == name).unwrap();
	let (drums, bass, keys) = (index("Drums"), index("Bass"), index("Keys"));
	let expected: Vec<_> = [drums, bass]
		.iter()
		.flat_map(|&i| p.tracks[i].clips.iter())
		.filter(|c| c.start < Tick(PPQ * 30) && c.end() > Tick(PPQ))
		.map(|c| c.id)
		.collect();
	let mut h = harness(a);
	h.run_steps(3);
	let tl = h.state().state.view.timeline;
	let top = h.state().track_rect(drums).unwrap();
	let bottom = h.state().track_rect(bass).unwrap();
	// From empty space in the Drums lane (before its clips) to the Bass row.
	let from = pos2(top.left() + tl.beat_to_x(1.0), top.center().y);
	let to = pos2(top.left() + tl.beat_to_x(30.0), bottom.center().y);
	h.drag_at(from);
	h.run_steps(1);
	h.hover_at(to);
	h.run_steps(1);
	h.drop_at(to);
	h.run_steps(2);
	let selected: std::collections::HashSet<_> =
		h.state().state.selection.clips.iter().copied().collect();
	assert!(!expected.is_empty());
	assert!(
		expected.iter().all(|c| selected.contains(c)),
		"{selected:?}"
	);
	assert_eq!(selected.len(), expected.len());

	// Drag the left edge of the second Keys clip one bar later.
	let (clip, start, abs_notes) = {
		let t = &h.state().state.project().tracks[keys];
		let c = &t.clips[1];
		let abs: Vec<_> = c.notes().iter().map(|n| c.start + n.start).collect();
		(c.id, c.start, abs)
	};
	let row = h.state().track_rect(keys).unwrap();
	let edge = pos2(row.left() + tl.tick_to_x(start) + 2.0, row.center().y);
	let target = pos2(edge.x + tl.ticks_to_px(Tick(PPQ * 4)), edge.y);
	h.drag_at(edge);
	h.run_steps(1);
	h.hover_at(target);
	h.run_steps(1);
	h.drop_at(target);
	h.run_steps(2);
	let (_, c) = h.state().state.project().find_clip(clip).unwrap();
	assert_eq!(c.start, start + Tick(PPQ * 4));
	assert_eq!(h.state().state.undo_label(), Some("Trim clip start"));
	for n in c.notes() {
		assert!(abs_notes.contains(&(c.start + n.start)) || n.start == Tick::ZERO);
	}
}

#[test]
fn empty_project_starts_and_shows_master_automation() {
	let mut h = harness(CadenzaApp::with_state(AppState::headless()));
	h.run_steps(3);
	h.state_mut().state.selection.select_master();
	h.key_press(Key::A);
	h.run_steps(3);
	let vol = cadenza_app::state::volume_address(None);
	assert!(h.state().lane_rect(&vol).is_some());
}

/// Renders a few app states to PNG files in `$CADENZA_SCREENSHOT_DIR` (needs a GPU):
/// `CADENZA_SCREENSHOT_DIR=/tmp/shots cargo test -p cadenza-app --test ui -- --ignored`.
#[test]
#[ignore]
fn render_screens() {
	let Some(dir) = std::env::var_os("CADENZA_SCREENSHOT_DIR").map(PathBuf::from) else {
		return;
	};
	std::fs::create_dir_all(&dir).unwrap();
	let mut a = app();
	let (track, device, clip) = {
		let p = a.state.project();
		let t = p.tracks.iter().find(|t| t.name == "Keys").unwrap();
		(t.id, t.channel.inserts[0].id, t.clips[1].id)
	};
	a.state.view.bookmarks = vec![example().parent().unwrap().to_path_buf()];
	a.state.selection.click_clip(track, clip, false);
	let mut h = Harness::builder()
		.with_size(vec2(1600.0, 1000.0))
		.wgpu()
		.build_ui_state(
			|ui, app: &mut CadenzaApp| {
				app.logic(ui.ctx());
				app.ui(ui);
			},
			a,
		);
	let shot = |h: &mut Harness<'_, CadenzaApp>, name: &str| {
		h.run_steps(3);
		h.render()
			.unwrap()
			.save(dir.join(format!("{name}.png")))
			.unwrap();
	};
	shot(&mut h, "clip_selected");
	h.state_mut().state.selection.select_device(DeviceRef {
		track: Some(track),
		device,
	});
	shot(&mut h, "device_selected");

	// Piano roll on the Keys clip, with a scale and a selection.
	let midi = {
		let p = h.state().state.project();
		let t = p.track(track).unwrap();
		t.clips.iter().find(|c| c.is_midi()).unwrap().clone()
	};
	{
		let s = &mut h.state_mut().state;
		s.open_piano_roll(midi.id);
		s.view.piano.scale = cadenza_theory::Scale::parse("A minor").ok();
		let ids: Vec<_> = midi.notes().iter().take(3).map(|n| n.id).collect();
		s.view.piano.select(ids, false);
	}
	shot(&mut h, "piano_roll");
	h.state_mut().state.view.panels.focus = Some(PanelKind::PianoRoll);
	h.key_press(Key::F11);
	shot(&mut h, "piano_roll_maximized");
	h.key_press(Key::F11);
	h.key_press(Key::F1);
	shot(&mut h, "help");
	h.key_press(Key::Escape);
	h.key_press(Key::F7);
	shot(&mut h, "arrange_mixer");
	h.get_by_label("View").click();
	shot(&mut h, "view_menu");
	h.get_by_label("View").click();
	h.key_press(Key::Escape);
	h.state_mut().state.view.panels.toggle(PanelKind::Browser);
	shot(&mut h, "browser_hidden");
	h.state_mut().state.view.panels.toggle(PanelKind::Browser);

	// Dragging a device over the empty area below the tracks.
	let item = h
		.state_mut()
		.state
		.browser_items()
		.iter()
		.find(|i| i.id == "electric_piano")
		.unwrap()
		.clone();
	egui::DragAndDrop::set_payload(&h.ctx, cadenza_app::state::dnd::Payload::Device(item));
	h.hover_at(pos2(900.0, 470.0));
	shot(&mut h, "dnd_device");
	egui::DragAndDrop::clear_payload(&h.ctx);
	h.remove_cursor();

	// Automation lanes (volume ramp + filter sweep) under Keys, master volume, tempo lane.
	{
		let s = &mut h.state_mut().state;
		let synth = s
			.project()
			.track(track)
			.unwrap()
			.instrument
			.clone()
			.unwrap();
		let param = cadenza_app::state::automation::device_params(&synth, None)
			.into_iter()
			.find(|(id, _)| id.contains("cutoff"))
			.map(|(id, _)| id)
			.unwrap_or_else(|| "gain".into());
		let vol = cadenza_core::ParamAddress::track_volume(track);
		let cut = cadenza_core::ParamAddress::device(Some(track), synth.id, param);
		let pt = cadenza_model::AutomationPoint::new;
		s.apply(cadenza_model::Command::SetAutomation {
			address: vol.clone(),
			points: vec![
				pt(Tick(0), -24.0),
				pt(Tick(PPQ * 8), -3.0).with_shape(cadenza_model::PointShape::Hold),
				pt(Tick(PPQ * 16), -10.0).with_shape(cadenza_model::PointShape::Smooth),
				pt(Tick(PPQ * 28), 0.0),
			],
		});
		let meta = s.lane_meta(&cut).unwrap();
		s.apply(cadenza_model::Command::SetAutomation {
			address: cut.clone(),
			points: vec![
				pt(Tick(0), meta.from_norm(0.2)).with_shape(cadenza_model::PointShape::Smooth),
				pt(Tick(PPQ * 12), meta.from_norm(0.9)),
				pt(Tick(PPQ * 20), meta.from_norm(0.4)),
			],
		});
		s.apply(cadenza_model::Command::SetTempo {
			tick: Tick(PPQ * 16),
			bpm: 96.0,
		});
		s.view.automation.show(vol.clone());
		s.view.automation.show(cut);
		s.view.automation.selected = vec![(vol, Tick(PPQ * 8))];
		s.view.automation.tempo = true;
		s.automate(cadenza_app::state::volume_address(None));
		s.view.panels.set(PanelKind::PianoRoll, false);
		s.selection.select_track(track);
		s.seek(Tick(PPQ * 20));
		s.view.panels.set(PanelKind::Mixer, false);
		let index = s.project().track_index(track).unwrap();
		s.view.scroll_y =
			cadenza_app::state::rows::Rows::new(s.project(), &s.view).track_top(index) - 20.0;
	}
	shot(&mut h, "automation_lanes");
	h.hover_at(pos2(752.0, 578.0));
	shot(&mut h, "automation_hover");
	h.remove_cursor();

	// Audio editor on an imported tone: fades, gain, a selection; then zoomed to samples.
	let tmp = tempfile::tempdir().unwrap();
	let wav = tone_wav(tmp.path());
	let clip = {
		let s = &mut h.state_mut().state;
		s.view.automation.tempo = false;
		s.import_audio_at(&wav, None, Some(Tick(PPQ * 4)));
		let clip = *s.selection.clips.iter().next().unwrap();
		let cmd = cadenza_app::state::audio_edit::set_fades(
			s.project(),
			clip,
			Some(cadenza_model::ClipFade::new(
				0.15,
				cadenza_model::FadeCurve::EqualPower,
			)),
			Some(cadenza_model::ClipFade::new(
				0.6,
				cadenza_model::FadeCurve::SCurve,
			)),
		);
		s.audio_edit(cmd);
		let cmd = cadenza_app::state::audio_edit::set_region_end(s.project(), clip, 1.7);
		s.audio_edit(cmd);
		s.audio_edit(cadenza_app::state::audio_edit::set_gain(
			s.project(),
			clip,
			-2.5,
		));
		s.open_editor(clip);
		s.view.audio.selection = Some((0.55, 0.9));
		clip
	};
	for _ in 0..300 {
		h.run_steps(1);
		if matches!(
			h.state_mut().state.peaks.get(&wav),
			cadenza_app::jobs::Peaks::Ready(_)
		) {
			break;
		}
		std::thread::sleep(std::time::Duration::from_millis(10));
	}
	shot(&mut h, "audio_editor");
	{
		let s = &mut h.state_mut().state;
		let _ = s.peaks.samples(&wav);
		s.view.audio.px_per_second = 30_000.0;
		s.view.audio.scroll = 0.6;
		s.view.audio.framed = Some(clip);
	}
	for _ in 0..300 {
		h.run_steps(1);
		if matches!(
			h.state_mut().state.peaks.samples(&wav),
			cadenza_app::jobs::Samples::Ready(_)
		) {
			break;
		}
		std::thread::sleep(std::time::Duration::from_millis(10));
	}
	shot(&mut h, "audio_editor_samples");

	// Overlapping audio clips on one track are marked like a crossfade; rubber band.
	{
		let s = &mut h.state_mut().state;
		if let Some((cmd, _)) =
			cadenza_app::state::edits::copy_clips(s.project(), &[clip], Tick(PPQ * 2), None)
		{
			s.apply(cmd);
		}
		s.close_piano_roll();
		s.view.panels.set(PanelKind::Mixer, false);
		s.view.scroll_y = 0.0;
	}
	h.run_steps(2);
	let rows = (
		h.state().track_rect(0).unwrap(),
		h.state().track_rect(2).unwrap(),
	);
	let tl = h.state().state.view.timeline;
	h.drag_at(pos2(rows.0.left() + tl.beat_to_x(2.0), rows.0.center().y));
	h.run_steps(1);
	h.hover_at(pos2(rows.1.left() + tl.beat_to_x(22.0), rows.1.center().y));
	shot(&mut h, "arrange_overlap_rubber_band");
	h.drop_at(pos2(rows.1.left() + tl.beat_to_x(22.0), rows.1.center().y));
	h.run_steps(2);
	h.remove_cursor();

	h.state_mut().state.dirty = true;
	h.state_mut()
		.state
		.request(cadenza_app::state::Pending::Quit);
	shot(&mut h, "unsaved_prompt");

	// A fresh window without any project (the empty document).
	let mut empty = Harness::builder()
		.with_size(vec2(1600.0, 1000.0))
		.wgpu()
		.build_ui_state(
			|ui, app: &mut CadenzaApp| {
				app.logic(ui.ctx());
				app.ui(ui);
			},
			CadenzaApp::with_state(AppState::headless()),
		);
	empty.run_steps(3);
	empty
		.render()
		.unwrap()
		.save(dir.join("empty_project.png"))
		.unwrap();
}
