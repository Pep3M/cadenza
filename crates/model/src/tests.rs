//! Cross-module tests: command application, undo/redo, validation and persistence.

use cadenza_core::{
	ClipId, DeviceId, MeterChange, NoteId, PPQ, ParamAddress, Tick, TimeSignature, TrackId,
};
use serde_json::json;
use uuid::Uuid;

use crate::controller::{Controller, ControllerPoint};
use crate::*;

fn beats(b: f64) -> Tick {
	Tick::from_beats(b)
}

struct Fixture {
	project: Project,
	lead: TrackId,
	drums: TrackId,
	audio: TrackId,
	reverb: TrackId,
	drum_bus: TrackId,
	synth: DeviceId,
	eq: DeviceId,
	comp: DeviceId,
	limiter: DeviceId,
	send: DeviceId,
	riff: ClipId,
	vox: ClipId,
	riff_notes: Vec<NoteId>,
}

/// A small but complete song: two buses (drum bus -> reverb), instrument/audio tracks,
/// inserts, sends, automation, markers and a tempo change.
fn fixture() -> Fixture {
	let mut project = Project::new("Fixture");
	project.tempo.set_point(beats(32.0), 90.0);

	let reverb = Track::new("Reverb", TrackKind::Bus);
	let mut drum_bus = Track::new("Drum Bus", TrackKind::Bus);
	drum_bus.channel.output = Output::Bus(reverb.id);
	drum_bus
		.channel
		.inserts
		.push(Device::builtin("compressor", "Bus Comp"));

	let synth = Device::builtin("subtractive", "Synth")
		.with_param("cutoff", 1200.0)
		.with_param("resonance", 0.3);
	let eq = Device::builtin("eq", "EQ").with_param("gain", -2.0);
	let comp = Device::builtin("compressor", "Comp");
	let notes = vec![
		Note::new(60, beats(0.0), beats(1.0), 0.8),
		Note::new(64, beats(1.0), beats(1.0), 0.7),
		Note::new(67, beats(2.0), beats(2.0), 0.9),
	];
	let riff_notes = notes.iter().map(|n| n.id).collect();
	let riff = Clip::midi("Riff", beats(0.0), beats(4.0)).with_notes(notes);
	let send = Send::new(reverb.id, -12.0);
	let mut lead = Track::new("Lead", TrackKind::Instrument)
		.with_instrument(synth.clone())
		.with_color("#FF8800");
	lead.channel.inserts = vec![eq.clone(), comp.clone()];
	lead.channel.sends.push(send.clone());
	lead.clips.push(riff.clone());
	lead.clips.push(
		Clip::midi("Riff 2", beats(8.0), beats(4.0)).with_notes([Note::new(
			72,
			Tick(0),
			Tick(PPQ),
			0.5,
		)]),
	);

	let mut drums =
		Track::new("Drums", TrackKind::Instrument).with_instrument(Device::builtin("drums", "Kit"));
	drums.channel.output = Output::Bus(drum_bus.id);
	drums.clips.push(
		Clip::midi("Beat", beats(0.0), beats(16.0)).with_notes([Note::new(
			36,
			Tick(0),
			Tick(PPQ),
			1.0,
		)]),
	);

	let vox_clip = Clip::audio("Vox", beats(4.0), beats(8.0), "vox.wav");
	let mut audio = Track::new("Vocals", TrackKind::Audio);
	audio.clips.push(vox_clip.clone());
	audio.channel.sends.push(Send::new(reverb.id, -6.0));

	let limiter = Device::builtin("limiter", "Limiter").with_param("ceiling", -0.3);
	project.master.inserts.push(limiter.clone());

	let fx = Fixture {
		lead: lead.id,
		drums: drums.id,
		audio: audio.id,
		reverb: reverb.id,
		drum_bus: drum_bus.id,
		synth: synth.id,
		eq: eq.id,
		comp: comp.id,
		limiter: limiter.id,
		send: send.id,
		riff: riff.id,
		vox: vox_clip.id,
		riff_notes,
		project: Project::new(""),
	};
	project.tracks = vec![lead, drums, audio, drum_bus, reverb];
	let lanes = [
		AutomationLane::new(
			ParamAddress::track_volume(fx.lead),
			vec![AutomationPoint::new(Tick(0), -6.0)],
		),
		AutomationLane::new(
			ParamAddress::device(Some(fx.lead), fx.synth, "cutoff"),
			vec![
				AutomationPoint::new(Tick(0), 400.0),
				AutomationPoint::new(beats(4.0), 4000.0),
			],
		),
		AutomationLane::new(
			ParamAddress::device(Some(fx.lead), fx.eq, "gain"),
			vec![AutomationPoint::new(Tick(0), 1.0)],
		),
		AutomationLane::new(
			ParamAddress::track_pan(fx.reverb),
			vec![AutomationPoint::new(Tick(0), 0.5)],
		),
		AutomationLane::new(
			ParamAddress::device(None, fx.limiter, "ceiling"),
			vec![AutomationPoint::new(Tick(0), -1.0)],
		),
	];
	for lane in lanes {
		let mut doc = Document::new(project);
		doc.execute(Command::SetAutomation {
			address: lane.address,
			points: lane.points,
		})
		.unwrap();
		project = doc.into_project();
	}
	project.markers = vec![
		Marker::new(Tick(0), "Intro"),
		Marker::new(beats(16.0), "Verse"),
	];
	project.validate().expect("fixture must be valid");
	Fixture { project, ..fx }
}

/// Executes `cmd`, then checks undo restores an equal project and redo re-applies it.
fn round_trip(project: &Project, cmd: Command) -> Project {
	let mut doc = Document::new(project.clone());
	let label = cmd.label();
	doc.execute(cmd.clone())
		.unwrap_or_else(|e| panic!("{} failed: {e}", cmd.op()));
	let after = doc.project().clone();
	assert_eq!(doc.history_labels(), vec![label.as_str()]);
	assert_eq!(doc.undo().unwrap(), label);
	assert_eq!(
		doc.project(),
		project,
		"undo of {} did not restore the project",
		cmd.op()
	);
	assert!(doc.can_redo() && !doc.can_undo());
	doc.redo().unwrap();
	assert_eq!(doc.project(), &after, "redo of {} differs", cmd.op());
	doc.undo().unwrap();
	assert_eq!(doc.project(), project);
	after
}

fn exec_err(project: &Project, cmd: Command) -> ModelError {
	let mut doc = Document::new(project.clone());
	let err = doc.execute(cmd).expect_err("command should fail");
	assert_eq!(
		doc.project(),
		project,
		"failed command must not change the project"
	);
	assert!(!doc.can_undo());
	err
}

#[test]
fn every_command_round_trips() {
	let fx = fixture();
	let p = &fx.project;
	let new_note = Note::new(62, beats(3.0), beats(0.5), 0.6);
	let commands = vec![
		Command::AddTrack {
			track: Track::new("Pad", TrackKind::Instrument),
			index: None,
		},
		Command::AddTrack {
			track: Track::new("Strings", TrackKind::Instrument)
				.with_instrument(Device::builtin("strings", "Strings")),
			index: Some(1),
		},
		Command::MoveTrack {
			track: fx.lead,
			index: 2,
		},
		Command::RemoveTrack { track: fx.lead },
		Command::RemoveTrack { track: fx.reverb },
		Command::RemoveTrack { track: fx.drum_bus },
		Command::UpdateTrack {
			track: fx.lead,
			patch: TrackPatch {
				name: Some("Lead 2".into()),
				color: Some(None),
				input: None,
			},
		},
		Command::UpdateTrack {
			track: fx.drums,
			patch: TrackPatch {
				name: None,
				color: Some(Some("#00FF00".into())),
				input: None,
			},
		},
		Command::SetChannel {
			track: Some(fx.lead),
			patch: ChannelPatch {
				volume_db: Some(-3.0),
				pan: Some(-0.5),
				mute: Some(true),
				solo: Some(true),
				output: Some(Output::Bus(fx.reverb)),
			},
		},
		Command::SetChannel {
			track: Some(fx.drums),
			patch: ChannelPatch {
				output: Some(Output::Master),
				..Default::default()
			},
		},
		Command::SetChannel {
			track: None,
			patch: ChannelPatch {
				volume_db: Some(-1.5),
				..Default::default()
			},
		},
		Command::SetInstrument {
			track: fx.lead,
			device: Some(Device::builtin("fm", "FM")),
		},
		Command::SetInstrument {
			track: fx.lead,
			device: None,
		},
		Command::SetInstrument {
			track: fx.lead,
			device: Some(Device {
				name: "Renamed".into(),
				..p.device(Some(fx.lead), fx.synth).unwrap().clone()
			}),
		},
		Command::InsertDevice {
			track: Some(fx.lead),
			device: Device::builtin("delay", "Delay"),
			index: Some(0),
		},
		Command::InsertDevice {
			track: None,
			device: Device::builtin("eq", "Master EQ"),
			index: None,
		},
		Command::RemoveDevice {
			track: Some(fx.lead),
			device: fx.eq,
		},
		Command::RemoveDevice {
			track: None,
			device: fx.limiter,
		},
		Command::MoveDevice {
			track: Some(fx.lead),
			device: fx.eq,
			index: 1,
		},
		Command::MoveDevice {
			track: Some(fx.lead),
			device: fx.comp,
			index: 0,
		},
		Command::SetDeviceBypass {
			track: Some(fx.lead),
			device: fx.synth,
			bypass: true,
		},
		Command::SetDeviceBypass {
			track: None,
			device: fx.limiter,
			bypass: true,
		},
		Command::SetParam {
			address: ParamAddress::track_volume(fx.lead),
			value: -10.0,
		},
		Command::SetParam {
			address: ParamAddress::track_pan(fx.lead),
			value: 0.25,
		},
		Command::SetParam {
			address: "master/volume".parse().unwrap(),
			value: -2.0,
		},
		Command::SetParam {
			address: ParamAddress::device(Some(fx.lead), fx.synth, "cutoff"),
			value: 900.0,
		},
		Command::SetParam {
			address: ParamAddress::device(Some(fx.lead), fx.comp, "ratio"),
			value: 4.0,
		},
		Command::SetParam {
			address: ParamAddress::device(None, fx.limiter, "ceiling"),
			value: -1.0,
		},
		Command::ResetParam {
			address: ParamAddress::device(Some(fx.lead), fx.synth, "cutoff"),
		},
		Command::AddSend {
			track: fx.drums,
			send: Send::new(fx.reverb, -18.0),
			index: None,
		},
		Command::AddSend {
			track: fx.lead,
			send: Send::new(fx.drum_bus, -20.0),
			index: Some(0),
		},
		Command::RemoveSend {
			track: fx.lead,
			send: fx.send,
		},
		Command::UpdateSend {
			track: fx.lead,
			send: fx.send,
			patch: SendPatch {
				level_db: Some(-3.0),
				pre_fader: Some(true),
			},
		},
		Command::AddClip {
			track: fx.lead,
			clip: Clip::midi("Late", beats(32.0), beats(4.0)),
		},
		Command::AddClip {
			track: fx.audio,
			clip: Clip::audio("Gtr", beats(0.0), beats(2.0), "gtr.wav"),
		},
		Command::RemoveClip { clip: fx.riff },
		Command::RemoveClip { clip: fx.vox },
		Command::UpdateClip {
			clip: fx.riff,
			patch: ClipPatch {
				name: Some("Moved".into()),
				start: Some(beats(12.0)),
				length: Some(beats(8.0)),
				track: None,
				take: None,
				muted: None,
				link: None,
			},
		},
		Command::UpdateClip {
			clip: fx.riff,
			patch: ClipPatch {
				track: Some(fx.drums),
				..Default::default()
			},
		},
		Command::AddNotes {
			clip: fx.riff,
			notes: vec![new_note.clone(), Note::new(48, beats(0.0), beats(8.0), 0.4)],
		},
		Command::RemoveNotes {
			clip: fx.riff,
			notes: vec![fx.riff_notes[2], fx.riff_notes[0]],
		},
		Command::UpdateNotes {
			clip: fx.riff,
			notes: vec![
				NotePatch {
					id: fx.riff_notes[0],
					pitch: Some(72),
					velocity: None,
					start: Some(beats(3.5)),
					length: None,
				},
				NotePatch {
					id: fx.riff_notes[1],
					pitch: None,
					velocity: Some(0.1),
					start: None,
					length: Some(beats(0.25)),
				},
			],
		},
		Command::SetTempo {
			tick: Tick::ZERO,
			bpm: 140.0,
		},
		Command::SetTempo {
			tick: beats(16.0),
			bpm: 100.0,
		},
		Command::RemoveTempoPoint { tick: beats(32.0) },
		Command::SetTimeSignature {
			time_signature: TimeSignature {
				numerator: 7,
				denominator: 8,
			},
			bar: 0,
		},
		Command::SetAutomation {
			address: ParamAddress::track_pan(fx.lead),
			points: vec![
				AutomationPoint::new(beats(4.0), 1.0),
				AutomationPoint::new(Tick(0), -1.0),
			],
		},
		Command::SetAutomation {
			address: ParamAddress::track_volume(fx.lead),
			points: vec![],
		},
		Command::SetAutomation {
			address: ParamAddress::device(Some(fx.lead), fx.synth, "cutoff"),
			points: vec![AutomationPoint::new(Tick(0), 50.0)],
		},
		Command::AddMarker {
			marker: Marker::new(beats(8.0), "Pre-Chorus"),
		},
		Command::RemoveMarker { tick: beats(16.0) },
		Command::Batch {
			label: "Arrange chorus".into(),
			commands: vec![
				Command::AddMarker {
					marker: Marker::new(beats(32.0), "Chorus"),
				},
				Command::SetParam {
					address: ParamAddress::track_volume(fx.drums),
					value: 2.0,
				},
				Command::RemoveTrack { track: fx.reverb },
				Command::Batch {
					label: "nested".into(),
					commands: vec![Command::SetTempo {
						tick: beats(32.0),
						bpm: 128.0,
					}],
				},
			],
		},
	];
	for cmd in commands {
		let after = round_trip(p, cmd.clone());
		assert_ne!(&after, p, "{} had no effect", cmd.op());
		after
			.validate()
			.unwrap_or_else(|e| panic!("{} left an invalid project: {e}", cmd.op()));
	}
}

