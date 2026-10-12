//! Sidechain inputs, extra outputs and factory presets of CLAP plugins, in
//! process and sandboxed, against the `clap_routing` test plugins.

mod common;

use cadenza_dsp::{MidiEvent, Processor, Transport};
use cadenza_plugin_host::{AuxOutput, ProcessMode, instantiate, instantiate_sandboxed};
use common::{DUCKER_ID, MULTI_ID, routing_descriptor, sandbox_options};

const SR: f32 = 48_000.0;
const BLOCK: usize = 64;

fn open(id: &str, sandboxed: bool, state: Option<&[u8]>) -> Box<dyn Processor> {
	let desc = routing_descriptor(id);
	if sandboxed {
		Box::new(
			instantiate_sandboxed(
				desc,
				SR,
				BLOCK,
				state,
				ProcessMode::Realtime,
				sandbox_options(),
			)
			.expect("sandboxed instance"),
		)
	} else {
		instantiate(desc, SR, BLOCK, state).expect("in-process instance")
	}
}

/// One block of the ducker with a constant main input and optional key.
fn duck(p: &mut dyn Processor, main: f32, key: Option<f32>) -> (f32, f32) {
	let mut l = [main; BLOCK];
	let mut r = [main; BLOCK];
	if let Some(k) = key {
		p.set_sidechain(&[k; BLOCK], &[-k; BLOCK]);
	}
	p.process(&mut l, &mut r, &[], &Transport::default());
	(l[BLOCK - 1], r[BLOCK - 1])
}

#[test]
fn scanner_reports_sidechain_inputs_and_extra_outputs() {
	let ducker = routing_descriptor(DUCKER_ID);
	assert_eq!(ducker.sidechain_channels, 2);
	assert!(ducker.aux_outputs.is_empty());
	assert!(
		ducker.description.contains("sidechain"),
		"{}",
		ducker.description
	);
	let multi = routing_descriptor(MULTI_ID);
	assert_eq!(multi.sidechain_channels, 0);
	assert_eq!(
		multi.aux_outputs,
		vec![
			AuxOutput {
				name: "Kick".into(),
				channels: 2
			},
			AuxOutput {
				name: "Snare".into(),
				channels: 1
			}
		]
	);
	assert!(
		multi.description.contains("Multi-output"),
		"{}",
		multi.description
	);
}

fn check_sidechain(sandboxed: bool) {
	let mut p = open(DUCKER_ID, sandboxed, None);
	assert!(p.has_sidechain());
	assert_eq!(duck(p.as_mut(), 0.25, Some(0.5)), (0.75, -0.25));
	// The key applies to one block only.
	assert_eq!(duck(p.as_mut(), 0.25, None), (0.25, 0.25));
	p.set_param(0, -60.0);
	let (l, _) = duck(p.as_mut(), 0.25, Some(0.5));
	assert!((l - 0.25).abs() < 1e-3, "{l}");
}

#[test]
fn sidechain_reaches_the_aux_input_in_process() {
	check_sidechain(false);
}

#[test]
fn sidechain_reaches_the_aux_input_sandboxed() {
	check_sidechain(true);
}

fn check_multi_output(sandboxed: bool) {
	let mut p = open(MULTI_ID, sandboxed, None);
	assert_eq!(p.aux_output_count(), 2);
	let mut block = |events: &[MidiEvent]| {
		let mut l = [1.0f32; BLOCK];
		let mut r = [1.0f32; BLOCK];
		p.process(&mut l, &mut r, events, &Transport::default());
		let kick = p
			.aux_output(0)
			.map(|(l, r)| (l.len(), l[BLOCK - 1], r[BLOCK - 1]));
		let snare = p.aux_output(1).map(|(l, r)| (l[BLOCK - 1], r[BLOCK - 1]));
		(l[BLOCK - 1], r[BLOCK - 1], kick, snare)
	};
	let on = |pitch| MidiEvent::NoteOn {
		offset: 0,
		pitch,
		velocity: 1.0,
	};
	assert_eq!(
		block(&[on(36)]),
		(0.0, 0.0, Some((BLOCK, 0.5, 0.5)), Some((0.0, 0.0)))
	);
	assert_eq!(
		block(&[on(38), on(60)]),
		(0.5, 0.5, Some((BLOCK, 0.5, 0.5)), Some((0.5, 0.5))),
		"mono Snare port is duplicated to both sides"
	);
	assert!(p.aux_output(2).is_none());
}

#[test]
fn extra_outputs_are_readable_in_process() {
	check_multi_output(false);
}

#[test]
fn extra_outputs_are_readable_sandboxed() {
	check_multi_output(true);
}

fn check_presets(sandboxed: bool) {
	let mut p = open(DUCKER_ID, sandboxed, None);
	let presets = p.presets();
	let names: Vec<&str> = presets.iter().map(|p| p.name.as_str()).collect();
	assert_eq!(names, ["Unity", "Quiet Key"]);
	assert_eq!(presets[0].category, "Factory");
	assert!(p.load_preset(&presets[1].key));
	assert!(!p.load_preset("not a key"));
	let (l, _) = duck(p.as_mut(), 0.0, Some(1.0));
	assert!((l - 0.1).abs() < 1e-4, "Quiet Key is -20 dB: {l}");

	// The loaded preset is part of the saved state, so a fresh instance
	// restored from it sounds the same.
	let state = p.save_state().expect("state");
	let mut q = open(DUCKER_ID, sandboxed, Some(&state));
	let (l, _) = duck(q.as_mut(), 0.0, Some(1.0));
	assert!((l - 0.1).abs() < 1e-4, "{l}");
}

#[test]
fn factory_presets_list_load_and_persist_in_process() {
	check_presets(false);
}

#[test]
fn factory_presets_list_load_and_persist_sandboxed() {
	check_presets(true);
}

#[test]
fn plugins_without_presets_list_none() {
	let mut p = open(MULTI_ID, false, None);
	assert!(p.presets().is_empty());
	assert!(!p.load_preset("plugin\u{1f}x"));
}
