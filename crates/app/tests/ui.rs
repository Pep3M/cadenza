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
	// FL Studio: the click places a note of the last length and the drag moves it
	// (snapped to the piano roll's default step).
	assert_eq!(added.length, Tick(PPQ / 4), "the last note length");
	assert!(added.start > Tick(PPQ), "dragging moved it later");
	assert_eq!(added.start.0 % (PPQ / 4), 0, "snapped to the step");
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

/// The Keys clip of the example, emptied and refilled with C4, D4, E4 at beats 0, 1, 2
/// (one beat each), open in a focused piano roll.
fn fl_piano() -> (Harness<'static, CadenzaApp>, cadenza_core::ClipId) {
	let mut a = app();
	let clip = {
		let p = a.state.project();
		let t = p.tracks.iter().find(|t| t.name == "Keys").unwrap();
		t.clips.iter().find(|c| c.is_midi()).unwrap().clone()
	};
	let old: Vec<_> = clip.notes().iter().map(|n| n.id).collect();
	assert!(a.state.apply(cadenza_model::Command::RemoveNotes {
		clip: clip.id,
		notes: old,
	}));
	assert!(
		a.state.apply(cadenza_model::Command::AddNotes {
			clip: clip.id,
			notes: [60, 62, 64]
				.iter()
				.enumerate()
				.map(|(i, p)| cadenza_model::Note::new(*p, Tick(PPQ * i as i64), Tick(PPQ), 0.8))
				.collect(),
		})
	);
	assert!(a.state.open_piano_roll(clip.id));
	// Short steps so two clicks fall within the double-click delay.
	let mut h = Harness::builder()
		.with_size(vec2(1600.0, 1000.0))
		.with_step_dt(1.0 / 60.0)
		.build_ui_state(
			|ui, app: &mut CadenzaApp| {
				app.logic(ui.ctx());
				app.ui(ui);
			},
			a,
		);
	h.run_steps(3);
	h.state_mut().state.view.panels.focus = Some(PanelKind::PianoRoll);
	(h, clip.id)
}

fn clip_notes(h: &Harness<'_, CadenzaApp>, clip: cadenza_core::ClipId) -> Vec<cadenza_model::Note> {
	let mut n = h
		.state()
		.state
		.project()
		.find_clip(clip)
		.unwrap()
		.1
		.notes()
		.to_vec();
	n.sort_by_key(|n| (n.start, n.pitch));
	n
}

/// Screen position of beat `beat` (relative to the clip) on `pitch`'s row.
fn note_pos(
	h: &Harness<'_, CadenzaApp>,
	clip: cadenza_core::ClipId,
	beat: f64,
	pitch: u8,
) -> egui::Pos2 {
	let grid = h.state().piano_grid().unwrap();
	let s = &h.state().state;
	let v = &s.view.piano;
	let start = s.project().find_clip(clip).unwrap().1.start;
	pos2(
		grid.left() + v.timeline.tick_to_x(start + Tick::from_beats(beat)),
		grid.top() + v.pitch_to_y(pitch) + v.row_height * 0.5,
	)
}

fn right_button(h: &Harness<'_, CadenzaApp>, pos: egui::Pos2, pressed: bool) {
	h.event(egui::Event::PointerButton {
		pos,
		button: egui::PointerButton::Secondary,
		pressed,
		modifiers: Modifiers::NONE,
	});
}

#[test]
fn alt_wheel_over_notes_changes_their_velocity_in_one_step() {
	let (mut h, clip) = fl_piano();
	let wheel = |h: &mut Harness<'_, CadenzaApp>, pos: egui::Pos2, notches: f32| {
		h.hover_at(pos);
		h.event(egui::Event::ModifiersChanged(Modifiers::ALT));
		h.run_steps(1);
		h.event(egui::Event::MouseWheel {
			unit: egui::MouseWheelUnit::Point,
			delta: vec2(0.0, 40.0 * notches),
			phase: egui::TouchPhase::Move,
			modifiers: Modifiers::ALT,
		});
		h.run_steps(30);
	};
	let velocities = |h: &Harness<'_, CadenzaApp>| -> Vec<f32> {
		clip_notes(h, clip).iter().map(|n| n.velocity).collect()
	};
	let depth = h.state().state.document().history_labels().len();
	let rows = h.state().state.view.piano.row_height;
	let pos = note_pos(&h, clip, 0.25, 60);
	wheel(&mut h, pos, 3.0);
	assert_eq!(
		velocities(&h),
		vec![0.8, 0.8, 0.8],
		"previewed, not committed yet"
	);
	assert_eq!(
		h.state().state.view.piano.row_height,
		rows,
		"no key-height zoom over a note"
	);
	assert!(h.query_by_label_contains("Velocity 86%").is_some());
	h.event(egui::Event::ModifiersChanged(Modifiers::NONE));
	h.run_steps(2);
	let v = velocities(&h);
	assert!(
		(v[0] - 0.86).abs() < 1e-3 && v[1] == 0.8 && v[2] == 0.8,
		"{v:?}"
	);
	assert_eq!(h.state().state.document().history_labels().len(), depth + 1);

	// Over a selected note: the whole selection moves (down), one more step.
	h.key_press_modifiers(Modifiers::COMMAND, Key::A);
	h.run_steps(2);
	let pos = note_pos(&h, clip, 1.25, 62);
	wheel(&mut h, pos, -5.0);
	h.event(egui::Event::ModifiersChanged(Modifiers::NONE));
	h.run_steps(2);
	let v = velocities(&h);
	assert!(
		(v[0] - 0.76).abs() < 1e-3 && (v[1] - 0.7).abs() < 1e-3,
		"{v:?}"
	);
	assert_eq!(h.state().state.document().history_labels().len(), depth + 2);

	// Over empty grid Alt+wheel still sets the key height.
	let pos = note_pos(&h, clip, 3.5, 57);
	wheel(&mut h, pos, -2.0);
	h.event(egui::Event::ModifiersChanged(Modifiers::NONE));
	h.run_steps(2);
	assert_ne!(h.state().state.view.piano.row_height, rows);
	assert_eq!(h.state().state.document().history_labels().len(), depth + 2);
}

#[test]
fn piano_roll_right_button_sweep_deletes_every_note_passed_in_one_step() {
	let (mut h, clip) = fl_piano();
	let undo_depth = h.state().state.document().history_labels().len();
	// Hold the right button on C4 and sweep diagonally over D4 to E4.
	let a = note_pos(&h, clip, 0.5, 60);
	let b = note_pos(&h, clip, 2.5, 64);
	h.hover_at(a);
	h.run_steps(1);
	right_button(&h, a, true);
	h.run_steps(1);
	for i in 1..=8 {
		h.hover_at(a + (b - a) * (i as f32 / 8.0));
		h.run_steps(1);
		if i == 4 {
			assert_eq!(clip_notes(&h, clip).len(), 3, "deleted only on release");
		}
	}
	right_button(&h, b, false);
	h.run_steps(2);
	assert!(
		clip_notes(&h, clip).is_empty(),
		"every note under the sweep"
	);
	assert_eq!(h.state().state.undo_label(), Some("Remove 3 notes"));
	assert_eq!(
		h.state().state.document().history_labels().len(),
		undo_depth + 1,
		"one undo step"
	);
	h.key_press_modifiers(Modifiers::COMMAND, Key::Z);
	h.run_steps(2);
	assert_eq!(clip_notes(&h, clip).len(), 3);

	// A plain right-click deletes just the note under the pointer.
	let d4 = note_pos(&h, clip, 1.5, 62);
	h.hover_at(d4);
	h.run_steps(1);
	right_button(&h, d4, true);
	h.run_steps(1);
	right_button(&h, d4, false);
	h.run_steps(2);
	let left: Vec<u8> = clip_notes(&h, clip).iter().map(|n| n.pitch).collect();
	assert_eq!(left, vec![60, 64]);

	// A right-click on empty space deselects everything (and deletes nothing).
	let ids: Vec<_> = clip_notes(&h, clip).iter().map(|n| n.id).collect();
	h.state_mut().state.view.piano.select(ids, false);
	let empty = note_pos(&h, clip, 3.5, 70);
	h.hover_at(empty);
	h.run_steps(1);
	right_button(&h, empty, true);
	h.run_steps(1);
	right_button(&h, empty, false);
	h.run_steps(2);
	assert!(h.state().state.view.piano.selected.is_empty());
	assert_eq!(clip_notes(&h, clip).len(), 2);
}

#[test]
fn piano_roll_double_click_opens_note_properties() {
	let (mut h, clip) = fl_piano();
	let e4 = note_pos(&h, clip, 2.5, 64);
	h.hover_at(e4);
	h.run_steps(1);
	for _ in 0..2 {
		for pressed in [true, false] {
			h.event(egui::Event::PointerButton {
				pos: e4,
				button: egui::PointerButton::Primary,
				pressed,
				modifiers: Modifiers::NONE,
			});
		}
		h.run_steps(1);
	}
	h.run_steps(2);
	assert!(
		h.query_by_label("Pitch").is_some(),
		"the properties popover"
	);
	assert_eq!(clip_notes(&h, clip).len(), 3, "a double-click adds nothing");
	h.key_press(Key::Escape);
	h.run_steps(2);
	assert!(h.query_by_label("Pitch").is_none(), "Esc closes it");
}

