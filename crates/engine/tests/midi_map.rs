//! MIDI learn on the audio thread: mapped controllers move their targets live (virtual
//! device + a MIDI queue instead of hardware) and never reach the instrument.

use cadenza_core::{ParamAddress, TrackId};
use cadenza_engine::{Player, VirtualDevice};
use cadenza_midi_io::{RawMessage, TimedMessage};
use cadenza_model::{
	Controller, Device, MapMode, MidiMapping, Project, Takeover, Track, TrackKind,
};

const SR: u32 = 48_000;
const BLOCK: usize = 256;

fn send(tx: &mut rtrb::Producer<TimedMessage>, bytes: &[u8]) {
	tx.push(TimedMessage {
		host_micros: 0,
		message: RawMessage::from_bytes(bytes).unwrap(),
	})
	.unwrap();
}

fn map(controller: Controller, target: ParamAddress, min: f32, max: f32) -> MidiMapping {
	MidiMapping {
		controller,
		channel: None,
		target,
		min,
		max,
		mode: MapMode::Absolute,
		takeover: Takeover::Jump,
	}
}

struct Rig {
	player: Player,
	dev: VirtualDevice,
	tx: rtrb::Producer<TimedMessage>,
	track: TrackId,
	gain: cadenza_core::DeviceId,
	project: Project,
}

/// A synth (short release) through a gain insert, monitored and playing.
fn rig(mappings: impl FnOnce(TrackId, cadenza_core::DeviceId) -> Vec<MidiMapping>) -> Rig {
	let mut p = Project::new("map");
	let mut t = Track::new("Keys", TrackKind::Instrument).with_instrument(
		Device::builtin("poly_synth", "Synth")
			.with_param("amp_release", 0.01)
			.with_param("amp_attack", 0.001),
	);
	let gain = Device::builtin("gain", "Gain");
	let gain_id = gain.id;
	t.channel.inserts.push(gain);
	let track = t.id;
	p.tracks.push(t);
	p.midi_map = mappings(track, gain_id);
	let (mut player, dev) = Player::virtual_device(SR, 0);
	player.load(&p).unwrap();
	let (tx, rx) = cadenza_midi_io::new_queue(64);
	player.set_midi_queue(rx).unwrap();
	player.set_monitor_track(Some(track)).unwrap();
	player.play().unwrap();
	Rig {
		player,
		dev,
		tx,
		track,
		gain: gain_id,
		project: p,
	}
}

/// Peak of the next `blocks` blocks.
fn run(dev: &mut VirtualDevice, blocks: usize) -> f32 {
	let mut out = vec![0.0f32; BLOCK * 2];
	let mut peak = 0.0f32;
	for _ in 0..blocks {
		dev.process(&mut out, &[]);
		peak = out.iter().fold(peak, |m, v| m.max(v.abs()));
	}
	peak
}

#[test]
fn a_mapped_cc_moves_a_param_live_and_the_monitor_shows_it() {
	let mut r = rig(|track, gain| {
		vec![map(
			Controller::Cc(74),
			ParamAddress::device(Some(track), gain, "gain"),
			-60.0,
			0.0,
		)]
	});
	let monitor = r.player.midi_map_monitor();
	send(&mut r.tx, &[0x90, 60, 110]);
	let loud = run(&mut r.dev, 40);
	assert!(loud > 0.05, "the note plays: {loud}");
	send(&mut r.tx, &[0xB0, 74, 0]);
	run(&mut r.dev, 20);
	let quiet = run(&mut r.dev, 20);
	assert!(quiet < loud * 0.01, "gain at -60 dB: {quiet} vs {loud}");
	let (seq, value) = monitor.value(0);
	assert!(seq > 0 && value == -60.0, "{seq} {value}");
	let last = monitor.last_controller().unwrap();
	assert_eq!((last.controller, last.channel), (Controller::Cc(74), 0));
	send(&mut r.tx, &[0xB0, 74, 127]);
	run(&mut r.dev, 20);
	assert!(run(&mut r.dev, 20) > loud * 0.5, "back up");
	assert_eq!(monitor.value(0).1, 0.0);
	let _ = r.track;
}

#[test]
fn a_mapped_sustain_pedal_does_not_reach_the_instrument() {
	// The pedal moves the gain instead of holding the note.
	let mut r = rig(|track, gain| {
		vec![map(
			Controller::SUSTAIN,
			ParamAddress::device(Some(track), gain, "gain"),
			-6.0,
			0.0,
		)]
	});
	send(&mut r.tx, &[0x90, 60, 110]);
	run(&mut r.dev, 20);
	send(&mut r.tx, &[0xB0, 64, 127]);
	run(&mut r.dev, 4);
	send(&mut r.tx, &[0x80, 60, 0]);
	run(&mut r.dev, 40);
	let after = run(&mut r.dev, 10);
	assert!(after < 1e-4, "released without the pedal: {after}");

	// Unmapped, the same pedal sustains.
	r.project.midi_map.clear();
	r.player.set_midi_map(&r.project).unwrap();
	send(&mut r.tx, &[0x90, 60, 110]);
	run(&mut r.dev, 20);
	send(&mut r.tx, &[0xB0, 64, 127]);
	run(&mut r.dev, 4);
	send(&mut r.tx, &[0x80, 60, 0]);
	run(&mut r.dev, 40);
	assert!(run(&mut r.dev, 10) > 0.01, "held by the pedal");
	let _ = r.gain;
}

#[test]
fn relative_encoders_step_from_the_current_value() {
	let mut r = rig(|track, _| {
		let mut m = map(
			Controller::Cc(16),
			ParamAddress::track_volume(track),
			-64.0,
			0.0,
		);
		m.mode = MapMode::Relative;
		vec![m]
	});
	let monitor = r.player.midi_map_monitor();
	// The UI set the fader to -10 dB: steps start there.
	r.player.set_live_volume(Some(r.track), -10.0).unwrap();
	run(&mut r.dev, 1);
	for _ in 0..4 {
		send(&mut r.tx, &[0xB0, 16, 1]);
	}
	run(&mut r.dev, 1);
	assert_eq!(monitor.value(0).1, -8.0);
	send(&mut r.tx, &[0xB0, 16, 127 - 9]); // -10 steps
	run(&mut r.dev, 1);
	assert_eq!(monitor.value(0).1, -13.0);
	for _ in 0..30 {
		send(&mut r.tx, &[0xB0, 16, 10]);
	}
	run(&mut r.dev, 1);
	assert_eq!(monitor.value(0).1, 0.0, "clamped to max");
}
