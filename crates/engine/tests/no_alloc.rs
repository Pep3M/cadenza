//! The graph's process path (shared by offline and realtime) never touches the heap.

mod common;

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

use cadenza_engine::compile;
use common::*;

struct CountingAlloc;

static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

thread_local! {
	static TRACKING: Cell<bool> = const { Cell::new(false) };
}

/// Where the first tracked allocation happened (the counter is shared by the tests running
/// in parallel, so without it every test fails and none says why).
static FIRST: OnceLock<String> = OnceLock::new();

fn record() {
	if TRACKING.try_with(Cell::get).unwrap_or(false)
		&& ALLOCATIONS.fetch_add(1, Ordering::SeqCst) == 0
	{
		// Untracked while capturing: the backtrace allocates.
		TRACKING.with(|t| t.set(false));
		let trace = std::backtrace::Backtrace::force_capture().to_string();
		let _ = FIRST.set(trace);
		TRACKING.with(|t| t.set(true));
	}
}

#[track_caller]
fn assert_no_allocations() {
	let n = ALLOCATIONS.load(Ordering::SeqCst);
	assert!(
		n == 0,
		"{n} allocation(s) on a tracked thread; the first one:\n{}",
		FIRST.get().map_or("(no backtrace)", String::as_str)
	);
}

unsafe impl GlobalAlloc for CountingAlloc {
	unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
		record();
		unsafe { System.alloc(layout) }
	}

	unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
		record();
		unsafe { System.alloc_zeroed(layout) }
	}

	unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
		record();
		unsafe { System.realloc(ptr, layout, new_size) }
	}

	unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
		record();
		unsafe { System.dealloc(ptr, layout) }
	}
}

#[global_allocator]
static GLOBAL: CountingAlloc = CountingAlloc;