#[test]
fn move_track_reorders_and_validates() {
	let fx = fixture();
	let mut doc = Document::new(fx.project.clone());
	let last = fx.project.tracks.len() - 1;
	doc.execute(Command::MoveTrack {
		track: fx.lead,
		index: last,
	})
	.unwrap();
	assert_eq!(doc.project().tracks[last].id, fx.lead);
	assert_eq!(doc.project().tracks.len(), fx.project.tracks.len());
	let err = exec_err(
		&fx.project,
		Command::MoveTrack {
			track: fx.lead,
			index: last + 1,
		},
	);
	assert!(err.to_string().contains("out of range"), "{err}");
}

#[test]
fn remove_bus_cascades_and_undo_restores() {
	let fx = fixture();
	let mut doc = Document::new(fx.project.clone());
	doc.execute(Command::RemoveTrack { track: fx.reverb })
		.unwrap();
	let p = doc.project();
	assert!(p.track(fx.reverb).is_none());
	assert!(
		p.tracks
			.iter()
			.all(|t| t.channel.sends.iter().all(|s| s.target != fx.reverb))
	);
	assert_eq!(p.track(fx.drum_bus).unwrap().channel.output, Output::Master);
	assert!(
		p.automation
			.iter()
			.all(|l| l.address.track != Some(fx.reverb))
	);
	assert_eq!(p.automation.len(), fx.project.automation.len() - 1);
	doc.undo().unwrap();
	assert_eq!(doc.project(), &fx.project);
}

#[test]
fn removing_devices_drops_their_automation() {
	let fx = fixture();
	let after = round_trip(
		&fx.project,
		Command::RemoveDevice {
			track: Some(fx.lead),
			device: fx.eq,
		},
	);
	assert!(
		after
			.automation_lane(&ParamAddress::device(Some(fx.lead), fx.eq, "gain"))
			.is_none()
	);

	let after = round_trip(
		&fx.project,
		Command::SetInstrument {
			track: fx.lead,
			device: None,
		},
	);
	assert!(
		after
			.automation_lane(&ParamAddress::device(Some(fx.lead), fx.synth, "cutoff"))
			.is_none()
	);

	// Replacing the instrument with the same id keeps its lanes.
	let same = Device {
		name: "Same".into(),
		..fx.project.device(Some(fx.lead), fx.synth).unwrap().clone()
	};
	let after = round_trip(
		&fx.project,
		Command::SetInstrument {
			track: fx.lead,
			device: Some(same),
		},
	);
	assert!(
		after
			.automation_lane(&ParamAddress::device(Some(fx.lead), fx.synth, "cutoff"))
			.is_some()
	);
}

#[test]
fn batch_is_atomic() {
	let fx = fixture();
	let cmd = Command::Batch {
		label: "Broken".into(),
		commands: vec![
			Command::SetTempo {
				tick: Tick::ZERO,
				bpm: 100.0,
			},
			Command::RemoveTrack { track: fx.lead },
			Command::AddNotes {
				clip: fx.riff,
				notes: vec![Note::new(60, Tick(0), Tick(PPQ), 0.5)],
			},
		],
	};
	let err = exec_err(&fx.project, cmd);
	match &err {
		ModelError::BatchFailed {
			index, op, source, ..
		} => {
			assert_eq!(*index, 2);
			assert_eq!(op, "add_notes");
			assert_eq!(**source, ModelError::ClipNotFound(fx.riff));
		}
		other => panic!("unexpected error {other}"),
	}
	assert!(err.to_string().contains("no changes were applied"));
	assert!(matches!(
		exec_err(
			&fx.project,
			Command::Batch {
				label: "x".into(),
				commands: vec![]
			}
		),
		ModelError::EmptyBatch { .. }
	));
}

#[test]
fn batch_undoes_as_one_step() {
	let fx = fixture();
	let mut doc = Document::new(fx.project.clone());
	doc.execute(Command::Batch {
		label: "Intro mix".into(),
		commands: vec![
			Command::SetParam {
				address: ParamAddress::track_volume(fx.lead),
				value: -4.0,
			},
			Command::SetParam {
				address: ParamAddress::track_volume(fx.lead),
				value: -8.0,
			},
			Command::AddMarker {
				marker: Marker::new(beats(4.0), "Hook"),
			},
		],
	})
	.unwrap();
	assert_eq!(doc.history_labels(), vec!["Intro mix"]);
	assert_eq!(
		doc.project().track(fx.lead).unwrap().channel.volume_db,
		-8.0
	);
	doc.undo().unwrap();
	assert_eq!(doc.project(), &fx.project);
}

#[test]
fn routing_cycles_are_rejected() {
	let fx = fixture();
	let p = &fx.project;
	let set_output = |track, bus| Command::SetChannel {
		track: Some(track),
		patch: ChannelPatch {
			output: Some(Output::Bus(bus)),
			..Default::default()
		},
	};
	// drum bus -> reverb already exists, so reverb -> drum bus closes a cycle.
	assert_eq!(
		exec_err(p, set_output(fx.reverb, fx.drum_bus)),
		ModelError::RoutingCycle {
			from: fx.reverb,
			to: fx.drum_bus
		}
	);
	assert!(matches!(
		exec_err(
			p,
			Command::AddSend {
				track: fx.reverb,
				send: Send::new(fx.drum_bus, 0.0),
				index: None
			}
		),
		ModelError::RoutingCycle { .. }
	));
	assert!(matches!(
		exec_err(p, set_output(fx.reverb, fx.reverb)),
		ModelError::RoutingCycle { .. }
	));
	assert_eq!(
		exec_err(p, set_output(fx.lead, fx.drums)),
		ModelError::NotABus {
			track: fx.drums,
			kind: TrackKind::Instrument
		}
	);
	let ghost = TrackId::new();
	assert_eq!(
		exec_err(p, set_output(fx.lead, ghost)),
		ModelError::TrackNotFound(ghost)
	);
	assert!(matches!(
		exec_err(
			p,
			Command::SetChannel {
				track: None,
				patch: ChannelPatch {
					output: Some(Output::Bus(fx.reverb)),
					..Default::default()
				}
			}
		),
		ModelError::MasterRestriction(_)
	));

	// A longer chain: new bus C feeds the drum bus; reverb -> C would close drum bus -> reverb -> C -> drum bus.
	let c = Track::new("C", TrackKind::Bus);
	let c_id = c.id;
	let mut doc = Document::new(p.clone());
	doc.execute(Command::AddTrack {
		track: c,
		index: None,
	})
	.unwrap();
	doc.execute(set_output(c_id, fx.drum_bus)).unwrap();
	assert!(matches!(
		doc.execute(set_output(fx.reverb, c_id)),
		Err(ModelError::RoutingCycle { .. })
	));
	// Non-cyclic re-routing is fine.
	doc.execute(set_output(fx.lead, c_id)).unwrap();
}

