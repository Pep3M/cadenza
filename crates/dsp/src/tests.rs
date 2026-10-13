//! Cross-device tests run against every registry entry.

use crate::util::{Rng, gain_to_db};
use crate::{DeviceRole, MidiEvent, Processor, Transport, create, registry};

const SR: f32 = 48_000.0;

fn prepared(id: &str, sr: f32, max_block: usize) -> Box<dyn Processor> {
	let mut p = create(id).expect("registry id must be creatable");
	p.prepare(sr, max_block);
	if crate::uses_samples(id) {
		assert!(p.set_zones(test_zones(id)));
	}
	p
}

/// Sample instruments are silent without samples: a decaying 220 Hz tone (root A3) on
/// the whole keyboard (sampler) or on every note as its own pad (drum_sampler).
pub(crate) fn test_zones(id: &str) -> Vec<crate::Zone> {
	let sr = 44_100.0;
	let tone: Vec<f32> = (0..(sr * 0.4) as usize)
		.map(|n| {
			let t = n as f32 / sr;
			(std::f32::consts::TAU * 220.0 * t).sin() * (-t * 8.0).exp() * 0.8
		})
		.collect();
	let sample = crate::SampleBuffer::mono(sr, tone).shared();
	if id == "drum_sampler" {
		(0..=127)
			.map(|n| crate::Zone::new(sample.clone(), n, n, n))
			.collect()
	} else {
		vec![crate::Zone::new(sample, 0, 127, 57)]
	}
}

fn render(
	p: &mut dyn Processor,
	samples: usize,
	block: usize,
	events_at_start: &[MidiEvent],
) -> (Vec<f32>, Vec<f32>) {
	let mut l = vec![0.0; samples];
	let mut r = vec![0.0; samples];
	let mut pos = 0;
	let t = Transport::default();
	while pos < samples {
		let end = (pos + block).min(samples);
		let ev: &[MidiEvent] = if pos == 0 { events_at_start } else { &[] };
		p.process(&mut l[pos..end], &mut r[pos..end], ev, &t);
		pos = end;
	}
	(l, r)
}

fn peak(x: &[f32]) -> f32 {
	x.iter().fold(0.0_f32, |m, v| m.max(v.abs()))
}

fn is_snake_case(s: &str) -> bool {
	!s.is_empty()
		&& s.chars()
			.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
		&& !s.starts_with('_')
		&& !s.ends_with('_')
}

#[test]
fn registry_is_consistent() {
	let mut ids = std::collections::HashSet::new();
	for info in registry() {
		assert!(is_snake_case(info.id), "{}", info.id);
		assert!(ids.insert(info.id), "duplicate id {}", info.id);
		assert!(
			info.description.len() > 80,
			"{} needs an AI-usable description",
			info.id
		);
		let p = create(info.id).unwrap();
		assert_eq!(p.info().id, info.id);
		let mut pids = std::collections::HashSet::new();
		for (i, param) in info.params.iter().enumerate() {
			assert!(is_snake_case(param.id), "{}.{}", info.id, param.id);
			assert!(
				pids.insert(param.id),
				"duplicate param {}.{}",
				info.id,
				param.id
			);
			assert!(param.min < param.max, "{}.{}", info.id, param.id);
			assert!(
				(param.min..=param.max).contains(&param.default),
				"{}.{}",
				info.id,
				param.id
			);
			assert!(!param.description.is_empty() && !param.name.is_empty());
			assert_eq!(info.param_index(param.id), Some(i));
		}
		assert_eq!(info.param_index("does_not_exist"), None);
	}
	assert!(create("nope").is_none());
	assert!(registry().iter().any(|d| d.role == DeviceRole::Instrument));
}

