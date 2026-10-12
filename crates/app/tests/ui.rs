//! Headless UI tests: the whole window driven through egui_kittest (AccessKit), without
//! an audio device.

use std::path::PathBuf;

use cadenza_app::CadenzaApp;
use cadenza_app::state::AppState;
use cadenza_app::state::notices::Level;
use cadenza_app::state::selection::DeviceRef;
use egui::{Key, Modifiers, vec2};
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
fn shortcuts_toggle_panels_and_transport() {
	let mut h = harness(app());
	h.run_steps(2);
	assert!(h.state().state.view.show_mixer);
	h.key_press(Key::F3);
	h.run_steps(2);
	assert!(!h.state().state.view.show_mixer);
	h.key_press(Key::F5);
	h.run_steps(2);
	assert!(!h.state().state.view.show_browser);

	// No audio device in tests: Space reports it instead of failing silently.
	h.key_press(Key::Space);
	h.run_steps(2);
	assert_eq!(h.state().state.notices.count(Level::Warning), 1);
	assert!(h.get_by_label("Snap").rect().width() > 0.0);
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
	h.run_steps(3);
	h.render()
		.unwrap()
		.save(dir.join("clip_selected.png"))
		.unwrap();
	h.state_mut().state.selection.select_device(DeviceRef {
		track: Some(track),
		device,
	});
	h.run_steps(3);
	h.render()
		.unwrap()
		.save(dir.join("device_selected.png"))
		.unwrap();
	h.state_mut().state.dirty = true;
	h.state_mut()
		.state
		.request(cadenza_app::state::Pending::Quit);
	h.run_steps(3);
	h.render()
		.unwrap()
		.save(dir.join("unsaved_prompt.png"))
		.unwrap();
}
