//! Pure math behind the kit's displays (`ui::device_kit`): the ADSR editor's cube-root
//! time axis, the frequency display's log/dB mapping, note names and the spectrum
//! analyzer (FFT, 1/6-octave smoothing, 12 dB/s fall), and the level history columns.

use std::collections::VecDeque;

use cadenza_engine::ProbeFrame;
use realfft::RealFftPlanner;

use crate::state::device::KnobSpec;
use crate::state::meters::FLOOR_DB;

// --- ADSR -----------------------------------------------------------------------------

/// Envelope values in their parameters' own units (times in ms, sustain as the
/// parameter stores it, e.g. %).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AdsrValues {
	pub attack: f32,
	pub decay: f32,
	pub sustain: f32,
	pub release: f32,
}

/// The four stages, in the order [`AdsrValues`] lists them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AdsrStage {
	Attack,
	Decay,
	Sustain,
	Release,
}

impl AdsrStage {
	pub const ALL: [AdsrStage; 4] = [
		AdsrStage::Attack,
		AdsrStage::Decay,
		AdsrStage::Sustain,
		AdsrStage::Release,
	];

	pub fn index(self) -> usize {
		self as usize
	}

	pub fn letter(self) -> &'static str {
		match self {
			AdsrStage::Attack => "A",
			AdsrStage::Decay => "D",
			AdsrStage::Sustain => "S",
			AdsrStage::Release => "R",
		}
	}
}

impl AdsrValues {
	pub fn get(&self, stage: AdsrStage) -> f32 {
		match stage {
			AdsrStage::Attack => self.attack,
			AdsrStage::Decay => self.decay,
			AdsrStage::Sustain => self.sustain,
			AdsrStage::Release => self.release,
		}
	}

	pub fn set(&mut self, stage: AdsrStage, v: f32) {
		match stage {
			AdsrStage::Attack => self.attack = v,
			AdsrStage::Decay => self.decay = v,
			AdsrStage::Sustain => self.sustain = v,
			AdsrStage::Release => self.release = v,
		}
	}
}

/// Ranges and defaults of the four stages (times in ms).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AdsrRanges {
	pub attack: KnobSpec,
	pub decay: KnobSpec,
	pub sustain: KnobSpec,
	pub release: KnobSpec,
	/// Unit of the sustain value ("%" for the built-ins).
	pub sustain_unit: &'static str,
}

impl AdsrRanges {
	pub fn get(&self, stage: AdsrStage) -> KnobSpec {
		match stage {
			AdsrStage::Attack => self.attack,
			AdsrStage::Decay => self.decay,
			AdsrStage::Sustain => self.sustain,
			AdsrStage::Release => self.release,
		}
	}

	pub fn defaults(&self) -> AdsrValues {
		AdsrValues {
			attack: self.attack.default,
			decay: self.decay.default,
			sustain: self.sustain.default,
			release: self.release.default,
		}
	}
}

/// Share of the width each time stage may take; the sustain hold takes the rest.
pub const ADSR_SLOT: f32 = 0.27;
pub const ADSR_HOLD: f32 = 1.0 - 3.0 * ADSR_SLOT;

/// Position (0..1) of `ms` on a stage's cube-root time axis: 1 ms and 10 s are both
/// a usable distance from the stage start.
pub fn time_to_axis(ms: f32, max_ms: f32) -> f32 {
	(ms / max_ms.max(f32::EPSILON)).clamp(0.0, 1.0).cbrt()
}

pub fn axis_to_time(axis: f32, max_ms: f32) -> f32 {
	axis.clamp(0.0, 1.0).powi(3) * max_ms
}

/// The envelope's corner points in unit coordinates (x 0..1 left to right, y 0..1
/// bottom to top): attack peak, decay end (= sustain level), sustain end, release end.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AdsrPoints {
	pub attack: (f32, f32),
	pub decay: (f32, f32),
	pub sustain: (f32, f32),
	pub release: (f32, f32),
}

impl AdsrPoints {
	/// The node that edits `stage`.
	pub fn node(&self, stage: AdsrStage) -> (f32, f32) {
		match stage {
			AdsrStage::Attack => self.attack,
			AdsrStage::Decay => self.decay,
			AdsrStage::Sustain => self.sustain,
			AdsrStage::Release => self.release,
		}
	}
}

pub fn adsr_points(v: &AdsrValues, r: &AdsrRanges) -> AdsrPoints {
	let level = r.sustain.to_pos(v.sustain);
	let xa = time_to_axis(v.attack, r.attack.max) * ADSR_SLOT;
	let xd = xa + time_to_axis(v.decay, r.decay.max) * ADSR_SLOT;
	let xs = xd + ADSR_HOLD;
	let xr = xs + time_to_axis(v.release, r.release.max) * ADSR_SLOT;
	AdsrPoints {
		attack: (xa, 1.0),
		decay: (xd, level),
		sustain: (xs, level),
		release: (xr, 0.0),
	}
}