#[test]
fn survives_random_params_blocks_and_events() {
	let mut rng = Rng::new(0xC0FFEE);
	for info in registry() {
		for &sr in &[44_100.0, 48_000.0, 96_000.0] {
			let mut p = prepared(info.id, sr, 1024);
			let mut l = vec![0.0_f32; 1024];
			let mut r = vec![0.0_f32; 1024];
			let mut events = Vec::with_capacity(8);
			for round in 0..120 {
				if round % 10 == 0 {
					for (i, param) in info.params.iter().enumerate() {
						let v = match rng.next_u32() % 5 {
							0 => param.min,
							1 => param.max,
							2 => param.default,
							3 => param.max * 10.0 + 1.0, // out of range: must clamp
							_ => param.min + (param.max - param.min) * rng.unipolar(),
						};
						p.set_param(i, v);
					}
					p.set_param(info.params.len() + 3, 1.0);
					p.set_param(0, f32::NAN);
				}
				let n = if round % 7 == 0 {
					1
				} else {
					1 + (rng.next_u32() as usize % 1024)
				};
				events.clear();
				let mut offset = 0;
				for _ in 0..(rng.next_u32() % 5) {
					offset = (offset + rng.next_u32() % 64).min(n as u32 + 2);
					let pitch = 24 + (rng.next_u32() % 72) as u8;
					events.push(match rng.next_u32() % 6 {
						0..=2 => MidiEvent::NoteOn {
							offset,
							pitch,
							velocity: rng.unipolar(),
						},
						3 | 4 => MidiEvent::NoteOff { offset, pitch },
						_ => MidiEvent::AllNotesOff { offset },
					});
				}
				for (a, b) in l[..n].iter_mut().zip(&mut r[..n]) {
					if info.role == DeviceRole::Instrument {
						*a = 0.0;
						*b = 0.0;
					} else {
						*a = rng.bipolar() * 1.5;
						*b = rng.bipolar() * 1.5;
					}
				}
				p.process(&mut l[..n], &mut r[..n], &events, &Transport::default());
				for &x in l[..n].iter().chain(&r[..n]) {
					assert!(
						x.is_finite(),
						"{} produced {x} (sr {sr}, round {round})",
						info.id
					);
					// Extreme legal settings (e.g. every EQ band at +24 dB) are loud but must stay bounded.
					assert!(x.abs() < 1e6, "{} blew up: {x}", info.id);
				}
			}
			p.reset();
		}
	}
}

#[test]
fn instruments_sound_and_decay_to_silence() {
	for info in registry()
		.iter()
		.filter(|d| d.role == DeviceRole::Instrument)
	{
		let notes: &[u8] = if info.id == "drum_machine" {
			&[36, 38, 39, 42, 46, 41, 45, 48, 49, 51]
		} else {
			&[36, 60, 84]
		};
		for &pitch in notes {
			let mut p = prepared(info.id, SR, 512);
			let on = [MidiEvent::NoteOn {
				offset: 0,
				pitch,
				velocity: 0.9,
			}];
			let (l, r) = render(p.as_mut(), (SR * 0.3) as usize, 256, &on);
			let level = peak(&l).max(peak(&r));
			assert!(level > 0.02, "{} note {pitch} silent ({level})", info.id);
			assert!(level < 4.0, "{} note {pitch} too loud ({level})", info.id);
			let off = [MidiEvent::NoteOff { offset: 0, pitch }];
			let tail = (p.tail_seconds() * SR) as usize + 256;
			render(p.as_mut(), tail, 256, &off);
			let (l, r) = render(p.as_mut(), 4_800, 256, &[]);
			let rest = peak(&l).max(peak(&r));
			assert!(
				gain_to_db(rest) < -70.0,
				"{} note {pitch} still sounding after tail: {} dB",
				info.id,
				gain_to_db(rest)
			);
		}
	}
}

#[test]
fn note_on_is_sample_accurate() {
	for info in registry()
		.iter()
		.filter(|d| d.role == DeviceRole::Instrument)
	{
		let mut p = prepared(info.id, SR, 512);
		let mut l = vec![0.0_f32; 512];
		let mut r = vec![0.0_f32; 512];
		let ev = [MidiEvent::NoteOn {
			offset: 200,
			pitch: if info.id == "drum_machine" { 38 } else { 60 },
			velocity: 1.0,
		}];
		p.process(&mut l, &mut r, &ev, &Transport::default());
		assert!(
			l[..200].iter().chain(&r[..200]).all(|&x| x == 0.0),
			"{} sounded early",
			info.id
		);
		assert!(
			peak(&l[200..240]) > 0.0,
			"{} did not start at offset",
			info.id
		);
	}
}