#[test]
fn validation_errors() {
	let fx = fixture();
	let p = &fx.project;
	let add_note = |note: Note| Command::AddNotes {
		clip: fx.riff,
		notes: vec![note],
	};

	assert!(matches!(
		exec_err(p, add_note(Note::new(128, Tick(0), Tick(PPQ), 0.5))),
		ModelError::InvalidPitch(128)
	));
	assert!(matches!(
		exec_err(p, add_note(Note::new(60, Tick(0), Tick(PPQ), 1.5))),
		ModelError::InvalidVelocity(_)
	));
	assert!(matches!(
		exec_err(p, add_note(Note::new(60, Tick(0), Tick(0), 0.5))),
		ModelError::InvalidLength { .. }
	));
	assert!(matches!(
		exec_err(p, add_note(Note::new(60, Tick(-1), Tick(PPQ), 0.5))),
		ModelError::NegativeStart { .. }
	));
	assert!(matches!(
		exec_err(p, add_note(Note::new(60, beats(4.0), Tick(PPQ), 0.5))),
		ModelError::NoteOutsideClip { .. }
	));
	// Notes may extend past the clip end.
	round_trip(p, add_note(Note::new(60, beats(3.5), beats(4.0), 0.5)));
	let dup = Note {
		id: fx.riff_notes[0],
		..Note::new(50, Tick(0), Tick(PPQ), 0.5)
	};
	assert!(matches!(
		exec_err(p, add_note(dup)),
		ModelError::DuplicateNoteId { .. }
	));
	assert!(matches!(
		exec_err(
			p,
			Command::AddNotes {
				clip: fx.vox,
				notes: vec![Note::new(60, Tick(0), Tick(PPQ), 0.5)]
			}
		),
		ModelError::NotAMidiClip(_)
	));

	assert!(matches!(
		exec_err(
			p,
			Command::AddClip {
				track: fx.lead,
				clip: Clip::midi("Neg", Tick(-PPQ), Tick(PPQ))
			}
		),
		ModelError::NegativeStart { .. }
	));
	assert!(matches!(
		exec_err(
			p,
			Command::AddClip {
				track: fx.audio,
				clip: Clip::midi("M", Tick(0), Tick(PPQ))
			}
		),
		ModelError::ClipKindMismatch { .. }
	));
	assert!(matches!(
		exec_err(
			p,
			Command::AddClip {
				track: fx.reverb,
				clip: Clip::audio("A", Tick(0), Tick(PPQ), "a.wav")
			}
		),
		ModelError::ClipKindMismatch { .. }
	));
	let dup_clip = Clip {
		id: fx.vox,
		..Clip::midi("Dup", Tick(0), Tick(PPQ))
	};
	assert_eq!(
		exec_err(
			p,
			Command::AddClip {
				track: fx.lead,
				clip: dup_clip
			}
		),
		ModelError::DuplicateClipId(fx.vox)
	);

	assert!(matches!(
		exec_err(
			p,
			Command::SetInstrument {
				track: fx.reverb,
				device: Some(Device::builtin("fm", "FM"))
			}
		),
		ModelError::InstrumentNotAllowed { .. }
	));
	let bad_bus = Track::new("Bus", TrackKind::Bus).with_instrument(Device::builtin("fm", "FM"));
	assert!(matches!(
		exec_err(
			p,
			Command::AddTrack {
				track: bad_bus,
				index: None
			}
		),
		ModelError::InstrumentNotAllowed { .. }
	));
	let dup_track = Track {
		id: fx.lead,
		..Track::new("Dup", TrackKind::Audio)
	};
	assert_eq!(
		exec_err(
			p,
			Command::AddTrack {
				track: dup_track,
				index: None
			}
		),
		ModelError::DuplicateTrackId(fx.lead)
	);
	let dup_device = Device {
		id: fx.limiter,
		..Device::builtin("eq", "EQ")
	};
	assert_eq!(
		exec_err(
			p,
			Command::InsertDevice {
				track: Some(fx.lead),
				device: dup_device,
				index: None
			}
		),
		ModelError::DuplicateDeviceId(fx.limiter)
	);
	assert!(matches!(
		exec_err(
			p,
			Command::AddTrack {
				track: Track::new("T", TrackKind::Audio),
				index: Some(99)
			}
		),
		ModelError::InvalidIndex { .. }
	));

	let vol = |v| Command::SetParam {
		address: ParamAddress::track_volume(fx.lead),
		value: v,
	};
	assert!(matches!(
		exec_err(p, vol(12.5)),
		ModelError::InvalidVolume(_)
	));
	assert!(matches!(
		exec_err(p, vol(-200.0)),
		ModelError::InvalidVolume(_)
	));
	assert!(matches!(
		exec_err(
			p,
			Command::SetParam {
				address: ParamAddress::track_pan(fx.lead),
				value: 1.5
			}
		),
		ModelError::InvalidPan(_)
	));
	assert!(matches!(
		exec_err(
			p,
			Command::SetParam {
				address: ParamAddress::device(Some(fx.lead), fx.limiter, "x"),
				value: 1.0
			}
		),
		ModelError::DeviceNotFound { .. }
	));
	assert!(matches!(
		exec_err(
			p,
			Command::SetParam {
				address: ParamAddress::device(Some(fx.lead), fx.synth, "x"),
				value: f32::NAN
			}
		),
		ModelError::InvalidValue { .. }
	));

	assert!(matches!(
		exec_err(
			p,
			Command::SetTempo {
				tick: Tick(0),
				bpm: 10.0
			}
		),
		ModelError::InvalidTempo(_)
	));
	assert!(matches!(
		exec_err(
			p,
			Command::SetTempo {
				tick: Tick(0),
				bpm: 1000.0
			}
		),
		ModelError::InvalidTempo(_)
	));
	assert_eq!(
		exec_err(p, Command::RemoveTempoPoint { tick: Tick(0) }),
		ModelError::CannotRemoveFirstTempoPoint
	);
	assert!(matches!(
		exec_err(p, Command::RemoveTempoPoint { tick: Tick(5) }),
		ModelError::TempoPointNotFound(_)
	));
	let ts = |numerator, denominator| Command::SetTimeSignature {
		time_signature: TimeSignature {
			numerator,
			denominator,
		},
		bar: 0,
	};
	assert!(matches!(
		exec_err(p, ts(3, 6)),
		ModelError::InvalidTimeSignature { .. }
	));
	assert!(matches!(
		exec_err(p, ts(0, 4)),
		ModelError::InvalidTimeSignature { .. }
	));
	assert!(matches!(
		exec_err(p, ts(33, 4)),
		ModelError::InvalidTimeSignature { .. }
	));

	assert!(matches!(
		exec_err(
			p,
			Command::UpdateClip {
				clip: fx.riff,
				patch: ClipPatch {
					length: Some(beats(2.0)),
					..Default::default()
				}
			}
		),
		ModelError::NotesWouldStartOutsideClip { count: 1, .. }
	));
	assert!(matches!(
		exec_err(
			p,
			Command::UpdateClip {
				clip: fx.riff,
				patch: ClipPatch {
					track: Some(fx.audio),
					..Default::default()
				}
			}
		),
		ModelError::ClipKindMismatch { .. }
	));
	assert!(matches!(
		exec_err(
			p,
			Command::UpdateTrack {
				track: fx.lead,
				patch: TrackPatch {
					name: None,
					color: Some(Some("orange".into())),
					input: None,
				}
			}
		),
		ModelError::InvalidValue { .. }
	));
	assert!(matches!(
		exec_err(
			p,
			Command::UpdateTrack {
				track: fx.lead,
				patch: TrackPatch::default()
			}
		),
		ModelError::InvalidValue { .. }
	));
	assert_eq!(
		exec_err(
			p,
			Command::AddMarker {
				marker: Marker::new(beats(16.0), "Again")
			}
		),
		ModelError::DuplicateMarker(beats(16.0))
	);
	assert_eq!(
		exec_err(
			p,
			Command::RemoveDevice {
				track: Some(fx.lead),
				device: fx.synth
			}
		),
		ModelError::DeviceIsInstrument(fx.synth)
	);
	assert!(matches!(
		exec_err(
			p,
			Command::MoveDevice {
				track: Some(fx.lead),
				device: fx.eq,
				index: 2
			}
		),
		ModelError::InvalidDeviceIndex { len: 2, .. }
	));
	assert!(matches!(
		exec_err(
			p,
			Command::SetAutomation {
				address: ParamAddress::track_volume(fx.lead),
				points: vec![
					AutomationPoint::new(Tick(0), 0.0),
					AutomationPoint::new(Tick(0), 1.0)
				],
			}
		),
		ModelError::InvalidAutomation { .. }
	));
	assert!(matches!(
		exec_err(
			p,
			Command::SetAutomation {
				address: ParamAddress::track_pan(fx.lead),
				points: vec![AutomationPoint::new(Tick(0), 3.0)],
			}
		),
		ModelError::InvalidPan(_)
	));
}

#[test]
fn errors_are_actionable() {
	let id = TrackId(Uuid::from_u128(7));
	let msg = ModelError::TrackNotFound(id).to_string();
	assert!(msg.contains(&id.to_string()));
	assert!(
		ModelError::InvalidPitch(200)
			.to_string()
			.contains("0..=127")
	);
	assert!(
		ModelError::InvalidVolume(20.0)
			.to_string()
			.contains("-144.0..=12.0")
	);
	assert!(
		ModelError::InvalidTimeSignature {
			numerator: 3,
			denominator: 6
		}
		.to_string()
		.contains("power of two")
	);
}

#[test]
fn notes_and_clips_stay_sorted() {
	let fx = fixture();
	let mut doc = Document::new(fx.project.clone());
	doc.execute(Command::AddNotes {
		clip: fx.riff,
		notes: vec![
			Note::new(50, beats(0.0), beats(1.0), 0.5),
			Note::new(40, beats(3.0), beats(1.0), 0.5),
		],
	})
	.unwrap();
	doc.execute(Command::UpdateNotes {
		clip: fx.riff,
		notes: vec![NotePatch {
			id: fx.riff_notes[2],
			pitch: None,
			velocity: None,
			start: Some(beats(0.5)),
			length: None,
		}],
	})
	.unwrap();
	let (_, clip) = doc.project().find_clip(fx.riff).unwrap();
	let keys: Vec<_> = clip.notes().iter().map(|n| (n.start, n.pitch)).collect();
	let mut sorted = keys.clone();
	sorted.sort();
	assert_eq!(keys, sorted);
	assert_eq!(keys[0], (Tick(0), 50));

	doc.execute(Command::UpdateClip {
		clip: fx.riff,
		patch: ClipPatch {
			start: Some(beats(20.0)),
			..Default::default()
		},
	})
	.unwrap();
	let lead = doc.project().track(fx.lead).unwrap();
	assert!(lead.clips.windows(2).all(|w| w[0].start <= w[1].start));
	assert_eq!(lead.clips.last().unwrap().id, fx.riff);
}

#[test]
fn execute_clears_redo_and_tracks_history() {
	let fx = fixture();
	let mut doc = Document::new(fx.project.clone());
	assert_eq!(doc.undo(), Err(ModelError::NothingToUndo));
	assert_eq!(doc.redo(), Err(ModelError::NothingToRedo));
	doc.execute(Command::SetTempo {
		tick: Tick::ZERO,
		bpm: 100.0,
	})
	.unwrap();
	doc.execute(Command::AddMarker {
		marker: Marker::new(beats(4.0), "Hook"),
	})
	.unwrap();
	assert_eq!(
		doc.history_labels(),
		vec!["Set tempo to 100 bpm", "Add marker 'Hook'"]
	);
	doc.undo().unwrap();
	assert!(doc.can_redo());
	assert_eq!(doc.redo_labels(), vec!["Add marker 'Hook'"]);
	doc.execute(Command::SetTimeSignature {
		time_signature: TimeSignature {
			numerator: 3,
			denominator: 4,
		},
		bar: 0,
	})
	.unwrap();
	assert!(!doc.can_redo());
	doc.undo().unwrap();
	doc.undo().unwrap();
	assert_eq!(doc.project(), &fx.project);
}

#[test]
fn json_round_trip() {
	let fx = fixture();
	let json = fx.project.to_json_string().unwrap();
	assert!(
		json.contains("\n\t") || json.contains("\n  "),
		"pretty printed"
	);
	assert_eq!(Project::from_json_str(&json).unwrap(), fx.project);

	let dir = std::env::temp_dir().join(format!("cadenza-model-test-{}", Uuid::new_v4()));
	std::fs::create_dir_all(&dir).unwrap();
	let path = dir.join("song.cadenza.json");
	fx.project.save_json(&path).unwrap();
	// Relative file references resolve against the project's folder on load.
	let mut expected = fx.project.clone();
	expected.map_file_paths(|p| crate::files::absolutize(p, &dir));
	assert_eq!(Project::load_json(&path).unwrap(), expected);
	std::fs::remove_dir_all(&dir).unwrap();

	assert!(matches!(
		Project::load_json(dir.join("missing.json")),
		Err(ModelError::Io { .. })
	));
}

#[test]
fn json_version_checks() {
	let mut value = serde_json::to_value(&fixture().project).unwrap();
	value["format_version"] = json!(99);
	let err = Project::from_json_str(&value.to_string()).unwrap_err();
	assert_eq!(
		err,
		ModelError::UnsupportedFormatVersion {
			found: 99,
			supported: FORMAT_VERSION
		}
	);
	assert!(err.to_string().contains("99"));

	value.as_object_mut().unwrap().remove("format_version");
	assert!(matches!(
		Project::from_json_str(&value.to_string()),
		Err(ModelError::Json(_))
	));
	assert!(matches!(
		Project::from_json_str("not json"),
		Err(ModelError::Json(_))
	));
}

#[test]
fn loading_rejects_invalid_projects() {
	let fx = fixture();
	let mut bad = fx.project.clone();
	bad.tracks[3].channel.output = Output::Bus(fx.drum_bus);
	assert!(matches!(
		Project::from_json_str(&bad.to_json_string().unwrap()),
		Err(ModelError::RoutingCycle { .. })
	));

	let mut bad = fx.project.clone();
	let lead = bad.tracks[0].clone();
	bad.tracks.push(Track {
		id: TrackId::new(),
		..lead
	});
	assert!(matches!(
		Project::from_json_str(&bad.to_json_string().unwrap()),
		Err(ModelError::DuplicateDeviceId(_))
	));

	// Out-of-order content is normalized rather than rejected.
	let mut shuffled = fx.project.clone();
	shuffled.markers.reverse();
	shuffled.automation.reverse();
	assert_eq!(
		Project::from_json_str(&shuffled.to_json_string().unwrap()).unwrap(),
		fx.project
	);
}

