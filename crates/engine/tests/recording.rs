//! Recording through a virtual duplex device: synthetic input in, files and clips out.
//! Positions are checked to the sample (latency compensation), plus punch ranges, loop
//! takes, ring overflow, MIDI into a pattern loop, the metronome and the count-in.

use cadenza_core::{PPQ, Tick, TrackId};
use cadenza_engine::{
	MetronomeSettings, Player, RecordOptions, RecordSetup, RecordTrack, Recording, VirtualDevice,
	record_commands,
};
use cadenza_midi_io::{RawMessage, TimedMessage};
use cadenza_model::{
	Clip, ClipContent, Device, Document, MonitorMode, Project, Track, TrackInput, TrackKind,
};

const SR: u32 = 48_000;
/// 120 bpm at 48 kHz.
const BEAT: usize = 24_000;
const BLOCK: usize = 256;

fn folder(name: &str) -> std::path::PathBuf {
	let dir = std::env::temp_dir().join(format!("cadenza-rec-{}-{name}", std::process::id()));
	let _ = std::fs::remove_dir_all(&dir);
	dir
}

fn vox_project(monitor: MonitorMode) -> (Project, TrackId) {
	let mut p = Project::new("rec");
	let mut t = Track::new("Vox", TrackKind::Audio);
	t.input = Some(TrackInput {
		channel: 0,
		stereo: false,
		monitor,
	});
	let id = t.id;
	p.tracks.push(t);
	(p, id)
}

/// Runs `blocks` callbacks; the input of stream frame `j` is `input(j)` on channel 0
/// (channel 1 silent). Returns the output (interleaved stereo).
fn run(
	dev: &mut VirtualDevice,
	stream: &mut usize,
	blocks: usize,
	input: &dyn Fn(usize) -> f32,
) -> Vec<f32> {
	let mut out_all = Vec::new();
	let mut out = vec![0.0f32; BLOCK * 2];
	let mut inp = vec![0.0f32; BLOCK * 2];
	for _ in 0..blocks {
		for i in 0..BLOCK {
			inp[i * 2] = input(*stream + i);
		}
		dev.process(&mut out, &inp);
		out_all.extend_from_slice(&out);
		*stream += BLOCK;
	}
	out_all
}

fn read_wav(path: &std::path::Path) -> Vec<f32> {
	hound::WavReader::open(path)
		.unwrap()
		.samples::<f32>()
		.map(Result::unwrap)
		.collect()
}

fn offset_frames(c: &Clip) -> i64 {
	match c.content {
		ClipContent::Audio { offset_seconds, .. } => {
			(offset_seconds * f64::from(SR)).round() as i64
		}
		_ => unreachable!(),
	}
}

fn tick_of(sample: usize) -> Tick {
	Tick((sample / 25) as i64)
}

fn apply(p: &Project, rec: &Recording, options: &RecordOptions) -> Document {
	let mut doc = Document::new(p.clone());
	doc.execute(record_commands(p, rec, options).expect("something recorded"))
		.unwrap();
	doc
}

fn audio_setup(dir: &std::path::Path, track: TrackId, start: usize) -> RecordSetup {
	RecordSetup::new(
		dir,
		tick_of(start),
		vec![RecordTrack {
			track,
			name: "Vox".into(),
			input: Some(TrackInput::mono(0)),
		}],
	)
}

