//! Built-in morphing wavetables (the poly synth's "Wavetable" oscillator).
//!
//! Each table is [`FRAMES`] single-cycle frames; a position `0..=1` morphs
//! linearly between neighbouring frames. Every frame is stored as [`LEVELS`]
//! band-limited copies (mipmaps by harmonic truncation: level `k` keeps the
//! first `MAX_HARMONICS >> k` harmonics), and the player picks the level whose
//! highest harmonic stays below Nyquist, so high notes do not alias. The
//! tables are generated once per process (FFT, off the audio thread) on first
//! use; playback only reads them.

use std::sync::OnceLock;

/// Frames per table.
pub const FRAMES: usize = 32;
/// Harmonics of the brightest mipmap level.
pub const MAX_HARMONICS: usize = 512;
/// Mipmap levels: 512, 256, ... 1 harmonics.
pub const LEVELS: usize = 10;
/// Samples of the analysis grid for time-domain shapes.
const ANALYSIS: usize = 8192;
/// Smallest stored cycle (keeps linear interpolation oversampled).
const MIN_SIZE: usize = 64;

/// Names of the built-in tables, by index (the `oscN_table` param).
pub const TABLE_NAMES: [&str; 5] = ["Basic", "PWM", "Formant", "Harmonics", "Digital"];
/// What each table morphs through as its position goes 0 -> 1 (for the AI and tooltips).
pub const TABLE_DESCRIPTIONS: [&str; 5] = [
	"sine -> triangle -> saw -> square",
	"square -> narrow pulse (pulse-width sweep)",
	"vowel formants a -> e -> i -> o -> u",
	"a bright partial sweeping up the harmonic series over a fundamental",
	"hard-synced, bit-reduced saw (sync ratio 1 -> 7.5)",
];
pub const N_TABLES: usize = TABLE_NAMES.len();

/// One mipmap level: `FRAMES` cycles of `size` samples, each followed by a
/// guard sample (= its first) so interpolation never wraps.
struct Level {
	size: usize,
	data: Vec<f32>,
}

pub struct Table {
	levels: Vec<Level>,
}

impl Table {
	/// The level whose highest harmonic stays below Nyquist at `inc`
	/// cycles/sample.
	#[inline]
	pub fn level_for(inc: f32) -> usize {
		let mut k = 0;
		while k < LEVELS - 1 && (MAX_HARMONICS >> k) as f32 * inc > 0.5 {
			k += 1;
		}
		k
	}

	/// One sample at `phase` (cycles, `0..1`) and morph `position` (`0..=1`)
	/// from mipmap `level`: linear between samples and between frames.
	#[inline]
	pub fn sample(&self, level: usize, position: f32, phase: f32) -> f32 {
		let lv = &self.levels[level.min(LEVELS - 1)];
		let fpos = position.clamp(0.0, 1.0) * (FRAMES - 1) as f32;
		let f0 = (fpos as usize).min(FRAMES - 2);
		let ff = fpos - f0 as f32;
		let x = phase.rem_euclid(1.0) * lv.size as f32;
		let i = (x as usize).min(lv.size - 1);
		let fr = x - i as f32;
		let stride = lv.size + 1;
		let a0 = lv.data[f0 * stride + i];
		let a1 = lv.data[f0 * stride + i + 1];
		let b0 = lv.data[(f0 + 1) * stride + i];
		let b1 = lv.data[(f0 + 1) * stride + i + 1];
		let s0 = a0 + (a1 - a0) * fr;
		let s1 = b0 + (b1 - b0) * fr;
		s0 + (s1 - s0) * ff
	}
}

pub struct Wavetables {
	tables: Vec<Table>,
}

impl Wavetables {
	/// Table `index` (clamped to the last one).
	#[inline]
	pub fn table(&self, index: usize) -> &Table {
		&self.tables[index.min(self.tables.len() - 1)]
	}
}

/// The built-in tables, generated on first call (call it off the audio thread
/// first: `PolySynth::new` does).
pub fn tables() -> &'static Wavetables {
	static TABLES: OnceLock<Wavetables> = OnceLock::new();
	TABLES.get_or_init(|| Wavetables {
		tables: (0..N_TABLES).map(build_table).collect(),
	})
}

