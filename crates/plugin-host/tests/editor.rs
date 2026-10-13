//! Plugin editors without a display: the scripted test plugin
//! (`dev.cadenza.test.gui`) opens a "floating" X11 editor that needs no X
//! server, then reports a GUI edit through the CLAP timer, flush and output
//! event paths. Covered in-process and sandboxed, while processing and while
//! idle (transport stopped). Display-dependent tests live in
//! `editor_display.rs`.

// Plugin editors are X11-only for now (`EDITORS_SUPPORTED`).
#![cfg(all(unix, not(target_os = "macos")))]

mod common;

use std::time::{Duration, Instant};

use cadenza_dsp::{Processor, Transport};
use cadenza_plugin_host::{
	EditorEvent, PluginEditor, ProcessMode, instantiate_sandboxed, instantiate_with_editor,
};
use common::{gui_descriptor, sandbox_options};

const SR: f32 = 48_000.0;
const BLOCK: usize = 64;

fn transport() -> Transport {
	Transport {
		bpm: 120.0,
		playing: true,
		beat_position: 0.0,
		..Default::default()
	}
}

/// Drives the processor (blocks, or idle calls when `idle`) and polls the
/// editor until `done` accepts the events seen so far.
fn pump(
	p: &mut dyn Processor,
	editor: &PluginEditor,
	idle: bool,
	done: impl Fn(&[EditorEvent]) -> bool,
) -> Vec<EditorEvent> {
	let started = Instant::now();
	let mut events = Vec::new();
	let (mut l, mut r) = (vec![0.0f32; BLOCK], vec![0.0f32; BLOCK]);
	while !done(&events) {
		assert!(
			started.elapsed() < Duration::from_secs(10),
			"editor events never arrived; got {events:?}"
		);
		if idle {
			p.idle();
		} else {
			p.process(&mut l, &mut r, &[], &transport());
		}
		events.extend(editor.poll());
		std::thread::sleep(Duration::from_millis(5));
	}
	events
}

fn scripted_edit(events: &[EditorEvent]) -> bool {
	events.contains(&EditorEvent::GestureEnd { param: "0".into() })
		&& events.contains(&EditorEvent::StateChanged)
}

fn check_scripted_edit(events: &[EditorEvent]) {
	let gesture: Vec<&EditorEvent> = events
		.iter()
		.filter(|e| !matches!(e, EditorEvent::StateChanged))
		.collect();
	assert_eq!(
		gesture,
		[
			&EditorEvent::GestureBegin { param: "0".into() },
			&EditorEvent::Value {
				param: "0".into(),
				value: -6.0
			},
			&EditorEvent::GestureEnd { param: "0".into() },
		],
		"{events:?}"
	);
}

/// The full editor round: open, scripted edit, live state, plugin-initiated
/// close, reopen, host close.
fn editor_round(p: &mut dyn Processor, editor: &PluginEditor, idle: bool) {
	assert!(!editor.is_open());
	editor.open("Cadenza Test GUI — Track 1").unwrap();
	assert!(editor.is_open());
	let events = pump(p, editor, idle, scripted_edit);
	check_scripted_edit(&events);

	// The edit is part of the live instance's state.
	let state = editor.save_state().unwrap().expect("the plugin has state");
	assert_eq!(&state[..4], b"CGUI");
	assert_eq!(f32::from_le_bytes(state[4..8].try_into().unwrap()), -6.0);

	// Opening again only raises the window.
	editor.open("Cadenza Test GUI — renamed").unwrap();

	// The plugin closes its own window (as the user would): reported once.
	p.set_param(1, 1.0);
	let events = pump(p, editor, idle, |e| e.contains(&EditorEvent::Closed));
	assert_eq!(
		events.iter().filter(|e| **e == EditorEvent::Closed).count(),
		1
	);
	assert!(!editor.is_open());
	p.set_param(1, 0.0);
	pump(p, editor, idle, |_| true);

	editor.open("Cadenza Test GUI — again").unwrap();
	assert!(editor.is_open());
	editor.close().unwrap();
	assert!(!editor.is_open());
	// Closing twice is harmless, and nothing is reported for host closes.
	editor.close().unwrap();
	assert!(!editor.poll().contains(&EditorEvent::Closed));
}

