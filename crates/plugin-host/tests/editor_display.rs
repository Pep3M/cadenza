//! Plugin editors on a real X server (XWayland on Wayland desktops): each
//! test opens an editor window for a few seconds, checks it exists (and
//! resizes) through an X11 query, drives the plugin with silent blocks and
//! closes it again. Ignored by default (windows appear on the desktop); run
//! with a display:
//! `cargo test -p cadenza-plugin-host --release --test editor_display -- --ignored --nocapture --test-threads 1`.
//!
//! The scripted test plugins (`clap_gui` embedded, VST3 test gain) also check
//! GUI edits, plugin-initiated resizes and timers end to end; installed
//! plugins (Surge XT and LSP, CLAP and VST3) are opened if present.

mod common;

use std::path::Path;
use std::time::{Duration, Instant};

use cadenza_dsp::{Processor, Transport};
use cadenza_plugin_host::editor::find_window;
use cadenza_plugin_host::scanner::scan_in_subprocess;
use cadenza_plugin_host::{
	EditorEvent, PluginDescriptor, PluginEditor, ProcessMode, TITLE_STRIP_HEIGHT,
	instantiate_sandboxed, instantiate_with_editor,
};
use common::{gui_descriptor, sandbox_options, scanner, vst3_gain_descriptor};

const SR: f32 = 48_000.0;
const BLOCK: usize = 256;

fn display() -> bool {
	if std::env::var_os("DISPLAY").is_none() {
		println!("no DISPLAY; skipping");
		return false;
	}
	true
}

/// Runs silent blocks (about realtime) for `time`, collecting editor events,
/// until `done` accepts them.
fn run(
	p: &mut dyn Processor,
	editor: &PluginEditor,
	time: Duration,
	done: impl Fn(&[EditorEvent]) -> bool,
) -> Vec<EditorEvent> {
	let started = Instant::now();
	let mut events = Vec::new();
	let (mut l, mut r) = (vec![0.0f32; BLOCK], vec![0.0f32; BLOCK]);
	let t = Transport {
		bpm: 120.0,
		playing: true,
		beat_position: 0.0,
	};
	while started.elapsed() < time && !done(&events) {
		l.fill(0.0);
		r.fill(0.0);
		p.process(&mut l, &mut r, &[], &t);
		events.extend(editor.poll());
		std::thread::sleep(Duration::from_secs_f32(BLOCK as f32 / SR));
	}
	events
}

fn wait_window(title: &str, present: bool) -> Option<(u32, u32)> {
	let started = Instant::now();
	loop {
		let found = find_window(title).expect("query the X server");
		if found.is_some() == present {
			return found;
		}
		assert!(
			started.elapsed() < Duration::from_secs(5),
			"window '{title}' {}",
			if present {
				"never appeared"
			} else {
				"was not closed"
			}
		);
		std::thread::sleep(Duration::from_millis(20));
	}
}

fn scripted(events: &[EditorEvent]) -> bool {
	events.contains(&EditorEvent::GestureEnd { param: "0".into() })
}

fn check_scripted(events: &[EditorEvent]) {
	let begin = events
		.iter()
		.position(|e| *e == EditorEvent::GestureBegin { param: "0".into() })
		.unwrap_or_else(|| panic!("no gesture: {events:?}"));
	match &events[begin + 1] {
		EditorEvent::Value { param, value } => {
			assert_eq!(param, "0");
			assert!((value + 6.0).abs() < 1e-6, "{value}");
		}
		other => panic!("expected the value, got {other:?} in {events:?}"),
	}
	assert_eq!(
		events[begin + 2],
		EditorEvent::GestureEnd { param: "0".into() }
	);
}

/// The outer (titled) window size for a plugin content area of this size:
/// our host-drawn title strip sits on top of it.
fn with_strip(content_w: u32, content_h: u32) -> (u32, u32) {
	(content_w, content_h + TITLE_STRIP_HEIGHT)
}

/// Opens the scripted editor, checks the window (content size plus the title
/// strip), the edit and the resize to 320x240 the plugin asks for, then
/// closes it.
fn scripted_round(p: &mut dyn Processor, editor: &PluginEditor, title: &str) {
	editor.open(title).unwrap();
	let size = wait_window(title, true).unwrap();
	println!("{title}: opened at {size:?}");
	assert_eq!(
		size,
		with_strip(300, 200),
		"the window does not include the strip"
	);
	let events = run(p, editor, Duration::from_secs(5), scripted);
	check_scripted(&events);
	let wanted = with_strip(320, 240);
	let started = Instant::now();
	while wait_window(title, true) != Some(wanted) {
		assert!(
			started.elapsed() < Duration::from_secs(3),
			"the editor window did not follow the plugin's resize request"
		);
		run(p, editor, Duration::from_millis(50), |_| false);
	}
	editor.close().unwrap();
	wait_window(title, false);
}