#[test]
fn command_json_shape_is_stable() {
	let track = TrackId(Uuid::from_u128(1));
	let device = DeviceId(Uuid::from_u128(2));
	let clip = ClipId(Uuid::from_u128(3));
	let note = NoteId(Uuid::from_u128(4));
	let t = |id: u128| Uuid::from_u128(id).to_string();

	let cases = [
		(
			Command::SetChannel {
				track: Some(track),
				patch: ChannelPatch {
					volume_db: Some(-6.0),
					..Default::default()
				},
			},
			json!({"op": "set_channel", "track": t(1), "volume_db": -6.0}),
		),
		(
			Command::SetChannel {
				track: None,
				patch: ChannelPatch {
					output: None,
					mute: Some(true),
					..Default::default()
				},
			},
			json!({"op": "set_channel", "track": null, "mute": true}),
		),
		(
			Command::SetChannel {
				track: Some(track),
				patch: ChannelPatch {
					output: Some(Output::Bus(track)),
					..Default::default()
				},
			},
			json!({"op": "set_channel", "track": t(1), "output": {"bus": t(1)}}),
		),
		(
			Command::SetParam {
				address: ParamAddress::device(Some(track), device, "cutoff"),
				value: 800.0,
			},
			json!({"op": "set_param", "address": format!("track/{}/device/{}/cutoff", t(1), t(2)), "value": 800.0}),
		),
		(
			Command::AddNotes {
				clip,
				notes: vec![Note {
					id: note,
					pitch: 60,
					velocity: 0.5,
					start: Tick(0),
					length: Tick(PPQ),
				}],
			},
			json!({"op": "add_notes", "clip": t(3), "notes": [
				{"id": t(4), "pitch": 60, "velocity": 0.5, "start": 0, "length": 960}
			]}),
		),
		(
			Command::UpdateTrack {
				track,
				patch: TrackPatch {
					name: Some("Bass".into()),
					color: Some(None),
					input: None,
				},
			},
			json!({"op": "update_track", "track": t(1), "name": "Bass", "color": null}),
		),
		(
			Command::InsertDevice {
				track: None,
				device: Device {
					id: device,
					kind: DeviceKind::Builtin { id: "eq".into() },
					name: "EQ".into(),
					bypass: false,
					params: [("gain".to_owned(), 1.5)].into(),
					state: None,
					sidechain: None,
					aux_outputs: Vec::new(),
					zones: Vec::new(),
					modulators: Vec::new(),
				},
				index: None,
			},
			json!({"op": "insert_device", "track": null, "device": {
				"id": t(2), "kind": {"type": "builtin", "id": "eq"}, "name": "EQ", "bypass": false, "params": {"gain": 1.5}
			}}),
		),
		(
			Command::Batch {
				label: "Tempo".into(),
				commands: vec![Command::SetTempo {
					tick: Tick(0),
					bpm: 90.0,
				}],
			},
			json!({"op": "batch", "label": "Tempo", "commands": [{"op": "set_tempo", "tick": 0, "bpm": 90.0}]}),
		),
	];
	for (cmd, expected) in cases {
		assert_eq!(serde_json::to_value(&cmd).unwrap(), expected);
		let back: Command = serde_json::from_value(expected).unwrap();
		assert_eq!(back, cmd);
	}
}

#[test]
fn command_json_from_llm_style_input() {
	let track = Uuid::from_u128(1).to_string();
	let cmd: Command =
		serde_json::from_value(json!({"op": "update_track", "track": track, "name": "Keys"}))
			.unwrap();
	assert!(matches!(
		cmd,
		Command::UpdateTrack {
			patch: TrackPatch { color: None, .. },
			..
		}
	));

	let cmd: Command = serde_json::from_value(json!({"op": "set_channel", "pan": -0.3})).unwrap();
	assert_eq!(
		cmd,
		Command::SetChannel {
			track: None,
			patch: ChannelPatch {
				pan: Some(-0.3),
				..Default::default()
			}
		}
	);

	let cmd: Command = serde_json::from_value(json!({
		"op": "add_track",
		"track": {"id": track, "name": "Bass", "kind": "instrument",
			"instrument": {"id": Uuid::from_u128(2).to_string(), "kind": {"type": "builtin", "id": "subtractive"}, "name": "Bass Synth"}}
	}))
	.unwrap();
	let Command::AddTrack { track, index: None } = &cmd else {
		panic!("wrong variant")
	};
	assert_eq!(track.channel, Channel::default());
	let mut doc = Document::new(Project::new("x"));
	doc.execute(cmd).unwrap();

	assert!(serde_json::from_value::<Command>(json!({"op": "explode"})).is_err());
}

#[test]
fn command_schema_generates() {
	let schema = schemars::schema_for!(Command);
	let text = serde_json::to_string(&schema).unwrap();
	assert!(text.contains("set_param"));
	let schema = schemars::schema_for!(ProjectSummary);
	assert!(
		serde_json::to_string(&schema)
			.unwrap()
			.contains("pitch_range")
	);
}

/// Adds an instrument track "Pad" (poly synth + compressor insert, output to master).
fn with_pad(project: &Project) -> (Project, TrackId, DeviceId, DeviceId) {
	let synth = Device::builtin("poly_synth", "Pad Synth");
	let comp = Device::builtin("compressor", "Pad Comp");
	let mut pad = Track::new("Pad", TrackKind::Instrument).with_instrument(synth.clone());
	pad.channel.inserts.push(comp.clone());
	let id = pad.id;
	let mut doc = Document::new(project.clone());
	doc.execute(Command::AddTrack {
		track: pad,
		index: None,
	})
	.unwrap();
	(doc.into_project(), id, synth.id, comp.id)
}

fn key(track: Option<TrackId>, device: DeviceId, source: TrackId, pre_fader: bool) -> Command {
	Command::SetSidechain {
		track,
		device,
		source: Some(SidechainSource {
			track: source,
			pre_fader,
		}),
	}
}

#[test]
fn sidechains_round_trip_and_reject_feedback() {
	let fx = fixture();
	let (p, pad, _, pad_comp) = with_pad(&fx.project);

	let keyed = round_trip(&p, key(Some(pad), pad_comp, fx.drums, true));
	let source = keyed
		.device(Some(pad), pad_comp)
		.unwrap()
		.sidechain
		.unwrap();
	assert_eq!((source.track, source.pre_fader), (fx.drums, true));
	round_trip(
		&keyed,
		Command::SetSidechain {
			track: Some(pad),
			device: pad_comp,
			source: None,
		},
	);
	// Master inserts may be keyed by any track; buses by tracks that do not depend on them.
	round_trip(&p, key(None, fx.limiter, fx.reverb, false));
	let bus_comp = p.tracks[3].channel.inserts[0].id;
	round_trip(&p, key(Some(fx.drum_bus), bus_comp, fx.lead, false));

	assert!(matches!(
		exec_err(&p, key(Some(pad), pad_comp, pad, false)),
		ModelError::InvalidSidechain { .. }
	));
	// The lead sends to the reverb bus, so the reverb cannot key the lead.
	let err = exec_err(&p, key(Some(fx.lead), fx.comp, fx.reverb, false));
	assert!(matches!(err, ModelError::InvalidSidechain { .. }), "{err}");
	assert!(err.to_string().contains("feedback loop"), "{err}");
	// drum bus -> reverb: the drum bus compressor cannot be keyed by the reverb.
	assert!(matches!(
		exec_err(&p, key(Some(fx.drum_bus), bus_comp, fx.reverb, false)),
		ModelError::InvalidSidechain { .. }
	));
	let ghost = TrackId::new();
	assert_eq!(
		exec_err(&p, key(Some(pad), pad_comp, ghost, false)),
		ModelError::TrackNotFound(ghost)
	);
	assert!(matches!(
		exec_err(&p, key(Some(pad), DeviceId::new(), fx.drums, false)),
		ModelError::DeviceNotFound { .. }
	));

	// Once the pad is keyed by the reverb, routing the pad into the reverb (directly or
	// through the drum bus) would close a loop.
	let mut doc = Document::new(p.clone());
	doc.execute(key(Some(pad), pad_comp, fx.reverb, false))
		.unwrap();
	assert!(matches!(
		doc.execute(Command::SetChannel {
			track: Some(pad),
			patch: ChannelPatch {
				output: Some(Output::Bus(fx.drum_bus)),
				..Default::default()
			},
		}),
		Err(ModelError::RoutingCycle { .. })
	));
	assert!(matches!(
		doc.execute(Command::AddSend {
			track: pad,
			send: Send::new(fx.reverb, -6.0),
			index: None,
		}),
		Err(ModelError::RoutingCycle { .. })
	));

	// A device inserted (or a track added) with a sidechain is validated the same way.
	assert!(matches!(
		exec_err(
			&p,
			Command::InsertDevice {
				track: Some(fx.lead),
				device: Device::builtin("compressor", "Duck").with_sidechain(fx.reverb, false),
				index: None,
			}
		),
		ModelError::InvalidSidechain { .. }
	));
	let mut looped = Track::new("Loop", TrackKind::Audio);
	looped.channel.output = Output::Bus(fx.reverb);
	looped
		.channel
		.inserts
		.push(Device::builtin("compressor", "Duck").with_sidechain(fx.reverb, false));
	assert!(matches!(
		exec_err(
			&p,
			Command::AddTrack {
				track: looped,
				index: None
			}
		),
		ModelError::InvalidSidechain { .. }
	));
}

#[test]
fn removing_a_key_source_disconnects_and_undo_reconnects() {
	let fx = fixture();
	let (p, pad, synth, pad_comp) = with_pad(&fx.project);
	let mut doc = Document::new(p);
	doc.execute(key(Some(pad), pad_comp, fx.drums, false))
		.unwrap();
	doc.execute(key(None, fx.limiter, fx.drums, true)).unwrap();
	doc.execute(Command::SetOutputRoutes {
		track: pad,
		routes: vec![OutputRoute {
			port: 1,
			bus: Some(fx.drum_bus),
		}],
	})
	.unwrap();
	let before = doc.project().clone();
	round_trip(&before, Command::RemoveTrack { track: fx.drums });
	round_trip(&before, Command::RemoveTrack { track: fx.drum_bus });
	doc.execute(Command::RemoveTrack { track: fx.drums })
		.unwrap();
	doc.execute(Command::RemoveTrack { track: fx.drum_bus })
		.unwrap();
	let p = doc.project();
	assert_eq!(p.device(Some(pad), pad_comp).unwrap().sidechain, None);
	assert_eq!(p.device(None, fx.limiter).unwrap().sidechain, None);
	assert!(p.device(Some(pad), synth).unwrap().aux_outputs.is_empty());
	doc.undo().unwrap();
	doc.undo().unwrap();
	assert_eq!(doc.project(), &before);
}