#[test]
fn piano_roll_keys_clone_transpose_and_nudge_like_fl() {
	let (mut h, clip) = fl_piano();
	// Ctrl+A, Ctrl+B: the 3-beat phrase is cloned right after itself, selected; again
	// appends once more.
	h.key_press_modifiers(Modifiers::COMMAND, Key::A);
	h.key_press_modifiers(Modifiers::COMMAND, Key::B);
	h.run_steps(2);
	let notes = clip_notes(&h, clip);
	assert_eq!(notes.len(), 6);
	let starts: Vec<f64> = notes.iter().map(|n| n.start.beats()).collect();
	assert_eq!(starts, vec![0.0, 1.0, 2.0, 3.0, 4.0, 5.0]);
	assert_eq!(h.state().state.view.piano.selected.len(), 3, "the copies");
	assert_eq!(h.state().state.undo_label(), Some("Clone notes"));
	h.key_press_modifiers(Modifiers::COMMAND, Key::B);
	h.run_steps(2);
	assert_eq!(clip_notes(&h, clip).len(), 9);
	assert_eq!(clip_notes(&h, clip)[8].start, Tick(PPQ * 8));
	let clip_len = h.state().state.project().find_clip(clip).unwrap().1.length;
	assert!(clip_len >= Tick(PPQ * 9), "the clip holds the copies");

	// Ctrl+D deselects (FL); arrows then move every note.
	h.key_press_modifiers(Modifiers::COMMAND, Key::D);
	h.run_steps(1);
	assert!(h.state().state.view.piano.selected.is_empty());
	h.key_press(Key::ArrowUp);
	h.run_steps(1);
	assert!(
		clip_notes(&h, clip)
			.iter()
			.take(3)
			.map(|n| n.pitch)
			.eq([61, 63, 65])
	);
	h.key_press_modifiers(Modifiers::COMMAND, Key::ArrowDown);
	h.run_steps(1);
	assert_eq!(clip_notes(&h, clip)[0].pitch, 49, "Ctrl+Down: an octave");
	h.key_press(Key::ArrowRight);
	h.run_steps(1);
	assert_eq!(
		clip_notes(&h, clip)[0].start,
		Tick(PPQ / 4),
		"one step (1/16)"
	);
	h.key_press_modifiers(Modifiers::SHIFT, Key::ArrowLeft);
	h.run_steps(1);
	assert_eq!(
		clip_notes(&h, clip)[0].start,
		Tick::ZERO,
		"clamped at the clip start"
	);

	// Ctrl+C at the untouched playhead, Ctrl+V: pasted where they were, selected.
	let first = clip_notes(&h, clip)[0].id;
	h.state_mut().state.view.piano.select([first], false);
	h.key_press_modifiers(Modifiers::COMMAND, Key::C);
	h.key_press_modifiers(Modifiers::COMMAND, Key::V);
	h.run_steps(2);
	let notes = clip_notes(&h, clip);
	assert_eq!(notes.len(), 10);
	assert_eq!(notes.iter().filter(|n| n.start == Tick::ZERO).count(), 2);
	// Moving the playhead first: pasted at the playhead.
	let start = h.state().state.project().find_clip(clip).unwrap().1.start;
	h.state_mut().state.seek(start + Tick(PPQ * 10));
	h.key_press_modifiers(Modifiers::COMMAND, Key::V);
	h.run_steps(2);
	assert!(
		clip_notes(&h, clip)
			.iter()
			.any(|n| n.start == Tick(PPQ * 10))
	);
}

