//! Sidechain inputs, extra outputs and factory presets of CLAP plugins, in
//! process and sandboxed, against the `clap_routing` test plugins.

mod common;

use cadenza_dsp::{MidiEvent, Processor, Transport};
use cadenza_plugin_host::{
	AuxOutput, ProcessMode, SandboxOptions, instantiate, instantiate_sandboxed,
};
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
				// Block-by-block checks: no batching latency.
				SandboxOptions {
					realtime_chunk: 0,
					..sandbox_options()
				},
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

/// Engine-like quanta through a batched sandbox: the main output, the
/// sidechain key and the extra outputs all come out exactly as in-process,
/// delayed by the batching latency.
#[test]
fn sidechain_and_extra_outputs_survive_batching() {
	const Q: usize = 32;
	let batched = SandboxOptions {
		realtime_chunk: 128,
		..sandbox_options()
	};
	for id in [DUCKER_ID, MULTI_ID] {
		let desc = routing_descriptor(id);
		let mut local = instantiate(desc, SR, Q, None).unwrap();
		let mut remote =
			instantiate_sandboxed(desc, SR, Q, None, ProcessMode::Realtime, batched.clone())
				.unwrap();
		let delay = (remote.latency() - local.latency()) as usize;
		assert_eq!(delay, 96);
		let run = |p: &mut dyn Processor| {
			let mut out: Vec<Vec<f32>> = vec![Vec::new(); 6];
			for b in 0..120usize {
				let x = (b % 7) as f32 * 0.1 + 0.05;
				let mut l = [x; Q];
				let mut r = [-x; Q];
				if b % 3 != 0 {
					let k = (b % 5) as f32 * 0.2;
					p.set_sidechain(&[k; Q], &[-k; Q]);
				}
				let events = match b % 8 {
					0 => vec![MidiEvent::NoteOn {
						offset: 4,
						pitch: [36, 38, 60][b / 8 % 3],
						velocity: 1.0,
					}],
					5 => vec![MidiEvent::AllNotesOff { offset: 9 }],
					_ => Vec::new(),
				};
				p.process(&mut l, &mut r, &events, &Transport::default());
				out[0].extend_from_slice(&l);
				out[1].extend_from_slice(&r);
				for k in 0..p.aux_output_count() {
					let (al, ar) = p.aux_output(k).unwrap();
					assert_eq!(al.len(), Q);
					out[2 + 2 * k].extend_from_slice(al);
					out[3 + 2 * k].extend_from_slice(ar);
				}
			}
			out
		};
		let a = run(local.as_mut());
		let b = run(&mut remote);
		for (c, (a, b)) in a.iter().zip(&b).enumerate() {
			if a.is_empty() {
				continue;
			}
			assert!(a.iter().any(|&x| x != 0.0), "{id} channel {c} is silent");
			assert_eq!(&a[..a.len() - delay], &b[delay..], "{id} channel {c}");
		}
	}
}

#[test]
fn plugins_without_presets_list_none() {
	let mut p = open(MULTI_ID, false, None);
	assert!(p.presets().is_empty());
	assert!(!p.load_preset("plugin\u{1f}x"));
}