/// The waveform of `table` at `position` over one cycle, `out.len()` points
/// (`out[i]` at phase `i / len`), read from the brightest level exactly as
/// the oscillator plays a low note. For the editor's picture.
pub fn wavetable_frame(table: usize, position: f32, out: &mut [f32]) {
	let t = tables().table(table);
	let n = out.len().max(1) as f32;
	for (i, y) in out.iter_mut().enumerate() {
		*y = t.sample(0, position, i as f32 / n);
	}
}

/// A phase accumulator reading a [`Table`].
#[derive(Debug, Clone, Copy, Default)]
pub struct WavetableOsc {
	phase: f32,
}

impl WavetableOsc {
	pub fn set_phase(&mut self, phase: f32) {
		self.phase = phase.rem_euclid(1.0);
	}

	/// One sample, then advances by `inc` cycles/sample. `level` comes from
	/// [`Table::level_for`] (computed at control rate by the caller).
	#[inline]
	pub fn next(&mut self, table: &Table, level: usize, position: f32, inc: f32) -> f32 {
		let y = table.sample(level, position, self.phase);
		self.phase += inc;
		if self.phase >= 1.0 {
			self.phase -= self.phase.floor();
		}
		y
	}
}

/// Harmonic `n` (1-based) of a frame: `(cos, sin)` amplitudes.
type Spectrum = Vec<(f64, f64)>;

fn build_table(index: usize) -> Table {
	let spectra: Vec<Spectrum> = (0..FRAMES)
		.map(|f| frame_spectrum(index, f as f64 / (FRAMES - 1) as f64))
		.collect();
	let mut levels = Vec::with_capacity(LEVELS);
	let mut scales = vec![1.0_f64; FRAMES];
	for k in 0..LEVELS {
		let harmonics = MAX_HARMONICS >> k;
		let size = (harmonics * 4).max(MIN_SIZE);
		let mut data = Vec::with_capacity(FRAMES * (size + 1));
		for (f, spectrum) in spectra.iter().enumerate() {
			let cycle = synthesize(spectrum, harmonics, size);
			if k == 0 {
				// Normalize every frame's full-band peak to 1 (all levels share
				// the scale, so switching level never jumps in loudness).
				let peak = cycle.iter().fold(0.0_f64, |m, x| m.max(x.abs()));
				scales[f] = if peak > 1e-9 { 1.0 / peak } else { 1.0 };
			}
			data.extend(cycle.iter().map(|x| (x * scales[f]) as f32));
			data.push((cycle[0] * scales[f]) as f32);
		}
		levels.push(Level { size, data });
	}
	Table { levels }
}

/// One cycle of `size` samples holding harmonics `1..=harmonics` of `spectrum`.
fn synthesize(spectrum: &Spectrum, harmonics: usize, size: usize) -> Vec<f64> {
	let mut re = vec![0.0; size];
	let mut im = vec![0.0; size];
	for (n, &(a, b)) in spectrum
		.iter()
		.enumerate()
		.take(harmonics.min(size / 2 - 1))
	{
		let h = n + 1;
		re[h] = a * 0.5;
		im[h] = -b * 0.5;
		re[size - h] = a * 0.5;
		im[size - h] = b * 0.5;
	}
	fft(&mut re, &mut im, 1.0);
	re
}

fn frame_spectrum(table: usize, pos: f64) -> Spectrum {
	match table {
		0 => analyze(|t| basic(pos, t)),
		1 => analyze(|t| {
			let width = 0.5 - 0.45 * pos;
			if t < width { 1.0 } else { -1.0 }
		}),
		2 => formant(pos),
		3 => harmonic_sweep(pos),
		_ => analyze(|t| {
			let ratio = 1.0 + 6.5 * pos;
			let saw = 2.0 * (ratio * t).fract() - 1.0;
			let steps = 16.0 - 12.0 * pos;
			(saw * steps).round() / steps
		}),
	}
}