#[test]
fn piano_roll_shift_drag_duplicates_and_paint_tool_paints_steps() {
	let (mut h, clip) = fl_piano();
	// Shift+drag C4 one beat up a semitone... to the right: a copy, the original stays.
	let a = note_pos(&h, clip, 0.5, 60);
	let b = note_pos(&h, clip, 4.5, 60);
	h.hover_at(a);
	h.run_steps(1);
	h.event(egui::Event::ModifiersChanged(Modifiers::SHIFT));
	h.run_steps(1);
	h.drag_at(a);
	h.run_steps(1);
	h.hover_at(b);
	h.run_steps(1);
	h.drop_at(b);
	h.event(egui::Event::ModifiersChanged(Modifiers::NONE));
	h.run_steps(2);
	let c4: Vec<f64> = clip_notes(&h, clip)
		.iter()
		.filter(|n| n.pitch == 60)
		.map(|n| n.start.beats())
		.collect();
	assert_eq!(c4, vec![0.0, 4.0]);
	assert_eq!(h.state().state.undo_label(), Some("Duplicate notes"));

	// B: paint tool. A drag over an empty row paints a note per step.
	h.key_press(Key::B);
	h.run_steps(1);
	assert_eq!(
		h.state().state.view.piano.tool,
		cadenza_app::state::piano::Tool::Paint
	);
	h.state_mut().state.view.piano.last_length = Tick(PPQ / 4);
	let a = note_pos(&h, clip, 0.05, 67);
	let b = note_pos(&h, clip, 0.95, 67);
	h.hover_at(a);
	h.run_steps(1);
	h.drag_at(a);
	h.run_steps(1);
	for i in 1..=6 {
		h.hover_at(a + (b - a) * (i as f32 / 6.0));
		h.run_steps(1);
	}
	h.drop_at(b);
	h.run_steps(2);
	let g4: Vec<f64> = clip_notes(&h, clip)
		.iter()
		.filter(|n| n.pitch == 67)
		.map(|n| n.start.beats())
		.collect();
	assert_eq!(g4, vec![0.0, 0.25, 0.5, 0.75]);
	assert_eq!(h.state().state.undo_label(), Some("Paint notes"));
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

	// Reverse renders a processed copy into the project's Audio/ folder and swaps it.
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
	assert!(
		swapped.starts_with(dir.path().join("Audio")),
		"{}",
		swapped.display()
	);
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

#[test]
fn adding_an_instrument_then_f7_opens_a_new_clip_and_l_toggles_pattern_mode() {
	use cadenza_app::state::transport::PlayMode;
	let mut h = harness(CadenzaApp::with_state(AppState::headless()));
	h.run_steps(3);
	let item = h
		.state_mut()
		.state
		.browser_items()
		.iter()
		.find(|i| i.id == "electric_piano")
		.unwrap()
		.clone();
	// What a double-click in the browser does.
	assert!(h.state_mut().state.use_device(&item, None));
	h.run_steps(2);
	let track = h.state().state.project().tracks[0].id;
	assert_eq!(h.state().state.selection.track, Some(track));
	assert!(
		h.query_all_by_label_contains("press F7 or double-click the track")
			.next()
			.is_some(),
		"the status bar says how to open the piano roll"
	);

	h.key_press(Key::F7);
	h.run_steps(3);
	let s = &h.state().state;
	let clip = s.view.piano.clip.expect("F7 opened a clip");
	let (owner, c) = s.project().find_clip(clip).unwrap();
	assert_eq!(owner.id, track);
	assert_eq!(c.name, item.name);
	assert!(s.view.panels.visible(PanelKind::PianoRoll));
	assert!(h.state().piano_grid().is_some());
	assert_eq!(s.undo_label(), Some("Add clip 'Electric Piano'"));

	// L: pattern mode; Space plays the open clip alone (no device here: just the setup).
	h.key_press(Key::L);
	h.run_steps(2);
	assert_eq!(h.state().state.view.transport.mode, PlayMode::Pattern);
	h.key_press(Key::Space);
	h.run_steps(2);
	let config = h.state().state.play_config().unwrap().clone();
	assert_eq!(config.pattern, Some(clip));
	assert_eq!(config.audition, Some(vec![track]));
	h.key_press(Key::L);
	h.run_steps(2);
	assert_eq!(h.state().state.view.transport.mode, PlayMode::Song);
	assert_eq!(h.state().state.play_config().unwrap().audition, None);

	// Ctrl+L: a loop region on the playhead's bar (from the arrange: in a focused piano
	// roll Ctrl+L is quick legato, like FL Studio).
	h.state_mut().state.view.panels.focus = Some(PanelKind::Arrange);
	h.key_press_modifiers(Modifiers::COMMAND, Key::L);
	h.run_steps(2);
	assert!(h.state().state.view.transport.loop_on);
}

#[test]
fn double_clicking_an_instrument_header_opens_its_piano_roll() {
	let mut a = CadenzaApp::with_state(AppState::headless());
	let cmd = {
		let p = a.state.project();
		let mut t = cadenza_app::state::edits::new_track(p, cadenza_model::TrackKind::Instrument);
		t.name = "Keys".into();
		cadenza_app::state::edits::add_track(p, t, None)
	};
	assert!(a.state.apply(cmd));
	// Short steps so two clicks fall within the double-click delay.
	let mut h = Harness::builder()
		.with_size(vec2(1600.0, 1000.0))
		.with_step_dt(1.0 / 60.0)
		.build_ui_state(
			|ui, app: &mut CadenzaApp| {
				app.logic(ui.ctx());
				app.ui(ui);
			},
			a,
		);
	h.run_steps(3);
	let lane = h.state().track_rect(0).unwrap();
	let at = pos2(lane.left() - 150.0, lane.bottom() - 8.0);
	h.hover_at(at);
	h.run_steps(1);
	// Two clicks one step (0.25 s) apart: a double-click.
	for _ in 0..2 {
		for pressed in [true, false] {
			h.event(egui::Event::PointerButton {
				pos: at,
				button: egui::PointerButton::Primary,
				pressed,
				modifiers: Modifiers::NONE,
			});
		}
		h.run_steps(1);
	}
	h.run_steps(2);
	let s = &h.state().state;
	let clip = s.view.piano.clip.expect("double-click opened a clip");
	assert_eq!(
		s.project().find_clip(clip).unwrap().0.id,
		s.project().tracks[0].id
	);
	assert!(s.view.panels.visible(PanelKind::PianoRoll));
}

#[test]
fn ruler_drags_set_the_loop_region() {
	let mut h = harness(app());
	h.run_steps(3);
	let lane = h.state().track_rect(0).unwrap();
	let tl = h.state().state.view.timeline;
	// The ruler sits right above the markers lane, above the tracks; its upper half
	// sets the loop. Find it from the lane top: overview + ruler + markers above.
	let ruler_top = lane.top()
		- cadenza_app::theme::MARKER_LANE_HEIGHT
		- cadenza_app::theme::RULER_HEIGHT
		- if h.state().state.view.automation.tempo {
			cadenza_app::state::automation::LANE_HEIGHT
		} else {
			0.0
		};
	let y = ruler_top + 5.0;
	let x = |beat: f64| lane.left() + tl.beat_to_x(beat);
	h.hover_at(pos2(x(4.2), y));
	h.drag_at(pos2(x(4.2), y));
	h.run_steps(1);
	h.hover_at(pos2(x(10.0), y));
	h.run_steps(1);
	h.drop_at(pos2(x(11.7), y));
	h.run_steps(2);
	let t = h.state().state.view.transport;
	assert!(t.loop_on);
	let (s, e) = t.loop_range.unwrap();
	assert!(s <= Tick(PPQ * 5) && s >= Tick(PPQ * 4), "{s:?}");
	assert!(e >= Tick(PPQ * 11) && e <= Tick(PPQ * 12), "{e:?}");
	// A click inside the region turns the loop off; the playhead did not move.
	let before = h.state().state.position();
	h.hover_at(pos2(x(6.0), y));
	h.drag_at(pos2(x(6.0), y));
	h.drop_at(pos2(x(6.0), y));
	h.run_steps(2);
	assert!(!h.state().state.view.transport.loop_on);
	assert_eq!(h.state().state.position(), before);
}

/// Renders a few app states to PNG files in `$CADENZA_SCREENSHOT_DIR` (needs a GPU):
/// A state whose AI chat answers from `turns` (scripted provider, in-memory key, no
/// network).
fn chat_app(
	turns: Vec<cadenza_agent::scripted::ScriptedTurn>,
	delay_ms: u64,
	dir: &std::path::Path,
) -> CadenzaApp {
	use cadenza_app::state::ai::secrets::{MemorySecretStore, SecretStore};
	use cadenza_app::state::ai::settings::ProviderKind;
	let mut a = app();
	let provider = cadenza_agent::scripted::ScriptedProvider::new(turns)
		.with_delay(std::time::Duration::from_millis(delay_ms));
	let store = MemorySecretStore::default();
	store
		.set(ProviderKind::OpenRouter.account(), "test-key")
		.unwrap();
	a.state.ai = cadenza_app::state::ai::AiChat::with_parts(
		std::sync::Arc::new(move |_, _| Ok(Box::new(provider.clone()))),
		std::sync::Arc::new(store),
		dir.join("session"),
		dir.to_path_buf(),
		None,
	);
	a
}

fn chat_turns() -> Vec<cadenza_agent::scripted::ScriptedTurn> {
	use cadenza_agent::scripted::ScriptedTurn;
	vec![
		ScriptedTurn::tools(
			Some("I'll add a bass track and mark the bridge."),
			&[
				(
					"create_track",
					r#"{"name":"Bass","kind":"instrument","instrument":{"device":"poly_synth"}}"#,
				),
				("add_marker", r#"{"beat":52,"name":"Bridge"}"#),
				("add_notes", r#"{"clip_id":"missing","notes":[]}"#),
			],
		)
		.with_cost(0.0021),
		ScriptedTurn::text(
			"Done:\n- added **Bass** (poly synth)\n- marked the `Bridge` at beat 52\n\nUndo it with Ctrl+Z.",
		)
		.with_cost(0.0009),
	]
}

#[test]
fn the_chat_panel_sends_a_request_and_shows_the_reply() {
	let tmp = tempfile::tempdir().unwrap();
	let mut h = harness(chat_app(chat_turns(), 0, tmp.path()));
	h.run_steps(2);
	assert!(!h.state().state.view.panels.visible(PanelKind::Chat));
	h.key_press(Key::F10);
	h.run_steps(2);
	assert!(h.state().state.view.panels.visible(PanelKind::Chat));
	assert!(h.query_by_label("AI PRODUCER").is_some());
	assert!(
		h.query_by_label("Mix the project").is_some(),
		"quick actions"
	);

	let tracks = h.state().state.project().tracks.len();
	h.state_mut().state.ai.input = "add a bass".into();
	h.ctx
		.memory_mut(|m| m.request_focus(egui::Id::new(cadenza_app::ui::chat::INPUT_ID)));
	h.run_steps(1);
	h.key_press(Key::Enter);
	h.run_steps(1);
	assert!(h.state().state.ai.busy() || !h.state().state.ai.items.is_empty());
	for _ in 0..500 {
		h.run_steps(1);
		if !h.state().state.ai.busy() {
			break;
		}
		std::thread::sleep(std::time::Duration::from_millis(10));
	}
	h.run_steps(3);
	let s = &h.state().state;
	assert!(!s.ai.busy());
	assert!(s.ai.input.is_empty(), "sent");
	assert_eq!(s.project().tracks.len(), tracks + 1);
	assert_eq!(s.undo_label(), Some("AI: add a bass"));
	assert!(h.query_by_label_contains("added Bass").is_some());
	assert!(h.query_by_label_contains("create_track Bass").is_some());
	assert!(
		h.query_by_label_contains("add_notes:").is_some(),
		"failed tool row"
	);
	assert!(h.query_by_label_contains("undo: AI: add a bass").is_some());

	// One Ctrl+Z takes the request back.
	h.key_press_modifiers(Modifiers::COMMAND, Key::Z);
	h.run_steps(2);
	assert_eq!(h.state().state.project().tracks.len(), tracks);
}

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
	// Pattern mode with an arrange loop region (the region shows on the ruler only).
	{
		let s = &mut h.state_mut().state;
		s.set_loop_range(Some((Tick(PPQ * 16), Tick(PPQ * 24))));
		s.set_play_mode(cadenza_app::state::transport::PlayMode::Pattern);
		s.seek(midi.start + Tick(PPQ * 2));
	}
	shot(&mut h, "pattern_mode");
	h.state_mut()
		.state
		.set_play_mode(cadenza_app::state::transport::PlayMode::Song);
	shot(&mut h, "song_loop");
	// The piano roll's clip menu, open.
	// The clip menu sits at the left of the piano roll toolbar.
	let grid = h.state().piano_grid().unwrap();
	let at = pos2(grid.left() + 100.0, grid.top() - 37.0);
	h.hover_at(at);
	h.drag_at(at);
	h.drop_at(at);
	shot(&mut h, "piano_clip_menu");
	h.key_press(Key::Escape);
	{
		let s = &mut h.state_mut().state;
		s.set_loop_range(None);
	}
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

	// The AI chat: a finished request, then one in progress, then the settings.
	let tmp_chat = tempfile::tempdir().unwrap();
	let mut chat = Harness::builder()
		.with_size(vec2(1600.0, 1000.0))
		.wgpu()
		.build_ui_state(
			|ui, app: &mut CadenzaApp| {
				app.logic(ui.ctx());
				app.ui(ui);
			},
			{
				let mut turns = chat_turns();
				turns.push(cadenza_agent::scripted::ScriptedTurn::tools(
					Some("Shaping the drums: more attack on the kick and a touch of saturation."),
					&[("add_marker", r#"{"beat":64,"name":"Outro"}"#)],
				));
				turns.push(cadenza_agent::scripted::ScriptedTurn::text("ok"));
				chat_app(turns, 0, tmp_chat.path())
			},
		);
	chat.state_mut()
		.state
		.view
		.panels
		.set(PanelKind::Chat, true);
	chat.state_mut().state.send_ai("add a bass");
	for _ in 0..500 {
		chat.run_steps(1);
		if !chat.state().state.ai.busy() {
			break;
		}
		std::thread::sleep(std::time::Duration::from_millis(10));
	}
	chat.run_steps(3);
	chat.render()
		.unwrap()
		.save(dir.join("ai_chat.png"))
		.unwrap();
	{
		let s = &mut chat.state_mut().state;
		s.send_ai("make the drums punchier");
	}
	// Freeze a mid-request look: wait for the streamed text, then draw.
	for _ in 0..200 {
		chat.run_steps(1);
		if chat
			.state()
			.state
			.ai
			.items
			.iter()
			.any(|i| matches!(i, cadenza_app::state::ai::ChatItem::Assistant { text, .. } if text.contains("drums")))
		{
			break;
		}
		std::thread::sleep(std::time::Duration::from_millis(5));
	}
	chat.state_mut().state.ai.running = Some(cadenza_app::state::ai::Running {
		request: "make the drums punchier".into(),
		started: std::time::Instant::now(),
		cost_usd: 0.0012,
		estimated: false,
		stopping: false,
	});
	chat.run_steps(2);
	chat.render()
		.unwrap()
		.save(dir.join("ai_chat_running.png"))
		.unwrap();
	// The scripted request is long done; drop the staged "running" look.
	std::thread::sleep(std::time::Duration::from_millis(200));
	chat.run_steps(2);
	chat.state_mut().state.ai.running = None;
	chat.state_mut().state.ai.settings_open = true;
	chat.state_mut().state.ai.settings.provider =
		cadenza_app::state::ai::settings::ProviderKind::OpenAiCompatible;
	chat.run_steps(4);
	chat.render()
		.unwrap()
		.save(dir.join("ai_settings.png"))
		.unwrap();

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
	// An instrument added from the browser: the hint, then the empty piano roll dock.
	let item = empty
		.state_mut()
		.state
		.browser_items()
		.iter()
		.find(|i| i.id == "electric_piano")
		.unwrap()
		.clone();
	empty.state_mut().state.use_device(&item, None);
	empty.state_mut().state.view.panels.open_piano_roll();
	empty.run_steps(3);
	empty
		.render()
		.unwrap()
		.save(dir.join("instrument_added.png"))
		.unwrap();
	empty.state_mut().state.close_piano_roll();
	empty.key_press(Key::F7);
	empty.run_steps(3);
	empty
		.render()
		.unwrap()
		.save(dir.join("instrument_f7.png"))
		.unwrap();
}

/// Factory samples rendered once into a temp folder for this test binary.
fn factory_samples() -> PathBuf {
	static DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
	DIR.get_or_init(|| {
		let dir = std::env::temp_dir().join(format!("cadenza-ui-factory-{}", std::process::id()));
		// SAFETY: set once (OnceLock) before the samples are first needed.
		unsafe { std::env::set_var(cadenza_engine::factory::FACTORY_SAMPLES_ENV, &dir) };
		cadenza_engine::factory::ensure_factory_samples().unwrap()
	})
	.clone()
}

/// An app with a drum sampler track and a sampler track, the drum editor open.
fn sampler_app() -> (CadenzaApp, DeviceRef, DeviceRef) {
	factory_samples();
	let mut state = AppState::headless();
	let items = state.browser_items().to_vec();
	let mut refs = Vec::new();
	for id in ["drum_sampler", "sampler"] {
		let item = items.iter().find(|i| i.id == id).unwrap();
		state.selection.select_master();
		assert!(state.use_device(item, None));
		let t = state.project().tracks.last().unwrap();
		refs.push(DeviceRef {
			track: Some(t.id),
			device: t.instrument.as_ref().unwrap().id,
		});
	}
	state.view.panels.set(PanelKind::Mixer, false);
	assert!(state.open_instrument_editor(refs[0]));
	(CadenzaApp::with_state(state), refs[0], refs[1])
}

fn wait_for_peaks(h: &mut Harness<'_, CadenzaApp>, path: &std::path::Path) {
	for _ in 0..400 {
		h.run_steps(1);
		if matches!(
			h.state_mut().state.peaks.get(path),
			cadenza_app::jobs::Peaks::Ready(_)
		) {
			break;
		}
		std::thread::sleep(std::time::Duration::from_millis(10));
	}
	h.run_steps(2);
}

#[test]
fn drum_sampler_editor_selects_pads_and_knobs_commit_once() {
	let (a, drums, _) = sampler_app();
	let mut h = harness(a);
	h.run_steps(3);
	let grid = h.state().pad_grid().expect("pad grid drawn");
	// Pad 1 (kick) sits bottom-left, pad 2 (snare) to its right.
	let snare = pos2(grid.left() + 62.0 + 6.0 + 30.0, grid.bottom() - 30.0);
	h.hover_at(snare);
	h.drag_at(snare);
	h.run_steps(1);
	h.drop_at(snare);
	h.run_steps(2);
	assert_eq!(h.state().state.view.instrument.pad, 38);
	assert!(
		h.state()
			.state
			.undo_label()
			.unwrap()
			.starts_with("Add track")
	);
	// The Gain knob of the selected pad: one drag = one undo step.
	let wave = h.state().instrument_wave().expect("pad waveform drawn");
	let knob = pos2(wave.left() + 23.0, wave.bottom() + 6.0 + 17.0);
	h.drag_at(knob);
	for i in 1..=6 {
		h.hover_at(knob - vec2(0.0, i as f32 * 8.0));
		h.run_steps(1);
	}
	h.drop_at(knob - vec2(0.0, 48.0));
	h.run_steps(2);
	let s = &h.state().state;
	assert_eq!(s.undo_label(), Some("Edit sample zones"));
	let zones = &s.device(drums).unwrap().zones;
	let gain = zones.iter().find(|z| z.key_low == 38).unwrap().gain_db;
	assert!(gain > 0.0, "dragged up: {gain}");
	assert!(
		zones
			.iter()
			.filter(|z| z.key_low != 38)
			.all(|z| z.gain_db <= 0.0)
	);
}

#[test]
fn sampler_editor_shows_the_zone_map_and_inspector_edit_opens_it() {
	let (a, _, sampler) = sampler_app();
	let mut h = harness(a);
	h.state_mut().state.selection.select_device(sampler);
	h.run_steps(3);
	h.get_by_label("Edit zones").click();
	h.run_steps(3);
	let s = &h.state().state;
	assert_eq!(s.view.instrument.device, Some(sampler));
	assert!(s.view.instrument_editor);
	assert!(h.query_by_label("Presets").is_some());
	// Back to the piano roll from the toolbar.
	h.get_by_label("Piano roll").click();
	h.run_steps(2);
	assert!(!h.state().state.view.instrument_editor);
}

/// `CADENZA_SCREENSHOT_DIR=/tmp/shots cargo test -p cadenza-app --test ui -- --ignored`.
#[test]
#[ignore]
fn render_sampler_screens() {
	let Some(dir) = std::env::var_os("CADENZA_SCREENSHOT_DIR").map(PathBuf::from) else {
		return;
	};
	std::fs::create_dir_all(&dir).unwrap();
	let (a, drums, sampler) = sampler_app();
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
	{
		let s = &mut h.state_mut().state;
		s.view.instrument.pad = 46;
		let cmd = cadenza_app::state::samples::edit_pad(s.project(), drums, 46, |z| {
			z.decay_ms = Some(600.0);
			z.output = 2;
		});
		s.apply_sample_edit(cmd);
		s.selection.select_device(drums);
	}
	wait_for_peaks(&mut h, &factory_samples().join("Open Hat.wav"));
	shot(&mut h, "drum_sampler_editor");
	// Dragging a file from the browser over the cowbell pad.
	let grid = h.state().pad_grid().unwrap();
	egui::DragAndDrop::set_payload(
		&h.ctx,
		cadenza_app::state::dnd::Payload::File(factory_samples().join("Rim.wav")),
	);
	h.hover_at(pos2(grid.right() - 30.0, grid.top() + 30.0));
	shot(&mut h, "drum_sampler_drop");
	egui::DragAndDrop::clear_payload(&h.ctx);
	h.remove_cursor();

	{
		let s = &mut h.state_mut().state;
		s.open_instrument_editor(sampler);
		s.view.instrument.zone = 1;
		let cmd = cadenza_app::state::samples::edit_zone(s.project(), sampler, 1, |z| {
			z.loop_mode = cadenza_model::LoopMode::PingPong;
		});
		s.apply_sample_edit(cmd);
	}
	wait_for_peaks(&mut h, &factory_samples().join("Sine Pad C3.wav"));
	shot(&mut h, "sampler_editor");
	// Alt-drag of an audio file below the tracks: a new sampler track.
	{
		let s = &mut h.state_mut().state;
		s.close_piano_roll();
	}
	h.run_steps(2);
	egui::DragAndDrop::set_payload(
		&h.ctx,
		cadenza_app::state::dnd::Payload::File(factory_samples().join("Sine Pad C4.wav")),
	);
	h.event(egui::Event::ModifiersChanged(Modifiers::ALT));
	h.hover_at(pos2(900.0, 600.0));
	shot(&mut h, "alt_drop_new_sampler");
	egui::DragAndDrop::clear_payload(&h.ctx);
}

/// The FL Studio-style piano roll mid-gesture (selection, properties popover, right-button
/// sweep, paint, velocity line, snap menu).
/// `CADENZA_SCREENSHOT_DIR=/tmp/shots cargo test -p cadenza-app --test ui -- --ignored`.
#[test]
#[ignore]
fn render_piano_fl_screens() {
	let Some(dir) = std::env::var_os("CADENZA_SCREENSHOT_DIR").map(PathBuf::from) else {
		return;
	};
	std::fs::create_dir_all(&dir).unwrap();
	let (h, clip) = fl_piano();
	let mut h = {
		let mut app = h.into_state();
		// A melody with varied velocities, two notes selected.
		let notes: Vec<cadenza_model::Note> = [
			(67, 3.0, 0.5, 0.35),
			(69, 3.5, 0.5, 0.55),
			(71, 4.0, 1.0, 1.0),
			(72, 5.0, 2.0, 0.7),
		]
		.iter()
		.map(|(p, s, l, v)| {
			cadenza_model::Note::new(*p, Tick::from_beats(*s), Tick::from_beats(*l), *v)
		})
		.collect();
		let ids: Vec<_> = notes.iter().take(2).map(|n| n.id).collect();
		assert!(
			app.state
				.apply(cadenza_model::Command::AddNotes { clip, notes })
		);
		app.state.view.piano.select(ids, false);
		app.state.view.piano.framed = None;
		Harness::builder()
			.with_size(vec2(1600.0, 1000.0))
			.with_step_dt(1.0 / 60.0)
			.wgpu()
			.build_ui_state(
				|ui, app: &mut CadenzaApp| {
					app.logic(ui.ctx());
					app.ui(ui);
				},
				app,
			)
	};
	let shot = |h: &mut Harness<'_, CadenzaApp>, name: &str| {
		h.run_steps(3);
		h.render()
			.unwrap()
			.save(dir.join(format!("{name}.png")))
			.unwrap();
	};
	h.state_mut().state.view.panels.focus = Some(PanelKind::PianoRoll);
	h.state_mut().state.view.panels.toggle_maximize();
	shot(&mut h, "piano_fl");

	// Double-click a note: the properties popover.
	let e4 = note_pos(&h, clip, 2.5, 64);
	h.hover_at(e4);
	h.run_steps(1);
	for _ in 0..2 {
		h.drag_at(e4);
		h.run_steps(1);
		h.event(egui::Event::PointerButton {
			pos: e4,
			button: egui::PointerButton::Primary,
			pressed: false,
			modifiers: Modifiers::NONE,
		});
		h.run_steps(1);
	}
	shot(&mut h, "piano_fl_properties");
	h.key_press(Key::Escape);
	h.run_steps(1);

	// Right button held, halfway through a sweep (the passed notes are gone).
	let a = note_pos(&h, clip, 0.5, 60);
	let b = note_pos(&h, clip, 2.5, 64);
	h.hover_at(a);
	h.run_steps(1);
	right_button(&h, a, true);
	h.run_steps(1);
	let mid = a + (b - a) * 0.55;
	h.hover_at(mid);
	shot(&mut h, "piano_fl_sweep");
	right_button(&h, mid, false);
	h.run_steps(1);
	h.key_press_modifiers(Modifiers::COMMAND, Key::Z);

	// Paint tool mid-drag.
	h.key_press(Key::B);
	h.run_steps(1);
	let a = note_pos(&h, clip, 8.05, 62);
	h.hover_at(a);
	h.run_steps(1);
	h.drag_at(a);
	h.run_steps(1);
	for i in 1..=4 {
		h.hover_at(note_pos(&h, clip, 8.05 + f64::from(i) * 0.3, 62 + i as u8));
		h.run_steps(1);
	}
	shot(&mut h, "piano_fl_paint");
	h.drop_at(note_pos(&h, clip, 9.3, 66));
	h.run_steps(1);
	h.key_press(Key::P);

	// Velocity lane: a right-drag line.
	let lane_y = {
		let grid = h.state().piano_grid().unwrap();
		grid.bottom() + 30.0
	};
	let x0 = note_pos(&h, clip, 0.0, 60).x + 1.0;
	let x1 = note_pos(&h, clip, 7.0, 60).x;
	h.hover_at(pos2(x0, lane_y + 30.0));
	h.run_steps(1);
	right_button(&h, pos2(x0, lane_y + 30.0), true);
	h.run_steps(1);
	h.hover_at(pos2(x1, lane_y - 25.0));
	shot(&mut h, "piano_fl_velocity_line");
	right_button(&h, pos2(x1, lane_y - 25.0), false);
	h.run_steps(1);

	// The snap ("magnet") menu, right of the tools.
	let grid = h.state().piano_grid().unwrap();
	let at = pos2(grid.left() + 580.0, grid.top() - 37.0);
	h.hover_at(at);
	h.run_steps(1);
	h.drag_at(at);
	h.run_steps(1);
	h.drop_at(at);
	shot(&mut h, "piano_fl_snap_menu");
}

#[test]
fn ctrl_e_opens_the_export_dialog_which_exports_and_reports() {
	let tmp = tempfile::tempdir().unwrap();
	let mut h = harness(app());
	h.run_steps(2);
	h.key_press_modifiers(Modifiers::COMMAND, Key::E);
	h.run_steps(2);
	assert!(h.state().state.export.open);
	assert!(h.query_by_label("Whole song").is_some());
	assert!(h.query_by_label("Normalize").is_some());
	assert!(
		h.query_by_label_contains("Writes first_light.wav")
			.is_some()
	);
	{
		let s = &mut h.state_mut().state;
		s.export.folder = Some(tmp.path().to_path_buf());
		s.export.settings.range = cadenza_app::state::export::RangeKind::Bars;
		s.export.settings.from_bar = 1;
		s.export.settings.to_bar = 1;
		s.export.settings.stems = cadenza_app::state::export::StemMode::Buses;
	}
	h.run_steps(2);
	h.get_by_label("Start export").click();
	h.run_steps(1);
	for _ in 0..1000 {
		h.run_steps(1);
		if h.state().state.export.running.is_none() {
			break;
		}
		std::thread::sleep(std::time::Duration::from_millis(10));
	}
	h.run_steps(2);
	let result = h.state().state.export.result.clone().expect("finished");
	let report = result.unwrap();
	assert!(report.files.len() > 2, "mix + bus stems");
	assert!(h.query_by_label_contains("✔ Exported").is_some());
	assert!(h.query_by_label("Open folder").is_some());
	h.get_by_label("Close").click();
	h.run_steps(2);
	assert!(!h.state().state.export.open);
}

#[test]
fn the_start_screen_offers_new_open_demo_and_recent_projects() {
	let tmp = tempfile::tempdir().unwrap();
	let mut state = AppState::headless();
	state.enable_safety(tmp.path().to_path_buf());
	state.view.start_screen = true;
	state.recent = vec![example()];
	let mut h = harness(CadenzaApp::with_state(state));
	h.run_steps(2);
	assert!(h.query_by_label("New project").is_some());
	assert!(h.query_by_label("Demo song").is_some());
	assert!(
		h.query_by_label_contains("first_light").is_some(),
		"recent list"
	);
	h.get_by_label("Demo song").click();
	h.run_steps(2);
	let s = &h.state().state;
	assert!(!s.view.start_screen);
	assert_eq!(s.project().name, "First Light");
	assert!(s.path().unwrap().starts_with(tmp.path().join("demo")));
	assert_eq!(s.recent.first().map(|p| p.as_path()), s.path());

	h.state_mut().state.view.start_screen = true;
	h.run_steps(2);
	h.get_by_label("New project").click();
	h.run_steps(2);
	assert!(!h.state().state.view.start_screen);
	assert_eq!(h.state().state.project().tracks.len(), 0);
}

#[test]
fn missing_files_open_the_locate_dialog() {
	let tmp = tempfile::tempdir().unwrap();
	let mut p = cadenza_model::Project::new("Lost");
	let mut t = cadenza_model::Track::new("Vox", cadenza_model::TrackKind::Audio);
	t.clips.push(cadenza_model::Clip::audio(
		"take",
		Tick::ZERO,
		Tick(PPQ * 4),
		tmp.path().join("gone").join("vox take.wav"),
	));
	p.tracks.push(t);
	let file = tmp.path().join("Lost").join("Lost.cadenza.json");
	p.save_json(&file).unwrap();
	let mut state = AppState::headless();
	assert!(state.open(&file));
	let mut h = harness(CadenzaApp::with_state(state));
	h.run_steps(2);
	assert!(h.query_by_label("Locate missing files").is_some());
	assert!(h.query_by_label("vox take.wav").is_some());
	assert!(h.query_by_label("Search folder…").is_some());
	h.get_by_label("Close").click();
	h.run_steps(2);
	assert!(h.state().state.relink.is_none());
}

/// Start screen, export dialog (settings, progress-free result), missing files and
/// recovery prompts:
/// `CADENZA_SCREENSHOT_DIR=/tmp/shots cargo test -p cadenza-app --test ui -- --ignored`.
#[test]
#[ignore]
fn render_project_screens() {
	use cadenza_app::state::export::{NormalizeChoice, StemMode};
	let Some(dir) = std::env::var_os("CADENZA_SCREENSHOT_DIR").map(PathBuf::from) else {
		return;
	};
	std::fs::create_dir_all(&dir).unwrap();
	let tmp = tempfile::tempdir().unwrap();
	let mut state = AppState::headless();
	state.enable_safety(tmp.path().join("state"));
	state.view.start_screen = true;
	state.recent = vec![
		example(),
		PathBuf::from("/home/me/Music/Night Drive/Night Drive.cadenza.json"),
	];
	let mut h = Harness::builder()
		.with_size(vec2(1600.0, 1000.0))
		.wgpu()
		.build_ui_state(
			|ui, app: &mut CadenzaApp| {
				app.logic(ui.ctx());
				app.ui(ui);
			},
			CadenzaApp::with_state(state),
		);
	let shot = |h: &mut Harness<'_, CadenzaApp>, name: &str| {
		h.run_steps(3);
		h.render()
			.unwrap()
			.save(dir.join(format!("{name}.png")))
			.unwrap();
	};
	shot(&mut h, "start_screen");

	assert!(h.state_mut().state.open(&example()));
	h.key_press_modifiers(Modifiers::COMMAND, Key::E);
	h.run_steps(2);
	{
		let s = &mut h.state_mut().state;
		s.export.folder = Some(tmp.path().join("Renders"));
		s.export.settings.stems = StemMode::Buses;
		s.export.settings.normalize = NormalizeChoice::Loudness;
		s.export.settings.artist = "Cadenza".into();
	}
	shot(&mut h, "export_dialog");
	{
		let s = &mut h.state_mut().state;
		s.export.settings.range = cadenza_app::state::export::RangeKind::Bars;
		s.export.settings.from_bar = 1;
		s.export.settings.to_bar = 2;
		assert!(s.start_export());
	}
	h.run_steps(1);
	shot(&mut h, "export_progress");
	for _ in 0..2000 {
		h.run_steps(1);
		if h.state().state.export.running.is_none() {
			break;
		}
		std::thread::sleep(std::time::Duration::from_millis(10));
	}
	shot(&mut h, "export_done");
	h.state_mut().state.export.open = false;

	let mut p = cadenza_model::Project::new("Lost");
	let mut t = cadenza_model::Track::new("Vox", cadenza_model::TrackKind::Audio);
	for name in ["vox take 3.wav", "gtr DI.wav"] {
		t.clips.push(cadenza_model::Clip::audio(
			name,
			Tick::ZERO,
			Tick(PPQ * 4),
			tmp.path().join("old drive").join(name),
		));
	}
	p.tracks.push(t);
	let file = tmp.path().join("Lost").join("Lost.cadenza.json");
	p.save_json(&file).unwrap();
	assert!(h.state_mut().state.open(&file));
	shot(&mut h, "locate_missing_files");
	h.state_mut().state.relink = None;
	h.state_mut().state.recovery = Some(cadenza_app::state::safety::Recovery {
		autosave: tmp.path().join("a.json"),
		modified: std::time::SystemTime::now() - std::time::Duration::from_secs(300),
		reason: cadenza_app::state::safety::RecoveryReason::Crashed,
		project: Some(file),
		cleanup: None,
	});
	shot(&mut h, "recovery_prompt");
}

/// The example song plus an audio track "Vox" holding a three-take loop recording over
/// bars 1-2 (written to a real WAV so waveforms draw).
fn takes_app() -> (CadenzaApp, cadenza_core::TrackId) {
	use cadenza_engine::{RecordedAudio, RecordedPass, Recording};
	let mut a = app();
	let vox =
		cadenza_model::Track::new("Vox", cadenza_model::TrackKind::Audio).with_color("#E05C8C");
	let id = vox.id;
	a.state.apply(cadenza_model::Command::AddTrack {
		track: vox,
		index: Some(0),
	});
	let dir = std::env::temp_dir().join(format!("cadenza-ui-takes-{}", std::process::id()));
	std::fs::create_dir_all(&dir).unwrap();
	let file = dir.join("vox.wav");
	let sr = 48_000u32;
	let tempo = a.state.project().tempo.bpm_at(Tick::ZERO);
	let bar = (4.0 * 60.0 / tempo * f64::from(sr)) as u64;
	let len = 2 * bar;
	let mut w = hound::WavWriter::create(
		&file,
		hound::WavSpec {
			channels: 1,
			sample_rate: sr,
			bits_per_sample: 32,
			sample_format: hound::SampleFormat::Float,
		},
	)
	.unwrap();
	for i in 0..3 * len {
		let pass = (i / len) as f32;
		let t = i as f32 / sr as f32;
		let env = 0.3 + 0.25 * (t * (1.5 + pass)).sin().abs();
		w.write_sample(env * (t * 220.0 * std::f32::consts::TAU).sin())
			.unwrap();
	}
	w.finalize().unwrap();
	let rec = Recording {
		sample_rate: sr,
		start: 0,
		punch_out: None,
		stop: 0,
		latency: 0,
		midi_latency: 0,
		passes: (0..3)
			.map(|k| RecordedPass {
				tag: 0,
				file_start: k * len,
				frames: len,
				wrapped_at: (k < 2).then_some(len as i64),
			})
			.collect(),
		audio: vec![RecordedAudio {
			track: id,
			name: "Vox".into(),
			file,
			frames: 3 * len,
			channels: 1,
		}],
		midi_tracks: Vec::new(),
		midi: Vec::new(),
		dropouts: 0,
		warnings: Vec::new(),
	};
	assert!(a.state.finish_recording(&rec));
	(a, id)
}

#[test]
fn transport_record_button_arm_and_record_mode() {
	let (a, vox) = takes_app();
	let mut h = harness(a);
	h.run_steps(2);
	// Nothing armed: Ctrl+R explains what to do.
	h.key_press_modifiers(Modifiers::COMMAND, Key::R);
	h.run_steps(2);
	assert!(
		h.state()
			.state
			.notices
			.last()
			.unwrap()
			.text
			.contains("Arm a track")
	);
	// The track header's arm button.
	h.get_by_label("Arm Vox").click();
	h.run_steps(2);
	assert!(h.state().state.is_armed(vox));
	assert!(
		h.state()
			.state
			.project()
			.track(vox)
			.unwrap()
			.input
			.is_some(),
		"armed audio tracks get an input"
	);
	// R: record mode; the record button (no audio device here) says why it cannot.
	h.key_press(Key::R);
	h.run_steps(2);
	assert!(h.state().state.record.mode);
	h.get_by_label("Record (Ctrl+R)").click();
	h.run_steps(2);
	let s = &h.state().state;
	assert!(!s.is_recording());
	assert!(s.notices.last().unwrap().text.contains("No audio device"));
	// The metronome button toggles the click while playing.
	h.get_by_label("Metronome").click();
	h.run_steps(2);
	assert!(h.state().state.record.settings.metronome.on_play);
	assert!(
		h.state().state.prefs().record.metronome.on_play,
		"persisted"
	);
}

#[test]
fn audio_settings_dialog_applies_and_persists() {
	let mut h = harness(app());
	h.run_steps(2);
	h.get_by_label("File").click();
	h.run_steps(2);
	h.get_by_label("Audio Settings… ").click();
	h.run_steps(2);
	assert!(h.state().state.record.dialog.is_some());
	h.get_by_label("Audio settings");
	assert!(
		h.query_by_label("Test tone (440 Hz, -20 dBFS)").is_some(),
		"test tone offered, off by default"
	);
	assert!(!h.state().state.record.test_tone);
	// Edit the draft as the widgets would, then OK.
	h.state_mut()
		.state
		.record
		.dialog
		.as_mut()
		.unwrap()
		.draft
		.latency_offset_ms = 4.5;
	h.get_by_label("Record and monitor audio input").click();
	h.run_steps(1);
	h.get_by_label("OK").click();
	h.run_steps(2);
	let s = &h.state().state;
	assert!(s.record.dialog.is_none());
	let prefs = s.prefs().audio;
	assert_eq!(prefs.latency_offset_ms, 4.5);
	assert!(prefs.config.input_enabled);
}

#[test]
fn take_lanes_pick_and_swipe_comp_takes() {
	let (mut a, vox) = takes_app();
	a.state.toggle_takes(vox);
	let mut h = harness(a);
	h.run_steps(3);
	let index = h.state().state.project().track_index(vox).unwrap();
	let lane = h.state().track_rect(index).expect("vox lane drawn");
	let take_y = |k: f32| lane.bottom() + (k + 0.5) * cadenza_app::state::rows::TAKE_LANE_HEIGHT;
	let x = |h: &Harness<'_, CadenzaApp>, beats: f64| {
		lane.left()
			+ h.state()
				.state
				.view
				.timeline
				.tick_to_x(Tick::from_beats(beats))
	};
	let playing = |h: &Harness<'_, CadenzaApp>| -> Vec<(u32, Tick)> {
		h.state()
			.state
			.project()
			.track(vox)
			.unwrap()
			.clips
			.iter()
			.filter(|c| c.plays())
			.map(|c| (c.take.unwrap().lane, c.start))
			.collect()
	};
	assert_eq!(playing(&h), vec![(2, Tick::ZERO)]);
	// Click take 1: it plays for the whole region.
	let p = pos2(x(&h, 2.0), take_y(0.0));
	h.hover_at(p);
	h.run_steps(1);
	h.drag_at(p);
	h.run_steps(1);
	h.drop_at(p);
	h.run_steps(2);
	assert_eq!(playing(&h), vec![(0, Tick::ZERO)]);
	assert_eq!(h.state().state.undo_label(), Some("Select take"));
	// Swipe take 2 over beats 4-6 (snapped): comped in, one undo step.
	let (from, to) = (pos2(x(&h, 4.0), take_y(1.0)), pos2(x(&h, 6.0), take_y(1.0)));
	h.hover_at(from);
	h.drag_at(from);
	for i in 1..=8 {
		h.hover_at(from + (to - from) * (i as f32 / 8.0));
		h.run_steps(1);
	}
	h.drop_at(to);
	h.run_steps(2);
	assert_eq!(h.state().state.undo_label(), Some("Comp take"));
	let comp = playing(&h);
	assert_eq!(comp.len(), 3, "{comp:?}");
	assert_eq!(comp[1], (1, Tick(PPQ * 4)));
}