#[test]
fn in_process_editor_reports_gui_edits_while_processing() {
	let (mut p, editor) =
		instantiate_with_editor(gui_descriptor(), SR, BLOCK, None, ProcessMode::Realtime).unwrap();
	editor_round(p.as_mut(), &editor, false);
}

#[test]
fn in_process_editor_reports_gui_edits_while_idle() {
	let (mut p, editor) =
		instantiate_with_editor(gui_descriptor(), SR, BLOCK, None, ProcessMode::Realtime).unwrap();
	editor_round(p.as_mut(), &editor, true);
}

#[test]
fn sandboxed_editor_reports_gui_edits_while_processing() {
	let mut p = instantiate_sandboxed(
		gui_descriptor(),
		SR,
		BLOCK,
		None,
		ProcessMode::Realtime,
		sandbox_options(),
	)
	.unwrap();
	let editor = p.editor();
	editor_round(&mut p, &editor, false);
}

#[test]
fn sandboxed_editor_reports_gui_edits_while_idle() {
	let mut p = instantiate_sandboxed(
		gui_descriptor(),
		SR,
		BLOCK,
		None,
		ProcessMode::Realtime,
		sandbox_options(),
	)
	.unwrap();
	let editor = p.editor();
	editor_round(&mut p, &editor, true);
}

#[test]
fn editor_handles_outlive_their_instance_and_crashes_close_the_editor() {
	let mut p = instantiate_sandboxed(
		gui_descriptor(),
		SR,
		BLOCK,
		None,
		ProcessMode::Realtime,
		sandbox_options(),
	)
	.unwrap();
	let editor = p.editor();
	editor.open("Crash — Track").unwrap();
	// The sandbox process dies: the editor is reported closed, once.
	#[cfg(unix)]
	unsafe {
		libc::kill(p.pid().unwrap() as i32, libc::SIGKILL)
	};
	let started = Instant::now();
	let mut events = Vec::new();
	while !events.contains(&EditorEvent::Closed) {
		assert!(started.elapsed() < Duration::from_secs(5), "{events:?}");
		let (mut l, mut r) = (vec![0.0f32; BLOCK], vec![0.0f32; BLOCK]);
		p.process(&mut l, &mut r, &[], &transport());
		events.extend(editor.poll());
		std::thread::sleep(Duration::from_millis(10));
	}
	assert!(!editor.is_open());
	assert!(editor.poll().is_empty());
	assert!(editor.open("again").is_err());
	assert!(editor.save_state().is_err());

	// In-process: dropping the processor tears the editor down with it; the
	// handle then fails cleanly.
	let (p, editor) =
		instantiate_with_editor(gui_descriptor(), SR, BLOCK, None, ProcessMode::Realtime).unwrap();
	editor.open("Dropped — Track").unwrap();
	drop(p);
	let started = Instant::now();
	while editor.open("Dropped — Track").is_ok() {
		assert!(started.elapsed() < Duration::from_secs(5));
		std::thread::sleep(Duration::from_millis(10));
	}
	assert!(editor.save_state().unwrap().is_none());
}

#[test]
fn plugins_without_an_editor_say_so() {
	let (_p, editor) = instantiate_with_editor(
		common::descriptor(common::GAIN_ID),
		SR,
		BLOCK,
		None,
		ProcessMode::Realtime,
	)
	.unwrap();
	let err = editor.open("Gain — Track").unwrap_err().to_string();
	assert!(err.contains("no editor"), "{err}");
	assert!(!editor.is_open());
}