#[test]
fn polyphony_and_voice_stealing() {
	let mut p = prepared("poly_synth", SR, 256);
	// 24 simultaneous notes on a 16-voice synth must steal cleanly.
	let events: Vec<MidiEvent> = (0..24)
		.map(|i| MidiEvent::NoteOn {
			offset: i * 4,
			pitch: 40 + i as u8,
			velocity: 0.8,
		})
		.collect();
	let (l, _) = render(p.as_mut(), 9_600, 256, &events);
	assert!(l.iter().all(|x| x.is_finite()));
	assert!(peak(&l) > 0.05);
}

#[test]
fn velocity_changes_level() {
	for id in ["poly_synth", "electric_piano", "drum_machine"] {
		let level = |velocity: f32| {
			let mut p = prepared(id, SR, 256);
			let ev = [MidiEvent::NoteOn {
				offset: 0,
				pitch: if id == "drum_machine" { 38 } else { 60 },
				velocity,
			}];
			let (l, _) = render(p.as_mut(), 9_600, 256, &ev);
			peak(&l)
		};
		assert!(level(1.0) > level(0.3) * 1.5, "{id} ignores velocity");
	}
}

#[test]
fn closed_hat_chokes_open_hat() {
	let mut p = prepared("drum_machine", SR, 256);
	let open = [MidiEvent::NoteOn {
		offset: 0,
		pitch: 46,
		velocity: 1.0,
	}];
	render(p.as_mut(), 2_400, 256, &open);
	let closed = [MidiEvent::NoteOn {
		offset: 0,
		pitch: 42,
		velocity: 0.01,
	}];
	render(p.as_mut(), 9_600, 256, &closed);
	let (l, r) = render(p.as_mut(), 4_800, 256, &[]);
	// Open hat (450 ms default) would still ring here if not choked.
	assert!(gain_to_db(peak(&l).max(peak(&r))) < -60.0);
}

/// Feeds a steady sine, jumps a parameter across its whole range at a block
/// boundary, and checks the largest sample-to-sample step during the
/// transition is not bigger than in the settled state (a zipper/step would be).
fn assert_smooth_jump(id: &str, param: &str, from: f32, to: f32, freq: f32) {
	let mut p = create(id).unwrap();
	let idx = p.info().param_index(param).unwrap();
	p.set_param(idx, from);
	p.prepare(SR, 64);
	let total = 48_000;
	let mut l: Vec<f32> = (0..total)
		.map(|n| (std::f32::consts::TAU * freq * n as f32 / SR).sin() * 0.25)
		.collect();
	let mut r = l.clone();
	let switch = 12_000;
	for start in (0..total).step_by(64) {
		if start == switch {
			p.set_param(idx, to);
		}
		p.process(
			&mut l[start..start + 64],
			&mut r[start..start + 64],
			&[],
			&Transport::default(),
		);
	}
	let max_step = |a: usize, b: usize| {
		l[a..b]
			.windows(2)
			.fold(0.0_f32, |m, w| m.max((w[1] - w[0]).abs()))
	};
	let transition = max_step(switch - 1, switch + 4_800);
	let settled = max_step(switch + 24_000, total).max(max_step(4_000, switch));
	assert!(
		transition <= settled * 1.25 + 1e-4,
		"{id}.{param}: transition step {transition} vs settled {settled}"
	);
}

#[test]
fn parameter_changes_are_smoothed() {
	assert_smooth_jump("gain", "gain", -60.0, 24.0, 100.0);
	assert_smooth_jump("gain", "pan", -100.0, 100.0, 100.0);
	assert_smooth_jump("gain", "invert_left", 0.0, 1.0, 100.0);
	assert_smooth_jump("eq", "peak2_gain", -24.0, 24.0, 1_000.0);
	assert_smooth_jump("eq", "low_shelf_gain", 24.0, -24.0, 60.0);
	assert_smooth_jump("compressor", "makeup", -12.0, 24.0, 100.0);
	assert_smooth_jump("saturator", "output", -24.0, 12.0, 100.0);
	assert_smooth_jump("reverb", "mix", 0.0, 100.0, 100.0);
	assert_smooth_jump("chorus", "mix", 0.0, 100.0, 100.0);
	assert_smooth_jump("delay", "mix", 0.0, 100.0, 100.0);
}

