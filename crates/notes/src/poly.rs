//! Polyphonic note detection (NE-8): Basic Pitch (Spotify, ICASSP 2022; code and weights
//! Apache-2.0, see `models/README.md`) ported to plain Rust. The network is tiny (36k
//! weights): a harmonic constant-Q transform (9 octaves x 36 bins by repeated half-band
//! decimation, log-magnitude normalized per 2 s window), harmonic stacking (8 shifted
//! copies: 1/2 and 1..7 times the frequency) and six small convolutions giving per-frame
//! note activations, onsets (88 keys) and a 1/3-semitone pitch contour. Notes come from the
//! published post-processing: onset peaks tracked forward while the activation lasts, then
//! the remaining activation peaks traced both ways ("melodia trick").
//!
//! Detection only: chords and polyphonic parts become notes (to MIDI, a tuning reference,
//! a read-only display). Editing single notes inside a polyphonic mix is not offered (see
//! the plan's legal note).

use std::sync::OnceLock;

/// The model's sample rate.
pub const SAMPLE_RATE: u32 = 22_050;
/// Samples per frame (~11.6 ms).
pub const HOP: usize = 256;
/// MIDI note of key 0.
pub const LOWEST_NOTE: u8 = 21;
pub const KEYS: usize = 88;
/// Contour bins (3 per semitone from A0 - 1/3 semitone... as the model gives them).
pub const CONTOUR_BINS: usize = 264;

const WINDOW: usize = 43_844;
const FRAMES: usize = 172;
/// Frames at each window edge thrown away (half of the 30 overlapping frames).
const EDGE: usize = 15;
const WINDOW_HOP: usize = WINDOW - 2 * EDGE * HOP;
const OCTAVES: usize = 9;
const BINS_PER_OCTAVE: usize = 36;
const CQT_BINS: usize = 309;
const KERNEL: usize = 256;
/// Bin shift of each stacked harmonic (0.5, 1, 2..7 times), 3 bins per semitone.
const HARMONIC_SHIFTS: [isize; 8] = [-36, 0, 36, 57, 72, 84, 93, 101];

struct Conv {
	out: usize,
	inp: usize,
	kh: usize,
	kw: usize,
	stride_w: usize,
	pad_h: usize,
	pad_w: usize,
	w: Vec<f32>,
	b: Vec<f32>,
}

struct Weights {
	cqt_a: Vec<f32>,
	cqt_b: Vec<f32>,
	lowpass: Vec<f32>,
	bin_norm: Vec<f32>,
	bn_scale: f32,
	bn_shift: f32,
	harm: Conv,
	contour1: Conv,
	contour2: Conv,
	note1: Conv,
	note2: Conv,
	onset: Conv,
}

static MODEL: &[u8] = include_bytes!("../models/basic_pitch.bin");

fn weights() -> &'static Weights {
	static W: OnceLock<Weights> = OnceLock::new();
	W.get_or_init(|| {
		assert_eq!(&MODEL[..6], b"CDZBP1", "basic_pitch.bin header");
		let mut at = 6;
		let u32_at = |at: &mut usize| {
			let v = u32::from_le_bytes(MODEL[*at..*at + 4].try_into().expect("4 bytes"));
			*at += 4;
			v as usize
		};
		let count = u32_at(&mut at);
		let mut tensors = Vec::with_capacity(count);
		for _ in 0..count {
			let n = u32_at(&mut at);
			let t: Vec<f32> = MODEL[at..at + 4 * n]
				.as_chunks::<4>()
				.0
				.iter()
				.map(|c| f32::from_le_bytes(*c))
				.collect();
			at += 4 * n;
			tensors.push(t);
		}
		assert_eq!(tensors.len(), 18, "basic_pitch.bin tensors");
		let mut t = tensors.into_iter();
		let mut next = || t.next().expect("tensor");
		let conv = |w: Vec<f32>,
		            b: Vec<f32>,
		            (out, inp, kh, kw): (usize, usize, usize, usize),
		            stride_w: usize,
		            (pad_h, pad_w): (usize, usize)| {
			assert_eq!(w.len(), out * inp * kh * kw);
			assert_eq!(b.len(), out);
			Conv {
				out,
				inp,
				kh,
				kw,
				stride_w,
				pad_h,
				pad_w,
				w,
				b,
			}
		};
		let (cqt_a, cqt_b, lowpass, bin_norm) = (next(), next(), next(), next());
		let (bn_scale, bn_shift) = (next()[0], next()[0]);
		Weights {
			cqt_a,
			cqt_b,
			lowpass,
			bin_norm,
			bn_scale,
			bn_shift,
			harm: conv(next(), next(), (32, 8, 5, 5), 3, (2, 1)),
			contour1: conv(next(), next(), (8, 8, 3, 39), 1, (1, 19)),
			contour2: conv(next(), next(), (1, 8, 5, 5), 1, (2, 2)),
			note1: conv(next(), next(), (32, 1, 7, 7), 3, (3, 2)),
			note2: conv(next(), next(), (1, 32, 7, 3), 1, (3, 1)),
			onset: conv(next(), next(), (1, 33, 3, 3), 1, (1, 1)),
		}
	})
}

