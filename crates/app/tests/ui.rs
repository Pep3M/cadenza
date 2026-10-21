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
	PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/first_light.cdz")
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
	// The inspector is hidden by default (floating windows show items): F8 shows it.
	assert!(!visible(&h, PanelKind::Browser) && visible(&h, PanelKind::Inspector));
	h.key_press(Key::F8);
	h.run_steps(2);
	assert!(!visible(&h, PanelKind::Inspector));
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
	let empty = note_pos(&h, clip, 3.5, 66);
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
	a.state.view.panels.set(PanelKind::Inspector, true);
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
	// As imports store it (macOS temp folders resolve to /private/var).
	let wav = cadenza_core::paths::real_path(tone_wav(dir.path())).unwrap();
	let mut a = app();
	// Saved in the temp dir, so processed copies land in "song audio" there.
	assert!(a.state.save_as(dir.path().join("song.cdz")));
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
	// The track number (the instrument line opens the instrument's device editor).
	let at = pos2(
		lane.left() - cadenza_app::theme::HEADER_WIDTH + 20.0,
		lane.top() + 14.0,
	);
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
	assert!(!s.view.device_editor);
	// The instrument name: its device editor.
	h.run_steps(60);
	let lane = h.state().track_rect(0).unwrap();
	double_click(&mut h, pos2(lane.left() - 150.0, lane.top() + 30.0));
	let s = &h.state().state;
	assert_eq!(
		s.view.windows.focused_device().map(|r| r.device),
		s.project().tracks[0].instrument.as_ref().map(|d| d.id),
		"the instrument opened in its floating window"
	);
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

/// An app with an empty instrument track "Lead" on top (room to draw clips).
fn lead_app() -> CadenzaApp {
	let mut a = app();
	a.state.view.panels.set(PanelKind::Mixer, false);
	let track = cadenza_model::Track::new("Lead", cadenza_model::TrackKind::Instrument);
	assert!(a.state.apply(cadenza_model::Command::AddTrack {
		track,
		index: Some(0),
	}));
	a
}

fn lead_clips(h: &Harness<'_, CadenzaApp>) -> Vec<(Tick, Tick)> {
	let mut v: Vec<_> = h.state().state.project().tracks[0]
		.clips
		.iter()
		.map(|c| (c.start, c.end()))
		.collect();
	v.sort();
	v
}

#[test]
fn the_draw_tool_draws_clips_on_an_instrument_lane_in_one_step_each() {
	use cadenza_app::state::arrange_tool::ArrangeTool;
	let mut h = harness(lead_app());
	h.run_steps(3);
	h.key_press(Key::D);
	h.run_steps(1);
	assert_eq!(h.state().state.view.arrange_tool, ArrangeTool::Draw);
	let lane = h.state().track_rect(0).unwrap();
	let tl = h.state().state.view.timeline;
	let x = |beat: f64| lane.left() + tl.beat_to_x(beat);
	let y = lane.center().y;
	let depth = h.state().state.document().history_labels().len();
	// Drag from beat 4.3 to 10.2: a clip covering the cells touched.
	h.hover_at(pos2(x(4.3), y));
	h.drag_at(pos2(x(4.3), y));
	h.run_steps(1);
	h.hover_at(pos2(x(8.0), y));
	h.run_steps(1);
	h.hover_at(pos2(x(10.2), y));
	h.run_steps(1);
	h.drop_at(pos2(x(10.2), y));
	h.run_steps(2);
	let clips = lead_clips(&h);
	assert_eq!(clips.len(), 1, "{clips:?}");
	let (s, e) = clips[0];
	assert!(
		s <= Tick::from_beats(4.3) && s >= Tick::from_beats(4.0),
		"{clips:?}"
	);
	assert!(
		e >= Tick::from_beats(10.2) && e <= Tick::from_beats(10.5),
		"{e:?}"
	);
	assert_eq!(h.state().state.document().history_labels().len(), depth + 1);
	// A click draws one bar from the grid cell under the pointer.
	h.hover_at(pos2(x(20.1), y));
	h.drag_at(pos2(x(20.1), y));
	h.drop_at(pos2(x(20.1), y));
	h.run_steps(2);
	let clips = lead_clips(&h);
	assert_eq!(clips.len(), 2, "{clips:?}");
	assert_eq!(clips[1].1 - clips[1].0, Tick(PPQ * 4));
	assert_eq!(h.state().state.document().history_labels().len(), depth + 2);
	h.key_press_modifiers(Modifiers::COMMAND, Key::Z);
	h.run_steps(2);
	assert_eq!(lead_clips(&h).len(), 1);
	// Select again (V): a drag on the empty lane is a rubber band, not a clip.
	h.key_press(Key::V);
	h.run_steps(1);
	h.drag_at(pos2(x(24.0), y));
	h.run_steps(1);
	h.hover_at(pos2(x(28.0), y));
	h.run_steps(1);
	h.drop_at(pos2(x(28.0), y));
	h.run_steps(2);
	assert_eq!(lead_clips(&h).len(), 1);
}

#[test]
fn the_split_tool_splits_a_clip_at_the_pointer_in_one_step() {
	use cadenza_app::state::arrange_tool::ArrangeTool;
	let mut a = lead_app();
	let track = a.state.project().tracks[0].id;
	assert!(a.state.apply(cadenza_model::Command::AddClip {
		track,
		clip: cadenza_model::Clip::midi("Riff", Tick(PPQ * 4), Tick(PPQ * 8)),
	}));
	let mut h = harness(a);
	h.run_steps(3);
	h.get_by_label("✂ Split").click();
	h.run_steps(2);
	assert_eq!(h.state().state.view.arrange_tool, ArrangeTool::Split);
	let lane = h.state().track_rect(0).unwrap();
	let tl = h.state().state.view.timeline;
	let at = pos2(lane.left() + tl.beat_to_x(7.0), lane.center().y);
	let depth = h.state().state.document().history_labels().len();
	h.hover_at(at);
	h.run_steps(1);
	h.drag_at(at);
	h.drop_at(at);
	h.run_steps(2);
	assert_eq!(
		lead_clips(&h),
		vec![
			(Tick(PPQ * 4), Tick(PPQ * 7)),
			(Tick(PPQ * 7), Tick(PPQ * 12))
		]
	);
	assert_eq!(h.state().state.document().history_labels().len(), depth + 1);
	assert_eq!(h.state().state.undo_label(), Some("Split clip"));
	h.key_press_modifiers(Modifiers::COMMAND, Key::Z);
	h.run_steps(2);
	assert_eq!(lead_clips(&h).len(), 1);
}

/// Two instrument tracks on top ("Keys": clip A at bar 2, "Pad": clip P at bar 2), the
/// mixer hidden.
fn playlist_app() -> CadenzaApp {
	let mut a = app();
	a.state.view.panels.set(PanelKind::Mixer, false);
	for (i, (name, clip)) in [("Keys", "A"), ("Pad", "P")].into_iter().enumerate() {
		let mut track = cadenza_model::Track::new(name, cadenza_model::TrackKind::Instrument);
		track.clips.push(cadenza_model::Clip::midi(
			clip,
			Tick(PPQ * 4),
			Tick(PPQ * 4),
		));
		assert!(a.state.apply(cadenza_model::Command::AddTrack {
			track,
			index: Some(i),
		}));
	}
	a
}

fn clips_of(h: &Harness<'_, CadenzaApp>, track: usize) -> Vec<(Tick, Tick)> {
	let mut v: Vec<_> = h.state().state.project().tracks[track]
		.clips
		.iter()
		.map(|c| (c.start, c.end()))
		.collect();
	v.sort();
	v
}

fn depth(h: &Harness<'_, CadenzaApp>) -> usize {
	h.state().state.document().history_labels().len()
}

/// Screen position of `beat` on track `index`'s lane (vertical centre).
fn lane_at(h: &Harness<'_, CadenzaApp>, index: usize, beat: f64) -> egui::Pos2 {
	let lane = h.state().track_rect(index).unwrap();
	let tl = h.state().state.view.timeline;
	pos2(lane.left() + tl.beat_to_x(beat), lane.center().y)
}

fn click(h: &mut Harness<'_, CadenzaApp>, at: egui::Pos2) {
	h.hover_at(at);
	h.run_steps(1);
	h.drag_at(at);
	h.run_steps(1);
	h.drop_at(at);
	h.run_steps(2);
}

fn drag(h: &mut Harness<'_, CadenzaApp>, from: egui::Pos2, to: egui::Pos2) {
	h.hover_at(from);
	h.run_steps(1);
	h.drag_at(from);
	h.run_steps(1);
	h.hover_at(from.lerp(to, 0.5));
	h.run_steps(1);
	h.hover_at(to);
	h.run_steps(1);
	h.drop_at(to);
	h.run_steps(2);
}

fn right_drag(h: &mut Harness<'_, CadenzaApp>, from: egui::Pos2, to: egui::Pos2) {
	let button = |h: &mut Harness<'_, CadenzaApp>, pos: egui::Pos2, pressed: bool| {
		h.event(egui::Event::PointerButton {
			pos,
			button: egui::PointerButton::Secondary,
			pressed,
			modifiers: Modifiers::NONE,
		});
	};
	h.hover_at(from);
	h.run_steps(1);
	button(h, from, true);
	h.run_steps(1);
	h.hover_at(from.lerp(to, 0.5));
	h.run_steps(1);
	h.hover_at(to);
	h.run_steps(1);
	button(h, to, false);
	h.run_steps(2);
}

#[test]
fn clips_copy_cut_and_paste_with_ctrl_c_x_v_in_one_step_each() {
	let mut h = harness(playlist_app());
	h.run_steps(3);
	let p = lane_at(&h, 0, 5.0);
	click(&mut h, p);
	assert_eq!(h.state().state.selection.clips.len(), 1);
	let d = depth(&h);
	h.key_press_modifiers(Modifiers::COMMAND, Key::C);
	h.run_steps(1);
	assert_eq!(depth(&h), d, "copy is not an edit");
	// Right after the copied span, then appending.
	h.key_press_modifiers(Modifiers::COMMAND, Key::V);
	h.run_steps(2);
	h.key_press_modifiers(Modifiers::COMMAND, Key::V);
	h.run_steps(2);
	let bar = |n: i64| Tick(PPQ * 4 * n);
	assert_eq!(
		clips_of(&h, 0),
		vec![(bar(1), bar(2)), (bar(2), bar(3)), (bar(3), bar(4))]
	);
	assert_eq!(depth(&h), d + 2);
	assert_eq!(h.state().state.undo_label(), Some("Add clip 'A'"));
	// Over the Pad lane: onto the track under the pointer.
	h.hover_at(lane_at(&h, 1, 30.0));
	h.run_steps(1);
	h.key_press_modifiers(Modifiers::COMMAND, Key::V);
	h.run_steps(2);
	assert_eq!(clips_of(&h, 1).len(), 2);
	// Cut: the selected (pasted) clip goes away in one step.
	let d = depth(&h);
	h.key_press_modifiers(Modifiers::COMMAND, Key::X);
	h.run_steps(2);
	assert_eq!(clips_of(&h, 1).len(), 1);
	assert_eq!(depth(&h), d + 1);
}

#[test]
fn ctrl_b_clones_the_selection_after_itself_and_keeps_appending() {
	let mut h = harness(playlist_app());
	h.run_steps(3);
	let p = lane_at(&h, 0, 5.0);
	click(&mut h, p);
	let d = depth(&h);
	for _ in 0..2 {
		h.key_press_modifiers(Modifiers::COMMAND, Key::B);
		h.run_steps(2);
	}
	let bar = |n: i64| Tick(PPQ * 4 * n);
	assert_eq!(
		clips_of(&h, 0),
		vec![(bar(1), bar(2)), (bar(2), bar(3)), (bar(3), bar(4))]
	);
	assert_eq!(depth(&h), d + 2, "one step each");
}

#[test]
fn the_paint_tool_repeats_the_clip_along_the_lane_in_one_step() {
	use cadenza_app::state::arrange_tool::ArrangeTool;
	let mut h = harness(playlist_app());
	h.run_steps(3);
	h.key_press(Key::B);
	h.run_steps(1);
	assert_eq!(h.state().state.view.arrange_tool, ArrangeTool::Paint);
	let d = depth(&h);
	// From the clip itself to bar 5: bars 3..5 get copies (bar 2 is the clip).
	let (a, b) = (lane_at(&h, 0, 4.5), lane_at(&h, 0, 17.0));
	drag(&mut h, a, b);
	let bar = |n: i64| Tick(PPQ * 4 * n);
	assert_eq!(
		clips_of(&h, 0),
		vec![
			(bar(1), bar(2)),
			(bar(2), bar(3)),
			(bar(3), bar(4)),
			(bar(4), bar(5))
		]
	);
	assert_eq!(depth(&h), d + 1);
	assert_eq!(h.state().state.undo_label(), Some("Paint clips"));
	// Right-drag deletes every clip swept over (one step).
	let (a, b) = (lane_at(&h, 0, 9.0), lane_at(&h, 0, 17.0));
	right_drag(&mut h, a, b);
	assert_eq!(clips_of(&h, 0), vec![(bar(1), bar(2))]);
	assert_eq!(depth(&h), d + 2);
}

#[test]
fn the_knife_cuts_every_track_it_crosses_in_one_step() {
	let mut h = harness(playlist_app());
	h.run_steps(3);
	h.key_press(Key::C);
	h.run_steps(1);
	let d = depth(&h);
	let (a, b) = (lane_at(&h, 0, 6.0), lane_at(&h, 1, 6.0));
	drag(&mut h, a, b);
	let cut = vec![
		(Tick(PPQ * 4), Tick(PPQ * 6)),
		(Tick(PPQ * 6), Tick(PPQ * 8)),
	];
	assert_eq!(clips_of(&h, 0), cut);
	assert_eq!(clips_of(&h, 1), cut);
	assert_eq!(depth(&h), d + 1);
	assert_eq!(h.state().state.undo_label(), Some("Slice clips"));
	// A click still splits one clip.
	let p = lane_at(&h, 0, 7.0);
	click(&mut h, p);
	assert_eq!(clips_of(&h, 0).len(), 3);
	assert_eq!(depth(&h), d + 2);
}

#[test]
fn the_mute_tool_mutes_and_unmutes_clips_in_one_step() {
	let mut h = harness(playlist_app());
	h.run_steps(3);
	h.key_press(Key::T);
	h.run_steps(1);
	let d = depth(&h);
	let muted =
		|h: &Harness<'_, CadenzaApp>, t: usize| h.state().state.project().tracks[t].clips[0].muted;
	// A sweep from Keys down to Pad mutes both.
	let (a, b) = (lane_at(&h, 0, 5.0), lane_at(&h, 1, 5.0));
	drag(&mut h, a, b);
	assert!(muted(&h, 0) && muted(&h, 1));
	assert_eq!(depth(&h), d + 1);
	let p = lane_at(&h, 1, 6.0);
	click(&mut h, p);
	assert!(muted(&h, 0) && !muted(&h, 1));
	assert_eq!(h.state().state.undo_label(), Some("Unmute clip"));
	h.key_press_modifiers(Modifiers::COMMAND, Key::Z);
	h.run_steps(2);
	assert!(muted(&h, 1));
}

#[test]
fn dragging_a_header_bottom_edge_resizes_that_track() {
	let mut h = app_harness(playlist_app(), vec2(1600.0, 1000.0), false);
	h.run_steps(3);
	let lane = h.state().track_rect(0).unwrap();
	let id = h.state().state.project().tracks[0].id;
	let before = h.state().state.view.height_of(id);
	let edge = pos2(lane.left() - 120.0, lane.bottom() - 1.5);
	let d = depth(&h);
	drag(&mut h, edge, edge + vec2(0.0, 40.0));
	let after = h.state().state.view.height_of(id);
	assert!((after - before - 40.0).abs() < 2.0, "{before} -> {after}");
	let other = h.state().state.project().tracks[1].id;
	assert_eq!(
		h.state().state.view.height_of(other),
		before,
		"only that track"
	);
	assert_eq!(depth(&h), d, "view state, not an edit");
	let lane = h.state().track_rect(0).unwrap();
	assert!((lane.height() - after).abs() < 0.5);
	// Double-click resets it.
	let edge = pos2(lane.left() - 120.0, lane.bottom() - 1.5);
	double_click(&mut h, edge);
	assert_eq!(h.state().state.view.height_of(id), before);
}

/// `playlist_app` with notes in clip A (Keys): C4 on beat 1, E4 on beat 3.
fn linked_app() -> CadenzaApp {
	let mut a = playlist_app();
	let clip = a.state.project().tracks[0].clips[0].id;
	assert!(a.state.apply(cadenza_model::Command::AddNotes {
		clip,
		notes: vec![
			cadenza_model::Note::new(60, Tick(0), Tick(PPQ), 0.8),
			cadenza_model::Note::new(64, Tick(PPQ * 2), Tick(PPQ), 0.8),
		],
	}));
	a
}

fn clip_ids(h: &Harness<'_, CadenzaApp>, track: usize) -> Vec<cadenza_core::ClipId> {
	let mut c: Vec<_> = h.state().state.project().tracks[track]
		.clips
		.iter()
		.map(|c| (c.start, c.id))
		.collect();
	c.sort();
	c.into_iter().map(|(_, id)| id).collect()
}

fn pitches(h: &Harness<'_, CadenzaApp>, clip: cadenza_core::ClipId) -> Vec<u8> {
	h.state()
		.state
		.project()
		.find_clip(clip)
		.unwrap()
		.1
		.notes()
		.iter()
		.map(|n| n.pitch)
		.collect()
}

#[test]
fn linked_paste_shares_note_edits_and_make_unique_splits_them() {
	let mut h = harness(linked_app());
	h.run_steps(3);
	let p = lane_at(&h, 0, 5.0);
	click(&mut h, p);
	h.key_press_modifiers(Modifiers::COMMAND, Key::C);
	h.run_steps(1);
	h.key_press_modifiers(Modifiers::COMMAND, Key::V);
	h.run_steps(2);
	let ids = clip_ids(&h, 0);
	assert_eq!(ids.len(), 2);
	let (a, b) = (ids[0], ids[1]);
	assert_eq!(
		cadenza_model::links::link_count(h.state().state.project(), a),
		2
	);
	// Editing a note of the copy edits the original too (one step).
	let note = h.state().state.project().find_clip(b).unwrap().1.notes()[0].id;
	let d = depth(&h);
	assert!(
		h.state_mut()
			.state
			.apply(cadenza_model::Command::UpdateNotes {
				clip: b,
				notes: vec![cadenza_model::NotePatch {
					id: note,
					pitch: Some(62),
					velocity: None,
					start: None,
					length: None,
				}],
			})
	);
	h.run_steps(2);
	assert_eq!(pitches(&h, a), vec![62, 64]);
	assert_eq!(depth(&h), d + 1);
	// Shift+Ctrl+V pastes an independent copy.
	h.key_press_modifiers(Modifiers::COMMAND | Modifiers::SHIFT, Key::V);
	h.run_steps(2);
	let c = *clip_ids(&h, 0).last().unwrap();
	assert_eq!(
		cadenza_model::links::link_count(h.state().state.project(), c),
		1
	);
	// Clip menu "Make unique" on the copy (Select tool: right-click opens the menu).
	let p = lane_at(&h, 0, 9.0);
	click(&mut h, p);
	right_button(&h, p, true);
	h.run_steps(1);
	right_button(&h, p, false);
	h.run_steps(2);
	h.get_by_label("Make unique  (2 linked)").click();
	h.run_steps(2);
	assert_eq!(h.state().state.undo_label(), Some("Make clip unique"));
	assert_eq!(
		cadenza_model::links::link_count(h.state().state.project(), b),
		1
	);
	assert!(
		h.state_mut()
			.state
			.apply(cadenza_model::Command::UpdateNotes {
				clip: b,
				notes: vec![cadenza_model::NotePatch {
					id: note,
					pitch: Some(67),
					velocity: None,
					start: None,
					length: None,
				}],
			})
	);
	assert_eq!(pitches(&h, a), vec![62, 64]);
}

#[test]
fn chop_and_shift_stretch_edit_a_clip_in_one_step_each() {
	let mut h = harness(linked_app());
	h.run_steps(3);
	let a = clip_ids(&h, 0)[0];
	// Shift+drag the right edge from bar 3 to bar 4: the notes double in length.
	let lane = h.state().track_rect(0).unwrap();
	let tl = h.state().state.view.timeline;
	let edge = pos2(lane.left() + tl.beat_to_x(8.0) - 2.0, lane.center().y);
	let to = pos2(lane.left() + tl.beat_to_x(12.0) - 2.0, lane.center().y);
	h.event(egui::Event::ModifiersChanged(Modifiers::SHIFT));
	let d = depth(&h);
	drag(&mut h, edge, to);
	h.event(egui::Event::ModifiersChanged(Modifiers::NONE));
	h.run_steps(1);
	assert_eq!(depth(&h), d + 1);
	assert_eq!(h.state().state.undo_label(), Some("Stretch clip"));
	let (_, clip) = h.state().state.project().find_clip(a).unwrap();
	assert_eq!(clip.length, Tick(PPQ * 8));
	let starts: Vec<Tick> = clip.notes().iter().map(|n| n.start).collect();
	assert_eq!(starts, vec![Tick(0), Tick(PPQ * 4)]);
	// Chop into 4 parts from the clip menu.
	let p = lane_at(&h, 0, 5.0);
	click(&mut h, p);
	right_button(&h, p, true);
	h.run_steps(1);
	right_button(&h, p, false);
	h.run_steps(2);
	assert!(h.query_by_label("Make unique").is_some(), "clip menu open");
	h.get_by_label_contains("Chop into").hover();
	h.run_steps(3);
	h.get_by_label("4 parts").click();
	h.run_steps(2);
	assert_eq!(h.state().state.undo_label(), Some("Chop clip"));
	let bar = |n: i64| Tick(PPQ * 4 * n);
	assert_eq!(
		clips_of(&h, 0),
		vec![
			(bar(1), Tick(PPQ * 6)),
			(Tick(PPQ * 6), bar(2)),
			(bar(2), Tick(PPQ * 10)),
			(Tick(PPQ * 10), bar(3)),
		]
	);
	assert_eq!(depth(&h), d + 2);
}

/// Linked clips (link glyph, a selected copy marking its siblings), a chopped clip and a
/// clip resized past its content (the pattern repeats).
#[test]
#[ignore]
fn render_linked_clip_screens() {
	let Some(dir) = std::env::var_os("CADENZA_SCREENSHOT_DIR").map(PathBuf::from) else {
		return;
	};
	std::fs::create_dir_all(&dir).unwrap();
	let mut a = linked_app();
	{
		let s = &mut a.state;
		let pad = s.project().tracks[1].clips[0].id;
		let notes: Vec<cadenza_model::Note> = (0..8)
			.map(|i| {
				cadenza_model::Note::new(
					[60, 64, 67, 72][i % 4],
					Tick(PPQ / 2 * i as i64),
					Tick(PPQ / 2),
					0.8,
				)
			})
			.collect();
		assert!(s.apply(cadenza_model::Command::AddNotes { clip: pad, notes }));
		// Pad: resized from 1 to 4 bars: its bar repeats.
		let cmd =
			cadenza_app::state::playlist::resize_repeating(s.project(), pad, Tick(PPQ * 4 * 5))
				.unwrap();
		assert!(s.apply(cmd));
		// Keys: A cloned linked twice.
		let keys_a = s.project().tracks[0].clips[0].id;
		let (cmd, ids) =
			cadenza_app::state::playlist::clone_clips(s.project(), &[keys_a], None, true).unwrap();
		assert!(s.apply(cmd));
		let (cmd, _) =
			cadenza_app::state::playlist::clone_clips(s.project(), &ids, None, true).unwrap();
		assert!(s.apply(cmd));
		s.selection.clips = [keys_a].into_iter().collect();
	}
	let mut h = screenshot_harness(a);
	h.run_steps(2);
	shot(&mut h, &dir, "linked_clips");
	// Chop the last Keys copy into 4 parts.
	let last = *clip_ids(&h, 0).last().unwrap();
	cadenza_app::ui::chop_clip(
		&mut h.state_mut().state,
		last,
		cadenza_app::state::playlist::Chop::Parts(4),
	);
	h.run_steps(2);
	shot(&mut h, &dir, "linked_chopped");
}

/// FL playlist editing: painted repeats, a muted clip, a taller track and the knife.
#[test]
#[ignore]
fn render_playlist_screens() {
	use cadenza_app::state::arrange_tool::ArrangeTool;
	let Some(dir) = std::env::var_os("CADENZA_SCREENSHOT_DIR").map(PathBuf::from) else {
		return;
	};
	std::fs::create_dir_all(&dir).unwrap();
	let mut a = playlist_app();
	{
		let s = &mut a.state;
		let keys = s.project().tracks[0].id;
		let pad = s.project().tracks[1].id;
		s.view.set_track_height(pad, 120.0);
		let clip = s.project().tracks[1].clips[0].id;
		let notes: Vec<cadenza_model::Note> = (0..8)
			.map(|i| {
				cadenza_model::Note::new(
					[60, 64, 67, 72][i % 4],
					Tick(PPQ / 2 * i as i64),
					Tick(PPQ / 2),
					0.8,
				)
			})
			.collect();
		assert!(s.apply(cadenza_model::Command::AddNotes { clip, notes }));
		let a_clip = s.project().tracks[0].clips[0].id;
		assert!(s.apply(cadenza_model::Command::UpdateClip {
			clip: a_clip,
			patch: cadenza_model::ClipPatch {
				muted: Some(true),
				..Default::default()
			},
		}));
		let _ = keys;
	}
	let mut h = screenshot_harness(a);
	h.run_steps(2);
	// Paint repeats of the Pad clip along its lane.
	h.state_mut().state.view.arrange_tool = ArrangeTool::Paint;
	h.run_steps(1);
	let (a, b) = (lane_at(&h, 1, 4.5), lane_at(&h, 1, 21.0));
	drag(&mut h, a, b);
	shot(&mut h, &dir, "playlist_paint");
	// The knife mid-drag across both tracks.
	h.state_mut().state.view.arrange_tool = ArrangeTool::Split;
	h.run_steps(1);
	let (from, to) = (lane_at(&h, 0, 10.0), lane_at(&h, 1, 14.0));
	h.hover_at(from);
	h.run_steps(1);
	h.drag_at(from);
	h.run_steps(1);
	h.hover_at(from.lerp(to, 0.5));
	h.run_steps(1);
	h.hover_at(to);
	h.run_steps(1);
	shot(&mut h, &dir, "playlist_knife");
	h.drop_at(to);
	h.run_steps(2);
	shot(&mut h, &dir, "playlist_sliced");
}

/// The arrange tools, ruler, loop region, markers and playhead (design 02).
#[test]
#[ignore]
fn render_arrange_tool_screens() {
	use cadenza_app::state::arrange_tool::ArrangeTool;
	let Some(dir) = std::env::var_os("CADENZA_SCREENSHOT_DIR").map(PathBuf::from) else {
		return;
	};
	std::fs::create_dir_all(&dir).unwrap();
	let mut a = lead_app();
	{
		let s = &mut a.state;
		s.set_loop_range(Some((Tick(PPQ * 8), Tick(PPQ * 24))));
		s.view.transport.loop_on = true;
		s.seek(Tick(PPQ * 13));
		s.view.automation.tempo = true;
		let track = s.project().tracks[0].id;
		assert!(s.apply(cadenza_model::Command::AddClip {
			track,
			clip: cadenza_model::Clip::midi("Riff", Tick(PPQ * 4), Tick(PPQ * 12)),
		}));
	}
	let mut h = screenshot_harness(a);
	shot(&mut h, &dir, "arrange_select");
	h.state_mut().state.view.automation.tempo = false;
	shot(&mut h, &dir, "arrange_ruler");

	// Draw: a clip being dragged out on the Lead lane.
	h.state_mut().state.view.arrange_tool = ArrangeTool::Draw;
	h.run_steps(2);
	let lane = h.state().track_rect(0).unwrap();
	let tl = h.state().state.view.timeline;
	let x = |beat: f64| lane.left() + tl.beat_to_x(beat);
	let y = lane.center().y;
	h.hover_at(pos2(x(20.2), y));
	h.drag_at(pos2(x(20.2), y));
	h.run_steps(1);
	h.hover_at(pos2(x(29.5), y));
	shot(&mut h, &dir, "arrange_draw");
	h.drop_at(pos2(x(29.5), y));
	h.run_steps(2);

	// Split: the cut line over a clip.
	h.state_mut().state.view.arrange_tool = ArrangeTool::Split;
	h.run_steps(1);
	h.hover_at(pos2(x(9.0), y));
	shot(&mut h, &dir, "arrange_split");
}

/// Renders a few app states to PNG files in `$CADENZA_SCREENSHOT_DIR` (needs a GPU):
/// A state whose AI chat answers from `turns` (scripted provider, in-memory key, no
/// network).
fn chat_app(
	turns: Vec<cadenza_agent::scripted::ScriptedTurn>,
	delay_ms: u64,
	dir: &std::path::Path,
) -> CadenzaApp {
	with_chat(app(), turns, delay_ms, dir)
}

/// `a` with its AI chat answering from `turns` (see [`chat_app`]).
fn with_chat(
	mut a: CadenzaApp,
	turns: Vec<cadenza_agent::scripted::ScriptedTurn>,
	delay_ms: u64,
	dir: &std::path::Path,
) -> CadenzaApp {
	use cadenza_app::state::ai::secrets::{MemorySecretStore, SecretStore};
	use cadenza_app::state::ai::settings::ProviderKind;
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
	assert!(
		h.query_by_label_contains("Undo step: AI: add a bass")
			.is_some()
	);

	// One Ctrl+Z takes the request back.
	h.key_press_modifiers(Modifiers::COMMAND, Key::Z);
	h.run_steps(2);
	assert_eq!(h.state().state.project().tracks.len(), tracks);
}