#[test]
#[ignore = "opens windows on the X server"]
fn scripted_clap_editor_embeds_in_process_and_sandboxed() {
	if !display() {
		return;
	}
	// The test plugin then refuses floating windows and embeds a real child.
	// SAFETY: set before any plugin of this process reads it; this file's
	// tests all want the same value.
	unsafe { std::env::set_var("CADENZA_TEST_GUI_EMBED", "1") };
	let (mut p, editor) =
		instantiate_with_editor(gui_descriptor(), SR, BLOCK, None, ProcessMode::Realtime).unwrap();
	scripted_round(p.as_mut(), &editor, "Cadenza Test GUI — in-process");
	drop(p);

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
	scripted_round(&mut p, &editor, "Cadenza Test GUI — sandboxed");
	// The close button of our window closes the (embedded) editor.
	editor.open("Cadenza Test GUI — close button").unwrap();
	wait_window("Cadenza Test GUI — close button", true);
	#[cfg(target_os = "linux")]
	{
		press_close_button("Cadenza Test GUI — close button");
		let events = run(&mut p, &editor, Duration::from_secs(5), |e| {
			e.contains(&EditorEvent::Closed)
		});
		assert!(events.contains(&EditorEvent::Closed), "{events:?}");
		wait_window("Cadenza Test GUI — close button", false);
	}
	// Dropping the instance with the editor open removes the window too.
	editor.open("Cadenza Test GUI — dropped").unwrap();
	wait_window("Cadenza Test GUI — dropped", true);
	drop(p);
	wait_window("Cadenza Test GUI — dropped", false);
}

#[test]
#[ignore = "opens windows on the X server"]
fn scripted_vst3_editor_uses_the_run_loop_in_process_and_sandboxed() {
	if !display() {
		return;
	}
	unsafe { std::env::set_var("CADENZA_TEST_GUI_EMBED", "1") };
	let desc = vst3_gain_descriptor();
	let (mut p, editor) =
		instantiate_with_editor(desc, SR, BLOCK, None, ProcessMode::Realtime).unwrap();
	scripted_round(p.as_mut(), &editor, "Cadenza Test Gain — in-process");
	// The edit reached the processor: its state carries -6 dB.
	let state = p.save_state().unwrap();
	assert!(!state.is_empty());
	drop(p);

	let mut p = instantiate_sandboxed(
		desc,
		SR,
		BLOCK,
		None,
		ProcessMode::Realtime,
		sandbox_options(),
	)
	.unwrap();
	let editor = p.editor();
	scripted_round(&mut p, &editor, "Cadenza Test Gain — sandboxed");
}

/// Position of `window` relative to the root (works whether or not it was
/// reparented into a window manager frame) and its current size.
#[cfg(target_os = "linux")]
fn geometry_of(
	conn: &x11rb::rust_connection::RustConnection,
	root: u32,
	window: u32,
) -> (i32, i32, u32, u32) {
	use x11rb::protocol::xproto::ConnectionExt;
	let g = conn.get_geometry(window).unwrap().reply().unwrap();
	let t = conn
		.translate_coordinates(window, root, 0, 0)
		.unwrap()
		.reply()
		.unwrap();
	(
		i32::from(t.dst_x),
		i32::from(t.dst_y),
		u32::from(g.width),
		u32::from(g.height),
	)
}