/// Loopback with a round trip of `delay` frames: an impulse heard at timeline sample
/// 30_000 comes back `delay` frames later; with that latency offset the recording puts
/// it back at 30_000 exactly.
fn loopback(delay: usize) {
	let dir = folder(&format!("loopback-{delay}"));
	let (p, track) = vox_project(MonitorMode::Off);
	let (mut player, mut dev) = Player::virtual_device(SR, 2);
	player.load(&p).unwrap();
	player.set_latency_offset(delay as i64);
	player.set_armed(track, true).unwrap();
	player
		.start_recording(&audio_setup(&dir, track, BEAT))
		.unwrap();
	let mut stream = 0;
	let hit = 30_000 + delay;
	run(&mut dev, &mut stream, 200, &|j| {
		if j == hit { 1.0 } else { 0.0 }
	});
	player.request_stop_recording().unwrap();
	run(&mut dev, &mut stream, 1, &|_| 0.0);
	let rec = player
		.finish_recording(std::time::Duration::from_secs(5))
		.unwrap();
	assert_eq!(rec.latency, delay as i64);
	assert_eq!(rec.dropouts, 0, "{:?}", rec.warnings);
	let doc = apply(&p, &rec, &RecordOptions::default());
	let clips = &doc.project().tracks[0].clips;
	assert_eq!(clips.len(), 1);
	let clip = &clips[0];
	assert_eq!(clip.start, tick_of(BEAT), "starts at the punch-in");
	assert_eq!(offset_frames(clip), delay as i64);
	let ClipContent::Audio { file, .. } = &clip.content else {
		unreachable!()
	};
	let samples = read_wav(file);
	let at = offset_frames(clip) as usize + (30_000 - BEAT);
	assert_eq!(samples[at], 1.0, "the impulse sits at timeline 30_000");
	assert_eq!(samples.iter().filter(|v| **v != 0.0).count(), 1);
	let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn recorded_audio_lands_where_it_was_heard() {
	loopback(0);
}

#[test]
fn latency_compensation_moves_the_recording_back_exactly() {
	loopback(137);
}

#[test]
fn punch_range_records_only_inside_it() {
	let dir = folder("punch");
	let (p, track) = vox_project(MonitorMode::Off);
	let (mut player, mut dev) = Player::virtual_device(SR, 2);
	player.load(&p).unwrap();
	let mut setup = audio_setup(&dir, track, BEAT);
	setup.punch_out = Some(tick_of(2 * BEAT));
	player.start_recording(&setup).unwrap();
	let mut stream = 0;
	// Input = its own timeline position: the file shows what was kept.
	run(&mut dev, &mut stream, 3 * BEAT / BLOCK + 1, &|j| j as f32);
	player.request_stop_recording().unwrap();
	run(&mut dev, &mut stream, 1, &|_| 0.0);
	let rec = player
		.finish_recording(std::time::Duration::from_secs(5))
		.unwrap();
	let doc = apply(&p, &rec, &RecordOptions::default());
	let clip = &doc.project().tracks[0].clips[0];
	assert_eq!((clip.start, clip.end()), (tick_of(BEAT), tick_of(2 * BEAT)));
	let ClipContent::Audio { file, .. } = &clip.content else {
		unreachable!()
	};
	let samples = read_wav(file);
	assert_eq!(samples[0], BEAT as f32, "nothing before the punch-in");
	// The tail past the punch-out (for latency compensation) stops at the stop.
	assert!(samples.len() >= BEAT && samples.len() <= 3 * BEAT);
	let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn loop_recording_makes_one_file_and_a_take_per_pass() {
	let dir = folder("loop");
	let (p, track) = vox_project(MonitorMode::Off);
	let (mut player, mut dev) = Player::virtual_device(SR, 2);
	player.load(&p).unwrap();
	let len = 2 * BEAT;
	player.set_loop(Some((Tick::ZERO, tick_of(len)))).unwrap();
	player
		.start_recording(&audio_setup(&dir, track, 0))
		.unwrap();
	let mut stream = 0;
	// Each pass plays a different constant: pass k = k + 1.
	let blocks = (3 * len + len / 2) / BLOCK;
	run(&mut dev, &mut stream, blocks, &|j| (j / len + 1) as f32);
	player.request_stop_recording().unwrap();
	run(&mut dev, &mut stream, 1, &|_| 0.0);
	let rec = player
		.finish_recording(std::time::Duration::from_secs(5))
		.unwrap();
	assert_eq!(rec.passes.len(), 4);
	assert_eq!(rec.audio.len(), 1, "one file for all passes");
	let doc = apply(&p, &rec, &RecordOptions::default());
	let clips = &doc.project().tracks[0].clips;
	assert_eq!(clips.len(), 4);
	let file = match &clips[0].content {
		ClipContent::Audio { file, .. } => file.clone(),
		_ => unreachable!(),
	};
	let samples = read_wav(&file);
	for c in clips {
		let take = c.take.expect("a take");
		assert_eq!(c.start, Tick::ZERO);
		assert_eq!(take.active, take.lane == 3, "the last pass plays");
		let first = samples[offset_frames(c) as usize];
		assert_eq!(
			first,
			(take.lane + 1) as f32,
			"take {} reads its pass",
			take.lane
		);
	}
	let full = clips.iter().filter(|c| c.length == tick_of(len)).count();
	assert_eq!(full, 3, "three full passes and a partial one");
	assert_eq!(doc.history_labels(), vec!["Record"]);
	let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_full_writer_ring_counts_dropouts_and_keeps_positions() {
	let dir = folder("overflow");
	let (p, track) = vox_project(MonitorMode::Off);
	let (mut player, mut dev) = Player::virtual_device(SR, 2);
	player.load(&p).unwrap();
	let mut setup = audio_setup(&dir, track, 0);
	// Less than two blocks of buffer: the writer cannot keep up with a burst.
	setup.buffer_seconds = 400.0 / f64::from(SR);
	player.start_recording(&setup).unwrap();
	let mut stream = 0;
	run(&mut dev, &mut stream, 400, &|j| j as f32 + 1.0);
	player.request_stop_recording().unwrap();
	run(&mut dev, &mut stream, 1, &|_| 0.0);
	let rec = player
		.finish_recording(std::time::Duration::from_secs(5))
		.unwrap();
	assert!(rec.dropouts > 0);
	assert!(rec.warnings.iter().any(|w| w.contains("lost")));
	let samples = read_wav(&rec.audio[0].file);
	// Whatever survived sits at its own position (gaps are silence).
	for (k, v) in samples.iter().enumerate() {
		assert!(*v == 0.0 || *v == k as f32 + 1.0, "frame {k}: {v}");
	}
	let _ = std::fs::remove_dir_all(&dir);
}

fn note(tx: &mut rtrb::Producer<TimedMessage>, bytes: &[u8]) {
	tx.push(TimedMessage {
		host_micros: 0,
		message: RawMessage::from_bytes(bytes).unwrap(),
	})
	.unwrap();
}

#[test]
fn midi_overdubs_into_the_pattern_loop_every_pass() {
	let mut p = Project::new("pat");
	let mut t = Track::new("Keys", TrackKind::Instrument)
		.with_instrument(Device::builtin("poly_synth", "Synth"));
	let pattern = Clip::midi("pattern", Tick::ZERO, Tick(PPQ * 4));
	let pattern_id = pattern.id;
	t.clips.push(pattern);
	let track = t.id;
	p.tracks.push(t);
	let (mut player, mut dev) = Player::virtual_device(SR, 0);
	player.load(&p).unwrap();
	let (mut tx, rx) = cadenza_midi_io::new_queue(64);
	player.set_midi_queue(rx).unwrap();
	player.set_monitor_track(Some(track)).unwrap();
	let bar = 4 * BEAT;
	player.set_loop(Some((Tick::ZERO, tick_of(bar)))).unwrap();
	let setup = RecordSetup::new(
		folder("midi"),
		Tick::ZERO,
		vec![RecordTrack {
			track,
			name: "Keys".into(),
			input: None,
		}],
	);
	player.start_recording(&setup).unwrap();
	let mut out = vec![0.0f32; BLOCK * 2];
	// Blocks of 256 frames: block 25 starts at 6400 frames = 256 ticks.
	for block in 0..(2 * bar / BLOCK + 10) {
		match block {
			25 => note(&mut tx, &[0x90, 60, 100]),
			50 => note(&mut tx, &[0x80, 60, 0]),
			// Second pass: 375 blocks per bar, 25 into it.
			400 => note(&mut tx, &[0x90, 64, 64]),
			425 => note(&mut tx, &[0x80, 64, 0]),
			_ => {}
		}
		dev.process(&mut out, &[]);
	}
	player.request_stop_recording().unwrap();
	dev.process(&mut out, &[]);
	let rec = player
		.finish_recording(std::time::Duration::from_secs(5))
		.unwrap();
	let options = RecordOptions {
		merge: true,
		overdub: true,
		pattern_clip: Some(pattern_id),
		..RecordOptions::default()
	};
	let doc = apply(&p, &rec, &options);
	let clips = &doc.project().tracks[0].clips;
	assert_eq!(clips.len(), 1, "merged into the pattern");
	let notes: Vec<(u8, Tick, Tick)> = clips[0]
		.notes()
		.iter()
		.map(|n| (n.pitch, n.start, n.length))
		.collect();
	assert_eq!(
		notes,
		vec![(60, Tick(256), Tick(256)), (64, Tick(256), Tick(256))]
	);
}

#[test]
fn the_metronome_clicks_on_beats_and_counts_in() {
	let p = Project::new("click");
	let (mut player, mut dev) = Player::virtual_device(SR, 0);
	player.load(&p).unwrap();
	player
		.set_metronome(MetronomeSettings {
			on_play: true,
			on_record: true,
			level_db: 0.0,
		})
		.unwrap();
	player.play().unwrap();
	let mut stream = 0;
	let out = run(&mut dev, &mut stream, 2 * BEAT / BLOCK + 8, &|_| 0.0);
	let left: Vec<f32> = out.chunks(2).map(|f| f[0]).collect();
	let first = |from: usize| {
		left[from..]
			.iter()
			.position(|v| *v != 0.0)
			.map(|i| i + from)
	};
	assert_eq!(first(0), Some(1), "the downbeat click starts at sample 0");
	assert_eq!(first(2000), Some(BEAT + 1), "the next beat, exactly");
	let accent = left[..2000].iter().fold(0.0f32, |m, v| m.max(v.abs()));
	let beat = left[BEAT..BEAT + 2000]
		.iter()
		.fold(0.0f32, |m, v| m.max(v.abs()));
	assert!(accent > beat, "bar accent {accent} > beat {beat}");

	// A one-bar count-in: the playhead waits 4 beats while it clicks.
	let dir = folder("count-in");
	let (vp, track) = vox_project(MonitorMode::Off);
	let (mut player, mut dev) = Player::virtual_device(SR, 1);
	player.load(&vp).unwrap();
	let mut setup = audio_setup(&dir, track, 0);
	setup.count_in_bars = 1;
	player.start_recording(&setup).unwrap();
	let mut stream = 0;
	let out = run(&mut dev, &mut stream, 4 * BEAT / BLOCK, &|_| 0.5);
	assert_eq!(player.position(), Tick::ZERO, "waiting for the count-in");
	let left: Vec<f32> = out.chunks(2).map(|f| f[0]).collect();
	for b in 0..4 {
		assert!(
			left[b * BEAT + 1..b * BEAT + 400].iter().any(|v| *v != 0.0),
			"count-in click {b}"
		);
	}
	run(&mut dev, &mut stream, BEAT / BLOCK, &|_| 0.5);
	assert!(player.position() > Tick(PPQ / 2), "{:?}", player.position());
	player.request_stop_recording().unwrap();
	run(&mut dev, &mut stream, 1, &|_| 0.0);
	let rec = player
		.finish_recording(std::time::Duration::from_secs(5))
		.unwrap();
	assert_eq!(rec.passes.len(), 1);
	assert_eq!(rec.passes[0].tag, 0, "recording starts after the count-in");
	let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn input_monitoring_follows_the_mode() {
	let level = |mode: MonitorMode, armed: bool, playing: bool| {
		let (p, track) = vox_project(mode);
		let (mut player, mut dev) = Player::virtual_device(SR, 2);
		player.load(&p).unwrap();
		player.set_armed(track, armed).unwrap();
		if playing {
			player.play().unwrap();
		}
		let mut stream = 0;
		let out = run(&mut dev, &mut stream, 20, &|j| {
			(std::f32::consts::TAU * 440.0 * j as f32 / SR as f32).sin() * 0.5
		});
		out[out.len() / 2..]
			.iter()
			.fold(0.0f32, |m, v| m.max(v.abs()))
	};
	assert!(level(MonitorMode::On, false, true) > 0.3, "always on");
	assert_eq!(level(MonitorMode::Off, true, false), 0.0);
	assert!(
		level(MonitorMode::Auto, true, false) > 0.3,
		"auto: armed and stopped"
	);
	assert_eq!(
		level(MonitorMode::Auto, false, false),
		0.0,
		"auto: not armed"
	);
	assert_eq!(
		level(MonitorMode::Auto, true, true),
		0.0,
		"auto: playing back (not recording) plays the clips"
	);
}
