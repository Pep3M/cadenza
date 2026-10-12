//! Live audio input: a cpal input stream feeding the output callback through a
//! lock-free ring, input metering and the per-track input FIFO of the graph.
//!
//! Threading and clocks: the input stream's callback only converts its samples to f32 and
//! pushes them (interleaved) into an `rtrb` ring, plus one [`InputStamp`] (capture time,
//! frame count) per callback into a second ring. The output callback drains the ring at
//! the start of every block ([`InputReader::read`]): each output block of `n` frames takes
//! `n` input frames, so frame `i` of the input travels with output frame `i` of the block
//! that read it. Nothing is ever allocated, locked or blocked on either side; a full ring
//! (the output side stalled) drops the newest input frames and counts them, an empty one
//! (the input is late) pads silence and counts it.
//!
//! Same device, same rate ([`InputMode::Exact`]): input and output share one clock, so the
//! ring's fill only wobbles by a callback period; the reader waits until it holds one
//! block plus a margin before it starts and then copies frames 1:1 (bit exact, which the
//! recording tests rely on). Separate devices or different rates ([`InputMode::Adaptive`]):
//! the clocks drift, so the reader resamples with linear interpolation at the nominal
//! ratio corrected by a slow proportional controller that keeps the ring's fill at its
//! target (at most ±0.5 %, inaudible); a fill that runs away anyway is trimmed (counted
//! as dropped frames).
//!
//! Latency: the delay between an input frame's capture and the playback of the output
//! frame it travels with is measured from the cpal timestamps when the backend provides
//! them (capture instant of the input packet vs. playback instant of the output block);
//! otherwise the average backlog left in the ring after each read stands in for it. The
//! recorder subtracts this delay (plus a user offset) to place recordings where the
//! performer heard the song (see `crate::record`).

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};
use rtrb::{Consumer, Producer, RingBuffer};

use crate::QUANTUM;
use crate::error::EngineError;

/// Input channels the engine can use (higher device channels are ignored).
pub const MAX_INPUT_CHANNELS: usize = 32;
/// Seconds of input the ring between the input and output callbacks holds.
const RING_SECONDS: f64 = 0.5;
/// Capture stamps in flight (one per input callback).
const STAMP_SLOTS: usize = 256;
/// Frames per channel the graph's input FIFO holds.
const FIFO_FRAMES: usize = 4096;
/// Largest drift correction of the adaptive resampler (relative ratio).
const MAX_ADJUST: f64 = 0.005;

/// Capture time of one input callback's first frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct InputStamp {
	/// Backend clock, nanoseconds (0 = unknown).
	pub capture_ns: u64,
	pub frames: u32,
}

/// How the output side consumes the input ring.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum InputMode {
	/// Same clock and rate as the output: frames are copied 1:1.
	Exact,
	/// Another clock (separate device) or rate: resampled, drift-corrected.
	Adaptive {
		/// Input rate / output rate.
		ratio: f64,
	},
}

/// The open audio input, as reported to the UI.
#[derive(Debug, Clone, PartialEq)]
pub struct InputInfo {
	pub device: String,
	pub channels: u16,
	pub sample_rate: u32,
	pub mode: InputMode,
}

/// Counters and levels of the audio input, shared with the control side (atomics only).
#[derive(Debug, Default)]
pub struct InputStats {
	/// Frames the input callback could not push (ring full: the output side stalled).
	pub(crate) overflow: AtomicU64,
	/// Frames the output side padded with silence (ring empty: input late).
	pub(crate) underrun: AtomicU64,
	/// Frames trimmed to keep the ring's fill (drift that the resampler could not absorb).
	pub(crate) trimmed: AtomicU64,
	/// Sum / count of the backlog (frames left in the ring after a read).
	pub(crate) backlog_sum: AtomicU64,
	pub(crate) backlog_count: AtomicU64,
	/// Sum / count of the measured capture -> playback delay (nanoseconds).
	pub(crate) delay_ns_sum: AtomicU64,
	pub(crate) delay_count: AtomicU64,
	/// Peak per input channel since the last read (f32 bits), like the track meters.
	peaks: [AtomicU32; MAX_INPUT_CHANNELS],
}