/// Recording screens: armed tracks, take lanes and a recording in progress (fed by the
/// engine's virtual device, no hardware), and the audio settings dialog.
/// `CADENZA_SCREENSHOT_DIR=/tmp/shots cargo test -p cadenza-app --test ui -- --ignored`.
#[test]
#[ignore]
fn render_recording_screens() {
	let Some(dir) = std::env::var_os("CADENZA_SCREENSHOT_DIR").map(PathBuf::from) else {
		return;
	};
	std::fs::create_dir_all(&dir).unwrap();
	let (mut a, vox) = takes_app();
	let keys = a
		.state
		.project()
		.tracks
		.iter()
		.find(|t| t.name == "Keys")
		.unwrap()
		.id;
	a.state.toggle_arm(vox);
	a.state.toggle_arm(keys);
	a.state.toggle_takes(vox);
	a.state.record.mode = true;
	a.state.record.input_levels = vec![0.5, 0.2];
	let project = a.state.project().clone();
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
	shot(&mut h, "record_armed_takes");

	// A recording in progress: the engine's virtual device records a synthetic voice.
	let (mut player, mut dev) = cadenza_engine::Player::virtual_device(48_000, 2);
	player.load(&project).unwrap();
	let rec_dir = std::env::temp_dir().join(format!("cadenza-ui-rec-{}", std::process::id()));
	let setup = cadenza_engine::RecordSetup::new(
		&rec_dir,
		Tick(PPQ * 8),
		vec![
			cadenza_engine::RecordTrack {
				track: vox,
				name: "Vox".into(),
				input: Some(cadenza_model::TrackInput::mono(0)),
			},
			cadenza_engine::RecordTrack {
				track: keys,
				name: "Keys".into(),
				input: None,
			},
		],
	);
	player.seek(Tick(PPQ * 8)).unwrap();
	let (mut midi_tx, midi_rx) = cadenza_midi_io::new_queue(64);
	player.set_midi_queue(midi_rx).unwrap();
	let monitor = player.start_recording(&setup).unwrap();
	let mut out = vec![0.0f32; 512];
	let mut input = vec![0.0f32; 512];
	let mut n = 0usize;
	for block in 0..700 {
		for i in 0..256 {
			let t = (n + i) as f32 / 48_000.0;
			input[2 * i] = (0.2 + 0.6 * (t * 2.1).sin().abs()) * (t * 330.0).sin();
		}
		if block % 60 == 0 {
			let pitch = 60 + (block / 60 % 5) as u8 * 2;
			let _ = midi_tx.push(cadenza_midi_io::TimedMessage {
				host_micros: 0,
				message: cadenza_midi_io::RawMessage::from_bytes(&[0x90, pitch, 100]).unwrap(),
			});
		}
		if block % 60 == 40 {
			let pitch = 60 + (block / 60 % 5) as u8 * 2;
			let _ = midi_tx.push(cadenza_midi_io::TimedMessage {
				host_micros: 0,
				message: cadenza_midi_io::RawMessage::from_bytes(&[0x80, pitch, 0]).unwrap(),
			});
		}
		dev.process(&mut out, &input);
		n += 256;
	}
	std::thread::sleep(std::time::Duration::from_millis(100));
	let now = player.position();
	{
		let s = &mut h.state_mut().state;
		s.record.phase = cadenza_app::state::record::Phase::Recording(monitor);
		s.seek(now);
	}
	shot(&mut h, "record_in_progress");
	player.request_stop_recording().unwrap();
	dev.process(&mut out, &input);
	let _ = player.finish_recording(std::time::Duration::from_secs(2));
	h.state_mut().state.record.phase = cadenza_app::state::record::Phase::Idle;
	let _ = std::fs::remove_dir_all(&rec_dir);

	h.state_mut().state.open_audio_settings();
	shot(&mut h, "audio_settings");
}

