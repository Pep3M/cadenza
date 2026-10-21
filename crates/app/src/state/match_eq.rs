//! Match EQ in the EQ editor (T31): pick a reference (an audio file or another track), the
//! song renders on a worker with the EQ and the inserts after it bypassed
//! (`cadenza_mcp::tools::match_eq::analyze_match_sources`, progress + cancel), the fit is
//! previewed as a dashed curve while the amount / max gain change it (pure, cheap), and
//! Apply commits it as ONE undo step "Match EQ to <reference>".

use crate::i18n::{tr, trf};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};

use cadenza_analysis::match_eq::MatchOptions;
use cadenza_analysis::spectrum::THIRD_OCTAVE_HZ;
use cadenza_core::TrackId;
use cadenza_mcp::tools::match_eq::{
	DEFAULT_MAX_GAIN_DB, MatchPlan, MatchReference, MatchSpectra, analyze_match_sources,
	match_label, plan_match,
};

use super::AppState;
use super::device_display::{ANALYZER_BINS, analyzer_freq};
use super::eq_editor::EqValues;
use super::selection::DeviceRef;

enum JobEvent {
	Progress(f32),
	Done(Result<MatchSpectra, String>),
}

/// Where a match is.
#[derive(Debug, Clone, PartialEq)]
pub enum MatchPhase {
	/// Rendering and analyzing (fraction done).
	Running(f32),
	/// Spectra ready: the proposal follows the amount / max gain.
	Ready(MatchSpectra),
	Failed(String),
}

/// One match on one EQ.
pub struct MatchEq {
	pub device: DeviceRef,
	/// File name or track name (the undo label names it).
	pub reference_name: String,
	/// 0..=100 % of the difference.
	pub amount: f32,
	pub max_gain_db: f32,
	pub phase: MatchPhase,
	rx: Option<Receiver<JobEvent>>,
	cancel: Arc<AtomicBool>,
}

impl MatchEq {
	pub fn options(&self) -> MatchOptions {
		MatchOptions {
			amount: self.amount.clamp(0.0, 100.0) / 100.0,
			max_gain_db: self.max_gain_db,
			..MatchOptions::default()
		}
	}

	/// A match made from ready spectra (tests, screenshots).
	pub fn ready(device: DeviceRef, spectra: MatchSpectra) -> Self {
		Self {
			device,
			reference_name: spectra.reference_name.clone(),
			amount: 100.0,
			max_gain_db: DEFAULT_MAX_GAIN_DB,
			phase: MatchPhase::Ready(spectra),
			rx: None,
			cancel: Arc::new(AtomicBool::new(false)),
		}
	}
}

/// The match flow of the EQ editors (one at a time).
#[derive(Default)]
pub struct MatchEqState {
	/// The device waiting for its reference file from the file dialog.
	pub pending_file: Option<DeviceRef>,
	pub current: Option<MatchEq>,
}

