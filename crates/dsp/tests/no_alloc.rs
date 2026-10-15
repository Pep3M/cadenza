//! Verifies that `Processor::process` never touches the heap, for every
//! registry device, across block sizes, events and parameter changes.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};

use cadenza_dsp::{
	DeviceRole, LoopMode, MidiEvent, SampleBuffer, Transport, Zone, create, registry, uses_samples,
};

struct CountingAlloc;

static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

thread_local! {
	static TRACKING: Cell<bool> = const { Cell::new(false) };
}

fn note(tracking_allocations: bool) {
	if tracking_allocations {
		ALLOCATIONS.fetch_add(1, Ordering::SeqCst);
	}
}

fn tracking() -> bool {
	TRACKING.try_with(Cell::get).unwrap_or(false)
}

unsafe impl GlobalAlloc for CountingAlloc {
	unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
		note(tracking());
		unsafe { System.alloc(layout) }
	}

	unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
		note(tracking());
		unsafe { System.alloc_zeroed(layout) }
	}

	unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
		note(tracking());
		unsafe { System.realloc(ptr, layout, new_size) }
	}

	unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
		note(tracking());
		unsafe { System.dealloc(ptr, layout) }
	}
}

#[global_allocator]
static GLOBAL: CountingAlloc = CountingAlloc;

fn counted<R>(f: impl FnOnce() -> R) -> (R, usize) {
	let before = ALLOCATIONS.load(Ordering::SeqCst);
	TRACKING.with(|t| t.set(true));
	let r = f();
	TRACKING.with(|t| t.set(false));
	(r, ALLOCATIONS.load(Ordering::SeqCst) - before)
}

#[test]
fn process_and_set_param_never_allocate() {
	// The counter itself must work, or the assertion below proves nothing.
	let (probe, n) = counted(|| Vec::<u8>::with_capacity(16));
	assert!(n > 0 && probe.capacity() >= 16);

	let max_block = 1024;
	let mut left = vec![0.0_f32; max_block];
	let mut right = vec![0.0_f32; max_block];
	let events: Vec<MidiEvent> = (0..12)
		.map(|i| {
			let pitch = [36, 38, 42, 46, 49, 51, 60, 64, 67, 72, 39, 45][i];
			MidiEvent::NoteOn {
				offset: i as u32,
				pitch,
				velocity: 0.8,
			}
		})
		.chain([
			MidiEvent::ControlChange {
				offset: 14,
				controller: 64,
				value: 1.0,
			},
			MidiEvent::ControlChange {
				offset: 15,
				controller: 1,
				value: 0.7,
			},
			MidiEvent::PitchBend {
				offset: 16,
				value: -0.4,
			},
			MidiEvent::ChannelPressure {
				offset: 17,
				value: 0.6,
			},
			MidiEvent::NoteOff {
				offset: 20,
				pitch: 60,
			},
			MidiEvent::ControlChange {
				offset: 25,
				controller: 64,
				value: 0.0,
			},
			MidiEvent::AllNotesOff { offset: 30 },
		])
		.collect();
	let transport = Transport {
		bpm: 97.0,
		playing: true,
		beat_position: 0.0,
		..Default::default()
	};

	for info in registry() {
		let mut device = create(info.id).unwrap();
		device.prepare(48_000.0, max_block);
		if uses_samples(info.id) {
			assert!(device.set_zones(zones()));
		}
		let mut total = 0;
		for (round, &block) in [1usize, 7, 64, 333, 1024, 128].iter().enumerate() {
			for (l, r) in left.iter_mut().zip(right.iter_mut()) {
				let x = if info.role == DeviceRole::Effect {
					0.5
				} else {
					0.0
				};
				*l = x;
				*r = -x;
			}
			let ev: &[MidiEvent] = if round == 2 { &events } else { &events[..2] };
			let ((), n) = counted(|| {
				for (i, p) in info.params.iter().enumerate() {
					let v = if round % 2 == 0 { p.max } else { p.min };
					device.set_param(i, v);
				}
				for _ in 0..4 {
					device.process(&mut left[..block], &mut right[..block], ev, &transport);
				}
				let _ = device.latency();
				let _ = device.tail_seconds();
			});
			total += n;
		}
		assert_eq!(
			total, 0,
			"{} allocated {total} times on the audio thread",
			info.id
		);
	}
}

/// Every playback path: loops (forward with crossfade, ping-pong), reverse, choke groups,
/// round robin, velocity layers, aux outputs, a missing sample, mono and stereo data at
/// another rate (resampled reads, sinc when transposing up).
fn zones() -> Vec<Zone> {
	let tone = |sr: f32, channels: usize| {
		let frames = (sr * 0.3) as usize;
		let ch: Vec<Vec<f32>> = (0..channels)
			.map(|c| {
				(0..frames)
					.map(|n| ((n as f32 * 0.03) + c as f32).sin() * 0.5)
					.collect()
			})
			.collect();
		SampleBuffer::new(sr, ch).shared()
	};
	let mono = tone(44_100.0, 1);
	let stereo = tone(48_000.0, 2);
	let mut z = Vec::new();
	let mut looped = Zone::new(mono.clone(), 0, 127, 60);
	looped.loop_mode = LoopMode::Forward;
	looped.loop_start = 0.05;
	looped.loop_end = Some(0.25);
	looped.loop_crossfade = 0.02;
	looped.vel_high = 0.85;
	z.push(looped);
	let mut pp = Zone::new(stereo.clone(), 0, 127, 48);
	pp.loop_mode = LoopMode::PingPong;
	pp.loop_start = 0.1;
	pp.vel_low = 0.5;
	pp.round_robin = 1;
	pp.output = 2;
	z.push(pp);
	let mut rev = Zone::new(stereo.clone(), 30, 60, 40);
	rev.reverse = true;
	rev.round_robin = 1;
	rev.choke = 1;
	rev.decay_ms = Some(80.0);
	rev.attack_ms = 3.0;
	z.push(rev);
	let mut choke = Zone::new(mono, 42, 46, 44);
	choke.choke = 1;
	choke.output = 5;
	z.push(choke);
	z.push(Zone {
		key_low: 60,
		key_high: 72,
		..Zone::default()
	});
	z
}