/// A [channels, time, freq] tensor.
struct Map {
	c: usize,
	h: usize,
	w: usize,
	v: Vec<f32>,
}

impl Map {
	fn zeros(c: usize, h: usize, w: usize) -> Self {
		Self {
			c,
			h,
			w,
			v: vec![0.0; c * h * w],
		}
	}
}

/// Cross-correlation like ONNX Conv (zero padding, stride on the frequency axis only).
fn conv2d(x: &Map, k: &Conv) -> Map {
	assert_eq!(x.c, k.inp);
	let w_out = (x.w + 2 * k.pad_w - k.kw) / k.stride_w + 1;
	let h_out = x.h + 2 * k.pad_h - k.kh + 1;
	let mut y = Map::zeros(k.out, h_out, w_out);
	for o in 0..k.out {
		let plane = &mut y.v[o * h_out * w_out..(o + 1) * h_out * w_out];
		plane.fill(k.b[o]);
		for c in 0..k.inp {
			let xin = &x.v[c * x.h * x.w..(c + 1) * x.h * x.w];
			for i in 0..k.kh {
				for j in 0..k.kw {
					let wt = k.w[((o * k.inp + c) * k.kh + i) * k.kw + j];
					if wt == 0.0 {
						continue;
					}
					// Output columns whose input column f*s + j - pad lies inside.
					let lo = k.pad_w.saturating_sub(j).div_ceil(k.stride_w);
					let hi = ((x.w + k.pad_w - j).saturating_sub(1)) / k.stride_w + 1;
					let hi = hi.min(w_out);
					if lo >= hi {
						continue;
					}
					for t in 0..h_out {
						let src = t + i;
						if src < k.pad_h || src - k.pad_h >= x.h {
							continue;
						}
						let row = &xin[(src - k.pad_h) * x.w..(src - k.pad_h + 1) * x.w];
						let out = &mut plane[t * w_out..(t + 1) * w_out];
						let first = lo * k.stride_w + j - k.pad_w;
						if k.stride_w == 1 {
							for (o, xv) in out[lo..hi].iter_mut().zip(&row[first..]) {
								*o += wt * xv;
							}
						} else {
							for (n, o) in out[lo..hi].iter_mut().enumerate() {
								*o += wt * row[first + n * k.stride_w];
							}
						}
					}
				}
			}
		}
	}
	y
}

fn relu(m: &mut Map) {
	for v in &mut m.v {
		*v = v.max(0.0);
	}
}

fn sigmoid(m: &mut Map) {
	for v in &mut m.v {
		*v = 1.0 / (1.0 + (-*v).exp());
	}
}

/// numpy "reflect" padding (the edge sample is not repeated).
fn reflect(x: &[f32], pad: usize) -> Vec<f32> {
	let n = x.len();
	let mut out = Vec::with_capacity(n + 2 * pad);
	out.extend((1..=pad).rev().map(|i| x[i.min(n - 1)]));
	out.extend_from_slice(x);
	out.extend((0..pad).map(|i| x[n.saturating_sub(2 + i)]));
	out
}

/// Half-band low-pass and keep every other sample.
fn decimate(x: &[f32], lowpass: &[f32]) -> Vec<f32> {
	let pad = (KERNEL - 1) / 2;
	let mut p = vec![0.0f32; x.len() + 2 * pad];
	p[pad..pad + x.len()].copy_from_slice(x);
	let n = (p.len() - KERNEL) / 2 + 1;
	(0..n)
		.map(|t| {
			p[2 * t..2 * t + KERNEL]
				.iter()
				.zip(lowpass)
				.map(|(a, b)| a * b)
				.sum()
		})
		.collect()
}

/// (contour, note, onset) of one window, each frames x bins, row-major.
type WindowOut = (Vec<f32>, Vec<f32>, Vec<f32>);