#[test]
fn output_routes_round_trip_and_validate() {
	let fx = fixture();
	let (p, pad, _, pad_comp) = with_pad(&fx.project);
	let routes = |list: &[(u32, Option<TrackId>)]| Command::SetOutputRoutes {
		track: fx.drums,
		routes: list
			.iter()
			.map(|&(port, bus)| OutputRoute { port, bus })
			.collect(),
	};
	let routed = round_trip(
		&p,
		routes(&[(1, Some(fx.reverb)), (2, None), (3, Some(fx.drum_bus))]),
	);
	let summary = routed.summary();
	let drums = summary.tracks.iter().find(|t| t.id == fx.drums).unwrap();
	assert_eq!(drums.instrument.as_ref().unwrap().aux_outputs.len(), 3);
	round_trip(&routed, routes(&[]));

	let invalid = |list: &[(u32, Option<TrackId>)]| exec_err(&p, routes(list));
	assert!(matches!(
		invalid(&[(0, None)]),
		ModelError::InvalidOutputRoute { .. }
	));
	assert!(matches!(
		invalid(&[(1, None), (1, Some(fx.reverb))]),
		ModelError::InvalidOutputRoute { .. }
	));
	assert!(matches!(
		invalid(&[(65, None)]),
		ModelError::InvalidOutputRoute { .. }
	));
	assert!(matches!(
		invalid(&[(1, Some(fx.lead))]),
		ModelError::NotABus { .. }
	));
	assert!(matches!(
		exec_err(
			&p,
			Command::SetOutputRoutes {
				track: fx.audio,
				routes: Vec::new()
			}
		),
		ModelError::NoInstrument(_)
	));
	// Pad keyed by the reverb: sending a pad output into the drum bus (-> reverb) loops.
	let mut doc = Document::new(p.clone());
	doc.execute(key(Some(pad), pad_comp, fx.reverb, false))
		.unwrap();
	assert!(matches!(
		doc.execute(Command::SetOutputRoutes {
			track: pad,
			routes: vec![OutputRoute {
				port: 1,
				bus: Some(fx.drum_bus)
			}],
		}),
		Err(ModelError::RoutingCycle { .. })
	));
	// Effects have no extra outputs.
	let mut fx_device = Device::builtin("eq", "EQ");
	fx_device
		.aux_outputs
		.push(OutputRoute { port: 1, bus: None });
	assert!(matches!(
		exec_err(
			&p,
			Command::InsertDevice {
				track: Some(fx.lead),
				device: fx_device,
				index: None
			}
		),
		ModelError::InvalidValue { .. }
	));
}

#[test]
fn device_state_round_trips() {
	let fx = fixture();
	let p = &fx.project;
	let stored = round_trip(
		p,
		Command::SetDeviceState {
			track: Some(fx.lead),
			device: fx.synth,
			state: Some("AAEC".into()),
			params: None,
		},
	);
	let synth = stored.device(Some(fx.lead), fx.synth).unwrap();
	assert_eq!(synth.state.as_deref(), Some("AAEC"));
	assert_eq!(synth.params.len(), 2, "params untouched without a map");
	assert!(
		stored.summary().tracks[0]
			.instrument
			.as_ref()
			.unwrap()
			.has_state
	);

	let preset: std::collections::BTreeMap<String, f32> = [("cutoff".to_owned(), 300.0)].into();
	let loaded = round_trip(
		&stored,
		Command::SetDeviceState {
			track: Some(fx.lead),
			device: fx.synth,
			state: None,
			params: Some(preset.clone()),
		},
	);
	let synth = loaded.device(Some(fx.lead), fx.synth).unwrap();
	assert_eq!((synth.state.as_deref(), &synth.params), (None, &preset));

	round_trip(
		p,
		Command::SetDeviceState {
			track: None,
			device: fx.limiter,
			state: Some("eA==".into()),
			params: Some(Default::default()),
		},
	);
	assert!(matches!(
		exec_err(
			p,
			Command::SetDeviceState {
				track: Some(fx.lead),
				device: fx.synth,
				state: None,
				params: Some([("bad/id".to_owned(), 1.0)].into()),
			}
		),
		ModelError::InvalidValue { .. }
	));
	assert!(matches!(
		exec_err(
			p,
			Command::SetDeviceState {
				track: Some(fx.audio),
				device: fx.synth,
				state: None,
				params: None,
			}
		),
		ModelError::DeviceNotFound { .. }
	));
}

#[test]
fn routing_fields_persist_and_old_files_load() {
	let fx = fixture();
	let (p, pad, _, pad_comp) = with_pad(&fx.project);
	let mut doc = Document::new(p);
	doc.execute(key(Some(pad), pad_comp, fx.drums, true))
		.unwrap();
	doc.execute(Command::SetOutputRoutes {
		track: pad,
		routes: vec![OutputRoute {
			port: 2,
			bus: Some(fx.reverb),
		}],
	})
	.unwrap();
	let p = doc.project().clone();
	let json = p.to_json_string().unwrap();
	assert!(json.contains("\"sidechain\"") && json.contains("\"aux_outputs\""));
	assert_eq!(Project::from_json_str(&json).unwrap(), p);

	// Files written before sidechains existed have neither field.
	let old = fx.project.to_json_string().unwrap();
	assert!(!old.contains("sidechain") && !old.contains("aux_outputs"));
	assert_eq!(Project::from_json_str(&old).unwrap(), fx.project);

	// Loading rejects keys from missing tracks, self-keys and keyed feedback loops.
	let mut bad = p.clone();
	let pad_index = bad.track_index(pad).unwrap();
	bad.tracks[pad_index].channel.inserts[0].sidechain = Some(SidechainSource {
		track: pad,
		pre_fader: false,
	});
	assert!(Project::from_json_str(&bad.to_json_string().unwrap()).is_err());
	let mut bad = p.clone();
	bad.tracks[pad_index].channel.inserts[0].sidechain = Some(SidechainSource {
		track: TrackId::new(),
		pre_fader: false,
	});
	assert!(matches!(
		Project::from_json_str(&bad.to_json_string().unwrap()),
		Err(ModelError::TrackNotFound(_))
	));
	let mut bad = p.clone();
	bad.tracks[pad_index].channel.inserts[0].sidechain = Some(SidechainSource {
		track: fx.reverb,
		pre_fader: false,
	});
	assert!(matches!(
		Project::from_json_str(&bad.to_json_string().unwrap()),
		Err(ModelError::RoutingCycle { .. })
	));
}

#[test]
fn clip_fades_round_trip_and_validate() {
	let fx = fixture();
	let fade = |s| Some(ClipFade::new(s, FadeCurve::EqualPower));
	// The vocal clip spans beats 4..12 at 120 bpm = 4 s.
	let after = round_trip(
		&fx.project,
		Command::SetClipFades {
			clip: fx.vox,
			fade_in: fade(0.01),
			fade_out: fade(2.0),
		},
	);
	let (_, clip) = after.find_clip(fx.vox).unwrap();
	assert_eq!(clip.fades(), (fade(0.01), fade(2.0)));
	let summary = serde_json::to_string(&after.summary()).unwrap();
	assert!(
		summary.contains("in 0.010 s equal_power, out 2.000 s equal_power"),
		"{summary}"
	);
	round_trip(
		&after,
		Command::SetClipFades {
			clip: fx.vox,
			fade_in: None,
			fade_out: None,
		},
	);

	let too_long = exec_err(
		&fx.project,
		Command::SetClipFades {
			clip: fx.vox,
			fade_in: fade(5.0),
			fade_out: None,
		},
	)
	.to_string();
	assert!(too_long.contains("lasts 4.000 s"), "{too_long}");
	let sum = exec_err(
		&fx.project,
		Command::SetClipFades {
			clip: fx.vox,
			fade_in: fade(2.5),
			fade_out: fade(2.0),
		},
	)
	.to_string();
	assert!(sum.contains("must not exceed"), "{sum}");
	let zero = exec_err(
		&fx.project,
		Command::SetClipFades {
			clip: fx.vox,
			fade_in: fade(0.0),
			fade_out: None,
		},
	)
	.to_string();
	assert!(zero.contains("null for no fade"), "{zero}");
	assert!(matches!(
		exec_err(
			&fx.project,
			Command::SetClipFades {
				clip: fx.riff,
				fade_in: fade(0.1),
				fade_out: None,
			},
		),
		ModelError::NotAnAudioClip(_)
	));
}

#[test]
fn audio_clips_without_fades_keep_the_old_json_shape() {
	let clip = Clip::audio("Vox", beats(0.0), beats(4.0), "vox.wav");
	let json = serde_json::to_value(&clip).unwrap();
	assert!(json["content"].get("fade_in").is_none());
	let old = json!({
		"id": clip.id, "name": "Vox", "start": 0, "length": 3840,
		"content": {"type": "audio", "file": "vox.wav", "offset_seconds": 0.0, "gain_db": 0.0}
	});
	let parsed: Clip = serde_json::from_value(old).unwrap();
	assert_eq!(parsed.fades(), (None, None));
	let with: Clip = serde_json::from_value(json!({
		"id": clip.id, "name": "Vox", "start": 0, "length": 3840,
		"content": {"type": "audio", "file": "vox.wav", "offset_seconds": 0.0, "gain_db": 0.0,
			"fade_out": {"seconds": 1.5}}
	}))
	.unwrap();
	assert_eq!(with.fades().1, Some(ClipFade::new(1.5, FadeCurve::Linear)));
}

#[test]
fn send_levels_are_automatable_and_their_lanes_follow_the_send() {
	let fx = fixture();
	let address = ParamAddress::send(fx.lead, fx.send);
	let after = round_trip(
		&fx.project,
		Command::SetParam {
			address: address.clone(),
			value: -3.0,
		},
	);
	let level = |p: &Project| p.track(fx.lead).unwrap().channel.sends[0].level_db;
	assert_eq!(level(&after), -3.0);
	assert!(matches!(
		exec_err(
			&fx.project,
			Command::SetParam {
				address: address.clone(),
				value: 40.0
			}
		),
		ModelError::InvalidVolume(_)
	));
	let lane = Command::SetAutomation {
		address: address.clone(),
		points: vec![
			AutomationPoint::new(Tick(0), -20.0),
			AutomationPoint::new(beats(4.0), 0.0).with_shape(PointShape::Smooth),
		],
	};
	let with_lane = round_trip(&fx.project, lane.clone());
	assert!(with_lane.automation_lane(&address).is_some());
	assert!(matches!(
		exec_err(
			&fx.project,
			Command::SetAutomation {
				address: ParamAddress::send(fx.lead, DeviceId::new()),
				points: vec![AutomationPoint::new(Tick(0), 0.0)],
			}
		),
		ModelError::SendNotFound { .. }
	));

	// Removing the send drops its lane; undo brings both back.
	let removed = round_trip(
		&with_lane,
		Command::RemoveSend {
			track: fx.lead,
			send: fx.send,
		},
	);
	assert!(removed.automation_lane(&address).is_none());
	// Removing the bus it feeds drops it too.
	let no_bus = round_trip(&with_lane, Command::RemoveTrack { track: fx.reverb });
	assert!(no_bus.automation_lane(&address).is_none());
	assert!(no_bus.validate().is_ok());
}

#[test]
fn point_shapes_interpolate_and_keep_the_old_json_shape() {
	let tempo = cadenza_core::TempoMap::constant(120.0);
	let points = vec![
		AutomationPoint::new(beats(0.0), 0.0),
		AutomationPoint::new(beats(4.0), 1.0).with_shape(PointShape::Hold),
		AutomationPoint::new(beats(8.0), 0.0).with_shape(PointShape::Smooth),
		AutomationPoint::new(beats(12.0), 1.0),
	];
	let at = |b: f64| automation_value(&points, beats(b), &tempo).unwrap();
	assert_eq!(at(-1.0), 0.0);
	assert!((at(2.0) - 0.5).abs() < 1e-6, "linear");
	assert_eq!(at(6.0), 1.0, "hold keeps the value");
	assert_eq!(at(7.99), 1.0);
	assert!((at(10.0) - 0.5).abs() < 1e-6, "smooth is symmetric");
	assert!(at(9.0) < 0.25, "smooth starts slowly");
	assert_eq!(at(100.0), 1.0);
	assert!(automation_value(&[], beats(1.0), &tempo).is_none());

	// Interpolation runs in seconds: across a tempo change the halfway value is where
	// half the time has passed, not half the beats.
	let mut slow = cadenza_core::TempoMap::constant(120.0);
	slow.set_point(beats(2.0), 60.0);
	let ramp = [
		AutomationPoint::new(beats(0.0), 0.0),
		AutomationPoint::new(beats(4.0), 3.0),
	];
	// 0..2 beats = 1 s, 2..4 beats = 2 s: beat 2 is a third of the way.
	assert!((automation_value(&ramp, beats(2.0), &slow).unwrap() - 1.0).abs() < 1e-5);

	let json = serde_json::to_value(AutomationPoint::new(Tick(0), 1.0)).unwrap();
	assert_eq!(json, json!({"tick": 0, "value": 1.0}));
	let held: AutomationPoint =
		serde_json::from_value(json!({"tick": 0, "value": 1.0, "shape": "hold"})).unwrap();
	assert_eq!(held.shape, PointShape::Hold);
	let old: AutomationPoint = serde_json::from_value(json!({"tick": 5, "value": 2.0})).unwrap();
	assert_eq!(old.shape, PointShape::Linear);
}