/// Sine -> triangle -> saw -> square, all rising through zero at phase 0.
fn basic(pos: f64, t: f64) -> f64 {
	let sine = (std::f64::consts::TAU * t).sin();
	let tri = if t < 0.25 {
		4.0 * t
	} else if t < 0.75 {
		2.0 - 4.0 * t
	} else {
		4.0 * t - 4.0
	};
	let saw = 2.0 * (t + 0.5).fract() - 1.0;
	let square = if t < 0.5 { 1.0 } else { -1.0 };
	let shapes = [sine, tri, saw, square];
	let x = pos.clamp(0.0, 1.0) * 3.0;
	let i = (x as usize).min(2);
	let f = x - i as f64;
	shapes[i] + (shapes[i + 1] - shapes[i]) * f
}

/// Vowel formants a, e, i, o, u (F1, F2, F3 in Hz) on a 110 Hz voice.
fn formant(pos: f64) -> Spectrum {
	const VOWELS: [[f64; 3]; 5] = [
		[800.0, 1_200.0, 2_500.0],
		[400.0, 2_000.0, 2_600.0],
		[300.0, 2_300.0, 3_000.0],
		[450.0, 800.0, 2_600.0],
		[325.0, 700.0, 2_500.0],
	];
	let x = pos.clamp(0.0, 1.0) * 4.0;
	let i = (x as usize).min(3);
	let f = x - i as f64;
	let formants: Vec<f64> = (0..3)
		.map(|k| (VOWELS[i][k] + (VOWELS[i + 1][k] - VOWELS[i][k]) * f) / 110.0)
		.collect();
	let gains = [1.0, 0.6, 0.25];
	(1..=MAX_HARMONICS)
		.map(|n| {
			let n = n as f64;
			let env: f64 = formants
				.iter()
				.zip(gains)
				.map(|(&h, g)| {
					let bw = 0.8 + 0.06 * h;
					g * (-0.5 * ((n - h) / bw).powi(2)).exp()
				})
				.sum();
			(0.0, (env + 0.03) / n.powf(0.6))
		})
		.collect()
}

/// A fundamental plus a bright partial cluster sweeping harmonics 2 -> 48.
fn harmonic_sweep(pos: f64) -> Spectrum {
	let centre = 2.0 + 46.0 * pos.clamp(0.0, 1.0);
	(1..=MAX_HARMONICS)
		.map(|n| {
			let n = n as f64;
			let fundamental = if n == 1.0 { 1.0 } else { 0.0 };
			let cluster = 0.8 * (-0.5 * ((n - centre) / 2.0).powi(2)).exp();
			(0.0, fundamental + cluster)
		})
		.collect()
}

/// Harmonics `1..=MAX_HARMONICS` of a time-domain cycle `shape(t)`, `t` in `0..1`.
fn analyze(shape: impl Fn(f64) -> f64) -> Spectrum {
	let mut re: Vec<f64> = (0..ANALYSIS)
		.map(|m| shape(m as f64 / ANALYSIS as f64))
		.collect();
	let mut im = vec![0.0; ANALYSIS];
	fft(&mut re, &mut im, -1.0);
	let scale = 2.0 / ANALYSIS as f64;
	(1..=MAX_HARMONICS)
		.map(|n| (re[n] * scale, -im[n] * scale))
		.collect()
}