#[test]
fn graph_process_does_not_allocate() {
	let mut p = mini_song();
	// Key a compressor on the last track from the first one (pre-fader), so sidechain
	// delivery is covered too.
	let source = p.tracks[0].id;
	let last = p.tracks.len() - 1;
	p.tracks[last]
		.channel
		.inserts
		.push(cadenza_model::Device::builtin("compressor", "Duck").with_sidechain(source, true));
	// Reverb ducking (by its input on one track, keyed on another), a 4-tap ping-pong
	// delay and the 2x / 4x oversampled limiter (before the duck, which the
	// modulators below expect last).
	p.tracks[last].channel.inserts.splice(
		0..0,
		[
			cadenza_model::Device::builtin("reverb", "Ducked verb")
				.with_param("duck", 9.0)
				.with_sidechain(source, false),
			cadenza_model::Device::builtin("delay", "Taps")
				.with_param("taps", 4.0)
				.with_param("ping_pong", 1.0),
			cadenza_model::Device::builtin("limiter", "Limiter 2x").with_param("oversample", 1.0),
		],
	);
	p.tracks[1].channel.inserts.push(
		cadenza_model::Device::builtin("reverb", "Self-ducked verb").with_param("duck", 12.0),
	);
	p.master.inserts.push(
		cadenza_model::Device::builtin("limiter", "Limiter 4x").with_param("oversample", 2.0),
	);
	// Automate a send level and a device parameter too.
	let chords = &p.tracks[2];
	let send = chords.channel.sends[0].id;
	let keys = chords.instrument.as_ref().unwrap().id;
	let lanes = vec![cadenza_model::AutomationLane::new(
		cadenza_core::ParamAddress::send(chords.id, send),
		vec![
			cadenza_model::AutomationPoint::new(beats(0.0), -30.0),
			cadenza_model::AutomationPoint::new(beats(4.0), 0.0)
				.with_shape(cadenza_model::PointShape::Smooth),
			cadenza_model::AutomationPoint::new(beats(8.0), -6.0)
				.with_shape(cadenza_model::PointShape::Hold),
		],
	)];
	p.automation.extend(lanes.clone());
	// MIDI controller lanes (sustain, mod wheel, bend, pressure) on the chords clip, so
	// controller scheduling, chasing and delivery are covered too.
	{
		use cadenza_model::{Controller, ControllerLane, ControllerPoint};
		let lane = |controller, values: &[(f64, f32)]| ControllerLane {
			controller,
			points: values
				.iter()
				.map(|&(b, v)| ControllerPoint::new(beats(b), v))
				.collect(),
		};
		let clip = p.tracks[2].clips[0].clone().with_controllers([
			lane(Controller::SUSTAIN, &[(0.0, 1.0), (1.5, 0.0), (2.0, 1.0)]),
			lane(Controller::MOD_WHEEL, &[(0.0, 0.0), (0.5, 0.5), (1.0, 1.0)]),
			lane(Controller::PitchBend, &[(0.25, -0.5), (0.75, 0.5)]),
			lane(Controller::ChannelPressure, &[(0.1, 0.3)]),
		]);
		p.tracks[2].clips[0] = clip;
	}
	// Both samplers, with loops, choke groups and a pad on an extra output routed to a bus.
	let wav = write_sine_wav(
		"no-alloc-sample.wav",
		44_100,
		2,
		(hound::SampleFormat::Float, 32),
		220.0,
		0.5,
		0.4,
	);
	let mut looped = cadenza_model::SampleZone::full_keyboard(&wav, 57);
	looped.loop_mode = cadenza_model::LoopMode::PingPong;
	looped.loop_start = 0.1;
	looped.loop_crossfade = 0.02;
	let notes: Vec<(u8, f64, f64)> = (0..32)
		.map(|i| (36 + (i % 12) as u8, i as f64 * 0.25, 0.5))
		.collect();
	p.tracks.push(instrument_track(
		"Sampler",
		cadenza_model::Device::builtin("sampler", "Sampler")
			.with_param("filter_type", 1.0)
			.with_param("glide", 30.0)
			.with_zones(vec![looped]),
		&notes,
		8.0,
	));
	let mut pads: Vec<cadenza_model::SampleZone> = (36..48)
		.map(|n| cadenza_model::SampleZone::pad(&wav, n))
		.collect();
	pads[6].choke = 1;
	pads[10].choke = 1;
	pads[2].output = 3;
	pads[3].reverse = true;
	let pad_bus = bus("Pads");
	let mut drums = instrument_track(
		"Pads",
		cadenza_model::Device::builtin("drum_sampler", "Pads").with_zones(pads),
		&notes,
		8.0,
	);
	drums.instrument.as_mut().unwrap().aux_outputs = vec![cadenza_model::OutputRoute {
		port: 3,
		bus: Some(pad_bus.id),
	}];
	// Modulators: an envelope on the duck's key + a note-retriggered LFO, an LFO and an
	// output follower on the keys (whose first param is also moved live), a synced
	// transport-locked random LFO on the master insert.
	{
		use cadenza_model::{
			EnvelopeSource, LfoShape, ModTarget, Modulator, ModulatorKind, Retrigger,
		};
		let target = |param: &str, depth| ModTarget {
			param: param.into(),
			depth,
		};
		let mut key_env = Modulator::envelope(cadenza_core::ModulatorId::new());
		if let ModulatorKind::Envelope { source, .. } = &mut key_env.kind {
			*source = EnvelopeSource::Sidechain;
		}
		key_env.targets = vec![target("threshold", -0.2)];
		let mut retrig = Modulator::lfo(cadenza_core::ModulatorId::new());
		if let ModulatorKind::Lfo { retrigger, .. } = &mut retrig.kind {
			*retrigger = Retrigger::Note;
		}
		retrig.targets = vec![target("ratio", 0.1), target("threshold", 0.05)];
		p.tracks[last]
			.channel
			.inserts
			.last_mut()
			.unwrap()
			.modulators = vec![key_env, retrig];
		let first = cadenza_dsp::device_info("electric_piano").unwrap().params[0].id;
		let mut lfo = Modulator::lfo(cadenza_core::ModulatorId::new());
		lfo.targets = vec![target(first, 0.2)];
		let mut out_env = Modulator::envelope(cadenza_core::ModulatorId::new());
		out_env.targets = vec![target(first, -0.1)];
		p.tracks[2].instrument.as_mut().unwrap().modulators = vec![lfo, out_env];
		let mut random = Modulator::lfo(cadenza_core::ModulatorId::new());
		if let ModulatorKind::Lfo {
			shape,
			sync_beats,
			retrigger,
			..
		} = &mut random.kind
		{
			*shape = LfoShape::Random;
			*sync_beats = Some(0.5);
			*retrigger = Retrigger::Transport;
		}
		let master = &mut p.master.inserts[0];
		let param = cadenza_dsp::device_info(master.builtin_id().unwrap())
			.unwrap()
			.params[0]
			.id;
		random.targets = vec![target(param, 0.05)];
		master.modulators = vec![random];
	}
	let pads_instrument = drums.instrument.as_ref().unwrap().id;
	let sampler_instrument = p.tracks.last().unwrap().instrument.as_ref().unwrap().id;
	p.tracks.push(drums);
	p.tracks.push(pad_bus);
	// A wavetable poly synth (both oscillators, unison) with an LFO on the morph position.
	{
		use cadenza_model::Modulator;
		let mut morph = Modulator::lfo(cadenza_core::ModulatorId::new());
		morph.targets = vec![cadenza_model::ModTarget {
			param: "osc1_position".into(),
			depth: 0.4,
		}];
		let mut wavetable = cadenza_model::Device::builtin("poly_synth", "Wavetable")
			.with_param("osc1_wave", 4.0)
			.with_param("osc1_table", 2.0)
			.with_param("osc1_position", 0.3)
			.with_param("osc2_wave", 4.0)
			.with_param("osc2_table", 4.0)
			.with_param("unison", 3.0);
		wavetable.modulators = vec![morph];
		let notes: Vec<(u8, f64, f64)> = (0..16)
			.map(|i| (48 + (i * 5 % 40) as u8, i as f64 * 0.5, 1.0))
			.collect();
		p.tracks
			.push(instrument_track("Wavetable", wavetable, &notes, 8.0));
	}
	let mut graph = compile(&p, 48_000, 1024).unwrap();
	assert!(graph.warnings().is_empty(), "{:?}", graph.warnings());
	let mut left = vec![0.0f32; 1024];
	let mut right = vec![0.0f32; 1024];
	graph.play();
	assert!(graph.set_monitor_track(Some(source)));
	// The probe follows an insert, then an instrument, then a master insert.
	let probe = std::sync::Arc::new(cadenza_engine::DeviceProbe::new(48_000));
	graph.attach_probe(Some(probe.clone()));
	let duck = p.tracks[last].channel.inserts.last().unwrap().id;
	let master_insert = p.master.inserts[0].id;
	graph.set_probe_target(Some(duck));
	let mut probed = Vec::with_capacity(probe.capacity());
	// Live parameter changes and automation edits, prepared off the audio thread.
	let info = cadenza_dsp::device_info("electric_piano").unwrap();
	let param = info.params[0].id;
	let index = graph.param_index(keys, param).unwrap();
	let mut edited = graph.prepare_automation(&p.automation[..1], &p.tempo);

	TRACKING.with(|t| t.set(true));
	for (i, block) in [1usize, 37, 512, 1024]
		.into_iter()
		.cycle()
		.take(400)
		.enumerate()
	{
		// Live MIDI input (monitoring), including bursts beyond the buffer.
		let pitch = 48 + (i % 24) as u8;
		for _ in 0..if i % 50 == 0 { 600 } else { 1 } {
			graph.push_live_event(cadenza_dsp::MidiEvent::NoteOn {
				offset: 0,
				pitch,
				velocity: 0.7,
			});
		}
		graph.push_live_event(cadenza_dsp::MidiEvent::NoteOff { offset: 0, pitch });
		if i == 200 {
			graph.set_monitor_track(None);
		}
		if i == 250 {
			graph.set_monitor_track(Some(source));
		}
		// Knob drags and automation previews reach the running graph.
		assert!(graph.set_device_param(keys, index, i as f32 * 0.01));
		assert!(graph.set_send_level(send, -(i as f32 % 40.0)));
		if i % 100 == 0 {
			graph.swap_automation(&mut edited);
		}
		match i {
			100 => graph.set_probe_target(Some(keys)),
			// Voice readings of both samplers (playheads, held keys).
			150 => graph.set_probe_target(Some(sampler_instrument)),
			220 => graph.set_probe_target(Some(pads_instrument)),
			300 => graph.set_probe_target(Some(master_insert)),
			_ => {}
		}
		graph.process(&mut left[..block], &mut right[..block]);
		probed.clear();
		probe.drain(&mut probed);
		let _ = probe.readings();
		let _ = probe.modulation();
	}
	graph.seek(beats(3.0));
	graph.process(&mut left, &mut right);
	graph.stop();
	graph.process(&mut left, &mut right);
	graph.play();
	graph.process(&mut left, &mut right);
	TRACKING.with(|t| t.set(false));

	assert_no_allocations();
	assert!(probe.is_active());
	assert!(
		probe
			.reading(cadenza_dsp::readings::GAIN_REDUCTION_DB)
			.is_some()
	);
}