/// The model outputs for one window of [`WINDOW`] samples.
fn window(x: &[f32]) -> WindowOut {
	let w = weights();
	debug_assert_eq!(x.len(), WINDOW);
	// Constant-Q magnitudes, octave 0 = the top (full rate).
	let mut power = vec![0.0f32; FRAMES * CQT_BINS];
	let mut signal = x.to_vec();
	for octave in 0..OCTAVES {
		let stride = HOP >> octave;
		let padded = reflect(&signal, KERNEL / 2);
		// Bin of kernel row k in the 309 kept (the lowest 15 of the 324 are dropped).
		let base = (OCTAVES - 1 - octave) * BINS_PER_OCTAVE;
		for f in 0..FRAMES {
			let seg = &padded[f * stride..f * stride + KERNEL];
			for k in 0..BINS_PER_OCTAVE {
				let Some(bin) = (base + k).checked_sub(OCTAVES * BINS_PER_OCTAVE - CQT_BINS) else {
					continue;
				};
				let row = |m: &[f32]| -> f32 {
					m[k * KERNEL..(k + 1) * KERNEL]
						.iter()
						.zip(seg)
						.map(|(a, b)| a * b)
						.sum()
				};
				let n = w.bin_norm[bin];
				let (a, b) = (row(&w.cqt_a) * n, row(&w.cqt_b) * n);
				let mag = (a * a + b * b).sqrt();
				power[f * CQT_BINS + bin] = mag * mag;
			}
		}
		if octave + 1 < OCTAVES {
			signal = decimate(&signal, &w.lowpass);
		}
	}
	// Log magnitude normalized to 0..1 over the window, then the batch norm.
	let db: Vec<f32> = power.iter().map(|p| 10.0 * (p + 1e-10).log10()).collect();
	let min = db.iter().copied().fold(f32::INFINITY, f32::min);
	let max = db.iter().map(|d| d - min).fold(f32::NEG_INFINITY, f32::max);
	let norm: Vec<f32> = db
		.iter()
		.map(|d| {
			let v = if max == 0.0 { 0.0 } else { (d - min) / max };
			v * w.bn_scale + w.bn_shift
		})
		.collect();
	// Harmonic stacking: channel h holds the spectrum shifted down by its harmonic.
	let mut stacked = Map::zeros(HARMONIC_SHIFTS.len(), FRAMES, CONTOUR_BINS);
	for (h, &shift) in HARMONIC_SHIFTS.iter().enumerate() {
		for t in 0..FRAMES {
			for f in 0..CONTOUR_BINS {
				let src = f as isize + shift;
				if (0..CQT_BINS as isize).contains(&src) {
					stacked.v[(h * FRAMES + t) * CONTOUR_BINS + f] =
						norm[t * CQT_BINS + src as usize];
				}
			}
		}
	}
	let mut features = conv2d(&stacked, &w.harm);
	relu(&mut features);
	let mut c1 = conv2d(&stacked, &w.contour1);
	relu(&mut c1);
	let mut contour = conv2d(&c1, &w.contour2);
	sigmoid(&mut contour);
	let mut n1 = conv2d(&contour, &w.note1);
	relu(&mut n1);
	let mut note = conv2d(&n1, &w.note2);
	sigmoid(&mut note);
	let mut both = Map::zeros(1 + features.c, FRAMES, KEYS);
	both.v[..FRAMES * KEYS].copy_from_slice(&note.v);
	both.v[FRAMES * KEYS..].copy_from_slice(&features.v);
	let mut onset = conv2d(&both, &w.onset);
	sigmoid(&mut onset);
	(contour.v, note.v, onset.v)
}

/// Frame-wise model outputs over a whole recording.
#[derive(Debug, Clone, PartialEq)]
pub struct Activations {
	/// Seconds of the recording at each frame's center.
	pub times: Vec<f64>,
	/// frames x [`KEYS`]: a note sounds (0..1).
	pub note: Vec<f32>,
	/// frames x [`KEYS`]: a note starts.
	pub onset: Vec<f32>,
	/// frames x [`CONTOUR_BINS`]: fine pitch salience.
	pub contour: Vec<f32>,
}

impl Activations {
	pub fn frames(&self) -> usize {
		self.times.len()
	}
}

