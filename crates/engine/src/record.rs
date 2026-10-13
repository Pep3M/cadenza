//! Recording: the audio-thread tap, the disk writer thread, the live view of a recording
//! in progress and the undoable "Record" batch built from the result.
//!
//! Data path: after rendering each block the output callback knows which timeline
//! samples the block plays ([`crate::graph::Segment`]: the playhead as heard, after
//! plugin delay compensation, split at loop wraps). The [`RecordTap`] pushes the input
//! frames that travelled with those output frames into a pre-allocated `rtrb` sample
//! ring and one [`RecEvent::Audio`] (timeline tag + frame count) into an event ring;
//! MIDI input is tagged with the block's first timeline sample ([`RecEvent::Midi`]) and
//! loop wraps become [`RecEvent::Wrap`]. When a ring is full the tap skips the block and
//! counts the frames as dropouts (the writer fills the gap with silence so later audio
//! keeps its position); nothing on the audio thread ever waits.
//!
//! The writer thread drains both rings into one 32-bit float WAV per armed audio track
//! (in the project's `Audio/` folder, or a temp folder for unsaved projects), keeps the
//! loop passes (where each one starts in the file and on the timeline) and the MIDI
//! events, and publishes waveform peaks for drawing while recording
//! ([`RecordingMonitor`]). A loop recording is one long file; each pass becomes a take
//! clip pointing into it.
//!
//! Latency compensation: a frame tagged with timeline sample `t` was captured `L`
//! samples before the output frame it travelled with was heard (the measured capture ->
//! playback delay, see `crate::input`, plus the user's offset), so it belongs at
//! `t - L`. Clips therefore start at the tag of their pass and read the file from
//! `L` frames further in ([`record_commands`]); a pass's last `L` frames are the first
//! `L` frames of the next pass in the file, which is why one continuous file per track
//! keeps loop takes seamless. MIDI is shifted by the output latency only.

use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use cadenza_core::{ClipId, Meter, MeterMap, TakeGroupId, TempoMap, Tick, TrackId};
use cadenza_midi_io::{
	RandomIds, RawMessage, RecordedControl, RecordedNote, Recorder, control_lanes,
};
use cadenza_model::{
	Clip, ClipContent, ClipPatch, Command, ControllerPoint, Document, Note, Project, Take,
	TrackInput,
};
use rtrb::{Consumer, Producer, RingBuffer};

use crate::error::EngineError;
use crate::input::MAX_INPUT_CHANNELS;

/// Seconds of audio the sample ring holds before the writer must have drained it.
pub const DEFAULT_BUFFER_SECONDS: f64 = 4.0;
/// Events in flight (a few per callback).
const EVENT_SLOTS: usize = 16_384;
/// Frames per waveform peak published while recording.
pub const PEAK_BLOCK: usize = 256;
/// How long [`crate::Player::finish_recording`] waits for the audio thread by default.
pub const FINISH_TIMEOUT: Duration = Duration::from_secs(2);
/// Frames recorded past the punch-out (covers the latency compensation).
const PUNCH_TAIL_SECONDS: f64 = 1.0;
/// Longest gap filled with silence (dropouts); longer jumps start a new pass.
const MAX_GAP_SECONDS: f64 = 10.0;

/// What a recording captures.
#[derive(Debug, Clone, PartialEq)]
pub struct RecordSetup {
	/// Where the WAV files go (created if needed).
	pub folder: PathBuf,
	/// Recording starts here (punch-in, or the playhead).
	pub start: Tick,
	/// Recording stops capturing here (punch-out); playback goes on.
	pub punch_out: Option<Tick>,
	/// Bars of metronome count-in before playback starts (0 = none).
	pub count_in_bars: u32,
	pub tracks: Vec<RecordTrack>,
	/// Seconds of audio buffered between the audio thread and the disk writer (more
	/// survives longer disk stalls; frames beyond it are dropouts).
	pub buffer_seconds: f64,
}

impl RecordSetup {
	pub fn new(folder: impl Into<PathBuf>, start: Tick, tracks: Vec<RecordTrack>) -> Self {
		Self {
			folder: folder.into(),
			start,
			punch_out: None,
			count_in_bars: 0,
			tracks,
			buffer_seconds: DEFAULT_BUFFER_SECONDS,
		}
	}
}

/// One armed track.
#[derive(Debug, Clone, PartialEq)]
pub struct RecordTrack {
	pub track: TrackId,
	pub name: String,
	/// Audio input to record (audio tracks); `None` records MIDI (instrument tracks).
	pub input: Option<TrackInput>,
}

/// What the audio thread hands the writer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum RecEvent {
	/// `frames` frames (of every recorded channel) heard at timeline sample `tag`; the
	/// samples are in the sample ring.
	Audio {
		tag: i64,
		frames: u32,
	},
	Midi {
		tag: i64,
		message: RawMessage,
	},
	/// The playhead jumped from `from` back to `to` (loop wrap or seek).
	Wrap {
		from: i64,
		to: i64,
	},
}

/// Counters of a recording, readable from any thread.
#[derive(Debug, Default)]
pub struct RecordStats {
	/// Frames the tap skipped because the writer fell behind.
	pub(crate) dropped: AtomicU64,
}

impl RecordStats {
	pub fn dropped_frames(&self) -> u64 {
		self.dropped.load(Ordering::Relaxed)
	}
}

/// Audio-thread side of a recording. Created (allocated) on the control side and handed
/// over boxed; never allocates afterwards.
pub(crate) struct RecordTap {
	events: Producer<RecEvent>,
	samples: Producer<f32>,
	/// Input channels recorded, in ring order.
	channels: [usize; MAX_INPUT_CHANNELS],
	count: usize,
	/// Tagged timeline range captured: `[start, end)`.
	start: i64,
	end: i64,
	midi: bool,
	/// Timeline sample the next segment continues at (wrap detection).
	pub(crate) next_tag: Option<i64>,
	stats: Arc<RecordStats>,
}