impl InputStats {
	/// Peak level of input channel `channel` since the previous call (linear, resets).
	pub fn take_peak(&self, channel: usize) -> f32 {
		self.peaks
			.get(channel)
			.map_or(0.0, |p| f32::from_bits(p.swap(0, Ordering::Relaxed)))
	}

	/// Frames lost or padded so far (overflows + underruns + trims): dropouts.
	pub fn dropouts(&self) -> u64 {
		self.overflow.load(Ordering::Relaxed)
			+ self.underrun.load(Ordering::Relaxed)
			+ self.trimmed.load(Ordering::Relaxed)
	}

	/// Restarts the latency measurement (at the start of a recording).
	pub(crate) fn reset_latency(&self) {
		for a in [
			&self.backlog_sum,
			&self.backlog_count,
			&self.delay_ns_sum,
			&self.delay_count,
		] {
			a.store(0, Ordering::Relaxed);
		}
	}

	/// Average capture -> playback delay in frames at `sample_rate`: measured from the
	/// backend's timestamps when available, else the average ring backlog.
	pub(crate) fn delay_frames(&self, sample_rate: u32) -> f64 {
		let n = self.delay_count.load(Ordering::Relaxed);
		if n > 0 {
			let ns = self.delay_ns_sum.load(Ordering::Relaxed) as f64 / n as f64;
			return ns * f64::from(sample_rate) / 1e9;
		}
		let n = self.backlog_count.load(Ordering::Relaxed);
		if n > 0 {
			return self.backlog_sum.load(Ordering::Relaxed) as f64 / n as f64;
		}
		0.0
	}

	fn measure(&self, channel: usize, x: &[f32]) {
		let peak = x.iter().fold(0.0f32, |m, v| m.max(v.abs()));
		if peak > 0.0 && peak.is_finite() {
			self.peaks[channel].fetch_max(peak.to_bits(), Ordering::Relaxed);
		}
	}
}

/// Output-side reader of the input ring (lives on the audio thread).
pub(crate) struct InputReader {
	ring: Consumer<f32>,
	stamps: Consumer<InputStamp>,
	channels: usize,
	sample_rate: u32,
	mode: InputMode,
	/// Extra frames kept in the ring as a cushion against callback jitter (`None`: one
	/// output block, decided at the first read).
	margin: Option<usize>,
	primed: bool,
	/// The stamp of the packet the next frame belongs to, and frames already read from it.
	stamp: Option<(InputStamp, u32)>,
	// Adaptive mode.
	pos: f64,
	prev: [f32; MAX_INPUT_CHANNELS],
	next: [f32; MAX_INPUT_CHANNELS],
	fill_avg: f64,
	stats: Arc<InputStats>,
}

impl InputReader {
	pub fn new(
		ring: Consumer<f32>,
		stamps: Consumer<InputStamp>,
		channels: usize,
		sample_rate: u32,
		mode: InputMode,
		margin: Option<usize>,
		stats: Arc<InputStats>,
	) -> Self {
		Self {
			ring,
			stamps,
			channels: channels.max(1),
			sample_rate,
			mode,
			margin,
			primed: false,
			stamp: None,
			pos: 1.0,
			prev: [0.0; MAX_INPUT_CHANNELS],
			next: [0.0; MAX_INPUT_CHANNELS],
			fill_avg: 0.0,
			stats,
		}
	}

	/// Channels the reader delivers (at most [`MAX_INPUT_CHANNELS`]).
	pub fn channels(&self) -> usize {
		self.channels.min(MAX_INPUT_CHANNELS)
	}

	fn available(&self) -> usize {
		self.ring.slots() / self.channels
	}

	/// Pops one interleaved frame into `frame`. False when the ring is empty.
	fn pop_frame(&mut self, frame: &mut [f32; MAX_INPUT_CHANNELS]) -> bool {
		if self.available() == 0 {
			return false;
		}
		let used = self.channels();
		let Ok(chunk) = self.ring.read_chunk(self.channels) else {
			return false;
		};
		let (a, b) = chunk.as_slices();
		for (c, v) in a.iter().chain(b).enumerate() {
			if c < used {
				frame[c] = *v;
			}
		}
		chunk.commit_all();
		self.advance_stamp(1);
		true
	}

