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

	h.state_mut().state.dirty = true;
	h.state_mut()
		.state
		.request(cadenza_app::state::Pending::Quit);
	shot(&mut h, "unsaved_prompt");
}