impl RecordTap {
	/// One played stretch: input frames `frames[c][offset..offset + len]` travelled with
	/// output heard at timeline samples `tag..tag + len`.
	pub fn audio(&mut self, frames: &[Vec<f32>], offset: usize, len: usize, tag: i64) {
		if len == 0 {
			return;
		}
		if let Some(next) = self.next_tag
			&& next != tag
		{
			self.event(RecEvent::Wrap {
				from: next,
				to: tag,
			});
		}
		self.next_tag = Some(tag + len as i64);
		if self.count == 0 {
			return;
		}
		let lo = tag.max(self.start);
		let hi = (tag + len as i64).min(self.end);
		if hi <= lo {
			return;
		}
		let skip = (lo - tag) as usize;
		let n = (hi - lo) as usize;
		let need = n * self.count;
		// Keep a slot for wrap/MIDI events so pass boundaries are never lost.
		if self.samples.slots() < need || self.events.slots() < 2 {
			self.stats.dropped.fetch_add(n as u64, Ordering::Relaxed);
			return;
		}
		if let Ok(chunk) = self.samples.write_chunk_uninit(need) {
			let channels = &self.channels[..self.count];
			chunk.fill_from_iter((0..n).flat_map(|i| {
				channels
					.iter()
					.map(move |&c| frames.get(c).map_or(0.0, |ch| ch[offset + skip + i]))
			}));
		}
		let _ = self.events.push(RecEvent::Audio {
			tag: lo,
			frames: n as u32,
		});
	}

	/// A MIDI message heard at timeline sample `tag`.
	pub fn midi(&mut self, tag: i64, message: RawMessage) {
		if self.midi && (self.start..self.end).contains(&tag) {
			self.event(RecEvent::Midi { tag, message });
		}
	}

	fn event(&mut self, e: RecEvent) {
		if self.events.push(e).is_err() {
			self.stats.dropped.fetch_add(1, Ordering::Relaxed);
		}
	}
}

/// One pass of a recording (a loop pass, or the whole take without a loop).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecordedPass {
	/// Timeline sample the pass starts at (as tagged, before latency compensation).
	pub tag: i64,
	/// Frame of the track files where it starts.
	pub file_start: u64,
	pub frames: u64,
	/// The loop end where it wrapped (`None`: it ended with the recording).
	pub wrapped_at: Option<i64>,
}

/// The file recorded for one audio track.
#[derive(Debug, Clone, PartialEq)]
pub struct RecordedAudio {
	pub track: TrackId,
	pub name: String,
	pub file: PathBuf,
	pub frames: u64,
	pub channels: u16,
}

/// MIDI input of a recording, in arrival order.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MidiItem {
	Event { tag: i64, message: RawMessage },
	Wrap { from: i64, to: i64 },
}

/// A finished recording, ready for [`record_commands`].
#[derive(Debug, Clone, PartialEq)]
pub struct Recording {
	pub sample_rate: u32,
	/// Timeline sample recording started at.
	pub start: i64,
	pub punch_out: Option<i64>,
	/// Timeline sample playback had reached when recording stopped.
	pub stop: i64,
	/// Audio latency compensation applied (frames).
	pub latency: i64,
	/// MIDI latency compensation (output latency, frames).
	pub midi_latency: i64,
	pub passes: Vec<RecordedPass>,
	pub audio: Vec<RecordedAudio>,
	pub midi_tracks: Vec<TrackId>,
	pub midi: Vec<MidiItem>,
	/// Frames lost to dropouts (input ring and writer ring).
	pub dropouts: u64,
	pub warnings: Vec<String>,
}

/// What a recording in progress shows (updated by the writer thread).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LiveRecording {
	pub passes: Vec<RecordedPass>,
	/// Peak (max abs over [`PEAK_BLOCK`] frames) of each audio track's file so far.
	pub peaks: Vec<(TrackId, Vec<f32>)>,
	pub midi: Vec<MidiItem>,
	/// Frames written per track file so far.
	pub frames: u64,
	pub error: Option<String>,
}

/// Shared view of a recording in progress.
#[derive(Debug, Default)]
pub struct RecordingMonitor {
	live: Mutex<LiveRecording>,
	stats: Arc<RecordStats>,
	pub(crate) sample_rate: u32,
	pub(crate) tracks: Vec<RecordTrack>,
	pub(crate) start: i64,
}

impl RecordingMonitor {
	/// A copy of the current state (brief lock; the writer never holds it long).
	pub fn snapshot(&self) -> LiveRecording {
		self.live.lock().unwrap_or_else(|p| p.into_inner()).clone()
	}

	pub fn tracks(&self) -> &[RecordTrack] {
		&self.tracks
	}

	pub fn sample_rate(&self) -> u32 {
		self.sample_rate
	}

	/// Timeline sample the recording started at.
	pub fn start_sample(&self) -> i64 {
		self.start
	}

	pub fn dropped_frames(&self) -> u64 {
		self.stats.dropped_frames()
	}

	/// Notes played so far (absolute ticks), held ones reaching `now`, for drawing.
	pub fn midi_notes(&self, tempo: &TempoMap, now: Tick) -> Vec<RecordedNote> {
		let items = self.snapshot().midi;
		let sr = f64::from(self.sample_rate.max(1));
		let tick = |s: i64| tempo.seconds_to_tick(s as f64 / sr);
		let mut recorder = Recorder::new(tempo.clone(), None);
		let mut out = Vec::new();
		for item in items {
			match item {
				MidiItem::Event { tag, message } => recorder.push_at(tick(tag), &message),
				MidiItem::Wrap { from, .. } => {
					out.extend(recorder.preview(tick(from)));
					recorder.release_all(tick(from));
					recorder.take_notes(&mut RandomIds);
				}
			}
		}
		out.extend(recorder.preview(now));
		out
	}
}