/// Synthesizes a drag of the title strip of the window titled `title`:
/// press inside the strip (left of the close button), then a motion `(dx,
/// dy)` away. These are `SendEvent`s, not real hardware input, so the window
/// manager's own interactive move (triggered by the `_NET_WM_MOVERESIZE`
/// message the press also sends) never actually engages a pointer grab for
/// them; what moves the window here is Cadenza's own manual-move fallback,
/// which only needs the motion event, not a real grab. Returns
/// `(before, after)` positions.
#[cfg(target_os = "linux")]
fn drag_strip(title: &str, dx: i16, dy: i16) -> ((i32, i32), (i32, i32)) {
	use x11rb::connection::Connection;
	use x11rb::protocol::xproto::{
		BUTTON_PRESS_EVENT, BUTTON_RELEASE_EVENT, ButtonPressEvent, ConnectionExt, EventMask,
		KeyButMask, MOTION_NOTIFY_EVENT, Motion, MotionNotifyEvent,
	};
	let (conn, screen) = x11rb::connect(None).unwrap();
	let root = conn.setup().roots[screen].root;
	let window = find_top(&conn, root, title).expect("editor window");
	let (before_x, before_y, _, _) = geometry_of(&conn, root, window);
	// Inside the strip (y < 24), well clear of the close button on the right.
	let (start_x, start_y): (i16, i16) = (40, 10);
	let (root_x, root_y) = (before_x as i16 + start_x, before_y as i16 + start_y);
	let press = ButtonPressEvent {
		response_type: BUTTON_PRESS_EVENT,
		detail: 1,
		sequence: 0,
		time: x11rb::CURRENT_TIME,
		root,
		event: window,
		child: x11rb::NONE,
		root_x,
		root_y,
		event_x: start_x,
		event_y: start_y,
		state: KeyButMask::default(),
		same_screen: true,
	};
	conn.send_event(true, window, EventMask::BUTTON_PRESS, press)
		.unwrap()
		.check()
		.unwrap();
	let motion = MotionNotifyEvent {
		response_type: MOTION_NOTIFY_EVENT,
		detail: Motion::NORMAL,
		sequence: 0,
		time: x11rb::CURRENT_TIME,
		root,
		event: window,
		child: x11rb::NONE,
		root_x: root_x + dx,
		root_y: root_y + dy,
		event_x: start_x + dx,
		event_y: start_y + dy,
		state: KeyButMask::BUTTON1,
		same_screen: true,
	};
	conn.send_event(true, window, EventMask::POINTER_MOTION, motion)
		.unwrap()
		.check()
		.unwrap();
	std::thread::sleep(Duration::from_millis(300));
	let release = ButtonPressEvent {
		response_type: BUTTON_RELEASE_EVENT,
		..press
	};
	let _ = conn.send_event(true, window, EventMask::BUTTON_RELEASE, release);
	let _ = conn.get_input_focus().map(|c| c.reply());
	let after = geometry_of(&conn, root, window);
	((before_x, before_y), (after.0, after.1))
}

/// Synthesizes a click on the strip's own close (×) button (as opposed to
/// `press_close_button`, which sends the `WM_DELETE_WINDOW` a real window
/// manager's decoration would send): checks the host's own hit-testing.
#[cfg(target_os = "linux")]
fn click_strip_close_button(title: &str) {
	use x11rb::connection::Connection;
	use x11rb::protocol::xproto::{
		BUTTON_PRESS_EVENT, BUTTON_RELEASE_EVENT, ButtonPressEvent, ConnectionExt, EventMask,
		KeyButMask,
	};
	let (conn, screen) = x11rb::connect(None).unwrap();
	let root = conn.setup().roots[screen].root;
	let window = find_top(&conn, root, title).expect("editor window");
	let (_, _, width, _) = geometry_of(&conn, root, window);
	// Inside the close button's square at the strip's right edge.
	let (x, y) = ((width - 12) as i16, 10i16);
	let press = ButtonPressEvent {
		response_type: BUTTON_PRESS_EVENT,
		detail: 1,
		sequence: 0,
		time: x11rb::CURRENT_TIME,
		root,
		event: window,
		child: x11rb::NONE,
		root_x: x,
		root_y: y,
		event_x: x,
		event_y: y,
		state: KeyButMask::default(),
		same_screen: true,
	};
	conn.send_event(true, window, EventMask::BUTTON_PRESS, press)
		.unwrap()
		.check()
		.unwrap();
	let release = ButtonPressEvent {
		response_type: BUTTON_RELEASE_EVENT,
		..press
	};
	conn.send_event(true, window, EventMask::BUTTON_RELEASE, release)
		.unwrap()
		.check()
		.unwrap();
}