	/// Drops `frames` frames from the ring.
	fn skip(&mut self, frames: usize) {
		let n = frames.min(self.available()) * self.channels;
		if let Ok(chunk) = self.ring.read_chunk(n) {
			chunk.commit_all();
		}
		self.advance_stamp(frames as u32);
	}

	/// Keeps track of which capture packet the next frame belongs to.
	fn advance_stamp(&mut self, mut frames: u32) {
		while frames > 0 {
			if self.stamp.is_none() {
				match self.stamps.pop() {
					Ok(s) => self.stamp = Some((s, 0)),
					Err(_) => return,
				}
			}
			let Some((s, used)) = &mut self.stamp else {
				return;
			};
			let left = s.frames.saturating_sub(*used);
			if frames < left {
				*used += frames;
				return;
			}
			frames -= left;
			self.stamp = None;
		}
	}

	/// Capture time of the next frame to read, if the backend reports it.
	fn next_capture_ns(&mut self) -> Option<u64> {
		if self.stamp.is_none() {
			self.stamp = self.stamps.pop().ok().map(|s| (s, 0));
		}
		let (s, used) = self.stamp?;
		(s.capture_ns > 0)
			.then(|| s.capture_ns + (f64::from(used) * 1e9 / f64::from(self.sample_rate)) as u64)
	}

	/// Reads `n` frames (deinterleaved, at the output rate) into `out[c][..n]`, padding
	/// with silence while priming or on underrun. `playback_ns`: when the first output
	/// frame of this block will be heard (backend clock), for the latency measurement.
	pub fn read(&mut self, out: &mut [Vec<f32>], n: usize, playback_ns: Option<u64>) {
		let used = self.channels().min(out.len());
		let margin = *self.margin.get_or_insert(n);
		match self.mode {
			InputMode::Exact => self.read_exact(out, used, n, margin, playback_ns),
			InputMode::Adaptive { ratio } => {
				self.read_adaptive(out, used, n, margin, ratio, playback_ns)
			}
		}
		for (c, ch) in out.iter().enumerate().take(used) {
			self.stats.measure(c, &ch[..n]);
		}
	}

	fn read_exact(
		&mut self,
		out: &mut [Vec<f32>],
		used: usize,
		n: usize,
		margin: usize,
		playback_ns: Option<u64>,
	) {
		let avail = self.available();
		if !self.primed {
			if avail < n + margin {
				for ch in out.iter_mut().take(used) {
					ch[..n].fill(0.0);
				}
				return;
			}
			// Start with exactly the cushion left over.
			self.skip(avail - n - margin);
			self.primed = true;
		}
		self.note_delay(playback_ns);
		let avail = self.available();
		let take = avail.min(n);
		let mut frame = [0.0f32; MAX_INPUT_CHANNELS];
		for i in 0..take {
			self.pop_frame(&mut frame);
			for (c, ch) in out.iter_mut().enumerate().take(used) {
				ch[i] = frame[c];
			}
		}
		if take < n {
			for ch in out.iter_mut().take(used) {
				ch[take..n].fill(0.0);
			}
			self.stats
				.underrun
				.fetch_add((n - take) as u64, Ordering::Relaxed);
			self.primed = false;
			return;
		}
		let backlog = self.available();
		// Same clock: the backlog only wobbles by a period. A runaway (a stalled output
		// that came back) is trimmed to the cushion.
		if backlog > margin + 4 * n.max(QUANTUM) {
			let extra = backlog - margin;
			self.skip(extra);
			self.stats
				.trimmed
				.fetch_add(extra as u64, Ordering::Relaxed);
		}
		self.note_backlog(self.available());
	}