/// Control-side handle of a running recording.
pub(crate) struct RecordSession {
	pub writer: Option<JoinHandle<WriterOutput>>,
	pub monitor: Arc<RecordingMonitor>,
	pub stats: Arc<RecordStats>,
	pub punch_out: Option<i64>,
	pub midi_tracks: Vec<TrackId>,
	pub input_dropouts_at_start: u64,
}

/// What the writer thread returns.
#[derive(Debug, Default)]
pub(crate) struct WriterOutput {
	pub passes: Vec<RecordedPass>,
	pub audio: Vec<RecordedAudio>,
	pub midi: Vec<MidiItem>,
	pub error: Option<String>,
}

struct TrackFile {
	track: TrackId,
	name: String,
	path: PathBuf,
	writer: Option<hound::WavWriter<BufWriter<File>>>,
	/// Indexes into the recorded channels (left, right).
	left: usize,
	right: Option<usize>,
	frames: u64,
	peak: f32,
	peak_frames: usize,
}

/// A unique `<folder>/<track> <timestamp>.wav`.
fn record_path(folder: &Path, track: &str, stamp: &str) -> PathBuf {
	let base = cadenza_model::files::sanitize_file_name(&format!("{track} rec {stamp}"));
	let mut path = folder.join(format!("{base}.wav"));
	let mut n = 2;
	while path.exists() {
		path = folder.join(format!("{base} ({n}).wav"));
		n += 1;
	}
	path
}

fn timestamp() -> String {
	let secs = std::time::SystemTime::now()
		.duration_since(std::time::UNIX_EPOCH)
		.map_or(0, |d| d.as_secs());
	// UTC date and time without a calendar dependency (civil-from-days).
	let days = (secs / 86_400) as i64;
	let rem = secs % 86_400;
	let z = days + 719_468;
	let era = z.div_euclid(146_097);
	let doe = z - era * 146_097;
	let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
	let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
	let mp = (5 * doy + 2) / 153;
	let d = doy - (153 * mp + 2) / 5 + 1;
	let m = if mp < 10 { mp + 3 } else { mp - 9 };
	let y = yoe + era * 400 + i64::from(m <= 2);
	format!(
		"{y:04}-{m:02}-{d:02} {:02}{:02}{:02}",
		rem / 3600,
		rem % 3600 / 60,
		rem % 60
	)
}

/// Creates the rings, the files and the writer thread of a recording. Returns the
/// tap for the audio thread and the control-side session.
pub(crate) fn start(
	setup: &RecordSetup,
	tempo: &TempoMap,
	sample_rate: u32,
	input_channels: usize,
) -> Result<(Box<RecordTap>, RecordSession), EngineError> {
	let sr = f64::from(sample_rate);
	let mut channels = [0usize; MAX_INPUT_CHANNELS];
	let mut count = 0;
	let mut index_of = |c: usize| -> Result<usize, EngineError> {
		if let Some(i) = channels[..count].iter().position(|x| *x == c) {
			return Ok(i);
		}
		if c >= input_channels.min(MAX_INPUT_CHANNELS) {
			return Err(EngineError::Record(format!(
				"input {} is not available: the audio input has {input_channels} channel(s); \
				 pick another input for the track or another input device",
				c + 1
			)));
		}
		channels[count] = c;
		count += 1;
		Ok(count - 1)
	};
	let audio_tracks: Vec<&RecordTrack> =
		setup.tracks.iter().filter(|t| t.input.is_some()).collect();
	let mut mapping = Vec::new();
	for t in &audio_tracks {
		let input = t.input.expect("audio track");
		let left = index_of(usize::from(input.channel))?;
		let right = if input.stereo {
			Some(index_of(usize::from(input.channel) + 1)?)
		} else {
			None
		};
		mapping.push((t, left, right));
	}
	if !audio_tracks.is_empty() {
		std::fs::create_dir_all(&setup.folder).map_err(|e| {
			EngineError::Record(format!(
				"cannot create the recording folder '{}': {e}",
				setup.folder.display()
			))
		})?;
	}
	let stamp = timestamp();
	let mut files = Vec::new();
	for (t, left, right) in mapping {
		let path = record_path(&setup.folder, &t.name, &stamp);
		let spec = hound::WavSpec {
			channels: if right.is_some() { 2 } else { 1 },
			sample_rate,
			bits_per_sample: 32,
			sample_format: hound::SampleFormat::Float,
		};
		let writer = hound::WavWriter::create(&path, spec)
			.map_err(|e| EngineError::Record(format!("cannot create '{}': {e}", path.display())))?;
		files.push(TrackFile {
			track: t.track,
			name: t.name.clone(),
			path,
			writer: Some(writer),
			left,
			right,
			frames: 0,
			peak: 0.0,
			peak_frames: 0,
		});
	}
	let ring = ((setup.buffer_seconds.clamp(0.001, 60.0) * sr) as usize * count.max(1)).max(1);
	let (samples_tx, samples_rx) = RingBuffer::new(ring);
	let (events_tx, events_rx) = RingBuffer::new(EVENT_SLOTS);
	let stats = Arc::new(RecordStats::default());
	let start_sample = tempo.tick_to_sample(setup.start, sr) as i64;
	let punch_out = setup
		.punch_out
		.filter(|p| *p > setup.start)
		.map(|p| tempo.tick_to_sample(p, sr) as i64);
	let tap = Box::new(RecordTap {
		events: events_tx,
		samples: samples_tx,
		channels,
		count,
		start: start_sample,
		end: punch_out.map_or(i64::MAX, |p| p + (PUNCH_TAIL_SECONDS * sr) as i64),
		midi: setup.tracks.iter().any(|t| t.input.is_none()),
		next_tag: None,
		stats: stats.clone(),
	});
	let monitor = Arc::new(RecordingMonitor {
		live: Mutex::new(LiveRecording {
			peaks: files.iter().map(|f| (f.track, Vec::new())).collect(),
			..LiveRecording::default()
		}),
		stats: stats.clone(),
		sample_rate,
		tracks: setup.tracks.clone(),
		start: start_sample,
	});
	let live = monitor.clone();
	let writer = std::thread::Builder::new()
		.name("cadenza-record-writer".into())
		.spawn(move || {
			Writer {
				events: events_rx,
				samples: samples_rx,
				channels: count,
				files,
				sample_rate,
				passes: Vec::new(),
				midi: Vec::new(),
				expected: None,
				wrap_pending: false,
				total: 0,
				error: None,
				scratch: Vec::new(),
			}
			.run(&live)
		})
		.map_err(|e| EngineError::Record(format!("cannot start the recording writer: {e}")))?;
	Ok((
		tap,
		RecordSession {
			writer: Some(writer),
			monitor,
			stats,
			punch_out,
			midi_tracks: setup
				.tracks
				.iter()
				.filter(|t| t.input.is_none())
				.map(|t| t.track)
				.collect(),
			input_dropouts_at_start: 0,
		},
	))
}