/// Runs the model over `samples` (mono, [`SAMPLE_RATE`]) in overlapping 2 s windows, on up
/// to 8 threads. `progress` gets 0..1 and returns false to cancel (`None`).
pub fn activations(samples: &[f32], progress: &mut dyn FnMut(f64) -> bool) -> Option<Activations> {
	let lead = EDGE * HOP;
	let total = samples.len();
	let windows = (total + lead).div_ceil(WINDOW_HOP).max(1);
	let input = |k: usize| -> Vec<f32> {
		let mut x = vec![0.0f32; WINDOW];
		let start = (k * WINDOW_HOP) as isize - lead as isize;
		for (i, v) in x.iter_mut().enumerate() {
			let s = start + i as isize;
			if s >= 0 && (s as usize) < total {
				*v = samples[s as usize];
			}
		}
		x
	};
	let threads = std::thread::available_parallelism()
		.map_or(1, |n| n.get())
		.clamp(1, 8)
		.min(windows);
	let mut outputs: Vec<Option<WindowOut>> = vec![None; windows];
	let mut done = 0;
	for batch in (0..windows).collect::<Vec<_>>().chunks(threads) {
		let results: Vec<_> = std::thread::scope(|s| {
			let handles: Vec<_> = batch
				.iter()
				.map(|&k| s.spawn(move || (k, window(&input(k)))))
				.collect();
			handles
				.into_iter()
				.map(|h| h.join().expect("a Basic Pitch window"))
				.collect()
		});
		for (k, out) in results {
			outputs[k] = Some(out);
		}
		done += batch.len();
		if !progress(done as f64 / windows as f64) {
			return None;
		}
	}
	let mut a = Activations {
		times: Vec::new(),
		note: Vec::new(),
		onset: Vec::new(),
		contour: Vec::new(),
	};
	let rate = f64::from(SAMPLE_RATE);
	let last_frame = total / HOP;
	'windows: for (k, out) in outputs.into_iter().enumerate() {
		let (contour, note, onset) = out.expect("every window ran");
		for f in EDGE..FRAMES - EDGE {
			// Frame f's center is input sample 256 f of the window.
			let sample = (k * WINDOW_HOP + f * HOP) as f64 - lead as f64;
			if a.times.len() > last_frame {
				break 'windows;
			}
			a.times.push(sample / rate);
			a.note.extend_from_slice(&note[f * KEYS..(f + 1) * KEYS]);
			a.onset.extend_from_slice(&onset[f * KEYS..(f + 1) * KEYS]);
			a.contour
				.extend_from_slice(&contour[f * CONTOUR_BINS..(f + 1) * CONTOUR_BINS]);
		}
	}
	Some(a)
}

/// A detected note of a polyphonic recording.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PolyNote {
	/// Seconds.
	pub onset: f64,
	pub offset: f64,
	/// MIDI note.
	pub pitch: u8,
	/// Mean activation 0..1 (a loudness proxy).
	pub amplitude: f32,
}

/// Note creation settings (Basic Pitch's defaults).
#[derive(Debug, Clone, PartialEq)]
pub struct PolyOptions {
	/// Onset peaks at least this strong start a note.
	pub onset_threshold: f32,
	/// A note lasts while its activation stays above this.
	pub frame_threshold: f32,
	/// Shorter notes are dropped, seconds.
	pub min_length: f64,
	/// Frames below the threshold a note survives (a short dip).
	pub energy_tolerance: usize,
	/// Lowest and highest MIDI note kept.
	pub range: (u8, u8),
	/// Find onsets also where the activation jumps up (soft attacks).
	pub infer_onsets: bool,
	/// Trace notes the onsets missed from the remaining activation.
	pub melodia_trick: bool,
	/// Drop a note sitting on a harmonic (octave, twelfth, two octaves...) of a louder note
	/// that starts with it (within [`GHOST_ONSET`]), at less than `overtone_ratio` of its
	/// activation: the model's overtone ghosts share their fundamental's attack. Off by
	/// default, like the published model: an octave played together is indistinguishable
	/// from a ghost (on a real piano loop the filter took 4 of 116 notes, all real octaves),
	/// while bright synthetic tones (electric piano, synths) do leave ghosts it catches.
	pub overtone_ratio: Option<f32>,
}

impl Default for PolyOptions {
	fn default() -> Self {
		Self {
			onset_threshold: 0.5,
			frame_threshold: 0.3,
			min_length: 0.1277,
			energy_tolerance: 11,
			range: (LOWEST_NOTE, LOWEST_NOTE + KEYS as u8 - 1),
			infer_onsets: true,
			melodia_trick: true,
			overtone_ratio: None,
		}
	}
}