/// The mixer extras (design 03): master live loudness, Mono + Dim, the output menu with
/// sidechain targets, Peak / Peak+RMS meters. Levels come from the engine's virtual device.
#[test]
#[ignore]
fn render_mixer_extra_screens() {
	let Some(dir) = std::env::var_os("CADENZA_SCREENSHOT_DIR").map(PathBuf::from) else {
		return;
	};
	std::fs::create_dir_all(&dir).unwrap();
	let mut a = app();
	let (drums, bass, bass_comp) = {
		let p = a.state.project();
		let d = p.tracks.iter().find(|t| t.name == "Drums").unwrap().id;
		let b = p.tracks.iter().find(|t| t.name == "Bass").unwrap();
		(d, b.id, b.channel.inserts[1].id)
	};
	// The bass compressor is already keyed by the drums (checked in the menu).
	assert!(a.state.apply(cadenza_model::Command::SetSidechain {
		track: Some(bass),
		device: bass_comp,
		source: Some(cadenza_model::SidechainSource {
			track: drums,
			pre_fader: false,
		}),
	}));
	a.state.view.panels.toggle_maximize_panel(PanelKind::Mixer);
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
	let (mut player, mut dev) = cadenza_engine::Player::virtual_device(48_000, 0);
	player.load(&project).unwrap();
	player.seek(Tick(PPQ * 32)).unwrap();
	player.play().unwrap();
	h.state_mut()
		.state
		.attach_meters(player.meters(), Some(player.master_tap()));
	let mut out = vec![0.0f32; 512];
	// About four seconds of the song: short-term and integrated loudness settle.
	let mut play = |h: &mut Harness<'_, CadenzaApp>, blocks: usize| {
		for i in 0..blocks {
			dev.process(&mut out, &[]);
			if i % 16 == 15 {
				h.state_mut().state.master_loudness.update(true, 0.0);
			}
		}
		h.state_mut().state.master_loudness.update(true, 0.0);
	};
	play(&mut h, 760);
	let shot = |h: &mut Harness<'_, CadenzaApp>, name: &str| {
		h.run_steps(1);
		h.render()
			.unwrap()
			.save(dir.join(format!("{name}.png")))
			.unwrap();
	};
	h.state_mut()
		.state
		.set_monitor_fx(cadenza_engine::MonitorFx {
			mono: true,
			dim: true,
		});
	shot(&mut h, "mixer_extras_peak_rms");
	assert!(h.state().state.master_loudness.meter.integrated() > -40.0);
	h.state_mut().state.view.mixer.meter = cadenza_app::state::mixer::MeterMode::Peak;
	play(&mut h, 20);
	shot(&mut h, "mixer_extras_peak");
	h.state_mut().state.view.mixer.meter = cadenza_app::state::mixer::MeterMode::PeakRms;
	h.state_mut().state.view.mixer.narrow = true;
	play(&mut h, 20);
	shot(&mut h, "mixer_extras_narrow");
	h.state_mut().state.view.mixer.narrow = false;
	// The drums' output menu: outputs, then the sidechain targets.
	play(&mut h, 20);
	h.run_steps(1);
	let combos = h.get_all_by_value("→ Master").collect::<Vec<_>>();
	let order: Vec<cadenza_core::TrackId> = h
		.state()
		.state
		.project()
		.tracks
		.iter()
		.filter(|t| t.kind != cadenza_model::TrackKind::Bus)
		.map(|t| t.id)
		.collect();
	let at = order.iter().position(|t| *t == drums).unwrap();
	combos[at].click();
	h.run_steps(2);
	shot(&mut h, "mixer_extras_output_menu");
	assert!(
		h.query_by_label_contains("Sidechain → Bass · Compressor")
			.is_some()
	);
	assert!(
		h.query_by_label_contains("Sidechain → Master · Compressor")
			.is_some()
	);
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
	h.state_mut()
		.state
		.view
		.panels
		.toggle_maximize_panel(PanelKind::Mixer);
	shot(&mut h, "mixer_maximized");
	h.state_mut().state.view.mixer.narrow = true;
	shot(&mut h, "mixer_narrow");
	h.state_mut().state.view.mixer.narrow = false;
	h.state_mut()
		.state
		.view
		.panels
		.toggle_maximize_panel(PanelKind::Mixer);

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
			cadenza_app::state::edits::copy_clips(s.project(), &[clip], Tick(PPQ * 2), None, false)
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
	assert!(state.open_device_in_dock(refs[0]));
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
	let mut h = app_harness(a, vec2(1600.0, 1000.0), false);
	h.run_steps(3);
	let grid = h.state().pad_grid().expect("pad grid drawn");
	// Pad 1 (kick) sits bottom-left, pad 2 (snare) to its right.
	let kick = center_of(&h, "Kick pad");
	let snare = center_of(&h, "Snare pad");
	assert!(kick.x < snare.x && (kick.y - snare.y).abs() < 1.0);
	assert!(kick.y > grid.center().y && kick.x < grid.center().x);
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
	// The Gain knob of the selected pad (right of its waveform): one drag = one undo step.
	let wave = h.state().instrument_wave().expect("pad waveform drawn");
	let knob = h
		.get_all_by_label("Gain")
		.map(|n| n.rect())
		.find(|r| r.left() > wave.right())
		.expect("the pad's gain knob")
		.center();
	let knob = pos2(knob.x as f32, knob.y as f32);
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

/// The centre of the widget `label` in the device editor dock (the inspector may show
/// the same device's compact form too).
fn center_of(h: &Harness<'_, CadenzaApp>, label: &str) -> egui::Pos2 {
	let body = h.state().device_body_rect().expect("device editor drawn");
	h.get_all_by_label(label)
		.map(|n| n.rect().center())
		.find(|c| body.contains(*c))
		.expect(label)
}

fn pad_stem(state: &AppState, r: DeviceRef, note: u8) -> String {
	state
		.device(r)
		.unwrap()
		.zones
		.iter()
		.find(|z| z.key_low == note)
		.map(|z| z.sample.stem())
		.unwrap_or_default()
}

#[test]
fn dragging_a_drum_sampler_pad_onto_another_swaps_them_in_one_undo_step() {
	let (a, drums, _) = sampler_app();
	let mut h = app_harness(a, vec2(1600.0, 1000.0), false);
	h.run_steps(3);
	let before = h.state().state.document().history_labels().len();
	let (kick, snare) = (
		pad_stem(&h.state().state, drums, 36),
		pad_stem(&h.state().state, drums, 38),
	);
	assert!(!kick.is_empty() && kick != snare);
	let from = center_of(&h, "Kick pad");
	let to = center_of(&h, "Snare pad");
	let d = to - from;
	drag_by(&mut h, from, &[d * 0.25, d * 0.6, d]);
	let s = &h.state().state;
	assert_eq!(
		s.document().history_labels().len(),
		before + 1,
		"one undo step"
	);
	assert_eq!(s.undo_label(), Some("Swap pads Kick and Snare"));
	assert_eq!(pad_stem(s, drums, 36), snare);
	assert_eq!(pad_stem(s, drums, 38), kick);
	assert_eq!(s.view.instrument.pad, 38, "the dropped-on pad is selected");
	h.state_mut().state.undo();
	h.run_steps(1);
	assert_eq!(pad_stem(&h.state().state, drums, 36), kick);
	// Alt-drag copies.
	h.event(egui::Event::ModifiersChanged(Modifiers::ALT));
	drag_by(&mut h, from, &[d * 0.25, d * 0.6, d]);
	h.event(egui::Event::ModifiersChanged(Modifiers::NONE));
	let s = &h.state().state;
	assert_eq!(s.undo_label(), Some("Copy pad Kick to Snare"));
	assert_eq!(pad_stem(s, drums, 38), kick);
	assert_eq!(pad_stem(s, drums, 36), kick);
}

#[test]
fn dragging_the_drum_sampler_start_flag_is_one_undo_step() {
	let (mut a, drums, _) = sampler_app();
	a.state.view.instrument.pad = 38;
	let path = a
		.state
		.device(drums)
		.unwrap()
		.zones
		.iter()
		.find(|z| z.key_low == 38)
		.unwrap()
		.sample
		.path
		.clone();
	let mut h = app_harness(a, vec2(1600.0, 1000.0), false);
	wait_for_peaks(&mut h, &path);
	let before = h.state().state.document().history_labels().len();
	let flag = center_of(&h, "Start flag");
	let wave = h.state().instrument_wave().unwrap();
	assert!(
		(flag.x - wave.left()).abs() < 3.0,
		"starts at the sample start"
	);
	drag_by(
		&mut h,
		flag + vec2(0.0, 20.0),
		&[vec2(10.0, 20.0), vec2(30.0, 20.0), vec2(60.0, 20.0)],
	);
	let start = |h: &Harness<'_, CadenzaApp>| {
		h.state()
			.state
			.device(drums)
			.unwrap()
			.zones
			.iter()
			.find(|z| z.key_low == 38)
			.unwrap()
			.start
	};
	let s = &h.state().state;
	assert_eq!(
		s.document().history_labels().len(),
		before + 1,
		"one undo step"
	);
	assert_eq!(s.undo_label(), Some("Snare start"));
	assert!(start(&h) > 0.0, "moved right: {}", start(&h));
	// A double-click resets it to the sample start (one more step).
	let flag = center_of(&h, "Start flag");
	double_click(&mut h, flag);
	assert_eq!(start(&h), 0.0);
	assert_eq!(
		h.state().state.document().history_labels().len(),
		before + 2
	);
}

#[test]
fn a_kit_folder_dropped_on_the_drum_sampler_maps_its_pads_by_name() {
	let (a, drums, _) = sampler_app();
	let mut h = app_harness(a, vec2(1600.0, 1000.0), false);
	h.run_steps(3);
	let dir = tempfile::tempdir().unwrap();
	let kit = dir.path().join("Dusty kit");
	std::fs::create_dir_all(kit.join("Snares")).unwrap();
	let src = factory_samples().join("Rim.wav");
	for f in [
		"kick_dusty_01.wav",
		"Snares/brush 04.wav",
		"hat_open_01.wav",
		"vox.wav",
	] {
		std::fs::copy(&src, kit.join(f)).unwrap();
	}
	let before = h.state().state.document().history_labels().len();
	let pad = center_of(&h, "Clap pad");
	egui::DragAndDrop::set_payload(&h.ctx, cadenza_app::state::dnd::Payload::File(kit.clone()));
	h.hover_at(pad);
	h.run_steps(2);
	h.drop_at(pad);
	h.run_steps(2);
	let s = &h.state().state;
	assert_eq!(
		s.document().history_labels().len(),
		before + 1,
		"one undo step"
	);
	assert_eq!(s.undo_label(), Some("Map kit from folder"));
	assert_eq!(pad_stem(s, drums, 36), "kick_dusty_01");
	assert_eq!(pad_stem(s, drums, 38), "brush 04");
	assert_eq!(pad_stem(s, drums, 46), "hat_open_01");
	let notice = s.notices.last().unwrap();
	assert!(notice.text.contains("vox.wav"), "{}", notice.text);
}

#[test]
fn sampler_editor_shows_the_zone_map_and_inspector_edit_opens_it() {
	let (mut a, _, sampler) = sampler_app();
	a.state.close_device_editor();
	a.state.view.panels.set(PanelKind::Inspector, true);
	let mut h = harness(a);
	h.state_mut().state.selection.select_device(sampler);
	h.run_steps(3);
	h.get_by_label("Edit zones").click();
	h.run_steps(3);
	let window = cadenza_app::state::windows::WindowItem::Device(sampler);
	let s = &h.state().state;
	assert!(
		s.view.windows.is_open(window),
		"Edit opens the floating window"
	);
	assert!(h.query_by_label("Presets").is_some());
	assert!(
		h.state().zone_map().is_some(),
		"the zone map draws in the window"
	);
	// Its × closes it.
	h.get_by_label("Close window").click();
	h.run_steps(2);
	assert!(!h.state().state.view.windows.is_open(window));
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
		s.open_device_in_dock(sampler);
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

/// Test WAVs of a felt piano: one per zone of 2 velocity layers x 6 octaves (C1–B6),
/// named like a multisample ("Felt upright F#3 hard.wav"), a decaying tone at the
/// zone's root.
fn felt_upright_files() -> Vec<(u8, u8, &'static str, PathBuf)> {
	static FILES: std::sync::OnceLock<Vec<(u8, u8, &'static str, PathBuf)>> =
		std::sync::OnceLock::new();
	FILES
		.get_or_init(|| {
			let dir = std::env::temp_dir().join(format!("cadenza-ui-felt-{}", std::process::id()));
			std::fs::create_dir_all(&dir).unwrap();
			let sr = 44_100u32;
			let mut out = Vec::new();
			for o in 0..6u8 {
				let lo = 24 + o * 12;
				let root = lo + 6;
				for layer in ["soft", "hard"] {
					let name = format!(
						"Felt upright {} {layer}.wav",
						cadenza_model::note_name(root)
					);
					let path = dir.join(&name);
					let mut w = hound::WavWriter::create(
						&path,
						hound::WavSpec {
							channels: 1,
							sample_rate: sr,
							bits_per_sample: 16,
							sample_format: hound::SampleFormat::Int,
						},
					)
					.unwrap();
					let f = 440.0 * 2f32.powf((f32::from(root) - 69.0) / 12.0);
					let level = if layer == "hard" { 0.85 } else { 0.5 };
					for i in 0..(3 * sr) {
						let t = i as f32 / sr as f32;
						let env = (0.15 + 0.85 * (-t * 2.2).exp()) * (t * 400.0).min(1.0);
						let tone: f32 = (1..=6)
							.map(|k| {
								let k = k as f32;
								(t * f * k * std::f32::consts::TAU).sin() / (k * k)
									* (-t * k * 0.6).exp()
							})
							.sum();
						let s = (level * env * tone * 0.8).clamp(-1.0, 1.0);
						w.write_sample((s * 32_000.0) as i16).unwrap();
					}
					w.finalize().unwrap();
					out.push((lo, root, layer, path));
				}
			}
			out
		})
		.clone()
}

/// The sampler of `sampler_app` holding the felt piano, zone 6 (C3–B3 hard) selected
/// with a forward loop, its editor open.
fn felt_upright(a: &mut CadenzaApp, sampler: DeviceRef) {
	use cadenza_app::state::sampler_editor as se;
	let zones: Vec<cadenza_model::SampleZone> = felt_upright_files()
		.into_iter()
		.map(|(lo, root, layer, path)| {
			let mut z = cadenza_model::SampleZone::new(
				cadenza_model::SampleRef::new(path),
				lo,
				lo + 11,
				root,
			);
			let (vl, vh) = if layer == "soft" { (0, 79) } else { (80, 127) };
			z.vel_low = se::step_vel(vl);
			z.vel_high = se::step_vel(vh);
			z
		})
		.collect();
	let s = &mut a.state;
	let cmd = se::set(s.project(), sampler, zones, "Felt upright");
	assert!(s.apply_sample_edit(cmd));
	let cmd = se::edit(s.project(), sampler, 5, "Loop", |z| {
		z.loop_mode = cadenza_model::LoopMode::Forward;
		z.loop_start = 1.42;
		z.loop_end = Some(2.36);
		z.loop_crossfade = 0.12;
		z.start = 0.02;
		z.end = Some(2.7);
	});
	assert!(s.apply_sample_edit(cmd));
	assert!(s.open_device_in_dock(sampler));
	s.view.instrument.zone = 5;
	s.view.panels.set(PanelKind::Inspector, false);
	s.view.panels.set(PanelKind::Browser, false);
}

fn sampler_zone(state: &AppState, r: DeviceRef, i: usize) -> cadenza_model::SampleZone {
	state.device(r).unwrap().zones[i].clone()
}

#[test]
fn dragging_a_sampler_zone_moves_its_keys_and_velocity_in_one_undo_step() {
	use cadenza_app::state::sampler_editor as se;
	let (mut a, _, sampler) = sampler_app();
	felt_upright(&mut a, sampler);
	let mut h = app_harness(a, vec2(1920.0, 1000.0), false);
	h.run_steps(3);
	let before = h.state().state.document().history_labels().len();
	let (map, w) = h.state().zone_map().expect("zone map drawn");
	let key_px = map.width() / w.keys();
	let vel_px = map.height() / w.vels();
	let z = sampler_zone(&h.state().state, sampler, 2);
	assert_eq!((z.key_low, z.key_high), (36, 47));
	let from = center_of(&h, "Zone 3");
	let d = vec2(2.0 * key_px, -10.0 * vel_px);
	drag_by(&mut h, from, &[d * 0.3, d * 0.7, d]);
	let s = &h.state().state;
	assert_eq!(
		s.document().history_labels().len(),
		before + 1,
		"one undo step"
	);
	assert_eq!(s.undo_label(), Some("Move zone"));
	let moved = sampler_zone(s, sampler, 2);
	assert_eq!((moved.key_low, moved.key_high), (38, 49));
	assert_eq!(moved.root_key, z.root_key + 2, "the root follows");
	assert_eq!(
		(se::vel_step(moved.vel_low), se::vel_step(moved.vel_high)),
		(10, 89)
	);
	assert_eq!(s.view.instrument.zone, 2, "the dragged zone is selected");
	// Undo puts it back.
	h.state_mut().state.undo();
	h.run_steps(1);
	assert_eq!(sampler_zone(&h.state().state, sampler, 2), z);
}

#[test]
fn dragging_the_sampler_loop_end_flag_is_one_undo_step() {
	let (mut a, _, sampler) = sampler_app();
	felt_upright(&mut a, sampler);
	let path = sampler_zone(&a.state, sampler, 5).sample.path;
	let mut h = app_harness(a, vec2(1920.0, 1000.0), false);
	wait_for_peaks(&mut h, &path);
	let before = h.state().state.document().history_labels().len();
	let flag = center_of(&h, "Loop end flag");
	drag_by(
		&mut h,
		flag + vec2(0.0, -20.0),
		&[vec2(-10.0, -20.0), vec2(-25.0, -20.0), vec2(-40.0, -20.0)],
	);
	let s = &h.state().state;
	assert_eq!(
		s.document().history_labels().len(),
		before + 1,
		"one undo step"
	);
	assert_eq!(s.undo_label(), Some("Loop end"));
	let end = sampler_zone(s, sampler, 5).loop_end.unwrap();
	assert!(end < 2.3 && end > 1.42, "moved left: {end}");
	let z = sampler_zone(s, sampler, 5);
	assert_eq!(z.loop_start, 1.42, "only the loop end moved");
}

/// The Sampler redesign (17e): the dock with the felt piano (2 velocity layers x 6
/// octaves, C3–B3 hard selected with a forward loop, a held note), the filter envelope
/// open, mid-width (map above), narrow (stacked) and the inspector's compact form.
/// `CADENZA_SCREENSHOT_DIR=/tmp/shots cargo test -p cadenza-app --release --test ui --
/// --ignored render_sampler_editor_screens`.
#[test]
#[ignore]
fn render_sampler_editor_screens() {
	use cadenza_app::ui::devices::sampler;
	let Some(dir) = std::env::var_os("CADENZA_SCREENSHOT_DIR").map(PathBuf::from) else {
		return;
	};
	std::fs::create_dir_all(&dir).unwrap();
	let shot = |h: &mut Harness<'_, CadenzaApp>, name: &str| {
		h.run_steps(4);
		h.render()
			.unwrap()
			.save(dir.join(format!("{name}.png")))
			.unwrap();
	};
	let wait_all = |h: &mut Harness<'_, CadenzaApp>| {
		for (.., path) in felt_upright_files() {
			wait_for_peaks(h, &path);
		}
	};
	for (width, name) in [
		(1920.0, "sampler_dock"),
		(1280.0, "sampler_dock_1280"),
		(540.0, "sampler_dock_540"),
	] {
		let (mut a, _, smp) = sampler_app();
		felt_upright(&mut a, smp);
		let track = smp.track.unwrap();
		let mut h = app_harness(a, vec2(width, 1000.0), true);
		wait_all(&mut h);
		h.state_mut().state.preview_note_on(track, 54, 0.9);
		shot(&mut h, name);
		if width < 600.0 {
			// Scrolled down to the zone and the globals.
			let body = h.state().device_body_rect().unwrap();
			let at = body.left_top() + vec2(40.0, 6.0);
			h.hover_at(at);
			h.event(egui::Event::MouseWheel {
				unit: egui::MouseWheelUnit::Point,
				delta: vec2(0.0, -560.0),
				phase: egui::TouchPhase::Move,
				modifiers: Modifiers::NONE,
			});
			h.run_steps(8);
			shot(&mut h, "sampler_dock_540_scrolled");
		}
		if width == 1920.0 {
			sampler::set_filter_envelope(&h.ctx, "dock", smp, true);
			let cmd = {
				let s = &h.state().state;
				let p = s.project();
				let t = p.track(track).unwrap();
				let d = t.instrument.as_ref().unwrap();
				let mut params = d.params.clone();
				params.insert("filter_type".into(), 1.0);
				params.insert("filter_cutoff".into(), 4200.0);
				params.insert("filter_resonance".into(), 20.0);
				params.insert("filter_env_amount".into(), 12.0);
				params.insert("amp_sustain".into(), 80.0);
				params.insert("amp_release".into(), 900.0);
				cadenza_model::Command::SetDeviceState {
					track: Some(track),
					device: d.id,
					state: d.state.clone(),
					params: Some(params),
				}
			};
			h.state_mut().state.apply(cmd);
			shot(&mut h, "sampler_dock_filter_env");
		}
		h.state_mut().state.preview_note_off(track, 54);
	}
	// The compact form in the inspector.
	let (mut a, _, smp) = sampler_app();
	felt_upright(&mut a, smp);
	a.state.close_device_editor();
	a.state.view.panels.set(PanelKind::Inspector, true);
	a.state.selection.select_device(smp);
	a.state.view.panels.set(PanelKind::PianoRoll, false);
	let mut h = app_harness(a, vec2(1600.0, 1000.0), true);
	wait_all(&mut h);
	shot(&mut h, "sampler_inspector_compact");
}

/// The Drum Sampler redesign (17d): the dock with Snare selected (layers, output tag,
/// decay), a file dragged over a pad, a folder dragged over the kit, narrower docks and
/// the inspector's compact form.
/// `CADENZA_SCREENSHOT_DIR=/tmp/shots cargo test -p cadenza-app --release --test ui --
/// --ignored render_drum_sampler_screens`.
#[test]
#[ignore]
fn render_drum_sampler_screens() {
	use cadenza_app::state::drum_sampler_editor as ds;
	use cadenza_app::state::samples as sm;
	use cadenza_app::ui::devices::drum_sampler;
	let Some(dir) = std::env::var_os("CADENZA_SCREENSHOT_DIR").map(PathBuf::from) else {
		return;
	};
	std::fs::create_dir_all(&dir).unwrap();
	let factory = factory_samples();
	let kit = |a: &mut CadenzaApp, drums: DeviceRef| {
		let s = &mut a.state;
		type Edit<'a> = Box<dyn Fn(&cadenza_model::Project) -> Option<cadenza_model::Command> + 'a>;
		let edits: Vec<Edit> = vec![
			Box::new(|p| ds::add_layer(p, drums, 38, &factory.join("Snare.wav"))),
			Box::new(|p| ds::add_layer(p, drums, 38, &factory.join("Rim.wav"))),
			Box::new(|p| {
				sm::edit_pad(p, drums, 38, |z| {
					z.output = 2;
					z.decay_ms = Some(260.0);
					z.attack_ms = 1.0;
					z.tune = 2.0;
					z.gain_db = -1.5;
					z.pan = -0.16;
				})
			}),
			Box::new(|p| ds::edit_layer(p, drums, 38, 1, |z| z.start = 0.004)),
			Box::new(|p| ds::add_layer(p, drums, 36, &factory.join("Kick.wav"))),
			Box::new(|p| sm::edit_pad(p, drums, 36, |z| z.output = 1)),
			Box::new(|p| ds::add_layer(p, drums, 42, &factory.join("Closed Hat.wav"))),
			Box::new(|p| ds::set_round_robin(p, drums, 42, true)),
			Box::new(|p| sm::edit_pad(p, drums, 42, |z| z.choke = 1)),
			Box::new(|p| sm::edit_pad(p, drums, 46, |z| z.choke = 1)),
			Box::new(|p| sm::edit_pad(p, drums, 44, |z| z.choke = 1)),
			Box::new(|p| sm::edit_pad(p, drums, 49, |z| z.choke = 2)),
		];
		for e in edits {
			let cmd = e(s.project());
			s.apply_sample_edit(cmd);
		}
		s.view.instrument.pad = 38;
		s.view.instrument.layer = 1;
	};
	let wait_all = |h: &mut Harness<'_, CadenzaApp>| {
		for f in std::fs::read_dir(&factory).unwrap().flatten() {
			wait_for_peaks(h, &f.path());
		}
	};
	let shot = |h: &mut Harness<'_, CadenzaApp>, name: &str| {
		h.run_steps(4);
		h.render()
			.unwrap()
			.save(dir.join(format!("{name}.png")))
			.unwrap();
	};
	for (width, name) in [
		(1920.0, "drum_sampler_dock"),
		(1280.0, "drum_sampler_dock_1280"),
		(900.0, "drum_sampler_dock_900"),
	] {
		let (mut a, drums, _) = sampler_app();
		a.state.view.panels.set(PanelKind::Inspector, false);
		a.state.view.panels.set(PanelKind::Browser, false);
		kit(&mut a, drums);
		let mut h = app_harness(a, vec2(width, 1000.0), true);
		wait_all(&mut h);
		drum_sampler::set_flash(&h.ctx, "dock", drums, Some((0, 0.8)));
		shot(&mut h, name);
		if width == 1920.0 {
			drum_sampler::set_flash(&h.ctx, "dock", drums, None);
			// A file from the browser over the ride pad: the blue dashed target.
			let ride = h
				.state()
				.state
				.device(drums)
				.map(|_| center_of(&h, "Ride pad"))
				.unwrap();
			egui::DragAndDrop::set_payload(
				&h.ctx,
				cadenza_app::state::dnd::Payload::File(factory.join("Crash.wav")),
			);
			h.hover_at(ride);
			shot(&mut h, "drum_sampler_drop");
			// A folder over the kit.
			egui::DragAndDrop::set_payload(
				&h.ctx,
				cadenza_app::state::dnd::Payload::File(factory.clone()),
			);
			h.hover_at(ride - vec2(200.0, 0.0));
			shot(&mut h, "drum_sampler_folder_drop");
			egui::DragAndDrop::clear_payload(&h.ctx);
			h.remove_cursor();
		}
	}
	// The compact form in the inspector.
	let (mut a, drums, _) = sampler_app();
	kit(&mut a, drums);
	a.state.close_device_editor();
	a.state.view.panels.set(PanelKind::Inspector, true);
	a.state.selection.select_device(drums);
	a.state.view.panels.set(PanelKind::PianoRoll, false);
	let mut h = app_harness(a, vec2(1600.0, 1000.0), true);
	wait_all(&mut h);
	shot(&mut h, "drum_sampler_inspector_compact");
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
	let lane_y = { h.state().piano_event_lane().unwrap().top() + 30.0 };
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