struct Writer {
	events: Consumer<RecEvent>,
	samples: Consumer<f32>,
	channels: usize,
	files: Vec<TrackFile>,
	sample_rate: u32,
	passes: Vec<RecordedPass>,
	midi: Vec<MidiItem>,
	/// Timeline sample the next audio chunk should start at.
	expected: Option<i64>,
	wrap_pending: bool,
	/// Frames written to every file.
	total: u64,
	error: Option<String>,
	scratch: Vec<f32>,
}

impl Writer {
	fn run(mut self, live: &RecordingMonitor) -> WriterOutput {
		loop {
			// Checked before draining: once the producer is gone and the ring is empty,
			// everything has been written.
			let done = self.events.is_abandoned();
			let mut changed = false;
			let mut peaks: Vec<(usize, f32)> = Vec::new();
			let midi_before = self.midi.len();
			while let Ok(event) = self.events.pop() {
				changed = true;
				self.handle(event, &mut peaks);
			}
			if changed {
				let mut l = live.live.lock().unwrap_or_else(|p| p.into_inner());
				l.passes.clone_from(&self.passes);
				l.frames = self.total;
				l.midi.extend_from_slice(&self.midi[midi_before..]);
				for (i, p) in peaks {
					if let Some((_, v)) = l.peaks.get_mut(i) {
						v.push(p);
					}
				}
				l.error.clone_from(&self.error);
			}
			if done && self.events.is_empty() {
				break;
			}
			if !changed {
				std::thread::sleep(Duration::from_millis(5));
			}
		}
		let mut audio = Vec::new();
		for mut f in self.files {
			let channels = if f.right.is_some() { 2 } else { 1 };
			if let Some(w) = f.writer.take()
				&& let Err(e) = w.finalize()
			{
				self.error
					.get_or_insert_with(|| format!("cannot finish '{}': {e}", f.path.display()));
			}
			if f.frames == 0 {
				let _ = std::fs::remove_file(&f.path);
				continue;
			}
			audio.push(RecordedAudio {
				track: f.track,
				name: f.name,
				file: f.path,
				frames: f.frames,
				channels,
			});
		}
		WriterOutput {
			passes: self.passes,
			audio,
			midi: self.midi,
			error: self.error,
		}
	}

	fn handle(&mut self, event: RecEvent, peaks: &mut Vec<(usize, f32)>) {
		match event {
			RecEvent::Midi { tag, message } => self.midi.push(MidiItem::Event { tag, message }),
			RecEvent::Wrap { from, to } => {
				self.midi.push(MidiItem::Wrap { from, to });
				if let Some(p) = self.passes.last_mut()
					&& p.wrapped_at.is_none()
					&& to < from
				{
					p.wrapped_at = Some(from);
				}
				self.wrap_pending = true;
			}
			RecEvent::Audio { tag, frames } => {
				let n = frames as usize * self.channels;
				self.scratch.clear();
				if let Ok(chunk) = self.samples.read_chunk(n.min(self.samples.slots())) {
					let (a, b) = chunk.as_slices();
					self.scratch.extend_from_slice(a);
					self.scratch.extend_from_slice(b);
					chunk.commit_all();
				}
				self.scratch.resize(n, 0.0);
				let max_gap = (MAX_GAP_SECONDS * f64::from(self.sample_rate)) as i64;
				let new_pass = match self.expected {
					None => true,
					Some(e) => self.wrap_pending || tag < e || tag - e > max_gap,
				};
				if new_pass {
					self.wrap_pending = false;
					self.passes.push(RecordedPass {
						tag,
						file_start: self.total,
						frames: 0,
						wrapped_at: None,
					});
				} else if let Some(e) = self.expected
					&& tag > e
				{
					// Dropouts: keep later audio at its position.
					self.write_silence((tag - e) as u64, peaks);
				}
				self.write_frames(frames as usize, peaks);
				self.expected = Some(tag + i64::from(frames));
			}
		}
	}

	fn write_silence(&mut self, frames: u64, peaks: &mut Vec<(usize, f32)>) {
		let saved = std::mem::take(&mut self.scratch);
		self.scratch = vec![0.0; frames as usize * self.channels];
		self.write_frames(frames as usize, peaks);
		self.scratch = saved;
	}

	fn write_frames(&mut self, frames: usize, peaks: &mut Vec<(usize, f32)>) {
		let ch = self.channels;
		for (fi, f) in self.files.iter_mut().enumerate() {
			for i in 0..frames {
				let l = self.scratch[i * ch + f.left];
				let r = f.right.map(|r| self.scratch[i * ch + r]);
				if let Some(w) = &mut f.writer {
					let result = w
						.write_sample(l)
						.and_then(|()| r.map_or(Ok(()), |r| w.write_sample(r)));
					if let Err(e) = result {
						self.error.get_or_insert_with(|| {
							format!(
								"writing '{}' failed: {e} (is the disk full?)",
								f.path.display()
							)
						});
						f.writer = None;
					}
				}
				f.peak = f.peak.max(l.abs()).max(r.map_or(0.0, f32::abs));
				f.peak_frames += 1;
				if f.peak_frames == PEAK_BLOCK {
					peaks.push((fi, f.peak));
					f.peak = 0.0;
					f.peak_frames = 0;
				}
			}
			f.frames += frames as u64;
		}
		self.total += frames as u64;
		if let Some(p) = self.passes.last_mut() {
			p.frames += frames as u64;
		}
	}
}

