//! Renders a short groove with only built-in devices to
//! `target/render_demo.wav` (32-bit float stereo) so a human can listen.
//!
//! `cargo run -p cadenza-dsp --example render_demo --release`

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::PathBuf;

use cadenza_dsp::{MidiEvent, Processor, Transport, create};

const SR: f32 = 48_000.0;
const BLOCK: usize = 256;
const BPM: f64 = 104.0;
const BARS: usize = 8;

struct Track {
	instrument: Box<dyn Processor>,
	inserts: Vec<Box<dyn Processor>>,
	/// (absolute sample, event with offset 0)
	events: Vec<(usize, MidiEvent)>,
	volume: f32,
	name: &'static str,
	peak: f32,
	energy: f64,
}

fn device(id: &str, params: &[(&str, f32)]) -> Box<dyn Processor> {
	let mut d = create(id).unwrap_or_else(|| panic!("unknown device {id}"));
	for &(pid, v) in params {
		let idx = d
			.info()
			.param_index(pid)
			.unwrap_or_else(|| panic!("{id} has no param {pid}"));
		d.set_param(idx, v);
	}
	d.prepare(SR, BLOCK);
	d
}

fn beat_to_sample(beat: f64) -> usize {
	(beat * 60.0 / BPM * SR as f64).round() as usize
}

fn note(events: &mut Vec<(usize, MidiEvent)>, beat: f64, length: f64, pitch: u8, velocity: f32) {
	events.push((
		beat_to_sample(beat),
		MidiEvent::NoteOn {
			offset: 0,
			pitch,
			velocity,
		},
	));
	events.push((
		beat_to_sample(beat + length),
		MidiEvent::NoteOff { offset: 0, pitch },
	));
}