#[test]
fn metering_does_not_allocate() {
	let p = mini_song();
	let mut graph = compile(&p, 48_000, 1024).unwrap();
	let meters = graph.meters();
	let ids: Vec<_> = p.tracks.iter().map(|t| t.id).collect();
	let mut left = vec![0.0f32; 1024];
	let mut right = vec![0.0f32; 1024];
	graph.play();

	TRACKING.with(|t| t.set(true));
	let mut loudest = 0.0f32;
	for block in [1usize, 64, 333, 1024].into_iter().cycle().take(200) {
		graph.process(&mut left[..block], &mut right[..block]);
		// The UI side reads (and resets) the levels between callbacks.
		loudest = loudest.max(meters.master().max_peak());
		for &id in &ids {
			let _ = meters.track(id);
		}
	}
	graph.stop();
	graph.process(&mut left, &mut right);
	TRACKING.with(|t| t.set(false));

	assert_no_allocations();
	assert!(loudest > 0.0, "meters saw the song");
	assert_eq!(meters.master().rms, [0.0, 0.0]);
}

#[test]
fn stopped_note_preview_does_not_allocate() {
	let p = mini_song();
	let source = p.tracks[0].id;
	let mut graph = compile(&p, 48_000, 1024).unwrap();
	let mut left = vec![0.0f32; 1024];
	let mut right = vec![0.0f32; 1024];
	assert!(graph.set_monitor_track(Some(source)));

	TRACKING.with(|t| t.set(true));
	let mut loudest = 0.0f32;
	for (i, block) in [1usize, 37, 512, 1024]
		.into_iter()
		.cycle()
		.take(300)
		.enumerate()
	{
		let pitch = 48 + (i % 24) as u8;
		if i % 10 == 0 {
			graph.push_live_event(cadenza_dsp::MidiEvent::NoteOn {
				offset: 0,
				pitch,
				velocity: 0.8,
			});
		}
		if i % 10 == 5 {
			graph.push_live_event(cadenza_dsp::MidiEvent::AllNotesOff { offset: 0 });
		}
		graph.process(&mut left[..block], &mut right[..block]);
		loudest = loudest.max(left[..block].iter().fold(0.0f32, |m, x| m.max(x.abs())));
	}
	graph.seek(beats(2.0));
	graph.process(&mut left, &mut right);
	graph.play();
	graph.process(&mut left, &mut right);
	graph.stop();
	graph.process(&mut left, &mut right);
	TRACKING.with(|t| t.set(false));

	assert_no_allocations();
	assert!(loudest > 0.0, "the stopped preview sounded");
}