#[test]
fn update_audio_clip_swaps_source_offset_and_gain() {
	let fx = fixture();
	let after = round_trip(
		&fx.project,
		Command::UpdateAudioClip {
			clip: fx.vox,
			patch: AudioClipPatch {
				file: Some("vox.processed.wav".into()),
				offset_seconds: Some(0.5),
				gain_db: Some(-3.0),
			},
		},
	);
	let (_, clip) = after.find_clip(fx.vox).unwrap();
	let ClipContent::Audio {
		file,
		offset_seconds,
		gain_db,
		..
	} = &clip.content
	else {
		panic!("audio clip");
	};
	assert_eq!(file, &std::path::PathBuf::from("vox.processed.wav"));
	assert_eq!((*offset_seconds, *gain_db), (0.5, -3.0));
	let label = Command::UpdateAudioClip {
		clip: fx.vox,
		patch: AudioClipPatch {
			gain_db: Some(2.0),
			..Default::default()
		},
	}
	.label();
	assert_eq!(label, "Set clip gain to 2.0 dB");
	for patch in [
		AudioClipPatch::default(),
		AudioClipPatch {
			offset_seconds: Some(-1.0),
			..Default::default()
		},
		AudioClipPatch {
			file: Some("".into()),
			..Default::default()
		},
	] {
		exec_err(
			&fx.project,
			Command::UpdateAudioClip {
				clip: fx.vox,
				patch,
			},
		);
	}
	assert!(matches!(
		exec_err(
			&fx.project,
			Command::UpdateAudioClip {
				clip: fx.riff,
				patch: AudioClipPatch {
					gain_db: Some(0.0),
					..Default::default()
				},
			}
		),
		ModelError::NotAnAudioClip(_)
	));
}

fn marker(beat: f64, name: &str) -> Command {
	Command::AddMarker {
		marker: Marker::new(beats(beat), name),
	}
}

#[test]
fn a_group_is_one_undo_step_that_redoes() {
	let mut doc = Document::new(Project::new("x"));
	doc.execute(marker(0.0, "Intro")).unwrap();
	let before = doc.project().clone();
	let r0 = doc.revision();
	doc.begin_group("AI: add a bridge");
	doc.execute(marker(8.0, "Bridge")).unwrap();
	doc.execute(Command::SetTempo {
		tick: Tick::ZERO,
		bpm: 90.0,
	})
	.unwrap();
	assert!(doc.in_group());
	assert!(doc.end_group());
	assert!(!doc.in_group());
	assert!(doc.revision() > r0);
	assert_eq!(
		doc.history_labels(),
		vec!["Add marker 'Intro'", "AI: add a bridge"]
	);
	let after = doc.project().clone();

	assert_eq!(doc.undo().unwrap(), "AI: add a bridge");
	assert_eq!(doc.project(), &before);
	assert_eq!(doc.redo().unwrap(), "AI: add a bridge");
	assert_eq!(doc.project(), &after);
	doc.undo().unwrap();
	doc.undo().unwrap();
	assert!(doc.project().markers.is_empty());
}

#[test]
fn empty_and_nested_groups() {
	let mut doc = Document::new(Project::new("x"));
	doc.execute(marker(0.0, "A")).unwrap();
	doc.undo().unwrap();
	let r = doc.revision();
	// Empty group: no step, the user's redo survives.
	doc.begin_group("AI: nothing");
	assert!(!doc.end_group());
	assert_eq!(doc.revision(), r);
	assert!(doc.can_redo());
	assert!(!doc.end_group(), "no group open");

	// Nested: the outer label wins and only the outer end folds.
	doc.begin_group("outer");
	doc.execute(marker(1.0, "B")).unwrap();
	doc.begin_group("inner");
	doc.execute(marker(2.0, "C")).unwrap();
	assert!(!doc.end_group());
	assert!(doc.in_group());
	doc.execute(marker(3.0, "D")).unwrap();
	assert!(doc.end_group());
	assert_eq!(doc.history_labels(), vec!["outer"]);
	assert!(!doc.can_redo(), "executing cleared the redo stack");
	doc.undo().unwrap();
	assert!(doc.project().markers.is_empty());
}

#[test]
fn undo_inside_a_group_stays_inside_it() {
	let mut doc = Document::new(Project::new("x"));
	doc.execute(marker(0.0, "User")).unwrap();
	doc.begin_group("AI");
	assert_eq!(
		doc.undo(),
		Err(ModelError::NothingToUndo),
		"user step is out of reach"
	);
	doc.execute(marker(4.0, "Mine")).unwrap();
	doc.execute(marker(8.0, "Scratch")).unwrap();
	doc.undo().unwrap();
	assert!(doc.can_redo());
	doc.undo().unwrap();
	assert_eq!(doc.undo(), Err(ModelError::NothingToUndo));
	doc.redo().unwrap();
	assert!(doc.end_group());
	// The scratch step the group undid is not redoable afterwards.
	assert!(!doc.can_redo());
	assert_eq!(doc.history_labels(), vec!["Add marker 'User'", "AI"]);
	assert_eq!(doc.project().markers.len(), 2);
}

#[test]
fn redo_inside_a_group_cannot_reach_older_steps() {
	let mut doc = Document::new(Project::new("x"));
	doc.execute(marker(0.0, "User")).unwrap();
	doc.undo().unwrap();
	doc.begin_group("AI");
	assert_eq!(doc.redo(), Err(ModelError::NothingToRedo));
	doc.end_group();
	doc.redo().unwrap();
	assert_eq!(doc.project().markers.len(), 1);
}

#[test]
fn aborting_a_group_reverts_it() {
	let mut doc = Document::new(Project::new("x"));
	doc.execute(marker(0.0, "User")).unwrap();
	let before = doc.project().clone();
	doc.begin_group("AI");
	doc.begin_group("nested");
	doc.execute(marker(4.0, "A")).unwrap();
	doc.execute(marker(8.0, "B")).unwrap();
	assert_eq!(doc.abort_group().unwrap(), 2);
	assert!(!doc.in_group());
	assert_eq!(doc.project(), &before);
	assert_eq!(doc.history_labels(), vec!["Add marker 'User'"]);
	assert_eq!(doc.abort_group().unwrap(), 0);
}

fn drum_kit_zones() -> Vec<SampleZone> {
	let mut closed = SampleZone::pad("/kit/Closed Hat.wav", 42);
	closed.choke = 1;
	let mut open = SampleZone::pad("/kit/Open Hat.wav", 46);
	open.choke = 1;
	open.decay_ms = Some(400.0);
	open.output = 2;
	let mut loop_pad = SampleZone::full_keyboard("/kit/Pad C3.wav", 48);
	loop_pad.loop_mode = LoopMode::PingPong;
	loop_pad.loop_start = 0.25;
	loop_pad.loop_end = Some(1.5);
	loop_pad.loop_crossfade = 0.05;
	loop_pad.vel_low = 0.5;
	vec![SampleZone::pad("/kit/Kick.wav", 36), closed, open, loop_pad]
}

#[test]
fn sample_zones_round_trip_persist_and_summarize() {
	let fx = fixture();
	let zones = drum_kit_zones();
	let after = round_trip(
		&fx.project,
		Command::SetSampleZones {
			track: Some(fx.lead),
			device: fx.synth,
			zones: zones.clone(),
		},
	);
	assert_eq!(after.device(Some(fx.lead), fx.synth).unwrap().zones, zones);
	// Persistence keeps every field; projects without zones still load (format 1).
	let json = after.to_json_string().unwrap();
	assert!(json.contains("\"ping_pong\"") && json.contains("Open Hat.wav"));
	assert_eq!(Project::from_json_str(&json).unwrap(), after);
	let old = fx.project.to_json_string().unwrap();
	assert!(!old.contains("zones"));
	assert_eq!(Project::from_json_str(&old).unwrap(), fx.project);

	let summary = after.summary();
	let zs = &summary.tracks[0].instrument.as_ref().unwrap().zones;
	assert_eq!(zs.len(), 4);
	assert_eq!(zs[0].keys, "C2 (36)");
	assert_eq!(zs[0].sample, "Kick.wav");
	assert!(zs[2].settings.contains("choke=1") && zs[2].settings.contains("output=2"));
	assert_eq!(zs[3].keys, "C-1..G9");
	assert!(
		zs[3].settings.contains("loop=ping_pong"),
		"{}",
		zs[3].settings
	);
	assert_eq!(zs[3].velocity.as_deref(), Some("0.50..1.00"));

	assert_eq!(
		Command::SetSampleZones {
			track: None,
			device: fx.limiter,
			zones: Vec::new()
		}
		.label(),
		"Unload samples"
	);
}

#[test]
fn sample_zones_are_validated() {
	let fx = fixture();
	let mut bad = drum_kit_zones();
	bad[1].key_high = 200;
	let err = exec_err(
		&fx.project,
		Command::SetSampleZones {
			track: Some(fx.lead),
			device: fx.synth,
			zones: bad,
		},
	);
	assert!(err.to_string().contains("zone 1 key_high"), "{err}");
	assert!(matches!(
		exec_err(
			&fx.project,
			Command::SetSampleZones {
				track: Some(fx.lead),
				device: DeviceId::new(),
				zones: Vec::new(),
			}
		),
		ModelError::DeviceNotFound { .. }
	));
	// Plugins cannot hold zones, neither through the command nor a new device.
	let mut p = fx.project.clone();
	let plugin = Device {
		kind: DeviceKind::Clap {
			plugin_id: "x.sampler".into(),
			path: "/x.clap".into(),
		},
		..Device::builtin("", "Plugin")
	};
	let plugin_id = plugin.id;
	let mut doc = Document::new(p.clone());
	doc.execute(Command::InsertDevice {
		track: Some(fx.lead),
		device: plugin.clone(),
		index: None,
	})
	.unwrap();
	p = doc.project().clone();
	assert!(
		exec_err(
			&p,
			Command::SetSampleZones {
				track: Some(fx.lead),
				device: plugin_id,
				zones: drum_kit_zones(),
			}
		)
		.to_string()
		.contains("only the built-in sampler")
	);
	assert!(matches!(
		exec_err(
			&fx.project,
			Command::InsertDevice {
				track: Some(fx.lead),
				device: plugin.with_zones(drum_kit_zones()),
				index: None,
			}
		),
		ModelError::InvalidValue { .. }
	));
}