/// How MIDI recordings land in the project.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RecordOptions {
	/// `true`: add the notes to the clip under the recording start (or the pattern
	/// clip); `false`: always a new clip.
	pub merge: bool,
	/// `true` (overdub): keep the notes already in the merged clip; `false` (replace):
	/// remove its notes inside the recorded range first.
	pub overdub: bool,
	/// Loop recording: each pass becomes its own take (else all passes merge, FL-style).
	pub midi_takes: bool,
	/// Snap recorded note starts to this grid.
	pub quantize: Option<Tick>,
	/// Pattern mode: record into this clip (its track only).
	pub pattern_clip: Option<ClipId>,
}

/// The clips (and take groups) of a finished recording as ONE undoable `Batch`
/// ("Record"). `None` when nothing was recorded.
pub fn record_commands(
	project: &Project,
	rec: &Recording,
	options: &RecordOptions,
) -> Option<Command> {
	let sr = f64::from(rec.sample_rate.max(1));
	let tempo = &project.tempo;
	let tick = |s: i64| tempo.seconds_to_tick(s.max(0) as f64 / sr);
	let mut doc = Document::new(project.clone());
	let mut commands = Vec::new();
	for audio in &rec.audio {
		let Some(track) = project.track(audio.track) else {
			continue;
		};
		let mut clips = Vec::new();
		for pass in &rec.passes {
			let offset = pass.file_start as i64 + rec.latency;
			let wanted = match (pass.wrapped_at, rec.punch_out) {
				(Some(end), _) => end - pass.tag,
				(None, Some(out)) if out > pass.tag => out - pass.tag,
				_ => pass.frames as i64 - rec.latency,
			};
			let len = wanted.min(audio.frames as i64 - offset);
			// Shorter than a millisecond: noise of a stop right at a wrap.
			if len < (sr / 1000.0) as i64 || offset < 0 {
				continue;
			}
			let (start, end) = (tick(pass.tag), tick(pass.tag + len));
			if end <= start {
				continue;
			}
			let mut clip = Clip::audio(track.name.clone(), start, end - start, &audio.file);
			if let ClipContent::Audio { offset_seconds, .. } = &mut clip.content {
				*offset_seconds = offset as f64 / sr;
			}
			clips.push(clip);
		}
		stack(&mut doc, &mut commands, track.id, clips);
	}
	let midi_tracks: Vec<TrackId> = rec
		.midi_tracks
		.iter()
		.copied()
		.filter(|t| project.track(*t).is_some())
		.collect();
	if !midi_tracks.is_empty() {
		let passes = midi_passes(rec, tempo, options.quantize);
		for track in midi_tracks {
			midi_clips(&mut doc, &mut commands, track, &passes, rec, options);
		}
	}
	(!commands.is_empty()).then(|| Command::Batch {
		label: "Record".into(),
		commands,
	})
}

/// Executes `cmd` on the scratch document and keeps it (`false` if the model refused).
fn run(doc: &mut Document, commands: &mut Vec<Command>, cmd: Command) -> bool {
	if doc.execute(cmd.clone()).is_ok() {
		commands.push(cmd);
		true
	} else {
		false
	}
}

/// Adds `clips` to `track`. Several clips (loop passes), or clips recorded over existing
/// ones, become a take group: the existing material inside the recorded range is cut out
/// of its clips and kept as the first (inactive) takes, the new passes follow, the last
/// one plays.
fn stack(doc: &mut Document, commands: &mut Vec<Command>, track: TrackId, clips: Vec<Clip>) {
	let Some(lo) = clips.iter().map(|c| c.start).min() else {
		return;
	};
	let hi = clips.iter().map(Clip::end).max().unwrap_or(lo);
	let overlapped: Vec<ClipId> = doc
		.project()
		.track(track)
		.map(|t| {
			t.clips
				.iter()
				.filter(|c| c.start < hi && c.end() > lo && c.is_midi() == clips[0].is_midi())
				.map(|c| c.id)
				.collect()
		})
		.unwrap_or_default();
	if clips.len() == 1 && overlapped.is_empty() {
		let clip = clips.into_iter().next().expect("one clip");
		run(doc, commands, Command::AddClip { track, clip });
		return;
	}
	let group = TakeGroupId::new();
	// Cut the overlapped clips at the range edges; the parts inside join the group.
	let mut inside: Vec<(u32, Tick, ClipId)> = Vec::new();
	for id in overlapped {
		let mut current = id;
		for at in [lo, hi] {
			let Some((_, c)) = doc.project().find_clip(current) else {
				break;
			};
			if c.start < at && at < c.end() {
				let right = ClipId::new();
				let take = c.take;
				let Some(cmds) =
					cadenza_model::takes::split_clip(doc.project(), current, at, right, take)
				else {
					continue;
				};
				for cmd in cmds {
					run(doc, commands, cmd);
				}
				if at == lo {
					current = right;
				}
			}
		}
		if let Some((_, c)) = doc.project().find_clip(current) {
			inside.push((c.take.map_or(0, |t| t.lane), c.start, current));
		}
	}
	inside.sort();
	let old = inside.len() as u32;
	for (lane, (_, _, id)) in inside.into_iter().enumerate() {
		run(
			doc,
			commands,
			Command::UpdateClip {
				clip: id,
				patch: ClipPatch {
					take: Some(Some(Take {
						group,
						lane: lane as u32,
						active: false,
					})),
					..ClipPatch::default()
				},
			},
		);
	}
	let last = clips.len().saturating_sub(1);
	for (k, mut clip) in clips.into_iter().enumerate() {
		clip.name = format!("{} take {}", clip.name, old as usize + k + 1);
		clip.take = Some(Take {
			group,
			lane: old + k as u32,
			active: k == last,
		});
		run(doc, commands, Command::AddClip { track, clip });
	}
}