	#[allow(clippy::too_many_arguments)]
	fn read_adaptive(
		&mut self,
		out: &mut [Vec<f32>],
		used: usize,
		n: usize,
		margin: usize,
		ratio: f64,
		playback_ns: Option<u64>,
	) {
		let target = margin as f64 + n as f64 * ratio;
		let avail = self.available();
		if !self.primed {
			if (avail as f64) < target {
				for ch in out.iter_mut().take(used) {
					ch[..n].fill(0.0);
				}
				return;
			}
			self.primed = true;
			self.fill_avg = avail as f64;
		}
		self.note_delay(playback_ns);
		self.fill_avg += (avail as f64 - self.fill_avg) * 0.05;
		if self.fill_avg > target * 4.0 + 1024.0 {
			let extra = (avail as f64 - target) as usize;
			self.skip(extra);
			self.stats
				.trimmed
				.fetch_add(extra as u64, Ordering::Relaxed);
			self.fill_avg = target;
		}
		let error = (self.fill_avg - target) / target.max(1.0);
		let step = ratio * (1.0 + (error * 0.002).clamp(-MAX_ADJUST, MAX_ADJUST));
		let mut missing = 0u64;
		for i in 0..n {
			while self.pos >= 1.0 {
				self.prev = self.next;
				let mut next = self.next;
				if !self.pop_frame(&mut next) {
					missing += 1;
				}
				self.next = next;
				self.pos -= 1.0;
			}
			let t = self.pos as f32;
			for (c, ch) in out.iter_mut().enumerate().take(used) {
				ch[i] = self.prev[c] + (self.next[c] - self.prev[c]) * t;
			}
			self.pos += step;
		}
		if missing > 0 {
			self.stats.underrun.fetch_add(missing, Ordering::Relaxed);
			self.primed = false;
		}
		self.note_backlog(self.available());
	}

	fn note_backlog(&self, frames: usize) {
		self.stats
			.backlog_sum
			.fetch_add(frames as u64, Ordering::Relaxed);
		self.stats.backlog_count.fetch_add(1, Ordering::Relaxed);
	}

	fn note_delay(&mut self, playback_ns: Option<u64>) {
		let (Some(play), Some(capture)) = (playback_ns, self.next_capture_ns()) else {
			return;
		};
		// Implausible (clocks from different bases): fall back to the backlog.
		let Some(delay) = play.checked_sub(capture).filter(|d| *d < 1_000_000_000) else {
			return;
		};
		self.stats.delay_ns_sum.fetch_add(delay, Ordering::Relaxed);
		self.stats.delay_count.fetch_add(1, Ordering::Relaxed);
	}
}

/// Graph-side FIFO between the input frames each callback reads and the quanta the graph
/// renders (which run up to a quantum ahead of the output). Single-threaded, owned by the
/// audio thread and lent to [`crate::Graph`] for each block.
pub(crate) struct InputFifo {
	buf: Vec<Vec<f32>>,
	read: usize,
	len: usize,
	channels: usize,
}

impl InputFifo {
	pub fn new() -> Self {
		Self {
			buf: (0..MAX_INPUT_CHANNELS)
				.map(|_| vec![0.0; FIFO_FRAMES])
				.collect(),
			read: 0,
			// One quantum of silence so the first quanta never run dry.
			len: QUANTUM,
			channels: 0,
		}
	}

	/// Appends `n` frames of `frames[c]` (one slice per channel, `channels` of them). A
	/// FIFO that filled up while nothing consumed it (stopped transport) is trimmed to
	/// one quantum first, so monitoring latency never accumulates.
	pub fn push(&mut self, frames: &[Vec<f32>], channels: usize, n: usize) {
		self.channels = channels.min(MAX_INPUT_CHANNELS);
		if self.len > 2 * QUANTUM {
			let drop = self.len - QUANTUM;
			self.read = (self.read + drop) % FIFO_FRAMES;
			self.len = QUANTUM;
		}
		let n = n.min(FIFO_FRAMES - self.len);
		let w = (self.read + self.len) % FIFO_FRAMES;
		for (src, dst) in frames.iter().zip(&mut self.buf).take(self.channels) {
			let first = n.min(FIFO_FRAMES - w);
			dst[w..w + first].copy_from_slice(&src[..first]);
			dst[..n - first].copy_from_slice(&src[first..n]);
		}
		self.len += n;
	}