/// The notes in `a`, in time order.
pub fn notes(a: &Activations, o: &PolyOptions) -> Vec<PolyNote> {
	let n = a.frames();
	if n < 3 {
		return Vec::new();
	}
	let at = |m: &[f32], t: usize, k: usize| m[t * KEYS + k];
	let in_range = |k: usize| {
		let p = LOWEST_NOTE as usize + k;
		(usize::from(o.range.0)..=usize::from(o.range.1)).contains(&p)
	};
	let mut frames: Vec<f32> = a.note.clone();
	let mut onsets: Vec<f32> = a.onset.clone();
	for t in 0..n {
		for k in 0..KEYS {
			if !in_range(k) {
				frames[t * KEYS + k] = 0.0;
				onsets[t * KEYS + k] = 0.0;
			}
		}
	}
	if o.infer_onsets {
		// The smallest rise of the activation over 1 and 2 frames, scaled to the onsets.
		let mut diff = vec![0.0f32; n * KEYS];
		for t in 2..n {
			for k in 0..KEYS {
				let v = at(&frames, t, k);
				let d = (v - at(&frames, t - 1, k)).min(v - at(&frames, t - 2, k));
				diff[t * KEYS + k] = d.max(0.0);
			}
		}
		let top_onset = onsets.iter().copied().fold(0.0f32, f32::max);
		let top_diff = diff.iter().copied().fold(0.0f32, f32::max);
		if top_diff > 0.0 {
			for (o, d) in onsets.iter_mut().zip(&diff) {
				*o = o.max(top_onset * d / top_diff);
			}
		}
	}
	let min_len = (o.min_length * f64::from(SAMPLE_RATE) / HOP as f64).round() as usize;
	let mut energy = frames.clone();
	let mut found: Vec<(usize, usize, usize, f32)> = Vec::new();
	let mean = |a: usize, b: usize, k: usize| {
		(a..b).map(|t| at(&frames, t, k)).sum::<f32>() / (b - a).max(1) as f32
	};
	let clear = |energy: &mut [f32], t: usize, k: usize| {
		for kk in k.saturating_sub(1)..=(k + 1).min(KEYS - 1) {
			energy[t * KEYS + kk] = 0.0;
		}
	};
	// Onset peaks (local maxima in time), latest first.
	let mut peaks: Vec<(usize, usize)> = Vec::new();
	for t in 1..n - 1 {
		for k in 0..KEYS {
			let v = at(&onsets, t, k);
			if v >= o.onset_threshold && v > at(&onsets, t - 1, k) && v > at(&onsets, t + 1, k) {
				peaks.push((t, k));
			}
		}
	}
	peaks.sort_by(|a, b| b.cmp(a));
	for (start, k) in peaks {
		if start >= n - 1 {
			continue;
		}
		let (mut i, mut low) = (start + 1, 0);
		while i < n - 1 && low < o.energy_tolerance {
			if energy[i * KEYS + k] < o.frame_threshold {
				low += 1;
			} else {
				low = 0;
			}
			i += 1;
		}
		i -= low;
		if i - start <= min_len {
			continue;
		}
		for t in start..i {
			clear(&mut energy, t, k);
		}
		found.push((start, i, k, mean(start, i, k)));
	}
	if o.melodia_trick {
		loop {
			let (best, value) = energy
				.iter()
				.enumerate()
				.fold((0, 0.0f32), |b, (i, &v)| if v > b.1 { (i, v) } else { b });
			if value <= o.frame_threshold {
				break;
			}
			let (mid, k) = (best / KEYS, best % KEYS);
			energy[best] = 0.0;
			let (mut i, mut low) = (mid + 1, 0);
			while i < n - 1 && low < o.energy_tolerance {
				if energy[i * KEYS + k] < o.frame_threshold {
					low += 1;
				} else {
					low = 0;
				}
				clear(&mut energy, i, k);
				i += 1;
			}
			let end = i - 1 - low;
			let (mut i, mut low) = (mid as isize - 1, 0);
			while i > 0 && low < o.energy_tolerance {
				if energy[i as usize * KEYS + k] < o.frame_threshold {
					low += 1;
				} else {
					low = 0;
				}
				clear(&mut energy, i as usize, k);
				i -= 1;
			}
			let start = (i + 1 + low as isize).max(0) as usize;
			if end <= start || end - start <= min_len {
				continue;
			}
			found.push((start, end, k, mean(start, end, k)));
		}
	}
	let mut out: Vec<PolyNote> = found
		.into_iter()
		.map(|(a_, b, k, amp)| PolyNote {
			onset: a.times[a_],
			offset: a.times[b.min(n - 1)],
			pitch: LOWEST_NOTE + k as u8,
			amplitude: amp,
		})
		.collect();
	out.sort_by(|x, y| x.onset.total_cmp(&y.onset).then(x.pitch.cmp(&y.pitch)));
	match o.overtone_ratio {
		Some(ratio) => without_overtones(&out, ratio),
		None => out,
	}
}