/// The piano roll screen (design 04): chords in C minor with scale rows, three notes
/// selected, the velocity lane, a mod wheel lane, and the same at a narrow width.
/// `CADENZA_SCREENSHOT_DIR=/tmp/shots cargo test -p cadenza-app --test ui -- --ignored`.
#[test]
#[ignore]
fn render_piano_roll_screens() {
	use cadenza_app::state::piano::EventLane;
	use cadenza_model::{Command, Controller, ControllerPoint, Note};
	let Some(dir) = std::env::var_os("CADENZA_SCREENSHOT_DIR").map(PathBuf::from) else {
		return;
	};
	std::fs::create_dir_all(&dir).unwrap();
	let (h, clip) = fl_piano();
	let mut app = h.into_state();
	let old: Vec<_> = clip_notes_of(&app, clip).iter().map(|n| n.id).collect();
	assert!(app.state.apply(Command::RemoveNotes { clip, notes: old }));
	// Cm, Ab, Eb/Bb, Bb in a 6 + 2 + 8 sixteenth rhythm, one bar each.
	let chords: [[u8; 3]; 4] = [[60, 63, 67], [60, 63, 68], [58, 63, 67], [58, 62, 65]];
	let rhythm = [
		(0.0, 1.5, [0.76, 0.69, 0.72]),
		(1.5, 0.5, [0.55, 0.5, 0.52]),
		(2.0, 2.0, [0.82, 0.76, 0.79]),
	];
	let mut notes = Vec::new();
	let mut selected = Vec::new();
	for (bar, chord) in chords.iter().enumerate() {
		for (ri, (start, len, vels)) in rhythm.iter().enumerate() {
			for (k, pitch) in chord.iter().enumerate() {
				let n = Note::new(
					*pitch,
					Tick::from_beats(bar as f64 * 4.0 + start),
					Tick::from_beats(*len),
					vels[k],
				);
				if bar == 1 && ri == 0 {
					selected.push(n.id);
				}
				notes.push(n);
			}
		}
	}
	assert!(app.state.apply(Command::AddNotes { clip, notes }));
	let bar = |b: f64| Tick::from_beats(b * 4.0);
	assert!(
		app.state.apply(Command::SetControllerLane {
			clip,
			controller: Controller::Cc(1),
			points: [(0.0, 0.2), (1.0, 0.2), (1.5, 0.6), (2.25, 0.5), (3.0, 0.85)]
				.iter()
				.map(|(b, v)| ControllerPoint::new(bar(*b), *v))
				.collect(),
		})
	);
	{
		let s = &mut app.state;
		s.view.panels.set(PanelKind::Mixer, false);
		s.view.piano.scale = cadenza_theory::Scale::parse("C minor").ok();
		s.view.piano.select(selected, false);
		s.view.piano.velocity_lane = true;
		s.view.piano.event_lane = EventLane::Velocity;
		s.view.piano.framed = None;
		s.seek(Tick::from_beats(6.0));
		s.view.panels.focus = Some(PanelKind::PianoRoll);
		s.view.panels.toggle_maximize();
	}
	let build = |app: CadenzaApp, size: egui::Vec2| {
		Harness::builder()
			.with_size(size)
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
	let mut h = build(app, vec2(1600.0, 1000.0));
	shot(&mut h, &dir, "piano_roll_notes");
	h.state_mut().state.view.piano.event_lane = EventLane::Controller(Controller::Cc(1));
	shot(&mut h, &dir, "piano_roll_mod_wheel");
	h.state_mut().state.view.piano.event_lane = EventLane::Velocity;
	let mut app = h.into_state();
	app.state.view.piano.framed = None;
	let mut h = build(app, vec2(1100.0, 760.0));
	shot(&mut h, &dir, "piano_roll_narrow");
	h.state_mut().state.view.panels.toggle_maximize();
	shot(&mut h, &dir, "piano_roll_docked");
}

fn clip_notes_of(app: &CadenzaApp, clip: cadenza_core::ClipId) -> Vec<cadenza_model::Note> {
	app.state
		.project()
		.find_clip(clip)
		.unwrap()
		.1
		.notes()
		.to_vec()
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
	assert!(h.query_by_label("Save to").is_some());
	assert!(h.query_by_label_contains("first_light.wav").is_some());
	{
		let s = &mut h.state_mut().state;
		s.export.folder = Some(tmp.path().to_path_buf());
		s.export.settings.range = cadenza_app::state::export::RangeKind::Bars;
		s.export.settings.from_bar = 1;
		s.export.settings.to_bar = 1;
		s.export.settings.stems = cadenza_app::state::export::StemMode::Buses;
	}
	h.run_steps(2);
	// The button's own screen rect sits right at the modal's lower edge, which a plain
	// `click()` (synthesized at the reported rect's center) can miss depending on the
	// exact layout pass; `click_accesskit()` drives it through the accessibility tree
	// instead, which is robust to that.
	h.get_by_label_contains("Export ").click_accesskit();
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
	assert!(h.query_by_label_contains("files ·").is_some());
	assert!(h.query_by_label("Open folder").is_some());
	h.get_by_label("Done").click_accesskit();
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
	assert!(h.query_by_label("Empty project").is_some());
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
	assert_eq!(
		s.recent.first(),
		Some(&cadenza_core::paths::real_path(s.path().unwrap()).unwrap())
	);

	h.state_mut().state.view.start_screen = true;
	h.run_steps(2);
	h.get_by_label("Empty project").click();
	h.run_steps(2);
	assert!(!h.state().state.view.start_screen);
	assert_eq!(h.state().state.project().tracks.len(), 0);
}

#[test]
fn a_start_screen_style_lays_out_a_new_song() {
	let mut state = AppState::headless();
	state.view.start_screen = true;
	let mut h = harness(CadenzaApp::with_state(state));
	h.run_steps(2);
	h.get_by_label("House").click();
	h.run_steps(2);
	let s = &h.state().state;
	assert!(!s.view.start_screen);
	assert!(
		s.project()
			.meta
			.style
			.as_deref()
			.is_some_and(|id| id.ends_with("house"))
	);
	assert!(!s.project().markers.is_empty(), "the style's form");
	assert_eq!(s.document().history_labels().len(), 1, "one undo step");
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
	let file = tmp.path().join("Lost").join("Lost.cdz");
	p.save_json(&file).unwrap();
	let mut state = AppState::headless();
	assert!(state.open(&file));
	let mut h = harness(CadenzaApp::with_state(state));
	h.run_steps(2);
	assert!(h.query_by_label("Locate missing files").is_some());
	assert!(h.query_by_label("vox take.wav").is_some());
	assert!(h.query_by_label("Search folder…").is_some());
	h.get_by_label("×").click();
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
		PathBuf::from("/home/me/Music/Night Drive/Night Drive.cdz"),
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
	let file = tmp.path().join("Lost").join("Lost.cdz");
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
	let lane_y = h.state().piano_event_lane().unwrap().top() + 30.0;
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
	h.get_by_label("Automation: Read").click();
	h.run_steps(2);
	assert_eq!(h.state().state.autowrite.mode, WriteMode::Touch);
	h.get_by_label("Automation: Touch").click();
	h.run_steps(2);
	assert_eq!(h.state().state.autowrite.mode, WriteMode::Latch);
	h.get_by_label("Automation: Latch").click();
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
	assert!(h.query_by_label("Credits and licenses").is_some());
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
	assert!(h.query_by_label("Harmony").is_some());
	// The card's tooltip (the pointer is still on it) would cover the document.
	h.remove_cursor();
	h.get_all_by_label("Apply").next().unwrap().scroll_to_me();
	h.run_steps(3);
	h.get_all_by_label("Apply").next().unwrap().click();
	h.run_steps(2);
	let s = &h.state().state;
	assert_eq!(s.project().meta.style.as_deref(), Some("core/latin_pop"));
	assert_eq!(s.project().tempo.points()[0].bpm, 94.0);
	assert!(s.project().markers.iter().any(|m| m.name == "Post-chorus"));
	assert_eq!(s.undo_label(), Some("Apply form 'Latin pop radio form'"));

	h.get_by_label("Close document").click();
	h.run_steps(2);
	assert!(h.state().state.library_view.open.is_none());
	h.get_by_label("Credits and licenses").scroll_to_me();
	h.run_steps(3);
	h.get_by_label("Credits and licenses").click();
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
	// The browser in Library mode at full height, as in the design (08).
	a.state.view.panels.set(PanelKind::Mixer, false);
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
	h.hover_at(pos2(1450.0, 410.0));
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
	assert!(h.query_by_label("Channels").is_some());
	assert!(h.query_by_label("Issues").is_some());
	assert!(
		h.query_by_label("Fix all · 1").is_none(),
		"Fix all only for two or more"
	);
	let undo = h.state().state.document().history_labels().len();
	h.get_all_by_label("Fix")
		.next()
		.expect("a fixable issue")
		.scroll_to_me();
	h.run_steps(2);
	// Toasts sit in the bottom-right corner: bring the button up above them.
	h.hover_at(pos2(1450.0, 500.0));
	let low = h.get_all_by_label("Fix").next().unwrap().rect().bottom() > 800.0;
	for _ in 0..if low { 6 } else { 0 } {
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
		h.query_by_label("⚠ Mix changed since this analysis")
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
	a.state.view.panels.set(PanelKind::Mixer, false);
	let mut h = screenshot_harness(a);
	h.run_steps(2);
	h.key_press(Key::F10);
	h.run_steps(2);
	h.get_by_label("MIX").click();
	h.run_steps(2);
	// Widen the AI panel to the design's 560 px by dragging its left edge.
	drag(
		&mut h,
		pos2(1600.0 - 340.0, 500.0),
		pos2(1600.0 - 560.0, 500.0),
	);
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

fn kit_harness(state: AppState, wgpu: bool) -> Harness<'static, AppState> {
	let mut b = Harness::builder()
		.with_size(vec2(1540.0, 1400.0))
		.with_step_dt(0.1);
	if wgpu {
		b = b.wgpu();
	}
	b.build_ui_state(
		|ui, state: &mut AppState| {
			if !cadenza_app::theme::applied(ui.ctx()) {
				cadenza_app::theme::apply(ui.ctx());
				ui.ctx().request_discard("Cadenza fonts");
				return;
			}
			egui::Frame::new()
				.fill(cadenza_app::theme::CANVAS)
				.inner_margin(egui::Margin::same(16))
				.show(ui, |ui| {
					ui.set_min_size(ui.available_size());
					cadenza_app::ui::device_kit::gallery(ui, state);
				});
		},
		state,
	)
}

/// The kit's param knobs read the project, preview while dragging and commit one
/// `SetParam` per gesture; double-click resets, the wheel commits once it rests.
#[test]
fn device_kit_param_knob_commits_one_command_per_gesture() {
	let mut state = AppState::headless();
	assert!(state.open(&example()));
	let mut h = kit_harness(state, false);
	h.run_steps(3);
	let history = |h: &Harness<'_, AppState>| h.state().document().history_labels().len();
	let before = history(&h);
	// The bound knobs end the rows above the displays (whose nodes are sliders too).
	let displays = h
		.get_all_by_label("Frequency display")
		.map(|n| n.rect().top())
		.fold(f32::INFINITY, f32::min);
	let knob = h
		.get_all_by_role(egui::accesskit::Role::Slider)
		.map(|n| n.rect())
		.rfind(|r| r.bottom() < displays)
		.unwrap();
	let c = knob.center();
	h.hover_at(c);
	h.run_steps(1);
	h.drag_at(c);
	h.run_steps(1);
	for dy in [10.0, 30.0, 60.0] {
		h.hover_at(c - vec2(0.0, dy));
		h.run_steps(1);
	}
	assert_eq!(history(&h), before, "nothing committed while dragging");
	h.drop_at(c - vec2(0.0, 60.0));
	h.run_steps(2);
	assert_eq!(history(&h), before + 1, "one command for the drag");
	// Wheel steps coalesce into one more command after the wheel rests.
	h.hover_at(c);
	h.run_steps(1);
	for _ in 0..3 {
		h.event(egui::Event::MouseWheel {
			unit: egui::MouseWheelUnit::Line,
			delta: vec2(0.0, -1.0),
			phase: egui::TouchPhase::Move,
			modifiers: Modifiers::NONE,
		});
		h.run_steps(1);
	}
	h.run_steps(8);
	assert_eq!(history(&h), before + 2, "one command for the wheel turn");
	// Click the readout, type a value with its unit, Enter.
	let readout = pos2(c.x, knob.bottom() + 28.0);
	h.hover_at(readout);
	h.run_steps(1);
	h.drag_at(readout);
	h.drop_at(readout);
	h.run_steps(2);
	h.event(egui::Event::Text("50 %".into()));
	h.run_steps(1);
	h.key_press(Key::Enter);
	h.run_steps(2);
	assert_eq!(history(&h), before + 3, "one command for the typed value");
	assert!(
		h.state().undo_label().is_some_and(|l| l.ends_with("to 50")),
		"{:?}",
		h.state().undo_label()
	);
	// Double-click resets to the default.
	h.hover_at(c);
	h.run_steps(1);
	for _ in 0..2 {
		h.drag_at(c);
		h.event(egui::Event::PointerButton {
			pos: c,
			button: egui::PointerButton::Primary,
			pressed: false,
			modifiers: Modifiers::NONE,
		});
		h.run_steps(1);
	}
	h.run_steps(2);
	assert_eq!(history(&h), before + 4, "one command for the reset");
}

#[test]
#[ignore]
fn render_device_kit() {
	let Some(dir) = std::env::var_os("CADENZA_SCREENSHOT_DIR").map(PathBuf::from) else {
		return;
	};
	std::fs::create_dir_all(&dir).unwrap();
	let mut state = AppState::headless();
	assert!(state.open(&example()));
	let mut h = kit_harness(state, true);
	h.run_steps(3);
	// Hover the "Hover" knob long enough for its floating value.
	let hover = h
		.get_by_role_and_label(egui::accesskit::Role::Slider, "Hover")
		.rect()
		.center();
	h.hover_at(hover);
	h.run_steps(6);
	h.render()
		.unwrap()
		.save(dir.join("device_kit.png"))
		.unwrap();
	// The displays: drag the ADSR decay node (white node + tag), then hover the EQ.
	let decay = h
		.get_by_role_and_label(egui::accesskit::Role::Slider, "Envelope decay")
		.rect()
		.center();
	h.hover_at(decay);
	h.run_steps(2);
	h.drag_at(decay);
	h.run_steps(1);
	h.hover_at(decay + vec2(-14.0, 20.0));
	h.run_steps(3);
	h.render()
		.unwrap()
		.save(dir.join("device_kit_adsr_drag.png"))
		.unwrap();
	h.drop_at(decay + vec2(-14.0, 20.0));
	h.run_steps(2);
	let band = h.get_by_label("Band 4").rect().center();
	h.hover_at(band + vec2(70.0, 40.0));
	h.run_steps(3);
	h.render()
		.unwrap()
		.save(dir.join("device_kit_eq_hover.png"))
		.unwrap();
}

/// A primary click at `at` (press and release one step apart).
fn click_at(h: &mut Harness<'_, CadenzaApp>, at: egui::Pos2) {
	h.hover_at(at);
	h.run_steps(1);
	for pressed in [true, false] {
		h.event(egui::Event::PointerButton {
			pos: at,
			button: egui::PointerButton::Primary,
			pressed,
			modifiers: Modifiers::NONE,
		});
		h.run_steps(1);
	}
}

/// Double-clicks at `at` with clicks one short step apart.
fn double_click(h: &mut Harness<'_, CadenzaApp>, at: egui::Pos2) {
	h.hover_at(at);
	h.run_steps(1);
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
}

fn app_harness(app: CadenzaApp, size: egui::Vec2, wgpu: bool) -> Harness<'static, CadenzaApp> {
	let mut b = Harness::builder().with_size(size).with_step_dt(1.0 / 60.0);
	if wgpu {
		b = b.wgpu();
	}
	b.build_ui_state(
		|ui, app: &mut CadenzaApp| {
			app.logic(ui.ctx());
			app.ui(ui);
		},
		app,
	)
}

/// The reverb insert of the example song's reverb bus.
fn reverb_insert(state: &AppState) -> DeviceRef {
	let t = state
		.project()
		.tracks
		.iter()
		.find(|t| {
			t.channel
				.inserts
				.iter()
				.any(|d| d.builtin_id() == Some("reverb"))
		})
		.unwrap();
	DeviceRef {
		track: Some(t.id),
		device: t.channel.inserts[0].id,
	}
}

/// The delay insert of the example song's delay bus.
fn delay_insert(state: &AppState) -> DeviceRef {
	let t = state
		.project()
		.tracks
		.iter()
		.find(|t| {
			t.channel
				.inserts
				.iter()
				.any(|d| d.builtin_id() == Some("delay"))
		})
		.unwrap();
	DeviceRef {
		track: Some(t.id),
		device: t.channel.inserts[0].id,
	}
}

#[test]
fn double_clicking_a_mixer_insert_opens_its_device_editor() {
	let a = app();
	let reverb = reverb_insert(&a.state);
	let mut h = app_harness(a, vec2(1600.0, 1000.0), false);
	h.run_steps(3);
	assert!(h.state().state.view.panels.visible(PanelKind::Mixer));
	let insert = h.get_by_label("Insert Reverb").rect().center();
	double_click(&mut h, insert);
	let s = &h.state().state;
	assert_eq!(
		s.view.windows.focused_device(),
		Some(reverb),
		"the click opened the device window (the double-click did not close it)"
	);
	assert_eq!(s.view.windows.open.len(), 1);
	assert_eq!(s.probe_target(), Some(reverb.device));
	h.run_steps(2);
	// The header names the device; the body has a knob per param.
	assert!(h.query_by_label("Presets").is_some());
	assert!(
		h.query_by_role_and_label(egui::accesskit::Role::Slider, "Mix")
			.is_some(),
		"Mix sits in the header"
	);
	let history = h.state().state.document().history_labels().len();
	// The dock's knob (the inspector shows the compact form with its own).
	let knob = h
		.query_all_by_role_and_label(egui::accesskit::Role::Slider, "Size")
		.map(|n| n.rect())
		.max_by(|a, b| a.top().total_cmp(&b.top()))
		.unwrap();
	let c = knob.center();
	h.hover_at(c);
	h.run_steps(1);
	h.drag_at(c);
	h.run_steps(1);
	for dy in [10.0, 30.0, 50.0] {
		h.hover_at(c - vec2(0.0, dy));
		h.run_steps(1);
	}
	h.drop_at(c - vec2(0.0, 50.0));
	h.run_steps(3);
	let s = &h.state().state;
	assert_eq!(
		s.document().history_labels().len(),
		history + 1,
		"one undo step"
	);
	let size = s.device(reverb).unwrap().params["size"];
	assert!(size > 50.0, "dragged up: {size}");
	assert!(s.preset_status(reverb).name.is_none() || s.preset_status(reverb).dirty);
}

/// `CADENZA_SCREENSHOT_DIR=dir cargo test -p cadenza-app --release --test ui -- --ignored
/// render_device_editor_screens`: the reverb with the full header, the compressor in a
/// narrow window (compact header), the drum sampler in the device editor.
#[test]
#[ignore]
fn render_device_editor_screens() {
	let Some(dir) = std::env::var_os("CADENZA_SCREENSHOT_DIR").map(PathBuf::from) else {
		return;
	};
	std::fs::create_dir_all(&dir).unwrap();
	let shot = |h: &mut Harness<'_, CadenzaApp>, name: &str| {
		h.run_steps(4);
		h.render()
			.unwrap()
			.save(dir.join(format!("{name}.png")))
			.unwrap();
	};
	let mut a = app();
	let reverb = reverb_insert(&a.state);
	a.state
		.commit_param(reverb.track, reverb.device, "size", 72.0);
	assert!(a.state.open_device_in_dock(reverb));
	let mut h = app_harness(a, vec2(1600.0, 1000.0), true);
	shot(&mut h, "device_editor_reverb");

	let mut a = app();
	let comp = {
		let t = a
			.state
			.project()
			.tracks
			.iter()
			.find(|t| t.name == "Drums")
			.unwrap();
		DeviceRef {
			track: Some(t.id),
			device: t.channel.inserts[0].id,
		}
	};
	let presets = a.state.device_presets(comp).to_vec();
	if let Some(p) = presets.first() {
		a.state.load_device_preset(comp, p);
		a.state.commit_param(comp.track, comp.device, "ratio", 6.0);
	}
	a.state.view.panels.set(PanelKind::Mixer, false);
	a.state.view.panels.set(PanelKind::Browser, false);
	a.state.view.panels.set(PanelKind::Inspector, false);
	assert!(a.state.open_device_in_dock(comp));
	let mut h = app_harness(a, vec2(720.0, 900.0), true);
	shot(&mut h, "device_editor_compressor_compact");

	// Many params: the generic body wraps its groups (and scrolls when they overflow).
	let mut a = app();
	let lead = {
		let t = a
			.state
			.project()
			.tracks
			.iter()
			.find(|t| t.name == "Lead")
			.unwrap();
		DeviceRef {
			track: Some(t.id),
			device: t.instrument.as_ref().unwrap().id,
		}
	};
	assert!(a.state.open_device_in_dock(lead));
	let mut h = app_harness(a, vec2(1600.0, 1000.0), true);
	shot(&mut h, "device_editor_poly_synth");

	let (a, drums, _) = sampler_app();
	let mut h = app_harness(a, vec2(1600.0, 1000.0), true);
	h.state_mut().state.selection.select_device(drums);
	wait_for_peaks_app(&mut h);
	shot(&mut h, "device_editor_drum_sampler");
}

fn wait_for_peaks_app(h: &mut Harness<'_, CadenzaApp>) {
	wait_for_peaks(h, &factory_samples().join("Kick.wav"));
}

/// The first Parametric EQ insert of the example song.
fn eq_insert(state: &AppState) -> DeviceRef {
	state
		.project()
		.tracks
		.iter()
		.find_map(|t| {
			let d = t
				.channel
				.inserts
				.iter()
				.find(|d| d.builtin_id() == Some("eq"))?;
			Some(DeviceRef {
				track: Some(t.id),
				device: d.id,
			})
		})
		.expect("the example song has an EQ")
}

fn eq_param(state: &AppState, r: DeviceRef, id: &str) -> f32 {
	state.param_value(r.track, r.device, id).unwrap_or_else(|| {
		cadenza_dsp::device_info("eq")
			.and_then(|i| i.param(id))
			.unwrap()
			.default
	})
}

/// The design's "vocal" settings (15a): HP on, Peak 1 cut, Peak 2 lift, Peak 3 dip, air.
fn shape_eq(state: &mut AppState, r: DeviceRef) {
	state.commit_params(
		r.track,
		r.device,
		&[
			("hp_freq", 80.0),
			("hp_slope", 2.0),
			("peak1_freq", 320.0),
			("peak1_gain", -3.5),
			("peak1_q", 1.2),
			("peak2_freq", 2800.0),
			("peak2_gain", 2.5),
			("peak2_q", 0.8),
			("peak3_freq", 6500.0),
			("peak3_gain", -2.5),
			("peak3_q", 4.0),
			("high_shelf_freq", 10_000.0),
			("high_shelf_gain", 3.0),
			("low_shelf_gain", 0.0),
			("lp_slope", 0.0),
		],
		"Shape EQ",
	);
}

#[test]
fn dragging_an_eq_node_moves_freq_and_gain_in_one_undo_step() {
	let mut a = app();
	let r = eq_insert(&a.state);
	shape_eq(&mut a.state, r);
	// The inspector would show the same EQ (compact) too.
	a.state.view.panels.set(PanelKind::Inspector, false);
	assert!(a.state.open_device_in_dock(r));
	let mut h = app_harness(a, vec2(1600.0, 1000.0), false);
	h.run_steps(3);
	let history = |h: &Harness<'_, CadenzaApp>| h.state().state.document().history_labels().len();
	let before = history(&h);
	let freq = eq_param(&h.state().state, r, "peak2_freq");
	let gain = eq_param(&h.state().state, r, "peak2_gain");
	let c = h.get_by_label("Band 2").rect().center();
	h.hover_at(c);
	h.run_steps(1);
	h.drag_at(c);
	h.run_steps(1);
	for d in [5.0, 20.0, 40.0] {
		h.hover_at(c + vec2(d, -d * 0.5));
		h.run_steps(1);
	}
	assert_eq!(history(&h), before, "nothing committed while dragging");
	assert!(
		eq_param(&h.state().state, r, "peak2_freq") > freq,
		"the drag previews"
	);
	h.drop_at(c + vec2(40.0, -20.0));
	h.run_steps(2);
	assert_eq!(history(&h), before + 1, "one undo step for the drag");
	assert_eq!(
		h.state().state.undo_label(),
		Some("Peak 2 freq and gain"),
		"the step names the band and its params"
	);
	let state = &h.state().state;
	assert!(eq_param(state, r, "peak2_freq") > freq * 1.05);
	assert!(eq_param(state, r, "peak2_gain") > gain + 0.5);
	h.state_mut().state.undo();
	h.run_steps(1);
	assert_eq!(eq_param(&h.state().state, r, "peak2_freq"), freq);

	// Del puts the selected band (Peak 2, picked by the drag) to sleep.
	h.state_mut().state.view.panels.focus = Some(PanelKind::PianoRoll);
	h.key_press(Key::Delete);
	h.run_steps(2);
	assert_eq!(eq_param(&h.state().state, r, "peak2_gain"), 0.0);
	assert_eq!(h.state().state.undo_label(), Some("Peak 2 off"));
}

/// A plausible mix spectrum before the EQ, and after it through the real response.
fn eq_analyzer(state: &AppState, r: DeviceRef) -> (Vec<f32>, Vec<f32>) {
	let values: Vec<f32> = cadenza_dsp::device_info("eq")
		.unwrap()
		.params
		.iter()
		.map(|p| eq_param(state, r, p.id))
		.collect();
	let eq = cadenza_dsp::response::EqBands::new(&values, 48_000.0);
	let pre: Vec<f32> = (0..cadenza_app::state::device_display::ANALYZER_BINS)
		.map(|i| {
			let f = cadenza_app::state::device_display::analyzer_freq(i);
			let tilt = -4.5 * (f / 1000.0).log2();
			let wobble = 3.0 * (i as f32 * 0.37).sin() + 1.5 * (i as f32 * 1.7).cos();
			let rolloff = if f < 60.0 {
				-18.0 * (60.0 / f).log2()
			} else {
				0.0
			};
			(-46.0 + tilt + wobble + rolloff).clamp(-90.0, -12.0)
		})
		.collect();
	let post: Vec<f32> = pre
		.iter()
		.enumerate()
		.map(|(i, db)| db + eq.response_db(cadenza_app::state::device_display::analyzer_freq(i)))
		.collect();
	(pre, post)
}

#[test]
#[ignore = "renders PNGs (wgpu); set CADENZA_SCREENSHOT_DIR"]
fn render_eq_screens() {
	let Some(dir) = std::env::var_os("CADENZA_SCREENSHOT_DIR").map(PathBuf::from) else {
		return;
	};
	std::fs::create_dir_all(&dir).unwrap();
	// 15a: the dock with Peak 2 being dragged (the value bubble shows).
	let mut a = app();
	let r = eq_insert(&a.state);
	shape_eq(&mut a.state, r);
	a.state.view.panels.set(PanelKind::Inspector, false);
	assert!(a.state.open_device_in_dock(r));
	let (pre, post) = eq_analyzer(&a.state, r);
	let mut h = app_harness(a, vec2(1920.0, 1000.0), true);
	h.run_steps(3);
	cadenza_app::ui::devices::eq::set_analyzer(&h.ctx, "dock", r, &pre, &post);
	let c = h.get_by_label("Band 2").rect().center();
	h.hover_at(c);
	h.run_steps(1);
	h.drag_at(c);
	h.run_steps(1);
	for d in [4.0, 12.0, 24.0] {
		h.hover_at(c + vec2(d, -d * 0.4));
		h.run_steps(1);
	}
	cadenza_app::ui::devices::eq::set_analyzer(&h.ctx, "dock", r, &pre, &post);
	h.run_steps(1);
	h.render()
		.unwrap()
		.save(dir.join("eq_dock_drag.png"))
		.unwrap();
	h.drop_at(c + vec2(24.0, -9.6));
	h.run_steps(2);

	// The footer open, a pass band selected (slope selector).
	cadenza_app::ui::devices::eq::set_analyzer(&h.ctx, "dock", r, &pre, &post);
	h.get_by_label("High-pass").click();
	h.run_steps(1);
	h.get_by_label("Analyzer and display").click();
	h.run_steps(2);
	h.render()
		.unwrap()
		.save(dir.join("eq_dock_footer.png"))
		.unwrap();

	// 15b: the compact form in the inspector.
	let mut a = app();
	let r = eq_insert(&a.state);
	shape_eq(&mut a.state, r);
	a.state.selection.select_device(r);
	a.state.view.panels.set(PanelKind::Inspector, true);
	a.state.view.panels.set(PanelKind::Mixer, false);
	let (pre, post) = eq_analyzer(&a.state, r);
	let mut h = app_harness(a, vec2(1600.0, 1000.0), true);
	h.run_steps(3);
	cadenza_app::ui::devices::eq::set_analyzer(&h.ctx, "inspector", r, &pre, &post);
	h.run_steps(2);
	h.render()
		.unwrap()
		.save(dir.join("eq_inspector_compact.png"))
		.unwrap();

	// The compact dock (narrow window).
	let mut a = app();
	let r = eq_insert(&a.state);
	shape_eq(&mut a.state, r);
	a.state.view.panels.set(PanelKind::Mixer, false);
	a.state.view.panels.set(PanelKind::Browser, false);
	a.state.view.panels.set(PanelKind::Inspector, false);
	assert!(a.state.open_device_in_dock(r));
	let (pre, post) = eq_analyzer(&a.state, r);
	let mut h = app_harness(a, vec2(720.0, 1000.0), true);
	h.run_steps(3);
	cadenza_app::ui::devices::eq::set_analyzer(&h.ctx, "dock", r, &pre, &post);
	h.run_steps(2);
	h.render()
		.unwrap()
		.save(dir.join("eq_dock_compact.png"))
		.unwrap();
}

/// The Bass compressor of the example song.
fn bass_compressor(state: &AppState) -> DeviceRef {
	let t = state
		.project()
		.tracks
		.iter()
		.find(|t| t.name == "Bass")
		.unwrap();
	let d = t
		.channel
		.inserts
		.iter()
		.find(|d| d.builtin_id() == Some("compressor"))
		.unwrap();
	DeviceRef {
		track: Some(t.id),
		device: d.id,
	}
}

fn comp_param(state: &AppState, r: DeviceRef, id: &str) -> f32 {
	state.param_value(r.track, r.device, id).unwrap_or_else(|| {
		cadenza_dsp::device_info("compressor")
			.and_then(|i| i.param(id))
			.unwrap()
			.default
	})
}

fn comp_dock_app() -> (CadenzaApp, DeviceRef) {
	let mut a = app();
	let r = bass_compressor(&a.state);
	a.state.view.panels.set(PanelKind::Inspector, false);
	a.state.view.panels.set(PanelKind::Browser, false);
	assert!(a.state.open_device_in_dock(r));
	(a, r)
}

#[test]
fn dragging_the_compressor_threshold_dot_makes_one_undo_step() {
	use cadenza_app::state::compressor_editor::{CompValues, db_to_t};
	let (a, r) = comp_dock_app();
	let mut h = app_harness(a, vec2(1920.0, 1000.0), false);
	h.run_steps(3);
	let history = |h: &Harness<'_, CadenzaApp>| h.state().state.document().history_labels().len();
	let before = history(&h);
	let state = &h.state().state;
	let values = CompValues::from_fn(|p| comp_param(state, r, p.id));
	let threshold = values.threshold;
	let rect = h.get_by_label("Transfer curve").rect();
	let (w, ht) = (rect.width() as f32, rect.height() as f32);
	let dot = pos2(
		rect.min.x as f32 + db_to_t(threshold) * w,
		rect.max.y as f32 - db_to_t(values.curve(threshold)) * ht,
	);
	h.hover_at(dot);
	h.run_steps(1);
	h.drag_at(dot);
	h.run_steps(1);
	for dx in [5.0, 20.0, 40.0] {
		h.hover_at(dot + vec2(dx, 0.0));
		h.run_steps(1);
	}
	assert_eq!(history(&h), before, "nothing committed while dragging");
	assert!(comp_param(&h.state().state, r, "threshold") > threshold);
	h.drop_at(dot + vec2(40.0, 0.0));
	h.run_steps(2);
	assert_eq!(history(&h), before + 1, "one undo step");
	let now = comp_param(&h.state().state, r, "threshold");
	let expected = threshold + 40.0 * 60.0 / w;
	assert!((now - expected).abs() < 0.3, "{now} vs {expected}");
	assert_eq!(
		comp_param(&h.state().state, r, "ratio"),
		values.ratio,
		"only the threshold moved"
	);
	h.state_mut().state.undo();
	h.run_steps(1);
	assert_eq!(comp_param(&h.state().state, r, "threshold"), threshold);
}

#[test]
fn choosing_a_compressor_key_source_makes_one_set_sidechain_step() {
	let (a, r) = comp_dock_app();
	let drums = a
		.state
		.project()
		.tracks
		.iter()
		.find(|t| t.name == "Drums")
		.unwrap()
		.id;
	let mut h = app_harness(a, vec2(1920.0, 1000.0), false);
	h.run_steps(3);
	let before = h.state().state.document().history_labels().len();
	h.get_by_label("Key source").click();
	h.run_steps(2);
	assert!(
		h.query_by_label("Key from Bass").is_none(),
		"a track cannot key itself"
	);
	h.get_by_label("Key from Drums").click();
	h.run_steps(2);
	let s = &h.state().state;
	assert_eq!(s.document().history_labels().len(), before + 1);
	assert_eq!(s.undo_label(), Some("Set sidechain"));
	let key = s.device(r).unwrap().sidechain.unwrap();
	assert_eq!((key.track, key.pre_fader), (drums, false));

	// Pre-fader, then none: one step each.
	h.get_by_label("Key source").click();
	h.run_steps(2);
	h.get_by_label("Pre-fader").click();
	h.run_steps(2);
	let s = &h.state().state;
	assert!(s.device(r).unwrap().sidechain.unwrap().pre_fader);
	assert_eq!(s.document().history_labels().len(), before + 2);
	h.get_by_label("Key source").click();
	h.run_steps(2);
	h.get_by_label("Key from none").click();
	h.run_steps(2);
	let s = &h.state().state;
	assert!(s.device(r).unwrap().sidechain.is_none());
	assert_eq!(s.undo_label(), Some("Remove sidechain"));
}

/// A bass ducked by a kick through a sidechain (design 15c), one column per probe step.
fn pump_history(
	threshold: f32,
	ratio: f32,
	knee: f32,
	makeup: f32,
) -> (cadenza_app::state::device_display::LevelHistory, f32) {
	use cadenza_app::state::device_display::LevelHistory;
	let columns = 300;
	// One frame per column.
	let mut h = LevelHistory::new(columns, 4.0, (columns / 4) as u32);
	let v = cadenza_app::state::compressor_editor::CompValues {
		threshold,
		ratio,
		knee,
		makeup,
	};
	let dt = 4.0 / columns as f32;
	let (att, rel) = (1.0 - (-dt / 0.010f32).exp(), 1.0 - (-dt / 0.120f32).exp());
	let mut seed = 5u32;
	let mut rnd = || {
		seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
		(seed >> 16) as f32 / 65_536.0
	};
	let mut gr = 0.0f32;
	for i in 0..columns {
		let t = i as f32 * dt;
		let ph = (t % 0.652) / 0.652;
		let key = (-15.0 - 52.0 * (1.0 - (-ph * 3.2).exp()) + rnd() * 1.5).max(-60.0);
		let input = (-13.0 + 3.0 * (t * 5.1).sin() + (rnd() - 0.5) * 5.0
			- if ph > 0.92 { 6.0 } else { 0.0 })
		.max(-60.0);
		let target = v.reduction(key);
		gr += (target - gr) * if target > gr { att } else { rel };
		let out = (input - gr + makeup).max(-60.0);
		let amp = |db: f32| 10f32.powf(db / 20.0);
		h.set_key(Some(key));
		h.push([amp(input), -amp(input)], [amp(out), -amp(out)], gr);
	}
	(h, gr.max(6.2))
}

