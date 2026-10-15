//! Plugin editors of compiled graphs: every plugin device has an editor handle, and the
//! edits made in a (scripted, display-free) editor reach the host both while the graph
//! plays and while the transport is stopped (`Processor::idle`), in-process and
//! sandboxed.

// Plugin editors are X11-only for now (`EDITORS_SUPPORTED`).
#![cfg(all(unix, not(target_os = "macos")))]

mod common;
mod plugin_support;

use std::time::{Duration, Instant};

use cadenza_engine::{EditorEvent, Graph, PluginHosting, QUANTUM, compile_hosted};
use cadenza_model::{Device, DeviceKind, Project};
use common::*;
use plugin_support::{GUI_ID, gui_catalog, sandboxed};

const SR: u32 = 48_000;

fn gui_project() -> (Project, Device) {
	let path = gui_catalog().find(GUI_ID).expect("scanned").path.clone();
	let mut fx = Device::builtin("unused", "Test GUI");
	fx.kind = DeviceKind::Clap {
		plugin_id: GUI_ID.into(),
		path,
	};
	let mut track = synth_track("lead", &[(60, 0.0, 1.0)], 4.0);
	track.channel.inserts.push(fx.clone());
	(project(vec![track]), fx)
}

fn graph(hosting: &PluginHosting) -> (Graph, Device) {
	let (project, fx) = gui_project();
	let graph = compile_hosted(&project, SR, QUANTUM, Some(&gui_catalog()), hosting).unwrap();
	assert!(graph.warnings().is_empty(), "{:?}", graph.warnings());
	let synth = project.tracks[0].instrument.as_ref().unwrap().id;
	assert!(
		graph.plugin_editor(synth).is_none(),
		"built-ins have no editor"
	);
	(graph, fx)
}

fn scripted_edit(graph: &mut Graph, fx: &Device, playing: bool) {
	let editor = graph.plugin_editor(fx.id).expect("plugin editor");
	editor.open("Test GUI — lead").unwrap();
	if playing {
		graph.play();
	}
	let (mut l, mut r) = (vec![0.0f32; 256], vec![0.0f32; 256]);
	let started = Instant::now();
	let mut events = Vec::new();
	while !events.contains(&EditorEvent::GestureEnd { param: "0".into() }) {
		assert!(started.elapsed() < Duration::from_secs(10), "{events:?}");
		graph.process(&mut l, &mut r);
		events.extend(editor.poll());
		std::thread::sleep(Duration::from_millis(5));
	}
	assert!(
		events.contains(&EditorEvent::Value {
			param: "0".into(),
			value: -6.0
		}),
		"{events:?}"
	);
	editor.close().unwrap();
}

#[test]
fn editor_edits_reach_the_host_while_stopped_and_playing_in_process() {
	let (mut g, fx) = graph(&PluginHosting::in_process());
	scripted_edit(&mut g, &fx, false);
	let (mut g, fx) = graph(&PluginHosting::in_process());
	scripted_edit(&mut g, &fx, true);
}

#[test]
fn editor_edits_reach_the_host_while_stopped_and_playing_sandboxed() {
	let (mut g, fx) = graph(&sandboxed());
	scripted_edit(&mut g, &fx, false);
	let (mut g, fx) = graph(&sandboxed());
	scripted_edit(&mut g, &fx, true);
}

/// The `Player` side: open, edits while stopped, editors kept across reloads, closed
/// when their device goes away. Skips without an audio output (the transport stays
/// stopped: nothing is heard).
#[test]
fn player_editors_survive_reloads_and_close_with_their_device() {
	use cadenza_engine::{EditorUpdate, Player};
	let mut player = match Player::new() {
		Ok(p) => p,
		Err(e) => {
			eprintln!("skipping player editor test: {e}");
			return;
		}
	};
	player.set_plugins(Some(gui_catalog()));
	player.set_hosting(sandboxed());
	let (project, fx) = gui_project();
	player.load(&project).unwrap();
	assert!(player.has_editor(fx.id));
	let synth = project.tracks[0].instrument.as_ref().unwrap().id;
	assert!(player.open_editor(synth, "Synth").is_err());
	player.open_editor(fx.id, "Test GUI — lead").unwrap();
	let wait = |player: &mut Player, want: &dyn Fn(&[EditorUpdate]) -> bool| {
		let started = Instant::now();
		let mut seen = Vec::new();
		while !want(&seen) {
			assert!(started.elapsed() < Duration::from_secs(10), "{seen:?}");
			seen.extend(player.poll_editors());
			std::thread::sleep(Duration::from_millis(20));
		}
		seen
	};
	let ended = |u: &[EditorUpdate]| {
		u.iter()
			.any(|u| u.event == EditorEvent::GestureEnd { param: "0".into() })
	};
	wait(&mut player, &ended);
	// The edit's own value is not echoed back to the plugin.
	assert!(player.set_live_param(fx.id, "0", -6.0).unwrap());

	// A reload moves the editor to the new instance (which scripts its edit again)
	// and carries the changed state over.
	player.load(&project).unwrap();
	assert_eq!(player.open_editors(), vec![fx.id]);
	wait(&mut player, &ended);
	player.capture_editor_states(&project, &[fx.id]);
	let states = player.take_editor_states();
	assert!(states.iter().any(|s| s.device == fx.id), "{states:?}");

	// Without the device, the editor closes.
	let (empty, _) = gui_project();
	let mut doc = cadenza_model::Document::new(empty);
	let track = doc.project().tracks[0].id;
	let fx_in_doc = doc.project().tracks[0].channel.inserts[0].id;
	doc.execute(cadenza_model::Command::RemoveDevice {
		track: Some(track),
		device: fx_in_doc,
	})
	.unwrap();
	player.load(doc.project()).unwrap();
	let closed = player.poll_editors();
	assert_eq!(
		closed,
		vec![EditorUpdate {
			device: fx.id,
			event: EditorEvent::Closed
		}]
	);
	assert!(player.open_editors().is_empty());
}