#[test]
fn looping_auditioning_and_rescheduling_do_not_allocate() {
	let mut p = mini_song();
	// Overlapping audio clips across the loop (automatic crossfade, micro-fades at the
	// clip edges and at every loop wrap).
	{
		let wav = write_sine_wav(
			"no-alloc-xfade.wav",
			48_000,
			1,
			(hound::SampleFormat::Float, 32),
			220.0,
			0.5,
			4.0,
		);
		let mut t = cadenza_model::Track::new("Loop audio", cadenza_model::TrackKind::Audio);
		t.clips.push(cadenza_model::Clip::audio(
			"a",
			beats(3.0),
			beats(3.0),
			&wav,
		));
		t.clips.push(cadenza_model::Clip::audio(
			"b",
			beats(5.5),
			beats(3.0),
			&wav,
		));
		p.tracks.push(t);
	}
	let chords = p.tracks[2].id;
	let mut graph = compile(&p, 48_000, 1024).unwrap();
	let mut left = vec![0.0f32; 1024];
	let mut right = vec![0.0f32; 1024];
	// Prepared off the audio thread: the audition mask and rescheduled clips (a note
	// removed, then restored).
	graph.set_audition(Some(&[chords]));
	let mut edited = p.tracks[2].clips.clone();
	if let cadenza_model::ClipContent::Midi { notes, .. } = &mut edited[0].content {
		notes.retain(|n| n.start.0 % (4 * PPQ_I) != 0 || n.pitch % 2 == 0);
	}
	let mut swaps: Vec<_> = (0..4)
		.map(|i| {
			let clips = if i % 2 == 0 {
				&edited
			} else {
				&p.tracks[2].clips
			};
			graph.prepare_schedule(chords, clips, &p.tempo).unwrap()
		})
		.collect();
	graph.play();

	TRACKING.with(|t| t.set(true));
	// A 1-bar loop that ends mid-quantum, wrapping many times.
	graph.set_loop(Some((beats(4.0), cadenza_core::Tick(8 * PPQ_I + 1))));
	graph.seek(beats(4.0));
	let mut loudest = 0.0f32;
	for (i, block) in [1usize, 37, 512, 1024]
		.into_iter()
		.cycle()
		.take(600)
		.enumerate()
	{
		if i % 150 == 75 {
			graph.swap_schedule(&mut swaps[i / 150]);
		}
		graph.process(&mut left[..block], &mut right[..block]);
		loudest = loudest.max(left[..block].iter().fold(0.0f32, |m, x| m.max(x.abs())));
	}
	graph.set_loop(None);
	graph.process(&mut left, &mut right);
	TRACKING.with(|t| t.set(false));

	graph.set_audition(None);
	TRACKING.with(|t| t.set(true));
	graph.process(&mut left, &mut right);
	TRACKING.with(|t| t.set(false));

	assert_no_allocations();
	assert!(loudest > 0.0, "the auditioned loop sounded");
}