/// `values` with `changes` applied: the proposed EQ (dashed curve).
pub fn proposed_values(values: &EqValues, changes: &[(&'static str, f32)]) -> EqValues {
	let mut out = values.clone();
	out.apply(changes);
	out
}

/// A third-octave spectrum (dB per [`THIRD_OCTAVE_HZ`]) at the display's analyzer points,
/// interpolated in log frequency and shifted by `offset_db`.
pub fn analyzer_bins(third_octave: &[f32], offset_db: f32) -> Vec<f32> {
	let n = THIRD_OCTAVE_HZ.len().min(third_octave.len());
	if n == 0 {
		return Vec::new();
	}
	(0..ANALYZER_BINS)
		.map(|i| {
			let f = analyzer_freq(i);
			let pos = ((f / THIRD_OCTAVE_HZ[0]).log2() * 3.0).clamp(0.0, (n - 1) as f32);
			let lo = pos.floor() as usize;
			let hi = (lo + 1).min(n - 1);
			let t = pos - lo as f32;
			third_octave[lo] * (1.0 - t) + third_octave[hi] * t + offset_db
		})
		.collect()
}

/// The input and the reference spectra for the display, level matched (the reference
/// moved onto the input between 63 Hz and 12.5 kHz) and both lifted so the loudest band
/// sits at -12 dB on the analyzer scale.
pub fn display_spectra(spectra: &MatchSpectra) -> (Vec<f32>, Vec<f32>) {
	let mid: Vec<usize> = (0..THIRD_OCTAVE_HZ.len())
		.filter(|&i| (63.0..=12_500.0).contains(&THIRD_OCTAVE_HZ[i]))
		.collect();
	let mean = |s: &[f32]| mid.iter().filter_map(|&i| s.get(i)).sum::<f32>() / mid.len() as f32;
	let level = mean(&spectra.input) - mean(&spectra.reference);
	let top = spectra
		.input
		.iter()
		.copied()
		.chain(spectra.reference.iter().map(|r| r + level))
		.fold(f32::MIN, f32::max);
	let lift = -12.0 - top;
	(
		analyzer_bins(&spectra.input, lift),
		analyzer_bins(&spectra.reference, level + lift),
	)
}

impl AppState {
	/// The running or ready match (any device).
	pub fn match_eq(&self) -> Option<&MatchEq> {
		self.match_eq.current.as_ref()
	}

	pub fn match_eq_mut(&mut self) -> Option<&mut MatchEq> {
		self.match_eq.current.as_mut()
	}

	/// Shows a ready match (tests, screenshots).
	pub fn set_match_eq(&mut self, m: Option<MatchEq>) {
		self.cancel_match_eq();
		self.match_eq.current = m;
	}

	/// "Match… → File…": asks for the reference file, then starts.
	pub fn match_eq_file_dialog(&mut self, device: DeviceRef) {
		self.match_eq.pending_file = Some(device);
		self.file_dialog(crate::jobs::FileAction::MatchReference);
	}

	pub(super) fn finish_match_file(&mut self, path: PathBuf) {
		if let Some(device) = self.match_eq.pending_file.take() {
			self.start_match_eq(
				device,
				MatchReference::File {
					path,
					start_seconds: None,
					seconds: None,
				},
			);
		}
	}

	/// Tracks that can be the reference of `device` (every other track with audio).
	pub fn match_reference_tracks(&self, device: DeviceRef) -> Vec<(TrackId, String)> {
		self.project()
			.tracks
			.iter()
			.filter(|t| Some(t.id) != device.track && t.kind != cadenza_model::TrackKind::Folder)
			.map(|t| (t.id, t.name.clone()))
			.collect()
	}

	/// Starts rendering and analyzing for a match of `device` to `reference`.
	pub fn start_match_eq(&mut self, device: DeviceRef, reference: MatchReference) {
		self.cancel_match_eq();
		let name = match &reference {
			MatchReference::File { path, .. } => path.file_name().map_or_else(
				|| path.display().to_string(),
				|n| n.to_string_lossy().into(),
			),
			MatchReference::Track(id) => self
				.project()
				.track(*id)
				.map_or_else(|| id.to_string(), |t| t.name.clone()),
		};
		let (project, plugins, hosting) = {
			let mut s = self.session();
			(
				s.document().project().clone(),
				s.plugins_for_project(),
				s.plugin_hosting().clone(),
			)
		};
		let (tx, rx) = mpsc::channel();
		let cancel = Arc::new(AtomicBool::new(false));
		let stop = cancel.clone();
		let waker = self.waker.clone();
		let wake = move || {
			if let Some(w) = &waker {
				w();
			}
		};
		let spawned = std::thread::Builder::new()
			.name("cadenza-match-eq".into())
			.spawn(move || {
				let mut last = 0.0;
				let result = analyze_match_sources(
					&project,
					plugins,
					hosting,
					device.track,
					device.device,
					&reference,
					(None, None),
					&mut |f| {
						if f - last >= 0.02 || f >= 1.0 {
							last = f;
							let _ = tx.send(JobEvent::Progress(f));
							wake();
						}
						!stop.load(Ordering::Relaxed)
					},
				)
				.map_err(|e| e.to_string());
				let _ = tx.send(JobEvent::Done(result));
				wake();
			});
		let phase = match spawned {
			Ok(_) => MatchPhase::Running(0.0),
			Err(e) => MatchPhase::Failed(trf("Cannot start the analysis: {e}", &[("e", &e)])),
		};
		self.match_eq.current = Some(MatchEq {
			device,
			reference_name: name,
			amount: 100.0,
			max_gain_db: DEFAULT_MAX_GAIN_DB,
			phase,
			rx: Some(rx),
			cancel,
		});
	}

	/// Stops a running match and closes the panel.
	pub fn cancel_match_eq(&mut self) {
		if let Some(m) = self.match_eq.current.take() {
			m.cancel.store(true, Ordering::Relaxed);
		}
	}

	/// The fit and its param changes for the current document (`None`: no ready match,
	/// or the device is gone).
	pub fn match_eq_proposal(&self) -> Option<MatchPlan> {
		let m = self.match_eq.current.as_ref()?;
		let MatchPhase::Ready(spectra) = &m.phase else {
			return None;
		};
		plan_match(
			self.project(),
			m.device.track,
			m.device.device,
			spectra,
			&m.options(),
		)
		.ok()
	}

	/// Applies the proposal as ONE undo step "Match EQ to <reference>" and closes the
	/// panel.
	pub fn apply_match_eq(&mut self) -> bool {
		let Some((_, changes)) = self.match_eq_proposal() else {
			return false;
		};
		let Some(m) = self.match_eq.current.take() else {
			return false;
		};
		let label = match_label(&m.reference_name);
		self.commit_params(m.device.track, m.device.device, &changes, &label)
	}

	/// Collects the worker's progress and result (each frame).
	pub(super) fn poll_match_eq(&mut self) {
		let Some(m) = &mut self.match_eq.current else {
			return;
		};
		let Some(rx) = &m.rx else {
			return;
		};
		loop {
			match rx.try_recv() {
				Ok(JobEvent::Progress(f)) => m.phase = MatchPhase::Running(f),
				Ok(JobEvent::Done(r)) => {
					m.phase = match r {
						Ok(s) => MatchPhase::Ready(s),
						Err(e) => MatchPhase::Failed(e),
					};
					m.rx = None;
					break;
				}
				Err(mpsc::TryRecvError::Empty) => break,
				Err(mpsc::TryRecvError::Disconnected) => {
					m.phase = MatchPhase::Failed(tr("the analysis thread stopped").into());
					m.rx = None;
					break;
				}
			}
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use cadenza_model::{Command, Device, Track, TrackKind};

	fn spectra() -> MatchSpectra {
		let input: Vec<f32> = THIRD_OCTAVE_HZ.iter().map(|_| -30.0).collect();
		// The reference has +4 dB around 2-4 kHz.
		let reference = THIRD_OCTAVE_HZ
			.iter()
			.map(|&f| {
				-30.0
					+ if (2_000.0..=4_000.0).contains(&f) {
						4.0
					} else {
						0.0
					}
			})
			.collect();
		MatchSpectra {
			input,
			reference,
			reference_name: "ref.wav".into(),
		}
	}

	#[test]
	fn a_ready_match_previews_and_applies_as_one_step() {
		let mut s = AppState::headless();
		let mut track = Track::new("Vox", TrackKind::Audio);
		let eq = Device::builtin("eq", "EQ");
		let device = DeviceRef {
			track: Some(track.id),
			device: eq.id,
		};
		track.channel.inserts.push(eq);
		assert!(s.apply(Command::AddTrack { track, index: None }));
		s.set_match_eq(Some(MatchEq::ready(device, spectra())));
		let (fit, changes) = s.match_eq_proposal().expect("ready");
		assert!(!fit.bands.is_empty());
		let boost = changes
			.iter()
			.filter(|(id, _)| id.ends_with("_gain"))
			.map(|c| c.1)
			.fold(0.0, f32::max);
		assert!(boost > 1.5, "{changes:?}");
		// Half the amount: a smaller proposal.
		s.match_eq_mut().unwrap().amount = 50.0;
		let (_, half) = s.match_eq_proposal().unwrap();
		let half_boost = half
			.iter()
			.filter(|(id, _)| id.ends_with("_gain"))
			.map(|c| c.1)
			.fold(0.0, f32::max);
		assert!(half_boost < boost, "{half_boost} vs {boost}");
		let steps = s.document().history_labels().len();
		assert!(s.apply_match_eq());
		assert!(s.match_eq().is_none());
		let labels: Vec<String> = s
			.document()
			.history_labels()
			.iter()
			.map(|l| l.to_string())
			.collect();
		assert_eq!(labels.len(), steps + 1);
		assert!(
			labels.iter().any(|l| l == "Match EQ to ref.wav"),
			"{labels:?}"
		);
		s.undo();
		assert_eq!(s.document().history_labels().len(), steps);
	}

	#[test]
	fn display_spectra_are_level_matched() {
		let mut sp = spectra();
		for r in &mut sp.reference {
			*r -= 20.0;
		}
		let (input, reference) = display_spectra(&sp);
		assert_eq!(input.len(), ANALYZER_BINS);
		let at = |bins: &[f32], hz: f32| {
			let i = (0..ANALYZER_BINS)
				.min_by(|&a, &b| {
					(analyzer_freq(a) / hz)
						.log2()
						.abs()
						.total_cmp(&(analyzer_freq(b) / hz).log2().abs())
				})
				.unwrap();
			bins[i]
		};
		assert!((at(&input, 200.0) - at(&reference, 200.0)).abs() < 1.0);
		assert!(at(&reference, 3_000.0) > at(&input, 3_000.0) + 2.0);
		let top = reference
			.iter()
			.chain(&input)
			.copied()
			.fold(f32::MIN, f32::max);
		assert!((top + 12.0).abs() < 0.5, "{top}");
	}
}