#[test]
#[ignore = "renders PNGs (wgpu); set CADENZA_SCREENSHOT_DIR"]
fn render_compressor_screens() {
	use cadenza_app::ui::devices::compressor;
	let Some(dir) = std::env::var_os("CADENZA_SCREENSHOT_DIR").map(PathBuf::from) else {
		return;
	};
	std::fs::create_dir_all(&dir).unwrap();
	let setup = |a: &mut CadenzaApp| {
		let r = bass_compressor(&a.state);
		let drums = a
			.state
			.project()
			.tracks
			.iter()
			.find(|t| t.name == "Drums")
			.unwrap()
			.id;
		a.state.apply(cadenza_model::Command::SetSidechain {
			track: r.track,
			device: r.device,
			source: Some(cadenza_model::SidechainSource {
				track: drums,
				pre_fader: false,
			}),
		});
		a.state.commit_params(
			r.track,
			r.device,
			&[
				("threshold", -24.0),
				("ratio", 4.0),
				("knee", 6.0),
				("makeup", 4.0),
				("attack", 10.0),
				("release", 120.0),
			],
			"Sidechain pump",
		);
		r
	};
	let (history, gr) = pump_history(-24.0, 4.0, 6.0, 4.0);

	// 15c: the dock, ducking the bass from the drums.
	let (mut a, _) = comp_dock_app();
	let r = setup(&mut a);
	let mut h = app_harness(a, vec2(1920.0, 1000.0), true);
	h.run_steps(2);
	compressor::set_display(&h.ctx, "dock", r, history.clone(), -12.0, gr);
	h.run_steps(3);
	h.render()
		.unwrap()
		.save(dir.join("compressor_dock.png"))
		.unwrap();
	h.get_by_label("Display options").click();
	h.run_steps(3);
	h.render()
		.unwrap()
		.save(dir.join("compressor_dock_footer.png"))
		.unwrap();

	// A narrower dock: one display at a time.
	let (mut a, _) = comp_dock_app();
	let r = setup(&mut a);
	let mut h = app_harness(a, vec2(1000.0, 900.0), true);
	h.run_steps(2);
	compressor::set_display(&h.ctx, "dock", r, history.clone(), -12.0, gr);
	h.run_steps(3);
	h.render()
		.unwrap()
		.save(dir.join("compressor_dock_tabs.png"))
		.unwrap();

	// 15d: the compact form in the inspector.
	let mut a = app();
	let r = setup(&mut a);
	a.state.selection.select_device(r);
	a.state.view.panels.set(PanelKind::Inspector, true);
	a.state.view.panels.set(PanelKind::Mixer, false);
	let mut h = app_harness(a, vec2(1600.0, 1000.0), true);
	h.run_steps(2);
	compressor::set_display(&h.ctx, "inspector", r, history.clone(), -12.0, gr);
	h.run_steps(3);
	h.render()
		.unwrap()
		.save(dir.join("compressor_inspector_compact.png"))
		.unwrap();

	// The compact dock (narrow window).
	let mut a = app();
	let r = setup(&mut a);
	a.state.view.panels.set(PanelKind::Mixer, false);
	a.state.view.panels.set(PanelKind::Browser, false);
	a.state.view.panels.set(PanelKind::Inspector, false);
	assert!(a.state.open_device_in_dock(r));
	let mut h = app_harness(a, vec2(720.0, 1000.0), true);
	h.run_steps(2);
	compressor::set_display(&h.ctx, "dock", r, history, -12.0, gr);
	h.run_steps(3);
	h.render()
		.unwrap()
		.save(dir.join("compressor_dock_compact.png"))
		.unwrap();
}

/// The full header's budget (`device::header_items`) degrading cleanly from the compact
/// two-row header's threshold up to where everything (undo hint, A→B label) fits: the
/// sidechain chip, then the IN/OUT meters, then the preset box give way before anything
/// overlaps (14 Device Kit polish). The compressor has a sidechain chip to exercise that
/// drop too.
#[test]
#[ignore = "renders PNGs (wgpu); set CADENZA_SCREENSHOT_DIR"]
fn render_device_header_widths() {
	let Some(dir) = std::env::var_os("CADENZA_SCREENSHOT_DIR").map(PathBuf::from) else {
		return;
	};
	std::fs::create_dir_all(&dir).unwrap();
	for width in [820.0, 900.0, 1100.0, 1400.0, 1700.0] {
		let mut a = app();
		let r = bass_compressor(&a.state);
		let drums = a
			.state
			.project()
			.tracks
			.iter()
			.find(|t| t.name == "Drums")
			.unwrap()
			.id;
		a.state.apply(cadenza_model::Command::SetSidechain {
			track: r.track,
			device: r.device,
			source: Some(cadenza_model::SidechainSource {
				track: drums,
				pre_fader: false,
			}),
		});
		a.state.view.panels.set(PanelKind::Mixer, false);
		a.state.view.panels.set(PanelKind::Browser, false);
		a.state.view.panels.set(PanelKind::Inspector, false);
		assert!(a.state.open_device_in_dock(r));
		let mut h = app_harness(a, vec2(width, 900.0), true);
		h.run_steps(4);
		h.render()
			.unwrap()
			.save(dir.join(format!("device_header_{width:.0}.png")))
			.unwrap();
	}
}

/// The limiter on the example song's master.
fn master_limiter(state: &AppState) -> DeviceRef {
	let d = state
		.project()
		.master
		.inserts
		.iter()
		.find(|d| d.builtin_id() == Some("limiter"))
		.unwrap();
	DeviceRef {
		track: None,
		device: d.id,
	}
}

fn lim_param(state: &AppState, r: DeviceRef, id: &str) -> f32 {
	state.param_value(r.track, r.device, id).unwrap_or_else(|| {
		cadenza_dsp::device_info("limiter")
			.and_then(|i| i.param(id))
			.unwrap()
			.default
	})
}

fn lim_dock_app() -> (CadenzaApp, DeviceRef) {
	let mut a = app();
	let r = master_limiter(&a.state);
	a.state.view.panels.set(PanelKind::Inspector, false);
	a.state.view.panels.set(PanelKind::Browser, false);
	assert!(a.state.open_device_in_dock(r));
	(a, r)
}

#[test]
fn dragging_the_limiter_ceiling_makes_one_undo_step() {
	let (a, r) = lim_dock_app();
	let mut h = app_harness(a, vec2(1920.0, 1000.0), false);
	h.run_steps(3);
	let history = |h: &Harness<'_, CadenzaApp>| h.state().state.document().history_labels().len();
	let before = history(&h);
	let ceiling = lim_param(&h.state().state, r, "ceiling");
	let gain = lim_param(&h.state().state, r, "input_gain");
	// The line spans the history; the knob of the same name is much narrower.
	let line = h
		.query_all_by_label("Ceiling")
		.map(|n| n.rect())
		.max_by(|a, b| a.width().total_cmp(&b.width()))
		.unwrap();
	let at = pos2(line.center().x as f32, line.center().y as f32);
	h.hover_at(at);
	h.run_steps(1);
	h.drag_at(at);
	h.run_steps(1);
	for dy in [4.0, 12.0, 24.0] {
		h.hover_at(at + vec2(0.0, dy));
		h.run_steps(1);
	}
	assert_eq!(history(&h), before, "nothing committed while dragging");
	assert!(lim_param(&h.state().state, r, "ceiling") < ceiling);
	h.drop_at(at + vec2(0.0, 24.0));
	h.run_steps(2);
	let s = &h.state().state;
	assert_eq!(history(&h), before + 1, "one undo step");
	assert_eq!(s.undo_label(), Some("Limiter ceiling"));
	let now = lim_param(s, r, "ceiling");
	assert!(now < ceiling && now >= -24.0, "{now}");
	assert_eq!(
		lim_param(s, r, "input_gain"),
		gain,
		"only the ceiling moved"
	);
	h.state_mut().state.undo();
	h.run_steps(1);
	assert_eq!(lim_param(&h.state().state, r, "ceiling"), ceiling);
}

#[test]
fn dragging_inside_the_limiter_history_moves_the_input_gain() {
	let (a, r) = lim_dock_app();
	let mut h = app_harness(a, vec2(1920.0, 1000.0), false);
	h.run_steps(3);
	let before = h.state().state.document().history_labels().len();
	let gain = lim_param(&h.state().state, r, "input_gain");
	let line = h
		.query_all_by_label("Ceiling")
		.map(|n| n.rect())
		.max_by(|a, b| a.width().total_cmp(&b.width()))
		.unwrap();
	// Halfway between the top copy of the ceiling line and the centre.
	let at = pos2(line.min.x as f32 + 200.0, line.min.y as f32 + 60.0);
	h.hover_at(at);
	h.run_steps(1);
	h.drag_at(at);
	h.run_steps(1);
	for dy in [-5.0, -20.0, -25.0] {
		h.hover_at(at + vec2(0.0, dy));
		h.run_steps(1);
	}
	h.drop_at(at + vec2(0.0, -25.0));
	h.run_steps(2);
	let s = &h.state().state;
	assert_eq!(s.document().history_labels().len(), before + 1);
	assert_eq!(s.undo_label(), Some("Limiter input gain"));
	assert!(lim_param(s, r, "input_gain") > gain + 1.0);
}

/// (input, output, gain reduction) of one frame.
type ProgramFrame = ([f32; 2], [f32; 2], f32);

/// Synthetic program (kick, snare, bass, pad, hats) through the real limiter, as the
/// probe would deliver it, per frame at 48 kHz.
fn limited_program(seconds: f32, gain_db: f32, ceiling: f32) -> Vec<ProgramFrame> {
	use cadenza_dsp::Transport;
	let rate = 48_000.0f32;
	let mut p = cadenza_dsp::create("limiter").unwrap();
	p.prepare(rate, 64);
	p.set_param(0, gain_db);
	p.set_param(1, ceiling);
	p.set_param(2, 80.0);
	let n = (seconds * rate) as usize;
	let mut seed = 9u32;
	let mut noise = || {
		seed ^= seed << 13;
		seed ^= seed >> 17;
		seed ^= seed << 5;
		seed as f32 / u32::MAX as f32 * 2.0 - 1.0
	};
	let tau = std::f32::consts::TAU;
	let beat = 0.652f32;
	let mut input = Vec::with_capacity(n);
	for i in 0..n {
		let t = i as f32 / rate;
		let tb = t % beat;
		let odd = (t / beat) as u32 % 2 == 1;
		let kick = (-tb / 0.09).exp() * (tau * (48.0 + 90.0 * (-tb / 0.025).exp()) * tb).sin();
		let snare = if odd {
			(-tb / 0.07).exp() * noise() * 0.55
		} else {
			0.0
		};
		let swell = 0.75 + 0.25 * (t * 1.3).sin();
		let pad = swell * (0.16 * (tau * 220.0 * t).sin() + 0.1 * (tau * 329.6 * t).sin());
		let bass = 0.22 * (tau * 55.0 * t).sin() * (0.6 + 0.4 * (-tb / 0.3).exp());
		let hats = noise() * 0.035 * (-((t % (beat / 2.0)) / 0.02)).exp();
		let m = 0.62 * (kick + snare + pad + bass);
		input.push([m + hats, m - hats * 0.7 + noise() * 0.01]);
	}
	let mut out = Vec::with_capacity(n);
	let mut reading = [0.0f32; cadenza_dsp::readings::MAX];
	for chunk in input.chunks(64) {
		let mut l: Vec<f32> = chunk.iter().map(|f| f[0]).collect();
		let mut r: Vec<f32> = chunk.iter().map(|f| f[1]).collect();
		p.process(&mut l, &mut r, &[], &Transport::default());
		let gr = if p.readings(&mut reading) > 0 {
			reading[cadenza_dsp::readings::GAIN_REDUCTION_DB].abs()
		} else {
			0.0
		};
		for (k, f) in chunk.iter().enumerate() {
			out.push((*f, [l[k], r[k]], gr));
		}
	}
	out
}

#[test]
#[ignore = "renders PNGs (wgpu); set CADENZA_SCREENSHOT_DIR"]
fn render_limiter_screens() {
	use cadenza_app::state::device_display::LevelHistory;
	use cadenza_app::state::loudness_meter::LiveLoudness;
	use cadenza_app::ui::devices::limiter;
	let Some(dir) = std::env::var_os("CADENZA_SCREENSHOT_DIR").map(PathBuf::from) else {
		return;
	};
	std::fs::create_dir_all(&dir).unwrap();
	let ceiling = -1.0;
	// The input gain that lands the program on the streaming target.
	let lufs_at = |gain: f32| {
		let mut l = LiveLoudness::new(48_000);
		let frames = limited_program(8.0, gain, ceiling);
		let (a, b): (Vec<f32>, Vec<f32>) = frames.iter().map(|f| (f.1[0], f.1[1])).unzip();
		l.push(&a, &b);
		l.integrated()
	};
	let (mut lo, mut hi) = (0.0f32, 24.0f32);
	for _ in 0..7 {
		let mid = (lo + hi) * 0.5;
		if lufs_at(mid) < -14.0 {
			lo = mid
		} else {
			hi = mid
		}
	}
	let gain = ((lo + hi) * 0.5 * 10.0).round() / 10.0;
	let frames = limited_program(34.0, gain, ceiling);
	let g = 10f32.powf(gain / 20.0);
	let mut history = LevelHistory::new(420, 6.0, 48_000);
	let mut loudness = LiveLoudness::new(48_000);
	for f in &frames[frames.len() - 6 * 48_000..] {
		history.push([f.0[0] * g, f.0[1] * g], f.1, f.2);
	}
	let (a, b): (Vec<f32>, Vec<f32>) = frames.iter().map(|f| (f.1[0], f.1[1])).unzip();
	// Integrated over the last 20 s (reset mid-song), the trace over all of it.
	let reset_at = frames.len() - 20 * 48_000;
	loudness.push(&a[..reset_at], &b[..reset_at]);
	loudness.reset_integrated();
	loudness.push(&a[reset_at..], &b[reset_at..]);
	let tail = &frames[frames.len() - 4_800..];
	let peak = |side: &dyn Fn(&ProgramFrame) -> [f32; 2]| -> [f32; 2] {
		let mut p = [0.0f32; 2];
		for x in tail {
			for (p, s) in p.iter_mut().zip(side(x)) {
				*p = p.max(s.abs());
			}
		}
		p.map(|a| 20.0 * a.max(1e-6).log10())
	};
	let input_db = peak(&|f| [f.0[0] * g, f.0[1] * g]);
	let output_db = peak(&|f| f.1);
	let gr = tail.iter().map(|f| f.2).fold(0.0, f32::max);
	let setup = |a: &mut CadenzaApp| {
		let r = master_limiter(&a.state);
		a.state.commit_params(
			r.track,
			r.device,
			&[
				("input_gain", gain),
				("ceiling", ceiling),
				("release", 80.0),
			],
			"Streaming target",
		);
		r
	};

	// 16a: the dock on the master.
	let (mut a, _) = lim_dock_app();
	let r = setup(&mut a);
	let mut h = app_harness(a, vec2(1920.0, 1000.0), true);
	h.run_steps(2);
	let show = |h: &mut Harness<'_, CadenzaApp>, salt: &str| {
		limiter::set_display(
			&h.ctx,
			salt,
			r,
			history.clone(),
			loudness.clone(),
			gr,
			input_db,
			output_db,
		);
	};
	show(&mut h, "dock");
	h.run_steps(3);
	h.render()
		.unwrap()
		.save(dir.join("limiter_dock.png"))
		.unwrap();
	h.get_by_label("Display options").click();
	h.get_by_label("Loud \u{2212}9").click();
	h.run_steps(3);
	h.render()
		.unwrap()
		.save(dir.join("limiter_dock_footer.png"))
		.unwrap();

	// A narrower dock: the knobs stack.
	let (mut a, _) = lim_dock_app();
	setup(&mut a);
	let mut h = app_harness(a, vec2(1100.0, 900.0), true);
	h.run_steps(2);
	show(&mut h, "dock");
	h.run_steps(3);
	h.render()
		.unwrap()
		.save(dir.join("limiter_dock_narrow.png"))
		.unwrap();

	// The compact form in the inspector.
	let mut a = app();
	let r = setup(&mut a);
	a.state.selection.select_device(r);
	a.state.view.panels.set(PanelKind::Inspector, true);
	a.state.view.panels.set(PanelKind::Mixer, false);
	let mut h = app_harness(a, vec2(1600.0, 1000.0), true);
	h.run_steps(2);
	show(&mut h, "inspector");
	h.run_steps(3);
	h.render()
		.unwrap()
		.save(dir.join("limiter_inspector_compact.png"))
		.unwrap();
	eprintln!(
		"limiter screens: gain {gain} dB, integrated {}",
		loudness.integrated()
	);
}

#[test]
fn clicking_b_after_an_edit_restores_b_in_one_undo_step() {
	let mut a = app();
	let r = eq_insert(&a.state);
	a.state.view.panels.set(PanelKind::Inspector, false);
	assert!(a.state.open_device_in_dock(r));
	let mut h = app_harness(a, vec2(1920.0, 1000.0), false);
	h.run_steps(3);
	let original = eq_param(&h.state().state, r, "peak2_gain");
	// Edit A (both slots started as the device's params when the editor opened).
	h.state_mut()
		.state
		.commit_params(r.track, r.device, &[("peak2_gain", 5.0)], "Peak 2 gain");
	h.run_steps(2);
	let hint = cadenza_app::state::device_ab::undo_hint("Peak 2 gain");
	assert!(
		h.query_by_label(&hint).is_some(),
		"the undo hint names the step that touched this device"
	);
	let history = h.state().state.document().history_labels().len();
	h.get_by_label("Compare B").click();
	h.run_steps(2);
	let s = &h.state().state;
	assert_eq!(
		eq_param(s, r, "peak2_gain"),
		original,
		"B holds the old params"
	);
	assert_eq!(s.document().history_labels().len(), history + 1);
	assert_eq!(s.undo_label(), Some("Compare B"));
	assert_eq!(s.compare_slot(r), cadenza_app::state::device_ab::Slot::B);
	h.state_mut().state.undo();
	h.run_steps(2);
	let s = &h.state().state;
	assert_eq!(
		eq_param(s, r, "peak2_gain"),
		5.0,
		"undo returns to A's values"
	);
	assert_eq!(s.compare_slot(r), cadenza_app::state::device_ab::Slot::A);

	// The A key switches while the editor has the focus (the dock here).
	h.state_mut().state.view.panels.focus = Some(PanelKind::PianoRoll);
	h.key_press(Key::A);
	h.run_steps(2);
	let s = &h.state().state;
	assert_eq!(s.compare_slot(r), cadenza_app::state::device_ab::Slot::B);
	assert_eq!(eq_param(s, r, "peak2_gain"), original);
	// A→B, now B→A: copy B onto A, then A sounds the same.
	h.get_by_label("B→A").click();
	h.run_steps(2);
	h.get_by_label("Compare A").click();
	h.run_steps(2);
	assert_eq!(eq_param(&h.state().state, r, "peak2_gain"), original);
}

#[test]
fn popping_out_the_device_editor_and_docking_it_round_trips() {
	use cadenza_app::state::windows::WindowItem;
	let mut a = app();
	let r = eq_insert(&a.state);
	assert!(a.state.open_device_in_dock(r));
	let mut h = app_harness(a, vec2(1600.0, 1000.0), false);
	h.run_steps(3);
	h.get_by_label("Pop out device editor").click();
	h.run_steps(3);
	let s = &h.state().state;
	assert!(s.view.windows.is_open(WindowItem::Device(r)) && !s.view.device_editor);
	assert_eq!(s.device_editor_target(), Some(r));
	assert_eq!(
		s.probe_target(),
		Some(r.device),
		"the probe follows the window"
	);
	assert!(h.query_by_label("Dock").is_some());
	assert!(h.query_by_label("Pop out device editor").is_none());
	assert!(h.state().window_rect(WindowItem::Device(r)).is_some());

	h.get_by_label("Dock").click();
	h.run_steps(3);
	let s = &h.state().state;
	assert!(s.view.windows.open.is_empty() && s.view.device_editor);
	assert!(s.view.panels.visible(PanelKind::PianoRoll));
	assert_eq!(s.device_editor_target(), Some(r));
	assert!(h.query_by_label("Pop out device editor").is_some());
	assert!(h.state().window_rect(WindowItem::Device(r)).is_none());
}

#[test]
fn several_device_windows_stay_open_focus_close_and_dock() {
	use cadenza_app::state::windows::WindowItem;
	let mut a = app();
	let eq = eq_insert(&a.state);
	let reverb = reverb_insert(&a.state);
	assert!(a.state.open_device_editor(eq));
	assert!(a.state.open_device_editor(reverb));
	let mut h = app_harness(a, vec2(1600.0, 1000.0), false);
	h.run_steps(4);
	let (we, wr) = (WindowItem::Device(eq), WindowItem::Device(reverb));
	let (re, rr) = (
		h.state().window_rect(we).expect("EQ window"),
		h.state().window_rect(wr).expect("reverb window"),
	);
	assert_eq!(rr.min - re.min, vec2(28.0, 28.0), "cascaded");
	assert!(!h.state().state.view.device_editor, "the dock is not used");
	assert_eq!(h.state().state.probe_target(), Some(reverb.device));
	// Clicking the EQ's visible title strip focuses it (keys + probe follow).
	click_at(&mut h, pos2(re.left() + 12.0, re.top() + 10.0));
	h.run_steps(2);
	let s = &h.state().state;
	assert_eq!(s.view.windows.focused_device(), Some(eq));
	assert_eq!(s.probe_target(), Some(eq.device));
	// Opening the reverb again (a mixer click) focuses its window, no duplicate.
	h.state_mut().state.open_device_editor(reverb);
	h.run_steps(2);
	let s = &h.state().state;
	assert_eq!(s.view.windows.open.len(), 2);
	assert_eq!(s.view.windows.focused_device(), Some(reverb));
	// Esc closes the focused one.
	h.key_press(egui::Key::Escape);
	h.run_steps(2);
	let s = &h.state().state;
	assert!(!s.view.windows.is_open(wr) && s.view.windows.is_open(we));
	// Dock puts the EQ into the bottom dock.
	h.get_by_label("Dock").click();
	h.run_steps(3);
	let s = &h.state().state;
	assert!(s.view.windows.open.is_empty());
	assert!(s.view.device_editor && s.view.instrument.device == Some(eq));
	// The rects are remembered in the prefs.
	let prefs = s.prefs();
	assert!(prefs.windows.item(&we.id_key()).is_some());
}

#[test]
fn the_track_window_edits_through_one_command() {
	use cadenza_app::state::windows::WindowItem;
	let a = app();
	let track = a.state.project().tracks[0].id;
	let mut h = app_harness(a, vec2(1600.0, 1000.0), false);
	h.run_steps(3);
	assert!(
		!h.state().state.view.panels.visible(PanelKind::Inspector),
		"the inspector is hidden by default"
	);
	// The track menu's "Track properties…" / a mixer strip double-click.
	assert!(h.state_mut().state.open_track_window(Some(track)));
	h.run_steps(3);
	let item = WindowItem::Track(Some(track));
	let rect = h.state().window_rect(item).expect("track window");
	assert!(rect.width() >= 260.0);
	let history = h.state().state.document().history_labels().len();
	let mute = h
		.query_all_by_label("Mute")
		.find(|n| rect.contains(n.rect().center()))
		.expect("the window's Mute toggle")
		.rect()
		.center();
	click_at(&mut h, mute);
	h.run_steps(2);
	let s = &h.state().state;
	assert!(s.project().tracks[0].channel.mute);
	assert_eq!(s.document().history_labels().len(), history + 1);
	// Clip properties from the clip menu's builder.
	let clip = s
		.project()
		.tracks
		.iter()
		.find_map(|t| t.clips.first())
		.unwrap()
		.id;
	assert!(h.state_mut().state.open_clip_window(clip));
	h.run_steps(3);
	assert!(h.state().window_rect(WindowItem::Clip(clip)).is_some());
	assert_eq!(h.state().state.view.windows.open.len(), 2);
}

/// `CADENZA_SCREENSHOT_DIR=dir cargo test -p cadenza-app --release --test ui -- --ignored
/// render_device_popout_screens`: the EQ popped out over the arrange (15f) and the dock
/// header at full width with A/B and the undo hint.
#[test]
#[ignore = "renders PNGs (wgpu); set CADENZA_SCREENSHOT_DIR"]
fn render_device_popout_screens() {
	let Some(dir) = std::env::var_os("CADENZA_SCREENSHOT_DIR").map(PathBuf::from) else {
		return;
	};
	std::fs::create_dir_all(&dir).unwrap();
	let mut a = app();
	let r = eq_insert(&a.state);
	shape_eq(&mut a.state, r);
	a.state.view.panels.set(PanelKind::Inspector, false);
	a.state.view.panels.set(PanelKind::Mixer, false);
	assert!(a.state.open_device_in_dock(r));
	a.state.pop_out_device_editor();
	a.state.close_piano_roll();
	let (pre, post) = eq_analyzer(&a.state, r);
	let mut h = app_harness(a, vec2(1920.0, 1000.0), true);
	h.run_steps(4);
	cadenza_app::ui::devices::eq::set_analyzer(&h.ctx, "dock", r, &pre, &post);
	h.run_steps(2);
	h.render()
		.unwrap()
		.save(dir.join("device_popout_eq.png"))
		.unwrap();

	let mut a = app();
	let r = eq_insert(&a.state);
	shape_eq(&mut a.state, r);
	a.state.view.panels.set(PanelKind::Inspector, false);
	assert!(a.state.open_device_in_dock(r));
	a.state
		.switch_compare(r, cadenza_app::state::device_ab::Slot::B);
	a.state
		.commit_params(r.track, r.device, &[("peak2_gain", 4.0)], "Peak 2 gain");
	let (pre, post) = eq_analyzer(&a.state, r);
	let mut h = app_harness(a, vec2(1920.0, 1000.0), true);
	h.run_steps(4);
	cadenza_app::ui::devices::eq::set_analyzer(&h.ctx, "dock", r, &pre, &post);
	h.run_steps(2);
	h.render()
		.unwrap()
		.save(dir.join("device_dock_ab_undo.png"))
		.unwrap();
}

/// `CADENZA_SCREENSHOT_DIR=dir cargo test -p cadenza-app --release --test ui -- --ignored
/// render_floating_window_screens`: FL-style floating windows (T37) — the EQ and the
/// compressor over the arrange and mixer, then a track window, inspector hidden.
#[test]
#[ignore = "renders PNGs (wgpu); set CADENZA_SCREENSHOT_DIR"]
fn render_floating_window_screens() {
	use cadenza_app::state::windows::WinRect;
	use cadenza_app::ui::devices::compressor;
	let Some(dir) = std::env::var_os("CADENZA_SCREENSHOT_DIR").map(PathBuf::from) else {
		return;
	};
	std::fs::create_dir_all(&dir).unwrap();
	let mut a = app();
	let eq = eq_insert(&a.state);
	shape_eq(&mut a.state, eq);
	let comp = bass_compressor(&a.state);
	a.state.commit_params(
		comp.track,
		comp.device,
		&[("threshold", -24.0), ("ratio", 4.0), ("makeup", 4.0)],
		"Compress",
	);
	let saved = &mut a.state.view.windows.saved.kinds;
	saved.insert(
		"device/eq".into(),
		WinRect::new(150.0, 110.0, 1000.0, 430.0),
	);
	saved.insert(
		"device/compressor".into(),
		WinRect::new(720.0, 330.0, 840.0, 400.0),
	);
	assert!(a.state.open_device_editor(eq));
	assert!(a.state.open_device_editor(comp));
	let (pre, post) = eq_analyzer(&a.state, eq);
	let (history, gr) = pump_history(-24.0, 4.0, 6.0, 4.0);
	let mut h = app_harness(a, vec2(1600.0, 1000.0), true);
	h.run_steps(4);
	cadenza_app::ui::devices::eq::set_analyzer(&h.ctx, "dock", eq, &pre, &post);
	compressor::set_display(&h.ctx, "dock", comp, history, -12.0, gr);
	h.run_steps(3);
	h.render()
		.unwrap()
		.save(dir.join("floating_eq_compressor.png"))
		.unwrap();
	// A track's properties next to them.
	let track = h.state().state.project().tracks[0].id;
	h.state_mut().state.open_track_window(Some(track));
	h.run_steps(4);
	h.render()
		.unwrap()
		.save(dir.join("floating_track_window.png"))
		.unwrap();
	// A clip's properties.
	let clip = h
		.state()
		.state
		.project()
		.tracks
		.iter()
		.find_map(|t| t.clips.first())
		.unwrap()
		.id;
	h.state_mut().state.open_clip_window(clip);
	h.run_steps(4);
	h.render()
		.unwrap()
		.save(dir.join("floating_clip_window.png"))
		.unwrap();
}

// --- AI changes on the device editor (T11, design 15e) ----------------------------------

/// The example song's EQ with the design's vocal settings, open in the dock, and an AI
/// that answers each request by moving `values` (one list per request).
fn ai_eq_app(values: &[&str], dir: &std::path::Path) -> (CadenzaApp, DeviceRef) {
	use cadenza_agent::scripted::ScriptedTurn;
	let mut a = app();
	let r = eq_insert(&a.state);
	shape_eq(&mut a.state, r);
	a.state.view.panels.set(PanelKind::Inspector, false);
	assert!(a.state.open_device_in_dock(r));
	let track = r.track.unwrap();
	let mut turns = Vec::new();
	for v in values {
		let args = format!(
			r#"{{"track_id":"{track}","device_id":"{}","params":{v}}}"#,
			r.device
		);
		turns.push(ScriptedTurn::tools(
			Some("Opening up the top end."),
			&[("set_device_params", args.as_str())],
		));
		turns.push(ScriptedTurn::text(
			"Raised the high shelf for air and cleared some boxiness at 320 Hz.",
		));
	}
	(with_chat(a, turns, 0, dir), r)
}

fn wait_ai(h: &mut Harness<'_, CadenzaApp>) {
	for _ in 0..800 {
		h.run_steps(1);
		if !h.state().state.ai.busy() {
			break;
		}
		std::thread::sleep(std::time::Duration::from_millis(5));
	}
	assert!(!h.state().state.ai.busy(), "the request finished");
	h.run_steps(3);
}