/// The envelope drawn as a polyline in unit coordinates: an eased attack and
/// exponential-looking decay and release, `steps` points per segment.
pub fn adsr_curve(p: &AdsrPoints, steps: usize) -> Vec<(f32, f32)> {
	fn seg(out: &mut Vec<(f32, f32)>, a: (f32, f32), b: (f32, f32), steps: usize, power: f32) {
		for i in 1..=steps {
			let t = i as f32 / steps as f32;
			let e = 1.0 - (1.0 - t).powf(power);
			out.push((a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * e));
		}
	}
	let steps = steps.max(2);
	let mut out = Vec::with_capacity(steps * 3 + 2);
	out.push((0.0, 0.0));
	seg(&mut out, (0.0, 0.0), p.attack, steps, 1.6);
	seg(&mut out, p.attack, p.decay, steps, 3.0);
	out.push(p.sustain);
	seg(&mut out, p.sustain, p.release, steps, 3.0);
	out
}

/// Where a sounding voice is on its envelope.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum VoicePos {
	/// Key held, ms since note-on.
	Held(f32),
	/// Key released, ms since the release.
	Released(f32),
}

/// The playhead dot of a voice in unit coordinates (time follows the drawn axis, the
/// level follows the drawn curve's corners linearly).
pub fn voice_point(v: &AdsrValues, r: &AdsrRanges, pos: VoicePos) -> (f32, f32) {
	let p = adsr_points(v, r);
	let along = |from: (f32, f32), to: (f32, f32), t: f32, power: f32| {
		let t = t.clamp(0.0, 1.0);
		let e = 1.0 - (1.0 - t).powf(power);
		(from.0 + (to.0 - from.0) * t, from.1 + (to.1 - from.1) * e)
	};
	match pos {
		VoicePos::Held(ms) => {
			if ms < v.attack {
				along((0.0, 0.0), p.attack, ms / v.attack.max(1e-3), 1.6)
			} else if ms < v.attack + v.decay {
				along(p.attack, p.decay, (ms - v.attack) / v.decay.max(1e-3), 3.0)
			} else {
				// The hold has no length in time: park the dot in its middle.
				((p.decay.0 + p.sustain.0) * 0.5, p.decay.1)
			}
		}
		VoicePos::Released(ms) => along(p.sustain, p.release, ms / v.release.max(1e-3), 3.0),
	}
}

/// Applies a node drag in unit coordinates (`dx`/`dy` as shares of the width/height,
/// y up) to the values the drag started from. Returns the changed values.
pub fn adsr_drag(
	start: &AdsrValues,
	r: &AdsrRanges,
	stage: AdsrStage,
	dx: f32,
	dy: f32,
) -> AdsrValues {
	let mut v = *start;
	let time = |ms: f32, spec: KnobSpec| {
		let axis = time_to_axis(ms, spec.max) + dx / ADSR_SLOT;
		axis_to_time(axis, spec.max).clamp(spec.min, spec.max)
	};
	let level = |s: f32| r.sustain.from_pos(r.sustain.to_pos(s) + dy);
	match stage {
		AdsrStage::Attack => v.attack = time(start.attack, r.attack),
		AdsrStage::Decay => {
			v.decay = time(start.decay, r.decay);
			v.sustain = level(start.sustain);
		}
		AdsrStage::Sustain => v.sustain = level(start.sustain),
		AdsrStage::Release => v.release = time(start.release, r.release),
	}
	v
}

// --- Frequency display ----------------------------------------------------------------

pub const FREQ_MIN: f32 = 20.0;
pub const FREQ_MAX: f32 = 20_000.0;

/// Pixel offset of `freq` on a log 20 Hz–20 kHz axis `width` wide.
pub fn freq_to_x(freq: f32, width: f32) -> f32 {
	width * (freq.max(1.0) / FREQ_MIN).log10() / (FREQ_MAX / FREQ_MIN).log10()
}

pub fn x_to_freq(x: f32, width: f32) -> f32 {
	FREQ_MIN * (FREQ_MAX / FREQ_MIN).powf(x / width.max(1.0))
}

/// The dB span of a frequency display.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DbRange {
	pub min: f32,
	pub max: f32,
	/// Grid step.
	pub step: f32,
}

impl DbRange {
	pub const PM12: DbRange = DbRange {
		min: -12.0,
		max: 12.0,
		step: 6.0,
	};
	pub const PM24: DbRange = DbRange {
		min: -24.0,
		max: 24.0,
		step: 12.0,
	};
	/// Filter responses: deep cuts, a resonance peak.
	pub const FILTER: DbRange = DbRange {
		min: -48.0,
		max: 24.0,
		step: 12.0,
	};

	/// The grid lines from the bottom up.
	pub fn lines(&self) -> impl Iterator<Item = f32> + '_ {
		let n = ((self.max - self.min) / self.step).round() as i32;
		(0..=n).map(move |i| self.min + i as f32 * self.step)
	}
}