fn screenshot_harness(a: CadenzaApp) -> Harness<'static, CadenzaApp> {
	Harness::builder()
		.with_size(vec2(1600.0, 1000.0))
		.wgpu()
		.build_ui_state(
			|ui, app: &mut CadenzaApp| {
				app.logic(ui.ctx());
				app.ui(ui);
			},
			a,
		)
}

fn shot(h: &mut Harness<'_, CadenzaApp>, dir: &std::path::Path, name: &str) {
	h.run_steps(3);
	h.render()
		.unwrap()
		.save(dir.join(format!("{name}.png")))
		.unwrap();
}

#[test]
#[ignore]
fn render_phase17_screens() {
	let Some(dir) = std::env::var_os("CADENZA_SCREENSHOT_DIR").map(PathBuf::from) else {
		return;
	};
	std::fs::create_dir_all(&dir).unwrap();
	let mut a = app();
	let ts = |numerator, denominator| cadenza_core::TimeSignature {
		numerator,
		denominator,
	};
	for (bar, sig) in [(4, ts(3, 4)), (6, ts(7, 8)), (8, ts(4, 4))] {
		assert!(a.state.apply(cadenza_model::Command::SetTimeSignature {
			time_signature: sig,
			bar,
		}));
	}
	a.state.view.automation.tempo = true;
	let midi = {
		let p = a.state.project();
		let t = p.tracks.iter().find(|t| t.name == "Keys").unwrap();
		t.clips.iter().find(|c| c.is_midi()).unwrap().id
	};
	a.state.open_piano_roll(midi);
	let mut h = screenshot_harness(a);
	shot(&mut h, &dir, "time_signatures");

	// Controller lanes in the piano roll: a mod wheel swell and a pitch bend dive.
	{
		use cadenza_model::controller::ramp;
		use cadenza_model::{Command, Controller};
		let s = &mut h.state_mut().state;
		let len = s.project().find_clip(midi).unwrap().1.length;
		for (c, a, b) in [
			(Controller::MOD_WHEEL, 0.0, 1.0),
			(Controller::PitchBend, 0.0, -1.0),
		] {
			let points = ramp(c, Tick(0), a, len - Tick(1), b, Tick(PPQ / 4), 0.6);
			assert!(s.apply(Command::SetControllerLane {
				clip: midi,
				controller: c,
				points,
			}));
		}
		s.view.piano.event_lane =
			cadenza_app::state::piano::EventLane::Controller(Controller::MOD_WHEEL);
	}
	shot(&mut h, &dir, "controller_lane_mod_wheel");
	h.state_mut().state.view.piano.event_lane =
		cadenza_app::state::piano::EventLane::Controller(cadenza_model::Controller::PitchBend);
	shot(&mut h, &dir, "controller_lane_pitch_bend");

	// Overlapping audio clips: an automatic crossfade with its length handle.
	{
		let wav = tone_wav(&dir);
		let s = &mut h.state_mut().state;
		let mut t = cadenza_model::Track::new("Vox", cadenza_model::TrackKind::Audio);
		let a = cadenza_model::Clip::audio("Take A", Tick(0), Tick(PPQ * 6), &wav);
		let b = cadenza_model::Clip::audio("Take B", Tick(PPQ * 4), Tick(PPQ * 6), &wav);
		t.clips = vec![b, a];
		assert!(s.apply(cadenza_model::Command::AddTrack {
			track: t,
			index: Some(0)
		}));
		s.close_piano_roll();
		s.view.timeline.px_per_beat = 60.0;
	}
	shot(&mut h, &dir, "crossfade");

	// A folder with two tracks (expanded, then folded).
	{
		let s = &mut h.state_mut().state;
		let ids: Vec<_> = s
			.project()
			.tracks
			.iter()
			.filter(|t| t.name == "Bass" || t.name == "Pad")
			.map(|t| t.id)
			.collect();
		s.group_into_folder(&ids);
	}
	shot(&mut h, &dir, "folder_open");
	let folder = h.state().state.selection.track.unwrap();
	h.state_mut().state.toggle_folder(folder);
	shot(&mut h, &dir, "folder_folded");
}