#[test]
fn time_signature_changes_round_trip_and_summarize() {
	let fx = fixture();
	let ts = |numerator, denominator| TimeSignature {
		numerator,
		denominator,
	};
	let p = round_trip(
		&fx.project,
		Command::SetTimeSignature {
			time_signature: ts(3, 4),
			bar: 4,
		},
	);
	assert_eq!(
		p.time_signature_changes,
		vec![MeterChange::new(4, ts(3, 4))]
	);
	let p = round_trip(
		&p,
		Command::SetTimeSignature {
			time_signature: ts(7, 8),
			bar: 4,
		},
	);
	assert_eq!(p.time_signature_changes[0].signature(), ts(7, 8));
	let p2 = round_trip(&p, Command::RemoveTimeSignature { bar: 4 });
	assert!(p2.time_signature_changes.is_empty());
	assert!(matches!(
		exec_err(&p2, Command::RemoveTimeSignature { bar: 4 }),
		ModelError::TimeSignatureChangeNotFound(4)
	));
	assert!(matches!(
		exec_err(&p2, Command::RemoveTimeSignature { bar: 0 }),
		ModelError::TimeSignatureChangeNotFound(0)
	));
	assert!(
		exec_err(
			&p2,
			Command::SetTimeSignature {
				time_signature: ts(3, 4),
				bar: 200_000,
			}
		)
		.to_string()
		.contains("bar index")
	);

	// Bars 1-4 in 4/4 (16 beats), then 3/4: a marker at beat 19 is bar 6.
	let mut p = p;
	p.time_signature_changes = vec![MeterChange::new(4, ts(3, 4))];
	p.markers = vec![Marker::new(beats(19.0), "B")];
	let s = p.summary();
	assert_eq!(s.time_signature_changes.len(), 1);
	assert_eq!(s.time_signature_changes[0].bar, 5);
	assert_eq!(s.time_signature_changes[0].beat, 16.0);
	assert_eq!(s.time_signature_changes[0].time_signature, "3/4");
	assert_eq!(s.markers[0].bar, 6.0);

	// Persisted as an optional field; old files without it still load.
	let json = p.to_json_string().unwrap();
	assert!(json.contains("time_signature_changes"));
	assert_eq!(Project::from_json_str(&json).unwrap(), p);
	let plain = fx.project.to_json_string().unwrap();
	assert!(!plain.contains("time_signature_changes"));
}

#[test]
fn controller_lanes_round_trip_split_and_persist() {
	let fx = fixture();
	let point = |tick: i64, value: f32| ControllerPoint::new(Tick(tick), value);
	let p = round_trip(
		&fx.project,
		Command::SetControllerLane {
			clip: fx.riff,
			controller: Controller::MOD_WHEEL,
			points: vec![point(PPQ * 2, 1.0), point(0, 0.25)],
		},
	);
	let (_, clip) = p.find_clip(fx.riff).unwrap();
	let lane = clip.controller(Controller::MOD_WHEEL).unwrap();
	assert_eq!(lane.points[0], point(0, 0.25), "sorted on apply");
	let p = round_trip(
		&p,
		Command::SetControllerLane {
			clip: fx.riff,
			controller: Controller::MOD_WHEEL,
			points: vec![],
		},
	);
	assert!(p.find_clip(fx.riff).unwrap().1.controllers().is_empty());
	assert!(
		exec_err(
			&p,
			Command::SetControllerLane {
				clip: fx.riff,
				controller: Controller::PitchBend,
				points: vec![point(0, 1.5)],
			}
		)
		.to_string()
		.contains("pitch bend values")
	);
	assert!(matches!(
		exec_err(
			&p,
			Command::SetControllerLane {
				clip: fx.vox,
				controller: Controller::SUSTAIN,
				points: vec![point(0, 1.0)],
			}
		),
		ModelError::NotAMidiClip(_)
	));

	// Split: the right part starts with the value in effect at the cut.
	let mut doc = Document::new(fx.project.clone());
	doc.execute(Command::SetControllerLane {
		clip: fx.riff,
		controller: Controller::PitchBend,
		points: vec![point(0, 0.0), point(PPQ, 0.5), point(PPQ * 3, -0.5)],
	})
	.unwrap();
	let (_, riff) = doc.project().find_clip(fx.riff).unwrap();
	let at = riff.start + Tick(PPQ * 2);
	let right = ClipId::new();
	let cmds = takes::split_clip(doc.project(), fx.riff, at, right, None).unwrap();
	doc.execute(Command::Batch {
		label: "Split".into(),
		commands: cmds,
	})
	.unwrap();
	let left = doc.project().find_clip(fx.riff).unwrap().1;
	assert_eq!(
		left.controller(Controller::PitchBend).unwrap().points.len(),
		2,
		"the left part keeps the points before the cut"
	);
	let r = doc.project().find_clip(right).unwrap().1;
	assert_eq!(
		r.controller(Controller::PitchBend).unwrap().points,
		vec![point(0, 0.5), point(PPQ, -0.5)]
	);
	let summary = doc.project().summary();
	let clip_summary = summary
		.tracks
		.iter()
		.flat_map(|t| t.clips.iter())
		.find(|c| c.id == right)
		.unwrap();
	assert_eq!(
		clip_summary.controllers,
		vec!["pitch_bend (2 events, -0.50..0.50)"]
	);
	let json = doc.project().to_json_string().unwrap();
	assert!(json.contains("\"controller\": \"pitch_bend\""));
	assert_eq!(&Project::from_json_str(&json).unwrap(), doc.project());
}

#[test]
fn folder_tracks_hold_tracks_and_reparent_on_removal() {
	let fx = fixture();
	let mut p = fx.project.clone();
	let outer = Track::new("Group", TrackKind::Folder);
	let inner = Track::new("Sub", TrackKind::Folder);
	let (outer_id, inner_id) = (outer.id, inner.id);
	p.tracks.push(outer);
	p.tracks.push(inner);
	let p = round_trip(
		&p,
		Command::SetTrackFolder {
			track: inner_id,
			folder: Some(outer_id),
		},
	);
	let p = round_trip(
		&p,
		Command::SetTrackFolder {
			track: fx.lead,
			folder: Some(inner_id),
		},
	);
	assert_eq!(p.folders_of(fx.lead), vec![inner_id, outer_id]);
	assert_eq!(p.folder_contents(outer_id), vec![fx.lead, inner_id]);
	// No cycles, folders only, no audio on folders.
	assert!(
		exec_err(
			&p,
			Command::SetTrackFolder {
				track: outer_id,
				folder: Some(inner_id),
			}
		)
		.to_string()
		.contains("inside itself")
	);
	assert!(
		exec_err(
			&p,
			Command::SetTrackFolder {
				track: fx.lead,
				folder: Some(fx.drum_bus),
			}
		)
		.to_string()
		.contains("Folder tracks")
	);
	assert!(
		exec_err(
			&p,
			Command::InsertDevice {
				track: Some(outer_id),
				device: Device::builtin("eq", "EQ"),
				index: None,
			}
		)
		.to_string()
		.contains("folder tracks carry no audio")
	);
	// Removing the inner folder moves its tracks up to the outer one (undo restores).
	let p2 = round_trip(&p, Command::RemoveTrack { track: inner_id });
	assert_eq!(p2.track(fx.lead).unwrap().folder, Some(outer_id));
	let s = p.summary();
	assert_eq!(
		s.tracks.iter().find(|t| t.id == fx.lead).unwrap().folder,
		Some(inner_id)
	);
	let json = p.to_json_string().unwrap();
	assert_eq!(Project::from_json_str(&json).unwrap(), p);
}

#[test]
fn project_meta_is_undoable_validated_and_optional_in_json() {
	let project = Project::new("Song");
	let after = round_trip(
		&project,
		Command::SetProjectMeta {
			meta: crate::ProjectMeta {
				style: Some("core/latin_pop".into()),
			},
		},
	);
	assert_eq!(after.meta.style.as_deref(), Some("core/latin_pop"));
	assert_eq!(after.summary().style.as_deref(), Some("core/latin_pop"));
	let json = serde_json::to_string(&after).unwrap();
	assert!(json.contains("\"meta\":{\"style\":\"core/latin_pop\"}"));
	assert!(!serde_json::to_string(&project).unwrap().contains("meta"));
	let mut doc = Document::new(project);
	assert!(
		doc.execute(Command::SetProjectMeta {
			meta: crate::ProjectMeta {
				style: Some(" ".into())
			}
		})
		.is_err()
	);
}

#[test]
fn armed_tracks_are_saved_undoable_and_follow_track_removal() {
	let mut p = Project::new("Song");
	let vox = Track::new("Vox", TrackKind::Audio);
	let keys = Track::new("Keys", TrackKind::Instrument);
	let bus = Track::new("Bus", TrackKind::Bus);
	let (vox_id, keys_id, bus_id) = (vox.id, keys.id, bus.id);
	p.tracks.extend([vox, keys, bus]);
	assert!(!serde_json::to_string(&p).unwrap().contains("armed"));
	let mut doc = Document::new(p);
	doc.execute(Command::SetArmedTracks {
		armed: vec![keys_id, vox_id],
	})
	.unwrap();
	assert_eq!(doc.project().armed, vec![keys_id, vox_id]);
	let json = doc.project().to_json_string().unwrap();
	assert_eq!(
		Project::from_json_str(&json).unwrap().armed,
		vec![keys_id, vox_id]
	);
	// Buses do not record; ids are checked.
	assert!(
		doc.execute(Command::SetArmedTracks {
			armed: vec![bus_id]
		})
		.is_err()
	);
	assert!(
		doc.execute(Command::SetArmedTracks {
			armed: vec![vox_id, vox_id]
		})
		.is_err()
	);
	// Removing an armed track disarms it; undo arms it again.
	doc.execute(Command::RemoveTrack { track: vox_id }).unwrap();
	assert_eq!(doc.project().armed, vec![keys_id]);
	doc.undo().unwrap();
	assert_eq!(doc.project().armed, vec![keys_id, vox_id]);
	doc.undo().unwrap();
	assert!(doc.project().armed.is_empty());
	// A stale id in a file is dropped on load, not an error.
	let mut v: serde_json::Value = serde_json::from_str(&json).unwrap();
	v["armed"]
		.as_array_mut()
		.unwrap()
		.push(json!(TrackId::new().to_string()));
	let loaded = Project::from_json_str(&v.to_string()).unwrap();
	assert_eq!(loaded.armed, vec![keys_id, vox_id]);
}

#[test]
fn muting_a_clip_is_one_undoable_step_and_omitted_from_json_when_off() {
	let mut doc = Document::new(Project::new("m"));
	let mut t = Track::new("Keys", TrackKind::Instrument);
	let clip = Clip::midi("A", Tick(0), Tick(PPQ));
	let id = clip.id;
	t.clips.push(clip);
	doc.execute(Command::AddTrack {
		track: t,
		index: None,
	})
	.unwrap();
	let json = serde_json::to_string(doc.project()).unwrap();
	assert!(!json.contains("muted"));
	doc.execute(Command::UpdateClip {
		clip: id,
		patch: ClipPatch {
			muted: Some(true),
			..ClipPatch::default()
		},
	})
	.unwrap();
	assert_eq!(doc.history_labels().last().copied(), Some("Mute clip"));
	let (_, c) = doc.project().find_clip(id).unwrap();
	assert!(c.muted && c.plays() && !c.sounds());
	let json = serde_json::to_string(doc.project()).unwrap();
	assert!(json.contains("\"muted\":true"));
	let back: Project = serde_json::from_str(&json).unwrap();
	assert!(back.find_clip(id).unwrap().1.muted);
	assert!(doc.project().summary().tracks[0].clips[0].muted);
	doc.undo().unwrap();
	assert!(!doc.project().find_clip(id).unwrap().1.muted);
}

fn linked_fixture() -> (Document, TrackId, ClipId, ClipId, NoteId) {
	let mut doc = Document::new(Project::new("links"));
	let mut t = Track::new("Keys", TrackKind::Instrument);
	let note = Note::new(60, Tick(0), Tick(PPQ), 0.8);
	let note_id = note.id;
	let clip = Clip::midi("Pat", Tick(0), Tick(PPQ * 4)).with_notes([note]);
	let a = clip.id;
	t.clips.push(clip);
	let track = t.id;
	doc.execute(Command::AddTrack {
		track: t,
		index: None,
	})
	.unwrap();
	let b = ClipId::new();
	let commands = links::copy_clip(
		doc.project(),
		a,
		b,
		None,
		Tick(PPQ * 4),
		true,
		cadenza_core::LinkId::new(),
	)
	.unwrap();
	doc.execute(Command::Batch {
		label: "Paste".into(),
		commands,
	})
	.unwrap();
	(doc, track, a, b, note_id)
}