/// MIDI passes: (start tick, end tick, notes with absolute starts, controller events).
type MidiPass = (Tick, Tick, Vec<Note>, Vec<RecordedControl>);

fn midi_passes(rec: &Recording, tempo: &TempoMap, quantize: Option<Tick>) -> Vec<MidiPass> {
	let sr = f64::from(rec.sample_rate.max(1));
	let tick = |s: i64| tempo.seconds_to_tick(s.max(0) as f64 / sr);
	let mut recorder = Recorder::new(tempo.clone(), quantize);
	let mut passes = Vec::new();
	let mut pass_start = rec.start;
	for item in &rec.midi {
		match *item {
			MidiItem::Event { tag, message } => {
				recorder.push_at(tick(tag - rec.midi_latency), &message);
			}
			MidiItem::Wrap { from, to } => {
				recorder.release_all(tick(from));
				passes.push((
					tick(pass_start),
					tick(from),
					recorder.take_notes(&mut RandomIds),
					recorder.take_controls(),
				));
				pass_start = to;
			}
		}
	}
	let end = rec.punch_out.map_or(rec.stop, |p| p.min(rec.stop));
	recorder.release_all(tick(end));
	passes.push((
		tick(pass_start),
		tick(end),
		recorder.take_notes(&mut RandomIds),
		recorder.take_controls(),
	));
	passes
}

fn bar_floor(t: Tick, meter: Meter<'_>) -> Tick {
	meter.bar_floor(t.max(Tick::ZERO))
}

fn bar_ceil(t: Tick, meter: Meter<'_>) -> Tick {
	meter.bar_ceil(t.max(Tick::ZERO))
}

fn relative(notes: &[Note], origin: Tick) -> Vec<Note> {
	notes
		.iter()
		.filter(|n| n.start >= origin)
		.map(|n| Note {
			start: n.start - origin,
			..n.clone()
		})
		.collect()
}