fn main() -> std::io::Result<()> {
	// A minor: Am - F - C - G, two bars each... one bar each, twice.
	let chords: [[u8; 4]; 4] = [
		[57, 60, 64, 69],
		[53, 57, 60, 65],
		[48, 55, 60, 64],
		[55, 59, 62, 67],
	];
	let roots: [u8; 4] = [33, 29, 36, 31];

	let mut drums = Vec::new();
	let mut bass = Vec::new();
	let mut pad = Vec::new();
	let mut keys = Vec::new();
	let mut pluck = Vec::new();
	for bar in 0..BARS {
		let b = bar as f64 * 4.0;
		let chord = chords[bar % 4];
		for (beat, pitch, vel) in [
			(0.0, 36, 1.0),
			(1.0, 38, 0.9),
			(1.75, 36, 0.7),
			(2.5, 36, 0.9),
			(3.0, 38, 0.95),
		] {
			note(&mut drums, b + beat, 0.1, pitch, vel);
		}
		for i in 0..8 {
			let vel = if i % 2 == 0 { 0.8 } else { 0.55 };
			let pitch = if i == 7 && bar % 2 == 1 { 46 } else { 42 };
			note(&mut drums, b + i as f64 * 0.5, 0.1, pitch, vel);
		}
		if bar % 4 == 0 {
			note(&mut drums, b, 0.1, 49, 0.8);
		}
		if bar % 4 == 3 {
			note(&mut drums, b + 3.5, 0.1, 39, 0.9);
		}

		let root = roots[bar % 4];
		for (beat, len, octave) in [
			(0.0, 0.4, 0),
			(0.75, 0.2, 0),
			(1.5, 0.4, 12),
			(2.5, 0.4, 0),
			(3.25, 0.5, 0),
		] {
			note(&mut bass, b + beat, len, root + octave, 0.85);
		}
		for &p in &chord {
			note(&mut pad, b, 3.9, p, 0.6);
		}
		if bar >= 2 {
			for (beat, vel) in [(0.5, 0.7), (1.5, 0.55), (2.75, 0.65)] {
				for &p in &chord[1..] {
					note(&mut keys, b + beat, 0.4, p + 12, vel);
				}
			}
		}
		if bar >= 4 {
			for i in 0..8 {
				let p = chord[[0, 2, 1, 3][i % 4]] + 12;
				note(&mut pluck, b + i as f64 * 0.5, 0.25, p, 0.7);
			}
		}
	}

	let mut tracks = vec![
		Track {
			instrument: device("drum_machine", &[]),
			inserts: vec![device(
				"compressor",
				&[
					("threshold", -14.0),
					("ratio", 4.0),
					("attack", 15.0),
					("mix", 60.0),
				],
			)],
			events: drums,
			name: "drums",
			peak: 0.0,
			energy: 0.0,
			volume: 0.75,
		},
		Track {
			instrument: device(
				"poly_synth",
				&[
					("osc1_wave", 1.0),
					("osc2_wave", 2.0),
					("osc2_semitones", -12.0),
					("osc_mix", 35.0),
					("filter_cutoff", 280.0),
					("filter_resonance", 25.0),
					("filter_env_amount", 30.0),
					("filter_decay", 220.0),
					("filter_sustain", 10.0),
					("amp_attack", 2.0),
					("amp_release", 80.0),
				],
			),
			inserts: vec![
				device("saturator", &[("mode", 1.0), ("drive", 6.0)]),
				device("gain", &[("width", 0.0)]),
			],
			events: bass,
			name: "bass",
			peak: 0.0,
			energy: 0.0,
			volume: 1.0,
		},
		Track {
			instrument: device(
				"poly_synth",
				&[
					("unison", 3.0),
					("unison_detune", 18.0),
					("filter_cutoff", 1_400.0),
					("filter_env_amount", 6.0),
					("amp_attack", 500.0),
					("amp_sustain", 90.0),
					("amp_release", 2_000.0),
					("gain", -3.0),
				],
			),
			inserts: vec![
				device("eq", &[("hp_freq", 200.0), ("hp_slope", 2.0)]),
				device("chorus", &[("mix", 35.0)]),
				device("reverb", &[("size", 80.0), ("decay", 3.5), ("mix", 40.0)]),
			],
			events: pad,
			name: "pad",
			peak: 0.0,
			energy: 0.0,
			volume: 0.8,
		},
		Track {
			instrument: device(
				"electric_piano",
				&[("brightness", 45.0), ("tremolo_depth", 30.0)],
			),
			inserts: vec![device(
				"reverb",
				&[("size", 40.0), ("decay", 1.6), ("mix", 25.0)],
			)],
			events: keys,
			name: "keys",
			peak: 0.0,
			energy: 0.0,
			volume: 0.6,
		},
		Track {
			instrument: device(
				"poly_synth",
				&[
					("filter_cutoff", 500.0),
					("filter_env_amount", 48.0),
					("filter_decay", 160.0),
					("filter_sustain", 0.0),
					("amp_decay", 300.0),
					("amp_sustain", 0.0),
					("amp_release", 200.0),
				],
			),
			inserts: vec![
				device(
					"delay",
					&[
						("sync", 7.0),
						("feedback", 40.0),
						("ping_pong", 1.0),
						("mix", 30.0),
					],
				),
				device("reverb", &[("mix", 20.0)]),
			],
			events: pluck,
			name: "pluck",
			peak: 0.0,
			energy: 0.0,
			volume: 0.9,
		},
	];
	let mut master: Vec<Box<dyn Processor>> = vec![
		device(
			"eq",
			&[
				("hp_freq", 30.0),
				("hp_slope", 1.0),
				("high_shelf_gain", 1.5),
			],
		),
		device(
			"compressor",
			&[
				("threshold", -12.0),
				("ratio", 2.0),
				("attack", 30.0),
				("release", 200.0),
			],
		),
		device("limiter", &[("input_gain", 3.0), ("ceiling", -1.0)]),
	];

	for t in &mut tracks {
		t.events
			.sort_by_key(|(at, ev)| (*at, matches!(ev, MidiEvent::NoteOn { .. })));
	}
	let total = beat_to_sample(BARS as f64 * 4.0) + (SR * 4.0) as usize;
	let mut out_l = Vec::with_capacity(total);
	let mut out_r = Vec::with_capacity(total);
	let mut mix_l = [0.0_f32; BLOCK];
	let mut mix_r = [0.0_f32; BLOCK];
	let mut buf_l = [0.0_f32; BLOCK];
	let mut buf_r = [0.0_f32; BLOCK];
	let mut block_events = Vec::new();
	let mut pos = 0;
	while pos < total {
		let n = BLOCK.min(total - pos);
		let transport = Transport {
			bpm: BPM,
			playing: true,
			beat_position: pos as f64 / SR as f64 * BPM / 60.0,
			..Default::default()
		};
		mix_l[..n].fill(0.0);
		mix_r[..n].fill(0.0);
		for t in &mut tracks {
			block_events.clear();
			for &(at, ev) in t
				.events
				.iter()
				.filter(|(at, _)| (pos..pos + n).contains(at))
			{
				let offset = (at - pos) as u32;
				block_events.push(match ev {
					MidiEvent::NoteOn {
						pitch, velocity, ..
					} => MidiEvent::NoteOn {
						offset,
						pitch,
						velocity,
					},
					MidiEvent::NoteOff { pitch, .. } => MidiEvent::NoteOff { offset, pitch },
					MidiEvent::AllNotesOff { .. } => MidiEvent::AllNotesOff { offset },
				});
			}
			buf_l[..n].fill(0.0);
			buf_r[..n].fill(0.0);
			t.instrument
				.process(&mut buf_l[..n], &mut buf_r[..n], &block_events, &transport);
			for fx in &mut t.inserts {
				fx.process(&mut buf_l[..n], &mut buf_r[..n], &[], &transport);
			}
			for i in 0..n {
				let (l, r) = (buf_l[i] * t.volume, buf_r[i] * t.volume);
				t.peak = t.peak.max(l.abs()).max(r.abs());
				t.energy += (l * l + r * r) as f64 * 0.5;
				mix_l[i] += l;
				mix_r[i] += r;
			}
		}
		for fx in &mut master {
			fx.process(&mut mix_l[..n], &mut mix_r[..n], &[], &transport);
		}
		out_l.extend_from_slice(&mix_l[..n]);
		out_r.extend_from_slice(&mix_r[..n]);
		pos += n;
	}

	for t in &tracks {
		println!(
			"{:>6}: peak {:6.1} dBFS, rms {:6.1} dBFS",
			t.name,
			20.0 * t.peak.max(1e-9).log10(),
			10.0 * (t.energy / total as f64).max(1e-12).log10()
		);
	}
	// Compensate the limiter's lookahead so the song starts at sample 0.
	let latency = master.iter().map(|d| d.latency() as usize).sum::<usize>();
	let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/render_demo.wav");
	write_wav(&path, &out_l[latency..], &out_r[latency..])?;
	let peak = out_l
		.iter()
		.chain(&out_r)
		.fold(0.0_f32, |m, x| m.max(x.abs()));
	println!(
		"wrote {} ({:.1} s, peak {:.2} dBFS)",
		path.display(),
		out_l.len() as f32 / SR,
		20.0 * peak.log10()
	);
	Ok(())
}