/// Pixel offset from the top of `db` on a display `height` tall.
pub fn db_to_y(db: f32, range: DbRange, height: f32) -> f32 {
	(range.max - db) / (range.max - range.min) * height
}

pub fn y_to_db(y: f32, range: DbRange, height: f32) -> f32 {
	range.max - y / height.max(1.0) * (range.max - range.min)
}

/// Unlabelled grid lines (1-2-3...9 per decade).
pub const FREQ_GRID: [f32; 26] = [
	30.0, 40.0, 50.0, 60.0, 70.0, 80.0, 90.0, 100.0, 200.0, 300.0, 400.0, 500.0, 600.0, 700.0,
	800.0, 900.0, 1000.0, 2000.0, 3000.0, 4000.0, 5000.0, 6000.0, 7000.0, 8000.0, 9000.0, 10_000.0,
];

/// The labelled lines of [`FREQ_GRID`].
pub fn freq_label(freq: f32) -> Option<&'static str> {
	Some(match freq as u32 {
		50 => "50",
		100 => "100",
		200 => "200",
		500 => "500",
		1000 => "1k",
		2000 => "2k",
		5000 => "5k",
		10_000 => "10k",
		_ => return None,
	})
}

const NOTE_NAMES: [&str; 12] = [
	"C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
];

/// MIDI note number (fractional) of `freq` (A4 = 69 = 440 Hz).
pub fn freq_to_midi(freq: f32) -> f32 {
	69.0 + 12.0 * (freq.max(1e-3) / 440.0).log2()
}

pub fn midi_to_freq(note: f32) -> f32 {
	440.0 * 2f32.powf((note - 69.0) / 12.0)
}

/// The nearest note name (C4 = 60) and the cents away from it.
pub fn freq_to_note(freq: f32) -> (String, i32) {
	let m = freq_to_midi(freq);
	let r = m.round();
	let cents = ((m - r) * 100.0).round() as i32;
	let n = r as i32;
	(
		format!(
			"{}{}",
			NOTE_NAMES[n.rem_euclid(12) as usize],
			n.div_euclid(12) - 1
		),
		cents,
	)
}

/// "2.8 kHz", "120 Hz".
pub fn short_hz(freq: f32) -> String {
	if freq >= 1000.0 {
		let text = format!("{:.*}", if freq >= 10_000.0 { 1 } else { 2 }, freq / 1000.0);
		let text = text.trim_end_matches('0').trim_end_matches('.');
		format!("{text} kHz")
	} else {
		format!("{freq:.0} Hz")
	}
}

/// The piano strip's hint: "2.8 kHz · F7 +4c".
pub fn note_hint(freq: f32) -> String {
	let (name, cents) = freq_to_note(freq);
	let cents = match cents {
		0 => String::new(),
		c if c > 0 => format!(" +{c}c"),
		c => format!(" -{}c", -c),
	};
	format!("{} · {name}{cents}", short_hz(freq))
}

pub fn is_black_key(note: i32) -> bool {
	matches!(note.rem_euclid(12), 1 | 3 | 6 | 8 | 10)
}

// --- Spectrum analyzer ----------------------------------------------------------------

/// Analyzer points, log-spaced over 20 Hz–20 kHz.
pub const ANALYZER_BINS: usize = 150;
/// The analyzer's floor (an empty analyzer sits here).
pub const ANALYZER_FLOOR_DB: f32 = -90.0;
/// Analyzers fall this fast (dB per second).
pub const ANALYZER_FALL_DB: f32 = 12.0;
/// Longest FFT [`spectrum_bins`] runs.
pub const FFT_MAX: usize = 8192;

/// Centre frequency of analyzer point `i`.
pub fn analyzer_freq(i: usize) -> f32 {
	FREQ_MIN * (FREQ_MAX / FREQ_MIN).powf(i as f32 / (ANALYZER_BINS - 1) as f32)
}

/// Band power of a spectrum over 1/6 octave around each analyzer point, in dB.
/// `power[k]` is the power of FFT bin `k` (`bin_hz` apart) scaled so a sine reads its
/// peak amplitude squared at its bin, and `enbw` the window's noise bandwidth in bins,
/// so a sine reads its level whichever band holds it. Bands narrower than a bin read
/// the interpolated bin power.
pub fn smooth_sixth_octave(power: &[f32], bin_hz: f32, enbw: f32) -> Vec<f32> {
	let half = 2f32.powf(1.0 / 12.0);
	(0..ANALYZER_BINS)
		.map(|i| {
			let fc = analyzer_freq(i);
			let lo = (fc / half / bin_hz).ceil().max(1.0) as usize;
			let hi = ((fc * half / bin_hz).floor() as usize).min(power.len().saturating_sub(1));
			let p = if lo <= hi {
				power[lo..=hi].iter().sum::<f32>() / enbw
			} else {
				let k = fc / bin_hz;
				let k0 = (k.floor() as usize).min(power.len().saturating_sub(1));
				let k1 = (k0 + 1).min(power.len().saturating_sub(1));
				let t = k - k0 as f32;
				power.get(k0).copied().unwrap_or(0.0) * (1.0 - t)
					+ power.get(k1).copied().unwrap_or(0.0) * t
			};
			(10.0 * p.max(1e-12).log10()).max(ANALYZER_FLOOR_DB)
		})
		.collect()
}