#[test]
fn linked_clips_share_note_and_controller_edits_in_one_undo_step() {
	let (mut doc, _, a, b, note) = linked_fixture();
	assert_eq!(links::link_count(doc.project(), a), 2);
	let before = doc.history_labels().len();
	doc.execute(Command::UpdateNotes {
		clip: b,
		notes: vec![NotePatch {
			id: note,
			pitch: Some(67),
			velocity: None,
			start: None,
			length: None,
		}],
	})
	.unwrap();
	doc.execute(Command::AddNotes {
		clip: a,
		notes: vec![Note::new(72, Tick(PPQ), Tick(PPQ), 0.5)],
	})
	.unwrap();
	doc.execute(Command::SetControllerLane {
		clip: a,
		controller: Controller::Cc(1),
		points: vec![ControllerPoint {
			tick: Tick(0),
			value: 0.5,
		}],
	})
	.unwrap();
	let p = doc.project();
	let (ca, cb) = (p.find_clip(a).unwrap().1, p.find_clip(b).unwrap().1);
	assert!(ca.same_midi_content(cb));
	assert_eq!(cb.notes().len(), 2);
	assert_eq!(cb.notes()[0].pitch, 67);
	assert_eq!(cb.controllers().len(), 1);
	assert_eq!(doc.history_labels().len(), before + 3);
	doc.undo().unwrap();
	doc.undo().unwrap();
	doc.undo().unwrap();
	let p = doc.project();
	let (ca, cb) = (p.find_clip(a).unwrap().1, p.find_clip(b).unwrap().1);
	assert!(ca.same_midi_content(cb));
	assert_eq!(cb.notes()[0].pitch, 60);
	assert_eq!(cb.notes().len(), 1);
	let summary = p.summary();
	assert_eq!(summary.tracks[0].clips[0].link, Some(vec![b]));
	let json = serde_json::to_string(p).unwrap();
	let back = Project::from_json_str(&json).unwrap();
	assert_eq!(
		back.find_clip(b).unwrap().1.link,
		p.find_clip(a).unwrap().1.link
	);
}

#[test]
fn linked_clips_validate_content_and_unlink() {
	let (mut doc, track, a, b, note) = linked_fixture();
	let link = doc.project().find_clip(a).unwrap().1.link;
	// A clip with other content cannot join the group.
	let mut other = Clip::midi("Other", Tick(PPQ * 8), Tick(PPQ));
	other.link = link;
	let err = doc
		.execute(Command::AddClip { track, clip: other })
		.unwrap_err();
	assert!(err.to_string().contains("linked clips share"), "{err}");
	// A shorter copy keeps the shared notes past its end; the longest copy bounds them.
	doc.execute(Command::UpdateClip {
		clip: b,
		patch: ClipPatch {
			length: Some(Tick(PPQ / 2)),
			..ClipPatch::default()
		},
	})
	.unwrap();
	doc.execute(Command::AddNotes {
		clip: b,
		notes: vec![Note::new(62, Tick(PPQ * 2), Tick(PPQ), 0.5)],
	})
	.unwrap();
	assert!(
		doc.execute(Command::AddNotes {
			clip: b,
			notes: vec![Note::new(62, Tick(PPQ * 5), Tick(PPQ), 0.5)],
		})
		.is_err()
	);
	// Make unique: the last copy becomes independent too; edits no longer propagate.
	let unique = links::make_unique(doc.project(), b).unwrap();
	doc.execute(unique).unwrap();
	assert_eq!(
		doc.history_labels().last().copied(),
		Some("Make clip unique")
	);
	assert!(doc.project().find_clip(a).unwrap().1.link.is_none());
	doc.execute(Command::RemoveNotes {
		clip: a,
		notes: vec![note],
	})
	.unwrap();
	assert_eq!(doc.project().find_clip(b).unwrap().1.notes().len(), 1);
	doc.undo().unwrap();
	doc.undo().unwrap();
	assert_eq!(links::link_count(doc.project(), b), 2);
	// Removing one copy keeps the rest consistent; undo restores it linked.
	doc.execute(Command::RemoveClip { clip: a }).unwrap();
	doc.undo().unwrap();
	assert_eq!(links::link_count(doc.project(), a), 2);
	// Splitting a linked clip unlinks the cut part first.
	let cmds = takes::split_clip(doc.project(), a, Tick(PPQ * 2), ClipId::new(), None).unwrap();
	doc.execute(Command::Batch {
		label: "Split".into(),
		commands: cmds,
	})
	.unwrap();
	assert!(doc.project().find_clip(a).unwrap().1.link.is_none());
	assert_eq!(doc.project().find_clip(b).unwrap().1.notes().len(), 1);
}

fn lfo_on(param: &str, depth: f32) -> Modulator {
	let mut m = Modulator::lfo(cadenza_core::ModulatorId::new());
	m.targets.push(ModTarget {
		param: param.into(),
		depth,
	});
	m
}

#[test]
fn modulators_round_trip_validate_and_stay_out_of_old_json() {
	let fx = fixture();
	let set = |mods: Vec<Modulator>| Command::SetModulators {
		track: Some(fx.lead),
		device: fx.synth,
		modulators: mods,
	};
	let modulated = round_trip(&fx.project, set(vec![lfo_on("cutoff", 0.3)]));
	let synth = modulated.device(Some(fx.lead), fx.synth).unwrap();
	assert_eq!(synth.modulators.len(), 1);
	assert_eq!(set(vec![]).label(), "Remove modulation");
	// Summary line.
	let summary = modulated.summary();
	let line = &summary.tracks[0].instrument.as_ref().unwrap().modulators[0];
	assert_eq!(line, "LFO sine 1 Hz -> cutoff +0.30");

	// JSON: omitted when empty, kept when set, and loads back.
	let plain = serde_json::to_string(&fx.project).unwrap();
	assert!(!plain.contains("modulators"));
	let json = serde_json::to_string(&modulated).unwrap();
	assert!(json.contains("\"modulators\"") && json.contains("\"type\":\"lfo\""));
	let back: Project = serde_json::from_str(&json).unwrap();
	assert_eq!(back, modulated);

	// Validation.
	let err = exec_err(&fx.project, set(vec![lfo_on("cutoff", 1.5)])).to_string();
	assert!(err.contains("depth") && err.contains("-1..=1"), "{err}");
	let too_many = (0..9).map(|_| lfo_on("cutoff", 0.1)).collect();
	let err = exec_err(&fx.project, set(too_many)).to_string();
	assert!(err.contains("at most 8"), "{err}");
	let mut targets = lfo_on("cutoff", 0.1);
	targets.targets = (0..17)
		.map(|i| ModTarget {
			param: format!("p{i}"),
			depth: 0.1,
		})
		.collect();
	assert!(
		exec_err(&fx.project, set(vec![targets]))
			.to_string()
			.contains("at most 16")
	);
	let mut twice = lfo_on("cutoff", 0.1);
	twice.targets.push(twice.targets[0].clone());
	assert!(
		exec_err(&fx.project, set(vec![twice]))
			.to_string()
			.contains("once per modulator")
	);
	let mut fast = lfo_on("cutoff", 0.1);
	if let ModulatorKind::Lfo { rate_hz, .. } = &mut fast.kind {
		*rate_hz = 100.0;
	}
	assert!(
		exec_err(&fx.project, set(vec![fast]))
			.to_string()
			.contains("rate_hz")
	);
	let mut keyed = Modulator::envelope(cadenza_core::ModulatorId::new());
	if let ModulatorKind::Envelope { source, .. } = &mut keyed.kind {
		*source = EnvelopeSource::Sidechain;
	}
	let err = exec_err(&fx.project, set(vec![keyed])).to_string();
	assert!(err.contains("set_sidechain"), "{err}");
	let dup = lfo_on("cutoff", 0.1);
	assert!(
		exec_err(&fx.project, set(vec![dup.clone(), dup]))
			.to_string()
			.contains("unique")
	);
}

fn cc_map(cc: u8, target: ParamAddress, min: f32, max: f32) -> MidiMapping {
	MidiMapping {
		controller: Controller::Cc(cc),
		channel: None,
		target,
		min,
		max,
		mode: MapMode::Absolute,
		takeover: Takeover::Jump,
	}
}

#[test]
fn midi_map_validates_round_trips_and_follows_removed_targets() {
	let fx = fixture();
	let cutoff = ParamAddress::device(Some(fx.lead), fx.synth, "cutoff");
	let set = |map: Vec<MidiMapping>| Command::SetMidiMap { map };
	let mapped = round_trip(
		&fx.project,
		set(vec![
			cc_map(74, cutoff.clone(), 100.0, 8000.0),
			cc_map(7, ParamAddress::track_volume(fx.lead), -60.0, 6.0),
		]),
	);
	assert_eq!(mapped.midi_map.len(), 2);
	assert_eq!(mapped.midi_map[0].tag(), "CC 74");
	assert_eq!(mapped.midi_map[0].value_at(0.5), 4050.0);

	// JSON: omitted when empty, defaults left out, loads back.
	let plain = serde_json::to_string(&fx.project).unwrap();
	assert!(!plain.contains("midi_map"));
	let json = serde_json::to_string(&mapped).unwrap();
	assert!(json.contains("\"midi_map\"") && json.contains("\"cc74\""));
	assert!(!json.contains("\"mode\"") && !json.contains("takeover"));
	let back: Project = serde_json::from_str(&json).unwrap();
	assert_eq!(back, mapped);

	// Validation, with actionable errors.
	let mut twice = vec![cc_map(74, cutoff.clone(), 0.0, 1.0)];
	twice.push(cc_map(71, cutoff.clone(), 0.0, 1.0));
	assert!(
		exec_err(&fx.project, set(twice))
			.to_string()
			.contains("mapped twice")
	);
	let same_cc = vec![
		cc_map(74, cutoff.clone(), 0.0, 1.0),
		cc_map(74, ParamAddress::track_pan(fx.lead), -1.0, 1.0),
	];
	assert!(
		exec_err(&fx.project, set(same_cc))
			.to_string()
			.contains("already moves")
	);
	let mut other_channel = cc_map(74, ParamAddress::track_pan(fx.lead), -1.0, 1.0);
	other_channel.channel = Some(1);
	let mut first = cc_map(74, cutoff.clone(), 0.0, 1.0);
	first.channel = Some(0);
	round_trip(&fx.project, set(vec![first, other_channel]));
	let mut bad_channel = cc_map(74, cutoff.clone(), 0.0, 1.0);
	bad_channel.channel = Some(16);
	assert!(
		exec_err(&fx.project, set(vec![bad_channel]))
			.to_string()
			.contains("0..=15")
	);
	let mut relative_bend = cc_map(1, cutoff.clone(), 0.0, 1.0);
	relative_bend.controller = Controller::PitchBend;
	relative_bend.mode = MapMode::Relative;
	assert!(
		exec_err(&fx.project, set(vec![relative_bend]))
			.to_string()
			.contains("relative")
	);
	let loud = cc_map(7, ParamAddress::track_volume(fx.lead), -60.0, 40.0);
	exec_err(&fx.project, set(vec![loud]));
	let gone = ParamAddress::device(Some(fx.lead), DeviceId::new(), "cutoff");
	exec_err(&fx.project, set(vec![cc_map(1, gone, 0.0, 1.0)]));

	// Removing the device drops its assignment; undo restores both.
	let mut doc = Document::new(mapped.clone());
	doc.execute(Command::SetInstrument {
		track: fx.lead,
		device: None,
	})
	.unwrap();
	assert_eq!(doc.project().midi_map.len(), 1);
	doc.undo().unwrap();
	assert_eq!(doc.project(), &mapped);
	doc.execute(Command::RemoveTrack { track: fx.lead })
		.unwrap();
	assert!(doc.project().midi_map.is_empty());
	doc.undo().unwrap();
	assert_eq!(doc.project(), &mapped);
}