const BRIGHTER: &str = r#"{"high_shelf_gain":6.0,"high_shelf_freq":8000.0,"peak1_gain":-5.0}"#;

#[test]
fn asking_ai_on_the_eq_marks_its_changes_keep_clears_and_undo_is_one_step() {
	let tmp = tempfile::tempdir().unwrap();
	let (mut a, r) = ai_eq_app(&[BRIGHTER, r#"{"high_shelf_gain":7.5}"#], tmp.path());
	// Docked: an overlay drawer would cover the editor's AI card at 1600 px.
	a.state.view.panels.chat_pinned = true;
	let mut h = app_harness(a, vec2(1600.0, 1000.0), false);
	h.run_steps(3);
	h.get_by_label("Ask AI").click();
	h.run_steps(2);
	assert!(
		h.query_by_label("Make it brighter").is_some(),
		"EQ suggestions"
	);
	h.state_mut().state.view.panels.focus = Some(PanelKind::PianoRoll);
	h.get_by_label("Clean up the low end");
	h.ctx.memory_mut(|m| {
		m.request_focus(cadenza_app::ui::device_ai::input_id(r));
	});
	h.run_steps(1);
	h.event(egui::Event::Text("make the vocal brighter".into()));
	h.run_steps(1);
	h.key_press(Key::Enter);
	h.run_steps(1);
	assert!(
		h.state().state.view.panels.visible(PanelKind::Chat),
		"chat opens"
	);
	wait_ai(&mut h);
	{
		let s = &h.state().state;
		let c = s.ai_changes.as_ref().expect("changes recorded");
		assert_eq!(c.request, "make the vocal brighter");
		assert_eq!(s.ai_changes_of(r.device).len(), 3);
		let hs = s.ai_change(r.device, "high_shelf_gain").unwrap();
		assert_eq!((hs.before, hs.after), (3.0, 6.0));
		assert!(
			s.undo_label()
				.unwrap()
				.starts_with("AI: make the vocal brighter (Parametric EQ on"),
			"one AI step naming the device: {:?}",
			s.undo_label()
		);
	}
	assert!(h.query_by_label_contains("AI changed 3 params").is_some());
	assert!(h.query_by_label("Hold to hear before").is_some());
	h.get_by_label("Keep").click();
	h.run_steps(2);
	assert!(h.state().state.ai_changes.is_none());
	assert!(h.query_by_label_contains("AI changed").is_none());
	assert_eq!(eq_param(&h.state().state, r, "high_shelf_gain"), 6.0);

	// A second request from a chip; Undo on its card takes it back in one step.
	h.get_by_label("Ask AI").click();
	h.run_steps(2);
	h.get_by_label("Make it brighter").click();
	h.run_steps(1);
	wait_ai(&mut h);
	assert_eq!(
		h.state().state.ai_changes_of(r.device).len(),
		1,
		"only the param this request moved"
	);
	let history = h.state().state.document().history_labels().len();
	let undo = if cfg!(target_os = "macos") {
		"Undo  ⌘Z"
	} else {
		"Undo  Ctrl+Z"
	};
	h.get_by_label(undo).click();
	h.run_steps(2);
	let s = &h.state().state;
	assert_eq!(s.document().history_labels().len(), history - 1);
	assert_eq!(eq_param(s, r, "high_shelf_gain"), 6.0);
	assert!(s.ai_changes.is_none());
}

#[test]
fn holding_hear_before_on_the_card_leaves_the_document_alone() {
	let tmp = tempfile::tempdir().unwrap();
	let (a, r) = ai_eq_app(&[BRIGHTER], tmp.path());
	let mut h = app_harness(a, vec2(1600.0, 1000.0), false);
	h.run_steps(2);
	assert!(h.state_mut().state.send_ai("brighter"));
	wait_ai(&mut h);
	let project = h.state().state.project().clone();
	let history = h.state().state.document().history_labels().len();
	let c = h.get_by_label("Hold to hear before").rect().center();
	h.hover_at(c);
	h.run_steps(1);
	h.event(egui::Event::PointerButton {
		pos: c,
		button: egui::PointerButton::Primary,
		pressed: true,
		modifiers: Modifiers::NONE,
	});
	h.run_steps(3);
	assert_eq!(h.state().state.hearing_before(), Some(r.device));
	assert_eq!(h.state().state.project(), &project);
	h.event(egui::Event::PointerButton {
		pos: c,
		button: egui::PointerButton::Primary,
		pressed: false,
		modifiers: Modifiers::NONE,
	});
	h.run_steps(3);
	let s = &h.state().state;
	assert_eq!(s.hearing_before(), None);
	assert_eq!(s.project(), &project);
	assert_eq!(s.document().history_labels().len(), history);
	assert!(s.ai_changes.is_some());
}

#[test]
#[ignore = "renders PNGs (wgpu); set CADENZA_SCREENSHOT_DIR"]
fn render_ai_device_screens() {
	let Some(dir) = std::env::var_os("CADENZA_SCREENSHOT_DIR").map(PathBuf::from) else {
		return;
	};
	std::fs::create_dir_all(&dir).unwrap();
	let tmp = tempfile::tempdir().unwrap();
	let (mut a, r) = ai_eq_app(
		&[
			r#"{"high_shelf_gain":6.0,"high_shelf_freq":8000.0,"peak1_gain":-5.0,"peak3_freq":5200.0,"peak3_gain":-3.5}"#,
		],
		tmp.path(),
	);
	a.state.view.panels.focus = Some(PanelKind::PianoRoll);
	let (pre, post) = eq_analyzer(&a.state, r);
	let mut h = app_harness(a, vec2(1920.0, 1000.0), true);
	h.run_steps(3);
	// The Ask AI popover with the EQ's suggestions.
	h.get_by_label("Ask AI").click();
	h.run_steps(2);
	h.ctx.memory_mut(|m| {
		m.request_focus(cadenza_app::ui::device_ai::input_id(r));
	});
	h.run_steps(1);
	h.event(egui::Event::Text("make the vocal brighter".into()));
	cadenza_app::ui::devices::eq::set_analyzer(&h.ctx, "dock", r, &pre, &post);
	h.run_steps(2);
	h.render()
		.unwrap()
		.save(dir.join("ai_device_ask.png"))
		.unwrap();
	h.key_press(Key::Enter);
	h.run_steps(1);
	wait_ai(&mut h);
	// 15e: rings on the moved bands, the dashed "before" curve and the card.
	h.state_mut().state.view.panels.set(PanelKind::Chat, false);
	let (pre, post) = eq_analyzer(&h.state().state, r);
	cadenza_app::ui::devices::eq::set_analyzer(&h.ctx, "dock", r, &pre, &post);
	h.get_by_label("Band 3").click();
	h.run_steps(2);
	cadenza_app::ui::devices::eq::set_analyzer(&h.ctx, "dock", r, &pre, &post);
	h.run_steps(1);
	h.render()
		.unwrap()
		.save(dir.join("ai_device_changes.png"))
		.unwrap();
	// The generic body (reverb) with an AI change: ring + delta tag on the knob.
	let reverb = {
		let s = &mut h.state_mut().state;
		let p = s.project().clone();
		let t = p.tracks.iter().find(|t| {
			t.channel
				.inserts
				.iter()
				.any(|d| d.builtin_id() == Some("reverb"))
		});
		t.map(|t| DeviceRef {
			track: Some(t.id),
			device: t
				.channel
				.inserts
				.iter()
				.find(|d| d.builtin_id() == Some("reverb"))
				.unwrap()
				.id,
		})
	};
	if let Some(rv) = reverb {
		let s = &mut h.state_mut().state;
		s.keep_ai_changes();
		let before = s.project().clone();
		s.commit_param(rv.track, rv.device, "decay", 3.2);
		s.ai_changes = cadenza_app::state::ai_changes::AiChanges::new(
			"AI: longer tail",
			"longer, darker tail",
			s.document().history_labels().len(),
			&cadenza_app::state::ai_changes::snapshot(&before),
			s.project(),
		);
		assert!(s.open_device_in_dock(rv));
		h.run_steps(3);
		h.render()
			.unwrap()
			.save(dir.join("ai_device_generic.png"))
			.unwrap();
	}
}

fn rv_param(state: &AppState, r: DeviceRef, id: &str) -> f32 {
	state.param_value(r.track, r.device, id).unwrap_or_else(|| {
		cadenza_dsp::device_info("reverb")
			.and_then(|i| i.param(id))
			.unwrap()
			.default
	})
}

fn dl_param(state: &AppState, r: DeviceRef, id: &str) -> f32 {
	state.param_value(r.track, r.device, id).unwrap_or_else(|| {
		cadenza_dsp::device_info("delay")
			.and_then(|i| i.param(id))
			.unwrap()
			.default
	})
}

#[test]
fn dragging_the_reverb_decay_node_makes_one_undo_step() {
	let mut a = app();
	let r = reverb_insert(&a.state);
	a.state.view.panels.set(PanelKind::Inspector, false);
	assert!(a.state.open_device_in_dock(r));
	let mut h = app_harness(a, vec2(1600.0, 1000.0), false);
	h.run_steps(3);
	let history = |h: &Harness<'_, CadenzaApp>| h.state().state.document().history_labels().len();
	let before = history(&h);
	let decay = rv_param(&h.state().state, r, "decay");
	let c = h.get_by_label("Decay node").rect().center();
	h.hover_at(c);
	h.run_steps(1);
	h.drag_at(c);
	h.run_steps(1);
	for d in [10.0, 30.0, 60.0] {
		h.hover_at(c + vec2(d, 0.0));
		h.run_steps(1);
	}
	assert_eq!(history(&h), before, "nothing committed while dragging");
	assert!(
		rv_param(&h.state().state, r, "decay") > decay,
		"the drag previews a longer decay"
	);
	h.drop_at(c + vec2(60.0, 0.0));
	h.run_steps(2);
	let s = &h.state().state;
	assert_eq!(
		s.document().history_labels().len(),
		before + 1,
		"one undo step"
	);
	assert_eq!(s.undo_label(), Some("Reverb decay"));
	assert!(
		rv_param(s, r, "decay") > decay,
		"{}",
		rv_param(s, r, "decay")
	);
	h.state_mut().state.undo();
	h.run_steps(1);
	assert_eq!(rv_param(&h.state().state, r, "decay"), decay);
}

#[test]
fn dragging_the_delays_first_echo_changes_the_time_in_one_undo_step() {
	let mut a = app();
	let r = delay_insert(&a.state);
	// Free mode: dragging the first echo sets `time` directly (no snapping).
	a.state.commit_param(r.track, r.device, "sync", 0.0);
	a.state.view.panels.set(PanelKind::Inspector, false);
	assert!(a.state.open_device_in_dock(r));
	let mut h = app_harness(a, vec2(1600.0, 1000.0), false);
	h.run_steps(3);
	let history = |h: &Harness<'_, CadenzaApp>| h.state().state.document().history_labels().len();
	let before = history(&h);
	let time = dl_param(&h.state().state, r, "time");
	let c = h.get_by_label("First echo").rect().center();
	h.hover_at(c);
	h.run_steps(1);
	h.drag_at(c);
	h.run_steps(1);
	for d in [10.0, 40.0, 80.0] {
		h.hover_at(c + vec2(d, 0.0));
		h.run_steps(1);
	}
	assert_eq!(history(&h), before, "nothing committed while dragging");
	assert!(
		dl_param(&h.state().state, r, "time") > time,
		"the drag previews a longer time"
	);
	h.drop_at(c + vec2(80.0, 0.0));
	h.run_steps(2);
	let s = &h.state().state;
	assert_eq!(
		s.document().history_labels().len(),
		before + 1,
		"one undo step"
	);
	assert_eq!(s.undo_label(), Some("Delay echo"));
	assert!(dl_param(s, r, "time") > time, "{}", dl_param(s, r, "time"));
	h.state_mut().state.undo();
	h.run_steps(1);
	assert_eq!(dl_param(&h.state().state, r, "time"), time);
}

/// `CADENZA_SCREENSHOT_DIR=dir cargo test -p cadenza-app --release --test ui -- --ignored
/// render_reverb_delay_screens`: the reverb dock on a bus, the delay dock with sync 1/8 +
/// ping-pong, and one compact form in the inspector.
#[test]
#[ignore = "renders PNGs (wgpu); set CADENZA_SCREENSHOT_DIR"]
fn render_reverb_delay_screens() {
	let Some(dir) = std::env::var_os("CADENZA_SCREENSHOT_DIR").map(PathBuf::from) else {
		return;
	};
	std::fs::create_dir_all(&dir).unwrap();
	let shot = |h: &mut Harness<'_, CadenzaApp>, name: &str| {
		h.run_steps(4);
		h.render()
			.unwrap()
			.save(dir.join(format!("{name}.png")))
			.unwrap();
	};

	// 16b: the reverb dock on its bus, at the design width and a narrow window.
	for (width, name) in [(1920.0, "reverb_dock"), (1100.0, "reverb_dock_narrow")] {
		let mut a = app();
		let r = reverb_insert(&a.state);
		a.state.commit_params(
			r.track,
			r.device,
			&[
				("size", 62.0),
				("decay", 2.4),
				("damping", 40.0),
				("pre_delay", 40.0),
				("width", 80.0),
				("low_cut", 180.0),
			],
			"Reverb shape",
		);
		a.state.view.panels.set(PanelKind::Inspector, false);
		a.state.view.panels.set(PanelKind::Browser, false);
		assert!(a.state.open_device_in_dock(r));
		let mut h = app_harness(a, vec2(width, 1000.0), true);
		h.run_steps(2);
		cadenza_app::ui::devices::reverb::set_display(&h.ctx, "dock", r, [-9.0, -11.0]);
		shot(&mut h, name);
	}

	// 16c: the delay dock, sync 1/8 with ping-pong on.
	for (width, name) in [(1920.0, "delay_dock"), (1100.0, "delay_dock_narrow")] {
		let mut a = app();
		let r = delay_insert(&a.state);
		a.state.commit_params(
			r.track,
			r.device,
			&[
				("sync", 6.0),
				("feedback", 60.0),
				("ping_pong", 1.0),
				("high_cut", 6_000.0),
				("low_cut", 200.0),
			],
			"Delay shape",
		);
		a.state.view.panels.set(PanelKind::Inspector, false);
		a.state.view.panels.set(PanelKind::Browser, false);
		assert!(a.state.open_device_in_dock(r));
		let mut h = app_harness(a, vec2(width, 1000.0), true);
		shot(&mut h, name);
	}

	// The compact forms in the inspector.
	let mut a = app();
	let r = reverb_insert(&a.state);
	a.state.commit_params(
		r.track,
		r.device,
		&[("size", 62.0), ("decay", 2.4), ("damping", 40.0)],
		"Reverb shape",
	);
	a.state.selection.select_device(r);
	a.state.view.panels.set(PanelKind::Inspector, true);
	a.state.view.panels.set(PanelKind::Mixer, false);
	let mut h = app_harness(a, vec2(1600.0, 1000.0), true);
	shot(&mut h, "reverb_inspector_compact");

	let mut a = app();
	let r = delay_insert(&a.state);
	a.state.commit_params(
		r.track,
		r.device,
		&[("sync", 6.0), ("feedback", 60.0), ("ping_pong", 1.0)],
		"Delay shape",
	);
	a.state.selection.select_device(r);
	a.state.view.panels.set(PanelKind::Inspector, true);
	a.state.view.panels.set(PanelKind::Mixer, false);
	let mut h = app_harness(a, vec2(1600.0, 1000.0), true);
	shot(&mut h, "delay_inspector_compact");
}

/// `id` inserted on the track named `track` (one undo step), or the existing one.
fn insert_builtin(state: &mut AppState, track: &str, id: &str, name: &str) -> DeviceRef {
	let t = state
		.project()
		.tracks
		.iter()
		.find(|t| t.name == track)
		.unwrap_or_else(|| panic!("the example song has a {track} track"));
	let tid = t.id;
	if let Some(d) = t
		.channel
		.inserts
		.iter()
		.find(|d| d.builtin_id() == Some(id))
	{
		return DeviceRef {
			track: Some(tid),
			device: d.id,
		};
	}
	let device = cadenza_model::Device::builtin(id, name);
	let did = device.id;
	assert!(state.apply(cadenza_model::Command::InsertDevice {
		track: Some(tid),
		device,
		index: None,
	}));
	DeviceRef {
		track: Some(tid),
		device: did,
	}
}

fn builtin_param(state: &AppState, r: DeviceRef, device: &str, id: &str) -> f32 {
	state.param_value(r.track, r.device, id).unwrap_or_else(|| {
		cadenza_dsp::device_info(device)
			.and_then(|i| i.param(id))
			.unwrap()
			.default
	})
}

/// The app with `id` on `track` open in the dock (inspector and browser hidden).
fn effect_dock_app(track: &str, id: &str, name: &str) -> (CadenzaApp, DeviceRef) {
	let mut a = app();
	let r = insert_builtin(&mut a.state, track, id, name);
	a.state.view.panels.set(PanelKind::Inspector, false);
	a.state.view.panels.set(PanelKind::Browser, false);
	assert!(a.state.open_device_in_dock(r));
	(a, r)
}

fn drag_by(h: &mut Harness<'_, CadenzaApp>, from: egui::Pos2, steps: &[egui::Vec2]) {
	h.hover_at(from);
	h.run_steps(1);
	h.drag_at(from);
	h.run_steps(1);
	for d in steps {
		h.hover_at(from + *d);
		h.run_steps(1);
	}
	h.drop_at(from + *steps.last().unwrap());
	h.run_steps(2);
}

#[test]
fn dragging_the_saturator_curve_changes_drive_in_one_undo_step() {
	let (a, r) = effect_dock_app("Bass", "saturator", "Saturator");
	let mut h = app_harness(a, vec2(1920.0, 1000.0), false);
	h.run_steps(3);
	let before = h.state().state.document().history_labels().len();
	let drive = builtin_param(&h.state().state, r, "saturator", "drive");
	let c = h.get_by_label("Shaping curve").rect().center();
	let c = pos2(c.x as f32, c.y as f32);
	drag_by(
		&mut h,
		c,
		&[vec2(0.0, -10.0), vec2(0.0, -25.0), vec2(0.0, -40.0)],
	);
	let s = &h.state().state;
	assert_eq!(
		s.document().history_labels().len(),
		before + 1,
		"one undo step"
	);
	assert_eq!(s.undo_label(), Some("Saturator drive"));
	let now = builtin_param(s, r, "saturator", "drive");
	assert!(now > drive, "{drive} -> {now}");
	assert_eq!((now * 2.0).fract(), 0.0, "0.5 dB steps: {now}");
	h.state_mut().state.undo();
	h.run_steps(1);
	assert_eq!(
		builtin_param(&h.state().state, r, "saturator", "drive"),
		drive
	);
}

#[test]
fn clicking_the_tube_card_is_one_undo_step() {
	let (a, r) = effect_dock_app("Bass", "saturator", "Saturator");
	let mut h = app_harness(a, vec2(1920.0, 1000.0), false);
	h.run_steps(3);
	let before = h.state().state.document().history_labels().len();
	assert_eq!(builtin_param(&h.state().state, r, "saturator", "mode"), 0.0);
	h.get_by_label("Tube").click();
	h.run_steps(2);
	let s = &h.state().state;
	assert_eq!(builtin_param(s, r, "saturator", "mode"), 1.0);
	assert_eq!(s.document().history_labels().len(), before + 1);
	// Clicking the selected card again changes nothing; 3 picks Hard clip by key.
	h.get_by_label("Tube").click();
	h.run_steps(2);
	assert_eq!(
		h.state().state.document().history_labels().len(),
		before + 1
	);
	h.state_mut().state.view.panels.focus = Some(PanelKind::PianoRoll);
	h.key_press(Key::Num3);
	h.run_steps(2);
	assert_eq!(builtin_param(&h.state().state, r, "saturator", "mode"), 2.0);
}

#[test]
fn dragging_the_goniometer_changes_pan_and_width_in_one_undo_step() {
	let (a, r) = effect_dock_app("Pad", "gain", "Gain / Utility");
	let mut h = app_harness(a, vec2(1920.0, 1000.0), false);
	h.run_steps(3);
	let before = h.state().state.document().history_labels().len();
	let c = h.get_by_label("Goniometer").rect().center();
	let c = pos2(c.x as f32, c.y as f32);
	drag_by(
		&mut h,
		c,
		&[vec2(10.0, -10.0), vec2(25.0, -25.0), vec2(30.0, -30.0)],
	);
	let s = &h.state().state;
	assert_eq!(
		s.document().history_labels().len(),
		before + 1,
		"one undo step"
	);
	assert_eq!(s.undo_label(), Some("Gain pan and width"));
	assert!(builtin_param(s, r, "gain", "pan") > 0.0);
	assert!(builtin_param(s, r, "gain", "width") > 100.0);
	h.state_mut().state.undo();
	h.run_steps(1);
	let s = &h.state().state;
	assert_eq!(builtin_param(s, r, "gain", "pan"), 0.0);
	assert_eq!(builtin_param(s, r, "gain", "width"), 100.0);
	// M = mono while the editor has the focus.
	h.state_mut().state.view.panels.focus = Some(PanelKind::PianoRoll);
	h.key_press(Key::M);
	h.run_steps(2);
	assert_eq!(builtin_param(&h.state().state, r, "gain", "width"), 0.0);
}

#[test]
fn dragging_the_chorus_wave_changes_rate_and_depth_in_one_undo_step() {
	let (a, r) = effect_dock_app("Lead", "chorus", "Chorus");
	let mut h = app_harness(a, vec2(1920.0, 1000.0), false);
	h.run_steps(3);
	let before = h.state().state.document().history_labels().len();
	let rate = builtin_param(&h.state().state, r, "chorus", "rate");
	let depth = builtin_param(&h.state().state, r, "chorus", "depth");
	let c = h.get_by_label("Chorus LFO").rect().center();
	let c = pos2(c.x as f32, c.y as f32);
	drag_by(
		&mut h,
		c,
		&[vec2(20.0, -5.0), vec2(60.0, -15.0), vec2(80.0, -20.0)],
	);
	let s = &h.state().state;
	assert_eq!(s.document().history_labels().len(), before + 1);
	assert_eq!(s.undo_label(), Some("Chorus rate and depth"));
	assert!(builtin_param(s, r, "chorus", "rate") > rate);
	assert!(builtin_param(s, r, "chorus", "depth") > depth);
}

/// Stereo program for the goniometer (a pad with some side, its right channel
/// recorded in reverse polarity) as the probe delivers the gain device's output (run
/// through the real DSP).
fn utility_frames(params: &[(usize, f32)], seconds: f32) -> Vec<[f32; 2]> {
	use cadenza_dsp::Transport;
	let rate = 48_000.0f32;
	let mut p = cadenza_dsp::create("gain").unwrap();
	p.prepare(rate, 64);
	for &(i, v) in params {
		p.set_param(i, v);
	}
	let tau = std::f32::consts::TAU;
	let n = (seconds * rate) as usize;
	let mut seed = 21u32;
	let mut noise = || {
		seed ^= seed << 13;
		seed ^= seed >> 17;
		seed ^= seed << 5;
		seed as f32 / u32::MAX as f32 * 2.0 - 1.0
	};
	let mut out = Vec::with_capacity(n);
	let mut l = vec![0.0f32; 64];
	let mut r = vec![0.0f32; 64];
	let (mut lp_m, mut lp_s) = (0.0f32, 0.0f32);
	for block in 0..n / 64 {
		for k in 0..64 {
			let t = (block * 64 + k) as f32 / rate;
			lp_m += 0.08 * (noise() - lp_m);
			lp_s += 0.08 * (noise() - lp_s);
			let m = 0.22 * (tau * 110.0 * t).sin() + 0.12 * (tau * 164.8 * t).sin() + 1.1 * lp_m;
			let s = 0.06 * (tau * 277.2 * t).sin() + 0.4 * lp_s;
			// A mis-wired right channel: Invert R puts it back in phase.
			l[k] = m + s;
			r[k] = -(m - s);
		}
		p.process(&mut l, &mut r, &[], &Transport::default());
		out.extend(l.iter().zip(&r).map(|(a, b)| [*a, *b]));
	}
	out
}

/// `CADENZA_SCREENSHOT_DIR=dir cargo test -p cadenza-app --release --test ui -- --ignored
/// render_mod_effects_screens`: the chorus dock, the saturator dock (Tube, 12 dB drive),
/// the gain dock (Invert R on, a stereo program on the goniometer) and compact forms.
#[test]
#[ignore = "renders PNGs (wgpu); set CADENZA_SCREENSHOT_DIR"]
fn render_mod_effects_screens() {
	use cadenza_app::ui::devices::{chorus, gain};
	let Some(dir) = std::env::var_os("CADENZA_SCREENSHOT_DIR").map(PathBuf::from) else {
		return;
	};
	std::fs::create_dir_all(&dir).unwrap();
	let shot = |h: &mut Harness<'_, CadenzaApp>, name: &str| {
		h.run_steps(4);
		h.render()
			.unwrap()
			.save(dir.join(format!("{name}.png")))
			.unwrap();
	};
	let chorus_params = [
		("rate", 0.6),
		("depth", 46.0),
		("feedback", 20.0),
		("width", 70.0),
		("mix", 50.0),
	];
	let sat_params = [
		("mode", 1.0),
		("drive", 12.0),
		("tone", 20.0),
		("output", -1.5),
	];
	let gain_params = [("gain", -3.0), ("width", 120.0), ("invert_right", 1.0)];
	let frames = utility_frames(&[(0, -3.0), (2, 120.0), (4, 1.0)], 0.5);

	// 16d: the chorus, traces moving (as when audio passes), and stopped.
	for (width, name) in [(1920.0, "chorus_dock"), (1100.0, "chorus_dock_narrow")] {
		let (mut a, r) = effect_dock_app("Lead", "chorus", "Chorus");
		a.state
			.commit_params(r.track, r.device, &chorus_params, "Chorus shape");
		let mut h = app_harness(a, vec2(width, 1000.0), true);
		h.run_steps(2);
		chorus::set_display(&h.ctx, "dock", r, 0.3);
		shot(&mut h, name);
	}
	let (mut a, r) = effect_dock_app("Lead", "chorus", "Chorus");
	a.state
		.commit_params(r.track, r.device, &chorus_params, "Chorus shape");
	let mut h = app_harness(a, vec2(1920.0, 1000.0), true);
	shot(&mut h, "chorus_dock_stopped");

	// 16e: the saturator in Tube mode at 12 dB drive.
	for (width, name) in [
		(1920.0, "saturator_dock"),
		(1100.0, "saturator_dock_narrow"),
	] {
		let (mut a, r) = effect_dock_app("Bass", "saturator", "Saturator");
		a.state
			.commit_params(r.track, r.device, &sat_params, "Warm tube");
		let mut h = app_harness(a, vec2(width, 1000.0), true);
		shot(&mut h, name);
	}

	// 16f: gain with Invert R on, the program on the goniometer.
	for (width, name) in [(1920.0, "gain_dock"), (900.0, "gain_dock_natural")] {
		let (mut a, r) = effect_dock_app("Pad", "gain", "Gain / Utility");
		a.state
			.commit_params(r.track, r.device, &gain_params, "Utility");
		let mut h = app_harness(a, vec2(width, 1000.0), true);
		h.run_steps(2);
		gain::set_display(&h.ctx, "dock", r, &frames);
		shot(&mut h, name);
	}

	// The compact forms in the inspector.
	for (track, id, name, params, shot_name) in [
		(
			"Bass",
			"saturator",
			"Saturator",
			&sat_params[..],
			"saturator_inspector_compact",
		),
		(
			"Lead",
			"chorus",
			"Chorus",
			&chorus_params[..],
			"chorus_inspector_compact",
		),
		(
			"Pad",
			"gain",
			"Gain / Utility",
			&gain_params[..],
			"gain_inspector_compact",
		),
	] {
		let mut a = app();
		let r = insert_builtin(&mut a.state, track, id, name);
		a.state.commit_params(r.track, r.device, params, "Shape");
		a.state.selection.select_device(r);
		a.state.view.panels.set(PanelKind::Mixer, false);
		let mut h = app_harness(a, vec2(1600.0, 1000.0), true);
		h.run_steps(2);
		if id == "gain" {
			gain::set_display(&h.ctx, "inspector", r, &frames);
		}
		if id == "chorus" {
			chorus::set_display(&h.ctx, "inspector", r, 0.3);
		}
		shot(&mut h, shot_name);
	}
}

/// The app with the Lead track's Poly Synth open in the dock (inspector and browser
/// hidden).
fn synth_dock_app() -> (CadenzaApp, DeviceRef) {
	let mut a = app();
	let r = {
		let t = a
			.state
			.project()
			.tracks
			.iter()
			.find(|t| t.name == "Lead")
			.unwrap();
		let d = t.instrument.as_ref().unwrap();
		assert_eq!(d.builtin_id(), Some("poly_synth"));
		DeviceRef {
			track: Some(t.id),
			device: d.id,
		}
	};
	a.state.view.panels.set(PanelKind::Inspector, false);
	a.state.view.panels.set(PanelKind::Browser, false);
	assert!(a.state.open_device_in_dock(r));
	(a, r)
}

#[test]
fn dragging_the_poly_synth_filter_node_moves_cutoff_and_resonance_in_one_step() {
	let (a, r) = synth_dock_app();
	let mut h = app_harness(a, vec2(1920.0, 1000.0), false);
	h.run_steps(3);
	let before = h.state().state.document().history_labels().len();
	let param = |h: &Harness<'_, CadenzaApp>, id: &str| {
		builtin_param(&h.state().state, r, "poly_synth", id)
	};
	let (cutoff, res) = (param(&h, "filter_cutoff"), param(&h, "filter_resonance"));
	let c = h.get_by_label("Band F").rect().center();
	let c = pos2(c.x as f32, c.y as f32);
	drag_by(
		&mut h,
		c,
		&[vec2(5.0, -4.0), vec2(20.0, -12.0), vec2(40.0, -24.0)],
	);
	let s = &h.state().state;
	assert_eq!(
		s.document().history_labels().len(),
		before + 1,
		"one undo step"
	);
	assert_eq!(s.undo_label(), Some("Filter cutoff and resonance"));
	assert!(param(&h, "filter_cutoff") > cutoff * 1.05);
	assert!(param(&h, "filter_resonance") > res + 5.0);
	h.state_mut().state.undo();
	h.run_steps(1);
	assert_eq!(param(&h, "filter_cutoff"), cutoff);
	assert_eq!(param(&h, "filter_resonance"), res);
}

