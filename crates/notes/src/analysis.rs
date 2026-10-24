//! Frame analysis of a mono recording (NE-1): the pitch track (pYIN) plus short-window
//! spectral features on one 5 ms grid. This is everything note segmentation (NE-2) reads;
//! it is computed once per audio content and cached on disk ([`Cache`]).

use std::io::{self, Read};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::features::spectral_frames;
use crate::pyin::{self, Candidate, PyinOptions};

/// Bumped whenever the analysis output changes: older cache entries are ignored.
pub const ANALYSIS_VERSION: u32 = 1;
/// Candidates kept per frame (the chosen one first when voiced).
pub const KEPT_CANDIDATES: usize = 3;

#[derive(Debug, Clone, Default)]
pub struct AnalysisOptions {
	pub pyin: PyinOptions,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnalysisFrame {
	/// Seconds (frame center).
	pub time: f64,
	/// Fractional MIDI pitch of the decoded path, `None` when unvoiced.
	pub pitch: Option<f64>,
	/// Voicing mass of the frame, 0..1.
	pub voicing: f64,
	/// Difference-function value of the chosen (or best) dip; 1 = no periodicity.
	pub aperiodicity: f64,
	pub level_db: f64,
	pub flux: f64,
	pub hf_ratio: f64,
	/// Other pitch hypotheses, most likely first (the decoded one first when voiced).
	pub candidates: Vec<Candidate>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Analysis {
	pub version: u32,
	pub sample_rate: u32,
	/// Samples between frames.
	pub hop: u32,
	pub duration: f64,
	pub frames: Vec<AnalysisFrame>,
}

impl Analysis {
	pub fn hop_seconds(&self) -> f64 {
		f64::from(self.hop) / f64::from(self.sample_rate.max(1))
	}

	/// Index of the frame nearest to `seconds`.
	pub fn frame_at(&self, seconds: f64) -> Option<usize> {
		if self.frames.is_empty() {
			return None;
		}
		let i = (seconds / self.hop_seconds()).round().max(0.0) as usize;
		Some(i.min(self.frames.len() - 1))
	}
}

/// Analyzes mono `samples`. `progress` gets 0..1 and returns false to cancel (`None`).
pub fn analyze(
	samples: &[f32],
	sample_rate: u32,
	opts: &AnalysisOptions,
	progress: &mut dyn FnMut(f64) -> bool,
) -> Option<Analysis> {
	let sr = sample_rate.max(1);
	let hop = ((f64::from(sr) * opts.pyin.hop_seconds).round() as usize).max(1);
	let candidate_frames = pyin::candidates(samples, sr, &opts.pyin, &mut |p| progress(0.8 * p))?;
	let path = pyin::decode(&candidate_frames, &opts.pyin);
	let spectral = spectral_frames(samples, sr, hop, candidate_frames.len(), &mut |p| {
		progress(0.8 + 0.2 * p)
	})?;
	let frames = candidate_frames
		.iter()
		.zip(&path)
		.zip(&spectral)
		.map(|((f, choice), s)| {
			let mut candidates = f.candidates.clone();
			if let Some(k) = *choice {
				let chosen = candidates.remove(k);
				candidates.insert(0, chosen);
			}
			candidates.truncate(KEPT_CANDIDATES);
			let aperiodicity = match choice {
				Some(k) => f.candidates[*k].aperiodicity,
				None => f
					.candidates
					.iter()
					.map(|c| c.aperiodicity)
					.fold(1.0, f64::min),
			};
			AnalysisFrame {
				time: f.time,
				pitch: choice.map(|k| f.candidates[k].pitch),
				voicing: f.voiced_mass(),
				aperiodicity,
				level_db: s.level_db,
				flux: s.flux,
				hf_ratio: s.hf_ratio,
				candidates,
			}
		})
		.collect();
	Some(Analysis {
		version: ANALYSIS_VERSION,
		sample_rate: sr,
		hop: hop as u32,
		duration: samples.len() as f64 / f64::from(sr),
		frames,
	})
}

/// Analyses on disk, one file per audio content + options, in a folder that is safe to
/// delete (`.cadenza/analysis/` of a project, or the user cache).
#[derive(Debug, Clone)]
pub struct Cache {
	dir: PathBuf,
}

const MAGIC: &[u8; 8] = b"CDZNOTE\0";

impl Cache {
	pub fn new(dir: impl Into<PathBuf>) -> Self {
		Self { dir: dir.into() }
	}

	pub fn dir(&self) -> &Path {
		&self.dir
	}

	/// Content key: SHA-256 of the samples, rate, analysis version and options.
	pub fn key(samples: &[f32], sample_rate: u32, opts: &AnalysisOptions) -> String {
		let mut h = Sha256::new();
		h.update(ANALYSIS_VERSION.to_le_bytes());
		h.update(sample_rate.to_le_bytes());
		h.update(format!("{:?}", opts.pyin).as_bytes());
		for s in samples {
			h.update(s.to_le_bytes());
		}
		h.finalize().iter().map(|b| format!("{b:02x}")).collect()
	}

	fn path(&self, key: &str) -> PathBuf {
		self.dir.join(format!("{key}.cdzna"))
	}

	/// The cached analysis, or `None` when missing, unreadable or from another version.
	pub fn load(&self, key: &str) -> Option<Analysis> {
		let bytes = std::fs::read(self.path(key)).ok()?;
		decode(&bytes).ok()
	}

	/// Writes atomically (temp file + rename).
	pub fn store(&self, key: &str, analysis: &Analysis) -> io::Result<()> {
		std::fs::create_dir_all(&self.dir)?;
		let target = self.path(key);
		let temp = self.dir.join(format!(".{key}.{}.tmp", std::process::id()));
		std::fs::write(&temp, encode(analysis))?;
		std::fs::rename(&temp, &target)
	}

	/// Cached, else analyzed and stored (a failed store is not an error: the analysis
	/// stands). `None` = cancelled.
	pub fn get_or_analyze(
		&self,
		samples: &[f32],
		sample_rate: u32,
		opts: &AnalysisOptions,
		progress: &mut dyn FnMut(f64) -> bool,
	) -> Option<Analysis> {
		let key = Self::key(samples, sample_rate, opts);
		if let Some(hit) = self.load(&key) {
			progress(1.0);
			return Some(hit);
		}
		let analysis = analyze(samples, sample_rate, opts, progress)?;
		let _ = self.store(&key, &analysis);
		Some(analysis)
	}
}

/// Binary form: magic, version, rate, hop, duration, frame count, then per frame
/// pitch (NaN = unvoiced), voicing, aperiodicity, level, flux, hf ratio and
/// [`KEPT_CANDIDATES`] x (pitch, probability, aperiodicity) with NaN pitch = none; all
/// little-endian f32 except the header.
pub fn encode(a: &Analysis) -> Vec<u8> {
	let mut out = Vec::with_capacity(36 + a.frames.len() * 4 * (6 + 3 * KEPT_CANDIDATES));
	out.extend_from_slice(MAGIC);
	out.extend_from_slice(&a.version.to_le_bytes());
	out.extend_from_slice(&a.sample_rate.to_le_bytes());
	out.extend_from_slice(&a.hop.to_le_bytes());
	out.extend_from_slice(&a.duration.to_le_bytes());
	out.extend_from_slice(&(a.frames.len() as u32).to_le_bytes());
	let mut put = |v: f64| out.extend_from_slice(&(v as f32).to_le_bytes());
	for f in &a.frames {
		put(f.pitch.unwrap_or(f64::NAN));
		put(f.voicing);
		put(f.aperiodicity);
		put(f.level_db);
		put(f.flux);
		put(f.hf_ratio);
		for k in 0..KEPT_CANDIDATES {
			match f.candidates.get(k) {
				Some(c) => {
					put(c.pitch);
					put(c.probability);
					put(c.aperiodicity);
				}
				None => {
					put(f64::NAN);
					put(0.0);
					put(1.0);
				}
			}
		}
	}
	out
}

pub fn decode(bytes: &[u8]) -> io::Result<Analysis> {
	let bad = |what: &str| io::Error::new(io::ErrorKind::InvalidData, what.to_string());
	let mut r = bytes;
	let mut magic = [0u8; 8];
	r.read_exact(&mut magic)?;
	if &magic != MAGIC {
		return Err(bad("not a Cadenza note analysis"));
	}
	let u32_at = |r: &mut &[u8]| -> io::Result<u32> {
		let mut b = [0u8; 4];
		r.read_exact(&mut b)?;
		Ok(u32::from_le_bytes(b))
	};
	let version = u32_at(&mut r)?;
	if version != ANALYSIS_VERSION {
		return Err(bad("analysis from another version"));
	}
	let sample_rate = u32_at(&mut r)?;
	let hop = u32_at(&mut r)?;
	let mut d = [0u8; 8];
	r.read_exact(&mut d)?;
	let duration = f64::from_le_bytes(d);
	let count = u32_at(&mut r)? as usize;
	let per_frame = 4 * (6 + 3 * KEPT_CANDIDATES);
	if r.len() != count * per_frame || sample_rate == 0 || hop == 0 {
		return Err(bad("truncated analysis"));
	}
	let mut get = || -> f64 {
		let (head, rest) = r.split_at(4);
		r = rest;
		f64::from(f32::from_le_bytes(head.try_into().expect("4 bytes")))
	};
	let hop_seconds = f64::from(hop) / f64::from(sample_rate);
	let mut frames = Vec::with_capacity(count);
	for i in 0..count {
		let pitch = get();
		let (voicing, aperiodicity, level_db, flux, hf_ratio) = (get(), get(), get(), get(), get());
		let mut candidates = Vec::with_capacity(KEPT_CANDIDATES);
		for _ in 0..KEPT_CANDIDATES {
			let (p, probability, ap) = (get(), get(), get());
			if !p.is_nan() {
				candidates.push(Candidate {
					pitch: p,
					probability,
					aperiodicity: ap,
				});
			}
		}
		frames.push(AnalysisFrame {
			time: i as f64 * hop_seconds,
			pitch: (!pitch.is_nan()).then_some(pitch),
			voicing,
			aperiodicity,
			level_db,
			flux,
			hf_ratio,
			candidates,
		});
	}
	Ok(Analysis {
		version,
		sample_rate,
		hop,
		duration,
		frames,
	})
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::eval::synth::{Phrase, SynthNote, Vowel, render};

	fn phrase() -> (Vec<f32>, u32) {
		let p = Phrase {
			notes: vec![
				SynthNote::new(57.0, 0.5),
				SynthNote::new(64.0, 0.5).legato(0.08),
			],
			vowel: Vowel::A,
			sample_rate: 22_050,
			snr_db: None,
			reverb: None,
			seed: 3,
		};
		(render(&p).samples, 22_050)
	}

	#[test]
	fn frames_line_up_with_pitch_and_features() {
		let (audio, sr) = phrase();
		let a = analyze(&audio, sr, &AnalysisOptions::default(), &mut |_| true).unwrap();
		assert_eq!(a.hop, 110);
		assert_eq!(a.frames.len(), audio.len().div_ceil(110));
		let f = &a.frames[a.frame_at(0.5).unwrap()];
		assert!((f.pitch.unwrap() - 57.0).abs() < 0.05, "{f:?}");
		assert_eq!(f.candidates[0].pitch, f.pitch.unwrap());
		assert!(f.level_db > -20.0);
		let silent = &a.frames[a.frame_at(0.05).unwrap()];
		assert!(silent.pitch.is_none() && silent.level_db < -100.0);
	}

	#[test]
	fn binary_round_trip_and_cache() {
		let (audio, sr) = phrase();
		let opts = AnalysisOptions::default();
		let a = analyze(&audio, sr, &opts, &mut |_| true).unwrap();
		let b = decode(&encode(&a)).unwrap();
		assert_eq!(b.frames.len(), a.frames.len());
		for (x, y) in a.frames.iter().zip(&b.frames) {
			assert!((x.time - y.time).abs() < 1e-9);
			assert_eq!(x.pitch.is_some(), y.pitch.is_some());
			assert!((x.pitch.unwrap_or(0.0) - y.pitch.unwrap_or(0.0)).abs() < 1e-4);
			assert_eq!(x.candidates.len(), y.candidates.len());
		}
		assert!(decode(&encode(&a)[..40]).is_err());

		let dir = std::env::temp_dir().join(format!("cadenza-notes-cache-{}", std::process::id()));
		let cache = Cache::new(&dir);
		let mut calls = 0;
		let first = cache.get_or_analyze(&audio, sr, &opts, &mut |_| {
			calls += 1;
			true
		});
		assert!(first.is_some() && calls > 1);
		let key = Cache::key(&audio, sr, &opts);
		assert!(cache.load(&key).is_some());
		let mut louder = audio.clone();
		louder[1000] += 0.01;
		assert_ne!(Cache::key(&louder, sr, &opts), key);
		std::fs::remove_dir_all(&dir).unwrap();
	}

	#[test]
	fn chunked_analysis_equals_the_single_threaded_one() {
		let (audio, sr) = phrase();
		let opts = AnalysisOptions::default();
		let one = crate::par::with_threads(1, || analyze(&audio, sr, &opts, &mut |_| true));
		let five = crate::par::with_threads(5, || analyze(&audio, sr, &opts, &mut |_| true));
		assert_eq!(one, five);
	}

	#[test]
	fn cancelling_returns_none() {
		let (audio, sr) = phrase();
		assert!(analyze(&audio, sr, &AnalysisOptions::default(), &mut |p| p < 0.5).is_none());
	}
}