/// The 1/6-octave smoothed spectrum (dB, [`ANALYZER_BINS`] points at [`analyzer_freq`])
/// of the latest samples: a Hann-windowed FFT over the last power-of-two run of
/// `samples` (at most [`FFT_MAX`]). A full-scale sine reads 0 dB.
pub fn spectrum_bins(samples: &[f32], sample_rate: f32) -> Vec<f32> {
	let mut n = FFT_MAX;
	while n > samples.len() && n > 64 {
		n /= 2;
	}
	if samples.len() < n || sample_rate <= 0.0 {
		return vec![ANALYZER_FLOOR_DB; ANALYZER_BINS];
	}
	let tail = &samples[samples.len() - n..];
	let window: Vec<f32> = (0..n)
		.map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / n as f32).cos())
		.collect();
	let gain: f32 = window.iter().sum();
	let mut input: Vec<f32> = tail.iter().zip(&window).map(|(s, w)| s * w).collect();
	let fft = RealFftPlanner::<f32>::new().plan_fft_forward(n);
	let mut spectrum = fft.make_output_vec();
	if fft.process(&mut input, &mut spectrum).is_err() {
		return vec![ANALYZER_FLOOR_DB; ANALYZER_BINS];
	}
	let scale = 2.0 / gain;
	let power: Vec<f32> = spectrum
		.iter()
		.map(|c| (c.norm() * scale).powi(2))
		.collect();
	// Hann: 1.5 bins of noise bandwidth.
	smooth_sixth_octave(&power, sample_rate / n as f32, 1.5)
}

/// The analyzer as drawn: rises at once, falls at 12 dB/s, decays to empty when no
/// data comes (transport stopped).
#[derive(Debug, Clone, PartialEq)]
pub struct AnalyzerState {
	pub bins: Vec<f32>,
}

impl Default for AnalyzerState {
	fn default() -> Self {
		Self {
			bins: vec![ANALYZER_FLOOR_DB; ANALYZER_BINS],
		}
	}
}

impl AnalyzerState {
	/// A new frame `dt` seconds after the last one.
	pub fn update(&mut self, frame: &[f32], dt: f32) {
		let fall = ANALYZER_FALL_DB * dt.max(0.0);
		for (b, &f) in self.bins.iter_mut().zip(frame) {
			*b = f.max(*b - fall).max(ANALYZER_FLOOR_DB);
		}
	}

	/// No data for `dt` seconds.
	pub fn decay(&mut self, dt: f32) {
		let fall = ANALYZER_FALL_DB * dt.max(0.0);
		for b in &mut self.bins {
			*b = (*b - fall).max(ANALYZER_FLOOR_DB);
		}
	}

	pub fn is_empty(&self) -> bool {
		self.bins.iter().all(|&b| b <= ANALYZER_FLOOR_DB)
	}
}

/// Which side of a probed device a display follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tap {
	Input,
	Output,
}

fn mono(frame: &ProbeFrame, tap: Tap) -> f32 {
	let s = match tap {
		Tap::Input => frame.input,
		Tap::Output => frame.output,
	};
	(s[0] + s[1]) * 0.5
}

/// The latest mono samples of one side of a probe, for [`spectrum_bins`].
#[derive(Debug, Clone)]
pub struct SampleWindow {
	samples: VecDeque<f32>,
	capacity: usize,
}

impl SampleWindow {
	pub fn new(capacity: usize) -> Self {
		Self {
			samples: VecDeque::with_capacity(capacity),
			capacity: capacity.max(1),
		}
	}

	pub fn push_frames(&mut self, frames: &[ProbeFrame], tap: Tap) {
		for f in frames {
			if self.samples.len() == self.capacity {
				self.samples.pop_front();
			}
			self.samples.push_back(mono(f, tap));
		}
	}

	/// The samples, oldest first.
	pub fn samples(&mut self) -> &[f32] {
		self.samples.make_contiguous()
	}

	pub fn clear(&mut self) {
		self.samples.clear();
	}
}

// --- Level history --------------------------------------------------------------------

/// One column of a level history: sample extremes of the input and output, the
/// largest gain reduction (positive dB) and the loudest sidechain key level (dBFS,
/// `FLOOR_DB` without a key) while it was collected.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HistoryColumn {
	pub input: (f32, f32),
	pub output: (f32, f32),
	pub gr_db: f32,
	pub key_db: f32,
}

impl HistoryColumn {
	const EMPTY: HistoryColumn = HistoryColumn {
		input: (0.0, 0.0),
		output: (0.0, 0.0),
		gr_db: 0.0,
		key_db: crate::state::meters::FLOOR_DB,
	};