#[test]
fn clicking_the_saw_card_on_osc_1_is_one_undo_step() {
	let (mut a, r) = synth_dock_app();
	a.state.commit_param(r.track, r.device, "osc1_wave", 0.0);
	let mut h = app_harness(a, vec2(1920.0, 1000.0), false);
	h.run_steps(3);
	let before = h.state().state.document().history_labels().len();
	h.get_by_label("Osc 1 Saw").click();
	h.run_steps(2);
	let s = &h.state().state;
	assert_eq!(builtin_param(s, r, "poly_synth", "osc1_wave"), 1.0);
	assert_eq!(s.document().history_labels().len(), before + 1);
	// The selected card again changes nothing.
	h.get_by_label("Osc 1 Saw").click();
	h.run_steps(2);
	assert_eq!(
		h.state().state.document().history_labels().len(),
		before + 1
	);
}

#[test]
fn the_wavetable_card_shows_the_table_and_its_picture_drags_the_position() {
	let (mut a, r) = synth_dock_app();
	a.state.commit_param(r.track, r.device, "osc1_wave", 1.0);
	let mut h = app_harness(a, vec2(1920.0, 1000.0), false);
	h.run_steps(3);
	assert!(h.query_by_label("Osc 1 wavetable position").is_none());
	h.get_by_label("Osc 1 Wavetable").click();
	h.run_steps(3);
	let param = |h: &Harness<'_, CadenzaApp>, id: &str| {
		builtin_param(&h.state().state, r, "poly_synth", id)
	};
	assert_eq!(param(&h, "osc1_wave"), 4.0);
	// Pick the Formant table from the menu: one step.
	let before = h.state().state.document().history_labels().len();
	h.get_by_label("Osc 1 table").click();
	h.run_steps(2);
	h.get_by_label("Formant").click();
	h.run_steps(2);
	assert_eq!(param(&h, "osc1_table"), 2.0);
	assert_eq!(
		h.state().state.document().history_labels().len(),
		before + 1
	);
	// Dragging the waterfall up morphs later in the table: one step on release.
	let c = h.get_by_label("Osc 1 wavetable position").rect().center();
	let c = pos2(c.x as f32, c.y as f32);
	drag_by(
		&mut h,
		c,
		&[vec2(0.0, -10.0), vec2(0.0, -30.0), vec2(0.0, -40.0)],
	);
	let s = &h.state().state;
	assert_eq!(s.document().history_labels().len(), before + 2);
	let pos = param(&h, "osc1_position");
	assert!((pos - 0.25).abs() < 0.02, "40 px of 160: {pos}");
	h.state_mut().state.undo();
	h.run_steps(1);
	assert_eq!(param(&h, "osc1_position"), 0.0);
}

#[test]
fn the_poly_synth_keyboard_plays_through_the_preview_path() {
	let (a, r) = synth_dock_app();
	let mut h = app_harness(a, vec2(1920.0, 1000.0), false);
	h.run_steps(3);
	let kb = h.get_by_label("Keyboard").rect();
	// The first white key (C3), low enough to miss the black keys.
	let at = pos2(kb.min.x as f32 + 8.0, kb.max.y as f32 - 6.0);
	h.hover_at(at);
	h.run_steps(1);
	h.drag_at(at);
	h.run_steps(2);
	assert_eq!(
		h.state().state.previewing,
		vec![(r.track.unwrap(), 48)],
		"the key sounds on the synth's track"
	);
	h.drop_at(at);
	h.run_steps(2);
	assert!(
		h.state().state.previewing.is_empty(),
		"released on mouse up"
	);

	// With the editor focused, A plays C3 and Z shifts the octave; nothing else hears them.
	h.state_mut().state.view.panels.focus = Some(PanelKind::PianoRoll);
	h.key_down(Key::A);
	h.run_steps(2);
	assert_eq!(h.state().state.previewing, vec![(r.track.unwrap(), 48)]);
	h.key_up(Key::A);
	h.run_steps(2);
	assert!(h.state().state.previewing.is_empty(), "released on key up");
	h.key_press(Key::Z);
	h.run_steps(1);
	h.key_down(Key::K);
	h.run_steps(2);
	assert_eq!(h.state().state.previewing, vec![(r.track.unwrap(), 48)]);
	// Focus elsewhere releases what the computer keys hold.
	h.state_mut().state.view.panels.focus = Some(PanelKind::Arrange);
	h.run_steps(2);
	assert!(
		h.state().state.previewing.is_empty(),
		"released on focus loss"
	);
	h.key_up(Key::K);
	h.run_steps(1);
}

/// A C3 E3 G3 pad chord through the real Poly Synth, mono, at 48 kHz.
fn synth_chord(params: &[(&str, f32)], seconds: f32) -> Vec<f32> {
	use cadenza_dsp::{MidiEvent, Transport};
	let info = cadenza_dsp::device_info("poly_synth").unwrap();
	let mut p = cadenza_dsp::create("poly_synth").unwrap();
	p.prepare(48_000.0, 64);
	for (id, v) in params {
		let i = info.params.iter().position(|q| q.id == *id).unwrap();
		p.set_param(i, *v);
	}
	let mut l = vec![0.0f32; 64];
	let mut rr = vec![0.0f32; 64];
	let on: Vec<MidiEvent> = [48u8, 52, 55]
		.iter()
		.map(|&pitch| MidiEvent::NoteOn {
			offset: 0,
			pitch,
			velocity: 0.8,
		})
		.collect();
	let mut out = Vec::new();
	let t = Transport::default();
	for block in 0..(seconds * 48_000.0 / 64.0) as usize {
		l.fill(0.0);
		rr.fill(0.0);
		p.process(&mut l, &mut rr, if block == 0 { &on } else { &[] }, &t);
		out.extend(l.iter().zip(&rr).map(|(a, b)| (a + b) * 0.5));
	}
	out
}

#[test]
#[ignore = "renders PNGs (wgpu); set CADENZA_SCREENSHOT_DIR"]
fn render_poly_synth_screens() {
	use cadenza_app::ui::devices::poly_synth;
	let Some(dir) = std::env::var_os("CADENZA_SCREENSHOT_DIR").map(PathBuf::from) else {
		return;
	};
	std::fs::create_dir_all(&dir).unwrap();
	let shot = |h: &mut Harness<'_, CadenzaApp>, name: &str| {
		h.run_steps(4);
		h.render()
			.unwrap()
			.save(dir.join(format!("{name}.png")))
			.unwrap();
	};
	// The design's warm saw pad (17a).
	let params = [
		("osc1_wave", 1.0),
		("osc2_wave", 2.0),
		("osc2_semitones", 7.0),
		("osc2_cents", -6.0),
		("osc_mix", 35.0),
		("unison", 3.0),
		("unison_detune", 18.0),
		("noise", 8.0),
		("filter_cutoff", 1800.0),
		("filter_resonance", 45.0),
		("filter_env_amount", 24.0),
		("amp_attack", 350.0),
		("amp_decay", 1200.0),
		("amp_sustain", 70.0),
		("amp_release", 2400.0),
		("filter_attack", 20.0),
		("filter_decay", 800.0),
		("filter_sustain", 30.0),
		("filter_release", 1500.0),
		("glide", 80.0),
		("gain", -6.0),
	];
	let mut clean = params.to_vec();
	clean.retain(|(id, _)| *id != "noise");
	let samples = synth_chord(&clean, 0.6);
	for (width, name) in [
		(1920.0, "poly_synth_dock"),
		(1280.0, "poly_synth_dock_1280"),
		(900.0, "poly_synth_dock_900"),
		(540.0, "poly_synth_dock_narrow"),
	] {
		let (mut a, r) = synth_dock_app();
		a.state
			.commit_params(r.track, r.device, &params, "Warm saw pad");
		let mut h = app_harness(a, vec2(width, 1000.0), true);
		h.run_steps(2);
		poly_synth::set_scope(&h.ctx, "dock", r, &samples);
		shot(&mut h, name);
		if width < 560.0 {
			// The stacked sections scroll: FILTER and the envelopes, then OUTPUT.
			let over = pos2(width * 0.5, 800.0);
			h.hover_at(over);
			for (step, shot_name) in [
				(1, "poly_synth_dock_narrow_filter"),
				(2, "poly_synth_dock_narrow_output"),
			] {
				for _ in 0..step * 3 {
					h.event(egui::Event::MouseWheel {
						unit: egui::MouseWheelUnit::Point,
						delta: vec2(0.0, -120.0),
						modifiers: Modifiers::NONE,
						phase: egui::TouchPhase::Move,
					});
					h.run_steps(2);
				}
				shot(&mut h, shot_name);
			}
		}
	}

	// The compact form in the inspector.
	let (mut a, r) = synth_dock_app();
	a.state.close_device_editor();
	a.state.view.panels.set(PanelKind::Inspector, true);
	a.state
		.commit_params(r.track, r.device, &params, "Warm saw pad");
	a.state.selection.select_device(r);
	a.state.view.panels.set(PanelKind::Mixer, false);
	a.state.view.panels.set(PanelKind::PianoRoll, false);
	let mut h = app_harness(a, vec2(1600.0, 1000.0), true);
	shot(&mut h, "poly_synth_inspector_compact");
}

/// The app with `track`'s instrument (built-in `id`) open in the dock (inspector and
/// browser hidden).
fn instrument_dock_app(track: &str, id: &str) -> (CadenzaApp, DeviceRef) {
	let mut a = app();
	let r = {
		let t = a
			.state
			.project()
			.tracks
			.iter()
			.find(|t| t.name == track)
			.unwrap();
		let d = t.instrument.as_ref().unwrap();
		assert_eq!(d.builtin_id(), Some(id));
		DeviceRef {
			track: Some(t.id),
			device: d.id,
		}
	};
	a.state.view.panels.set(PanelKind::Inspector, false);
	a.state.view.panels.set(PanelKind::Browser, false);
	assert!(a.state.open_device_in_dock(r));
	(a, r)
}

#[test]
fn dragging_a_drum_strip_level_bar_is_one_undo_step() {
	let (a, r) = instrument_dock_app("Drums", "drum_machine");
	let mut h = app_harness(a, vec2(1920.0, 1000.0), false);
	h.run_steps(3);
	let before = h.state().state.document().history_labels().len();
	let level = |h: &Harness<'_, CadenzaApp>| {
		builtin_param(&h.state().state, r, "drum_machine", "snare_level")
	};
	let start = level(&h);
	let c = h.get_by_label("Snare level").rect().center();
	let c = pos2(c.x as f32, c.y as f32);
	drag_by(
		&mut h,
		c,
		&[vec2(4.0, 0.0), vec2(10.0, 1.0), vec2(20.0, 2.0)],
	);
	let s = &h.state().state;
	assert_eq!(
		s.document().history_labels().len(),
		before + 1,
		"one undo step"
	);
	assert_eq!(s.undo_label(), Some("Snare level"));
	assert!(level(&h) > start + 3.0, "dragged right: {}", level(&h));
	h.state_mut().state.undo();
	h.run_steps(1);
	assert_eq!(level(&h), start);
}

#[test]
fn clicking_a_drum_pad_auditions_and_selects_the_voice() {
	use cadenza_app::ui::devices::drum_machine;
	let (a, r) = instrument_dock_app("Drums", "drum_machine");
	let track = r.track.unwrap();
	let mut h = app_harness(a, vec2(1920.0, 1000.0), false);
	h.run_steps(3);
	let history = h.state().state.document().history_labels().len();
	assert_eq!(drum_machine::selected_voice(&h.ctx, "dock", r), 0);
	let c = h.get_by_label("Snare pad").rect().center();
	let c = pos2(c.x as f32, c.y as f32);
	h.hover_at(c);
	h.run_steps(1);
	h.drag_at(c);
	h.run_steps(2);
	assert_eq!(
		h.state().state.previewing,
		vec![(track, 38)],
		"the snare sounds through the preview path"
	);
	assert_eq!(drum_machine::selected_voice(&h.ctx, "dock", r), 1);
	h.drop_at(c);
	h.run_steps(2);
	assert!(
		h.state().state.previewing.is_empty(),
		"released on mouse up"
	);

	// Ctrl / Cmd-click only selects.
	let clap = h.get_by_label("Clap pad").rect().center();
	let clap = pos2(clap.x as f32, clap.y as f32);
	h.hover_at(clap);
	h.run_steps(1);
	for pressed in [true, false] {
		h.event_modifiers(
			egui::Event::PointerButton {
				pos: clap,
				button: egui::PointerButton::Primary,
				pressed,
				modifiers: Modifiers::COMMAND,
			},
			Modifiers::COMMAND,
		);
		h.run_steps(1);
		assert!(h.state().state.previewing.is_empty(), "silent");
	}
	assert_eq!(drum_machine::selected_voice(&h.ctx, "dock", r), 2);

	// → / ← move between the voices while the editor has the focus.
	h.state_mut().state.view.panels.focus = Some(PanelKind::PianoRoll);
	h.key_press(Key::ArrowRight);
	h.run_steps(1);
	assert_eq!(drum_machine::selected_voice(&h.ctx, "dock", r), 3);
	h.key_press(Key::ArrowLeft);
	h.key_press(Key::ArrowLeft);
	h.run_steps(1);
	assert_eq!(drum_machine::selected_voice(&h.ctx, "dock", r), 1);
	assert_eq!(
		h.state().state.document().history_labels().len(),
		history,
		"auditions and selection are not edits"
	);
}

#[test]
fn dragging_an_electric_piano_big_knob_is_one_undo_step() {
	let (a, r) = instrument_dock_app("Keys", "electric_piano");
	let mut h = app_harness(a, vec2(1920.0, 1000.0), false);
	h.run_steps(3);
	let before = h.state().state.document().history_labels().len();
	let bell =
		|h: &Harness<'_, CadenzaApp>| builtin_param(&h.state().state, r, "electric_piano", "bell");
	let start = bell(&h);
	let knob = h
		.query_all_by_role_and_label(egui::accesskit::Role::Slider, "Bell")
		.next()
		.unwrap()
		.rect();
	let c = pos2(knob.center().x as f32, knob.center().y as f32);
	drag_by(
		&mut h,
		c,
		&[vec2(0.0, -10.0), vec2(0.0, -30.0), vec2(0.0, -50.0)],
	);
	let s = &h.state().state;
	assert_eq!(
		s.document().history_labels().len(),
		before + 1,
		"one undo step"
	);
	assert!(bell(&h) > start + 10.0, "dragged up: {}", bell(&h));
	h.state_mut().state.undo();
	h.run_steps(1);
	assert_eq!(bell(&h), start);
}

#[test]
fn the_electric_piano_keyboard_and_brightness_slider_work() {
	let (a, r) = instrument_dock_app("Keys", "electric_piano");
	let track = r.track.unwrap();
	let mut h = app_harness(a, vec2(1920.0, 1000.0), false);
	h.run_steps(3);
	h.state_mut().state.view.panels.focus = Some(PanelKind::PianoRoll);
	h.key_down(Key::A);
	h.run_steps(2);
	assert_eq!(h.state().state.previewing, vec![(track, 48)]);
	h.key_up(Key::A);
	h.run_steps(2);
	assert!(h.state().state.previewing.is_empty());
	let before = h.state().state.document().history_labels().len();
	let slider = h.get_by_label("Brightness slider").rect();
	let right = pos2(slider.max.x as f32 - 6.0, slider.center().y as f32);
	h.hover_at(right);
	h.run_steps(1);
	h.drag_at(right);
	h.run_steps(1);
	h.drop_at(right);
	h.run_steps(2);
	let b = builtin_param(&h.state().state, r, "electric_piano", "brightness");
	assert!(b > 90.0, "clicked near Bright: {b}");
	assert_eq!(
		h.state().state.document().history_labels().len(),
		before + 1
	);
}

#[test]
#[ignore = "renders PNGs (wgpu); set CADENZA_SCREENSHOT_DIR"]
fn render_ep_drum_screens() {
	use cadenza_app::ui::devices::drum_machine;
	let Some(dir) = std::env::var_os("CADENZA_SCREENSHOT_DIR").map(PathBuf::from) else {
		return;
	};
	std::fs::create_dir_all(&dir).unwrap();
	let shot = |h: &mut Harness<'_, CadenzaApp>, name: &str| {
		h.run_steps(4);
		h.render()
			.unwrap()
			.save(dir.join(format!("{name}.png")))
			.unwrap();
	};
	// 17b: a mellow suitcase with the tremolo on.
	let ep = [
		("brightness", 38.0),
		("bell", 55.0),
		("decay", 5_600.0),
		("release", 420.0),
		("tremolo_rate", 4.5),
		("tremolo_depth", 40.0),
		("gain", -3.0),
	];
	for (width, name) in [
		(1920.0, "ep_dock"),
		(1280.0, "ep_dock_1280"),
		(900.0, "ep_dock_900"),
		(540.0, "ep_dock_narrow"),
	] {
		let (mut a, r) = instrument_dock_app("Keys", "electric_piano");
		a.state.commit_params(r.track, r.device, &ep, "Suitcase");
		let mut h = app_harness(a, vec2(width, 1000.0), true);
		shot(&mut h, name);
	}
	// 17c: a boom bap kit, Kick selected, the hats mid-flash.
	let kit = [
		("kick_tune", -2.0),
		("kick_punch", 70.0),
		("kick_level", -1.0),
		("snare_tune", 1.0),
		("snare_snappy", 55.0),
		("snare_level", -2.5),
		("clap_level", -6.0),
		("hat_decay", 45.0),
		("open_hat_decay", 600.0),
		("hat_level", -5.0),
		("tom_tune", 3.0),
		("cymbal_level", -8.0),
		("perc_decay", 120.0),
		("perc_level", -7.0),
		("gain", -2.0),
	];
	for (width, name) in [
		(1920.0, "drum_machine_dock"),
		(1280.0, "drum_machine_dock_1280"),
		(640.0, "drum_machine_dock_narrow"),
	] {
		let (mut a, r) = instrument_dock_app("Drums", "drum_machine");
		a.state
			.commit_params(r.track, r.device, &kit, "Boom bap kit");
		let mut h = app_harness(a, vec2(width, 1000.0), true);
		h.run_steps(2);
		drum_machine::set_flash(&h.ctx, "dock", r, Some((3, 0.8)));
		shot(&mut h, name);
	}
	{
		// The hats selected with → from the kick.
		let (mut a, r) = instrument_dock_app("Drums", "drum_machine");
		a.state
			.commit_params(r.track, r.device, &kit, "Boom bap kit");
		let mut h = app_harness(a, vec2(1920.0, 1000.0), true);
		h.run_steps(2);
		h.state_mut().state.view.panels.focus = Some(PanelKind::PianoRoll);
		h.key_press(Key::ArrowRight);
		h.key_press(Key::ArrowRight);
		h.key_press(Key::ArrowRight);
		h.run_steps(1);
		shot(&mut h, "drum_machine_dock_hats");
	}

	// The compact forms in the inspector.
	for (track, id, params, name) in [
		("Keys", "electric_piano", &ep[..], "ep_inspector_compact"),
		(
			"Drums",
			"drum_machine",
			&kit[..],
			"drum_machine_inspector_compact",
		),
	] {
		let (mut a, r) = instrument_dock_app(track, id);
		a.state.close_device_editor();
		a.state.view.panels.set(PanelKind::Inspector, true);
		a.state.commit_params(r.track, r.device, params, "Shape");
		a.state.selection.select_device(r);
		a.state.view.panels.set(PanelKind::Mixer, false);
		a.state.view.panels.set(PanelKind::PianoRoll, false);
		let mut h = app_harness(a, vec2(1600.0, 1000.0), true);
		h.run_steps(2);
		if id == "drum_machine" {
			drum_machine::set_flash(&h.ctx, "inspector", r, Some((0, 0.9)));
		}
		shot(&mut h, name);
	}
}

/// The Keys track, its first clip and its instrument in the example.
fn keys_parts(state: &AppState) -> (cadenza_core::TrackId, cadenza_core::ClipId, DeviceRef) {
	let t = state
		.project()
		.tracks
		.iter()
		.find(|t| t.name == "Keys")
		.unwrap();
	(
		t.id,
		t.clips[0].id,
		DeviceRef {
			track: Some(t.id),
			device: t.instrument.as_ref().unwrap().id,
		},
	)
}

#[test]
fn inspector_tabs_follow_the_selection_and_edit_through_commands() {
	use cadenza_app::state::inspector::InspectorTab;
	let mut a = app();
	// The inspector at full height (the mixer docks below it by default).
	a.state.view.panels.set(PanelKind::Mixer, false);
	a.state.view.panels.set(PanelKind::Inspector, true);
	let (track, clip, piano) = keys_parts(&a.state);
	let mut h = harness(a);
	h.run_steps(2);
	assert_eq!(h.state().state.view.inspector.tab, InspectorTab::Track);
	h.state_mut().state.selection.click_clip(track, clip, false);
	h.run_steps(2);
	assert_eq!(h.state().state.view.inspector.tab, InspectorTab::Clip);
	assert!(h.query_by_label("Open in piano roll  F7").is_some());
	// The user picks the Track tab: it holds while the selection stays.
	h.get_by_label("Track tab").click();
	h.run_steps(2);
	assert_eq!(h.state().state.view.inspector.tab, InspectorTab::Track);
	h.get_by_label("Mute").click();
	h.run_steps(2);
	let s = &h.state().state;
	assert!(s.project().track(track).unwrap().channel.mute);
	assert_eq!(s.view.inspector.tab, InspectorTab::Track);
	// Clicking an insert of the Track tab shows it in the Device tab.
	h.get_by_label("Parametric EQ insert").click();
	h.run_steps(2);
	let s = &h.state().state;
	assert_eq!(s.view.inspector.tab, InspectorTab::Device);
	assert!(s.selection.device.is_some_and(|d| d.device != piano.device));
	// Selecting the instrument from elsewhere: the Device tab follows it.
	h.state_mut().state.selection.select_device(piano);
	h.run_steps(2);
	assert_eq!(h.state().state.view.inspector.tab, InspectorTab::Device);
	// Clip tab without one clip: a hint, nothing to edit.
	h.state_mut().state.selection = Default::default();
	h.run_steps(2);
	h.get_by_label("Clip tab").click();
	h.run_steps(2);
	assert_eq!(h.state().state.view.inspector.tab, InspectorTab::Clip);
	assert!(h.query_by_label("Open in piano roll  F7").is_none());
}

#[test]
fn typing_a_clip_length_in_the_inspector_is_one_undo_step() {
	let mut a = app();
	a.state.view.panels.set(PanelKind::Mixer, false);
	a.state.view.panels.set(PanelKind::Inspector, true);
	let (track, clip, _) = keys_parts(&a.state);
	let mut h = harness(a);
	h.state_mut().state.selection.click_clip(track, clip, false);
	h.run_steps(2);
	let before = h.state().state.project().find_clip(clip).unwrap().1.length;
	let id = egui::Id::new(("insp-clip-length", clip));
	h.ctx.memory_mut(|m| m.request_focus(id));
	h.run_steps(1);
	h.ctx
		.data_mut(|d| d.insert_temp(id.with("buf"), "6.0.000".to_owned()));
	h.run_steps(1);
	h.ctx.memory_mut(|m| m.surrender_focus(id));
	h.run_steps(2);
	let s = &h.state().state;
	let after = s.project().find_clip(clip).unwrap().1.length;
	assert_ne!(before, after);
	assert_eq!(after, Tick(PPQ * 24));
	s.undo_label().unwrap();
}

/// Inspector tabs (design 01 / 02): Track, Clip (MIDI and audio), Device, an empty tab and
/// the folded history.
#[test]
#[ignore]
fn render_inspector_screens() {
	let Some(dir) = std::env::var_os("CADENZA_SCREENSHOT_DIR").map(PathBuf::from) else {
		return;
	};
	std::fs::create_dir_all(&dir).unwrap();
	let mut a = app();
	a.state.view.panels.set(PanelKind::Inspector, true);
	let (track, clip, piano) = keys_parts(&a.state);
	// A few steps so the history has rows (one badged as the AI's).
	assert!(a.state.apply(cadenza_model::Command::Batch {
		label: "AI: add a bass".into(),
		commands: vec![cadenza_model::Command::SetChannel {
			track: Some(track),
			patch: cadenza_model::ChannelPatch {
				volume_db: Some(-7.0),
				..Default::default()
			},
		}],
	}));
	a.state.selection.select_track(track);
	let mut h = screenshot_harness(a);
	// The default layout: the mixer docked below, the history shrinks.
	shot(&mut h, &dir, "inspector_track_with_mixer");
	h.state_mut().state.view.panels.set(PanelKind::Mixer, false);
	shot(&mut h, &dir, "inspector_track");
	h.state_mut().state.selection.click_clip(track, clip, false);
	shot(&mut h, &dir, "inspector_clip_midi");
	h.state_mut().state.selection.select_device(piano);
	shot(&mut h, &dir, "inspector_device");
	{
		let s = &mut h.state_mut().state;
		s.selection = Default::default();
		s.view.inspector.history_folded = true;
	}
	// Nothing selected: the Track tab follows (project overview); then the user picks
	// the empty Device tab.
	h.run_steps(2);
	h.state_mut()
		.state
		.view
		.inspector
		.pick(cadenza_app::state::inspector::InspectorTab::Device);
	shot(&mut h, &dir, "inspector_empty_device");
	let wav = cadenza_core::paths::real_path(tone_wav(&dir)).unwrap();
	{
		let s = &mut h.state_mut().state;
		s.view.inspector.history_folded = false;
		s.import_audio_at(&wav, None, Some(Tick::ZERO));
		let clip = *s.selection.clips.iter().next().unwrap();
		let fade = Some(cadenza_model::ClipFade::new(
			0.25,
			cadenza_model::FadeCurve::EqualPower,
		));
		assert!(s.apply(cadenza_model::Command::SetClipFades {
			clip,
			fade_in: fade,
			fade_out: None,
		}));
	}
	shot(&mut h, &dir, "inspector_clip_audio");
	let audio = h.state().state.selection.track.unwrap();
	h.state_mut().state.selection.select_track(audio);
	h.state_mut().state.selection.clear_clips();
	shot(&mut h, &dir, "inspector_track_audio");
}

/// The main window at the design-12 sizes: arrange + mixer with the browser and the AI
/// panel open (docked, rail or overlay depending on the width).
#[test]
#[ignore]
fn render_responsive_screens() {
	let Some(dir) = std::env::var_os("CADENZA_SCREENSHOT_DIR").map(PathBuf::from) else {
		return;
	};
	std::fs::create_dir_all(&dir).unwrap();
	for (w, h) in [
		(1024.0, 700.0),
		(1280.0, 800.0),
		(1600.0, 1000.0),
		(2560.0, 1440.0),
	] {
		let mut a = app();
		let s = &mut a.state;
		s.view.panels.set(PanelKind::Browser, true);
		s.view.panels.set(PanelKind::Mixer, true);
		s.view.panels.set(PanelKind::Chat, true);
		let mut harness = app_harness(a, vec2(w, h), true);
		let name = format!("responsive_{w}x{h}");
		shot(&mut harness, &dir, &name);
		if w < 1600.0 {
			// The browser overlay next to its rail, then the transport's Modes popover.
			harness
				.state_mut()
				.state
				.view
				.panels
				.set(PanelKind::Chat, false);
			harness.get_by_label("Browser Devices").click();
			shot(&mut harness, &dir, &format!("{name}_browser"));
			harness.get_by_label("Modes").click();
			shot(&mut harness, &dir, &format!("{name}_modes"));
		}
	}
	// A short window with the inspector overlay and a floating track window.
	let mut a = app();
	let track = a.state.project().tracks[0].id;
	a.state.view.panels.set(PanelKind::Inspector, true);
	a.state.open_track_window(Some(track));
	let mut harness = app_harness(a, vec2(1024.0, 700.0), true);
	shot(&mut harness, &dir, "responsive_1024x700_inspector");
}

/// At 1024 px the transport folds its mode toggles into "Modes" and nothing overlaps;
/// the popover still reaches every option.
#[test]
fn transport_fits_a_1024_px_window_without_overlaps() {
	use cadenza_app::state::layout::TransportLayout;
	let mut h = app_harness(app(), vec2(1024.0, 700.0), false);
	h.run_steps(3);
	let layout = h.state().state.view.responsive.transport;
	assert_eq!(layout, TransportLayout::for_width(1024.0));
	assert!(layout.modes_folded && layout.width() <= 1024.0);
	assert!(h.query_by_label("Count-in").is_none());
	assert!(h.query_by_label("Metronome").is_none());
	let rects: Vec<egui::Rect> = ["Record (Ctrl+R)", "⟲", "Modes"]
		.iter()
		.map(|l| h.get_by_label(l).rect())
		.collect();
	for (i, a) in rects.iter().enumerate() {
		assert!(
			a.right() <= 1024.0 - layout.meter,
			"{a:?} runs into the meter"
		);
		for b in &rects[i + 1..] {
			assert!(!a.intersects(*b), "{a:?} overlaps {b:?}");
			assert!(a.right() < b.left(), "left to right");
		}
	}
	// The status bar's right side leaves the message its room (keys only).
	assert!(h.query_by_label("Mixer panel").is_some());
	assert!(h.query_by_label_contains("undo steps").is_none());
	h.get_by_label("Modes").click();
	h.run_steps(2);
	h.get_by_label("Metronome").click();
	h.run_steps(2);
	let s = h.state().state.record.settings;
	assert!(s.metronome.on_play && s.metronome.on_record);
	// The browser is an icon rail: an icon opens the overlay, Esc closes it.
	assert!(h.state().state.view.responsive.browser_rail);
	h.get_by_label("Browser Files").click();
	h.run_steps(2);
	assert!(h.state().state.view.panels.browser_overlay);
	h.key_press(Key::Escape);
	h.key_press(Key::Escape);
	h.run_steps(2);
	assert!(!h.state().state.view.panels.browser_overlay);
}

#[test]
fn view_language_switches_the_ui_to_spanish_live_and_back() {
	use cadenza_app::i18n::{Language, LanguageChoice};
	let mut h = harness(app());
	h.run_steps(2);
	assert!(h.query_by_label("File").is_some());
	h.get_by_label("View").click();
	h.run_steps(2);
	h.get_by_label_contains("Language").hover();
	h.run_steps(2);
	h.get_by_label_contains("Español").click();
	h.run_steps(3);
	assert_eq!(
		h.state().state.language,
		LanguageChoice::Fixed(Language::Spanish)
	);
	assert_eq!(
		h.state().state.prefs().language,
		LanguageChoice::Fixed(Language::Spanish),
		"persisted with the other preferences"
	);
	assert!(
		h.query_by_label("Archivo").is_some(),
		"the menu bar follows at once"
	);
	assert!(h.query_by_label("File").is_none());
	assert!(h.query_by_label("Ver").is_some());
	h.state_mut()
		.state
		.set_language(LanguageChoice::Fixed(Language::English));
	h.run_steps(2);
	assert!(h.query_by_label("File").is_some());
	h.state_mut().state.set_language(LanguageChoice::System);
}