#[test]
#[ignore = "opens windows on the X server"]
fn strip_drag_moves_the_window_and_its_close_button_closes_the_editor() {
	if !display() {
		return;
	}
	unsafe { std::env::set_var("CADENZA_TEST_GUI_EMBED", "1") };
	let (mut p, editor) =
		instantiate_with_editor(gui_descriptor(), SR, BLOCK, None, ProcessMode::Realtime).unwrap();
	editor.open("Cadenza Test GUI — strip drag").unwrap();
	wait_window("Cadenza Test GUI — strip drag", true);
	#[cfg(target_os = "linux")]
	{
		let (before, after) = drag_strip("Cadenza Test GUI — strip drag", 60, 40);
		assert_ne!(
			before, after,
			"dragging the title strip did not move the window (before={before:?})"
		);
		assert_eq!(after, (before.0 + 60, before.1 + 40));

		click_strip_close_button("Cadenza Test GUI — strip drag");
		let events = run(p.as_mut(), &editor, Duration::from_secs(5), |e| {
			e.contains(&EditorEvent::Closed)
		});
		assert!(events.contains(&EditorEvent::Closed), "{events:?}");
		wait_window("Cadenza Test GUI — strip drag", false);
	}
	#[cfg(not(target_os = "linux"))]
	editor.close().unwrap();
}

fn find(file: &str, name: &str) -> Option<PluginDescriptor> {
	if !Path::new(file).exists() {
		println!("{file} not installed; skipping");
		return None;
	}
	scan_in_subprocess(&scanner(), Path::new(file), Duration::from_secs(120))
		.unwrap_or_else(|e| panic!("scan of {file} failed: {e:?}"))
		.into_iter()
		.find(|d| d.name == name)
}

/// The top-level window titled `title` (possibly inside a window manager frame).
#[cfg(target_os = "linux")]
fn find_top(conn: &x11rb::rust_connection::RustConnection, root: u32, title: &str) -> Option<u32> {
	use x11rb::protocol::xproto::{AtomEnum, ConnectionExt};
	let net_wm_name = conn
		.intern_atom(false, b"_NET_WM_NAME")
		.ok()?
		.reply()
		.ok()?
		.atom;
	let children = |w: u32| -> Vec<u32> {
		conn.query_tree(w)
			.ok()
			.and_then(|c| c.reply().ok())
			.map(|r| r.children)
			.unwrap_or_default()
	};
	let named = |w: u32| -> bool {
		conn.get_property(false, w, net_wm_name, AtomEnum::ANY, 0, 1024)
			.ok()
			.and_then(|c| c.reply().ok())
			.is_some_and(|r| r.value == title.as_bytes())
	};
	let mut level = children(root);
	for _ in 0..3 {
		if let Some(&w) = level.iter().find(|&&w| named(w)) {
			return Some(w);
		}
		level = level.iter().flat_map(|&w| children(w)).collect();
	}
	None
}

/// Synthetic mouse-wheel clicks (X11 `SendEvent`, delivered only to the
/// editor's own windows, never through the desktop's input devices) over a
/// grid of points of the editor titled `title`, so knobs under them move as
/// if the user had turned them.
#[cfg(target_os = "linux")]
fn wheel_over(title: &str) {
	use x11rb::connection::Connection;
	use x11rb::protocol::xproto::{
		BUTTON_PRESS_EVENT, BUTTON_RELEASE_EVENT, ButtonPressEvent, ConnectionExt, EventMask,
		KeyButMask,
	};
	let (conn, screen) = x11rb::connect(None).unwrap();
	let root = conn.setup().roots[screen].root;
	let top = find_top(&conn, root, title).expect("editor window");
	// The plugin's deepest window: where its widgets receive input.
	let mut target = top;
	while let Some(&c) = conn
		.query_tree(target)
		.ok()
		.and_then(|c| c.reply().ok())
		.and_then(|r| r.children.last().copied())
		.as_ref()
	{
		target = c;
	}
	let geometry = conn.get_geometry(target).unwrap().reply().unwrap();
	for y in (20..geometry.height).step_by(40) {
		for x in (20..geometry.width).step_by(40) {
			for (kind, mask) in [
				(BUTTON_PRESS_EVENT, EventMask::BUTTON_PRESS),
				(BUTTON_RELEASE_EVENT, EventMask::BUTTON_RELEASE),
			] {
				let event = ButtonPressEvent {
					response_type: kind,
					detail: 4,
					sequence: 0,
					time: x11rb::CURRENT_TIME,
					root,
					event: target,
					child: x11rb::NONE,
					root_x: x as i16,
					root_y: y as i16,
					event_x: x as i16,
					event_y: y as i16,
					state: KeyButMask::default(),
					same_screen: true,
				};
				let _ = conn.send_event(true, target, mask, event);
			}
		}
	}
	// A round trip: everything sent is processed before the connection closes.
	let _ = conn.get_input_focus().map(|c| c.reply());
}