	/// Peak level of a side in dBFS.
	pub fn peak_db(range: (f32, f32)) -> f32 {
		let a = range.0.abs().max(range.1.abs());
		if a <= 1e-7 {
			crate::state::meters::FLOOR_DB
		} else {
			20.0 * a.log10()
		}
	}
}

/// A scrolling level history: a ring of [`HistoryColumn`]s, oldest first, filled from
/// probe frames at `frames_per_column`.
#[derive(Debug, Clone)]
pub struct LevelHistory {
	columns: VecDeque<HistoryColumn>,
	capacity: usize,
	frames_per_column: usize,
	current: HistoryColumn,
	collected: usize,
	key_db: Option<f32>,
}

impl LevelHistory {
	/// `columns` columns covering `seconds` at `sample_rate`.
	pub fn new(columns: usize, seconds: f32, sample_rate: u32) -> Self {
		let columns = columns.max(1);
		Self {
			columns: VecDeque::with_capacity(columns),
			capacity: columns,
			frames_per_column: ((seconds * sample_rate as f32) / columns as f32)
				.round()
				.max(1.0) as usize,
			current: HistoryColumn::EMPTY,
			collected: 0,
			key_db: None,
		}
	}

	/// The sidechain key level (dBFS) the next frames are collected with; `None` = no key.
	pub fn set_key(&mut self, key_db: Option<f32>) {
		self.key_db = key_db;
	}

	pub fn capacity(&self) -> usize {
		self.capacity
	}

	pub fn frames_per_column(&self) -> usize {
		self.frames_per_column
	}

	/// Complete columns, oldest first.
	pub fn columns(&self) -> impl ExactSizeIterator<Item = &HistoryColumn> + DoubleEndedIterator {
		self.columns.iter()
	}

	pub fn len(&self) -> usize {
		self.columns.len()
	}

	pub fn is_empty(&self) -> bool {
		self.columns.is_empty()
	}

	pub fn clear(&mut self) {
		self.columns.clear();
		self.current = HistoryColumn::EMPTY;
		self.collected = 0;
	}

	/// One frame of input/output samples (stereo) with the gain reduction now.
	pub fn push(&mut self, input: [f32; 2], output: [f32; 2], gr_db: f32) {
		let c = &mut self.current;
		for s in input {
			c.input.0 = c.input.0.min(s);
			c.input.1 = c.input.1.max(s);
		}
		for s in output {
			c.output.0 = c.output.0.min(s);
			c.output.1 = c.output.1.max(s);
		}
		c.gr_db = c.gr_db.max(gr_db.abs());
		if let Some(key) = self.key_db {
			c.key_db = c.key_db.max(key);
		}
		self.collected += 1;
		if self.collected >= self.frames_per_column {
			if self.columns.len() == self.capacity {
				self.columns.pop_front();
			}
			self.columns.push_back(self.current);
			self.current = HistoryColumn::EMPTY;
			self.collected = 0;
		}
	}

	/// Probe frames drained this UI frame; `gr_db` is the device's latest reading.
	pub fn push_frames(&mut self, frames: &[ProbeFrame], gr_db: f32) {
		for f in frames {
			self.push(f.input, f.output, gr_db);
		}
	}
}

/// Half-height fraction of a level on a linear (waveform) history whose top is
/// `max_db`: amplitudes, not dB, so a waveform mirrors around the centre line.
pub fn wave_fraction(db: f32, max_db: f32) -> f32 {
	10f32.powf((db - max_db) / 20.0).clamp(0.0, 1.0)
}

/// Inverse of [`wave_fraction`] (`FLOOR_DB` at the centre line).
pub fn wave_db(fraction: f32, max_db: f32) -> f32 {
	let f = fraction.abs();
	if f <= 1e-6 {
		FLOOR_DB
	} else {
		(max_db + 20.0 * f.log10()).max(FLOOR_DB)
	}
}

// --- Header meters --------------------------------------------------------------------

/// The device header's IN / OUT peaks per channel (dBFS, `FLOOR_DB` = silence): each
/// frame's probe frames raise them, and they fall at [`IoLevels::FALL_DB`] per second.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct IoLevels {
	pub input: [f32; 2],
	pub output: [f32; 2],
}

impl Default for IoLevels {
	fn default() -> Self {
		Self {
			input: [FLOOR_DB; 2],
			output: [FLOOR_DB; 2],
		}
	}
}

impl IoLevels {
	pub const FALL_DB: f32 = 24.0;

	pub fn update(&mut self, frames: &[ProbeFrame], dt: f32) {
		let mut peak = [[0.0f32; 2]; 2];
		for f in frames {
			for (side, signal) in peak.iter_mut().zip([f.input, f.output]) {
				for (p, s) in side.iter_mut().zip(signal) {
					*p = p.max(s.abs());
				}
			}
		}
		let fall = Self::FALL_DB * dt.max(0.0);
		for (levels, peak) in [&mut self.input, &mut self.output].into_iter().zip(peak) {
			for (level, amp) in levels.iter_mut().zip(peak) {
				*level = crate::state::meters::amp_to_db(amp)
					.max(*level - fall)
					.max(FLOOR_DB);
			}
		}
	}