/// `notes` without the likely overtone ghosts: a note on a harmonic of a louder note that
/// starts with it and sounds mostly with it, at less than `ratio` of its activation.
pub fn without_overtones(notes: &[PolyNote], ratio: f32) -> Vec<PolyNote> {
	let ghost = |g: &PolyNote| {
		notes.iter().any(|f| {
			let shared = g.offset.min(f.offset) - g.onset.max(f.onset);
			g.pitch > f.pitch
				&& (g.onset - f.onset).abs() <= GHOST_ONSET
				&& OVERTONES.contains(&(g.pitch - f.pitch))
				&& shared >= 0.5 * (g.offset - g.onset)
				&& g.amplitude < ratio * f.amplitude
		})
	};
	notes.iter().filter(|g| !ghost(g)).copied().collect()
}

/// A ghost starts with its fundamental, seconds.
pub const GHOST_ONSET: f64 = 0.04;

/// Semitones from a fundamental to its harmonics 2..8.
const OVERTONES: [u8; 7] = [12, 19, 24, 28, 31, 34, 36];

/// Share of the sounding time during which two or more of `notes` sound at once (0 when
/// nothing sounds): how polyphonic a part is. Overtone ghosts do not count (a single
/// bright voice leaves some; a chord's notes come in on their own or stay strong).
pub fn polyphony(notes: &[PolyNote]) -> f64 {
	let notes = without_overtones(notes, 0.6);
	let mut edges: Vec<(f64, i32)> = notes
		.iter()
		.flat_map(|n| [(n.onset, 1), (n.offset, -1)])
		.collect();
	edges.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
	let (mut sounding, mut chords, mut depth, mut last) = (0.0, 0.0, 0, 0.0);
	for (t, d) in edges {
		if depth >= 1 {
			sounding += t - last;
		}
		if depth >= 2 {
			chords += t - last;
		}
		depth += d;
		last = t;
	}
	if sounding > 0.0 {
		chords / sounding
	} else {
		0.0
	}
}

/// Notes of a mono recording at any rate: resampled to the model's rate, the model run and
/// the notes made. `None` when cancelled.
pub fn transcribe(
	samples: &[f32],
	rate: u32,
	o: &PolyOptions,
	progress: &mut dyn FnMut(f64) -> bool,
) -> Option<(Activations, Vec<PolyNote>)> {
	let resampled;
	let x = if rate == SAMPLE_RATE {
		samples
	} else {
		resampled = resample(samples, rate);
		&resampled
	};
	let a = activations(x, progress)?;
	let n = notes(&a, o);
	Some((a, n))
}

/// [`transcribe`] through an on-disk cache of the model's activations in `dir` (by the
/// content of `samples`, the rate and [`ACTIVATIONS_VERSION`]): the network runs once per
/// recording, the notes are made again from the stored activations (other options cost
/// nothing). The contour output is not stored (`contour` comes back empty from a hit).
pub fn transcribe_cached(
	samples: &[f32],
	rate: u32,
	o: &PolyOptions,
	dir: &std::path::Path,
	progress: &mut dyn FnMut(f64) -> bool,
) -> Option<(Activations, Vec<PolyNote>)> {
	let path = dir.join(format!("{}.cdzbp", activations_key(samples, rate)));
	if let Some(a) = std::fs::read(&path)
		.ok()
		.and_then(|b| decode_activations(&b))
	{
		progress(1.0);
		let n = notes(&a, o);
		return Some((a, n));
	}
	let (a, n) = transcribe(samples, rate, o, progress)?;
	// A failed store only costs the next run its time.
	let _ = std::fs::create_dir_all(dir).and_then(|()| {
		let temp = dir.join(format!(".{}.tmp", std::process::id()));
		std::fs::write(&temp, encode_activations(&a))?;
		std::fs::rename(&temp, &path)
	});
	Some((a, n))
}

/// Bumped when the activations the model gives change (old cache entries are ignored).
pub const ACTIVATIONS_VERSION: u32 = 1;
const ACTIVATIONS_MAGIC: &[u8; 8] = b"CDZBPA\0\0";

