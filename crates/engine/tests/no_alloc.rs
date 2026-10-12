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