	/// Nothing to show (all meters at the floor).
	pub fn is_silent(&self) -> bool {
		self.input
			.iter()
			.chain(&self.output)
			.all(|&db| db <= FLOOR_DB)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn ranges() -> AdsrRanges {
		AdsrRanges {
			attack: KnobSpec::new(0.0, 10_000.0, 5.0),
			decay: KnobSpec::new(1.0, 10_000.0, 400.0),
			sustain: KnobSpec::new(0.0, 100.0, 70.0),
			release: KnobSpec::new(1.0, 20_000.0, 250.0),
			sustain_unit: "%",
		}
	}

	#[test]
	fn cube_root_axis_spreads_short_and_long_times() {
		assert!((time_to_axis(10_000.0, 10_000.0) - 1.0).abs() < 1e-6);
		assert!((time_to_axis(1250.0, 10_000.0) - 0.5).abs() < 1e-5);
		// 1 ms is still a few percent of the slot, 10 s the whole of it.
		let one = time_to_axis(1.0, 10_000.0);
		assert!(one > 0.04 && one < 0.05, "{one}");
		for ms in [0.0, 1.0, 37.0, 400.0, 9_999.0] {
			assert!(
				(axis_to_time(time_to_axis(ms, 10_000.0), 10_000.0) - ms).abs()
					< 0.01 * ms.max(1.0)
			);
		}
		assert_eq!(axis_to_time(2.0, 100.0), 100.0);
	}

	#[test]
	fn adsr_points_follow_the_values() {
		let r = ranges();
		let v = AdsrValues {
			attack: 10_000.0,
			decay: 10_000.0,
			sustain: 50.0,
			release: 20_000.0,
		};
		let p = adsr_points(&v, &r);
		assert!((p.attack.0 - ADSR_SLOT).abs() < 1e-6);
		assert_eq!(p.attack.1, 1.0);
		assert!((p.decay.0 - 2.0 * ADSR_SLOT).abs() < 1e-6);
		assert!((p.decay.1 - 0.5).abs() < 1e-6);
		assert_eq!(p.sustain.1, p.decay.1);
		assert!((p.release.0 - 1.0).abs() < 1e-5);
		assert_eq!(p.release.1, 0.0);
		let curve = adsr_curve(&p, 16);
		assert_eq!(curve.first(), Some(&(0.0, 0.0)));
		assert_eq!(*curve.last().unwrap(), p.release);
		assert!(curve.contains(&p.sustain));
		assert!(
			curve.windows(2).all(|w| w[1].0 >= w[0].0 - 1e-6),
			"x never goes back"
		);
	}

	#[test]
	fn adsr_drag_moves_each_node_on_its_axes() {
		let r = ranges();
		let start = r.defaults();
		let a = adsr_drag(&start, &r, AdsrStage::Attack, 0.05, 0.3);
		assert!(a.attack > start.attack);
		assert_eq!(
			(a.decay, a.sustain, a.release),
			(start.decay, start.sustain, start.release)
		);
		let d = adsr_drag(&start, &r, AdsrStage::Decay, -0.02, -0.2);
		assert!(d.decay < start.decay);
		assert!((d.sustain - 50.0).abs() < 1e-3);
		let s = adsr_drag(&start, &r, AdsrStage::Sustain, 0.3, 0.5);
		assert_eq!(s.decay, start.decay);
		assert_eq!(s.sustain, 100.0);
		let rel = adsr_drag(&start, &r, AdsrStage::Release, -1.0, 0.0);
		assert_eq!(rel.release, 1.0, "clamped to the minimum");
	}

	#[test]
	fn voice_dots_ride_the_envelope() {
		let r = ranges();
		let v = r.defaults();
		let p = adsr_points(&v, &r);
		assert_eq!(voice_point(&v, &r, VoicePos::Held(0.0)), (0.0, 0.0));
		let peak = voice_point(&v, &r, VoicePos::Held(v.attack));
		assert!((peak.0 - p.attack.0).abs() < 1e-5 && (peak.1 - 1.0).abs() < 1e-5);
		let held = voice_point(&v, &r, VoicePos::Held(5000.0));
		assert!(held.0 > p.decay.0 && held.0 < p.sustain.0);
		let end = voice_point(&v, &r, VoicePos::Released(1e6));
		assert!((end.0 - p.release.0).abs() < 1e-5 && end.1.abs() < 1e-5);
	}

	#[test]
	fn frequency_and_db_axes_round_trip() {
		assert_eq!(freq_to_x(20.0, 600.0), 0.0);
		assert!((freq_to_x(20_000.0, 600.0) - 600.0).abs() < 1e-3);
		// 200 Hz and 2 kHz are a third and two thirds of the way.
		assert!((freq_to_x(200.0, 600.0) - 200.0).abs() < 1e-3);
		assert!((freq_to_x(2000.0, 600.0) - 400.0).abs() < 1e-3);
		for f in [20.0, 63.0, 1000.0, 12_345.0] {
			assert!((x_to_freq(freq_to_x(f, 777.0), 777.0) - f).abs() < 1e-3 * f);
		}
		assert_eq!(db_to_y(12.0, DbRange::PM12, 200.0), 0.0);
		assert_eq!(db_to_y(0.0, DbRange::PM12, 200.0), 100.0);
		assert_eq!(db_to_y(-48.0, DbRange::FILTER, 144.0), 144.0);
		assert_eq!(db_to_y(0.0, DbRange::FILTER, 144.0), 48.0);
		assert!(
			(y_to_db(db_to_y(-7.5, DbRange::PM24, 300.0), DbRange::PM24, 300.0) + 7.5).abs() < 1e-4
		);
		assert_eq!(
			DbRange::PM24.lines().collect::<Vec<_>>(),
			[-24.0, -12.0, 0.0, 12.0, 24.0]
		);
		assert_eq!(DbRange::FILTER.lines().count(), 7);
		let labels: Vec<_> = FREQ_GRID.iter().filter_map(|&f| freq_label(f)).collect();
		assert_eq!(labels, ["50", "100", "200", "500", "1k", "2k", "5k", "10k"]);
	}

	#[test]
	fn frequencies_name_their_notes() {
		assert_eq!(freq_to_note(440.0), ("A4".to_owned(), 0));
		assert_eq!(freq_to_note(261.63), ("C4".to_owned(), 0));
		assert_eq!(freq_to_note(2800.0), ("F7".to_owned(), 4));
		assert_eq!(freq_to_note(27.5), ("A0".to_owned(), 0));
		let (name, cents) = freq_to_note(450.0);
		assert_eq!((name.as_str(), cents), ("A4", 39));
		assert_eq!(freq_to_note(452.9).0, "A#4");
		assert_eq!(note_hint(2800.0), "2.8 kHz · F7 +4c");
		assert_eq!(note_hint(440.0), "440 Hz · A4");
		assert_eq!(note_hint(430.0), "430 Hz · A4 -40c");
		assert_eq!(short_hz(10_000.0), "10 kHz");
		assert_eq!(short_hz(1250.0), "1.25 kHz");
		assert!(is_black_key(61) && !is_black_key(60) && is_black_key(-2));
		assert!((midi_to_freq(freq_to_midi(1234.0)) - 1234.0).abs() < 0.01);
	}

	fn sine(freq: f32, amp: f32, sr: f32, n: usize) -> Vec<f32> {
		(0..n)
			.map(|i| amp * (std::f32::consts::TAU * freq * i as f32 / sr).sin())
			.collect()
	}

	fn nearest_bin(freq: f32) -> usize {
		(0..ANALYZER_BINS)
			.min_by(|&a, &b| {
				(analyzer_freq(a) / freq)
					.ln()
					.abs()
					.total_cmp(&(analyzer_freq(b) / freq).ln().abs())
			})
			.unwrap()
	}

	#[test]
	fn spectrum_bins_read_a_sine_at_its_level() {
		let sr = 48_000.0;
		for (freq, amp) in [(1000.0, 0.5), (5000.0, 1.0), (150.0, 0.25)] {
			let bins = spectrum_bins(&sine(freq, amp, sr, 10_000), sr);
			assert_eq!(bins.len(), ANALYZER_BINS);
			let at = bins[nearest_bin(freq)];
			let want = 20.0 * f32::log10(amp);
			assert!((at - want).abs() < 1.5, "{freq} Hz: {at} vs {want}");
			// Two octaves away the sine is gone.
			let far = bins[nearest_bin(freq * 4.0)];
			assert!(far < want - 40.0, "{freq} Hz leaks {far}");
		}
		assert!(
			spectrum_bins(&[0.0; 16], sr)
				.iter()
				.all(|&b| b == ANALYZER_FLOOR_DB)
		);
		assert!(
			spectrum_bins(&vec![0.0; 4096], sr)
				.iter()
				.all(|&b| b == ANALYZER_FLOOR_DB)
		);
	}

	#[test]
	fn sixth_octave_bands_sum_their_bins() {
		// Flat unit power density: a band's level grows with its width (1/6 octave
		// = about 11.6 % of its centre).
		let bin_hz = 10.0;
		let power = vec![1.0; 2001];
		let bins = smooth_sixth_octave(&power, bin_hz, 1.0);
		let i = nearest_bin(10_000.0);
		let width_bins =
			analyzer_freq(i) * (2f32.powf(1.0 / 12.0) - 2f32.powf(-1.0 / 12.0)) / bin_hz;
		assert!(
			(bins[i] - 10.0 * width_bins.log10()).abs() < 0.3,
			"{}",
			bins[i]
		);
		// Narrower than a bin: interpolated, not zero.
		let coarse = smooth_sixth_octave(&[1.0; 64], 50.0, 1.0);
		assert!(coarse[0].abs() < 1e-3);
	}

	#[test]
	fn analyzer_rises_at_once_and_falls_at_12_db_per_second() {
		let mut a = AnalyzerState::default();
		assert!(a.is_empty());
		let mut frame = vec![ANALYZER_FLOOR_DB; ANALYZER_BINS];
		frame[10] = -20.0;
		a.update(&frame, 1.0 / 60.0);
		assert_eq!(a.bins[10], -20.0);
		frame[10] = -60.0;
		a.update(&frame, 0.5);
		assert!((a.bins[10] + 26.0).abs() < 1e-4);
		a.decay(1.0);
		assert!((a.bins[10] + 38.0).abs() < 1e-4);
		a.decay(10.0);
		assert!(a.is_empty());
	}

	#[test]
	fn sample_window_keeps_the_latest_mono_samples() {
		let mut w = SampleWindow::new(3);
		let frames: Vec<ProbeFrame> = (0..5)
			.map(|i| ProbeFrame {
				input: [i as f32, i as f32 + 2.0],
				output: [0.0, 0.0],
			})
			.collect();
		w.push_frames(&frames, Tap::Input);
		assert_eq!(w.samples(), [3.0, 4.0, 5.0]);
		w.push_frames(&frames[..1], Tap::Output);
		assert_eq!(w.samples(), [4.0, 5.0, 0.0]);
	}

	#[test]
	fn level_history_accumulates_columns() {
		let mut h = LevelHistory::new(4, 1.0, 400);
		assert_eq!(h.frames_per_column(), 100);
		for i in 0..250 {
			let s = if i == 30 { 0.9 } else { 0.1 };
			h.push(
				[s, -s * 0.5],
				[s * 0.5, 0.0],
				if i == 120 { 6.0 } else { 1.0 },
			);
		}
		assert_eq!(h.len(), 2, "the half column is still collecting");
		let cols: Vec<_> = h.columns().copied().collect();
		assert_eq!(cols[0].input, (-0.45, 0.9));
		assert_eq!(cols[0].output, (0.0, 0.45));
		assert_eq!(cols[0].gr_db, 1.0);
		assert_eq!(cols[1].gr_db, 6.0);
		assert_eq!(cols[1].input, (-0.05, 0.1));
		assert!((HistoryColumn::peak_db(cols[0].input) - 20.0 * 0.9f32.log10()).abs() < 1e-4);
		for _ in 0..1000 {
			h.push([0.0; 2], [0.0; 2], 0.0);
		}
		assert_eq!(h.len(), 4, "the ring keeps its capacity");
		assert_eq!(
			HistoryColumn::peak_db(h.columns().last().unwrap().input),
			crate::state::meters::FLOOR_DB
		);
		let frames = [ProbeFrame {
			input: [0.5, 0.5],
			output: [0.25, 0.25],
		}; 100];
		h.push_frames(&frames, -3.0);
		let last = h.columns().last().unwrap();
		assert_eq!((last.input.1, last.output.1, last.gr_db), (0.5, 0.25, 3.0));
		assert_eq!(last.key_db, crate::state::meters::FLOOR_DB, "no key");
		h.set_key(Some(-12.0));
		h.push_frames(&frames, 0.0);
		assert_eq!(h.columns().last().unwrap().key_db, -12.0);
		h.clear();
		assert!(h.is_empty());
	}

	#[test]
	fn io_levels_take_peaks_and_fall() {
		let mut l = IoLevels::default();
		assert!(l.is_silent());
		let frames = [
			ProbeFrame {
				input: [0.5, -0.25],
				output: [1.0, 0.0],
			},
			ProbeFrame {
				input: [-0.1, 0.1],
				output: [0.0, -0.5],
			},
		];
		l.update(&frames, 0.0);
		assert!((l.input[0] - -6.02).abs() < 0.05, "{:?}", l);
		assert!((l.input[1] - -12.04).abs() < 0.05);
		assert!(l.output[0].abs() < 0.01 && (l.output[1] - -6.02).abs() < 0.05);
		// No new frames: half a second falls 12 dB.
		l.update(&[], 0.5);
		assert!((l.input[0] - -18.02).abs() < 0.05);
		for _ in 0..20 {
			l.update(&[], 0.5);
		}
		assert!(l.is_silent());
	}

	#[test]
	fn waveform_levels_are_linear_amplitudes() {
		assert_eq!(wave_fraction(4.0, 4.0), 1.0);
		assert!((wave_fraction(-2.0, 4.0) - 0.5012).abs() < 1e-3);
		assert_eq!(wave_fraction(10.0, 4.0), 1.0);
		assert!((wave_db(wave_fraction(-7.5, 4.0), 4.0) + 7.5).abs() < 1e-4);
		assert_eq!(wave_db(0.0, 4.0), FLOOR_DB);
		assert!((wave_db(-0.5, 0.0) + 6.0206).abs() < 1e-3, "either half");
	}
}