/// The main window, the export dialog and a device window in Spanish at 1280 and
/// 1024 px (layout overflow check for the longer strings).
#[test]
#[ignore]
fn render_language_screens() {
	use cadenza_app::i18n::{Language, LanguageChoice};
	use cadenza_app::state::windows::WindowItem;
	let Some(dir) = std::env::var_os("CADENZA_SCREENSHOT_DIR").map(PathBuf::from) else {
		return;
	};
	std::fs::create_dir_all(&dir).unwrap();
	let tmp = tempfile::tempdir().unwrap();
	for (w, h_px) in [(1280.0, 800.0), (1024.0, 700.0)] {
		let mut a = app();
		a.state
			.set_language(LanguageChoice::Fixed(Language::Spanish));
		a.state.view.panels.mixer = true;
		let mut h = app_harness(a, vec2(w, h_px), true);
		h.run_steps(3);
		shot(&mut h, &dir, &format!("es_main_{w}"));
		h.get_by_label("Ver").click();
		h.run_steps(2);
		h.get_by_label_contains("Idioma").hover();
		h.run_steps(2);
		shot(&mut h, &dir, &format!("es_view_menu_{w}"));
		h.get_by_label("Ver").click();
		h.run_steps(2);
		h.hover_at(pos2(w * 0.5, h_px * 0.5));
		h.key_press(Key::Escape);
		h.run_steps(2);
		h.key_press_modifiers(Modifiers::COMMAND, Key::E);
		h.run_steps(2);
		h.state_mut().state.export.folder = Some(tmp.path().join("Renders"));
		shot(&mut h, &dir, &format!("es_export_{w}"));
		// The same dialog in English, for comparison.
		h.state_mut()
			.state
			.set_language(LanguageChoice::Fixed(Language::English));
		shot(&mut h, &dir, &format!("en_export_{w}"));
		h.state_mut()
			.state
			.set_language(LanguageChoice::Fixed(Language::Spanish));
		h.state_mut().state.export.open = false;
		h.run_steps(2);
		let r = eq_insert(&h.state().state);
		assert!(h.state_mut().state.open_window(WindowItem::Device(r)));
		shot(&mut h, &dir, &format!("es_device_window_{w}"));
		let r = reverb_insert(&h.state().state);
		assert!(h.state_mut().state.open_device_in_dock(r));
		shot(&mut h, &dir, &format!("es_device_dock_{w}"));
		h.key_press(Key::F1);
		h.run_steps(2);
		shot(&mut h, &dir, &format!("es_shortcuts_{w}"));
		h.state_mut().state.set_language(LanguageChoice::System);
	}
}

/// T41a: the main window, the View menu and the export dialog in Portuguese (Brazil) and
/// French at 1024 px (the narrowest layout, so overflow shows first).
#[test]
#[ignore]
fn render_language_screens_pt_fr() {
	use cadenza_app::i18n::{Language, LanguageChoice};
	let Some(dir) = std::env::var_os("CADENZA_SCREENSHOT_DIR").map(PathBuf::from) else {
		return;
	};
	std::fs::create_dir_all(&dir).unwrap();
	let tmp = tempfile::tempdir().unwrap();
	let (w, h_px) = (1024.0, 700.0);
	for (lang, code, view_menu, language_item) in [
		(Language::PortugueseBr, "pt", "Exibir", "Idioma"),
		(Language::French, "fr", "Affichage", "Langue"),
	] {
		let mut a = app();
		a.state.set_language(LanguageChoice::Fixed(lang));
		a.state.view.panels.mixer = true;
		let mut h = app_harness(a, vec2(w, h_px), true);
		h.run_steps(3);
		shot(&mut h, &dir, &format!("{code}_main_{w}"));
		h.get_by_label(view_menu).click();
		h.run_steps(2);
		h.get_by_label_contains(language_item).hover();
		h.run_steps(2);
		shot(&mut h, &dir, &format!("{code}_view_menu_{w}"));
		h.get_by_label(view_menu).click();
		h.run_steps(2);
		h.hover_at(pos2(w * 0.5, h_px * 0.5));
		h.key_press(Key::Escape);
		h.run_steps(2);
		h.key_press_modifiers(Modifiers::COMMAND, Key::E);
		h.run_steps(2);
		h.state_mut().state.export.folder = Some(tmp.path().join("Renders"));
		shot(&mut h, &dir, &format!("{code}_export_{w}"));
		h.state_mut().state.set_language(LanguageChoice::System);
	}
}

/// T41b: the main window, the View menu and the export dialog in German, Italian and
/// Russian at 1024 px (the narrowest layout, so overflow shows first; German is the
/// longest of the shipped languages).
#[test]
#[ignore]
fn render_language_screens_de_it_ru() {
	use cadenza_app::i18n::{Language, LanguageChoice};
	let Some(dir) = std::env::var_os("CADENZA_SCREENSHOT_DIR").map(PathBuf::from) else {
		return;
	};
	std::fs::create_dir_all(&dir).unwrap();
	let tmp = tempfile::tempdir().unwrap();
	let (w, h_px) = (1024.0, 700.0);
	for (lang, code, view_menu, language_item) in [
		(Language::German, "de", "Ansicht", "Sprache"),
		(Language::Italian, "it", "Visualizza", "Lingua"),
		(Language::Russian, "ru", "Вид", "Язык"),
	] {
		let mut a = app();
		a.state.set_language(LanguageChoice::Fixed(lang));
		a.state.view.panels.mixer = true;
		let mut h = app_harness(a, vec2(w, h_px), true);
		h.run_steps(3);
		shot(&mut h, &dir, &format!("{code}_main_{w}"));
		h.get_by_label(view_menu).click();
		h.run_steps(2);
		h.get_by_label_contains(language_item).hover();
		h.run_steps(2);
		shot(&mut h, &dir, &format!("{code}_view_menu_{w}"));
		h.get_by_label(view_menu).click();
		h.run_steps(2);
		h.hover_at(pos2(w * 0.5, h_px * 0.5));
		h.key_press(Key::Escape);
		h.run_steps(2);
		h.key_press_modifiers(Modifiers::COMMAND, Key::E);
		h.run_steps(2);
		h.state_mut().state.export.folder = Some(tmp.path().join("Renders"));
		shot(&mut h, &dir, &format!("{code}_export_{w}"));
		h.state_mut().state.set_language(LanguageChoice::System);
	}
}

/// T41c: the main window, the View menu and the export dialog in Japanese, Simplified
/// Chinese and Korean (the new CJK languages) at 1024 px, and German again (the toolbar
/// fold fix: at this width its arrange toolbar used to overflow, "Tempo" drawn under
/// "Einpassen" and "Sel" cut off — see docs/design/redesign-progress.md T41).
#[test]
#[ignore]
fn render_language_screens_ja_zh_ko() {
	use cadenza_app::i18n::{Language, LanguageChoice};
	let Some(dir) = std::env::var_os("CADENZA_SCREENSHOT_DIR").map(PathBuf::from) else {
		return;
	};
	std::fs::create_dir_all(&dir).unwrap();
	let tmp = tempfile::tempdir().unwrap();
	let (w, h_px) = (1024.0, 700.0);
	for (lang, code, view_menu, language_item) in [
		(Language::Japanese, "ja", "表示", "言語"),
		(Language::ChineseSimplified, "zh-CN", "视图", "语言"),
		(Language::Korean, "ko", "보기", "언어"),
		(Language::German, "de", "Ansicht", "Sprache"),
	] {
		let mut a = app();
		a.state.set_language(LanguageChoice::Fixed(lang));
		a.state.view.panels.mixer = true;
		let mut h = app_harness(a, vec2(w, h_px), true);
		h.run_steps(3);
		shot(&mut h, &dir, &format!("{code}_main_{w}"));
		h.get_by_label(view_menu).click();
		h.run_steps(2);
		h.get_by_label_contains(language_item).hover();
		h.run_steps(2);
		shot(&mut h, &dir, &format!("{code}_view_menu_{w}"));
		h.get_by_label(view_menu).click();
		h.run_steps(2);
		h.hover_at(pos2(w * 0.5, h_px * 0.5));
		h.key_press(Key::Escape);
		h.run_steps(2);
		h.key_press_modifiers(Modifiers::COMMAND, Key::E);
		h.run_steps(2);
		h.state_mut().state.export.folder = Some(tmp.path().join("Renders"));
		shot(&mut h, &dir, &format!("{code}_export_{w}"));
		h.state_mut().state.set_language(LanguageChoice::System);
	}
}

/// `CADENZA_SCREENSHOT_DIR=dir cargo test -p cadenza-app --release --test ui -- --ignored
/// render_listen_freeze_screens`: the compressor listening to its key (with the key
/// trace), an EQ band solo (dock and compact) and the reverb frozen.
#[test]
#[ignore = "renders PNGs (wgpu); set CADENZA_SCREENSHOT_DIR"]
fn render_listen_freeze_screens() {
	use cadenza_app::ui::devices::compressor;
	use cadenza_engine::Listen;
	let Some(dir) = std::env::var_os("CADENZA_SCREENSHOT_DIR").map(PathBuf::from) else {
		return;
	};
	std::fs::create_dir_all(&dir).unwrap();
	let shot = |h: &mut Harness<'_, CadenzaApp>, name: &str| {
		h.run_steps(4);
		h.render()
			.unwrap()
			.save(dir.join(format!("{name}.png")))
			.unwrap();
	};

	// Compressor: keyed from the drums, "Listen to key" on.
	let (mut a, _) = comp_dock_app();
	let r = bass_compressor(&a.state);
	let drums = a
		.state
		.project()
		.tracks
		.iter()
		.find(|t| t.name == "Drums")
		.unwrap()
		.id;
	a.state.apply(cadenza_model::Command::SetSidechain {
		track: r.track,
		device: r.device,
		source: Some(cadenza_model::SidechainSource {
			track: drums,
			pre_fader: false,
		}),
	});
	let (history, gr) = pump_history(-18.0, 4.0, 6.0, 0.0);
	let mut h = app_harness(a, vec2(1920.0, 1000.0), true);
	h.run_steps(2);
	h.get_by_label("Listen to key").click();
	h.run_steps(1);
	assert_eq!(h.state().state.listen_mode(r.device), Listen::Key);
	compressor::set_display(&h.ctx, "dock", r, history, -12.0, gr);
	shot(&mut h, "compressor_listen_key");

	// EQ: Peak 2 soloed from its Listen toggle, then the compact form.
	let mut a = app();
	let r = eq_insert(&a.state);
	shape_eq(&mut a.state, r);
	a.state.view.panels.set(PanelKind::Inspector, false);
	assert!(a.state.open_device_in_dock(r));
	let (pre, post) = eq_analyzer(&a.state, r);
	let mut h = app_harness(a, vec2(1920.0, 1000.0), true);
	h.run_steps(3);
	h.get_by_label("Band 2").click();
	h.run_steps(1);
	h.get_by_label("Listen").click();
	h.run_steps(1);
	assert_eq!(h.state().state.listen_mode(r.device), Listen::Band(3));
	cadenza_app::ui::devices::eq::set_analyzer(&h.ctx, "dock", r, &pre, &post);
	shot(&mut h, "eq_band_listen");

	let mut a = app();
	let r = eq_insert(&a.state);
	shape_eq(&mut a.state, r);
	a.state.selection.select_device(r);
	a.state.view.panels.set(PanelKind::Inspector, true);
	a.state.view.panels.set(PanelKind::Mixer, false);
	let mut h = app_harness(a, vec2(1600.0, 1000.0), true);
	h.run_steps(3);
	h.state_mut().state.set_listen(r.device, Listen::Band(3));
	cadenza_app::ui::devices::eq::set_analyzer(&h.ctx, "inspector", r, &pre, &post);
	shot(&mut h, "eq_band_listen_compact");

	// Reverb: frozen.
	let mut a = app();
	let r = reverb_insert(&a.state);
	a.state.commit_params(
		r.track,
		r.device,
		&[("size", 80.0), ("decay", 3.5), ("damping", 55.0)],
		"Reverb shape",
	);
	a.state.view.panels.set(PanelKind::Inspector, false);
	a.state.view.panels.set(PanelKind::Browser, false);
	assert!(a.state.open_device_in_dock(r));
	let mut h = app_harness(a, vec2(1920.0, 1000.0), true);
	h.run_steps(2);
	h.get_by_label("Freeze").click();
	h.run_steps(1);
	assert_eq!(
		h.state()
			.state
			.project()
			.device(r.track, r.device)
			.unwrap()
			.params
			.get("freeze"),
		Some(&1.0),
		"freeze is a stored param"
	);
	cadenza_app::ui::devices::reverb::set_display(&h.ctx, "dock", r, [-9.0, -11.0]);
	shot(&mut h, "reverb_freeze");
}

/// T28: what the probed instrument plays, from its voice readings (a virtual device
/// renders the real processors): the Sampler with playheads on the selected zone, lit
/// zones and keys and the envelope dots; the Drum Sampler with a pad playhead and lit
/// pads; the Poly Synth with ADSR voice dots and a lit keyboard; the Electric Piano with
/// its voices on the note picture.
/// `CADENZA_SCREENSHOT_DIR=/tmp/shots cargo test -p cadenza-app --release --test ui --
/// --ignored render_voice_reading_screens`.
#[test]
#[ignore]
fn render_voice_reading_screens() {
	let Some(dir) = std::env::var_os("CADENZA_SCREENSHOT_DIR").map(PathBuf::from) else {
		return;
	};
	std::fs::create_dir_all(&dir).unwrap();
	let shot = |h: &mut Harness<'_, CadenzaApp>, name: &str| {
		h.run_steps(4);
		h.render()
			.unwrap()
			.save(dir.join(format!("{name}.png")))
			.unwrap();
	};
	// The editor's device probed in a virtual player on the app's project; `play` hands
	// it notes (on, then off after `held` blocks of 512 frames) and renders `tail` more.
	struct Live {
		player: cadenza_engine::Player,
		dev: cadenza_engine::VirtualDevice,
	}
	impl Live {
		fn new(h: &mut Harness<'_, CadenzaApp>, r: DeviceRef) -> Self {
			h.run_steps(2);
			let project = h.state().state.project().clone();
			let (mut player, dev) = cadenza_engine::Player::virtual_device(48_000, 0);
			player.load(&project).unwrap();
			player.set_probe(Some(r.device)).unwrap();
			let s = &mut h.state_mut().state;
			assert_eq!(s.probe_target(), Some(r.device), "the editor probes it");
			s.attach_probe(player.probe());
			Self { player, dev }
		}
		fn run(&mut self, blocks: usize) {
			let mut out = vec![0.0f32; 2 * 512];
			for _ in 0..blocks {
				self.dev.process(&mut out, &[]);
			}
		}
		fn on(&mut self, track: cadenza_core::TrackId, notes: &[u8], velocity: f32) {
			for &n in notes {
				self.player.preview_note_on(track, n, velocity).unwrap();
			}
		}
		fn off(&mut self, track: cadenza_core::TrackId, notes: &[u8]) {
			for &n in notes {
				self.player.preview_note_off(track, n).unwrap();
			}
		}
	}

	// Sampler: the felt piano, C3-B3 hard selected; two hard notes held 0.4 s in it plus
	// one released.
	let (mut a, _, smp) = sampler_app();
	felt_upright(&mut a, smp);
	let track = smp.track.unwrap();
	let mut h = app_harness(a, vec2(1920.0, 1000.0), true);
	for (.., path) in felt_upright_files() {
		wait_for_peaks(&mut h, &path);
	}
	let mut live = Live::new(&mut h, smp);
	live.on(track, &[50, 55], 0.95);
	live.run(30);
	live.on(track, &[67], 0.95);
	live.run(10);
	live.off(track, &[67]);
	live.run(4);
	let voices = h.state().state.live_voices(smp.device);
	assert_eq!(voices.held_keys().collect::<Vec<_>>(), vec![50, 55]);
	assert!(voices.voices.len() >= 3, "{voices:?}");
	shot(&mut h, "voices_sampler_playheads");
	live.off(track, &[50, 55]);

	// Drum sampler: snare selected, kick + snare + hat just hit.
	let (a, drums, _) = sampler_app();
	let track = drums.track.unwrap();
	let mut h = app_harness(a, vec2(1600.0, 1000.0), true);
	wait_for_peaks_app(&mut h);
	for f in ["Snare.wav", "Closed Hat.wav"] {
		wait_for_peaks(&mut h, &factory_samples().join(f));
	}
	h.state_mut().state.view.instrument.pad = 38;
	let mut live = Live::new(&mut h, drums);
	live.on(track, &[36, 38, 42], 1.0);
	live.run(8);
	let voices = h.state().state.live_voices(drums.device);
	assert!(voices.voices.iter().any(|v| v.note == 38), "{voices:?}");
	shot(&mut h, "voices_drum_sampler_pad_playhead");

	// Poly synth (the song's Lead): a held C major triad, the top note released.
	let mut a = app();
	let lead = {
		let t = a
			.state
			.project()
			.tracks
			.iter()
			.find(|t| t.name == "Lead")
			.unwrap();
		DeviceRef {
			track: Some(t.id),
			device: t.instrument.as_ref().unwrap().id,
		}
	};
	a.state
		.commit_param(lead.track, lead.device, "amp_attack", 400.0);
	a.state
		.commit_param(lead.track, lead.device, "amp_release", 1500.0);
	assert!(a.state.open_device_in_dock(lead));
	let track = lead.track.unwrap();
	let mut h = app_harness(a, vec2(1600.0, 1000.0), true);
	let mut live = Live::new(&mut h, lead);
	live.on(track, &[60], 0.8);
	live.run(12);
	live.on(track, &[64, 67], 0.8);
	live.run(30);
	live.off(track, &[67]);
	live.run(20);
	let voices = h.state().state.live_voices(lead.device);
	assert_eq!(voices.held_keys().collect::<Vec<_>>(), vec![60, 64]);
	shot(&mut h, "voices_poly_synth_adsr_keys");

	// Electric piano: a chord struck 0.6 s ago, one note released.
	let mut a = app();
	let keys = {
		let t = a
			.state
			.project()
			.tracks
			.iter()
			.find(|t| {
				t.instrument
					.as_ref()
					.is_some_and(|d| d.builtin_id() == Some("electric_piano"))
			})
			.expect("the example song has an electric piano");
		DeviceRef {
			track: Some(t.id),
			device: t.instrument.as_ref().unwrap().id,
		}
	};
	assert!(a.state.open_device_in_dock(keys));
	let track = keys.track.unwrap();
	let mut h = app_harness(a, vec2(1600.0, 1000.0), true);
	let mut live = Live::new(&mut h, keys);
	live.on(track, &[57, 60, 64], 0.8);
	live.run(56);
	live.off(track, &[64]);
	live.run(12);
	shot(&mut h, "voices_electric_piano");
}

#[test]
fn ask_ai_about_a_control_opens_the_prefilled_popover() {
	let (a, r) = effect_dock_app("Bass", "compressor", "Compressor");
	let mut h = app_harness(a, vec2(1920.0, 1000.0), false);
	h.run_steps(3);
	// The knob (the level history's threshold line has the same name).
	h.get_all_by_label("Threshold")
		.find(|n| n.rect().width() < 100.0)
		.unwrap()
		.click_secondary();
	h.run_steps(2);
	assert!(
		h.query_by_label("Reset").is_some(),
		"the control menu opens"
	);
	h.get_by_label("Ask AI about this").click();
	h.run_steps(3);
	assert!(
		h.query_by_label("Ask AI about Compressor").is_some(),
		"the device's Ask AI popover opens"
	);
	let threshold = builtin_param(&h.state().state, r, "compressor", "threshold");
	let typed = h
		.query_all_by(|n| {
			n.value()
				.is_some_and(|v| v.starts_with("Threshold (now ") && v.ends_with("): "))
		})
		.next()
		.map(|n| n.value().unwrap());
	let typed = typed.expect("prefilled with the param and its value");
	assert!(
		typed.contains(&format!("{:.1}", threshold.abs())),
		"{typed}"
	);
}

#[test]
fn editor_keys_bypass_nudge_select_and_compare_without_the_synth_keyboard() {
	// Compressor: B bypasses, ↑ / ↓ nudge the main param (threshold) in 0.5 dB.
	let (a, r) = effect_dock_app("Bass", "compressor", "Compressor");
	let mut h = app_harness(a, vec2(1920.0, 1000.0), false);
	h.run_steps(3);
	h.state_mut().state.view.panels.focus = Some(PanelKind::PianoRoll);
	let before = builtin_param(&h.state().state, r, "compressor", "threshold");
	h.key_press(Key::ArrowUp);
	h.run_steps(1);
	let after = builtin_param(&h.state().state, r, "compressor", "threshold");
	assert!((after - before - 0.5).abs() < 1e-3, "{before} -> {after}");
	h.key_press_modifiers(Modifiers::SHIFT, Key::ArrowDown);
	h.run_steps(1);
	let fine = builtin_param(&h.state().state, r, "compressor", "threshold");
	assert!((after - fine - 0.1).abs() < 1e-3, "{after} -> {fine}");
	h.key_press(Key::B);
	h.run_steps(1);
	assert!(h.state().state.device(r).unwrap().bypass, "B bypasses");
	h.key_press(Key::B);
	h.run_steps(1);
	assert!(!h.state().state.device(r).unwrap().bypass);

	// EQ: → selects the next band, ↑ lifts its gain.
	let (a, r) = effect_dock_app("Bass", "eq", "Parametric EQ");
	let mut h = app_harness(a, vec2(1920.0, 1000.0), false);
	h.run_steps(3);
	h.state_mut().state.view.panels.focus = Some(PanelKind::PianoRoll);
	h.key_press(Key::Num3);
	h.run_steps(1);
	h.key_press(Key::ArrowRight);
	h.run_steps(1);
	let band = cadenza_app::state::eq_editor::BANDS[3].gain.unwrap();
	let before = eq_param(&h.state().state, r, band);
	h.key_press(Key::ArrowUp);
	h.run_steps(1);
	assert!((eq_param(&h.state().state, r, band) - before - 0.5).abs() < 1e-3);

	// Poly synth: Shift+A switches A/B and plays nothing; A still plays a note.
	let (a, r) = synth_dock_app();
	let mut h = app_harness(a, vec2(1920.0, 1000.0), false);
	h.run_steps(3);
	h.state_mut().state.view.panels.focus = Some(PanelKind::PianoRoll);
	h.key_press_modifiers(Modifiers::SHIFT, Key::A);
	h.run_steps(2);
	assert_eq!(
		h.state().state.compare_slot(r),
		cadenza_app::state::device_ab::Slot::B
	);
	assert!(
		h.state().state.previewing.is_empty(),
		"no note from Shift+A"
	);
	h.key_down(Key::A);
	h.run_steps(2);
	assert_eq!(h.state().state.previewing, vec![(r.track.unwrap(), 48)]);
	h.key_up(Key::A);
	h.run_steps(2);
}

#[test]
fn the_header_meters_move_with_audio_through_the_probed_device() {
	// A compressor on the synth's track, open in the dock, probed in a virtual player
	// that plays a held note through it.
	let (mut a, synth) = synth_dock_app();
	let track = synth.track.unwrap();
	let name = a.state.project().track(track).unwrap().name.clone();
	let r = insert_builtin(&mut a.state, &name, "compressor", "Compressor");
	assert!(a.state.open_device_in_dock(r));
	let mut h = app_harness(a, vec2(1600.0, 1000.0), false);
	h.run_steps(2);
	let quiet = h.state().device_io_levels();
	let floor = cadenza_app::state::meters::FLOOR_DB;
	assert!(
		quiet
			.input
			.iter()
			.chain(&quiet.output)
			.all(|db| *db <= floor)
	);
	let project = h.state().state.project().clone();
	let (mut player, mut dev) = cadenza_engine::Player::virtual_device(48_000, 0);
	player.load(&project).unwrap();
	player.set_probe(Some(r.device)).unwrap();
	assert_eq!(h.state().state.probe_target(), Some(r.device));
	h.state_mut().state.attach_probe(player.probe());
	player.preview_note_on(track, 60, 0.9).unwrap();
	let mut out = vec![0.0f32; 2 * 512];
	for _ in 0..8 {
		dev.process(&mut out, &[]);
	}
	h.run_steps(1);
	let live = h.state().device_io_levels();
	assert!(
		live.input.iter().all(|db| *db > floor) && live.output.iter().all(|db| *db > floor),
		"IN and OUT move: {live:?}"
	);
	player.preview_note_off(track, 60).unwrap();
}

/// T29 kit polish: the export dialog (kit segmented choices, visible checkboxes, the
/// two-row metadata) in English and Japanese at 1024 px, the Japanese mixer toolbar
/// folded at 1024 px with its Rows menu, a knob's hover readout, the control menu's
/// "Ask AI about this" and the prefilled popover, the compressor's compact form with
/// "Listen to key" and an EQ band solo dimming the other bands.
/// `CADENZA_SCREENSHOT_DIR=dir cargo test -p cadenza-app --release --test ui -- --ignored
/// render_kit_polish_screens`.
#[test]
#[ignore = "renders PNGs (wgpu); set CADENZA_SCREENSHOT_DIR"]
fn render_kit_polish_screens() {
	use cadenza_app::i18n::{Language, LanguageChoice};
	use cadenza_engine::Listen;
	let Some(dir) = std::env::var_os("CADENZA_SCREENSHOT_DIR").map(PathBuf::from) else {
		return;
	};
	std::fs::create_dir_all(&dir).unwrap();
	let tmp = tempfile::tempdir().unwrap();
	let shot = |h: &mut Harness<'_, CadenzaApp>, name: &str| {
		h.run_steps(3);
		h.render()
			.unwrap()
			.save(dir.join(format!("{name}.png")))
			.unwrap();
	};

	for (lang, code) in [(Language::English, "en"), (Language::Japanese, "ja")] {
		let mut a = app();
		a.state.set_language(LanguageChoice::Fixed(lang));
		a.state.view.panels.mixer = true;
		let mut h = app_harness(a, vec2(1024.0, 700.0), true);
		h.run_steps(3);
		shot(&mut h, &format!("{code}_mixer_toolbar_1024"));
		if let Some(rows) = h.query_by_label(cadenza_app::i18n::tr("Rows ▾")) {
			rows.click();
			h.run_steps(2);
			shot(&mut h, &format!("{code}_mixer_rows_menu_1024"));
			h.key_press(Key::Escape);
			h.hover_at(pos2(512.0, 200.0));
			h.run_steps(2);
		}
		h.key_press_modifiers(Modifiers::COMMAND, Key::E);
		h.run_steps(2);
		{
			let e = &mut h.state_mut().state.export;
			e.folder = Some(tmp.path().join("Renders"));
			// Unchecked boxes next to checked ones.
			e.settings.tail = false;
			e.settings.dither = false;
		}
		h.set_size(vec2(1024.0, 820.0));
		shot(&mut h, &format!("{code}_export_1024"));
		h.state_mut().state.export.open = false;
		// Narrow: the toolbar folds its rows (and then its choices) into the menu.
		for w in [820.0, 620.0] {
			h.set_size(vec2(w, 700.0));
			h.run_steps(3);
			shot(&mut h, &format!("{code}_mixer_toolbar_{w}"));
			if let Some(rows) = h.query_by_label(cadenza_app::i18n::tr("Rows ▾")) {
				rows.click();
				h.run_steps(2);
				shot(&mut h, &format!("{code}_mixer_rows_menu_{w}"));
				h.key_press(Key::Escape);
				h.run_steps(2);
			}
		}
		h.state_mut().state.set_language(LanguageChoice::System);
	}
	let mut a = app();
	a.state.view.panels.mixer = true;
	let mut h = app_harness(a, vec2(1600.0, 1000.0), true);
	h.run_steps(3);
	shot(&mut h, "en_mixer_toolbar_1600");

	// Compressor dock: the hover readout of Threshold, its menu, the prefilled popover.
	let (a, _) = effect_dock_app("Bass", "compressor", "Compressor");
	let mut h = app_harness(a, vec2(1600.0, 1000.0), true);
	h.run_steps(3);
	let knob = h
		.get_all_by_label("Threshold")
		.find(|n| n.rect().width() < 100.0)
		.unwrap()
		.rect()
		.center();
	h.hover_at(pos2(knob.x as f32, knob.y as f32));
	for _ in 0..30 {
		h.step();
	}
	shot(&mut h, "kit_hover_readout");
	h.get_all_by_label("Threshold")
		.find(|n| n.rect().width() < 100.0)
		.unwrap()
		.click_secondary();
	h.run_steps(2);
	shot(&mut h, "kit_ask_ai_menu");
	h.get_by_label("Ask AI about this").click();
	h.run_steps(3);
	shot(&mut h, "kit_ask_ai_prefilled");

	// The compressor's compact form with Listen to key.
	let mut a = app();
	let r = bass_compressor(&a.state);
	a.state.selection.select_device(r);
	a.state.view.panels.set(PanelKind::Inspector, true);
	a.state.view.panels.set(PanelKind::Mixer, false);
	let mut h = app_harness(a, vec2(1600.0, 1000.0), true);
	h.run_steps(3);
	h.state_mut().state.set_listen(r.device, Listen::Key);
	shot(&mut h, "compressor_compact_listen_key");

	// EQ: Peak 2 soloed, the other bands dimmed.
	let mut a = app();
	let r = eq_insert(&a.state);
	shape_eq(&mut a.state, r);
	a.state.view.panels.set(PanelKind::Inspector, false);
	assert!(a.state.open_device_in_dock(r));
	let mut h = app_harness(a, vec2(1920.0, 1000.0), true);
	h.run_steps(3);
	h.state_mut().state.set_listen(r.device, Listen::Band(3));
	shot(&mut h, "eq_solo_dims_others");
}