#[test]
fn recording_with_input_monitoring_metronome_and_loops_does_not_allocate() {
	use cadenza_engine::{MetronomeSettings, Player, RecordSetup, RecordTrack};
	use cadenza_model::{MonitorMode, Track, TrackInput, TrackKind};

	let mut p = mini_song();
	let keys = p.tracks[2].id;
	let keys_instrument = p.tracks[2].instrument.as_ref().unwrap().id;
	let drums = p.tracks[0].id;
	let mut vox = Track::new("Vox", TrackKind::Audio);
	vox.input = Some(TrackInput {
		channel: 0,
		stereo: true,
		monitor: MonitorMode::Auto,
	});
	vox.channel
		.inserts
		.push(cadenza_model::Device::builtin("compressor", "Comp").with_param("key_hp", 150.0));
	// A dynamic band: detector, envelope and per-quantum coefficients on the audio thread;
	// it works on the side only, and a mid high-pass makes the EQ run in M/S.
	vox.channel.inserts.push(
		cadenza_model::Device::builtin("eq", "EQ")
			.with_param("peak3_gain", -6.0)
			.with_param("peak3_dyn", 1.0)
			.with_param("peak3_threshold", -40.0)
			.with_param("peak3_ms", 2.0)
			.with_param("hp_slope", 1.0)
			.with_param("hp_ms", 1.0),
	);
	let vox_id = vox.id;
	let vox_comp = vox.channel.inserts[0].id;
	p.tracks.push(vox);
	// MIDI learn: an absolute knob on the comp threshold, a relative encoder on the keys
	// fader, a pickup fader on the master pan (applied on the audio thread).
	let knob = |controller, target, min, max| cadenza_model::MidiMapping {
		controller,
		channel: None,
		target,
		min,
		max,
		mode: cadenza_model::MapMode::Absolute,
		takeover: cadenza_model::Takeover::Jump,
	};
	let mut encoder = knob(
		cadenza_model::Controller::Cc(16),
		cadenza_core::ParamAddress::track_volume(keys),
		-60.0,
		6.0,
	);
	encoder.mode = cadenza_model::MapMode::Relative;
	let mut fader = knob(
		cadenza_model::Controller::Cc(10),
		cadenza_core::ParamAddress {
			track: None,
			target: cadenza_core::ParamTarget::Pan,
		},
		-1.0,
		1.0,
	);
	fader.takeover = cadenza_model::Takeover::Pickup;
	p.midi_map = vec![
		knob(
			cadenza_model::Controller::Cc(74),
			cadenza_core::ParamAddress::device(Some(vox_id), vox_comp, "threshold"),
			-40.0,
			0.0,
		),
		encoder,
		fader,
	];
	let dir = std::env::temp_dir().join(format!("cadenza-noalloc-rec-{}", std::process::id()));
	let (mut player, mut dev) = Player::virtual_device(48_000, 2);
	player.load(&p).unwrap();
	let (mut midi_tx, midi_rx) = cadenza_midi_io::new_queue(256);
	player.set_midi_queue(midi_rx).unwrap();
	player.set_monitor_track(Some(keys)).unwrap();
	player.set_armed(vox_id, true).unwrap();
	player.set_armed(keys, true).unwrap();
	player
		.set_metronome(MetronomeSettings {
			on_play: true,
			on_record: true,
			level_db: -6.0,
		})
		.unwrap();
	player.set_loop(Some((beats(0.0), beats(2.0)))).unwrap();
	let mut setup = RecordSetup::new(
		&dir,
		beats(0.0),
		vec![
			RecordTrack {
				track: vox_id,
				name: "Vox".into(),
				input: Some(TrackInput::stereo(0)),
			},
			RecordTrack {
				track: keys,
				name: "Keys".into(),
				input: None,
			},
		],
	);
	setup.count_in_bars = 1;
	player.start_recording(&setup).unwrap();
	let comp = p.tracks.last().unwrap().channel.inserts[0].id;
	let eq = p.tracks.last().unwrap().channel.inserts[1].id;
	player.set_probe(Some(comp)).unwrap();
	let probe = player.probe().unwrap();
	let mut probed = Vec::with_capacity(probe.capacity());
	let master_tap = player.master_tap();
	let mut tapped = Vec::with_capacity(master_tap.capacity());
	let mut out = vec![0.0f32; 2 * 1024];
	let input: Vec<f32> = (0..2 * 1024)
		.map(|i| ((i as f32) * 0.01).sin() * 0.3)
		.collect();
	let note = |bytes: &[u8]| cadenza_midi_io::TimedMessage {
		host_micros: 0,
		message: cadenza_midi_io::RawMessage::from_bytes(bytes).unwrap(),
	};

	TRACKING.with(|t| t.set(true));
	// Count-in (one bar), then four passes of the two-beat loop, in odd block sizes.
	for (i, block) in [64usize, 256, 333, 1024]
		.into_iter()
		.cycle()
		.take(900)
		.enumerate()
	{
		if i % 40 == 0 {
			let _ = midi_tx.push(note(&[0x90, 60 + (i % 12) as u8, 100]));
		}
		if i % 40 == 20 {
			let _ = midi_tx.push(note(&[0x80, 60 + (i % 12) as u8, 0]));
		}
		if i % 7 == 3 {
			let _ = midi_tx.push(note(&[0xB0, 74, (i % 128) as u8]));
			let _ = midi_tx.push(note(&[0xB0, 16, if i % 2 == 0 { 2 } else { 126 }]));
			let _ = midi_tx.push(note(&[0xB0, 10, (i * 3 % 128) as u8]));
		}
		// A new map (built off the audio thread) swaps in; a fader set by hand.
		if i == 500 {
			TRACKING.with(|t| t.set(false));
			player.set_midi_map(&p).unwrap();
			TRACKING.with(|t| t.set(true));
		}
		// The control side drops the returned table here (not the audio thread).
		if i == 501 {
			TRACKING.with(|t| t.set(false));
			player.set_live_volume(Some(keys), -12.0).unwrap();
			TRACKING.with(|t| t.set(true));
		}
		// A piano-roll preview on another track takes the live input and gives it back.
		if i % 60 == 30 {
			player.preview_note_on(drums, 38, 0.9).unwrap();
		}
		if i % 60 == 35 {
			player.preview_note_off(drums, 38).unwrap();
		}
		if i == 450 {
			player.set_probe(Some(keys_instrument)).unwrap();
		}
		// Listen modes: the compressor's key, then an EQ band solo (replacing it), off.
		if i == 100 {
			player
				.set_listen(comp, cadenza_engine::Listen::Key)
				.unwrap();
		}
		if i == 300 {
			player
				.set_listen(eq, cadenza_engine::Listen::Band(3))
				.unwrap();
		}
		if i == 700 {
			player.set_listen(eq, cadenza_engine::Listen::Off).unwrap();
		}
		// Monitoring switches on the device output (mono + dim, then dim only).
		if i == 200 {
			player
				.set_monitor(cadenza_engine::MonitorFx {
					mono: true,
					dim: true,
				})
				.unwrap();
		}
		if i == 600 {
			player
				.set_monitor(cadenza_engine::MonitorFx {
					mono: false,
					dim: true,
				})
				.unwrap();
		}
		dev.process(&mut out[..2 * block], &input[..2 * block]);
		probed.clear();
		probe.drain(&mut probed);
		tapped.clear();
		master_tap.drain(&mut tapped);
	}
	TRACKING.with(|t| t.set(false));
	assert_no_allocations();
	assert!(probe.is_active());
	assert!(master_tap.is_active());

	player.request_stop_recording().unwrap();
	dev.process(&mut out, &input);
	let rec = player
		.finish_recording(std::time::Duration::from_secs(5))
		.unwrap();
	assert!(rec.passes.len() >= 3, "{:?}", rec.passes.len());
	assert_eq!(rec.audio[0].channels, 2);
	let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn note_audition_snippets_do_not_allocate() {
	let (mut player, mut dev) = cadenza_engine::Player::virtual_device(48_000, 0);
	player.load(&mini_song()).unwrap();
	let tone = |hz: f32| -> Vec<f32> {
		(0..22_050)
			.map(|n| (n as f32 * hz * std::f32::consts::TAU / 44_100.0).sin() * 0.5)
			.collect()
	};
	let mut out = vec![0.0f32; 2 * 256];
	let mut loudest = 0.0f32;
	let mut run = |dev: &mut cadenza_engine::VirtualDevice, blocks: usize| {
		TRACKING.with(|t| t.set(true));
		for _ in 0..blocks {
			dev.process(&mut out, &[]);
			loudest = loudest.max(out.iter().fold(0.0f32, |m, x| m.max(x.abs())));
		}
		TRACKING.with(|t| t.set(false));
	};
	// Stopped: the snippet plays over silence, a newer one replaces it (the old fades),
	// one is stopped, and finished ones are handed back.
	player.play_snippet(vec![tone(220.0)], 44_100, 0.0).unwrap();
	run(&mut dev, 20);
	player
		.play_snippet(vec![tone(330.0), tone(330.0)], 44_100, -6.0)
		.unwrap();
	run(&mut dev, 20);
	player.stop_snippet().unwrap();
	run(&mut dev, 20);
	player.play_snippet(vec![tone(440.0)], 44_100, 0.0).unwrap();
	run(&mut dev, 200);
	assert_no_allocations();
	assert!(loudest > 0.3, "the snippets sounded: {loudest}");
	// After it ended: silence again.
	dev.process(&mut out, &[]);
	assert!(out.iter().all(|x| *x == 0.0));
}

#[test]
fn effects_fed_midi_do_not_allocate() {
	let mut p = mini_song();
	let melody = p.tracks[0].id;
	let mut tune = cadenza_model::Device::builtin("pitch_correct", "Tune");
	tune.midi_input = Some(melody);
	p.tracks[1].channel.inserts.push(tune.clone());
	let mut master_tune = tune;
	master_tune.id = cadenza_core::DeviceId::new();
	p.master.inserts.push(master_tune);
	let mut graph = compile(&p, 48_000, 1024).unwrap();
	graph.set_loop(Some((beats(1.0), beats(3.0))));
	let mut left = vec![0.0f32; 1024];
	let mut right = vec![0.0f32; 1024];
	graph.play();
	TRACKING.with(|t| t.set(true));
	for block in [1usize, 37, 512, 1024].into_iter().cycle().take(400) {
		graph.process(&mut left[..block], &mut right[..block]);
	}
	graph.seek(beats(2.5));
	graph.process(&mut left, &mut right);
	graph.stop();
	graph.process(&mut left, &mut right);
	TRACKING.with(|t| t.set(false));
	assert_no_allocations();
}

#[test]
fn note_bends_do_not_allocate() {
	let mut p = mini_song();
	// Every note of the first track bent up and back, chased by the loop and a seek.
	for clip in &mut p.tracks[0].clips {
		if let cadenza_model::ClipContent::Midi { notes, .. } = &mut clip.content {
			for n in notes.iter_mut() {
				n.bend = (0..8)
					.map(|i| cadenza_model::NoteBendPoint {
						tick: cadenza_core::Tick(n.length.0 * i / 8),
						semitones: if i % 2 == 0 { 0.0 } else { 1.5 },
					})
					.collect();
			}
		}
	}
	let mut graph = compile(&p, 48_000, 1024).unwrap();
	graph.set_loop(Some((beats(1.0), beats(3.0))));
	let mut left = vec![0.0f32; 1024];
	let mut right = vec![0.0f32; 1024];
	graph.play();
	TRACKING.with(|t| t.set(true));
	for block in [1usize, 37, 512, 1024].into_iter().cycle().take(400) {
		graph.process(&mut left[..block], &mut right[..block]);
	}
	graph.seek(beats(2.5));
	graph.process(&mut left, &mut right);
	graph.stop();
	graph.process(&mut left, &mut right);
	TRACKING.with(|t| t.set(false));
	assert_no_allocations();
}