/// Minimal WAVE_FORMAT_IEEE_FLOAT writer (no dependencies).
fn write_wav(path: &PathBuf, left: &[f32], right: &[f32]) -> std::io::Result<()> {
	let mut w = BufWriter::new(File::create(path)?);
	let channels: u16 = 2;
	let bits: u16 = 32;
	let block_align = channels * bits / 8;
	let data_len = (left.len() * block_align as usize) as u32;
	w.write_all(b"RIFF")?;
	w.write_all(&(4 + 26 + 12 + 8 + data_len).to_le_bytes())?;
	w.write_all(b"WAVE")?;
	w.write_all(b"fmt ")?;
	w.write_all(&18u32.to_le_bytes())?;
	w.write_all(&3u16.to_le_bytes())?; // IEEE float
	w.write_all(&channels.to_le_bytes())?;
	w.write_all(&(SR as u32).to_le_bytes())?;
	w.write_all(&(SR as u32 * block_align as u32).to_le_bytes())?;
	w.write_all(&block_align.to_le_bytes())?;
	w.write_all(&bits.to_le_bytes())?;
	w.write_all(&0u16.to_le_bytes())?; // cbSize
	w.write_all(b"fact")?;
	w.write_all(&4u32.to_le_bytes())?;
	w.write_all(&(left.len() as u32).to_le_bytes())?;
	w.write_all(b"data")?;
	w.write_all(&data_len.to_le_bytes())?;
	for (l, r) in left.iter().zip(right) {
		w.write_all(&l.to_le_bytes())?;
		w.write_all(&r.to_le_bytes())?;
	}
	w.flush()
}