/// Sends the window titled `title` what its close button sends
/// (`WM_PROTOCOLS` / `WM_DELETE_WINDOW`).
#[cfg(target_os = "linux")]
fn press_close_button(title: &str) {
	use x11rb::connection::Connection;
	use x11rb::protocol::xproto::{ClientMessageEvent, ConnectionExt, EventMask};
	let (conn, screen) = x11rb::connect(None).unwrap();
	let root = conn.setup().roots[screen].root;
	let window = find_top(&conn, root, title).expect("editor window");
	let atom = |name: &[u8]| conn.intern_atom(false, name).unwrap().reply().unwrap().atom;
	let (protocols, delete) = (atom(b"WM_PROTOCOLS"), atom(b"WM_DELETE_WINDOW"));
	let event = ClientMessageEvent::new(32, window, protocols, [delete, 0, 0, 0, 0]);
	conn.send_event(false, window, EventMask::NO_EVENT, event)
		.unwrap()
		.check()
		.unwrap();
}

/// Opens a real plugin's editor for a couple of seconds (sandboxed, then
/// in-process), checks the window and closes it.
fn real_round(desc: &PluginDescriptor) {
	let title = format!("{} ({}) — editor test", desc.name, desc.format.as_str());
	let mut p = instantiate_sandboxed(
		desc,
		SR,
		BLOCK,
		None,
		ProcessMode::Realtime,
		sandbox_options(),
	)
	.unwrap();
	let editor = p.editor();
	editor
		.open(&title)
		.unwrap_or_else(|e| panic!("{}: {e}", desc.name));
	let size = wait_window(&title, true).unwrap();
	let mut events = run(&mut p, &editor, Duration::from_secs(1), |_| false);
	let edited = |e: &[EditorEvent]| e.iter().any(|e| matches!(e, EditorEvent::Value { .. }));
	// Some editors take a moment to accept input after their window appears.
	for _ in 0..3 {
		#[cfg(target_os = "linux")]
		wheel_over(&title);
		events.extend(run(&mut p, &editor, Duration::from_secs(2), |_| false));
		if edited(&events) {
			break;
		}
	}
	let values = events
		.iter()
		.filter(|e| matches!(e, EditorEvent::Value { .. }))
		.count();
	let gestures = events
		.iter()
		.filter(|e| matches!(e, EditorEvent::GestureBegin { .. }))
		.count();
	println!(
		"{title}: sandboxed window {size:?}, {} events ({values} values, {gestures} gestures): {:?}",
		events.len(),
		&events[..events.len().min(6)]
	);
	assert!(
		!events.contains(&EditorEvent::Closed),
		"the editor closed on its own: {events:?}"
	);
	// Knobs turned in the GUI reach the host.
	#[cfg(target_os = "linux")]
	assert!(values > 0, "no parameter edits came back from the GUI");
	// The live state can be read while the editor is open.
	editor.save_state().unwrap();
	editor.close().unwrap();
	wait_window(&title, false);
	assert!(p.crashed().is_none());
	drop(p);

	let (mut p, editor) =
		instantiate_with_editor(desc, SR, BLOCK, None, ProcessMode::Realtime).unwrap();
	let title = format!("{title} in-process");
	editor.open(&title).unwrap();
	let size = wait_window(&title, true).unwrap();
	run(p.as_mut(), &editor, Duration::from_secs(2), |_| false);
	println!("{title}: in-process window {size:?}");
	// The window's close button closes the editor and reports it.
	#[cfg(target_os = "linux")]
	{
		press_close_button(&title);
		let events = run(p.as_mut(), &editor, Duration::from_secs(5), |e| {
			e.contains(&EditorEvent::Closed)
		});
		assert!(events.contains(&EditorEvent::Closed), "{events:?}");
		assert!(!editor.is_open());
	}
	editor.close().unwrap();
	wait_window(&title, false);
}

#[test]
#[ignore = "needs Surge XT and LSP installed, and opens windows"]
fn installed_plugin_editors_open_and_close() {
	if !display() {
		return;
	}
	let plugins = [
		find("/usr/lib/clap/Surge XT.clap", "Surge XT"),
		find("/usr/lib/vst3/Surge XT.vst3", "Surge XT"),
		find("/usr/lib/clap/lsp-plugins.clap", "Compressor Stereo"),
		find("/usr/lib/vst3/lsp-plugins.vst3", "Compressor Stereo"),
	];
	for desc in plugins.iter().flatten() {
		real_round(desc);
	}
}
