//! The graph's process path (shared by offline and realtime) never touches the heap.

mod common;

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};

use cadenza_engine::compile;
use common::*;

struct CountingAlloc;

static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

thread_local! {
	static TRACKING: Cell<bool> = const { Cell::new(false) };
}

fn record() {
	if TRACKING.try_with(Cell::get).unwrap_or(false) {
		ALLOCATIONS.fetch_add(1, Ordering::SeqCst);
	}
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
	p.tracks.push(drums);
	p.tracks.push(pad_bus);
	let mut graph = compile(&p, 48_000, 1024).unwrap();
	assert!(graph.warnings().is_empty(), "{:?}", graph.warnings());
	let mut left = vec![0.0f32; 1024];
	let mut right = vec![0.0f32; 1024];
	graph.play();
	assert!(graph.set_monitor_track(Some(source)));
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
		graph.process(&mut left[..block], &mut right[..block]);
	}
	graph.seek(beats(3.0));
	graph.process(&mut left, &mut right);
	graph.stop();
	graph.process(&mut left, &mut right);
	graph.play();
	graph.process(&mut left, &mut right);
	TRACKING.with(|t| t.set(false));

	assert_eq!(ALLOCATIONS.load(Ordering::SeqCst), 0);
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

	assert_eq!(ALLOCATIONS.load(Ordering::SeqCst), 0);
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

	assert_eq!(ALLOCATIONS.load(Ordering::SeqCst), 0);
	assert!(loudest > 0.0, "the stopped preview sounded");
}

#[test]
fn looping_auditioning_and_rescheduling_do_not_allocate() {
	let p = mini_song();
	let chords = p.tracks[2].id;
	let mut graph = compile(&p, 48_000, 1024).unwrap();
	let mut left = vec![0.0f32; 1024];
	let mut right = vec![0.0f32; 1024];
	// Prepared off the audio thread: the audition mask and rescheduled clips (a note
	// removed, then restored).
	graph.set_audition(Some(&[chords]));
	let mut edited = p.tracks[2].clips.clone();
	if let cadenza_model::ClipContent::Midi { notes } = &mut edited[0].content {
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

	assert_eq!(ALLOCATIONS.load(Ordering::SeqCst), 0);
	assert!(loudest > 0.0, "the auditioned loop sounded");
}

#[test]
fn recording_with_input_monitoring_metronome_and_loops_does_not_allocate() {
	use cadenza_engine::{MetronomeSettings, Player, RecordSetup, RecordTrack};
	use cadenza_model::{MonitorMode, Track, TrackInput, TrackKind};

	let mut p = mini_song();
	let keys = p.tracks[2].id;
	let mut vox = Track::new("Vox", TrackKind::Audio);
	vox.input = Some(TrackInput {
		channel: 0,
		stereo: true,
		monitor: MonitorMode::Auto,
	});
	vox.channel
		.inserts
		.push(cadenza_model::Device::builtin("compressor", "Comp"));
	let vox_id = vox.id;
	p.tracks.push(vox);
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
		dev.process(&mut out[..2 * block], &input[..2 * block]);
	}
	TRACKING.with(|t| t.set(false));
	assert_eq!(ALLOCATIONS.load(Ordering::SeqCst), 0);

	player.request_stop_recording().unwrap();
	dev.process(&mut out, &input);
	let rec = player
		.finish_recording(std::time::Duration::from_secs(5))
		.unwrap();
	assert!(rec.passes.len() >= 3, "{:?}", rec.passes.len());
	assert_eq!(rec.audio[0].channels, 2);
	let _ = std::fs::remove_dir_all(&dir);
}