	/// Takes one quantum per channel into `out` (silence for missing frames/channels).
	pub fn read_quantum(&mut self, out: &mut [[f32; QUANTUM]]) {
		let take = self.len.min(QUANTUM);
		for (c, dst) in out.iter_mut().enumerate() {
			if c >= self.channels {
				dst.fill(0.0);
				continue;
			}
			let mut r = self.read;
			for v in dst.iter_mut().take(take) {
				*v = self.buf[c][r];
				r += 1;
				if r == FIFO_FRAMES {
					r = 0;
				}
			}
			dst[take..].fill(0.0);
		}
		self.read = (self.read + take) % FIFO_FRAMES;
		self.len -= take;
	}
}

/// One audio device as listed for the settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioDeviceInfo {
	pub name: String,
	pub is_default: bool,
	/// Most channels of any supported configuration.
	pub channels: u16,
	/// Common rates (44.1/48/88.2/96 kHz) the device supports.
	pub sample_rates: Vec<u32>,
}

/// Output and input devices of the default host.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AudioDevices {
	pub outputs: Vec<AudioDeviceInfo>,
	pub inputs: Vec<AudioDeviceInfo>,
}

const COMMON_RATES: [u32; 4] = [44_100, 48_000, 88_200, 96_000];

pub(crate) fn device_name(device: &cpal::Device) -> String {
	device
		.description()
		.map(|d| d.name().to_string())
		.unwrap_or_else(|_| "unknown device".into())
}

/// Lists the audio devices (empty lists when the audio system is unavailable).
pub fn audio_devices() -> AudioDevices {
	let host = cpal::default_host();
	let default_out = host.default_output_device().map(|d| device_name(&d));
	let default_in = host.default_input_device().map(|d| device_name(&d));
	let mut out = AudioDevices::default();
	if let Ok(devices) = host.output_devices() {
		for d in devices {
			let Ok(configs) = d.supported_output_configs() else {
				continue;
			};
			out.outputs
				.push(describe(&d, configs, default_out.as_deref()));
		}
	}
	if let Ok(devices) = host.input_devices() {
		for d in devices {
			let Ok(configs) = d.supported_input_configs() else {
				continue;
			};
			out.inputs
				.push(describe(&d, configs, default_in.as_deref()));
		}
	}
	out
}

fn describe(
	device: &cpal::Device,
	configs: impl Iterator<Item = cpal::SupportedStreamConfigRange>,
	default: Option<&str>,
) -> AudioDeviceInfo {
	let name = device_name(device);
	let mut channels = 0;
	let mut rates = Vec::new();
	for c in configs {
		channels = channels.max(c.channels());
		for r in COMMON_RATES {
			if (c.min_sample_rate()..=c.max_sample_rate()).contains(&r) && !rates.contains(&r) {
				rates.push(r);
			}
		}
	}
	rates.sort_unstable();
	AudioDeviceInfo {
		is_default: default == Some(name.as_str()),
		name,
		channels,
		sample_rates: rates,
	}
}

/// Finds an output (`input` false) or input device by exact name (`None`: the default).
pub(crate) fn find_device(name: Option<&str>, input: bool) -> Result<cpal::Device, EngineError> {
	let host = cpal::default_host();
	let Some(name) = name else {
		let d = if input {
			host.default_input_device()
		} else {
			host.default_output_device()
		};
		return d.ok_or(if input {
			EngineError::Audio("no audio input device available".into())
		} else {
			EngineError::NoOutputDevice
		});
	};
	let devices = if input {
		host.input_devices().map(|d| d.collect::<Vec<_>>())
	} else {
		host.output_devices().map(|d| d.collect::<Vec<_>>())
	}
	.map_err(|e| EngineError::Audio(e.to_string()))?;
	devices
		.into_iter()
		.find(|d| device_name(d) == name)
		.ok_or_else(|| {
			EngineError::Audio(format!(
				"audio {} device '{name}' not found; pick another one in the audio settings",
				if input { "input" } else { "output" }
			))
		})
}