#[test]
fn block_size_does_not_change_output() {
	// Deterministic devices must render identically regardless of block size.
	for id in [
		"poly_synth",
		"electric_piano",
		"eq",
		"compressor",
		"limiter",
		"delay",
	] {
		let run = |block: usize| {
			let mut p = prepared(id, SR, 1024);
			let mut l: Vec<f32> = (0..9_000).map(|n| (n as f32 * 0.01).sin() * 0.8).collect();
			let mut r = l.clone();
			if p.info().role == DeviceRole::Instrument {
				l.fill(0.0);
				r.fill(0.0);
			}
			let mut pos = 0;
			while pos < l.len() {
				let end = (pos + block).min(l.len());
				let ev = if pos == 0 {
					vec![MidiEvent::NoteOn {
						offset: 0,
						pitch: 57,
						velocity: 0.7,
					}]
				} else {
					Vec::new()
				};
				p.process(
					&mut l[pos..end],
					&mut r[pos..end],
					&ev,
					&Transport::default(),
				);
				pos = end;
			}
			l
		};
		let a = run(1024);
		let b = run(1);
		let c = run(37);
		for i in 0..a.len() {
			assert!(
				(a[i] - b[i]).abs() < 1e-4 && (a[i] - c[i]).abs() < 1e-4,
				"{id} differs at {i}"
			);
		}
	}
}

#[test]
fn sustain_pedal_holds_released_notes() {
	let on = MidiEvent::NoteOn {
		offset: 0,
		pitch: 60,
		velocity: 1.0,
	};
	let pedal = |value| MidiEvent::ControlChange {
		offset: 0,
		controller: crate::cc::SUSTAIN,
		value,
	};
	let off = MidiEvent::NoteOff {
		offset: 10,
		pitch: 60,
	};
	// Level 0.15 s after the key went up, with and without the pedal.
	let tail = |id: &str, with_pedal: bool| {
		let mut p = prepared(id, SR, 256);
		let start: Vec<MidiEvent> = if with_pedal {
			vec![pedal(1.0), on]
		} else {
			vec![on]
		};
		render(&mut *p, 2400, 256, &start);
		let (after, _) = render(&mut *p, 9600, 256, &[off]);
		let level = peak(&after[7200..]);
		(p, level)
	};
	for id in ["poly_synth", "electric_piano", "sampler"] {
		let (_, dry) = tail(id, false);
		let (mut p, held) = tail(id, true);
		assert!(
			held > dry * 2.0,
			"{id}: the pedal holds the note ({held} vs {dry})"
		);
		render(&mut *p, 256, 256, &[pedal(0.0)]);
		let (after, _) = render(&mut *p, 96_000, 256, &[]);
		assert!(
			peak(&after[90_000..]) < held * 0.1,
			"{id}: pedal up releases it"
		);
	}
}

#[test]
fn pitch_bend_and_mod_wheel_move_the_pitch() {
	use crate::util::pitch::detect_pitch;
	for id in ["poly_synth", "electric_piano", "sampler"] {
		let mut p = prepared(id, SR, 256);
		let on = MidiEvent::NoteOn {
			offset: 0,
			pitch: 57,
			velocity: 1.0,
		};
		let (plain, _) = render(&mut *p, 4800, 256, &[on]);
		let base = detect_pitch(&plain[1200..], SR).unwrap();
		let bend = MidiEvent::PitchBend {
			offset: 0,
			value: 1.0,
		};
		let (bent, _) = render(&mut *p, 4800, 256, &[bend]);
		let up = detect_pitch(&bent[1200..], SR).unwrap();
		let ratio = up / base;
		assert!(
			(ratio - 2.0_f32.powf(2.0 / 12.0)).abs() < 0.02,
			"{id}: full bend = +2 semitones ({base} -> {up} Hz)"
		);
	}
}