#[test]
fn piano_roll_event_lane_draws_controllers_in_one_step() {
	use cadenza_model::Controller;
	let (mut h, clip) = fl_piano();
	h.state_mut().state.view.piano.event_lane =
		cadenza_app::state::piano::EventLane::Controller(Controller::MOD_WHEEL);
	h.run_steps(2);
	let lane_y = h.state().piano_grid().unwrap().bottom() + 30.0;
	let x = |h: &Harness<'_, CadenzaApp>, beat: f64| note_pos(h, clip, beat, 60).x;
	let undo_depth = h.state().state.document().history_labels().len();
	// Pencil: a stroke rising from left to right.
	h.hover_at(pos2(x(&h, 0.1), lane_y + 20.0));
	h.run_steps(1);
	h.drag_at(pos2(x(&h, 0.1), lane_y + 20.0));
	h.run_steps(1);
	for i in 1..=6 {
		h.hover_at(pos2(
			x(&h, 0.1 + f64::from(i) * 0.3),
			lane_y + 20.0 - i as f32 * 6.0,
		));
		h.run_steps(1);
	}
	h.drop_at(pos2(x(&h, 1.9), lane_y - 16.0));
	h.run_steps(2);
	let lane = |h: &Harness<'_, CadenzaApp>, c: Controller| {
		h.state()
			.state
			.project()
			.find_clip(clip)
			.unwrap()
			.1
			.controller(c)
			.map(|l| l.points.clone())
			.unwrap_or_default()
	};
	let points = lane(&h, Controller::MOD_WHEEL);
	assert!(points.len() >= 4, "{points:?}");
	assert!(
		points.last().unwrap().value > points[0].value,
		"rising stroke"
	);
	assert_eq!(
		h.state().state.document().history_labels().len(),
		undo_depth + 1,
		"one undo step"
	);
	assert_eq!(h.state().state.undo_label(), Some("Edit Mod wheel"));

	// Pitch bend: a right-drag ramp, then a right-click deletes one event.
	h.state_mut().state.view.piano.event_lane =
		cadenza_app::state::piano::EventLane::Controller(Controller::PitchBend);
	h.run_steps(2);
	h.hover_at(pos2(x(&h, 2.0), lane_y));
	h.run_steps(1);
	right_button(&h, pos2(x(&h, 2.0), lane_y), true);
	h.run_steps(1);
	h.hover_at(pos2(x(&h, 3.0), lane_y - 30.0));
	h.run_steps(1);
	right_button(&h, pos2(x(&h, 3.0), lane_y - 30.0), false);
	h.run_steps(2);
	let bend = lane(&h, Controller::PitchBend);
	assert!(bend.len() >= 3, "{bend:?}");
	assert!(bend.last().unwrap().value > 0.5);
	let n = bend.len();
	let at = pos2(x(&h, bend[1].tick.beats()), lane_y);
	h.hover_at(at);
	h.run_steps(1);
	right_button(&h, at, true);
	h.run_steps(1);
	right_button(&h, at, false);
	h.run_steps(2);
	assert_eq!(lane(&h, Controller::PitchBend).len(), n - 1);
}