/// A running cpal input stream and the ring ends the output side consumes.
pub(crate) struct OpenInput {
	pub stream: cpal::Stream,
	pub info: InputInfo,
	pub ring: Consumer<f32>,
	pub stamps: Consumer<InputStamp>,
}

/// Creates the rings between an input callback and the output side for `channels` at
/// `sample_rate`.
/// Producer and consumer ends of the input sample ring and its capture stamps.
pub(crate) type InputRingEnds = (
	(Producer<f32>, Producer<InputStamp>),
	(Consumer<f32>, Consumer<InputStamp>),
);

pub(crate) fn input_rings(channels: usize, sample_rate: u32) -> InputRingEnds {
	let frames = (f64::from(sample_rate) * RING_SECONDS) as usize;
	let (tx, rx) = RingBuffer::new(frames * channels.max(1));
	let (stx, srx) = RingBuffer::new(STAMP_SLOTS);
	((tx, stx), (rx, srx))
}

/// Opens input device `name` (`None`: the default), preferring `output_rate` and the
/// output device's clock (`same_device`).
pub(crate) fn open_input(
	name: Option<&str>,
	output_rate: u32,
	output_device: &str,
	stats: Arc<InputStats>,
) -> Result<OpenInput, EngineError> {
	let device = find_device(name, true)?;
	let device_name = device_name(&device);
	let audio = |e: &dyn std::fmt::Display| {
		EngineError::Audio(format!("cannot open audio input '{device_name}': {e}"))
	};
	let supported = device
		.supported_input_configs()
		.map_err(|e| audio(&e))?
		.filter(|c| (c.min_sample_rate()..=c.max_sample_rate()).contains(&output_rate))
		.max_by_key(|c| (c.sample_format() == SampleFormat::F32, c.channels()))
		.map(|c| c.with_sample_rate(output_rate));
	let supported = match supported {
		Some(s) => s,
		None => device.default_input_config().map_err(|e| audio(&e))?,
	};
	let format = supported.sample_format();
	let config: cpal::StreamConfig = supported.config();
	let channels = config.channels as usize;
	let ((tx, stx), (ring, stamps)) = input_rings(channels, config.sample_rate);
	let stream = match format {
		SampleFormat::F32 => build_input::<f32>(&device, &config, tx, stx, stats),
		SampleFormat::I16 => build_input::<i16>(&device, &config, tx, stx, stats),
		SampleFormat::I32 => build_input::<i32>(&device, &config, tx, stx, stats),
		SampleFormat::U16 => build_input::<u16>(&device, &config, tx, stx, stats),
		SampleFormat::F64 => build_input::<f64>(&device, &config, tx, stx, stats),
		other => Err(EngineError::Audio(format!(
			"unsupported input sample format {other}"
		))),
	}?;
	stream.play().map_err(|e| audio(&e))?;
	let mode = if config.sample_rate == output_rate && device_name == output_device {
		InputMode::Exact
	} else {
		InputMode::Adaptive {
			ratio: f64::from(config.sample_rate) / f64::from(output_rate),
		}
	};
	Ok(OpenInput {
		stream,
		info: InputInfo {
			device: device_name,
			channels: config.channels,
			sample_rate: config.sample_rate,
			mode,
		},
		ring,
		stamps,
	})
}

fn build_input<T: SizedSample>(
	device: &cpal::Device,
	config: &cpal::StreamConfig,
	mut ring: Producer<f32>,
	mut stamps: Producer<InputStamp>,
	stats: Arc<InputStats>,
) -> Result<cpal::Stream, EngineError>
where
	f32: FromSample<T>,
{
	let channels = config.channels as usize;
	device
		.build_input_stream(
			*config,
			move |data: &[T], info: &cpal::InputCallbackInfo| {
				let frames = data.len() / channels.max(1);
				let room = ring.slots() / channels.max(1);
				let take = frames.min(room);
				if take < frames {
					stats
						.overflow
						.fetch_add((frames - take) as u64, Ordering::Relaxed);
				}
				if take == 0 {
					return;
				}
				let capture = info.timestamp().capture;
				let capture_ns = u64::try_from(capture.as_nanos()).unwrap_or(0);
				let _ = stamps.push(InputStamp {
					capture_ns,
					frames: take as u32,
				});
				if let Ok(chunk) = ring.write_chunk_uninit(take * channels) {
					chunk.fill_from_iter(
						data[..take * channels].iter().map(|s| s.to_sample::<f32>()),
					);
				}
			},
			|_err| {},
			None,
		)
		.map_err(|e| EngineError::Audio(e.to_string()))
}