fn activations_key(samples: &[f32], rate: u32) -> String {
	use sha2::{Digest, Sha256};
	let mut h = Sha256::new();
	h.update(ACTIVATIONS_VERSION.to_le_bytes());
	h.update(rate.to_le_bytes());
	for s in samples {
		h.update(s.to_le_bytes());
	}
	h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// Magic, version, frame count, then the frame times (f64) and the note and onset
/// activations (f32), little-endian.
fn encode_activations(a: &Activations) -> Vec<u8> {
	let mut out = Vec::with_capacity(16 + a.frames() * (8 + 8 * KEYS));
	out.extend_from_slice(ACTIVATIONS_MAGIC);
	out.extend_from_slice(&ACTIVATIONS_VERSION.to_le_bytes());
	out.extend_from_slice(&(a.frames() as u32).to_le_bytes());
	for t in &a.times {
		out.extend_from_slice(&t.to_le_bytes());
	}
	for v in a.note.iter().chain(&a.onset) {
		out.extend_from_slice(&v.to_le_bytes());
	}
	out
}

fn decode_activations(bytes: &[u8]) -> Option<Activations> {
	let (magic, rest) = bytes.split_at_checked(8)?;
	if magic != ACTIVATIONS_MAGIC {
		return None;
	}
	let word = |b: &[u8]| u32::from_le_bytes(b.try_into().unwrap_or_default());
	let (version, rest) = rest.split_at_checked(4)?;
	let (count, rest) = rest.split_at_checked(4)?;
	if word(version) != ACTIVATIONS_VERSION {
		return None;
	}
	let frames = word(count) as usize;
	let (times, rest) = rest.split_at_checked(frames * 8)?;
	if rest.len() != frames * KEYS * 8 {
		return None;
	}
	let floats: Vec<f32> = rest
		.as_chunks::<4>()
		.0
		.iter()
		.map(|b| f32::from_le_bytes(*b))
		.collect();
	let (note, onset) = floats.split_at(frames * KEYS);
	Some(Activations {
		times: times
			.as_chunks::<8>()
			.0
			.iter()
			.map(|b| f64::from_le_bytes(*b))
			.collect(),
		note: note.to_vec(),
		onset: onset.to_vec(),
		contour: Vec::new(),
	})
}

/// Windowed-sinc resampling to [`SAMPLE_RATE`] (band-limited to the lower Nyquist).
fn resample(x: &[f32], rate: u32) -> Vec<f32> {
	let ratio = f64::from(SAMPLE_RATE) / f64::from(rate);
	let cutoff = ratio.min(1.0) * 0.95;
	let half = 16.0 / cutoff;
	let len = (x.len() as f64 * ratio).floor() as usize;
	(0..len)
		.map(|i| {
			let center = i as f64 / ratio;
			let lo = (center - half).ceil().max(0.0) as usize;
			let hi = ((center + half).floor() as usize).min(x.len().saturating_sub(1));
			let mut acc = 0.0f64;
			for (j, v) in x.iter().enumerate().take(hi + 1).skip(lo) {
				let d = j as f64 - center;
				let sinc = if d == 0.0 {
					cutoff
				} else {
					(std::f64::consts::PI * cutoff * d).sin() / (std::f64::consts::PI * d)
				};
				let win = 0.5 + 0.5 * (std::f64::consts::PI * d / half).cos();
				acc += f64::from(*v) * sinc * win;
			}
			acc as f32
		})
		.collect()
}

#[cfg(test)]
mod tests {
	use super::*;

	fn tone(pitches: &[(u8, f64, f64)], seconds: f64) -> Vec<f32> {
		let rate = f64::from(SAMPLE_RATE);
		let n = (seconds * rate) as usize;
		let mut x = vec![0.0f32; n];
		for &(p, start, end) in pitches {
			let f = 440.0 * 2f64.powf((f64::from(p) - 69.0) / 12.0);
			for (i, v) in x.iter_mut().enumerate() {
				let t = i as f64 / rate;
				if t < start || t >= end {
					continue;
				}
				let env = (-(t - start) * 1.5).exp() * ((t - start) / 0.01).min(1.0);
				for h in 1..6 {
					*v += (env * (2.0 * std::f64::consts::PI * f * h as f64 * t).sin() * 0.2
						/ h as f64) as f32;
				}
			}
		}
		x
	}

	#[test]
	fn the_cache_gives_the_same_notes_without_running_the_model() {
		let x = tone(&[(60, 0.1, 0.9), (64, 0.1, 0.9)], 1.0);
		let dir = std::env::temp_dir().join(format!("cadenza-bp-cache-{}", std::process::id()));
		let o = PolyOptions::default();
		let (a, first) = transcribe_cached(&x, SAMPLE_RATE, &o, &dir, &mut |_| true).unwrap();
		let stored: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().collect();
		assert_eq!(stored.len(), 1);
		// A hit never asks for progress past the start: a run would report fractions.
		let mut calls = Vec::new();
		let (b, again) = transcribe_cached(&x, SAMPLE_RATE, &o, &dir, &mut |p| {
			calls.push(p);
			true
		})
		.unwrap();
		assert_eq!(calls, [1.0]);
		assert_eq!(first, again);
		assert_eq!((a.times, a.note, a.onset), (b.times, b.note, b.onset));
		assert!(b.contour.is_empty());
		// Other options reuse the activations.
		let fewer = PolyOptions {
			onset_threshold: 0.9,
			..o
		};
		let (_, strict) = transcribe_cached(&x, SAMPLE_RATE, &fewer, &dir, &mut |_| true).unwrap();
		assert!(strict.len() <= first.len());
		std::fs::remove_dir_all(&dir).unwrap();
	}

	#[test]
	fn the_weights_load_and_reflect_padding_matches_numpy() {
		let w = weights();
		assert_eq!(w.cqt_a.len(), 36 * 256);
		assert_eq!(w.bin_norm.len(), CQT_BINS);
		assert_eq!(
			reflect(&[1.0, 2.0, 3.0, 4.0], 2),
			[3.0, 2.0, 1.0, 2.0, 3.0, 4.0, 3.0, 2.0]
		);
	}

	#[test]
	fn a_triad_then_a_bass_note_come_out_as_notes() {
		let x = tone(
			&[
				(60, 0.2, 1.6),
				(64, 0.2, 1.6),
				(67, 0.2, 1.6),
				(45, 2.0, 3.2),
			],
			3.6,
		);
		// A synthetic tone with strong harmonics: the ghosts go with the filter on.
		let options = PolyOptions {
			overtone_ratio: Some(0.6),
			..PolyOptions::default()
		};
		let (a, found) =
			transcribe(&x, SAMPLE_RATE, &options, &mut |_| true).expect("not cancelled");
		assert_eq!(a.frames(), x.len() / HOP + 1);
		let pitches = |from: f64, to: f64| {
			let mut p: Vec<u8> = found
				.iter()
				.filter(|n| n.onset < to && n.offset > from)
				.map(|n| n.pitch)
				.collect();
			p.sort_unstable();
			p.dedup();
			p
		};
		assert_eq!(pitches(0.3, 1.2), [60, 64, 67], "{found:?}");
		assert_eq!(pitches(2.2, 3.0), [45], "{found:?}");
		for n in &found {
			if n.pitch == 60 {
				assert!((n.onset - 0.2).abs() < 0.05, "{n:?}");
			}
		}
	}

	/// Parity with the original model: `CADENZA_BASIC_PITCH_REFERENCE` = the file
	/// `tools/basic-pitch/export.py --reference` writes (onnxruntime outputs for its test
	/// signal).
	#[test]
	fn one_window_matches_the_onnx_model() {
		let Ok(path) = std::env::var("CADENZA_BASIC_PITCH_REFERENCE") else {
			return;
		};
		let bytes = std::fs::read(path).expect("reference file");
		let mut at = 10;
		let mut tensors = Vec::new();
		for _ in 0..4 {
			let n = u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
			at += 4;
			tensors.push(
				bytes[at..at + 4 * n]
					.as_chunks::<4>()
					.0
					.iter()
					.map(|c| f32::from_le_bytes(*c))
					.collect::<Vec<f32>>(),
			);
			at += 4 * n;
		}
		let (contour, note, onset) = window(&tensors[0]);
		for (name, ours, theirs) in [
			("contour", &contour, &tensors[1]),
			("note", &note, &tensors[2]),
			("onset", &onset, &tensors[3]),
		] {
			assert_eq!(ours.len(), theirs.len(), "{name}");
			let worst = ours
				.iter()
				.zip(theirs)
				.map(|(a, b)| (a - b).abs())
				.fold(0.0f32, f32::max);
			eprintln!("{name}: max difference {worst}");
			assert!(worst < 1e-3, "{name}: {worst}");
		}
	}

	#[test]
	fn resampling_keeps_a_tone_in_place() {
		let rate = 44_100;
		let x: Vec<f32> = (0..rate)
			.map(|i| (2.0 * std::f64::consts::PI * 440.0 * i as f64 / f64::from(rate)).sin() as f32)
			.collect();
		let y = resample(&x, rate);
		assert_eq!(y.len(), SAMPLE_RATE as usize);
		// Zero crossings of 440 Hz over the second.
		let crossings = y.windows(2).filter(|w| w[0] <= 0.0 && w[1] > 0.0).count();
		assert!((438..=441).contains(&crossings), "{crossings}");
	}
}