#[test]
fn the_automation_mode_button_cycles_read_touch_latch() {
	use cadenza_app::state::autowrite::WriteMode;
	let mut h = harness(app());
	h.run_steps(2);
	h.get_by_label("A: Read").click();
	h.run_steps(2);
	assert_eq!(h.state().state.autowrite.mode, WriteMode::Touch);
	h.get_by_label("A: Touch").click();
	h.run_steps(2);
	assert_eq!(h.state().state.autowrite.mode, WriteMode::Latch);
	h.get_by_label("A: Latch").click();
	h.run_steps(2);
	assert_eq!(h.state().state.autowrite.mode, WriteMode::Off);
}

#[test]
fn a_frozen_track_edited_afterwards_offers_a_one_click_refreeze() {
	// An unsaved copy of the demo: the refreeze render goes to a temporary state folder.
	let tmp = tempfile::tempdir().unwrap();
	let mut state = AppState::headless();
	let project = cadenza_model::Project::load_json(example()).unwrap();
	state.load_document(cadenza_model::Document::new(project), None);
	state.safety.state_dir = tmp.path().to_path_buf();
	let mut h = harness(CadenzaApp::with_state(state));
	h.run_steps(2);
	let (keys, clip) = {
		let p = h.state().state.project();
		let t = p.tracks.iter().find(|t| t.name == "Keys").unwrap();
		(t.id, t.clips[0].id)
	};
	{
		let s = &mut h.state_mut().state;
		let source = s.project().freeze_source(keys);
		assert!(s.apply(cadenza_model::Command::SetTrackFreeze {
			track: keys,
			freeze: Some(cadenza_model::Freeze {
				file: "/nonexistent/Keys freeze 1.wav".into(),
				source,
			}),
		}));
	}
	h.run_steps(2);
	assert!(
		h.query_all_by_label("Keys is frozen").count() >= 2,
		"header and mixer"
	);
	assert!(h.query_by_label("Refreeze Keys").is_none());
	assert!(h.state_mut().state.apply(cadenza_model::Command::AddNotes {
		clip,
		notes: vec![cadenza_model::Note::new(72, Tick(0), Tick(PPQ), 0.7)],
	}));
	h.run_steps(2);
	assert!(h.state().state.document().freeze_stale(keys));
	h.query_all_by_label("Refreeze Keys")
		.next()
		.unwrap()
		.click();
	h.run_steps(2);
	assert!(
		h.state().state.bounce_busy(),
		"the click renders the track again"
	);
	let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
	while h.state().state.bounce_busy() {
		assert!(std::time::Instant::now() < deadline, "render timed out");
		std::thread::sleep(std::time::Duration::from_millis(20));
		h.run_steps(1);
	}
	assert!(!h.state().state.document().freeze_stale(keys));
	assert!(h.query_all_by_label("Keys is frozen").count() >= 2);
}

#[test]
fn the_xrun_indicator_shows_dropouts_and_resets_on_click() {
	let mut h = harness(app());
	h.run_steps(2);
	assert!(
		h.query_by_label("Dropouts").is_none(),
		"hidden without dropouts"
	);
	h.state_mut()
		.state
		.xruns
		.update(3, Vec::new(), std::time::Instant::now());
	h.run_steps(2);
	assert_eq!(h.state().state.xruns.count(), 3);
	h.get_by_label("Dropouts").click();
	h.run_steps(2);
	assert_eq!(h.state().state.xruns.count(), 0);
	assert!(h.query_by_label("Dropouts").is_none());
}

#[test]
fn folders_group_tracks_and_fold_in_the_arrange() {
	let mut a = app();
	let ids: Vec<_> = a
		.state
		.project()
		.tracks
		.iter()
		.filter(|t| t.name == "Bass" || t.name == "Pad")
		.map(|t| t.id)
		.collect();
	a.state.group_into_folder(&ids);
	assert_eq!(a.state.undo_label(), Some("Group into folder"));
	let folder = a.state.selection.track.unwrap();
	assert_eq!(a.state.project().folder_contents(folder), ids);
	let mut h = harness(a);
	h.run_steps(2);
	let rows_of = |h: &Harness<'_, CadenzaApp>| {
		let s = &h.state().state;
		cadenza_app::state::rows::Rows::new(s.project(), &s.view)
			.rows
			.len()
	};
	let rows = rows_of(&h);
	h.get_by_label("Fold").click();
	h.run_steps(2);
	assert!(h.state().state.view.collapsed.contains(&folder));
	assert_eq!(rows_of(&h), rows - 2, "contents hidden");
}

fn open_library(h: &mut Harness<'_, CadenzaApp>) {
	h.run_steps(2);
	h.get_by_label("Library").click();
	h.run_steps(2);
}

#[test]
fn library_tab_searches_previews_and_writes_patterns() {
	let mut h = harness(app());
	open_library(&mut h);
	assert!(h.query_by_label("Credits").is_some());
	h.state_mut().state.library_view.query = "dembow".into();
	h.run_steps(2);
	h.get_by_label("Dembow basic").click();
	h.run_steps(2);
	let s = &h.state().state;
	assert_eq!(
		s.library_view.selected.as_deref(),
		Some("core/dembow_basic")
	);
	assert_eq!(
		s.pattern_preview(),
		Some("core/dembow_basic"),
		"click auditions the pattern"
	);
	// Clicking again stops it.
	h.get_by_label("Dembow basic").click();
	h.run_steps(2);
	assert_eq!(h.state().state.pattern_preview(), None);

	// Dropping it below the tracks adds a drum track with the clip: one undo step.
	let tracks = h.state().state.project().tracks.len();
	let s = &mut h.state_mut().state;
	assert!(s.drop_payload(
		&cadenza_app::state::dnd::Payload::Pattern {
			id: "core/dembow_basic".into(),
			name: "Dembow basic".into(),
		},
		cadenza_app::state::dnd::DropTarget::Below {
			at: Tick(PPQ * 4 * 8)
		},
		false,
	));
	assert_eq!(s.project().tracks.len(), tracks + 1);
	let track = s.project().tracks.last().unwrap();
	assert_eq!(track.name, "Drums");
	assert_eq!(track.clips[0].name, "Dembow basic [core/dembow_basic]");
	assert_eq!(track.clips[0].start, Tick(PPQ * 4 * 8));
	assert_eq!(s.undo_label(), Some("Add pattern 'Dembow basic'"));
	s.undo();
	assert_eq!(s.project().tracks.len(), tracks);
}