#[cfg(test)]
mod tests {
	use super::*;

	fn reader(mode: InputMode, channels: usize) -> (Producer<f32>, InputReader) {
		let ((tx, _stx), (rx, srx)) = input_rings(channels, 48_000);
		let stats = Arc::new(InputStats::default());
		(
			tx,
			InputReader::new(rx, srx, channels, 48_000, mode, Some(0), stats),
		)
	}

	fn push(tx: &mut Producer<f32>, samples: impl IntoIterator<Item = f32>) {
		for s in samples {
			tx.push(s).unwrap();
		}
	}

	#[test]
	fn exact_mode_copies_frames_in_lock_step() {
		let (mut tx, mut r) = reader(InputMode::Exact, 2);
		let mut out = vec![vec![0.0f32; 64]; 2];
		push(&mut tx, (0..128).map(|i| i as f32));
		r.read(&mut out, 64, None);
		// Interleaved L R L R ... -> L = even, R = odd.
		assert_eq!(out[0][..3], [0.0, 2.0, 4.0]);
		assert_eq!(out[1][63], 127.0);
		// An empty ring pads silence and counts it.
		r.read(&mut out, 64, None);
		assert!(out[0].iter().all(|v| *v == 0.0));
		assert_eq!(r.stats.underrun.load(Ordering::Relaxed), 64);
		assert!(r.stats.take_peak(1) >= 127.0);
	}

	#[test]
	fn adaptive_mode_resamples_and_keeps_the_fill() {
		// 44.1 kHz in, 48 kHz out: a 441 Hz sine stays a 441 Hz sine.
		let ratio = 44_100.0 / 48_000.0;
		let (mut tx, mut r) = reader(InputMode::Adaptive { ratio }, 1);
		r.margin = Some(256);
		let mut out = vec![vec![0.0f32; 480]];
		let mut phase = 0usize;
		let mut collected = Vec::new();
		for _ in 0..200 {
			// The input device delivers ~441 frames per 10 ms block.
			push(
				&mut tx,
				(0..441).map(|i| {
					let t = (phase + i) as f32 / 44_100.0;
					(std::f32::consts::TAU * 441.0 * t).sin()
				}),
			);
			phase += 441;
			r.read(&mut out, 480, None);
			collected.extend_from_slice(&out[0]);
		}
		let tail = &collected[collected.len() / 2..];
		let crossings = tail
			.windows(2)
			.filter(|w| w[0] < 0.0 && w[1] >= 0.0)
			.count();
		let freq = crossings as f64 * 48_000.0 / tail.len() as f64;
		assert!((freq - 441.0).abs() < 3.0, "{freq}");
		assert_eq!(r.stats.underrun.load(Ordering::Relaxed), 0);
		assert!(
			r.available() < 4096,
			"fill stays bounded: {}",
			r.available()
		);
	}

	#[test]
	fn the_fifo_never_lets_latency_accumulate() {
		let mut fifo = InputFifo::new();
		let frames = vec![vec![1.0f32; 1024]; 2];
		let mut q = [[0.0f32; QUANTUM]; MAX_INPUT_CHANNELS];
		// Nothing consumed for a while (stopped): trimmed back to a quantum.
		for _ in 0..8 {
			fifo.push(&frames, 2, 1024);
		}
		fifo.push(&frames, 2, 64);
		assert_eq!(fifo.len, QUANTUM + 64);
		fifo.read_quantum(&mut q);
		assert_eq!(q[0], [1.0; QUANTUM]);
		assert_eq!(q[2], [0.0; QUANTUM], "unused channels are silent");
	}
}