/// In-place radix-2 complex FFT: `X[k] = sum x[m] e^(sign * i 2 pi k m / n)`.
fn fft(re: &mut [f64], im: &mut [f64], sign: f64) {
	let n = re.len();
	debug_assert!(n.is_power_of_two() && im.len() == n);
	let mut j = 0;
	for i in 1..n {
		let mut bit = n >> 1;
		while j & bit != 0 {
			j ^= bit;
			bit >>= 1;
		}
		j |= bit;
		if i < j {
			re.swap(i, j);
			im.swap(i, j);
		}
	}
	let mut len = 2;
	while len <= n {
		let half = len / 2;
		for k in 0..half {
			let ang = sign * std::f64::consts::TAU * k as f64 / len as f64;
			let (wi, wr) = ang.sin_cos();
			for start in (0..n).step_by(len) {
				let a = start + k;
				let b = a + half;
				let tr = re[b] * wr - im[b] * wi;
				let ti = re[b] * wi + im[b] * wr;
				re[b] = re[a] - tr;
				im[b] = im[a] - ti;
				re[a] += tr;
				im[a] += ti;
			}
		}
		len <<= 1;
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn spectrum_power(x: &[f32]) -> Vec<f64> {
		let n = x.len();
		let mut re: Vec<f64> = x
			.iter()
			.enumerate()
			.map(|(i, &v)| {
				let w = 0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / n as f64).cos();
				v as f64 * w
			})
			.collect();
		let mut im = vec![0.0; n];
		fft(&mut re, &mut im, -1.0);
		(0..n / 2).map(|k| re[k] * re[k] + im[k] * im[k]).collect()
	}

	/// Share of the power that is not within a few bins of a harmonic of `freq`.
	fn inharmonic_share(x: &[f32], freq: f64, sr: f64) -> f64 {
		let power = spectrum_power(x);
		let n = x.len() as f64;
		let total: f64 = power.iter().sum();
		let mut harmonic = 0.0;
		for (k, p) in power.iter().enumerate() {
			let f = k as f64 * sr / n;
			let h = (f / freq).round();
			if h >= 1.0 && ((f - h * freq) * n / sr).abs() <= 4.0 {
				harmonic += p;
			}
		}
		(total - harmonic) / total
	}

	fn play(table: usize, position: f32, freq: f32, sr: f32, level: Option<usize>) -> Vec<f32> {
		let t = tables().table(table);
		let inc = freq / sr;
		let lvl = level.unwrap_or_else(|| Table::level_for(inc));
		let mut osc = WavetableOsc::default();
		(0..16_384)
			.map(|_| osc.next(t, lvl, position, inc))
			.collect()
	}

	#[test]
	fn high_notes_do_not_alias() {
		let sr = 48_000.0;
		// C7 and a note near 5 kHz, saw / square / digital frames: band-limited,
		// every partial lands on a harmonic.
		for &(table, pos) in &[(0, 0.66), (0, 1.0), (1, 0.8), (4, 1.0), (2, 0.5)] {
			for freq in [2_093.0_f32, 4_978.0] {
				let x = play(table, pos, freq, sr, None);
				let share = inharmonic_share(&x, freq as f64, sr as f64);
				assert!(share < 1e-3, "table {table} pos {pos} {freq} Hz: {share}");
				// The full-band level would alias badly: the mipmap is what fixes it.
				if table == 0 && pos == 1.0 {
					let raw = play(table, pos, freq, sr, Some(0));
					assert!(inharmonic_share(&raw, freq as f64, sr as f64) > 1e-2);
				}
			}
		}
	}

	#[test]
	fn morph_is_continuous_and_frames_are_normalized() {
		let mut a = vec![0.0; 256];
		let mut b = vec![0.0; 256];
		for table in 0..N_TABLES {
			for step in 0..200 {
				let p = step as f32 / 200.0;
				wavetable_frame(table, p, &mut a);
				wavetable_frame(table, p + 0.0005, &mut b);
				let diff = a
					.iter()
					.zip(&b)
					.fold(0.0_f32, |m, (x, y)| m.max((x - y).abs()));
				assert!(diff < 0.05, "table {table} at {p}: jump {diff}");
				let peak = a.iter().fold(0.0_f32, |m, x| m.max(x.abs()));
				assert!(
					peak > 0.5 && peak < 1.3,
					"table {table} at {p}: peak {peak}"
				);
			}
		}
		// Basic starts on a sine and ends on a square.
		wavetable_frame(0, 0.0, &mut a);
		assert!((a[64] - 1.0).abs() < 0.01 && a[0].abs() < 0.01);
		wavetable_frame(0, 1.0, &mut a);
		assert!(a[32] > 0.8 && a[160] < -0.8);
	}

	#[test]
	fn level_choice_keeps_harmonics_under_nyquist() {
		assert_eq!(Table::level_for(20.0 / 48_000.0), 0);
		for inc in [0.001_f32, 0.01, 0.05, 0.2, 0.45] {
			let k = Table::level_for(inc);
			assert!(((MAX_HARMONICS >> k) as f32 * inc <= 0.5) || k == LEVELS - 1);
		}
	}
}