/// T30: the compressor's key filter strip (dock and compact) and the EQ with a dynamic
/// band reducing (dock and compact).
/// `CADENZA_SCREENSHOT_DIR=/tmp/shots cargo test -p cadenza-app --release --test ui --
/// --ignored render_dynamics_screens`.
#[test]
#[ignore]
fn render_dynamics_screens() {
	use cadenza_app::ui::devices::{compressor, eq};
	let Some(dir) = std::env::var_os("CADENZA_SCREENSHOT_DIR").map(PathBuf::from) else {
		return;
	};
	std::fs::create_dir_all(&dir).unwrap();
	let shot = |h: &mut Harness<'_, CadenzaApp>, name: &str| {
		h.run_steps(4);
		h.render()
			.unwrap()
			.save(dir.join(format!("{name}.png")))
			.unwrap();
	};
	let key_filter = [("key_hp", 150.0), ("key_lp", 6_000.0)];

	let (mut a, r) = comp_dock_app();
	a.state
		.commit_params(r.track, r.device, &key_filter, "Key filter");
	let (history, gr) = pump_history(-18.0, 4.0, 6.0, 0.0);
	let mut h = app_harness(a, vec2(1920.0, 1000.0), true);
	h.run_steps(2);
	compressor::set_display(&h.ctx, "dock", r, history.clone(), -12.0, gr);
	shot(&mut h, "compressor_key_filter");

	let mut a = app();
	let r = bass_compressor(&a.state);
	a.state
		.commit_params(r.track, r.device, &key_filter, "Key filter");
	a.state.selection.select_device(r);
	a.state.view.panels.set(PanelKind::Inspector, true);
	a.state.view.panels.set(PanelKind::Mixer, false);
	let mut h = app_harness(a, vec2(1600.0, 1000.0), true);
	h.run_steps(3);
	shot(&mut h, "compressor_key_filter_compact");

	// EQ: Peak 3 dynamic (-8 dB depth), playing -5 dB right now; the low shelf dynamic
	// at rest.
	let dynamic = [
		("peak3_gain", -8.0),
		("peak3_dyn", 1.0),
		("peak3_threshold", -30.0),
		("peak3_ratio", 4.0),
		("low_shelf_gain", 3.0),
		("low_shelf_dyn", 1.0),
	];
	let live = [0.0, 0.3, -3.5, 2.5, -5.0, 3.0, 0.0];
	let mut a = app();
	let r = eq_insert(&a.state);
	shape_eq(&mut a.state, r);
	a.state
		.commit_params(r.track, r.device, &dynamic, "Dynamic");
	a.state.view.panels.set(PanelKind::Inspector, false);
	assert!(a.state.open_device_in_dock(r));
	let mut h = app_harness(a, vec2(1920.0, 1000.0), true);
	h.run_steps(3);
	h.get_by_label("Band 3").click();
	h.run_steps(1);
	eq::set_live_gains(&h.ctx, "dock", r, &live);
	shot(&mut h, "eq_dynamic_band");

	let mut a = app();
	let r = eq_insert(&a.state);
	shape_eq(&mut a.state, r);
	a.state
		.commit_params(r.track, r.device, &dynamic, "Dynamic");
	a.state.selection.select_device(r);
	a.state.view.panels.set(PanelKind::Inspector, true);
	a.state.view.panels.set(PanelKind::Mixer, false);
	let mut h = app_harness(a, vec2(1600.0, 1000.0), true);
	h.run_steps(3);
	h.get_by_label("Band 3").click();
	h.run_steps(1);
	eq::set_live_gains(&h.ctx, "inspector", r, &live);
	shot(&mut h, "eq_dynamic_band_compact");
}

/// T31: an EQ with a side band (mid and side curves, M / S badges; dock and compact) and
/// a match EQ preview (reference spectrum, dashed proposal, the match card).
/// `CADENZA_SCREENSHOT_DIR=/tmp/shots cargo test -p cadenza-app --release --test ui --
/// --ignored render_mid_side_match_screens`.
#[test]
#[ignore]
fn render_mid_side_match_screens() {
	use cadenza_analysis::spectrum::THIRD_OCTAVE_HZ;
	use cadenza_app::state::match_eq::MatchEq;
	use cadenza_mcp::tools::match_eq::MatchSpectra;
	let Some(dir) = std::env::var_os("CADENZA_SCREENSHOT_DIR").map(PathBuf::from) else {
		return;
	};
	std::fs::create_dir_all(&dir).unwrap();
	let shot = |h: &mut Harness<'_, CadenzaApp>, name: &str| {
		h.run_steps(4);
		h.render()
			.unwrap()
			.save(dir.join(format!("{name}.png")))
			.unwrap();
	};
	// Side: high shelf +4 dB (width) and a high-pass at 180 Hz (mono low end); mid: a
	// 300 Hz cut.
	let ms = [
		("high_shelf_gain", 4.0),
		("high_shelf_freq", 6_000.0),
		("high_shelf_ms", 2.0),
		("hp_freq", 180.0),
		("hp_slope", 2.0),
		("hp_ms", 2.0),
		("peak1_freq", 300.0),
		("peak1_gain", -4.0),
		("peak1_ms", 1.0),
	];
	let mut a = app();
	let r = eq_insert(&a.state);
	shape_eq(&mut a.state, r);
	a.state.commit_params(r.track, r.device, &ms, "Mid/side");
	a.state.view.panels.set(PanelKind::Inspector, false);
	assert!(a.state.open_device_in_dock(r));
	let mut h = app_harness(a, vec2(1920.0, 1000.0), true);
	h.run_steps(3);
	h.get_by_label("High shelf").click();
	h.run_steps(1);
	shot(&mut h, "eq_side_band");

	let mut a = app();
	let r = eq_insert(&a.state);
	shape_eq(&mut a.state, r);
	a.state.commit_params(r.track, r.device, &ms, "Mid/side");
	a.state.selection.select_device(r);
	a.state.view.panels.set(PanelKind::Inspector, true);
	a.state.view.panels.set(PanelKind::Mixer, false);
	let mut h = app_harness(a, vec2(1600.0, 1000.0), true);
	h.run_steps(3);
	h.get_by_label("High shelf").click();
	h.run_steps(1);
	shot(&mut h, "eq_side_band_compact");

	// Match: a darker input against a brighter reference with less low mids.
	let input: Vec<f32> = THIRD_OCTAVE_HZ
		.iter()
		.map(|&f| {
			-24.0 - 4.0 * (f / 400.0).log2().abs().min(3.0) - (f / 4_000.0).log2().max(0.0) * 4.0
		})
		.collect();
	let reference: Vec<f32> = THIRD_OCTAVE_HZ
		.iter()
		.zip(&input)
		.map(|(&f, i)| {
			i + 3.0 * (f / 2_500.0).log2().clamp(0.0, 2.0)
				- if (200.0..=500.0).contains(&f) {
					3.0
				} else {
					0.0
				}
		})
		.collect();
	let spectra = MatchSpectra {
		input,
		reference,
		reference_name: "Reference mix.wav".into(),
	};
	let mut a = app();
	let r = eq_insert(&a.state);
	a.state.view.panels.set(PanelKind::Inspector, false);
	assert!(a.state.open_device_in_dock(r));
	a.state.set_match_eq(Some(MatchEq::ready(r, spectra)));
	let mut h = app_harness(a, vec2(1920.0, 1000.0), true);
	h.run_steps(3);
	shot(&mut h, "eq_match_preview");
}

#[test]
fn the_eq_dynamic_toggle_is_one_undo_step() {
	let mut a = app();
	let r = eq_insert(&a.state);
	shape_eq(&mut a.state, r);
	a.state.view.panels.set(PanelKind::Inspector, false);
	assert!(a.state.open_device_in_dock(r));
	let mut h = app_harness(a, vec2(1920.0, 1000.0), false);
	h.run_steps(3);
	h.get_by_label("Band 3").click();
	h.run_steps(1);
	let before = h.state().state.document().history_labels().len();
	h.get_by_label("Dynamic").click();
	h.run_steps(2);
	let state = &h.state().state;
	assert_eq!(state.document().history_labels().len(), before + 1);
	let params = &state.project().device(r.track, r.device).unwrap().params;
	assert_eq!(params.get("peak3_dyn"), Some(&1.0));
	// Thr and Ratio show while it is on.
	assert!(h.query_by_label("Thr").is_some());
}

/// Runs the Mix assistant's analysis to the end.
fn settle_state_mix(s: &mut AppState) {
	let start = std::time::Instant::now();
	while s.mix.busy() {
		s.tick(std::time::Instant::now());
		assert!(start.elapsed().as_secs() < 120, "analysis did not finish");
		std::thread::sleep(std::time::Duration::from_millis(5));
	}
	s.tick(std::time::Instant::now());
}

/// The example song analyzed, with an EQ on the track that collides most (the EQ is
/// inserted before the analysis so the report stays fresh).
fn collision_app() -> (CadenzaApp, DeviceRef) {
	use cadenza_app::state::collisions::collisions_of;
	let mut a = app();
	let s = &mut a.state;
	s.mix_analyze();
	settle_state_mix(s);
	let report = s.mix.report.clone().expect("a report");
	let (track, _) = s
		.project()
		.tracks
		.iter()
		.filter_map(|t| {
			let top = collisions_of(&report.channels, t.id).first()?.score;
			Some((t.id, top))
		})
		.max_by(|a, b| a.1.total_cmp(&b.1))
		.expect("the example song has collisions");
	let existing = s.project().track(track).and_then(|t| {
		t.channel
			.inserts
			.iter()
			.find(|d| d.builtin_id() == Some("eq"))
			.map(|d| d.id)
	});
	let device = match existing {
		Some(d) => d,
		None => {
			let eq = cadenza_model::Device::builtin("eq", "EQ");
			let id = eq.id;
			assert!(s.apply(cadenza_model::Command::InsertDevice {
				track: Some(track),
				device: eq,
				index: Some(0),
			}));
			s.mix_analyze();
			settle_state_mix(s);
			id
		}
	};
	(
		a,
		DeviceRef {
			track: Some(track),
			device,
		},
	)
}

#[test]
fn the_collision_view_suggests_a_cut_applied_as_one_step() {
	let (mut a, r) = collision_app();
	let s = &mut a.state;
	assert!(!s.mix_report_stale());
	s.show_collisions(r);
	assert!(!s.mix.busy(), "the fresh report is reused");
	let report = s.mix.report.clone().unwrap();
	let c = cadenza_app::state::collisions::collisions_of(&report.channels, r.track.unwrap());
	s.suggest_collision_cut(r, &c[0]);
	let steps = s.document().history_labels().len();
	assert!(s.apply_collision_cut());
	assert_eq!(s.document().history_labels().len(), steps + 1);
	assert!(s.mix_report_stale(), "the cut changed the mix");
	s.hide_collisions();
	assert!(!s.collisions_shown(r));
}

#[test]
#[ignore = "writes PNGs to CADENZA_SCREENSHOT_DIR"]
fn render_collision_screens() {
	let Some(dir) = std::env::var_os("CADENZA_SCREENSHOT_DIR").map(PathBuf::from) else {
		return;
	};
	std::fs::create_dir_all(&dir).unwrap();
	let shot = |h: &mut Harness<'_, CadenzaApp>, name: &str| {
		h.run_steps(4);
		h.render()
			.unwrap()
			.save(dir.join(format!("{name}.png")))
			.unwrap();
	};
	let (mut a, r) = collision_app();
	a.state.view.panels.set(PanelKind::Inspector, false);
	assert!(a.state.open_device_in_dock(r));
	a.state.show_collisions(r);
	let mut h = app_harness(a, vec2(1920.0, 1000.0), true);
	h.run_steps(3);
	h.get_by_label("Analyzer and display").click();
	h.run_steps(2);
	shot(&mut h, "eq_collisions");
	let report = h.state().state.mix.report.clone().unwrap();
	let c = cadenza_app::state::collisions::collisions_of(&report.channels, r.track.unwrap());
	h.state_mut().state.suggest_collision_cut(r, &c[0]);
	shot(&mut h, "eq_collision_cut");

	let (mut a, r) = collision_app();
	a.state.selection.select_device(r);
	a.state.view.panels.set(PanelKind::Inspector, true);
	a.state.view.panels.set(PanelKind::Mixer, false);
	a.state.show_collisions(r);
	let mut h = app_harness(a, vec2(1600.0, 1000.0), true);
	h.run_steps(3);
	shot(&mut h, "eq_collisions_compact");
}

/// The example song's Lead poly synth.
fn lead_synth(state: &AppState) -> DeviceRef {
	let t = state
		.project()
		.tracks
		.iter()
		.find(|t| t.name == "Lead")
		.unwrap();
	DeviceRef {
		track: Some(t.id),
		device: t.instrument.as_ref().unwrap().id,
	}
}

fn set_mods(state: &mut AppState, r: DeviceRef, mods: Vec<cadenza_model::Modulator>) {
	assert!(state.apply(cadenza_model::Command::SetModulators {
		track: r.track,
		device: r.device,
		modulators: mods,
	}));
}

fn mod_target(param: &str, depth: f32) -> cadenza_model::ModTarget {
	cadenza_model::ModTarget {
		param: param.into(),
		depth,
	}
}

#[test]
fn the_mod_section_adds_sources_in_one_step_each() {
	let mut a = app();
	let lead = lead_synth(&a.state);
	assert!(a.state.open_device_in_dock(lead));
	a.state.view.panels.set(PanelKind::Mixer, false);
	let mut h = app_harness(a, vec2(1600.0, 1000.0), false);
	h.run_steps(3);
	let before = h.state().state.document().history_labels().len();
	h.get_by_label("▸ MOD").click();
	h.run_steps(2);
	h.get_by_label("+ LFO").click();
	h.run_steps(2);
	h.get_by_label("+ Envelope").click();
	h.run_steps(2);
	let d = h.state().state.device(lead).unwrap().clone();
	assert_eq!(d.modulators.len(), 2);
	assert!(d.modulators[0].is_lfo() && !d.modulators[1].is_lfo());
	assert_eq!(
		h.state().state.document().history_labels().len(),
		before + 2
	);
}

/// T33: modulation. The poly synth's filter cutoff on a triangle LFO (ring + live dot
/// from a virtual device), the MOD section, an envelope follower on the compressor's
/// threshold and on an EQ band gain.
/// `CADENZA_SCREENSHOT_DIR=dir cargo test -p cadenza-app --release --test ui -- --ignored
/// render_modulation_screens`.
#[test]
#[ignore = "renders PNGs (wgpu); set CADENZA_SCREENSHOT_DIR"]
fn render_modulation_screens() {
	use cadenza_model::{LfoShape, Modulator, ModulatorKind};
	let Some(dir) = std::env::var_os("CADENZA_SCREENSHOT_DIR").map(PathBuf::from) else {
		return;
	};
	std::fs::create_dir_all(&dir).unwrap();
	let shot = |h: &mut Harness<'_, CadenzaApp>, name: &str| {
		h.run_steps(4);
		h.render()
			.unwrap()
			.save(dir.join(format!("{name}.png")))
			.unwrap();
	};
	// Plays `r`'s track for a while through a virtual device so the rings move.
	let live = |h: &mut Harness<'_, CadenzaApp>, r: DeviceRef, note: bool| {
		let project = h.state().state.project().clone();
		let (mut player, mut dev) = cadenza_engine::Player::virtual_device(48_000, 0);
		player.load(&project).unwrap();
		player.set_probe(Some(r.device)).unwrap();
		h.state_mut().state.attach_probe(player.probe());
		if note {
			player.preview_note_on(r.track.unwrap(), 57, 0.9).unwrap();
		} else {
			// Bar 5: drums and bass play.
			player.seek(cadenza_core::Tick::from_beats(16.5)).unwrap();
			player.play().unwrap();
		}
		let mut out = vec![0.0f32; 2 * 512];
		for _ in 0..30 {
			dev.process(&mut out, &[]);
		}
		h.run_steps(2);
		(player, dev)
	};

	let mut a = app();
	let lead = lead_synth(&a.state);
	let mut lfo = Modulator::lfo(cadenza_core::ModulatorId::new());
	if let ModulatorKind::Lfo {
		shape, sync_beats, ..
	} = &mut lfo.kind
	{
		*shape = LfoShape::Triangle;
		*sync_beats = Some(0.5);
	}
	lfo.targets = vec![
		mod_target("filter_cutoff", 0.3),
		mod_target("filter_resonance", 0.1),
	];
	let mut env = Modulator::envelope(cadenza_core::ModulatorId::new());
	env.targets = vec![mod_target("filter_cutoff", -0.1)];
	set_mods(&mut a.state, lead, vec![lfo, env]);
	a.state.view.panels.set(PanelKind::Mixer, false);
	a.state.view.panels.set(PanelKind::Arrange, false);
	assert!(a.state.open_device_in_dock(lead));
	let mut h = app_harness(a, vec2(1600.0, 1000.0), true);
	h.run_steps(3);
	let _keep = live(&mut h, lead, true);
	shot(&mut h, "mod_poly_synth_lfo");

	let mut a = app();
	let comp = {
		let t = a
			.state
			.project()
			.tracks
			.iter()
			.find(|t| t.name == "Drums")
			.unwrap();
		DeviceRef {
			track: Some(t.id),
			device: t.channel.inserts[0].id,
		}
	};
	let mut env = Modulator::envelope(cadenza_core::ModulatorId::new());
	env.targets = vec![mod_target("threshold", -0.2), mod_target("ratio", 0.15)];
	let mut lfo = Modulator::lfo(cadenza_core::ModulatorId::new());
	lfo.targets = vec![mod_target("threshold", 0.05)];
	set_mods(&mut a.state, comp, vec![env, lfo]);
	a.state.view.panels.set(PanelKind::Mixer, false);
	a.state.view.panels.set(PanelKind::Arrange, false);
	assert!(a.state.open_device_in_dock(comp));
	let mut h = app_harness(a, vec2(1600.0, 1000.0), true);
	h.run_steps(3);
	let _keep = live(&mut h, comp, false);
	shot(&mut h, "mod_compressor_envelope");

	let mut a = app();
	let eq = eq_insert(&a.state);
	let mut env = Modulator::envelope(cadenza_core::ModulatorId::new());
	env.targets = vec![mod_target("peak2_gain", -0.15)];
	a.state.commit_param(eq.track, eq.device, "peak2_gain", 4.0);
	set_mods(&mut a.state, eq, vec![env]);
	a.state.view.panels.set(PanelKind::Mixer, false);
	a.state.view.panels.set(PanelKind::Arrange, false);
	assert!(a.state.open_device_in_dock(eq));
	let mut h = app_harness(a, vec2(1280.0, 900.0), true);
	h.run_steps(3);
	let _keep = live(&mut h, eq, false);
	shot(&mut h, "mod_eq_envelope");
}

/// T34: the reverb with ducking (knobs, duck bar on the wet meter, key picker), a 4-tap
/// delay (taps 2-4 on the echo picture, dock and compact) and the limiter at 4x.
/// `CADENZA_SCREENSHOT_DIR=/tmp/shots cargo test -p cadenza-app --release --test ui --
/// --ignored render_t34_screens`.
#[test]
#[ignore = "renders PNGs (wgpu); set CADENZA_SCREENSHOT_DIR"]
fn render_t34_screens() {
	use cadenza_app::ui::devices::reverb;
	let Some(dir) = std::env::var_os("CADENZA_SCREENSHOT_DIR").map(PathBuf::from) else {
		return;
	};
	std::fs::create_dir_all(&dir).unwrap();
	let shot = |h: &mut Harness<'_, CadenzaApp>, name: &str| {
		h.run_steps(4);
		h.render()
			.unwrap()
			.save(dir.join(format!("{name}.png")))
			.unwrap();
	};

	let mut a = app();
	let r = reverb_insert(&a.state);
	a.state.commit_params(
		r.track,
		r.device,
		&[("duck", 9.0), ("duck_release", 350.0), ("mix", 30.0)],
		"Duck",
	);
	a.state.view.panels.set(PanelKind::Inspector, false);
	assert!(a.state.open_device_in_dock(r));
	let mut h = app_harness(a, vec2(1920.0, 1000.0), true);
	h.run_steps(2);
	reverb::set_display_ducked(&h.ctx, "dock", r, [-14.0, -15.5], 6.4);
	shot(&mut h, "reverb_ducking");

	let taps = [
		("taps", 4.0),
		("tap_spread", 50.0),
		("tap2_level", 80.0),
		("tap3_pan", 0.8),
		("feedback", 30.0),
	];
	let mut a = app();
	let r = delay_insert(&a.state);
	a.state.commit_params(r.track, r.device, &taps, "Taps");
	a.state.view.panels.set(PanelKind::Inspector, false);
	assert!(a.state.open_device_in_dock(r));
	let mut h = app_harness(a, vec2(1920.0, 1000.0), true);
	h.run_steps(3);
	shot(&mut h, "delay_four_taps");
	let s = &h.state().state;
	assert_eq!(dl_param(s, r, "taps"), 4.0);

	let mut a = app();
	let r = delay_insert(&a.state);
	a.state.commit_params(r.track, r.device, &taps, "Taps");
	a.state.selection.select_device(r);
	a.state.view.panels.set(PanelKind::Inspector, true);
	a.state.view.panels.set(PanelKind::Mixer, false);
	let mut h = app_harness(a, vec2(1600.0, 1000.0), true);
	h.run_steps(3);
	shot(&mut h, "delay_four_taps_compact");

	let (mut a, r) = lim_dock_app();
	a.state
		.commit_params(r.track, r.device, &[("oversample", 2.0)], "4x");
	let mut h = app_harness(a, vec2(1920.0, 1000.0), true);
	h.run_steps(3);
	shot(&mut h, "limiter_4x");
}

#[test]
fn dragging_a_delay_tap_is_one_undo_step() {
	let mut a = app();
	let r = delay_insert(&a.state);
	a.state
		.commit_params(r.track, r.device, &[("taps", 3.0)], "Taps");
	a.state.view.panels.set(PanelKind::Inspector, false);
	assert!(a.state.open_device_in_dock(r));
	let mut h = app_harness(a, vec2(1920.0, 1000.0), false);
	h.run_steps(3);
	let before = h.state().state.document().history_labels().len();
	let level = dl_param(&h.state().state, r, "tap2_level");
	let c = h.get_by_label("Tap 2").rect().center();
	h.hover_at(c);
	h.run_steps(1);
	h.drag_at(c);
	h.run_steps(1);
	for dy in [-5.0, -15.0, -20.0] {
		h.hover_at(c + vec2(0.0, dy));
		h.run_steps(1);
	}
	h.drop_at(c + vec2(0.0, -20.0));
	h.run_steps(2);
	let s = &h.state().state;
	assert_eq!(s.document().history_labels().len(), before + 1);
	assert_eq!(s.undo_label(), Some("Delay tap"));
	assert!(dl_param(s, r, "tap2_level") > level + 3.0);
}

#[test]
#[ignore = "renders PNGs (wgpu); set CADENZA_SCREENSHOT_DIR"]
fn render_wavetable_screens() {
	use cadenza_app::ui::devices::poly_synth;
	use cadenza_model::{LfoShape, Modulator, ModulatorKind};
	let Some(dir) = std::env::var_os("CADENZA_SCREENSHOT_DIR").map(PathBuf::from) else {
		return;
	};
	std::fs::create_dir_all(&dir).unwrap();
	let shot = |h: &mut Harness<'_, CadenzaApp>, name: &str| {
		h.run_steps(4);
		h.render()
			.unwrap()
			.save(dir.join(format!("{name}.png")))
			.unwrap();
	};
	let params = [
		("osc1_wave", 4.0),
		("osc1_table", 2.0),
		("osc1_position", 0.35),
		("osc2_wave", 4.0),
		("osc2_table", 0.0),
		("osc2_position", 0.6),
		("osc2_semitones", 7.0),
		("osc_mix", 35.0),
		("unison", 2.0),
		("filter_cutoff", 3500.0),
		("amp_attack", 20.0),
		("amp_sustain", 90.0),
	];
	let lfo = || {
		let mut lfo = Modulator::lfo(cadenza_core::ModulatorId::new());
		if let ModulatorKind::Lfo {
			shape, sync_beats, ..
		} = &mut lfo.kind
		{
			*shape = LfoShape::Triangle;
			*sync_beats = Some(2.0);
		}
		lfo.targets = vec![mod_target("osc1_position", 0.25)];
		lfo
	};
	let samples = synth_chord(&params, 0.6);
	for (width, name, both) in [
		(1920.0, "wavetable_dock", true),
		(1280.0, "wavetable_dock_1280_one", false),
		(900.0, "wavetable_dock_900", true),
		(540.0, "wavetable_dock_narrow", true),
	] {
		let (mut a, r) = synth_dock_app();
		let mut p = params.to_vec();
		if !both {
			p.retain(|(id, _)| !id.starts_with("osc2_"));
		}
		a.state
			.commit_params(r.track, r.device, &p, "Wavetable pad");
		set_mods(&mut a.state, r, vec![lfo()]);
		let mut h = app_harness(a, vec2(width, 1000.0), true);
		h.run_steps(2);
		poly_synth::set_scope(&h.ctx, "dock", r, &samples);
		// Play a note so the modulation ring moves.
		let project = h.state().state.project().clone();
		let (mut player, mut dev) = cadenza_engine::Player::virtual_device(48_000, 0);
		player.load(&project).unwrap();
		player.set_probe(Some(r.device)).unwrap();
		h.state_mut().state.attach_probe(player.probe());
		player.preview_note_on(r.track.unwrap(), 57, 0.9).unwrap();
		let mut out = vec![0.0f32; 2 * 512];
		for _ in 0..60 {
			dev.process(&mut out, &[]);
		}
		shot(&mut h, name);
	}

	// The compact form in the inspector.
	let (mut a, r) = synth_dock_app();
	a.state.close_device_editor();
	a.state.view.panels.set(PanelKind::Inspector, true);
	a.state
		.commit_params(r.track, r.device, &params, "Wavetable pad");
	set_mods(&mut a.state, r, vec![lfo()]);
	a.state.selection.select_device(r);
	a.state.view.panels.set(PanelKind::Mixer, false);
	a.state.view.panels.set(PanelKind::PianoRoll, false);
	let mut h = app_harness(a, vec2(1600.0, 1000.0), true);
	shot(&mut h, "wavetable_inspector_compact");
}

/// Feeds CC messages to a virtual player whose MIDI-learn monitor the app follows.
struct MidiRig {
	player: cadenza_engine::Player,
	dev: cadenza_engine::VirtualDevice,
	/// Pushes one MIDI message into the player's queue.
	send: MidiSend,
}

type MidiSend = Box<dyn FnMut(&[u8])>;

impl MidiRig {
	fn new(h: &mut Harness<'_, CadenzaApp>) -> Self {
		let (mut player, dev) = cadenza_engine::Player::virtual_device(48_000, 0);
		player.load(h.state().state.project()).unwrap();
		let (mut producer, rx) = cadenza_midi_io::new_queue(64);
		player.set_midi_queue(rx).unwrap();
		h.state_mut()
			.state
			.attach_midi_map(Some(player.midi_map_monitor()));
		let send = move |bytes: &[u8]| {
			producer
				.push(cadenza_midi_io::TimedMessage {
					host_micros: 0,
					message: cadenza_midi_io::RawMessage::from_bytes(bytes).unwrap(),
				})
				.unwrap();
		};
		Self {
			player,
			dev,
			send: Box::new(send),
		}
	}

	fn cc(&mut self, n: u8, v: u8) {
		(self.send)(&[0xB0, n, v]);
		let mut out = vec![0.0f32; 512];
		self.dev.process(&mut out, &[]);
	}
}

#[test]
fn midi_learn_from_a_knob_menu_maps_the_next_cc() {
	let (a, r) = effect_dock_app("Bass", "compressor", "Compressor");
	let mut h = app_harness(a, vec2(1920.0, 1000.0), false);
	h.run_steps(3);
	let mut rig = MidiRig::new(&mut h);
	h.get_all_by_label("Threshold")
		.find(|n| n.rect().width() < 100.0)
		.unwrap()
		.click_secondary();
	h.run_steps(2);
	h.get_by_label("MIDI learn").click();
	h.run_steps(2);
	let threshold = cadenza_core::ParamAddress::device(r.track, r.device, "threshold");
	assert!(h.state().state.is_learning(&threshold));
	// Esc cancels; learning again then takes the next CC.
	h.key_press(Key::Escape);
	h.run_steps(2);
	assert!(!h.state().state.is_learning(&threshold));
	h.state_mut().state.start_midi_learn(threshold.clone());
	rig.cc(74, 30);
	h.run_steps(2);
	let s = &h.state().state;
	assert!(!s.is_learning(&threshold));
	assert_eq!(s.undo_label(), Some("MIDI learn CC 74"));
	assert_eq!(
		cadenza_app::state::midi_learn::mapping_for(s.project(), &threshold)
			.unwrap()
			.tag(),
		"CC 74"
	);
	// The mapped controller moves the knob and lands as one step after it rests.
	rig.player.set_midi_map(h.state().state.project()).unwrap();
	let steps = h.state().state.document().history_labels().len();
	rig.cc(74, 0);
	h.run_steps(2);
	let min = h.state().state.project().midi_map[0].min;
	assert_eq!(
		builtin_param(&h.state().state, r, "compressor", "threshold"),
		min
	);
	std::thread::sleep(std::time::Duration::from_millis(450));
	h.run_steps(2);
	assert_eq!(h.state().state.document().history_labels().len(), steps + 1);
	// The mappings window lists it; × forgets it in one step.
	h.state_mut().state.midi.window_open = true;
	h.run_steps(2);
	assert!(h.query_by_label("MIDI mappings").is_some());
	h.get_by_label("×").click();
	h.run_steps(2);
	assert!(h.state().state.project().midi_map.is_empty());
}

/// MIDI learn (pulsing ring, CC badges, the mappings window) and "Explain" in the chat.
/// `CADENZA_SCREENSHOT_DIR=dir cargo test -p cadenza-app --release --test ui -- --ignored
/// render_midi_learn_screens`.
#[test]
#[ignore = "renders PNGs (wgpu); set CADENZA_SCREENSHOT_DIR"]
fn render_midi_learn_screens() {
	use cadenza_agent::scripted::ScriptedTurn;
	let Some(dir) = std::env::var_os("CADENZA_SCREENSHOT_DIR").map(PathBuf::from) else {
		return;
	};
	std::fs::create_dir_all(&dir).unwrap();
	let tmp = tempfile::tempdir().unwrap();
	let (a, r) = effect_dock_app("Bass", "compressor", "Compressor");
	let a = with_chat(
		a,
		vec![
			ScriptedTurn::tools(None, &[("get_project", "{}")]),
			ScriptedTurn::text(
				"**Compressor on Bass** keeps the bass even so it sits steady under the kick.\n\n- **Threshold -24 dB**: it starts working on most notes.\n- **Ratio 4:1**: firm but musical control.\n- **Attack 10 ms**: lets the pluck through before clamping.\n\nSuggestion: a slower attack (20-30 ms) would keep more punch.",
			),
		],
		0,
		tmp.path(),
	);
	let mut h = app_harness(a, vec2(1600.0, 1000.0), true);
	h.run_steps(3);
	let mut rig = MidiRig::new(&mut h);
	let address = |p: &str| cadenza_core::ParamAddress::device(r.track, r.device, p);
	h.state_mut().state.start_midi_learn(address("ratio"));
	rig.cc(71, 64);
	h.run_steps(2);
	h.state_mut().state.start_midi_learn(address("attack"));
	rig.cc(72, 64);
	h.run_steps(2);
	h.state_mut().state.start_midi_learn(address("threshold"));
	h.run_steps(5);
	shot(&mut h, &dir, "midi_learn_ring");
	rig.cc(74, 64);
	h.run_steps(3);
	shot(&mut h, &dir, "midi_learn_badges");
	h.state_mut().state.midi.window_open = true;
	h.run_steps(3);
	shot(&mut h, &dir, "midi_mappings_window");
	h.state_mut().state.midi.window_open = false;
	h.get_by_label("Explain").click();
	for _ in 0..200 {
		h.run_steps(1);
		if !h.state().state.ai.busy() {
			break;
		}
		std::thread::sleep(std::time::Duration::from_millis(10));
	}
	h.run_steps(5);
	shot(&mut h, &dir, "explain_device_chat");
	assert!(h.state().state.project().midi_map.len() == 3);
}
