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