#[test]
fn library_documents_apply_forms_and_list_credits() {
	let mut h = harness(app());
	open_library(&mut h);
	{
		let v = &mut h.state_mut().state.library_view;
		v.kind = Some(cadenza_library::ItemKind::Style);
		v.query = "latin pop".into();
	}
	h.run_steps(2);
	h.get_by_label("Latin pop (reggaeton-pop)").click();
	h.run_steps(2);
	assert_eq!(
		h.state().state.library_view.open.as_deref(),
		Some("core/latin_pop")
	);
	assert!(h.query_by_label("HARMONY").is_some());
	h.get_all_by_label("Apply").next().unwrap().click();
	h.run_steps(2);
	let s = &h.state().state;
	assert_eq!(s.project().meta.style.as_deref(), Some("core/latin_pop"));
	assert_eq!(s.project().tempo.points()[0].bpm, 94.0);
	assert!(s.project().markers.iter().any(|m| m.name == "Post-chorus"));
	assert_eq!(s.undo_label(), Some("Apply form 'Latin pop radio form'"));

	h.get_by_label("‹ Library").click();
	h.run_steps(2);
	h.get_by_label("Credits").click();
	h.run_steps(2);
	assert!(h.state().state.library_view.credits);
	assert!(h.query_by_label("Cadenza Core").is_some());
}

/// `CADENZA_SCREENSHOT_DIR=/tmp/shots cargo test -p cadenza-app --test ui -- --ignored`.
#[test]
#[ignore]
fn render_library_screens() {
	let Some(dir) = std::env::var_os("CADENZA_SCREENSHOT_DIR").map(PathBuf::from) else {
		return;
	};
	std::fs::create_dir_all(&dir).unwrap();
	let mut a = app();
	a.state.view.panels.focus = Some(PanelKind::Browser);
	let mut h = screenshot_harness(a);
	open_library(&mut h);
	shot(&mut h, &dir, "library_list");
	{
		let v = &mut h.state_mut().state.library_view;
		v.genre = Some("reggaeton".into());
		v.role = Some(cadenza_library::Role::Bass);
	}
	shot(&mut h, &dir, "library_filtered");
	{
		let s = &mut h.state_mut().state;
		s.library_view.clear_filters();
		s.library_view.query = "dembow".into();
	}
	h.run_steps(2);
	h.get_by_label("Dembow basic").click();
	shot(&mut h, &dir, "library_preview");
	h.state_mut().state.stop_pattern_preview();
	h.get_by_label("Dembow basic").hover();
	h.run_steps(60);
	shot(&mut h, &dir, "library_pattern_tooltip");
	h.remove_cursor();
	{
		let v = &mut h.state_mut().state.library_view;
		v.query.clear();
		v.open = Some("core/latin_pop".into());
	}
	shot(&mut h, &dir, "library_style");
	h.state_mut().state.library_view.open = Some("core/form_latin_pop_radio".into());
	shot(&mut h, &dir, "library_form");
	{
		let v = &mut h.state_mut().state.library_view;
		v.open = None;
		v.credits = true;
	}
	shot(&mut h, &dir, "library_credits");
	h.state_mut().state.library_view.credits = false;

	// Dragging a pattern over the Bass lane: the drop hint and the ghost clip.
	egui::DragAndDrop::set_payload(
		&h.ctx,
		cadenza_app::state::dnd::Payload::Pattern {
			id: "core/bass_tresillo_808".into(),
			name: "Tresillo 808 bass".into(),
		},
	);
	h.hover_at(pos2(900.0, 365.0));
	shot(&mut h, &dir, "library_dnd");
	egui::DragAndDrop::clear_payload(&h.ctx);
	h.remove_cursor();
}

/// Runs frames until the mix assistant and the AI are idle.
fn settle_mix(h: &mut Harness<'_, CadenzaApp>) {
	for _ in 0..3000 {
		h.run_steps(1);
		let s = &h.state().state;
		if !s.mix.busy() && !s.ai.busy() {
			break;
		}
		std::thread::sleep(std::time::Duration::from_millis(10));
	}
	h.run_steps(3);
	assert!(
		!h.state().state.mix.busy(),
		"the mix assistant did not finish"
	);
}

#[test]
fn the_mix_tab_analyzes_the_song_and_fixes_an_issue() {
	let mut h = harness(app());
	h.run_steps(2);
	h.key_press(Key::F10);
	h.run_steps(2);
	h.get_by_label("MIX").click();
	h.run_steps(2);
	assert!(h.state().state.mix.open);
	assert!(h.query_by_label("AI mix pass").is_some());
	// A far target makes sure there is a loudness issue to fix.
	h.state_mut().state.mix.target_lufs = Some(-30.0);
	h.get_by_label("Analyze").click();
	h.run_steps(1);
	settle_mix(&mut h);
	assert!(h.state().state.mix.report.is_some());
	assert!(h.query_by_label("lows mono").is_some());
	assert!(h.query_by_label("CHANNELS").is_some());
	assert!(h.query_by_label_contains("ISSUES (").is_some());
	let undo = h.state().state.document().history_labels().len();
	h.get_all_by_label("Fix")
		.next()
		.expect("a fixable issue")
		.scroll_to_me();
	h.run_steps(2);
	// Toasts sit in the bottom-right corner: bring the button up above them.
	h.hover_at(pos2(1450.0, 500.0));
	for _ in 0..6 {
		h.event(egui::Event::MouseWheel {
			unit: egui::MouseWheelUnit::Point,
			delta: vec2(0.0, -60.0),
			phase: egui::TouchPhase::Move,
			modifiers: Modifiers::NONE,
		});
		h.run_steps(1);
	}
	h.get_all_by_label("Fix").next().unwrap().click();
	h.run_steps(2);
	let s = &h.state().state;
	assert_eq!(s.document().history_labels().len(), undo + 1);
	assert!(s.undo_label().unwrap().starts_with("Mix fix"));
	assert!(
		h.query_by_label("The project changed since this analysis")
			.is_some()
	);
	h.get_by_label("AI PRODUCER").click();
	h.run_steps(2);
	assert!(!h.state().state.mix.open);
}

/// `CADENZA_SCREENSHOT_DIR=/tmp/shots cargo test -p cadenza-app --test ui -- --ignored`.
#[test]
#[ignore]
fn render_mix_screens() {
	use cadenza_agent::scripted::ScriptedTurn;
	let Some(dir) = std::env::var_os("CADENZA_SCREENSHOT_DIR").map(PathBuf::from) else {
		return;
	};
	std::fs::create_dir_all(&dir).unwrap();
	let tmp = tempfile::tempdir().unwrap();
	let turns = vec![
		ScriptedTurn::tools(
			Some("Gain staging and a master limiter."),
			&[(
				"apply_mix_moves",
				r#"{"moves":[{"move":"limiter","ceiling_db":-1.5,"input_gain_db":3},{"move":"eq","kind":"high_shelf","freq_hz":8000,"gain_db":2}]}"#,
			)],
		),
		ScriptedTurn::text("Done: louder, peaks under the ceiling, a touch more air."),
	];
	let mut a = chat_app(turns, 30, tmp.path());
	a.state.view.panels.focus = Some(PanelKind::Arrange);
	let mut h = screenshot_harness(a);
	h.run_steps(2);
	h.key_press(Key::F10);
	h.run_steps(2);
	h.get_by_label("MIX").click();
	shot(&mut h, &dir, "mix_empty");
	h.get_by_label("Analyze").click();
	h.run_steps(2);
	shot(&mut h, &dir, "mix_analyzing");
	settle_mix(&mut h);
	shot(&mut h, &dir, "mix_report");
	// Scroll to the issues.
	h.hover_at(pos2(1450.0, 600.0));
	for _ in 0..20 {
		h.event(egui::Event::MouseWheel {
			unit: egui::MouseWheelUnit::Point,
			delta: vec2(0.0, -60.0),
			phase: egui::TouchPhase::Move,
			modifiers: Modifiers::NONE,
		});
		h.run_steps(1);
	}
	shot(&mut h, &dir, "mix_issues");
	h.remove_cursor();
	h.get_by_label("AI mix pass").click();
	h.run_steps(3);
	shot(&mut h, &dir, "mix_pass_running");
	settle_mix(&mut h);
	shot(&mut h, &dir, "mix_pass_review");
	// The browser's Files tab (its ◆ bookmarks glyph comes from the fallback font).
	h.get_by_label("Files").click();
	shot(&mut h, &dir, "files_tab_glyphs");
}

#[test]
#[ignore]
fn render_phase19_screens() {
	let Some(dir) = std::env::var_os("CADENZA_SCREENSHOT_DIR").map(PathBuf::from) else {
		return;
	};
	std::fs::create_dir_all(&dir).unwrap();
	let mut h = screenshot_harness(app());
	h.run_steps(2);
	// A frozen track edited afterwards (amber FRZ! on its header and mixer strip), a frozen
	// one that is current, and the xrun indicator after a few dropouts.
	{
		let s = &mut h.state_mut().state;
		let p = s.project().clone();
		for name in ["Keys", "Bass"] {
			let t = p.tracks.iter().find(|t| t.name == name).unwrap();
			let source = p.freeze_source(t.id);
			assert!(s.apply(cadenza_model::Command::SetTrackFreeze {
				track: t.id,
				freeze: Some(cadenza_model::Freeze {
					file: "/nonexistent/freeze.wav".into(),
					source,
				}),
			}));
		}
		let keys = p.tracks.iter().find(|t| t.name == "Keys").unwrap();
		assert!(s.apply(cadenza_model::Command::AddNotes {
			clip: keys.clips[0].id,
			notes: vec![cadenza_model::Note::new(72, Tick(0), Tick(PPQ), 0.7)],
		}));
		s.xruns.update(4, Vec::new(), std::time::Instant::now());
	}
	shot(&mut h, &dir, "freeze_stale_and_xruns");
}