fn midi_clips(
	doc: &mut Document,
	commands: &mut Vec<Command>,
	track: TrackId,
	passes: &[MidiPass],
	rec: &Recording,
	options: &RecordOptions,
) {
	let meter = MeterMap::new(doc.project().meter());
	let meter = meter.view();
	let Some(name) = doc.project().track(track).map(|t| t.name.clone()) else {
		return;
	};
	let played: Vec<&MidiPass> = passes
		.iter()
		.filter(|p| !p.2.is_empty() || !p.3.is_empty())
		.collect();
	if played.is_empty() {
		return;
	}
	if options.midi_takes && passes.len() > 1 {
		let clips = played
			.iter()
			.filter_map(|(start, end, notes, controls)| {
				let (s, e) = (*start, (*end).max(*start + Tick(1)));
				let notes: Vec<Note> = relative(notes, s)
					.into_iter()
					.filter(|n| n.start < e - s)
					.collect();
				let lanes = control_lanes(controls, s);
				(!notes.is_empty() || !lanes.is_empty()).then(|| {
					Clip::midi(name.clone(), s, e - s)
						.with_notes(notes)
						.with_controllers(lanes)
				})
			})
			.collect();
		stack(doc, commands, track, clips);
		return;
	}
	let notes: Vec<Note> = played.iter().flat_map(|p| p.2.iter().cloned()).collect();
	let controls: Vec<RecordedControl> = played.iter().flat_map(|p| p.3.iter().copied()).collect();
	let first = notes
		.iter()
		.map(|n| n.start)
		.chain(controls.iter().map(|c| c.tick))
		.min()
		.unwrap_or_default();
	let last = notes
		.iter()
		.map(Note::end)
		.chain(controls.iter().map(|c| c.tick + Tick(1)))
		.max()
		.unwrap_or(first);
	let sr = f64::from(rec.sample_rate.max(1));
	let rec_start = doc
		.project()
		.tempo
		.seconds_to_tick(rec.start.max(0) as f64 / sr);
	let range = (
		passes.iter().map(|p| p.0).min().unwrap_or(rec_start),
		passes.iter().map(|p| p.1).max().unwrap_or(last),
	);
	let target = options
		.pattern_clip
		.filter(|c| {
			doc.project()
				.find_clip(*c)
				.is_some_and(|(t, c)| t.id == track && c.is_midi())
		})
		.or_else(|| {
			options.merge.then_some(()).and_then(|()| {
				doc.project().track(track).and_then(|t| {
					t.clips
						.iter()
						.find(|c| c.is_midi() && c.plays() && c.start <= first && first < c.end())
						.map(|c| c.id)
				})
			})
		});
	let Some(target) = target else {
		let start = bar_floor(first.min(rec_start.max(first)), meter);
		let end = bar_ceil(last, meter).max(meter.bar_at(start).end());
		let clip = Clip::midi(name, start, end - start)
			.with_notes(relative(&notes, start))
			.with_controllers(control_lanes(&controls, start));
		run(doc, commands, Command::AddClip { track, clip });
		return;
	};
	let Some((_, clip)) = doc.project().find_clip(target) else {
		return;
	};
	let (origin, clip_end) = (clip.start, clip.end());
	if !options.overdub {
		let gone: Vec<cadenza_core::NoteId> = clip
			.notes()
			.iter()
			.filter(|n| {
				let at = origin + n.start;
				range.0 <= at && at < range.1
			})
			.map(|n| n.id)
			.collect();
		if !gone.is_empty() {
			run(
				doc,
				commands,
				Command::RemoveNotes {
					clip: target,
					notes: gone,
				},
			);
		}
	}
	if last > clip_end {
		let end = bar_ceil(last, meter);
		run(
			doc,
			commands,
			Command::UpdateClip {
				clip: target,
				patch: ClipPatch {
					length: Some(end - origin),
					..ClipPatch::default()
				},
			},
		);
	}
	let notes = relative(&notes, origin);
	if !notes.is_empty() {
		run(
			doc,
			commands,
			Command::AddNotes {
				clip: target,
				notes,
			},
		);
	}
	// Controller lanes: replace mode clears the recorded range of each lane played;
	// overdub keeps the other events (the new ones win at the same tick).
	for lane in control_lanes(&controls, origin) {
		let old: Vec<ControllerPoint> = doc
			.project()
			.find_clip(target)
			.and_then(|(_, c)| c.controller(lane.controller))
			.map(|l| l.points.clone())
			.unwrap_or_default();
		let points = if options.overdub {
			let mut merged: Vec<ControllerPoint> = old
				.into_iter()
				.filter(|p| !lane.points.iter().any(|n| n.tick == p.tick))
				.chain(lane.points.iter().copied())
				.collect();
			merged.sort_by_key(|p| p.tick);
			merged
		} else {
			cadenza_model::controller::splice(
				&old,
				range.0 - origin,
				range.1 - origin,
				&lane.points,
			)
		};
		run(
			doc,
			commands,
			Command::SetControllerLane {
				clip: target,
				controller: lane.controller,
				points,
			},
		);
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use cadenza_core::PPQ;
	use cadenza_model::{Track, TrackKind};

	const SR: u32 = 48_000;
	/// 120 bpm at 48 kHz: one beat.
	const BEAT: i64 = 24_000;

	fn recording(passes: Vec<RecordedPass>, frames: u64, latency: i64) -> Recording {
		Recording {
			sample_rate: SR,
			start: passes.first().map_or(0, |p| p.tag),
			punch_out: None,
			stop: 0,
			latency,
			midi_latency: 0,
			passes,
			audio: Vec::new(),
			midi_tracks: Vec::new(),
			midi: Vec::new(),
			dropouts: 0,
			warnings: Vec::new(),
		}
		.with_frames(frames)
	}

	impl Recording {
		fn with_frames(mut self, frames: u64) -> Self {
			self.stop = self.passes.last().map_or(0, |p| p.tag + p.frames as i64);
			self.audio.push(RecordedAudio {
				track: TrackId::new(),
				name: "Vox".into(),
				file: PathBuf::from("/rec/vox.wav"),
				frames,
				channels: 1,
			});
			self
		}
	}

	fn audio_project(track: TrackId) -> Project {
		let mut p = Project::new("r");
		let mut t = Track::new("Vox", TrackKind::Audio);
		t.id = track;
		p.tracks.push(t);
		p
	}

	fn offset(c: &Clip) -> f64 {
		match c.content {
			ClipContent::Audio { offset_seconds, .. } => offset_seconds,
			_ => unreachable!(),
		}
	}

	#[test]
	fn a_take_is_shifted_by_the_latency() {
		let pass = RecordedPass {
			tag: 2 * BEAT,
			file_start: 0,
			frames: 4 * BEAT as u64,
			wrapped_at: None,
		};
		let rec = recording(vec![pass], 4 * BEAT as u64, 480);
		let p = audio_project(rec.audio[0].track);
		let mut doc = Document::new(p);
		doc.execute(record_commands(doc.project(), &rec, &RecordOptions::default()).unwrap())
			.unwrap();
		let clips = &doc.project().tracks[0].clips;
		assert_eq!(clips.len(), 1);
		assert_eq!(clips[0].start, Tick(PPQ * 2));
		assert!((offset(&clips[0]) - 0.01).abs() < 1e-12, "480 frames");
		assert_eq!(clips[0].length, Tick(PPQ * 4) - Tick(480 / 25));
		assert!(clips[0].take.is_none());
		assert_eq!(doc.history_labels(), vec!["Record"]);
	}

	#[test]
	fn loop_passes_become_takes_reading_one_file() {
		// Loop [1 beat, 3 beats): three full passes and a partial one.
		let len = 2 * BEAT as u64;
		let passes: Vec<RecordedPass> = (0..4)
			.map(|k| RecordedPass {
				tag: BEAT,
				file_start: k * len,
				frames: if k < 3 { len } else { len / 2 },
				wrapped_at: (k < 3).then_some(3 * BEAT),
			})
			.collect();
		let rec = recording(passes, 3 * len + len / 2, 100);
		let mut p = audio_project(rec.audio[0].track);
		// An existing clip under the loop becomes the first take (its outside part stays).
		let old = Clip::audio("old", Tick::ZERO, Tick(PPQ * 2), "/old.wav");
		p.tracks[0].clips.push(old);
		let mut doc = Document::new(p);
		doc.execute(record_commands(doc.project(), &rec, &RecordOptions::default()).unwrap())
			.unwrap();
		let clips = &doc.project().tracks[0].clips;
		let takes: Vec<&Clip> = clips.iter().filter(|c| c.take.is_some()).collect();
		// old inside part + 4 passes.
		assert_eq!(takes.len(), 5);
		let playing: Vec<&Clip> = clips.iter().filter(|c| c.plays()).collect();
		assert_eq!(playing.len(), 2, "outside part of old + the last pass");
		let last = playing.iter().find(|c| c.take.is_some()).unwrap();
		assert_eq!(last.take.unwrap().lane, 4);
		// Pass 2 reads the file from its own start + latency.
		let second = takes.iter().find(|c| c.take.unwrap().lane == 2).unwrap();
		assert!((offset(second) * f64::from(SR) - (len as f64 + 100.0)).abs() < 1e-6);
		assert_eq!(second.start, Tick(PPQ));
		assert_eq!(
			second.length,
			Tick(PPQ * 2),
			"full pass, reading into the next"
		);
		let first_take = takes.iter().find(|c| c.take.unwrap().lane == 0).unwrap();
		assert_eq!(
			(first_take.start, first_take.end()),
			(Tick(PPQ), Tick(PPQ * 2))
		);
		assert_eq!(doc.history_labels(), vec!["Record"]);
		doc.undo().unwrap();
		assert_eq!(doc.project().tracks[0].clips.len(), 1);
	}

	fn note_on(p: u8) -> RawMessage {
		RawMessage::from_bytes(&[0x90, p, 100]).unwrap()
	}

	fn note_off(p: u8) -> RawMessage {
		RawMessage::from_bytes(&[0x80, p, 0]).unwrap()
	}

	fn midi_recording(track: TrackId, midi: Vec<MidiItem>, start: i64, stop: i64) -> Recording {
		Recording {
			sample_rate: SR,
			start,
			punch_out: None,
			stop,
			latency: 0,
			midi_latency: 0,
			passes: Vec::new(),
			audio: Vec::new(),
			midi_tracks: vec![track],
			midi,
			dropouts: 0,
			warnings: Vec::new(),
		}
	}

	#[test]
	fn midi_overdubs_into_the_pattern_clip_every_pass() {
		let mut p = Project::new("m");
		let mut t = Track::new("Keys", TrackKind::Instrument);
		let pattern = Clip::midi("pat", Tick::ZERO, Tick(PPQ * 4)).with_notes([Note::new(
			48,
			Tick::ZERO,
			Tick(PPQ),
			0.8,
		)]);
		let pattern_id = pattern.id;
		t.clips.push(pattern);
		let track = t.id;
		p.tracks.push(t);
		let ev = |tag: i64, m: RawMessage| MidiItem::Event { tag, message: m };
		let midi = vec![
			ev(BEAT, note_on(60)),
			ev(2 * BEAT, note_off(60)),
			// Held across the loop end: cut there.
			ev(3 * BEAT + BEAT / 2, note_on(62)),
			MidiItem::Wrap {
				from: 4 * BEAT,
				to: 0,
			},
			ev(BEAT / 2, note_off(62)),
			ev(2 * BEAT, note_on(64)),
			ev(2 * BEAT + BEAT / 4, note_off(64)),
		];
		let rec = midi_recording(track, midi, 0, 3 * BEAT);
		let options = RecordOptions {
			merge: true,
			overdub: true,
			pattern_clip: Some(pattern_id),
			..RecordOptions::default()
		};
		let mut doc = Document::new(p);
		doc.execute(record_commands(doc.project(), &rec, &options).unwrap())
			.unwrap();
		let notes = doc.project().tracks[0].clips[0].notes();
		let got: Vec<(u8, Tick, Tick)> =
			notes.iter().map(|n| (n.pitch, n.start, n.length)).collect();
		assert_eq!(
			got,
			vec![
				(48, Tick::ZERO, Tick(PPQ)),
				(60, Tick(PPQ), Tick(PPQ)),
				(64, Tick(PPQ * 2), Tick(PPQ / 4)),
				(62, Tick(PPQ * 3 + PPQ / 2), Tick(PPQ / 2)),
			]
		);
		assert_eq!(doc.project().tracks[0].clips.len(), 1);
	}

	#[test]
	fn midi_replace_takes_and_new_clips() {
		let mut p = Project::new("m");
		let mut t = Track::new("Keys", TrackKind::Instrument);
		let existing = Clip::midi("c", Tick::ZERO, Tick(PPQ * 8)).with_notes([Note::new(
			48,
			Tick(PPQ),
			Tick(PPQ),
			0.8,
		)]);
		t.clips.push(existing);
		let track = t.id;
		p.tracks.push(t);
		let ev = |tag: i64, m: RawMessage| MidiItem::Event { tag, message: m };
		let rec = midi_recording(
			track,
			vec![ev(BEAT / 2, note_on(60)), ev(BEAT, note_off(60))],
			0,
			4 * BEAT,
		);
		// Replace: the old note inside the recorded range goes.
		let replace = RecordOptions {
			merge: true,
			..RecordOptions::default()
		};
		let mut doc = Document::new(p.clone());
		doc.execute(record_commands(doc.project(), &rec, &replace).unwrap())
			.unwrap();
		let pitches: Vec<u8> = doc.project().tracks[0].clips[0]
			.notes()
			.iter()
			.map(|n| n.pitch)
			.collect();
		assert_eq!(pitches, vec![60]);
		// New clip: a bar-aligned clip of its own (overlapping clips both play for MIDI).
		let mut doc = Document::new(p.clone());
		doc.execute(record_commands(doc.project(), &rec, &RecordOptions::default()).unwrap())
			.unwrap();
		assert_eq!(doc.project().tracks[0].clips.len(), 2);
		// Loop takes: each pass a take, the old clip joins the group.
		let rec = midi_recording(
			track,
			vec![
				ev(BEAT / 2, note_on(60)),
				ev(BEAT, note_off(60)),
				MidiItem::Wrap {
					from: 4 * BEAT,
					to: 0,
				},
				ev(BEAT / 2, note_on(62)),
				ev(BEAT, note_off(62)),
			],
			0,
			4 * BEAT,
		);
		let takes = RecordOptions {
			midi_takes: true,
			..RecordOptions::default()
		};
		let mut doc = Document::new(p);
		doc.execute(record_commands(doc.project(), &rec, &takes).unwrap())
			.unwrap();
		let playing: Vec<&Clip> = doc.project().tracks[0]
			.clips
			.iter()
			.filter(|c| c.plays())
			.collect();
		assert_eq!(playing.len(), 2, "the old clip's tail + the last pass");
		assert!(
			playing
				.iter()
				.any(|c| c.notes().first().is_some_and(|n| n.pitch == 62))
		);
	}

	#[test]
	fn timestamps_are_calendar_dates() {
		let s = timestamp();
		assert_eq!(s.len(), 17, "{s}");
		assert!(s.starts_with("20"));
	}
}
